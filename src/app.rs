//! REPL 主控与业务编排（最大模块）。
//!
//! `App` 聚合 Config/KnowledgeBase/Session/client，`repl()` 提供交互循环，
//! `run_command()` 按命令名分发给各 `cmd_*`。核心业务流程：
//! - ingest：抽 PDF 文本 → LLM 生成四段笔记 Markdown → Rust 解析成笔记树并编号 →
//!   登记论文 → 首次交互式询问导出文件名（后续 ask 自动同步导出）。
//! - ask/check：用 `build_context_messages` 拼上下文（笔记块 + 论文全文 +
//!   知识库相关概念 + 对话根路径），LLM 流式回答；ask 建 Explanation 挂到
//!   locate 定位的块并递归嵌套，check 只建对话节点不写笔记。
//! - sum：把当前节点子树收集后让 LLM 概括，折叠进 Explanation。
//! - 全程 `record_usage` 累加 token/成本，`check_budget` 超预算即中断；
//!   所有调用先 `interrupt::reset()` 再 poll 打断标志。
//! 补全信息（section/node 编号、presets）通过 Arc<Mutex> 与 rustyline helper 共享。

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use owo_colors::OwoColorize;
use rustyline::completion::{Completer, FilenameCompleter};
use rustyline::error::ReadlineError;
use rustyline::highlight::MatchingBracketHighlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::MatchingBracketValidator;
use rustyline::{Cmd, Editor, KeyEvent, Helper};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::config::Config;
use crate::conversation::{Conversation, ConvNode};
use crate::export;
use crate::interrupt;
use crate::knowledge::{Concept, ConceptRelation, KnowledgeBase, Paper};
use crate::llm::{self, Message};
use crate::notes::{self, Explanation};
use crate::output::Emitter;
use crate::pdf;
use crate::session::{Annotation, Session};

/// 输出宏：把标准输出的语义接到 `$slf.emitter` 上（终端或 Web/SSE 由 emitter 决定）。
/// 需显式传入 `self`（macro_rules 对 self 是卫生的，无法从调用点隐式取得）。
/// outln 带换行，outerr 走错误输出。
macro_rules! outln {
    ($slf:expr, $($arg:tt)*) => { $slf.emitter.stdout(format!($($arg)*)) };
}
macro_rules! outerr {
    ($slf:expr, $($arg:tt)*) => { $slf.emitter.stderr(format!($($arg)*)) };
}

/// ask/check 的 system prompt：优先用 .paperhelper/prompts/ask.txt（用户可编辑），
/// 不存在则用内置默认并首启写出。
fn sys_ask() -> String {
    crate::prompts::load_prompt(
        &crate::prompts::prompts_dir(),
        "ask.txt",
        crate::prompts::DEFAULT_ASK_PROMPT,
    )
}

const COMMANDS: &[&str] = &[
    "ingest", "ask", "check", "sum", "del", "undo", "blocks", "note", "tree", "goto", "stats",
    "budget", "save", "load", "export", "papers", "concepts", "graph", "styles", "config", "new",
    "help", "exit",
];

/// `del`/批注删除的撤销快照：保存可恢复的笔记、对话树与批注，不含 stats
/// （成本是真实发生的，撤销删除不应回退用量统计）。
#[derive(Clone)]
struct UndoSnapshot {
    notes: Option<crate::notes::Note>,
    conversation: Conversation,
    annotations: Vec<Annotation>,
}

/// 配置键定义表：补全与用法提示的单一来源。
/// set_config 的 match 分支与此表保持键名一致。
struct ConfigKeyDef {
    key: &'static str,
    /// bool 型键（补全候选 true/false）；其余按 presets 或不补全
    is_bool: bool,
    /// 模型/端点类键（候选来自 config 的 presets）
    from_presets: Option<PresetKind>,
}

#[derive(PartialEq, Clone, Copy)]
enum PresetKind {
    Models,
    Endpoints,
}

const CONFIG_KEY_DEFS: &[ConfigKeyDef] = &[
    ConfigKeyDef { key: "llm.api_key", is_bool: false, from_presets: None },
    ConfigKeyDef { key: "llm.api_endpoint", is_bool: false, from_presets: Some(PresetKind::Endpoints) },
    ConfigKeyDef { key: "llm.model", is_bool: false, from_presets: Some(PresetKind::Models) },
    ConfigKeyDef { key: "llm.context_length", is_bool: false, from_presets: None },
    ConfigKeyDef { key: "llm.thinking_mode", is_bool: true, from_presets: None },
    ConfigKeyDef { key: "llm.pdf_input", is_bool: true, from_presets: None },
    ConfigKeyDef { key: "pricing.input_price_per_1m", is_bool: false, from_presets: None },
    ConfigKeyDef { key: "pricing.output_price_per_1m", is_bool: false, from_presets: None },
    ConfigKeyDef { key: "budget.token_budget", is_bool: false, from_presets: None },
    ConfigKeyDef { key: "update.auto_check", is_bool: true, from_presets: None },
    ConfigKeyDef { key: "update.source_url", is_bool: false, from_presets: None },
];

/// 命令补全器：
/// - 第一个词：补全命令名
/// - ingest/save/load/export 的参数：补全文件路径
/// - goto/ask/check 的第一个参数：补全节点编号或 section 编号
/// - config：子命令 / 键名（表驱动）/ 值候选（bool 表驱动，模型与端点来自 presets）
struct CommandCompleter {
    file_completer: FilenameCompleter,
    /// 当前可用的 section 编号（如 ["1", "1.1", "2", "3.2"]），由 App 在 ingest/ask 后更新
    section_numbers: Arc<Mutex<Vec<String>>>,
    /// 当前可用的对话树节点编号（如 ["1", "2", "3"]），由 App 在 ingest/ask/goto 后更新
    node_numbers: Arc<Mutex<Vec<String>>>,
    /// 模型名/端点候选（从 config 的 presets 克隆，改 config.toml 后重启生效）
    preset_models: Vec<String>,
    preset_endpoints: Vec<String>,
}

impl Completer for CommandCompleter {
    type Candidate = String;
    fn complete(&self, line: &str, pos: usize, _ctx: &rustyline::Context<'_>) -> rustyline::Result<(usize, Vec<String>)> {
        let start = line[..pos].rfind(' ').map(|i| i + 1).unwrap_or(0);
        let word = &line[start..pos];

        // 第一个词：补全命令名
        if line[..start].trim().is_empty() {
            let matches: Vec<String> = COMMANDS
                .iter()
                .filter(|c| c.starts_with(word))
                .map(|c| c.to_string())
                .collect();
            return Ok((start, matches));
        }

        // 解析命令名
        let cmd = line.trim_start().split_whitespace().next().unwrap_or("");

        // config 的子命令/键名/值补全
        if cmd == "config" {
            let words_before = line[..start].split_whitespace().count();
            match words_before {
                // 第一个参数：补全子命令 set/show
                1 => {
                    let matches: Vec<String> = ["set", "show"]
                        .iter()
                        .filter(|s| s.starts_with(word))
                        .map(|s| s.to_string())
                        .collect();
                    return Ok((start, matches));
                }
                // set 后的键名（从 CONFIG_KEY_DEFS 表派生）
                2 if line[..start].split_whitespace().nth(1) == Some("set") => {
                    let matches: Vec<String> = CONFIG_KEY_DEFS
                        .iter()
                        .filter(|d| d.key.starts_with(word))
                        .map(|d| d.key.to_string())
                        .collect();
                    return Ok((start, matches));
                }
                // set 后的值：bool 键从表派生；模型/端点从 presets 读
                3 if line[..start].split_whitespace().nth(1) == Some("set") => {
                    let key = line[..start].split_whitespace().nth(2).unwrap_or("");
                    let def = CONFIG_KEY_DEFS.iter().find(|d| d.key == key);
                    let candidates: Vec<String> = match def {
                        Some(d) if d.is_bool => vec!["true".into(), "false".into()],
                        Some(d) => match d.from_presets {
                            Some(PresetKind::Models) => self.preset_models.clone(),
                            Some(PresetKind::Endpoints) => self.preset_endpoints.clone(),
                            None => vec![],
                        },
                        None => vec![],
                    };
                    let matches: Vec<String> = candidates
                        .iter()
                        .filter(|c| c.starts_with(word))
                        .cloned()
                        .collect();
                    return Ok((start, matches));
                }
                _ => return Ok((start, Vec::new())),
            }
        }

        let args_started = line.trim_start().len() > cmd.len() && line[start - 1..start].trim().is_empty();

        // 文件路径补全的命令
        if matches!(cmd, "ingest" | "save" | "load" | "export") {
            let (s, pairs) = self.file_completer.complete_path(line, pos)?;
            let candidates: Vec<String> = pairs.into_iter().map(|p| p.replacement).collect();
            return Ok((s, candidates));
        }

        // export 的格式补全：export <md|mindmap|html> <file>
        if cmd == "export" && !args_started {
            let matches: Vec<String> = ["md", "markdown", "mindmap", "mm", "html"]
                .iter()
                .filter(|f| f.starts_with(word))
                .map(|s| s.to_string())
                .collect();
            if !matches.is_empty() {
                return Ok((start, matches));
            }
        }

        // goto 的参数：补全对话树节点编号
        if cmd == "goto" {
            let nums = self.node_numbers.lock().unwrap();
            let matches: Vec<String> = nums
                .iter()
                .filter(|n| n.starts_with(word))
                .map(|n| n.to_string())
                .collect();
            return Ok((start, matches));
        }

        // ask/check 的第一个参数：补全 section 编号
        if matches!(cmd, "ask" | "check") {
            let nums = self.section_numbers.lock().unwrap();
            let matches: Vec<String> = nums
                .iter()
                .filter(|n| n.starts_with(word))
                .map(|n| n.to_string())
                .collect();
            return Ok((start, matches));
        }

        Ok((start, Vec::new()))
    }
}

/// rustyline Helper：命令补全 + 括号高亮/校验。
struct PaperHelperHelper {
    completer: CommandCompleter,
    highlighter: MatchingBracketHighlighter,
    validator: MatchingBracketValidator,
}

impl Helper for PaperHelperHelper {}

impl Completer for PaperHelperHelper {
    type Candidate = String;
    fn complete(&self, line: &str, pos: usize, ctx: &rustyline::Context<'_>) -> rustyline::Result<(usize, Vec<String>)> {
        self.completer.complete(line, pos, ctx)
    }
}

impl Hinter for PaperHelperHelper {
    type Hint = String;
}

impl rustyline::highlight::Highlighter for PaperHelperHelper {
    fn highlight<'l>(&self, line: &'l str, pos: usize) -> std::borrow::Cow<'l, str> {
        self.highlighter.highlight(line, pos)
    }
}

impl rustyline::validate::Validator for PaperHelperHelper {
    fn validate(&self, ctx: &mut rustyline::validate::ValidationContext<'_>) -> rustyline::Result<rustyline::validate::ValidationResult> {
        self.validator.validate(ctx)
    }
}

impl PaperHelperHelper {
    fn new(
        section_numbers: Arc<Mutex<Vec<String>>>,
        node_numbers: Arc<Mutex<Vec<String>>>,
        preset_models: Vec<String>,
        preset_endpoints: Vec<String>,
    ) -> Self {
        Self {
            completer: CommandCompleter {
                file_completer: FilenameCompleter::new(),
                section_numbers,
                node_numbers,
                preset_models,
                preset_endpoints,
            },
            highlighter: MatchingBracketHighlighter::new(),
            validator: MatchingBracketValidator::new(),
        }
    }
}

pub struct App {
    pub config: Config,
    pub kb: KnowledgeBase,
    /// 当前会话现场（笔记树+对话树+用量统计+编号/会话名）。
    pub session: Session,
    pub client: reqwest::Client,
    /// ask 后自动导出笔记的文件路径（ingest 时由用户指定）。
    pub export_path: Option<String>,
    /// 输出器：CLI 下写终端，Web 下写 SSE。所有用户可见输出都经它。
    pub emitter: Emitter,
    /// 当前笔记的 section 编号列表（供补全用）
    section_numbers: Arc<Mutex<Vec<String>>>,
    /// 当前对话树节点编号列表（供补全用）
    node_numbers: Arc<Mutex<Vec<String>>>,
    /// del 的内存撤销栈（最近在后，上限 20）。
    undo_stack: Vec<UndoSnapshot>,
    /// 每个会话的状态版本号（key = session_id；未保存的新会话用固定占位符）。
    /// 任何会改动会话内容（笔记/对话/批注）的操作都会递增对应会话的版本号。
    /// 在途 LLM 任务在开始时记录「发起会话 key + 版本号」，完成时：
    /// - 结果写回发起会话（用户切走了也不丢，见 `with_session`）；
    /// - 若发起会话的版本号已变（被编辑/删除/被别的任务写过），则判冲突。
    pub session_revs: HashMap<String, u64>,
}

/// 一次 ask/批注提问的「可锁外执行」上下文：
/// prepare 阶段在持锁时构造；`run()` 不访问 App（可在锁外流式执行）；
/// `commit_ask()` 再持锁落地，并检查发起会话的版本号是否变化。
pub struct AskJob {
    msgs: Vec<Message>,
    cfg: crate::config::LlmConfig,
    client: reqwest::Client,
    /// 发起会话的版本号 key（`App::session_key`）——完成时据此写回发起会话。
    session: String,
    epoch: u64,
    /// LLM 失败/中止时要恢复的 conversation.current（批注流程会临时改它）。
    restore_current: Option<String>,
    /// 新节点的父节点（prepare 时捕获，避免流式期间用户 goto 影响挂载位置）。
    parent: Option<String>,
    question: String,
    block_id: Option<String>,
    is_check: bool,
    quote: Option<String>,
    record_concept: bool,
    approx_tokens: usize,
    /// 是否把解释写进笔记树（普通 ask/check 为 true；PDF 页面提问为 false，
    /// 只建独立对话线程 + 批注，不改动生成的笔记）。
    attach_note: bool,
}

/// 提问附件（在途发送，**不写入会话历史**）：文本注入 prompt，图片作为多模态输入。
#[derive(Debug, Clone, Default)]
pub struct ExtraInput {
    /// 已拼接好的附件文本（含文件名与分隔），空表示无文本附件。
    pub text: String,
    /// 图片 data URL（如 PDF 页截图 / 用户上传图片）。
    pub images: Vec<String>,
}

impl ExtraInput {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty()
    }

    /// 把附件并入发给模型的消息列表：追加到末条 user 消息（正文 + 图片）。
    fn apply_to(&self, msgs: &mut Vec<Message>) {
        if self.is_empty() {
            return;
        }
        let has_user = msgs.last().map(|m| m.role == "user").unwrap_or(false);
        if !has_user {
            let mut m = Message::text("user", self.text.clone());
            for img in &self.images {
                m = m.with_image(img.clone());
            }
            msgs.push(m);
            return;
        }
        let mut last = msgs.pop().expect("has_user implies non-empty");
        if !self.text.trim().is_empty() {
            last.content.push_str(&self.text);
        }
        for img in &self.images {
            last = last.with_image(img.clone());
        }
        msgs.push(last);
    }
}

impl AskJob {
    /// 锁外执行 LLM 流式调用（进度/思考/正文都经 emitter 输出）。
    pub async fn run(&self, emitter: &Emitter) -> Result<crate::llm::LlmResult> {
        interrupt::reset();
        emitter.progress(&format!(
            "{}（上下文约 {} token）",
            if self.is_check { "核对中…" } else { "思考中…" },
            self.approx_tokens
        ));
        let mut first = true;
        let res = llm::chat(&self.client, &self.cfg, &self.msgs, false, self.cfg.thinking_mode, &mut |t| {
            if first {
                emitter.progress_done();
                first = false;
                emitter.token("\n");
            }
            emitter.token(t);
        }, Some(&mut |r| emitter.reasoning(r))).await;
        if first {
            emitter.progress_done();
        }
        res
    }
}

/// 批注锚点（prepare 时确定，commit 时写入 Annotation）。
pub enum AnnAnchor {
    Note { block_id: String, quote: String, quote_tex: Option<String> },
    Answer { node_id: String, quote: String, quote_tex: Option<String> },
    /// PDF 阅读器里针对某页选区/图片/整页的提问（不写入笔记树）。
    Pdf { page: u32, rects: Vec<[f32; 4]>, kind: String, quote: String },
}

/// ingest（LLM 生成路径）的任务：prepare 在持锁时构造，`run()` 可在锁外流式执行。
pub struct IngestJob {
    msgs: Vec<Message>,
    cfg: crate::config::LlmConfig,
    client: reqwest::Client,
    /// 发起会话的版本号 key（`App::session_key`）——完成时据此写回发起会话。
    session: String,
    epoch: u64,
    raw_text: String,
    source_path: String,
    export_file: String,
    kind: String,
}

/// ingest 的三种执行方式：直接导入（无 LLM）、仅阅读 PDF（无 LLM、不生成笔记）
/// 或 LLM 生成（可锁外流式执行）。
pub enum IngestPrep {
    Direct { file_path: String, mode: String, kind: String },
    Read { file_path: String },
    Llm(Box<IngestJob>),
}

/// 「仅阅读」抽取文本的结果分类（见 `readonly_extract_notice`）。
#[derive(Debug, PartialEq)]
enum ExtractOutcome {
    /// 正常抽到文本（无需打扰用户）
    Text,
    /// 未抽到文本：可能是扫描/图片版
    Empty,
    /// 解析失败（原始错误只写日志，不给用户看技术堆栈）
    Failed,
}

/// 根据抽取结果给出要提示给用户的文案（`None` = 正常，无需提示）。
/// 仅阅读模式抽不到文字并不影响阅读，故只提示、不拦截；文案说明原因并给出建议。
fn readonly_extract_notice(outcome: &ExtractOutcome) -> Option<String> {
    match outcome {
        ExtractOutcome::Text => None,
        ExtractOutcome::Empty => Some(
            "未能从该 PDF 提取到文字：它可能是扫描/图片版，文字在图片里而不是可直接提取的文本层。\
             你仍可阅读原文并选中内容提问（提问会发送该页截图）；\
             如需文字版笔记，可用 OCR 方式重新导入。"
                .to_string(),
        ),
        ExtractOutcome::Failed => Some(
            "读取该 PDF 的文字失败（仍可阅读原件，提问将依赖页面截图）。\
             可能是文件损坏或不是有效的 PDF；若原文也无法显示，请更换文件后重试。"
                .to_string(),
        ),
    }
}

/// 简单 LLM 任务（AI 重写 / 按风格重写全文）：只生成内容，不改会话（除用量统计）。
pub struct SimpleJob {
    msgs: Vec<Message>,
    cfg: crate::config::LlmConfig,
    client: reqwest::Client,
    progress: String,
    /// 发起会话的版本号 key：用量写回这个会话（用户切走也不记错账）。
    session: String,
}

impl SimpleJob {
    /// 锁外执行 LLM 流式调用。
    pub async fn run(&self, emitter: &Emitter) -> Result<crate::llm::LlmResult> {
        interrupt::reset();
        emitter.progress(&self.progress);
        let mut first = true;
        let res = llm::chat(&self.client, &self.cfg, &self.msgs, false, self.cfg.thinking_mode, &mut |t| {
            if first {
                emitter.progress_done();
                first = false;
            }
            emitter.token(t);
        }, Some(&mut |r| emitter.reasoning(r))).await;
        if first {
            emitter.progress_done();
        }
        res
    }
}

/// sum（子树概括）的任务：prepare 在持锁时构造，`run()` 可在锁外流式执行。
pub struct SumJob {
    msgs: Vec<Message>,
    cfg: crate::config::LlmConfig,
    client: reqwest::Client,
    /// 发起会话的版本号 key：总结写回这个会话（用户切走也不丢）。
    session: String,
    epoch: u64,
    /// 被概括的子树根节点 id（commit 时据此定位 Explanation）。
    cur: String,
}

impl SumJob {
    /// 锁外执行 LLM 非流式调用（总结是一次性输出）。
    pub async fn run(&self, emitter: &Emitter) -> Result<crate::llm::LlmResult> {
        interrupt::reset();
        emitter.progress("概括总结中…");
        let res = llm::chat(&self.client, &self.cfg, &self.msgs, false, self.cfg.thinking_mode, &mut |_| {}, None).await;
        emitter.progress_done();
        res
    }
}

/// 知识图谱关系整理（`graph build`）：只让 LLM 判断「新概念」与已有概念的关系。
/// 与论文/会话无关，锁外执行，`commit_graph` 再持锁合并进知识库。
pub struct GraphJob {
    msgs: Vec<Message>,
    cfg: crate::config::LlmConfig,
    client: reqwest::Client,
    /// 本次参与整理的新概念名（commit 时标记为已整理）。
    new_names: Vec<String>,
    /// 是否重建：commit 时先清空旧关系再合并。
    rebuild: bool,
}

impl GraphJob {
    /// 锁外执行 LLM 非流式调用（关系输出是一小段结构化文本）。
    pub async fn run(&self, emitter: &Emitter) -> Result<crate::llm::LlmResult> {
        interrupt::reset();
        emitter.progress("整理概念关系…");
        let res = llm::chat(&self.client, &self.cfg, &self.msgs, false, self.cfg.thinking_mode, &mut |_| {}, None).await;
        emitter.progress_done();
        res
    }
}

impl IngestJob {
    /// 锁外执行 LLM 流式调用（正文 + 思考 + 字数进度）。
    pub async fn run(&self, emitter: &Emitter) -> Result<crate::llm::LlmResult> {
        interrupt::reset();
        let web = !emitter.is_terminal();
        let mut first_token = true;
        emitter.progress("笔记生成中…");
        let mut gen_chars: u64 = 0;
        let mut pending_chars: u64 = 0;
        let mut last_emit = std::time::Instant::now();
        let res = llm::chat(&self.client, &self.cfg, &self.msgs, false, self.cfg.thinking_mode, &mut |t| {
            if first_token {
                if !web {
                    emitter.progress_done();
                }
                first_token = false;
            }
            // 流式笔记文本：CLI 直接打印，Web 送到「笔记区」的生成中预览
            emitter.token(t);
            if web {
                // 同时节流上报总字数，供顶部进度条显示
                let n = t.chars().count() as u64;
                gen_chars += n;
                pending_chars += n;
                if pending_chars >= 64 || last_emit.elapsed() >= std::time::Duration::from_millis(200) {
                    emitter.chars(gen_chars);
                    pending_chars = 0;
                    last_emit = std::time::Instant::now();
                }
            }
        }, Some(&mut |r| emitter.reasoning(r))).await;
        if web {
            emitter.chars(gen_chars);
        }
        if first_token || web {
            emitter.progress_done();
        }
        res
    }
}

impl App {
    /// 组装 App：写出默认提示词/风格模板、初始化空会话与补全列表。
    pub fn new(config: Config, kb: KnowledgeBase, client: reqwest::Client) -> Self {
        // 首次启动写出默认提示词模板（ask/rewrite）与风格目录（styles/）
        let _ = crate::prompts::ensure_prompt_files();
        let _ = crate::prompts::ensure_styles();
        Self {
            config,
            kb,
            session: Session::default(),
            client,
            export_path: None,
            emitter: Emitter::terminal(),
            section_numbers: Arc::new(Mutex::new(Vec::new())),
            node_numbers: Arc::new(Mutex::new(Vec::new())),
            undo_stack: Vec::new(),
            session_revs: HashMap::new(),
        }
    }

    /// 当前会话的版本号 key（未保存的新会话用固定占位符，不会与时间戳 id 冲突）。
    pub fn session_key(&self) -> String {
        if self.session.session_id.is_empty() {
            "\u{0}unsaved".to_string()
        } else {
            self.session.session_id.clone()
        }
    }

    /// 某个会话当前的版本号（没记录过为 0）。
    pub fn session_epoch(&self, key: &str) -> u64 {
        self.session_revs.get(key).copied().unwrap_or(0)
    }

    /// 当前会话对应的 PDF 原件路径（仅当存在且确为 `.pdf` 文件时返回）。
    /// 优先用笔记记录的源路径，回退到知识库里当前论文的路径；用于
    /// `/api/pdf/file` 把原件喂给前端 PDF.js 阅读器。
    pub fn pdf_source(&self) -> Option<std::path::PathBuf> {
        let by_note = self
            .session
            .notes
            .as_ref()
            .and_then(|n| n.source_path.clone());
        let by_paper = self.session.current_paper_id.as_ref().and_then(|id| {
            self.kb
                .papers
                .iter()
                .find(|p| &p.id == id)
                .map(|p| p.path.clone())
        });
        for cand in [by_note, by_paper].into_iter().flatten() {
            let p = std::path::PathBuf::from(&cand);
            let is_pdf = p
                .extension()
                .map(|e| e.eq_ignore_ascii_case("pdf"))
                .unwrap_or(false);
            if is_pdf && p.is_file() {
                return Some(p);
            }
        }
        None
    }

    /// 递增**当前会话**的版本号（改动其内容后调用，见 `session_revs`）。
    pub(crate) fn bump_epoch(&mut self) {
        let key = self.session_key();
        self.bump_session(&key);
    }

    /// 递增指定会话的版本号（如删除会话时给在途任务制造冲突）。
    pub(crate) fn bump_session(&mut self, key: &str) {
        let e = self.session_revs.entry(key.to_string()).or_insert(0);
        *e = e.wrapping_add(1);
    }

    /// 给有内容但尚未保存的会话分配 session_id：在途任务用它的 key 定位发起会话。
    pub(crate) fn ensure_session_id(&mut self) {
        if self.session.session_id.is_empty()
            && (self.session.notes.is_some() || !self.session.conversation.nodes.is_empty())
        {
            self.session.session_id = crate::paths::new_session_stamp();
        }
    }

    /// 把当前会话临时换成 `target_id` 对应的磁盘会话，执行 `f`，成功后保存并换回。
    /// `f` 内的 `self.session` / `self.export_path` 操作都会落在目标会话上；
    /// `f` 失败（或返回 Err）则目标会话不落盘。用于把 LLM 结果写回发起会话。
    fn with_session<F, T>(&mut self, target_id: &str, f: F) -> Result<T>
    where
        F: FnOnce(&mut Self) -> Result<T>,
    {
        let path = crate::paths::session_path(target_id);
        let loaded = Session::load(&path)?;
        let prev_session = std::mem::replace(&mut self.session, loaded);
        let prev_export = std::mem::replace(&mut self.export_path, self.session.export_path.clone());
        let out = f(self);
        let target_export = std::mem::replace(&mut self.export_path, prev_export);
        let mut target_session = std::mem::replace(&mut self.session, prev_session);
        let value = out?;
        target_session.export_path = target_export;
        crate::paths::ensure_sessions_dir()?;
        target_session.save(&path)?;
        Ok(value)
    }

    /// 是否有可撤销的删除（Web 用于启用/禁用撤销按钮）。
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// 记录一次可撤销的会话快照（笔记 / 对话 / 批注），供 `undo` 恢复。
    /// 所有会改动笔记或对话的操作在动手前都应调用；栈深上限 20。
    fn push_undo(&mut self) {
        self.undo_stack.push(UndoSnapshot {
            notes: self.session.notes.clone(),
            conversation: self.session.conversation.clone(),
            annotations: self.session.annotations.clone(),
        });
        if self.undo_stack.len() > 20 {
            self.undo_stack.remove(0);
        }
    }

    /// 主循环：打印横幅与配置引导 → 初始化 rustyline 编辑器（history、
    /// Ctrl-C 绑定、补全 helper）→ 逐行 `run_command` → 退出前 `autosave_on_exit`。
    /// Ctrl-C 语义：当前无任务时取消输入行；有 LLM 任务时置打断标志中止流式。
    pub async fn repl(&mut self) -> Result<()> {
        outln!(self, "{}", "=== PaperHelper 论文学习助手 ===".bold().cyan());
        outln!(self, 
            "{} {} | {} {}",
            "模型:".dimmed(),
            self.config.llm.model.green(),
            "端点:".dimmed(),
            self.config.llm.api_endpoint.green(),
        );

        // 配置完整性检查：缺 key 或端点仍是默认 OpenAI 时给出引导
        let need_key = self.config.llm.api_key.is_empty();
        let default_endpoint = self.config.llm.api_endpoint
            == "https://api.openai.com/v1/chat/completions";
        if need_key || default_endpoint {
            outln!(self, "{}", "⚠️  配置不完整，请先完成以下设置（或写 .env）：".yellow());
            if need_key {
                outln!(self, "  > config set llm.api_key <你的key>");
            }
            if default_endpoint {
                outln!(self, "  > config set llm.api_endpoint https://api.deepseek.com/v1/chat/completions");
                outln!(self, "  > config set llm.model deepseek-v4-pro");
            }
            outln!(self, "  示例（DeepSeek）：endpoint=https://api.deepseek.com/v1/chat/completions  model=deepseek-v4-pro");
        }
        outln!(self, "{}  help 查看命令；exit 退出。Ctrl-C 打断当前任务，↑↓ 切换历史，Tab 补全。\n",
            "输入".dimmed());

        let hist_path = crate::paths::data_dir().join("history.txt");
        let mut rl = Editor::<PaperHelperHelper, DefaultHistory>::new()?;
        rl.set_helper(Some(PaperHelperHelper::new(
            self.section_numbers.clone(),
            self.node_numbers.clone(),
            self.config.presets.models.clone(),
            self.config.presets.endpoints.clone(),
        )));
        // 绑定 Ctrl-C 为中断信号（不打断程序，只取消当前输入/任务）
        rl.bind_sequence(KeyEvent::ctrl('c'), Cmd::Interrupt);
        let _ = rl.load_history(&hist_path);
        // 初始化补全编号（恢复会话后也能补全）
        self.update_completions();

        loop {
            let cur = self.session.conversation.current_label().to_string();
            let prompt = format!(
                "{} [{}]> ",
                "paperhelper".bold().cyan(),
                short(&cur, 20).purple(),
            );
            let readline = rl.readline(&prompt);
            match readline {
                Ok(line) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let _ = rl.add_history_entry(line);
                    if matches!(line, "exit" | "quit") {
                        break;
                    }
                    if let Err(e) = self.run_command(line).await {
                        outerr!(self, "{} {e:#}", "❌".red());
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    // Ctrl-C：打断当前 LLM 任务（如果有），不退出
                    if !interrupt::is_interrupted() {
                        outerr!(self, "{}", "[已打断当前任务]".yellow());
                    }
                }
                Err(ReadlineError::Eof) => {
                    outln!(self, "");
                    break;
                }
                Err(e) => {
                    outerr!(self, "{} 读取输入失败: {e}", "❌".red());
                }
            }
        }
        let _ = rl.save_history(&hist_path);
        // 退出时自动保存会话
        self.autosave_on_exit().await?;
        Ok(())
    }

    /// 退出时自动保存会话到 .paperhelper/sessions/。
    /// 编号 = 首次保存时间戳（session_id，之后每次退出覆盖保存同一文件不变）。
    /// 保存前先调 LLM 取一个简短会话名（失败则兜底"未命名会话"）。
    pub async fn autosave_on_exit(&mut self) -> Result<()> {
        if self.session.notes.is_none() && self.session.conversation.nodes.is_empty() {
            return Ok(());
        }
        crate::paths::ensure_sessions_dir()?;

        // 让 LLM 给会话取个简短名字（带进度条）
        self.emitter.progress("保存会话中…");
        let session_name = self.generate_session_name().await;
        self.emitter.progress_done();
        self.session.session_name = session_name;

        // 会话编号：首次保存时生成（保存时间戳），此后不变；文件按编号覆盖保存
        if self.session.session_id.is_empty() {
            self.session.session_id = crate::paths::new_session_stamp();
        }
        self.session.export_path = self.export_path.clone();
        let path = crate::paths::session_path(&self.session.session_id);
        self.session.save(&path)?;
        outln!(self, "{} 会话已保存：{}", "✓".green().bold(), self.session.session_name);
        outln!(self, "  恢复会话，请执行：paperhelper -s {}", self.session.session_id);
        Ok(())
    }

    /// Web 端自动保存会话（**不调用 LLM 取名**，用笔记标题兜底），
    /// 让新导入的会话立即出现在会话列表里，切换/新建都不会丢。
    pub fn auto_persist(&mut self) -> Result<()> {
        if self.session.notes.is_none() && self.session.conversation.nodes.is_empty() {
            return Ok(());
        }
        if self.session.session_id.is_empty() {
            self.session.session_id = crate::paths::new_session_stamp();
        }
        if self.session.session_name.trim().is_empty() {
            let title = self
                .session
                .notes
                .as_ref()
                .map(|n| n.title.clone())
                .unwrap_or_default();
            self.session.session_name = if title.trim().is_empty() {
                "未命名会话".to_string()
            } else {
                title.chars().take(30).collect()
            };
        }
        self.session.export_path = self.export_path.clone();
        crate::paths::ensure_sessions_dir()?;
        let path = crate::paths::session_path(&self.session.session_id);
        self.session.save(&path)?;
        Ok(())
    }

    /// 调 LLM 根据笔记标题+对话历史概括一个简短会话名（≤20字）。
    async fn generate_session_name(&self) -> String {
        let title = self
            .session
            .notes
            .as_ref()
            .map(|n| n.title.clone())
            .unwrap_or_default();
        let labels: Vec<String> = self
            .session
            .conversation
            .path_to_current()
            .iter()
            .map(|n| n.label.clone())
            .collect();
        let summary = if labels.is_empty() {
            title.clone()
        } else {
            format!("{title}；提问：{}", labels.join("、"))
        };
        let prompt = format!(
            "请用不超过20个中文字/英文单词概括以下会话主题，只输出名字，不要解释：\n{summary}"
        );
        let msgs = vec![Message::text("user", prompt)];
        match llm::chat(&self.client, &self.config.llm, &msgs, false, false, &mut |_| {}, None).await {
            Ok(res) => {
                let name = res.content.trim().to_string();
                if name.is_empty() {
                    title.chars().take(20).collect()
                } else {
                    name
                }
            }
            Err(_) => {
                // LLM 调用失败，用"未命名会话"兜底（编号另由 session_id 承担）
                "未命名会话".to_string()
            }
        }
    }

    /// 命令分发器：把一行输入拆成 (命令, 剩余参数) 后匹配执行各 cmd_*；
    /// 每条命令结束后统一刷新补全列表（区块/节点编号可能已变化）。
    /// 支持别名：tree|trajectory、ingest|pdf、ask|q、help|?。
    pub async fn run_command(&mut self, line: &str) -> Result<()> {
        // 注意：不在这里 interrupt::reset()——LLM 任务可能正在另一个线程流式执行、
        // 并依赖打断标志；各 LLM 任务在真正开始调模型前自己 reset（见 Job::run）。
        let (cmd, rest) = split_cmd(line);
        crate::logging::debug(format!("收到命令: {line}"));
        let t0 = std::time::Instant::now();
        let result = match cmd {
            "help" | "?" => self.cmd_help(),
            "config" => self.cmd_config(rest).await,
            "budget" => self.cmd_budget(rest).await,
            "blocks" => self.cmd_blocks().await,
            "note" => self.cmd_note().await,
            "tree" | "trajectory" => {
                outln!(self, "{}", self.session.conversation.render_tree());
                Ok(())
            }
            "goto" => self.cmd_goto(rest).await,
            "stats" => self.cmd_stats().await,
            "papers" => self.cmd_papers().await,
            "concepts" => self.cmd_concepts().await,
            "graph" => self.cmd_graph(rest).await,
            "styles" => self.cmd_styles(rest).await,
            "new" => {
                // 先保存当前会话：在途 LLM 任务稍后仍能写回它（写回不需要它是当前会话）
                if let Err(e) = self.auto_persist() {
                    crate::logging::warn(format!("新建会话前保存失败: {e:#}"));
                }
                if self.session.session_id.is_empty() {
                    // 空会话没有 id（key 是占位符，new 后会重复使用）：递增版本号让在途任务判冲突
                    self.bump_session(&self.session_key());
                }
                self.session = Session::default();
                self.export_path = None;
                self.undo_stack.clear();
                outln!(self, "已新建会话。");
                Ok(())
            }
            "save" => self.cmd_save(rest).await,
            "load" => self.cmd_load(rest).await,
            "export" => self.cmd_export(rest).await,
            "ingest" | "pdf" => self.cmd_ingest(rest).await,
            "ask" | "q" => self.cmd_ask(rest).await,
            "check" => self.cmd_check(rest).await,
            "sum" => self.cmd_sum(rest).await,
            "del" | "rm" => self.cmd_del(rest).await,
            "undo" => self.cmd_undo().await,
            _ => {
                outln!(self, "未知命令: {cmd}。输入 help 查看帮助。");
                Ok(())
            }
        };
        crate::logging::info(format!(
            "命令 `{cmd}` 用时 {:.2}s：{}",
            t0.elapsed().as_secs_f64(),
            if result.is_ok() { "ok" } else { "err" }
        ));
        result?;
        self.update_completions();
        Ok(())
    }

    fn cmd_help(&self) -> Result<()> {
        let h = "\
PaperHelper 命令：
  ingest <pdf>            解析 PDF 并按风格生成笔记（--style <风格id>，默认 four）
  ingest --note <文件>    直接导入笔记/讲义（不调 LLM；--kind paper|note|lecture）
  ingest --read <pdf>     仅阅读 PDF 原件（不调 LLM、不生成笔记；扫描件也能读）
  ingest --text <txt>     直接读取文本文件（跳过PDF解析）
  ingest --ocr <pdf>      OCR 识别扫描件（需 tesseract）
  ingest --extra 「要求」   本次额外要求（拼到所选风格提示词之后）
                          例: ingest --style translate paper.pdf   （逐段翻译）
                              ingest --note lecture.md             （直接导入讲义）
                              ingest --style lecture lecture.pdf   （LLM 整理成讲义提纲）
  styles                  列出笔记风格（styles show <id> 看提示词；Web「管理风格」可编辑）
  ask <编号> <问题>        基于论文全文+笔记回答，解释插入笔记对应位置
                          编号见 blocks 或导出笔记的标题（如 3.2）；不填编号则关键词匹配
                          例: ask 3.2 BERTScore的公式里max_k是什么意思
  check <编号> <想法>      与 ask 类似但不写入笔记，用于核对想法
                          例: check 3.2 我觉得BERTScore就是余弦相似度，对吗
  sum                      把当前节点子树的追问折叠并替换为「总结：…」（可点击展开）
  del [--yes]              删除当前节点及其子树（需 --yes 确认；根节点不可删）
  undo                     撤销上一次编辑/删除（内存多级，最多 20 步）
  blocks                   列出笔记结构（带编号）
  note                     打印完整笔记(Markdown)
  tree                     以文件树展示对话轨迹（带 [n] 编号）
  goto <n|id前缀>          跳到对话树某节点，其根路径成为上下文
  stats                    查看本次/累计 token 用量与成本
  budget <n>              设置 token 预算(0=不限)，到上限自动中断
  save [file]              保存会话(默认 session.json)
  load <file>              加载会话
  export md|mindmap|html <file> 导出笔记为 Markdown / 思维导图 / HTML
                          html: 自包含网页(KaTeX公式渲染+对话树侧栏)，浏览器打开；
                          也是实验特性，仅供测试体验，正式笔记建议用 md
  papers                   列出已读论文
  concepts                 列出已学概念(跨论文)
  graph [build|rebuild|clear|show]  知识图谱：LLM 判断概念间关系
                           build 只整理新概念(懒惰更新)；rebuild 重建全部；
                           clear 清空关系；show 查看状态(默认)
  config show              查看配置
  config set <k> <v>       设置(如 llm.api_key / llm.model / llm.context_length)
  config presets [id]      列出内置服务商预设 / 一键填入(deepseek/paratera/ollama/custom)
  config test              测试 LLM 连接(端点/Key/模型)，失败显示原始响应
  new                      新建会话
  exit                     退出（自动保存会话）

启动方式：
  paperhelper              新会话
  paperhelper -s <序号>    恢复指定会话（先用 -l 查看序号）
  paperhelper -l           列出所有已保存会话";
        outln!(self, "{h}");
        Ok(())
    }

    /// config show / set：查看或修改配置。set 经 `set_config` 改内存再整体
    /// `Config::save()` 落盘 config.toml；api_key 回显时脱敏。模型/端点可切换
    /// （OpenAI ⇄ DeepSeek ⇄ 本地 Ollama），这正是"用户可自由改 API 配置"的入口。
    async fn cmd_config(&mut self, rest: &str) -> Result<()> {
        let (sub, args) = split_cmd(rest);
        match sub {
            "" | "show" => {
                let k = &self.config.llm;
                outln!(self, "=== 配置 ===");
                outln!(self, "llm.api_endpoint   = {}", k.api_endpoint);
                outln!(self, "llm.api_key        = {}", mask_key(&k.api_key));
                outln!(self, "llm.model           = {}", k.model);
                outln!(self, "llm.context_length  = {}", k.context_length);
                outln!(self, "llm.thinking_mode   = {}", k.thinking_mode);
                outln!(self, "llm.pdf_input       = {} (file模式未实现,均走text)", k.pdf_input);
                outln!(self, "pricing.input_price_per_1m  = {}", self.config.pricing.input_price_per_1m);
                outln!(self, "pricing.output_price_per_1m = {}", self.config.pricing.output_price_per_1m);
                outln!(self, "budget.token_budget = {} (0=不限)", self.config.budget.token_budget);
                outln!(self, "提示：api_endpoint 需是完整 URL（含 /chat/completions），如 https://api.deepseek.com/v1/chat/completions");
            }
            "set" => {
                let (key, val) = split_cmd(args);
                if key.is_empty() {
                    outln!(self, "用法: config set <key> <value>");
                    let keys: Vec<&str> = CONFIG_KEY_DEFS.iter().map(|d| d.key).collect();
                    outln!(self, "可设: {}", keys.join(" "));
                    outln!(self, "常见端点：");
                    outln!(self, "  DeepSeek : https://api.deepseek.com/v1/chat/completions  model=deepseek-v4-pro");
                    outln!(self, "  OpenAI   : https://api.openai.com/v1/chat/completions    model=gpt-4o-mini");
                    outln!(self, "  本地Ollama: http://localhost:11434/v1/chat/completions    model=qwen2.5:7b");
                    return Ok(());
                }
                let val = normalize_path_arg(val);
                self.set_config(key, &val)?;
                self.config.save()?;
                // api_key 脱敏回显，避免明文泄露
                let display = if key == "llm.api_key" { mask_key(&val) } else { val.clone() };
                outln!(self, "已设置 {key} = {display}（已写入 .paperhelper/config.toml）");
            }
            "test" => self.cmd_config_test().await?,
            "presets" | "preset" => self.cmd_config_presets(args)?,
            _ => outln!(self, "用法: config [show | set <key> <value> | presets [id] | test]"),
        }
        Ok(())
    }

    /// `config presets [id]`：列出内置服务商预设；带 id（或序号）时把
    /// Endpoint/模型/上下文/单价预填进配置并保存。预设只是"预填表单"，
    /// 不代理请求：用户仍用自己的 Key 从本机直连所选服务商。
    fn cmd_config_presets(&mut self, args: &str) -> Result<()> {
        let (id, _) = split_cmd(args);
        let list = crate::presets::all();
        if id.is_empty() {
            outln!(self, "=== 内置服务商预设（config presets <id> 一键填入）===");
            for (i, p) in list.iter().enumerate() {
                outln!(self, "{}. {:<24} {}", i + 1, p.name, p.id);
                outln!(self, "   端点: {}", if p.endpoint.is_empty() { "(自行填写)" } else { p.endpoint });
                outln!(self, "   模型: {}   上下文: {}   {}", if p.model.is_empty() { "(自行填写)" } else { p.model }, p.context_length, p.note);
                if p.needs_key {
                    outln!(self, "   Key : {}", p.key_url);
                }
            }
            outln!(self, "用法: config presets deepseek  → 填入端点/模型/上下文/单价（不改 Key）");
            outln!(self, "      再用 config set llm.api_key <你的Key> 或启动 Web 首启向导填写 Key");
            return Ok(());
        }
        let preset = list
            .iter()
            .find(|p| p.id.eq_ignore_ascii_case(id))
            .or_else(|| {
                let n = id.parse::<usize>().ok()?;
                if n >= 1 { list.get(n - 1) } else { None }
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "没有预设 `{id}`。可用: {}",
                    list.iter().map(|p| p.id).collect::<Vec<_>>().join(" / ")
                )
            })?;
        if !preset.endpoint.is_empty() {
            self.config.llm.api_endpoint = preset.endpoint.to_string();
        }
        if !preset.model.is_empty() {
            self.config.llm.model = preset.model.to_string();
        }
        self.config.llm.context_length = preset.context_length;
        self.config.llm.thinking_mode = preset.thinking;
        self.config.pricing.input_price_per_1m = preset.input_price_per_1m;
        self.config.pricing.output_price_per_1m = preset.output_price_per_1m;
        self.config.save()?;
        outln!(self, "{} 已应用预设「{}」：", "✓".green().bold(), preset.name);
        outln!(self, "  llm.api_endpoint = {}", self.config.llm.api_endpoint);
        outln!(self, "  llm.model        = {}", self.config.llm.model);
        outln!(self, "  llm.context_length = {}", self.config.llm.context_length);
        outln!(self, "  pricing = {}/{} per 1M", self.config.pricing.input_price_per_1m, self.config.pricing.output_price_per_1m);
        if preset.needs_key {
            outln!(self, "  下一步: config set llm.api_key <你的Key>（申请: {}）", preset.key_url);
        }
        outln!(self, "  提示: config test 可验证连接");
        Ok(())
    }

    /// `config test`：用当前配置发一条最小请求，验证端点/Key/模型，失败展示原始响应。
    async fn cmd_config_test(&mut self) -> Result<()> {
        let cfg = self.config.llm.clone();
        outln!(self, "正在测试 LLM 连接：{}（模型 {}）…", cfg.api_endpoint, cfg.model);
        self.emitter.progress("测试连接中…");
        interrupt::reset();
        let r = llm::test(&self.client, &cfg).await;
        self.emitter.progress_done();
        if r.ok {
            outln!(
                self,
                "{} 连接成功（HTTP {}，{}ms）",
                "✓".green().bold(),
                r.status,
                r.latency_ms
            );
            outln!(self, "  模型: {}", r.model);
            outln!(self, "  回复: {}", r.reply);
            outln!(self, "  usage: in={} out={}", r.input_tokens, r.output_tokens);
        } else {
            outln!(
                self,
                "{} 连接失败（HTTP {}，{}ms）",
                "✗".red().bold(),
                r.status,
                r.latency_ms
            );
            outln!(self, "原始响应：\n{}", r.raw);
        }
        Ok(())
    }

    pub fn set_config(&mut self, key: &str, val: &str) -> Result<()> {
        match key {
            "llm.api_key" => self.config.llm.api_key = val.into(),
            "llm.api_endpoint" => self.config.llm.api_endpoint = val.into(),
            "llm.model" => self.config.llm.model = val.into(),
            "llm.context_length" => self.config.llm.context_length = val.parse().context("需要整数")?,
            "llm.thinking_mode" => self.config.llm.thinking_mode = parse_bool(val),
            "llm.pdf_input" => self.config.llm.pdf_input = parse_bool(val),
            "pricing.input_price_per_1m" => self.config.pricing.input_price_per_1m = val.parse().context("需要数字")?,
            "pricing.output_price_per_1m" => self.config.pricing.output_price_per_1m = val.parse().context("需要数字")?,
            "budget.token_budget" => self.config.budget.token_budget = val.parse().context("需要整数")?,
            "update.auto_check" => self.config.update.auto_check = parse_bool(val),
            "update.source_url" => self.config.update.source_url = val.trim().into(),
            _ => {
                let keys: Vec<&str> = CONFIG_KEY_DEFS.iter().map(|d| d.key).collect();
                bail!("未知配置项: {key}。可设: {}", keys.join(" "));
            }
        }
        Ok(())
    }

    async fn cmd_budget(&mut self, rest: &str) -> Result<()> {
        if rest.trim().is_empty() {
            outln!(self, "当前 token 预算: {} (0=不限)", self.config.budget.token_budget);
            return Ok(());
        }
        self.config.budget.token_budget = rest.trim().parse().context("需要整数")?;
        self.config.save()?;
        outln!(self, "token 预算已设为 {}", self.config.budget.token_budget);
        Ok(())
    }

    async fn cmd_blocks(&self) -> Result<()> {
        let note = self.session.notes.as_ref().ok_or_else(|| anyhow!("还没有笔记，先 `ingest <pdf>`"))?;
        for (b, depth) in note.flatten() {
            let indent = "  ".repeat(depth);
            let text: String = b.text.chars().take(60).collect();
            let nexpl = b.explanations.len();
            let expl = if nexpl > 0 { format!("  [{}条解释]", nexpl) } else { String::new() };
            let num = if b.number.is_empty() { format!("{:>6}", "") } else { format!("{:>6}", b.number) };
            outln!(self, "{} {}{} {} {}{}", num, indent, b.kind.tag(), text, "", expl);
        }
        Ok(())
    }

    async fn cmd_note(&self) -> Result<()> {
        let note = self.session.notes.as_ref().ok_or_else(|| anyhow!("还没有笔记"))?;
        outln!(self, "{}", export::to_markdown(note));
        Ok(())
    }

    async fn cmd_stats(&self) -> Result<()> {
        let s = &self.session.stats;
        let k = &self.kb.stats;
        outln!(self, "=== 用量统计 ===");
        outln!(self, "本次会话: {} 次调用 | 输入 {} / 输出 {} tok | 小计 ${:.6}", s.calls, s.total_input, s.total_output, s.total_cost);
        outln!(self, "累计(跨会话): {} 次调用 | 输入 {} / 输出 {} tok | 小计 ${:.6}", k.calls, k.total_input, k.total_output, k.total_cost);
        let tot = s.total_tokens() + k.total_tokens();
        if self.config.budget.token_budget > 0 {
            outln!(self, "预算: {} (累计已用 {:.1}%)", self.config.budget.token_budget,
                tot as f64 / self.config.budget.token_budget as f64 * 100.0);
        } else {
            outln!(self, "预算: 未设置（`budget <n>` 设置）");
        }
        Ok(())
    }

    /// goto <编号|前缀>：跳转对话树节点。数字按 DFS 序（tree 里的 [n]），
    /// 其它当 id 前缀（唯一前缀即可）。跳转后打印该节点的根路径问答，供确认。
    async fn cmd_goto(&mut self, rest: &str) -> Result<()> {
        let arg = rest.trim();
        if arg.is_empty() {
            bail!("用法: goto <编号|id前缀>。先用 `tree` 查看编号。");
        }
        let node = if let Ok(n) = arg.parse::<usize>() {
            self.session.conversation.goto_index(n)
        } else {
            self.session.conversation.goto_prefix(arg)
        };
        match node {
            Some(n) => {
                outln!(self, "已跳转到: {}", n.label);
                outln!(self, "--- 根路径对话 ---");
                for (i, x) in self.session.conversation.path_to_current().iter().enumerate() {
                    outln!(self, "{}. Q: {}", i + 1, x.question);
                }
            }
            None => outln!(self, "找不到节点 {arg}。用 `tree` 查看可用节点。"),
        }
        Ok(())
    }

    async fn cmd_papers(&self) -> Result<()> {
        if self.kb.papers.is_empty() {
            outln!(self, "（还没有读过论文）");
            return Ok(());
        }
        for p in &self.kb.papers {
            let kind = match p.kind.as_str() {
                "lecture" => "讲义",
                "note" => "笔记",
                _ => "论文",
            };
            outln!(self, "- [{}] 《{}》[{}]（{}）", &p.id[..6.min(p.id.len())], p.title, kind, p.path);
        }
        Ok(())
    }

    /// styles [show <id>]：列出可用笔记风格（或打印某个风格的提示词）。
    async fn cmd_styles(&self, rest: &str) -> Result<()> {
        let rest = rest.trim();
        if let Some(id) = rest.strip_prefix("show ") {
            let (meta, prompt) = crate::prompts::style_prompt(id.trim())?;
            outln!(self, "# {}（{}） scope={}\n{}", meta.id, meta.label, meta.scope, prompt);
            outln!(self, "（固定的输出格式要求与资料全文由程序自动附加，不在此显示）");
            return Ok(());
        }
        let styles = crate::prompts::list_styles()?;
        for s in styles {
            outln!(
                self,
                "- {}（{}）[{}]{}: {}",
                s.id,
                s.label,
                s.scope,
                if s.builtin { " 内置" } else { " 自定义" },
                s.desc
            );
        }
        Ok(())
    }

    async fn cmd_concepts(&self) -> Result<()> {
        if self.kb.concepts.is_empty() {
            outln!(self, "（还没有学过概念）");
            return Ok(());
        }
        for c in &self.kb.concepts {
            let d: String = c.definition.chars().take(80).collect();
            outln!(self, "- {}（来自《{}》）: {}", c.name, c.paper_title, d);
        }
        Ok(())
    }

    async fn cmd_save(&mut self, rest: &str) -> Result<()> {
        let path = if rest.trim().is_empty() { "session.json".to_string() } else { normalize_path_arg(rest) };
        self.session.export_path = self.export_path.clone();
        self.session.save(Path::new(&path))?;
        outln!(self, "会话已保存到 {path}");
        Ok(())
    }

    async fn cmd_load(&mut self, rest: &str) -> Result<()> {
        if rest.trim().is_empty() {
            bail!("用法: load <文件>");
        }
        let path = normalize_path_arg(rest);
        // 先保存当前会话：在途 LLM 任务稍后仍能写回它
        if let Err(e) = self.auto_persist() {
            crate::logging::warn(format!("加载会话前保存失败: {e:#}"));
        }
        self.session = Session::load(Path::new(&path))?;
        // 从文件加载的会话可能没有 id：补一个，避免在途任务用占位符 key 混淆
        self.ensure_session_id();
        self.export_path = self.session.export_path.clone();
        outln!(self, "已加载会话: 笔记={}, 对话节点={}",
            self.session.notes.is_some(),
            self.session.conversation.nodes.len());
        Ok(())
    }

    /// export <md|mindmap|html> [file]：按格式渲染整篇笔记（md→Markdown、
    /// mindmap→markmap、html→自包含 HTML），写盘。路径处理见上（自动补后缀、
    /// 缺省用 ingest 笔记名的 stem）。渲染本身在 export.rs，这里只做分流与落盘。
    async fn cmd_export(&self, rest: &str) -> Result<()> {
        let (fmt, path) = split_cmd(rest);
        // 格式 → 默认后缀
        let default_ext = match fmt {
            "md" | "markdown" => ".md",
            "mindmap" | "mm" => ".mm",
            "html" | "htm" => ".html",
            _ => bail!("未知格式: {fmt}（可用: markdown, mindmap, html）"),
        };
        // 目标路径：无文件名时用 ingest 时设置的笔记名（export_path 去后缀的 stem）
        let mut path = if path.is_empty() {
            match &self.export_path {
                Some(p) => Path::new(p)
                    .with_extension("") // 去后缀，后续按格式加
                    .to_string_lossy()
                    .trim_end_matches('.').to_string(),
                None => "note".to_string(),
            }
        } else {
            normalize_path_arg(path)
        };
        // 智能补后缀：已有（任意）后缀则不添加
        if !Path::new(&path).extension().is_some_and(|e| !e.is_empty()) {
            path.push_str(default_ext);
        }
        let note = self.session.notes.as_ref().ok_or_else(|| anyhow!("还没有笔记"))?;
        let content = match fmt {
            "md" | "markdown" => export::to_markdown(note),
            "mindmap" | "mm" => export::to_mindmap(note),
            "html" | "htm" => export::to_html(note, &self.session.conversation, &self.session.annotations),
            _ => unreachable!(),
        };
        std::fs::write(&path, content)?;
        outln!(self, "已导出到 {path}");
        Ok(())
    }

    // ===== 以下为异步 LLM 相关命令（ingest / ask）=====

    /// ingest 主流程（PDF/文本/OCR 三种模式）：
    /// 1. 抽取全文（text 直读 / pdf 走 PyMuPDF / ocr 走 tesseract，均带进度条与打断）；
    /// 2. 后台线程 `llm::chat` 生成笔记（流式打印到终端便于观察）；
    /// 3. 成功后 `parse_markdown_note` 解析成树、登记知识库 Paper；
    ///    首次询问导出文件名（写 export_path，之后 ask 自动同步到该文件）。
    /// 全程记录 token，超预算/被打断即中止且不写会话。
    /// ingest 的 **prepare 段**（持锁）：解析参数、抽取文本、组装提示词。
    /// 返回 `IngestPrep::Llm` 时，其任务可在**不持 App 锁**时流式执行。
    pub async fn prepare_ingest(&mut self, rest: &str) -> Result<IngestPrep> {
        let rest = rest.trim();
        // 先剥离选项（--style / --extra / --kind / --note，与 --text/--ocr 任意顺序）
        let (style_raw, rest) = take_style_arg(rest)?;
        let style = if style_raw.is_empty() { "four".to_string() } else { style_raw };
        let (extra, rest) = take_value_arg(&rest, "extra");
        let (kind_raw, rest) = take_value_arg(&rest, "kind");
        let (direct_import, rest) = take_bool_arg(&rest, "note");
        let (read_only, rest) = take_bool_arg(&rest, "read");
        let rest = rest.trim();
        // 解析选项（路径参数做 shell 风格还原：剥引号/反斜杠转义，支持含空格文件名）
        let (mode, file_path) = if let Some(r) = rest.strip_prefix("--text ") {
            ("text", normalize_path_arg(r))
        } else if let Some(r) = rest.strip_prefix("--ocr ") {
            ("ocr", normalize_path_arg(r))
        } else if rest == "--text" || rest == "--ocr" {
            bail!("用法: ingest [--style <风格>] [--note] [--extra <要求>] [--text|--ocr] <路径>");
        } else {
            ("pdf", normalize_path_arg(rest))
        };

        if file_path.is_empty() {
            bail!("用法: ingest [--style <风格>] [--note] [--extra <要求>] <pdf路径>\n  ingest --read <pdf路径>  仅阅读原件（不生成笔记）\n  ingest --text <txt路径>  直接读文本（跳过PDF解析）\n  ingest --ocr <pdf路径>  OCR提取（需安装 tesseract）");
        }
        let p = Path::new(&file_path);
        if !p.exists() {
            bail!("文件不存在: {file_path}");
        }
        // --read：仅阅读 PDF 原件（不调 LLM、不生成笔记；扫描件也能读）
        if read_only {
            if !file_path.to_ascii_lowercase().ends_with(".pdf") {
                bail!("--read 只支持 PDF 文件（当前: {file_path}）");
            }
            return Ok(IngestPrep::Read { file_path });
        }
        // --note：直接导入笔记/讲义（不调 LLM，0 token）
        if direct_import {
            let kind = match kind_raw.trim() {
                "paper" => "paper",
                "lecture" => "lecture",
                _ => "note",
            };
            return Ok(IngestPrep::Direct { file_path, mode: mode.to_string(), kind: kind.to_string() });
        }
        interrupt::reset();

        // 1. 抽取文本（PDF/OCR 子进程已异步，可被 Ctrl-C / Web 停止打断）
        let progress_msg = match mode {
            "text" => "读取文本文件…",
            "ocr" => "OCR 识别中（可能较慢，可 Ctrl-C/停止 打断）…",
            _ => "解析 PDF…",
        };
        self.emitter.progress(progress_msg);
        let raw_text = match mode {
            "text" => tokio::fs::read_to_string(&file_path)
                .await
                .with_context(|| format!("读取文本文件失败: {file_path}"))?,
            "ocr" => pdf::ocr_extract(Path::new(&file_path)).await?,
            _ => pdf::extract_pages(Path::new(&file_path)).await?.join("\n\n"),
        };
        self.emitter.progress_done();
        if raw_text.trim().is_empty() {
            bail!("文本内容为空（可能是扫描件，试试 ingest --ocr <pdf>）");
        }
        let char_count = raw_text.chars().count();
        outln!(self, "{} 文本已就绪: {} 字符", "✓".green().bold(), char_count);

        if interrupt::is_interrupted() {
            bail!("已打断");
        }

        // 2. 确定笔记导出文件名（终端交互询问；Web 用预设 export_path）
        let export_file = self.ask_export_name(&file_path)?;

        // 3. 预算检查 + 组装提示词（风格注册表 + 固定输出契约）
        if !self.check_budget()? {
            bail!("已达 token 预算，无法继续。用 `budget <n>` 调整。");
        }
        let (meta, _template) = crate::prompts::style_prompt(&style)?;
        crate::logging::info(format!(
            "生成笔记：风格={}（{}），scope={}",
            meta.id, meta.label, meta.scope
        ));
        let kind = match kind_raw.trim() {
            "paper" => "paper",
            "note" => "note",
            "lecture" => "lecture",
            _ => {
                if meta.scope == "note" {
                    "lecture"
                } else {
                    "paper"
                }
            }
        };
        let prompt = crate::prompts::compose_style_prompt(&style, &raw_text, &extra)?;
        let msgs = vec![
            Message::text("system", "你是笔记生成助手，只输出 Markdown。"),
            Message::text("user", prompt),
        ];
        // 有笔记/对话的会话先分配 id：在途任务完成时按此 key 写回发起会话
        self.ensure_session_id();
        let session = self.session_key();
        Ok(IngestPrep::Llm(Box::new(IngestJob {
            msgs,
            cfg: self.config.llm.clone(),
            client: self.client.clone(),
            session: session.clone(),
            epoch: self.session_epoch(&session),
            raw_text,
            source_path: file_path,
            export_file,
            kind: kind.to_string(),
        })))
    }

    /// ingest 的 **落地段**（持锁）：解析生成结果、登记知识库与会话、导出。
    /// 用户已切换到别的会话时，把结果写回发起会话；发起会话被改动/删除时另存为新会话。
    pub fn commit_ingest(&mut self, job: IngestJob, res: crate::llm::LlmResult) -> Result<()> {
        let target = job.session.clone();
        let current = self.session_key();
        if target != current {
            let ok = job.epoch == self.session_epoch(&target)
                && crate::paths::session_path(&target).exists();
            if ok {
                let name = self.with_session(&target, |app| {
                    app.commit_ingest_local(&job, res)?;
                    Ok(app.session.session_name.clone())
                })?;
                outln!(self, "{} 导入完成，笔记已写入会话《{}》", "✓".green().bold(), name);
                self.update_completions();
                return Ok(());
            }
            return self.commit_ingest_as_new(job, res, None);
        }
        if job.epoch != self.session_epoch(&target) {
            return self.commit_ingest_as_new(job, res, Some("导入期间会话已变更"));
        }
        self.commit_ingest_local(&job, res)?;
        self.update_completions();
        Ok(())
    }

    /// ingest 落地到「当前会话」（调用前已确保目标会话就是当前会话）。
    fn commit_ingest_local(&mut self, job: &IngestJob, res: crate::llm::LlmResult) -> Result<()> {
        self.record_usage(res.input_tokens, res.output_tokens);
        if res.truncated() {
            outerr!(
                self,
                "{} 模型输出达到上限（finish_reason=length），笔记可能被截断。\
                 可换更长输出上限的模型，或改用更短的论文/文本（翻译风格尤其容易触发）。",
                "⚠️ ".yellow()
            );
        }
        let mut note = notes::parse_markdown_note(&res.content, &job.raw_text);
        note.material_kind = job.kind.clone();
        let title = note.title.clone();
        let nblocks = note.count_blocks();
        outln!(self, "{} 笔记已生成: 《{}》({} 个结构块)", "✓".green().bold(), title, nblocks);
        self.register_note(note, &job.source_path, &job.export_file, res.input_tokens, res.output_tokens, false, false)
            .with_context(|| format!("导入《{title}》"))
    }

    /// 无法写回发起会话时，把导入结果另存为新会话（知识库用量记全局）。
    fn commit_ingest_as_new(
        &mut self,
        job: IngestJob,
        res: crate::llm::LlmResult,
        why: Option<&str>,
    ) -> Result<()> {
        let (in_tok, out_tok) = (res.input_tokens, res.output_tokens);
        let cost = self.price_cost(in_tok, out_tok);
        self.kb.stats.add(in_tok, out_tok, cost);
        let mut note = notes::parse_markdown_note(&res.content, &job.raw_text);
        note.material_kind = job.kind.clone();
        let paper_id = uuid::Uuid::new_v4().to_string();
        note.paper_id = paper_id.clone();
        let title = note.title.clone();
        let mut sess = Session::default();
        sess.notes = Some(note);
        sess.current_paper_id = Some(paper_id.clone());
        sess.session_name = title.clone();
        sess.session_id = crate::paths::new_session_stamp();
        sess.export_path = Some(job.export_file.clone());
        sess.stats.add(in_tok, out_tok, cost);
        self.kb.add_paper(Paper {
            id: paper_id,
            title: title.clone(),
            path: job.source_path.clone(),
            read_at: Utc::now().to_rfc3339(),
            kind: job.kind.clone(),
            pinned: false,
        });
        self.kb.save()?;
        crate::paths::ensure_sessions_dir()?;
        sess.save(&crate::paths::session_path(&sess.session_id))?;
        outerr!(
            self,
            "{} {}，结果已另存为新会话《{}》（{}）",
            "⚠️ ".yellow(),
            why.unwrap_or("发起会话已删除"),
            title,
            sess.session_id
        );
        Ok(())
    }

    /// ingest 主流程（CLI）：prepare → 锁外 LLM → commit。
    async fn cmd_ingest(&mut self, rest: &str) -> Result<()> {
        match self.prepare_ingest(rest).await? {
            IngestPrep::Direct { file_path, mode, kind } => {
                self.import_note(&file_path, &mode, &kind).await
            }
            IngestPrep::Read { file_path } => self.import_readonly(&file_path).await,
            IngestPrep::Llm(job) => {
                let emitter = self.emitter.clone();
                match job.run(&emitter).await {
                    Ok(res) => self.commit_ingest(*job, res),
                    Err(e) => Err(e),
                }
            }
        }
    }

    /// 直接导入笔记/讲义（不调 LLM，0 token）：抽取文本 → 解析成笔记树 → 登记知识库。
    /// `mode`：text（md/txt）/ ocr / pdf；`kind`：paper / note / lecture。
    pub(crate) async fn import_note(&mut self, file_path: &str, mode: &str, kind: &str) -> Result<()> {
        let progress_msg = match mode {
            "text" => "读取文本文件…",
            "ocr" => "OCR 识别中（可能较慢，可 Ctrl-C/停止 打断）…",
            _ => "解析 PDF…",
        };
        self.emitter.progress(progress_msg);
        let raw_text = match mode {
            "text" => tokio::fs::read_to_string(file_path)
                .await
                .with_context(|| format!("读取文本文件失败: {file_path}"))?,
            "ocr" => pdf::ocr_extract(Path::new(file_path)).await?,
            _ => pdf::extract_pages(Path::new(file_path)).await?.join("\n\n"),
        };
        self.emitter.progress_done();
        if raw_text.trim().is_empty() {
            bail!("文本内容为空（可能是扫描件，试试 ingest --note --ocr <pdf>）");
        }
        let export_file = self.ask_export_name(file_path)?;
        let stem = Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("note");
        let title_guess: String = stem.chars().take(20).collect();
        let mut note = notes::parse_import_note(&raw_text, &title_guess);
        note.material_kind = kind.to_string();
        let title = note.title.clone();
        let nblocks = note.count_blocks();
        let kind_label = match kind {
            "lecture" => "讲义",
            "paper" => "资料",
            _ => "笔记",
        };
        outln!(self, "{} 已导入: 《{}》({} 个结构块，不调 LLM)", "✓".green().bold(), title, nblocks);
        self.register_note(note, file_path, &export_file, 0, 0, false, false)
            .with_context(|| format!("导入{kind_label}《{title}》"))?;
        self.update_completions();
        Ok(())
    }

    /// 仅阅读 PDF 原件（不调 LLM、不生成笔记）：尽力抽文本作 ask 上下文，
    /// 扫描件 / 解析失败也照样可读（阅读器走页面截图提问）。
    pub(crate) async fn import_readonly(&mut self, file_path: &str) -> Result<()> {
        self.emitter.progress("解析 PDF…");
        let (raw_text, outcome) = match pdf::extract_pages_lenient(Path::new(file_path)).await {
            Ok(pages) => {
                let text = pages.join("\n\n");
                let outcome = if text.trim().is_empty() {
                    crate::logging::warn(format!(
                        "仅阅读模式未抽到文本，可能是扫描/图片版: {file_path}"
                    ));
                    ExtractOutcome::Empty
                } else {
                    ExtractOutcome::Text
                };
                (text, outcome)
            }
            Err(e) => {
                crate::logging::warn(format!("仅阅读模式抽取文本失败（仍可阅读）: {e:#}"));
                (String::new(), ExtractOutcome::Failed)
            }
        };
        self.emitter.progress_done();
        if let Some(notice) = readonly_extract_notice(&outcome) {
            self.emitter.stderr(format!("⚠️  {notice}"));
        }
        let stem = Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("paper");
        let title: String = stem.chars().take(40).collect();
        let note = notes::readonly_note(title.as_str(), raw_text);
        outln!(self, "{} 已打开: 《{}》(仅阅读，不生成笔记，不调 LLM)", "✓".green().bold(), title);
        self.register_note(note, file_path, "", 0, 0, true, true)
            .with_context(|| format!("打开《{title}》"))?;
        self.update_completions();
        Ok(())
    }

    /// 登记笔记到知识库与会话（ingest / import_note 共用）：
    /// 写 KB Paper（kind 取自 note.material_kind）、重建会话根节点、写导出文件。
    /// `skip_export`：仅阅读模式不写导出文件；`read_only`：标记为仅阅读会话。
    #[allow(clippy::too_many_arguments)]
    fn register_note(
        &mut self,
        mut note: notes::Note,
        file_path: &str,
        export_file: &str,
        input_tokens: u64,
        output_tokens: u64,
        skip_export: bool,
        read_only: bool,
    ) -> Result<()> {
        let paper_id = uuid::Uuid::new_v4().to_string();
        let title = note.title.clone();
        let nblocks = note.count_blocks();
        let kind = if note.material_kind.is_empty() {
            "paper".to_string()
        } else {
            note.material_kind.clone()
        };
        self.bump_epoch();
        note.paper_id = paper_id.clone();
        note.source_path = Some(file_path.to_string());
        self.session.notes = Some(note);
        self.session.current_paper_id = Some(paper_id.clone());
        self.session.read_only = read_only;
        self.kb.add_paper(Paper {
            id: paper_id.clone(),
            title: title.clone(),
            path: file_path.to_string(),
            read_at: Utc::now().to_rfc3339(),
            kind: kind.clone(),
            pinned: false,
        });
        self.kb.save()?;
        let cost = input_tokens as f64 * self.config.pricing.input_price_per_1m / 1_000_000.0
            + output_tokens as f64 * self.config.pricing.output_price_per_1m / 1_000_000.0;
        // 重建对话树，0 号根节点代表「已导入、尚未追问」状态
        let root_id = uuid::Uuid::new_v4().to_string();
        self.session.conversation = Conversation::default();
        self.session.conversation.add_exchange(ConvNode {
            id: root_id.clone(),
            parent: None,
            question: if read_only {
                format!("（已打开《{}》，仅阅读，未生成笔记）", title)
            } else {
                format!("（已导入《{}》，{} 个结构块）", title, nblocks)
            },
            quote: None,
            answer: String::new(),
            block_id: None,
            explanation_id: None,
            input_tokens,
            output_tokens,
            cost,
            created_at: Utc::now().to_rfc3339(),
            label: if read_only {
                format!("打开《{}》", title)
            } else {
                format!("导入《{}》", title)
            },
        });
        self.session.conversation.current = Some(root_id);
        if skip_export {
            self.export_path = None;
            return Ok(());
        }
        // 导出 markdown 笔记
        if let Some(parent) = Path::new(export_file).parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                bail!("导出目录不存在: {}（请先创建目录，或改用当前目录下的文件名）", parent.display());
            }
        }
        self.export_path = Some(export_file.to_string());
        if let Some(note) = &self.session.notes {
            std::fs::write(export_file, export::render_for(export_file, note, &self.session.conversation, &self.session.annotations))?;
            outln!(self, "{} 笔记已导出到 {}", "✓".green().bold(), export_file);
        }
        Ok(())
    }

    /// 确定笔记导出文件名：终端交互询问；Web 用预设 export_path，否则用文件名的默认名。
    fn ask_export_name(&mut self, file_path: &str) -> Result<String> {
        let stem = Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("note");
        let title_guess: String = stem.chars().take(20).collect();
        let default_name = format!("笔记_{title_guess}.md");
        if self.emitter.is_terminal() {
            print!("请输入笔记导出文件名（回车默认 {default_name}）: ");
            io::stdout().flush()?;
            let mut name = String::new();
            io::stdin().lock().read_line(&mut name)?;
            let name = name.trim();
            if name.is_empty() {
                Ok(default_name)
            } else {
                Ok(sanitize_filename(&normalize_path_arg(name)))
            }
        } else {
            let name = self.export_path.clone().unwrap_or(default_name);
            Ok(sanitize_filename(&name))
        }
    }

    /// ask 主流程（追问 → 写笔记）：
    /// 1. 解析 `ask <编号> <问题>`（无编号则 locate 关键词匹配，编号对应
    ///    build_context_messages 里按 find_section_by_number 定位块文本）；
    /// 2. 预算检查 → `build_context_messages` 拼上下文 → 后台线程 LLM 流式回答
    ///    （进度条+实时打印，Ctrl-C 可打断，token 精确统计）；
    /// 3. 回答写入会话：定位 explanation 父（当前对话节点的 explanation_id），
    ///    在笔记树对应位置插入/嵌套 Explanation，block_id/explanation_id 落到新对话节点；
    /// 4. 回答自动提取概念（`extract_concept`，只认模型标注的 [[概念: …]]）
    ///    存入知识库；自动导出笔记到 export_path。
    async fn cmd_ask(&mut self, args: &str) -> Result<()> {
        let args = args.trim();
        // ask --help：打印用法
        if args == "--help" || args == "-h" || args.is_empty() {
            outln!(self, "用法: ask <编号> <问题>");
            outln!(self, "  <编号>    笔记中 Section 的编号（见 blocks 或导出笔记标题，如 3.2）");
            outln!(self, "  <问题>    你的追问内容");
            outln!(self, "例:");
            outln!(self, "  ask 3.2 BERTScore的公式里max_k是什么意思");
            outln!(self, "  ask 2.1 灰盒方法为什么对黑盒不适用");
            outln!(self, "  ask --no-concept 3.2 这里的符号指什么   （本次不写入「已学概念」）");
            outln!(self, "说明: 解释会插入笔记对应 Section 下方。不填编号则退化为关键词匹配。");
            return Ok(());
        }
        let job = self.prepare_ask_command(args, false)?;
        self.run_ask(job).await.map(|_| ())
    }

    /// ask/check 的 **prepare 段入口**（CLI 与 Web 三段式共用）：
    /// 解析参数 → 定位块 → `prepare_ask`；返回的 `AskJob` 可在不持 App 锁时流式执行。
    pub fn prepare_ask_command(&mut self, args: &str, is_check: bool) -> Result<AskJob> {
        let args = args.trim();
        // `--no-concept`：本次回答不写入「已学概念」（适合“这段什么意思”这类操作性提问）
        let (record_concept, args) = if let Some(rest) = strip_flag(args, "--no-concept") {
            (false, rest)
        } else {
            (true, args.to_string())
        };
        let args = args.trim();
        let (block_num, question) = parse_ask_args(args);
        let question = question.trim();
        if question.is_empty() {
            bail!(
                "用法: {} <编号> <问题>   例: {} 3.2 BERTScore是什么",
                if is_check { "check" } else { "ask" },
                if is_check { "check" } else { "ask" }
            );
        }
        if block_num.is_none() {
            outerr!(self, "{} 未指定编号，将用关键词匹配定位（可能不准）。建议用 `ask <编号> <问题>`。", "⚠️ ".yellow());
        }
        if self.session.notes.is_none() {
            bail!("还没有笔记，先 `ingest <pdf>`");
        }
        let block_id = self.resolve_block_id(question, &block_num);
        self.prepare_ask(question, block_id, is_check, None, record_concept)
    }

    /// check <编号> <想法>：与 ask 类似调 LLM 回答，但不写入笔记、不增加追问嵌套。
    /// 对话树仍记录此节点（用于上下文），但 explanation_id 为 None。
    async fn cmd_check(&mut self, args: &str) -> Result<()> {
        let args = args.trim();
        if args == "--help" || args == "-h" || args.is_empty() {
            outln!(self, "用法: check <编号> <想法>");
            outln!(self, "  <编号>    笔记中 Section 的编号（见 blocks 或导出笔记标题，如 3.2）");
            outln!(self, "  <想法>    你想核对/验证的想法或理解");
            outln!(self, "例:");
            outln!(self, "  check 3.2 我觉得BERTScore本质上就是余弦相似度，对吗");
            outln!(self, "说明: 回答只显示在终端，不写入笔记。对话树会记录此节点。");
            return Ok(());
        }
        let job = self.prepare_ask_command(args, true)?;
        self.run_ask(job).await.map(|_| ())
    }

    /// 按编号/关键词定位笔记块（ask/check 共用）。
    fn resolve_block_id(&self, question: &str, block_num: &Option<String>) -> Option<String> {
        let note = self.session.notes.as_ref()?;
        match block_num {
            Some(num) => note
                .find_section_by_number(num)
                .or_else(|| note.locate(question))
                .map(|b| b.id.clone()),
            None => note.locate(question).map(|b| b.id.clone()),
        }
    }

    /// ask/check 共用核心：预算检查 → 构建上下文 → 流式 LLM → 写解释（仅 ask）→
    /// 记录会话节点。返回 `(新节点 id, 新解释 id 或 None)`。
    /// ask/check 的 **prepare 段**（持锁）：预算检查 + 组装上下文。
    /// 返回的 `AskJob` 自带 msgs/config/client/emitter，可在**不持 App 锁**时流式执行。
    pub fn prepare_ask(
        &mut self,
        question: &str,
        block_id: Option<String>,
        is_check: bool,
        quote: Option<&str>,
        record_concept: bool,
    ) -> Result<AskJob> {
        if !self.check_budget()? {
            bail!("已达 token 预算，自动中断。用 `budget <n>` 调整。");
        }
        self.ensure_session_id();
        let session = self.session_key();
        let (msgs, block_id) = self.build_context_messages(question, block_id.as_deref(), quote);
        let approx_tokens = msgs.iter().map(|m| m.content.chars().count()).sum::<usize>() / 4;
        Ok(AskJob {
            msgs,
            cfg: self.config.llm.clone(),
            client: self.client.clone(),
            session: session.clone(),
            epoch: self.session_epoch(&session),
            restore_current: self.session.conversation.current.clone(),
            parent: self.session.conversation.current.clone(),
            question: question.to_string(),
            block_id,
            is_check,
            quote: quote.map(|s| s.to_string()),
            record_concept,
            approx_tokens,
            attach_note: true,
        })
    }

    /// LLM 段失败/被中止：恢复 prepare 时临时改动的 conversation.current。
    /// 用户已切走时，直接在发起会话的磁盘文件上恢复（若它还在）。
    pub fn abort_ask(&mut self, job: &AskJob) {
        if job.session != self.session_key() {
            let path = crate::paths::session_path(&job.session);
            if let Ok(mut sess) = Session::load(&path) {
                sess.conversation.current = job.restore_current.clone();
                let _ = sess.save(&path);
            }
            return;
        }
        self.session.conversation.current = job.restore_current.clone();
    }

    /// ask/check 的 **落地段**（持锁）：写解释/概念/对话节点。
    /// 用户已切走时把结果写回发起会话；发起会话被改动/删除则拒写（不污染任何会话）。
    pub fn commit_ask(&mut self, job: AskJob, res: crate::llm::LlmResult) -> Result<(String, Option<String>)> {
        let target = job.session.clone();
        let current = self.session_key();
        if target != current {
            if job.epoch != self.session_epoch(&target) {
                bail!("会话已变更，本次回答未写入（可重新提问）");
            }
            if !crate::paths::session_path(&target).exists() {
                bail!("发起会话已被删除，本次回答未写入");
            }
            let (name, out) = self.with_session(&target, |app| {
                let name = app.session.session_name.clone();
                let out = app.commit_ask_local(&job, res)?;
                Ok((name, out))
            })?;
            outln!(self, "{} 回答已写入会话《{}》", "✓".green().bold(), name);
            return Ok(out);
        }
        if job.epoch != self.session_epoch(&target) {
            bail!("会话已变更，本次回答未写入（可重新提问）");
        }
        let out = self.commit_ask_local(&job, res)?;
        self.update_completions();
        Ok(out)
    }

    /// ask/check 落地到「当前会话」（调用前已确保目标会话就是当前会话）。
    pub(crate) fn commit_ask_local(&mut self, job: &AskJob, res: crate::llm::LlmResult) -> Result<(String, Option<String>)> {
        self.record_usage(res.input_tokens, res.output_tokens);
        let (clean_answer, concept) = extract_concept(&res.content);
        // 节点标题：模型给出了概念就用它，否则用问题前若干字（仅作显示）
        let concept_label = concept.clone().unwrap_or_else(|| derive_concept(&job.question));
        let now = Utc::now().to_rfc3339();

        let mut explanation_id: Option<String> = None;
        let mut is_nested = false;

        if !job.is_check && job.attach_note {
            let expl_id = uuid::Uuid::new_v4().to_string();
            let parent_expl_id = job
                .parent
                .as_deref()
                .and_then(|cur| Conversation::explanation_ancestor(&self.session.conversation.nodes, cur));
            is_nested = parent_expl_id.is_some();
            if let Some(parent_eid) = parent_expl_id {
                if let Some(note) = self.session.notes.as_mut() {
                    if let Some(parent_expl) = note.find_explanation_mut(&parent_eid) {
                        parent_expl.children.push(Explanation {
                            id: expl_id.clone(),
                            question: job.question.clone(),
                            answer: clean_answer.clone(),
                            concept: concept_label.clone(),
                            created_at: now.clone(),
                            children: Vec::new(),
                            summary: None,
                            collapsed: false,
                        });
                    }
                }
            } else if let Some(bid) = &job.block_id {
                if let Some(note) = self.session.notes.as_mut() {
                    let target_id = note.find_block(bid).and_then(|b| {
                        if b.kind == notes::BlockKind::Section {
                            b.children
                                .iter()
                                .find(|c| c.kind == notes::BlockKind::Paragraph)
                                .map(|c| c.id.clone())
                        } else {
                            Some(b.id.clone())
                        }
                    });
                    if let Some(tid) = target_id {
                        if let Some(b) = note.find_block_mut(&tid) {
                            b.explanations.push(Explanation {
                                id: expl_id.clone(),
                                question: job.question.clone(),
                                answer: clean_answer.clone(),
                                concept: concept_label.clone(),
                                created_at: now.clone(),
                                children: Vec::new(),
                                summary: None,
                                collapsed: false,
                            });
                        }
                    }
                }
            }
            explanation_id = Some(expl_id);

            // 加入知识库概念：只在用户允许、且模型明确标注了知识点时记录
            if let (true, Some(concept_name)) = (job.record_concept, concept.as_ref()) {
                let (pid, ptitle) = self
                    .session
                    .current_paper_id
                    .clone()
                    .and_then(|id| self.kb.papers.iter().find(|p| p.id == id).map(|p| (id, p.title.clone())))
                    .unwrap_or_default();
                self.kb.add_concept(Concept {
                    name: concept_name.clone(),
                    definition: clean_answer.chars().take(200).collect(),
                    paper_id: pid,
                    paper_title: ptitle,
                    block_id: if is_nested { None } else { job.block_id.clone() },
                    created_at: now.clone(),
                    pinned: false,
                    graph_seen: false,
                });
                self.kb.save()?;
            }

            // 自动同步导出
            self.sync_export(job.block_id.as_deref())?;
        }

        // 记录会话节点（当前节点为父）
        let parent = job.parent.clone();
        let node_id = uuid::Uuid::new_v4().to_string();
        self.session.conversation.add_exchange(ConvNode {
            id: node_id.clone(),
            parent,
            question: job.question.clone(),
            quote: job.quote.clone(),
            answer: clean_answer.clone(),
            block_id: if job.is_check || is_nested { None } else { job.block_id.clone() },
            explanation_id: explanation_id.clone(),
            input_tokens: res.input_tokens,
            output_tokens: res.output_tokens,
            cost: res.input_tokens as f64 * self.config.pricing.input_price_per_1m / 1_000_000.0
                + res.output_tokens as f64 * self.config.pricing.output_price_per_1m / 1_000_000.0,
            created_at: now.clone(),
            label: if job.is_check { format!("[核对] {}", concept_label) } else { concept_label.clone() },
        });
        self.session.conversation.current = Some(node_id.clone());

        if res.estimated {
            outln!(self, "[注: 本次 token 数为估算]");
        }
        self.bump_epoch();
        Ok((node_id, explanation_id))
    }


    /// 执行 `AskJob`（锁外流式）并落地：失败/被打断时恢复会话指针。
    pub async fn run_ask(&mut self, job: AskJob) -> Result<(String, Option<String>)> {
        let emitter = self.emitter.clone();
        match job.run(&emitter).await {
            Ok(res) => self.commit_ask(job, res),
            Err(e) => {
                self.abort_ask(&job);
                Err(e)
            }
        }
    }

    /// 自动同步导出笔记（ask/批注后调用）；终端下必要时询问导出文件名。
    fn sync_export(&mut self, block_id_for_hint: Option<&str>) -> Result<()> {
        // 仅阅读会话不生成笔记，也就不自动导出
        if self.session.read_only {
            return Ok(());
        }
        if self.export_path.is_none() {
            let title = self.session.notes.as_ref().map(|n| n.title.clone()).unwrap_or_default();
            let default_name = format!("笔记_{}.md", title.chars().take(20).collect::<String>());
            if self.emitter.is_terminal() {
                print!("请输入笔记导出文件名（回车默认 {}，输 skip 跳过）: ", default_name);
                io::stdout().flush()?;
                let mut name = String::new();
                io::stdin().lock().read_line(&mut name)?;
                let name = name.trim();
                if name == "skip" || name == "s" {
                    outerr!(self, "{} 已跳过导出，之后可用 `export md <file>` 手动导出。", "".dimmed());
                    return Ok(());
                }
                let path = if name.is_empty() { default_name } else { name.to_string() };
                self.export_path = Some(path);
            } else {
                self.export_path = Some(default_name);
            }
        }
        if let Some(p) = &self.export_path {
            if let Some(note) = &self.session.notes {
                if std::fs::write(p, export::render_for(p, note, &self.session.conversation, &self.session.annotations)).is_ok() {
                    let location = block_id_for_hint.and_then(|bid| {
                        note.find_block(bid).map(|b| {
                            let num = if b.number.is_empty() { String::new() } else { format!("{} ", b.number) };
                            format!("{num}{}", b.text.chars().take(30).collect::<String>())
                        })
                    });
                    match location {
                        Some(loc) => outln!(self, "{} 笔记已同步更新到 {}（更新位置：{}）", "✓".green().bold(), p, loc),
                        None => outln!(self, "{} 笔记已同步更新到 {}", "✓".green().bold(), p),
                    }
                }
            }
        }
        Ok(())
    }

    // ===== 笔记编辑（Web）：改文字 / 整节重写 / 插入 / 删除 =====

    /// 修改块的文字（`notes::TITLE_ID` 表示标题）。
    pub fn edit_block(&mut self, block_id: &str, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            bail!("内容不能为空");
        }
        {
            let note = self.session.notes.as_ref().ok_or_else(|| anyhow!("还没有笔记"))?;
            if block_id != notes::TITLE_ID && note.find_block(block_id).is_none() {
                bail!("找不到要编辑的块（笔记可能已变化）");
            }
        }
        self.push_undo();
        {
            let note = self.session.notes.as_mut().unwrap();
            note.set_text(block_id, text);
        }
        self.after_note_change();
        Ok(())
    }

    /// 整节重写：段落 → 直接替换文本；章节 → 用 Markdown 解析出的新块替换其 children。
    /// 若内容以小节标题开头（编辑弹窗会带上原标题），则同时更新该节标题。
    pub fn rewrite_block(&mut self, block_id: &str, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            bail!("内容不能为空");
        }
        let is_section = {
            let note = self.session.notes.as_ref().ok_or_else(|| anyhow!("还没有笔记"))?;
            let Some(b) = note.find_block(block_id) else {
                bail!("找不到要重写的块（笔记可能已变化）");
            };
            b.kind == notes::BlockKind::Section
        };
        let parsed = if is_section {
            let blocks = notes::parse_markdown_blocks(text);
            if blocks.is_empty() {
                bail!("内容解析不出任何块");
            }
            Some(blocks)
        } else {
            None
        };
        self.push_undo();
        {
            let note = self.session.notes.as_mut().unwrap();
            if let Some(mut blocks) = parsed {
                // 首块是 Section（用户重写了标题行）→ 用它更新本节标题，正文取其子块
                let (new_title, children) = if blocks
                    .first()
                    .map(|b| b.kind == notes::BlockKind::Section)
                    .unwrap_or(false)
                {
                    let first = blocks.remove(0);
                    (Some(first.text), first.children)
                } else {
                    (None, blocks)
                };
                if let Some(t) = new_title {
                    if !t.trim().is_empty() {
                        if let Some(sec) = note.find_block_mut(block_id) {
                            sec.text = t;
                        }
                    }
                }
                note.replace_children(block_id, children);
                note.renumber();
            } else {
                note.set_text(block_id, text);
            }
        }
        self.after_note_change();
        Ok(())
    }

    /// 在目标块之后插入 Markdown 解析出的块（可一次插入多个）。
    pub fn insert_after(&mut self, after_block_id: &str, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            bail!("内容不能为空");
        }
        let blocks = notes::parse_markdown_blocks(text);
        if blocks.is_empty() {
            bail!("内容解析不出任何块");
        }
        self.push_undo();
        {
            let note = self.session.notes.as_mut().ok_or_else(|| anyhow!("还没有笔记"))?;
            if !note.insert_blocks_after(after_block_id, blocks) {
                bail!("找不到插入位置（笔记可能已变化）");
            }
            note.renumber();
        }
        self.after_note_change();
        Ok(())
    }

    /// 删除块及其子树（含其上的追问）；同时移除指向这些块的批注。返回删除的块数。
    pub fn remove_note_block(&mut self, block_id: &str) -> Result<usize> {
        if block_id == notes::TITLE_ID {
            bail!("标题不能删除");
        }
        let ids = {
            let note = self.session.notes.as_ref().ok_or_else(|| anyhow!("还没有笔记"))?;
            let Some(b) = note.find_block(block_id) else {
                bail!("找不到要删除的块");
            };
            let mut ids = Vec::new();
            collect_block_ids(b, &mut ids);
            ids
        };
        self.push_undo();
        {
            let note = self.session.notes.as_mut().unwrap();
            if !note.remove_block(block_id) {
                bail!("删除失败（笔记可能已变化）");
            }
            note.renumber();
        }
        let removed_ann = self
            .session
            .annotations
            .iter()
            .filter(|a| ids.contains(&a.block_id))
            .count();
        if removed_ann > 0 {
            self.session.annotations.retain(|a| !ids.contains(&a.block_id));
        }
        self.after_note_change();
        Ok(ids.len())
    }

    /// 变更笔记后的统一收尾：同步导出到 export_path + 保存会话（失败只记日志，不影响主操作）。
    fn after_note_change(&mut self) {
        self.bump_epoch();
        if let Some(p) = self.export_path.clone() {
            if !p.is_empty() {
                if let Some(note) = &self.session.notes {
                    let content =
                        export::render_for(&p, note, &self.session.conversation, &self.session.annotations);
                    if let Err(e) = std::fs::write(&p, content) {
                        crate::logging::error(format!("同步导出失败 {p}: {e:#}"));
                    }
                }
            }
        }
        if let Err(e) = self.auto_persist() {
            crate::logging::error(format!("保存会话失败: {e:#}"));
        }
    }

    /// 构造「AI 改写 / 补充」的消息（只生成、不改笔记；由 Web 端流式返回给用户确认）。
    /// `mode`：`"rewrite"`（重写该片段）或 `"append"`（补充到该片段之后）。
    pub fn note_ai_messages(&self, block_id: &str, instruction: &str, mode: &str) -> Result<Vec<Message>> {
        if !matches!(mode, "rewrite" | "append") {
            bail!("mode 只能是 rewrite 或 append");
        }
        let note = self.session.notes.as_ref().ok_or_else(|| anyhow!("还没有笔记"))?;
        let is_section = block_id != notes::TITLE_ID
            && note
                .find_block(block_id)
                .map(|b| b.kind == notes::BlockKind::Section)
                .unwrap_or(false);
        let target = if block_id == notes::TITLE_ID {
            format!("（整篇笔记标题）{}", note.title)
        } else {
            let kind = if is_section { "章节" } else { "段落" };
            let content = note
                .block_markdown(block_id)
                .ok_or_else(|| anyhow!("找不到要处理的块"))?;
            format!("（{kind}）\n{content}")
        };
        let task = if mode == "append" {
            "请生成要**补充**到该片段之后的新内容；不要重复已有内容，可直接给出新的段落或 `##`/`###` 小节。"
        } else if is_section {
            "请**重写**该章节：可重写标题（用 `##` 开头，`###` 表示其下小节）与全部正文；保持结构清晰、内容更完整。"
        } else {
            "请**重写**该片段（整段替换）；保留原意，使表述更清晰、更完整，必要时可拆成多段。"
        };
        let template = crate::prompts::load_prompt(
            &crate::prompts::prompts_dir(),
            "rewrite.txt",
            crate::prompts::DEFAULT_REWRITE_PROMPT,
        );
        let prompt = template
            .replace("{paper}", &note.raw_text)
            .replace("{note}", &note.to_markdown())
            .replace("{target}", &target)
            .replace("{instruction}", instruction)
            .replace("{task}", task);
        Ok(vec![Message::text("user", prompt)])
    }

    /// 「按风格重写全文」的提示词：以论文原文（若有）或当前笔记为素材，
    /// 套用所选笔记风格（程序会自动前置固定输出契约）。
    pub fn restyle_messages(&self, style: &str, extra: &str) -> Result<Vec<Message>> {
        let note = self.session.notes.as_ref().ok_or_else(|| anyhow!("还没有笔记"))?;
        let material = if note.raw_text.trim().is_empty() {
            note.to_markdown()
        } else {
            note.raw_text.clone()
        };
        let prompt = crate::prompts::compose_style_prompt(style, &material, extra)?;
        Ok(vec![
            Message::text("system", "你是笔记生成助手，只输出 Markdown。"),
            Message::text("user", prompt),
        ])
    }

    /// 组装消息后的通用提示：估算上下文规模；超限时告警（经 emitter 输出）。
    /// 返回带上下文规模的进度文案，供锁外任务使用。
    fn announce_context(&self, msgs: &[Message], label: &str) -> String {
        let approx = msgs.iter().map(|m| m.content.chars().count()).sum::<usize>() / 4;
        let ctx = self.config.llm.context_length;
        if ctx > 0 && approx > ctx {
            self.emitter.stderr(format!(
                "⚠️ 提示上下文约 {approx} token，超过配置的 {ctx}，可能报错；可精简论文或调大 llm.context_length"
            ));
        }
        format!("{label}（上下文约 {approx} token）")
    }

    /// AI 重写/补充的 **prepare 段**（持锁）：只读会话，组装消息。
    pub fn prepare_note_ai(&mut self, block_id: &str, instruction: &str, mode: &str) -> Result<SimpleJob> {
        let msgs = self.note_ai_messages(block_id, instruction, mode)?;
        let progress = self.announce_context(&msgs, "AI 生成中…");
        self.ensure_session_id();
        Ok(SimpleJob {
            msgs,
            cfg: self.config.llm.clone(),
            client: self.client.clone(),
            progress,
            session: self.session_key(),
        })
    }

    /// 按风格重写全文的 **prepare 段**（持锁）。
    pub fn prepare_restyle(&mut self, style: &str, extra: &str) -> Result<SimpleJob> {
        let msgs = self.restyle_messages(style, extra)?;
        let progress = self.announce_context(&msgs, "按风格重写中…");
        self.ensure_session_id();
        Ok(SimpleJob {
            msgs,
            cfg: self.config.llm.clone(),
            client: self.client.clone(),
            progress,
            session: self.session_key(),
        })
    }

    /// 只生成、不改会话的任务落地（持锁）：记用量（写回发起会话）+ 截断提示。
    pub fn commit_usage(&mut self, job: &SimpleJob, res: &crate::llm::LlmResult) {
        self.record_usage_for(&job.session, res.input_tokens, res.output_tokens);
        if res.truncated() {
            outerr!(self, "{} 输出达到上限（finish_reason=length），内容可能被截断", "⚠️ ".yellow());
        }
    }

    /// 用「按风格重写」的结果替换整篇笔记（重建全部块 id）。
    /// 会清空现有批注（块锚点已失效），并入撤销栈供「撤销」恢复。
    pub fn apply_restyle(&mut self, markdown: &str) -> Result<()> {
        if markdown.trim().is_empty() {
            bail!("内容为空");
        }
        let Some(old) = self.session.notes.as_ref() else {
            bail!("还没有笔记");
        };
        let raw_text = old.raw_text.clone();
        let paper_id = old.paper_id.clone();
        let material_kind = old.material_kind.clone();
        let math_macros = old.math_macros.clone();
        self.push_undo();
        let mut note = notes::parse_markdown_note(markdown, &raw_text);
        note.paper_id = paper_id;
        note.material_kind = material_kind;
        if note.math_macros.is_none() && math_macros.is_some() {
            note.math_macros = math_macros;
        }
        self.session.annotations.clear(); // 旧批注锚定在旧块上，整篇重建后失效
        self.session.notes = Some(note);
        self.update_completions();
        self.after_note_change();
        Ok(())
    }

    /// 新建批注（Web）：在 block_id 处针对选中文字提问，作为独立线程的根节点。
    /// 返回 `(批注 id, 根节点 id, 解释 id 或 None)`。
    /// 笔记批注的 **prepare 段**（持锁）：校验块、切到独立线程、组装上下文。
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_annotate(
        &mut self,
        block_id: &str,
        quote: &str,
        quote_tex: Option<&str>,
        question: &str,
        is_check: bool,
        record_concept: bool,
        extra: &ExtraInput,
    ) -> Result<(AskJob, AnnAnchor)> {
        // 全文提问：block_id 用哨兵 __title__，解释挂到首个块（保证追问嵌套），
        // 但批注仍记录 __title__ 供前端高亮标题。
        let is_title = block_id == "__title__";
        if !is_title
            && self
                .session
                .notes
                .as_ref()
                .and_then(|n| n.find_block(block_id))
                .is_none()
        {
            bail!("找不到引用的笔记块（笔记可能已变化）");
        }
        let insert_block = if is_title {
            self.session
                .notes
                .as_ref()
                .and_then(|n| n.blocks.first())
                .map(|b| b.id.clone())
        } else {
            Some(block_id.to_string())
        };
        let saved = self.session.conversation.current.clone();
        self.session.conversation.current = None; // 独立线程：新根
        // 给 LLM 的上下文优先用 quote_tex（公式还原成 TeX）
        let ctx = quote_tex.filter(|s| !s.trim().is_empty()).unwrap_or(quote);
        let mut job = match self.prepare_ask(question, insert_block, is_check, Some(ctx), record_concept) {
            Ok(j) => j,
            Err(e) => {
                self.session.conversation.current = saved;
                return Err(e);
            }
        };
        extra.apply_to(&mut job.msgs);
        job.restore_current = saved;
        let anchor = AnnAnchor::Note {
            block_id: block_id.to_string(),
            quote: quote.to_string(),
            quote_tex: quote_tex.filter(|s| !s.trim().is_empty()).map(|s| s.to_string()),
        };
        Ok((job, anchor))
    }

    /// PDF 页面批注的 **prepare 段**（持锁）：针对某页的选区文本/图片/整页提问，
    /// 作为独立线程的根节点；把该页渲染图（data URL）附在本次请求中（不进历史）。
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_annotate_pdf(
        &mut self,
        page: u32,
        rects: Vec<[f32; 4]>,
        kind: &str,
        quote: &str,
        question: &str,
        is_check: bool,
        record_concept: bool,
        extra: &ExtraInput,
    ) -> Result<(AskJob, AnnAnchor)> {
        let saved = self.session.conversation.current.clone();
        self.session.conversation.current = None; // 独立线程：新根
        let ctx = if quote.trim().is_empty() { None } else { Some(quote) };
        let mut job = match self.prepare_ask(question, None, is_check, ctx, record_concept) {
            Ok(j) => j,
            Err(e) => {
                self.session.conversation.current = saved;
                return Err(e);
            }
        };
        // PDF 提问不写笔记树：只建对话节点 + 批注
        job.attach_note = false;
        extra.apply_to(&mut job.msgs);
        job.restore_current = saved.clone();
        let anchor = AnnAnchor::Pdf {
            page,
            rects,
            kind: kind.to_string(),
            quote: quote.to_string(),
        };
        Ok((job, anchor))
    }

    /// 批注的 **落地段**（持锁）：写问答节点 + 批注记录。
    /// 用户已切走时把批注写回发起会话；发起会话被改动/删除则拒写。
    pub fn commit_annotate(
        &mut self,
        job: AskJob,
        anchor: AnnAnchor,
        res: crate::llm::LlmResult,
    ) -> Result<(String, String, Option<String>)> {
        let target = job.session.clone();
        let current = self.session_key();
        if target != current {
            if job.epoch != self.session_epoch(&target) {
                bail!("会话已变更，本次批注未写入（可重新提问）");
            }
            if !crate::paths::session_path(&target).exists() {
                bail!("发起会话已被删除，本次批注未写入");
            }
            let (name, out) = self.with_session(&target, |app| {
                let name = app.session.session_name.clone();
                let out = app.commit_annotate_local(&job, anchor, res)?;
                Ok((name, out))
            })?;
            outln!(self, "{} 批注已写入会话《{}》", "✓".green().bold(), name);
            return Ok(out);
        }
        if job.epoch != self.session_epoch(&target) {
            bail!("会话已变更，本次批注未写入（可重新提问）");
        }
        let out = self.commit_annotate_local(&job, anchor, res)?;
        self.update_completions();
        Ok(out)
    }

    /// 批注落地到「当前会话」（调用前已确保目标会话就是当前会话）。
    fn commit_annotate_local(
        &mut self,
        job: &AskJob,
        anchor: AnnAnchor,
        res: crate::llm::LlmResult,
    ) -> Result<(String, String, Option<String>)> {
        let (node_id, expl_id) = self.commit_ask_local(job, res)?;
        let ann_id = uuid::Uuid::new_v4().to_string();
        let annotation = match anchor {
            AnnAnchor::Note { block_id, quote, quote_tex } => Annotation {
                id: ann_id.clone(),
                block_id,
                quote,
                quote_tex,
                node_id: None,
                root_node_id: node_id.clone(),
                created_at: Utc::now().to_rfc3339(),
                ..Default::default()
            },
            AnnAnchor::Answer { node_id: anchor_node, quote, quote_tex } => Annotation {
                id: ann_id.clone(),
                block_id: String::new(),
                quote,
                quote_tex,
                node_id: Some(anchor_node),
                root_node_id: node_id.clone(),
                created_at: Utc::now().to_rfc3339(),
                ..Default::default()
            },
            AnnAnchor::Pdf { page, rects, kind, quote } => Annotation {
                id: ann_id.clone(),
                block_id: String::new(),
                quote,
                quote_tex: None,
                node_id: None,
                root_node_id: node_id.clone(),
                created_at: Utc::now().to_rfc3339(),
                page: Some(page),
                rects,
                kind: Some(kind),
            },
        };
        self.session.annotations.push(annotation);
        self.bump_epoch();
        Ok((ann_id, node_id, expl_id))
    }

    /// 批注内追问的 **prepare 段**（持锁）。
    pub fn prepare_annotate_reply(
        &mut self,
        node_id: &str,
        question: &str,
        is_check: bool,
        record_concept: bool,
        extra: &ExtraInput,
    ) -> Result<AskJob> {
        if !self.session.conversation.nodes.iter().any(|n| n.id == node_id) {
            bail!("找不到对话节点");
        }
        let fallback_block = self.annotation_block_for_node(node_id);
        let saved = self.session.conversation.current.clone();
        self.session.conversation.current = Some(node_id.to_string());
        let mut job = match self.prepare_ask(question, fallback_block, is_check, None, record_concept) {
            Ok(j) => j,
            Err(e) => {
                self.session.conversation.current = saved;
                return Err(e);
            }
        };
        extra.apply_to(&mut job.msgs);
        job.restore_current = saved;
        Ok(job)
    }

    /// 回答批注的 **prepare 段**（持锁）：锚点是对话节点（`node_id`）。
    pub fn prepare_annotate_answer(
        &mut self,
        node_id: &str,
        quote: &str,
        quote_tex: Option<&str>,
        question: &str,
        is_check: bool,
        record_concept: bool,
        extra: &ExtraInput,
    ) -> Result<(AskJob, AnnAnchor)> {
        if !self.session.conversation.nodes.iter().any(|n| n.id == node_id) {
            bail!("找不到对话节点");
        }
        // 给 LLM 的上下文优先用 quote_tex（公式还原成 TeX）
        let ctx = quote_tex.filter(|s| !s.trim().is_empty()).unwrap_or(quote);
        let fallback_block = self.annotation_block_for_node(node_id);
        let saved = self.session.conversation.current.clone();
        self.session.conversation.current = Some(node_id.to_string());
        let mut job = match self.prepare_ask(question, fallback_block, is_check, Some(ctx), record_concept) {
            Ok(j) => j,
            Err(e) => {
                self.session.conversation.current = saved;
                return Err(e);
            }
        };
        extra.apply_to(&mut job.msgs);
        job.restore_current = saved;
        let anchor = AnnAnchor::Answer {
            node_id: node_id.to_string(),
            quote: quote.to_string(),
            quote_tex: quote_tex.filter(|s| !s.trim().is_empty()).map(|s| s.to_string()),
        };
        Ok((job, anchor))
    }

    /// 找到包含指定节点的批注，返回其 block_id（批注内追问的兜底定位）。
    fn annotation_block_for_node(&self, node_id: &str) -> Option<String> {
        for ann in &self.session.annotations {
            let mut cur = Some(node_id.to_string());
            while let Some(id) = cur {
                if id == ann.root_node_id {
                    return Some(ann.block_id.clone());
                }
                cur = self
                    .session
                    .conversation
                    .nodes
                    .iter()
                    .find(|n| n.id == id)
                    .and_then(|n| n.parent.clone());
            }
        }
        None
    }

    /// 删除整条批注（Web 右键高亮文字）：移除批注条目、其会话子树、笔记中对应解释。
    /// 入撤销栈，可用 `undo` 恢复。
    pub fn delete_annotation(&mut self, annotation_id: &str) -> Result<()> {
        let Some(idx) = self.session.annotations.iter().position(|a| a.id == annotation_id) else {
            bail!("找不到该批注");
        };
        let root = self.session.annotations[idx].root_node_id.clone();
        self.push_undo();
        let subtree = self.collect_subtree(&root);
        let expl_ids: Vec<String> = subtree
            .iter()
            .filter_map(|(_, n)| n.explanation_id.clone())
            .collect();
        if let Some(note) = self.session.notes.as_mut() {
            for eid in &expl_ids {
                let _ = note.remove_explanation(eid);
            }
        }
        self.session.conversation.remove_subtree(&root);
        self.session.annotations.remove(idx);
        self.bump_epoch();
        self.update_completions();
        Ok(())
    }

    /// sum：把当前对话节点子树（含自己）的全部问答概括成"总结"，写入笔记。
    /// 流程：DFS 收集子树 → 预算检查 → LLM 概括（进度条）→ 找到当前节点向上最近
    /// 带解释的祖先（跳过 check）→ 给该 Explanation 写 summary 并折叠（collapsed）
    /// → 同步导出。终端先打印总结正文，笔记中体现为 <details> 折叠+总结。
    async fn cmd_sum(&mut self, rest: &str) -> Result<()> {
        let job = self.prepare_sum(rest)?;
        let emitter = self.emitter.clone();
        match job.run(&emitter).await {
            Ok(res) => self.commit_sum(job, res),
            Err(e) => Err(e),
        }
    }

    /// sum 的 **prepare 段**（持锁）：定位节点、收集子树、组 prompt。
    /// 返回的 `SumJob` 可在不持 App 锁时流式执行（Web 三段式复用）。
    pub fn prepare_sum(&mut self, rest: &str) -> Result<SumJob> {
        if self.session.notes.is_none() {
            bail!("还没有笔记，先 `ingest <pdf>`");
        }
        // sum [n]：n 为对话树 DFS 编号；不填则用当前节点
        let cur = match rest.trim().parse::<usize>() {
            Ok(n) => self
                .session
                .conversation
                .dfs_order()
                .get(n.wrapping_sub(1))
                .map(|x| x.id.clone())
                .ok_or_else(|| anyhow!("找不到节点 {n}（见 tree）"))?,
            Err(_) => self
                .session
                .conversation
                .current
                .clone()
                .ok_or_else(|| anyhow!("当前不在任何对话节点上，先 ask 提问"))?,
        };
        // 收集子树（含自己），DFS 顺序
        let subtree = self.collect_subtree(&cur);
        if subtree.is_empty() {
            bail!("当前节点无问答内容");
        }
        if !self.check_budget()? {
            bail!("已达 token 预算，自动中断。用 `budget <n>` 调整。");
        }

        // 组 prompt：按层级缩进列出 Q&A
        let mut qa = String::new();
        for (depth, n) in &subtree {
            let indent = "  ".repeat(*depth);
            qa.push_str(&format!("{indent}问：{}\n{indent}答：{}\n\n", n.question, n.answer));
        }
        let title = self.session.notes.as_ref().map(|n| n.title.clone()).unwrap_or_default();
        let prompt = format!(
            "以下是一段关于论文《{title}》的递归追问记录（缩进表示追问层级）。\n\
             请把这段追问弄明白的内容概括成一段总结，直接输出总结正文：\n\
             - 不要标题头（不要 # 开头），3-6 句或用 `- ` 要点列表\n\
             - 保留关键公式（用 $...$ / $$...$$）与结论\n\
             只输出总结本身。\n\n{qa}"
        );
        let msgs = vec![
            Message::text("system", "你是学习总结助手，只输出总结正文。"),
            Message::text("user", prompt),
        ];
        self.ensure_session_id();
        let session = self.session_key();
        Ok(SumJob {
            msgs,
            cfg: self.config.llm.clone(),
            client: self.client.clone(),
            session: session.clone(),
            epoch: self.session_epoch(&session),
            cur,
        })
    }

    /// sum 的 **落地段**（持锁）：把总结挂到 Explanation（折叠子树）并同步导出。
    /// 用户已切走时写回发起会话；发起会话被改动/删除则拒写。
    pub fn commit_sum(&mut self, job: SumJob, res: crate::llm::LlmResult) -> Result<()> {
        let target = job.session.clone();
        let current = self.session_key();
        if target != current {
            if job.epoch != self.session_epoch(&target) {
                bail!("会话已变更，本次总结未写入（可重试）");
            }
            if !crate::paths::session_path(&target).exists() {
                bail!("发起会话已被删除，本次总结未写入");
            }
            let name = self.with_session(&target, |app| {
                app.commit_sum_local(&job, res)?;
                Ok(app.session.session_name.clone())
            })?;
            outln!(self, "{} 总结已写入会话《{}》", "✓".green().bold(), name);
            self.update_completions();
            return Ok(());
        }
        if job.epoch != self.session_epoch(&target) {
            bail!("会话已变更，本次总结未写入（可重试）");
        }
        self.commit_sum_local(&job, res)?;
        self.update_completions();
        Ok(())
    }

    /// sum 落地到「当前会话」（调用前已确保目标会话就是当前会话）。
    fn commit_sum_local(&mut self, job: &SumJob, res: crate::llm::LlmResult) -> Result<()> {
        self.record_usage(res.input_tokens, res.output_tokens);
        if res.estimated {
            outln!(self, "[注: 本次 token 数为估算]");
        }
        let summary = res.content.trim().to_string();
        outln!(self, "\n**总结**：{summary}\n");

        // 写入笔记：插入当前节点（或其最近有解释的祖先，跳过 check）对应的
        // Explanation：折叠其子树 + 挂总结
        let target_expl_id = Conversation::explanation_ancestor(&self.session.conversation.nodes, &job.cur)
            .ok_or_else(|| anyhow!("当前对话链上没有可插入总结的追问（先 `ask` 产生追问后再 `sum`）"))?;
        if let Some(note) = self.session.notes.as_mut() {
            if let Some(expl) = note.find_explanation_mut(&target_expl_id) {
                expl.summary = Some(summary);
                expl.collapsed = true;
            }
        }
        self.bump_epoch();
        // 自动同步导出（与 ask 相同逻辑）
        if let Some(p) = &self.export_path {
            if let Some(note) = &self.session.notes {
                if std::fs::write(p, export::render_for(p, note, &self.session.conversation, &self.session.annotations)).is_ok() {
                    outln!(self, "{} 笔记已同步更新到 {}（已插入总结）", "✓".green().bold(), p);
                }
            }
        }
        Ok(())
    }

    /// graph 的 prepare 段：确定要整理的新概念并构造 LLM 消息。
    /// `rebuild` 为真时整理全部概念（配合 `graph rebuild`，commit 时先清空旧关系）。
    pub fn prepare_graph(&mut self, rebuild: bool) -> Result<GraphJob> {
        let all = self.kb.unique_concept_names();
        if all.is_empty() {
            bail!("知识库还没有概念：先在回答里「记概念」（或提问时保持默认），再来整理关系。");
        }
        let new_names = if rebuild {
            all
        } else {
            self.kb.pending_graph_names()
        };
        if new_names.is_empty() {
            bail!("没有新概念需要整理（用 `graph rebuild` 重建全部关系）。");
        }
        let msgs = build_graph_messages(&self.kb, &new_names);
        Ok(GraphJob {
            msgs,
            cfg: self.config.llm.clone(),
            client: self.client.clone(),
            new_names,
            rebuild,
        })
    }

    /// CLI 用：锁内 prepare → run → commit（Web 端走 api_run 的三段式，锁外执行 LLM）。
    pub async fn run_graph(&mut self, job: GraphJob) -> Result<()> {
        let emitter = self.emitter.clone();
        let res = job.run(&emitter).await?;
        self.commit_graph(job, res)
    }

    /// graph 的 commit 段：解析 LLM 输出的关系、合并入库、标记已整理并落盘。
    pub fn commit_graph(&mut self, job: GraphJob, res: crate::llm::LlmResult) -> Result<()> {
        self.record_usage(res.input_tokens, res.output_tokens);
        if res.estimated {
            outln!(self, "[注: 本次 token 数为估算]");
        }
        let rels = parse_graph_relations(&res.content);
        if job.rebuild {
            self.kb.reset_graph();
        }
        let added = self.kb.merge_relations(rels);
        self.kb.mark_graph_seen(&job.new_names);
        self.kb.save()?;
        outln!(
            self,
            "{} 知识图谱已更新：新增 {} 条关系，当前共 {} 个概念、{} 条关系。",
            "✓".green().bold(),
            added,
            self.kb.unique_concept_names().len(),
            self.kb.relations.len()
        );
        Ok(())
    }

    /// graph [build|rebuild|clear|show]：查看 / 懒惰更新 / 重建 / 清空概念关系。
    async fn cmd_graph(&mut self, rest: &str) -> Result<()> {
        match rest.trim() {
            "" | "show" | "status" => {
                self.print_graph_status();
                Ok(())
            }
            "clear" => {
                let n = self.kb.relations.len();
                self.kb.relations.clear();
                self.kb.save()?;
                outln!(self, "已清空 {n} 条概念关系（概念本身保留）。");
                Ok(())
            }
            "build" | "update" => {
                let job = self.prepare_graph(false)?;
                self.run_graph(job).await
            }
            "--all" | "rebuild" => {
                let job = self.prepare_graph(true)?;
                self.run_graph(job).await
            }
            other => {
                outln!(self, "用法: graph [build|rebuild|clear|show]（未知参数: {other}）");
                Ok(())
            }
        }
    }

    fn print_graph_status(&self) {
        outln!(
            self,
            "知识图谱: {} 个概念，{} 条关系，{} 个概念待整理。",
            self.kb.unique_concept_names().len(),
            self.kb.relations.len(),
            self.kb.pending_graph_names().len()
        );
    }

    /// del [n] [--yes]：删除指定（默认当前）对话节点及其子树（根节点不可删）。
    /// 不带 `--yes` 只打印警告与将删除的内容；执行前把笔记+对话树入撤销栈。
    async fn cmd_del(&mut self, rest: &str) -> Result<()> {
        let mut target: Option<String> = None;
        let mut confirm = false;
        for tok in rest.split_whitespace() {
            if tok == "--yes" || tok == "-y" {
                confirm = true;
            } else if let Ok(n) = tok.parse::<usize>() {
                target = Some(
                    self.session
                        .conversation
                        .dfs_order()
                        .get(n.wrapping_sub(1))
                        .map(|x| x.id.clone())
                        .ok_or_else(|| anyhow!("找不到节点 {n}（见 tree）"))?,
                );
            }
        }
        let cur = match target {
            Some(id) => id,
            None => self
                .session
                .conversation
                .current
                .clone()
                .ok_or_else(|| anyhow!("当前不在任何对话节点上，先 ask 提问"))?,
        };
        let node = self
            .session
            .conversation
            .nodes
            .iter()
            .find(|n| n.id == cur)
            .ok_or_else(|| anyhow!("找不到当前节点"))?
            .clone();
        if node.parent.is_none() {
            bail!("根节点不可删除（如需清空请用 `new` 新建会话）");
        }
        let subtree = self.collect_subtree(&cur);
        if !confirm {
            outln!(self, "⚠️  将删除节点「{}」及其 {} 个子节点：", node.label, subtree.len().saturating_sub(1));
            for (depth, n) in &subtree {
                outln!(self, "  {}- {}", "  ".repeat(*depth), n.label);
            }
            outln!(self, "确认请执行：del --yes（可用 undo 撤销）");
            return Ok(());
        }
        // 入撤销栈（限 20 条）
        self.push_undo();
        // 同步移除笔记中对应的解释（含嵌套子树）
        let expl_ids: Vec<String> = subtree
            .iter()
            .filter_map(|(_, n)| n.explanation_id.clone())
            .collect();
        if let Some(note) = self.session.notes.as_mut() {
            for eid in &expl_ids {
                let _ = note.remove_explanation(eid);
            }
        }
        let removed = self.session.conversation.remove_subtree(&cur);
        self.bump_epoch();
        // 批注线程的根若在被删子树里，批注记录一并清理（否则会悬空）
        let removed_set: std::collections::HashSet<&str> = removed.iter().map(|s| s.as_str()).collect();
        let before = self.session.annotations.len();
        self.session.annotations.retain(|a| !removed_set.contains(a.root_node_id.as_str()));
        let ann_removed = before - self.session.annotations.len();
        self.update_completions();
        outln!(
            self,
            "✓ 已删除 {} 个对话节点{}（可用 `undo` 撤销）",
            removed.len(),
            if ann_removed > 0 { format!("、{ann_removed} 条批注") } else { String::new() }
        );
        Ok(())
    }

    /// undo：撤销上一次 `del` 或批注删除（恢复笔记、对话树与批注，不影响用量统计）。
    async fn cmd_undo(&mut self) -> Result<()> {
        let snap = self
            .undo_stack
            .pop()
            .ok_or_else(|| anyhow!("没有可撤销的操作"))?;
        self.session.notes = snap.notes;
        self.session.conversation = snap.conversation;
        self.session.annotations = snap.annotations;
        self.update_completions();
        self.after_note_change(); // 同步导出 + 保存会话
        outln!(self, "✓ 已撤销上一次操作（笔记/对话已恢复）");
        Ok(())
    }

    /// 收集以 id 为根的子树（含自己），返回 (相对深度, 节点) 的 DFS 序。
    fn collect_subtree(&self, root: &str) -> Vec<(usize, crate::conversation::ConvNode)> {
        use std::collections::HashMap;
        let mut kids: HashMap<&str, Vec<&crate::conversation::ConvNode>> = HashMap::new();
        let mut by_id: HashMap<&str, &crate::conversation::ConvNode> = HashMap::new();
        for n in &self.session.conversation.nodes {
            by_id.insert(n.id.as_str(), n);
            if let Some(p) = n.parent.as_deref() {
                kids.entry(p).or_default().push(n);
            }
        }
        let mut out = Vec::new();
        let Some(root_node) = by_id.get(root) else { return out };
        // DFS 栈：(id, depth)
        let mut stack = vec![(root_node.id.as_str(), 0usize)];
        while let Some((id, depth)) = stack.pop() {
            if let Some(n) = by_id.get(id) {
                out.push((depth, (*n).clone()));
                // 逆序压栈保持 DFS 原顺序
                if let Some(ks) = kids.get(id) {
                    for k in ks.iter().rev() {
                        stack.push((k.id.as_str(), depth + 1));
                    }
                }
            }
        }
        out
    }

    // ===== 内部辅助 =====

    /// 预算检查：返回 false 表示已达上限应中断。
    fn check_budget(&self) -> Result<bool> {
        let budget = self.config.budget.token_budget;
        if budget == 0 {
            return Ok(true);
        }
        let used = self.kb.stats.total_tokens() + self.session.stats.total_tokens();
        Ok(used < budget)
    }

    /// 按当前价格配置换算一次调用的成本（不做任何记录）。
    pub(crate) fn price_cost(&self, input: u64, output: u64) -> f64 {
        let in_price = self.config.pricing.input_price_per_1m;
        let out_price = self.config.pricing.output_price_per_1m;
        input as f64 * in_price / 1_000_000.0 + output as f64 * out_price / 1_000_000.0
    }

    /// 记录一次 LLM 调用的 token 与成本（会话 + 全局）。
    pub(crate) fn record_usage(&mut self, input: u64, output: u64) {
        let cost = self.price_cost(input, output);
        self.session.stats.add(input, output, cost);
        self.kb.stats.add(input, output, cost);
    }

    /// 记录一次 LLM 调用的用量，并写回到发起会话（用户可能已切走）。
    /// 目标会话被删除时只记全局，避免凭空造会话。
    pub(crate) fn record_usage_for(&mut self, session_key: &str, input: u64, output: u64) {
        let cost = self.price_cost(input, output);
        self.kb.stats.add(input, output, cost);
        if session_key == self.session_key() {
            self.session.stats.add(input, output, cost);
            return;
        }
        if crate::paths::session_path(session_key).exists() {
            if let Err(e) = self.with_session(session_key, |app| {
                app.session.stats.add(input, output, cost);
                Ok(())
            }) {
                crate::logging::warn(format!("用量写回会话失败（{session_key}）: {e}"));
            }
        }
    }

    /// 更新补全用的编号列表（section 编号 + 对话树节点编号）。
    pub fn update_completions(&self) {
        let sections: Vec<String> = if let Some(note) = &self.session.notes {
            note.flatten()
                .iter()
                .filter(|(b, _)| b.kind == notes::BlockKind::Section && !b.number.is_empty())
                .map(|(b, _)| b.number.clone())
                .collect()
        } else {
            Vec::new()
        };
        *self.section_numbers.lock().unwrap() = sections;

        let order = self.session.conversation.dfs_order();
        let nodes: Vec<String> = (1..=order.len()).map(|i| i.to_string()).collect();
        *self.node_numbers.lock().unwrap() = nodes;
    }

    /// 构建 ask/check 共用的上下文消息序列（论文全文+笔记+对话路径+概念注入）。
    /// 消息编排：system=ask 提示词；user=论文全文；assistant=已生成笔记
    /// （把它们放进多轮对话让 LLM"看过"长文，再以多轮 Q&A 追加历史）；
    /// user=问题（追加检索到的知识库相关概念提示语）。`block_id` 由调用方
    /// 预先定位（章节编号/关键词/批注引用的块），仅用于返回定位信息。
    /// 上下文长度控制：先估算 论文+笔记+问题 的 token 基数，在
    /// context_length 内从后往前保留尽量多的对话历史，溢出则提示并截断最早轮。
    /// 返回 (messages, block_id)。
    fn build_context_messages(&self, question: &str, block_id: Option<&str>, quote: Option<&str>) -> (Vec<Message>, Option<String>) {
        let (raw_text, notes_md) = match self.session.notes.as_ref() {
            Some(note) => (note.raw_text.clone(), note.to_markdown()),
            None => (String::new(), String::new()),
        };
        let block_id = block_id.map(|s| s.to_string());

        let path: Vec<(String, String)> = self
            .session
            .conversation
            .path_to_current()
            .iter()
            .map(|n| (n.question.clone(), n.answer.clone()))
            .collect();

        let ctx = self.config.llm.context_length;
        let base_tokens = (raw_text.chars().count() + notes_md.chars().count()) / 4;
        if base_tokens > ctx {
            outerr!(self, "{} 论文+笔记约 {} token，超过模型上下文 {}，可能报错。", "⚠️ ".yellow(), base_tokens, ctx);
        }
        let avail = ctx.saturating_sub(base_tokens + question.chars().count() / 4 + 200);
        let mut used = 0usize;
        let mut keep_from = path.len();
        while keep_from > 0 {
            let (q, a) = &path[keep_from - 1];
            let t = (q.chars().count() + a.chars().count()) / 4;
            if used + t > avail {
                break;
            }
            used += t;
            keep_from -= 1;
        }
        let dropped = keep_from;
        let kept_pairs: Vec<(String, String)> = path[keep_from..].to_vec();
        if dropped > 0 {
            outerr!(self, "{}（上下文偏长，已省略最早 {} 轮对话）", "".dimmed(), dropped);
        }

        // 论文/资料原文；直接导入的笔记 raw_text 为空 → 省略该段（只发笔记本身）
        let mut msgs = vec![Message::text("system", sys_ask())];
        if !raw_text.trim().is_empty() {
            msgs.push(Message::text("user", format!("【原文材料】\n{raw_text}")));
        }
        msgs.push(Message::text("assistant", format!("【已生成笔记】\n{notes_md}")));
        for (q, a) in &kept_pairs {
            msgs.push(Message::text("user", q.clone()));
            msgs.push(Message::text("assistant", a.clone()));
        }

        let related = self.kb.search(question);
        // 批注提问时，把用户选中的原文一并作为上下文（普通 ask/check 无 quote）
        let mut q_final = String::new();
        if let Some(qt) = quote {
            if !qt.trim().is_empty() {
                q_final.push_str("【用户选中的原文】\n");
                q_final.push_str(qt.trim());
                q_final.push_str("\n【问题】\n");
            }
        }
        q_final.push_str(question);
        if !related.is_empty() {
            q_final.push_str("\n\n【你之前学过的相关概念，可参考并建立联系】");
            for c in &related {
                let d: String = c.definition.chars().take(80).collect();
                q_final.push_str(&format!("\n- {}（来自《{}》）: {}", c.name, c.paper_title, d));
            }
        }
        msgs.push(Message::text("user", q_final));
        (msgs, block_id)
    }
}

// ===== 辅助函数 =====

/// 解析用户输入的文件路径/值参数（shell 风格）：
/// - 整体被 "..." 包裹 → 剥掉双引号（内部反斜杠转义一并还原）
/// - 整体被 '...' 包裹 → 剥掉单引号（内部无转义）
/// - 否则 → 还原反斜杠转义（Tab 补全器对含空格路径插入的 `my\ file.pdf` 形式）
/// 这样 `ingest my\ file.pdf`、`ingest "my file.pdf"`、`ingest 'my file.pdf'` 均可用。
fn normalize_path_arg(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        rustyline::completion::unescape(&t[1..t.len() - 1], Some('\\')).into_owned()
    } else if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
        t[1..t.len() - 1].to_string()
    } else {
        rustyline::completion::unescape(t, Some('\\')).into_owned()
    }
}

fn split_cmd(line: &str) -> (&str, &str) {
    // 按首个空白切分：命令 + 其余参数（命令本身不含空格）
    let mut it = line.splitn(2, char::is_whitespace);
    let cmd = it.next().unwrap_or("");
    let rest = it.next().unwrap_or("").trim();
    (cmd, rest)
}

/// 取出 `--style <值>`（也支持 `--style=值`），返回 (值, 去掉该选项后的剩余参数)。
/// 允许 `--style` 与 `--text`/`--ocr` 任意顺序；路径含空格请用引号包裹。
fn take_style_arg(rest: &str) -> Result<(String, String)> {
    let toks = split_args_quoted(rest);
    let mut style = String::new();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i].as_str();
        if t == "--style" {
            if i + 1 >= toks.len() {
                bail!("--style 缺少取值（可用 `styles` 查看全部风格）");
            }
            style = toks[i + 1].clone();
            i += 2;
            continue;
        }
        if let Some(v) = t.strip_prefix("--style=") {
            style = v.to_string();
            i += 1;
            continue;
        }
        out.push(toks[i].clone());
        i += 1;
    }
    Ok((style, out.join(" ")))
}

/// 按 shell 风格切分参数：支持双/单引号（引号内保留空格），
/// 引号与转义序列原样保留，交给 `normalize_path_arg` 统一还原。
fn split_args_quoted(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == '\\' && q == '"' {
                    cur.push(c);
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                } else if c == q {
                    cur.push(c);
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                    cur.push(c);
                } else if c.is_whitespace() {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                } else {
                    cur.push(c);
                }
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 取出 `--<name> <值>` / `--<name>=<值>`（值按 shell 风格还原引号/转义），返回 (值, 剩余)。
fn take_value_arg(rest: &str, name: &str) -> (String, String) {
    let toks = split_args_quoted(rest);
    let mut val = String::new();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i].as_str();
        let flag = format!("--{name}");
        if t == flag {
            if i + 1 < toks.len() {
                val = normalize_path_arg(&toks[i + 1]);
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(v) = t.strip_prefix(&format!("--{name}=")) {
            val = normalize_path_arg(v);
            i += 1;
            continue;
        }
        out.push(toks[i].clone());
        i += 1;
    }
    (val, out.join(" "))
}

/// 取出布尔开关 `--<name>`，返回 (是否存在, 剩余)。
fn take_bool_arg(rest: &str, name: &str) -> (bool, String) {
    let toks = split_args_quoted(rest);
    let mut found = false;
    let out: Vec<String> = toks
        .into_iter()
        .filter(|t| {
            if *t == format!("--{name}") {
                found = true;
                false
            } else {
                true
            }
        })
        .collect();
    (found, out.join(" "))
}

/// 从参数串里去掉一个布尔开关（任意位置，支持引号），返回剩余参数；
/// 没有该开关时返回 None。用于 `ask --no-concept` 这类可选项。
fn strip_flag(rest: &str, flag: &str) -> Option<String> {
    let toks = split_args_quoted(rest);
    if !toks.iter().any(|t| t == flag) {
        return None;
    }
    let out: Vec<String> = toks.into_iter().filter(|t| t != flag).collect();
    Some(out.join(" "))
}

/// 收集块及其全部子块的 id（删除块后清理相关批注用）。
fn collect_block_ids(b: &notes::Block, out: &mut Vec<String>) {
    out.push(b.id.clone());
    for c in &b.children {
        collect_block_ids(c, out);
    }
}

fn short(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn parse_bool(s: &str) -> bool {
    matches!(s.to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// 用问题前若干字作为概念名 / 节点标签（免 token）。
/// 解析 ask 参数：若第一个 token 形如 "3.2"（数字.数字...）则视为编号，
/// 返回 (编号, 剩余问题)；否则返回 (None, 整个 args)。
fn parse_ask_args(args: &str) -> (Option<String>, String) {
    let args = args.trim();
    let mut parts = args.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("");
    if is_section_number(first) && !rest.trim().is_empty() {
        (Some(first.to_string()), rest.to_string())
    } else {
        (None, args.to_string())
    }
}


/// 文件名安全化：替换非法字符（保留 `/` 目录分隔符与空格，允许用户指定输出目录），
/// 限制长度。若含路径，父目录需存在。
fn sanitize_filename(s: &str) -> String {
    let s = s.trim();
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' | '\r' => '_',
            // 保留 '/'（目录分隔符）、空格、中文等合法字符
            _ => c,
        })
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        "note.md".to_string()
    } else if !cleaned.ends_with(".md") && !cleaned.ends_with(".html") {
        format!("{}.md", cleaned.chars().take(80).collect::<String>())
    } else {
        cleaned.chars().take(83).collect()
    }
}

/// 判断是否是章节编号：如 "3", "3.2", "3.2.1"。
fn is_section_number(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn derive_concept(q: &str) -> String {
    q.chars().take(20).collect()
}

/// 从 LLM 回答末尾解析 [[概念: XXX]] 行，返回 (去掉该行的正文, 概念名)。
/// 模型没标注（或标注为空）时返回 `None`：**不再用问题文本兜底**，
/// 避免把“这段什么意思”这类非知识点提问也记成“已学概念”。
fn extract_concept(content: &str) -> (String, Option<String>) {
    // 找最后一行含 [[概念: ...]] 的
    let mut concept: Option<String> = None;
    let mut clean_lines = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("[[概念:").and_then(|s| s.strip_suffix("]]")) {
            let name = rest.trim().to_string();
            if !name.is_empty() {
                concept = Some(name);
                continue; // 跳过这行，不加入 clean
            }
        }
        clean_lines.push(line);
    }
    let clean_answer = clean_lines.join("\n").trim_end().to_string();
    (clean_answer, concept)
}

/// 构造「整理概念关系」的 LLM 消息：列出所有概念（名称+定义摘要），
/// 要求只针对 `new_names` 输出固定格式的关系行。
fn build_graph_messages(kb: &KnowledgeBase, new_names: &[String]) -> Vec<Message> {
    let list: Vec<String> = kb
        .unique_concept_names()
        .iter()
        .map(|n| {
            let def = kb
                .concepts
                .iter()
                .find(|c| &c.name == n)
                .map(|c| c.definition.chars().take(80).collect::<String>())
                .unwrap_or_default();
            format!("- {n}：{def}")
        })
        .collect();
    let sys = "你在为学习笔记构建跨论文概念图谱。只依据给定的概念定义判断关系，不要杜撰概念或关系。";
    let user = format!(
        "概念列表（名称：定义）：\n{}\n\n请判断这些「新概念」与列表中其它概念之间的关系。新概念：{}\n\n\
输出要求：\n\
- 每行一条关系，格式严格为：概念A => 概念B || 关系类型 || 一句话说明\n\
- 关系类型只能取：前置、相关、对比、包含、应用；前置/包含/应用有方向（A 是 B 的前提/组成/应用）\n\
- 只输出新概念参与、且确实存在的关系，宁少勿错，最多 5 条\n\
- 没有任何明显关系就只输出：无\n\
- 不要输出标题、解释、序号或代码块标记",
        list.join("\n"),
        new_names.join("、")
    );
    vec![Message::text("system", sys), Message::text("user", user)]
}

/// 解析 LLM 输出的关系行（`A => B || 类型 || 说明`），跳过格式不符的行。
pub fn parse_graph_relations(reply: &str) -> Vec<ConceptRelation> {
    let mut out: Vec<ConceptRelation> = Vec::new();
    for raw in reply.lines() {
        let line = raw
            .trim()
            .trim_start_matches(|c: char| c == '-' || c == '*' || c == ' ')
            .trim();
        if line.is_empty() || line.contains('`') {
            continue;
        }
        let Some((lhs, rhs)) = line.split_once("=>") else {
            continue;
        };
        let from = lhs.trim().to_string();
        let parts: Vec<&str> = rhs.split("||").map(|s| s.trim()).collect();
        if parts.len() < 2 || from.is_empty() || parts[0].is_empty() || parts[1].is_empty() {
            continue;
        }
        let rel = ConceptRelation {
            from,
            to: parts[0].to_string(),
            kind: parts[1].to_string(),
            note: parts.get(2).map(|s| s.to_string()).unwrap_or_default(),
        };
        if !out
            .iter()
            .any(|x| x.from == rel.from && x.to == rel.to && x.kind == rel.kind)
        {
            out.push(rel);
        }
    }
    out
}

pub fn mask_key(k: &str) -> String {
    if k.is_empty() {
        "（未设置）".into()
    } else if k.len() <= 8 {
        format!("{}…", &k[..k.len() / 2])
    } else {
        format!("{}…{}", &k[..4], &k[k.len() - 4..])
    }
}

#[cfg(test)]
mod tests {
    use super::{
        extract_concept, normalize_path_arg, parse_graph_relations, strip_flag, take_bool_arg,
        take_style_arg, take_value_arg, CommandCompleter, ExtraInput,
    };
    use rustyline::completion::Completer;
    use std::sync::{Arc, Mutex};

    #[test]
    fn take_style_arg_variants() {
        let (s, r) = take_style_arg("--style translate \"a b.pdf\"").unwrap();
        assert_eq!(s, "translate");
        assert_eq!(r, "\"a b.pdf\"");
        let (s, r) = take_style_arg("--text --style=free x.txt").unwrap();
        assert_eq!(s, "free");
        assert_eq!(r, "--text x.txt");
        let (s, r) = take_style_arg("x.pdf").unwrap();
        assert!(s.is_empty());
        assert_eq!(r, "x.pdf");
        assert!(take_style_arg("--style").is_err(), "缺取值应报错");
    }

    /// 关系行解析：合法行解析、去重、跳过「无」与格式不符/代码块标记的行。
    #[test]
    fn graph_relation_parsing_is_lenient() {
        let out = parse_graph_relations(
            "无\n\
             - 贝叶斯定理 => 先验概率 || 前置 || 先验是贝叶斯的基础\n\
             * 熵 => 交叉熵 || 相关 || 都由信息量定义\n\
             熵 => 交叉熵 || 相关 || 重复不应再加一条\n\
             `代码块跳过`\n\
             缺字段 => 只有名字\n\
             => 缺左边 || 相关 || x",
        );
        assert_eq!(out.len(), 2, "重复与非法行应被丢弃: {out:?}");
        assert_eq!(out[0].from, "贝叶斯定理");
        assert_eq!(out[0].to, "先验概率");
        assert_eq!(out[0].kind, "前置");
        assert!(out[0].note.contains("基础"));
        assert_eq!(out[1].note, "都由信息量定义");
    }

    /// 附件并入消息列表：追加到末条 user 消息；末条非 user 时新增一条；空附件无副作用。
    #[test]
    fn extra_input_appends_to_last_user_message() {
        use crate::llm::Message;
        let extra = ExtraInput {
            text: "\n\n【附件：a.txt】\n内容".to_string(),
            images: vec!["data:image/png;base64,AA".to_string()],
        };
        let mut msgs = vec![Message::text("user", "问题")];
        extra.apply_to(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.contains("问题") && msgs[0].content.contains("内容"));
        assert_eq!(msgs[0].images.len(), 1);

        let mut msgs = vec![Message::text("assistant", "答")];
        extra.apply_to(&mut msgs);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].role, "user");

        let mut msgs = vec![Message::text("user", "问")];
        ExtraInput::default().apply_to(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "问");
        assert!(msgs[0].images.is_empty());
    }

    #[test]
    fn take_value_and_bool_args() {
        let (v, r) = take_value_arg("--extra 补充直觉 --text a.pdf", "extra");
        assert_eq!(v, "补充直觉");
        assert_eq!(r, "--text a.pdf");
        let (v, r) = take_value_arg("--kind=lecture x.md", "kind");
        assert_eq!(v, "lecture");
        assert_eq!(r, "x.md");
        let (v, r) = take_value_arg("x.md", "extra");
        assert!(v.is_empty());
        assert_eq!(r, "x.md");
        let (b, r) = take_bool_arg("--note --text x.md", "note");
        assert!(b);
        assert_eq!(r, "--text x.md");
        let (b, r) = take_bool_arg("--text x.md", "note");
        assert!(!b);
        assert_eq!(r, "--text x.md");
        // 带空格的引号值（「本次额外要求」常见）
        let (v, r) = take_value_arg("--extra \"只翻译 不要总结\" --text x.md", "extra");
        assert_eq!(v, "只翻译 不要总结");
        assert_eq!(r, "--text x.md");
        let (b, r) = take_bool_arg("--note --extra \"a b\" x.md", "note");
        assert!(b);
        assert_eq!(r, "--extra \"a b\" x.md");
    }

    #[test]
    fn extract_concept_from_answer() {
        let content = "BERTScore是相似度指标。\n\n[[概念: BERTScore]]";
        let (clean, concept) = extract_concept(content);
        assert!(!clean.contains("[[概念"), "clean 应去掉概念行: {clean}");
        assert_eq!(concept.as_deref(), Some("BERTScore"));
    }

    #[test]
    fn extract_concept_without_marker_is_none() {
        // 没有 [[概念: …]] 时不再用问题文本兜底（“这段什么意思”不该成为已学概念）
        let content = "这个方法叫注意力机制。";
        let (clean, concept) = extract_concept(content);
        assert_eq!(clean, content);
        assert!(concept.is_none());
    }

    #[test]
    fn strip_flag_removes_option() {
        assert_eq!(strip_flag("3.2 什么是X --no-concept", "--no-concept").as_deref(), Some("3.2 什么是X"));
        assert_eq!(strip_flag("--no-concept 什么是X", "--no-concept").as_deref(), Some("什么是X"));
        assert!(strip_flag("3.2 什么是X", "--no-concept").is_none());
    }

    fn make_completer() -> CommandCompleter {
        CommandCompleter {
            file_completer: rustyline::completion::FilenameCompleter::new(),
            section_numbers: Arc::new(Mutex::new(vec!["1".into(), "3.2".into()])),
            node_numbers: Arc::new(Mutex::new(vec!["1".into(), "2".into()])),
            preset_models: vec!["deepseek-v4-pro".into(), "test-model".into()],
            preset_endpoints: vec!["https://api.deepseek.com/v1/chat/completions".into()],
        }
    }

    fn complete(line: &str) -> Vec<String> {
        let c = make_completer();
        // 模拟光标在行尾
        let (_, cands) = c.complete(line, line.len(), &mut rustyline::Context::new(&rustyline::history::MemHistory::new())).unwrap();
        cands
    }

    #[test]
    fn config_completes_subcommand() {
        let cands = complete("config ");
        assert!(cands.contains(&"set".to_string()), "应有 set: {cands:?}");
        assert!(cands.contains(&"show".to_string()), "应有 show: {cands:?}");
        // 前缀过滤（set 和 show 都以 s 开头；用 se 只剩 set）
        let cands = complete("config se");
        assert!(cands.contains(&"set".to_string()));
        assert!(!cands.contains(&"show".to_string()), "se 前缀不应匹配 show: {cands:?}");
    }

    #[test]
    fn config_completes_keys() {
        let cands = complete("config set ");
        assert!(cands.contains(&"llm.api_key".to_string()), "应有 llm.api_key: {cands:?}");
        assert!(cands.contains(&"budget.token_budget".to_string()));
        // 前缀过滤
        let cands = complete("config set llm.");
        assert!(cands.iter().all(|c| c.starts_with("llm.")));
        assert!(cands.contains(&"llm.model".to_string()));
        // show 后不补键名
        let cands = complete("config show ");
        assert!(cands.is_empty());
    }

    #[test]
    fn config_completes_values() {
        let cands = complete("config set llm.thinking_mode ");
        assert!(cands.contains(&"true".to_string()));
        assert!(cands.contains(&"false".to_string()));
        let cands = complete("config set llm.api_endpoint ");
        assert!(cands.iter().any(|c| c.contains("deepseek")));
    }

    #[test]
    fn normalize_path_arg_forms() {
        // 双引号包裹
        assert_eq!(normalize_path_arg("\"my file.pdf\""), "my file.pdf");
        // 单引号包裹
        assert_eq!(normalize_path_arg("'my file.pdf'"), "my file.pdf");
        // 补全器插入的反斜杠转义
        assert_eq!(normalize_path_arg("my\\ file.pdf"), "my file.pdf");
        // 转义的括号/引号（中文文件名常见）
        assert_eq!(normalize_path_arg("【MIND】\\ paper.pdf"), "【MIND】 paper.pdf");
        // 普通路径原样
        assert_eq!(normalize_path_arg("samples/paper.pdf"), "samples/paper.pdf");
        // 首尾空格剥掉
        assert_eq!(normalize_path_arg("  a.pdf  "), "a.pdf");
        // 双引号内转义的引号
        assert_eq!(normalize_path_arg("\"a\\\"b.pdf\""), "a\"b.pdf");
    }

    #[test]
    fn normalize_path_arg_integration() {
        // 创建带空格文件名的临时文件，验证 normalize 后能命中存在检查
        let dir = std::env::temp_dir().join("paperhelper_space_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("my file.pdf");
        std::fs::write(&p, "x").unwrap();
        let base = dir.to_str().unwrap();
        // 三种 shell 风格：引号包整体 / 反斜杠转义
        for form in [
            format!("\"{base}/my file.pdf\""),
            format!("'{base}/my file.pdf'"),
            format!("{base}/my\\ file.pdf"),
        ] {
            let normalized = normalize_path_arg(&form);
            assert!(std::path::Path::new(&normalized).exists(), "应存在: {normalized} (from {form})");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `ingest --read`：仅接受 PDF 且走 Read 分支（不进入 LLM 抽取流程）。
    #[tokio::test]
    async fn prepare_ingest_read_only_routes() {
        use crate::config::Config;
        use crate::knowledge::KnowledgeBase;
        let mut app = super::App::new(Config::default(), KnowledgeBase::default(), reqwest::Client::new());
        let dir = std::env::temp_dir().join("paperhelper_read_test");
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("paper.pdf");
        let txt = dir.join("note.txt");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        std::fs::write(&txt, b"hello").unwrap();

        let prep = app.prepare_ingest(&format!("--read {}", pdf.display())).await.unwrap();
        assert!(
            matches!(prep, super::IngestPrep::Read { .. }),
            "--read PDF 应走 Read 分支"
        );

        let e = match app.prepare_ingest(&format!("--read {}", txt.display())).await {
            Ok(_) => panic!("--read 非 PDF 应报错"),
            Err(e) => e,
        };
        assert!(e.to_string().contains("只支持 PDF"), "错误应说明仅支持 PDF: {e}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 仅阅读抽文本的提示文案：正常不打扰；空/失败要说明原因与建议。
    #[test]
    fn readonly_extract_notice_covers_three_cases() {
        assert_eq!(
            super::readonly_extract_notice(&super::ExtractOutcome::Text),
            None,
            "抽到文本时不应打扰用户"
        );
        let empty = super::readonly_extract_notice(&super::ExtractOutcome::Empty).unwrap();
        assert!(
            empty.contains("扫描") && empty.contains("OCR"),
            "空文本应说明是扫描/图片版并建议 OCR: {empty}"
        );
        assert!(
            !empty.contains("tesseract"),
            "仅阅读提示不应把依赖名塞给用户: {empty}"
        );
        let failed = super::readonly_extract_notice(&super::ExtractOutcome::Failed)
            .expect("失败必须提示用户");
        assert!(
            failed.contains("失败") && !failed.contains("Traceback"),
            "失败提示应友好、不含技术堆栈: {failed}"
        );
    }
}

