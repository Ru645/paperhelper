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
    /// 支持中英文混排：英文按单词切分，中文按 2-gram 切分，避免"BERTScore是什么"被当成一个词。
    pub fn locate(&self, query: &str) -> Option<&Block> {
        let terms = tokenize_query(query);
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

/// 把查询切分为关键词用于 locate 定位。
/// 英文：连续 ASCII 字母数字为单词（≥2 字符）；中文：2-gram（连续汉字每 2 字一组）。
/// 过滤常见无意义词。注意：Rust 的 is_alphanumeric 对汉字返回 true，需显式排除 CJK。
fn tokenize_query(query: &str) -> Vec<String> {
    let q = query.to_lowercase();
    let mut terms: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut chars = q.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            buf.push(c);
        } else {
            if !buf.is_empty() {
                if buf.chars().count() >= 2 {
                    terms.push(buf.clone());
                }
                buf.clear();
            }
            // 中文 2-gram：连续 CJK 字符两两组合
            if is_cjk(c) {
                let mut gram = String::new();
                gram.push(c);
                if let Some(&next) = chars.peek() {
                    if is_cjk(next) {
                        gram.push(next);
                        chars.next();
                        terms.push(gram);
                        continue;
                    }
                }
            }
        }
    }
    if !buf.is_empty() && buf.chars().count() >= 2 {
        terms.push(buf);
    }
    const STOP: &[&str] = &["什么", "怎么", "为什么", "如何", "这个", "那个", "可以", "一下", "请问", "是什么", "the", "is", "a", "an", "of", "to", "in"];
    terms.retain(|t| !STOP.iter().any(|s| s == t));
    terms
}

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
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
            // 空行不分割块：仅作段落内分隔（一个 section 下所有内容合成一块），
            // 在 para_buf 中插入一个换行表示段落边界，但不 flush。
            if !para_buf.is_empty() && !para_buf.ends_with('\n') {
                para_buf.push('\n');
            }
            i += 1;
            continue;
        }

        // 标题行
        if trimmed.strip_prefix('#').is_some() {
            let level = trimmed.chars().take_while(|c| *c == '#').count();
            let text = trimmed.trim_start_matches('#').trim().to_string();
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
        // 公式视为所在段落的一部分，不单独切块、不分割段落：
        //  - 若当前正在累积段落（para_buf 非空），直接追加进去；
        //  - 若 para_buf 已被空行 flush，则并入最近一个段落块的文本末尾。
        if trimmed.starts_with("$$") {
            let mut content = String::new();
            let first_inner = trimmed.strip_prefix("$$").unwrap_or(trimmed);
            if first_inner.trim().ends_with("$$") {
                let mid = first_inner.trim().strip_suffix("$$").unwrap_or(first_inner).trim();
                if !mid.is_empty() {
                    content.push_str(mid);
                }
            } else if !first_inner.trim().is_empty() {
                content.push_str(first_inner.trim());
                content.push('\n');
            }
            if !trimmed[2..].contains("$$") || trimmed.len() == 2 {
                while i + 1 < lines.len() {
                    i += 1;
                    let l = lines[i].trim();
                    if l.ends_with("$$") {
                        let body = l.strip_suffix("$$").unwrap_or(l).trim();
                        if !body.is_empty() {
                            content.push_str(body);
                            content.push('\n');
                        }
                        break;
                    }
                    content.push_str(l);
                    content.push('\n');
                }
            }
            let formula_text = format!("$$\n{}\n$$", content.trim());
            if !para_buf.is_empty() {
                para_buf.push('\n');
                para_buf.push_str(&formula_text);
            } else {
                // 并入最近一个段落块（跳过被空行隔开的情况）
                let target: Option<&mut Block> = if let Some(sec) = stack.last_mut() {
                    sec.children.last_mut()
                } else {
                    roots.last_mut()
                };
                match target {
                    Some(b) if b.kind == BlockKind::Paragraph => {
                        if !b.text.is_empty() {
                            b.text.push('\n');
                        }
                        b.text.push_str(&formula_text);
                    }
                    _ => {
                        // 前面没有段落（如 section 开头），公式作为新段落累积
                        para_buf.push_str(&formula_text);
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_markdown() {
        let md = "# Title\n## 1. Intro\nsome text\n## 2. Method\n### 2.1 Attn\npara1\n$$\nE=mc^2\n$$\npara2\n";
        let note = parse_markdown_note(md, "raw");
        assert_eq!(note.title, "Title");
        // 一个 section 下所有段落+公式合成一块
        let flat = note.flatten();
        let attn = flat
            .iter()
            .find(|(b, _)| b.text.contains("para1"))
            .expect("find para");
        assert!(attn.0.text.contains("E=mc^2"), "公式与段落应在同一块");
        assert!(attn.0.text.contains("para2"), "后续段落也合并进来");
        // 不应有独立的 Formula 块
        assert!(!flat.iter().any(|(b, _)| b.kind == BlockKind::Formula));
    }
    #[test]
    fn locate_finds_block() {
        let md = "# T\n## Intro\nWe propose a transformer model.\n## Method\nThe attention is key.\n";
        let note = parse_markdown_note(md, "raw");
        let b = note.locate("attention mechanism").expect("should locate");
        assert!(b.text.to_lowercase().contains("attention"));
    }

    #[test]
    fn locate_chinese_query() {
        // 模拟中文无空格提问"BERTScore是什么"，应切出 bertscore 并命中
        let md = "# T\n## BERTScore\n对句子 r_i 用 BERTScore 计算相似度。\n## Other\n无关内容\n";
        let note = parse_markdown_note(md, "raw");
        let b = note.locate("BERTScore是什么").expect("应命中 BERTScore 块");
        assert!(b.text.contains("BERTScore"), "命中的块应含 BERTScore");
    }

    #[test]
    fn formula_merges_into_preceding_paragraph() {
        // 模拟真实 LLM 输出：公式前后有空行，但同一 section 下应合成一块
        let md = "# T\n## M\nThe entropy is defined as:\n\n$$\nH = -\\sum p \\ln p\n$$\n\nThis means high uncertainty.\n";
        let note = parse_markdown_note(md, "raw");
        let flat = note.flatten();
        // section M 下应只有一块，含全部文字与公式
        let m_block = flat
            .iter()
            .find(|(b, _)| b.text.contains("entropy"))
            .expect("find entropy block");
        assert!(
            m_block.0.text.contains("H = -"),
            "公式应在同一块内: {}",
            m_block.0.text
        );
        assert!(
            m_block.0.text.contains("high uncertainty"),
            "后续段落也合并进同一块: {}",
            m_block.0.text
        );
        assert!(
            !flat.iter().any(|(b, _)| b.kind == BlockKind::Formula),
            "不应有独立 Formula 块"
        );
    }

    #[test]
    fn explanations_attach_and_export() {
        let md = "# T\n## Intro\nWe propose a transformer.\n";
        let mut note = parse_markdown_note(md, "raw");
        // 定位并挂解释
        let bid = note.locate("transformer").unwrap().id.clone();
        note.find_block_mut(&bid).unwrap().explanations.push(Explanation {
            id: "x".into(),
            question: "什么是transformer?".into(),
            answer: "一种基于注意力的模型。".into(),
            concept: "transformer".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        });
        let md_out = crate::export::to_markdown(&note);
        assert!(md_out.contains("追问"), "export should include explanation: {md_out}");
        assert!(md_out.contains("transformer"));
        let mm = crate::export::to_mindmap(&note);
        assert!(mm.contains("# T"), "mindmap: {mm}");
    }
}
