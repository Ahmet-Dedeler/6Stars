// Percy6Stars by Ahmet Dedeler (https://github.com/Ahmet-Dedeler, https://ahmetdedeler.com)
// Fork of PercyStar by rpatty-dev (https://github.com/rpatty-dev/PercyStar).
// Source: https://github.com/Ahmet-Dedeler/Percy6Stars
// License: PercyStar Non-Commercial Attribution & Copyleft License v1.2 (see LICENSE)
// If you copy, vendor or port this file, keep this header and credit both projects in your README.
//! Context menus, copy toasts and the Related files panel.
//!
//! One menu (`entry_menu`) is shared by every place that shows an entry: Explorer rows, the
//! Model Viewer list, texture gallery tiles, and a right click in the preview / 3D view. Plain
//! copies happen right in the menu. Actions that need decoding or file I/O (AI context,
//! extracting, related files) are queued and run on background threads; their results come
//! back through `ContextUi::done` and are applied at the start of the next frame, so the UI
//! never waits on a decoder.

use crate::context_copy::{self as cc, AssetContext, Decoded, ListMode, RelatedLine, ResourceHeader};
use crate::relations::{self, Related, RelationIndex, Rule};
use crate::{kind_color, read_entry_bytes, AppTab, Rpf9App, RpfEntry, ScanState};
use eframe::egui::{self, Color32, RichText};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Most entries one "Copy as AI context" covers; bigger selections are cut with a note.
const MAX_CONTEXT_ASSETS: usize = 200;
/// Up to this many selected models / dictionaries / banks are fully decoded for the context.
const MAX_DECODED_ASSETS: usize = 4;

const ACCENT: Color32 = Color32::from_rgb(255, 168, 28);
const MUTED: Color32 = Color32::from_rgb(140, 134, 160);
const PANEL_BG: Color32 = Color32::from_rgb(20, 14, 31);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToastKind {
    Ok,
    Info,
    Error,
}

struct Toast {
    text: String,
    kind: ToastKind,
    born: Instant,
}

/// Work queued by a menu, applied by `Rpf9App::context_tick`.
enum Action {
    AiContext { targets: Vec<usize>, json: bool, head: bool },
    Reveal(PathBuf),
    Extract(Vec<usize>),
    ShowRelated(usize),
    CopyRelatedPaths(usize),
    ExtractRelated(usize),
}

/// Results from background threads.
enum Done {
    Copy { text: String, toast: String },
    Toast(String, ToastKind),
    Index(Arc<RelationIndex>),
    /// A background index build found the scan had moved on; another is started next frame.
    IndexSkipped,
    Textures { entry: usize, index_ver: u64, rels: Vec<Related>, note: String },
}

/// Related files for one entry, as shown in the panel.
struct RelatedView {
    entry: usize,
    index_ver: u64,
    list: Vec<Related>,
    /// Shader textures for models: `None` while resolving.
    textures: Option<Vec<Related>>,
    texture_note: String,
}

#[derive(Default)]
pub struct ContextUi {
    toasts: Vec<Toast>,
    pending: Vec<Action>,
    done: Arc<Mutex<Vec<Done>>>,
    index: Option<Arc<RelationIndex>>,
    index_building: bool,
    pub related_open: bool,
    related: Option<RelatedView>,
    /// Multi-selection captured when a row's menu opened (ordering a big view every frame
    /// would be wasteful).
    menu_selection: Vec<usize>,
    /// Right click in the preview: where, and for which entry.
    preview_menu: Option<(egui::Pos2, usize)>,
    preview_menu_born: u64,
    frame: u64,
    /// Last text put on the clipboard (tests and screenshots read it).
    pub last_copied: String,
}

impl ContextUi {
    pub(crate) fn invalidate_related(&mut self) {
        self.related = None;
    }

    /// Queues "Copy as AI context" for `targets` (used by tests and the offscreen capture).
    #[cfg(test)]
    pub(crate) fn queue_ai_context(&mut self, targets: Vec<usize>, json: bool, head: bool) {
        self.pending.push(Action::AiContext { targets, json, head });
    }

    /// True once the panel shows `entry` with its shader-texture lookup finished.
    #[cfg(test)]
    pub(crate) fn related_ready(&self, entry: usize) -> bool {
        self.related.as_ref().is_some_and(|v| v.entry == entry && v.textures.is_some())
    }

    fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toasts.push(Toast { text: text.into(), kind, born: Instant::now() });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    fn copy(&mut self, ctx: &egui::Context, text: String, toast: impl Into<String>) {
        ctx.copy_text(text.clone());
        self.last_copied = text;
        self.toast(toast, ToastKind::Ok);
    }
}

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal Archive in Finder"
    } else if cfg!(windows) {
        "Show Archive in Explorer"
    } else {
        "Open Archive Folder"
    }
}

/// Shows `path` selected in the system file manager.
fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(windows)]
    let r = crate::silent_cmd("explorer").arg("/select,").arg(path).spawn();
    #[cfg(not(any(target_os = "macos", windows)))]
    let r = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
    match r {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(2.0);
    ui.label(RichText::new(title).size(10.0).strong().color(MUTED));
}

fn item(ui: &mut egui::Ui, label: &str, hint: &str) -> bool {
    let r = ui.add(egui::Button::new(RichText::new(label).size(12.5)).min_size(egui::vec2(230.0, 0.0)));
    let r = if hint.is_empty() { r } else { r.on_hover_text(hint) };
    r.clicked()
}

/// The shared entry menu. `selection` is the ordered multi-selection when the clicked entry
/// is part of one (empty otherwise). Returns true when an item was used.
pub fn entry_menu(ui: &mut egui::Ui, cu: &mut ContextUi, state: &Arc<Mutex<ScanState>>, idx: usize, selection: &[usize]) -> bool {
    let (e, picked): (Option<RpfEntry>, Vec<RpfEntry>) = {
        let st = state.lock().unwrap();
        let picked = if selection.len() > 1 {
            selection.iter().filter_map(|&i| st.entries.get(i).cloned()).collect()
        } else {
            Vec::new()
        };
        (st.entries.get(idx).cloned(), picked)
    };
    let Some(e) = e else { return false };
    let ctx = ui.ctx().clone();
    let mut used = false;
    ui.set_min_width(240.0);

    if picked.len() > 1 {
        let n = picked.len();
        section(ui, &format!("{n} SELECTED"));
        let refs: Vec<&RpfEntry> = picked.iter().collect();
        if item(ui, &format!("Copy {n} Names"), "") {
            cu.copy(&ctx, cc::format_list(&refs, ListMode::Names), format!("Copied {n} names"));
            used = true;
        }
        if item(ui, &format!("Copy {n} Paths"), "archive.rpf/name, one per line") {
            cu.copy(&ctx, cc::format_list(&refs, ListMode::Paths), format!("Copied {n} paths"));
            used = true;
        }
        if item(ui, &format!("Copy {n} Offsets & Sizes"), "") {
            cu.copy(&ctx, cc::format_list(&refs, ListMode::Offsets), format!("Copied {n} offsets"));
            used = true;
        }
        if item(ui, &format!("Copy {n} as AI Context"), "Markdown summary of every selected asset, for pasting into a chat with a language model") {
            cu.pending.push(Action::AiContext { targets: selection.to_vec(), json: false, head: false });
            used = true;
        }
        if item(ui, &format!("Copy {n} as JSON"), "") {
            cu.pending.push(Action::AiContext { targets: selection.to_vec(), json: true, head: false });
            used = true;
        }
        if item(ui, &format!("Extract {n}…"), "Export the selected entries (models as OBJ, textures as PNG, the rest as files)") {
            cu.pending.push(Action::Extract(selection.to_vec()));
            used = true;
        }
        ui.separator();
        section(ui, "THIS FILE");
    } else {
        section(ui, "COPY");
    }
    if item(ui, "Copy Name", &e.name) {
        cu.copy(&ctx, e.name.clone(), format!("Copied name: {}", e.name));
        used = true;
    }
    let full = cc::full_path(&e);
    if item(ui, "Copy Full Path", &full) {
        cu.copy(&ctx, full.clone(), format!("Copied path: {full}"));
        used = true;
    }
    let id = cc::entry_id(&e);
    if item(ui, "Copy Hash / ID", &id) {
        cu.copy(&ctx, id, "Copied hash / id");
        used = true;
    }
    let loc = cc::offset_line(&e);
    if item(ui, "Copy Offset & Size", &loc) {
        cu.copy(&ctx, loc, "Copied offset and size");
        used = true;
    }
    let disk = e.source_rpf.display().to_string();
    if item(ui, "Copy Archive Location on Disk", &disk) {
        cu.copy(&ctx, disk.clone(), "Copied archive location");
        used = true;
    }
    ui.separator();
    section(ui, "FOR LANGUAGE MODELS");
    if item(ui, "Copy as AI Context", "Compact Markdown: path, kind, sizes, resource header, decoded summary, related files, decoder note and spec section") {
        cu.pending.push(Action::AiContext { targets: vec![idx], json: false, head: false });
        used = true;
    }
    if item(ui, "Copy as AI Context + First 64 Bytes", "Same, plus one hex line of the first 64 decoded bytes") {
        cu.pending.push(Action::AiContext { targets: vec![idx], json: false, head: true });
        used = true;
    }
    if item(ui, "Copy as JSON", "") {
        cu.pending.push(Action::AiContext { targets: vec![idx], json: true, head: false });
        used = true;
    }
    ui.separator();
    section(ui, "RELATED FILES");
    if item(ui, "Show Related Files", "Files that belong with this asset, and the rule that linked each one") {
        cu.pending.push(Action::ShowRelated(idx));
        used = true;
    }
    if item(ui, "Copy All Related Paths", "") {
        cu.pending.push(Action::CopyRelatedPaths(idx));
        used = true;
    }
    if item(ui, "Extract With Related Files…", "This file and its related files into one folder, with a manifest.json") {
        cu.pending.push(Action::ExtractRelated(idx));
        used = true;
    }
    ui.separator();
    if item(ui, reveal_label(), &disk) {
        cu.pending.push(Action::Reveal(e.source_rpf.clone()));
        used = true;
    }
    if item(ui, "Extract…", "Export this entry (models as OBJ, textures as PNG, the rest as a file)") {
        cu.pending.push(Action::Extract(vec![idx]));
        used = true;
    }
    if used {
        ui.close_menu();
    }
    used
}

/// Attaches the entry menu to a row or tile. `selection` runs once, when the menu opens.
pub fn attach_entry_menu(
    resp: &egui::Response,
    cu: &mut ContextUi,
    state: &Arc<Mutex<ScanState>>,
    idx: usize,
    selection: impl FnOnce() -> Vec<usize>,
) {
    if resp.secondary_clicked() {
        cu.menu_selection = selection();
        if !cu.menu_selection.contains(&idx) {
            cu.menu_selection.clear();
        }
    }
    resp.context_menu(|ui| {
        let sel = std::mem::take(&mut cu.menu_selection);
        entry_menu(ui, cu, state, idx, &sel);
        cu.menu_selection = sel;
    });
}

/// The multi-selection in view order.
pub fn ordered_selection(view: &[usize], multi: &std::collections::HashSet<usize>) -> Vec<usize> {
    if multi.len() < 2 {
        return Vec::new();
    }
    view.iter().copied().filter(|i| multi.contains(i)).collect()
}

/// First bytes of an entry's payload: inflated for zlib-stored entries, raw otherwise.
fn read_head(e: &RpfEntry, n: usize) -> Option<Vec<u8>> {
    use flate2::read::ZlibDecoder;
    let mut f = File::open(&e.source_rpf).ok()?;
    if let Some(&(coff, clen)) = e.chunks.first() {
        f.seek(SeekFrom::Start(coff)).ok()?;
        let mut raw = vec![0u8; (clen as usize).min(4 << 20)];
        f.read_exact(&mut raw).ok()?;
        let mut out = Vec::new();
        ZlibDecoder::new(&raw[..]).take(n as u64).read_to_end(&mut out).ok()?;
        return Some(out);
    }
    f.seek(SeekFrom::Start(e.offset)).ok()?;
    if !matches!(e.kind, "TTPF" | "AWC" | "BK2" | "HMAP") {
        let mut out = Vec::new();
        if ZlibDecoder::new(BufReader::new(f.try_clone().ok()?).take(16 << 20)).take(n as u64).read_to_end(&mut out).is_ok() && !out.is_empty() {
            return Some(out);
        }
        f.seek(SeekFrom::Start(e.offset)).ok()?;
    }
    let want = n.min(e.comp_size.max(64) as usize);
    let mut raw = vec![0u8; want];
    let got = f.read(&mut raw).ok()?;
    raw.truncate(got);
    Some(raw)
}

/// Summary from decoders, run on a background thread. `full` allows the slow decoders
/// (model meshlets, every texture of a dictionary, Opus streams).
fn summarize(e: &RpfEntry, full: bool, meta: Option<&crate::dat::ResolvedAwcMeta>) -> (Decoded, Vec<(String, PathBuf, usize)>) {
    let mut tex_refs = Vec::new();
    let d = match e.kind {
        "TTPF" => match crate::tex::ttpf_index(&e.source_rpf) {
            Ok(ix) => match ix.descs.get(e.tracks as usize) {
                Some(d) => Decoded::Texture {
                    format: crate::tex::dxgi_info(d.dxgi).map_or_else(|| format!("DXGI {}", d.dxgi), |x| x.0.to_string()),
                    w: d.w,
                    h: d.h,
                    mips: d.mip_count,
                    note: Some(format!("swizzle mode {}", if d.tile_mode == 0 { "64 KiB" } else { "4 KiB" })),
                },
                None => Decoded::Unavailable(format!("texture #{} is not in the store's index", e.tracks)),
            },
            Err(err) => Decoded::Unavailable(err),
        },
        "TXD" if full => match crate::tex::decode_txd_from_rpf(&e.source_rpf, e.offset) {
            Ok(t) => Decoded::TextureDict { textures: t.iter().map(|t| format!("{} {}x{} {}", t.name, t.w, t.h, t.fmt_name)).collect() },
            Err(err) => Decoded::Unavailable(err),
        },
        "TXD" => Decoded::Unavailable(format!("{} texture(s); not listed for large selections", e.tracks)),
        "MODEL" | "VEH" if full => {
            let refs = crate::model3d::list_model_texture_refs(&e.source_rpf, e.offset).unwrap_or_default();
            let shaders = refs.iter().map(|r| r.shaders).max().unwrap_or(0);
            let textures: Vec<(String, bool)> = refs.iter().map(|r| (r.role(), r.hit.is_some())).collect();
            for r in &refs {
                if let Some(h) = &r.hit {
                    tex_refs.push((r.role(), h.path.clone(), h.tex_idx));
                }
            }
            match crate::decode_rage9_model_from_rpf_lod(&e.source_rpf, e.offset, &e.name, 0) {
                Ok(m) => Decoded::Model {
                    verts: m.verts.len(),
                    tris: m.tris.len(),
                    geoms: m.geom_count,
                    meshlets: m.meshlet_count,
                    lod: m.lod_label.clone(),
                    lod_geoms: m.lod_geoms.to_vec(),
                    parts: m.parts.len(),
                    bones: m.bone_count,
                    textures,
                    shaders_binding: shaders,
                },
                Err(err) => Decoded::Unavailable(err),
            }
        }
        "MODEL" | "VEH" => Decoded::Unavailable("model; decoded only for selections of up to 4".into()),
        "AWC" => {
            let pack = meta.map(|m| m.pack_name.clone()).filter(|p| !p.is_empty());
            let used_by = meta.map(|m| m.used_by.clone()).unwrap_or_default();
            if full {
                match read_entry_bytes(e) {
                    Ok(bytes) => {
                        let res = crate::awc::decode_awc_bytes_in_memory(&bytes, "stream", |_, _| {});
                        Decoded::Audio {
                            codec: "Opus (ADAT)".into(),
                            streams: res
                                .tracks
                                .iter()
                                .map(|t| cc::AudioStream { name: t.name.clone(), channels: t.channels, sample_rate: t.sample_rate, duration_secs: t.duration_secs })
                                .collect(),
                            pack,
                            used_by,
                        }
                    }
                    Err(err) => Decoded::Unavailable(err.to_string()),
                }
            } else {
                Decoded::Audio {
                    codec: "Opus (ADAT), not decoded".into(),
                    streams: (0..e.tracks).map(|i| cc::AudioStream { name: format!("stream_{i}"), channels: 0, sample_rate: 0, duration_secs: 0.0 }).collect(),
                    pack,
                    used_by,
                }
            }
        }
        "ANIM" => Decoded::Anim { clips: e.tracks },
        "NAVM" => Decoded::Navmesh { polys: e.tracks },
        k if crate::pedphys::is_kind(k) && full => match crate::pedphys::context_facts(e) {
            Ok((what, facts)) => Decoded::PedPhysics { what: what.into(), facts },
            Err(err) => Decoded::Unavailable(err),
        },
        k if crate::pedphys::is_kind(k) => Decoded::Unavailable(format!("{}; decoded only for selections of up to 4", crate::pedphys::kind_label(k))),
        "XML" if full => match read_entry_bytes(e) {
            Ok(b) => {
                let s = String::from_utf8_lossy(&b);
                Decoded::Text { bytes: b.len(), lines: s.lines().count(), first_line: s.lines().next().unwrap_or("").trim().to_string() }
            }
            Err(err) => Decoded::Unavailable(err.to_string()),
        },
        "BK2" => Decoded::Unavailable("video; play it in the preview".into()),
        _ => Decoded::Unavailable("open it in the preview".into()),
    };
    (d, tex_refs)
}

/// Inputs a context job needs from the UI thread.
struct JobInput {
    entries: Vec<(usize, RpfEntry)>,
    live: HashMap<usize, Decoded>,
    extra: HashMap<usize, Vec<Related>>,
    meta: HashMap<u64, crate::dat::ResolvedAwcMeta>,
    json: bool,
    head: bool,
    cut: usize,
}

/// Shown when a rescan changes the entry list while a job runs: the positions it holds would
/// now name other files, so it stops instead.
const SCAN_CHANGED: &str = "The file list changed while this ran (a scan is updating it). Try again in a moment.";

/// Runs `f` under the scan lock if the scan is still at version `ver`. Entry positions (the
/// selection, relation indexes) are only meaningful within the version they came from.
fn at_ver<R>(state: &Arc<Mutex<ScanState>>, ver: u64, f: impl FnOnce(&ScanState) -> R) -> Option<R> {
    let st = state.lock().unwrap();
    (st.ver == ver).then(|| f(&st))
}

fn related_lines(state: &Arc<Mutex<ScanState>>, ver: u64, rels: &[Related]) -> Option<Vec<RelatedLine>> {
    at_ver(state, ver, |st| {
        rels.iter()
            .filter_map(|r| {
                st.entries.get(r.entry).map(|o| RelatedLine { path: cc::full_path(o), kind: o.kind, why: format!("{}: {}", r.rule.title().to_lowercase(), r.why) })
            })
            .collect()
    })
}

/// Returns the relation index for scan version `ver`, reusing `have` when it matches and
/// otherwise building it (on this thread, holding the scan lock for the build). `None` when the
/// scan has moved past `ver`: an index of another version must never be queried with these
/// entries, since its positions would point at other files or past the end.
fn index_for(state: &Arc<Mutex<ScanState>>, have: Option<Arc<RelationIndex>>, ver: u64) -> Option<Arc<RelationIndex>> {
    let st = state.lock().unwrap();
    if st.ver != ver {
        return None;
    }
    if let Some(ix) = have.filter(|ix| ix.ver == ver) {
        return Some(ix);
    }
    Some(Arc::new(RelationIndex::build(&st.entries, &st.audio_bank_meta, ver)))
}

impl Rpf9App {
    /// Called once per frame before any panel: applies queued menu actions and finished
    /// jobs, keeps the related panel current, and draws toasts and the preview menu.
    pub(crate) fn context_tick(&mut self, ctx: &egui::Context) {
        self.ctx_ui.frame += 1;
        let done: Vec<Done> = std::mem::take(&mut *self.ctx_ui.done.lock().unwrap());
        for d in done {
            match d {
                Done::Copy { text, toast } => self.ctx_ui.copy(ctx, text, toast),
                Done::Toast(t, k) => self.ctx_ui.toast(t, k),
                Done::Index(ix) => {
                    self.ctx_ui.index_building = false;
                    self.ctx_ui.index = Some(ix);
                }
                Done::IndexSkipped => self.ctx_ui.index_building = false,
                Done::Textures { entry, index_ver, rels, note } => {
                    if let Some(v) = self.ctx_ui.related.as_mut().filter(|v| v.entry == entry && v.index_ver == index_ver) {
                        v.textures = Some(rels);
                        v.texture_note = note;
                    }
                }
            }
        }
        if !ctx.wants_keyboard_input() && ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::C)) {
            let targets = self.ordered_selected_indices();
            if !targets.is_empty() {
                self.ctx_ui.pending.push(Action::AiContext { targets, json: false, head: false });
            }
        }
        for a in std::mem::take(&mut self.ctx_ui.pending) {
            self.run_action(ctx, a);
        }
        if self.ctx_ui.related_open {
            self.refresh_related(ctx);
        }
        self.preview_menu_ui(ctx);
        self.draw_toasts(ctx);
    }

    fn run_action(&mut self, ctx: &egui::Context, a: Action) {
        match a {
            Action::AiContext { targets, json, head } => self.spawn_context_job(ctx, targets, json, head),
            Action::Reveal(p) => {
                if let Err(err) = reveal_in_file_manager(&p) {
                    self.ctx_ui.toast(format!("Could not open the file manager: {err}"), ToastKind::Error);
                }
            }
            Action::Extract(t) => self.export_entries(&t),
            Action::ShowRelated(i) => {
                self.ctx_ui.related_open = true;
                if self.selected != Some(i) {
                    self.open_related_entry(i);
                }
                self.ctx_ui.related = None;
            }
            Action::CopyRelatedPaths(i) => self.spawn_related_job(ctx, i, None),
            Action::ExtractRelated(i) => {
                if let Some(dir) = rfd::FileDialog::new().set_title("Extract with related files").pick_folder() {
                    self.spawn_related_job(ctx, i, Some(dir));
                }
            }
        }
    }

    /// What the preview already decoded for `idx`, so the context doesn't decode it twice.
    fn live_decoded(&self, idx: usize) -> Option<Decoded> {
        if self.model_entry == Some(idx) {
            if let Some(m) = self.mesh3d.as_ref() {
                return Some(Decoded::Model {
                    verts: m.verts.len(),
                    tris: m.tris.len(),
                    geoms: m.geom_count,
                    meshlets: m.meshlet_count,
                    lod: m.lod_label.clone(),
                    lod_geoms: m.lod_geoms.to_vec(),
                    parts: m.parts.len(),
                    bones: m.bone_count,
                    textures: Vec::new(),
                    shaders_binding: 0,
                });
            }
        }
        if self.audio_entry == Some(idx) && !self.audio_player.tracks.is_empty() {
            let ctx = self.audio_player.bank_ctx.as_ref();
            return Some(Decoded::Audio {
                codec: "Opus (ADAT)".into(),
                streams: self
                    .audio_player
                    .tracks
                    .iter()
                    .map(|t| cc::AudioStream { name: t.name.clone(), channels: t.channels, sample_rate: t.sample_rate, duration_secs: t.duration_secs })
                    .collect(),
                pack: ctx.map(|c| c.pack_name.clone()).filter(|p| !p.is_empty()),
                used_by: ctx.map(|c| c.used_by.clone()).unwrap_or_default(),
            });
        }
        if self.texture_entry == Some(idx) && !self.textures.is_empty() && !self.tex_from_model {
            let kind = self.state.lock().unwrap().entries.get(idx).map(|e| e.kind);
            if kind == Some("TXD") {
                return Some(Decoded::TextureDict {
                    textures: self.textures.iter().map(|t| format!("{} {}x{} {}", t.name, t.w, t.h, t.fmt_name)).collect(),
                });
            }
            let t = &self.textures[0];
            let (w, h) = t.full_size.unwrap_or((t.w, t.h));
            return Some(Decoded::Texture { format: t.fmt_name.to_string(), w, h, mips: t.mips, note: t.reduced_note.clone() });
        }
        None
    }

    /// Vehicle configuration parts of the viewed vehicle (resolved by the Configuration picker).
    fn live_vehicle_config(&self, idx: usize) -> Vec<Related> {
        if self.model_entry != Some(idx) {
            return Vec::new();
        }
        self.vehicle_variations
            .iter()
            .flat_map(|v| v.entries.iter().map(move |&entry| Related { entry, rule: Rule::VehicleConfig, why: v.label.clone() }))
            .collect()
    }

    fn spawn_context_job(&mut self, ctx: &egui::Context, mut targets: Vec<usize>, json: bool, head: bool) {
        let cut = targets.len().saturating_sub(MAX_CONTEXT_ASSETS);
        targets.truncate(MAX_CONTEXT_ASSETS);
        let (entries, meta, ver) = {
            let st = self.state.lock().unwrap();
            let entries: Vec<(usize, RpfEntry)> = targets.iter().filter_map(|&i| st.entries.get(i).map(|e| (i, e.clone()))).collect();
            let meta = entries
                .iter()
                .filter(|(_, e)| e.kind == "AWC" && e.rpf_name.eq_ignore_ascii_case("audio.rpf"))
                .filter_map(|(_, e)| st.audio_bank_meta.get(&e.offset).map(|m| (e.offset, m.clone())))
                .collect();
            (entries, meta, st.ver)
        };
        if entries.is_empty() {
            return;
        }
        let mut live = HashMap::new();
        let mut extra = HashMap::new();
        for (i, _) in &entries {
            if let Some(d) = self.live_decoded(*i) {
                live.insert(*i, d);
            }
            let mut x = self.live_vehicle_config(*i);
            if let Some(v) = self.ctx_ui.related.as_ref().filter(|v| v.entry == *i && v.index_ver == ver) {
                x.extend(v.textures.clone().unwrap_or_default());
            }
            if !x.is_empty() {
                extra.insert(*i, x);
            }
        }
        let n = entries.len();
        self.ctx_ui.toast(if n == 1 { "Building AI context…".to_string() } else { format!("Building AI context for {n} assets…") }, ToastKind::Info);
        let input = JobInput { entries, live, extra, meta, json, head, cut };
        let state = Arc::clone(&self.state);
        let done = Arc::clone(&self.ctx_ui.done);
        let have = self.ctx_ui.index.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let changed = || {
                done.lock().unwrap().push(Done::Toast(SCAN_CHANGED.into(), ToastKind::Error));
                ctx.request_repaint();
            };
            let Some(ix) = index_for(&state, have, ver) else { return changed() };
            done.lock().unwrap().push(Done::Index(Arc::clone(&ix)));
            let full = input.entries.len() <= MAX_DECODED_ASSETS;
            let mut assets = Vec::with_capacity(input.entries.len());
            for (i, e) in &input.entries {
                let mut a = AssetContext::from_entry(e);
                let head_bytes = read_head(e, 64 << 10);
                a.header = head_bytes.as_deref().and_then(ResourceHeader::parse);
                if input.head {
                    a.head64 = head_bytes.map(|mut b| {
                        b.truncate(64);
                        b
                    });
                }
                let (mut decoded, mut tex_refs) = summarize(e, full && !input.live.contains_key(i), input.meta.get(&e.offset));
                if let Some(l) = input.live.get(i) {
                    // the preview's numbers, plus the texture list the decoder pass found
                    decoded = match (l.clone(), decoded) {
                        (Decoded::Model { verts, tris, geoms, meshlets, lod, lod_geoms, parts, bones, .. }, _) => {
                            let refs = crate::model3d::list_model_texture_refs(&e.source_rpf, e.offset).unwrap_or_default();
                            tex_refs.extend(refs.iter().filter_map(|r| r.hit.as_ref().map(|h| (r.role(), h.path.clone(), h.tex_idx))));
                            Decoded::Model {
                                verts,
                                tris,
                                geoms,
                                meshlets,
                                lod,
                                lod_geoms,
                                parts,
                                bones,
                                shaders_binding: refs.iter().map(|r| r.shaders).max().unwrap_or(0),
                                textures: refs.iter().map(|r| (r.role(), r.hit.is_some())).collect(),
                            }
                        }
                        (live, _) => live,
                    };
                }
                let Some(mut rels) = at_ver(&state, ver, |st| ix.find(&st.entries, &st.audio_bank_meta, *i)) else { return changed() };
                relations::merge(&mut rels, ix.texture_relations(&tex_refs), *i);
                relations::merge(&mut rels, input.extra.get(i).cloned().unwrap_or_default(), *i);
                let Some(lines) = related_lines(&state, ver, &rels) else { return changed() };
                a.related = Some(lines);
                a.decoded = decoded;
                assets.push(a);
            }
            let mut text = if input.json { cc::to_json(&assets) } else { cc::to_markdown(&assets) };
            if input.cut > 0 && !input.json {
                text.push_str(&format!("\n({} more selected entries were left out; the limit is {MAX_CONTEXT_ASSETS}.)\n", input.cut));
            }
            let what = if input.json { "JSON" } else { "AI context" };
            let toast = if assets.len() == 1 { format!("Copied {what} for {}", assets[0].name) } else { format!("Copied {what} for {} assets", assets.len()) };
            done.lock().unwrap().push(Done::Copy { text, toast });
            ctx.request_repaint();
        });
    }

    /// Related paths to the clipboard, or (with `out`) everything extracted into one folder.
    fn spawn_related_job(&mut self, ctx: &egui::Context, idx: usize, out: Option<PathBuf>) {
        let state = Arc::clone(&self.state);
        let ver = state.lock().unwrap().ver;
        let done = Arc::clone(&self.ctx_ui.done);
        let have = self.ctx_ui.index.clone();
        let mut extra = self.live_vehicle_config(idx);
        if let Some(v) = self.ctx_ui.related.as_ref().filter(|v| v.entry == idx && v.index_ver == ver) {
            extra.extend(v.textures.clone().unwrap_or_default());
        }
        let ctx = ctx.clone();
        if out.is_some() {
            self.ctx_ui.toast("Extracting related files…", ToastKind::Info);
        }
        std::thread::spawn(move || {
            let changed = || {
                done.lock().unwrap().push(Done::Toast(SCAN_CHANGED.into(), ToastKind::Error));
                ctx.request_repaint();
            };
            let Some(ix) = index_for(&state, have, ver) else { return changed() };
            done.lock().unwrap().push(Done::Index(Arc::clone(&ix)));
            let Some((main, mut rels)) = at_ver(&state, ver, |st| (st.entries.get(idx).cloned(), ix.find(&st.entries, &st.audio_bank_meta, idx))) else {
                return changed();
            };
            let Some(main) = main else { return };
            if matches!(main.kind, "MODEL" | "VEH") {
                let refs: Vec<(String, PathBuf, usize)> = crate::model3d::list_model_texture_refs(&main.source_rpf, main.offset)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|r| r.hit.as_ref().map(|h| (r.role(), h.path.clone(), h.tex_idx)))
                    .collect();
                relations::merge(&mut rels, ix.texture_relations(&refs), idx);
            }
            relations::merge(&mut rels, extra, idx);
            // resolved in the same scan version as the lookup (the shader read above ran unlocked)
            let Some(items) = at_ver(&state, ver, |st| {
                std::iter::once((main.clone(), None))
                    .chain(rels.iter().filter_map(|r| st.entries.get(r.entry).map(|e| (e.clone(), Some(r.clone())))))
                    .collect::<Vec<(RpfEntry, Option<Related>)>>()
            }) else {
                return changed();
            };
            match out {
                None => {
                    if rels.is_empty() {
                        done.lock().unwrap().push(Done::Toast(format!("No related files found for {}", main.name), ToastKind::Info));
                    } else {
                        let text = items.iter().skip(1).map(|(e, _)| cc::full_path(e)).collect::<Vec<_>>().join("\n");
                        done.lock().unwrap().push(Done::Copy { text, toast: format!("Copied {} related paths", rels.len()) });
                    }
                }
                Some(dir) => {
                    let msg = match extract_bundle(&dir, &items) {
                        Ok(x) => x.toast(),
                        Err(err) => (format!("Extract failed: {err}"), ToastKind::Error),
                    };
                    done.lock().unwrap().push(Done::Toast(msg.0, msg.1));
                }
            }
            ctx.request_repaint();
        });
    }

    /// Keeps the panel's list in step with the selected entry and the scan.
    fn refresh_related(&mut self, ctx: &egui::Context) {
        let Some(sel) = self.selected.or(self.model_entry) else {
            self.ctx_ui.related = None;
            return;
        };
        let (ver, scanning) = {
            let st = self.state.lock().unwrap();
            (st.ver, st.scanning)
        };
        let stale = self.ctx_ui.index.as_ref().is_none_or(|ix| ix.ver != ver && !scanning);
        if stale && !self.ctx_ui.index_building {
            self.ctx_ui.index_building = true;
            let state = Arc::clone(&self.state);
            let done = Arc::clone(&self.ctx_ui.done);
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                // the scan moved on before the build: the next frame asks again
                if let Some(ix) = index_for(&state, None, ver) {
                    done.lock().unwrap().push(Done::Index(ix));
                } else {
                    done.lock().unwrap().push(Done::IndexSkipped);
                }
                ctx.request_repaint();
            });
        }
        let Some(ix) = self.ctx_ui.index.clone() else { return };
        if ix.ver != ver {
            // an index of another scan version can't be used with these entries (its positions
            // would name other files); show nothing until the matching one is built
            if self.ctx_ui.related.as_ref().is_some_and(|v| v.index_ver != ver) {
                self.ctx_ui.related = None;
            }
            return;
        }
        if self.ctx_ui.related.as_ref().is_some_and(|v| v.entry == sel && v.index_ver == ix.ver) {
            return;
        }
        let Some((list, model)) = at_ver(&self.state, ix.ver, |st| {
            let model = st.entries.get(sel).filter(|e| matches!(e.kind, "MODEL" | "VEH")).cloned();
            (ix.find(&st.entries, &st.audio_bank_meta, sel), model)
        }) else {
            return;
        };
        let mut list = list;
        relations::merge(&mut list, self.live_vehicle_config(sel), sel);
        let textures = if let Some(m) = model {
            let done = Arc::clone(&self.ctx_ui.done);
            let ix2 = Arc::clone(&ix);
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let (rels, note) = match crate::model3d::list_model_texture_refs(&m.source_rpf, m.offset) {
                    Ok(refs) => {
                        let hits: Vec<(String, PathBuf, usize)> = refs.iter().filter_map(|r| r.hit.as_ref().map(|h| (r.role(), h.path.clone(), h.tex_idx))).collect();
                        let rels = ix2.texture_relations(&hits);
                        let note = format!(
                            "{} texture(s) bound, {} found in the ttpf stores, {} of those in this scan",
                            refs.len(),
                            hits.len(),
                            rels.len()
                        );
                        (rels, note)
                    }
                    Err(err) => {
                        // Keep the raw error for the log; show a plain sentence in the panel.
                        eprintln!("related: shader texture lookup failed: {err}");
                        (Vec::new(), "Couldn't read this model's shader textures, so texture links aren't shown.".to_string())
                    }
                };
                done.lock().unwrap().push(Done::Textures { entry: sel, index_ver: ix2.ver, rels, note });
                ctx.request_repaint();
            });
            None
        } else {
            Some(Vec::new())
        };
        self.ctx_ui.related = Some(RelatedView { entry: sel, index_ver: ix.ver, list, textures, texture_note: String::new() });
    }

    /// Opens a related entry in the view that fits it.
    fn open_related_entry(&mut self, i: usize) {
        let is_model = self.state.lock().unwrap().entries.get(i).is_some_and(|e| matches!(e.kind, "MODEL" | "VEH" | "NAVM"));
        if self.tab == AppTab::ModelViewer && is_model {
            self.select_entry(i);
            self.show_3d = true;
        } else {
            if self.tab != AppTab::Explorer {
                self.tab = AppTab::Explorer;
            }
            self.select_entry(i);
            self.explorer_scroll_to_sel = true;
        }
    }

    /// Toggle for the Related files panel (Explorer list header, Model Viewer header).
    pub(crate) fn context_toolbar(&mut self, ui: &mut egui::Ui) {
        let open = self.ctx_ui.related_open;
        let btn = egui::Button::new(RichText::new("Related").color(if open { Color32::BLACK } else { Color32::WHITE })).fill(if open {
            ACCENT
        } else {
            Color32::from_rgb(38, 24, 56)
        });
        if ui.add(btn).on_hover_text("Show the files that belong with the selected asset. Right-click any file to copy its name, path or AI context; Cmd/Ctrl+Shift+C copies the selection as AI context.").clicked() {
            self.ctx_ui.related_open = !open;
        }
    }

    /// Right-click anywhere in the preview area (`rect`) opens the entry menu for the shown entry.
    pub(crate) fn preview_context_hook(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        let Some(idx) = self.model_entry.or(self.selected) else { return };
        let (clicked, pos) = ctx.input(|i| (i.pointer.secondary_clicked(), i.pointer.interact_pos()));
        if clicked && !ctx.memory(|m| m.any_popup_open()) {
            if let Some(p) = pos.filter(|p| rect.contains(*p)) {
                self.ctx_ui.preview_menu = Some((p, idx));
                self.ctx_ui.preview_menu_born = self.ctx_ui.frame;
            }
        }
    }

    fn preview_menu_ui(&mut self, ctx: &egui::Context) {
        let Some((pos, idx)) = self.ctx_ui.preview_menu else { return };
        let state = Arc::clone(&self.state);
        let cu = &mut self.ctx_ui;
        let resp = egui::Area::new(egui::Id::new("preview_entry_menu"))
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .constrain(true)
            .show(ctx, |ui| {
                ui.style_mut().visuals.button_frame = false;
                egui::Frame::menu(ui.style()).show(ui, |ui| entry_menu(ui, cu, &state, idx, &[])).inner
            });
        let used = resp.inner;
        let clicked_outside = cu.frame > cu.preview_menu_born + 1
            && ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !resp.response.rect.contains(p)));
        if used || clicked_outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.ctx_ui.preview_menu = None;
        }
    }

    fn draw_toasts(&mut self, ctx: &egui::Context) {
        const LIFE: f32 = 2.6;
        self.ctx_ui.toasts.retain(|t| t.born.elapsed().as_secs_f32() < LIFE);
        if self.ctx_ui.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("copy_toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -40.0))
            .order(egui::Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                for t in &self.ctx_ui.toasts {
                    let age = t.born.elapsed().as_secs_f32();
                    let a = ((LIFE - age) / 0.4).clamp(0.0, 1.0);
                    let (dot, edge) = match t.kind {
                        ToastKind::Ok => (Color32::from_rgb(90, 235, 165), Color32::from_rgb(60, 140, 110)),
                        ToastKind::Info => (ACCENT, Color32::from_rgb(140, 100, 40)),
                        ToastKind::Error => (Color32::from_rgb(255, 110, 110), Color32::from_rgb(150, 60, 60)),
                    };
                    egui::Frame::new()
                        .fill(Color32::from_rgba_unmultiplied(28, 20, 42, (240.0 * a) as u8))
                        .stroke(egui::Stroke::new(1.0_f32, edge.gamma_multiply(a)))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("●").size(10.0).color(dot.gamma_multiply(a)));
                                ui.label(RichText::new(&t.text).size(12.5).color(Color32::from_rgb(235, 230, 245).gamma_multiply(a)));
                            });
                        });
                    ui.add_space(6.0);
                }
            });
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    /// The Related files panel (right side). Declared before the central panel of a tab.
    pub(crate) fn related_side_panel(&mut self, ctx: &egui::Context) {
        if !self.ctx_ui.related_open {
            return;
        }
        let mut open_entry: Option<usize> = None;
        let mut close = false;
        egui::SidePanel::right("related_files")
            .default_width(340.0)
            .min_width(260.0)
            .frame(egui::Frame::new().fill(PANEL_BG).inner_margin(10.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("RELATED FILES").size(12.0).strong().color(ACCENT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Close").clicked() {
                            close = true;
                        }
                    });
                });
                let Some(sel) = self.selected.or(self.model_entry) else {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Select a file to see what belongs with it.").color(MUTED));
                    return;
                };
                let entry = self.state.lock().unwrap().entries.get(sel).cloned();
                let Some(entry) = entry else { return };
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(entry.kind).strong().size(11.0).color(kind_color(entry.kind)));
                    ui.add(egui::Label::new(RichText::new(&entry.name).size(12.5).color(Color32::WHITE)).truncate());
                });
                ui.label(RichText::new(&entry.rpf_name).size(10.5).color(MUTED));
                ui.add_space(6.0);
                let Some(view) = self.ctx_ui.related.as_ref().filter(|v| v.entry == sel) else {
                    ui.horizontal(|ui| {
                        crate::draw_spinner(ui, 14.0);
                        ui.label(RichText::new("Indexing names…").color(MUTED));
                    });
                    return;
                };
                let mut all: Vec<Related> = view.list.clone();
                if let Some(t) = &view.textures {
                    relations::merge(&mut all, t.clone(), sel);
                }
                let resolving = view.textures.is_none();
                let note = view.texture_note.clone();
                ui.horizontal_wrapped(|ui| {
                    let n = all.len();
                    if ui.add_enabled(n > 0, egui::Button::new(format!("Copy {n} Paths"))).clicked() {
                        self.ctx_ui.pending.push(Action::CopyRelatedPaths(sel));
                    }
                    if ui.button("Extract All…").on_hover_text("This file and its related files into one folder, with manifest.json").clicked() {
                        self.ctx_ui.pending.push(Action::ExtractRelated(sel));
                    }
                    if ui.button("AI Context").on_hover_text("Copy this asset as AI context, related files included").clicked() {
                        self.ctx_ui.pending.push(Action::AiContext { targets: vec![sel], json: false, head: false });
                    }
                });
                ui.add_space(6.0);
                ui.separator();
                if resolving {
                    ui.horizontal(|ui| {
                        crate::draw_spinner(ui, 14.0);
                        ui.label(RichText::new("Resolving shader textures…").size(11.5).color(MUTED));
                    });
                } else if !note.is_empty() {
                    ui.label(RichText::new(note).size(10.5).color(MUTED));
                }
                if all.is_empty() {
                    if !resolving {
                        ui.add_space(10.0);
                        ui.label(RichText::new("No related files found").strong().color(Color32::from_rgb(220, 210, 235)));
                        ui.label(
                            RichText::new(format!("Rules checked for {}: {}.", entry.kind, rules_for(entry.kind).iter().map(|r| r.title().to_lowercase()).collect::<Vec<_>>().join(", ")))
                                .size(11.0)
                                .color(MUTED),
                        );
                        ui.label(RichText::new("Percy only links files by these rules, so an empty list means none matched, not that nothing exists.").size(11.0).color(MUTED));
                    }
                    return;
                }
                let state = Arc::clone(&self.state);
                let rows: Vec<(Related, Option<RpfEntry>)> = {
                    let st = state.lock().unwrap();
                    all.iter().map(|r| (r.clone(), st.entries.get(r.entry).cloned())).collect()
                };
                egui::ScrollArea::vertical().id_salt("related_scroll").auto_shrink([false, false]).show(ui, |ui| {
                    let mut i = 0;
                    while i < rows.len() {
                        let rule = rows[i].0.rule;
                        let group: Vec<&(Related, Option<RpfEntry>)> = rows[i..].iter().take_while(|r| r.0.rule == rule).collect();
                        i += group.len();
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(rule.title()).size(12.0).strong().color(Color32::from_rgb(230, 220, 245)));
                            ui.label(RichText::new(format!("{}", group.len())).size(11.0).color(MUTED));
                            ui.label(RichText::new("ⓘ").size(11.0).color(MUTED)).on_hover_text(rule.explain());
                        });
                        for (r, e) in group {
                            let Some(e) = e else { continue };
                            let resp = ui
                                .horizontal(|ui| {
                                    ui.add_space(4.0);
                                    ui.label(RichText::new(e.kind).size(10.5).strong().color(kind_color(e.kind)));
                                    let label = if e.source_rpf == entry.source_rpf { e.name.clone() } else { format!("{} · {}", e.name, e.rpf_name) };
                                    let resp = ui.add(egui::Button::new(RichText::new(label).size(12.0)).frame(false).truncate());
                                    resp
                                })
                                .inner;
                            let resp = resp.on_hover_text(format!("{}\n{}", cc::full_path(e), r.why));
                            if resp.clicked() {
                                open_entry = Some(r.entry);
                            }
                            attach_entry_menu(&resp, &mut self.ctx_ui, &state, r.entry, Vec::new);
                            ui.label(RichText::new(format!("      {}", r.why)).size(10.5).color(MUTED));
                        }
                    }
                });
            });
        if close {
            self.ctx_ui.related_open = false;
        }
        if let Some(i) = open_entry {
            self.open_related_entry(i);
        }
    }
}

/// Rules that can apply to a kind (for the empty state).
fn rules_for(kind: &str) -> Vec<Rule> {
    match kind {
        "VEH" => vec![Rule::DetailLevel, Rule::MapLod, Rule::VehicleConfig, Rule::VehiclePart, Rule::ShaderTexture, Rule::SameStem, Rule::ClipDictHash],
        "MODEL" => vec![
            Rule::DetailLevel,
            Rule::MapLod,
            Rule::VehicleBody,
            Rule::PedEyes,
            Rule::PedPart,
            Rule::CollisionBounds,
            Rule::ShaderTexture,
            Rule::SameStem,
            Rule::ClipDictHash,
        ],
        "ANIM" => vec![Rule::ClipDictHash],
        "AWC" => vec![Rule::AudioCatalog, Rule::AudioPack],
        "DAT" => vec![Rule::AudioCatalogBank],
        "NAVM" => vec![Rule::NavNeighbor],
        "TTPF" => Vec::new(),
        _ => vec![Rule::SameStem],
    }
}

fn safe_file_name(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' }).collect()
}

/// What [`extract_bundle`] wrote.
struct Extracted {
    folder: PathBuf,
    /// Entries with output kept (in full, or in part for a TXD).
    written: usize,
    /// Entries that failed (decode or write), or that were written only in part (a TXD with
    /// some textures missing). Each has an `"error"` in the manifest.
    failed: Vec<String>,
}

impl Extracted {
    fn toast(&self) -> (String, ToastKind) {
        let at = self.folder.display();
        match (self.written, self.failed.len()) {
            (n, 0) => (format!("Extracted {n} files to {at}"), ToastKind::Ok),
            (0, f) => (format!("Extract failed for all {f} files (see manifest.json in {at})"), ToastKind::Error),
            (n, f) => (
                format!("Extracted {n} files to {at}; {f} failed or are incomplete: {} (manifest.json says why)", self.failed.join(", ")),
                ToastKind::Error,
            ),
        }
    }
}

/// Writes the main entry and its related files into `<dir>/<stem>_related/` with a
/// manifest.json that records where each file came from, why it was included, and any error.
/// Files that were written are kept when others fail.
fn extract_bundle(dir: &Path, items: &[(RpfEntry, Option<Related>)]) -> Result<Extracted, String> {
    let main = &items.first().ok_or("nothing to extract")?.0;
    let folder = dir.join(format!("{}_related", safe_file_name(&cc::raw_stem(&main.name))));
    fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let mut written = 0usize;
    let mut failed = Vec::new();
    let mut rows = Vec::new();
    for (n, (e, rel)) in items.iter().enumerate() {
        let base = format!("{n:02}_{}", safe_file_name(&e.name.replace('#', "")));
        // the file (or folder) written, and an error if the entry failed or is incomplete
        let (file, err): (Option<String>, Option<String>) = match e.kind {
            "MODEL" | "VEH" => match crate::decode_rage9_model_from_rpf(&e.source_rpf, e.offset, &e.name) {
                Ok(m) => {
                    let f = format!("{base}.obj");
                    match m.export_obj(&folder.join(&f)) {
                        Ok(()) => (Some(f), None),
                        Err(err) => (None, Some(format!("write failed: {err}"))),
                    }
                }
                Err(err) => (None, Some(err)),
            },
            "TTPF" => match crate::tex::decode_ttpf_texture(&e.source_rpf, e.tracks as usize) {
                Ok(t) => {
                    let f = format!("{base}.png");
                    match crate::tex::export_texture_png(&t, &folder.join(&f)) {
                        Ok(()) => (Some(f), None),
                        Err(err) => (None, Some(format!("write failed: {err}"))),
                    }
                }
                Err(err) => (None, Some(err)),
            },
            "TXD" => match crate::tex::decode_txd_from_rpf(&e.source_rpf, e.offset) {
                Ok(texs) => {
                    let sub = format!("{base}_textures");
                    match fs::create_dir_all(folder.join(&sub)) {
                        Err(err) => (None, Some(format!("write failed: {err}"))),
                        Ok(()) => {
                            let errs: Vec<String> = texs
                                .iter()
                                .filter_map(|t| {
                                    let name = format!("{}.png", safe_file_name(&t.name));
                                    crate::tex::export_texture_png(t, &folder.join(&sub).join(&name)).err().map(|err| format!("{name}: {err}"))
                                })
                                .collect();
                            match errs.len() {
                                0 => (Some(sub), None),
                                k if k == texs.len() => (None, Some(format!("no textures written: {}", errs[0]))),
                                k => (Some(sub), Some(format!("{k} of {} textures not written: {}", texs.len(), errs.join("; ")))),
                            }
                        }
                    }
                }
                Err(err) => (None, Some(err)),
            },
            _ => match read_entry_bytes(e) {
                Ok(b) => match fs::write(folder.join(&base), b) {
                    Ok(()) => (Some(base.clone()), None),
                    Err(err) => (None, Some(format!("write failed: {err}"))),
                },
                Err(err) => (None, Some(err.to_string())),
            },
        };
        // counted apart: a TXD with some textures written is both kept and incomplete
        if file.is_some() {
            written += 1;
        }
        if err.is_some() {
            failed.push(e.name.clone());
        }
        let mut fields = vec![
            format!("\"path\":{}", cc::json_str(&cc::full_path(e))),
            format!("\"kind\":{}", cc::json_str(e.kind)),
            format!("\"offset\":{}", cc::json_str(&format!("0x{:x}", e.offset))),
            format!("\"size\":{}", e.raw_size),
            format!("\"file\":{}", file.as_deref().map_or("null".into(), cc::json_str)),
        ];
        match rel {
            None => fields.push("\"role\":\"selected\"".into()),
            Some(r) => {
                fields.push(format!("\"rule\":{}", cc::json_str(r.rule.title())));
                fields.push(format!("\"why\":{}", cc::json_str(&r.why)));
            }
        }
        if let Some(err) = err {
            fields.push(format!("\"error\":{}", cc::json_str(&err)));
        }
        rows.push(format!("    {{{}}}", fields.join(",")));
    }
    let manifest = format!(
        "{{\n  \"tool\":{},\n  \"selected\":{},\n  \"files\":[\n{}\n  ]\n}}\n",
        cc::json_str(&format!("Percy6Stars {}", crate::APP_VERSION)),
        cc::json_str(&cc::full_path(main)),
        rows.join(",\n")
    );
    fs::write(folder.join("manifest.json"), manifest).map_err(|e| e.to_string())?;
    Ok(Extracted { folder, written, failed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_selection_follows_view_order() {
        let multi: std::collections::HashSet<usize> = [9, 3, 5].into_iter().collect();
        assert_eq!(ordered_selection(&[1, 3, 5, 7, 9], &multi), vec![3, 5, 9]);
        assert!(ordered_selection(&[1, 3], &[3].into_iter().collect()).is_empty());
    }

    #[test]
    fn bundle_writes_files_and_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("plain.rpf");
        fs::write(&src, b"hello world payload").unwrap();
        let e = RpfEntry {
            source_rpf: src.clone(),
            rpf_name: "plain.rpf".into(),
            name: "notes #1.bin".into(),
            kind: "BIN",
            offset: 6,
            comp_size: 5,
            raw_size: 5,
            tracks: 0,
            chunks: Vec::new(),
        };
        let rel = Related { entry: 1, rule: Rule::SameStem, why: "x".into() };
        let x = extract_bundle(dir.path(), &[(e.clone(), None), (e.clone(), Some(rel.clone()))]).unwrap();
        assert_eq!((x.written, x.failed.len()), (2, 0));
        assert!(x.toast().1 == ToastKind::Ok);
        let folder = x.folder;
        assert_eq!(fs::read(folder.join("00_notes_1.bin")).unwrap(), b"world");
        let m = fs::read_to_string(folder.join("manifest.json")).unwrap();
        assert!(m.contains("\"selected\":\"plain.rpf/notes #1.bin\""));
        assert!(m.contains("\"role\":\"selected\""));
        assert!(m.contains("\"rule\":\"Same name, other kinds\""));

        // a file that can't be written is reported, not counted, and the others are kept
        fs::remove_file(folder.join("01_notes_1.bin")).unwrap();
        fs::create_dir(folder.join("01_notes_1.bin")).unwrap();
        let x = extract_bundle(dir.path(), &[(e.clone(), None), (e, Some(rel))]).unwrap();
        assert_eq!((x.written, x.failed.len()), (1, 1));
        assert!(x.toast().1 == ToastKind::Error);
        assert_eq!(fs::read(folder.join("00_notes_1.bin")).unwrap(), b"world");
        let m = fs::read_to_string(folder.join("manifest.json")).unwrap();
        assert!(m.contains("\"file\":null") && m.contains("\"error\":\"write failed"));
    }
}
