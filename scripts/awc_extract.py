"""GTA 6 ADAT (.awc) v1 audio extractor to playable .wav via Ogg Opus + ffmpeg."""
import glob
import os
import struct
import subprocess
import sys


def ogg_crc(page: bytes) -> int:
    crc = 0
    for b in page:
        crc ^= b << 24
        for _ in range(8):
            crc = ((crc << 1) ^ 0x04C11DB7) & 0xFFFFFFFF if (crc & 0x80000000) else (crc << 1) & 0xFFFFFFFF
    return crc


def make_ogg_page(serial: int, seq: int, granule: int, flags: int, packets: list[bytes]) -> bytes:
    segs = []
    body = bytearray()
    for p in packets:
        n = len(p)
        while n >= 255:
            segs.append(255)
            n -= 255
        segs.append(n)
        body.extend(p)
    hdr = bytearray(struct.pack("<4sBBqIIIB", b"OggS", 0, flags, granule, serial, seq, 0, len(segs)))
    hdr.extend(segs)
    hdr.extend(body)
    struct.pack_into("<I", hdr, 22, ogg_crc(hdr))
    return bytes(hdr)


def write_opus_ogg(packets: list[bytes], out_opus: str, rate: int = 48000, channels: int = 1) -> None:
    serial = 0x12345678
    opus_head = struct.pack("<8sBBHIhB", b"OpusHead", 1, channels, 312, rate, 0, 0)
    opus_tags = struct.pack("<8sI", b"OpusTags", 8) + b"gta6-awc" + struct.pack("<I", 0)
    with open(out_opus, "wb") as f:
        f.write(make_ogg_page(serial, 0, 0, 2, [opus_head]))
        f.write(make_ogg_page(serial, 1, 0, 0, [opus_tags]))
        granule, seq = 0, 2
        for i in range(0, len(packets), 50):
            batch = packets[i : i + 50]
            granule += len(batch) * 960
            flag = 4 if i + 50 >= len(packets) else 0
            f.write(make_ogg_page(serial, seq, granule, flag, batch))
            seq += 1


def parse_awc(data: bytes):
    if len(data) < 16 or data[:4] != b"ADAT":
        return []
    _, ver, flags, scount, hsz = struct.unpack_from("<4sHHII", data, 0)
    if hsz > len(data) or scount == 0:
        return []
    pos = 0x10
    if flags & 1:
        pos += scount * 2
    stream_ids = struct.unpack_from(f"<{scount}I", data, pos)
    pos += scount * 4
    # Align chunk table to end of header (hsz - total_chunks * 8)
    rem = hsz - pos
    cbase = pos + (rem % 8)
    total_chunks = (hsz - cbase) // 8
    chunks = [struct.unpack_from("<Q", data, cbase + i * 8)[0] for i in range(total_chunks)]
    if flags & 1:
        idxs = list(struct.unpack_from(f"<{scount}H", data, 0x10)) + [total_chunks]
    else:
        step = total_chunks // scount
        idxs = [i * step for i in range(scount + 1)]
    streams = []
    for s in range(scount):
        s_chunks = []
        for c in range(idxs[s], min(idxs[s + 1], total_chunks)):
            val = chunks[c]
            s_chunks.append((val >> 56, (val >> 28) & 0x0FFFFFFF, val & 0x0FFFFFFF))
        streams.append((stream_ids[s], s_chunks))
    return streams


def extract_opus_packets(data: bytes, off: int, sz: int) -> list[bytes]:
    pkts = []
    end = min(off + sz, len(data))
    for blk in range(off, end, 0x800):
        pos = blk
        blk_end = min(blk + 0x800, end)
        while pos + 2 <= blk_end:
            plen = struct.unpack_from("<H", data, pos)[0]
            if plen == 0 or plen > 1500 or pos + 2 + plen > blk_end:
                break
            pkts.append(data[pos + 2 : pos + 2 + plen])
            pos += 2 + plen
    return pkts


def main():
    if len(sys.argv) < 3:
        print("Usage: python awc_extract.py <input_awc_dir> <output_wav_dir>")
        sys.exit(1)
    in_dir = sys.argv[1]
    out_dir = sys.argv[2]
    os.makedirs(out_dir, exist_ok=True)
    exported = 0
    for awc_path in sorted(glob.glob(os.path.join(in_dir, "*.awc"))):
        base_name = os.path.splitext(os.path.basename(awc_path))[0]
        with open(awc_path, "rb") as f:
            data = f.read()
        for sid, s_chunks in parse_awc(data):
            data_chunks = [c for c in s_chunks if c[0] == 0x55]
            if not data_chunks:
                continue
            _, sz, off = data_chunks[0]
            if off + sz > len(data):
                continue
            pkts = extract_opus_packets(data, off, sz)
            if len(pkts) < 2:
                continue
            ch = 2 if (pkts[0][0] & 0x04) else 1
            ogg_p = os.path.join(out_dir, f"{base_name}_0x{sid:08x}.opus")
            wav_p = os.path.join(out_dir, f"{base_name}_0x{sid:08x}.wav")
            write_opus_ogg(pkts, ogg_p, 48000, ch)
            subprocess.run(["ffmpeg", "-y", "-hide_banner", "-loglevel", "error", "-i", ogg_p, wav_p], check=True)
            os.remove(ogg_p)
            exported += 1
    print(f"Exported {exported} playable .wav files -> {out_dir}")


if __name__ == "__main__":
    main()
