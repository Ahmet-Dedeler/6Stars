"""GTA 6 RPF9 (9FPR) unencrypted payload carver (zlib, ADAT .awc, KB2 .bk2)."""
import os
import re
import struct
import sys
import zlib


def carve_rpf(rpf_path: str, out_dir: str, max_bytes: int | None = None) -> dict[str, int]:
    os.makedirs(out_dir, exist_ok=True)
    sz = os.path.getsize(rpf_path)
    limit = min(sz, max_bytes) if max_bytes else sz
    counts = {"bk2": 0, "awc": 0, "zlib": 0}

    with open(rpf_path, "rb") as f:
        hdr = f.read(64)
        magic = hdr[:4]
        print(f"RPF: {os.path.basename(rpf_path)} | size={sz:,} | magic={magic!r} | hdr[4:16]={hdr[4:16].hex()}")
        if magic != b"9FPR":
            print("  Warning: not 9FPR (incomplete or zeroed header); scanning anyway...")

        # Pass 1: Page-aligned (0x1000) ADAT & KB2 headers in 16MB chunks
        awc_offsets = []
        bk2_ranges = []
        f.seek(0)
        off = 0
        CHUNK = 16 * 1024 * 1024
        while off < limit:
            buf = f.read(min(CHUNK, limit - off))
            if not buf:
                break
            for p in range(0, len(buf) - 44, 0x1000):
                tag = buf[p : p + 4]
                if tag == b"ADAT":
                    awc_offsets.append(off + p)
                elif tag[:3] == b"KB2" and 0x61 <= tag[3] <= 0x7A:
                    _, fsize, frames, _, frames2, w, h, fn, fd, _, _ = struct.unpack_from("<4sIIIIIIIIII", buf, p)
                    total = fsize + 8
                    if 1 <= frames <= 500000 and frames == frames2 and 64 <= w <= 8192 and 64 <= h <= 8192 and 1024 <= total <= sz - (off + p):
                        bk2_ranges.append((off + p, total, w, h, frames))
            off += len(buf)

        # Extract KB2
        if bk2_ranges:
            bdir = os.path.join(out_dir, "bk2")
            os.makedirs(bdir, exist_ok=True)
            for idx, (boff, bsz, w, h, frames) in enumerate(bk2_ranges, 1):
                f.seek(boff)
                out_p = os.path.join(bdir, f"movie_{idx:03d}_{w}x{h}_{frames}f.bk2")
                rem = bsz
                with open(out_p, "wb") as wf:
                    while rem > 0:
                        c = f.read(min(rem, 4 * 1024 * 1024))
                        if not c:
                            break
                        wf.write(c)
                        rem -= len(c)
                counts["bk2"] += 1

        # Extract ADAT (.awc) using headerSize + chunk extents
        if awc_offsets:
            adir = os.path.join(out_dir, "awc")
            os.makedirs(adir, exist_ok=True)
            for idx, aoff in enumerate(awc_offsets, 1):
                next_off = awc_offsets[idx] if idx < len(awc_offsets) else limit
                read_sz = min(next_off - aoff, 32 * 1024 * 1024)
                f.seek(aoff)
                chunk = f.read(read_sz)
                out_p = os.path.join(adir, f"sound_{idx:04d}_0x{aoff:08x}.awc")
                with open(out_p, "wb") as wf:
                    wf.write(chunk)
                counts["awc"] += 1

        # Pass 2: Zlib streams (only if file <= 512MB or max_bytes set, to keep fast)
        if limit <= 512 * 1024 * 1024:
            zdir = os.path.join(out_dir, "zlib")
            os.makedirs(zdir, exist_ok=True)
            f.seek(0)
            data = f.read(limit)
            i = 0x40
            while i < len(data) - 8:
                if data[i] == 0x78 and data[i + 1] in (0x01, 0x5E, 0x9C, 0xDA):
                    d = zlib.decompressobj()
                    try:
                        out = d.decompress(data[i : i + 1024 * 1024]) + d.flush()
                        if d.eof and len(out) >= 32:
                            comp_len = min(len(data) - i, 1024 * 1024) - len(d.unused_data)
                            counts["zlib"] += 1
                            head = out[:64].lstrip(b"\xef\xbb\xbf\r\n\t ")
                            if head.startswith(b"<?xml") or head.startswith(b"<"):
                                ext = ".xml"
                            elif out[:4] == b"PSIN":
                                ext = ".psin"
                            elif all(32 <= b < 127 or b in (9, 10, 13) for b in out[:128]):
                                ext = ".txt"
                            else:
                                ext = ".bin"
                            with open(os.path.join(zdir, f"z_{counts['zlib']:04d}_0x{i:08x}{ext}"), "wb") as wf:
                                wf.write(out)
                            i += max(comp_len, 2)
                            continue
                    except Exception:
                        pass
                i += 1

    print(f"  Extracted -> bk2={counts['bk2']}, awc={counts['awc']}, zlib={counts['zlib']} in {out_dir}")
    return counts


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print("Usage: python rpf9_carve.py <input.rpf> <out_dir> [max_mb]")
        sys.exit(1)
    max_b = int(sys.argv[3]) * 1024 * 1024 if len(sys.argv) > 3 else None
    carve_rpf(sys.argv[1], sys.argv[2], max_b)
