//! One module per family of CLI commands.
//!
//! `main.rs` used to be a 900-line file holding the dispatch table, the
//! y-cruncher orchestration, the benchmark harness and the stress-test builder
//! all at once. Each of those now lives next to the command that needs it.

mod bench;
mod gpu;
mod hunt;
mod pi;
mod runs;

use std::io::IsTerminal;

use anyhow::Result;

use crate::cli::{Cli, Commands, GpuCommands};
use crate::config::Config;
use crate::render::Theme;

/// Everything a command handler needs that is not specific to the command:
/// resolved user preferences, the palette to draw with, and the output mode.
pub struct CommandContext {
    pub config: Config,
    pub theme: Theme,
    pub json: bool,
}

impl CommandContext {
    fn new(cli: &Cli) -> Self {
        let load = Config::load();
        if let Some(warning) = load.warning {
            eprintln!("warning: {warning}");
        }
        // Colour is suppressed by `--no-color`, by `NO_COLOR`, and by stdout not
        // being a terminal — piping `history` into `grep` should not deliver
        // escape sequences.
        let color = !cli.no_color
            && std::env::var_os("NO_COLOR").is_none()
            && std::io::stdout().is_terminal();
        let theme = load.config.theme(color);
        Self {
            config: load.config,
            theme,
            json: cli.json,
        }
    }
}

pub fn dispatch(cli: Cli) -> Result<()> {
    let context = CommandContext::new(&cli);
    let Some(command) = cli.command else {
        // No subcommand means the interactive app.
        return crate::tui::run(context);
    };

    match command {
        Commands::Templates => runs::templates(),
        Commands::Preview(args) => runs::preview(args, &context),
        Commands::Start(args) => hunt::start_or_hunt(args, false, &context),
        Commands::Hunt(args) => hunt::start_or_hunt(args, true, &context),
        Commands::Resume(args) => hunt::resume(args, &context),
        Commands::List => runs::list(&context),
        Commands::Status(args) => runs::status(args, &context),
        Commands::History(args) => runs::history(args, &context),
        Commands::ShowBest(args) => runs::show_best(args, &context),
        Commands::Export(args) => runs::export(args),
        Commands::Delete(args) => runs::delete(args),
        Commands::Benchmark(args) => bench::benchmark(args, &context),
        Commands::StressTest(args) => bench::stress_test(args, &context),
        Commands::Gpu { command } => match command {
            GpuCommands::Info => {
                gpu::info();
                Ok(())
            }
        },
        Commands::Pi { command } => pi::dispatch(command),
    }
}

/// Pretty JSON on stdout, used by every `--json` path.
pub(crate) fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
