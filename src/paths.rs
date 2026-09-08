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

/// 生成保存时间戳 ID（本地时间 %Y%m%d_%H%M%S），作为会话文件名与恢复标识。
pub fn new_session_stamp() -> String {
    chrono::Local::now().format("%Y%m%d_%H%M%S").to_string()
}

/// 会话文件路径（按文件名标识，如时间戳）
pub fn session_path(id: &str) -> PathBuf {
    sessions_dir().join(format!("{id}.json"))
}

/// 列出所有会话文件名标识（去 .json，按名称排序）。
/// 新会话为时间戳（如 20260908_175624）；历史遗留的纯数字 ID 同样按文件名匹配。
pub fn list_sessions() -> Vec<String> {
    let dir = sessions_dir();
    if !dir.exists() {
        return Vec::new();
    }
    let mut ids = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
                if let Some(stem) = entry.path().file_stem().and_then(|s| s.to_str()) {
                    ids.push(stem.to_string());
                }
            }
        }
    }
    ids.sort();
    ids
}
