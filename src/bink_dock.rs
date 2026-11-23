// Percy6Stars by Ahmet Dedeler (https://github.com/Ahmet-Dedeler, https://ahmetdedeler.com)
// Fork of PercyStar by rpatty-dev (https://github.com/rpatty-dev/PercyStar).
// Source: https://github.com/Ahmet-Dedeler/Percy6Stars
// License: PercyStar Non-Commercial Attribution & Copyleft License v1.2 (see LICENSE)
// If you copy, vendor or port this file, keep this header and credit both projects in your README.
use eframe::egui;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use tempfile::TempDir;

#[cfg(windows)]
use std::time::{Duration, Instant};

/// Finds RAD's player. Order: `BINKPLAY_PATH`, next to the exe (or `tools/`), the working
/// directory, `PATH`, then the usual RAD Video Tools install folders (upstream fix for
/// rpatty-dev/PercyStar#15, where a missing player surfaced as "os error 3").
pub fn player_path() -> Option<PathBuf> {
    let name = if cfg!(windows) { "binkplay.exe" } else { "binkplay" };
    if let Some(p) = std::env::var_os("BINKPLAY_PATH") {
        let p = PathBuf::from(p.to_string_lossy().trim().trim_matches('"'));
        if p.is_file() {
            return Some(p);
        }
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        dirs.push(dir.clone());
        dirs.push(dir.join("tools"));
    }
    #[cfg(windows)]
    {
        // never the working directory: it may be an untrusted dump folder with a planted exe
        if let Some(path) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        for key in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA", "USERPROFILE"] {
            if let Some(base) = std::env::var_os(key) {
                let base = PathBuf::from(base);
                dirs.push(base.join("RADVideo"));
                dirs.push(base.join("RAD Video Tools"));
            }
        }
    }
    if let Some(found) = dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file()) {
        return Some(found);
    }
    #[cfg(windows)]
    return None;
    #[cfg(not(windows))]
    Some(PathBuf::from(name))
}

pub struct BinkDock {
    child: Child,
    _dir: TempDir,
    title: String,
    #[cfg(windows)]
    hwnd: Option<isize>,
    #[cfg(windows)]
    last_rect: Option<[i32; 4]>,
    #[cfg(windows)]
    started: Instant,
    #[cfg(windows)]
    audio_retry: Instant,
    #[cfg(windows)]
    has_audio: bool,
    #[cfg(windows)]
    applied_audio: Option<(f32, bool)>,
}

impl BinkDock {
    pub fn spawn(player: &Path, dir: TempDir, title: String, _volume: f32, looping: bool) -> Result<Self, String> {
        let mut cmd = Command::new(player);
        cmd.current_dir(dir.path()).arg("clip.bk2");
        #[cfg(windows)]
        let audio_track = crate::video_clip::first_audio_track(&dir.path().join("clip.bk2"));
        #[cfg(windows)]
        {
            cmd.args(["/NoBorder", "/W640", "/H360"]);
            if looping { cmd.arg("/L"); }
            if let Some(track) = audio_track.filter(|&id| id != 0) {
                cmd.arg(format!("/T{track}"));
            }
        }
        #[cfg(not(windows))]
        let _ = looping;
        let child = cmd.spawn().map_err(|err| format!("Start Bink player: {err}"))?;
        Ok(Self {
            child, _dir: dir, title,
            #[cfg(windows)]
            hwnd: None,
            #[cfg(windows)]
            last_rect: None,
            #[cfg(windows)]
            started: Instant::now(),
            #[cfg(windows)]
            audio_retry: Instant::now(),
            #[cfg(windows)]
            has_audio: audio_track.is_some(),
            #[cfg(windows)]
            applied_audio: None,
        })
    }

    /// Returns true when the preview should be dropped.
    pub fn ui(&mut self, ui: &mut egui::Ui, _frame: &eframe::Frame, _volume: &mut f32, _muted: &mut bool, looping: &mut bool) -> Result<bool, String> {
        if let Some(status) = self.child.try_wait().map_err(|err| err.to_string())? {
            return if status.success() { Ok(true) } else { Err(format!("Bink player stopped ({status})")) };
        }
        let mut stop = false;
        ui.horizontal(|ui| {
            ui.strong(&self.title);
            stop = ui.button("Stop").clicked();
            #[cfg(windows)]
            {
                ui.toggle_value(looping, "Loop").on_hover_text("Applies to the next video you open");
                ui.add_enabled_ui(self.has_audio, |ui| {
                    ui.toggle_value(_muted, "Mute");
                    ui.add(egui::Slider::new(_volume, 0.0..=1.0).show_value(false)).on_hover_text("Video volume");
                });
            }
            #[cfg(not(windows))]
            let _ = looping;
        });
        if stop { return Ok(true); }
        #[cfg(windows)]
        {
            ui.label("Click the video and press Space to pause. Drag its slider to seek.");
            ui.label(egui::RichText::new("Bink Video by RAD Game Tools").small().weak());
            ui.add_space(6.0);
            self.dock(ui, _frame)?;
            let audio = (*_volume, *_muted);
            if self.has_audio && self.applied_audio != Some(audio) && self.audio_retry <= Instant::now() {
                self.audio_retry = Instant::now() + Duration::from_millis(250);
                if win::set_volume(self.child.id(), audio) {
                    self.applied_audio = Some(audio);
                }
            }
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        Ok(false)
    }

    #[cfg(windows)]
    fn dock(&mut self, ui: &egui::Ui, frame: &eframe::Frame) -> Result<(), String> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let rect = ui.available_rect_before_wrap();
        let scale = ui.ctx().pixels_per_point();
        let bounds = [
            (rect.min.x * scale).round() as i32,
            (rect.min.y * scale).round() as i32,
            (rect.width() * scale).round().max(1.0) as i32,
            (rect.height() * scale).round().max(1.0) as i32,
        ];
        if let Some(child) = self.hwnd {
            if self.last_rect != Some(bounds) {
                win::move_window(child, bounds);
                self.last_rect = Some(bounds);
            }
        } else if let Some(child) = win::player_window(self.child.id()) {
            let Ok(RawWindowHandle::Win32(parent)) = frame.window_handle().map(|h| h.as_raw()) else {
                return Err("Could not find the Percy6Stars window to dock video".into());
            };
            if !win::dock_window(child, parent.hwnd.get()) {
                return Err("Windows could not dock the Bink player window".into());
            }
            win::move_window(child, bounds);
            self.hwnd = Some(child);
            self.last_rect = Some(bounds);
        } else if self.started.elapsed() > Duration::from_secs(10) {
            return Err("Bink player did not open a video window".into());
        } else {
            ui.ctx().request_repaint_after(Duration::from_millis(30));
        }
        Ok(())
    }
}

impl Drop for BinkDock {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(windows)]
mod win {
    use std::ptr;
    use windows::core::Interface;
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::Media::Audio::{
        eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
        ISimpleAudioVolume, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(callback: unsafe extern "system" fn(isize, isize) -> i32, param: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
        fn GetClassNameW(hwnd: isize, name: *mut u16, len: i32) -> i32;
        fn IsWindowVisible(hwnd: isize) -> i32;
        fn GetParent(hwnd: isize) -> isize;
        fn SetParent(child: isize, parent: isize) -> isize;
        fn MoveWindow(hwnd: isize, x: i32, y: i32, w: i32, h: i32, repaint: i32) -> i32;
        fn GetWindowLongW(hwnd: isize, index: i32) -> i32;
        fn SetWindowLongW(hwnd: isize, index: i32, value: i32) -> i32;
        fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, w: i32, h: i32, flags: u32) -> i32;
        fn SetForegroundWindow(hwnd: isize) -> i32;
        fn SetFocus(hwnd: isize) -> isize;
    }

    pub fn player_window(pid: u32) -> Option<isize> {
        unsafe extern "system" fn find(hwnd: isize, arg: isize) -> i32 {
            let target = &mut *(arg as *mut (u32, isize));
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == target.0 && IsWindowVisible(hwnd) != 0 {
                let mut class = [0u16; 64];
                let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
                if len > 0 && !class[..len as usize].iter().copied().eq("#32770".encode_utf16()) {
                    target.1 = hwnd;
                    return 0;
                }
            }
            1
        }
        let mut target = (pid, 0);
        unsafe { EnumWindows(find, &mut target as *mut _ as isize); }
        (target.1 != 0).then_some(target.1)
    }

    pub fn dock_window(child: isize, parent: isize) -> bool {
        const GWL_STYLE: i32 = -16;
        const WS_POPUP: i32 = i32::MIN;
        const WS_CHILD: i32 = 0x4000_0000;
        const WS_CLIPCHILDREN: i32 = 0x0200_0000;
        const WS_CAPTION: i32 = 0x00c0_0000;
        const WS_THICKFRAME: i32 = 0x0004_0000;
        const UPDATE_FRAME: u32 = 0x37; // FRAMECHANGED | NOACTIVATE | NOZORDER | NOMOVE | NOSIZE
        unsafe {
            let style = GetWindowLongW(child, GWL_STYLE);
            SetWindowLongW(child, GWL_STYLE, (style & !(WS_POPUP | WS_CAPTION | WS_THICKFRAME)) | WS_CHILD);
            SetParent(child, parent);
            if GetParent(child) != parent { return false; }
            let parent_style = GetWindowLongW(parent, GWL_STYLE);
            if parent_style & WS_CLIPCHILDREN == 0 {
                SetWindowLongW(parent, GWL_STYLE, parent_style | WS_CLIPCHILDREN);
            }
            SetWindowPos(child, 0, 0, 0, 0, 0, UPDATE_FRAME);
            // Keep keyboard focus on our window so Explorer arrow keys keep working while a
            // clip plays (upstream fix for rpatty-dev/PercyStar#11).
            SetForegroundWindow(parent);
            SetFocus(parent);
            true
        }
    }

    pub fn move_window(hwnd: isize, [x, y, w, h]: [i32; 4]) {
        unsafe { MoveWindow(hwnd, x, y, w, h, 1); }
    }

    pub fn set_volume(pid: u32, (volume, muted): (f32, bool)) -> bool {
        unsafe {
            let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if init.is_err() && init != RPC_E_CHANGED_MODE { return false; }
            let result = (|| -> windows::core::Result<bool> {
                let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)?;
                let endpoints = devices.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
                for index in 0..endpoints.GetCount()? {
                    let Ok(audio) = endpoints.Item(index)
                        .and_then(|d| d.Activate::<IAudioSessionManager2>(CLSCTX_INPROC_SERVER, None)) else { continue; };
                    let Ok(sessions) = audio.GetSessionEnumerator() else { continue; };
                    for i in 0..sessions.GetCount().unwrap_or_default() {
                        let Ok(session) = sessions.GetSession(i).and_then(|s| s.cast::<IAudioSessionControl2>()) else { continue; };
                        if session.GetProcessId() == Ok(pid) {
                            let mixer: ISimpleAudioVolume = session.cast()?;
                            mixer.SetMasterVolume(volume, ptr::null())?;
                            mixer.SetMute(muted, ptr::null())?;
                            return Ok(true);
                        }
                    }
                }
                Ok(false)
            })();
            if init.is_ok() { CoUninitialize(); }
            result.unwrap_or(false)
        }
    }
}
