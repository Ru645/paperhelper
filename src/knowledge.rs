use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;

use crate::paths;
use crate::session::SessionStats;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paper {
    pub id: String,
    pub title: String,
    pub path: String,
    pub read_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Concept {
    pub name: String,
    pub definition: String,
    pub paper_id: String,
    pub paper_title: String,
    #[serde(default)]
    pub block_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KnowledgeBase {
    #[serde(default)]
    pub papers: Vec<Paper>,
    #[serde(default)]
    pub concepts: Vec<Concept>,
    /// 跨会话累计用量与成本。
    #[serde(default)]
    pub stats: SessionStats,
}

impl KnowledgeBase {
    pub fn load() -> Result<Self> {
        let p = paths::knowledge_path();
        if !p.exists() {
            return Ok(KnowledgeBase::default());
        }
        let s = fs::read_to_string(&p).context("读取 knowledge.json")?;
        let kb: KnowledgeBase = serde_json::from_str(&s).context("解析 knowledge.json")?;
        Ok(kb)
    }

    pub fn save(&self) -> Result<()> {
        paths::ensure_data_dir()?;
        let s = serde_json::to_string_pretty(self)?;
        fs::write(paths::knowledge_path(), s)?;
        Ok(())
    }

    pub fn add_paper(&mut self, paper: Paper) {
        if !self.papers.iter().any(|p| p.id == paper.id) {
            self.papers.push(paper);
        }
    }

    pub fn add_concept(&mut self, c: Concept) {
        if !self
            .concepts
            .iter()
            .any(|x| x.name == c.name && x.paper_id == c.paper_id)
        {
            self.concepts.push(c);
        }
    }

    /// 检索与查询相关的已学概念（关键词重叠，跨论文关联）。
    pub fn search(&self, query: &str) -> Vec<&Concept> {
        let terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .filter(|t| t.chars().count() >= 2)
            .map(|t| t.to_string())
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, &Concept)> = Vec::new();
        for c in &self.concepts {
            let hay = format!("{} {}", c.name, c.definition).to_lowercase();
            let score = terms.iter().map(|t| hay.matches(t.as_str()).count()).sum::<usize>();
            if score > 0 {
                scored.push((score, c));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().take(5).map(|(_, c)| c).collect()
    }
}
