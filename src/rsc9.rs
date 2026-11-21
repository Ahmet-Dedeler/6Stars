use std::fmt::Write as _;

fn rd32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

fn rd64(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3], d[o + 4], d[o + 5], d[o + 6], d[o + 7]])
}

// detect 0x71717171 pointer tagged rsc9 hash/data table
pub fn is_qqqq_table(head: &[u8]) -> bool {
    if head.len() < 0x30 {
        return false;
    }
    let cnt = rd32(head, 0x14);
    let p0 = rd64(head, 0x28);
    rd64(head, 0) == 0 && (1..=200_000).contains(&cnt) && p0 >= 0x40 && p0 < 0x1000_0000
}

// decode 0x71717171 tagged rsc9 table header pointer array and node records
pub fn parse_qqqq_table(data: &[u8]) -> String {
    if data.len() < 0x40 {
        return String::from("Truncated RSC9 table");
    }
    let count = rd32(data, 0x14) as usize;
    let qqqq_cnt = data.windows(4).filter(|w| *w == b"qqqq").count();
    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "=== RAGE 9 RSC9 Pointer-Tagged Table (0x71717171) ===\nSize: {} B | Root Entries: {} (0x{:x}) | 0x71717171 Ptrs: {}\n",
        data.len(),
        count,
        count,
        qqqq_cnt
    );
    let _ = writeln!(out, "--- Sample Root Nodes ---");
    for idx in 0..count.min(48) {
        let poff = 0x28 + idx * 8;
        if poff + 8 > data.len() {
            break;
        }
        let target = rd64(data, poff) as usize;
        if target + 32 > data.len() {
            let _ = writeln!(out, "  [{idx:04}] -> 0x{target:06x} (out of range)");
            continue;
        }
        let h0 = rd32(data, target);
        let h1 = rd32(data, target + 8);
        let ptr = rd32(data, target + 16);
        let tag = rd32(data, target + 20);
        let _ = writeln!(
            out,
            "  [{idx:04}] @0x{target:06x} h0=0x{h0:08x} h1=0x{h1:08x} ptr=0x{ptr:06x} tag=0x{tag:08x}"
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synthetic_qqqq_table() {
        let mut buf = vec![0u8; 0x80];
        buf[0x14..0x18].copy_from_slice(&1u32.to_le_bytes());
        buf[0x28..0x30].copy_from_slice(&0x40u64.to_le_bytes());
        buf[0x40..0x44].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        buf[0x50..0x54].copy_from_slice(&0x60u32.to_le_bytes());
        buf[0x54..0x58].copy_from_slice(&0x7171_7171u32.to_le_bytes());
        assert!(is_qqqq_table(&buf));
        let txt = parse_qqqq_table(&buf);
        assert!(txt.contains("Root Entries: 1"));
        assert!(txt.contains("0x71717171 Ptrs: 1"));
        assert!(txt.contains("0x12345678"));
    }
}
