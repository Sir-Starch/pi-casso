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
    Info,
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
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=100))]
    pub gpu_utilization: Option<u8>,
    #[arg(long)]
    pub chunk_size: Option<usize>,
    #[arg(long)]
    pub queue_depth: Option<usize>,
    #[arg(long)]
    pub memory_limit_mb: Option<usize>,
    #[arg(long)]
    pub ui_refresh_ms: Option<u64>,
    #[arg(long, value_enum, default_value_t = ThermalMode::Normal)]
    pub thermal_mode: ThermalMode,
    #[arg(long)]
    pub background_yield_ms: Option<u64>,
    #[arg(long)]
    pub pause_when_on_battery: bool,
    #[arg(long)]
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
    #[arg(long)]
    pub max_offset: Option<u64>,
    #[arg(long)]
    pub limit: Option<u64>,
    #[arg(long)]
    pub workers: Option<usize>,
    #[arg(long)]
    pub cpu_workers: Option<usize>,
    #[arg(long, default_value_t = 5)]
    pub checkpoint_every: u64,
    #[arg(long)]
    pub top: Option<usize>,
    #[arg(long)]
    pub no_tui: bool,
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
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=100))]
    pub gpu_utilization: Option<u8>,
    #[arg(long)]
    pub chunk_size: Option<usize>,
    #[arg(long)]
    pub queue_depth: Option<usize>,
    #[arg(long)]
    pub memory_limit_mb: Option<usize>,
    #[arg(long)]
    pub ui_refresh_ms: Option<u64>,
    #[arg(long, value_enum, default_value_t = ThermalMode::Normal)]
    pub thermal_mode: ThermalMode,
    #[arg(long)]
    pub background_yield_ms: Option<u64>,
    #[arg(long)]
    pub pause_when_on_battery: bool,
    #[arg(long)]
    pub max_fps: Option<u32>,
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
pub struct BenchmarkArgs {
    #[arg(long)]
    pub template: Option<String>,
    #[arg(long, default_value_t = 10)]
    pub seconds: u64,
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
    #[arg(long)]
    pub cpu_workers: Option<usize>,
    #[arg(long)]
    pub chunk_size: Option<usize>,
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
    #[arg(long, value_enum, default_value_t = SearchBackendChoice::Auto)]
    pub backend: SearchBackendChoice,
    #[arg(long, value_enum, default_value_t = GpuMode::Auto)]
    pub gpu: GpuMode,
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
}
