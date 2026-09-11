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
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
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
use crate::{export, paths, session};

type SharedApp = Arc<Mutex<App>>;

/// 构建路由（抽成独立函数便于测试）。
pub fn router(app: SharedApp) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/style.css", get(stylesheet))
        .route("/app.js", get(script))
        .route("/api/run", post(api_run))
        .route("/api/interrupt", post(api_interrupt))
        .route("/api/state", get(api_state))
        .route("/api/note", get(api_note))
        .route("/api/config", get(api_config_get).post(api_config_set))
        .route("/api/sessions", get(api_sessions))
        .route("/api/sessions/load", post(api_session_load))
        .route("/api/sessions/save", post(api_session_save))
        .with_state(app)
}

/// 启动 Web 服务（仅监听 127.0.0.1，本机使用）。
pub async fn serve(app: App, port: u16) -> Result<()> {
    let shared: SharedApp = Arc::new(Mutex::new(app));
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("PaperHelper Web 已启动: http://{addr}");
    println!("（Ctrl-C 停止服务）");
    axum::serve(listener, router(shared)).await?;
    Ok(())
}

// ===== 静态前端 =====

async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../web/style.css"),
    )
}

async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        include_str!("../web/app.js"),
    )
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
        // 锁在整个命令期间持有：单用户串行，避免状态竞争
        let mut guard = app2.lock().await;
        guard.emitter = Emitter::channel(tx);
        if let Some(e) = req.export {
            if !e.trim().is_empty() {
                guard.export_path = Some(e);
            }
        }
        match guard.run_command(&req.command).await {
            Ok(()) => guard.emitter.done(),
            Err(e) => guard.emitter.error(format!("{e:#}")),
        }
        // 关键：恢复为终端输出器，丢弃 SSE sender，让接收端在 done 后正常结束流
        guard.emitter = Emitter::terminal();
    });

    let stream = UnboundedReceiverStream::new(rx).map(|ev| Ok::<_, Infallible>(to_sse(ev)));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// 把内部输出事件转成 SSE（data 用 JSON 字符串编码，避免换行破坏协议）。
fn to_sse(ev: OutEvent) -> SseEvent {
    let (name, data) = match ev {
        OutEvent::Stdout(s) => ("stdout", s),
        OutEvent::Stderr(s) => ("stderr", s),
        OutEvent::Token(s) => ("token", s),
        OutEvent::Progress(s) => ("progress", s),
        OutEvent::ProgressDone => ("progress_done", String::new()),
        OutEvent::Done => ("done", String::new()),
        OutEvent::Error(s) => ("error", s),
    };
    let data = serde_json::to_string(&data).unwrap_or_else(|_| "\"\"".to_string());
    SseEvent::default().event(name).data(data)
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

fn build_state(a: &App) -> serde_json::Value {
    let s = &a.session.stats;
    let g = &a.kb.stats;
    let used = s.total_tokens() + g.total_tokens();

    // 对话树（DFS 编号，供前端 goto）
    let order = a.session.conversation.dfs_order();
    let depth_of = |id: &str| -> usize {
        let mut d = 0usize;
        let mut cur = a
            .session
            .conversation
            .nodes
            .iter()
            .find(|n| n.id == id)
            .and_then(|n| n.parent.clone());
        while let Some(p) = cur {
            d += 1;
            cur = a
                .session
                .conversation
                .nodes
                .iter()
                .find(|n| n.id == p)
                .and_then(|n| n.parent.clone());
        }
        d
    };
    let tree: Vec<serde_json::Value> = order
        .iter()
        .enumerate()
        .map(|(i, n)| {
            json!({
                "n": i + 1,
                "label": n.label,
                "depth": depth_of(&n.id),
                "current": a.session.conversation.current.as_deref() == Some(n.id.as_str()),
                "input": n.input_tokens,
                "output": n.output_tokens,
            })
        })
        .collect();

    // 笔记结构块
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    if let Some(note) = &a.session.notes {
        for (b, depth) in note.flatten() {
            blocks.push(json!({
                "number": b.number,
                "kind": format!("{:?}", b.kind).to_lowercase(),
                "text": b.text.chars().take(80).collect::<String>(),
                "depth": depth,
                "explanations": b.explanations.len(),
            }));
        }
    }

    let papers: Vec<serde_json::Value> = a
        .kb
        .papers
        .iter()
        .map(|p| json!({ "title": p.title, "path": p.path }))
        .collect();
    let concepts: Vec<serde_json::Value> = a
        .kb
        .concepts
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "paper": c.paper_title,
                "definition": c.definition.chars().take(80).collect::<String>(),
            })
        })
        .collect();

    json!({
        "model": a.config.llm.model,
        "endpoint": a.config.llm.api_endpoint,
        "api_key_set": !a.config.llm.api_key.is_empty(),
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
        "export_path": a.export_path,
        "current": a.session.conversation.current_label(),
        "tree": tree,
        "blocks": blocks,
        "papers": papers,
        "concepts": concepts,
    })
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
        return (StatusCode::NOT_FOUND, "还没有笔记，请先 ingest 一篇论文").into_response();
    };
    let want_md = matches!(q.format.as_deref(), Some("md") | Some("markdown"));
    if want_md {
        (
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            export::to_markdown(note),
        )
            .into_response()
    } else {
        (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            export::to_html(note, &a.session.conversation),
        )
            .into_response()
    }
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

// ===== 会话历史 =====

async fn api_sessions() -> Json<serde_json::Value> {
    let ids = paths::list_sessions();
    let list: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| {
            json!({
                "id": id,
                "name": crate::session_name_of(id),
            })
        })
        .collect();
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
    let ids = paths::list_sessions();
    let target = ids
        .iter()
        .find(|s| *s == &req.id)
        .cloned()
        .or_else(|| {
            let hits: Vec<&String> = ids.iter().filter(|s| s.starts_with(&req.id)).collect();
            if hits.len() == 1 {
                Some(hits[0].clone())
            } else {
                None
            }
        })
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("会话不存在或不唯一: {}", req.id)))?;

    let path = paths::session_path(&target);
    let loaded =
        session::Session::load(&path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let mut a = app.lock().await;
    a.session = loaded;
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
