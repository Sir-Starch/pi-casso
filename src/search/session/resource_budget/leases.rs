use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};

use crate::search::types::WindowScore;

use super::ResourceBudget;

#[derive(Debug)]
pub(crate) struct ChunkLease {
    pub(super) budget: Arc<ResourceBudget>,
    pub(super) bytes: u64,
}

impl Drop for ChunkLease {
    fn drop(&mut self) {
        self.budget.release_chunk(self.bytes);
    }
}

pub(crate) struct CpuLease {
    pub(super) budget: Arc<ResourceBudget>,
    pub(super) permits: u64,
}

impl Drop for CpuLease {
    fn drop(&mut self) {
        self.budget.release_cpu(self.permits);
    }
}

pub(crate) struct GpuLease {
    pub(super) budget: Arc<ResourceBudget>,
}

impl Drop for GpuLease {
    fn drop(&mut self) {
        self.budget.release_gpu();
    }
}

pub(crate) struct GeneratorLease {
    pub(super) budget: Arc<ResourceBudget>,
    pub(super) reserved_bytes: u64,
    pub(super) cpu_permits: u64,
}

impl Drop for GeneratorLease {
    fn drop(&mut self) {
        self.budget
            .release_generator(self.reserved_bytes, self.cpu_permits);
    }
}

pub(crate) fn chunk_reservation_bytes(windows: usize, window_len: usize) -> Result<u64> {
    let read_len = windows
        .checked_add(window_len.saturating_sub(1))
        .ok_or_else(|| anyhow!("chunk source reservation overflowed"))?;
    let source_bytes = u64::try_from(read_len)?
        .checked_mul(2)
        .ok_or_else(|| anyhow!("chunk source reservation exceeded the addressable byte range"))?;
    let score_bytes = u64::try_from(windows)?
        .checked_mul(u64::try_from(std::mem::size_of::<WindowScore>())?)
        .ok_or_else(|| anyhow!("chunk score reservation exceeded the addressable byte range"))?;
    let accelerator_staging_bytes = u64::try_from(windows)?
        .checked_mul(8 * u64::try_from(std::mem::size_of::<u32>())?)
        .ok_or_else(|| {
            anyhow!("accelerator staging reservation exceeded the addressable byte range")
        })?;
    Ok(65_536_u64
        .saturating_add(source_bytes)
        .saturating_add(score_bytes)
        .saturating_add(accelerator_staging_bytes)
        .saturating_add(u64::try_from(window_len)?.saturating_mul(2)))
}

pub(crate) fn minimum_reservation_bytes(window_len: usize) -> Result<u64> {
    let production = chunk_reservation_bytes(1, window_len)?.max(65_536);
    if test_mode_enabled() {
        if let Some(value) = std::env::var("PI_CASSO_TEST_MIN_RESERVATION_BYTES")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
        {
            return Ok(production.max(value));
        }
    }
    Ok(production)
}

pub(crate) fn test_mode_enabled() -> bool {
    crate::gpu_ring::test_mode_enabled()
}

pub(crate) fn test_consumer_delay() -> Duration {
    if !test_mode_enabled() {
        return Duration::ZERO;
    }
    std::env::var("PI_CASSO_TEST_CONSUMER_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(Duration::ZERO, Duration::from_millis)
}
