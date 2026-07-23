use std::thread;
use std::time::{Duration, Instant};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::search::MatchMode;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceProfile {
    Eco,
    Balanced,
    Performance,
    Max,
    Custom,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum SearchBackendChoice {
    Cpu,
    Gpu,
    Auto,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum GeneratorBackendChoice {
    Cpu,
    YCruncher,
    Auto,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum GpuMode {
    Off,
    On,
    Auto,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ThermalMode {
    Quiet,
    Normal,
    Aggressive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub cpu_workers: usize,
    pub cpu_utilization: u8,
    pub gpu_utilization: u8,
    pub chunk_size: usize,
    pub queue_depth: usize,
    pub memory_limit_mb: usize,
    pub ui_refresh_ms: u64,
    pub checkpoint_every_secs: u64,
    pub background_yield_ms: u64,
    pub max_fps: u32,
    pub pause_when_on_battery: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PerformanceSettings {
    pub profile: PerformanceProfile,
    pub backend: SearchBackendChoice,
    pub generator_backend: GeneratorBackendChoice,
    pub gpu: GpuMode,
    pub gpu_device: Option<String>,
    pub thermal_mode: ThermalMode,
    pub stress_test: bool,
    pub show_metrics: bool,
    pub limits: ResourceLimits,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeMetrics {
    pub profile: PerformanceProfile,
    pub search_backend: String,
    pub generator_backend: String,
    pub cpu_workers: usize,
    pub cpu_utilization_target: u8,
    pub gpu_status: String,
    pub gpu_device: Option<String>,
    pub gpu_utilization_target: u8,
    pub chunk_size: usize,
    pub queue_depth: usize,
    pub memory_limit_mb: usize,
    pub memory_estimate_mb: usize,
    pub chunk_processing_ms: f64,
    pub checkpoint_count: u64,
    pub tui_refresh_ms: u64,
    pub max_fps: u32,
    pub thermal_mode: ThermalMode,
    pub throttling_active: bool,
    pub pause_when_on_battery: bool,
    pub battery_throttle_active: bool,
    pub generated_digits: u64,
    pub searched_offset: u64,
    pub cache_gap_digits: u64,
}

#[derive(Clone, Debug, Default)]
pub struct PerformanceOverrides {
    pub cpu_workers: Option<usize>,
    pub cpu_utilization: Option<u8>,
    pub gpu_utilization: Option<u8>,
    pub chunk_size: Option<usize>,
    pub queue_depth: Option<usize>,
    pub memory_limit_mb: Option<usize>,
    pub ui_refresh_ms: Option<u64>,
    pub checkpoint_every_secs: Option<u64>,
    pub background_yield_ms: Option<u64>,
    pub max_fps: Option<u32>,
    pub pause_when_on_battery: bool,
}

impl PerformanceProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eco => "eco",
            Self::Balanced => "balanced",
            Self::Performance => "performance",
            Self::Max => "max",
            Self::Custom => "custom",
        }
    }
}

impl SearchBackendChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
            Self::Auto => "auto",
        }
    }
}

impl GeneratorBackendChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::YCruncher => "y-cruncher",
            Self::Auto => "auto",
        }
    }
}

impl GpuMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Auto => "auto",
        }
    }
}

impl ThermalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::Normal => "normal",
            Self::Aggressive => "aggressive",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Quiet => Self::Normal,
            Self::Normal => Self::Aggressive,
            Self::Aggressive => Self::Quiet,
        }
    }
}

impl PerformanceSettings {
    #[allow(clippy::too_many_arguments)]
    pub fn from_profile(
        profile: PerformanceProfile,
        backend: SearchBackendChoice,
        generator_backend: GeneratorBackendChoice,
        gpu: GpuMode,
        gpu_device: Option<String>,
        thermal_mode: ThermalMode,
        stress_test: bool,
        show_metrics: bool,
        match_mode: MatchMode,
        overrides: PerformanceOverrides,
    ) -> Self {
        let logical = std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1);
        let mut limits = ResourceLimits::for_profile(profile, thermal_mode, logical, match_mode);
        limits.apply(overrides);
        Self {
            profile,
            backend,
            generator_backend,
            gpu,
            gpu_device,
            thermal_mode,
            stress_test,
            show_metrics,
            limits,
        }
    }

    pub fn resolve_backend(&self) -> (&'static str, String) {
        match (self.backend, self.gpu) {
            (SearchBackendChoice::Cpu, _) | (_, GpuMode::Off) => ("cpu", "disabled".to_string()),
            (SearchBackendChoice::Gpu, _) | (_, GpuMode::On) => (
                "gpu",
                "requested; runtime fallback to cpu if unavailable".to_string(),
            ),
            (SearchBackendChoice::Auto, GpuMode::Auto) => (
                "auto",
                "cpu for small work; gpu for large emergence/stress work when available"
                    .to_string(),
            ),
        }
    }

    pub fn throttle_after_batch(&self, active: Duration) -> bool {
        let utilization = self.limits.cpu_utilization.clamp(1, 100);
        let mut sleep_for = Duration::from_millis(self.limits.background_yield_ms);
        if utilization < 100 {
            let active_secs = active.as_secs_f64();
            let target_total = active_secs * (100.0 / utilization as f64);
            let idle_secs = (target_total - active_secs).max(0.0);
            sleep_for += Duration::from_secs_f64(idle_secs.min(0.5));
        }
        if sleep_for.is_zero() {
            return false;
        }
        thread::sleep(sleep_for);
        true
    }

    pub fn estimated_memory_mb(&self, window_len: usize) -> usize {
        let chunk_bytes = self
            .limits
            .chunk_size
            .saturating_add(window_len)
            .saturating_mul(self.limits.queue_depth.max(1));
        chunk_bytes.div_ceil(1024 * 1024).max(1)
    }
}

impl ResourceLimits {
    fn for_profile(
        profile: PerformanceProfile,
        thermal_mode: ThermalMode,
        logical_cores: usize,
        match_mode: MatchMode,
    ) -> Self {
        let emergence = match_mode == MatchMode::Emergence;
        let base_chunk = if emergence { 65_536 } else { 50_000 };
        let mut limits = match profile {
            PerformanceProfile::Eco => Self {
                cpu_workers: logical_cores.clamp(1, 2),
                cpu_utilization: 25,
                gpu_utilization: 0,
                chunk_size: (base_chunk / 4).max(64),
                queue_depth: 1,
                memory_limit_mb: 128,
                ui_refresh_ms: 750,
                checkpoint_every_secs: 10,
                background_yield_ms: 25,
                max_fps: 2,
                pause_when_on_battery: false,
            },
            PerformanceProfile::Balanced | PerformanceProfile::Custom => Self {
                cpu_workers: (logical_cores / 2).max(1),
                cpu_utilization: 60,
                gpu_utilization: 50,
                chunk_size: base_chunk,
                queue_depth: 2,
                memory_limit_mb: 512,
                ui_refresh_ms: 150,
                checkpoint_every_secs: 5,
                background_yield_ms: 0,
                max_fps: 10,
                pause_when_on_battery: false,
            },
            PerformanceProfile::Performance => Self {
                cpu_workers: logical_cores.saturating_sub(1).max(1),
                cpu_utilization: 90,
                gpu_utilization: 80,
                chunk_size: base_chunk.saturating_mul(4),
                queue_depth: 3,
                memory_limit_mb: 1024,
                ui_refresh_ms: 80,
                checkpoint_every_secs: 5,
                background_yield_ms: 0,
                max_fps: 20,
                pause_when_on_battery: false,
            },
            PerformanceProfile::Max => Self {
                cpu_workers: logical_cores.max(1),
                cpu_utilization: 100,
                gpu_utilization: 100,
                chunk_size: base_chunk.saturating_mul(8),
                queue_depth: 4,
                memory_limit_mb: 2048,
                ui_refresh_ms: 50,
                checkpoint_every_secs: 3,
                background_yield_ms: 0,
                max_fps: 30,
                pause_when_on_battery: false,
            },
        };

        match thermal_mode {
            ThermalMode::Quiet => {
                limits.cpu_utilization = limits.cpu_utilization.min(35);
                limits.ui_refresh_ms = limits.ui_refresh_ms.max(750);
                limits.background_yield_ms = limits.background_yield_ms.max(30);
                limits.chunk_size = (limits.chunk_size / 2).max(32);
                limits.max_fps = limits.max_fps.min(3);
            }
            ThermalMode::Normal => {}
            ThermalMode::Aggressive => {
                limits.cpu_utilization = limits.cpu_utilization.max(85);
                limits.chunk_size = limits.chunk_size.saturating_mul(2);
                limits.ui_refresh_ms = limits.ui_refresh_ms.min(80);
                limits.background_yield_ms = 0;
            }
        }
        limits
    }

    fn apply(&mut self, overrides: PerformanceOverrides) {
        if let Some(value) = overrides.cpu_workers {
            self.cpu_workers = value.max(1);
        }
        if let Some(value) = overrides.cpu_utilization {
            self.cpu_utilization = value.clamp(1, 100);
        }
        if let Some(value) = overrides.gpu_utilization {
            self.gpu_utilization = value.clamp(1, 100);
        }
        if let Some(value) = overrides.chunk_size {
            self.chunk_size = value.max(1);
        }
        if let Some(value) = overrides.queue_depth {
            self.queue_depth = value.max(1);
        }
        if let Some(value) = overrides.memory_limit_mb {
            self.memory_limit_mb = value.max(1);
        }
        if let Some(value) = overrides.ui_refresh_ms {
            self.ui_refresh_ms = value.max(16);
        }
        if let Some(value) = overrides.checkpoint_every_secs {
            self.checkpoint_every_secs = value.max(1);
        }
        if let Some(value) = overrides.background_yield_ms {
            self.background_yield_ms = value;
        }
        if let Some(value) = overrides.max_fps {
            self.max_fps = value.max(1);
        }
        if overrides.pause_when_on_battery {
            self.pause_when_on_battery = true;
        }
    }
}

impl RuntimeMetrics {
    #[allow(clippy::too_many_arguments)]
    pub fn from_settings(
        settings: &PerformanceSettings,
        chunk_processing: Duration,
        checkpoint_count: u64,
        generated_digits: u64,
        searched_offset: u64,
        cache_gap_digits: u64,
        window_len: usize,
        throttling_active: bool,
        battery_throttle_active: bool,
    ) -> Self {
        let (backend, gpu_status) = settings.resolve_backend();
        Self {
            profile: settings.profile,
            search_backend: backend.to_string(),
            generator_backend: settings.generator_backend.as_str().to_string(),
            cpu_workers: settings.limits.cpu_workers,
            cpu_utilization_target: settings.limits.cpu_utilization,
            gpu_status,
            gpu_device: settings.gpu_device.clone(),
            gpu_utilization_target: settings.limits.gpu_utilization,
            chunk_size: settings.limits.chunk_size,
            queue_depth: settings.limits.queue_depth,
            memory_limit_mb: settings.limits.memory_limit_mb,
            memory_estimate_mb: settings.estimated_memory_mb(window_len),
            chunk_processing_ms: chunk_processing.as_secs_f64() * 1000.0,
            checkpoint_count,
            tui_refresh_ms: settings.limits.ui_refresh_ms,
            max_fps: settings.limits.max_fps,
            thermal_mode: settings.thermal_mode,
            throttling_active,
            pause_when_on_battery: settings.limits.pause_when_on_battery,
            battery_throttle_active,
            generated_digits,
            searched_offset,
            cache_gap_digits,
        }
    }
}

pub fn on_battery_power() -> Option<bool> {
    let entries = std::fs::read_dir("/sys/class/power_supply").ok()?;
    let mut saw_ac = false;
    for entry in entries.flatten() {
        let ty = std::fs::read_to_string(entry.path().join("type")).ok()?;
        if ty.trim() == "Mains" || ty.trim() == "USB" || ty.trim() == "USB_C" {
            saw_ac = true;
            let online = std::fs::read_to_string(entry.path().join("online")).ok()?;
            if online.trim() == "1" {
                return Some(false);
            }
        }
    }
    saw_ac.then_some(true)
}

pub fn measure_elapsed(start: Instant) -> Duration {
    start.elapsed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_defaults_scale_worker_counts() {
        let eco = ResourceLimits::for_profile(
            PerformanceProfile::Eco,
            ThermalMode::Normal,
            16,
            MatchMode::Emergence,
        );
        let max = ResourceLimits::for_profile(
            PerformanceProfile::Max,
            ThermalMode::Normal,
            16,
            MatchMode::Emergence,
        );
        assert_eq!(eco.cpu_workers, 2);
        assert_eq!(max.cpu_workers, 16);
        assert!(eco.chunk_size < max.chunk_size);
    }

    #[test]
    fn custom_overrides_are_clamped() {
        let settings = PerformanceSettings::from_profile(
            PerformanceProfile::Custom,
            SearchBackendChoice::Cpu,
            GeneratorBackendChoice::Cpu,
            GpuMode::Off,
            None,
            ThermalMode::Normal,
            false,
            false,
            MatchMode::Threshold,
            PerformanceOverrides {
                cpu_workers: Some(0),
                cpu_utilization: Some(0),
                chunk_size: Some(0),
                queue_depth: Some(0),
                ..PerformanceOverrides::default()
            },
        );
        assert_eq!(settings.limits.cpu_workers, 1);
        assert_eq!(settings.limits.cpu_utilization, 1);
        assert_eq!(settings.limits.chunk_size, 1);
        assert_eq!(settings.limits.queue_depth, 1);
    }

    #[test]
    fn gpu_backend_reports_runtime_fallback_policy() {
        let settings = PerformanceSettings::from_profile(
            PerformanceProfile::Performance,
            SearchBackendChoice::Gpu,
            GeneratorBackendChoice::Cpu,
            GpuMode::On,
            None,
            ThermalMode::Normal,
            false,
            false,
            MatchMode::Emergence,
            PerformanceOverrides::default(),
        );
        let (backend, status) = settings.resolve_backend();
        assert_eq!(backend, "gpu");
        assert!(status.contains("fallback"));
    }
}
