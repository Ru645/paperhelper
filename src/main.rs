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
mod notes;
mod paths;
mod pdf;
mod prompts;
mod session;

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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    interrupt::install();
    let config = config::Config::load()?;
    let kb = knowledge::KnowledgeBase::load()?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?;
    let mut app = app::App::new(config, kb, client);

    // 解析命令行参数
    let args: Vec<String> = std::env::args().skip(1).collect();
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
fn session_name_of(id: &str) -> String {
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
