# 6Stars

Formerly PercyStar (upstream `rpatty-dev/PercyStar`); renamed to 6Stars. Rust + egui explorer for RPF9/TTPF archives: Explorer list, previews (textures, BK2 video, AWC audio, text/XML/UI), and a 3D Model Viewer. Several agents (Claude, Devin, others) work here in parallel, so keep this file to stable rules.

## Remotes and branches
- `origin` = upstream `rpatty-dev/PercyStar`. Never push there. Changes go upstream only as focused PRs from branches cut off `origin/main`.
- `fork` = public `Ahmet-Dedeler/6Stars` (renamed from `Ahmet-Dedeler/PercyStar`; old URLs redirect). `fork/main` is the maintained fork that the README tells people to clone and build. Work lands there through feature branches; don't force-push `main`.
- `mine` = private `Ahmet-Dedeler/PercyStar-private` (`ahmet/main`): local-only experiments.
- Before starting, `git fetch fork` and branch from `fork/main`. Other agents push often; rebase before you push.
- [CONTRIBUTING.md](CONTRIBUTING.md) is the public contribution guide. The remotes above describe this maintainer checkout; contributors don't need to reproduce that setup.

## Naming
- User-facing name is **6Stars** (window title, bundle, exe, docs). Rust identifiers can't start with a digit, so code and env vars use `sixstars` / `SIXSTARS_` where a name is needed.
- Keep upstream references (`rpatty-dev/PercyStar`), the upstream `LICENSE`, and dev-only test gates (`PERCYSTAR_*`, `PERCY_*`). [LICENSE-MIT](LICENSE-MIT) only applies to modules explicitly marked MIT.
- Compatibility with pre-rename installs: the data folder `PercyStar` is moved to `6Stars` on first launch, old `percystar-*` temp folders are still cleaned, and `.obj` files with a `# PercyStar` header still load as z-up.

## Every change must work on macOS and Windows
- Targets: macOS (Apple Silicon first, universal build) and Windows x64. Treat both as first-class.
- Platform code goes behind `#[cfg(target_os = "macos")]` / `#[cfg(windows)]`, with a working path (or a clear "not supported" message) on the other OS. Never leave a feature that silently does nothing on one platform.
- For code changes, run both before opening a PR when possible. Docs and license notices don't need a Rust build.
  - `cargo test --release`
  - `cargo check --release --all-targets --target x86_64-pc-windows-msvc` (install with `rustup target add x86_64-pc-windows-msvc`)
- Windows-specific traps already hit here: release builds are GUI-subsystem (no console; CLI `--` modes call `AttachConsole`), helper processes need `CREATE_NO_WINDOW`, paths can contain spaces, unicode and apostrophes, PowerShell 5.1 needs TLS 1.2 forced, and scripts are run with `-ExecutionPolicy Bypass`.
- macOS traps: Finder opens files through `application:openURLs:` (`src/mac_open.rs`), not argv. New openable file types also need `UTImportedTypeDeclarations` / `CFBundleDocumentTypes` in `scripts/build_macos.sh`.
- If you don't have both platforms, say what you tested and what you couldn't run. Windows CI runs on PRs; don't describe a compile check as a runtime test.

## New feature or format? Wire up every surface
Adding a kind, format or category means touching all the places it shows up. Missing one is the most common bug in this repo (e.g. video previews worked, but searching "video" found nothing). Check each:
- **Scanner:** `scan_single_rpf` / folder scan / `collect_rpfs`, plus the loose-file path (`loose_media_entry`) if it can exist outside an archive.
- **Scan cache:** the kind string in `load_disk_cache_from`. Bump `scan_cache_vN.tsv` when classification changes.
- **Explorer:** category filter and counts in `rebuild_view`, the category list (`CATS` and the sidebar labels), `kind_color`, and search words in `kind_search_words`.
- **Model Viewer:** `classify_model_cat`, `MODEL_CATS`, `model_cat_idx`, `model_cat_tag`, and the array sizes that depend on the category count.
- **Preview and export:** the preview panel, Export/Extract buttons, and the `--extract-*` CLI if relevant.
- **Opening files:** drag-and-drop, the Open File dialog filters, command-line paths (Windows "Open with"), and Finder (macOS plist + `mac_open`).
- **Both platforms:** see the section above.
- **Docs:** README "What This Fork Adds" and badges (category counts, platforms).
- **Tests:** a unit test for the new parser or classifier.

## Versioning and releases
- The version lives in `Cargo.toml` (`APP_VERSION` reads it) and `app.rc` (Windows file version, both the numeric and string fields). Update both together, plus `Cargo.lock`.
- Bump the minor version for user-visible features, patch for fixes.
- The in-app updater checks `fork` releases. A Windows release asset must be named `6Stars_v{ver}_Win64.zip` with `6Stars.exe` at the zip's top level.
- Include `LICENSE-MIT` with any authorized release that contains the MIT-licensed modules.

## Build and test
- macOS app: `bash scripts/build_macos.sh` builds `dist/6Stars.app` (set `SIXSTARS_APP_PATH` for a side-by-side test build). Windows: `scripts/build_windows.ps1` builds `dist\6Stars-Win64`, with `binkplay.exe` next to the exe.
- Data-gated tests: `PERCYSTAR_LEVELS_RPF=<levels.rpf>` (model regressions), `PERCYSTAR_BK2=<file.bk2>` (in-panel Bink, run with `-- --ignored`), `PERCYSTAR_AUDIT=1` (+ `PERCYSTAR_AUDIT_CACHE`, `PERCYSTAR_AUDIT_N`) for the per-format coverage audit in `src/audit_tests.rs`.
- macOS signing: `build_macos.sh` signs with the keychain's Developer ID / Apple Development certificate so the app keeps its Downloads permission across rebuilds. Don't switch it back to ad-hoc, and don't launch throwaway test builds of the app against the user's Downloads folder: each one is a new privacy prompt.
- Never play sound or steal focus in tests: Bink tests run at volume 0. Don't click media in the live app to "verify".
- Use your own `CARGO_TARGET_DIR` when another agent may be building at the same time.

## Media notes
- macOS BK2: `src/bink_embed.rs` hosts the bundled Bink decoder and frame bridge (`github.com/Ahmet-Dedeler/bink-player-mac`, `src/embed.inc`). Change both sides together.
- Windows BK2: `src/bink_dock.rs` docks RAD's `binkplay.exe` inside the window. `scripts/setup_deps.ps1` pins the extracted player's hash, not the unversioned archive.
- Clips are copied (or hard-linked, for standalone files) to a `6stars-video-*` temp folder as `clip.bk2`, which keeps odd paths safe. Never write to that file: for standalone clips it's a hard link to the user's original.
- AWC: `src/awc.rs` decodes, `src/audio_player.rs` plays (afplay on macOS, `PlaySoundW` on Windows).
- The SixNG project (GPLv3) is a reference only. Don't copy its code.
