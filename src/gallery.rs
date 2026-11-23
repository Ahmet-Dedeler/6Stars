// Percy6Stars by Ahmet Dedeler (https://github.com/Ahmet-Dedeler, https://ahmetdedeler.com)
// Fork of PercyStar by rpatty-dev (https://github.com/rpatty-dev/PercyStar).
// Source: https://github.com/Ahmet-Dedeler/Percy6Stars
// License: PercyStar Non-Commercial Attribution & Copyleft License v1.2 (see LICENSE)
// If you copy, vendor or port this file, keep this header and credit both projects in your README.
//! Thumbnail grid for texture entries in the Explorer (TTPF textures and TXD dictionaries).
//!
//! Suggested by @paryzius, whose ttpf-viewer (github.com/paryzius/ttpf-viewer) browses a
//! .ttpf as a gallery. Thumbnails are decoded on background threads, only for tiles that
//! are on screen, newest request first, so scrolling through 10k textures stays smooth.

use crate::tex::{decode_ttpf_thumbnail, decode_txd_thumbnail};
use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::SystemTime;

/// Identifies one texture entry independently of the current scan's entry order.
#[derive(Clone, Hash, PartialEq, Eq)]
pub struct ThumbKey {
    pub source: PathBuf,
    pub revision: Option<(u64, Option<SystemTime>)>,
    pub offset: u64,
    /// TTPF descriptor index (`RpfEntry::tracks`); 0 for TXD.
    pub index: u32,
    pub kind: &'static str,
}

enum Thumb {
    Pending,
    Ready { tex: TextureHandle, size: [usize; 2], reduced: bool },
    Failed(String),
}

struct CachedThumb {
    image: Thumb,
    max: usize,
    last_visible: u64,
}

struct Job {
    key: ThumbKey,
    max: usize,
    ctx: egui::Context,
}

type Done = (ThumbKey, Result<(ColorImage, bool), String>);

#[derive(Default)]
struct Queue {
    jobs: Vec<Job>,
    shutdown: bool,
}

/// Size (longest side, in points) the grid draws thumbnails at; decoded at up to 2x for
/// sharp display on Retina screens.
pub const SIZES: std::ops::RangeInclusive<f32> = 64.0..=256.0;
const MAX_QUEUED: usize = 192;
const MAX_CACHED: usize = 3000;
const MAX_CACHE_BYTES: usize = 64 << 20;

pub struct Gallery {
    thumbs: HashMap<ThumbKey, CachedThumb>,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    done_rx: mpsc::Receiver<Done>,
    pub size: f32,
    checker: Option<TextureHandle>,
    /// Scroll offset and visible height from the last frame, to keep the selection in view.
    view: (f32, f32),
    frame: u64,
}

impl Drop for Gallery {
    fn drop(&mut self) {
        self.queue.0.lock().unwrap().shutdown = true;
        self.queue.1.notify_all();
    }
}

/// Box-filter `pixels` (w x h) down so the longest side is at most `max`.
fn shrink(pixels: &[Color32], w: usize, h: usize, max: usize) -> ColorImage {
    let s = (w.max(h) as f32 / max as f32).max(1.0);
    let (tw, th) = (((w as f32 / s).round() as usize).max(1), ((h as f32 / s).round() as usize).max(1));
    if (tw, th) == (w, h) {
        return ColorImage { size: [w, h], pixels: pixels.to_vec() };
    }
    let mut out = Vec::with_capacity(tw * th);
    for ty in 0..th {
        let (y0, y1) = (ty * h / th, ((ty + 1) * h / th).max(ty * h / th + 1));
        for tx in 0..tw {
            let (x0, x1) = (tx * w / tw, ((tx + 1) * w / tw).max(tx * w / tw + 1));
            let mut acc = [0u32; 4];
            for y in y0..y1 {
                for p in &pixels[y * w + x0..y * w + x1] {
                    for (a, v) in acc.iter_mut().zip(p.to_array()) {
                        *a += v as u32;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let [r, g, b, a] = acc.map(|v| (v / n) as u8);
            out.push(Color32::from_rgba_premultiplied(r, g, b, a));
        }
    }
    ColorImage { size: [tw, th], pixels: out }
}

fn decode(job: &Job) -> Result<(ColorImage, bool), String> {
    let k = &job.key;
    let (px, w, h, reduced) = match k.kind {
        "TTPF" => {
            let t = decode_ttpf_thumbnail(&k.source, k.index as usize)?;
            (t.pixels, t.w, t.h, t.full_size.is_some())
        }
        "TXD" => {
            let t = decode_txd_thumbnail(&k.source, k.offset)?;
            (t.pixels, t.w, t.h, false)
        }
        _ => return Err("not a texture".into()),
    };
    // decoded pixels are straight alpha; egui (and the box filter below) want premultiplied
    let px = crate::tex::premultiplied_for_egui(&px);
    Ok((shrink(&px, w, h, job.max), reduced))
}

/// Short reason for a tile that can't be shown.
fn short_reason(err: &str) -> String {
    if let Some(i) = err.find("BCPack (") {
        // "BCPack (BC7)" -> "BCPack BC7"
        let mode = err[i + 8..].split(')').next().unwrap_or("");
        format!("BCPack {mode}")
    } else if err.contains("BCPack") {
        "BCPack".into()
    } else if err.contains("Unsupported DXGI") {
        "format".into()
    } else if err.contains("too large") {
        "too large".into()
    } else {
        "error".into()
    }
}

impl Gallery {
    pub fn new() -> Self {
        let queue: Arc<(Mutex<Queue>, Condvar)> = Arc::default();
        let (done_tx, done_rx) = mpsc::sync_channel::<Done>(8);
        // Each decode can read and inflate several archive blocks before shrinking.
        for _ in 0..2 {
            let queue = Arc::clone(&queue);
            let tx = done_tx.clone();
            std::thread::spawn(move || loop {
                let job = {
                    let (lock, cv) = &*queue;
                    let mut q = lock.lock().unwrap();
                    loop {
                        if q.shutdown {
                            return;
                        }
                        // newest first: that's what is on screen now
                        if let Some(j) = q.jobs.pop() {
                            break j;
                        }
                        q = cv.wait(q).unwrap();
                    }
                };
                let r = decode(&job);
                if tx.send((job.key, r)).is_err() {
                    return;
                }
                job.ctx.request_repaint();
            });
        }
        Self { thumbs: HashMap::new(), queue, done_rx, size: 112.0, checker: None, view: (0.0, 0.0), frame: 0 }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        // Upload at most one channel's worth per frame, even if workers are still busy.
        for _ in 0..8 {
            let Ok((key, r)) = self.done_rx.try_recv() else { return };
            let Some(slot) = self.thumbs.get_mut(&key) else { continue };
            slot.image = match r {
                Ok((img, reduced)) => {
                    let size = img.size;
                    let tex = ctx.load_texture(format!("thumb_{}_{}_{}", key.source.display(), key.offset, key.index), img, TextureOptions::LINEAR);
                    Thumb::Ready { tex, size, reduced }
                }
                Err(e) => Thumb::Failed(short_reason(&e)),
            };
        }
        ctx.request_repaint();
    }

    fn request(&mut self, key: &ThumbKey, ctx: &egui::Context) {
        let max = ((self.size * ctx.pixels_per_point().clamp(1.0, 2.0)).ceil() as usize).next_power_of_two();
        if let Some(slot) = self.thumbs.get_mut(key) {
            slot.last_visible = self.frame;
            if !matches!(slot.image, Thumb::Ready { .. }) || slot.max >= max {
                return;
            }
        }
        self.thumbs.insert(key.clone(), CachedThumb { image: Thumb::Pending, max, last_visible: self.frame });
        let (lock, cv) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.jobs.push(Job { key: key.clone(), max, ctx: ctx.clone() });
        // Scrolled past: drop the oldest requests and let them be asked for again later.
        if q.jobs.len() > MAX_QUEUED {
            let excess = q.jobs.len() - MAX_QUEUED;
            let stale: Vec<Job> = q.jobs.drain(..excess).collect();
            for j in stale {
                self.thumbs.remove(&j.key);
            }
        }
        cv.notify_one();
    }

    fn trim_cache(&mut self) {
        let mut bytes = self.thumbs.values().map(|slot| match &slot.image {
            Thumb::Ready { size: [w, h], .. } => w * h * 4,
            _ => 0,
        }).sum::<usize>();
        if bytes <= MAX_CACHE_BYTES && self.thumbs.len() <= MAX_CACHED {
            return;
        }
        let mut evictable: Vec<_> = self.thumbs.iter().filter_map(|(key, slot)| {
            let size = match &slot.image {
                Thumb::Pending => return None,
                Thumb::Ready { size: [w, h], .. } => w * h * 4,
                Thumb::Failed(_) => 0,
            };
            Some((slot.last_visible, size, key.clone()))
        }).collect();
        evictable.sort_unstable_by_key(|&(last_visible, _, _)| last_visible);
        for (_, size, key) in evictable {
            self.thumbs.remove(&key);
            bytes -= size;
            if bytes <= MAX_CACHE_BYTES && self.thumbs.len() <= MAX_CACHED {
                break;
            }
        }
    }

    fn checker(&mut self, ctx: &egui::Context) -> egui::TextureId {
        self.checker
            .get_or_insert_with(|| {
                let (a, b) = (Color32::from_gray(58), Color32::from_gray(40));
                let pixels = (0..4).map(|i| if (i % 2) ^ (i / 2) == 0 { a } else { b }).collect();
                let opts = TextureOptions { magnification: egui::TextureFilter::Nearest, wrap_mode: egui::TextureWrapMode::Repeat, ..TextureOptions::NEAREST };
                ctx.load_texture("gallery_checker", ColorImage { size: [2, 2], pixels }, opts)
            })
            .id()
    }

    /// Draw the grid. `items` gives, for a visible index range of `count` entries, each
    /// entry's (entry index, key, label). Returns the entry clicked, if any.
    pub fn grid_ui(
        &mut self,
        ui: &mut egui::Ui,
        count: usize,
        selected: Option<usize>,
        items: impl Fn(std::ops::Range<usize>) -> Vec<(usize, Option<ThumbKey>, String)>,
        position_of: impl Fn(usize) -> Option<usize>,
        // right-click menu for a tile (the shared entry menu in context_ui)
        mut menu: impl FnMut(&egui::Response, usize),
    ) -> Option<usize> {
        let ctx = ui.ctx().clone();
        self.frame += 1;
        self.poll(&ctx);
        let checker = self.checker(&ctx);
        let gap = 6.0;
        let cell = self.size;
        let label_h = 16.0;
        let row_h = cell + label_h + gap;
        let cols = (((ui.available_width() + gap) / (cell + gap)).floor() as usize).max(1);
        let rows = count.div_ceil(cols);

        // keyboard: arrows move the selection through the grid
        let mut clicked = None;
        let mut scroll_to_row = None;
        let step = if ctx.memory(|m| m.focused().is_none()) {
            ctx.input(|i| {
                if i.key_pressed(egui::Key::ArrowRight) { 1 } else if i.key_pressed(egui::Key::ArrowLeft) { -1 }
                else if i.key_pressed(egui::Key::ArrowDown) { cols as isize } else if i.key_pressed(egui::Key::ArrowUp) { -(cols as isize) } else { 0 }
            })
        } else {
            0
        };
        if let Some(pos) = selected.filter(|_| step != 0 && count > 0).and_then(&position_of) {
            let np = (pos as isize + step).clamp(0, count as isize - 1) as usize;
            if np != pos {
                clicked = items(np..np + 1).first().map(|(idx, _, _)| *idx);
                scroll_to_row = Some(np / cols);
            }
        }

        let mut area = egui::ScrollArea::vertical().id_salt("texture_grid").auto_shrink([false, false]);
        if let Some(r) = scroll_to_row {
            // keep the keyboard selection in view
            let (top, bottom) = (r as f32 * row_h, (r + 1) as f32 * row_h);
            let (off, h) = self.view;
            if top < off {
                area = area.vertical_scroll_offset(top);
            } else if bottom > off + h {
                area = area.vertical_scroll_offset(bottom - h);
            }
        }
        // show_rows adds item_spacing.y per row; rows carry their own gap instead
        ui.spacing_mut().item_spacing.y = 0.0;
        let out = area.show_rows(ui, row_h, rows, |ui, row_range| {
            let first = row_range.start * cols;
            let last = (row_range.end * cols).min(count);
            let visible = items(first..last);
            // rows must be exactly row_h tall or show_rows miscomputes the scroll height
            for row in visible.chunks(cols) {
                let w = ui.available_width();
                ui.allocate_ui_with_layout(egui::vec2(w, row_h), egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                    ui.set_height(row_h);
                    ui.spacing_mut().item_spacing.x = gap;
                    for (idx, key, label) in row {
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(cell, cell + label_h), egui::Sense::click());
                        let img_rect = egui::Rect::from_min_size(rect.min, egui::vec2(cell, cell));
                        let p = ui.painter();
                        let is_sel = selected == Some(*idx);
                        p.rect_filled(img_rect, 3.0, Color32::from_rgb(30, 22, 44));
                        match key.as_ref().map(|k| {
                            self.request(k, &ctx);
                            self.thumbs.get(k).map(|slot| &slot.image)
                        }) {
                            Some(Some(Thumb::Ready { tex, size, reduced })) => {
                                let fit = (cell / size[0] as f32).min(cell / size[1] as f32);
                                let disp = egui::vec2(size[0] as f32 * fit, size[1] as f32 * fit);
                                let r = egui::Rect::from_center_size(img_rect.center(), disp);
                                let reps = disp / 8.0;
                                p.image(checker, r, egui::Rect::from_min_max(egui::Pos2::ZERO, reps.to_pos2()), Color32::WHITE);
                                p.image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                                if *reduced {
                                    p.text(img_rect.right_top() + egui::vec2(-4.0, 3.0), egui::Align2::RIGHT_TOP, "low-res", egui::FontId::proportional(10.0), Color32::from_rgb(255, 200, 90));
                                }
                            }
                            Some(Some(Thumb::Failed(why))) => {
                                p.text(img_rect.center(), egui::Align2::CENTER_CENTER, why, egui::FontId::proportional(11.0), Color32::from_rgb(150, 120, 150));
                            }
                            Some(_) => {
                                p.text(img_rect.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::proportional(18.0), Color32::from_rgb(120, 110, 140));
                            }
                            None => {
                                p.text(img_rect.center(), egui::Align2::CENTER_CENTER, "not a texture", egui::FontId::proportional(10.0), Color32::from_rgb(120, 110, 140));
                            }
                        }
                        let stroke = if is_sel {
                            egui::Stroke::new(2.0_f32, Color32::from_rgb(255, 168, 28))
                        } else if resp.hovered() {
                            egui::Stroke::new(1.0_f32, Color32::from_rgb(160, 130, 200))
                        } else {
                            egui::Stroke::NONE
                        };
                        if stroke.width > 0.0 {
                            p.rect_stroke(img_rect, 3.0, stroke, egui::StrokeKind::Inside);
                        }
                        let lr = egui::Rect::from_min_size(rect.min + egui::vec2(0.0, cell), egui::vec2(cell, label_h));
                        p.with_clip_rect(lr).text(
                            lr.left_center() + egui::vec2(2.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            label,
                            egui::FontId::proportional(10.5),
                            if is_sel { Color32::from_rgb(255, 168, 28) } else { Color32::from_rgb(175, 168, 195) },
                        );
                        if resp.clicked() {
                            clicked = Some(*idx);
                        }
                        menu(&resp, *idx);
                        resp.on_hover_text(RichText::new(label.clone()));
                    }
                });
            }
        });
        self.view = (out.state.offset.y, out.inner_rect.height());
        self.trim_cache();
        clicked
    }
}

/// Tile label from a scan entry name: TTPF `tex_00012_512x256_BC7_UNORM_mip.ttpf` -> `#12 · 512x256`.
pub fn tile_label(kind: &str, name: &str) -> String {
    if kind == "TTPF" {
        let mut parts = name.trim_start_matches("tex_").split('_');
        if let (Some(i), Some(dims)) = (parts.next(), parts.next()) {
            return format!("#{} · {dims}", i.trim_start_matches('0').parse::<u32>().unwrap_or(0));
        }
    }
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_completion_wakes_an_idle_ui() {
        let ctx = egui::Context::default();
        // Let egui settle its initial repaint before installing the callback.
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |_| {});
        }
        let (wake, woken) = mpsc::channel();
        ctx.set_request_repaint_callback(move |_| { let _ = wake.send(()); });
        let dir = tempfile::tempdir().unwrap();
        let key = ThumbKey { source: dir.path().join("missing.ttpf"), revision: None, offset: 0, index: 0, kind: "TTPF" };
        let mut gallery = Gallery::new();
        gallery.request(&key, &ctx);
        woken.recv_timeout(std::time::Duration::from_secs(5)).expect("worker did not wake the UI");
        gallery.poll(&ctx);
        assert!(matches!(gallery.thumbs[&key].image, Thumb::Failed(_)));
    }

    #[test]
    fn labels_and_shrink() {
        assert_eq!(tile_label("TTPF", "tex_00012_512x256_BC7_UNORM_mip.ttpf"), "#12 · 512x256");
        assert_eq!(tile_label("TTPF", "tex_00000_32x32_R8G8B8A8_UNORM.ttpf"), "#0 · 32x32");
        assert_eq!(tile_label("TXD", "hud.#td"), "hud.#td");
        let px = vec![Color32::from_rgb(200, 100, 50); 512 * 256];
        let img = shrink(&px, 512, 256, 128);
        assert_eq!(img.size, [128, 64]);
        assert_eq!(img.pixels[0], Color32::from_rgb(200, 100, 50));
        assert_eq!(shrink(&px[..64 * 32], 64, 32, 128).size, [64, 32]);
    }
}
