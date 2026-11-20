<p align="center">
  <img src="assets/logo.png" alt="PercyStar Logo" width="140" />
</p>

<h1 align="center">PercyStar</h1>

<p align="center">
  <strong>High-speed native Rust RPF9 (<code>9FPR</code>) explorer, RAGE 9 3D meshlet viewer, docked D3D11 Bink2 player, and <code>ADAT</code> Opus audio decoder</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-1.85%2B-orange?style=flat-square&logo=rust" alt="Rust" />
  <img src="https://img.shields.io/badge/UI-egui%20%2F%20SF%20Pro%20Display-FCAF17?style=flat-square" alt="UI" />
  <img src="https://img.shields.io/badge/3D-RAGE9%20Meshlets%20%2B%20UVs-FF55AF?style=flat-square" alt="3D" />
  <img src="https://img.shields.io/badge/Platform-Windows%20x64-0078D6?style=flat-square" alt="Windows" />
  <img src="https://img.shields.io/badge/License-Source--Available%20(No%20Modifications)-red?style=flat-square" alt="License" />
</p>

---

## Features

- **Native RAGE 9 3D Model Viewer (`.#dr`, `.#ft`, `.#dd`, `.obj`)**
  - Direct click-to-view decoding of unencrypted RAGE 9 multi-chunk virtual (`0x50xxxxxx`) + physical (`0x60xxxxxx`) meshlet resources straight out of `.rpf` archives.
  - Clean **LOD0-only** extraction via `gtaDrawable.Lod0 -> grmModel -> grmGeometry`, 24/24/16-bit quantized vertex decoding, 7-bit packed meshlet triangles + `u16` vertex remap table, and **UV0** (`type 0x28`) extraction.
  - Zero-allocation **Z-buffered software rasterizer** (`Textured/Mat`, `Clay Solid`, `Solid+Wire`, `Wire`, `Spin`) with adaptive drag resolution, embedded `BC1`/`BC3` TXD decoding, custom `.png`/`.bmp` texture loading, and full `.OBJ` export (`v` + `vt` + `f v/vt`).
  - Dedicated **`Vehicles Only`** toggle in the 3D Models tab + sequential fast-indexing of vehicle archives.
- **Whole-Folder & Single-RPF Scanning**
  - Point PercyStar at a single `.rpf` or an entire folder; indexes across worker threads, streams results live, and caches to `%LOCALAPPDATA%\PercyStar\scan_cache_v4.tsv` for instant (`<50 ms`) reloads.
- **Docked D3D11 Bink2 Video Player**
  - Docks `binkplay.exe` (`/NoBorder /DrawD3D11 /L`) directly into the inspector pane via Win32 `SetParent` for 60 fps `KB2*` playback with audio.
- **`ADAT` (`.awc`) Opus Audio Decoder**
  - Parses `ADAT` v1 chunk tables and decodes `2048`-byte Opus packet blocks to playable `.wav` streams via `scripts/awc_extract.py` + `ffmpeg`.
- **Auto-Stitched XMLs & `PSIN` PSO Inspector**
  - Automatically stitches split `64 KB` zlib XML chunks (named by root tag) and decodes `PSIN` PSO sections (`LSCH`, `LENM`, `LSNS`, `LGID`, `LRSC`) and strings.
- **CLI Batch Vehicle Export**
  ```powershell
  PercyStar.exe --extract-vehicles "<path-to-levels.rpf>" "<output-dir>"
  ```

---

## Project Layout

```text
PercyStar/
├── assets/
│   ├── logo.png           # App & window icon
│   └── logo.ico           # Windows executable icon
├── scripts/
│   ├── awc_extract.py     # Standalone ADAT (.awc) Opus -> .wav decoder
│   └── rpf9_carve.py      # CLI RPF9 carver (bk2 / awc / zlib)
├── src/
│   ├── main.rs            # PercyStar egui application & RPF9 scanner
│   └── model3d.rs         # RAGE 9 meshlet decoder, UV/TXD parser & Z-buffer renderer
├── app.rc                 # Win32 icon resource
├── build.rs               # Resource compiler hook
├── Cargo.toml
└── LICENSE
```

---

## Building from Source

Requires **Rust** (`x86_64-pc-windows-gnu` with MinGW `windres`/`dlltool`, or `msvc`), optional **FFmpeg** + **Python 3** in `PATH` (for `.awc` -> `.wav`), and optional **RAD Video Tools** (`binkplay.exe` beside `PercyStar.exe` or in `C:\Program Files (x86)\RADVideo`).

```powershell
cargo build --release
```

Compiled binary: `target/release/PercyStar.exe`.

---

## Scope & Notes

PercyStar inspects and previews **unencrypted** streams (`KB2*` video, `ADAT` audio, plain `zlib` XML/PSIN/RAGE 9 meshlet resources). It does **not** include or ship any proprietary encryption keys, DRM bypasses, or copyrighted game assets.

---

## License

**Copyright (c) 2026 rpatty-dev — All Rights Reserved.**  
See [LICENSE](LICENSE). You may download, run, and fork this repository on GitHub for personal use/reference, **but you may not modify, alter, or redistribute modified versions of the code or binaries without explicit written permission.**
