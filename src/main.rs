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
        // -s <会话名|ID|序号> : 恢复指定会话
        // 匹配优先级：会话名精确匹配（含时间戳名）→ 文件 ID → 列表序号(1-based)
        if args.len() == 2 && args[0] == "-s" {
            let sid = &args[1];
            let sessions = paths::list_sessions();

            // ① 按会话名精确匹配（时间戳 fallback 名、自定义 ASCII 名可直接用）
            let mut target: Option<(u64, String)> = sessions
                .iter()
                .find(|&&id| session_name_of(id) == *sid)
                .map(|&id| (id, sid.clone()));

            // ② 纯数字：按文件 ID（id.json 存在）
            if target.is_none() && sid.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(id) = sid.parse::<u64>() {
                    if paths::session_path(id).exists() {
                        target = Some((id, sid.clone()));
                    }
                }
            }

            // ③ 纯数字：按列表序号（1-based，向后兼容）
            if target.is_none() && sid.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(n) = sid.parse::<usize>() {
                    if let Some(&id) = n.checked_sub(1).and_then(|i| sessions.get(i)) {
                        target = Some((id, sid.clone()));
                    }
                }
            }

            match target {
                Some((id, _)) => {
                    let path = paths::session_path(id);
                    app.session = session::Session::load(&path)?;
                    let name = app.session.session_name.clone();
                    println!("已恢复会话 {id}：{}", if name.is_empty() { "（未命名）".into() } else { name });
                    return app.repl().await;
                }
                None => {
                    eprintln!("会话 {sid} 不存在。可用会话（ID  会话名）：");
                    if sessions.is_empty() {
                        eprintln!("  （无已保存会话）");
                    } else {
                        for s in &sessions {
                            let name = session_name_of(*s);
                            eprintln!("  {s}  {name}");
                        }
                    }
                    return Err(anyhow!("会话不存在"));
                }
            }
        }
        // -l : 列出所有会话
        if args.len() == 1 && (args[0] == "-l" || args[0] == "--list") {
            let sessions = paths::list_sessions();
            if sessions.is_empty() {
                println!("（无已保存会话）");
            } else {
                println!("已保存的会话：");
                println!("{:>4}  {:<30}  {}", "ID", "会话名", "保存时间");
                for id in &sessions {
                    let path = paths::session_path(*id);
                    let name = session_name_of(*id);
                    let time = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| {
                            t.duration_since(std::time::UNIX_EPOCH)
                                .ok()
                                .map(|d| {
                                    let dt = chrono::DateTime::from_timestamp(d.as_secs() as i64, 0).unwrap_or_default();
                                    dt.format("%Y-%m-%d %H:%M").to_string()
                                })
                        })
                        .unwrap_or_else(|| "未知".to_string());
                    println!("{:>4}  {:<30}  {}", id, name, time);
                }
                println!("用 paperhelper -s <ID> 恢复（如 paperhelper -s 1）");
            }
            return Ok(());
        }
        // 其他参数：当单次命令执行
        app.run_command(&args.join(" ")).await?;
        return Ok(());
    }
    app.repl().await
}

/// 读取会话文件里的 session_name 字段（不完整反序列化，只取名字）。
fn session_name_of(id: u64) -> String {
    let path = paths::session_path(id);
    let Ok(s) = std::fs::read_to_string(&path) else {
        return "（读取失败）".into();
    };
    // 简单提取 "session_name":"xxx"
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
