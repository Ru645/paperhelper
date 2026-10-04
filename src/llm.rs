//! OpenAI 兼容流式 LLM 客户端（DeepSeek / OpenAI / 本地模型均可）。
//!
//! 用法：`chat(client, cfg, messages, json_mode, thinking, on_token)`。
//! 要点：
//! - SSE 流式读取：逐 chunk 解析 `data:` 行，内容实时回调给 UI（进度渲染/打断）。
//!   `stream_options.include_usage` 让服务端在流末回传 usage → 精确统计 token。
//! - 兼容性降级：本地模型不认 response_format/reasoning_effort 等扩展字段时
//!   返回 400/422，去掉这些字段重试一次；无 usage 时报文长度估算并标记 estimated。
//! - 网络重试：连接/超时/请求错误退避重试（1s、2s），不重复计费（请求未达）。
//!   每次流块之间检查 `interrupt::is_interrupted()`，Ctrl-C 即时中断。

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::json;

use crate::config::LlmConfig;
use crate::interrupt;
use crate::logging;

/// 任务被用户中止（Ctrl-C / Web 停止按钮）。
#[derive(Debug)]
pub struct Interrupted;

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "任务已中止")
    }
}

impl std::error::Error for Interrupted {}

/// 判断错误链里是否含「用户中止」。
pub fn is_interrupted_error(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<Interrupted>().is_some())
}

/// 模型请求调用的一次工具（OpenAI function tool）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// 工具参数（JSON 字符串，流式分片累积而来）。
    #[serde(default)]
    pub arguments: String,
}

/// 一次对话消息（OpenAI roles: system / user / assistant / tool）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    /// 附加给本条消息的图片（data URL，如 `data:image/png;base64,...`）。
    /// 仅用于「PDF 页面提问」等在途请求，**不写入会话历史**；为空时按纯文本发送。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// assistant 消息里的工具调用（工具调用链路用；普通对话为空）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// role=tool 消息对应的调用 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    /// 纯文本消息。
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// 追加一张图片（data URL）。
    pub fn with_image(mut self, data_url: impl Into<String>) -> Self {
        self.images.push(data_url.into());
        self
    }

    /// 附加图片数量（估算 token 用）。
    pub fn image_count(&self) -> usize {
        self.images.len()
    }
}

/// 一次完成的 LLM 结果：正文 + 精确/估算的 token 计数与成本核算依据。
#[derive(Debug, Clone, Default)]
pub struct LlmResult {
    pub content: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// true 表示 usage 缺失、token 数由字符数估算（无 includes_usage 的本地模型）。
    pub estimated: bool,
    /// 流末的 finish_reason（如 "stop"/"length"）；`length` 表示输出达到上限被截断。
    pub finish_reason: Option<String>,
}

impl LlmResult {
    /// 输出是否因达到上限被截断。
    pub fn truncated(&self) -> bool {
        self.finish_reason.as_deref() == Some("length")
    }
}

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    delta: Option<Delta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Delta {
    content: Option<String>,
    /// 部分推理模型（如 DeepSeek/paratera）在流里单独回传思考过程。
    #[serde(default)]
    reasoning_content: Option<String>,
    /// 工具调用分片（OpenAI 流式：id/name 只在首帧，arguments 逐帧累积）。
    #[serde(default)]
    tool_calls: Vec<DeltaToolCall>,
}

#[derive(Deserialize)]
struct DeltaToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<DeltaFunction>,
}

#[derive(Deserialize)]
struct DeltaFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

/// 单条消息 → OpenAI JSON。带图片时 `content` 用多模态数组（text + image_url）。
fn message_json(m: &Message) -> serde_json::Value {
    if let Some(id) = &m.tool_call_id {
        return json!({ "role": m.role, "content": m.content, "tool_call_id": id });
    }
    if !m.tool_calls.is_empty() {
        let calls: Vec<serde_json::Value> = m
            .tool_calls
            .iter()
            .map(|tc| {
                json!({
                    "id": tc.id,
                    "type": "function",
                    "function": { "name": tc.name, "arguments": tc.arguments },
                })
            })
            .collect();
        return json!({ "role": m.role, "content": m.content, "tool_calls": calls });
    }
    if m.images.is_empty() {
        json!({ "role": m.role, "content": m.content })
    } else {
        let mut parts = vec![json!({ "type": "text", "text": m.content })];
        for url in &m.images {
            parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
        }
        json!({ "role": m.role, "content": parts })
    }
}

fn build_body(
    cfg: &LlmConfig,
    messages: &[Message],
    json_mode: bool,
    thinking: bool,
    extras: bool,
    tools: Option<&serde_json::Value>,
) -> serde_json::Value {
    let msgs: Vec<serde_json::Value> = messages.iter().map(message_json).collect();
    let mut body = json!({
        "model": cfg.model,
        "messages": msgs,
        "stream": true,
    });
    if extras {
        body["stream_options"] = json!({ "include_usage": true });
        if json_mode {
            body["response_format"] = json!({ "type": "json_object" });
        }
        if thinking {
            body["reasoning_effort"] = json!("medium");
        }
        if let Some(t) = tools {
            body["tools"] = json!([t.clone()]);
            body["tool_choice"] = json!("auto");
        }
    }
    body
}

/// 判断错误是否可安全重试（连接/超时类，请求未达服务端或可重发）。
fn is_retryable(e: &reqwest::Error) -> bool {
    e.is_connect() || e.is_timeout() || e.is_request()
}

/// 把 reqwest 网络错误翻译成可读原因。
fn friendly_net_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "请求超时（服务端长时间无响应）".to_string()
    } else if e.is_connect() {
        "建立连接失败（地址/端口不可达，或被网络、代理拦截）".to_string()
    } else if e.is_request() {
        "请求发送失败".to_string()
    } else {
        e.to_string()
    }
}

/// 把 HTTP 错误归类成「中文摘要 + 排查建议 + 原始响应」的错误链。
/// 摘要用于界面展示，`{e:#}` 的完整链包含原始响应，便于日志/详情排查。
fn friendly_http_error(status: StatusCode, body: &str, cfg: &LlmConfig) -> anyhow::Error {
    let code = status.as_u16();
    let lower = body.to_lowercase();
    let (summary, hint): (String, String) = match code {
        401 => (
            "API Key 无效或未授权".to_string(),
            "检查 llm.api_key 是否正确：Web「配置 → 测试连接」，或 `config set llm.api_key <key>`。"
                .to_string(),
        ),
        403 => (
            "API Key 无权限访问该模型".to_string(),
            "确认账号/Key 已开通该模型，或改用有权限的模型。".to_string(),
        ),
        404 => (
            "接口地址不存在（404）".to_string(),
            format!(
                "检查 llm.api_endpoint 是否为可直接 POST 的完整 URL（当前：{}），通常以 /chat/completions 结尾。",
                cfg.api_endpoint
            ),
        ),
        429 => (
            "请求过于频繁或额度不足（429）".to_string(),
            "稍后重试；若持续出现，检查账号余额 / 并发配额。".to_string(),
        ),
        400 | 422
            if lower.contains("context")
                || lower.contains("too long")
                || lower.contains("maximum") =>
        {
            let extra = if cfg.paper_relation != "concept" {
                format!(
                    "若开启了「论文关联增强」（llm.paper_relation={}），可改回 concept 以显著减少输入。",
                    cfg.paper_relation
                )
            } else {
                String::new()
            };
            (
                "输入超出模型上下文长度".to_string(),
                format!(
                    "减少输入（少带对话历史/缩短论文文本）或调大 llm.context_length（当前 {}）。{extra}",
                    cfg.context_length
                ),
            )
        }
        400 | 422 => (
            "请求参数被服务端拒绝（400）".to_string(),
            "可能模型不支持某参数（如思考模式/JSON 模式）；已自动降级重试一次。若仍失败，请检查模型名是否正确。"
                .to_string(),
        ),
        500..=599 => (
            format!("LLM 服务端错误（HTTP {code}）"),
            "服务端临时故障，稍后重试；持续失败请联系服务提供方。".to_string(),
        ),
        _ => (
            format!("LLM 请求失败（HTTP {code}）"),
            "检查端点 / 模型 / Key 配置：Web「配置 → 测试连接」。".to_string(),
        ),
    };
    let raw: String = body.chars().take(2000).collect();
    anyhow::anyhow!("原始响应：{raw}")
        .context(hint)
        .context(format!("{summary}（模型 {}）", cfg.model))
}

async fn post_with_retry(
    client: &reqwest::Client,
    cfg: &LlmConfig,
    body: serde_json::Value,
) -> Result<reqwest::Response> {
    if cfg.api_key.is_empty() {
        bail!(
            "API key 未设置。请运行 `config set llm.api_key <你的key>` \
             或在 .env 中设置 PAPERHELPER_API_KEY。"
        );
    }
    const MAX_RETRIES: usize = 2; // 共 1+2 次尝试
    for attempt in 0..=MAX_RETRIES {
        if interrupt::is_interrupted() {
            bail!(Interrupted);
        }
        let send = client
            .post(&cfg.api_endpoint)
            .bearer_auth(&cfg.api_key)
            .json(&body)
            .send();
        // 与打断信号竞争：即使服务端迟迟不响应也能立即中止
        let result = tokio::select! {
            r = send => r,
            _ = interrupt::wait() => bail!(Interrupted),
        };
        match result {
            Ok(resp) => return Ok(resp),
            Err(e) if is_retryable(&e) && attempt < MAX_RETRIES => {
                let wait = std::time::Duration::from_secs(1 << attempt); // 1s, 2s
                logging::warn(format!(
                    "网络抖动，{:.0}s 后重试 ({}/{}): {e}",
                    wait.as_secs_f64(),
                    attempt + 1,
                    MAX_RETRIES,
                ));
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = interrupt::wait() => bail!(Interrupted),
                }
            }
            Err(e) => {
                let reason = friendly_net_error(&e);
                logging::error(format!("连接 LLM 端点失败 {}: {e}", cfg.api_endpoint));
                let msg = format!(
                    "无法连接 LLM 端点（{}）：{reason}\n\
                     提示：检查本机网络/代理是否可用，并确认 api_endpoint 是完整 URL（含 /v1/chat/completions）。",
                    cfg.api_endpoint
                );
                return Err(anyhow::Error::from(e).context(msg));
            }
        }
    }
    unreachable!("重试循环必有限定次数")
}

fn estimate_tokens(s: &str) -> u64 {
    ((s.chars().count() as f64) / 4.0).ceil() as u64
}

fn estimate_input_tokens(messages: &[Message]) -> u64 {
    // 图片 token 依赖分辨率，这里按固定的每张约 1000 token 粗估（仅用于
    // 服务端不返回 usage 的本地模型；正常情况以 usage 为准）。
    const TOKENS_PER_IMAGE: u64 = 1000;
    messages
        .iter()
        .map(|m| estimate_tokens(&m.content) + m.image_count() as u64 * TOKENS_PER_IMAGE)
        .sum()
}

/// SSE 行缓冲：按**字节**累积，只在行边界做 UTF-8 解码。
///
/// 网络分片（`bytes_stream`）的边界是任意的，可能把一个多字节字符（如中文、
/// emoji）切成两半。若对每个分片直接 `from_utf8_lossy`，两半都会变成 `�`，
/// 这就是思考过程/流式正文出现乱码的根源。这里保证整行字节齐全后再解码。
#[derive(Default)]
struct LineBuffer {
    buf: Vec<u8>,
}

impl LineBuffer {
    /// 追加一段网络字节，返回其中已完整的所有行（已 trim，不含换行）。
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut lines = Vec::new();
        let mut start = 0;
        while let Some(pos) = self.buf[start..].iter().position(|&b| b == b'\n') {
            let end = start + pos;
            lines.push(String::from_utf8_lossy(&self.buf[start..end]).trim().to_string());
            start = end + 1;
        }
        if start > 0 {
            self.buf.drain(..start);
        }
        lines
    }

    /// 流结束时取出残留的最后一行（服务端未以换行结尾时）。
    fn finish(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            return None;
        }
        let line = String::from_utf8_lossy(&self.buf).trim().to_string();
        self.buf.clear();
        (!line.is_empty()).then_some(line)
    }
}

/// 处理一条 SSE 行：解析 `data:` JSON 并回调 token/思考分片。
fn handle_stream_line<F: FnMut(&str)>(
    line: &str,
    usage: &mut Option<Usage>,
    finish_reason: &mut Option<String>,
    content: &mut String,
    on_token: &mut F,
    on_reasoning: &mut Option<&mut (dyn FnMut(&str) + Send)>,
    tool_calls: &mut Vec<ToolCall>,
) {
    if line.is_empty() {
        return;
    }
    let Some(rest) = line.strip_prefix("data:") else {
        return;
    };
    let rest = rest.trim();
    if rest == "[DONE]" {
        return;
    }
    let Ok(chunk) = serde_json::from_str::<Chunk>(rest) else {
        return;
    };
    if let Some(u) = chunk.usage {
        *usage = Some(u);
    }
    for ch in chunk.choices {
        if let Some(fr) = ch.finish_reason {
            *finish_reason = Some(fr);
        }
        if let Some(d) = ch.delta {
            if let Some(r) = d.reasoning_content {
                if !r.is_empty() {
                    if let Some(cb) = on_reasoning.as_deref_mut() {
                        cb(&r);
                    }
                }
            }
            if let Some(t) = d.content {
                if !t.is_empty() {
                    on_token(&t);
                    content.push_str(&t);
                }
            }
            for tc in d.tool_calls {
                while tool_calls.len() <= tc.index {
                    tool_calls.push(ToolCall::default());
                }
                let slot = &mut tool_calls[tc.index];
                if let Some(id) = tc.id {
                    if !id.is_empty() {
                        slot.id = id;
                    }
                }
                if let Some(f) = tc.function {
                    if let Some(n) = f.name {
                        if !n.is_empty() {
                            slot.name = n;
                        }
                    }
                    if let Some(a) = f.arguments {
                        slot.arguments.push_str(&a);
                    }
                }
            }
        }
    }
}

/// 读取一次流式响应：回调 token/思考分片，累积正文与工具调用，解析 usage。
async fn read_stream(
    resp: reqwest::Response,
    messages: &[Message],
    on_token: &mut impl FnMut(&str),
    on_reasoning: &mut Option<&mut (dyn FnMut(&str) + Send)>,
) -> Result<(LlmResult, Vec<ToolCall>)> {
    let mut content = String::new();
    let mut usage: Option<Usage> = None;
    let mut finish_reason: Option<String> = None;
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut buf = LineBuffer::default();
    let mut stream = resp.bytes_stream();
    loop {
        if interrupt::is_interrupted() {
            bail!(Interrupted);
        }
        // 与打断信号竞争：服务端挂起不吐 token 时也能立即中止
        let next = tokio::select! {
            item = stream.next() => item,
            _ = interrupt::wait() => bail!(Interrupted),
        };
        let Some(item) = next else { break };
        let bytes = item.context("读取响应流失败（连接可能被中断）")?;
        for line in buf.push(&bytes) {
            handle_stream_line(
                &line,
                &mut usage,
                &mut finish_reason,
                &mut content,
                on_token,
                on_reasoning,
                &mut tool_calls,
            );
        }
    }
    // 个别服务端最后一帧不带换行，收尾时补处理
    if let Some(line) = buf.finish() {
        handle_stream_line(
            &line,
            &mut usage,
            &mut finish_reason,
            &mut content,
            on_token,
            on_reasoning,
            &mut tool_calls,
        );
    }

    let (in_tok, out_tok, estimated) = if let Some(u) = usage {
        (
            u.prompt_tokens.unwrap_or(0),
            u.completion_tokens.unwrap_or(0),
            false,
        )
    } else {
        (
            estimate_input_tokens(messages),
            estimate_tokens(&content),
            true,
        )
    };
    Ok((
        LlmResult {
            content,
            input_tokens: in_tok,
            output_tokens: out_tok,
            estimated,
            finish_reason,
        },
        tool_calls,
    ))
}

pub async fn chat(
    client: &reqwest::Client,
    cfg: &LlmConfig,
    messages: &[Message],
    json_mode: bool,
    thinking: bool,
    on_token: &mut impl FnMut(&str),
    on_reasoning: Option<&mut (dyn FnMut(&str) + Send)>,
) -> Result<LlmResult> {
    let started = std::time::Instant::now();
    logging::info(format!(
        "LLM 请求 → {} | model={} | {} 条消息 | thinking={} json={}",
        cfg.api_endpoint,
        cfg.model,
        messages.len(),
        thinking,
        json_mode
    ));

    let mut resp = post_with_retry(
        client,
        cfg,
        build_body(cfg, messages, json_mode, thinking, true, None),
    )
    .await?;

    // 兼容本地模型：若服务端不认 stream_options / response_format / reasoning_effort
    // (返回 400/422)，则去掉这些字段重试一次。
    if resp.status() == StatusCode::BAD_REQUEST || resp.status() == StatusCode::UNPROCESSABLE_ENTITY
    {
        logging::warn(format!(
            "LLM 返回 {}，去掉扩展字段（stream_options/response_format/reasoning_effort）重试一次",
            resp.status()
        ));
        resp = post_with_retry(
            client,
            cfg,
            build_body(cfg, messages, json_mode, thinking, false, None),
        )
        .await?;
    }

    if !resp.status().is_success() {
        let st = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        let err = friendly_http_error(st, &txt, cfg);
        logging::error(format!("LLM 失败 | model={} | {err:#}", cfg.model));
        return Err(err);
    }

    let mut on_reasoning = on_reasoning;
    let (res, _) = read_stream(resp, messages, on_token, &mut on_reasoning).await?;
    logging::info(format!(
        "LLM 完成 ← {} | model={} | 用时 {:.1}s | in={} out={} estimated={}",
        cfg.api_endpoint,
        cfg.model,
        started.elapsed().as_secs_f64(),
        res.input_tokens,
        res.output_tokens,
        res.estimated
    ));
    Ok(res)
}

/// 带工具的对话：模型可调用 `tool`，`execute` 把工具参数（JSON 字符串）落地为结果文本，
/// 回填后继续对话，最多 `MAX_TOOL_ROUNDS` 轮，返回最终 assistant 结果（token 已累加）。
///
/// 返回 `Ok(None)` 表示**服务端不支持工具调用**（首个请求返回 400/422），
/// 调用方应降级到不带工具的方案；其它错误按 `Err` 返回。
#[allow(clippy::too_many_arguments)]
pub async fn chat_with_tools(
    client: &reqwest::Client,
    cfg: &LlmConfig,
    messages: &[Message],
    tool: &serde_json::Value,
    thinking: bool,
    on_token: &mut impl FnMut(&str),
    on_reasoning: Option<&mut (dyn FnMut(&str) + Send)>,
    execute: &mut (dyn FnMut(&str) -> String + Send),
) -> Result<Option<LlmResult>> {
    const MAX_TOOL_ROUNDS: usize = 3;
    let mut msgs = messages.to_vec();
    let mut total_in = 0u64;
    let mut total_out = 0u64;
    let mut estimated = false;
    let mut on_reasoning = on_reasoning;
    logging::info(format!("工具调用：开始（最多 {MAX_TOOL_ROUNDS} 轮）"));
    for round in 1..=MAX_TOOL_ROUNDS {
        let body = build_body(cfg, &msgs, false, thinking, true, Some(tool));
        let resp = post_with_retry(client, cfg, body).await?;
        if resp.status() == StatusCode::BAD_REQUEST
            || resp.status() == StatusCode::UNPROCESSABLE_ENTITY
        {
            logging::warn("服务端不支持工具调用（400/422），降级为不带工具的方案");
            return Ok(None);
        }
        if !resp.status().is_success() {
            let st = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            let err = friendly_http_error(st, &txt, cfg);
            logging::error(format!("LLM 工具调用失败 | model={} | {err:#}", cfg.model));
            return Err(err);
        }
        let (res, calls) = read_stream(resp, &msgs, on_token, &mut on_reasoning).await?;
        total_in += res.input_tokens;
        total_out += res.output_tokens;
        estimated |= res.estimated;
        if calls.is_empty() {
            logging::info(format!("工具调用：第 {round} 轮模型未再请求工具，结束"));
            return Ok(Some(LlmResult {
                input_tokens: total_in,
                output_tokens: total_out,
                estimated,
                ..res
            }));
        }
        let summary = calls
            .iter()
            .map(|c| {
                let a: String = c.arguments.chars().take(200).collect();
                format!("{}({a})", c.name)
            })
            .collect::<Vec<_>>()
            .join("; ");
        logging::info(format!(
            "工具调用：第 {round} 轮收到 {} 个调用：{summary}",
            calls.len()
        ));
        let mut assistant = Message::text("assistant", res.content.clone());
        assistant.tool_calls = calls.clone();
        msgs.push(assistant);
        for c in &calls {
            let out = execute(&c.arguments);
            let mut tool_msg = Message::text("tool", out);
            tool_msg.tool_call_id = Some(c.id.clone());
            msgs.push(tool_msg);
        }
    }
    logging::warn(format!("工具调用：达到最大轮数 {MAX_TOOL_ROUNDS}，返回当前结果"));
    Ok(Some(LlmResult {
        input_tokens: total_in,
        output_tokens: total_out,
        estimated,
        ..Default::default()
    }))
}

/// 一次 LLM 配置连通性测试的结果（Web/CLI 共用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LlmTestResult {
    pub ok: bool,
    pub status: u16,
    pub latency_ms: u64,
    pub model: String,
    pub reply: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 原始响应体（成功为 JSON，失败为错误体），供界面/CLI 排查。
    pub raw: String,
}

/// 用当前配置发一条最小请求，验证端点/Key/模型是否可用。
/// 不打印错误到日志之外；返回结构化结果供界面展示原始响应。
pub async fn test(client: &reqwest::Client, cfg: &LlmConfig) -> LlmTestResult {
    let t0 = std::time::Instant::now();
    let mut result = LlmTestResult {
        ok: false,
        status: 0,
        latency_ms: 0,
        model: cfg.model.clone(),
        reply: String::new(),
        input_tokens: 0,
        output_tokens: 0,
        raw: String::new(),
    };
    if cfg.api_key.is_empty() {
        result.raw = "API key 未设置".to_string();
        return result;
    }
    let body = json!({
        "model": cfg.model,
        "messages": [{ "role": "user", "content": "ping" }],
        "max_tokens": 8,
        "stream": false,
    });
    let sent = client
        .post(&cfg.api_endpoint)
        .bearer_auth(&cfg.api_key)
        .json(&body)
        .send();
    let resp = tokio::select! {
        r = sent => r,
        _ = interrupt::wait() => {
            result.latency_ms = t0.elapsed().as_millis() as u64;
            result.raw = "已中止".to_string();
            return result;
        }
    };
    result.latency_ms = t0.elapsed().as_millis() as u64;
    match resp {
        Ok(r) => {
            let status = r.status();
            result.status = status.as_u16();
            let txt = r.text().await.unwrap_or_default();
            result.raw = txt.chars().take(4000).collect();
            if status.is_success() {
                match serde_json::from_str::<serde_json::Value>(&txt) {
                    Ok(v) => {
                        result.ok = true;
                        result.reply = v["choices"][0]["message"]["content"]
                            .as_str()
                            .unwrap_or("")
                            .trim()
                            .chars()
                            .take(200)
                            .collect();
                        result.input_tokens = v["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
                        result.output_tokens = v["usage"]["completion_tokens"].as_u64().unwrap_or(0);
                    }
                    Err(_) => {
                        result.raw = format!("HTTP {status} 响应不是合法 JSON：{txt}");
                    }
                }
            }
        }
        Err(e) => {
            result.raw = format!("请求失败：{}", friendly_net_error(&e));
        }
    }
    logging::info(format!(
        "LLM 配置测试：ok={} status={} 用时 {}ms model={}",
        result.ok, result.status, result.latency_ms, result.model
    ));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cfg() -> LlmConfig {
        crate::config::Config::default().llm
    }

    #[test]
    fn friendly_error_maps_auth_and_keeps_raw() {
        let e = friendly_http_error(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"Authentication Fails"}}"#,
            &test_cfg(),
        );
        let msg = format!("{e:#}");
        assert!(msg.contains("API Key 无效"), "{msg}");
        assert!(msg.contains("Authentication Fails"), "{msg}");
    }

    #[test]
    fn friendly_error_maps_context_length() {
        let e = friendly_http_error(
            StatusCode::BAD_REQUEST,
            "This model's maximum context length is 8192 tokens",
            &test_cfg(),
        );
        let msg = format!("{e:#}");
        assert!(msg.contains("上下文长度"), "{msg}");
    }

    #[test]
    fn friendly_error_maps_404_to_endpoint_hint() {
        let e = friendly_http_error(StatusCode::NOT_FOUND, "not found", &test_cfg());
        let msg = format!("{e:#}");
        assert!(msg.contains("接口地址不存在"), "{msg}");
    }

    #[test]
    fn interrupted_error_is_detected_through_context() {
        let e = anyhow::Error::new(Interrupted).context("外层上下文");
        assert!(is_interrupted_error(&e));
    }

    #[test]
    fn normal_error_is_not_treated_as_interrupt() {
        let e = anyhow::anyhow!("普通错误");
        assert!(!is_interrupted_error(&e));
    }

    #[test]
    fn chunk_parses_reasoning_content_and_plain_content() {
        // 推理模型：同一帧里可能只有 reasoning_content（还没有正文）
        let raw = r#"{"choices":[{"delta":{"reasoning_content":"先想一下","content":null},"finish_reason":null}]}"#;
        let ch: Chunk = serde_json::from_str(raw).unwrap();
        let d = ch.choices[0].delta.as_ref().unwrap();
        assert_eq!(d.reasoning_content.as_deref(), Some("先想一下"));
        assert!(d.content.is_none());

        // 普通模型/普通帧：没有 reasoning_content 字段也能解析
        let raw2 = r#"{"choices":[{"delta":{"content":"答"},"finish_reason":"stop"}]}"#;
        let ch2: Chunk = serde_json::from_str(raw2).unwrap();
        let d2 = ch2.choices[0].delta.as_ref().unwrap();
        assert_eq!(d2.content.as_deref(), Some("答"));
        assert!(d2.reasoning_content.is_none());
    }

    #[test]
    fn message_json_is_text_without_image_and_multimodal_with_image() {
        let plain = Message::text("user", "你好");
        let v = message_json(&plain);
        assert_eq!(v["content"], "你好");

        let with_img = Message::text("user", "看图").with_image("data:image/png;base64,AAA");
        let v = message_json(&with_img);
        let parts = v["content"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "看图");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AAA");
        assert_eq!(with_img.image_count(), 1);
    }

    #[test]
    fn estimate_input_tokens_counts_images() {
        let plain = vec![Message::text("user", "abcd")];
        let with_img = vec![Message::text("user", "abcd").with_image("data:image/png;base64,AAA")];
        assert!(estimate_input_tokens(&with_img) > estimate_input_tokens(&plain));
    }

    #[test]
    fn line_buffer_decodes_multibyte_at_line_boundary_only() {
        // 逐字节喂入中文 SSE：必须在行边界整体解码，绝不能出现替换字符
        let raw = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"先想一下\"}}]}\n";
        let mut buf = LineBuffer::default();
        let mut lines = Vec::new();
        for b in raw.as_bytes() {
            lines.extend(buf.push(std::slice::from_ref(b)));
        }
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("先想一下"), "{}", lines[0]);
        assert!(!lines[0].contains('\u{FFFD}'));
        assert!(buf.finish().is_none());
    }

    #[test]
    fn stream_lines_preserve_reasoning_and_content_across_chunks() {
        // 一帧思考 + 一帧正文 + usage，按 3 字节任意切分（必然切断多字节字符）
        let raw = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"先想一下：\"},\"finish_reason\":null}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"你好，世界🌍\"},\"finish_reason\":null}]}\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":34}}\n",
            "data: [DONE]\n"
        );
        let mut buf = LineBuffer::default();
        let mut usage = None;
        let mut finish_reason = None;
        let mut content = String::new();
        let mut tokens = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut on_token = |t: &str| tokens.push_str(t);
        let mut on_reasoning = |r: &str| reasoning.push_str(r);
        let mut on_reasoning_opt: Option<&mut (dyn FnMut(&str) + Send)> = Some(&mut on_reasoning);

        for part in raw.as_bytes().chunks(3) {
            for line in buf.push(part) {
                handle_stream_line(
                    &line,
                    &mut usage,
                    &mut finish_reason,
                    &mut content,
                    &mut on_token,
                    &mut on_reasoning_opt,
                    &mut tool_calls,
                );
            }
        }
        // 结尾补一帧不带换行的场景
        let tail = "data: {\"choices\":[{\"delta\":{\"content\":\"！\"}}]}";
        for line in buf.push(tail.as_bytes()) {
            handle_stream_line(
                &line,
                &mut usage,
                &mut finish_reason,
                &mut content,
                &mut on_token,
                &mut on_reasoning_opt,
                &mut tool_calls,
            );
        }
        if let Some(line) = buf.finish() {
            handle_stream_line(
                &line,
                &mut usage,
                &mut finish_reason,
                &mut content,
                &mut on_token,
                &mut on_reasoning_opt,
                &mut tool_calls,
            );
        }

        assert_eq!(reasoning, "先想一下：");
        assert_eq!(content, "你好，世界🌍！");
        assert_eq!(tokens, content);
        assert!(!content.contains('\u{FFFD}'), "{content}");
        let u = usage.expect("usage 应被解析");
        assert_eq!(u.prompt_tokens, Some(12));
        assert_eq!(u.completion_tokens, Some(34));
        assert_eq!(finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn stream_accumulates_tool_calls_by_index() {
        // id/name 只在首帧，arguments 跨帧拼接（模拟真实 OpenAI 流式）
        let raw = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"search_concepts\",\"arguments\":\"{\\\"que\"}}]},\"finish_reason\":null}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"ry\\\":\\\"注意力\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n",
            "data: [DONE]\n"
        );
        let mut usage = None;
        let mut finish_reason = None;
        let mut content = String::new();
        let mut tool_calls = Vec::new();
        let mut on_token = |_: &str| {};
        let mut on_reasoning: Option<&mut (dyn FnMut(&str) + Send)> = None;
        let mut buf = LineBuffer::default();
        for line in buf.push(raw.as_bytes()) {
            handle_stream_line(
                &line,
                &mut usage,
                &mut finish_reason,
                &mut content,
                &mut on_token,
                &mut on_reasoning,
                &mut tool_calls,
            );
        }
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].name, "search_concepts");
        assert_eq!(tool_calls[0].arguments, "{\"query\":\"注意力\"}");
        assert_eq!(finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn message_json_serializes_tool_calls_and_tool_result() {
        let call = ToolCall {
            id: "call_1".into(),
            name: "search_concepts".into(),
            arguments: "{\"query\":\"x\"}".into(),
        };
        let mut assistant = Message::text("assistant", "");
        assistant.tool_calls = vec![call.clone()];
        let v = message_json(&assistant);
        assert_eq!(v["tool_calls"][0]["id"], "call_1");
        assert_eq!(v["tool_calls"][0]["type"], "function");
        assert_eq!(v["tool_calls"][0]["function"]["name"], "search_concepts");

        let mut tool_msg = Message::text("tool", "结果");
        tool_msg.tool_call_id = Some("call_1".into());
        let v2 = message_json(&tool_msg);
        assert_eq!(v2["role"], "tool");
        assert_eq!(v2["tool_call_id"], "call_1");
    }

    #[test]
    fn build_body_includes_tools_only_with_extras() {
        let cfg = test_cfg();
        let msgs = vec![Message::text("user", "hi")];
        let tool = json!({ "type": "function", "function": { "name": "f" } });
        let with = build_body(&cfg, &msgs, false, false, true, Some(&tool));
        assert_eq!(with["tools"][0]["function"]["name"], "f");
        assert_eq!(with["tool_choice"], "auto");
        let without = build_body(&cfg, &msgs, false, false, false, Some(&tool));
        assert!(without.get("tools").is_none());
    }
}
