// PaperHelper Web 前端：原生 JS，无构建。
// 后端约定：POST /api/run 返回 SSE（event: stdout/stderr/token/progress/progress_done/done/error，
// data 为 JSON 字符串）。其余接口为普通 JSON。

const $ = (id) => document.getElementById(id);
const consoleEl = $("console");
const noteFrame = $("note-frame");

let running = false;
let streamSpan = null;      // 当前流式 token 的容器
let lastState = null;       // 最近一次 /api/state 快照
const dynamicTabs = new Map(); // key -> { btn, pane }
// 批注（选中文字提问）
let annotationsCache = [];
let currentAnnotation = null; // { id, block_id, quote }
let annSelectedNode = null;   // 弹窗内当前选中的节点（追问挂到它下面）
let annMode = "ask";          // ask | check

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
let progressTimer = null;

function updateProgressText() {
  const el = $("progress");
  if (!el || !progressLabel) return;
  const secs = Math.round((Date.now() - progressStart) / 1000);
  let meta = secs + "s";
  if (progressChars > 0) meta = progressChars.toLocaleString() + " 字 · " + meta;
  el.innerHTML = `<span class="spin"></span><span>${esc(progressLabel)}</span><b class="meta">${meta}</b>`;
}

/// 开始/更新一个进度提示（空串=结束）。
function setProgress(msg) {
  if (!msg) { stopProgress(); return; }
  if (msg !== progressLabel) {
    progressLabel = msg;
    progressStart = Date.now();
    progressChars = 0;
  }
  $("progress").classList.remove("hidden");
  $("progress-bar").classList.remove("hidden");
  updateProgressText();
  if (!progressTimer) progressTimer = setInterval(updateProgressText, 400);
}

function stopProgress() {
  progressLabel = "";
  progressChars = 0;
  $("progress").classList.add("hidden");
  $("progress-bar").classList.add("hidden");
  if (progressTimer) { clearInterval(progressTimer); progressTimer = null; }
}

function setRunning(v) {
  running = v;
  if (!v) setProgress("");
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
    case "error": appendConsole("❌ " + text, "err"); switchTab("console"); break;
    case "done": appendConsole("✓ 完成", "ok"); break;
    default: if (text) appendConsole(text);
  }
}

async function runCommand(command, opts = {}) {
  if (running) return;
  if (!command || !command.trim()) return;
  setRunning(true);
  appendConsole("> " + command, "ok");
  // 不自动跳控制台；仅出错时（handleFrame 的 error）切过去
  try {
    const res = await fetch("/api/run", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ command, export: opts.export || null }),
    });
    if (!res.ok || !res.body) {
      appendConsole("❌ 请求失败: HTTP " + res.status, "err");
      switchTab("console");
      setRunning(false);
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
    appendConsole("❌ 连接中断: " + e, "err");
    switchTab("console");
  } finally {
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
    renderModel(st);
    renderUsage(st);
    renderPapers(st);
    renderConcepts(st);
  } catch (e) {
    console.error(e);
  }
  await refreshSessions();
  await refreshAnnotations();
  renderOutline(lastState);
  applyHighlights();
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
  if (!st.papers || st.papers.length === 0) {
    ul.innerHTML = '<li class="muted">（无）</li>';
    return;
  }
  ul.innerHTML = "";
  for (const p of st.papers) {
    const li = document.createElement("li");
    li.className = "clickable";
    const pin = p.pinned ? '<span class="pin" title="已置顶">★</span>' : "";
    li.innerHTML = `${pin}《${esc(p.title)}》`;
    li.title = "点击查看笔记与对应会话 · 右键更多";
    li.onclick = () => openPaperTab(p);
    li.oncontextmenu = (e) => {
      e.preventDefault();
      showPaperMenu(e.clientX, e.clientY, p);
    };
    ul.appendChild(li);
  }
}

function renderConcepts(st) {
  const ul = $("concepts");
  if (!st.concepts || st.concepts.length === 0) {
    ul.innerHTML = '<li class="muted">（无）</li>';
    return;
  }
  ul.innerHTML = "";
  for (const c of st.concepts) {
    const li = document.createElement("li");
    li.className = "clickable";
    const pin = c.pinned ? '<span class="pin" title="已置顶">★</span>' : "";
    li.innerHTML = `${pin}${esc(c.name)}`;
    li.title = "点击查看概念详情 · 右键更多";
    li.onclick = () => openConceptTab(c.name);
    li.oncontextmenu = (e) => {
      e.preventDefault();
      showConceptMenu(e.clientX, e.clientY, c);
    };
    ul.appendChild(li);
  }
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
    doc.addEventListener("scroll", hideSelButton, true);
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
  try {
    await postSse(url, body, ({ name, text }) => {
      if (name === "error") {
        appendConsole("❌ " + text, "err");
        ansEl.textContent = "（出错：" + text + "）";
      } else if (name === "token") {
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
    });
  } catch (e) {
    appendConsole("❌ " + e, "err");
    ansEl.textContent = "（出错：" + e + "）";
  }
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
async function postSse(url, body, onEvent) {
  const res = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
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
    return;
  }
  const activeId = lastState && lastState.session_id ? lastState.session_id : null;
  for (const s of list) {
    const li = document.createElement("li");
    li.dataset.id = s.id;
    if (s.id === activeId) li.classList.add("active");
    const pin = s.pinned ? '<span class="pin" title="已置顶">★</span>' : "";
    li.innerHTML = `${pin}<span class="s-name">${esc(s.name)}</span><span class="s-time">${esc(fmtTime(s.updated_at))}</span>`;
    li.title = "点击加载 · 右键更多";
    li.onclick = () => loadSession(s.id);
    li.oncontextmenu = (e) => {
      e.preventDefault();
      showSessionMenu(e.clientX, e.clientY, s);
    };
    ul.appendChild(li);
  }
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
  showMenu(x, y, [
    { label: s.pinned ? "取消置顶" : "置顶会话", fn: () => pinSession(s.id, !s.pinned) },
    { label: "重命名", fn: () => renameSession(s) },
    { label: "删除会话", danger: true, fn: () => deleteSession(s) },
  ]);
}

function showPaperMenu(x, y, p) {
  showMenu(x, y, [
    { label: p.pinned ? "取消置顶" : "置顶论文", fn: () => pinPaper(p, !p.pinned) },
    { label: "删除论文", danger: true, fn: () => deletePaper(p) },
  ]);
}

function showConceptMenu(x, y, c) {
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
async function importFile(file) {
  if (!file) return;
  if (running) { alert("有任务正在运行，请稍后再导入。"); return; }
  // 先询问笔记文件名（默认 笔记_<文件stem>.md）
  const stem = file.name.replace(/\.[^.]+$/, "").slice(0, 20);
  const defaultName = "笔记_" + stem + ".md";
  const input = prompt("笔记文件名（可写 .md 或 .html）", defaultName);
  if (input === null) return; // 取消
  const exportName = input.trim() || defaultName;
  setProgress("上传中…");
  switchTab("console");
  appendConsole("> 导入文件: " + file.name);
  try {
    const fd = new FormData();
    fd.append("file", file);
    const res = await fetch("/api/upload", { method: "POST", body: fd });
    if (!res.ok) {
      appendConsole("❌ 上传失败: " + (await res.text()), "err");
      setProgress("");
      return;
    }
    const j = await res.json();
    setProgress("");
    // .txt/.md 走 --text（跳过 PDF 解析），其余按 PDF 处理
    const isText = /\.(txt|md|markdown)$/i.test(file.name);
    const cmd = (isText ? "ingest --text " : "ingest ") + shellQuote(j.path);
    await runCommand(cmd, { export: exportName });
  } catch (e) {
    appendConsole("❌ 导入失败: " + e, "err");
    setProgress("");
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

// ===== 事件绑定 =====

document.addEventListener("DOMContentLoaded", () => {
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
  $("btn-upload").onclick = () => $("file-input").click();
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
  $("btn-config-save").onclick = saveConfig;

  document.addEventListener("click", (e) => { if (!e.target.closest("#ctx-menu")) hideCtxMenu(); });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") hideCtxMenu(); });

  refreshState();
  setInterval(() => { if (!running) refreshState(); }, 15000);
});
