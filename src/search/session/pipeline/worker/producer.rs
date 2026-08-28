use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use crate::digits::DigitSource;
use crate::pi::{GENERATOR_FIXED_BYTES, GenerationDemand, GenerationMetrics};
use crate::search::session::DigitReaderPool;
use crate::search::session::resource_budget::{
    ChunkLease, ResourceBudget, chunk_reservation_bytes,
};

pub(crate) struct ProducerConfig {
    pub start_offset: u64,
    pub start_scanned: u64,
    pub max_offset: Option<u64>,
    pub limit: Option<u64>,
    pub chunk_windows: usize,
    pub window_len: usize,
    pub growing: bool,
    pub accelerator: bool,
}

pub(crate) struct SourceChunk {
    pub _lease: ChunkLease,
    pub start_offset: u64,
    pub start_scanned: u64,
    pub actual_windows: usize,
    pub digits: Vec<u8>,
    pub read: Duration,
    pub parse: Duration,
    pub cache_hit: Duration,
}

#[derive(Default)]
pub(crate) struct ProducerReport {
    pub queue_wait: Duration,
    pub generator_wait: Duration,
    pub source_wait: Duration,
    pub producer_epochs: u64,
    pub coalesced_requests: u64,
    pub generation_batches: u64,
    pub event_wake_latency: Duration,
    pub lead_digits: u64,
    pub high_water_digits: u64,
}

pub(crate) fn produce<'source>(
    tx: SyncSender<SourceChunk>,
    reader_pool: DigitReaderPool<'source>,
    source: &'source dyn DigitSource,
    config: ProducerConfig,
    budget: Arc<ResourceBudget>,
    stop: Arc<AtomicBool>,
) -> Result<(DigitReaderPool<'source>, ProducerReport)> {
    let mut report = ProducerReport::default();
    let mut offset = config.start_offset;
    let mut scanned = config.start_scanned;
    let prefetch_horizon = u64::try_from(
        config
            .chunk_windows
            .saturating_mul(8)
            .max(config.window_len),
    )?;
    let required_window = u64::try_from(config.window_len)?;
    let mut prefetched_target = 0_u64;
    if config.accelerator && config.growing {
        let source_len = source.len()?;
        let target = initial_prefetch_target(
            source_len,
            config.start_offset,
            required_window,
            &config,
            prefetch_horizon,
        );
        if target > source_len {
            source.prefetch_generation(
                GenerationDemand {
                    absolute_target: target,
                    window_len: config.window_len,
                    generator_fixed_bytes: GENERATOR_FIXED_BYTES,
                    cpu_workers: generation_cpu_workers(budget.cpu_permits_max()?),
                },
                Arc::clone(&budget) as Arc<dyn crate::pi::GenerationBudget>,
                Arc::clone(&stop),
            )?;
            prefetched_target = target;
        }
    }
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if config
            .limit
            .is_some_and(|limit| scanned.saturating_sub(config.start_scanned) >= limit)
            || config.max_offset.is_some_and(|limit| offset >= limit)
        {
            break;
        }

        let source_started = Instant::now();
        let source_len = source.len()?;
        report.source_wait += source_started.elapsed();
        let available = source_len.saturating_sub(offset);
        if config.accelerator && config.growing && available < prefetch_horizon {
            let target = offset
                .saturating_add(required_window)
                .saturating_add(prefetch_horizon);
            if target > prefetched_target {
                source.prefetch_generation(
                    GenerationDemand {
                        absolute_target: target,
                        window_len: config.window_len,
                        generator_fixed_bytes: GENERATOR_FIXED_BYTES,
                        cpu_workers: generation_cpu_workers(budget.cpu_permits_max()?),
                    },
                    Arc::clone(&budget) as Arc<dyn crate::pi::GenerationBudget>,
                    Arc::clone(&stop),
                )?;
                prefetched_target = target;
            }
        } else if source_len >= prefetched_target {
            prefetched_target = 0;
        }
        if available < required_window {
            if !config.growing {
                break;
            }
            let metrics = source.request_generation(
                GenerationDemand {
                    absolute_target: generation_target(offset, required_window, &config)?,
                    window_len: config.window_len,
                    generator_fixed_bytes: GENERATOR_FIXED_BYTES,
                    cpu_workers: generation_cpu_workers(budget.cpu_permits_max()?),
                },
                Arc::clone(&budget) as Arc<dyn crate::pi::GenerationBudget>,
                Arc::clone(&stop),
            )?;
            record_generation_metrics(&mut report, metrics);
            continue;
        }

        let available_windows = available.saturating_sub(required_window).saturating_add(1);
        let mut windows = u64::try_from(config.chunk_windows)?.min(available_windows);
        if let Some(limit) = config.limit {
            windows =
                windows.min(limit.saturating_sub(scanned.saturating_sub(config.start_scanned)));
        }
        if let Some(limit) = config.max_offset {
            windows = windows.min(limit.saturating_sub(offset));
        }
        let actual_windows = usize::try_from(windows)?;
        if actual_windows == 0 {
            break;
        }

        let reservation = chunk_reservation_bytes(actual_windows, config.window_len)?;
        let (lease, queue_wait) = budget.acquire_chunk(reservation)?;
        report.queue_wait += queue_wait;
        let read_len = actual_windows
            .checked_add(config.window_len.saturating_sub(1))
            .ok_or_else(|| anyhow!("chunk read length overflowed"))?;
        let digit_read = reader_pool.read_range_ready(offset, read_len)?;
        if digit_read.len() < config.window_len {
            drop(lease);
            if config.growing {
                let metrics = source.request_generation(
                    GenerationDemand {
                        absolute_target: generation_target(offset, required_window, &config)?,
                        window_len: config.window_len,
                        generator_fixed_bytes: GENERATOR_FIXED_BYTES,
                        cpu_workers: generation_cpu_workers(budget.cpu_permits_max()?),
                    },
                    Arc::clone(&budget) as Arc<dyn crate::pi::GenerationBudget>,
                    Arc::clone(&stop),
                )?;
                record_generation_metrics(&mut report, metrics);
                continue;
            }
            break;
        }
        let actual_windows = (digit_read.len() - config.window_len + 1).min(actual_windows);
        let digits = digit_read.to_vec();
        let chunk = SourceChunk {
            _lease: lease,
            start_offset: offset,
            start_scanned: scanned,
            actual_windows,
            digits,
            read: digit_read.read_time(),
            parse: digit_read.parse_time(),
            cache_hit: digit_read.cache_hit_time(),
        };
        let queue_started = Instant::now();
        if tx.send(chunk).is_err() {
            break;
        }
        report.queue_wait += queue_started.elapsed();
        offset = offset.saturating_add(u64::try_from(actual_windows)?);
        scanned = scanned.saturating_add(u64::try_from(actual_windows)?);
    }

    Ok((reader_pool, report))
}

const fn generation_cpu_workers(total_permits: usize) -> usize {
    if total_permits > 1 {
        total_permits - 1
    } else {
        1
    }
}

fn generation_target(offset: u64, required_window: u64, config: &ProducerConfig) -> Result<u64> {
    let demand_horizon = config.chunk_windows.max(config.window_len);
    Ok(offset
        .saturating_add(required_window)
        .saturating_add(u64::try_from(demand_horizon)?))
}

fn initial_prefetch_target(
    source_len: u64,
    start_offset: u64,
    required_window: u64,
    config: &ProducerConfig,
    horizon: u64,
) -> u64 {
    let bounded_target = [
        config.limit.map(|limit| {
            start_offset
                .saturating_add(limit)
                .saturating_add(required_window)
        }),
        config
            .max_offset
            .map(|offset| offset.saturating_add(required_window)),
    ]
    .into_iter()
    .flatten()
    .min();
    bounded_target.unwrap_or_else(|| source_len.saturating_add(horizon))
}

fn record_generation_metrics(report: &mut ProducerReport, metrics: GenerationMetrics) {
    report.generator_wait = metrics.generator_wait;
    report.producer_epochs = metrics.producer_epochs;
    report.coalesced_requests = metrics.coalesced_request_count;
    report.generation_batches = metrics.generation_batches;
    report.event_wake_latency = metrics.event_wake_latency;
    report.lead_digits = metrics.lead_digits;
    report.high_water_digits = metrics.high_water_digits;
}

#[cfg(test)]
mod prefetch_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    use anyhow::Result;

    struct PrefetchSource {
        prefetches: AtomicUsize,
    }

    impl DigitSource for PrefetchSource {
        fn kind(&self) -> &'static str {
            "test-growing"
        }

        fn len(&self) -> Result<u64> {
            Ok(100)
        }

        fn validate(&self) -> Result<()> {
            Ok(())
        }

        fn read_range(&self, _offset: u64, len: usize) -> Result<Vec<u8>> {
            Ok(vec![3; len])
        }

        fn is_growing(&self) -> bool {
            true
        }

        fn prefetch_generation(
            &self,
            _demand: GenerationDemand,
            _budget: Arc<dyn crate::pi::GenerationBudget>,
            _stop: Arc<AtomicBool>,
        ) -> Result<()> {
            self.prefetches.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn accelerator_producer_prefetches_before_the_cache_edge() -> Result<()> {
        let source = PrefetchSource {
            prefetches: AtomicUsize::new(0),
        };
        let budget = ResourceBudget::new(100, 64, 1)?;
        let reader_pool = DigitReaderPool::new(&source, 1, 1, 3)?;
        let (sender, _receiver) = std::sync::mpsc::sync_channel(100);
        let config = ProducerConfig {
            start_offset: 0,
            start_scanned: 0,
            max_offset: None,
            limit: Some(99),
            chunk_windows: 2,
            window_len: 2,
            growing: true,
            accelerator: true,
        };

        produce(
            sender,
            reader_pool,
            &source,
            config,
            budget,
            Arc::new(AtomicBool::new(false)),
        )?;

        assert!(source.prefetches.load(Ordering::SeqCst) > 0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_reserves_one_cpu_permit_for_search_when_capacity_allows() {
        // Given: a shared four-permit CPU budget.
        let total_permits = 4;

        // When: the search producer chooses the generation width.
        let generation_permits = generation_cpu_workers(total_permits);

        // Then: generation uses only the capacity not reserved for search.
        assert_eq!(generation_permits, 3);
        assert_eq!(generation_cpu_workers(1), 1);
    }

    #[test]
    fn producer_report_excludes_separately_reported_cache_publication() {
        // Given: cumulative producer metrics with distinct compute and publication durations.
        let mut report = ProducerReport::default();
        let metrics = GenerationMetrics {
            generator_wait: Duration::from_millis(2),
            cache_write: Duration::from_millis(9),
            ..GenerationMetrics::default()
        };

        // When: the search producer records generation completion.
        record_generation_metrics(&mut report, metrics);

        // Then: generator wait contains compute only, without adding publication elapsed again.
        assert_eq!(report.generator_wait, Duration::from_millis(2));
    }
}
