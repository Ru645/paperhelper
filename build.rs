//! 构建脚本：把 `web/vendor/` 下的第三方前端资源（marked / KaTeX，MIT 许可，
//! 随仓库提交）编译进二进制，供离线使用：
//!
//! 1. `$OUT_DIR/vendor_files.rs`：`/vendor/*` 路由的嵌入表（`include_bytes!`）
//! 2. `$OUT_DIR/katex_inline.css`：KaTeX 样式，woff2 字体转 data URI、
//!    去掉 woff/ttf 备源（供 `export.rs` 的 HTML 导出内联，单文件完全离线）
//!
//! 不联网、不下载任何东西；升级第三方库 = 替换 `web/vendor/` 里的文件。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let vendor = manifest.join("web").join("vendor");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    println!("cargo:rerun-if-changed={}", vendor.display());
    println!("cargo:rerun-if-changed=build.rs");

    // 1) vendor 文件表（递归；路径统一用 `/` 分隔，Windows 下也能稳定匹配）
    let mut files = Vec::new();
    collect(&vendor, &vendor, &mut files);
    files.sort();
    let mut table = String::from("// 由 build.rs 生成，请勿手改\n");
    table.push_str(
        "pub struct VendorFile { pub path: &'static str, pub bytes: &'static [u8], pub mime: &'static str }\n",
    );
    table.push_str("pub static VENDOR_FILES: &[VendorFile] = &[\n");
    for rel in &files {
        let abs = vendor.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        let _ = writeln!(
            table,
            "    VendorFile {{ path: {rel:?}, bytes: include_bytes!({:?}), mime: {:?} }},",
            abs.to_string_lossy(),
            mime_of(rel)
        );
    }
    table.push_str("];\n");
    std::fs::write(out.join("vendor_files.rs"), table).expect("write vendor_files.rs");

    // 2) KaTeX 内联样式（导出用）
    let css = std::fs::read_to_string(vendor.join("katex.min.css")).unwrap_or_default();
    let inline = inline_katex_fonts(&css, &vendor);
    std::fs::write(out.join("katex_inline.css"), inline).expect("write katex_inline.css");
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(root, &p, out);
        } else if p.is_file() {
            if let Ok(rel) = p.strip_prefix(root) {
                let s = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/");
                if !s.is_empty() {
                    out.push(s);
                }
            }
        }
    }
}

fn mime_of(rel: &str) -> &'static str {
    match rel.rsplit('.').next().unwrap_or("") {
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "map" => "application/json",
        _ => "text/plain; charset=utf-8",
    }
}

/// 把 `url(fonts/xxx.woff2)` 换成 data URI；非 woff2 的备源（woff/ttf）连同
/// 前面的逗号与后面的 `format(...)` 一并删除（现代浏览器都支持 woff2）。
fn inline_katex_fonts(css: &str, vendor: &Path) -> String {
    const PREFIX: &str = "url(fonts/";
    let mut out = String::with_capacity(css.len() * 2);
    let mut rest = css;
    while let Some(pos) = rest.find(PREFIX) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + PREFIX.len()..];
        let Some(end) = after.find(')') else {
            out.push_str(&rest[pos..]);
            return out;
        };
        let name = &after[..end];
        let mut tail = &after[end + 1..];
        // 可选的后缀 ` format('xxx')`
        let fmt = if tail.starts_with(" format(") {
            let Some(close) = tail.find(')') else {
                out.push_str(&rest[pos..]);
                return out;
            };
            let f = tail[..=close].to_string();
            tail = &tail[close + 1..];
            f
        } else {
            String::new()
        };
        if name.ends_with(".woff2") {
            let path = vendor.join("fonts").join(name);
            let data = std::fs::read(&path).unwrap_or_default();
            out.push_str("url(data:font/woff2;base64,");
            out.push_str(&base64(&data));
            out.push(')');
            out.push_str(&fmt);
        } else if out.ends_with(',') {
            // 非 woff2 备源：删掉前导逗号（连 fmt 一起丢弃）
            out.pop();
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// 最小 base64（避免为构建脚本引入依赖）。
fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((*chunk.get(1).unwrap_or(&0) as u32) << 8)
            | (*chunk.get(2).unwrap_or(&0) as u32);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}
