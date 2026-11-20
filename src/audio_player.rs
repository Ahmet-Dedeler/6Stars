//! In-panel player for decoded AWC streams (16-bit PCM WAVs written by `awc.rs`).
//!
//! Playback goes through the platform `play_wav_async` (PlaySound on Windows, afplay on macOS),
//! which can't seek or report position. So we track position ourselves and, to start anywhere
//! but 0, play a temporary WAV that begins at that sample. Pause = stop + remember position.

use eframe::egui::{self, Color32, RichText};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{play_wav_async, stop_wav};

#[derive(Clone)]
struct WavInfo {
    channels: u16,
    rate: u32,
    data_off: usize,
    data_len: usize,
}

impl WavInfo {
    fn read(path: &Path) -> Option<Self> {
        let b = fs::read(path).ok()?;
        if b.len() < 44 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
            return None;
        }
        // walk chunks for fmt / data instead of assuming 44-byte headers
        let (mut off, mut channels, mut rate) = (12usize, 0u16, 0u32);
        while off + 8 <= b.len() {
            let id = &b[off..off + 4];
            let sz = u32::from_le_bytes(b[off + 4..off + 8].try_into().ok()?) as usize;
            if id == b"fmt " && off + 16 <= b.len() {
                channels = u16::from_le_bytes(b[off + 10..off + 12].try_into().ok()?);
                rate = u32::from_le_bytes(b[off + 12..off + 16].try_into().ok()?);
            } else if id == b"data" {
                let len = sz.min(b.len() - off - 8);
                return (channels > 0 && rate > 0).then_some(Self { channels, rate, data_off: off + 8, data_len: len });
            }
            off += 8 + sz + (sz & 1);
        }
        None
    }

    fn duration(&self) -> f64 {
        self.data_len as f64 / (self.rate as f64 * self.channels as f64 * 2.0)
    }
}

pub struct AudioPlayer {
    streams: Vec<(u32, PathBuf, Option<WavInfo>)>,
    sel: usize,
    /// position when not playing; when playing, position at `started`
    pos: f64,
    started: Option<Instant>,
    scrub: Option<f64>,
    tmp_seq: u64,
}

impl AudioPlayer {
    pub fn new(streams: &[(u32, PathBuf)]) -> Self {
        let streams = streams.iter().map(|(i, p)| (*i, p.clone(), WavInfo::read(p))).collect();
        Self { streams, sel: 0, pos: 0.0, started: None, scrub: None, tmp_seq: 0 }
    }

    /// Same stream list as the one this player was built for.
    pub fn matches(&self, streams: &[(u32, PathBuf)]) -> bool {
        self.streams.len() == streams.len() && self.streams.iter().zip(streams).all(|(a, b)| a.1 == b.1)
    }

    fn duration(&self) -> f64 {
        self.streams.get(self.sel).and_then(|s| s.2.as_ref()).map_or(0.0, WavInfo::duration)
    }

    fn position(&self) -> f64 {
        match self.started {
            Some(t) => (self.pos + t.elapsed().as_secs_f64()).min(self.duration()),
            None => self.pos,
        }
    }

    fn play_from(&mut self, at: f64) {
        stop_wav();
        let Some((_, path, Some(info))) = self.streams.get(self.sel).cloned() else { return };
        let at = at.clamp(0.0, info.duration());
        let file = if at < 0.05 {
            path
        } else {
            // temp WAV starting at `at`, frame-aligned
            let Ok(b) = fs::read(&path) else { return };
            let frame = info.channels as usize * 2;
            let skip = ((at * info.rate as f64) as usize * frame).min(info.data_len);
            let data = &b[info.data_off + skip..info.data_off + info.data_len];
            let mut out = Vec::with_capacity(44 + data.len());
            out.extend_from_slice(b"RIFF");
            out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
            out.extend_from_slice(b"WAVEfmt ");
            out.extend_from_slice(&16u32.to_le_bytes());
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&info.channels.to_le_bytes());
            out.extend_from_slice(&info.rate.to_le_bytes());
            out.extend_from_slice(&(info.rate * frame as u32).to_le_bytes());
            out.extend_from_slice(&(frame as u16).to_le_bytes());
            out.extend_from_slice(&16u16.to_le_bytes());
            out.extend_from_slice(b"data");
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(data);
            self.tmp_seq += 1;
            let p = std::env::temp_dir().join(format!("percystar_seek_{}_{}.wav", std::process::id(), self.tmp_seq % 2));
            if fs::write(&p, out).is_err() {
                return;
            }
            p
        };
        play_wav_async(&file);
        self.pos = at;
        self.started = Some(Instant::now());
    }

    fn pause(&mut self) {
        self.pos = self.position();
        self.started = None;
        stop_wav();
    }

    pub fn stop(&mut self) {
        self.started = None;
        self.pos = 0.0;
        stop_wav();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, status: &str) {
        let dur = self.duration();
        if self.started.is_some() {
            if self.position() >= dur {
                self.started = None;
                self.pos = 0.0;
            } else {
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
        }
        ui.label(RichText::new(status).strong().color(Color32::WHITE));
        ui.add_space(6.0);

        // transport for the selected stream
        ui.horizontal(|ui| {
            let playing = self.started.is_some();
            let space = ui.input(|i| i.key_pressed(egui::Key::Space)) && !ui.ctx().wants_keyboard_input();
            if ui.button(if playing { "⏸ Pause" } else { "▶ Play" }).clicked() || space {
                if playing { self.pause() } else { self.play_from(if self.pos >= dur { 0.0 } else { self.pos }) }
            }
            if ui.button("⏹").on_hover_text("Stop").clicked() {
                self.stop();
            }
            let mut pos = self.scrub.unwrap_or_else(|| self.position());
            ui.label(RichText::new(format!("{} / {}", fmt_time(pos), fmt_time(dur))).monospace().color(Color32::from_rgb(200, 200, 215)));
            ui.spacing_mut().slider_width = (ui.available_width() - 10.0).max(80.0);
            let r = ui.add(egui::Slider::new(&mut pos, 0.0..=dur.max(0.01)).show_value(false));
            if r.dragged() {
                self.scrub = Some(pos);
            }
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.scrub = None;
                if self.started.is_some() { self.play_from(pos) } else { self.pos = pos }
            }
        });
        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut pick = None;
            for (i, (idx, path, info)) in self.streams.iter().enumerate() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let meta = info.as_ref().map_or("unreadable".to_string(), |w| {
                    format!("{} · {} Hz · {}", fmt_time(w.duration()), w.rate, if w.channels == 1 { "mono".into() } else { format!("{} ch", w.channels) })
                });
                let playing_here = i == self.sel && self.started.is_some();
                let label = format!("{}Stream #{idx}  ·  {meta}  ·  {name}", if playing_here { "▶ " } else { "" });
                if ui.selectable_label(i == self.sel, label).clicked() {
                    pick = Some(i);
                }
            }
            if let Some(i) = pick {
                // clicking a stream plays it from the start, like the old per-stream Play button
                self.stop();
                self.sel = i;
                self.play_from(0.0);
            }
        });
    }
}

fn fmt_time(s: f64) -> String {
    let s = s.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_info_reads_duration_and_skips_extra_chunks() {
        let (rate, ch) = (48_000u32, 2u16);
        let data = vec![0u8; rate as usize * ch as usize * 2 * 3 / 2]; // 1.5 s
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&ch.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 4).to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"LIST");
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(b"abc\0"); // odd size + pad byte
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        let p = std::env::temp_dir().join("percystar_wavinfo_test.wav");
        fs::write(&p, &b).unwrap();
        let w = WavInfo::read(&p).unwrap();
        let _ = fs::remove_file(&p);
        assert_eq!((w.channels, w.rate), (2, 48_000));
        assert!((w.duration() - 1.5).abs() < 1e-9);
    }
}
