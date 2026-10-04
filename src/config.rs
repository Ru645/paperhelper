//! 配置管理：加载、环境变量覆盖、持久化。
//!
//! 配置来源优先级：环境变量（PAPERHELPER_*）> `.env` > `.paperhelper/config.toml` > 内置默认。
//! 实现要点：
//! - `Config::load()` 依次尝试 dotenvy 注入环境、读 toml、再逐项用 env 覆盖
//! - `presets` 节是 Tab 补全候选（模型/端点），字段缺失或为空时回落内置默认，
//!   避免用户手改 toml 导致解析崩溃
//! - `Config::save()` 全量写回 toml（`config set` / `budget` 命令后调用）

use anyhow::{bail, Context, Result};
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
    /// 提问时向 LLM 传递的笔记上下文档位：
    /// `block`=仅选中文字所在段落（右键小标题则为其整节）；`note`=整篇笔记（默认）；
    /// `full`=整篇笔记 + 论文原文。档位越高，每次提问的 token 消耗越大。
    #[serde(default = "default_context_scope")]
    pub context_scope: String,
    /// 概念检索代理：回答前先让模型检索/规划「相关概念」再作答。
    /// 优先尝试工具调用（模型自行按需检索）；网关不支持时自动降级为一次规划调用；
    /// 规划再失败则回落本地关键词检索。默认关闭（多一次调用、更耗 token）。
    #[serde(default)]
    pub kb_agent: bool,
}

/// 论文关联增强的合法取值。
pub const PAPER_RELATION_MODES: &[&str] = &["concept", "note", "full"];

fn default_paper_relation() -> String {
    "concept".into()
}

/// 提问上下文档位的合法取值。
pub const CONTEXT_SCOPE_MODES: &[&str] = &["block", "note", "full"];

fn default_context_scope() -> String {
    "note".into()
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

/// 字段的呈现/校验类型（供前端渲染与 CLI 补全派生）。
#[derive(Clone, Copy)]
pub enum FieldKind {
    Secret,
    Bool,
    Int,
    Float,
    Enum(&'static [&'static str]),
    Shortcut,
    Presets(PresetKind),
}

/// presets 候选来源。
#[derive(Clone, Copy, PartialEq)]
pub enum PresetKind {
    Models,
    Endpoints,
}

/// 单个配置项的单一来源定义：键名、类型、环境变量、默认值、展示与读写。
pub struct FieldDef {
    pub key: &'static str,
    pub kind: FieldKind,
    pub env: Option<&'static str>,
    pub default: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub help: &'static str,
    pub get: fn(&Config) -> String,
    pub set: fn(&mut Config, &str) -> Result<()>,
}

macro_rules! field_plain {
    ($kind:expr, $key:literal, $env:expr, $def:literal, $label:literal, $group:literal, $help:literal, $($f:ident).+) => {
        FieldDef {
            key: $key,
            kind: $kind,
            env: $env,
            default: $def,
            label: $label,
            group: $group,
            help: $help,
            get: |c: &Config| c.$($f).+.clone(),
            set: |c: &mut Config, v: &str| -> Result<()> {
                c.$($f).+ = v.to_string();
                Ok(())
            },
        }
    };
}

macro_rules! field_bool {
    ($key:literal, $env:expr, $def:literal, $label:literal, $group:literal, $help:literal, $($f:ident).+) => {
        FieldDef {
            key: $key,
            kind: FieldKind::Bool,
            env: $env,
            default: $def,
            label: $label,
            group: $group,
            help: $help,
            get: |c: &Config| c.$($f).+.to_string(),
            set: |c: &mut Config, v: &str| -> Result<()> {
                c.$($f).+ = parse_bool(v);
                Ok(())
            },
        }
    };
}

macro_rules! field_num {
    ($kind:expr, $key:literal, $env:expr, $def:literal, $label:literal, $group:literal, $help:literal, $ty:ty, $err:literal, $($f:ident).+) => {
        FieldDef {
            key: $key,
            kind: $kind,
            env: $env,
            default: $def,
            label: $label,
            group: $group,
            help: $help,
            get: |c: &Config| c.$($f).+.to_string(),
            set: |c: &mut Config, v: &str| -> Result<()> {
                c.$($f).+ = v.trim().parse::<$ty>().context($err)?;
                Ok(())
            },
        }
    };
}

macro_rules! field_enum {
    ($key:literal, $env:expr, $def:literal, $opts:expr, $label:literal, $group:literal, $help:literal, $($f:ident).+) => {
        FieldDef {
            key: $key,
            kind: FieldKind::Enum($opts),
            env: $env,
            default: $def,
            label: $label,
            group: $group,
            help: $help,
            get: |c: &Config| c.$($f).+.clone(),
            set: |c: &mut Config, v: &str| -> Result<()> {
                let v = v.trim();
                if !$opts.contains(&v) {
                    bail!("{} 只能是 {}", $key, $opts.join(" / "));
                }
                c.$($f).+ = v.to_string();
                Ok(())
            },
        }
    };
}

macro_rules! field_shortcut {
    ($key:literal, $env:expr, $def:literal, $label:literal, $help:literal, $($f:ident).+) => {
        FieldDef {
            key: $key,
            kind: FieldKind::Shortcut,
            env: $env,
            default: $def,
            label: $label,
            group: "ui",
            help: $help,
            get: |c: &Config| c.$($f).+.clone(),
            set: |c: &mut Config, v: &str| -> Result<()> {
                let v = v.trim();
                if !valid_shortcut(v) {
                    bail!("{} 需形如 ctrl+b（至少含一个 Ctrl/Alt/⌘ 修饰键），或留空表示不启用", $key);
                }
                c.$($f).+ = v.to_string();
                Ok(())
            },
        }
    };
}

/// 全部配置项的单一来源：CLI / Web API / 设置界面 / 环境变量 / 校验均由此派生。
/// 新增配置项只需：加结构体字段 + Default + 此表一行（需要时再加 1 处行为消费）。
pub const CONFIG_FIELDS: &[FieldDef] = &[
    field_plain!(FieldKind::Secret, "llm.api_key", Some("PAPERHELPER_API_KEY"), "", "API 密钥", "llm", "服务商控制台创建的密钥；只存本机，不会上传。", llm.api_key),
    field_plain!(FieldKind::Presets(PresetKind::Endpoints), "llm.api_endpoint", Some("PAPERHELPER_API_ENDPOINT"), "https://api.openai.com/v1/chat/completions", "API 端点", "llm", "服务商提供的接口地址，通常以 /chat/completions 结尾。", llm.api_endpoint),
    field_plain!(FieldKind::Presets(PresetKind::Models), "llm.model", Some("PAPERHELPER_MODEL"), "gpt-4o-mini", "模型名", "llm", "", llm.model),
    field_num!(FieldKind::Int, "llm.context_length", Some("PAPERHELPER_CONTEXT_LENGTH"), "8192", "上下文长度（token）", "llm", "", usize, "需要整数", llm.context_length),
    field_bool!("llm.thinking_mode", Some("PAPERHELPER_THINKING"), "false", "思考模式", "llm", "让推理模型在回答前先思考，通常更准，但更慢也更耗 token。", llm.thinking_mode),
    field_bool!("llm.pdf_input", Some("PAPERHELPER_PDF_INPUT"), "false", "PDF 直传", "llm", "让模型直接读取 PDF 原件（暂未启用，目前都按提取的文本处理）。", llm.pdf_input),
    field_enum!("llm.paper_relation", Some("PAPERHELPER_PAPER_RELATION"), "concept", PAPER_RELATION_MODES, "论文关联增强", "llm", "回答时是否参考其它相关论文：默认只补充相关概念；调高后还会带上关联论文的笔记，甚至原文，更全面但更耗 token。", llm.paper_relation),
    field_enum!("llm.context_scope", Some("PAPERHELPER_CONTEXT_SCOPE"), "note", CONTEXT_SCOPE_MODES, "提问上下文", "llm", "提问时发给模型的资料多少：只发选中段落最省；发整篇笔记更完整；连论文原文一起发最全也最费 token。", llm.context_scope),
    field_bool!("llm.kb_agent", Some("PAPERHELPER_KB_AGENT"), "false", "概念检索代理", "llm", "回答前先让模型自动挑选知识库里的相关概念（服务商不支持时自动改用本地匹配）；更准，但会多一次调用、更耗 token。", llm.kb_agent),
    field_num!(FieldKind::Float, "pricing.input_price_per_1m", Some("PAPERHELPER_INPUT_PRICE"), "0.15", "输入单价（/1M token）", "pricing", "", f64, "需要数字", pricing.input_price_per_1m),
    field_num!(FieldKind::Float, "pricing.output_price_per_1m", Some("PAPERHELPER_OUTPUT_PRICE"), "0.60", "输出单价（/1M token）", "pricing", "", f64, "需要数字", pricing.output_price_per_1m),
    field_num!(FieldKind::Int, "budget.token_budget", Some("PAPERHELPER_TOKEN_BUDGET"), "0", "token 预算", "budget", "0 = 不限。", u64, "需要整数", budget.token_budget),
    field_shortcut!("ui.toggle_sidebar", Some("PAPERHELPER_UI_TOGGLE_SIDEBAR"), "ctrl+b", "左侧栏开关", "留空 = 不启用。", ui.toggle_sidebar),
    field_shortcut!("ui.open_settings", Some("PAPERHELPER_UI_OPEN_SETTINGS"), "ctrl+,", "打开设置", "留空 = 不启用。", ui.open_settings),
    field_shortcut!("ui.toggle_ask", Some("PAPERHELPER_UI_TOGGLE_ASK"), "ctrl+alt+b", "提问栏开关", "留空 = 不启用。", ui.toggle_ask),
    field_shortcut!("ui.toggle_tree", Some("PAPERHELPER_UI_TOGGLE_TREE"), "ctrl+alt+t", "对话树开关", "留空 = 不启用。", ui.toggle_tree),
    field_shortcut!("ui.stop_task", Some("PAPERHELPER_UI_STOP_TASK"), "ctrl+.", "停止任务", "留空 = 不启用。", ui.stop_task),
    field_shortcut!("ui.undo", Some("PAPERHELPER_UI_UNDO"), "ctrl+z", "撤销", "留空 = 不启用。", ui.undo),
];

fn parse_bool(s: &str) -> bool {
    matches!(s.to_lowercase().as_str(), "1" | "true" | "yes" | "on")
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
                context_scope: "note".into(),
                kb_agent: false,
            },
            pricing: PricingConfig {
                input_price_per_1m: 0.15,
                output_price_per_1m: 0.60,
            },
            budget: BudgetConfig {
                token_budget: 0,
            },
            presets: PresetsConfig::default(),
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
        // 手改 toml 写坏的值（非法枚举/快捷键）回落默认，避免后续匹配落空
        cfg.normalize();
        // 环境变量覆盖（优先级最高）
        cfg.apply_env();

        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        paths::ensure_data_dir()?;
        let s = toml::to_string_pretty(self).context("序列化 config")?;
        fs::write(paths::config_path(), s).context("写入 config.toml")?;
        Ok(())
    }

    /// 读取单个配置项（未知键返回 None）。
    pub fn get_field(&self, key: &str) -> Option<String> {
        CONFIG_FIELDS.iter().find(|f| f.key == key).map(|f| (f.get)(self))
    }

    /// 写入单个配置项（未知键报错，含可设键清单）。
    pub fn set_field(&mut self, key: &str, val: &str) -> Result<()> {
        match CONFIG_FIELDS.iter().find(|f| f.key == key) {
            Some(f) => (f.set)(self, val),
            None => {
                let keys: Vec<&str> = CONFIG_FIELDS.iter().map(|f| f.key).collect();
                bail!("未知配置项: {key}。可设: {}", keys.join(" "))
            }
        }
    }

    /// 逐项用环境变量覆盖（非法值静默跳过，保留原值）。
    fn apply_env(&mut self) {
        for f in CONFIG_FIELDS {
            if let Some(env) = f.env {
                if let Ok(v) = std::env::var(env) {
                    let _ = (f.set)(self, &v);
                }
            }
        }
    }

    /// 非法枚举/快捷键回落各自默认（空快捷键合法 = 不启用，保留）。
    fn normalize(&mut self) {
        for f in CONFIG_FIELDS {
            let cur = (f.get)(self);
            let bad = match f.kind {
                FieldKind::Enum(opts) => !opts.contains(&cur.as_str()),
                FieldKind::Shortcut => !valid_shortcut(&cur),
                _ => false,
            };
            if bad {
                let _ = (f.set)(self, f.default);
            }
        }
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
        let mut c = Config::default();
        assert!(c.set_field("llm.paper_relation", "concept").is_ok());
        assert!(c.set_field("llm.paper_relation", "note").is_ok());
        assert!(c.set_field("llm.paper_relation", "full").is_ok());
        assert!(c.set_field("llm.paper_relation", "everything").is_err());
        assert!(c.set_field("llm.paper_relation", "").is_err());
    }

    /// 旧 config.toml 没有 context_scope 字段时应回落到默认 note。
    #[test]
    fn legacy_llm_config_defaults_context_scope() {
        let llm: LlmConfig = toml::from_str(
            "api_endpoint = \"e\"\napi_key = \"\"\nmodel = \"m\"\ncontext_length = 8192\nthinking_mode = false\n",
        )
        .unwrap();
        assert_eq!(llm.context_scope, "note", "默认应为中档（整篇笔记）");
    }

    #[test]
    fn context_scope_validates_modes() {
        let mut c = Config::default();
        assert!(c.set_field("llm.context_scope", "block").is_ok());
        assert!(c.set_field("llm.context_scope", "note").is_ok());
        assert!(c.set_field("llm.context_scope", "full").is_ok());
        assert!(c.set_field("llm.context_scope", "all").is_err());
        assert!(c.set_field("llm.context_scope", "").is_err());
    }

    #[test]
    fn config_field_keys_unique_and_defaults_valid() {
        let mut seen = std::collections::HashSet::new();
        for f in CONFIG_FIELDS {
            assert!(seen.insert(f.key), "重复键: {}", f.key);
            // 默认值必须能被 set 接受
            let mut c = Config::default();
            assert!((f.set)(&mut c, f.default).is_ok(), "默认值非法: {} = {}", f.key, f.default);
        }
    }

    #[test]
    fn config_field_set_get_roundtrip() {
        let mut c = Config::default();
        c.set_field("llm.context_length", "12345").unwrap();
        assert_eq!(c.get_field("llm.context_length").as_deref(), Some("12345"));
        c.set_field("llm.thinking_mode", "true").unwrap();
        assert_eq!(c.get_field("llm.thinking_mode").as_deref(), Some("true"));
        c.set_field("budget.token_budget", "999").unwrap();
        assert_eq!(c.get_field("budget.token_budget").as_deref(), Some("999"));
        c.set_field("ui.undo", "").unwrap();
        assert_eq!(c.get_field("ui.undo").as_deref(), Some(""));
    }

    #[test]
    fn config_field_unknown_key_errors() {
        let mut c = Config::default();
        assert!(c.set_field("llm.nope", "x").is_err());
        assert!(c.get_field("llm.nope").is_none());
    }

    #[test]
    fn normalize_resets_bad_enum_and_shortcut() {
        let mut c = Config::default();
        c.llm.paper_relation = "坏值".into();
        c.ui.undo = "没有修饰键".into();
        c.normalize();
        assert_eq!(c.llm.paper_relation, "concept");
        assert_eq!(c.ui.undo, "ctrl+z");
        // 合法值不动
        let mut c2 = Config::default();
        c2.llm.context_scope = "full".into();
        c2.normalize();
        assert_eq!(c2.llm.context_scope, "full");
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
