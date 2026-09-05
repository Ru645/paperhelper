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
        // -s <编号> : 恢复指定会话
        if args.len() == 2 && args[0] == "-s" {
            let sid = &args[1];
            let path = paths::session_path(sid);
            if !path.exists() {
                eprintln!("会话 {sid} 不存在。可用会话：");
                let sessions = paths::list_sessions();
                if sessions.is_empty() {
                    eprintln!("  （无已保存会话）");
                } else {
                    for s in &sessions {
                        eprintln!("  {s}");
                    }
                }
                return Err(anyhow!("会话不存在"));
            }
            app.session = session::Session::load(&path)?;
            println!("已恢复会话 {sid}");
            return app.repl().await;
        }
        // -l : 列出所有会话
        if args.len() == 1 && (args[0] == "-l" || args[0] == "--list") {
            let sessions = paths::list_sessions();
            if sessions.is_empty() {
                println!("（无已保存会话）");
            } else {
                println!("已保存的会话：");
                println!("{:>4}  {:<30}  {}", "序号", "会话名", "保存时间");
                for (i, s) in sessions.iter().enumerate() {
                    let path = paths::session_path(s);
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
                    println!("{:>4}  {:<30}  {}", i + 1, s, time);
                }
                println!("用 paperhelper -s <会话名> 恢复");
            }
            return Ok(());
        }
        // 其他参数：当单次命令执行
        app.run_command(&args.join(" ")).await?;
        return Ok(());
    }
    app.repl().await
}
