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

use anyhow::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    interrupt::install();
    let config = config::Config::load()?;
    let kb = knowledge::KnowledgeBase::load()?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?;
    let mut app = app::App::new(config, kb, client);

    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        app.run_command(&args.join(" ")).await?;
        return Ok(());
    }
    app.repl().await
}
