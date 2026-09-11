// PaperHelper Web 前端：原生 JS，无构建。
// 后端约定：POST /api/run 返回 SSE（event: stdout/stderr/token/progress/progress_done/done/error，
// data 为 JSON 字符串）。其余接口为普通 JSON。

const $ = (id) => document.getElementById(id);
const consoleEl = $("console");
const noteFrame = $("note-frame");
const cmdInput = $("cmd-input");
const suggestEl = $("cmd-suggest");

let running = false;
let streamSpan = null;      // 当前流式 token 的容器
let lastState = null;       // 最近一次 /api/state 快照
const dynamicTabs = new Map(); // key -> { btn, pane }
let suggestIndex = 0;
const cmdHistory = [];
let histIdx = 0;

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
  if (!streamSpan) {
    streamSpan = document.createElement("span");
    streamSpan.className = "stream";
    consoleEl.appendChild(streamSpan);
  }
  streamSpan.textContent += t;
  consoleEl.scrollTop = consoleEl.scrollHeight;
}

function setProgress(msg) { $("progress").textContent = msg || ""; }

function setRunning(v) {
  running = v;
  $("btn-send").disabled = v;
  $("btn-stop").disabled = !v;
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
  btn.innerHTML = `${esc(title)} <span class="close" title="关闭">×</span>`;
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

function handleFrame(frame) {
  let name = "message";
  let data = "";
  for (const line of frame.split("\n")) {
    if (line.startsWith("event:")) name = line.slice(6).trim();
    else if (line.startsWith("data:")) data += line.slice(5).trim();
  }
  let text = data;
  try { text = JSON.parse(data); } catch (e) { /* 原始文本 */ }
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

async function runCommand(command, exportPath) {
  if (running) return;
  if (!command || !command.trim()) return;
  setRunning(true);
  appendConsole("> " + command, "ok");
  // 不自动跳控制台；仅出错时（handleFrame 的 error）切过去
  try {
    const res = await fetch("/api/run", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ command, export: exportPath || null }),
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
    reloadNote();
  }
}

// ===== 状态刷新 =====

async function refreshState() {
  try {
    const st = await (await fetch("/api/state")).json();
    lastState = st;
    renderModel(st);
    renderUsage(st);
    renderTree(st);
    renderPapers(st);
    renderConcepts(st);
  } catch (e) {
    console.error(e);
  }
  await refreshSessions();
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
  $("btn-undo").disabled = !st.can_undo;
}

function renderTree(st) {
  const ul = $("tree");
  ul.innerHTML = "";
  if (!st.tree || st.tree.length === 0) {
    ul.innerHTML = '<li class="muted">（尚无对话）</li>';
    return;
  }
  for (const n of st.tree) {
    const li = document.createElement("li");
    li.style.paddingLeft = n.depth * 14 + "px";
    if (n.current) li.className = "current";
    li.textContent = `[${n.n}] ${n.label}`;
    li.title = "点击跳到此节点";
    li.onclick = () => runCommand("goto " + n.n);
    ul.appendChild(li);
  }
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
    li.textContent = `《${p.title}》`;
    li.title = "点击查看笔记与对应会话";
    li.onclick = () => openPaperTab(p);
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
    li.textContent = c.name;
    li.title = "点击查看概念详情";
    li.onclick = () => openConceptTab(c.name);
    ul.appendChild(li);
  }
}

function reloadNote() {
  noteFrame.src = "/api/note?format=html&t=" + Date.now();
}

// ===== 概念 / 论文详情标签页 =====

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
    const loadBtn = c.session_id
      ? `<button class="mini primary" data-load="${esc(c.session_id)}">加载该会话</button>`
      : "";
    pane.innerHTML = `<div class="detail">
      <h2>概念：${esc(c.name)}</h2>
      <dl>
        <dt>概念内容</dt><dd>${esc(c.definition || "（无）")}</dd>
        <dt>出自论文</dt><dd>${esc(c.paper_title || "（未知）")}</dd>
        <dt>所属会话</dt><dd>${esc(c.session_name || "（未找到）")} ${c.updated_at ? "· " + esc(fmtTime(c.updated_at)) : ""} ${loadBtn}</dd>
        <dt>当时的提问</dt><dd>${esc(c.question || "（未找到对应会话记录）")}</dd>
        <dt>当时的回答</dt><dd class="answer">${esc(c.answer || "（无）")}</dd>
      </dl>
    </div>`;
    pane.querySelectorAll("[data-load]").forEach((b) => (b.onclick = () => loadSession(b.dataset.load)));
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
      : '<p class="muted">找不到对应会话的笔记</p>';
    pane.innerHTML = `<div class="detail">
      <h2>《${esc(d.title)}》</h2>
      <p class="meta">会话：${esc(d.session_name || "（未知）")} ${d.updated_at ? "· " + esc(fmtTime(d.updated_at)) : ""} ${loadBtn}</p>
      ${noteBlock}
    </div>`;
    pane.querySelectorAll("[data-load]").forEach((b) => (b.onclick = () => loadSession(b.dataset.load)));
  } catch (e) {
    pane.innerHTML = `<div class="detail err">加载失败: ${esc(e)}</div>`;
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
  if (!list.length) {
    ul.innerHTML = '<li class="muted">（无）</li>';
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

async function loadSession(id) {
  const res = await fetch("/api/sessions/load", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ id }),
  });
  if (res.ok) {
    await refreshState();
    reloadNote();
  } else {
    appendConsole("❌ 加载失败: " + (await res.text()), "err");
  }
}

function showSessionMenu(x, y, s) {
  const menu = $("ctx-menu");
  menu.innerHTML = "";
  const item = (label, cls, fn) => {
    const d = document.createElement("div");
    d.textContent = label;
    if (cls) d.className = cls;
    d.onclick = () => { hideCtxMenu(); fn(); };
    menu.appendChild(d);
  };
  item(s.pinned ? "取消置顶" : "置顶会话", "", () => pinSession(s.id, !s.pinned));
  item("重命名", "", () => renameSession(s));
  item("删除会话", "danger", () => deleteSession(s));
  menu.style.left = x + "px";
  menu.style.top = y + "px";
  menu.classList.remove("hidden");
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

// ===== 命令面板 =====

const COMMANDS = [
  { name: "ask", usage: "ask <编号> <问题>", desc: "按编号追问，回答插入笔记" },
  { name: "check", usage: "check <编号> <想法>", desc: "核对想法，不写入笔记" },
  { name: "sum", usage: "sum", desc: "折叠当前子树为总结" },
  { name: "ingest", usage: "ingest [--text|--ocr] <路径>", desc: "导入论文生成笔记" },
  { name: "goto", usage: "goto <编号>", desc: "跳转对话节点" },
  { name: "del", usage: "del [--yes]", desc: "删除当前节点及子树" },
  { name: "undo", usage: "undo", desc: "撤销上一次删除" },
  { name: "export", usage: "export <md|mindmap|html> [文件]", desc: "导出笔记" },
  { name: "blocks", usage: "blocks", desc: "查看笔记结构" },
  { name: "note", usage: "note", desc: "打印笔记 Markdown" },
  { name: "tree", usage: "tree", desc: "查看对话轨迹" },
  { name: "stats", usage: "stats", desc: "查看用量与成本" },
  { name: "budget", usage: "budget <n>", desc: "设置 token 预算" },
  { name: "papers", usage: "papers", desc: "列出已读论文" },
  { name: "concepts", usage: "concepts", desc: "列出已学概念" },
  { name: "save", usage: "save [文件]", desc: "保存会话" },
  { name: "load", usage: "load <文件>", desc: "加载会话" },
  { name: "config", usage: "config show | set <k> <v>", desc: "查看/设置配置" },
  { name: "new", usage: "new", desc: "新建会话" },
  { name: "help", usage: "help", desc: "帮助" },
];

function hideSuggest() { suggestEl.classList.add("hidden"); }

function updateSuggest() {
  const v = cmdInput.value;
  if (!v.startsWith("/")) return hideSuggest();
  const body = v.slice(1);
  if (body.includes(" ")) return hideSuggest();
  const matches = COMMANDS.filter((c) => c.name.startsWith(body.toLowerCase()));
  if (!matches.length) return hideSuggest();
  suggestIndex = Math.min(suggestIndex, matches.length - 1);
  suggestEl.innerHTML = "";
  matches.forEach((c, i) => {
    const li = document.createElement("li");
    li.className = i === suggestIndex ? "active" : "";
    li.innerHTML = `<b>/${esc(c.name)}</b> <span class="usage">${esc(c.usage)}</span><span class="desc">${esc(c.desc)}</span>`;
    li.onmousedown = (e) => { e.preventDefault(); acceptSuggest(c.name); };
    suggestEl.appendChild(li);
  });
  suggestEl.classList.remove("hidden");
}

function acceptSuggest(name) {
  cmdInput.value = "/" + name + " ";
  hideSuggest();
  cmdInput.focus();
}

function submitInput() {
  const value = cmdInput.value.trim();
  if (!value) return;
  cmdHistory.push(value);
  histIdx = cmdHistory.length;
  const command = value.startsWith("/") ? value.slice(1).trim() : "ask " + value;
  cmdInput.value = "";
  hideSuggest();
  if (command) runCommand(command);
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

  // 命令输入：/ 触发候选、全键盘、历史
  cmdInput.addEventListener("input", () => { suggestIndex = 0; updateSuggest(); });
  cmdInput.addEventListener("keydown", (e) => {
    const suggestVisible = !suggestEl.classList.contains("hidden");
    const count = suggestEl.children.length;
    if (e.key === "ArrowDown") {
      if (suggestVisible) { e.preventDefault(); suggestIndex = (suggestIndex + 1) % count; updateSuggest(); }
    } else if (e.key === "ArrowUp") {
      if (suggestVisible) { e.preventDefault(); suggestIndex = (suggestIndex - 1 + count) % count; updateSuggest(); }
      else if (histIdx > 0) { e.preventDefault(); histIdx--; cmdInput.value = cmdHistory[histIdx] || ""; }
    } else if (e.key === "Tab") {
      if (suggestVisible) { e.preventDefault(); acceptSuggest(suggestEl.children[suggestIndex].querySelector("b").textContent.slice(1)); }
    } else if (e.key === "Enter") {
      if (suggestVisible) { e.preventDefault(); acceptSuggest(suggestEl.children[suggestIndex].querySelector("b").textContent.slice(1)); }
    } else if (e.key === "Escape") {
      hideSuggest();
    }
  });
  cmdInput.addEventListener("blur", () => setTimeout(hideSuggest, 120));

  $("cmd-form").onsubmit = (e) => { e.preventDefault(); submitInput(); };

  $("btn-stop").onclick = async () => {
    setProgress("已请求停止…");
    try { await fetch("/api/interrupt", { method: "POST" }); } catch (e) { console.error(e); }
  };

  $("btn-refresh").onclick = () => { refreshState(); reloadNote(); };
  $("btn-config").onclick = openConfig;
  $("btn-config-cancel").onclick = () => $("config-modal").classList.add("hidden");
  $("btn-config-save").onclick = saveConfig;

  $("btn-undo").onclick = () => runCommand("undo");

  $("btn-del").onclick = () => {
    const tree = (lastState && lastState.tree) || [];
    const cur = tree.find((n) => n.current);
    if (!cur) { alert("当前不在任何对话节点上"); return; }
    if (cur.depth === 0) { alert("根节点不可删除"); return; }
    const i = tree.findIndex((x) => x.n === cur.n);
    let desc = 0;
    for (let j = i + 1; j < tree.length && tree[j].depth > cur.depth; j++) desc++;
    if (!confirm(`删除节点「${cur.label}」及其 ${desc} 个子节点？\n可用「撤销」恢复。`)) return;
    runCommand("del --yes");
  };

  document.addEventListener("click", (e) => { if (!e.target.closest("#ctx-menu")) hideCtxMenu(); });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") hideCtxMenu(); });

  refreshState();
  setInterval(() => { if (!running) refreshState(); }, 15000);
});
