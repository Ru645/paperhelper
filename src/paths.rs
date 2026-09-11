//! 数据目录与文件路径管理。
//!
//! 所有运行时数据集中在当前工作目录的 `.paperhelper/` 下（已被 gitignore）：
//! - `config.toml`：用户配置（模型/价格/预算/补全预设）
//! - `knowledge.json`：跨论文知识库（论文清单+已学概念+累计用量）
//! - `sessions/<编号>.json`：会话存档，编号 = 首次保存的时间戳（如 20260909_021633），
//!   作为 `-s` 的恢复参数；list_sessions 返回排序后的编号列表

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

/// 置顶会话编号列表（旁路文件，避免改写大会话文件）。
pub fn pins_path() -> PathBuf {
    data_dir().join("pins.json")
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
