use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvNode {
    pub id: String,
    pub parent: Option<String>,
    pub role: Role,
    pub content: String,
    #[serde(default)]
    pub block_id: Option<String>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cost: f64,
    pub created_at: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Conversation {
    #[serde(default)]
    pub nodes: Vec<ConvNode>,
    #[serde(default)]
    pub current: Option<String>,
}

impl Conversation {
    pub fn add_node(&mut self, node: ConvNode) {
        self.nodes.push(node);
    }

    pub fn back(&mut self) {
        if let Some(cur) = &self.current {
            if let Some(node) = self.nodes.iter().find(|n| &n.id == cur) {
                self.current = node.parent.clone();
            }
        }
    }

    pub fn goto(&mut self, id: &str) -> bool {
        if self.nodes.iter().any(|n| n.id == id) {
            self.current = Some(id.to_string());
            true
        } else {
            false
        }
    }

    pub fn current_label(&self) -> &str {
        if let Some(cur) = &self.current {
            if let Some(node) = self.nodes.iter().find(|n| &n.id == cur) {
                return &node.label;
            }
        }
        "主线程"
    }

    /// 以 ASCII 树展示对话轨迹（含角色、token、成本，当前节点用 * 标记）。
    pub fn render_tree(&self) -> String {
        let mut children: HashMap<Option<String>, Vec<&ConvNode>> = HashMap::new();
        for n in &self.nodes {
            children.entry(n.parent.clone()).or_default().push(n);
        }
        let mut out = String::new();
        self.walk(None, 0, &children, &mut out);
        out
    }

    fn walk(
        &self,
        parent: Option<String>,
        depth: usize,
        children: &HashMap<Option<String>, Vec<&ConvNode>>,
        out: &mut String,
    ) {
        if let Some(nodes) = children.get(&parent) {
            for n in nodes {
                let prefix = "  ".repeat(depth);
                let mark = if self.current.as_deref() == Some(n.id.as_str()) {
                    "* "
                } else {
                    "- "
                };
                let id_short = n.id.get(..6).unwrap_or(&n.id);
                let role = match n.role {
                    Role::System => "系统",
                    Role::User => "用户",
                    Role::Assistant => "助手",
                };
                let tok = if n.input_tokens + n.output_tokens > 0 {
                    format!(" | {}→{} tok ${:.4}", n.input_tokens, n.output_tokens, n.cost)
                } else {
                    String::new()
                };
                out.push_str(&format!(
                    "{prefix}{mark}[{id_short}] {role}: {label}{tok}\n",
                    prefix = prefix,
                    mark = mark,
                    id_short = id_short,
                    role = role,
                    label = n.label,
                    tok = tok
                ));
                self.walk(Some(n.id.clone()), depth + 1, children, out);
            }
        }
    }
}
