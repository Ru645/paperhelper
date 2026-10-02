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
    /// 更新检查（旧配置缺 `[update]` 节时用默认：自动检查开、官方源）。
    #[serde(default)]
    pub update: UpdateConfig,
    /// 界面偏好（旧配置缺 `[ui]` 节时用默认）。
    #[serde(default)]
    pub ui: UiConfig,
}

/// 界面偏好（`[ui]` 节）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// 左侧栏开关快捷键，形如 `ctrl+b`；空字符串 = 不启用快捷键。
    /// 需至少包含一个 Ctrl / Alt / ⌘ 修饰键，末位为普通按键。
    #[serde(default = "default_sidebar_shortcut")]
    pub toggle_sidebar: String,
    /// 打开设置快捷键。
    #[serde(default = "default_settings_shortcut")]
    pub open_settings: String,
    /// 右侧提问栏开关快捷键。
    #[serde(default = "default_ask_shortcut")]
    pub toggle_ask: String,
    /// 对话树面板开关快捷键。
    #[serde(default = "default_tree_shortcut")]
    pub toggle_tree: String,
    /// 停止当前任务快捷键。
    #[serde(default = "default_stop_shortcut")]
    pub stop_task: String,
    /// 撤销快捷键。
    #[serde(default = "default_undo_shortcut")]
    pub undo: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            toggle_sidebar: default_sidebar_shortcut(),
            open_settings: default_settings_shortcut(),
            toggle_ask: default_ask_shortcut(),
            toggle_tree: default_tree_shortcut(),
            stop_task: default_stop_shortcut(),
            undo: default_undo_shortcut(),
        }
    }
}

fn default_sidebar_shortcut() -> String {
    "ctrl+b".into()
}

fn default_settings_shortcut() -> String {
    "ctrl+,".into()
}

fn default_ask_shortcut() -> String {
    "ctrl+alt+b".into()
}

fn default_tree_shortcut() -> String {
    "ctrl+alt+t".into()
}

fn default_stop_shortcut() -> String {
    "ctrl+.".into()
}

fn default_undo_shortcut() -> String {
    "ctrl+z".into()
}

/// 校验快捷键字符串：空串合法（表示不启用）；否则须为 `修饰键(+修饰键)*+按键`。
/// 修饰键仅允许 ctrl / alt / shift / meta，且至少一个、不重复；末位为具体按键（不能是修饰键）。
pub fn valid_shortcut(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return true;
    }
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    if parts.len() < 2 {
        return false;
    }
    let (mods, key) = parts.split_at(parts.len() - 1);
    let key = key[0];
    // 末键不能为空、也不能是修饰键本身（必须至少有一个真正的按键）
    if key.is_empty() || matches!(key.to_lowercase().as_str(), "ctrl" | "alt" | "shift" | "meta") {
        return false;
    }
    // 前置 token 只能是修饰键，且不能重复
    let mut seen = std::collections::HashSet::new();
    for m in mods {
        let m = m.to_lowercase();
        if !matches!(m.as_str(), "ctrl" | "alt" | "shift" | "meta") {
            return false;
        }
        if !seen.insert(m) {
            return false;
        }
    }
    true
}

/// 版本更新检查配置（`[update]` 节）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// 启动时自动检查新版本（24h 节流；失败静默，不影响使用）。
    #[serde(default = "default_true")]
    pub auto_check: bool,
    /// 更新源地址（update.json）；留空用官方 GitHub Releases，可指向镜像。
    #[serde(default)]
    pub source_url: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        UpdateConfig {
            auto_check: true,
            source_url: String::new(),
        }
    }
}

fn default_true() -> bool {
    true
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
    /// 论文关联增强：提问提及「另一篇已学论文」时如何纳入上下文。
    /// `concept`=只注入相关概念摘要（默认）；`note`=额外注入该论文整篇笔记；
    /// `full`=再额外注入该论文原文全文。后两档会显著增大每次提问的 token 消耗。
    #[serde(default = "default_paper_relation")]
    pub paper_relation: String,
}

/// 论文关联增强的合法取值。
pub const PAPER_RELATION_MODES: [&str; 3] = ["concept", "note", "full"];

pub fn valid_paper_relation(v: &str) -> bool {
    PAPER_RELATION_MODES.contains(&v)
}

fn default_paper_relation() -> String {
    "concept".into()
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
                paper_relation: "concept".into(),
            },
            pricing: PricingConfig {
                input_price_per_1m: 0.15,
                output_price_per_1m: 0.60,
            },
            budget: BudgetConfig {
                token_budget: 0,
            },
            presets: PresetsConfig::default(),
            update: UpdateConfig::default(),
            ui: UiConfig::default(),
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
        // 手改 toml 写坏的值回落默认，避免后续匹配落空
        if !valid_paper_relation(&cfg.llm.paper_relation) {
            cfg.llm.paper_relation = default_paper_relation();
        }
        // 手改 toml 写坏的快捷键回落默认（空串合法 = 不启用，保留）
        let ui_defaults = UiConfig::default();
        let ui = &mut cfg.ui;
        for (val, def) in [
            (&mut ui.toggle_sidebar, ui_defaults.toggle_sidebar),
            (&mut ui.open_settings, ui_defaults.open_settings),
            (&mut ui.toggle_ask, ui_defaults.toggle_ask),
            (&mut ui.toggle_tree, ui_defaults.toggle_tree),
            (&mut ui.stop_task, ui_defaults.stop_task),
            (&mut ui.undo, ui_defaults.undo),
        ] {
            if !valid_shortcut(val) {
                *val = def;
            }
        }

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
        if let Ok(v) = std::env::var("PAPERHELPER_PAPER_RELATION") {
            let v = v.trim();
            if valid_paper_relation(v) {
                cfg.llm.paper_relation = v.to_string();
            }
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
        // 更新源可用环境变量覆盖（镜像/内网/测试用）
        if let Ok(v) = std::env::var("PAPERHELPER_UPDATE_SOURCE") {
            if !v.trim().is_empty() {
                cfg.update.source_url = v.trim().to_string();
            }
        }
        if let Ok(v) = std::env::var("PAPERHELPER_AUTO_UPDATE") {
            cfg.update.auto_check = matches!(v.as_str(), "1" | "true" | "TRUE");
        }
        for (env_key, val) in [
            ("PAPERHELPER_UI_TOGGLE_SIDEBAR", &mut cfg.ui.toggle_sidebar),
            ("PAPERHELPER_UI_OPEN_SETTINGS", &mut cfg.ui.open_settings),
            ("PAPERHELPER_UI_TOGGLE_ASK", &mut cfg.ui.toggle_ask),
            ("PAPERHELPER_UI_TOGGLE_TREE", &mut cfg.ui.toggle_tree),
            ("PAPERHELPER_UI_STOP_TASK", &mut cfg.ui.stop_task),
            ("PAPERHELPER_UI_UNDO", &mut cfg.ui.undo),
        ] {
            if let Ok(v) = std::env::var(env_key) {
                let v = v.trim();
                if valid_shortcut(v) {
                    *val = v.to_string();
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧 config.toml 没有 paper_relation 字段时应回落到默认 concept。
    #[test]
    fn legacy_llm_config_defaults_paper_relation() {
        let llm: LlmConfig = toml::from_str(
            "api_endpoint = \"e\"\napi_key = \"\"\nmodel = \"m\"\ncontext_length = 8192\nthinking_mode = false\n",
        )
        .unwrap();
        assert_eq!(llm.paper_relation, "concept");
    }

    #[test]
    fn paper_relation_validates_modes() {
        assert!(valid_paper_relation("concept"));
        assert!(valid_paper_relation("note"));
        assert!(valid_paper_relation("full"));
        assert!(!valid_paper_relation("everything"));
        assert!(!valid_paper_relation(""));
    }

    /// 旧 config.toml 没有 [ui] 节时应回落到默认快捷键。
    #[test]
    fn legacy_config_defaults_sidebar_shortcut() {
        let cfg: Config = toml::from_str(
            "[llm]\napi_endpoint = \"e\"\napi_key = \"\"\nmodel = \"m\"\ncontext_length = 8192\nthinking_mode = false\n\n[pricing]\ninput_price_per_1m = 0\noutput_price_per_1m = 0\n\n[budget]\ntoken_budget = 0\n",
        )
        .unwrap();
        assert_eq!(cfg.ui.toggle_sidebar, "ctrl+b");
        assert_eq!(cfg.ui.open_settings, "ctrl+,");
        assert_eq!(cfg.ui.toggle_ask, "ctrl+alt+b");
        assert_eq!(cfg.ui.toggle_tree, "ctrl+alt+t");
        assert_eq!(cfg.ui.stop_task, "ctrl+.");
        assert_eq!(cfg.ui.undo, "ctrl+z");
    }

    /// 旧 [ui] 节只写了部分键：缺失的键取默认；非法值此处不校验（由 Config::load 回落）。
    #[test]
    fn partial_ui_config_fills_missing_defaults() {
        let ui: UiConfig = toml::from_str("toggle_sidebar = \"ctrl+j\"\nundo = \"乱写\"\n").unwrap();
        assert_eq!(ui.toggle_sidebar, "ctrl+j");
        assert_eq!(ui.toggle_ask, "ctrl+alt+b");
        // 反序列化不校验，校验发生在 Config::load；此处仅确认缺失项有默认
        assert_eq!(ui.undo, "乱写");
    }

    #[test]
    fn shortcut_validation() {
        // 空串合法 = 不启用快捷键
        assert!(valid_shortcut(""));
        assert!(valid_shortcut("   "));
        assert!(valid_shortcut("ctrl+b"));
        assert!(valid_shortcut("Ctrl+Shift+B"));
        assert!(valid_shortcut("alt+1"));
        assert!(valid_shortcut("meta+/"));
        assert!(valid_shortcut("ctrl+f12"));
        // 必须有非修饰末键、至少一个修饰键、不能重复
        assert!(!valid_shortcut("b"));
        assert!(!valid_shortcut("ctrl"));
        assert!(!valid_shortcut("b+ctrl"));
        assert!(!valid_shortcut("ctrl+ctrl+b"));
        assert!(!valid_shortcut("super+b"));
    }
}
