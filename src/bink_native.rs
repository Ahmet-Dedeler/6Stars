// Percy6Stars by Ahmet Dedeler (https://github.com/Ahmet-Dedeler, https://ahmetdedeler.com)
// Fork of PercyStar by rpatty-dev (https://github.com/rpatty-dev/PercyStar).
// Source: https://github.com/Ahmet-Dedeler/Percy6Stars
// License: PercyStar Non-Commercial Attribution & Copyleft License v1.2 (see LICENSE)
// If you copy, vendor or port this file, keep this header and credit both projects in your README.
//! In-panel Bink 2 (.bk2) playback on Linux.
//!
//! Video is decoded in-process by the clean-room `bink2` crate (imported as `bink2_decoder`)
//! (https://github.com/codingkoala/bink2-decoder, Apache-2.0) on a background thread, converted
//! to RGB the way RAD's tools do (BT.601 full range, centred bilinear chroma) and shown in an
//! egui texture. Bink Audio (track 0) is decoded up front on a second thread and played through
//! the AWC player's command-line backend (`audio_player::PcmPlayer`), restarted at the video's
//! position on play, seek and loop.

use crate::audio_player::PcmPlayer;
use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

enum Cmd {
    TogglePause,
    Seek(f64),
    Loop(bool),
    Volume(f32),
    Quit,
}

#[derive(Default, Clone, Copy)]
pub struct NativeStatus {
    pub width: u32,
    pub height: u32,
    /// 1-based number of the frame on screen (0 before the first one).
    pub frame: u32,
    pub frames: u32,
    pub fps: f64,
    pub paused: bool,
    pub has_audio: bool,
    /// A player process is playing the clip's sound right now.
    pub audio_playing: bool,
}

#[derive(Default)]
struct Shared {
    status: NativeStatus,
    seq: u64,
    image: Option<ColorImage>,
    error: Option<String>,
    /// Why the clip plays without sound (audio can't be decoded or played), if it has audio.
    audio_note: Option<String>,
    exited: bool,
    /// The sound player. It lives here rather than on the decoder thread so that closing the
    /// panel (or the app) stops the sound at once, without waiting for the detached thread.
    player: Option<PcmPlayer>,
    /// Set when the panel closed: the decoder thread must not start a new player.
    closed: bool,
}

/// Decoded audio track: interleaved 16-bit PCM, sample rate, channels.
type Pcm = (Arc<Vec<i16>>, u32, u16);

pub struct BinkNative {
    title: String,
    tx: Sender<Cmd>,
    shared: Arc<Mutex<Shared>>,
    tex: Option<TextureHandle>,
    last_seq: u64,
    status: NativeStatus,
    audio_note: Option<String>,
    scrub: Option<f64>,
    muted: bool,
}

impl BinkNative {
    /// Starts decoding `dir/clip.bk2`. `ctx` is used to wake the UI when a frame is ready.
    /// The clip is read and validated on the decoder thread, so a large clip doesn't stall the UI;
    /// read and header errors show up in the panel like decode errors.
    pub fn spawn(ctx: &egui::Context, dir: TempDir, title: String, volume: f32, looping: bool) -> Result<Self, String> {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, rx) = mpsc::channel();
        let (sh, ctx) = (Arc::clone(&shared), ctx.clone());
        // The thread is detached: it owns the clip folder and removes it when it ends, so closing
        // the panel never waits for it (it may still be reading a large clip).
        std::thread::Builder::new()
            .name("bink2-decode".into())
            .spawn(move || {
                // A decoder panic on a damaged clip must still reach the panel as an error,
                // not leave it on "Starting Bink decoder…" forever.
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    std::fs::read(dir.path().join("clip.bk2"))
                        .map_err(|e| format!("Read video clip: {e}"))
                        .and_then(|data| play(Arc::new(data), &sh, &ctx, rx, volume, looping))
                }))
                .unwrap_or_else(|_| Err("the decoder hit a bug on this clip".into()));
                drop(dir); // keep the clip folder until playback ends
                let player = {
                    let mut s = sh.lock().unwrap_or_else(|p| p.into_inner());
                    s.error = res.err();
                    s.exited = true;
                    s.status.audio_playing = false;
                    s.player.take()
                };
                drop(player); // stop audio immediately if video decode errored or panicked
                ctx.request_repaint();
            })
            .map_err(|e| format!("Start video decoder: {e}"))?;
        Ok(Self { title, tx, shared, tex: None, last_seq: 0, status: NativeStatus::default(), audio_note: None, scrub: None, muted: false })
    }

    /// Uploads a new frame if the decoder published one. Returns (exited, error).
    fn poll(&mut self, ctx: &egui::Context) -> (bool, Option<String>) {
        let mut s = self.shared.lock().unwrap_or_else(|p| p.into_inner());
        self.status = s.status;
        self.audio_note.clone_from(&s.audio_note);
        if s.seq != self.last_seq {
            if let Some(img) = s.image.take() {
                self.last_seq = s.seq;
                match &mut self.tex {
                    Some(t) => t.set(img, TextureOptions::LINEAR),
                    None => self.tex = Some(ctx.load_texture("bk2_native", img, TextureOptions::LINEAR)),
                }
            }
        }
        (s.exited, s.error.clone())
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, volume: &mut f32, looping: &mut bool) {
        let ctx = ui.ctx().clone();
        let (exited, error) = self.poll(&ctx);
        if !exited {
            // the decoder thread also wakes us per frame; this keeps the clock label moving
            ctx.request_repaint_after(Duration::from_millis(if self.status.paused { 250 } else { 100 }));
        }
        let st = self.status;

        ui.horizontal(|ui| {
            ui.label(RichText::new(&self.title).strong().color(Color32::from_rgb(252, 175, 23)));
            if st.width > 0 {
                ui.label(RichText::new(format!("{}x{} · {:.2} fps · {} frames", st.width, st.height, st.fps, st.frames)).color(Color32::from_rgb(170, 200, 230)));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("Bink Video by RAD Game Tools · decoded by bink2-decoder").small().weak());
            });
        });
        if let Some(err) = &error {
            ui.label(RichText::new(format!("The video decoder stopped: {err}")).color(Color32::from_rgb(240, 120, 120)));
        }
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
            let msg = if exited { "The video decoder stopped before the first frame" } else { "Starting Bink decoder…" };
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(14.0), Color32::from_rgb(150, 150, 170));
        }
        if resp.clicked() {
            self.send(Cmd::TogglePause);
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let space = ui.input(|i| i.key_pressed(egui::Key::Space)) && !ui.ctx().wants_keyboard_input();
            if ui.button(if st.paused { "▶ Play" } else { "⏸ Pause" }).clicked() || space {
                self.send(Cmd::TogglePause);
            }
            let total = if st.fps > 0.0 { st.frames as f64 / st.fps } else { 0.0 };
            let cur = if st.fps > 0.0 { st.frame.saturating_sub(1) as f64 / st.fps } else { 0.0 };
            let mut pos = self.scrub.unwrap_or(cur);
            ui.label(RichText::new(format!("{} / {}", fmt_time(pos), fmt_time(total))).monospace().color(Color32::from_rgb(200, 200, 215)));
            let slider_w = (ui.available_width() - 330.0).max(80.0);
            ui.spacing_mut().slider_width = slider_w;
            let r = ui.add(egui::Slider::new(&mut pos, 0.0..=total.max(0.01)).show_value(false));
            if r.dragged() {
                self.scrub = Some(pos);
            }
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.send(Cmd::Seek(pos));
                self.scrub = None;
            }
            if let Some(note) = &self.audio_note {
                ui.label(RichText::new(note).small().weak());
            } else if st.has_audio {
                if ui.selectable_label(self.muted, if self.muted { "🔇" } else { "🔊" }).clicked() {
                    self.muted = !self.muted;
                    self.send(Cmd::Volume(if self.muted { 0.0 } else { *volume }));
                }
                ui.spacing_mut().slider_width = 70.0;
                if ui.add(egui::Slider::new(volume, 0.0..=1.0).show_value(false)).changed() {
                    self.muted = false;
                    self.send(Cmd::Volume(*volume));
                }
            }
            if ui.selectable_label(*looping, "Loop").clicked() {
                *looping = !*looping;
                self.send(Cmd::Loop(*looping));
            }
        });
    }
}

impl Drop for BinkNative {
    /// Tells the decoder thread to stop without waiting for it: it checks for Quit between frames
    /// (and during seeks), and a read in progress finishes on its own.
    fn drop(&mut self) {
        self.send(Cmd::Quit);
        let player = {
            let mut s = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            s.closed = true;
            s.status.audio_playing = false;
            s.player.take()
        };
        drop(player); // stop the sound now
    }
}

/// Index of the last keyframe at or before `target`.
fn keyframe_before(frames: &[bink2_decoder::FrameEntry], target: usize) -> usize {
    (0..=target.min(frames.len().saturating_sub(1))).rev().find(|&i| frames[i].keyframe).unwrap_or(0)
}

/// Decodes audio track 0 of the clip in full (a few ms per minute of audio in release builds).
fn decode_audio(data: &[u8]) -> Result<Pcm, String> {
    let file = bink2_decoder::Container::parse(data).map_err(|e| e.to_string())?;
    let (pcm, rate, channels) = bink2_decoder::decode_track(&file, 0).map_err(|e| e.to_string())?;
    Ok((Arc::new(pcm), rate, channels))
}

/// Audio sample frame that plays together with video frame `frame` (audio starts at video time 0).
fn audio_frame_at(frame: usize, fps: f64, rate: u32) -> usize {
    (frame as f64 / fps * rate as f64).round() as usize
}

/// Decoder thread: keeps the playback clock, decodes every frame and publishes the ones that are
/// due. Audio plays from a separate player process that is restarted at the video position
/// whenever playback (re)starts. Returns when told to quit (Ok) or when the video cannot be
/// decoded (Err); audio problems only mute the clip.
fn play(data: Arc<Vec<u8>>, sh: &Mutex<Shared>, ctx: &egui::Context, rx: Receiver<Cmd>, mut volume: f32, mut looping: bool) -> Result<(), String> {
    let file = bink2_decoder::Container::parse(&data).map_err(|e| format!("Bink 2 header: {e}"))?;
    let hdr = &file.header;
    let n = file.num_frames();
    if n == 0 {
        return Err("the video has no frames".into());
    }
    let mut dec = bink2_decoder::Decoder::new(hdr).map_err(|e| e.to_string())?;
    dec.set_options(bink2_decoder::DecoderOptions { conceal_slice_errors: true, ..Default::default() });
    let fps = if hdr.fps_num > 0 && hdr.fps_den > 0 { hdr.fps_num as f64 / hdr.fps_den as f64 } else { 30.0 };
    let period = Duration::from_secs_f64(1.0 / fps);
    let has_audio = !hdr.audio_tracks.is_empty();
    {
        let mut s = sh.lock().unwrap();
        s.status = NativeStatus { width: hdr.width, height: hdr.height, frames: n as u32, fps, has_audio, ..Default::default() };
    }
    let set_note = |note: String| {
        sh.lock().unwrap().audio_note = Some(note);
        ctx.request_repaint();
    };

    // Audio is decoded next to the video so the first frame isn't delayed.
    let (atx, audio_rx) = mpsc::channel();
    let mut audio_pending = has_audio;
    if has_audio {
        let data = Arc::clone(&data);
        let spawned = std::thread::Builder::new().name("bink2-audio".into()).spawn(move || {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode_audio(&data)))
                .unwrap_or_else(|_| Err("audio decoder panicked on this clip".into()));
            let _ = atx.send(res);
        });
        if let Err(e) = spawned {
            audio_pending = false;
            set_note(format!("No audio: {e}"));
        }
    }
    let mut pcm: Option<Pcm> = None;
    let set_player = |p: Option<PcmPlayer>| {
        let mut s = sh.lock().unwrap_or_else(|p| p.into_inner());
        let new = if s.closed { None } else { p };
        let old = std::mem::replace(&mut s.player, new);
        s.status.audio_playing = s.player.is_some();
        drop(s);
        drop(old); // kills the old player process, outside the lock
    };
    // (Re)starts the sound at video frame `at`, or stops it when there is nothing to play.
    let restart_audio = |pcm: &Option<Pcm>, at: Option<usize>, volume: f32| {
        set_player(None);
        let (Some((samples, rate, ch)), Some(at)) = (pcm, at) else { return };
        match PcmPlayer::play_from(Arc::clone(samples), *rate, *ch, audio_frame_at(at, fps, *rate), volume) {
            Ok(p) => set_player(Some(p)),
            Err(e) => set_note(format!("No audio: {e}")),
        }
    };

    let mut next = 0usize; // next frame to decode
    let mut paused = false;
    let mut clock = Instant::now(); // when frame `next` is due
    let mut show_now = false; // publish the next decoded frame even if late or paused (after a seek)
    // When decoding can't keep up, late frames are dropped but one is still shown every few periods.
    let mut last_shown: Option<Instant> = None;
    let mut pending_vol_at: Option<Instant> = None;
    loop {
        if audio_pending && pcm.is_none() {
            match audio_rx.try_recv() {
                Ok(Ok(p)) => {
                    audio_pending = false;
                    pcm = Some(p);
                    // join the video where it is now
                    restart_audio(&pcm, (!paused && next < n).then_some(next), volume);
                }
                Ok(Err(e)) => {
                    audio_pending = false;
                    set_note(format!("No audio: {e}"));
                }
                Err(TryRecvError::Disconnected) => {
                    audio_pending = false;
                    set_note("No audio: audio decoder stopped unexpectedly".into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(at) = pending_vol_at {
            if at.elapsed() >= Duration::from_millis(250) {
                pending_vol_at = None;
                if !paused && next < n {
                    restart_audio(&pcm, Some(next), volume);
                }
            }
        }
        let base_wait = if show_now {
            Duration::ZERO
        } else if paused {
            Duration::from_millis(if audio_pending { 50 } else { 500 })
        } else {
            clock.saturating_duration_since(Instant::now())
        };
        let wait = match pending_vol_at {
            Some(at) => base_wait.min(Duration::from_millis(250).saturating_sub(at.elapsed())),
            None => base_wait,
        };
        match rx.recv_timeout(wait) {
            Err(RecvTimeoutError::Disconnected) | Ok(Cmd::Quit) => return Ok(()),
            Ok(Cmd::Loop(l)) => looping = l,
            Ok(Cmd::Volume(v)) => {
                volume = v;
                if pcm.is_some() && !paused && next < n {
                    pending_vol_at = Some(Instant::now());
                }
            }
            Ok(Cmd::TogglePause) => {
                paused = !paused;
                if !paused && next >= n {
                    next = 0; // play again from the top after stopping at the end
                }
                clock = Instant::now();
                restart_audio(&pcm, (!paused).then_some(next), volume);
                sh.lock().unwrap().status.paused = paused;
            }
            Ok(Cmd::Seek(mut secs)) => {
                // Rebuilding from the keyframe can take a while on clips with few keyframes, so
                // keep listening: Quit stops at once and a newer seek replaces this one.
                let target = 'seek: loop {
                    let target = ((secs * fps).round().max(0.0) as usize).min(n - 1);
                    dec.reset();
                    for i in keyframe_before(&hdr.frames, target)..target {
                        match rx.try_recv() {
                            Ok(Cmd::Quit) | Err(TryRecvError::Disconnected) => return Ok(()),
                            Ok(Cmd::Seek(s)) => {
                                secs = s;
                                continue 'seek;
                            }
                            Ok(Cmd::Loop(l)) => looping = l,
                            Ok(Cmd::Volume(v)) => volume = v, // the sound restarts after the seek
                            Ok(Cmd::TogglePause) => {
                                paused = !paused;
                                sh.lock().unwrap().status.paused = paused;
                            }
                            Err(TryRecvError::Empty) => {}
                        }
                        decode(&file, &mut dec, i)?;
                    }
                    break target;
                };
                next = target;
                show_now = true;
                clock = Instant::now();
                restart_audio(&pcm, (!paused).then_some(next), volume);
            }
            Err(RecvTimeoutError::Timeout) => {
                if paused && !show_now {
                    continue;
                }
                if next >= n {
                    if looping {
                        next = 0;
                        restart_audio(&pcm, Some(0), volume);
                    } else {
                        paused = true;
                        set_player(None);
                        sh.lock().unwrap().status.paused = true;
                        continue;
                    }
                }
                let frame = decode(&file, &mut dec, next)?;
                next += 1;
                let overdue = last_shown.is_none_or(|t| t.elapsed() >= period * 4);
                if show_now || overdue || Instant::now() <= clock + period {
                    last_shown = Some(Instant::now());
                    let img = to_color_image(&frame);
                    let mut s = sh.lock().unwrap();
                    s.seq += 1;
                    s.image = Some(img);
                    s.status.frame = next as u32;
                    drop(s);
                    ctx.request_repaint();
                }
                show_now = false;
                clock += period;
                if Instant::now() > clock + period * 8 {
                    clock = Instant::now(); // far behind (machine busy): stop catching up
                }
            }
        }
    }
}

fn decode(file: &bink2_decoder::Container, dec: &mut bink2_decoder::Decoder, i: usize) -> Result<bink2_decoder::VideoFrame, String> {
    let pkt = file.frame(i).map_err(|e| format!("frame {i}: {e}"))?;
    dec.decode(pkt.video, pkt.keyframe).map_err(|e| format!("frame {i}: {e}"))
}

/// YUV 4:2:0 to RGB like RAD Video Tools: BT.601 matrix at full range, chroma centred on 128.5,
/// centred bilinear chroma upsampling. Rows are split across threads.
fn to_color_image(f: &bink2_decoder::VideoFrame) -> ColorImage {
    let (w, h) = (f.width, f.height);
    // all-zero pixels: the allocation comes back zeroed from the OS, and the threads write it
    let mut pixels = vec![Color32::TRANSPARENT; w * h];
    let taps: Vec<(u32, u32, u32)> = (0..w).map(|x| chroma_taps(x, f.chroma_width())).map(|(a, b, k)| (a as u32, b as u32, k)).collect();
    let lut = conv_lut();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    let band = h.div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (b, out) in pixels.chunks_mut(band * w).enumerate() {
            let taps = &taps;
            s.spawn(move || {
                let mut rows = (vec![0u16; f.chroma_width()], vec![0u16; f.chroma_width()]);
                for (r, row) in out.chunks_mut(w).enumerate() {
                    convert_row(f, b * band + r, row, taps, lut, &mut rows);
                }
            });
        }
    });
    ColorImage { size: [w, h], pixels }
}

/// Chroma sample positions and weights (in quarters) for output coordinate `x` of a plane
/// `cw` samples wide: the chroma sample sits halfway between two output pixels.
#[inline]
fn chroma_taps(x: usize, cw: usize) -> (usize, usize, u32) {
    let c = x / 2;
    let last = cw - 1;
    if x.is_multiple_of(2) {
        (c.saturating_sub(1), c.min(last), 1) // 1/4 of the left sample, 3/4 of this one
    } else {
        (c.min(last), (c + 1).min(last), 3) // 3/4 of this one, 1/4 of the right sample
    }
}

/// The bilinear chroma sum is in 16ths (0 ..= 16·255). For each sum, the matrix terms
/// `1.402·v`, `0.344136·u`, `0.714136·v` and `1.772·u`, with u, v = sum / 16 − 128.5.
struct ConvLut {
    rv: Vec<f32>,
    gu: Vec<f32>,
    gv: Vec<f32>,
    bu: Vec<f32>,
}

fn conv_lut() -> &'static ConvLut {
    static LUT: std::sync::OnceLock<ConvLut> = std::sync::OnceLock::new();
    LUT.get_or_init(|| {
        let c: Vec<f32> = (0..=16 * 255).map(|s| s as f32 / 16.0 - 128.5).collect();
        ConvLut {
            rv: c.iter().map(|v| 1.402 * v).collect(),
            gu: c.iter().map(|u| 0.344_136 * u).collect(),
            gv: c.iter().map(|v| 0.714_136 * v).collect(),
            bu: c.iter().map(|u| 1.772 * u).collect(),
        }
    })
}

/// Converts output row `y`. `taps` holds `chroma_taps` for every column, `rows` is scratch for
/// the vertically blended chroma rows (in quarters).
fn convert_row(f: &bink2_decoder::VideoFrame, y: usize, row: &mut [Color32], taps: &[(u32, u32, u32)], lut: &ConvLut, rows: &mut (Vec<u16>, Vec<u16>)) {
    let (w, cw, ch) = (f.width, f.chroma_width(), f.chroma_height());
    let (r0, r1, wy) = chroma_taps(y, ch);
    // weight of the first row in quarters: even rows lean 1/4 on the row above
    let (ka, kb) = if wy == 1 { (1u16, 3u16) } else { (3, 1) };
    let blend = |plane: &[u8], out: &mut [u16]| {
        let (a, b) = (&plane[r0 * cw..r0 * cw + cw], &plane[r1 * cw..r1 * cw + cw]);
        for ((o, &a), &b) in out.iter_mut().zip(a).zip(b) {
            *o = a as u16 * ka + b as u16 * kb;
        }
    };
    blend(&f.u, &mut rows.0);
    blend(&f.v, &mut rows.1);
    let (us, vs) = (&rows.0[..], &rows.1[..]);
    let ys = &f.y[y * w..y * w + w];
    let q = |c: f32| (c + 0.5).clamp(0.0, 255.0) as u8;
    for ((px, &yy), &(c0, c1, wx)) in row.iter_mut().zip(ys).zip(taps) {
        let (c0, c1) = (c0 as usize, c1 as usize);
        let (ja, jb) = if wx == 1 { (1u16, 3u16) } else { (3, 1) };
        let su = (us[c0] * ja + us[c1] * jb) as usize;
        let sv = (vs[c0] * ja + vs[c1] * jb) as usize;
        let yy = yy as f32;
        let r = yy + lut.rv[sv];
        let g = yy - lut.gu[su] - lut.gv[sv];
        let b = yy + lut.bu[su];
        *px = Color32::from_rgb(q(r), q(g), q(b));
    }
}

fn fmt_time(s: f64) -> String {
    let s = s.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: usize, h: usize, y: u8, u: u8, v: u8) -> bink2_decoder::VideoFrame {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        bink2_decoder::VideoFrame { width: w, height: h, y: vec![y; w * h], u: vec![u; cw * ch], v: vec![v; cw * ch], alpha: None, keyframe: true }
    }

    #[test]
    fn converts_like_rad_full_range() {
        let px = |y, u, v| to_color_image(&flat(6, 4, y, u, v)).pixels[7];
        // chroma centre is 128.5, so 128/129 straddle grey
        let g = px(128, 128, 128);
        assert!((g.r() as i32 - 128).abs() <= 1 && (g.g() as i32 - 128).abs() <= 1 && (g.b() as i32 - 128).abs() <= 1, "{g:?}");
        assert_eq!(px(0, 128, 128).r(), 0);
        let w = px(255, 129, 129); // G = 255 - 0.344·0.5 - 0.714·0.5 = 254.47
        assert!(w.r() == 255 && w.g() == 254 && w.b() == 255, "{w:?}");
        let red = px(76, 85, 255); // full-range BT.601 red
        assert!(red.r() > 250 && red.g() < 5 && red.b() < 5, "{red:?}");
    }

    #[test]
    fn chroma_is_bilinear_between_samples() {
        // two chroma columns: left 100, right 200; output x=1 is 3/4 left, x=2 is 1/4 left
        let mut f = flat(4, 2, 128, 128, 128);
        f.u = vec![100, 200];
        let taps: Vec<_> = (0..4).map(|x| chroma_taps(x, 2)).collect();
        assert_eq!(taps, vec![(0, 0, 1), (0, 1, 3), (0, 1, 1), (1, 1, 3)]);
        let img = to_color_image(&f);
        // blue rises monotonically with U
        let b: Vec<u8> = img.pixels[..4].iter().map(|p| p.b()).collect();
        assert!(b[0] < b[1] && b[1] < b[2] && b[2] < b[3], "{b:?}");
    }

    /// The straightforward per-pixel conversion the fast path must reproduce exactly.
    fn convert_ref(f: &bink2_decoder::VideoFrame) -> Vec<Color32> {
        let (w, cw, ch) = (f.width, f.chroma_width(), f.chroma_height());
        let mut out = Vec::with_capacity(w * f.height);
        for y in 0..f.height {
            let (r0, r1, wy) = chroma_taps(y, ch);
            let (ka, kb) = if wy == 1 { (1u32, 3u32) } else { (3, 1) };
            for x in 0..w {
                let up = |plane: &[u8]| -> f32 {
                    let (c0, c1, wx) = chroma_taps(x, cw);
                    let (ja, jb) = if wx == 1 { (1u32, 3u32) } else { (3, 1) };
                    let a = plane[r0 * cw + c0] as u32 * ja + plane[r0 * cw + c1] as u32 * jb;
                    let b = plane[r1 * cw + c0] as u32 * ja + plane[r1 * cw + c1] as u32 * jb;
                    (a * ka + b * kb) as f32 / 16.0
                };
                let yy = f.y[y * w + x] as f32;
                let (u, v) = (up(&f.u) - 128.5, up(&f.v) - 128.5);
                let q = |c: f32| (c + 0.5).clamp(0.0, 255.0) as u8;
                out.push(Color32::from_rgb(q(yy + 1.402 * v), q(yy - 0.344_136 * u - 0.714_136 * v), q(yy + 1.772 * u)));
            }
        }
        out
    }

    #[test]
    fn fast_conversion_matches_reference() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 24) as u8
        };
        for (w, h) in [(37, 23), (64, 32), (1, 1), (2, 3)] {
            let mut f = flat(w, h, 0, 0, 0);
            f.y.iter_mut().for_each(|p| *p = rnd());
            f.u.iter_mut().for_each(|p| *p = rnd());
            f.v.iter_mut().for_each(|p| *p = rnd());
            assert!(to_color_image(&f).pixels == convert_ref(&f), "{w}x{h}");
        }
        // every chroma sum with every luma value
        let mut f = flat(256, 2, 0, 0, 0);
        for (x, p) in f.y.iter_mut().enumerate() {
            *p = x as u8;
        }
        for u in 0..=255u8 {
            f.u.iter_mut().for_each(|p| *p = u);
            f.v.iter_mut().for_each(|p| *p = 255 - u);
            assert!(to_color_image(&f).pixels == convert_ref(&f), "u {u}");
        }
    }

    #[test]
    fn seek_starts_from_previous_keyframe() {
        let e = |k| bink2_decoder::FrameEntry { offset: 0, size: 0, keyframe: k };
        let frames = [e(true), e(false), e(false), e(true), e(false)];
        assert_eq!(keyframe_before(&frames, 0), 0);
        assert_eq!(keyframe_before(&frames, 2), 0);
        assert_eq!(keyframe_before(&frames, 3), 3);
        assert_eq!(keyframe_before(&frames, 4), 3);
        assert_eq!(keyframe_before(&frames, 99), 3);
    }

    #[test]
    fn audio_starts_at_the_video_position() {
        assert_eq!(audio_frame_at(0, 30.0, 48000), 0);
        assert_eq!(audio_frame_at(30, 30.0, 48000), 48000);
        assert_eq!(audio_frame_at(1, 29.97, 44100), 1471);
    }

    /// Silent end-to-end check on a real clip:
    /// PERCYSTAR_BK2=<clip.bk2> cargo test --release bink_native -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bink_native_plays_pauses_and_seeks() {
        use crate::video_clip::copy_clip;
        use std::path::Path;
        use std::sync::atomic::AtomicBool;
        let Ok(src) = std::env::var("PERCYSTAR_BK2") else { return };
        let src = Path::new(&src);
        let clip = copy_clip(src, 0, src.metadata().unwrap().len(), &AtomicBool::new(false)).unwrap();
        let ctx = egui::Context::default();
        // closing right after opening (while the clip is still being read) must not wait for it
        let t = Instant::now();
        drop(BinkNative::spawn(&ctx, copy_clip(src, 0, src.metadata().unwrap().len(), &AtomicBool::new(false)).unwrap(), "test".into(), 0.0, true).unwrap());
        assert!(t.elapsed() < Duration::from_millis(100), "closing while loading took {:?}", t.elapsed());
        let mut p = BinkNative::spawn(&ctx, clip, "test".into(), 0.0, true).unwrap(); // volume 0: silent
        let wait = |p: &mut BinkNative, ms: u64| {
            let t = Instant::now();
            let mut frames = 0;
            while t.elapsed() < Duration::from_millis(ms) {
                let before = p.last_seq;
                let (exited, err) = p.poll(&ctx);
                assert!(!exited, "decoder exited: {err:?}");
                if p.last_seq != before {
                    frames += 1;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            frames
        };
        wait(&mut p, 1000);
        let got = wait(&mut p, 2000);
        let st = p.status;
        println!("{}x{} {:.2}fps frame {}/{} shown {got} frames in 2s", st.width, st.height, st.fps, st.frame, st.frames);
        assert!(p.tex.is_some() && got as f64 > st.fps * 1.5, "frames should stream at ~video fps");
        if st.has_audio {
            assert!(st.audio_playing, "audio should play along: {:?}", p.audio_note);
        }
        p.send(Cmd::TogglePause);
        wait(&mut p, 400);
        let f0 = p.status.frame;
        wait(&mut p, 600);
        assert!(p.status.paused && p.status.frame == f0, "pause should hold the frame");
        assert!(!p.status.audio_playing, "pause should stop the sound");
        let target = (st.frames as f64 / st.fps * 0.5).min(1.0);
        p.send(Cmd::Seek(target));
        wait(&mut p, 600);
        let want = (target * st.fps).round() as u32 + 1;
        assert_eq!(p.status.frame, want, "seek while paused shows the target frame");
        let seq = p.last_seq;
        p.send(Cmd::TogglePause);
        wait(&mut p, 600);
        // compare publications, not frame numbers: a short looping clip may wrap meanwhile
        assert!(!p.status.paused && p.last_seq > seq, "playback resumes after the seek");
        assert_eq!(p.status.audio_playing, st.has_audio, "sound resumes after the seek");
        // closing the panel during a long seek (rebuilding from an early keyframe) must not hang
        p.send(Cmd::Seek(st.frames as f64 / st.fps));
        std::thread::sleep(Duration::from_millis(20));
        let t = Instant::now();
        drop(p);
        println!("closed during a seek in {:?}", t.elapsed());
        assert!(t.elapsed() < Duration::from_millis(500), "closing during a seek took {:?}", t.elapsed());
    }
}
