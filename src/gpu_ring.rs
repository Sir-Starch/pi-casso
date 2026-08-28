use std::collections::BTreeSet;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RingTelemetry {
    pub(crate) overlap: Duration,
    pub(crate) submissions: u64,
    pub(crate) completions: u64,
    pub(crate) max_in_flight: u64,
    pub(crate) overlap_events: u64,
}

pub(crate) fn test_mode_enabled() -> bool {
    (cfg!(debug_assertions) || cfg!(test))
        && std::env::var("PI_CASSO_TEST_MODE").is_ok_and(|value| value == "1")
}

pub(crate) fn test_mock_enabled() -> bool {
    test_mode_enabled()
        && std::env::var_os("PI_CASSO_TEST_FAKE_WGPU_PREFLIGHT").is_some()
        && std::env::var_os("PI_CASSO_TEST_FAKE_WGPU_EXECUTION").is_some()
}

pub(crate) fn test_runtime_fault_for(backend: &str) -> bool {
    test_mode_enabled()
        && std::env::var("PI_CASSO_TEST_STRESS_RUNTIME_FAULT").is_ok_and(|value| value == backend)
}

pub(crate) fn test_backend_mock_enabled(backend: &str) -> bool {
    match backend {
        "gpu" | "wgpu" => test_mock_enabled(),
        "cuda" => {
            test_mode_enabled()
                && std::env::var_os("PI_CASSO_TEST_FAKE_CUDA_PREFLIGHT").is_some()
                && std::env::var_os("PI_CASSO_TEST_FAKE_CUDA_EXECUTION").is_some()
        }
        _ => false,
    }
}

pub(crate) fn run_mock_ring(windows: usize, depth: usize) -> RingTelemetry {
    let batch_count = depth.max(1).min(windows.max(1));
    let delay = test_mode_enabled()
        .then(|| std::env::var("PI_CASSO_TEST_GPU_COMPLETION_DELAY_MS").ok())
        .flatten()
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(Duration::ZERO, Duration::from_millis);
    let (tx, rx) = mpsc::sync_channel(batch_count);
    let mut telemetry = RingTelemetry::default();
    let mut in_flight = 0_u64;
    let mut overlap_started = None;

    for sequence in 0..batch_count {
        let completion = tx.clone();
        thread::spawn(move || {
            if !delay.is_zero() {
                thread::sleep(delay);
            }
            let _ = completion.send(sequence);
        });
        telemetry.submissions = telemetry.submissions.saturating_add(1);
        in_flight = in_flight.saturating_add(1);
        telemetry.max_in_flight = telemetry.max_in_flight.max(in_flight);
        if in_flight == 2 {
            telemetry.overlap_events = telemetry.overlap_events.saturating_add(1);
            overlap_started = Some(Instant::now());
        }
    }
    drop(tx);

    let mut completed = BTreeSet::new();
    let mut next = 0_usize;
    while let Ok(sequence) = rx.recv() {
        completed.insert(sequence);
        while completed.remove(&next) {
            telemetry.completions = telemetry.completions.saturating_add(1);
            in_flight = in_flight.saturating_sub(1);
            if in_flight == 1 {
                if let Some(started) = overlap_started.take() {
                    telemetry.overlap += started.elapsed();
                }
            }
            next = next.saturating_add(1);
        }
    }
    telemetry
}

#[cfg(test)]
mod tests {
    use super::run_mock_ring;

    #[test]
    fn ring_depth_bounds_completion_and_preserves_order() {
        let serial = run_mock_ring(16, 1);
        let overlapped = run_mock_ring(16, 4);

        assert_eq!(serial.submissions, serial.completions);
        assert_eq!(serial.max_in_flight, 1);
        assert_eq!(overlapped.submissions, overlapped.completions);
        assert_eq!(overlapped.max_in_flight, 4);
        assert!(overlapped.overlap_events > 0);
    }
}
