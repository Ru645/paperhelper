//! 程序入口与命令行参数处理。
//!
//! 职责：
//! - 组装依赖（配置、知识库、HTTP 客户端）并启动 REPL（`app::App`）
//! - 解析三类命令行参数：
//!   - `--completions`：输出 bash 补全脚本（`-s` 后补全会话编号）
//!   - `-s <编号>`：按会话文件名精确/唯一前缀匹配，恢复会话后进入 REPL
//!   - `-l`：列出所有会话（编号+标题）
//! - 其余参数按单次 REPL 命令执行（便于脚本化调用）

mod app;
mod config;
mod conversation;
mod export;
mod interrupt;
mod knowledge;
mod llm;
mod logging;
mod notes;
mod output;
mod paths;
mod pdf;
mod presets;
mod prompts;
mod session;
mod transfer;
mod web;

use anyhow::{anyhow, Result};

const BASH_COMPLETION: &str = r#"_paperhelper() {
  local cur prev
  COMPREPLY=()
  cur="${COMP_WORDS[COMP_CWORD]}"
  prev="${COMP_WORDS[COMP_CWORD-1]}"
  if [[ "$prev" == "-s" ]]; then
    # 枚举当前目录 .paperhelper/sessions/ 下的会话标识（时间戳）
    local sess
    sess=$(ls .paperhelper/sessions/*.json 2>/dev/null | xargs -r -n1 basename | sed 's/\.json$//')
    COMPREPLY=( $(compgen -W "$sess" -- "$cur") )
  else
    COMPREPLY=( $(compgen -W "-s -l --completions" -- "$cur") )
  fi
}
complete -F _paperhelper paperhelper
"#;

#[tokio::main]
async fn main() -> Result<()> {
    logging::init();
    let config = config::Config::load()?;
    let kb = knowledge::KnowledgeBase::load()?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?;
    let mut app = app::App::new(config, kb, client);

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    logging::info(format!(
        "PaperHelper v{} 启动 | cwd={} | 数据目录={} | 日志={}",
        env!("CARGO_PKG_VERSION"),
        cwd,
        paths::data_dir().display(),
        logging::log_path().display(),
    ));
    logging::info(format!(
        "LLM 配置：endpoint={} | model={} | key={} | context={} | thinking={} | budget={}",
        app.config.llm.api_endpoint,
        app.config.llm.model,
        app::mask_key(&app.config.llm.api_key),
        app.config.llm.context_length,
        app.config.llm.thinking_mode,
        app.config.budget.token_budget,
    ));

    // 解析命令行参数
    let args: Vec<String> = std::env::args().skip(1).collect();

    // web 子命令：paperhelper web [--port N] [--open] [--port-file <路径>]（默认 8080，
    // 仅监听本机；端口被占自动顺延；--open 启动后自动打开默认浏览器；
    // --port-file 把实际端口写入文件，供 Windows 桌面壳握手，端口传 0 时由系统随机分配）
    if !args.is_empty() && args[0] == "web" {
        let mut port = 8080u16;
        let mut open = false;
        let mut port_file: Option<std::path::PathBuf> = None;
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--port" if i + 1 < args.len() => {
                    port = args[i + 1].parse().unwrap_or(8080);
                    i += 2;
                }
                "--port-file" if i + 1 < args.len() => {
                    port_file = Some(std::path::PathBuf::from(&args[i + 1]));
                    i += 2;
                }
                "--open" => {
                    open = true;
                    i += 1;
                }
                other => {
                    if let Some(p) = other.strip_prefix("--port=") {
                        port = p.parse().unwrap_or(8080);
                    } else if let Some(p) = other.strip_prefix("--port-file=") {
                        port_file = Some(std::path::PathBuf::from(p));
                    }
                    i += 1;
                }
            }
        }
        return web::serve(app, port, open, port_file).await;
    }

    // 仅 CLI 路径安装 REPL 打断器（web 模式自行处理 Ctrl-C 退出，见 web::serve）。
    // 放在这里是为了让 web 模式不注册该 SIGINT 处理器，从而保留 Ctrl-C 终止进程的能力。
    interrupt::install();

    if !args.is_empty() {
        // --completions : 输出 bash 补全脚本
        if args.len() == 1 && args[0] == "--completions" {
            print!("{BASH_COMPLETION}");
            return Ok(());
        }
        // -s <时间戳> : 恢复指定会话（按会话文件名精确匹配，或唯一前缀）
        if args.len() == 2 && args[0] == "-s" {
            let key = &args[1];
            let sessions = paths::list_sessions();
            // 精确匹配
            let mut target = sessions.iter().find(|s| *s == key).cloned();
            // 唯一前缀匹配（配合 Tab 补全，输前几位即可）
            if target.is_none() {
                let prefix_hits: Vec<String> = sessions
                    .iter()
                    .filter(|s| s.starts_with(key.as_str()))
                    .cloned()
                    .collect();
                if prefix_hits.len() == 1 {
                    target = Some(prefix_hits[0].clone());
                } else if prefix_hits.len() > 1 {
                    eprintln!("前缀 {key} 匹配到多个会话：");
                    for s in &prefix_hits {
                        eprintln!("  {s}  {}", session_name_of(s));
                    }
                    return Err(anyhow!("前缀不唯一，请补全更多位"));
                }
            }
            match target {
                Some(id) => {
                    let path = paths::session_path(&id);
                    app.session = session::Session::load(&path)?;
                    app.export_path = app.session.export_path.clone();
                    let name = app.session.session_name.clone();
                    println!("已恢复会话 {id}：{}", if name.is_empty() { "（未命名）".into() } else { name });
                    return app.repl().await;
                }
                None => {
                    eprintln!("会话 {key} 不存在。可用会话（标识  会话名）：");
                    if sessions.is_empty() {
                        eprintln!("  （无已保存会话）");
                    } else {
                        for s in &sessions {
                            eprintln!("  {s}  {}", session_name_of(s));
                        }
                    }
                    return Err(anyhow!("会话不存在"));
                }
            }
        }
        // -l : 列出所有会话（编号 + 标题）
        if args.len() == 1 && (args[0] == "-l" || args[0] == "--list") {
            let sessions = paths::list_sessions();
            if sessions.is_empty() {
                println!("（无已保存会话）");
            } else {
                println!("{:<18}  {}", "编号", "标题");
                for id in &sessions {
                    println!("{:<18}  {}", id, session_name_of(id));
                }
                println!("恢复：paperhelper -s <编号>（支持唯一前缀与 Tab 补全）");
            }
            return Ok(());
        }
        // 其他参数：当单次命令执行
        app.run_command(&args.join(" ")).await?;
        return Ok(());
    }
    app.repl().await
}

/// 读取会话文件里的 session_name 字段。
/// 实现方式：不做完整 JSON 反序列化（会话文件可能很大），而是字符串查找
/// `"session_name"` 键后取下一个 JSON 字符串字面量，轻量且够用。
pub(crate) fn session_name_of(id: &str) -> String {
    let path = paths::session_path(id);
    let Ok(s) = std::fs::read_to_string(&path) else {
        return "（读取失败）".into();
    };
    if let Some(pos) = s.find("\"session_name\"") {
        let rest = &s[pos..];
        if let Some(colon) = rest.find(':') {
            let rest = &rest[colon + 1..];
            let rest = rest.trim_start();
            if rest.starts_with('"') {
                let rest = &rest[1..];
                if let Some(end) = rest.find('"') {
                    return rest[..end].to_string();
                }
            }
        }
    }
    "（未命名）".into()
}
