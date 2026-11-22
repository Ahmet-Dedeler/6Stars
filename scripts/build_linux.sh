#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT="dist/percystar_v${VERSION}_linux_x86_64"

rustup target add x86_64-unknown-linux-gnu >/dev/null

RUSTFLAGS="-C target-cpu=x86-64-v3" cargo build --release --target x86_64-unknown-linux-gnu

mkdir -p dist
cp target/x86_64-unknown-linux-gnu/release/PercyStar dist/PercyStar
strip dist/PercyStar

zip "${OUT}.zip" dist/PercyStar
echo "Built ${OUT}.zip ($(du -sh "${OUT}.zip" | cut -f1))"
