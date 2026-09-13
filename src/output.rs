//! 输出抽象层：让同一套业务逻辑既能输出到终端（CLI），也能输出到 SSE（Web）。
//!
//! `Emitter` 有两个后端：
//! - **终端**（`Emitter::terminal()`，默认）：`stdout/stderr` 走 `println!/eprintln!`，
//!   `progress` 用 indicatif spinner，`token` 直接打印并 flush——CLI 行为与改造前一致。
//! - **通道**（`Emitter::channel(tx)`）：所有输出打包成 `Event` 发给 mpsc，
//!   由 web 层转成 SSE 事件推给浏览器。
//!
//! 业务代码只调用 `emitter.stdout(...)` / `.token(...)` / `.progress(...)`，
//! 不关心自己在终端还是 Web 里跑。`is_terminal()` 用于需要交互式 stdin 的分支
//! （Web 下不能读 stdin，改用默认值）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use tokio::sync::mpsc::UnboundedSender;

/// 一条输出事件（Web 端转 SSE 时按类型映射成不同 event 名）。
#[derive(Debug, Clone)]
pub enum Event {
    /// 一整行普通输出（对应 println!）
    Stdout(String),
    /// 一整行错误/警告（对应 eprintln!）
    Stderr(String),
    /// LLM 流式 token（无换行，需前端累加）
    Token(String),
    /// 进度提示开始/更新（对应 spinner 的 set_message）
    Progress(String),
    /// 进度结束（对应 spinner 的 finish_and_clear）
    ProgressDone,
    /// 命令正常结束
    Done,
    /// 命令被用户中止（Ctrl-C / Web 停止按钮）
    Aborted,
    /// 命令出错：`summary` 给用户看的中文摘要，`detail` 完整错误链/原始响应
    Error { summary: String, detail: String },
}

/// 可插拔输出器（克隆代价极小：Option<Sender> + Arc）。
#[derive(Clone)]
pub struct Emitter {
    tx: Option<UnboundedSender<Event>>,
    /// 终端模式下复用的 spinner 句柄（跨多次 progress 调用保持同一个）
    bar: Arc<Mutex<Option<ProgressBar>>>,
}

impl Default for Emitter {
    fn default() -> Self {
        Self {
            tx: None,
            bar: Arc::new(Mutex::new(None)),
        }
    }
}

impl Emitter {
    /// 终端后端（CLI 默认）。
    pub fn terminal() -> Self {
        Self::default()
    }

    /// 通道后端（Web 使用），输出全部转成 `Event`。
    pub fn channel(tx: UnboundedSender<Event>) -> Self {
        Self {
            tx: Some(tx),
            bar: Arc::new(Mutex::new(None)),
        }
    }

    /// 是否终端模式。用于判断能否进行交互式 stdin 读取。
    pub fn is_terminal(&self) -> bool {
        self.tx.is_none()
    }

    fn send(&self, e: Event) -> bool {
        match &self.tx {
            Some(tx) => {
                let _ = tx.send(e);
                true
            }
            None => false,
        }
    }

    /// 输出一行（对应 println!）。
    pub fn stdout(&self, s: impl Into<String>) {
        let s = s.into();
        if !self.send(Event::Stdout(s.clone())) {
            println!("{s}");
        }
    }

    /// 输出一行错误/警告（对应 eprintln!）。
    pub fn stderr(&self, s: impl Into<String>) {
        let s = s.into();
        if !self.send(Event::Stderr(s.clone())) {
            eprintln!("{s}");
        }
    }

    /// 输出 LLM 流式 token（无换行）。
    pub fn token(&self, t: &str) {
        if !self.send(Event::Token(t.to_string())) {
            use std::io::Write;
            print!("{t}");
            let _ = std::io::stdout().flush();
        }
    }

    /// 开始/更新一个进度提示。终端下复用同一个 spinner。
    pub fn progress(&self, msg: &str) {
        if self.tx.is_some() {
            self.send(Event::Progress(msg.to_string()));
            return;
        }
        let mut g = self.bar.lock().unwrap();
        if g.is_none() {
            let bar = ProgressBar::new_spinner();
            bar.set_style(spinner_style());
            bar.enable_steady_tick(Duration::from_millis(100));
            *g = Some(bar);
        }
        if let Some(b) = g.as_ref() {
            b.set_message(msg.to_string());
        }
    }

    /// 结束当前进度提示。
    pub fn progress_done(&self) {
        if self.tx.is_some() {
            self.send(Event::ProgressDone);
            return;
        }
        let mut g = self.bar.lock().unwrap();
        if let Some(b) = g.take() {
            b.finish_and_clear();
        }
    }

    /// 命令正常结束（Web 端据此关闭 SSE）。
    pub fn done(&self) {
        self.send(Event::Done);
    }

    /// 命令被用户中止。
    pub fn aborted(&self) {
        self.send(Event::Aborted);
    }

    /// 命令出错：摘要给用户看，详情给「查看详情」/日志。
    pub fn error(&self, summary: impl Into<String>, detail: impl Into<String>) {
        self.send(Event::Error {
            summary: summary.into(),
            detail: detail.into(),
        });
    }
}

/// indicatif spinner 样式（终端进度用）。
pub fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner} {msg}")
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
}
