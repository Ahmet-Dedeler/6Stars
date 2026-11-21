"""decode adat v1 awc audio into playable wav streams"""
import glob
import os
import struct
import subprocess
import sys


def crc32_ogg(data: bytes) -> int:
    crc = 0
    for byte in data:
        crc ^= byte << 24
        for _ in range(8):
            crc = ((crc << 1) ^ 0x04C11DB7) & 0xFFFFFFFF if (crc & 0x80000000) else (crc << 1) & 0xFFFFFFFF
    return crc


def ogg_page(serial: int, seq: int, header_type: int, granule: int, packets: list[bytes]) -> bytes:
    seg_table = bytearray()
    body = bytearray()
    for p in packets:
        rem = len(p)
        while rem >= 255:
            seg_table.append(255)
            rem -= 255
        seg_table.append(rem)
        body.extend(p)
    hdr = struct.pack(
        "<4sBBqIIIB",
        b"OggS", 0, header_type, granule, serial, seq, 0, len(seg_table),
    ) + seg_table
    full = bytearray(hdr + body)
    crc = crc32_ogg(full)
    struct.pack_into("<I", full, 22, crc)
    return bytes(full)


def extract_awc(awc_path: str, out_dir: str) -> list[str]:
    with open(awc_path, "rb") as f:
        data = f.read()
    if len(data) < 16 or data[:4] != b"ADAT":
        return []
    ver, flags, sc, hdr_sz = struct.unpack_from("<HHII", data, 4)
    if sc == 0 or sc > 4096:
        return []

    chunk_table_start = 16 + sc * 4
    off = chunk_table_start
    data_chunks = []
    # unpack 64bit tag size offset entries lookign for data chunks 0x55
    while off + 8 <= len(data) and off < hdr_sz:
        val = struct.unpack_from("<Q", data, off)[0]
        tag = val >> 56
        sz = (val >> 28) & 0x0FFFFFFF
        coff = val & 0x0FFFFFFF
        if tag == 0x55 and 0 < sz <= len(data) - coff and coff >= 0x20:
            data_chunks.append((coff, sz))
        if tag == 0 and coff == 0:
            break
        off += 8

    base = os.path.splitext(os.path.basename(awc_path))[0]
    os.makedirs(out_dir, exist_ok=True)
    out_wavs = []
    for idx, (coff, csz) in enumerate(data_chunks):
        blob = data[coff : coff + csz]
        packets = []
        # rage opus streams split into 2048 byte blocks of u16 prefixed frams
        for bpos in range(0, len(blob), 2048):
            blk = blob[bpos : bpos + 2048]
            p = 0
            while p + 2 <= len(blk):
                plen = struct.unpack_from("<H", blk, p)[0]
                if plen == 0 or p + 2 + plen > len(blk):
                    break
                packets.append(blk[p + 2 : p + 2 + plen])
                p += 2 + plen
        if not packets:
            continue

        ogg_p = os.path.join(out_dir, f"{base}_s{idx:02d}.opus")
        wav_p = os.path.join(out_dir, f"{base}_s{idx:02d}.wav")
        serial = 0x50530001 + idx
        opus_head = struct.pack("<8sBBHIhB", b"OpusHead", 1, 1, 0, 48000, 0, 0)
        opus_tags = b"OpusTags" + struct.pack("<I", 6) + b"6Stars" + struct.pack("<I", 0)
        ogg = bytearray()
        ogg.extend(ogg_page(serial, 0, 2, 0, [opus_head]))
        ogg.extend(ogg_page(serial, 1, 0, 0, [opus_tags]))
        granule = 0
        seq = 2
        for i in range(0, len(packets), 50):
            batch = packets[i : i + 50]
            granule += len(batch) * 960
            ht = 4 if (i + 50 >= len(packets)) else 0
            ogg.extend(ogg_page(serial, seq, ht, granule, batch))
            seq += 1
        with open(ogg_p, "wb") as wf:
            wf.write(ogg)
        subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", ogg_p, wav_p], check=False)
        try:
            os.remove(ogg_p)
        except OSError:
            pass
        if os.path.exists(wav_p) and os.path.getsize(wav_p) > 44:
            out_wavs.append(wav_p)
    return out_wavs


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print("Usage: python awc_extract.py <input_awc_dir> <output_wav_dir>")
        sys.exit(1)
    in_dir, out_dir = sys.argv[1], sys.argv[2]
    total = 0
    for p in sorted(glob.glob(os.path.join(in_dir, "*.awc"))):
        total += len(extract_awc(p, out_dir))
    print(f"Done: {total} playable .wav files extracted to {out_dir}")
