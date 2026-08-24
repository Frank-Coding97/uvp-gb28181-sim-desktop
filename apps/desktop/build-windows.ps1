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
Copy-Item -Force -LiteralPath $FfmpegBin -Destination $StagedFfmpeg

try {
  Push-Location $DesktopDir
  npm run tauri build -- --bundles nsis,msi --config '{"bundle":{"resources":["runtime/ffmpeg/*"]}}'
}
finally {
  Pop-Location
  Remove-Item -Force -ErrorAction SilentlyContinue $StagedFfmpeg
}

Write-Host "Windows 安装包已生成；FFmpeg 已作为应用资源内嵌。"
