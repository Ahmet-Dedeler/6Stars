#!/usr/bin/env bash
# Builds dist/6Stars.app: universal (arm64 + x86_64), fat LTO, CPU-tuned per arch.
# Signed with your Developer ID / Apple Development certificate when one is in the keychain
# (SIXSTARS_SIGN_IDENTITY overrides), otherwise ad-hoc.
set -euo pipefail
cd "$(dirname "$0")/.."

# SIXSTARS_APP_PATH picks the output; PERCYSTAR_APP_PATH still works from before the rename
APP="${SIXSTARS_APP_PATH:-${PERCYSTAR_APP_PATH:-dist/6Stars.app}}"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
export MACOSX_DEPLOYMENT_TARGET=11.0

# A real certificate gives the app a stable identity (bundle id + team), so macOS keeps
# privacy grants like Downloads access across rebuilds. Ad-hoc signatures are tied to the
# exact binary, so every rebuild asks again.
if [[ -z "${SIXSTARS_SIGN_IDENTITY:-}" ]]; then
  ids=$(security find-identity -v -p codesigning 2>/dev/null || true)
  SIXSTARS_SIGN_IDENTITY=$(grep -o '"Developer ID Application: [^"]*"' <<<"$ids" | head -1 | tr -d '"')
  [[ -n "$SIXSTARS_SIGN_IDENTITY" ]] || SIXSTARS_SIGN_IDENTITY=$(grep -o '"Apple Development: [^"]*"' <<<"$ids" | head -1 | tr -d '"')
  [[ -n "$SIXSTARS_SIGN_IDENTITY" ]] || SIXSTARS_SIGN_IDENTITY=-
fi
export SIXSTARS_SIGN_IDENTITY
if [[ "$SIXSTARS_SIGN_IDENTITY" == "-" ]]; then
  echo "Signing ad-hoc: macOS will ask for folder access again after each rebuild." >&2
else
  echo "Signing with: $SIXSTARS_SIGN_IDENTITY"
fi

if [[ -e "$APP" || -L "$APP" ]]; then
  echo "$APP already exists. Set SIXSTARS_APP_PATH to a new output path." >&2
  exit 1
fi
mkdir -p "$(dirname "$APP")"
stage=$(mktemp -d "$(dirname "$APP")/.6stars-build.XXXXXX")
trap 'rm -r "$stage"' EXIT
bundle="$stage/6Stars.app"

rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null

# apple-m1 tuning runs on every Apple Silicon Mac; x86-64-v3 (Haswell+) covers every Intel Mac that runs macOS 11+
RUSTFLAGS="-C target-cpu=apple-m1" cargo build --locked --release --target aarch64-apple-darwin
RUSTFLAGS="-C target-cpu=x86-64-v3" cargo build --locked --release --target x86_64-apple-darwin

mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
lipo -create -output "$bundle/Contents/MacOS/6Stars" \
  "$TARGET_DIR/aarch64-apple-darwin/release/6Stars" \
  "$TARGET_DIR/x86_64-apple-darwin/release/6Stars"

ICONSET="$stage/6Stars.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/icon.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) assets/icon.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$bundle/Contents/Resources/6Stars.icns"

cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>6Stars</string>
  <key>CFBundleDisplayName</key><string>6Stars</string>
  <key>CFBundleIdentifier</key><string>io.github.ahmet-dedeler.6stars</string>
  <key>CFBundleExecutable</key><string>6Stars</string>
  <key>CFBundleIconFile</key><string>6Stars</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>${MACOSX_DEPLOYMENT_TARGET}</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <!-- Lets Finder open these with 6Stars (double-click, Open With, drop on the Dock icon).
       Archives default to 6Stars; .bk2/.awc are Alternate so an installed Bink Player or
       audio tool keeps the default. -->
  <key>UTImportedTypeDeclarations</key>
  <array>
    <dict>
      <key>UTTypeIdentifier</key><string>io.github.ahmet-dedeler.6stars.rpf</string>
      <key>UTTypeDescription</key><string>RAGE Package File</string>
      <key>UTTypeConformsTo</key><array><string>public.data</string></array>
      <key>UTTypeTagSpecification</key><dict><key>public.filename-extension</key><array><string>rpf</string></array></dict>
    </dict>
    <dict>
      <key>UTTypeIdentifier</key><string>io.github.ahmet-dedeler.6stars.ttpf</string>
      <key>UTTypeDescription</key><string>RAGE Texture Pack</string>
      <key>UTTypeConformsTo</key><array><string>public.data</string></array>
      <key>UTTypeTagSpecification</key><dict><key>public.filename-extension</key><array><string>ttpf</string></array></dict>
    </dict>
    <dict>
      <key>UTTypeIdentifier</key><string>com.radgametools.bink2</string>
      <key>UTTypeDescription</key><string>Bink 2 Video</string>
      <key>UTTypeConformsTo</key><array><string>public.movie</string><string>public.data</string></array>
      <key>UTTypeTagSpecification</key><dict><key>public.filename-extension</key><array><string>bk2</string></array></dict>
    </dict>
    <dict>
      <key>UTTypeIdentifier</key><string>io.github.ahmet-dedeler.6stars.awc</string>
      <key>UTTypeDescription</key><string>RAGE Audio Wave Container</string>
      <key>UTTypeConformsTo</key><array><string>public.audio</string><string>public.data</string></array>
      <key>UTTypeTagSpecification</key><dict><key>public.filename-extension</key><array><string>awc</string></array></dict>
    </dict>
  </array>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>RAGE Archive</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Default</string>
      <key>CFBundleTypeExtensions</key><array><string>rpf</string><string>ttpf</string></array>
      <key>LSItemContentTypes</key><array><string>io.github.ahmet-dedeler.6stars.rpf</string><string>io.github.ahmet-dedeler.6stars.ttpf</string></array>
    </dict>
    <dict>
      <key>CFBundleTypeName</key><string>Bink 2 Video</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>CFBundleTypeExtensions</key><array><string>bk2</string></array>
      <key>LSItemContentTypes</key><array><string>com.radgametools.bink2</string></array>
    </dict>
    <dict>
      <key>CFBundleTypeName</key><string>RAGE Audio</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>CFBundleTypeExtensions</key><array><string>awc</string></array>
      <key>LSItemContentTypes</key><array><string>io.github.ahmet-dedeler.6stars.awc</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST

bash scripts/build_bink_macos.sh "$bundle"
codesign --force --timestamp=none --sign "$SIXSTARS_SIGN_IDENTITY" "$bundle"
codesign --verify --deep --strict "$bundle"
mv "$bundle" "$APP"
echo "Built $APP ($(du -sh "$APP" | cut -f1))"
lipo -info "$APP/Contents/MacOS/6Stars"
