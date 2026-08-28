use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

const BYTES_PER_MB: u64 = 1_048_576;
const RESIDENT_BYTES_PER_DIGIT: u64 = 2;

mod cpu;
mod leases;
mod process_memory;

pub(crate) use leases::{ChunkLease, CpuLease, GeneratorLease, GpuLease};
pub(crate) use leases::{
    chunk_reservation_bytes, minimum_reservation_bytes, test_consumer_delay, test_mode_enabled,
};

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(crate) struct ResourceBudgetSnapshot {
    pub queue_current: u64,
    pub queue_peak: u64,
    pub queue_limit: u64,
    pub memory_reserved_bytes: u64,
    pub reader_capacity_bytes: u64,
    pub memory_peak_bytes: u64,
    pub memory_limit_bytes: u64,
    pub rss_peak_mb: f64,
    pub rss_baseline_mb: f64,
    pub rss_margin_mb: f64,
    pub cpu_permits_in_use: u64,
    pub cpu_permits_peak: u64,
    pub cpu_permits_max: u64,
    pub gpu_permits_in_use: u64,
    pub gpu_permits_peak: u64,
    pub gpu_permits_max: u64,
    pub generator_leases_in_use: u64,
    pub generator_leases_peak: u64,
    pub generator_leases_max: u64,
    pub generator_fixed_bytes_charged: u64,
    pub generator_fixed_charge_count: u64,
}

#[derive(Debug, Default)]
struct BudgetState {
    queue_current: u64,
    queue_peak: u64,
    memory_reserved_bytes: u64,
    reader_capacity_bytes: u64,
    memory_peak_bytes: u64,
    cpu_permits_in_use: u64,
    cpu_permits_peak: u64,
    gpu_permits_in_use: u64,
    gpu_permits_peak: u64,
    generator_leases_in_use: u64,
    generator_leases_peak: u64,
    generator_fixed_bytes_charged: u64,
    generator_fixed_charge_count: u64,
}

#[derive(Debug)]
pub(crate) struct ResourceBudget {
    state: Mutex<BudgetState>,
    ready: Condvar,
    cpu_pool: OnceLock<std::result::Result<Arc<rayon::ThreadPool>, String>>,
    queue_limit: u64,
    memory_limit_bytes: u64,
    rss_baseline_bytes: u64,
    cpu_permits_max: u64,
    gpu_permits_max: u64,
    generator_leases_max: u64,
}

pub(crate) struct ReaderCapacityLease {
    budget: Arc<ResourceBudget>,
    bytes: u64,
}

impl Drop for ReaderCapacityLease {
    fn drop(&mut self) {
        self.budget.release_reader_capacity(self.bytes);
    }
}

impl ResourceBudget {
    pub(crate) fn new(
        queue_depth: usize,
        memory_limit_mb: usize,
        cpu_permits_max: usize,
    ) -> Result<Arc<Self>> {
        let memory_limit_bytes = u64::try_from(memory_limit_mb.max(1))?
            .checked_mul(BYTES_PER_MB)
            .ok_or_else(|| anyhow!("memory budget overflowed"))?;
        Self::new_with_memory_limit(queue_depth, memory_limit_bytes, cpu_permits_max)
    }

    fn new_with_memory_limit(
        queue_depth: usize,
        memory_limit_bytes: u64,
        cpu_permits_max: usize,
    ) -> Result<Arc<Self>> {
        let queue_limit = u64::try_from(queue_depth.max(1))?;
        let cpu_permits_max = cpu_permits_max.max(1);
        Self::validate_cpu_pool_size(cpu_permits_max)?;
        let cpu_permits_max = u64::try_from(cpu_permits_max)?;
        let rss_baseline_bytes = process_memory::sample().current_bytes;
        Ok(Arc::new(Self {
            state: Mutex::new(BudgetState::default()),
            ready: Condvar::new(),
            cpu_pool: OnceLock::new(),
            queue_limit,
            memory_limit_bytes,
            rss_baseline_bytes,
            cpu_permits_max,
            gpu_permits_max: queue_limit,
            generator_leases_max: 1,
        }))
    }

    #[cfg(test)]
    pub(crate) fn new_bytes(
        queue_depth: usize,
        memory_limit_bytes: u64,
        cpu_permits_max: usize,
    ) -> Result<Arc<Self>> {
        Self::new_with_memory_limit(queue_depth, memory_limit_bytes, cpu_permits_max)
    }

    pub(crate) fn cpu_pool(&self) -> Result<Option<Arc<rayon::ThreadPool>>> {
        let workers = usize::try_from(self.cpu_permits_max)?;
        if workers == 1 {
            return Ok(None);
        }
        let pool = self.cpu_pool.get_or_init(|| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(workers)
                .build()
                .map(Arc::new)
                .map_err(|error| error.to_string())
        });
        match pool {
            Ok(pool) => Ok(Some(Arc::clone(pool))),
            Err(error) => bail!("failed to construct bounded CPU pool: {error}"),
        }
    }

    pub(crate) fn validate_cpu_pool_size(cpu_workers: usize) -> Result<()> {
        let cpu_workers = cpu_workers.max(1);
        let maximum = rayon::max_num_threads();
        if cpu_workers > maximum {
            bail!("CPU worker request {cpu_workers} exceeds Rayon pool limit {maximum}");
        }
        Ok(())
    }

    pub(crate) fn validate_reader_capacity_limit(memory_limit_mb: usize, bytes: u64) -> Result<()> {
        let memory_limit_bytes = u64::try_from(memory_limit_mb.max(1))?
            .checked_mul(BYTES_PER_MB)
            .ok_or_else(|| anyhow!("memory budget overflowed"))?;
        if bytes > memory_limit_bytes {
            bail!(
                "digit reader capacity is {bytes} bytes, above memory budget {memory_limit_bytes} bytes"
            );
        }
        Ok(())
    }

    pub(crate) fn validate_reader_capacity(&self, bytes: u64) -> Result<()> {
        if bytes > self.memory_limit_bytes {
            bail!(
                "digit reader capacity is {bytes} bytes, above memory budget {} bytes",
                self.memory_limit_bytes
            );
        }
        Ok(())
    }

    pub(crate) fn reserve_reader_capacity(
        self: &Arc<Self>,
        bytes: u64,
    ) -> Result<ReaderCapacityLease> {
        self.validate_reader_capacity(bytes)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("resource budget was poisoned"))?;
        if state.memory_reserved_bytes.saturating_add(bytes) > self.memory_limit_bytes {
            let available_bytes = self
                .memory_limit_bytes
                .saturating_sub(state.memory_reserved_bytes);
            bail!(
                "digit reader capacity requires {bytes} bytes, but only {available_bytes} bytes remain in the memory budget"
            );
        }
        state.memory_reserved_bytes = state.memory_reserved_bytes.saturating_add(bytes);
        state.reader_capacity_bytes = state.reader_capacity_bytes.saturating_add(bytes);
        state.memory_peak_bytes = state.memory_peak_bytes.max(state.memory_reserved_bytes);
        Ok(ReaderCapacityLease {
            budget: Arc::clone(self),
            bytes,
        })
    }

    pub(crate) fn snapshot(&self) -> ResourceBudgetSnapshot {
        let rss = process_memory::sample();
        let rss_peak_bytes = rss
            .peak_bytes
            .max(rss.current_bytes)
            .max(self.rss_baseline_bytes);
        let rss_baseline_mb = process_memory::bytes_to_mb(self.rss_baseline_bytes);
        let rss_peak_mb = process_memory::bytes_to_mb(rss_peak_bytes);
        let rss_margin_mb =
            process_memory::bytes_to_mb(rss_peak_bytes.saturating_sub(self.rss_baseline_bytes));
        self.state.lock().map_or_else(
            |_| ResourceBudgetSnapshot {
                queue_limit: self.queue_limit,
                memory_limit_bytes: self.memory_limit_bytes,
                rss_peak_mb,
                rss_baseline_mb,
                rss_margin_mb,
                cpu_permits_max: self.cpu_permits_max,
                gpu_permits_max: self.gpu_permits_max,
                generator_leases_max: self.generator_leases_max,
                ..ResourceBudgetSnapshot::default()
            },
            |state| ResourceBudgetSnapshot {
                queue_current: state.queue_current,
                queue_peak: state.queue_peak,
                queue_limit: self.queue_limit,
                memory_reserved_bytes: state.memory_reserved_bytes,
                reader_capacity_bytes: state.reader_capacity_bytes,
                memory_peak_bytes: state.memory_peak_bytes,
                memory_limit_bytes: self.memory_limit_bytes,
                rss_peak_mb,
                rss_baseline_mb,
                rss_margin_mb,
                cpu_permits_in_use: state.cpu_permits_in_use,
                cpu_permits_peak: state.cpu_permits_peak,
                cpu_permits_max: self.cpu_permits_max,
                gpu_permits_in_use: state.gpu_permits_in_use,
                gpu_permits_peak: state.gpu_permits_peak,
                gpu_permits_max: self.gpu_permits_max,
                generator_leases_in_use: state.generator_leases_in_use,
                generator_leases_peak: state.generator_leases_peak,
                generator_leases_max: self.generator_leases_max,
                generator_fixed_bytes_charged: state.generator_fixed_bytes_charged,
                generator_fixed_charge_count: state.generator_fixed_charge_count,
            },
        )
    }

    pub(crate) fn validate_generation_window(
        &self,
        window_len: usize,
        generator_fixed_bytes: u64,
    ) -> Result<()> {
        let window_bytes = bytes_for_one_window(window_len)?;
        let required = generator_fixed_bytes
            .checked_add(window_bytes)
            .ok_or_else(|| anyhow!("generator minimum reservation overflowed"))?;
        if required > self.memory_limit_bytes {
            bail!(
                "pi generator requires {required} bytes for one window, above memory budget {} bytes",
                self.memory_limit_bytes
            );
        }
        Ok(())
    }

    pub(crate) fn plan_generation(
        &self,
        current_digits: u64,
        max_pending_absolute_target: u64,
        window_len: usize,
        generator_fixed_bytes: u64,
    ) -> Result<crate::pi::GenerationPlan> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("resource budget was poisoned"))?;
        let free_logical_memory_bytes = self
            .memory_limit_bytes
            .saturating_sub(state.memory_reserved_bytes);
        drop(state);
        let free_after_fixed = free_logical_memory_bytes.saturating_sub(generator_fixed_bytes);
        let window_bytes = bytes_for_one_window(window_len)?;
        if window_bytes > free_after_fixed {
            bail!(
                "pi generator requires {window_bytes} bytes for one window after fixed reservation, only {free_after_fixed} bytes are free"
            );
        }
        let available_lead_digits = free_after_fixed / RESIDENT_BYTES_PER_DIGIT;
        let pending_lead_digits = max_pending_absolute_target.saturating_sub(current_digits);
        let window_digits = u64::try_from(window_len)?;
        let lead_digits = available_lead_digits.min(window_digits.max(pending_lead_digits));
        let high_water_digits =
            max_pending_absolute_target.min(current_digits.saturating_add(lead_digits));
        Ok(crate::pi::GenerationPlan {
            free_logical_memory_bytes,
            available_lead_digits,
            pending_lead_digits,
            lead_digits,
            high_water_digits,
        })
    }

    pub(crate) fn validate_minimum(&self, window_len: usize) -> Result<()> {
        let required = minimum_reservation_bytes(window_len)?;
        if required > self.memory_limit_bytes {
            bail!(
                "minimum logical reservation is {required} bytes, above memory budget {} bytes",
                self.memory_limit_bytes
            );
        }
        Ok(())
    }

    pub(crate) fn cpu_permits_max(&self) -> Result<usize> {
        usize::try_from(self.cpu_permits_max).map_err(Into::into)
    }

    pub(crate) fn acquire_chunk(self: &Arc<Self>, bytes: u64) -> Result<(ChunkLease, Duration)> {
        if bytes > self.memory_limit_bytes {
            bail!(
                "chunk reservation is {bytes} bytes, above memory budget {} bytes",
                self.memory_limit_bytes
            );
        }
        let started = Instant::now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("resource budget was poisoned"))?;
        while state.queue_current >= self.queue_limit
            || state.memory_reserved_bytes.saturating_add(bytes) > self.memory_limit_bytes
        {
            state = self
                .ready
                .wait(state)
                .map_err(|_| anyhow!("resource budget was poisoned"))?;
        }
        state.queue_current = state.queue_current.saturating_add(1);
        state.queue_peak = state.queue_peak.max(state.queue_current);
        state.memory_reserved_bytes = state.memory_reserved_bytes.saturating_add(bytes);
        state.memory_peak_bytes = state.memory_peak_bytes.max(state.memory_reserved_bytes);
        Ok((
            ChunkLease {
                budget: Arc::clone(self),
                bytes,
            },
            started.elapsed(),
        ))
    }

    pub(crate) fn acquire_gpu(self: &Arc<Self>) -> Result<(GpuLease, Duration)> {
        let started = Instant::now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("resource budget was poisoned"))?;
        while state.gpu_permits_in_use >= self.gpu_permits_max {
            state = self
                .ready
                .wait(state)
                .map_err(|_| anyhow!("resource budget was poisoned"))?;
        }
        state.gpu_permits_in_use = state.gpu_permits_in_use.saturating_add(1);
        state.gpu_permits_peak = state.gpu_permits_peak.max(state.gpu_permits_in_use);
        Ok((
            GpuLease {
                budget: Arc::clone(self),
            },
            started.elapsed(),
        ))
    }

    pub(crate) fn acquire_generator(
        self: &Arc<Self>,
        fixed_bytes: u64,
        resident_digit_bytes: u64,
        cpu_permits: usize,
    ) -> Result<(GeneratorLease, Duration)> {
        let cpu_permits = u64::try_from(cpu_permits)?;
        let reserved_bytes = fixed_bytes
            .checked_add(resident_digit_bytes)
            .ok_or_else(|| anyhow!("generator reservation overflowed"))?;
        if reserved_bytes > self.memory_limit_bytes {
            bail!(
                "generator reservation is {reserved_bytes} bytes, above memory budget {} bytes",
                self.memory_limit_bytes
            );
        }
        if cpu_permits == 0 || cpu_permits > self.cpu_permits_max {
            bail!(
                "generator CPU permit request {cpu_permits} is outside the global limit {}",
                self.cpu_permits_max
            );
        }
        let started = Instant::now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("resource budget was poisoned"))?;
        while state.generator_leases_in_use >= self.generator_leases_max
            || state.memory_reserved_bytes.saturating_add(reserved_bytes) > self.memory_limit_bytes
            || state.cpu_permits_in_use > self.cpu_permits_max - cpu_permits
        {
            state = self
                .ready
                .wait(state)
                .map_err(|_| anyhow!("resource budget was poisoned"))?;
        }
        state.generator_leases_in_use = state.generator_leases_in_use.saturating_add(1);
        state.generator_leases_peak = state
            .generator_leases_peak
            .max(state.generator_leases_in_use);
        state.memory_reserved_bytes = state.memory_reserved_bytes.saturating_add(reserved_bytes);
        state.memory_peak_bytes = state.memory_peak_bytes.max(state.memory_reserved_bytes);
        state.cpu_permits_in_use = state.cpu_permits_in_use.saturating_add(cpu_permits);
        state.cpu_permits_peak = state.cpu_permits_peak.max(state.cpu_permits_in_use);
        state.generator_fixed_bytes_charged = fixed_bytes;
        state.generator_fixed_charge_count = state.generator_fixed_charge_count.saturating_add(1);
        Ok((
            GeneratorLease {
                budget: Arc::clone(self),
                reserved_bytes,
                cpu_permits,
            },
            started.elapsed(),
        ))
    }

    fn release_chunk(&self, bytes: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.queue_current = state.queue_current.saturating_sub(1);
            state.memory_reserved_bytes = state.memory_reserved_bytes.saturating_sub(bytes);
            self.ready.notify_all();
        }
    }

    fn release_reader_capacity(&self, bytes: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.memory_reserved_bytes = state.memory_reserved_bytes.saturating_sub(bytes);
            state.reader_capacity_bytes = state.reader_capacity_bytes.saturating_sub(bytes);
            self.ready.notify_all();
        }
    }

    fn release_gpu(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.gpu_permits_in_use = state.gpu_permits_in_use.saturating_sub(1);
            self.ready.notify_all();
        }
    }

    fn release_generator(&self, reserved_bytes: u64, cpu_permits: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.generator_leases_in_use = state.generator_leases_in_use.saturating_sub(1);
            state.memory_reserved_bytes =
                state.memory_reserved_bytes.saturating_sub(reserved_bytes);
            state.cpu_permits_in_use = state.cpu_permits_in_use.saturating_sub(cpu_permits);
            self.ready.notify_all();
        }
    }
}

impl ResourceBudgetSnapshot {
    pub(crate) fn transient_memory_reserved_bytes(&self) -> u64 {
        self.memory_reserved_bytes
            .saturating_sub(self.reader_capacity_bytes)
    }
}

pub(crate) fn bytes_for_one_window(window_len: usize) -> Result<u64> {
    u64::try_from(window_len)?
        .checked_mul(RESIDENT_BYTES_PER_DIGIT)
        .ok_or_else(|| anyhow!("pi generator window reservation overflowed"))
}

impl crate::pi::GenerationPermit for GeneratorLease {}

impl crate::pi::GenerationBudget for ResourceBudget {
    fn plan_generation(
        &self,
        current_digits: u64,
        absolute_target: u64,
        window_len: usize,
        fixed_bytes: u64,
    ) -> Result<crate::pi::GenerationPlan> {
        self.plan_generation(current_digits, absolute_target, window_len, fixed_bytes)
    }

    fn acquire_generation(
        self: Arc<Self>,
        fixed_bytes: u64,
        resident_digit_bytes: u64,
        cpu_workers: usize,
    ) -> Result<(Box<dyn crate::pi::GenerationPermit>, Duration)> {
        let (lease, wait) =
            self.acquire_generator(fixed_bytes, resident_digit_bytes, cpu_workers)?;
        Ok((Box::new(lease), wait))
    }
}

#[cfg(test)]
mod generation_tests {
    use super::ResourceBudget;

    #[test]
    fn pi_generation_accounting_uses_two_bytes_per_digit_and_one_fixed_charge() {
        // Given: the exact Todo 10 accounting vector and 16 KiB already reserved.
        let budget = ResourceBudget::new_bytes(2, 64 * 1024, 4).expect("resource budget");
        let (_existing, _) = budget
            .acquire_chunk(16 * 1024)
            .expect("existing reservation");

        // When: the producer plans one high-water batch and reserves fixed metadata once.
        let plan = budget
            .plan_generation(4096, 250_000, 4096, 4096)
            .expect("generation plan");
        let (_generator, _) = budget
            .acquire_generator(4096, 22_528 * 2, 1)
            .expect("generator reservation");

        // Then: every intermediate value is unit-correct and the fixed charge is singular.
        assert_eq!(plan.free_logical_memory_bytes, 49_152);
        assert_eq!(plan.available_lead_digits, 22_528);
        assert_eq!(plan.pending_lead_digits, 245_904);
        assert_eq!(plan.lead_digits, 22_528);
        assert_eq!(plan.high_water_digits, 26_624);
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.memory_reserved_bytes, 64 * 1024);
        assert_eq!(snapshot.generator_fixed_bytes_charged, 4096);
        assert_eq!(snapshot.generator_fixed_charge_count, 1);
    }

    #[test]
    fn pi_generation_rejects_a_window_larger_than_the_budget() {
        // Given: less logical memory than one 4096-digit raw+parsed window needs.
        let budget = ResourceBudget::new_bytes(1, 8191, 1).expect("resource budget");

        // When: generator resources are validated.
        let error = budget
            .validate_generation_window(4096, 0)
            .expect_err("one window must not fit");

        // Then: the bounded resource error reports the exact minimum.
        assert!(error.to_string().contains("8192"));
    }

    #[test]
    fn pi_generation_can_overlap_a_full_search_queue_within_resource_limits() {
        // Given: one queued search chunk with enough independent memory and CPU for generation.
        let budget = ResourceBudget::new_bytes(1, 64 * 1024, 1).expect("resource budget");
        let (_chunk, _) = budget.acquire_chunk(16 * 1024).expect("search chunk");
        let (sender, receiver) = std::sync::mpsc::channel();

        // When: the asynchronous producer requests its separately bounded generator lease.
        std::thread::spawn(move || {
            sender
                .send(budget.acquire_generator(4096, 32 * 1024, 1))
                .expect("send acquisition result");
        });

        // Then: queue occupancy alone does not serialize the producer behind its consumer.
        let (generator, _) = receiver
            .recv_timeout(std::time::Duration::from_millis(100))
            .expect("generator acquisition must not wait for queue capacity")
            .expect("generator reservation");
        drop(generator);
    }
}

#[cfg(test)]
mod reader_capacity_tests {
    use super::ResourceBudget;

    #[test]
    fn reader_capacity_reservation_fails_promptly_when_shared_budget_cannot_fit_another_pool() {
        // Given: one permanent reader pool already occupies most of a shared budget.
        let budget = ResourceBudget::new_bytes(1, 100, 1).expect("resource budget");
        let _existing = budget
            .reserve_reader_capacity(60)
            .expect("first reader pool reservation");
        let (sender, receiver) = std::sync::mpsc::channel();

        // When: another session tries to initialize its permanent reader pool.
        std::thread::spawn(move || {
            sender
                .send(budget.reserve_reader_capacity(60).map(|_| ()))
                .expect("send reservation result");
        });
        let error = receiver
            .recv_timeout(std::time::Duration::from_millis(100))
            .expect("reader capacity initialization must fail without waiting")
            .expect_err("an oversized shared reservation must be rejected");

        // Then: initialization returns a prompt resource-capacity error.
        assert!(
            error
                .to_string()
                .contains("only 40 bytes remain in the memory budget")
        );
    }

    #[test]
    fn reader_capacity_stays_in_total_telemetry_but_not_transient_drain_accounting() {
        // Given: a permanent reader reservation and one transient search chunk share the budget.
        let budget = ResourceBudget::new_bytes(1, 100, 1).expect("resource budget");
        let reader = budget
            .reserve_reader_capacity(60)
            .expect("reader capacity reservation");
        let (chunk, _) = budget.acquire_chunk(20).expect("search chunk reservation");

        // When: the budget is sampled while both lifetimes are active.
        let active = budget.snapshot();

        // Then: total telemetry keeps both charges, while drain accounting sees only the chunk.
        assert_eq!(active.memory_reserved_bytes, 80);
        assert_eq!(active.transient_memory_reserved_bytes(), 20);

        drop(chunk);
        let drained_workers = budget.snapshot();
        assert_eq!(drained_workers.memory_reserved_bytes, 60);
        assert_eq!(drained_workers.transient_memory_reserved_bytes(), 0);

        drop(reader);
        assert_eq!(budget.snapshot().memory_reserved_bytes, 0);
    }
}
