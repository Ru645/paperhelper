use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Command;

const SCRIPT: &str = r#"
import sys, pymupdf
d = pymupdf.open(sys.argv[1])
out = []
for p in d:
    out.append(p.get_text())
sys.stdout.write("\f".join(out))
"#;

/// 用 PyMuPDF（Python 子进程）抽取 PDF 全文，按页用 form-feed 分隔。
/// 需要环境里装了 pymupdf：`pip install pymupdf`。
pub fn extract_pages(path: &Path) -> Result<Vec<String>> {
    let out = Command::new("python3")
        .arg("-c")
        .arg(SCRIPT)
        .arg(path)
        .output()
        .map_err(|e| anyhow!("调用 python3 失败（需安装 pymupdf）: {e}"))?;
    if !out.status.success() {
        let e = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("pymupdf 抽取失败: {e}"));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let pages: Vec<String> = text
        .split('\u{0C}')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if pages.is_empty() {
        return Err(anyhow!("PDF 没有解析出任何文本（可能是扫描件/纯图片）"));
    }
    Ok(pages)
}

/// 用 Tesseract OCR 对 PDF 逐页做文字识别。
/// 需要安装：Debian/Ubuntu `sudo apt install tesseract-ocr tesseract-ocr-chi-sim`；
/// macOS `brew install tesseract tesseract-lang`。
/// 流程：用 PyMuPDF 把每页渲染成图片 → tesseract 识别。
/// 预检 tesseract 是否安装；语言包按可用性自动选择（缺中文包时降级 eng 并提示）。
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

pub fn ocr_extract(path: &Path) -> Result<String> {
    // 预检 1：tesseract 是否安装
    match Command::new("tesseract").arg("--version").output() {
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
    let lang = if let Ok(out) = Command::new("tesseract").arg("--list-langs").output() {
        let langs = String::from_utf8_lossy(&out.stdout);
        if langs.contains("chi_sim") {
            "chi_sim+eng".to_string()
        } else {
            eprintln!(
                "{} 未检测到中文语言包（chi_sim），OCR 将只用英文（eng）。中文论文效果会差，建议安装：sudo apt install tesseract-ocr-chi-sim",
                "⚠️ "
            );
            "eng".to_string()
        }
    } else {
        "eng".to_string() // --list-langs 失败时保守用 eng
    };

    let out = Command::new("python3")
        .arg("-c")
        .arg(OCR_SCRIPT)
        .arg(path)
        .arg(&lang)
        .output()
        .map_err(|e| anyhow!("调用 python3 失败（需安装 pymupdf）: {e}"))?;
    if !out.status.success() {
        let e = String::from_utf8_lossy(&out.stderr);
        if e.contains("No such file or directory") || e.contains("FileNotFoundError") {
            return Err(anyhow!("tesseract 运行失败（可能已被卸载）: {e}"));
        }
        return Err(anyhow!("OCR 识别失败: {e}"));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        return Err(anyhow!("OCR 未识别出任何文本"));
    }
    Ok(text)
}
