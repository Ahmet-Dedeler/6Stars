<p align="center">
  <img src="assets/logo.png" alt="PercyStar Logo" width="140" />
</p>

<h1 align="center">PercyStar</h1>

<p align="center">
  <strong>Zero-dependency native Rust RPF9 (<code>9FPR</code>) explorer, RAGE 9 3D meshlet viewer, pure-Rust <code>ADAT</code> Opus audio decoder, and docked D3D11 Bink2 player</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-1.85%2B-orange?style=flat-square&logo=rust" alt="Rust" />
  <img src="https://img.shields.io/badge/Audio-Pure%20Rust%20Opus%20(Zero%20FFmpeg)-90EBA5?style=flat-square" alt="Audio" />
  <img src="https://img.shields.io/badge/3D-RAGE9%20Meshlets%20%2B%20UVs-FF55AF?style=flat-square" alt="3D" />
  <img src="https://img.shields.io/badge/Platform-Windows%20x64-0078D6?style=flat-square" alt="Windows" />
  <img src="https://img.shields.io/badge/License-Source--Available%20(No%20Modifications)-red?style=flat-square" alt="License" />
</p>

---

## Features

- **100% Native Runtime Dependencies for Audio, 3D, XML, and PSO**
  - **No Python, no FFmpeg, no external DLLs required** — `ADAT` (`.awc`) Opus streams are decoded natively inside `PercyStar.exe` using pure-Rust `rusty-opus` (RFC 6716), and `.#dr`/`.#ft`/`.#dd` 3D models are decoded and rasterized natively in Rust.
  - All Rust crate dependencies are pinned in [`Cargo.lock`](Cargo.lock).
- **Native RAGE 9 3D Model Viewer (`.#dr`, `.#ft`, `.#dd`, `.obj`)**
  - Direct click-to-view decoding of unencrypted RAGE 9 virtual (`0x50xxxxxx`) + physical (`0x60xxxxxx`) meshlet resources straight out of `.rpf` archives.
  - Clean **LOD0-only** extraction (`gtaDrawable.Lod0 -> grmModel -> grmGeometry`), 24/24/16-bit quantized vertices, 7-bit packed meshlet triangles + `u16` remap table, and **UV0** (`type 0x28`).
  - Zero-alloc **Z-buffered software rasterizer** (`Textured/Mat`, `Clay Solid`, `Solid+Wire`, `Wire`, `Spin`), embedded `BC1`/`BC3` TXD decoding, custom `.png`/`.bmp` texture loading, and full `.OBJ` export (`v` + `vt` + `f v/vt`).
  - Dedicated **`Vehicles Only`** toggle in the 3D Models tab + sequential fast-indexing of vehicle archives.
- **Whole-Folder & Single-RPF Scanning**
  - Indexes across worker threads, streams results live, and caches to `%LOCALAPPDATA%\PercyStar\scan_cache_v4.tsv`.
- **Docked D3D11 Bink2 Video Player (Optional `binkplay.exe`)**
  - Docks RAD `binkplay.exe` (`/NoBorder /DrawD3D11 /L`) directly into the inspector pane. Run [`scripts/setup_deps.ps1`](scripts/setup_deps.ps1) if you don't already have RAD Video Tools installed.
- **CLI Batch Model Export**
  ```powershell
  PercyStar.exe --extract-vehicles "<path-to-levels.rpf>" "<output-dir>"
  PercyStar.exe --extract-models "<path-to-archive.rpf>" "<output-dir>" "filter1,filter2"
  ```

---

## Project Layout

```text
PercyStar/
├── assets/
│   ├── logo.png            # App & window icon
│   └── logo.ico            # Windows executable icon
├── scripts/
│   ├── setup_deps.ps1      # Optional RAD Video Tools (binkplay.exe) setup
│   ├── awc_extract.py      # Standalone CLI ADAT (.awc) Opus -> .wav helper
│   └── rpf9_carve.py       # Standalone CLI RPF9 carver
├── src/
│   ├── main.rs             # PercyStar egui application & RPF9 scanner
│   ├── model3d.rs          # RAGE 9 meshlet decoder, UV/TXD parser & Z-buffer renderer
│   └── awc.rs              # Pure-Rust ADAT (.awc) Opus -> .wav decoder
├── app.rc / build.rs       # Win32 icon resource
├── Cargo.toml / Cargo.lock # All Rust crate dependencies pinned cleanly
└── LICENSE
```

---

## Building from Source

Requires **Rust** (`x86_64-pc-windows-gnu` with MinGW `windres`/`dlltool`, or `msvc`). Cargo fetches all pinned dependencies from `Cargo.lock`:

```powershell
cargo test --release
cargo build --release
```

Compiled standalone binary: `target/release/PercyStar.exe`.

---

## Scope & License

PercyStar inspects **unencrypted** streams (`KB2*` video, `ADAT` audio, plain `zlib` XML/PSIN/RAGE 9 meshlet resources). It does not include encryption keys, DRM bypasses, or third-party proprietary binaries.

**Copyright (c) 2026 rpatty-dev — All Rights Reserved.**  
See [LICENSE](LICENSE). You may download, run, and fork this repository on GitHub for personal use/reference, **but you may not modify, alter, or redistribute modified versions of the code or binaries without explicit written permission.**
