use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use rustyline::completion::Completer;
use rustyline::error::ReadlineError;
use rustyline::highlight::MatchingBracketHighlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::MatchingBracketValidator;
use rustyline::{Cmd, Editor, KeyEvent, Helper};
use std::io::{self, Write};
use std::path::Path;
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

const SYS_ASK: &str = "你是一位耐心的论文学习助手。用户会给你一篇论文的全文、已生成的结构化笔记，以及（可能的）历史问答。请基于这些回答用户问题，简洁清晰（300字以内），尽量和笔记的章节结构对齐。若涉及已学概念，点明它们的联系。";

const COMMANDS: &[&str] = &[
    "ingest", "ask", "blocks", "note", "tree", "goto", "stats", "budget",
    "save", "load", "export", "papers", "concepts", "config", "new", "help", "exit",
];

/// 命令补全器：补全第一个单词（命令名）。
struct CommandCompleter;

impl Completer for CommandCompleter {
    type Candidate = String;
    fn complete(&self, line: &str, pos: usize, _ctx: &rustyline::Context<'_>) -> rustyline::Result<(usize, Vec<String>)> {
        let start = line[..pos].rfind(' ').map(|i| i + 1).unwrap_or(0);
        let word = &line[start..pos];
        if line[..start].trim().is_empty() {
            let matches: Vec<String> = COMMANDS
                .iter()
                .filter(|c| c.starts_with(word))
                .map(|c| c.to_string())
                .collect();
            Ok((start, matches))
        } else {
            Ok((start, Vec::new()))
        }
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
    fn new() -> Self {
        Self {
            completer: CommandCompleter,
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
}

impl App {
    pub fn new(config: Config, kb: KnowledgeBase, client: reqwest::Client) -> Self {
        Self {
            config,
            kb,
            session: Session::default(),
            client,
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
        rl.set_helper(Some(PaperHelperHelper::new()));
        // 绑定 Ctrl-C 为中断信号（不打断程序，只取消当前输入/任务）
        rl.bind_sequence(KeyEvent::ctrl('c'), Cmd::Interrupt);
        let _ = rl.load_history(&hist_path);

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
        Ok(())
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
            _ => {
                println!("未知命令: {cmd}。输入 help 查看帮助。");
                Ok(())
            }
        }
    }

    fn cmd_help(&self) -> Result<()> {
        let h = "\
PaperHelper 命令：
  ingest <pdf>            解析 PDF 并生成结构化笔记
  ask <问题>               基于论文全文+笔记回答，解释插入笔记对应位置
  blocks                   列出笔记结构（带序号）
  note                     打印完整笔记(Markdown)
  tree                     以文件树展示对话轨迹（带 [n] 编号）
  goto <n|id前缀>          跳到对话树某节点，其根路径成为上下文
  stats                    查看本次/累计 token 用量与成本
  budget <n>              设置 token 预算(0=不限)，到上限自动中断
  save [file]              保存会话(默认 session.json)
  load <file>              加载会话
  export md|mindmap <file> 导出笔记为 Markdown / 思维导图
  papers                   列出已读论文
  concepts                 列出已学概念(跨论文)
  config show              查看配置
  config set <k> <v>       设置(如 llm.api_key / llm.model / llm.context_length)
  new                      新建会话
  exit                     退出";
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
                    println!("可设: llm.api_key llm.api_endpoint llm.model llm.context_length \
                              llm.thinking_mode llm.pdf_input \
                              pricing.input_price_per_1m pricing.output_price_per_1m \
                              budget.token_budget");
                    println!("常见端点：");
                    println!("  DeepSeek : https://api.deepseek.com/v1/chat/completions  model=deepseek-v4-pro");
                    println!("  OpenAI   : https://api.openai.com/v1/chat/completions    model=gpt-4o-mini");
                    println!("  本地Ollama: http://localhost:11434/v1/chat/completions    model=qwen2.5:7b");
                    return Ok(());
                }
                self.set_config(key, val)?;
                self.config.save()?;
                // api_key 脱敏回显，避免明文泄露
                let display = if key == "llm.api_key" { mask_key(val) } else { val.to_string() };
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
            _ => bail!("未知配置项: {key}（help 可看可设项）"),
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
        for (i, (b, depth)) in note.flatten().iter().enumerate() {
            let indent = "  ".repeat(*depth);
            let text: String = b.text.chars().take(60).collect();
            let nexpl = b.explanations.len();
            let expl = if nexpl > 0 { format!("  [{}条解释]", nexpl) } else { String::new() };
            println!("{:>3}. {}{} {} {}{}", i, indent, b.kind.tag(), text, "", expl);
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
        let path = if rest.trim().is_empty() { "session.json" } else { rest.trim() };
        self.session.save(Path::new(path))?;
        println!("会话已保存到 {path}");
        Ok(())
    }

    async fn cmd_load(&mut self, rest: &str) -> Result<()> {
        if rest.trim().is_empty() {
            bail!("用法: load <文件>");
        }
        self.session = Session::load(Path::new(rest.trim()))?;
        println!("已加载会话: 笔记={}, 对话节点={}",
            self.session.notes.is_some(),
            self.session.conversation.nodes.len());
        Ok(())
    }

    async fn cmd_export(&self, rest: &str) -> Result<()> {
        let (fmt, path) = split_cmd(rest);
        if path.is_empty() {
            bail!("用法: export <markdown|mindmap> <文件>");
        }
        let note = self.session.notes.as_ref().ok_or_else(|| anyhow!("还没有笔记"))?;
        let content = match fmt {
            "md" | "markdown" => export::to_markdown(note),
            "mindmap" | "mm" => export::to_mindmap(note),
            _ => bail!("未知格式: {fmt}（可用: markdown, mindmap）"),
        };
        std::fs::write(path, content)?;
        println!("已导出到 {path}");
        Ok(())
    }

    // ===== 以下为异步 LLM 相关命令（ingest / ask）=====

    async fn cmd_ingest(&mut self, rest: &str) -> Result<()> {
        let path = rest.trim();
        if path.is_empty() {
            bail!("用法: ingest <pdf路径>");
        }
        let p = Path::new(path);
        if !p.exists() {
            bail!("文件不存在: {path}");
        }
        interrupt::reset();

        // 1. 抽取纯文本（确定性，Rust 完成）
        let bar = ProgressBar::new_spinner();
        bar.set_style(spinner_style());
        bar.set_message("解析 PDF…");
        bar.enable_steady_tick(Duration::from_millis(100));
        let pages = pdf::extract_pages(p)?;
        let raw_text = pages.join("\n\n");
        bar.finish_and_clear();
        println!("{} PDF 已解析: {} 页, {} 字符", "✓".green().bold(), pages.len(), raw_text.chars().count());

        if interrupt::is_interrupted() {
            bail!("已打断");
        }

        // 2. 调用 LLM 生成结构化 Markdown 笔记
        let budget_ok = self.check_budget()?;
        if !budget_ok {
            bail!("已达 token 预算，无法继续。用 `budget <n>` 调整。");
        }
        let prompt = format!(
            "请阅读以下论文全文，生成一份**详细**的学习笔记 Markdown，遵循固定四段架构。\n\n\
             架构与分块规则：\n\
             - 第一行 `# 论文标题`。\n\
             - 用四个一级章节 `## 一、要解决的问题` `## 二、前人方案及其不足` `## 三、本文方案及其优点` `## 四、前景与发展方向`。\n\
             - 每个一级章节下，用 `###` 三级小标题细分。例如：\n\
               - 「二、前人方案」下，每个前人方案一个 `###` 小标题，说清做法与不足；\n\
               - 「三、本文方案」下，若论文提出多个方案/变体（如 5 种变体），**每个变体单独一个 `###` 小标题**，详细说明做法、公式、数据、直觉、优缺点；\n\
               - 「四、前景」下，每个方向一个 `###` 小标题。\n\
             - 每个 `###` 小标题下的内容（含多段落、公式、表格）合并为一块，不要为每句话单独成块。\n\
             - 关键公式用 `$$...$$` 包裹，直接写在所属段落里。\n\
             - 要详细：保留论文中的关键公式、数值结果、对比表格（用 markdown 表格）、算法步骤。不要泛泛概括，要展开具体内容。\n\
             - 不要输出额外说明，直接给 Markdown。\n\n\
             论文全文：\n{raw_text}"
        );
        let msgs = vec![
            Message { role: "system".into(), content: "你是论文笔记生成助手，只输出 Markdown。".into() },
            Message { role: "user".into(), content: prompt },
        ];
        let raw_clone = raw_text.clone();
        let mut first_token = true;
        let bar2 = ProgressBar::new_spinner();
        bar2.set_style(spinner_style());
        bar2.set_message("调用 LLM 生成笔记…");
        bar2.enable_steady_tick(Duration::from_millis(100));
        let res = llm::chat(&self.client, &self.config.llm, &msgs, false, self.config.llm.thinking_mode, &mut |t| {
            if first_token {
                bar2.finish_and_clear();
                first_token = false;
                print!("\n(LLM 输出中) ");
                let _ = io::stdout().flush();
            }
            print!("{t}");
            let _ = io::stdout().flush();
        }).await;
        println!();
        let res = res?;
        bar2.finish_and_clear();

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
            path: path.to_string(),
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
            input_tokens: res.input_tokens,
            output_tokens: res.output_tokens,
            cost: res.input_tokens as f64 * self.config.pricing.input_price_per_1m / 1_000_000.0
                + res.output_tokens as f64 * self.config.pricing.output_price_per_1m / 1_000_000.0,
            created_at: Utc::now().to_rfc3339(),
            label: format!("导入《{}》", title),
        });
        self.session.conversation.current = Some(root_id);
        Ok(())
    }

    async fn cmd_ask(&mut self, args: &str) -> Result<()> {
        let question = args.trim();
        if question.is_empty() {
            bail!("用法: ask <你的问题>");
        }
        if self.session.notes.is_none() {
            bail!("还没有笔记，先 `ingest <pdf>`");
        }
        interrupt::reset();

        let budget_ok = self.check_budget()?;
        if !budget_ok {
            bail!("已达 token 预算，自动中断。用 `budget <n>` 调整。");
        }

        // 1. 取出笔记原文 + 定位最相关 block（确定性 Rust 逻辑）
        let (raw_text, notes_md, block_id) = {
            let note = self.session.notes.as_ref().unwrap();
            let blk = note.locate(question);
            let bid = blk.map(|b| b.id.clone());
            (note.raw_text.clone(), note.to_markdown(), bid)
        };

        // 2. 拼对话上下文：根路径上的所有 Q&A
        let path: Vec<(String, String)> = self
            .session
            .conversation
            .path_to_current()
            .iter()
            .map(|n| (n.question.clone(), n.answer.clone()))
            .collect();

        // 3. 上下文长度检查：超长则丢弃最早的对话轮
        let ctx = self.config.llm.context_length;
        let base_tokens = (raw_text.chars().count() + notes_md.chars().count()) / 4;
        if base_tokens > ctx {
            eprintln!("{} 论文+笔记约 {} token，超过模型上下文 {}，可能报错。建议换更大上下文的模型。", "⚠️ ".yellow(), base_tokens, ctx);
        }
        let mut kept = path.clone();
        let mut used = 0usize;
        let avail = ctx.saturating_sub(base_tokens + question.chars().count() / 4 + 200);
        let mut drop_idx = 0;
        while drop_idx < kept.len() {
            let (q, a) = &kept[drop_idx];
            let t = (q.chars().count() + a.chars().count()) / 4;
            if used + t > avail {
                break;
            }
            used += t;
            drop_idx += 1;
        }
        let dropped = drop_idx;
        let kept_pairs: Vec<(String, String)> = kept.drain(drop_idx..).collect();
        if dropped > 0 {
            eprintln!("{}（上下文偏长，已省略最早 {} 轮对话）", "".dimmed(), dropped);
        }

        let mut msgs = vec![
            Message { role: "system".into(), content: SYS_ASK.into() },
            Message { role: "user".into(), content: format!("【论文全文】\n{raw_text}") },
            Message { role: "assistant".into(), content: format!("【已生成笔记】\n{notes_md}") },
        ];
        for (q, a) in &kept_pairs {
            msgs.push(Message { role: "user".into(), content: q.clone() });
            msgs.push(Message { role: "assistant".into(), content: a.clone() });
        }

        // 4. 跨论文已学概念注入
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

        // 7. 把解释插入笔记对应 block
        if let Some(bid) = &block_id {
            if let Some(note) = self.session.notes.as_mut() {
                if let Some(b) = note.find_block_mut(bid) {
                    b.explanations.push(Explanation {
                        id: uuid::Uuid::new_v4().to_string(),
                        question: question.to_string(),
                        answer: res.content.clone(),
                        concept: derive_concept(question),
                        created_at: Utc::now().to_rfc3339(),
                    });
                }
            }
        }

        // 8. 记录对话树节点（当前节点为父）
        let parent = self.session.conversation.current.clone();
        let node_id = uuid::Uuid::new_v4().to_string();
        self.session.conversation.add_exchange(ConvNode {
            id: node_id.clone(),
            parent,
            question: question.to_string(),
            answer: res.content.clone(),
            block_id: block_id.clone(),
            input_tokens: res.input_tokens,
            output_tokens: res.output_tokens,
            cost: res.input_tokens as f64 * self.config.pricing.input_price_per_1m / 1_000_000.0
                + res.output_tokens as f64 * self.config.pricing.output_price_per_1m / 1_000_000.0,
            created_at: Utc::now().to_rfc3339(),
            label: derive_concept(question),
        });
        self.session.conversation.current = Some(node_id);

        // 9. 加入知识库概念
        let (pid, ptitle) = self
            .session
            .current_paper_id
            .clone()
            .and_then(|id| self.kb.papers.iter().find(|p| p.id == id).map(|p| (id, p.title.clone())))
            .unwrap_or_default();
        self.kb.add_concept(Concept {
            name: derive_concept(question),
            definition: res.content.chars().take(200).collect(),
            paper_id: pid,
            paper_title: ptitle,
            block_id,
            created_at: Utc::now().to_rfc3339(),
        });
        self.kb.save()?;

        if res.estimated {
            println!("[注: 本次 token 数为估算]");
        }
        Ok(())
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
}

// ===== 辅助函数 =====

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
fn derive_concept(q: &str) -> String {
    q.chars().take(20).collect()
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

