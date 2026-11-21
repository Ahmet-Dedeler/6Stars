use rusty_opus::OpusDecoder;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

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

/// One entry of the ADAT chunk table: `tag:8 | size:28 | offset:28`.
#[derive(Clone, Copy)]
struct AwcChunk {
    tag: u8,
    off: usize,
    size: usize,
}

const TAG_DATA: u8 = 0x55;
const TAG_STREAM_FORMAT: u8 = 0x48;

/// Parses the ADAT stream table into per-stream chunk lists.
///
/// Layout: 16-byte header (`ADAT`, u16 version, u16 flags, u32 stream count, u32 data offset),
/// optional flag-dependent fields, then `stream_count` u32 stream infos (id:29 | chunk count:3)
/// and the u64 chunk table, which ends exactly at the data offset. The optional fields vary by
/// flags (e.g. 0x01 adds a u16 per-stream chunk index table), so the info table is located by
/// that invariant instead of a fixed offset.
fn parse_awc_streams(data: &[u8]) -> Option<Vec<Vec<AwcChunk>>> {
    let sc = ru32(data, 8) as usize;
    let doff = ru32(data, 12) as usize;
    if sc == 0 || sc > 4096 || doff > data.len() {
        return None;
    }
    let mut x = 16usize;
    while x + sc * 4 <= doff {
        let counts: Vec<usize> = (0..sc).map(|i| (ru32(data, x + i * 4) >> 29) as usize).collect();
        let total: usize = counts.iter().sum();
        if total > 0 && x + sc * 4 + total * 8 == doff {
            let mut off = x + sc * 4;
            let mut streams = Vec::with_capacity(sc);
            let mut ok = true;
            for &n in &counts {
                let mut chunks = Vec::with_capacity(n);
                for _ in 0..n {
                    let v = ru64(data, off);
                    off += 8;
                    let c = AwcChunk { tag: (v >> 56) as u8, size: ((v >> 28) & 0x0FFF_FFFF) as usize, off: (v & 0x0FFF_FFFF) as usize };
                    ok &= c.off + c.size <= data.len();
                    chunks.push(c);
                }
                streams.push(chunks);
            }
            if ok {
                return Some(streams);
            }
        }
        x += 2;
    }
    None
}

/// Splits RAGE Opus data into packets: 2 KB frames, each a run of u16-length-prefixed packets.
fn opus_packets<'a>(frames: impl Iterator<Item = &'a [u8]>, out: &mut Vec<&'a [u8]>) {
    for blk in frames {
        let mut p = 0usize;
        while p + 2 <= blk.len() {
            let plen = ru16(blk, p) as usize;
            if plen == 0 || p + 2 + plen > blk.len() {
                break;
            }
            out.push(&blk[p + 2..p + 2 + plen]);
            p += 2 + plen;
        }
    }
}

/// Decodes Opus packets at 48 kHz. Channel count comes from the first packet's TOC stereo bit.
fn decode_opus(packets: &[&[u8]]) -> Option<(usize, Vec<i16>)> {
    let ch: usize = if (packets.first()?[0] & 0x04) != 0 { 2 } else { 1 };
    let mut dec = OpusDecoder::new(48_000, ch).ok()?;
    let mut pcm_f32 = vec![0.0f32; 5760 * 2];
    let mut samples = Vec::with_capacity(packets.len() * 960 * ch);
    for pkt in packets {
        if let Ok(n_per_ch) = dec.decode(pkt, 5760, &mut pcm_f32[..5760 * ch]) {
            samples.extend(pcm_f32[..n_per_ch * ch].iter().map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16));
        }
    }
    (!samples.is_empty()).then_some((ch, samples))
}

/// Multichannel (flag 0x04) container: a stream-format chunk describes `block_count` blocks of
/// `block_size` bytes and N channels (16 bytes each: id, sample count, ...). Every block holds
/// N 24-byte channel headers (frame count at +4, samples to skip at +8), a u32 seek entry per
/// frame, padding to 0x800, then each channel's 2 KB Opus frames back to back. Blocks overlap:
/// the first `skip` decoded samples of each block repeat the previous block's tail.
fn decode_multichannel(data: &[u8], fmt: AwcChunk, body: AwcChunk) -> Vec<(usize, Vec<i16>)> {
    let f = &data[fmt.off..fmt.off + fmt.size];
    if f.len() < 12 {
        return Vec::new();
    }
    let (block_count, block_size, nch) = (ru32(f, 0) as usize, ru32(f, 4) as usize, ru32(f, 8) as usize);
    if nch == 0 || nch > 64 || block_size < 0x800 || f.len() < 12 + nch * 16 {
        return Vec::new();
    }
    let body = &data[body.off..body.off + body.size];
    // per channel: (samples to skip, packets) for every block
    let mut segments: Vec<Vec<(usize, Vec<&[u8]>)>> = vec![Vec::new(); nch];
    for b in 0..block_count {
        let start = b * block_size;
        if start + nch * 24 > body.len() {
            break;
        }
        let blk = &body[start..(start + block_size).min(body.len())];
        let frames: Vec<usize> = (0..nch).map(|c| ru32(blk, c * 24 + 4) as usize).collect();
        let hdr = nch * 24 + frames.iter().sum::<usize>() * 4;
        let mut p = (hdr + 0x7FF) & !0x7FF;
        for (c, &n) in frames.iter().enumerate() {
            let end = (p + n * 2048).min(blk.len());
            let mut pk = Vec::new();
            if p < end {
                opus_packets(blk[p..end].chunks(2048), &mut pk);
            }
            segments[c].push((ru32(blk, c * 24 + 8) as usize, pk));
            p += n * 2048;
        }
    }
    segments
        .iter()
        .enumerate()
        .filter_map(|(c, segs)| {
            let first = segs.iter().find_map(|(_, pk)| pk.first())?;
            let ch: usize = if (first[0] & 0x04) != 0 { 2 } else { 1 };
            let mut dec = OpusDecoder::new(48_000, ch).ok()?;
            let mut pcm_f32 = vec![0.0f32; 5760 * 2];
            let mut s: Vec<i16> = Vec::new();
            for (skip, pk) in segs {
                let mut left = skip * ch;
                for pkt in pk {
                    if let Ok(n) = dec.decode(pkt, 5760, &mut pcm_f32[..5760 * ch]) {
                        let got = &pcm_f32[..n * ch];
                        let drop = left.min(got.len());
                        left -= drop;
                        s.extend(got[drop..].iter().map(|&v| (v.clamp(-1.0, 1.0) * 32767.0) as i16));
                    }
                }
            }
            if s.is_empty() {
                return None;
            }
            // trim decoder tail to the channel's declared length
            let declared = ru32(f, 12 + c * 16 + 4) as usize * ch;
            if declared > 0 && s.len() > declared {
                s.truncate(declared);
            }
            Some((ch, s))
        })
        .collect()
}

// pure rust adat awc to wav decodr zero python or ffmpeg needed
pub fn decode_awc_bytes_to_wavs(data: &[u8], out_dir: &Path, stem: &str) -> Vec<(u32, PathBuf)> {
    if data.len() < 16 || &data[..4] != b"ADAT" {
        return Vec::new();
    }
    let flags = ru16(data, 6);
    let mut decoded: Vec<(usize, Vec<i16>)> = Vec::new();

    if let Some(streams) = parse_awc_streams(data) {
        for chunks in &streams {
            let body = chunks.iter().find(|c| c.tag == TAG_DATA && c.size > 0);
            let fmt = chunks.iter().find(|c| c.tag == TAG_STREAM_FORMAT);
            match (body, fmt) {
                (Some(&b), Some(&f)) if flags & 0x04 != 0 => decoded.extend(decode_multichannel(data, f, b)),
                (Some(b), _) => {
                    let mut pk = Vec::new();
                    opus_packets(data[b.off..b.off + b.size].chunks(2048), &mut pk);
                    decoded.extend(decode_opus(&pk));
                }
                _ => {}
            }
        }
    } else {
        // legacy loose scan: u64 entries right after the u32 stream infos, keep every data chunk
        let sc = ru32(data, 8) as usize;
        let hdr_sz = (ru32(data, 12) as usize).min(data.len());
        if sc == 0 || sc > 4096 {
            return Vec::new();
        }
        let mut off = 16 + sc * 4;
        while off + 8 <= hdr_sz {
            let val = ru64(data, off);
            let tag = (val >> 56) as u8;
            let sz = ((val >> 28) & 0x0FFF_FFFF) as usize;
            let coff = (val & 0x0FFF_FFFF) as usize;
            if tag == 0 && coff == 0 {
                break;
            }
            if tag == TAG_DATA && sz > 0 && coff >= 0x20 && coff + sz <= data.len() {
                let mut pk = Vec::new();
                opus_packets(data[coff..coff + sz].chunks(2048), &mut pk);
                decoded.extend(decode_opus(&pk));
            }
            off += 8;
        }
    }

    let _ = fs::create_dir_all(out_dir);
    let mut wavs = Vec::new();
    for (idx, (ch, samples)) in decoded.iter().enumerate() {
        let wav_p = out_dir.join(format!("{stem}_s{idx:02}.wav"));
        if write_pcm_wav(&wav_p, 48_000, *ch as u16, samples).is_ok() {
            wavs.push((idx as u32 + 1, wav_p));
        }
    }
    wavs
}

fn write_pcm_wav(path: &Path, sample_rate: u32, channels: u16, samples: &[i16]) -> std::io::Result<()> {
    let data_bytes = (samples.len() * 2) as u32;
    let byte_rate = sample_rate * (channels as u32) * 2;
    let block_align = channels * 2;
    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_bytes).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?;
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&byte_rate.to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&16u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_bytes.to_le_bytes())?;
    for &s in samples {
        w.write_all(&s.to_le_bytes())?;
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_opus::{Application, OpusEncoder};

    #[test]
    fn roundtrip_adat_opus_to_wav() {
        let mut enc = OpusEncoder::new(48_000, 1, Application::Audio).unwrap();
        let pcm = vec![0.25f32; 960];
        let mut pkt = [0u8; 512];
        let plen = enc.encode(&pcm, 960, &mut pkt).unwrap();

        // build fake adat blob matching adat v1 layout
        let mut blk = vec![0u8; 2048];
        blk[0..2].copy_from_slice(&(plen as u16).to_le_bytes());
        blk[2..2 + plen].copy_from_slice(&pkt[..plen]);

        let coff = 0x40u64;
        let csz = blk.len() as u64;
        let entry_val = (0x55u64 << 56) | (csz << 28) | coff;
        let mut adat = vec![0u8; 0x40];
        adat[0..4].copy_from_slice(b"ADAT");
        adat[8..12].copy_from_slice(&1u32.to_le_bytes());
        adat[12..16].copy_from_slice(&0x40u32.to_le_bytes());
        adat[20..28].copy_from_slice(&entry_val.to_le_bytes());
        adat.extend_from_slice(&blk);

        let tmp = std::env::temp_dir().join("percy6stars_awc_test");
        let wavs = decode_awc_bytes_to_wavs(&adat, &tmp, "test");
        assert_eq!(wavs.len(), 1);
        let bytes = fs::read(&wavs[0].1).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert!(bytes.len() > 44 + 960);
        let _ = fs::remove_dir_all(&tmp);
    }
}

