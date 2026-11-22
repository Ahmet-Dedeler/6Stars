// Percy6Stars by Ahmet Dedeler (https://github.com/Ahmet-Dedeler, https://ahmetdedeler.com)
// Fork of PercyStar by rpatty-dev (https://github.com/rpatty-dev/PercyStar).
// Source: https://github.com/Ahmet-Dedeler/Percy6Stars
// License: PercyStar Non-Commercial Attribution & Copyleft License v1.2 (see LICENSE)
// If you copy, vendor or port this file, keep this header and credit both projects in your README.
//! Opt-in format coverage audit over a scan of your own archives.
//!
//! Samples entries of every kind from the Percy6Stars scan cache and runs the same decoder the
//! UI uses on selection, reporting how many succeed per kind plus example failures.
//!
//!   PERCYSTAR_AUDIT=1 cargo test --release format_audit -- --ignored --nocapture
//!
//! `PERCYSTAR_AUDIT_N` sets samples per kind (default 60); `PERCYSTAR_AUDIT_CACHE` picks the cache file.

use crate::*;
use std::collections::BTreeMap;

fn check(e: &RpfEntry, tmp: &Path) -> Result<String, String> {
    match e.kind {
        "MODEL" | "VEH" => decode_rage9_model_from_rpf(&e.source_rpf, e.offset, &e.name).and_then(|m| {
            if m.tris.is_empty() { Err("0 triangles".into()) } else { Ok(format!("{} tris", m.tris.len())) }
        }),
        "NAVM" => tex::decode_navmesh_from_rpf(&e.source_rpf, e.offset, &e.name).map(|m| format!("{} tris", m.tris.len())),
        "TXD" => tex::decode_txd_from_rpf(&e.source_rpf, e.offset).and_then(|t| {
            if t.is_empty() { Err("0 textures".into()) } else { Ok(format!("{} textures", t.len())) }
        }),
        k if pedphys::is_kind(k) => pedphys::audit_decode(e),
        "TTPF" => decode_ttpf_texture(&e.source_rpf, e.tracks as usize).map(|t| match t.full_size {
            Some((fw, fh)) => format!("{}x{} reduced from {fw}x{fh}", t.w, t.h),
            None => format!("{}x{}", t.w, t.h),
        }),
        _ => {
            let data = read_entry_bytes(e).map_err(|err| format!("read: {err}"))?;
            match e.kind {
                "BK2" => {
                    if data.len() > 4 && &data[..2] == b"KB" { Ok(format!("{} bytes", data.len())) } else { Err(format!("bad magic {:02x?}", &data[..data.len().min(4)])) }
                }
                "AWC" => {
                    let dir = tmp.join(format!("awc_{}", e.offset));
                    let w = awc::decode_awc_bytes_to_wavs(&data, &dir, "s");
                    let _ = fs::remove_dir_all(&dir);
                    if w.is_empty() { Err("0 streams".into()) } else { Ok(format!("{} streams", w.len())) }
                }
                "RBF0" => tex::decode_rbf0(&data).map(|s| format!("{} chars", s.len())).ok_or_else(|| "rbf0 decode failed (hex fallback)".into()),
                "GXT" => tex::decode_0drpz_text_min(&data, 1).map(|s| format!("{} chars", s.len())).ok_or_else(|| "gxt decode failed (hex fallback)".into()),
                "GFX" => Ok(format!("{} chars", tex::inspect_gfx(&data).len())),
                "PSIN" => Ok(format!("{} chars", psin::parse_psin(&data).len())),
                "XML" => std::str::from_utf8(&data[..data.len().min(4096)]).map(|_| "utf8".into()).map_err(|_| "not utf8".into()),
                _ => Ok(format!("{} bytes (hex view)", data.len())),
            }
        }
    }
}

#[test]
#[ignore]
fn format_audit() {
    if std::env::var("PERCYSTAR_AUDIT").is_err() {
        return;
    }
    let per_kind: usize = std::env::var("PERCYSTAR_AUDIT_N").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
    let mut by_kind: BTreeMap<&'static str, Vec<RpfEntry>> = BTreeMap::new();
    // PERCYSTAR_AUDIT_CACHE overrides the cache file (e.g. an older scan_cache_vN.tsv)
    let cache = std::env::var("PERCYSTAR_AUDIT_CACHE").map(PathBuf::from).ok().or_else(cache_file_path).expect("cache path");
    for entries in load_disk_cache_from(&cache).into_values() {
        for e in entries {
            by_kind.entry(e.kind).or_default().push(e);
        }
    }
    let tmp = std::env::temp_dir().join("percy6stars_audit");
    let _ = fs::create_dir_all(&tmp);
    for (kind, list) in &by_kind {
        let step = (list.len() / per_kind).max(1);
        let (mut ok, mut n, mut reduced) = (0usize, 0usize, 0usize);
        let mut fails: BTreeMap<String, (usize, String)> = BTreeMap::new();
        let t0 = std::time::Instant::now();
        for e in list.iter().step_by(step).take(per_kind) {
            n += 1;
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(e, &tmp)))
                .unwrap_or_else(|p| Err(format!("PANIC {:?}", p.downcast_ref::<String>().cloned().or(p.downcast_ref::<&str>().map(|s| s.to_string())))));
            match r {
                Ok(msg) => {
                    ok += 1;
                    reduced += msg.contains("reduced") as usize;
                }
                Err(err) => {
                    let key: String = err.chars().take(70).collect();
                    let f = fails.entry(key).or_insert((0, format!("{}:{}@{}", e.rpf_name, e.name, e.offset)));
                    f.0 += 1;
                }
            }
        }
        println!("{kind:5} total={:7} sampled={n:3} ok={ok:3} ({:3.0}%) {:.1}s", list.len(), 100.0 * ok as f64 / n.max(1) as f64, t0.elapsed().as_secs_f32());
        if reduced > 0 {
            println!("        {reduced} of the ok entries decoded at reduced resolution");
        }
        for (err, (c, ex)) in &fails {
            println!("        {c:3}x {err}  e.g. {ex}");
        }
    }
}
