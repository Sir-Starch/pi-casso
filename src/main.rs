mod art;
mod benchmark_build;
mod benchmark_contract;
#[cfg(test)]
mod benchmark_contract_tests;
mod benchmark_execute;
mod benchmark_report;
mod benchmark_runner;
mod benchmark_stats;
mod capability;
mod cli;
mod cli_output;
mod commands;
mod config;
#[cfg(feature = "cuda-native")]
mod cuda;
#[cfg(feature = "cuda-native")]
mod cuda_artifact;
#[cfg(feature = "cuda-native")]
mod cuda_engine;
mod digits;
mod gpu;
mod gpu_ring;
mod performance;
mod pi;
mod pi_benchmark;
mod render;
mod search;
mod storage;
mod tui;

use anyhow::Result;
use clap::Parser;

use crate::cli::Cli;

fn main() {
    restore_default_sigpipe();
    if let Err(err) = real_main() {
        if let Some(exit) = err.downcast_ref::<commands::CommandExit>() {
            std::process::exit(exit.0);
        }
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    commands::dispatch(Cli::parse())
}

/// Rust ignores SIGPIPE, so `pi-casso list | head` used to end in a panic about
/// a broken pipe instead of quietly stopping. Restoring the default handler is
/// what every other command-line tool does.
#[cfg(unix)]
fn restore_default_sigpipe() {
    // SAFETY: installing the OS default disposition for SIGPIPE is exactly what
    // the process starts with before Rust's runtime overrides it, and it is done
    // once, before any thread is spawned.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_default_sigpipe() {}
