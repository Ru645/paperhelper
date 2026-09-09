//! 会话状态（Session）与用量统计（SessionStats）。
//!
//! Session 是一次完整可持久化的工作现场：笔记树 + 对话树 + 用量统计 +
//! 当前论文引用 + 会话名/编号。序列化为 JSON 存档在 `.paperhelper/sessions/`。
//! 全部字段带 `#[serde(default)]`，保证老版本存档新增字段后仍可向后兼容加载。
//! `session_id`（首次保存时间戳）同时充当文件名、`-l` 展示键、`-s` 恢复参数；
//! `save()` 总是覆盖同一编号文件，实现"随时可恢复、对话位置不丢"。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::conversation::Conversation;
use crate::notes::Note;

/// 每次 API 调用的 token 与成本累计（会话级）。
/// 由 `ask/check/ingest/…` 在 LLM 返回后累加，退出时并入跨会话 knowledge.json。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionStats {
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub total_input: u64,
    #[serde(default)]
    pub total_output: u64,
    #[serde(default)]
    pub total_cost: f64,
}

impl SessionStats {
    pub fn total_tokens(&self) -> u64 {
        self.total_input + self.total_output
    }

    pub fn add(&mut self, input: u64, output: u64, cost: f64) {
        self.calls += 1;
        self.total_input += input;
        self.total_output += output;
        self.total_cost += cost;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Session {
    #[serde(default)]
    pub notes: Option<Note>,
    #[serde(default)]
    pub conversation: Conversation,
    #[serde(default)]
    pub stats: SessionStats,
    #[serde(default)]
    pub current_paper_id: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    /// 会话名（LLM 退出时生成，用于 -l 展示）。
    #[serde(default)]
    pub session_name: String,
    /// 会话编号（首次保存时生成的保存时间戳，形如 20260909_020452，此后不变；
    /// 同时也是会话文件名与 `-s` 的恢复参数）。
    #[serde(default)]
    pub session_id: String,
}

impl Session {
    pub fn touch(&mut self) {
        let now = chrono::Utc::now().to_rfc3339();
        if self.created_at.is_empty() {
            self.created_at = now.clone();
        }
        self.updated_at = now;
    }

    pub fn save(&mut self, path: &Path) -> Result<()> {
        self.touch();
        let s = serde_json::to_string_pretty(self)?;
        fs::write(path, s)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let s = fs::read_to_string(path)?;
        let mut sess: Session = serde_json::from_str(&s)?;
        if sess.created_at.is_empty() {
            sess.created_at = chrono::Utc::now().to_rfc3339();
        }
        Ok(sess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{Conversation, ConvNode};
    use crate::notes::parse_markdown_note;

    #[test]
    fn session_save_load_roundtrip() {
        let dir = std::env::temp_dir().join("paperhelper_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sess.json");

        let note = parse_markdown_note("# 论文A\n## 引言\n内容\n", "rawA");
        let mut conv = Conversation::default();
        conv.add_exchange(ConvNode {
            id: "n1".into(),
            parent: None,
            question: "什么是X?".into(),
            answer: "X是…".into(),
            block_id: None,
            explanation_id: None,
            input_tokens: 10,
            output_tokens: 20,
            cost: 0.001,
            created_at: "2026-01-01T00:00:00Z".into(),
            label: "什么是X?".into(),
        });
        conv.current = Some("n1".into());

        let mut sess = Session {
            notes: Some(note),
            conversation: conv,
            stats: SessionStats {
                calls: 1,
                total_input: 10,
                total_output: 20,
                total_cost: 0.001,
            },
            current_paper_id: Some("p1".into()),
            created_at: String::new(),
            updated_at: String::new(),
            session_name: String::new(),
            session_id: String::new(),
        };
        sess.session_id = "20260101_000000".into();
        sess.save(&path).unwrap();

        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded.session_id, "20260101_000000", "session_id 应随会话持久化");
        assert!(loaded.notes.is_some());
        assert_eq!(loaded.conversation.nodes.len(), 1);
        assert_eq!(loaded.stats.calls, 1);
        assert_eq!(loaded.conversation.current.as_deref(), Some("n1"));
        let _ = std::fs::remove_file(&path);
    }
}
