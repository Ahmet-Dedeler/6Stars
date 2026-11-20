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

// pure rust adat awc to wav decodr zero python or ffmpeg needed
pub fn decode_awc_bytes_to_wavs(data: &[u8], out_dir: &Path, stem: &str) -> Vec<(u32, PathBuf)> {
    if data.len() < 16 || &data[..4] != b"ADAT" {
        return Vec::new();
    }
    let sc = ru32(data, 8) as usize;
    let hdr_sz = (ru32(data, 12) as usize).min(data.len());
    if sc == 0 || sc > 4096 {
        return Vec::new();
    }

    let mut data_chunks = Vec::new();
    let mut off = 16 + sc * 4;
    // unpack 64bit tag size offset entries lookign for 0x55 data blocks
    while off + 8 <= hdr_sz {
        let val = ru64(data, off);
        let tag = (val >> 56) as u8;
        let sz = ((val >> 28) & 0x0FFF_FFFF) as usize;
        let coff = (val & 0x0FFF_FFFF) as usize;
        if tag == 0 && coff == 0 {
            break;
        }
        if tag == 0x55 && sz > 0 && coff >= 0x20 && coff + sz <= data.len() {
            data_chunks.push((coff, sz));
        }
        off += 8;
    }

    let _ = fs::create_dir_all(out_dir);
    let mut wavs = Vec::new();
    let mut pcm_f32 = vec![0.0f32; 5760 * 2];

    for (idx, &(coff, csz)) in data_chunks.iter().enumerate() {
        let blob = &data[coff..coff + csz];
        let mut packets: Vec<&[u8]> = Vec::new();
        // rage opus splits payloads into 2kb blocks of u16 prefixed frams
        for blk in blob.chunks(2048) {
            let mut p = 0usize;
            while p + 2 <= blk.len() {
                let plen = ru16(blk, p) as usize;
                if plen == 0 || p + 2 + plen > blk.len() {
                    break;
                }
                packets.push(&blk[p + 2..p + 2 + plen]);
                p += 2 + plen;
            }
        }
        if packets.is_empty() {
            continue;
        }

        let ch: usize = if (packets[0][0] & 0x04) != 0 { 2 } else { 1 };
        let Ok(mut dec) = OpusDecoder::new(48_000, ch) else { continue };
        let mut samples_i16: Vec<i16> = Vec::with_capacity(packets.len() * 960 * ch);

        for pkt in &packets {
            if let Ok(n_per_ch) = dec.decode(pkt, 5760, &mut pcm_f32[..5760 * ch]) {
                for &s in &pcm_f32[..n_per_ch * ch] {
                    samples_i16.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
                }
            }
        }
        if samples_i16.is_empty() {
            continue;
        }

        let wav_p = out_dir.join(format!("{stem}_s{idx:02}.wav"));
        if write_pcm_wav(&wav_p, 48_000, ch as u16, &samples_i16).is_ok() {
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

        let tmp = std::env::temp_dir().join("percystar_awc_test");
        let wavs = decode_awc_bytes_to_wavs(&adat, &tmp, "test");
        assert_eq!(wavs.len(), 1);
        let bytes = fs::read(&wavs[0].1).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert!(bytes.len() > 44 + 960);
        let _ = fs::remove_dir_all(&tmp);
    }
}
