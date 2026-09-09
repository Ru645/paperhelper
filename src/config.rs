//! 配置管理：加载、环境变量覆盖、持久化。
//!
//! 配置来源优先级：环境变量（PAPERHELPER_*）> `.env` > `.paperhelper/config.toml` > 内置默认。
//! 实现要点：
//! - `Config::load()` 依次尝试 dotenvy 注入环境、读 toml、再逐项用 env 覆盖
//! - `presets` 节是 Tab 补全候选（模型/端点），字段缺失或为空时回落内置默认，
//!   避免用户手改 toml 导致解析崩溃
//! - `Config::save()` 全量写回 toml（`config set` / `budget` 命令后调用）

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;

use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub llm: LlmConfig,
    pub pricing: PricingConfig,
    pub budget: BudgetConfig,
    /// 补全用的预设（模型名/端点候选），用户可在 config.toml 里增删。
    #[serde(default)]
    pub presets: PresetsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub api_endpoint: String,
    pub api_key: String,
    pub model: String,
    pub context_length: usize,
    pub thinking_mode: bool,
    /// 是否声明当前模型支持直接读取 PDF（file 模式）。
    /// false=使用 pdf-extract 抽纯文本塞入 prompt（text 模式，全模型通用）。
    #[serde(default)]
    pub pdf_input: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingConfig {
    pub input_price_per_1m: f64,
    pub output_price_per_1m: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    pub token_budget: u64, // 0 = unlimited
}

/// `config set` 的候选值预设（供 Tab 补全）。用户可在 .paperhelper/config.toml 增删。
/// 字段缺失或为空时回落到内置默认。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetsConfig {
    /// 模型名候选
    #[serde(default)]
    pub models: Vec<String>,
    /// API 端点候选
    #[serde(default)]
    pub endpoints: Vec<String>,
}

impl Default for PresetsConfig {
    fn default() -> Self {
        PresetsConfig {
            models: vec![
                "deepseek-v4-pro".into(),
                "deepseek-v4-flash".into(),
                "gpt-4o-mini".into(),
            ],
            endpoints: vec![
                "https://api.deepseek.com/v1/chat/completions".into(),
                "https://api.openai.com/v1/chat/completions".into(),
            ],
        }
    }
}

impl PresetsConfig {
    /// 缺失/为空的项回落默认（允许用户只自定义其中一组）。
    fn fill_missing_defaults(&mut self) {
        if self.models.is_empty() {
            self.models = PresetsConfig::default().models;
        }
        if self.endpoints.is_empty() {
            self.endpoints = PresetsConfig::default().endpoints;
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            llm: LlmConfig {
                api_endpoint: "https://api.openai.com/v1/chat/completions".into(),
                api_key: String::new(),
                model: "gpt-4o-mini".into(),
                context_length: 8192,
                thinking_mode: false,
                pdf_input: false,
            },
            pricing: PricingConfig {
                input_price_per_1m: 0.15,
                output_price_per_1m: 0.60,
            },
            budget: BudgetConfig {
                token_budget: 0,
            },
            presets: PresetsConfig::default(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let _ = dotenvy::dotenv();

        let path = paths::config_path();
        let mut cfg = if path.exists() {
            let s = fs::read_to_string(&path).context("读取 config.toml")?;
            toml::from_str::<Config>(&s)
                .with_context(|| format!("解析 config.toml 失败（文件可能被改坏，可删除 {} 恢复默认）", path.display()))?
        } else {
            Config::default()
        };
        // presets 字段缺失/为空时回落默认
        cfg.presets.fill_missing_defaults();

        if let Ok(v) = std::env::var("PAPERHELPER_API_KEY") {
            cfg.llm.api_key = v;
        }
        if let Ok(v) = std::env::var("PAPERHELPER_API_ENDPOINT") {
            cfg.llm.api_endpoint = v;
        }
        if let Ok(v) = std::env::var("PAPERHELPER_MODEL") {
            cfg.llm.model = v;
        }
        if let Ok(v) = std::env::var("PAPERHELPER_CONTEXT_LENGTH") {
            if let Ok(n) = v.parse() {
                cfg.llm.context_length = n;
            }
        }
        if let Ok(v) = std::env::var("PAPERHELPER_THINKING") {
            cfg.llm.thinking_mode = matches!(v.as_str(), "1" | "true" | "TRUE");
        }
        if let Ok(v) = std::env::var("PAPERHELPER_PDF_INPUT") {
            cfg.llm.pdf_input = matches!(v.as_str(), "1" | "true" | "TRUE");
        }
        if let Ok(v) = std::env::var("PAPERHELPER_INPUT_PRICE") {
            if let Ok(n) = v.parse() {
                cfg.pricing.input_price_per_1m = n;
            }
        }
        if let Ok(v) = std::env::var("PAPERHELPER_OUTPUT_PRICE") {
            if let Ok(n) = v.parse() {
                cfg.pricing.output_price_per_1m = n;
            }
        }
        if let Ok(v) = std::env::var("PAPERHELPER_TOKEN_BUDGET") {
            if let Ok(n) = v.parse() {
                cfg.budget.token_budget = n;
            }
        }

        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        paths::ensure_data_dir()?;
        let s = toml::to_string_pretty(self).context("序列化 config")?;
        fs::write(paths::config_path(), s).context("写入 config.toml")?;
        Ok(())
    }
}
