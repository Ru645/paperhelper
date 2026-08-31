use crate::notes::{Block, BlockKind, Note};

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
                s.push_str(&format!("{} {}\n\n", "#".repeat(level), b.text));
            }
            BlockKind::Paragraph => {
                s.push_str(&format!("{}\n\n", b.text));
            }
            BlockKind::Formula => {
                s.push_str(&format!("$$\n{}\n$$\n\n", b.text));
            }
        }
        for e in &b.explanations {
            s.push_str(&format!(
                "> **追问**：{}\n>\n> **解答**：{}\n\n",
                e.question, e.answer
            ));
        }
        walk_md(&b.children, depth + 1, s);
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
            let q: String = e.question.chars().take(40).collect();
            s.push_str(&format!("{indent}  - 💬 {q}\n"));
        }
        walk_mm(&b.children, depth + 1, s);
    }
}
