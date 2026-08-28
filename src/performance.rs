use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::benchmark_report::{
    MemoryReport, QueueReport, ReducerReport, SourceReport, StageTimings, Waits,
};
use crate::search::{MatchMode, SnapshotIncompatible};

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
    Cuda,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_utilization: Option<u8>,
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
    pub stage_timings: StageTimings,
    pub waits: Waits,
    pub source: SourceReport,
    pub queue: QueueReport,
    pub memory: MemoryReport,
    pub reducer: ReducerReport,
    pub cpu_permits_in_use: u64,
    pub cpu_permits_peak: u64,
    pub cpu_permits_max: u64,
    pub resolved_backend: String,
    pub backend_device: String,
    pub backend_feature_available: bool,
    pub backend_fault_status: String,
    pub fallback: bool,
    pub fallback_reason: String,
    pub fallback_count: u64,
    pub gpu_submissions: u64,
    pub gpu_completions: u64,
    pub gpu_buffer_creations: u64,
    pub gpu_bind_group_creations: u64,
    pub gpu_resource_reuses: u64,
    pub gpu_overlap_ms: u64,
    pub gpu_max_in_flight: u64,
    pub gpu_overlap_events: u64,
    pub gpu_test_only_mock: bool,
    pub gpu_duty_wait_ms: u64,
    pub gpu_initial_submission_wait_ms: u64,
    pub active_submission_ratio: f64,
    pub dispatch_quantum_ratio: f64,
    pub generator_digits_per_second: f64,
    pub telemetry_enabled: bool,
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

pub const PERFORMANCE_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const GPU_DUTY_WINDOW_MS: u64 = 1_000;

#[derive(Clone, Copy, Debug, Default)]
pub struct GpuDutyMetrics {
    pub wait: Duration,
    pub initial_submission_wait: Duration,
    pub active_submission_ratio: f64,
    pub dispatch_quantum_ratio: f64,
}

#[derive(Debug)]
pub struct GpuDutyPolicy {
    percent: u8,
    tokens_ms: f64,
    last_refill: Instant,
    observation_start: Instant,
    steady_state_start: Option<Instant>,
    first_submission: bool,
    total_wait: Duration,
    initial_submission_wait: Duration,
    all_active: Duration,
    steady_state_active: Duration,
    max_dispatch: Duration,
}

impl GpuDutyPolicy {
    pub fn new(percent: u8) -> Self {
        Self::new_at(percent, Instant::now())
    }

    fn new_at(percent: u8, now: Instant) -> Self {
        Self {
            percent,
            tokens_ms: GPU_DUTY_WINDOW_MS as f64,
            last_refill: now,
            observation_start: now,
            steady_state_start: None,
            first_submission: true,
            total_wait: Duration::ZERO,
            initial_submission_wait: Duration::ZERO,
            all_active: Duration::ZERO,
            steady_state_active: Duration::ZERO,
            max_dispatch: Duration::ZERO,
        }
    }

    pub fn wait_before_submission(&mut self) {
        let wait = self.required_wait_at(Instant::now());
        if !wait.is_zero() {
            thread::sleep(wait);
            self.record_wait_at(wait, Instant::now());
        }
    }

    pub fn record_submission(&mut self, active: Duration) {
        self.record_submission_at(active, Instant::now());
    }

    pub fn metrics(&self) -> GpuDutyMetrics {
        self.metrics_at(Instant::now())
    }

    fn refill_at(&mut self, now: Instant) {
        if self.percent == 0 || self.percent == 100 {
            self.last_refill = now;
            return;
        }
        let elapsed_ms = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64()
            * 1_000.0;
        self.tokens_ms = (self.tokens_ms + elapsed_ms * f64::from(self.percent) / 100.0)
            .min(GPU_DUTY_WINDOW_MS as f64);
        self.last_refill = now;
    }

    fn required_wait_at(&mut self, now: Instant) -> Duration {
        self.refill_at(now);
        let wait = if self.percent == 0 || self.percent == 100 || self.tokens_ms > 0.0 {
            Duration::ZERO
        } else {
            let refill_per_ms = f64::from(self.percent) / 100.0;
            Duration::from_secs_f64(((-self.tokens_ms + 1.0) / refill_per_ms) / 1_000.0)
        };
        if self.first_submission {
            self.initial_submission_wait = wait;
            self.first_submission = false;
        }
        wait
    }

    fn record_wait_at(&mut self, wait: Duration, now: Instant) {
        self.total_wait = self.total_wait.saturating_add(wait);
        self.refill_at(now);
    }

    fn record_submission_at(&mut self, active: Duration, now: Instant) {
        if self.percent == 0 {
            return;
        }
        self.refill_at(now);
        self.all_active = self.all_active.saturating_add(active);
        self.max_dispatch = self.max_dispatch.max(active);
        if self.steady_state_start.is_some() {
            self.steady_state_active = self.steady_state_active.saturating_add(active);
        }
        if self.percent < 100 {
            let consumed_ms = (active.as_secs_f64() * 1_000.0).min(GPU_DUTY_WINDOW_MS as f64);
            self.tokens_ms -= consumed_ms;
            if self.steady_state_start.is_none() && self.tokens_ms <= 0.0 {
                self.steady_state_start = Some(now);
            }
        }
    }

    fn metrics_at(&self, now: Instant) -> GpuDutyMetrics {
        let (active, interval) = match self.steady_state_start {
            Some(start) => (
                self.steady_state_active,
                now.saturating_duration_since(start),
            ),
            None => (
                self.all_active,
                now.saturating_duration_since(self.observation_start),
            ),
        };
        let interval_seconds = interval.as_secs_f64();
        let active_submission_ratio = if self.percent == 0 || interval_seconds == 0.0 {
            0.0
        } else {
            (active.as_secs_f64() / interval_seconds).min(1.0)
        };
        let dispatch_quantum_ratio = if self.percent == 0 || interval_seconds == 0.0 {
            0.0
        } else {
            (self.max_dispatch.as_secs_f64() / interval_seconds).min(1.0)
        };
        GpuDutyMetrics {
            wait: self.total_wait,
            initial_submission_wait: self.initial_submission_wait,
            active_submission_ratio,
            dispatch_quantum_ratio,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PerformanceSnapshot {
    pub schema_version: u32,
    pub settings: PerformanceSettings,
    pub current_offset: Option<u64>,
    pub work_windows: Option<u64>,
    pub limit: Option<u64>,
    pub max_offset: Option<u64>,
    pub keep_going_after_perfect: bool,
    pub no_tui: bool,
    pub legacy_extra: Map<String, Value>,
}

impl PerformanceSnapshot {
    pub fn from_settings(
        settings: PerformanceSettings,
        current_offset: Option<u64>,
        work_windows: Option<u64>,
        limit: Option<u64>,
    ) -> Self {
        Self {
            schema_version: PERFORMANCE_SNAPSHOT_SCHEMA_VERSION,
            settings,
            current_offset,
            work_windows,
            limit,
            max_offset: None,
            keep_going_after_perfect: false,
            no_tui: false,
            legacy_extra: Map::new(),
        }
    }

    pub fn encode_value(&self) -> Value {
        let mut value = json!({
            "schema_version": self.schema_version,
            "settings": self.settings,
            "current_offset": self.current_offset,
            "work_windows": self.work_windows,
            "limit": self.limit,
            "max_offset": self.max_offset,
            "keep_going_after_perfect": self.keep_going_after_perfect,
            "no_tui": self.no_tui,
        });
        if let Value::Object(object) = &mut value {
            for (key, extra) in &self.legacy_extra {
                object.entry(key.clone()).or_insert_with(|| extra.clone());
            }
        }
        value
    }

    pub fn decode_value(value: Value) -> Result<Self> {
        let schema_version = value.get("schema_version").and_then(Value::as_u64);
        Self::decode_value_inner(value).map_err(|error| {
            anyhow::Error::new(SnapshotIncompatible::new(
                format!("{error:#}"),
                schema_version,
            ))
        })
    }

    fn decode_value_inner(value: Value) -> Result<Self> {
        let mut object = value
            .as_object()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("performance snapshot must be a JSON object"))?;
        let schema_version = object
            .remove("schema_version")
            .map(|value| {
                serde_json::from_value(value)
                    .with_context(|| "performance snapshot field schema_version has the wrong type")
            })
            .transpose()?;
        match schema_version {
            None | Some(0) => Self::decode_legacy(object),
            Some(PERFORMANCE_SNAPSHOT_SCHEMA_VERSION) => Self::decode_v1(object),
            Some(version) => bail!("unsupported performance snapshot schema version {version}"),
        }
    }

    fn decode_v1(mut object: Map<String, Value>) -> Result<Self> {
        let settings = take_required::<PerformanceSettings>(&mut object, "settings")?;
        validate_settings(&settings)?;
        Ok(Self {
            schema_version: PERFORMANCE_SNAPSHOT_SCHEMA_VERSION,
            settings,
            current_offset: take_nullable(&mut object, "current_offset")?,
            work_windows: take_nullable(&mut object, "work_windows")?,
            limit: take_nullable(&mut object, "limit")?,
            max_offset: take_nullable(&mut object, "max_offset")?,
            keep_going_after_perfect: take_optional(&mut object, "keep_going_after_perfect")?
                .unwrap_or(false),
            no_tui: take_optional(&mut object, "no_tui")?.unwrap_or(false),
            legacy_extra: object,
        })
    }

    fn decode_legacy(mut object: Map<String, Value>) -> Result<Self> {
        let profile =
            take_optional(&mut object, "profile")?.unwrap_or(PerformanceProfile::Balanced);
        let backend = take_optional(&mut object, "backend")?.unwrap_or(SearchBackendChoice::Auto);
        let generator_backend = take_optional(&mut object, "generator_backend")?
            .unwrap_or(GeneratorBackendChoice::Auto);
        let gpu = legacy_gpu(object.remove("gpu"))?;
        let gpu_device = take_optional::<String>(&mut object, "gpu_device")?;
        let device = take_optional::<String>(&mut object, "device")?;
        if gpu_device.is_some() && device.is_some() {
            bail!("legacy gpu_device and device aliases conflict");
        }
        let gpu_device = gpu_device.or(device).or_else(|| Some("auto".to_string()));
        let cpu_workers = legacy_workers(&mut object)?;
        let thermal_mode =
            take_optional(&mut object, "thermal_mode")?.unwrap_or(ThermalMode::Normal);
        let keep_going_after_perfect =
            take_optional(&mut object, "keep_going_after_perfect")?.unwrap_or(false);
        let no_tui = take_optional(&mut object, "no_tui")?.unwrap_or(false);
        let overrides = PerformanceOverrides {
            cpu_workers,
            cpu_utilization: take_optional(&mut object, "cpu_utilization")?,
            gpu_utilization: take_optional(&mut object, "gpu_utilization")?,
            chunk_size: take_optional(&mut object, "chunk_size")?,
            queue_depth: take_optional(&mut object, "queue_depth")?,
            memory_limit_mb: take_optional(&mut object, "memory_limit_mb")?,
            ui_refresh_ms: take_optional(&mut object, "ui_refresh_ms")?,
            checkpoint_every_secs: take_optional(&mut object, "checkpoint_every")?,
            background_yield_ms: take_optional(&mut object, "background_yield_ms")?,
            max_fps: take_optional(&mut object, "max_fps")?,
            pause_when_on_battery: take_optional(&mut object, "pause_when_on_battery")?
                .unwrap_or(false),
        };
        validate_overrides(&overrides)?;
        let settings = PerformanceSettings::from_profile(
            profile,
            backend,
            generator_backend,
            gpu,
            gpu_device,
            thermal_mode,
            take_optional(&mut object, "stress_test")?.unwrap_or(false),
            take_optional(&mut object, "show_metrics")?.unwrap_or(false),
            MatchMode::Emergence,
            overrides,
        );
        let current_offset = take_optional(&mut object, "current_offset")?;
        let offset = take_optional(&mut object, "offset")?;
        if current_offset.is_some() && offset.is_some() {
            bail!("legacy current_offset and offset aliases conflict");
        }
        validate_settings(&settings)?;
        Ok(Self {
            schema_version: PERFORMANCE_SNAPSHOT_SCHEMA_VERSION,
            settings,
            current_offset: current_offset.or(offset),
            work_windows: take_optional(&mut object, "work_windows")?,
            limit: take_optional(&mut object, "limit")?,
            max_offset: take_optional(&mut object, "max_offset")?,
            keep_going_after_perfect,
            no_tui,
            legacy_extra: object,
        })
    }
}

fn take_required<T: DeserializeOwned>(object: &mut Map<String, Value>, key: &str) -> Result<T> {
    let value = object
        .remove(key)
        .ok_or_else(|| anyhow::anyhow!("performance snapshot is missing {key}"))?;
    serde_json::from_value(value)
        .with_context(|| format!("performance snapshot field {key} has the wrong type"))
}

fn take_optional<T: DeserializeOwned>(
    object: &mut Map<String, Value>,
    key: &str,
) -> Result<Option<T>> {
    match object.remove(key) {
        None => Ok(None),
        Some(value) => serde_json::from_value(value)
            .map(Some)
            .with_context(|| format!("performance snapshot field {key} has the wrong type")),
    }
}

fn take_nullable<T: DeserializeOwned>(
    object: &mut Map<String, Value>,
    key: &str,
) -> Result<Option<T>> {
    match object.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value)
            .map(Some)
            .with_context(|| format!("performance snapshot field {key} has the wrong type")),
    }
}

fn legacy_workers(object: &mut Map<String, Value>) -> Result<Option<usize>> {
    let workers = take_optional(object, "workers")?;
    let cpu_workers = take_optional(object, "cpu_workers")?;
    if workers.is_some() && cpu_workers.is_some() {
        bail!("legacy workers and cpu_workers aliases conflict");
    }
    Ok(cpu_workers.or(workers))
}

fn legacy_gpu(value: Option<Value>) -> Result<GpuMode> {
    match value {
        None => Ok(GpuMode::Auto),
        Some(Value::Bool(false)) => Ok(GpuMode::Off),
        Some(Value::Bool(true)) => Ok(GpuMode::On),
        Some(Value::String(value)) => match value.as_str() {
            "auto" => Ok(GpuMode::Auto),
            "on" => Ok(GpuMode::On),
            "off" => Ok(GpuMode::Off),
            _ => bail!("legacy gpu has an invalid value"),
        },
        Some(_) => bail!("legacy gpu has the wrong type"),
    }
}

fn validate_settings(settings: &PerformanceSettings) -> Result<()> {
    if settings.limits.cpu_workers == 0
        || settings.limits.cpu_utilization == 0
        || settings.limits.cpu_utilization > 100
        || settings
            .limits
            .gpu_utilization
            .is_some_and(|value| value > 100)
        || settings.limits.chunk_size == 0
        || settings.limits.queue_depth == 0
        || settings.limits.memory_limit_mb == 0
        || !(16..=60_000).contains(&settings.limits.ui_refresh_ms)
        || !(1..=120).contains(&settings.limits.max_fps)
        || settings.limits.checkpoint_every_secs == 0
    {
        bail!("performance snapshot contains an invalid resource limit");
    }
    Ok(())
}

fn validate_overrides(overrides: &PerformanceOverrides) -> Result<()> {
    if overrides.cpu_workers == Some(0)
        || overrides.cpu_utilization.is_some_and(|value| value == 0)
        || overrides.chunk_size == Some(0)
        || overrides.queue_depth == Some(0)
        || overrides.memory_limit_mb == Some(0)
        || overrides
            .ui_refresh_ms
            .is_some_and(|value| !(16..=60_000).contains(&value))
        || overrides
            .max_fps
            .is_some_and(|value| !(1..=120).contains(&value))
        || overrides.checkpoint_every_secs == Some(0)
    {
        bail!("performance snapshot contains an invalid legacy resource limit");
    }
    if overrides.cpu_utilization.is_some_and(|value| value > 100)
        || overrides.gpu_utilization.is_some_and(|value| value > 100)
    {
        bail!("performance snapshot contains an invalid utilization");
    }
    Ok(())
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
            Self::Cuda => "cuda",
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
    // CLIPPY-ALLOW: preserve the existing public settings-construction API.
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
            (SearchBackendChoice::Gpu | SearchBackendChoice::Cuda, _) | (_, GpuMode::On) => (
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
            if !sleep_for.is_zero() {
                sleep_for = sleep_for.max(Duration::from_millis(1));
            }
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

    pub fn effective_gpu_utilization(&self) -> u8 {
        self.limits.gpu_utilization.unwrap_or(match self.profile {
            PerformanceProfile::Eco => 0,
            PerformanceProfile::Balanced | PerformanceProfile::Custom => 50,
            PerformanceProfile::Performance => 80,
            PerformanceProfile::Max => 100,
        })
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
                gpu_utilization: Some(0),
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
                gpu_utilization: Some(50),
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
                gpu_utilization: Some(80),
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
                gpu_utilization: Some(100),
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
            self.gpu_utilization = Some(value.min(100));
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
    // CLIPPY-ALLOW: preserve the existing public metrics-construction API.
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
            gpu_utilization_target: settings.effective_gpu_utilization(),
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
            stage_timings: StageTimings::default(),
            waits: Waits::default(),
            source: SourceReport::default(),
            queue: QueueReport {
                global_limit: u64::try_from(settings.limits.queue_depth).unwrap_or(u64::MAX),
                permits: u64::try_from(settings.limits.queue_depth).unwrap_or(u64::MAX),
                ..QueueReport::default()
            },
            memory: MemoryReport {
                logical_budget_mb: settings.limits.memory_limit_mb as f64,
                ..MemoryReport::default()
            },
            reducer: ReducerReport {
                ordered: true,
                ..ReducerReport::default()
            },
            cpu_permits_in_use: 0,
            cpu_permits_peak: 0,
            cpu_permits_max: u64::try_from(settings.limits.cpu_workers).unwrap_or(u64::MAX),
            resolved_backend: backend.to_string(),
            backend_device: backend.to_string(),
            backend_feature_available: backend == "cpu",
            backend_fault_status: "none".to_string(),
            fallback: false,
            fallback_reason: String::new(),
            fallback_count: 0,
            gpu_submissions: 0,
            gpu_completions: 0,
            gpu_buffer_creations: 0,
            gpu_bind_group_creations: 0,
            gpu_resource_reuses: 0,
            gpu_overlap_ms: 0,
            gpu_max_in_flight: 0,
            gpu_overlap_events: 0,
            gpu_test_only_mock: false,
            gpu_duty_wait_ms: 0,
            gpu_initial_submission_wait_ms: 0,
            active_submission_ratio: 0.0,
            dispatch_quantum_ratio: 0.0,
            generator_digits_per_second: 0.0,
            telemetry_enabled: settings.show_metrics,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performance_snapshot_schema0_mapping_table() {
        // Given: a legacy payload with every documented field and an unknown key.
        let legacy = serde_json::json!({
            "schema_version": 0,
            "profile": "eco",
            "backend": "cuda",
            "generator_backend": "y_cruncher",
            "workers": 3,
            "gpu": "on",
            "device": "adapter-0",
            "thermal_mode": "quiet",
            "stress_test": true,
            "show_metrics": true,
            "cpu_utilization": 41,
            "gpu_utilization": 0,
            "chunk_size": 4096,
            "queue_depth": 3,
            "memory_limit_mb": 512,
            "ui_refresh_ms": 16,
            "checkpoint_every": 7,
            "background_yield_ms": 9,
            "max_fps": 120,
            "pause_when_on_battery": true,
            "current_offset": 42,
            "work_windows": 77,
            "limit": 88,
            "max_offset": 99,
            "keep_going_after_perfect": true,
            "no_tui": true,
            "future_legacy_key": {"kept": true}
        });

        // When: the payload crosses the versioned snapshot boundary.
        let snapshot = PerformanceSnapshot::decode_value(legacy).expect("legacy snapshot decodes");

        // Then: every field maps exactly, zero remains meaningful, and unknown data survives.
        assert_eq!(snapshot.schema_version, PERFORMANCE_SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(snapshot.settings.profile, PerformanceProfile::Eco);
        assert_eq!(snapshot.settings.backend, SearchBackendChoice::Cuda);
        assert_eq!(
            snapshot.settings.generator_backend,
            GeneratorBackendChoice::YCruncher
        );
        assert_eq!(snapshot.settings.limits.cpu_workers, 3);
        assert_eq!(snapshot.settings.gpu, GpuMode::On);
        assert_eq!(snapshot.settings.gpu_device.as_deref(), Some("adapter-0"));
        assert_eq!(snapshot.settings.thermal_mode, ThermalMode::Quiet);
        assert!(snapshot.settings.stress_test);
        assert!(snapshot.settings.show_metrics);
        assert_eq!(snapshot.settings.limits.cpu_utilization, 41);
        assert_eq!(snapshot.settings.limits.gpu_utilization, Some(0));
        assert_eq!(snapshot.settings.limits.chunk_size, 4096);
        assert_eq!(snapshot.settings.limits.queue_depth, 3);
        assert_eq!(snapshot.settings.limits.memory_limit_mb, 512);
        assert_eq!(snapshot.settings.limits.ui_refresh_ms, 16);
        assert_eq!(snapshot.settings.limits.checkpoint_every_secs, 7);
        assert_eq!(snapshot.settings.limits.background_yield_ms, 9);
        assert_eq!(snapshot.settings.limits.max_fps, 120);
        assert!(snapshot.settings.limits.pause_when_on_battery);
        assert_eq!(snapshot.current_offset, Some(42));
        assert_eq!(snapshot.work_windows, Some(77));
        assert_eq!(snapshot.limit, Some(88));
        assert_eq!(snapshot.max_offset, Some(99));
        assert!(snapshot.keep_going_after_perfect);
        assert!(snapshot.no_tui);
        assert_eq!(snapshot.legacy_extra["future_legacy_key"]["kept"], true);

        for (gpu, expected) in [
            (serde_json::json!(false), GpuMode::Off),
            (serde_json::json!(true), GpuMode::On),
            (serde_json::json!("auto"), GpuMode::Auto),
            (serde_json::json!("on"), GpuMode::On),
            (serde_json::json!("off"), GpuMode::Off),
        ] {
            let value = serde_json::json!({"schema_version": 0, "gpu": gpu});
            let decoded = PerformanceSnapshot::decode_value(value).expect("gpu alias decodes");
            assert_eq!(decoded.settings.gpu, expected);
            assert_eq!(decoded.settings.gpu_device.as_deref(), Some("auto"));
        }

        let no_schema = PerformanceSnapshot::decode_value(serde_json::json!({}))
            .expect("missing schema version is legacy");
        assert_eq!(no_schema.settings.gpu, GpuMode::Auto);
        assert_eq!(no_schema.settings.gpu_device.as_deref(), Some("auto"));
    }

    #[test]
    fn performance_snapshot_rejects_wrong_types_versions_aliases_and_offsets() {
        // Given: malformed schema versions, fields, aliases, offsets, and ranges.
        let mut valid = PerformanceSnapshot::from_settings(
            PerformanceSettings::from_profile(
                PerformanceProfile::Balanced,
                SearchBackendChoice::Cpu,
                GeneratorBackendChoice::Cpu,
                GpuMode::Off,
                None,
                ThermalMode::Normal,
                false,
                false,
                MatchMode::Threshold,
                PerformanceOverrides::default(),
            ),
            Some(4),
            Some(8),
            Some(16),
        )
        .encode_value();
        let mut cases = vec![
            (
                "schema version type",
                serde_json::json!({"schema_version": "1"}),
            ),
            (
                "schema version null",
                serde_json::json!({"schema_version": null}),
            ),
            (
                "unsupported version",
                serde_json::json!({"schema_version": 2}),
            ),
            (
                "settings type",
                serde_json::json!({"schema_version": 1, "settings": []}),
            ),
            (
                "invalid enum",
                serde_json::json!({"schema_version": 0, "profile": "turbo"}),
            ),
            (
                "legacy gpu type",
                serde_json::json!({"schema_version": 0, "gpu": 1}),
            ),
            (
                "conflicting workers aliases",
                serde_json::json!({"schema_version": 0, "workers": 2, "cpu_workers": 3}),
            ),
            (
                "conflicting offset aliases",
                serde_json::json!({"schema_version": 0, "current_offset": 2, "offset": 3}),
            ),
            (
                "negative legacy offset",
                serde_json::json!({"schema_version": 0, "offset": -1}),
            ),
            (
                "fractional current offset",
                serde_json::json!({"schema_version": 1, "current_offset": 1.5}),
            ),
        ];
        valid["settings"]["limits"]["ui_refresh_ms"] = serde_json::json!(15);
        cases.push(("ui refresh range", valid.clone()));
        valid["settings"]["limits"]["ui_refresh_ms"] = serde_json::json!(16);
        valid["settings"]["limits"]["cpu_utilization"] = serde_json::json!(0);
        cases.push(("cpu utilization range", valid.clone()));
        valid["settings"]["limits"]["cpu_utilization"] = serde_json::json!(60);
        valid["settings"]["limits"]["max_fps"] = serde_json::json!(121);
        cases.push(("max fps range", valid.clone()));
        valid["settings"]["limits"]["max_fps"] = serde_json::json!(120);
        valid["settings"]["limits"]["gpu_utilization"] = serde_json::json!(101);
        cases.push(("gpu utilization range", valid));

        // When: each malformed payload crosses the snapshot boundary.
        for (name, value) in cases {
            let error = PerformanceSnapshot::decode_value(value)
                .expect_err("malformed snapshot must be rejected");
            let incompatible = error
                .downcast_ref::<SnapshotIncompatible>()
                .expect("codec errors use the snapshot incompatibility type");

            // Then: callers can classify every codec failure as snapshot_incompatible.
            assert_eq!(incompatible.status, "snapshot_incompatible");
            assert!(
                error.to_string().starts_with("snapshot_incompatible"),
                "{name}: {error:#}"
            );
        }
    }

    #[test]
    fn performance_snapshot_preserves_unknown_fields() {
        // Given: a current snapshot extended by a future writer.
        let mut settings = PerformanceSettings::from_profile(
            PerformanceProfile::Balanced,
            SearchBackendChoice::Cpu,
            GeneratorBackendChoice::Cpu,
            GpuMode::Off,
            None,
            ThermalMode::Normal,
            false,
            false,
            MatchMode::Threshold,
            PerformanceOverrides::default(),
        );
        settings.profile = PerformanceProfile::Performance;
        settings.backend = SearchBackendChoice::Cuda;
        settings.generator_backend = GeneratorBackendChoice::YCruncher;
        settings.gpu = GpuMode::On;
        settings.gpu_device = Some("adapter-7".to_string());
        settings.thermal_mode = ThermalMode::Aggressive;
        settings.stress_test = true;
        settings.show_metrics = true;
        settings.limits = ResourceLimits {
            cpu_workers: 7,
            cpu_utilization: 73,
            gpu_utilization: Some(0),
            chunk_size: 4096,
            queue_depth: 4,
            memory_limit_mb: 768,
            ui_refresh_ms: 16,
            checkpoint_every_secs: 9,
            background_yield_ms: 11,
            max_fps: 120,
            pause_when_on_battery: true,
        };
        let expected_settings = serde_json::to_value(&settings).expect("settings serialize");
        let mut encoded =
            PerformanceSnapshot::from_settings(settings, Some(11), Some(22), Some(33))
                .encode_value();
        encoded["future_field"] = serde_json::json!({"stable": true});

        // When: it is decoded and re-encoded by this version.
        let decoded = PerformanceSnapshot::decode_value(encoded).expect("current snapshot decodes");
        let round_tripped = decoded.encode_value();

        // Then: unsupported data has not been discarded.
        assert_eq!(round_tripped["settings"], expected_settings);
        assert_eq!(round_tripped["current_offset"], 11);
        assert_eq!(round_tripped["work_windows"], 22);
        assert_eq!(round_tripped["limit"], 33);
        assert_eq!(round_tripped["future_field"]["stable"], true);
    }

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
    fn eco_profile_preserves_gpu_utilization_zero() {
        // Given: the eco profile's explicit disabled GPU policy.
        let settings = PerformanceSettings::from_profile(
            PerformanceProfile::Eco,
            SearchBackendChoice::Cpu,
            GeneratorBackendChoice::Cpu,
            GpuMode::Off,
            None,
            ThermalMode::Normal,
            false,
            false,
            MatchMode::Threshold,
            PerformanceOverrides {
                gpu_utilization: Some(0),
                ..PerformanceOverrides::default()
            },
        );

        // When: settings cross the versioned snapshot boundary.
        let encoded = PerformanceSnapshot::from_settings(settings, None, None, None).encode_value();
        let decoded = PerformanceSnapshot::decode_value(encoded.clone()).expect("snapshot decodes");

        // Then: zero remains explicit, while omission remains structurally absent.
        assert_eq!(encoded["settings"]["limits"]["gpu_utilization"], 0);
        assert_eq!(decoded.settings.limits.gpu_utilization, Some(0));
        assert_eq!(decoded.settings.effective_gpu_utilization(), 0);
        let mut omitted = decoded;
        omitted.settings.limits.gpu_utilization = None;
        assert!(PerformanceSnapshot::from_settings(omitted.settings, None, None, None)
            .encode_value()["settings"]["limits"]
            .get("gpu_utilization")
            .is_none());
    }

    #[test]
    fn gpu_utilization_zero_disables_duty_policy() {
        // Given: the explicit zero policy and multiple accelerator completions.
        let started = Instant::now();
        let mut policy = GpuDutyPolicy::new_at(0, started);
        let mut now = started;

        // When: submissions are admitted and completed.
        for _ in 0..16 {
            let wait = policy.required_wait_at(now);
            assert!(wait.is_zero());
            now += Duration::from_millis(150);
            policy.record_submission_at(Duration::from_millis(150), now);
        }
        let metrics = policy.metrics_at(now);

        // Then: zero introduces no throttle and makes no utilization claim.
        assert!(metrics.wait.is_zero());
        assert!(metrics.initial_submission_wait.is_zero());
        assert_eq!(metrics.active_submission_ratio, 0.0);
        assert_eq!(metrics.dispatch_quantum_ratio, 0.0);
    }

    #[test]
    fn gpu_utilization_100_and_50_enforce_steady_state_policy() {
        fn exercise(percent: u8) -> GpuDutyMetrics {
            let started = Instant::now();
            let mut policy = GpuDutyPolicy::new_at(percent, started);
            let mut now = started;
            for submission in 0..24 {
                let wait = policy.required_wait_at(now);
                if submission == 0 {
                    assert!(wait.is_zero());
                }
                if !wait.is_zero() {
                    now += wait;
                    policy.record_wait_at(wait, now);
                }
                now += Duration::from_millis(150);
                policy.record_submission_at(Duration::from_millis(150), now);
            }
            policy.metrics_at(now)
        }

        // Given/When: equal multi-batch workloads run at full and half policy budgets.
        let full = exercise(100);
        let half = exercise(50);

        // Then: full budget never waits and half budget is bounded after initial credit.
        assert!(full.initial_submission_wait.is_zero());
        assert!(half.initial_submission_wait.is_zero());
        assert!(full.wait.is_zero());
        assert!(half.wait > Duration::ZERO);
        assert!(full.active_submission_ratio > 0.0);
        assert!(half.active_submission_ratio > 0.0);
        assert!(
            half.active_submission_ratio <= 0.5 + half.dispatch_quantum_ratio,
            "ratio={} quantum={}",
            half.active_submission_ratio,
            half.dispatch_quantum_ratio
        );
    }

    #[test]
    fn explicit_gpu_utilization_hundred_is_preserved() {
        // Given: an explicit fully enabled GPU policy.
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
                gpu_utilization: Some(100),
                ..PerformanceOverrides::default()
            },
        );

        // When: the explicit override is applied.
        // Then: the upper boundary remains exact.
        assert_eq!(settings.limits.gpu_utilization, Some(100));
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
