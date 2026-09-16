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

use std::io::IsTerminal;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use tokio::sync::mpsc::UnboundedSender;

/// 去掉 ANSI 转义序列（颜色码、OSC 超链接等）。
///
/// Web 控制台、日志文件、重定向输出都不认颜色码，若不剥离就会显示成
/// `[1m[32m✓[39m[0m` 这类乱码。
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            // CSI：ESC [ 参数 中间字节 终止字节（@-~）
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC：ESC ] ... BEL 或 ST（ESC \）
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        let _ = chars.next();
                        break;
                    }
                }
            }
            // 其它两字节转义序列：ESC X
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// 当前输出端是否应保留颜色（`NO_COLOR` 或非终端时为否）。
fn color_enabled(stderr: bool) -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if stderr {
        std::io::stderr().is_terminal()
    } else {
        std::io::stdout().is_terminal()
    }
}

/// 一条输出事件（Web 端转 SSE 时按类型映射成不同 event 名）。
#[derive(Debug, Clone)]
pub enum Event {
    /// 一整行普通输出（对应 println!）
    Stdout(String),
    /// 一整行错误/警告（对应 eprintln!）
    Stderr(String),
    /// LLM 流式 token（无换行，需前端累加）
    Token(String),
    /// 已生成的总字数（Web 导入等场景：只关心进度，不逐字外显）
    Chars(u64),
    /// LLM 流式思考过程（reasoning 模型才可能有；用于界面展示进度）
    Reasoning(String),
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
        // 通道模式（Web）：浏览器控制台不渲染 ANSI，必须剥离
        if self.send(Event::Stdout(strip_ansi(&s))) {
            return;
        }
        if color_enabled(false) {
            println!("{s}");
        } else {
            println!("{}", strip_ansi(&s));
        }
    }

    /// 输出一行错误/警告（对应 eprintln!）。
    pub fn stderr(&self, s: impl Into<String>) {
        let s = s.into();
        if self.send(Event::Stderr(strip_ansi(&s))) {
            return;
        }
        if color_enabled(true) {
            eprintln!("{s}");
        } else {
            eprintln!("{}", strip_ansi(&s));
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

    /// 输出 LLM 的思考过程分片：Web 端转 SSE 供界面展示；
    /// 终端下用暗色写到 stderr（避免和正文混在一起）。
    pub fn reasoning(&self, t: &str) {
        if !self.send(Event::Reasoning(t.to_string())) {
            use std::io::Write;
            if color_enabled(true) {
                eprint!("\x1b[2m{t}\x1b[0m");
            } else {
                eprint!("{t}");
            }
            let _ = std::io::stderr().flush();
        }
    }

    /// 报告「已生成 N 字」（Web 端顶部进度用；终端无操作，因为 CLI 直接打印 token）。
    pub fn chars(&self, n: u64) {
        self.send(Event::Chars(n));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_color_codes_but_keeps_text() {
        // 典型场景："✓ 文本已就绪" 被 owo-colors 着色后的字节序列
        let colored = "\x1b[1m\x1b[32m✓\x1b[39m\x1b[0m 文本已就绪: 12 字符";
        assert_eq!(strip_ansi(colored), "✓ 文本已就绪: 12 字符");
    }

    #[test]
    fn strip_ansi_handles_osc_and_plain_text() {
        let s = "正常中文abc\x1b]8;;https://example.com\x07链接\x1b]8;;\x07";
        assert_eq!(strip_ansi(s), "正常中文abc链接");
        assert_eq!(strip_ansi("没有转义码"), "没有转义码");
    }

    #[test]
    fn channel_emitter_strips_ansi_from_stdout_and_stderr() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let e = Emitter::channel(tx);
        e.stdout("\x1b[32mOK\x1b[0m");
        e.stderr("\x1b[31m坏消息\x1b[0m");
        match rx.try_recv().unwrap() {
            Event::Stdout(s) => assert_eq!(s, "OK"),
            other => panic!("期望 Stdout，得到 {other:?}"),
        }
        match rx.try_recv().unwrap() {
            Event::Stderr(s) => assert_eq!(s, "坏消息"),
            other => panic!("期望 Stderr，得到 {other:?}"),
        }
    }
}
