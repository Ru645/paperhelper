use crate::conversation::Conversation;
use crate::notes::{Block, BlockKind, Explanation, Note};

pub fn to_markdown(note: &Note) -> String {
    let mut s = format!("# {}\n\n", note.title);
    walk_md(&note.blocks, 0, &mut s);
    s
}

fn walk_md(blocks: &[Block], depth: usize, s: &mut String) {
    for b in blocks {
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
        for e in &b.explanations {
            render_explanation(e, 1, s);
        }
        walk_md(&b.children, depth + 1, s);
    }
}

/// 递归渲染追问，depth=1 为顶层（`> `），depth=2 为子追问（`> > `），以此类推。
/// 多行 answer 每行都加当前层级的前缀，避免 markdown 引用块断裂。
fn render_explanation(e: &Explanation, depth: usize, s: &mut String) {
    let prefix = if depth == 1 {
        "> ".to_string()
    } else {
        format!("{}> ", "> ".repeat(depth - 1))
    };
    // 追问行
    s.push_str(&format!("{}**追问**：{}\n", prefix, e.question));
    s.push_str(&prefix.trim_end_matches(' '));
    s.push('\n');
    // 解答：首行加 **解答**：前缀，后续行也加 prefix
    let lines: Vec<&str> = e.answer.lines().collect();
    if lines.is_empty() {
        s.push_str(&format!("{}**解答**：\n", prefix));
    } else {
        s.push_str(&format!("{}**解答**：{}\n", prefix, lines[0]));
        for line in &lines[1..] {
            s.push_str(&format!("{}{}\n", prefix, line));
        }
    }
    s.push('\n');
    // 递归子追问
    for child in &e.children {
        render_explanation(child, depth + 1, s);
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
pub fn render_for(path: &str, note: &Note, conv: &Conversation) -> String {
    if path.to_lowercase().ends_with(".html") {
        to_html(note, conv)
    } else {
        to_markdown(note)
    }
}

/// 生成自包含 HTML：单文件，CDN 引入 marked + KaTeX 渲染 Markdown 与公式；
/// 左侧对话树（当前节点高亮），右侧笔记；CDN 不可用时降级显示原文。
pub fn to_html(note: &Note, conv: &Conversation) -> String {
    let md = to_markdown(note);
    let md_json = serde_json::to_string(&md).unwrap_or_default();
    let tree = conv_tree_html(conv);
    let title = html_escape(&note.title);
    format!(
        r#"<!DOCTYPE html>
<html lang="zh">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} — PaperHelper 笔记</title>
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/katex@0.16.9/dist/katex.min.css">
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
</style>
</head>
<body>
<div class="layout">
  <aside>
    <h2>对话轨迹</h2>
    {tree}
  </aside>
  <main>
    <div id="note"></div>
    <pre id="fallback"></pre>
  </main>
</div>
<script src="https://cdn.jsdelivr.net/npm/marked@12.0.2/marked.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/katex@0.16.9/dist/katex.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/katex@0.16.9/dist/contrib/auto-render.min.js"></script>
<script>
const MD = {md_json};
function render() {{
  if (window.marked) {{
    document.getElementById('note').innerHTML = marked.parse(MD);
    if (window.renderMathInElement) {{
      renderMathInElement(document.getElementById('note'), {{
        delimiters: [
          {{left: '$$', right: '$$', display: true}},
          {{left: '$', right: '$', display: false}},
          {{left: '\\\\(', right: '\\\\)', display: false}},
          {{left: '\\\\[', right: '\\\\]', display: true}}
        ],
        throwOnError: false
      }});
    }}
  }} else {{
    // CDN 不可用时降级为纯文本
    document.getElementById('fallback').style.display = 'block';
    document.getElementById('fallback').textContent = MD;
  }}
}}
render();
</script>
</body>
</html>
"#,
        title = title,
        tree = tree,
        md_json = md_json
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
