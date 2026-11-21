use flate2::read::DeflateDecoder;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"SGD5";
const COMPRESSED_MAGIC: u32 = 0x000D_DCE7;
const STAGES: [(&str, &str); 6] = [
    ("VS", "vs"),
    ("PS", "ps"),
    ("GS", "gs"),
    ("DS", "ds"),
    ("HS", "hs"),
    ("CS", "cs"),
];

#[derive(Clone, Debug)]
pub struct Sgd5Shader {
    pub stage: &'static str,
    pub index: u32,
    pub name: String,
    pub wave_size: u8,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub hash: u64,
    pub register_count: u16,
    pub cbuffer_count: u16,
    pub texture_count: u16,
    pub binary_offset: usize,
}

#[derive(Clone, Debug)]
pub struct Sgd5File {
    pub data: Vec<u8>,
    pub shaders: Vec<Sgd5Shader>,
    pub effect_count: u32,
    pub trailer_offset: usize,
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Sgd5Error {}

fn rd16(data: &[u8], offset: usize) -> Result<u16, Sgd5Error> {
    data.get(offset..offset + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| Sgd5Error(format!("truncated u16 at 0x{offset:x}")))
}

fn rd32(data: &[u8], offset: usize) -> Result<u32, Sgd5Error> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| Sgd5Error(format!("truncated u32 at 0x{offset:x}")))
}

fn rd64(data: &[u8], offset: usize) -> Result<u64, Sgd5Error> {
    data.get(offset..offset + 8)
        .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
        .ok_or_else(|| Sgd5Error(format!("truncated u64 at 0x{offset:x}")))
}

fn read_string(data: &[u8], offset: usize, length: usize) -> Result<String, Sgd5Error> {
    let bytes = data
        .get(offset..offset + length)
        .ok_or_else(|| Sgd5Error(format!("truncated shader name at 0x{offset:x}")))?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

fn find_arrays_offset(data: &[u8]) -> Result<usize, Sgd5Error> {
    let mut search = 0x20;
    while let Some(found) = data[search..]
        .windows(3)
        .position(|window| window == b"VS_")
        .map(|position| search + position)
    {
        let count_offset = found.saturating_sub(5);
        if found > 0x20
            && rd32(data, count_offset)? < 100_000
            && data.get(found - 1).is_some_and(|&length| length > 3)
        {
            return Ok(count_offset);
        }
        search = found + 1;
    }
    Err(Sgd5Error(
        "could not locate SGD5 vertex-shader array".into(),
    ))
}

fn parse_shader(
    data: &[u8],
    stage: &'static str,
    index: u32,
    offset: usize,
) -> Result<(Sgd5Shader, usize), Sgd5Error> {
    let name_length = *data
        .get(offset)
        .ok_or_else(|| Sgd5Error(format!("truncated shader at 0x{offset:x}")))?;
    let name = read_string(data, offset + 1, name_length as usize)?;
    let mut cursor = offset + 1 + name_length as usize;
    if name.is_empty() {
        return Err(Sgd5Error(format!("empty shader name at 0x{offset:x}")));
    }

    let wave_size = *data
        .get(cursor)
        .ok_or_else(|| Sgd5Error(format!("{name}: truncated wave size")))?;
    let compressed_size = rd32(data, cursor + 1)?;
    let binary_offset = cursor + 5;
    cursor = binary_offset + compressed_size as usize;
    let hash = rd64(data, cursor)?;
    cursor += 8;

    let root_signature_end = cursor
        .checked_add(144)
        .ok_or_else(|| Sgd5Error(format!("{name}: root-signature offset overflow")))?;
    if root_signature_end > data.len() {
        return Err(Sgd5Error(format!("{name}: truncated root signature")));
    }
    cursor = root_signature_end;

    let block_size = rd32(data, cursor)?;
    cursor += 4;
    let block_start = cursor;
    let register_count = rd16(data, cursor)?;
    let cbuffer_count = rd16(data, cursor + 2)?;
    let texture_count = rd16(data, cursor + 4)?;
    let block_size_copy = rd16(data, cursor + 6)?;
    let expected_block_size = 8usize
        .checked_add(
            24usize
                .checked_mul(register_count as usize + cbuffer_count as usize)
                .ok_or_else(|| Sgd5Error(format!("{name}: reflection count overflow")))?,
        )
        .and_then(|size| size.checked_add(1))
        .ok_or_else(|| Sgd5Error(format!("{name}: block size overflow")))?;
    if block_size as usize != expected_block_size {
        return Err(Sgd5Error(format!(
            "{name}: block size {} does not match {} registers and {} cbuffers",
            block_size, register_count, cbuffer_count
        )));
    }
    if block_size_copy != block_size as u16 {
        return Err(Sgd5Error(format!(
            "{name}: duplicate block size {block_size_copy} does not match {block_size}"
        )));
    }

    let uncompressed_size = rd32(data, binary_offset + 8)?;
    if rd32(data, binary_offset)? != COMPRESSED_MAGIC {
        return Err(Sgd5Error(format!(
            "{name}: invalid compressed-container magic"
        )));
    }
    if compressed_size < 12 {
        return Err(Sgd5Error(format!(
            "{name}: compressed container is too short"
        )));
    }

    cursor = block_start + block_size as usize;
    if cursor > data.len() {
        return Err(Sgd5Error(format!("{name}: truncated reflection block")));
    }

    Ok((
        Sgd5Shader {
            stage,
            index,
            name,
            wave_size,
            compressed_size,
            uncompressed_size,
            hash,
            register_count,
            cbuffer_count,
            texture_count,
            binary_offset,
        },
        cursor,
    ))
}

pub fn parse_sgd5(path: &Path) -> Result<Sgd5File, Sgd5Error> {
    let data =
        fs::read(path).map_err(|error| Sgd5Error(format!("read {}: {error}", path.display())))?;
    parse_sgd5_data(path, data)
}

pub fn load_sgd5(path: &Path) -> Result<Vec<Sgd5File>, Sgd5Error> {
    let data =
        fs::read(path).map_err(|error| Sgd5Error(format!("read {}: {error}", path.display())))?;
    parse_sgd5_streams(path, data)
}

fn parse_sgd5_data(path: &Path, data: Vec<u8>) -> Result<Sgd5File, Sgd5Error> {
    if data.len() < 0x20 || &data[..4] != MAGIC {
        return Err(Sgd5Error(format!(
            "{} is not an SGD5 shader-group file",
            path.display()
        )));
    }

    let mut offset = find_arrays_offset(&data)?;
    let mut shaders = Vec::new();
    for (stage, _) in STAGES {
        let count = rd32(&data, offset)?;
        offset += 4;
        if count > 100_000 {
            return Err(Sgd5Error(format!("invalid {stage} shader count {count}")));
        }
        for index in 0..count {
            let (shader, next_offset) = parse_shader(&data, stage, index, offset)?;
            shaders.push(shader);
            offset = next_offset;
        }
    }

    let effect_count = rd32(&data, offset)?;
    Ok(Sgd5File {
        data,
        shaders,
        effect_count,
        trailer_offset: offset + 4,
    })
}

fn find_sgd5_starts(data: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut offset = 0usize;
    while let Some(found) = data[offset..]
        .windows(MAGIC.len())
        .position(|window| window == MAGIC)
        .map(|position| offset + position)
    {
        starts.push(found);
        offset = found + 1;
    }

    let aligned: Vec<_> = starts
        .iter()
        .copied()
        .filter(|offset| *offset == 0 || offset % 0x1000 == 0)
        .collect();
    if aligned.is_empty() {
        starts
    } else {
        aligned
    }
}

fn parse_sgd5_streams(path: &Path, mut data: Vec<u8>) -> Result<Vec<Sgd5File>, Sgd5Error> {
    let starts = find_sgd5_starts(&data);
    let mut files = Vec::new();
    for start in starts.iter().rev() {
        let stream = data.split_off(*start);
        if let Ok(file) = parse_sgd5_data(path, stream) {
            files.push(file);
        }
    }
    drop(data);
    files.reverse();
    if files.is_empty() {
        return Err(Sgd5Error("no valid SGD5 data".into()));
    }
    Ok(files)
}

pub fn decompress_shader(file: &Sgd5File, shader: &Sgd5Shader) -> Result<Vec<u8>, Sgd5Error> {
    let compressed = file
        .data
        .get(shader.binary_offset + 12..shader.binary_offset + shader.compressed_size as usize)
        .ok_or_else(|| Sgd5Error(format!("{}: truncated compressed payload", shader.name)))?;
    let mut decoder = DeflateDecoder::new(compressed);
    let mut output = Vec::with_capacity(shader.uncompressed_size as usize);
    decoder
        .read_to_end(&mut output)
        .map_err(|error| Sgd5Error(format!("{}: DEFLATE error: {error}", shader.name)))?;
    if output.len() != shader.uncompressed_size as usize {
        return Err(Sgd5Error(format!(
            "{}: decompressed {} bytes, expected {}",
            shader.name,
            output.len(),
            shader.uncompressed_size
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
        "index,stage,name,wave_size,compressed_size,uncompressed_size,hash,registers,cbuffers,textures,filename\n",
    );
    for shader in selected {
        let blob = decompress_shader(&file, shader)?;
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
            shader.compressed_size,
            shader.uncompressed_size,
            shader.hash,
            shader.register_count,
            shader.cbuffer_count,
            shader.texture_count,
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
    let files = parse_sgd5_streams(input, data)?;
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
    use flate2::write::DeflateEncoder;
    use flate2::Compression;
    use std::io::Write as _;

    #[test]
    fn rejects_non_sgd5_data() {
        let error = parse_sgd5(Path::new("does-not-exist")).unwrap_err();
        assert!(error.to_string().contains("read"));
    }

    #[test]
    fn sanitizes_shader_names() {
        assert_eq!(safe_name("VS_Test/Name*"), "VS_Test_Name");
        assert_eq!(safe_name("///"), "shader");
    }

    fn synthetic_sgd5() -> (Vec<u8>, Vec<u8>) {
        let mut dxbc = b"DXBC synthesized shader payload".to_vec();
        dxbc.resize(64, 0);
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&dxbc).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut data = vec![0u8; 0x20];
        data[..4].copy_from_slice(MAGIC);
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&[8u8]);
        data.extend_from_slice(b"VS_Test\0");
        data.push(50);
        data.extend_from_slice(&(12 + compressed.len() as u32).to_le_bytes());
        data.extend_from_slice(&COMPRESSED_MAGIC.to_le_bytes());
        data.extend_from_slice(&(12 + compressed.len() as u32).to_le_bytes());
        data.extend_from_slice(&(dxbc.len() as u32).to_le_bytes());
        data.extend_from_slice(&compressed);
        data.extend_from_slice(&0x0123_4567_89ab_cdefu64.to_le_bytes());
        data.extend_from_slice(&[0u8; 144]);
        data.extend_from_slice(&9u32.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&9u16.to_le_bytes());
        data.push(0);
        for _ in 1..6 {
            data.extend_from_slice(&0u32.to_le_bytes());
        }
        data.extend_from_slice(&1u32.to_le_bytes());
        (data, dxbc)
    }

    #[test]
    fn parses_and_decompresses_synthetic_sgd5() {
        let (data, dxbc) = synthetic_sgd5();

        let file = parse_sgd5_data(Path::new("synthetic.sgd5"), data).unwrap();
        assert_eq!(file.shaders.len(), 1);
        assert_eq!(file.effect_count, 1);
        assert_eq!(file.shaders[0].name, "VS_Test");
        assert_eq!(file.shaders[0].wave_size, 50);
        assert_eq!(decompress_shader(&file, &file.shaders[0]).unwrap(), dxbc);
    }

    #[test]
    fn extracts_multiple_sgd5_groups() {
        let (first, _) = synthetic_sgd5();
        let (second, _) = synthetic_sgd5();
        let mut container = first.clone();
        container.resize(0x1000, 0);
        container.extend_from_slice(&second);

        let input =
            std::env::temp_dir().join(format!("percystar_sgd5_multi_{}.bin", std::process::id()));
        let output =
            std::env::temp_dir().join(format!("percystar_sgd5_multi_{}", std::process::id()));
        fs::write(&input, container).unwrap();
        let extraction = extract_sgd5(&input, &output, 0).unwrap();

        assert_eq!(extraction.files.len(), 2);
        assert_eq!(extraction.extracted_count, 2);
        assert_eq!(extraction.manifests.len(), 2);
        assert!(output.join("sgd5_000").join("sgd5_index.csv").is_file());
        assert!(output.join("sgd5_001").join("sgd5_index.csv").is_file());

        let limited_output =
            std::env::temp_dir().join(format!("percystar_sgd5_limited_{}", std::process::id()));
        let limited = extract_sgd5(&input, &limited_output, 1).unwrap();
        assert_eq!(limited.extracted_count, 1);

        fs::remove_dir_all(&limited_output).unwrap();
        fs::remove_dir_all(&output).unwrap();
        fs::remove_file(&input).unwrap();
    }
}
