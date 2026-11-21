#!/usr/bin/env bash
# Builds dist/PercyStar.app: universal (arm64 + x86_64), fat LTO, CPU-tuned per arch, ad-hoc signed.
set -euo pipefail
cd "$(dirname "$0")/.."

APP="${PERCYSTAR_APP_PATH:-dist/PercyStar.app}"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
export MACOSX_DEPLOYMENT_TARGET=11.0

if [[ -e "$APP" || -L "$APP" ]]; then
  echo "$APP already exists. Set PERCYSTAR_APP_PATH to a new output path." >&2
  exit 1
fi
mkdir -p "$(dirname "$APP")"
stage=$(mktemp -d "$(dirname "$APP")/.percystar-build.XXXXXX")
trap 'rm -r "$stage"' EXIT
bundle="$stage/PercyStar.app"

rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null

# apple-m1 tuning runs on every Apple Silicon Mac; x86-64-v3 (Haswell+) covers every Intel Mac that runs macOS 11+
RUSTFLAGS="-C target-cpu=apple-m1" cargo build --locked --release --target aarch64-apple-darwin
RUSTFLAGS="-C target-cpu=x86-64-v3" cargo build --locked --release --target x86_64-apple-darwin

mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
lipo -create -output "$bundle/Contents/MacOS/PercyStar" \
  "$TARGET_DIR/aarch64-apple-darwin/release/PercyStar" \
  "$TARGET_DIR/x86_64-apple-darwin/release/PercyStar"

ICONSET="$stage/PercyStar.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/logo.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) assets/logo.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$bundle/Contents/Resources/PercyStar.icns"

cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>PercyStar</string>
  <key>CFBundleDisplayName</key><string>PercyStar</string>
  <key>CFBundleIdentifier</key><string>dev.rpatty.percystar</string>
  <key>CFBundleExecutable</key><string>PercyStar</string>
  <key>CFBundleIconFile</key><string>PercyStar</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>${MACOSX_DEPLOYMENT_TARGET}</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

bash scripts/build_bink_macos.sh "$bundle"
codesign --force --sign - "$bundle"
codesign --verify --deep --strict "$bundle"
mv "$bundle" "$APP"
echo "Built $APP ($(du -sh "$APP" | cut -f1))"
lipo -info "$APP/Contents/MacOS/PercyStar"
