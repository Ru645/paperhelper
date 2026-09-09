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
use crate::interrupt::is_interrupted;

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
        let result = client
            .post(&cfg.api_endpoint)
            .bearer_auth(&cfg.api_key)
            .json(&body)
            .send()
            .await;
        match result {
            Ok(resp) => return Ok(resp),
            Err(e) if is_retryable(&e) && attempt < MAX_RETRIES => {
                let wait = std::time::Duration::from_secs(1 << attempt); // 1s, 2s
                eprintln!(
                    "[网络抖动，{:.0}s 后重试 ({}/{}): {e}]",
                    wait.as_secs_f64(),
                    attempt + 1,
                    MAX_RETRIES,
                );
                tokio::time::sleep(wait).await;
            }
            Err(e) => {
                return Err(anyhow::Error::from(e)
                    .context(format!("请求 LLM 端点失败: {}", cfg.api_endpoint)));
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
    let mut resp = post_with_retry(client, cfg, build_body(cfg, messages, json_mode, thinking, true)).await?;

    // 兼容本地模型：若服务端不认 stream_options / response_format / reasoning_effort
    // (返回 400/422)，则去掉这些字段重试一次。
    if resp.status() == StatusCode::BAD_REQUEST || resp.status() == StatusCode::UNPROCESSABLE_ENTITY
    {
        resp = post_with_retry(client, cfg, build_body(cfg, messages, json_mode, thinking, false)).await?;
    }

    if !resp.status().is_success() {
        let st = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        bail!("LLM 返回错误 {st}: {txt}");
    }

    let mut content = String::new();
    let mut usage: Option<Usage> = None;
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(item) = stream.next().await {
        if is_interrupted() {
            bail!("任务已打断");
        }
        let bytes = item.context("读取响应流失败")?;
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

    Ok(LlmResult {
        content,
        input_tokens: in_tok,
        output_tokens: out_tok,
        estimated,
    })
}
