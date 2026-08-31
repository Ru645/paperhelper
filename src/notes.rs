use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BlockKind {
    Section,
    Paragraph,
    Formula,
}

impl BlockKind {
    pub fn tag(&self) -> &'static str {
        match self {
            BlockKind::Section => "§",
            BlockKind::Paragraph => "¶",
            BlockKind::Formula => "∑",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Explanation {
    pub id: String,
    pub question: String,
    pub answer: String,
    pub concept: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub id: String,
    pub kind: BlockKind,
    pub text: String,
    #[serde(default)]
    pub children: Vec<Block>,
    #[serde(default)]
    pub explanations: Vec<Explanation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Note {
    #[serde(default)]
    pub paper_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub blocks: Vec<Block>,
    /// 论文全文纯文本（ingest 时抽取，ask 时作为上下文重发）。
    #[serde(default)]
    pub raw_text: String,
}

impl Note {
    pub fn flatten(&self) -> Vec<(&Block, usize)> {
        let mut out = Vec::new();
        fn walk<'a>(blocks: &'a [Block], depth: usize, out: &mut Vec<(&'a Block, usize)>) {
            for b in blocks {
                out.push((b, depth));
                walk(&b.children, depth + 1, out);
            }
        }
        walk(&self.blocks, 0, &mut out);
        out
    }

    pub fn count_blocks(&self) -> usize {
        self.flatten().len()
    }

    pub fn find_block(&self, id: &str) -> Option<&Block> {
        fn walk<'a>(blocks: &'a [Block], id: &str) -> Option<&'a Block> {
            for b in blocks {
                if b.id == id {
                    return Some(b);
                }
                if let Some(f) = walk(&b.children, id) {
                    return Some(f);
                }
            }
            None
        }
        walk(&self.blocks, id)
    }

    pub fn find_block_mut(&mut self, id: &str) -> Option<&mut Block> {
        fn walk<'a>(blocks: &'a mut [Block], id: &str) -> Option<&'a mut Block> {
            for b in blocks {
                if b.id == id {
                    return Some(b);
                }
                if let Some(f) = walk(&mut b.children, id) {
                    return Some(f);
                }
            }
            None
        }
        walk(&mut self.blocks, id)
    }

    /// 用关键词重叠度定位最相关的块（Rust 确定性逻辑）。
    pub fn locate(&self, query: &str) -> Option<&Block> {
        let terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .filter(|t| t.chars().count() >= 2)
            .map(|t| t.to_string())
            .collect();
        if terms.is_empty() {
            return None;
        }
        let mut best: Option<(&Block, usize)> = None;
        for (b, _) in self.flatten() {
            let low = b.text.to_lowercase();
            let score = terms.iter().map(|t| low.matches(t.as_str()).count()).sum::<usize>();
            if score > 0 && best.map_or(true, |(_, s)| score > s) {
                best = Some((b, score));
            }
        }
        best.map(|(b, _)| b)
    }

    /// 把笔记渲染回 Markdown（供 ask 上下文使用，也供导出参考）。
    pub fn to_markdown(&self) -> String {
        let mut s = format!("# {}\n", self.title);
        fn walk(blocks: &[Block], depth: usize, s: &mut String) {
            for b in blocks {
                match b.kind {
                    BlockKind::Section => {
                        let level = (depth + 2).min(6);
                        s.push_str(&format!("{} {}\n", "#".repeat(level), b.text));
                    }
                    BlockKind::Paragraph => {
                        s.push_str(&format!("{}\n", b.text));
                    }
                    BlockKind::Formula => {
                        s.push_str(&format!("$$\n{}\n$$\n", b.text));
                    }
                }
                walk(&b.children, depth + 1, s);
            }
        }
        walk(&self.blocks, 0, &mut s);
        s
    }
}

fn uid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 解析 LLM 输出的 Markdown 为笔记树。
/// 约定：`#`=标题；`##`/`###`…=按层级嵌套的 Section；`$$…$$`=Formula；
/// 其余非空行=Paragraph（连续行合并为一段），挂到最近的 Section 下。
pub fn parse_markdown_note(md: &str, raw_text: &str) -> Note {
    let md = strip_fences(md);
    let lines: Vec<&str> = md.lines().collect();

    let mut title = String::new();
    let mut roots: Vec<Block> = Vec::new();
    // stack[i] 是当前打开的 depth=i 的 Section。
    let mut stack: Vec<Block> = Vec::new();
    let mut para_buf = String::new();

    let flush_para = |buf: &mut String, roots: &mut Vec<Block>, stack: &mut Vec<Block>| {
        if !buf.trim().is_empty() {
            let block = Block {
                id: uid(),
                kind: BlockKind::Paragraph,
                text: buf.trim().to_string(),
                children: Vec::new(),
                explanations: Vec::new(),
            };
            if let Some(sec) = stack.last_mut() {
                sec.children.push(block);
            } else {
                roots.push(block);
            }
        }
        buf.clear();
    };

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            flush_para(&mut para_buf, &mut roots, &mut stack);
            i += 1;
            continue;
        }

        // 标题行
        if let Some(rest) = trimmed.strip_prefix('#') {
            let level = trimmed.chars().take_while(|c| *c == '#').count();
            let text = rest.trim().to_string();
            flush_para(&mut para_buf, &mut roots, &mut stack);

            if level == 1 {
                if title.is_empty() {
                    title = text;
                }
                i += 1;
                continue;
            }

            // 关闭 depth >= (level-2) 的 Section
            let target_depth = level - 2;
            while stack.len() > target_depth {
                let sec = stack.pop().unwrap();
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(sec);
                } else {
                    roots.push(sec);
                }
            }
            stack.push(Block {
                id: uid(),
                kind: BlockKind::Section,
                text,
                children: Vec::new(),
                explanations: Vec::new(),
            });
            i += 1;
            continue;
        }

        // 公式块 $$ ... $$（可能跨行）
        if trimmed.starts_with("$$") {
            flush_para(&mut para_buf, &mut roots, &mut stack);
            let mut content = String::new();
            let first = trimmed.trim_start_matches("$$");
            if first.trim().is_empty() {
                // 多行公式
            } else {
                content.push_str(first.trim());
            }
            // 若单行就闭合
            if !trimmed.ends_with("$$") || trimmed.len() > 2 {
                // 继续收集到闭合
                while i + 1 < lines.len() && !lines[i + 1].trim().ends_with("$$") {
                    i += 1;
                    content.push('\n');
                    content.push_str(lines[i].trim());
                }
                if i + 1 < lines.len() {
                    i += 1;
                    let last = lines[i].trim();
                    let last = last.strip_suffix("$$").unwrap_or(last).trim();
                    if !last.is_empty() {
                        content.push('\n');
                        content.push_str(last);
                    }
                }
            }
            let block = Block {
                id: uid(),
                kind: BlockKind::Formula,
                text: content.trim().to_string(),
                children: Vec::new(),
                explanations: Vec::new(),
            };
            if let Some(sec) = stack.last_mut() {
                sec.children.push(block);
            } else {
                roots.push(block);
            }
            i += 1;
            continue;
        }

        // 普通段落行：累积
        if !para_buf.is_empty() {
            para_buf.push('\n');
        }
        para_buf.push_str(trimmed);
        i += 1;
    }
    flush_para(&mut para_buf, &mut roots, &mut stack);

    // 关闭剩余打开的 Section
    while let Some(sec) = stack.pop() {
        if let Some(parent) = stack.last_mut() {
            parent.children.push(sec);
        } else {
            roots.push(sec);
        }
    }

    if title.is_empty() {
        title = "未命名论文".to_string();
    }

    // 兜底：完全没解析出内容时，用原文做一个段落
    if roots.is_empty() && !raw_text.is_empty() {
        let t: String = raw_text.chars().take(2000).collect();
        roots.push(Block {
            id: uid(),
            kind: BlockKind::Paragraph,
            text: t,
            children: Vec::new(),
            explanations: Vec::new(),
        });
    }

    Note {
        paper_id: String::new(),
        title,
        blocks: roots,
        raw_text: raw_text.to_string(),
    }
}

fn strip_fences(content: &str) -> &str {
    let t = content.trim();
    if let Some(rest) = t.strip_prefix("```") {
        // 跳过语言标识行
        let rest = rest.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '-');
        let rest = rest.trim_start_matches('\n');
        if let Some(rest) = rest.strip_suffix("```") {
            return rest.trim();
        }
        return rest.trim();
    }
    t
}
