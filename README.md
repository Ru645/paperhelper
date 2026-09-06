# PaperHelper — 论文学习助手

读论文时遇到不懂的概念就追问，追问中又引出新的前置知识，等搞明白已经忘了主线？通用 AI 的对话只会向前延伸，几次深入追问后就丢失上下文。

PaperHelper 用**树形对话**解决这个问题：每次追问是树上的一个节点，随时跳回主线或其他分支继续提问，根路径自动成为上下文。

## 核心功能

- **导入 PDF → 自动生成结构化笔记**：LLM 按固定四段架构（问题/前人方案/本文方案/前景）生成详细 Markdown 笔记，含公式、表格、数值结果
- **按编号追问**：看笔记里的 `### 3.2 变体一：BERTScore`，直接 `ask 3.2 ...`，解释自动插入对应位置
- **递归嵌套批注**：对追问的回答再追问，批注层层嵌套，不会断开
- **核对想法**：`check 3.2 我觉得这本质就是余弦相似度，对吗`，LLM 回答但不写入笔记
- **对话树**：`tree` 查看全部对话轨迹，`goto 2` 跳到任意节点继续，根路径即上下文
- **跨论文知识库**：自动积累读过的论文和学过的概念，`concepts` 查看知识清单
- **会话自动保存**：退出时自动保存，`paperhelper -s 1` 直接恢复，对话位置不丢

## 快速开始

### 1. 安装依赖

```bash
# Rust（需 1.85+）
rustc --version

# PDF 解析依赖（Python 的 PyMuPDF，Rust 通过子进程调用）
pip install pymupdf
```

### 2. 编译

直接 clone 出来就是一个独立 Cargo crate，正常构建即可。如果你的上级目录恰好是 Cargo workspace（`../Cargo.toml` 把子目录当成员），则需加 `-p paperhelper`：

```bash
git clone <仓库地址> && cd paperhelper
cargo build                 # 独立 crate 直接构建
cargo build -p paperhelper   # 在 workspace 下时加 -p
cargo test                  # 运行测试（13 个）
```

编译产物在 `./target/debug/paperhelper`。

### 3. 加入 PATH（可选，方便直接用 `paperhelper` 命令）

```bash
# 把下面的 <项目路径> 替换为你实际的 paperhelper 目录
echo 'export PATH="<项目路径>/target/debug:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

不加 PATH 也可用 `cargo run -p paperhelper` 启动。

### 4. 配置大模型

首次启动会提示配置，三种方式任选其一（优先级：命令行 > .env > config.toml）：

**交互式配置**（推荐）：
```bash
paperhelper
> config set llm.api_key sk-xxxxxxxx
> config set llm.model deepseek-v4-pro
> config set llm.api_endpoint https://api.deepseek.com/v1/chat/completions
```

> ⚠️ `api_endpoint` 必须是**完整 URL**（含 `/chat/completions`），不是文档里的 `base_url`。
> DeepSeek：`https://api.deepseek.com/v1/chat/completions`
> OpenAI：`https://api.openai.com/v1/chat/completions`

或写 `.env`（已被 gitignore，不会泄露）：
```bash
cat > .env <<'EOF'
PAPERHELPER_API_KEY=sk-xxxxxxxx
PAPERHELPER_API_ENDPOINT=https://api.deepseek.com/v1/chat/completions
PAPERHELPER_MODEL=deepseek-v4-pro
PAPERHELPER_CONTEXT_LENGTH=64000
EOF
```

### 5. 开始使用

```bash
paperhelper                 # 新会话
paperhelper -l              # 列出所有已保存会话
paperhelper -s 1            # 恢复会话 1（对话位置自动恢复）
```

## 使用流程

```
> ingest samples/某论文.pdf       # 导入论文，生成笔记
请输入笔记导出文件名（回车默认 笔记_xxx.md）: my_note.md
⠋ 笔记生成中…
✓ 笔记已导出到 my_note.md

> blocks                         # 看笔记结构和编号
   1   § 一、要解决的问题
   1.1   § 背景
   2   § 二、前人方案及其不足
   3   § 三、本文方案及其优点
   3.1   § 核心思想
   3.2   § 变体一：BERTScore
   3.3   § 变体二：MQAG
   ...

> ask 3.2 BERTScore的公式里max_k是什么意思   # 按编号追问，解释插入 3.2 节
> ask 3.2 它和余弦相似度有什么区别            # 对上一个回答再追问，自动嵌套
> check 3.2 我觉得这就是余弦相似度，对吗      # 核对想法，不写入笔记
> goto 1                                     # 跳回根节点，开启新追问线
> tree                                       # 看对话树
[1] 导入《论文》
    └── [2] BERTScore
        └── [3] 余弦相似度区别
            └── [4] [核对] 余弦相似度 *
> concepts                                   # 看学过的概念
> stats                                      # 看 token 用量和成本
> exit                                       # 退出，自动保存
✓ 会话已保存：BERTScore与余弦相似度讨论
  恢复方式：paperhelper -s 3
```

### 导入选项

```bash
> ingest samples/论文.pdf           # 默认：PyMuPDF 提取文本
> ingest --text samples/论文.txt     # 直接读取文本文件（跳过PDF解析）
> ingest --ocr samples/扫描件.pdf    # OCR 识别（需安装 tesseract）
```

- `--text`：适合已用其他工具提取好文本的场景，或想手动修正 PDF 提取结果
- `--ocr`：适合扫描版 PDF（无文本层）。需安装 `tesseract-ocr` 和中文语言包（`apt install tesseract-ocr tesseract-ocr-chi-sim`）

### 推荐工作流：双开窗口

命令行看长笔记不方便，推荐双开：

1. **左窗口**：终端运行 `paperhelper`，对话提问
2. **右窗口**：用 Markdown 阅读器（如 VS Code 预览、Typora、Obsidian）打开导出的 `.md` 文件

每次 `ask` 后笔记自动同步更新，右窗口刷新即可看到新插入的追问解释。

## 命令一览

| 命令 | 说明 |
|------|------|
| `ingest <pdf>` | 导入 PDF，生成结构化笔记（四段架构），自动导出 Markdown |
| `ingest --text <txt>` | 直接读取文本文件（跳过 PDF 解析） |
| `ingest --ocr <pdf>` | OCR 识别扫描件（需 tesseract） |
| `ask <编号> <问题>` | 按编号定位 Section 追问，解释插入笔记对应位置，递归嵌套 |
| `check <编号> <想法>` | 与 ask 类似但不写入笔记，用于核对理解 |
| `blocks` | 列出笔记结构（带层级编号） |
| `note` | 打印完整笔记 Markdown 到终端 |
| `tree` | 以树形展示对话轨迹（带 `[n]` 编号，`*` 标记当前位置） |
| `goto <n>` | 跳到对话树节点 n，根路径成为上下文 |
| `stats` | 查看本次/累计 token 用量与成本 |
| `budget <n>` | 设置 token 预算（0=不限），到上限自动中断 |
| `export md\|mindmap <file>` | 导出笔记为 Markdown / 思维导图（markmap 兼容） |
| `papers` | 列出已读论文（跨会话累积） |
| `concepts` | 列出已学概念（跨论文关联，LLM 自动提取概念名） |
| `save [file]` | 手动保存会话 |
| `load <file>` | 加载会话 |
| `new` | 新建会话 |
| `config show` | 查看配置 |
| `config set <k> <v>` | 设置配置项 |
| `exit` | 退出（自动保存会话，告知恢复方式） |

**REPL 操作**：`↑↓` 切换历史命令，`←→` 移动光标，`Tab` 补全命令名，`Ctrl-C` 打断当前任务。

## 配置项

| 配置项 | 说明 | 默认值 |
|--------|------|--------|
| `llm.api_key` | API 密钥 | 空 |
| `llm.api_endpoint` | 完整端点 URL | OpenAI |
| `llm.model` | 模型名 | gpt-4o-mini |
| `llm.context_length` | 上下文长度（token） | 8192 |
| `llm.thinking_mode` | 思考模式 | false |
| `pricing.input_price_per_1m` | 输入单价 | 0.15 |
| `pricing.output_price_per_1m` | 输出单价 | 0.60 |
| `budget.token_budget` | token 预算（0=不限） | 0 |

配置存储在 `.paperhelper/config.toml`（已被 gitignore）。

## 数据目录

```
.paperhelper/
├── config.toml          # 配置文件
├── knowledge.json        # 跨论文知识库（论文+概念+累计用量）
└── sessions/             # 会话存档
    ├── counter           # ID 计数器
    ├── 1.json            # 会话 1
    └── 2.json            # 会话 2
```

所有文件已被 `.gitignore` 忽略，不会泄露。

## 技术架构

- **Rust 主控**：PDF 解析、笔记树、对话树、编号、定位、导出、统计——全部 Rust 确定性逻辑
- **LLM 只产自然语言**：生成笔记、回答追问、提取概念名、给会话取名
- **两棵树**：笔记树（Section/Paragraph + 递归 Explanation）+ 对话树（Q&A 节点，路径即上下文）
- **流式输出 + 进度条**：LLM 输出实时渲染，耗时任务显示进度，`Ctrl-C` 可打断

## 开发

```bash
cargo build -p paperhelper            # 构建
cargo test  -p paperhelper            # 13 个单元测试
cargo build -p paperhelper --release  # 发布构建
```

二进制产物：`../target/debug/paperhelper`
