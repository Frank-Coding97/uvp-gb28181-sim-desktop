param(
  [string]$FfmpegBin = $env:FFMPEG_BIN,
  [string]$FfprobeBin = $env:FFPROBE_BIN
)

$ErrorActionPreference = "Stop"
$DesktopDir = Split-Path -Parent $MyInvocation.MyCommand.Path
# ⛔ 必须是 src-tauri\runtime\ffmpeg，不是 desktop\runtime\ffmpeg。
#    `bundle.resources` 的路径基准是 tauri.conf.json 所在目录（src-tauri），
#    放在上级目录时 `runtime/ffmpeg/*` 匹配不到任何文件，tauri-build 直接报
#    `glob pattern runtime/ffmpeg/* path not found or didn't match any files.` 并中断编译
#    （2026-11-08 在 52 上实测，前端已构建完，卡在 Rust 编译阶段）。
$RuntimeDir = Join-Path $DesktopDir "src-tauri\runtime\ffmpeg"

# ffmpeg：主干媒体能力（容器转封装、摄像头/屏幕采集、快照、录像收尾）。
if ([string]::IsNullOrWhiteSpace($FfmpegBin)) {
  $resolved = Get-Command "ffmpeg.exe" -ErrorAction SilentlyContinue
  if ($null -ne $resolved) { $FfmpegBin = $resolved.Source }
}

if ([string]::IsNullOrWhiteSpace($FfmpegBin) -or !(Test-Path -LiteralPath $FfmpegBin)) {
  throw "发布包必须内嵌 FFmpeg。请设置 FFMPEG_BIN=绝对路径\ffmpeg.exe，或把 ffmpeg.exe 放入 PATH。"
}

# ffprobe：录像音轨校验与历史回放时间轴。缺它不会让录像失败，但会静默跳过音轨核验，
# 交付包必须一起带——这是 Windows 端曾经"包内只有 ffmpeg"留下的缺口。
if ([string]::IsNullOrWhiteSpace($FfprobeBin)) {
  $sibling = Join-Path (Split-Path -Parent $FfmpegBin) "ffprobe.exe"
  if (Test-Path -LiteralPath $sibling) {
    $FfprobeBin = $sibling
  }
  else {
    $resolvedProbe = Get-Command "ffprobe.exe" -ErrorAction SilentlyContinue
    if ($null -ne $resolvedProbe) { $FfprobeBin = $resolvedProbe.Source }
  }
}

if ([string]::IsNullOrWhiteSpace($FfprobeBin) -or !(Test-Path -LiteralPath $FfprobeBin)) {
  throw "发布包必须内嵌 ffprobe。它应与 ffmpeg.exe 同目录，或设置 FFPROBE_BIN=绝对路径\ffprobe.exe。"
}

New-Item -ItemType Directory -Force -Path $RuntimeDir | Out-Null
$StagedFfmpeg = Join-Path $RuntimeDir "ffmpeg.exe"
$StagedFfprobe = Join-Path $RuntimeDir "ffprobe.exe"
$CapabilityReport = Join-Path $DesktopDir "..\..\target\release\preview-ffmpeg-capabilities.json"
Copy-Item -Force -LiteralPath $FfmpegBin -Destination $StagedFfmpeg
Copy-Item -Force -LiteralPath $FfprobeBin -Destination $StagedFfprobe

# shared 构建的 ffmpeg 依赖同目录 DLL（avcodec-60.dll / avformat-60.dll 等）。
# 只拷两个 exe 的话，用户机上 ffmpeg 会因缺 DLL 直接起不来——而包内自检只会报
# "ffmpeg 未就绪"，看不出是缺文件。静态构建（gpl-static）没有 DLL，这段自然空转。
$StagedDlls = @()
$DllSource = Split-Path -Parent $FfmpegBin
$SiblingDlls = @(Get-ChildItem -LiteralPath $DllSource -Filter "*.dll" -File -ErrorAction SilentlyContinue)
foreach ($dll in $SiblingDlls) {
  $dest = Join-Path $RuntimeDir $dll.Name
  Copy-Item -Force -LiteralPath $dll.FullName -Destination $dest
  $StagedDlls += $dest
}
if ($StagedDlls.Count -gt 0) {
  Write-Host "检测到 shared 构建的 FFmpeg，已随包内嵌 $($StagedDlls.Count) 个依赖 DLL。"
}

$version = (& $StagedFfmpeg -version 2>$null | Select-Object -First 1)
$probeVersion = (& $StagedFfprobe -version 2>$null | Select-Object -First 1)
if ([string]::IsNullOrWhiteSpace($probeVersion)) {
  throw "内嵌的 ffprobe 无法执行：$StagedFfprobe"
}
$hwaccels = (& $StagedFfmpeg -hide_banner -hwaccels 2>&1 | Out-String)
$decoders = (& $StagedFfmpeg -hide_banner -decoders 2>&1 | Select-String -Pattern "h264|hevc|d3d11va|dxva2" | Out-String)
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $CapabilityReport) | Out-Null
@{
  schema_version = 1
  checked_at = (Get-Date).ToUniversalTime().ToString("o")
  host = @{ os = [System.Environment]::OSVersion.VersionString; machine = $env:PROCESSOR_ARCHITECTURE }
  ffmpeg = @{ path = $FfmpegBin; version = $version; hardware_accelerators = $hwaccels; matching_decoders = $decoders }
  ffprobe = @{ path = $FfprobeBin; version = $probeVersion }
  expected_backend = "d3d11va/dxva2"
} | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 -Path $CapabilityReport
Write-Host "FFmpeg/ffprobe 能力报告已生成：$CapabilityReport"

# `--config` 对数组是**整体替换**，不是追加：必须把内置演示视频一并列回，
# 否则 runtime/ffmpeg/* 会把 resources/builtin-camera.mp4 挤出交付包。
#
# ⛔ 不能写成内联 JSON 字符串：Windows 上 PowerShell → npm.cmd → node 这三层会把双引号吃光，
#    tauri 收到的是 {bundle:{resources:[...]}}，直接报
#    `key must be a string at line 1 column 2`，构建根本没开始（2026-11-08 在 52 上实测）。
#    改成写临时 JSON 文件再传路径，绕开所有引号问题。
# ⛔ 必须用 File.WriteAllText（UTF-8 无 BOM）。PS 5.1 的 Set-Content -Encoding UTF8 会写 BOM，
#    serde_json 见到 BOM 会报 "expected value"。
$BundleConfig = Join-Path $env:TEMP "uvp-desktop-bundle-config.json"
# 在线升级：只有发版时才生成 updater 制品（需要签名私钥），日常/PR 构建不签，
# 否则本地 `tauri build` 会因为没配私钥直接失败。
$BuildUpdater = ($env:UVP_UPDATER -eq "1")
if ($BuildUpdater -and
    [string]::IsNullOrWhiteSpace($env:TAURI_SIGNING_PRIVATE_KEY) -and
    [string]::IsNullOrWhiteSpace($env:TAURI_SIGNING_PRIVATE_KEY_PATH)) {
  throw "UVP_UPDATER=1 需要签名私钥：请设置 TAURI_SIGNING_PRIVATE_KEY（内容）或 TAURI_SIGNING_PRIVATE_KEY_PATH（文件路径）。"
}
$BundlePayload = @{
  resources = @("resources/builtin-camera.mp4", "runtime/ffmpeg/*")
}
# ⛔ 这里**故意不**注入 createUpdaterArtifacts：tauri build 内置签名会去交互读私钥密码，
#    在无 TTY 的 ssh / CI 里直接挂住（安装包已经打完，进程却永远不退出，2026-11-12 在 52 实测）。
#    改成构建结束后自己调 `tauri signer sign`，用 cmd 传空密码（见文件末尾）。
$BundleConfigJson = @{ bundle = $BundlePayload } | ConvertTo-Json -Depth 5
[System.IO.File]::WriteAllText($BundleConfig, $BundleConfigJson)

try {
  Push-Location $DesktopDir
  # ⛔ 别写成 `npm run tauri build -- --bundles ...`：npm 会把 `--` 一并交给 tauri CLI，
  #    后续参数被当成「要转发给 cargo 的 args」，cargo 直接报
  #    "unexpected argument 'nsis,msi'"（看着像参数名写错，其实是多了一层 --）。
  #    这里直接调 tauri CLI，绕开 npm 的参数重组。
  # ⛔ 只出 NSIS（-setup.exe）：在线升级只支持 NSIS 安装包。msi 走不了 updater，
  #    用 msi 装的用户升级时会被静默装成第二份。1.4.0 起不再发布 msi。
  # ⛔ bundles 值必须加引号：PowerShell 会把裸写的逗号当数组分隔符拆成两个参数。
  & '.\node_modules\.bin\tauri.cmd' build --bundles 'nsis' --config $BundleConfig
  # ⛔ 原生命令失败不会让 PowerShell 抛异常（$ErrorActionPreference 管不到），
  #    必须自己看退出码，否则「构建失败」会被后面的「已生成」误报成成功。
  if ($LASTEXITCODE -ne 0) {
    throw "tauri build 失败（exit $LASTEXITCODE），安装包未生成。"
  }
}
finally {
  Pop-Location
  Remove-Item -Force -ErrorAction SilentlyContinue $StagedFfmpeg, $StagedFfprobe
  if ($StagedDlls.Count -gt 0) {
    Remove-Item -Force -ErrorAction SilentlyContinue $StagedDlls
  }
}

# 收尾提示：把「有没有带 DLL」说清楚，免得下一轮误判包内 ffmpeg 是完整的。
$dllNote = ""
if ($StagedDlls.Count -gt 0) { $dllNote = "，另含 $($StagedDlls.Count) 个依赖 DLL" }
Write-Host "Windows 安装包已生成；ffmpeg.exe 与 ffprobe.exe 已作为应用资源内嵌（resources\runtime\ffmpeg\${dllNote}）。"

if ($BuildUpdater) {
  # updater 在 Windows 上就是把 -setup.exe 本身当更新包，签名文件必须和它并排存在。
  $setup = Get-ChildItem -Path (Join-Path $DesktopDir "..\..\target\release\bundle\nsis") -Filter "*-setup.exe" -File -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
  if ($null -eq $setup) {
    throw "未找到 NSIS 安装包（*-setup.exe），无法确认在线升级产物。"
  }
  $sig = "$($setup.FullName).sig"
  Remove-Item -Force -ErrorAction SilentlyContinue $sig

  # 私钥优先用文件（cmd 不适合传多行内容）；只有内容时落一个临时文件。
  $keyFile = $env:TAURI_SIGNING_PRIVATE_KEY_PATH
  $tempKey = $false
  if ([string]::IsNullOrWhiteSpace($keyFile) -or !(Test-Path -LiteralPath $keyFile)) {
    $keyFile = Join-Path $env:TEMP "uvp-updater-sign.key"
    [System.IO.File]::WriteAllText($keyFile, $env:TAURI_SIGNING_PRIVATE_KEY)
    $tempKey = $true
  }
  # ⛔ 环境变量和 -f 不能同时给：CLI 会报
  #    `the argument '--private-key-path' cannot be used with '--private-key'`
  #    （2026-11-12 在 52 上实测，安装包已生成却判为构建失败）。
  Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY -ErrorAction SilentlyContinue

  # ⛔ 必须经 cmd 传 `-p ""`：PowerShell 会把空字符串参数丢掉，CLI 于是转去交互读密码，
  #    在无 TTY 的 ssh / CI 里永久挂住（2026-11-12 在 52 上踩过）。写个临时 .cmd 绕开引号问题。
  $signerJs = Join-Path $DesktopDir "node_modules\@tauri-apps\cli\tauri.js"
  if (!(Test-Path -LiteralPath $signerJs)) { throw "找不到 tauri CLI：$signerJs" }
  $nodeExe = (Get-Command node -ErrorAction Stop).Source
  $signCmd = Join-Path $env:TEMP "uvp-updater-sign.cmd"
  $cmdLines = @(
    "@echo off",
    "`"$nodeExe`" `"$signerJs`" signer sign -f `"$keyFile`" -p `"`" `"$($setup.FullName)`""
  )
  [System.IO.File]::WriteAllLines($signCmd, $cmdLines, (New-Object System.Text.UTF8Encoding($false)))
  & cmd /c $signCmd
  if ($LASTEXITCODE -ne 0) { throw "升级包签名失败（exit $LASTEXITCODE）：$($setup.Name)" }
  if ($tempKey) { Remove-Item -Force -ErrorAction SilentlyContinue $keyFile }

  if (!(Test-Path -LiteralPath $sig) -or (Get-Item -LiteralPath $sig).Length -le 0) {
    throw "升级签名缺失：$sig。请确认签名私钥与公钥（tauri.conf.json plugins.updater.pubkey）配对。"
  }
  Write-Host "在线升级包已生成：$($setup.Name)（签名 $((Get-Item -LiteralPath $sig).Length) 字节）"
}
