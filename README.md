# 论文学习助手 Agent 选题报告

## 现实痛点

在我阅读论文时，通用对话 AI 暴露出了几个明显问题。

- 对话是线性的，而论文阅读的思路更类似于树形。读论文时遇到不懂的概念会去追问，追问中又可能引出新的前置知识，等搞明白之后，往往已经忘了自己原本读到哪一段、上下文是什么。通用 AI 的对话记录只会不断向前延伸，几次深入追问后很容易丢失主线。
- 通用 AI 无法建立跨论文的知识关联。连续阅读的论文往往会属于相近领域，会反复遇到相似的概念和方法。但对话式 AI 不存储我已经学过的知识，每次都是独立对话。当我读下一篇论文时，它无法提醒我某个概念在之前哪篇论文中出现过、和现在的内容有什么关系。我只能自己翻找以前的对话记录，或者干脆从头再来。
- 通用 AI 没法把解释插入到笔记的对应位置。当我针对论文中某一段或某个公式提问时，得到的回答只能追加在对话末尾，无法定位到原文附近。几天后回看，根本不知道某条解释对应的是哪一段。我需要反复手动整理上下文、把解释复制粘贴回原文，效率很低。

## 预期功能

我向 Agent 提供一个 PDF 论文，它能自动分析并生成一份有组织的笔记，把内容整理成章节、段落、公式等逻辑块。我阅读笔记时，可以针对任何不理解的概念或公式用自然语言追问，Agent 会在对应位置附近添加解释，而不是把回答丢在对话末尾。

对话不再是线性的，历史对话采用树的结构展示。我深入追问某个前置知识之后，可以返回主线，继续阅读和提问其他部分。Agent 会记录我已经学过的知识和读过的论文。当后续生成笔记或回答问题时，如果涉及已学过的内容或与其他论文相关，会自动标注关联，帮助我建立知识之间的联系。

学习完一篇论文后，Agent 可以将笔记导出为 Markdown 文件，方便线性阅读和编辑；也可以导出为思维导图格式，直观展示论文结构和学习路径。

## 预期实现

项目以 Rust 为主要语言，提供CLI界面。

核心流程是：用户输入 PDF 路径，Agent 解析 PDF 并生成json格式的结构化的笔记；用户通过 CLI 追问，Agent 调用可配置的大模型 API 生成解释，并将解释写入笔记的对应位置；用户可随时将笔记导出为 Markdown 或思维导图文件。

Rust 代码负责确定性部分：PDF 处理、笔记结构管理、定位用户提问对应的位置、记录用户已学过的概念和读过的论文、在生成笔记或回答时检索并标注关联知识、实现笔记到 Markdown 或思维导图的编译器、管理会话历史。LLM 只负责理解用户问题并生成自然语言内容。

## 编译

本项目位于上级 Cargo workspace（`../Cargo.toml` 把所有同级子目录作为成员）下，因此构建/测试**必须**带 `-p paperhelper`，否则会构建整个 workspace：

```bash
cargo build -p paperhelper            # 调试构建
cargo build -p paperhelper --release   # 发布构建
cargo test  -p paperhelper            # 运行单元测试
```

编译产物位于上级目录：`../target/debug/paperhelper`（或 `../target/release/paperhelper`）。

PDF 解析通过子进程调用 Python 的 PyMuPDF（符合「允许调用其他语言库」），首次使用前需安装：

```bash
pip install pymupdf
```

## 运行

### 配置大模型

以下三种方式任选其一（优先级：命令行配置 > .env > .paperhelper/config.toml）：

**方式一：交互式配置**（首次运行推荐）
```bash
paperhelper                       # 进入 REPL
> config set llm.api_key <你的key>
> config set llm.model deepseek-v4-pro
> config set llm.api_endpoint https://api.deepseek.com/v1/chat/completions
> config show                     # 查看（key 自动脱敏）
```

**方式二：.env 文件**（项目根目录，已被 .gitignore 忽略）
```bash
cat > .env <<'EOF'
PAPERHELPER_API_KEY=sk-xxxxxxxx
PAPERHELPER_API_ENDPOINT=https://api.deepseek.com/v1/chat/completions
PAPERHELPER_MODEL=deepseek-v4-pro
PAPERHELPER_CONTEXT_LENGTH=64000
EOF
```

**方式三：环境变量**（临时，不落盘）
```bash
export PAPERHELPER_API_KEY=sk-xxxxxxxx
export PAPERHELPER_API_ENDPOINT=https://api.deepseek.com/v1/chat/completions
export PAPERHELPER_MODEL=deepseek-v4-pro
```

可配置项一览（`config set <key> <value>` 或 .env 同名大写变量）：

| 配置项 | 说明 | 示例 |
|--------|------|------|
| `llm.api_key` | API 密钥 | `sk-...` |
| `llm.api_endpoint` | OpenAI 兼容端点 | `https://api.deepseek.com/v1/chat/completions` |
| `llm.model` | 模型名 | `deepseek-v4-pro` |
| `llm.context_length` | 上下文长度（token） | `64000` |
| `llm.thinking_mode` | 思考模式（reasoning_effort） | `true` |
| `pricing.input_price_per_1m` | 输入单价（$/百万token） | `0.27` |
| `pricing.output_price_per_1m` | 输出单价（$/百万token） | `1.10` |
| `budget.token_budget` | token 预算，0=不限 | `1000000` |

### 使用

```bash
paperhelper                       # 进入交互式 REPL
```

常用命令：

```text
> ingest samples/某论文.pdf          # 解析PDF → LLM生成Markdown笔记 → 树
> ask 这篇论文的核心方法是什么?      # 基于论文全文+笔记+对话历史回答
> blocks                            # 查看笔记结构（带序号）
> note                              # 打印完整笔记(Markdown)
> tree                              # 以文件树展示对话轨迹（带 [n] 编号）
> goto 2                            # 跳到节点2，其根路径成为对话上下文
> stats                             # token 用量 + 成本（本次/累计）
> budget 1000000                    # 设置 token 预算，到上限自动中断
> export md note.md                 # 导出笔记为 Markdown
> export mindmap note.mm            # 导出为思维导图（markmap 兼容）
> papers                            # 列出已读论文（跨会话累积）
> concepts                          # 列出已学概念（跨论文关联）
> save sess.json                    # 保存会话
> load sess.json                    # 加载会话
> new                               # 新建会话
> exit                              # 退出
```

也支持单次命令（不进 REPL）：`paperhelper ingest samples/某论文.pdf`。

> 提示：Ctrl-C 可打断耗时任务（PDF 解析、LLM 生成）。LLM 输出采用流式实时渲染，超长对话自动截断早期轮次以适配上下文长度。
