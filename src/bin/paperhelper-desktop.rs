//! Windows 桌面壳：把 `paperhelper web` 服务装进原生窗口（WebView2），零基础用户双击即用。
//!
//! 工作方式（不做 lib 重构，桌面壳只当「启动器 + 窗口」）：
//! 1. 单实例互斥锁（`Local\PaperHelperDesktop`）：已在运行则把已有窗口拉到前台并退出；
//! 2. 数据目录：未设 `PAPERHELPER_DATA_DIR` 时默认 `%USERPROFILE%\PaperHelper\.paperhelper`
//!    （与 `src/paths.rs` 文档一致，避免「双击图标后笔记找不到」）；
//! 3. 后台启动同目录的 `paperhelper.exe web --port 0 --port-file <临时文件>`（隐藏控制台），
//!    用 Job Object（KILL_ON_JOB_CLOSE）保证壳退出/崩溃时子进程一起结束；
//! 4. 轮询端口文件拿到实际端口后，用 tao 建窗口 + wry 加载 `http://127.0.0.1:<port>/`；
//! 5. 关窗：先 `/api/interrupt`（停掉在途 LLM 任务）再 `/api/shutdown`（优雅退出），
//!    等待子进程退出（超时强杀）并清理端口文件；
//! 6. 一键更新：服务写入更新标记后，壳复制自身到临时目录并以 `--run-updater` 启动，
//!    更新器等壳退出 → 静默运行安装包 `/S` → 重新启动新版本 → 自删临时副本。
//!
//! 其他平台编译为空壳，保证 `cargo build` / `cargo test` 跨平台可用。

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("paperhelper-desktop 仅支持 Windows：其他平台请用 `paperhelper web` 在浏览器中使用。");
}

#[cfg(windows)]
fn main() {
    // 更新器模式：等旧壳退出 → 静默安装 → 重新启动（不能再走单实例/窗口逻辑）
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|a| a.as_str()) == Some("--run-updater") {
        std::process::exit(win::run_updater(&args[2..]));
    }
    if let Err(e) = win::run() {
        win::fatal(&format!("{e:#}"));
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod win {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use tao::dpi::LogicalSize;
    use tao::event::{Event, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoop};
    use tao::platform::run_return::EventLoopExtRunReturn;
    use tao::window::{Icon, WindowBuilder};
    use wry::WebViewBuilder;

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, MessageBoxW, SetForegroundWindow, ShowWindow, MB_ICONERROR,
        MB_ICONINFORMATION, MB_OK, SW_RESTORE,
    };

    const WINDOW_TITLE: &str = "PaperHelper — 论文学习助手";
    const MUTEX_NAME: &str = "Local\\PaperHelperDesktopSingleton";
    /// 子进程控制台不弹窗（CREATE_NO_WINDOW）。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    /// 图标：32x32 原始 RGBA（由 scripts/make_icon.py 生成）。
    const ICON_32: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon_32.rgba"));

    /// 桌面壳日志文件（子进程 stdout/stderr 也重定向到这里，出问题时把路径告诉用户）。
    fn log_path() -> PathBuf {
        std::env::temp_dir().join(format!("paperhelper-desktop-{}.log", std::process::id()))
    }

    fn log(msg: &str) {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path())
        {
            let _ = writeln!(f, "[{}] {msg}", crate_time());
        }
    }

    /// 不引入 chrono：用 SystemTime 打印 UNIX 毫秒即可，日志只用于排查顺序问题。
    fn crate_time() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub fn fatal(msg: &str) {
        log(&format!("错误：{msg}"));
        let text = format!("{msg}\n\n日志：{}", log_path().display());
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                wide(&text).as_ptr(),
                wide("PaperHelper 启动失败").as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    fn info_box(msg: &str) {
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                wide(msg).as_ptr(),
                wide("PaperHelper").as_ptr(),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }

    /// 单实例互斥锁：拿到返回 guard；已被占用返回 None。
    struct Singleton(HANDLE);

    impl Drop for Singleton {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    fn acquire_singleton() -> Option<Singleton> {
        let name = wide(MUTEX_NAME);
        let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
        if handle.is_null() {
            log("CreateMutexW 失败，跳过单实例检查");
            return Some(Singleton(handle));
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { CloseHandle(handle) };
            return None;
        }
        Some(Singleton(handle))
    }

    /// 已在运行时：把已有窗口拉到前台（找不到窗口也不报错，只提示）。
    fn focus_existing_window() {
        unsafe {
            let hwnd = FindWindowW(std::ptr::null(), wide(WINDOW_TITLE).as_ptr());
            if !hwnd.is_null() {
                ShowWindow(hwnd, SW_RESTORE);
                SetForegroundWindow(hwnd);
            } else {
                info_box("PaperHelper 已在运行（窗口可能被最小化到了任务栏）。");
            }
        }
    }

    /// 把子进程放进「壳退出即被杀」的 Job Object。句柄有意不关闭：
    /// 进程结束时系统关闭句柄 → 触发 KILL_ON_JOB_CLOSE，杜绝残留后台服务。
    fn attach_kill_on_close(child: &Child) {
        use std::os::windows::io::AsRawHandle;
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                log("CreateJobObjectW 失败：壳退出时可能残留服务进程");
                return;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                log("SetInformationJobObject 失败：壳退出时可能残留服务进程");
                return;
            }
            if AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0 {
                log("AssignProcessToJobObject 失败：壳退出时可能残留服务进程");
            }
        }
    }

    /// 找一个同目录的 CLI 可执行文件（打包版为 paperhelper.exe；开发时可能是 hash 后缀名）。
    fn find_cli_exe(shell_exe: &Path) -> Option<PathBuf> {
        let dir = shell_exe.parent()?;
        let cli = dir.join("paperhelper.exe");
        if cli.is_file() {
            return Some(cli);
        }
        // cargo 开发目录：找名字以 paperhelper- 开头的 exe，排除自己
        let mut hits: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
                    && p.file_stem()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.starts_with("paperhelper-") && s != "paperhelper-desktop")
            })
            .collect();
        hits.sort();
        hits.into_iter().next()
    }

    /// 数据目录：尊重用户已设的 PAPERHELPER_DATA_DIR，否则 `%USERPROFILE%\PaperHelper\.paperhelper`。
    fn resolve_data_dir() -> PathBuf {
        if let Some(v) = std::env::var_os("PAPERHELPER_DATA_DIR") {
            if !v.is_empty() {
                return PathBuf::from(v);
            }
        }
        let home = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join("PaperHelper").join(".paperhelper")
    }

    /// 启动服务子进程，返回（子进程, 实际端口）。
    fn spawn_server(cli: &Path, data_dir: &Path, marker: &Path) -> anyhow::Result<(Child, u16)> {
        let port_file = std::env::temp_dir().join(format!(
            "paperhelper-desktop-{}.port",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&port_file);
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path())?;
        let err_file = log_file.try_clone()?;

        // 是否安装版（安装目录含 uninstall.exe）：决定 Web 端是否提供「一键更新」
        let installed = cli
            .parent()
            .map(|d| d.join("uninstall.exe").is_file())
            .unwrap_or(false);

        let mut cmd = Command::new(cli);
        cmd.arg("web")
            .arg("--port")
            .arg("0")
            .arg("--port-file")
            .arg(&port_file)
            .env("PAPERHELPER_DATA_DIR", data_dir)
            .env("PAPERHELPER_DESKTOP", "1")
            .env("PAPERHELPER_INSTALLED", if installed { "1" } else { "0" })
            .env("PAPERHELPER_UPDATE_MARKER", marker)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(err_file));
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("无法启动 {}：{e}", cli.display()))?;
        attach_kill_on_close(&child);

        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(&port_file) {
                if let Ok(port) = text.trim().parse::<u16>() {
                    return Ok((child, port));
                }
            }
            if let Ok(Some(status)) = child.try_wait() {
                anyhow::bail!("服务进程提前退出（{status}），详情见日志");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        anyhow::bail!("等待服务启动超时（30 秒），详情见日志")
    }

    /// 极简 HTTP POST（不引入 reqwest：只用于关窗时的 interrupt/shutdown）。
    fn post(port: u16, path: &str) {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(800)) else {
            return;
        };
        let _ = stream.set_write_timeout(Some(Duration::from_millis(800)));
        let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(req.as_bytes());
        let mut buf = [0u8; 128];
        let _ = stream.read(&mut buf);
        log(&format!("已请求 {path}（端口 {port}）"));
    }

    /// 优雅关闭服务：先停掉在途任务，再请求退出。
    fn request_shutdown(port: u16) {
        post(port, "/api/interrupt");
        post(port, "/api/shutdown");
    }

    /// 更新标记文件：服务（子进程）写入，桌面壳轮询到后启动更新器。
    fn update_marker_path() -> PathBuf {
        std::env::temp_dir().join(format!("paperhelper-update-{}.marker", std::process::id()))
    }

    /// 读取更新标记（两行：版本号、安装包路径）；内容不完整或安装包不存在返回 None。
    fn read_marker(path: &Path) -> Option<(String, PathBuf)> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut lines = text.lines();
        let version = lines.next()?.trim().to_string();
        let setup = PathBuf::from(lines.next()?.trim());
        if version.is_empty() || setup.as_os_str().is_empty() {
            return None;
        }
        if !setup.is_file() {
            log(&format!("更新标记里的安装包不存在：{}", setup.display()));
            return None;
        }
        Some((version, setup))
    }

    /// 启动更新器：把自己复制到临时目录再运行（安装器要覆盖原 exe，运行中的文件不能占用）。
    fn launch_updater(setup: &Path) -> anyhow::Result<()> {
        let self_exe = std::env::current_exe()?;
        let copy = std::env::temp_dir()
            .join(format!("paperhelper-updater-{}.exe", std::process::id()));
        let _ = std::fs::remove_file(&copy);
        std::fs::copy(&self_exe, &copy).map_err(|e| {
            anyhow::anyhow!("复制更新器失败（{}）：{e}", copy.display())
        })?;
        Command::new(&copy)
            .arg("--run-updater")
            .arg(std::process::id().to_string())
            .arg(&self_exe)
            .arg(setup)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| anyhow::anyhow!("启动更新器失败：{e}"))?;
        log(&format!(
            "更新器已启动（{} → 安装包 {}）",
            copy.display(),
            setup.display()
        ));
        Ok(())
    }

    /// 等待进程退出（最多 timeout）；进程已不存在也算退出。
    fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
        /// SYNCHRONIZE（PROCESS_ACCESS_RIGHTS），避免为一个常量开启额外 feature。
        const SYNCHRONIZE: u32 = 0x0010_0000;
        unsafe {
            let h = OpenProcess(SYNCHRONIZE, 0, pid);
            if h.is_null() {
                return true;
            }
            let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
            let r = WaitForSingleObject(h, ms);
            CloseHandle(h);
            r == WAIT_OBJECT_0
        }
    }

    /// 更新器模式（`--run-updater <壳PID> <壳exe> <安装包> [版本]`）：
    /// 等旧壳退出 → 静默安装 → 重新启动 → 自删临时副本。返回值为进程退出码。
    pub fn run_updater(args: &[String]) -> i32 {
        log("更新器启动");
        if args.len() < 3 {
            log("更新器参数不足，退出");
            return 2;
        }
        let pid: u32 = args[0].parse().unwrap_or(0);
        let shell = PathBuf::from(&args[1]);
        let setup = PathBuf::from(&args[2]);
        let version = args.get(3).cloned().unwrap_or_default();

        if pid != 0 {
            log(&format!("等待旧桌面壳（PID {pid}）退出"));
            if !wait_for_process_exit(pid, Duration::from_secs(120)) {
                log("等待超时，继续安装");
            }
        }
        std::thread::sleep(Duration::from_millis(500)); // 留点时间让文件句柄释放

        let mut installed = false;
        if setup.is_file() {
            log(&format!("静默安装 v{version}：{}", setup.display()));
            match Command::new(&setup).arg("/S").status() {
                Ok(st) if st.success() => installed = true,
                Ok(st) => log(&format!("安装包返回 {st}")),
                Err(e) => log(&format!("运行安装包失败：{e}")),
            }
            if installed {
                let _ = std::fs::remove_file(&setup);
            }
        } else {
            log(&format!("安装包不存在：{}", setup.display()));
        }

        // 不管安装结果如何都重新启动，避免用户「点了更新后什么都没打开」
        if shell.is_file() {
            log(&format!("重新启动：{}", shell.display()));
            if let Err(e) = Command::new(&shell).spawn() {
                log(&format!("重新启动失败：{e}"));
            }
        }

        // 自删：借一个短暂存活的 cmd 在更新器退出后删除临时副本
        if let Ok(me) = std::env::current_exe() {
            if me
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with("paperhelper-updater-"))
            {
                let cmdline = format!("ping -n 2 127.0.0.1 >nul & del /f /q \"{}\"", me.display());
                let _ = Command::new("cmd")
                    .arg("/C")
                    .arg(cmdline)
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn();
            }
        }
        log("更新器退出");
        0
    }

    pub fn run() -> anyhow::Result<()> {
        log("桌面壳启动");

        if acquire_singleton().is_none() {
            log("检测到已有实例，聚焦已有窗口");
            focus_existing_window();
            return Ok(());
        }

        let shell_exe = std::env::current_exe()?;
        let cli = find_cli_exe(&shell_exe).ok_or_else(|| {
            anyhow::anyhow!(
                "找不到 paperhelper.exe（应与 paperhelper-desktop.exe 在同一目录）：{}",
                shell_exe.display()
            )
        })?;
        let data_dir = resolve_data_dir();
        log(&format!("CLI={} 数据目录={}", cli.display(), data_dir.display()));

        let marker = update_marker_path();
        let _ = std::fs::remove_file(&marker);
        let (mut child, port) = spawn_server(&cli, &data_dir, &marker)?;
        log(&format!("服务已就绪：端口 {port}"));

        let mut event_loop = EventLoop::new();
        let icon = Icon::from_rgba(ICON_32.to_vec(), 32, 32).ok();
        let window = WindowBuilder::new()
            .with_title(WINDOW_TITLE)
            .with_inner_size(LogicalSize::new(1360.0, 860.0))
            .with_min_inner_size(LogicalSize::new(960.0, 600.0))
            .with_window_icon(icon)
            .build(&event_loop)?;

        let url = format!("http://127.0.0.1:{port}/");
        let mut builder = WebViewBuilder::new().with_url(&url);
        // 调试开关：PAPERHELPER_DEVTOOLS=1 时可用 F12 打开开发者工具
        if std::env::var_os("PAPERHELPER_DEVTOOLS").is_some() {
            builder = builder.with_devtools(true);
        }
        let _webview = builder.build(&window).map_err(|e| {
            anyhow::anyhow!(
                "无法创建 WebView2 窗口：{e}\n\n请安装 Microsoft Edge WebView2 运行时后重试：\nhttps://go.microsoft.com/fwlink/p/?LinkId=2124703"
            )
        })?;

        event_loop.run_return(|event, _, control_flow| {
            *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500));
            if let Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } = event
            {
                log("窗口关闭请求：通知服务退出");
                request_shutdown(port);
                *control_flow = ControlFlow::Exit;
                return;
            }
            // 服务端写入更新标记 → 启动更新器并退出（更新器负责安装与重启）
            if marker.is_file() {
                let parsed = read_marker(&marker);
                let _ = std::fs::remove_file(&marker);
                match parsed {
                    Some((version, setup)) => {
                        log(&format!("收到更新请求：v{version}"));
                        match launch_updater(&setup) {
                            Ok(()) => {
                                request_shutdown(port);
                                *control_flow = ControlFlow::Exit;
                            }
                            Err(e) => {
                                log(&format!("启动更新器失败：{e:#}"));
                                info_box(&format!(
                                    "无法启动更新程序：{e:#}\n\n请到下载页手动更新：\n{}",
                                    "https://github.com/Ru645/paperhelper/releases/latest"
                                ));
                            }
                        }
                    }
                    None => log("更新标记内容不完整，已忽略"),
                }
            }
        });

        // 等子进程自己退出（最多 5 秒），避免留下后台服务
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    log(&format!("服务已退出：{status}"));
                    break;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                _ => {
                    log("服务未在 5 秒内退出，强制结束");
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("paperhelper-desktop-{}.port", std::process::id())),
        );
        let _ = std::fs::remove_file(&marker);
        log("桌面壳退出");
        Ok(())
    }
}
