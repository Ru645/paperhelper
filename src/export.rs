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
