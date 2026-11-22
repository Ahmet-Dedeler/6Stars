// Percy6Stars by Ahmet Dedeler (https://github.com/Ahmet-Dedeler, https://ahmetdedeler.com)
// Fork of PercyStar by rpatty-dev (https://github.com/rpatty-dev/PercyStar).
// Source: https://github.com/Ahmet-Dedeler/Percy6Stars
// License: PercyStar Non-Commercial Attribution & Copyleft License v1.2 (see LICENSE)
// If you copy, vendor or port this file, keep this header and credit both projects in your README.
//! Text that the copy actions put on the clipboard: names, paths, offsets, and the
//! "Copy as AI context" block (Markdown or JSON).
//!
//! Everything here is pure formatting over plain data, so it is unit tested without a UI or
//! game files. The AI context is meant to be pasted into a chat with a language model: it is
//! short, has one fact per line, and never carries payload bytes beyond an optional hex line
//! of the first 64 decoded bytes.

use crate::RpfEntry;
use std::fmt::Write as _;

/// Most related files listed per asset in the AI context; the rest are summarised as a count.
pub const MAX_RELATED_LISTED: usize = 25;

/// `archive.rpf/inner/path`: the archive's file name, then the entry name. The RPF9 table of
/// contents is encrypted, so the entry name is the carved name Percy gives it (spec §1).
pub fn full_path(e: &RpfEntry) -> String {
    format!("{}/{}", e.rpf_name, e.name)
}

/// Lowercase name without its extension, the ` (+N)` duplicate marker or a `_hi` / `_mid`
/// detail suffix. `prop_x_hi.#dr` and `prop_x.#dr` share the stem `prop_x`.
pub fn name_stem(name: &str) -> String {
    let raw = raw_stem(name);
    raw.strip_suffix("_hi").or_else(|| raw.strip_suffix("_mid")).map(str::to_string).unwrap_or(raw)
}

/// Lowercase name without its extension and ` (+N)` marker, keeping detail suffixes.
pub fn raw_stem(name: &str) -> String {
    let mut s = name.rsplit_once('.').map_or(name, |x| x.0).to_ascii_lowercase();
    if let Some(cut) = s.rfind(" (+") {
        if s.ends_with(')') {
            s.truncate(cut);
        }
    }
    s
}

/// The id line for an entry: the joaat hash of its name stem (how RAGE refers to named
/// assets), plus a kind-specific index where one exists.
pub fn entry_id(e: &RpfEntry) -> String {
    let stem = raw_stem(&e.name);
    let mut out = format!("joaat(\"{stem}\") = 0x{:08x}", crate::psin::joaat(&stem));
    match e.kind {
        "TTPF" => {
            let _ = write!(out, " · texture #{} in the store", e.tracks);
        }
        "ANIM" if e.name.starts_with("clipdict_") => {
            if let Some(h) = e.name.get(9..17) {
                let _ = write!(out, " · dictionary hash 0x{h}");
            }
        }
        _ => {}
    }
    out
}

/// `archive @ 0xOFFSET (N bytes)`.
pub fn offset_line(e: &RpfEntry) -> String {
    format!("{} @ 0x{:08x} ({} bytes)", e.rpf_name, e.offset, e.comp_size.max(e.raw_size))
}

/// What the copy-several actions produce, one entry per line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListMode {
    Names,
    Paths,
    Offsets,
}

pub fn format_list(entries: &[&RpfEntry], mode: ListMode) -> String {
    entries
        .iter()
        .map(|e| match mode {
            ListMode::Names => e.name.clone(),
            ListMode::Paths => full_path(e),
            ListMode::Offsets => offset_line(e),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Header of a RAGE9 resource, read from the start of its first zlib stream (spec §2.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceHeader {
    /// u64 at +0x08: page-map pointer (`0x5xxxxxxx`, into the system page).
    pub page_map: u64,
    /// Page counts stored at the page map (+8 system, +9 graphics), when the map is in reach.
    pub pages: Option<(u8, u8)>,
    /// `PRD0` (`0DRP` on disk) at +0x10 and the type byte after it, for self-describing resources.
    pub prd0_type: Option<u8>,
}

impl ResourceHeader {
    /// Parses the first bytes of an inflated resource. `None` if it doesn't look like one.
    pub fn parse(head: &[u8]) -> Option<Self> {
        if head.len() < 0x18 {
            return None;
        }
        let vft = u64::from_le_bytes(head[0..8].try_into().ok()?);
        let page_map = u64::from_le_bytes(head[8..16].try_into().ok()?);
        if vft != 0 || !(0x5000_0000..0x5800_0000).contains(&page_map) {
            return None;
        }
        let at = (page_map - 0x5000_0000) as usize;
        let pages = head.get(at + 8..at + 10).map(|b| (b[0], b[1]));
        let prd0_type = (&head[0x10..0x14] == b"0DRP" || &head[0x10..0x14] == b"PRD0").then(|| head[0x14]);
        Some(Self { page_map, pages, prd0_type })
    }

    pub fn line(&self) -> String {
        let mut s = format!("page map 0x{:08x}", self.page_map);
        if let Some((sys, gfx)) = self.pages {
            let _ = write!(s, " · {sys} system + {gfx} graphics page(s)");
        }
        if let Some(t) = self.prd0_type {
            let _ = write!(s, " · PRD0 type 0x{t:02x}");
        }
        s
    }
}

/// What a decoder said about an entry. Only facts the decoders actually produce.
#[derive(Clone, Debug, PartialEq)]
pub enum Decoded {
    Texture {
        format: String,
        w: usize,
        h: usize,
        mips: u8,
        /// Shown size differs from the stored mip 0, e.g. a BCPack fallback.
        note: Option<String>,
    },
    TextureDict {
        textures: Vec<String>,
    },
    Model {
        verts: usize,
        tris: usize,
        geoms: usize,
        meshlets: usize,
        lod: String,
        /// Geometry count per LOD level stored in the file (zero = level absent).
        lod_geoms: Vec<usize>,
        /// Separate drawables in a dictionary.
        parts: usize,
        /// Bones in the embedded skeleton (fragments), zero if none.
        bones: usize,
        /// Textures bound by the shaders: (role, found in a TTPF pool).
        textures: Vec<(String, bool)>,
        shaders_binding: usize,
    },
    Audio {
        codec: String,
        streams: Vec<AudioStream>,
        pack: Option<String>,
        used_by: Vec<String>,
    },
    Anim {
        clips: u32,
    },
    Navmesh {
        polys: u32,
    },
    Text {
        bytes: usize,
        lines: usize,
        first_line: String,
    },
    /// Ped physics resource from packs.rpf (hair groom, cloth, soft body, head delta): what it
    /// is and the decoder's facts.
    PedPhysics {
        what: String,
        facts: Vec<String>,
    },
    /// Not decoded, with the reason (e.g. "select the entry to decode it").
    Unavailable(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioStream {
    pub name: String,
    pub channels: u16,
    pub sample_rate: u32,
    pub duration_secs: f32,
}

impl Decoded {
    pub fn line(&self) -> String {
        match self {
            Decoded::Texture { format, w, h, mips, note } => {
                let mut s = format!("texture {format} · {w}x{h} · {mips} mip(s)");
                if let Some(n) = note {
                    let _ = write!(s, " · {n}");
                }
                s
            }
            Decoded::TextureDict { textures } => {
                let shown: Vec<&str> = textures.iter().take(12).map(String::as_str).collect();
                let more = textures.len().saturating_sub(shown.len());
                let mut s = format!("texture dictionary · {} texture(s): {}", textures.len(), shown.join(", "));
                if more > 0 {
                    let _ = write!(s, " (+{more} more)");
                }
                s
            }
            Decoded::Model { verts, tris, geoms, meshlets, lod, lod_geoms, parts, bones, textures, shaders_binding } => {
                let mut s = format!("model · {verts} verts · {tris} tris · {geoms} geometries · {meshlets} meshlets · showing {lod}");
                let levels: Vec<String> = lod_geoms
                    .iter()
                    .enumerate()
                    .filter(|(_, &n)| n > 0)
                    .map(|(i, n)| format!("LOD{i}={n}"))
                    .collect();
                if !levels.is_empty() {
                    let _ = write!(s, " · LOD table {}", levels.join(" "));
                }
                if *parts > 1 {
                    let _ = write!(s, " · {parts} drawables (dictionary)");
                }
                if *bones > 0 {
                    let _ = write!(s, " · skeleton embedded ({bones} bones)");
                } else {
                    s.push_str(" · no embedded skeleton");
                }
                if !textures.is_empty() {
                    let found = textures.iter().filter(|t| t.1).count();
                    let roles: Vec<&str> = textures.iter().take(10).map(|t| t.0.as_str()).collect();
                    let _ = write!(s, " · shaders bind {} texture(s), {found} found in TTPF pools: {}", textures.len(), roles.join(", "));
                    if *shaders_binding > 1 {
                        let _ = write!(s, " (most shared one is used by {shaders_binding} shaders)");
                    }
                    if textures.len() > roles.len() {
                        let _ = write!(s, " (+{} more)", textures.len() - roles.len());
                    }
                }
                s
            }
            Decoded::Audio { codec, streams, pack, used_by } => {
                let total: f32 = streams.iter().map(|t| t.duration_secs).sum();
                let mut s = format!("audio {codec} · {} stream(s)", streams.len());
                if total > 0.0 {
                    let _ = write!(s, " · {total:.1}s total");
                }
                if let Some(first) = streams.first() {
                    if first.sample_rate > 0 {
                        let _ = write!(s, " · {} ch @ {} Hz", first.channels, first.sample_rate);
                    }
                }
                let named: Vec<String> = streams
                    .iter()
                    .take(8)
                    .map(|t| if t.duration_secs > 0.0 { format!("{} ({:.1}s)", t.name, t.duration_secs) } else { t.name.clone() })
                    .collect();
                if !named.is_empty() {
                    let _ = write!(s, " · streams: {}", named.join(", "));
                    if streams.len() > named.len() {
                        let _ = write!(s, " (+{} more)", streams.len() - named.len());
                    }
                }
                if let Some(p) = pack {
                    let _ = write!(s, " · pack {p}");
                }
                if !used_by.is_empty() {
                    let shown: Vec<&str> = used_by.iter().take(6).map(String::as_str).collect();
                    let _ = write!(s, " · used by {}", shown.join(", "));
                    if used_by.len() > shown.len() {
                        let _ = write!(s, " (+{} more)", used_by.len() - shown.len());
                    }
                }
                s
            }
            Decoded::Anim { clips } => format!("clip dictionary · {clips} clip(s)"),
            Decoded::Navmesh { polys } => format!("navmesh tile · {polys} polygons"),
            Decoded::PedPhysics { what, facts } => std::iter::once(what.as_str()).chain(facts.iter().map(String::as_str)).collect::<Vec<_>>().join(" · "),
            Decoded::Text { bytes, lines, first_line } => {
                format!("text · {bytes} bytes · {lines} line(s) · first line: {}", clip(first_line, 100))
            }
            Decoded::Unavailable(why) => format!("not decoded ({why})"),
        }
    }
}

/// One file that belongs with the asset, and the rule that linked it.
#[derive(Clone, Debug, PartialEq)]
pub struct RelatedLine {
    pub path: String,
    pub kind: &'static str,
    pub why: String,
}

/// Everything the AI context says about one entry.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetContext {
    pub archive: String,
    pub archive_path: String,
    pub path: String,
    pub name: String,
    pub kind: &'static str,
    pub raw_size: u64,
    pub comp_size: u64,
    pub offset: u64,
    pub chunks: usize,
    pub id: String,
    pub header: Option<ResourceHeader>,
    pub decoded: Decoded,
    /// `None` = the relation finder didn't run (e.g. index still building).
    pub related: Option<Vec<RelatedLine>>,
    pub head64: Option<Vec<u8>>,
}

impl AssetContext {
    /// Facts that come from the scan entry alone; decoders fill in the rest.
    pub fn from_entry(e: &RpfEntry) -> Self {
        Self {
            archive: e.rpf_name.clone(),
            archive_path: e.source_rpf.display().to_string(),
            path: full_path(e),
            name: e.name.clone(),
            kind: e.kind,
            raw_size: e.raw_size,
            comp_size: e.comp_size,
            offset: e.offset,
            chunks: e.chunks.len(),
            id: entry_id(e),
            header: None,
            decoded: Decoded::Unavailable("not decoded yet".into()),
            related: None,
            head64: None,
        }
    }
}

/// Human name of a kind, with the GTA V equivalent where the spec gives one (§2.2).
pub fn kind_label(kind: &str, name: &str) -> String {
    let ext = name.rsplit_once('.').map(|x| x.1).unwrap_or("");
    let what = match (kind, ext) {
        ("MODEL" | "VEH", "#ft") => "fragment `#ft` (GTA V .yft)",
        ("MODEL" | "VEH", "#dd") => "drawable dictionary `#dd` (GTA V .ydd)",
        ("MODEL" | "VEH", _) => "drawable `#dr` (GTA V .ydr)",
        ("TXD", _) => "texture dictionary `#td` (GTA V .ytd)",
        ("TTPF", _) => "texture in a ttpf store",
        ("AWC", _) => "audio bank (ADAT / .awc)",
        ("BK2", _) => "Bink 2 video",
        ("ANIM", _) => "clip dictionary (GTA V .ycd)",
        ("NAVM", _) => "navmesh tile (GTA V .ynv)",
        ("META7A", _) => "meta resource, PRD0 type 0x7a",
        ("PSIN", _) => "PSIN scene placements",
        ("GXT", _) => "text table (GXT)",
        ("DAT", _) => "audio metadata (DAT .rel)",
        ("RSC9", _) => "RAGE9 resource without meshlets",
        ("RBF0", _) => "binary UI layout (RBF0)",
        ("GFX", _) => "Scaleform UI movie",
        ("XML", _) => "text / XML",
        ("HMAP", _) => "terrain heightmap",
        ("ZLIB", _) => "zlib-compressed binary",
        _ => "",
    };
    if what.is_empty() {
        kind.to_string()
    } else {
        format!("{kind}, {what}")
    }
}

/// How Percy decodes a kind and which GTA6-Asset-Info section covers it.
pub fn decode_note(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "MODEL" | "VEH" => (
            "inflates the RAGE9 system + graphics pages, walks drawable -> LOD table -> geometries, decodes meshlet index buffers and vertex blobs; textures are matched by 64-bit hash in the ttpf pools",
            "formats/02-resources.md §2.1, formats/07-models.md §7.1-7.7, formats/12-materials.md",
        ),
        "TTPF" => (
            "reads the ETPF descriptor (size, format, mips), inflates the chunks (zlib or BCPack BC1), then undoes the RDNA2 64 KiB / 4 KiB swizzle",
            "formats/05-ttpf.md, formats/06-bcpack.md",
        ),
        "TXD" => (
            "parses the pgDictionary<grcTexture> resource and undoes the Xbox Series tiling per texture",
            "formats/03-texture-dictionaries.md, formats/04-xbox-tiling.md",
        ),
        "AWC" => (
            "parses the ADAT chunk table and decodes the Opus streams with rusty-opus; bank names come from the audio.rpf DAT catalogue",
            "structure/03-audio-awc.md, formats/17-other.md (Music catalogue)",
        ),
        "DAT" => ("inflates the audio metadata chunk and reads its object index table to name sound banks", "formats/17-other.md (Music catalogue)"),
        "ANIM" => ("probes the crClipDictionary header for its clip count; the preview is a dummy rig", "formats/17-other.md (Animation)"),
        "NAVM" => ("decodes the CNavMesh tile (version 0x00010015) polygons into a mesh", "formats/14-navmesh.md"),
        "META7A" => ("parses the type 0x7a meta tables (structs, enums, objects)", "formats/08-meta.md"),
        "PSIN" => ("reads the PSIN placement records", "formats/17-other.md (PSIN scene placements)"),
        "GXT" => ("reads the GXT string table", "formats/17-other.md (Text (GXT))"),
        "RSC9" => ("a RAGE9 resource root without meshlet data; shown as hex", "formats/02-resources.md"),
        "BK2" => ("hands the clip to the bundled RAD Bink 2 decoder", "not covered by the spec"),
        "XML" | "ZLIB" | "RBF0" | "GFX" => ("inflates the 64 KiB zlib chunks of a plain file", "formats/01-rpf9.md (Payload)"),
        "HMAP" => ("reads the terrain height grid for the world map", "not covered by the spec"),
        _ => ("shown as hex", "formats/01-rpf9.md"),
    }
}

/// `1.2 MB` style sizes (same steps as the file list).
pub fn human_size(bytes: u64) -> String {
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

fn hex_line(b: &[u8]) -> String {
    b.iter().take(64).map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

fn clip(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if t.len() < s.len() {
        format!("{t}…")
    } else {
        t
    }
}

fn md_one(out: &mut String, a: &AssetContext) {
    let _ = writeln!(out, "- archive: `{}` (`{}`)", a.archive, a.archive_path);
    let _ = writeln!(out, "- path: `{}`", a.path);
    let _ = writeln!(out, "- kind: {}", kind_label(a.kind, &a.name));
    let mut size = format!("- size: {} raw · {} stored", human_size(a.raw_size), human_size(a.comp_size));
    let _ = write!(size, " · offset 0x{:x}", a.offset);
    if a.chunks > 0 {
        let _ = write!(size, " · {} zlib chunk(s)", a.chunks);
    }
    let _ = writeln!(out, "{size}");
    let _ = writeln!(out, "- id: {}", a.id);
    if let Some(h) = &a.header {
        let _ = writeln!(out, "- resource: {}", h.line());
    }
    let _ = writeln!(out, "- decoded: {}", a.decoded.line());
    match &a.related {
        None => {
            let _ = writeln!(out, "- related: not checked");
        }
        Some(r) if r.is_empty() => {
            let _ = writeln!(out, "- related: none found by Percy's rules");
        }
        Some(r) => {
            let _ = writeln!(out, "- related ({}):", r.len());
            for l in r.iter().take(MAX_RELATED_LISTED) {
                let _ = writeln!(out, "  - `{}` {} ({})", l.path, l.kind, l.why);
            }
            if r.len() > MAX_RELATED_LISTED {
                let _ = writeln!(out, "  - +{} more", r.len() - MAX_RELATED_LISTED);
            }
        }
    }
    let (how, spec) = decode_note(a.kind);
    let _ = writeln!(out, "- percy: {how}");
    let _ = writeln!(out, "- spec: GTA6-Asset-Info {spec}");
    if let Some(h) = &a.head64 {
        let _ = writeln!(out, "- first {} bytes (decoded): `{}`", h.len().min(64), hex_line(h));
    }
}

/// Markdown block for one or more assets. One asset: a flat list. Several: a numbered
/// section per asset under one heading.
pub fn to_markdown(assets: &[AssetContext]) -> String {
    let mut out = String::new();
    match assets {
        [] => {}
        [one] => {
            let _ = writeln!(out, "### Asset `{}` (Percy6Stars)", one.name);
            md_one(&mut out, one);
        }
        many => {
            let _ = writeln!(out, "### {} assets (Percy6Stars)", many.len());
            for (i, a) in many.iter().enumerate() {
                let _ = writeln!(out, "\n#### {}. `{}`", i + 1, a.name);
                md_one(&mut out, a);
            }
        }
    }
    let _ = writeln!(out, "\nNote: RPF9 tables of contents are encrypted, so names and paths are the ones Percy carved from the payload (spec formats/01-rpf9.md).");
    out
}

/// Minimal JSON string escaping (quotes, backslashes, control characters).
pub fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn json_one(a: &AssetContext) -> String {
    let (how, spec) = decode_note(a.kind);
    let mut f: Vec<String> = vec![
        format!("\"archive\":{}", json_str(&a.archive)),
        format!("\"archive_path\":{}", json_str(&a.archive_path)),
        format!("\"path\":{}", json_str(&a.path)),
        format!("\"name\":{}", json_str(&a.name)),
        format!("\"kind\":{}", json_str(a.kind)),
        format!("\"kind_label\":{}", json_str(&kind_label(a.kind, &a.name))),
        format!("\"raw_size\":{}", a.raw_size),
        format!("\"stored_size\":{}", a.comp_size),
        format!("\"offset\":{}", json_str(&format!("0x{:x}", a.offset))),
        format!("\"zlib_chunks\":{}", a.chunks),
        format!("\"id\":{}", json_str(&a.id)),
    ];
    if let Some(h) = &a.header {
        let pages = h.pages.map_or("null".to_string(), |(s, g)| format!("{{\"system\":{s},\"graphics\":{g}}}"));
        let t = h.prd0_type.map_or("null".to_string(), |t| json_str(&format!("0x{t:02x}")));
        f.push(format!(
            "\"resource\":{{\"page_map\":{},\"pages\":{pages},\"prd0_type\":{t}}}",
            json_str(&format!("0x{:08x}", h.page_map))
        ));
    }
    f.push(format!("\"decoded\":{}", json_str(&a.decoded.line())));
    match &a.related {
        None => f.push("\"related\":null".into()),
        Some(r) => {
            let items: Vec<String> = r
                .iter()
                .map(|l| format!("{{\"path\":{},\"kind\":{},\"why\":{}}}", json_str(&l.path), json_str(l.kind), json_str(&l.why)))
                .collect();
            f.push(format!("\"related\":[{}]", items.join(",")));
        }
    }
    f.push(format!("\"percy_decoder\":{}", json_str(how)));
    f.push(format!("\"spec\":{}", json_str(&format!("GTA6-Asset-Info {spec}"))));
    if let Some(h) = &a.head64 {
        f.push(format!("\"first_bytes_hex\":{}", json_str(&hex_line(h))));
    }
    format!("{{{}}}", f.join(","))
}

/// JSON for one asset (an object) or several (an array). Pretty enough to read, one key
/// per asset line.
pub fn to_json(assets: &[AssetContext]) -> String {
    match assets {
        [one] => json_one(one),
        many => format!("[\n{}\n]", many.iter().map(json_one).collect::<Vec<_>>().join(",\n")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(name: &str, kind: &'static str) -> RpfEntry {
        RpfEntry {
            source_rpf: PathBuf::from("/data/sample/world.rpf"),
            rpf_name: "world.rpf".into(),
            name: name.into(),
            kind,
            offset: 0x1234_5010,
            comp_size: 2048,
            raw_size: 8192,
            tracks: 3,
            chunks: Vec::new(),
        }
    }

    #[test]
    fn stems_drop_extension_detail_and_duplicate_marker() {
        assert_eq!(name_stem("Prop_Box_hi.#dr"), "prop_box");
        assert_eq!(name_stem("prop_box_mid.#dr"), "prop_box");
        assert_eq!(name_stem("waterfog2 (+1).#td"), "waterfog2");
        assert_eq!(raw_stem("prop_box_hi.#dr"), "prop_box_hi");
        assert_eq!(name_stem("noext"), "noext");
    }

    #[test]
    fn lists_and_ids() {
        let a = entry("crate_a.#dr", "MODEL");
        let b = entry("crate_b.#dr", "MODEL");
        assert_eq!(format_list(&[&a, &b], ListMode::Names), "crate_a.#dr\ncrate_b.#dr");
        assert_eq!(format_list(&[&a], ListMode::Paths), "world.rpf/crate_a.#dr");
        assert_eq!(format_list(&[&a], ListMode::Offsets), "world.rpf @ 0x12345010 (8192 bytes)");
        assert_eq!(entry_id(&a), format!("joaat(\"crate_a\") = 0x{:08x}", crate::psin::joaat("crate_a")));
        let t = entry("tex_00003_64x64_BC1_UNORM.ttpf", "TTPF");
        assert!(entry_id(&t).ends_with("texture #3 in the store"));
        let c = entry("clipdict_f14a15be_1clips.ycd", "ANIM");
        assert!(entry_id(&c).ends_with("dictionary hash 0xf14a15be"));
    }

    #[test]
    fn resource_header_parses_page_map_and_prd0() {
        let mut head = vec![0u8; 0x80];
        head[8..16].copy_from_slice(&0x5000_0040u64.to_le_bytes());
        head[0x10..0x14].copy_from_slice(b"0DRP");
        head[0x14] = 0x7a;
        head[0x48] = 2;
        head[0x49] = 5;
        let h = ResourceHeader::parse(&head).unwrap();
        assert_eq!(h, ResourceHeader { page_map: 0x5000_0040, pages: Some((2, 5)), prd0_type: Some(0x7a) });
        assert_eq!(h.line(), "page map 0x50000040 · 2 system + 5 graphics page(s) · PRD0 type 0x7a");
        // not a resource: non-zero vtable or a pointer outside the system page
        head[0] = 1;
        assert!(ResourceHeader::parse(&head).is_none());
        assert!(ResourceHeader::parse(&[0u8; 8]).is_none());
    }

    #[test]
    fn markdown_is_compact_and_has_every_field() {
        let e = entry("crate_a.#dr", "MODEL");
        let mut a = AssetContext::from_entry(&e);
        a.header = Some(ResourceHeader { page_map: 0x5000_0040, pages: Some((1, 2)), prd0_type: None });
        a.decoded = Decoded::Model {
            verts: 1200,
            tris: 800,
            geoms: 3,
            meshlets: 9,
            lod: "LOD0".into(),
            lod_geoms: vec![3, 1, 0, 0],
            parts: 1,
            bones: 0,
            textures: vec![("diffuseTex".into(), true), ("normal_1a2b3c4d".into(), false)],
            shaders_binding: 2,
        };
        a.related = Some(vec![RelatedLine { path: "world.rpf/crate_a_hi.#dr".into(), kind: "MODEL", why: "detail-level file".into() }]);
        a.head64 = Some((0u8..80).collect());
        let md = to_markdown(&[a]);
        for want in [
            "### Asset `crate_a.#dr` (Percy6Stars)",
            "- archive: `world.rpf` (`/data/sample/world.rpf`)",
            "- path: `world.rpf/crate_a.#dr`",
            "- kind: MODEL, drawable `#dr` (GTA V .ydr)",
            "- size: 8.0 KB raw · 2.0 KB stored · offset 0x12345010",
            "- resource: page map 0x50000040 · 1 system + 2 graphics page(s)",
            "1200 verts · 800 tris · 3 geometries · 9 meshlets · showing LOD0 · LOD table LOD0=3 LOD1=1",
            "no embedded skeleton",
            "shaders bind 2 texture(s), 1 found in TTPF pools: diffuseTex, normal_1a2b3c4d (most shared one is used by 2 shaders)",
            "- related (1):\n  - `world.rpf/crate_a_hi.#dr` MODEL (detail-level file)",
            "- spec: GTA6-Asset-Info formats/02-resources.md",
        ] {
            assert!(md.contains(want), "missing {want:?} in\n{md}");
        }
        // the hex line stops at 64 bytes
        let hex = md.lines().find(|l| l.starts_with("- first")).unwrap();
        assert!(hex.starts_with("- first 64 bytes (decoded): `00 01 02"));
        assert!(hex.ends_with("3e 3f`"));
        assert!(md.len() < 1600, "context block grew to {} bytes", md.len());
    }

    #[test]
    fn markdown_states_missing_relations_honestly() {
        let e = entry("lonely.#dr", "MODEL");
        let mut a = AssetContext::from_entry(&e);
        assert!(to_markdown(&[a.clone()]).contains("- related: not checked"));
        a.related = Some(Vec::new());
        assert!(to_markdown(&[a.clone()]).contains("- related: none found by Percy's rules"));
        a.related = Some((0..30).map(|i| RelatedLine { path: format!("p{i}"), kind: "MODEL", why: "x".into() }).collect());
        let md = to_markdown(&[a]);
        assert!(md.contains("- related (30):"));
        assert!(md.contains("  - +5 more"));
        assert!(!md.contains("`p25`"));
    }

    #[test]
    fn markdown_numbers_several_assets() {
        let a = AssetContext::from_entry(&entry("one.#dr", "MODEL"));
        let mut b = AssetContext::from_entry(&entry("sound_0001_2st.awc", "AWC"));
        b.decoded = Decoded::Audio {
            codec: "Opus".into(),
            streams: vec![
                AudioStream { name: "stream_0".into(), channels: 1, sample_rate: 48000, duration_secs: 1.5 },
                AudioStream { name: "stream_1".into(), channels: 1, sample_rate: 48000, duration_secs: 2.0 },
            ],
            pack: Some("pack_a".into()),
            used_by: vec!["sound_x".into()],
        };
        let md = to_markdown(&[a, b]);
        assert!(md.starts_with("### 2 assets (Percy6Stars)"));
        assert!(md.contains("#### 1. `one.#dr`"));
        assert!(md.contains("#### 2. `sound_0001_2st.awc`"));
        assert!(md.contains("audio Opus · 2 stream(s) · 3.5s total · 1 ch @ 48000 Hz · streams: stream_0 (1.5s), stream_1 (2.0s) · pack pack_a · used by sound_x"));
        assert!(md.contains("structure/03-audio-awc.md"));
    }

    #[test]
    fn json_escapes_and_lists_fields() {
        let mut e = entry("we\"ird\\name.#dr", "MODEL");
        e.source_rpf = PathBuf::from("C:\\games\\world.rpf");
        let mut a = AssetContext::from_entry(&e);
        a.related = Some(vec![RelatedLine { path: "world.rpf/x".into(), kind: "TTPF", why: "texture".into() }]);
        let j = to_json(&[a.clone()]);
        assert!(j.starts_with('{') && j.ends_with('}'));
        assert!(j.contains("\"name\":\"we\\\"ird\\\\name.#dr\""));
        assert!(j.contains("\"archive_path\":\"C:\\\\games\\\\world.rpf\""));
        assert!(j.contains("\"related\":[{\"path\":\"world.rpf/x\",\"kind\":\"TTPF\",\"why\":\"texture\"}]"));
        assert!(j.contains("\"offset\":\"0x12345010\""));
        assert!(!j.contains("first_bytes_hex"));
        let many = to_json(&[a.clone(), a]);
        assert!(many.starts_with("[\n{") && many.ends_with("}\n]"));
        assert_eq!(json_str("a\u{1}b\n"), "\"a\\u0001b\\n\"");
    }

    #[test]
    fn every_kind_has_a_note_and_spec() {
        for k in ["MODEL", "VEH", "TTPF", "TXD", "AWC", "DAT", "ANIM", "NAVM", "META7A", "PSIN", "GXT", "RSC9", "BK2", "XML", "ZLIB", "RBF0", "GFX", "HMAP", "???"] {
            let (how, spec) = decode_note(k);
            assert!(!how.is_empty() && !spec.is_empty(), "{k}");
        }
        assert_eq!(kind_label("MODEL", "x.#ft"), "MODEL, fragment `#ft` (GTA V .yft)");
        assert_eq!(kind_label("NEW", "x"), "NEW");
    }
}
