//! 内置 LLM 服务商预设：首启向导（`GET /api/presets`）与 CLI `config presets` 共用。
//!
//! 只是把官方 Endpoint / 模型 / 上下文 / 单价**预填**到配置表单，方便新手上手；
//! 不是代理或中转：用户仍用自己的 Key 从本机直连所选服务商。
//! 价格与上下文仅供参考，以服务商官网为准（用户可在向导/设置里改）。

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub name: &'static str,
    /// 完整 chat/completions URL（OpenAI 兼容）
    pub endpoint: &'static str,
    pub model: &'static str,
    pub context_length: usize,
    pub thinking: bool,
    pub input_price_per_1m: f64,
    pub output_price_per_1m: f64,
    /// 是否需要 API Key（本地 Ollama 不需要）
    pub needs_key: bool,
    /// 申请 / 管理 Key 的页面（本地服务为下载页）
    pub key_url: &'static str,
    /// 一句话说明（填入的只是预设值，可改）
    pub note: &'static str,
}

/// 全部内置预设（顺序即向导里的卡片顺序）。
pub fn all() -> Vec<ProviderPreset> {
    vec![
        ProviderPreset {
            id: "deepseek",
            name: "DeepSeek 官方",
            endpoint: "https://api.deepseek.com/v1/chat/completions",
            model: "deepseek-v4-pro",
            context_length: 500_000,
            thinking: false,
            input_price_per_1m: 0.15,
            output_price_per_1m: 0.60,
            needs_key: true,
            key_url: "https://platform.deepseek.com/api_keys",
            note: "注册后在「API Keys」页创建；单价为示例，以官网为准",
        },
        ProviderPreset {
            id: "paratera",
            name: "并行科技 Paratera",
            endpoint: "https://llmapi.paratera.com/v1/chat/completions",
            model: "DeepSeek-V4-Pro",
            context_length: 131_072,
            thinking: false,
            input_price_per_1m: 0.0,
            output_price_per_1m: 0.0,
            needs_key: true,
            key_url: "https://llmapi.paratera.com",
            note: "模型名可在控制台查看（如 DeepSeek-V4-Pro / DeepSeek-V4-Flash）；单价请按计费页填写",
        },
        ProviderPreset {
            id: "ollama",
            name: "Ollama 本地",
            endpoint: "http://127.0.0.1:11434/v1/chat/completions",
            model: "qwen2.5:7b",
            context_length: 32_768,
            thinking: false,
            input_price_per_1m: 0.0,
            output_price_per_1m: 0.0,
            needs_key: false,
            key_url: "https://ollama.com/download",
            note: "先安装并启动 Ollama（ollama serve），模型名用 `ollama list` 查看；Key 留空即可",
        },
        ProviderPreset {
            id: "custom",
            name: "自定义（OpenAI 兼容）",
            endpoint: "",
            model: "",
            context_length: 131_072,
            thinking: false,
            input_price_per_1m: 0.0,
            output_price_per_1m: 0.0,
            needs_key: true,
            key_url: "",
            note: "任何 OpenAI 兼容服务：填完整 /chat/completions 地址与模型名",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_well_formed() {
        let list = all();
        assert_eq!(list.len(), 4);
        for p in &list {
            assert!(!p.id.is_empty() && !p.name.is_empty() && !p.note.is_empty());
            assert!(p.context_length > 0);
            if !p.endpoint.is_empty() {
                assert!(
                    p.endpoint.starts_with("https://")
                        || p.endpoint.starts_with("http://127.0.0.1"),
                    "{} 端点不是完整 URL: {}",
                    p.id,
                    p.endpoint
                );
                assert!(p.endpoint.ends_with("/chat/completions"), "{} 缺少 /chat/completions", p.id);
            }
            if p.needs_key && p.id != "custom" {
                assert!(!p.key_url.is_empty(), "{} 需要 Key 但没有申请页面", p.id);
            }
            assert!(p.input_price_per_1m >= 0.0 && p.output_price_per_1m >= 0.0);
        }
        // id 唯一
        let mut ids: Vec<&str> = list.iter().map(|p| p.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), list.len());
    }
}
