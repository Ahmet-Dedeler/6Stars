//! In-panel Bink 2 (.bk2) playback on macOS.
//!
//! There is no open Bink 2 decoder, so decoding stays in RAD's player, run through Bink Player.app
//! (github.com/Ahmet-Dedeler/bink-player-mac) in its embed mode: the player window stays
//! invisible and every decoded frame is copied into a shared file that we map into an egui
//! texture. Audio plays from the player process. Control goes back through the same file.
//!
//! Shared file layout (see bink-player `src/embed.inc`):
//!   0x00 magic 'KBEM', version | 0x08 u64 seq (latest frame in slot seq & 1)
//!   0x10 width, height, stride, pixfmt | 0x20 frame, frames, fps_num, fps_den
//!   0x30 paused, muted, f32 volume, pid | 0x40 cmd_seq, cmd, f64 arg (we write) | 0x50 loop
//!   0x80 slot 0, slot 1 (stride * height each, BGRA)

use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const MAGIC: u32 = 0x4D45_424B;
const HDR: u64 = 0x80;
const CMD_TOGGLE_PAUSE: u32 = 1;
const CMD_SEEK_SECS: u32 = 2;
const CMD_VOLUME: u32 = 3;
const CMD_TOGGLE_MUTE: u32 = 4;
const CMD_LOOP: u32 = 5;
const CMD_QUIT: u32 = 6;

#[derive(Default, Clone, Copy)]
pub struct EmbedStatus {
    pub width: u32,
    pub height: u32,
    pub frame: u32,
    pub frames: u32,
    pub fps: f64,
    pub paused: bool,
    pub muted: bool,
}

/// Bink Player.app binaries if installed and new enough to have embed mode.
fn player_paths() -> Option<(PathBuf, PathBuf)> {
    let mut roots = vec![PathBuf::from("/Applications/Bink Player.app")];
    if let Some(h) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(h).join("Applications/Bink Player.app"));
    }
    roots.into_iter().find_map(|r| {
        let macos = r.join("Contents/MacOS");
        let (bin, dylib) = (macos.join("BinkMacArmPlayer"), macos.join("libBinkHUD.dylib"));
        let has_embed = fs::read(&dylib).ok()?.windows(13).any(|w| w == b"BINKHUD_EMBED");
        (bin.exists() && has_embed).then_some((bin, dylib))
    })
}

/// True when in-panel playback can be used on this machine.
pub fn available() -> bool {
    cfg!(target_os = "macos") && player_paths().is_some()
}

pub struct BinkEmbed {
    child: Child,
    shm_path: PathBuf,
    shm: File,
    pub clip: PathBuf,
    title: String,
    last_seq: u64,
    cmd_seq: u32,
    tex: Option<TextureHandle>,
    pub status: EmbedStatus,
    scrub: Option<f64>,
    exited: bool,
}

impl BinkEmbed {
    pub fn spawn(clip: &Path, title: &str, volume: f32, looping: bool) -> Result<Self, String> {
        let (bin, dylib) = player_paths().ok_or("Bink Player.app with embed support not installed")?;
        let shm_path = clip.with_extension("embed");
        fs::write(&shm_path, [0u8; HDR as usize]).map_err(|e| format!("embed file: {e}"))?;
        let shm = OpenOptions::new().read(true).write(true).open(&shm_path).map_err(|e| e.to_string())?;
        let child = Command::new(bin)
            .args(["-m", "-l"])
            .arg(clip)
            .env("DYLD_INSERT_LIBRARIES", dylib)
            .env("BINKHUD_EMBED", &shm_path)
            .env("BINKHUD_VOLUME", format!("{volume:.3}"))
            .env("BINKHUD_LOOP", if looping { "1" } else { "0" })
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("start player: {e}"))?;
        Ok(Self { child, shm_path, shm, clip: clip.to_path_buf(), title: title.to_string(), last_seq: 0, cmd_seq: 0, tex: None, status: EmbedStatus::default(), scrub: None, exited: false })
    }

    fn send(&mut self, cmd: u32, arg: f64) {
        self.cmd_seq = self.cmd_seq.wrapping_add(1);
        let mut b = [0u8; 12];
        b[..4].copy_from_slice(&cmd.to_le_bytes());
        b[4..].copy_from_slice(&arg.to_le_bytes());
        // payload first, sequence number last so the player never sees a half-written command
        let _ = self.shm.write_all_at(&b, 0x44);
        let _ = self.shm.write_all_at(&self.cmd_seq.to_le_bytes(), 0x40);
    }

    fn read_header(&self) -> Option<([u8; 0x58], u64)> {
        let mut h = [0u8; 0x58];
        self.shm.read_exact_at(&mut h, 0).ok()?;
        let r32 = |o: usize| u32::from_le_bytes(h[o..o + 4].try_into().unwrap());
        (r32(0) == MAGIC).then(|| (h, u64::from_le_bytes(h[8..16].try_into().unwrap())))
    }

    /// Pulls status and, if a new frame was published, uploads it. Returns false once the player is gone.
    fn poll(&mut self, ctx: &egui::Context) -> bool {
        if !self.exited {
            self.exited = matches!(self.child.try_wait(), Ok(Some(_)));
        }
        let Some((h, seq)) = self.read_header() else { return !self.exited };
        let r32 = |o: usize| u32::from_le_bytes(h[o..o + 4].try_into().unwrap());
        let (w, hh, stride) = (r32(0x10), r32(0x14), r32(0x18));
        let (fnum, fden) = (r32(0x28), r32(0x2c));
        self.status = EmbedStatus {
            width: w,
            height: hh,
            frame: r32(0x20),
            frames: r32(0x24),
            fps: if fnum > 0 && fden > 0 { fnum as f64 / fden as f64 } else { 30.0 },
            paused: r32(0x30) != 0,
            muted: r32(0x34) != 0,
        };
        if seq == 0 || seq == self.last_seq || w == 0 || hh == 0 || stride < w * 4 {
            return !self.exited;
        }
        let bytes = stride as usize * hh as usize;
        let mut px = vec![0u8; bytes];
        if self.shm.read_exact_at(&mut px, HDR + (seq & 1) * bytes as u64).is_err() {
            return !self.exited;
        }
        // seqlock: the slot we read is only rewritten two frames later
        if self.read_header().map_or(true, |(_, s2)| s2 > seq + 1) {
            return !self.exited;
        }
        self.last_seq = seq;
        let mut rgba = Vec::with_capacity(w as usize * hh as usize * 4);
        for row in px.chunks_exact(stride as usize) {
            for p in row[..w as usize * 4].chunks_exact(4) {
                rgba.extend_from_slice(&[p[2], p[1], p[0], 255]);
            }
        }
        let img = ColorImage::from_rgba_unmultiplied([w as usize, hh as usize], &rgba);
        match &mut self.tex {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture("bk2_embed", img, TextureOptions::LINEAR)),
        }
        true
    }

    /// Draws the video and its transport bar. Returns true when the user asked to pop it out.
    pub fn ui(&mut self, ui: &mut egui::Ui, volume: &mut f32, looping: &mut bool) -> bool {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        ctx.request_repaint_after(Duration::from_millis(12));
        let st = self.status;
        let mut pop_out = false;

        ui.horizontal(|ui| {
            ui.label(RichText::new(&self.title).strong().color(Color32::from_rgb(252, 175, 23)));
            if st.width > 0 {
                ui.label(RichText::new(format!("{}x{} · {:.2} fps · {} frames", st.width, st.height, st.fps, st.frames)).color(Color32::from_rgb(170, 200, 230)));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Pop out").on_hover_text("Open in Bink Player's own window").clicked() {
                    pop_out = true;
                }
            });
        });
        ui.add_space(4.0);

        const BAR_H: f32 = 34.0;
        let avail = ui.available_size();
        let area = egui::vec2(avail.x, (avail.y - BAR_H - 8.0).max(80.0));
        let (rect, resp) = ui.allocate_exact_size(area, egui::Sense::click());
        ui.painter().rect_filled(rect, 6.0, Color32::BLACK);
        if let Some(t) = &self.tex {
            let [tw, th] = t.size();
            let s = (rect.width() / tw as f32).min(rect.height() / th as f32);
            let r = egui::Rect::from_center_size(rect.center(), egui::vec2(tw as f32 * s, th as f32 * s));
            ui.painter().image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        } else {
            let msg = if self.exited { "Bink player exited before the first frame" } else { "Starting Bink decoder…" };
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(14.0), Color32::from_rgb(150, 150, 170));
        }
        if resp.clicked() {
            self.send(CMD_TOGGLE_PAUSE, 0.0);
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let space = ui.input(|i| i.key_pressed(egui::Key::Space)) && !ui.ctx().wants_keyboard_input();
            if ui.button(if st.paused { "▶ Play" } else { "⏸ Pause" }).clicked() || space {
                self.send(CMD_TOGGLE_PAUSE, 0.0);
            }
            let total = if st.fps > 0.0 { st.frames as f64 / st.fps } else { 0.0 };
            let cur = if st.fps > 0.0 { st.frame.saturating_sub(1) as f64 / st.fps } else { 0.0 };
            let mut pos = self.scrub.unwrap_or(cur);
            ui.label(RichText::new(format!("{} / {}", fmt_time(pos), fmt_time(total))).monospace().color(Color32::from_rgb(200, 200, 215)));
            let slider_w = (ui.available_width() - 250.0).max(80.0);
            ui.spacing_mut().slider_width = slider_w;
            let r = ui.add(egui::Slider::new(&mut pos, 0.0..=total.max(0.01)).show_value(false));
            if r.dragged() {
                self.scrub = Some(pos);
            }
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.send(CMD_SEEK_SECS, pos);
                self.scrub = None;
            }
            ui.spacing_mut().slider_width = 70.0;
            if ui.selectable_label(st.muted, if st.muted { "🔇" } else { "🔊" }).clicked() {
                self.send(CMD_TOGGLE_MUTE, 0.0);
            }
            if ui.add(egui::Slider::new(volume, 0.0..=1.0).show_value(false)).changed() {
                self.send(CMD_VOLUME, *volume as f64);
            }
            if ui.selectable_label(*looping, "Loop").clicked() {
                *looping = !*looping;
                self.send(CMD_LOOP, if *looping { 1.0 } else { 0.0 });
            }
        });
        pop_out
    }
}

impl Drop for BinkEmbed {
    fn drop(&mut self) {
        self.send(CMD_QUIT, 0.0);
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.shm_path);
    }
}

fn fmt_time(s: f64) -> String {
    let s = s.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Silent end-to-end check against the installed Bink Player.app:
    /// PERCYSTAR_BK2=<file.bk2> cargo test --release bink_embed -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bink_embed_plays_pauses_and_seeks() {
        let Ok(src) = std::env::var("PERCYSTAR_BK2") else { return };
        assert!(available(), "Bink Player.app with embed mode not installed");
        let clip = std::env::temp_dir().join("percystar_embed_test.bk2");
        fs::copy(&src, &clip).unwrap();
        let ctx = egui::Context::default();
        let mut be = BinkEmbed::spawn(&clip, "test", 0.0, true).unwrap();
        let wait = |be: &mut BinkEmbed, ms: u64| {
            let t = std::time::Instant::now();
            let mut frames = 0;
            while t.elapsed() < Duration::from_millis(ms) {
                let before = be.last_seq;
                be.poll(&ctx);
                if be.last_seq != before { frames += 1; }
                std::thread::sleep(Duration::from_millis(5));
            }
            frames
        };
        wait(&mut be, 2500);
        let got = wait(&mut be, 2000);
        let st = be.status;
        println!("{}x{} {:.2}fps frame {}/{} uploaded {got} frames in 2s", st.width, st.height, st.fps, st.frame, st.frames);
        assert!(be.tex.is_some() && got as f64 > st.fps * 1.5, "frames should stream at ~video fps");
        be.send(CMD_TOGGLE_PAUSE, 0.0);
        wait(&mut be, 600);
        let f0 = be.status.frame;
        wait(&mut be, 800);
        assert!(be.status.paused && be.status.frame == f0, "pause should hold the frame");
        be.send(CMD_SEEK_SECS, 1.0);
        wait(&mut be, 800);
        println!("after seek to 1s: frame {}", be.status.frame);
        assert!((be.status.frame as f64 - st.fps).abs() <= 3.0, "seek should land near 1s");
        drop(be);
        let _ = fs::remove_file(&clip);
    }
}
