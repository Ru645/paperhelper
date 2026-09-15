//! 提示词模板与笔记风格的落盘与加载。
//!
//! - 行为提示词（ask / rewrite）外置为 `.paperhelper/prompts/*.txt`，用户可直接编辑。
//! - **笔记风格**是一套「名字 + 说明 + 提示词模板（含 `{raw_text}`）」：
//!   清单在 `.paperhelper/styles.toml`，提示词在 `.paperhelper/styles/<id>.txt`；
//!   内置风格可编辑/恢复默认，用户可新建自定义风格（Web 界面或直接改文件）。
//! - `ask.txt` 约定 LLM 用 `[[概念: 名字]]` 行回报核心概念，是知识库提取概念的接口协议。

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::paths;

/// 提示词模板目录：.paperhelper/prompts/
/// 文件存在则用户自定义生效；不存在则用内置默认并写出默认文件供用户编辑。
pub fn prompts_dir() -> std::path::PathBuf {
    paths::data_dir().join("prompts")
}

/// 首次启动时把默认提示词模板写到 .paperhelper/prompts/，供用户编辑。
/// （笔记风格已迁到 `styles/`，见 `ensure_styles`。）
pub fn ensure_prompt_files() -> Result<()> {
    let dir = prompts_dir();
    if !dir.exists() {
        fs::create_dir_all(&dir)?;
    }
    let defaults: &[(&str, &str)] = &[
        ("ask.txt", DEFAULT_ASK_PROMPT),
        ("rewrite.txt", DEFAULT_REWRITE_PROMPT),
    ];
    for (name, content) in defaults {
        let p = dir.join(name);
        if !p.exists() {
            fs::write(&p, content)?;
        }
    }
    Ok(())
}

/// 加载提示词：若 dir 下存在 name 则读用户自定义，否则返回内置默认。
pub fn load_prompt(dir: &Path, name: &str, default: &str) -> String {
    let p = dir.join(name);
    match fs::read_to_string(&p) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => default.to_string(),
    }
}

pub const DEFAULT_ASK_PROMPT: &str = "你是一位耐心的学习助手（适用于论文、课程讲义等资料）。用户会给你一份资料的原文（可能没有）、已生成的结构化笔记，以及（可能的）历史问答。请基于这些回答用户问题，简洁清晰（300字以内），尽量和笔记的章节结构对齐。若涉及已学概念，点明它们的联系。

**数学公式必须用定界符包裹**：行内公式（变量、下标/上标、符号、希腊字母、LaTeX 命令）一律写成 `$...$`，如 `$p_{ij}$`、`$S_n$`、`$\\tilde p_{ij}$`；行间公式写成 `$$...$$`。不要写裸的 `p_ij` 或 `\\sum`。

回答完毕后，另起一行写 [[概念: 概念名]]，概念名是1-8个词的短语，概括本次问答涉及的核心知识点（如\"BERTScore\"、\"MQAG框架\"、\"语义熵\"）。";

/// 笔记生成模板：{raw_text} 会被替换为论文全文。
pub const DEFAULT_NOTE_PROMPT: &str = r#"请阅读以下论文全文，生成一份**详细**的学习笔记 Markdown，遵循固定四段架构。

架构与分块规则：
- 第一行 `# 论文标题`。
- 用四个一级章节 `## 一、要解决的问题` `## 二、前人方案及其不足` `## 三、本文方案及其优点` `## 四、前景与发展方向`。
- 每个一级章节下，用 `###` 三级小标题细分。例如：
  - 「二、前人方案」下，每个前人方案一个 `###` 小标题，说清做法与不足；
  - 「三、本文方案」下，若论文提出多个方案/变体（如 5 种变体），**每个变体单独一个 `###` 小标题**，详细说明做法、公式、数据、直觉、优缺点；
  - 「四、前景」下，每个方向一个 `###` 小标题。
- 每个 `###` 小标题下的内容（含多段落、公式、表格）合并为一块，不要为每句话单独成块。
- **所有数学都必须用定界符包裹**：行内公式（变量、下标/上标、符号、希腊字母、LaTeX 命令）一律写成 `$...$`，例如 `$p_{ij}$`、`$S_n$`、`$s^n_k$`、`$\\tilde p_{ij}$`、`$\\sum_j$`、`$R$`、`$J$`；行间公式用 `$$...$$`。**绝不要**写成裸的 `p_ij`、`S_n` 或 `\\sum`。
- 要详细：保留论文中的关键公式、数值结果、对比表格（用 markdown 表格）、算法步骤。不要泛泛概括，要展开具体内容。
- 不要输出额外说明，直接给 Markdown。

论文全文：
{raw_text}"#;

/// 逐段翻译模板：忠实翻译、保留原文结构（单次整篇；输出被截断时会提示用户）。
pub const DEFAULT_TRANSLATE_PROMPT: &str = r#"请把以下资料**忠实翻译**成中文，生成一份「原文照搬式」的笔记 Markdown。

要求：
- 第一行输出 `# 标题`（标题翻译成中文）。
- 尽量忠实：逐段翻译，保留原文的章节结构、段落顺序与层级。原文的章节标题用 `##`/`###` 表示，不要合并、省略或重排。
- 公式保持原样（行内 `$...$`、行间 `$$...$$`）；模型名、数据集名、指标名等专有名词保留英文。
- 不要总结、不要发挥、不要添加原文没有的内容，也不要输出额外的说明文字。
- 直接输出完整 Markdown。

资料全文：
{raw_text}"#;

/// 自由笔记模板：不加结构约束，让模型自行组织。
pub const DEFAULT_FREE_PROMPT: &str = r#"请阅读以下资料，生成一份你认为最有帮助的学习笔记 Markdown。结构、详略、排版都由你决定；若材料有清晰章节，建议沿用，以便对照原文。

资料全文：
{raw_text}"#;

/// 忠实照抄模板：内容与顺序保持原样，只做 Markdown 结构化。
pub const DEFAULT_VERBATIM_PROMPT: &str = r#"请把以下资料整理成一份**忠实照抄式**的 Markdown 笔记：内容与顺序尽量保持原样，只做必要的结构化。

要求：
- 第一行输出 `# 标题`（优先用资料自身的标题；没有就用文件名的意思）。
- 保留原文的章节结构：章节标题用 `##`/`###` 表示，不要合并、省略或重排；段落文字不删改、不总结、不发挥。
- 所有数学公式都要用定界符包裹：行内 `$...$`、行间 `$$...$$`（原文若写成裸的 `p_ij`、`\sum` 也要补上）。
- 表格转成 Markdown 表格；图表位置用一句话说明占位（如 `![图：…](图)`），不要凭空编造内容。
- 扫描/OCR 可能有错字：只修正明显的断行、连字符与乱码，不做语义改写。
- 不要输出额外说明，直接给 Markdown。

资料全文：
{raw_text}"#;

/// 讲义提纲模板：按知识点分节，适合课程讲义/幻灯片。
pub const DEFAULT_LECTURE_PROMPT: &str = r#"请阅读以下课程讲义/教学材料，生成一份**复习提纲式**的学习笔记 Markdown。

要求：
- 第一行 `# 讲义标题`（用材料标题或文件名的意思）。
- 用 `##` 按**知识点/主题**分节（不要按页码分）；每个知识点下用 `###` 细分（定义、定理/公式、推导、例子、易错点），按材料实际内容取舍。
- 每个 `###` 小标题下的内容（含多段落、公式、表格）合并为一块，不要为每句话单独成块。
- 保留关键定义、定理、公式与推导、例题结论；省略寒暄、课程通知与重复内容。
- 数学公式一律用 `$...$` / `$$...$$`；表格用 Markdown 表格。
- 结尾加一节 `## 复习提纲`，用要点列出需要掌握的概念与题型。
- 不要输出额外说明，直接给 Markdown。

讲义全文：
{raw_text}"#;

/// 中英对照翻译模板。
pub const DEFAULT_TRANSLATE_BI_PROMPT: &str = r#"请把以下资料**逐段翻译**成中文，生成一份「原文 + 译文」对照的 Markdown 笔记。

要求：
- 第一行输出 `# 标题`（中英标题都写）。
- 保留原文的章节结构：章节标题用 `##`/`###`；每个段落先给原文、再给译文（译文用 `> ` 引用块，或另起一段，全文保持一致）。
- 公式保持原样（行内 `$...$`、行间 `$$...$$`），公式本身不翻译。
- 专有名词保留英文（首次出现可在括号内注中文）；不要总结、不要发挥、不要添加原文没有的内容。
- 不要输出额外说明，直接给 Markdown。

资料全文：
{raw_text}"#;

/// AI 改写 / 补充模板。占位符：{paper} {note} {target} {instruction} {task}
pub const DEFAULT_REWRITE_PROMPT: &str = r#"你是一位论文笔记编辑助手。用户会给你论文全文、当前笔记，以及要处理的笔记片段，请按用户要求完成编辑。

要求：
- 只输出 Markdown 正文，不要解释、不要前言、不要用代码块围栏包裹。
- 数学公式用 `$...$` / `$$...$$`；小节标题用 `##`/`###`（不要用 `#`，`#` 是整篇笔记标题）。
- 与当前笔记的风格、术语、详略保持一致。

【论文全文】
{paper}

【当前笔记】
{note}

【待处理的片段】
{target}

【用户要求】
{instruction}

【任务】
{task}

直接输出结果 Markdown："#;

// ===== 笔记风格注册表：.paperhelper/styles.toml + styles/<id>.txt =====

/// 一种笔记风格：生成笔记时使用的提示词模板（含 `{raw_text}` 占位符）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteStyle {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub desc: String,
    /// 提示词文件名（styles 目录下）；缺省为 `<id>.txt`。
    #[serde(default)]
    pub file: String,
    /// 内置风格可「恢复默认」，不可删除。
    #[serde(default)]
    pub builtin: bool,
    /// 适用范围：paper / note / any。
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "any".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StylesFile {
    #[serde(default)]
    style: Vec<NoteStyle>,
}

/// 风格目录：.paperhelper/styles/
pub fn styles_dir() -> PathBuf {
    paths::data_dir().join("styles")
}

fn styles_toml_path() -> PathBuf {
    paths::data_dir().join("styles.toml")
}

fn style_file_name(s: &NoteStyle) -> String {
    let f = s.file.trim();
    if f.is_empty() {
        format!("{}.txt", s.id)
    } else {
        f.to_string()
    }
}

/// 内置风格：(元信息, 默认提示词)。顺序即界面展示顺序。
fn builtin_styles() -> Vec<(NoteStyle, &'static str)> {
    let mk = |id: &str, label: &str, desc: &str, scope: &str| NoteStyle {
        id: id.to_string(),
        label: label.to_string(),
        desc: desc.to_string(),
        file: format!("{id}.txt"),
        builtin: true,
        scope: scope.to_string(),
    };
    vec![
        (
            mk("four", "四段式", "问题 / 前人方案 / 本文方案 / 前景", "paper"),
            DEFAULT_NOTE_PROMPT,
        ),
        (
            mk(
                "translate",
                "逐段翻译",
                "忠实翻译成中文（只译文）；单次整篇，长文可能被截断",
                "any",
            ),
            DEFAULT_TRANSLATE_PROMPT,
        ),
        (
            mk(
                "translate-bi",
                "中英对照",
                "逐段给原文 + 译文（笔记体积翻倍）",
                "any",
            ),
            DEFAULT_TRANSLATE_BI_PROMPT,
        ),
        (
            mk(
                "verbatim",
                "忠实照抄",
                "保留原文内容与顺序，只做标题/公式/表格结构化，不总结不删改",
                "any",
            ),
            DEFAULT_VERBATIM_PROMPT,
        ),
        (
            mk(
                "lecture",
                "讲义提纲",
                "按知识点分节：定义/定理/例题/易错点 + 复习清单",
                "note",
            ),
            DEFAULT_LECTURE_PROMPT,
        ),
        (mk("free", "自由笔记", "不加结构约束，由模型自行组织", "any"), DEFAULT_FREE_PROMPT),
    ]
}

/// 准备风格目录：迁移旧的 `prompts/{note,translate,free}.txt`（用户改动不丢）、
/// 补齐内置风格与提示词文件、写回 `styles.toml`。启动时调用。
pub fn ensure_styles() -> Result<()> {
    let dir = styles_dir();
    if !dir.exists() {
        fs::create_dir_all(&dir)?;
    }
    let path = styles_toml_path();
    let mut file: StylesFile = match fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => toml::from_str(&s).unwrap_or_default(),
        _ => StylesFile::default(),
    };
    // 迁移：旧的笔记提示词文件 → styles/<id>.txt
    let legacy: &[(&str, &str)] = &[
        ("four", "note.txt"),
        ("translate", "translate.txt"),
        ("free", "free.txt"),
    ];
    for (id, old) in legacy {
        let dest = dir.join(format!("{id}.txt"));
        if !dest.exists() {
            if let Ok(s) = fs::read_to_string(prompts_dir().join(old)) {
                if !s.trim().is_empty() {
                    let _ = fs::write(&dest, s);
                }
            }
        }
    }
    for (meta, default) in builtin_styles() {
        if !file.style.iter().any(|s| s.id == meta.id) {
            file.style.push(meta.clone());
        }
        let p = dir.join(style_file_name(&meta));
        if !p.exists() {
            fs::write(&p, default)?;
        }
    }
    // 自定义风格缺文件时补空文件，避免界面读取失败
    for s in &file.style {
        let p = dir.join(style_file_name(s));
        if !p.exists() {
            fs::write(&p, "")?;
        }
    }
    let out = toml::to_string_pretty(&file)?;
    if fs::read_to_string(&path).unwrap_or_default() != out {
        fs::write(&path, out)?;
    }
    Ok(())
}

fn read_styles_file() -> Result<StylesFile> {
    ensure_styles()?;
    let raw = fs::read_to_string(styles_toml_path()).unwrap_or_default();
    Ok(toml::from_str(&raw).unwrap_or_default())
}

/// 列出全部风格（清单顺序）。
pub fn list_styles() -> Result<Vec<NoteStyle>> {
    Ok(read_styles_file()?.style)
}

/// 按 id 取风格与提示词内容；id 为空按 `four`。未知 id 报错并列出可用风格。
pub fn style_prompt(style_id: &str) -> Result<(NoteStyle, String)> {
    let id = if style_id.trim().is_empty() {
        "four"
    } else {
        style_id.trim()
    };
    let styles = list_styles()?;
    let Some(meta) = styles.iter().find(|s| s.id == id).cloned() else {
        let ids: Vec<String> = styles.iter().map(|s| s.id.clone()).collect();
        bail!("未知的笔记风格 `{id}`（可用：{}）", ids.join(" / "));
    };
    Ok((meta.clone(), style_prompt_text(&meta)))
}

/// 读取某风格的提示词内容（文件缺失/为空时回落内置默认）。
pub fn style_prompt_text(meta: &NoteStyle) -> String {
    let p = styles_dir().join(style_file_name(meta));
    let text = fs::read_to_string(&p).unwrap_or_default();
    if text.trim().is_empty() {
        if let Some((_, d)) = builtin_styles().into_iter().find(|(m, _)| m.id == meta.id) {
            return d.to_string();
        }
    }
    text
}

/// 风格 id 合法性：1~40 个 ASCII 字母/数字/`-`/`_`，且以字母或数字开头。
pub fn valid_style_id(id: &str) -> bool {
    let id = id.trim();
    if id.is_empty() || id.len() > 40 {
        return false;
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphanumeric()) {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 保存（新建或更新）一个风格：写清单 + 提示词文件。
/// 内置风格的 id 也允许更新（改名称/说明/提示词），但不能改 id/删除。
pub fn save_style(meta: &NoteStyle, prompt: &str) -> Result<()> {
    let id = meta.id.trim();
    if !valid_style_id(id) {
        bail!("风格 id 不合法（1~40 个字母/数字/-/_，且以字母或数字开头）");
    }
    if meta.label.trim().is_empty() {
        bail!("风格名称不能为空");
    }
    ensure_styles()?;
    let mut file = read_styles_file()?;
    let builtin = builtin_styles().into_iter().find(|(m, _)| m.id == id).map(|(m, _)| m);
    let entry = match file.style.iter_mut().find(|s| s.id == id) {
        Some(e) => e,
        None => {
            file.style.push(NoteStyle {
                id: id.to_string(),
                label: String::new(),
                desc: String::new(),
                file: format!("{id}.txt"),
                builtin: false,
                scope: "any".to_string(),
            });
            file.style.last_mut().unwrap()
        }
    };
    entry.label = meta.label.trim().to_string();
    entry.desc = meta.desc.trim().to_string();
    entry.scope = match meta.scope.trim() {
        "paper" => "paper".to_string(),
        "note" => "note".to_string(),
        _ => "any".to_string(),
    };
    if entry.builtin || builtin.is_some() {
        entry.builtin = true;
    }
    if entry.file.trim().is_empty() {
        entry.file = format!("{id}.txt");
    }
    let fname = style_file_name(entry);
    fs::write(styles_dir().join(fname), prompt)?;
    fs::write(styles_toml_path(), toml::to_string_pretty(&file)?)?;
    Ok(())
}

/// 删除自定义风格（内置不可删）。同时删除其提示词文件。
pub fn delete_style(id: &str) -> Result<()> {
    let id = id.trim();
    let mut file = read_styles_file()?;
    let Some(pos) = file.style.iter().position(|s| s.id == id) else {
        bail!("找不到风格 `{id}`");
    };
    if file.style[pos].builtin {
        bail!("内置风格不可删除（可改为「恢复默认」）");
    }
    let fname = style_file_name(&file.style[pos]);
    file.style.remove(pos);
    let _ = fs::remove_file(styles_dir().join(fname));
    fs::write(styles_toml_path(), toml::to_string_pretty(&file)?)?;
    Ok(())
}

/// 恢复内置风格的默认提示词与说明。
pub fn reset_style(id: &str) -> Result<()> {
    let id = id.trim();
    let Some((meta, default)) = builtin_styles().into_iter().find(|(m, _)| m.id == id) else {
        bail!("`{id}` 不是内置风格，无法恢复默认");
    };
    let mut file = read_styles_file()?;
    if let Some(e) = file.style.iter_mut().find(|s| s.id == id) {
        e.label = meta.label.clone();
        e.desc = meta.desc.clone();
        e.scope = meta.scope.clone();
        e.builtin = true;
    }
    fs::write(styles_dir().join(style_file_name(&meta)), default)?;
    fs::write(styles_toml_path(), toml::to_string_pretty(&file)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_styles_cover_core_ids() {
        let ids: Vec<String> = builtin_styles().into_iter().map(|(m, _)| m.id).collect();
        for want in ["four", "translate", "translate-bi", "verbatim", "lecture", "free"] {
            assert!(ids.contains(&want.to_string()), "缺少内置风格 {want}");
        }
        // 每个内置风格都有默认提示词且带 {raw_text} 占位符
        for (m, prompt) in builtin_styles() {
            assert!(prompt.contains("{raw_text}"), "风格 {} 的提示词缺少 {{raw_text}}", m.id);
        }
    }

    #[test]
    fn valid_style_id_rules() {
        assert!(valid_style_id("my-style_1"));
        assert!(valid_style_id("four"));
        assert!(!valid_style_id(""));
        assert!(!valid_style_id("中文"));
        assert!(!valid_style_id("-lead"));
        assert!(!valid_style_id("a/b"));
        assert!(!valid_style_id(&"a".repeat(41)));
    }
}
