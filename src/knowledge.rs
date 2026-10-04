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
    /// 常见别名 / 英文缩写（模型可标注，如「语义熵 | Semantic Entropy」）；参与检索匹配。
    #[serde(default)]
    pub aliases: Vec<String>,
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
            match self
                .concepts
                .iter_mut()
                .find(|x| x.name == c.name && x.paper_id == c.paper_id)
            {
                Some(existing) => {
                    for a in &c.aliases {
                        if !a.is_empty() && a != &existing.name && !existing.aliases.contains(a) {
                            existing.aliases.push(a.clone());
                        }
                    }
                }
                None => {
                    self.concepts.push(c.clone());
                    counts.concepts_added += 1;
                }
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
    /// 已存在同名同篇概念时，并集其别名（模型每次可能给出不同的别名）。
    pub fn add_concept(&mut self, mut c: Concept) {
        let mut seen = std::collections::HashSet::new();
        c.aliases
            .retain(|a| !a.is_empty() && a != &c.name && seen.insert(a.clone()));
        if let Some(existing) = self
            .concepts
            .iter_mut()
            .find(|x| x.name == c.name && x.paper_id == c.paper_id)
        {
            for a in c.aliases {
                if !existing.aliases.contains(&a) {
                    existing.aliases.push(a);
                }
            }
            return;
        }
        self.concepts.push(c);
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

    /// 检索与查询相关的已学概念（字段加权 + IDF 打分，跨论文关联）。
    ///
    /// 打分要点（避免「通用词命中定义」导致的误关联）：
    /// - 停用词（中英文高频虚词/2-gram）先丢弃；
    /// - 字段加权：概念名/别名 ×5、来源论文标题 ×2、定义 ×1；
    /// - **必须命中名称/别名/标题**才入选（只命中定义不算，防止无关概念被带出）；
    /// - IDF：命中超过半数概念的词权重归零（跨库通用词无区分度）；概念太少时跳过；
    /// - 置顶概念轻微加权；最多取 3 条。
    pub fn search(&self, query: &str) -> Vec<&Concept> {
        search_concepts(&self.concepts, query)
    }

    /// 提问中「被提及的已学论文」（论文关联增强用）：
    /// ①标题在问题里命中（归一化后子串匹配）；②上面检索到的相关概念的来源论文。
    /// 按「标题命中优先、其次概念得分」排序，去重。数量不设上限，由调用方决定如何使用。
    pub fn related_papers(&self, query: &str) -> Vec<Paper> {
        let q = normalize_for_match(query);
        let mut out: Vec<Paper> = Vec::new();
        for p in &self.papers {
            let t = normalize_for_match(&p.title);
            if t.chars().count() >= 4 && q.contains(&t) {
                push_unique_paper(&mut out, p);
            }
        }
        for c in self.search(query) {
            if let Some(p) = self.papers.iter().find(|p| p.id == c.paper_id) {
                push_unique_paper(&mut out, p);
            }
        }
        out
    }
}

fn push_unique_paper(out: &mut Vec<Paper>, p: &Paper) {
    if !out.iter().any(|x| x.id == p.id) {
        out.push(p.clone());
    }
}

/// 是否汉字（含扩展 A 区），用于中文 2-gram 分词与归一化匹配。
fn is_cjk(c: char) -> bool {
    ('\u{3400}'..='\u{9fff}').contains(&c)
}

/// 归一化：小写，只保留字母/数字/汉字（去掉空白、标点、书名号等），便于标题子串匹配。
fn normalize_for_match(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || is_cjk(*c))
        .collect()
}

/// 检索通用停用词（中英文高频虚词/疑问词/指代 2-gram）。
/// 命中这些词不参与打分——它们是「通用 2-gram 误关联」的主要来源。
const STOPWORDS: &[&str] = &[
    // 英文
    "the", "and", "for", "are", "was", "were", "with", "that", "this", "from", "have", "has",
    "had", "not", "but", "you", "your", "our", "their", "they", "them", "then", "than", "what",
    "which", "who", "whom", "why", "how", "when", "where", "will", "would", "can", "could",
    "should", "shall", "does", "did", "been", "being", "into", "about", "over", "under", "again",
    "more", "most", "some", "such", "only", "also", "very", "much", "many", "any", "all", "each",
    "both", "other", "its", "here", "there", "these", "those", "of", "to", "in", "is", "it", "on",
    "at", "as", "be", "by", "or", "an", "if", "so", "no", "up", "we", "us", "my", "me", "he",
    "she", "his", "her", "him",
    // 中文 2-gram（疑问 / 指代 / 虚词）
    "什么", "怎么", "怎样", "如何", "为何", "为什", "哪里", "哪个", "哪些", "这里", "这个",
    "那个", "这些", "那些", "一下", "一个", "意思", "指的", "的是", "是否", "可以", "以及",
    "还有", "并且", "所以", "因为", "但是", "如果", "就是", "我们", "你们", "他们", "它们",
    "自己", "一般", "通常", "例如", "比如", "关于", "对于", "中的", "了的", "请问", "解释",
    "说明", "知道", "觉得", "认为", "能够", "应该", "需要", "不用", "没有", "不是", "不能",
    "不会", "有点", "一点",
    // 中文单字（虚词/连接词：仅当被空格或英文隔成孤立单字时产生，按停用词丢弃，
    // 避免「jal 和 jalr 有什么区别」里的「和」误命中含「和」的概念名）
    "的", "了", "着", "过", "是", "在", "和", "与", "或", "及", "对", "把", "被", "给", "让",
    "等", "就", "也", "都", "很", "更", "最", "会", "能", "要", "可", "之", "其", "而", "则",
    "于", "以", "为", "从", "向", "到", "按", "这", "那", "我", "你", "他", "她", "它", "们",
    "吗", "呢", "吧", "啊", "请",
];

fn is_stopword(t: &str) -> bool {
    STOPWORDS.contains(&t)
}

/// 检索打分权重：名称/别名 ×5、来源论文标题 ×2、定义 ×1。
const W_NAME: f64 = 5.0;
const W_TITLE: f64 = 2.0;
const W_DEF: f64 = 1.0;
/// 单字段命中次数上限（同一词在定义里反复出现不应无限加分）。
const MAX_FIELD_HITS: f64 = 3.0;
/// 当选概念数 ≥ 此值时才启用 IDF（小库跳过，避免把唯一命中压没）。
const IDF_MIN_DOCS: usize = 4;
/// 入选的最低分（名称/标题必须命中，故有效命中通常 ≥2）。
const MIN_SCORE: f64 = 1.0;
/// 最多返回的概念条数。
const MAX_RESULTS: usize = 3;
/// 中文 n-gram 的最长长度（2-gram 太弱，3/4-gram 更具体、更能区分）。
const MAX_GRAM: usize = 4;

/// 单个检索词的权重：中文 n-gram 按其字符数（越长越具体）；英文整词按 4 计。
fn term_weight(t: &str) -> f64 {
    if t.chars().any(is_cjk) {
        t.chars().count() as f64
    } else {
        4.0
    }
}

/// 概念是否在任一字段包含检索词（判断文档频率 df 用）。
fn concept_contains(c: &Concept, t: &str) -> bool {
    c.name.to_lowercase().contains(t)
        || c.aliases.iter().any(|a| a.to_lowercase().contains(t))
        || c.paper_title.to_lowercase().contains(t)
        || c.definition.to_lowercase().contains(t)
}

/// 在给定概念集合上做字段加权检索（知识库与「锁外快照」共用同一实现）。
pub fn search_concepts<'a>(concepts: &'a [Concept], query: &str) -> Vec<&'a Concept> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let n = concepts.len();
    // IDF：统计每个词的文档频率；命中超过半数概念的通用词权重归零。
    let common: Vec<bool> = terms
        .iter()
        .map(|t| {
            if n < IDF_MIN_DOCS {
                return false;
            }
            let df = concepts.iter().filter(|c| concept_contains(c, t)).count();
            df * 2 > n
        })
        .collect();

    let mut scored: Vec<(f64, &Concept)> = Vec::new();
    for c in concepts {
        let name = c.name.to_lowercase();
        let aliases = c.aliases.join(" ").to_lowercase();
        let title = c.paper_title.to_lowercase();
        let def = c.definition.to_lowercase();
        let mut score = 0.0f64;
        let mut strong = 0.0f64;
        for (i, t) in terms.iter().enumerate() {
            if common[i] {
                continue;
            }
            let w = term_weight(t);
            let na = (name.matches(t.as_str()).count() + aliases.matches(t.as_str()).count())
                .min(MAX_FIELD_HITS as usize) as f64;
            let ti = title.matches(t.as_str()).count().min(MAX_FIELD_HITS as usize) as f64;
            let de = def.matches(t.as_str()).count().min(MAX_FIELD_HITS as usize) as f64;
            score += w * (W_NAME * na + W_TITLE * ti + W_DEF * de);
            // 名称/别名/标题命中才算「强命中」（只命中定义不足以入选）
            strong += w * (W_NAME * na + W_TITLE * ti);
        }
        if strong <= 0.0 || score < MIN_SCORE {
            continue;
        }
        if c.pinned {
            score *= 1.1;
        }
        scored.push((score, c));
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(MAX_RESULTS).map(|(_, c)| c).collect()
}

/// 查询分词：英文/数字词（≥2 字符，整词保留）+ 中文 2-4-gram（单汉字保留），
/// 去停用词、去重。用更长的 n-gram 是因为 2-gram 区分度太低、易误关联。
fn query_terms(query: &str) -> Vec<String> {
    fn flush_ascii(ascii: &mut String, terms: &mut Vec<String>) {
        if ascii.chars().count() >= 2 {
            terms.push(ascii.clone());
        }
        ascii.clear();
    }
    fn flush_cjk(cjk: &mut Vec<char>, terms: &mut Vec<String>) {
        let len = cjk.len();
        if len == 1 {
            terms.push(cjk[0].to_string());
        } else {
            for n in 2..=MAX_GRAM.min(len) {
                for w in cjk.windows(n) {
                    terms.push(w.iter().collect());
                }
            }
        }
        cjk.clear();
    }
    let mut terms: Vec<String> = Vec::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();
    for ch in query.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            if !cjk.is_empty() {
                flush_cjk(&mut cjk, &mut terms);
            }
            ascii.push(ch);
        } else if is_cjk(ch) {
            if !ascii.is_empty() {
                flush_ascii(&mut ascii, &mut terms);
            }
            cjk.push(ch);
        } else {
            if !ascii.is_empty() {
                flush_ascii(&mut ascii, &mut terms);
            }
            if !cjk.is_empty() {
                flush_cjk(&mut cjk, &mut terms);
            }
        }
    }
    if !ascii.is_empty() {
        flush_ascii(&mut ascii, &mut terms);
    }
    if !cjk.is_empty() {
        flush_cjk(&mut cjk, &mut terms);
    }
    terms.retain(|t| !is_stopword(t));
    terms.sort();
    terms.dedup();
    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn concept(name: &str, paper: &str) -> Concept {
        Concept {
            name: name.to_string(),
            aliases: Vec::new(),
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

    fn paper(id: &str, title: &str) -> Paper {
        Paper {
            id: id.to_string(),
            title: title.to_string(),
            path: format!("{id}.pdf"),
            read_at: String::new(),
            kind: "paper".into(),
            pinned: false,
        }
    }

    fn names(hits: &[&Concept]) -> Vec<String> {
        hits.iter().map(|c| c.name.clone()).collect()
    }

    fn concept_of(name: &str, def: &str, paper_id: &str, paper_title: &str) -> Concept {
        Concept {
            name: name.to_string(),
            aliases: Vec::new(),
            definition: def.to_string(),
            paper_id: paper_id.to_string(),
            paper_title: paper_title.to_string(),
            block_id: None,
            created_at: String::new(),
            pinned: false,
            graph_seen: false,
        }
    }

    /// 中文整句能切出 2-4-gram，英文词按 ≥2 字符整词保留，停用词被丢弃。
    #[test]
    fn query_terms_splits_cjk_and_ascii() {
        let terms = query_terms("Transformer 的注意力机制");
        assert!(terms.contains(&"transformer".to_string()), "{terms:?}");
        assert!(terms.contains(&"注意".to_string()), "应含 2-gram: {terms:?}");
        assert!(terms.contains(&"意力".to_string()), "{terms:?}");
        assert!(terms.contains(&"机制".to_string()), "{terms:?}");
        assert!(
            terms.contains(&"注意力机".to_string()),
            "应含 4-gram（2-gram 区分度太低）: {terms:?}"
        );
        assert!(!terms.contains(&"a".to_string()), "单字母英文词应丢弃");
        let stop = query_terms("这是什么东西");
        assert!(!stop.contains(&"什么".to_string()), "停用词应丢弃: {stop:?}");
    }

    /// 孤立单字虚词（如「和」）不应把含该字的概念误带出来。
    #[test]
    fn isolated_function_char_does_not_match() {
        let mut kb = KnowledgeBase::default();
        kb.add_concept(concept("生成问题的模型只会根据r_i和R生成问题", "p1"));
        let hits = kb.search("jal 和 jalr 有什么区别");
        assert!(hits.is_empty(), "单字虚词不应命中: {:?}", names(&hits));
    }

    /// 只命中「定义」的概念不入选（防止通用词把无关概念带出来）。
    #[test]
    fn search_ignores_definition_only_match() {
        let mut kb = KnowledgeBase::default();
        kb.add_concept(concept_of("自洽性检查", "用可靠性判断输出是否可信", "p1", "论文甲"));
        let hits = kb.search("可靠性怎么保证");
        assert!(hits.is_empty(), "仅定义命中不应入选: {:?}", names(&hits));
    }

    /// 别名参与「强命中」：问题里出现缩写也能命中概念。
    #[test]
    fn search_matches_alias() {
        let mut kb = KnowledgeBase::default();
        let mut c = concept_of("思维链", "让模型逐步推理的方法", "p1", "论文甲");
        c.aliases = vec!["CoT".into(), "Chain of Thought".into()];
        kb.add_concept(c);
        let hits = kb.search("CoT 和普通提示有什么区别");
        assert_eq!(hits.len(), 1, "别名命中应带出概念: {:?}", names(&hits));
        assert_eq!(hits[0].name, "思维链");
    }

    /// 复现误关联场景：纯指代性提问不应带出无关概念。
    #[test]
    fn search_generic_question_no_false_positive() {
        let mut kb = KnowledgeBase::default();
        kb.add_concept(concept_of("自检生成", "检测模型是否编造事实的框架", "p1", "论文甲"));
        kb.add_concept(concept_of("语义熵", "衡量语言模型输出不确定性的指标", "p2", "论文乙"));
        let hits = kb.search("这里的 CoT 指的是什么");
        assert!(hits.is_empty(), "不应误关联: {:?}", names(&hits));
    }

    /// 命中超过半数概念的通用词被 IDF 压掉（概念足够多时）。
    #[test]
    fn search_idf_downs_common_terms() {
        let mut kb = KnowledgeBase::default();
        for i in 0..5 {
            kb.add_concept(concept_of(&format!("模型{i}"), "一种模型的定义", "p1", "论文甲"));
        }
        let hits = kb.search("模型");
        assert!(hits.is_empty(), "通用词应被 IDF 压掉: {:?}", names(&hits));
        // 概念太少（<4）时不启用 IDF，仍可正常命中
        let mut tiny = KnowledgeBase::default();
        tiny.add_concept(concept_of("模型", "一种模型的定义", "p1", "论文甲"));
        assert_eq!(tiny.search("模型").len(), 1);
    }

    /// 最多返回 3 条。
    #[test]
    fn search_caps_results_at_three() {
        let mut kb = KnowledgeBase::default();
        for i in 0..5 {
            kb.add_concept(concept_of(&format!("注意力机制{i}"), "定义", "p1", "论文甲"));
        }
        let hits = kb.search("注意力机制");
        assert!(hits.len() <= 3, "最多 3 条，实得 {}", hits.len());
    }

    /// 同名同篇概念重复入库时并集别名（去重、剔除空值与名称本身）。
    #[test]
    fn add_concept_merges_aliases() {
        let mut kb = KnowledgeBase::default();
        let mk = |aliases: Vec<&str>| {
            let mut c = concept_of("思维链", "定义", "p1", "论文甲");
            c.aliases = aliases.into_iter().map(String::from).collect();
            c
        };
        kb.add_concept(mk(vec!["CoT", "思维链"]));
        kb.add_concept(mk(vec!["CoT", "", "Chain of Thought"]));
        assert_eq!(kb.concepts.len(), 1, "同名同篇应合并");
        assert_eq!(kb.concepts[0].aliases, vec!["CoT", "Chain of Thought"]);
    }

    /// 中文整句提问能命中中文概念名/定义（旧版按空格分词时命中不了）。
    #[test]
    fn search_matches_chinese_sentence() {
        let mut kb = KnowledgeBase::default();
        kb.add_concept(concept_of("注意力机制", "用权重加权求和", "p1", "深度学习"));
        let hits = kb.search("请解释一下注意力机制的原理");
        assert_eq!(hits.len(), 1, "应命中注意力机制");
        assert_eq!(hits[0].name, "注意力机制");
    }

    /// 匹配范围含来源论文标题，提到论文标题也能带出它的概念。
    #[test]
    fn search_haystack_includes_paper_title() {
        let mut kb = KnowledgeBase::default();
        kb.add_concept(concept_of("反向传播", "链式法则求梯度", "p1", "深度学习入门"));
        let hits = kb.search("《深度学习入门》讲了什么");
        assert_eq!(hits.len(), 1, "标题命中应带出概念");
    }

    /// related_papers：①标题命中；②相关概念的来源论文；去重；无命中返回空。
    #[test]
    fn related_papers_by_title_and_concept() {
        let mut kb = KnowledgeBase::default();
        kb.add_paper(paper("p1", "深度学习入门"));
        kb.add_paper(paper("p2", "计算机组成原理"));
        kb.add_concept(concept_of("神经网络", "多层感知机", "p1", "深度学习入门"));

        let by_title = kb.related_papers("和《深度学习入门》里的方法比呢");
        assert_eq!(by_title.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["p1"]);

        let by_concept = kb.related_papers("神经网络是怎么工作的");
        assert_eq!(by_concept.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["p1"]);

        assert!(kb.related_papers("完全无关的问题").is_empty());
    }
}
