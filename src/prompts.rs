//! 提示词模板与笔记风格的落盘与加载。
//!
//! - 行为提示词（ask / rewrite）外置为 `.paperhelper/prompts/*.txt`，用户可直接编辑。
//! - **笔记风格**是一套「名字 + 说明 + 提示词模板」：
//!   清单在 `.paperhelper/styles.toml`，提示词在 `.paperhelper/styles/<id>.txt`；
//!   内置风格可编辑/恢复默认，用户可新建自定义风格（Web 界面或直接改文件）。
//!   资料全文由 `compose_style_prompt` 固定附加，风格文本里不应出现 `{raw_text}`。
//! - `ask.txt` 约定 LLM 用 `[[概念: 名字]]` 行回报核心概念，是知识库提取概念的接口协议。

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::logging;
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
    // ask.txt：如果内容还是旧版默认（用户没改过），升级为“按需标注概念”的新版
    let ask = dir.join("ask.txt");
    if let Ok(cur) = fs::read_to_string(&ask) {
        if cur.trim() == LEGACY_ASK_PROMPT.trim() {
            let _ = fs::write(&ask, DEFAULT_ASK_PROMPT);
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

当这次回答确实围绕一个**明确的知识点/概念**时（如 BERTScore、语义熵、NP 完全），在末尾另起一行写 [[概念: 概念名]]，概念名是 1-8 个词的短语；如果只是操作性/指代性提问（如「这段是什么意思」「这里的符号指什么」），**不要**写这一行。";

/// 旧版 ask.txt 的默认内容：仅用于 `ensure_prompt_files` 判断用户是否改过，
/// 没改过就自动升级成新版（“按需标注概念”）。
const LEGACY_ASK_PROMPT: &str = "你是一位耐心的学习助手（适用于论文、课程讲义等资料）。用户会给你一份资料的原文（可能没有）、已生成的结构化笔记，以及（可能的）历史问答。请基于这些回答用户问题，简洁清晰（300字以内），尽量和笔记的章节结构对齐。若涉及已学概念，点明它们的联系。

**数学公式必须用定界符包裹**：行内公式（变量、下标/上标、符号、希腊字母、LaTeX 命令）一律写成 `$...$`，如 `$p_{ij}$`、`$S_n$`、`$\\tilde p_{ij}$`；行间公式写成 `$$...$$`。不要写裸的 `p_ij` 或 `\\sum`。

回答完毕后，另起一行写 [[概念: 概念名]]，概念名是1-8个词的短语，概括本次问答涉及的核心知识点（如\"BERTScore\"、\"MQAG框架\"、\"语义熵\"）。";

/// 笔记生成模板（资料全文由程序在 `compose_style_prompt` 末尾固定附加）。
pub const DEFAULT_NOTE_PROMPT: &str = r#"请阅读以下论文全文，生成一份**详细**的学习笔记 Markdown，遵循固定四段架构。

架构与分块规则：
- 用四个一级章节 `## 一、要解决的问题` `## 二、前人方案及其不足` `## 三、本文方案及其优点` `## 四、前景与发展方向`。
- 每个一级章节下，用 `###` 三级小标题细分。例如：
  - 「二、前人方案」下，每个前人方案一个 `###` 小标题，说清做法与不足；
  - 「三、本文方案」下，若论文提出多个方案/变体（如 5 种变体），**每个变体单独一个 `###` 小标题**，详细说明做法、公式、数据、直觉、优缺点；
  - 「四、前景」下，每个方向一个 `###` 小标题。
- 要详细：保留论文中的关键公式、数值结果、对比表格、算法步骤。不要泛泛概括，要展开具体内容。"#;

/// 逐段翻译模板：忠实翻译、保留原文结构（单次整篇；输出被截断时会提示用户）。
pub const DEFAULT_TRANSLATE_PROMPT: &str = r#"请把以下资料**忠实翻译**成中文，生成一份「原文照搬式」的笔记 Markdown。

要求：
- 标题翻译成中文。
- 尽量忠实：逐段翻译，保留原文的章节结构、段落顺序与层级，不要合并、省略或重排。
- 模型名、数据集名、指标名等专有名词保留英文；公式本身不翻译。
- 不要总结、不要发挥、不要添加原文没有的内容。"#;

/// 中英对照翻译模板。
pub const DEFAULT_TRANSLATE_BI_PROMPT: &str = r#"请把以下资料**逐段翻译**成中文，生成一份「原文 + 译文」对照的 Markdown 笔记。

要求：
- 标题中英都写。
- 保留原文的章节结构；每个段落先给原文、再给译文（译文用 `> ` 引用块，或另起一段，全文保持一致）。
- 公式本身不翻译；专有名词保留英文（首次出现可在括号内注中文）。
- 不要总结、不要发挥、不要添加原文没有的内容。"#;

/// 自由笔记模板：不加结构约束，让模型自行组织。
pub const DEFAULT_FREE_PROMPT: &str = r#"请阅读以下资料，生成一份你认为最有帮助的学习笔记 Markdown。结构、详略、排版都由你决定；若材料有清晰章节，建议沿用，以便对照原文。"#;

/// 忠实照抄模板：内容与顺序保持原样，只做 Markdown 结构化。
pub const DEFAULT_VERBATIM_PROMPT: &str = r#"请把以下资料整理成一份**忠实照抄式**的 Markdown 笔记：内容与顺序尽量保持原样，只做必要的结构化。

要求：
- 保留原文的章节结构：不要合并、省略或重排；段落文字不删改、不总结、不发挥。
- 图表位置用一句话说明占位（如 `![图：…](图)`），不要凭空编造内容。
- 扫描/OCR 可能有错字：只修正明显的断行、连字符与乱码，不做语义改写。"#;

/// 讲义提纲模板：按知识点分节，适合课程讲义/幻灯片。
pub const DEFAULT_LECTURE_PROMPT: &str = r#"请阅读以下课程讲义/教学材料，生成一份**复习提纲式**的学习笔记 Markdown。

要求：
- 用 `##` 按**知识点/主题**分节（不要按页码分）；每个知识点下用 `###` 细分（定义、定理/公式、推导、例子、易错点），按材料实际内容取舍。
- 保留关键定义、定理、公式与推导、例题结论；省略寒暄、课程通知与重复内容。
- 结尾加一节 `## 复习提纲`，用要点列出需要掌握的概念与题型。"#;

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

/// 一种笔记风格：生成笔记时使用的提示词模板（资料全文由程序固定附加）。
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

/// 旧版内置风格的默认提示词：仅用于启动时判断内置风格文件是否仍是
/// 「未被用户改过的旧默认」，是则升级为新的「只含内容要求」版
/// （格式要求已由 `STYLE_CONTRACT` 统一附加，不应出现在用户的编辑栏里）。
const LEGACY_BUILTIN_STYLE_DEFAULTS: &[(&str, &str)] = &[
    ("four", r#"请阅读以下论文全文，生成一份**详细**的学习笔记 Markdown，遵循固定四段架构。

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
{raw_text}"#),
    ("four", r#"请阅读以下论文全文，生成一份**详细**的学习笔记 Markdown，遵循固定四段架构。

架构与分块规则：
- 第一行 `# 论文标题`。
- 用四个一级章节 `## 一、要解决的问题` `## 二、前人方案及其不足` `## 三、本文方案及其优点` `## 四、前景与发展方向`。
- 每个一级章节下，用 `###` 三级小标题细分。例如：
  - 「二、前人方案」下，每个前人方案一个 `###` 小标题，说清做法与不足；
  - 「三、本文方案」下，若论文提出多个方案/变体（如 5 种变体），**每个变体单独一个 `###` 小标题**，详细说明做法、公式、数据、直觉、优缺点；
  - 「四、前景」下，每个方向一个 `###` 小标题。
- 每个 `###` 小标题下的内容（含多段落、公式、表格）合并为一块，不要为每句话单独成块。
- 关键公式用 `$$...$$` 包裹，直接写在所属段落里。
- 要详细：保留论文中的关键公式、数值结果、对比表格（用 markdown 表格）、算法步骤。不要泛泛概括，要展开具体内容。
- 不要输出额外说明，直接给 Markdown。

论文全文：
{raw_text}"#),
    ("translate", r#"请把以下论文**忠实翻译**成中文，生成一份「原文照搬式」的笔记 Markdown。

要求：
- 第一行输出 `# 论文标题`（标题翻译成中文）。
- 尽量忠实：逐段翻译，保留原文的章节结构、段落顺序与层级。原文的章节标题用 `##`/`###` 表示，不要合并、省略或重排。
- 公式保持原样（行内 `$...$`、行间 `$$...$$`）；模型名、数据集名、指标名等专有名词保留英文。
- 不要总结、不要发挥、不要添加原文没有的内容，也不要输出额外的说明文字。
- 直接输出完整 Markdown。

论文全文：
{raw_text}"#),
    ("translate", r#"请把以下资料**忠实翻译**成中文，生成一份「原文照搬式」的笔记 Markdown。

要求：
- 第一行输出 `# 标题`（标题翻译成中文）。
- 尽量忠实：逐段翻译，保留原文的章节结构、段落顺序与层级。原文的章节标题用 `##`/`###` 表示，不要合并、省略或重排。
- 公式保持原样（行内 `$...$`、行间 `$$...$$`）；模型名、数据集名、指标名等专有名词保留英文。
- 不要总结、不要发挥、不要添加原文没有的内容，也不要输出额外的说明文字。
- 直接输出完整 Markdown。

资料全文：
{raw_text}"#),
    ("translate-bi", r#"请把以下资料**逐段翻译**成中文，生成一份「原文 + 译文」对照的 Markdown 笔记。

要求：
- 第一行输出 `# 标题`（中英标题都写）。
- 保留原文的章节结构：章节标题用 `##`/`###`；每个段落先给原文、再给译文（译文用 `> ` 引用块，或另起一段，全文保持一致）。
- 公式保持原样（行内 `$...$`、行间 `$$...$$`），公式本身不翻译。
- 专有名词保留英文（首次出现可在括号内注中文）；不要总结、不要发挥、不要添加原文没有的内容。
- 不要输出额外说明，直接给 Markdown。

资料全文：
{raw_text}"#),
    ("verbatim", r#"请把以下资料整理成一份**忠实照抄式**的 Markdown 笔记：内容与顺序尽量保持原样，只做必要的结构化。

要求：
- 第一行输出 `# 标题`（优先用资料自身的标题；没有就用文件名的意思）。
- 保留原文的章节结构：章节标题用 `##`/`###` 表示，不要合并、省略或重排；段落文字不删改、不总结、不发挥。
- 所有数学公式都要用定界符包裹：行内 `$...$`、行间 `$$...$$`（原文若写成裸的 `p_ij`、`\sum` 也要补上）。
- 表格转成 Markdown 表格；图表位置用一句话说明占位（如 `![图：…](图)`），不要凭空编造内容。
- 扫描/OCR 可能有错字：只修正明显的断行、连字符与乱码，不做语义改写。
- 不要输出额外说明，直接给 Markdown。

资料全文：
{raw_text}"#),
    ("lecture", r#"请阅读以下课程讲义/教学材料，生成一份**复习提纲式**的学习笔记 Markdown。

要求：
- 第一行 `# 讲义标题`（用材料标题或文件名的意思）。
- 用 `##` 按**知识点/主题**分节（不要按页码分）；每个知识点下用 `###` 细分（定义、定理/公式、推导、例子、易错点），按材料实际内容取舍。
- 每个 `###` 小标题下的内容（含多段落、公式、表格）合并为一块，不要为每句话单独成块。
- 保留关键定义、定理、公式与推导、例题结论；省略寒暄、课程通知与重复内容。
- 数学公式一律用 `$...$` / `$$...$$`；表格用 Markdown 表格。
- 结尾加一节 `## 复习提纲`，用要点列出需要掌握的概念与题型。
- 不要输出额外说明，直接给 Markdown。

讲义全文：
{raw_text}"#),
    ("free", r#"请阅读以下论文全文，生成一份你认为最有帮助的学习笔记 Markdown。结构、详略、排版都由你决定；若论文有清晰章节，建议沿用，以便对照原文。

论文全文：
{raw_text}"#),
];

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
    // 升级内置风格：文件若仍是旧版默认（用户没改过），改写为「只含内容要求」的新版
    for (id, legacy) in LEGACY_BUILTIN_STYLE_DEFAULTS {
        let Some((meta, default)) = builtin_styles().into_iter().find(|(m, _)| m.id == *id) else {
            continue;
        };
        let path = dir.join(style_file_name(&meta));
        if let Ok(cur) = fs::read_to_string(&path) {
            if cur.trim() == legacy.trim() {
                let _ = fs::write(&path, default);
            }
        }
    }
    // 清理风格文本里的 {raw_text} 占位符（原文改由程序固定附加，用户不应看到）
    for s in &file.style {
        let p = dir.join(style_file_name(s));
        if let Ok(cur) = fs::read_to_string(&p) {
            let cleaned = sanitize_style_prompt(&cur);
            if cleaned != cur {
                logging::info(format!(
                    "风格 `{}` 的提示词已清除 {{raw_text}} 占位符（原文由程序自动附加）",
                    s.id
                ));
                let _ = fs::write(&p, cleaned);
            }
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

/// 读取某风格的提示词内容（文件缺失/为空时回落内置默认；自动清理旧占位符）。
pub fn style_prompt_text(meta: &NoteStyle) -> String {
    let p = styles_dir().join(style_file_name(meta));
    let text = fs::read_to_string(&p).unwrap_or_default();
    if text.trim().is_empty() {
        if let Some((_, d)) = builtin_styles().into_iter().find(|(m, _)| m.id == meta.id) {
            return d.to_string();
        }
    }
    sanitize_style_prompt(&text)
}

/// **固定输出契约**：让笔记能被程序解析成树（标题层级、公式定界符等）。
/// 由程序自动前置到每个风格提示词之前；UI 不展示、用户无需填写。
pub const STYLE_CONTRACT: &str = "【输出格式（程序解析笔记所必需，必须严格遵守）】
- 只输出 Markdown 正文；不要解释、前言、后记，不要用代码围栏包裹整篇。
- 第一行必须是 `# 标题`（整篇笔记的标题）。
- 章节标题用 `##`、子节用 `###`（可继续细分），不要跳级。
- 行内公式用 `$...$`，行间公式用 `$$...$$`；表格用 Markdown 表格。
- 每个标题与其下正文合并为一块，不要为每句话单独成行。

";

/// 清除风格文本里的原文占位符：`{raw_text}` 所在行，以及移除后残留在尾部的
/// 「资料全文：」等纯标签行。
///
/// 资料全文改由程序在 `compose_style_prompt` 里固定附加，用户不应在风格里
/// 编辑/看到占位符；旧文件在读取、保存与启动迁移时自动清理。
pub fn sanitize_style_prompt(text: &str) -> String {
    if !text.contains("{raw_text}") {
        return text.to_string();
    }
    let mut lines: Vec<&str> = text.lines().filter(|l| !l.contains("{raw_text}")).collect();
    while let Some(last) = lines.last() {
        let t = last.trim().trim_end_matches([':', '：']).trim();
        let label_only = matches!(t, "资料全文" | "论文全文" | "讲义全文" | "原文" | "资料");
        if last.trim().is_empty() || label_only {
            lines.pop();
        } else {
            break;
        }
    }
    lines.join("\n").trim_end().to_string()
}

/// 组装完整的笔记生成提示词：固定契约 + 风格（用户可编辑的「写什么」部分）
/// + 资料全文（程序固定附加）+ 额外要求。
pub fn compose_style_prompt(style: &str, raw_text: &str, extra: &str) -> Result<String> {
    let (_, body) = style_prompt(style)?;
    let body = sanitize_style_prompt(&body);
    let mut prompt = format!("{STYLE_CONTRACT}{body}\n\n资料全文：\n{raw_text}");
    if !extra.trim().is_empty() {
        prompt.push_str(&format!("\n\n【本次额外要求】\n{}", extra.trim()));
    }
    Ok(prompt)
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
    fs::write(styles_dir().join(fname), sanitize_style_prompt(prompt))?;
    fs::write(styles_toml_path(), toml::to_string_pretty(&file)?)?;
    Ok(())
}

/// 重命名**自定义**风格：同步改 `<id>.txt` 文件名与清单里的 id/file。
/// 内置风格的 id 固定（`--style four` 等示例与「恢复默认」依赖它），只能改名称/说明/提示词。
pub fn rename_style(old_id: &str, new_id: &str) -> Result<()> {
    let old_id = old_id.trim();
    let new_id = new_id.trim();
    if !valid_style_id(new_id) {
        bail!("风格 id 不合法（1~40 个字母/数字/-/_，且以字母或数字开头）");
    }
    if old_id == new_id {
        return Ok(());
    }
    ensure_styles()?;
    let mut file = read_styles_file()?;
    let Some(pos) = file.style.iter().position(|s| s.id == old_id) else {
        bail!("找不到风格 `{old_id}`");
    };
    if file.style[pos].builtin {
        bail!("内置风格的 id 不可修改（可改名称/说明/提示词）");
    }
    if file.style.iter().any(|s| s.id == new_id) {
        bail!("风格 id `{new_id}` 已存在");
    }
    let old_file = style_file_name(&file.style[pos]);
    let new_file = format!("{new_id}.txt");
    if old_file != new_file {
        let src = styles_dir().join(&old_file);
        if src.exists() {
            fs::rename(&src, styles_dir().join(&new_file))?;
        }
    }
    file.style[pos].id = new_id.to_string();
    file.style[pos].file = new_file;
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
        // 内置提示词不含 {raw_text}（原文由程序固定附加），且不为空
        for (m, prompt) in builtin_styles() {
            assert!(!prompt.trim().is_empty(), "风格 {} 的提示词为空", m.id);
            assert!(!prompt.contains("{raw_text}"), "风格 {} 不应含 {{raw_text}}", m.id);
        }
    }

    #[test]
    fn sanitize_style_prompt_strips_placeholder_and_label() {
        // 旧内置格式：标签行 + 占位符行
        let old = "请生成笔记。\n\n要求：\n- 要详细。\n\n资料全文：\n{raw_text}";
        assert_eq!(sanitize_style_prompt(old), "请生成笔记。\n\n要求：\n- 要详细。");
        // 占位符与标签同行
        let inline = "请阅读以下内容。\n论文全文：{raw_text}";
        assert_eq!(sanitize_style_prompt(inline), "请阅读以下内容。");
        // 无占位符时保持原样
        let cur = "只写内容要求：先直觉后公式。\n";
        assert_eq!(sanitize_style_prompt(cur), cur);
    }

    #[test]
    fn compose_style_prompt_has_contract_and_material() {
        let p = compose_style_prompt("four", "【资料】这里是一段论文", "只保留公式").unwrap();
        assert!(p.contains("【输出格式"), "应前置固定契约");
        assert!(p.contains("## 一、要解决的问题"), "应包含风格内容");
        assert!(
            p.contains("资料全文：\n【资料】这里是一段论文"),
            "应固定附加资料全文"
        );
        assert!(p.contains("【本次额外要求】\n只保留公式"), "应追加额外要求");
        assert!(!p.contains("{raw_text}"), "不应残留占位符");
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
