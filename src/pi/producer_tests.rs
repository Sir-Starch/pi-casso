use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

use super::{CachedGrowingPiSource, GenerationBackend, GenerationDemand, PiCache};
use crate::digits::DigitSource;
use crate::pi::GenerationBudget;
use crate::pi::producer::{
    CHUDNOVSKY_PREFIX_CACHE_DIGITS, ChudnovskyPrefixCache, CoordinatedRayonPool,
    generation_wait_duration,
};
use crate::search::ResourceBudget;

const WINDOW_LEN: usize = 4096;
const FIXED_BYTES: u64 = 4096;
const KNOWN_PI_PREFIX_128: &str = "31415926535897932384626433832795028841971693993751058209749445923078164062862089986280348253421170679821480865132823066470938446";

fn demand(absolute_target: u64) -> GenerationDemand {
    GenerationDemand {
        absolute_target,
        window_len: WINDOW_LEN,
        generator_fixed_bytes: FIXED_BYTES,
        cpu_workers: 4,
    }
}

fn single_worker_demand(absolute_target: u64) -> GenerationDemand {
    GenerationDemand {
        cpu_workers: 1,
        ..demand(absolute_target)
    }
}

fn paused_source(backend: GenerationBackend) -> (TempDir, Arc<CachedGrowingPiSource>) {
    let directory = tempdir().expect("temporary cache directory");
    let source = Arc::new(CachedGrowingPiSource::new_paused_for_test(
        PiCache::new(directory.path().join("pi.txt")),
        backend,
    ));
    (directory, source)
}

#[test]
fn generation_accounting_excludes_separately_reported_cache_write() {
    // Given: one producer batch whose total blocking time includes persistence.
    let total_blocking = Duration::from_millis(17);
    let cache_write = Duration::from_millis(9);

    // When: producer compute/blocking time is accounted.
    let generation_compute = generation_wait_duration(total_blocking, cache_write);

    // Then: persistence remains excluded from the generation wait metric.
    assert_eq!(generation_compute, Duration::from_millis(8));
}

#[test]
fn coordinated_chudnovsky_workers_retain_one_bounded_pool_for_differing_widths() {
    // Given: fresh coordinated pool state.
    let mut workers = CoordinatedRayonPool::default();

    // When: differing parallel widths are requested and then reused.
    let direct_rejected = workers.pool(1).is_err();
    let first_two = workers.pool(2).expect("first two-worker pool") as *const rayon::ThreadPool;
    let first_width = workers
        .pool(2)
        .expect("first pool width")
        .current_num_threads();
    let differing_width =
        workers.pool(4).expect("reused pool at differing width") as *const rayon::ThreadPool;
    let second_two = workers.pool(2).expect("reused two-worker pool") as *const rayon::ThreadPool;
    let retained_width = workers
        .pool(4)
        .expect("reused pool width")
        .current_num_threads();

    // Then: one-worker work stays direct and the bounded pool is never recreated.
    assert!(direct_rejected);
    assert_eq!(first_two, second_two);
    assert_eq!(first_two, differing_width);
    assert_eq!(workers.retained_pool_count(), 1);
    assert_eq!(first_width, 2);
    assert_eq!(retained_width, first_width);
}

#[test]
fn process_lifetime_chudnovsky_prefix_cache_is_bounded_and_reusable() {
    // Given: a deterministic prefix cache and inputs on both sides of its fixed bound.
    let mut cache = ChudnovskyPrefixCache::default();
    let short_prefix = vec![3_u8, 1, 4, 1, 5, 9];
    let oversized_prefix = vec![0_u8; CHUDNOVSKY_PREFIX_CACHE_DIGITS + 1];

    // When: a reusable prefix is remembered before an oversized result is offered.
    cache.remember(&short_prefix);
    cache.remember(&oversized_prefix);

    // Then: available digits are reused and the oversized result cannot expand retained memory.
    assert_eq!(cache.prefix(4), Some(vec![3, 1, 4, 1]));
    assert_eq!(cache.retained_digits(), short_prefix.len());
}

#[test]
#[cfg_attr(
    all(windows, target_env = "msvc"),
    ignore = "Chudnovsky generation fails on Windows MSVC"
)]
fn chudnovsky_prefetches_to_the_bounded_geometric_high_water() {
    // Given: one request below the process-lifetime Chudnovsky prefix ceiling.
    let directory = tempdir().expect("temporary cache directory");
    let source = CachedGrowingPiSource::new_with_backend_for_test(
        PiCache::new(directory.path().join("pi.txt")),
        GenerationBackend::Chudnovsky,
    );
    let budget = ResourceBudget::new(1, 1, 1).expect("resource budget");

    // When: the single-worker producer completes the requested target.
    let metrics = source
        .request_generation(
            single_worker_demand(70_000),
            budget,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("bounded Chudnovsky generation");

    // Then: the published lead reaches the next geometric boundary without exceeding the cap.
    assert_eq!(metrics.high_water_digits, 131_072);
    assert_eq!(metrics.generated_source_digits, 131_072);
    assert_eq!(source.len().expect("cache length"), 131_072);
}

#[test]
#[cfg_attr(
    all(windows, target_env = "msvc"),
    ignore = "Chudnovsky generation fails on Windows MSVC"
)]
fn chudnovsky_geometric_prefetch_respects_the_live_resource_budget() {
    // Given: a resource budget that can retain only 47,952 lead digits after fixed metadata.
    let directory = tempdir().expect("temporary cache directory");
    let source = CachedGrowingPiSource::new_with_backend_for_test(
        PiCache::new(directory.path().join("pi.txt")),
        GenerationBackend::Chudnovsky,
    );
    let budget = ResourceBudget::new_bytes(1, 100_000, 1).expect("resource budget");

    // When: the requested target would otherwise round up to 65,536 digits.
    let metrics = source
        .request_generation(
            single_worker_demand(33_000),
            budget,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("resource-bounded Chudnovsky generation");

    // Then: publication stops at the budget-derived high water and still completes the request.
    assert_eq!(metrics.high_water_digits, 47_952);
    assert_eq!(metrics.generated_source_digits, 47_952);
    assert_eq!(source.len().expect("cache length"), 47_952);
}

#[test]
fn accelerator_prefetch_queues_without_waiting_for_the_digits() {
    let (_directory, source) = paused_source(GenerationBackend::Spigot);
    let budget = ResourceBudget::new(1, 64, 1).expect("resource budget");
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let prefetch_source = Arc::clone(&source);

    let handle = thread::spawn(move || {
        sender.send(prefetch_source.queue_generation_prefetch(
            single_worker_demand(10_000),
            budget,
            stop,
        ))
    });
    receiver
        .recv_timeout(Duration::from_millis(100))
        .expect("prefetch must return while the producer is paused")
        .expect("prefetch request");
    handle
        .join()
        .expect("prefetch thread joins")
        .expect("prefetch result is delivered");

    assert_eq!(
        source
            .pending_demand_slots_for_test()
            .expect("pending slots"),
        1
    );
    source.shutdown().expect("producer joins cleanly");
}

#[test]
#[cfg_attr(
    all(windows, target_env = "msvc"),
    ignore = "Chudnovsky generation fails on Windows MSVC"
)]
fn pi_generation_coalesces_high_water_requests() {
    // Given: four requests are barrier-released while the producer is paused.
    let (_directory, source) = paused_source(GenerationBackend::Chudnovsky);
    let budget = ResourceBudget::new(8, 512, 4).expect("resource budget");
    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(5));
    let targets = [1_000_u64, 10_000, 100_000, 250_000];

    thread::scope(|scope| {
        let mut waiters = Vec::new();
        for target in targets {
            let source = Arc::clone(&source);
            let budget = Arc::clone(&budget);
            let stop = Arc::clone(&stop);
            let barrier = Arc::clone(&barrier);
            waiters.push(scope.spawn(move || {
                barrier.wait();
                source.request_generation(demand(target), budget, stop)
            }));
        }

        // When: all four requests are pending before one producer wakeup.
        barrier.wait();
        source
            .wait_for_pending_requests(4)
            .expect("all requests become pending");
        assert_eq!(
            source
                .pending_demand_slots_for_test()
                .expect("pending slots"),
            1
        );
        source.resume_producer();
        for waiter in waiters {
            waiter
                .join()
                .expect("request thread joins")
                .expect("generation succeeds");
        }
    });

    // Then: one epoch reaches the maximum target in one bounded high-water batch.
    let metrics = source.metrics().expect("producer metrics");
    assert_eq!(metrics.concurrent_requests, 4);
    assert_eq!(metrics.coalesced_request_count, 4);
    assert_eq!(metrics.producer_epochs, 1);
    assert_eq!(metrics.generation_batches, 1);
    assert_eq!(metrics.chudnovsky_target_computations, 1);
    assert_eq!(metrics.high_water_digits, 250_000);
    assert!(metrics.lead_digits > WINDOW_LEN as u64);
    assert!(metrics.event_wake_latency < Duration::from_millis(20));
    assert_eq!(source.len().expect("cache length"), 250_000);
    let snapshot = budget.snapshot();
    assert!(snapshot.cpu_permits_peak <= snapshot.cpu_permits_max);
    assert_eq!(snapshot.generator_leases_peak, 1);
}

#[test]
fn pi_spigot_producer_continues_without_replaying_from_zero() {
    // Given: a fresh producer forced onto the persistent spigot backend.
    let directory = tempdir().expect("temporary cache directory");
    let source = CachedGrowingPiSource::new_with_backend_for_test(
        PiCache::new(directory.path().join("pi.txt")),
        GenerationBackend::Spigot,
    );
    let budget = ResourceBudget::new(2, 64, 1).expect("resource budget");
    let generation_budget: Arc<dyn GenerationBudget> = budget;
    let stop = Arc::new(AtomicBool::new(false));

    // When: two sequential target extensions are published.
    source
        .request_generation(
            single_worker_demand(64),
            Arc::clone(&generation_budget),
            Arc::clone(&stop),
        )
        .expect("first generation");
    source
        .request_generation(single_worker_demand(128), generation_budget, stop)
        .expect("second generation");

    // Then: the published bytes equal an independently known prefix, so replay,
    // skip, or a reset between epochs changes the observable output.
    let metrics = source.metrics().expect("producer metrics");
    assert_eq!(metrics.producer_epochs, 2);
    assert_eq!(metrics.generation_batches, 2);
    assert_eq!(source.len().expect("cache length"), 128);
    let expected = KNOWN_PI_PREFIX_128
        .bytes()
        .map(|digit| digit - b'0')
        .collect::<Vec<_>>();
    assert_eq!(
        source.read_range(0, 128).expect("published prefix"),
        expected
    );
}

#[test]
fn fresh_spigot_producer_continues_a_prepopulated_cache_prefix() {
    // Given: a cache populated before this producer instance is created.
    let directory = tempdir().expect("temporary cache directory");
    let cache = PiCache::new(directory.path().join("pi.txt"));
    let expected = KNOWN_PI_PREFIX_128
        .bytes()
        .map(|digit| digit - b'0')
        .collect::<Vec<_>>();
    cache
        .append_digits(&expected[..64])
        .expect("prepopulate cache prefix");
    let source = CachedGrowingPiSource::new_with_backend_for_test(cache, GenerationBackend::Spigot);

    // When: the fresh producer extends the existing prefix.
    source
        .request_generation(
            single_worker_demand(128),
            ResourceBudget::new(2, 64, 1).expect("resource budget"),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("extend prepopulated cache");

    // Then: the complete cache remains the canonical pi prefix.
    assert_eq!(
        source.read_range(0, 128).expect("published prefix"),
        expected
    );
}

#[test]
fn pi_producer_shutdown_wakes_waiters() {
    // Given: one waiter blocked behind a deliberately paused producer.
    let (_directory, source) = paused_source(GenerationBackend::Spigot);
    let budget = ResourceBudget::new(2, 64, 1).expect("resource budget");
    let stop = Arc::new(AtomicBool::new(false));
    let waiter_source = Arc::clone(&source);
    let waiter = thread::spawn(move || {
        waiter_source.request_generation(single_worker_demand(10_000), budget, stop)
    });
    source
        .wait_for_pending_requests(1)
        .expect("request becomes pending");

    // When: shutdown is signaled before the producer starts the epoch.
    source.shutdown().expect("producer joins cleanly");

    // Then: the blocked waiter observes the shared shutdown error and no thread leaks.
    let error = waiter
        .join()
        .expect("waiter joins")
        .expect_err("shutdown must fail");
    assert!(error.to_string().contains("shut down"));
    assert!(source.producer_joined_for_test());
}

#[test]
fn pi_producer_cancellation_wakes_a_coalesced_waiter() {
    // Given: one admitted waiter behind a paused producer.
    let (_directory, source) = paused_source(GenerationBackend::Spigot);
    let waiter_source = Arc::clone(&source);
    let waiter = thread::spawn(move || {
        waiter_source.request_generation(
            single_worker_demand(10_000),
            ResourceBudget::new(2, 64, 1).expect("resource budget"),
            Arc::new(AtomicBool::new(false)),
        )
    });
    source
        .wait_for_pending_requests(1)
        .expect("request is admitted");

    // When: cancellation is broadcast before generation starts.
    source.cancel_waiters();

    // Then: the waiter observes cancellation and the producer still shuts down cleanly.
    let error = waiter
        .join()
        .expect("waiter joins")
        .expect_err("cancel must fail");
    assert!(error.to_string().contains("cancelled"));
    source.shutdown().expect("producer joins cleanly");
}

#[test]
fn active_spigot_generation_stops_promptly_when_waiters_are_cancelled() {
    // Given: a spigot epoch that is actively generating a deliberately distant target.
    let (_directory, source) = paused_source(GenerationBackend::Spigot);
    let waiter_source = Arc::clone(&source);
    let waiter = thread::spawn(move || {
        waiter_source.request_generation(
            single_worker_demand(10_000_000),
            ResourceBudget::new(2, 64, 1).expect("resource budget"),
            Arc::new(AtomicBool::new(false)),
        )
    });
    source
        .wait_for_pending_requests(1)
        .expect("request is admitted");
    source.resume_producer();
    source.wait_for_active_for_test().expect("epoch is active");

    // When: pipeline cancellation is broadcast during active generation.
    let cancelled_at = Instant::now();
    source.cancel_waiters();
    let error = waiter
        .join()
        .expect("waiter joins")
        .expect_err("cancel must fail");
    source.shutdown().expect("producer joins cleanly");

    // Then: both the waiter and producer stop without finishing the distant target.
    assert!(error.to_string().contains("cancelled"));
    assert!(cancelled_at.elapsed() < Duration::from_secs(1));
    assert!(source.len().expect("cache length") < 10_000_000);
}
