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
