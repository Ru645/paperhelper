use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;

use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub llm: LlmConfig,
    pub pricing: PricingConfig,
    pub budget: BudgetConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub api_endpoint: String,
    pub api_key: String,
    pub model: String,
    pub context_length: usize,
    pub thinking_mode: bool,
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

impl Default for Config {
    fn default() -> Self {
        Config {
            llm: LlmConfig {
                api_endpoint: "https://api.openai.com/v1/chat/completions".into(),
                api_key: String::new(),
                model: "gpt-4o-mini".into(),
                context_length: 8192,
                thinking_mode: false,
            },
            pricing: PricingConfig {
                input_price_per_1m: 0.15,
                output_price_per_1m: 0.60,
            },
            budget: BudgetConfig {
                token_budget: 0,
            },
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let _ = dotenvy::dotenv();

        let path = paths::config_path();
        let mut cfg = if path.exists() {
            let s = fs::read_to_string(&path).context("读取 config.toml")?;
            toml::from_str(&s).context("解析 config.toml")?
        } else {
            Config::default()
        };

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
