mod model3d;
use model3d::{Mesh3D, decode_rage9_model_from_rpf, extract_0drpz_mesh, extract_rage9_model_name, is_vehicle_virtual, parse_obj, probe_large_rage9_model, render_mesh3d_viewport};
use eframe::egui::{self, Color32, ColorImage, CornerRadius, FontData, FontDefinitions, FontFamily, RichText, Stroke, TextureHandle, TextureOptions};
use flate2::{Decompress, FlushDecompress, Status};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[link(name = "winmm")]
extern "system" {
    fn PlaySoundW(pszSound: *const u16, hmod: isize, fdwSound: u32) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(lpEnumFunc: unsafe extern "system" fn(isize, isize) -> i32, lParam: isize) -> i32;
    fn GetWindowThreadProcessId(hWnd: isize, lpdwProcessId: *mut u32) -> u32;
    fn IsWindowVisible(hWnd: isize) -> i32;
    fn SetParent(hWndChild: isize, hWndNewParent: isize) -> isize;
    fn MoveWindow(hWnd: isize, X: i32, Y: i32, nWidth: i32, nHeight: i32, bRepaint: i32) -> i32;
    fn GetActiveWindow() -> isize;
    fn GetWindowLongW(hWnd: isize, nIndex: i32) -> i32;
    fn SetWindowLongW(hWnd: isize, nIndex: i32, dwNewLong: i32) -> i32;
}

struct EnumCtx {
    pid: u32,
    hwnd: isize,
}

unsafe extern "system" fn enum_cb(hwnd: isize, lparam: isize) -> i32 {
    let ctx = &mut *(lparam as *mut EnumCtx);
    let mut wpid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut wpid);
    if wpid == ctx.pid && IsWindowVisible(hwnd) != 0 {
        ctx.hwnd = hwnd;
        return 0;
    }
    1
}

fn find_hwnd_for_pid(pid: u32) -> Option<isize> {
    let mut ctx = EnumCtx { pid, hwnd: 0 };
    unsafe { EnumWindows(enum_cb, &mut ctx as *mut _ as isize); }
    if ctx.hwnd != 0 { Some(ctx.hwnd) } else { None }
}

fn dock_child_hwnd(child_hwnd: isize, parent_hwnd: isize, x: i32, y: i32, w: i32, h: i32) {
    const GWL_STYLE: i32 = -16;
    const WS_CHILD: i32 = 0x4000_0000;
    const WS_POPUP: i32 = -0x8000_0000_i32;
    const WS_CAPTION: i32 = 0x00C0_0000;
    const WS_THICKFRAME: i32 = 0x0004_0000;
    unsafe {
        let style = GetWindowLongW(child_hwnd, GWL_STYLE);
        SetWindowLongW(child_hwnd, GWL_STYLE, (style & !WS_POPUP & !WS_CAPTION & !WS_THICKFRAME) | WS_CHILD);
        SetParent(child_hwnd, parent_hwnd);
        MoveWindow(child_hwnd, x, y, w.max(64), h.max(64), 1);
    }
}

fn play_wav_async(path: &std::path::Path) {
    let mut wide: Vec<u16> = path.to_string_lossy().encode_utf16().collect();
    wide.push(0);
    unsafe { PlaySoundW(wide.as_ptr(), 0, 0x00020000 | 0x0001); }
}

fn stop_wav() {
    unsafe { PlaySoundW(std::ptr::null(), 0, 0); }
}

#[derive(Clone, PartialEq, Eq)]
enum Category {
    All,
    Vehicles,
    Models,
    Videos,
    Audio,
    Xml,
    Resources,
    Binary,
}

#[derive(Clone, Debug)]
struct RpfEntry {
    source_rpf: PathBuf,
    rpf_name: String,
    name: String,
    kind: &'static str,
    offset: u64,
    comp_size: u64,
    raw_size: u64,
    fps: f32,
    tracks: u32,
    chunks: Vec<(u64, u64)>,
}

#[derive(Default)]
struct ScanState {
    scanning: bool,
    header_info: String,
    status: String,
    entries: Vec<RpfEntry>,
    rpf_names: Vec<String>,
    file_sig: HashMap<PathBuf, (u64, Option<SystemTime>)>,
}

#[derive(Default)]
struct MediaState {
    loading: bool,
    status: String,
    frames: Vec<ColorImage>,
    fps: f32,
    wav_path: Option<PathBuf>,
    awc_streams: Vec<(u32, PathBuf)>,
    bink_child_pid: Option<u32>,
    bink_hwnd: Option<isize>,
    bink_clip: Option<PathBuf>,
    loaded_mesh: Option<Mesh3D>,
}

struct Rpf9App {
    target_path: Option<PathBuf>,
    is_folder: bool,
    auto_update: bool,
    selected_rpf: String,
    state: Arc<Mutex<ScanState>>,
    media: Arc<Mutex<MediaState>>,
    category: Category,
    selected: Option<usize>,
    preview: String,
    filter: String,
    logo_tex: Option<TextureHandle>,
    textures: Vec<TextureHandle>,
    playing: bool,
    frame_idx: usize,
    last_frame_time: Instant,
    watch_gen: Arc<Mutex<u64>>,
    bink_proc: Option<Child>,
    parent_hwnd: isize,
    last_dock_rect: [i32; 4],
    clip_seq: u64,
    mesh3d: Option<Mesh3D>,
    show_3d: bool,
}

impl Rpf9App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_style(&cc.egui_ctx);
        let logo_tex = image::load_from_memory(include_bytes!("../assets/logo.png")).ok().map(|img| {
            let rgba = img.to_rgba8();
            let ci = ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
            cc.egui_ctx.load_texture("percystar_logo", ci, TextureOptions::LINEAR)
        });
        let mut app = Self {
            target_path: None,
            is_folder: false,
            auto_update: true,
            selected_rpf: "All RPFs".into(),
            state: Arc::new(Mutex::new(ScanState {
                header_info: "No archive or folder loaded".into(),
                status: "Open an .rpf or select the GTA6 folder".into(),
                ..Default::default()
            })),
            media: Arc::new(Mutex::new(MediaState::default())),
            category: Category::All,
            selected: None,
            preview: String::new(),
            filter: String::new(),
            logo_tex,
            textures: Vec::new(),
            playing: false,
            frame_idx: 0,
            last_frame_time: Instant::now(),
            watch_gen: Arc::new(Mutex::new(0)),
            bink_proc: None,
            parent_hwnd: 0,
            last_dock_rect: [0; 4],
            clip_seq: 0,
            mesh3d: None,
            show_3d: false,
        };
        if let Some(last) = load_last_folder() {
            if last.is_dir() {
                app.start_folder_scan(last);
            }
        }
        app
    }

    fn stop_bink_player(&mut self) {
        if let Some(mut c) = self.bink_proc.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let mut m = self.media.lock().unwrap();
        m.bink_child_pid = None;
        m.bink_hwnd = None;
        m.bink_clip = None;
    }

    fn start_single_scan(&mut self, path: PathBuf) {
        self.target_path = Some(path.clone());
        self.is_folder = false;
        self.selected_rpf = "All RPFs".into();
        self.selected = None;
        self.preview.clear();
        self.textures.clear();
        self.playing = false;
        stop_wav();
        let gen = {
            let mut g = self.watch_gen.lock().unwrap();
            *g += 1;
            *g
        };
        {
            let mut st = self.state.lock().unwrap();
            st.scanning = true;
            st.entries.clear();
            st.rpf_names.clear();
            st.file_sig.clear();
            st.status = "Scanning RPF9 archive...".into();
        }
        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            match scan_single_rpf(&path, Some(&state)) {
                Ok((hdr, mut entries)) => {
                    let mut st = state.lock().unwrap();
                    st.header_info = hdr;
                    st.rpf_names = vec![path.file_name().unwrap_or_default().to_string_lossy().into_owned()];
                    let n = entries.len();
                    st.entries.append(&mut entries);
                    st.scanning = false;
                    st.status = format!("{n} entries ready");
                }
                Err(e) => {
                    let mut st = state.lock().unwrap();
                    st.scanning = false;
                    st.status = format!("Error: {e}");
                }
            }
            let _ = gen;
        });
    }

    fn start_folder_scan(&mut self, folder: PathBuf) {
        save_last_folder(&folder);
        self.target_path = Some(folder.clone());
        self.is_folder = true;
        self.selected_rpf = "All RPFs".into();
        self.selected = None;
        self.preview.clear();
        self.textures.clear();
        self.playing = false;
        stop_wav();
        let gen = {
            let mut g = self.watch_gen.lock().unwrap();
            *g += 1;
            *g
        };
        {
            let mut st = self.state.lock().unwrap();
            st.entries.clear();
            st.rpf_names.clear();
            st.file_sig.clear();
            st.scanning = true;
            st.header_info = format!("Folder: {}", folder.display());
            st.status = "Scanning folder for .rpf archives...".into();
        }

        let state = Arc::clone(&self.state);
        let watch_gen = Arc::clone(&self.watch_gen);
        std::thread::spawn(move || {
            let mut disk_cache = load_disk_cache();
            loop {
                if *watch_gen.lock().unwrap() != gen {
                    break;
                }
                let mut rpfs = Vec::new();
                collect_rpfs(&folder, &mut rpfs);
                // Smallest RPFs first so XMLs/scripts/minigame audio appear in <2s
                rpfs.sort_by_key(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(u64::MAX));

                let mut changed = Vec::new();
                let mut cache_hits = 0usize;
                {
                    let mut st = state.lock().unwrap();
                    for p in &rpfs {
                        if let Ok(meta) = fs::metadata(p) {
                            let mtime_s = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
                            let sig = (meta.len(), meta.modified().ok());
                            if st.file_sig.get(p) != Some(&sig) {
                                let key = format!("{}|{}|{}", p.display(), meta.len(), mtime_s);
                                if let Some(cached_entries) = disk_cache.get(&key) {
                                    st.entries.retain(|e| &e.source_rpf != p);
                                    st.entries.extend(cached_entries.iter().cloned());
                                    st.file_sig.insert(p.clone(), sig);
                                    let rname = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                                    if !st.rpf_names.contains(&rname) {
                                        st.rpf_names.push(rname);
                                    }
                                    cache_hits += 1;
                                } else {
                                    changed.push((p.clone(), sig, key));
                                }
                            }
                        }
                    }
                    if cache_hits > 0 {
                        st.rpf_names.sort();
                        st.status = format!("Loaded {cache_hits} cached RPFs ({} entries)...", st.entries.len());
                    }
                }

                if !changed.is_empty() {
                    state.lock().unwrap().scanning = true;
                    let total = changed.len();
                    let next_idx = Arc::new(Mutex::new(0usize));
                    let cache_arc = Arc::new(Mutex::new(disk_cache));
                    std::thread::scope(|s| {
                        for _ in 0..4 {
                            let state = Arc::clone(&state);
                            let watch_gen = Arc::clone(&watch_gen);
                            let next_idx = Arc::clone(&next_idx);
                            let cache_arc = Arc::clone(&cache_arc);
                            let changed = &changed;
                            s.spawn(move || loop {
                                if *watch_gen.lock().unwrap() != gen {
                                    return;
                                }
                                let idx = {
                                    let mut g = next_idx.lock().unwrap();
                                    let i = *g;
                                    *g += 1;
                                    i
                                };
                                if idx >= total {
                                    return;
                                }
                                let (p, sig, key) = &changed[idx];
                                let rname = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                                state.lock().unwrap().status = format!("Scanning [{}/{}] {}...", idx + 1, total, rname);
                                if let Ok((_, new_entries)) = scan_single_rpf(p, None) {
                                    {
                                        let mut st = state.lock().unwrap();
                                        st.entries.retain(|e| &e.source_rpf != p);
                                        st.entries.extend(new_entries.iter().cloned());
                                        st.file_sig.insert(p.clone(), *sig);
                                        if !st.rpf_names.contains(&rname) {
                                            st.rpf_names.push(rname);
                                            st.rpf_names.sort();
                                        }
                                    }
                                    // Save cache incrementally so completed RPFs are never re-scanned
                                    let mut c = cache_arc.lock().unwrap();
                                    c.insert(key.clone(), new_entries);
                                    save_disk_cache(&c);
                                }
                            });
                        }
                    });
                    disk_cache = Arc::try_unwrap(cache_arc).unwrap().into_inner().unwrap();
                    let mut st = state.lock().unwrap();
                    st.scanning = false;
                    st.status = format!("{} RPFs · {} entries (cached + auto-updating)", st.rpf_names.len(), st.entries.len());
                } else {
                    let mut st = state.lock().unwrap();
                    st.scanning = false;
                    if cache_hits > 0 {
                        st.status = format!("{} RPFs · {} entries (instant cache)", st.rpf_names.len(), st.entries.len());
                    }
                }

                for _ in 0..20 {
                    if *watch_gen.lock().unwrap() != gen {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        });
    }

    fn select_entry(&mut self, idx: usize) {
        let Some(e) = self.state.lock().unwrap().entries.get(idx).cloned() else { return };
        self.selected = Some(idx);
        self.preview.clear();
        self.textures.clear();
        self.playing = false;
        self.frame_idx = 0;
        self.last_dock_rect = [0; 4];
        self.mesh3d = None;
        self.show_3d = false;
        stop_wav();
        self.stop_bink_player();

        {
            let mut m = self.media.lock().unwrap();
            m.frames.clear();
            m.awc_streams.clear();
            m.wav_path = None;
            m.status.clear();
            m.loading = false;
            m.loaded_mesh = None;
        }

        if e.kind == "MODEL" || e.kind == "VEH" {
            let media = Arc::clone(&self.media);
            {
                let mut m = media.lock().unwrap();
                m.loading = true;
                m.status = format!("Decoding RAGE9 meshlets: {}...", e.name);
            }
            let src = e.source_rpf.clone();
            let off = e.offset;
            let nm = e.name.clone();
            std::thread::spawn(move || {
                match decode_rage9_model_from_rpf(&src, off, &nm) {
                    Ok(mesh) => {
                        let mut m = media.lock().unwrap();
                        m.loading = false;
                        m.status.clear();
                        m.loaded_mesh = Some(mesh);
                    }
                    Err(err) => {
                        let mut m = media.lock().unwrap();
                        m.loading = false;
                        m.status = format!("Model decode error ({nm}): {err}");
                    }
                }
            });
            return;
        }

        if e.kind == "BK2" {
            let tmp = std::env::temp_dir().join("rpf9_preview_bk2");
            let _ = fs::create_dir_all(&tmp);
            // Best-effort cleanup of old clip files; unique name avoids OS error 32 lock
            if let Ok(rd) = fs::read_dir(&tmp) {
                for old in rd.flatten() {
                    let _ = fs::remove_file(old.path());
                }
            }
            self.clip_seq += 1;
            let bk2_p = tmp.join(format!("clip_{}.bk2", self.clip_seq));
            match read_entry_bytes(&e) {
                Ok(bytes) => {
                    if let Err(err) = fs::write(&bk2_p, &bytes) {
                        self.media.lock().unwrap().status = format!("Write error: {err}");
                        return;
                    }
                }
                Err(err) => {
                    self.media.lock().unwrap().status = format!("Read error: {err}");
                    return;
                }
            }
            let binkplay = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("binkplay.exe")))
                .filter(|p| p.exists())
                .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)\RADVideo\binkplay.exe"));
            match Command::new(binkplay)
                .args([bk2_p.to_str().unwrap(), "/NoBorder", "/DrawD3D11", "/L", "/W640", "/H360"])
                .spawn()
            {
                Ok(child) => {
                    let pid = child.id();
                    self.bink_proc = Some(child);
                    let mut m = self.media.lock().unwrap();
                    m.bink_child_pid = Some(pid);
                    m.bink_clip = Some(bk2_p);
                    m.status = format!("Playing {} (D3D11 BinkPlay)", e.name);
                }
                Err(err) => {
                    self.media.lock().unwrap().status = format!("binkplay.exe error: {err}");
                }
            }
        } else if e.kind == "AWC" {
            let media = Arc::clone(&self.media);
            {
                let mut m = media.lock().unwrap();
                m.loading = true;
                m.status = format!("Decoding {} Opus streams...", e.name);
            }
            std::thread::spawn(move || decode_awc_for_player(&e, &media));
        } else {
            match read_entry_bytes(&e) {
                Ok(data) => {
                    let sample = &data[..data.len().min(200)];
                    let is_text = e.kind == "XML"
                        || (!sample.is_empty()
                            && sample.iter().all(|&b| (32..127).contains(&b) || b == 9 || b == 10 || b == 13 || b >= 0x80));
                    if e.kind == "PSIN" {
                        self.preview = parse_psin(&data);
                    } else if is_text {
                        self.preview = String::from_utf8_lossy(&data[..data.len().min(250_000)]).into_owned();
                    } else {
                        if data.len() >= 24 && &data[16..21] == b"0DRPz" {
                            if let Some(m) = extract_0drpz_mesh(&e.name, &data) {
                                self.mesh3d = Some(m);
                            }
                        }
                        self.preview = format_hex(&data[..data.len().min(2048)]);
                    }
                }
                Err(err) => self.preview = format!("Read error: {err}"),
            }
        }
    }

    fn export_entries(&self, indices: &[usize]) {
        let Some(out_dir) = rfd::FileDialog::new().set_directory(r"F:\Ext6").pick_folder() else { return };
        let entries: Vec<RpfEntry> = {
            let st = self.state.lock().unwrap();
            indices.iter().filter_map(|&i| st.entries.get(i).cloned()).collect()
        };
        let mut ok = 0usize;
        for e in entries {
            if e.kind == "MODEL" || e.kind == "VEH" {
                if let Ok(mesh) = decode_rage9_model_from_rpf(&e.source_rpf, e.offset, &e.name) {
                    let safe = e.name.replace('#', "");
                    let fname = format!("{safe}.obj");
                    if mesh.export_obj(&out_dir.join(fname)).is_ok() {
                        ok += 1;
                    }
                }
            } else if let Ok(data) = read_entry_bytes(&e) {
                let fname = if self.is_folder { format!("{}_{}", e.rpf_name.trim_end_matches(".rpf"), e.name) } else { e.name.clone() };
                if fs::write(out_dir.join(fname), &data).is_ok() {
                    ok += 1;
                }
            }
        }
        self.state.lock().unwrap().status = format!("Exported {ok} file(s) to {}", out_dir.display());
    }
}

fn cache_file_path() -> Option<PathBuf> {
    std::env::var("LOCALAPPDATA").ok().map(|d| PathBuf::from(d).join("PercyStar").join("scan_cache_v2.tsv"))
}

fn load_last_folder() -> Option<PathBuf> {
    let p = std::env::var("LOCALAPPDATA").ok().map(|d| PathBuf::from(d).join("PercyStar").join("last_folder.txt"))?;
    let s = fs::read_to_string(p).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() { None } else { Some(PathBuf::from(trimmed)) }
}

fn save_last_folder(folder: &PathBuf) {
    if let Ok(d) = std::env::var("LOCALAPPDATA") {
        let dir = PathBuf::from(d).join("PercyStar");
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(dir.join("last_folder.txt"), folder.display().to_string());
    }
}

fn load_disk_cache() -> HashMap<String, Vec<RpfEntry>> {
    let mut map: HashMap<String, Vec<RpfEntry>> = HashMap::new();
    let Some(p) = cache_file_path() else { return map };
    let Ok(f) = File::open(p) else { return map };
    for line in BufReader::new(f).lines().flatten() {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 10 { continue; }
        let key = cols[0].to_string();
        let src = PathBuf::from(cols[1]);
        let rname = cols[2].to_string();
        let name = cols[3].to_string();
        let kind: &'static str = match cols[4] {
            "VEH" => "VEH",
            "MODEL" => "MODEL",
            "BK2" => "BK2",
            "AWC" => "AWC",
            "XML" => "XML",
            "PSIN" => "PSIN",
            _ => "ZLIB",
        };
        let offset = cols[5].parse().unwrap_or(0);
        let comp_size = cols[6].parse().unwrap_or(0);
        let raw_size = cols[7].parse().unwrap_or(0);
        let fps = cols[8].parse().unwrap_or(0.0);
        let tracks = cols[9].parse().unwrap_or(0);
        let chunks = if cols.len() > 10 && !cols[10].is_empty() {
            cols[10].split(';').filter_map(|pair| {
                let mut it = pair.split(',');
                Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
            }).collect()
        } else {
            Vec::new()
        };
        map.entry(key).or_default().push(RpfEntry {
            source_rpf: src, rpf_name: rname, name, kind, offset, comp_size, raw_size, fps, tracks, chunks,
        });
    }
    map
}

fn save_disk_cache(map: &HashMap<String, Vec<RpfEntry>>) {
    let Some(p) = cache_file_path() else { return };
    if let Some(parent) = p.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(f) = File::create(p) else { return };
    let mut w = BufWriter::new(f);
    for (key, entries) in map {
        for e in entries {
            let chunks_s: String = e.chunks.iter().map(|(a, b)| format!("{a},{b}")).collect::<Vec<_>>().join(";");
            let _ = writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                key, e.source_rpf.display(), e.rpf_name, e.name, e.kind,
                e.offset, e.comp_size, e.raw_size, e.fps, e.tracks, chunks_s
            );
        }
    }
}

fn collect_rpfs(dir: &PathBuf, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_rpfs(&p, out);
            } else if p.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("rpf")).unwrap_or(false) {
                out.push(p);
            }
        }
    }
}

fn decode_bk2_for_player(e: &RpfEntry, media: &Arc<Mutex<MediaState>>) {
    let tmp = std::env::temp_dir().join("rpf9_preview_bk2");
    let _ = fs::remove_dir_all(&tmp);
    let _ = fs::create_dir_all(&tmp);

    let bk2_p = tmp.join("clip.bk2");
    match read_entry_bytes(e) {
        Ok(bytes) => {
            if let Err(err) = fs::write(&bk2_p, &bytes) {
                let mut m = media.lock().unwrap();
                m.loading = false;
                m.status = format!("Write error: {err}");
                return;
            }
        }
        Err(err) => {
            let mut m = media.lock().unwrap();
            m.loading = false;
            m.status = format!("Read error: {err}");
            return;
        }
    }

    let rad = r"C:\Program Files (x86)\RADVideo\radvideo64.exe";
    let frame_out = tmp.join("frame.bmp");
    let _ = Command::new(rad)
        .args(["BinkConv", bk2_p.to_str().unwrap(), frame_out.to_str().unwrap(), "/O", "/#"])
        .status();

    let wav_p = tmp.join("audio.wav");
    if e.tracks > 0 {
        let _ = Command::new(rad)
            .args(["BinkConv", bk2_p.to_str().unwrap(), wav_p.to_str().unwrap(), "/NoVideo", "/O", "/#"])
            .status();
    }

    let mut bmps: Vec<PathBuf> = fs::read_dir(&tmp)
        .into_iter()
        .flatten()
        .flatten()
        .map(|d| d.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("bmp"))
        .collect();
    bmps.sort();

    let slice = &bmps[..bmps.len().min(450)];
    let mut frames_opt: Vec<Option<ColorImage>> = vec![None; slice.len()];
    let chunks = (slice.len() + 3) / 4;
    if chunks > 0 {
        std::thread::scope(|s| {
            for (t_idx, out_chunk) in frames_opt.chunks_mut(chunks).enumerate() {
                let start = t_idx * chunks;
                let paths = &slice[start..start + out_chunk.len()];
                s.spawn(move || {
                    for (j, p) in paths.iter().enumerate() {
                        if let Ok(img) = image::open(p) {
                            let rgba = img.to_rgba8();
                            let size = [rgba.width() as usize, rgba.height() as usize];
                            out_chunk[j] = Some(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()));
                        }
                    }
                });
            }
        });
    }
    let frames: Vec<ColorImage> = frames_opt.into_iter().flatten().collect();

    let mut m = media.lock().unwrap();
    m.loading = false;
    let total_bmps = bmps.len();
    m.frames = frames;
    if wav_p.exists() && fs::metadata(&wav_p).map(|x| x.len() > 44).unwrap_or(false) {
        m.wav_path = Some(wav_p);
    }
    m.status = if m.frames.is_empty() {
        "Failed to decode .bk2 (check RAD Video Tools at C:\\Program Files (x86)\\RADVideo)".into()
    } else if total_bmps > m.frames.len() {
        format!("Loaded {}/{} frames @ {:.1} fps", m.frames.len(), total_bmps, m.fps)
    } else {
        format!("Loaded {} frames @ {:.1} fps", m.frames.len(), m.fps)
    };
}

fn decode_awc_for_player(e: &RpfEntry, media: &Arc<Mutex<MediaState>>) {
    let tmp = std::env::temp_dir().join("rpf9_preview_awc");
    let _ = fs::remove_dir_all(&tmp);
    let _ = fs::create_dir_all(&tmp);
    let awc_p = tmp.join("sound.awc");
    if let Ok(bytes) = read_entry_bytes(e) {
        let _ = fs::write(&awc_p, &bytes);
    }
    let awc_py = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("awc_extract.py")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(r"F:\Ext6\awc_extract.py"));
    let _ = Command::new("python")
        .args([awc_py.to_str().unwrap(), tmp.to_str().unwrap(), tmp.to_str().unwrap()])
        .status();

    let mut wavs: Vec<(u32, PathBuf)> = fs::read_dir(&tmp)
        .into_iter()
        .flatten()
        .flatten()
        .map(|d| d.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("wav"))
        .enumerate()
        .map(|(i, p)| (i as u32 + 1, p))
        .collect();
    wavs.sort_by(|a, b| a.1.cmp(&b.1));

    let mut m = media.lock().unwrap();
    m.loading = false;
    m.status = format!("Decoded {} playable .wav stream(s)", wavs.len());
    m.awc_streams = wavs;
}

fn setup_style(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let base = PathBuf::from(local).join(r"Microsoft\Windows\Fonts");
        for name in ["SF-Pro-Display-Regular.otf", "SF-Pro-Display-Medium.otf", "SF-Pro-Display-Semibold.otf"] {
            if let Ok(bytes) = fs::read(base.join(name)) {
                fonts.font_data.insert("sf_pro".into(), Arc::new(FontData::from_owned(bytes)));
                fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "sf_pro".into());
                break;
            }
        }
    }
    ctx.set_fonts(fonts);

    let mut vis = egui::Visuals::dark();
    vis.panel_fill = Color32::from_rgb(16, 17, 22);
    vis.window_fill = Color32::from_rgb(22, 23, 30);
    vis.extreme_bg_color = Color32::from_rgb(11, 12, 16);
    vis.widgets.inactive.bg_fill = Color32::from_rgb(28, 30, 38);
    vis.widgets.inactive.corner_radius = CornerRadius::same(6);
    vis.widgets.hovered.bg_fill = Color32::from_rgb(44, 46, 58);
    vis.widgets.hovered.corner_radius = CornerRadius::same(6);
    vis.widgets.active.bg_fill = Color32::from_rgb(252, 175, 23);
    vis.widgets.active.fg_stroke = Stroke::new(1.5_f32, Color32::BLACK);
    vis.widgets.active.corner_radius = CornerRadius::same(6);
    vis.selection.bg_fill = Color32::from_rgba_unmultiplied(252, 175, 23, 55);
    vis.selection.stroke = Stroke::new(1.0_f32, Color32::from_rgb(252, 175, 23));
    ctx.set_visuals(vis);
}

fn draw_percystar_spinner(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let t = ui.input(|i| i.time) as f32;
    let p = ui.painter();
    let c = rect.center();
    p.circle_stroke(c, size * 0.44, Stroke::new(2.0_f32, Color32::from_rgb(255, 85, 175)));
    let mut pts = Vec::with_capacity(10);
    for i in 0..10 {
        let ang = t * 3.5 + (i as f32) * std::f32::consts::PI / 5.0 - std::f32::consts::FRAC_PI_2;
        let r = if i % 2 == 0 { size * 0.34 } else { size * 0.14 };
        pts.push(egui::pos2(c.x + r * ang.cos(), c.y + r * ang.sin()));
    }
    p.add(egui::Shape::convex_polygon(pts, Color32::from_rgb(252, 175, 23), Stroke::NONE));
    ui.ctx().request_repaint();
}

fn kind_color(kind: &str) -> Color32 {
    match kind {
        "VEH" => Color32::from_rgb(255, 85, 175),
        "MODEL" => Color32::from_rgb(130, 175, 255),
        "XML" => Color32::from_rgb(72, 210, 255),
        "AWC" => Color32::from_rgb(90, 235, 165),
        "BK2" => Color32::from_rgb(252, 175, 23),
        "PSIN" => Color32::from_rgb(210, 120, 255),
        _ => Color32::from_rgb(145, 152, 168),
    }
}

fn parse_psin(data: &[u8]) -> String {
    let mut out = String::new();
    if data.len() < 16 || &data[..4] != b"PSIN" {
        return format_hex(&data[..data.len().min(2048)]);
    }
    let data_sz = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let root_hash = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let flags = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    out.push_str("=== RAGE PSIN (PSO Serialized Metadata) ===\n");
    out.push_str(&format!("Total Size: {} B | PSIN Block: {} B | RootHash: 0x{:08x} | Flags: 0x{:08x}\n\n", data.len(), data_sz, root_hash, flags));

    out.push_str("--- PSO Sections ---\n");
    for tag in [b"LSCH", b"LENM", b"LSNS", b"LGID", b"LRSC", b"CHKS", b"PSCH", b"PMAP", b"STRF", b"STRS"] {
        let mut pos = 0usize;
        while let Some(rel) = data[pos..].windows(4).position(|w| w == tag) {
            let off = pos + rel;
            let sz = if off + 8 <= data.len() {
                u32::from_be_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]])
            } else {
                0
            };
            out.push_str(&format!("  {} @ 0x{:06x} (be_len={})\n", std::str::from_utf8(tag).unwrap_or("?"), off, sz));
            pos = off + 4;
        }
    }

    out.push_str("\n--- Readable Strings ---\n");
    let mut cur = Vec::new();
    let mut strs = Vec::new();
    for &b in data {
        if (32..127).contains(&b) {
            cur.push(b);
        } else {
            if cur.len() >= 4 && !cur.iter().all(|&c| c == b'p' || c == b'a') {
                strs.push(String::from_utf8_lossy(&cur).into_owned());
            }
            cur.clear();
        }
    }
    if cur.len() >= 4 && !cur.iter().all(|&c| c == b'p' || c == b'a') {
        strs.push(String::from_utf8_lossy(&cur).into_owned());
    }
    for s in strs.iter().take(120) {
        out.push_str(&format!("  {s}\n"));
    }

    out.push_str("\n--- Header Hex (first 512 B) ---\n");
    out.push_str(&format_hex(&data[..data.len().min(512)]));
    out
}

fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1_073_741_824.0 {
        format!("{:.2} GB", b / 1_073_741_824.0)
    } else if b >= 1_048_576.0 {
        format!("{:.1} MB", b / 1_048_576.0)
    } else if b >= 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn format_hex(data: &[u8]) -> String {
    let mut out = String::new();
    for (row, chunk) in data.chunks(16).enumerate() {
        out.push_str(&format!("{:04x}: ", row * 16));
        for b in chunk {
            out.push_str(&format!("{b:02x} "));
        }
        for _ in chunk.len()..16 {
            out.push_str("   ");
        }
        out.push(' ');
        for &b in chunk {
            out.push(if (32..127).contains(&b) { b as char } else { '.' });
        }
        out.push('\n');
    }
    out
}

fn read_entry_bytes(e: &RpfEntry) -> std::io::Result<Vec<u8>> {
    use flate2::read::ZlibDecoder;
    let mut f = File::open(&e.source_rpf)?;
    if !e.chunks.is_empty() {
        let mut combined = Vec::new();
        for &(coff, clen) in &e.chunks {
            f.seek(SeekFrom::Start(coff))?;
            let mut raw = vec![0u8; clen as usize];
            f.read_exact(&mut raw)?;
            let mut dec = ZlibDecoder::new(&raw[..]);
            dec.read_to_end(&mut combined)?;
        }
        Ok(combined)
    } else {
        f.seek(SeekFrom::Start(e.offset))?;
        let mut raw = vec![0u8; e.comp_size as usize];
        f.read_exact(&mut raw)?;
        if matches!(e.kind, "XML" | "PSIN" | "ZLIB") {
            let mut dec = ZlibDecoder::new(&raw[..]);
            let mut out = Vec::new();
            dec.read_to_end(&mut out)?;
            Ok(out)
        } else {
            Ok(raw)
        }
    }
}

fn extract_xml_tag(bytes: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(&bytes[..bytes.len().min(300)]).ok()?;
    for part in s.split('<').skip(1) {
        let tag = part.split(|c: char| c.is_whitespace() || c == '>').next()?;
        if !tag.starts_with('?') && !tag.starts_with('!') && !tag.starts_with('/') && tag.len() >= 3 && tag != "Item" {
            return Some(tag.chars().filter(|c| c.is_alphanumeric() || *c == '_').take(36).collect());
        }
    }
    None
}

fn scan_single_rpf(path: &PathBuf, progress_state: Option<&Arc<Mutex<ScanState>>>) -> std::io::Result<(String, Vec<RpfEntry>)> {
    let mut f = File::open(path)?;
    let sz = f.metadata()?.len();
    let rname = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let mut hdr = [0u8; 64];
    let n = f.read(&mut hdr)?;
    let magic = String::from_utf8_lossy(&hdr[..n.min(4)]).into_owned();
    let hdr_hex: String = hdr[4..n.min(16)].iter().map(|b| format!("{b:02x}")).collect();
    let hdr_info = format!("{magic}  ·  {sz} B  ·  {hdr_hex}");
    if let Some(st) = progress_state {
        st.lock().unwrap().header_info = hdr_info.clone();
    }

    let mut found = Vec::new();
    let mut off = 0u64;
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    let mut probe_dec = Decompress::new(true);
    let mut probe_out = vec![0u8; 32768];
    let mut bk2_idx = 0usize;
    let mut awc_idx = 0usize;
    let mut last_ui_mb = 0u64;
    let mut need_seek = true;
    while off < sz {
        if let Some(st) = progress_state {
            let cur_mb = off / (64 * 1024 * 1024);
            if cur_mb != last_ui_mb {
                last_ui_mb = cur_mb;
                st.lock().unwrap().status = format!("Scanning {:.2} / {:.2} GB ({} found)...", off as f64 / 1e9, sz as f64 / 1e9, found.len());
            }
        }
        if need_seek {
            f.seek(SeekFrom::Start(off))?;
            need_seek = false;
        }
        let read_n = f.read(&mut buf)?;
        if read_n == 0 { break; }
        let mut pos = 0usize;
        let mut jumped = false;
        while pos + 44 <= read_n {
            // Fast 16-byte check for zero-prefixed RAGE9 resource at +0x10
            let h0 = u64::from_le_bytes([buf[pos], buf[pos+1], buf[pos+2], buf[pos+3], buf[pos+4], buf[pos+5], buf[pos+6], buf[pos+7]]);
            let h1 = u64::from_le_bytes([buf[pos+8], buf[pos+9], buf[pos+10], buf[pos+11], buf[pos+12], buf[pos+13], buf[pos+14], buf[pos+15]]);
            if h0 == 0 && h1 == 0 {
                if buf[pos + 0x10] == 0x78 && matches!(buf[pos + 0x11], 0x9c | 0xda) && (buf[pos + 0x12] & 0x06) != 0x06 {
                    let p = pos + 0x10;
                    let end = (p + 32768).min(read_n);
                    probe_dec.reset(true);
                    let _ = probe_dec.decompress(&buf[p..end], &mut probe_out, FlushDecompress::None);
                    let olen = probe_dec.total_out() as usize;
                    if olen >= 0x80 {
                        let vft = u64::from_le_bytes([probe_out[0], probe_out[1], probe_out[2], probe_out[3], probe_out[4], probe_out[5], probe_out[6], probe_out[7]]);
                        let bmap = u64::from_le_bytes([probe_out[8], probe_out[9], probe_out[10], probe_out[11], probe_out[12], probe_out[13], probe_out[14], probe_out[15]]);
                        if vft == 0 && (0x5000_0000..0x5800_0000).contains(&bmap) && &probe_out[0x10..0x15] != b"0DRPz" {
                            let model_hit = if let Some((mname, _ext)) = extract_rage9_model_name(&probe_out[..olen]) {
                                let is_veh = is_vehicle_virtual(&probe_out[..olen], &mname);
                                Some((mname, is_veh, olen))
                            } else if olen >= 32768 && (bmap - 0x5000_0000) >= 32768 {
                                // Large multi-chunk virtual resource (e.g. full vehicle .#ft): read full virtual segment
                                probe_large_rage9_model(path, off + p as u64)
                            } else {
                                None
                            };
                            if let Some((mname, is_veh, vlen)) = model_hit {
                                let kind = if is_veh { "VEH" } else { "MODEL" };
                                found.push(RpfEntry {
                                    source_rpf: path.clone(),
                                    rpf_name: rname.clone(),
                                    name: mname,
                                    kind,
                                    offset: off + p as u64,
                                    comp_size: probe_dec.total_in(),
                                    raw_size: vlen as u64,
                                    fps: 0.0,
                                    tracks: 0,
                                    chunks: Vec::new(),
                                });
                            }
                        }
                    }
                }
                pos += 0x1000;
                continue;
            }
            let tag = &buf[pos..pos + 4];
            if tag == b"ADAT" {
                awc_idx += 1;
                let sc = u32::from_le_bytes([buf[pos + 8], buf[pos + 9], buf[pos + 10], buf[pos + 11]]);
                found.push(RpfEntry {
                    source_rpf: path.clone(),
                    rpf_name: rname.clone(),
                    name: format!("sound_{awc_idx:04}_{sc}st.awc"),
                    kind: "AWC",
                    offset: off + pos as u64,
                    comp_size: 0,
                    raw_size: 0,
                    fps: 0.0,
                    tracks: sc,
                    chunks: Vec::new(),
                });
            } else if &tag[..3] == b"KB2" && (b'a'..=b'z').contains(&tag[3]) {
                let fsize = u32::from_le_bytes([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]]) as u64;
                let frames = u32::from_le_bytes([buf[pos + 8], buf[pos + 9], buf[pos + 10], buf[pos + 11]]);
                let frames2 = u32::from_le_bytes([buf[pos + 16], buf[pos + 17], buf[pos + 18], buf[pos + 19]]);
                let w = u32::from_le_bytes([buf[pos + 20], buf[pos + 21], buf[pos + 22], buf[pos + 23]]);
                let h = u32::from_le_bytes([buf[pos + 24], buf[pos + 25], buf[pos + 26], buf[pos + 27]]);
                let fn_ = u32::from_le_bytes([buf[pos + 28], buf[pos + 29], buf[pos + 30], buf[pos + 31]]);
                let fd_ = u32::from_le_bytes([buf[pos + 32], buf[pos + 33], buf[pos + 34], buf[pos + 35]]);
                let tracks = u32::from_le_bytes([buf[pos + 40], buf[pos + 41], buf[pos + 42], buf[pos + 43]]);
                let tot = fsize + 8;
                if (1..=500_000).contains(&frames) && frames == frames2 && (64..=8192).contains(&w) && (64..=8192).contains(&h) && fn_ > 0 && fd_ > 0 && tot >= 1024 && tot <= sz - (off + pos as u64) {
                    bk2_idx += 1;
                    let abs_off = off + pos as u64;
                    found.push(RpfEntry {
                        source_rpf: path.clone(),
                        rpf_name: rname.clone(),
                        name: format!("video_{bk2_idx:03}_{w}x{h}_{frames}f.bk2"),
                        kind: "BK2",
                        offset: abs_off,
                        comp_size: tot,
                        raw_size: tot,
                        fps: (fn_ as f32) / (fd_ as f32),
                        tracks,
                        chunks: Vec::new(),
                    });
                    let next_page = (abs_off + tot + 0xfff) & !0xfff;
                    if next_page > off + read_n as u64 {
                        off = next_page;
                        jumped = true;
                        need_seek = true;
                        break;
                    } else {
                        pos = (next_page - off) as usize;
                        continue;
                    }
                }
            }
            pos += 0x1000;
        }
        if !jumped {
            off += read_n as u64;
        }
    }

    let awc_positions: Vec<usize> = found.iter().enumerate().filter(|(_, e)| e.kind == "AWC").map(|(i, _)| i).collect();
    for (k, &i) in awc_positions.iter().enumerate() {
        let nxt = if k + 1 < awc_positions.len() { found[awc_positions[k + 1]].offset } else { sz };
        let csz = (nxt - found[i].offset).min(32 * 1024 * 1024);
        found[i].comp_size = csz;
        found[i].raw_size = csz;
    }

    if sz <= 512 * 1024 * 1024 {
        f.seek(SeekFrom::Start(0))?;
        let mut data = Vec::with_capacity(sz as usize);
        f.read_to_end(&mut data)?;
        let mut i = 0x40usize;
        let mut stitch_chunks: Vec<(u64, u64)> = Vec::new();
        let mut stitch_raw = 0u64;
        let mut stitch_head: Vec<u8> = Vec::new();
        let mut z_idx = 0usize;
        let mut decomp = Decompress::new(true);
        let mut out = vec![0u8; 131072];

        while i + 8 < data.len() {
            if data[i] == 0x78 && matches!(data[i + 1], 0x01 | 0x5e | 0x9c | 0xda) && (data[i + 2] & 0x06) != 0x06 {
                decomp.reset(true);
                let end = (i + 1024 * 1024).min(data.len());
                if let Ok(Status::StreamEnd) = decomp.decompress(&data[i..end], &mut out, FlushDecompress::Finish) {
                    let out_len = decomp.total_out() as usize;
                    let clen = decomp.total_in() as usize;
                    if out_len >= 32 && clen > 0 {
                        if stitch_chunks.is_empty() {
                            stitch_head = out[..out_len.min(300)].to_vec();
                        }
                        stitch_chunks.push((i as u64, clen as u64));
                        stitch_raw += out_len as u64;

                        if out_len != 65536 {
                            z_idx += 1;
                            let trimmed = stitch_head.iter().position(|&b| !matches!(b, 0xef | 0xbb | 0xbf | b' ' | b'\t' | b'\r' | b'\n')).unwrap_or(0);
                            let head = &stitch_head[trimmed..stitch_head.len().min(trimmed + 32)];
                            let (kind, name) = if head.starts_with(b"<?xml") || head.starts_with(b"<") {
                                let tag = extract_xml_tag(&stitch_head[trimmed..]).unwrap_or_else(|| "config".into());
                                ("XML", format!("{z_idx:04}_{tag}.xml"))
                            } else if stitch_head.starts_with(b"PSIN") {
                                ("PSIN", format!("{z_idx:04}_resource.psin"))
                            } else {
                                ("ZLIB", format!("{z_idx:04}_data.bin"))
                            };
                            found.push(RpfEntry {
                                source_rpf: path.clone(),
                                rpf_name: rname.clone(),
                                name,
                                kind,
                                offset: stitch_chunks[0].0,
                                comp_size: clen as u64,
                                raw_size: stitch_raw,
                                fps: 0.0,
                                tracks: 0,
                                chunks: std::mem::take(&mut stitch_chunks),
                            });
                            stitch_raw = 0;
                        }
                        i = (i + clen + 15) & !15;
                        continue;
                    }
                }
            }
            i += 16;
        }
    }

    Ok((hdr_info, found))
}

impl eframe::App for Rpf9App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (media_loading, media_status, wav_path, awc_streams, fps) = {
            let mut m = self.media.lock().unwrap();
            if let Some(mesh) = m.loaded_mesh.take() {
                self.mesh3d = Some(mesh);
                self.show_3d = true;
            }
            if self.textures.is_empty() && !m.frames.is_empty() {
                for (i, img) in m.frames.drain(..).enumerate() {
                    self.textures.push(ctx.load_texture(format!("f_{i}"), img, TextureOptions::LINEAR));
                }
                if !self.textures.is_empty() {
                    self.playing = true;
                    self.frame_idx = 0;
                    self.last_frame_time = Instant::now();
                    if let Some(w) = &m.wav_path {
                        play_wav_async(w);
                    }
                }
            }
            (m.loading, m.status.clone(), m.wav_path.clone(), m.awc_streams.clone(), m.fps)
        };

        if self.playing && !self.textures.is_empty() {
            let step = 1.0 / fps.max(1.0);
            if self.last_frame_time.elapsed().as_secs_f32() >= step {
                self.frame_idx = (self.frame_idx + 1) % self.textures.len();
                self.last_frame_time = Instant::now();
            }
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("top").frame(egui::Frame::new().fill(Color32::from_rgb(20, 21, 27)).inner_margin(10.0)).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(tex) = &self.logo_tex {
                    ui.image((tex.id(), egui::vec2(28.0, 28.0)));
                }
                ui.add_space(6.0);
                ui.label(RichText::new("PERCYSTAR").size(17.0).strong().color(Color32::from_rgb(252, 175, 23)));
                ui.label(RichText::new("RPF9").size(13.0).strong().color(Color32::from_rgb(255, 95, 185)));
                ui.add_space(10.0);
                if ui.button("Open Folder...").clicked() {
                    if let Some(folder) = rfd::FileDialog::new()
                        .set_directory(r"D:\Downloads\GTA6.Xbox.v1.0.2.1-CYBERLEEK")
                        .pick_folder()
                    {
                        self.start_folder_scan(folder);
                    }
                }
                if ui.button("Open .RPF").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_directory(r"D:\Downloads\GTA6.Xbox.v1.0.2.1-CYBERLEEK")
                        .add_filter("RPF Archives", &["rpf"])
                        .pick_file()
                    {
                        self.start_single_scan(p);
                    }
                }
                if ui.button("Open .OBJ").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("OBJ 3D Model", &["obj"]).pick_file() {
                        if let Some(m) = parse_obj(&p) {
                            self.stop_bink_player();
                            self.mesh3d = Some(m);
                            self.show_3d = true;
                        }
                    }
                }
                if self.is_folder {
                    if ui.button("Rescan").clicked() {
                        if let Some(folder) = self.target_path.clone() {
                            self.start_folder_scan(folder);
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Export All").clicked() {
                        let n = self.state.lock().unwrap().entries.len();
                        let all: Vec<usize> = (0..n).collect();
                        self.export_entries(&all);
                    }
                    if ui.button("Export Selected").clicked() {
                        if let Some(i) = self.selected {
                            self.export_entries(&[i]);
                        }
                    }
                    ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(190.0).hint_text("Search files..."));
                });
            });
        });

        egui::TopBottomPanel::bottom("bottom").frame(egui::Frame::new().fill(Color32::from_rgb(12, 13, 17)).inner_margin(egui::Margin::symmetric(12, 5))).show(ctx, |ui| {
            let (hdr, status, scanning) = {
                let st = self.state.lock().unwrap();
                (st.header_info.clone(), st.status.clone(), st.scanning)
            };
            ui.horizontal(|ui| {
                if scanning { draw_percystar_spinner(ui, 18.0); }
                ui.label(RichText::new(status).size(12.0).color(Color32::from_rgb(185, 190, 204)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(hdr).size(11.5).color(Color32::from_rgb(115, 122, 136)));
                });
            });
        });

        egui::SidePanel::left("tree").default_width(195.0).frame(egui::Frame::new().fill(Color32::from_rgb(16, 17, 21)).inner_margin(8.0)).show(ctx, |ui| {
            let (entries_all, rpf_names) = {
                let st = self.state.lock().unwrap();
                (st.entries.clone(), st.rpf_names.clone())
            };
            let cnt = |k: &str| entries_all.iter().filter(|e| e.kind == k).count();
            ui.label(RichText::new("ARCHIVE TREE").size(11.0).color(Color32::from_rgb(110, 118, 134)));
            ui.add_space(4.0);
            for (cat, label, count) in [
                (Category::All, "All Files", entries_all.len()),
                (Category::Vehicles, "Vehicles (.#ft)", cnt("VEH")),
                (Category::Models, "3D Models (.#dr/.#ft)", cnt("MODEL") + cnt("VEH")),
                (Category::Videos, "Videos (.bk2)", cnt("BK2")),
                (Category::Audio, "Audio (.awc)", cnt("AWC")),
                (Category::Xml, "Configs (.xml)", cnt("XML")),
                (Category::Resources, "Resources (.psin)", cnt("PSIN")),
                (Category::Binary, "Binary (.bin)", cnt("ZLIB")),
            ] {
                if ui.selectable_label(self.category == cat, format!("{label} ({count})")).clicked() {
                    self.category = cat;
                }
            }
            if rpf_names.len() > 1 {
                ui.add_space(10.0);
                ui.label(RichText::new("ARCHIVES").size(11.0).color(Color32::from_rgb(110, 118, 134)));
                ui.add_space(4.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if ui.selectable_label(self.selected_rpf == "All RPFs", "All RPFs").clicked() {
                        self.selected_rpf = "All RPFs".into();
                    }
                    for rn in &rpf_names {
                        if ui.selectable_label(&self.selected_rpf == rn, rn).clicked() {
                            self.selected_rpf = rn.clone();
                        }
                    }
                });
            }
        });

        egui::SidePanel::left("filelist").default_width(390.0).frame(egui::Frame::new().fill(Color32::from_rgb(19, 21, 26)).inner_margin(8.0)).show(ctx, |ui| {
            let entries = self.state.lock().unwrap().entries.clone();
            let q = self.filter.to_lowercase();
            let filtered: Vec<(usize, RpfEntry)> = entries
                .into_iter()
                .enumerate()
                .filter(|(_, e)| {
                    let cat_ok = match self.category {
                        Category::All => true,
                        Category::Vehicles => e.kind == "VEH",
                        Category::Models => e.kind == "MODEL" || e.kind == "VEH",
                        Category::Videos => e.kind == "BK2",
                        Category::Audio => e.kind == "AWC",
                        Category::Xml => e.kind == "XML",
                        Category::Resources => e.kind == "PSIN",
                        Category::Binary => e.kind == "ZLIB",
                    };
                    let rpf_ok = self.selected_rpf == "All RPFs" || e.rpf_name == self.selected_rpf;
                    cat_ok && rpf_ok && (q.is_empty() || e.name.to_lowercase().contains(&q) || e.rpf_name.to_lowercase().contains(&q))
                })
                .collect();

            egui::ScrollArea::vertical().show_rows(ui, 24.0, filtered.len(), |ui, range| {
                for &(idx, ref e) in &filtered[range] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{:<4}", e.kind)).strong().size(11.5).color(kind_color(e.kind)));
                        let sz_s = format_size(e.raw_size);
                        let label = if self.is_folder {
                            format!("{} / {}  ({})", e.rpf_name, e.name, sz_s)
                        } else {
                            format!("{}  ({})", e.name, sz_s)
                        };
                        if ui.selectable_label(self.selected == Some(idx), RichText::new(label).size(12.5)).clicked() {
                            self.select_entry(idx);
                        }
                    });
                }
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::from_rgb(13, 14, 17)).inner_margin(12.0)).show(ctx, |ui| {
            if self.parent_hwnd == 0 {
                let h = unsafe { GetActiveWindow() };
                if h != 0 { self.parent_hwnd = h; }
            }
            let (bink_pid, bink_hwnd_opt, bink_clip_opt) = {
                let m = self.media.lock().unwrap();
                (m.bink_child_pid, m.bink_hwnd, m.bink_clip.clone())
            };

            if let Some(pid) = bink_pid {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&media_status).strong().color(Color32::from_rgb(252, 175, 23)));
                    if ui.button("Stop").clicked() {
                        self.stop_bink_player();
                    }
                    if ui.button("Pop-out").clicked() {
                        if let Some(cp) = bink_clip_opt {
                            self.stop_bink_player();
                            let binkplay = std::env::current_exe()
                                .ok()
                                .and_then(|p| p.parent().map(|d| d.join("binkplay.exe")))
                                .filter(|p| p.exists())
                                .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)\RADVideo\binkplay.exe"));
                            let _ = Command::new(binkplay).arg(&cp).spawn();
                        }
                    }
                });
                ui.add_space(6.0);
                let rect = ui.available_rect_before_wrap();
                let ppp = ctx.pixels_per_point();
                let rx = (rect.min.x * ppp) as i32;
                let ry = (rect.min.y * ppp) as i32;
                let rw = (rect.width() * ppp) as i32;
                let rh = (rect.height() * ppp) as i32;
                if let Some(bh) = bink_hwnd_opt {
                    let cur = [rx, ry, rw, rh];
                    if cur != self.last_dock_rect {
                        self.last_dock_rect = cur;
                        unsafe { MoveWindow(bh, rx, ry, rw.max(64), rh.max(64), 1); }
                    }
                } else if self.parent_hwnd != 0 {
                    if let Some(bh) = find_hwnd_for_pid(pid) {
                        dock_child_hwnd(bh, self.parent_hwnd, rx, ry, rw, rh);
                        self.media.lock().unwrap().bink_hwnd = Some(bh);
                        self.last_dock_rect = [rx, ry, rw, rh];
                    } else {
                        ctx.request_repaint();
                    }
                }
            } else if media_loading {
                ui.horizontal(|ui| {
                    draw_percystar_spinner(ui, 24.0);
                    ui.label(RichText::new(&media_status).color(Color32::from_rgb(252, 175, 23)));
                });
            } else if !self.textures.is_empty() {
                ui.horizontal(|ui| {
                    if ui.button(if self.playing { "Pause" } else { "Play" }).clicked() {
                        self.playing = !self.playing;
                        if self.playing {
                            if let Some(w) = &wav_path { play_wav_async(w); }
                        } else {
                            stop_wav();
                        }
                    }
                    if ui.button("Restart").clicked() {
                        self.frame_idx = 0;
                        self.playing = true;
                        if let Some(w) = &wav_path { play_wav_async(w); }
                    }
                    if ui.button("Pop-out BinkPlay").clicked() {
                        let bk2_p = std::env::temp_dir().join("rpf9_preview_bk2").join("clip.bk2");
                        if bk2_p.exists() {
                            let _ = Command::new(r"C:\Program Files (x86)\RADVideo\binkplay.exe").arg(&bk2_p).spawn();
                        }
                    }
                    ui.add(egui::Slider::new(&mut self.frame_idx, 0..=self.textures.len() - 1).text("Frame"));
                    ui.label(RichText::new(&media_status).size(12.0).color(Color32::from_rgb(140, 148, 164)));
                });
                ui.add_space(8.0);
                let tex = &self.textures[self.frame_idx];
                let avail = ui.available_size();
                let tsz = tex.size_vec2();
                let scale = (avail.x / tsz.x).min(avail.y / tsz.y).min(1.0);
                ui.image((tex.id(), tsz * scale));
            } else if !awc_streams.is_empty() {
                ui.label(RichText::new(&media_status).strong().color(Color32::WHITE));
                ui.add_space(8.0);
                if ui.button("Stop Audio").clicked() {
                    stop_wav();
                }
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (idx, wp) in &awc_streams {
                        ui.horizontal(|ui| {
                            if ui.button("Play").clicked() {
                                play_wav_async(wp);
                            }
                            ui.label(format!("Stream #{idx}: {}", wp.file_name().unwrap_or_default().to_string_lossy()));
                        });
                    }
                });
            } else if !media_status.is_empty() {
                ui.label(RichText::new(&media_status).color(Color32::from_rgb(255, 140, 120)));
            } else if let Some(mesh) = self.mesh3d.as_mut() {
                let mut toggle_3d = self.show_3d;
                ui.horizontal(|ui| {
                    if ui.selectable_label(!toggle_3d, "Hex / Info").clicked() { toggle_3d = false; }
                    if ui.selectable_label(toggle_3d, "3D Model View").clicked() { toggle_3d = true; }
                });
                self.show_3d = toggle_3d;
                ui.separator();
                if self.show_3d {
                    render_mesh3d_viewport(ui, mesh);
                } else {
                    egui::ScrollArea::both().show(ui, |ui| {
                        ui.add(egui::TextEdit::multiline(&mut self.preview).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
                    });
                }
            } else {
                egui::ScrollArea::both().show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut self.preview).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
                });
            }
        });
    }
}

fn batch_extract_vehicles(levels_rpf: &PathBuf, out_dir: &PathBuf) {
    let _ = fs::create_dir_all(out_dir);
    let sz = fs::metadata(levels_rpf).map(|m| m.len()).unwrap_or(0);
    // Vehicle section in levels.rpf runs from 0x10b2000000 (~71.71 GB) to EOF (~76.16 GB)
    let start_off: u64 = 0x10b2_0000_00u64.min(sz);
    let total_len = sz.saturating_sub(start_off);
    let n_threads = 4u64;
    let slice_len = ((total_len / n_threads) + 0xfff) & !0xfff;
    let found_all: Arc<Mutex<Vec<(u64, String)>>> = Arc::new(Mutex::new(Vec::new()));

    eprintln!("Scanning {:.2} GB vehicle zone in {} with {} threads...", total_len as f64 / 1e9, levels_rpf.display(), n_threads);
    std::thread::scope(|s| {
        for t in 0..n_threads {
            let s0 = start_off + t * slice_len;
            let s1 = (s0 + slice_len).min(sz);
            let found_all = Arc::clone(&found_all);
            s.spawn(move || {
                let Ok(mut f) = File::open(levels_rpf) else { return };
                let mut buf = vec![0u8; 4 * 1024 * 1024];
                let mut probe_dec = Decompress::new(true);
                let mut probe_out = vec![0u8; 32768];
                let mut off = s0;
                while off < s1 {
                    if f.seek(SeekFrom::Start(off)).is_err() { break; }
                    let to_read = (buf.len() as u64).min(s1 - off) as usize;
                    let Ok(n) = f.read(&mut buf[..to_read]) else { break };
                    if n == 0 { break; }
                    let mut pos = 0usize;
                    while pos + 44 <= n {
                        let h0 = u64::from_le_bytes([buf[pos], buf[pos+1], buf[pos+2], buf[pos+3], buf[pos+4], buf[pos+5], buf[pos+6], buf[pos+7]]);
                        let h1 = u64::from_le_bytes([buf[pos+8], buf[pos+9], buf[pos+10], buf[pos+11], buf[pos+12], buf[pos+13], buf[pos+14], buf[pos+15]]);
                        if h0 == 0 && h1 == 0 && buf[pos + 0x10] == 0x78 && matches!(buf[pos + 0x11], 0x9c | 0xda) && (buf[pos + 0x12] & 0x06) != 0x06 {
                            let p = pos + 0x10;
                            let end = (p + 32768).min(n);
                            probe_dec.reset(true);
                            let _ = probe_dec.decompress(&buf[p..end], &mut probe_out, FlushDecompress::None);
                            let olen = probe_dec.total_out() as usize;
                            if olen >= 0x80 {
                                let vft = u64::from_le_bytes([probe_out[0], probe_out[1], probe_out[2], probe_out[3], probe_out[4], probe_out[5], probe_out[6], probe_out[7]]);
                                let bmap = u64::from_le_bytes([probe_out[8], probe_out[9], probe_out[10], probe_out[11], probe_out[12], probe_out[13], probe_out[14], probe_out[15]]);
                                // Full vehicles have large virtual headers (> 48KB)
                                if vft == 0 && (0x5000_c000..0x5800_0000).contains(&bmap) && &probe_out[0x10..0x15] != b"0DRPz" {
                                    if let Some((mname, is_veh, vlen)) = probe_large_rage9_model(levels_rpf, off + p as u64) {
                                        let lo = mname.to_ascii_lowercase();
                                        // Keep full vehicles (_hi.#ft preferred, skip _mid/_dmg_/tiny mods)
                                        let is_full = is_veh
                                            && vlen >= 50_000
                                            && lo.ends_with(".#ft")
                                            && !lo.contains("_mid.")
                                            && !lo.contains("_dmg_")
                                            && !lo.contains("_livery")
                                            && !lo.contains("_roof_")
                                            && !lo.contains("_spoiler")
                                            && !lo.contains("_exhaust")
                                            && !lo.contains("_bumf_")
                                            && !lo.contains("_bumr_")
                                            && !lo.contains("_skirt_")
                                            && !lo.contains("_bonnet")
                                            && !lo.contains("_wing_")
                                            && !lo.contains("_door_")
                                            && !lo.contains("_mirror")
                                            && !lo.contains("_lights_")
                                            && !lo.contains("_intakes_");
                                        if is_full {
                                            found_all.lock().unwrap().push((off + p as u64, mname));
                                        }
                                    }
                                }
                            }
                        }
                        pos += 0x1000;
                    }
                    off += n as u64;
                }
            });
        }
    });

    // Deduplicate: prefer `_hi.#ft` over base `.#ft` for each vehicle stem
    let mut best: HashMap<String, (u64, String, bool)> = HashMap::new();
    for (off, name) in found_all.lock().unwrap().iter() {
        let stem_raw = name.trim_end_matches(".#ft");
        let is_hi = stem_raw.ends_with("_hi");
        let base = stem_raw.trim_end_matches("_hi").to_ascii_lowercase();
        match best.get(&base) {
            Some((_, _, prev_hi)) if *prev_hi && !is_hi => {}
            _ => {
                best.insert(base, (*off, name.clone(), is_hi));
            }
        }
    }
    let mut list: Vec<(String, u64, String)> = best.into_iter().map(|(b, (o, n, _))| (b, o, n)).collect();
    list.sort_by(|a, b| a.0.cmp(&b.0));
    eprintln!("Found {} unique full GTA 6 vehicles! Extracting .OBJs...", list.len());

    let ok_cnt = Arc::new(Mutex::new(0usize));
    let next_i = Arc::new(Mutex::new(0usize));
    std::thread::scope(|s| {
        for _ in 0..4 {
            let list = &list;
            let ok_cnt = Arc::clone(&ok_cnt);
            let next_i = Arc::clone(&next_i);
            s.spawn(move || loop {
                let idx = {
                    let mut g = next_i.lock().unwrap();
                    let i = *g;
                    *g += 1;
                    i
                };
                if idx >= list.len() { return; }
                let (base, off, name) = &list[idx];
                if let Ok(mesh) = decode_rage9_model_from_rpf(levels_rpf, *off, name) {
                    let out_p = out_dir.join(format!("{base}.obj"));
                    if mesh.export_obj(&out_p).is_ok() {
                        let mut c = ok_cnt.lock().unwrap();
                        *c += 1;
                        if *c <= 30 || *c % 25 == 0 {
                            eprintln!("  [{}/{}] {} ({} v, {} f)", *c, list.len(), base, mesh.verts.len(), mesh.tris.len());
                        }
                    }
                }
            });
        }
    });
    let final_ok = *ok_cnt.lock().unwrap();
    eprintln!("DONE: Exported {final_ok}/{} vehicles to {}", list.len(), out_dir.display());
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 4 && args[1] == "--extract-vehicles" {
        batch_extract_vehicles(&PathBuf::from(&args[2]), &PathBuf::from(&args[3]));
        return Ok(());
    }
    let mut vb = egui::ViewportBuilder::default().with_inner_size([1320.0, 800.0]);
    if let Ok(img) = image::load_from_memory(include_bytes!("../assets/logo.png")) {
        let rgba = img.to_rgba8();
        vb = vb.with_icon(egui::IconData {
            rgba: rgba.as_raw().to_vec(),
            width: rgba.width(),
            height: rgba.height(),
        });
    }
    let opts = eframe::NativeOptions {
        viewport: vb,
        ..Default::default()
    };
    eframe::run_native("PercyStar RPF9 Explorer", opts, Box::new(|cc| Ok(Box::new(Rpf9App::new(cc)))))
}
