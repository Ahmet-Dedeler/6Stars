# Percy6Stars

Percy6Stars is a Rust + egui explorer for RPF9 and TTPF archives, built on `rpatty-dev/PercyStar`. It includes an Explorer, previews for textures, BK2 video, AWC audio, text/XML/UI, and a 3D Model Viewer.

If you're here to make a PR, start with [CONTRIBUTING.md](CONTRIBUTING.md). This file is for the details that help people and agents work in the same repo without getting in each other's way. Keep it to things the next person will still need to know.

## Remotes and branches
- `origin` = upstream `rpatty-dev/PercyStar`. Use it as a reference, not a push target. If a change belongs upstream, send a focused PR from a branch based on `origin/main`.
- `fork` = public `Ahmet-Dedeler/Percy6Stars` (the old `Ahmet-Dedeler/PercyStar` URLs redirect here). Start Percy6Stars work from `fork/main` and land it through a feature branch. Keep `main` free of force-pushes so other people's branches keep working.
- `mine` = private `Ahmet-Dedeler/PercyStar-private` (`ahmet/main`): local-only experiments.
- Run `git fetch fork` before branching and rebase before you push, since others may have landed changes. These remote names are for the maintainer checkout. You don't need them all to contribute.

## Naming
- User-facing name is **Percy6Stars** (window title, bundle, exe, docs). Rust identifiers can't start with a digit, so code and env vars use `sixstars` / `SIXSTARS_` where a name is needed.
- Keep upstream references (`rpatty-dev/PercyStar`), the upstream `LICENSE`, and dev-only test gates (`PERCYSTAR_*`, `PERCY_*`). [LICENSE-MIT](LICENSE-MIT) only applies to modules explicitly marked MIT.
- Compatibility with pre-rename installs (PercyStar, then 6Stars): the data folder `6Stars` or `PercyStar` is moved to `Percy6Stars` on first launch, old `6stars-*` / `percystar-*` temp folders are still cleaned, and `.obj` files with a `# 6Stars` or `# PercyStar` header still load as z-up. The macOS bundle id and UTType ids stay `io.github.ahmet-dedeler.6stars` so folder permissions and file associations survive.
- Naming is a license term: upstream's license requires "Percy" in the repo and app name and credit to @rpatty-dev with a link to `rpatty-dev/PercyStar` in README and LICENSE. Don't drop either. The project must stay non-commercial and keep LICENSE verbatim.

## Supporting macOS and Windows
- We support macOS (Apple Silicon first, universal build) and Windows x64.
- Put platform code behind `#[cfg(target_os = "macos")]` or `#[cfg(windows)]`. Give the other OS a working path or a message that explains what's not supported, so people aren't left wondering if a button is broken.
- For code changes, run both before opening a PR when possible. Docs and license notices don't need a Rust build.
  - `cargo test` (debug, ~20 s once warm; `--release` re-runs fat LTO and takes ~10 min, so save it for the data-gated audits below)
  - `cargo check --all-targets --target x86_64-pc-windows-msvc` (install with `rustup target add x86_64-pc-windows-msvc`)
- `.githooks/pre-push` runs `cargo test` when a push touches Rust code (`src/`, `Cargo.*`, `build.rs`). Enable it once per clone with `git config core.hooksPath .githooks` (worktrees share it). Only skip it with `--no-verify` for docs or an emergency, and say so in the PR.
- Windows CI (`windows`) is a required check on `main`: it runs the tests on Windows and builds the package, without fat LTO. A red check blocks the merge, so fix it rather than bypassing it.
- On Windows, release builds use the GUI subsystem (no console; CLI `--` modes call `AttachConsole`). Helpers need `CREATE_NO_WINDOW`, and paths can have spaces, Unicode, and apostrophes. PowerShell 5.1 needs TLS 1.2 forced, and scripts are run with `-ExecutionPolicy Bypass`.
- On macOS, Finder opens files through `application:openURLs:` (`src/mac_open.rs`), not argv. For a new file type, also update `UTImportedTypeDeclarations` / `CFBundleDocumentTypes` in `scripts/build_macos.sh`.
- If you don't have both platforms, say what you tested and what you couldn't run. Windows CI runs on PRs; don't describe a compile check as a runtime test.

## Adding a format or category
When we added video previews, searching for "video" still found nothing. This checklist helps you find the other places that need to know about a new kind, format, or category:
- **Scanner:** `scan_single_rpf` / folder scan / `collect_rpfs`, plus the loose-file path (`loose_media_entry`) if it can exist outside an archive.
- **Scan cache:** the kind string in `load_disk_cache_from`. Bump `scan_cache_vN.tsv` when classification changes.
- **Explorer:** category filter and counts in `rebuild_view`, the category list (`CATS` and the sidebar labels), `kind_color`, and search words in `kind_search_words`.
- **Model Viewer:** `classify_model_cat`, `MODEL_CATS`, `model_cat_idx`, `model_cat_tag`, and the array sizes that depend on the category count.
- **Preview and export:** the preview panel, Export/Extract buttons, and the `--extract-*` CLI if relevant.
- **Opening files:** drag-and-drop, the Open File dialog filters, command-line paths (Windows "Open with"), and Finder (macOS plist + `mac_open`).
- **Both platforms:** see [supporting macOS and Windows](#supporting-macos-and-windows).
- **Docs:** README "What This Fork Adds" and badges (category counts, platforms).
- **Tests:** a unit test for the new parser or classifier.

## Versioning and releases
- The version lives in `Cargo.toml` (`APP_VERSION` reads it) and `app.rc` (Windows file version, both the numeric and string fields). Update both together, plus `Cargo.lock`.
- Bump the minor version for user-visible features, patch for fixes.
- The in-app updater checks `fork` releases. A Windows release asset must be named `Percy6Stars_v{ver}_Win64.zip` with `Percy6Stars.exe` at the zip's top level. Builds from before the rename look for `6Stars_v{ver}_Win64.zip` with `6Stars.exe`, so attach that copy too for a while.
- Include `LICENSE-MIT` with any authorized release that contains the MIT-licensed modules.

## Build and test
- macOS app: `bash scripts/build_macos.sh` builds `dist/Percy6Stars.app` (set `SIXSTARS_APP_PATH` for a side-by-side test build). Windows: `scripts/build_windows.ps1` builds `dist\Percy6Stars-Win64`, with `binkplay.exe` next to the exe.
- Data-gated tests: `PERCYSTAR_LEVELS_RPF=<levels.rpf>` (model regressions), `PERCYSTAR_BK2=<file.bk2>` (in-panel Bink, run with `-- --ignored`), `PERCYSTAR_AUDIT=1` (+ `PERCYSTAR_AUDIT_CACHE`, `PERCYSTAR_AUDIT_N`) for the per-format coverage audit in `src/audit_tests.rs`.
- Keep Developer ID / Apple Development signing in `build_macos.sh` so the app keeps its Downloads permission across rebuilds. Use a separate test folder for throwaway builds so we don't trigger new privacy prompts for Downloads.
- Use the silent Bink tests (volume 0) instead of clicking media in the live app. Tests should leave the user's audio and focus alone.
- Use your own `CARGO_TARGET_DIR` when another agent may be building at the same time.
- Installing a local build: the app ignores `osascript ... quit`, so stop it with `pkill -x Percy6Stars`, move the old `/Applications/Percy6Stars.app` to the Trash, then `ditto dist/Percy6Stars.app /Applications/Percy6Stars.app`.
- Decode bug or render bug? Dump the one entry in a throwaway `#[ignore]` test: find it via `load_disk_cache_from(cache_file_path())` and call the decoder the UI uses (see `src/audit_tests.rs`). Don't commit it.
- The 3D view is a CPU rasterizer in `src/model3d.rs`: Z-up, orbits the visible bounding-box center, perspective-correct UVs (weighted by `persp_buf`). Keep new textured paths perspective-correct, or textures slide while orbiting.

## Media notes
- macOS BK2: `src/bink_embed.rs` hosts the bundled Bink decoder and frame bridge (`github.com/Ahmet-Dedeler/bink-player-mac`, `src/embed.inc`). Change both sides together.
- Windows BK2: `src/bink_dock.rs` docks RAD's `binkplay.exe` inside the window. `scripts/setup_deps.ps1` pins the extracted player's hash, not the unversioned archive.
- Clips go into a `percy6stars-video-*` temp folder as `clip.bk2` so unusual paths work. Treat that file as read-only: a standalone clip may be a hard link to the user's original.
- AWC: `src/awc.rs` decodes (upstream's in-memory chunk decoder, or our stream-table decoder when it recovers more audio, e.g. multichannel banks). `src/audio_player.rs` (upstream's player) plays through `waveOut` on Windows and `afplay` on a temporary WAV on macOS; Linux has no output yet.
- Upstream sync: `rpatty-dev/PercyStar` is merged into the fork with a real merge (not cherry-picks), keeping both histories. Keep our decoders where they measure better and note the measurement in the commit.
- DXIL fallback (`src/dxbc.rs`) loads DXC at runtime: `dxcompiler.dll` on Windows, `libdxcompiler.so` on Linux. Linux DXC's emulated COM uses the same vtable slots as Windows (checked v1.9.2609). Check with `PERCYSTAR_DXIL=<shader.cso> cargo test dxc_disassembles -- --ignored`.
- Use SixNG (GPLv3) to read up on formats, not as a source of code to copy into this repo.
