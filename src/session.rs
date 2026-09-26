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

use crate::conversation::{strip_legacy_check_label, Conversation};
use crate::notes::Note;
use crate::paths;

/// 每次 API 调用的 token 与成本累计（会话级）。
/// 由 `ask/ingest/…` 在 LLM 返回后累加，退出时并入跨会话 knowledge.json。
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
    /// 批注列表（Web 端在笔记选中文字提问产生；每条批注关联一段会话子树）。
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    /// 该会话的笔记导出文件路径（ingest/ask 自动同步用；随会话持久化）。
    #[serde(default)]
    pub export_path: Option<String>,
    /// 仅阅读会话：只挂 PDF 原件、未生成笔记（不自动导出、默认进「原文」）。
    #[serde(default)]
    pub read_only: bool,
}

/// 一条批注：笔记里的一段引用文字 + 其对话线程（以 `root_node_id` 为根的会话子树）。
/// 一条批注可拥有多个对话根节点（森林）：对同一段选中文字提出多个互不相关的独立问题时，
/// 每次都新增一个根节点。`root_node_id` 保留为「首根」以兼容旧数据。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Annotation {
    pub id: String,
    /// 引用文字所在的笔记块 id（回答批注 / PDF 批注为空）。
    pub block_id: String,
    /// 选中的引用文字（可见文本，用于高亮匹配）。
    pub quote: String,
    /// 交给 LLM 的上下文（把选中公式还原成 TeX，如 `$N$`；旧数据为空则回退 `quote`）。
    #[serde(default)]
    pub quote_tex: Option<String>,
    /// 回答批注的锚点：引用文字所在对话节点（笔记批注为 None）。
    #[serde(default)]
    pub node_id: Option<String>,
    /// 该批注对话线程的**首个**根节点 id（旧字段，兼容旧数据；等于 `root_node_ids[0]`）。
    pub root_node_id: String,
    /// 该批注的全部对话根节点 id（森林）。旧数据为空，加载时由 `root_node_id` 回填。
    #[serde(default)]
    pub root_node_ids: Vec<String>,
    #[serde(default)]
    pub created_at: String,
    /// PDF 批注：原件页码（从 1 开始；非 PDF 批注为 None）。
    #[serde(default)]
    pub page: Option<u32>,
    /// PDF 批注：页面内归一化矩形 `[x0,y0,x1,y1]`（相对页宽/页高，0..1），
    /// 支持多行选区（多条）。非 PDF 批注为空。
    #[serde(default)]
    pub rects: Vec<[f32; 4]>,
    /// PDF 批注类型：`text`（选中文本）/ `image`（图片或框选区域）/ `page`（整页）；
    /// 非 PDF 批注为 None。
    #[serde(default)]
    pub kind: Option<String>,
}

impl Annotation {
    /// 该批注的全部对话根节点；旧数据 `root_node_ids` 为空时回退首根字段。
    pub fn roots(&self) -> Vec<&str> {
        if !self.root_node_ids.is_empty() {
            self.root_node_ids.iter().map(|s| s.as_str()).collect()
        } else if !self.root_node_id.is_empty() {
            vec![self.root_node_id.as_str()]
        } else {
            Vec::new()
        }
    }

    /// 把旧的单根字段迁移进 `root_node_ids`（旧数据只填了 `root_node_id`）。
    fn sync_roots_from_legacy(&mut self) {
        if self.root_node_ids.is_empty() && !self.root_node_id.is_empty() {
            self.root_node_ids.push(self.root_node_id.clone());
        }
    }

    /// 追加一个对话根节点，保持 `root_node_id`（首根）与 `root_node_ids` 同步。
    pub fn push_root(&mut self, id: &str) {
        if id.is_empty() {
            return;
        }
        self.sync_roots_from_legacy();
        if self.root_node_ids.iter().any(|r| r == id) {
            return;
        }
        self.root_node_ids.push(id.to_string());
        if self.root_node_id.is_empty() {
            self.root_node_id = id.to_string();
        }
    }

    /// 从 `root_node_ids` 中移除某根，并同步首根字段（供删除子树后清理）。
    pub fn remove_root(&mut self, id: &str) {
        self.sync_roots_from_legacy();
        self.root_node_ids.retain(|r| r != id);
        if self.root_node_id == id || self.root_node_id.is_empty() {
            self.root_node_id = self.root_node_ids.first().cloned().unwrap_or_default();
        }
    }
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
        self.write(path)
    }

    /// 非内容变更的落盘（切走 / 退出 / 改名等）：不改写已有的 `updated_at`，
    /// 让会话列表仍按「最后一次内容修改」排序；仅当字段为空（新会话）时补时间。
    pub fn save_preserving_time(&mut self, path: &Path) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        if self.created_at.is_empty() {
            self.created_at = now.clone();
        }
        if self.updated_at.is_empty() {
            self.updated_at = now;
        }
        self.write(path)
    }

    fn write(&self, path: &Path) -> Result<()> {
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
        sess.normalize_legacy();
        Ok(sess)
    }

    /// 旧数据迁移：移除已废弃的「核对(check)」标记，节点保留为普通问答。
    fn normalize_legacy(&mut self) {
        for node in &mut self.conversation.nodes {
            let stripped = strip_legacy_check_label(&node.label).to_string();
            if stripped != node.label {
                node.label = stripped;
            }
        }
        // 旧批注只有单根 `root_node_id`：回填多根字段，保证森林逻辑统一。
        for ann in &mut self.annotations {
            if ann.root_node_ids.is_empty() && !ann.root_node_id.is_empty() {
                ann.root_node_ids.push(ann.root_node_id.clone());
            }
        }
    }
}

/// 会话元信息（轻量解析，忽略 raw_text 等大字段，供列表/扫描使用）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionMeta {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub session_name: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub current_paper_id: Option<String>,
}

/// 读取会话文件里的元信息（serde 跳过未知的大字段如 raw_text）。
pub fn read_meta(path: &Path) -> Option<SessionMeta> {
    let s = fs::read_to_string(path).ok()?;
    serde_json::from_str::<SessionMeta>(&s).ok()
}

/// 扫描用的精简结构：会话元信息 + 对话节点的问答。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct NodeLite {
    #[serde(default)]
    id: String,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    label: String,
    #[serde(default)]
    question: String,
    #[serde(default)]
    answer: String,
    #[serde(default)]
    explanation_id: Option<String>,
}

/// 批注的轻量视图（只取定位所需字段）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AnnLite {
    #[serde(default)]
    id: String,
    #[serde(default)]
    block_id: String,
    #[serde(default)]
    root_node_id: String,
    #[serde(default)]
    root_node_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConvLite {
    #[serde(default)]
    nodes: Vec<NodeLite>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct NoteLite {
    #[serde(default)]
    title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SessionScan {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    session_name: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    current_paper_id: Option<String>,
    #[serde(default)]
    notes: Option<NoteLite>,
    #[serde(default)]
    conversation: ConvLite,
    #[serde(default)]
    annotations: Vec<AnnLite>,
}

impl SessionScan {
    fn meta(&self) -> SessionMeta {
        SessionMeta {
            session_id: self.session_id.clone(),
            session_name: self.session_name.clone(),
            updated_at: self.updated_at.clone(),
            current_paper_id: self.current_paper_id.clone(),
        }
    }

    /// 若该对话节点属于某条批注线程（沿 parent 上溯到根，与批注的任一 `root_node_id` 比对），
    /// 返回 `(批注 id, 所属块 id)`。
    fn annotation_of(&self, node: &NodeLite) -> Option<(String, String)> {
        let mut root = node.id.clone();
        let mut cur = node.parent.clone();
        let mut hops = 0;
        while let Some(pid) = cur {
            let Some(p) = self.conversation.nodes.iter().find(|n| n.id == pid) else {
                break;
            };
            root = p.id.clone();
            cur = p.parent.clone();
            hops += 1;
            if hops > 1000 {
                break;
            }
        }
        self.annotations
            .iter()
            .find(|a| {
                !a.id.is_empty()
                    && (a.root_node_id == root || a.root_node_ids.iter().any(|r| r == &root))
            })
            .map(|a| (a.id.clone(), a.block_id.clone()))
    }
}

fn scan(path: &Path) -> Option<SessionScan> {
    let s = fs::read_to_string(path).ok()?;
    let mut sc = serde_json::from_str::<SessionScan>(&s).ok()?;
    for node in &mut sc.conversation.nodes {
        let stripped = strip_legacy_check_label(&node.label).to_string();
        if stripped != node.label {
            node.label = stripped;
        }
    }
    Some(sc)
}

/// 找包含指定论文的会话，取 `updated_at` 最新者。
/// 优先按 `current_paper_id` 精确匹配；若论文被重新导入导致 id 变化，
/// 则回退按笔记标题（忽略大小写）匹配。返回 `(会话编号, 元信息)`。
pub fn find_session_by_paper(paper_id: &str, title: &str) -> Option<(String, SessionMeta)> {
    let mut best: Option<(u8, String, SessionMeta)> = None;
    for id in paths::list_sessions() {
        let Some(sc) = scan(&paths::session_path(&id)) else {
            continue;
        };
        let by_id = !paper_id.is_empty() && sc.current_paper_id.as_deref() == Some(paper_id);
        let by_title = !title.is_empty()
            && sc
                .notes
                .as_ref()
                .is_some_and(|n| n.title.eq_ignore_ascii_case(title));
        if !by_id && !by_title {
            continue;
        }
        let prio = if by_id { 0u8 } else { 1u8 };
        let meta = sc.meta();
        let better = match &best {
            None => true,
            Some((bp, _, b)) => prio < *bp || (prio == *bp && meta.updated_at > b.updated_at),
        };
        if better {
            best = Some((prio, id, meta));
        }
    }
    best.map(|(_, id, meta)| (id, meta))
}

/// 找某个概念（ask 节点 `label == name`）的问答及所在会话。
/// 优先匹配概念来源论文 `prefer_paper`，否则取 `updated_at` 最新者。
/// 返回 `(元信息, question, answer, explanation_id, annotation)`；
/// `annotation` 为 `Some((批注 id, 块 id))` 表示该问答属于批注线程（正文无 `expl-` 锚点）。
pub fn find_concept_qa(
    name: &str,
    prefer_paper: Option<&str>,
) -> Option<(SessionMeta, String, String, Option<String>, Option<(String, String)>)> {
    type Hit = (SessionMeta, String, String, Option<String>, Option<(String, String)>);
    let mut fallback: Option<Hit> = None;
    let mut preferred: Option<Hit> = None;
    for id in paths::list_sessions() {
        let Some(sc) = scan(&paths::session_path(&id)) else {
            continue;
        };
        for n in &sc.conversation.nodes {
            if n.label != name {
                continue;
            }
            let hit: Hit = (
                sc.meta(),
                n.question.clone(),
                n.answer.clone(),
                n.explanation_id.clone(),
                sc.annotation_of(n),
            );
            let is_pref = prefer_paper.is_some() && sc.current_paper_id.as_deref() == prefer_paper;
            if is_pref {
                let better = preferred
                    .as_ref()
                    .map_or(true, |p| hit.0.updated_at > p.0.updated_at);
                if better {
                    preferred = Some(hit);
                }
            } else {
                let better = fallback
                    .as_ref()
                    .map_or(true, |p| hit.0.updated_at > p.0.updated_at);
                if better {
                    fallback = Some(hit);
                }
            }
        }
    }
    preferred.or(fallback)
}

/// 读取置顶会话编号列表（`.paperhelper/pins.json`），不存在则空。
pub fn load_pins() -> Vec<String> {
    load_pins_from(&paths::data_dir())
}

/// 读取指定数据目录的置顶列表（迁移/测试用）。
pub fn load_pins_from(dir: &Path) -> Vec<String> {
    match fs::read_to_string(paths::pins_path_in(dir)) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// 写回置顶会话编号列表。
pub fn save_pins(ids: &[String]) -> Result<()> {
    save_pins_to(&paths::data_dir(), ids)
}

/// 写回指定数据目录的置顶列表（迁移/测试用）。
pub fn save_pins_to(dir: &Path, ids: &[String]) -> Result<()> {
    if !dir.exists() {
        fs::create_dir_all(dir)?;
    }
    let s = serde_json::to_string_pretty(ids)?;
    fs::write(paths::pins_path_in(dir), s)?;
    Ok(())
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
            quote: None,
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
            annotations: Vec::new(),
            export_path: None,
            read_only: true,
        };
        sess.session_id = "20260101_000000".into();
        sess.save(&path).unwrap();

        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded.session_id, "20260101_000000", "session_id 应随会话持久化");
        assert!(loaded.notes.is_some());
        assert!(loaded.read_only, "read_only 应随会话持久化");
        assert_eq!(loaded.conversation.nodes.len(), 1);
        assert_eq!(loaded.stats.calls, 1);
        assert_eq!(loaded.conversation.current.as_deref(), Some("n1"));
        let _ = std::fs::remove_file(&path);
    }

    /// 非内容落盘（切换 / 退出 / 改名）不改写 `updated_at`；内容落盘 `save()`
    /// 与首次落盘仍会写入时间，保证会话列表按内容修改时间排序。
    #[test]
    fn save_preserving_time_keeps_updated_at() {
        let dir = std::env::temp_dir().join("paperhelper_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("preserve_time.json");

        let mut sess = Session {
            created_at: "2019-01-01T00:00:00Z".into(),
            updated_at: "2020-01-01T00:00:00Z".into(),
            ..Default::default()
        };
        sess.save_preserving_time(&path).unwrap();
        let loaded = Session::load(&path).unwrap();
        assert_eq!(
            loaded.updated_at, "2020-01-01T00:00:00Z",
            "非内容落盘不应改写 updated_at"
        );
        assert_eq!(loaded.created_at, "2019-01-01T00:00:00Z");

        let mut fresh = Session::default();
        fresh.save_preserving_time(&path).unwrap();
        assert!(!fresh.updated_at.is_empty(), "新会话首次落盘应补上时间");

        let mut bump = Session {
            updated_at: "2020-01-01T00:00:00Z".into(),
            ..Default::default()
        };
        bump.save(&path).unwrap();
        assert_ne!(bump.updated_at, "2020-01-01T00:00:00Z", "save() 应刷新 updated_at");
        let _ = std::fs::remove_file(&path);
    }

    /// 旧会话里的 `[核对] ` 标签应在加载时剥离，节点保留为普通问答。
    #[test]
    fn load_strips_legacy_check_label() {
        let dir = std::env::temp_dir().join("paperhelper_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("legacy_check.json");
        let old = r#"{"conversation":{"nodes":[
            {"id":"n1","question":"q1","answer":"a1","created_at":"t","label":"[核对] 余弦相似度"},
            {"id":"n2","parent":"n1","question":"q2","answer":"a2","created_at":"t","label":"BERT"}
        ]}}"#;
        std::fs::write(&path, old).unwrap();
        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded.conversation.nodes[0].label, "余弦相似度");
        assert_eq!(loaded.conversation.nodes[1].label, "BERT", "非核对标签不应改动");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn annotation_backward_compatible() {
        // 旧会话 JSON 没有 quote_tex / node_id 字段，应能正常反序列化
        let old = r#"{"id":"a1","block_id":"b1","quote":"x","root_node_id":"n1","created_at":""}"#;
        let ann: Annotation = serde_json::from_str(old).unwrap();
        assert_eq!(ann.quote, "x");
        assert!(ann.quote_tex.is_none(), "旧数据 quote_tex 应为 None");
        assert!(ann.node_id.is_none(), "旧数据 node_id 应为 None");
        assert!(ann.page.is_none(), "旧数据 page 应为 None");
        assert!(ann.rects.is_empty(), "旧数据 rects 应为空");
        assert!(ann.kind.is_none(), "旧数据 kind 应为 None");
        assert!(ann.root_node_ids.is_empty(), "旧数据 root_node_ids 应为空");
        assert_eq!(ann.roots(), vec!["n1"], "旧数据 roots() 应回退到 root_node_id");

        // 新字段可正常往返
        let ann = Annotation {
            id: "a2".into(),
            block_id: String::new(),
            quote: "y".into(),
            quote_tex: Some("$y$".into()),
            node_id: Some("n9".into()),
            root_node_id: "n10".into(),
            created_at: String::new(),
            ..Default::default()
        };
        let back: Annotation = serde_json::from_str(&serde_json::to_string(&ann).unwrap()).unwrap();
        assert_eq!(back.quote_tex.as_deref(), Some("$y$"));
        assert_eq!(back.node_id.as_deref(), Some("n9"));

        // PDF 批注字段可正常往返
        let pdf = Annotation {
            id: "a3".into(),
            quote: "第 3 页正文".into(),
            root_node_id: "n11".into(),
            page: Some(3),
            rects: vec![[0.1, 0.2, 0.5, 0.25], [0.1, 0.3, 0.6, 0.35]],
            kind: Some("text".into()),
            ..Default::default()
        };
        let back: Annotation = serde_json::from_str(&serde_json::to_string(&pdf).unwrap()).unwrap();
        assert_eq!(back.page, Some(3));
        assert_eq!(back.rects.len(), 2);
        assert_eq!(back.kind.as_deref(), Some("text"));
    }

    /// 多根批注：push_root/remove_root 与 roots() 行为，且旧数据加载时回填根列表。
    #[test]
    fn annotation_multi_root_and_legacy_backfill() {
        let mut ann = Annotation {
            id: "a1".into(),
            root_node_id: "r1".into(),
            ..Default::default()
        };
        assert_eq!(ann.roots(), vec!["r1"]);
        ann.push_root("r2");
        ann.push_root("r2"); // 去重
        assert_eq!(ann.roots(), vec!["r1", "r2"]);
        assert_eq!(ann.root_node_id, "r1", "首根字段应保持");
        ann.remove_root("r1");
        assert_eq!(ann.roots(), vec!["r2"]);
        assert_eq!(ann.root_node_id, "r2", "首根被删后应顺延");
        ann.remove_root("r2");
        assert!(ann.roots().is_empty());

        // 旧会话加载：root_node_id 回填进 root_node_ids
        let dir = std::env::temp_dir().join("paperhelper_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("legacy_multiroot.json");
        let old = r#"{"session_id":"20250101_000000","annotations":[
            {"id":"a1","block_id":"b1","quote":"x","root_node_id":"n1"}
        ]}"#;
        std::fs::write(&path, old).unwrap();
        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded.annotations[0].root_node_ids, vec!["n1"]);
        assert_eq!(loaded.annotations[0].roots(), vec!["n1"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_only_defaults_false_for_old_session() {
        // 旧存档没有 read_only 字段，加载后应为 false（向后兼容）
        let old = r#"{"session_id":"20250101_000000","notes":null}"#;
        let sess: Session = serde_json::from_str(old).unwrap();
        assert!(!sess.read_only, "旧会话 read_only 默认应为 false");
    }
}
