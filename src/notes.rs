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
}

fn uid() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Deserialize)]
struct RawNote {
    #[serde(default)]
    title: String,
    #[serde(default)]
    blocks: Vec<RawBlock>,
}

#[derive(Deserialize)]
struct RawBlock {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    children: Vec<RawBlock>,
}

fn parse_kind(s: &str) -> BlockKind {
    match s.to_lowercase().as_str() {
        "section" => BlockKind::Section,
        "formula" | "equation" => BlockKind::Formula,
        _ => BlockKind::Paragraph,
    }
}

fn convert_block(raw: RawBlock) -> Block {
    Block {
        id: uid(),
        kind: parse_kind(&raw.kind),
        text: raw.text,
        children: raw.children.into_iter().map(convert_block).collect(),
        explanations: Vec::new(),
    }
}

fn strip_fences(content: &str) -> String {
    let t = content.trim();
    if let Some(rest) = t.strip_prefix("```") {
        let rest = rest.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_');
        let rest = rest.trim_start_matches('\n');
        if let Some(rest) = rest.strip_suffix("```") {
            return rest.trim().to_string();
        }
        return rest.trim().to_string();
    }
    t.to_string()
}

/// 把 LLM 返回的 JSON 解析成 Note；失败则用原文兜底成一个段落块。
pub fn parse_note(content: &str, fallback_text: &str) -> Note {
    let cleaned = strip_fences(content);
    match serde_json::from_str::<RawNote>(&cleaned) {
        Ok(r) => {
            let blocks: Vec<Block> = r.blocks.into_iter().map(convert_block).collect();
            Note {
                paper_id: String::new(),
                title: if r.title.is_empty() {
                    "未命名论文".to_string()
                } else {
                    r.title
                },
                blocks,
            }
        }
        Err(_) => {
            let t: String = fallback_text.chars().take(2000).collect();
            Note {
                paper_id: String::new(),
                title: "未命名论文（结构化失败，使用原文）".to_string(),
                blocks: vec![Block {
                    id: uid(),
                    kind: BlockKind::Paragraph,
                    text: t,
                    children: Vec::new(),
                    explanations: Vec::new(),
                }],
            }
        }
    }
}
