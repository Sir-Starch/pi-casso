use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(any(target_os = "linux", windows))]
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
    #[cfg(any(target_os = "linux", windows))]
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
                    match observe(&paths.lock)? {
                        LockState::Missing => continue,
                        #[cfg(any(target_os = "linux", windows))]
                        LockState::Dead => {
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
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LockState::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    let Ok(record) = serde_json::from_slice::<LockRecord>(&bytes) else {
        return Ok(LockState::Unverifiable);
    };
    if record.schema_version != 1 || record.pid == 0 || record.created_unix_ms == 0 {
        return Ok(LockState::Unverifiable);
    }
    process_state(record.pid)
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
fn process_state(pid: u32) -> Result<LockState> {
    match fs::metadata(format!("/proc/{pid}")) {
        Ok(_) => Ok(LockState::Live),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(LockState::Dead),
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

#[cfg(all(not(target_os = "linux"), not(windows)))]
fn process_state(pid: u32) -> Result<LockState> {
    if pid == std::process::id() {
        Ok(LockState::Live)
    } else {
        Ok(LockState::Unverifiable)
    }
}
