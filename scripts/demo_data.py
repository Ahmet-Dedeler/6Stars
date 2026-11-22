# Percy6Stars by Ahmet Dedeler (https://github.com/Ahmet-Dedeler, https://ahmetdedeler.com)
# Fork of PercyStar by rpatty-dev (https://github.com/rpatty-dev/PercyStar).
# Source: https://github.com/Ahmet-Dedeler/Percy6Stars
# License: PercyStar Non-Commercial Attribution & Copyleft License v1.2 (see LICENSE)
# If you copy, vendor or port this file, keep this header and credit both projects in your README.
#
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy", "pillow"]
# ///
"""Generates the made-up sample set used for the README demos.

Everything here is original and procedural: the "Sunset GT" coupe, a lighthouse, a palm tree,
a small city block, and a handful of textures (neon sign, poster, livery decals, road tile).
No game files are read or included. The README recorder (`src/demo_capture.rs`) loads this
folder and shows it through the real Percy6Stars UI.

    uv run scripts/demo_data.py <out-dir>

Models are written as Z-up `.obj` files with `vt` coordinates plus one atlas `.png` each.
"""

from __future__ import annotations

import math
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

RNG = np.random.default_rng(6)
FONTS = Path("/System/Library/Fonts/Supplemental")


def font(name: str, size: int) -> ImageFont.FreeTypeFont:
    for cand in (FONTS / name, Path("/System/Library/Fonts") / name):
        if cand.exists():
            return ImageFont.truetype(str(cand), size)
    return ImageFont.load_default(size)


# ----------------------------------------------------------------------------------------
# mesh building
# ----------------------------------------------------------------------------------------


class Mesh:
    def __init__(self) -> None:
        self.v: list[tuple[float, float, float]] = []
        self.vt: list[tuple[float, float]] = []
        self.f: list[tuple[int, int, int]] = []

    def vert(self, p, uv) -> int:
        self.v.append((float(p[0]), float(p[1]), float(p[2])))
        self.vt.append((float(uv[0]), float(uv[1])))
        return len(self.v) - 1

    def tri(self, a, b, c) -> None:
        self.f.append((a, b, c))

    def grid(self, pts, uvs, close_u=False, flip=False) -> None:
        """Quads over a (rows x cols) grid of points; rows run along the sweep."""
        rows, cols = len(pts), len(pts[0])
        idx = [[self.vert(pts[r][c], uvs[r][c]) for c in range(cols)] for r in range(rows)]
        for r in range(rows - 1):
            for c in range(cols - 1):
                a, b, cc, d = idx[r][c], idx[r][c + 1], idx[r + 1][c + 1], idx[r + 1][c]
                if flip:
                    self.tri(a, cc, b)
                    self.tri(a, d, cc)
                else:
                    self.tri(a, b, cc)
                    self.tri(a, cc, d)

    def fan(self, center, ring, uv_center, uv_ring, flip=False) -> None:
        c = self.vert(center, uv_center)
        ids = [self.vert(p, uv) for p, uv in zip(ring, uv_ring)]
        for i in range(len(ids) - 1):
            if flip:
                self.tri(c, ids[i + 1], ids[i])
            else:
                self.tri(c, ids[i], ids[i + 1])

    def box(self, lo, hi, uv_rect, uv_scale=None) -> None:
        """Axis-aligned box; every face maps to `uv_rect` (u0, v0, u1, v1)."""
        (x0, y0, z0), (x1, y1, z1) = lo, hi
        u0, v0, u1, v1 = uv_rect
        faces = [
            [(x0, y0, z0), (x1, y0, z0), (x1, y0, z1), (x0, y0, z1)],
            [(x1, y1, z0), (x0, y1, z0), (x0, y1, z1), (x1, y1, z1)],
            [(x1, y0, z0), (x1, y1, z0), (x1, y1, z1), (x1, y0, z1)],
            [(x0, y1, z0), (x0, y0, z0), (x0, y0, z1), (x0, y1, z1)],
            [(x0, y0, z1), (x1, y0, z1), (x1, y1, z1), (x0, y1, z1)],
            [(x0, y1, z0), (x1, y1, z0), (x1, y0, z0), (x0, y0, z0)],
        ]
        for q in faces:
            sv = 1.0
            if uv_scale is not None:
                sv = uv_scale
            uvq = [(u0, v1), (u1, v1), (u1, v1 - (v1 - v0) * sv), (u0, v1 - (v1 - v0) * sv)]
            ids = [self.vert(p, uv) for p, uv in zip(q, uvq)]
            self.tri(ids[0], ids[1], ids[2])
            self.tri(ids[0], ids[2], ids[3])

    def merge(self, other: "Mesh") -> None:
        base = len(self.v)
        self.v += other.v
        self.vt += other.vt
        self.f += [(a + base, b + base, c + base) for a, b, c in other.f]

    def transform(self, fn) -> "Mesh":
        self.v = [tuple(fn(np.array(p))) for p in self.v]
        return self

    def save(self, path: Path, name: str) -> None:
        with open(path, "w") as fh:
            fh.write(f"# {name}: original procedural model for the Percy6Stars README demos (Z-up)\n")
            for p in self.v:
                fh.write(f"v {p[0]:.5f} {p[1]:.5f} {p[2]:.5f}\n")
            for uv in self.vt:
                fh.write(f"vt {uv[0]:.5f} {uv[1]:.5f}\n")
            for a, b, c in self.f:
                fh.write(f"f {a + 1}/{a + 1} {b + 1}/{b + 1} {c + 1}/{c + 1}\n")
        print(f"  {path.name}: {len(self.v)} verts, {len(self.f)} tris")


def region(rect, size=1024):
    """Pixel rect -> uv rect (u0, v0, u1, v1) with v=0 at the top of the image."""
    x0, y0, x1, y1 = rect
    return (x0 / size, y0 / size, x1 / size, y1 / size)


def lerp(a, b, t):
    return a + (b - a) * t


def superellipse(theta, a, b, n=3.2):
    c, s = math.cos(theta), math.sin(theta)
    x = a * math.copysign(abs(c) ** (2 / n), c)
    y = b * math.copysign(abs(s) ** (2 / n), s)
    return x, y


def revolve(mesh: Mesh, profile, segs, uv_rect, axis="z", center=(0, 0, 0), u_repeat=1.0):
    """Revolves (radius, height) points around an axis. u follows the angle, v the profile."""
    u0, v0, u1, v1 = uv_rect
    pts, uvs = [], []
    n = len(profile)
    for i, (r, h) in enumerate(profile):
        row, rowuv = [], []
        for k in range(segs + 1):
            a = 2 * math.pi * k / segs
            if axis == "z":
                p = (center[0] + r * math.cos(a), center[1] + r * math.sin(a), center[2] + h)
            else:  # around y
                p = (center[0] + r * math.cos(a), center[1] + h, center[2] + r * math.sin(a))
            row.append(p)
            rowuv.append((lerp(u0, u1, (k / segs * u_repeat) % 1.0001), lerp(v1, v0, i / max(n - 1, 1))))
        pts.append(row)
        uvs.append(rowuv)
    mesh.grid(pts, uvs)


# ----------------------------------------------------------------------------------------
# Sunset GT coupe
# ----------------------------------------------------------------------------------------


def car_atlas(path: Path) -> None:
    S = 1024
    im = Image.new("RGB", (S, S), (20, 20, 24))
    d = ImageDraw.Draw(im)
    # body paint (0,0)-(1024,512): magenta -> orange along the length, darker toward the sills
    paint = np.zeros((512, S, 3), np.float32)
    u = np.linspace(0, 1, S)[None, :, None]
    v = np.linspace(0, 1, 512)[:, None, None]
    c0 = np.array([214, 38, 120], np.float32)
    c1 = np.array([255, 128, 40], np.float32)
    paint[:] = c0 * (1 - u) + c1 * u
    # v runs around the section: 0 bottom, .25 side A, .5 top, .75 side B, 1 bottom
    shade = 0.55 + 0.45 * np.clip(1 - np.abs(v - 0.5) * 2.2, 0, 1) ** 0.6
    paint *= shade
    im.paste(Image.fromarray(paint.clip(0, 255).astype(np.uint8)), (0, 0))
    # twin racing stripes over the top
    for off in (-22, 22):
        d.rectangle([0, 256 + off - 9, S, 256 + off + 9], fill=(250, 246, 236))
    # side stripe + roundel number on both flanks
    for vc, mirror in ((128, False), (384, True)):  # mirror=True is side B
        d.rectangle([60, vc + 34, S - 60, vc + 42], fill=(250, 210, 90))
        badge = Image.new("RGBA", (150, 150), (0, 0, 0, 0))
        bd = ImageDraw.Draw(badge)
        bd.ellipse([4, 4, 146, 146], fill=(250, 246, 236), outline=(30, 20, 40), width=6)
        bd.text((75, 78), "06", fill=(30, 20, 40), font=font("Futura.ttc", 84), anchor="mm")
        # side A runs bottom-up and rear-to-front as seen from outside, so it needs a half turn
        if not mirror:
            badge = badge.rotate(180)
        im.paste(badge, (520, vc - 75), badge)
        # door seam
        d.line([(430, vc - 60), (430, vc + 70)], fill=(70, 20, 50), width=3)
        d.line([(690, vc - 60), (690, vc + 70)], fill=(70, 20, 50), width=3)
    # sills
    d.rectangle([0, 0, S, 20], fill=(30, 26, 34))
    d.rectangle([0, 492, S, 512], fill=(30, 26, 34))
    # glass (0,512)-(512,768): roof band paint on top, tinted glass below
    g = np.zeros((256, 512, 3), np.float32)
    gv = np.linspace(0, 1, 256)[:, None, None]
    g[:] = np.array([16, 26, 46]) * (1 - gv) + np.array([60, 96, 150]) * gv
    gi = Image.fromarray(g.astype(np.uint8))
    gd = ImageDraw.Draw(gi)
    for x in range(-200, 512, 140):
        gd.polygon([(x, 256), (x + 40, 256), (x + 170, 0), (x + 130, 0)], fill=(90, 130, 190))
    gi = gi.filter(ImageFilter.GaussianBlur(6))
    im.paste(gi, (0, 512))
    d.rectangle([0, 512, 512, 560], fill=(205, 60, 95))  # roof paint
    d.rectangle([0, 560, 512, 566], fill=(20, 16, 24))  # rubber seal
    # rim (512,512)-(768,768)
    cx, cy, r = 640, 640, 124
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(26, 26, 30))
    d.ellipse([cx - r + 14, cy - r + 14, cx + r - 14, cy + r - 14], fill=(200, 204, 214))
    d.ellipse([cx - r + 26, cy - r + 26, cx + r - 26, cy + r - 26], fill=(40, 40, 48))
    for k in range(5):
        a = k * 2 * math.pi / 5
        pts = []
        for da, rr in ((-0.16, 22), (0.16, 22), (0.09, 98), (-0.09, 98)):
            pts.append((cx + rr * math.cos(a + da), cy + rr * math.sin(a + da)))
        d.polygon(pts, fill=(222, 226, 236))
    d.ellipse([cx - 26, cy - 26, cx + 26, cy + 26], fill=(240, 90, 140))
    d.ellipse([cx - 8, cy - 8, cx + 8, cy + 8], fill=(30, 30, 36))
    # tire (768,512)-(1024,768)
    d.rectangle([768, 512, 1024, 768], fill=(28, 28, 32))
    for y in range(512, 768, 16):
        d.line([(768, y), (1024, y + 8)], fill=(40, 40, 46), width=4)
    # headlight (0,768)-(256,896), taillight (256,768)-(512,896), grille (512,768)-(768,896)
    hl = Image.new("RGB", (256, 128), (230, 236, 255))
    hd = ImageDraw.Draw(hl)
    hd.rounded_rectangle([10, 30, 246, 98], 30, fill=(255, 255, 255), outline=(150, 170, 220), width=6)
    im.paste(hl.filter(ImageFilter.GaussianBlur(2)), (0, 768))
    tl = Image.new("RGB", (256, 128), (90, 0, 10))
    td = ImageDraw.Draw(tl)
    for x in range(14, 250, 34):
        td.rectangle([x, 36, x + 24, 92], fill=(255, 40, 60))
    im.paste(tl.filter(ImageFilter.GaussianBlur(3)), (256, 768))
    d.rectangle([512, 768, 768, 896], fill=(16, 16, 20))
    for x in range(520, 768, 18):
        d.line([(x, 776), (x, 888)], fill=(60, 60, 70), width=4)
    # chrome (768,768)-(1024,896) and underside (0,896)-(1024,1024)
    ch = np.linspace(255, 120, 128)[:, None].repeat(256, 1)
    im.paste(Image.fromarray(ch.astype(np.uint8)).convert("RGB"), (768, 768))
    d.rectangle([0, 896, 1024, 1024], fill=(24, 22, 28))
    # spoiler paint (paint the underside rect's right half with livery orange)
    d.rectangle([768, 896, 1024, 1024], fill=(250, 120, 50))
    im.save(path)


def build_car() -> Mesh:
    m = Mesh()
    L = 2.2
    # body stations: x, half width, z bottom, z top
    st = [
        (-2.20, 0.78, 0.36, 0.78),
        (-2.12, 0.88, 0.28, 0.86),
        (-1.90, 0.93, 0.24, 0.90),
        (-1.40, 0.95, 0.22, 0.90),
        (-0.60, 0.94, 0.22, 0.88),
        (0.40, 0.94, 0.22, 0.86),
        (1.10, 0.93, 0.22, 0.80),
        (1.70, 0.90, 0.24, 0.72),
        (2.05, 0.84, 0.28, 0.62),
        (2.20, 0.74, 0.34, 0.52),
    ]
    segs = 28
    pts, uvs = [], []
    for x, hw, zb, zt in st:
        row, rowuv = [], []
        zc, hz = (zb + zt) / 2, (zt - zb) / 2
        for k in range(segs + 1):
            t = k / segs
            th = -math.pi / 2 + t * 2 * math.pi  # start at bottom, side A (+y), top, side B
            yy, zz = superellipse(th, hw, hz, 4.5)
            row.append((x, yy, zc + zz))
            rowuv.append(((x + L) / (2 * L) * 0.98 + 0.01, t * 0.5))
        pts.append(row)
        uvs.append(rowuv)
    m.grid(pts, uvs)
    # end caps
    for x, hw, zb, zt in (st[0], st[-1]):
        zc, hz = (zb + zt) / 2, (zt - zb) / 2
        ring = []
        for k in range(segs + 1):
            th = -math.pi / 2 + k / segs * 2 * math.pi
            yy, zz = superellipse(th, hw, hz, 4.5)
            ring.append((x, yy, zc + zz))
        uvc = (0.5 if x > 0 else 0.03, 0.47)
        m.fan((x, 0, zc), ring, uvc, [uvc] * len(ring), flip=x < 0)
    # cabin: from windshield base to rear deck
    cab = [
        (-1.55, 0.86, 0.88, 0.0),
        (-1.20, 0.82, 1.06, 0.0),
        (-0.75, 0.70, 1.24, 0.0),
        (-0.20, 0.68, 1.27, 0.0),
        (0.20, 0.70, 1.22, 0.0),
        (0.80, 0.84, 0.92, 0.0),
    ]
    gl = region((0, 512, 512, 768))
    segs_c = 16
    pts, uvs = [], []
    for i, (x, hw, ztop, _) in enumerate(cab):
        zb = 0.86
        row, rowuv = [], []
        for k in range(segs_c + 1):
            t = k / segs_c  # side A bottom -> top -> side B bottom
            th = t * math.pi
            # tumblehome: narrower at the top
            yy = math.cos(th) * lerp(hw + 0.12, hw, abs(math.sin(th)))
            zz = zb + (ztop - zb) * (math.sin(th) ** 0.55)
            row.append((x, yy, zz))
            vv = 1 - abs(math.sin(th)) ** 0.55  # 0 at roof -> 1 at sills
            rowuv.append((lerp(gl[0], gl[2], i / (len(cab) - 1)), lerp(gl[1], gl[3], vv)))
        pts.append(row)
        uvs.append(rowuv)
    m.grid(pts, uvs, flip=True)
    # wheels
    rim = region((512 + 6, 512 + 6, 768 - 6, 768 - 6))
    tire = region((768, 512, 1024, 768))
    for wx in (-1.38, 1.36):
        for side in (-1, 1):
            wy = side * 0.86
            r, w = 0.37, 0.27
            prof = [(r * 0.70, -w / 2), (r * 0.96, -w / 2), (r, -w / 4), (r, w / 4), (r * 0.96, w / 2), (r * 0.70, w / 2)]
            revolve(m, prof, 24, tire, axis="y", center=(wx, wy, r), u_repeat=1.0)
            # rim disc on the outer side
            ring, ruv = [], []
            for k in range(33):
                a = 2 * math.pi * k / 32
                ring.append((wx + r * 0.72 * math.cos(a), wy + side * w / 2 * 1.01, r + r * 0.72 * math.sin(a)))
                ruv.append((lerp(rim[0], rim[2], 0.5 + 0.5 * math.cos(a)), lerp(rim[1], rim[3], 0.5 - 0.5 * math.sin(a))))
            m.fan((wx, wy + side * w / 2 * 1.03, r), ring, ((rim[0] + rim[2]) / 2, (rim[1] + rim[3]) / 2), ruv, flip=side > 0)
    # lights and grille as thin panels on the nose and tail
    hl, tl, gr = region((0, 768, 256, 896)), region((256, 768, 512, 896)), region((512, 768, 768, 896))
    for side in (-1, 1):
        m.box((2.10, side * 0.62 - 0.17, 0.50), (2.19, side * 0.62 + 0.17, 0.58), hl)
        m.box((-2.20, side * 0.60 - 0.20, 0.64), (-2.12, side * 0.60 + 0.20, 0.74), tl)
    m.box((2.15, -0.34, 0.36), (2.23, 0.34, 0.47), gr)
    # rear wing
    sp = region((768, 896, 1024, 1024))
    m.box((-2.12, -0.84, 1.05), (-1.80, 0.84, 1.09), sp)
    for side in (-1, 1):
        m.box((-2.02, side * 0.55 - 0.04, 0.88), (-1.94, side * 0.55 + 0.04, 1.06), sp)
    # side mirrors
    for side in (-1, 1):
        m.box((0.55, side * 0.95 - 0.06, 0.92), (0.72, side * 0.95 + 0.06, 1.0), region((800, 900, 1000, 1000)))
    return m


# ----------------------------------------------------------------------------------------
# lighthouse
# ----------------------------------------------------------------------------------------


def lighthouse_atlas(path: Path) -> None:
    S = 1024
    im = Image.new("RGB", (S, S), (40, 40, 40))
    d = ImageDraw.Draw(im)
    # tower (0,0)-(512,1024): 5 red/white bands, a few windows
    bands = 6
    for i in range(bands):
        col = (236, 236, 232) if i % 2 == 0 else (210, 44, 52)
        d.rectangle([0, i * S // bands, 512, (i + 1) * S // bands], fill=col)
    for y in (240, 520, 800):
        for x in (60, 316):
            d.rounded_rectangle([x, y, x + 36, y + 70], 14, fill=(30, 40, 60))
    noise = (RNG.normal(0, 8, (S, 512, 1))).repeat(3, 2)
    arr = np.asarray(im, np.float32)
    arr[:, :512] += noise
    im = Image.fromarray(arr.clip(0, 255).astype(np.uint8))
    d = ImageDraw.Draw(im)
    # lantern glass (512,0)-(768,512): warm glow with mullions
    g = np.linspace(255, 170, 512)[:, None].repeat(256, 1)
    lg = np.stack([g, g * 0.86, g * 0.42], -1)
    im.paste(Image.fromarray(lg.astype(np.uint8)), (512, 0))
    for x in range(512, 768, 42):
        d.rectangle([x, 0, x + 5, 512], fill=(30, 30, 34))
    # red roof (768,0)-(1024,512)
    d.rectangle([768, 0, 1024, 512], fill=(180, 36, 44))
    for y in range(0, 512, 40):
        d.line([(768, y), (1024, y)], fill=(150, 26, 34), width=6)
    # gallery / rail dark gray (512,512)-(768,768)
    d.rectangle([512, 512, 768, 768], fill=(46, 48, 56))
    # rock (768,512)-(1024,1024) and grass (512,768)-(768,1024)
    rock = (RNG.normal(0, 1, (512, 256)) * 18 + 112).clip(0, 255)
    rock = np.stack([rock, rock * 0.96, rock * 0.9], -1).astype(np.uint8)
    im.paste(Image.fromarray(rock).filter(ImageFilter.GaussianBlur(1.2)), (768, 512))
    grass = (RNG.normal(0, 1, (256, 256)) * 14).clip(-40, 40)
    gr = np.stack([60 + grass, 130 + grass, 70 + grass], -1).clip(0, 255).astype(np.uint8)
    im.paste(Image.fromarray(gr), (512, 768))
    im.save(path)


def build_lighthouse() -> Mesh:
    m = Mesh()
    tower = region((0, 0, 512, 1024))
    glass = region((512, 0, 768, 512))
    roof = region((768, 0, 1024, 512))
    metal = region((512, 512, 768, 768))
    rock = region((768, 512, 1024, 1024))
    grass = region((512, 768, 768, 1024))
    # rocky island
    segs = 40
    pts, uvs = [], []
    rings = 7
    for i in range(rings):
        t = i / (rings - 1)
        row, rowuv = [], []
        for k in range(segs + 1):
            a = 2 * math.pi * k / segs
            jitter = 1 + 0.12 * math.sin(a * 5 + i) + 0.07 * math.sin(a * 11 + 2 * i)
            r = lerp(3.4, 1.3, t ** 0.8) * jitter
            z = lerp(-0.6, 0.55, t ** 0.6) + 0.08 * math.sin(a * 7 + i)
            row.append((r * math.cos(a), r * math.sin(a), z))
            rowuv.append((lerp(rock[0], rock[2], k / segs), lerp(rock[3], rock[1], t)))
        pts.append(row)
        uvs.append(rowuv)
    m.grid(pts, uvs, flip=True)
    top = pts[-1]
    m.fan((0, 0, 0.6), top, ((grass[0] + grass[2]) / 2, (grass[1] + grass[3]) / 2), [((grass[0] + grass[2]) / 2, (grass[1] + grass[3]) / 2)] * len(top), flip=True)
    # tower
    prof = [(lerp(1.05, 0.68, i / 12), 0.5 + i * 0.5) for i in range(13)]
    revolve(m, [(r, h) for r, h in prof], 36, tower, axis="z")
    # gallery deck + rail
    revolve(m, [(0.62, 6.5), (1.05, 6.5), (1.05, 6.62), (0.62, 6.62)], 36, metal)
    for k in range(24):
        a = 2 * math.pi * k / 24
        x, y = 1.0 * math.cos(a), 1.0 * math.sin(a)
        m.box((x - 0.025, y - 0.025, 6.62), (x + 0.025, y + 0.025, 7.0), metal)
    revolve(m, [(0.98, 6.98), (1.03, 6.98), (1.03, 7.03), (0.98, 7.03)], 36, metal)
    # lantern + roof + ball
    revolve(m, [(0.52, 6.62), (0.52, 7.55)], 24, glass)
    revolve(m, [(0.62, 7.55), (0.66, 7.62), (0.5, 7.9), (0.22, 8.12), (0.06, 8.22), (0.0, 8.24)], 24, roof)
    revolve(m, [(0.0, 8.22), (0.08, 8.3), (0.08, 8.4), (0.0, 8.46)], 12, metal)
    return m


# ----------------------------------------------------------------------------------------
# palm tree
# ----------------------------------------------------------------------------------------


def palm_atlas(path: Path) -> None:
    S = 1024
    im = Image.new("RGB", (S, S))
    # bark (0,0)-(256,1024): rings
    y = np.arange(S)[:, None]
    ringv = (np.sin(y / 1024 * 2 * math.pi * 26) * 0.5 + 0.5) ** 3
    base = np.array([138, 104, 70], np.float32)
    dark = np.array([86, 62, 42], np.float32)
    bark = base * (1 - ringv[..., None]) + dark * ringv[..., None]
    bark = bark.repeat(256, 1) + RNG.normal(0, 7, (S, 256, 1))
    im.paste(Image.fromarray(bark.clip(0, 255).astype(np.uint8)), (0, 0))
    # leaf (256,0)-(768,1024): gradient green, darker spine
    lv = np.linspace(0, 1, S)[:, None, None]
    lu = np.linspace(-1, 1, 512)[None, :, None]
    c_tip = np.array([140, 200, 70], np.float32)
    c_base = np.array([40, 120, 60], np.float32)
    leaf = c_base * (1 - lv) + c_tip * lv
    leaf = leaf * (0.75 + 0.25 * (1 - np.abs(lu)))
    leaf = leaf.repeat(1, 0) * np.ones((1, 512, 1))
    spine = np.exp(-(lu * 14) ** 2)
    leaf = leaf * (1 - spine * 0.5) + np.array([200, 210, 120]) * spine * 0.4
    im.paste(Image.fromarray(leaf.clip(0, 255).astype(np.uint8)), (256, 0))
    # coconut (768,0)-(1024,256) + sand (768,256)-(1024,512)
    d = ImageDraw.Draw(im)
    d.rectangle([768, 0, 1024, 256], fill=(88, 64, 36))
    sand = (RNG.normal(0, 1, (256, 256)) * 9)
    sd = np.stack([226 + sand, 200 + sand, 150 + sand], -1).clip(0, 255).astype(np.uint8)
    im.paste(Image.fromarray(sd), (768, 256))
    im.save(path)


def build_palm() -> Mesh:
    m = Mesh()
    bark = region((0, 0, 256, 1024))
    leaf = region((256, 0, 768, 1024))
    nut = region((768, 0, 1024, 256))
    sand = region((768, 256, 1024, 512))

    def trunk_at(t):
        # gentle S curve leaning toward +x
        return np.array([1.6 * t ** 2 - 0.3 * math.sin(t * math.pi), 0.25 * math.sin(t * math.pi * 1.2), 6.4 * t])

    n, segs = 26, 14
    pts, uvs = [], []
    for i in range(n):
        t = i / (n - 1)
        c = trunk_at(t)
        tang = trunk_at(min(t + 0.01, 1)) - trunk_at(max(t - 0.01, 0))
        tang /= np.linalg.norm(tang)
        side = np.cross(tang, [0, 1, 0.0])
        side /= np.linalg.norm(side)
        up = np.cross(side, tang)
        r = lerp(0.26, 0.13, t) * (1 + 0.06 * (i % 2))
        row, rowuv = [], []
        for k in range(segs + 1):
            a = 2 * math.pi * k / segs
            p = c + r * (math.cos(a) * side + math.sin(a) * up)
            row.append(tuple(p))
            rowuv.append((lerp(bark[0], bark[2], k / segs), lerp(bark[3], bark[1], t)))
        pts.append(row)
        uvs.append(rowuv)
    m.grid(pts, uvs)
    top = trunk_at(1.0)
    # sandy mound
    mound = []
    for k in range(33):
        a = 2 * math.pi * k / 32
        mound.append((1.5 * math.cos(a), 1.5 * math.sin(a), -0.05))
    cu = ((sand[0] + sand[2]) / 2, (sand[1] + sand[3]) / 2)
    m.fan((0, 0, 0.18), mound, cu, [cu] * len(mound), flip=True)
    # fronds: spine + alternating leaflets
    fronds = 11
    for f in range(fronds):
        yaw = 2 * math.pi * f / fronds + 0.3 * math.sin(f * 2.1)
        lift = 0.45 - 0.5 * (f % 3) / 2
        length = 2.6 + 0.4 * math.sin(f * 1.7)
        dirv = np.array([math.cos(yaw), math.sin(yaw), 0.0])
        steps = 14
        spine = []
        for s in range(steps + 1):
            t = s / steps
            p = top + dirv * length * t + np.array([0, 0, lift * t * 2.2 - 1.9 * t ** 2.2])
            spine.append(p)
        side = np.cross(dirv, [0, 0, 1.0])
        for s in range(1, steps):
            t = s / steps
            p0, p1 = spine[s], spine[s + 1]
            w = 0.75 * math.sin(math.pi * min(t * 1.15, 1)) + 0.08
            droop = np.array([0, 0, -0.35 * w])
            for sgn in (-1, 1):
                tip = p0 + side * sgn * w + droop + dirv * 0.18
                ids = [
                    m.vert(p0, (lerp(leaf[0], leaf[2], 0.5), lerp(leaf[3], leaf[1], t))),
                    m.vert(p1, (lerp(leaf[0], leaf[2], 0.5), lerp(leaf[3], leaf[1], t + 1 / steps))),
                    m.vert(tip, (lerp(leaf[0], leaf[2], 0.5 + 0.5 * sgn), lerp(leaf[3], leaf[1], t + 0.5 / steps))),
                ]
                m.tri(*ids)
                m.tri(ids[0], ids[2], ids[1])
    # coconuts
    for k in range(4):
        a = 2 * math.pi * k / 4 + 0.4
        c = top + np.array([0.22 * math.cos(a), 0.22 * math.sin(a), -0.22])
        prof = [(0.13 * math.sin(math.pi * i / 6), -0.13 * math.cos(math.pi * i / 6)) for i in range(7)]
        revolve(m, prof, 10, nut, axis="z", center=tuple(c))
    return m


# ----------------------------------------------------------------------------------------
# city block
# ----------------------------------------------------------------------------------------


def city_atlas(path: Path) -> None:
    S = 1024
    im = Image.new("RGB", (S, S), (30, 30, 36))
    d = ImageDraw.Draw(im)
    # four facade styles, each (256 x 512): wall color + window grid (lit and unlit)
    walls = [(232, 196, 170), (120, 190, 200), (236, 230, 214), (196, 120, 150)]
    for i, wc in enumerate(walls):
        x0 = i * 256
        d.rectangle([x0, 0, x0 + 256, 512], fill=wc)
        for row in range(10):
            for col in range(4):
                lit = RNG.random() < 0.35
                c = (255, 214, 120) if lit else (40, 60, 90)
                x = x0 + 18 + col * 58
                y = 14 + row * 50
                d.rectangle([x, y, x + 40, y + 34], fill=c)
    # roof (0,512)-(256,768)
    d.rectangle([0, 512, 256, 768], fill=(70, 70, 78))
    d.rectangle([40, 560, 120, 640], fill=(90, 92, 100))
    # road + sidewalk (256,512)-(1024,1024)
    d.rectangle([256, 512, 1024, 1024], fill=(48, 48, 54))
    for x in range(300, 1024, 90):
        d.rectangle([x, 760, x + 50, 776], fill=(240, 210, 90))
    # park grass (0,768)-(256,1024)
    d.rectangle([0, 768, 256, 1024], fill=(80, 150, 90))
    im.save(path)


def build_city() -> Mesh:
    m = Mesh()
    road = region((256, 512, 1024, 1024))
    roof = region((0, 512, 256, 768))
    park = region((0, 768, 256, 1024))
    m.box((-14, -14, -0.2), (14, 14, 0.0), road)
    blocks = [(-11, -11), (-11, 2), (2, -11), (2, 2)]
    for bi, (bx, by) in enumerate(blocks):
        m.box((bx, by, 0.0), (bx + 9, by + 9, 0.15), roof)
        if bi == 3:
            m.box((bx + 0.5, by + 0.5, 0.15), (bx + 8.5, by + 8.5, 0.2), park)
            continue
        for gx in range(2):
            for gy in range(2):
                h = float(RNG.uniform(4, 16)) if (gx + gy + bi) % 3 else float(RNG.uniform(14, 24))
                style = int(RNG.integers(0, 4))
                fac = region((style * 256, 0, style * 256 + 256, 512))
                x0, y0 = bx + 0.6 + gx * 4.2, by + 0.6 + gy * 4.2
                # facade texture repeats roughly every 10 units of height
                m.box((x0, y0, 0.15), (x0 + 3.6, y0 + 3.6, 0.15 + h), fac, uv_scale=min(h / 12.0, 1.0))
                m.box((x0 + 0.2, y0 + 0.2, 0.15 + h), (x0 + 3.4, y0 + 3.4, 0.35 + h), roof)
    return m


# ----------------------------------------------------------------------------------------
# flat textures for the texture previews
# ----------------------------------------------------------------------------------------


def glow_text(base: Image.Image, xy, text, fnt, color, blur=18, anchor="mm"):
    layer = Image.new("RGBA", base.size, (0, 0, 0, 0))
    ImageDraw.Draw(layer).text(xy, text, fill=color, font=fnt, anchor=anchor)
    halo = layer.filter(ImageFilter.GaussianBlur(blur))
    base.alpha_composite(halo)
    base.alpha_composite(halo)
    core = Image.new("RGBA", base.size, (0, 0, 0, 0))
    ImageDraw.Draw(core).text(xy, text, fill=(255, 255, 255, 235), font=fnt, anchor=anchor)
    base.alpha_composite(layer)
    base.alpha_composite(core.filter(ImageFilter.GaussianBlur(1.2)))


def neon_sign(path: Path) -> None:
    W, H = 1024, 1024
    im = Image.new("RGBA", (W, H), (26, 18, 30, 255))
    d = ImageDraw.Draw(im)
    for row in range(0, H, 36):
        off = 0 if (row // 36) % 2 == 0 else 38
        for x in range(-76 + off, W, 76):
            shade = int(RNG.integers(36, 52))
            d.rectangle([x + 2, row + 2, x + 72, row + 32], fill=(shade + 10, shade - 6, shade + 4, 255))
    glow_text(im, (512, 300), "SIX STARS", font("Brush Script.ttf", 190), (255, 70, 170, 255))
    glow_text(im, (512, 520), "MOTEL", font("Futura.ttc", 150), (70, 220, 255, 255))
    for k in range(6):
        cx = 182 + k * 132
        glow_text(im, (cx, 690), "★", font("Apple Symbols.ttf", 96), (255, 200, 60, 255), blur=12)
    glow_text(im, (512, 860), "VACANCY", font("Futura.ttc", 84), (120, 255, 140, 255), blur=14)
    im.convert("RGB").save(path)


def sunset_poster(path: Path) -> None:
    W, H = 1024, 1024
    y = np.linspace(0, 1, H)[:, None, None]
    top = np.array([40, 20, 90], np.float32)
    mid = np.array([240, 70, 140], np.float32)
    low = np.array([255, 170, 60], np.float32)
    sky = np.where(y < 0.55, top * (1 - y / 0.55) + mid * (y / 0.55), mid * (1 - (y - 0.55) / 0.45) + low * ((y - 0.55) / 0.45))
    sky = sky * np.ones((1, W, 1))
    im = Image.fromarray(sky.astype(np.uint8)).convert("RGBA")
    d = ImageDraw.Draw(im)
    # striped sun
    cx, cy, r = 512, 560, 250
    sun = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    sd = ImageDraw.Draw(sun)
    for i in range(r * 2):
        yy = cy - r + i
        t = i / (2 * r)
        col = (255, int(lerp(230, 90, t)), int(lerp(90, 120, t)), 255)
        half = math.sqrt(max(r * r - (yy - cy) ** 2, 0))
        if t > 0.5 and int((t - 0.5) * 40) % 3 == 2:
            continue
        sd.line([(cx - half, yy), (cx + half, yy)], fill=col)
    im.alpha_composite(sun)
    # sea
    d.rectangle([0, 700, W, H], fill=(30, 16, 60, 255))
    for k in range(18):
        yy = 712 + k * 17
        w = 260 - k * 9
        d.line([(cx - w, yy), (cx + w, yy)], fill=(255, 140, 120, 200), width=3)
    # palm silhouettes
    def palm(x0, h, lean):
        pts = []
        for i in range(20):
            t = i / 19
            pts.append((x0 + lean * t * t * h * 0.3, 700 - h * t))
        for (xa, ya), (xb, yb) in zip(pts, pts[1:]):
            d.line([(xa, ya), (xb, yb)], fill=(20, 8, 30, 255), width=int(14 - 6 * (ya - 700) / -h))
        tx, ty = pts[-1]
        for k in range(8):
            a = math.pi * (0.05 + 0.9 * k / 7)
            ex, ey = tx + 150 * math.cos(a) * (1 if k % 2 else -1), ty - 40 * math.sin(a) + 50
            d.line([(tx, ty), ((tx + ex) / 2, ty - 30), (ex, ey)], fill=(20, 8, 30, 255), width=12, joint="curve")
    palm(150, 520, 1)
    palm(880, 430, -1)
    title = font("Futura.ttc", 120)
    d.text((512, 120), "SUNSET", fill=(255, 240, 230, 255), font=title, anchor="mm")
    d.text((512, 230), "COAST", fill=(255, 210, 120, 255), font=title, anchor="mm")
    im.convert("RGB").save(path)


def livery_sheet(path: Path) -> None:
    W, H = 1024, 1024
    im = Image.new("RGBA", (W, H), (240, 236, 228, 255))
    d = ImageDraw.Draw(im)
    for i, c in enumerate([(214, 38, 120), (255, 128, 40), (250, 210, 90)]):
        d.polygon([(0, 70 + i * 60), (W, 20 + i * 60), (W, 60 + i * 60), (0, 110 + i * 60)], fill=c + (255,))
    for k, (x, y, num) in enumerate([(200, 470, "06"), (512, 470, "17"), (824, 470, "42")]):
        d.ellipse([x - 130, y - 130, x + 130, y + 130], fill=(30, 20, 40, 255))
        d.ellipse([x - 112, y - 112, x + 112, y + 112], fill=(250, 246, 236, 255))
        d.text((x, y + 6), num, fill=(30, 20, 40, 255), font=font("Futura.ttc", 150), anchor="mm")
    d.rounded_rectangle([90, 680, 934, 820], 40, fill=(30, 20, 40, 255))
    d.text((512, 752), "SUNSET GT", fill=(255, 128, 40, 255), font=font("Futura.ttc", 110), anchor="mm")
    for k in range(7):
        x = 110 + k * 134
        d.text((x, 920), "★", fill=(214, 38, 120, 255), font=font("Apple Symbols.ttf", 110), anchor="mm")
    im.convert("RGB").save(path)


def road_tile(path: Path) -> None:
    S = 1024
    base = RNG.normal(0, 1, (S, S))
    base = np.asarray(Image.fromarray(((base * 20) + 70).clip(0, 255).astype(np.uint8)).filter(ImageFilter.GaussianBlur(1.1)), np.float32)
    rgb = np.stack([base, base, base * 1.06], -1)
    im = Image.fromarray(rgb.clip(0, 255).astype(np.uint8)).convert("RGBA")
    d = ImageDraw.Draw(im)
    for y in range(0, S, 160):
        d.rectangle([496, y, 528, y + 90], fill=(236, 200, 70, 255))
    d.rectangle([60, 0, 76, S], fill=(230, 230, 230, 255))
    d.rectangle([948, 0, 964, S], fill=(230, 230, 230, 255))
    for x in range(120, 900, 80):
        d.rectangle([x, 820, x + 48, 960], fill=(236, 236, 236, 255))
    im.convert("RGB").save(path)


SAMPLE_XML = """<?xml version="1.0" encoding="UTF-8"?>
<!-- made-up handling data for the Percy6Stars demo coupe -->
<VehicleHandling name="sunset_gt">
  <Mass value="1340.0" />
  <DragCoefficient value="0.29" />
  <CenterOfMass x="0.0" y="-0.05" z="0.42" />
  <Engine>
    <Layout>front-mid</Layout>
    <Cylinders value="6" />
    <PeakPower kw="298" rpm="7200" />
    <RedLine rpm="7800" />
  </Engine>
  <Gearbox type="sequential" gears="6">
    <Ratio gear="1" value="3.31" />
    <Ratio gear="2" value="2.13" />
    <Ratio gear="3" value="1.55" />
    <Ratio gear="4" value="1.21" />
    <Ratio gear="5" value="0.98" />
    <Ratio gear="6" value="0.81" />
  </Gearbox>
  <Tyres front="245/35R19" rear="285/30R20" grip="1.12" />
  <Suspension stiffness="0.72" rideHeight="0.11" antiRoll="0.65" />
  <Paint primary="#D62678" secondary="#FF8028" livery="sunset_stripes" />
</VehicleHandling>
"""

SAMPLE_STRINGS = """=== Text table (12 strings) ===

DEMO_TITLE        Welcome to Sunset Coast
DEMO_SUB          A made-up sample set for the Percy6Stars README
MOTEL_SIGN        Six Stars Motel · Vacancy
GARAGE_ENTER      Press E to enter the garage
GARAGE_PAINT      Pick a paint: Sunset Pink, Ocean Blue, Lemon
RADIO_STATION_01  Coastline FM · synthwave all night
RADIO_STATION_02  Harbor Talk · call in after the beep
LIGHTHOUSE_HINT   Follow the lighthouse to the marina
RACE_START        3 · 2 · 1 · GO
RACE_FINISH       New lap record: 1:12.06
SHOP_CLOSED       Back in 5 minutes. Probably.
CREDITS_ROLL      Thanks for exploring the demo
"""


def main() -> None:
    out = Path(sys.argv[1] if len(sys.argv) > 1 else "demo-data")
    out.mkdir(parents=True, exist_ok=True)
    print("models:")
    car_atlas(out / "sunset_gt.png")
    build_car().save(out / "sunset_gt.obj", "sunset_gt")
    lighthouse_atlas(out / "harbor_lighthouse.png")
    build_lighthouse().save(out / "harbor_lighthouse.obj", "harbor_lighthouse")
    palm_atlas(out / "palm_tall.png")
    build_palm().save(out / "palm_tall.obj", "palm_tall")
    city_atlas(out / "city_block.png")
    build_city().save(out / "city_block.obj", "city_block")
    print("textures:")
    neon_sign(out / "neon_motel_sign.png")
    sunset_poster(out / "sunset_coast_poster.png")
    livery_sheet(out / "gt_livery_decals.png")
    road_tile(out / "coast_road.png")
    (out / "sunset_gt_handling.xml").write_text(SAMPLE_XML)
    (out / "demo_strings.txt").write_text(SAMPLE_STRINGS)
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
