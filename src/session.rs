use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::conversation::Conversation;
use crate::notes::Note;

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
        };
        sess.save(&path).unwrap();

        let loaded = Session::load(&path).unwrap();
        assert!(loaded.notes.is_some());
        assert_eq!(loaded.conversation.nodes.len(), 1);
        assert_eq!(loaded.stats.calls, 1);
        assert_eq!(loaded.conversation.current.as_deref(), Some("n1"));
        let _ = std::fs::remove_file(&path);
    }
}
