# PaperHelper — 论文学习助手

读论文时遇到不懂的概念就追问，追问中又引出新的前置知识，等搞明白已经忘了主线？通用 AI 的对话只会向前延伸，几次深入追问后就丢失上下文。

PaperHelper 用**树形对话 + 笔记批注**解决这个问题：每次追问是树上的一个节点，随时跳回主线或其他分支继续提问；在笔记里**选中文字即可就地提问**，答案钉在原文位置，点击高亮即可回看。

主界面是**浏览器 Web 应用**（`paperhelper web`），也提供功能等价的**命令行界面（CLI）**。安装与启动见文末[「安装与部署」](#安装与部署)。

## 核心功能

- **Web 界面（主）**：导入、阅读笔记、选中提问、管理会话、配置模型、导出，全部在浏览器里完成
- **笔记内就地提问**：在笔记里选中一段文字 → 点浮出的「提问」→ 小窗口内问答；答案与所选文字绑定，点击高亮即可重新展开
- **章节提问**：右键笔记标题（h1=全文，h2/h3=小节）→「对本章节提问」
- **递归嵌套批注**：对追问的回答再追问，批注层层嵌套；`sum` 把子树折叠成「总结」并可展开
- **多种笔记风格**：导入时可选「四段式 / 逐段翻译（忠实原文）/ 自由笔记（结构自定）」；翻译为单次整篇，输出被截断会明确提示
- **编辑与 AI 改写**：悬停笔记块点「✎ 编辑」即可改文字；也可让 AI 重写 / 补充某段，或手动插入内容、删除整块（普通编辑不换块 id，追问与批注不受影响）；**所有编辑/删除都能用顶栏「撤销」回退**（内存多级，最多 20 步）
- **导入 PDF → 结构化笔记**：LLM 按固定四段架构（问题 / 前人方案 / 本文方案 / 前景）生成详细 Markdown，含公式、表格、数值结果
- **跨论文知识库**：自动积累读过的论文与学过的概念，追问时自动关联已学知识
- **导出**：一键下载 Markdown / 思维导图（markmap）/ 自包含 HTML（KaTeX 公式渲染，含只读批注弹窗）
- **成本可控**：精确统计每次调用的 token 与成本，可设 token 预算，到上限自动中断
- **会话历史**：自动保存，可列表查看、置顶 / 重命名 / 删除、随时恢复
- **随时中止**：导入 / 提问 / 批注等长任务，Web 端点「停止」、CLI 按 `Ctrl-C` 立即打断（含卡住的网络请求与 PDF / OCR 子进程）
- **可排查的错误**：后端错误归类成中文提示（Key 无效 / 端点错 / 限流 / 超上下文…），可展开「详情」查看原始 API Response；配置弹窗内置「测试连接」
- **运行日志**：启动、HTTP 请求、LLM 调用、上传、命令执行都写入 stderr 与 `.paperhelper/logs/paperhelper.log`
- **CLI（附带）**：功能等价的全键盘 REPL，适合脚本化与服务器环境

## Web 界面详解

```bash
paperhelper web              # 默认 http://127.0.0.1:8080
paperhelper web --port 9000  # 指定端口
```

浏览器打开后即可使用。在运行服务的终端按 `Ctrl-C` 停止服务。

![PaperHelper Web 界面](screenshots/ui.png)

**顶栏**：`导出 ▾`（Markdown / 思维导图 / HTML，浏览器下载）· `撤销` · `帮助` · `配置` · `刷新`，并显示当前模型。

**笔记区**（主区「笔记」标签）：
- **选中提问**：在笔记里选中一段文字 → 浮出「提问」→ 小窗口内输入问题（可切 ask / check）→ 回车发送；回答流式显示，并与所选文字**高亮绑定**，点击高亮可重新打开小窗口。
- **章节提问**：右键标题（h1=全文，h2/h3=小节）→「对本章节提问」；已有批注时另有「打开 / 删除」。
- **批注弹窗**：节点左键跳转、右键删除 / 总结；`sum` 后的节点显示为「总结」并可展开查看原对话；弹窗可**按住顶部标题栏拖动**（不记忆位置，下次仍出现在选区旁）。
- **右键高亮**：删除整条批注。

**主区标签**：固定「笔记」「控制台」；点击侧栏的论文 / 概念会新增并列小标签页（概念详情、论文笔记 + 对应会话）。

**侧栏**：左侧竖排**活动栏**（笔记结构 / 会话历史 / 已读论文 / 已学概念）切换右侧面板，同一时刻只显示一个；再点当前图标可收起 / 展开侧栏；拖动侧栏右缘可调整宽度（记忆在浏览器）。底部**常驻「用量」**：
- **用量**：会话总开销、跨会话累计、当前节点上下文 token（均为 API 返回的精确值）。
- **笔记结构**：笔记大纲，点击定位到笔记对应位置；有批注的块标 `●`。
- **会话历史**：顶部「＋ 新会话」；点条目加载；右键置顶 / 重命名 / 删除。
- **已读论文 / 已学概念**：点击查看详情；「加载该会话」可跳回当时的问答（若该问答来自批注，会滚动到原文并打开批注弹窗）；右键置顶 / 删除。
- **批量操作**：三个列表均支持 **Ctrl(⌘) 点选、Shift 连选**，右键即可批量置顶 / 删除；标题旁显示「已选 N」，`Esc` 取消选择。

**导入**：新建会话（或还没有笔记）时，「笔记」页中央显示「＋ 导入论文」按钮，点击选择文件；也可直接把 PDF / TXT 拖进窗口。上传前弹出**导入窗口**：填笔记文件名、选**笔记风格**（四段式 / 逐段翻译 / 自由笔记）。上传时显示百分比；解析与生成笔记期间，顶部显示**流动进度条 + 实时已生成字数/耗时**，控制台同步流式输出 Markdown。

**编辑笔记**：鼠标悬停任意标题 / 段落 → 右上角浮出「✎ 编辑」→ 弹窗内可：
- **手动改文字**：直接改（支持 Markdown 与 `$公式$`），点「应用」保存；
- **AI 重写 / AI 补充**：填一句要求 →「生成」→ 结果**流式写入编辑框**（可「停止」）→ 可再修改 → 点「应用」才写回（重写）或「插入」（补充）；
- **插入到该块后**：把编辑框内容按 Markdown 解析成多个块插入；
- **删除该块**：连同子树、追问与相关批注一起删除（二次确认）。

编辑**章节**时，编辑框会带出**该节标题与全部子块内容**，点「应用」即整体重写该节（标题可一并修改）。所有编辑与删除都可通过顶栏「撤销」回退。

**运行中与中止**：所有会调 LLM 的操作（导入 / 提问 / 批注 / 总结 / AI 改写 / 配置测试）都有进度提示与「停止」——导入等命令用标签栏的流动进度条 + `⏹ 停止`；批注弹窗、笔记编辑弹窗、配置测试弹窗内各有一条迷你进度条 + 「停止」。点击立即中止（后端打断 + 断开本地流）。

**错误与测试**：出错时控制台显示**中文摘要**（如「API Key 无效或未授权」「输入超出模型上下文长度」），并给出「查看详情」展开**原始 API Response / 错误链**。配置弹窗的「测试连接」可用当前表单值发一条请求，显示 HTTP 状态、耗时与原始响应（测试中可点「停止」中止），便于保存前排查。

实现方式：`paperhelper web` 启动内嵌的 axum 服务，把业务输出抽象为 SSE 事件流（`src/output.rs` 的 `Emitter`），前端用原生 JS 消费。仅监听 `127.0.0.1`，不对外暴露。每个请求、每次 LLM 调用与命令执行都会写入日志（见[「日志与排错」](#日志与排错)）。

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
> ingest samples/论文.pdf                       # 默认：PyMuPDF 提取文本 + 四段式笔记
> ingest --style translate samples/论文.pdf    # 逐段翻译风格（忠实原文）
> ingest --style free samples/论文.pdf         # 自由笔记（不加结构约束）
> ingest --text samples/论文.txt                # 直接读取文本文件（跳过 PDF 解析）
> ingest --ocr samples/扫描件.pdf               # OCR 识别（需安装 tesseract）
```

- `--style four|translate|free`：笔记生成风格（可与其他选项任意顺序组合），对应提示词 `note.txt`/`translate.txt`/`free.txt`
- `--text`：适合已用其他工具提取好文本的场景，或想手动修正 PDF 提取结果
- `--ocr`：适合扫描版 PDF（无文本层）；未装 tesseract 会给出安装指引，不影响其他功能
- 翻译为**单次整篇**生成；若模型输出达到上限被截断，会提示「⚠️ 笔记可能被截断」

### REPL 操作与补全

- `↑↓` 切换历史命令，`←→` 移动光标，`Ctrl-C` 立即打断当前任务（网络请求挂起、PDF/OCR 卡住时也有效）
- Tab 补全：命令名；`ingest/save/load/export` 补全文件路径；`goto` 补全节点编号；`ask/check` 补全笔记编号；`config set` 补全键名与常用值

## 命令一览（CLI）

| 命令 | 说明 |
|------|------|
| `ingest [--style four\|translate\|free] <pdf>` | 导入 PDF 生成笔记（可选风格），自动导出 |
| `ingest --text <txt>` | 直接读取文本文件（跳过 PDF 解析） |
| `ingest --ocr <pdf>` | OCR 识别扫描件（需 tesseract） |
| `ask <编号> <问题>` | 按编号定位 Section 追问，解释插入笔记对应位置，递归嵌套 |
| `check <编号> <想法>` | 与 ask 类似但不写入笔记，用于核对理解 |
| `sum [n]` | 把节点 n（默认当前）的子树折叠（`<details>` 可展开）并替换为「总结」 |
| `del [n] [--yes]` | 删除节点 n（默认当前）及其子树（同时移除笔记中对应解释）；根节点不可删 |
| `undo` | 撤销上一次编辑 / 删除（内存多级，最多 20 步，重启后失效） |
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
| `config test` | 用当前配置发一条最小请求，测试端点 / Key / 模型；失败打印原始响应 |
| `exit` | 退出（自动保存会话，告知恢复方式） |

---

## 安装与部署

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
cargo test                  # 运行测试（37 个）
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

### 4. 启动

```bash
paperhelper web              # Web 界面（推荐，默认 http://127.0.0.1:8080）
paperhelper                  # CLI REPL
```

在运行 Web 服务的终端按 `Ctrl-C` 停止服务；CLI 下 `Ctrl-C` 是打断当前任务。

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
│   ├── note.txt         # 四段式笔记模板（{raw_text} 为论文占位符）
│   ├── translate.txt    # 逐段翻译模板
│   ├── free.txt         # 自由笔记模板
│   └── rewrite.txt      # AI 改写/补充模板（{paper}/{note}/{target}/{instruction}/{task}）
├── uploads/             # Web 端上传的论文文件
├── logs/                # 运行日志（超过 5MB 轮转为 paperhelper.log.1）
│   └── paperhelper.log
└── sessions/            # 会话存档（文件名 = 会话编号 = 首次保存时间戳）
    ├── 20260909_021633.json
    └── 20260909_033219.json
```

所有文件已被 `.gitignore` 忽略，不会泄露。

## 日志与排错

- **日志**：启动横幅（cwd / 数据目录 / 端点 / 模型 / Key 脱敏 / 端口）、每个 HTTP 请求（方法 / 路径 / 状态 / 耗时）、每次 LLM 调用（模型 / 消息数 / 耗时 / token / 重试）、命令执行、上传、PDF/OCR 子进程都会写入 **stderr 与 `.paperhelper/logs/paperhelper.log`**。级别用环境变量 `PAPERHELPER_LOG` 控制（`error`/`warn`/`info`/`debug`，默认 `info`）：
  ```bash
  PAPERHELPER_LOG=debug paperhelper web   # CLI 同理
  tail -f .paperhelper/logs/paperhelper.log
  ```
- **测试连接**：CLI `config test`，或 Web「配置 → 测试连接」，显示 HTTP 状态 / 耗时 / 原始响应。
- **错误详情**：Web 控制台里错误显示中文摘要，可展开「详情」看完整错误链与**原始 API Response**；CLI 直接打印 `{e:#}` 错误链。
- **常见错误对照**：`401/403` → Key 无效/无权限；`404` → 端点路径不对（需含 `/chat/completions`）；`429` → 限流或额度不足；`400 + context` → 超出上下文；连接失败 → 网络/代理问题；上传失败/`413` → 文件超过 200MB 上限。

## 自定义提示词与补全预设

- **提示词**：直接编辑 `.paperhelper/prompts/` 下的模板：`ask.txt`（回答风格）、`note.txt` / `translate.txt` / `free.txt`（三种笔记风格，`{raw_text}` 会被替换为论文全文）、`rewrite.txt`（AI 改写/补充）。改完重启生效；删除文件则恢复内置默认。
- **补全预设**：`config.toml` 的 `[presets]` 节可增删 `config set llm.model` / `llm.api_endpoint` 的 Tab 补全候选：

```toml
[presets]
models = ["deepseek-v4-pro", "deepseek-v4-flash", "gpt-4o-mini"]
endpoints = ["https://api.deepseek.com/v1/chat/completions", "https://api.openai.com/v1/chat/completions"]
```

---

## 技术架构

- **Rust 主控**：PDF 解析、笔记树、对话树、编号、定位、导出、统计——全部 Rust 确定性逻辑
- **LLM 只产自然语言**：生成笔记、回答追问、提取概念名、给会话取名、生成总结
- **两棵树**：笔记树（Section/Paragraph + 递归嵌套的 Explanation，`sum` 后折叠）+ 对话树（每节点一次 Q&A，跳转节点的根路径即上下文）
- **文本锚点批注**：批注记录 `block_id + 选中文字`，渲染时按文本引用定位并高亮，点击可重开弹窗；匹配时跳过 KaTeX 隐藏 MathML、按可见文本逐节点包裹，含公式的引用也能高亮且不破坏公式结构
- **可编辑笔记树**：块级编辑（改文字 / 整节重写 / 插入 / 删除）尽量保持块 id 稳定；`parse_markdown_blocks` 把 Markdown 解析成块序列（`#` 也当 Section），结构变化后统一重排编号
- **流式输出 + 进度**：LLM 输出实时渲染，耗时任务显示进度条与实时字数/耗时
- **可中止**：`interrupt` 全局信号 + `tokio::select!`——LLM 的发送/读取、重试等待、PDF/OCR 子进程（`kill_on_drop`）都能被 `Ctrl-C` / Web「停止」立即打断
- **Web / CLI 双前端**：业务输出经 `output::Emitter` 抽象，同一套代码分别写终端与 SSE（`src/web.rs` 为 axum 服务，前端原生 JS 内嵌）
- **错误归类 + 日志**：HTTP/网络错误映射成中文摘要与排查建议（原始响应进错误链与日志）；`src/logging.rs` 轻量日志写 stderr 与 `.paperhelper/logs/`
- **健壮性**：网络抖动自动重试（2 次退避）、上下文超长自动截断早期对话、token 预算到上限自动中断、上传流式写盘并限 200MB
- **HTML 导出**：marked + KaTeX（CDN 带 npmmirror 备源），离线降级纯文本

## 开发

```bash
cargo build                 # 构建
cargo test                  # 37 个单元测试
cargo build --release       # 发布构建
```

二进制产物：`./target/debug/paperhelper`（或 `./target/release/paperhelper`）。
