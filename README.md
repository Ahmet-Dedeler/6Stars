<p align="center">
  <img src="assets/logo.png" alt="PercyStar Logo" width="140" />
</p>

<h1 align="center">PercyStar</h1>

<p align="center">
  <strong>High-speed Rust RPF9 (<code>9FPR</code>) archive explorer, docked D3D11 Bink2 player, and <code>ADAT</code> Opus audio extractor</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-1.99%2B-orange?style=flat-square&logo=rust" alt="Rust" />
  <img src="https://img.shields.io/badge/UI-egui%20%2F%20SF%20Pro%20Display-FCAF17?style=flat-square" alt="UI" />
  <img src="https://img.shields.io/badge/Platform-Windows%20x64-0078D6?style=flat-square" alt="Windows" />
</p>

---

## Features

- **Whole-Folder & Single-RPF Scanning** — Point PercyStar at an archive or an entire build folder; scans smallest archives first across 4 worker threads and auto-updates every `10s` as files change.
- **Incremental Disk Cache** — Caches scanned archives in `%LOCALAPPDATA%\PercyStar\scan_cache.tsv` keyed by `(path, size, mtime)` for instant (`<50 ms`) reloads, and remembers your last opened folder.
- **In-App D3D11 Video Player** — Docks `binkplay.exe` (`/NoBorder /DrawD3D11 /L`) directly into the inspector pane via Win32 `SetParent` for zero-copy 60fps Bink2 (`KB2*`) playback + audio.
- **GTA 6 `ADAT` (`.awc`) Audio Decoder** — Parses `ADAT` v1 chunk tables and decodes `2048`-byte Opus packet blocks to playable `.wav` streams via `scripts/awc_extract.py` + `ffmpeg`.
- **Auto-Stitched XMLs & `PSIN` PSO Inspector** — Automatically stitches `64 KB` split zlib XML chunks, names them by root tag, and decodes `PSIN` PSO headers, sections (`LSCH`, `LENM`, `LSNS`, `LGID`, `LRSC`), and strings.

---

## Project Layout

```text
PercyStar/
├── assets/
│   ├── logo.png        # Embedded app & window icon
│   └── logo.ico        # Windows executable icon
├── scripts/
│   ├── awc_extract.py  # Standalone ADAT (.awc) Opus -> .wav decoder
│   └── rpf9_carve.py   # CLI RPF9 carver (bk2 / awc / zlib)
├── src/
│   └── main.rs         # PercyStar egui application
├── app.rc              # Win32 icon resource
├── build.rs            # Resource compiler hook
└── Cargo.toml
```

---

## Building

Requires **Rust** (`x86_64-pc-windows-gnu` with MinGW `windres`/`dlltool`, or `msvc`), **FFmpeg** in `PATH` (for `.awc` -> `.wav` decoding), and **RAD Video Tools** (`binkplay.exe` placed alongside `PercyStar.exe` or installed in `C:\Program Files (x86)\RADVideo`).

```powershell
cargo build --release
```

The compiled binary is output to `target/release/PercyStar.exe`.
