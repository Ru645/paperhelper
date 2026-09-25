// PDF 阅读器适配层（按需加载）。
//
// 后端 `GET /api/pdf/file` 只返回「当前会话」对应的 PDF 原件；渲染全部在前端完成，
// 使用 vendored PDF.js（Apache-2.0，/vendor/pdfjs/）。本脚本由 app.js 在用户切到
// 「原文」时动态注入，复用 app.js 的批注弹窗：
//   - 选中文字 → 复用 #sel-btn → openAnnotationCreate({ pdf, quote, image })
//   - 「提问本页」→ openAnnotationCreate({ pdf, quote, image(kind=page) })
//   - 页面上按 page/rects 重绘高亮；点高亮 → openAnnotationView(id, { rect })
(function () {
  "use strict";

  const PAD = 24;               // 页面两侧留白（贴合宽度用）
  const MAX_IMG_WIDTH = 1600;   // 捕获图片最大宽度，控制 data URL 体积
  const RENDER_MARGIN = "800px"; // 预渲染视口外一屏

  let lib = null;               // pdf.js 模块
  let doc = null;               // PDFDocumentProxy
  let pages = [];               // [{ num, wrap, canvas, textLayer }]
  let pageCount = 0;
  let current = 1;
  let zoom = "fit";             // "fit" | 数字字符串
  let baseWidth = 0;            // 第 1 页 scale=1 的 CSS 宽度
  let observer = null;
  let docPromise = null;        // 当前加载任务（去重）

  const $ = (id) => document.getElementById(id);
  const pagesEl = () => $("pdf-pages");
  const setStatus = (t) => { const el = $("pdf-status"); if (el) el.textContent = t || ""; };

  async function loadLib() {
    if (lib) return lib;
    const mod = await import("/vendor/pdfjs/pdf.min.mjs");
    mod.GlobalWorkerOptions.workerSrc = "/vendor/pdfjs/pdf.worker.min.mjs";
    lib = mod;
    return lib;
  }

  function reset() {
    docPromise = null;
    if (doc) { try { doc.destroy(); } catch (e) { /* 忽略 */ } doc = null; }
    if (observer) { observer.disconnect(); observer = null; }
    pages = [];
    pageCount = 0;
    current = 1;
    baseWidth = 0;
    const c = pagesEl();
    if (c) c.innerHTML = "";
    const toc = $("pdf-toc");
    if (toc) { toc.innerHTML = ""; toc.classList.add("hidden"); }
    const tocBtn = $("pdf-toc-btn");
    if (tocBtn) tocBtn.classList.add("hidden");
    setStatus("");
  }

  async function open() {
    if (docPromise) return docPromise;
    const view = $("pdf-view");
    if (view) view.classList.remove("hidden");
    docPromise = (async () => {
      setStatus("正在加载 PDF…");
      try {
        await loadLib();
        doc = await lib.getDocument({
          url: "/api/pdf/file",
          cMapUrl: "/vendor/pdfjs/cmaps/",
          cMapPacked: true,
          standardFontDataUrl: "/vendor/pdfjs/standard_fonts/",
          wasmUrl: "/vendor/pdfjs/wasm/",
        }).promise;
      } catch (e) {
        setStatus("PDF 加载失败：" + (e && e.message ? e.message : e));
        docPromise = null;
        return;
      }
      pageCount = doc.numPages;
      try {
        const p1 = await doc.getPage(1);
        baseWidth = p1.getViewport({ scale: 1 }).width;
      } catch (e) { baseWidth = 612; }
      buildPages();
      loadToc();
      setStatus("");
    })();
    return docPromise;
  }

  function currentScale() {
    const c = pagesEl();
    if (zoom === "fit") {
      const avail = (c ? c.clientWidth : 800) - PAD * 2;
      return Math.max(0.2, avail / (baseWidth || 612));
    }
    return Number(zoom) || 1;
  }

  function buildPages() {
    const c = pagesEl();
    if (!c) return;
    if (observer) { observer.disconnect(); observer = null; }
    c.innerHTML = "";
    pages = [];
    const scale = currentScale();
    const estW = (baseWidth || 612) * scale;
    const estH = estW * 1.414;
    for (let i = 0; i < pageCount; i++) {
      const wrap = document.createElement("div");
      wrap.className = "pdf-page";
      wrap.dataset.page = String(i + 1);
      wrap.style.width = estW + "px";
      wrap.style.height = estH + "px";
      const canvas = document.createElement("canvas");
      const textLayer = document.createElement("div");
      textLayer.className = "pdf-text-layer";
      wrap.appendChild(canvas);
      wrap.appendChild(textLayer);
      c.appendChild(wrap);
      pages.push({ num: i + 1, wrap, canvas, textLayer, rendered: false, rendering: false });
    }
    observer = new IntersectionObserver((entries) => {
      for (const en of entries) {
        if (!en.isIntersecting) continue;
        const idx = Number(en.target.dataset.page) - 1;
        renderPage(idx);
      }
    }, { root: c, rootMargin: RENDER_MARGIN });
    for (const p of pages) observer.observe(p.wrap);
    applyHighlights();
    jumpTo(current, false);
  }

  async function renderPage(idx) {
    const pc = pages[idx];
    if (!pc || pc.rendered || pc.rendering || !doc) return;
    pc.rendering = true;
    try {
      const page = await doc.getPage(pc.num);
      const scale = currentScale();
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const vp = page.getViewport({ scale });
      const canvas = pc.canvas;
      canvas.width = Math.floor(vp.width * dpr);
      canvas.height = Math.floor(vp.height * dpr);
      canvas.style.width = vp.width + "px";
      canvas.style.height = vp.height + "px";
      pc.wrap.style.width = vp.width + "px";
      pc.wrap.style.height = vp.height + "px";
      const ctx = canvas.getContext("2d");
      ctx.save();
      ctx.fillStyle = "#fff";
      ctx.fillRect(0, 0, canvas.width, canvas.height);
      await page.render({ canvasContext: ctx, viewport: vp, transform: dpr !== 1 ? [dpr, 0, 0, dpr, 0, 0] : null }).promise;
      ctx.restore();

      const tl = pc.textLayer;
      tl.innerHTML = "";
      tl.style.setProperty("--total-scale-factor", String(vp.scale));
      tl.style.setProperty("--scale-round-x", "1px");
      tl.style.setProperty("--scale-round-y", "1px");
      tl.style.width = vp.width + "px";
      tl.style.height = vp.height + "px";
      if (lib.TextLayer) {
        const layer = new lib.TextLayer({
          textContentSource: page.streamTextContent(),
          container: tl,
          viewport: vp,
        });
        await layer.render();
      } else {
        const tc = await page.getTextContent();
        const layer = new lib.TextLayer({ textContentSource: tc, container: tl, viewport: vp });
        await layer.render();
      }
      pc.rendered = true;
      applyHighlights();
    } catch (e) {
      console.error("渲染 PDF 页面失败", e);
    } finally {
      pc.rendering = false;
    }
  }

  // ---- 目录 ----
  async function loadToc() {
    const btn = $("pdf-toc-btn");
    const box = $("pdf-toc");
    if (!btn || !box || !doc) return;
    let outline = null;
    try { outline = await doc.getOutline(); } catch (e) { outline = null; }
    if (!outline || !outline.length) { btn.classList.add("hidden"); return; }
    btn.classList.remove("hidden");
    box.innerHTML = "";
    for (const item of outline) {
      const b = document.createElement("button");
      b.className = "pdf-toc-item";
      b.textContent = item.title || "(未命名)";
      b.style.paddingLeft = (6 + (item.depth || 0) * 14) + "px";
      b.onclick = async () => {
        try {
          const dest = typeof item.dest === "string" ? await doc.getDestination(item.dest) : item.dest;
          if (dest && dest[0]) {
            const ref = dest[0];
            const pi = typeof ref === "object" ? (await doc.getPageIndex(ref)) : 1;
            jumpTo(pi + (typeof ref === "object" ? 1 : 0), true);
          }
        } catch (e) { /* 目的页解析失败则忽略 */ }
      };
      box.appendChild(b);
    }
  }

  // ---- 翻页 / 缩放 ----
  function jumpTo(num, scroll) {
    current = Math.max(1, Math.min(pageCount || 1, num || 1));
    const inp = $("pdf-page-input");
    if (inp) inp.value = String(current);
    const tot = $("pdf-page-total");
    if (tot) tot.textContent = "/ " + (pageCount || 0);
    if (!scroll) {
      const pc = pages[current - 1];
      const c = pagesEl();
      if (pc && c) c.scrollTop = pc.wrap.offsetTop - 8;
    } else {
      const pc = pages[current - 1];
      if (pc) pc.wrap.scrollIntoView({ block: "start" });
    }
  }

  function rebuildForZoom(newZoom) {
    zoom = newZoom;
    // 记住当前可见页
    syncCurrentFromScroll();
    const keep = current;
    buildPages();
    current = keep;
    jumpTo(current, false);
  }

  function syncCurrentFromScroll() {
    const c = pagesEl();
    if (!c || !pages.length) return;
    const y = c.scrollTop;
    for (const p of pages) {
      if (p.wrap.offsetTop + p.wrap.offsetHeight / 2 > y) { current = p.num; break; }
    }
    const inp = $("pdf-page-input");
    if (inp) inp.value = String(current);
  }

  // ---- 选中文字 → 提问 ----
  function closestPage(node) {
    let el = node && node.nodeType === Node.TEXT_NODE ? node.parentElement : node;
    while (el && el !== document) {
      if (el.classList && el.classList.contains("pdf-page")) return el;
      el = el.parentElement;
    }
    return null;
  }

  function normalizedRects(pageEl, range) {
    const pr = pageEl.getBoundingClientRect();
    const out = [];
    for (const r of range.getClientRects()) {
      if (r.width < 1 || r.height < 1) continue;
      const clamp = (v) => Math.max(0, Math.min(1, v));
      out.push([
        clamp((r.left - pr.left) / pr.width),
        clamp((r.top - pr.top) / pr.height),
        clamp((r.right - pr.left) / pr.width),
        clamp((r.bottom - pr.top) / pr.height),
      ]);
    }
    return out;
  }

  function unionBox(rects) {
    if (!rects.length) return [0, 0, 1, 1];
    let x0 = 1, y0 = 1, x1 = 0, y1 = 0;
    for (const r of rects) {
      x0 = Math.min(x0, r[0]); y0 = Math.min(y0, r[1]);
      x1 = Math.max(x1, r[2]); y1 = Math.max(y1, r[3]);
    }
    return [x0, y0, x1, y1];
  }

  /// 从该页 canvas 截取归一化区域（带少量留白），返回 data URL；整页传 null。
  function captureImage(idx, box) {
    const pc = pages[idx];
    if (!pc || !pc.canvas || !pc.canvas.width) return null;
    const src = pc.canvas;
    let sx = 0, sy = 0, sw = src.width, sh = src.height;
    if (box) {
      const mx = (box[2] - box[0]) * 0.02, my = (box[3] - box[1]) * 0.02;
      sx = Math.max(0, (box[0] - mx) * src.width);
      sy = Math.max(0, (box[1] - my) * src.height);
      sw = Math.min(src.width - sx, (box[2] - box[0] + mx * 2) * src.width);
      sh = Math.min(src.height - sy, (box[3] - box[1] + my * 2) * src.height);
    }
    if (sw < 1 || sh < 1) return null;
    const ratio = Math.min(1, MAX_IMG_WIDTH / sw);
    const out = document.createElement("canvas");
    out.width = Math.max(1, Math.round(sw * ratio));
    out.height = Math.max(1, Math.round(sh * ratio));
    const ctx = out.getContext("2d");
    ctx.fillStyle = "#fff";
    ctx.fillRect(0, 0, out.width, out.height);
    ctx.drawImage(src, sx, sy, sw, sh, 0, 0, out.width, out.height);
    try { return out.toDataURL("image/png"); } catch (e) { return null; }
  }

  function showSelBtn() {
    const btn = $("sel-btn");
    if (!btn) return;
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed || !sel.rangeCount) return btn.classList.add("hidden");
    const range = sel.getRangeAt(0);
    const pageEl = closestPage(range.startContainer);
    if (!pageEl) return btn.classList.add("hidden");
    const idx = pages.findIndex((p) => p.wrap === pageEl);
    if (idx < 0) return btn.classList.add("hidden");
    const quote = sel.toString().replace(/\s+/g, " ").trim();
    if (!quote) return btn.classList.add("hidden");
    const rects = normalizedRects(pageEl, range);
    const r = range.getBoundingClientRect();
    btn.classList.remove("hidden");
    btn.style.left = Math.min(window.innerWidth - 60, Math.max(8, r.left)) + "px";
    btn.style.top = Math.min(window.innerHeight - 40, Math.max(8, r.bottom + 6)) + "px";
    btn.onmousedown = (e) => e.preventDefault();
    btn.onclick = () => {
      btn.classList.add("hidden");
      const image = captureImage(idx, unionBox(rects));
      openAnnotationCreate({
        pdf: { page: pages[idx].num, rects, kind: "text" },
        quote,
        context: quote,
        rect: r,
        image,
      });
    };
  }

  async function askPage() {
    if (!doc) return;
    syncCurrentFromScroll();
    const idx = current - 1;
    const pc = pages[idx];
    if (!pc) return;
    const image = captureImage(idx, null);
    const r = pc.wrap.getBoundingClientRect();
    openAnnotationCreate({
      pdf: { page: pc.num, rects: [], kind: "page" },
      quote: "第 " + pc.num + " 页",
      context: "第 " + pc.num + " 页整页内容",
      rect: { left: r.left + r.width / 2 - 20, bottom: r.top + 40 },
      image,
    });
  }

  // ---- 高亮（来自 /api/annotations 里带 page 的批注） ----
  function applyHighlights() {
    for (const pc of pages) {
      pc.textLayer.querySelectorAll(".pdf-hl").forEach((el) => el.remove());
    }
    const list = (typeof annotationsCache !== "undefined" && annotationsCache) || [];
    const byPage = new Map();
    for (const ann of list) {
      if (ann.page == null) continue;
      if (!byPage.has(ann.page)) byPage.set(ann.page, []);
      byPage.get(ann.page).push(ann);
    }
    for (const pc of pages) {
      const anns = byPage.get(pc.num);
      if (!anns) continue;
      for (const ann of anns) {
        for (const rect of (ann.rects || [])) {
          const el = document.createElement("div");
          el.className = "pdf-hl";
          el.dataset.annId = ann.id;
          el.style.left = (rect[0] * 100).toFixed(2) + "%";
          el.style.top = (rect[1] * 100).toFixed(2) + "%";
          el.style.width = ((rect[2] - rect[0]) * 100).toFixed(2) + "%";
          el.style.height = ((rect[3] - rect[1]) * 100).toFixed(2) + "%";
          el.style.cursor = "pointer";
          el.style.position = "absolute";
          el.style.background = "rgba(255,214,0,.45)";
          el.style.borderRadius = "2px";
          el.onmousedown = (e) => e.preventDefault();
          el.onclick = (e) => {
            e.stopPropagation();
            const r = el.getBoundingClientRect();
            openAnnotationView(ann.id, { scroll: true, rect: { left: r.left, bottom: r.bottom } });
          };
          pc.textLayer.appendChild(el);
        }
        // 整页批注没有 rects：在页眉画一个标记，便于回看/再次打开
        if ((ann.kind === "page" || !ann.rects || !ann.rects.length) && !ann.rects.length) {
          const badge = document.createElement("button");
          badge.className = "pdf-hl pdf-page-badge";
          badge.dataset.annId = ann.id;
          badge.textContent = "批注";
          badge.title = ann.quote || "";
          badge.style.cssText = "position:absolute;top:4px;right:4px;font-size:11px;padding:1px 6px;border:1px solid #d9a800;background:#fff3bf;color:#7a5c00;border-radius:4px;cursor:pointer;z-index:2";
          badge.onclick = (e) => {
            e.stopPropagation();
            const r = badge.getBoundingClientRect();
            openAnnotationView(ann.id, { scroll: false, rect: { left: r.left, bottom: r.bottom + 4 } });
          };
          pc.textLayer.appendChild(badge);
        }
      }
    }
  }

  // ---- 工具栏绑定（只绑一次） ----
  let bound = false;
  function bindUi() {
    if (bound) return;
    bound = true;
    const c = pagesEl();
    if (c) {
      c.addEventListener("mouseup", () => setTimeout(showSelBtn, 0));
      c.addEventListener("scroll", () => { syncCurrentFromScroll(); }, { passive: true });
    }
    const prev = $("pdf-prev"), next = $("pdf-next");
    if (prev) prev.onclick = () => jumpTo(current - 1, true);
    if (next) next.onclick = () => jumpTo(current + 1, true);
    const inp = $("pdf-page-input");
    if (inp) inp.onchange = () => { const n = parseInt(inp.value, 10); if (n) jumpTo(n, true); };
    const zl = $("pdf-zoom");
    if (zl) zl.onchange = () => rebuildForZoom(zl.value);
    const tb = $("pdf-toc-btn");
    if (tb) tb.onclick = () => { const box = $("pdf-toc"); if (box) box.classList.toggle("hidden"); };
    const ap = $("pdf-ask-page");
    if (ap) ap.onclick = askPage;
    document.addEventListener("mouseup", (e) => {
      const btn = $("sel-btn");
      if (!btn || btn.classList.contains("hidden")) return;
      if (e.target === btn) return;
      // 点到别处才收起（选区在 PDF 内时不收起，交给 showSelBtn 判断）
      if (!closestPage(e.target)) btn.classList.add("hidden");
    });
  }

  window.phPdf = {
    open: () => { bindUi(); return open(); },
    reset,
    close: () => {},
    applyHighlights,
  };
  window.phPdfRefresh = applyHighlights;
  window.phPdfReset = reset;
})();
