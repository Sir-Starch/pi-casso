//! `gpu info`: what wgpu can see on this machine.

use crate::gpu;

pub fn info() {
    println!("CPU backend: available=true");
    let adapters = gpu::list_adapters();
    println!("GPU backend: available={}", !adapters.is_empty());
    if adapters.is_empty() {
        println!("GPU status: no compatible adapters reported by wgpu");
        return;
    }
    for (index, adapter) in adapters.iter().enumerate() {
        println!(
            "[{index}] {}  type={}  backend={}  driver={} {}",
            adapter.name, adapter.device_type, adapter.backend, adapter.driver, adapter.driver_info
        );
    }
}
