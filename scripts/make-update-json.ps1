# 生成一键更新清单 update.json（作为 Release 资产，供 PaperHelper 应用内更新使用）。
#
# 用法（CI 或本地出包后）：
#   pwsh scripts/make-update-json.ps1 -Tag v0.2.0 `
#     -SetupPath dist-win/paperhelper-setup.exe `
#     -ZipPath   dist-win/paperhelper-windows-x64.zip `
#     -OutPath   dist-win/update.json
#
# 要点：
# - 版本号取自 tag，且必须与 Cargo.toml 的 version 一致（防止「tag 打了、版本没改」的发版事故）；
# - sha256 由 Get-FileHash 计算，写入 JSON（小写），客户端下载后会校验；
# - JSON 以 UTF-8 无 BOM 写入（serde_json 不接受 BOM）。
param(
  [Parameter(Mandatory = $true)][string]$Tag,
  [Parameter(Mandatory = $true)][string]$SetupPath,
  [Parameter(Mandatory = $true)][string]$ZipPath,
  [string]$OutPath = "dist-win/update.json",
  [string]$Repo = "Ru645/paperhelper"
)

$ErrorActionPreference = "Stop"

if (-not $Tag.StartsWith("v")) { $Tag = "v$Tag" }
$version = $Tag.TrimStart("v")

$cargo = Get-Content -LiteralPath "Cargo.toml" -Raw
if ($cargo -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
  throw "无法从 Cargo.toml 读取版本号"
}
$cargoVersion = $Matches[1]
if ($cargoVersion -ne $version) {
  throw "版本不一致：tag 是 $Tag，Cargo.toml 是 $cargoVersion；请先更新 Cargo.toml 再打 tag"
}

foreach ($f in @($SetupPath, $ZipPath)) {
  if (-not (Test-Path -LiteralPath $f)) { throw "缺少打包产物：$f" }
}

function New-Asset([string]$File, [string]$Url) {
  $item = Get-Item -LiteralPath $File
  $hash = (Get-FileHash -LiteralPath $File -Algorithm SHA256).Hash.ToLowerInvariant()
  [ordered]@{
    url    = $Url
    size   = [long]$item.Length
    sha256 = $hash
  }
}

$base = "https://github.com/$Repo/releases/download/$Tag"
$manifest = [ordered]@{
  schema      = 1
  version     = $version
  released_at = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd")
  notes_url   = "https://github.com/$Repo/releases/tag/$Tag"
  setup       = New-Asset $SetupPath "$base/paperhelper-setup.exe"
  zip         = New-Asset $ZipPath "$base/paperhelper-windows-x64.zip"
}

$json = $manifest | ConvertTo-Json -Depth 4
[System.IO.File]::WriteAllText($OutPath, $json, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "已生成 $OutPath（v$version；setup $($manifest.setup.size) 字节 sha256=$($manifest.setup.sha256.Substring(0, 12))…）"
