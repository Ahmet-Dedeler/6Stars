// SPDX-License-Identifier: MIT
// Copyright (c) 2026 paryzius
//
// Ported from paryzius/ttpf-viewer (`src/tile.rs` and the mip placement in `src/archive.rs`),
// https://github.com/paryzius/ttpf-viewer, under the MIT License:
//
// Permission is hereby granted, free of charge, to any person obtaining a copy of this software
// and associated documentation files (the "Software"), to deal in the Software without
// restriction, including without limitation the rights to use, copy, modify, merge, publish,
// distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all copies or
// substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING
// BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
// NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
// DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

//! Bit-pattern untiling for TTPF textures (AMD standard swizzle), from ttpf-viewer.
//!
//! A pattern lists, from the lowest bit of an element's index inside a tile up, whether that
//! bit moves the element along x or y. Elements are 4x4 blocks for BCn and single pixels for
//! uncompressed formats; tiles are laid out row-major.
//!
//! `tex.rs` uses this as one more candidate placement for mip 0 (and mip 1, for scoring).
//! It untiles small uncompressed textures (UI icons) and some short BCn chains that the
//! Gfx10 layout in `tex.rs` scrambles, while `tex.rs` keeps the cases this layout misses.

/// Tile pattern for `bpe`-byte elements; `None` when it isn't known for that size.
/// Each was checked against decoded textures by ttpf-viewer's author.
pub fn pattern(bpe: usize, use_64k: bool) -> Option<&'static str> {
    Some(match (use_64k, bpe) {
        (true, 16) => "xyxyyxyxyxyx",
        (false, 16) => "yyxxyxyx",
        (true, 8) => "xyxxyyxyxyxyx",
        (false, 8) => "xyyxxyxyx",
        (true, 4) => "xxyyxyyxyxyxyx",
        (false, 4) => "xxyyyxyxyx",
        (true, 2) => "xxxyyyxyxyxyxyx",
        (false, 2) => "xxxyyyxyxyx",
        (true, 1) => "xxxyyyxyyxyxyxyx",
        (false, 1) => "xxxxyyyyyxyx",
        _ => return None,
    })
}

/// Tile size in elements (width, height) for a full pattern.
pub fn tile_elems(pattern: &str) -> (usize, usize) {
    let xs = pattern.bytes().filter(|&c| c == b'x').count();
    (1 << xs, 1 << (pattern.len() - xs))
}

/// One mip level placed the way ttpf-viewer places it: levels are stored smallest first
/// and each is padded to whole tiles, so mip 0 ends where the allocation ends and every
/// level ends where the next larger one starts.
#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    /// Padded size in elements.
    pub ew: usize,
    pub eh: usize,
    /// Byte range from the end of the allocation: the level is at `[end - from_end, end - from_end + len)`.
    pub from_end: u64,
    pub len: u64,
}

/// Mip 0 and, when it is tile-aligned too, mip 1. Smaller levels may share a tile, so they
/// are not placed. `ew`/`eh` are mip 0's size in elements (blocks for BCn).
pub fn levels(ew: usize, eh: usize, bpe: usize, use_64k: bool, mip_count: usize) -> Vec<Level> {
    let Some(p) = pattern(bpe, use_64k) else { return Vec::new() };
    let (tw, th) = tile_elems(p);
    let mut out = Vec::new();
    let mut from_end = 0u64;
    for k in 0..mip_count.clamp(1, 2) {
        let (lw, lh) = ((ew >> k).max(1), (eh >> k).max(1));
        let (pw, ph) = (lw.div_ceil(tw) * tw, lh.div_ceil(th) * th);
        if k > 0 && (pw, ph) != (lw, lh) {
            break;
        }
        let len = (pw * ph * bpe) as u64;
        from_end += len;
        out.push(Level { ew: pw, eh: ph, from_end, len });
    }
    out
}

fn log2(n: usize) -> usize {
    (usize::BITS - 1 - n.max(1).leading_zeros()) as usize
}

/// Rearrange tiled elements into a linear `w` x `h` element grid. Surfaces smaller than a
/// tile use only the low bits of the pattern. Elements that land outside are dropped and
/// cells nobody fills stay zero.
pub fn untile(data: &[u8], bpe: usize, w: usize, h: usize, pattern: &str) -> Vec<u8> {
    let n = data.len() / bpe;
    let t = &pattern.as_bytes()[..pattern.len().min(log2(n))];
    let tile_len = 1usize << t.len();
    let mut lx = vec![0usize; tile_len];
    let mut ly = vec![0usize; tile_len];
    for j in 0..tile_len {
        let (mut x, mut y, mut xi, mut yi) = (0, 0, 0, 0);
        for (b, &c) in t.iter().enumerate() {
            let bit = (j >> b) & 1;
            if c == b'x' {
                x |= bit << xi;
                xi += 1;
            } else {
                y |= bit << yi;
                yi += 1;
            }
        }
        lx[j] = x;
        ly[j] = y;
    }
    let xs = t.iter().filter(|&&c| c == b'x').count();
    let (tw, th) = (1usize << xs, 1usize << (t.len() - xs));
    let tiles_per_row = w.div_ceil(tw).max(1);
    let mask = tile_len - 1;
    let mut out = vec![0u8; w * h * bpe];
    for i in 0..n {
        let tile = i >> t.len();
        let x = (tile % tiles_per_row) * tw + lx[i & mask];
        let y = (tile / tiles_per_row) * th + ly[i & mask];
        if x < w && y < h {
            let d = (y * w + x) * bpe;
            out[d..d + bpe].copy_from_slice(&data[i * bpe..(i + 1) * bpe]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untile_is_a_permutation_for_every_pattern() {
        for bpe in [1, 2, 4, 8, 16] {
            for use_64k in [false, true] {
                let p = pattern(bpe, use_64k).unwrap();
                let (tw, th) = tile_elems(p);
                assert_eq!(tw * th * bpe, if use_64k { 65536 } else { 4096 }, "{bpe} {use_64k}");
                // Two tiles wide, one high. Untile element indices (u32, bpe = 4) with this
                // pattern and check every index lands exactly once.
                let (w, h) = (tw * 2, th);
                let data: Vec<u8> = (0..(w * h) as u32).flat_map(u32::to_le_bytes).collect();
                let lin = untile(&data, 4, w, h, p);
                let mut seen = vec![false; w * h];
                for e in lin.chunks_exact(4) {
                    let i = u32::from_le_bytes(e.try_into().unwrap()) as usize;
                    assert!(!seen[i], "{bpe} {use_64k}: element {i} placed twice");
                    seen[i] = true;
                }
            }
        }
    }

    #[test]
    fn levels_stack_from_the_end() {
        // 256x256 BC7 (64x64 blocks) in 4 KB tiles (16x16 blocks): mip 1 is 32x32 blocks.
        let l = levels(64, 64, 16, false, 9);
        assert_eq!(l.len(), 2);
        assert_eq!((l[0].ew, l[0].eh, l[0].len, l[0].from_end), (64, 64, 65536, 65536));
        assert_eq!((l[1].ew, l[1].eh, l[1].len, l[1].from_end), (32, 32, 16384, 81920));
        // a 32x32 RGBA8 icon is padded to one 32x32 pixel tile; mip 1 would share a tile
        let s = levels(32, 32, 4, false, 6);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].len, 4096);
    }
}
