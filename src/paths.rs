use std::fs;
use std::path::PathBuf;

use anyhow::Result;

const DATA_DIR: &str = ".paperhelper";

pub fn data_dir() -> PathBuf {
    PathBuf::from(DATA_DIR)
}

pub fn ensure_data_dir() -> Result<()> {
    if !data_dir().exists() {
        fs::create_dir_all(data_dir())?;
    }
    Ok(())
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

pub fn knowledge_path() -> PathBuf {
    data_dir().join("knowledge.json")
}

/// 会话存档目录
pub fn sessions_dir() -> PathBuf {
    data_dir().join("sessions")
}

/// 确保会话目录存在
pub fn ensure_sessions_dir() -> Result<()> {
    if !sessions_dir().exists() {
        fs::create_dir_all(sessions_dir())?;
    }
    Ok(())
}

/// 分配一个新的会话 ID（递增数字，持久化在 .paperhelper/sessions/counter）。
/// ID 一旦分配永不变，保证 -s <ID> 稳定。
pub fn next_session_id() -> Result<u64> {
    let counter = sessions_dir().join("counter");
    let id = if counter.exists() {
        fs::read_to_string(&counter)?.trim().parse::<u64>().unwrap_or(0) + 1
    } else {
        1
    };
    ensure_sessions_dir()?;
    fs::write(&counter, id.to_string())?;
    Ok(id)
}

/// 会话文件路径（按数字 ID）
pub fn session_path(id: u64) -> PathBuf {
    sessions_dir().join(format!("{id}.json"))
}

/// 列出所有会话 ID（数字，升序）
pub fn list_sessions() -> Vec<u64> {
    let dir = sessions_dir();
    if !dir.exists() {
        return Vec::new();
    }
    let mut ids = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Some(stem) = entry.path().file_stem().and_then(|s| s.to_str()) {
                if let Ok(id) = stem.parse::<u64>() {
                    ids.push(id);
                }
            }
        }
    }
    ids.sort();
    ids
}
