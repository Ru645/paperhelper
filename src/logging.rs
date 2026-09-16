//! 轻量日志：同时写 stderr 与 `.paperhelper/logs/paperhelper.log`。
//!
//! - 级别由环境变量 `PAPERHELPER_LOG` 控制：error / warn / info / debug（默认 info）。
//! - 日志文件超过 5MB 时轮转为 `paperhelper.log.1`（只保留一份历史）。
//! - 无第三方依赖；`init()` 在 `main` 最早期调用，之后各模块直接用
//!   `logging::info(...)` 等函数即可（`init` 之前调用也能输出到 stderr）。
//!
//! 注意：**绝不记录 API Key 等敏感信息**，端点/模型等可记录。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }

    fn from_env() -> Self {
        match std::env::var("PAPERHELPER_LOG").unwrap_or_default().to_lowercase().as_str() {
            "error" => Level::Error,
            "warn" | "warning" => Level::Warn,
            "debug" | "trace" => Level::Debug,
            _ => Level::Info,
        }
    }
}

struct Logger {
    level: Level,
    file: Option<File>,
}

static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// 初始化日志（幂等）。应在程序最早期调用。
pub fn init() {
    let level = Level::from_env();
    let file = open_log_file();
    let _ = LOGGER.set(Mutex::new(Logger { level, file }));
}

/// 日志文件路径（供启动横幅展示）。
pub fn log_path() -> std::path::PathBuf {
    crate::paths::logs_dir().join("paperhelper.log")
}

fn open_log_file() -> Option<File> {
    if let Err(e) = crate::paths::ensure_logs_dir() {
        eprintln!("[logging] 无法创建日志目录: {e:#}");
        return None;
    }
    let dir = crate::paths::logs_dir();
    let path = dir.join("paperhelper.log");
    if let Ok(md) = fs::metadata(&path) {
        if md.len() > MAX_LOG_BYTES {
            let _ = fs::rename(&path, dir.join("paperhelper.log.1"));
        }
    }
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("[logging] 无法打开日志文件 {}: {e}", path.display());
            None
        }
    }
}

/// 写一条日志（级别过滤后同时写 stderr 与文件）。
pub fn log(level: Level, msg: &str) {
    let enabled = LOGGER
        .get()
        .and_then(|l| l.lock().ok().map(|g| level <= g.level))
        .unwrap_or(true);
    if !enabled {
        return;
    }
    let line = format!(
        "{} [{}] {}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        level.label(),
        crate::output::strip_ansi(msg)
    );
    eprintln!("{line}");
    if let Some(l) = LOGGER.get() {
        if let Ok(mut g) = l.lock() {
            if let Some(f) = g.file.as_mut() {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}

pub fn error(msg: impl AsRef<str>) {
    log(Level::Error, msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    log(Level::Warn, msg.as_ref());
}

pub fn info(msg: impl AsRef<str>) {
    log(Level::Info, msg.as_ref());
}

pub fn debug(msg: impl AsRef<str>) {
    log(Level::Debug, msg.as_ref());
}
