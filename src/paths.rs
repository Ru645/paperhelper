//! 数据目录与文件路径管理。
//!
//! 所有运行时数据集中在数据目录下（已被 gitignore）：
//! - `config.toml`：用户配置（模型/价格/预算/补全预设）
//! - `knowledge.json`：跨论文知识库（论文清单+已学概念+累计用量）
//! - `sessions/<编号>.json`：会话存档，编号 = 首次保存的时间戳（如 20260909_021633），
//!   作为 `-s` 的恢复参数；list_sessions 返回排序后的编号列表
//!
//! 数据目录默认是当前工作目录的 `.paperhelper/`；打包版启动器会设置
//! `PAPERHELPER_DATA_DIR=%USERPROFILE%\PaperHelper\.paperhelper`（用户目录，换
//! 工作目录也不会"丢"笔记），环境变量支持绝对路径或 `~` 开头。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

const DATA_DIR: &str = ".paperhelper";

/// 数据目录：`PAPERHELPER_DATA_DIR`（非空时优先，支持 `~`）> 默认 `.paperhelper`。
pub fn data_dir() -> PathBuf {
    let raw = std::env::var("PAPERHELPER_DATA_DIR").ok();
    resolve_data_dir(raw.as_deref(), home_dir().as_deref())
}

/// 解析数据目录（纯函数便于单测）：空/未设置 → `.paperhelper`（相对 cwd）；
/// `~`/`~/x` → home 下；其余原样（相对/绝对均可）。
fn resolve_data_dir(raw: Option<&str>, home: Option<&Path>) -> PathBuf {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return PathBuf::from(DATA_DIR);
    };
    if raw == "~" {
        return home.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        if let Some(home) = home {
            if !rest.is_empty() {
                return home.join(rest);
            }
            return home.to_path_buf();
        }
    }
    PathBuf::from(raw)
}

/// 用户主目录（跨平台：HOME / USERPROFILE / HOMEDRIVE+HOMEPATH）。
fn home_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HOME").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if let Some(p) = std::env::var_os("USERPROFILE").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let drive = std::env::var_os("HOMEDRIVE")?;
    let path = std::env::var_os("HOMEPATH")?;
    if drive.is_empty() || path.is_empty() {
        return None;
    }
    let mut s = drive;
    s.push(path);
    Some(PathBuf::from(s))
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

/// 指定数据目录下的知识库文件（迁移/测试用）。
pub fn knowledge_path_in(dir: &Path) -> PathBuf {
    dir.join("knowledge.json")
}

/// 指定数据目录下的置顶文件（迁移/测试用）。
pub fn pins_path_in(dir: &Path) -> PathBuf {
    dir.join("pins.json")
}

/// 更新检查状态文件（`.paperhelper/update_state.json`）。
pub fn update_state_path() -> PathBuf {
    data_dir().join("update_state.json")
}

/// 上传文件目录（Web 端导入 PDF 等）。
pub fn uploads_dir() -> PathBuf {
    data_dir().join("uploads")
}

/// 确保上传目录存在。
pub fn ensure_uploads_dir() -> Result<()> {
    if !uploads_dir().exists() {
        fs::create_dir_all(uploads_dir())?;
    }
    Ok(())
}

/// 日志目录（Web/CLI 运行日志）。
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

/// 确保日志目录存在。
pub fn ensure_logs_dir() -> Result<()> {
    if !logs_dir().exists() {
        fs::create_dir_all(logs_dir())?;
    }
    Ok(())
}

/// 会话存档目录
pub fn sessions_dir() -> PathBuf {
    sessions_dir_in(&data_dir())
}

/// 指定数据目录下的会话存档目录（迁移/测试用）。
pub fn sessions_dir_in(dir: &Path) -> PathBuf {
    dir.join("sessions")
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
    session_path_in(&data_dir(), id)
}

/// 指定数据目录下的会话文件路径（迁移/测试用）。
pub fn session_path_in(dir: &Path, id: &str) -> PathBuf {
    sessions_dir_in(dir).join(format!("{id}.json"))
}

/// 列出所有会话文件名标识（去 .json，按名称排序）。
/// 新会话为时间戳（如 20260908_175624）；历史遗留的纯数字 ID 同样按文件名匹配。
pub fn list_sessions() -> Vec<String> {
    list_sessions_in(&data_dir())
}

/// 列出指定数据目录下的会话标识（迁移/测试用）。
pub fn list_sessions_in(dir: &Path) -> Vec<String> {
    let dir = sessions_dir_in(dir);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_defaults_to_cwd_relative() {
        assert_eq!(resolve_data_dir(None, Some(Path::new("/home/u"))), PathBuf::from(".paperhelper"));
        assert_eq!(resolve_data_dir(Some(""), Some(Path::new("/home/u"))), PathBuf::from(".paperhelper"));
        assert_eq!(resolve_data_dir(Some("  "), None), PathBuf::from(".paperhelper"));
    }

    #[test]
    fn data_dir_absolute_path_as_is() {
        assert_eq!(
            resolve_data_dir(Some("/tmp/ph-data"), None),
            PathBuf::from("/tmp/ph-data")
        );
        assert_eq!(
            resolve_data_dir(Some("D:\\PaperHelper\\.paperhelper"), None),
            PathBuf::from("D:\\PaperHelper\\.paperhelper")
        );
    }

    #[test]
    fn data_dir_expands_tilde() {
        let home = Path::new("/home/u");
        assert_eq!(resolve_data_dir(Some("~"), Some(home)), PathBuf::from("/home/u"));
        assert_eq!(
            resolve_data_dir(Some("~/.paperhelper"), Some(home)),
            PathBuf::from("/home/u/.paperhelper")
        );
        // 无 home 信息时保持原样，不 panic
        assert_eq!(resolve_data_dir(Some("~/x"), None), PathBuf::from("~/x"));
    }
}
