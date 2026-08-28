#![allow(dead_code)]

use anyhow::Result;
use std::time::Duration;

use crate::art::Bitmap;

#[cfg(feature = "gpu")]
pub use implementation::*;

#[cfg(not(feature = "gpu"))]
pub use fallback::*;

#[derive(Debug)]
pub struct GpuDeviceInfo {
    pub name: String,
    pub backend: String,
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuEmergenceStatistics {
    pub covered: u32,
    pub total: u32,
    pub leaked: u32,
    pub background_total: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GpuWindowScore {
    pub score: f64,
    pub digit: u8,
    pub x: usize,
    pub y: usize,
    pub coverage: f64,
    pub leakage: f64,
    pub statistics: Option<GpuEmergenceStatistics>,
}

#[derive(Debug, Default)]
pub struct GpuChunkTelemetry {
    pub allocation: Duration,
    pub upload: Duration,
    pub dispatch: Duration,
    pub readback_map: Duration,
    pub submissions: u64,
    pub completions: u64,
    pub buffer_creations: u64,
    pub bind_group_creations: u64,
    pub resource_reuses: u64,
    pub overlap: Duration,
    pub max_in_flight: u64,
    pub overlap_events: u64,
    pub test_only_mock: bool,
    pub fallback_reason: String,
}

impl GpuChunkTelemetry {
    pub fn accounted_duration(&self) -> Duration {
        self.allocation
            .saturating_add(self.upload)
            .saturating_add(self.dispatch)
            .saturating_add(self.readback_map)
    }
}

#[cfg(not(feature = "gpu"))]
mod fallback {
    use super::*;
    use anyhow::bail;
    use std::cell::RefCell;

    thread_local! {
        static CHUNK_TELEMETRY: RefCell<GpuChunkTelemetry> = RefCell::new(GpuChunkTelemetry::default());
    }

    pub struct GpuSearchEngine;

    impl GpuSearchEngine {
        pub fn new(_device_filter: Option<&str>) -> Result<Self> {
            bail!("GPU support was disabled at compile time")
        }

        pub fn new_with_depth(_device_filter: Option<&str>, _ring_depth: usize) -> Result<Self> {
            bail!("GPU support was disabled at compile time")
        }

        pub fn name(&self) -> String {
            "disabled".to_string()
        }

        pub fn device_name(&self) -> &str {
            "disabled"
        }

        pub const fn ring_depth(&self) -> usize {
            0
        }

        pub fn emergence_scores(
            &self,
            _digits: &[u8],
            _actual_windows: usize,
            _target: &Bitmap,
            _canvas_width: usize,
            _canvas_height: usize,
        ) -> Result<Vec<GpuWindowScore>> {
            bail!("GPU support was disabled at compile time")
        }
    }

    pub fn list_adapters() -> Vec<GpuDeviceInfo> {
        Vec::new()
    }

    pub fn take_chunk_telemetry() -> GpuChunkTelemetry {
        CHUNK_TELEMETRY.with(|telemetry| std::mem::take(&mut *telemetry.borrow_mut()))
    }

    pub(crate) fn record_mock_ring(ring: crate::gpu_ring::RingTelemetry) {
        CHUNK_TELEMETRY.with(|telemetry| {
            let mut telemetry = telemetry.borrow_mut();
            telemetry.overlap += ring.overlap;
            telemetry.submissions = telemetry.submissions.saturating_add(ring.submissions);
            telemetry.completions = telemetry.completions.saturating_add(ring.completions);
            telemetry.max_in_flight = telemetry.max_in_flight.max(ring.max_in_flight);
            telemetry.overlap_events = telemetry.overlap_events.saturating_add(ring.overlap_events);
            telemetry.test_only_mock = true;
        });
    }
}

#[cfg(feature = "gpu")]
mod implementation {
    use super::*;
    use anyhow::{Context, anyhow};
    use bytemuck::{Pod, Zeroable};
    use std::cell::RefCell;
    use std::cmp::Ordering;
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::mpsc;
    use std::time::Instant;

    const EMERGENCE_COVERAGE_WEIGHT: f64 = 0.70;
    const EMERGENCE_CONTRAST_WEIGHT: f64 = 0.20;
    const EMERGENCE_CLEANLINESS_WEIGHT: f64 = 0.10;

    thread_local! {
        static CHUNK_TELEMETRY: RefCell<GpuChunkTelemetry> = RefCell::new(GpuChunkTelemetry::default());
    }

    pub fn take_chunk_telemetry() -> GpuChunkTelemetry {
        CHUNK_TELEMETRY.with(|telemetry| std::mem::take(&mut *telemetry.borrow_mut()))
    }

    fn reset_chunk_telemetry() {
        CHUNK_TELEMETRY.with(|telemetry| *telemetry.borrow_mut() = GpuChunkTelemetry::default());
    }

    fn record_gpu_stage(update: impl FnOnce(&mut GpuChunkTelemetry)) {
        CHUNK_TELEMETRY.with(|telemetry| update(&mut telemetry.borrow_mut()));
    }

    pub(crate) fn record_mock_ring(ring: crate::gpu_ring::RingTelemetry) {
        record_gpu_stage(|telemetry| {
            telemetry.overlap += ring.overlap;
            telemetry.submissions = telemetry.submissions.saturating_add(ring.submissions);
            telemetry.completions = telemetry.completions.saturating_add(ring.completions);
            telemetry.max_in_flight = telemetry.max_in_flight.max(ring.max_in_flight);
            telemetry.overlap_events = telemetry.overlap_events.saturating_add(ring.overlap_events);
            telemetry.test_only_mock = true;
        });
    }

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
    digit: u32,
    x: u32,
    y: u32,
    covered: u32,
    total: u32,
    leaked: u32,
    background_total: u32,
    pad0: u32,
}

@group(0) @binding(0)
var<storage, read> digits: array<u32>;
@group(0) @binding(1)
var<storage, read> placement_offsets: array<u32>;
@group(0) @binding(2)
var<storage, read> score_ranks: array<u32>;
@group(0) @binding(3)
var<uniform> params: Params;
@group(0) @binding(4)
var<storage, read_write> scores: array<Score>;

@compute @workgroup_size(128)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let window_index = id.x;
    if (window_index >= params.actual_windows || params.shape_pixels == 0u) {
        return;
    }

    let max_y = params.canvas_height - params.target_height;
    let max_x = params.canvas_width - params.target_width;
    let placement_width = max_x + 1u;
    let placement_stride = params.target_width * params.target_height;
    let rank_stride = params.background_pixels + 1u;
    var best_rank = 0u;
    var best_digit = 0u;
    var best_x = 0u;
    var best_y = 0u;
    var best_covered = 0u;
    var best_leaked = 0u;

    for (var placement = 0u; placement < params.placement_count; placement = placement + 1u) {
        var shape_counts: array<u32, 10>;
        var background_counts: array<u32, 10>;
        for (var digit = 0u; digit < 10u; digit = digit + 1u) {
            shape_counts[digit] = 0u;
            background_counts[digit] = 0u;
        }

        let placement_base = placement * placement_stride;
        for (var offset_index = 0u; offset_index < params.shape_pixels; offset_index = offset_index + 1u) {
            let digit = digits[window_index + placement_offsets[placement_base + offset_index]];
            shape_counts[digit] = shape_counts[digit] + 1u;
        }
        for (var offset_index = 0u; offset_index < params.background_pixels; offset_index = offset_index + 1u) {
            let digit = digits[window_index + placement_offsets[placement_base + params.shape_pixels + offset_index]];
            background_counts[digit] = background_counts[digit] + 1u;
        }

        for (var digit = 0u; digit < 10u; digit = digit + 1u) {
            let matched = shape_counts[digit];
            let leaked = background_counts[digit];
            let rank = score_ranks[matched * rank_stride + leaked];
            if (rank > best_rank) {
                best_rank = rank;
                best_digit = digit;
                best_x = placement % placement_width;
                best_y = placement / placement_width;
                best_covered = matched;
                best_leaked = leaked;
            }
        }
    }

    scores[window_index] = Score(
        best_digit,
        best_x,
        best_y,
        best_covered,
        params.shape_pixels,
        best_leaked,
        params.background_pixels,
        0u,
    );
}
"#;

    fn canonical_emergence_score(coverage: f64, leakage: f64) -> f64 {
        let coverage = coverage.clamp(0.0, 1.0);
        let leakage = leakage.clamp(0.0, 1.0);
        if coverage == 1.0 && leakage == 0.0 {
            return 1.0;
        }
        let coverage_density = coverage * coverage;
        let contrast = if coverage > leakage {
            (coverage - leakage) / (1.0 - leakage).max(f64::EPSILON)
        } else {
            0.0
        };
        let cleanliness = 1.0 - leakage;
        EMERGENCE_COVERAGE_WEIGHT * coverage_density
            + EMERGENCE_CONTRAST_WEIGHT * contrast
            + EMERGENCE_CLEANLINESS_WEIGHT * cleanliness
    }

    fn score_rank_table(shape_pixels: usize, background_pixels: usize) -> Result<Vec<u32>> {
        const MAX_SCORE_RANK_ENTRIES: usize = 16_777_216;

        let rank_stride = background_pixels
            .checked_add(1)
            .context("GPU score rank stride overflowed")?;
        let entry_count = shape_pixels
            .checked_add(1)
            .and_then(|count| count.checked_mul(rank_stride))
            .context("GPU score rank table size overflowed")?;
        if entry_count > MAX_SCORE_RANK_ENTRIES {
            anyhow::bail!(
                "GPU exact score rank table requires {entry_count} entries, above limit {MAX_SCORE_RANK_ENTRIES}"
            );
        }
        let shape = shape_pixels as f64;
        let background = background_pixels as f64;
        let mut entries = Vec::with_capacity(entry_count);
        for covered in 0..=shape_pixels {
            let coverage = covered as f64 / shape;
            for leaked in 0..=background_pixels {
                let leakage = if background_pixels == 0 {
                    0.0
                } else {
                    leaked as f64 / background
                };
                entries.push((
                    covered,
                    leaked,
                    canonical_emergence_score(coverage, leakage),
                    coverage,
                    leakage,
                ));
            }
        }
        entries.sort_by(|left, right| {
            right
                .2
                .partial_cmp(&left.2)
                .unwrap_or(Ordering::Equal)
                .then_with(|| right.3.partial_cmp(&left.3).unwrap_or(Ordering::Equal))
                .then_with(|| left.4.partial_cmp(&right.4).unwrap_or(Ordering::Equal))
        });

        let entry_count_u32 = u32::try_from(entry_count)
            .context("GPU score rank table is too large for a u32 rank")?;
        let mut ranks = vec![0_u32; entry_count];
        for (order, (covered, leaked, _, _, _)) in entries.into_iter().enumerate() {
            let order_u32 =
                u32::try_from(order).context("GPU score rank order is outside the u32 range")?;
            ranks[covered * rank_stride + leaked] = entry_count_u32 - order_u32;
        }
        Ok(ranks)
    }

    fn placement_offsets(
        target: &Bitmap,
        canvas_width: usize,
        canvas_height: usize,
    ) -> Result<Vec<u32>> {
        let max_x = canvas_width
            .checked_sub(target.width)
            .context("GPU target is wider than the canvas")?;
        let max_y = canvas_height
            .checked_sub(target.height)
            .context("GPU target is taller than the canvas")?;
        let placement_count = max_x
            .checked_add(1)
            .and_then(|width| {
                max_y
                    .checked_add(1)
                    .and_then(|height| width.checked_mul(height))
            })
            .context("GPU placement count overflowed")?;
        let target_pixels = target
            .width
            .checked_mul(target.height)
            .context("GPU target pixel count overflowed")?;
        let offset_count = placement_count
            .checked_mul(target_pixels)
            .context("GPU placement offset count overflowed")?;
        let mut offsets = Vec::with_capacity(offset_count);
        for y_offset in 0..=max_y {
            for x_offset in 0..=max_x {
                for shape in [true, false] {
                    for target_y in 0..target.height {
                        for target_x in 0..target.width {
                            let target_index = target_y * target.width + target_x;
                            let is_shape = target.pixels[target_index] == 1;
                            if is_shape == shape {
                                let offset = (y_offset + target_y)
                                    .checked_mul(canvas_width)
                                    .and_then(|row| row.checked_add(x_offset + target_x))
                                    .context("GPU placement offset overflowed")?;
                                offsets
                                    .push(u32::try_from(offset).context(
                                        "GPU placement offset is outside the u32 range",
                                    )?);
                            }
                        }
                    }
                }
            }
        }
        Ok(offsets)
    }

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
        digit: u32,
        x: u32,
        y: u32,
        covered: u32,
        total: u32,
        leaked: u32,
        background_total: u32,
        _pad0: u32,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(crate) struct GpuResourceCounters {
        pub(crate) buffer_creations: u64,
        pub(crate) bind_group_creations: u64,
        pub(crate) resource_reuses: u64,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct GpuResourceCapacity {
        digits: wgpu::BufferAddress,
        placement_offsets: wgpu::BufferAddress,
        score_ranks: wgpu::BufferAddress,
        output: wgpu::BufferAddress,
    }

    impl GpuResourceCapacity {
        pub(crate) fn for_batch(
            digits: usize,
            placement_offsets: usize,
            score_ranks: usize,
            output: usize,
        ) -> Result<Self> {
            let u32_size = std::mem::size_of::<u32>();
            let score_size = std::mem::size_of::<GpuScore>();
            Ok(Self {
                digits: u64::try_from(
                    digits
                        .checked_mul(u32_size)
                        .context("GPU digit upload size overflowed")?,
                )
                .context("GPU digit upload size is outside the buffer address range")?,
                placement_offsets: u64::try_from(
                    placement_offsets
                        .checked_mul(u32_size)
                        .context("GPU placement offset size overflowed")?,
                )
                .context("GPU placement offset size is outside the buffer address range")?,
                score_ranks: u64::try_from(
                    score_ranks
                        .checked_mul(u32_size)
                        .context("GPU score rank size overflowed")?,
                )
                .context("GPU score rank size is outside the buffer address range")?,
                output: u64::try_from(
                    output
                        .checked_mul(score_size)
                        .context("GPU score output size overflowed")?,
                )
                .context("GPU score output size is outside the buffer address range")?,
            })
        }

        const fn supports(self, required: Self) -> bool {
            self.digits >= required.digits
                && self.placement_offsets >= required.placement_offsets
                && self.score_ranks >= required.score_ranks
                && self.output >= required.output
        }

        fn validate(self, maximum: wgpu::BufferAddress) -> Result<()> {
            for (label, size) in [
                ("digit", self.digits),
                ("placement offset", self.placement_offsets),
                ("score rank", self.score_ranks),
                ("output", self.output),
            ] {
                if size == 0 {
                    anyhow::bail!("GPU {label} buffer cannot be empty");
                }
                if size > maximum {
                    anyhow::bail!(
                        "GPU {label} buffer would be {size} bytes, above device limit {maximum} bytes"
                    );
                }
            }
            Ok(())
        }
    }

    pub(crate) struct GpuResourceSlot {
        capacity: GpuResourceCapacity,
        digits: wgpu::Buffer,
        placement_offsets: wgpu::Buffer,
        score_ranks: wgpu::Buffer,
        params: wgpu::Buffer,
        output: wgpu::Buffer,
        staging: wgpu::Buffer,
        bind_group: wgpu::BindGroup,
    }

    type GpuUploadParts<'a> = (&'a GpuResourceSlot, &'a [u32], &'a [u32], &'a [u32]);

    pub(crate) struct GpuResourcePool {
        device: wgpu::Device,
        bind_group_layout: wgpu::BindGroupLayout,
        maximum_buffer_size: wgpu::BufferAddress,
        slots: Vec<Option<GpuResourceSlot>>,
        host_digits: Vec<u32>,
        host_placement_offsets: Vec<u32>,
        host_score_ranks: Vec<u32>,
        counters: GpuResourceCounters,
    }

    impl GpuResourcePool {
        fn new(
            device: &wgpu::Device,
            bind_group_layout: &wgpu::BindGroupLayout,
            maximum_buffer_size: wgpu::BufferAddress,
        ) -> Self {
            Self {
                device: device.clone(),
                bind_group_layout: bind_group_layout.clone(),
                maximum_buffer_size,
                slots: Vec::new(),
                host_digits: Vec::new(),
                host_placement_offsets: Vec::new(),
                host_score_ranks: Vec::new(),
                counters: GpuResourceCounters::default(),
            }
        }

        fn prepare_host_uploads(
            &mut self,
            digits: &[u8],
            placement_offsets: &[u32],
            score_ranks: &[u32],
        ) {
            self.host_digits.clear();
            self.host_digits
                .extend(digits.iter().copied().map(u32::from));
            self.host_placement_offsets.clear();
            self.host_placement_offsets
                .extend_from_slice(placement_offsets);
            self.host_score_ranks.clear();
            self.host_score_ranks.extend_from_slice(score_ranks);
        }

        pub(crate) fn acquire(&mut self, required: GpuResourceCapacity) -> Result<()> {
            self.acquire_at(0, required)
        }

        fn acquire_at(&mut self, index: usize, required: GpuResourceCapacity) -> Result<()> {
            required.validate(self.maximum_buffer_size)?;
            if self.slots.len() <= index {
                self.slots.resize_with(index.saturating_add(1), || None);
            }
            if self
                .slots
                .get(index)
                .and_then(Option::as_ref)
                .is_some_and(|slot| slot.capacity.supports(required))
            {
                self.counters.resource_reuses = self.counters.resource_reuses.saturating_add(1);
                return Ok(());
            }

            let digits = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pi-casso gpu digits"),
                size: required.digits,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let placement_offsets = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pi-casso gpu placement offsets"),
                size: required.placement_offsets,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let score_ranks = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pi-casso gpu score ranks"),
                size: required.score_ranks,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let params = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pi-casso gpu params"),
                size: u64::try_from(std::mem::size_of::<GpuParams>())
                    .context("GPU parameter size is outside the buffer address range")?,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let output = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pi-casso gpu scores"),
                size: required.output,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pi-casso gpu scores staging"),
                size: required.output,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pi-casso gpu search bind group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: digits.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: placement_offsets.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: score_ranks.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: output.as_entire_binding(),
                    },
                ],
            });
            self.slots[index] = Some(GpuResourceSlot {
                capacity: required,
                digits,
                placement_offsets,
                score_ranks,
                params,
                output,
                staging,
                bind_group,
            });
            self.counters.buffer_creations = self.counters.buffer_creations.saturating_add(6);
            self.counters.bind_group_creations =
                self.counters.bind_group_creations.saturating_add(1);
            Ok(())
        }

        fn upload_parts_at(&self, index: usize) -> Result<GpuUploadParts<'_>> {
            let slot = self
                .slots
                .get(index)
                .and_then(Option::as_ref)
                .context("GPU resource slot was not acquired")?;
            Ok((
                slot,
                &self.host_digits,
                &self.host_placement_offsets,
                &self.host_score_ranks,
            ))
        }

        const fn counters(&self) -> GpuResourceCounters {
            self.counters
        }

        fn host_upload_capacities(&self) -> (usize, usize) {
            (
                self.host_digits.capacity(),
                self.host_placement_offsets.capacity(),
            )
        }
    }

    pub struct GpuSearchEngine {
        device: wgpu::Device,
        queue: wgpu::Queue,
        pipeline: wgpu::ComputePipeline,
        bind_group_layout: wgpu::BindGroupLayout,
        max_storage_binding_size: u64,
        max_buffer_size: u64,
        resource_pool: GpuResourcePool,
        ring_depth: usize,
        info: GpuDeviceInfo,
    }

    impl GpuSearchEngine {
        pub fn new(device_filter: Option<&str>) -> Result<Self> {
            Self::new_with_depth(device_filter, 1)
        }

        pub fn new_with_depth(device_filter: Option<&str>, ring_depth: usize) -> Result<Self> {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
            let adapter = select_adapter(&instance, device_filter)?;
            let info = adapter_info(adapter.get_info());
            let limits = adapter.limits();
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
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
            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("pi-casso gpu search bind group layout"),
                    entries: &[
                        storage_entry(0, true),
                        storage_entry(1, true),
                        storage_entry(2, true),
                        uniform_entry(3),
                        storage_entry(4, false),
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
            let resource_pool = GpuResourcePool::new(
                &device,
                &bind_group_layout,
                max_storage_binding_size.min(max_buffer_size),
            );

            Ok(Self {
                device,
                queue,
                pipeline,
                bind_group_layout,
                max_storage_binding_size,
                max_buffer_size,
                resource_pool,
                ring_depth: ring_depth.max(1),
                info,
            })
        }

        pub fn device_name(&self) -> &str {
            &self.info.name
        }

        pub const fn ring_depth(&self) -> usize {
            self.ring_depth
        }

        pub fn emergence_scores(
            &mut self,
            digits: &[u8],
            actual_windows: usize,
            target: &Bitmap,
            canvas_width: usize,
            canvas_height: usize,
        ) -> Result<Vec<GpuWindowScore>> {
            reset_chunk_telemetry();
            if actual_windows == 0 {
                return Ok(Vec::new());
            }
            let shape_pixels = target.pixels.iter().filter(|pixel| **pixel == 1).count();
            if shape_pixels == 0 {
                return Err(anyhow!("GPU emergence target has no shape pixels"));
            }
            let background_pixels = target.pixels.len().saturating_sub(shape_pixels);
            let placement_count =
                (canvas_width - target.width + 1).saturating_mul(canvas_height - target.height + 1);
            let placement_offsets = placement_offsets(target, canvas_width, canvas_height)?;
            let score_ranks = score_rank_table(shape_pixels, background_pixels)?;
            let params = GpuParams {
                canvas_width: canvas_width as u32,
                canvas_height: canvas_height as u32,
                target_width: target.width as u32,
                target_height: target.height as u32,
                actual_windows: actual_windows as u32,
                shape_pixels: shape_pixels as u32,
                background_pixels: background_pixels as u32,
                placement_count: placement_count as u32,
            };
            self.run_batch(digits, &placement_offsets, &score_ranks, params)
                .inspect_err(|error| {
                    record_gpu_stage(|telemetry| {
                        telemetry.fallback_reason = format!("{error:#}");
                    });
                })
        }

        pub(crate) const fn resource_counters(&self) -> GpuResourceCounters {
            self.resource_pool.counters()
        }

        pub(crate) fn host_upload_capacities(&self) -> (usize, usize) {
            self.resource_pool.host_upload_capacities()
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
            &mut self,
            digits: &[u8],
            placement_offsets: &[u32],
            score_ranks: &[u32],
            params: GpuParams,
        ) -> Result<Vec<GpuWindowScore>> {
            struct PendingBatch {
                sequence: usize,
                slot_index: usize,
                output_size: wgpu::BufferAddress,
                readback_started: Instant,
            }

            let total_windows = params.actual_windows as usize;
            let ring_depth = self.ring_depth;
            let window_len =
                (params.canvas_width as usize).saturating_mul(params.canvas_height as usize);
            let hardware_batch_limit = self.max_batch_windows(window_len, 0);
            let preferred_batch = total_windows.div_ceil(ring_depth);
            let batch_limit = hardware_batch_limit.min(preferred_batch.max(1));
            let GpuSearchEngine {
                device,
                queue,
                pipeline,
                resource_pool,
                ..
            } = self;
            let (completion_tx, completion_rx) = mpsc::sync_channel(ring_depth);
            let mut pending = VecDeque::with_capacity(ring_depth);
            let mut completed = BTreeMap::new();
            let mut scores = Vec::with_capacity(total_windows);
            let mut start = 0_usize;
            let mut sequence = 0_usize;
            let mut overlap_started = None;

            while start < total_windows || !pending.is_empty() {
                while start < total_windows && pending.len() < ring_depth {
                    let batch_windows = (total_windows - start).min(batch_limit);
                    let batch_digits_len =
                        batch_windows.saturating_add(window_len.saturating_sub(1));
                    let slot_index = sequence % ring_depth;
                    let batch_params = GpuParams {
                        actual_windows: batch_windows as u32,
                        ..params
                    };
                    let required = GpuResourceCapacity::for_batch(
                        batch_digits_len,
                        placement_offsets.len(),
                        score_ranks.len(),
                        batch_windows,
                    )?;
                    let counters_before = resource_pool.counters();
                    let allocation_started = Instant::now();
                    resource_pool.acquire_at(slot_index, required)?;
                    let counters_after = resource_pool.counters();
                    record_gpu_stage(|telemetry| {
                        telemetry.allocation += allocation_started.elapsed();
                        telemetry.buffer_creations = telemetry.buffer_creations.saturating_add(
                            counters_after
                                .buffer_creations
                                .saturating_sub(counters_before.buffer_creations),
                        );
                        telemetry.bind_group_creations =
                            telemetry.bind_group_creations.saturating_add(
                                counters_after
                                    .bind_group_creations
                                    .saturating_sub(counters_before.bind_group_creations),
                            );
                        telemetry.resource_reuses = telemetry.resource_reuses.saturating_add(
                            counters_after
                                .resource_reuses
                                .saturating_sub(counters_before.resource_reuses),
                        );
                    });

                    let upload_started = Instant::now();
                    resource_pool.prepare_host_uploads(
                        &digits[start..start.saturating_add(batch_digits_len)],
                        placement_offsets,
                        score_ranks,
                    );
                    let (slot, host_digits, host_placement_offsets, host_score_ranks) =
                        resource_pool.upload_parts_at(slot_index)?;
                    queue.write_buffer(&slot.digits, 0, bytemuck::cast_slice(host_digits));
                    queue.write_buffer(
                        &slot.placement_offsets,
                        0,
                        bytemuck::cast_slice(host_placement_offsets),
                    );
                    queue.write_buffer(
                        &slot.score_ranks,
                        0,
                        bytemuck::cast_slice(host_score_ranks),
                    );
                    queue.write_buffer(&slot.params, 0, bytemuck::bytes_of(&batch_params));
                    record_gpu_stage(|telemetry| telemetry.upload += upload_started.elapsed());

                    let dispatch_started = Instant::now();
                    let mut encoder =
                        device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("pi-casso gpu search encoder"),
                        });
                    {
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some("pi-casso gpu emergence pass"),
                            timestamp_writes: None,
                        });
                        pass.set_pipeline(pipeline);
                        pass.set_bind_group(0, &slot.bind_group, &[]);
                        pass.dispatch_workgroups((batch_windows as u32).div_ceil(128), 1, 1);
                    }
                    encoder.copy_buffer_to_buffer(
                        &slot.output,
                        0,
                        &slot.staging,
                        0,
                        required.output,
                    );
                    queue.submit(Some(encoder.finish()));
                    let callback = completion_tx.clone();
                    let callback_sequence = sequence;
                    slot.staging.slice(..required.output).map_async(
                        wgpu::MapMode::Read,
                        move |result| {
                            let _ = callback.send((callback_sequence, result));
                        },
                    );
                    pending.push_back(PendingBatch {
                        sequence,
                        slot_index,
                        output_size: required.output,
                        readback_started: Instant::now(),
                    });
                    if pending.len() == 2 {
                        overlap_started = Some(Instant::now());
                        record_gpu_stage(|telemetry| {
                            telemetry.overlap_events = telemetry.overlap_events.saturating_add(1);
                        });
                    }
                    let in_flight = u64::try_from(pending.len()).unwrap_or(u64::MAX);
                    record_gpu_stage(|telemetry| {
                        telemetry.dispatch += dispatch_started.elapsed();
                        telemetry.submissions = telemetry.submissions.saturating_add(1);
                        telemetry.max_in_flight = telemetry.max_in_flight.max(in_flight);
                    });
                    start = start.saturating_add(batch_windows);
                    sequence = sequence.saturating_add(1);
                }

                let next_sequence = pending
                    .front()
                    .context("GPU ring lost its oldest slot")?
                    .sequence;
                while !completed.contains_key(&next_sequence) {
                    if let Err(error) = device.poll(wgpu::PollType::Poll) {
                        for outstanding in &pending {
                            if let Some(slot) = resource_pool
                                .slots
                                .get(outstanding.slot_index)
                                .and_then(Option::as_ref)
                            {
                                slot.staging.unmap();
                            }
                        }
                        return Err(error).context("failed while polling GPU search completion");
                    }
                    match completion_rx.recv_timeout(Duration::from_millis(1)) {
                        Ok((finished, result)) => {
                            completed.insert(finished, result);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            for outstanding in &pending {
                                if let Some(slot) = resource_pool
                                    .slots
                                    .get(outstanding.slot_index)
                                    .and_then(Option::as_ref)
                                {
                                    slot.staging.unmap();
                                }
                            }
                            anyhow::bail!("GPU result mapping callback channel disconnected")
                        }
                    }
                }
                let mapping = completed
                    .remove(&next_sequence)
                    .context("GPU completion disappeared before reduction")?;
                if let Err(error) = mapping {
                    for outstanding in &pending {
                        if let Some(slot) = resource_pool
                            .slots
                            .get(outstanding.slot_index)
                            .and_then(Option::as_ref)
                        {
                            slot.staging.unmap();
                        }
                    }
                    return Err(error).context("failed to map GPU search result buffer");
                }
                let finished = pending
                    .pop_front()
                    .context("GPU ring completion had no slot")?;
                let slot = resource_pool
                    .slots
                    .get(finished.slot_index)
                    .and_then(Option::as_ref)
                    .context("GPU completed slot was not allocated")?;
                let slice = slot.staging.slice(..finished.output_size);
                let mapped = slice.get_mapped_range();
                let raw_scores: &[GpuScore] = bytemuck::cast_slice(&mapped);
                for score in raw_scores {
                    let coverage = f64::from(score.covered) / f64::from(score.total);
                    let leakage = if score.background_total == 0 {
                        0.0
                    } else {
                        f64::from(score.leaked) / f64::from(score.background_total)
                    };
                    scores.push(GpuWindowScore {
                        score: canonical_emergence_score(coverage, leakage),
                        digit: u8::try_from(score.digit)
                            .context("GPU candidate digit is outside the u8 range")?,
                        x: usize::try_from(score.x)
                            .context("GPU candidate x coordinate is outside the usize range")?,
                        y: usize::try_from(score.y)
                            .context("GPU candidate y coordinate is outside the usize range")?,
                        coverage,
                        leakage,
                        statistics: Some(GpuEmergenceStatistics {
                            covered: score.covered,
                            total: score.total,
                            leaked: score.leaked,
                            background_total: score.background_total,
                        }),
                    });
                }
                drop(mapped);
                slot.staging.unmap();
                record_gpu_stage(|telemetry| {
                    telemetry.readback_map += finished.readback_started.elapsed();
                    telemetry.completions = telemetry.completions.saturating_add(1);
                });
                if pending.len() == 1 {
                    if let Some(overlap) = overlap_started.take() {
                        record_gpu_stage(|telemetry| telemetry.overlap += overlap.elapsed());
                    }
                }
            }
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

    fn select_adapter(
        instance: &wgpu::Instance,
        device_filter: Option<&str>,
    ) -> Result<wgpu::Adapter> {
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn gpu_resource_reuse_rejects_incompatible_capacity() {
            // Given: a real adapter and one warmed resource slot.
            let Ok(mut engine) = GpuSearchEngine::new(None) else {
                eprintln!("SKIP: wgpu adapter/device/pipeline preflight unavailable");
                return;
            };
            let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
            let small_digits = vec![1, 2, 3, 1, 4];
            let expected = engine
                .emergence_scores(&small_digits, 2, &target, 2, 2)
                .unwrap();
            let warmed = engine.resource_counters();
            let warmed_hosts = engine.host_upload_capacities();

            // When: an equal-capacity batch runs, then a larger batch forces growth.
            let repeated = engine
                .emergence_scores(&small_digits, 2, &target, 2, 2)
                .unwrap();
            let reused = engine.resource_counters();
            let reused_hosts = engine.host_upload_capacities();
            let reused_stages = take_chunk_telemetry();
            let larger_digits = vec![1, 2, 3, 1, 4, 1, 5];
            let larger = engine
                .emergence_scores(&larger_digits, 4, &target, 2, 2)
                .unwrap();
            let grown = engine.resource_counters();

            // Then: compatible resources and host buffers are stable, while the
            // incompatible capacity creates exactly one replacement slot.
            assert_eq!(repeated, expected);
            assert_eq!(reused.buffer_creations, warmed.buffer_creations);
            assert_eq!(reused.bind_group_creations, warmed.bind_group_creations);
            assert_eq!(reused.resource_reuses, warmed.resource_reuses + 1);
            assert_eq!(reused_stages.buffer_creations, 0);
            assert_eq!(reused_stages.bind_group_creations, 0);
            assert_eq!(reused_stages.resource_reuses, 1);
            assert_eq!(reused_hosts, warmed_hosts);
            assert_eq!(larger.len(), 4);
            assert_eq!(grown.buffer_creations, reused.buffer_creations + 6);
            assert_eq!(grown.bind_group_creations, reused.bind_group_creations + 1);
            eprintln!(
                "task7_resource_reuse warmed_buffers={} warmed_bind_groups={} reused_buffers={} reused_bind_groups={} resource_reuses={} host_digits_capacity={} host_target_capacity={} grown_buffers={} grown_bind_groups={}",
                warmed.buffer_creations,
                warmed.bind_group_creations,
                reused.buffer_creations,
                reused.bind_group_creations,
                reused.resource_reuses,
                reused_hosts.0,
                reused_hosts.1,
                grown.buffer_creations,
                grown.bind_group_creations,
            );
        }

        #[test]
        fn gpu_ring_depths_match_serial_reference() {
            // Given: one deterministic chunk and real engines at depths one, two, and four.
            let Ok(mut serial) = GpuSearchEngine::new_with_depth(None, 1) else {
                eprintln!("SKIP: wgpu adapter/device/pipeline preflight unavailable");
                return;
            };
            let mut two = GpuSearchEngine::new_with_depth(None, 2).unwrap();
            let mut four = GpuSearchEngine::new_with_depth(None, 4).unwrap();
            let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
            let digits = vec![1, 2, 3, 1, 4, 1, 5];

            // When: every depth executes the same four ordered windows.
            let expected = serial.emergence_scores(&digits, 4, &target, 2, 2).unwrap();
            let at_two = two.emergence_scores(&digits, 4, &target, 2, 2).unwrap();
            let at_four = four.emergence_scores(&digits, 4, &target, 2, 2).unwrap();

            // Then: every score, offset, diagnostic, and tie field is byte-equivalent.
            assert_eq!(at_two, expected);
            assert_eq!(at_four, expected);
            assert_eq!(take_chunk_telemetry().max_in_flight, 4);
        }

        #[test]
        fn exact_score_rank_table_orders_count_pairs() {
            let ranks = score_rank_table(2, 2).expect("small score rank table");
            let stride = 3;

            assert!(ranks[2 * stride] > ranks[stride]);
            assert!(ranks[stride] > ranks[0]);
            assert!(ranks[2 * stride] > ranks[2 * stride + 1]);
            assert_eq!(ranks.len(), 9);
        }
    }
}
