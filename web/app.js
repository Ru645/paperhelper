// PaperHelper Web 前端：原生 JS，无构建。
// 后端约定：POST /api/run 返回 SSE（event: stdout/stderr/token/progress/progress_done/done/error，
// data 为 JSON 字符串）。其余接口为普通 JSON。

const $ = (id) => document.getElementById(id);
const consoleEl = $("console");
const noteFrame = $("note-frame");

let running = false;
// 正在执行的 LLM 任务名（导入/提问/AI 生成…）：LLM 任务互斥，非 LLM 操作可并行
let llmBusyTask = "";
const LLM_CMD_RE = /^(ingest|ask|q|check|sum)\b/;
let streamSpan = null;      // 当前流式 token 的容器
let lastState = null;       // 最近一次 /api/state 快照
let currentAbort = null;    // 当前 /api/run 的 AbortController
let annAbort = null;        // 当前批注请求的 AbortController
let importResolve = null;   // 导入弹窗的 Promise resolve
let editBlockId = null;     // 正在编辑的块 id
let editBlockKind = "paragraph"; // 正在编辑的块类型：paragraph | section | title
let editMode = "manual";    // manual | rewrite | append
// AI 生成结果按块存草稿：block_id -> { text, status, cls }（关弹窗/切会话都不丢）
const editDrafts = new Map();
// 正在后台生成的 AI 编辑任务：{ blockId, ctrl }（与顶部进度/停止按钮联动）
let editRun = null;
let importRun = null;       // 后台导入任务：{ sessionId }，用于切回发起会话时恢复覆盖层
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

// ===== 思考过程（reasoning 流）与「等待响应」计时 =====
const reasoningBuf = { ann: "", edit: "" };

/// 追加一段思考流（截断保留末尾，避免无限增长），并展开折叠区。
function appendReasoning(which, text) {
  const box = $(which + "-reasoning");
  const el = $(which + "-reasoning-text");
  if (!box || !el) return;
  reasoningBuf[which] = (reasoningBuf[which] + text).slice(-4000);
  el.textContent = reasoningBuf[which];
  box.classList.remove("hidden");
  box.open = true;
  el.scrollTop = el.scrollHeight;
}

// ===== 笔记区「生成中」覆盖层（导入/生成笔记的实时反馈） =====
let noteGenTimer = null, noteGenStart = 0, noteGenChars = 0;

function noteGenTick() {
  const secs = Math.round((Date.now() - noteGenStart) / 1000);
  const parts = [secs + "s"];
  if (noteGenChars > 0) parts.unshift(noteGenChars.toLocaleString() + " 字");
  $("note-gen-meta").textContent = parts.join(" · ");
}

/// 显示覆盖层（内容保留）：切回发起会话时可恢复实时反馈。
function noteGenShow() {
  $("note-gen").classList.remove("hidden");
  if (!noteGenTimer) noteGenTimer = setInterval(noteGenTick, 500);
}

function noteGenReset() {
  $("note-gen-stream").textContent = "";
  $("note-gen-log").innerHTML = "";
  $("note-gen-title").textContent = "正在生成笔记…";
  $("note-gen-meta").textContent = "";
  resetReasoning("note-gen");
  if (noteGenTimer) { clearInterval(noteGenTimer); noteGenTimer = null; }
  noteGenStart = Date.now();
  noteGenChars = 0;
  noteGenShow();
}

function noteGenHide() {
  $("note-gen").classList.add("hidden");
  if (noteGenTimer) { clearInterval(noteGenTimer); noteGenTimer = null; }
}

function noteGenPhase(text) {
  if (text) $("note-gen-title").textContent = text;
}

function noteGenLog(text, cls) {
  if (text === "" || text === undefined) return;
  const d = document.createElement("div");
  if (cls) d.className = cls;
  d.textContent = text;
  $("note-gen-log").appendChild(d);
}

function noteGenToken(text) {
  const pre = $("note-gen-stream");
  pre.textContent += text;
  pre.scrollTop = pre.scrollHeight;
  noteGenChars += text.length;
}

function resetReasoning(which) {
  const box = $(which + "-reasoning");
  const el = $(which + "-reasoning-text");
  reasoningBuf[which] = "";
  if (el) el.textContent = "";
  if (box) { box.classList.add("hidden"); box.open = true; }
}

/// 弹窗右上角的进度文案 + 计时（后端 progress 事件会更新 base，计时器每秒刷新）。
let annProgBase = "", annProgStart = 0, annProgTimer = null;
function setAnnProgress(msg) {
  if (!msg) {
    if (annProgTimer) { clearInterval(annProgTimer); annProgTimer = null; }
    annProgBase = "";
    $("ann-progress").textContent = "";
    return;
  }
  annProgBase = msg;
  if (!annProgTimer) annProgStart = Date.now();
  $("ann-progress").textContent = msg;
  if (!annProgTimer) {
    annProgTimer = setInterval(() => {
      if (!annProgBase) return;
      $("ann-progress").textContent = annProgBase + " · " + Math.round((Date.now() - annProgStart) / 1000) + "s";
    }, 1000);
  }
}

/// AI 重写弹窗的状态行计时（有 token / 结束时停止）。
let editProgBase = "", editProgStart = 0, editProgTimer = null;
function setEditProgress(msg) {
  const st = $("edit-status");
  if (!msg) {
    if (editProgTimer) { clearInterval(editProgTimer); editProgTimer = null; }
    editProgBase = "";
    return;
  }
  editProgBase = msg;
  if (!editProgTimer) editProgStart = Date.now();
  st.className = "status";
  st.textContent = msg;
  if (!editProgTimer) {
    editProgTimer = setInterval(() => {
      if (!editProgBase) return;
      if (st.classList.contains("err") || st.classList.contains("ok")) return;
      st.textContent = editProgBase + " · " + Math.round((Date.now() - editProgStart) / 1000) + "s";
    }, 1000);
  }
}

function setRunning(v) {
  running = v;
  if (!v) setProgress("");
  $("btn-stop").classList.toggle("hidden", !v);
}

/// 当前正在进行的任务名（顶部进度优先，其次 LLM 任务名）。
function busyLabel() {
  return progressLabel || llmBusyTask || "任务";
}

/// 轻提示：正在忙什么、请稍候（2.6s 自动消失）。
function showBusyHint(text) {
  const el = $("busy-hint");
  el.textContent = text || ("正在" + busyLabel() + "，请稍候…（可点顶部「停止」）");
  el.classList.remove("hidden");
  clearTimeout(showBusyHint._timer);
  showBusyHint._timer = setTimeout(() => el.classList.add("hidden"), 2600);
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

function handleFrame(frame, opts = {}) {
  const { name, text } = parseFrame(frame);
  // 导入/生成笔记：反馈都进「笔记区」覆盖层（控制台只在出错时用）
  if (opts.ui === "import") {
    // 用户切走会话时覆盖层被隐藏：任务继续，完成/错误仍需在控制台可见
    const overlayHidden = () => $("note-gen").classList.contains("hidden");
    switch (name) {
      case "stdout": if (overlayHidden()) appendConsole(text, "ok"); else noteGenLog(text); return;
      case "stderr": if (overlayHidden()) appendConsole(text, "err"); else noteGenLog(text, "err"); return;
      case "token": if (!overlayHidden()) noteGenToken(text); return;
      case "chars": {
        noteGenChars = Math.max(0, parseInt(text, 10) || 0);
        progressChars = noteGenChars;
        updateProgressText();
        return;
      }
      case "reasoning": if (!overlayHidden()) appendReasoning("note-gen", text); return;
      case "progress": noteGenPhase(text); setProgress(text); return;
      case "progress_done": setProgress(""); return;
      case "aborted":
        if (overlayHidden()) appendConsole("⏹ 已中止", "warn"); else noteGenLog("⏹ 已中止", "err");
        setProgress("");
        return;
      case "error": renderError(text); return;
      case "done":
        if (overlayHidden()) {
          appendConsole("✓ 生成完成（结果已写入发起会话，见左侧会话列表）", "ok");
          showBusyHint("✓ 生成完成，结果已写入发起会话");
        } else {
          noteGenLog("✓ 完成");
        }
        setProgress("");
        return;
      default: if (text) noteGenLog(text); return;
    }
  }
  switch (name) {
    case "stdout": appendConsole(text); break;
    case "stderr": appendConsole(text, "err"); break;
    case "token": appendToken(text); break;
    // 已生成字数：只更新顶部进度（导入笔记等场景不再把 token 灌进控制台）
    case "chars": progressChars = Math.max(0, parseInt(text, 10) || 0); updateProgressText(); break;
    case "reasoning": appendConsole(text, "dim"); break;
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
  if (editRun) { try { editRun.ctrl.abort(); } catch (e) { /* 忽略 */ } }
}

async function runCommand(command, opts = {}) {
  if (!command || !command.trim()) return;
  // LLM 任务互斥；非 LLM 命令（撤销/跳转/查看等）在 LLM 任务期间仍可执行
  const isLlm = LLM_CMD_RE.test(command.trim());
  if (isLlm && (running || llmBusyTask)) {
    showBusyHint();
    return;
  }
  if (isLlm) {
    llmBusyTask = command.trim().startsWith("ingest") ? "生成笔记" : "处理当前提问";
    setRunning(true);
  }
  if (!opts.quiet) appendConsole("> " + command, "ok");
  // 不自动跳控制台；仅出错时（handleFrame 的 error）切过去
  const controller = new AbortController();
  if (isLlm) currentAbort = controller;
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
        if (frame.trim()) handleFrame(frame, opts);
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
    if (isLlm && currentAbort === controller) currentAbort = null;
    if (isLlm) {
      setRunning(false);
      llmBusyTask = "";
    }
    await refreshState();
    if (opts.skipReload) {
      // 笔记未变（如 goto）：不重载，直接滚动到目标位置
      if (opts.scrollAnchor) scrollNoteTo(opts.scrollAnchor);
    } else {
      reloadNote(opts.scrollAnchor, opts.keepScroll);
    }
    if (opts.ui === "import") noteGenHide();
  }
}

// ===== 状态刷新 =====

async function refreshState() {
  try {
    const st = await (await fetch("/api/state")).json();
    lastState = st;
    mathMacros = parseMathMacros(st.math_macros || "");
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
    const badge = p.kind === "lecture" ? ' <span class="badge">讲义</span>' : p.kind === "note" ? ' <span class="badge">笔记</span>' : "";
    li.innerHTML = `${pin}《${esc(p.title)}》${badge}`;
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
let pendingAnnOpen = null; // { id, block }：笔记加载完后自动打开某条批注

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

// 主页面按需加载 marked + KaTeX：优先本地内嵌资源（/vendor/*，离线可用），
// 失败再回退 CDN（jsdelivr → npmmirror）。复用与笔记相同的“公式保护”策略，
// 使概念详情里的 $...$ / $$...$$ 能正确渲染。
let mathLibsPromise = null;
function loadMathLibs() {
  if (mathLibsPromise) return mathLibsPromise;
  const SOURCES = ["/vendor", "https://cdn.jsdelivr.net/npm", "https://registry.npmmirror.com"];
  const remotePath = {
    "katex.min.css": "katex@0.16.9/dist/katex.min.css",
    "marked.min.js": "marked@12.0.2/marked.min.js",
    "katex.min.js": "katex@0.16.9/dist/katex.min.js",
  };
  const url = (src, name) => {
    if (src === "/vendor") return `/vendor/${name}`;
    const path = remotePath[name];
    if (src.includes("npmmirror")) {
      const slash = path.indexOf("/");
      const parts = path.slice(0, slash).split("@");
      const rest = path.slice(slash + 1);
      return `https://registry.npmmirror.com/${parts[0]}/${parts[parts.length - 1]}/files/${rest}`;
    }
    return src + "/" + path;
  };
  const load = (tag, name) =>
    new Promise((resolve) => {
      let i = 0;
      const next = () => {
        if (i >= SOURCES.length) return resolve(false);
        const el = document.createElement(tag);
        if (tag === "link") {
          el.rel = "stylesheet";
          el.href = url(SOURCES[i], name);
        } else {
          el.src = url(SOURCES[i], name);
        }
        el.onload = () => resolve(true);
        el.onerror = () => { el.remove(); i++; next(); };
        (tag === "link" ? document.head : document.body).appendChild(el);
      };
      next();
    });
  mathLibsPromise = (async () => {
    await load("link", "katex.min.css");
    await load("script", "marked.min.js");
    await load("script", "katex.min.js");
  })();
  return mathLibsPromise;
}

/// 渲染 Markdown + LaTeX 为 HTML（先抽公式占位符，marked 后再用 KaTeX 回填）。
/// 数学宏定义（来自当前笔记，HTML/讲义导入时收集）→ KaTeX 的 `macros` 选项。
let mathMacros = {};

/// 扫描文本里的宏定义（支持多级花括号嵌套的宏体）：
/// 返回 `{ macros: { name: body }, leftover: 去掉定义后的文本 }`。
function scanMacroDefs(raw) {
  const macros = {};
  const s = String(raw || "");
  const readGroup = (i) => {
    let depth = 0;
    for (let j = i; j < s.length; j++) {
      if (s[j] === "{") depth++;
      else if (s[j] === "}") {
        depth--;
        if (depth === 0) return [s.slice(i + 1, j), j + 1];
      }
    }
    return [null, s.length];
  };
  const readName = (i) => {
    let j = i;
    while (j < s.length && /\s/.test(s[j])) j++;
    if (s[j] === "{") {
      const [inner, end] = readGroup(j);
      const m = inner && inner.match(/^\\([A-Za-z]+)$/);
      return [m ? m[1] : null, end];
    }
    const m = /^\\([A-Za-z]+)/.exec(s.slice(j));
    return m ? [m[1], j + m[0].length] : [null, j];
  };
  let leftover = "";
  let last = 0;
  const re = /\\(?:(?:re)?new|provide)command|\\g?def/g;
  let m;
  while ((m = re.exec(s))) {
    let j = m.index + m[0].length;
    const [name, afterName] = readName(j);
    if (!name) continue;
    j = afterName;
    while (j < s.length && /[#\d\s]/.test(s[j])) j++; // \def 的参数占位 #1#2…
    if (s[j] === "[") {
      const k = s.indexOf("]", j);
      if (k > 0) j = k + 1;
    }
    while (j < s.length && /\s/.test(s[j])) j++;
    if (s[j] !== "{") continue;
    const [body, end] = readGroup(j);
    if (body == null) continue;
    macros[name] = body;
    leftover += s.slice(last, m.index);
    last = end;
    re.lastIndex = end; // 宏体里若再定义宏也能继续扫
  }
  leftover += s.slice(last);
  return { macros, leftover };
}

/// 宏定义文本 → KaTeX 的 `macros` 选项对象。
/// 注意：KaTeX 的 macro 名必须带反斜杠（`"\\abs"`），否则单字母会被当成
/// 普通字符展开，导致 `A` → `\mathcal{A}` 这类无限递归。
function parseMathMacros(raw) {
  const out = {};
  const defs = scanMacroDefs(raw).macros;
  for (const [name, body] of Object.entries(defs)) out["\\" + name] = body;
  return out;
}

/// 把 LaTeX 的 \(…\) / \[…\] 定界符统一成 $ / $$（按 ``` 围栏跳过代码块）。
function convertMathDelims(md) {
  const out = [];
  let inCode = false;
  for (const line of String(md).split("\n")) {
    const t = line.trimStart();
    if (t.startsWith("```")) { inCode = !inCode; out.push(line); continue; }
    if (inCode) { out.push(line); continue; }
    // 注意：replace 的替换串里 `$$` 表示字面 `$`，必须用函数返回 "$$"
    out.push(line
      .replace(/\\\[/g, () => "$$").replace(/\\\]/g, () => "$$")
      .replace(/\\\(/g, "$").replace(/\\\)/g, "$"));
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
      try { return katex.renderToString(tex, { displayMode: display, throwOnError: false, macros: mathMacros }); } catch (e) {}
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
      ? `<button class="mini primary" data-load="${esc(c.session_id)}" data-anchor="${esc(c.explanation_id || "")}" data-ann="${esc(c.annotation_id || "")}" data-block="${esc(c.block_id || "")}">加载该会话</button>`
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
      (b) => (b.onclick = () =>
        loadSession(b.dataset.load, {
          anchor: b.dataset.anchor || null,
          ann: b.dataset.ann || null,
          block: b.dataset.block || null,
        }))
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
    // 注意：鼠标从笔记移到「✎ 编辑」（在父文档里）会触发 iframe 的 mouseleave，
    // 若立即隐藏按钮，指针就又落回笔记内容上 → 按钮反复闪烁、首次点击落空。
    // 因此延迟隐藏，指针进入按钮时取消（见 cancelBlkEditHide）。
    doc.addEventListener("mouseover", (e) => { cancelBlkEditHide(); showBlkEditBtn(e.target); });
    doc.addEventListener("mouseleave", scheduleBlkEditHide);
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
  // 概念页「加载该会话」指向批注时：滚到块并自动打开批注弹窗
  if (pendingAnnOpen) {
    const p = pendingAnnOpen;
    pendingAnnOpen = null;
    if (p.block) scrollNoteToBlock(p.block);
    openAnnotationView(p.id, { scroll: true });
  }
}

/// 右键标题：对该章节（h1=全文）提问 / 打开或删除已有批注。
function showHeadingMenu(doc, heading, x, y) {
  const blockId = blockIdForNode(doc, heading);
  if (!blockId) return;
  const text = (heading.textContent || "").trim();
  const items = [{ label: "对本章节提问", fn: () => openAnnotationCreate({ block_id: blockId, quote: text, context: text }) }];
  const ann = (annotationsCache || []).find((a) => a.block_id === blockId);
  if (ann) {
    items.unshift({ label: "打开批注", fn: () => openAnnotationView(ann.id) });
    items.push({ label: "删除该批注", danger: true, fn: () => deleteAnnotation(ann.id) });
  }
  showMenu(x, y, items);
}

/// 右键高亮文字：打开 / 删除整条批注。
function showAnnMarkMenu(annId, x, y) {
  const ann = (annotationsCache || []).find((a) => a.id === annId);
  const items = [];
  // 回答批注就嵌在当前线程里，无需「打开批注」跳走
  if (ann && !ann.node_id) items.push({ label: "打开批注", fn: () => openAnnotationView(annId) });
  items.push({ label: "删除该批注", danger: true, fn: () => deleteAnnotation(annId) });
  showMenu(x, y, items);
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
/// 清洗引用文本：去掉 KaTeX 隐藏 MathML 的数学斜体（U+1D400–U+1D7FF），
/// 折叠空白并去首尾。用于存储、展示与匹配，保证与「可见文本」一致。
function cleanQuote(s) {
  return String(s == null ? "" : s)
    .replace(/[\u{1D400}-\u{1D7FF}]/gu, "")
    .replace(/\s+/g, " ")
    .trim();
}

/// 计算 range 与 node 内容范围的交集（无交集返回 null）。
function rangeIntersect(doc, range, node) {
  const r = doc.createRange();
  r.selectNodeContents(node);
  if (range.compareBoundaryPoints(Range.END_TO_START, r) > 0 ||
      range.compareBoundaryPoints(Range.START_TO_END, r) < 0) return null;
  const out = doc.createRange();
  if (range.compareBoundaryPoints(Range.START_TO_START, r) <= 0) out.setStart(r.startContainer, r.startOffset);
  else out.setStart(range.startContainer, range.startOffset);
  if (range.compareBoundaryPoints(Range.END_TO_END, r) >= 0) out.setEnd(r.endContainer, r.endOffset);
  else out.setEnd(range.endContainer, range.endOffset);
  return out;
}

/// 某段范围的「可见文本」：去掉 KaTeX 隐藏的 MathML 层（复制后再删，不动原 DOM）。
function rangeVisibleText(r) {
  const frag = r.cloneContents();
  frag.querySelectorAll(".katex-mathml").forEach((m) => m.remove());
  return frag.textContent || "";
}

/// 把选区转成批注的「引用 + LLM 上下文」：
/// - quote：可见文本（与笔记 DOM 一致，用于高亮匹配）
/// - context：公式替换成 KaTeX 的原始 TeX（`$…$` / `$$…$$`），其余照旧；
///   只选中公式一部分时无法还原子表达式 TeX，给整条并在括号里注明选中片段。
function extractQuoteContext(doc, range) {
  const isKatex = (el) => el && el.classList && el.classList.contains("katex");
  const visibleOf = (el) => {
    const c = el.cloneNode(true);
    c.querySelectorAll(".katex-mathml").forEach((m) => m.remove());
    return c.textContent || "";
  };
  const texOf = (el) => {
    const ann = el.querySelector('annotation[encoding="application/x-tex"]');
    return ann ? ann.textContent.trim() : "";
  };
  let quote = "", context = "";
  const partials = [];
  const pushFormula = (el) => {
    const r = rangeIntersect(doc, range, el);
    if (!r || r.collapsed) return;
    const vis = cleanQuote(rangeVisibleText(r));
    if (!vis) return;
    const full = cleanQuote(visibleOf(el));
    quote += vis;
    const tex = texOf(el) || vis;
    const wrapped = el.closest && el.closest(".katex-display") ? `$$${tex}$$` : `$${tex}$`;
    if (full && vis !== full) {
      partials.push(tex);
      context += `${wrapped}（用户只选中了其中「${vis}」）`;
    } else {
      context += wrapped;
    }
  };
  const root = range.commonAncestorContainer;
  const rootEl = root.nodeType === Node.ELEMENT_NODE ? root : root.parentElement;
  const rootKatex = rootEl && (isKatex(rootEl) ? rootEl : rootEl.closest && rootEl.closest(".katex"));
  if (rootKatex && range.intersectsNode(rootKatex)) {
    // 选区整个落在某个公式内（含只选了公式里的一小段）
    pushFormula(rootKatex);
  } else if (root.nodeType === Node.TEXT_NODE) {
    quote += range.toString();
    context += range.toString();
  } else {
    const walker = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT | NodeFilter.SHOW_ELEMENT, {
      acceptNode: (n) => {
        if (n.nodeType === Node.ELEMENT_NODE) {
          if (isKatex(n)) return range.intersectsNode(n) ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT;
          return NodeFilter.FILTER_SKIP; // 普通容器：进入子节点
        }
        if (n.parentElement && n.parentElement.closest && n.parentElement.closest(".katex")) {
          return NodeFilter.FILTER_REJECT; // 公式内部由 .katex 整体处理
        }
        return range.intersectsNode(n) ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT;
      },
    });
    let n;
    while ((n = walker.nextNode())) {
      if (n.nodeType === Node.ELEMENT_NODE) { pushFormula(n); continue; }
      const r = rangeIntersect(doc, range, n);
      if (r) { quote += r.toString(); context += r.toString(); }
    }
  }
  return {
    quote: cleanQuote(quote),
    context: context.replace(/[ \t]+/g, " ").trim(),
    partials,
  };
}

/// 在块范围内高亮 quote：**逐文本节点**包裹 <mark>，只切分文本节点、
/// 不移动/切分任何元素，因此不会破坏 KaTeX 的 span 结构。
/// 匹配时跳过隐藏的 `.katex-mathml`，并把空白视为 `\s*`，
/// 以兼容换行与 KaTeX 两套渲染层（旧批注的 quote 里带 `\n`/数学斜体也能命中）。
function wrapQuote(doc, range, quote, annId) {
  const q = cleanQuote(quote);
  if (!q) return;
  const walker = doc.createTreeWalker(range.commonAncestorContainer, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => {
      if (!range.intersectsNode(n)) return NodeFilter.FILTER_REJECT;
      const el = n.parentElement;
      if (el && el.closest && el.closest(".katex-mathml")) return NodeFilter.FILTER_REJECT;
      return NodeFilter.FILTER_ACCEPT;
    },
  });
  const items = [];
  while (walker.nextNode()) {
    const node = walker.currentNode;
    items.push({ node, len: node.nodeValue.length });
  }
  if (!items.length) return;
  const full = items.map((it) => it.node.nodeValue).join("");
  // 宽松匹配：空白 -> \s*，其余字符正则转义
  const pattern = q
    .split(" ")
    .map((part) => part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))
    .join("\\s*");
  if (!pattern) return;
  const m = new RegExp(pattern).exec(full);
  if (!m) return;
  const start = m.index;
  const end = m.index + m[0].length;
  let acc = 0;
  for (const { node, len } of items) {
    const a = Math.max(start, acc);
    const b = Math.min(end, acc + len);
    if (a < b) {
      let n = node;
      const s = a - acc;
      const e = b - acc;
      if (s > 0) n = n.splitText(s);
      if (e - s < n.nodeValue.length) n.splitText(e - s);
      const mark = doc.createElement("mark");
      mark.className = "ann-mark";
      mark.dataset.annId = annId;
      n.parentNode.insertBefore(mark, n);
      mark.appendChild(n);
    }
    acc += len;
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
  const range = sel.getRangeAt(0);
  const blockId = blockIdForNode(doc, range.startContainer);
  if (!blockId) return hideSelButton();
  const info = extractQuoteContext(doc, range);
  if (!info.quote) return hideSelButton();
  const rect = range.getBoundingClientRect();
  const fr = noteFrame.getBoundingClientRect();
  const btn = $("sel-btn");
  btn.classList.remove("hidden");
  btn.style.left = Math.min(window.innerWidth - 60, Math.max(8, fr.left + rect.left)) + "px";
  btn.style.top = Math.min(window.innerHeight - 40, Math.max(8, fr.top + rect.bottom + 6)) + "px";
  btn.onmousedown = (e) => e.preventDefault();
  btn.onclick = () => {
    hideSelButton();
    openAnnotationCreate({ block_id: blockId, quote: info.quote, context: info.context });
  };
}

// ---- 批注弹窗内：选中回答文字 → 追问（回答批注） ----

let annSelRange = null;

/// 在批注弹窗的回答里选中文字后，浮出「提问」按钮（与笔记选区互不影响）。
function showAnnSelButton() {
  const btn = $("ann-sel-btn");
  const pop = $("ann-popup");
  if (!pop || pop.classList.contains("hidden")) return btn.classList.add("hidden");
  const sel = document.getSelection();
  if (!sel || sel.isCollapsed || !sel.rangeCount) return btn.classList.add("hidden");
  const range = sel.getRangeAt(0);
  const startEl = range.startContainer.nodeType === Node.TEXT_NODE
    ? range.startContainer.parentElement
    : range.startContainer;
  const answer = startEl && startEl.closest ? startEl.closest(".ann-a") : null;
  const nodeEl = answer && answer.closest(".ann-node[data-node-id]");
  if (!answer || !nodeEl || !pop.contains(answer)) return btn.classList.add("hidden");
  annSelRange = range.cloneRange();
  const rect = range.getBoundingClientRect();
  btn.classList.remove("hidden");
  btn.style.left = Math.min(window.innerWidth - 70, Math.max(8, rect.left)) + "px";
  btn.style.top = Math.min(window.innerHeight - 40, Math.max(8, rect.bottom + 6)) + "px";
  btn.onmousedown = (e) => e.preventDefault();
  btn.onclick = () => {
    btn.classList.add("hidden");
    const r = annSelRange;
    if (!r) return;
    const info = extractQuoteContext(document, r);
    if (!info.quote) return;
    // 不清除选区：让用户仍能看到选中的位置；引文另有附件条展示（见 openAnnotationCreate）
    openAnnotationCreate({
      node_id: nodeEl.dataset.nodeId,
      quote: info.quote,
      context: info.context,
      rect: r.getBoundingClientRect(),
    });
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

/// 延迟隐藏「✎ 编辑」：指针从笔记移到按钮上时 iframe 会 mouseleave，
/// 但按钮本身在父文档，需要给指针进入按钮留出时间，否则按钮闪烁、首次点击落空。
let blkEditHideTimer = null;
function cancelBlkEditHide() {
  if (blkEditHideTimer) { clearTimeout(blkEditHideTimer); blkEditHideTimer = null; }
}
function scheduleBlkEditHide() {
  cancelBlkEditHide();
  blkEditHideTimer = setTimeout(() => {
    blkEditHideTimer = null;
    $("blk-edit-btn").classList.add("hidden");
  }, 150);
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
  btn.onmouseenter = cancelBlkEditHide;
  btn.onmouseleave = scheduleBlkEditHide;
  btn.onclick = () => {
    cancelBlkEditHide();
    btn.classList.add("hidden");
    openEditModal(id);
  };
}

function setEditTab(mode) {
  editMode = mode;
  document.querySelectorAll(".edit-tab").forEach((t) =>
    t.classList.toggle("active", t.dataset.mode === mode)
  );
  $("edit-ai-row").classList.toggle("hidden", mode === "manual");
  $("edit-style-row").classList.toggle("hidden", mode !== "restyle");
  $("btn-edit-apply").textContent = mode === "append" ? "插入" : "应用";
}

/// 填充「按风格重写」的风格下拉（来自风格注册表）。
function populateEditStyles() {
  const sel = $("edit-style");
  const prev = sel.value;
  sel.innerHTML = "";
  for (const st of stylesCache) {
    const o = document.createElement("option");
    o.value = st.id;
    o.textContent = st.label + (st.builtin ? "" : "（自定义）");
    o.title = st.desc || "";
    sel.appendChild(o);
  }
  if ([...sel.options].some((o) => o.value === prev)) sel.value = prev;
  else if ([...sel.options].some((o) => o.value === "four")) sel.value = "four";
}

/// 弹窗是否正打开着某个块（AI 流式回填只在此时更新界面）。
function editModalOpen(bid) {
  return !$("edit-modal").classList.contains("hidden") && editBlockId === bid;
}

/// 按后台生成状态刷新「生成/停止」按钮（生成属于别的块时只禁用生成）。
function reflectEditRun() {
  const mine = !!editRun && editRun.blockId === editBlockId;
  $("btn-edit-gen").disabled = !!editRun;
  $("btn-edit-stop").classList.toggle("hidden", !mine);
  showBar("edit-ai-bar", mine);
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
    // 标题：提供「按风格重写全文」；不支持 AI 补充
    $("edit-tab-restyle").classList.toggle("hidden", !isTitle);
    document.querySelector('.edit-tab[data-mode="append"]').classList.toggle("hidden", isTitle);
    $("edit-instruction").placeholder = isTitle
      ? "额外要求（可选）：如只保留公式与结论 / 更口语化"
      : "告诉 AI 怎么改，如：补充这个方法的直觉解释 / 重写得更有条理";
    if (isTitle) {
      setEditTab("restyle");
      refreshStyles().then(populateEditStyles);
      $("edit-hint").textContent = "按所选笔记风格重写整篇（以原文或当前笔记为素材）；应用后会重建全部结构块，现有批注将失效，可用「撤销」恢复。";
    }
    // 章节：编辑框展示「标题 + 全部子块」；段落/标题：只有自身文字
    $("edit-text").value = editBlockKind === "section" && b.markdown ? b.markdown : (b.text || "");
    // 后台生成的草稿（含生成中途的实时内容）：打开时恢复
    const draft = editDrafts.get(id);
    if (draft && draft.text) $("edit-text").value = draft.text;
    if (draft && draft.status) {
      $("edit-status").textContent = draft.status;
      $("edit-status").className = "status " + (draft.cls || "");
    } else if (editRun && editRun.blockId === id) {
      $("edit-status").textContent = "AI 生成中…";
      $("edit-status").className = "status";
    }
    reflectEditRun();
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
  // AI 生成任务不中断：结果会存为草稿（editDrafts），关弹窗/切会话都不丢；
  // 需要停止请点弹窗内或顶部「停止」。
  setEditProgress("");
  $("edit-modal").classList.add("hidden");
  $("blk-edit-btn").classList.add("hidden");
}

async function generateEdit() {
  if (!editBlockId) return;
  if (running || llmBusyTask) { showBusyHint(); return; }
  const bid = editBlockId;
  const instruction = $("edit-instruction").value.trim();
  const st = $("edit-status");
  const isRestyle = editMode === "restyle";
  if (!instruction && !isRestyle) {
    st.textContent = "请先填写要求，如「补充直觉解释」";
    st.className = "status err";
    return;
  }
  const mode = editMode === "append" ? "append" : "rewrite";
  llmBusyTask = isRestyle ? "按风格重写全文" : "AI 生成";
  setRunning(true); // 顶部进度 + 停止按钮：切走后也能随时回到/中止
  setProgress(isRestyle ? "按风格重写中…" : "AI 生成中…");
  setEditProgress(isRestyle ? "按风格重写中…" : "AI 生成中…");
  resetReasoning("edit");
  const ctrl = new AbortController();
  const run = { blockId: bid, ctrl };
  editRun = run;
  let acc = "";
  const setStatus = (s, cls) => {
    const d = editDrafts.get(bid) || { text: "" };
    d.text = acc;
    d.status = s;
    d.cls = cls || "";
    editDrafts.set(bid, d);
    if (editModalOpen(bid)) {
      $("edit-status").textContent = s;
      $("edit-status").className = "status " + (cls || "");
    }
  };
  setStatus("生成中…");
  if (editModalOpen(bid)) {
    $("edit-text").value = "";
    $("btn-edit-gen").disabled = true;
    $("btn-edit-stop").classList.remove("hidden");
  }
  showBar("edit-ai-bar", true);
  try {
    const url = isRestyle ? "/api/note/restyle" : "/api/note/ai";
    const body = isRestyle
      ? { style: $("edit-style").value, extra: instruction }
      : { block_id: bid, instruction, mode };
    await postSse(url, body, ({ name, text }) => {
      if (name === "token") {
        if (!acc) {
          showBar("edit-ai-bar", false); // 有内容流出即收起进度条与计时
          setEditProgress("");
        }
        acc += text;
        setStatus("生成中…");
        if (editModalOpen(bid)) {
          $("edit-text").value = acc;
          $("edit-text").scrollTop = $("edit-text").scrollHeight;
        }
      } else if (name === "reasoning") {
        if (editModalOpen(bid)) appendReasoning("edit", text);
      } else if (name === "progress") {
        setEditProgress(text);
      } else if (name === "stderr") {
        appendConsole(text, "warn");
      } else if (name === "error") {
        const { summary } = parseError(text);
        setEditProgress("");
        setStatus("❌ " + summary, "err");
        appendConsole("❌ AI 生成失败：" + summary, "err");
      } else if (name === "aborted") {
        setEditProgress("");
        setStatus("⏹ 已中止");
        if (!editModalOpen(bid)) appendConsole("⏹ AI 生成已中止", "warn");
      }
    }, { signal: ctrl.signal });
    const cur = editDrafts.get(bid) || {};
    if (acc && !cur.cls) {
      setEditProgress("");
      setStatus("✓ 已生成（可修改后点" + (mode === "append" ? "「插入」" : "「应用」") + "）", "ok");
      if (!editModalOpen(bid)) {
        appendConsole("✓ AI 生成完成，草稿已保存（打开对应块即可应用）", "ok");
        showBusyHint("✓ 生成完成，草稿已保存");
      }
    }
  } catch (e) {
    if (e && e.name === "AbortError") {
      setStatus("⏹ 已中止");
      if (!editModalOpen(bid)) appendConsole("⏹ AI 生成已中止", "warn");
    } else {
      setStatus("❌ " + e.message, "err");
      appendConsole("❌ AI 生成失败：" + e.message, "err");
    }
  }
  if (editRun === run) editRun = null;
  llmBusyTask = "";
  setRunning(false);
  setEditProgress("");
  showBar("edit-ai-bar", false);
  if (editModalOpen(bid)) {
    $("btn-edit-gen").disabled = false;
    $("btn-edit-stop").classList.add("hidden");
  }
  const rbox = $("edit-reasoning");
  if (rbox && !rbox.classList.contains("hidden")) rbox.open = false; // 思考过程收起但保留
}

function stopEditGen() {
  fetch("/api/interrupt", { method: "POST" }).catch(() => {});
  if (editRun) {
    try { editRun.ctrl.abort(); } catch (e) { /* 忽略 */ }
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
  // 按风格重写：整篇替换（警告会清批注），单独走一个端点
  if (editMode === "restyle") {
    if (!confirm("按风格重写会重建整篇笔记：现有批注将失效（可用「撤销」恢复）。确定应用？")) return;
    st.textContent = "保存中…";
    st.className = "status";
    try {
      await postJson("/api/note/restyle/apply", { text });
      editDrafts.clear(); // 整篇重建：旧块 id 的草稿全部失效
      st.textContent = "✓ 已保存";
      st.className = "status ok";
      closeEditModal();
      await refreshState();
      reloadNote(null, true);
    } catch (e) {
      st.textContent = "❌ " + e.message;
      st.className = "status err";
    }
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
    editDrafts.delete(editBlockId);
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
    editDrafts.delete(editBlockId);
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
  const r = el.getBoundingClientRect();
  const w = r.width || 380, h = r.height || 460;
  // 弹窗允许比视口大：这时只保证左/上边可见，不再往负方向推
  el.style.left = Math.min(Math.max(12, window.innerWidth - w - 12), Math.max(12, x)) + "px";
  el.style.top = Math.min(Math.max(12, window.innerHeight - h - 12), Math.max(12, y)) + "px";
}

/// 统一的指针拖拽：对 handle 做 setPointerCapture，
/// 这样指针移到笔记 iframe 上时事件也会重定向回 handle —— 不会丢 pointerup、
/// 不会「黏住」、不会在松开左键后还继续改大小。
function startPointerDrag(handle, e, { cursor, onMove, onEnd }) {
  const id = e.pointerId;
  try { handle.setPointerCapture(id); } catch (err) { /* 忽略 */ }
  document.body.classList.add("dragging");
  if (cursor) document.body.style.cursor = cursor;
  cancelBlkEditHide();
  $("blk-edit-btn").classList.add("hidden");
  const move = (ev) => { if (ev.pointerId === id) onMove(ev); };
  const finish = (ev) => {
    if (ev && ev.pointerId !== id) return;
    try { handle.releasePointerCapture(id); } catch (err) { /* 忽略 */ }
    handle.removeEventListener("pointermove", move);
    handle.removeEventListener("pointerup", finish);
    handle.removeEventListener("pointercancel", finish);
    document.body.classList.remove("dragging");
    document.body.style.cursor = "";
    if (onEnd) onEnd();
  };
  handle.addEventListener("pointermove", move);
  handle.addEventListener("pointerup", finish);
  handle.addEventListener("pointercancel", finish);
  e.preventDefault();
}

/// 让批注弹窗可拖动：按住头部（按钮/输入框除外）即可移动，并保证头部不滑出视口。
/// 位置不持久化——每次打开仍由 positionPopup 定位到选区 / 高亮附近。
function setupAnnDrag() {
  const popup = $("ann-popup");
  const head = popup.querySelector(".ann-head");
  if (!head) return;
  const MIN_VISIBLE_X = 60; // 横向至少露出这么多，避免拖出屏幕找不回
  const HEAD_H = 44;        // 纵向至少露出头部

  head.addEventListener("pointerdown", (e) => {
    if (e.target.closest("button, input, textarea")) return; // 按钮/输入框不触发拖动
    const r = popup.getBoundingClientRect();
    const off = { dx: e.clientX - r.left, dy: e.clientY - r.top, w: r.width };
    popup.classList.add("dragging");
    startPointerDrag(head, e, {
      cursor: "move",
      onMove: (ev) => {
        const left = Math.min(
          window.innerWidth - MIN_VISIBLE_X,
          Math.max(MIN_VISIBLE_X - off.w, ev.clientX - off.dx)
        );
        const top = Math.min(window.innerHeight - HEAD_H, Math.max(0, ev.clientY - off.dy));
        popup.style.left = left + "px";
        popup.style.top = top + "px";
      },
      onEnd: () => popup.classList.remove("dragging"),
    });
  });

  // 窗口尺寸变化后把可见的弹窗拉回视口内
  window.addEventListener("resize", () => {
    if (popup.classList.contains("hidden")) return;
    const r = popup.getBoundingClientRect();
    popup.style.left = Math.min(window.innerWidth - MIN_VISIBLE_X, Math.max(MIN_VISIBLE_X - r.width, r.left)) + "px";
    popup.style.top = Math.min(window.innerHeight - HEAD_H, Math.max(0, r.top)) + "px";
  });
}

const ANN_SIZE_KEY = "ph.ann.size";

/// 批注弹窗缩放：四条边 + 四个角都能拖（无上限，可超出视口再拖回来），
/// 尺寸存 localStorage，双击任意把手恢复默认。
function setupAnnResize() {
  const popup = $("ann-popup");
  const handles = [...popup.querySelectorAll(".ann-rz")];
  if (!handles.length) return;
  const MIN_W = 300, MIN_H = 240;
  try {
    const s = JSON.parse(localStorage.getItem(ANN_SIZE_KEY) || "null");
    if (s && s.w >= MIN_W && s.h >= MIN_H) {
      popup.style.width = s.w + "px";
      popup.style.height = s.h + "px";
    }
  } catch (e) { /* 忽略损坏的存储 */ }

  const saveSize = () => {
    const r = popup.getBoundingClientRect();
    try {
      localStorage.setItem(ANN_SIZE_KEY, JSON.stringify({ w: Math.round(r.width), h: Math.round(r.height) }));
    } catch (err) { /* 忽略 */ }
  };
  const resetSize = () => {
    popup.style.width = "";
    popup.style.height = "";
    try { localStorage.removeItem(ANN_SIZE_KEY); } catch (e) { /* 忽略 */ }
  };

  for (const handle of handles) {
    const dir = handle.dataset.dir || "se";
    handle.addEventListener("pointerdown", (e) => {
      const r = popup.getBoundingClientRect();
      const start = { x: e.clientX, y: e.clientY, l: r.left, t: r.top, w: r.width, h: r.height };
      const cursor = getComputedStyle(handle).cursor || "nwse-resize";
      popup.classList.add("resizing");
      startPointerDrag(handle, e, {
        cursor,
        onMove: (ev) => {
          const dx = ev.clientX - start.x, dy = ev.clientY - start.y;
          let l = start.l, t = start.t, w = start.w, h = start.h;
          if (dir.includes("e")) w = Math.max(MIN_W, start.w + dx);
          if (dir.includes("s")) h = Math.max(MIN_H, start.h + dy);
          if (dir.includes("w")) {
            w = Math.max(MIN_W, start.w - dx);
            l = start.l + (start.w - w); // 左/右边固定，向右扩展
          }
          if (dir.includes("n")) {
            h = Math.max(MIN_H, start.h - dy);
            t = start.t + (start.h - h); // 上/下边固定，向下扩展
          }
          popup.style.left = Math.round(l) + "px";
          popup.style.top = Math.round(t) + "px";
          popup.style.width = Math.round(w) + "px";
          popup.style.height = Math.round(h) + "px";
        },
        onEnd: () => {
          popup.classList.remove("resizing");
          saveSize();
        },
      });
    });
    handle.addEventListener("dblclick", resetSize);
  }
}

/// 思考过程区高度：底部拖动条调整（按面板存 localStorage）。
function setupReasoningResize() {
  document.querySelectorAll(".reasoning-resize").forEach((handle) => {
    const target = document.getElementById(handle.dataset.target || "");
    if (!target) return;
    const key = "ph.reasoning.h." + handle.dataset.target;
    try {
      const h = Number(localStorage.getItem(key) || 0);
      if (h >= 60) target.style.height = h + "px";
    } catch (e) { /* 忽略损坏的存储 */ }
    handle.addEventListener("pointerdown", (e) => {
      const start = { y: e.clientY, h: target.getBoundingClientRect().height };
      startPointerDrag(handle, e, {
        cursor: "row-resize",
        onMove: (ev) => {
          target.style.height = Math.max(60, Math.round(start.h + (ev.clientY - start.y))) + "px";
        },
        onEnd: () => {
          try {
            localStorage.setItem(key, String(Math.round(target.getBoundingClientRect().height)));
          } catch (err) { /* 忽略 */ }
        },
      });
    });
  });
}

/// 输入框随内容自增高（受 CSS max-height 限制，超出出滚动条）。
/// 用户拖动输入栏上游的把手会写入 dataset.baseH 作为最小高度。
function annInputGrow() {
  const ta = $("ann-q");
  if (!ta) return;
  const base = Number(ta.dataset.baseH || 0);
  ta.style.height = "auto";
  ta.style.height = Math.max(base, ta.scrollHeight) + "px";
}

/// 输入栏上的拖动条：上下拖动改变输入框高度（相对底部输入区）。
function setupAnnInputResize() {
  const handle = $("ann-input-resize");
  const ta = $("ann-q");
  if (!handle || !ta) return;
  handle.addEventListener("pointerdown", (e) => {
    const start = { y: e.clientY, h: ta.getBoundingClientRect().height };
    handle.classList.add("dragging");
    startPointerDrag(handle, e, {
      cursor: "row-resize",
      onMove: (ev) => {
        const h = Math.max(34, Math.round(start.h + (start.y - ev.clientY)));
        ta.dataset.baseH = String(h);
        ta.style.height = h + "px";
      },
      onEnd: () => handle.classList.remove("dragging"),
    });
  });
}

/// 引用附件条：展示本次提问针对的选中文字（可一键清除引用）。
function showAnnSubquote(text) {
  const t = (text || "").trim();
  const box = $("ann-subquote");
  if (!t) return clearAnnSubquote();
  $("ann-subquote-text").textContent = t;
  $("ann-subquote-text").title = t;
  box.classList.remove("hidden");
}
function clearAnnSubquote() {
  $("ann-subquote").classList.add("hidden");
  $("ann-subquote-text").textContent = "";
}

/// 新建批注：anchor = { block_id?, node_id?, quote（可见文本）, context（给 LLM，公式为 TeX）, rect? }。
/// 笔记批注：`block_id` 锚定笔记块；回答批注：`node_id` 锚定弹窗里的某条回答。
async function openAnnotationCreate(anchor) {
  const isAnswer = !!anchor.node_id;
  currentAnnotation = {
    id: null,
    block_id: anchor.block_id || "",
    node_id: anchor.node_id || null,
    quote: anchor.quote,
    quote_tex: anchor.context || anchor.quote,
  };
  if (!isAnswer) annSelectedNode = null;
  await loadMathLibs();
  $("ann-quote").textContent = anchor.quote;
  showAnnSubquote(anchor.quote);
  if (!isAnswer) {
    $("ann-thread").innerHTML = '<p class="muted">输入问题后回车发送；这会在该处创建一条批注。</p>';
  }
  $("ann-popup").classList.remove("hidden");
  if (isAnswer && anchor.rect) {
    positionPopup(anchor.rect.left, anchor.rect.bottom + 10);
  } else {
    const doc = noteFrame.contentDocument;
    const sel = doc && doc.getSelection();
    const rect = sel && sel.rangeCount ? sel.getRangeAt(0).getBoundingClientRect() : { left: 200, bottom: 200 };
    const fr = noteFrame.getBoundingClientRect();
    positionPopup(fr.left + rect.left, fr.top + rect.bottom + 10);
  }
  $("ann-q").value = "";
  annInputGrow();
  $("ann-q").focus();
}

async function openAnnotationView(annId, opts = {}) {
  await refreshAnnotations();
  const ann = annotationsCache.find((a) => a.id === annId);
  if (!ann) return;
  currentAnnotation = {
    id: ann.id,
    block_id: ann.block_id,
    node_id: ann.node_id || null,
    quote: ann.quote,
    quote_tex: ann.quote_tex || ann.quote,
  };
  annSelectedNode = ann.thread ? ann.thread.node_id : null;
  await loadMathLibs();
  clearAnnSubquote();
  $("ann-quote").textContent = cleanQuote(ann.quote);
  renderAnnThread(ann.thread);
  $("ann-popup").classList.remove("hidden");
  const doc = noteFrame.contentDocument;
  const mark = doc && doc.querySelector(`mark.ann-mark[data-ann-id="${annId}"]`);
  if (mark && opts.scroll) mark.scrollIntoView({ block: "center" });
  if (mark) {
    const r = mark.getBoundingClientRect();
    const fr = noteFrame.getBoundingClientRect();
    positionPopup(fr.left + r.left, fr.top + r.bottom + 10);
  } else {
    // 笔记里没有对应高亮（回答批注 / 笔记被改过）：优先找弹窗线程里的高亮
    const tmark = document.querySelector(`#ann-thread mark.ann-mark[data-ann-id="${annId}"]`);
    if (tmark) {
      if (opts.scroll) tmark.scrollIntoView({ block: "center" });
      const r = tmark.getBoundingClientRect();
      positionPopup(r.left, r.bottom + 10);
    } else {
      // 至少滚动到批注所在块（回答批注没有块则居中显示）
      if (opts.scroll && ann.block_id) scrollNoteToBlock(ann.block_id);
      positionPopup(window.innerWidth / 2 - 190, 120);
    }
  }
  $("ann-q").focus();
}

/// 绑定节点点击但忽略「拖动选择」：拖选回答文字后浏览器会在共同祖先补发 click，
/// 若不忽略就会触发节点跳转/折叠并重建 DOM，把选中的高亮清掉（并闪出顶栏停止按钮）。
function onAnnNodeClick(el, fn) {
  let down = null;
  el.addEventListener("pointerdown", (e) => { down = { x: e.clientX, y: e.clientY }; });
  el.addEventListener("click", (e) => {
    const dragged = down && (Math.abs(e.clientX - down.x) > 4 || Math.abs(e.clientY - down.y) > 4);
    down = null;
    if (dragged) return;
    fn(e);
  });
}

let renderedThreadRoot = null; // 当前弹窗里渲染的线程根（点击回答高亮时需要重渲染）

/// 节点引文行：提问所针对的原文/回答片段（回看时能看出“问的是哪一段”）。
function annQuoteEl(quote) {
  const text = (quote || "").trim();
  if (!text) return null;
  const el = document.createElement("div");
  el.className = "ann-q-quote";
  el.textContent = "针对：" + text;
  el.title = text;
  return el;
}

function renderAnnThread(root) {
  const box = $("ann-thread");
  box.innerHTML = "";
  renderedThreadRoot = root || null;
  if (!root) { box.innerHTML = '<p class="muted">（尚无问答）</p>'; return; }
  const add = (node, depth, container) => {
    const div = document.createElement("div");
    div.className = "ann-node" + (node.is_check ? " check" : "") + (annSelectedNode === node.node_id ? " selected" : "");
    div.dataset.nodeId = node.node_id;
    div.style.marginLeft = depth * 10 + "px";

    const q = document.createElement("div");
    q.className = "ann-q";
    q.textContent = (node.is_check ? "[核对] " : "") + node.question;
    const quoteEl = annQuoteEl(node.quote);
    if (quoteEl) div.appendChild(quoteEl);
    const a = document.createElement("div");
    a.className = "ann-a";
    a.innerHTML = renderMathMarkdown(node.answer || "");
    // 回答批注：在该条回答里高亮它引用的文字
    for (const ann of annotationsCache) {
      if (ann.node_id && ann.node_id === node.node_id) {
        const r = document.createRange();
        r.selectNodeContents(a);
        wrapQuote(document, r, ann.quote, ann.id);
      }
    }

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
      onAnnNodeClick(div, (e) => {
        e.stopPropagation();
        const open = orig.style.display !== "none";
        orig.style.display = open ? "none" : "block";
        hint.textContent = open ? "▶ 展开原对话" : "▼ 收起";
      });
      div.oncontextmenu = (e) => {
        if (e.target.closest && e.target.closest("mark.ann-mark")) return; // 回答高亮交给批注菜单
        e.preventDefault();
        showAnnNodeMenu(e.clientX, e.clientY, node);
      };
      container.appendChild(div);
      return;
    }

    // 普通节点
    div.appendChild(q);
    div.appendChild(a);
    onAnnNodeClick(div, () => {
      annSelectedNode = node.node_id;
      renderAnnThread(root);
      runCommand("goto " + node.n, { skipReload: true });
    });
    div.oncontextmenu = (e) => {
      if (e.target.closest && e.target.closest("mark.ann-mark")) return; // 回答高亮交给批注菜单
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
  $("ann-sel-btn").classList.add("hidden");
  clearAnnSubquote();
  currentAnnotation = null;
  annSelectedNode = null;
}

/// 乐观追加一个待回答节点（问题 + 思考中…），返回回答元素供流式填充。
function appendPendingNode(question, quote) {
  const box = $("ann-thread");
  const muted = box.querySelector(".muted");
  if (muted) box.innerHTML = "";
  const div = document.createElement("div");
  div.className = "ann-node pending";
  const quoteEl = annQuoteEl(quote);
  if (quoteEl) div.appendChild(quoteEl);
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

/// 「记概念」开关：读 DOM（缺省勾选），并发时保持一致。
const ANN_RECORD_KEY = "ph.ann.recordConcept";
function recordConceptOn() {
  const el = $("ann-record");
  return el ? !!el.checked : true;
}

async function sendAnnotation() {
  const q = $("ann-q").value.trim();
  if (!q || !currentAnnotation) return;
  if (running || llmBusyTask) { showBusyHint(); return; }
  llmBusyTask = "回答提问";
  $("ann-q").value = "";
  annInputGrow();
  clearAnnSubquote();
  $("ann-send").disabled = true;
  setAnnProgress("思考中…");
  resetReasoning("ann");
  const ansEl = appendPendingNode(q, currentAnnotation.quote);
  let url, body;
  const answerAnchorNode = currentAnnotation.node_id && !currentAnnotation.id ? currentAnnotation.node_id : null;
  if (!currentAnnotation.id) {
    if (answerAnchorNode) {
      // 回答批注：锚定该回答所在节点，新问答成为它的子节点
      url = "/api/annotate/answer";
      body = {
        node_id: answerAnchorNode,
        quote: currentAnnotation.quote,
        quote_tex: currentAnnotation.quote_tex || null,
        question: q,
        mode: annMode,
        record_concept: recordConceptOn(),
      };
    } else {
      url = "/api/annotate";
      body = {
        block_id: currentAnnotation.block_id,
        quote: currentAnnotation.quote,
        quote_tex: currentAnnotation.quote_tex || null,
        question: q,
        mode: annMode,
        record_concept: recordConceptOn(),
      };
    }
  } else {
    const nodeId = annSelectedNode;
    if (!nodeId) { $("ann-send").disabled = false; setAnnProgress(""); return; }
    url = "/api/annotate/reply";
    body = { node_id: nodeId, question: q, mode: annMode, record_concept: recordConceptOn() };
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
        if (!streamed) {
          showBar("ann-bar", false); // 有内容流出即收起进度条与计时
          setAnnProgress("");
        }
        streamed += text;
        ansEl.textContent = streamed;
        $("ann-thread").scrollTop = $("ann-thread").scrollHeight;
      } else if (name === "reasoning") {
        appendReasoning("ann", text);
      } else if (name === "stdout") {
        appendConsole(text);
      } else if (name === "stderr") {
        appendConsole(text, "err");
      } else if (name === "progress") {
        setAnnProgress(text);   // 进度显示在弹窗右上角（自动带计时）
      } else if (name === "progress_done") {
        setAnnProgress("");
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
  llmBusyTask = "";
  showBar("ann-bar", false);
  $("ann-stop").classList.add("hidden");
  $("ann-send").disabled = false;
  setAnnProgress("");
  const rbox = $("ann-reasoning");
  if (rbox && !rbox.classList.contains("hidden")) rbox.open = false; // 思考过程收起但保留
  await refreshState();
  await refreshAnnotations();
  if (currentAnnotation.id) {
    const ann = annotationsCache.find((a) => a.id === currentAnnotation.id);
    if (ann) {
      currentAnnotation = {
        id: ann.id,
        block_id: ann.block_id,
        node_id: ann.node_id || null,
        quote: ann.quote,
        quote_tex: ann.quote_tex || ann.quote,
      };
      renderAnnThread(ann.thread);
    }
  } else {
    // 新建：匹配最新一条（同块/同节点 + 同引用）
    const latest = annotationsCache[annotationsCache.length - 1];
    const sameAnchor = latest &&
      (latest.block_id || "") === (currentAnnotation.block_id || "") &&
      (latest.node_id || "") === (currentAnnotation.node_id || "");
    if (sameAnchor && latest.quote === currentAnnotation.quote) {
      currentAnnotation = {
        id: latest.id,
        block_id: latest.block_id,
        node_id: latest.node_id || null,
        quote: latest.quote,
        quote_tex: latest.quote_tex || latest.quote,
      };
      annSelectedNode = latest.thread ? latest.thread.node_id : null;
      renderAnnThread(latest.thread);
    }
  }
  // 回答批注：在父线程里选中并滚动到刚创建的子节点
  if (answerAnchorNode) {
    const latest = [...annotationsCache].reverse().find((a) => a.node_id === answerAnchorNode);
    if (latest) {
      annSelectedNode = latest.root_node_id;
      const parent = currentAnnotation.id
        ? annotationsCache.find((a) => a.id === currentAnnotation.id)
        : null;
      if (parent) renderAnnThread(parent.thread);
      const el = document.querySelector(`#ann-thread .ann-node[data-node-id="${latest.root_node_id}"]`);
      if (el) el.scrollIntoView({ block: "center" });
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
  // LLM 任务运行中也可自由切换：任务与发起会话绑定，结果写回那里（后端按会话版本校验）；
  // 导入的实时反馈覆盖层随会话隐藏/恢复，完成后在控制台提示。
  if (importRun) {
    if (importRun.sessionId && importRun.sessionId === id) noteGenShow();
    else noteGenHide();
  } else {
    noteGenHide(); // 没有后台导入时切会话，确保覆盖层不残留
  }
  const res = await fetch("/api/sessions/load", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ id }),
  });
  if (res.ok) {
    await refreshState();
    switchTab("note");
    // 批注定位：笔记加载完后自动打开该批注（正文里没有 expl- 锚点）
    if (opts.ann) pendingAnnOpen = { id: opts.ann, block: opts.block || null };
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

// ===== 导入（论文 / 笔记·讲义）与笔记风格 =====

let stylesCache = [];   // GET /api/styles 的结果
let importCtx = null;   // 导入弹窗状态：{ file, mode: "paper"|"note" }

/// 拉取风格列表（导入弹窗与风格管理共用）。
async function refreshStyles() {
  try {
    const d = await (await fetch("/api/styles")).json();
    stylesCache = d.styles || [];
  } catch (e) {
    console.error(e);
  }
}

/// 按当前模式渲染风格下拉：论文只看 scope=paper/any；
/// 笔记·讲义看 scope=note/any，且第一项是「原样导入（不调 LLM）」。
function renderImportStyles() {
  const sel = $("import-style");
  const mode = importCtx ? importCtx.mode : "paper";
  const ok = (scope) =>
    mode === "paper" ? scope === "paper" || scope === "any" : scope === "note" || scope === "any";
  const prev = sel.value;
  sel.innerHTML = "";
  if (mode === "note") {
    const o = document.createElement("option");
    o.value = "__raw__";
    o.textContent = "原样导入（不调 LLM，0 token）";
    sel.appendChild(o);
  }
  for (const s of stylesCache) {
    if (!ok(s.scope)) continue;
    const o = document.createElement("option");
    o.value = s.id;
    o.textContent = s.label + (s.builtin ? "" : "（自定义）");
    o.title = s.desc || "";
    sel.appendChild(o);
  }
  const has = [...sel.options].some((o) => o.value === prev);
  sel.value = has ? prev : (mode === "note" ? "__raw__" : "four");
  updateImportStyleHint();
}

/// 切换风格时更新说明与「额外要求/另存为」的可见性。
function updateImportStyleHint() {
  const id = $("import-style").value;
  const raw = id === "__raw__";
  const s = stylesCache.find((x) => x.id === id);
  $("import-style-desc").textContent = raw
    ? "直接读取文件内容建笔记，不调用模型、不花 token"
    : (s ? s.desc : "");
  $("import-extra-wrap").classList.toggle("hidden", raw);
  $("btn-import-save-style").classList.toggle("hidden", raw);
}

function setImportMode(mode) {
  if (!importCtx) return;
  importCtx.mode = mode;
  document.querySelectorAll("#import-modes .mode-tab").forEach((t) =>
    t.classList.toggle("active", t.dataset.mode === mode)
  );
  renderImportStyles();
}

/// 弹出导入弹窗，返回 {name, mode, style, extra} 或 null（取消）。
async function promptImport(file, defaultName) {
  await refreshStyles();
  const isPdf = /\.pdf$/i.test(file.name);
  return new Promise((resolve) => {
    importResolve = resolve;
    importCtx = { file, mode: isPdf ? "paper" : "note" };
    $("import-file").textContent = "文件：" + file.name + "（可直接拖拽多个文件逐个导入）";
    $("import-name").value = defaultName;
    $("import-extra").value = "";
    document.querySelectorAll("#import-modes .mode-tab").forEach((t) =>
      t.classList.toggle("active", t.dataset.mode === importCtx.mode)
    );
    renderImportStyles();
    $("import-modal").classList.remove("hidden");
    $("import-name").focus();
    $("import-name").select();
  });
}

function closeImportModal(result) {
  $("import-modal").classList.add("hidden");
  importCtx = null;
  if (importResolve) {
    const r = importResolve;
    importResolve = null;
    r(result);
  }
}

/// 「另存为风格…」：把所选风格的提示词 + 本次额外要求保存成一个新风格。
async function saveImportAsStyle() {
  const id = $("import-style").value;
  if (id === "__raw__") return;
  const base = (stylesCache.find((s) => s.id === id) || {}).prompt || "";
  const extra = $("import-extra").value.trim();
  const newId = (prompt("新风格 id（字母/数字/-/_）", "") || "").trim();
  if (!newId) return;
  const label = (prompt("新风格名称", newId) || "").trim() || newId;
  const promptText = base + (extra ? `\n\n【本次额外要求】\n${extra}` : "");
  const scope = importCtx && importCtx.mode === "note" ? "note" : "paper";
  try {
    await postJson("/api/styles/save", {
      id: newId,
      label,
      desc: extra.slice(0, 60) || `由「${id}」另存`,
      scope,
      prompt: promptText,
    });
    await refreshStyles();
    renderImportStyles();
    $("import-style").value = newId;
    updateImportStyleHint();
    appendConsole(`✓ 已保存风格 ${newId}（可在「管理…」里编辑）`);
  } catch (e) {
    alert("保存失败: " + e.message);
  }
}

async function importFile(file) {
  if (!file) return;
  if (running || llmBusyTask) { showBusyHint(); return; }
  const stem = file.name.replace(/\.[^.]+$/, "").slice(0, 20);
  const opts = await promptImport(file, "笔记_" + stem + ".md");
  if (!opts) return; // 取消
  const exportName = opts.name.trim() || ("笔记_" + stem + ".md");
  try {
    // HTML：浏览器端先转成 Markdown（公式按 KaTeX 隐藏层还原），再当 md 上传
    let uploadTarget = file;
    let isText = /\.(txt|md|markdown)$/i.test(file.name);
    if (/\.html?$/i.test(file.name)) {
      setProgress("转换 HTML…");
      const conv = htmlToMarkdown(await file.text());
      if (!conv.md.trim()) throw new Error("HTML 里没有提取到正文");
      // 数学宏定义随笔记一起带走（解析器会抽出），供 KaTeX 注册
      const withMacros = conv.macros
        ? `<!-- paperhelper-macros\n${conv.macros}\n-->\n\n${conv.md}`
        : conv.md;
      uploadTarget = new File([withMacros], stem + ".md", { type: "text/markdown" });
      isText = true;
    }
    setProgress("上传中…");
    // 反馈全部进「笔记区」覆盖层（阶段/思考/流式笔记），不占用控制台
    switchTab("note");
    noteGenReset();
    noteGenPhase("上传中…");
    const isRaw = opts.mode === "note" && opts.style === "__raw__";
    const modeLabel = opts.mode === "paper" ? "论文 → 生成笔记" : isRaw ? "原样导入" : "风格 " + opts.style;
    noteGenLog("导入文件：" + file.name + "（" + modeLabel + "）");
    const j = await uploadFile(uploadTarget, (p) => setUploadPct(p));
    setProgress("");
    const extra = (opts.extra || "").trim();
    const extraArg = extra ? " --extra " + shellQuote(extra) : "";
    const textArg = isText ? "--text " : "";
    // PDF 讲义标为 lecture，md/txt/html 笔记标为 note
    const kind = opts.mode === "note" ? (/\.pdf$/i.test(file.name) ? "lecture" : "note") : "paper";
    const cmd = isRaw
      ? `ingest --note --kind ${kind} ${textArg}` + shellQuote(j.path)
      : `ingest --style ${opts.style} --kind ${kind}${extraArg} ${textArg}` + shellQuote(j.path);
    // 记录发起会话：用户切走再切回时恢复实时反馈覆盖层
    importRun = { sessionId: (lastState && lastState.session_id) || "" };
    try {
      await runCommand(cmd, { export: exportName, ui: "import", quiet: true });
    } finally {
      importRun = null;
    }
  } catch (e) {
    noteGenLog("❌ 导入失败: " + e.message, "err");
    appendConsole("❌ 导入失败: " + e.message, "err");
    setProgress("");
    noteGenHide();
    switchTab("console");
  }
}

/// 把路径包成双引号（内部反斜杠/引号转义），与后端 normalize_path_arg 对应。
function shellQuote(p) {
  return '"' + String(p).replace(/\\/g, "\\\\").replace(/"/g, '\\"') + '"';
}

// ===== 风格管理（设置弹窗的「笔记风格」页签）=====

let editingStyleId = null; // null = 新建

function renderStylesList() {
  const box = $("styles-list");
  box.innerHTML = "";
  for (const s of stylesCache) {
    const scope = s.scope === "paper" ? "论文" : s.scope === "note" ? "讲义" : "通用";
    const d = document.createElement("div");
    d.className = "style-item" + (s.id === editingStyleId ? " active" : "");
    d.innerHTML = `<span>${esc(s.label)}</span><span class="s-meta">${esc(s.id)} · ${s.builtin ? "内置" : "自定义"} · ${scope}</span>`;
    d.onclick = () => selectStyleForEdit(s.id);
    box.appendChild(d);
  }
}

function selectStyleForEdit(id) {
  const s = stylesCache.find((x) => x.id === id);
  if (!s) return;
  editingStyleId = id;
  $("style-id").value = s.id;
  // 内置风格的 id 固定（--style 示例/「恢复默认」依赖它）；自定义风格可改名
  $("style-id").disabled = !!s.builtin;
  $("style-label").value = s.label;
  $("style-desc").value = s.desc || "";
  $("style-scope").value = s.scope || "any";
  $("style-prompt").value = s.prompt || "";
  $("style-status").textContent = s.builtin
    ? "内置风格：可改名称/说明/提示词，点「恢复默认」可还原（id 固定）"
    : "自定义风格：可改 id（文件名会一起改）与全部内容";
  $("style-status").className = "status";
  renderStylesList();
}

function newStyleForEdit() {
  editingStyleId = null;
  $("style-id").value = "";
  $("style-id").disabled = false;
  $("style-label").value = "";
  $("style-desc").value = "";
  $("style-scope").value = "any";
  $("style-prompt").value =
    "请阅读以下资料，生成一份学习笔记 Markdown。\n\n要求：\n- 先讲清直觉，再给公式与推导；\n- 保留关键公式、数据与例子。";
  $("style-status").textContent = "新建风格：填 id 与名称后点「保存」";
  $("style-status").className = "status";
  renderStylesList();
}

function copyStyleForEdit() {
  const s = stylesCache.find((x) => x.id === editingStyleId);
  if (!s) { newStyleForEdit(); return; }
  const base = { id: s.id, label: s.label, desc: s.desc, scope: s.scope, prompt: s.prompt };
  newStyleForEdit();
  $("style-id").value = base.id + "-copy";
  $("style-label").value = base.label + "（副本）";
  $("style-desc").value = base.desc || "";
  $("style-scope").value = base.scope || "any";
  $("style-prompt").value = base.prompt || "";
}

async function saveStyleFromForm() {
  const id = $("style-id").value.trim();
  const st = $("style-status");
  try {
    const oldId = editingStyleId && editingStyleId !== id ? editingStyleId : null;
    await postJson("/api/styles/save", {
      id,
      old_id: oldId,
      label: $("style-label").value.trim(),
      desc: $("style-desc").value.trim(),
      scope: $("style-scope").value,
      prompt: $("style-prompt").value,
    });
    await refreshStyles();
    editingStyleId = id;
    selectStyleForEdit(id);
    st.textContent = "✓ 已保存";
    st.className = "status ok";
  } catch (e) {
    st.textContent = "❌ " + e.message;
    st.className = "status err";
  }
}

async function deleteStyleFromForm() {
  const id = $("style-id").value.trim();
  const st = $("style-status");
  if (!id || !confirm(`删除风格「${id}」？提示词文件会一并删除。`)) return;
  try {
    await postJson("/api/styles/delete", { id });
    await refreshStyles();
    editingStyleId = stylesCache.length ? stylesCache[0].id : null;
    if (editingStyleId) selectStyleForEdit(editingStyleId);
    else newStyleForEdit();
    st.textContent = "✓ 已删除";
    st.className = "status ok";
  } catch (e) {
    st.textContent = "❌ " + e.message;
    st.className = "status err";
  }
}

async function resetStyleFromForm() {
  const id = $("style-id").value.trim();
  const st = $("style-status");
  if (!id || !confirm(`把「${id}」恢复为内置默认提示词？`)) return;
  try {
    await postJson("/api/styles/reset", { id });
    await refreshStyles();
    selectStyleForEdit(id);
    st.textContent = "✓ 已恢复默认";
    st.className = "status ok";
  } catch (e) {
    st.textContent = "❌ " + e.message;
    st.className = "status err";
  }
}

// ===== HTML → Markdown（浏览器端，导入讲义用） =====

/// 把 HTML 转成 Markdown：只提取白名单结构/属性（script/style/iframe/on* 一律丢弃），
/// KaTeX 公式从隐藏 MathML 的 <annotation encoding="application/x-tex"> 还原成 $...$，
/// Pandoc/MathJax 的 `.math` 容器（`\(...\)`/`\[...\]`）同样转成 $...$/$$...$$。
/// 隐藏元素一律跳过；但「几乎全是 \newcommand 定义」的宏块会被收集，返回 { md, macros }。
function htmlToMarkdown(html) {
  const doc = new DOMParser().parseFromString(String(html || ""), "text/html");
  const macros = [];
  const safeUrl = (u) => {
    const s = String(u || "").trim();
    return /^(https?:|mailto:|data:image\/)/i.test(s) ? s : "";
  };
  const isHidden = (el) => {
    if (!el.getAttribute) return false;
    if (el.hasAttribute("hidden") || el.getAttribute("aria-hidden") === "true") return true;
    const style = el.getAttribute("style") || "";
    return /display\s*:\s*none/i.test(style);
  };
  // 宏块识别：含宏定义，且去掉定义后几乎没有别的正文
  const collectMacros = (text) => {
    if (!text || !text.includes("\\")) return;
    const { macros: defs, leftover } = scanMacroDefs(text);
    if (!Object.keys(defs).length) return;
    const rest = leftover.replace(/\\[()\[\]]/g, "").replace(/\s+/g, "");
    if (rest.length <= 24) macros.push(text.trim());
  };
  const inline = (node) => {
    if (node.nodeType === Node.TEXT_NODE) return node.nodeValue.replace(/\s+/g, " ");
    if (node.nodeType !== Node.ELEMENT_NODE) return "";
    const el = node;
    const tag = el.tagName.toLowerCase();
    if (el.classList && el.classList.contains("katex")) {
      const ann = el.querySelector('annotation[encoding="application/x-tex"]');
      const tex = ann ? ann.textContent.trim() : (el.textContent || "").trim();
      return (el.closest && el.closest(".katex-display")) ? `$$${tex}$$` : `$${tex}$`;
    }
    // Pandoc/MathJax：<span class="math inline|display">\(…\) / \[…\]
    if (el.classList && el.classList.contains("math") && !el.querySelector(".katex")) {
      const display = el.classList.contains("display");
      const tex = (el.textContent || "")
        .replace(/^\s*\\[\(\[]/, "")
        .replace(/\\[\)\]]\s*$/, "")
        .trim();
      if (tex) return display ? `$$${tex}$$` : `$${tex}$`;
    }
    if (["script", "style", "noscript", "iframe", "svg", "canvas"].includes(tag)) return "";
    const children = [...el.childNodes].map(inline).join("");
    switch (tag) {
      case "strong": case "b": return children.trim() ? `**${children.trim()}**` : "";
      case "em": case "i": return children.trim() ? `*${children.trim()}*` : "";
      case "code": return children ? "`" + children.trim() + "`" : "";
      case "a": {
        const href = safeUrl(el.getAttribute("href"));
        return href ? `[${children.trim() || href}](${href})` : children;
      }
      case "img": {
        const src = safeUrl(el.getAttribute("src"));
        const alt = el.getAttribute("alt") || "";
        return src ? `![${alt}](${src})` : alt ? `![${alt}]()` : "";
      }
      case "br": return "\n";
      default: return children;
    }
  };
  const blocks = [];
  const walkBlock = (el) => {
    for (const child of el.children) {
      const tag = child.tagName.toLowerCase();
      if (["script", "style", "noscript", "iframe", "nav", "footer", "svg", "canvas"].includes(tag)) continue;
      // 宏块（常被藏在 display:none 的容器里）：收集而不是当正文
      const before = macros.length;
      collectMacros(child.textContent || "");
      if (macros.length > before) continue;
      if (isHidden(child)) continue;
      if (/^h[1-6]$/.test(tag)) {
        const text = inline(child).trim();
        if (text) blocks.push("#".repeat(+tag[1]) + " " + text);
        continue;
      }
      if (tag === "p") {
        const text = inline(child).trim();
        if (text) blocks.push(text);
        continue;
      }
      if (["div", "section", "article", "main", "header", "aside"].includes(tag)) {
        const hasBlock = child.querySelector("p,div,section,article,ul,ol,table,pre,h1,h2,h3,h4,h5,h6");
        if (hasBlock) walkBlock(child);
        else {
          const text = inline(child).trim();
          if (text) blocks.push(text);
        }
        continue;
      }
      if (tag === "ul" || tag === "ol") {
        const items = [...child.querySelectorAll(":scope > li")]
          .map((li, i) => {
            const text = inline(li).trim();
            return text ? (tag === "ol" ? i + 1 + ". " : "- ") + text : "";
          })
          .filter(Boolean);
        if (items.length) blocks.push(items.join("\n"));
        continue;
      }
      if (tag === "pre") {
        const code = child.textContent.replace(/^\n+|\s+$/g, "");
        if (code) blocks.push("```\n" + code + "\n```");
        continue;
      }
      if (tag === "table") {
        const rows = [...child.querySelectorAll("tr")].map((tr) =>
          [...tr.children].map((td) => inline(td).trim().replace(/\|/g, "\\|"))
        ).filter((r) => r.length);
        if (rows.length) {
          const lines = [
            "| " + rows[0].join(" | ") + " |",
            "| " + rows[0].map(() => "---").join(" | ") + " |",
          ];
          for (const r of rows.slice(1)) lines.push("| " + r.join(" | ") + " |");
          blocks.push(lines.join("\n"));
        }
        continue;
      }
      const text = inline(child).trim();
      if (text) blocks.push(text);
      else walkBlock(child);
    }
  };
  walkBlock(doc.body);
  return {
    md: blocks.join("\n\n").replace(/\n{3,}/g, "\n\n").trim(),
    macros: macros.join("\n").trim(),
  };
}

// ===== 设置弹窗（模型设置 / 笔记风格）=====

/// 切换设置页签。
function setSettingsTab(tab) {
  document.querySelectorAll("#settings-tabs .settings-tab").forEach((t) =>
    t.classList.toggle("active", t.dataset.tab === tab)
  );
  $("settings-model").classList.toggle("hidden", tab !== "model");
  $("settings-style").classList.toggle("hidden", tab !== "style");
}

/// 载入「笔记风格」页签（列表 + 当前选中项）。
async function loadStylesPane() {
  await refreshStyles();
  if (!editingStyleId && stylesCache.length) editingStyleId = stylesCache[0].id;
  renderStylesList();
  if (editingStyleId) selectStyleForEdit(editingStyleId);
}

async function openConfig(tab = "model") {
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
    if (tab === "style") await loadStylesPane();
    setSettingsTab(tab);
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
  setupAnnDrag();

  // 中止：进度条旁的停止按钮 / 批注弹窗停止按钮（等同 Ctrl-C）
  $("btn-stop").onclick = stopCurrent;
  $("ann-stop").onclick = stopCurrent;
  $("note-gen-stop").onclick = stopCurrent;

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
  {
    const rec = $("ann-record");
    rec.checked = localStorage.getItem(ANN_RECORD_KEY) !== "0";
    rec.onchange = () => {
      try { localStorage.setItem(ANN_RECORD_KEY, rec.checked ? "1" : "0"); } catch (e) { /* 忽略 */ }
    };
  }
  $("ann-subquote-clear").onclick = () => clearAnnSubquote();
  $("ann-send").onclick = sendAnnotation;
  // 弹窗线程内：选中回答文字浮出「提问」；回答里的高亮可点击/右键
  document.addEventListener("mouseup", () => setTimeout(showAnnSelButton, 0));
  $("ann-thread").addEventListener("click", (e) => {
    const mark = e.target.closest ? e.target.closest("mark.ann-mark") : null;
    if (!mark) return;
    e.preventDefault();
    e.stopPropagation();
    const ann = annotationsCache.find((a) => a.id === mark.dataset.annId);
    if (!ann) return;
    annSelectedNode = ann.root_node_id;
    if (renderedThreadRoot) renderAnnThread(renderedThreadRoot);
    const el = document.querySelector(`#ann-thread .ann-node[data-node-id="${ann.root_node_id}"]`);
    if (el) el.scrollIntoView({ block: "center" });
  });
  $("ann-thread").addEventListener("contextmenu", (e) => {
    const mark = e.target.closest ? e.target.closest("mark.ann-mark") : null;
    if (!mark) return;
    e.preventDefault();
    e.stopPropagation();
    showAnnMarkMenu(mark.dataset.annId, e.clientX, e.clientY);
  });
  $("ann-mode").onclick = () => {
    annMode = annMode === "ask" ? "check" : "ask";
    $("ann-mode").textContent = annMode;
    $("ann-mode").classList.toggle("check", annMode === "check");
  };
  $("ann-q").addEventListener("keydown", (e) => {
    // Enter 发送；Shift+Enter 换行；中文输入法组词中的回车不发送
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); sendAnnotation(); }
  });
  $("ann-q").addEventListener("input", annInputGrow);
  setupAnnInputResize();
  setupAnnResize();
  setupReasoningResize();
  $("btn-config").onclick = () => openConfig("model");
  $("btn-settings-close").onclick = () => $("config-modal").classList.add("hidden");
  document.querySelectorAll("#settings-tabs .settings-tab").forEach(
    (t) => (t.onclick = () => {
      setSettingsTab(t.dataset.tab);
      if (t.dataset.tab === "style") loadStylesPane();
    })
  );
  $("btn-config-test").onclick = testConfig;
  $("btn-config-test-stop").onclick = stopConfigTest;
  $("btn-config-save").onclick = saveConfig;

  // 导入弹窗：模式 + 动态风格 + 临时额外要求 + 风格管理
  $("btn-import-cancel").onclick = () => closeImportModal(null);
  $("btn-import-ok").onclick = () =>
    closeImportModal({
      name: $("import-name").value.trim(),
      mode: importCtx ? importCtx.mode : "paper",
      style: $("import-style").value,
      extra: $("import-extra").value,
    });
  $("import-name").addEventListener("keydown", (e) => {
    if (e.key === "Enter") { e.preventDefault(); $("btn-import-ok").click(); }
  });
  document.querySelectorAll("#import-modes .mode-tab").forEach(
    (t) => (t.onclick = () => setImportMode(t.dataset.mode))
  );
  $("import-style").onchange = updateImportStyleHint;
  $("btn-style-manage").onclick = () => openConfig("style");
  $("btn-import-save-style").onclick = saveImportAsStyle;

  // 风格管理（设置 → 笔记风格）
  $("btn-style-new").onclick = newStyleForEdit;
  $("btn-style-copy").onclick = copyStyleForEdit;
  $("btn-style-save").onclick = saveStyleFromForm;
  $("btn-style-delete").onclick = deleteStyleFromForm;
  $("btn-style-reset").onclick = resetStyleFromForm;

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
  setInterval(refreshState, 15000);
});
