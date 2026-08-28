use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};

use super::{CpuLease, ResourceBudget};

impl ResourceBudget {
    pub(crate) fn acquire_cpu(self: &Arc<Self>, requested: usize) -> Result<(CpuLease, Duration)> {
        let permits = u64::try_from(requested)?;
        if permits == 0 || permits > self.cpu_permits_max {
            bail!(
                "CPU permit request {permits} is outside the global limit {}",
                self.cpu_permits_max
            );
        }
        let started = Instant::now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("resource budget was poisoned"))?;
        while state.cpu_permits_in_use > self.cpu_permits_max - permits {
            state = self
                .ready
                .wait(state)
                .map_err(|_| anyhow!("resource budget was poisoned"))?;
        }
        state.cpu_permits_in_use = state.cpu_permits_in_use.saturating_add(permits);
        state.cpu_permits_peak = state.cpu_permits_peak.max(state.cpu_permits_in_use);
        Ok((
            CpuLease {
                budget: Arc::clone(self),
                permits,
            },
            started.elapsed(),
        ))
    }

    pub(super) fn release_cpu(&self, permits: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.cpu_permits_in_use = state.cpu_permits_in_use.saturating_sub(permits);
            self.ready.notify_all();
        }
    }
}
