use crate::model3d::{extract_rage9_model_name, has_rage9_meshlets, read_virtual_from_file};
use crate::psin::joaat;
use flate2::{Decompress, FlushDecompress};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const ENTITY_STRUCT_HASH: u32 = 0xce50_1483;
pub const MAPDATA_STRUCT_HASH: u32 = 0xd359_3fa6;

#[derive(Clone, Copy, Debug)]
pub struct EntityPlacement {
    pub archetype_hash: u32,
    pub pos: [f32; 3],
    pub rot: [f32; 4], // world-space quaternion (x, y, z, w)
    pub scale_xy: f32,
    pub scale_z: f32,
}

#[derive(Default)]
pub struct WorldIndex {
    pub map_count: usize,
    pub models: HashMap<u32, u64>,
    pub entities: Vec<EntityPlacement>,
}

#[inline]
fn is_zlib(b: &[u8]) -> bool {
    b.len() >= 2 && b[0] == 0x78 && matches!(b[1], 0x01 | 0x5e | 0x9c | 0xda)
}
#[inline]
fn ru16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
fn ru32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
#[inline]
fn ru64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7]])
}
#[inline]
fn rf32(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
#[inline]
fn vptr(p: u64) -> Option<usize> {
    ((p >> 28) == 5).then_some((p & 0x0FFF_FFFF) as usize)
}

// decode 0DRPz (0x7a) map placement resource into world-space EntityPlacements
// layout credited to @holdmysocks (SixNG meta7a.rs)
pub fn parse_0drpz_map_entities(sys: &[u8]) -> Option<Vec<EntityPlacement>> {
    if sys.len() < 0x50 || sys.get(0x10..0x15) != Some(b"0DRPz") {
        return None;
    }
    let nbl = ru16(sys, 0x4C) as usize;
    let it = vptr(ru64(sys, 0x38))?;
    if it + nbl * 16 > sys.len() {
        return None;
    }

    let mut out = Vec::new();
    let mut has_map_root = false;
    for i in 0..nbl {
        let o = it + i * 16;
        let hash = ru32(sys, o);
        let sz = ru32(sys, o + 4) as usize;
        if hash == MAPDATA_STRUCT_HASH {
            has_map_root = true;
        }
        if hash != ENTITY_STRUCT_HASH || sz < 0x90 {
            continue;
        }
        let Some(base) = vptr(ru64(sys, o + 8)) else { continue };
        let end = base.saturating_add(sz).min(sys.len());
        let mut cur = base;
        while cur + 0x90 <= end {
            let arch = ru32(sys, cur + 0x44);
            let pos = [rf32(sys, cur + 0x38), rf32(sys, cur + 0x3C), rf32(sys, cur + 0x40)];
            // stored quaternion is inverse rotation; conjugate gives world rotation
            let rot = [
                -rf32(sys, cur + 0x10),
                -rf32(sys, cur + 0x14),
                -rf32(sys, cur + 0x18),
                rf32(sys, cur + 0x1C),
            ];
            let sxy = rf32(sys, cur + 0x54);
            let sz_s = rf32(sys, cur + 0x58);
            if pos.iter().all(|v| v.is_finite() && v.abs() < 30_000.0) && rot.iter().all(|v| v.is_finite()) {
                out.push(EntityPlacement {
                    archetype_hash: arch,
                    pos,
                    rot,
                    scale_xy: if sxy.is_finite() && sxy > 0.01 && sxy < 100.0 { sxy } else { 1.0 },
                    scale_z: if sz_s.is_finite() && sz_s > 0.01 && sz_s < 100.0 { sz_s } else { 1.0 },
                });
            }
            cur += 0x90;
        }
    }
    if has_map_root || !out.is_empty() {
        Some(out)
    } else {
        None
    }
}

fn cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    Some(base.join("PercyStar").join("world_index_v1.bin"))
}

fn save_world_index(path: &Path, rpf_len: u64, ix: &WorldIndex) {
    if let Some(par) = path.parent() {
        let _ = fs::create_dir_all(par);
    }
    let Ok(f) = File::create(path) else { return };
    let mut w = BufWriter::new(f);
    let _ = w.write_all(b"PSW1");
    let _ = w.write_all(&rpf_len.to_le_bytes());
    let _ = w.write_all(&(ix.map_count as u64).to_le_bytes());
    let _ = w.write_all(&(ix.models.len() as u64).to_le_bytes());
    let _ = w.write_all(&(ix.entities.len() as u64).to_le_bytes());
    for (&h, &off) in &ix.models {
        let _ = w.write_all(&h.to_le_bytes());
        let _ = w.write_all(&off.to_le_bytes());
    }
    for e in &ix.entities {
        let _ = w.write_all(&e.archetype_hash.to_le_bytes());
        for v in e.pos { let _ = w.write_all(&v.to_le_bytes()); }
        for v in e.rot { let _ = w.write_all(&v.to_le_bytes()); }
        let _ = w.write_all(&e.scale_xy.to_le_bytes());
        let _ = w.write_all(&e.scale_z.to_le_bytes());
    }
    let _ = w.flush();
}

fn load_world_index(path: &Path, rpf_len: u64) -> Option<WorldIndex> {
    let mut r = BufReader::new(File::open(path).ok()?);
    let mut hdr = [0u8; 36];
    r.read_exact(&mut hdr).ok()?;
    if &hdr[..4] != b"PSW1" || ru64(&hdr, 4) != rpf_len {
        return None;
    }
    let map_count = ru64(&hdr, 12) as usize;
    let n_models = ru64(&hdr, 20) as usize;
    let n_ents = ru64(&hdr, 28) as usize;
    let mut models = HashMap::with_capacity(n_models);
    let mut mbuf = [0u8; 12];
    for _ in 0..n_models {
        r.read_exact(&mut mbuf).ok()?;
        models.insert(ru32(&mbuf, 0), ru64(&mbuf, 4));
    }
    let mut entities = Vec::with_capacity(n_ents);
    let mut ebuf = [0u8; 40];
    for _ in 0..n_ents {
        r.read_exact(&mut ebuf).ok()?;
        entities.push(EntityPlacement {
            archetype_hash: ru32(&ebuf, 0),
            pos: [rf32(&ebuf, 4), rf32(&ebuf, 8), rf32(&ebuf, 12)],
            rot: [rf32(&ebuf, 16), rf32(&ebuf, 20), rf32(&ebuf, 24), rf32(&ebuf, 28)],
            scale_xy: rf32(&ebuf, 32),
            scale_z: rf32(&ebuf, 36),
        });
    }
    Some(WorldIndex { map_count, models, entities })
}

pub fn build_or_load_world_index(levels_rpf: &Path) -> WorldIndex {
    let Ok(mut f) = File::open(levels_rpf) else { return WorldIndex::default() };
    let sz = f.metadata().map(|m| m.len()).unwrap_or(0);
    if let Some(cp) = cache_path() {
        if let Some(ix) = load_world_index(&cp, sz) {
            return ix;
        }
    }

    let mut ix = WorldIndex::default();
    let chunk_sz = 64 * 1024 * 1024usize;
    let mut buf = vec![0u8; chunk_sz];
    let mut off = 0u64;
    let mut dec = Decompress::new(true);
    let mut out = vec![0u8; 32768];
    let mut f_res = File::open(levels_rpf).unwrap();

    while off < sz {
        let _ = f.seek(SeekFrom::Start(off));
        let mut read_n = 0usize;
        while read_n < chunk_sz {
            match f.read(&mut buf[read_n..]) {
                Ok(0) => break,
                Ok(n) => read_n += n,
                Err(_) => break,
            }
        }
        if read_n < 64 {
            break;
        }
        let mut pos = 0usize;
        while pos + 0x20 <= read_n {
            let p = pos + 0x10;
            if is_zlib(&buf[p..]) {
                dec.reset(true);
                let _ = dec.decompress(&buf[p..(p + 32768).min(read_n)], &mut out, FlushDecompress::None);
                let olen = dec.total_out() as usize;
                if olen >= 0x80 {
                    let vft = ru64(&out, 0);
                    let bmap = ru64(&out, 8);
                    if vft == 0 && (0x5000_0000..0x5800_0000).contains(&bmap) {
                        let abs_off = off + p as u64;
                        if out.get(0x10..0x15) == Some(b"0DRPz") {
                            if let Ok((sys, _, _)) = read_virtual_from_file(&mut f_res, abs_off) {
                                if let Some(ents) = parse_0drpz_map_entities(&sys) {
                                    ix.map_count += 1;
                                    ix.entities.extend(ents);
                                }
                            }
                        } else if has_rage9_meshlets(&out[..olen]) {
                            if let Some((nm, ext)) = extract_rage9_model_name(&out[..olen]) {
                                let stem = nm.strip_suffix(ext).unwrap_or(&nm);
                                ix.models.entry(joaat(stem)).or_insert(abs_off);
                            }
                        }
                    }
                }
            }
            pos += 0x1000;
        }
        off += read_n as u64;
    }

    if let Some(cp) = cache_path() {
        save_world_index(&cp, sz, &ix);
    }
    ix
}

pub fn probe_maps_cli(rpf_path: &Path) {
    let ix = build_or_load_world_index(rpf_path);
    println!("WorldIndex: {} maps, {} models, {} entity placements", ix.map_count, ix.models.len(), ix.entities.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synthetic_0drpz_map_entities() {
        let mut sys = vec![0u8; 0x180];
        sys[0x10..0x15].copy_from_slice(b"0DRPz");
        sys[0x4C..0x4E].copy_from_slice(&2u16.to_le_bytes()); // 2 blocks
        sys[0x38..0x40].copy_from_slice(&(0x5000_0080u64).to_le_bytes());
        // block 0: MAPDATA
        sys[0x80..0x84].copy_from_slice(&MAPDATA_STRUCT_HASH.to_le_bytes());
        // block 1: ENTITY (1 * 0x90) at 0x5000_00E0
        sys[0x90..0x94].copy_from_slice(&ENTITY_STRUCT_HASH.to_le_bytes());
        sys[0x94..0x98].copy_from_slice(&0x90u32.to_le_bytes());
        sys[0x98..0xA0].copy_from_slice(&(0x5000_00E0u64).to_le_bytes());
        let e = 0xE0;
        sys[e + 0x44..e + 0x48].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        sys[e + 0x38..e + 0x3C].copy_from_slice(&(-2351.0f32).to_le_bytes());
        sys[e + 0x3C..e + 0x40].copy_from_slice(&(-5533.0f32).to_le_bytes());
        sys[e + 0x40..e + 0x44].copy_from_slice(&8.0f32.to_le_bytes());
        sys[e + 0x1C..e + 0x20].copy_from_slice(&1.0f32.to_le_bytes());
        sys[e + 0x54..e + 0x58].copy_from_slice(&1.5f32.to_le_bytes());
        sys[e + 0x58..e + 0x5C].copy_from_slice(&2.0f32.to_le_bytes());

        let ents = parse_0drpz_map_entities(&sys).unwrap();
        assert_eq!(ents.len(), 1);
        assert_eq!(ents[0].archetype_hash, 0x1234_5678);
        assert_eq!(ents[0].pos, [-2351.0, -5533.0, 8.0]);
        assert_eq!(ents[0].scale_xy, 1.5);
        assert_eq!(ents[0].scale_z, 2.0);
    }
}
