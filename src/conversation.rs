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

    /// 以树形（类似 `tree` 命令）渲染整棵对话树，当前节点用 * 标记。
    /// 每个节点带 `[n]` 编号（DFS 顺序，标号在缩进之后），可用 `goto n` 跳转。
    pub fn render_tree(&self) -> String {
        let order = self.dfs_order();
        if order.is_empty() {
            return "（对话树为空，先用 ask 提问）\n".to_string();
        }
        // id -> 1-based DFS 编号
        let mut num: HashMap<&str, usize> = HashMap::new();
        for (i, n) in order.iter().enumerate() {
            num.insert(n.id.as_str(), i + 1);
        }
        // parent -> children
        let mut children: HashMap<Option<&str>, Vec<&ConvNode>> = HashMap::new();
        for n in &self.nodes {
            children.entry(n.parent.as_deref()).or_default().push(n);
        }
        let mut out = String::new();
        self.render_sub(None, &children, &mut Vec::new(), &num, &mut out);
        out
    }

    fn render_sub(
        &self,
        parent: Option<&str>,
        children: &HashMap<Option<&str>, Vec<&ConvNode>>,
        prefix: &mut Vec<&'static str>,
        num: &HashMap<&str, usize>,
        out: &mut String,
    ) {
        let Some(sibs) = children.get(&parent) else {
            return;
        };
        let total = sibs.len();
        for (i, node) in sibs.iter().enumerate() {
            let is_last = i == total - 1;
            // 根级节点不带连接符；子级带 ├── / └──
            let conn = if parent.is_none() {
                ""
            } else if is_last {
                "└── "
            } else {
                "├── "
            };
            let head: String = prefix.iter().cloned().collect::<String>() + conn;
            let n_num = num.get(node.id.as_str()).copied().unwrap_or(0);
            let mark = if self.current.as_deref() == Some(node.id.as_str()) {
                " *"
            } else {
                ""
            };
            let tok = if node.input_tokens + node.output_tokens > 0 {
                format!("  [{}→{} tok ${:.4}]", node.input_tokens, node.output_tokens, node.cost)
            } else {
                String::new()
            };
            out.push_str(&format!("{}[{}] {}{}{}\n", head, n_num, node.label, mark, tok));
            // 下一层前缀：本层若不是最后一个孩子，则画竖线；否则留空
            let child_entry: &'static str = if is_last { "    " } else { "│   " };
            prefix.push(child_entry);
            self.render_sub(Some(node.id.as_str()), children, prefix, num, out);
            prefix.pop();
        }
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
}
