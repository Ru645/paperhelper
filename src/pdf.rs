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
/// 需要安装 tesseract 和 ImageMagick：`apt install tesseract-ocr imagemagick`。
/// 流程：用 PyMuPDF 把每页渲染成图片 → tesseract 识别。
const OCR_SCRIPT: &str = r#"
import sys, pymupdf, subprocess, tempfile, os
d = pymupdf.open(sys.argv[1])
out = []
for i, page in enumerate(d):
    # 渲染为高分辨率图片
    mat = pymupdf.Matrix(3, 3)
    pix = page.get_pixmap(matrix=mat)
    with tempfile.NamedTemporaryFile(suffix=".png", delete=False) as f:
        pix.save(f.name)
        # tesseract 识别
        r = subprocess.run(["tesseract", f.name, "stdout", "-l", "chi_sim+eng"],
                           capture_output=True, text=True)
        out.append(r.stdout.strip())
        os.unlink(f.name)
sys.stdout.write("\f".join(out))
"#;

pub fn ocr_extract(path: &Path) -> Result<String> {
    let out = Command::new("python3")
        .arg("-c")
        .arg(OCR_SCRIPT)
        .arg(path)
        .output()
        .map_err(|e| anyhow!("OCR 调用失败（需安装 tesseract）: {e}"))?;
    if !out.status.success() {
        let e = String::from_utf8_lossy(&out.stderr);
        if e.contains("not found") || e.contains("No module") {
            return Err(anyhow!("OCR 依赖缺失（需安装 tesseract-ocr 和中文语言包）: {e}"));
        }
        return Err(anyhow!("OCR 识别失败: {e}"));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        return Err(anyhow!("OCR 未识别出任何文本"));
    }
    Ok(text)
}
