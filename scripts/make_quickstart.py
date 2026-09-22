#!/usr/bin/env python3
"""生成《快速开始.html》（自包含单文件：图片以 base64 内嵌，双击即可离线打开）。

用法（仓库根目录）：python3 scripts/make_quickstart.py
产物会同步进 Windows 安装包 / 绿色版（见 scripts/package-windows.ps1）。
"""
from __future__ import annotations

import base64
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "快速开始.html"
REPO = "https://github.com/Ru645/paperhelper"
RELEASES = f"{REPO}/releases/latest"


def b64(rel: str, mime: str) -> str:
    data = base64.b64encode((ROOT / rel).read_bytes()).decode("ascii")
    return f"data:{mime};base64,{data}"


HTML = f"""<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>PaperHelper 快速开始</title>
<style>
  :root {{ --brand: #4f46e5; --brand2: #7c3aed; --ink: #1f2430; --muted: #5b6472; --line: #e6e8ef; --bg: #f7f8fc; }}
  * {{ box-sizing: border-box; }}
  body {{ margin: 0; background: var(--bg); color: var(--ink);
         font: 16px/1.75 -apple-system, "Segoe UI", "Microsoft YaHei", "PingFang SC", "Noto Sans CJK SC", sans-serif; }}
  .wrap {{ max-width: 880px; margin: 0 auto; padding: 36px 20px 72px; }}
  header {{ display: flex; align-items: center; gap: 16px; margin-bottom: 6px; }}
  header img {{ width: 56px; height: 56px; border-radius: 12px; }}
  h1 {{ font-size: 30px; margin: 0; }}
  .sub {{ color: var(--muted); margin: 4px 0 26px; }}
  h2 {{ font-size: 21px; margin: 38px 0 10px; padding-top: 14px; border-top: 1px solid var(--line); }}
  h2 .num {{ display: inline-flex; width: 30px; height: 30px; margin-right: 10px; border-radius: 50%;
             background: linear-gradient(135deg, var(--brand), var(--brand2)); color: #fff;
             font-size: 16px; align-items: center; justify-content: center; vertical-align: 2px; }}
  h3 {{ font-size: 17px; margin: 20px 0 6px; }}
  p, li {{ margin: 6px 0; }}
  ul, ol {{ padding-left: 22px; }}
  code {{ background: #eef0f7; border-radius: 5px; padding: 1px 6px; font-size: 14px;
          font-family: "Cascadia Mono", Consolas, ui-monospace, monospace; }}
  .card {{ background: #fff; border: 1px solid var(--line); border-radius: 14px; padding: 18px 22px; margin: 14px 0;
           box-shadow: 0 1px 2px rgba(23, 30, 55, .04); }}
  .tip {{ border-left: 4px solid var(--brand); background: #f2f1ff; }}
  .warn {{ border-left: 4px solid #f59e0b; background: #fff8eb; }}
  a {{ color: var(--brand); }}
  .btn {{ display: inline-block; background: linear-gradient(135deg, var(--brand), var(--brand2)); color: #fff;
          text-decoration: none; padding: 9px 18px; border-radius: 9px; font-weight: 600; }}
  figure {{ margin: 16px 0; }}
  figure img {{ width: 100%; border: 1px solid var(--line); border-radius: 12px; display: block; }}
  figcaption {{ color: var(--muted); font-size: 14px; margin-top: 6px; text-align: center; }}
  table {{ border-collapse: collapse; width: 100%; margin: 10px 0; background: #fff; }}
  th, td {{ border: 1px solid var(--line); padding: 8px 12px; text-align: left; vertical-align: top; }}
  th {{ background: #f2f3f9; font-weight: 600; }}
  footer {{ margin-top: 46px; color: var(--muted); font-size: 14px; border-top: 1px solid var(--line); padding-top: 16px; }}
  details {{ background: #fff; border: 1px solid var(--line); border-radius: 10px; padding: 10px 16px; margin: 8px 0; }}
  summary {{ cursor: pointer; font-weight: 600; }}
  @media print {{ body {{ background: #fff; }} .card, details, table {{ box-shadow: none; }} }}
</style>
</head>
<body>
<div class="wrap">

<header>
  <img src="{b64('assets/icon.png', 'image/png')}" alt="PaperHelper">
  <div>
    <h1>PaperHelper 快速开始</h1>
    <div class="sub">Windows 10 / 11 三分钟上手：装好 → 粘贴 API Key → 导入论文</div>
  </div>
</header>

<p><b>PaperHelper</b> 是一个读论文的本地助手：把论文 PDF 放进来看它生成的结构化笔记，
遇到不懂的地方就<b>选中文字提问</b>，答案钉在原文位置，随时点高亮回看；追问会形成一棵对话树，
不会像普通 AI 聊天那样越问越乱。笔记、会话、API Key 都只存在你自己的电脑上。</p>

<figure>
  <img src="{b64('screenshots/ui.png', 'image/png')}" alt="PaperHelper 主界面">
  <figcaption>主界面：左边是笔记结构和会话，中间是笔记正文，选中文字即可提问</figcaption>
</figure>

<div class="card tip">
  <b>要准备的东西只有一样：一个大模型的 API Key。</b>
  推荐 DeepSeek（国内可直连、便宜，读一篇论文通常只要几分钱，可在软件里设预算上限）。
  界面、公式渲染、导出、笔记管理等功能都不花钱；只有「生成笔记」和「提问」会调用模型。
</div>

<h2><span class="num">1</span>下载安装（约 1 分钟）</h2>
<ol>
  <li>打开下载页：<a href="{RELEASES}">{RELEASES}</a></li>
  <li>下载 <code>paperhelper-setup.exe</code>，双击安装（推荐；装到当前用户目录，<b>不需要管理员</b>）。<br>
      也可以下载 <code>paperhelper-windows-x64.zip</code>，解压后双击里面的 <code>paperhelper-desktop.exe</code>（绿色版，免安装）。</li>
  <li>如果 Windows 弹出蓝色的「Windows 已保护你的电脑」，点<b>「更多信息」→「仍要运行」</b>即可
      （软件没有买代码签名证书，属于正常提示，不是病毒）。</li>
</ol>
<p>装好后桌面和开始菜单会出现 PaperHelper 图标，以后双击即可。<b>不需要安装 Python、Rust 或任何命令行工具</b>——
PDF 解析用的 Python 与 PyMuPDF 已经打包在内。完整图文说明（含常见问题）就在安装目录里的
<code>快速开始.html</code>（也就是本文件），开始菜单里也能直接打开。</p>

<h2><span class="num">2</span>填一下 API Key（约 1 分钟）</h2>
<p>第一次打开会自动弹出四步向导，跟着走就行：</p>
<figure>
  <img src="{b64('screenshots/wizard-step1.png', 'image/png')}" alt="首次使用向导：选择服务商">
  <figcaption>第 1 步选服务商（推荐 DeepSeek）→ 第 2 步粘贴 Key → 第 3 步检查环境 → 第 4 步导入示例</figcaption>
</figure>
<ol>
  <li><b>选服务商</b>：一般选 DeepSeek；想完全免费、在自己电脑上跑，可以选 Ollama（需另装 Ollama 并下载一个模型）。</li>
  <li><b>复制 Key</b>：点向导里的「去获取 Key」跳到服务商网站 → 注册 / 登录 → 创建 API Key → 复制。
      DeepSeek 进「API keys」页面创建，形如 <code>sk-…</code>；账号需要一点余额（充 10 元能读很多篇）。</li>
  <li><b>粘贴并测试</b>：回到向导粘贴 Key，点「测试连接」，显示成功即可（模型名、端点向导已填好，不用改）。</li>
  <li><b>检查环境</b>：这一步会自动检测内置的 PDF 解析，显示可用就通过了。</li>
</ol>
<div class="card warn">
  Key 只保存在你电脑的 <code>%USERPROFILE%\\PaperHelper\\.paperhelper\\config.toml</code>，
  除了发给该服务商以外不会上传到任何地方，也不会被写进日志。<b>不要</b>把 Key 发给别人或提交到网上。
</div>

<h2><span class="num">3</span>导入论文，开始读（约 1 分钟）</h2>
<ol>
  <li>把论文 PDF <b>直接拖进窗口</b>（或点「笔记」页的「＋ 导入资料」），类别选「论文 → 生成笔记」，等它生成。
      文字版 PDF 直接解析；扫描版需要 OCR（可选组件，见下方常见问题）。</li>
  <li>生成后读笔记：<b>选中一段不懂的文字 → 点浮出的「提问」</b>，答案会钉在原文位置；
      右键笔记标题还能「对本章节提问」；回答里可以继续选中追问，形成树状讨论。</li>
  <li>想省事可以先在向导第 4 步点「导入示例」：内置一篇示例笔记（0 花费）和一篇示例论文，先把流程走一遍。</li>
  <li>随时用「导出」保存为 Markdown / 思维导图 / 自包含 HTML；会话自动保存，下次打开继续。</li>
</ol>

<h2>常见问题</h2>

<details>
  <summary>双击后窗口打不开 / 提示缺少 WebView2？</summary>
  桌面窗口依赖 Windows 自带的 <b>Microsoft Edge WebView2 运行时</b>（Win11 和较新的 Win10 一般都自带）。
  安装程序检测不到时会提示并打开官方下载页，装完再打开 PaperHelper 即可（很小，装完不用重启）。
</details>

<details>
  <summary>怎么更新到新版本？</summary>
  启动时会自动检查（每天至多一次），有新版本顶栏出现「发现新版本」，点开可选「跳过此版本」或更新。
  <b>安装版</b>点「立即更新」即可：自动下载、校验、静默安装并重启，笔记与会话不受影响；
  <b>绿色版</b>点「手动下载」到发布页下载新版压缩包，解压覆盖程序即可（数据在安装目录之外，不会丢）。
  「设置」里也能随时手动检查更新。
</details>

<details>
  <summary>没有 API Key 能用吗？</summary>
  界面、笔记浏览、导出、公式渲染都能用；生成笔记和提问需要模型。想零成本可以选 Ollama：
  在本机装 Ollama 并下载一个模型（如 <code>qwen2.5:7b</code>），向导里选 Ollama 即可，不联网、不花钱。
</details>

<details>
  <summary>扫描版 PDF 读不出文字？</summary>
  纯图片的 PDF 需要 OCR。装 <code>tesseract-ocr</code>（含中文语言包）后，导入时勾选 OCR 即可；
  普通文字版 PDF 不需要，跳过这步完全没问题。
</details>

<details>
  <summary>读一篇论文大概花多少钱？</summary>
  按服务商的 token 单价计费（DeepSeek 很便宜，一篇论文通常几分钱到几毛钱）。
  软件里有「统计」可以看每次调用的 token 与费用，还能设 token 预算，到上限自动停止。
</details>

<details>
  <summary>我的数据放在哪？卸载会丢吗？</summary>
  笔记、会话、配置都在 <code>%USERPROFILE%\\PaperHelper\\.paperhelper</code>（安装目录之外）。
  卸载只删程序，<b>不会</b>删这些数据；想彻底清理手动删这个文件夹即可。
</details>

<details>
  <summary>生成到一半想停下 / 关窗口会怎样？</summary>
  生成时界面有「停止」按钮，按了立刻中断（网络请求和 PDF 解析子进程都会停）。
  直接关窗口也会先中断任务再退出，不会留下后台进程。
</details>

<details>
  <summary>出错了怎么排查？</summary>
  错误提示会给出中文原因（Key 无效 / 端点不对 / 限流 / 超出上下文等），点「详情」能看到原始返回；
  「设置」里还有「测试连接」。日志在 <code>%USERPROFILE%\\PaperHelper\\.paperhelper\\logs\\paperhelper.log</code>。
</details>

<details>
  <summary>能完全离线用吗？</summary>
  可以离线打开界面、看笔记、导出（公式和样式都已内置，不用联网加载）；只有调用在线模型时需要联网。
  用 Ollama 本地模型则全程离线。
</details>

<details>
  <summary>Linux / macOS 用户怎么用？</summary>
  需要 Rust 与 Python：<code>cargo build -p paperhelper</code> 后运行 <code>target/debug/paperhelper web</code>，
  浏览器打开提示的地址即可（详见仓库 README 的「安装与部署」）。
</details>

<footer>
  PaperHelper · 项目主页与更新：<a href="{REPO}">{REPO}</a><br>
  本文件由 <code>scripts/make_quickstart.py</code> 生成（图片已内嵌，可离线打开 / 直接打印）。
</footer>

</div>
</body>
</html>
"""


def main() -> None:
    OUT.write_text(HTML, encoding="utf-8")
    size = OUT.stat().st_size / 1024
    print(f"wrote {OUT.name} ({size:.0f} KB)")


if __name__ == "__main__":
    main()
