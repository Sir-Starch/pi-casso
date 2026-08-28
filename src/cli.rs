use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::performance::{
    GeneratorBackendChoice, GpuMode, PerformanceProfile, SearchBackendChoice, ThermalMode,
};

#[derive(Parser, Debug)]
#[command(
    name = "pi-casso",
    version,
    about = "Find ASCII art hiding in pi digits."
)]
pub struct Cli {
    #[arg(long, global = true, help = "Disable ANSI color output")]
    pub no_color: bool,
    #[arg(
        long,
        global = true,
        help = "Emit machine-readable JSON where supported"
    )]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
#[command(rename_all = "kebab-case")]
pub enum Commands {
    Templates,
    Preview(PreviewArgs),
    Start(StartArgs),
    Hunt(StartArgs),
    Resume(ResumeArgs),
    List,
    Status(RunArg),
    History(HistoryArgs),
    ShowBest(RunArg),
    Export(ExportArgs),
    Delete(RunArg),
    Benchmark(BenchmarkArgs),
    StressTest(StressTestArgs),
    Gpu {
        #[command(subcommand)]
        command: GpuCommands,
    },
    Pi {
        #[command(subcommand)]
        command: PiCommands,
    },
}

#[derive(Subcommand, Debug)]
#[command(rename_all = "kebab-case")]
pub enum GpuCommands {
    Info,
}

#[derive(Subcommand, Debug)]
#[command(rename_all = "kebab-case")]
pub enum PiCommands {
    Import {
        file: PathBuf,
        #[arg(long)]
        allow_decimal_prefix: bool,
    },
    CacheInfo,
    CacheRepair {
        #[arg(long)]
        force: bool,
    },
    Generate {
        #[arg(long)]
        digits: u64,
        #[arg(long, value_enum, default_value_t = GeneratorBackendChoice::Auto)]
        generator_backend: GeneratorBackendChoice,
        #[arg(long)]
        y_cruncher_path: Option<PathBuf>,
        #[arg(long)]
        workers: Option<usize>,
    },
    Benchmark(PiBenchmarkArgs),
    Info,
}

#[derive(Args, Debug)]
pub struct PiBenchmarkArgs {
    #[arg(long, value_delimiter = ',', default_value = "1000,10000,100000")]
    pub targets: Vec<u64>,
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub repetitions: u32,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(0..=100))]
    pub warmup: u32,
    #[arg(long, value_enum, default_value_t = GeneratorBackendChoice::Auto)]
    pub generator_backend: GeneratorBackendChoice,
    #[arg(long)]
    pub y_cruncher_path: Option<PathBuf>,
    #[arg(long, default_value_t = 4)]
    pub workers: usize,
    #[arg(long, value_enum, default_value_t = PiDemandMode::Serial)]
    pub demand_mode: PiDemandMode,
    #[arg(long, default_value_t = 4096)]
    pub search_work_windows: u64,
}

#[derive(Args, Debug)]
pub struct PreviewArgs {
    #[arg(long)]
    pub template: Option<String>,
    #[arg(long)]
    pub file: Option<PathBuf>,
    #[arg(long, value_enum)]
    pub mode: Option<SizeMode>,
    #[arg(long)]
    pub width: Option<usize>,
    #[arg(long)]
    pub height: Option<usize>,
    #[arg(long)]
    pub empty: Option<String>,
    #[arg(long)]
    pub filled: Option<String>,
}

#[derive(Args, Debug)]
pub struct StartArgs {
    #[arg(long)]
    pub template: Option<String>,
    #[arg(long)]
    pub file: Option<PathBuf>,
    #[arg(long)]
    pub name: String,
    #[arg(long, value_enum)]
    pub mode: Option<SizeMode>,
    #[arg(long)]
    pub width: Option<usize>,
    #[arg(long)]
    pub height: Option<usize>,
    #[arg(long, value_enum, default_value_t = MatchModeArg::Emergence)]
    pub match_mode: MatchModeArg,
    #[arg(long)]
    pub canvas_width: Option<usize>,
    #[arg(long)]
    pub canvas_height: Option<usize>,
    #[arg(long)]
    pub empty: Option<String>,
    #[arg(long)]
    pub filled: Option<String>,
    #[arg(long)]
    pub start_offset: Option<u64>,
    #[arg(long)]
    pub max_offset: Option<u64>,
    #[arg(long)]
    pub limit: Option<u64>,
    #[arg(long)]
    pub work_windows: Option<u64>,
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(0..=9))]
    pub threshold: u8,
    #[arg(long)]
    pub invert: bool,
    #[arg(long)]
    pub workers: Option<usize>,
    #[arg(long)]
    pub cpu_workers: Option<usize>,
    #[arg(long, default_value_t = 5)]
    pub checkpoint_every: u64,
    #[arg(long, default_value_t = 10)]
    pub top: usize,
    #[arg(long)]
    pub no_tui: bool,
    #[arg(long)]
    pub pi_file: Option<PathBuf>,
    #[arg(long)]
    pub allow_decimal_prefix: bool,
    #[arg(long)]
    pub infinite: bool,
    #[arg(long)]
    pub keep_going_after_perfect: bool,
    #[arg(long, value_enum, default_value_t = PerformanceProfile::Balanced)]
    pub profile: PerformanceProfile,
    #[arg(long, value_enum, default_value_t = SearchBackendChoice::Auto)]
    pub backend: SearchBackendChoice,
    #[arg(long, value_enum, default_value_t = GeneratorBackendChoice::Auto)]
    pub generator_backend: GeneratorBackendChoice,
    #[arg(long, value_enum, default_value_t = GpuMode::Auto)]
    pub gpu: GpuMode,
    #[arg(long)]
    pub gpu_device: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=100))]
    pub cpu_utilization: Option<u8>,
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub gpu_utilization: Option<u8>,
    #[arg(long)]
    pub chunk_size: Option<usize>,
    #[arg(long)]
    pub queue_depth: Option<usize>,
    #[arg(long)]
    pub memory_limit_mb: Option<usize>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(16..=60_000))]
    pub ui_refresh_ms: Option<u64>,
    #[arg(long, value_enum, default_value_t = ThermalMode::Normal)]
    pub thermal_mode: ThermalMode,
    #[arg(long)]
    pub background_yield_ms: Option<u64>,
    #[arg(long)]
    pub pause_when_on_battery: bool,
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=120))]
    pub max_fps: Option<u32>,
    #[arg(long)]
    pub benchmark: bool,
    #[arg(long)]
    pub stress_test: bool,
    #[arg(long)]
    pub show_metrics: bool,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct ResumeArgs {
    pub run: String,
    #[arg(long, allow_hyphen_values = true)]
    pub max_offset: Option<i64>,
    #[arg(long)]
    pub limit: Option<u64>,
    #[arg(long)]
    pub work_windows: Option<u64>,
    #[arg(long)]
    pub workers: Option<usize>,
    #[arg(long)]
    pub cpu_workers: Option<usize>,
    #[arg(long)]
    pub checkpoint_every: Option<u64>,
    #[arg(long)]
    pub top: Option<usize>,
    #[arg(long)]
    pub no_tui: bool,
    #[arg(long, conflicts_with = "no_tui")]
    pub tui: bool,
    #[arg(long, conflicts_with = "stop_after_perfect")]
    pub keep_going_after_perfect: bool,
    #[arg(long, conflicts_with = "keep_going_after_perfect")]
    pub stop_after_perfect: bool,
    #[arg(long, value_enum)]
    pub profile: Option<PerformanceProfile>,
    #[arg(long, value_enum)]
    pub backend: Option<SearchBackendChoice>,
    #[arg(long, value_enum)]
    pub generator_backend: Option<GeneratorBackendChoice>,
    #[arg(long, value_enum)]
    pub gpu: Option<GpuMode>,
    #[arg(long)]
    pub gpu_device: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=100))]
    pub cpu_utilization: Option<u8>,
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub gpu_utilization: Option<u8>,
    #[arg(long)]
    pub chunk_size: Option<usize>,
    #[arg(long)]
    pub queue_depth: Option<usize>,
    #[arg(long)]
    pub memory_limit_mb: Option<usize>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(16..=60_000))]
    pub ui_refresh_ms: Option<u64>,
    #[arg(long, value_enum)]
    pub thermal_mode: Option<ThermalMode>,
    #[arg(long)]
    pub background_yield_ms: Option<u64>,
    #[arg(long, conflicts_with = "allow_on_battery")]
    pub pause_when_on_battery: bool,
    #[arg(long, conflicts_with = "pause_when_on_battery")]
    pub allow_on_battery: bool,
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=120))]
    pub max_fps: Option<u32>,
    #[arg(long, conflicts_with = "no_stress_test")]
    pub stress_test: bool,
    #[arg(long, conflicts_with = "stress_test")]
    pub no_stress_test: bool,
    #[arg(long, conflicts_with = "checkpoint")]
    pub stress_no_checkpoint: bool,
    #[arg(long, conflicts_with = "stress_no_checkpoint")]
    pub checkpoint: bool,
    #[arg(long, conflicts_with = "no_show_metrics")]
    pub show_metrics: bool,
    #[arg(long, conflicts_with = "show_metrics")]
    pub no_show_metrics: bool,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub force: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResumeBooleanOverrides {
    pub keep_going_after_perfect: Option<bool>,
    pub no_tui: Option<bool>,
    pub show_metrics: Option<bool>,
    pub pause_when_on_battery: Option<bool>,
    pub stress_test: Option<bool>,
    pub stress_no_checkpoint: Option<bool>,
}

impl ResumeArgs {
    pub const fn booleans(&self) -> ResumeBooleanOverrides {
        ResumeBooleanOverrides {
            keep_going_after_perfect: explicit_bool(
                self.keep_going_after_perfect,
                self.stop_after_perfect,
            ),
            no_tui: explicit_bool(self.no_tui, self.tui),
            show_metrics: explicit_bool(self.show_metrics, self.no_show_metrics),
            pause_when_on_battery: explicit_bool(self.pause_when_on_battery, self.allow_on_battery),
            stress_test: explicit_bool(self.stress_test, self.no_stress_test),
            stress_no_checkpoint: explicit_bool(self.stress_no_checkpoint, self.checkpoint),
        }
    }

    pub const fn launches_tui(&self) -> bool {
        self.tui
    }
}

const fn explicit_bool(enabled: bool, disabled: bool) -> Option<bool> {
    if enabled {
        Some(true)
    } else if disabled {
        Some(false)
    } else {
        None
    }
}

#[derive(Args, Debug)]
pub struct BenchmarkArgs {
    #[arg(long)]
    pub template: Option<String>,
    #[arg(long, default_value_t = 10)]
    pub seconds: u64,
    #[arg(long)]
    pub work_windows: Option<u64>,
    #[arg(long, default_value_t = 0)]
    pub start_offset: u64,
    #[arg(long, allow_hyphen_values = true)]
    pub max_offset: Option<i64>,
    #[arg(long, value_enum, default_value_t = BenchmarkSourceMode::Finite)]
    pub source_mode: BenchmarkSourceMode,
    #[arg(long, value_enum, default_value_t = BenchmarkCacheState::Cold)]
    pub cache_state: BenchmarkCacheState,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub repetitions: u32,
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=100))]
    pub warmup: u32,
    #[arg(long, value_enum, default_value_t = PerformanceProfile::Balanced)]
    pub profile: PerformanceProfile,
    #[arg(long, value_enum)]
    pub backend: Option<SearchBackendChoice>,
    #[arg(long, value_enum, default_value_t = GeneratorBackendChoice::Auto)]
    pub generator_backend: GeneratorBackendChoice,
    #[arg(long)]
    pub y_cruncher_path: Option<PathBuf>,
    #[arg(long, value_enum)]
    pub gpu: Option<GpuMode>,
    #[arg(long)]
    pub gpu_device: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=100))]
    pub cpu_utilization: Option<u8>,
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub gpu_utilization: Option<u8>,
    #[arg(long)]
    pub cpu_workers: Option<usize>,
    #[arg(long)]
    pub chunk_size: Option<usize>,
    #[arg(long)]
    pub queue_depth: Option<usize>,
    #[arg(long)]
    pub memory_limit_mb: Option<usize>,
    #[arg(long)]
    pub show_metrics: bool,
}

#[derive(Args, Debug)]
pub struct StressTestArgs {
    #[arg(long, value_enum, default_value_t = StressTarget::Cpu)]
    pub stress_target: StressTarget,
    #[arg(long)]
    pub stress_duration: Option<u64>,
    #[arg(long)]
    pub stress_no_checkpoint: bool,
    #[arg(long)]
    pub template: Option<String>,
    #[arg(long, value_enum, default_value_t = PerformanceProfile::Max)]
    pub profile: PerformanceProfile,
    #[arg(long, value_enum)]
    pub backend: Option<SearchBackendChoice>,
    #[arg(long, value_enum)]
    pub gpu: Option<GpuMode>,
    #[arg(long)]
    pub gpu_device: Option<String>,
    #[arg(long)]
    pub workers: Option<usize>,
    #[arg(long)]
    pub cpu_workers: Option<usize>,
    #[arg(long)]
    pub queue_depth: Option<usize>,
    #[arg(long)]
    pub memory_limit_mb: Option<usize>,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct RunArg {
    pub run: String,
}

#[derive(Args, Debug)]
pub struct HistoryArgs {
    pub run: String,
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct ExportArgs {
    pub run: String,
    #[arg(long, value_enum, default_value_t = ExportFormat::Json)]
    pub format: ExportFormat,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum ExportFormat {
    Json,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum SizeMode {
    #[value(name = "8x8")]
    Eight,
    #[value(name = "12x12")]
    Twelve,
    #[value(name = "16x16")]
    Sixteen,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum MatchModeArg {
    Emergence,
    Threshold,
    Exact,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum StressTarget {
    Cpu,
    Gpu,
    Both,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum BenchmarkSourceMode {
    Finite,
    Growing,
}

impl BenchmarkSourceMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Finite => "finite",
            Self::Growing => "growing",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum BenchmarkCacheState {
    Cold,
    Warm,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum PiDemandMode {
    Serial,
    Concurrent,
    SearchOverlap,
}

impl PiDemandMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Serial => "serial",
            Self::Concurrent => "concurrent",
            Self::SearchOverlap => "search-overlap",
        }
    }
}

impl BenchmarkCacheState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Warm => "warm",
        }
    }
}

impl SizeMode {
    pub fn dimensions(self) -> (usize, usize) {
        match self {
            Self::Eight => (8, 8),
            Self::Twelve => (12, 12),
            Self::Sixteen => (16, 16),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn parses_hunt_performance_flags() {
        let cli = Cli::parse_from([
            "pi-casso",
            "hunt",
            "--template",
            "arch",
            "--name",
            "arch-fast",
            "--infinite",
            "--profile",
            "performance",
            "--backend",
            "auto",
            "--cpu-workers",
            "8",
            "--chunk-size",
            "2000",
            "--thermal-mode",
            "aggressive",
        ]);
        let Some(Commands::Hunt(args)) = cli.command else {
            panic!("expected hunt command");
        };
        assert_eq!(args.profile, PerformanceProfile::Performance);
        assert_eq!(args.backend, SearchBackendChoice::Auto);
        assert_eq!(args.cpu_workers, Some(8));
        assert_eq!(args.chunk_size, Some(2000));
        assert_eq!(args.thermal_mode, ThermalMode::Aggressive);
    }

    #[test]
    fn parses_gpu_info_command() {
        let cli = Cli::parse_from(["pi-casso", "gpu", "info"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Gpu {
                command: GpuCommands::Info
            })
        ));
    }

    #[test]
    fn parses_stress_test_command() {
        let cli = Cli::parse_from([
            "pi-casso",
            "stress-test",
            "--stress-target",
            "both",
            "--stress-duration",
            "10",
            "--yes",
        ]);
        let Some(Commands::StressTest(args)) = cli.command else {
            panic!("expected stress-test command");
        };
        assert_eq!(args.stress_target, StressTarget::Both);
        assert_eq!(args.stress_duration, Some(10));
        assert!(args.yes);
    }

    #[test]
    fn resume_args_when_overrides_are_omitted_preserve_presence() {
        // Given: a resume command with no persisted-setting overrides.
        let cli = Cli::parse_from(["pi-casso", "resume", "saved-run"]);

        // When: clap materializes the resume boundary.
        let Some(Commands::Resume(args)) = cli.command else {
            panic!("expected resume command");
        };

        // Then: omission remains distinct from every explicit value.
        assert_eq!(args.profile, None);
        assert_eq!(args.backend, None);
        assert_eq!(args.gpu, None);
        assert_eq!(args.checkpoint_every, None);
        assert_eq!(args.booleans(), ResumeBooleanOverrides::default());
    }

    #[test]
    fn resume_args_when_false_and_zero_are_explicit_preserve_values() {
        // Given: every false/zero form whose presence changes resume precedence.
        let cli = Cli::parse_from([
            "pi-casso",
            "resume",
            "saved-run",
            "--gpu-utilization",
            "0",
            "--stop-after-perfect",
            "--tui",
            "--no-show-metrics",
            "--allow-on-battery",
            "--no-stress-test",
            "--checkpoint",
        ]);

        // When: clap parses the explicit forms.
        let Some(Commands::Resume(args)) = cli.command else {
            panic!("expected resume command");
        };

        // Then: zero and false survive rather than collapsing into omission.
        assert_eq!(args.gpu_utilization, Some(0));
        assert_eq!(
            args.booleans(),
            ResumeBooleanOverrides {
                keep_going_after_perfect: Some(false),
                no_tui: Some(false),
                show_metrics: Some(false),
                pause_when_on_battery: Some(false),
                stress_test: Some(false),
                stress_no_checkpoint: Some(false),
            }
        );
        assert!(args.launches_tui());
    }

    #[test]
    fn benchmark_args_preserve_explicit_gpu_utilization_zero() {
        // Given: a benchmark command with the disabled GPU duty sentinel.
        let cli = Cli::parse_from(["pi-casso", "benchmark", "--gpu-utilization", "0"]);

        // When: clap materializes benchmark arguments.
        let Some(Commands::Benchmark(args)) = cli.command else {
            panic!("expected benchmark command");
        };

        // Then: zero remains explicit rather than collapsing into omission.
        assert_eq!(args.gpu_utilization, Some(0));
    }
}
