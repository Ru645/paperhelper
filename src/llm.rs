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

/// 一次对话消息（OpenAI roles: system / user / assistant）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// 一次完成的 LLM 结果：正文 + 精确/估算的 token 计数与成本核算依据。
#[derive(Debug, Clone, Default)]
pub struct LlmResult {
    pub content: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// true 表示 usage 缺失、token 数由字符数估算（无 includes_usage 的本地模型）。
    pub estimated: bool,
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
}

#[derive(Deserialize)]
struct Delta {
    content: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

fn build_body(
    cfg: &LlmConfig,
    messages: &[Message],
    json_mode: bool,
    thinking: bool,
    extras: bool,
) -> serde_json::Value {
    let msgs: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect();
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
            (
                "输入超出模型上下文长度".to_string(),
                format!(
                    "减少输入（少带对话历史/缩短论文文本）或调大 llm.context_length（当前 {}）。",
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
    messages.iter().map(|m| estimate_tokens(&m.content)).sum()
}

pub async fn chat(
    client: &reqwest::Client,
    cfg: &LlmConfig,
    messages: &[Message],
    json_mode: bool,
    thinking: bool,
    on_token: &mut impl FnMut(&str),
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

    let mut resp = post_with_retry(client, cfg, build_body(cfg, messages, json_mode, thinking, true)).await?;

    // 兼容本地模型：若服务端不认 stream_options / response_format / reasoning_effort
    // (返回 400/422)，则去掉这些字段重试一次。
    if resp.status() == StatusCode::BAD_REQUEST || resp.status() == StatusCode::UNPROCESSABLE_ENTITY
    {
        logging::warn(format!(
            "LLM 返回 {}，去掉扩展字段（stream_options/response_format/reasoning_effort）重试一次",
            resp.status()
        ));
        resp = post_with_retry(client, cfg, build_body(cfg, messages, json_mode, thinking, false)).await?;
    }

    if !resp.status().is_success() {
        let st = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        let err = friendly_http_error(st, &txt, cfg);
        logging::error(format!("LLM 失败 | model={} | {err:#}", cfg.model));
        return Err(err);
    }

    let mut content = String::new();
    let mut usage: Option<Usage> = None;
    let mut buf = String::new();
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
        buf.push_str(&String::from_utf8_lossy(&bytes));
        loop {
            let Some(pos) = buf.find('\n') else {
                break;
            };
            let line: String = buf[..pos].trim().into();
            buf = buf[pos + 1..].to_string();
            if line.is_empty() {
                continue;
            }
            let Some(rest) = line.strip_prefix("data:") else {
                continue;
            };
            let rest = rest.trim();
            if rest == "[DONE]" {
                continue;
            }
            if let Ok(chunk) = serde_json::from_str::<Chunk>(rest) {
                if let Some(u) = chunk.usage {
                    usage = Some(u);
                }
                for ch in chunk.choices {
                    if let Some(d) = ch.delta {
                        if let Some(t) = d.content {
                            if !t.is_empty() {
                                on_token(&t);
                                content.push_str(&t);
                            }
                        }
                    }
                }
            }
        }
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

    logging::info(format!(
        "LLM 完成 ← {} | model={} | 用时 {:.1}s | in={} out={} estimated={}",
        cfg.api_endpoint,
        cfg.model,
        started.elapsed().as_secs_f64(),
        in_tok,
        out_tok,
        estimated
    ));

    Ok(LlmResult {
        content,
        input_tokens: in_tok,
        output_tokens: out_tok,
        estimated,
    })
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
}
