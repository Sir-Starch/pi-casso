//! Number and duration formatting shared by the TUI and the plain CLI output.
//! Previously duplicated verbatim in `ui.rs` and `interactive.rs`.

use std::time::Duration;

/// Compact rate: `412K`, `1.2M`. Used for windows/sec and digits/sec.
pub fn fmt_rate(value: f64) -> String {
    if !value.is_finite() || value < 0.0 {
        return "-".to_string();
    }
    if value >= 1_000_000_000.0 {
        format!("{:.1}G", value / 1_000_000_000.0)
    } else if value >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("{:.1}K", value / 1_000.0)
    } else {
        format!("{value:.0}")
    }
}

pub fn fmt_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

/// Offsets run into the billions, where a bare digit run is unreadable.
/// Thin-space grouping keeps the column narrow while staying scannable.
pub fn fmt_count(value: u64, unicode: bool) -> String {
    let digits = value.to_string();
    let separator = if unicode { '\u{202f}' } else { ' ' };
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(separator);
        }
        out.push(ch);
    }
    out
}

pub fn fmt_percent(ratio: f64) -> String {
    format!("{:.2}%", ratio * 100.0)
}

/// Byte counts for the pi cache on disk.
pub fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

/// Truncates on character boundaries, not bytes, so multi-byte run names
/// cannot be cut mid-character.
pub fn truncate(value: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

pub fn opt_u64(value: Option<u64>) -> String {
    value
        .map(|inner| inner.to_string())
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_thresholds() {
        assert_eq!(fmt_rate(0.0), "0");
        assert_eq!(fmt_rate(999.0), "999");
        assert_eq!(fmt_rate(1_000.0), "1.0K");
        assert_eq!(fmt_rate(999_999.0), "1000.0K");
        assert_eq!(fmt_rate(1_000_000.0), "1.0M");
        assert_eq!(fmt_rate(2_500_000_000.0), "2.5G");
        assert_eq!(fmt_rate(f64::NAN), "-");
    }

    #[test]
    fn duration_pads_each_field() {
        assert_eq!(fmt_duration(Duration::from_secs(0)), "00:00:00");
        assert_eq!(fmt_duration(Duration::from_secs(61)), "00:01:01");
        assert_eq!(fmt_duration(Duration::from_secs(3661)), "01:01:01");
        assert_eq!(fmt_duration(Duration::from_secs(360_000)), "100:00:00");
    }

    #[test]
    fn counts_group_by_thousands() {
        assert_eq!(fmt_count(0, false), "0");
        assert_eq!(fmt_count(999, false), "999");
        assert_eq!(fmt_count(1_000, false), "1 000");
        assert_eq!(fmt_count(1_284_991, false), "1 284 991");
        assert_eq!(fmt_count(1_000, true), "1\u{202f}000");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdef", 4), "abc…");
        // Would panic on a byte-slice implementation.
        assert_eq!(truncate("подмена", 4), "под…");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn bytes_scale() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2048), "2.0 KB");
        assert_eq!(fmt_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}
