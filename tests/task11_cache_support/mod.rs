use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

pub const KNOWN_PI_PREFIX: &[u8] =
    b"3141592653589793238462643383279502884197169399375105820974944592307816406286208998628034825342117067";

pub struct CacheFixture {
    root: TempDir,
    data_dir: PathBuf,
    cache_path: PathBuf,
}

impl CacheFixture {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for CacheFixture {
    fn default() -> Self {
        let root = TempDir::new().expect("cache test root");
        let data_dir = root.path().join("data");
        fs::create_dir_all(root.path().join("tmp")).expect("cache test tmp");
        fs::create_dir_all(&data_dir).expect("cache test data");
        let cache_path = data_dir.join("pi-cache.txt");
        Self {
            root,
            data_dir,
            cache_path,
        }
    }
}

impl CacheFixture {
    pub fn command(&self) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("pi-casso"));
        command
            .env_remove("PI_CASSO_DATA_DIR")
            .env_remove("PI_CASSO_CONFIG")
            .env("PI_CASSO_TEST_MODE", "1")
            .env("PI_CASSO_DATA_DIR", &self.data_dir)
            .env("TMPDIR", self.root.path().join("tmp"));
        #[cfg(all(windows, target_env = "msvc"))]
        command.env("PI_CASSO_TEST_GENERATOR_VARIANT", "spigot-persistent");
        command
    }

    pub fn output(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .output()
            .expect("pi-casso command starts")
    }

    pub fn generate(&self, digits: u64) -> Output {
        let mut command = self.command();
        command.args([
            "pi",
            "generate",
            "--digits",
            &digits.to_string(),
            "--generator-backend",
            "cpu",
            "--workers",
            "1",
        ]);
        command.output().expect("pi generation starts")
    }

    pub fn import_file(&self, source: &Path) -> Output {
        self.command()
            .args(["pi", "import"])
            .arg(source)
            .output()
            .expect("pi import starts")
    }

    pub fn repair(&self) -> Output {
        self.output(&["pi", "cache-repair", "--force"])
    }

    pub fn info(&self) -> Value {
        let output = self.output(&["--json", "pi", "cache-info"]);
        assert!(
            output.status.success(),
            "cache-info failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("cache-info JSON")
    }

    pub fn raw(&self) -> Vec<u8> {
        fs::read(&self.cache_path).expect("published raw cache")
    }

    pub fn cache_path(&self) -> &Path {
        &self.cache_path
    }

    pub fn write_raw(&self, bytes: &[u8]) {
        fs::write(&self.cache_path, bytes).expect("raw cache fixture");
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    pub fn sidecar_path(&self) -> PathBuf {
        self.sidecar_path_if_present()
            .expect("published cache sidecar")
    }

    pub fn sidecar_path_if_present(&self) -> Option<PathBuf> {
        let mut paths = fs::read_dir(&self.data_dir)
            .expect("cache data directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path != &self.cache_path)
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths.into_iter().next()
    }

    pub fn previous_path_if_present(&self) -> Option<PathBuf> {
        fs::read_dir(&self.data_dir)
            .expect("cache data directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains("previous"))
            })
    }

    pub fn lock_path(&self) -> PathBuf {
        self.data_dir.join("pi-cache.digits.lock")
    }

    pub fn read_sidecar(&self) -> Value {
        serde_json::from_slice(&fs::read(self.sidecar_path()).expect("published sidecar bytes"))
            .expect("published sidecar JSON")
    }

    pub fn assert_published(&self, info: &Value, expected: &[u8]) {
        assert_eq!(info["sidecar_status"], "ok");
        assert_eq!(info["digits"], json!(expected.len()));
        assert_eq!(info["published_digits"], json!(expected.len()));
        assert_eq!(info["raw_file_size"], json!(expected.len()));
        assert_eq!(info["valid_ascii"], true);
        assert!(expected.iter().all(u8::is_ascii_digit));
        let hash = format!("{:x}", Sha256::digest(expected));
        assert_eq!(info["published_prefix_sha256"], hash);
        let sidecar = self.read_sidecar();
        assert_eq!(sidecar["schema_version"], 1);
        assert_eq!(sidecar["published_digits"], json!(expected.len()));
        assert_eq!(sidecar["raw_file_size"], json!(expected.len()));
        assert_eq!(sidecar["published_prefix_sha256"], hash);
    }

    pub fn write_lock_with_pid(&self, pid: u32) {
        let lock = serde_json::from_slice::<Value>(
            &fs::read(self.lock_path()).expect("writer lock bytes"),
        )
        .expect("writer lock JSON");
        let mut lock = lock;
        lock["pid"] = json!(pid);
        fs::write(
            self.lock_path(),
            serde_json::to_vec(&lock).expect("writer lock JSON bytes"),
        )
        .expect("live writer lock fixture");
    }
}

pub fn assert_failed(output: &Output, context: &str) {
    assert!(!output.status.success(), "{context} unexpectedly succeeded");
}

pub fn distinct_digits() -> &'static [u8] {
    b"2718281828459045235360287471352662497757247093699959574966967627"
}
