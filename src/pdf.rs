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
