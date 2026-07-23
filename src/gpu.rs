use std::sync::mpsc;

use anyhow::{Context, Result, anyhow};
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::art::Bitmap;

const SCORE_SCALE: f64 = 1_000_000.0;

const EMERGENCE_SHADER: &str = r#"
struct Params {
    canvas_width: u32,
    canvas_height: u32,
    target_width: u32,
    target_height: u32,
    actual_windows: u32,
    shape_pixels: u32,
    background_pixels: u32,
    placement_count: u32,
}

struct Score {
    score: u32,
    digit: u32,
    x: u32,
    y: u32,
    coverage: u32,
    leakage: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0)
var<storage, read> digits: array<u32>;
@group(0) @binding(1)
var<storage, read> target_mask: array<u32>;
@group(0) @binding(2)
var<uniform> params: Params;
@group(0) @binding(3)
var<storage, read_write> scores: array<Score>;

@compute @workgroup_size(128)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let window_index = id.x;
    if (window_index >= params.actual_windows || params.shape_pixels == 0u) {
        return;
    }

    let max_y = params.canvas_height - params.target_height;
    let max_x = params.canvas_width - params.target_width;
    var best_score = 0u;
    var best_digit = 0u;
    var best_x = 0u;
    var best_y = 0u;
    var best_coverage = 0u;
    var best_leakage = 1000000u;

    for (var y_offset = 0u; y_offset <= max_y; y_offset = y_offset + 1u) {
        for (var x_offset = 0u; x_offset <= max_x; x_offset = x_offset + 1u) {
            var shape_counts: array<u32, 10>;
            var background_counts: array<u32, 10>;
            for (var digit = 0u; digit < 10u; digit = digit + 1u) {
                shape_counts[digit] = 0u;
                background_counts[digit] = 0u;
            }

            for (var target_y = 0u; target_y < params.target_height; target_y = target_y + 1u) {
                for (var target_x = 0u; target_x < params.target_width; target_x = target_x + 1u) {
                    let target_index = target_y * params.target_width + target_x;
                    let canvas_index = window_index + (y_offset + target_y) * params.canvas_width + x_offset + target_x;
                    let digit = digits[canvas_index];
                    if (target_mask[target_index] == 1u) {
                        shape_counts[digit] = shape_counts[digit] + 1u;
                    } else {
                        background_counts[digit] = background_counts[digit] + 1u;
                    }
                }
            }

            for (var digit = 0u; digit < 10u; digit = digit + 1u) {
                let matched = shape_counts[digit];
                let leaked = background_counts[digit];
                let coverage = matched * 1000000u / params.shape_pixels;
                var leakage = 0u;
                if (params.background_pixels > 0u) {
                    leakage = leaked * 1000000u / params.background_pixels;
                }
                let coverage_f = f32(coverage) / 1000000.0;
                let leakage_f = f32(leakage) / 1000000.0;
                let coverage_density = coverage_f * coverage_f;
                var contrast = 0.0;
                if (coverage_f > leakage_f) {
                    contrast = (coverage_f - leakage_f) / max(1.0 - leakage_f, 0.000001);
                }
                let cleanliness = 1.0 - leakage_f;
                let score = u32((0.70 * coverage_density + 0.20 * contrast + 0.10 * cleanliness) * 1000000.0);
                if (
                    score > best_score ||
                    (score == best_score && coverage > best_coverage) ||
                    (score == best_score && coverage == best_coverage && leakage < best_leakage)
                ) {
                    best_score = score;
                    best_digit = digit;
                    best_x = x_offset;
                    best_y = y_offset;
                    best_coverage = coverage;
                    best_leakage = leakage;
                }
            }
        }
    }

    scores[window_index] = Score(
        best_score,
        best_digit,
        best_x,
        best_y,
        best_coverage,
        best_leakage,
        0u,
        0u,
    );
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuParams {
    canvas_width: u32,
    canvas_height: u32,
    target_width: u32,
    target_height: u32,
    actual_windows: u32,
    shape_pixels: u32,
    background_pixels: u32,
    placement_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuScore {
    score: u32,
    digit: u32,
    x: u32,
    y: u32,
    coverage: u32,
    leakage: u32,
    _pad0: u32,
    _pad1: u32,
}

#[derive(Clone, Debug)]
pub struct GpuDeviceInfo {
    pub name: String,
    pub backend: String,
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
}

#[derive(Clone, Debug)]
pub struct GpuWindowScore {
    pub score: f64,
    pub digit: u8,
    pub x: usize,
    pub y: usize,
    pub coverage: f64,
    pub leakage: f64,
}

pub struct GpuSearchEngine {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    max_storage_binding_size: u64,
    max_buffer_size: u64,
    info: GpuDeviceInfo,
}

impl GpuSearchEngine {
    pub fn new(device_filter: Option<&str>) -> Result<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = select_adapter(&instance, device_filter)?;
        let info = adapter_info(adapter.get_info());
        let limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("pi-casso gpu search device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(limits),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .with_context(|| format!("failed to request GPU device {}", info.name))?;
        let device_limits = device.limits();
        let max_storage_binding_size = device_limits.max_storage_buffer_binding_size as u64;
        let max_buffer_size = device_limits.max_buffer_size;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pi-casso emergence scorer"),
            source: wgpu::ShaderSource::Wgsl(EMERGENCE_SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pi-casso gpu search bind group layout"),
            entries: &[
                storage_entry(0, true),
                storage_entry(1, true),
                uniform_entry(2),
                storage_entry(3, false),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pi-casso gpu search pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pi-casso gpu emergence pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        Ok(Self {
            device,
            queue,
            pipeline,
            bind_group_layout,
            max_storage_binding_size,
            max_buffer_size,
            info,
        })
    }

    pub fn device_name(&self) -> &str {
        &self.info.name
    }

    pub fn emergence_scores(
        &self,
        digits: &[u8],
        actual_windows: usize,
        target: &Bitmap,
        canvas_width: usize,
        canvas_height: usize,
    ) -> Result<Vec<GpuWindowScore>> {
        if actual_windows == 0 {
            return Ok(Vec::new());
        }
        let shape_pixels = target.pixels.iter().filter(|pixel| **pixel == 1).count();
        let background_pixels = target.pixels.len().saturating_sub(shape_pixels);
        let target_u32: Vec<u32> = target.pixels.iter().map(|pixel| *pixel as u32).collect();
        let placement_count =
            (canvas_width - target.width + 1).saturating_mul(canvas_height - target.height + 1);
        let max_batch_windows =
            self.max_batch_windows(canvas_width * canvas_height, placement_count);
        let mut out = Vec::with_capacity(actual_windows);
        let mut start = 0usize;
        while start < actual_windows {
            let batch_windows = (actual_windows - start).min(max_batch_windows);
            let batch_digits_len = batch_windows + canvas_width * canvas_height - 1;
            let batch_digits: Vec<u32> = digits[start..start + batch_digits_len]
                .iter()
                .map(|digit| *digit as u32)
                .collect();
            let params = GpuParams {
                canvas_width: canvas_width as u32,
                canvas_height: canvas_height as u32,
                target_width: target.width as u32,
                target_height: target.height as u32,
                actual_windows: batch_windows as u32,
                shape_pixels: shape_pixels as u32,
                background_pixels: background_pixels as u32,
                placement_count: placement_count as u32,
            };
            out.extend(self.run_batch(&batch_digits, &target_u32, params)?);
            start += batch_windows;
        }
        Ok(out)
    }

    fn max_batch_windows(&self, window_len: usize, _placement_count: usize) -> usize {
        let score_size = std::mem::size_of::<GpuScore>() as u64;
        // Keep margin for validation/alignment differences and for adapters whose
        // max_buffer_size is lower than max_storage_buffer_binding_size.
        let score_buffer_limit = self
            .max_storage_binding_size
            .min(self.max_buffer_size)
            .saturating_mul(3)
            / 4;
        let max_scores = (score_buffer_limit / score_size).max(1).max(1) as usize;
        let digit_buffer_limit = self
            .max_storage_binding_size
            .min(self.max_buffer_size)
            .saturating_mul(3)
            / 4;
        let max_digits = (digit_buffer_limit / std::mem::size_of::<u32>() as u64)
            .saturating_sub(window_len as u64)
            .max(1) as usize;
        max_scores.min(max_digits).min(u32::MAX as usize)
    }

    fn run_batch(
        &self,
        digits: &[u32],
        target: &[u32],
        params: GpuParams,
    ) -> Result<Vec<GpuWindowScore>> {
        let output_len = params.actual_windows as usize;
        let output_size = (output_len * std::mem::size_of::<GpuScore>()) as wgpu::BufferAddress;
        let max_output_size = self.max_storage_binding_size.min(self.max_buffer_size);
        if output_size > max_output_size {
            anyhow::bail!(
                "GPU output buffer would be {} bytes, above device limit {} bytes",
                output_size,
                max_output_size
            );
        }
        let digits_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("pi-casso gpu digits"),
                contents: bytemuck::cast_slice(digits),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let target_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("pi-casso gpu target"),
                contents: bytemuck::cast_slice(target),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let params_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("pi-casso gpu params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pi-casso gpu scores"),
            size: output_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pi-casso gpu scores staging"),
            size: output_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pi-casso gpu search bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: digits_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: target_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: output_buffer.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("pi-casso gpu search encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("pi-casso gpu emergence pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups((output_len as u32).div_ceil(128), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output_buffer, 0, &staging_buffer, 0, output_size);
        self.queue.submit(Some(encoder.finish()));

        let slice = staging_buffer.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::Wait)
            .context("failed while waiting for GPU search results")?;
        rx.recv()
            .context("GPU result mapping callback did not run")?
            .context("failed to map GPU search result buffer")?;
        let mapped = slice.get_mapped_range();
        let raw_scores: &[GpuScore] = bytemuck::cast_slice(&mapped);
        let scores = raw_scores
            .iter()
            .map(|score| GpuWindowScore {
                score: score.score as f64 / SCORE_SCALE,
                digit: score.digit as u8,
                x: score.x as usize,
                y: score.y as usize,
                coverage: score.coverage as f64 / SCORE_SCALE,
                leakage: score.leakage as f64 / SCORE_SCALE,
            })
            .collect();
        drop(mapped);
        staging_buffer.unmap();
        Ok(scores)
    }
}

pub fn list_adapters() -> Vec<GpuDeviceInfo> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    instance
        .enumerate_adapters(wgpu::Backends::all())
        .into_iter()
        .map(|adapter| adapter_info(adapter.get_info()))
        .collect()
}

fn select_adapter(instance: &wgpu::Instance, device_filter: Option<&str>) -> Result<wgpu::Adapter> {
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());
    if let Some(filter) = device_filter.filter(|value| !value.trim().is_empty()) {
        if let Ok(index) = filter.parse::<usize>() {
            return adapters
                .into_iter()
                .nth(index)
                .ok_or_else(|| anyhow!("GPU device index {index} was not found"));
        }
        let filter = filter.to_lowercase();
        return adapters
            .into_iter()
            .find(|adapter| adapter.get_info().name.to_lowercase().contains(&filter))
            .ok_or_else(|| anyhow!("GPU device matching {filter:?} was not found"));
    }

    adapters
        .into_iter()
        .find(|adapter| {
            matches!(
                adapter.get_info().device_type,
                wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
            )
        })
        .or_else(|| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            }))
            .ok()
        })
        .ok_or_else(|| anyhow!("no compatible GPU adapter found"))
}

fn adapter_info(info: wgpu::AdapterInfo) -> GpuDeviceInfo {
    GpuDeviceInfo {
        name: info.name,
        backend: format!("{:?}", info.backend),
        device_type: format!("{:?}", info.device_type),
        driver: info.driver,
        driver_info: info.driver_info,
    }
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
