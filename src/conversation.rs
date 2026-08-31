use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 对话树的一个节点 = 一次问答交换（用户问 + 助手答）。
/// 跳到某节点时，从根到该节点的路径即为对话上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvNode {
    pub id: String,
    pub parent: Option<String>,
    pub question: String,
    pub answer: String,
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
    pub fn add_exchange(&mut self, node: ConvNode) {
        self.nodes.push(node);
    }

    pub fn goto(&mut self, id: &str) -> bool {
        if self.nodes.iter().any(|n| n.id == id) {
            self.current = Some(id.to_string());
            true
        } else {
            false
        }
    }

    /// 按 DFS 编号跳转（1-based）。返回跳转到的节点。
    pub fn goto_index(&mut self, idx: usize) -> Option<&ConvNode> {
        let target = self.dfs_order().get(idx.wrapping_sub(1))?.id.clone();
        self.current = Some(target.clone());
        self.nodes.iter().find(|n| n.id == target)
    }

    /// 按 id 前缀或完整 id 跳转。返回跳转到的节点。
    pub fn goto_prefix(&mut self, prefix: &str) -> Option<&ConvNode> {
        let target = self
            .nodes
            .iter()
            .find(|n| n.id == prefix || n.id.starts_with(prefix))?
            .id
            .clone();
        self.current = Some(target.clone());
        self.nodes.iter().find(|n| n.id == target)
    }

    pub fn current_label(&self) -> &str {
        if let Some(cur) = &self.current {
            if let Some(node) = self.nodes.iter().find(|n| &n.id == cur) {
                return &node.label;
            }
        }
        "（尚未开始对话）"
    }

    /// 从根到当前节点的路径（含当前节点），用于拼对话上下文。
    pub fn path_to_current(&self) -> Vec<&ConvNode> {
        let mut path = Vec::new();
        let mut cur = self.current.clone();
        while let Some(id) = cur {
            if let Some(node) = self.nodes.iter().find(|n| n.id == id) {
                path.push(node);
                cur = node.parent.clone();
            } else {
                break;
            }
        }
        path.reverse();
        path
    }

    /// 以文件树样式渲染整棵对话树，当前节点用 * 标记。
    /// 每个节点前带 `[n]` 编号（DFS 顺序），可用 `goto n` 跳转。
    pub fn render_tree(&self) -> String {
        let order = self.dfs_order();
        let mut out = String::new();
        if order.is_empty() {
            out.push_str("（对话树为空，先用 ask 提问）\n");
            return out;
        }
        for (i, n) in order.iter().enumerate() {
            let depth = self.depth_of(n.id);
            let prefix = if depth == 0 {
                String::new()
            } else {
                format!("{}  ", "│ ".repeat(depth - 1))
            };
            let branch = if depth == 0 { "" } else { "├─ " };
            let mark = if self.current.as_deref() == Some(n.id.as_str()) {
                " *"
            } else {
                ""
            };
            let id_short = n.id.get(..6).unwrap_or(&n.id);
            let tok = if n.input_tokens + n.output_tokens > 0 {
                format!("  [{}→{} tok ${:.4}]", n.input_tokens, n.output_tokens, n.cost)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "{prefix}{branch}[{}] ({}) {}{mark}{tok}\n",
                i + 1,
                id_short,
                n.label
            ));
        }
        out
    }

    /// DFS 顺序（根优先）的所有节点引用。
    pub fn dfs_order(&self) -> Vec<&ConvNode> {
        let mut children: HashMap<Option<String>, Vec<&ConvNode>> = HashMap::new();
        for n in &self.nodes {
            children.entry(n.parent.clone()).or_default().push(n);
        }
        let mut out = Vec::new();
        self.collect(None, &children, &mut out);
        out
    }

    fn collect<'a>(
        &self,
        parent: Option<String>,
        children: &HashMap<Option<String>, Vec<&'a ConvNode>>,
        out: &mut Vec<&'a ConvNode>,
    ) {
        if let Some(nodes) = children.get(&parent) {
            for n in nodes {
                out.push(n);
                self.collect(Some(n.id.clone()), children, out);
            }
        }
    }

    fn depth_of(&self, id: &str) -> usize {
        let mut depth = 0;
        let mut cur = Some(id.to_string());
        while let Some(cid) = cur {
            if let Some(node) = self.nodes.iter().find(|n| n.id == cid) {
                depth += 1;
                cur = node.parent.clone();
            } else {
                break;
            }
        }
        depth
    }
}
