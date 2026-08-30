use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(any(unix, windows))]
use anyhow::Context;
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
    remove_on_drop: bool,
}

impl WriterLock {
    fn acquire(paths: &PublicationPaths) -> Result<Self> {
        let started = Instant::now();
        loop {
            match create_lock(&paths.lock) {
                Ok(()) => {
                    paths.sync_parent()?;
                    return Ok(Self {
                        path: paths.lock.clone(),
                        remove_on_drop: true,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let observed = observe_lock(&paths.lock)?;
                    match observed.state {
                        LockState::Missing => continue,
                        #[cfg(any(unix, windows))]
                        LockState::Dead => {
                            let current = observe_lock(&paths.lock)?;
                            if current.state != LockState::Dead || current.bytes != observed.bytes {
                                continue;
                            }
                            fs::remove_file(&paths.lock).with_context(|| {
                                format!("failed to remove dead lock {}", paths.lock.display())
                            })?;
                            paths.sync_parent()?;
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
            let _ = fs::remove_file(&self.path);
            if let Some(parent) = self.path.parent() {
                let _ = std::fs::File::open(parent).and_then(|directory| directory.sync_all());
            }
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

fn create_lock(path: &Path) -> std::io::Result<()> {
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
    let bytes = serde_json::to_vec(&record).map_err(std::io::Error::other)?;

    #[cfg(windows)]
    {
        let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.flush()?;
            file.sync_all()
        })();
        if result.is_err() {
            drop(file);
            let _ = fs::remove_file(path);
        }
        return result;
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
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)?;
            file.write_all(&bytes)?;
            file.flush()?;
            file.sync_all()?;
            fs::hard_link(&temp, path)
        })();
        let cleanup = fs::remove_file(&temp);
        match result {
            Ok(()) => match cleanup {
                Ok(()) => Ok(()),
                Err(error) => {
                    let _ = fs::remove_file(path);
                    Err(error)
                }
            },
            Err(error) => {
                let _ = cleanup;
                Err(error)
            }
        }
    }
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
    let status = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    if status == 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            CloseHandle(handle);
        }
        return Err(error.into());
    }
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
