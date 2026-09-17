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
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::process::Command;

use crate::logging;

/// 探测到的 Python 命令缓存：`(程序, 前置参数)`，如 `("py", ["-3"])`。
/// 首次使用时按候选顺序探测（跑 `--version`），成功后缓存；安装依赖后可重置。
static PY_CACHE: std::sync::LazyLock<tokio::sync::RwLock<Option<(String, Vec<String>)>>> =
    std::sync::LazyLock::new(|| tokio::sync::RwLock::new(None));

/// Python 候选顺序：`PAPERHELPER_PYTHON`（显式指定，不校验）→ 打包内置
/// （exe 同级 `python/python.exe` 或 `python/bin/python3`）→ Windows `py -3` →
/// `python` → `python3`。最后两个只作兜底（无法同步判断 PATH 是否存在）。
fn python_candidates(
    env: Option<&str>,
    exe_dir: Option<&Path>,
    windows: bool,
) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(p) = env.map(str::trim).filter(|s| !s.is_empty()) {
        out.push((p.to_string(), Vec::new()));
    }
    if let Some(dir) = exe_dir {
        let embedded = if windows {
            dir.join("python").join("python.exe")
        } else {
            dir.join("python").join("bin").join("python3")
        };
        if embedded.is_file() {
            out.push((embedded.to_string_lossy().to_string(), Vec::new()));
        }
    }
    if windows {
        out.push(("py".into(), vec!["-3".into()]));
        out.push(("python".into(), Vec::new()));
        out.push(("python3".into(), Vec::new()));
    } else {
        out.push(("python3".into(), Vec::new()));
        out.push(("python".into(), Vec::new()));
    }
    // 去重（保持顺序）
    let mut seen = std::collections::HashSet::new();
    out.retain(|(p, a)| seen.insert((p.clone(), a.join(" "))));
    out
}

/// 跑 `--version` 判断候选是否可用（3 秒超时，找不到即 false）。
async fn probe_ok(program: &str, args: &[String]) -> bool {
    let mut cmd = Command::new(program);
    cmd.args(args).arg("--version");
    let fut = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(tokio::time::timeout(Duration::from_secs(3), fut).await, Ok(Ok(s)) if s.success())
}

/// 当前可用的 Python 命令（程序 + 前置参数）。探测结果缓存，`reset_python_probe` 可清空。
pub async fn python_command() -> (String, Vec<String>) {
    if let Some(c) = PY_CACHE.read().await.clone() {
        return c;
    }
    let env = std::env::var("PAPERHELPER_PYTHON").ok();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let candidates = python_candidates(env.as_deref(), exe_dir.as_deref(), cfg!(windows));
    let mut found: Option<(String, Vec<String>)> = None;
    for (p, a) in &candidates {
        if probe_ok(p, a).await {
            found = Some((p.clone(), a.clone()));
            break;
        }
    }
    // 全部失败时用最后的兜底候选，让报错信息里能看到尝试过的命令
    let result = found.unwrap_or_else(|| candidates.last().cloned().unwrap_or(("python3".into(), vec![])));
    if PY_CACHE.read().await.is_none() {
        *PY_CACHE.write().await = Some(result.clone());
        logging::info(format!(
            "Python 探测：{} {}",
            result.0,
            if result.1.is_empty() { String::new() } else { result.1.join(" ") }
        ));
    }
    result
}

/// 安装/卸载依赖后重置探测缓存（下次调用重新探测）。
pub async fn reset_python_probe() {
    *PY_CACHE.write().await = None;
}

/// Python / PyMuPDF 环境检测结果（向导「环境检查」与 `/api/deps` 用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DepsStatus {
    /// 探测到的 Python 命令（未找到则为兜底候选）
    pub python_cmd: String,
    /// Python 版本（无法运行时为 None）
    pub python_version: Option<String>,
    /// PyMuPDF 版本（未安装为 None）
    pub pymupdf_version: Option<String>,
    /// 是否可以直接解析 PDF
    pub ready: bool,
    /// 是否来自打包内置 Python（exe 同级 python/ 目录）
    pub embedded: bool,
}

const STATUS_SCRIPT: &str = r#"
import sys
print("PY=" + sys.version.split()[0])
try:
    import pymupdf
    print("PYMUPDF=" + str(getattr(pymupdf, "__version__", "unknown")))
except Exception:
    print("PYMUPDF=missing")
"#;

/// 检测 Python 与 PyMuPDF 是否可用（供首启向导展示/一键安装）。
pub async fn deps_status() -> DepsStatus {
    let (py, args) = python_command().await;
    let exe_embedded = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .map(|d| {
            d.join("python").join("python.exe").is_file() || d.join("python").join("bin").join("python3").is_file()
        })
        .unwrap_or(false);
    let mut cmd = Command::new(&py);
    cmd.args(&args)
        .arg("-c")
        .arg(STATUS_SCRIPT)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let mut python_version = None;
    let mut pymupdf_version = None;
    // 独立超时，不接全局 interrupt（状态查询不应被上一次中止信号影响）
    if let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(10), cmd.output()).await {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("PY=") {
                    python_version = Some(v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("PYMUPDF=") {
                    if v.trim() != "missing" {
                        pymupdf_version = Some(v.trim().to_string());
                    }
                }
            }
        }
    }
    DepsStatus {
        python_cmd: if args.is_empty() { py.clone() } else { format!("{py} {}", args.join(" ")) },
        ready: python_version.is_some() && pymupdf_version.is_some(),
        python_version,
        pymupdf_version,
        embedded: exe_embedded,
    }
}

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

/// 清理 PDF 字体私有编码字符（PUA）等无法映射到 Unicode 的占位字符。
///
/// 部分 PDF 的公式/符号字体缺少 ToUnicode 映射，PyMuPDF 只能给出私有区码位
/// （如 U+E000–U+F8FF），直接展示是豆腐块，发给 LLM 也是噪声。
/// 返回（清理后的文本, 删除的字符数）。
pub fn clean_pua(text: &str) -> (String, usize) {
    let mut removed = 0usize;
    let cleaned: String = text
        .chars()
        .filter(|c| {
            let u = *c as u32;
            let pua = (0xE000..=0xF8FF).contains(&u) // BMP 私有区
                || (0xF0000..=0xFFFFD).contains(&u) // 补充私有区 A
                || (0x100000..=0x10FFFD).contains(&u); // 补充私有区 B
            if pua {
                removed += 1;
            }
            !pua
        })
        .collect();
    (cleaned, removed)
}

/// 抽取文本统一清洗：有删除时写日志并在 CLI 提示（字体缺映射属数据质量问题）。
fn clean_extracted(text: String, what: &str) -> String {
    let (cleaned, removed) = clean_pua(&text);
    if removed > 0 {
        let msg =
            format!("{what} 有 {removed} 个字符因字体缺少 Unicode 映射（私有编码）无法识别，已跳过");
        logging::warn(&msg);
        eprintln!("⚠️  {msg}");
    }
    cleaned
}

/// 用 PyMuPDF（Python 子进程）抽取 PDF 全文，按页用 form-feed 分隔。
/// 需要环境里装了 pymupdf：`pip install pymupdf`。
pub async fn extract_pages(path: &Path) -> Result<Vec<String>> {
    let t0 = std::time::Instant::now();
    let (py, args) = python_command().await;
    let mut cmd = Command::new(&py);
    cmd.args(&args).arg("-c").arg(SCRIPT).arg(path);
    let out = run_killable(cmd, &format!("{py}（需安装 pymupdf）")).await?;
    if !out.status.success() {
        let e = String::from_utf8_lossy(&out.stderr);
        logging::error(format!("PDF 解析失败 {}: {e}", path.display()));
        return Err(anyhow!(
            "PDF 解析失败: {e}\n提示：确认已 `pip install pymupdf`，且文件是有效 PDF。"
        ));
    }
    let text = clean_extracted(
        String::from_utf8_lossy(&out.stdout).into_owned(),
        "PDF 文本",
    );
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
    let (py, args) = python_command().await;
    let mut cmd = Command::new(&py);
    cmd.args(&args).arg("-c").arg(OCR_SCRIPT).arg(path).arg(&lang);
    logging::info(format!("OCR 开始（lang={lang}）：{}", path.display()));
    let out = run_killable(cmd, &format!("OCR（{py}/tesseract）")).await?;
    if !out.status.success() {
        let e = String::from_utf8_lossy(&out.stderr);
        logging::error(format!("OCR 失败 {}: {e}", path.display()));
        if e.contains("No such file or directory") || e.contains("FileNotFoundError") {
            return Err(anyhow!("tesseract 运行失败（可能已被卸载）: {e}"));
        }
        return Err(anyhow!("OCR 识别失败: {e}"));
    }
    let text = clean_extracted(String::from_utf8_lossy(&out.stdout).into_owned(), "OCR 文本");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_pua_removes_private_use_chars_and_counts() {
        // 模拟该论文公式处的私有区字符：\uf8eb-\uf8fb
        let raw = "SE(\u{f8eb}x\u{f8fc}) = \u{f8ed} (\u{f8ee}x)\u{f8ef} log (\u{f8f0}x\u{f8f1})";
        let (clean, removed) = clean_pua(raw);
        assert_eq!(removed, 7);
        assert!(clean.contains("SE("));
        assert!(!clean.contains('\u{f8eb}'));
        assert!(!clean.contains('\u{f8f1}'));
    }

    #[test]
    fn clean_pua_keeps_normal_text_and_emoji() {
        let raw = "正常中文、English、公式 $E=mc^2$、emoji 🌍";
        let (clean, removed) = clean_pua(raw);
        assert_eq!(removed, 0);
        assert_eq!(clean, raw);
    }

    #[test]
    fn clean_pua_removes_supplementary_private_use() {
        let raw = "a\u{f0001}b\u{10fffd}c";
        let (clean, removed) = clean_pua(raw);
        assert_eq!(removed, 2);
        assert_eq!(clean, "abc");
    }

    #[test]
    fn python_candidates_order_env_first_and_embedded_used() {
        let dir = std::path::Path::new("/opt/ph");
        let cands = python_candidates(Some("/usr/bin/python3.12"), Some(dir), false);
        assert_eq!(cands[0], ("/usr/bin/python3.12".to_string(), vec![]));
        // 不存在的内置路径不入选；其余为 PATH 兜底候选
        let names: Vec<&str> = cands.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(names, vec!["/usr/bin/python3.12", "python3", "python"]);
    }

    #[test]
    fn python_candidates_windows_tries_py_launcher() {
        let cands = python_candidates(None, None, true);
        assert_eq!(cands[0], ("py".to_string(), vec!["-3".to_string()]));
        assert_eq!(cands[1].0, "python");
        assert_eq!(cands[2].0, "python3");
        // 空环境变量不产生候选，也不重复
        let cands2 = python_candidates(Some("  "), None, false);
        assert_eq!(cands2.len(), 2);
    }
}
