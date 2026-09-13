//! PDF/文本抽取：通过 Python 子进程调用外部工具，Rust 只做编排。
//!
//! - `extract_pages`：调 PyMuPDF（`python3 -c <内嵌脚本>`）抽文本层，按页用
//!   form-feed（\x0c）分隔，返回逐页文本向量——保留页边界便于后续按页引用。
//! - `ocr_extract`：扫描件处理。内嵌 Python 脚本逐页渲染成高分辨率 PNG 再喂
//!   tesseract。Rust 侧先预检 tesseract 是否安装、探测语言包（缺 chi_sim
//!   时降级 eng 并提示），失败时给出分平台的安装指引。
//!
//! 两者都用 `tokio::process` + `kill_on_drop`：用户中止（Ctrl-C / Web 停止）
//! 时立即杀掉子进程，不会卡在长时间 OCR 上。
//! 这样"允许调用其他语言库但主控在 Rust"：解析成功与否、文本流向都由 Rust 决定。

use std::path::Path;
use std::process::Stdio;

use anyhow::{anyhow, Result};
use tokio::process::Command;

use crate::logging;

const SCRIPT: &str = r#"
import sys, pymupdf
d = pymupdf.open(sys.argv[1])
out = []
for p in d:
    out.append(p.get_text())
sys.stdout.write("\f".join(out))
"#;

/// 运行子进程并收集输出；被打断时 abort 等待任务 → Child 被 drop →
/// `kill_on_drop` 杀掉子进程本身。
async fn run_killable(mut cmd: Command, what: &str) -> Result<std::process::Output> {
    let child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow!("启动 {what} 失败: {e}"))?;
    let mut handle = tokio::spawn(async move { child.wait_with_output().await });
    tokio::select! {
        r = &mut handle => r
            .map_err(|e| anyhow!("{what} 等待任务异常: {e}"))?
            .map_err(|e| anyhow!("等待 {what} 结束失败: {e}")),
        _ = crate::interrupt::wait() => {
            handle.abort();
            Err(anyhow!(crate::llm::Interrupted))
        }
    }
}

/// 用 PyMuPDF（Python 子进程）抽取 PDF 全文，按页用 form-feed 分隔。
/// 需要环境里装了 pymupdf：`pip install pymupdf`。
pub async fn extract_pages(path: &Path) -> Result<Vec<String>> {
    let t0 = std::time::Instant::now();
    let mut cmd = Command::new("python3");
    cmd.arg("-c").arg(SCRIPT).arg(path);
    let out = run_killable(cmd, "python3（需安装 pymupdf）").await?;
    if !out.status.success() {
        let e = String::from_utf8_lossy(&out.stderr);
        logging::error(format!("PDF 解析失败 {}: {e}", path.display()));
        return Err(anyhow!(
            "PDF 解析失败: {e}\n提示：确认已 `pip install pymupdf`，且文件是有效 PDF。"
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let pages: Vec<String> = text
        .split('\u{0C}')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if pages.is_empty() {
        return Err(anyhow!(
            "PDF 没有解析出任何文本（可能是扫描件/纯图片）。\
             可改用 `ingest --ocr <pdf>`（需 tesseract）或 `ingest --text <txt>`（自备文本）。"
        ));
    }
    logging::info(format!(
        "PDF 解析完成：{} 页，用时 {:.1}s（{}）",
        pages.len(),
        t0.elapsed().as_secs_f64(),
        path.display()
    ));
    Ok(pages)
}

/// 用 Tesseract OCR 对 PDF 逐页做文字识别。
/// 需要安装：Debian/Ubuntu `sudo apt install tesseract-ocr tesseract-ocr-chi-sim`；
/// macOS `brew install tesseract tesseract-lang`。
/// 流程：用 PyMuPDF 把每页渲染成图片 → tesseract 识别。
const OCR_SCRIPT: &str = r#"
import sys, pymupdf, subprocess, tempfile, os
d = pymupdf.open(sys.argv[1])
lang = sys.argv[2]
out = []
for i, page in enumerate(d):
    # 渲染为高分辨率图片
    mat = pymupdf.Matrix(3, 3)
    pix = page.get_pixmap(matrix=mat)
    with tempfile.NamedTemporaryFile(suffix=".png", delete=False) as f:
        pix.save(f.name)
        # tesseract 识别
        r = subprocess.run(["tesseract", f.name, "stdout", "-l", lang],
                           capture_output=True, text=True)
        if r.returncode != 0:
            sys.stderr.write(r.stderr)
            sys.exit(1)
        out.append(r.stdout.strip())
        os.unlink(f.name)
sys.stdout.write("\f".join(out))
"#;

pub async fn ocr_extract(path: &Path) -> Result<String> {
    // 预检 1：tesseract 是否安装
    match Command::new("tesseract").arg("--version").output().await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(anyhow!(
                "未安装 tesseract，无法 OCR。安装方式：\n  \
                 Debian/Ubuntu: sudo apt install tesseract-ocr tesseract-ocr-chi-sim\n  \
                 macOS:         brew install tesseract tesseract-lang\n  \
                 或改用 `ingest <pdf>`（文本层提取）或 `ingest --text <txt>`（自备文本）"
            ));
        }
        Err(e) => return Err(anyhow!("无法运行 tesseract: {e}")),
    }

    // 预检 2：探测可用语言包，缺 chi_sim 时降级 eng
    let lang = if let Ok(out) = Command::new("tesseract").arg("--list-langs").output().await {
        let langs = String::from_utf8_lossy(&out.stdout);
        if langs.contains("chi_sim") {
            "chi_sim+eng".to_string()
        } else {
            let msg = "未检测到中文语言包（chi_sim），OCR 将只用英文（eng）。\
                       中文论文效果会差，建议安装：sudo apt install tesseract-ocr-chi-sim";
            logging::warn(msg);
            eprintln!("⚠️  {msg}");
            "eng".to_string()
        }
    } else {
        "eng".to_string() // --list-langs 失败时保守用 eng
    };

    let t0 = std::time::Instant::now();
    let mut cmd = Command::new("python3");
    cmd.arg("-c").arg(OCR_SCRIPT).arg(path).arg(&lang);
    logging::info(format!("OCR 开始（lang={lang}）：{}", path.display()));
    let out = run_killable(cmd, "OCR（python3/tesseract）").await?;
    if !out.status.success() {
        let e = String::from_utf8_lossy(&out.stderr);
        logging::error(format!("OCR 失败 {}: {e}", path.display()));
        if e.contains("No such file or directory") || e.contains("FileNotFoundError") {
            return Err(anyhow!("tesseract 运行失败（可能已被卸载）: {e}"));
        }
        return Err(anyhow!("OCR 识别失败: {e}"));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        return Err(anyhow!("OCR 未识别出任何文本"));
    }
    logging::info(format!(
        "OCR 完成：{} 字符，用时 {:.1}s",
        text.chars().count(),
        t0.elapsed().as_secs_f64()
    ));
    Ok(text)
}
