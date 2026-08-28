//! `gpu info`: what wgpu can see on this machine.

use anyhow::Result;

use crate::commands::{CommandContext, print_json};
use crate::gpu;

pub fn info(context: &CommandContext) -> Result<()> {
    let capability = crate::capability::GpuCapability::detect();
    if context.json {
        print_json(&capability)?;
        return match capability.probe_exit_code() {
            0 => Ok(()),
            code => Err(anyhow::anyhow!(crate::commands::CommandExit(code))),
        };
    }
    println!("CPU backend: available=true");
    let adapters = gpu::list_adapters();
    println!("GPU backend: available={}", !adapters.is_empty());
    if adapters.is_empty() {
        println!("GPU status: no compatible adapters reported by wgpu");
        return Ok(());
    }
    for (index, adapter) in adapters.iter().enumerate() {
        println!(
            "[{index}] {}  type={}  backend={}  driver={} {}",
            adapter.name, adapter.device_type, adapter.backend, adapter.driver, adapter.driver_info
        );
    }
    Ok(())
}
