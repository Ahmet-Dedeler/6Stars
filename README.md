<p align="center">
  <img src="assets/logo.png" alt="PercyStar Logo" width="140" />
</p>

<h1 align="center">PercyStar (Maintained Fork)</h1>

<p align="center">
  A maintained fork of <a href="https://github.com/rpatty-dev/PercyStar">rpatty-dev/PercyStar</a>, the Rust RPF9 and TTPF archive explorer.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-1.85%2B-orange?style=flat-square&logo=rust" alt="Rust" />
  <img src="https://img.shields.io/badge/Shaders-RDNA2%20GFX10.3%20%E2%86%92%20HLSL%20%2F%20SPIR--V-FF4FA3?style=flat-square" alt="Shaders" />
  <img src="https://img.shields.io/badge/Textures-DXGI%20%2B%20RDNA2%204KB%2F64KB-48D2FF?style=flat-square" alt="Textures" />
  <img src="https://img.shields.io/badge/3D-Model%20Viewer%20(11%20Categories)-FF55AF?style=flat-square" alt="3D" />
  <img src="https://img.shields.io/badge/Audio-Pure%20Rust%20Opus%20(Zero%20FFmpeg)-90EBA5?style=flat-square" alt="Audio" />
  <img src="https://img.shields.io/badge/Platform-Windows%20x64-0078D6?style=flat-square" alt="Windows" />
  <img src="https://img.shields.io/badge/Platform-macOS%20(Universal)-111111?style=flat-square&logo=apple" alt="macOS" />
  <img src="https://img.shields.io/badge/License-Source--Available%20(No%20Modifications)-red?style=flat-square" alt="License" />
</p>

<p align="center">
  <strong>Live Research &amp; Breakthroughs Dashboard: <a href="https://percystar-breakthroughs.vercel.app">percystar-breakthroughs.vercel.app</a></strong> · <strong><a href="https://mapnavmeshsite.vercel.app">Interactive NavMesh World Map</a></strong>
</p>

<p align="center">
  <img src="assets/demo_overview.webp" alt="PercyStar demo: Neon Sunset UI, search, and vehicle filters" width="920" />
</p>

## What This Fork Adds

- **BK2 video on Apple Silicon and Windows x64:** Select a `.bk2` to play it in the preview panel. Both build scripts include the decoder, so there's no second app to install. On Windows, click the video and press Space to pause, or use its slider to seek.
- **AWC audio previews:** Play and seek through individual streams without exporting them first. The decoder also reads multichannel banks and files with extra stream-table headers that upstream can miss.
- **Better 3D previews:** Drawables in a model dictionary appear separately instead of stacked on top of each other. Fixed rotated meshes and false model entries, added streamed `.ttpf` textures and vehicle paint previews, and added a **Highest detail only** toggle for character and clothing models.
- **More accurate model colors:** The viewer finds shared textures across packs, binds them to the right materials, and reads both packed and floating-point UVs. If it can't find a usable color texture, it shows that part in gray.
- **Open files directly:** Double-click an `.rpf`, `.ttpf`, `.bk2` or `.awc` in Finder, use "Open with" on Windows, drop files on the window, or pick several in **Open File...**. Standalone videos and sound banks are added to the current list instead of replacing it.
- **Easier browsing:** Search matches file types, so "video" or "sound" finds clips with generated names. Model names that match no rule go to **Uncategorized** instead of World / Map. Sort the Explorer and Model Viewer by name or size. Small text tables show their strings instead of hex, and binary blobs no longer show up as XML just because they start with `<`.

## Build from Source

```sh
git clone https://github.com/Ahmet-Dedeler/PercyStar.git
cd PercyStar
```

<details>
<summary><b>macOS</b> (Apple Silicon, macOS 12+)</summary>

1. Install Rust via `rustup`, Python 3, and the Xcode Command Line Tools.
2. Run `bash scripts/build_macos.sh`.
3. Open `dist/PercyStar.app`.

The script fetches and verifies the pinned decoder and frame bridge, so video playback works offline. The app also builds for Intel, but the bundled decoder is ARM-only.

</details>

<details>
<summary><b>Windows x64</b></summary>

1. Install Rust, the Visual Studio C++ Build Tools, and 7-Zip.
2. In PowerShell, run `powershell -ExecutionPolicy Bypass -File .\scripts\build_windows.ps1`.
3. Open `dist\PercyStar-Win64\PercyStar.exe`.

The build fetches and verifies RAD's decoder and puts it next to PercyStar without running the RAD installer. Keep the two files together. A plain `cargo build` builds the app without bundling the player.

</details>

<details>
<summary><b>Updating</b></summary>

Run `git pull --ff-only` and rebuild. Set `-Destination` on Windows or `PERCYSTAR_APP_PATH` on macOS to keep your previous build:

```sh
PERCYSTAR_APP_PATH=dist/PercyStar-next.app bash scripts/build_macos.sh
```

</details>

---

## Promo

<p align="center">
  <a href="assets/promo.mp4"><img src="assets/promo-poster.jpg" alt="PercyStar promo video: pulling a car out of an RPF9 archive into an .OBJ" width="860" /></a><br />
  <sub><a href="assets/promo.mp4">Watch the 31-second promo (MP4)</a></sub>
</p>

---

## Visual Showcase

### RAGE 9 3D Meshlet Viewer (`Textured/Mat` · `Clay Solid` · `Solid+Wire` · `Wire`)
<p align="center">
  <img src="assets/demo_3d.webp" alt="PercyStar 3D Meshlet Viewer — Shading Modes & Smooth Orbit" width="900" />
</p>

### Pure-Rust Audio (`ADAT` Opus → WAV), `PSIN` Metadata & XML Inspector
<p align="center">
  <img src="assets/demo_inspector.webp" alt="PercyStar Pure-Rust Opus Audio, PSIN PSO Inspector & XML Viewer" width="900" />
</p>

<details>
<summary>Static High-Res Screenshot (Neon Sunset)</summary>
<p align="center">
  <img src="assets/preview.png" alt="PercyStar 3D Meshlet Viewer — Neon Sunset Theme" width="900" />
</p>
</details>

---

## Features & Format Breakthroughs

- **Native Xbox Series X `PS\x00\x09` AMD RDNA2 (GFX10.3) Shader Disassembler & HLSL / SPIR-V Lifter (`Shaders (RDNA2)` Tab)**
  - Parses the complete `PS\x00\x09` PSO cache (`25,471` Graphics PSOs, `2,693` Compute PSOs, `54,576` metadata blobs, `21,522` compiled AMD GFX10.3 shaders), including `0x41` `USER_DATA` root-signature bindings (`SRV`/`CBV`/`UAV`/`Sampler`), `COMPUTE_PGM_RSRC1/2`, and `[numthreads(x,y,z)]`.
  - Auto-tags every shader stage (**PS**, **VS**, **CS**, **NGG/Meshlet** `exp prim`, and **Hardware RT** `image_bvh64_intersect_ray`).
  - Live 3-view inspector (**`GFX10.3 ASM`** · **`Lifted HLSL (SM 6.6)`** · **`SPIR-V 1.5 / SGPR ABI`**) + 1-click **Export `.hlsl` / `.spv` / `.asm`** and CLI batch exporter.
- **Dedicated 3D Model Viewer Tab (`Explorer | Model Viewer | Shaders (RDNA2)`)**
  - Full-screen 3D workspace built for instant 60 FPS browsing across **144,000+ RAGE 9 3D models**.
  - **10 Strict RAGE 9 Categories** with live counts: **All Models**, **Vehicles**, **Air & Sea**, **Weapons**, **Characters**, **Clothing**, **Animals** (`cr[mbfraci]_` taxonomy), **Props & Items**, **World / Map**, and **NavMesh 3D**.
  - **Hide LODs/Bounds** toggle (filters `_lod`/`_slod`/`batchlod`/`bipedbounds`/`collision_`), `◀ Prev` / `Next ▶` / `Random` buttons, `Up`/`Down` arrow-key browsing, **5 Camera Presets** (`3/4`, `Front`, `Side`, `Back`, `Top`), and **1-Click Catalog CSV Import** + `.OBJ` loader.
- **Xbox Series X (Scarlett / RDNA2) Texture Engine (`.#td` & `.ttpf`)**
  - **100.00% TTPF Format Identification (357,115 / 357,115 textures):** Solved ETPF descriptor `u1` bits 3..9 (`(u1 >> 3) & 0x7F` = exact Microsoft `DXGI_FORMAT` enum) — zero unknown texture formats.
  - **All 20+ DXGI Formats Decoded Natively in Pure Rust:** `BC1`–`BC7`, `BC6H_UF16`/`SF16` HDR, `R16G16B16A16_FLOAT`/`UNORM`, `R16_FLOAT`/`UNORM`, `R16G16_FLOAT`, `R32_FLOAT`, `R10G10B10A2_UNORM`, `R8G8B8A8`, `B8G8R8A8`, `R8_UNORM`, `A8_UNORM` (`texture2ddecoder` + IEEE-754 half-float Reinhard tonemapping).
  - **RDNA2 4KB (`SW_4KB_D`) & 64KB Deswizzle for All 5 Bit Depths (8 / 16 / 32 / 64 / 128bpp):** Raw and zlib-backed textures support preview and PNG/DDS export. The scanner recognizes BCPack chunks but does not decode their payloads.
- **Expanded RAGE 9 3D Meshlet Decoder (`.#dr`, `.#ft`, `.#dd`, `.obj`)**
  - Direct click-to-view decoding of unencrypted RAGE 9 virtual (`0x50xxxxxx`) + physical (`0x60xxxxxx`) meshlet resources straight out of `.rpf` archives, including `+0xA8` drawable name resolution and non-zero page header support (**38× more models discovered**).
  - Clean **LOD0-only** extraction (`Lod0 -> grmModel -> grmGeometry`), 24/24/16-bit quantized vertices, 7-bit packed meshlet triangles + `u16` remap table, **UV0** (`type 0x28`), upgraded embedded `SW_4KB_D` BC7/BC3/BC1 textures, custom `.png`/`.bmp` texture loading, and full `.OBJ` export (`v` + `vt` + `f v/vt`).
- **Native UI, Localization & NavMesh Decoders (`RBF0`, `GFX`, `0DRPz`, `CNavMesh`)**
  - **`0DRPz` Text / GXT Viewer:** Decodes multi-chunk RAGE 9 UTF-8 localization tables & standalone string pools under **UI & Text (`RBF0/GFX/GXT`)** with searchable preview-slug filenames.
  - **`RBF0` (`fwui`) → XML:** Handles the 4-byte `0xFB 0xFF 0x01 0x00` header to decode binary UI resources cleanly into XML (with `.xml` export).
  - **Scaleform GFx v42 (AS3) Inspector:** Dumps ActionScript 3 classes, packages, and symbol references.
  - **`CNavMesh` (`0x00010015`) 3D Viewer:** Detects navigation tiles with world grid coordinates (`nav_{gx}_{gy}_(x,y).ynv`), triangulates polygons colored by surface type (land/water/road/pavement/interior), and exports `.OBJ`.
- **100% Native Runtime Dependencies for Audio, 3D, Textures, UI, XML & PSO**
  - **No Python, no FFmpeg, no external DLLs required** — `ADAT` (`.awc`) Opus streams decode natively via pure-Rust `rusty-opus` (RFC 6716). All crates pinned in [`Cargo.lock`](Cargo.lock).
- **Whole-Folder & Single-Archive Scanning (`.rpf` & `.ttpf`)**
  - Multi-threaded live scanning cached to `%LOCALAPPDATA%\PercyStar\scan_cache_v8.tsv`.
- **In-app Bink 2 playback**
  - The Apple Silicon build includes the decoder and frame bridge. Windows docks RAD's native player for pause and seeking. The Windows loop toggle applies when you next open a video.
- **CLI Batch Exporters (Models, Textures & RDNA2 Shaders)**
  ```powershell
  PercyStar.exe --extract-vehicles "<path-to-levels.rpf>" "<output-dir>"
  PercyStar.exe --extract-models "<path-to-archive.rpf>" "<output-dir>" "filter1,filter2"
  PercyStar.exe --dump-textures "<path-to-archive.rpf|.ttpf>" "<output-dir>" [limit]
  PercyStar.exe --disasm-shaders "<path-to-shaders.rpf|zlib_dir|.ps09>" "<output-dir>" [limit]
  ```

---

## Project Layout

```text
PercyStar/
├── assets/
│   ├── logo.png            # App & window icon
│   ├── logo.ico            # Windows executable icon
│   ├── demo_overview.webp  # 30fps Neon Sunset UI and search demo
│   ├── demo_3d.webp        # 30fps 3D meshlet viewer shading modes showcase
│   ├── demo_inspector.webp # 30fps XML / PSIN / ADAT Opus audio showcase
│   ├── preview.png         # High-res Neon Sunset screenshot
│   ├── promo.mp4           # 31s promo video
│   └── promo-poster.jpg    # Promo poster frame
├── promo/                  # HyperFrames source for the promo video
├── scripts/
│   ├── build_macos.sh      # Universal macOS (.app / .zip) build helper
│   ├── build_windows.ps1  # Builds the Windows app with its video helper
│   ├── setup_deps.ps1      # Fetches and extracts the pinned Windows player
│   ├── awc_extract.py      # Standalone CLI ADAT (.awc) Opus -> .wav helper
│   └── rpf9_carve.py       # Standalone CLI RPF9 carver
├── docs/
│   └── breakthroughs.json  # Live-synced research & breakthroughs timeline feed
├── src/
│   ├── main.rs             # PercyStar egui app, Model Viewer, RDNA2 Shader Lifter, RPF9/TTPF scanner & CLI
│   ├── rdna2.rs            # Xbox Scarlett PS\x00\x09 parser, AMD GFX10.3 disassembler, HLSL SM 6.6 & SPIR-V 1.5 lifter
│   ├── model3d.rs          # RAGE 9 meshlet decoder, UV/TXD parser & Z-buffer renderer
│   ├── tex.rs              # Xbox Scarlett RDNA2 4KB/64KB textures, TTPF, RBF0, GFx, 0DRPz & CNavMesh
│   └── awc.rs              # Pure-Rust ADAT (.awc) Opus -> .wav decoder
├── app.rc / build.rs       # Win32 icon resource
├── Cargo.toml / Cargo.lock # All Rust crate dependencies pinned cleanly
└── LICENSE
```

---

## What's Still Missing

The Xbox dump scan recognized 754,364 entries, including 797 videos. The [format audit](docs/format-coverage.md) lists what rendered, what fell back to raw bytes, and what the scan doesn't cover.

---

## Scope, Clean-Room Research & Legal Notice

PercyStar is an independent archive viewer for **unencrypted, user-supplied files**. The Rust archive decoders are separate from video playback, which uses RAD's player.

- **Zero Circumvention / Zero Keys:** Contains **no** decryption routines, encryption keys, hashes, DRM bypasses, or access-control workarounds. Encrypted data is skipped untouched.
- **No Game Files:** The source repo contains no RAD binaries or game assets. The Mac and Windows builds download their player from RAD for local use; the decoder remains under RAD's terms. Users supply their own archives.
- **Independent Trademark Disclaimer:** PercyStar is an independent open-source project and is not affiliated with, endorsed by, or sponsored by any game studio or publisher.

**Copyright (c) 2026 rpatty-dev — All Rights Reserved.**  
See [LICENSE](LICENSE). You may download, run, and fork this repository on GitHub for personal use/reference, **but you may not modify, alter, or redistribute modified versions of the code or binaries without explicit written permission.**
