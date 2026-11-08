param(
  [string]$FfmpegBin = $env:FFMPEG_BIN,
  [string]$FfprobeBin = $env:FFPROBE_BIN
)

$ErrorActionPreference = "Stop"
$DesktopDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RuntimeDir = Join-Path $DesktopDir "runtime\ffmpeg"

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

try {
  Push-Location $DesktopDir
  # `--config` 对数组是**整体替换**，不是追加：必须把内置演示视频一并列回，
  # 否则 runtime/ffmpeg/* 会把 resources/builtin-camera.mp4 挤出交付包。
  npm run tauri build -- --bundles nsis,msi --config '{"bundle":{"resources":["resources/builtin-camera.mp4","runtime/ffmpeg/*"]}}'
}
finally {
  Pop-Location
  Remove-Item -Force -ErrorAction SilentlyContinue $StagedFfmpeg, $StagedFfprobe
}

Write-Host "Windows 安装包已生成；ffmpeg.exe 与 ffprobe.exe 已作为应用资源内嵌（resources\runtime\ffmpeg\）。"
