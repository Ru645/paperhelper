//! Web 界面后端：用 axum 把现有 `App` 的能力暴露成 HTTP + SSE。
//!
//! 设计要点：
//! - **单用户单会话**：全局 `Arc<tokio::sync::Mutex<App>>`。命令在锁内串行执行，
//!   与 CLI 行为一致。前端页面由 `include_str!` 内嵌，单二进制自包含。
//! - **实时输出**：`/api/run` 为每条命令新建一个 mpsc 通道，把 `App.emitter`
//!   切成 `Emitter::channel(tx)`，命令执行期间的 token/进度/文本全部转成 SSE 事件。
//! - **可打断**：`/api/interrupt` 直接调 `interrupt::request()`（全局 atomic），
//!   **不抢 App 锁**，因此任务运行中也能立即打断。
//! - **安全**：`/api/state`、`/api/config` 只回传 key 是否设置与掩码，绝不返回明文。
//! - **笔记渲染**：`/api/note?format=html` 复用 `export::to_html`（KaTeX + 对话树），
//!   前端用 iframe 展示；`format=md` 返回 Markdown 源。

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::{DefaultBodyLimit, Extension, Multipart, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::app::App;
use crate::interrupt;
use crate::output::{Emitter, Event as OutEvent};
use crate::{export, knowledge, llm, logging, notes, paths, pdf, session, transfer, update};

type SharedApp = Arc<Mutex<App>>;

/// 构建路由（抽成独立函数便于测试）。
pub fn router(app: SharedApp) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/style.css", get(stylesheet))
        .route("/app.js", get(script))
        .route("/pdf-viewer.js", get(pdf_viewer_script))
        .route("/api/run", post(api_run))
        .route("/api/interrupt", post(api_interrupt))
        .route("/api/state", get(api_state))
        .route("/api/deps", get(api_deps))
        .route("/api/deps/install", post(api_deps_install))
        .route("/api/shutdown", post(api_shutdown))
        .route("/api/open-data-dir", post(api_open_data_dir))
        .route("/api/presets", get(api_presets))
        .route("/vendor/*path", get(api_vendor))
        .route("/api/note", get(api_note))
        .route("/api/styles", get(api_styles))
        .route("/api/styles/save", post(api_style_save))
        .route("/api/styles/delete", post(api_style_delete))
        .route("/api/styles/reset", post(api_style_reset))
        .route("/api/note/block", get(api_note_block))
        .route("/api/note/edit", post(api_note_edit))
        .route("/api/note/rewrite", post(api_note_rewrite))
        .route("/api/note/add", post(api_note_add))
        .route("/api/note/delete", post(api_note_delete))
        .route("/api/note/ai", post(api_note_ai))
        .route("/api/note/restyle", post(api_note_restyle))
        .route("/api/note/restyle/apply", post(api_note_restyle_apply))
        .route("/api/export", get(api_export))
        .route("/api/config", get(api_config_get).post(api_config_set))
        .route("/api/config/test", post(api_config_test))
        .route("/api/update/check", get(api_update_check))
        .route("/api/update/skip", post(api_update_skip))
        .route("/api/update/status", get(api_update_status))
        .route("/api/update/download", post(api_update_download))
        .route("/api/update/apply", post(api_update_apply))
        .route("/api/sessions", get(api_sessions))
        .route("/api/sessions/load", post(api_session_load))
        .route("/api/sessions/save", post(api_session_save))
        .route("/api/sessions/pin", post(api_session_pin))
        .route("/api/sessions/rename", post(api_session_rename))
        .route("/api/sessions/delete", post(api_session_delete))
        .route("/api/sessions/export", get(api_sessions_export))
        .route(
            "/api/sessions/import",
            post(api_sessions_import)
                .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES as usize + 1024 * 1024)),
        )
        .route("/api/concept", get(api_concept))
        .route("/api/paper", get(api_paper))
        .route("/api/paper/note", get(api_paper_note))
        .route("/api/kb/paper/pin", post(api_paper_pin))
        .route("/api/kb/paper/delete", post(api_paper_delete))
        .route("/api/kb/concept/pin", post(api_concept_pin))
        .route("/api/kb/concept/delete", post(api_concept_delete))
        .route(
            "/api/annotate",
            post(api_annotate).layer(DefaultBodyLimit::max(MAX_IMAGE_BODY_BYTES as usize)),
        )
        .route("/api/annotate/answer", post(api_annotate_answer))
        .route("/api/annotate/reply", post(api_annotate_reply))
        .route("/api/annotate/delete", post(api_annotation_delete))
        .route("/api/annotations", get(api_annotations))
        .route(
            "/api/upload",
            post(api_upload).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES as usize + 1024 * 1024)),
        )
        .route("/api/samples", get(api_samples))
        .route("/api/samples/import", post(api_sample_import))
        .route("/api/pdf/file", get(api_pdf_file))
        .layer(middleware::from_fn(log_requests))
        .with_state(app)
}

/// 上传大小上限（200MB，与前端提示一致）。
const MAX_UPLOAD_BYTES: u64 = 200 * 1024 * 1024;

/// 批注请求体上限（PDF 页渲染图以 data URL 随请求发送，可能达数 MB）。
const MAX_IMAGE_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// serde 默认值：字段缺省时按 true（保持旧前端/旧会话的原有行为）。
fn default_true() -> bool {
    true
}

/// 全局 LLM 门：同一时刻只允许一个 LLM 流式任务（导入/提问/重写…）。
/// 不持 App 锁即可抢，因此**非 LLM 请求不会被 LLM 任务阻塞**。
static LLM_GATE: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// 抢 LLM 门失败时的统一文案。
const LLM_BUSY_MSG: &str = "已有 LLM 任务在运行（如导入或提问），请稍候或先点「停止」";

/// 不持 App 锁时把结果发回 SSE（prepare 失败等场景）。
fn emit_result_channel(
    tx: &mpsc::UnboundedSender<OutEvent>,
    result: anyhow::Result<()>,
    what: &str,
    t0: std::time::Instant,
) {
    let emitter = Emitter::channel(tx.clone());
    emit_result(&emitter, result, what, t0);
}

/// 每个请求记一条日志：方法、路径、状态码、耗时。
async fn log_requests(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let t0 = std::time::Instant::now();
    let resp = next.run(req).await;
    logging::info(format!(
        "HTTP {} {} → {}（{:.0}ms）",
        method,
        path,
        resp.status().as_u16(),
        t0.elapsed().as_millis()
    ));
    resp
}

/// 本机 Web 服务的优雅退出句柄：`/api/shutdown` 触发（桌面壳关窗也用它替代 Ctrl-C）。
#[derive(Default)]
pub struct ShutdownHandle {
    notify: tokio::sync::Notify,
}

impl ShutdownHandle {
    /// 请求退出（可在任意任务里调用）。
    pub fn trigger(&self) {
        self.notify.notify_one();
    }

    /// 等待退出请求（`with_graceful_shutdown` 用）。
    pub async fn wait(&self) {
        self.notify.notified().await;
    }
}

/// 绑定监听端口：从 `start` 起最多顺延 `tries` 个端口（被占则 +1），
/// 返回（监听器, 实际地址, 请求的起始端口）。`start=0` 时由系统分配随机端口。
pub async fn bind_with_fallback(
    start: u16,
    tries: u16,
) -> Result<(tokio::net::TcpListener, SocketAddr, u16)> {
    let mut last_err: Option<(SocketAddr, std::io::Error)> = None;
    for offset in 0..tries.max(1) {
        let addr = SocketAddr::from(([127, 0, 0, 1], start.saturating_add(offset)));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => {
                let actual = l.local_addr().unwrap_or(addr);
                return Ok((l, actual, start));
            }
            Err(e) => last_err = Some((addr, e)),
        }
    }
    let (addr, e) = last_err.ok_or_else(|| anyhow::anyhow!("无法绑定监听端口"))?;
    Err(anyhow::anyhow!(
        "端口 {start} 起连续 {tries} 个都被占用（最后一次尝试 {addr}：{e}），可用 --port 指定其他端口"
    ))
}

/// 打开系统默认浏览器（失败只提示，不影响服务）。
pub fn open_browser(url: &str) {
    let (prog, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start", "", url])
    } else if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else {
        ("xdg-open", vec![url])
    };
    match std::process::Command::new(prog)
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => logging::info(format!("已请求打开浏览器：{url}")),
        Err(e) => {
            logging::warn(format!("无法自动打开浏览器（{prog}: {e}）；请手动访问 {url}"));
            println!("无法自动打开浏览器，请手动访问 {url}");
        }
    }
}

/// 启动 Web 服务（仅监听 127.0.0.1，本机使用）。
/// `open=true` 时启动后自动打开浏览器；端口被占自动顺延（8080→8090）。
/// `port_file` 非空时，绑定成功后把实际端口写入该文件（供桌面壳握手，退出时删除）。
pub async fn serve(
    app: App,
    port: u16,
    open: bool,
    port_file: Option<std::path::PathBuf>,
) -> Result<()> {
    let shared: SharedApp = Arc::new(Mutex::new(app));
    let (listener, addr, requested) = bind_with_fallback(port, 10).await?;
    if requested != 0 && addr.port() != requested {
        let msg = format!("端口 {requested} 被占用，已自动改用 {}。", addr.port());
        logging::warn(&msg);
        println!("{msg}");
    }
    if let Some(pf) = &port_file {
        if let Some(dir) = pf.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // 写失败必须立即报错：桌面壳靠这个文件拿端口，否则会一直等（超时后才能提示）
        std::fs::write(pf, addr.port().to_string())
            .map_err(|e| anyhow::anyhow!("无法写入端口文件 {}：{e}", pf.display()))?;
        logging::info(format!("已写入端口文件 {}（端口 {}）", pf.display(), addr.port()));
    }
    let url = format!("http://{addr}");
    println!("PaperHelper Web 已启动: {url}");
    println!("（Ctrl-C 停止服务）");
    logging::info(format!("Web 服务监听 {url}（数据目录 {}）", paths::data_dir().display()));
    if open {
        open_browser(&url);
    }

    // web 模式不安装 CLI 的 REPL 打断器，这里自行监听 Ctrl-C 并终止整个进程。
    // 直接 exit 以确保即使有浏览器 SSE 长连接也能立即退出（不做 graceful 等待）。
    tokio::spawn(async {
        if tokio::signal::ctrl_c().await.is_ok() {
            logging::warn("收到 Ctrl-C，停止 Web 服务");
            eprintln!("\n[收到 Ctrl-C，停止服务]");
            interrupt::request(); // 若有在途任务，先请求中止
            std::process::exit(0);
        }
    });

    // /api/shutdown（顶栏「退出」按钮）走优雅退出；桌面壳关窗时也调用它。
    let shutdown = Arc::new(ShutdownHandle::default());
    let wait_handle = shutdown.clone();
    axum::serve(listener, router(shared).layer(Extension(shutdown.clone())))
        .with_graceful_shutdown(async move { wait_handle.wait().await })
        .await?;
    if let Some(pf) = &port_file {
        let _ = std::fs::remove_file(pf);
    }
    Ok(())
}

// ===== 静态前端 =====

async fn index() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-cache")],
        Html(include_str!("../web/index.html")),
    )
}

async fn stylesheet() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("../web/style.css"),
    )
}

async fn script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("../web/app.js"),
    )
}

/// PDF.js 阅读器适配层（按需加载：打开「原文」时才请求本文件）。
async fn pdf_viewer_script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("../web/pdf-viewer.js"),
    )
}

/// 编译期内嵌的第三方前端资源（marked / KaTeX，含字体）——断网也能渲染公式。
mod vendor_assets {
    include!(concat!(env!("OUT_DIR"), "/vendor_files.rs"));
}

/// `/vendor/*`：按路径查嵌入表返回资源（长缓存：升级随二进制版本变化）。
async fn api_vendor(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    let path = path.trim_start_matches('/');
    match vendor_assets::VENDOR_FILES.iter().find(|f| f.path == path) {
        Some(f) => (
            [
                (header::CONTENT_TYPE, f.mime),
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            f.bytes,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, format!("vendor 资源不存在: {path}")).into_response(),
    }
}

// ===== 首启向导：示例材料（编译期内嵌，离线可用） =====

/// 示例一：SelfCheckGPT 学习笔记（Markdown，原样导入：不调模型、0 token）。
const SAMPLE_NOTE_MD: &[u8] = include_bytes!("../assets/samples/selfcheckgpt-note.md");
/// 示例二：MIND 幻觉检测论文（PDF，完整体验：解析 → 调用一次模型生成笔记）。
const SAMPLE_PAPER_PDF: &[u8] = include_bytes!("../assets/samples/mind-hallucination.pdf");

struct SampleDef {
    id: &'static str,
    name: &'static str,
    kind: &'static str,
    desc: &'static str,
    /// 写进 uploads 目录时的文件名（带时间戳前缀防冲突）。
    file_name: &'static str,
    /// 导入后自动导出的笔记文件名。
    export_name: &'static str,
    needs_key: bool,
    bytes: &'static [u8],
}

fn samples() -> [SampleDef; 2] {
    [
        SampleDef {
            id: "note",
            name: "SelfCheckGPT 学习笔记",
            kind: "md",
            desc: "先看看生成好的笔记长什么样：原样导入，不调用模型、0 token，断网也能导入",
            file_name: "SelfCheckGPT_学习笔记.md",
            export_name: "笔记_SelfCheckGPT.md",
            needs_key: false,
            bytes: SAMPLE_NOTE_MD,
        },
        SampleDef {
            id: "paper",
            name: "MIND 幻觉检测论文（PDF）",
            kind: "pdf",
            desc: "完整体验：解析 PDF 全文 → 调用一次模型生成结构化笔记（约几分钱）",
            file_name: "MIND幻觉检测论文.pdf",
            export_name: "MIND幻觉检测_笔记.md",
            needs_key: true,
            bytes: SAMPLE_PAPER_PDF,
        },
    ]
}

/// 示例清单（供向导第四步渲染）。
async fn api_samples() -> Json<serde_json::Value> {
    let list: Vec<_> = samples()
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "name": s.name,
                "kind": s.kind,
                "desc": s.desc,
                "size": s.bytes.len(),
                "needs_key": s.needs_key,
            })
        })
        .collect();
    Json(json!({ "samples": list }))
}

/// 当前会话 PDF 原件的字节流（供前端 PDF.js 阅读器加载）。
/// 只读取 `App::pdf_source()` 解析出的路径，**不接受前端传入的任意路径**，
/// 避免任意文件读取；无源或文件不存在返回 404。
async fn api_pdf_file(State(app): State<SharedApp>) -> Response {
    let src = {
        let a = app.lock().await;
        a.pdf_source()
    };
    let Some(path) = src else {
        return (StatusCode::NOT_FOUND, "当前会话没有可阅读的 PDF 原件").into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mut resp = ([(header::CONTENT_TYPE, "application/pdf")], bytes).into_response();
            resp.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("private, max-age=3600"),
            );
            resp
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("读取 PDF 失败：{e}"),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct SampleReq {
    id: String,
}

/// 把内嵌示例写到 uploads 目录，返回可直接执行的导入命令（由前端走 `/api/run`）。
async fn api_sample_import(
    Json(req): Json<SampleReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let Some(s) = samples().into_iter().find(|s| s.id == req.id) else {
        return Err((StatusCode::BAD_REQUEST, format!("未知示例：{}", req.id)));
    };
    paths::ensure_uploads_dir()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("创建上传目录失败: {e:#}")))?;
    let stamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let path = paths::uploads_dir().join(format!("{stamp}_{}", s.file_name));
    tokio::fs::write(&path, s.bytes)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("写入示例文件失败: {e}")))?;
    logging::info(format!("导入示例：{} → {}", s.name, path.display()));
    Ok(Json(json!({
        "path": path.to_string_lossy(),
        "command": sample_command(&s, &path),
        "export": s.export_name,
        "name": s.name,
        "needs_key": s.needs_key,
    })))
}

/// 示例导入命令：md 原样导入（0 token），pdf 用内置「四段式」风格生成笔记。
fn sample_command(s: &SampleDef, path: &std::path::Path) -> String {
    let q = format!(
        "\"{}\"",
        path.display().to_string().replace('\\', "\\\\").replace('"', "\\\"")
    );
    if s.kind == "md" {
        format!("ingest --note --kind note --text {q}")
    } else {
        format!("ingest --style four --kind paper {q}")
    }
}

// ===== 命令执行（SSE） =====

#[derive(Deserialize)]
struct RunReq {
    /// 要执行的命令（与 CLI 完全一致，如 "ask 3.2 ..."）。
    command: String,
    /// 可选：本次任务自动导出笔记的路径（ingest/ask 用）。
    #[serde(default)]
    export: Option<String>,
}

async fn api_run(
    State(app): State<SharedApp>,
    Json(req): Json<RunReq>,
) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>> {
    let (tx, rx) = mpsc::unbounded_channel::<OutEvent>();
    let app2 = app.clone();
    tokio::spawn(async move {
        let RunReq { command, export } = req;
        let first = command.split_whitespace().next().unwrap_or("").to_string();
        let what = format!("命令 `{command}`");
        let t0 = std::time::Instant::now();
        logging::info(format!("执行命令: {command}"));

        // ingest：走 prepare/run/commit 三段式（LLM 生成阶段不持 App 锁）
        if matches!(first.as_str(), "ingest" | "pdf") {
            let rest: String = command
                .split_once(char::is_whitespace)
                .map(|(_, r)| r.to_string())
                .unwrap_or_default();
            let prepared = {
                let mut g = app2.lock().await;
                g.emitter = Emitter::channel(tx.clone());
                if let Some(e) = export.clone() {
                    if !e.trim().is_empty() {
                        g.export_path = Some(e);
                    }
                }
                let r = g.prepare_ingest(&rest).await;
                g.emitter = Emitter::terminal();
                r
            };
            match prepared {
                Ok(crate::app::IngestPrep::Direct { file_path, mode, kind }) => {
                    let mut g = app2.lock().await;
                    g.emitter = Emitter::channel(tx.clone());
                    let result = g.import_note(&file_path, &mode, &kind).await;
                    if result.is_ok() {
                        let _ = g.auto_persist();
                    }
                    emit_result(&g.emitter, result, &what, t0);
                    g.emitter = Emitter::terminal();
                }
                Ok(crate::app::IngestPrep::Llm(job)) => {
                    let res = match LLM_GATE.try_lock() {
                        Ok(_guard) => {
                            let emitter = Emitter::channel(tx.clone());
                            job.run(&emitter).await
                        }
                        Err(_) => Err(anyhow::anyhow!(LLM_BUSY_MSG)),
                    };
                    let mut g = app2.lock().await;
                    g.emitter = Emitter::channel(tx.clone());
                    let result = match res {
                        Ok(res) => g.commit_ingest(*job, res),
                        Err(e) => Err(e),
                    };
                    if result.is_ok() {
                        let _ = g.auto_persist();
                    }
                    emit_result(&g.emitter, result, &what, t0);
                    g.emitter = Emitter::terminal();
                }
                Err(e) => emit_result_channel(&tx, Err(e), &what, t0),
            }
            return;
        }

        // ask/check/sum：与 ingest 相同的三段式（LLM 阶段不持 App 锁，
        // 期间用户可浏览/切换会话；结果由 commit 写回发起会话）。
        if matches!(first.as_str(), "ask" | "q" | "check" | "sum") {
            enum Job {
                Ask(crate::app::AskJob),
                Sum(crate::app::SumJob),
            }
            let rest: String = command
                .split_once(char::is_whitespace)
                .map(|(_, r)| r.to_string())
                .unwrap_or_default();
            let prepared: anyhow::Result<Job> = {
                let mut g = app2.lock().await;
                g.emitter = Emitter::channel(tx.clone());
                let r = if first == "sum" {
                    g.prepare_sum(&rest).map(Job::Sum)
                } else {
                    g.prepare_ask_command(&rest, first == "check").map(Job::Ask)
                };
                g.emitter = Emitter::terminal();
                r
            };
            let job = match prepared {
                Ok(j) => j,
                Err(e) => {
                    emit_result_channel(&tx, Err(e), &what, t0);
                    return;
                }
            };
            // 锁外执行（LLM 门串行化）
            let res = match LLM_GATE.try_lock() {
                Ok(_guard) => {
                    let emitter = Emitter::channel(tx.clone());
                    match &job {
                        Job::Ask(j) => j.run(&emitter).await,
                        Job::Sum(j) => j.run(&emitter).await,
                    }
                }
                Err(_) => Err(anyhow::anyhow!(LLM_BUSY_MSG)),
            };
            // 落地（持锁）
            let mut g = app2.lock().await;
            g.emitter = Emitter::channel(tx.clone());
            let result = match res {
                Ok(res) => match job {
                    Job::Ask(j) => g.commit_ask(j, res).map(|_| ()),
                    Job::Sum(j) => g.commit_sum(j, res),
                },
                Err(e) => {
                    if let Job::Ask(j) = &job {
                        g.abort_ask(j);
                    }
                    Err(e)
                }
            };
            if result.is_ok() {
                let _ = g.auto_persist();
            }
            emit_result(&g.emitter, result, &what, t0);
            g.emitter = Emitter::terminal();
            return;
        }

        // 其它命令：短命令持锁执行（不再有长时间 LLM 命令走这里）
        let mut guard = app2.lock().await;
        guard.emitter = Emitter::channel(tx.clone());
        if let Some(e) = export.clone() {
            if !e.trim().is_empty() {
                guard.export_path = Some(e);
            }
        }
        let mutating = matches!(
            first.as_str(),
            "ingest" | "pdf" | "del" | "rm" | "undo" | "new" | "load"
        );
        let result = guard.run_command(&command).await;
        if result.is_ok() && mutating {
            let _ = guard.auto_persist();
        }
        emit_result(&guard.emitter, result, &what, t0);
        // 关键：恢复为终端输出器，丢弃 SSE sender，让接收端在 done 后正常结束流
        guard.emitter = Emitter::terminal();
    });

    let stream = UnboundedReceiverStream::new(rx).map(|ev| Ok::<_, Infallible>(to_sse(ev)));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// 把内部输出事件转成 SSE（data 用 JSON 字符串编码，避免换行破坏协议）。
/// 错误事件发结构化 JSON 对象 `{summary, detail}`，前端可展示摘要+原始详情。
fn to_sse(ev: OutEvent) -> SseEvent {
    if let OutEvent::Error { summary, detail } = ev {
        let obj = serde_json::json!({ "summary": summary, "detail": detail });
        return SseEvent::default().event("error").data(obj.to_string());
    }
    let (name, data) = match ev {
        OutEvent::Stdout(s) => ("stdout", s),
        OutEvent::Stderr(s) => ("stderr", s),
        OutEvent::Token(s) => ("token", s),
        OutEvent::Chars(n) => ("chars", n.to_string()),
        OutEvent::Reasoning(s) => ("reasoning", s),
        OutEvent::Progress(s) => ("progress", s),
        OutEvent::ProgressDone => ("progress_done", String::new()),
        OutEvent::Done => ("done", String::new()),
        OutEvent::Aborted => ("aborted", String::new()),
        OutEvent::Error { .. } => unreachable!("错误事件已在上方处理"),
    };
    let data = serde_json::to_string(&data).unwrap_or_else(|_| "\"\"".to_string());
    SseEvent::default().event(name).data(data)
}

/// 统一处理命令结果 → SSE：成功 done、用户中止 aborted、其他错误 error{摘要,详情}。
fn emit_result(emitter: &Emitter, result: anyhow::Result<()>, what: &str, t0: std::time::Instant) {
    match result {
        Ok(()) => {
            logging::info(format!("{what} 完成（{:.1}s）", t0.elapsed().as_secs_f64()));
            emitter.done();
        }
        Err(e) => {
            logging::error(format!(
                "{what} 失败（{:.1}s）：{e:#}",
                t0.elapsed().as_secs_f64()
            ));
            if llm::is_interrupted_error(&e) || interrupt::is_interrupted() {
                emitter.aborted();
            } else {
                emitter.error(format!("{e}"), format!("{e:#}"));
            }
        }
    }
}

async fn api_interrupt() -> Json<serde_json::Value> {
    interrupt::request();
    Json(json!({ "ok": true }))
}

// ===== 状态快照 =====

async fn api_state(State(app): State<SharedApp>) -> Json<serde_json::Value> {
    let a = app.lock().await;
    Json(build_state(&a))
}

/// 是否本地模型服务（Ollama / llama.cpp 等，通常不需要 API Key）。
fn is_local_endpoint(endpoint: &str) -> bool {
    let e = endpoint.to_ascii_lowercase();
    e.contains("127.0.0.1")
        || e.contains("localhost")
        || e.contains("[::1]")
        || e.contains("0.0.0.0")
}

fn build_state(a: &App) -> serde_json::Value {
    let s = &a.session.stats;
    let g = &a.kb.stats;
    let used = s.total_tokens() + g.total_tokens();

    // 当前会话是否可打开 PDF 原件（前端据此显示「原文」阅读器入口）
    let pdf_src = a.pdf_source();
    let pdf_name = pdf_src
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();

    // 当前节点上次真实请求的精确 input_tokens（API 返回的 usage，非估算）
    let current_input_tokens = a
        .session
        .conversation
        .current
        .as_ref()
        .and_then(|id| a.session.conversation.nodes.iter().find(|n| n.id == *id))
        .map(|n| n.input_tokens)
        .unwrap_or(0);

    // 笔记结构块
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    if let Some(note) = &a.session.notes {
        for (b, depth) in note.flatten() {
            blocks.push(json!({
                "id": b.id,
                "number": b.number,
                "kind": format!("{:?}", b.kind).to_lowercase(),
                "text": b.text.chars().take(80).collect::<String>(),
                "depth": depth,
                "explanations": b.explanations.len(),
            }));
        }
    }

    // 已读论文：置顶优先，其余按 read_at 倒序
    let mut papers_ref: Vec<&crate::knowledge::Paper> = a.kb.papers.iter().collect();
    papers_ref.sort_by(|x, y| {
        y.pinned.cmp(&x.pinned).then_with(|| y.read_at.cmp(&x.read_at))
    });
    let papers: Vec<serde_json::Value> = papers_ref
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "title": p.title,
                "path": p.path,
                "read_at": p.read_at,
                "kind": p.kind,
                "pinned": p.pinned,
            })
        })
        .collect();

    // 已学概念：置顶优先，其余按 created_at 倒序
    let mut concepts_ref: Vec<&crate::knowledge::Concept> = a.kb.concepts.iter().collect();
    concepts_ref.sort_by(|x, y| {
        y.pinned.cmp(&x.pinned).then_with(|| y.created_at.cmp(&x.created_at))
    });
    let concepts: Vec<serde_json::Value> = concepts_ref
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "paper": c.paper_title,
                "paper_id": c.paper_id,
                "definition": c.definition.chars().take(80).collect::<String>(),
                "pinned": c.pinned,
            })
        })
        .collect();

    json!({
        "model": a.config.llm.model,
        "endpoint": a.config.llm.api_endpoint,
        "api_key_set": !a.config.llm.api_key.is_empty(),
        "llm_ready": !a.config.llm.api_key.is_empty()
            || is_local_endpoint(&a.config.llm.api_endpoint),
        "api_key_masked": crate::app::mask_key(&a.config.llm.api_key),
        "context_length": a.config.llm.context_length,
        "thinking_mode": a.config.llm.thinking_mode,
        "pdf_input": a.config.llm.pdf_input,
        "pricing": {
            "input": a.config.pricing.input_price_per_1m,
            "output": a.config.pricing.output_price_per_1m,
        },
        "budget": a.config.budget.token_budget,
        "used_total": used,
        "stats": {
            "session": { "calls": s.calls, "input": s.total_input, "output": s.total_output, "cost": s.total_cost },
            "global":  { "calls": g.calls, "input": g.total_input, "output": g.total_output, "cost": g.total_cost },
        },
        "has_note": a.session.notes.is_some(),
        "note_title": a.session.notes.as_ref().map(|n| n.title.clone()).unwrap_or_default(),
        "pdf": {
            "available": pdf_src.is_some(),
            "name": pdf_name,
        },
        // 数学宏定义（HTML/讲义导入时收集）；前端渲染公式时注册给 KaTeX
        "math_macros": a
            .session
            .notes
            .as_ref()
            .and_then(|n| n.math_macros.clone())
            .unwrap_or_default(),
        "session_id": a.session.session_id,
        "export_path": a.export_path,
        "version": update::current_version(),
        "desktop": update::is_desktop(),
        "installed": update::is_installed(),
        "current": a.session.conversation.current_label(),
        "current_input_tokens": current_input_tokens,
        "context_length": a.config.llm.context_length,
        "can_undo": a.can_undo(),
        "blocks": blocks,
        "papers": papers,
        "concepts": concepts,
    })
}

// ===== 环境检测 / 依赖安装 / 服务商预设 / 退出 =====

/// 环境快照：Python 与 PyMuPDF 是否可用（首启向导「环境检查」用）。
async fn api_deps() -> Json<serde_json::Value> {
    let st = pdf::deps_status().await;
    Json(json!({
        "python": st.python_version,
        "python_cmd": st.python_cmd,
        "pymupdf": st.pymupdf_version,
        "ready": st.ready,
        "embedded": st.embedded,
        "data_dir": paths::data_dir().display().to_string(),
        "pip_index": pip_index(),
    }))
}

/// 依赖安装门：同一时刻只允许一个 pip 安装任务。
static DEPS_GATE: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// pip 安装源：默认清华镜像，可用 `PAPERHELPER_PIP_INDEX` 覆盖（内网/离线源）。
fn pip_index() -> String {
    std::env::var("PAPERHELPER_PIP_INDEX")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://pypi.tuna.tsinghua.edu.cn/simple".to_string())
}

/// 一键安装 PyMuPDF：清华镜像、SSE 流式进度、可中止（Ctrl-C / Web「停止」）。
async fn api_deps_install() -> Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>> {
    let (tx, rx) = mpsc::unbounded_channel::<OutEvent>();
    tokio::spawn(async move {
        let emitter = Emitter::channel(tx.clone());
        let t0 = std::time::Instant::now();
        let result = install_pymupdf(&emitter).await;
        emitter.progress_done();
        emit_result(&emitter, result, "安装 PyMuPDF", t0);
    });
    let stream = UnboundedReceiverStream::new(rx).map(|ev| Ok::<_, Infallible>(to_sse(ev)));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// 执行 `python -m pip install pymupdf`（流式回显输出，可中止）。
async fn install_pymupdf(emitter: &Emitter) -> anyhow::Result<()> {
    interrupt::reset();
    let _guard = DEPS_GATE
        .try_lock()
        .map_err(|_| anyhow::anyhow!("已有依赖安装任务在运行，请稍候或先点「停止」"))?;

    let st = pdf::deps_status().await;
    if let Some(v) = st.pymupdf_version.clone() {
        emitter.stdout(format!("✓ PyMuPDF 已安装（{v}），无需重复安装"));
        return Ok(());
    }
    let (py, args) = pdf::python_command().await;
    let index = pip_index();
    if st.python_version.is_none() {
        emitter.stdout(format!(
            "⚠ 未检测到可用的 Python（尝试的命令：{py}）。请先安装 Python 3，\
             或设置 PAPERHELPER_PYTHON 指向解释器；仍将尝试用该命令执行 pip。"
        ));
    }
    emitter.stdout(format!("使用 Python：{py} {}", args.join(" ")));
    emitter.progress("正在从镜像安装 PyMuPDF…");
    logging::info(format!("开始安装 PyMuPDF：{py} -m pip install -i {index} pymupdf"));

    let mut cmd = tokio::process::Command::new(&py);
    cmd.args(&args)
        .arg("-m")
        .arg("pip")
        .arg("install")
        .arg("--disable-pip-version-check")
        .arg("-i")
        .arg(&index)
        .arg("pymupdf")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("无法启动 pip（{py}）: {e}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let em_out = emitter.clone();
    let em_err = emitter.clone();
    let h_out = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            em_out.stdout(l);
        }
    });
    let h_err = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            em_err.stderr(l);
        }
    });

    let status = tokio::select! {
        s = child.wait() => s.map_err(|e| anyhow::anyhow!("等待 pip 结束失败: {e}"))?,
        _ = interrupt::wait() => {
            let _ = child.kill().await;
            return Err(anyhow::anyhow!(llm::Interrupted));
        }
    };
    let _ = h_out.await;
    let _ = h_err.await;

    if !status.success() {
        return Err(anyhow::anyhow!(
            "PyMuPDF 安装失败（pip 退出码 {:?}）。可手动安装后重试：\n  \
             {py} {} -m pip install -i {index} pymupdf",
            status.code(),
            args.join(" ")
        ));
    }
    // 安装后重置探测缓存并复检
    pdf::reset_python_probe().await;
    let st = pdf::deps_status().await;
    match st.pymupdf_version {
        Some(v) => {
            emitter.stdout(format!("✓ 安装完成：PyMuPDF {v}"));
            Ok(())
        }
        None => Err(anyhow::anyhow!(
            "pip 执行完成，但仍无法 import pymupdf。可能装到了其他 Python 环境：\
             可用 PAPERHELPER_PYTHON 指定解释器路径后重试"
        )),
    }
}

// ===== 版本更新 =====

#[derive(Deserialize)]
struct UpdateCheckQuery {
    /// 带 force 参数（任意值）= 手动检查，跳过 24h 节流。
    #[serde(default)]
    force: Option<String>,
}

/// `GET /api/update/check[?force=1]`：检查新版本（自动检查走节流、失败静默）。
async fn api_update_check(
    State(app): State<SharedApp>,
    Query(q): Query<UpdateCheckQuery>,
) -> Json<serde_json::Value> {
    let (client, cfg) = {
        let a = app.lock().await;
        (a.client.clone(), a.config.update.clone())
    };
    let force = q.force.is_some();
    if !force && !cfg.auto_check {
        return Json(json!({
            "ok": true,
            "disabled": true,
            "current": update::current_version(),
        }));
    }
    let status = update::check(&client, &cfg, force).await;
    let mut v = serde_json::to_value(&status).unwrap_or_default();
    v["desktop"] = json!(update::is_desktop());
    v["installed"] = json!(update::is_installed());
    v["auto_install"] = json!(update::is_desktop()
        && update::is_installed()
        && !status.setup_url.is_empty());
    Json(v)
}

#[derive(Deserialize)]
struct UpdateSkipReq {
    version: String,
}

/// `POST /api/update/skip`：跳过某版本，不再主动提示（手动检查仍能看到）。
async fn api_update_skip(
    Json(req): Json<UpdateSkipReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    update::skip(&req.version)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/update/status`：安装包下载/安装进度（前端轮询）。
async fn api_update_status() -> Json<serde_json::Value> {
    Json(serde_json::to_value(update::download_status()).unwrap_or_default())
}

/// `POST /api/update/download`：后台下载安装包（进度见 status；重复请求不重开任务）。
async fn api_update_download(State(app): State<SharedApp>) -> Json<serde_json::Value> {
    let (client, cfg) = {
        let a = app.lock().await;
        (a.client.clone(), a.config.update.clone())
    };
    let phase = update::download_status().phase;
    if phase == "downloading" || phase == "verifying" {
        return Json(json!({ "ok": true, "phase": phase }));
    }
    tokio::spawn(async move {
        let _ = update::download_setup(&client, &cfg).await;
    });
    Json(json!({ "ok": true, "phase": "downloading" }))
}

/// `POST /api/update/apply`：通知桌面壳退出并静默安装（仅桌面安装版）。
async fn api_update_apply() -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    update::apply_update().map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

/// 服务商预设（先启向导卡片；与 CLI `config presets` 同一份数据）。
async fn api_presets() -> Json<serde_json::Value> {
    Json(json!({ "presets": crate::presets::all() }))
}

/// 优雅退出（顶栏「退出」按钮；桌面壳关窗也用它）。
async fn api_shutdown(Extension(sh): Extension<Arc<ShutdownHandle>>) -> Json<serde_json::Value> {
    logging::info("收到 /api/shutdown，Web 服务即将退出");
    sh.trigger();
    Json(json!({ "ok": true }))
}

/// 在系统文件管理器里打开数据目录（新手找不到 `.paperhelper` 时用）。
async fn api_open_data_dir() -> Json<serde_json::Value> {
    if let Err(e) = paths::ensure_data_dir() {
        return Json(json!({ "ok": false, "error": format!("创建数据目录失败: {e:#}") }));
    }
    let dir = paths::data_dir();
    let abs = std::fs::canonicalize(&dir).unwrap_or(dir);
    match open_path(&abs) {
        Ok(()) => Json(json!({ "ok": true, "path": abs.display().to_string() })),
        Err(e) => Json(json!({ "ok": false, "path": abs.display().to_string(), "error": e })),
    }
}

/// 用系统文件管理器打开路径（WSL 下 xdg-open 可能不可用，返回中文提示）。
fn open_path(path: &std::path::Path) -> std::result::Result<(), String> {
    let p = path.to_string_lossy().to_string();
    let (prog, args): (&str, Vec<String>) = if cfg!(target_os = "windows") {
        ("explorer", vec![p])
    } else if cfg!(target_os = "macos") {
        ("open", vec![p])
    } else {
        ("xdg-open", vec![p])
    };
    std::process::Command::new(prog)
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("无法打开文件管理器（{prog} 不可用：{e}）"))
}

// ===== 笔记渲染 =====

#[derive(Deserialize)]
struct NoteQuery {
    #[serde(default)]
    format: Option<String>,
}

async fn api_note(State(app): State<SharedApp>, Query(q): Query<NoteQuery>) -> Response {
    let a = app.lock().await;
    let Some(note) = &a.session.notes else {
        return (StatusCode::NOT_FOUND, "还没有笔记，请先导入一篇论文").into_response();
    };
    let want_md = matches!(q.format.as_deref(), Some("md") | Some("markdown"));
    if want_md {
        (
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            export::to_markdown(note),
        )
            .into_response()
    } else {
        let hidden = hidden_expl_ids(&a.session);
        (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            export::to_html_bare(note, &a.session.conversation, &hidden),
        )
            .into_response()
    }
}

/// 计算批注线程涉及的所有解释 id（这些问答不在笔记正文内联显示）。
fn hidden_expl_ids(sess: &session::Session) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for ann in &sess.annotations {
        let mut stack = vec![ann.root_node_id.clone()];
        while let Some(id) = stack.pop() {
            if let Some(n) = sess.conversation.nodes.iter().find(|n| n.id == id) {
                if let Some(e) = &n.explanation_id {
                    set.insert(e.clone());
                }
                for c in sess
                    .conversation
                    .nodes
                    .iter()
                    .filter(|c| c.parent.as_deref() == Some(id.as_str()))
                {
                    stack.push(c.id.clone());
                }
            }
        }
    }
    set
}

// ===== 笔记风格（管理 / 导入时可选） =====

async fn api_styles() -> Json<serde_json::Value> {
    match crate::prompts::list_styles() {
        Ok(list) => {
            let items: Vec<serde_json::Value> = list
                .iter()
                .map(|s| {
                    json!({
                        "id": s.id,
                        "label": s.label,
                        "desc": s.desc,
                        "builtin": s.builtin,
                        "scope": s.scope,
                        "prompt": crate::prompts::style_prompt_text(s),
                    })
                })
                .collect();
            Json(json!({ "styles": items }))
        }
        Err(e) => Json(json!({ "styles": [], "error": format!("{e:#}") })),
    }
}

#[derive(Deserialize)]
struct StyleSaveReq {
    id: String,
    /// 自定义风格改名时的旧 id（内置风格忽略）。
    #[serde(default)]
    old_id: Option<String>,
    label: String,
    #[serde(default)]
    desc: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    prompt: String,
}

/// 新建/更新一个风格（写 `styles.toml` + `styles/<id>.txt`）。
async fn api_style_save(
    Json(req): Json<StyleSaveReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let meta = crate::prompts::NoteStyle {
        id: req.id.trim().to_string(),
        label: req.label,
        desc: req.desc,
        file: String::new(),
        builtin: false,
        scope: if req.scope.trim().is_empty() { "any".into() } else { req.scope },
    };
    if let Some(old) = req.old_id.as_deref() {
        if !old.trim().is_empty() && old.trim() != meta.id {
            crate::prompts::rename_style(old, &meta.id)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
        }
    }
    crate::prompts::save_style(&meta, &req.prompt)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct StyleIdReq {
    id: String,
}

/// 删除自定义风格（内置不可删）。
async fn api_style_delete(
    Json(req): Json<StyleIdReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    crate::prompts::delete_style(&req.id)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

/// 恢复内置风格的默认提示词与说明。
async fn api_style_reset(
    Json(req): Json<StyleIdReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    crate::prompts::reset_style(&req.id)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

// ===== 笔记编辑（改文字 / 整节重写 / 插入 / 删除 / AI 生成） =====

fn note_err(e: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, format!("{e:#}"))
}

#[derive(Deserialize)]
struct BlockQuery {
    id: String,
}

/// 取单个块的完整文本（供编辑弹窗；`__title__` 返回标题）。
async fn api_note_block(
    State(app): State<SharedApp>,
    Query(q): Query<BlockQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = app.lock().await;
    let note = a
        .session
        .notes
        .as_ref()
        .ok_or((StatusCode::NOT_FOUND, "还没有笔记".to_string()))?;
    if q.id == notes::TITLE_ID {
        return Ok(Json(json!({
            "id": notes::TITLE_ID,
            "kind": "section",
            "number": "",
            "text": note.title,
            "markdown": note.title,
            "explanations": 0,
            "children": 0,
        })));
    }
    let b = note
        .find_block(&q.id)
        .ok_or((StatusCode::NOT_FOUND, "找不到该块（笔记可能已变化）".to_string()))?;
    Ok(Json(json!({
        "id": b.id,
        "kind": format!("{:?}", b.kind).to_lowercase(),
        "number": b.number,
        "text": b.text,
        "markdown": note.block_markdown(&b.id),
        "explanations": b.explanations.len(),
        "children": b.children.len(),
    })))
}

#[derive(Deserialize)]
struct BlockEditReq {
    block_id: String,
    text: String,
}

/// 改块文字（`__title__` 改标题）。
async fn api_note_edit(
    State(app): State<SharedApp>,
    Json(req): Json<BlockEditReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut a = app.lock().await;
    a.edit_block(&req.block_id, &req.text).map_err(note_err)?;
    logging::info(format!("编辑笔记块 {}", req.block_id));
    Ok(Json(json!({ "ok": true })))
}

/// 整节重写：段落替换文本、章节替换 children。
async fn api_note_rewrite(
    State(app): State<SharedApp>,
    Json(req): Json<BlockEditReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut a = app.lock().await;
    a.rewrite_block(&req.block_id, &req.text).map_err(note_err)?;
    logging::info(format!("重写笔记块 {}", req.block_id));
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct BlockAddReq {
    after_block_id: String,
    text: String,
}

/// 在某块后插入 Markdown 解析出的块。
async fn api_note_add(
    State(app): State<SharedApp>,
    Json(req): Json<BlockAddReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut a = app.lock().await;
    a.insert_after(&req.after_block_id, &req.text).map_err(note_err)?;
    logging::info(format!("在 {} 后插入笔记内容", req.after_block_id));
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct BlockDeleteReq {
    block_id: String,
}

/// 删除块及子树（含追问与相关批注）。
async fn api_note_delete(
    State(app): State<SharedApp>,
    Json(req): Json<BlockDeleteReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut a = app.lock().await;
    let n = a.remove_note_block(&req.block_id).map_err(note_err)?;
    logging::info(format!("删除笔记块 {}（含子树共 {n} 块）", req.block_id));
    Ok(Json(json!({ "ok": true, "removed_blocks": n })))
}

#[derive(Deserialize)]
struct NoteAiReq {
    block_id: String,
    instruction: String,
    #[serde(default)]
    mode: Option<String>,
}

/// AI 改写/补充（SSE 流式、可停止）：只生成内容，不改笔记；
/// 用户确认后由前端调用 rewrite / add 落地。
async fn api_note_ai(
    State(app): State<SharedApp>,
    Json(req): Json<NoteAiReq>,
) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>> {
    let (tx, rx) = mpsc::unbounded_channel::<OutEvent>();
    let app2 = app.clone();
    tokio::spawn(async move {
        let mode = req.mode.clone().unwrap_or_else(|| "rewrite".to_string());
        let what = format!("AI {mode} {}", req.block_id);
        let t0 = std::time::Instant::now();
        // 1) prepare（持锁）
        let prepared = {
            let mut g = app2.lock().await;
            g.emitter = Emitter::channel(tx.clone());
            let r = g.prepare_note_ai(&req.block_id, &req.instruction, &mode);
            g.emitter = Emitter::terminal();
            r
        };
        let job = match prepared {
            Ok(j) => j,
            Err(e) => {
                emit_result_channel(&tx, Err(e), &what, t0);
                return;
            }
        };
        // 2) 锁外执行（LLM 门串行化）
        let res = match LLM_GATE.try_lock() {
            Ok(_guard) => {
                let emitter = Emitter::channel(tx.clone());
                job.run(&emitter).await
            }
            Err(_) => Err(anyhow::anyhow!(LLM_BUSY_MSG)),
        };
        // 3) commit（持锁）
        let mut g = app2.lock().await;
        g.emitter = Emitter::channel(tx.clone());
        let result = match res {
            Ok(res) => {
                g.commit_usage(&job, &res);
                Ok(())
            }
            Err(e) => Err(e),
        };
        emit_result(&g.emitter, result, &what, t0);
        g.emitter = Emitter::terminal();
    });
    let stream = UnboundedReceiverStream::new(rx).map(|ev| Ok::<_, Infallible>(to_sse(ev)));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct RestyleReq {
    style: String,
    /// 额外要求（可选）。
    #[serde(default)]
    extra: String,
}

/// 按风格重写全文（SSE 流式）：只生成、不落地；用户确认后调 `/api/note/restyle/apply`。
async fn api_note_restyle(
    State(app): State<SharedApp>,
    Json(req): Json<RestyleReq>,
) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>> {
    let (tx, rx) = mpsc::unbounded_channel::<OutEvent>();
    let app2 = app.clone();
    tokio::spawn(async move {
        let what = format!("按风格重写（{}）", req.style);
        let t0 = std::time::Instant::now();
        // 1) prepare（持锁）
        let prepared = {
            let mut g = app2.lock().await;
            g.emitter = Emitter::channel(tx.clone());
            let r = g.prepare_restyle(&req.style, &req.extra);
            g.emitter = Emitter::terminal();
            r
        };
        let job = match prepared {
            Ok(j) => j,
            Err(e) => {
                emit_result_channel(&tx, Err(e), &what, t0);
                return;
            }
        };
        // 2) 锁外执行（LLM 门）
        let res = match LLM_GATE.try_lock() {
            Ok(_guard) => {
                let emitter = Emitter::channel(tx.clone());
                job.run(&emitter).await
            }
            Err(_) => Err(anyhow::anyhow!(LLM_BUSY_MSG)),
        };
        // 3) commit（持锁）
        let mut g = app2.lock().await;
        g.emitter = Emitter::channel(tx.clone());
        let result = match res {
            Ok(res) => {
                g.commit_usage(&job, &res);
                Ok(())
            }
            Err(e) => Err(e),
        };
        emit_result(&g.emitter, result, &what, t0);
        g.emitter = Emitter::terminal();
    });
    let stream = UnboundedReceiverStream::new(rx).map(|ev| Ok::<_, Infallible>(to_sse(ev)));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct RestyleApplyReq {
    text: String,
}

/// 把「按风格重写」的生成结果落地为新的整篇笔记（会清空旧批注，可撤销）。
async fn api_note_restyle_apply(
    State(app): State<SharedApp>,
    Json(req): Json<RestyleApplyReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut a = app.lock().await;
    a.apply_restyle(&req.text).map_err(note_err)?;
    let _ = a.auto_persist();
    crate::logging::info("按风格重写整篇笔记".to_string());
    Ok(Json(json!({ "ok": true })))
}

// ===== 导出下载 =====

#[derive(Deserialize)]
struct ExportQuery {
    #[serde(default)]
    format: Option<String>,
}

/// `GET /api/export?format=md|mm|html`：返回对应渲染内容，供浏览器下载。
async fn api_export(State(app): State<SharedApp>, Query(q): Query<ExportQuery>) -> Response {
    let a = app.lock().await;
    let Some(note) = &a.session.notes else {
        return (StatusCode::NOT_FOUND, "还没有笔记").into_response();
    };
    let fmt = q.format.as_deref().unwrap_or("md");
    let (content, ext) = match fmt {
        "html" | "htm" => (
            export::to_html(note, &a.session.conversation, &a.session.annotations),
            "html",
        ),
        "mm" | "mindmap" => (export::to_mindmap(note), "mm"),
        _ => (export::to_markdown(note), "md"),
    };
    // 文件名：优先用会话的导出名 stem，否则用笔记标题
    let stem = a
        .export_path
        .as_ref()
        .and_then(|p| {
            std::path::Path::new(p)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            let t: String = note.title.chars().take(40).collect();
            if t.trim().is_empty() {
                "note".to_string()
            } else {
                t
            }
        });
    let filename = format!("{}.{}", sanitize_download_name(&stem), ext);
    let ctype = if ext == "html" {
        "text/html; charset=utf-8"
    } else {
        "text/markdown; charset=utf-8"
    };
    let cd = format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        filename
            .chars()
            .map(|c| if c.is_ascii() && c != '"' { c } else { '_' })
            .collect::<String>(),
        pct_encode(&filename)
    );
    let mut resp = ([(header::CONTENT_TYPE, ctype)], content).into_response();
    if let Ok(v) = header::HeaderValue::from_str(&cd) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}

/// 下载文件名安全化（去掉路径分隔符等）。
fn sanitize_download_name(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' | '\r' => '_',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// RFC 5987 百分号编码（用于 Content-Disposition 的 filename*）。
fn pct_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ===== 配置 =====

async fn api_config_get(State(app): State<SharedApp>) -> Json<serde_json::Value> {
    let a = app.lock().await;
    Json(json!({
        "llm": {
            "api_endpoint": a.config.llm.api_endpoint,
            "api_key_masked": crate::app::mask_key(&a.config.llm.api_key),
            "api_key_set": !a.config.llm.api_key.is_empty(),
            "model": a.config.llm.model,
            "context_length": a.config.llm.context_length,
            "thinking_mode": a.config.llm.thinking_mode,
            "pdf_input": a.config.llm.pdf_input,
        },
        "pricing": {
            "input_price_per_1m": a.config.pricing.input_price_per_1m,
            "output_price_per_1m": a.config.pricing.output_price_per_1m,
        },
        "budget": { "token_budget": a.config.budget.token_budget },
        "presets": {
            "models": a.config.presets.models,
            "endpoints": a.config.presets.endpoints,
        },
    }))
}

#[derive(Deserialize)]
struct ConfigReq {
    key: String,
    value: String,
}

async fn api_config_set(
    State(app): State<SharedApp>,
    Json(req): Json<ConfigReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut a = app.lock().await;
    a.set_config(&req.key, &req.value)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    a.config
        .save()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize, Default)]
struct ConfigTestReq {
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

/// 测试 LLM 配置：可带未保存的表单值覆盖，返回结构化结果与原始响应。
/// 先在锁内克隆配置/客户端，网络请求在锁外执行，避免阻塞其它接口。
async fn api_config_test(
    State(app): State<SharedApp>,
    Json(req): Json<ConfigTestReq>,
) -> Json<serde_json::Value> {
    let (client, mut cfg) = {
        let a = app.lock().await;
        (a.client.clone(), a.config.llm.clone())
    };
    if let Some(ep) = req.endpoint.filter(|s| !s.trim().is_empty()) {
        cfg.api_endpoint = ep.trim().to_string();
    }
    if let Some(m) = req.model.filter(|s| !s.trim().is_empty()) {
        cfg.model = m.trim().to_string();
    }
    if let Some(k) = req.api_key.filter(|s| !s.trim().is_empty()) {
        cfg.api_key = k.trim().to_string();
    }
    let result = llm::test(&client, &cfg).await;
    Json(serde_json::to_value(result).unwrap_or_else(|_| {
        json!({ "ok": false, "status": 0, "latency_ms": 0, "model": "", "reply": "", "input_tokens": 0, "output_tokens": 0, "raw": "结果序列化失败" })
    }))
}

// ===== 会话历史 =====

/// 精确匹配或唯一前缀匹配会话编号。
fn resolve_session_id(key: &str) -> Result<String, (StatusCode, String)> {
    let ids = paths::list_sessions();
    if let Some(exact) = ids.iter().find(|s| s.as_str() == key) {
        return Ok(exact.clone());
    }
    let hits: Vec<&String> = ids.iter().filter(|s| s.starts_with(key)).collect();
    match hits.len() {
        1 => Ok(hits[0].clone()),
        0 => Err((StatusCode::NOT_FOUND, format!("会话不存在: {key}"))),
        _ => Err((StatusCode::CONFLICT, format!("前缀不唯一: {key}"))),
    }
}

/// 会话列表：置顶优先，其余按 `updated_at` 新→旧。
async fn api_sessions() -> Json<serde_json::Value> {
    let pins = session::load_pins();
    let mut list: Vec<serde_json::Value> = paths::list_sessions()
        .iter()
        .map(|id| {
            let meta = session::read_meta(&paths::session_path(id)).unwrap_or_default();
            let name = if meta.session_name.is_empty() {
                "（未命名）".to_string()
            } else {
                meta.session_name
            };
            json!({
                "id": id,
                "name": name,
                "updated_at": meta.updated_at,
                "pinned": pins.iter().any(|p| p == id),
            })
        })
        .collect();
    list.sort_by(|a, b| {
        let pa = a["pinned"].as_bool().unwrap_or(false);
        let pb = b["pinned"].as_bool().unwrap_or(false);
        pb.cmp(&pa).then_with(|| {
            let ua = a["updated_at"].as_str().unwrap_or("");
            let ub = b["updated_at"].as_str().unwrap_or("");
            ub.cmp(ua)
        })
    });
    Json(json!({ "sessions": list }))
}

#[derive(Deserialize)]
struct SessionReq {
    id: String,
}

async fn api_session_load(
    State(app): State<SharedApp>,
    Json(req): Json<SessionReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let target = resolve_session_id(&req.id)?;
    let path = paths::session_path(&target);
    let loaded = session::Session::load(&path)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let mut a = app.lock().await;
    // 切换前先落盘当前会话：在途 LLM 任务稍后会把结果写回它（见 App::with_session）
    if let Err(e) = a.auto_persist() {
        crate::logging::warn(format!("切换会话前保存失败: {e:#}"));
    }
    a.session = loaded;
    a.export_path = a.session.export_path.clone();
    a.update_completions();
    let name = a.session.session_name.clone();
    Ok(Json(json!({ "ok": true, "id": target, "name": name })))
}

async fn api_session_save(
    State(app): State<SharedApp>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut a = app.lock().await;
    a.emitter = Emitter::terminal();
    a.autosave_on_exit()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(json!({
        "ok": true,
        "id": a.session.session_id,
        "name": a.session.session_name,
    })))
}

#[derive(Deserialize)]
struct PinReq {
    id: String,
    pinned: bool,
}

async fn api_session_pin(
    Json(req): Json<PinReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let target = resolve_session_id(&req.id)?;
    let mut pins = session::load_pins();
    if req.pinned {
        if !pins.iter().any(|p| p == &target) {
            pins.push(target.clone());
        }
    } else {
        pins.retain(|p| p != &target);
    }
    session::save_pins(&pins)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true, "id": target, "pinned": req.pinned })))
}

#[derive(Deserialize)]
struct RenameReq {
    id: String,
    name: String,
}

async fn api_session_rename(
    State(app): State<SharedApp>,
    Json(req): Json<RenameReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let target = resolve_session_id(&req.id)?;
    let path = paths::session_path(&target);
    let mut sess = session::Session::load(&path)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    sess.session_name = req.name.trim().to_string();
    sess.save(&path)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    // 若改的是当前会话，同步内存中的名字
    let mut a = app.lock().await;
    if a.session.session_id == target {
        a.session.session_name = sess.session_name.clone();
    }
    Ok(Json(json!({ "ok": true, "id": target, "name": sess.session_name })))
}

async fn api_session_delete(
    State(app): State<SharedApp>,
    Json(req): Json<SessionReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let target = resolve_session_id(&req.id)?;
    let path = paths::session_path(&target);
    std::fs::remove_file(&path)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("删除失败: {e}")))?;
    let mut pins = session::load_pins();
    pins.retain(|p| p != &target);
    let _ = session::save_pins(&pins);

    let mut a = app.lock().await;
    // 给在途 LLM 任务制造冲突：结果不会写回已删除的会话
    a.bump_session(&target);
    let mut reset = false;
    if a.session.session_id == target {
        // 删除的是当前会话：自动新建空会话
        a.session = session::Session::default();
        a.export_path = None;
        a.update_completions();
        reset = true;
    }
    Ok(Json(json!({ "ok": true, "id": target, "reset": reset })))
}

// ===== 会话迁移：导出 / 导入 =====

#[derive(Deserialize)]
struct SessionExportQuery {
    /// 会话编号（逗号分隔多个）；缺省 = 全部会话 + 知识库 + 置顶。
    #[serde(default)]
    id: Option<String>,
}

/// `GET /api/sessions/export[?id=<编号[,编号…]>]`
/// - 单个：原始会话 JSON（可被 CLI `load` 直接加载）
/// - 多个/缺省：备份包（缺省含全部会话 + 知识库 + 全部置顶）
async fn api_sessions_export(Query(q): Query<SessionExportQuery>) -> Response {
    let dir = paths::data_dir();
    let keys: Vec<String> = q
        .id
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    if keys.len() == 1 {
        let target = match resolve_session_id(&keys[0]) {
            Ok(t) => t,
            Err((code, msg)) => return (code, msg).into_response(),
        };
        let sess = match session::Session::load(&paths::session_path(&target)) {
            Ok(s) => s,
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, format!("读取会话失败：{e:#}"))
                    .into_response()
            }
        };
        return match serde_json::to_string_pretty(&sess) {
            Ok(body) => json_attachment(body, format!("{target}.json")),
            Err(e) => {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("序列化失败：{e}")).into_response()
            }
        };
    }

    let selected = if keys.is_empty() {
        None
    } else {
        let mut ids = Vec::with_capacity(keys.len());
        for k in &keys {
            match resolve_session_id(k) {
                Ok(t) => ids.push(t),
                Err((code, msg)) => return (code, msg).into_response(),
            }
        }
        Some(ids)
    };
    let bundle = match transfer::export_bundle(&dir, selected.as_deref()) {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("导出失败：{e:#}")).into_response()
        }
    };
    let body = match serde_json::to_string_pretty(&bundle) {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("序列化失败：{e}")).into_response()
        }
    };
    let name = format!(
        "paperhelper-backup-{}.json",
        chrono::Local::now().format("%Y%m%d_%H%M%S")
    );
    json_attachment(body, name)
}

/// `POST /api/sessions/import`（multipart，字段 `file`）：
/// 接受备份包或单个会话 JSON；编号冲突保留两者（自动改名），完全相同则跳过。
async fn api_sessions_import(
    State(app): State<SharedApp>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("读取上传失败（连接可能中断）：{e}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let filename = field.file_name().unwrap_or("import.json").to_string();
        let bytes = field
            .bytes()
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("读取上传数据失败：{e}")))?;
        if bytes.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "文件为空".into()));
        }
        if bytes.len() as u64 > MAX_UPLOAD_BYTES {
            let limit_mb = MAX_UPLOAD_BYTES / 1024 / 1024;
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("文件超过 {limit_mb}MB 上限，已拒绝"),
            ));
        }
        let input = transfer::parse_import(&bytes)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
        let dir = paths::data_dir();
        let report = tokio::task::spawn_blocking(move || transfer::import_into(&dir, input))
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("导入任务异常：{e}")))?
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("导入失败：{e:#}")))?;
        // 备份包可能带知识库：合并后重新载入内存（非 LLM 操作，短暂持锁）
        if report.papers_added > 0 || report.concepts_added > 0 {
            let mut a = app.lock().await;
            match knowledge::KnowledgeBase::load() {
                Ok(kb) => a.kb = kb,
                Err(e) => logging::warn(format!("导入后重新加载知识库失败：{e:#}")),
            }
        }
        logging::info(format!(
            "会话导入完成：{filename}（新增 {}，跳过 {}；知识库 +{} 论文/+{} 概念；置顶 +{}）",
            report.sessions.iter().filter(|s| !s.skipped).count(),
            report.sessions.iter().filter(|s| s.skipped).count(),
            report.papers_added,
            report.concepts_added,
            report.pins_added,
        ));
        return Ok(Json(json!({
            "ok": true,
            "sessions": report.sessions,
            "knowledge": {
                "papers_added": report.papers_added,
                "concepts_added": report.concepts_added,
            },
            "pins_added": report.pins_added,
        })));
    }
    Err((StatusCode::BAD_REQUEST, "缺少 file 字段".into()))
}

/// 以附件下载形式返回 JSON（文件名 ASCII 回退 + RFC 5987 filename*，支持中文）。
fn json_attachment(body: String, filename: String) -> Response {
    let ascii: String = filename
        .chars()
        .map(|c| if c.is_ascii() && c != '"' { c } else { '_' })
        .collect();
    let cd = format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{}",
        pct_encode(&filename)
    );
    let mut resp = (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        body,
    )
        .into_response();
    if let Ok(v) = header::HeaderValue::from_str(&cd) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}

// ===== 概念 / 论文详情 =====

#[derive(Deserialize)]
struct NameQuery {
    name: String,
}

async fn api_concept(
    State(app): State<SharedApp>,
    Query(q): Query<NameQuery>,
) -> Json<serde_json::Value> {
    let a = app.lock().await;
    let concept = a.kb.concepts.iter().find(|c| c.name == q.name);
    let definition = concept.map(|c| c.definition.clone()).unwrap_or_default();
    let paper_title = concept.map(|c| c.paper_title.clone()).unwrap_or_default();
    let paper_id = concept.and_then(|c| {
        if c.paper_id.is_empty() {
            None
        } else {
            Some(c.paper_id.clone())
        }
    });
    let hit = session::find_concept_qa(&q.name, paper_id.as_deref());
    let (sid, sname, supd, question, answer, expl_id, annotation_id, block_id) = match hit {
        Some((m, qu, an, eid, ann)) => {
            let (aid, bid) = match ann {
                Some((a, b)) => (Some(a), b),
                None => (None, String::new()),
            };
            (m.session_id, m.session_name, m.updated_at, qu, an, eid, aid, bid)
        }
        None => (
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            None,
            None,
            String::new(),
        ),
    };
    Json(json!({
        "name": q.name,
        "definition": definition,
        "paper_title": paper_title,
        "session_id": sid,
        "session_name": sname,
        "updated_at": supd,
        "question": question,
        "answer": answer,
        "explanation_id": expl_id,
        "annotation_id": annotation_id,
        "block_id": block_id,
    }))
}

#[derive(Deserialize)]
struct IdQuery {
    id: String,
}

async fn api_paper(
    State(app): State<SharedApp>,
    Query(q): Query<IdQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = app.lock().await;
    let paper = a
        .kb
        .papers
        .iter()
        .find(|p| p.id == q.id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "论文不存在".to_string()))?;
    let (sid, sname, supd) = match session::find_session_by_paper(&q.id, &paper.title) {
        Some((id, m)) => (id, m.session_name, m.updated_at),
        None => (String::new(), String::new(), String::new()),
    };
    Ok(Json(json!({
        "title": paper.title,
        "path": paper.path,
        "read_at": paper.read_at,
        "session_id": sid,
        "session_name": sname,
        "updated_at": supd,
    })))
}

/// 论文对应会话的笔记 HTML（无树侧栏，供标签页 iframe 使用）。
async fn api_paper_note(State(app): State<SharedApp>, Query(q): Query<IdQuery>) -> Response {
    let (paper_id, title) = {
        let a = app.lock().await;
        match a.kb.papers.iter().find(|p| p.id == q.id) {
            Some(p) => (p.id.clone(), p.title.clone()),
            None => (q.id.clone(), String::new()),
        }
    };
    let Some((sid, _meta)) = session::find_session_by_paper(&paper_id, &title) else {
        return (StatusCode::NOT_FOUND, "找不到该论文对应的会话").into_response();
    };
    let path = paths::session_path(&sid);
    let Ok(sess) = session::Session::load(&path) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "加载会话失败").into_response();
    };
    let Some(note) = &sess.notes else {
        return (StatusCode::NOT_FOUND, "该会话没有笔记").into_response();
    };
    let hidden = hidden_expl_ids(&sess);
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        export::to_html_bare(note, &sess.conversation, &hidden),
    )
        .into_response()
}

// ===== 知识库（已读论文 / 已学概念）置顶与删除 =====

#[derive(Deserialize)]
struct PaperPinReq {
    id: String,
    pinned: bool,
}

async fn api_paper_pin(
    State(app): State<SharedApp>,
    Json(req): Json<PaperPinReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut a = app.lock().await;
    let Some(p) = a.kb.papers.iter_mut().find(|p| p.id == req.id) else {
        return Err((StatusCode::NOT_FOUND, "论文不存在".to_string()));
    };
    p.pinned = req.pinned;
    a.kb.save()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

async fn api_paper_delete(
    State(app): State<SharedApp>,
    Json(req): Json<SessionReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut a = app.lock().await;
    let before = a.kb.papers.len();
    a.kb.papers.retain(|p| p.id != req.id);
    if a.kb.papers.len() == before {
        return Err((StatusCode::NOT_FOUND, "论文不存在".to_string()));
    }
    a.kb.save()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ConceptKeyReq {
    name: String,
    paper_id: String,
    pinned: bool,
}

async fn api_concept_pin(
    State(app): State<SharedApp>,
    Json(req): Json<ConceptKeyReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut a = app.lock().await;
    let Some(c) = a
        .kb
        .concepts
        .iter_mut()
        .find(|c| c.name == req.name && c.paper_id == req.paper_id)
    else {
        return Err((StatusCode::NOT_FOUND, "概念不存在".to_string()));
    };
    c.pinned = req.pinned;
    a.kb.save()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ConceptDeleteReq {
    name: String,
    paper_id: String,
}

async fn api_concept_delete(
    State(app): State<SharedApp>,
    Json(req): Json<ConceptDeleteReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut a = app.lock().await;
    let before = a.kb.concepts.len();
    a.kb.concepts
        .retain(|c| !(c.name == req.name && c.paper_id == req.paper_id));
    if a.kb.concepts.len() == before {
        return Err((StatusCode::NOT_FOUND, "概念不存在".to_string()));
    }
    a.kb.save()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

// ===== 批注（笔记选中文字提问） =====

#[derive(Deserialize)]
struct AnnotateReq {
    block_id: String,
    quote: String,
    /// 给 LLM 的上下文（公式已还原成 TeX）；缺省回退 `quote`。
    #[serde(default)]
    quote_tex: Option<String>,
    question: String,
    /// "ask"（默认，写入笔记解释）或 "check"（只进批注线程）。
    #[serde(default)]
    mode: Option<String>,
    /// 是否把模型标注的 [[概念: …]] 写入「已学概念」（默认 true）。
    #[serde(default = "default_true")]
    record_concept: bool,
    /// PDF 批注锚点（笔记批注缺省）。
    #[serde(default)]
    pdf: Option<PdfAnchorReq>,
    /// 该页渲染图（data URL）。仅 PDF 批注用，随本次请求发给模型，不入历史。
    #[serde(default)]
    image: Option<String>,
}

/// PDF 阅读器批注锚点：页码 + 页面内归一化矩形 + 类型。
#[derive(Deserialize)]
struct PdfAnchorReq {
    #[serde(default)]
    page: u32,
    #[serde(default)]
    rects: Vec<[f32; 4]>,
    /// `text`（默认）/ `image` / `page`。
    #[serde(default)]
    kind: Option<String>,
}

async fn api_annotate(
    State(app): State<SharedApp>,
    Json(req): Json<AnnotateReq>,
) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>> {
    let (tx, rx) = mpsc::unbounded_channel::<OutEvent>();
    let app2 = app.clone();
    tokio::spawn(async move {
        let is_check = matches!(req.mode.as_deref(), Some("check"));
        let what = format!("批注提问「{}」", req.question);
        let t0 = std::time::Instant::now();
        let prepared = {
            let mut g = app2.lock().await;
            g.emitter = Emitter::channel(tx.clone());
            let r = if let Some(pdf) = &req.pdf {
                g.prepare_annotate_pdf(
                    pdf.page,
                    pdf.rects.clone(),
                    pdf.kind.as_deref().unwrap_or("text"),
                    &req.quote,
                    &req.question,
                    is_check,
                    req.record_concept,
                    req.image.as_deref(),
                )
            } else {
                g.prepare_annotate(
                    &req.block_id,
                    &req.quote,
                    req.quote_tex.as_deref(),
                    &req.question,
                    is_check,
                    req.record_concept,
                )
            };
            g.emitter = Emitter::terminal();
            r
        };
        let (job, anchor) = match prepared {
            Ok(v) => v,
            Err(e) => {
                emit_result_channel(&tx, Err(e), &what, t0);
                return;
            }
        };
        let res = match LLM_GATE.try_lock() {
            Ok(_guard) => {
                let emitter = Emitter::channel(tx.clone());
                job.run(&emitter).await
            }
            Err(_) => Err(anyhow::anyhow!(LLM_BUSY_MSG)),
        };
        let mut g = app2.lock().await;
        g.emitter = Emitter::channel(tx.clone());
        let result = match res {
            Ok(res) => g.commit_annotate(job, anchor, res).map(|_| ()),
            Err(e) => {
                g.abort_ask(&job);
                Err(e)
            }
        };
        if result.is_ok() {
            let _ = g.auto_persist();
        }
        emit_result(&g.emitter, result, &what, t0);
        g.emitter = Emitter::terminal();
    });
    let stream = UnboundedReceiverStream::new(rx).map(|ev| Ok::<_, Infallible>(to_sse(ev)));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct AnnotateAnswerReq {
    /// 引用文字所在的对话节点（回答批注锚点）。
    node_id: String,
    quote: String,
    /// 给 LLM 的上下文（公式已还原成 TeX）；缺省回退 `quote`。
    #[serde(default)]
    quote_tex: Option<String>,
    question: String,
    #[serde(default)]
    mode: Option<String>,
    /// 是否把模型标注的 [[概念: …]] 写入「已学概念」（默认 true）。
    #[serde(default = "default_true")]
    record_concept: bool,
}

/// 回答批注：在某个回答里选中文字提问（新问答挂在该节点下，回答里高亮引用）。
async fn api_annotate_answer(
    State(app): State<SharedApp>,
    Json(req): Json<AnnotateAnswerReq>,
) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>> {
    let (tx, rx) = mpsc::unbounded_channel::<OutEvent>();
    let app2 = app.clone();
    tokio::spawn(async move {
        let is_check = matches!(req.mode.as_deref(), Some("check"));
        let what = format!("回答批注「{}」", req.question);
        let t0 = std::time::Instant::now();
        let prepared = {
            let mut g = app2.lock().await;
            g.emitter = Emitter::channel(tx.clone());
            let r = g.prepare_annotate_answer(
                &req.node_id,
                &req.quote,
                req.quote_tex.as_deref(),
                &req.question,
                is_check,
                req.record_concept,
            );
            g.emitter = Emitter::terminal();
            r
        };
        let (job, anchor) = match prepared {
            Ok(v) => v,
            Err(e) => {
                emit_result_channel(&tx, Err(e), &what, t0);
                return;
            }
        };
        let res = match LLM_GATE.try_lock() {
            Ok(_guard) => {
                let emitter = Emitter::channel(tx.clone());
                job.run(&emitter).await
            }
            Err(_) => Err(anyhow::anyhow!(LLM_BUSY_MSG)),
        };
        let mut g = app2.lock().await;
        g.emitter = Emitter::channel(tx.clone());
        let result = match res {
            Ok(res) => g.commit_annotate(job, anchor, res).map(|_| ()),
            Err(e) => {
                g.abort_ask(&job);
                Err(e)
            }
        };
        if result.is_ok() {
            let _ = g.auto_persist();
        }
        emit_result(&g.emitter, result, &what, t0);
        g.emitter = Emitter::terminal();
    });
    let stream = UnboundedReceiverStream::new(rx).map(|ev| Ok::<_, Infallible>(to_sse(ev)));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct AnnotateReplyReq {
    node_id: String,
    question: String,
    #[serde(default)]
    mode: Option<String>,
    /// 是否把模型标注的 [[概念: …]] 写入「已学概念」（默认 true）。
    #[serde(default = "default_true")]
    record_concept: bool,
}

async fn api_annotate_reply(
    State(app): State<SharedApp>,
    Json(req): Json<AnnotateReplyReq>,
) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>> {
    let (tx, rx) = mpsc::unbounded_channel::<OutEvent>();
    let app2 = app.clone();
    tokio::spawn(async move {
        let is_check = matches!(req.mode.as_deref(), Some("check"));
        let what = format!("批注追问「{}」", req.question);
        let t0 = std::time::Instant::now();
        let prepared = {
            let mut g = app2.lock().await;
            g.emitter = Emitter::channel(tx.clone());
            let r = g.prepare_annotate_reply(&req.node_id, &req.question, is_check, req.record_concept);
            g.emitter = Emitter::terminal();
            r
        };
        let job = match prepared {
            Ok(j) => j,
            Err(e) => {
                emit_result_channel(&tx, Err(e), &what, t0);
                return;
            }
        };
        let res = match LLM_GATE.try_lock() {
            Ok(_guard) => {
                let emitter = Emitter::channel(tx.clone());
                job.run(&emitter).await
            }
            Err(_) => Err(anyhow::anyhow!(LLM_BUSY_MSG)),
        };
        let mut g = app2.lock().await;
        g.emitter = Emitter::channel(tx.clone());
        let result = match res {
            Ok(res) => g.commit_ask(job, res).map(|_| ()),
            Err(e) => {
                g.abort_ask(&job);
                Err(e)
            }
        };
        if result.is_ok() {
            let _ = g.auto_persist();
        }
        emit_result(&g.emitter, result, &what, t0);
        g.emitter = Emitter::terminal();
    });
    let stream = UnboundedReceiverStream::new(rx).map(|ev| Ok::<_, Infallible>(to_sse(ev)));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct AnnotationDeleteReq {
    annotation_id: String,
}

async fn api_annotation_delete(
    State(app): State<SharedApp>,
    Json(req): Json<AnnotationDeleteReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut a = app.lock().await;
    a.emitter = Emitter::terminal();
    a.delete_annotation(&req.annotation_id)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e:#}")))?;
    let _ = a.auto_persist();
    Ok(Json(json!({ "ok": true })))
}

async fn api_annotations(State(app): State<SharedApp>) -> Json<serde_json::Value> {
    let a = app.lock().await;
    let summary_map = a
        .session
        .notes
        .as_ref()
        .map(|n| n.summary_map())
        .unwrap_or_default();
    let list: Vec<serde_json::Value> = a
        .session
        .annotations
        .iter()
        .map(|ann| {
            json!({
                "id": ann.id,
                "block_id": ann.block_id,
                "node_id": ann.node_id,
                "quote": ann.quote,
                "quote_tex": ann.quote_tex,
                "root_node_id": ann.root_node_id,
                "page": ann.page,
                "rects": ann.rects,
                "kind": ann.kind,
                "thread": build_thread(&a.session.conversation, &ann.root_node_id, &summary_map),
            })
        })
        .collect();
    Json(json!({ "annotations": list }))
}

/// 把以 `root` 为根的会话子树渲染成嵌套 JSON（`n` 为全局 DFS 编号，供 goto）。
/// `summary_map` 提供解释 id → (summary, collapsed)，用于把被 sum 的节点标成总结节点。
fn build_thread(
    conv: &crate::conversation::Conversation,
    root: &str,
    summary_map: &std::collections::HashMap<String, (Option<String>, bool)>,
) -> serde_json::Value {
    use std::collections::HashMap;
    let order = conv.dfs_order();
    let num_of: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i + 1))
        .collect();
    fn build(
        conv: &crate::conversation::Conversation,
        id: &str,
        num_of: &std::collections::HashMap<&str, usize>,
        summary_map: &std::collections::HashMap<String, (Option<String>, bool)>,
    ) -> serde_json::Value {
        let Some(n) = conv.nodes.iter().find(|x| x.id == id) else {
            return serde_json::Value::Null;
        };
        let children: Vec<serde_json::Value> = conv
            .nodes
            .iter()
            .filter(|x| x.parent.as_deref() == Some(id))
            .map(|c| build(conv, &c.id, num_of, summary_map))
            .collect();
        let (summary, collapsed) = n
            .explanation_id
            .as_ref()
            .and_then(|eid| summary_map.get(eid))
            .cloned()
            .unwrap_or((None, false));
        json!({
            "n": num_of.get(id).copied().unwrap_or(0),
            "node_id": n.id,
            "question": n.question,
            "quote": n.quote,
            "answer": n.answer,
            "is_check": n.explanation_id.is_none(),
            "summary": summary,
            "collapsed": collapsed,
            "children": children,
        })
    }
    build(conv, root, &num_of, summary_map)
}

// ===== 文件上传（Web 端导入论文文件） =====

/// 上传失败统一返回 JSON `{error}`，前端直接展示中文原因。
fn upload_err(
    code: StatusCode,
    msg: impl Into<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    (code, Json(json!({ "error": msg.into() })))
}

/// 上传：**流式写盘**（不再整文件读进内存，避免大文件 OOM/静默失败），
/// 超过 `MAX_UPLOAD_BYTES` 时删除半成品并返回明确中文错误。
async fn api_upload(
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| upload_err(StatusCode::BAD_REQUEST, format!("读取上传失败（连接可能中断）: {e}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let filename = field.file_name().unwrap_or("upload.pdf").to_string();
        paths::ensure_uploads_dir().map_err(|e| {
            upload_err(StatusCode::INTERNAL_SERVER_ERROR, format!("创建上传目录失败: {e:#}"))
        })?;
        let stamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let name = format!("{stamp}_{}", sanitize_upload_name(&filename));
        let path = paths::uploads_dir().join(&name);

        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| upload_err(StatusCode::INTERNAL_SERVER_ERROR, format!("创建文件失败: {e}")))?;
        let mut total: u64 = 0;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| upload_err(StatusCode::BAD_REQUEST, format!("读取上传数据失败: {e}")))?
        {
            total += chunk.len() as u64;
            if total > MAX_UPLOAD_BYTES {
                drop(file);
                let _ = tokio::fs::remove_file(&path).await;
                let limit_mb = MAX_UPLOAD_BYTES / 1024 / 1024;
                logging::warn(format!("上传被拒（超过 {limit_mb}MB）：{filename}"));
                return Err(upload_err(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!("文件超过 {limit_mb}MB 上限，已拒绝"),
                ));
            }
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .map_err(|e| {
                    upload_err(StatusCode::INTERNAL_SERVER_ERROR, format!("写入文件失败: {e}"))
                })?;
        }
        if total == 0 {
            drop(file);
            let _ = tokio::fs::remove_file(&path).await;
            return Err(upload_err(StatusCode::BAD_REQUEST, "文件为空"));
        }
        logging::info(format!(
            "上传完成：{filename}（{:.1}MB）→ {}",
            total as f64 / 1024.0 / 1024.0,
            path.display()
        ));
        return Ok(Json(json!({
            "path": path.to_string_lossy(),
            "name": filename,
            "size": total,
        })));
    }
    Err(upload_err(StatusCode::BAD_REQUEST, "缺少 file 字段"))
}

/// 上传文件名安全化：去掉路径分隔符与危险字符（保留空格/中文）。
fn sanitize_upload_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' | '\r' | '\0' => '_',
            _ => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() {
        "upload.pdf".to_string()
    } else {
        cleaned.chars().take(120).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一致性检查：`app.js` 里 `$("id")` 引用的元素，必须都在 `index.html` 里存在。
    /// 防止「新 app.js + 旧 index.html」这类版本错配导致初始化抛错、按钮全部失效。
    #[test]
    fn web_asset_element_ids_match() {
        let app_js = include_str!("../web/app.js");
        let index_html = include_str!("../web/index.html");
        let mut missing: Vec<String> = Vec::new();
        let mut rest = app_js;
        while let Some(pos) = rest.find("$(\"") {
            let after = &rest[pos + 3..];
            let Some(end) = after.find("\")") else { break };
            let id = &after[..end];
            // 显式动态创建的元素（app.js 里 innerHTML 生成 `id="..."`）同样有效
            let is_ident = !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if is_ident
                && !index_html.contains(&format!("id=\"{id}\""))
                && !app_js.contains(&format!("id=\"{id}\""))
            {
                if !missing.iter().any(|m| m == id) {
                    missing.push(id.to_string());
                }
            }
            rest = &after[end + 2..];
        }
        assert!(
            missing.is_empty(),
            "app.js 引用了 index.html 中不存在的元素 id（版本错配）: {missing:?}"
        );
    }

    /// 本地服务端点识别（Ollama 等无需 Key，不应被向导门禁拦住）。
    #[test]
    fn local_endpoint_detection() {
        assert!(is_local_endpoint("http://127.0.0.1:11434/v1/chat/completions"));
        assert!(is_local_endpoint("http://localhost:8080/v1/chat/completions"));
        assert!(is_local_endpoint("http://[::1]:11434/v1/chat/completions"));
        assert!(!is_local_endpoint("https://api.deepseek.com/v1/chat/completions"));
        assert!(!is_local_endpoint(""));
    }

    /// 端口顺延：占住一个端口后应从下一个可用端口启动，并返回原始请求端口。
    #[tokio::test]
    async fn bind_with_fallback_skips_busy_port() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let busy = listener.local_addr().unwrap().port();
        if busy > 65530 {
            return; // 端口太靠后，没有顺延空间
        }
        let (_l2, addr, requested) = bind_with_fallback(busy, 10).await.unwrap();
        assert_eq!(requested, busy);
        assert_ne!(addr.port(), busy, "被占端口不应复用");
        assert!(addr.port() > busy);
    }

    /// start=0 时由系统分配随机端口（打包版/测试常用）。
    #[tokio::test]
    async fn bind_with_fallback_supports_random_port() {
        let (_l, addr, requested) = bind_with_fallback(0, 1).await.unwrap();
        assert_eq!(requested, 0);
        assert_ne!(addr.port(), 0);
    }

    /// ShutdownHandle：trigger 后 wait 立即返回（用于 /api/shutdown 与桌面壳关窗）。
    #[tokio::test]
    async fn shutdown_handle_notifies_waiters() {
        let sh = Arc::new(ShutdownHandle::default());
        let sh2 = sh.clone();
        let h = tokio::spawn(async move { sh2.wait().await });
        sh.trigger();
        tokio::time::timeout(std::time::Duration::from_secs(2), h)
            .await
            .expect("wait 未被唤醒")
            .unwrap();
    }

    /// /vendor 嵌入表：关键资源齐全、MIME 正确（离线渲染公式的前提）。
    #[test]
    fn vendor_assets_embedded() {
        let find = |p: &str| vendor_assets::VENDOR_FILES.iter().find(|f| f.path == p);
        assert!(find("marked.min.js").is_some(), "缺少 marked");
        assert!(find("katex.min.js").is_some(), "缺少 katex js");
        assert_eq!(
            find("katex.min.css").expect("缺少 katex css").mime,
            "text/css; charset=utf-8"
        );
        assert_eq!(
            find("fonts/KaTeX_Main-Regular.woff2").expect("缺少字体").mime,
            "font/woff2"
        );
        assert!(find("marked.min.js").unwrap().bytes.len() > 10_000);
    }

    /// 示例材料已内嵌且格式正确（向导第四步的前提）。
    #[test]
    fn sample_assets_embedded() {
        assert!(SAMPLE_NOTE_MD.len() > 1000, "示例笔记为空");
        assert!(SAMPLE_PAPER_PDF.starts_with(b"%PDF-"), "示例论文不是 PDF");
        assert!(String::from_utf8_lossy(SAMPLE_NOTE_MD).contains('#'));
        let s = samples();
        assert_eq!(s.len(), 2);
        assert!(!s[0].needs_key && s[1].needs_key, "示例的 needs_key 标记不对");
    }

    /// 示例导入命令：md 走原样导入（0 token），pdf 走四段式生成。
    #[test]
    fn sample_command_variants() {
        let p = std::path::Path::new("/tmp/示例 笔记.md");
        assert!(sample_command(&samples()[0], p).starts_with("ingest --note --kind note --text \""));
        assert!(sample_command(&samples()[1], p).starts_with("ingest --style four --kind paper \""));
    }
}
