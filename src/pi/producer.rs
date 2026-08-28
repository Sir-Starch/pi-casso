use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use super::generator_backend::{GeneratorSelection, GeneratorVariant};
use super::y_cruncher::generate_to_target;
use super::{
    PiCache, PiGenerator, chudnovsky_pi_digits, chudnovsky_pi_digits_sequential,
    generation_guard_with_stop,
};
use crate::digits::{DigitRead, DigitSource, GenerationState, ReaderPath};

pub(crate) const GENERATOR_FIXED_BYTES: u64 = 4096;
pub(crate) const CHUDNOVSKY_PREFIX_CACHE_DIGITS: usize = 131_072;
pub trait GenerationPermit: Send {}

pub trait GenerationBudget: Send + Sync {
    fn plan_generation(
        &self,
        current_digits: u64,
        absolute_target: u64,
        window_len: usize,
        fixed_bytes: u64,
    ) -> Result<GenerationPlan>;

    fn acquire_generation(
        self: Arc<Self>,
        fixed_bytes: u64,
        resident_digit_bytes: u64,
        cpu_workers: usize,
    ) -> Result<(Box<dyn GenerationPermit>, Duration)>;
}

#[derive(Clone, Copy, Debug)]
pub struct GenerationDemand {
    pub absolute_target: u64,
    pub window_len: usize,
    pub generator_fixed_bytes: u64,
    pub cpu_workers: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GenerationPlan {
    pub free_logical_memory_bytes: u64,
    pub available_lead_digits: u64,
    pub pending_lead_digits: u64,
    pub lead_digits: u64,
    pub high_water_digits: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GenerationMetrics {
    pub concurrent_requests: u64,
    pub coalesced_request_count: u64,
    pub producer_epochs: u64,
    pub generation_batches: u64,
    pub chudnovsky_target_computations: u64,
    pub lead_digits: u64,
    pub high_water_digits: u64,
    pub generator_wait: Duration,
    pub event_wake_latency: Duration,
    pub cache_write: Duration,
    pub generated_source_digits: u64,
    pub recomputed_source_digits: u64,
    pub skipped_source_digits: u64,
}

#[derive(Clone, Debug)]
pub(crate) enum GenerationBackend {
    Chudnovsky,
    Spigot,
    YCruncher(super::y_cruncher::ValidatedYCruncher),
}

#[derive(Default)]
pub(crate) struct GenerationWorkers {
    spigot: Option<PiGenerator>,
}

#[derive(Default)]
pub(crate) struct ChudnovskyPrefixCache {
    prefix: Vec<u8>,
}

impl ChudnovskyPrefixCache {
    pub(crate) fn prefix(&self, target: usize) -> Option<Vec<u8>> {
        (target <= self.prefix.len()).then(|| self.prefix[..target].to_vec())
    }

    pub(crate) fn remember(&mut self, prefix: &[u8]) {
        if prefix.len() <= CHUDNOVSKY_PREFIX_CACHE_DIGITS && prefix.len() > self.prefix.len() {
            self.prefix.clear();
            self.prefix.extend_from_slice(prefix);
        }
    }

    #[cfg(test)]
    pub(crate) fn retained_digits(&self) -> usize {
        self.prefix.len()
    }
}

#[derive(Default)]
pub(crate) struct CoordinatedRayonPool {
    pool: Option<rayon::ThreadPool>,
}

impl CoordinatedRayonPool {
    pub(crate) fn pool(&mut self, cpu_workers: usize) -> Result<&rayon::ThreadPool> {
        if cpu_workers <= 1 {
            return Err(anyhow!(
                "coordinated Rayon pool requires more than one CPU worker"
            ));
        }
        if self.pool.is_none() {
            self.pool = Some(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(cpu_workers)
                    .build()?,
            );
        }
        self.pool
            .as_ref()
            .ok_or_else(|| anyhow!("coordinated Rayon pool was not initialized"))
    }

    #[cfg(test)]
    pub(crate) fn retained_pool_count(&self) -> usize {
        self.pool.iter().count()
    }
}

static CHUDNOVSKY_POOL: OnceLock<Mutex<CoordinatedRayonPool>> = OnceLock::new();
static CHUDNOVSKY_PREFIX_CACHE: OnceLock<Mutex<ChudnovskyPrefixCache>> = OnceLock::new();

impl GenerationWorkers {
    fn generate_batch(
        &mut self,
        cache: &PiCache,
        backend: &GenerationBackend,
        request: GenerationBatchRequest,
        stop: &AtomicBool,
    ) -> Result<GenerationBatch> {
        match backend {
            GenerationBackend::Chudnovsky => {
                let target = usize::try_from(request.plan.high_water_digits)?;
                let prefix = {
                    let mut prefix_cache = CHUDNOVSKY_PREFIX_CACHE
                        .get_or_init(|| Mutex::new(ChudnovskyPrefixCache::default()))
                        .lock()
                        .map_err(|_| anyhow!("Chudnovsky prefix cache was poisoned"))?;
                    if let Some(prefix) = prefix_cache.prefix(target) {
                        prefix
                    } else {
                        let prefix = if request.cpu_workers <= 1 {
                            chudnovsky_pi_digits_sequential(target)?
                        } else {
                            let mut coordinated_pool = CHUDNOVSKY_POOL
                                .get_or_init(|| Mutex::new(CoordinatedRayonPool::default()))
                                .lock()
                                .map_err(|_| anyhow!("coordinated Rayon pool was poisoned"))?;
                            coordinated_pool
                                .pool(request.cpu_workers)?
                                .install(|| chudnovsky_pi_digits(target))?
                        };
                        prefix_cache.remember(&prefix);
                        prefix
                    }
                };
                let digits = prefix[usize::try_from(request.current)?..].to_vec();
                publish_builtin(cache, digits, request.current)
            }
            GenerationBackend::Spigot => {
                let batch_len = usize::try_from(
                    request
                        .plan
                        .high_water_digits
                        .checked_sub(request.current)
                        .ok_or_else(|| anyhow!("spigot high-water mark precedes cache prefix"))?,
                )?;
                if self.spigot.is_none() {
                    let mut generator = PiGenerator::new();
                    generator.skip(request.current, stop)?;
                    if stop.load(Ordering::Acquire) {
                        return Err(anyhow!("pi generation was cancelled"));
                    }
                    self.spigot = Some(generator);
                }
                let digits = self
                    .spigot
                    .as_mut()
                    .ok_or_else(|| anyhow!("spigot generator was not initialized"))?
                    .generate(batch_len, stop)?;
                publish_builtin(cache, digits, 0)
            }
            GenerationBackend::YCruncher(executable) => {
                let generation = generate_to_target(
                    cache,
                    request.plan.high_water_digits,
                    executable,
                    request.cpu_workers,
                )?;
                Ok(GenerationBatch {
                    generated_source_digits: generation.generated_digits,
                    cache_write: generation.cache_write,
                    recomputed_source_digits: request.current,
                })
            }
        }
    }
}

#[derive(Clone, Copy)]
struct GenerationBatchRequest {
    current: u64,
    plan: GenerationPlan,
    cpu_workers: usize,
}

struct PendingDemand {
    demand: GenerationDemand,
    budget: Arc<dyn GenerationBudget>,
}

#[derive(Default)]
struct ProducerState {
    pending: Option<PendingDemand>,
    pending_request_count: u64,
    completed_target: u64,
    target_digits: u64,
    active: bool,
    paused: bool,
    shutdown: bool,
    cancel_epoch: u64,
    error: Option<String>,
    signaled_at: Option<Instant>,
    metrics: GenerationMetrics,
    waiters: u64,
}

struct SharedProducer {
    state: Mutex<ProducerState>,
    changed: Condvar,
    cancelled: AtomicBool,
}

pub struct CachedGrowingPiSource {
    cache: PiCache,
    shared: Arc<SharedProducer>,
    handle: Mutex<Option<JoinHandle<Result<()>>>>,
}

impl CachedGrowingPiSource {
    pub fn new(cache: PiCache) -> Self {
        Self::start(cache, GenerationBackend::Chudnovsky, false)
    }

    pub(crate) fn from_selection(cache: PiCache, selection: &GeneratorSelection) -> Result<Self> {
        Ok(Self::start(
            cache,
            backend_from_selection(selection)?,
            false,
        ))
    }

    pub(crate) fn paused_from_selection(
        cache: PiCache,
        selection: &GeneratorSelection,
    ) -> Result<Self> {
        Ok(Self::start(cache, backend_from_selection(selection)?, true))
    }

    fn start(cache: PiCache, backend: GenerationBackend, paused: bool) -> Self {
        let shared = Arc::new(SharedProducer {
            state: Mutex::new(ProducerState {
                paused,
                ..ProducerState::default()
            }),
            changed: Condvar::new(),
            cancelled: AtomicBool::new(false),
        });
        let worker_cache = cache.clone();
        let worker_shared = Arc::clone(&shared);
        let handle = thread::spawn(move || producer_loop(worker_cache, worker_shared, backend));
        Self {
            cache,
            shared,
            handle: Mutex::new(Some(handle)),
        }
    }

    pub fn request_generation(
        &self,
        demand: GenerationDemand,
        budget: Arc<dyn GenerationBudget>,
        stop: Arc<AtomicBool>,
    ) -> Result<GenerationMetrics> {
        if self.cache.published_digit_count()? >= demand.absolute_target {
            return self.metrics();
        }
        let mut state = self.lock_state()?;
        if state.shutdown {
            return Err(anyhow!("pi producer shut down"));
        }
        if let Some(error) = state.error.as_ref() {
            return Err(anyhow!(error.clone()));
        }
        let cancel_epoch = state.cancel_epoch;
        state.waiters = state.waiters.saturating_add(1);
        state.metrics.concurrent_requests = state.metrics.concurrent_requests.max(state.waiters);
        state.pending_request_count = state.pending_request_count.saturating_add(1);
        if state
            .pending
            .as_ref()
            .is_none_or(|pending| demand.absolute_target >= pending.demand.absolute_target)
        {
            state.pending = Some(PendingDemand { demand, budget });
        }
        state.signaled_at.get_or_insert_with(Instant::now);
        self.shared.changed.notify_all();
        loop {
            if state.completed_target >= demand.absolute_target {
                state.waiters = state.waiters.saturating_sub(1);
                return Ok(state.metrics);
            }
            if state.shutdown {
                state.waiters = state.waiters.saturating_sub(1);
                return Err(anyhow!("pi producer shut down"));
            }
            if state.cancel_epoch != cancel_epoch || stop.load(Ordering::Acquire) {
                state.waiters = state.waiters.saturating_sub(1);
                return Err(anyhow!("pi generation was cancelled"));
            }
            if let Some(error) = state.error.as_ref() {
                let error = error.clone();
                state.waiters = state.waiters.saturating_sub(1);
                return Err(anyhow!(error));
            }
            state = self
                .shared
                .changed
                .wait(state)
                .map_err(|_| anyhow!("pi producer state was poisoned"))?;
        }
    }

    pub(crate) fn queue_generation_prefetch(
        &self,
        demand: GenerationDemand,
        budget: Arc<dyn GenerationBudget>,
        stop: Arc<AtomicBool>,
    ) -> Result<()> {
        if stop.load(Ordering::Acquire)
            || self.cache.published_digit_count()? >= demand.absolute_target
        {
            return Ok(());
        }
        let mut state = self.lock_state()?;
        if state.shutdown {
            return Err(anyhow!("pi producer shut down"));
        }
        if let Some(error) = state.error.as_ref() {
            return Err(anyhow!(error.clone()));
        }
        let active_target_reaches_demand =
            state.active && state.target_digits >= demand.absolute_target;
        let pending_target_reaches_demand = state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.demand.absolute_target >= demand.absolute_target);
        if active_target_reaches_demand || pending_target_reaches_demand {
            return Ok(());
        }
        state.pending_request_count = state.pending_request_count.saturating_add(1);
        state.pending = Some(PendingDemand { demand, budget });
        state.signaled_at.get_or_insert_with(Instant::now);
        self.shared.changed.notify_all();
        Ok(())
    }

    pub fn metrics(&self) -> Result<GenerationMetrics> {
        Ok(self.lock_state()?.metrics)
    }

    pub fn cancel_waiters(&self) {
        if let Ok(mut state) = self.shared.state.lock() {
            self.shared.cancelled.store(true, Ordering::Release);
            state.cancel_epoch = state.cancel_epoch.saturating_add(1);
            self.shared.changed.notify_all();
        }
    }

    pub fn shutdown(&self) -> Result<()> {
        if let Ok(mut state) = self.shared.state.lock() {
            self.shared.cancelled.store(true, Ordering::Release);
            state.shutdown = true;
            self.shared.changed.notify_all();
        }
        let handle = self
            .handle
            .lock()
            .map_err(|_| anyhow!("pi producer handle was poisoned"))?
            .take();
        match handle {
            Some(handle) => handle
                .join()
                .map_err(|_| anyhow!("pi producer thread panicked"))?,
            None => Ok(()),
        }
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, ProducerState>> {
        self.shared
            .state
            .lock()
            .map_err(|_| anyhow!("pi producer state was poisoned"))
    }

    #[cfg(test)]
    pub(crate) fn new_paused_for_test(cache: PiCache, backend: GenerationBackend) -> Self {
        Self::start(cache, backend, true)
    }

    #[cfg(test)]
    pub(crate) fn new_with_backend_for_test(cache: PiCache, backend: GenerationBackend) -> Self {
        Self::start(cache, backend, false)
    }

    pub(crate) fn wait_for_pending_requests(&self, count: usize) -> Result<()> {
        let count = u64::try_from(count)?;
        let mut state = self.lock_state()?;
        while state.pending_request_count < count {
            state = self
                .shared
                .changed
                .wait(state)
                .map_err(|_| anyhow!("pi producer state was poisoned"))?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn wait_for_active_for_test(&self) -> Result<()> {
        let mut state = self.lock_state()?;
        while !state.active {
            state = self
                .shared
                .changed
                .wait(state)
                .map_err(|_| anyhow!("pi producer state was poisoned"))?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn pending_demand_slots_for_test(&self) -> Result<usize> {
        Ok(usize::from(self.lock_state()?.pending.is_some()))
    }

    pub(crate) fn resume_producer(&self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.paused = false;
            state.signaled_at = Some(Instant::now());
            self.shared.changed.notify_all();
        }
    }

    #[cfg(test)]
    pub(crate) fn producer_joined_for_test(&self) -> bool {
        self.handle.lock().is_ok_and(|handle| handle.is_none())
    }
}

impl Drop for CachedGrowingPiSource {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl DigitSource for CachedGrowingPiSource {
    fn kind(&self) -> &'static str {
        "cache"
    }

    fn len(&self) -> Result<u64> {
        self.cache.published_digit_count()
    }

    fn validate(&self) -> Result<()> {
        self.cache.validate()
    }

    fn read_range(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.cache.read_range(offset, len)
    }

    fn read_range_timed(&self, offset: u64, len: usize) -> Result<DigitRead> {
        self.cache.read_range_timed(offset, len)
    }

    fn reader_path(&self) -> Option<ReaderPath<'_>> {
        None
    }

    fn is_growing(&self) -> bool {
        true
    }

    fn generation(&self) -> Option<GenerationState> {
        self.lock_state().ok().map(|state| GenerationState {
            active: state.active,
            target_digits: state.target_digits,
        })
    }

    fn request_generation(
        &self,
        demand: GenerationDemand,
        budget: Arc<dyn GenerationBudget>,
        stop: Arc<AtomicBool>,
    ) -> Result<GenerationMetrics> {
        self.request_generation(demand, budget, stop)
    }

    fn prefetch_generation(
        &self,
        demand: GenerationDemand,
        budget: Arc<dyn GenerationBudget>,
        stop: Arc<AtomicBool>,
    ) -> Result<()> {
        self.queue_generation_prefetch(demand, budget, stop)
    }

    fn cancel_generation_waiters(&self) {
        self.cancel_waiters();
    }

    fn generation_metrics(&self) -> Option<GenerationMetrics> {
        self.metrics().ok()
    }
}

fn producer_loop(
    cache: PiCache,
    shared: Arc<SharedProducer>,
    backend: GenerationBackend,
) -> Result<()> {
    let mut workers = GenerationWorkers::default();
    loop {
        let request = {
            let mut state = shared
                .state
                .lock()
                .map_err(|_| anyhow!("pi producer state was poisoned"))?;
            while (state.pending.is_none() || state.paused) && !state.shutdown {
                state = shared
                    .changed
                    .wait(state)
                    .map_err(|_| anyhow!("pi producer state was poisoned"))?;
            }
            if state.shutdown {
                return Ok(());
            }
            shared.cancelled.store(false, Ordering::Release);
            let request = state
                .pending
                .take()
                .ok_or_else(|| anyhow!("pi producer epoch had no request"))?;
            let request_count = std::mem::take(&mut state.pending_request_count);
            let signaled_at = state.signaled_at.take().unwrap_or_else(Instant::now);
            state.active = true;
            state.target_digits = request.demand.absolute_target;
            state.metrics.producer_epochs = state.metrics.producer_epochs.saturating_add(1);
            state.metrics.coalesced_request_count = state
                .metrics
                .coalesced_request_count
                .saturating_add(request_count);
            state.metrics.event_wake_latency = signaled_at.elapsed();
            shared.changed.notify_all();
            request
        };
        let result = run_epoch(&cache, &shared, &backend, &request, &mut workers);
        let mut state = shared
            .state
            .lock()
            .map_err(|_| anyhow!("pi producer state was poisoned"))?;
        state.active = false;
        match result {
            Ok(target) => state.completed_target = state.completed_target.max(target),
            Err(_) if shared.cancelled.load(Ordering::Acquire) => {}
            Err(error) => state.error = Some(format!("{error:#}")),
        }
        shared.changed.notify_all();
    }
}

fn run_epoch(
    cache: &PiCache,
    shared: &SharedProducer,
    backend: &GenerationBackend,
    request: &PendingDemand,
    workers: &mut GenerationWorkers,
) -> Result<u64> {
    let target = request.demand.absolute_target;
    let _generation_guard = generation_guard_with_stop(&shared.cancelled)?;
    let mut current = cache.published_digit_count()?;
    while current < target {
        if shared.cancelled.load(Ordering::Acquire) {
            return Err(anyhow!("pi generation was cancelled"));
        }
        let immediate_plan = request.budget.plan_generation(
            current,
            target,
            request.demand.window_len,
            request.demand.generator_fixed_bytes,
        )?;
        let plan_target = match backend {
            GenerationBackend::Chudnovsky => {
                chudnovsky_prefetch_target(current, target, immediate_plan.available_lead_digits)?
            }
            GenerationBackend::Spigot | GenerationBackend::YCruncher(_) => target,
        };
        let plan = if plan_target == target {
            immediate_plan
        } else {
            request.budget.plan_generation(
                current,
                plan_target,
                request.demand.window_len,
                request.demand.generator_fixed_bytes,
            )?
        };
        let started = Instant::now();
        let (permit, _permit_wait) = Arc::clone(&request.budget).acquire_generation(
            request.demand.generator_fixed_bytes,
            plan.lead_digits.saturating_mul(2),
            request.demand.cpu_workers,
        )?;
        let batch = workers.generate_batch(
            cache,
            backend,
            GenerationBatchRequest {
                current,
                plan,
                cpu_workers: request.demand.cpu_workers,
            },
            &shared.cancelled,
        )?;
        let generation_wait = generation_wait_duration(started.elapsed(), batch.cache_write);
        current = current.saturating_add(batch.generated_source_digits);
        drop(permit);
        let mut state = shared
            .state
            .lock()
            .map_err(|_| anyhow!("pi producer state was poisoned"))?;
        state.metrics.generator_wait += generation_wait;
        state.metrics.generation_batches = state.metrics.generation_batches.saturating_add(1);
        state.metrics.cache_write += batch.cache_write;
        state.metrics.generated_source_digits = state
            .metrics
            .generated_source_digits
            .saturating_add(batch.generated_source_digits);
        state.metrics.recomputed_source_digits = state
            .metrics
            .recomputed_source_digits
            .saturating_add(batch.recomputed_source_digits);
        state.metrics.lead_digits = plan.lead_digits;
        state.metrics.high_water_digits = plan.high_water_digits;
        if matches!(backend, GenerationBackend::Chudnovsky) {
            state.metrics.chudnovsky_target_computations = state
                .metrics
                .chudnovsky_target_computations
                .saturating_add(1);
        }
    }
    Ok(target)
}

fn chudnovsky_prefetch_target(
    current: u64,
    requested_target: u64,
    available_lead_digits: u64,
) -> Result<u64> {
    let prefix_limit = u64::try_from(CHUDNOVSKY_PREFIX_CACHE_DIGITS)?;
    let resource_limit = current.saturating_add(available_lead_digits);
    if requested_target >= prefix_limit || resource_limit <= requested_target {
        return Ok(requested_target);
    }
    let geometric_target = match requested_target.checked_next_power_of_two() {
        Some(target) => target.min(prefix_limit),
        None => prefix_limit,
    };
    Ok(geometric_target.min(resource_limit).max(requested_target))
}

pub(crate) fn generation_wait_duration(
    total_blocking: Duration,
    cache_write: Duration,
) -> Duration {
    total_blocking.saturating_sub(cache_write)
}

struct GenerationBatch {
    generated_source_digits: u64,
    cache_write: Duration,
    recomputed_source_digits: u64,
}

fn publish_builtin(cache: &PiCache, digits: Vec<u8>, recomputed: u64) -> Result<GenerationBatch> {
    let generated_source_digits = u64::try_from(digits.len())?;
    let started = Instant::now();
    cache.append_digits(&digits)?;
    Ok(GenerationBatch {
        generated_source_digits,
        cache_write: started.elapsed(),
        recomputed_source_digits: recomputed,
    })
}

fn backend_from_selection(selection: &GeneratorSelection) -> Result<GenerationBackend> {
    match selection.selected_variant {
        Some(GeneratorVariant::Chudnovsky) => Ok(GenerationBackend::Chudnovsky),
        Some(GeneratorVariant::Spigot) => Ok(GenerationBackend::Spigot),
        Some(GeneratorVariant::YCruncher) => selection
            .executable
            .clone()
            .map(GenerationBackend::YCruncher)
            .ok_or_else(|| anyhow!("executable_missing")),
        None => Err(anyhow!(selection.reason.clone())),
    }
}
