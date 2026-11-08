//! File-based diagnostics logging for the LSP server.
//!
//! The server speaks JSON-RPC over **stdout**, so diagnostics must never be
//! printed there (or through `println!`). Everything goes to a dated file under
//! a user-configured directory. Logging is completely disabled when the
//! configured directory is empty.
//!
//! One file per UTC day (`ld-java-analyze-YYYY-MM-DD.log`), retained for 7 days.
//! Stale files are removed whenever a logger is created (i.e. on every server
//! start, and when `logDir` changes at runtime).

use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const RETENTION_DAYS: i64 = 7;
const PREFIX: &str = "ld-java-analyze-";
const SUFFIX: &str = ".log";

#[derive(Debug)]
pub struct Logger {
    dir: PathBuf,
    current: Mutex<Option<(String, File)>>,
}

impl Logger {
    /// Enables file logging, creates the directory and prunes stale logs.
    /// Returns `None` when the directory cannot be created.
    pub fn enable(dir: PathBuf) -> Option<Self> {
        fs::create_dir_all(&dir).ok()?;
        let removed = cleanup(&dir, RETENTION_DAYS);
        let logger = Self {
            dir,
            current: Mutex::new(None),
        };
        logger.write(
            "INFO",
            "startup",
            &format!(
                "日志目录 {}；保留 {RETENTION_DAYS} 天；本次清理历史日志 {} 个",
                logger.dir.display(),
                removed.len()
            ),
        );
        Some(logger)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn info(&self, scope: &str, message: impl AsRef<str>) {
        self.write("INFO", scope, message.as_ref());
    }

    pub fn warn(&self, scope: &str, message: impl AsRef<str>) {
        self.write("WARN", scope, message.as_ref());
    }

    pub fn error(&self, scope: &str, message: impl AsRef<str>) {
        self.write("ERROR", scope, message.as_ref());
    }

    pub fn write(&self, level: &str, scope: &str, message: &str) {
        let (days, millis) = now_utc();
        let day = date_string(days);
        let mut guard = self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.as_ref().map(|(open, _)| open.as_str()) != Some(day.as_str()) {
            let path = self.dir.join(format!("{PREFIX}{day}{SUFFIX}"));
            // If the directory became unwritable, drop the entry silently:
            // logging must never break the JSON-RPC transport.
            let opened = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .ok();
            *guard = opened.map(|file| (day.clone(), file));
        }
        if let Some((_, file)) = guard.as_mut() {
            let _ = writeln!(
                file,
                "{} [{}] [{}] {}",
                time_string(millis),
                level,
                scope,
                message
            );
            let _ = file.flush();
        }
    }
}

/// Reads `logDir` from either a flat initialization object or a `ld-java-analyze` wrapper.
/// `None` means "key absent, keep the current setting"; `Some("")` disables logging.
pub fn log_dir_from_settings(settings: &Value) -> Option<String> {
    let settings = settings
        .get("ld-java-analyze")
        .filter(|value| value.is_object())
        .unwrap_or(settings);
    settings
        .get("logDir")
        .or_else(|| settings.get("log_dir"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Optional editor-independent config: `<workspace>/.ld-java-analyze/config.json`.
/// `initializationOptions.logDir` takes precedence when present.
pub fn log_dir_from_file(state_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(state_dir.join("config.json")).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value
        .get("logDir")
        .or_else(|| value.get("log_dir"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Empty config disables logging. Relative paths are resolved against the
/// workspace root so the default behaviour never writes to a user profile dir.
pub fn resolve_log_dir(root: &Path, configured: &str) -> Option<PathBuf> {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    Some(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

/// Deletes `ld-java-analyze-*.log` files older than the retention window. Files with a
/// different name are never touched. Returns the removed paths.
pub fn cleanup(dir: &Path, retention_days: i64) -> Vec<PathBuf> {
    let today = now_utc().0;
    let cutoff = date_string(today - (retention_days - 1));
    let mut removed = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return removed;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(PREFIX) || !name.ends_with(SUFFIX) {
            continue;
        }
        let stem = &name[PREFIX.len()..name.len() - SUFFIX.len()];
        let bytes = stem.as_bytes();
        let dated = stem.len() == 10 && bytes[4] == b'-' && bytes[7] == b'-';
        let expired = if dated {
            stem < cutoff.as_str()
        } else {
            // Unknown naming: fall back to the last-modified time.
            entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .is_some_and(|age| today - (age.as_secs() as i64 / 86_400) >= retention_days)
        };
        if expired && fs::remove_file(entry.path()).is_ok() {
            removed.push(entry.path());
        }
    }
    removed
}

fn now_utc() -> (i64, u32) {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = duration.as_secs() as i64;
    (
        seconds.div_euclid(86_400),
        (seconds.rem_euclid(86_400) * 1000) as u32 + duration.subsec_millis(),
    )
}

/// Converts days since 1970-01-01 into a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year as i32, month as u32, day as u32)
}

fn date_string(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

fn time_string(millis_of_day: u32) -> String {
    let total = millis_of_day / 1000;
    let millis = millis_of_day % 1000;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        total / 3600,
        (total / 60) % 60,
        total % 60,
        millis
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ld-java-analyze-log-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"old\n").unwrap();
    }

    #[test]
    fn civil_date_conversion_matches_known_epochs() {
        assert_eq!(date_string(0), "1970-01-01");
        assert_eq!(date_string(10_957), "2000-01-01");
        assert_eq!(date_string(20_765), "2026-11-08");
    }

    #[test]
    fn cleanup_keeps_last_seven_days_and_leaves_other_files_alone() {
        let dir = temp_dir("cleanup");
        let today = now_utc().0;
        for offset in 0..7 {
            touch(
                &dir,
                &format!("ld-java-analyze-{}.log", date_string(today - offset)),
            );
        }
        touch(
            &dir,
            &format!("ld-java-analyze-{}.log", date_string(today - 7)),
        );
        touch(
            &dir,
            &format!("ld-java-analyze-{}.log", date_string(today - 30)),
        );
        touch(&dir, "ld-java-analyze-notes.txt");
        touch(&dir, "other-2020-01-01.log");

        let removed = cleanup(&dir, RETENTION_DAYS);
        assert_eq!(removed.len(), 2);
        for offset in 0..7 {
            assert!(dir
                .join(format!(
                    "ld-java-analyze-{}.log",
                    date_string(today - offset)
                ))
                .exists());
        }
        assert!(dir.join("ld-java-analyze-notes.txt").exists());
        assert!(dir.join("other-2020-01-01.log").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_config_disables_logging_and_relative_paths_use_workspace() {
        let root = Path::new("E:/workspace");
        assert_eq!(resolve_log_dir(root, ""), None);
        assert_eq!(resolve_log_dir(root, "  "), None);
        assert_eq!(resolve_log_dir(root, "logs"), Some(root.join("logs")));
        assert_eq!(
            resolve_log_dir(root, "D:/logs/ld-java-analyze"),
            Some(PathBuf::from("D:/logs/ld-java-analyze"))
        );
    }

    #[test]
    fn settings_are_read_from_flat_and_namespaced_objects() {
        assert_eq!(
            log_dir_from_settings(&serde_json::json!({ "logDir": "logs" })).as_deref(),
            Some("logs")
        );
        assert_eq!(
            log_dir_from_settings(&serde_json::json!({ "ld-java-analyze": { "logDir": "" } }))
                .as_deref(),
            Some("")
        );
        assert_eq!(
            log_dir_from_settings(&serde_json::json!({ "excludeRules": [] })),
            None
        );
    }

    #[test]
    fn config_file_can_enable_logging() {
        let dir = temp_dir("config");
        assert_eq!(log_dir_from_file(&dir), None);
        fs::write(
            dir.join("config.json"),
            r#"{ "logDir": ".ld-java-analyze/logs" }"#,
        )
        .unwrap();
        assert_eq!(
            log_dir_from_file(&dir).as_deref(),
            Some(".ld-java-analyze/logs")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn enabled_logger_writes_a_dated_file() {
        let dir = temp_dir("write");
        let logger = Logger::enable(dir.clone()).unwrap();
        logger.info("test", "hello 日志");
        logger.warn("test", "something odd");
        let path = dir.join(format!("ld-java-analyze-{}.log", date_string(now_utc().0)));
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("[INFO] [test] hello 日志"));
        assert!(content.contains("[WARN] [test] something odd"));
        drop(logger);
        let _ = fs::remove_dir_all(&dir);
    }
}
