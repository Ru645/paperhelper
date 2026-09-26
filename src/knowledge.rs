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
    /// 是否已参与过「知识图谱」关系整理（懒惰更新：只处理未整理的新概念）。
    #[serde(default)]
    pub graph_seen: bool,
}

/// 概念间的一条关系（由 LLM 判断，`from`/`to` 为概念名）。
/// 方向语义随 `kind` 而定：前置/包含/应用有方向（from 是 to 的…），相关/对比无方向。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptRelation {
    pub from: String,
    pub to: String,
    /// 关系类型：前置 / 相关 / 对比 / 包含 / 应用。
    pub kind: String,
    /// 一句话说明。
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KnowledgeBase {
    #[serde(default)]
    pub papers: Vec<Paper>,
    #[serde(default)]
    pub concepts: Vec<Concept>,
    /// 概念间关系（知识图谱的边）。
    #[serde(default)]
    pub relations: Vec<ConceptRelation>,
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
        for r in &other.relations {
            if !self
                .relations
                .iter()
                .any(|x| x.from == r.from && x.to == r.to && x.kind == r.kind)
            {
                self.relations.push(r.clone());
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

    /// 概念名去重后的列表（保持首次出现顺序），作为知识图谱的节点集合。
    pub fn unique_concept_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for c in &self.concepts {
            if !names.iter().any(|n| n == &c.name) {
                names.push(c.name.clone());
            }
        }
        names
    }

    /// 尚未参与关系整理的概念名（懒惰更新：只处理这些「新概念」）。
    pub fn pending_graph_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for c in &self.concepts {
            if !c.graph_seen && !names.iter().any(|n| n == &c.name) {
                names.push(c.name.clone());
            }
        }
        names
    }

    /// 合并关系（按 from+to+kind 去重），返回新增条数；两端概念都必须存在。
    pub fn merge_relations(&mut self, rels: Vec<ConceptRelation>) -> usize {
        let known = self.unique_concept_names();
        let mut added = 0usize;
        for r in rels {
            if !known.iter().any(|n| n == &r.from) || !known.iter().any(|n| n == &r.to) {
                continue;
            }
            if r.from == r.to {
                continue;
            }
            if self
                .relations
                .iter()
                .any(|x| x.from == r.from && x.to == r.to && x.kind == r.kind)
            {
                continue;
            }
            self.relations.push(r);
            added += 1;
        }
        added
    }

    /// 把给定概念名标记为已整理（同名概念全部标记）。
    pub fn mark_graph_seen(&mut self, names: &[String]) {
        for c in &mut self.concepts {
            if names.iter().any(|n| n == &c.name) {
                c.graph_seen = true;
            }
        }
    }

    /// 清空关系并重置整理标记（用于「重建」）。
    pub fn reset_graph(&mut self) {
        self.relations.clear();
        for c in &mut self.concepts {
            c.graph_seen = false;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn concept(name: &str, paper: &str) -> Concept {
        Concept {
            name: name.to_string(),
            definition: format!("{name} 的定义"),
            paper_id: paper.to_string(),
            paper_title: paper.to_string(),
            block_id: None,
            created_at: String::new(),
            pinned: false,
            graph_seen: false,
        }
    }

    /// 图谱辅助：概念名去重、待整理标记、关系合并（去重/校验端点/拒绝自环）、重置。
    #[test]
    fn graph_helpers() {
        let mut kb = KnowledgeBase::default();
        kb.add_concept(concept("A", "p1"));
        kb.add_concept(concept("A", "p2"));
        kb.add_concept(concept("B", "p1"));
        assert_eq!(kb.unique_concept_names(), vec!["A", "B"]);
        assert_eq!(kb.pending_graph_names(), vec!["A", "B"]);

        let rel = |from: &str, to: &str, kind: &str| ConceptRelation {
            from: from.to_string(),
            to: to.to_string(),
            kind: kind.to_string(),
            note: String::new(),
        };
        let added = kb.merge_relations(vec![
            rel("A", "B", "前置"),
            rel("A", "B", "前置"),
            rel("A", "幽灵", "相关"),
            rel("A", "A", "相关"),
        ]);
        assert_eq!(added, 1, "重复/未知端点/自环都应被过滤");
        assert_eq!(kb.relations.len(), 1);

        kb.mark_graph_seen(&["A".to_string()]);
        assert_eq!(kb.pending_graph_names(), vec!["B"], "同名概念应一并标记");

        kb.reset_graph();
        assert!(kb.relations.is_empty());
        assert_eq!(kb.pending_graph_names(), vec!["A", "B"]);
    }
}
