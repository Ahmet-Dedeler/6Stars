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

// decode 0x71717171 tagged rsc9 table or generic rsc9 virtual blockmap
pub fn parse_rsc9(data: &[u8]) -> String {
    if is_qqqq_table(data) {
        return parse_qqqq_table(data);
    }
    parse_rsc9_generic(data)
}

// inspect generic rsc9 virtual 0x50xxxxxx blockmap and optional phBound aabb
pub fn parse_rsc9_generic(data: &[u8]) -> String {
    if data.len() < 32 {
        return String::from("Truncated RSC9 resource");
    }
    let bmap = rd64(data, 8);
    let mut vptrs = 0usize;
    let mut pptrs = 0usize;
    for o in (0..data.len().saturating_sub(7)).step_by(8) {
        let v = rd64(data, o);
        if (0x5000_0000..0x5800_0000).contains(&v) {
            vptrs += 1;
        } else if (0x6000_0000..0x6800_0000).contains(&v) {
            pptrs += 1;
        }
    }
    let mut out = String::with_capacity(1024);
    let _ = writeln!(
        out,
        "=== RAGE 9 RSC9 Resource ===\nSize: {} B (0x{:x}) | BlockMap: 0x{:08x} | VirtPtrs: {vptrs} | PhysPtrs: {pptrs}\n",
        data.len(),
        data.len(),
        bmap
    );

    // check plausible phBound aabb min/max vec3 pair
    for off in [0x10usize, 0x20, 0x30, 0x40] {
        if off + 32 <= data.len() {
            let minx = f32::from_bits(rd32(data, off));
            let miny = f32::from_bits(rd32(data, off + 4));
            let minz = f32::from_bits(rd32(data, off + 8));
            let maxx = f32::from_bits(rd32(data, off + 16));
            let maxy = f32::from_bits(rd32(data, off + 20));
            let maxz = f32::from_bits(rd32(data, off + 24));
            if [minx, miny, minz, maxx, maxy, maxz].iter().all(|v| v.is_finite() && v.abs() < 1e4)
                && minx < maxx && miny < maxy && minz < maxz
                && (maxx - minx) > 0.01
            {
                let _ = writeln!(
                    out,
                    "--- Candidate phBound AABB @0x{off:02x} ---\n  Min: ({minx:.3}, {miny:.3}, {minz:.3})\n  Max: ({maxx:.3}, {maxy:.3}, {maxz:.3})\n"
                );
                break;
            }
        }
    }

    let _ = writeln!(out, "--- Header QWords ---");
    for o in (0..data.len().min(128)).step_by(16) {
        let q0 = rd64(data, o);
        let q1 = if o + 8 < data.len() { rd64(data, o + 8) } else { 0 };
        let _ = writeln!(out, "  +0x{o:02x}: {q0:016x} {q1:016x}");
    }
    out
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
