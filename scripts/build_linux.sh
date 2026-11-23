#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
OUT="dist/Percy6Stars_v${VERSION}_linux_x86_64"
TARGET_CPU="${TARGET_CPU:-x86-64-v3}"

rustup target add x86_64-unknown-linux-gnu >/dev/null

RUSTFLAGS="-C target-cpu=${TARGET_CPU}" cargo build --release --locked --target x86_64-unknown-linux-gnu

mkdir -p dist
cp "${TARGET_DIR}/x86_64-unknown-linux-gnu/release/Percy6Stars" dist/Percy6Stars
command -v strip >/dev/null 2>&1 && strip dist/Percy6Stars || true

rm -f "${OUT}.zip"
zip -j -q "${OUT}.zip" dist/Percy6Stars LICENSE LICENSE-MIT THIRD_PARTY_NOTICES.md
echo "Built ${OUT}.zip ($(du -sh "${OUT}.zip" | cut -f1))"
