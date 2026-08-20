use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

/// What the producer behind a growing source is doing right now.
///
/// Without this the UI could only say "waiting for more pi", which reads as a
/// stall. A growing cache is almost never stalled — it is computing, and saying
/// so (with a rate) is the difference between "stuck" and "working".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GenerationState {
    /// True while digits are actively being computed.
    pub active: bool,
    /// How many digits the producer has been asked to reach in total.
    pub target_digits: u64,
}

pub trait DigitSource: Send {
    fn kind(&self) -> &'static str;
    fn len(&self) -> Result<u64>;
    fn validate(&self) -> Result<()>;
    fn read_range(&self, offset: u64, len: usize) -> Result<Vec<u8>>;
    fn is_growing(&self) -> bool {
        false
    }

    fn request_prefetch(&self, _min_digits: u64) -> Result<()> {
        Ok(())
    }

    /// `None` for sources that cannot grow, so callers can distinguish
    /// "finite and finished" from "growing and momentarily behind".
    fn generation(&self) -> Option<GenerationState> {
        None
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DigitSourceSpec {
    pub source_type: String,
    pub source_path: Option<String>,
    #[serde(default)]
    pub allow_decimal_prefix: bool,
}

impl DigitSourceSpec {
    pub fn demo() -> Self {
        Self {
            source_type: "demo".to_string(),
            source_path: None,
            allow_decimal_prefix: false,
        }
    }

    pub fn file(path: PathBuf, allow_decimal_prefix: bool) -> Self {
        Self {
            source_type: "file".to_string(),
            source_path: Some(path.to_string_lossy().into_owned()),
            allow_decimal_prefix,
        }
    }

    pub fn cache(path: PathBuf) -> Self {
        Self {
            source_type: "cache".to_string(),
            source_path: Some(path.to_string_lossy().into_owned()),
            allow_decimal_prefix: false,
        }
    }

    pub fn open(&self) -> Result<Box<dyn DigitSource>> {
        match self.source_type.as_str() {
            "demo" => Ok(Box::new(DemoDigitSource)),
            "file" => {
                let path = self
                    .source_path
                    .as_ref()
                    .ok_or_else(|| anyhow!("file digit source is missing a path"))?;
                Ok(Box::new(FileDigitSource::new_with_options(
                    PathBuf::from(path),
                    self.allow_decimal_prefix,
                )))
            }
            "cache" => {
                let path = self
                    .source_path
                    .as_ref()
                    .ok_or_else(|| anyhow!("cache digit source is missing a path"))?;
                Ok(Box::new(crate::pi::CachedGrowingPiSource::new(
                    crate::pi::PiCache::new(PathBuf::from(path)),
                )))
            }
            other => bail!("unsupported digit source type {other:?}"),
        }
    }
}

#[derive(Debug)]
pub struct FileDigitSource {
    path: PathBuf,
    allow_decimal_prefix: bool,
    cursor: Mutex<FileReadCursor>,
    digit_len: Mutex<Option<u64>>,
}

impl FileDigitSource {
    pub fn new_with_options(path: PathBuf, allow_decimal_prefix: bool) -> Self {
        Self {
            path,
            allow_decimal_prefix,
            cursor: Mutex::new(FileReadCursor::default()),
            digit_len: Mutex::new(None),
        }
    }

    pub fn copy_digits_to(&self, destination: &Path) -> Result<u64> {
        let file = File::open(&self.path)
            .with_context(|| format!("failed to open pi digit file {}", self.path.display()))?;
        let mut reader = BufReader::new(file);
        let output = File::create(destination)
            .with_context(|| format!("failed to create {}", destination.display()))?;
        let mut writer = BufWriter::new(output);
        let mut state = ParseState::default();
        let mut byte_offset = 0_u64;
        let mut buf = [0_u8; 64 * 1024];

        loop {
            let read = reader.read(&mut buf)?;
            if read == 0 {
                break;
            }
            for byte in &buf[..read] {
                if let Some(digit) =
                    parse_source_byte(*byte, byte_offset, &mut state, self.allow_decimal_prefix)?
                {
                    writer.write_all(&[b'0' + digit])?;
                }
                byte_offset += 1;
            }
        }

        writer.flush()?;
        *self
            .digit_len
            .lock()
            .map_err(|_| anyhow!("digit source length cache was poisoned"))? =
            Some(state.digits_seen);
        Ok(state.digits_seen)
    }

    pub fn append_digits_from_to(&self, destination: &Path, start_digit: u64) -> Result<u64> {
        let file = File::open(&self.path)
            .with_context(|| format!("failed to open pi digit file {}", self.path.display()))?;
        let mut reader = BufReader::new(file);
        let output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(destination)
            .with_context(|| format!("failed to open {}", destination.display()))?;
        let mut writer = BufWriter::new(output);
        let mut state = ParseState::default();
        let mut byte_offset = 0_u64;
        let mut copied = 0_u64;
        let mut buf = [0_u8; 64 * 1024];

        loop {
            let read = reader.read(&mut buf)?;
            if read == 0 {
                break;
            }
            for byte in &buf[..read] {
                let digit_index = state.digits_seen;
                if let Some(digit) =
                    parse_source_byte(*byte, byte_offset, &mut state, self.allow_decimal_prefix)?
                {
                    if digit_index >= start_digit {
                        writer.write_all(&[b'0' + digit])?;
                        copied += 1;
                    }
                }
                byte_offset += 1;
            }
        }

        writer.flush()?;
        *self
            .digit_len
            .lock()
            .map_err(|_| anyhow!("digit source length cache was poisoned"))? =
            Some(state.digits_seen);
        Ok(copied)
    }

    fn count_digits(&self) -> Result<u64> {
        let file = File::open(&self.path)
            .with_context(|| format!("failed to open pi digit file {}", self.path.display()))?;
        let mut reader = BufReader::new(file);
        let mut state = ParseState::default();
        let mut byte_offset = 0_u64;
        let mut buf = [0_u8; 64 * 1024];

        loop {
            let read = reader.read(&mut buf)?;
            if read == 0 {
                break;
            }
            for byte in &buf[..read] {
                parse_source_byte(*byte, byte_offset, &mut state, self.allow_decimal_prefix)?;
                byte_offset += 1;
            }
        }

        Ok(state.digits_seen)
    }
}

impl DigitSource for FileDigitSource {
    fn kind(&self) -> &'static str {
        "file"
    }

    fn len(&self) -> Result<u64> {
        if let Some(len) = *self
            .digit_len
            .lock()
            .map_err(|_| anyhow!("digit source length cache was poisoned"))?
        {
            return Ok(len);
        }
        let len = self.count_digits()?;
        *self
            .digit_len
            .lock()
            .map_err(|_| anyhow!("digit source length cache was poisoned"))? = Some(len);
        Ok(len)
    }

    fn validate(&self) -> Result<()> {
        let len = self.count_digits()?;
        *self
            .digit_len
            .lock()
            .map_err(|_| anyhow!("digit source length cache was poisoned"))? = Some(len);
        Ok(())
    }

    fn read_range(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let cached_cursor = *self
            .cursor
            .lock()
            .map_err(|_| anyhow!("digit source cursor was poisoned"))?;
        let mut cursor = if offset >= cached_cursor.digit_offset {
            cached_cursor
        } else {
            FileReadCursor::default()
        };

        let mut file = File::open(&self.path)
            .with_context(|| format!("failed to open pi digit file {}", self.path.display()))?;
        file.seek(SeekFrom::Start(cursor.byte_offset))?;
        let mut reader = BufReader::new(file);
        let mut out = Vec::with_capacity(len);
        let mut buf = [0_u8; 64 * 1024];
        let mut request_cursor = None;

        loop {
            let read = reader.read(&mut buf)?;
            if read == 0 {
                break;
            }

            for byte in &buf[..read] {
                let state_before = cursor.state;
                if let Some(digit) = parse_source_byte(
                    *byte,
                    cursor.byte_offset,
                    &mut cursor.state,
                    self.allow_decimal_prefix,
                )? {
                    let digit_offset = state_before.digits_seen;
                    if digit_offset >= offset {
                        if request_cursor.is_none() {
                            request_cursor = Some(FileReadCursor {
                                digit_offset,
                                byte_offset: cursor.byte_offset,
                                state: state_before,
                            });
                        }
                        out.push(digit);
                        if out.len() == len {
                            if let Some(request_cursor) = request_cursor {
                                *self
                                    .cursor
                                    .lock()
                                    .map_err(|_| anyhow!("digit source cursor was poisoned"))? =
                                    request_cursor;
                            }
                            return Ok(out);
                        }
                    }
                }
                cursor.byte_offset += 1;
            }
        }

        if let Some(request_cursor) = request_cursor {
            *self
                .cursor
                .lock()
                .map_err(|_| anyhow!("digit source cursor was poisoned"))? = request_cursor;
        }
        Ok(out)
    }
}

#[derive(Clone, Debug)]
pub struct DemoDigitSource;

impl DigitSource for DemoDigitSource {
    fn kind(&self) -> &'static str {
        "demo"
    }

    fn len(&self) -> Result<u64> {
        Ok(DEMO_PI_DIGITS.len() as u64)
    }

    fn validate(&self) -> Result<()> {
        Ok(())
    }

    fn read_range(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let start = offset as usize;
        if start >= DEMO_PI_DIGITS.len() {
            return Ok(Vec::new());
        }
        let end = (start + len).min(DEMO_PI_DIGITS.len());
        convert_ascii_digits(&DEMO_PI_DIGITS.as_bytes()[start..end])
    }
}

pub fn convert_ascii_digits(bytes: &[u8]) -> Result<Vec<u8>> {
    bytes
        .iter()
        .map(|byte| {
            if byte.is_ascii_digit() {
                Ok(byte - b'0')
            } else {
                Err(anyhow!(
                    "digit source contained non-digit byte 0x{byte:02x}"
                ))
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default)]
struct ParseState {
    digits_seen: u64,
    decimal_prefix_seen: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct FileReadCursor {
    digit_offset: u64,
    byte_offset: u64,
    state: ParseState,
}

fn parse_source_byte(
    byte: u8,
    byte_offset: u64,
    state: &mut ParseState,
    allow_decimal_prefix: bool,
) -> Result<Option<u8>> {
    if byte.is_ascii_digit() {
        state.digits_seen += 1;
        return Ok(Some(byte - b'0'));
    }

    if matches!(byte, b' ' | b'\n' | b'\r' | b'\t') {
        return Ok(None);
    }

    if byte == b'.' && allow_decimal_prefix && state.digits_seen == 1 && !state.decimal_prefix_seen
    {
        state.decimal_prefix_seen = true;
        return Ok(None);
    }

    Err(invalid_source_byte(byte, byte_offset, allow_decimal_prefix))
}

fn invalid_source_byte(byte: u8, byte_offset: u64, allow_decimal_prefix: bool) -> anyhow::Error {
    let display = if byte.is_ascii_graphic() {
        format!(" ('{}')", char::from(byte))
    } else {
        String::new()
    };

    if byte == b'.' && !allow_decimal_prefix {
        anyhow!(
            "pi digit file contains invalid byte 0x{byte:02x}{display} at byte offset \
             {byte_offset}; decimal points are rejected by default (use \
             --allow-decimal-prefix only for files that start with a 3. prefix)"
        )
    } else {
        anyhow!(
            "pi digit file contains invalid byte 0x{byte:02x}{display} at byte offset \
             {byte_offset}; only digits and ASCII whitespace (space, newline, carriage \
             return, tab) are allowed"
        )
    }
}

const DEMO_PI_DIGITS: &str = concat!(
    "314159265358979323846264338327950288419716939937510",
    "58209749445923078164062862089986280348253421170679",
    "82148086513282306647093844609550582231725359408128",
    "48111745028410270193852110555964462294895493038196",
    "44288109756659334461284756482337867831652712019091",
    "45648566923460348610454326648213393607260249141273",
    "72458700660631558817488152092096282925409171536436",
    "78925903600113305305488204665213841469519415116094"
);

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn converts_ascii_digits() {
        assert_eq!(convert_ascii_digits(b"314").unwrap(), vec![3, 1, 4]);
    }

    #[test]
    fn file_source_validates_and_reads_without_loading_all() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "314159").unwrap();
        let source = FileDigitSource::new_with_options(file.path().to_path_buf(), false);
        source.validate().unwrap();
        assert_eq!(source.len().unwrap(), 6);
        assert_eq!(source.read_range(2, 3).unwrap(), vec![4, 1, 5]);
    }

    #[test]
    fn file_source_accepts_trailing_newline() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "314159").unwrap();
        let source = FileDigitSource::new_with_options(file.path().to_path_buf(), false);
        source.validate().unwrap();
        assert_eq!(source.len().unwrap(), 6);
        assert_eq!(source.read_range(0, 6).unwrap(), vec![3, 1, 4, 1, 5, 9]);
    }

    #[test]
    fn file_source_accepts_multi_line_pi_digits() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "314\n159\r\n265").unwrap();
        let source = FileDigitSource::new_with_options(file.path().to_path_buf(), false);
        source.validate().unwrap();
        assert_eq!(
            source.read_range(0, 9).unwrap(),
            vec![3, 1, 4, 1, 5, 9, 2, 6, 5]
        );
    }

    #[test]
    fn file_source_accepts_spaces_and_tabs_between_digit_groups() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "314 159\t265").unwrap();
        let source = FileDigitSource::new_with_options(file.path().to_path_buf(), false);
        source.validate().unwrap();
        assert_eq!(source.read_range(3, 6).unwrap(), vec![1, 5, 9, 2, 6, 5]);
    }

    #[test]
    fn file_source_rejects_decimal_point_by_default() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "3.14").unwrap();
        let source = FileDigitSource::new_with_options(file.path().to_path_buf(), false);
        assert!(source.validate().is_err());
    }

    #[test]
    fn file_source_allows_decimal_prefix_when_explicit() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "3.1415").unwrap();
        let source = FileDigitSource::new_with_options(file.path().to_path_buf(), true);
        source.validate().unwrap();
        assert_eq!(source.len().unwrap(), 5);
        assert_eq!(source.read_range(0, 5).unwrap(), vec![3, 1, 4, 1, 5]);
    }

    #[test]
    fn file_source_reports_clear_byte_offset_for_invalid_non_whitespace() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "314x159").unwrap();
        let source = FileDigitSource::new_with_options(file.path().to_path_buf(), false);
        let err = source.validate().unwrap_err().to_string();
        assert!(err.contains("invalid byte 0x78 ('x')"));
        assert!(err.contains("byte offset 3"));
    }
}
