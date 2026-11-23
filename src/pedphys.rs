// Percy6Stars by Ahmet Dedeler (https://github.com/Ahmet-Dedeler, https://ahmetdedeler.com)
// Fork of PercyStar by rpatty-dev (https://github.com/rpatty-dev/PercyStar).
// Source: https://github.com/Ahmet-Dedeler/Percy6Stars
// License: PercyStar Non-Commercial Attribution & Copyleft License v1.2 (see LICENSE)
// If you copy, vendor or port this file, keep this header and credit both projects in your README.
//! Ped physics resources: the unnamed RAGE 9 resources in character packs that hold strand
//! hair, verlet cloth, soft bodies and per-head identity deltas.
//!
//! None of them carry a `name.#xx` string. The byte at system +0x1A is a family type and the
//! u32 at +0x1C is 0 or the constant `7d8f3db3`:
//!
//! | family | key |
//! |---|---|
//! | hair / fur groom (`HAIR`) | type 3, `7d8f3db3`, one graphics buffer (flags 0x1380) |
//! | verlet cloth (`CLOTH`) | type 2, +0x1C = 0, no graphics buffer |
//! | soft body (`SOFT`) | type 2, `7d8f3db3` (same layout as cloth) |
//! | head identity delta (`HDELTA`) | 240-byte system page, one 0x1601 buffer, base-head joaat at +0x58 |
//!
//! Grooms are guide strands (S strands x K points x 4 half floats) in model space, Z-up metres,
//! the same frame as the ped head drawables. Cloth stores particles (4 x f32) and distance
//! constraints. Head deltas store one quantised offset per vertex of a base head, in a vertex
//! order of their own. Layouts follow the GTA6-Asset-Info "ped physics resources" page.

mod bones;

use crate::model3d::{self, LineOverlay, Mesh3D};
use crate::RpfEntry;
use eframe::egui::{self, Color32, RichText};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// The kind strings these families use in the scan and its cache.
pub const KINDS: [&str; 4] = ["HAIR", "CLOTH", "SOFT", "HDELTA"];

/// `7d8f3db3` at system +0x1C on grooms, soft bodies and a few other families. Its name is
/// not known.
const FAMILY_KEY: u32 = 0x7d8f_3db3;

pub fn is_kind(kind: &str) -> bool {
    KINDS.contains(&kind)
}

/// Sidebar and header label.
pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "HAIR" => "Hair groom",
        "CLOTH" => "Cloth",
        "SOFT" => "Soft body",
        "HDELTA" => "Head delta",
        _ => "",
    }
}

/// One line saying what the thing is, shown at the top of its preview.
pub fn kind_blurb(kind: &str) -> &'static str {
    match kind {
        "HAIR" => "Guide strands the game grows hair, beards, brows and fur from.",
        "CLOTH" => "Verlet cloth: particles held together by distance constraints (flags, awnings, garment pieces).",
        "SOFT" => "Soft-body cage that jiggles parts of a ped's body, driven by helper bones.",
        "HDELTA" => "Per-vertex offsets that turn a base head into one specific face.",
        _ => "",
    }
}

pub fn search_words(kind: &str) -> &'static str {
    match kind {
        "HAIR" => "hair groom strand strands fur beard brows lashes ped physics",
        "CLOTH" => "cloth verlet particles flag garment ped physics",
        "SOFT" => "soft body softbody jiggle ped physics",
        "HDELTA" => "head delta face identity morph ped",
        _ => "",
    }
}

pub fn kind_color(kind: &str) -> Color32 {
    match kind {
        "HAIR" => Color32::from_rgb(255, 176, 92),
        "CLOTH" => Color32::from_rgb(92, 214, 255),
        "SOFT" => Color32::from_rgb(255, 128, 196),
        _ => Color32::from_rgb(196, 160, 255),
    }
}

// ---------------------------------------------------------------- little readers

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    b.get(o..o + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap()))
}
fn f32_at(b: &[u8], o: usize) -> Option<f32> {
    u32_at(b, o).map(f32::from_bits)
}
fn vec3_at(b: &[u8], o: usize) -> Option<[f32; 3]> {
    Some([f32_at(b, o)?, f32_at(b, o + 4)?, f32_at(b, o + 8)?])
}
/// System-page pointer (`0x5xxxxxxx`) to an offset inside `b`.
fn sys_ptr(b: &[u8], o: usize) -> Option<usize> {
    let q = u64_at(b, o)?;
    (q >> 28 == 5).then(|| (q & 0x0FFF_FFFF) as usize).filter(|&p| p < b.len())
}
/// Graphics-page pointer (`0x6xxxxxxx`) to an offset in the graphics pages.
fn gfx_ptr(b: &[u8], o: usize) -> Option<usize> {
    let q = u64_at(b, o)?;
    (q >> 28 == 6).then_some((q & 0x0FFF_FFFF) as usize)
}

/// IEEE half float to f32.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if man == 0 => sign,
        0 => {
            let v = man as f32 * (1.0 / 16_777_216.0);
            return if sign != 0 { -v } else { v };
        }
        31 => sign | 0x7f80_0000 | (man << 13),
        _ => sign | ((exp + 112) << 23) | (man << 13),
    };
    f32::from_bits(bits)
}

/// Bob Jenkins one-at-a-time hash, lower-cased, as RAGE names things.
pub fn joaat(s: &str) -> u32 {
    crate::psin::joaat(s)
}

fn bone_table() -> &'static HashMap<u32, &'static str> {
    static T: OnceLock<HashMap<u32, &'static str>> = OnceLock::new();
    T.get_or_init(|| bones::NAMES.iter().map(|&n| (joaat(n), n)).collect())
}

/// Bone (or tuning profile) name for a joaat hash, if it is one the app knows.
pub fn bone_name(h: u32) -> Option<&'static str> {
    bone_table().get(&h).copied()
}

/// A hash as its name, or as hex.
pub fn hash_label(h: u32) -> String {
    bone_name(h).map(str::to_string).unwrap_or_else(|| format!("0x{h:08x}"))
}

// ---------------------------------------------------------------- scanner probe

/// What the scanner learns from a resource's first system stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    pub kind: &'static str,
    pub name: String,
    /// Strands (hair), particles (cloth, soft body) or vertices (head delta).
    pub count: u32,
}

/// Recognises a ped physics resource from the start of its system pages. `sys` is the first
/// decompressed system stream (the scanner may cut it at 32 KiB); `whole_len` is its untrimmed
/// length when known. `off` names the entry.
pub fn probe(sys: &[u8], whole_len: usize, off: u64) -> Option<Probe> {
    if sys.len() < 0x48 || u64_at(sys, 0)? != 0 {
        return None;
    }
    let pm = u64_at(sys, 8)?;
    if pm >> 28 != 5 {
        return None;
    }
    // graphics page count from the page map, when it lies inside the stream we have
    let pm_off = (pm & 0x0FFF_FFFF) as usize;
    let gfx_pages = sys.get(pm_off + 9).copied();
    let ty = sys[0x1A];
    let keyed = u32_at(sys, 0x1C)? == FAMILY_KEY;

    // head identity delta: fixed 240-byte system page, descriptor at 0x70 with flags 0x1601
    if whole_len == 240 && sys.len() >= 240 && u64_at(sys, 0x10)? == 0x5000_0070 && u16_at(sys, 0x80)? == 0x1601 {
        let bytes = u32_at(sys, 0x78)? as usize * 4;
        if bytes < 0x20 || (bytes - 0x20) % 8 != 0 {
            return None;
        }
        let v = ((bytes - 0x20) / 8) as u32;
        let base = u32_at(sys, 0x58)?;
        return Some(Probe {
            kind: "HDELTA",
            name: format!("headdelta_{off:09x}_{}_{base:08x}.hdelta", lod_tag(v)),
            count: v,
        });
    }

    if ty == 3 && keyed && sys.len() >= 0x140 && gfx_pages != Some(0) {
        let desc = sys_ptr(sys, 0x70)?;
        if u32_at(sys, desc + 0x10)? != 0x1380 || gfx_ptr(sys, desc + 0x18).is_none() {
            return None;
        }
        let strands = u16_at(sys, 0xDE)? as u32;
        let n_bones = u32_at(sys, 0x98)? as usize;
        let first_bone = sys_ptr(sys, 0xA0)
            .filter(|_| n_bones > 0)
            .and_then(|p| u32_at(sys, p))
            .and_then(bone_name)
            .map(|b| format!("_{b}"))
            .unwrap_or_default();
        return Some(Probe {
            kind: "HAIR",
            name: format!("groom_{off:09x}_{strands}s{first_bone}.hair"),
            count: strands,
        });
    }

    if ty == 2 {
        // cloth has no graphics pages; the soft-body variant is keyed
        if !keyed && gfx_pages.is_some_and(|p| p != 0) {
            return None;
        }
        let pp = sys_ptr(sys, 0x28).or_else(|| (u64_at(sys, 0x28)? >> 28 == 5).then_some(usize::MAX))?;
        let pe = sys_ptr(sys, 0x38).or_else(|| (u64_at(sys, 0x38)? >> 28 == 5).then_some(usize::MAX))?;
        let (n, ncap) = (u16_at(sys, 0x30)?, u16_at(sys, 0x32)?);
        let (m, mcap) = (u16_at(sys, 0x40)?, u16_at(sys, 0x42)?);
        if n == 0 || ncap < n || mcap < m || pp == 0 || pe == 0 {
            return None;
        }
        let (kind, stem, ext) = if keyed {
            ("SOFT", "softbody", "softbody")
        } else {
            ("CLOTH", "cloth", "cloth")
        };
        return Some(Probe {
            kind,
            name: format!("{stem}_{off:09x}_{n}p_{m}e.{ext}"),
            count: n as u32,
        });
    }
    None
}

/// Small resources are sometimes packed straight behind another one instead of starting on a
/// 4 KiB page, where the page-by-page scan never looks. Starting at the zlib stream `first` (a
/// recognised resource), walk the streams that follow (each starts 16-byte aligned after zero
/// padding) and return the ones that begin a ped physics resource. The walk ends at a stream
/// that starts on a page (the page scan sees those), at anything that isn't a zlib stream, or
/// after `max_walk` bytes.
pub fn packed_followers(buf: &[u8], first: usize, max_walk: usize, file_off: u64, path: &Path, rname: &str) -> Vec<RpfEntry> {
    walk_packed(buf, WalkStart::After(first), max_walk, file_off, path, rname).0
}

/// Where [`walk_packed`] starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WalkStart {
    /// At a recognised resource's zlib stream: skip it and probe the ones behind it.
    After(usize),
    /// At a stream an earlier walk couldn't finish: inflate and probe it (it may start a page:
    /// only streams found by searching stop there).
    Stream(usize),
    /// Where an earlier walk ran out of its buffer between streams: find the next one from here.
    Resume(usize),
}

/// [`packed_followers`] that also says when the walk ran out of `buf` in the middle of a group:
/// the file offset to carry on from, and how (positions in it relative to a window starting at
/// that offset). The scanner reads big archives in windows, so a group near a window's end
/// carries on in the next one.
pub fn walk_packed(buf: &[u8], start: WalkStart, max_walk: usize, file_off: u64, path: &Path, rname: &str) -> (Vec<RpfEntry>, Option<(u64, WalkStart)>) {
    use flate2::{Decompress, FlushDecompress, Status};
    enum Next {
        At(usize),
        Stop,
        OutOfBuf,
    }
    // next stream after `pos`: 16-byte aligned in the file (a resumed window can start
    // anywhere), after zero padding
    let next = |pos: usize| -> Next {
        let mut n = pos + ((16 - ((file_off + pos as u64) % 16) as usize) % 16);
        let pad_end = (n + 0x1000).min(buf.len());
        while n + 16 <= pad_end && buf[n..n + 16].iter().all(|&b| b == 0) {
            n += 16;
        }
        if n + 16 > buf.len() {
            return Next::OutOfBuf;
        }
        let on_page = (file_off + n as u64) & 0xFFF == 0x10;
        if on_page || !model3d::is_zlib_head(&buf[n..]) {
            return Next::Stop;
        }
        Next::At(n)
    };
    let mut out = Vec::new();
    let (first, mut s, mut probe_s) = match start {
        WalkStart::After(f) => (f, f, false),
        WalkStart::Stream(f) => (f, f, true),
        WalkStart::Resume(p) => match next(p) {
            Next::At(n) => (p, n, true),
            Next::Stop => return (out, None),
            Next::OutOfBuf => return (out, Some((file_off + p as u64, WalkStart::Resume(0)))),
        },
    };
    let mut dec = Decompress::new(true);
    let mut sink = vec![0u8; 1 << 16];
    let mut head = Vec::new();
    let limit = first + max_walk;
    while s < limit {
        // inflate the stream at `s` to find where it ends, keeping its first 32 KiB
        dec.reset(true);
        head.clear();
        let mut pos = s;
        let mut ran_out = false;
        let ended = loop {
            let (bi, bo) = (dec.total_in(), dec.total_out());
            let Ok(st) = dec.decompress(&buf[pos..], &mut sink, FlushDecompress::None) else {
                break false;
            };
            let (cin, cout) = ((dec.total_in() - bi) as usize, (dec.total_out() - bo) as usize);
            pos += cin;
            if head.len() < 32768 {
                head.extend_from_slice(&sink[..cout.min(32768 - head.len())]);
            }
            if st == Status::StreamEnd {
                break true;
            }
            if pos >= buf.len() {
                ran_out = true;
                break false;
            }
            if cin == 0 && cout == 0 {
                break false;
            }
        };
        if !ended {
            // a stream cut off by the end of the window is walked again from its start, probed
            // unless it is the recognised resource the walk began at
            let again = if probe_s { WalkStart::Stream(0) } else { WalkStart::After(0) };
            return (out, ran_out.then_some((file_off + s as u64, again)));
        }
        if probe_s {
            let whole = dec.total_out() as usize;
            if let Some(p) = probe(&head, whole, file_off + s as u64) {
                out.push(RpfEntry {
                    source_rpf: path.to_path_buf(),
                    rpf_name: rname.to_string(),
                    name: p.name,
                    kind: p.kind,
                    offset: file_off + s as u64,
                    comp_size: dec.total_in(),
                    raw_size: whole as u64,
                    tracks: p.count,
                    chunks: Vec::new(),
                });
            }
        }
        match next(pos) {
            Next::At(n) => {
                s = n;
                probe_s = true;
            }
            Next::Stop => break,
            Next::OutOfBuf => return (out, Some((file_off + pos as u64, WalkStart::Resume(0)))),
        }
    }
    (out, None)
}

/// `lod0`..`lod3` from a head delta's vertex count.
fn lod_tag(v: u32) -> String {
    match head_lod(v as usize) {
        Some(l) => format!("lod{l}"),
        None => format!("{v}v"),
    }
}

/// Head LOD (0 = full detail) from a delta's vertex count.
pub fn head_lod(v: usize) -> Option<usize> {
    match v {
        8983 | 8988 => Some(0),
        2958 => Some(1),
        1680 => Some(2),
        599 => Some(3),
        _ => None,
    }
}

/// Head drawable stem (name without `.#dr`) and the joaat names a delta can use for it.
fn base_hash_names(stem: &str) -> [(u32, String); 3] {
    [
        (joaat(stem), stem.to_string()),
        (joaat(&format!("{stem}_lod2")), format!("{stem}_lod2")),
        (joaat(&format!("{stem}_lod3")), format!("{stem}_lod3")),
    ]
}

fn head_stem(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".#dr").unwrap_or(name);
    stem.starts_with("head_").then_some(stem)
}

/// joaat -> name for every head drawable in `entries` (and its `_lod2` / `_lod3` names).
pub fn head_hash_names(entries: &[RpfEntry]) -> HashMap<u32, String> {
    let mut m = HashMap::new();
    for e in entries.iter().filter(|e| e.kind == "MODEL") {
        if let Some(stem) = head_stem(&e.name) {
            if !m.contains_key(&joaat(stem)) {
                for (h, n) in base_hash_names(stem) {
                    m.insert(h, n);
                }
            }
        }
    }
    m
}

/// After a scan: replace the base-head hash in head delta names with the head's name when the
/// same archive holds that head drawable.
pub fn name_head_deltas(entries: &mut [RpfEntry]) {
    if !entries.iter().any(|e| e.kind == "HDELTA") {
        return;
    }
    let names = head_hash_names(entries);
    for e in entries.iter_mut().filter(|e| e.kind == "HDELTA") {
        let Some(hex) = e.name.strip_suffix(".hdelta").and_then(|s| s.rsplit('_').next()) else {
            continue;
        };
        let Ok(h) = u32::from_str_radix(hex, 16) else { continue };
        if hex.len() != 8 {
            continue;
        }
        if let Some(n) = names.get(&h) {
            e.name = format!("{}{n}.hdelta", &e.name[..e.name.len() - ".hdelta".len() - 8]);
        }
    }
}

// ---------------------------------------------------------------- reading

/// System and graphics pages of the resource at `off`.
pub fn read_resource(path: &Path, off: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
    model3d::read_resource_from_file(path, off)
}

/// Raw bytes for "Export" of an entry without a better format: system then graphics pages.
pub fn resource_bytes(path: &Path, off: u64) -> std::io::Result<Vec<u8>> {
    let (mut s, g) = read_resource(path, off).map_err(std::io::Error::other)?;
    s.extend_from_slice(&g);
    Ok(s)
}

// ---------------------------------------------------------------- hair groom

/// A decoded guide-strand groom.
#[derive(Clone, Debug, Default)]
pub struct Groom {
    /// Guide strands, points per strand, root triangles.
    pub strands: usize,
    pub points_per_strand: usize,
    pub root_tris: Vec<[u16; 3]>,
    /// `strands * points_per_strand` points, strand-major: x, y, z (Z-up metres) and w
    /// (thickness or mass; non-zero at the root).
    pub points: Vec<[f32; 4]>,
    /// Stored strand lengths (B0), when present.
    pub lengths: Vec<f32>,
    /// Bones the groom follows (joaat hashes).
    pub bones: Vec<u32>,
    /// Root bounding box (min, max) and its stored root radius.
    pub root_box: ([f32; 3], [f32; 3]),
    pub root_radius: f32,
    pub flags: u32,
    /// Graphics buffer size in bytes, from the descriptor.
    pub buffer_bytes: usize,
}

impl Groom {
    pub fn point(&self, s: usize, k: usize) -> [f32; 3] {
        let p = self.points[s * self.points_per_strand + k];
        [p[0], p[1], p[2]]
    }

    /// Bounding box over all points.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for p in &self.points {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        (!self.points.is_empty()).then_some((lo, hi))
    }
}

/// Offsets of the groom's graphics buffer header fields.
struct HairHeader {
    a: [u32; 8],
    b: [u32; 8],
    q: [u32; 4],
    s: usize,
    t: usize,
    k: usize,
    tail_off: usize,
}

fn hair_header(g: &[u8]) -> Result<HairHeader, String> {
    if g.len() < 0xC0 {
        return Err(format!("graphics buffer too small ({} bytes)", g.len()));
    }
    let r = |o| u32_at(g, o).unwrap();
    let mut a = [0u32; 8];
    let mut b = [0u32; 8];
    let mut q = [0u32; 4];
    for i in 0..8 {
        a[i] = r(i * 4);
        b[i] = r(0x30 + i * 4);
    }
    for (i, x) in q.iter_mut().enumerate() {
        *x = r(0xB0 + i * 4);
    }
    Ok(HairHeader {
        a,
        b,
        q,
        s: u16_at(g, 0x58).unwrap() as usize,
        t: u16_at(g, 0x5A).unwrap() as usize,
        k: r(0x5C) as usize,
        tail_off: r(0xAC) as usize,
    })
}

/// The groom's graphics buffer: graphics pages from the descriptor's pointer, trimmed to its size.
fn hair_buffer<'a>(sys: &[u8], gfx: &'a [u8]) -> Result<&'a [u8], String> {
    let desc = sys_ptr(sys, 0x70).ok_or("no buffer descriptor at +0x70")?;
    let bytes = u32_at(sys, desc + 8).ok_or("truncated descriptor")? as usize * 4;
    let start = gfx_ptr(sys, desc + 0x18).ok_or("descriptor without graphics pointer")?;
    gfx.get(start..start + bytes)
        .ok_or_else(|| format!("graphics buffer {start:#x}+{bytes:#x} outside {} bytes of pages", gfx.len()))
}

pub fn decode_groom(sys: &[u8], gfx: &[u8]) -> Result<Groom, String> {
    if sys.len() < 0x140 {
        return Err("system page too small for a groom".into());
    }
    if sys[0x1A] != 3 || u32_at(sys, 0x1C) != Some(FAMILY_KEY) {
        return Err("not a groom (type byte / key)".into());
    }
    let g = hair_buffer(sys, gfx)?;
    let h = hair_header(g)?;
    let (s, k, t) = (h.s, h.k, h.t);
    // 2..=70 in the shipped data
    if !(1..=128).contains(&k) {
        return Err(format!("implausible points per strand {k}"));
    }
    if u16_at(sys, 0xDE) != Some(s as u16) || u16_at(sys, 0xDC) != Some(t as u16) {
        return Err("strand / triangle counts disagree between system page and buffer".into());
    }
    let pts_off = h.b[5] as usize;
    if pts_off == 0 {
        return Err("no point section (B5)".into());
    }
    let pts = g.get(pts_off..pts_off + s * k * 8).ok_or("point section outside the buffer")?;
    let points = pts
        .chunks_exact(8)
        .map(|c| {
            let f = |i: usize| f16_to_f32(u16::from_le_bytes([c[i], c[i + 1]]));
            [f(0), f(2), f(4), f(6)]
        })
        .collect();
    let mut root_tris = Vec::new();
    if h.b[4] != 0 {
        let tri = g
            .get(h.b[4] as usize..h.b[4] as usize + t * 6)
            .ok_or("root triangles outside the buffer")?;
        root_tris = tri
            .chunks_exact(6)
            .map(|c| {
                [
                    u16::from_le_bytes([c[0], c[1]]),
                    u16::from_le_bytes([c[2], c[3]]),
                    u16::from_le_bytes([c[4], c[5]]),
                ]
            })
            .collect();
    }
    let mut lengths = Vec::new();
    if h.b[0] != 0 {
        let l = g
            .get(h.b[0] as usize..h.b[0] as usize + s * 2)
            .ok_or("strand lengths outside the buffer")?;
        lengths = l.chunks_exact(2).map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]]))).collect();
    }
    let n_bones = u32_at(sys, 0x98).unwrap_or(0) as usize;
    let bones = match sys_ptr(sys, 0xA0) {
        Some(p) if n_bones > 0 => (0..n_bones)
            .map(|i| u32_at(sys, p + i * 4).ok_or("bone list outside the page"))
            .collect::<Result<_, _>>()?,
        _ => Vec::new(),
    };
    let root_box = (vec3_at(sys, 0x30).unwrap(), vec3_at(sys, 0x40).unwrap());
    Ok(Groom {
        strands: s,
        points_per_strand: k,
        root_tris,
        points,
        lengths,
        bones,
        root_box,
        root_radius: f32_at(sys, 0x3C).unwrap_or(0.0).abs(),
        flags: u32_at(sys, 0x20).unwrap_or(0),
        buffer_bytes: g.len(),
    })
}

/// Bytes of a resource that the typed layout explains, and anything it couldn't.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Coverage {
    pub sys_typed: usize,
    pub sys_total: usize,
    pub gfx_typed: usize,
    pub gfx_total: usize,
    pub problems: Vec<String>,
}

impl Coverage {
    pub fn complete(&self) -> bool {
        self.problems.is_empty() && self.sys_typed == self.sys_total && self.gfx_typed == self.gfx_total
    }
}

fn a4(n: usize) -> usize {
    (n + 3) & !3
}

/// Byte coverage of a groom (port of check_hair.py / hair_layout.py): the system page must be
/// tiled by header, descriptor (+ aux block), bone list, part records, optional extra block and
/// page map; the graphics buffer by its typed sections, the implicit trailing arrays and the
/// tail records.
pub fn groom_coverage(sys: &[u8], gfx: &[u8]) -> Coverage {
    let mut c = Coverage {
        sys_total: sys.len(),
        ..Default::default()
    };
    groom_sys_coverage(sys, &mut c);
    match hair_buffer(sys, gfx) {
        Ok(g) => groom_gfx_coverage(g, &mut c),
        Err(e) => c.problems.push(e),
    }
    c
}

fn groom_sys_coverage(s: &[u8], c: &mut Coverage) {
    let q = |o: usize| u64_at(s, o).unwrap_or(0);
    let p = |o: usize| (q(o) & 0x0FFF_FFFF) as usize;
    let mut fields = vec![0x08, 0x70, 0xA0, 0xB0];
    if q(0x10) != 0 {
        fields.push(0x10);
    }
    for &o in &fields {
        if q(o) >> 28 != 5 || p(o) >= s.len() {
            c.problems.push(format!("bad system pointer at +{o:#x}"));
        }
    }
    if !c.problems.is_empty() {
        return;
    }
    let desc = p(0x70);
    let mut blocks: Vec<(usize, usize, &str)> = vec![(0, 0x140, "header"), (desc, 0x40, "descriptor")];
    let aux = q(desc + 0x28);
    if aux >> 28 == 5 {
        blocks.push(((aux & 0x0FFF_FFFF) as usize, 0x20, "descriptor aux"));
    }
    if q(desc + 0x18) >> 28 != 6 {
        c.problems.push("descriptor without graphics pointer".into());
    }
    let n_bones = u32_at(s, 0x98).unwrap_or(0) as usize;
    if n_bones > 0 {
        blocks.push((p(0xA0), n_bones.saturating_mul(4), "bones"));
    }
    let n_parts = u32_at(s, 0xA8).unwrap_or(0) as usize;
    if n_parts > 0 {
        blocks.push((p(0xB0), n_parts.saturating_mul(12), "parts"));
    }
    if q(0x10) != 0 {
        blocks.push((p(0x10), 0x10, "extra"));
    }
    let pm = p(0x08);
    blocks.push((pm, 0x20.min(s.len() - pm), "page map"));
    blocks.sort();
    let (mut cur, mut typed) = (0usize, 0usize);
    for (o, n, name) in blocks {
        if o < cur {
            c.problems.push(format!("overlap at {name} {o:#x}"));
            continue;
        }
        let gap = s.get(cur..o.min(s.len())).unwrap_or(&[]);
        if o - cur >= 16 || gap.iter().any(|&b| b != 0) {
            c.problems.push(format!("untyped system bytes before {name} {cur:#x}..{o:#x}"));
        } else {
            typed += o - cur;
        }
        let Some(block_end) = o.checked_add(n).filter(|&e| e <= s.len()) else {
            c.problems.push(format!("block {name} out of bounds {o:#x}+{n:#x} > {:#x}", s.len()));
            cur = s.len();
            continue;
        };
        typed += n;
        cur = block_end;
    }
    let rest = s.get(cur..).unwrap_or(&[]);
    if rest.len() >= 16 || rest.iter().any(|&b| b != 0) {
        c.problems.push(format!("untyped system tail {cur:#x}..{:#x}", s.len()));
    } else {
        typed += rest.len();
    }
    c.sys_typed = typed.min(s.len());
}

/// (name, offset, typed bytes) of every present section.
fn hair_sections(h: &HairHeader) -> Vec<(String, usize, usize)> {
    let (s, t, k) = (h.s, h.t, h.k);
    let a_size = [s * 4, s * 4, s * 4, s * 4, s * 8, s * 8, s * 12, s * 8];
    let b_size = [s * 2, s * k * 4, s * k * 2, s * k.saturating_sub(1) * 4, t * 6, s * k * 8, t * 4, t];
    let q_size = [s * 4, 4, s * 4, s * 4];
    let mut out = Vec::new();
    for i in 0..8 {
        if h.a[i] != 0 {
            out.push((format!("A{i}"), h.a[i] as usize, a_size[i]));
        }
    }
    for i in 0..8 {
        if h.b[i] != 0 {
            out.push((format!("B{i}"), h.b[i] as usize, b_size[i]));
        }
    }
    for i in 0..4 {
        if h.q[i] != 0 {
            out.push((format!("Q{i}"), h.q[i] as usize, q_size[i]));
        }
    }
    out.sort_by_key(|x| x.1);
    out
}

fn groom_gfx_coverage(g: &[u8], c: &mut Coverage) {
    c.gfx_total = g.len();
    let h = match hair_header(g) {
        Ok(h) => h,
        Err(e) => {
            c.problems.push(e);
            return;
        }
    };
    let (mut typed, mut cur) = (0xC0usize, 0xC0usize);
    for (name, o, n) in hair_sections(&h) {
        if o != cur {
            c.problems.push(format!("graphics gap before {name}: {cur:#x}..{o:#x}"));
        }
        if o + n > g.len() {
            c.problems.push(format!("section {name} runs past the buffer"));
            break;
        }
        typed += n;
        cur = o + a4(n);
        if g.get(o + n..cur).is_some_and(|pad| pad.iter().all(|&b| b == 0)) {
            typed += a4(n) - n;
        }
    }
    let end = if h.tail_off != 0 { h.tail_off } else { g.len() };
    // implicit trailing arrays: two u32 per strand, then a 2 x f16 range and {f16, u16} pairs
    if cur < end && end - cur >= 8 * h.s + 8 && (end - cur - 8 * h.s) % 4 == 0 {
        typed += end - cur;
        cur = end;
    }
    if cur != end {
        c.problems.push(format!("graphics gap before tail: {cur:#x}..{end:#x}"));
    }
    if h.tail_off != 0 && h.tail_off <= g.len() {
        let rec = g.len() - h.tail_off;
        typed += rec - rec % 4;
    }
    c.gfx_typed = typed.min(g.len());
}

/// Hair colours: warm roots fading to pale tips, with a little per-strand variation so
/// neighbouring strands read apart.
fn strand_color(s: usize, t: f32) -> Color32 {
    let jitter = ((s.wrapping_mul(2_654_435_761) >> 7) % 1000) as f32 / 1000.0;
    let root = [196.0 + 40.0 * jitter, 92.0 + 30.0 * jitter, 32.0];
    let tip = [255.0, 226.0 + 20.0 * jitter, 150.0 + 40.0 * jitter];
    let m = |a: f32, b: f32| (a + (b - a) * t).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(m(root[0], tip[0]), m(root[1], tip[1]), m(root[2], tip[2]))
}

/// Strands as polylines for the 3D view, coloured root to tip.
pub fn groom_overlay(g: &Groom) -> LineOverlay {
    let k = g.points_per_strand;
    let mut ov = LineOverlay {
        points: Vec::with_capacity(g.points.len()),
        colors: Vec::with_capacity(g.points.len()),
        segments: Vec::with_capacity(g.strands * k.saturating_sub(1)),
        ..Default::default()
    };
    for s in 0..g.strands {
        for i in 0..k {
            ov.points.push(g.point(s, i));
            ov.colors.push(strand_color(s, i as f32 / (k.max(2) - 1) as f32));
            if i > 0 {
                let a = (s * k + i - 1) as u32;
                ov.segments.push([a, a + 1]);
            }
        }
    }
    if k == 1 {
        ov.dot_px = 2.0;
    }
    ov.summary = format!("{} strands x {} points  ·  {} root triangles", g.strands, k, g.root_tris.len());
    ov
}

// ---------------------------------------------------------------- cloth / soft body

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClothEdge {
    pub a: u16,
    pub b: u16,
    pub rest: f32,
    pub flags: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Cloth {
    pub soft: bool,
    /// x, y, z and w (per-particle, probably mass).
    pub particles: Vec<[f32; 4]>,
    pub edges: Vec<ClothEdge>,
    /// Edges whose rest length equals the distance between their particles in the stored pose.
    pub rest_matches: usize,
    /// Edges that name a particle past the end.
    pub bad_edges: usize,
    /// Bone and profile names found among the page's u32 values.
    pub names: Vec<&'static str>,
}

pub fn decode_cloth(sys: &[u8]) -> Result<Cloth, String> {
    if sys.len() < 0x48 || sys[0x1A] != 2 {
        return Err("not a cloth resource (type byte)".into());
    }
    let pp = sys_ptr(sys, 0x28).ok_or("no particle array at +0x28")?;
    let pe = sys_ptr(sys, 0x38).ok_or("no edge array at +0x38")?;
    let n = u16_at(sys, 0x30).unwrap() as usize;
    let m = u16_at(sys, 0x40).unwrap() as usize;
    let pb = sys.get(pp..pp + n * 16).ok_or("particle array outside the page")?;
    let eb = sys.get(pe..pe + m * 16).ok_or("edge array outside the page")?;
    let particles: Vec<[f32; 4]> = pb
        .chunks_exact(16)
        .map(|c| std::array::from_fn(|i| f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap())))
        .collect();
    let edges: Vec<ClothEdge> = eb
        .chunks_exact(16)
        .map(|c| ClothEdge {
            a: u16::from_le_bytes([c[0], c[1]]),
            b: u16::from_le_bytes([c[2], c[3]]),
            rest: f32::from_le_bytes(c[4..8].try_into().unwrap()),
            flags: u32::from_le_bytes(c[12..16].try_into().unwrap()),
        })
        .collect();
    let mut rest_matches = 0;
    let mut bad_edges = 0;
    for e in &edges {
        let (Some(a), Some(b)) = (particles.get(e.a as usize), particles.get(e.b as usize)) else {
            bad_edges += 1;
            continue;
        };
        let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
        if (d - e.rest).abs() < 1e-3 + 1e-3 * e.rest {
            rest_matches += 1;
        }
    }
    let mut names = Vec::new();
    for c in sys.chunks_exact(4) {
        if let Some(n) = bone_name(u32::from_le_bytes(c.try_into().unwrap())) {
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    Ok(Cloth {
        soft: u32_at(sys, 0x1C) == Some(FAMILY_KEY),
        particles,
        edges,
        rest_matches,
        bad_edges,
        names,
    })
}

/// Particles as dots and constraints as lines. Edges that hold their stored length are teal;
/// the rest (bend constraints and slack) are drawn dimmer.
pub fn cloth_overlay(c: &Cloth) -> LineOverlay {
    let pts: Vec<[f32; 3]> = c.particles.iter().map(|p| [p[0], p[1], p[2]]).collect();
    let mut ov = LineOverlay {
        dot_px: 3.0,
        ..Default::default()
    };
    // each edge gets its own two points so it can carry its own colour
    let n = pts.len() as u32;
    ov.points = pts.clone();
    let dot = if c.soft {
        Color32::from_rgb(255, 150, 205)
    } else {
        Color32::from_rgb(255, 196, 64)
    };
    ov.colors = vec![dot; pts.len()];
    for e in &c.edges {
        if e.a as u32 >= n || e.b as u32 >= n {
            continue;
        }
        let (a, b) = (pts[e.a as usize], pts[e.b as usize]);
        let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
        let col = if (d - e.rest).abs() < 1e-3 + 1e-3 * e.rest {
            Color32::from_rgb(70, 200, 230)
        } else {
            Color32::from_rgb(70, 110, 150)
        };
        ov.segment_colors.push(col);
        ov.segments.push([e.a as u32, e.b as u32]);
    }
    ov.summary = format!("{} particles  ·  {} constraints", c.particles.len(), c.edges.len());
    ov
}

// ---------------------------------------------------------------- head delta

#[derive(Clone, Debug, Default)]
pub struct HeadDelta {
    /// joaat of the base head drawable the deltas apply to.
    pub base_hash: u32,
    /// One offset per base-head vertex, in the deltas' own vertex order (metres).
    pub deltas: Vec<[f32; 3]>,
    pub quant_base: [f32; 3],
    pub quant_step: [f32; 3],
}

impl HeadDelta {
    pub fn lod(&self) -> Option<usize> {
        head_lod(self.deltas.len())
    }
    /// Largest offset length in metres.
    pub fn max_len(&self) -> f32 {
        self.deltas
            .iter()
            .map(|d| (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt())
            .fold(0.0, f32::max)
    }
}

pub fn decode_head_delta(sys: &[u8], gfx: &[u8]) -> Result<HeadDelta, String> {
    if sys.len() != 240 || u64_at(sys, 0x10) != Some(0x5000_0070) || u16_at(sys, 0x80) != Some(0x1601) {
        return Err("not a head delta (system layout)".into());
    }
    let bytes = u32_at(sys, 0x78).unwrap() as usize * 4;
    let start = gfx_ptr(sys, 0x88).unwrap_or(0);
    let g = gfx.get(start..start + bytes).ok_or("delta buffer outside the graphics pages")?;
    if bytes < 0x20 || (bytes - 0x20) % 8 != 0 || u32_at(g, 0) != Some(1) {
        return Err("unexpected delta buffer header".into());
    }
    let quant_base = vec3_at(g, 4).unwrap();
    let quant_step = vec3_at(g, 0x10).unwrap();
    let deltas = g[0x20..]
        .chunks_exact(8)
        .map(|c| {
            let q = u64::from_le_bytes(c.try_into().unwrap());
            // same 24/24/16-bit quantiser as mesh-blob positions
            let x = (((q >> 16) & 0xFFFF) << 8 | (q & 0xFF)) as f32;
            let y = (((q >> 32) & 0xFFFF) << 8 | ((q >> 8) & 0xFF)) as f32;
            let z = (q >> 48) as f32;
            [
                x * quant_step[0] + quant_base[0],
                y * quant_step[1] + quant_base[1],
                z * quant_step[2] + quant_base[2],
            ]
        })
        .collect();
    Ok(HeadDelta {
        base_hash: u32_at(sys, 0x58).unwrap(),
        deltas,
        quant_base,
        quant_step,
    })
}

/// Coverage of a head delta: the fixed 240-byte page and `0x20 + 8 V` buffer bytes, with any
/// page padding after the buffer zero.
pub fn head_delta_coverage(sys: &[u8], gfx: &[u8]) -> Coverage {
    let mut c = Coverage {
        sys_total: sys.len(),
        ..Default::default()
    };
    match decode_head_delta(sys, gfx) {
        Ok(d) => {
            c.sys_typed = sys.len();
            let start = gfx_ptr(sys, 0x88).unwrap_or(0);
            let bytes = 0x20 + d.deltas.len() * 8;
            c.gfx_total = bytes;
            c.gfx_typed = bytes;
            if gfx.get(start + bytes..).is_some_and(|pad| pad.iter().any(|&b| b != 0)) {
                c.problems.push("non-zero padding after the delta buffer".into());
            }
        }
        Err(e) => c.problems.push(e),
    }
    c
}

// ---------------------------------------------------------------- export

/// glTF 2.0 binary container around a JSON document and one buffer.
fn glb(json: &str, mut bin: Vec<u8>) -> Vec<u8> {
    let mut js = json.as_bytes().to_vec();
    while js.len() % 4 != 0 {
        js.push(b' ');
    }
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let total = 12 + 8 + js.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&0x4654_6C67u32.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(js.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    out.extend_from_slice(&js);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x004E_4942u32.to_le_bytes());
    out.extend_from_slice(&bin);
    out
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// RAGE Z-up to glTF Y-up: (x, y, z) -> (x, z, -y).
fn y_up(p: [f32; 3]) -> [f32; 3] {
    [p[0], p[2], -p[1]]
}

fn finite_f32(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

fn finite_vec3(p: [f32; 3]) -> [f32; 3] {
    [finite_f32(p[0]), finite_f32(p[1]), finite_f32(p[2])]
}

fn minmax(pts: &[[f32; 3]]) -> (String, String) {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for p in pts {
        for k in 0..3 {
            let v = finite_f32(p[k]);
            lo[k] = lo[k].min(v);
            hi[k] = hi[k].max(v);
        }
    }
    if pts.is_empty() {
        lo = [0.0; 3];
        hi = [0.0; 3];
    }
    (
        format!("[{},{},{}]", lo[0], lo[1], lo[2]),
        format!("[{},{},{}]", hi[0], hi[1], hi[2]),
    )
}

/// Groom as glTF line strands (port of hair_gltf.py): one LINES primitive, every guide strand a
/// polyline of K points, positions converted to Y-up, and the f16 w lane as `_RADIUS`.
pub fn groom_glb(g: &Groom, name: &str) -> Vec<u8> {
    let pos: Vec<[f32; 3]> = g.points.iter().map(|p| finite_vec3(y_up([p[0], p[1], p[2]]))).collect();
    let k = g.points_per_strand;
    let mut idx: Vec<u32> = Vec::with_capacity(g.strands * k.saturating_sub(1) * 2);
    for s in 0..g.strands {
        for i in 1..k {
            let a = s * k + i - 1;
            let b = s * k + i;
            if b < pos.len() {
                idx.push(a as u32);
                idx.push(b as u32);
            }
        }
    }
    let mut bin = Vec::new();
    pos.iter().flatten().for_each(|f| bin.extend_from_slice(&f.to_le_bytes()));
    let w_off = bin.len();
    g.points.iter().for_each(|p| bin.extend_from_slice(&finite_f32(p[3]).to_le_bytes()));
    let i_off = bin.len();
    idx.iter().for_each(|i| bin.extend_from_slice(&i.to_le_bytes()));
    let (lo, hi) = minmax(&pos);
    let n = pos.len();
    let ni = idx.len();
    let (primitives, buffers, views, accessors) = if n == 0 {
        (String::new(), String::new(), String::new(), String::new())
    } else if ni == 0 {
        (
            r#"{"attributes":{"POSITION":0,"_RADIUS":1},"mode":0}"#.to_string(),
            format!(r#"{{"byteLength":{}}}"#, a4(bin.len())),
            format!(
                r#"{{"buffer":0,"byteOffset":0,"byteLength":{plen},"target":34962}},{{"buffer":0,"byteOffset":{woff},"byteLength":{wlen},"target":34962}}"#,
                plen = w_off,
                woff = w_off,
                wlen = i_off - w_off,
            ),
            format!(
                r#"{{"bufferView":0,"componentType":5126,"count":{n},"type":"VEC3","min":{lo},"max":{hi}}},{{"bufferView":1,"componentType":5126,"count":{n},"type":"SCALAR"}}"#
            ),
        )
    } else {
        (
            r#"{"attributes":{"POSITION":0,"_RADIUS":1},"indices":2,"mode":1}"#.to_string(),
            format!(r#"{{"byteLength":{}}}"#, a4(bin.len())),
            format!(
                r#"{{"buffer":0,"byteOffset":0,"byteLength":{plen},"target":34962}},{{"buffer":0,"byteOffset":{woff},"byteLength":{wlen},"target":34962}},{{"buffer":0,"byteOffset":{ioff},"byteLength":{ilen},"target":34963}}"#,
                plen = w_off,
                woff = w_off,
                wlen = i_off - w_off,
                ioff = i_off,
                ilen = bin.len() - i_off,
            ),
            format!(
                r#"{{"bufferView":0,"componentType":5126,"count":{n},"type":"VEC3","min":{lo},"max":{hi}}},{{"bufferView":1,"componentType":5126,"count":{n},"type":"SCALAR"}},{{"bufferView":2,"componentType":5125,"count":{ni},"type":"SCALAR"}}"#
            ),
        )
    };
    let json = format!(
        concat!(
            r#"{{"asset":{{"version":"2.0","generator":"Percy6Stars"}},"scene":0,"scenes":[{{"nodes":[0]}}],"#,
            r#""nodes":[{{"mesh":0,"name":{name}}}],"#,
            r#""meshes":[{{"name":"guide_strands","primitives":[{primitives}]}}],"#,
            r#""buffers":[{buffers}],"#,
            r#""bufferViews":[{views}],"#,
            r#""accessors":[{accessors}],"#,
            r#""extras":{{"strands":{s},"points_per_strand":{k},"root_triangles":{t}}}}}"#
        ),
        name = json_str(name),
        primitives = primitives,
        buffers = buffers,
        views = views,
        accessors = accessors,
        s = g.strands,
        k = k,
        t = g.root_tris.len(),
    );
    glb(&json, bin)
}

/// Cloth as glTF: constraints as a LINES primitive and particles as a POINTS primitive
/// (w as `_MASS`).
pub fn cloth_glb(c: &Cloth, name: &str) -> Vec<u8> {
    let pos: Vec<[f32; 3]> = c.particles.iter().map(|p| finite_vec3(y_up([p[0], p[1], p[2]]))).collect();
    let n = pos.len();
    let idx: Vec<u32> = c
        .edges
        .iter()
        .filter(|e| (e.a as usize) < n && (e.b as usize) < n)
        .flat_map(|e| [e.a as u32, e.b as u32])
        .collect();
    let mut bin = Vec::new();
    pos.iter().flatten().for_each(|f| bin.extend_from_slice(&f.to_le_bytes()));
    let w_off = bin.len();
    c.particles.iter().for_each(|p| bin.extend_from_slice(&finite_f32(p[3]).to_le_bytes()));
    let i_off = bin.len();
    idx.iter().for_each(|i| bin.extend_from_slice(&i.to_le_bytes()));
    let (lo, hi) = minmax(&pos);
    let ni = idx.len();
    let (primitives, buffers, views, accessors) = if n == 0 {
        (String::new(), String::new(), String::new(), String::new())
    } else if ni == 0 {
        (
            r#"{"attributes":{"POSITION":0,"_MASS":1},"mode":0}"#.to_string(),
            format!(r#"{{"byteLength":{}}}"#, a4(bin.len())),
            format!(
                r#"{{"buffer":0,"byteOffset":0,"byteLength":{plen},"target":34962}},{{"buffer":0,"byteOffset":{woff},"byteLength":{wlen},"target":34962}}"#,
                plen = w_off,
                woff = w_off,
                wlen = i_off - w_off,
            ),
            format!(
                r#"{{"bufferView":0,"componentType":5126,"count":{n},"type":"VEC3","min":{lo},"max":{hi}}},{{"bufferView":1,"componentType":5126,"count":{n},"type":"SCALAR"}}"#
            ),
        )
    } else {
        (
            r#"{"attributes":{"POSITION":0},"indices":2,"mode":1},{"attributes":{"POSITION":0,"_MASS":1},"mode":0}"#.to_string(),
            format!(r#"{{"byteLength":{}}}"#, a4(bin.len())),
            format!(
                r#"{{"buffer":0,"byteOffset":0,"byteLength":{plen},"target":34962}},{{"buffer":0,"byteOffset":{woff},"byteLength":{wlen},"target":34962}},{{"buffer":0,"byteOffset":{ioff},"byteLength":{ilen},"target":34963}}"#,
                plen = w_off,
                woff = w_off,
                wlen = i_off - w_off,
                ioff = i_off,
                ilen = bin.len() - i_off,
            ),
            format!(
                r#"{{"bufferView":0,"componentType":5126,"count":{n},"type":"VEC3","min":{lo},"max":{hi}}},{{"bufferView":1,"componentType":5126,"count":{n},"type":"SCALAR"}},{{"bufferView":2,"componentType":5125,"count":{ni},"type":"SCALAR"}}"#
            ),
        )
    };
    let json = format!(
        concat!(
            r#"{{"asset":{{"version":"2.0","generator":"Percy6Stars"}},"scene":0,"scenes":[{{"nodes":[0]}}],"#,
            r#""nodes":[{{"mesh":0,"name":{name}}}],"#,
            r#""meshes":[{{"name":"cloth","primitives":[{primitives}]}}],"#,
            r#""buffers":[{buffers}],"#,
            r#""bufferViews":[{views}],"#,
            r#""accessors":[{accessors}],"#,
            r#""extras":{{"particles":{n},"constraints":{m},"soft_body":{soft}}}}}"#
        ),
        name = json_str(name),
        primitives = primitives,
        buffers = buffers,
        views = views,
        accessors = accessors,
        n = n,
        m = c.edges.len(),
        soft = c.soft,
    );
    glb(&json, bin)
}

fn safe_file_stem(name: &str) -> String {
    let stem = name.rsplit_once('.').map(|x| x.0).unwrap_or(name);
    stem.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Batch export of one entry into `dir`: grooms and cloth as `.glb` line geometry, head deltas
/// as their raw resource bytes. With `per_archive` (a folder of archives is open) the archive
/// name leads the file name, since resources in two archives can get the same generated name.
/// Returns the written file.
pub fn export_entry(e: &RpfEntry, dir: &Path, per_archive: bool) -> Result<PathBuf, String> {
    let (sys, gfx) = read_resource(&e.source_rpf, e.offset)?;
    let stem = safe_file_stem(&e.name);
    let (file, bytes) = match e.kind {
        "HAIR" => (format!("{stem}.glb"), groom_glb(&decode_groom(&sys, &gfx)?, &stem)),
        "CLOTH" | "SOFT" => (format!("{stem}.glb"), cloth_glb(&decode_cloth(&sys)?, &stem)),
        _ => {
            let mut b = sys;
            b.extend_from_slice(&gfx);
            (e.name.clone(), b)
        }
    };
    let file = if per_archive {
        format!("{}_{file}", safe_file_stem(&e.rpf_name))
    } else {
        file
    };
    let p = dir.join(file);
    std::fs::write(&p, bytes).map_err(|err| err.to_string())?;
    Ok(p)
}

/// What the decoder found, for the copied AI context: a short description of the resource and
/// its facts (counts, bones, sizes), in the same terms as the preview panel.
pub fn context_facts(e: &RpfEntry) -> Result<(&'static str, Vec<String>), String> {
    let (sys, gfx) = read_resource(&e.source_rpf, e.offset)?;
    let names = |hs: &mut dyn Iterator<Item = u32>| {
        let all: Vec<String> = hs.map(hash_label).collect();
        let more = all.len().saturating_sub(8);
        let shown = all[..all.len().min(8)].join(", ");
        if more > 0 { format!("{shown} (+{more} more)") } else { shown }
    };
    match e.kind {
        "HAIR" => {
            let g = decode_groom(&sys, &gfx)?;
            let mut f = vec![
                format!("{} guide strands x {} points", g.strands, g.points_per_strand),
                format!("{} root triangles", g.root_tris.len()),
            ];
            if !g.bones.is_empty() {
                f.push(format!("follows bone(s) {}", names(&mut g.bones.iter().copied())));
            }
            if let Some((lo, hi)) = g.bounds() {
                f.push(format!(
                    "bounds {:.3}..{:.3} x {:.3}..{:.3} x {:.3}..{:.3} m (Z up)",
                    lo[0], hi[0], lo[1], hi[1], lo[2], hi[2]
                ));
            }
            f.push(format!("graphics buffer {} bytes", g.buffer_bytes));
            Ok(("strand hair groom", f))
        }
        "CLOTH" | "SOFT" => {
            let c = decode_cloth(&sys)?;
            let mut f = vec![
                format!("{} particles, {} edges", c.particles.len(), c.edges.len()),
                format!("{} of {} edges at their rest length in the stored pose", c.rest_matches, c.edges.len()),
            ];
            if !c.names.is_empty() {
                f.push(format!("names {}", c.names.join(", ")));
            }
            Ok((if c.soft { "verlet soft body" } else { "verlet cloth" }, f))
        }
        _ => {
            let d = decode_head_delta(&sys, &gfx)?;
            let lod = d.lod().map_or_else(|| "unknown LOD".to_string(), |l| format!("LOD {l}"));
            Ok((
                "head identity delta",
                vec![
                    format!("{} vertex offsets ({lod})", d.deltas.len()),
                    format!("largest offset {:.2} mm", d.max_len() * 1000.0),
                    format!("base head {}", hash_label(d.base_hash)),
                ],
            ))
        }
    }
}

/// Decode used by the format audit (`src/audit_tests.rs`).
pub fn audit_decode(e: &RpfEntry) -> Result<String, String> {
    let (sys, gfx) = read_resource(&e.source_rpf, e.offset)?;
    match e.kind {
        "HAIR" => decode_groom(&sys, &gfx).map(|g| format!("{} strands x {}", g.strands, g.points_per_strand)),
        "CLOTH" | "SOFT" => decode_cloth(&sys).map(|c| format!("{} particles, {} edges", c.particles.len(), c.edges.len())),
        _ => decode_head_delta(&sys, &gfx).map(|d| format!("{} vertices", d.deltas.len())),
    }
}

// ---------------------------------------------------------------- head delta application

/// Vertex positions and UVs of the head geometry with exactly `v` vertices, if the decoded head
/// has one (heads keep the skin as one geometry per LOD).
fn geometry_with_verts(m: &Mesh3D, v: usize) -> Option<std::ops::Range<usize>> {
    let mut ranges: HashMap<u16, (usize, usize)> = HashMap::new();
    for (t, &g) in m.tris.iter().zip(&m.tri_geom) {
        let r = ranges.entry(g).or_insert((usize::MAX, 0));
        for &i in t {
            r.0 = r.0.min(i as usize);
            r.1 = r.1.max(i as usize);
        }
    }
    ranges.values().find(|r| r.1 + 1 - r.0 == v).map(|r| r.0..r.1 + 1)
}

/// Pairs every vertex of `base` with the vertex of `head` that has the same UV and the nearest
/// position (UVs match exactly between heads of one topology; positions differ by the delta).
fn pair_by_uv(base: (&[[f32; 3]], &[[f32; 2]]), head: (&[[f32; 3]], &[[f32; 2]])) -> Option<Vec<usize>> {
    let key = |uv: [f32; 2]| ((uv[0] * 8192.0).round() as i32, (uv[1] * 8192.0).round() as i32);
    let mut by_uv: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (i, &uv) in head.1.iter().enumerate() {
        by_uv.entry(key(uv)).or_default().push(i);
    }
    base.0
        .iter()
        .zip(base.1)
        .map(|(p, &uv)| {
            by_uv.get(&key(uv))?.iter().copied().min_by(|&a, &b| {
                let d = |j: usize| {
                    let q = head.0[j];
                    (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2) + (q[2] - p[2]).powi(2)
                };
                d(a).total_cmp(&d(b))
            })
        })
        .collect()
}

fn sorted_lengths(d: &[[f32; 3]]) -> Vec<f32> {
    let mut l: Vec<f32> = d.iter().map(|v| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()).collect();
    l.sort_by(f32::total_cmp);
    l
}

/// Decodes the files of one head (one per LOD) until a geometry with `v` vertices turns up,
/// trying the LOD slot the vertex count implies first.
fn find_lod_with_verts(files: &[HeadChoice], v: usize) -> Option<Mesh3D> {
    let want = head_lod(v).unwrap_or(0);
    let lods: Vec<usize> = std::iter::once(want).chain((0..model3d::MAX_LODS).filter(|&l| l != want)).collect();
    // the largest file first: it holds the full detail levels
    let mut files = files.to_vec();
    files.sort_by_key(|f| std::cmp::Reverse(f.size));
    files.iter().find_map(|f| {
        lods.iter().find_map(|&lod| {
            let m = model3d::decode_rage9_model_from_rpf_lod(&f.src, f.offset, &f.name, lod).ok()?;
            (m.lod == lod && geometry_with_verts(&m, v).is_some()).then_some(m)
        })
    })
}

/// Result of applying a head delta to its base head.
pub struct AppliedDelta {
    /// Base head mesh with the delta's geometry moved by the offsets.
    pub mesh: Mesh3D,
    /// The head drawable whose shape the delta reproduces, and the worst vertex error (metres).
    pub matched_head: String,
    pub max_error: f32,
}

/// Puts each delta on the vertex whose offset it equals: a one-to-one assignment where every
/// delta lands within `tol` metres of its offset. Returns the vertex for each delta and the worst
/// error, or `None` if any delta has no free offset that close (a grid on the offsets keeps this
/// linear; the cell is `tol`, so the 3×3×3 neighbourhood covers it).
fn assign_deltas(deltas: &[[f32; 3]], offs: &[[f32; 3]], tol: f32) -> Option<(Vec<usize>, f32)> {
    if deltas.len() != offs.len() {
        return None;
    }
    let ck = |d: [f32; 3]| ((d[0] / tol).round() as i64, (d[1] / tol).round() as i64, (d[2] / tol).round() as i64);
    let mut grid: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
    for (j, &o) in offs.iter().enumerate() {
        grid.entry(ck(o)).or_default().push(j);
    }
    let mut used = vec![false; offs.len()];
    let mut slot = Vec::with_capacity(deltas.len());
    let mut max_e2 = 0.0f32;
    for d in deltas {
        let c = ck(*d);
        let mut best: Option<(f32, usize)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for &j in grid.get(&(c.0 + dx, c.1 + dy, c.2 + dz)).into_iter().flatten() {
                        if used[j] {
                            continue;
                        }
                        let o = offs[j];
                        let e = (o[0] - d[0]).powi(2) + (o[1] - d[1]).powi(2) + (o[2] - d[2]).powi(2);
                        if best.is_none_or(|b| e < b.0) {
                            best = Some((e, j));
                        }
                    }
                }
            }
        }
        let (e, j) = best.filter(|b| b.0 <= tol * tol)?;
        used[j] = true;
        max_e2 = max_e2.max(e);
        slot.push(j);
    }
    Some((slot, max_e2.sqrt()))
}

/// Applies `delta` to the `base` head. The deltas are stored in a vertex order of their own,
/// so the order is recovered from a head drawable of the same class: for each candidate head,
/// the head minus base offsets (vertices paired by UV and position) must equal the deltas as a
/// set, every delta matching its own offset within 50 µm. The sorted offset lengths are a cheap
/// prefilter; the best full match fixes the order, and the base plus the re-ordered deltas is
/// the result.
pub fn apply_head_delta(
    delta: &HeadDelta,
    base: &Mesh3D,
    candidates: &mut dyn Iterator<Item = (String, Mesh3D)>,
    progress: &dyn Fn(&str),
) -> Result<AppliedDelta, String> {
    const TOL: f32 = 5e-5;
    let v = delta.deltas.len();
    let br = geometry_with_verts(base, v).ok_or_else(|| format!("base head has no {v}-vertex geometry"))?;
    let (bp, buv) = (&base.verts[br.clone()], &base.uvs[br.clone()]);
    let want = sorted_lengths(&delta.deltas);
    let mut best: Option<(f32, String, Vec<usize>)> = None;
    for (name, head) in candidates {
        progress(&name);
        let Some(hr) = geometry_with_verts(&head, v) else { continue };
        let (hp, huv) = (&head.verts[hr.clone()], &head.uvs[hr.clone()]);
        let Some(pair) = pair_by_uv((bp, buv), (hp, huv)) else { continue };
        let offs: Vec<[f32; 3]> = pair
            .iter()
            .zip(bp)
            .map(|(&j, b)| [hp[j][0] - b[0], hp[j][1] - b[1], hp[j][2] - b[2]])
            .collect();
        let got = sorted_lengths(&offs);
        let sig_err = want.iter().zip(&got).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        if sig_err >= TOL {
            continue;
        }
        // equal lengths aren't enough (a mirrored head has them too): every vector must match
        let Some((slot, err)) = assign_deltas(&delta.deltas, &offs, TOL) else { continue };
        if best.as_ref().is_none_or(|b| err < b.0) {
            best = Some((err, name, slot));
        }
    }
    let Some((max_error, name, slot)) = best else {
        return Err(
            "No head drawable of this class matches the delta (some heads aren't in the archive), so its vertex order can't be recovered."
                .into(),
        );
    };
    let mut applied = bp.to_vec();
    for (d, &j) in delta.deltas.iter().zip(&slot) {
        applied[j] = [bp[j][0] + d[0], bp[j][1] + d[1], bp[j][2] + d[2]];
    }
    let mut mesh = base.clone();
    mesh.verts[br.clone()].copy_from_slice(&applied);
    mesh.normals = mesh
        .tris
        .iter()
        .map(|t| {
            let a = mesh.verts[t[0] as usize];
            let b = mesh.verts[t[1] as usize];
            let c = mesh.verts[t[2] as usize];
            let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let nx = ab[1] * ac[2] - ab[2] * ac[1];
            let ny = ab[2] * ac[0] - ab[0] * ac[2];
            let nz = ab[0] * ac[1] - ab[1] * ac[0];
            let len = (nx * nx + ny * ny + nz * nz).sqrt();
            if len > 1e-12 {
                [nx / len, ny / len, nz / len]
            } else {
                [0.0, 0.0, 1.0]
            }
        })
        .collect();
    mesh.name = format!("{} + delta", base.name);
    mesh.invalidate_normals();
    Ok(AppliedDelta {
        mesh,
        matched_head: name,
        max_error,
    })
}

// ---------------------------------------------------------------- preview panel

/// A head drawable the user can put a groom on.
#[derive(Clone)]
struct HeadChoice {
    name: String,
    src: PathBuf,
    offset: u64,
    size: u64,
}

#[derive(Default)]
struct Job {
    busy: bool,
    status: String,
    mesh: Option<Mesh3D>,
    note: String,
    error: String,
}

enum Decoded {
    Groom(Groom, Coverage),
    Cloth(Cloth),
    Delta(HeadDelta, Coverage),
}

/// Preview of one ped physics entry: metadata, a 3D view, head overlay and export.
pub struct PedPhysView {
    entry: RpfEntry,
    decoded: Result<Decoded, String>,
    mesh: Option<Mesh3D>,
    show_3d: bool,
    heads: Vec<HeadChoice>,
    all_head_files: Vec<HeadChoice>,
    head_sel: Option<usize>,
    head_filter: String,
    /// Base head name for a delta, resolved from its joaat.
    base_name: Option<String>,
    head_names: HashMap<u32, String>,
    /// Same-archive head delta entries in archive order (for the delta's class and tuple).
    deltas_around: Vec<RpfEntry>,
    job: Arc<Mutex<Job>>,
    on_head: Option<String>,
    note: String,
    pub status: String,
}

/// Heads and grooms face -Y: turn the camera to a three-quarter front view.
fn face_front(m: &mut Mesh3D) {
    m.yaw = 0.55;
    m.pitch = 0.18;
    m.dirty = true;
}

/// Name the app shows for a head drawable entry.
fn display_head(name: &str) -> &str {
    name.strip_suffix(".#dr").unwrap_or(name)
}

/// The first base head drawable to put a groom on.
const DEFAULT_HEAD: &str = "head_mr1_base_000_m_full.#dr";

impl PedPhysView {
    /// Decodes `e` and gathers what its preview needs from the rest of the scan.
    pub fn open(e: &RpfEntry, entries: &[RpfEntry]) -> Self {
        let decoded = read_resource(&e.source_rpf, e.offset).and_then(|(sys, gfx)| match e.kind {
            "HAIR" => decode_groom(&sys, &gfx).map(|g| Decoded::Groom(g, groom_coverage(&sys, &gfx))),
            "CLOTH" | "SOFT" => decode_cloth(&sys).map(Decoded::Cloth),
            _ => decode_head_delta(&sys, &gfx).map(|d| Decoded::Delta(d, head_delta_coverage(&sys, &gfx))),
        });
        let mut v = PedPhysView {
            entry: e.clone(),
            decoded,
            mesh: None,
            show_3d: true,
            heads: Vec::new(),
            all_head_files: Vec::new(),
            head_sel: None,
            head_filter: String::new(),
            base_name: None,
            head_names: HashMap::new(),
            deltas_around: Vec::new(),
            job: Arc::new(Mutex::new(Job::default())),
            on_head: None,
            note: String::new(),
            status: String::new(),
        };
        // head drawables of the same archive, one entry per name (the largest file is the full LOD)
        let mut best: HashMap<&str, &RpfEntry> = HashMap::new();
        for h in entries.iter().filter(|h| h.kind == "MODEL" && h.source_rpf == e.source_rpf) {
            if head_stem(&h.name).is_some() && !h.name.contains("_eyes") {
                v.all_head_files.push(HeadChoice {
                    name: h.name.clone(),
                    src: h.source_rpf.clone(),
                    offset: h.offset,
                    size: h.raw_size,
                });
                let slot = best.entry(h.name.as_str()).or_insert(h);
                if h.raw_size > slot.raw_size {
                    *slot = h;
                }
            }
        }
        let mut heads: Vec<HeadChoice> = best
            .into_values()
            .map(|h| HeadChoice {
                name: h.name.clone(),
                src: h.source_rpf.clone(),
                offset: h.offset,
                size: h.raw_size,
            })
            .collect();
        heads.sort_by(|a, b| (!a.name.contains("_base_"), &a.name).cmp(&(!b.name.contains("_base_"), &b.name)));
        v.head_sel = heads
            .iter()
            .position(|h| h.name == DEFAULT_HEAD)
            .or_else(|| (!heads.is_empty()).then_some(0));
        v.heads = heads;
        match &v.decoded {
            Ok(Decoded::Groom(g, _)) => {
                let mut m = Mesh3D::from_lines(e.name.clone(), groom_overlay(g));
                face_front(&mut m);
                v.mesh = Some(m);
            }
            Ok(Decoded::Cloth(c)) => v.mesh = Some(Mesh3D::from_lines(e.name.clone(), cloth_overlay(c))),
            Ok(Decoded::Delta(d, _)) => {
                v.head_names = head_hash_names(entries);
                v.base_name = v.head_names.get(&d.base_hash).cloned();
                let mut around: Vec<RpfEntry> = entries
                    .iter()
                    .filter(|x| x.kind == "HDELTA" && x.source_rpf == e.source_rpf)
                    .cloned()
                    .collect();
                around.sort_by_key(|x| x.offset);
                v.deltas_around = around;
                v.show_3d = false;
            }
            Err(_) => v.show_3d = false,
        }
        v
    }

    fn poll_job(&mut self) {
        let mut j = self.job.lock().unwrap();
        if let Some(mut m) = j.mesh.take() {
            if let Some(old) = &self.mesh {
                m.yaw = old.yaw;
                m.pitch = old.pitch;
            }
            self.mesh = Some(m);
            self.on_head = Some(std::mem::take(&mut j.status));
            self.note = std::mem::take(&mut j.note);
            self.show_3d = true;
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll_job();
        let kind = self.entry.kind;
        ui.horizontal(|ui| {
            ui.label(RichText::new(kind_label(kind)).strong().size(15.0).color(kind_color(kind)));
            ui.label(RichText::new(&self.entry.name).size(13.0).color(Color32::from_rgb(200, 195, 220)));
        });
        ui.label(RichText::new(kind_blurb(kind)).size(12.0).color(Color32::from_rgb(160, 168, 190)));
        ui.add_space(4.0);
        if let Err(e) = &self.decoded {
            ui.label(RichText::new(format!("Couldn't decode this resource: {e}")).color(Color32::from_rgb(255, 140, 120)));
            return;
        }
        let has_3d = self.mesh.is_some();
        ui.horizontal(|ui| {
            if ui.selectable_label(!self.show_3d, "Info").clicked() {
                self.show_3d = false;
            }
            if has_3d && ui.selectable_label(self.show_3d, "3D View").clicked() {
                self.show_3d = true;
            }
            ui.separator();
            self.toolbar(ui);
        });
        let busy = {
            let j = self.job.lock().unwrap();
            if !j.error.is_empty() {
                ui.label(RichText::new(&j.error).color(Color32::from_rgb(255, 140, 120)));
            }
            j.busy.then(|| j.status.clone())
        };
        if let Some(st) = busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(st).color(Color32::from_rgb(252, 175, 23)));
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        if !self.note.is_empty() {
            ui.label(RichText::new(&self.note).size(11.5).color(Color32::from_rgb(150, 200, 160)));
        }
        if !self.status.is_empty() {
            ui.label(RichText::new(&self.status).size(11.5).color(Color32::from_rgb(150, 160, 185)));
        }
        ui.separator();
        if self.show_3d {
            if let Some(m) = self.mesh.as_mut() {
                model3d::render_mesh3d_viewport(ui, m);
            }
        } else {
            egui::ScrollArea::vertical().show(ui, |ui| self.info(ui));
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let busy = self.job.lock().unwrap().busy;
        match &self.decoded {
            Ok(Decoded::Groom(g, _)) => {
                let g = g.clone();
                ui.label(RichText::new("Show on head:").size(12.0));
                let cur = self
                    .head_sel
                    .and_then(|i| self.heads.get(i))
                    .map(|h| display_head(&h.name).to_string())
                    .unwrap_or_else(|| "no heads in this archive".into());
                egui::ComboBox::from_id_salt("pedphys_head")
                    .selected_text(cur)
                    .width(250.0)
                    .show_ui(ui, |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.head_filter).hint_text("filter heads"));
                        let f = self.head_filter.to_lowercase();
                        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                            for (i, h) in self
                                .heads
                                .iter()
                                .enumerate()
                                .filter(|(_, h)| f.is_empty() || h.name.to_lowercase().contains(&f))
                                .take(400)
                            {
                                ui.selectable_value(&mut self.head_sel, Some(i), display_head(&h.name));
                            }
                        });
                    });
                if ui
                    .add_enabled(!busy && self.head_sel.is_some(), egui::Button::new("Show"))
                    .clicked()
                {
                    self.show_on_head(&g);
                }
                if self.on_head.is_some() && ui.button("Groom only").clicked() {
                    let mut m = Mesh3D::from_lines(self.entry.name.clone(), groom_overlay(&g));
                    if let Some(old) = &self.mesh {
                        m.yaw = old.yaw;
                        m.pitch = old.pitch;
                    }
                    self.mesh = Some(m);
                    self.on_head = None;
                    self.note.clear();
                }
                ui.label(RichText::new("ⓘ").color(Color32::from_rgb(150, 160, 185)))
                    .on_hover_text("Pairing is manual: the game's link between a head and its hair isn't decoded yet. Heads and grooms share model space, so the groom sits where the game puts it.");
                if ui
                    .button("Export .glb")
                    .on_hover_text("Guide strands as glTF line strands (Y-up), thickness as _RADIUS")
                    .clicked()
                {
                    self.save(groom_glb(&g, &safe_file_stem(&self.entry.name)), "glb");
                }
            }
            Ok(Decoded::Cloth(c)) => {
                let c = c.clone();
                if ui
                    .button("Export .glb")
                    .on_hover_text("Constraints as lines and particles as points (Y-up)")
                    .clicked()
                {
                    self.save(cloth_glb(&c, &safe_file_stem(&self.entry.name)), "glb");
                }
            }
            Ok(Decoded::Delta(d, _)) => {
                let d = d.clone();
                let label = match &self.base_name {
                    Some(b) => format!("Apply to {b}"),
                    None => "Apply to base head".into(),
                };
                if ui.add_enabled(!busy, egui::Button::new(label)).on_hover_text("Moves the base head's vertices by these offsets. The delta's vertex order is recovered from a head drawable of the same class.").clicked() {
                    self.apply_delta(&d);
                }
            }
            Err(_) => {}
        }
    }

    fn save(&mut self, bytes: Vec<u8>, ext: &str) {
        let name = format!("{}.{ext}", safe_file_stem(&self.entry.name));
        if let Some(p) = rfd::FileDialog::new().set_file_name(&name).save_file() {
            self.status = match std::fs::write(&p, bytes) {
                Ok(()) => format!("Saved {}", p.display()),
                Err(e) => format!("Save failed: {e}"),
            };
        }
    }

    fn show_on_head(&mut self, g: &Groom) {
        let Some(h) = self.head_sel.and_then(|i| self.heads.get(i)).cloned() else {
            return;
        };
        let ov = groom_overlay(g);
        let job = Arc::clone(&self.job);
        {
            let mut j = job.lock().unwrap();
            j.busy = true;
            j.error.clear();
            j.status = format!("Loading {}...", display_head(&h.name));
        }
        // the scan lists one file per LOD under the same name: show the most detailed one
        let files = self.base_files(&h.name);
        std::thread::spawn(move || {
            let r = files
                .iter()
                .filter_map(|f| model3d::decode_rage9_model_from_rpf(&f.src, f.offset, &f.name).ok())
                .max_by_key(|m| m.verts.len())
                .ok_or_else(|| "no decodable file".to_string());
            let mut j = job.lock().unwrap();
            j.busy = false;
            match r {
                Ok(mut m) => {
                    m.overlay = Some(ov);
                    m.frame_all();
                    j.status = h.name.clone();
                    j.note = format!(
                        "Groom on {}. Pairing is manual: the game's link isn't decoded yet.",
                        display_head(&h.name)
                    );
                    j.mesh = Some(m);
                }
                Err(e) => j.error = format!("Couldn't load {}: {e}", display_head(&h.name)),
            }
        });
    }

    /// Base head drawable for a delta. LOD1 base names aren't known, so an unresolved hash
    /// takes the base of another delta in the same head tuple: the deltas of one head sit
    /// together and the tuple ends with its 599-vertex LOD3 delta.
    fn delta_base(&self, d: &HeadDelta) -> Option<String> {
        let strip = |n: &str| n.trim_end_matches("_lod2").trim_end_matches("_lod3").to_string();
        if let Some(n) = self.head_names.get(&d.base_hash) {
            return Some(strip(n));
        }
        let resolve = |x: &RpfEntry| -> Option<String> {
            let stem = x.name.strip_suffix(".hdelta")?;
            let hex = stem.rsplit('_').next()?;
            if let Some(n) = u32::from_str_radix(hex, 16)
                .ok()
                .filter(|_| hex.len() == 8)
                .and_then(|h| self.head_names.get(&h))
            {
                return Some(strip(n));
            }
            // already named by the scan: the name ends in the base head
            stem.find("head_").map(|at| strip(&stem[at..]))
        };
        let i = self.deltas_around.iter().position(|x| x.offset == self.entry.offset)?;
        // forward to the end of this tuple
        for x in &self.deltas_around[i + 1..] {
            if let Some(n) = resolve(x) {
                return Some(n);
            }
            if x.tracks == 599 {
                break;
            }
        }
        // back to the end of the previous tuple
        for x in self.deltas_around[..i].iter().rev() {
            if x.tracks == 599 {
                break;
            }
            if let Some(n) = resolve(x) {
                return Some(n);
            }
        }
        None
    }

    fn apply_delta(&mut self, d: &HeadDelta) {
        let Some(base_stem) = self.delta_base(d) else {
            self.job.lock().unwrap().error = "The base head isn't in this archive (or its name isn't known).".into();
            return;
        };
        // class prefix: head_<class>_
        let class: String = base_stem.splitn(3, '_').take(2).collect::<Vec<_>>().join("_") + "_";
        let base_name = format!("{base_stem}.#dr");
        // candidate heads of the class (not eye caps or lashes), each with every file the scan
        // lists under its name (one per LOD)
        let mut by_name: std::collections::BTreeMap<&str, Vec<HeadChoice>> = std::collections::BTreeMap::new();
        for h in self
            .all_head_files
            .iter()
            .filter(|h| h.name.starts_with(&class) && !["_base_", "eyecap", "eyelash"].iter().any(|x| h.name.contains(x)))
        {
            by_name.entry(h.name.as_str()).or_default().push(h.clone());
        }
        let cands: Vec<Vec<HeadChoice>> = by_name.into_values().collect();
        let all_bases = self.base_files(&base_name);
        let d = d.clone();
        let job = Arc::clone(&self.job);
        {
            let mut j = job.lock().unwrap();
            j.busy = true;
            j.error.clear();
            j.status = format!("Loading {base_stem}...");
        }
        let v = d.deltas.len();
        std::thread::spawn(move || {
            let set = |s: String| job.lock().unwrap().status = s;
            let base = find_lod_with_verts(&all_bases, v);
            let Some(base) = base else {
                let mut j = job.lock().unwrap();
                j.busy = false;
                j.error = format!("No {v}-vertex LOD found in {base_stem}.");
                return;
            };
            let mut it = cands
                .iter()
                .filter_map(|files| Some((files.first()?.name.clone(), find_lod_with_verts(files, v)?)));
            let r = apply_head_delta(&d, &base, &mut it, &|n| {
                set(format!("Matching vertex order against {}...", display_head(n)))
            });
            let mut j = job.lock().unwrap();
            j.busy = false;
            match r {
                Ok(a) => {
                    j.status = base_stem.clone();
                    j.note = format!(
                        "{} + this delta. It reproduces {} (largest vertex error {:.1} µm), so that is the head it belongs to.",
                        base_stem,
                        display_head(&a.matched_head),
                        a.max_error * 1e6
                    );
                    let mut m = a.mesh;
                    face_front(&mut m);
                    j.mesh = Some(m);
                }
                Err(e) => j.error = e,
            }
        });
    }

    /// Every file named `base_name` (the scan lists one file per LOD under the same name).
    fn base_files(&self, base_name: &str) -> Vec<HeadChoice> {
        self.all_head_files.iter().filter(|h| h.name == base_name).cloned().collect()
    }

    fn info(&self, ui: &mut egui::Ui) {
        let row = |ui: &mut egui::Ui, k: &str, v: String| {
            ui.label(RichText::new(k).color(Color32::from_rgb(150, 160, 185)));
            ui.label(RichText::new(v).color(Color32::from_rgb(230, 228, 240)));
            ui.end_row();
        };
        egui::Grid::new("pedphys_info")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| match &self.decoded {
                Ok(Decoded::Groom(g, cov)) => {
                    row(ui, "Guide strands", g.strands.to_string());
                    row(ui, "Points per strand", g.points_per_strand.to_string());
                    row(ui, "Total points", g.points.len().to_string());
                    row(ui, "Root triangles", g.root_tris.len().to_string());
                    if !g.lengths.is_empty() {
                        let mean = g.lengths.iter().sum::<f32>() / g.lengths.len() as f32;
                        let max = g.lengths.iter().copied().fold(0.0, f32::max);
                        row(
                            ui,
                            "Strand length",
                            format!("{:.1} cm mean, {:.1} cm longest", mean * 100.0, max * 100.0),
                        );
                    }
                    if let Some((lo, hi)) = g.bounds() {
                        row(
                            ui,
                            "Size",
                            format!(
                                "{:.1} x {:.1} x {:.1} cm",
                                (hi[0] - lo[0]) * 100.0,
                                (hi[1] - lo[1]) * 100.0,
                                (hi[2] - lo[2]) * 100.0
                            ),
                        );
                    }
                    let r = g.root_radius + 2e-3;
                let (lo, hi) = g.root_box;
                let inside = (0..g.strands)
                    .filter(|&s| (0..3).all(|k| (lo[k] - r..=hi[k] + r).contains(&g.point(s, 0)[k])))
                    .count();
                row(ui, "Roots inside stored root box", format!("{inside} of {}", g.strands));
                row(ui, "Graphics buffer", format!("{} bytes", g.buffer_bytes));
                row(ui, "Flags", format!("0x{:08x}", g.flags));
                    row(ui, "Bytes decoded", coverage_text(cov));
                }
                Ok(Decoded::Cloth(c)) => {
                    row(ui, "Variant", if c.soft { "Soft body".into() } else { "Cloth".into() });
                    row(ui, "Particles", c.particles.len().to_string());
                    row(ui, "Constraints", c.edges.len().to_string());
                    if !c.edges.is_empty() {
                        row(
                            ui,
                            "Hold stored length",
                            format!(
                                "{} of {} ({:.0}%)",
                                c.rest_matches,
                                c.edges.len(),
                                100.0 * c.rest_matches as f32 / c.edges.len() as f32
                            ),
                        );
                    }
                    if c.bad_edges > 0 {
                        row(ui, "Edges out of range", c.bad_edges.to_string());
                    }
                    let pts: Vec<[f32; 3]> = c.particles.iter().map(|p| [p[0], p[1], p[2]]).collect();
                    if let Some((lo, hi)) = (model3d::LineOverlay {
                        points: pts,
                        ..Default::default()
                    })
                    .bounds()
                    {
                        row(
                            ui,
                            "Size",
                            format!("{:.2} x {:.2} x {:.2} m", hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]),
                        );
                    }
                }
                Ok(Decoded::Delta(d, cov)) => {
                    row(ui, "Vertices", d.deltas.len().to_string());
                    row(
                        ui,
                        "Head LOD",
                        d.lod().map(|l| format!("LOD{l}")).unwrap_or_else(|| "unknown".into()),
                    );
                    row(
                        ui,
                        "Base head",
                        self.base_name
                            .clone()
                            .unwrap_or_else(|| format!("0x{:08x} (name not known)", d.base_hash)),
                    );
                    row(ui, "Largest offset", format!("{:.2} mm", d.max_len() * 1000.0));
                row(
                    ui,
                    "Quantisation",
                    format!("step {:.2} µm, from {:.1} mm", d.quant_step[0] * 1e6, d.quant_base[0] * 1e3),
                );
                    row(ui, "Bytes decoded", coverage_text(cov));
                }
                Err(_) => {}
            });
        ui.add_space(8.0);
        let names: Vec<String> = match &self.decoded {
            Ok(Decoded::Groom(g, _)) => g.bones.iter().map(|&h| hash_label(h)).collect(),
            Ok(Decoded::Cloth(c)) => c.names.iter().map(|s| s.to_string()).collect(),
            _ => Vec::new(),
        };
        if !names.is_empty() {
            let title = match self.entry.kind {
                "HAIR" => "Bones the groom follows",
                _ => "Bone and profile names in this resource",
            };
            ui.label(RichText::new(title).strong().color(Color32::from_rgb(200, 195, 220)));
            ui.horizontal_wrapped(|ui| {
                for n in names {
                    egui::Frame::new()
                        .fill(Color32::from_rgb(34, 30, 52))
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::symmetric(6, 2))
                        .show(ui, |ui| {
                            ui.label(RichText::new(n).monospace().size(11.5).color(Color32::from_rgb(220, 215, 240)));
                        });
                }
            });
        }
        ui.add_space(8.0);
        let foot = match self.entry.kind {
            "HAIR" => "Positions are model space, Z-up metres, the same frame as the ped heads. Which head a groom belongs to isn't stored anywhere we can read yet.",
            "HDELTA" => "head = base head + delta, per vertex. The offsets are stored in a vertex order of their own, so applying them pairs vertices by UV and position.",
            _ => "Particles are model space, Z-up metres. Teal lines keep their stored rest length; dim ones are bend constraints or slack.",
        };
        ui.label(RichText::new(foot).size(11.5).color(Color32::from_rgb(140, 148, 170)));
    }
}

fn coverage_text(c: &Coverage) -> String {
    let typed = c.sys_typed + c.gfx_typed;
    let total = c.sys_total + c.gfx_total;
    let mut s = format!("{:.1}% of {} bytes", 100.0 * typed as f64 / total.max(1) as f64, typed.max(total));
    if c.complete() {
        s.push_str(", every byte accounted for");
    }
    if !c.problems.is_empty() {
        s.push_str(&format!(" ({})", c.problems[0]));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(b: &mut Vec<u8>, o: usize, v: &[u8]) {
        if b.len() < o + v.len() {
            b.resize(o + v.len(), 0);
        }
        b[o..o + v.len()].copy_from_slice(v);
    }
    fn f16(x: f32) -> u16 {
        // test values are exact in half precision
        let b = x.to_bits();
        if x == 0.0 {
            return 0;
        }
        let sign = ((b >> 16) & 0x8000) as u16;
        let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
        sign | ((exp as u16) << 10) | ((b >> 13) & 0x3ff) as u16
    }

    /// A made-up groom: S strands of K points hanging down from a row of roots.
    pub(super) fn synthetic_groom(s: usize, k: usize) -> (Vec<u8>, Vec<u8>) {
        let t = 1usize;
        let mut sys = vec![0u8; 0x140];
        put(&mut sys, 0x08, &0x5000_01C0u64.to_le_bytes());
        sys[0x1A] = 3;
        put(&mut sys, 0x1C, &FAMILY_KEY.to_le_bytes());
        put(&mut sys, 0x20, &0x81u32.to_le_bytes());
        for (o, v) in [(0x30, [-0.1f32, -0.01, 1.59, -0.01]), (0x40, [0.1, 0.01, 1.61, 0.01])] {
            for (i, f) in v.iter().enumerate() {
                put(&mut sys, o + i * 4, &f.to_le_bytes());
            }
        }
        put(&mut sys, 0x70, &0x5000_0140u64.to_le_bytes());
        put(&mut sys, 0x98, &3u32.to_le_bytes());
        put(&mut sys, 0xA0, &0x5000_01A0u64.to_le_bytes());
        put(&mut sys, 0xA8, &1u32.to_le_bytes());
        put(&mut sys, 0xB0, &0x5000_01B0u64.to_le_bytes());
        put(&mut sys, 0xDC, &(t as u16).to_le_bytes());
        put(&mut sys, 0xDE, &(s as u16).to_le_bytes());
        // graphics buffer: header, B5 points, B4 root triangles, B0 lengths
        let mut g = vec![0u8; 0xC0];
        let b5 = 0xC0;
        let b4 = b5 + s * k * 8;
        let b0 = a4(b4 + t * 6);
        let end = a4(b0 + s * 2);
        put(&mut g, 0x30 + 5 * 4, &(b5 as u32).to_le_bytes());
        put(&mut g, 0x30 + 4 * 4, &(b4 as u32).to_le_bytes());
        put(&mut g, 0x30, &(b0 as u32).to_le_bytes());
        put(&mut g, 0x58, &(s as u16).to_le_bytes());
        put(&mut g, 0x5A, &(t as u16).to_le_bytes());
        put(&mut g, 0x5C, &(k as u32).to_le_bytes());
        for si in 0..s {
            for ki in 0..k {
                let p = [si as f32 * 0.0625, 0.0, 1.5 - ki as f32 * 0.03125, if ki == 0 { 0.5 } else { 0.25 }];
                for (c, v) in p.iter().enumerate() {
                    put(&mut g, b5 + (si * k + ki) * 8 + c * 2, &f16(*v).to_le_bytes());
                }
            }
            put(&mut g, b0 + si * 2, &f16(0.03125 * (k - 1) as f32).to_le_bytes());
        }
        for (i, v) in [0u16, 1, 2].iter().enumerate() {
            put(&mut g, b4 + i * 2, &v.to_le_bytes());
        }
        g.resize(end, 0);
        // descriptor (size in dwords, flags, graphics pointer, aux block), bones, part, page map
        put(&mut sys, 0x148, &((end / 4) as u32).to_le_bytes());
        put(&mut sys, 0x150, &0x1380u32.to_le_bytes());
        put(&mut sys, 0x158, &0x6000_0000u64.to_le_bytes());
        put(&mut sys, 0x168, &0x5000_0180u64.to_le_bytes());
        for (i, n) in ["BM_Head", "BM_Chin", "EarRig_L_Top"].iter().enumerate() {
            put(&mut sys, 0x1A0 + i * 4, &joaat(n).to_le_bytes());
        }
        put(&mut sys, 0x1B0, &[1u8; 12]);
        put(&mut sys, 0x1C0 + 8, &[1, 1]);
        sys.resize(0x1E0, 0);
        // page padding after the buffer, as the archive stores it
        g.resize(end + 64, 0);
        (sys, g)
    }

    fn synthetic_cloth(soft: bool) -> Vec<u8> {
        let mut sys = vec![0u8; 0x50];
        put(&mut sys, 0x08, &0x5000_00D0u64.to_le_bytes());
        sys[0x1A] = 2;
        if soft {
            put(&mut sys, 0x1C, &FAMILY_KEY.to_le_bytes());
        }
        put(&mut sys, 0x28, &0x5000_0050u64.to_le_bytes());
        put(&mut sys, 0x30, &[4, 0, 4, 0]);
        put(&mut sys, 0x38, &0x5000_0090u64.to_le_bytes());
        put(&mut sys, 0x40, &[5, 0, 5, 0]);
        put(&mut sys, 0x48, &joaat("BM_pelvis").to_le_bytes());
        let quad = [
            [0.0f32, 0.0, 0.0, 3.0],
            [1.0, 0.0, 0.0, 3.0],
            [1.0, 0.0, 1.0, 3.0],
            [0.0, 0.0, 1.0, 3.0],
        ];
        for (i, p) in quad.iter().enumerate() {
            for (c, v) in p.iter().enumerate() {
                put(&mut sys, 0x50 + i * 16 + c * 4, &v.to_le_bytes());
            }
        }
        // four sides at rest length, one shear diagonal stored slack
        for (i, (a, b, r)) in [(0u16, 1u16, 1.0f32), (1, 2, 1.0), (2, 3, 1.0), (3, 0, 1.0), (0, 2, 1.5)]
            .iter()
            .enumerate()
        {
            let o = 0x90 + i * 16;
            put(&mut sys, o, &a.to_le_bytes());
            put(&mut sys, o + 2, &b.to_le_bytes());
            put(&mut sys, o + 4, &r.to_le_bytes());
            put(&mut sys, o + 12, &7u32.to_le_bytes());
        }
        put(&mut sys, 0xE0 + 8, &[1, 0]);
        sys.resize(0xF0, 0);
        sys
    }

    fn synthetic_delta(base: &str, deltas: &[[i64; 3]]) -> (Vec<u8>, Vec<u8>) {
        let mut sys = vec![0u8; 240];
        put(&mut sys, 0x08, &0x5000_00D0u64.to_le_bytes());
        put(&mut sys, 0x10, &0x5000_0070u64.to_le_bytes());
        put(&mut sys, 0x58, &joaat(base).to_le_bytes());
        let bytes = 0x20 + deltas.len() * 8;
        put(&mut sys, 0x78, &((bytes / 4) as u32).to_le_bytes());
        put(&mut sys, 0x80, &0x1601u16.to_le_bytes());
        put(&mut sys, 0x88, &0x6000_0000u64.to_le_bytes());
        put(&mut sys, 0xD0 + 8, &[1, 1]);
        let mut g = Vec::new();
        put(&mut g, 0, &1u32.to_le_bytes());
        for (i, v) in [-0.015f32, -0.015, -0.015, 1e-6, 1e-6, 1e-6].iter().enumerate() {
            put(&mut g, 4 + i * 4, &v.to_le_bytes());
        }
        for (i, d) in deltas.iter().enumerate() {
            let (x, y, z) = (d[0] as u64, d[1] as u64, d[2] as u64);
            let q = (x & 0xFF) | ((y & 0xFF) << 8) | ((x >> 8) << 16) | ((y >> 8) << 32) | (z << 48);
            put(&mut g, 0x20 + i * 8, &q.to_le_bytes());
        }
        g.resize(bytes + 32, 0);
        (sys, g)
    }

    #[test]
    fn half_floats() {
        assert_eq!(f16_to_f32(0x3C00), 1.0);
        assert_eq!(f16_to_f32(0xC000), -2.0);
        assert_eq!(f16_to_f32(0x3555), 0.333_251_95);
        assert_eq!(f16_to_f32(0x0001), 5.960_464_5e-8);
        assert!(f16_to_f32(0x7C00).is_infinite());
        assert_eq!(f16(1.5), 0x3E00);
    }

    #[test]
    fn groom_decodes_with_full_coverage() {
        let (sys, gfx) = synthetic_groom(5, 7);
        let g = decode_groom(&sys, &gfx).unwrap();
        assert_eq!((g.strands, g.points_per_strand, g.root_tris.len()), (5, 7, 1));
        assert_eq!(g.point(2, 0), [0.125, 0.0, 1.5]);
        assert_eq!(g.point(4, 6), [0.25, 0.0, 1.5 - 6.0 * 0.03125]);
        assert_eq!(g.points[0][3], 0.5);
        assert_eq!(g.lengths, vec![0.1875; 5]);
        assert_eq!(g.root_tris[0], [0, 1, 2]);
        let names: Vec<String> = g.bones.iter().map(|&b| hash_label(b)).collect();
        assert_eq!(names, ["BM_Head", "BM_Chin", "EarRig_L_Top"]);
        let cov = groom_coverage(&sys, &gfx);
        assert!(cov.complete(), "{cov:?}");
        assert_eq!(cov.gfx_total, g.buffer_bytes);
        // the overlay is one polyline per strand
        let ov = groom_overlay(&g);
        assert_eq!((ov.points.len(), ov.segments.len()), (35, 30));
        assert!(!ov.segments.iter().any(|s| s[0] % 7 == 6), "no segment joins two strands");
    }

    #[test]
    fn groom_coverage_flags_untyped_bytes() {
        let (sys, mut gfx) = synthetic_groom(3, 4);
        // move B0 four bytes later, leaving a gap
        let b0 = u32_at(&gfx, 0x30).unwrap();
        put(&mut gfx, 0x30, &(b0 + 4).to_le_bytes());
        let cov = groom_coverage(&sys, &gfx);
        assert!(!cov.complete());
        assert!(cov.problems.iter().any(|p| p.contains("gap before B0")), "{cov:?}");
    }

    #[test]
    fn probe_recognises_each_family() {
        let (hs, _) = synthetic_groom(9, 3);
        let p = probe(&hs, hs.len(), 0x1234).unwrap();
        assert_eq!((p.kind, p.count), ("HAIR", 9));
        assert_eq!(p.name, "groom_000001234_9s_BM_Head.hair");

        let p = probe(&synthetic_cloth(false), usize::MAX, 0x10).unwrap();
        assert_eq!((p.kind, p.count, p.name.as_str()), ("CLOTH", 4, "cloth_000000010_4p_5e.cloth"));
        assert_eq!(probe(&synthetic_cloth(true), usize::MAX, 0x10).unwrap().kind, "SOFT");

        let (ds, _) = synthetic_delta("head_xx1_base_000_m_full", &[[0, 0, 0]; 599]);
        let p = probe(&ds, 240, 0x20).unwrap();
        assert_eq!((p.kind, p.count), ("HDELTA", 599));
        assert!(p.name.starts_with("headdelta_000000020_lod3_"), "{}", p.name);
        // a cut stream can't be a whole 240-byte page
        assert!(probe(&ds, usize::MAX, 0x20).is_none());

        // unrelated resources stay unclassified
        let mut other = hs.clone();
        other[0x1A] = 4;
        assert!(probe(&other, other.len(), 0).is_none());
        assert!(probe(&[0u8; 0x200], 0x200, 0).is_none());
    }

    #[test]
    fn packed_followers_finds_resources_mid_page() {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write as _;
        let z = |b: &[u8]| {
            let mut e = ZlibEncoder::new(Vec::new(), Compression::best());
            e.write_all(b).unwrap();
            e.finish().unwrap()
        };
        let align = |v: &mut Vec<u8>| v.resize((v.len() + 15) & !15, 0);
        let (gs, gg) = synthetic_groom(3, 4);
        // page: 16-byte header, groom (system + graphics streams), padding + header, cloth
        let mut page = vec![0u8; 16];
        page.extend(z(&gs));
        align(&mut page);
        page.extend(z(&gg));
        align(&mut page);
        page.extend([0u8; 16]);
        let cloth_at = page.len();
        page.extend(z(&synthetic_cloth(false)));
        page.resize(0x1000, 0);
        let f = packed_followers(&page, 16, 1 << 20, 0x5000, Path::new("x.rpf"), "x.rpf");
        assert_eq!(f.len(), 1, "{:?}", f.iter().map(|e| &e.name).collect::<Vec<_>>());
        assert_eq!((f[0].kind, f[0].offset), ("CLOTH", 0x5000 + cloth_at as u64));

        // the scanner's read window ends inside the cloth stream, or inside the padding before
        // it: the walk says where to resume, and resuming there finds the cloth
        for cut in [cloth_at + 8, cloth_at - 8] {
            let (f, resume) = walk_packed(&page[..cut], WalkStart::After(16), 1 << 20, 0x5000, Path::new("x.rpf"), "x.rpf");
            assert!(f.is_empty());
            let (at, how) = resume.expect("walk stopped at the window's end");
            let at = at as usize - 0x5000;
            // (the padding after the cloth runs to this test page's end, so that walk asks to
            // resume too; in a file the next stream starts a page and ends it)
            let (f, _) = walk_packed(&page[at..], how, 1 << 20, 0x5000 + at as u64, Path::new("x.rpf"), "x.rpf");
            assert_eq!(f.len(), 1);
            assert_eq!((f[0].kind, f[0].offset), ("CLOTH", 0x5000 + cloth_at as u64));
        }

        // the window ends inside the recognised stream itself, which starts a page (offset
        // ...010): the walk retries that stream (without counting it) instead of stopping at
        // the page start, and finds the cloth behind it
        let cut = 16 + z(&gs).len() / 2;
        let (f, resume) = walk_packed(&page[..cut], WalkStart::After(16), 1 << 20, 0x5000, Path::new("x.rpf"), "x.rpf");
        assert!(f.is_empty());
        assert_eq!(resume, Some((0x5010, WalkStart::After(0))));
        let (f, _) = walk_packed(&page[16..], WalkStart::After(0), 1 << 20, 0x5010, Path::new("x.rpf"), "x.rpf");
        assert_eq!(f.iter().map(|e| (e.kind, e.offset)).collect::<Vec<_>>(), vec![("CLOTH", 0x5000 + cloth_at as u64)]);
    }

    #[test]
    fn cloth_decodes_particles_edges_and_names() {
        for soft in [false, true] {
            let c = decode_cloth(&synthetic_cloth(soft)).unwrap();
            assert_eq!(c.soft, soft);
            assert_eq!((c.particles.len(), c.edges.len()), (4, 5));
            assert_eq!(c.particles[2], [1.0, 0.0, 1.0, 3.0]);
            assert_eq!(
                c.edges[4],
                ClothEdge {
                    a: 0,
                    b: 2,
                    rest: 1.5,
                    flags: 7
                }
            );
            assert_eq!((c.rest_matches, c.bad_edges), (4, 0));
            assert!(c.names.contains(&"BM_pelvis"));
            let ov = cloth_overlay(&c);
            assert_eq!((ov.segments.len(), ov.segment_colors.len()), (5, 5));
            assert!(ov.dot_px > 0.0);
        }
    }

    #[test]
    fn head_delta_dequantises_like_mesh_positions() {
        let q = [[15_000i64, 15_000, 15_000], [16_000, 14_000, 15_500], [0, 30_000, 0]];
        let (sys, gfx) = synthetic_delta("head_xx1_base_000_m_full_lod2", &q);
        let d = decode_head_delta(&sys, &gfx).unwrap();
        assert_eq!(d.base_hash, joaat("head_xx1_base_000_m_full_lod2"));
        assert_eq!(d.deltas.len(), 3);
        for (got, q) in d.deltas.iter().zip(q) {
            for c in 0..3 {
                assert!((got[c] - (-0.015 + q[c] as f32 * 1e-6)).abs() < 1e-7, "{got:?} vs {q:?}");
            }
        }
        assert!(head_delta_coverage(&sys, &gfx).complete());
    }

    #[test]
    fn head_delta_names_resolve_to_scanned_heads() {
        let mk = |name: &str, kind: &'static str| RpfEntry {
            source_rpf: PathBuf::from("x.rpf"),
            rpf_name: "x.rpf".into(),
            name: name.into(),
            kind,
            offset: 0,
            comp_size: 0,
            raw_size: 0,
            tracks: 0,
            chunks: Vec::new(),
        };
        let h = joaat("head_ab1_base_000_m_full_lod3");
        let mut v = vec![
            mk("head_ab1_base_000_m_full.#dr", "MODEL"),
            mk(&format!("headdelta_000000020_lod3_{h:08x}.hdelta"), "HDELTA"),
            mk("headdelta_000000030_lod1_45817e90.hdelta", "HDELTA"),
        ];
        name_head_deltas(&mut v);
        assert_eq!(v[1].name, "headdelta_000000020_lod3_head_ab1_base_000_m_full_lod3.hdelta");
        assert_eq!(v[2].name, "headdelta_000000030_lod1_45817e90.hdelta");
    }

    fn parse_glb(b: &[u8]) -> (String, usize) {
        assert_eq!(&b[0..4], b"glTF");
        assert_eq!(u32_at(b, 8).unwrap() as usize, b.len());
        let jl = u32_at(b, 12).unwrap() as usize;
        assert_eq!(u32_at(b, 16), Some(0x4E4F_534A));
        let json = String::from_utf8(b[20..20 + jl].to_vec()).unwrap();
        let bl = u32_at(b, 20 + jl).unwrap() as usize;
        assert_eq!(u32_at(b, 24 + jl), Some(0x004E_4942));
        assert_eq!(28 + jl + bl, b.len());
        (json, bl)
    }

    #[test]
    fn glb_exports_are_well_formed() {
        let (sys, gfx) = synthetic_groom(4, 5);
        let g = decode_groom(&sys, &gfx).unwrap();
        let (json, bl) = parse_glb(&groom_glb(&g, "a \"groom\""));
        assert!(json.contains(r#""mode":1"#) && json.contains(r#""_RADIUS":1"#), "{json}");
        assert!(
            json.contains(r#""count":32,"type":"SCALAR""#),
            "4 strands x 4 segments x 2 indices: {json}"
        );
        assert!(json.contains(r#""name":"a \"groom\"""#));
        // positions (20 x 12) + radius (20 x 4) + indices (32 x 4)
        assert_eq!(bl, 240 + 80 + 128);
        // Y-up: the root at z = 1.5 becomes y = 1.5
        assert!(
            json.contains(r#""max":[0.1875,1.5,-0]"#) || json.contains(r#""max":[0.1875,1.5,0]"#),
            "{json}"
        );

        let c = decode_cloth(&synthetic_cloth(false)).unwrap();
        let (json, _) = parse_glb(&cloth_glb(&c, "cloth"));
        assert!(
            json.contains(r#""mode":0"#) && json.contains(r#""count":10,"type":"SCALAR""#),
            "{json}"
        );
    }

    /// Grid head of `n` x `n` vertices with UVs; `f` displaces vertex i.
    fn grid_head(n: usize, f: impl Fn(usize) -> [f32; 3], order: &[usize]) -> Mesh3D {
        let mut verts = Vec::new();
        let mut uvs = Vec::new();
        for &i in order {
            let (x, y) = ((i % n) as f32, (i / n) as f32);
            let d = f(i);
            verts.push([x * 0.01 + d[0], 0.1 + d[1], y * 0.01 + d[2]]);
            uvs.push([x / n as f32, y / n as f32]);
        }
        let tris: Vec<[u32; 3]> = (0..verts.len() as u32 - 2).map(|i| [i, i + 1, i + 2]).collect();
        let tri_geom = vec![0u16; tris.len()];
        Mesh3D::from_parts(
            "head".into(),
            verts,
            uvs,
            tris,
            tri_geom,
            Vec::new(),
            None,
            String::new(),
            1,
            0,
            String::new(),
        )
    }

    #[test]
    fn head_delta_applies_through_a_matching_head() {
        let n = 12;
        let v = n * n;
        let off = |i: usize| {
            [
                ((i * 7) % 11) as f32 * 1e-3,
                ((i * 3) % 5) as f32 * -1e-3,
                ((i * 5) % 13) as f32 * 5e-4,
            ]
        };
        let ident: Vec<usize> = (0..v).collect();
        // the head and the deltas each use their own vertex order
        let head_order: Vec<usize> = (0..v).map(|i| (i * 37) % v).collect();
        let base = grid_head(n, |_| [0.0; 3], &ident);
        let head = grid_head(n, off, &head_order);
        let other = grid_head(n, |i| [off(i)[1], off(i)[0], 0.0], &ident);
        let delta = HeadDelta {
            deltas: (0..v).map(|i| off((i * 53) % v)).collect(),
            ..Default::default()
        };
        let mut cands = vec![("other".to_string(), other), ("target".to_string(), head)].into_iter();
        let a = apply_head_delta(&delta, &base, &mut cands, &|_| {}).unwrap();
        assert_eq!(a.matched_head, "target");
        assert!(a.max_error < 1e-6, "{}", a.max_error);
        for i in 0..v {
            let want = [
                base.verts[i][0] + off(i)[0],
                base.verts[i][1] + off(i)[1],
                base.verts[i][2] + off(i)[2],
            ];
            for c in 0..3 {
                assert!((a.mesh.verts[i][c] - want[c]).abs() < 1e-6, "vertex {i}");
            }
        }

        // a head whose offsets are the deltas mirrored in X has the same lengths, but no
        // vector matches, so it must not be taken as the delta's head
        let mirror = grid_head(n, |i| [-off(i)[0], off(i)[1], off(i)[2]], &head_order);
        let mut cands = vec![("mirror".to_string(), mirror)].into_iter();
        assert!(apply_head_delta(&delta, &base, &mut cands, &|_| {}).is_err());
    }

    // ------------------------------------------------------------ data-gated

    /// Scan of the packs archive named by `PERCYSTAR_PACKS_RPF`, cached in
    /// `PERCYSTAR_PEDPHYS_CACHE` (a scan cache TSV) when set.
    pub(crate) fn packs_entries() -> Option<Vec<RpfEntry>> {
        let packs = PathBuf::from(std::env::var_os("PERCYSTAR_PACKS_RPF")?);
        let cache = std::env::var_os("PERCYSTAR_PEDPHYS_CACHE").map(PathBuf::from);
        if let Some(c) = cache.as_ref().filter(|c| c.is_file()) {
            if let Some(v) = crate::load_disk_cache_from(c).into_values().next() {
                return Some(v);
            }
        }
        let t0 = std::time::Instant::now();
        let (_, entries) = crate::scan_single_rpf(&packs, None).expect("scan packs");
        eprintln!("scanned {} entries in {:.0}s", entries.len(), t0.elapsed().as_secs_f32());
        if let Some(c) = cache {
            use std::io::Write as _;
            let mut w = std::io::BufWriter::new(std::fs::File::create(&c).unwrap());
            for e in &entries {
                writeln!(
                    w,
                    "packs\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t",
                    e.source_rpf.display(),
                    e.rpf_name,
                    e.name,
                    e.kind,
                    e.offset,
                    e.comp_size,
                    e.raw_size,
                    e.tracks
                )
                .unwrap();
            }
        }
        Some(entries)
    }

    /// Decodes every groom, cloth, soft body and head delta in a real packs archive and checks
    /// byte coverage on grooms (all of them, or `PERCYSTAR_PEDPHYS_N`) like check_hair.py.
    ///
    ///   PERCYSTAR_PACKS_RPF=<packs.rpf> cargo test --release pedphys_packs -- --ignored --nocapture
    #[test]
    #[ignore]
    fn pedphys_packs() {
        let Some(entries) = packs_entries() else {
            eprintln!("set PERCYSTAR_PACKS_RPF to run");
            return;
        };
        let n_cov: usize = std::env::var("PERCYSTAR_PEDPHYS_N")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(usize::MAX);
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for e in &entries {
            *counts.entry(e.kind).or_default() += 1;
        }
        eprintln!(
            "kinds: HAIR {:?} CLOTH {:?} SOFT {:?} HDELTA {:?}",
            counts.get("HAIR"),
            counts.get("CLOTH"),
            counts.get("SOFT"),
            counts.get("HDELTA")
        );
        if let Ok(p) = std::env::var("PERCYSTAR_PEDPHYS_DUMP") {
            use std::io::Write as _;
            let mut w = std::fs::File::create(p).unwrap();
            for e in entries.iter().filter(|e| is_kind(e.kind)) {
                writeln!(w, "{}\t{}", e.offset, e.kind).unwrap();
            }
        }
        let t0 = std::time::Instant::now();
        let (mut errs, mut cov_n, mut cov_full) = (Vec::new(), 0usize, 0usize);
        let (mut st, mut sy, mut gt, mut gy) = (0usize, 0usize, 0usize, 0usize);
        let (mut strands, mut points, mut particles, mut edges, mut deltas) = (0usize, 0usize, 0usize, 0usize, 0usize);
        let mut problems: HashMap<String, usize> = HashMap::new();
        for e in entries.iter().filter(|e| is_kind(e.kind)) {
            let r = read_resource(&e.source_rpf, e.offset).and_then(|(sys, gfx)| match e.kind {
                "HAIR" => decode_groom(&sys, &gfx).map(|g| {
                    strands += g.strands;
                    points += g.points.len();
                    if cov_n < n_cov {
                        let c = groom_coverage(&sys, &gfx);
                        cov_n += 1;
                        cov_full += c.complete() as usize;
                        (st, sy, gt, gy) = (st + c.sys_total, sy + c.sys_typed, gt + c.gfx_total, gy + c.gfx_typed);
                        for p in c.problems {
                            *problems.entry(p.split(':').next().unwrap_or("").to_string()).or_default() += 1;
                        }
                    }
                }),
                "CLOTH" | "SOFT" => decode_cloth(&sys).and_then(|c| {
                    particles += c.particles.len();
                    edges += c.edges.len();
                    if c.bad_edges > 0 {
                        Err(format!("{} edges out of range", c.bad_edges))
                    } else {
                        Ok(())
                    }
                }),
                _ => decode_head_delta(&sys, &gfx).map(|d| deltas += d.deltas.len()),
            });
            if let Err(err) = r {
                errs.push(format!("{} {}: {err}", e.kind, e.name));
            }
        }
        eprintln!(
            "decoded in {:.0}s: {strands} strands, {points} points, {particles} particles, {edges} edges, {deltas} delta vertices",
            t0.elapsed().as_secs_f32()
        );
        eprintln!(
            "groom coverage over {cov_n}: {cov_full} complete; system {sy}/{st} B ({:.4}%), graphics {gy}/{gt} B ({:.4}%)",
            100.0 * sy as f64 / st.max(1) as f64,
            100.0 * gy as f64 / gt.max(1) as f64
        );
        if !problems.is_empty() {
            eprintln!("coverage problems: {problems:?}");
        }
        eprintln!("decode errors: {}", errs.len());
        for e in errs.iter().take(20) {
            eprintln!("  {e}");
        }
        assert!(errs.is_empty());
        assert_eq!(cov_full, cov_n);
    }

    fn g_len_ok(lo: [f32; 3], hi: [f32; 3]) -> bool {
        hi[0] - lo[0] > 0.15 && hi[1] - lo[1] > 0.15
    }

    /// Headless screenshots of the real app (no window, no focus, no sound): a groom alone, the
    /// same groom on a head, a cloth mesh and a head delta applied to its base head.
    ///
    ///   PERCYSTAR_PACKS_RPF=<packs.rpf> SIXSTARS_PEDPHYS_SHOTS=<out dir> \
    ///     cargo test --release pedphys_screenshots -- --ignored --nocapture
    #[test]
    #[ignore]
    fn pedphys_screenshots() {
        use egui_kittest::Harness;
        let Some(out) = std::env::var_os("SIXSTARS_PEDPHYS_SHOTS").map(PathBuf::from) else {
            eprintln!("set SIXSTARS_PEDPHYS_SHOTS and PERCYSTAR_PACKS_RPF to capture");
            return;
        };
        let Some(entries) = packs_entries() else { return };
        std::fs::create_dir_all(&out).unwrap();
        // sandbox HOME so the real app data is never read or written (restored on return)
        let _home = crate::context_capture::SandboxHome::new();

        let pick = |kind: &str, ok: &dyn Fn(&RpfEntry) -> bool| {
            let name = std::env::var(format!("SIXSTARS_PEDPHYS_{kind}")).ok();
            entries
                .iter()
                .position(|e| e.kind == kind && name.as_deref().map_or_else(|| ok(e), |n| e.name.contains(n)))
                .expect(kind)
        };
        // a full head of hair: reaches the crown of the default head (top near z = 0.81)
        let groom = pick("HAIR", &|e| {
            e.name.ends_with("_BM_Head.hair")
                && (300..1500).contains(&e.tracks)
                && read_resource(&e.source_rpf, e.offset)
                    .ok()
                    .and_then(|(s, g)| decode_groom(&s, &g).ok())
                    .and_then(|g| g.bounds())
                    .is_some_and(|(lo, hi)| hi[2] > 0.8 && lo[2] < 0.7 && g_len_ok(lo, hi))
        });
        let cloth = pick("CLOTH", &|e| (400..1500).contains(&e.tracks));
        let delta = pick("HDELTA", &|e| e.tracks == 2958 && e.name.contains("0968d6010"));

        let mut h = Harness::builder()
            .with_size(egui::vec2(1360.0, 820.0))
            .with_pixels_per_point(2.0)
            .with_max_steps(8)
            .wgpu()
            .build_eframe(|cc| crate::Rpf9App::new(cc, None, Vec::new()));
        {
            let app = h.state_mut();
            let mut st = app.state.lock().unwrap();
            st.entries = entries.clone();
            st.rpf_names = vec!["packs.rpf".into()];
            st.scanning = false;
            st.ver += 1;
            drop(st);
            app.view_key = None;
            app.category = crate::Category::PedPhysics;
            app.update.lock().unwrap().available = None;
        }
        let wait = |h: &mut Harness<crate::Rpf9App>| {
            for _ in 0..600 {
                h.step();
                let busy = h.state().pedphys_view.as_ref().is_some_and(|v| v.job.lock().unwrap().busy);
                if !busy {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            for _ in 0..3 {
                h.step();
            }
        };
        let shot = |h: &mut Harness<crate::Rpf9App>, name: &str| {
            let p = out.join(name);
            h.render().expect("render").save(&p).unwrap();
            eprintln!("wrote {}", p.display());
        };

        h.state_mut().select_entry(groom);
        wait(&mut h);
        shot(&mut h, "groom.png");

        {
            let v = h.state_mut().pedphys_view.as_mut().unwrap();
            let Ok(Decoded::Groom(g, _)) = &v.decoded else {
                panic!("groom decode")
            };
            let g = g.clone();
            v.show_on_head(&g);
        }
        wait(&mut h);
        shot(&mut h, "groom_on_head.png");
        h.state_mut().select_entry(cloth);
        wait(&mut h);
        shot(&mut h, "cloth.png");

        h.state_mut().select_entry(delta);
        wait(&mut h);
        shot(&mut h, "headdelta_info.png");
        {
            let v = h.state_mut().pedphys_view.as_mut().unwrap();
            let Ok(Decoded::Delta(d, _)) = &v.decoded else {
                panic!("delta decode")
            };
            let d = d.clone();
            v.apply_delta(&d);
        }
        wait(&mut h);
        shot(&mut h, "headdelta_applied.png");
        let v = h.state().pedphys_view.as_ref().unwrap();
        eprintln!("delta: {} {}", v.note, v.job.lock().unwrap().error);
    }
}
