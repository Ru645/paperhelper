//! 跨安装迁移：会话 / 知识库 / 置顶 的导出包与导入合并。
//!
//! - 单会话导出 = 原始 Session JSON（CLI `load` 可直接读取）；
//! - 整库 / 多选导出 = 备份包 `SessionBundle`（JSON；有意不含 config.toml 里的 API Key）；
//! - 导入策略：编号冲突时自动分配新编号（保留两者）；完全相同的（编号 + 更新时间 +
//!   名称）视为已导入直接跳过；跨平台的 `export_path` 清理，避免目标机自动同步写入失败。
//!
//! 所有函数都接收显式数据目录参数，便于单测（不依赖全局 PAPERHELPER_DATA_DIR）。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::knowledge::{KnowledgeBase, MergeCounts};
use crate::{paths, session};

/// 备份包版本（导入时校验；高于当前版本要求先升级程序）。
pub const BUNDLE_VERSION: u32 = 1;

/// 备份包：会话 + 知识库 + 置顶（均可缺省，向后兼容）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionBundle {
    /// 包格式版本（导入识别与兼容校验用）。
    #[serde(default)]
    pub paperhelper_bundle: u32,
    #[serde(default)]
    pub exported_at: String,
    #[serde(default)]
    pub sessions: Vec<session::Session>,
    /// 整库导出时带知识库；多选导出为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<KnowledgeBase>,
    #[serde(default)]
    pub pins: Vec<String>,
}

/// 解析后的导入输入。
#[derive(Debug)]
pub enum ImportInput {
    Single(Box<session::Session>),
    Bundle(Box<SessionBundle>),
}

/// 解析导入 JSON：含 `sessions` 数组视为备份包，否则视为单个会话。
pub fn parse_import(bytes: &[u8]) -> Result<ImportInput> {
    let v: serde_json::Value = serde_json::from_slice(bytes)
        .context("不是有效的 JSON 文件（请选择 PaperHelper 导出的会话或备份 .json）")?;
    if v.get("sessions").map(|s| s.is_array()).unwrap_or(false) {
        let bundle: SessionBundle =
            serde_json::from_value(v).context("备份包解析失败（文件可能损坏或不是备份包）")?;
        match bundle.paperhelper_bundle {
            0 | BUNDLE_VERSION => {}
            v => bail!(
                "不支持的数据包版本 v{v}（当前支持 v{BUNDLE_VERSION}），请升级 PaperHelper 后再导入"
            ),
        }
        if bundle.sessions.is_empty() {
            bail!("备份包里没有会话");
        }
        return Ok(ImportInput::Bundle(Box::new(bundle)));
    }
    let sess: session::Session = serde_json::from_value(v)
        .context("不是有效的会话 JSON（请选择 PaperHelper 导出的 .json 文件）")?;
    if sess.notes.is_none() && sess.conversation.nodes.is_empty() && sess.annotations.is_empty() {
        bail!("文件里没有会话内容（笔记 / 对话 / 批注均为空）");
    }
    Ok(ImportInput::Single(Box::new(sess)))
}

/// 单个会话的导入结果。
#[derive(Debug, Clone, Serialize)]
pub struct ImportedSession {
    /// 来源编号（包内编号）。
    pub from: String,
    /// 落盘编号（冲突时为自动分配的新编号；跳过时为已存在的编号）。
    pub to: String,
    pub name: String,
    pub skipped: bool,
}

/// 导入汇总。
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportReport {
    pub sessions: Vec<ImportedSession>,
    pub papers_added: usize,
    pub concepts_added: usize,
    pub pins_added: usize,
}

/// 生成备份包；`ids = None` 表示全量（带知识库与全部置顶），
/// 否则只打包指定会话及这些会话的置顶标记。
pub fn export_bundle(dir: &Path, ids: Option<&[String]>) -> Result<SessionBundle> {
    let full = ids.is_none();
    let wanted: Vec<String> = match ids {
        Some(ids) => ids.to_vec(),
        None => paths::list_sessions_in(dir),
    };
    let mut sessions = Vec::with_capacity(wanted.len());
    for id in &wanted {
        let path = paths::session_path_in(dir, id);
        sessions.push(
            session::Session::load(&path).with_context(|| format!("读取会话 {id} 失败"))?,
        );
    }
    let pins = session::load_pins_from(dir);
    let pins = if full {
        pins
    } else {
        pins.into_iter().filter(|p| wanted.contains(p)).collect()
    };
    let knowledge = if full {
        Some(KnowledgeBase::load_from(dir)?)
    } else {
        None
    };
    Ok(SessionBundle {
        paperhelper_bundle: BUNDLE_VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        sessions,
        knowledge,
        pins,
    })
}

/// 把导入内容写入 `dir`：会话文件 + 合并知识库 + 合并置顶。
pub fn import_into(dir: &Path, input: ImportInput) -> Result<ImportReport> {
    fs::create_dir_all(paths::sessions_dir_in(dir)).context("创建会话目录失败")?;
    let mut used: HashSet<String> = paths::list_sessions_in(dir).into_iter().collect();
    let mut report = ImportReport::default();
    let mut id_map: HashMap<String, String> = HashMap::new();

    let (sessions, knowledge, pins) = match input {
        ImportInput::Single(s) => (vec![*s], None, Vec::new()),
        ImportInput::Bundle(b) => (b.sessions, b.knowledge, b.pins),
    };

    for mut sess in sessions {
        let from = if sess.session_id.trim().is_empty() {
            paths::new_session_stamp()
        } else {
            sess.session_id.trim().to_string()
        };
        let existing = paths::session_path_in(dir, &from);
        if existing.exists() {
            if let Some(meta) = session::read_meta(&existing) {
                // 幂等：同一文件重复导入不产生副本
                let same = !sess.updated_at.is_empty()
                    && meta.updated_at == sess.updated_at
                    && meta.session_name == sess.session_name;
                if same {
                    let to = from.clone();
                    report.sessions.push(ImportedSession {
                        from,
                        to,
                        name: sess.session_name,
                        skipped: true,
                    });
                    continue;
                }
            }
        }
        let to = unique_session_id(&mut used, &from);
        id_map.insert(from.clone(), to.clone());
        sess.session_id = to.clone();
        // 跨平台路径清理：目标机上不可用的导出路径置空（相对路径保留）
        sess.export_path = sess.export_path.filter(|p| usable_export_path(p));
        let json = serde_json::to_string_pretty(&sess).context("序列化会话失败")?;
        fs::write(paths::session_path_in(dir, &to), json)
            .with_context(|| format!("写入会话 {to} 失败"))?;
        report.sessions.push(ImportedSession {
            from,
            to,
            name: sess.session_name,
            skipped: false,
        });
    }

    if let Some(other) = knowledge {
        let mut kb = KnowledgeBase::load_from(dir)?;
        let MergeCounts {
            papers_added,
            concepts_added,
        } = kb.merge_from(&other);
        kb.save_to(dir)?;
        report.papers_added = papers_added;
        report.concepts_added = concepts_added;
    }

    let mut local_pins = session::load_pins_from(dir);
    for pin in pins {
        let mapped = id_map.get(&pin).cloned().unwrap_or(pin);
        // 指向未导入 / 本机不存在的会话则忽略，避免悬挂置顶
        if !paths::session_path_in(dir, &mapped).exists() {
            continue;
        }
        if !local_pins.contains(&mapped) {
            local_pins.push(mapped);
            report.pins_added += 1;
        }
    }
    if report.pins_added > 0 {
        session::save_pins_to(dir, &local_pins)?;
    }

    Ok(report)
}

/// 生成不与 `used` 冲突的编号：原编号可用则沿用，否则加 `_2`、`_3`… 后缀。
fn unique_session_id(used: &mut HashSet<String>, base: &str) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    let mut i = 2u32;
    loop {
        let cand = format!("{base}_{i}");
        if used.insert(cand.clone()) {
            return cand;
        }
        i += 1;
    }
}

/// 导入时判断 `export_path` 是否在目标机可用：
/// 绝对路径要求父目录存在；Unix `/…`、Windows `C:\…` 这类外来路径一律清理；
/// 相对路径保留（与原行为一致）。
fn usable_export_path(p: &str) -> bool {
    let p = p.trim();
    if p.is_empty() {
        return false;
    }
    let path = Path::new(p);
    if path.is_absolute() {
        return path.parent().map(|d| d.exists()).unwrap_or(false);
    }
    if p.starts_with('/') || p.starts_with("\\\\") {
        return false;
    }
    let b = p.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{ConvNode, Conversation};
    use crate::knowledge::{Concept, Paper};

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "paperhelper_transfer_{tag}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_session(id: &str, name: &str, updated_at: &str) -> session::Session {
        let mut conv = Conversation::default();
        conv.add_exchange(ConvNode {
            id: "n1".into(),
            parent: None,
            question: "Q".into(),
            quote: None,
            answer: "A".into(),
            block_id: None,
            explanation_id: None,
            input_tokens: 1,
            output_tokens: 2,
            cost: 0.0,
            created_at: updated_at.into(),
            label: String::new(),
        });
        session::Session {
            conversation: conv,
            session_name: name.into(),
            session_id: id.into(),
            created_at: updated_at.into(),
            updated_at: updated_at.into(),
            ..Default::default()
        }
    }

    fn write_session(dir: &Path, sess: &session::Session) {
        fs::create_dir_all(paths::sessions_dir_in(dir)).unwrap();
        let json = serde_json::to_string_pretty(sess).unwrap();
        fs::write(paths::session_path_in(dir, &sess.session_id), json).unwrap();
    }

    #[test]
    fn parse_import_distinguishes_bundle_and_single() {
        let sess = sample_session("20260101_000000", "会话A", "2026-01-01T00:00:00Z");
        let single = serde_json::to_vec(&sess).unwrap();
        let mut conv = Conversation::default();
        conv.add_exchange(ConvNode {
            id: "n2".into(),
            parent: None,
            question: "Q2".into(),
            quote: None,
            answer: "A2".into(),
            block_id: None,
            explanation_id: None,
            input_tokens: 0,
            output_tokens: 0,
            cost: 0.0,
            created_at: String::new(),
            label: String::new(),
        });
        let mut sess2 = session::Session::default();
        sess2.conversation = conv;
        let bundle = SessionBundle {
            paperhelper_bundle: BUNDLE_VERSION,
            exported_at: "2026-01-01T00:00:00Z".into(),
            sessions: vec![sess, sess2],
            knowledge: None,
            pins: vec![],
        };
        let bundle_json = serde_json::to_vec(&bundle).unwrap();

        assert!(matches!(parse_import(&single).unwrap(), ImportInput::Single(_)));
        assert!(matches!(parse_import(&bundle_json).unwrap(), ImportInput::Bundle(_)));

        // 不支持的版本给出可排查的中文错误
        let bad = br#"{"paperhelper_bundle":99,"sessions":[{"conversation":{},"notes":null}]}"#;
        let err = format!("{:#}", parse_import(bad).unwrap_err());
        assert!(err.contains("不支持的数据包版本"), "err={err}");

        // 空对象 / 无内容会话
        assert!(parse_import(b"{}").is_err());
        assert!(parse_import(b"{\"conversation\":{\"nodes\":[]}}").is_err());
    }

    #[test]
    fn export_bundle_full_and_partial() {
        let dir = tmp_dir("export");
        let a = sample_session("20260101_000001", "A", "2026-01-01T00:00:00Z");
        let b = sample_session("20260101_000002", "B", "2026-01-02T00:00:00Z");
        write_session(&dir, &a);
        write_session(&dir, &b);
        session::save_pins_to(&dir, &[a.session_id.clone()]).unwrap();

        let full = export_bundle(&dir, None).unwrap();
        assert_eq!(full.sessions.len(), 2);
        assert!(full.knowledge.is_some());
        assert_eq!(full.pins, vec![a.session_id.clone()]);

        let partial = export_bundle(&dir, Some(&[b.session_id.clone()])).unwrap();
        assert_eq!(partial.sessions.len(), 1);
        assert!(partial.knowledge.is_none());
        assert!(partial.pins.is_empty(), "只应带选中会话的置顶");
    }

    #[test]
    fn import_single_idempotent_and_conflict_renames() {
        let dir = tmp_dir("conflict");
        let a = sample_session("20260101_000001", "A", "2026-01-01T00:00:00Z");

        let r1 = import_into(&dir, ImportInput::Single(Box::new(a.clone()))).unwrap();
        assert_eq!(r1.sessions.len(), 1);
        assert_eq!(r1.sessions[0].to, "20260101_000001");
        assert!(!r1.sessions[0].skipped);
        assert!(paths::session_path_in(&dir, "20260101_000001").exists());

        // 同一份再导入 → 跳过
        let r2 = import_into(&dir, ImportInput::Single(Box::new(a.clone()))).unwrap();
        assert!(r2.sessions[0].skipped);

        // 同编号但内容已更新（updated_at 不同）→ 保留两者，自动改名
        let mut a2 = sample_session("20260101_000001", "A", "2026-02-01T00:00:00Z");
        a2.export_path = Some("/nonexistent_dir_xyz/note.md".into());
        let r3 = import_into(&dir, ImportInput::Single(Box::new(a2))).unwrap();
        assert!(!r3.sessions[0].skipped);
        assert_ne!(r3.sessions[0].to, "20260101_000001");
        assert!(paths::session_path_in(&dir, &r3.sessions[0].to).exists());

        let ids = paths::list_sessions_in(&dir);
        assert_eq!(ids.len(), 2);
        // 外来绝对路径被清理
        let imported = session::Session::load(&paths::session_path_in(&dir, &r3.sessions[0].to)).unwrap();
        assert!(imported.export_path.is_none());
    }

    #[test]
    fn import_bundle_merges_knowledge_and_pins() {
        let dir = tmp_dir("merge");
        // 本机已有：会话 L + 置顶 L + 知识库（paper1、本地概念）
        let l = sample_session("20260101_000010", "L", "2026-01-01T00:00:00Z");
        write_session(&dir, &l);
        session::save_pins_to(&dir, &[l.session_id.clone()]).unwrap();
        let mut local_kb = KnowledgeBase::default();
        local_kb.papers.push(Paper {
            id: "p1".into(),
            title: "论文1".into(),
            path: "/wsl/p1.pdf".into(),
            read_at: String::new(),
            kind: "paper".into(),
            pinned: false,
        });
        local_kb.stats.total_input = 10;
        local_kb.save_to(&dir).unwrap();

        // 备份包：会话 B（被置顶）+ paper1（重复）、paper2、概念1 + 用量更大
        let b = sample_session("20260101_000011", "B", "2026-01-02T00:00:00Z");
        let mut remote_kb = KnowledgeBase::default();
        remote_kb.papers.push(Paper {
            id: "p1".into(),
            title: "论文1".into(),
            path: "/wsl/p1.pdf".into(),
            read_at: String::new(),
            kind: "paper".into(),
            pinned: false,
        });
        remote_kb.papers.push(Paper {
            id: "p2".into(),
            title: "论文2".into(),
            path: "/wsl/p2.pdf".into(),
            read_at: String::new(),
            kind: "paper".into(),
            pinned: false,
        });
        remote_kb.concepts.push(Concept {
            name: "概念X".into(),
            definition: "定义".into(),
            paper_id: "p2".into(),
            paper_title: "论文2".into(),
            block_id: None,
            created_at: String::new(),
            pinned: false,
            graph_seen: false,
        });
        remote_kb.stats.total_input = 100;
        remote_kb.stats.calls = 5;
        let bundle = SessionBundle {
            paperhelper_bundle: BUNDLE_VERSION,
            exported_at: String::new(),
            sessions: vec![b],
            knowledge: Some(remote_kb),
            pins: vec!["20260101_000011".into()],
        };

        let r = import_into(&dir, ImportInput::Bundle(Box::new(bundle))).unwrap();
        assert_eq!(r.papers_added, 1, "p1 去重，p2 新增");
        assert_eq!(r.concepts_added, 1);
        assert_eq!(r.pins_added, 1);

        let kb = KnowledgeBase::load_from(&dir).unwrap();
        assert_eq!(kb.papers.len(), 2);
        assert_eq!(kb.concepts.len(), 1);
        assert_eq!(kb.stats.total_input, 100, "用量取 max（重复导入不膨胀）");
        assert_eq!(kb.stats.calls, 5);

        let pins = session::load_pins_from(&dir);
        assert_eq!(pins.len(), 2);
        assert!(pins.contains(&"20260101_000011".to_string()));
    }

    #[test]
    fn import_bundle_remaps_pins_when_conflict() {
        let dir = tmp_dir("pinmap");
        let a = sample_session("20260101_000020", "A", "2026-01-01T00:00:00Z");
        write_session(&dir, &a);
        let mut incoming = sample_session("20260101_000020", "A", "2026-03-01T00:00:00Z");
        incoming.export_path = None;
        let bundle = SessionBundle {
            paperhelper_bundle: BUNDLE_VERSION,
            exported_at: String::new(),
            sessions: vec![incoming],
            knowledge: None,
            pins: vec!["20260101_000020".into()],
        };
        let r = import_into(&dir, ImportInput::Bundle(Box::new(bundle))).unwrap();
        let new_id = r.sessions[0].to.clone();
        assert_ne!(new_id, "20260101_000020");
        let pins = session::load_pins_from(&dir);
        assert!(pins.contains(&new_id), "置顶应映射到新编号: {pins:?}");
        assert!(!pins.contains(&"20260101_000020".to_string()));
    }

    #[test]
    fn usable_export_path_rules() {
        let dir = tmp_dir("paths");
        let ok = dir.join("note.md");
        assert!(usable_export_path(ok.to_str().unwrap()));
        assert!(usable_export_path("note.md"));
        assert!(!usable_export_path(""));
        assert!(!usable_export_path("/nonexistent_dir_xyz/note.md"));
        assert!(!usable_export_path("C:\\Users\\x\\note.md"));
    }
}
