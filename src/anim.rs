use std::fmt::Write as _;

fn rd32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

fn rd64(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3], d[o + 4], d[o + 5], d[o + 6], d[o + 7]])
}

fn vptr_off(ptr: u64) -> Option<usize> {
    if (0x5000_0000..0x5800_0000).contains(&ptr) {
        Some((ptr - 0x5000_0000) as usize)
    } else {
        None
    }
}

// detect rage 9 crClipDictionary header in decompressed rsc9 page
pub fn probe_anim_dict(data: &[u8]) -> Option<(String, u32)> {
    if data.len() < 0x50 {
        return None;
    }
    if rd64(data, 0) != 0 || vptr_off(rd64(data, 8)).is_none() {
        return None;
    }
    if &data[0x10..0x15] != b"0DRPz" {
        return None;
    }
    let count = rd32(data, 0x20);
    if count == 0 || count > 4096 {
        return None;
    }
    let p_hashes = vptr_off(rd64(data, 0x28))?;
    let p_anims = vptr_off(rd64(data, 0x38));
    if p_hashes == 0 && p_anims.is_none() {
        return None;
    }
    let first_hash = if p_hashes + 4 <= data.len() { rd32(data, p_hashes) } else { 0 };
    Some((format!("clipdict_{first_hash:08x}_{count}clips.ycd"), count))
}

// decode rage 9 crClipDictionary header hash table and anim entries
pub fn parse_anim_dict(data: &[u8]) -> String {
    if data.len() < 0x50 || &data[0x10..0x15] != b"0DRPz" {
        return String::from("Not a valid RAGE 9 crClipDictionary (0DRPz)");
    }
    let bmap = rd64(data, 8);
    let count = rd32(data, 0x20);
    let p_hashes = rd64(data, 0x28);
    let p_clips = rd64(data, 0x30);
    let p_anims = rd64(data, 0x38);
    let p_skel = rd64(data, 0x40);

    let mut out = String::with_capacity(1024);
    let _ = writeln!(
        out,
        "=== RAGE 9 crClipDictionary (ANIM) ===\nSize: {} B | BlockMap: 0x{:08x} | Clips: {}",
        data.len(),
        bmap,
        count
    );
    let _ = writeln!(
        out,
        "Tables: hashes=0x{:08x} clips=0x{:08x} anims=0x{:08x} skel=0x{:08x}\n",
        p_hashes, p_clips, p_anims, p_skel
    );

    if let Some(hoff) = vptr_off(p_hashes) {
        let _ = writeln!(out, "--- Clip Hash Entries ---");
        for idx in 0..(count as usize).min(64) {
            let e = hoff + idx * 32;
            if e + 32 > data.len() {
                break;
            }
            let chash = rd32(data, e);
            let flags = rd32(data, e + 8);
            let cptr = rd64(data, e + 16);
            let _ = writeln!(out, "  [{idx:02}] hash=0x{chash:08x} flags=0x{flags:04x} ptr=0x{cptr:08x}");
        }
    }

    if let Some(aoff) = vptr_off(p_anims) {
        let _ = writeln!(out, "\n--- Animation Blocks ---");
        for idx in 0..(count as usize).min(64) {
            let e = aoff + 16 + idx * 16;
            if e + 16 > data.len() {
                break;
            }
            let ahash = rd32(data, e);
            let asize = rd32(data, e + 4);
            let aptr = rd64(data, e + 8);
            let tracks = vptr_off(aptr)
                .filter(|&o| o + 32 <= data.len())
                .map(|o| {
                    let end = (o + asize as usize).min(data.len());
                    data[o..end].windows(4).filter(|w| w[0] == 0x09 && w[1] == 0x00).count()
                })
                .unwrap_or(0);
            let _ = writeln!(out, "  [{idx:02}] anim=0x{ahash:08x} block_sz=0x{asize:04x} ptr=0x{aptr:08x} tracks~={tracks}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_and_parses_synthetic_clip_dict() {
        let mut buf = vec![0u8; 0x100];
        buf[8..16].copy_from_slice(&0x5000_00c0u64.to_le_bytes());
        buf[0x10..0x15].copy_from_slice(b"0DRPz");
        buf[0x20..0x24].copy_from_slice(&2u32.to_le_bytes());
        buf[0x28..0x30].copy_from_slice(&0x5000_0060u64.to_le_bytes());
        buf[0x38..0x40].copy_from_slice(&0x5000_00a0u64.to_le_bytes());
        buf[0x60..0x64].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        let (name, cnt) = probe_anim_dict(&buf).expect("should detect clip dict");
        assert_eq!(cnt, 2);
        assert!(name.contains("deadbeef"));
        let txt = parse_anim_dict(&buf);
        assert!(txt.contains("Clips: 2"));
        assert!(txt.contains("0xdeadbeef"));
    }
}
