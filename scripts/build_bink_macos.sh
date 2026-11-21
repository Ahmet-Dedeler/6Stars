#!/usr/bin/env bash
# Packages just the decoder and MIT-licensed frame bridge for local Mac builds.
set -euo pipefail

app="${1:?PercyStar.app path required}"
cache="${CARGO_TARGET_DIR:-target}/bink-macos"
rad_url=https://www.radgametools.com/down/Bink/BinkMacArmPlayer.dmg
rad_sha=a9496ed44219b2a061cf542bd7c4d565f29d117c8cbe143c85e8901d7b295966
bridge_rev=76d516ab7dfb6fbf61af352d35b651fb3d1640e3
bridge_sha=b8846c23ba07d62b9ebe60b2c1828977e26ee7b396b23e0c53130266addabd80
mkdir -p "$cache" "$app/Contents/Helpers" "$app/Contents/Resources"
tmp="$(mktemp -d)"
mounted=0
cleanup() {
  if [[ "$mounted" == 1 ]]; then hdiutil detach "$tmp/rad" -quiet || return; fi
  rm -r "$tmp"
}
trap cleanup EXIT

matches_sha() {
  [[ -f "$1" ]] && [[ "$(shasum -a 256 "$1" | cut -d' ' -f1)" == "$2" ]]
}

rad="$cache/BinkMacArmPlayer"
if ! matches_sha "$rad" "$rad_sha"; then
  curl -fsSL --retry 2 "$rad_url" -o "$tmp/rad.dmg"
  mkdir "$tmp/rad"
  hdiutil attach "$tmp/rad.dmg" -nobrowse -readonly -mountpoint "$tmp/rad" -quiet
  mounted=1
  candidate="$tmp/rad/BinkMacArmPlayer.app/Contents/MacOS/BinkMacArmPlayer"
  matches_sha "$candidate" "$rad_sha" || { echo "RAD player changed; the frame bridge needs updating." >&2; exit 1; }
  cp -X "$candidate" "$tmp/player"
  hdiutil detach "$tmp/rad" -quiet
  mounted=0
  cp "$tmp/player" "$rad"
fi

bridge="$cache/bridge-$bridge_rev.tar.gz"
if ! matches_sha "$bridge" "$bridge_sha"; then
  curl -fsSL --retry 2 "https://codeload.github.com/Ahmet-Dedeler/bink-player-mac/tar.gz/$bridge_rev" -o "$tmp/bridge.tar.gz"
  matches_sha "$tmp/bridge.tar.gz" "$bridge_sha" || { echo "Unexpected Bink frame bridge checksum." >&2; exit 1; }
  cp "$tmp/bridge.tar.gz" "$bridge"
fi
mkdir "$tmp/bridge"
tar -xzf "$bridge" --strip-components=1 -C "$tmp/bridge"

helpers="$app/Contents/Helpers"
cp -X "$rad" "$helpers/BinkMacArmPlayer"
# Same audio-track flag as the standalone wrapper. Check the original instruction first.
python3 - "$helpers/BinkMacArmPlayer" <<'PY'
from pathlib import Path
import struct
import sys

path = Path(sys.argv[1])
data = bytearray(path.read_bytes())
if struct.unpack_from("<I", data, 0xe44c)[0] != 0x52808009:
    sys.exit("Unexpected BinkOpen flags instruction")
struct.pack_into("<I", data, 0xe44c, 0x52888009)
path.write_bytes(data)
PY

clang -arch arm64 -mmacosx-version-min=12.0 -fobjc-arc -Os -shared \
  -Wno-deprecated-declarations -Wl,-dead_strip -Wl,-install_name,@rpath/libBinkHUD.dylib \
  -framework Cocoa -framework MetalKit -framework QuartzCore -framework AudioToolbox \
  "$tmp/bridge/src/libBinkHUD.m" -o "$helpers/libBinkHUD.dylib"
cp "$tmp/bridge/LICENSE" "$app/Contents/Resources/BinkBridge-LICENSE.txt"
for helper in "$helpers/BinkMacArmPlayer" "$helpers/libBinkHUD.dylib"; do
  codesign --force --sign - "$helper"
done
