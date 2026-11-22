#!/usr/bin/env bash
# Percy6Stars by Ahmet Dedeler (https://github.com/Ahmet-Dedeler, https://ahmetdedeler.com)
# Fork of PercyStar by rpatty-dev (https://github.com/rpatty-dev/PercyStar).
# Source: https://github.com/Ahmet-Dedeler/Percy6Stars
# License: PercyStar Non-Commercial Attribution & Copyleft License v1.2 (see LICENSE)
# If you copy, vendor or port this file, keep this header and credit both projects in your README.
#
# Rebuilds the README demo media from the made-up sample set (no game files involved).
#
#   bash scripts/readme_media.sh
#
# 1. generates the sample models/textures (scripts/demo_data.py)
# 2. records the real Percy6Stars UI offscreen (src/demo_capture.rs, no window, no sound)
# 3. encodes animated WebPs, a still and a short MP4 into assets/
set -euo pipefail
cd "$(dirname "$0")/.."
WORK="${SIXSTARS_DEMO_WORK:-${TMPDIR:-/tmp}/percy6stars-readme-media}"
mkdir -p "$WORK"
if [ -z "${SKIP_RECORD:-}" ]; then
uv run -q scripts/demo_data.py "$WORK/data"
SIXSTARS_DEMO_DATA="$WORK/data" SIXSTARS_DEMO_OUT="$WORK/rec" \
  cargo test --release demo_capture -- --ignored --nocapture
fi

# animated WebP: 1360px wide (2x source downscaled), 25 fps, lossy q80.
# ffmpeg builds often lack libwebp, so frames go through img2webp (brew install webp).
webp() {
  local frames="$WORK/frames-$1"
  rm -rf "$frames" && mkdir -p "$frames"
  ffmpeg -v error -y -i "$WORK/rec/$1.mp4" -vf "fps=25,scale=1360:-1:flags=lanczos" "$frames/%04d.png"
  img2webp -loop 0 -lossy -q 80 -m 6 -d 40 "$frames"/*.png -o "assets/$2.webp" >/dev/null
  echo "assets/$2.webp $(du -h "assets/$2.webp" | cut -f1)"
}
webp overview demo_overview
webp models demo_3d
webp media demo_media

# still for the collapsed screenshot section
ffmpeg -v error -y -ss 5.0 -i "$WORK/rec/overview.mp4" -frames:v 1 -vf "scale=1600:-1:flags=lanczos" assets/preview.png

# one MP4 with all three clips for people who want the full-quality version
ffmpeg -v error -y -i "$WORK/rec/overview.mp4" -i "$WORK/rec/models.mp4" -i "$WORK/rec/media.mp4" \
  -filter_complex "[0:v][1:v][2:v]concat=n=3:v=1:a=0,scale=1920:-2:flags=lanczos,format=yuv420p[v]" \
  -map "[v]" -c:v libx264 -preset slow -crf 20 -movflags +faststart assets/demo.mp4
ffmpeg -v error -y -ss 4.0 -i "$WORK/rec/overview.mp4" -frames:v 1 -vf "scale=1280:-1" -q:v 3 assets/demo-poster.jpg
du -h assets/demo.mp4 assets/demo-poster.jpg assets/preview.png
