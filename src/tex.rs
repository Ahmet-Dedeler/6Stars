//! Native Xbox Series X (Scarlett / RDNA2) texture, TTPF (RDS5/ETPF), RBF0, Scaleform GFX,
//! and CNavMesh decoders for RPF9-era archives. Zero external runtime dependencies.

use crate::model3d::{is_zlib_head, Mesh3D, Texture2D};
use eframe::egui::Color32;
use flate2::{Decompress, FlushDecompress, Status};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

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
#[inline]
fn rf32(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
#[inline]
fn vptr(q: u64) -> Option<usize> {
    (0x5000_0000..0x5800_0000).contains(&q).then(|| (q - 0x5000_0000) as usize)
}
#[inline]
fn pptr(q: u64) -> Option<usize> {
    (0x6000_0000..0x7000_0000).contains(&q).then(|| (q - 0x6000_0000) as usize)
}

#[derive(Clone)]
pub struct DecodedTexture {
    pub name: String,
    pub w: usize,
    pub h: usize,
    pub mips: u8,
    pub dxgi: u8,
    pub fmt_name: &'static str,
    pub pixels: Vec<Color32>,
    pub raw_dds_payload: Vec<u8>,
    /// Full mip-0 size when only a smaller mip could be decoded (TTPF lower-mip fallback).
    /// `w`/`h`/`pixels` then describe the reduced mip.
    pub full_size: Option<(usize, usize)>,
}

pub fn dxgi_info(dxgi: u8) -> Option<(&'static str, bool, usize)> {
    Some(match dxgi {
        10 => ("R16G16B16A16_FLOAT", false, 8),
        11 => ("R16G16B16A16_UNORM", false, 8),
        24 => ("R10G10B10A2_UNORM", false, 4),
        28 => ("R8G8B8A8_UNORM", false, 4),
        29 => ("R8G8B8A8_UNORM_SRGB", false, 4),
        34 => ("R16G16_FLOAT", false, 4),
        41 => ("R32_FLOAT", false, 4),
        49 => ("R8G8_UNORM", false, 2),
        54 => ("R16_FLOAT", false, 2),
        56 => ("R16_UNORM", false, 2),
        61 => ("R8_UNORM", false, 1),
        65 => ("A8_UNORM", false, 1),
        71 => ("BC1_UNORM", true, 8),
        72 => ("BC1_UNORM_SRGB", true, 8),
        74 => ("BC2_UNORM", true, 16),
        75 => ("BC2_UNORM_SRGB", true, 16),
        77 => ("BC3_UNORM", true, 16),
        78 => ("BC3_UNORM_SRGB", true, 16),
        80 => ("BC4_UNORM", true, 8),
        81 => ("BC4_SNORM", true, 8),
        83 => ("BC5_UNORM", true, 16),
        84 => ("BC5_SNORM", true, 16),
        87 => ("B8G8R8A8_UNORM", false, 4),
        91 => ("B8G8R8A8_UNORM_SRGB", false, 4),
        95 => ("BC6H_UF16", true, 16),
        96 => ("BC6H_SF16", true, 16),
        98 => ("BC7_UNORM", true, 16),
        99 => ("BC7_UNORM_SRGB", true, 16),
        _ => return None,
    })
}

pub fn padded_4kb_size(w: usize, h: usize, is_bc: bool, bpe: usize) -> usize {
    let ew = if is_bc { (w + 3) / 4 } else { w }.max(1);
    let eh = if is_bc { (h + 3) / 4 } else { h }.max(1);
    let (tw, th) = match bpe {
        16 => (16usize, 16usize),
        8 => (32, 16),
        4 => (32, 32),
        2 => (64, 32),
        _ => (64, 64),
    };
    let pw = ((ew + tw - 1) / tw) * tw;
    let ph = ((eh + th - 1) / th) * th;
    pw * ph * bpe
}

pub fn padded_64kb_size(w: usize, h: usize, is_bc: bool, bpe: usize) -> usize {
    let ew = if is_bc { (w + 3) / 4 } else { w }.max(1);
    let eh = if is_bc { (h + 3) / 4 } else { h }.max(1);
    let (tw, th) = match bpe {
        16 => (64usize, 64usize),
        8 => (128, 64),
        4 => (128, 128),
        2 => (256, 128),
        _ => (256, 256),
    };
    let pw = ((ew + tw - 1) / tw) * tw;
    let ph = ((eh + th - 1) / th) * th;
    pw * ph * bpe
}

/// Verified AMD RDNA2 / Xbox Scarlett SW_4KB_D deswizzle for all 5 bit depths (8..128bpp).
pub fn deswizzle_4kb_d(raw: &[u8], w: usize, h: usize, is_bc: bool, bpe: usize) -> Vec<u8> {
    let ew = if is_bc { (w + 3) / 4 } else { w }.max(1);
    let eh = if is_bc { (h + 3) / 4 } else { h }.max(1);
    let (tw, th) = match bpe {
        16 => (16usize, 16usize),
        8 => (32, 16),
        4 => (32, 32),
        2 => (64, 32),
        _ => (64, 64),
    };
    let pw = ((ew + tw - 1) / tw) * tw;
    let ph = ((eh + th - 1) / th) * th;
    let needed = pw * ph * bpe;
    let lin_len = ew * eh * bpe;
    if raw.len() < needed {
        let mut out = vec![0u8; lin_len];
        let n = raw.len().min(lin_len);
        out[..n].copy_from_slice(&raw[..n]);
        return out;
    }

    let mut out = vec![0u8; lin_len];
    let tiles_x = pw / tw;
    let elems_per_tile = tw * th;

    for y in 0..eh {
        let ty = y / th;
        let ly = y % th;
        for x in 0..ew {
            let tx = x / tw;
            let lx = x % tw;
            let t_idx = ty * tiles_x + tx;
            let m = match bpe {
                // 128bpp (16x16): y0,y1,x0,x1, y2,x2,y3,x3
                16 => (ly & 3) | ((lx & 3) << 2) | (((ly >> 2) & 1) << 4) | (((lx >> 2) & 1) << 5) | (((ly >> 3) & 1) << 6) | (((lx >> 3) & 1) << 7),
                // 64bpp (32x16): x0,y0,y1,x1,x2, y2,x3,y3,x4
                8 => (lx & 1) | ((ly & 3) << 1) | (((lx >> 1) & 3) << 3) | (((ly >> 2) & 1) << 5) | (((lx >> 3) & 1) << 6) | (((ly >> 3) & 1) << 7) | (((lx >> 4) & 1) << 8),
                // 32bpp (32x32): x0,x1,y0,y1,x2,y2, y3,x3,y4,x4
                4 => (lx & 3) | ((ly & 3) << 2) | (((lx >> 2) & 1) << 4) | (((ly >> 2) & 1) << 5) | (((ly >> 3) & 1) << 6) | (((lx >> 3) & 1) << 7) | (((ly >> 4) & 1) << 8) | (((lx >> 4) & 1) << 9),
                // 16bpp (64x32): x0,x1,x2,y0,y1,y2, x3,y3,x4,y4,x5
                2 => (lx & 7) | ((ly & 7) << 3) | (((lx >> 3) & 1) << 6) | (((ly >> 3) & 1) << 7) | (((lx >> 4) & 1) << 8) | (((ly >> 4) & 1) << 9) | (((lx >> 5) & 1) << 10),
                // 8bpp (64x64): y0,y1,y2,x1,x0,x2, y3,x3,x4,y4,x5,y5
                _ => (ly & 7) | (((lx >> 1) & 1) << 3) | ((lx & 1) << 4) | (((lx >> 2) & 1) << 5) | (((ly >> 3) & 1) << 6) | (((lx >> 3) & 1) << 7) | (((lx >> 4) & 1) << 8) | (((ly >> 4) & 1) << 9) | (((lx >> 5) & 1) << 10) | (((ly >> 5) & 1) << 11),
            };
            let src_off = (t_idx * elems_per_tile + m) * bpe;
            let dst_off = (y * ew + x) * bpe;
            out[dst_off..dst_off + bpe].copy_from_slice(&raw[src_off..src_off + bpe]);
        }
    }
    out
}

/// Verified Xbox Scarlett 64KB TTPF tile deswizzle for ALL 5 bit depths (8..128bpp, BCn & uncompressed).
pub fn deswizzle_64kb_ttpf(raw: &[u8], w: usize, h: usize, is_bc: bool, bpe: usize) -> Vec<u8> {
    let bw = if is_bc { (w + 3) / 4 } else { w }.max(1);
    let bh = if is_bc { (h + 3) / 4 } else { h }.max(1);
    let (tw, th, shift) = match bpe {
        16 => (64usize, 64usize, 12usize),
        8 => (128, 64, 13),
        4 => (128, 128, 14),
        2 => (256, 128, 15),
        _ => (256, 256, 16),
    };
    let pw = ((bw + tw - 1) / tw) * tw;
    let ph = ((bh + th - 1) / th) * th;
    let needed = pw * ph * bpe;
    let mut padded = Vec::new();
    let src: &[u8] = if raw.len() >= needed {
        &raw[..needed]
    } else {
        padded.resize(needed, 0);
        let n = raw.len().min(needed);
        padded[..n].copy_from_slice(&raw[..n]);
        &padded
    };
    let mut out = vec![0u8; bw * bh * bpe];
    let tiles_x = pw / tw;
    for y in 0..bh {
        let ty = y / th;
        let ly = y % th;
        for x in 0..bw {
            let tx = x / tw;
            let lx = x % tw;
            let t_idx = ty * tiles_x + tx;
            let m = match bpe {
                16 => (lx & 1)
                    | ((ly & 1) << 1)
                    | (((lx >> 1) & 1) << 2)
                    | (((ly >> 1) & 1) << 3)
                    | (((ly >> 2) & 1) << 4)
                    | (((lx >> 2) & 1) << 5)
                    | (((ly >> 3) & 1) << 6)
                    | (((lx >> 3) & 1) << 7)
                    | (((ly >> 4) & 1) << 8)
                    | (((lx >> 4) & 1) << 9)
                    | (((ly >> 5) & 1) << 10)
                    | (((lx >> 5) & 1) << 11),
                8 => (lx & 1)
                    | ((ly & 1) << 1)
                    | (((lx >> 1) & 3) << 2)
                    | (((ly >> 1) & 1) << 4)
                    | (((ly >> 2) & 1) << 5)
                    | (((lx >> 3) & 1) << 6)
                    | (((ly >> 3) & 1) << 7)
                    | (((lx >> 4) & 1) << 8)
                    | (((ly >> 4) & 1) << 9)
                    | (((lx >> 5) & 1) << 10)
                    | (((ly >> 5) & 1) << 11)
                    | (((lx >> 6) & 1) << 12),
                4 => (lx & 3)
                    | ((ly & 3) << 2)
                    | (((lx >> 2) & 1) << 4)
                    | (((ly >> 2) & 1) << 5)
                    | (((ly >> 3) & 1) << 6)
                    | (((lx >> 3) & 1) << 7)
                    | (((ly >> 4) & 1) << 8)
                    | (((lx >> 4) & 1) << 9)
                    | (((ly >> 5) & 1) << 10)
                    | (((lx >> 5) & 1) << 11)
                    | (((ly >> 6) & 1) << 12)
                    | (((lx >> 6) & 1) << 13),
                2 => (lx & 7)
                    | ((ly & 7) << 3)
                    | (((lx >> 3) & 1) << 6)
                    | (((ly >> 3) & 1) << 7)
                    | (((lx >> 4) & 1) << 8)
                    | (((ly >> 4) & 1) << 9)
                    | (((lx >> 5) & 1) << 10)
                    | (((ly >> 5) & 1) << 11)
                    | (((lx >> 6) & 1) << 12)
                    | (((ly >> 6) & 1) << 13)
                    | (((lx >> 7) & 1) << 14),
                _ => (ly & 7)
                    | (((lx >> 1) & 1) << 3)
                    | ((lx & 1) << 4)
                    | (((lx >> 2) & 1) << 5)
                    | (((ly >> 3) & 1) << 6)
                    | (((lx >> 3) & 1) << 7)
                    | (((lx >> 4) & 1) << 8)
                    | (((ly >> 4) & 1) << 9)
                    | (((lx >> 5) & 1) << 10)
                    | (((ly >> 5) & 1) << 11)
                    | (((lx >> 6) & 1) << 12)
                    | (((ly >> 6) & 1) << 13)
                    | (((lx >> 7) & 1) << 14)
                    | (((ly >> 7) & 1) << 15),
            };
            let src_off = ((t_idx << shift) | m) * bpe;
            let dst_off = (y * bw + x) * bpe;
            out[dst_off..dst_off + bpe].copy_from_slice(&src[src_off..src_off + bpe]);
        }
    }
    out
}

#[inline]
fn f16_to_f32(bits: u16) -> f32 {
    let s = ((bits >> 15) & 1) as u32;
    let e = ((bits >> 10) & 0x1F) as u32;
    let m = (bits & 0x3FF) as u32;
    if e == 0 {
        if m == 0 { return if s != 0 { -0.0 } else { 0.0 }; }
        let v = (m as f32) * (2.0_f32).powi(-24);
        return if s != 0 { -v } else { v };
    }
    if e == 31 {
        return 0.0;
    }
    f32::from_bits((s << 31) | ((e + 112) << 23) | (m << 13))
}

#[inline]
fn tonemap_u8(v: f32) -> u8 {
    let x = v.max(0.0);
    let m = x / (1.0 + x);
    (m.powf(1.0 / 2.2) * 255.0).clamp(0.0, 255.0) as u8
}

pub fn decode_linear_to_rgba(lin: &[u8], w: usize, h: usize, dxgi: u8) -> Option<Vec<Color32>> {
    if matches!(dxgi, 71 | 72 | 77 | 78 | 80 | 81 | 83 | 84 | 95 | 96 | 98 | 99) {
        let mut out_u32 = vec![0u32; w * h];
        let res = match dxgi {
            71 | 72 => texture2ddecoder::decode_bc1a(lin, w, h, &mut out_u32),
            77 | 78 => texture2ddecoder::decode_bc3(lin, w, h, &mut out_u32),
            80 | 81 => texture2ddecoder::decode_bc4(lin, w, h, &mut out_u32),
            83 | 84 => texture2ddecoder::decode_bc5(lin, w, h, &mut out_u32),
            95 | 96 => {
                // Work around texture2ddecoder 0.1.2 bug: BC6H mode 15 (0x0F, endpoint_bits=16)
                // overflows `1i16 << 16` in debug builds. Convert mode-15 blocks to mode-3 (0x03)
                // untransformed blocks safely so BC6H never panics.
                let mut safe = lin.to_vec();
                for blk in safe.chunks_exact_mut(16) {
                    if (blk[0] & 0x1F) == 0x0F {
                        blk[0] = (blk[0] & !0x1F) | 0x03;
                    }
                }
                if dxgi == 95 {
                    texture2ddecoder::decode_bc6_unsigned(&safe, w, h, &mut out_u32)
                } else {
                    texture2ddecoder::decode_bc6_signed(&safe, w, h, &mut out_u32)
                }
            }
            98 | 99 => texture2ddecoder::decode_bc7(lin, w, h, &mut out_u32),
            _ => unreachable!(),
        };
        res.ok()?;
        let mut pixels = Vec::with_capacity(w * h);
        if matches!(dxgi, 80 | 81 | 83 | 84) {
            for px in out_u32 {
                let r = ((px >> 16) & 0xFF) as u8;
                let g = ((px >> 8) & 0xFF) as u8;
                pixels.push(if dxgi <= 81 {
                    Color32::from_gray(r)
                } else {
                    let (nx, ny) = (r as f32 / 127.5 - 1.0, g as f32 / 127.5 - 1.0);
                    let nz = (1.0 - nx * nx - ny * ny).max(0.0).sqrt();
                    Color32::from_rgb(r, g, ((nz * 0.5 + 0.5) * 255.0) as u8)
                });
            }
            return Some(pixels);
        }
        for px in out_u32 {
            // texture2ddecoder packs u32 as 0xAARRGGBB (BGRA little-endian)
            let b = (px & 0xFF) as u8;
            let g = ((px >> 8) & 0xFF) as u8;
            let r = ((px >> 16) & 0xFF) as u8;
            let a = ((px >> 24) & 0xFF) as u8;
            pixels.push(Color32::from_rgba_unmultiplied(r, g, b, a));
        }
        return Some(pixels);
    }
    if matches!(dxgi, 74 | 75) {
        // BC2 = 8-byte 4-bit explicit alpha + 8-byte BC1 color per 4x4 block
        let bw = (w + 3) / 4;
        let bh = (h + 3) / 4;
        if lin.len() < bw * bh * 16 { return None; }
        let mut bc1_only = Vec::with_capacity(bw * bh * 8);
        for b in 0..bw * bh {
            bc1_only.extend_from_slice(&lin[b * 16 + 8..b * 16 + 16]);
        }
        let mut out_u32 = vec![0u32; w * h];
        texture2ddecoder::decode_bc1(&bc1_only, w, h, &mut out_u32).ok()?;
        let mut pixels = vec![Color32::TRANSPARENT; w * h];
        for by in 0..bh {
            for bx in 0..bw {
                let abits = ru64(lin, (by * bw + bx) * 16);
                for py in 0..4 {
                    let y = by * 4 + py;
                    if y >= h { continue; }
                    for px in 0..4 {
                        let x = bx * 4 + px;
                        if x >= w { continue; }
                        let c = out_u32[y * w + x];
                        let a4 = ((abits >> (4 * (py * 4 + px))) & 0xF) as u8;
                        pixels[y * w + x] = Color32::from_rgba_unmultiplied(
                            ((c >> 16) & 0xFF) as u8,
                            ((c >> 8) & 0xFF) as u8,
                            (c & 0xFF) as u8,
                            a4 | (a4 << 4),
                        );
                    }
                }
            }
        }
        return Some(pixels);
    }
    let mut pixels = Vec::with_capacity(w * h);
    match dxgi {
        10 => {
            if lin.len() < w * h * 8 { return None; }
            for px in lin[..w * h * 8].chunks_exact(8) {
                let r = tonemap_u8(f16_to_f32(ru16(px, 0)));
                let g = tonemap_u8(f16_to_f32(ru16(px, 2)));
                let b = tonemap_u8(f16_to_f32(ru16(px, 4)));
                let a = (f16_to_f32(ru16(px, 6)).clamp(0.0, 1.0) * 255.0) as u8;
                pixels.push(Color32::from_rgba_unmultiplied(r, g, b, a.max(255)));
            }
        }
        11 => {
            if lin.len() < w * h * 8 { return None; }
            for px in lin[..w * h * 8].chunks_exact(8) {
                pixels.push(Color32::from_rgba_unmultiplied(px[1], px[3], px[5], px[7]));
            }
        }
        24 => {
            if lin.len() < w * h * 4 { return None; }
            for px in lin[..w * h * 4].chunks_exact(4) {
                let v = ru32(px, 0);
                let r = ((v & 0x3FF) >> 2) as u8;
                let g = (((v >> 10) & 0x3FF) >> 2) as u8;
                let b = (((v >> 20) & 0x3FF) >> 2) as u8;
                let a = (((v >> 30) & 3) * 85) as u8;
                pixels.push(Color32::from_rgba_unmultiplied(r, g, b, a));
            }
        }
        28 | 29 => {
            if lin.len() < w * h * 4 { return None; }
            for px in lin[..w * h * 4].chunks_exact(4) {
                pixels.push(Color32::from_rgba_unmultiplied(px[0], px[1], px[2], px[3]));
            }
        }
        34 => {
            if lin.len() < w * h * 4 { return None; }
            for px in lin[..w * h * 4].chunks_exact(4) {
                let r = tonemap_u8(f16_to_f32(ru16(px, 0)));
                let g = tonemap_u8(f16_to_f32(ru16(px, 2)));
                pixels.push(Color32::from_rgb(r, g, 0));
            }
        }
        41 => {
            if lin.len() < w * h * 4 { return None; }
            for px in lin[..w * h * 4].chunks_exact(4) {
                let g = tonemap_u8(rf32(px, 0));
                pixels.push(Color32::from_gray(g));
            }
        }
        87 | 91 => {
            if lin.len() < w * h * 4 { return None; }
            for px in lin[..w * h * 4].chunks_exact(4) {
                pixels.push(Color32::from_rgba_unmultiplied(px[2], px[1], px[0], px[3]));
            }
        }
        49 => {
            if lin.len() < w * h * 2 { return None; }
            for px in lin[..w * h * 2].chunks_exact(2) {
                pixels.push(Color32::from_rgb(px[0], px[1], 0));
            }
        }
        54 => {
            if lin.len() < w * h * 2 { return None; }
            let vals: Vec<f32> = lin[..w * h * 2].chunks_exact(2).map(|px| f16_to_f32(ru16(px, 0))).collect();
            let mut mn = f32::INFINITY;
            let mut mx = f32::NEG_INFINITY;
            for &v in &vals {
                if v.is_finite() {
                    if v < mn { mn = v; }
                    if v > mx { mx = v; }
                }
            }
            let span = (mx - mn).max(1e-5);
            for v in vals {
                let g = (((v - mn) / span).clamp(0.0, 1.0) * 255.0) as u8;
                pixels.push(Color32::from_gray(g));
            }
        }
        56 => {
            if lin.len() < w * h * 2 { return None; }
            for px in lin[..w * h * 2].chunks_exact(2) {
                pixels.push(Color32::from_gray(px[1]));
            }
        }
        61 | 65 => {
            if lin.len() < w * h { return None; }
            for &g in &lin[..w * h] {
                pixels.push(Color32::from_gray(g));
            }
        }
        _ => return None,
    }
    Some(pixels)
}

pub fn build_dds_dx10(w: usize, h: usize, dxgi: u8, lin: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(148 + lin.len());
    out.extend_from_slice(b"DDS ");
    for v in [124u32, 0x1007, h as u32, w as u32, lin.len() as u32, 1, 1] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&[0u8; 44]);
    // pixelformat
    out.extend_from_slice(&32u32.to_le_bytes());
    out.extend_from_slice(&0x4u32.to_le_bytes());
    out.extend_from_slice(b"DX10");
    out.extend_from_slice(&[0u8; 20]);
    // caps
    out.extend_from_slice(&0x1000u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);
    // dx10 header
    for v in [dxgi as u32, 3, 0, 1, 0] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(lin);
    out
}

pub fn export_texture_png(tex: &DecodedTexture, path: &Path) -> Result<(), String> {
    let mut rgba = Vec::with_capacity(tex.w * tex.h * 4);
    for c in &tex.pixels {
        rgba.extend_from_slice(&c.to_srgba_unmultiplied());
    }
    image::save_buffer(path, &rgba, tex.w as u32, tex.h as u32, image::ColorType::Rgba8)
        .map_err(|e| format!("PNG save error: {e}"))
}

/// Extract ALL textures from a pgDictionary<grcTexture> inside virtual `v` + physical `p`.
pub fn extract_txd_textures(v: &[u8], p: &[u8], dict_off: usize) -> Vec<DecodedTexture> {
    extract_txd_textures_by_ptr(v, p, dict_off).into_iter().map(|(_, tex)| tex).collect()
}

/// Keep each virtual texture pointer so drawable shaders can bind the correct entry.
pub fn extract_txd_textures_by_ptr(v: &[u8], p: &[u8], dict_off: usize) -> Vec<(usize, DecodedTexture)> {
    if dict_off + 0x3a > v.len() { return Vec::new(); }
    let Some(tarr) = vptr(ru64(v, dict_off + 0x30)) else { return Vec::new() };
    let tcnt = ru16(v, dict_off + 0x38) as usize;
    if tcnt == 0 || tcnt > 2048 || tarr + tcnt * 8 > v.len() { return Vec::new(); }

    let mut raw_list = Vec::new();
    for ti in 0..tcnt {
        let Some(tp) = vptr(ru64(v, tarr + ti * 8)) else { continue };
        if tp + 0x48 > v.len() { continue; }
        let Some(poff) = pptr(ru64(v, tp + 0x18)) else { continue };
        if poff >= p.len() { continue; }
        let w = ru16(v, tp + 0x38) as usize;
        let h = ru16(v, tp + 0x3a) as usize;
        let dxgi = v[tp + 0x3f];
        let swiz = v[tp + 0x40];
        let mips = v[tp + 0x42];
        if !(4..=8192).contains(&w) || !(4..=8192).contains(&h) || dxgi_info(dxgi).is_none() {
            continue;
        }
        let mut name = format!("tex_{ti:03}");
        if let Some(mut no) = vptr(ru64(v, tp + 0x30)) {
            if no + 8 < v.len() && v[no..no + 8] == [0u8; 8] {
                no += 8;
            }
            if let Some(rel) = v[no..].iter().position(|&b| b == 0) {
                if rel > 0 && rel < 128 {
                    let s = String::from_utf8_lossy(&v[no..no + rel]).into_owned();
                    if s.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                        name = s;
                    }
                }
            }
        }
        raw_list.push((poff, tp, name, w, h, dxgi, swiz, mips));
    }

    let mut sorted_poffs: Vec<usize> = raw_list.iter().map(|t| t.0).collect();
    sorted_poffs.sort_unstable();
    sorted_poffs.dedup();
    let mut next_map: HashMap<usize, usize> = HashMap::new();
    for w2 in sorted_poffs.windows(2) {
        next_map.insert(w2[0], w2[1]);
    }

    let mut out = Vec::new();
    for (poff, tp, name, w, h, dxgi, swiz, mips) in raw_list {
        let Some((fmt_name, is_bc, bpe)) = dxgi_info(dxgi) else { continue };
        let alloc_end = next_map.get(&poff).copied().unwrap_or(p.len());
        let alloc_sz = alloc_end.saturating_sub(poff);
        let mip0_sz = padded_4kb_size(w, h, is_bc, bpe);
        let mip0_off = if mips > 1 && alloc_sz >= mip0_sz {
            poff + (alloc_sz - mip0_sz)
        } else {
            poff
        };
        if mip0_off >= p.len() { continue; }
        let slice_end = (mip0_off + mip0_sz).min(p.len());
        let lin = if swiz == 0 {
            let ew = if is_bc { (w + 3) / 4 } else { w }.max(1);
            let eh = if is_bc { (h + 3) / 4 } else { h }.max(1);
            let need = ew * eh * bpe;
            let mut b = vec![0u8; need];
            let n = (p.len() - mip0_off).min(need);
            b[..n].copy_from_slice(&p[mip0_off..mip0_off + n]);
            b
        } else {
            deswizzle_4kb_d(&p[mip0_off..slice_end], w, h, is_bc, bpe)
        };
        if let Some(pixels) = decode_linear_to_rgba(&lin, w, h, dxgi) {
            out.push((tp, DecodedTexture {
                name,
                w,
                h,
                mips,
                dxgi,
                fmt_name,
                pixels,
                raw_dds_payload: lin,
                full_size: None,
            }));
        }
    }
    out
}

/// Convert DecodedTexture into model3d::Texture2D.
pub fn to_model_tex(t: DecodedTexture) -> Texture2D {
    let mut pixels = t.pixels;
    if t.dxgi == 65 {
        // The texture browser shows A8 as grayscale; models use it as opacity.
        for c in &mut pixels {
            *c = Color32::from_white_alpha(c.r());
        }
    }
    Texture2D { w: t.w, h: t.h, pixels }
}

/// Read RAGE9 resource virtual + physical buffers from RPF at `file_off` and decode all TXD textures.
pub fn decode_txd_from_rpf(rpf_path: &Path, file_off: u64) -> Result<Vec<DecodedTexture>, String> {
    let mut f = File::open(rpf_path).map_err(|e| format!("Open error: {e}"))?;
    let (v, p) = read_rage9_vp(&mut f, file_off)?;
    let texs = extract_txd_textures(&v, &p, 0);
    if texs.is_empty() {
        return Err("No decodable textures found in dictionary".into());
    }
    Ok(texs)
}

fn decompress_stream_at(f: &mut File, file_off: u64, max_out: usize) -> Result<(Vec<u8>, u64), String> {
    f.seek(SeekFrom::Start(file_off)).map_err(|e| e.to_string())?;
    let mut dec = Decompress::new(true);
    let mut in_buf = [0u8; 65536];
    let mut out_chunk = [0u8; 65536];
    let mut out = Vec::with_capacity(65536);
    let mut consumed = 0u64;
    loop {
        let n = f.read(&mut in_buf).map_err(|e| e.to_string())?;
        if n == 0 { break; }
        let mut pos = 0usize;
        while pos < n {
            let bi = dec.total_in();
            let bo = dec.total_out();
            let st = dec.decompress(&in_buf[pos..n], &mut out_chunk, FlushDecompress::None).map_err(|e| e.to_string())?;
            let cin = (dec.total_in() - bi) as usize;
            let cout = (dec.total_out() - bo) as usize;
            pos += cin;
            consumed += cin as u64;
            if cout > 0 {
                out.extend_from_slice(&out_chunk[..cout]);
                if out.len() > max_out {
                    return Ok((out, file_off + consumed));
                }
            }
            if st == Status::StreamEnd {
                return Ok((out, file_off + consumed));
            }
            if cin == 0 && cout == 0 { break; }
        }
    }
    Ok((out, file_off + consumed))
}

fn find_next_zlib_near(f: &mut File, start_off: u64) -> Result<Option<u64>, String> {
    f.seek(SeekFrom::Start(start_off)).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 4096];
    let n = f.read(&mut buf).map_err(|e| e.to_string())?;
    for i in 0..n.saturating_sub(3) {
        if is_zlib_head(&buf[i..n]) {
            return Ok(Some(start_off + i as u64));
        }
    }
    Ok(None)
}

pub fn read_rage9_vp(f: &mut File, file_off: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
    let (mut v, mut next_off) = decompress_stream_at(f, file_off, 16 * 1024 * 1024)?;
    if v.len() < 0x40 { return Err("Virtual chunk too small".into()); }
    let Some(bmap_off) = vptr(ru64(&v, 8)) else { return Err("Missing BlockMap".into()) };
    let mut v_read = 1usize;
    while v.len() <= bmap_off + 16 && v_read < 32 {
        let Some(zoff) = find_next_zlib_near(f, next_off)? else { break };
        let (c, nxt) = decompress_stream_at(f, zoff, 16 * 1024 * 1024)?;
        v.extend_from_slice(&c);
        next_off = nxt;
        v_read += 1;
    }
    let (v_cnt, p_cnt) = if bmap_off + 10 <= v.len() {
        ((v[bmap_off + 8] as usize).clamp(1, 32), (v[bmap_off + 9] as usize).clamp(0, 64))
    } else {
        (v_read, 1)
    };
    for _ in v_read..v_cnt {
        let Some(zoff) = find_next_zlib_near(f, next_off)? else { break };
        let (c, nxt) = decompress_stream_at(f, zoff, 16 * 1024 * 1024)?;
        v.extend_from_slice(&c);
        next_off = nxt;
    }
    let mut p = Vec::new();
    for _ in 0..p_cnt {
        let Some(zoff) = find_next_zlib_near(f, next_off)? else { break };
        let (c, nxt) = decompress_stream_at(f, zoff, 64 * 1024 * 1024)?;
        p.extend_from_slice(&c);
        next_off = nxt;
    }
    Ok((v, p))
}

/// Probe if `c0` (first chunk of a RAGE9 resource) is a pgDictionary<grcTexture>.
pub fn probe_txd_header(c0: &[u8]) -> Option<(String, usize)> {
    if c0.len() < 0x40 { return None; }
    let h_ptr = ru64(c0, 0x20);
    let h_cnt = ru16(c0, 0x28) as usize;
    let i_ptr = ru64(c0, 0x30);
    let i_cnt = ru16(c0, 0x38) as usize;
    let i_cap = ru16(c0, 0x3a) as usize;
    if vptr(h_ptr).is_none() || vptr(i_ptr).is_none() { return None; }
    if !(1..=2048).contains(&i_cnt) || i_cnt != h_cnt || i_cnt > i_cap { return None; }
    let arr_off = vptr(i_ptr)?;
    if arr_off + 8 > c0.len() { return None; }
    let tp = vptr(ru64(c0, arr_off))?;
    if tp + 0x44 > c0.len() { return None; }
    pptr(ru64(c0, tp + 0x18))?;
    let w = ru16(c0, tp + 0x38) as usize;
    let h = ru16(c0, tp + 0x3a) as usize;
    let dxgi = c0[tp + 0x3f];
    if !(4..=8192).contains(&w) || !(4..=8192).contains(&h) || dxgi_info(dxgi).is_none() {
        return None;
    }
    let mut first_nm = format!("txd_{i_cnt}tex");
    if let Some(mut no) = vptr(ru64(c0, tp + 0x30)) {
        if no + 8 < c0.len() && c0[no..no + 8] == [0u8; 8] { no += 8; }
        if let Some(rel) = c0[no..].iter().position(|&b| b == 0) {
            if rel >= 2 && rel < 96 {
                let s = String::from_utf8_lossy(&c0[no..no + rel]).into_owned();
                if s.chars().all(|c| c.is_ascii_graphic()) {
                    first_nm = if i_cnt > 1 { format!("{s} (+{}).#td", i_cnt - 1) } else { format!("{s}.#td") };
                }
            }
        }
    }
    Some((first_nm, i_cnt))
}

// =============================================================================
// TTPF (RDS5 / ETPF) Native Scanner & Decoder
// =============================================================================

#[derive(Clone, Debug)]
pub struct TtpfTexMeta {
    pub idx: usize,
    pub w: usize,
    pub h: usize,
    pub fmt_byte: u8,
    pub dxgi: u8,
    pub has_mips: bool,
    /// Mip levels in the chain (descriptor word 0, bits 28..31, plus one).
    pub mip_count: u8,
    /// Gfx10 swizzle mode (descriptor word 1, bits 24..27): 0 = 64 KiB blocks, 1..3 = 4 KiB blocks.
    pub tile_mode: u8,
    pub byte_off: u64,
    pub alloc_sz: u64,
}

pub fn scan_ttpf_descriptors(path: &Path) -> Result<(String, Vec<TtpfTexMeta>), String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let sz = f.metadata().map_err(|e| e.to_string())?.len();
    let mut hdr = [0u8; 24];
    f.read_exact(&mut hdr).map_err(|e| e.to_string())?;
    if &hdr[..4] != b"RDS5" {
        return Err("Not an RDS5 TTPF file".into());
    }
    let n_chunks = ru32(&hdr, 4);
    let tot_uncomp = ru32(&hdr, 8) as u64;
    let hdr_pages = ru32(&hdr, 16) as u64;
    let etpf_off = hdr_pages * 0x100;

    f.seek(SeekFrom::Start(etpf_off)).map_err(|e| e.to_string())?;
    let mut ehdr = [0u8; 16];
    f.read_exact(&mut ehdr).map_err(|e| e.to_string())?;
    if &ehdr[..4] != b"ETPF" {
        return Err(format!("Missing ETPF header at 0x{etpf_off:x}"));
    }
    let tex_count = ru32(&ehdr, 8) as usize;
    let mut recs = vec![0u8; tex_count * 16];
    f.read_exact(&mut recs).map_err(|e| e.to_string())?;

    let mut out = Vec::with_capacity(tex_count);
    for i in 0..tex_count {
        let u0 = ru32(&recs, i * 16);
        let u1 = ru32(&recs, i * 16 + 4);
        let u2 = ru32(&recs, i * 16 + 8);
        let w = ((u0 & 0x3FFF) + 1) as usize;
        let h = (((u0 >> 14) & 0x3FFF) + 1) as usize;
        let fmt_byte = (u1 & 0xFF) as u8;
        // Verified 100.00% across all 357,115 textures in all 57 .ttpf files:
        // ETPF u1 bits 3..9 ((u1 >> 3) & 0x7F) is the exact Microsoft DXGI_FORMAT enum!
        let dxgi = ((u1 >> 3) & 0x7F) as u8;
        let has_mips = (u1 >> 24) > 0;
        let mip_count = ((u0 >> 28) + 1) as u8;
        let tile_mode = ((u1 >> 24) & 0xF) as u8;
        let page4k = (u2 & 0x000F_FFFF) as u64;
        out.push(TtpfTexMeta { idx: i, w, h, fmt_byte, dxgi, has_mips, mip_count, tile_mode, byte_off: page4k * 0x1000, alloc_sz: 0 });
    }
    let mut offs: Vec<u64> = out.iter().map(|d| d.byte_off).collect();
    offs.sort_unstable();
    offs.dedup();
    let mut nxt: HashMap<u64, u64> = HashMap::new();
    for w2 in offs.windows(2) {
        nxt.insert(w2[0], w2[1]);
    }
    for d in &mut out {
        let (_, is_bc, bpe) = dxgi_info(d.dxgi).unwrap_or(("BC7_UNORM", true, 16));
        let ew = if is_bc { (d.w + 3) / 4 } else { d.w }.max(1);
        let eh = if is_bc { (d.h + 3) / 4 } else { d.h }.max(1);
        let est = ((ew * eh * bpe) as u64).max(0x10000);
        d.alloc_sz = nxt.get(&d.byte_off).copied().unwrap_or((d.byte_off + est).min(tot_uncomp)).saturating_sub(d.byte_off).max(0x1000);
    }
    let info = format!("RDS5/ETPF · {sz} B · {n_chunks} chunks · {tex_count} textures");
    Ok((info, out))
}

/// Exact virtual layout of every RDS5 chunk, straight from the chunk table (no inflating needed).
/// Entry i encodes chunk i's virtual END as (hi & 0xFFFFF) * 16KB + (lo >> 30) * 4KB,
/// and (lo >> 26) & 0xF encodes the NEXT chunk's codec (0=RAW, 1..7=BCPack BC1..BC7, 8=ZLIB).
pub struct TtpfIndex {
    pub descs: Vec<TtpfTexMeta>,
    /// (file_start, file_end, virt_start, virt_len, codec)
    chunks: Vec<(u64, u64, u64, u64, u8)>,
}

static TTPF_INDEX: std::sync::OnceLock<std::sync::Mutex<HashMap<std::path::PathBuf, std::sync::Arc<TtpfIndex>>>> = std::sync::OnceLock::new();

pub fn ttpf_index(path: &Path) -> Result<std::sync::Arc<TtpfIndex>, String> {
    let map = TTPF_INDEX.get_or_init(Default::default);
    if let Some(ix) = map.lock().unwrap().get(path) {
        return Ok(ix.clone());
    }
    let (_, descs) = scan_ttpf_descriptors(path)?;
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut hdr = [0u8; 24];
    f.read_exact(&mut hdr).map_err(|e| e.to_string())?;
    let n_chunks = ru32(&hdr, 4) as usize;
    let etpf_off = ru32(&hdr, 16) as u64 * 0x100;
    let mut tbl = vec![0u8; n_chunks * 8];
    f.read_exact(&mut tbl).map_err(|e| e.to_string())?;

    let mut chunks = Vec::with_capacity(n_chunks);
    let (mut prev_c, mut vstart, mut cur_codec) = (etpf_off, 0u64, 0u8);
    for i in 0..n_chunks {
        let lo = ru32(&tbl, i * 8);
        let hi = ru32(&tbl, i * 8 + 4);
        let cend = (lo & 0x03FF_FFFF) as u64 * 0x100;
        let next_codec = ((lo >> 26) & 0xF) as u8;
        let r = (lo >> 30) as u64;
        let vend = (hi & 0x000F_FFFF) as u64 * 0x4000 + r * 0x1000;
        if cend > prev_c && vend > vstart {
            chunks.push((prev_c, cend, vstart, vend - vstart, cur_codec));
            vstart = vend;
        }
        prev_c = cend;
        cur_codec = next_codec;
    }
    let ix = std::sync::Arc::new(TtpfIndex { descs, chunks });
    map.lock().unwrap().insert(path.to_path_buf(), ix.clone());
    Ok(ix)
}

/// Read `len` bytes of the RDS5 virtual stream starting at `off`, inflating only overlapping chunks.
pub fn ttpf_read_virtual(path: &Path, ix: &TtpfIndex, off: u64, len: usize) -> Result<Vec<u8>, String> {
    let end = off + len as u64;
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut out_buf = vec![0u8; len];
    let mut covered = 0usize;
    let mut dec = Decompress::new(true);
    let first = ix.chunks.partition_point(|c| c.2 + c.3 <= off);
    for &(cs, ce, vs, vl, codec) in &ix.chunks[first..] {
        if vs >= end { break; }
        let mut ch = vec![0u8; (ce - cs) as usize];
        f.seek(SeekFrom::Start(cs)).map_err(|e| e.to_string())?;
        f.read_exact(&mut ch).map_err(|e| e.to_string())?;
        let data = if is_zlib_head(&ch) {
            let mut out = Vec::with_capacity(vl as usize);
            dec.reset(true);
            let _ = dec.decompress_vec(&ch, &mut out, FlushDecompress::Finish); // partial output is still usable
            out
        } else if ch.len() as u64 == vl {
            ch
        } else {
            let mode = match codec {
                1 => "BC1", 2 => "BC2", 3 => "BC3", 4 => "BC4", 5 => "BC5", 6 => "BC6H", 7 => "BC7", _ => "HW",
            };
            return Err(format!("Texture uses Xbox Series X hardware BCPack ({mode}) — ASIC-only compression (not decodable on PC)"));
        };
        let s0 = vs.max(off);
        let s1 = (vs + data.len() as u64).min(end);
        if s1 > s0 {
            out_buf[(s0 - off) as usize..(s1 - off) as usize].copy_from_slice(&data[(s0 - vs) as usize..(s1 - vs) as usize]);
            covered += (s1 - s0) as usize;
        }
    }
    if covered == 0 {
        return Err("Texture data not found in zlib chunks (likely BCPack)".into());
    }
    Ok(out_buf)
}

// -----------------------------------------------------------------------------
// TTPF mip chains and the lower-mip fallback
//
// A texture's allocation holds its whole mip chain in the AMD Gfx10 (RDNA2) layout: mips
// are stored smallest first and mip 0 comes last. Mips small enough to fit in half a
// swizzle block share one "mip tail" block at the start of the allocation, each at its own
// 2D origin inside that block; every larger mip gets whole blocks of its own after it.
//
// The RDS5 store compresses each chunk with zlib, stores it raw, or uses BCPack (the Xbox
// hardware texture codec, no public decoder). When mip 0 lands in a BCPack chunk the
// smaller mips often still sit in a zlib or raw chunk, so we decode the largest of those.
// -----------------------------------------------------------------------------

/// Element dimensions of one swizzle block (`use_64k`: 64 KiB block, else 4 KiB).
/// Same block shapes the 4 KiB / 64 KiB deswizzlers above use.
fn swizzle_block_dims(bpe: usize, use_64k: bool) -> (usize, usize) {
    match (use_64k, bpe) {
        (false, 16) => (16, 16),
        (false, 8) => (32, 16),
        (false, 4) => (32, 32),
        (false, 2) => (64, 32),
        (false, _) => (64, 64),
        (true, 16) => (64, 64),
        (true, 8) => (128, 64),
        (true, 4) => (128, 128),
        (true, 2) => (256, 128),
        (true, _) => (256, 256),
    }
}

/// Where one mip level lives inside a texture allocation.
#[derive(Clone, Debug, PartialEq)]
struct TtpfMipSlot {
    /// Size in elements (4x4 blocks for BCn).
    ew: usize,
    eh: usize,
    /// Byte offset of the mip's first block from the texture start. Tail mips share the
    /// block at offset 0.
    offset: u64,
    /// Element origin inside the shared tail block, for mips in the tail.
    tail_origin: Option<(usize, usize)>,
}

#[derive(Clone, Debug)]
struct TtpfMipLayout {
    use_64k: bool,
    bpe: usize,
    block_w: usize,
    block_h: usize,
    block_bytes: u64,
    mips: Vec<TtpfMipSlot>,
}

impl TtpfMipLayout {
    /// `ew`/`eh`: mip-0 size in elements. Mip n is `max(1, e >> n)` elements per axis.
    fn new(ew: usize, eh: usize, mip_count: usize, bpe: usize, use_64k: bool) -> Self {
        Self::new_ext(ew, eh, mip_count, bpe, use_64k, true)
    }

    /// `extend`: lay out levels below the declared chain down to a tail block (see below).
    fn new_ext(ew: usize, eh: usize, mip_count: usize, bpe: usize, use_64k: bool, extend: bool) -> Self {
        let (block_w, block_h) = swizzle_block_dims(bpe, use_64k);
        let block_bytes = (block_w * block_h * bpe) as u64;
        // Gfx10 caps how many trailing mips may share the tail block at log2(block bytes) - 4.
        let max_in_tail = if use_64k { 12 } else { 8 };
        let declared = mip_count.max(1);
        let dim = |i: usize| ((ew >> i).max(1), (eh >> i).max(1));
        // A mip joins the tail once it fits in half a block (half width, full height) and is
        // among the last `max_in_tail` levels; every smaller mip is then in the tail too.
        let find_tail = |n: usize| {
            (0..n).find(|&i| dim(i).0 <= block_w / 2 && dim(i).1 <= block_h && n - i <= max_in_tail).unwrap_or(n)
        };
        // Measured on the stores: when the declared chain stops before reaching the tail
        // (wide textures like 1024x256 with 5 mips), the allocation still holds the lower
        // levels down to a tail block, so lay those out too and hide them afterwards.
        let mut n = declared;
        while extend && find_tail(n) == n && n < 16 && dim(n - 1) != (1, 1) {
            n += 1;
        }
        let first_tail = find_tail(n);
        let dims: Vec<(usize, usize)> = (0..n).map(dim).collect();
        // 256-byte micro block in elements; tail origins step in these units.
        let (micro_w, micro_h) = match bpe {
            16 => (4, 4),
            8 => (8, 4),
            4 => (8, 8),
            2 => (16, 8),
            _ => (16, 16),
        };
        let mut mips = vec![TtpfMipSlot { ew: 0, eh: 0, offset: 0, tail_origin: None }; n];
        // Mips outside the tail follow the tail block (if any), smallest first.
        let mut cursor = if first_tail < n { block_bytes } else { 0 };
        for i in (0..first_tail).rev() {
            let (w, h) = dims[i];
            mips[i] = TtpfMipSlot { ew: w, eh: h, offset: cursor, tail_origin: None };
            cursor += (w.div_ceil(block_w) * h.div_ceil(block_h)) as u64 * block_bytes;
        }
        for (k, i) in (first_tail..n).enumerate() {
            // Slot ids count down from the largest tail mip. A slot's byte position in the
            // block, split into 256-byte micro blocks, is a Morton code (y in the even bits,
            // x in the odd bits) of the mip's origin in micro-block units.
            let slot = (max_in_tail - 1 - k) as u32;
            let byte_pos: u32 = if slot > 6 { 16 << slot } else { slot << 8 };
            let morton = byte_pos >> 8;
            let (mut mx, mut my) = (0usize, 0usize);
            for bit in 0..6 {
                my |= (((morton >> (2 * bit)) & 1) as usize) << bit;
                mx |= (((morton >> (2 * bit + 1)) & 1) as usize) << bit;
            }
            mips[i] = TtpfMipSlot { ew: dims[i].0, eh: dims[i].1, offset: 0, tail_origin: Some((mx * micro_w, my * micro_h)) };
        }
        mips.truncate(declared);
        TtpfMipLayout { use_64k, bpe, block_w, block_h, block_bytes, mips }
    }

    /// Byte range of mip `m` from the texture start (the whole tail block for tail mips).
    fn span(&self, m: usize) -> (u64, u64) {
        let s = &self.mips[m];
        if s.tail_origin.is_some() {
            return (0, self.block_bytes);
        }
        let blocks = (s.ew.div_ceil(self.block_w) * s.eh.div_ceil(self.block_h)) as u64;
        (s.offset, s.offset + blocks * self.block_bytes)
    }

    /// Untile mip `m` from `raw` (the bytes of `span(m)`) into row-major elements.
    fn untile(&self, raw: &[u8], m: usize) -> Vec<u8> {
        let s = &self.mips[m];
        let bpe = self.bpe;
        // The deswizzlers divide pixel sizes by 4 for BCn; pass element sizes with
        // is_bc = false so they work in elements directly.
        let swz = |buf: &[u8], w: usize, h: usize| {
            if self.use_64k { deswizzle_64kb_ttpf(buf, w, h, false, bpe) } else { deswizzle_4kb_d(buf, w, h, false, bpe) }
        };
        let Some((ox, oy)) = s.tail_origin else { return swz(raw, s.ew, s.eh) };
        let block = swz(raw, self.block_w, self.block_h);
        let mut out = vec![0u8; s.ew * s.eh * bpe];
        let n = s.ew.min(self.block_w.saturating_sub(ox)) * bpe;
        for y in 0..s.eh.min(self.block_h.saturating_sub(oy)) {
            let src = ((oy + y) * self.block_w + ox) * bpe;
            out[y * s.ew * bpe..][..n].copy_from_slice(&block[src..src + n]);
        }
        out
    }
}

impl TtpfIndex {
    /// True if any byte of the virtual range [off, off + len) lies in a BCPack chunk.
    fn range_hits_bcpack(&self, off: u64, len: u64) -> bool {
        let end = off + len;
        let first = self.chunks.partition_point(|c| c.2 + c.3 <= off);
        self.chunks[first..].iter().take_while(|c| c.2 < end).any(|c| (1..=7).contains(&c.4))
    }
}

/// Mip-chain layout of a BCn texture, or None for tile modes we can't place.
fn ttpf_bc_layout(d: &TtpfTexMeta, bpe: usize) -> Option<TtpfMipLayout> {
    let use_64k = match d.tile_mode {
        0 => true,
        1..=3 => false,
        _ => return None,
    };
    Some(TtpfMipLayout::new(d.w.div_ceil(4).max(1), d.h.div_ceil(4).max(1), d.mip_count as usize, bpe, use_64k))
}

/// A smaller mip decoded in place of a BCPack-compressed mip 0.
struct TtpfLowerMip {
    mip: usize,
    w: usize,
    h: usize,
    lin: Vec<u8>,
    pixels: Vec<Color32>,
}

/// Decode the largest mip below mip 0 that is stored outside BCPack chunks.
fn ttpf_decode_lower_mip(path: &Path, ix: &TtpfIndex, d: &TtpfTexMeta) -> Option<TtpfLowerMip> {
    let (_, is_bc, bpe) = dxgi_info(d.dxgi)?;
    // BCPack only wraps BCn data, and the mip-chain layout is only verified for BCn.
    if !is_bc || d.mip_count < 2 {
        return None;
    }
    let layout = ttpf_bc_layout(d, bpe)?;
    for m in 1..layout.mips.len() {
        let (a, b) = layout.span(m);
        if ix.range_hits_bcpack(d.byte_off + a, b - a) {
            continue;
        }
        let Ok(raw) = ttpf_read_virtual(path, ix, d.byte_off + a, (b - a) as usize) else { continue };
        let lin = layout.untile(&raw, m);
        let s = &layout.mips[m];
        // Decode whole 4x4 blocks, then trim to the mip's pixel size.
        let (bw, bh) = (s.ew * 4, s.eh * 4);
        let Some(px) = decode_linear_to_rgba(&lin, bw, bh, d.dxgi) else { continue };
        let (w, h) = ((d.w >> m).max(1).min(bw), (d.h >> m).max(1).min(bh));
        let pixels = if (w, h) == (bw, bh) { px } else { (0..h).flat_map(|y| px[y * bw..y * bw + w].iter().copied()).collect() };
        return Some(TtpfLowerMip { mip: m, w, h, lin, pixels });
    }
    None
}

/// Virtual offset, padded length and 64 KiB-tiling flag of a texture's full-size mip.
fn ttpf_mip0_range(d: &TtpfTexMeta, is_bc: bool, bpe: usize) -> (u64, usize, bool) {
    let ew = if is_bc { (d.w + 3) / 4 } else { d.w }.max(1);
    let eh = if is_bc { (d.h + 3) / 4 } else { d.h }.max(1);
    let mip0_lin = ew * eh * bpe;
    // Mipmapped textures use 4KB tiles at every size; mip-less ones use 64KB tiles once over 64KB
    // (uncompressed >= 128KB also uses 64KB tiles when mip-less or full-size allocation).
    let use_64k = (!d.has_mips || !is_bc) && mip0_lin > 65536;
    let mip0_padded = if use_64k {
        padded_64kb_size(d.w, d.h, is_bc, bpe)
    } else {
        padded_4kb_size(d.w, d.h, is_bc, bpe)
    };
    // Mips are stored smallest-first, so mip0 sits at the END of the allocation
    let mip0_off = if d.alloc_sz > mip0_padded as u64 && d.alloc_sz <= (mip0_padded as f64 * 1.65) as u64 {
        d.byte_off + (d.alloc_sz - mip0_padded as u64)
    } else {
        d.byte_off
    };
    (mip0_off, mip0_padded, use_64k)
}

/// Decode a single texture by descriptor index from a `.ttpf` archive.
///
/// When mip 0 is BCPack-compressed this falls back to the largest smaller mip stored as raw
/// or zlib data: `full_size` is then set and `w`/`h`/`pixels` describe the reduced mip.
pub fn decode_ttpf_texture(path: &Path, tex_idx: usize) -> Result<DecodedTexture, String> {
    let ix = ttpf_index(path)?;
    let d = ix.descs.get(tex_idx).ok_or("Texture index out of range")?;

    let (fmt_name, is_bc, bpe) = dxgi_info(d.dxgi)
        .ok_or_else(|| format!("Unsupported DXGI format {} (fmt_byte=0x{:02x})", d.dxgi, d.fmt_byte))?;

    let (lin, pixels) = match ttpf_decode_mip0(path, &ix, d, is_bc, bpe) {
        Ok(v) => v,
        Err(err) if err.contains("BCPack") => {
            let Some(low) = ttpf_decode_lower_mip(path, &ix, d) else {
                return Err(format!("{err}; no smaller mip is stored outside BCPack"));
            };
            return Ok(DecodedTexture {
                name: format!("tex_{:05}_{}x{}_{fmt_name}_mip{}", d.idx, d.w, d.h, low.mip),
                w: low.w,
                h: low.h,
                mips: d.mip_count,
                dxgi: d.dxgi,
                fmt_name,
                pixels: low.pixels,
                raw_dds_payload: low.lin,
                full_size: Some((d.w, d.h)),
            });
        }
        Err(err) => return Err(err),
    };
    Ok(DecodedTexture {
        name: format!("tex_{:05}_{}x{}_{fmt_name}", d.idx, d.w, d.h),
        w: d.w,
        h: d.h,
        mips: d.mip_count,
        dxgi: d.dxgi,
        fmt_name,
        pixels,
        raw_dds_payload: lin,
        full_size: None,
    })
}

/// One guess at where mip 0 lives, plus where its mip 1 would then be.
struct Mip0Guess {
    label: &'static str,
    off: u64,
    len: usize,
    /// Untile with this layout's mip 0; None means the original deswizzlers.
    layout: Option<TtpfMipLayout>,
    use_64k: bool,
    /// Mip 1 location and the layout that untiles it (its mip index 1).
    mip1: Option<(u64, u64, TtpfMipLayout)>,
}

/// Decodes a texture's full-size mip, returning (linear blocks, pixels).
///
/// The descriptor doesn't say exactly where mip 0 starts, and no single rule fits the
/// whole dump: the original guess (end of the allocation, else its start) shows the
/// small-mip strip for many multi-mip textures, and the computed mip-chain layout is off
/// for many UI and short-chain textures. So for mipmapped BCn textures we try each
/// plausible placement and keep the one whose mip 1 is a 2x downscale of its mip 0;
/// a wrong placement pairs two unrelated images. Measured with
/// `ttpf_mip0_choice_audit`. When every guess agrees (most textures) only one is decoded.
fn ttpf_decode_mip0(path: &Path, ix: &TtpfIndex, d: &TtpfTexMeta, is_bc: bool, bpe: usize) -> Result<(Vec<u8>, Vec<Color32>), String> {
    ttpf_decode_mip0_labeled(path, ix, d, is_bc, bpe).map(|(_, v)| v)
}

fn ttpf_decode_mip0_labeled(path: &Path, ix: &TtpfIndex, d: &TtpfTexMeta, is_bc: bool, bpe: usize) -> Result<(&'static str, (Vec<u8>, Vec<Color32>)), String> {
    let guesses = ttpf_mip0_guesses(d, is_bc, bpe);
    let decode = |g: &Mip0Guess| ttpf_decode_guess(path, ix, d, is_bc, bpe, g);
    if guesses.len() == 1 {
        return decode(&guesses[0]).map(|v| (guesses[0].label, v));
    }
    // (decoded mip 0, how well its mip 1 matches) for each guess; guesses[0] is the baseline
    let mut results: Vec<(Result<(Vec<u8>, Vec<Color32>), String>, Option<f64>)> = guesses
        .iter()
        .map(|g| {
            let r = decode(g);
            let score = match (&r, &g.mip1) {
                (Ok((_, px0)), Some((o1, l1, lay))) => ttpf_read_virtual(path, ix, *o1, *l1 as usize).ok().and_then(|raw1| {
                    let s1 = &lay.mips[1];
                    let px1 = decode_linear_to_rgba(&lay.untile(&raw1, 1), s1.ew * 4, s1.eh * 4, d.dxgi)?;
                    mip_pair_error(px0, d.w, d.h, &px1, s1.ew * 4)
                }),
                _ => None,
            };
            (r, score)
        })
        .collect();
    // Move away from the baseline only on clear evidence: an alternative whose mip pair is
    // close (mean error under MIP_MATCH_MAX per channel) and at most half the baseline's
    // error, or, when the baseline can't be scored or read, a very close pair
    // (MIP_MATCH_STRICT). Flat images such as UI icons pair up fairly well even when
    // misplaced, and the baseline is right for those; real fixes on the dump score under 5.
    const MIP_MATCH_MAX: f64 = 12.0;
    const MIP_MATCH_STRICT: f64 = 5.0;
    let base_score = if results[0].0.is_ok() { results[0].1 } else { None };
    let best_alt = (1..results.len())
        .filter(|&i| results[i].0.is_ok())
        .filter_map(|i| results[i].1.map(|e| (i, e)))
        .min_by(|a, b| a.1.total_cmp(&b.1));
    let pick = match (best_alt, base_score) {
        (Some((i, e)), Some(o)) if e < MIP_MATCH_MAX && e * 2.0 < o => i,
        (Some((i, e)), None) if e < MIP_MATCH_STRICT => i,
        _ => 0,
    };
    let label = guesses[pick].label;
    // An unreadable baseline (BCPack) keeps its error so the lower-mip fallback runs.
    results.swap_remove(pick).0.map(|v| (label, v))
}

/// Read and untile one mip-0 guess, returning (linear blocks, pixels).
fn ttpf_decode_guess(path: &Path, ix: &TtpfIndex, d: &TtpfTexMeta, is_bc: bool, bpe: usize, g: &Mip0Guess) -> Result<(Vec<u8>, Vec<Color32>), String> {
    let raw = ttpf_read_virtual(path, ix, g.off, g.len)?;
    let lin = match &g.layout {
        Some(l) => l.untile(&raw, 0),
        None if g.use_64k => deswizzle_64kb_ttpf(&raw, d.w, d.h, is_bc, bpe),
        None => deswizzle_4kb_d(&raw, d.w, d.h, is_bc, bpe),
    };
    let px = decode_linear_to_rgba(&lin, d.w, d.h, d.dxgi).ok_or_else(|| format!("Pixel decode failed for DXGI {} ({}x{})", d.dxgi, d.w, d.h))?;
    Ok((lin, px))
}

/// Mean absolute RGB difference between `px1` and a 2x2 box filter of `px0`, on a sample
/// of pixels. None when the texture is too small to compare.
fn mip_pair_error(px0: &[Color32], w0: usize, h0: usize, px1: &[Color32], stride1: usize) -> Option<f64> {
    let (w1, h1) = (w0 / 2, h0 / 2);
    if w1 < 2 || h1 < 2 {
        return None;
    }
    let step = ((w1 * h1) as f64 / 16384.0).sqrt().ceil().max(1.0) as usize;
    // A guess that lands on padding decodes to one flat color for both mips and would
    // score a perfect match, so a (nearly) single-color mip 1 proves nothing.
    let (mut sum, mut sum2, mut cnt) = (0f64, 0f64, 0f64);
    for y in (0..h1).step_by(step) {
        for x in (0..w1).step_by(step) {
            let c = px1[y * stride1 + x];
            let v = c.r() as f64 + c.g() as f64 + c.b() as f64 + c.a() as f64;
            sum += v;
            sum2 += v * v;
            cnt += 1.0;
        }
    }
    let var = sum2 / cnt - (sum / cnt).powi(2);
    if var < 16.0 {
        return None;
    }
    let (mut err, mut n) = (0u64, 0u64);
    for y in (0..h1).step_by(step) {
        for x in (0..w1).step_by(step) {
            let q = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(dx, dy)| px0[(2 * y + dy) * w0 + 2 * x + dx]);
            let c = px1[y * stride1 + x];
            for (k, v) in [c.r(), c.g(), c.b(), c.a()].into_iter().enumerate() {
                let avg = q.iter().map(|p| [p.r(), p.g(), p.b(), p.a()][k] as u32).sum::<u32>() / 4;
                err += (avg as i32 - v as i32).unsigned_abs() as u64;
            }
            n += 4;
        }
    }
    Some(err as f64 / n.max(1) as f64)
}

/// Candidate placements for mip 0, first = preferred when they can't be told apart.
fn ttpf_mip0_guesses(d: &TtpfTexMeta, is_bc: bool, bpe: usize) -> Vec<Mip0Guess> {
    let (oo, ol, o64) = ttpf_mip0_range(d, is_bc, bpe);
    let old = |mip1| Mip0Guess { label: "old", off: oo, len: ol, layout: None, use_64k: o64, mip1 };
    let Some(ext) = (if is_bc && d.mip_count > 1 { ttpf_bc_layout(d, bpe) } else { None }) else {
        return vec![old(None)];
    };
    let (ew, eh) = (d.w.div_ceil(4).max(1), d.h.div_ceil(4).max(1));
    let noext = TtpfMipLayout::new_ext(ew, eh, d.mip_count as usize, bpe, ext.use_64k, false);
    let mut out: Vec<Mip0Guess> = Vec::new();
    let push = |out: &mut Vec<Mip0Guess>, g: Mip0Guess| {
        if !out.iter().any(|o| o.off == g.off && o.use_64k == g.use_64k) {
            out.push(g);
        }
    };
    let span_len = |l: &TtpfMipLayout, m: usize| {
        let (a, b) = l.span(m);
        b - a
    };
    // mip 1 sits right before mip 0 when neither is in the tail block
    let before = |off0: u64, l: &TtpfMipLayout| -> Option<(u64, u64, TtpfMipLayout)> {
        let n1 = span_len(l, 1);
        (l.mips[0].tail_origin.is_none() && l.mips[1].tail_origin.is_none() && off0 >= d.byte_off + n1).then(|| (off0 - n1, n1, l.clone()))
    };
    let from_layout = |label: &'static str, l: TtpfMipLayout| {
        let (a, b) = l.span(0);
        let (a1, b1) = l.span(1);
        Mip0Guess { label, off: d.byte_off + a, len: (b - a) as usize, use_64k: l.use_64k, mip1: Some((d.byte_off + a1, b1 - a1, l.clone())), layout: Some(l) }
    };
    // Baseline first: what Percy6Stars decoded before guesses were compared. Tiny textures whose
    // mip 0 sits inside the shared tail block are read from the tail (reading the block's
    // origin shows an empty mask); everything else uses the original allocation guess.
    if ext.mips[0].tail_origin.is_some() && ext.block_bytes <= d.alloc_sz {
        push(&mut out, from_layout("tail", ext.clone()));
    }
    // the old guess's mip 1, untiled the way the old guess untiles mip 0
    let old_tiling = if o64 == ext.use_64k { ext.clone() } else { TtpfMipLayout::new(ew, eh, d.mip_count as usize, bpe, o64) };
    let old_mip1 = before(oo, &old_tiling);
    push(&mut out, old(old_mip1));
    push(&mut out, from_layout("layout", ext.clone()));
    push(&mut out, from_layout("layout-short", noext));
    let n0 = span_len(&ext, 0);
    if ext.mips[0].tail_origin.is_none() && d.alloc_sz >= n0 {
        let off = d.byte_off + d.alloc_sz - n0;
        let mut one = ext.clone();
        one.mips.truncate(2);
        push(&mut out, Mip0Guess { label: "end", off, len: n0 as usize, layout: Some(one.clone()), use_64k: one.use_64k, mip1: before(off, &one) });
    }
    out
}

/// Global 64-bit ETPF texture hash -> (ttpf archive path, descriptor index, w, h, dxgi) resolver.
/// Each RDS5/ETPF archive stores `tex_count` 64-bit texture hashes immediately after the
/// `tex_count * 16`-byte descriptor table, 1:1 indexed with the descriptors.
#[derive(Clone, Debug)]
pub struct TtpfHashHit {
    pub path: std::path::PathBuf,
    pub tex_idx: usize,
    pub w: usize,
    pub h: usize,
    pub dxgi: u8,
}

/// Texture hash -> location, built once per archive folder (keyed by that root).
type TtpfHashIndex = std::sync::Arc<HashMap<u64, TtpfHashHit>>;
static TTPF_HASH_INDEX: std::sync::OnceLock<std::sync::Mutex<HashMap<std::path::PathBuf, TtpfHashIndex>>> =
    std::sync::OnceLock::new();

/// Cache by archive and descriptor, since different dumps can contain the same hashes.
type TtpfTexCache = HashMap<(std::path::PathBuf, usize), Option<std::sync::Arc<Texture2D>>>;
static TTPF_TEX_CACHE: std::sync::OnceLock<std::sync::Mutex<TtpfTexCache>> = std::sync::OnceLock::new();

fn collect_ttpf_files(dir: &Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
    // pack/<name>/data/ttdb/<category> is five levels below the dump root.
    if depth > 5 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !name.starts_with('.') {
                collect_ttpf_files(&p, depth + 1, out);
            }
        } else if p
            .extension()
            .and_then(|s| s.to_str())
            .map(|e| e.eq_ignore_ascii_case("ttpf"))
            .unwrap_or(false)
        {
            out.push(p);
        }
    }
}

fn find_dump_root_for_ttpf(rpf_path: &Path) -> Option<std::path::PathBuf> {
    // Pack drawables can reference both their own TTDB and the base game's TTDB.
    for ancestor in rpf_path.ancestors().skip(1).take(4) {
        if ancestor.file_name().is_some_and(|name| name == "pack") {
            if let Some(root) = ancestor.parent().filter(|p| p.join("data/ttdb").is_dir()) {
                return Some(root.to_path_buf());
            }
        }
    }
    let mut cur = rpf_path.parent()?;
    for _ in 0..4 {
        if cur.join("data").join("ttdb").is_dir() || cur.join("textures_loose.ttpf").is_file() {
            return Some(cur.to_path_buf());
        }
        if let Ok(rd) = std::fs::read_dir(cur) {
            if rd.flatten().any(|e| {
                e.path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|x| x.eq_ignore_ascii_case("ttpf"))
                    .unwrap_or(false)
            }) {
                return Some(cur.to_path_buf());
            }
        }
        cur = match cur.parent() {
            Some(p) => p,
            None => break,
        };
    }
    None
}

pub fn ttpf_hash_index_for_rpf(rpf_path: &Path) -> Option<std::sync::Arc<HashMap<u64, TtpfHashHit>>> {
    let root = find_dump_root_for_ttpf(rpf_path)?;
    let map = TTPF_HASH_INDEX.get_or_init(Default::default);
    if let Some(existing) = map.lock().unwrap().get(&root) {
        return Some(existing.clone());
    }

    let mut ttpf_files = Vec::new();
    collect_ttpf_files(&root, 0, &mut ttpf_files);
    // Prefer the shared archives on duplicate hashes and make selection reproducible.
    ttpf_files.sort_unstable_by(|a, b| a.components().count().cmp(&b.components().count()).then_with(|| a.cmp(b)));

    let mut index: HashMap<u64, TtpfHashHit> = HashMap::with_capacity(360_000);
    for tp in ttpf_files {
        let Ok(mut f) = File::open(&tp) else { continue };
        let mut hdr = [0u8; 24];
        if f.read_exact(&mut hdr).is_err() || &hdr[..4] != b"RDS5" {
            continue;
        }
        let etpf_off = ru32(&hdr, 16) as u64 * 0x100;
        if f.seek(SeekFrom::Start(etpf_off)).is_err() {
            continue;
        }
        let mut ehdr = [0u8; 16];
        if f.read_exact(&mut ehdr).is_err() || &ehdr[..4] != b"ETPF" {
            continue;
        }
        let tex_count = ru32(&ehdr, 8) as usize;
        if tex_count == 0 || tex_count > 500_000 {
            continue;
        }
        let mut recs = vec![0u8; tex_count * 16];
        let mut hashes = vec![0u8; tex_count * 8];
        if f.read_exact(&mut recs).is_err() || f.read_exact(&mut hashes).is_err() {
            continue;
        }
        for i in 0..tex_count {
            let h64 = ru64(&hashes, i * 8);
            if h64 == 0 { continue; }
            let u0 = ru32(&recs, i * 16);
            let u1 = ru32(&recs, i * 16 + 4);
            let w = ((u0 & 0x3FFF) + 1) as usize;
            let h = (((u0 >> 14) & 0x3FFF) + 1) as usize;
            let dxgi = ((u1 >> 3) & 0x7F) as u8;
            index.entry(h64).or_insert_with(|| TtpfHashHit {
                path: tp.clone(),
                tex_idx: i,
                w,
                h,
                dxgi,
            });
        }
    }

    if index.is_empty() {
        return None;
    }
    let arc = std::sync::Arc::new(index);
    map.lock().unwrap().insert(root, arc.clone());
    Some(arc)
}

pub fn decode_ttpf_texture_cached(hit: &TtpfHashHit) -> Option<std::sync::Arc<Texture2D>> {
    let cache = TTPF_TEX_CACHE.get_or_init(Default::default);
    let key = (hit.path.clone(), hit.tex_idx);
    if let Some(entry) = cache.lock().unwrap().get(&key) {
        return entry.clone();
    }
    let decoded = decode_ttpf_texture(&hit.path, hit.tex_idx)
        .ok()
        .map(|t| std::sync::Arc::new(to_model_tex(t)));
    let mut g = cache.lock().unwrap();
    if g.len() > 512 {
        g.clear();
    }
    g.insert(key, decoded.clone());
    decoded
}

// =============================================================================
// RBF0 (fwui UIObject) -> Readable XML Decoder
// =============================================================================

pub fn decode_rbf0(data: &[u8]) -> Option<String> {
    if data.len() < 8 || &data[..4] != b"RBF0" { return None; }
    let mut pos = 4usize;
    let n = data.len();
    let mut descs: Vec<(String, u8)> = Vec::new();
    let mut stack: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
    let mut root_lines: Vec<String> = Vec::new();

    let esc = |s: &str| -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    };

    while pos < n {
        let b0 = data[pos];
        if b0 == 0xFB && pos + 3 < n && data[pos + 1] == 0xFF {
            pos += 4;
            continue;
        }
        if b0 == 0xFF {
            if pos + 1 >= n { break; }
            let b1 = data[pos + 1];
            pos += 2;
            if b1 == 0xFF {
                if let Some((tag, attrs, kids)) = stack.pop() {
                    let indent = "  ".repeat(stack.len());
                    let attr_s = if attrs.is_empty() { String::new() } else { format!(" {}", attrs.join(" ")) };
                    let line = if kids.is_empty() {
                        format!("{indent}<{tag}{attr_s} />")
                    } else if kids.len() == 1 && !kids[0].starts_with('<') && !kids[0].contains('\n') {
                        format!("{indent}<{tag}{attr_s}>{}</{tag}>", kids[0])
                    } else {
                        format!("{indent}<{tag}{attr_s}>\n{}\n{indent}</{tag}>", kids.join("\n"))
                    };
                    if let Some(p) = stack.last_mut() { p.2.push(line); } else { root_lines.push(line); }
                }
            }
            continue;
        }
        if b0 == 0xFD && pos + 5 < n && data[pos + 1] == 0xFF {
            pos += 2;
            let dlen = ru32(data, pos) as usize;
            pos += 4;
            if pos + dlen > n { break; }
            let raw = &data[pos..pos + dlen];
            pos += dlen;
            let trimmed = raw.strip_suffix(&[0]).unwrap_or(raw);
            let txt = String::from_utf8_lossy(trimmed);
            if let Some(p) = stack.last_mut() { p.2.push(esc(&txt)); }
            continue;
        }
        if pos + 2 > n { break; }
        let idx = b0 as usize;
        let dtype = data[pos + 1];
        pos += 2;

        let name = if idx == descs.len() {
            if pos + 2 > n { break; }
            let nlen = ru16(data, pos) as usize;
            pos += 2;
            if pos + nlen > n { break; }
            let nm = String::from_utf8_lossy(&data[pos..pos + nlen]).into_owned();
            pos += nlen;
            descs.push((nm.clone(), dtype));
            nm
        } else if idx < descs.len() {
            descs[idx].0.clone()
        } else {
            break;
        };

        match dtype {
            0x00 => {
                if pos + 6 > n { break; }
                pos += 6;
                stack.push((name, Vec::new(), Vec::new()));
            }
            0x10 => {
                if pos + 4 > n { break; }
                let v = ru32(data, pos); pos += 4;
                if let Some(p) = stack.last_mut() { p.1.push(format!("{name}=\"{v}\"")); }
            }
            0x20 => {
                if pos + 1 > n { break; }
                let v = data[pos] != 0; pos += 1;
                if let Some(p) = stack.last_mut() { p.1.push(format!("{name}=\"{v}\"")); }
            }
            0x30 => {
                if pos + 1 > n { break; }
                let v = data[pos]; pos += 1;
                if let Some(p) = stack.last_mut() { p.1.push(format!("{name}=\"{v}\"")); }
            }
            0x40 => {
                if pos + 4 > n { break; }
                let v = rf32(data, pos); pos += 4;
                if let Some(p) = stack.last_mut() { p.1.push(format!("{name}=\"{v:.5}\"")); }
            }
            0x50 => {
                if pos + 12 > n { break; }
                let (x, y, z) = (rf32(data, pos), rf32(data, pos + 4), rf32(data, pos + 8));
                pos += 12;
                if let Some(p) = stack.last_mut() { p.1.push(format!("{name}=\"{x:.4},{y:.4},{z:.4}\"")); }
            }
            0x60 => {
                if pos + 2 > n { break; }
                let slen = ru16(data, pos) as usize; pos += 2;
                if pos + slen > n { break; }
                let sv = String::from_utf8_lossy(&data[pos..pos + slen]);
                pos += slen;
                if let Some(p) = stack.last_mut() { p.1.push(format!("{name}=\"{}\"", esc(&sv))); }
            }
            _ => break,
        }
    }
    while let Some((tag, attrs, kids)) = stack.pop() {
        let indent = "  ".repeat(stack.len());
        let attr_s = if attrs.is_empty() { String::new() } else { format!(" {}", attrs.join(" ")) };
        let line = format!("{indent}<{tag}{attr_s}>\n{}\n{indent}</{tag}>", kids.join("\n"));
        if let Some(p) = stack.last_mut() { p.2.push(line); } else { root_lines.push(line); }
    }
    (!root_lines.is_empty()).then(|| root_lines.join("\n"))
}

// =============================================================================
// Scaleform GFX (v42 AS3) Inspector
// =============================================================================

pub fn inspect_gfx(data: &[u8]) -> String {
    if data.len() < 8 || &data[..3] != b"GFX" {
        return "Not a Scaleform GFX binary".into();
    }
    let ver = data[3];
    let fsize = ru32(data, 4);
    let mut out = format!("Scaleform GFx Binary (ActionScript 3)\nVersion: {ver} (0x{ver:02x})  |  Header Size: {fsize} B  |  Chunk Size: {} B\n\nRecovered AS3 Classes / Packages / Symbols:\n", data.len());
    let mut seen = HashSet::new();
    let mut cur = Vec::new();
    let mut syms = Vec::new();
    for &b in data {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'/') {
            cur.push(b);
        } else {
            if cur.len() >= 6 && cur.len() <= 96 {
                if let Ok(s) = std::str::from_utf8(&cur) {
                    if (s.contains('.') || s.contains(':') || s.contains("fontlib") || s.contains("rockstar") || s.contains("scaleform") || s.contains("general"))
                        && s.chars().filter(|c| c.is_ascii_alphabetic()).count() >= 4
                        && seen.insert(s.to_string())
                    {
                        syms.push(s.to_string());
                    }
                }
            }
            cur.clear();
        }
    }
    syms.sort();
    for s in syms.iter().take(400) {
        out.push_str("  ");
        out.push_str(s);
        out.push('\n');
    }
    out
}

/// Fast snippet extractor for naming `0DRPz` / standalone text chunks in the archive list
/// so users can Ctrl+F radio artists, weapons, vehicles, or dialogue directly in Percy6Stars.
pub fn probe_gxt_snippet(head: &[u8]) -> Option<String> {
    let is_0drpz = head.len() >= 24 && &head[16..21] == b"0DRPz";
    let start = if is_0drpz { 0x60.min(head.len()) } else { 0 };
    let mut good = Vec::new();
    for raw in head[start..].split(|&b| b == 0) {
        if raw.len() < 4 || raw.len() > 160 { continue; }
        let Ok(s) = std::str::from_utf8(raw) else { continue };
        let s = s.trim();
        if s.len() < 4 || s == "0DRPz" { continue; }
        let ok_chars = s.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '\'' | '.' | '!' | '?' | ',')).count();
        let letters = s.chars().filter(|c| c.is_ascii_alphabetic()).count();
        if ok_chars * 100 / s.chars().count().max(1) >= 92 && letters >= 3 {
            good.push(s);
            if good.len() >= 2 { break; }
        }
    }
    if is_0drpz || good.len() >= 2 {
        let slug: String = good.first().copied().unwrap_or("text")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(32)
            .collect();
        let slug = slug.trim_matches('_');
        return Some(if slug.is_empty() { "text".into() } else { slug.into() });
    }
    None
}

/// Extract clean readable UTF-8 strings from a RAGE9 `0DRPz` localization/text table
/// (or standalone null-terminated string pool chunk in `data.rpf`).
pub fn decode_0drpz_text(data: &[u8]) -> Option<String> {
    decode_0drpz_text_min(data, 3)
}

/// Like [`decode_0drpz_text`] but accepts tables with as few as `min_lines` strings. Use 1 for
/// entries already known to be text tables (GXT); keep 3 when probing arbitrary binaries so
/// stray byte runs don't pass as text.
pub fn decode_0drpz_text_min(data: &[u8], min_lines: usize) -> Option<String> {
    let start = if data.len() >= 0x60 && &data[0x10..0x15] == b"0DRPz" { 0x60 } else { 0 };
    let end = if start == 0x60 {
        let bmap = ru64(data, 8);
        if (0x5000_0000..0x5800_0000).contains(&bmap) {
            ((bmap - 0x5000_0000) as usize).clamp(start, data.len())
        } else {
            data.len()
        }
    } else {
        data.len()
    };
    let mut lines = Vec::new();
    for raw in data[start..end].split(|&b| b == 0) {
        if raw.len() < 3 || raw.len() > 2000 { continue; }
        let Ok(s) = std::str::from_utf8(raw) else { continue };
        let s = s.trim();
        if s.len() < 3 || s == "0DRPz" { continue; }
        let printable = s.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t')).count();
        let letters = s.chars().filter(|c| c.is_alphabetic()).count();
        if printable * 100 / s.chars().count().max(1) >= 96 && letters >= 2 {
            // Skip tiny binary hash-table noise
            if s.len() <= 5 && s.chars().any(|c| !c.is_alphanumeric()) && !s.starts_with('~') {
                continue;
            }
            lines.push(s.to_string());
            if lines.len() >= 6000 { break; }
        }
    }
    if lines.len() < min_lines.max(1) {
        return None;
    }
    let header = format!("=== Text Table ({} strings) ===\n\n", lines.len());
    Some(header + &lines.join("\n"))
}
// =============================================================================

pub fn decode_navmesh_from_rpf(rpf_path: &Path, file_off: u64, fallback_name: &str) -> Result<Mesh3D, String> {
    let mut f = File::open(rpf_path).map_err(|e| format!("Open error: {e}"))?;
    let (v, _) = read_rage9_vp(&mut f, file_off)?;
    if v.len() < 0xD0 || ru32(&v, 0x14) != 0x0001_0015 {
        return Err("Not a valid CNavMesh tile (0x00010015)".into());
    }
    let ext = [rf32(&v, 0x60), rf32(&v, 0x64), rf32(&v, 0x68)];
    let qt = vptr(ru64(&v, 0xA0)).ok_or("Missing QuadTree")?;
    if qt + 32 > v.len() { return Err("Truncated QuadTree".into()); }
    let min_v = [rf32(&v, qt), rf32(&v, qt + 4), rf32(&v, qt + 8)];

    let read_split_blocks = |ptr_off: usize| -> Option<Vec<(usize, usize)>> {
        let sa = vptr(ru64(&v, ptr_off))?;
        if sa + 0x24 > v.len() { return None; }
        let p_blks = vptr(ru64(&v, sa + 0x10))?;
        let n_blks = ru32(&v, sa + 0x20) as usize;
        if n_blks == 0 || n_blks > 4096 || p_blks + n_blks * 16 > v.len() { return None; }
        let mut blks = Vec::with_capacity(n_blks);
        for bi in 0..n_blks {
            let bptr = vptr(ru64(&v, p_blks + bi * 16))?;
            let bcnt = ru32(&v, p_blks + bi * 16 + 8) as usize;
            blks.push((bptr, bcnt));
        }
        Some(blks)
    };

    let v_blks = read_split_blocks(0x70).ok_or("Missing vertex split array")?;
    let i_blks = read_split_blocks(0x80).ok_or("Missing index split array")?;
    let p_blks = read_split_blocks(0x98).ok_or("Missing poly split array")?;

    let mut verts = Vec::new();
    for (boff, cnt) in v_blks {
        for i in 0..cnt {
            let o = boff + i * 6;
            if o + 6 > v.len() { break; }
            let vx = min_v[0] + (ru16(&v, o) as f32 / 65535.0) * ext[0];
            let vy = min_v[1] + (ru16(&v, o + 2) as f32 / 65535.0) * ext[1];
            let vz = min_v[2] + (ru16(&v, o + 4) as f32 / 65535.0) * ext[2];
            verts.push([vx, vy, vz]);
        }
    }

    let mut indices = Vec::new();
    for (boff, cnt) in i_blks {
        for i in 0..cnt {
            let o = boff + i * 2;
            if o + 2 > v.len() { break; }
            indices.push(ru16(&v, o) as u32);
        }
    }

    let mut tris = Vec::new();
    let mut tri_geom = Vec::new();
    let mut poly_cnt = 0usize;
    for (boff, cnt) in p_blks {
        for i in 0..cnt {
            let o = boff + i * 44;
            if o + 44 > v.len() { break; }
            let vinfo = ru32(&v, o + 4);
            let f8 = ru32(&v, o + 8);
            let f18 = ru32(&v, o + 0x18);
            let nv = (vinfo & 0xF) as usize;
            let first = (vinfo >> 4) as usize;
            if nv < 3 || first + nv > indices.len() { continue; }
            // Group material ID by navmesh surface type: 0=Land, 1=Water, 2=Road, 3=Pavement, 4=Interior/Shelter
            let mat: u16 = if (f8 & 0x80) != 0 || (f18 & 0x80000) != 0 {
                1
            } else if (f8 & 0x08) != 0 {
                4
            } else if (f18 & 0x2000) != 0 {
                2
            } else if (f18 & 0x1000) != 0 {
                3
            } else {
                0
            };
            let i0 = indices[first];
            for k in 1..nv - 1 {
                let (i1, i2) = (indices[first + k], indices[first + k + 1]);
                if (i0 as usize) < verts.len() && (i1 as usize) < verts.len() && (i2 as usize) < verts.len() {
                    tris.push([i0, i1, i2]);
                    tri_geom.push(mat);
                }
            }
            poly_cnt += 1;
        }
    }
    if verts.is_empty() || tris.is_empty() {
        return Err("Empty CNavMesh geometry".into());
    }
    Ok(Mesh3D::from_parts(
        fallback_name.to_string(),
        verts,
        Vec::new(),
        tris,
        tri_geom,
        Vec::new(),
        None,
        format!("CNavMesh ({poly_cnt} polys, Z {:.0}..{:.0}m)", min_v[2], min_v[2] + ext[2]),
        5,
        poly_cnt,
        "NavMesh".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc1_preserves_one_bit_transparency() {
        // c0 <= c1 selects BC1's transparent color index; first pixel uses it.
        let px = decode_linear_to_rgba(&[0, 0, 255, 255, 3, 0, 0, 0], 4, 4, 72).unwrap();
        assert_eq!(px[0], Color32::TRANSPARENT);
        assert_eq!(px[1], Color32::BLACK);
    }

    #[test]
    fn ttpf_mip_layout_places_tail_and_body() {
        // 64x64 BC1 (16x16 elements, 8 bytes each), 6 mips: whole chain fits the 4 KiB tail.
        let l = TtpfMipLayout::new(16, 16, 6, 8, false);
        assert!(l.mips.iter().all(|m| m.tail_origin.is_some()));
        assert_eq!(l.span(0), (0, 4096));
        // 128x128 BC1, 7 mips: tail block first, then mip 0 in two 32x16 blocks.
        let l = TtpfMipLayout::new(32, 32, 7, 8, false);
        assert_eq!(l.mips[0].tail_origin, None);
        assert_eq!(l.span(0), (4096, 12288));
        assert_eq!(l.mips[1].tail_origin, Some((16, 0)));
        assert_eq!(l.mips[1].ew, 16);
        // 512x512 BC7 (128x128 elements, 16x16 per block), 10 mips: mips 0..3 own blocks,
        // stored smallest first after the tail block; mip 4 (8x8 elements) starts the tail.
        let l = TtpfMipLayout::new(128, 128, 10, 16, false);
        assert_eq!(l.span(3), (4096, 2 * 4096));
        assert_eq!(l.span(2), (2 * 4096, 6 * 4096));
        assert_eq!(l.span(1), (6 * 4096, 22 * 4096));
        assert_eq!(l.span(0), (22 * 4096, 86 * 4096));
        assert_eq!(l.mips[3].tail_origin, None);
        assert_eq!(l.mips[4].tail_origin, Some((8, 0)));
    }

    /// Dump root for the data-gated tests (folder holding `data/ttdb`).
    fn dump_root() -> Option<std::path::PathBuf> {
        std::env::var("PERCYSTAR_DUMP").ok().map(std::path::PathBuf::from).filter(|p| p.join("data/ttdb").is_dir())
    }

    #[test]
    #[ignore]
    fn ttpf_mip0_debug() {
        let (Some(root), Ok(picks)) = (dump_root(), std::env::var("PERCYSTAR_PICKS")) else { return };
        let mut files = Vec::new();
        collect_ttpf_files(&root, 0, &mut files);
        for pick in picks.split(';') {
            let (f, i) = pick.split_once(':').unwrap();
            let i: usize = i.parse().unwrap();
            let Some(path) = files.iter().find(|p| p.to_string_lossy().ends_with(f)) else { continue };
            let ix = ttpf_index(path).unwrap();
            let d = &ix.descs[i];
            let (_, is_bc, bpe) = dxgi_info(d.dxgi).unwrap();
            println!("DBG {f}:{i} {}x{} dxgi {} tile {} mips {} off {:#x} alloc {:#x}", d.w, d.h, d.dxgi, d.tile_mode, d.mip_count, d.byte_off, d.alloc_sz);
            for g in ttpf_mip0_guesses(d, is_bc, bpe) {
                let score = (|| {
                    let raw = ttpf_read_virtual(path, &ix, g.off, g.len).ok()?;
                    let lin = match &g.layout { Some(l) => l.untile(&raw, 0), None if g.use_64k => deswizzle_64kb_ttpf(&raw, d.w, d.h, is_bc, bpe), None => deswizzle_4kb_d(&raw, d.w, d.h, is_bc, bpe) };
                    let px0 = decode_linear_to_rgba(&lin, d.w, d.h, d.dxgi)?;
                    let (o1, l1, lay) = g.mip1.as_ref()?;
                    let raw1 = ttpf_read_virtual(path, &ix, *o1, *l1 as usize).ok()?;
                    let s1 = &lay.mips[1];
                    let px1 = decode_linear_to_rgba(&lay.untile(&raw1, 1), s1.ew * 4, s1.eh * 4, d.dxgi)?;
                    mip_pair_error(&px0, d.w, d.h, &px1, s1.ew * 4)
                })();
                println!("   {:13} off+{:#x} len {:#x} 64k {} mip1 {} score {:?}", g.label, g.off - d.byte_off, g.len, g.use_64k, g.mip1.as_ref().map(|m| format!("+{:#x}", m.0 - d.byte_off)).unwrap_or("-".into()), score);
            }
        }
    }

    /// Which mip-0 placement wins across the dump, and how often the result differs from the
    /// original guess. `PERCY_OUT=<dir>` writes old | chosen pairs for a sample of changes.
    ///
    ///   PERCYSTAR_DUMP=<dump root> cargo test --release ttpf_mip0_choice_audit -- --ignored --nocapture
    #[test]
    #[ignore]
    fn ttpf_mip0_choice_audit() {
        let Some(root) = dump_root() else { return };
        let filt = std::env::var("PERCYSTAR_TTPF_FILTER").unwrap_or_default();
        let out = std::env::var("PERCY_OUT").ok();
        let every: usize = std::env::var("PERCY_EVERY").ok().and_then(|v| v.parse().ok()).unwrap_or(997);
        let mut files = Vec::new();
        collect_ttpf_files(&root, 0, &mut files);
        let mut wins = std::collections::BTreeMap::<&str, usize>::new();
        let (mut total, mut multi, mut changed, mut written) = (0usize, 0usize, 0usize, 0usize);
        for path in files.iter().filter(|p| p.to_string_lossy().contains(&filt)) {
            let Ok(ix) = ttpf_index(path) else { continue };
            for d in ix.descs.iter() {
                let Some((_, is_bc, bpe)) = dxgi_info(d.dxgi) else { continue };
                let guesses = ttpf_mip0_guesses(d, is_bc, bpe);
                let Ok((label, (lin, px))) = ttpf_decode_mip0_labeled(path, &ix, d, is_bc, bpe) else { continue };
                total += 1;
                if guesses.len() < 2 {
                    continue;
                }
                multi += 1;
                *wins.entry(label).or_default() += 1;
                // baseline = what v1.4.1 decoded (tail block for tiny textures, else the old guess)
                let Ok((olin, po)) = ttpf_decode_guess(path, &ix, d, is_bc, bpe, &guesses[0]) else { continue };
                if olin == lin {
                    continue;
                }
                changed += 1;
                if let Some(dir) = &out {
                    if written < 24 && changed % every == 1 && d.w >= 128 && d.h >= 128 {
                        let mut img = image::RgbaImage::new((d.w * 2 + 8) as u32, d.h as u32);
                        for y in 0..d.h {
                            for x in 0..d.w {
                                for (k, p) in [&po, &px].into_iter().enumerate() {
                                    let c = p[y * d.w + x];
                                    img.put_pixel((k * (d.w + 8) + x) as u32, y as u32, image::Rgba([c.r(), c.g(), c.b(), 255]));
                                }
                            }
                        }
                        let name = format!("{label}__{}_{}.png", path.file_name().unwrap().to_string_lossy(), d.idx);
                        img.save(std::path::Path::new(dir).join(name)).unwrap();
                        written += 1;
                    }
                }
            }
        }
        println!("AUDIT decoded {total}, ambiguous {multi}, changed vs v1.4.1 baseline {changed}, winners {wins:?}");
    }

    /// Counts BCPack-blocked textures and how many the lower-mip fallback recovers.
    ///
    ///   PERCYSTAR_DUMP=<dump root> cargo test --release ttpf_lower_mip_fallback -- --ignored --nocapture
    ///
    /// `PERCYSTAR_TTPF_ALL=1` measures every `.ttpf` under the dump instead of two stores.
    #[test]
    #[ignore]
    fn ttpf_lower_mip_fallback() {
        let Some(root) = dump_root() else { return };
        let all = std::env::var("PERCYSTAR_TTPF_ALL").is_ok();
        let stores: Vec<std::path::PathBuf> = if all {
            let mut v = Vec::new();
            collect_ttpf_files(&root, 0, &mut v);
            v.sort();
            v
        } else {
            ["data/ttdb/vehicles/ttdb_vehicles.ttpf", "pack/core_restofworld/data/ttdb/props/ttdb_props.ttpf"]
                .iter()
                .map(|r| root.join(r))
                .filter(|p| p.is_file())
                .collect()
        };
        assert!(!stores.is_empty(), "no ttpf stores under {}", root.display());
        let (mut all_blocked, mut all_recovered, mut all_textures, mut flagged_but_stored) = (0usize, 0usize, 0usize, 0usize);
        for path in &stores {
            let ix = ttpf_index(path).expect("index");
            let (mut blocked, mut recovered) = (0usize, 0usize);
            let mut by_mip = [0usize; 16];
            let mut example = String::new();
            for d in &ix.descs {
                let Some((_, is_bc, bpe)) = dxgi_info(d.dxgi) else { continue };
                let (off, len, _) = ttpf_mip0_range(d, is_bc, bpe);
                if !ix.range_hits_bcpack(off, len as u64) {
                    continue;
                }
                blocked += 1;
                if let Ok(t) = decode_ttpf_texture(path, d.idx) {
                    let Some((fw, fh)) = t.full_size else {
                        // A few chunks carry a BCPack codec but are exactly their virtual size;
                        // the reader takes those as stored data, so mip 0 isn't blocked.
                        blocked -= 1;
                        flagged_but_stored += 1;
                        continue;
                    };
                    assert_eq!((fw, fh), (d.w, d.h));
                    assert_eq!(t.pixels.len(), t.w * t.h);
                    assert!(t.w < fw || t.h < fh);
                    recovered += 1;
                    if example.is_empty() {
                        example = format!("e.g. #{} {}x{} of {fw}x{fh}", d.idx, t.w, t.h);
                    }
                    let m = t.name.rsplit("_mip").next().and_then(|s| s.parse::<usize>().ok()).unwrap_or(15);
                    by_mip[m.min(15)] += 1;
                }
            }
            all_blocked += blocked;
            all_recovered += recovered;
            all_textures += ix.descs.len();
            let rel = path.strip_prefix(&root).unwrap_or(path).display();
            let mips: Vec<String> = by_mip.iter().enumerate().filter(|(_, &c)| c > 0).map(|(m, c)| format!("mip{m}={c}")).collect();
            println!("{rel}: {} textures, blocked {blocked}, recovered {recovered} [{}] {example}", ix.descs.len(), mips.join(" "));
        }
        println!(
            "TOTAL: {} stores, {all_textures} textures, blocked {all_blocked}, recovered {all_recovered} ({:.1}%); {flagged_but_stored} BCPack-flagged but stored",
            stores.len(),
            100.0 * all_recovered as f64 / all_blocked.max(1) as f64
        );
        assert!(all_recovered > 0, "fallback recovered nothing");

        // Known case: ttdb_vehicles.ttpf #102 is a 2048x2048 BC texture whose top two mips
        // are BCPack; mip 2 is stored as zlib. `PERCY_OUT=<dir>` also writes it as a PNG.
        let veh = root.join("data/ttdb/vehicles/ttdb_vehicles.ttpf");
        if veh.is_file() {
            let t = decode_ttpf_texture(&veh, 102).expect("vehicles #102 via fallback");
            assert_eq!(t.full_size, Some((2048, 2048)));
            assert_eq!((t.w, t.h), (512, 512));
            assert!(t.name.ends_with("_mip2"), "{}", t.name);
            println!("vehicles #102: {} tile mode {}", t.name, ttpf_index(&veh).unwrap().descs[102].tile_mode);
            if let Ok(out) = std::env::var("PERCY_OUT") {
                let _ = std::fs::create_dir_all(&out);
                export_texture_png(&t, &Path::new(&out).join(format!("{}.png", t.name))).expect("png");
            }
        }
    }

    /// The layout must agree with the existing full-size decoder on unblocked textures, and
    /// its mip 1 must look like its mip 0 shrunk by half.
    ///
    ///   PERCYSTAR_DUMP=<dump root> cargo test --release ttpf_mip_layout_matches_real_data -- --ignored --nocapture
    #[test]
    #[ignore]
    fn ttpf_mip_layout_matches_real_data() {
        let Some(root) = dump_root() else { return };
        let path = root.join("data/ttdb/vehicles/ttdb_vehicles.ttpf");
        if !path.is_file() {
            return;
        }
        let ix = ttpf_index(&path).expect("index");
        let (mut checked, mut mip0_same, mut mip1_close) = (0usize, 0usize, 0usize);
        for d in ix.descs.iter().filter(|d| d.mip_count >= 3 && d.w >= 64 && d.h >= 64) {
            // BC6H decodes to tonemapped floats; skip it to keep the comparison simple.
            let Some((_, true, bpe)) = dxgi_info(d.dxgi) else { continue };
            if matches!(d.dxgi, 95 | 96) {
                continue;
            }
            let Some(l) = ttpf_bc_layout(d, bpe) else { continue };
            let (a, b) = l.span(0);
            if ix.range_hits_bcpack(d.byte_off, b) {
                continue;
            }
            let Ok(full) = decode_ttpf_texture(&path, d.idx) else { continue };
            let Ok(raw0) = ttpf_read_virtual(&path, &ix, d.byte_off + a, (b - a) as usize) else { continue };
            checked += 1;
            let lin0 = l.untile(&raw0, 0);
            if lin0 == full.raw_dds_payload {
                mip0_same += 1;
            } else {
                // Seen only where the main decoder's alloc-end guess for mip 0 is off (it then
                // shows the tail block); the layout's mip 0 looks right in those cases.
                println!("  #{} {}x{} dxgi {} tile {} mips {} alloc {:#x}: mip0 differs from main decoder", d.idx, d.w, d.h, d.dxgi, d.tile_mode, d.mip_count, d.alloc_sz);
            }
            let s0 = &l.mips[0];
            let w0 = s0.ew * 4;
            let px0 = decode_linear_to_rgba(&lin0, w0, s0.eh * 4, d.dxgi).unwrap();
            let (a1, b1) = l.span(1);
            let raw1 = ttpf_read_virtual(&path, &ix, d.byte_off + a1, (b1 - a1) as usize).unwrap();
            let s1 = &l.mips[1];
            let px1 = decode_linear_to_rgba(&l.untile(&raw1, 1), s1.ew * 4, s1.eh * 4, d.dxgi).unwrap();
            // Mean absolute RGB difference between mip 1 and a 2x2 box filter of mip 0.
            let (w1, h1) = (d.w / 2, d.h / 2);
            let mut err = 0u64;
            for y in 0..h1 {
                for x in 0..w1 {
                    let q = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(dx, dy)| px0[(2 * y + dy) * w0 + 2 * x + dx]);
                    let c = px1[y * s1.ew * 4 + x];
                    for (k, v) in [c.r(), c.g(), c.b()].into_iter().enumerate() {
                        let avg = q.iter().map(|p| [p.r(), p.g(), p.b()][k] as i32).sum::<i32>() / 4;
                        err += (avg - v as i32).unsigned_abs() as u64;
                    }
                }
            }
            let mean_err = err as f64 / (3 * w1 * h1) as f64;
            if mean_err < 12.0 {
                mip1_close += 1;
            } else {
                println!("  #{} {}x{} dxgi {} tile {} mips {}: mip1 mean error {mean_err:.1}", d.idx, d.w, d.h, d.dxgi, d.tile_mode, d.mip_count);
            }
            if checked >= 200 {
                break;
            }
        }
        println!("layout check: {checked} textures, mip0 identical {mip0_same}, mip1 close to downsampled mip0 {mip1_close}");
        assert!(checked >= 20, "too few textures checked");
        assert!(mip0_same * 100 >= checked * 95, "layout disagrees with the main decoder too often");
        assert!(mip1_close * 100 >= checked * 98, "mip 1 does not look like mip 0 shrunk");
    }
}
