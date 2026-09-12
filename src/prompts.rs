//! 提示词模板的落盘与加载。
//!
//! LLM 的行为提示词全部外置为 `.paperhelper/prompts/*.txt`，用户可直接编辑
//! （改完重启生效），删除则回落内置默认。首次运行 `ensure_prompt_files()`
//! 会把内置默认写盘供编辑。`ask.txt` 约定 LLM 用 `[[概念: 名字]]` 行回报
//! 核心概念，是知识库自动提取概念的接口协议。

use anyhow::Result;
use std::fs;
use std::path::Path;

use crate::paths;

/// 提示词模板目录：.paperhelper/prompts/
/// 文件存在则用户自定义生效；不存在则用内置默认并写出默认文件供用户编辑。
pub fn prompts_dir() -> std::path::PathBuf {
    paths::data_dir().join("prompts")
}

/// 首次启动时把默认提示词模板写到 .paperhelper/prompts/，供用户编辑。
pub fn ensure_prompt_files() -> Result<()> {
    let dir = prompts_dir();
    if !dir.exists() {
        fs::create_dir_all(&dir)?;
    }
    let defaults: &[(&str, &str)] = &[
        ("ask.txt", DEFAULT_ASK_PROMPT),
        ("note.txt", DEFAULT_NOTE_PROMPT),
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

pub const DEFAULT_ASK_PROMPT: &str = "你是一位耐心的论文学习助手。用户会给你一篇论文的全文、已生成的结构化笔记，以及（可能的）历史问答。请基于这些回答用户问题，简洁清晰（300字以内），尽量和笔记的章节结构对齐。若涉及已学概念，点明它们的联系。

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
