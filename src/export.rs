//! 笔记导出渲染：Markdown / 思维导图(markmap) / 自包含 HTML。
//!
//! 三种格式共享同一棵笔记树与对话树：
//! - Markdown：递归输出 Section（带 Rust 编号）、Paragraph、Explanation 追问块。
//!   追问渲染规则见 `render_explanation`（父级空引用连接子追问、兄弟用裸空行
//!   分隔、sum 后子树进 <details> 且总结在外），保证导出/浏览器折叠一致。
//! - 思维导图：缩进 markdown，追问以 💬 前缀展示（markmap 兼容）。
//! - HTML：单文件。把 Markdown 以 JS 字符串嵌入，运行时由 marked 渲染、
//!   KaTeX 渲染公式；左侧为对话树 <ul>（当前节点高亮 + token 显示）。
//!   CDN 双源（jsdelivr → npmmirror）自动 fallback，全挂则降级纯文本。
//!   导出前先 `convert_inline_math_delims` 统一公式定界符，规避 marked 转义。

use std::collections::HashSet;

use crate::conversation::Conversation;
use crate::notes::{Block, BlockKind, Explanation, Note};
use crate::session::Annotation;

/// 整棵笔记渲染为 Markdown（根标题 + 逐块递归，追问挂在所属块下）。
pub fn to_markdown(note: &Note) -> String {
    to_markdown_with(note, false, &HashSet::new())
}

/// 渲染 Markdown。
/// - `anchors=true`：给每个块注入 `<a id="blk-…">`、每条追问注入 `<a id="expl-…">`，
///   供 Web 端笔记 iframe 内定位（Markdown 导出保持干净，不注入）。
/// - `hidden`：要跳过的解释 id 集合（批注线程的问答不在正文内联显示，只在弹窗里看）。
fn to_markdown_with(note: &Note, anchors: bool, hidden: &HashSet<String>) -> String {
    let mut s = format!("# {}\n\n", note.title);
    walk_md(&note.blocks, 0, anchors, hidden, &mut s);
    s
}

fn walk_md(blocks: &[Block], depth: usize, anchors: bool, hidden: &HashSet<String>, s: &mut String) {
    for b in blocks {
        if anchors {
            s.push_str(&format!("<a id=\"blk-{}\"></a>\n", b.id));
        }
        match b.kind {
            BlockKind::Section => {
                let level = (depth + 2).min(6);
                let title = if b.number.is_empty() {
                    b.text.clone()
                } else {
                    format!("{} {}", b.number, b.text)
                };
                s.push_str(&format!("{} {}\n\n", "#".repeat(level), title));
            }
            BlockKind::Paragraph => {
                s.push_str(&format!("{}\n\n", b.text));
            }
            BlockKind::Formula => {
                s.push_str(&format!("$$\n{}\n$$\n\n", b.text));
            }
        }
        for (i, e) in b.explanations.iter().enumerate() {
            if hidden.contains(&e.id) {
                continue; // 批注线程的问答：正文不内联显示
            }
            if i > 0 {
                s.push('\n'); // 顶层追问之间空行分隔（上块尾已有 \n）
            }
            render_explanation(e, 1, anchors, hidden, s);
            s.push('\n');
        }
        walk_md(&b.children, depth + 1, anchors, hidden, s);
    }
}

/// 递归渲染追问，depth=1 为顶层（`> `），depth=2 为子追问（`> > `），以此类推。
/// 排版规则（体现父子关系）：
/// - 父亲与它的第一个儿子之间用「父级空引用行」（如 `>`）连接，保持嵌套结构连续；
/// - 兄弟儿子之间用裸空行断开（分支分隔），`> >` 前缀仍保证渲染为嵌套层级；
/// - 多行 answer 每行都加当前层级前缀，避免引用块断裂。
/// - collapsed（sum 折叠）：整个追问子树包进 <details>，总结显示在折叠块外。
/// - anchors=true 时，在追问最前面注入 `<a id="expl-<id>">` 供页面内跳转。
/// 块与块之间的顶层分隔由调用者处理。
fn render_explanation(
    e: &Explanation,
    depth: usize,
    anchors: bool,
    hidden: &HashSet<String>,
    s: &mut String,
) {
    let prefix = "> ".repeat(depth);
    let parent_quote_empty = "> ".repeat(depth - 1) + ">"; // depth 个 >，无尾空格

    if anchors {
        s.push_str(&format!("{prefix}<a id=\"expl-{}\"></a>\n", e.id));
    }

    if e.collapsed {
        // 折叠块：<details> 包裹 追问+解答+子树，总结在外
        let q_short: String = e.question.chars().take(30).collect();
        s.push_str(&format!("{prefix}<details>\n"));
        s.push_str(&format!("{prefix}<summary>追问：{q_short}…（已概括，点击展开）</summary>\n"));
        // details 内需要空行才能渲染 markdown
        s.push_str(&format!("{parent_quote_empty}\n"));
        render_qa_body(e, depth, s);
        // 子树也在折叠块内
        let mut first = true;
        for child in e.children.iter() {
            if hidden.contains(&child.id) {
                continue;
            }
            if first {
                s.push_str(&format!("{parent_quote_empty}\n"));
                first = false;
            } else {
                s.push('\n');
            }
            render_explanation(child, depth + 1, anchors, hidden, s);
        }
        s.push_str(&format!("{prefix}</details>\n"));
        // 总结显示在折叠块外
        if let Some(summary) = &e.summary {
            s.push_str(&format!("{parent_quote_empty}\n"));
            render_summary(summary, &prefix, s);
        }
        return;
    }

    render_qa_body(e, depth, s);
    // sum 生成的总结（未折叠时）：紧随解答，同级前缀连续
    if let Some(summary) = &e.summary {
        s.push_str(&format!("{parent_quote_empty}\n"));
        render_summary(summary, &prefix, s);
    }
    // 递归子追问
    let mut first = true;
    for child in e.children.iter() {
        if hidden.contains(&child.id) {
            continue;
        }
        if first {
            // 父子连续：父级空引用行衔接（解答行尾已有 \n），同一引用块内继续
            s.push_str(&format!("{parent_quote_empty}\n"));
            first = false;
        } else {
            // 兄弟之间：裸空行断开（上一块尾已有 \n，再补一个成空行）
            s.push('\n');
        }
        render_explanation(child, depth + 1, anchors, hidden, s);
    }
}

/// 渲染单条追问的 问答 主体（不含 summary/children）。
fn render_qa_body(e: &Explanation, depth: usize, s: &mut String) {
    let prefix = "> ".repeat(depth);
    let parent_quote_empty = "> ".repeat(depth - 1) + ">";
    s.push_str(&format!("{prefix}**追问**：{}\n", e.question));
    s.push_str(&parent_quote_empty);
    s.push('\n');
    let lines: Vec<&str> = e.answer.lines().collect();
    if lines.is_empty() {
        s.push_str(&format!("{prefix}**解答**：\n"));
    } else {
        s.push_str(&format!("{prefix}**解答**：{}\n", lines[0]));
        for line in &lines[1..] {
            s.push_str(&format!("{prefix}{line}\n"));
        }
    }
}

/// 渲染总结块（**总结**：…，多行加前缀）。
fn render_summary(summary: &str, prefix: &str, s: &mut String) {
    let slines: Vec<&str> = summary.lines().collect();
    if slines.is_empty() {
        s.push_str(&format!("{prefix}**总结**：\n"));
    } else {
        s.push_str(&format!("{prefix}**总结**：{}\n", slines[0]));
        for line in &slines[1..] {
            s.push_str(&format!("{prefix}{line}\n"));
        }
    }
}

/// 输出 markmap 兼容的缩进 markdown，可直接用 markmap 打开为思维导图。
pub fn to_mindmap(note: &Note) -> String {
    let mut s = format!("# {}\n", note.title);
    walk_mm(&note.blocks, 0, &mut s);
    s
}

fn walk_mm(blocks: &[Block], depth: usize, s: &mut String) {
    for b in blocks {
        let indent = "  ".repeat(depth + 1);
        let tag = match b.kind {
            BlockKind::Section => "",
            BlockKind::Paragraph => "¶ ",
            BlockKind::Formula => "∑ ",
        };
        let text: String = b.text.chars().take(60).collect();
        s.push_str(&format!("{indent}- {tag}{text}\n"));
        for e in &b.explanations {
            render_explanation_mm(e, depth + 1, s);
        }
        walk_mm(&b.children, depth + 1, s);
    }
}

fn render_explanation_mm(e: &Explanation, depth: usize, s: &mut String) {
    let indent = "  ".repeat(depth + 1);
    let q: String = e.question.chars().take(40).collect();
    s.push_str(&format!("{indent}- 💬 {q}\n"));
    for child in &e.children {
        render_explanation_mm(child, depth + 1, s);
    }
}

/// 按导出路径扩展名选择格式：.html → 自包含 HTML；其余 → Markdown。
pub fn render_for(path: &str, note: &Note, conv: &Conversation, annotations: &[Annotation]) -> String {
    if path.to_lowercase().ends_with(".html") {
        to_html(note, conv, annotations)
    } else {
        to_markdown(note)
    }
}

/// 生成自包含 HTML：单文件，CDN 引入 marked + KaTeX 渲染 Markdown 与公式；
/// 左侧对话树（当前节点高亮），右侧笔记；CDN 不可用时降级显示原文。
/// 资源加载带 fallback：jsdelivr 失败自动切 npmmirror。
/// `annotations`：批注（引用文字+问答线程）——正文隐藏其问答，改为文字高亮 +
/// 点击弹出只读小窗口；高亮失败的在文末「批注」列表兜底。
pub fn to_html(note: &Note, conv: &Conversation, annotations: &[Annotation]) -> String {
    let hidden = annotation_hidden_ids(annotations, conv);
    to_html_with(note, conv, true, &hidden, annotations)
}

/// 只渲染笔记正文、不含左侧对话树（Web 内嵌用，避免与页面自身树重复）。
/// `hidden` 为批注线程的解释 id 集合：这些问答不在正文内联显示（只在弹窗看）。
pub fn to_html_bare(note: &Note, conv: &Conversation, hidden: &HashSet<String>) -> String {
    to_html_with(note, conv, false, hidden, &[])
}

/// 批注线程涉及的所有解释 id（正文隐藏这些问答）。
fn annotation_hidden_ids(annotations: &[Annotation], conv: &Conversation) -> HashSet<String> {
    let mut set = HashSet::new();
    for ann in annotations {
        let mut stack = vec![ann.root_node_id.clone()];
        while let Some(id) = stack.pop() {
            if let Some(n) = conv.nodes.iter().find(|n| n.id == id) {
                if let Some(e) = &n.explanation_id {
                    set.insert(e.clone());
                }
                for c in conv
                    .nodes
                    .iter()
                    .filter(|c| c.parent.as_deref() == Some(id.as_str()))
                {
                    stack.push(c.id.clone());
                }
            }
        }
    }
    set
}

/// 把以 `root` 为根的会话子树转成 JSON（`{question, answer, is_check, summary, collapsed, children}`）。
fn thread_json(
    conv: &Conversation,
    root: &str,
    summary_map: &std::collections::HashMap<String, (Option<String>, bool)>,
) -> serde_json::Value {
    let Some(n) = conv.nodes.iter().find(|x| x.id == root) else {
        return serde_json::Value::Null;
    };
    let children: Vec<serde_json::Value> = conv
        .nodes
        .iter()
        .filter(|x| x.parent.as_deref() == Some(root))
        .map(|c| thread_json(conv, &c.id, summary_map))
        .collect();
    let (summary, collapsed) = n
        .explanation_id
        .as_ref()
        .and_then(|eid| summary_map.get(eid))
        .cloned()
        .unwrap_or((None, false));
    serde_json::json!({
        "question": n.question,
        "answer": n.answer,
        "is_check": n.explanation_id.is_none(),
        "summary": summary,
        "collapsed": collapsed,
        "children": children,
    })
}

/// 批注相关的 CSS（注入导出 HTML；单独字符串避免 `format!` 花括号转义）。
const ANN_CSS: &str = r#"
  main mark.ann-mark { background: #fff3a3; cursor: pointer; padding: 0 1px; border-radius: 2px; }
  main mark.ann-mark:hover { background: #ffe066; }
  .ann-popup { position: fixed; z-index: 90; width: 380px; max-height: 70vh; background: #fff; border: 1px solid #ddd; border-radius: 10px; box-shadow: 0 10px 40px rgba(0,0,0,.22); display: flex; flex-direction: column; }
  .ann-popup.hidden { display: none; }
  .ann-head { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-bottom: 1px solid #ddd; }
  .ann-head .ann-quote { font-size: 12px; color: #555; background: #fff7cc; padding: 2px 6px; border-radius: 4px; max-width: 300px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ann-close { margin-left: auto; border: none; background: none; font-size: 18px; cursor: pointer; color: #888; }
  .ann-thread { overflow-y: auto; padding: 8px 10px; }
  .ann-node { border-left: 2px solid #dbeafe; padding: 6px 8px; margin: 6px 0; border-radius: 0 6px 6px 0; background: #fafafa; }
  .ann-node.check { border-left-color: #f59e0b; }
  .ann-q { font-size: 13px; font-weight: 600; margin-bottom: 4px; }
  .ann-a { font-size: 13px; line-height: 1.6; color: #333; }
  .ann-a p { margin: 4px 0; }
  .ann-a .katex-display { overflow-x: auto; }
  .ann-node.summary { background: #fffbe6; border-left-color: #f59e0b; }
  .ann-summary { font-size: 13px; line-height: 1.6; color: #333; }
  .ann-summary p { margin: 4px 0; }
  .ann-summary-hint { font-size: 11px; color: #2563eb; margin-top: 4px; }
  .ann-original { margin-top: 6px; padding-top: 6px; border-top: 1px dashed #e5c76b; }
  .ann-fallback { margin-top: 40px; border-top: 1px solid #ddd; padding-top: 16px; }
  .ann-fallback:empty { display: none; }
  .ann-fallback h2 { font-size: 18px; }
  .ann-fallback-item { margin: 12px 0; padding: 8px 12px; background: #fafafa; border-radius: 8px; }
  .ann-fallback-item .ann-quote { font-size: 12px; color: #555; background: #fff7cc; padding: 2px 6px; border-radius: 4px; display: inline-block; }
"#;

/// 批注相关的 JS（只读：高亮 + 点击弹窗；注入导出 HTML）。
const ANN_SCRIPT: &str = r#"
function convertMathDelims(md) {
  const out = [];
  let inCode = false;
  for (const line of String(md).split('\n')) {
    const t = line.trimStart();
    if (t.startsWith('```')) { inCode = !inCode; out.push(line); continue; }
    if (inCode) { out.push(line); continue; }
    out.push(line.replace(/\\\[/g, '$$').replace(/\\\]/g, '$$').replace(/\\\(/g, '$').replace(/\\\)/g, '$'));
  }
  return out.join('\n');
}
function renderMd(md) {
  if (!window.marked) return md;
  md = convertMathDelims(md);
  const store = [];
  const token = (i) => '\u2063M' + i + '\u2063';
  let src = md.replace(/\$\$([\s\S]*?)\$\$/g, (m, tex) => { store.push([tex, true]); return token(store.length - 1); });
  src = src.replace(/\$([^$\n]+?)\$/g, (m, tex) => { store.push([tex, false]); return token(store.length - 1); });
  let html = marked.parse(src);
  html = html.replace(/\u2063M(\d+)\u2063/g, (_, i) => {
    const entry = store[+i];
    const tex = entry[0].replace(/^(?:[ \t]*>[ \t]?)+/gm, '').trim();
    const display = entry[1];
    if (window.katex) {
      try { return katex.renderToString(tex, { displayMode: display, throwOnError: false }); } catch (e) {}
    }
    const raw = display ? '$$' + tex + '$$' : '$' + tex + '$';
    return raw.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  });
  return html;
}
function wrapQuote(doc, range, quote, ann) {
  if (!quote) return false;
  const walker = doc.createTreeWalker(range.commonAncestorContainer, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => (range.intersectsNode(n) ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT),
  });
  const nodes = [];
  while (walker.nextNode()) nodes.push(walker.currentNode);
  const full = nodes.map((n) => n.nodeValue).join('');
  const idx = full.indexOf(quote);
  if (idx < 0) return false;
  let acc = 0, startNode = null, startOffset = 0, endNode = null, endOffset = 0;
  for (const n of nodes) {
    const len = n.nodeValue.length;
    if (!startNode && idx < acc + len) { startNode = n; startOffset = idx - acc; }
    if (!endNode && idx + quote.length <= acc + len) { endNode = n; endOffset = idx + quote.length - acc; break; }
    acc += len;
  }
  if (!startNode || !endNode) return false;
  const r = doc.createRange();
  r.setStart(startNode, startOffset);
  r.setEnd(endNode, endOffset);
  const mark = doc.createElement('mark');
  mark.className = 'ann-mark';
  try {
    r.surroundContents(mark);
  } catch (e) {
    try { const frag = r.extractContents(); mark.appendChild(frag); r.insertNode(mark); } catch (e2) { return false; }
  }
  mark.onclick = (ev) => { ev.stopPropagation(); showAnnPopup(ann, ev); };
  return true;
}
function applyAnnotations() {
  const note = document.getElementById('note');
  if (!note) return;
  const failed = [];
  for (const ann of ANNOTATIONS) {
    const anchor = document.getElementById('blk-' + ann.block_id);
    if (!anchor) { failed.push(ann); continue; }
    const anchors = note.querySelectorAll('a[id^="blk-"]');
    let next = null;
    for (const a of anchors) {
      if (a.compareDocumentPosition(anchor) & Node.DOCUMENT_POSITION_PRECEDING) { next = a; break; }
    }
    const range = document.createRange();
    range.setStartAfter(anchor);
    if (next) range.setEndBefore(next); else range.setEnd(note, note.childNodes.length);
    if (!wrapQuote(document, range, ann.quote, ann)) failed.push(ann);
  }
  renderFallback(failed);
}
function renderThread(container, node, depth) {
  const div = document.createElement('div');
  div.className = 'ann-node' + (node.is_check ? ' check' : '');
  div.style.marginLeft = depth * 10 + 'px';
  const q = document.createElement('div');
  q.className = 'ann-q';
  q.textContent = (node.is_check ? '[核对] ' : '') + node.question;
  const a = document.createElement('div');
  a.className = 'ann-a';
  a.innerHTML = renderMd(node.answer || '');
  if (node.summary) {
    div.classList.add('summary');
    const sum = document.createElement('div');
    sum.className = 'ann-summary';
    sum.innerHTML = renderMd(node.summary);
    const hint = document.createElement('div');
    hint.className = 'ann-summary-hint';
    hint.textContent = '▶ 展开原对话';
    const orig = document.createElement('div');
    orig.className = 'ann-original';
    orig.style.display = 'none';
    orig.appendChild(q);
    orig.appendChild(a);
    (node.children || []).forEach((c) => renderThread(orig, c, depth + 1));
    div.appendChild(sum);
    div.appendChild(hint);
    div.appendChild(orig);
    div.onclick = (e) => {
      e.stopPropagation();
      const open = orig.style.display !== 'none';
      orig.style.display = open ? 'none' : 'block';
      hint.textContent = open ? '▶ 展开原对话' : '▼ 收起';
    };
    container.appendChild(div);
    return;
  }
  div.appendChild(q);
  div.appendChild(a);
  container.appendChild(div);
  (node.children || []).forEach((c) => renderThread(container, c, depth + 1));
}
function showAnnPopup(ann, ev) {
  const el = document.getElementById('ann-popup');
  el.innerHTML = '';
  const head = document.createElement('div');
  head.className = 'ann-head';
  const quote = document.createElement('span');
  quote.className = 'ann-quote';
  quote.textContent = ann.quote;
  const close = document.createElement('button');
  close.className = 'ann-close';
  close.textContent = '\u00d7';
  close.onclick = () => el.classList.add('hidden');
  head.appendChild(quote);
  head.appendChild(close);
  const thread = document.createElement('div');
  thread.className = 'ann-thread';
  if (ann.thread) renderThread(thread, ann.thread, 0); else thread.textContent = '（无）';
  el.appendChild(head);
  el.appendChild(thread);
  el.classList.remove('hidden');
  const x = Math.min(window.innerWidth - 400, ev.clientX);
  const y = Math.min(window.innerHeight - 220, ev.clientY + 12);
  el.style.left = Math.max(8, x) + 'px';
  el.style.top = Math.max(8, y) + 'px';
}
function renderFallback(failed) {
  const sec = document.getElementById('ann-fallback');
  if (!sec) return;
  if (!failed.length) { sec.style.display = 'none'; return; }
  sec.innerHTML = '<h2>批注（未能定位高亮）</h2>';
  for (const ann of failed) {
    const div = document.createElement('div');
    div.className = 'ann-fallback-item';
    const q = document.createElement('div');
    q.className = 'ann-quote';
    q.textContent = '引用：' + ann.quote;
    div.appendChild(q);
    const thread = document.createElement('div');
    thread.className = 'ann-thread';
    if (ann.thread) renderThread(thread, ann.thread, 0);
    div.appendChild(thread);
    sec.appendChild(div);
  }
}
document.addEventListener('click', (e) => {
  const p = document.getElementById('ann-popup');
  if (p && !(e.target.closest && e.target.closest('#ann-popup'))) p.classList.add('hidden');
});
"#;

fn to_html_with(
    note: &Note,
    conv: &Conversation,
    include_tree: bool,
    hidden: &HashSet<String>,
    annotations: &[Annotation],
) -> String {
    let md = convert_inline_math_delims(&to_markdown_with(note, true, hidden));
    let md_json = serde_json::to_string(&md).unwrap_or_default();
    let summary_map = note.summary_map();
    let anns: Vec<serde_json::Value> = annotations
        .iter()
        .map(|a| {
            serde_json::json!({
                "block_id": a.block_id,
                "quote": a.quote,
                "thread": thread_json(conv, &a.root_node_id, &summary_map),
            })
        })
        .collect();
    let annotations_json = serde_json::to_string(&anns).unwrap_or_else(|_| "[]".to_string());
    let aside = if include_tree {
        format!(
            "  <aside>\n    <h2>对话轨迹</h2>\n    {}\n  </aside>\n",
            conv_tree_html(conv)
        )
    } else {
        String::new()
    };
    let title = html_escape(&note.title);
    format!(
        r#"<!DOCTYPE html>
<html lang="zh">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} — PaperHelper 笔记</title>
<style>/* 占位：KaTeX 样式由 JS loader 按需注入（带 CDN fallback） */</style>
<style>
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  body {{ font-family: -apple-system, "Segoe UI", "Noto Sans CJK SC", sans-serif; color: #1a1a1a; background: #fafafa; }}
  .layout {{ display: flex; min-height: 100vh; }}
  aside {{ width: 280px; flex-shrink: 0; background: #f0f0ee; border-right: 1px solid #ddd; padding: 16px; position: sticky; top: 0; height: 100vh; overflow-y: auto; }}
  aside h2 {{ font-size: 14px; color: #666; margin-bottom: 10px; font-weight: 600; }}
  main {{ flex: 1; max-width: 860px; margin: 0 auto; padding: 32px 40px; }}
  ul.tree {{ list-style: none; }}
  ul.tree ul {{ list-style: none; padding-left: 18px; border-left: 1px solid #ccc; margin-left: 8px; }}
  ul.tree li {{ padding: 3px 0; font-size: 13px; line-height: 1.4; }}
  ul.tree li.current > .label {{ background: #2563eb; color: #fff; border-radius: 4px; padding: 1px 6px; }}
  ul.tree .label {{ cursor: default; }}
  ul.tree .tok {{ color: #999; font-size: 11px; }}
  main h1 {{ font-size: 26px; margin: 0 0 24px; border-bottom: 2px solid #2563eb; padding-bottom: 10px; }}
  main h2 {{ font-size: 21px; margin: 28px 0 12px; }}
  main h3 {{ font-size: 17px; margin: 22px 0 10px; }}
  main p {{ margin: 10px 0; line-height: 1.75; }}
  main blockquote {{ border-left: 3px solid #2563eb; background: #eff6ff; margin: 12px 0; padding: 8px 14px; border-radius: 0 6px 6px 0; }}
  main blockquote blockquote {{ border-left-color: #93c5fd; background: #dbeafe; }}
  main table {{ border-collapse: collapse; margin: 12px 0; }}
  main th, main td {{ border: 1px solid #ccc; padding: 6px 12px; font-size: 14px; }}
  main th {{ background: #f3f4f6; }}
  main pre {{ background: #f3f4f6; padding: 10px; border-radius: 6px; overflow-x: auto; }}
  #fallback {{ display: none; white-space: pre-wrap; font-family: monospace; font-size: 13px; }}
{ann_css}</style>
</head>
<body>
<div class="layout">
{aside}  <main>
    <div id="note"></div>
    <pre id="fallback"></pre>
    <section id="ann-fallback" class="ann-fallback"></section>
  </main>
</div>
<div id="ann-popup" class="ann-popup hidden"></div>
<script>
// 资源加载器：按序尝试多个 CDN（jsdelivr → npmmirror），全部失败走降级
const CDNS = [
  'https://cdn.jsdelivr.net/npm',
  'https://registry.npmmirror.com'
];
function cssUrl(cdn, path) {{
  // path 形如 "name@version/rest"；npmmirror 用 files API 映射
  if (cdn.includes('npmmirror')) {{
    const slash = path.indexOf('/');
    const pkg = path.slice(0, slash);
    const rest = path.slice(slash + 1);
    const parts = pkg.split('@');
    const name = parts[0], ver = parts[parts.length - 1];
    return `https://registry.npmmirror.com/${{name}}/${{ver}}/files/${{rest}}`;
  }}
  return cdn + '/' + path;
}}
function loadCss(paths) {{
  return new Promise(resolve => {{
    let i = 0;
    const tryNext = () => {{
      if (i >= CDNS.length) return resolve(false);
      const link = document.createElement('link');
      link.rel = 'stylesheet';
      link.href = cssUrl(CDNS[i % CDNS.length], paths[0]);
      link.onload = () => resolve(true);
      link.onerror = () => {{ i++; tryNext(); }};
      document.head.appendChild(link);
    }};
    tryNext();
  }});
}}
function loadScripts(paths) {{
  // 串行加载，每个脚本依次尝试各 CDN
  return paths.reduce((p, path) => p.then(() => new Promise(resolve => {{
    let i = 0;
    const tryNext = () => {{
      if (i >= CDNS.length) return resolve(false);
      const s = document.createElement('script');
      s.src = cssUrl(CDNS[i], path);
      s.onload = () => resolve(true);
      s.onerror = () => {{ i++; tryNext(); }};
      document.body.appendChild(s);
    }};
    tryNext();
  }})), Promise.resolve(true));
}}
const MD = {md_json};
const ANNOTATIONS = {annotations_json};
{ann_script}
(async () => {{
  await loadCss(['katex@0.16.9/dist/katex.min.css']);
  await loadScripts([
    'marked@12.0.2/marked.min.js',
    'katex@0.16.9/dist/katex.min.js'
  ]);
  if (window.marked) {{
    document.getElementById('note').innerHTML = renderMd(MD);
    applyAnnotations();
  }} else {{
    // CDN 全部不可用时降级为纯文本
    document.getElementById('fallback').style.display = 'block';
    document.getElementById('fallback').textContent = MD;
  }}
}})();
</script>
</body>
</html>
"#,
        title = title,
        aside = aside,
        md_json = md_json,
        annotations_json = annotations_json,
        ann_css = ANN_CSS,
        ann_script = ANN_SCRIPT
    )
}

/// 对话树渲染为嵌套 <ul>，当前节点高亮。
fn conv_tree_html(conv: &Conversation) -> String {
    use std::collections::HashMap;
    let mut children: HashMap<Option<&str>, Vec<&crate::conversation::ConvNode>> = HashMap::new();
    for n in &conv.nodes {
        children.entry(n.parent.as_deref()).or_default().push(n);
    }
    fn walk(
        parent: Option<&str>,
        children: &HashMap<Option<&str>, Vec<&crate::conversation::ConvNode>>,
        current: Option<&str>,
        out: &mut String,
    ) {
        let Some(nodes) = children.get(&parent) else { return };
        out.push_str("<ul class=\"tree\">");
        for n in nodes {
            let cur = if current == Some(n.id.as_str()) { " class=\"current\"" } else { "" };
            let tok = if n.input_tokens + n.output_tokens > 0 {
                format!(" <span class=\"tok\">{}→{}tok</span>", n.input_tokens, n.output_tokens)
            } else {
                String::new()
            };
            let label = html_escape(&n.label);
            out.push_str(&format!("<li{cur}><span class=\"label\">◦ {label}</span>{tok}"));
            walk(Some(n.id.as_str()), children, current, out);
            out.push_str("</li>");
        }
        out.push_str("</ul>");
    }
    let mut out = String::new();
    walk(None, &children, conv.current.as_deref(), &mut out);
    if out.is_empty() {
        out.push_str("<p style=\"color:#999;font-size:13px\">（尚无对话）</p>");
    }
    out
}

/// HTML 文本转义。
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 把 `\(` `\)` `\[` `\]` 定界符统一转换为 `$` `$$`。
/// 原因：markdown 中 `\(` 是合法反斜杠转义，marked 会将其吃成 `(`，
/// 导致 KaTeX auto-render 找不到行内公式定界符。按 ``` 代码围栏切分，
/// 只转换非代码段，避免破坏代码示例。
pub(crate) fn convert_inline_math_delims(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut in_code = false;
    for line in md.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with("```") {
            in_code = !in_code;
            out.push_str(line);
            continue;
        }
        if in_code {
            out.push_str(line);
            continue;
        }
        let converted = line
            .replace("\\[", "$$")
            .replace("\\]", "$$")
            .replace("\\(", "$")
            .replace("\\)", "$");
        out.push_str(&converted);
    }
    out
}
