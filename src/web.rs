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
use axum::extract::{DefaultBodyLimit, Multipart, Query, State};
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
        .route("/api/sessions/pin", post(api_session_pin))
        .route("/api/sessions/rename", post(api_session_rename))
        .route("/api/sessions/delete", post(api_session_delete))
        .route("/api/concept", get(api_concept))
        .route("/api/paper", get(api_paper))
        .route("/api/paper/note", get(api_paper_note))
        .route("/api/kb/paper/pin", post(api_paper_pin))
        .route("/api/kb/paper/delete", post(api_paper_delete))
        .route("/api/kb/concept/pin", post(api_concept_pin))
        .route("/api/kb/concept/delete", post(api_concept_delete))
        .route("/api/annotate", post(api_annotate))
        .route("/api/annotate/reply", post(api_annotate_reply))
        .route("/api/annotations", get(api_annotations))
        .route(
            "/api/upload",
            post(api_upload).layer(DefaultBodyLimit::max(200 * 1024 * 1024)),
        )
        .with_state(app)
}

/// 启动 Web 服务（仅监听 127.0.0.1，本机使用）。
pub async fn serve(app: App, port: u16) -> Result<()> {
    let shared: SharedApp = Arc::new(Mutex::new(app));
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("PaperHelper Web 已启动: http://{addr}");
    println!("（Ctrl-C 停止服务）");

    // web 模式不安装 CLI 的 REPL 打断器，这里自行监听 Ctrl-C 并终止整个进程。
    // 直接 exit 以确保即使有浏览器 SSE 长连接也能立即退出（不做 graceful 等待）。
    tokio::spawn(async {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                break;
            }
            eprintln!("\n[收到 Ctrl-C，停止服务]");
            interrupt::request(); // 若有在途任务，先请求中止
            std::process::exit(0);
        }
    });

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
        let first = req.command.split_whitespace().next().unwrap_or("");
        let mutating = matches!(
            first,
            "ingest" | "pdf" | "ask" | "q" | "check" | "sum" | "del" | "rm" | "undo"
        );
        match guard.run_command(&req.command).await {
            Ok(()) => {
                if mutating {
                    let _ = guard.auto_persist();
                }
                guard.emitter.done();
            }
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
        "session_id": a.session.session_id,
        "export_path": a.export_path,
        "current": a.session.conversation.current_label(),
        "current_input_tokens": current_input_tokens,
        "context_length": a.config.llm.context_length,
        "can_undo": a.can_undo(),
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
    let (sid, sname, supd, question, answer, expl_id) = match hit {
        Some((m, qu, an, eid)) => (m.session_id, m.session_name, m.updated_at, qu, an, eid),
        None => (
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            None,
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
    question: String,
    /// "ask"（默认，写入笔记解释）或 "check"（只进批注线程）。
    #[serde(default)]
    mode: Option<String>,
}

async fn api_annotate(
    State(app): State<SharedApp>,
    Json(req): Json<AnnotateReq>,
) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>> {
    let (tx, rx) = mpsc::unbounded_channel::<OutEvent>();
    let app2 = app.clone();
    tokio::spawn(async move {
        let mut guard = app2.lock().await;
        guard.emitter = Emitter::channel(tx);
        let is_check = matches!(req.mode.as_deref(), Some("check"));
        match guard
            .annotate(&req.block_id, &req.quote, &req.question, is_check)
            .await
        {
            Ok(_) => {
                let _ = guard.auto_persist();
                guard.emitter.done();
            }
            Err(e) => guard.emitter.error(format!("{e:#}")),
        }
        guard.emitter = Emitter::terminal();
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
}

async fn api_annotate_reply(
    State(app): State<SharedApp>,
    Json(req): Json<AnnotateReplyReq>,
) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>> {
    let (tx, rx) = mpsc::unbounded_channel::<OutEvent>();
    let app2 = app.clone();
    tokio::spawn(async move {
        let mut guard = app2.lock().await;
        guard.emitter = Emitter::channel(tx);
        let is_check = matches!(req.mode.as_deref(), Some("check"));
        match guard
            .annotate_reply(&req.node_id, &req.question, is_check)
            .await
        {
            Ok(_) => {
                let _ = guard.auto_persist();
                guard.emitter.done();
            }
            Err(e) => guard.emitter.error(format!("{e:#}")),
        }
        guard.emitter = Emitter::terminal();
    });
    let stream = UnboundedReceiverStream::new(rx).map(|ev| Ok::<_, Infallible>(to_sse(ev)));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn api_annotations(State(app): State<SharedApp>) -> Json<serde_json::Value> {
    let a = app.lock().await;
    let list: Vec<serde_json::Value> = a
        .session
        .annotations
        .iter()
        .map(|ann| {
            json!({
                "id": ann.id,
                "block_id": ann.block_id,
                "quote": ann.quote,
                "root_node_id": ann.root_node_id,
                "thread": build_thread(&a.session.conversation, &ann.root_node_id),
            })
        })
        .collect();
    Json(json!({ "annotations": list }))
}

/// 把以 `root` 为根的会话子树渲染成嵌套 JSON（`n` 为全局 DFS 编号，供 goto）。
fn build_thread(conv: &crate::conversation::Conversation, root: &str) -> serde_json::Value {
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
    ) -> serde_json::Value {
        let Some(n) = conv.nodes.iter().find(|x| x.id == id) else {
            return serde_json::Value::Null;
        };
        let children: Vec<serde_json::Value> = conv
            .nodes
            .iter()
            .filter(|x| x.parent.as_deref() == Some(id))
            .map(|c| build(conv, &c.id, num_of))
            .collect();
        json!({
            "n": num_of.get(id).copied().unwrap_or(0),
            "node_id": n.id,
            "question": n.question,
            "answer": n.answer,
            "is_check": n.explanation_id.is_none(),
            "children": children,
        })
    }
    build(conv, root, &num_of)
}

// ===== 文件上传（Web 端导入论文文件） =====

async fn api_upload(
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("读取上传失败: {e}")))?
    {
        let fname = field.name().map(|s| s.to_string());
        if fname.as_deref() != Some("file") {
            continue;
        }
        let filename = field.file_name().unwrap_or("upload.pdf").to_string();
        let data = field
            .bytes()
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("读取文件内容失败: {e}")))?;
        if data.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "文件为空".to_string()));
        }
        paths::ensure_uploads_dir()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        let stamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let name = format!("{stamp}_{}", sanitize_upload_name(&filename));
        let path = paths::uploads_dir().join(&name);
        std::fs::write(&path, &data)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("保存文件失败: {e}")))?;
        return Ok(Json(json!({
            "path": path.to_string_lossy(),
            "name": filename,
        })));
    }
    Err((StatusCode::BAD_REQUEST, "缺少 file 字段".to_string()))
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
