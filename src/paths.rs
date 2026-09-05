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

/// 列出所有已保存的会话编号（文件名去掉 .json）
pub fn list_sessions() -> Vec<String> {
    let dir = sessions_dir();
    if !dir.exists() {
        return Vec::new();
    }
    let mut sessions = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.path().file_stem().and_then(|s| s.to_str()) {
                sessions.push(name.to_string());
            }
        }
    }
    sessions.sort();
    sessions
}

/// 会话文件路径
pub fn session_path(id: &str) -> PathBuf {
    sessions_dir().join(format!("{id}.json"))
}
