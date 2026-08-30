use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use super::paths::PublicationPaths;

const LIVE_LOCK_WAIT: Duration = Duration::from_secs(5);
const LOCK_RETRY: Duration = Duration::from_millis(10);
static LOCAL_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
#[cfg(not(windows))]
static LOCK_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
thread_local! {
    static HELD_PATHS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    static CRASH_TRIGGERED: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LockState {
    Missing,
    Live,
    #[cfg(any(unix, windows))]
    Dead,
    Unverifiable,
}

#[derive(Clone, Copy)]
enum CrashPhase {
    AfterRawSync,
    AfterSidecarRename,
}

impl CrashPhase {
    const fn name(self) -> &'static str {
        match self {
            Self::AfterRawSync => "after_raw_sync",
            Self::AfterSidecarRename => "after_sidecar_rename",
        }
    }

    const fn error(self) -> &'static str {
        match self {
            Self::AfterRawSync => "simulated cache publication crash after raw synchronization",
            Self::AfterSidecarRename => "simulated cache publication crash after sidecar rename",
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LockRecord {
    schema_version: u8,
    pid: u32,
    created_unix_ms: u64,
    #[cfg(target_os = "linux")]
    #[serde(default)]
    pid_namespace: Option<u64>,
}

struct ObservedLock {
    bytes: Vec<u8>,
    state: LockState,
}

pub(crate) struct WriterLock {
    path: PathBuf,
    bytes: Vec<u8>,
    _file: Option<File>,
    remove_on_drop: bool,
}

impl WriterLock {
    fn acquire(paths: &PublicationPaths) -> Result<Self> {
        let started = Instant::now();
        loop {
            match create_lock(&paths.lock) {
                Ok(locked) => return Self::from_locked(paths, locked),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let observed = observe_lock(&paths.lock)?;
                    match observed.state {
                        LockState::Missing => continue,
                        #[cfg(any(unix, windows))]
                        LockState::Dead => {
                            if let Some(locked) = recover_dead_lock(&paths.lock, &observed)? {
                                return Self::from_locked(paths, locked);
                            }
                        }
                        LockState::Live if started.elapsed() < LIVE_LOCK_WAIT => {
                            thread::sleep(LOCK_RETRY);
                        }
                        LockState::Live => bail!("pi cache writer lock belongs to a live process"),
                        LockState::Unverifiable => {
                            bail!("pi cache writer lock cannot be verified")
                        }
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn from_locked(paths: &PublicationPaths, locked: LockedFile) -> Result<Self> {
        let LockedFile { file, bytes } = locked;
        if let Err(error) = paths.sync_parent() {
            remove_owned_lock(&paths.lock, &bytes);
            drop(file);
            return Err(error);
        }
        Ok(Self {
            path: paths.lock.clone(),
            bytes,
            _file: Some(file),
            remove_on_drop: true,
        })
    }

    pub(crate) fn fail_after_raw_sync(&mut self) -> Result<()> {
        self.fail_at(CrashPhase::AfterRawSync)
    }

    pub(crate) fn fail_after_sidecar_rename(&mut self) -> Result<()> {
        self.fail_at(CrashPhase::AfterSidecarRename)
    }

    fn fail_at(&mut self, phase: CrashPhase) -> Result<()> {
        if crate::gpu_ring::test_mode_enabled()
            && std::env::var("PI_CASSO_TEST_CACHE_CRASH_PHASE").as_deref() == Ok(phase.name())
        {
            self.remove_on_drop = false;
            CRASH_TRIGGERED.set(true);
            bail!(phase.error());
        }
        Ok(())
    }
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        if self.remove_on_drop {
            remove_owned_lock(&self.path, &self.bytes);
        }
    }
}

pub(crate) fn with_writer_lock<T>(
    paths: &PublicationPaths,
    operation: impl FnOnce(&mut WriterLock) -> Result<T>,
) -> Result<T> {
    let key = absolute_path(&paths.raw)?;
    if HELD_PATHS.with_borrow(|held| held.contains(&key)) {
        let mut nested = WriterLock {
            path: paths.lock.clone(),
            bytes: Vec::new(),
            _file: None,
            remove_on_drop: false,
        };
        return operation(&mut nested);
    }
    let local = local_lock(&paths.raw)?;
    let _local_guard = local
        .lock()
        .map_err(|_| anyhow!("process-local pi cache lock was poisoned"))?;
    let mut writer = WriterLock::acquire(paths)?;
    HELD_PATHS.with_borrow_mut(|held| held.push(key.clone()));
    CRASH_TRIGGERED.set(false);
    let result = operation(&mut writer);
    HELD_PATHS.with_borrow_mut(|held| {
        if let Some(position) = held.iter().rposition(|path| path == &key) {
            held.remove(position);
        }
    });
    if CRASH_TRIGGERED.get() {
        writer.remove_on_drop = false;
    }
    result
}

pub(crate) fn with_exclusive<T>(
    raw_path: &Path,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let paths = PublicationPaths::new(raw_path)?;
    with_writer_lock(&paths, |_writer| operation())
}

pub(crate) fn observe(path: &Path) -> Result<LockState> {
    Ok(observe_lock(path)?.state)
}

fn observe_lock(path: &Path) -> Result<ObservedLock> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ObservedLock {
                bytes: Vec::new(),
                state: LockState::Missing,
            });
        }
        Err(error) => return Err(error.into()),
    };
    let Ok(record) = serde_json::from_slice::<LockRecord>(&bytes) else {
        return Ok(ObservedLock {
            bytes,
            state: LockState::Unverifiable,
        });
    };
    if record.schema_version != 1 || record.pid == 0 || record.created_unix_ms == 0 {
        return Ok(ObservedLock {
            bytes,
            state: LockState::Unverifiable,
        });
    }
    Ok(ObservedLock {
        bytes,
        state: record_state(&record)?,
    })
}

struct LockedFile {
    file: File,
    bytes: Vec<u8>,
}

fn create_lock(path: &Path) -> std::io::Result<LockedFile> {
    let bytes = lock_record_bytes()?;

    #[cfg(windows)]
    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)?;
        let result = (|| {
            if !try_lock_file(&file)? {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "new pi cache lock could not be acquired",
                ));
            }
            write_lock_record(&mut file, &bytes)
        })();
        if let Err(error) = result {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
        return Ok(LockedFile { file, bytes });
    }

    #[cfg(not(windows))]
    {
        let sequence = LOCK_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "lock path is not UTF-8")
            })?;
        let temp = path.with_file_name(format!(
            ".{}.{}.{}.tmp",
            file_name,
            std::process::id(),
            sequence
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&temp)?;
        let result = (|| {
            if !try_lock_file(&file)? {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "new pi cache lock could not be acquired",
                ));
            }
            write_lock_record(&mut file, &bytes)?;
            fs::hard_link(&temp, path)
        })();
        let cleanup = fs::remove_file(&temp);
        match result {
            Ok(()) => match cleanup {
                Ok(()) => Ok(LockedFile { file, bytes }),
                Err(error) => {
                    remove_owned_lock(path, &bytes);
                    drop(file);
                    Err(error)
                }
            },
            Err(error) => {
                drop(file);
                let _ = cleanup;
                Err(error)
            }
        }
    }
}

#[cfg(any(unix, windows))]
fn recover_dead_lock(path: &Path, observed: &ObservedLock) -> std::io::Result<Option<LockedFile>> {
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !try_lock_file(&file)? {
        return Ok(None);
    }
    let current_bytes = read_lock_file(&mut file)?;
    if current_bytes != observed.bytes {
        return Ok(None);
    }
    let record =
        serde_json::from_slice::<LockRecord>(&current_bytes).map_err(std::io::Error::other)?;
    if record_state(&record).map_err(|error| std::io::Error::other(error.to_string()))?
        != LockState::Dead
    {
        return Ok(None);
    }
    let bytes = lock_record_bytes()?;
    write_lock_record(&mut file, &bytes)?;
    Ok(Some(LockedFile { file, bytes }))
}

fn lock_record_bytes() -> std::io::Result<Vec<u8>> {
    let created_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_millis();
    let record = LockRecord {
        schema_version: 1,
        pid: std::process::id(),
        created_unix_ms: u64::try_from(created_unix_ms).map_err(std::io::Error::other)?,
        #[cfg(target_os = "linux")]
        pid_namespace: current_pid_namespace(),
    };
    serde_json::to_vec(&record).map_err(std::io::Error::other)
}

fn read_lock_file(file: &mut File) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn write_lock_record(file: &mut File, bytes: &[u8]) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()
}

fn remove_owned_lock(path: &Path, bytes: &[u8]) {
    let owns_path = fs::read(path).is_ok_and(|current| current.as_slice() == bytes);
    if owns_path && fs::remove_file(path).is_ok() {
        if let Some(parent) = path.parent() {
            let _ = File::open(parent).and_then(|directory| directory.sync_all());
        }
    }
}

#[cfg(unix)]
fn try_lock_file(file: &File) -> std::io::Result<bool> {
    use std::os::unix::io::AsRawFd;

    // SAFETY: `file` owns a valid descriptor for the duration of this call;
    // `flock` only changes the advisory lock state of that descriptor.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::EACCES || code == libc::EAGAIN => Ok(false),
        _ => Err(error),
    }
}

#[cfg(windows)]
fn try_lock_file(file: &File) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut overlapped = OVERLAPPED::default();
    // SAFETY: `file` owns a valid handle and `overlapped` is a valid mutable
    // zero-initialized structure for the synchronous, non-blocking call.
    let result = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if result != 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(all(not(unix), not(windows)))]
fn try_lock_file(_file: &File) -> std::io::Result<bool> {
    Ok(true)
}

fn local_lock(path: &Path) -> Result<Arc<Mutex<()>>> {
    let key = absolute_path(path)?;
    let mut locks = LOCAL_LOCKS
        .lock()
        .map_err(|_| anyhow!("pi cache lock registry was poisoned"))?;
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[cfg(target_os = "linux")]
fn record_state(record: &LockRecord) -> Result<LockState> {
    if record.pid_namespace.is_none() || record.pid_namespace != current_pid_namespace() {
        return Ok(LockState::Unverifiable);
    }
    process_state(record.pid)
}

#[cfg(not(target_os = "linux"))]
fn record_state(record: &LockRecord) -> Result<LockState> {
    process_state(record.pid)
}

#[cfg(target_os = "linux")]
fn current_pid_namespace() -> Option<u64> {
    fs::metadata("/proc/self/ns/pid")
        .ok()
        .map(|metadata| metadata.ino())
}

#[cfg(target_os = "linux")]
fn proc_visibility_is_ambiguous() -> bool {
    let Ok(mounts) = fs::read_to_string("/proc/mounts") else {
        return true;
    };
    let mut found_proc = false;
    for line in mounts.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.get(1) != Some(&"/proc") || fields.get(2) != Some(&"proc") {
            continue;
        }
        found_proc = true;
        if fields.get(3).is_some_and(|options| {
            options
                .split(',')
                .any(|option| option.starts_with("hidepid="))
        }) {
            return true;
        }
    }
    !found_proc
}

#[cfg(target_os = "linux")]
fn process_state(pid: u32) -> Result<LockState> {
    match fs::metadata(format!("/proc/{pid}")) {
        Ok(_) => Ok(LockState::Live),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if proc_visibility_is_ambiguous() {
                return Ok(LockState::Unverifiable);
            }
            let pid = libc::pid_t::try_from(pid)
                .map_err(|_| anyhow!("pi cache writer lock pid does not fit this platform"))?;
            // SAFETY: `pid` was checked for platform range; signal 0 probes
            // liveness without delivering a signal or mutating the process.
            let result = unsafe { libc::kill(pid, 0) };
            if result == 0 {
                return Ok(LockState::Live);
            }
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                Some(code) if code == libc::ESRCH => Ok(LockState::Dead),
                Some(code) if code == libc::EPERM => Ok(LockState::Unverifiable),
                _ => Err(error.into()),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            Ok(LockState::Unverifiable)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn process_state(pid: u32) -> Result<LockState> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: `OpenProcess` is called with a plain PID and read-only query
    // access; the returned handle is closed on every path below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            Ok(LockState::Dead)
        } else {
            Ok(LockState::Unverifiable)
        };
    }

    let mut exit_code = 0;
    // SAFETY: `handle` was returned by `OpenProcess`, and `exit_code` is a
    // valid writable output buffer.
    let status = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    if status == 0 {
        let error = std::io::Error::last_os_error();
        // SAFETY: `handle` is the live handle returned above and is closed once.
        unsafe {
            CloseHandle(handle);
        }
        return Err(error.into());
    }
    // SAFETY: `handle` is the live handle returned above and is closed once.
    unsafe {
        CloseHandle(handle);
    }
    if exit_code == STILL_ACTIVE as u32 {
        Ok(LockState::Live)
    } else {
        Ok(LockState::Dead)
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_state(pid: u32) -> Result<LockState> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| anyhow!("pi cache writer lock pid does not fit this platform"))?;
    // SAFETY: `pid` was checked for platform range; signal 0 probes liveness
    // without delivering a signal or mutating the process.
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return Ok(LockState::Live);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::ESRCH => Ok(LockState::Dead),
        Some(code) if code == libc::EPERM => Ok(LockState::Unverifiable),
        _ => Err(error.into()),
    }
}

#[cfg(all(not(unix), not(windows)))]
fn process_state(pid: u32) -> Result<LockState> {
    if pid == std::process::id() {
        Ok(LockState::Live)
    } else {
        Ok(LockState::Unverifiable)
    }
}
