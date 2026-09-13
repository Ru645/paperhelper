# PaperHelper — 论文学习助手

读论文时遇到不懂的概念就追问，追问中又引出新的前置知识，等搞明白已经忘了主线？通用 AI 的对话只会向前延伸，几次深入追问后就丢失上下文。

PaperHelper 用**树形对话 + 笔记批注**解决这个问题：每次追问是树上的一个节点，随时跳回主线或其他分支继续提问；在笔记里**选中文字即可就地提问**，答案钉在原文位置，点击高亮即可回看。

主界面是**浏览器 Web 应用**（`paperhelper web`），也提供功能等价的**命令行界面（CLI）**。

## 核心功能

- **Web 界面（主）**：导入、阅读笔记、选中提问、管理会话、配置模型、导出，全部在浏览器里完成
- **笔记内就地提问**：在笔记里选中一段文字 → 点浮出的「提问」→ 小窗口内问答；答案与所选文字绑定，点击高亮即可重新展开
- **章节提问**：右键笔记标题（h1=全文，h2/h3=小节）→「对本章节提问」
- **递归嵌套批注**：对追问的回答再追问，批注层层嵌套；`sum` 把子树折叠成「总结」并可展开
- **导入 PDF → 结构化笔记**：LLM 按固定四段架构（问题 / 前人方案 / 本文方案 / 前景）生成详细 Markdown，含公式、表格、数值结果
- **跨论文知识库**：自动积累读过的论文与学过的概念，追问时自动关联已学知识
- **导出**：一键下载 Markdown / 思维导图（markmap）/ 自包含 HTML（KaTeX 公式渲染，含只读批注弹窗）
- **成本可控**：精确统计每次调用的 token 与成本，可设 token 预算，到上限自动中断
- **会话历史**：自动保存，可列表查看、置顶 / 重命名 / 删除、随时恢复
- **CLI（附带）**：功能等价的全键盘 REPL，适合脚本化与服务器环境

## 快速开始

### 1. 安装依赖

| 依赖 | 用途 | 必需性 |
|------|------|--------|
| Rust 1.85+ | 编译 | 必需 |
| Python 3 + PyMuPDF | PDF 文本层提取 | 必需 |
| tesseract + 中文语言包 | OCR 扫描件（`ingest --ocr`） | 可选（仅扫描版 PDF 需要） |

```bash
# ① Rust（需 1.85+）
rustc --version

# ② PDF 解析（Python 的 PyMuPDF，Rust 通过子进程调用）
pip install pymupdf

# ③ OCR（可选）
sudo apt install tesseract-ocr tesseract-ocr-chi-sim    # Debian/Ubuntu
brew install tesseract tesseract-lang                    # macOS
# 无 sudo 权限时可用 conda 装用户级：
# conda install -c conda-forge tesseract tesseract-data-chi_sim
```

> 没装 tesseract 时只有 `--ocr` 不可用，其余功能不受影响。

### 2. 编译

直接 clone 出来就是一个独立 Cargo crate，正常构建即可。若你的上级目录恰好是 Cargo workspace，则加 `-p paperhelper`：

```bash
git clone <仓库地址> && cd paperhelper
cargo build                 # 独立 crate 直接构建
cargo build -p paperhelper  # 在 workspace 下时加 -p
cargo test                  # 运行测试（32 个）
```

编译产物在 `./target/debug/paperhelper`。

### 3. 配置大模型

任选其一（优先级：环境变量 > `.env` > `.paperhelper/config.toml`）。

**方式 1：Web 配置弹窗（推荐）** —— 启动后点顶栏「配置」即可改 API Endpoint / Key / 模型 / 上下文长度 / 思考模式 / 单价 / 预算，即改即存。

**方式 2：`.env`（已被 gitignore，不会泄露）**
```bash
cat > .env <<'EOF'
PAPERHELPER_API_KEY=sk-xxxxxxxx
PAPERHELPER_API_ENDPOINT=https://api.deepseek.com/v1/chat/completions
PAPERHELPER_MODEL=deepseek-v4-pro
PAPERHELPER_CONTEXT_LENGTH=64000
EOF
```

**方式 3：CLI 交互式**
```bash
paperhelper
> config set llm.api_key sk-xxxxxxxx
> config set llm.model deepseek-v4-pro
> config set llm.api_endpoint https://api.deepseek.com/v1/chat/completions
```

> ⚠️ `api_endpoint` 必须是**完整 URL**（含 `/chat/completions`），不是文档里的 `base_url`。
> DeepSeek：`https://api.deepseek.com/v1/chat/completions`
> OpenAI：`https://api.openai.com/v1/chat/completions`

### 4. 启动 Web 界面（推荐）

```bash
paperhelper web              # 默认 http://127.0.0.1:8080
paperhelper web --port 9000  # 指定端口
```

浏览器打开后即可使用。在运行服务的终端按 `Ctrl-C` 停止服务。

## Web 界面详解

**顶栏**：`＋ 导入` · `导出 ▾`（Markdown / 思维导图 / HTML，浏览器下载）· `撤销` · `帮助` · `配置` · `刷新`，并显示当前模型。

**笔记区**（主区「笔记」标签）：
- **选中提问**：在笔记里选中一段文字 → 浮出「提问」→ 小窗口内输入问题（可切 ask / check）→ 回车发送；回答流式显示，并与所选文字**高亮绑定**，点击高亮可重新打开小窗口。
- **章节提问**：右键标题（h1=全文，h2/h3=小节）→「对本章节提问」；已有批注时另有「打开 / 删除」。
- **批注弹窗**：节点左键跳转、右键删除 / 总结；`sum` 后的节点显示为「总结」并可展开查看原对话。
- **右键高亮**：删除整条批注。

**主区标签**：固定「笔记」「控制台」；点击侧栏的论文 / 概念会新增并列小标签页（概念详情、论文笔记 + 对应会话）。

**侧栏**：左侧竖排**活动栏**（笔记结构 / 会话历史 / 已读论文 / 已学概念）切换右侧面板，同一时刻只显示一个；再点当前图标可收起 / 展开侧栏；拖动侧栏右缘可调整宽度（记忆在浏览器）。底部**常驻「用量」**：
- **用量**：会话总开销、跨会话累计、当前节点上下文 token（均为 API 返回的精确值）。
- **笔记结构**：笔记大纲，点击定位到笔记对应位置；有批注的块标 `●`。
- **会话历史**：顶部「＋ 新会话」；点条目加载；右键置顶 / 重命名 / 删除。
- **已读论文 / 已学概念**：点击查看详情；右键置顶 / 删除。

**导入**：点「＋ 导入」选择文件，或直接把 PDF / TXT 拖进窗口；上传后自动执行 ingest（并询问笔记文件名），无需手输命令。解析与生成笔记期间，顶部显示**流动进度条 + 实时已生成字数/耗时**，控制台同步流式输出 Markdown。

实现方式：`paperhelper web` 启动内嵌的 axum 服务，把业务输出抽象为 SSE 事件流（`src/output.rs` 的 `Emitter`），前端用原生 JS 消费。仅监听 `127.0.0.1`，不对外暴露。

## 命令行界面（CLI）

功能与 Web 等价，适合脚本化或服务器环境。

### 启动与恢复

```bash
paperhelper                 # 新会话
paperhelper -l              # 列出所有已保存会话（编号+标题）
paperhelper -s 20260909_020452   # 按编号恢复（支持唯一前缀，如 -s 20260909_02）
```

会话编号是**首次保存时的时间戳**，此后不变（每次 exit 覆盖保存同一编号）。

**Shell 补全（可选）**：`-s` 后 Tab 补全时间戳：
```bash
paperhelper --completions >> ~/.bashrc   # 安装 bash 补全
source ~/.bashrc
```

### 使用流程

```
> ingest samples/某论文.pdf       # 导入论文，生成笔记
请输入笔记导出文件名（回车默认 笔记_xxx.md）: my_note.md
⠋ 笔记生成中…
✓ 笔记已导出到 my_note.md

> blocks                         # 看笔记结构和编号
   1   § 一、要解决的问题
   1.1   § 背景
   3.2   § 变体一：BERTScore
   ...

> ask 3.2 BERTScore的公式里max_k是什么意思   # 按编号追问，解释插入 3.2 节
> ask 3.2 它和余弦相似度有什么区别            # 对上一个回答再追问，自动嵌套
> check 3.2 我觉得这就是余弦相似度，对吗      # 核对想法，不写入笔记
> sum                                        # 折叠当前追问子树为「总结」
> stats                                      # 看 token 用量和成本
> exit                                       # 退出，自动保存
✓ 会话已保存：BERTScore与余弦相似度讨论
  恢复会话，请执行：paperhelper -s 20260909_021633
```

### 导入选项

```bash
> ingest samples/论文.pdf           # 默认：PyMuPDF 提取文本
> ingest --text samples/论文.txt     # 直接读取文本文件（跳过 PDF 解析）
> ingest --ocr samples/扫描件.pdf    # OCR 识别（需安装 tesseract）
```

- `--text`：适合已用其他工具提取好文本的场景，或想手动修正 PDF 提取结果
- `--ocr`：适合扫描版 PDF（无文本层）；未装 tesseract 会给出安装指引，不影响其他功能

### REPL 操作与补全

- `↑↓` 切换历史命令，`←→` 移动光标，`Ctrl-C` 打断当前任务
- Tab 补全：命令名；`ingest/save/load/export` 补全文件路径；`goto` 补全节点编号；`ask/check` 补全笔记编号；`config set` 补全键名与常用值

## 命令一览（CLI）

| 命令 | 说明 |
|------|------|
| `ingest <pdf>` | 导入 PDF，生成结构化笔记（四段架构），自动导出 |
| `ingest --text <txt>` | 直接读取文本文件（跳过 PDF 解析） |
| `ingest --ocr <pdf>` | OCR 识别扫描件（需 tesseract） |
| `ask <编号> <问题>` | 按编号定位 Section 追问，解释插入笔记对应位置，递归嵌套 |
| `check <编号> <想法>` | 与 ask 类似但不写入笔记，用于核对理解 |
| `sum [n]` | 把节点 n（默认当前）的子树折叠（`<details>` 可展开）并替换为「总结」 |
| `del [n] [--yes]` | 删除节点 n（默认当前）及其子树（同时移除笔记中对应解释）；根节点不可删 |
| `undo` | 撤销上一次删除（内存多级，重启后失效） |
| `blocks` | 列出笔记结构（带层级编号） |
| `note` | 打印完整笔记 Markdown |
| `tree` | 以树形展示对话轨迹（带 `[n]` 编号，`*` 标记当前位置） |
| `goto <n>` | 跳到对话树节点 n，根路径成为上下文 |
| `stats` | 查看本次/累计 token 用量与成本 |
| `budget <n>` | 设置 token 预算（0=不限），到上限自动中断 |
| `export md\|mindmap\|html [file]` | 导出笔记；自动补后缀，省略文件名时用 ingest 时的笔记名 |
| `papers` / `concepts` | 列出已读论文 / 已学概念 |
| `save [file]` / `load <file>` | 保存 / 加载会话 |
| `new` | 新建会话 |
| `config show` / `config set <k> <v>` | 查看 / 设置配置 |
| `exit` | 退出（自动保存会话，告知恢复方式） |

## 配置项

| 配置项 | 说明 | 默认值 |
|--------|------|--------|
| `llm.api_key` | API 密钥 | 空 |
| `llm.api_endpoint` | 完整端点 URL | OpenAI |
| `llm.model` | 模型名 | gpt-4o-mini |
| `llm.context_length` | 上下文长度（token） | 8192 |
| `llm.thinking_mode` | 思考模式（给推理模型发 `reasoning_effort`） | false |
| `llm.pdf_input` | 声明模型支持 PDF 直传（file 模式，暂未实现均走文本） | false |
| `pricing.input_price_per_1m` | 输入单价 | 0.15 |
| `pricing.output_price_per_1m` | 输出单价 | 0.60 |
| `budget.token_budget` | token 预算（0=不限） | 0 |

配置存储在 `.paperhelper/config.toml`（已被 gitignore）。Web 端可直接在「配置」弹窗里修改。

## 数据目录

```
.paperhelper/
├── config.toml          # 配置文件
├── knowledge.json       # 跨论文知识库（论文+概念+累计用量）
├── prompts/             # 提示词模板（可编辑）
│   ├── ask.txt          # ask/check 的 system prompt
│   └── note.txt         # 笔记生成模板（{raw_text} 为论文占位符）
├── uploads/             # Web 端上传的论文文件
└── sessions/            # 会话存档（文件名 = 会话编号 = 首次保存时间戳）
    ├── 20260909_021633.json
    └── 20260909_033219.json
```

所有文件已被 `.gitignore` 忽略，不会泄露。

### 自定义提示词与补全预设

- **提示词**：直接编辑 `.paperhelper/prompts/ask.txt` 和 `note.txt`。`note.txt` 中 `{raw_text}` 会被替换为论文全文。改完重启生效；删除文件则恢复内置默认。
- **补全预设**：`config.toml` 的 `[presets]` 节可增删 `config set llm.model` / `llm.api_endpoint` 的 Tab 补全候选：

```toml
[presets]
models = ["deepseek-v4-pro", "deepseek-v4-flash", "gpt-4o-mini"]
endpoints = ["https://api.deepseek.com/v1/chat/completions", "https://api.openai.com/v1/chat/completions"]
```

## 技术架构

- **Rust 主控**：PDF 解析、笔记树、对话树、编号、定位、导出、统计——全部 Rust 确定性逻辑
- **LLM 只产自然语言**：生成笔记、回答追问、提取概念名、给会话取名、生成总结
- **两棵树**：笔记树（Section/Paragraph + 递归嵌套的 Explanation，`sum` 后折叠）+ 对话树（每节点一次 Q&A，跳转节点的根路径即上下文）
- **文本锚点批注**：批注记录 `block_id + 选中文字`，渲染时按文本引用定位并高亮，点击可重开弹窗
- **流式输出 + 进度**：LLM 输出实时渲染，耗时任务显示进度，`Ctrl-C` 可打断
- **Web / CLI 双前端**：业务输出经 `output::Emitter` 抽象，同一套代码分别写终端与 SSE（`src/web.rs` 为 axum 服务，前端原生 JS 内嵌）
- **健壮性**：网络抖动自动重试（2 次退避）、上下文超长自动截断早期对话、token 预算到上限自动中断
- **HTML 导出**：marked + KaTeX（CDN 带 npmmirror 备源），离线降级纯文本

## 开发

```bash
cargo build                 # 构建
cargo test                  # 32 个单元测试
cargo build --release       # 发布构建
```

二进制产物：`./target/debug/paperhelper`（或 `./target/release/paperhelper`）。
