use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};

use super::{ReadTelemetry, ReaderPoolTelemetry, ReusableReader};
use crate::digits::{ParseState, ReaderPath, parse_source_byte};

const BYTE_BUFFER_CAPACITY: usize = 64 * 1024;

pub(super) struct PathReader<'a> {
    path: &'a Path,
    allow_decimal_prefix: bool,
    missing_is_empty: bool,
    file: Option<File>,
    bytes: Vec<u8>,
    byte_len: usize,
    byte_position: usize,
    byte_offset: u64,
    state: ParseState,
    digits: Vec<u8>,
    cache_start: u64,
    view_start: usize,
    view_len: usize,
    telemetry: ReaderPoolTelemetry,
}

impl<'a> PathReader<'a> {
    pub(super) fn configured_capacity_bytes(max_read_len: usize) -> Result<u64> {
        let byte_capacity = BYTE_BUFFER_CAPACITY.min(max_read_len.max(1));
        u64::try_from(byte_capacity)?
            .checked_add(u64::try_from(max_read_len)?)
            .ok_or_else(|| anyhow!("digit reader capacity overflowed"))
    }

    pub(super) fn new(spec: ReaderPath<'a>, max_read_len: usize) -> Result<Self> {
        let (file, opened) = open_file(spec.path, spec.missing_is_empty)?;
        let byte_capacity = BYTE_BUFFER_CAPACITY.min(max_read_len.max(1));
        Ok(Self {
            path: spec.path,
            allow_decimal_prefix: spec.allow_decimal_prefix,
            missing_is_empty: spec.missing_is_empty,
            file,
            bytes: vec![0; byte_capacity],
            byte_len: 0,
            byte_position: 0,
            byte_offset: 0,
            state: ParseState::default(),
            digits: Vec::with_capacity(max_read_len),
            cache_start: 0,
            view_start: 0,
            view_len: 0,
            telemetry: ReaderPoolTelemetry {
                reader_open_count: opened,
                ..ReaderPoolTelemetry::default()
            },
        })
    }

    fn prepare_cache(
        &mut self,
        offset: u64,
        len: usize,
        telemetry: &mut ReadTelemetry,
    ) -> Result<(usize, bool)> {
        let requested_end = offset
            .checked_add(u64::try_from(len)?)
            .ok_or_else(|| anyhow!("requested digit range overflowed"))?;
        let cache_end = self
            .cache_start
            .saturating_add(u64::try_from(self.digits.len())?);
        if offset >= self.cache_start && offset < cache_end {
            let overlap_started = Instant::now();
            let start = usize::try_from(offset - self.cache_start)?;
            let available = self.digits.len() - start;
            let overlap = available.min(len);
            self.telemetry.cache_hit_count = self.telemetry.cache_hit_count.saturating_add(1);
            if requested_end <= cache_end {
                self.view_start = start;
                self.view_len = len;
                self.telemetry.cache_hit += overlap_started.elapsed();
                return Ok((len, true));
            }
            self.digits.copy_within(start.., 0);
            self.digits.truncate(available);
            self.cache_start = offset;
            self.view_start = 0;
            self.view_len = overlap;
            self.telemetry.cache_hit += overlap_started.elapsed();
            return Ok((overlap, false));
        }
        if offset != self.state.digits_seen {
            self.reset()?;
            while self.state.digits_seen < offset {
                if self.next_digit(telemetry)?.is_none() {
                    break;
                }
            }
        }
        self.digits.clear();
        self.cache_start = offset;
        self.view_start = 0;
        self.view_len = 0;
        Ok((0, false))
    }

    fn reset(&mut self) -> Result<()> {
        if let Some(file) = self.file.as_mut() {
            if file.seek(SeekFrom::Start(0)).is_err() {
                self.file = None;
                self.reopen_at(0)?;
            }
            self.telemetry.reader_seek_count = self.telemetry.reader_seek_count.saturating_add(1);
        } else {
            self.reopen_at(0)?;
        }
        self.byte_len = 0;
        self.byte_position = 0;
        self.byte_offset = 0;
        self.state = ParseState::default();
        self.digits.clear();
        self.cache_start = 0;
        Ok(())
    }

    fn reopen_at(&mut self, offset: u64) -> Result<bool> {
        let (mut file, opened) = open_file(self.path, self.missing_is_empty)?;
        if let Some(open) = file.as_mut() {
            open.seek(SeekFrom::Start(offset))
                .with_context(|| format!("failed to seek digit source {}", self.path.display()))?;
        }
        self.file = file;
        self.telemetry.reader_open_count = self.telemetry.reader_open_count.saturating_add(opened);
        Ok(self.file.is_some())
    }

    fn refill(&mut self, read: &mut ReadTelemetry) -> Result<usize> {
        if self.file.is_none() && !self.reopen_at(self.byte_offset)? {
            return Ok(0);
        }
        let started = Instant::now();
        let result = self
            .file
            .as_mut()
            .ok_or_else(|| anyhow!("digit source reader was unavailable"))?
            .read(&mut self.bytes);
        read.read += started.elapsed();
        let count = match result {
            Ok(count) => count,
            Err(_) => {
                self.file = None;
                if !self.reopen_at(self.byte_offset)? {
                    return Ok(0);
                }
                let retry_started = Instant::now();
                let retry = self
                    .file
                    .as_mut()
                    .ok_or_else(|| anyhow!("digit source reader was unavailable"))?
                    .read(&mut self.bytes)
                    .with_context(|| {
                        format!("failed to read digit source {}", self.path.display())
                    });
                read.read += retry_started.elapsed();
                retry?
            }
        };
        self.byte_len = count;
        self.byte_position = 0;
        Ok(count)
    }

    fn next_digit(&mut self, telemetry: &mut ReadTelemetry) -> Result<Option<u8>> {
        loop {
            if self.byte_position == self.byte_len && self.refill(telemetry)? == 0 {
                return Ok(None);
            }
            let byte = self.bytes[self.byte_position];
            self.byte_position += 1;
            let parse_started = Instant::now();
            let digit = if self.missing_is_empty {
                if !byte.is_ascii_digit() {
                    return Err(anyhow!(
                        "pi cache contains invalid byte 0x{byte:02x} at byte offset {}",
                        self.byte_offset
                    ));
                }
                self.state.digits_seen = self.state.digits_seen.saturating_add(1);
                Some(byte - b'0')
            } else {
                parse_source_byte(
                    byte,
                    self.byte_offset,
                    &mut self.state,
                    self.allow_decimal_prefix,
                )?
            };
            telemetry.parse += parse_started.elapsed();
            self.byte_offset = self.byte_offset.saturating_add(1);
            if digit.is_some() {
                return Ok(digit);
            }
        }
    }
}

impl ReusableReader for PathReader<'_> {
    fn read_range(&mut self, offset: u64, len: usize) -> Result<ReadTelemetry> {
        let digit_capacity = self.digits.capacity();
        let mut telemetry = ReadTelemetry::default();
        let (mut populated, complete) = self.prepare_cache(offset, len, &mut telemetry)?;
        if !complete {
            while populated < len {
                match self.next_digit(&mut telemetry)? {
                    Some(digit) => {
                        self.digits.push(digit);
                        populated += 1;
                    }
                    None => break,
                }
            }
            self.view_start = 0;
            self.view_len = populated;
        }
        if self.digits.capacity() != digit_capacity {
            self.telemetry.buffer_growth_count =
                self.telemetry.buffer_growth_count.saturating_add(1);
        }
        Ok(telemetry)
    }

    fn digits(&self) -> &[u8] {
        &self.digits[self.view_start..self.view_start + self.view_len]
    }

    fn drain_telemetry(&mut self) -> ReaderPoolTelemetry {
        std::mem::take(&mut self.telemetry)
    }

    fn reserved_bytes(&self) -> u64 {
        let capacity = self.bytes.capacity().saturating_add(self.digits.capacity());
        u64::try_from(capacity).unwrap_or(u64::MAX)
    }
}

fn open_file(path: &Path, missing_is_empty: bool) -> Result<(Option<File>, u64)> {
    if super::super::resource_budget::test_mode_enabled()
        && std::env::var("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN").is_ok_and(|value| value == "1")
    {
        bail!("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN reached the source-open boundary");
    }
    match File::open(path) {
        Ok(file) => Ok((Some(file), 1)),
        Err(error) if missing_is_empty && error.kind() == std::io::ErrorKind::NotFound => {
            Ok((None, 0))
        }
        Err(error) => Err(error).with_context(|| format!("failed to open {}", path.display())),
    }
}
