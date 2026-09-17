<#
.SYNOPSIS
  构建 Windows 免安装包（paperhelper.exe + paperhelper-desktop.exe + 内置 Python/PyMuPDF），
  并在装有 NSIS（makensis）时顺带生成安装包 setup.exe。

.DESCRIPTION
  产物（默认在 dist-win\ 下）：
    runtime\                      免安装目录（zip 的内容）
      paperhelper.exe             CLI / Web 服务
      paperhelper-desktop.exe     桌面窗口版
      python\                     内置 Windows 解释器（python.org embeddable）+ PyMuPDF
    paperhelper-windows-x64.zip   解压即用
    paperhelper-setup.exe         安装包（需 makensis，per-user 安装到 %LOCALAPPDATA%\PaperHelper）

.EXAMPLE
  pwsh scripts/package-windows.ps1
  pwsh scripts/package-windows.ps1 -PythonVersion 3.13.13 -OutDir dist-win -SkipInstaller
#>
[CmdletBinding()]
param(
    [string]$RepoRoot = "",
    [string]$OutDir = "dist-win",
    [string]$PythonVersion = "3.13.13",
    [string]$PythonMirror = "",
    [string]$PyMuPDF = "pymupdf",
    [string]$PipIndex = "https://pypi.tuna.tsinghua.edu.cn/simple",
    [string]$Version = "",
    [switch]$SkipBuild,
    [switch]$SkipZip,
    [switch]$SkipInstaller
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
# 原生命令（cargo/pip/python）往 stderr 写提示不算失败（pwsh 7.3+ 可能把它当错误）
$PSNativeCommandUseErrorActionPreference = $false

function Info([string]$msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Fail([string]$msg) { Write-Host "错误：$msg" -ForegroundColor Red; exit 1 }

# 打 zip（自己写条目名，保证用 "/" 分隔——Compress-Archive 会写反斜杠，跨平台解压会散架）
function New-SpecZip([string]$srcDir, [string]$zipPath) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $srcFull = (Get-Item $srcDir).FullName
    $zip = [System.IO.Compression.ZipFile]::Open($zipPath, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($f in Get-ChildItem -Path $srcFull -Recurse -File) {
            $rel = $f.FullName.Substring($srcFull.Length + 1).Replace('\', '/')
            [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                $zip, $f.FullName, $rel, [System.IO.Compression.CompressionLevel]::Optimal)
        }
    } finally { $zip.Dispose() }
}

# 注意：$PSScriptRoot 在 Windows PowerShell 5.1 的参数默认值里可能为空，所以放到这里算
if ([string]::IsNullOrWhiteSpace($RepoRoot)) {
    $self = $MyInvocation.MyCommand.Path
    if ([string]::IsNullOrWhiteSpace($self)) { $self = $PSScriptRoot }
    if ([string]::IsNullOrWhiteSpace($self)) { Fail "无法确定脚本位置，请用 -RepoRoot 指定仓库根目录" }
    $RepoRoot = Split-Path -Parent (Split-Path -Parent $self)
}

# ---- 版本号（用于安装包显示；优先参数 > git tag > Cargo.toml）----
if ([string]::IsNullOrWhiteSpace($Version)) {
    if ($env:GITHUB_REF_NAME -match '^v?(\d+\.\d+\.\d+)') {
        $Version = $Matches[1]
    } else {
        Push-Location $RepoRoot
        $Version = (Select-String -Path "Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
        Pop-Location
    }
}
$Version4 = ($Version -replace '[^0-9\.]', '')
if ($Version4 -notmatch '^[0-9]') { $Version4 = "0.1.0" }
while (($Version4 -split '\.').Count -lt 4) { $Version4 += ".0" }
Info "版本：$Version（VIProductVersion=$Version4）"

$stage = Join-Path $RepoRoot (Join-Path $OutDir "runtime")
$zipPath = Join-Path $RepoRoot (Join-Path $OutDir "paperhelper-windows-x64.zip")
$setupPath = Join-Path $RepoRoot (Join-Path $OutDir "paperhelper-setup.exe")
$pyDir = Join-Path $stage "python"
$sitePkgs = Join-Path $pyDir "Lib\site-packages"

# ---- 1. 编译 release ----
if (-not $SkipBuild) {
    Info "cargo build --release"
    Push-Location $RepoRoot
    & cargo build --release -p paperhelper --bins
    if ($LASTEXITCODE -ne 0) { Pop-Location; Fail "cargo build 失败" }
    Pop-Location
} else {
    Info "跳过编译（-SkipBuild）"
}

$rel = Join-Path $RepoRoot "target\release"
foreach ($exe in @("paperhelper.exe", "paperhelper-desktop.exe")) {
    if (-not (Test-Path (Join-Path $rel $exe))) { Fail "缺少 $rel\$exe（先跑 cargo build --release）" }
}

# ---- 2. 组装 runtime 目录 ----
Info "组装 $stage"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage | Out-Null
Copy-Item (Join-Path $rel "paperhelper.exe") $stage
Copy-Item (Join-Path $rel "paperhelper-desktop.exe") $stage
# 上手文档（生成物已入库；改内容后重新生成：python3 scripts/make_quickstart.py）
$quickstart = Join-Path $RepoRoot "快速开始.html"
if (-not (Test-Path $quickstart)) { Fail "缺少 快速开始.html（在仓库根目录运行 python3 scripts/make_quickstart.py 生成）" }
Copy-Item $quickstart $stage
New-Item -ItemType Directory -Path $pyDir | Out-Null

# ---- 3. 内置 Python（python.org embeddable）----
$pyUrl = "https://www.python.org/ftp/python/$PythonVersion/python-$PythonVersion-embed-amd64.zip"
if (-not [string]::IsNullOrWhiteSpace($PythonMirror)) {
    $pyUrl = "$($PythonMirror.TrimEnd('/'))/$PythonVersion/python-$PythonVersion-embed-amd64.zip"
}
$tmp = Join-Path $env:TEMP "paperhelper-pack-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $pyZip = Join-Path $tmp "python-embed.zip"
    Info "下载 $pyUrl"
    Invoke-WebRequest -Uri $pyUrl -OutFile $pyZip -UseBasicParsing
    Expand-Archive -Path $pyZip -DestinationPath $pyDir -Force

    # 让 site-packages 生效（embeddable 默认 ._pth 不带 site-packages，且注释了 import site）
    $pth = Get-ChildItem -Path $pyDir -Filter "python*._pth" | Select-Object -First 1
    if (-not $pth) { Fail "内置 Python 缺 pythonXX._pth（$pyDir）" }
    $zipName = ($pth.Name -replace '\._pth$', '.zip')
    @(
        $zipName
        "."
        "Lib\site-packages"
        "import site"
    ) | Set-Content -Path $pth.FullName -Encoding ASCII
    New-Item -ItemType Directory -Path $sitePkgs -Force | Out-Null

    # ---- 4. 下载 PyMuPDF wheel 并解包（避免依赖内嵌 Python 的 pip/网络配置）----
    if (-not (Get-Command python -ErrorAction SilentlyContinue)) {
        Fail "PATH 里没有 python（打包脚本用它执行 pip download，下载 Windows 版 wheel）"
    }
    Info "下载 $PyMuPDF（win_amd64 / cp3$($PythonVersion.Split('.')[1])，镜像 $PipIndex）"
    $wheelDir = Join-Path $tmp "wheels"
    New-Item -ItemType Directory -Path $wheelDir | Out-Null
    $pyTag = $PythonVersion.Split('.')[0..1] -join '.'
    & python -m pip download --only-binary=:all: --no-deps `
        --platform win_amd64 --implementation cp --python-version $pyTag `
        -i $PipIndex --dest $wheelDir $PyMuPDF
    if ($LASTEXITCODE -ne 0) { Fail "pip download 失败（可换 -PipIndex 镜像重试）" }
    foreach ($whl in Get-ChildItem -Path $wheelDir -Filter "*.whl") {
        Info "解包 $($whl.Name)"
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        [System.IO.Compression.ZipFile]::ExtractToDirectory($whl.FullName, $sitePkgs)
    }
    # PyMuPDF 的 mupdf-devel 是 C 头文件/静态库（6MB+），运行用不到
    $devel = Join-Path $sitePkgs "pymupdf\mupdf-devel"
    if (Test-Path $devel) { Remove-Item $devel -Recurse -Force -ErrorAction SilentlyContinue }

    # ---- 5. 验证内置 Python 能 import PyMuPDF ----
    Info "验证内置 Python：import pymupdf"
    $pyOut = @(& (Join-Path $pyDir "python.exe") -c "import pymupdf; print('PYMUPDF=' + str(pymupdf.__version__))" 2>&1)
    $pyVer = ($pyOut | Where-Object { $_ -match '^PYMUPDF=' } | Select-Object -First 1) -replace '^PYMUPDF=', ''
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($pyVer)) {
        Write-Host ($pyOut -join "`n") -ForegroundColor DarkGray
        Fail "内置 Python 无法 import pymupdf"
    }
    Info "内置 PyMuPDF $pyVer"
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

# ---- 6. 压缩免安装包 ----
if (-not $SkipZip) {
    Info "压缩 $zipPath"
    Remove-Item $zipPath -Force -ErrorAction SilentlyContinue
    New-SpecZip -srcDir $stage -zipPath $zipPath
    $mb = [math]::Round((Get-Item $zipPath).Length / 1MB, 1)
    Info "zip 完成：$zipPath（$mb MB）"
}

# ---- 7. 安装包（可选，需要 NSIS）----
if (-not $SkipInstaller) {
    $makensis = Get-Command makensis -ErrorAction SilentlyContinue
    if ($makensis) {
        $makensisExe = $makensis.Source
    } else {
        # chocolatey 安装后当前会话 PATH 可能没刷新，兜底找默认安装目录
        $makensisExe = $null
        foreach ($cand in @("${env:ProgramFiles(x86)}\NSIS\makensis.exe", "$env:ProgramFiles\NSIS\makensis.exe")) {
            if (Test-Path $cand) { $makensisExe = $cand; break }
        }
    }
    if ([string]::IsNullOrWhiteSpace($makensisExe)) {
        Write-Host "未找到 makensis（NSIS），跳过安装包；可 choco install nsis -y 后重试" -ForegroundColor Yellow
    } else {
        $nsi = Join-Path $RepoRoot "packaging\windows\installer.nsi"
        Info "makensis $nsi"
        & $makensisExe `
            "-DVERSION=$Version" `
            "-DVI4=$Version4" `
            "-DDIST=$stage" `
            "-DOUT=$setupPath" `
            "-DICON=$(Join-Path $RepoRoot 'assets\icon.ico')" `
            $nsi
        if ($LASTEXITCODE -ne 0) { Fail "makensis 失败" }
        $mb = [math]::Round((Get-Item $setupPath).Length / 1MB, 1)
        Info "安装包完成：$setupPath（$mb MB）"
    }
}

Info "打包结束"
