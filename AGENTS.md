# 项目说明

## 目标

按照README.md所述，做一个agent

## 要求

- 核心业务逻辑（数据处理、算法流程、API 调用编排）必须用 Rust 写。允许调用其他语言的库（比如 Python 的 PyTorch），但主控流程必须在 Rust 里。
- 用户交互界面: 至少提供以下之一：Web 界面、CLI 交互式终端、桌面 App、手机 App。界面必须能触发 Agent 任务，并展示结果。
- Agent 的用户必须能自由修改大模型的 API Endpoint 和 API Key（比如在 OpenAI 和本地模型之间切换），既可以通过配置文件（.env 或 config.toml），也可以通过 UI/CLI 设置页这类用户友好的界面。此外还要能配置上下文长度、思考模式、API 价格等。
- 执行时间超过 3 秒的任务，UI/CLI 必须实时渲染进度，并且允许用户打断。比如处理照片时显示“已处理 45/120 张”，数学证明时显示“正在尝试证明引理...”。Web 端可以用 SSE/WebSocket 等技术，CLI 端可以用进度条库。
- Agent 必须能管理多轮对话或任务状态的历史记录。用户能查看历史任务，也能保存/加载某次会话的完整上下文（比如存成 JSON 文件）。也就是说，用户能看到 Agent 任务背后实际的工作流程（如 DeepSeek Harness 的轨迹显示），而不是把它当成一个黑盒。
- 系统必须精确统计每次 API 调用的输入 token 数和输出 token 数（这两个数字在 API 响应里就能拿到），并根据你配置的模型价格（或预设价格表）实时换算成本。统计信息必须在界面上清晰展示。允许设置 token 预算，用量到预算时自动中断，免得月底看着账单流泪。

## 开发命令

本项目处于上级 workspace（`../Cargo.toml` 把所有子目录当成员）下，所以构建/测试**必须**带 `-p paperhelper`，否则会构建整个 workspace：

```bash
cargo build -p paperhelper          # 构建
cargo test  -p paperhelper          # 跑单元测试
cargo build -p paperhelper --release  # 发布构建
```

二进制产物在上级 `../target/debug/paperhelper`。

## 运行

```bash
# 首次需安装 PDF 解析依赖（Rust 通过子进程调 PyMuPDF，符合"允许调用其他语言库"）
pip install pymupdf

# 配置大模型（.env 或交互式）
paperhelper              # 进入 REPL
> config set llm.api_key <你的key>
> config set llm.model deepseek-v4-pro
> config set llm.api_endpoint https://api.deepseek.com/v1/chat/completions
# 或写 .env：PAPERHELPER_API_KEY=... PAPERHELPER_API_ENDPOINT=... PAPERHELPER_MODEL=...

# 用法
> ingest samples/某论文.pdf   # 解析PDF→LLM生成Markdown笔记→树
> ask 这篇论文的核心方法是什么?   # 基于论文全文+笔记+对话历史回答
> blocks                      # 看笔记结构
> tree                        # 看对话轨迹(带[n]编号)
> goto 2                      # 跳到节点2，其根路径成为上下文
> stats                       # token用量+成本
> export md note.md           # 导出笔记
> export mindmap note.mm      # 导出思维导图(markmap兼容)
> save sess.json / load sess.json
```

配置项一览（`config set <key> <value>` 或 .env 同名大写变量）：
`llm.api_key` `llm.api_endpoint` `llm.model` `llm.context_length` `llm.thinking_mode` `llm.pdf_input` `pricing.input_price_per_1m` `pricing.output_price_per_1m` `budget.token_budget`（0=不限）。

## 架构要点

- 两棵树：笔记树（Section/Paragraph/Formula，LLM 生成 Markdown→Rust 确定性解析）+ 对话树（每节点=一次Q&A，跳转某节点则根路径=对话上下文）。
- 不自实现 RAG：单篇论文整篇塞进 ask 上下文（长上下文模型装得下），每次 ask 重发全文。预算功能防超支。
- 跨论文知识库 `.paperhelper/knowledge.json`：累积读过论文+学过概念，ask 时按关键词检索注入"相关概念"。
- token/成本：会话级 + 跨会话累计；预算到上限自动中断。
- >3秒任务用 indicatif 进度条 + LLM 流式输出实时渲染，Ctrl-C 打断。

## 约定

- **图片素材**：生成的截图等图片统一放项目根的 `screenshots/`，**不要**写到 `~` 或 `/tmp`（snap Chromium 的 `/tmp` 为私有，写不进去）。截图流程示例：

  ```bash
  ./target/debug/paperhelper web --port 18090 &   # 后台起服务
  # 载入含笔记的会话，让界面有内容（会话 id 见 .paperhelper/sessions/）
  curl -s -X POST http://127.0.0.1:18090/api/sessions/load \
    -H 'Content-Type: application/json' -d '{"id":"<会话id>"}'
  /snap/bin/chromium --headless --no-sandbox --disable-gpu --hide-scrollbars \
    --window-size=1440,900 --virtual-time-budget=8000 \
    --screenshot=screenshots/ui.png http://127.0.0.1:18090/
  ```
