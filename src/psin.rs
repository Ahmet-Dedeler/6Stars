use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::OnceLock;

// common rage pso struct and field names for joaat hash resolution
const KNOWN_NAMES: &[&str] = &[
    "Name", "Flags", "Type", "Id", "Hash", "Key", "Value", "Index", "Count", "Version",
    "Enabled", "Active", "Visible", "Hidden", "Priority", "State", "Category", "Group",
    "Tag", "Tags", "Label", "Title", "Text", "Description", "Path", "File", "Asset",
    "Position", "Rotation", "Scale", "Offset", "Size", "Min", "Max", "Bounds", "Radius",
    "Color", "Intensity", "Duration", "Delay", "Speed", "Weight", "Probability", "Chance",
    "Items", "Entries", "Data", "Children", "Parent", "Components", "Attributes",
    "Properties", "Params", "Metadata", "Events", "Actions", "Conditions", "Triggers",
    "Script", "ModelName", "ArchetypeName", "TextureName", "AudioName", "AnimName",
    "x", "y", "z", "w", "r", "g", "b", "a",
    "CMapData", "CMapTypes", "CEntityDef", "CBaseArchetypeDef", "CMloArchetypeDef",
    "CDataFileMgr__ContentDef", "CDataFileMgr__ContentChangeSetDef",
    "CWeaponInfo", "CVehicleModelInfo", "CPedModelInfo", "CScenarioPoint",
];

#[derive(Clone, Debug)]
pub struct PsinField {
    pub name_hash: u32,
    pub offset: u32,
    pub type_raw: u32,
    pub ref_hash: u32,
}

#[derive(Clone, Debug)]
pub struct PsinStruct {
    pub hash: u32,
    pub flags: u32,
    pub size: u32,
    pub fields: Vec<PsinField>,
}

#[derive(Clone, Debug)]
pub struct PsinEnum {
    pub hash: u32,
    pub values: Vec<(i64, u32)>,
}

#[derive(Clone, Debug)]
pub struct PsinInstance {
    pub tag: u32,
    pub size: u32,
    pub offset: usize,
}

pub fn joaat(s: &str) -> u32 {
    let mut h = 0u32;
    for &b in s.as_bytes() {
        let c = b.to_ascii_lowercase() as u32;
        h = h.wrapping_add(c);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h.wrapping_add(h << 15)
}

fn known_joaat_map() -> &'static HashMap<u32, &'static str> {
    static MAP: OnceLock<HashMap<u32, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::with_capacity(KNOWN_NAMES.len());
        for &n in KNOWN_NAMES {
            m.insert(joaat(n), n);
        }
        m
    })
}

fn fmt_hash(h: u32) -> String {
    if let Some(&name) = known_joaat_map().get(&h) {
        format!("{name} (0x{h:08x})")
    } else {
        format!("0x{h:08x}")
    }
}

fn rd32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

fn rd64(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3], d[o + 4], d[o + 5], d[o + 6], d[o + 7]])
}

fn parse_iff_chunks(data: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut chunks = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        let tag = &data[pos..pos + 4];
        let sz = u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]) as usize;
        if sz < 8 || pos + sz > data.len() {
            break;
        }
        chunks.push((tag, &data[pos + 8..pos + sz]));
        pos += sz;
    }
    chunks
}

fn parse_lsch(lsch: &[u8]) -> Vec<PsinStruct> {
    let mut off = 0usize;
    let mut hdrs = Vec::new();
    while off + 32 <= lsch.len() && &lsch[off + 16..off + 24] == &[0xAA; 8] {
        let hash = rd32(lsch, off);
        let flags = rd32(lsch, off + 8);
        let size = rd32(lsch, off + 24);
        let fcnt = (rd32(lsch, off + 28) >> 16) as usize;
        hdrs.push((hash, flags, size, fcnt));
        off += 32;
    }
    let mut structs = Vec::with_capacity(hdrs.len());
    for (hash, flags, size, fcnt) in hdrs {
        let mut fields = Vec::with_capacity(fcnt);
        for _ in 0..fcnt {
            if off + 16 > lsch.len() {
                break;
            }
            fields.push(PsinField {
                name_hash: rd32(lsch, off),
                offset: rd32(lsch, off + 4),
                type_raw: rd32(lsch, off + 8),
                ref_hash: rd32(lsch, off + 12),
            });
            off += 16;
        }
        structs.push(PsinStruct { hash, flags, size, fields });
    }
    structs
}

fn parse_lenm(lenm: &[u8]) -> Vec<PsinEnum> {
    let mut enums = Vec::new();
    let mut off = 0usize;
    while off + 24 <= lenm.len() && &lenm[off + 8..off + 16] == &[0xEE; 8] {
        let hash = rd32(lenm, off);
        let cnt = rd32(lenm, off + 16) as usize;
        off += 24;
        let mut values = Vec::with_capacity(cnt.min(4096));
        for _ in 0..cnt {
            if off + 16 > lenm.len() {
                break;
            }
            let val = rd64(lenm, off) as i64;
            let nh = rd32(lenm, off + 8);
            values.push((val, nh));
            off += 16;
        }
        enums.push(PsinEnum { hash, values });
    }
    enums
}

fn fmt_field_val(
    block: &[u8],
    f: &PsinField,
    enum_map: &HashMap<u32, &PsinEnum>,
) -> String {
    let o = f.offset as usize;
    let kind = f.type_raw & 0xFF;
    match kind {
        0x01 if o < block.len() => format!("bool({})", block[o] != 0),
        0x02 | 0x10 if o < block.len() => format!("u8({})", block[o]),
        0x11 | 0x12 if o + 2 <= block.len() => {
            let v = u16::from_le_bytes([block[o], block[o + 1]]);
            format!("u16({v})")
        }
        0x13..=0x16 if o + 4 <= block.len() => {
            let v = rd32(block, o);
            let fv = f32::from_bits(v);
            if fv.is_finite() && fv.abs() > 1e-6 && fv.abs() < 1e7 && v >= 0x3800_0000 {
                format!("f32({fv:.4}) / 0x{v:08x}")
            } else {
                format!("{v} (0x{v:08x})")
            }
        }
        0x17 | 0x18 if o + 8 <= block.len() => format!("u64(0x{:016x})", rd64(block, o)),
        0x20..=0x22 if o + 12 <= block.len() => {
            let x = f32::from_bits(rd32(block, o));
            let y = f32::from_bits(rd32(block, o + 4));
            let z = f32::from_bits(rd32(block, o + 8));
            format!("vec3({x:.3}, {y:.3}, {z:.3})")
        }
        0x44 | 0x48 | 0x4a if o + 4 <= block.len() => {
            let h = rd32(block, o);
            format!("hash({})", fmt_hash(h))
        }
        0x52 | 0x5c if o + 12 <= block.len() => {
            let cnt = rd32(block, o + 8);
            format!("array(len={cnt})")
        }
        0x60 if o < block.len() => {
            let v = block[o] as i64;
            if let Some(en) = enum_map.get(&f.ref_hash) {
                if let Some((_, nh)) = en.values.iter().find(|(ev, _)| *ev == v) {
                    return format!("enum({} = {})", v, fmt_hash(*nh));
                }
            }
            format!("enum({v} @ {})", fmt_hash(f.ref_hash))
        }
        0x05 => format!("struct({})", fmt_hash(f.ref_hash)),
        _ => {
            let end = (o + 8).min(block.len());
            if o < end {
                let hex: String = block[o..end].iter().map(|b| format!("{b:02x}")).collect();
                format!("type=0x{kind:02x} [{hex}]")
            } else {
                format!("type=0x{kind:02x}")
            }
        }
    }
}

// decode rage 9 binary pso (psin + lsch + lenm + lsns + lrsc + chks)
pub fn parse_psin(data: &[u8]) -> String {
    if data.len() < 16 || &data[..4] != b"PSIN" {
        return String::from("Not a valid PSIN container");
    }
    let chunks = parse_iff_chunks(data);
    let psin_body = chunks.iter().find(|(t, _)| *t == b"PSIN").map(|(_, b)| *b).unwrap_or(&[]);
    let lsch_body = chunks.iter().find(|(t, _)| *t == b"LSCH").map(|(_, b)| *b).unwrap_or(&[]);
    let lenm_body = chunks.iter().find(|(t, _)| *t == b"LENM").map(|(_, b)| *b).unwrap_or(&[]);

    // parse instance entry table up to pppppppp sentinel
    let mut i = 0usize;
    let mut raw_insts = Vec::new();
    while i + 8 <= psin_body.len() {
        if &psin_body[i..i + 8] == b"pppppppp" {
            i += 8;
            break;
        }
        if i + 16 > psin_body.len() {
            break;
        }
        raw_insts.push((rd32(psin_body, i), rd32(psin_body, i + 4)));
        i += 16;
    }
    let inst_payload = &psin_body[i..];
    let mut cursor = 0usize;
    let mut insts = Vec::with_capacity(raw_insts.len());
    for (tag, size) in raw_insts {
        insts.push(PsinInstance { tag, size, offset: cursor });
        cursor = cursor.saturating_add((size as usize + 15) & !15);
    }

    let structs = parse_lsch(lsch_body);
    let enums = parse_lenm(lenm_body);
    let struct_map: HashMap<u32, &PsinStruct> = structs.iter().map(|s| (s.hash, s)).collect();
    let enum_map: HashMap<u32, &PsinEnum> = enums.iter().map(|e| (e.hash, e)).collect();

    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "=== RAGE 9 PSIN (Binary PSO Decoder) ===\nTotal: {} B | Chunks: {} | Structs: {} | Enums: {} | Instances: {}",
        data.len(),
        chunks.len(),
        structs.len(),
        enums.len(),
        insts.len()
    );
    let _ = write!(out, "Sections:");
    for (tag, body) in &chunks {
        let _ = write!(out, " {}({}B)", String::from_utf8_lossy(tag), body.len() + 8);
    }
    out.push_str("\n\n");

    // decoded instances
    let _ = writeln!(out, "--- Decoded Instances ---");
    for (idx, inst) in insts.iter().enumerate().take(64) {
        let end = (inst.offset + inst.size as usize).min(inst_payload.len());
        let block = inst_payload.get(inst.offset..end).unwrap_or(&[]);
        if let Some(st) = struct_map.get(&inst.tag) {
            let _ = writeln!(out, "[{idx}] <{} size=0x{:x} fields={}>", fmt_hash(st.hash), inst.size, st.fields.len());
            for f in st.fields.iter().filter(|f| f.name_hash != 0x0000_0100).take(48) {
                let val = fmt_field_val(block, f, &enum_map);
                let _ = writeln!(out, "  +0x{:03x} {} = {val}", f.offset, fmt_hash(f.name_hash));
            }
        } else {
            let ascii_preview: String = block.iter().take(64).map(|&b| if (32..127).contains(&b) { b as char } else { '.' }).collect();
            let _ = writeln!(out, "[{idx}] <heap/blob 0x{:08x} size=0x{:x}> \"{ascii_preview}\"", inst.tag, inst.size);
        }
    }

    if !enums.is_empty() {
        let _ = writeln!(out, "\n--- LENM Enums ({}) ---", enums.len());
        for en in enums.iter().take(24) {
            let _ = writeln!(out, "enum {} ({} values):", fmt_hash(en.hash), en.values.len());
            for (val, nh) in en.values.iter().take(16) {
                let _ = writeln!(out, "  {val} = {}", fmt_hash(*nh));
            }
        }
    }

    let _ = writeln!(out, "\n--- LSCH Schema ({} structs) ---", structs.len());
    for st in structs.iter().take(24) {
        let _ = writeln!(out, "struct {} size=0x{:x} flags=0x{:x} fields={}", fmt_hash(st.hash), st.size, st.flags, st.fields.len());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synthetic_psin_with_lsch_and_lenm() {
        let mut buf = Vec::new();
        // psin chunk: header + 1 instance (hash 0x11223344, size 16) + pppppppp + 16B payload
        let mut psin = Vec::new();
        psin.extend_from_slice(&0x1122_3344u32.to_le_bytes());
        psin.extend_from_slice(&16u32.to_le_bytes());
        psin.extend_from_slice(&[0xAA; 8]);
        psin.extend_from_slice(b"pppppppp");
        psin.extend_from_slice(&42u32.to_le_bytes()); // offset 0: u32
        psin.push(1); // offset 4: bool
        psin.push(2); // offset 5: enum
        psin.extend_from_slice(&[0u8; 10]);

        buf.extend_from_slice(b"PSIN");
        buf.extend_from_slice(&((8 + psin.len()) as u32).to_be_bytes());
        buf.extend_from_slice(&psin);

        // lsch chunk: 1 struct with 3 fields
        let mut lsch = Vec::new();
        lsch.extend_from_slice(&0x1122_3344u32.to_le_bytes());
        lsch.extend_from_slice(&0u32.to_le_bytes());
        lsch.extend_from_slice(&0x0300u32.to_le_bytes());
        lsch.extend_from_slice(&0u32.to_le_bytes());
        lsch.extend_from_slice(&[0xAA; 8]);
        lsch.extend_from_slice(&16u32.to_le_bytes());
        lsch.extend_from_slice(&(3u32 << 16).to_le_bytes());
        // field 0: Count @0x00 u32 (0x15)
        lsch.extend_from_slice(&joaat("Count").to_le_bytes());
        lsch.extend_from_slice(&0u32.to_le_bytes());
        lsch.extend_from_slice(&0x15u32.to_le_bytes());
        lsch.extend_from_slice(&0u32.to_le_bytes());
        // field 1: Enabled @0x04 bool (0x01)
        lsch.extend_from_slice(&joaat("Enabled").to_le_bytes());
        lsch.extend_from_slice(&4u32.to_le_bytes());
        lsch.extend_from_slice(&0x01u32.to_le_bytes());
        lsch.extend_from_slice(&0u32.to_le_bytes());
        // field 2: State @0x05 enum (0x60) -> 0x55667788
        lsch.extend_from_slice(&joaat("State").to_le_bytes());
        lsch.extend_from_slice(&5u32.to_le_bytes());
        lsch.extend_from_slice(&0x60u32.to_le_bytes());
        lsch.extend_from_slice(&0x5566_7788u32.to_le_bytes());

        buf.extend_from_slice(b"LSCH");
        buf.extend_from_slice(&((8 + lsch.len()) as u32).to_be_bytes());
        buf.extend_from_slice(&lsch);

        // lenm chunk: enum 0x55667788 with 1 value (2 -> Active)
        let mut lenm = Vec::new();
        lenm.extend_from_slice(&0x5566_7788u32.to_le_bytes());
        lenm.extend_from_slice(&0u32.to_le_bytes());
        lenm.extend_from_slice(&[0xEE; 8]);
        lenm.extend_from_slice(&1u32.to_le_bytes());
        lenm.extend_from_slice(&0u32.to_le_bytes());
        lenm.extend_from_slice(&2i64.to_le_bytes());
        lenm.extend_from_slice(&joaat("Active").to_le_bytes());
        lenm.extend_from_slice(&0u32.to_le_bytes());

        buf.extend_from_slice(b"LENM");
        buf.extend_from_slice(&((8 + lenm.len()) as u32).to_be_bytes());
        buf.extend_from_slice(&lenm);

        let out = parse_psin(&buf);
        assert!(out.contains("Structs: 1 | Enums: 1 | Instances: 1"));
        assert!(out.contains("Count"));
        assert!(out.contains("42 (0x0000002a)"));
        assert!(out.contains("bool(true)"));
        assert!(out.contains("enum(2 = Active"));
    }
}
