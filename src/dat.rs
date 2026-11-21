use std::fmt::Write as _;

fn rd32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

// detect rage 9 audio metadata rel container (dat25 dat26 dat41 dat61)
pub fn probe_dat(head: &[u8]) -> Option<u32> {
    if head.len() < 16 {
        return None;
    }
    let ver = rd32(head, 0);
    let data_sz = rd32(head, 4);
    let ts = rd32(head, 8);
    if matches!(ver, 16 | 19 | 25 | 26 | 29 | 41 | 61 | 151)
        && (64..=64 * 1024 * 1024).contains(&data_sz)
        && ts != 0
        && ts != 0xFFFF_FFFF
    {
        Some(ver)
    } else {
        None
    }
}

// decode rage 9 audMetadata rel header hash index table and embedded paths
pub fn parse_dat(data: &[u8]) -> String {
    if data.len() < 16 {
        return String::from("Truncated DAT audio metadata");
    }
    let ver = rd32(data, 0);
    let data_sz = rd32(data, 4) as usize;
    let ts = rd32(data, 8);
    let flags = rd32(data, 12);

    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "=== RAGE 9 Audio Metadata (DAT{ver}) ===\nTotal: {} B | Payload: {} B (0x{:x}) | BuildTag: 0x{:08x} | Flags: 0x{:08x}\n",
        data.len(),
        data_sz,
        data_sz,
        ts,
        flags
    );

    let idx_off = 16usize.saturating_add(data_sz);
    if idx_off + 12 <= data.len() {
        let num_tables = rd32(data, idx_off);
        let entry_cnt = rd32(data, idx_off + 8) as usize;
        let _ = writeln!(
            out,
            "--- Object Index Table @0x{idx_off:x} (tables={num_tables}, entries={entry_cnt}) ---"
        );
        let list_off = idx_off + 12;
        for i in 0..entry_cnt.min(64) {
            let e = list_off + i * 8;
            if e + 8 > data.len() {
                break;
            }
            let hash = rd32(data, e);
            let obj_off = rd32(data, e + 4);
            let _ = writeln!(out, "  [{i:03}] hash=0x{hash:08x} -> obj@0x{obj_off:06x}");
        }
    }

    // extract readable audio paths and bank references
    let mut cur = Vec::new();
    let mut strs = Vec::new();
    for &b in data.iter().take(256 * 1024) {
        if (32..127).contains(&b) {
            cur.push(b);
        } else {
            if cur.len() >= 5 {
                strs.push(String::from_utf8_lossy(&cur).into_owned());
                if strs.len() >= 64 {
                    break;
                }
            }
            cur.clear();
        }
    }
    if !strs.is_empty() {
        let _ = writeln!(out, "\n--- Embedded Audio Strings / Paths ({}) ---", strs.len());
        for s in strs {
            let _ = writeln!(out, "  {s}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_and_parses_synthetic_dat26() {
        let mut buf = vec![0u8; 64];
        buf[0..4].copy_from_slice(&26u32.to_le_bytes());
        buf[4..8].copy_from_slice(&128u32.to_le_bytes());
        buf[8..12].copy_from_slice(&0x045a_88eau32.to_le_bytes());
        buf[12..16].copy_from_slice(&8u32.to_le_bytes());
        assert_eq!(probe_dat(&buf), Some(26));
        let txt = parse_dat(&buf);
        assert!(txt.contains("DAT26"));
        assert!(txt.contains("0x045a88ea"));
    }
}
