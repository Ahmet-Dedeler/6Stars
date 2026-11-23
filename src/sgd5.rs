use flate2::read::DeflateDecoder;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"SGD5";
const COMPRESSED_MAGIC: u32 = 0x000D_DCE7;
const TABLE_REFERENCE: u32 = 0x8000_0000;
const REFLECTION_RECORD_SIZE: usize = 24;
const MAX_SHADER_BYTES: u32 = 64 << 20;
const STAGES: [(&str, &str); 6] = [
    ("VS", "vs"),
    ("PS", "ps"),
    ("GS", "gs"),
    ("DS", "ds"),
    ("HS", "hs"),
    ("CS", "cs"),
];

#[derive(Clone, Debug, Default)]
pub struct Sgd5ShaderArchive {
    pub offset: usize,
    pub archive_size: u32,
    pub decompressed_size: u32,
}

#[derive(Clone, Debug)]
pub struct Sgd5Shader {
    pub stage: &'static str,
    pub index: u32,
    pub name: String,
    pub wave_size: u8,
    pub archive: Sgd5ShaderArchive,
    pub sga_hash: u64,
    /// Opaque data historically called the root signature; its encoding is still unresolved.
    pub root_signature_data: [u8; 0x90],
    pub reflection: Sgd5ReflectionTable,
}

#[derive(Clone, Debug)]
pub struct Sgd5BindingEntry {
    pub name_hash: u32,
    pub binding_type: u16,
    pub binding_index: u16,
    pub name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Sgd5BindingTableHeader {
    pub entry_count: u16,
    pub unknown0: u8,
    pub srv_count: u8,
    pub unknown1: u8,
    pub uav_count: u8,
    pub sampler_count: u8,
    pub cbv_count: u8,
    pub table_hash: u32,
    pub unknown2: u32,
}

#[derive(Clone, Debug)]
pub struct Sgd5BindingTable {
    pub offset: usize,
    pub size: u32,
    pub hash: u64,
    pub header: Sgd5BindingTableHeader,
    pub entries: Vec<Sgd5BindingEntry>,
}

#[derive(Clone, Debug)]
pub struct Sgd5ResourceRecord {
    pub offset: usize,
    pub resource_type: u16,
    /// Low-byte index; its relationship to HLSL registers remains unconfirmed.
    pub slot: u8,
    pub auxiliary_record_count: u8,
    pub member_count: u8,
    pub flags: u8,
    pub member_data_offset: u16,
    pub name_string_offset: u16,
    pub reserved_0a: u16,
    /// CBV data size; zero for records that do not carry a constant-buffer layout.
    pub cbuffer_size: u16,
    pub auxiliary_data_offset: u16,
    pub name_hash: u32,
    pub secondary_hash: u32,
    pub name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Sgd5MemberRecord {
    pub offset: usize,
    pub value_type: u8,
    pub auxiliary_record_count: u8,
    pub array_size: u16,
    pub byte_offset: u16,
    pub reserved_06: u16,
    pub reserved_08: u16,
    pub default_value_offset: u16,
    pub auxiliary_data_offset: u16,
    pub reserved_0e: u16,
    pub name_hash: u32,
    pub secondary_hash: u32,
    pub name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Sgd5AuxiliaryRecord {
    pub offset: usize,
    pub hash: u32,
    pub data: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Sgd5ReflectionTable {
    pub offset: usize,
    pub size: u32,
    pub resource_count: u16,
    pub member_count: u16,
    pub auxiliary_record_count: u16,
    pub resources: Vec<Sgd5ResourceRecord>,
    pub members: Vec<Sgd5MemberRecord>,
    pub auxiliary_records: Vec<Sgd5AuxiliaryRecord>,
    pub string_pool: Vec<u8>,
    pub strings: Vec<String>,
    pub default_values_offset: usize,
    pub default_value_data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub enum Sgd5PropertyEntry {
    TableReference { offset: usize, table_hash: u32 },
    Inline(Sgd5BindingTable),
}

#[derive(Clone, Debug)]
pub struct Sgd5PropertyList {
    pub offset: usize,
    pub flags: u16,
    pub entries: Vec<Sgd5PropertyEntry>,
    pub strings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Sgd5Effect {
    pub name: String,
    pub data_buffer_size: u32,
    pub stage_indices: [Vec<u32>; 6],
    pub payload_offset: usize,
    pub payload_end: usize,
    pub metadata: Vec<u8>,
    pub main_reflection: Sgd5ReflectionTable,
    pub secondary_reflection: Sgd5ReflectionTable,
    pub property_list: Sgd5PropertyList,
}

#[derive(Clone, Debug)]
pub struct Sgd5File {
    pub data: Vec<u8>,
    pub group_hash: u64,
    pub binding_tables: Vec<Sgd5BindingTable>,
    pub shared_reflection: Sgd5ReflectionTable,
    pub shaders: Vec<Sgd5Shader>,
    pub declared_effect_count: u32,
    pub effect_count: u32,
    pub trailer_flags: u32,
    pub trailer_reserved: [u8; 6],
    pub effects: Vec<Sgd5Effect>,
    pub hash_names: HashMap<u32, String>,
    pub effect_shader_map: HashMap<(u8, u32), Vec<usize>>,
}

impl Sgd5File {
    pub fn resolve_name(&self, hash: u32) -> Option<&str> {
        self.hash_names.get(&hash).map(String::as_str)
    }

    pub fn effect_names_for_shader(&self, stage: &str, index: u32) -> Vec<&str> {
        let Some(stage_index) = STAGES.iter().position(|(name, _)| *name == stage) else {
            return Vec::new();
        };
        self.effect_shader_map
            .get(&(stage_index as u8, index))
            .map(|effect_indices| {
                effect_indices
                    .iter()
                    .filter_map(|effect_index| self.effects.get(*effect_index))
                    .map(|effect| effect.name.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug)]
pub struct Sgd5Extraction {
    pub files: Vec<Sgd5File>,
    pub extracted_count: usize,
    pub manifests: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct Sgd5Error(String);

impl std::fmt::Display for Sgd5Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Sgd5Error {}

struct Reader<'a> {
    data: &'a [u8],
    position: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            end: data.len(),
        }
    }

    fn remaining(&self) -> usize {
        self.end - self.position
    }

    fn take(&mut self, size: usize) -> Result<&'a [u8], Sgd5Error> {
        let end = self
            .position
            .checked_add(size)
            .filter(|end| *end <= self.end)
            .ok_or_else(|| {
                Sgd5Error(format!(
                    "truncated data at 0x{:x}: need {size} bytes",
                    self.position
                ))
            })?;
        let bytes = &self.data[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Sgd5Error> {
        Ok(self.take(N)?.try_into().unwrap())
    }

    fn u8(&mut self) -> Result<u8, Sgd5Error> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, Sgd5Error> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, Sgd5Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn peek_u32(&self) -> Result<u32, Sgd5Error> {
        let end = self
            .position
            .checked_add(4)
            .filter(|end| *end <= self.end)
            .ok_or_else(|| Sgd5Error(format!("truncated u32 at 0x{:x}", self.position)))?;
        Ok(u32::from_le_bytes(
            self.data[self.position..end].try_into().unwrap(),
        ))
    }

    fn u64(&mut self) -> Result<u64, Sgd5Error> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn section(&mut self, size: usize) -> Result<Self, Sgd5Error> {
        let position = self.position;
        self.take(size)?;
        Ok(Self {
            data: self.data,
            position,
            end: self.position,
        })
    }

    fn require_records(&self, count: usize, width: usize) -> Result<usize, Sgd5Error> {
        count
            .checked_mul(width)
            .filter(|size| *size <= self.remaining())
            .ok_or_else(|| {
                Sgd5Error(format!(
                    "truncated {count} records at 0x{:x}",
                    self.position
                ))
            })
    }

    fn name(&mut self, label: &str) -> Result<String, Sgd5Error> {
        let offset = self.position;
        let length = self.u8()? as usize;
        let bytes = self.take(length)?;
        if bytes.last() != Some(&0) || length < 2 || bytes[..length - 1].contains(&0) {
            return Err(Sgd5Error(format!("invalid {label} name at 0x{offset:x}")));
        }
        String::from_utf8(bytes[..length - 1].to_vec())
            .map_err(|_| Sgd5Error(format!("invalid UTF-8 {label} name at 0x{offset:x}")))
    }
}

pub fn joaat_lower(name: &str) -> u32 {
    let mut hash = 0u32;
    for byte in name.as_bytes().iter().map(|byte| byte.to_ascii_lowercase()) {
        hash = hash.wrapping_add(byte as u32);
        hash = hash.wrapping_add(hash << 10);
        hash ^= hash >> 6;
    }
    hash = hash.wrapping_add(hash << 3);
    hash ^= hash >> 11;
    hash = hash.wrapping_add(hash << 15);
    hash
}

fn parse_nul_pool(pool: &[u8]) -> Result<Vec<String>, Sgd5Error> {
    if pool.is_empty() {
        return Ok(Vec::new());
    }
    if pool.first() != Some(&0) || pool.last() != Some(&0) {
        return Err(Sgd5Error("string pool is not NUL-delimited".into()));
    }
    Ok(pool[1..]
        .split(|byte| *byte == 0)
        .filter(|token| !token.is_empty())
        .map(|token| String::from_utf8_lossy(token).into_owned())
        .collect())
}

fn parse_resource_record(reader: &mut Reader<'_>) -> Result<Sgd5ResourceRecord, Sgd5Error> {
    Ok(Sgd5ResourceRecord {
        offset: reader.position,
        resource_type: reader.u16()?,
        slot: reader.u8()?,
        auxiliary_record_count: reader.u8()?,
        member_count: reader.u8()?,
        flags: reader.u8()?,
        member_data_offset: reader.u16()?,
        name_string_offset: reader.u16()?,
        reserved_0a: reader.u16()?,
        cbuffer_size: reader.u16()?,
        auxiliary_data_offset: reader.u16()?,
        name_hash: reader.u32()?,
        secondary_hash: reader.u32()?,
        name: None,
    })
}

fn parse_member_record(reader: &mut Reader<'_>) -> Result<Sgd5MemberRecord, Sgd5Error> {
    Ok(Sgd5MemberRecord {
        offset: reader.position,
        value_type: reader.u8()?,
        auxiliary_record_count: reader.u8()?,
        array_size: reader.u16()?,
        byte_offset: reader.u16()?,
        reserved_06: reader.u16()?,
        reserved_08: reader.u16()?,
        default_value_offset: reader.u16()?,
        auxiliary_data_offset: reader.u16()?,
        reserved_0e: reader.u16()?,
        name_hash: reader.u32()?,
        secondary_hash: reader.u32()?,
        name: None,
    })
}

fn reflection_name(
    data: &[u8],
    offset: usize,
    relative_offset: u16,
    hash: u32,
    pool_start: usize,
    pool_end: usize,
) -> Option<String> {
    if relative_offset == 0 {
        return None;
    }
    let target = offset.checked_add(relative_offset as usize)?;
    if !(pool_start..pool_end).contains(&target) {
        return None;
    }
    let bytes = data.get(target..pool_end)?;
    let end = bytes.iter().position(|byte| *byte == 0)?;
    let name = std::str::from_utf8(&bytes[..end]).ok()?;
    (!name.is_empty() && joaat_lower(name) == hash).then(|| name.to_string())
}

fn resolve_reflection_names(table: &mut Sgd5ReflectionTable, names: &HashMap<u32, String>) {
    for record in &mut table.resources {
        if record.name.is_none() {
            record.name = names.get(&record.name_hash).cloned();
        }
    }
    for record in &mut table.members {
        if record.name.is_none() {
            record.name = names.get(&record.name_hash).cloned();
        }
    }
}

fn parse_reflection_table(reader: &mut Reader<'_>) -> Result<Sgd5ReflectionTable, Sgd5Error> {
    let offset = reader.position;
    let size = reader.u32()?;
    let mut section = reader.section(size as usize)?;
    let resource_count = section.u16()?;
    let member_count = section.u16()?;
    let auxiliary_record_count = section.u16()?;
    if section.u16()? as u32 != size {
        return Err(Sgd5Error(format!(
            "reflection table at 0x{offset:x}: size copy mismatch"
        )));
    }
    section.require_records(
        resource_count as usize + member_count as usize,
        REFLECTION_RECORD_SIZE,
    )?;
    let mut resources = Vec::with_capacity(resource_count as usize);
    for _ in 0..resource_count {
        resources.push(parse_resource_record(&mut section)?);
    }
    let mut members = Vec::with_capacity(member_count as usize);
    for _ in 0..member_count {
        members.push(parse_member_record(&mut section)?);
    }
    section.require_records(auxiliary_record_count as usize, 8)?;
    let mut auxiliary_records = Vec::with_capacity(auxiliary_record_count as usize);
    for _ in 0..auxiliary_record_count {
        auxiliary_records.push(Sgd5AuxiliaryRecord {
            offset: section.position,
            hash: section.u32()?,
            data: section.u32()?,
        });
    }

    let pool_start = section.position;
    let mut default_values_offset = section.end;
    let value_links = members
        .iter()
        .map(|record| (record.offset, record.default_value_offset));
    for (record_offset, relative_offset) in value_links {
        if relative_offset == 0 {
            continue;
        }
        let target = record_offset
            .checked_add(relative_offset as usize)
            .filter(|target| *target >= pool_start && *target <= section.end.saturating_sub(4))
            .ok_or_else(|| {
                Sgd5Error(format!(
                    "invalid default-value offset at 0x{record_offset:x}"
                ))
            })?;
        default_values_offset = default_values_offset.min(target);
    }
    // Default data is untyped; its boundary follows the actual value links, not the auxiliary count.
    let string_pool = section.take(default_values_offset - pool_start)?.to_vec();
    let strings = parse_nul_pool(&string_pool)?;
    let default_value_data = section.take(section.remaining())?.to_vec();
    for record in &mut resources {
        record.name = reflection_name(
            section.data,
            record.offset,
            record.name_string_offset,
            record.name_hash,
            pool_start,
            default_values_offset,
        );
    }
    let mut table = Sgd5ReflectionTable {
        offset,
        size,
        resource_count,
        member_count,
        auxiliary_record_count,
        resources,
        members,
        auxiliary_records,
        string_pool,
        strings,
        default_values_offset,
        default_value_data,
    };
    let names = table
        .strings
        .iter()
        .map(|name| (joaat_lower(name), name.clone()))
        .collect();
    resolve_reflection_names(&mut table, &names);
    Ok(table)
}

fn parse_binding_table(reader: &mut Reader<'_>) -> Result<Sgd5BindingTable, Sgd5Error> {
    let offset = reader.position;
    let size = reader.u32()?;
    let mut section = reader.section(size as usize)?;
    let hash = section.u64()?;
    let header = Sgd5BindingTableHeader {
        entry_count: section.u16()?,
        unknown0: section.u8()?,
        srv_count: section.u8()?,
        unknown1: section.u8()?,
        uav_count: section.u8()?,
        sampler_count: section.u8()?,
        cbv_count: section.u8()?,
        table_hash: section.u32()?,
        unknown2: section.u32()?,
    };
    let entries_size = section.require_records(header.entry_count as usize, 8)?;
    if section.remaining() != entries_size {
        return Err(Sgd5Error(format!(
            "binding table at 0x{offset:x}: size does not match entry count"
        )));
    }
    let mut entries = Vec::with_capacity(header.entry_count as usize);
    for _ in 0..header.entry_count {
        entries.push(Sgd5BindingEntry {
            name_hash: section.u32()?,
            binding_type: section.u16()?,
            binding_index: section.u16()?,
            name: None,
        });
    }
    Ok(Sgd5BindingTable {
        offset,
        size,
        hash,
        header,
        entries,
    })
}

fn parse_shader_archive(
    reader: &mut Reader<'_>,
    expected_size: u32,
) -> Result<Sgd5ShaderArchive, Sgd5Error> {
    let offset = reader.position;
    let mut section = reader.section(expected_size as usize)?;
    if section.u32()? != COMPRESSED_MAGIC {
        return Err(Sgd5Error(format!(
            "invalid compressed-container magic at 0x{offset:x}"
        )));
    }
    let archive_size = section.u32()?;
    let decompressed_size = section.u32()?;
    if decompressed_size > MAX_SHADER_BYTES {
        return Err(Sgd5Error(format!(
            "shader archive at 0x{offset:x}: decompressed size exceeds 64 MiB"
        )));
    }
    if archive_size != expected_size {
        return Err(Sgd5Error(format!(
            "shader archive at 0x{offset:x}: size copy mismatch"
        )));
    }
    Ok(Sgd5ShaderArchive {
        offset,
        archive_size,
        decompressed_size,
    })
}

fn parse_shader(
    reader: &mut Reader<'_>,
    stage: &'static str,
    index: u32,
) -> Result<Sgd5Shader, Sgd5Error> {
    let name = reader.name("shader")?;
    let wave_size = reader.u8()?;
    let archive_size = reader.u32()?;
    let archive = parse_shader_archive(reader, archive_size)?;
    let sga_hash = reader.u64()?;
    let root_signature_data = reader.array()?;
    let reflection = parse_reflection_table(reader)?;
    Ok(Sgd5Shader {
        stage,
        index,
        name,
        wave_size,
        archive,
        sga_hash,
        root_signature_data,
        reflection,
    })
}

fn parse_effect_metadata(reader: &mut Reader<'_>) -> Result<Vec<u8>, Sgd5Error> {
    let start = reader.position;
    let base = reader.take(0x5c)?;
    let extension_count = u32::from_le_bytes(base[0x50..0x54].try_into().unwrap()) as usize;
    reader.require_records(extension_count, 0x10)?;
    let mut tail = u32::from_le_bytes(base[0x58..0x5c].try_into().unwrap());
    for _ in 0..extension_count {
        let extension = reader.take(0x10)?;
        if extension[..4] != [0; 4] || extension[4..8] != [1, 1, 0, 0] {
            return Err(Sgd5Error(format!(
                "invalid effect extension at 0x{:x}",
                reader.position - 0x10
            )));
        }
        tail = u32::from_le_bytes(extension[0x0c..0x10].try_into().unwrap());
    }
    while tail >> 16 != 0 {
        let continuation = reader.take(0x18)?;
        tail = u32::from_le_bytes(continuation[0x14..0x18].try_into().unwrap());
    }
    if tail != 0 && tail & 0xff00 == 0 {
        let size = reader.require_records((tail & 0xff) as usize, 0x44)?;
        reader.take(size)?;
    }
    for width in [0x24, 4, 0x18, 4] {
        let count = reader.u32()? as usize;
        let size = reader.require_records(count, width)?;
        reader.take(size)?;
    }
    let end = reader.position;
    if reader.u32()? != TABLE_REFERENCE {
        return Err(Sgd5Error(format!("missing reflection marker at 0x{end:x}")));
    }
    Ok(reader.data[start..end].to_vec())
}

fn parse_property_list(reader: &mut Reader<'_>) -> Result<Sgd5PropertyList, Sgd5Error> {
    let offset = reader.position;
    let count = reader.u16()?;
    let flags = reader.u16()?;
    reader.require_records(count as usize, 8)?;
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let entry_offset = reader.position;
        let marker = reader.peek_u32()?;
        if marker == TABLE_REFERENCE {
            reader.u32()?;
            entries.push(Sgd5PropertyEntry::TableReference {
                offset: entry_offset,
                table_hash: reader.u32()?,
            });
        } else {
            entries.push(Sgd5PropertyEntry::Inline(parse_binding_table(reader)?));
        }
    }
    let strings_size = reader.u32()? as usize;
    let strings = parse_nul_pool(reader.take(strings_size)?)?;
    Ok(Sgd5PropertyList {
        offset,
        flags,
        entries,
        strings,
    })
}

fn parse_effect(reader: &mut Reader<'_>, stage_counts: &[u32; 6]) -> Result<Sgd5Effect, Sgd5Error> {
    let name = reader.name("effect")?;
    let data_buffer_size = reader.u32()?;
    let mut stage_indices: [Vec<u32>; 6] = std::array::from_fn(|_| Vec::new());
    for (stage_index, indices) in stage_indices.iter_mut().enumerate() {
        let count = reader.u32()? as usize;
        reader.require_records(count, 4)?;
        indices.reserve(count);
        for _ in 0..count {
            let index = reader.u32()?;
            if index >= stage_counts[stage_index] {
                return Err(Sgd5Error(format!(
                    "effect {name}: invalid {} shader index {index}",
                    STAGES[stage_index].0
                )));
            }
            indices.push(index);
        }
    }
    let payload_offset = reader.position;
    let metadata = parse_effect_metadata(reader)?;
    let main_reflection = parse_reflection_table(reader)?;
    let secondary_reflection = parse_reflection_table(reader)?;
    let property_list = parse_property_list(reader)?;
    Ok(Sgd5Effect {
        name,
        data_buffer_size,
        stage_indices,
        payload_offset,
        payload_end: reader.position,
        metadata,
        main_reflection,
        secondary_reflection,
        property_list,
    })
}

fn build_hash_names(
    shared_reflection: &Sgd5ReflectionTable,
    shaders: &[Sgd5Shader],
    effects: &[Sgd5Effect],
) -> HashMap<u32, String> {
    let tables = std::iter::once(shared_reflection)
        .chain(shaders.iter().map(|shader| &shader.reflection))
        .chain(
            effects
                .iter()
                .flat_map(|effect| [&effect.main_reflection, &effect.secondary_reflection]),
        );
    let strings = tables.flat_map(|table| table.strings.iter()).chain(
        effects
            .iter()
            .flat_map(|effect| effect.property_list.strings.iter()),
    );
    strings
        .map(|name| (joaat_lower(name), name.clone()))
        .collect()
}

fn resolve_binding_names(table: &mut Sgd5BindingTable, names: &HashMap<u32, String>) {
    for binding in &mut table.entries {
        binding.name = names.get(&binding.name_hash).cloned();
    }
}

fn build_effect_shader_map(effects: &[Sgd5Effect]) -> HashMap<(u8, u32), Vec<usize>> {
    let mut map = HashMap::new();
    for (effect_index, effect) in effects.iter().enumerate() {
        for (stage_index, indices) in effect.stage_indices.iter().enumerate() {
            for shader_index in indices {
                let effect_indices = map
                    .entry((stage_index as u8, *shader_index))
                    .or_insert_with(Vec::new);
                if effect_indices.last() != Some(&effect_index) {
                    effect_indices.push(effect_index);
                }
            }
        }
    }
    map
}

pub fn load_sgd5(path: &Path) -> Result<Vec<Sgd5File>, Sgd5Error> {
    let data =
        fs::read(path).map_err(|error| Sgd5Error(format!("read {}: {error}", path.display())))?;
    parse_sgd5_streams(path, &data)
}

fn parse_sgd5_data(path: &Path, data: &[u8]) -> Result<Sgd5File, Sgd5Error> {
    let mut reader = Reader::new(data);
    if reader.take(4)? != MAGIC {
        return Err(Sgd5Error(format!(
            "{} is not an SGD5 shader-group file",
            path.display()
        )));
    }
    let group_hash = reader.u64()?;
    let binding_table_count = reader.u32()? as usize;
    reader.require_records(binding_table_count, 28)?;
    let mut binding_tables = Vec::with_capacity(binding_table_count.min(256));
    for _ in 0..binding_table_count {
        binding_tables.push(parse_binding_table(&mut reader)?);
    }
    let mut shared_reflection = parse_reflection_table(&mut reader)?;
    let mut shaders = Vec::new();
    let mut stage_counts = [0u32; 6];
    for (stage_index, (stage, _)) in STAGES.iter().enumerate() {
        let count = reader.u32()?;
        reader.require_records(count as usize, 1)?;
        stage_counts[stage_index] = count;
        for index in 0..count {
            let offset = reader.position;
            shaders.push(parse_shader(&mut reader, stage, index).map_err(|error| {
                Sgd5Error(format!("{stage} shader {index} at 0x{offset:x}: {error}"))
            })?);
        }
    }
    let declared_effect_count = reader.u32()?;
    reader.require_records(declared_effect_count as usize, 1)?;
    let mut effects = Vec::with_capacity(declared_effect_count.min(1_024) as usize);
    for index in 0..declared_effect_count {
        let offset = reader.position;
        effects.push(
            parse_effect(&mut reader, &stage_counts)
                .map_err(|error| Sgd5Error(format!("effect {index} at 0x{offset:x}: {error}")))?,
        );
    }
    let trailer_flags = reader.u32()?;
    let trailer_reserved = reader.array()?;
    if !matches!(trailer_flags, 0 | 1) || trailer_reserved != [0; 6] {
        return Err(Sgd5Error(format!(
            "invalid SGD5 stream trailer at 0x{:x}",
            reader.position - 10
        )));
    }

    let hash_names = build_hash_names(&shared_reflection, &shaders, &effects);
    resolve_reflection_names(&mut shared_reflection, &hash_names);
    let mut table_hashes = HashMap::new();
    for (index, table) in binding_tables.iter_mut().enumerate() {
        if table_hashes
            .insert(table.header.table_hash, index)
            .is_some()
        {
            return Err(Sgd5Error(format!(
                "duplicate binding table hash {:08x}",
                table.header.table_hash
            )));
        }
        resolve_binding_names(table, &hash_names);
    }
    for effect in &mut effects {
        resolve_reflection_names(&mut effect.main_reflection, &hash_names);
        resolve_reflection_names(&mut effect.secondary_reflection, &hash_names);
        for property in &mut effect.property_list.entries {
            match property {
                Sgd5PropertyEntry::Inline(table) => resolve_binding_names(table, &hash_names),
                Sgd5PropertyEntry::TableReference { table_hash, .. } => {
                    if !table_hashes.contains_key(table_hash) {
                        return Err(Sgd5Error(format!(
                            "effect {}: unknown binding table hash {table_hash:08x}",
                            effect.name
                        )));
                    }
                }
            }
        }
    }
    for shader in &mut shaders {
        resolve_reflection_names(&mut shader.reflection, &hash_names);
    }
    let effect_shader_map = build_effect_shader_map(&effects);
    Ok(Sgd5File {
        data: data[..reader.position].to_vec(),
        group_hash,
        binding_tables,
        shared_reflection,
        shaders,
        declared_effect_count,
        effect_count: effects.len() as u32,
        trailer_flags,
        trailer_reserved,
        effects,
        hash_names,
        effect_shader_map,
    })
}

fn parse_sgd5_streams(path: &Path, data: &[u8]) -> Result<Vec<Sgd5File>, Sgd5Error> {
    let mut files = Vec::new();
    let mut offset = 0;
    while let Some(found) = data[offset..]
        .windows(MAGIC.len())
        .position(|window| window == MAGIC)
    {
        let start = offset + found;
        let file = parse_sgd5_data(path, &data[start..]).map_err(|error| {
            Sgd5Error(format!(
                "SGD5 stream {} at 0x{start:x}: {error}",
                files.len()
            ))
        })?;
        offset = start + file.data.len();
        files.push(file);
    }
    if files.is_empty() {
        return Err(Sgd5Error("no valid SGD5 data".into()));
    }
    Ok(files)
}

pub fn decompress_shader(file: &Sgd5File, shader: &Sgd5Shader) -> Result<Vec<u8>, Sgd5Error> {
    let archive = &shader.archive;
    if archive.decompressed_size > MAX_SHADER_BYTES {
        return Err(Sgd5Error(format!(
            "{}: decompressed size exceeds 64 MiB",
            shader.name
        )));
    }
    let mut reader = Reader::new(&file.data);
    reader.take(archive.offset)?;
    let mut section = reader.section(archive.archive_size as usize)?;
    section.take(12)?;
    let compressed = section.take(section.remaining())?;
    let mut decoder = DeflateDecoder::new(compressed);
    let mut output = Vec::new();
    decoder
        .by_ref()
        .take(archive.decompressed_size as u64 + 1)
        .read_to_end(&mut output)
        .map_err(|error| Sgd5Error(format!("{}: DEFLATE error: {error}", shader.name)))?;
    if output.len() != archive.decompressed_size as usize {
        return Err(Sgd5Error(format!(
            "{}: decompressed {} bytes, expected {}",
            shader.name,
            output.len(),
            archive.decompressed_size
        )));
    }
    if output.len() < 32 || &output[..4] != b"DXBC" {
        return Err(Sgd5Error(format!(
            "{}: decompressed payload is not DXBC",
            shader.name
        )));
    }
    Ok(output)
}

fn safe_name(name: &str) -> String {
    let mut output = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.') {
            output.push(character);
        } else {
            output.push('_');
        }
    }
    let trimmed = output.trim_matches('_');
    if trimmed.is_empty() {
        "shader".to_string()
    } else {
        trimmed.to_string()
    }
}

fn extract_sgd5_file(
    file: &Sgd5File,
    output_dir: &Path,
    limit: Option<usize>,
    manifest_name: &str,
) -> Result<usize, Sgd5Error> {
    fs::create_dir_all(output_dir)
        .map_err(|error| Sgd5Error(format!("create {}: {error}", output_dir.display())))?;

    let selected = limit.map_or(&file.shaders[..], |limit| {
        &file.shaders[..limit.min(file.shaders.len())]
    });

    let mut manifest = String::from(
        "index,stage,name,wave_size,archive_size,decompressed_size,sga_hash,resources,members,auxiliary_records,filename\n",
    );
    for shader in selected {
        let blob = decompress_shader(file, shader)?;
        let filename = format!(
            "{:05}_{}_{}.dxbc",
            shader.index,
            shader.stage.to_lowercase(),
            safe_name(&shader.name)
        );
        fs::write(output_dir.join(&filename), &blob)
            .map_err(|error| Sgd5Error(format!("write {filename}: {error}")))?;
        let _ = writeln!(
            manifest,
            "{},{},{},{},{},{},0x{:016x},{},{},{},{}",
            shader.index,
            shader.stage,
            shader.name,
            shader.wave_size,
            shader.archive.archive_size,
            shader.archive.decompressed_size,
            shader.sga_hash,
            shader.reflection.resource_count,
            shader.reflection.member_count,
            shader.reflection.auxiliary_record_count,
            filename
        );
    }
    fs::write(output_dir.join(manifest_name), manifest)
        .map_err(|error| Sgd5Error(format!("write {manifest_name}: {error}")))?;

    Ok(selected.len())
}

pub fn extract_sgd5(
    input: &Path,
    output_dir: &Path,
    limit: usize,
) -> Result<Sgd5Extraction, Sgd5Error> {
    let data =
        fs::read(input).map_err(|error| Sgd5Error(format!("read {}: {error}", input.display())))?;
    let files = parse_sgd5_streams(input, &data)?;
    fs::create_dir_all(output_dir)
        .map_err(|error| Sgd5Error(format!("create {}: {error}", output_dir.display())))?;

    let mut remaining = if limit == 0 { None } else { Some(limit) };
    let mut extracted_count = 0usize;
    let mut manifests = Vec::new();
    for (stream_index, file) in files.iter().enumerate() {
        let single_output = if files.len() == 1 {
            output_dir.to_path_buf()
        } else {
            output_dir.join(format!("sgd5_{stream_index:03}"))
        };
        let manifest_name = "sgd5_index.csv";
        let extracted = extract_sgd5_file(file, &single_output, remaining, manifest_name)?;
        if let Some(remaining) = remaining.as_mut() {
            *remaining = remaining.saturating_sub(extracted);
        }
        extracted_count += extracted;
        manifests.push(single_output.join(manifest_name));
    }

    Ok(Sgd5Extraction {
        files,
        extracted_count,
        manifests,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_reflection() -> Vec<u8> {
        let mut data = 9u32.to_le_bytes().to_vec();
        data.extend_from_slice(&[0, 0, 0, 0, 0, 0, 9, 0, 0]);
        data
    }

    fn empty_stream() -> Vec<u8> {
        let mut data = MAGIC.to_vec();
        data.extend_from_slice(&0u64.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend(empty_reflection());
        data.extend_from_slice(&[0; 28]);
        data.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        data
    }

    fn binding_table(count: u16) -> Vec<u8> {
        let size = 24 + count as u32 * 8;
        let mut data = size.to_le_bytes().to_vec();
        data.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
        data.extend_from_slice(&count.to_le_bytes());
        data.extend_from_slice(&[0; 6]);
        data.extend_from_slice(&0xaabb_ccddu32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        for index in 0..count {
            data.extend_from_slice(&(index as u32).to_le_bytes());
            data.extend_from_slice(&0x0102u16.to_le_bytes());
            data.extend_from_slice(&index.to_le_bytes());
        }
        data
    }

    fn resource(words: [u16; 8], hash: u32, secondary_hash: u32) -> Vec<u8> {
        let mut data = Vec::new();
        for word in words {
            data.extend_from_slice(&word.to_le_bytes());
        }
        data.extend_from_slice(&hash.to_le_bytes());
        data.extend_from_slice(&secondary_hash.to_le_bytes());
        data
    }

    #[test]
    fn reflection_preserves_sections_offsets_and_untyped_defaults() {
        let pool = b"\0Buffer\0Member\0";
        let size = 8 + 48 + 16 + pool.len() + 8;
        let mut data = vec![0; 7];
        data.extend_from_slice(&(size as u32).to_le_bytes());
        data.extend_from_slice(&[1, 0, 1, 0, 2, 0]);
        data.extend_from_slice(&(size as u16).to_le_bytes());
        data.extend(resource(
            [0x0430, 0x0105, 0x4001, 24, 65, 0xffff, 16, 48],
            joaat_lower("Buffer"),
            0x8765_4321,
        ));
        data.extend(resource(
            [0x0109, 0, 12, 0x55aa, 0xffff, 55, 32, 0xbeef],
            joaat_lower("Member"),
            0x1234_5678,
        ));
        data.extend_from_slice(&0x1c57_54ecu32.to_le_bytes());
        data.extend_from_slice(&0x050fu32.to_le_bytes());
        data.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(pool);
        data.extend_from_slice(&1.25f32.to_le_bytes());
        data.extend_from_slice(&0xdead_beefu32.to_le_bytes());
        data.extend_from_slice(&0x1122_3344u32.to_le_bytes());
        let mut reader = Reader::new(&data);
        reader.take(7).unwrap();
        let table = parse_reflection_table(&mut reader).unwrap();
        assert_eq!(table.offset, 7);
        assert_eq!(table.resources[0].name.as_deref(), Some("Buffer"));
        assert_eq!(table.resources[0].slot, 5);
        assert_eq!(table.resources[0].auxiliary_record_count, 1);
        assert_eq!(table.resources[0].member_count, 1);
        assert_eq!(table.resources[0].flags, 0x40);
        assert_eq!(table.resources[0].reserved_0a, 0xffff);
        assert_eq!(table.resources[0].cbuffer_size, 16);
        assert_eq!(table.resources[0].auxiliary_data_offset, 48);
        assert_eq!(table.resources[0].secondary_hash, 0x8765_4321);
        assert_eq!(table.members[0].name.as_deref(), Some("Member"));
        assert_eq!(table.members[0].byte_offset, 12);
        assert_eq!(table.members[0].value_type, 9);
        assert_eq!(table.members[0].auxiliary_record_count, 1);
        assert_eq!(table.members[0].reserved_06, 0x55aa);
        assert_eq!(table.members[0].reserved_08, 0xffff);
        assert_eq!(table.members[0].reserved_0e, 0xbeef);
        assert_eq!(table.members[0].auxiliary_data_offset, 32);
        assert_eq!(table.auxiliary_records[0].data, 0x050f);
        assert_eq!(table.default_value_data.len(), 8);
        assert_eq!(table.default_value_data[4..], 0xdead_beefu32.to_le_bytes());
        assert_eq!(table.string_pool, pool);
        assert_eq!(reader.u32().unwrap(), 0x1122_3344);
    }

    #[test]
    fn property_tables_use_the_same_binding_model_and_u16_count() {
        let inline = binding_table(257);
        let table = parse_binding_table(&mut Reader::new(&inline)).unwrap();
        assert_eq!(table.hash, 0x1122_3344_5566_7788);
        assert_eq!(table.header.table_hash, 0xaabb_ccdd);
        assert_eq!(table.entries.len(), 257);
        assert_eq!(table.entries[256].binding_index, 256);
        assert_eq!(table.entries[0].binding_type, 0x0102);
        let mut data = vec![2, 0, 0x12, 0x34];
        data.extend_from_slice(&TABLE_REFERENCE.to_le_bytes());
        data.extend_from_slice(&table.header.table_hash.to_le_bytes());
        data.extend(inline);
        data.extend_from_slice(&0u32.to_le_bytes());
        let list = parse_property_list(&mut Reader::new(&data)).unwrap();
        assert_eq!(list.flags, 0x3412);
        assert!(matches!(
            list.entries[0],
            Sgd5PropertyEntry::TableReference {
                table_hash: 0xaabb_ccdd,
                ..
            }
        ));
        let Sgd5PropertyEntry::Inline(table) = &list.entries[1] else {
            panic!("missing inline table")
        };
        assert_eq!(table.entries.len(), 257);
    }

    #[test]
    fn table_reads_cannot_cross_their_declared_extent() {
        let mut data = empty_reflection();
        data[4..6].copy_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&[0; 64]);
        assert!(parse_reflection_table(&mut Reader::new(&data)).is_err());
        let mut table = binding_table(1);
        table[..4].copy_from_slice(&24u32.to_le_bytes());
        assert!(parse_binding_table(&mut Reader::new(&table)).is_err());
        let mut reader = Reader::new(&data);
        assert!(reader.take(usize::MAX).is_err());
        let mut short = empty_reflection();
        short[10..12].copy_from_slice(&8u16.to_le_bytes());
        assert!(parse_reflection_table(&mut Reader::new(&short)).is_err());
    }

    #[test]
    fn shader_archive_checks_both_sizes_and_header_extent() {
        let mut data = COMPRESSED_MAGIC.to_le_bytes().to_vec();
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&32u32.to_le_bytes());
        data.extend_from_slice(b"SGD5");
        let archive = parse_shader_archive(&mut Reader::new(&data), 16).unwrap();
        assert_eq!(archive.decompressed_size, 32);
        assert!(parse_shader_archive(&mut Reader::new(&data), 8).is_err());
        data[4..8].copy_from_slice(&15u32.to_le_bytes());
        assert!(parse_shader_archive(&mut Reader::new(&data), 16).is_err());
        data[4..8].copy_from_slice(&16u32.to_le_bytes());
        data[8..12].copy_from_slice(&(MAX_SHADER_BYTES + 1).to_le_bytes());
        assert!(parse_shader_archive(&mut Reader::new(&data), 16).is_err());
    }

    #[test]
    fn property_list_truncation_never_reads_past_the_input() {
        let mut data = vec![2, 0, 0, 0];
        data.extend(binding_table(1));
        data.extend_from_slice(&TABLE_REFERENCE.to_le_bytes());
        data.extend_from_slice(&0xaabb_ccddu32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        for length in 0..data.len() {
            assert!(parse_property_list(&mut Reader::new(&data[..length])).is_err());
        }
        assert!(parse_property_list(&mut Reader::new(&data)).is_ok());
    }

    #[test]
    fn effect_metadata_walks_extensions_continuations_and_state_chunks() {
        let mut data = vec![0; 0x5c];
        data[0x50..0x54].copy_from_slice(&1u32.to_le_bytes());
        let mut extension = [0; 0x10];
        extension[4..8].copy_from_slice(&[1, 1, 0, 0]);
        extension[0x0c..0x10].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        data.extend_from_slice(&extension);
        let mut continuation = [0; 0x18];
        continuation[0x14..0x18].copy_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&continuation);
        data.extend_from_slice(&[0; 0x44]);
        data.extend_from_slice(&[0; 16]);
        let metadata_size = data.len();
        data.extend_from_slice(&TABLE_REFERENCE.to_le_bytes());
        let mut reader = Reader::new(&data);
        assert_eq!(
            parse_effect_metadata(&mut reader).unwrap().len(),
            metadata_size
        );
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn stream_scanning_skips_magic_inside_shader_payloads() {
        let mut stream = empty_stream();
        stream.truncate(29);
        stream.extend_from_slice(&1u32.to_le_bytes());
        stream.extend_from_slice(&[3, b'V', b'S', 0, 32]);
        stream.extend_from_slice(&16u32.to_le_bytes());
        stream.extend_from_slice(&COMPRESSED_MAGIC.to_le_bytes());
        stream.extend_from_slice(&16u32.to_le_bytes());
        stream.extend_from_slice(&32u32.to_le_bytes());
        stream.extend_from_slice(MAGIC);
        stream.extend_from_slice(&0u64.to_le_bytes());
        stream.extend_from_slice(&[0; 0x90]);
        stream.extend(empty_reflection());
        stream.extend_from_slice(&[0; 24]);
        stream.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let expected_size = stream.len();
        stream.extend_from_slice(&[0; 7]);
        stream.extend(empty_stream());
        let files = parse_sgd5_streams(Path::new("synthetic"), &stream).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].data.len(), expected_size);
        assert_eq!(files[0].shaders.len(), 1);
        assert_eq!(files[0].shaders[0].root_signature_data, [0; 0x90]);
    }

    #[test]
    fn truncated_streams_return_errors_and_effect_counts_are_enforced() {
        let data = empty_stream();
        for length in 0..data.len() {
            assert!(parse_sgd5_data(Path::new("synthetic"), &data[..length]).is_err());
        }
        assert!(parse_sgd5_data(Path::new("synthetic"), &data).is_ok());
        let mut invalid = data.clone();
        invalid[53..57].copy_from_slice(&1u32.to_le_bytes());
        assert!(parse_sgd5_data(Path::new("synthetic"), &invalid).is_err());
    }

    #[test]
    #[ignore = "requires PERCY_SGD5_SAMPLE pointing to the analysis archive"]
    fn sample_archive_uses_common_tables_and_decompresses_each_stage() {
        let path =
            PathBuf::from(std::env::var_os("PERCY_SGD5_SAMPLE").expect("set PERCY_SGD5_SAMPLE"));
        let files = load_sgd5(&path).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(
            files.iter().map(|file| file.shaders.len()).sum::<usize>(),
            26_503
        );
        assert_eq!(
            files.iter().map(|file| file.effects.len()).sum::<usize>(),
            932
        );
        let mut reference_count = 0;
        let mut auxiliary_count = 0;
        let mut table_count = 0;
        let mut member_link_count = 0;
        for file in &files {
            let tables =
                std::iter::once(&file.shared_reflection)
                    .chain(file.shaders.iter().map(|shader| &shader.reflection))
                    .chain(file.effects.iter().flat_map(|effect| {
                        [&effect.main_reflection, &effect.secondary_reflection]
                    }));
            for table in tables {
                table_count += 1;
                let mut linked_auxiliary_count = 0;
                for resource in &table.resources {
                    assert_eq!(resource.reserved_0a, 0);
                    assert_eq!(resource.member_count == 0, resource.member_data_offset == 0);
                    if resource.member_count != 0 {
                        member_link_count += 1;
                        for index in 0..resource.member_count as usize {
                            let target =
                                resource.offset + resource.member_data_offset as usize + index * 24;
                            assert!(table.members.iter().any(|member| member.offset == target));
                        }
                    }
                    assert_eq!(
                        resource.auxiliary_record_count == 0,
                        resource.auxiliary_data_offset == 0
                    );
                    for index in 0..resource.auxiliary_record_count as usize {
                        let target =
                            resource.offset + resource.auxiliary_data_offset as usize + index * 8;
                        assert!(table
                            .auxiliary_records
                            .iter()
                            .any(|record| record.offset == target));
                    }
                    linked_auxiliary_count += resource.auxiliary_record_count as usize;
                }
                for member in &table.members {
                    assert_eq!(member.reserved_06, 0);
                    assert_eq!(member.reserved_08, 0);
                    assert_eq!(member.reserved_0e, 0);
                    assert_eq!(
                        member.auxiliary_record_count == 0,
                        member.auxiliary_data_offset == 0
                    );
                    for index in 0..member.auxiliary_record_count as usize {
                        let target =
                            member.offset + member.auxiliary_data_offset as usize + index * 8;
                        assert!(table
                            .auxiliary_records
                            .iter()
                            .any(|record| record.offset == target));
                    }
                    linked_auxiliary_count += member.auxiliary_record_count as usize;
                }
                assert_eq!(linked_auxiliary_count, table.auxiliary_records.len());
            }
            assert_eq!(file.declared_effect_count, file.effects.len() as u32);
            assert!(file.shared_reflection.default_value_data.is_empty());
            for stage in STAGES {
                if let Some(shader) = file.shaders.iter().find(|shader| shader.stage == stage.0) {
                    assert_eq!(&decompress_shader(file, shader).unwrap()[..4], b"DXBC");
                    assert!(shader.reflection.default_value_data.is_empty());
                    let metadata = crate::sgd5_metadata::format_shader_metadata(file, shader);
                    assert!(metadata.contains("DXBC input/output signatures"));
                    assert!(!metadata.contains("DXBC signatures unavailable"));
                    assert!(metadata.contains("Binding tables"));
                }
            }
            for effect in &file.effects {
                assert_eq!(effect.secondary_reflection.resource_count, 1);
                assert_eq!(
                    effect.secondary_reflection.resources[0].name_hash,
                    0xe435_592b
                );
                assert!(effect.secondary_reflection.default_value_data.is_empty());
                auxiliary_count += effect.main_reflection.auxiliary_records.len();
                reference_count += effect
                    .property_list
                    .entries
                    .iter()
                    .filter(|entry| matches!(entry, Sgd5PropertyEntry::TableReference { .. }))
                    .count();
            }
        }
        assert_eq!(reference_count, 3_247);
        assert_eq!(auxiliary_count, 5_605);
        assert_eq!(table_count, 28_369);
        assert_eq!(member_link_count, 94_397);
    }
}
