#!/usr/bin/env bash
# 生成用于 C 档联调的真实 H.264 Annex B 测试码流(测试图案,8 秒)。
# 需要 ffmpeg。产物:assets/media/test.h264(不入库,按需重建)。
set -e
cd "$(dirname "$0")"
ffmpeg -y -f lavfi -i "testsrc=size=640x480:rate=25:duration=8" \
  -c:v libx264 -profile:v baseline -level 3.0 -pix_fmt yuv420p \
  -g 25 -keyint_min 25 -bsf:v h264_mp4toannexb -f h264 test.h264
echo "已生成 test.h264"
