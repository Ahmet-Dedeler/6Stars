use crate::sgd5::{
    Sgd5BindingTable, Sgd5File, Sgd5MemberRecord, Sgd5PropertyEntry, Sgd5ReflectionTable,
    Sgd5ResourceRecord, Sgd5Shader,
};
use std::collections::HashSet;
use std::fmt::Write as _;

pub fn resource_type_name(resource_type: u16) -> &'static str {
    match resource_type & !0x80 {
        0x0002 => "Texture1D",
        0x0102 => "Texture2D",
        0x0142 => "Texture2DArray",
        0x0202 => "TextureCube",
        0x0242 => "TextureCubeArray",
        0x0302 => "Texture3D",
        0x0401 => "Buffer",
        0x0405 => "StructuredBuffer",
        0x0407 => "ByteAddressBuffer",
        0x001c => "RWTexture1D",
        0x011c => "RWTexture2D",
        0x015c => "RWTexture2DArray",
        0x031c => "RWTexture3D",
        0x040e => "Buffer subtype 0x0e",
        0x0414 => "RWStructuredBuffer",
        0x0416 => "RWStructuredBuffer subtype 0x16",
        0x0418 => "RWByteAddressBuffer",
        0x0423 => "SamplerState",
        0x0430 => "ConstantBuffer",
        _ => "unknown resource type",
    }
}

fn value_type(value_type: u8) -> (&'static str, Option<usize>) {
    match value_type {
        0 => ("bool", Some(4)),
        1 => ("uint", Some(4)),
        2 => ("uint2", Some(8)),
        3 => ("uint3", Some(12)),
        4 => ("uint4", Some(16)),
        5 => ("int", Some(4)),
        6 => ("int2", Some(8)),
        7 => ("int3", Some(12)),
        8 => ("int4", Some(16)),
        9 => ("float", Some(4)),
        10 => ("float2", Some(8)),
        11 => ("float3", Some(12)),
        12 => ("float4", Some(16)),
        13 => ("float4x3", Some(48)),
        14 => ("float4x4", Some(64)),
        _ => ("unknown value type", None),
    }
}

fn member_at(table: &Sgd5ReflectionTable, offset: usize) -> Option<&Sgd5MemberRecord> {
    let first = table.members.first()?.offset;
    let relative = offset.checked_sub(first)?;
    if relative % 24 != 0 {
        return None;
    }
    table
        .members
        .get(relative / 24)
        .filter(|member| member.offset == offset)
}

fn name(file: &Sgd5File, hash: u32, resolved: Option<&str>) -> String {
    resolved
        .or_else(|| file.resolve_name(hash))
        .unwrap_or("unresolved")
        .to_string()
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset.checked_add(4).ok_or("signature offset overflow")?;
    let bytes = data.get(offset..end).ok_or("truncated signature data")?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

pub fn format_dxbc_signatures(data: &[u8]) -> Result<String, String> {
    if data.get(..4) != Some(b"DXBC") {
        return Err("not a DXBC container".into());
    }
    let size = read_u32(data, 24)? as usize;
    let data = data
        .get(..size)
        .filter(|data| data.len() >= 32)
        .ok_or("invalid DXBC extent")?;
    let count = read_u32(data, 28)? as usize;
    let table_end = count
        .checked_mul(4)
        .and_then(|size| size.checked_add(32))
        .filter(|end| *end <= data.len())
        .ok_or("invalid DXBC chunk count")?;
    let mut text = String::from(
        "DXBC input/output signatures (signature registers, not ISA VGPR assignments)\n",
    );
    let mut found = false;
    for table_offset in (32..table_end).step_by(4) {
        let chunk_offset = read_u32(data, table_offset)? as usize;
        let tag = data
            .get(chunk_offset..chunk_offset.checked_add(4).ok_or("chunk offset overflow")?)
            .ok_or("invalid DXBC chunk offset")?;
        let (kind, extended) = match tag {
            b"ISG1" => ("input", true),
            b"OSG1" => ("output", true),
            b"PSG1" => ("patch constant", true),
            b"ISGN" => ("input", false),
            b"OSGN" => ("output", false),
            b"PCSG" => ("patch constant", false),
            _ => continue,
        };
        let payload_size = read_u32(
            data,
            chunk_offset.checked_add(4).ok_or("chunk offset overflow")?,
        )? as usize;
        let payload_start = chunk_offset.checked_add(8).ok_or("chunk offset overflow")?;
        let payload_end = payload_start
            .checked_add(payload_size)
            .ok_or("signature size overflow")?;
        let payload = data
            .get(payload_start..payload_end)
            .ok_or("truncated signature chunk")?;
        let element_count = read_u32(payload, 0)? as usize;
        if element_count > 4096 {
            return Err("signature element count exceeds metadata limit".into());
        }
        let elements_start = read_u32(payload, 4)? as usize;
        let stride = if extended { 32 } else { 24 };
        let elements_end = element_count
            .checked_mul(stride)
            .and_then(|size| elements_start.checked_add(size))
            .filter(|end| *end <= payload.len() && elements_start >= 8)
            .ok_or("invalid signature element count")?;
        found = true;
        let _ = writeln!(text, "  {kind}: {element_count} elements");
        for offset in (elements_start..elements_end).step_by(stride) {
            let row = &payload[offset..offset + stride];
            let base = if extended { 4 } else { 0 };
            let name_offset = read_u32(row, base)? as usize;
            if name_offset < elements_end {
                return Err("signature name overlaps records".into());
            }
            let bytes = payload
                .get(name_offset..)
                .ok_or("invalid signature name offset")?;
            let end = bytes
                .iter()
                .position(|byte| *byte == 0)
                .ok_or("unterminated signature name")?;
            if end > 1024 || text.len() > (4 << 20) {
                return Err("signature text exceeds metadata limit".into());
            }
            let semantic =
                std::str::from_utf8(&bytes[..end]).map_err(|_| "invalid signature name")?;
            let semantic_index = read_u32(row, base + 4)?;
            let system_value = read_u32(row, base + 8)?;
            let component_type = match read_u32(row, base + 12)? {
                1 => "uint32",
                2 => "int32",
                3 => "float32",
                _ => "unknown",
            };
            let register = read_u32(row, base + 16)?;
            let register = if register == u32::MAX {
                "none".into()
            } else {
                register.to_string()
            };
            let stream = if extended { read_u32(row, 0)? } else { 0 };
            let _ = writeln!(text, "    {semantic}[{semantic_index}] stream={stream} signature_register={register} mask=0x{:02x} rw_mask=0x{:02x} component={component_type} system_value={system_value}", row[base + 20], row[base + 21]);
        }
    }
    if !found {
        text.push_str("  no supported signature chunks\n");
    }
    Ok(text)
}

fn write_member(
    text: &mut String,
    file: &Sgd5File,
    table: &Sgd5ReflectionTable,
    member: &Sgd5MemberRecord,
) {
    let (type_name, width) = value_type(member.value_type);
    let array = if member.array_size == 0 {
        String::new()
    } else {
        format!("[{}]", member.array_size)
    };
    let extent = width.map(|width| width * member.array_size.max(1) as usize);
    let _ = writeln!(text, "    {}{} {} | type=0x{:02x} byte_offset={} byte_size={} name_hash=0x{:08x} secondary_hash=0x{:08x}",
        type_name, array, name(file, member.name_hash, member.name.as_deref()), member.value_type,
        member.byte_offset, extent.map(|size| size.to_string()).unwrap_or_else(|| "unknown".into()), member.name_hash, member.secondary_hash);
    if member.default_value_offset != 0 {
        let target = member.offset + member.default_value_offset as usize;
        let table_end = table.offset + 4 + table.size as usize;
        let size = extent.unwrap_or(4);
        let available = table_end.saturating_sub(target);
        let preview_size = size.min(16).min(available);
        if target >= table.default_values_offset && target < table_end {
            if let Some(bytes) = file.data.get(target..target + preview_size) {
                let preview = bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let _ = writeln!(
                    text,
                    "      default data at 0x{target:x}: {preview}{}",
                    if size > preview_size { " ..." } else { "" }
                );
            }
        }
    }
}

fn write_resource(
    text: &mut String,
    file: &Sgd5File,
    table: &Sgd5ReflectionTable,
    record: &Sgd5ResourceRecord,
) {
    let _ = writeln!(text, "  {} {} | type=0x{:04x} metadata_slot={} flags=0x{:02x} name_hash=0x{:08x} secondary_hash=0x{:08x} cbuffer_bytes={} members={} auxiliary_records={}",
        resource_type_name(record.resource_type), name(file, record.name_hash, record.name.as_deref()),
        record.resource_type, record.slot, record.flags, record.name_hash, record.secondary_hash,
        record.cbuffer_size, record.member_count, record.auxiliary_record_count);
    if record.member_count != 0 {
        let start = record.offset + record.member_data_offset as usize;
        for index in 0..record.member_count as usize {
            if let Some(member) = member_at(table, start + index * 24) {
                write_member(text, file, table, member);
            }
        }
    }
}

fn write_binding_table(
    text: &mut String,
    index: usize,
    table: &Sgd5BindingTable,
    source: &str,
    shader_hashes: &HashSet<u32>,
    file: &Sgd5File,
) {
    let header = &table.header;
    let matches = table
        .entries
        .iter()
        .filter(|entry| shader_hashes.contains(&entry.name_hash))
        .count();
    let _ = writeln!(text, "  table[{index}] {source} signature=0x{:016x} entries={} SRV={} UAV={} sampler={} CBV={} matching_shader_hashes={matches}",
        table.hash, header.entry_count, header.srv_count, header.uav_count, header.sampler_count, header.cbv_count);
    for entry in table
        .entries
        .iter()
        .filter(|entry| shader_hashes.contains(&entry.name_hash))
    {
        let _ = writeln!(
            text,
            "    {} | name_hash=0x{:08x} binding_type=0x{:04x} table_index={}",
            name(file, entry.name_hash, entry.name.as_deref()),
            entry.name_hash,
            entry.binding_type,
            entry.binding_index
        );
    }
}

pub fn format_shader_metadata(file: &Sgd5File, shader: &Sgd5Shader) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "SGD5 container metadata");
    let _ = writeln!(
        text,
        "Shader: {} #{} {}",
        shader.stage, shader.index, shader.name
    );
    let _ = writeln!(
        text,
        "Group hash: 0x{:016x} | SGA hash: 0x{:016x} | wave-size byte: {}",
        file.group_hash, shader.sga_hash, shader.wave_size
    );
    let _ = writeln!(
        text,
        "Archive: {} bytes -> {} bytes | reflection at 0x{:x}",
        shader.archive.archive_size, shader.archive.decompressed_size, shader.reflection.offset
    );
    let _ = writeln!(
        text,
        "Resource types describe SGD5 metadata; texture/buffer element types are not inferred."
    );
    let _ = writeln!(text, "Metadata slots and binding-table indices have no confirmed mapping to ISA descriptors or HLSL registers.");
    let _ = writeln!(text, "Secondary hashes and the 144-byte shader metadata remain opaque. Default bytes are initialization data candidates.");
    text.push('\n');
    match crate::sgd5::decompress_shader(file, shader) {
        Ok(blob) => {
            match format_dxbc_signatures(&blob) {
                Ok(signatures) => text.push_str(&signatures),
                Err(error) => {
                    let _ = writeln!(text, "DXBC signatures unavailable: {error}");
                }
            }
            match crate::dxbc::extract_fssm(&blob) {
                Ok(program) => text.push_str(&crate::dxbc::format_fssm_metadata(&program)),
                Err(error) => {
                    let _ = writeln!(text, "FSSM program metadata unavailable: {error}");
                }
            }
        }
        Err(error) => {
            let _ = writeln!(text, "DXBC signatures unavailable: {error}");
        }
    }
    let _ = writeln!(
        text,
        "\nShader resources ({})",
        shader.reflection.resources.len()
    );
    for resource in &shader.reflection.resources {
        write_resource(&mut text, file, &shader.reflection, resource);
    }
    let referenced_members: HashSet<_> = shader
        .reflection
        .resources
        .iter()
        .flat_map(|resource| {
            (0..resource.member_count as usize).map(move |index| {
                resource.offset + resource.member_data_offset as usize + index * 24
            })
        })
        .collect();
    for member in shader
        .reflection
        .members
        .iter()
        .filter(|member| !referenced_members.contains(&member.offset))
    {
        write_member(&mut text, file, &shader.reflection, member);
    }
    let shader_hashes: HashSet<_> = shader
        .reflection
        .resources
        .iter()
        .map(|record| record.name_hash)
        .chain(
            shader
                .reflection
                .members
                .iter()
                .map(|record| record.name_hash),
        )
        .collect();
    let shared_matches = file
        .shared_reflection
        .resources
        .iter()
        .filter(|record| shader_hashes.contains(&record.name_hash))
        .count();
    let _ = writeln!(
        text,
        "\nShared reflection: {} resources, {} members; {shared_matches} resource hash matches",
        file.shared_reflection.resources.len(),
        file.shared_reflection.members.len()
    );
    for resource in file
        .shared_reflection
        .resources
        .iter()
        .filter(|record| shader_hashes.contains(&record.name_hash))
    {
        write_resource(&mut text, file, &file.shared_reflection, resource);
    }
    let stage_index = ["VS", "PS", "GS", "DS", "HS", "CS"]
        .iter()
        .position(|stage| *stage == shader.stage);
    let effect_indices =
        stage_index.and_then(|stage| file.effect_shader_map.get(&(stage as u8, shader.index)));
    let _ = writeln!(
        text,
        "\nEffects referencing this shader: {}",
        effect_indices.map_or(0, Vec::len)
    );
    for effect_index in effect_indices.into_iter().flatten() {
        let Some(effect) = file.effects.get(*effect_index) else {
            continue;
        };
        let _ = writeln!(
            text,
            "\nEffect: {} | data_buffer_size={}",
            effect.name, effect.data_buffer_size
        );
        let stages = ["VS", "PS", "GS", "DS", "HS", "CS"];
        let stage_counts = effect
            .stage_indices
            .iter()
            .enumerate()
            .filter(|(_, indices)| !indices.is_empty())
            .map(|(index, indices)| format!("{}={}", stages[index], indices.len()))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = writeln!(text, "  stage shader references: {stage_counts}");
        let _ = writeln!(
            text,
            "  reflection: {} resources, {} members, {} auxiliary records; {} default bytes",
            effect.main_reflection.resources.len(),
            effect.main_reflection.members.len(),
            effect.main_reflection.auxiliary_records.len(),
            effect.main_reflection.default_value_data.len()
        );
        let _ = writeln!(
            text,
            "  secondary reflection: {} resources, {} members, {} auxiliary records",
            effect.secondary_reflection.resources.len(),
            effect.secondary_reflection.members.len(),
            effect.secondary_reflection.auxiliary_records.len()
        );
        let _ = writeln!(
            text,
            "  effect string pool (technique/pass-name candidates): {}",
            effect.property_list.strings.join(", ")
        );
        for resource in &effect.main_reflection.resources {
            let target = resource.offset + resource.member_data_offset as usize;
            let member_match = (0..resource.member_count as usize).any(|index| {
                member_at(&effect.main_reflection, target + index * 24)
                    .is_some_and(|member| shader_hashes.contains(&member.name_hash))
            });
            if shader_hashes.contains(&resource.name_hash) || member_match {
                write_resource(&mut text, file, &effect.main_reflection, resource);
            }
        }
        let _ = writeln!(
            text,
            "  Binding tables (hash matches indicate shared identity, not register assignment):"
        );
        for (index, property) in effect.property_list.entries.iter().enumerate() {
            match property {
                Sgd5PropertyEntry::Inline(table) => {
                    write_binding_table(&mut text, index, table, "inline", &shader_hashes, file)
                }
                Sgd5PropertyEntry::TableReference { table_hash, .. } => {
                    if let Some(table) = file
                        .binding_tables
                        .iter()
                        .find(|table| table.header.table_hash == *table_hash)
                    {
                        write_binding_table(
                            &mut text,
                            index,
                            table,
                            &format!("shared hash=0x{table_hash:08x}"),
                            &shader_hashes,
                            file,
                        );
                    }
                }
            }
        }
    }
    text
}

pub fn prepend_comments(metadata: &str, prefix: &str, output: String) -> String {
    let mut text = String::with_capacity(metadata.len() + output.len() + 256);
    for line in metadata.lines() {
        let _ = writeln!(text, "{prefix} {line}");
    }
    text.push('\n');
    text.push_str(&output);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_family_labels_preserve_uncertain_element_types() {
        assert_eq!(resource_type_name(0x0182), "Texture2D");
        assert_eq!(resource_type_name(0x04b0), "ConstantBuffer");
        assert_eq!(resource_type_name(0x048e), "Buffer subtype 0x0e");
        assert_eq!(value_type(0x0f).1, None);
    }

    #[test]
    fn metadata_is_commented_without_creating_hlsl_bindings() {
        let text = prepend_comments(
            "metadata_slot=19\nfloat4[12] byte_offset=304",
            "//",
            "void VSMain() {}".into(),
        );
        assert!(text.starts_with("// metadata_slot=19\n// float4[12] byte_offset=304\n"));
        assert!(!text.contains("register(t19)"));
        assert!(text.ends_with("void VSMain() {}"));
    }

    #[test]
    fn signature_chunks_report_semantics_and_bound_all_offsets() {
        let mut payload = Vec::new();
        for word in [1u32, 8, 0, 40, 0, 0, 3, 3, 0x00000f0f, 0] {
            payload.extend_from_slice(&word.to_le_bytes());
        }
        payload.extend_from_slice(b"POSITION\0");
        let mut data = vec![0; 36];
        data[..4].copy_from_slice(b"DXBC");
        data[28..32].copy_from_slice(&1u32.to_le_bytes());
        data[32..36].copy_from_slice(&36u32.to_le_bytes());
        data.extend_from_slice(b"ISG1");
        data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        data.extend_from_slice(&payload);
        let size = data.len() as u32;
        data[24..28].copy_from_slice(&size.to_le_bytes());
        let text = format_dxbc_signatures(&data).unwrap();
        assert!(text.contains("POSITION[0] stream=0 signature_register=3 mask=0x0f"));
        assert!(text.contains("component=float32"));
        data[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(format_dxbc_signatures(&data).is_err());
        data[28..32].copy_from_slice(&1u32.to_le_bytes());
        data[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(format_dxbc_signatures(&data).is_err());
    }
}
