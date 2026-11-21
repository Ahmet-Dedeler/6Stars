use flate2::{Decompress, FlushDecompress};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const CMAPDATA_HASH: u32 = 0x62ca_d9f0;
pub const CENTITYDEF_HASH: u32 = 0x2231_3605;
pub const CMAPTYPES_HASH: u32 = 0x3ba1_fc1e;

#[derive(Clone, Debug)]
pub struct EntityPlacement {
    pub archetype_hash: u32,
    pub pos: [f32; 3],
    pub rot: [f32; 4],
    pub scale: [f32; 3],
    pub lod_dist: f32,
}

#[derive(Clone, Debug)]
pub struct MapSector {
    pub offset: u64,
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub entities: Vec<EntityPlacement>,
}

#[inline]
fn is_zlib(b: &[u8]) -> bool {
    b.len() >= 2 && b[0] == 0x78 && matches!(b[1], 0x01 | 0x5e | 0x9c | 0xda)
}

pub fn probe_maps_cli(rpf_path: &Path) {
    let Ok(mut f) = File::open(rpf_path) else {
        eprintln!("Failed to open {}", rpf_path.display());
        return;
    };
    let sz = f.metadata().map(|m| m.len()).unwrap_or(0);
    println!("Probing {} ({:.2} GB)...", rpf_path.display(), sz as f64 / 1e9);

    let chunk_sz = 64 * 1024 * 1024usize;
    let mut buf = vec![0u8; chunk_sz];
    let mut off = 0u64;
    let mut dec = Decompress::new(true);
    let mut out = vec![0u8; 4096];

    let mut psin_cnt = 0usize;
    let mut rsc_cnt = 0usize;
    let mut model_cnt = 0usize;
    let mut psin_structs: HashMap<u32, (usize, u64)> = HashMap::new();
    let mut rsc_sigs: HashMap<(u32, u32), (usize, u64)> = HashMap::new();

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
            // check direct psin at page start
            if is_zlib(&buf[pos..]) {
                dec.reset(true);
                let _ = dec.decompress(&buf[pos..(pos + 16384).min(read_n)], &mut out, FlushDecompress::None);
                let olen = dec.total_out() as usize;
                if olen >= 16 && &out[..4] == b"PSIN" {
                    psin_cnt += 1;
                    // find LSCH first struct hash if present in first 4kb
                    if let Some(lsch_p) = out[..olen].windows(4).position(|w| w == b"LSCH") {
                        if lsch_p + 12 <= olen {
                            let h = u32::from_le_bytes([out[lsch_p + 8], out[lsch_p + 9], out[lsch_p + 10], out[lsch_p + 11]]);
                            let e = psin_structs.entry(h).or_insert((0, off + pos as u64));
                            e.0 += 1;
                        }
                    }
                }
            }
            // check rage9 resource at pos + 0x10
            let p = pos + 0x10;
            if p + 8 <= read_n && is_zlib(&buf[p..]) {
                dec.reset(true);
                let _ = dec.decompress(&buf[p..(p + 16384).min(read_n)], &mut out, FlushDecompress::None);
                let olen = dec.total_out() as usize;
                if olen >= 0x80 {
                    let vft = u64::from_le_bytes(out[0..8].try_into().unwrap());
                    let bmap = u64::from_le_bytes(out[8..16].try_into().unwrap());
                    if vft == 0 && (0x5000_0000..0x5800_0000).contains(&bmap) {
                        rsc_cnt += 1;
                        let sig0 = u32::from_le_bytes(out[0x10..0x14].try_into().unwrap());
                        let sig1 = u32::from_le_bytes(out[0x14..0x18].try_into().unwrap());
                        let m60 = u64::from_le_bytes(out[0x60..0x68].try_into().unwrap());
                        if m60 == 0x1601 || (sig0 == 0 && sig1 == 0) {
                            model_cnt += 1;
                        } else if sig1 != 0x0001_0015 && &out[0x10..0x14] != b"0DRP" {
                            let e = rsc_sigs.entry((sig0, sig1)).or_insert((0, off + pos as u64));
                            e.0 += 1;
                        }
                    }
                }
            }
            pos += 0x1000;
        }
        off += read_n as u64;
        if (off / (1024 * 1024 * 1024)) != ((off - read_n as u64) / (1024 * 1024 * 1024)) {
            println!("  {:.1} / {:.1} GB | psin={psin_cnt} rsc={rsc_cnt} models={model_cnt}", off as f64 / 1e9, sz as f64 / 1e9);
        }
    }

    println!("Done: psin={psin_cnt}, rsc={rsc_cnt}, models={model_cnt}");
    let mut ps: Vec<_> = psin_structs.into_iter().collect();
    ps.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    println!("Top PSIN root hashes:");
    for (h, (cnt, first_off)) in ps.into_iter().take(15) {
        println!("  0x{h:08x}: {cnt} (first @ 0x{first_off:x})");
    }
    let mut rs: Vec<_> = rsc_sigs.into_iter().collect();
    rs.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    println!("Top non-model RSC sigs (u32@0x10, u32@0x14):");
    for ((s0, s1), (cnt, first_off)) in rs.into_iter().take(15) {
        println!("  (0x{s0:08x}, 0x{s1:08x}): {cnt} (first @ 0x{first_off:x})");
    }
}
