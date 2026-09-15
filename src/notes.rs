//! 笔记树模型与"Markdown → 树"确定性解析器。
//!
//! 笔记树节点有三类（BlockKind）：Section（带层级编号如 3.2）、Paragraph、
//! Formula。LLM 生成的 Markdown 由 `parse_markdown_note` 解析：`#` 标题、
//! `##/###…` 按深度压栈成嵌套 Section，段落连续行合并成单个 Paragraph 块、
//! `$$…$$` 公式并入所在段落（不单独切块）。编号全部由 Rust 的
//! `assign_numbers` 后处理赋予，故解析前先 `strip_leading_number` 剥掉
//! LLM 自带编号（含中文"一、"序号）。
//!
//! Explanation 是挂在 Block 上的递归追问树（回答→再追问层层嵌套），
//! 支持 `sum` 折叠（collapsed + summary）。`locate` 用中英混排词袋打分定位
//! 最相关块，实现 `ask` 的"自动找位置"。

use serde::{Deserialize, Serialize};

/// 笔记标题在「块编辑」接口里的哨兵 id（Web 端用 `blk-__title__` 定位标题）。
pub const TITLE_ID: &str = "__title__";

/// 笔记块的类型：章节 / 段落 / 公式。
/// Formula 在解析阶段被并入 Paragraph，通常只在导出渲染细节中使用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BlockKind {
    Section,
    Paragraph,
    Formula,
}

impl BlockKind {
    /// blocks/tree 视图中的小图标。
    pub fn tag(&self) -> &'static str {
        match self {
            BlockKind::Section => "§",
            BlockKind::Paragraph => "¶",
            BlockKind::Formula => "∑",
        }
    }
}

/// 一次追问的解释（可递归嵌套子追问）。
/// 由 ask/check 产生并挂到某 Block 的 explanations；`children` 承载对
/// 本回答的再追问；sum 之后 `collapsed=true` 且 `summary` 为概括文字。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Explanation {
    pub id: String,
    pub question: String,
    pub answer: String,
    pub concept: String,
    pub created_at: String,
    #[serde(default)]
    pub children: Vec<Explanation>,
    /// sum 生成的知识卡片（"总结：……"），渲染在该追问块内。
    #[serde(default)]
    pub summary: Option<String>,
    /// sum 后折叠：追问子树包进 <details>，总结显示在外。
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub id: String,
    pub kind: BlockKind,
    pub text: String,
    /// 层级编号，如 "3.2"，仅 Section 有；Paragraph 无（空串）。
    #[serde(default)]
    pub number: String,
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
    /// 材料类型：paper（论文，默认）/ note（自己的笔记）/ lecture（课程讲义）。
    /// 用于界面标注与 ask 上下文策略；直接导入的笔记 raw_text 为空。
    #[serde(default)]
    pub material_kind: String,
    /// 数学宏定义（`\newcommand` 等原文），渲染公式时作为 KaTeX 的 macros 注册。
    /// 常见于 HTML/讲义导入（MathJax 的宏块），普通笔记为空。
    #[serde(default)]
    pub math_macros: Option<String>,
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

    /// 修改块文本；`TITLE_ID` 表示修改标题。返回是否找到目标。
    pub fn set_text(&mut self, id: &str, text: &str) -> bool {
        if id == TITLE_ID {
            self.title = text.to_string();
            return true;
        }
        match self.find_block_mut(id) {
            Some(b) => {
                b.text = text.to_string();
                true
            }
            None => false,
        }
    }

    /// 渲染某块（含子树）为 Markdown，供编辑弹窗展示（**不含自动编号**）。
    /// Section 从 `##` 起、子树逐层 `###`…；段落/公式输出正文。`TITLE_ID` 返回标题 + 全文。
    pub fn block_markdown(&self, id: &str) -> Option<String> {
        fn render_block(b: &Block, level: usize, s: &mut String) {
            match b.kind {
                BlockKind::Section => {
                    s.push_str(&format!("{} {}\n\n", "#".repeat(level.min(6)), b.text));
                    for c in &b.children {
                        render_block(c, level + 1, s);
                    }
                }
                BlockKind::Paragraph => s.push_str(&format!("{}\n\n", b.text)),
                BlockKind::Formula => s.push_str(&format!("$$\n{}\n$$\n\n", b.text)),
            }
        }
        if id == TITLE_ID {
            let mut s = format!("# {}\n\n", self.title);
            for b in &self.blocks {
                render_block(b, 2, &mut s);
            }
            return Some(s.trim_end().to_string());
        }
        let b = self.find_block(id)?;
        let mut s = String::new();
        render_block(b, 2, &mut s);
        Some(s.trim_end().to_string())
    }

    /// 在目标块之后插入若干块（同父级）。返回是否找到目标。
    pub fn insert_blocks_after(&mut self, target_id: &str, blocks: Vec<Block>) -> bool {
        fn walk(blocks: &mut Vec<Block>, target_id: &str, new_blocks: &[Block]) -> bool {
            if let Some(pos) = blocks.iter().position(|b| b.id == target_id) {
                for (k, b) in new_blocks.iter().enumerate() {
                    blocks.insert(pos + 1 + k, b.clone());
                }
                return true;
            }
            for b in blocks.iter_mut() {
                if walk(&mut b.children, target_id, new_blocks) {
                    return true;
                }
            }
            false
        }
        walk(&mut self.blocks, target_id, &blocks)
    }

    /// 用新块替换某 Section 的全部 children（整节重写）。
    /// 段落块会先提升为 Section（重写段落直接改文本，见 `set_text`）。
    pub fn replace_children(&mut self, section_id: &str, blocks: Vec<Block>) -> bool {
        match self.find_block_mut(section_id) {
            Some(b) if b.kind == BlockKind::Section => {
                b.children = blocks;
                true
            }
            _ => false,
        }
    }

    /// 删除块及其子树（含追问）。返回是否删除。
    pub fn remove_block(&mut self, id: &str) -> bool {
        fn walk(blocks: &mut Vec<Block>, id: &str) -> bool {
            if let Some(pos) = blocks.iter().position(|b| b.id == id) {
                blocks.remove(pos);
                return true;
            }
            for b in blocks.iter_mut() {
                if walk(&mut b.children, id) {
                    return true;
                }
            }
            false
        }
        walk(&mut self.blocks, id)
    }

    /// 结构变化（插入/删除/重写）后重新分配章节编号。
    pub fn renumber(&mut self) {
        assign_numbers(self);
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
                        let title = if b.number.is_empty() {
                            b.text.clone()
                        } else {
                            format!("{} {}", b.number, b.text)
                        };
                        s.push_str(&format!("{} {}\n", "#".repeat(level), title));
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

    /// 按编号查找 Section 块（如 "3.2" 匹配 number=="3.2"）。
    pub fn find_section_by_number(&self, num: &str) -> Option<&Block> {
        for (b, _) in self.flatten() {
            if b.kind == BlockKind::Section && b.number == num {
                return Some(b);
            }
        }
        None
    }

    /// 按编号查找 Section 块（可变）。
    #[allow(dead_code)]
    pub fn find_section_by_number_mut(&mut self, num: &str) -> Option<&mut Block> {
        fn walk<'a>(blocks: &'a mut [Block], num: &str) -> Option<&'a mut Block> {
            for b in blocks {
                if b.kind == BlockKind::Section && b.number == num {
                    return Some(b);
                }
                if let Some(f) = walk(&mut b.children, num) {
                    return Some(f);
                }
            }
            None
        }
        walk(&mut self.blocks, num)
    }

    /// 递归查找指定 id 的 Explanation（可变），用于嵌套追问插入。
    pub fn find_explanation_mut(&mut self, id: &str) -> Option<&mut Explanation> {
        fn walk_expl<'a>(expls: &'a mut [Explanation], id: &str) -> Option<&'a mut Explanation> {
            for e in expls {
                if e.id == id {
                    return Some(e);
                }
                if let Some(f) = walk_expl(&mut e.children, id) {
                    return Some(f);
                }
            }
            None
        }
        fn walk<'a>(blocks: &'a mut [Block], id: &str) -> Option<&'a mut Explanation> {
            for b in blocks {
                if let Some(f) = walk_expl(&mut b.explanations, id) {
                    return Some(f);
                }
                if let Some(f) = walk(&mut b.children, id) {
                    return Some(f);
                }
            }
            None
        }
        walk(&mut self.blocks, id)
    }

    /// 递归删除指定 id 的 Explanation（连同其嵌套子树），返回被删除的节点。
    /// 用于 `del` 删除对话节点时同步清理笔记中的解释。找不到返回 None。
    pub fn remove_explanation(&mut self, id: &str) -> Option<Explanation> {
        fn walk_expl(expls: &mut Vec<Explanation>, id: &str) -> Option<Explanation> {
            if let Some(pos) = expls.iter().position(|e| e.id == id) {
                return Some(expls.remove(pos));
            }
            for e in expls.iter_mut() {
                if let Some(f) = walk_expl(&mut e.children, id) {
                    return Some(f);
                }
            }
            None
        }
        fn walk(blocks: &mut [Block], id: &str) -> Option<Explanation> {
            for b in blocks.iter_mut() {
                if let Some(f) = walk_expl(&mut b.explanations, id) {
                    return Some(f);
                }
                if let Some(f) = walk(&mut b.children, id) {
                    return Some(f);
                }
            }
            None
        }
        walk(&mut self.blocks, id)
    }

    /// 返回 id → (summary, collapsed) 映射，供 Web/导出把被 sum 的节点渲染成
    /// 「总结」节点（显示总结、可展开查看原对话）。
    pub fn summary_map(&self) -> std::collections::HashMap<String, (Option<String>, bool)> {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        fn walk_expl(expls: &[Explanation], map: &mut HashMap<String, (Option<String>, bool)>) {
            for e in expls {
                map.insert(e.id.clone(), (e.summary.clone(), e.collapsed));
                walk_expl(&e.children, map);
            }
        }
        fn walk_blocks(blocks: &[Block], map: &mut HashMap<String, (Option<String>, bool)>) {
            for b in blocks {
                walk_expl(&b.explanations, map);
                walk_blocks(&b.children, map);
            }
        }
        walk_blocks(&self.blocks, &mut map);
        map
    }
}

/// 去掉标题开头的编号前缀（数字 "1.1 " 与中文序号 "一、"），
/// 因为编号由 Rust 后处理统一赋值，避免 LLM 自带编号导致重复（如 "3 三、本文方案"）。
/// 循环去除，防止 LLM 写了 "1.1 1.1"、"一、 二、" 这类重复。
fn strip_leading_number(s: &str) -> String {
    let mut s = s.trim().to_string();
    loop {
        let stripped = strip_one_leading_number(&s);
        if stripped == s {
            break;
        }
        s = stripped;
    }
    s
}

fn strip_one_leading_number(s: &str) -> String {
    let s = s.trim();
    // 中文序号前缀："一、" "十二、"
    if let Some(rest) = strip_chinese_ordinal(s) {
        return rest.to_string();
    }
    let mut chars = s.chars().peekable();
    let mut consumed = 0usize;
    let mut saw_digit = false;
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            saw_digit = true;
            chars.next();
            consumed += c.len_utf8();
        } else if c == '.' && saw_digit {
            chars.next();
            consumed += 1;
        } else if c == ' ' && saw_digit {
            chars.next();
            consumed += 1;
            break;
        } else {
            break;
        }
    }
    if saw_digit && consumed > 0 {
        let rest = &s[consumed..];
        if !rest.is_empty() {
            return rest.trim().to_string();
        }
    }
    s.to_string()
}

/// 剥除中文序号前缀："一、xxx" → "xxx"，"十二、xxx" → "xxx"。
/// 仅当 中文数字(≥1个) + 分隔符(、：:．.) 出现在开头时剥除，
/// 避免误伤 "三体问题" 这类以汉字数字开头但非序号的标题。
fn strip_chinese_ordinal(s: &str) -> Option<&str> {
    let mut consumed = 0usize;
    let mut n_digits = 0usize;
    for c in s.chars() {
        if matches!(c, '一' | '二' | '三' | '四' | '五' | '六' | '七' | '八' | '九' | '十' | '百' | '零' | '〇') {
            n_digits += 1;
            consumed += c.len_utf8();
        } else if n_digits > 0 && matches!(c, '、' | '：' | ':' | '．' | '.') {
            consumed += c.len_utf8();
            let rest = &s[consumed..];
            if !rest.trim().is_empty() {
                return Some(rest.trim_start());
            }
            return None; // 序号后没有内容，保守不剥
        } else {
            return None; // 数字后不是分隔符（如 "三体问题"），不剥
        }
    }
    None
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

/// 从笔记 Markdown 里取出并剥离开头的 `<!-- paperhelper-macros … -->` 宏定义注释。
/// 返回 `(宏原文, 去掉注释后的 Markdown)`；没有注释时原样返回。
pub fn take_macros_comment(md: &str) -> (Option<String>, String) {
    const START: &str = "<!-- paperhelper-macros";
    let t = md.trim_start();
    let Some(rest) = t.strip_prefix(START) else {
        return (None, md.to_string());
    };
    let Some(end) = rest.find("-->") else {
        return (None, md.to_string());
    };
    let raw = rest[..end].trim().to_string();
    let after = rest[end + 3..]
        .trim_start_matches(['\r', '\n', ' ', '\t'])
        .to_string();
    (if raw.is_empty() { None } else { Some(raw) }, after)
}

/// 解析 LLM 输出的 Markdown 为笔记树。
/// 约定：`#`=标题；`##`/`###`…=按层级嵌套的 Section；`$$…$$`=Formula；
/// 其余非空行=Paragraph（连续行合并为一段），挂到最近的 Section 下。
pub fn parse_markdown_note(md: &str, raw_text: &str) -> Note {
    let (macros, md) = take_macros_comment(md);
    let (mut title, mut roots) = parse_blocks_core(&md, true);

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
            number: String::new(),
            children: Vec::new(),
            explanations: Vec::new(),
        });
    }

    let mut note = Note {
        paper_id: String::new(),
        title,
        blocks: roots,
        raw_text: raw_text.to_string(),
        material_kind: String::new(),
        math_macros: macros,
    };
    assign_numbers(&mut note);
    note
}

/// 直接导入笔记/讲义时的解析：有 `#` 标题按现有规则建 Section 树；
/// 无标题则按空行切成多个 Paragraph（保证大纲/锚点/批注可用）。
/// 与 `parse_markdown_note` 不同：不剥开头的 ``` 围栏（保留代码块），raw_text 置空。
pub fn parse_import_note(md: &str, fallback_title: &str) -> Note {
    let (macros, md) = take_macros_comment(md);
    let has_heading = md.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with('#') && t.strip_prefix('#').is_some_and(|r| r.starts_with(' ') || r.starts_with('\t'))
    });
    let (title, mut roots) = if has_heading {
        parse_blocks_core_inner(&md, true)
    } else {
        (String::new(), split_into_paragraphs(&md))
    };
    if roots.is_empty() && !md.trim().is_empty() {
        roots.push(Block {
            id: uid(),
            kind: BlockKind::Paragraph,
            text: md.trim().to_string(),
            number: String::new(),
            children: Vec::new(),
            explanations: Vec::new(),
        });
    }
    let title = if title.trim().is_empty() { fallback_title.to_string() } else { title };
    let mut note = Note {
        paper_id: String::new(),
        title,
        blocks: roots,
        raw_text: String::new(),
        material_kind: String::new(),
        math_macros: macros,
    };
    assign_numbers(&mut note);
    note
}

/// 无标题文本：按空行切分为多个段落块。
fn split_into_paragraphs(md: &str) -> Vec<Block> {
    let mut out = Vec::new();
    for chunk in md.split("\n\n") {
        let t = chunk.trim();
        if t.is_empty() {
            continue;
        }
        out.push(Block {
            id: uid(),
            kind: BlockKind::Paragraph,
            text: t.to_string(),
            number: String::new(),
            children: Vec::new(),
            explanations: Vec::new(),
        });
    }
    out
}

/// 解析一段 Markdown 为块序列（供「插入内容 / 整节重写」使用）。
/// 与 `parse_markdown_note` 不同：`#` 也视为顶层 Section，而非被当成标题吞掉。
pub fn parse_markdown_blocks(md: &str) -> Vec<Block> {
    let (_title, roots) = parse_blocks_core(md, false);
    roots
}

/// 解析核心：返回 (标题, 顶层块)。`h1_as_title=true` 时 `#` 作标题（ingest 用）；
/// false 时 `#`/`##`/`###` 分别对应深度 0/1/2（插入/重写用）。
fn parse_blocks_core(md: &str, h1_as_title: bool) -> (String, Vec<Block>) {
    parse_blocks_core_inner(strip_fences(md), h1_as_title)
}

/// `parse_blocks_core` 的实现（不含剥围栏，供直接导入保留代码块）。
fn parse_blocks_core_inner(md: &str, h1_as_title: bool) -> (String, Vec<Block>) {
    let lines: Vec<&str> = md.lines().collect();

    let mut title = String::new();
    let mut roots: Vec<Block> = Vec::new();
    // 当前打开的各层 Section：(块, 标题层级)。层级用于判断新标题挂到哪一层。
    let mut stack: Vec<(Block, usize)> = Vec::new();
    let mut para_buf = String::new();

    let flush_para = |buf: &mut String, roots: &mut Vec<Block>, stack: &mut Vec<(Block, usize)>| {
        if !buf.trim().is_empty() {
            let block = Block {
                id: uid(),
                kind: BlockKind::Paragraph,
                text: buf.trim().to_string(),
                number: String::new(),
                children: Vec::new(),
                explanations: Vec::new(),
            };
            if let Some((sec, _)) = stack.last_mut() {
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
            let raw = trimmed.trim_start_matches('#').trim();
            let text = strip_leading_number(raw);
            flush_para(&mut para_buf, &mut roots, &mut stack);

            if level == 1 && h1_as_title {
                if title.is_empty() {
                    title = text;
                }
                i += 1;
                continue;
            }

            // 关闭所有层级 >= 当前标题的 Section（同层或更深的都收束为兄弟/上提）
            while let Some((_, l)) = stack.last() {
                if *l >= level {
                    let (sec, _) = stack.pop().unwrap();
                    if let Some((parent, _)) = stack.last_mut() {
                        parent.children.push(sec);
                    } else {
                        roots.push(sec);
                    }
                } else {
                    break;
                }
            }
            stack.push((
                Block {
                    id: uid(),
                    kind: BlockKind::Section,
                    text,
                    number: String::new(),
                    children: Vec::new(),
                    explanations: Vec::new(),
                },
                level,
            ));
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
                let target: Option<&mut Block> = if let Some((sec, _)) = stack.last_mut() {
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
    while let Some((sec, _)) = stack.pop() {
        if let Some((parent, _)) = stack.last_mut() {
            parent.children.push(sec);
        } else {
            roots.push(sec);
        }
    }

    (title, roots)
}

/// 给每个 Section 块按层级分配编号（如 "3.2"），写进 block.number。
/// Paragraph 不编号。在 parse 完成后调用一次，之后不变。
fn assign_numbers(note: &mut Note) {
    fn walk(blocks: &mut [Block], prefix: &[usize]) {
        let mut idx = 0;
        for b in blocks {
            idx += 1;
            if b.kind == BlockKind::Section {
                let mut num = prefix.to_vec();
                num.push(idx);
                b.number = num
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(".");
                walk(&mut b.children, &num);
            }
        }
    }
    walk(&mut note.blocks, &[]);
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
        let md = "# T\n## BERTScore\n对句子 r_i 用 BERTScore 计算相似度。\n## Other\n无关内容\n";
        let note = parse_markdown_note(md, "raw");
        let b = note.locate("BERTScore是什么").expect("应命中 BERTScore 块");
        assert!(b.text.contains("BERTScore"), "命中的块应含 BERTScore");
    }

    #[test]
    fn section_numbers_assigned() {
        let md = "# T\n## A\npara\n## B\n### B1\npara\n### B2\npara\n## C\npara\n";
        let note = parse_markdown_note(md, "raw");
        let secs: Vec<&Block> = note.flatten().iter().map(|(b, _)| *b).filter(|b| b.kind == BlockKind::Section).collect();
        let numbers: Vec<&str> = secs.iter().map(|s| s.number.as_str()).collect();
        assert!(numbers.contains(&"1"), "A 应为 1: {:?}", numbers);
        assert!(numbers.contains(&"2"), "B 应为 2: {:?}", numbers);
        assert!(numbers.contains(&"2.1"), "B1 应为 2.1: {:?}", numbers);
        assert!(numbers.contains(&"2.2"), "B2 应为 2.2: {:?}", numbers);
        assert!(numbers.contains(&"3"), "C 应为 3: {:?}", numbers);
        assert!(note.flatten().iter().all(|(b, _)| b.kind != BlockKind::Paragraph || b.number.is_empty()));
    }

    #[test]
    fn find_section_by_number_works() {
        let md = "# T\n## A\npara\n## B\n### B1\npara\n";
        let note = parse_markdown_note(md, "raw");
        let b = note.find_section_by_number("2.1").expect("应找到 2.1");
        assert!(b.text.contains("B1"));
        assert!(note.find_section_by_number("9.9").is_none());
    }

    #[test]
    fn strips_llm_leading_number() {
        // LLM 自带编号 "### 1.1 标题"，Rust 应去掉并用自己的编号
        let md = "# T\n## 1 一、问题\npara\n### 1.1 1.1 细节\npara\n";
        let note = parse_markdown_note(md, "raw");
        let secs: Vec<&Block> = note.flatten().iter().map(|(b, _)| *b).filter(|b| b.kind == BlockKind::Section).collect();
        for s in &secs {
            assert!(!s.text.starts_with("1."), "标题不应含 LLM 残留编号: {}", s.text);
            assert!(!s.text.starts_with("一、"), "中文序号也应剥除: {}", s.text);
        }
        assert!(secs.iter().any(|s| s.text == "问题"), "应剥成 '问题': {:?}", secs.iter().map(|s| &s.text).collect::<Vec<_>>());
    }

    #[test]
    fn strips_chinese_ordinal_prefix() {
        use super::{strip_chinese_ordinal, strip_leading_number};
        // 常见中文序号
        assert_eq!(strip_chinese_ordinal("一、要解决的问题"), Some("要解决的问题"));
        assert_eq!(strip_chinese_ordinal("十二、实验与分析"), Some("实验与分析"));
        assert_eq!(strip_chinese_ordinal("三：方法"), Some("方法"));
        // 非序号场景不误伤
        assert_eq!(strip_chinese_ordinal("三体问题"), None, "'三'后无分隔符不剥");
        assert_eq!(strip_chinese_ordinal("十维向量"), None);
        // 循环剥重复 "一、 二、"
        assert_eq!(strip_leading_number("一、 二、标题"), "标题");
        // 端到端：一级标题带中文序号，导出标题只剩 Rust 编号
        let md = "# T\n## 一、要解决的问题\npara\n## 三、本文方案\npara\n";
        let note = parse_markdown_note(md, "raw");
        let out = note.to_markdown();
        assert!(out.contains("## 1 要解决的问题"), "应为 '## 1 要解决的问题': {out}");
        assert!(out.contains("## 2 本文方案"), "应为 '## 2 本文方案': {out}");
        assert!(!out.contains("三、本文方案"), "不应残留中文序号: {out}");
    }

    #[test]
    fn formula_merges_into_preceding_paragraph() {
        let md = "# T\n## M\nThe entropy is defined as:\n\n$$\nH = -\\sum p \\ln p\n$$\n\nThis means high uncertainty.\n";
        let note = parse_markdown_note(md, "raw");
        let flat = note.flatten();
        let m_block = flat
            .iter()
            .find(|(b, _)| b.text.contains("entropy"))
            .expect("find entropy block");
        assert!(m_block.0.text.contains("H = -"), "公式应在同一块内: {}", m_block.0.text);
        assert!(m_block.0.text.contains("high uncertainty"), "后续段落也合并进同一块: {}", m_block.0.text);
        assert!(!flat.iter().any(|(b, _)| b.kind == BlockKind::Formula), "不应有独立 Formula 块");
    }

    #[test]
    fn explanations_attach_and_export() {
        let md = "# T\n## Intro\nWe propose a transformer.\n";
        let mut note = parse_markdown_note(md, "raw");
        let bid = note.locate("transformer").unwrap().id.clone();
        note.find_block_mut(&bid).unwrap().explanations.push(Explanation {
            id: "x".into(),
            question: "什么是transformer?".into(),
            answer: "一种基于注意力的模型。".into(),
            concept: "transformer".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            children: Vec::new(),
            summary: None,
            collapsed: false,
        });
        let md_out = crate::export::to_markdown(&note);
        assert!(md_out.contains("追问"), "export should include explanation: {md_out}");
        assert!(md_out.contains("transformer"));
        let mm = crate::export::to_mindmap(&note);
        assert!(mm.contains("# T"), "mindmap: {mm}");
    }

    #[test]
    fn nested_explanation_export() {
        // 顶层追问 + 嵌套子追问，验证导出时层级正确
        let md = "# T\n## M\nSome method.\n";
        let mut note = parse_markdown_note(md, "raw");
        let bid = note.locate("method").unwrap().id.clone();
        note.find_block_mut(&bid).unwrap().explanations.push(Explanation {
            id: "p1".into(),
            question: "这个方法是什么?".into(),
            answer: "是方法A。".into(),
            concept: "方法A".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            children: vec![Explanation {
                id: "c1".into(),
                question: "方法A的参数怎么调?".into(),
                answer: "用默认值。".into(),
                concept: "方法A参数".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
                children: Vec::new(),
                summary: None,
                collapsed: false,
            }],
            summary: None,
            collapsed: false,
        });
        let out = crate::export::to_markdown(&note);
        // 顶层用 > ，子层用 > >
        assert!(out.contains("> **追问**：这个方法是什么"), "应有顶层追问: {out}");
        assert!(out.contains("> > **追问**：方法A的参数怎么调"), "应有嵌套追问: {out}");
        // 父子连续：父亲解答行后紧跟父级空引用行再接儿子（无裸空行）
        let after: String = out.find("是方法A。")
            .and_then(|p| out[p..].find('\n').map(|n| out[p + n..].chars().take(6).collect()))
            .unwrap_or_default();
        assert!(after.starts_with("\n>\n> >"), "父子应以父级空引用行连续衔接: {after:?}\n{out}");
    }

    #[test]
    fn collapsed_explanation_renders_details_and_summary() {
        // sum 折叠：子树包进 <details>，总结在折叠块外
        let md = "# T\n## M\nSome method.\n";
        let mut note = parse_markdown_note(md, "raw");
        let bid = note.locate("method").unwrap().id.clone();
        note.find_block_mut(&bid).unwrap().explanations.push(Explanation {
            id: "p".into(),
            question: "n-gram是什么".into(),
            answer: "父答。".into(),
            concept: "c".into(),
            created_at: "t".into(),
            children: vec![Explanation {
                id: "c1".into(),
                question: "子问".into(),
                answer: "子答".into(),
                concept: "c".into(),
                created_at: "t".into(),
                children: vec![],
                summary: None,
                collapsed: false,
            }],
            summary: Some("总结内容：Max(-log p) 最有效。".into()),
            collapsed: true,
        });
        let out = crate::export::to_markdown(&note);
        assert!(out.contains("<details>"), "应有折叠开始: {out}");
        assert!(out.contains("</details>"), "应有折叠结束: {out}");
        assert!(out.contains("已概括，点击展开"), "应有提示: {out}");
        // 子树在折叠块内
        let d_open = out.find("<details>").unwrap();
        let d_close = out.find("</details>").unwrap();
        let child_pos = out.find("**追问**：子问").unwrap();
        assert!(d_open < child_pos && child_pos < d_close, "子树应在 details 内");
        // 总结在折叠块外
        let sum_pos = out.find("**总结**：总结内容").unwrap();
        assert!(sum_pos > d_close, "总结应在 details 外: {sum_pos} vs {d_close}");
    }

    #[test]
    fn explanation_ancestor_skips_check() {
        // check 节点（explanation_id=None）的儿子向上找父时应跳过 check
        use crate::conversation::{Conversation, ConvNode};
        let mk = |id: &str, parent: Option<&str>, expl: Option<&str>| ConvNode {
            id: id.into(),
            parent: parent.map(String::from),
            question: format!("Q{id}"),
            quote: None,
            answer: format!("A{id}"),
            block_id: None,
            explanation_id: expl.map(String::from),
            input_tokens: 0,
            output_tokens: 0,
            cost: 0.0,
            created_at: "t".into(),
            label: format!("L{id}"),
        };
        let nodes = vec![
            mk("root", None, None),
            mk("ask1", Some("root"), Some("e1")),
            mk("chk", Some("ask1"), None),       // check 节点
            mk("ask2", Some("chk"), None),       // check 的儿子（ask）
        ];
        // 从 ask2 向上：跳过 chk，命中 ask1 的 e1
        assert_eq!(Conversation::explanation_ancestor(&nodes, "ask2").as_deref(), Some("e1"));
        // 从 chk 向上：命中 ask1 的 e1
        assert_eq!(Conversation::explanation_ancestor(&nodes, "chk").as_deref(), Some("e1"));
        // 从 root：无
        assert_eq!(Conversation::explanation_ancestor(&nodes, "root"), None);
    }

    #[test]
    fn summary_rendered_inside_explanation() {
        // sum 生成的总结应渲染为追问块内的 "**总结**：…"，紧随解答
        let md = "# T\n## M\nSome method.\n";
        let mut note = parse_markdown_note(md, "raw");
        let bid = note.locate("method").unwrap().id.clone();
        note.find_block_mut(&bid).unwrap().explanations.push(Explanation {
            id: "p".into(),
            question: "父".into(),
            answer: "父答".into(),
            concept: "c".into(),
            created_at: "t".into(),
            children: vec![],
            summary: Some("n-gram 用采样频率近似概率，Max(-log p) 最有效。".into()),
            collapsed: false,
        });
        let out = crate::export::to_markdown(&note);
        assert!(out.contains("**总结**：n-gram"), "应渲染总结: {out}");
        // 总结在解答之后、同级前缀（> **总结**）
        let ia = out.find("**解答**：父答").expect("解答");
        let is = out.find("**总结**：n-gram").expect("总结");
        assert!(ia < is, "总结应在解答后");
        assert!(out[..is].ends_with(">\n> "), "总结应与解答同级连续: {}",
            out[..is].chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<String>());
    }

    #[test]
    fn sibling_explanations_separated() {        // 同一父亲下的两个儿子之间应有裸空行断开
        let md = "# T\n## M\nSome method.\n";
        let mut note = parse_markdown_note(md, "raw");
        let bid = note.locate("method").unwrap().id.clone();
        note.find_block_mut(&bid).unwrap().explanations.push(Explanation {
            id: "p".into(),
            question: "父".into(),
            answer: "父答".into(),
            concept: "c".into(),
            created_at: "t".into(),
            children: vec![
                Explanation { id: "c1".into(), question: "儿1".into(), answer: "答1".into(), concept: "c".into(), created_at: "t".into(), children: vec![], summary: None, collapsed: false },
                Explanation { id: "c2".into(), question: "儿2".into(), answer: "答2".into(), concept: "c".into(), created_at: "t".into(), children: vec![], summary: None, collapsed: false },
            ],
            summary: None,
            collapsed: false,
        });
        let out = crate::export::to_markdown(&note);
        let i1 = out.find("**追问**：儿1").expect("儿1");
        let i2 = out.find("**追问**：儿2").expect("儿2");
        let between = &out[i1..i2];
        assert!(between.contains("\n\n> > "), "兄弟之间应有裸空行断开: {between:?}\n{out}");
    }

    #[test]
    fn convert_inline_math_delims_works() {
        use crate::export::convert_inline_math_delims;
        // 行内 \( \) 与块级 \[ \] 都转成 $ / $$
        let md = "行内 \\(E=mc^2\\) 公式，块级：\n\\[H = -\\sum p\\log p\\]\n";
        let out = convert_inline_math_delims(md);
        assert!(out.contains("$E=mc^2$"), "{out}");
        assert!(out.contains("$$H = -\\sum p\\log p$$"), "{out}");
        // 代码块内不转换
        let md2 = "```\ncode \\(x\\) here\n```\n";
        let out2 = convert_inline_math_delims(md2);
        assert!(out2.contains("code \\(x\\) here"), "{out2}");
    }

    #[test]
    fn html_export_contains_note_and_tree() {
        use crate::conversation::{Conversation, ConvNode};
        let md = "# T\n## M\nSome method with $E=mc^2$.\n";
        let note = parse_markdown_note(md, "raw");
        let mut conv = Conversation::default();
        conv.add_exchange(ConvNode {
            id: "n1".into(),
            parent: None,
            question: "Q".into(),
            quote: None,
            answer: "A".into(),
            block_id: None,
            explanation_id: None,
            input_tokens: 10,
            output_tokens: 20,
            cost: 0.001,
            created_at: "t".into(),
            label: "概念X".into(),
        });
        conv.current = Some("n1".into());
        let html = crate::export::to_html(&note, &conv, &[]);
        // 结构完整
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("katex"), "应引 KaTeX");
        assert!(html.contains("marked"), "应引 marked");
        // 标题与树节点（label 转义后）
        assert!(html.contains("对话轨迹"));
        assert!(html.contains("概念X"), "树应含节点 label");
        assert!(html.contains("class=\"current\""), "当前节点应高亮");
        assert!(html.contains("10→20tok"), "应显示 token");
        // markdown 以 JS 字符串嵌入
        assert!(html.contains("const MD = "));
        // render_for 按扩展名分流
        assert!(crate::export::render_for("a.html", &note, &conv, &[]).contains("<!DOCTYPE"));
        assert!(crate::export::render_for("a.md", &note, &conv, &[]).contains("# T"));
    }

    #[test]
    fn html_export_protects_math_from_markdown() {
        // 多行 $$ 公式含下划线/小于号，且被嵌套引用（> 前缀）包裹：
        // 必须先把公式抽成占位符再渲染 Markdown，最后用 katex.renderToString 回填，
        // 否则 marked 会把 _ 配对成 <em>、把 < 当 HTML 标签，破坏 LaTeX。
        let md = "# T\n## M\n> > $$\n> > S_{\\text{n-gram}}^{\\text{Avg}}(i) = -\\frac{1}{J}\\sum_{j}\\log \\tilde p_{ij}\n> > $$\n";
        let note = parse_markdown_note(md, "raw");
        let conv = crate::conversation::Conversation::default();
        let html = crate::export::to_html(&note, &conv, &[]);
        assert!(html.contains("marked.parse(src)"), "应先渲染 markdown");
        assert!(html.contains("katex.renderToString"), "应用 KaTeX 回填公式");
        assert!(html.contains("store.push"), "应抽取公式占位符");
    }

    #[test]
    fn find_explanation_mut_works() {
        let md = "# T\n## M\nSome method.\n";
        let mut note = parse_markdown_note(md, "raw");
        let bid = note.locate("method").unwrap().id.clone();
        note.find_block_mut(&bid).unwrap().explanations.push(Explanation {
            id: "root_expl".into(),
            question: "Q1".into(),
            answer: "A1".into(),
            concept: "C1".into(),
            created_at: "t".into(),
            children: vec![Explanation {
                id: "child_expl".into(),
                question: "Q2".into(),
                answer: "A2".into(),
                concept: "C2".into(),
                created_at: "t".into(),
                children: Vec::new(),
                summary: None,
                collapsed: false,
            }],
            summary: None,
            collapsed: false,
        });
        // 找到子解释并修改
        let found = note.find_explanation_mut("child_expl").unwrap();
        assert_eq!(found.question, "Q2");
        // 找不到
        assert!(note.find_explanation_mut("nonexistent").is_none());
    }

    #[test]
    fn remove_explanation_removes_subtree() {
        let md = "# T\n## M\nSome method.\n";
        let mut note = parse_markdown_note(md, "raw");
        let bid = note.locate("method").unwrap().id.clone();
        note.find_block_mut(&bid).unwrap().explanations.push(Explanation {
            id: "p".into(),
            question: "父".into(),
            answer: "答".into(),
            concept: "c".into(),
            created_at: "t".into(),
            children: vec![Explanation {
                id: "c1".into(),
                question: "子".into(),
                answer: "答".into(),
                concept: "c".into(),
                created_at: "t".into(),
                children: Vec::new(),
                summary: None,
                collapsed: false,
            }],
            summary: None,
            collapsed: false,
        });
        let removed = note.remove_explanation("p").expect("应删除父解释");
        assert_eq!(removed.children.len(), 1, "返回值应含子树");
        assert!(note.find_explanation_mut("p").is_none());
        assert!(note.find_explanation_mut("c1").is_none(), "子解释应一并删除");
        assert!(note.remove_explanation("nope").is_none());
    }

    #[test]
    fn html_export_bare_has_no_tree() {
        let md = "# T\n## M\nSome method.\n";
        let note = parse_markdown_note(md, "raw");
        let conv = crate::conversation::Conversation::default();
        let full = crate::export::to_html(&note, &conv, &[]);
        let bare = crate::export::to_html_bare(&note, &conv, &std::collections::HashSet::new());
        assert!(full.contains("<aside>"), "完整导出应含对话树侧栏");
        assert!(!bare.contains("<aside>"), "bare 导出不应含树侧栏");
        assert!(bare.contains("const MD = "), "bare 仍应含笔记正文");
    }

    #[test]
    fn html_injects_anchors_but_markdown_does_not() {
        let md = "# T\n## M\nSome method.\n";
        let mut note = parse_markdown_note(md, "raw");
        let bid = note.locate("method").unwrap().id.clone();
        note.find_block_mut(&bid).unwrap().explanations.push(Explanation {
            id: "e1".into(),
            question: "Q".into(),
            answer: "A".into(),
            concept: "c".into(),
            created_at: "t".into(),
            children: Vec::new(),
            summary: None,
            collapsed: false,
        });
        let conv = crate::conversation::Conversation::default();
        let html = crate::export::to_html_bare(&note, &conv, &std::collections::HashSet::new());
        assert!(html.contains("expl-e1"), "HTML 应含解释锚点: ");
        let md_out = crate::export::to_markdown(&note);
        assert!(!md_out.contains("expl-e1"), "Markdown 导出不应含锚点");
    }

    #[test]
    fn set_text_updates_block_and_title() {
        let md = "# T\n## A\nold text\n";
        let mut note = parse_markdown_note(md, "raw");
        let para = note.locate("old").unwrap().id.clone();
        assert!(note.set_text(&para, "new text"));
        assert_eq!(note.locate("new").unwrap().text, "new text");
        assert!(note.set_text(TITLE_ID, "NewTitle"));
        assert_eq!(note.title, "NewTitle");
        assert!(!note.set_text("no-such-id", "x"));
    }

    #[test]
    fn block_markdown_renders_subtree_and_reparses() {
        let md = "# T\n## A\n正文A\n### A1\n正文A1\n## B\n正文B\n";
        let note = parse_markdown_note(md, "raw");
        let a = note.find_section_by_number("1").unwrap().id.clone();
        let s = note.block_markdown(&a).unwrap();
        assert!(s.starts_with("## A"), "{s}");
        assert!(s.contains("正文A"));
        assert!(s.contains("### A1"));
        assert!(s.contains("正文A1"));
        assert!(!s.contains("正文B"), "不应包含其他章节");
        // 渲染结果应能被重新解析回结构（供整节重写）
        let blocks = parse_markdown_blocks(&s);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "A");
        assert_eq!(blocks[0].children.len(), 2);
        assert!(note.block_markdown("nope").is_none());
    }

    #[test]
    fn parse_markdown_blocks_treats_h1_as_section() {
        let blocks = parse_markdown_blocks("# A\n## B\ntext\n");
        assert_eq!(blocks.len(), 1, "h1 应成为顶层 Section");
        assert_eq!(blocks[0].text, "A");
        assert_eq!(blocks[0].kind, BlockKind::Section);
        assert_eq!(blocks[0].children.len(), 1);
        assert_eq!(blocks[0].children[0].text, "B");
    }

    #[test]
    fn parse_markdown_blocks_same_level_siblings() {
        // 内容以 ### 开头（没有 ## 父级）时，同层标题应互为兄弟而非嵌套
        let blocks = parse_markdown_blocks("### A\n文本A\n### B\n文本B\n");
        assert_eq!(blocks.len(), 2, "同层标题应为兄弟");
        assert_eq!(blocks[0].text, "A");
        assert_eq!(blocks[1].text, "B");
    }

    #[test]
    fn insert_blocks_after_places_siblings() {
        let md = "# T\n## A\none\n## B\ntwo\n";
        let mut note = parse_markdown_note(md, "raw");
        let a = note.locate("one").unwrap().id.clone();
        // 注意：段落与 section 同属 roots，插入到段落之后
        let new_blocks = parse_markdown_blocks("### New\nfresh\n");
        assert!(note.insert_blocks_after(&a, new_blocks));
        let flat = note.flatten();
        let idx_a = flat.iter().position(|(b, _)| b.id == a).unwrap();
        assert_eq!(flat[idx_a + 1].0.text, "New");
        assert!(flat[idx_a + 2].0.text.contains("fresh"));
        assert!(!note.insert_blocks_after("nope", vec![]));
    }

    #[test]
    fn replace_children_swaps_section_body_and_renumbers() {
        let md = "# T\n## A\nold\n## B\ntwo\n";
        let mut note = parse_markdown_note(md, "raw");
        let a = note.find_section_by_number("1").unwrap().id.clone();
        let new_children = parse_markdown_blocks("### 子节\n新的内容\n");
        assert!(note.replace_children(&a, new_children));
        let a_block = note.find_block(&a).unwrap();
        assert_eq!(a_block.children.len(), 1);
        assert_eq!(a_block.children[0].text, "子节");
        assert!(a_block.children[0]
            .children
            .iter()
            .any(|c| c.text.contains("新的内容")));
        // 段落块不能替换 children
        let para = note.locate("two").unwrap().id.clone();
        assert!(!note.replace_children(&para, vec![]));
        note.renumber();
        assert_eq!(note.find_section_by_number("1.1").unwrap().text, "子节");
    }

    #[test]
    fn remove_block_removes_subtree() {
        let md = "# T\n## A\n### A1\naaaa\n### A2\nbbbb\n## B\ncccc\n";
        let mut note = parse_markdown_note(md, "raw");
        let a = note.find_section_by_number("1").unwrap().id.clone();
        assert!(note.remove_block(&a));
        assert!(note.find_block(&a).is_none());
        assert!(note.locate("aaaa").is_none(), "子树应一并删除");
        assert!(note.locate("cccc").is_some(), "兄弟节点保留");
        assert!(!note.remove_block("nope"));
        note.renumber();
        assert_eq!(note.find_section_by_number("1").unwrap().text, "B");
    }

    #[test]
    fn import_note_headingless_splits_paragraphs() {
        let note = parse_import_note("第一段\n\n第二段\n\n第三段", "讲义");
        assert_eq!(note.title, "讲义");
        assert_eq!(note.blocks.len(), 3);
        assert!(note.blocks.iter().all(|b| b.kind == BlockKind::Paragraph));
        assert!(note.raw_text.is_empty(), "直接导入的笔记 raw_text 应为空");
    }

    #[test]
    fn import_note_with_headings_builds_sections() {
        let md = "# 标题\n## 第一章\n内容 A\n\n## 第二章\n内容 B\n";
        let note = parse_import_note(md, "fallback");
        assert_eq!(note.title, "标题");
        assert_eq!(note.blocks.len(), 2);
        assert_eq!(note.blocks[0].kind, BlockKind::Section);
        assert_eq!(note.blocks[0].text, "第一章");
    }

    #[test]
    fn import_note_keeps_leading_code_fence() {
        let md = "```python\nprint(1)\n```\n\n后面的说明";
        let note = parse_import_note(md, "代码");
        assert_eq!(note.title, "代码");
        assert!(
            note.blocks.iter().any(|b| b.text.contains("```python")),
            "直接导入应保留代码围栏，不应被剥掉"
        );
    }

    #[test]
    fn import_note_extracts_macros_comment() {
        let md = "<!-- paperhelper-macros\n\\newcommand{\\ket}[1]{\\vert#1\\rangle}\n-->\n\n# 讲义\n\n正文 $\\ket{0}$";
        let note = parse_import_note(md, "fallback");
        assert_eq!(note.title, "讲义");
        assert!(
            note.math_macros.as_deref().unwrap_or("").contains("\\ket"),
            "宏定义应被抽出保存"
        );
        assert_eq!(note.blocks.len(), 1, "宏注释不应变成内容块");
        assert!(!note.blocks[0].text.contains("paperhelper-macros"));
    }

    #[test]
    fn take_macros_comment_without_comment_is_noop() {
        let (m, md) = take_macros_comment("# T\n\nx");
        assert!(m.is_none());
        assert_eq!(md, "# T\n\nx");
    }
}
