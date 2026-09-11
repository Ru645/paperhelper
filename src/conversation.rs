//! 对话树模型：记录每次问答在"哪个上下文下产生"。
//!
//! 与线性聊天不同，对话是树：节点 = 一次问答（ConvNode），`parent` 指向
//! 触发它的节点。`tree`/`goto` 让人跳回任意节点继续，从根到当前节点的路径
//! 就是下一次 ask 的对话上下文（见 app.rs `build_context_messages`）。
//! 节点还记录 `block_id`（定位笔记段落）与 `explanation_id`（该回答创建的
//! Explanation，check 节点为 None）；`explanation_ancestor` 沿父链跳过
//! check 找到最近的真实解释，供笔记嵌套定位使用。

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
    /// 本节点创建的 Explanation 的 id（用于关联追问嵌套）。
    #[serde(default)]
    pub explanation_id: Option<String>,
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

    /// 从 from 节点沿父链向上找第一个携带 explanation_id 的节点（跳过 check 等
    /// 无解释节点），返回其 explanation_id。用于笔记嵌套定位：check 节点的儿子，
    /// 其笔记中的父亲应是 check 往上第一个非 check 节点。
    pub fn explanation_ancestor(nodes: &[ConvNode], from: &str) -> Option<String> {
        let mut cur = Some(from.to_string());
        while let Some(id) = cur {
            let node = nodes.iter().find(|n| n.id == id)?;
            if let Some(e) = &node.explanation_id {
                return Some(e.clone());
            }
            cur = node.parent.clone();
        }
        None
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
    ///
    /// 实现方式：先建 (parent → children) 邻接表，再 DFS。渲染时用前缀栈画
    /// ├──/└── 连接线；非末位孩子的后续行补 `│   ` 竖线、末位补空格。
    /// 每行附 token 与成本（非 0 时），便于 `stats` 之外看单点开销。
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

    /// 删除以 `root` 为根的整棵子树（含自己），返回被删除节点的 id（DFS 序）。
    /// 若 `current` 落在被删子树内，则回退到被删根的父节点（根被删则为 None）。
    pub fn remove_subtree(&mut self, root: &str) -> Vec<String> {
        // 收集子树 id
        let mut ids: Vec<String> = Vec::new();
        let mut stack = vec![root.to_string()];
        while let Some(id) = stack.pop() {
            if !self.nodes.iter().any(|n| n.id == id) {
                continue;
            }
            ids.push(id.clone());
            for c in self.nodes.iter().filter(|n| n.parent.as_deref() == Some(id.as_str())) {
                stack.push(c.id.clone());
            }
        }
        // current 若在被删子树内，回退到根的父节点
        let parent_of_root = self
            .nodes
            .iter()
            .find(|n| n.id == root)
            .and_then(|n| n.parent.clone());
        if let Some(cur) = &self.current {
            if ids.iter().any(|x| x == cur) {
                self.current = parent_of_root;
            }
        }
        self.nodes.retain(|n| !ids.iter().any(|x| x == &n.id));
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(id: &str, parent: Option<&str>) -> ConvNode {
        ConvNode {
            id: id.into(),
            parent: parent.map(String::from),
            question: format!("Q{id}"),
            answer: format!("A{id}"),
            block_id: None,
            explanation_id: None,
            input_tokens: 0,
            output_tokens: 0,
            cost: 0.0,
            created_at: "t".into(),
            label: format!("L{id}"),
        }
    }

    #[test]
    fn remove_subtree_removes_descendants_and_resets_current() {
        let mut c = Conversation::default();
        c.add_exchange(mk("root", None));
        c.add_exchange(mk("a", Some("root")));
        c.add_exchange(mk("a1", Some("a")));
        c.add_exchange(mk("b", Some("root")));
        c.current = Some("a1".into());
        let removed = c.remove_subtree("a");
        assert_eq!(removed.len(), 2, "应删除 a 与 a1");
        assert!(c.nodes.iter().all(|n| n.id != "a" && n.id != "a1"));
        assert!(c.nodes.iter().any(|n| n.id == "b"), "兄弟节点应保留");
        assert_eq!(c.current.as_deref(), Some("root"), "current 应回退到 a 的父节点");
    }

    #[test]
    fn remove_subtree_of_root_clears_conversation() {
        let mut c = Conversation::default();
        c.add_exchange(mk("root", None));
        c.add_exchange(mk("a", Some("root")));
        c.current = Some("a".into());
        let removed = c.remove_subtree("root");
        assert_eq!(removed.len(), 2);
        assert!(c.nodes.is_empty());
        assert_eq!(c.current, None);
    }
}
