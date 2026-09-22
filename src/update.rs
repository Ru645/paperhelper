//! 版本更新：检查新版本、供 Web 端提示与一键更新。
//!
//! 设计要点：
//! - 检查 `releases/latest/download/update.json`（CI 每次发版生成并作为 Release 资产，
//!   见 `.github/workflows/windows-release.yml`）；`[update] source_url` 可指向镜像。
//! - 自动检查 24h 节流，结果缓存在 `.paperhelper/update_state.json`；失败静默（只记日志），
//!   网络不通不影响正常使用。
//! - 手写版本比较与 SHA-256（避免引入新依赖），均带单测（含 NIST 标准向量）。

use std::cmp::Ordering;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::config::UpdateConfig;
use crate::interrupt;
use crate::logging;
use crate::paths;

/// 默认更新源：GitHub Releases 上的 update.json。
pub const DEFAULT_SOURCE_URL: &str =
    "https://github.com/Ru645/paperhelper/releases/latest/download/update.json";

/// 手动下载页（浏览器版 / 绿色版更新时打开）。
pub const RELEASES_PAGE: &str = "https://github.com/Ru645/paperhelper/releases/latest";

/// 检查更新的网络超时：失败静默，不拖慢启动。
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// 自动检查间隔（秒）：24h 内直接返回缓存。
const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

/// 当前程序版本（编译期注入）。
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 是否桌面壳启动（桌面版才有一键静默更新）。
pub fn is_desktop() -> bool {
    std::env::var("PAPERHELPER_DESKTOP").map(|v| v == "1").unwrap_or(false)
}

/// 是否安装版（安装目录含 uninstall.exe；绿色版无法静默安装，走手动下载）。
pub fn is_installed() -> bool {
    std::env::var("PAPERHELPER_INSTALLED").map(|v| v == "1").unwrap_or(false)
}

// ===== 版本比较 =====

/// `candidate` 是否比 `current` 新。容忍 `v` 前缀、缺段（`0.2` == `0.2.0`）
/// 与 `-beta` 之类的预发布后缀。
pub fn is_newer(candidate: &str, current: &str) -> bool {
    compare_versions(candidate, current) == Ordering::Greater
}

/// 解析版本号为数字段；非数字前缀按 0 处理（不 panic）。
fn version_parts(v: &str) -> Vec<u64> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let core = v.split(['-', '+']).next().unwrap_or("");
    core.split('.')
        .map(|seg| {
            let digits: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().unwrap_or(0)
        })
        .collect()
}

fn compare_versions(a: &str, b: &str) -> Ordering {
    let (pa, pb) = (version_parts(a), version_parts(b));
    let n = pa.len().max(pb.len());
    for i in 0..n {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    Ordering::Equal
}

// ===== SHA-256（流式，避免新依赖） =====

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// 增量 SHA-256（下载大文件时边读边算）。
struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    len_bits: u64,
}

impl Sha256 {
    fn new() -> Self {
        Sha256 {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buf: [0; 64],
            buf_len: 0,
            len_bits: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.len_bits = self.len_bits.wrapping_add((data.len() as u64).wrapping_mul(8));
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
        let mut chunks = data.chunks_exact(64);
        for chunk in &mut chunks {
            let mut block = [0u8; 64];
            block.copy_from_slice(chunk);
            self.compress(&block);
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            self.buf[self.buf_len..self.buf_len + rest.len()].copy_from_slice(rest);
            self.buf_len += rest.len();
        }
    }

    fn finish(mut self) -> [u8; 32] {
        let len_bits = self.len_bits;
        self.update(&[0x80]);
        while self.buf_len != 56 {
            self.update(&[0]);
        }
        self.update(&len_bits.to_be_bytes());
        let mut out = [0u8; 32];
        for (i, v) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
        self.h[5] = self.h[5].wrapping_add(f);
        self.h[6] = self.h[6].wrapping_add(g);
        self.h[7] = self.h[7].wrapping_add(h);
    }
}

/// 计算内存数据的 SHA-256 十六进制摘要（单测校验实现用）。
#[cfg(test)]
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex(&h.finish())
}

/// 流式计算文件的 SHA-256（下载校验用，不整文件载入内存）。
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)
        .with_context(|| format!("读取文件失败: {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .with_context(|| format!("读取文件失败: {}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finish()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ===== 更新清单 / 状态 =====

/// 更新清单（update.json）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Manifest {
    /// 格式版本（当前为 1）；更高版本说明格式不兼容，提示手动下载。
    #[serde(default)]
    pub schema: u32,
    pub version: String,
    #[serde(default)]
    pub released_at: String,
    /// 版本说明页（Release 页面）。
    #[serde(default)]
    pub notes_url: String,
    /// 安装包（Windows 安装版一键更新用）。
    #[serde(default)]
    pub setup: Option<Asset>,
    /// 免安装 zip（绿色版/浏览器用户手动下载用）。
    #[serde(default)]
    pub zip: Option<Asset>,
}

/// 清单里的一个下载资产。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Asset {
    pub url: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub sha256: String,
}

/// 检查状态（`.paperhelper/update_state.json`）：24h 节流 + 跳过版本 + 离线缓存。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateState {
    /// 上次检查时间（unix 秒；0=从未检查）。
    #[serde(default)]
    pub last_check_at: i64,
    /// 上次检查到的清单（网络失败时保留旧值）。
    #[serde(default)]
    pub latest: Option<Manifest>,
    /// 用户选择「跳过此版本」的版本号。
    #[serde(default)]
    pub skipped_version: String,
}

impl UpdateState {
    pub fn load() -> Self {
        let path = paths::update_state_path();
        let Ok(s) = fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&s).unwrap_or_else(|e| {
            logging::warn(format!(
                "解析更新状态失败（{}），按未检查处理：{e}",
                path.display()
            ));
            Self::default()
        })
    }

    pub fn save(&self) -> Result<()> {
        paths::ensure_data_dir()?;
        let s = serde_json::to_string_pretty(self).context("序列化更新状态")?;
        fs::write(paths::update_state_path(), s).context("写入 update_state.json")?;
        Ok(())
    }

    fn save_logged(&self) {
        if let Err(e) = self.save() {
            logging::warn(format!("保存更新状态失败：{e:#}"));
        }
    }

    /// 距上次检查是否不足 24h（是则直接用缓存，不再请求网络）。
    fn is_fresh(&self, now: i64) -> bool {
        self.last_check_at > 0 && now.saturating_sub(self.last_check_at) < CHECK_INTERVAL_SECS
    }

    /// 是否已选择跳过 `version`。
    fn is_skipped(&self, version: &str) -> bool {
        !self.skipped_version.is_empty() && self.skipped_version == version
    }
}

/// 一次检查的结果（直接作为 Web API 的 JSON）。
#[derive(Debug, Clone, Serialize, Default)]
pub struct Status {
    pub ok: bool,
    pub error: String,
    pub current: String,
    pub latest: String,
    pub released_at: String,
    pub newer: bool,
    pub skipped: bool,
    pub notes_url: String,
    pub manual_url: String,
    pub setup_url: String,
    pub zip_url: String,
    pub from_cache: bool,
    pub checked_at: i64,
    pub source_url: String,
}

impl Status {
    fn from_manifest(m: &Manifest, state: &UpdateState, source: &str, from_cache: bool) -> Status {
        Status {
            ok: true,
            error: String::new(),
            current: current_version().to_string(),
            latest: m.version.clone(),
            released_at: m.released_at.clone(),
            newer: is_newer(&m.version, current_version()),
            skipped: state.is_skipped(&m.version),
            notes_url: if m.notes_url.is_empty() {
                RELEASES_PAGE.to_string()
            } else {
                m.notes_url.clone()
            },
            manual_url: RELEASES_PAGE.to_string(),
            setup_url: m.setup.as_ref().map(|a| a.url.clone()).unwrap_or_default(),
            zip_url: m.zip.as_ref().map(|a| a.url.clone()).unwrap_or_default(),
            from_cache,
            checked_at: state.last_check_at,
            source_url: source.to_string(),
        }
    }
}

/// 生效的更新源：`[update] source_url` 非空时优先（镜像/内网），否则官方地址。
pub fn effective_source(cfg: &UpdateConfig) -> String {
    let custom = cfg.source_url.trim();
    if custom.is_empty() {
        DEFAULT_SOURCE_URL.to_string()
    } else {
        custom.to_string()
    }
}

/// 检查更新：24h 节流（`force=true` 跳过）；网络失败静默并返回缓存/错误。
pub async fn check(client: &reqwest::Client, cfg: &UpdateConfig, force: bool) -> Status {
    let now = unix_now();
    let mut state = UpdateState::load();
    let source = effective_source(cfg);
    if !force && state.is_fresh(now) {
        if let Some(m) = state.latest.clone() {
            return Status::from_manifest(&m, &state, &source, true);
        }
    }
    match fetch_manifest(client, &source).await {
        Ok(m) => {
            state.last_check_at = now;
            state.latest = Some(m.clone());
            state.save_logged();
            Status::from_manifest(&m, &state, &source, false)
        }
        Err(e) => {
            state.last_check_at = now;
            state.save_logged();
            logging::warn(format!("检查更新失败（{source}）：{e:#}"));
            let mut st = state
                .latest
                .clone()
                .map(|m| Status::from_manifest(&m, &state, &source, true))
                .unwrap_or_else(|| Status {
                    current: current_version().to_string(),
                    manual_url: RELEASES_PAGE.to_string(),
                    source_url: source.clone(),
                    ..Status::default()
                });
            st.ok = false;
            st.error = format!("{e:#}");
            st
        }
    }
}

/// 记录「跳过此版本」：该版本不再提示（手动检查仍能看到、仍可更新）。
pub fn skip(version: &str) -> Result<()> {
    let mut state = UpdateState::load();
    state.skipped_version = version.trim().to_string();
    state.save()
}

// ===== 一键更新：下载与安装（Windows 桌面安装版） =====

/// 下载/安装进度（全局，供 `GET /api/update/status` 轮询；独立于会话锁）。
#[derive(Debug, Clone, Serialize, Default)]
pub struct DownloadStatus {
    /// `idle` / `downloading` / `verifying` / `ready` / `error`
    pub phase: String,
    pub version: String,
    pub downloaded: u64,
    pub total: u64,
    /// 就绪后的安装包路径。
    pub path: String,
    pub error: String,
}

impl DownloadStatus {
    fn idle() -> Self {
        DownloadStatus {
            phase: "idle".into(),
            ..Default::default()
        }
    }
}

static DOWNLOAD: LazyLock<Mutex<DownloadStatus>> =
    LazyLock::new(|| Mutex::new(DownloadStatus::idle()));

fn set_download(f: impl FnOnce(&mut DownloadStatus)) {
    if let Ok(mut st) = DOWNLOAD.lock() {
        f(&mut st);
    }
}

/// 当前下载状态快照。
pub fn download_status() -> DownloadStatus {
    DOWNLOAD.lock().map(|g| g.clone()).unwrap_or_default()
}

/// 安装包缓存路径：`%TEMP%/paperhelper-update/paperhelper-setup-<version>.exe`。
fn setup_cache_path(version: &str) -> PathBuf {
    std::env::temp_dir()
        .join("paperhelper-update")
        .join(format!("paperhelper-setup-{version}.exe"))
}

/// 下载安装包并校验 SHA-256（Web 后台任务调用）；进度与错误写入全局下载状态。
pub async fn download_setup(client: &reqwest::Client, cfg: &UpdateConfig) -> Result<PathBuf> {
    // 清掉上次任务残留的打断标志，使本次下载可正常开始（下载中按「停止」可取消）。
    interrupt::reset();
    set_download(|st| {
        *st = DownloadStatus {
            phase: "downloading".into(),
            ..DownloadStatus::default()
        }
    });
    let result = download_setup_inner(client, cfg).await;
    if let Err(e) = &result {
        let msg = format!("{e:#}");
        logging::warn(format!("下载更新安装包失败：{msg}"));
        set_download(|st| {
            st.phase = "error".into();
            st.error = msg;
        });
    }
    result
}

async fn download_setup_inner(client: &reqwest::Client, cfg: &UpdateConfig) -> Result<PathBuf> {
    let source = effective_source(cfg);
    let manifest = fetch_manifest(client, &source).await?;
    let asset = manifest.setup.clone().ok_or_else(|| {
        anyhow!("这个版本没有可用的安装包，请到 {RELEASES_PAGE} 手动下载")
    })?;
    set_download(|st| {
        st.version = manifest.version.clone();
        st.total = asset.size;
    });

    let dest = setup_cache_path(&manifest.version);
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).with_context(|| format!("创建下载目录失败: {}", dir.display()))?;
    }
    if dest.is_file() {
        set_download(|st| st.phase = "verifying".into());
        if verify_asset(&dest, &asset.sha256) {
            set_download(|st| {
                st.phase = "ready".into();
                st.downloaded = dest.metadata().map(|m| m.len()).unwrap_or(0);
                st.path = dest.display().to_string();
            });
            return Ok(dest);
        }
        // 上次没下完或已损坏：删掉重下
        let _ = fs::remove_file(&dest);
    }

    let part = dest.with_extension("exe.part");
    let _ = fs::remove_file(&part);
    if let Err(e) = download_stream(client, &asset, &part).await {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    set_download(|st| st.phase = "verifying".into());
    let (check_path, expected) = (part.clone(), asset.sha256.clone());
    let verified = tokio::task::spawn_blocking(move || verify_asset(&check_path, &expected))
        .await
        .unwrap_or(false);
    if !verified {
        let _ = fs::remove_file(&part);
        bail!("安装包校验失败（可能下载不完整），请重试或手动下载");
    }
    fs::rename(&part, &dest)
        .with_context(|| format!("保存安装包失败: {}", dest.display()))?;

    set_download(|st| {
        st.phase = "ready".into();
        st.downloaded = dest.metadata().map(|m| m.len()).unwrap_or(0);
        st.path = dest.display().to_string();
    });
    logging::info(format!(
        "更新安装包已就绪：{}（v{}）",
        dest.display(),
        manifest.version
    ));
    Ok(dest)
}

/// 下载到 `<dest>.part`，边下边更新进度；被打断（Web「停止」/Ctrl-C）则中止。
async fn download_stream(client: &reqwest::Client, asset: &Asset, dest: &Path) -> Result<()> {
    let resp = client
        .get(&asset.url)
        .header("User-Agent", format!("paperhelper/{}", current_version()))
        .send()
        .await
        .map_err(|e| anyhow!("下载安装包失败（{e}），请检查网络后重试"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("下载安装包失败：服务器返回 HTTP {status}");
    }
    let total = resp.content_length().unwrap_or(asset.size);
    if total > 0 {
        set_download(|st| st.total = total);
    }
    let mut file = tokio::fs::File::create(dest)
        .await
        .with_context(|| format!("创建下载文件失败: {}", dest.display()))?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;
    loop {
        let chunk = tokio::select! {
            c = stream.next() => c,
            _ = interrupt::wait() => {
                let _ = file.shutdown().await;
                bail!("下载已取消");
            }
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|e| anyhow!("下载中断（{e}），请重试"))?;
        file.write_all(&chunk).await.context("写入安装包失败")?;
        downloaded += chunk.len() as u64;
        set_download(|st| st.downloaded = downloaded);
    }
    let _ = file.flush().await;
    if total > 0 && downloaded != total {
        bail!("下载不完整（{downloaded}/{total} 字节），请重试");
    }
    Ok(())
}

/// 校验文件 SHA-256；清单未提供摘要时放行（只记日志）。
fn verify_asset(path: &Path, expected: &str) -> bool {
    let expected = expected.trim();
    if expected.is_empty() {
        logging::warn("更新清单未提供 sha256，跳过安装包校验");
        return true;
    }
    sha256_file(path)
        .map(|actual| actual.eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

/// 写入「更新标记」，桌面壳看到后退出并启动安装器（仅桌面安装版可用）。
pub fn apply_update() -> Result<()> {
    if !is_desktop() {
        bail!("当前不是桌面版，无法自动安装，请手动下载");
    }
    if !is_installed() {
        bail!("当前为免安装版，无法自动安装，请手动下载");
    }
    let marker = std::env::var("PAPERHELPER_UPDATE_MARKER").unwrap_or_default();
    if marker.trim().is_empty() {
        bail!("找不到更新标记路径（请重启软件后重试）");
    }
    let st = download_status();
    if st.phase != "ready" || st.path.trim().is_empty() {
        bail!("安装包尚未下载完成，请先下载");
    }
    let marker_path = Path::new(marker.trim());
    if let Some(dir) = marker_path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    // 先写临时文件再改名：桌面壳读到的内容总是完整的
    let tmp = marker_path.with_extension("tmp");
    fs::write(&tmp, format!("{}\n{}\n", st.version, st.path))
        .with_context(|| format!("写入更新标记失败: {}", tmp.display()))?;
    fs::rename(&tmp, marker_path)
        .with_context(|| format!("写入更新标记失败: {}", marker_path.display()))?;
    logging::info(format!(
        "已请求更新到 v{}（安装包 {}），等待桌面壳重启安装",
        st.version, st.path
    ));
    Ok(())
}

async fn fetch_manifest(client: &reqwest::Client, url: &str) -> Result<Manifest> {
    let resp = client
        .get(url)
        .timeout(CHECK_TIMEOUT)
        .header("User-Agent", format!("paperhelper/{}", current_version()))
        .send()
        .await
        .map_err(|e| anyhow!("无法连接更新服务器（{e}）"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("更新服务器返回 HTTP {status}"));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| anyhow!("读取更新信息失败（{e}）"))?;
    let m: Manifest = serde_json::from_str(&text)
        .map_err(|e| anyhow!("更新信息格式不正确（{e}）"))?;
    if m.version.trim().is_empty() {
        return Err(anyhow!("更新信息缺少版本号"));
    }
    if m.schema > 1 {
        return Err(anyhow!(
            "更新信息格式版本过高（schema {}），请到 {} 手动下载",
            m.schema,
            RELEASES_PAGE
        ));
    }
    Ok(m)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_edges() {
        assert!(is_newer("0.1.10", "0.1.9"), "0.1.10 应比 0.1.9 新");
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("v0.2.0", "0.1.11"));
        assert!(is_newer("0.2", "0.1.9"));
        assert!(!is_newer("0.1.9", "0.1.9"));
        assert!(!is_newer("v0.1.9", "0.1.9"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("", "0.0.1"));
        assert_eq!(compare_versions("0.2.0-beta.1", "0.2.0"), Ordering::Equal);
    }

    #[test]
    fn sha256_matches_nist_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn sha256_streaming_matches_oneshot_across_blocks() {
        let data: Vec<u8> = (0..200u16).map(|i| (i % 251) as u8).collect();
        for len in [0usize, 1, 55, 56, 57, 63, 64, 65, 127, 128, 129, 200] {
            let mut h = Sha256::new();
            for chunk in data[..len].chunks(7) {
                h.update(chunk);
            }
            assert_eq!(h.finish(), {
                let mut one = Sha256::new();
                one.update(&data[..len]);
                one.finish()
            }, "len={len}");
        }
    }

    #[test]
    fn parse_manifest_tolerates_missing_fields() {
        let m: Manifest = serde_json::from_str(r#"{"version":"0.2.0"}"#).unwrap();
        assert_eq!(m.version, "0.2.0");
        assert_eq!(m.schema, 0);
        assert!(m.setup.is_none() && m.zip.is_none());
        let full: Manifest = serde_json::from_str(
            r#"{"schema":1,"version":"0.2.0","released_at":"2026-09-22",
                "notes_url":"https://example.com/notes",
                "setup":{"url":"https://example.com/a.exe","size":123,"sha256":"ab"},
                "zip":{"url":"https://example.com/a.zip","size":456,"sha256":"cd"}}"#,
        )
        .unwrap();
        assert_eq!(full.setup.unwrap().sha256, "ab");
        assert_eq!(full.zip.unwrap().size, 456);
    }

    #[test]
    fn state_freshness_and_skip() {
        let now = unix_now();
        assert!(!UpdateState::default().is_fresh(now));
        let fresh = UpdateState {
            last_check_at: now - 10,
            ..Default::default()
        };
        assert!(fresh.is_fresh(now));
        let stale = UpdateState {
            last_check_at: now - CHECK_INTERVAL_SECS - 1,
            ..Default::default()
        };
        assert!(!stale.is_fresh(now));
        let skipped = UpdateState {
            skipped_version: "0.2.0".into(),
            ..Default::default()
        };
        assert!(skipped.is_skipped("0.2.0"));
        assert!(!skipped.is_skipped("0.2.1"));
    }

    #[test]
    fn status_from_manifest_compares_and_falls_back() {
        let m = Manifest {
            version: "999.0.0".into(),
            ..Default::default()
        };
        let state = UpdateState {
            last_check_at: 123,
            skipped_version: "999.0.0".into(),
            latest: None,
        };
        let s = Status::from_manifest(&m, &state, "src", true);
        assert!(s.ok && s.newer && s.skipped && s.from_cache);
        assert_eq!(s.checked_at, 123);
        assert_eq!(s.notes_url, RELEASES_PAGE, "notes_url 缺省应回落下载页");
    }

    #[test]
    fn setup_cache_path_is_in_temp_with_version() {
        let p = setup_cache_path("0.2.0");
        assert_eq!(p.file_name().unwrap(), "paperhelper-setup-0.2.0.exe");
        assert!(p.to_string_lossy().contains("paperhelper-update"));
        assert!(p.starts_with(std::env::temp_dir()));
    }

    #[test]
    fn verify_asset_checks_sha_and_allows_empty() {
        let dir = std::env::temp_dir().join("paperhelper-test-verify");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.bin");
        fs::write(&file, b"hello update").unwrap();
        let sha = sha256_hex(b"hello update");
        assert!(verify_asset(&file, &sha));
        assert!(verify_asset(&file, &sha.to_uppercase()), "大小写不敏感");
        assert!(!verify_asset(&file, "deadbeef"));
        assert!(verify_asset(&file, ""), "清单缺摘要时放行");
        assert!(!verify_asset(&dir.join("missing.bin"), &sha), "文件不存在不 panic");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_update_requires_desktop_and_ready() {
        // 测试进程未设 PAPERHELPER_DESKTOP：应直接拒绝，且不写任何标记文件
        let err = apply_update().unwrap_err().to_string();
        assert!(err.contains("桌面版"), "错误提示应说明需要桌面版：{err}");
    }

    #[test]
    fn download_status_default_is_idle() {
        let st = DownloadStatus::default();
        assert_eq!(st.phase, "");
        assert_eq!(st.downloaded, 0);
        assert!(st.error.is_empty());
    }
}
