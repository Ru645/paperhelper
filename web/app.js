// PaperHelper Web 前端：原生 JS，无构建。
// 后端约定：POST /api/run 返回 SSE（event: stdout/stderr/token/progress/progress_done/done/error，
// data 为 JSON 字符串）。其余接口为普通 JSON。

const $ = (id) => document.getElementById(id);
const consoleEl = $("console");
const noteFrame = $("note-frame");

let running = false;
let streamSpan = null;      // 当前流式 token 的容器
let lastState = null;       // 最近一次 /api/state 快照
let currentAbort = null;    // 当前 /api/run 的 AbortController
let annAbort = null;        // 当前批注请求的 AbortController
let importResolve = null;   // 导入弹窗的 Promise resolve
let editBlockId = null;     // 正在编辑的块 id
let editBlockKind = "paragraph"; // 正在编辑的块类型：paragraph | section | title
let editMode = "manual";    // manual | rewrite | append
let editAbort = null;       // AI 生成时的 AbortController
const dynamicTabs = new Map(); // key -> { btn, pane }
// 批注（选中文字提问）
let annotationsCache = [];
let currentAnnotation = null; // { id, block_id, quote }
let annSelectedNode = null;   // 弹窗内当前选中的节点（追问挂到它下面）
let annMode = "ask";          // ask | check

// ===== 侧栏列表多选（Ctrl/⌘ 点选，Shift 连选，右键批量置顶/删除）=====
const SEL_SEP = "\u0001";
const selection = { sessions: new Set(), papers: new Set(), concepts: new Set() };
const selAnchor = { sessions: null, papers: null, concepts: null };
let lastSessions = [];
let lastPapers = [];
let lastConcepts = [];

function paperKey(p) { return p.id; }
function conceptKey(c) { return c.name + SEL_SEP + (c.paper_id || ""); }

/// 某列表当前渲染顺序对应的 key 数组（Shift 连选用）。
function listKeys(kind) {
  if (kind === "sessions") return lastSessions.map((s) => s.id);
  if (kind === "papers") return lastPapers.map(paperKey);
  return lastConcepts.map(conceptKey);
}

/// 某列表的条目数组。
function listItems(kind) {
  return kind === "sessions" ? lastSessions : kind === "papers" ? lastPapers : lastConcepts;
}

function keyOf(kind, item) {
  return kind === "sessions" ? item.id : kind === "papers" ? paperKey(item) : conceptKey(item);
}

/// 刷新选中样式与「已选 N」角标。
function paintSelection(kind) {
  const ul = $(kind);
  if (!ul) return;
  const sel = selection[kind];
  for (const li of ul.children) {
    const k = li.dataset ? li.dataset.key : null;
    if (k != null) li.classList.toggle("selected", sel.has(k));
  }
  const badge = $(`sel-count-${kind}`);
  if (badge) badge.textContent = sel.size ? `已选 ${sel.size} · Esc 取消` : "";
}

function clearSelection(kind) {
  selection[kind].clear();
  selAnchor[kind] = null;
  paintSelection(kind);
}

function clearAllSelections() {
  for (const k of Object.keys(selection)) clearSelection(k);
}

/// 列表重渲染后清掉已不存在的选中项。
function pruneSelection(kind, validKeys) {
  const valid = new Set(validKeys);
  const sel = selection[kind];
  for (const k of [...sel]) if (!valid.has(k)) sel.delete(k);
  if (selAnchor[kind] != null && !valid.has(selAnchor[kind])) selAnchor[kind] = null;
}

/// 列表项点击：Shift 连选 / Ctrl(⌘) 点选 / 普通点击执行默认动作。
function listClick(kind, e, key, defaultFn) {
  const sel = selection[kind];
  if (e.shiftKey && selAnchor[kind] != null) {
    const keys = listKeys(kind);
    const a = keys.indexOf(selAnchor[kind]);
    const b = keys.indexOf(key);
    if (a >= 0 && b >= 0) {
      const [lo, hi] = a <= b ? [a, b] : [b, a];
      sel.clear();
      for (let i = lo; i <= hi; i++) sel.add(keys[i]);
      paintSelection(kind);
    }
    return;
  }
  if (e.ctrlKey || e.metaKey) {
    if (sel.has(key)) sel.delete(key);
    else sel.add(key);
    selAnchor[kind] = key;
    paintSelection(kind);
    return;
  }
  clearSelection(kind);
  selAnchor[kind] = key;
  defaultFn();
}

/// 右键时：若点在多选集合内，返回选中的条目数组；否则返回 null。
function selectedItems(kind, clickedKey) {
  const sel = selection[kind];
  if (sel.size <= 1 || !sel.has(clickedKey)) return null;
  return listItems(kind).filter((x) => sel.has(keyOf(kind, x)));
}

// ===== 通用工具 =====

function esc(s) {
  return String(s == null ? "" : s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
}

function stripAnsi(s) {
  return String(s).replace(/\x1b\[[0-9;]*m/g, "");
}

function fmtTime(s) {
  if (!s) return "未知时间";
  const d = new Date(s);
  if (isNaN(d.getTime())) return "未知时间";
  const p = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

// ===== 输出渲染 =====

function endStream() { streamSpan = null; }

function appendConsole(text, cls) {
  endStream();
  text = stripAnsi(text);
  if (text === "" || text === undefined) return;
  const div = document.createElement("div");
  if (cls) div.className = cls;
  div.textContent = text;
  consoleEl.appendChild(div);
  consoleEl.scrollTop = consoleEl.scrollHeight;
}

function appendToken(t) {
  if (progressLabel) progressChars += t.length;
  if (!streamSpan) {
    streamSpan = document.createElement("span");
    streamSpan.className = "stream";
    consoleEl.appendChild(streamSpan);
  }
  streamSpan.textContent += t;
  consoleEl.scrollTop = consoleEl.scrollHeight;
}

// ===== 进度指示（顶部动画进度条 + 实时字数/耗时）=====
let progressLabel = "";
let progressStart = 0;
let progressChars = 0;
let progressUploadPct = -1;  // 上传百分比（0~1），-1 表示不显示
let progressTimer = null;

function updateProgressText() {
  const el = $("progress");
  if (!el || !progressLabel) return;
  const secs = Math.round((Date.now() - progressStart) / 1000);
  let meta = secs + "s";
  if (progressUploadPct >= 0) meta = Math.round(progressUploadPct * 100) + "% · " + meta;
  else if (progressChars > 0) meta = progressChars.toLocaleString() + " 字 · " + meta;
  el.innerHTML = `<span class="spin"></span><span>${esc(progressLabel)}</span><b class="meta">${meta}</b>`;
}

/// 开始/更新一个进度提示（空串=结束）。
function setProgress(msg) {
  if (!msg) { stopProgress(); return; }
  if (msg !== progressLabel) {
    progressLabel = msg;
    progressStart = Date.now();
    progressChars = 0;
    progressUploadPct = -1;
  }
  $("progress").classList.remove("hidden");
  $("progress-bar").classList.remove("hidden");
  updateProgressText();
  if (!progressTimer) progressTimer = setInterval(updateProgressText, 400);
}

/// 上传百分比（0~1），不重置已用时间。
function setUploadPct(p) {
  progressUploadPct = p;
  updateProgressText();
}

function stopProgress() {
  progressLabel = "";
  progressChars = 0;
  progressUploadPct = -1;
  $("progress").classList.add("hidden");
  $("progress-bar").classList.add("hidden");
  if (progressTimer) { clearInterval(progressTimer); progressTimer = null; }
}

/// 显示/隐藏某个迷你不定长进度条（弹窗内：配置测试 / 批注提问 / AI 生成）。
function showBar(id, on) {
  const el = $(id);
  if (el) el.classList.toggle("hidden", !on);
}

function setRunning(v) {
  running = v;
  if (!v) setProgress("");
  $("btn-stop").classList.toggle("hidden", !v);
}

// ===== 标签页 =====

function switchTab(key) {
  document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t.dataset.key === key));
  document.querySelectorAll(".pane").forEach((p) => p.classList.toggle("active", p.dataset.key === key));
}

function addDynamicTab(key, title, pane) {
  const btn = document.createElement("button");
  btn.className = "tab";
  btn.dataset.key = key;
  // 标题放进 .tab-label（CSS 省略号截断），关闭按钮独立在外
  btn.innerHTML = `<span class="tab-label">${esc(title)}</span><span class="close" title="关闭">×</span>`;
  btn.onclick = (e) => {
    if (e.target.classList.contains("close")) closeTab(key);
    else switchTab(key);
  };
  $("tabs").insertBefore(btn, $("tab-spacer"));
  pane.dataset.key = key;
  $("panes").appendChild(pane);
  dynamicTabs.set(key, { btn, pane });
}

function closeTab(key) {
  const t = dynamicTabs.get(key);
  if (!t) return;
  t.btn.remove();
  t.pane.remove();
  dynamicTabs.delete(key);
  if (!document.querySelector(".pane.active")) switchTab("note");
}

// ===== SSE =====

/// 解析一帧 SSE：返回 { name, text }（data 为 JSON 字符串时自动解析）。
function parseFrame(frame) {
  let name = "message";
  let data = "";
  for (const line of frame.split("\n")) {
    if (line.startsWith("event:")) name = line.slice(6).trim();
    else if (line.startsWith("data:")) data += line.slice(5).trim();
  }
  let text = data;
  try { text = JSON.parse(data); } catch (e) { /* 原始文本 */ }
  return { name, text };
}

function handleFrame(frame) {
  const { name, text } = parseFrame(frame);
  switch (name) {
    case "stdout": appendConsole(text); break;
    case "stderr": appendConsole(text, "err"); break;
    case "token": appendToken(text); break;
    case "progress": setProgress(text); break;
    case "progress_done": setProgress(""); break;
    case "aborted": appendConsole("⏹ 已中止", "warn"); setProgress(""); break;
    case "error": renderError(text); break;
    case "done": appendConsole("✓ 完成", "ok"); break;
    default: if (text) appendConsole(text);
  }
}

/// 从 error 事件数据里取出 {summary, detail}（兼容对象或字符串）。
function parseError(data) {
  let obj = data;
  if (typeof obj === "string") {
    try { obj = JSON.parse(obj); } catch (e) { obj = null; }
  }
  if (obj && typeof obj === "object" && obj.summary) {
    return { summary: String(obj.summary), detail: String(obj.detail || "") };
  }
  return { summary: typeof data === "string" ? data : JSON.stringify(data), detail: "" };
}

/// 展示错误：中文摘要 + 可展开的「详情」（原始 API Response / 错误链）。
function renderError(data) {
  const { summary, detail } = parseError(data);
  appendConsole("❌ " + summary, "err");
  if (detail && detail !== summary) {
    const det = document.createElement("details");
    det.className = "err-detail";
    const sum = document.createElement("summary");
    sum.textContent = "查看详情（原始响应 / 错误链）";
    const pre = document.createElement("pre");
    pre.textContent = detail;
    det.appendChild(sum);
    det.appendChild(pre);
    consoleEl.appendChild(det);
    consoleEl.scrollTop = consoleEl.scrollHeight;
  }
  switchTab("console");
}

/// 请求中止当前任务：通知后端打断 + 断开本地 SSE 流（立即恢复 UI）。
async function stopCurrent() {
  appendConsole("⏹ 正在中止…", "warn");
  try { await fetch("/api/interrupt", { method: "POST" }); } catch (e) { /* 忽略 */ }
  if (currentAbort) { try { currentAbort.abort(); } catch (e) { /* 忽略 */ } }
  if (annAbort) { try { annAbort.abort(); } catch (e) { /* 忽略 */ } }
}

async function runCommand(command, opts = {}) {
  if (running) return;
  if (!command || !command.trim()) return;
  setRunning(true);
  appendConsole("> " + command, "ok");
  // 不自动跳控制台；仅出错时（handleFrame 的 error）切过去
  const controller = new AbortController();
  currentAbort = controller;
  try {
    const res = await fetch("/api/run", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ command, export: opts.export || null }),
      signal: controller.signal,
    });
    if (!res.ok || !res.body) {
      appendConsole("❌ 请求失败: HTTP " + res.status, "err");
      switchTab("console");
      return;
    }
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buf = "";
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });
      let idx;
      while ((idx = buf.indexOf("\n\n")) >= 0) {
        const frame = buf.slice(0, idx);
        buf = buf.slice(idx + 2);
        if (frame.trim()) handleFrame(frame);
      }
    }
  } catch (e) {
    if (e && e.name === "AbortError") {
      appendConsole("⏹ 已中止", "warn");
    } else {
      appendConsole("❌ 连接中断: " + e, "err");
      switchTab("console");
    }
  } finally {
    if (currentAbort === controller) currentAbort = null;
    setRunning(false);
    await refreshState();
    if (opts.skipReload) {
      // 笔记未变（如 goto）：不重载，直接滚动到目标位置
      if (opts.scrollAnchor) scrollNoteTo(opts.scrollAnchor);
    } else {
      reloadNote(opts.scrollAnchor, opts.keepScroll);
    }
  }
}

// ===== 状态刷新 =====

async function refreshState() {
  try {
    const st = await (await fetch("/api/state")).json();
    lastState = st;
    $("btn-undo").disabled = !st.can_undo;
    renderModel(st);
    renderUsage(st);
    renderPapers(st);
    renderConcepts(st);
    renderNoteEmpty(st);
  } catch (e) {
    console.error(e);
  }
  await refreshSessions();
  await refreshAnnotations();
  renderOutline(lastState);
  applyHighlights();
}

/// 无笔记时隐藏 iframe，显示居中的导入入口（导入只在新笔记时需要）。
function renderNoteEmpty(st) {
  const empty = $("note-empty");
  if (!empty) return;
  const has = !!(st && st.has_note);
  empty.classList.toggle("hidden", has);
  noteFrame.classList.toggle("hidden", !has);
}

function renderModel(st) {
  $("model").innerHTML = `模型 <b>${esc(st.model)}</b>`;
}

function renderUsage(st) {
  const s = st.stats.session;
  const g = st.stats.global;
  const fmt = (n) => (n || 0).toLocaleString();
  const pct = st.context_length > 0 ? ((st.current_input_tokens / st.context_length) * 100).toFixed(1) : "0.0";
  const warn = st.context_length > 0 && st.current_input_tokens / st.context_length >= 0.8 ? " warn" : "";
  let budget = "";
  if (st.budget > 0) {
    const bp = ((st.used_total / st.budget) * 100).toFixed(1);
    budget = `<div class="u-row"><span>预算</span><b>${fmt(st.used_total)} / ${fmt(st.budget)}</b></div>
      <div class="u-sub${bp >= 80 ? " warn" : ""}">已用 ${bp}%</div>`;
  }
  $("usage").innerHTML = `
    <div class="u-row"><span>会话总开销</span><b>$${s.cost.toFixed(4)}</b></div>
    <div class="u-sub">${fmt(s.input)}→${fmt(s.output)} tok · ${s.calls} 次调用</div>
    <div class="u-row"><span>跨会话累计</span><b>$${g.cost.toFixed(4)}</b></div>
    <div class="u-sub">${fmt(g.input)}→${fmt(g.output)} tok</div>
    <div class="u-row"><span>当前节点上下文</span><b>${fmt(st.current_input_tokens)}</b></div>
    <div class="u-sub${warn}">/ ${fmt(st.context_length)} tok (${pct}%)</div>
    ${budget}`;
}

function renderPapers(st) {
  const ul = $("papers");
  const list = st.papers || [];
  lastPapers = list;
  if (list.length === 0) {
    ul.innerHTML = '<li class="muted">（无）</li>';
    pruneSelection("papers", []);
    paintSelection("papers");
    return;
  }
  ul.innerHTML = "";
  pruneSelection("papers", list.map(paperKey));
  for (const p of list) {
    const li = document.createElement("li");
    li.className = "clickable";
    li.dataset.key = paperKey(p);
    if (selection.papers.has(paperKey(p))) li.classList.add("selected");
    const pin = p.pinned ? '<span class="pin" title="已置顶">★</span>' : "";
    li.innerHTML = `${pin}《${esc(p.title)}》`;
    li.title = "点击查看笔记与对应会话 · Ctrl/⌘ 点选、Shift 连选 · 右键更多";
    li.onclick = (e) => listClick("papers", e, paperKey(p), () => openPaperTab(p));
    li.oncontextmenu = (e) => {
      e.preventDefault();
      showPaperMenu(e.clientX, e.clientY, p);
    };
    ul.appendChild(li);
  }
  paintSelection("papers");
}

function renderConcepts(st) {
  const ul = $("concepts");
  const list = st.concepts || [];
  lastConcepts = list;
  if (list.length === 0) {
    ul.innerHTML = '<li class="muted">（无）</li>';
    pruneSelection("concepts", []);
    paintSelection("concepts");
    return;
  }
  ul.innerHTML = "";
  pruneSelection("concepts", list.map(conceptKey));
  for (const c of list) {
    const li = document.createElement("li");
    li.className = "clickable";
    li.dataset.key = conceptKey(c);
    if (selection.concepts.has(conceptKey(c))) li.classList.add("selected");
    const pin = c.pinned ? '<span class="pin" title="已置顶">★</span>' : "";
    li.innerHTML = `${pin}${esc(c.name)}`;
    li.title = "点击查看概念详情 · Ctrl/⌘ 点选、Shift 连选 · 右键更多";
    li.onclick = (e) => listClick("concepts", e, conceptKey(c), () => openConceptTab(c.name));
    li.oncontextmenu = (e) => {
      e.preventDefault();
      showConceptMenu(e.clientX, e.clientY, c);
    };
    ul.appendChild(li);
  }
  paintSelection("concepts");
}

let pendingAnchor = null;
let pendingScrollTop = null;

// iframe 重载完成后，若有待定锚点则滚动定位；若需保持滚动位置则恢复（等 marked/KaTeX 执行）
noteFrame.addEventListener("load", () => {
  if (pendingAnchor !== null) {
    const a = pendingAnchor;
    pendingAnchor = null;
    setTimeout(() => scrollNoteTo(a), 60);
  } else if (pendingScrollTop !== null) {
    const y = pendingScrollTop;
    pendingScrollTop = null;
    setTimeout(() => {
      const w = noteFrame.contentWindow;
      if (w) w.scrollTo(0, y);
    }, 60);
  }
  onNoteLoaded();
});

/// 重载笔记。`anchor` 非空则定位到某解释；`keepScroll=true` 则保持当前滚动位置。
function reloadNote(anchor, keepScroll) {
  pendingAnchor = anchor || null;
  if (keepScroll) {
    const w = noteFrame.contentWindow;
    pendingScrollTop = w ? w.scrollY : 0;
  } else {
    pendingScrollTop = null;
  }
  noteFrame.src = "/api/note?format=html&t=" + Date.now();
}

/// 把笔记 iframe 滚动到某条解释的锚点（expl-<id>）。同源可直接访问其文档。
/// 若锚点在 sum 折叠的 <details> 内，先展开所有祖先 details 再滚动。
function scrollNoteTo(anchor) {
  if (!anchor) return;
  try {
    const doc = noteFrame.contentDocument;
    const el = doc && doc.getElementById("expl-" + anchor);
    if (!el) return;
    let p = el.parentElement;
    while (p) {
      if (p.tagName === "DETAILS") p.open = true;
      p = p.parentElement;
    }
    requestAnimationFrame(() => el.scrollIntoView({ block: "center" }));
  } catch (e) {
    console.error(e);
  }
}

// ===== 概念 / 论文详情标签页 =====

// 主页面按需加载 marked + KaTeX（带 CDN 备源），并复用与笔记相同的“公式保护”策略，
// 使概念详情里的 $...$ / $$...$$ 能正确渲染。
let mathLibsPromise = null;
function loadMathLibs() {
  if (mathLibsPromise) return mathLibsPromise;
  const CDNS = ["https://cdn.jsdelivr.net/npm", "https://registry.npmmirror.com"];
  const url = (cdn, path) => {
    if (cdn.includes("npmmirror")) {
      const slash = path.indexOf("/");
      const pkg = path.slice(0, slash);
      const rest = path.slice(slash + 1);
      const parts = pkg.split("@");
      return `https://registry.npmmirror.com/${parts[0]}/${parts[parts.length - 1]}/files/${rest}`;
    }
    return cdn + "/" + path;
  };
  const loadCss = (path) =>
    new Promise((resolve) => {
      let i = 0;
      const next = () => {
        if (i >= CDNS.length) return resolve(false);
        const l = document.createElement("link");
        l.rel = "stylesheet";
        l.href = url(CDNS[i], path);
        l.onload = () => resolve(true);
        l.onerror = () => { i++; next(); };
        document.head.appendChild(l);
      };
      next();
    });
  const loadScript = (path) =>
    new Promise((resolve) => {
      let i = 0;
      const next = () => {
        if (i >= CDNS.length) return resolve(false);
        const s = document.createElement("script");
        s.src = url(CDNS[i], path);
        s.onload = () => resolve(true);
        s.onerror = () => { i++; next(); };
        document.body.appendChild(s);
      };
      next();
    });
  mathLibsPromise = (async () => {
    await loadCss("katex@0.16.9/dist/katex.min.css");
    await loadScript("marked@12.0.2/marked.min.js");
    await loadScript("katex@0.16.9/dist/katex.min.js");
  })();
  return mathLibsPromise;
}

/// 渲染 Markdown + LaTeX 为 HTML（先抽公式占位符，marked 后再用 KaTeX 回填）。
/// 把 LaTeX 的 \(…\) / \[…\] 定界符统一成 $ / $$（按 ``` 围栏跳过代码块）。
function convertMathDelims(md) {
  const out = [];
  let inCode = false;
  for (const line of String(md).split("\n")) {
    const t = line.trimStart();
    if (t.startsWith("```")) { inCode = !inCode; out.push(line); continue; }
    if (inCode) { out.push(line); continue; }
    out.push(line.replace(/\\\[/g, "$$").replace(/\\\]/g, "$$").replace(/\\\(/g, "$").replace(/\\\)/g, "$"));
  }
  return out.join("\n");
}

function renderMathMarkdown(md) {
  if (!window.marked) return esc(md);
  md = convertMathDelims(md);
  const store = [];
  const token = (i) => "\u2063M" + i + "\u2063";
  let src = String(md).replace(/\$\$([\s\S]*?)\$\$/g, (m, tex) => { store.push([tex, true]); return token(store.length - 1); });
  // 行内公式：遵循 Pandoc 规则——开头 $ 后不能是空白、结尾 $ 前不能是空白，
  // 以免把货币美元（如 "$200 元"）误当成公式定界符。同时跳过转义的 \$
  src = src.replace(/(?<!\\)\$(?!\s)([^$\n]+?)(?<!\s)\$/g, (m, tex) => { store.push([tex, false]); return token(store.length - 1); });
  // 包裹漏加 $ 的裸数学 token（下标/上标，如 p_ij、S_n、s^n_k）；跳过代码围栏与行内代码
  let inFence = false;
  src = String(src).split("\n").map((line) => {
    const t = line.trimStart();
    if (t.startsWith("```")) { inFence = !inFence; return line; }
    if (inFence) return line;
    const codes = [];
    line = line.replace(/`[^`]*`/g, (m) => { codes.push(m); return "\u2063C" + (codes.length - 1) + "\u2063"; });
    line = line.replace(/(?<![\w$\\])(\\?[A-Za-z\u0370-\u03ff][A-Za-z0-9\u0370-\u03ff']*(?:(?:_|\^)(?:\{[^{}]*\}|[A-Za-z0-9\u0370-\u03ff]+))+)(?![\w])/g, (m) => { store.push([m, false]); return token(store.length - 1); });
    line = line.replace(/\u2063C(\d+)\u2063/g, (_, i) => codes[+i]);
    return line;
  }).join("\n");
  let html = marked.parse(src);
  html = html.replace(/\u2063M(\d+)\u2063/g, (_, i) => {
    const entry = store[+i];
    const tex = entry[0].replace(/^(?:[ \t]*>[ \t]?)+/gm, "").trim();
    const display = entry[1];
    if (window.katex) {
      try { return katex.renderToString(tex, { displayMode: display, throwOnError: false }); } catch (e) {}
    }
    const raw = display ? "$$" + tex + "$$" : "$" + tex + "$";
    return raw.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  });
  return html;
}

async function openConceptTab(name) {
  const key = "concept:" + name;
  if (dynamicTabs.has(key)) { switchTab(key); return; }
  const pane = document.createElement("div");
  pane.className = "pane";
  pane.innerHTML = '<div class="detail"><p class="muted">加载中…</p></div>';
  addDynamicTab(key, "概念: " + name, pane);
  switchTab(key);
  try {
    const c = await (await fetch("/api/concept?name=" + encodeURIComponent(name))).json();
    await loadMathLibs();
    const defHtml = renderMathMarkdown(c.definition || "（无）");
    const ansHtml = renderMathMarkdown(c.answer || "（无）");
    const loadBtn = c.session_id
      ? `<button class="mini primary" data-load="${esc(c.session_id)}" data-anchor="${esc(c.explanation_id || "")}">加载该会话</button>`
      : "";
    pane.innerHTML = `<div class="detail">
      <h2>概念：${esc(c.name)}</h2>
      <dl>
        <dt>概念内容</dt><dd class="md">${defHtml}</dd>
        <dt>出自论文</dt><dd>${esc(c.paper_title || "（未知）")}</dd>
        <dt>所属会话</dt><dd>${esc(c.session_name || "（未找到）")} ${c.updated_at ? "· " + esc(fmtTime(c.updated_at)) : ""} ${loadBtn}</dd>
        <dt>当时的提问</dt><dd>${esc(c.question || "（未找到对应会话记录）")}</dd>
        <dt>当时的回答</dt><dd class="answer md">${ansHtml}</dd>
      </dl>
    </div>`;
    pane.querySelectorAll("[data-load]").forEach(
      (b) => (b.onclick = () => loadSession(b.dataset.load, { anchor: b.dataset.anchor || null }))
    );
  } catch (e) {
    pane.innerHTML = `<div class="detail err">加载失败: ${esc(e)}</div>`;
  }
}

async function openPaperTab(p) {
  const key = "paper:" + p.id;
  if (dynamicTabs.has(key)) { switchTab(key); return; }
  const pane = document.createElement("div");
  pane.className = "pane";
  pane.innerHTML = '<div class="detail"><p class="muted">加载中…</p></div>';
  addDynamicTab(key, "论文: " + p.title, pane);
  switchTab(key);
  try {
    const d = await (await fetch("/api/paper?id=" + encodeURIComponent(p.id))).json();
    const loadBtn = d.session_id
      ? `<button class="mini primary" data-load="${esc(d.session_id)}">加载该会话</button>`
      : "";
    const noteBlock = d.session_id
      ? `<div class="note-frame-wrap"><iframe src="/api/paper/note?id=${encodeURIComponent(p.id)}"></iframe></div>`
      : '<p class="muted" style="padding:16px 24px">找不到对应会话的笔记</p>';
    // 论文页：头部固定，笔记 iframe 填满剩余高度，避免「外层滚动套内层滚动」
    pane.innerHTML = `<div class="detail paper">
      <div class="paper-head">
        <h2>《${esc(d.title)}》</h2>
        <p class="meta">会话：${esc(d.session_name || "（未知）")} ${d.updated_at ? "· " + esc(fmtTime(d.updated_at)) : ""} ${loadBtn}</p>
      </div>
      ${noteBlock}
    </div>`;
    pane.querySelectorAll("[data-load]").forEach((b) => (b.onclick = () => loadSession(b.dataset.load, { anchor: null })));
  } catch (e) {
    pane.innerHTML = `<div class="detail err">加载失败: ${esc(e)}</div>`;
  }
}

// ===== 批注（笔记选中文字提问） =====

async function refreshAnnotations() {
  try {
    const d = await (await fetch("/api/annotations")).json();
    annotationsCache = d.annotations || [];
  } catch (e) {
    console.error(e);
  }
}

/// 笔记 iframe 加载后：绑定交互（选区/点击高亮）+ 渲染高亮。
function onNoteLoaded() {
  const doc = noteFrame.contentDocument;
  if (!doc) return;
  if (!doc.__annBound) {
    doc.__annBound = true;
    doc.addEventListener("mouseup", () => setTimeout(showSelButton, 0));
    doc.addEventListener("mouseover", (e) => showBlkEditBtn(e.target));
    doc.addEventListener("mouseleave", () => $("blk-edit-btn").classList.add("hidden"));
    doc.addEventListener("click", (e) => {
      hideCtxMenu();
      const mark = e.target && e.target.closest ? e.target.closest("mark.ann-mark") : null;
      if (mark) {
        e.preventDefault();
        openAnnotationView(mark.dataset.annId);
      }
    });
    doc.addEventListener("contextmenu", (e) => {
      const t = e.target;
      const fr = noteFrame.getBoundingClientRect();
      const mark = t && t.closest ? t.closest("mark.ann-mark") : null;
      if (mark) {
        e.preventDefault();
        showAnnMarkMenu(mark.dataset.annId, fr.left + e.clientX, fr.top + e.clientY);
        return;
      }
      const heading = t && t.closest ? t.closest("h1, h2, h3, h4, h5, h6") : null;
      if (heading) {
        e.preventDefault();
        showHeadingMenu(doc, heading, fr.left + e.clientX, fr.top + e.clientY);
      }
    });
    doc.addEventListener("scroll", () => {
      hideSelButton();
      $("blk-edit-btn").classList.add("hidden");
    }, true);
  }
  applyHighlights();
}

/// 右键标题：对该章节（h1=全文）提问 / 打开或删除已有批注。
function showHeadingMenu(doc, heading, x, y) {
  const blockId = blockIdForNode(doc, heading);
  if (!blockId) return;
  const text = (heading.textContent || "").trim();
  const items = [{ label: "对本章节提问", fn: () => openAnnotationCreate(blockId, text) }];
  const ann = (annotationsCache || []).find((a) => a.block_id === blockId);
  if (ann) {
    items.unshift({ label: "打开批注", fn: () => openAnnotationView(ann.id) });
    items.push({ label: "删除该批注", danger: true, fn: () => deleteAnnotation(ann.id) });
  }
  showMenu(x, y, items);
}

/// 右键高亮文字：打开 / 删除整条批注。
function showAnnMarkMenu(annId, x, y) {
  showMenu(x, y, [
    { label: "打开批注", fn: () => openAnnotationView(annId) },
    { label: "删除该批注", danger: true, fn: () => deleteAnnotation(annId) },
  ]);
}

async function deleteAnnotation(annId) {
  if (!confirm("删除该批注及其全部问答？（可用顶栏「撤销」恢复）")) return;
  try {
    await postJson("/api/annotate/delete", { annotation_id: annId });
  } catch (e) {
    appendConsole("❌ 删除批注失败: " + e.message, "err");
  }
  if (currentAnnotation && currentAnnotation.id === annId) closeAnnPopup();
  await refreshState();
  reloadNote(null, true); // 保持当前滚动位置
}

// ---- 高亮 ----

function applyHighlights() {
  const doc = noteFrame.contentDocument;
  if (!doc) return;
  const note = doc.getElementById("note");
  if (!note) return;
  // 清除旧高亮（重载/刷新后避免重复包裹）
  doc.querySelectorAll("mark.ann-mark").forEach((m) => {
    const p = m.parentNode;
    while (m.firstChild) p.insertBefore(m.firstChild, m);
    p.removeChild(m);
    p.normalize();
  });
  for (const ann of annotationsCache) highlightAnnotation(doc, note, ann);
}

function highlightAnnotation(doc, note, ann) {
  const anchor = doc.getElementById("blk-" + ann.block_id);
  if (!anchor) return;
  // 块范围：[块锚点, 下一个块锚点)
  const anchors = note.querySelectorAll('a[id^="blk-"]');
  let next = null;
  for (const a of anchors) {
    if (a.compareDocumentPosition(anchor) & Node.DOCUMENT_POSITION_PRECEDING) { next = a; break; }
  }
  const range = doc.createRange();
  range.setStartAfter(anchor);
  if (next) range.setEndBefore(next);
  else range.setEnd(note, note.childNodes.length);
  wrapQuote(doc, range, ann.quote, ann.id);
}

/// 在 range 内查找 quote 文本并包成 <mark>（跨文本节点时切分包裹）。
function wrapQuote(doc, range, quote, annId) {
  if (!quote) return;
  const walker = doc.createTreeWalker(range.commonAncestorContainer, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => (range.intersectsNode(n) ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT),
  });
  const nodes = [];
  while (walker.nextNode()) nodes.push(walker.currentNode);
  const full = nodes.map((n) => n.nodeValue).join("");
  const idx = full.indexOf(quote);
  if (idx < 0) return;
  let acc = 0, startNode = null, startOffset = 0, endNode = null, endOffset = 0;
  for (const n of nodes) {
    const len = n.nodeValue.length;
    if (!startNode && idx < acc + len) { startNode = n; startOffset = idx - acc; }
    if (!endNode && idx + quote.length <= acc + len) { endNode = n; endOffset = idx + quote.length - acc; break; }
    acc += len;
  }
  if (!startNode || !endNode) return;
  const r = doc.createRange();
  r.setStart(startNode, startOffset);
  r.setEnd(endNode, endOffset);
  const mark = doc.createElement("mark");
  mark.className = "ann-mark";
  mark.dataset.annId = annId;
  try {
    r.surroundContents(mark);
  } catch (e) {
    try {
      const frag = r.extractContents();
      mark.appendChild(frag);
      r.insertNode(mark);
    } catch (e2) { /* 放弃该高亮 */ }
  }
}

// ---- 选区 → “提问”按钮 ----

function blockIdForNode(doc, node) {
  const note = doc.getElementById("note");
  if (!note || !note.contains(node)) return null;
  // 标题可能把 blk- 锚点包在内部，优先用它
  const inner = node.querySelector ? node.querySelector('a[id^="blk-"]') : null;
  if (inner) return inner.id.slice(4);
  let best = null;
  for (const a of note.querySelectorAll('a[id^="blk-"]')) {
    const pos = a.compareDocumentPosition(node);
    if (pos & Node.DOCUMENT_POSITION_FOLLOWING) best = a;
    else if (pos & Node.DOCUMENT_POSITION_PRECEDING) break;
  }
  return best ? best.id.slice(4) : null;
}

function hideSelButton() { $("sel-btn").classList.add("hidden"); }

function showSelButton() {
  const doc = noteFrame.contentDocument;
  if (!doc) return;
  const sel = doc.getSelection();
  if (!sel || sel.isCollapsed || !sel.rangeCount) return hideSelButton();
  const text = sel.toString().trim();
  if (!text) return hideSelButton();
  const range = sel.getRangeAt(0);
  const blockId = blockIdForNode(doc, range.startContainer);
  if (!blockId) return hideSelButton();
  const rect = range.getBoundingClientRect();
  const fr = noteFrame.getBoundingClientRect();
  const btn = $("sel-btn");
  btn.classList.remove("hidden");
  btn.style.left = Math.min(window.innerWidth - 60, Math.max(8, fr.left + rect.left)) + "px";
  btn.style.top = Math.min(window.innerHeight - 40, Math.max(8, fr.top + rect.bottom + 6)) + "px";
  btn.onmousedown = (e) => e.preventDefault();
  btn.onclick = () => {
    hideSelButton();
    openAnnotationCreate(blockId, text);
  };
}

// ---- 笔记块编辑（悬浮 ✎ → 弹窗：手动 / AI 重写 / AI 补充）----

/// 找到某块的展示元素：段落锚点在 <p> 内（与文字同段）；章节锚点常被单独包在
/// 空 <p> 里，此时取它的下一个元素（真正的标题）。
function blockElementFor(doc, id) {
  const anchor = doc.getElementById("blk-" + id);
  if (!anchor) return null;
  const parent = anchor.parentElement;
  if (!parent) return null;
  if (parent.tagName === "P" && !parent.textContent.trim()) {
    return parent.nextElementSibling || parent;
  }
  return parent;
}

/// 悬停到某块上时，把「✎ 编辑」按钮浮到该块右上角。
function showBlkEditBtn(target) {
  const doc = noteFrame.contentDocument;
  if (!doc || !target) return;
  const btn = $("blk-edit-btn");
  const note = doc.getElementById("note");
  if (!note || !note.contains(target)) { btn.classList.add("hidden"); return; }
  const id = blockIdForNode(doc, target);
  if (!id) { btn.classList.add("hidden"); return; }
  const el = blockElementFor(doc, id);
  if (!el) { btn.classList.add("hidden"); return; }
  const r = el.getBoundingClientRect();
  const fr = noteFrame.getBoundingClientRect();
  const top = fr.top + r.top;
  if (top < fr.top - 8 || top > fr.bottom - 8) { btn.classList.add("hidden"); return; }
  btn.classList.remove("hidden");
  btn.style.left = Math.max(8, Math.min(window.innerWidth - 90, fr.left + r.right - 72)) + "px";
  btn.style.top = Math.max(6, top + 2) + "px";
  btn.dataset.blockId = id;
  btn.onmousedown = (e) => e.preventDefault();
  btn.onclick = () => openEditModal(id);
}

function setEditTab(mode) {
  editMode = mode;
  document.querySelectorAll(".edit-tab").forEach((t) =>
    t.classList.toggle("active", t.dataset.mode === mode)
  );
  $("edit-ai-row").classList.toggle("hidden", mode === "manual");
  $("btn-edit-apply").textContent = mode === "append" ? "插入" : "应用";
}

async function openEditModal(id) {
  editBlockId = id;
  setEditTab("manual");
  $("edit-title").textContent = "编辑笔记块";
  $("edit-hint").textContent = "加载中…";
  $("edit-text").value = "";
  $("edit-status").textContent = "";
  $("edit-status").className = "status";
  $("edit-modal").classList.remove("hidden");
  try {
    const res = await fetch("/api/note/block?id=" + encodeURIComponent(id));
    if (!res.ok) throw new Error(await res.text());
    const b = await res.json();
    const isTitle = id === "__title__";
    editBlockKind = isTitle ? "title" : b.kind;
    // 章节：编辑框展示「标题 + 全部子块」；段落/标题：只有自身文字
    $("edit-text").value = editBlockKind === "section" && b.markdown ? b.markdown : (b.text || "");
    const kind = isTitle ? "标题" : b.kind === "section" ? `章节 ${b.number || ""}`.trim() : "段落";
    $("edit-title").textContent = "编辑" + kind;
    const parts = [];
    if (b.explanations) parts.push(`${b.explanations} 条追问`);
    if (b.children) parts.push(`${b.children} 个子块`);
    if (editBlockKind === "section") {
      $("edit-hint").textContent = parts.length
        ? `编辑框含本节标题与全部内容（${parts.join(" / ")}）：应用后会整体重写本节，子块的批注需重做。`
        : "编辑框含本节标题与内容；应用后会整体重写本节。";
    } else {
      $("edit-hint").textContent = parts.length
        ? `该块含 ${parts.join(" / ")}：改文字不影响它们。`
        : "支持 Markdown；数学公式用 $...$ 或 $$...$$。";
    }
    $("btn-edit-delete").classList.toggle("hidden", isTitle);
    $("edit-text").focus();
  } catch (e) {
    $("edit-status").textContent = "❌ " + e.message;
    $("edit-status").className = "status err";
  }
}

function closeEditModal() {
  if (editAbort) {
    try { editAbort.abort(); } catch (e) { /* 忽略 */ }
    editAbort = null;
  }
  $("edit-modal").classList.add("hidden");
  $("blk-edit-btn").classList.add("hidden");
}

async function generateEdit() {
  if (!editBlockId) return;
  const instruction = $("edit-instruction").value.trim();
  const st = $("edit-status");
  if (!instruction) {
    st.textContent = "请先填写要求，如「补充直觉解释」";
    st.className = "status err";
    return;
  }
  const mode = editMode === "append" ? "append" : "rewrite";
  st.textContent = "AI 生成中…";
  st.className = "status";
  $("edit-text").value = "";
  $("btn-edit-gen").disabled = true;
  $("btn-edit-stop").classList.remove("hidden");
  showBar("edit-ai-bar", true);
  const ctrl = new AbortController();
  editAbort = ctrl;
  let acc = "";
  try {
    await postSse("/api/note/ai", { block_id: editBlockId, instruction, mode }, ({ name, text }) => {
      if (name === "token") {
        if (!acc) showBar("edit-ai-bar", false); // 有内容流出即收起进度条
        acc += text;
        $("edit-text").value = acc;
        $("edit-text").scrollTop = $("edit-text").scrollHeight;
      } else if (name === "progress") {
        st.textContent = text;
      } else if (name === "stderr") {
        appendConsole(text, "warn");
      } else if (name === "error") {
        const { summary } = parseError(text);
        st.textContent = "❌ " + summary;
        st.className = "status err";
      } else if (name === "aborted") {
        st.textContent = "⏹ 已中止";
        st.className = "status";
      }
    }, { signal: ctrl.signal });
    if (acc && !st.classList.contains("err")) {
      st.textContent = "✓ 已生成（可修改后点" + (mode === "append" ? "「插入」" : "「应用」") + "）";
      st.className = "status ok";
    }
  } catch (e) {
    if (e && e.name === "AbortError") {
      st.textContent = "⏹ 已中止";
      st.className = "status";
    } else {
      st.textContent = "❌ " + e.message;
      st.className = "status err";
    }
  }
  editAbort = null;
  showBar("edit-ai-bar", false);
  $("btn-edit-gen").disabled = false;
  $("btn-edit-stop").classList.add("hidden");
}

function stopEditGen() {
  fetch("/api/interrupt", { method: "POST" }).catch(() => {});
  if (editAbort) {
    try { editAbort.abort(); } catch (e) { /* 忽略 */ }
  }
}

/// `forceInsert=true` 或处于「AI 补充」页签时插入到该块后，否则按当前页签保存。
async function applyEdit(forceInsert) {
  if (!editBlockId) return;
  const text = $("edit-text").value.trim();
  const st = $("edit-status");
  if (!text) {
    st.textContent = "内容不能为空";
    st.className = "status err";
    return;
  }
  let url = "/api/note/edit";
  let body = { block_id: editBlockId, text };
  if (forceInsert || editMode === "append") {
    url = "/api/note/add";
    body = { after_block_id: editBlockId, text };
  } else if (editMode === "rewrite" || editBlockKind === "section") {
    // 章节的手动编辑 = 整体重写（编辑框里是「标题 + 全部子块」）
    url = "/api/note/rewrite";
  }
  st.textContent = "保存中…";
  st.className = "status";
  try {
    await postJson(url, body);
    st.textContent = "✓ 已保存";
    st.className = "status ok";
    closeEditModal();
    await refreshState();
    reloadNote(null, true);
  } catch (e) {
    st.textContent = "❌ " + e.message;
    st.className = "status err";
  }
}

async function deleteEditBlock() {
  if (!editBlockId || editBlockId === "__title__") return;
  const hint = $("edit-hint").textContent || "";
  if (!confirm("删除该块及其子树？子块与追问会一并删除，相关批注也会移除。\n" + hint)) return;
  try {
    await postJson("/api/note/delete", { block_id: editBlockId });
    closeEditModal();
    await refreshState();
    reloadNote(null, true);
  } catch (e) {
    $("edit-status").textContent = "❌ " + e.message;
    $("edit-status").className = "status err";
  }
}

// ---- 弹窗 ----

function positionPopup(x, y) {
  const el = $("ann-popup");
  const w = 380, h = 460;
  el.style.left = Math.min(window.innerWidth - w - 12, Math.max(12, x)) + "px";
  el.style.top = Math.min(window.innerHeight - h - 12, Math.max(12, y)) + "px";
}

async function openAnnotationCreate(blockId, quote) {
  currentAnnotation = { id: null, block_id: blockId, quote };
  annSelectedNode = null;
  await loadMathLibs();
  $("ann-quote").textContent = quote;
  $("ann-thread").innerHTML = '<p class="muted">输入问题后回车发送；这会在该处创建一条批注。</p>';
  $("ann-popup").classList.remove("hidden");
  const doc = noteFrame.contentDocument;
  const sel = doc && doc.getSelection();
  const rect = sel && sel.rangeCount ? sel.getRangeAt(0).getBoundingClientRect() : { left: 200, bottom: 200 };
  const fr = noteFrame.getBoundingClientRect();
  positionPopup(fr.left + rect.left, fr.top + rect.bottom + 10);
  $("ann-q").value = "";
  $("ann-q").focus();
}

async function openAnnotationView(annId) {
  await refreshAnnotations();
  const ann = annotationsCache.find((a) => a.id === annId);
  if (!ann) return;
  currentAnnotation = { id: ann.id, block_id: ann.block_id, quote: ann.quote };
  annSelectedNode = ann.thread ? ann.thread.node_id : null;
  await loadMathLibs();
  $("ann-quote").textContent = ann.quote;
  renderAnnThread(ann.thread);
  $("ann-popup").classList.remove("hidden");
  const doc = noteFrame.contentDocument;
  const mark = doc && doc.querySelector(`mark.ann-mark[data-ann-id="${annId}"]`);
  if (mark) {
    const r = mark.getBoundingClientRect();
    const fr = noteFrame.getBoundingClientRect();
    positionPopup(fr.left + r.left, fr.top + r.bottom + 10);
  } else {
    positionPopup(window.innerWidth / 2 - 190, 120);
  }
  $("ann-q").focus();
}

function renderAnnThread(root) {
  const box = $("ann-thread");
  box.innerHTML = "";
  if (!root) { box.innerHTML = '<p class="muted">（尚无问答）</p>'; return; }
  const add = (node, depth, container) => {
    const div = document.createElement("div");
    div.className = "ann-node" + (node.is_check ? " check" : "") + (annSelectedNode === node.node_id ? " selected" : "");
    div.style.marginLeft = depth * 10 + "px";

    const q = document.createElement("div");
    q.className = "ann-q";
    q.textContent = (node.is_check ? "[核对] " : "") + node.question;
    const a = document.createElement("div");
    a.className = "ann-a";
    a.innerHTML = renderMathMarkdown(node.answer || "");

    if (node.summary) {
      // 总结节点：显示总结，折叠原对话（点击展开）
      div.classList.add("summary");
      const sum = document.createElement("div");
      sum.className = "ann-summary";
      sum.innerHTML = renderMathMarkdown(node.summary);
      const hint = document.createElement("div");
      hint.className = "ann-summary-hint";
      hint.textContent = "▶ 展开原对话";
      const orig = document.createElement("div");
      orig.className = "ann-original";
      orig.style.display = "none";
      orig.appendChild(q);
      orig.appendChild(a);
      (node.children || []).forEach((c) => add(c, depth + 1, orig));
      div.appendChild(sum);
      div.appendChild(hint);
      div.appendChild(orig);
      div.onclick = (e) => {
        e.stopPropagation();
        const open = orig.style.display !== "none";
        orig.style.display = open ? "none" : "block";
        hint.textContent = open ? "▶ 展开原对话" : "▼ 收起";
      };
      div.oncontextmenu = (e) => {
        e.preventDefault();
        showAnnNodeMenu(e.clientX, e.clientY, node);
      };
      container.appendChild(div);
      return;
    }

    // 普通节点
    div.appendChild(q);
    div.appendChild(a);
    div.onclick = () => {
      annSelectedNode = node.node_id;
      renderAnnThread(root);
      runCommand("goto " + node.n, { skipReload: true });
    };
    div.oncontextmenu = (e) => {
      e.preventDefault();
      showAnnNodeMenu(e.clientX, e.clientY, node);
    };
    container.appendChild(div);
    (node.children || []).forEach((c) => add(c, depth + 1, container));
  };
  add(root, 0, box);
}

function showAnnNodeMenu(x, y, node) {
  showMenu(x, y, [
    { label: "删除该节点及子树", danger: true, fn: () => annDeleteNode(node) },
    { label: "总结该子树", fn: () => annSumNode(node) },
  ]);
}

async function annDeleteNode(node) {
  if (!confirm(`删除节点「${(node.question || "").slice(0, 20)}」及其子节点？`)) return;
  await runCommand("del " + node.n + " --yes", { skipReload: true });
  await afterAnnotationChange();
}

async function annSumNode(node) {
  await runCommand("sum " + node.n, { skipReload: true });
  await afterAnnotationChange();
}

async function afterAnnotationChange() {
  await refreshState();
  await refreshAnnotations();
  if (currentAnnotation && currentAnnotation.id) {
    const ann = annotationsCache.find((a) => a.id === currentAnnotation.id);
    if (ann) renderAnnThread(ann.thread);
    else closeAnnPopup();
  }
  reloadNote(null, true);
}

function closeAnnPopup() {
  $("ann-popup").classList.add("hidden");
  currentAnnotation = null;
  annSelectedNode = null;
}

/// 乐观追加一个待回答节点（问题 + 思考中…），返回回答元素供流式填充。
function appendPendingNode(question) {
  const box = $("ann-thread");
  const muted = box.querySelector(".muted");
  if (muted) box.innerHTML = "";
  const div = document.createElement("div");
  div.className = "ann-node pending";
  const q = document.createElement("div");
  q.className = "ann-q";
  q.textContent = question;
  const a = document.createElement("div");
  a.className = "ann-a";
  a.textContent = "思考中…";
  div.appendChild(q);
  div.appendChild(a);
  box.appendChild(div);
  box.scrollTop = box.scrollHeight;
  return a;
}

async function sendAnnotation() {
  const q = $("ann-q").value.trim();
  if (!q || !currentAnnotation) return;
  $("ann-q").value = "";
  $("ann-send").disabled = true;
  $("ann-progress").textContent = "思考中…";
  const ansEl = appendPendingNode(q);
  let url, body;
  if (!currentAnnotation.id) {
    url = "/api/annotate";
    body = { block_id: currentAnnotation.block_id, quote: currentAnnotation.quote, question: q, mode: annMode };
  } else {
    const nodeId = annSelectedNode;
    if (!nodeId) { $("ann-send").disabled = false; $("ann-progress").textContent = ""; return; }
    url = "/api/annotate/reply";
    body = { node_id: nodeId, question: q, mode: annMode };
  }
  let streamed = "";
  const controller = new AbortController();
  annAbort = controller;
  $("ann-stop").classList.remove("hidden");
  showBar("ann-bar", true);
  try {
    await postSse(url, body, ({ name, text }) => {
      if (name === "error") {
        const { summary, detail } = parseError(text);
        appendConsole("❌ " + summary, "err");
        ansEl.textContent = "（出错：" + summary + "）";
        if (detail) appendConsole("详情：\n" + detail);
      } else if (name === "aborted") {
        appendConsole("⏹ 已中止", "warn");
        ansEl.textContent = "（已中止）";
      } else if (name === "token") {
        if (!streamed) showBar("ann-bar", false); // 有内容流出即收起进度条
        streamed += text;
        ansEl.textContent = streamed;
        $("ann-thread").scrollTop = $("ann-thread").scrollHeight;
      } else if (name === "stdout") {
        appendConsole(text);
      } else if (name === "stderr") {
        appendConsole(text, "err");
      } else if (name === "progress") {
        $("ann-progress").textContent = text;   // 进度显示在弹窗右上角
      } else if (name === "progress_done") {
        $("ann-progress").textContent = "";
      }
    }, { signal: controller.signal });
  } catch (e) {
    if (e && e.name === "AbortError") {
      appendConsole("⏹ 已中止", "warn");
      ansEl.textContent = "（已中止）";
    } else {
      appendConsole("❌ " + e, "err");
      ansEl.textContent = "（出错：" + e + "）";
    }
  }
  if (annAbort === controller) annAbort = null;
  showBar("ann-bar", false);
  $("ann-stop").classList.add("hidden");
  $("ann-send").disabled = false;
  $("ann-progress").textContent = "";
  await refreshState();
  await refreshAnnotations();
  if (currentAnnotation.id) {
    const ann = annotationsCache.find((a) => a.id === currentAnnotation.id);
    if (ann) {
      currentAnnotation = { id: ann.id, block_id: ann.block_id, quote: ann.quote };
      renderAnnThread(ann.thread);
    }
  } else {
    // 新建：匹配最新一条（同块同引用）
    const latest = annotationsCache[annotationsCache.length - 1];
    if (latest && latest.block_id === currentAnnotation.block_id && latest.quote === currentAnnotation.quote) {
      currentAnnotation = { id: latest.id, block_id: latest.block_id, quote: latest.quote };
      annSelectedNode = latest.thread ? latest.thread.node_id : null;
      renderAnnThread(latest.thread);
    }
  }
  reloadNote(null, true);
}

/// 向返回 SSE 的接口发 POST，逐帧回调。
async function postSse(url, body, onEvent, opts = {}) {
  const res = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
    signal: opts.signal,
  });
  if (!res.ok || !res.body) throw new Error("HTTP " + res.status);
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    let idx;
    while ((idx = buf.indexOf("\n\n")) >= 0) {
      const frame = buf.slice(0, idx);
      buf = buf.slice(idx + 2);
      if (frame.trim()) onEvent(parseFrame(frame));
    }
  }
}

// ===== 会话历史 =====

async function refreshSessions() {
  try {
    const data = await (await fetch("/api/sessions")).json();
    renderSessions(data.sessions || []);
  } catch (e) {
    console.error(e);
  }
}

function renderSessions(list) {
  lastSessions = list;
  const ul = $("sessions");
  ul.innerHTML = "";
  // 永远置顶的“新会话”
  const newLi = document.createElement("li");
  newLi.className = "session-new";
  newLi.innerHTML = '<span class="s-name">＋ 新会话</span>';
  newLi.title = "创建新会话";
  newLi.onclick = () => runCommand("new");
  ul.appendChild(newLi);

  if (!list.length) {
    const empty = document.createElement("li");
    empty.className = "muted";
    empty.textContent = "（无历史会话）";
    ul.appendChild(empty);
    pruneSelection("sessions", []);
    paintSelection("sessions");
    return;
  }
  pruneSelection("sessions", list.map((s) => s.id));
  const activeId = lastState && lastState.session_id ? lastState.session_id : null;
  for (const s of list) {
    const li = document.createElement("li");
    li.dataset.id = s.id;
    li.dataset.key = s.id;
    if (s.id === activeId) li.classList.add("active");
    if (selection.sessions.has(s.id)) li.classList.add("selected");
    const pin = s.pinned ? '<span class="pin" title="已置顶">★</span>' : "";
    li.innerHTML = `${pin}<span class="s-name">${esc(s.name)}</span><span class="s-time">${esc(fmtTime(s.updated_at))}</span>`;
    li.title = "点击加载 · Ctrl/⌘ 点选、Shift 连选 · 右键更多";
    li.onclick = (e) => listClick("sessions", e, s.id, () => loadSession(s.id));
    li.oncontextmenu = (e) => {
      e.preventDefault();
      showSessionMenu(e.clientX, e.clientY, s);
    };
    ul.appendChild(li);
  }
  paintSelection("sessions");
}

async function loadSession(id, opts = {}) {
  const res = await fetch("/api/sessions/load", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ id }),
  });
  if (res.ok) {
    await refreshState();
    switchTab("note");
    // 概念加载传锚点（定位到该问答）；论文/列表加载传 null（回到笔记开头）
    reloadNote(opts.anchor || null);
  } else {
    appendConsole("❌ 加载失败: " + (await res.text()), "err");
  }
}

/// 通用右键菜单：items = [{ label, danger, fn }]
function showMenu(x, y, items) {
  const menu = $("ctx-menu");
  menu.innerHTML = "";
  for (const it of items) {
    const d = document.createElement("div");
    d.textContent = it.label;
    if (it.danger) d.className = "danger";
    d.onclick = () => { hideCtxMenu(); it.fn(); };
    menu.appendChild(d);
  }
  menu.style.left = x + "px";
  menu.style.top = y + "px";
  menu.classList.remove("hidden");
}

function showSessionMenu(x, y, s) {
  const bulk = selectedItems("sessions", s.id);
  if (bulk) {
    const allPinned = bulk.every((it) => it.pinned);
    showMenu(x, y, [
      {
        label: `${allPinned ? "取消置顶" : "置顶"}选中 ${bulk.length} 项`,
        fn: () => pinSessionsBulk(bulk, !allPinned),
      },
      { label: `删除选中 ${bulk.length} 项…`, danger: true, fn: () => deleteSessionsBulk(bulk) },
    ]);
    return;
  }
  clearSelection("sessions");
  showMenu(x, y, [
    { label: s.pinned ? "取消置顶" : "置顶会话", fn: () => pinSession(s.id, !s.pinned) },
    { label: "重命名", fn: () => renameSession(s) },
    { label: "删除会话", danger: true, fn: () => deleteSession(s) },
  ]);
}

function showPaperMenu(x, y, p) {
  const bulk = selectedItems("papers", paperKey(p));
  if (bulk) {
    const allPinned = bulk.every((it) => it.pinned);
    showMenu(x, y, [
      {
        label: `${allPinned ? "取消置顶" : "置顶"}选中 ${bulk.length} 项`,
        fn: () => pinPapersBulk(bulk, !allPinned),
      },
      { label: `删除选中 ${bulk.length} 项…`, danger: true, fn: () => deletePapersBulk(bulk) },
    ]);
    return;
  }
  clearSelection("papers");
  showMenu(x, y, [
    { label: p.pinned ? "取消置顶" : "置顶论文", fn: () => pinPaper(p, !p.pinned) },
    { label: "删除论文", danger: true, fn: () => deletePaper(p) },
  ]);
}

function showConceptMenu(x, y, c) {
  const bulk = selectedItems("concepts", conceptKey(c));
  if (bulk) {
    const allPinned = bulk.every((it) => it.pinned);
    showMenu(x, y, [
      {
        label: `${allPinned ? "取消置顶" : "置顶"}选中 ${bulk.length} 项`,
        fn: () => pinConceptsBulk(bulk, !allPinned),
      },
      { label: `删除选中 ${bulk.length} 项…`, danger: true, fn: () => deleteConceptsBulk(bulk) },
    ]);
    return;
  }
  clearSelection("concepts");
  showMenu(x, y, [
    { label: c.pinned ? "取消置顶" : "置顶概念", fn: () => pinConcept(c, !c.pinned) },
    { label: "删除概念", danger: true, fn: () => deleteConcept(c) },
  ]);
}

async function postJson(url, body) {
  const res = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(await res.text());
  return res.json();
}

async function pinPaper(p, pinned) {
  try { await postJson("/api/kb/paper/pin", { id: p.id, pinned }); } catch (e) { appendConsole("❌ " + e.message, "err"); }
  refreshState();
}

async function deletePaper(p) {
  if (!confirm(`从知识库移除论文《${p.title}》？\n（不影响对应会话与笔记）`)) return;
  try { await postJson("/api/kb/paper/delete", { id: p.id }); } catch (e) { appendConsole("❌ " + e.message, "err"); }
  refreshState();
}

async function pinConcept(c, pinned) {
  try { await postJson("/api/kb/concept/pin", { name: c.name, paper_id: c.paper_id, pinned }); } catch (e) { appendConsole("❌ " + e.message, "err"); }
  refreshState();
}

async function deleteConcept(c) {
  if (!confirm(`从知识库移除概念「${c.name}」？`)) return;
  try { await postJson("/api/kb/concept/delete", { name: c.name, paper_id: c.paper_id }); } catch (e) { appendConsole("❌ " + e.message, "err"); }
  refreshState();
}

function hideCtxMenu() { $("ctx-menu").classList.add("hidden"); }

async function pinSession(id, pinned) {
  await fetch("/api/sessions/pin", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ id, pinned }),
  });
  refreshSessions();
}

async function renameSession(s) {
  const name = prompt("重命名会话", s.name);
  if (name == null) return;
  const res = await fetch("/api/sessions/rename", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ id: s.id, name }),
  });
  if (!res.ok) appendConsole("❌ 重命名失败: " + (await res.text()), "err");
  refreshSessions();
}

async function deleteSession(s) {
  if (!confirm(`删除会话「${s.name}」？此操作不可恢复。`)) return;
  const res = await fetch("/api/sessions/delete", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ id: s.id }),
  });
  if (!res.ok) {
    appendConsole("❌ 删除失败: " + (await res.text()), "err");
    return;
  }
  const j = await res.json();
  if (j.reset) {
    // 删除的是当前会话：后端已重置为空会话
    await refreshState();
    reloadNote();
  } else {
    refreshSessions();
  }
}

// ===== 批量置顶 / 删除（多选后右键）=====

async function pinSessionsBulk(items, pinned) {
  for (const s of items) {
    try { await postJson("/api/sessions/pin", { id: s.id, pinned }); }
    catch (e) { appendConsole("❌ 置顶失败: " + e.message, "err"); }
  }
  clearSelection("sessions");
  refreshSessions();
}

async function deleteSessionsBulk(items) {
  if (!confirm(`删除选中的 ${items.length} 个会话？此操作不可恢复。`)) return;
  for (const s of items) {
    try { await postJson("/api/sessions/delete", { id: s.id }); }
    catch (e) { appendConsole("❌ 删除失败: " + e.message, "err"); }
  }
  clearSelection("sessions");
  await refreshState();
  reloadNote();
}

async function pinPapersBulk(items, pinned) {
  for (const p of items) {
    try { await postJson("/api/kb/paper/pin", { id: p.id, pinned }); }
    catch (e) { appendConsole("❌ 置顶失败: " + e.message, "err"); }
  }
  clearSelection("papers");
  refreshState();
}

async function deletePapersBulk(items) {
  if (!confirm(`从知识库移除选中的 ${items.length} 篇论文？\n（不影响对应会话与笔记）`)) return;
  for (const p of items) {
    try { await postJson("/api/kb/paper/delete", { id: p.id }); }
    catch (e) { appendConsole("❌ 删除失败: " + e.message, "err"); }
  }
  clearSelection("papers");
  refreshState();
}

async function pinConceptsBulk(items, pinned) {
  for (const c of items) {
    try { await postJson("/api/kb/concept/pin", { name: c.name, paper_id: c.paper_id, pinned }); }
    catch (e) { appendConsole("❌ 置顶失败: " + e.message, "err"); }
  }
  clearSelection("concepts");
  refreshState();
}

async function deleteConceptsBulk(items) {
  if (!confirm(`从知识库移除选中的 ${items.length} 个概念？`)) return;
  for (const c of items) {
    try { await postJson("/api/kb/concept/delete", { name: c.name, paper_id: c.paper_id }); }
    catch (e) { appendConsole("❌ 删除失败: " + e.message, "err"); }
  }
  clearSelection("concepts");
  refreshState();
}

// ===== 顶栏操作：导出 / 撤销 / 帮助 =====

function downloadExport(fmt) {
  if (!lastState || !lastState.has_note) { alert("还没有笔记"); return; }
  const a = document.createElement("a");
  a.href = "/api/export?format=" + encodeURIComponent(fmt);
  a.download = "";
  document.body.appendChild(a);
  a.click();
  a.remove();
}

// ===== 笔记结构大纲 =====

function renderOutline(st) {
  const ul = $("outline");
  ul.innerHTML = "";
  if (!st || !st.has_note || !st.blocks || st.blocks.length === 0) {
    ul.innerHTML = '<li class="muted">（还没有笔记）</li>';
    return;
  }
  const annBlocks = new Set((annotationsCache || []).map((a) => a.block_id));
  for (const b of st.blocks) {
    const li = document.createElement("li");
    li.className = "outline-item" + (b.kind === "paragraph" ? " para" : "");
    li.style.paddingLeft = b.depth * 12 + "px";
    const num = b.number ? b.number + " " : "";
    const text = b.text.length > 40 ? b.text.slice(0, 40) + "…" : b.text;
    const mark = annBlocks.has(b.id) ? '<span class="outline-ann" title="有批注">●</span>' : "";
    li.innerHTML = `${mark}<span class="outline-num">${esc(num)}</span>${esc(text)}`;
    li.title = "点击定位到笔记中的位置";
    li.onclick = () => scrollNoteToBlock(b.id);
    ul.appendChild(li);
  }
}

function scrollNoteToBlock(blockId) {
  const doc = noteFrame.contentDocument;
  if (!doc) return;
  const el = doc.getElementById("blk-" + blockId);
  if (el) el.scrollIntoView({ block: "start" });
}


// ===== 文件导入（上传 + 拖拽） =====

/// 上传文件到服务器，再按类型执行 ingest。
/// 用 XHR 上传（fetch 无法获取上传进度），返回解析后的 JSON。
function uploadFile(file, onProgress) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", "/api/upload");
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable && onProgress) onProgress(e.loaded / e.total);
    };
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        try {
          resolve(JSON.parse(xhr.responseText || "{}"));
        } catch (e) {
          reject(new Error("服务器响应无法解析"));
        }
        return;
      }
      let msg = xhr.responseText || ("HTTP " + xhr.status);
      try {
        const o = JSON.parse(xhr.responseText || "{}");
        if (o && o.error) msg = o.error;
      } catch (e) { /* 非 JSON，保留原文 */ }
      if (xhr.status === 413 && !msg) msg = "文件超过服务器大小上限";
      reject(new Error(msg));
    };
    xhr.onerror = () => reject(new Error("上传中断（网络问题，或文件超过服务器上限）"));
    const fd = new FormData();
    fd.append("file", file);
    xhr.send(fd);
  });
}

/// 弹出导入弹窗，返回 {name, style} 或 null（取消）。
function promptImport(file, defaultName) {
  return new Promise((resolve) => {
    importResolve = resolve;
    $("import-file").textContent = "文件：" + file.name;
    $("import-name").value = defaultName;
    $("import-style").value = "four";
    $("import-modal").classList.remove("hidden");
    $("import-name").focus();
    $("import-name").select();
  });
}

function closeImportModal(result) {
  $("import-modal").classList.add("hidden");
  if (importResolve) {
    const r = importResolve;
    importResolve = null;
    r(result);
  }
}

async function importFile(file) {
  if (!file) return;
  if (running) { alert("有任务正在运行，请稍后再导入。"); return; }
  const stem = file.name.replace(/\.[^.]+$/, "").slice(0, 20);
  const opts = await promptImport(file, "笔记_" + stem + ".md");
  if (!opts) return; // 取消
  const exportName = opts.name.trim() || ("笔记_" + stem + ".md");
  setProgress("上传中…");
  switchTab("console");
  appendConsole("> 导入文件: " + file.name + "（风格：" + opts.style + "）");
  try {
    const j = await uploadFile(file, (p) => setUploadPct(p));
    setProgress("");
    // .txt/.md 走 --text（跳过 PDF 解析），其余按 PDF 处理
    const isText = /\.(txt|md|markdown)$/i.test(file.name);
    const cmd = `ingest ${isText ? "--text " : ""}--style ${opts.style} ` + shellQuote(j.path);
    await runCommand(cmd, { export: exportName });
  } catch (e) {
    appendConsole("❌ 上传失败: " + e.message, "err");
    setProgress("");
    switchTab("console");
  }
}

/// 把路径包成双引号（内部反斜杠/引号转义），与后端 normalize_path_arg 对应。
function shellQuote(p) {
  return '"' + String(p).replace(/\\/g, "\\\\").replace(/"/g, '\\"') + '"';
}

// ===== 配置弹窗 =====

async function openConfig() {
  try {
    const c = await (await fetch("/api/config")).json();
    $("cfg-endpoint").value = c.llm.api_endpoint || "";
    $("cfg-key").value = "";
    $("cfg-key").placeholder = "留空则不修改（当前：" + (c.llm.api_key_masked || "未设置") + "）";
    $("cfg-model").value = c.llm.model || "";
    $("cfg-context").value = c.llm.context_length || 0;
    $("cfg-thinking").checked = !!c.llm.thinking_mode;
    $("cfg-inprice").value = c.pricing.input_price_per_1m;
    $("cfg-outprice").value = c.pricing.output_price_per_1m;
    $("cfg-budget").value = c.budget.token_budget;
    $("cfg-status").textContent = "";
    $("cfg-status").className = "status";
    $("cfg-test-result").classList.add("hidden");
    showBar("cfg-test-bar", false);
    $("btn-config-test-stop").classList.add("hidden");
    $("btn-config-test").disabled = false;
    $("config-modal").classList.remove("hidden");
  } catch (e) {
    alert("读取配置失败: " + e);
  }
}

async function setConfig(key, value) {
  const res = await fetch("/api/config", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ key, value }),
  });
  if (!res.ok) throw new Error(`${key}: ${await res.text()}`);
}

async function saveConfig() {
  const status = $("cfg-status");
  status.textContent = "保存中…";
  status.className = "status";
  try {
    await setConfig("llm.api_endpoint", $("cfg-endpoint").value.trim());
    await setConfig("llm.model", $("cfg-model").value.trim());
    await setConfig("llm.context_length", String(parseInt($("cfg-context").value, 10) || 0));
    await setConfig("llm.thinking_mode", $("cfg-thinking").checked ? "true" : "false");
    await setConfig("pricing.input_price_per_1m", String(parseFloat($("cfg-inprice").value) || 0));
    await setConfig("pricing.output_price_per_1m", String(parseFloat($("cfg-outprice").value) || 0));
    await setConfig("budget.token_budget", String(parseInt($("cfg-budget").value, 10) || 0));
    const key = $("cfg-key").value.trim();
    if (key) await setConfig("llm.api_key", key);
    status.textContent = "✓ 已保存";
    status.className = "status ok";
    await refreshState();
    setTimeout(() => $("config-modal").classList.add("hidden"), 600);
  } catch (e) {
    status.textContent = "❌ " + e.message;
    status.className = "status err";
  }
}

let cfgTestAbort = null;

/// 测试当前表单里的 LLM 配置（保存前也可测），展示耗时/状态/原始响应；可中途停止。
async function testConfig() {
  const result = $("cfg-test-result");
  result.classList.remove("hidden");
  result.className = "test-result";
  result.textContent = "测试中…";
  showBar("cfg-test-bar", true);
  $("btn-config-test").disabled = true;
  $("btn-config-test-stop").classList.remove("hidden");
  const ctrl = new AbortController();
  cfgTestAbort = ctrl;
  try {
    const res = await fetch("/api/config/test", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        endpoint: $("cfg-endpoint").value.trim(),
        model: $("cfg-model").value.trim(),
        api_key: $("cfg-key").value.trim(),
      }),
      signal: ctrl.signal,
    });
    const r = await res.json();
    result.innerHTML = "";
    if (r.ok) {
      result.className = "test-result ok";
      result.textContent =
        `✓ 连接成功 · HTTP ${r.status} · ${r.latency_ms}ms · 模型 ${r.model}` +
        ` · 回复「${r.reply}」· tokens ${r.input_tokens}/${r.output_tokens}`;
    } else if (r.raw === "已中止") {
      result.className = "test-result";
      result.textContent = "⏹ 已中止";
    } else {
      result.className = "test-result err";
      const head = document.createElement("div");
      head.textContent =
        `✗ 连接失败 · ${r.status ? "HTTP " + r.status : "网络错误"} · ${r.latency_ms}ms`;
      result.appendChild(head);
      if (r.raw) {
        const det = document.createElement("details");
        const sum = document.createElement("summary");
        sum.textContent = "查看原始响应";
        const pre = document.createElement("pre");
        pre.textContent = r.raw;
        det.appendChild(sum);
        det.appendChild(pre);
        result.appendChild(det);
      }
    }
  } catch (e) {
    if (e && e.name === "AbortError") {
      result.className = "test-result";
      result.textContent = "⏹ 已中止";
    } else {
      result.className = "test-result err";
      result.textContent = "❌ 测试请求失败: " + e.message;
    }
  }
  cfgTestAbort = null;
  showBar("cfg-test-bar", false);
  $("btn-config-test").disabled = false;
  $("btn-config-test-stop").classList.add("hidden");
}

function stopConfigTest() {
  fetch("/api/interrupt", { method: "POST" }).catch(() => {});
  if (cfgTestAbort) {
    try { cfgTestAbort.abort(); } catch (e) { /* 忽略 */ }
  }
}

// ===== 事件绑定 =====

// ===== 侧栏：活动栏切换 + 宽度拖拽 =====
const PANEL_KEY = "ph.panel";
const WIDTH_KEY = "ph.sidebarWidth";

function setupSidebar() {
  const acts = document.querySelectorAll("#activitybar .act");
  const sidebar = $("sidebar");
  const panels = document.querySelectorAll("#sidebar-panels section[data-panel]");
  if (!acts.length || !sidebar) return;

  const apply = (name) => {
    acts.forEach((b) => b.classList.toggle("active", b.dataset.panel === name));
    panels.forEach((p) => p.classList.toggle("active", p.dataset.panel === name));
  };

  let current = localStorage.getItem(PANEL_KEY) || "outline";
  if (![...panels].some((p) => p.dataset.panel === current)) current = "outline";
  apply(current);

  acts.forEach((b) => {
    b.onclick = () => {
      // 再点当前面板 = 收起/展开侧栏（VS Code 行为）
      if (b.classList.contains("active") && !sidebar.classList.contains("collapsed")) {
        sidebar.classList.add("collapsed");
        return;
      }
      sidebar.classList.remove("collapsed");
      current = b.dataset.panel;
      localStorage.setItem(PANEL_KEY, current);
      apply(current);
    };
  });

  // 宽度拖拽
  const saved = parseInt(localStorage.getItem(WIDTH_KEY) || "", 10);
  if (saved >= 180 && saved <= 640) sidebar.style.setProperty("--sidebar-width", saved + "px");
  const resizer = $("sidebar-resizer");
  if (!resizer) return;
  let dragging = false;
  resizer.addEventListener("pointerdown", (e) => {
    dragging = true;
    resizer.classList.add("dragging");
    resizer.setPointerCapture(e.pointerId);
    e.preventDefault();
  });
  resizer.addEventListener("pointermove", (e) => {
    if (!dragging) return;
    const left = sidebar.getBoundingClientRect().left;
    const w = Math.min(640, Math.max(180, e.clientX - left));
    sidebar.style.setProperty("--sidebar-width", w + "px");
  });
  const stopDrag = () => {
    if (!dragging) return;
    dragging = false;
    resizer.classList.remove("dragging");
    localStorage.setItem(WIDTH_KEY, String(Math.round(sidebar.getBoundingClientRect().width)));
  };
  resizer.addEventListener("pointerup", stopDrag);
  resizer.addEventListener("pointercancel", stopDrag);
}

document.addEventListener("DOMContentLoaded", () => {
  setupSidebar();

  // 中止：进度条旁的停止按钮 / 批注弹窗停止按钮（等同 Ctrl-C）
  $("btn-stop").onclick = stopCurrent;
  $("ann-stop").onclick = stopCurrent;

  document.querySelectorAll(".tab[data-key]").forEach((t) => (t.onclick = () => switchTab(t.dataset.key)));

  // 顶栏：导出 / 撤销 / 帮助
  $("btn-export").onclick = (e) => {
    e.stopPropagation();
    $("export-menu").classList.toggle("hidden");
  };
  $("export-menu").querySelectorAll("[data-fmt]").forEach((el) => {
    el.onclick = () => {
      $("export-menu").classList.add("hidden");
      downloadExport(el.dataset.fmt);
    };
  });
  $("btn-undo").onclick = () => runCommand("undo");
  $("btn-help").onclick = () => $("help-modal").classList.remove("hidden");
  $("btn-help-close").onclick = () => $("help-modal").classList.add("hidden");
  document.addEventListener("click", (e) => {
    if (!e.target.closest(".dropdown")) $("export-menu").classList.add("hidden");
  });

  // 文件导入：按钮 + 拖拽
  $("btn-import").onclick = () => $("file-input").click();
  $("file-input").onchange = (e) => {
    const f = e.target.files && e.target.files[0];
    e.target.value = "";
    if (f) importFile(f);
  };
  let dragDepth = 0;
  const hasFiles = (e) => e.dataTransfer && Array.from(e.dataTransfer.types || []).includes("Files");
  document.addEventListener("dragenter", (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    dragDepth++;
    $("drop-overlay").classList.remove("hidden");
  });
  document.addEventListener("dragover", (e) => {
    if (hasFiles(e)) e.preventDefault();
  });
  document.addEventListener("dragleave", (e) => {
    if (!hasFiles(e)) return;
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) $("drop-overlay").classList.add("hidden");
  });
  document.addEventListener("drop", (e) => {
    e.preventDefault();
    dragDepth = 0;
    $("drop-overlay").classList.add("hidden");
    const f = e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files[0];
    if (f) importFile(f);
  });

  $("btn-refresh").onclick = () => { refreshState(); reloadNote(); };

  // 批注弹窗
  $("ann-close").onclick = closeAnnPopup;
  $("ann-send").onclick = sendAnnotation;
  $("ann-mode").onclick = () => {
    annMode = annMode === "ask" ? "check" : "ask";
    $("ann-mode").textContent = annMode;
    $("ann-mode").classList.toggle("check", annMode === "check");
  };
  $("ann-q").addEventListener("keydown", (e) => {
    if (e.key === "Enter") { e.preventDefault(); sendAnnotation(); }
  });
  $("btn-config").onclick = openConfig;
  $("btn-config-cancel").onclick = () => $("config-modal").classList.add("hidden");
  $("btn-config-test").onclick = testConfig;
  $("btn-config-test-stop").onclick = stopConfigTest;
  $("btn-config-save").onclick = saveConfig;

  // 导入弹窗：文件名 + 笔记风格
  $("btn-import-cancel").onclick = () => closeImportModal(null);
  $("btn-import-ok").onclick = () =>
    closeImportModal({ name: $("import-name").value.trim(), style: $("import-style").value });
  $("import-name").addEventListener("keydown", (e) => {
    if (e.key === "Enter") { e.preventDefault(); $("btn-import-ok").click(); }
  });

  // 笔记编辑弹窗：手动 / AI 重写 / AI 补充
  document.querySelectorAll(".edit-tab").forEach((t) => (t.onclick = () => setEditTab(t.dataset.mode)));
  $("btn-edit-cancel").onclick = closeEditModal;
  $("btn-edit-gen").onclick = generateEdit;
  $("btn-edit-stop").onclick = stopEditGen;
  $("btn-edit-apply").onclick = () => applyEdit(false);
  $("btn-edit-insert").onclick = () => applyEdit(true);
  $("btn-edit-delete").onclick = deleteEditBlock;

  document.addEventListener("click", (e) => { if (!e.target.closest("#ctx-menu")) hideCtxMenu(); });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") { hideCtxMenu(); clearAllSelections(); }
  });

  refreshState();
  setInterval(() => { if (!running) refreshState(); }, 15000);
});
