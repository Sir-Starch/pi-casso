use num_traits::ToPrimitive;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ProcessMemorySample {
    pub current_bytes: u64,
    pub peak_bytes: u64,
}

#[cfg(target_os = "linux")]
pub(super) fn sample() -> ProcessMemorySample {
    std::fs::read_to_string("/proc/self/status").map_or_else(
        |_| ProcessMemorySample::default(),
        |status| ProcessMemorySample {
            current_bytes: parse_kib(&status, "VmRSS:"),
            peak_bytes: parse_kib(&status, "VmHWM:"),
        },
    )
}

#[cfg(not(target_os = "linux"))]
pub(super) const fn sample() -> ProcessMemorySample {
    ProcessMemorySample {
        current_bytes: 0,
        peak_bytes: 0,
    }
}

#[cfg(target_os = "linux")]
fn parse_kib(status: &str, key: &str) -> u64 {
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix(key)?
                .split_ascii_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })
        .and_then(|kib| kib.checked_mul(1_024))
        .unwrap_or(0)
}

pub(super) fn bytes_to_mb(bytes: u64) -> f64 {
    bytes.to_f64().unwrap_or(f64::MAX) / 1_048_576.0
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::parse_kib;

    #[test]
    fn parses_linux_status_kib_as_bytes() {
        let status = "Name:\tpi-casso\nVmRSS:\t   1234 kB\nVmHWM:\t5678 kB\n";

        assert_eq!(parse_kib(status, "VmRSS:"), 1_263_616);
        assert_eq!(parse_kib(status, "VmHWM:"), 5_814_272);
        assert_eq!(parse_kib(status, "VmSwap:"), 0);
    }
}
