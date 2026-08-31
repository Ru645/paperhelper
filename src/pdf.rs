use anyhow::{anyhow, Result};
use std::path::Path;

/// 把 PDF 按页拆分，返回每页文本（按 form-feed 分页符切分）。
pub fn extract_pages(path: &Path) -> Result<Vec<String>> {
    let text = pdf_extract::extract_text(path)
        .map_err(|e| anyhow!("解析 PDF 失败 ({}): {e}", path.display()))?;
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
