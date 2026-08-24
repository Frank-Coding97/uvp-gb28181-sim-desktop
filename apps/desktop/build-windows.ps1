param(
  [string]$FfmpegBin = $env:FFMPEG_BIN
)

$ErrorActionPreference = "Stop"
$DesktopDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RuntimeDir = Join-Path $DesktopDir "runtime\ffmpeg"

if ([string]::IsNullOrWhiteSpace($FfmpegBin)) {
  $resolved = Get-Command "ffmpeg.exe" -ErrorAction SilentlyContinue
  if ($null -ne $resolved) { $FfmpegBin = $resolved.Source }
}

if ([string]::IsNullOrWhiteSpace($FfmpegBin) -or !(Test-Path -LiteralPath $FfmpegBin)) {
  throw "发布包必须内嵌 FFmpeg。请设置 FFMPEG_BIN=绝对路径\ffmpeg.exe，或把 ffmpeg.exe 放入 PATH。"
}

New-Item -ItemType Directory -Force -Path $RuntimeDir | Out-Null
$StagedFfmpeg = Join-Path $RuntimeDir "ffmpeg.exe"
$CapabilityReport = Join-Path $DesktopDir "..\..\target\release\preview-ffmpeg-capabilities.json"
Copy-Item -Force -LiteralPath $FfmpegBin -Destination $StagedFfmpeg

$version = (& $StagedFfmpeg -version 2>$null | Select-Object -First 1)
$hwaccels = (& $StagedFfmpeg -hide_banner -hwaccels 2>&1 | Out-String)
$decoders = (& $StagedFfmpeg -hide_banner -decoders 2>&1 | Select-String -Pattern "h264|hevc|d3d11va|dxva2" | Out-String)
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $CapabilityReport) | Out-Null
@{
  schema_version = 1
  checked_at = (Get-Date).ToUniversalTime().ToString("o")
  host = @{ os = [System.Environment]::OSVersion.VersionString; machine = $env:PROCESSOR_ARCHITECTURE }
  ffmpeg = @{ path = $FfmpegBin; version = $version; hardware_accelerators = $hwaccels; matching_decoders = $decoders }
  expected_backend = "d3d11va/dxva2"
} | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 -Path $CapabilityReport
Write-Host "FFmpeg 能力报告已生成：$CapabilityReport"

try {
  Push-Location $DesktopDir
  npm run tauri build -- --bundles nsis,msi --config '{"bundle":{"resources":["runtime/ffmpeg/*"]}}'
}
finally {
  Pop-Location
  Remove-Item -Force -ErrorAction SilentlyContinue $StagedFfmpeg
}

Write-Host "Windows 安装包已生成；FFmpeg 已作为应用资源内嵌。"
