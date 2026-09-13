//! Ctrl-C 全局打断机制。
//!
//! 实现方式：tokio 任务常驻监听 SIGINT，收到后置位全局 `AtomicBool`；
//! 长任务（LLM 流式读取、Python 子进程前后）轮询 `is_interrupted()` 自行中止，
//! REPL 每条命令开始时 `reset()`。这样 Ctrl-C 只打断当前任务而不退出程序。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use tokio::sync::Notify;

static FLAG: AtomicBool = AtomicBool::new(false);
static NOTIFY: OnceLock<Notify> = OnceLock::new();

pub fn notify() -> &'static Notify {
    NOTIFY.get_or_init(Notify::new)
}

pub fn is_interrupted() -> bool {
    FLAG.load(Ordering::Relaxed)
}

pub fn reset() {
    FLAG.store(false, Ordering::Relaxed);
}

/// 从外部请求打断当前任务（Web 的停止按钮调用）。
/// 与 Ctrl-C 等价：置位标志并唤醒等待者；长任务轮询到后自行中止。
pub fn request() {
    if !FLAG.swap(true, Ordering::Relaxed) {
        notify().notify_waiters();
    }
}

/// 等待一次打断信号，供 `tokio::select!` 与长任务竞争。
///
/// 用 `Notified::enable()` 先注册再复查标志，避免「置位发生在注册之前」
/// 导致永久等待的竞态。
pub async fn wait() {
    if is_interrupted() {
        return;
    }
    let notified = notify().notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    if is_interrupted() {
        return;
    }
    notified.await;
}

pub fn install() {
    tokio::spawn(async {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                break;
            }
            if !FLAG.swap(true, Ordering::Relaxed) {
                eprintln!("\n[收到 Ctrl-C，正在停止当前任务… 输入 exit 退出程序]");
                notify().notify_waiters();
            }
        }
    });
}
