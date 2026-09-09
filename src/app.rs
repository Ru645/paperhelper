use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use rustyline::completion::{Completer, FilenameCompleter};
use rustyline::error::ReadlineError;
use rustyline::highlight::MatchingBracketHighlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::MatchingBracketValidator;
use rustyline::{Cmd, Editor, KeyEvent, Helper};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::Config;
use crate::conversation::{Conversation, ConvNode};
use crate::export;
use crate::interrupt;
use crate::knowledge::{Concept, KnowledgeBase, Paper};
use crate::llm::{self, Message};
use crate::notes::{self, Explanation};
use crate::pdf;
use crate::session::Session;

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
    "ingest", "ask", "check", "sum", "blocks", "note", "tree", "goto", "stats", "budget",
    "save", "load", "export", "papers", "concepts", "config", "new", "help", "exit",
];

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
    pub session: Session,
    pub client: reqwest::Client,
    /// ask 后自动导出笔记的文件路径（ingest 时由用户指定）。
    pub export_path: Option<String>,
    /// 当前笔记的 section 编号列表（供补全用）
    section_numbers: Arc<Mutex<Vec<String>>>,
    /// 当前对话树节点编号列表（供补全用）
    node_numbers: Arc<Mutex<Vec<String>>>,
}

impl App {
    pub fn new(config: Config, kb: KnowledgeBase, client: reqwest::Client) -> Self {
        // 首次启动写出默认提示词模板，供用户在 .paperhelper/prompts/ 编辑
        let _ = crate::prompts::ensure_prompt_files();
        Self {
            config,
            kb,
            session: Session::default(),
            client,
            export_path: None,
            section_numbers: Arc::new(Mutex::new(Vec::new())),
            node_numbers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn repl(&mut self) -> Result<()> {
        println!("{}", "=== PaperHelper 论文学习助手 ===".bold().cyan());
        println!(
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
            println!("{}", "⚠️  配置不完整，请先完成以下设置（或写 .env）：".yellow());
            if need_key {
                println!("  > config set llm.api_key <你的key>");
            }
            if default_endpoint {
                println!("  > config set llm.api_endpoint https://api.deepseek.com/v1/chat/completions");
                println!("  > config set llm.model deepseek-v4-pro");
            }
            println!("  示例（DeepSeek）：endpoint=https://api.deepseek.com/v1/chat/completions  model=deepseek-v4-pro");
        }
        println!("{}  help 查看命令；exit 退出。Ctrl-C 打断当前任务，↑↓ 切换历史，Tab 补全。\n",
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
                        eprintln!("{} {e:#}", "❌".red());
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    // Ctrl-C：打断当前 LLM 任务（如果有），不退出
                    if !interrupt::is_interrupted() {
                        eprintln!("{}", "[已打断当前任务]".yellow());
                    }
                }
                Err(ReadlineError::Eof) => {
                    println!();
                    break;
                }
                Err(e) => {
                    eprintln!("{} 读取输入失败: {e}", "❌".red());
                }
            }
        }
        let _ = rl.save_history(&hist_path);
        // 退出时自动保存会话
        self.autosave_on_exit().await?;
        Ok(())
    }

    /// 退出时自动保存会话到 .paperhelper/sessions/。
    /// 用递增数字 ID 作文件名（永不变），会话名存在 JSON 内部供 -l 展示。
    async fn autosave_on_exit(&mut self) -> Result<()> {
        if self.session.notes.is_none() && self.session.conversation.nodes.is_empty() {
            return Ok(());
        }
        crate::paths::ensure_sessions_dir()?;

        // 让 LLM 给会话取个简短名字（带进度条）
        let bar = ProgressBar::new_spinner();
        bar.set_style(spinner_style());
        bar.set_message("保存会话中…");
        bar.enable_steady_tick(Duration::from_millis(100));
        let session_name = self.generate_session_name().await;
        bar.finish_and_clear();
        self.session.session_name = session_name;

        // 会话编号：首次保存时生成（保存时间戳），此后不变；文件按编号覆盖保存
        if self.session.session_id.is_empty() {
            self.session.session_id = crate::paths::new_session_stamp();
        }
        let path = crate::paths::session_path(&self.session.session_id);
        self.session.save(&path)?;
        println!("{} 会话已保存：{}", "✓".green().bold(), self.session.session_name);
        println!("  恢复会话，请执行：paperhelper -s {}", self.session.session_id);
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
        let msgs = vec![Message {
            role: "user".into(),
            content: prompt,
        }];
        match llm::chat(&self.client, &self.config.llm, &msgs, false, false, &mut |_| {}).await {
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

    pub async fn run_command(&mut self, line: &str) -> Result<()> {
        let (cmd, rest) = split_cmd(line);
        match cmd {
            "help" | "?" => self.cmd_help(),
            "config" => self.cmd_config(rest).await,
            "budget" => self.cmd_budget(rest).await,
            "blocks" => self.cmd_blocks().await,
            "note" => self.cmd_note().await,
            "tree" | "trajectory" => {
                println!("{}", self.session.conversation.render_tree());
                Ok(())
            }
            "goto" => self.cmd_goto(rest).await,
            "stats" => self.cmd_stats().await,
            "papers" => self.cmd_papers().await,
            "concepts" => self.cmd_concepts().await,
            "new" => {
                self.session = Session::default();
                println!("已新建会话。");
                Ok(())
            }
            "save" => self.cmd_save(rest).await,
            "load" => self.cmd_load(rest).await,
            "export" => self.cmd_export(rest).await,
            "ingest" | "pdf" => self.cmd_ingest(rest).await,
            "ask" | "q" => self.cmd_ask(rest).await,
            "check" => self.cmd_check(rest).await,
            "sum" => self.cmd_sum().await,
            _ => {
                println!("未知命令: {cmd}。输入 help 查看帮助。");
                Ok(())
            }
        }?;
        self.update_completions();
        Ok(())
    }

    fn cmd_help(&self) -> Result<()> {
        let h = "\
PaperHelper 命令：
  ingest <pdf>            解析 PDF 并生成结构化笔记
  ingest --text <txt>     直接读取文本文件（跳过PDF解析）
  ingest --ocr <pdf>      OCR 识别扫描件（需 tesseract）
  ask <编号> <问题>        基于论文全文+笔记回答，解释插入笔记对应位置
                          编号见 blocks 或导出笔记的标题（如 3.2）；不填编号则关键词匹配
                          例: ask 3.2 BERTScore的公式里max_k是什么意思
  check <编号> <想法>      与 ask 类似但不写入笔记，用于核对想法
                          例: check 3.2 我觉得BERTScore就是余弦相似度，对吗
  sum                      把当前节点子树的追问折叠并替换为「总结：…」（可点击展开）
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
  config show              查看配置
  config set <k> <v>       设置(如 llm.api_key / llm.model / llm.context_length)
  new                      新建会话
  exit                     退出（自动保存会话）

启动方式：
  paperhelper              新会话
  paperhelper -s <序号>    恢复指定会话（先用 -l 查看序号）
  paperhelper -l           列出所有已保存会话";
        println!("{h}");
        Ok(())
    }

    async fn cmd_config(&mut self, rest: &str) -> Result<()> {
        let (sub, args) = split_cmd(rest);
        match sub {
            "" | "show" => {
                let k = &self.config.llm;
                println!("=== 配置 ===");
                println!("llm.api_endpoint   = {}", k.api_endpoint);
                println!("llm.api_key        = {}", mask_key(&k.api_key));
                println!("llm.model           = {}", k.model);
                println!("llm.context_length  = {}", k.context_length);
                println!("llm.thinking_mode   = {}", k.thinking_mode);
                println!("llm.pdf_input       = {} (file模式未实现,均走text)", k.pdf_input);
                println!("pricing.input_price_per_1m  = {}", self.config.pricing.input_price_per_1m);
                println!("pricing.output_price_per_1m = {}", self.config.pricing.output_price_per_1m);
                println!("budget.token_budget = {} (0=不限)", self.config.budget.token_budget);
                println!("提示：api_endpoint 需是完整 URL（含 /chat/completions），如 https://api.deepseek.com/v1/chat/completions");
            }
            "set" => {
                let (key, val) = split_cmd(args);
                if key.is_empty() {
                    println!("用法: config set <key> <value>");
                    let keys: Vec<&str> = CONFIG_KEY_DEFS.iter().map(|d| d.key).collect();
                    println!("可设: {}", keys.join(" "));
                    println!("常见端点：");
                    println!("  DeepSeek : https://api.deepseek.com/v1/chat/completions  model=deepseek-v4-pro");
                    println!("  OpenAI   : https://api.openai.com/v1/chat/completions    model=gpt-4o-mini");
                    println!("  本地Ollama: http://localhost:11434/v1/chat/completions    model=qwen2.5:7b");
                    return Ok(());
                }
                let val = normalize_path_arg(val);
                self.set_config(key, &val)?;
                self.config.save()?;
                // api_key 脱敏回显，避免明文泄露
                let display = if key == "llm.api_key" { mask_key(&val) } else { val.clone() };
                println!("已设置 {key} = {display}（已写入 .paperhelper/config.toml）");
            }
            _ => println!("用法: config [show | set <key> <value>]"),
        }
        Ok(())
    }

    fn set_config(&mut self, key: &str, val: &str) -> Result<()> {
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
            _ => {
                let keys: Vec<&str> = CONFIG_KEY_DEFS.iter().map(|d| d.key).collect();
                bail!("未知配置项: {key}。可设: {}", keys.join(" "));
            }
        }
        Ok(())
    }

    async fn cmd_budget(&mut self, rest: &str) -> Result<()> {
        if rest.trim().is_empty() {
            println!("当前 token 预算: {} (0=不限)", self.config.budget.token_budget);
            return Ok(());
        }
        self.config.budget.token_budget = rest.trim().parse().context("需要整数")?;
        self.config.save()?;
        println!("token 预算已设为 {}", self.config.budget.token_budget);
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
            println!("{} {}{} {} {}{}", num, indent, b.kind.tag(), text, "", expl);
        }
        Ok(())
    }

    async fn cmd_note(&self) -> Result<()> {
        let note = self.session.notes.as_ref().ok_or_else(|| anyhow!("还没有笔记"))?;
        println!("{}", export::to_markdown(note));
        Ok(())
    }

    async fn cmd_stats(&self) -> Result<()> {
        let s = &self.session.stats;
        let k = &self.kb.stats;
        println!("=== 用量统计 ===");
        println!("本次会话: {} 次调用 | 输入 {} / 输出 {} tok | 小计 ${:.6}", s.calls, s.total_input, s.total_output, s.total_cost);
        println!("累计(跨会话): {} 次调用 | 输入 {} / 输出 {} tok | 小计 ${:.6}", k.calls, k.total_input, k.total_output, k.total_cost);
        let tot = s.total_tokens() + k.total_tokens();
        if self.config.budget.token_budget > 0 {
            println!("预算: {} (累计已用 {:.1}%)", self.config.budget.token_budget,
                tot as f64 / self.config.budget.token_budget as f64 * 100.0);
        } else {
            println!("预算: 未设置（`budget <n>` 设置）");
        }
        Ok(())
    }

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
                println!("已跳转到: {}", n.label);
                println!("--- 根路径对话 ---");
                for (i, x) in self.session.conversation.path_to_current().iter().enumerate() {
                    println!("{}. Q: {}", i + 1, x.question);
                }
            }
            None => println!("找不到节点 {arg}。用 `tree` 查看可用节点。"),
        }
        Ok(())
    }

    async fn cmd_papers(&self) -> Result<()> {
        if self.kb.papers.is_empty() {
            println!("（还没有读过论文）");
            return Ok(());
        }
        for p in &self.kb.papers {
            println!("- [{}] 《{}》（{}）", &p.id[..6.min(p.id.len())], p.title, p.path);
        }
        Ok(())
    }

    async fn cmd_concepts(&self) -> Result<()> {
        if self.kb.concepts.is_empty() {
            println!("（还没有学过概念）");
            return Ok(());
        }
        for c in &self.kb.concepts {
            let d: String = c.definition.chars().take(80).collect();
            println!("- {}（来自《{}》）: {}", c.name, c.paper_title, d);
        }
        Ok(())
    }

    async fn cmd_save(&mut self, rest: &str) -> Result<()> {
        let path = if rest.trim().is_empty() { "session.json".to_string() } else { normalize_path_arg(rest) };
        self.session.save(Path::new(&path))?;
        println!("会话已保存到 {path}");
        Ok(())
    }

    async fn cmd_load(&mut self, rest: &str) -> Result<()> {
        if rest.trim().is_empty() {
            bail!("用法: load <文件>");
        }
        let path = normalize_path_arg(rest);
        self.session = Session::load(Path::new(&path))?;
        println!("已加载会话: 笔记={}, 对话节点={}",
            self.session.notes.is_some(),
            self.session.conversation.nodes.len());
        Ok(())
    }

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
            "html" | "htm" => export::to_html(note, &self.session.conversation),
            _ => unreachable!(),
        };
        std::fs::write(&path, content)?;
        println!("已导出到 {path}");
        Ok(())
    }

    // ===== 以下为异步 LLM 相关命令（ingest / ask）=====

    async fn cmd_ingest(&mut self, rest: &str) -> Result<()> {
        let rest = rest.trim();
        // 解析选项（路径参数做 shell 风格还原：剥引号/反斜杠转义，支持含空格文件名）
        let (mode, file_path) = if let Some(r) = rest.strip_prefix("--text ") {
            ("text", normalize_path_arg(r))
        } else if let Some(r) = rest.strip_prefix("--ocr ") {
            ("ocr", normalize_path_arg(r))
        } else if rest == "--text" || rest == "--ocr" {
            bail!("用法: ingest --text <txt路径>  或  ingest --ocr <pdf路径>  或  ingest <pdf路径>");
        } else {
            ("pdf", normalize_path_arg(rest))
        };

        if file_path.is_empty() {
            bail!("用法: ingest <pdf路径>\n  ingest --text <txt路径>  直接读文本（跳过PDF解析）\n  ingest --ocr <pdf路径>  OCR提取（需安装 tesseract）");
        }
        let p = Path::new(&file_path);
        if !p.exists() {
            bail!("文件不存在: {file_path}");
        }
        interrupt::reset();

        // 1. 抽取文本
        let bar = ProgressBar::new_spinner();
        bar.set_style(spinner_style());
        bar.enable_steady_tick(Duration::from_millis(100));

        let raw_text = match mode {
            "text" => {
                bar.set_message("读取文本文件…");
                std::fs::read_to_string(p)?
            }
            "ocr" => {
                bar.set_message("OCR 识别中（可能较慢）…");
                pdf::ocr_extract(p)?
            }
            _ => {
                bar.set_message("解析 PDF…");
                let pages = pdf::extract_pages(p)?;
                pages.join("\n\n")
            }
        };
        bar.finish_and_clear();
        if raw_text.trim().is_empty() {
            bail!("文本内容为空（可能是扫描件，试试 ingest --ocr <pdf>）");
        }
        let char_count = raw_text.chars().count();
        println!("{} 文本已就绪: {} 字符", "✓".green().bold(), char_count);

        if interrupt::is_interrupted() {
            bail!("已打断");
        }

        // 2. 先询问笔记导出文件名（在等待 LLM 时让用户知道笔记存哪）
        let stem = Path::new(&file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("note");
        let title_guess: String = stem.chars().take(20).collect();
        let default_name = format!("笔记_{title_guess}.md");
        print!("请输入笔记导出文件名（回车默认 {default_name}）: ");
        io::stdout().flush()?;
        let mut name = String::new();
        io::stdin().lock().read_line(&mut name)?;
        let name = name.trim();
        let export_file = if name.is_empty() {
            default_name.clone()
        } else {
            sanitize_filename(&normalize_path_arg(name))
        };

        // 3. 调用 LLM 生成结构化 Markdown 笔记（不打印输出，只显示进度条）
        let budget_ok = self.check_budget()?;
        if !budget_ok {
            bail!("已达 token 预算，无法继续。用 `budget <n>` 调整。");
        }
        // 提示词模板：.paperhelper/prompts/note.txt（用户可编辑），{raw_text} 为占位符
        let template = crate::prompts::load_prompt(
            &crate::prompts::prompts_dir(),
            "note.txt",
            crate::prompts::DEFAULT_NOTE_PROMPT,
        );
        let prompt = template.replace("{raw_text}", &raw_text);
        let msgs = vec![
            Message { role: "system".into(), content: "你是论文笔记生成助手，只输出 Markdown。".into() },
            Message { role: "user".into(), content: prompt },
        ];
        let raw_clone = raw_text.clone();
        let mut first_token = true;
        let bar2 = ProgressBar::new_spinner();
        bar2.set_style(spinner_style());
        bar2.set_message("笔记生成中…");
        bar2.enable_steady_tick(Duration::from_millis(100));
        let res = llm::chat(&self.client, &self.config.llm, &msgs, false, self.config.llm.thinking_mode, &mut |t| {
            if first_token {
                bar2.finish_and_clear();
                first_token = false;
            }
            print!("{t}");
            let _ = io::stdout().flush();
        }).await;
        println!();
        let res = res?;
        if first_token {
            bar2.finish_and_clear();
        }

        // 3. 统计与预算检查
        self.record_usage(res.input_tokens, res.output_tokens);

        // 4. 解析 Markdown 为笔记树
        let note = notes::parse_markdown_note(&res.content, &raw_clone);
        let title = note.title.clone();
        let nblocks = note.count_blocks();
        println!("{} 笔记已生成: 《{}》({} 个结构块)", "✓".green().bold(), title, nblocks);

        // 5. 注册到知识库
        let paper_id = uuid::Uuid::new_v4().to_string();
        let mut note = note;
        note.paper_id = paper_id.clone();
        self.session.notes = Some(note);
        self.session.current_paper_id = Some(paper_id.clone());
        self.kb.add_paper(Paper {
            id: paper_id.clone(),
            title: title.clone(),
            path: file_path.to_string(),
            read_at: Utc::now().to_rfc3339(),
        });
        self.kb.save()?;

        // 6. 创建对话树的 0 号根节点（代表"论文已导入、尚未追问"状态）
        let root_id = uuid::Uuid::new_v4().to_string();
        self.session.conversation = Conversation::default();
        self.session.conversation.add_exchange(ConvNode {
            id: root_id.clone(),
            parent: None,
            question: format!("（导入论文《{}》，生成笔记，{} 个结构块）", title, nblocks),
            answer: String::new(),
            block_id: None,
            explanation_id: None,
            input_tokens: res.input_tokens,
            output_tokens: res.output_tokens,
            cost: res.input_tokens as f64 * self.config.pricing.input_price_per_1m / 1_000_000.0
                + res.output_tokens as f64 * self.config.pricing.output_price_per_1m / 1_000_000.0,
            created_at: Utc::now().to_rfc3339(),
            label: format!("导入《{}》", title),
        });
        self.session.conversation.current = Some(root_id);

        // 7. 导出 markdown 笔记（文件名在第 2 步已询问）
        if let Some(parent) = Path::new(&export_file).parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                bail!("导出目录不存在: {}（请先创建目录，或改用当前目录下的文件名）", parent.display());
            }
        }
        self.export_path = Some(export_file.clone());
        if let Some(note) = &self.session.notes {
            std::fs::write(&export_file, export::render_for(&export_file, note, &self.session.conversation))?;
            println!("{} 笔记已导出到 {}", "✓".green().bold(), export_file);
        }
        self.update_completions();
        Ok(())
    }

    async fn cmd_ask(&mut self, args: &str) -> Result<()> {
        let args = args.trim();
        // ask --help：打印用法
        if args == "--help" || args == "-h" || args.is_empty() {
            println!("用法: ask <编号> <问题>");
            println!("  <编号>    笔记中 Section 的编号（见 blocks 或导出笔记标题，如 3.2）");
            println!("  <问题>    你的追问内容");
            println!("例:");
            println!("  ask 3.2 BERTScore的公式里max_k是什么意思");
            println!("  ask 2.1 灰盒方法为什么对黑盒不适用");
            println!("说明: 解释会插入笔记对应 Section 下方。不填编号则退化为关键词匹配。");
            return Ok(());
        }
        let (block_num, question) = parse_ask_args(args);
        let question = question.trim();
        if question.is_empty() {
            bail!("用法: ask <编号> <问题>   例: ask 3.2 BERTScore是什么   (ask --help 看详情)");
        }
        if block_num.is_none() {
            eprintln!("{} 未指定编号，将用关键词匹配定位（可能不准）。建议用 `ask <编号> <问题>`。", "⚠️ ".yellow());
        }
        if self.session.notes.is_none() {
            bail!("还没有笔记，先 `ingest <pdf>`");
        }
        interrupt::reset();

        let budget_ok = self.check_budget()?;
        if !budget_ok {
            bail!("已达 token 预算，自动中断。用 `budget <n>` 调整。");
        }

        // 1-4. 构建上下文消息（论文全文+笔记+对话路径+概念注入）
        let (msgs, block_id) = self.build_context_messages(question, &block_num);
        let block_id_for_hint = block_id.clone();

        // 5. 流式调用 LLM
        let mut first_token = true;
        let bar = ProgressBar::new_spinner();
        bar.set_style(spinner_style());
        bar.set_message("思考中…");
        bar.enable_steady_tick(Duration::from_millis(100));
        let res = llm::chat(&self.client, &self.config.llm, &msgs, false, self.config.llm.thinking_mode, &mut |t| {
            if first_token {
                bar.finish_and_clear();
                first_token = false;
                print!("\n");
            }
            print!("{t}");
            let _ = io::stdout().flush();
        }).await;
        let res = res?;
        if first_token {
            bar.finish_and_clear();
        }
        println!();

        // 6. 记录统计
        self.record_usage(res.input_tokens, res.output_tokens);

        // 6.5 从 LLM 回答末尾提取概念名，去掉 [[概念:]] 标记行
        let (clean_answer, concept_name) = extract_concept(&res.content, question);
        let now = Utc::now().to_rfc3339();
        let expl_id = uuid::Uuid::new_v4().to_string();

        // 7. 把解释插入笔记
        //    嵌套判断：从当前节点沿父链向上找第一个有 explanation_id 的祖先（跳过
        //    check 等核对节点）——check 的儿子在笔记中的父亲是 check 的父亲。
        let parent_expl_id = self
            .session
            .conversation
            .current
            .as_deref()
            .and_then(|cur| {
                Conversation::explanation_ancestor(&self.session.conversation.nodes, cur)
            });

        let is_nested = parent_expl_id.is_some();
        if let Some(parent_eid) = parent_expl_id {
            // 嵌套追问：找到父解释，插入其 children
            if let Some(note) = self.session.notes.as_mut() {
                if let Some(parent_expl) = note.find_explanation_mut(&parent_eid) {
                    parent_expl.children.push(Explanation {
                        id: expl_id.clone(),
                        question: question.to_string(),
                        answer: clean_answer.clone(),
                        concept: concept_name.clone(),
                        created_at: now.clone(),
                        children: Vec::new(),
                        summary: None,
                        collapsed: false,
                    });
                }
            }
        } else {
            // 顶层追问：按 block_id 定位 block，插入顶层 explanations
            if let Some(bid) = &block_id {
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
                                question: question.to_string(),
                                answer: clean_answer.clone(),
                                concept: concept_name.clone(),
                                created_at: now.clone(),
                                children: Vec::new(),
                                summary: None,
                                collapsed: false,
                            });
                        }
                    }
                }
            }
        }

        // 8. 记录对话树节点（当前节点为父），关联 explanation_id
        let parent = self.session.conversation.current.clone();
        let node_id = uuid::Uuid::new_v4().to_string();
        self.session.conversation.add_exchange(ConvNode {
            id: node_id.clone(),
            parent,
            question: question.to_string(),
            answer: clean_answer.clone(),
            block_id: if is_nested { None } else { block_id.clone() },
            explanation_id: Some(expl_id.clone()),
            input_tokens: res.input_tokens,
            output_tokens: res.output_tokens,
            cost: res.input_tokens as f64 * self.config.pricing.input_price_per_1m / 1_000_000.0
                + res.output_tokens as f64 * self.config.pricing.output_price_per_1m / 1_000_000.0,
            created_at: now.clone(),
            label: concept_name.clone(),
        });
        self.session.conversation.current = Some(node_id);

        // 9. 加入知识库概念（用 LLM 提取的概念名）
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
            block_id: if is_nested { None } else { block_id },
            created_at: now,
        });
        self.kb.save()?;

        // 自动更新导出的 markdown 文件
        // 若 export_path 未设置，引导用户输入一次
        if self.export_path.is_none() {
            let title = self.session.notes.as_ref().map(|n| n.title.clone()).unwrap_or_default();
            let default_name = format!("笔记_{}.md", title.chars().take(20).collect::<String>());
            print!("请输入笔记导出文件名（回车默认 {}，输 skip 跳过）: ", default_name);
            io::stdout().flush()?;
            let mut name = String::new();
            io::stdin().lock().read_line(&mut name)?;
            let name = name.trim();
            if name == "skip" || name == "s" {
                eprintln!("{} 已跳过导出，之后可用 `export md <file>` 手动导出。", "".dimmed());
            } else {
                let path = if name.is_empty() { default_name } else { name.to_string() };
                self.export_path = Some(path.clone());
            }
        }
        if let Some(p) = &self.export_path {
            if let Some(note) = &self.session.notes {
                if std::fs::write(p, export::render_for(p, note, &self.session.conversation)).is_ok() {
                    // 查更新位置（block_id 对应的 section 编号+标题）
                    let location = block_id_for_hint.as_ref().and_then(|bid| {
                        note.find_block(bid).map(|b| {
                            let num = if b.number.is_empty() { String::new() } else { format!("{} ", b.number) };
                            format!("{num}{}", b.text.chars().take(30).collect::<String>())
                        })
                    });
                    match location {
                        Some(loc) => println!("{} 笔记已同步更新到 {}（更新位置：{}）", "✓".green().bold(), p, loc),
                        None => println!("{} 笔记已同步更新到 {}", "✓".green().bold(), p),
                    }
                }
            }
        }

        if res.estimated {
            println!("[注: 本次 token 数为估算]");
        }
        self.update_completions();
        Ok(())
    }

    /// check <编号> <想法>：与 ask 类似调 LLM 回答，但不写入笔记、不增加追问嵌套。
    /// 对话树仍记录此节点（用于上下文），但 explanation_id 为 None。
    async fn cmd_check(&mut self, args: &str) -> Result<()> {
        let args = args.trim();
        if args == "--help" || args == "-h" || args.is_empty() {
            println!("用法: check <编号> <想法>");
            println!("  <编号>    笔记中 Section 的编号（见 blocks 或导出笔记标题，如 3.2）");
            println!("  <想法>    你想核对/验证的想法或理解");
            println!("例:");
            println!("  check 3.2 我觉得BERTScore本质上就是余弦相似度，对吗");
            println!("说明: 回答只显示在终端，不写入笔记。对话树会记录此节点。");
            return Ok(());
        }
        let (block_num, question) = parse_ask_args(args);
        let question = question.trim();
        if question.is_empty() {
            bail!("用法: check <编号> <想法>   例: check 3.2 我觉得这个方法等价于余弦相似度");
        }
        if self.session.notes.is_none() {
            bail!("还没有笔记，先 `ingest <pdf>`");
        }
        interrupt::reset();

        let budget_ok = self.check_budget()?;
        if !budget_ok {
            bail!("已达 token 预算，自动中断。用 `budget <n>` 调整。");
        }

        let (msgs, _block_id) = self.build_context_messages(question, &block_num);

        // 流式调用 LLM
        let mut first_token = true;
        let bar = ProgressBar::new_spinner();
        bar.set_style(spinner_style());
        bar.set_message("核对中…");
        bar.enable_steady_tick(Duration::from_millis(100));
        let res = llm::chat(&self.client, &self.config.llm, &msgs, false, self.config.llm.thinking_mode, &mut |t| {
            if first_token {
                bar.finish_and_clear();
                first_token = false;
                print!("\n");
            }
            print!("{t}");
            let _ = io::stdout().flush();
        }).await;
        let res = res?;
        if first_token {
            bar.finish_and_clear();
        }
        println!();

        // 记录统计
        self.record_usage(res.input_tokens, res.output_tokens);

        // 提取概念名（用于 label）
        let (clean_answer, concept_name) = extract_concept(&res.content, question);
        let now = Utc::now().to_rfc3339();

        // 对话树记录节点，但 explanation_id = None（不关联笔记解释）
        let parent = self.session.conversation.current.clone();
        let node_id = uuid::Uuid::new_v4().to_string();
        self.session.conversation.add_exchange(ConvNode {
            id: node_id.clone(),
            parent,
            question: question.to_string(),
            answer: clean_answer.clone(),
            block_id: None,
            explanation_id: None,
            input_tokens: res.input_tokens,
            output_tokens: res.output_tokens,
            cost: res.input_tokens as f64 * self.config.pricing.input_price_per_1m / 1_000_000.0
                + res.output_tokens as f64 * self.config.pricing.output_price_per_1m / 1_000_000.0,
            created_at: now,
            label: format!("[核对] {}", concept_name),
        });
        self.session.conversation.current = Some(node_id);

        if res.estimated {
            println!("[注: 本次 token 数为估算]");
        }
        self.update_completions();
        Ok(())
    }

    /// sum：把当前对话节点子树（含自己）的全部问答概括成知识卡片。
    /// 终端显示 + 追加写入 <笔记名>.cards.md。
    async fn cmd_sum(&mut self) -> Result<()> {
        if self.session.notes.is_none() {
            bail!("还没有笔记，先 `ingest <pdf>`");
        }
        let cur = self
            .session
            .conversation
            .current
            .clone()
            .ok_or_else(|| anyhow!("当前不在任何对话节点上，先 ask 提问"))?;
        // 收集子树（含自己），DFS 顺序
        let subtree = self.collect_subtree(&cur);
        if subtree.is_empty() {
            bail!("当前节点无问答内容");
        }
        interrupt::reset();
        let budget_ok = self.check_budget()?;
        if !budget_ok {
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
            Message { role: "system".into(), content: "你是学习总结助手，只输出总结正文。".into() },
            Message { role: "user".into(), content: prompt },
        ];

        let bar = ProgressBar::new_spinner();
        bar.set_style(spinner_style());
        bar.set_message("概括总结中…");
        bar.enable_steady_tick(Duration::from_millis(100));
        let res = llm::chat(&self.client, &self.config.llm, &msgs, false, self.config.llm.thinking_mode, &mut |_| {}).await;
        bar.finish_and_clear();
        let res = res?;
        self.record_usage(res.input_tokens, res.output_tokens);

        let summary = res.content.trim().to_string();
        println!("\n**总结**：{summary}\n");

        // 写入笔记：插入当前节点（或其最近有解释的祖先，跳过 check）对应的
        // Explanation：折叠其子树 + 挂总结
        let target_expl_id = Conversation::explanation_ancestor(&self.session.conversation.nodes, &cur)
            .ok_or_else(|| anyhow!("当前对话链上没有可插入总结的追问（先 `ask` 产生追问后再 `sum`）"))?;
        if let Some(note) = self.session.notes.as_mut() {
            if let Some(expl) = note.find_explanation_mut(&target_expl_id) {
                expl.summary = Some(summary);
                expl.collapsed = true;
            }
        }
        // 自动同步导出（与 ask 相同逻辑）
        if let Some(p) = &self.export_path {
            if let Some(note) = &self.session.notes {
                if std::fs::write(p, export::render_for(p, note, &self.session.conversation)).is_ok() {
                    println!("{} 笔记已同步更新到 {}（已插入总结）", "✓".green().bold(), p);
                }
            }
        }
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

    /// 记录一次 LLM 调用的 token 与成本（会话 + 全局）。
    fn record_usage(&mut self, input: u64, output: u64) {
        let in_price = self.config.pricing.input_price_per_1m;
        let out_price = self.config.pricing.output_price_per_1m;
        let cost = input as f64 * in_price / 1_000_000.0 + output as f64 * out_price / 1_000_000.0;
        self.session.stats.add(input, output, cost);
        self.kb.stats.add(input, output, cost);
    }

    /// 更新补全用的编号列表（section 编号 + 对话树节点编号）。
    fn update_completions(&self) {
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

    /// 构建 ask/check 共用的上下文消息（论文全文+笔记+对话路径+概念注入）。
    /// 返回 (messages, block_id)。
    fn build_context_messages(&self, question: &str, block_num: &Option<String>) -> (Vec<Message>, Option<String>) {
        let (raw_text, notes_md, block_id) = {
            let note = self.session.notes.as_ref().unwrap();
            let blk = if let Some(num) = block_num {
                note.find_section_by_number(num)
                    .or_else(|| note.locate(question))
            } else {
                note.locate(question)
            };
            let bid = blk.map(|b| b.id.clone());
            (note.raw_text.clone(), note.to_markdown(), bid)
        };

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
            eprintln!("{} 论文+笔记约 {} token，超过模型上下文 {}，可能报错。", "⚠️ ".yellow(), base_tokens, ctx);
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
            eprintln!("{}（上下文偏长，已省略最早 {} 轮对话）", "".dimmed(), dropped);
        }

        let mut msgs = vec![
            Message { role: "system".into(), content: sys_ask() },
            Message { role: "user".into(), content: format!("【论文全文】\n{raw_text}") },
            Message { role: "assistant".into(), content: format!("【已生成笔记】\n{notes_md}") },
        ];
        for (q, a) in &kept_pairs {
            msgs.push(Message { role: "user".into(), content: q.clone() });
            msgs.push(Message { role: "assistant".into(), content: a.clone() });
        }

        let related = self.kb.search(question);
        let mut q_final = question.to_string();
        if !related.is_empty() {
            q_final.push_str("\n\n【你之前学过的相关概念，可参考并建立联系】");
            for c in &related {
                let d: String = c.definition.chars().take(80).collect();
                q_final.push_str(&format!("\n- {}（来自《{}》）: {}", c.name, c.paper_title, d));
            }
        }
        msgs.push(Message { role: "user".into(), content: q_final });
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
    let mut it = line.splitn(2, char::is_whitespace);
    let cmd = it.next().unwrap_or("");
    let rest = it.next().unwrap_or("").trim();
    (cmd, rest)
}

fn short(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn parse_bool(s: &str) -> bool {
    matches!(s.to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner} {msg}").unwrap_or_else(|_| ProgressStyle::default_spinner())
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
/// 若未找到概念行，concept 用 derive_concept(question) 兜底。
fn extract_concept(content: &str, question: &str) -> (String, String) {
    // 找最后一行含 [[概念: ...]] 的
    let mut concept = None;
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
    let concept = concept.unwrap_or_else(|| derive_concept(question));
    (clean_answer, concept)
}

fn mask_key(k: &str) -> String {
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
    use super::{extract_concept, normalize_path_arg, CommandCompleter};
    use rustyline::completion::Completer;
    use std::sync::{Arc, Mutex};

    #[test]
    fn extract_concept_from_answer() {
        let content = "BERTScore是相似度指标。\n\n[[概念: BERTScore]]";
        let (clean, concept) = extract_concept(content, "什么是BERTScore");
        assert!(!clean.contains("[[概念"), "clean 应去掉概念行: {clean}");
        assert_eq!(concept, "BERTScore");
    }

    #[test]
    fn extract_concept_fallback() {
        let content = "这个方法叫注意力机制。";
        let (clean, concept) = extract_concept(content, "注意力机制是什么");
        assert_eq!(clean, content);
        assert!(!concept.is_empty());
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
}

