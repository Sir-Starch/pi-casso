use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};

use crate::digits::DigitSource;

mod lease;
mod path;

use lease::PooledDigitRead;
use path::PathReader;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ReaderPoolTelemetry {
    pub reader_pool_size: u64,
    pub reader_open_count: u64,
    pub reader_reuse_count: u64,
    pub reader_seek_count: u64,
    pub buffer_growth_count: u64,
    pub cache_hit_count: u64,
    pub reserved_bytes: u64,
    pub cache_hit: Duration,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ReadTelemetry {
    read: Duration,
    parse: Duration,
    source: ReaderPoolTelemetry,
}

pub(super) trait ReusableReader: Send {
    fn read_range(&mut self, offset: u64, len: usize) -> Result<ReadTelemetry>;
    fn digits(&self) -> &[u8];
    fn drain_telemetry(&mut self) -> ReaderPoolTelemetry;
    fn reserved_bytes(&self) -> u64;
}

struct DelegatingReader<'a> {
    source: &'a dyn DigitSource,
    digits: Vec<u8>,
    telemetry: ReaderPoolTelemetry,
}

impl<'a> DelegatingReader<'a> {
    fn new(source: &'a dyn DigitSource, max_read_len: usize) -> Self {
        Self {
            source,
            digits: Vec::with_capacity(max_read_len),
            telemetry: ReaderPoolTelemetry {
                reader_open_count: 1,
                reserved_bytes: u64::try_from(max_read_len).unwrap_or(u64::MAX),
                ..ReaderPoolTelemetry::default()
            },
        }
    }
}

impl ReusableReader for DelegatingReader<'_> {
    fn read_range(&mut self, offset: u64, len: usize) -> Result<ReadTelemetry> {
        let capacity = self.digits.capacity();
        let (read, parse) = self
            .source
            .read_range_into_timed(offset, len, &mut self.digits)?;
        if self.digits.capacity() != capacity {
            self.telemetry.buffer_growth_count =
                self.telemetry.buffer_growth_count.saturating_add(1);
        }
        Ok(ReadTelemetry {
            read,
            parse,
            source: ReaderPoolTelemetry::default(),
        })
    }

    fn digits(&self) -> &[u8] {
        &self.digits
    }

    fn drain_telemetry(&mut self) -> ReaderPoolTelemetry {
        std::mem::take(&mut self.telemetry)
    }

    fn reserved_bytes(&self) -> u64 {
        u64::try_from(self.digits.capacity()).unwrap_or(u64::MAX)
    }
}

pub(super) struct ReaderSlot<'a> {
    pub(super) reader: Box<dyn ReusableReader + 'a>,
    uses: u64,
}

pub(crate) struct DigitReaderPool<'a> {
    #[cfg(test)]
    source: &'a dyn DigitSource,
    state: Arc<PoolState<'a>>,
    max_read_len: usize,
}

pub(super) struct PoolState<'a> {
    available: Mutex<Vec<ReaderSlot<'a>>>,
    ready: Condvar,
    telemetry: Mutex<ReaderPoolTelemetry>,
}

impl<'a> DigitReaderPool<'a> {
    pub(crate) const fn size_for(cpu_workers: usize, queue_depth: usize) -> usize {
        let workers = if cpu_workers == 0 { 1 } else { cpu_workers };
        if workers < queue_depth {
            workers
        } else {
            queue_depth
        }
    }

    pub(crate) fn configured_capacity_bytes(
        source: &dyn DigitSource,
        cpu_workers: usize,
        queue_depth: usize,
        max_read_len: usize,
    ) -> Result<u64> {
        let per_reader = if source.reader_path().is_some() {
            PathReader::configured_capacity_bytes(max_read_len)?
        } else {
            u64::try_from(max_read_len)?
        };
        Self::pool_capacity_bytes(cpu_workers, queue_depth, per_reader)
    }

    pub(crate) fn configured_path_capacity_bytes(
        cpu_workers: usize,
        queue_depth: usize,
        max_read_len: usize,
    ) -> Result<u64> {
        let per_reader = PathReader::configured_capacity_bytes(max_read_len)?;
        Self::pool_capacity_bytes(cpu_workers, queue_depth, per_reader)
    }

    fn pool_capacity_bytes(cpu_workers: usize, queue_depth: usize, per_reader: u64) -> Result<u64> {
        per_reader
            .checked_mul(u64::try_from(Self::size_for(cpu_workers, queue_depth))?)
            .ok_or_else(|| anyhow!("digit reader pool capacity overflowed"))
    }

    pub(crate) fn new(
        source: &'a dyn DigitSource,
        cpu_workers: usize,
        queue_depth: usize,
        max_read_len: usize,
    ) -> Result<Self> {
        if queue_depth == 0 {
            bail!("digit reader queue depth must be non-zero");
        }
        if max_read_len == 0 {
            bail!("digit reader buffer size must be non-zero");
        }
        let pool_size = Self::size_for(cpu_workers, queue_depth);
        let mut slots = Vec::with_capacity(pool_size);
        let mut telemetry = ReaderPoolTelemetry {
            reader_pool_size: u64::try_from(pool_size)?,
            ..ReaderPoolTelemetry::default()
        };
        for _ in 0..pool_size {
            let mut reader: Box<dyn ReusableReader + 'a> = match source.reader_path() {
                Some(spec) => Box::new(PathReader::new(spec, max_read_len)?),
                None => Box::new(DelegatingReader::new(source, max_read_len)),
            };
            add_telemetry(&mut telemetry, reader.drain_telemetry());
            telemetry.reserved_bytes = telemetry
                .reserved_bytes
                .saturating_add(reader.reserved_bytes());
            slots.push(ReaderSlot { reader, uses: 0 });
        }
        Ok(Self {
            #[cfg(test)]
            source,
            state: Arc::new(PoolState {
                available: Mutex::new(slots),
                ready: Condvar::new(),
                telemetry: Mutex::new(telemetry),
            }),
            max_read_len,
        })
    }

    pub(crate) fn read_range_ready(&self, offset: u64, len: usize) -> Result<PooledDigitRead<'a>> {
        if len > self.max_read_len {
            bail!(
                "digit range length {len} exceeds bounded reader capacity {}",
                self.max_read_len
            );
        }
        let mut slot = self.state.checkout()?;
        let reuse = u64::from(slot.uses > 0);
        slot.uses = slot.uses.saturating_add(1);
        let read = match slot.reader.read_range(offset, len) {
            Ok(read) => read,
            Err(error) => {
                let source = slot.reader.drain_telemetry();
                self.state.record(source, reuse);
                self.state.return_slot(slot);
                return Err(error);
            }
        };
        let mut source = read.source;
        add_telemetry(&mut source, slot.reader.drain_telemetry());
        self.state.record(source, reuse);
        Ok(PooledDigitRead::new(
            Arc::clone(&self.state),
            slot,
            read.read,
            read.parse,
            source.cache_hit,
        ))
    }

    #[cfg(test)]
    pub(crate) fn read_range(&self, offset: u64, len: usize) -> Result<PooledDigitRead<'a>> {
        let required = offset
            .checked_add(u64::try_from(len)?)
            .ok_or_else(|| anyhow!("requested digit range overflowed"))?;
        if self.source.is_growing() {
            self.source.request_prefetch(required)?;
        }
        self.read_range_ready(offset, len)
    }

    pub(crate) fn telemetry(&self) -> ReaderPoolTelemetry {
        self.state
            .telemetry
            .lock()
            .map_or_else(|_| ReaderPoolTelemetry::default(), |telemetry| *telemetry)
    }
}

impl<'a> PoolState<'a> {
    fn checkout(&self) -> Result<ReaderSlot<'a>> {
        let mut available = self
            .available
            .lock()
            .map_err(|_| anyhow!("digit reader pool was poisoned"))?;
        loop {
            if let Some(slot) = available.pop() {
                return Ok(slot);
            }
            available = self
                .ready
                .wait(available)
                .map_err(|_| anyhow!("digit reader pool was poisoned"))?;
        }
    }

    fn record(&self, source: ReaderPoolTelemetry, reuse: u64) {
        if let Ok(mut telemetry) = self.telemetry.lock() {
            add_telemetry(&mut telemetry, source);
            telemetry.reader_reuse_count = telemetry.reader_reuse_count.saturating_add(reuse);
        }
    }

    pub(super) fn return_slot(&self, slot: ReaderSlot<'a>) {
        if let Ok(mut available) = self.available.lock() {
            available.push(slot);
            self.ready.notify_one();
        }
    }
}

fn add_telemetry(total: &mut ReaderPoolTelemetry, delta: ReaderPoolTelemetry) {
    total.reader_open_count = total
        .reader_open_count
        .saturating_add(delta.reader_open_count);
    total.reader_seek_count = total
        .reader_seek_count
        .saturating_add(delta.reader_seek_count);
    total.buffer_growth_count = total
        .buffer_growth_count
        .saturating_add(delta.buffer_growth_count);
    total.cache_hit_count = total.cache_hit_count.saturating_add(delta.cache_hit_count);
    total.cache_hit += delta.cache_hit;
}

#[cfg(test)]
mod tests;
