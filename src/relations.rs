// Percy6Stars by Ahmet Dedeler (https://github.com/Ahmet-Dedeler, https://ahmetdedeler.com)
// Fork of PercyStar by rpatty-dev (https://github.com/rpatty-dev/PercyStar).
// Source: https://github.com/Ahmet-Dedeler/Percy6Stars
// License: PercyStar Non-Commercial Attribution & Copyleft License v1.2 (see LICENSE)
// If you copy, vendor or port this file, keep this header and credit both projects in your README.
//! Related files: for one entry, the other entries that belong to the same asset.
//!
//! RPF9 archives have no readable directory tree (the TOC is encrypted, spec §1), so an asset
//! that the game splits over several files (detail levels, vehicle parts, a character's
//! pieces, its collision bounds, the textures its shaders bind) shows up as unrelated rows.
//! Every link here comes from a named rule that matches something concrete: a name pattern
//! that the existing viewers already rely on (LOD switching, vehicle parts), a hash the
//! decoders resolve (shader textures in the ttpf pools, joaat names), or the audio catalogue.
//! Each result carries its rule, so the UI can say why a file is listed, and no rule
//! guesses. When nothing matches, the answer is an honest empty list.

use crate::context_copy::{name_stem, raw_stem};
use crate::RpfEntry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Why an entry is related. The order is the display order and the dedup priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rule {
    DetailLevel,
    MapLod,
    VehicleConfig,
    VehiclePart,
    VehicleBody,
    PedEyes,
    PedPart,
    CollisionBounds,
    ShaderTexture,
    SameStem,
    ClipDictHash,
    AudioCatalog,
    AudioCatalogBank,
    AudioPack,
    NavNeighbor,
}

impl Rule {
    /// Group heading in the panel.
    pub fn title(self) -> &'static str {
        match self {
            Rule::DetailLevel => "Detail levels",
            Rule::MapLod => "Map LOD models",
            Rule::VehicleConfig => "Vehicle configuration parts",
            Rule::VehiclePart => "Vehicle parts",
            Rule::VehicleBody => "Vehicle body",
            Rule::PedEyes => "Eyes / head",
            Rule::PedPart => "Character parts",
            Rule::CollisionBounds => "Collision bounds",
            Rule::ShaderTexture => "Textures bound by shaders",
            Rule::SameStem => "Same name, other kinds",
            Rule::ClipDictHash => "Clip dictionaries",
            Rule::AudioCatalog => "Audio catalogue",
            Rule::AudioCatalogBank => "Banks named by this catalogue",
            Rule::AudioPack => "Same audio pack",
            Rule::NavNeighbor => "Neighbouring navmesh tiles",
        }
    }

    /// The rule in one sentence, shown as a tooltip on the group.
    pub fn explain(self) -> &'static str {
        match self {
            Rule::DetailLevel => "Same model stem with _hi / _mid / no suffix, same archive and extension. The LOD switch in the 3D header uses the same rule.",
            Rule::MapLod => "Same stem with a _lod / _slod / _slodN suffix in the same archive (map LOD chain).",
            Rule::VehicleConfig => "Listed in one of this vehicle's authored configurations (packs.rpf type 0x7a tables), resolved the same way as the Configuration picker.",
            Rule::VehiclePart => "Name starts with '<vehicle stem>_' in the same archive, one per part (the vehicle Parts picker uses the same rule).",
            Rule::VehicleBody => "A vehicle (VEH) in the same archive whose stem is a token prefix of this part's name.",
            Rule::PedEyes => "<head>_eyes_full and <head>_full in the same archive.",
            Rule::PedPart => "Character parts with a ped rig code (_mr1_, _fr1_, ...) and the same character prefix (cs_ / ig_ / hc_ / player_ names) in the same archive.",
            Rule::CollisionBounds => "char_bipedbounds_<male|female>_<garment stem> and the garment drawable with that stem.",
            Rule::ShaderTexture => "The model's shaders bind a texture whose 64-bit hash is found in a ttpf store; the 3D viewer textures the model from the same lookup.",
            Rule::SameStem => "Exactly the same name stem with a different kind (for example a texture dictionary named like the model).",
            Rule::ClipDictHash => "Clip dictionary named clipdict_<hash> where hash = joaat(model stem).",
            Rule::AudioCatalog => "The audio.rpf metadata (DAT) chunks of the archive whose catalogue resolved this bank's name.",
            Rule::AudioCatalogBank => "Sound banks in the same archive that this archive's catalogue named.",
            Rule::AudioPack => "Banks the audio catalogue puts in the same pack.",
            Rule::NavNeighbor => "Navmesh tiles one grid cell away (nav_<x>_<y> names).",
        }
    }
}

/// One related entry, with the concrete detail of the match ("_hi file", "bank 0x...").
#[derive(Clone, Debug, PartialEq)]
pub struct Related {
    pub entry: usize,
    pub rule: Rule,
    pub why: String,
}

/// Lookup tables over a scan, built once per scan version (a few hundred ms for a full
/// dump) so that finding an entry's relations is a handful of hash lookups.
#[derive(Default)]
pub struct RelationIndex {
    pub ver: u64,
    /// raw stem -> entries, for named kinds (models, dictionaries, text, ...).
    stems: HashMap<String, Vec<u32>>,
    /// Sorted keys of `stems`, for prefix searches.
    sorted: Vec<String>,
    archive_ids: HashMap<PathBuf, u32>,
    /// (archive id, texture index) -> TTPF entry.
    ttpf: HashMap<(u32, u32), u32>,
    /// joaat(model stem) -> models, for clip dictionaries named by hash.
    model_hash: HashMap<u32, Vec<u32>>,
    /// clipdict hash -> clip dictionaries.
    clip_hash: HashMap<u32, Vec<u32>>,
    nav_grid: HashMap<(u32, u32), Vec<u32>>,
    /// (archive id, pack name) -> banks.
    awc_pack: HashMap<(u32, String), Vec<u32>>,
    /// archive id -> DAT metadata chunks.
    dat_by_archive: HashMap<u32, Vec<u32>>,
    /// archive id -> banks the catalogue named.
    named_banks: HashMap<u32, Vec<u32>>,
}

/// Kinds whose names are real asset names (not generated like `tex_00001_...`).
fn named_kind(kind: &str, name: &str) -> bool {
    match kind {
        "MODEL" | "VEH" | "TXD" | "META7A" | "PSIN" | "GXT" | "XML" | "BK2" | "GFX" | "RBF0" => true,
        "ANIM" => !name.starts_with("clipdict_"),
        _ => false,
    }
}

fn is_model(kind: &str) -> bool {
    matches!(kind, "MODEL" | "VEH")
}

fn ext_of(name: &str) -> &str {
    name.rsplit_once('.').map_or("", |x| x.1)
}

/// `clipdict_f14a15be_1clips.ycd` -> 0xf14a15be.
fn clipdict_hash(name: &str) -> Option<u32> {
    let h = name.strip_prefix("clipdict_")?.get(..8)?;
    u32::from_str_radix(h, 16).ok()
}

/// `nav_123_045_(...)` -> (123, 45).
fn nav_cell(name: &str) -> Option<(u32, u32)> {
    let rest = name.strip_prefix("nav_")?;
    let mut it = rest.split('_');
    let x = it.next()?.parse().ok()?;
    let y = it.next()?.get(..3)?.parse().ok()?;
    Some((x, y))
}

const PED_RIGS: [&str; 6] = ["mr1", "fr1", "mb1", "fb1", "mo1", "fo1"];
const CHARACTER_PREFIXES: [&str; 4] = ["cs", "ig", "hc", "player"];

/// Character prefix of a ped part: the tokens before the component token that precedes the
/// rig code. `cs_ali_head_fr1_afr_000_m_full` -> `cs_ali`. `None` for generic parts
/// (`head_mr1_...`) and names without a rig code.
pub fn character_key(stem: &str) -> Option<String> {
    let toks: Vec<&str> = stem.split('_').collect();
    let rig = toks.iter().position(|t| PED_RIGS.contains(t))?;
    if rig < 3 || !CHARACTER_PREFIXES.contains(&toks[0]) {
        return None;
    }
    Some(toks[..rig - 1].join("_"))
}

fn has_rig(stem: &str) -> bool {
    stem.split('_').any(|t| PED_RIGS.contains(&t))
}

impl RelationIndex {
    pub fn build(entries: &[RpfEntry], audio_meta: &HashMap<u64, crate::dat::ResolvedAwcMeta>, ver: u64) -> Self {
        let mut ix = RelationIndex { ver, ..Default::default() };
        for (i, e) in entries.iter().enumerate() {
            let i = i as u32;
            let next = ix.archive_ids.len() as u32;
            let aid = match ix.archive_ids.get(&e.source_rpf) {
                Some(&a) => a,
                None => {
                    ix.archive_ids.insert(e.source_rpf.clone(), next);
                    next
                }
            };
            match e.kind {
                "TTPF" => {
                    ix.ttpf.insert((aid, e.tracks), i);
                }
                "ANIM" => {
                    if let Some(h) = clipdict_hash(&e.name) {
                        ix.clip_hash.entry(h).or_default().push(i);
                    }
                }
                "NAVM" => {
                    if let Some(c) = nav_cell(&e.name) {
                        ix.nav_grid.entry(c).or_default().push(i);
                    }
                }
                "DAT" => ix.dat_by_archive.entry(aid).or_default().push(i),
                "AWC" => {
                    if let Some(meta) = audio_meta.get(&e.offset).filter(|_| e.rpf_name.eq_ignore_ascii_case("audio.rpf")) {
                        ix.named_banks.entry(aid).or_default().push(i);
                        if !meta.pack_name.is_empty() {
                            ix.awc_pack.entry((aid, meta.pack_name.clone())).or_default().push(i);
                        }
                    }
                }
                _ => {}
            }
            if named_kind(e.kind, &e.name) {
                let stem = raw_stem(&e.name);
                if is_model(e.kind) {
                    ix.model_hash.entry(crate::psin::joaat(&name_stem(&e.name))).or_default().push(i);
                }
                ix.stems.entry(stem).or_default().push(i);
            }
        }
        ix.sorted = ix.stems.keys().cloned().collect();
        ix.sorted.sort_unstable();
        ix
    }

    fn stem(&self, s: &str) -> &[u32] {
        self.stems.get(s).map_or(&[], Vec::as_slice)
    }

    /// Stems that start with `prefix`, in sorted order.
    fn with_prefix<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        let start = self.sorted.partition_point(|s| s.as_str() < prefix);
        self.sorted[start..].iter().take_while(move |s| s.starts_with(prefix)).map(String::as_str)
    }

    fn archive_id(&self, p: &Path) -> Option<u32> {
        self.archive_ids.get(p).copied()
    }

    /// Every related entry for `idx`, grouped by rule (in `Rule` order), first rule wins
    /// when an entry matches several.
    pub fn find(&self, entries: &[RpfEntry], audio_meta: &HashMap<u64, crate::dat::ResolvedAwcMeta>, idx: usize) -> Vec<Related> {
        let Some(e) = entries.get(idx) else { return Vec::new() };
        let mut out: Vec<Related> = Vec::new();
        let mut seen: HashSet<usize> = HashSet::from([idx]);
        let same_archive = |j: u32| entries.get(j as usize).is_some_and(|o| o.source_rpf == e.source_rpf);
        let mut push = |out: &mut Vec<Related>, j: u32, rule: Rule, why: String| {
            if seen.insert(j as usize) {
                out.push(Related { entry: j as usize, rule, why });
            }
        };
        let stem = name_stem(&e.name);
        let raw = raw_stem(&e.name);

        if is_model(e.kind) {
            let ext = ext_of(&e.name);
            // detail-level files: stem_hi / stem_mid / stem (asm512's LOD switching)
            for (cand, label) in [(format!("{stem}_hi"), "_hi detail file"), (format!("{stem}_mid"), "_mid detail file"), (stem.clone(), "base detail file")] {
                for &j in self.stem(&cand) {
                    let o = &entries[j as usize];
                    if is_model(o.kind) && same_archive(j) && ext_of(&o.name) == ext {
                        push(&mut out, j, Rule::DetailLevel, label.into());
                    }
                }
            }
            // map LOD chain: base, base_lod, base_slod, base_slod1..4
            let lod_base = ["_lod", "_slod", "_slod1", "_slod2", "_slod3", "_slod4"]
                .iter()
                .find_map(|s| stem.strip_suffix(s))
                .unwrap_or(&stem)
                .to_string();
            for suf in ["", "_lod", "_slod", "_slod1", "_slod2", "_slod3", "_slod4"] {
                let cand = format!("{lod_base}{suf}");
                for &j in self.stem(&cand) {
                    if is_model(entries[j as usize].kind) && same_archive(j) {
                        let why = if suf.is_empty() { "full-detail model of this LOD".to_string() } else { format!("`{suf}` LOD model") };
                        push(&mut out, j, Rule::MapLod, why);
                    }
                }
            }
            if e.kind == "VEH" {
                // one candidate per logical part, preferring the same detail level (find_vehicle_parts)
                let prefix = format!("{stem}_");
                let want_hi = raw.ends_with("_hi");
                let mut parts: HashMap<String, (u32, bool)> = HashMap::new();
                for s in self.with_prefix(&prefix) {
                    if s == format!("{stem}_hi") || s == format!("{stem}_mid") {
                        continue;
                    }
                    for &j in self.stem(s) {
                        if !is_model(entries[j as usize].kind) || !same_archive(j) {
                            continue;
                        }
                        let logical = s.strip_suffix("_hi").or_else(|| s.strip_suffix("_mid")).unwrap_or(s).to_string();
                        let matches = s.ends_with("_hi") == want_hi;
                        parts
                            .entry(logical)
                            .and_modify(|slot| {
                                if matches && !slot.1 {
                                    *slot = (j, true);
                                }
                            })
                            .or_insert((j, matches));
                    }
                }
                let mut parts: Vec<(String, u32)> = parts.into_iter().map(|(k, v)| (k, v.0)).collect();
                parts.sort();
                for (logical, j) in parts {
                    let part = logical.strip_prefix(&prefix).unwrap_or(&logical).to_string();
                    push(&mut out, j, Rule::VehiclePart, format!("part `{part}`"));
                }
            } else {
                // the vehicle this part belongs to: longest token prefix that is a VEH stem
                let toks: Vec<&str> = stem.split('_').collect();
                'body: for n in (1..toks.len()).rev() {
                    let cand = toks[..n].join("_");
                    for &j in self.stem(&cand).iter().chain(self.stem(&format!("{cand}_hi"))) {
                        if entries[j as usize].kind == "VEH" && same_archive(j) {
                            push(&mut out, j, Rule::VehicleBody, format!("vehicle `{cand}`"));
                            break 'body;
                        }
                    }
                }
            }
            // eyes mesh <-> head
            let eyes_pair = if let Some(head) = stem.strip_suffix("_eyes_full") {
                Some((format!("{head}_full"), "head this eyes mesh belongs to"))
            } else {
                stem.strip_suffix("_full").map(|head| (format!("{head}_eyes_full"), "eyes mesh of this head"))
            };
            if let Some((cand, why)) = eyes_pair {
                for &j in self.stem(&cand) {
                    if is_model(entries[j as usize].kind) && same_archive(j) {
                        push(&mut out, j, Rule::PedEyes, why.into());
                    }
                }
            }
            // character parts sharing a character prefix
            if let Some(key) = character_key(&stem) {
                let toks: Vec<&str> = key.split('_').collect();
                let mut prefixes: Vec<String> = (2..=toks.len()).map(|n| format!("{}_", toks[..n].join("_"))).collect();
                prefixes.dedup();
                let mut found: Vec<(String, u32)> = Vec::new();
                for p in &prefixes {
                    for s in self.with_prefix(p) {
                        let Some(other) = character_key(s) else { continue };
                        let related = other == key || other.starts_with(&format!("{key}_")) || key.starts_with(&format!("{other}_"));
                        if !related || !has_rig(s) {
                            continue;
                        }
                        for &j in self.stem(s) {
                            if is_model(entries[j as usize].kind) && same_archive(j) {
                                found.push((s.to_string(), j));
                            }
                        }
                    }
                }
                found.sort();
                for (_, j) in found.into_iter().take(300) {
                    push(&mut out, j, Rule::PedPart, format!("character `{key}`"));
                }
            }
            // biped collision bounds <-> garment
            let garment = ["char_bipedbounds_male_", "char_bipedbounds_female_"].iter().find_map(|p| stem.strip_prefix(p));
            if let Some(g) = garment {
                for &j in self.stem(g) {
                    if is_model(entries[j as usize].kind) {
                        push(&mut out, j, Rule::CollisionBounds, "garment these bounds belong to".into());
                    }
                }
            } else {
                for p in ["char_bipedbounds_male_", "char_bipedbounds_female_"] {
                    for &j in self.stem(&format!("{p}{stem}")) {
                        push(&mut out, j, Rule::CollisionBounds, "biped collision bounds for this garment".into());
                    }
                }
            }
            // clip dictionaries named by hash
            for &j in self.clip_hash.get(&crate::psin::joaat(&stem)).map_or(&[][..], Vec::as_slice) {
                push(&mut out, j, Rule::ClipDictHash, format!("clipdict hash = joaat(\"{stem}\")"));
            }
        }

        if e.kind == "ANIM" {
            if let Some(h) = clipdict_hash(&e.name) {
                for &j in self.model_hash.get(&h).map_or(&[][..], Vec::as_slice) {
                    push(&mut out, j, Rule::ClipDictHash, format!("model whose joaat(stem) = 0x{h:08x}"));
                }
            }
        }

        // same name stem, different kind (any archive)
        if named_kind(e.kind, &e.name) {
            for cand in [raw.as_str(), stem.as_str()] {
                for &j in self.stem(cand) {
                    let o = &entries[j as usize];
                    if o.kind != e.kind && !(is_model(o.kind) && is_model(e.kind)) {
                        push(&mut out, j, Rule::SameStem, format!("{} named `{cand}`", o.kind));
                    }
                }
            }
        }

        if e.kind == "AWC" && e.rpf_name.eq_ignore_ascii_case("audio.rpf") {
            if let (Some(meta), Some(aid)) = (audio_meta.get(&e.offset), self.archive_id(&e.source_rpf)) {
                for &j in self.dat_by_archive.get(&aid).map_or(&[][..], Vec::as_slice) {
                    push(&mut out, j, Rule::AudioCatalog, format!("catalogue that names bank 0x{:08x}", meta.bank_hash));
                }
                if !meta.pack_name.is_empty() {
                    let banks = self.awc_pack.get(&(aid, meta.pack_name.clone())).map_or(&[][..], Vec::as_slice);
                    for &j in banks.iter().take(200) {
                        push(&mut out, j, Rule::AudioPack, format!("pack `{}`", meta.pack_name));
                    }
                }
            }
        }
        if e.kind == "DAT" {
            if let Some(aid) = self.archive_id(&e.source_rpf) {
                for &j in self.named_banks.get(&aid).map_or(&[][..], Vec::as_slice).iter().take(200) {
                    push(&mut out, j, Rule::AudioCatalogBank, "bank named by this catalogue".into());
                }
            }
        }

        if e.kind == "NAVM" {
            if let Some((x, y)) = nav_cell(&e.name) {
                for (dx, dy, dir) in [(0i32, 1i32, "north"), (1, 0, "east"), (0, -1, "south"), (-1, 0, "west"), (1, 1, "north-east"), (-1, 1, "north-west"), (1, -1, "south-east"), (-1, -1, "south-west")] {
                    let (nx, ny) = (x as i64 + dx as i64, y as i64 + dy as i64);
                    if nx < 0 || ny < 0 {
                        continue;
                    }
                    for &j in self.nav_grid.get(&(nx as u32, ny as u32)).map_or(&[][..], Vec::as_slice) {
                        push(&mut out, j, Rule::NavNeighbor, format!("tile to the {dir}"));
                    }
                }
            }
        }

        out.sort_by_key(|r| r.rule);
        out
    }

    /// Shader texture references (role, ttpf store path, texture index) mapped to entries.
    /// References whose store or index isn't in the scan are left out.
    pub fn texture_relations(&self, refs: &[(String, PathBuf, usize)]) -> Vec<Related> {
        let mut out: Vec<Related> = Vec::new();
        let mut seen = HashSet::new();
        for (role, path, tex_idx) in refs {
            let Some(aid) = self.archive_id(path) else { continue };
            if let Some(&j) = self.ttpf.get(&(aid, *tex_idx as u32)) {
                if seen.insert(j) {
                    out.push(Related { entry: j as usize, rule: Rule::ShaderTexture, why: format!("bound as `{role}`") });
                }
            }
        }
        out
    }
}

/// Merges extra relations (shader textures, vehicle configurations) into `base`, keeping
/// rule order and the first rule per entry.
pub fn merge(base: &mut Vec<Related>, extra: Vec<Related>, self_idx: usize) {
    let mut seen: HashSet<usize> = base.iter().map(|r| r.entry).collect();
    seen.insert(self_idx);
    for r in extra {
        if seen.insert(r.entry) {
            base.push(r);
        }
    }
    base.sort_by_key(|r| r.rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(archive: &str, name: &str, kind: &'static str, offset: u64, tracks: u32) -> RpfEntry {
        RpfEntry {
            source_rpf: PathBuf::from(format!("/dump/{archive}")),
            rpf_name: archive.into(),
            name: name.into(),
            kind,
            offset,
            comp_size: 100,
            raw_size: 100,
            tracks,
            chunks: Vec::new(),
        }
    }

    fn find_names(entries: &[RpfEntry], meta: &HashMap<u64, crate::dat::ResolvedAwcMeta>, name: &str) -> Vec<(String, Rule)> {
        let ix = RelationIndex::build(entries, meta, 1);
        let idx = entries.iter().position(|e| e.name == name).unwrap();
        ix.find(entries, meta, idx).into_iter().map(|r| (entries[r.entry].name.clone(), r.rule)).collect()
    }

    #[test]
    fn detail_levels_and_map_lods_stay_in_one_archive() {
        let es = vec![
            ent("a.rpf", "prop_crate.#dr", "MODEL", 0, 0),
            ent("a.rpf", "prop_crate_hi.#dr", "MODEL", 1, 0),
            ent("a.rpf", "prop_crate_mid.#dr", "MODEL", 2, 0),
            ent("b.rpf", "prop_crate_hi.#dr", "MODEL", 3, 0),
            ent("a.rpf", "prop_crate_lod.#dr", "MODEL", 4, 0),
            ent("a.rpf", "prop_crate_slod1.#dr", "MODEL", 5, 0),
            ent("a.rpf", "prop_crate_hi.#ft", "MODEL", 6, 0),
            ent("a.rpf", "prop_cratebox.#dr", "MODEL", 7, 0),
        ];
        let m = HashMap::new();
        let r = find_names(&es, &m, "prop_crate.#dr");
        assert_eq!(
            r,
            vec![
                ("prop_crate_hi.#dr".into(), Rule::DetailLevel),
                ("prop_crate_mid.#dr".into(), Rule::DetailLevel),
                ("prop_crate_lod.#dr".into(), Rule::MapLod),
                ("prop_crate_slod1.#dr".into(), Rule::MapLod),
            ]
        );
        // from the LOD side, the chain points back at the full model and the other LOD
        let r = find_names(&es, &m, "prop_crate_slod1.#dr");
        assert!(r.contains(&("prop_crate.#dr".into(), Rule::MapLod)));
        assert!(r.contains(&("prop_crate_lod.#dr".into(), Rule::MapLod)));
    }

    #[test]
    fn vehicle_parts_and_body() {
        let es = vec![
            ent("v.rpf", "speedo_hi.#ft", "VEH", 0, 0),
            ent("v.rpf", "speedo.#ft", "VEH", 1, 0),
            ent("v.rpf", "speedo_wheel_hi.#dr", "MODEL", 2, 0),
            ent("v.rpf", "speedo_wheel.#dr", "MODEL", 3, 0),
            ent("v.rpf", "speedo_bumper_f_01.#dr", "MODEL", 4, 0),
            ent("w.rpf", "speedo_spoiler.#dr", "MODEL", 5, 0),
            ent("v.rpf", "speedometer.#dr", "MODEL", 6, 0),
        ];
        let m = HashMap::new();
        let r = find_names(&es, &m, "speedo_hi.#ft");
        assert_eq!(
            r,
            vec![
                ("speedo.#ft".into(), Rule::DetailLevel),
                ("speedo_bumper_f_01.#dr".into(), Rule::VehiclePart),
                ("speedo_wheel_hi.#dr".into(), Rule::VehiclePart),
            ]
        );
        let r = find_names(&es, &m, "speedo_bumper_f_01.#dr");
        assert_eq!(r, vec![("speedo.#ft".into(), Rule::VehicleBody)]);
        // a different archive or a longer word is not a part
        assert!(find_names(&es, &m, "speedometer.#dr").is_empty());
        assert!(find_names(&es, &m, "speedo_spoiler.#dr").is_empty());
    }

    #[test]
    fn character_parts_eyes_and_bounds() {
        let es = vec![
            ent("p.rpf", "cs_kim_head_fr1_asi_000_m_full.#dr", "MODEL", 0, 0),
            ent("p.rpf", "cs_kim_head_fr1_asi_000_m_eyes_full.#dr", "MODEL", 1, 0),
            ent("p.rpf", "cs_kim_hairbob_fr1_000_curly_full.#dr", "MODEL", 2, 0),
            ent("p.rpf", "cs_kimberly_hairbob_fr1_000_full.#dr", "MODEL", 3, 0),
            ent("p.rpf", "cs_kim_doe_beard_mr1_000_full.#dr", "MODEL", 4, 0),
            ent("p.rpf", "head_fr1_asi_000_m_full.#dr", "MODEL", 5, 0),
            ent("p.rpf", "hoodiezip_fr1_002_regular_full.#dr", "MODEL", 6, 0),
            ent("p.rpf", "char_bipedbounds_female_hoodiezip_fr1_002_regular_full.#ft", "MODEL", 7, 0),
        ];
        let m = HashMap::new();
        let r = find_names(&es, &m, "cs_kim_head_fr1_asi_000_m_full.#dr");
        assert_eq!(
            r,
            vec![
                ("cs_kim_head_fr1_asi_000_m_eyes_full.#dr".into(), Rule::PedEyes),
                ("cs_kim_doe_beard_mr1_000_full.#dr".into(), Rule::PedPart),
                ("cs_kim_hairbob_fr1_000_curly_full.#dr".into(), Rule::PedPart),
            ]
        );
        assert_eq!(character_key("cs_kim_head_fr1_asi_000_m_full"), Some("cs_kim".into()));
        assert_eq!(character_key("head_fr1_asi_000_m_full"), None);
        assert_eq!(
            find_names(&es, &m, "hoodiezip_fr1_002_regular_full.#dr"),
            vec![("char_bipedbounds_female_hoodiezip_fr1_002_regular_full.#ft".into(), Rule::CollisionBounds)]
        );
        assert_eq!(
            find_names(&es, &m, "char_bipedbounds_female_hoodiezip_fr1_002_regular_full.#ft"),
            vec![("hoodiezip_fr1_002_regular_full.#dr".into(), Rule::CollisionBounds)]
        );
    }

    #[test]
    fn hashes_textures_and_same_stem() {
        let h = crate::psin::joaat("prop_lamp");
        let es = vec![
            ent("a.rpf", "prop_lamp.#dr", "MODEL", 0, 0),
            ent("a.rpf", &format!("clipdict_{h:08x}_2clips.ycd"), "ANIM", 1, 2),
            ent("a.rpf", "clipdict_00000000_2clips.ycd", "ANIM", 2, 2),
            ent("t.rpf", "prop_lamp.#td", "TXD", 3, 4),
            ent("pool.ttpf", "tex_00007_64x64_BC1_UNORM.ttpf", "TTPF", 4, 7),
        ];
        let m = HashMap::new();
        let r = find_names(&es, &m, "prop_lamp.#dr");
        assert_eq!(r, vec![("prop_lamp.#td".into(), Rule::SameStem), (es[1].name.clone(), Rule::ClipDictHash)]);
        assert_eq!(find_names(&es, &m, &es[1].name.clone()), vec![("prop_lamp.#dr".into(), Rule::ClipDictHash)]);
        assert!(find_names(&es, &m, "clipdict_00000000_2clips.ycd").is_empty());
        let ix = RelationIndex::build(&es, &m, 1);
        let refs = vec![
            ("diffuseTex".to_string(), PathBuf::from("/dump/pool.ttpf"), 7usize),
            ("normal".to_string(), PathBuf::from("/dump/pool.ttpf"), 8usize),
            ("other".to_string(), PathBuf::from("/elsewhere.ttpf"), 7usize),
        ];
        let t = ix.texture_relations(&refs);
        assert_eq!(t, vec![Related { entry: 4, rule: Rule::ShaderTexture, why: "bound as `diffuseTex`".into() }]);
        let mut base = ix.find(&es, &m, 0);
        merge(&mut base, t, 0);
        assert_eq!(base.iter().map(|r| r.rule).collect::<Vec<_>>(), vec![Rule::ShaderTexture, Rule::SameStem, Rule::ClipDictHash]);
    }

    #[test]
    fn audio_catalogue_and_navmesh_neighbours() {
        let es = vec![
            ent("audio.rpf", "0003_audioconfig_dat26.rel", "DAT", 10, 0),
            ent("audio.rpf", "amb_beach_a.awc", "AWC", 100, 2),
            ent("audio.rpf", "amb_beach_b.awc", "AWC", 200, 2),
            ent("audio.rpf", "music_x.awc", "AWC", 300, 2),
            ent("audio.rpf", "sound_0009_1st.awc", "AWC", 400, 1),
            ent("nav.rpf", "nav_010_020_(0,0).ynv", "NAVM", 1, 0),
            ent("nav.rpf", "nav_011_020_(0,0).ynv", "NAVM", 2, 0),
            ent("nav.rpf", "nav_009_019_(0,0).ynv", "NAVM", 3, 0),
            ent("nav.rpf", "nav_012_020_(0,0).ynv", "NAVM", 4, 0),
        ];
        let meta = |name: &str, pack: &str, hash: u32| crate::dat::ResolvedAwcMeta {
            name: name.into(),
            exact_size: 0,
            bank_hash: hash,
            pack_name: pack.into(),
            used_by: Vec::new(),
        };
        let m: HashMap<u64, crate::dat::ResolvedAwcMeta> = HashMap::from([
            (100, meta("amb_beach_a", "ambience_beach", 0xaaaa)),
            (200, meta("amb_beach_b", "ambience_beach", 0xbbbb)),
            (300, meta("music_x", "music", 0xcccc)),
        ]);
        assert_eq!(
            find_names(&es, &m, "amb_beach_a.awc"),
            vec![("0003_audioconfig_dat26.rel".into(), Rule::AudioCatalog), ("amb_beach_b.awc".into(), Rule::AudioPack)]
        );
        // a bank the catalogue didn't name gets nothing
        assert!(find_names(&es, &m, "sound_0009_1st.awc").is_empty());
        let dat = find_names(&es, &m, "0003_audioconfig_dat26.rel");
        assert_eq!(dat.len(), 3);
        assert!(dat.iter().all(|r| r.1 == Rule::AudioCatalogBank));
        assert_eq!(
            find_names(&es, &m, "nav_010_020_(0,0).ynv"),
            vec![("nav_011_020_(0,0).ynv".into(), Rule::NavNeighbor), ("nav_009_019_(0,0).ynv".into(), Rule::NavNeighbor)]
        );
    }

    #[test]
    fn unrelated_entry_gets_an_empty_list() {
        let es = vec![ent("a.rpf", "lonely_thing.#dr", "MODEL", 0, 0), ent("a.rpf", "other_thing.#dr", "MODEL", 1, 0)];
        assert!(find_names(&es, &HashMap::new(), "lonely_thing.#dr").is_empty());
        let ix = RelationIndex::build(&es, &HashMap::new(), 1);
        assert!(ix.find(&es, &HashMap::new(), 99).is_empty());
    }

    /// Runs the relation finder over N real entries of the scan cache and reports how many
    /// have at least one related file, per kind and per rule.
    ///   PERCYSTAR_RELATIONS=1 [PERCYSTAR_AUDIT_CACHE=<tsv>] [PERCYSTAR_AUDIT_N=20000] cargo test relations_coverage -- --ignored --nocapture
    #[test]
    #[ignore]
    fn relations_coverage() {
        if std::env::var_os("PERCYSTAR_RELATIONS").is_none() {
            eprintln!("set PERCYSTAR_RELATIONS=1 to run");
            return;
        }
        let cache = std::env::var_os("PERCYSTAR_AUDIT_CACHE").map(PathBuf::from).or_else(crate::cache_file_path).expect("cache path");
        let map = crate::load_disk_cache_from(&cache);
        let mut entries: Vec<RpfEntry> = Vec::new();
        let mut audio_meta = HashMap::new();
        for mut list in map.into_values() {
            // name the audio.rpf banks from its catalogue, as the scan does
            if let Some(p) = list.first().filter(|e| e.rpf_name.eq_ignore_ascii_case("audio.rpf")).map(|e| e.source_rpf.clone()) {
                if let Ok(md) = std::fs::metadata(&p) {
                    audio_meta = crate::enrich_audio_rpf_entries(&p, md.len(), &mut list);
                }
            }
            entries.extend(list);
        }
        entries.sort_by(|a, b| (&a.source_rpf, a.offset).cmp(&(&b.source_rpf, b.offset)));
        let n: usize = std::env::var("PERCYSTAR_AUDIT_N").ok().and_then(|s| s.parse().ok()).unwrap_or(20_000);
        let t = std::time::Instant::now();
        let ix = RelationIndex::build(&entries, &audio_meta, 1);
        eprintln!("index over {} entries built in {:?}", entries.len(), t.elapsed());
        let step = (entries.len() / n.max(1)).max(1);
        let mut per_kind: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
        let mut per_rule: std::collections::BTreeMap<String, usize> = Default::default();
        let mut examples: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        let t = std::time::Instant::now();
        let mut queried = 0;
        for i in (0..entries.len()).step_by(step) {
            let r = ix.find(&entries, &audio_meta, i);
            let slot = per_kind.entry(entries[i].kind).or_default();
            slot.0 += 1;
            if !r.is_empty() {
                slot.1 += 1;
            }
            for x in &r {
                let ex = examples.entry(format!("{:?}", x.rule)).or_default();
                if ex.len() < 3 && std::env::var_os("PERCYSTAR_RELATIONS_EXAMPLES").is_some() {
                    ex.push(format!("{} -> {} ({})", entries[i].name, entries[x.entry].name, x.why));
                }
            }
            let mut rules: Vec<Rule> = r.iter().map(|x| x.rule).collect();
            rules.dedup();
            for rule in rules {
                *per_rule.entry(format!("{rule:?}")).or_default() += 1;
            }
            queried += 1;
        }
        eprintln!("{queried} queries in {:?}", t.elapsed());
        for (k, (total, hit)) in &per_kind {
            eprintln!("{k:>7}: {hit:>6} / {total:>6} have related files ({:.1}%)", *hit as f64 * 100.0 / *total as f64);
        }
        for (rule, c) in &per_rule {
            eprintln!("  rule {rule}: {c}");
            for ex in examples.get(rule).into_iter().flatten() {
                eprintln!("      {ex}");
            }
        }
    }
}
