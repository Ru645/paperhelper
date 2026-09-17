//! 跨论文知识库（persisted in `.paperhelper/knowledge.json`）。
//!
//! 记录三件事：已读论文（Paper）、已学概念（Concept）、跨会话累计用量（SessionStats）。
//! - ingest 成功后在 `papers` 登记论文；ask 回答末尾的 `[[概念: xxx]]` 由 LLM 回报、
//!   Rust 解析后落入 `concepts`（name+paper_id 去重），definition 取该回答正文。
//! - ask/check 组织上下文时 `search(query)` 检索相关概念注入 prompt，实现跨论文关联：
//!   读到第二篇相关论文时，能引用第一篇学过的概念。
//! 去重策略：papers 按 id、concepts 按 (name, paper_id) 组合判重，避免重复堆积。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::paths;
use crate::session::SessionStats;

/// 一篇已读论文的登记信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paper {
    pub id: String,
    pub title: String,
    pub path: String,
    pub read_at: String,
    /// 材料类型：paper（论文，默认）/ note（笔记）/ lecture（讲义）。
    #[serde(default)]
    pub kind: String,
    /// 是否在列表中置顶。
    #[serde(default)]
    pub pinned: bool,
}

/// 一个已学概念：来源论文 + 精确定义 + 首次出现的追问位置（block_id）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Concept {
    pub name: String,
    pub definition: String,
    pub paper_id: String,
    pub paper_title: String,
    #[serde(default)]
    pub block_id: Option<String>,
    pub created_at: String,
    /// 是否在列表中置顶。
    #[serde(default)]
    pub pinned: bool,
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

/// 知识库合并结果（新增计数，供导入报告展示）。
#[derive(Debug, Clone, Copy, Default)]
pub struct MergeCounts {
    pub papers_added: usize,
    pub concepts_added: usize,
}

impl KnowledgeBase {
    /// 从磁盘加载；文件不存在时返回空库（首次使用）。
    pub fn load() -> Result<Self> {
        Self::load_from(&paths::data_dir())
    }

    /// 从指定数据目录加载（迁移/测试用）。
    pub fn load_from(dir: &Path) -> Result<Self> {
        let p = paths::knowledge_path_in(dir);
        if !p.exists() {
            return Ok(KnowledgeBase::default());
        }
        let s = fs::read_to_string(&p).context("读取 knowledge.json")?;
        let kb: KnowledgeBase = serde_json::from_str(&s).context("解析 knowledge.json")?;
        Ok(kb)
    }

    /// 全量写回磁盘（knowledge.json 体积小，无需增量）。
    pub fn save(&self) -> Result<()> {
        self.save_to(&paths::data_dir())
    }

    /// 全量写回指定数据目录（迁移/测试用）。
    pub fn save_to(&self, dir: &Path) -> Result<()> {
        if !dir.exists() {
            fs::create_dir_all(dir)?;
        }
        let s = serde_json::to_string_pretty(self)?;
        fs::write(paths::knowledge_path_in(dir), s)?;
        Ok(())
    }

    /// 合并另一份知识库（跨安装迁移导入）：论文按 id、概念按 (name, paper_id) 去重；
    /// 累计用量各字段取 max（重复导入不膨胀，迁到新机时效果 = 原机累计）。
    pub fn merge_from(&mut self, other: &KnowledgeBase) -> MergeCounts {
        let mut counts = MergeCounts::default();
        for p in &other.papers {
            if !self.papers.iter().any(|x| x.id == p.id) {
                self.papers.push(p.clone());
                counts.papers_added += 1;
            }
        }
        for c in &other.concepts {
            if !self
                .concepts
                .iter()
                .any(|x| x.name == c.name && x.paper_id == c.paper_id)
            {
                self.concepts.push(c.clone());
                counts.concepts_added += 1;
            }
        }
        self.stats.calls = self.stats.calls.max(other.stats.calls);
        self.stats.total_input = self.stats.total_input.max(other.stats.total_input);
        self.stats.total_output = self.stats.total_output.max(other.stats.total_output);
        self.stats.total_cost = self.stats.total_cost.max(other.stats.total_cost);
        counts
    }

    /// 登记一篇论文（按 id 判重，已读不重复入库）。
    pub fn add_paper(&mut self, paper: Paper) {
        if !self.papers.iter().any(|p| p.id == paper.id) {
            self.papers.push(paper);
        }
    }

    /// 登记一个概念（按 name+paper_id 判重：不同论文可分别记录同名概念）。
    pub fn add_concept(&mut self, c: Concept) {
        if !self
            .concepts
            .iter()
            .any(|x| x.name == c.name && x.paper_id == c.paper_id)
        {
            self.concepts.push(c);
        }
    }

    /// 检索与查询相关的已学概念（关键词重叠打分，跨论文关联）。
    ///
    /// 实现方式：把查询切成 ≥2 字符的单词，对每个概念计算"词项在
    /// 概念名+定义中出现的次数"之和作为分数，取分最高的 5 条。简单词袋匹配，
    /// 足够在追问注入场景用；返回带定义供 prompt 拼上下文。
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
