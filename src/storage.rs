use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use directories::BaseDirs;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::art::Bitmap;
use crate::digits::DigitSourceSpec;
use crate::search::{BestMatchDetails, MatchMode, TopMatch};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Paused,
    PerfectFound,
    SourceExhausted,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::PerfectFound => "perfect_found",
            Self::SourceExhausted => "source_exhausted",
        }
    }

    pub fn from_str(value: &str) -> Result<Self> {
        match value {
            "running" => Ok(Self::Running),
            "paused" => Ok(Self::Paused),
            "perfect_found" => Ok(Self::PerfectFound),
            "source_exhausted" => Ok(Self::SourceExhausted),
            other => bail!("unknown run status {other:?}"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
    pub source: DigitSourceSpec,
    pub template_name: Option<String>,
    pub art_hash: String,
    pub width: u32,
    pub height: u32,
    pub canvas_width: u32,
    pub canvas_height: u32,
    pub match_mode: MatchMode,
    pub threshold: u8,
    pub invert_enabled: bool,
    pub current_offset: u64,
    pub scanned_windows: u64,
    pub best_score: f64,
    pub best_offset: Option<u64>,
    pub best_bitmap: Option<Bitmap>,
    pub best_inverted: bool,
    pub best_match: Option<BestMatchDetails>,
    pub target_bitmap: Bitmap,
    pub status: RunStatus,
    pub total_runtime_secs: f64,
    pub generated_digit_count: u64,
    pub params_json: String,
    pub top_matches: Vec<TopMatch>,
}

#[derive(Clone, Debug)]
pub struct NewRun {
    pub name: String,
    pub source: DigitSourceSpec,
    pub template_name: Option<String>,
    pub art_hash: String,
    pub width: u32,
    pub height: u32,
    pub canvas_width: u32,
    pub canvas_height: u32,
    pub match_mode: MatchMode,
    pub threshold: u8,
    pub invert_enabled: bool,
    pub start_offset: Option<u64>,
    pub target_bitmap: Bitmap,
    pub generated_digit_count: u64,
    pub params_json: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BestEventRecord {
    pub id: i64,
    pub run_id: String,
    pub timestamp: String,
    pub offset: u64,
    pub score: f64,
    pub bitmap: Bitmap,
    pub inverted: bool,
    pub scanned_windows: u64,
    #[serde(default)]
    pub details: Option<BestMatchDetails>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BestSummary {
    pub score: f64,
    pub offset: Option<u64>,
    pub bitmap: Option<Bitmap>,
    pub inverted: bool,
    pub details: Option<BestMatchDetails>,
}

impl RunRecord {
    pub fn best_summary(&self) -> BestSummary {
        BestSummary {
            score: self.best_score,
            offset: self.best_offset,
            bitmap: self.best_bitmap.clone(),
            inverted: self.best_inverted,
            details: self.best_match.clone(),
        }
    }
}

pub struct Storage {
    conn: Connection,
}

impl Storage {
    pub fn open_default() -> Result<Self> {
        let path = db_path()?;
        Self::open_path(path)
    }

    pub fn open_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database {}", path.display()))?;
        configure_connection(&conn)
            .with_context(|| format!("failed to configure database {}", path.display()))?;
        let storage = Self { conn };
        storage
            .migrate()
            .with_context(|| format!("failed to initialize database {}", path.display()))?;
        Ok(storage)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                source_type TEXT NOT NULL,
                source_path TEXT,
                source_allow_decimal_prefix INTEGER NOT NULL DEFAULT 0,
                template_name TEXT,
                art_hash TEXT NOT NULL,
                width INTEGER NOT NULL,
                height INTEGER NOT NULL,
                canvas_width INTEGER NOT NULL DEFAULT 0,
                canvas_height INTEGER NOT NULL DEFAULT 0,
                match_mode TEXT NOT NULL DEFAULT 'emergence',
                threshold INTEGER NOT NULL,
                invert_enabled INTEGER NOT NULL,
                current_offset INTEGER NOT NULL,
                scanned_windows INTEGER NOT NULL,
                best_score REAL NOT NULL,
                best_offset INTEGER,
                best_bitmap TEXT,
                best_inverted INTEGER NOT NULL DEFAULT 0,
                best_match_json TEXT,
                target_bitmap TEXT NOT NULL,
                status TEXT NOT NULL,
                total_runtime_secs REAL NOT NULL DEFAULT 0,
                generated_digit_count INTEGER NOT NULL DEFAULT 0,
                params_json TEXT NOT NULL DEFAULT '{}',
                top_matches TEXT NOT NULL DEFAULT '[]'
            );

            CREATE TABLE IF NOT EXISTS best_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                offset INTEGER NOT NULL,
                score REAL NOT NULL,
                bitmap TEXT NOT NULL,
                inverted INTEGER NOT NULL DEFAULT 0,
                scanned_windows INTEGER NOT NULL,
                match_json TEXT,
                FOREIGN KEY(run_id) REFERENCES runs(id) ON DELETE CASCADE
            );
            "#,
        )?;
        if !self.column_exists("runs", "source_allow_decimal_prefix")? {
            self.conn.execute(
                "ALTER TABLE runs ADD COLUMN source_allow_decimal_prefix INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !self.column_exists("runs", "generated_digit_count")? {
            self.conn.execute(
                "ALTER TABLE runs ADD COLUMN generated_digit_count INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !self.column_exists("runs", "canvas_width")? {
            self.conn.execute(
                "ALTER TABLE runs ADD COLUMN canvas_width INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !self.column_exists("runs", "canvas_height")? {
            self.conn.execute(
                "ALTER TABLE runs ADD COLUMN canvas_height INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !self.column_exists("runs", "match_mode")? {
            self.conn.execute(
                "ALTER TABLE runs ADD COLUMN match_mode TEXT NOT NULL DEFAULT 'threshold'",
                [],
            )?;
        }
        if !self.column_exists("runs", "best_match_json")? {
            self.conn
                .execute("ALTER TABLE runs ADD COLUMN best_match_json TEXT", [])?;
        }
        if !self.column_exists("best_events", "match_json")? {
            self.conn
                .execute("ALTER TABLE best_events ADD COLUMN match_json TEXT", [])?;
        }
        Ok(())
    }

    fn column_exists(&self, table: &str, column: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        for row in rows {
            if row? == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn create_run(&mut self, new: NewRun) -> Result<RunRecord> {
        if new.threshold > 9 {
            bail!("threshold must be between 0 and 9");
        }
        let id = Uuid::new_v4().simple().to_string();
        let now = Utc::now().to_rfc3339();
        let start_offset = new.start_offset.unwrap_or(0);
        let target_bits = new.target_bitmap.to_bit_string();
        self.conn
            .execute(
                r#"
            INSERT INTO runs (
                id, name, created_at, updated_at, source_type, source_path,
                source_allow_decimal_prefix, template_name, art_hash, width, height,
                canvas_width, canvas_height, match_mode, threshold, invert_enabled, current_offset,
                scanned_windows, best_score, best_offset, best_bitmap, best_inverted,
                best_match_json, target_bitmap, status, total_runtime_secs, generated_digit_count,
                params_json, top_matches
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                ?17, 0, 0.0, NULL, NULL, 0, NULL, ?18, ?19, 0.0, ?20, ?21, '[]')
            "#,
                params![
                    id,
                    new.name,
                    now,
                    now,
                    new.source.source_type,
                    new.source.source_path,
                    bool_to_i64(new.source.allow_decimal_prefix),
                    new.template_name,
                    new.art_hash,
                    new.width,
                    new.height,
                    new.canvas_width,
                    new.canvas_height,
                    new.match_mode.as_str(),
                    new.threshold,
                    bool_to_i64(new.invert_enabled),
                    u64_to_i64(start_offset)?,
                    target_bits,
                    RunStatus::Paused.as_str(),
                    u64_to_i64(new.generated_digit_count)?,
                    new.params_json,
                ],
            )
            .with_context(|| "failed to create run; run names must be unique")?;

        self.resolve_run(&id)
    }

    pub fn update_run(&self, run: &mut RunRecord) -> Result<()> {
        run.updated_at = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            UPDATE runs SET
                updated_at = ?2,
                current_offset = ?3,
                scanned_windows = ?4,
                best_score = ?5,
                best_offset = ?6,
                best_bitmap = ?7,
                best_inverted = ?8,
                best_match_json = ?9,
                status = ?10,
                total_runtime_secs = ?11,
                generated_digit_count = ?12,
                top_matches = ?13
            WHERE id = ?1
            "#,
            params![
                run.id,
                run.updated_at,
                u64_to_i64(run.current_offset)?,
                u64_to_i64(run.scanned_windows)?,
                run.best_score,
                opt_u64_to_i64(run.best_offset)?,
                run.best_bitmap.as_ref().map(Bitmap::to_bit_string),
                bool_to_i64(run.best_inverted),
                run.best_match
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                run.status.as_str(),
                run.total_runtime_secs,
                u64_to_i64(run.generated_digit_count)?,
                serde_json::to_string(&run.top_matches)?,
            ],
        )?;
        Ok(())
    }

    pub fn insert_best_event(&self, event: &BestEventRecord) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO best_events (
                run_id, timestamp, offset, score, bitmap, inverted, scanned_windows, match_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                event.run_id,
                event.timestamp,
                u64_to_i64(event.offset)?,
                event.score,
                event.bitmap.to_bit_string(),
                bool_to_i64(event.inverted),
                u64_to_i64(event.scanned_windows)?,
                event
                    .details
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
            ],
        )?;
        Ok(())
    }

    pub fn resolve_run(&self, id_or_name: &str) -> Result<RunRecord> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, name, created_at, updated_at, source_type, source_path,
                source_allow_decimal_prefix, template_name, art_hash, width, height,
                threshold, invert_enabled, current_offset,
                scanned_windows, best_score, best_offset, best_bitmap, best_inverted,
                target_bitmap, status, total_runtime_secs, generated_digit_count, params_json,
                top_matches, canvas_width, canvas_height, match_mode, best_match_json
            FROM runs WHERE id = ?1 OR name = ?1
            "#,
        )?;
        stmt.query_row([id_or_name], row_to_run)
            .optional()?
            .ok_or_else(|| anyhow!("run {id_or_name:?} was not found"))
    }

    pub fn list_runs(&self) -> Result<Vec<RunRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, name, created_at, updated_at, source_type, source_path,
                source_allow_decimal_prefix, template_name, art_hash, width, height,
                threshold, invert_enabled, current_offset,
                scanned_windows, best_score, best_offset, best_bitmap, best_inverted,
                target_bitmap, status, total_runtime_secs, generated_digit_count, params_json,
                top_matches, canvas_width, canvas_height, match_mode, best_match_json
            FROM runs ORDER BY updated_at DESC
            "#,
        )?;
        let rows = stmt.query_map([], row_to_run)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn history(&self, run_id: &str, limit: Option<usize>) -> Result<Vec<BestEventRecord>> {
        let run = self.resolve_run(run_id)?;
        let width = run.width as usize;
        let height = run.height as usize;
        let sql = match limit {
            Some(limit) => format!(
                "SELECT id, run_id, timestamp, offset, score, bitmap, inverted, scanned_windows, match_json \
                 FROM best_events WHERE run_id = ?1 ORDER BY offset ASC LIMIT {limit}"
            ),
            None => {
                "SELECT id, run_id, timestamp, offset, score, bitmap, inverted, scanned_windows, match_json \
                 FROM best_events WHERE run_id = ?1 ORDER BY offset ASC"
                    .to_string()
            }
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let bitmap_width = if run.match_mode == MatchMode::Emergence {
            run.canvas_width as usize
        } else {
            width
        };
        let bitmap_height = if run.match_mode == MatchMode::Emergence {
            run.canvas_height as usize
        } else {
            height
        };
        let rows = stmt.query_map([run_id], |row| {
            row_to_event(row, bitmap_width, bitmap_height)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn delete_run(&mut self, id_or_name: &str) -> Result<RunRecord> {
        let run = self.resolve_run(id_or_name)?;
        self.conn
            .execute("DELETE FROM best_events WHERE run_id = ?1", [&run.id])?;
        self.conn
            .execute("DELETE FROM runs WHERE id = ?1", [&run.id])?;
        Ok(run)
    }
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    let width: u32 = row.get::<_, i64>(9)? as u32;
    let height: u32 = row.get::<_, i64>(10)? as u32;
    let threshold: u8 = row.get::<_, i64>(11)? as u8;
    let target_bits: String = row.get(19)?;
    let best_bits: Option<String> = row.get(17)?;
    let top_matches_json: String = row.get(24)?;
    let raw_canvas_width: u32 = row.get::<_, i64>(25)? as u32;
    let raw_canvas_height: u32 = row.get::<_, i64>(26)? as u32;
    let match_mode_string: String = row.get(27)?;
    let match_mode = MatchMode::from_str(&match_mode_string).map_err(to_sql_from_err)?;
    let best_match_json: Option<String> = row.get(28)?;
    let canvas_width = if raw_canvas_width == 0 {
        width
    } else {
        raw_canvas_width
    };
    let canvas_height = if raw_canvas_height == 0 {
        height
    } else {
        raw_canvas_height
    };
    let source = DigitSourceSpec {
        source_type: row.get(4)?,
        source_path: row.get(5)?,
        allow_decimal_prefix: row.get::<_, i64>(6)? != 0,
    };
    let status_string: String = row.get(20)?;
    let status = RunStatus::from_str(&status_string).map_err(to_sql_from_err)?;
    let target_bitmap = Bitmap::from_bit_string(width as usize, height as usize, &target_bits)
        .map_err(to_sql_from_err)?;
    let best_bitmap_width = if match_mode == MatchMode::Emergence {
        canvas_width as usize
    } else {
        width as usize
    };
    let best_bitmap_height = if match_mode == MatchMode::Emergence {
        canvas_height as usize
    } else {
        height as usize
    };
    let best_bitmap = best_bits
        .as_deref()
        .map(|bits| Bitmap::from_bit_string(best_bitmap_width, best_bitmap_height, bits))
        .transpose()
        .map_err(to_sql_from_err)?;
    let top_matches = serde_json::from_str(&top_matches_json).unwrap_or_default();
    let best_match = best_match_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .unwrap_or_default();

    Ok(RunRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
        source,
        template_name: row.get(7)?,
        art_hash: row.get(8)?,
        width,
        height,
        canvas_width,
        canvas_height,
        match_mode,
        threshold,
        invert_enabled: row.get::<_, i64>(12)? != 0,
        current_offset: i64_to_u64(row.get(13)?).map_err(to_sql_from_err)?,
        scanned_windows: i64_to_u64(row.get(14)?).map_err(to_sql_from_err)?,
        best_score: row.get(15)?,
        best_offset: row
            .get::<_, Option<i64>>(16)?
            .map(i64_to_u64)
            .transpose()
            .map_err(to_sql_from_err)?,
        best_bitmap,
        best_inverted: row.get::<_, i64>(18)? != 0,
        best_match,
        target_bitmap,
        status,
        total_runtime_secs: row.get(21)?,
        generated_digit_count: i64_to_u64(row.get(22)?).map_err(to_sql_from_err)?,
        params_json: row.get(23)?,
        top_matches,
    })
}

fn row_to_event(
    row: &rusqlite::Row<'_>,
    width: usize,
    height: usize,
) -> rusqlite::Result<BestEventRecord> {
    let bits: String = row.get(5)?;
    let bitmap = Bitmap::from_bit_string(width, height, &bits).map_err(to_sql_from_err)?;
    let details_json: Option<String> = row.get(8)?;
    let details = details_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .unwrap_or_default();
    Ok(BestEventRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        timestamp: row.get(2)?,
        offset: i64_to_u64(row.get(3)?).map_err(to_sql_from_err)?,
        score: row.get(4)?,
        bitmap,
        inverted: row.get::<_, i64>(6)? != 0,
        scanned_windows: i64_to_u64(row.get(7)?).map_err(to_sql_from_err)?,
        details,
    })
}

/// The TUI runs a search worker on its own connection while the main thread keeps
/// reading run lists, so the database is genuinely used concurrently. WAL lets a
/// reader and a writer coexist, and the busy timeout absorbs the remaining
/// writer-vs-writer overlap instead of failing with `database is locked`.
fn configure_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;
        "#,
    )?;
    Ok(())
}

pub fn app_data_dir() -> Result<PathBuf> {
    if let Ok(env_dir) = std::env::var("PI_CASSO_DATA_DIR") {
        return Ok(PathBuf::from(env_dir));
    }
    let base = BaseDirs::new().ok_or_else(|| anyhow!("could not determine data directory"))?;
    Ok(base.data_dir().join("pi-casso"))
}

pub fn db_path() -> Result<PathBuf> {
    Ok(app_data_dir()?.join("pi-casso.db"))
}

fn bool_to_i64(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

fn i64_to_u64(value: i64) -> Result<u64> {
    if value < 0 {
        bail!("database contained a negative offset/count");
    }
    Ok(value as u64)
}

fn u64_to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| anyhow!("value {value} is too large for SQLite integer"))
}

fn opt_u64_to_i64(value: Option<u64>) -> Result<Option<i64>> {
    value.map(u64_to_i64).transpose()
}

fn to_sql_from_err(err: anyhow::Error) -> rusqlite::Error {
    let io_err = std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string());
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(io_err))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::art::Bitmap;

    #[test]
    fn saves_and_resolves_checkpoint() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("state.db");
        let mut storage = Storage::open_path(db).unwrap();
        let target = Bitmap::new(2, 2, vec![0, 1, 1, 0]).unwrap();
        let mut run = storage
            .create_run(NewRun {
                name: "test".to_string(),
                source: DigitSourceSpec::demo(),
                template_name: Some("pi".to_string()),
                art_hash: target.sha256(),
                width: 2,
                height: 2,
                canvas_width: 2,
                canvas_height: 2,
                match_mode: MatchMode::Threshold,
                threshold: 5,
                invert_enabled: false,
                start_offset: Some(4),
                target_bitmap: target,
                generated_digit_count: 0,
                params_json: "{}".to_string(),
            })
            .unwrap();
        run.current_offset = 9;
        run.scanned_windows = 5;
        storage.update_run(&mut run).unwrap();
        let loaded = storage.resolve_run("test").unwrap();
        assert_eq!(loaded.current_offset, 9);
        assert_eq!(loaded.scanned_windows, 5);
    }

    #[test]
    fn wal_mode_is_enabled() {
        let dir = tempdir().unwrap();
        let storage = Storage::open_path(dir.path().join("state.db")).unwrap();
        let mode: String = storage
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn a_writer_and_a_reader_can_share_the_database() {
        // The TUI does exactly this: the search worker checkpoints on one
        // connection while the UI thread lists runs on another. Without WAL and
        // a busy timeout this fails with "database is locked".
        let dir = tempdir().unwrap();
        let db = dir.path().join("state.db");
        let mut writer = Storage::open_path(&db).unwrap();
        let reader = Storage::open_path(&db).unwrap();

        let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
        let mut run = writer
            .create_run(NewRun {
                name: "shared".to_string(),
                source: DigitSourceSpec::demo(),
                template_name: None,
                art_hash: target.sha256(),
                width: 2,
                height: 2,
                canvas_width: 2,
                canvas_height: 2,
                match_mode: MatchMode::Threshold,
                threshold: 5,
                invert_enabled: false,
                start_offset: Some(0),
                target_bitmap: target,
                generated_digit_count: 0,
                params_json: "{}".to_string(),
            })
            .unwrap();

        for offset in 1..=25u64 {
            run.current_offset = offset;
            writer.update_run(&mut run).unwrap();
            let runs = reader.list_runs().expect("reader must not be locked out");
            assert_eq!(runs.len(), 1);
        }
        assert_eq!(reader.resolve_run("shared").unwrap().current_offset, 25);
    }
}
