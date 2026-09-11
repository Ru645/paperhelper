// PaperHelper Web 前端：原生 JS，无构建。
// 与后端约定：POST /api/run 返回 SSE（event: stdout/stderr/token/progress/progress_done/done/error，
// data 为 JSON 字符串）。其余接口为普通 JSON。

const $ = (id) => document.getElementById(id);

const consoleEl = $("console");
const noteFrame = $("note-frame");

let running = false;
let streamSpan = null; // 当前流式 token 的容器

// ===== 输出渲染 =====

function endStream() {
  streamSpan = null;
}

// 去掉终端 ANSI 颜色转义（CLI 输出带 owo-colors，浏览器里需清除）
function stripAnsi(s) {
  return String(s).replace(/\x1b\[[0-9;]*m/g, "");
}

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

function setProgress(msg) {
  $("progress").textContent = msg || "";
}

function setRunning(v) {
  running = v;
  $("btn-send").disabled = v;
  $("btn-stop").disabled = !v;
  if (!v) setProgress("");
}

// ===== SSE 解析 =====

function handleFrame(frame) {
  let name = "message";
  let data = "";
  for (const line of frame.split("\n")) {
    if (line.startsWith("event:")) name = line.slice(6).trim();
    else if (line.startsWith("data:")) data += line.slice(5).trim();
  }
  let text = data;
  try {
    text = JSON.parse(data);
  } catch (e) {
    /* 保留原始文本 */
  }
  switch (name) {
    case "stdout": appendConsole(text); break;
    case "stderr": appendConsole(text, "err"); break;
    case "token": appendToken(text); break;
    case "progress": setProgress(text); break;
    case "progress_done": setProgress(""); break;
    case "error": appendConsole("❌ " + text, "err"); break;
    case "done": appendConsole("✓ 完成", "ok"); break;
    default: if (text) appendConsole(text);
  }
}

async function runCommand(command, exportPath) {
  if (running) return;
  if (!command || !command.trim()) return;
  setRunning(true);
  appendConsole("> " + command, "ok");
  switchTab("console");
  try {
    const res = await fetch("/api/run", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ command, export: exportPath || null }),
    });
    if (!res.ok || !res.body) {
      appendConsole("❌ 请求失败: HTTP " + res.status, "err");
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
    renderStats(st);
    renderTree(st);
    renderPapers(st);
    renderConcepts(st);
  } catch (e) {
    console.error(e);
  }
  await refreshSessions();
}

function renderStats(st) {
  const s = st.stats.session;
  const g = st.stats.global;
  const fmt = (n) => n.toLocaleString();
  let budget = "预算：未设置";
  if (st.budget > 0) {
    const pct = ((st.used_total / st.budget) * 100).toFixed(1);
    budget = `<span class="${pct >= 80 ? "warn" : ""}">预算：${fmt(st.used_total)} / ${fmt(st.budget)} (${pct}%)</span>`;
  }
  $("stats").innerHTML =
    `模型 <b>${esc(st.model)}</b>` +
    ` ｜ 本次 <b>${fmt(s.input)}→${fmt(s.output)}</b> tok · $${s.cost.toFixed(4)}` +
    ` ｜ 累计 <b>${fmt(g.input)}→${fmt(g.output)}</b> tok · $${g.cost.toFixed(4)}` +
    ` ｜ ${budget}`;
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
    const tok = n.input + n.output > 0 ? `<span class="tok">${n.input}→${n.output}tok</span>` : "";
    li.innerHTML = `[${n.n}] ${esc(n.label)}${tok}`;
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
  ul.innerHTML = st.papers.map((p) => `<li>《${esc(p.title)}》</li>`).join("");
}

function renderConcepts(st) {
  const ul = $("concepts");
  if (!st.concepts || st.concepts.length === 0) {
    ul.innerHTML = '<li class="muted">（无）</li>';
    return;
  }
  ul.innerHTML = st.concepts
    .map((c) => `<li title="${esc(c.definition)}">${esc(c.name)} <span class="muted">· ${esc(c.paper)}</span></li>`)
    .join("");
}

async function refreshSessions() {
  try {
    const data = await (await fetch("/api/sessions")).json();
    const sel = $("sessions");
    const prev = sel.value;
    sel.innerHTML = "";
    if (!data.sessions || data.sessions.length === 0) {
      sel.innerHTML = '<option value="">（无已保存会话）</option>';
      return;
    }
    for (const s of data.sessions) {
      const opt = document.createElement("option");
      opt.value = s.id;
      opt.textContent = `${s.id}  ${s.name}`;
      sel.appendChild(opt);
    }
    if (prev) sel.value = prev;
  } catch (e) {
    console.error(e);
  }
}

function reloadNote() {
  noteFrame.src = "/api/note?format=html&t=" + Date.now();
}

function switchTab(name) {
  document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t.dataset.tab === name));
  $("pane-note").classList.toggle("active", name === "note");
  $("pane-console").classList.toggle("active", name === "console");
}

function esc(s) {
  return String(s == null ? "" : s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
}

// ===== 命令表单 =====

function updateFormForKind() {
  const kind = $("cmd-kind").value;
  const arg = $("cmd-arg");
  const q = $("cmd-question");
  arg.style.display = "";
  q.style.display = "";
  switch (kind) {
    case "ask":
    case "check":
      arg.placeholder = "编号（可空，如 3.2）";
      q.placeholder = "问题内容";
      break;
    case "ingest":
      arg.placeholder = "服务器上的文件路径，如 samples/论文.pdf";
      q.placeholder = "导出文件名（可空，默认 笔记_xxx.md）";
      break;
    case "goto":
      arg.placeholder = "节点编号（见左侧对话轨迹）";
      q.style.display = "none";
      break;
    case "export":
      arg.placeholder = "md|mindmap|html [文件名]";
      q.style.display = "none";
      break;
    case "sum":
      arg.style.display = "none";
      q.style.display = "none";
      break;
    case "raw":
      arg.placeholder = "完整命令，如 blocks / stats / concepts";
      q.style.display = "none";
      break;
  }
}

function buildCommand() {
  const kind = $("cmd-kind").value;
  const arg = $("cmd-arg").value.trim();
  const q = $("cmd-question").value.trim();
  let command = "";
  let exportPath = null;
  switch (kind) {
    case "ask":
      command = arg ? `ask ${arg} ${q}` : `ask ${q}`;
      break;
    case "check":
      command = arg ? `check ${arg} ${q}` : `check ${q}`;
      break;
    case "sum":
      command = "sum";
      break;
    case "ingest":
      command = "ingest " + arg;
      exportPath = q || null;
      break;
    case "goto":
      command = "goto " + arg;
      break;
    case "export":
      command = "export " + arg;
      break;
    case "raw":
      command = arg;
      break;
  }
  return { command, exportPath };
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
  if (!res.ok) {
    const txt = await res.text();
    throw new Error(`${key}: ${txt}`);
  }
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
  document.querySelectorAll(".tab").forEach((t) => (t.onclick = () => switchTab(t.dataset.tab)));

  $("cmd-kind").onchange = updateFormForKind;
  updateFormForKind();

  $("cmd-form").onsubmit = (e) => {
    e.preventDefault();
    const { command, exportPath } = buildCommand();
    if (!command || !command.trim()) return;
    runCommand(command, exportPath);
  };

  $("btn-stop").onclick = async () => {
    setProgress("已请求停止…");
    try {
      await fetch("/api/interrupt", { method: "POST" });
    } catch (e) {
      console.error(e);
    }
  };

  $("btn-refresh").onclick = () => {
    refreshState();
    reloadNote();
  };
  $("btn-config").onclick = openConfig;
  $("btn-config-cancel").onclick = () => $("config-modal").classList.add("hidden");
  $("btn-config-save").onclick = saveConfig;

  $("btn-load").onclick = async () => {
    const id = $("sessions").value;
    if (!id) return;
    const res = await fetch("/api/sessions/load", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id }),
    });
    if (res.ok) {
      appendConsole("✓ 已加载会话 " + id, "ok");
      await refreshState();
      reloadNote();
    } else {
      appendConsole("❌ 加载失败: " + (await res.text()), "err");
    }
  };

  $("btn-save").onclick = async () => {
    setProgress("保存会话中…");
    const res = await fetch("/api/sessions/save", { method: "POST" });
    setProgress("");
    if (res.ok) {
      const j = await res.json();
      appendConsole("✓ 会话已保存：" + (j.name || "") + "（" + (j.id || "") + "）", "ok");
      await refreshSessions();
    } else {
      appendConsole("❌ 保存失败: " + (await res.text()), "err");
    }
  };

  refreshState();
  setInterval(() => {
    // 空闲时轻量轮询统计（任务运行时由 SSE 更新）
    if (!running) refreshState();
  }, 15000);
});
