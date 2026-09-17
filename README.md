# PaperHelper — 论文学习助手

读论文时遇到不懂的概念就追问，追问中又引出新的前置知识，等搞明白已经忘了主线？通用 AI 的对话只会向前延伸，几次深入追问后就丢失上下文。

PaperHelper 用**树形对话 + 笔记批注**解决这个问题：每次追问是树上的一个节点，随时跳回主线或其他分支继续提问；在笔记里**选中文字即可就地提问**，答案钉在原文位置，点击高亮即可回看。

主界面是**浏览器 Web 应用**（`paperhelper web`），也提供功能等价的**命令行界面（CLI）**。安装与启动见文末[「安装与部署」](#安装与部署)。

## 核心功能

- **Web 界面（主）**：导入、阅读笔记、选中提问、管理会话、配置模型、导出，全部在浏览器里完成
- **Windows 桌面版**：Release 提供一键安装包 `paperhelper-setup.exe`（或解压即用的 zip），双击装好即用（原生窗口 + 内置 WebView2），**内置 Python 与 PyMuPDF、用户机器不用装 Python**；自动启动本地服务、关窗即停、单实例防重复打开；无需终端与浏览器，数据固定存 `%USERPROFILE%\PaperHelper\.paperhelper`
- **四步上手向导**：首次打开（或未配置模型）自动弹出「选服务商 → 填 API Key → 检查环境 → 导入示例」；内置一份示例笔记（原样导入，0 token）与一篇示例论文（体验完整流程），填完 Key 可当场「测试连接」；未配置模型时发起提问也会自动引导到向导
- **笔记内就地提问**：在笔记里选中一段文字 → 点浮出的「提问」→ 小窗口内问答；答案与所选文字绑定，点击高亮即可重新展开。选中公式会把它在 KaTeX 里的**原始 LaTeX**交给模型（不是渲染后的文本）
- **章节提问**：右键笔记标题（h1=全文，h2/h3=小节）→「对本章节提问」
- **递归嵌套批注**：对追问的回答再追问，批注层层嵌套；**回答里也能选中文字提问**（引用同样持久高亮，点击回看）；`sum` 把子树折叠成「总结」并可展开
- **多种笔记风格（可自定义）**：导入时可选内置风格「四段式 / 逐段翻译 / 中英对照 / 忠实照抄 / 讲义提纲 / 自由笔记」，也能新建自己的风格；导入时可再填一句「本次额外要求」（如"只翻译"），并可一键「另存为风格」。风格 = 名字 + 说明 + 提示词模板，**格式要求由程序固定附加**（`#` 标题 / `##` 分节 / `$公式$` 等，界面里不用写），文件在 `.paperhelper/styles/`（清单 `styles.toml`），Web 里可视化新建/编辑/复制/删除/恢复默认，自定义风格还能改 id（同步改文件名）
- **直接导入笔记 / 课程讲义**：把已有的笔记、讲义（Markdown / TXT / HTML / PDF）直接导入，不调用模型、不花 token；PDF 讲义可在导入时改选「AI 整理为讲义」或「逐段翻译」。HTML 会在浏览器端转成 Markdown（公式按 KaTeX 还原，MathJax 的 `\newcommand` 宏定义会自动收集并注册给 KaTeX，公式里的自定义宏照常渲染）
- **编辑与 AI 改写**：悬停笔记块点「✎ 编辑」即可改文字；也可让 AI 重写 / 补充某段，或手动插入内容、删除整块（普通编辑不换块 id，追问与批注不受影响）；**所有编辑/删除都能用顶栏「撤销」回退**（内存多级，最多 20 步）
- **导入 PDF → 结构化笔记**：LLM 按固定四段架构（问题 / 前人方案 / 本文方案 / 前景）生成详细 Markdown，含公式、表格、数值结果
- **跨论文知识库**：自动积累读过的论文与学过的概念（只记录模型明确标注为知识点的 `[[概念: …]]`；提问弹窗的「记概念」可关，CLI 可加 `--no-concept`），追问时自动关联已学知识
- **导出**：一键下载 Markdown / 思维导图（markmap）/ 自包含 HTML（KaTeX 公式渲染，含只读批注弹窗）
- **成本可控**：精确统计每次调用的 token 与成本，可设 token 预算，到上限自动中断
- **会话历史**：自动保存，可列表查看、置顶 / 重命名 / 删除、随时恢复
- **随时中止**：导入 / 提问 / 批注等长任务，Web 端点「停止」、CLI 按 `Ctrl-C` 立即打断（含卡住的网络请求与 PDF / OCR 子进程）
- **后台任务不挡手**：LLM 生成期间可自由浏览 / 切换会话、查看笔记与批注；任务与发起会话绑定，完成后结果写回那里（发起会话被改动过才提示冲突）
- **可排查的错误**：后端错误归类成中文提示（Key 无效 / 端点错 / 限流 / 超上下文…），可展开「详情」查看原始 API Response；设置弹窗内置「测试连接」
- **运行日志**：启动、HTTP 请求、LLM 调用、上传、命令执行都写入 stderr 与 `.paperhelper/logs/paperhelper.log`
- **CLI（附带）**：功能等价的全键盘 REPL，适合脚本化与服务器环境

## Web 界面详解

```bash
paperhelper web              # 默认 http://127.0.0.1:8080
paperhelper web --port 9000  # 指定端口
```

浏览器打开后即可使用。在运行服务的终端按 `Ctrl-C` 停止服务。

![PaperHelper Web 界面](screenshots/ui.png)

**首次使用向导**：第一次打开（还没有配置 API Key）会自动弹出四步向导：① 选服务商（DeepSeek / Paratera / Ollama 本地 / 自定义）② 填 Endpoint 与 Key，可当场「测试连接」再保存 ③ 检查 Python / PyMuPDF，缺了一键安装（走国内镜像，可「停止」）④ 导入内置示例（示例笔记 0 token 直接看效果；示例论文走一遍完整生成）。随时可点顶栏「向导」重看；未配置模型时发起提问也会自动跳到第 2 步。Ollama 等本地服务不需要 Key。

![首次使用向导](screenshots/wizard-step1.png)

**顶栏**：`导出 ▾`（Markdown / 思维导图 / HTML，浏览器下载）· `撤销` · `向导` · `帮助` · `设置` · `刷新`，并显示当前模型。「设置」分两个页签：**模型设置**（API 端点 / 密钥 / 模型 / 上下文长度 / 思考模式 / 单价 / 预算）与**笔记风格**（风格的新建 / 编辑 / 复制 / 删除 / 恢复默认；格式要求由程序自动附加，这里只写内容与结构要求）。

**笔记区**（主区「笔记」标签）：
- **选中提问**：在笔记里选中一段文字 → 浮出「提问」→ 小窗口内输入问题（可切 ask / check）→ 回车发送；回答流式显示，并与所选文字**高亮绑定**，点击高亮可重新打开小窗口。选中内容里的公式会按 KaTeX 隐藏层还原成 `$...$` 的 LaTeX 交给模型；只选中公式的一小段时，会给出整条公式并注明选中片段。
- **章节提问**：右键标题（h1=全文，h2/h3=小节）→「对本章节提问」；已有批注时另有「打开 / 删除」。
- **回答里追问**：在批注弹窗的任意回答中选中文字 → 浮出「提问」→ 新的问答挂在该回答节点下，选中文字在回答里**持久高亮**，点击高亮即可回到该问答（右键删除）。选区不会被清掉，输入框上方还有「引用」附件条显示这次问的是哪一段；节点标题上方也会标出「针对：<引文>」。「记概念」勾选框决定这次问答是否写入「已学概念」（默认开；操作性提问可取消）。
- **批注弹窗**：节点左键跳转、右键删除 / 总结；`sum` 后的节点显示为「总结」并可展开查看原对话。弹窗可**按住顶部标题栏拖动**（不记忆位置）、**四条边 + 四个角都能拖拽缩放**（尺寸记忆在浏览器、无上限，双击把手恢复默认）；输入栏为多行（回车发送 / Shift+回车换行，随内容自动增高，也可拖上缘调整高度），「思考过程」区可拖下缘调高度。
- **右键高亮**：删除整条批注。

**主区标签**：固定「笔记」「控制台」；点击侧栏的论文 / 概念会新增并列小标签页（概念详情、论文笔记 + 对应会话）。

**侧栏**：左侧竖排**活动栏**（笔记结构 / 会话历史 / 已读论文 / 已学概念）切换右侧面板，同一时刻只显示一个；再点当前图标可收起 / 展开侧栏；拖动侧栏右缘可调整宽度（记忆在浏览器）。底部**常驻「用量」**：
- **用量**：会话总开销、跨会话累计、当前节点上下文 token（均为 API 返回的精确值）。
- **笔记结构**：笔记大纲，点击定位到笔记对应位置；有批注的块标 `●`。
- **会话历史**：顶部「＋ 新会话」；点条目加载；右键置顶 / 重命名 / 删除。
- **已读论文 / 已学概念**：点击查看详情；「加载该会话」可跳回当时的问答（若该问答来自批注，会滚动到原文并打开批注弹窗）；右键置顶 / 删除。
- **批量操作**：三个列表均支持 **Ctrl(⌘) 点选、Shift 连选**，右键即可批量置顶 / 删除；标题旁显示「已选 N」，`Esc` 取消选择。

**导入**：新建会话（或还没有笔记）时，「笔记」页中央显示「＋ 导入资料」按钮，点击选择文件；也可直接把 PDF / TXT / MD / HTML 拖进窗口。上传前弹出**导入窗口**，先选模式：

- **论文 → 生成笔记**：读论文全文，按所选风格生成结构化笔记（默认四段式；也可选逐段翻译 / 中英对照 / 忠实照抄 / 自由笔记等）。
- **笔记 / 讲义 → 导入**：导入已有的讲义或自己整理的笔记。默认「**原样导入**」——直接解析成笔记树，不调用模型、不花 token；也可以选一种风格让 AI 处理（例如 PDF 讲义选「讲义提纲」，英文讲义选「逐段翻译」）。上传 **HTML** 时会自动转成 Markdown：标题成章节、公式按 KaTeX 隐藏层还原成 `$...$`、脚本与事件属性剔除；页面里 MathJax 风格的 `\newcommand` 宏块会被收集起来，渲染时注册给 KaTeX（公式里的 `\abs`、`\ket` 这类自定义宏也能正常显示）。

导入窗口里还能：填「**本次额外要求**」（如"只翻译""数学符号别翻译"），它会被拼到所选风格的提示词之后；点「**另存为风格…**」把当前风格 + 这次要求存成一个新风格；点「**管理…**」打开「设置 → 笔记风格」（新建 / 编辑 / 复制 / 删除 / 恢复默认，改完即可在导入时选用）。

上传时显示百分比；解析与生成笔记期间，顶部显示**流动进度条 + 实时已生成字数/耗时**，笔记页覆盖层里实时流式显示 Markdown、阶段文本与思考过程；切到其他会话时覆盖层隐藏（任务继续），切回发起会话可继续看，完成后结果写回发起会话。翻译/整理为单次整篇，**超长讲义可能被截断**（会明确提示，建议按章拆分导入）。直接导入的笔记不发原文给模型，ask 时只带笔记本身。

**编辑笔记**：鼠标悬停任意标题 / 段落 → 右上角浮出「✎ 编辑」→ 弹窗内可：
- **手动改文字**：直接改（支持 Markdown 与 `$公式$`），点「应用」保存；
- **AI 重写 / AI 补充**：填一句要求 →「生成」→ 结果**流式写入编辑框**（可「停止」）→ 可再修改 → 点「应用」才写回（重写）或「插入」（补充）；生成中可关闭弹窗、切换会话，任务在后台继续，重开弹窗会恢复已生成内容与草稿状态；
- **插入到该块后**：把编辑框内容按 Markdown 解析成多个块插入；
- **删除该块**：连同子树、追问与相关批注一起删除（二次确认）。
- **主标题按风格重写全文**：编辑主标题时多一个「按风格重写全文」页签——选一种笔记风格（可加额外要求）让 AI 重写整篇；应用会重建全部块 id 并清空批注（可用「撤销」恢复）。

编辑**章节**时，编辑框会带出**该节标题与全部子块内容**，点「应用」即整体重写该节（标题可一并修改）。所有编辑与删除都可通过顶栏「撤销」回退。

**并发与等待**：需要 LLM 的操作（导入 / 提问 / 批注 / AI 改写 / 按风格重写）同一时刻只允许一个；生成期间**其余操作照常可用**——切换/浏览会话、查看笔记、撤销、导出、改配置等都不会被卡住。**LLM 任务与「发起它的会话 + 会话版本」绑定**：切走后任务在后台继续，完成后结果自动写回**发起会话**（若发起会话期间被你改动过，会提示「会话已变更」不写入；若已被删除则明确提示未写入）。导入的实时反馈覆盖层只在发起会话显示：切走隐藏、切回恢复，完成后在控制台提示。LLM 任务进行中再发起第二个 LLM 操作会明确提示「已有 LLM 任务在运行，请稍候或先点停止」；顶栏流动进度条 + `⏹ 停止` 始终可见，随时可中止。

**运行中与进度**：所有会调 LLM 的操作（导入 / 提问 / 批注 / 总结 / AI 改写 / 配置测试）都有进度提示与「停止」——导入等命令用标签栏的流动进度条 + `⏹ 停止`；批注弹窗、笔记编辑弹窗、配置测试弹窗内各有一条迷你进度条 + 「停止」。等待首个 token 期间显示**已发送的上下文规模 + 计时**（如「思考中…（上下文约 12k token）· 8s」）；推理模型的**思考过程**（`reasoning_content`）会流式显示在可折叠的「思考过程」区，CLI 下以暗色打印。**导入 / 生成笔记的反馈都显示在「笔记」页**（阶段文本、思考过程、正在生成的笔记文本、已生成字数与耗时），不再刷控制台；控制台只在出错时自动打开。「停止」点击立即中止（后端打断 + 断开本地流）。

**错误与测试**：出错时控制台显示**中文摘要**（如「API Key 无效或未授权」「输入超出模型上下文长度」），并给出「查看详情」展开**原始 API Response / 错误链**。设置弹窗的「测试连接」可用当前表单值发一条请求，显示 HTTP 状态、耗时与原始响应（测试中可点「停止」中止），便于保存前排查。

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
> ingest --style translate samples/论文.pdf    # 逐段翻译（只译文；长文可能被截断）
> ingest --style lecture samples/讲义.pdf      # 讲义提纲（知识点 + 复习清单）
> ingest --style verbatim samples/资料.pdf     # 忠实照抄（只做标题/公式/表格结构化）
> ingest --note --text samples/我的笔记.md      # 直接导入笔记/讲义（不调 LLM，0 token）
> ingest --note --ocr samples/扫描讲义.pdf     # 扫描件 OCR 后直接导入
> ingest --extra "只保留公式和结论" --style free samples/资料.pdf   # 本次额外要求
> ingest --text samples/论文.txt                # 直接读取文本文件（跳过 PDF 解析）
> ingest --ocr samples/扫描件.pdf               # OCR 识别（需安装 tesseract）
> styles                                       # 列出全部风格；styles show <id> 看提示词
```

- `--style <风格id>`：任意内置或自定义风格（`styles` 可列出），对应提示词在 `.paperhelper/styles/<id>.txt`
- `--note`：直接导入笔记/讲义——解析成笔记树、登记知识库，**不调用 LLM**；`--kind paper|note|lecture` 只影响列表里的类型标记
- `--extra "…"`：本次额外要求，拼到所选风格提示词之后（Web 的「另存为风格…」可把它固化成新风格）
- `--text`：适合已用其他工具提取好文本的场景，或想手动修正 PDF 提取结果；直接导入 HTML 请在 Web 界面操作（浏览器端转换会保留公式）
- `--ocr`：适合扫描版 PDF（无文本层）；未装 tesseract 会给出安装指引，不影响其他功能
- 翻译为**单次整篇**生成；若模型输出达到上限被截断，会提示「⚠️ 笔记可能被截断」

### REPL 操作与补全

- `↑↓` 切换历史命令，`←→` 移动光标，`Ctrl-C` 立即打断当前任务（网络请求挂起、PDF/OCR 卡住时也有效）
- Tab 补全：命令名；`ingest/save/load/export` 补全文件路径；`goto` 补全节点编号；`ask/check` 补全笔记编号；`config set` 补全键名与常用值

## 命令一览（CLI）

| 命令 | 说明 |
|------|------|
| `ingest [--style <id>] [--extra "…"] <pdf>` | 导入 PDF 按风格生成笔记（`styles` 可列出风格），自动导出 |
| `ingest --note <文件>` | 直接导入笔记/讲义（不调 LLM；`--kind paper\|note\|lecture`） |
| `ingest --text <txt>` | 直接读取文本文件（跳过 PDF 解析） |
| `ingest --ocr <pdf>` | OCR 识别扫描件（需 tesseract） |
| `ask <编号> <问题>` | 按编号定位 Section 追问，解释插入笔记对应位置，递归嵌套 |
| `check <编号> <想法>` | 与 ask 类似但不写入笔记，用于核对理解 |
| `ask --no-concept <编号> <问题>` | 本次回答不写入「已学概念」（操作性提问用） |
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
| `papers` / `concepts` | 列出已读论文（含讲义/笔记标记）/ 已学概念 |
| `styles` / `styles show <id>` | 列出笔记风格 / 查看某个风格的提示词（编辑走 Web 或改文件） |
| `save [file]` / `load <file>` | 保存 / 加载会话 |
| `new` | 新建会话 |
| `config show` / `config set <k> <v>` | 查看 / 设置配置 |
| `config presets [id]` | 列出内置服务商预设；带 id（`deepseek`/`paratera`/`ollama`/`custom`）预填端点/模型/上下文/单价 |
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

**Python 解释器的查找顺序**（找不到 PATH 里的 `python3` 时尤其有用）：

1. 环境变量 `PAPERHELPER_PYTHON`（显式指定，如 `C:\Python313\python.exe`）
2. 打包内置解释器（exe 同级 `python/` 目录，官方 Windows 包自带）
3. Windows：`py -3` → `python` → `python3`；Linux/macOS：`python3` → `python`

`import pymupdf` 失败的报错里会给出当前解释器的修复命令。Web「环境检查」页也有同样信息。

### 2. 编译

直接 clone 出来就是一个独立 Cargo crate，正常构建即可。若你的上级目录恰好是 Cargo workspace，则加 `-p paperhelper`：

```bash
git clone <仓库地址> && cd paperhelper
cargo build                 # 独立 crate 直接构建
cargo build -p paperhelper  # 在 workspace 下时加 -p
cargo test                  # 运行测试（81 个）
```

编译产物在 `./target/debug/paperhelper`；Windows 上还会多一个 `./target/debug/paperhelper-desktop.exe`（桌面窗口版，Linux/macOS 编译为空壳，不影响构建与测试）。

### 3. 配置大模型（设置 → 模型设置）

任选其一（优先级：环境变量 > `.env` > `.paperhelper/config.toml`）。最省事的是启动后跟着**首启向导**走（见上文「首次使用向导」），或用下面任一方式。

**方式 1：Web 设置弹窗（推荐）** —— 启动后点顶栏「设置」→「模型设置」即可改 API Endpoint / Key / 模型 / 上下文长度 / 思考模式 / 单价 / 预算，即改即存；「笔记风格」页签管理笔记生成风格。

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
> config presets deepseek          # 一键预填端点/模型/上下文/单价（deepseek/paratera/ollama/custom）
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
paperhelper web --open       # 启动后自动打开默认浏览器
paperhelper web --port 9000  # 指定端口（被占会自动顺延到下一个可用端口）
paperhelper-desktop          # Windows 桌面窗口版（见下）
paperhelper                  # CLI REPL
```

在运行 Web 服务的终端按 `Ctrl-C` 停止服务；CLI 下 `Ctrl-C` 是打断当前任务。

**Windows 桌面窗口版**（`paperhelper-desktop.exe`，与 `paperhelper.exe` 同目录）：双击即以原生窗口打开界面，不用终端、不用浏览器。它做的事：单实例互斥锁（重复双击会聚焦已有窗口）→ 用隐藏控制台启动 `paperhelper web --port 0 --port-file <临时文件>`（随机端口、不弹黑框）→ 读端口文件后用 WebView2 打开 `http://127.0.0.1:<端口>/` → 关窗先 `/api/interrupt`（终止在途 LLM 任务）再 `/api/shutdown`（优雅退出），并把服务进程放进 Job Object，壳崩溃也不会残留后台服务。需要系统里有 Microsoft Edge WebView2 运行时（Win10 1803+ / Win11 一般自带），没装时安装程序会提示并打开官方下载页。源码 `src/bin/paperhelper-desktop.rs`（仅 Windows 编译，其他平台是空壳）；图标由 `python3 scripts/make_icon.py` 生成到 `assets/icon.ico`。

### 5. Windows 安装包 / 绿色版（维护者）

普通用户直接用 Release 里的两个产物即可：`paperhelper-setup.exe`（安装到 `%LOCALAPPDATA%\PaperHelper`，桌面 + 开始菜单快捷方式，per-user 免管理员）或 `paperhelper-windows-x64.zip`（解压后双击 `paperhelper-desktop.exe`）。两者都内置 Python 解释器与 PyMuPDF，用户机器上**不需要装 Python**。

自己出包（在 Windows 上）：

```powershell
pwsh scripts/package-windows.ps1                  # 编译 + 下载 Python/PyMuPDF + 出 zip 和 setup.exe
pwsh scripts/package-windows.ps1 -SkipInstaller   # 只要绿色 zip（不装 NSIS 时自动跳过安装包）
```

脚本流程：`cargo build --release -p paperhelper --bins` → 下载 python.org embeddable（`-PythonVersion`，默认 3.13.13；`-PythonMirror` 可换镜像）→ 改写 `python313._pth` 让 `Lib\site-packages` 生效 → `pip download` 拉 win_amd64 的 PyMuPDF wheel 解包进去（`-PipIndex` 换镜像）→ 用内置 `python.exe -c "import pymupdf"` 自检 → 组 `dist-win\runtime\` → 压缩 zip → 有 `makensis` 时按 `packaging/windows/installer.nsi` 出安装包。安装包不删用户数据（笔记在 `%USERPROFILE%\PaperHelper\.paperhelper`，与安装目录分离）。

CI：`.github/workflows/windows-release.yml` 在推 `v*` tag 时自动跑「编译 → CLI 起服务冒烟（`--port-file` 握手 + `/api/state`）→ 打包 → 发 Release」；也可在 Actions 页手动触发（只出构建产物不发布）。

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
├── knowledge.json       # 跨论文知识库（论文/讲义/笔记 + 概念 + 累计用量）
├── styles.toml          # 笔记风格清单（id / 名称 / 说明 / 适用 / 文件）
├── styles/              # 笔记风格提示词（可直接编辑；内置风格首次启动自动写出）
│   ├── four.txt         # 四段式（{raw_text} 为资料全文占位符）
│   ├── translate.txt    # 逐段翻译
│   ├── translate-bi.txt # 中英对照
│   ├── verbatim.txt     # 忠实照抄
│   ├── lecture.txt      # 讲义提纲
│   ├── free.txt         # 自由笔记
│   └── my-style.txt     # 自定义风格（Web「管理风格」新建）
├── prompts/             # 行为提示词（可编辑）
│   ├── ask.txt          # ask/check 的 system prompt
│   └── rewrite.txt      # AI 改写/补充模板（{paper}/{note}/{target}/{instruction}/{task}）
├── uploads/             # Web 端上传的文件（论文/讲义/笔记）
├── logs/                # 运行日志（超过 5MB 轮转为 paperhelper.log.1）
│   └── paperhelper.log
└── sessions/            # 会话存档（文件名 = 会话编号 = 首次保存时间戳）
    ├── 20260909_021633.json
    └── 20260909_033219.json
```

所有文件已被 `.gitignore` 忽略，不会泄露。

数据目录默认是**当前工作目录**下的 `.paperhelper/`。想固定到一个位置（换工作目录也不"丢"笔记），可用环境变量 `PAPERHELPER_DATA_DIR`，支持绝对路径与 `~` 开头：

```bash
PAPERHELPER_DATA_DIR="$HOME/PaperHelper/.paperhelper" paperhelper web
# Windows PowerShell: $env:PAPERHELPER_DATA_DIR="$env:USERPROFILE\PaperHelper\.paperhelper"
```

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

- **笔记风格**：编辑 `.paperhelper/styles/<id>.txt`（`{raw_text}` 会被替换为资料全文），或改 `.paperhelper/styles.toml` 增删风格；Web「设置 → 笔记风格」可视化新建/编辑/复制/删除/恢复默认。内置风格删掉文件后重启会自动恢复默认。
- **行为提示词**：直接编辑 `.paperhelper/prompts/` 下的模板：`ask.txt`（回答风格）、`rewrite.txt`（AI 改写/补充）。改完重启生效；删除文件则恢复内置默认。
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
- **文本锚点批注**：批注记录「锚点 + 选中文字」，笔记锚点是 `block_id`、回答锚点是 `node_id`，渲染时按文本引用定位并高亮，点击可重开弹窗；匹配时跳过 KaTeX 隐藏 MathML、按可见文本逐节点包裹，含公式的引用也能高亮且不破坏公式结构
- **公式上下文**：选中 KaTeX 公式时从隐藏层的 `<annotation encoding="application/x-tex">` 还原 LaTeX（`$...$`/`$$...$$`）作为给 LLM 的上下文（`quote_tex`），避免传渲染后的线性文本；只选中子片段时给整条公式并注明片段（KaTeX 无子表达式源码映射）
- **笔记风格注册表**：风格 = `id/名称/说明/适用/提示词`，清单在 `.paperhelper/styles.toml`、提示词在 `.paperhelper/styles/<id>.txt`（旧 `prompts/{note,translate,free}.txt` 首次启动自动迁移，用户改动不丢）；生成时由程序自动前置**固定输出契约**（`#` 标题 / `##` 分节 / `$公式$` 等）并固定附加**资料全文**，提示词里只写「写什么、按什么顺序讲」（旧文件里的 `{raw_text}` 占位符在打开/保存/启动时自动清除）；末尾追加可选的「本次额外要求」
- **材料类型**：`Note.material_kind` / `Paper.kind`（`paper|note|lecture`，serde 默认值兼容旧数据）；直接导入的笔记 `raw_text` 为空，`build_context_messages` 会省略【原文材料】段——ask 只发笔记本身
- **导入解析**：`parse_import_note`（有标题建 Section 树、无标题按空行切多个段落、不剥代码围栏）；HTML 在浏览器端用 DOMParser 白名单转换（KaTeX 公式还原、脚本/事件属性丢弃、隐藏元素跳过）
- **数学宏**：HTML 里「几乎全是 `\newcommand`」的宏块会被收集为 `Note.math_macros`（随笔记保存），渲染公式时用 `scanMacroDefs` 解析成 KaTeX 的 `macros` 选项（名字带反斜杠，避免单字母被当普通字符展开）；笔记 iframe / HTML 导出 / 弹窗回答三处共用
- **可编辑笔记树**：块级编辑（改文字 / 整节重写 / 插入 / 删除）尽量保持块 id 稳定；`parse_markdown_blocks` 把 Markdown 解析成块序列（`#` 也当 Section），结构变化后统一重排编号
- **流式输出 + 进度**：LLM 输出实时渲染；推理模型的 `reasoning_content` 走独立的 `Event::Reasoning` → SSE `reasoning`，界面显示「思考过程」，等待期显示上下文规模与计时；耗时任务显示进度条与实时字数/耗时。流式解码按**字节缓冲 + 行边界 UTF-8 解码**（网络分片切断中文等多字节字符也不会出现 `�`）；`Emitter` 在 Web/非终端/日志场景剥离 ANSI 颜色码
- **并发模型**：LLM 流式阶段不持全局状态锁（prepare / 锁外 run / commit 三段式），并用全局 LLM 门串行化多个 LLM 任务、用会话版本号（epoch）防止把结果写进已被切换/编辑的会话；因此非 LLM 请求在生成期间不受影响
- **可中止**：`interrupt` 全局信号 + `tokio::select!`——LLM 的发送/读取、重试等待、PDF/OCR 子进程（`kill_on_drop`）都能被 `Ctrl-C` / Web「停止」立即打断
- **Web / CLI 双前端**：业务输出经 `output::Emitter` 抽象，同一套代码分别写终端与 SSE（`src/web.rs` 为 axum 服务，前端原生 JS 内嵌）
- **错误归类 + 日志**：HTTP/网络错误映射成中文摘要与排查建议（原始响应进错误链与日志）；`src/logging.rs` 轻量日志写 stderr 与 `.paperhelper/logs/`
- **健壮性**：网络抖动自动重试（2 次退避）、上下文超长自动截断早期对话、token 预算到上限自动中断、上传流式写盘并限 200MB；PDF/OCR 文本自动清洗字体缺失映射产生的私有区占位字符（避免豆腐块与噪声）
- **HTML 导出 / 前端渲染完全离线**：marked + KaTeX（含字体）编译期内嵌，导出的 HTML 单文件自包含、断网也能渲染公式；主界面优先用本地 `/vendor/*`，仅失败才回退 CDN

## 开发

```bash
cargo build -p paperhelper          # 构建
cargo test  -p paperhelper          # 81 个单元测试
cargo build -p paperhelper --release  # 发布构建
```

二进制产物：`./target/debug/paperhelper`（或 `./target/release/paperhelper`）。

第三方前端资源（marked / KaTeX，MIT 许可）放在 `web/vendor/` 并随仓库提交；
`build.rs` 会把它们编译进二进制（`/vendor/*` 路由 + HTML 导出的内联样式与字体，
woff2 转 data URI）。升级 = 替换 `web/vendor/` 下的文件后重新构建，无需联网。
