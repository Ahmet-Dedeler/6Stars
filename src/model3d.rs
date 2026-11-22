use eframe::egui::{self, Color32, ColorImage, CornerRadius, RichText, TextureHandle, TextureOptions};
use flate2::{Decompress, FlushDecompress, Status};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

mod vertex_stream;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ShadeMode {
    #[default]
    Textured,
    Solid,
    SolidWire,
    Wireframe,
}

#[derive(Clone)]
pub struct Texture2D {
    pub w: usize,
    pub h: usize,
    pub pixels: Vec<Color32>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum MaterialPass {
    #[default]
    Opaque,
    Cutout,
    Alpha,
    Decal,
    Glass { texture_alpha: bool },
}

#[derive(Clone, Default)]
pub struct GeomMaterial {
    // surface color texture currently applied first enabled entry of color_layers
    pub texture: Option<Arc<Texture2D>>,
    pub is_vehicle_paint: bool,
    pass: MaterialPass,
    auxiliary_pass: bool,
    // tinted opaque vehicle body material shown as car paint while no color texture is applied
    paint_capable: bool,
    // indices into mesh tex_layers color candidates in priority order
    color_layers: Vec<u16>,
    normal_layers: Vec<u16>,
    overlay_layers: Vec<u16>,
    // normal map and multiply overlays currently applied
    normal: Option<Arc<Texture2D>>,
    overlays: Vec<Arc<Texture2D>>,
    /// Colour texture from the drawable's own embedded dictionary (fork addition). It is
    /// not a pooled layer, so it stays applied whatever the layer toggles say.
    embedded_color: Option<Arc<Texture2D>>,
    /// Eyeball shader: the iris texture is composited over a sclera colour.
    eye: bool,
}

// how an applied texture layer contributes to the preview
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TexLayerKind {
    // surface color
    Color,
    // tangent space normal map that perturbs the lighting
    Normal,
    // dirt dust and mask maps multiplied over the surface
    Overlay,
}

// one pooled texture bound by the materials of a mesh that can be switched on or off in the preview
#[derive(Clone)]
pub struct TexLayer {
    pub hash64: u64,
    pub label: String,
    pub kind: TexLayerKind,
    pub w: usize,
    pub h: usize,
    // pool file and descriptor index
    pub source: String,
    // number of drawn geometries whose material binds it
    pub mats: usize,
    pub enabled: bool,
    // part of the default look
    pub default_on: bool,
    hit: crate::tex::TtpfHashHit,
    tex: Option<Arc<Texture2D>>,
    failed: bool,
}

#[derive(Clone, Copy)]
pub struct ElsPoint {
    pub position: [f32; 3],
    pub bank: bool,
    pub slot: u8,
}

// Decoded from the RAGE 9 Sirens PSO preset (id 3) in data.rpf. As in GTA V,
// each light consumes one bit per beat, most-significant bit first. GTA VI keeps
// the 32-step sequencers but stores the carcols-style data in hashed PSO fields.
const RAGE9_SIREN_BPM: f64 = 500.0;
const RAGE9_SIREN_MULTIPLES: u64 = 3;
const RAGE9_SIREN_SEQUENCERS: [u32; 20] = [
    0xAAAA_AAAA, 0xAA00_AA80, 0x5500_5500, 0x5555_5555,
    0xAA5A_A954, 0x5555_554A, 0x5555_554A, 0xAA5A_A954,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

fn rage9_siren_lit(slot: u8, phase: u64) -> bool {
    let Some(&sequencer) = slot.checked_sub(1).and_then(|i| RAGE9_SIREN_SEQUENCERS.get(i as usize)) else {
        return false;
    };
    let subdivisions = RAGE9_SIREN_MULTIPLES * 2;
    let beat = (phase / subdivisions) % 32;
    let pulse = phase % subdivisions;
    sequencer & (1 << (31 - beat)) != 0 && pulse.is_multiple_of(2)
}

impl GeomMaterial {
    fn blends(&self, has_uvs: bool) -> bool {
        matches!(self.pass, MaterialPass::Glass { .. })
            || (has_uvs && self.texture.is_some() && matches!(self.pass, MaterialPass::Alpha | MaterialPass::Decal))
    }
}

const GLASS_PREVIEW_ALPHA: u8 = 64;

const DEFAULT_PAINT_COLOR: [f32; 3] = [0.78, 0.11, 0.14];

const PAINT_PRESETS: [(&str, [f32; 3]); 8] = [
    ("Red", [0.78, 0.11, 0.14]),
    ("Vice Pink", [0.92, 0.25, 0.58]),
    ("Blue", [0.10, 0.30, 0.76]),
    ("Yellow", [0.92, 0.74, 0.10]),
    ("Green", [0.09, 0.48, 0.28]),
    ("Silver", [0.76, 0.79, 0.84]),
    ("Black", [0.10, 0.11, 0.13]),
    ("White", [0.93, 0.94, 0.96]),
];

impl Texture2D {
    #[inline]
    pub fn sample(&self, u: f32, v: f32) -> Color32 {
        if self.w == 0 || self.h == 0 {
            return Color32::WHITE;
        }
        let x = ((u.rem_euclid(1.0) * (self.w as f32)) as usize).min(self.w - 1);
        let y = ((v.rem_euclid(1.0) * (self.h as f32)) as usize).min(self.h - 1);
        self.pixels[y * self.w + x]
    }
}

const DEFAULT_YAW: f32 = 2.45;
const DEFAULT_PITCH: f32 = 0.38;

#[derive(Clone)]
pub struct Mesh3D {
    pub name: String,
    pub verts: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub tris: Vec<[u32; 3]>,
    pub tri_geom: Vec<u16>,
    pub normals: Vec<[f32; 3]>,
    pub edges: Vec<(u32, u32)>,
    pub texture: Option<Texture2D>,
    pub geom_materials: Vec<GeomMaterial>,
    // pooled textures bound by the materials toggled from the viewer
    pub tex_layers: Vec<TexLayer>,
    pub has_paint_geom: bool,
    pub paint_color: [f32; 3],
    pub has_els: bool,
    pub els_enabled: bool,
    pub els_flash: bool,
    pub els_points: Vec<ElsPoint>,
    vehicle_attach_points: Vec<(String, [f32; 3])>,
    els_flash_phase: u64,
    pub tex_status: String,
    pub center: [f32; 3],
    pub radius: f32,
    pub geom_count: usize,
    pub meshlet_count: usize,
    pub bone_count: usize,
    pub lod_label: String,
    /// Decoded LOD level and geometry count per level (all zero without a LOD table).
    pub lod: usize,
    pub lod_geoms: [usize; MAX_LODS],
    /// Separate drawables (dictionary entries); empty for single-drawable meshes.
    pub parts: Vec<MeshPart>,
    /// `None` = show every part (grid layout), `Some(i)` = isolate `parts[i]`.
    pub active_part: Option<usize>,
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
    pub shade_mode: ShadeMode,
    pub auto_spin: bool,
    pub sky_bg: bool,
    // reused softwre rasterizer buffrs zero allocs per fram
    fb_w: usize,
    fb_h: usize,
    color_buf: Vec<Color32>,
    z_buf: Vec<f32>,
    proj_buf: Vec<[f32; 3]>,
    /// Per-vertex perspective scale (proportional to 1/w), used for perspective-correct UVs.
    persp_buf: Vec<f32>,
    blend_order: Vec<(usize, f32, bool)>,
    tex: Option<TextureHandle>,
    pub dirty: bool,
    last_dragging: bool,
}

impl Mesh3D {
    // rebinds every material to its enabled texture layers decoding newly enabled ones on demand
    pub fn apply_tex_layers(&mut self) {
        let mut budget = 384usize;
        for l in &mut self.tex_layers {
            if l.enabled && l.tex.is_none() && !l.failed && budget > 0 {
                budget -= 1;
                l.tex = crate::tex::decode_ttpf_texture_cached(&l.hit);
                l.failed = l.tex.is_none();
            }
        }
        let layers = &self.tex_layers;
        let live = |ids: &[u16]| -> Vec<Arc<Texture2D>> {
            ids.iter()
                .filter_map(|&i| layers.get(i as usize))
                .filter(|l| l.enabled)
                .filter_map(|l| l.tex.clone())
                .collect()
        };
        for m in &mut self.geom_materials {
            // highest priority ticked color layer wins even when it could not be decoded
            m.texture = match &m.embedded_color {
                Some(t) => Some(t.clone()),
                None => m.color_layers.iter().filter_map(|&i| layers.get(i as usize)).find(|l| l.enabled).and_then(|l| l.tex.clone()),
            };
            if m.eye {
                // The eye's diffuseTex is just the iris on a transparent background; the game
                // shader fills the rest with the sclera. Bake that in so the preview isn't black.
                m.texture = m.texture.take().map(|t| over_sclera(&t));
            }
            m.normal = live(&m.normal_layers).into_iter().next();
            m.overlays = live(&m.overlay_layers);
            if matches!(m.pass, MaterialPass::Glass { .. }) {
                if m.texture.as_ref().is_some_and(|t| t.w <= 4 && t.h <= 4 && t.pixels.iter().all(|p| p.a() == 0)) {
                    m.texture = None;
                }
                m.pass = MaterialPass::Glass {
                    texture_alpha: m.texture.as_ref().is_some_and(|t| t.pixels.iter().any(|p| p.a() < 255)),
                };
            }
            // an enabled color texture that failed to decode keeps the neutral material look instead of paint
            let color_bound = m.color_layers.iter().any(|&i| layers.get(i as usize).is_some_and(|l| l.enabled));
            m.is_vehicle_paint = m.paint_capable && !color_bound;
        }
        self.has_paint_geom = self.geom_materials.iter().any(|m| m.paint_capable);
        self.refresh_tex_status();
        self.dirty = true;
    }

    // switches layers on or off by predicate and reapplies them
    pub fn set_tex_layers(&mut self, on: impl Fn(&TexLayer) -> bool) {
        for l in &mut self.tex_layers {
            l.enabled = on(l);
        }
        self.apply_tex_layers();
    }

    fn refresh_tex_status(&mut self) {
        if self.geom_materials.is_empty() {
            return;
        }
        if let Some(at) = self.tex_status.find(" + ELS ") {
            self.tex_status.truncate(at);
        }
        if self.texture.is_none() {
            let textured = self.geom_materials.iter().filter(|m| m.texture.is_some()).count();
            let paint = self.geom_materials.iter().filter(|m| m.is_vehicle_paint).count();
            let drawn = self.geom_materials.iter().filter(|m| !m.auxiliary_pass).count().max(1);
            if textured > 0 && paint > 0 {
                self.tex_status = format!("Textures: {textured}/{drawn} geoms + preview paint ({paint})");
            } else if textured > 0 {
                self.tex_status = format!("Textures: {textured}/{drawn} geoms");
            } else if paint > 0 {
                self.tex_status = format!("Preview paint: {paint}/{drawn} geoms");
            } else if !self.tex_layers.is_empty() {
                self.tex_status = "No color textures applied; neutral preview".into();
            }
        }
        if self.has_els {
            self.tex_status.push_str(&format!(" + ELS {} emitters", self.els_points.len()));
        }
    }

    pub fn from_parts(
        name: String,
        verts: Vec<[f32; 3]>,
        uvs: Vec<[f32; 2]>,
        tris: Vec<[u32; 3]>,
        tri_geom: Vec<u16>,
        edges: Vec<(u32, u32)>,
        texture: Option<Texture2D>,
        tex_status: String,
        geom_count: usize,
        meshlet_count: usize,
        lod_label: String,
    ) -> Self {
        let (mut min_v, mut max_v) = ([f32::MAX; 3], [f32::MIN; 3]);
        for v in &verts {
            for k in 0..3 {
                if v[k] < min_v[k] { min_v[k] = v[k]; }
                if v[k] > max_v[k] { max_v[k] = v[k]; }
            }
        }
        let center = if verts.is_empty() {
            [0.0; 3]
        } else {
            [
                (min_v[0] + max_v[0]) * 0.5,
                (min_v[1] + max_v[1]) * 0.5,
                (min_v[2] + max_v[2]) * 0.5,
            ]
        };
        let mut r2 = 0.0f32;
        for v in &verts {
            let dx = v[0] - center[0];
            let dy = v[1] - center[1];
            let dz = v[2] - center[2];
            r2 = r2.max(dx * dx + dy * dy + dz * dz);
        }
        let radius = r2.sqrt().max(1e-3);

        // precomput unit face normals once so drag stays fast
        let mut normals = Vec::with_capacity(tris.len());
        for t in &tris {
            let a = verts[t[0] as usize];
            let b = verts[t[1] as usize];
            let c = verts[t[2] as usize];
            let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let nx = ab[1] * ac[2] - ab[2] * ac[1];
            let ny = ab[2] * ac[0] - ab[0] * ac[2];
            let nz = ab[0] * ac[1] - ab[1] * ac[0];
            let len = (nx * nx + ny * ny + nz * nz).sqrt();
            if len > 1e-12 {
                normals.push([nx / len, ny / len, nz / len]);
            } else {
                normals.push([0.0, 0.0, 1.0]);
            }
        }

        Self {
            name,
            verts,
            uvs,
            tris,
            tri_geom,
            normals,
            edges,
            texture,
            geom_materials: Vec::new(),
            has_paint_geom: false,
            tex_layers: Vec::new(),
            paint_color: DEFAULT_PAINT_COLOR,
            has_els: false,
            els_enabled: false,
            els_flash: true,
            els_points: Vec::new(),
            vehicle_attach_points: Vec::new(),
            els_flash_phase: 0,
            tex_status,
            center,
            radius,
            geom_count,
            meshlet_count,
            bone_count: 0,
            lod_label,
            lod: 0,
            lod_geoms: [0; MAX_LODS],
            parts: Vec::new(),
            active_part: None,
            yaw: DEFAULT_YAW,
            pitch: DEFAULT_PITCH,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            shade_mode: ShadeMode::Textured,
            auto_spin: false,
            sky_bg: false,
            fb_w: 0,
            fb_h: 0,
            color_buf: Vec::new(),
            z_buf: Vec::new(),
            proj_buf: Vec::new(),
            persp_buf: Vec::new(),
            blend_order: Vec::new(),
            tex: None,
            dirty: true,
            last_dragging: false,
        }
    }

    /// Triangle range currently shown (the active part, or everything).
    pub fn visible_tris(&self) -> std::ops::Range<usize> {
        self.active_part
            .and_then(|i| self.parts.get(i))
            .map(|p| p.tris.start.min(self.tris.len())..p.tris.end.min(self.tris.len()))
            .unwrap_or(0..self.tris.len())
    }

    /// Isolates one part (or all with `None`) and re-frames the camera on it.
    pub fn set_active_part(&mut self, part: Option<usize>) {
        self.active_part = part.filter(|&i| i < self.parts.len());
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for t in &self.tris[self.visible_tris()] {
            for &i in t {
                let v = self.verts[i as usize];
                for k in 0..3 {
                    lo[k] = lo[k].min(v[k]);
                    hi[k] = hi[k].max(v[k]);
                }
            }
        }
        if lo[0] <= hi[0] {
            self.center = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5];
            let d = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
            self.radius = ((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() * 0.5).max(1e-3);
        }
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        self.dirty = true;
    }

    // Adds an independently stored vehicle option at the same model-space
    // origin. RAGE 9 tuning parts (bumpers, light bars, liveries, wheels, etc.)
    // are separate fragment resources authored against the parent vehicle.
    pub fn append_vehicle_part(&mut self, mut part: Mesh3D) {
        let offset = if part.name.to_ascii_lowercase().starts_with("cmn_siren_roofbar1_") {
            self.vehicle_attach_points
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("siren_roof_1"))
                .map(|(_, position)| *position)
                .unwrap_or([0.0; 3])
        } else {
            [0.0; 3]
        };
        if offset != [0.0; 3] {
            for vertex in &mut part.verts {
                for axis in 0..3 {
                    vertex[axis] += offset[axis];
                }
            }
            for light in &mut part.els_points {
                for axis in 0..3 {
                    light.position[axis] += offset[axis];
                }
            }
            for (_, point) in &mut part.vehicle_attach_points {
                for axis in 0..3 {
                    point[axis] += offset[axis];
                }
            }
        }
        let vertex_base = self.verts.len() as u32;
        let triangle_base = self.tris.len();
        let geom_base = self.geom_materials.len() as u16;
        let layer_base = self.tex_layers.len() as u16;
        if self.parts.is_empty() && triangle_base > 0 {
            self.parts.push(MeshPart {
                name: self.name.clone(),
                geoms: self.geom_count,
                tris: 0..triangle_base,
            });
        }

        self.verts.append(&mut part.verts);
        self.uvs.append(&mut part.uvs);
        self.normals.append(&mut part.normals);
        self.tris.extend(part.tris.drain(..).map(|t| {
            [t[0] + vertex_base, t[1] + vertex_base, t[2] + vertex_base]
        }));
        self.tri_geom.extend(part.tri_geom.drain(..).map(|g| g.saturating_add(geom_base)));
        self.edges.extend(part.edges.drain(..).map(|(a, b)| (a + vertex_base, b + vertex_base)));

        for material in &mut part.geom_materials {
            for layer in material
                .color_layers
                .iter_mut()
                .chain(&mut material.normal_layers)
                .chain(&mut material.overlay_layers)
            {
                *layer = layer.saturating_add(layer_base);
            }
        }
        self.geom_materials.append(&mut part.geom_materials);
        self.tex_layers.append(&mut part.tex_layers);
        self.els_points.append(&mut part.els_points);
        self.vehicle_attach_points.append(&mut part.vehicle_attach_points);

        let triangle_end = self.tris.len();
        if triangle_end > triangle_base {
            self.parts.push(MeshPart {
                name: part.name,
                geoms: part.geom_count,
                tris: triangle_base..triangle_end,
            });
        }
        self.geom_count += part.geom_count;
        self.meshlet_count += part.meshlet_count;
        self.bone_count = self.bone_count.max(part.bone_count);
        self.has_els |= part.has_els;
        self.active_part = None;
        self.set_active_part(None);
        self.apply_tex_layers();
    }

    // writes visible parts as obj one group per drawable and recenters isolated part
    pub fn export_obj(&self, path: &Path) -> std::io::Result<()> {
        let mut w = BufWriter::new(File::create(path)?);
        writeln!(w, "# Percy6Stars RAGE9 Model Export: {}", self.name)?;
        let groups: Vec<(&str, std::ops::Range<usize>)> = match self.active_part.and_then(|i| self.parts.get(i)) {
            Some(p) => vec![(p.name.as_str(), self.visible_tris())],
            None if !self.parts.is_empty() => self.parts.iter().map(|p| (p.name.as_str(), p.tris.clone())).collect(),
            None => vec![(self.name.as_str(), 0..self.tris.len())],
        };
        let shift = if self.active_part.is_some() { [self.center[0], self.center[1], 0.0] } else { [0.0; 3] };
        let has_uv = self.uvs.len() == self.verts.len() && !self.uvs.is_empty();
        let mut remap = vec![0u32; self.verts.len()];
        let mut next = 1u32;
        for (name, range) in groups {
            writeln!(w, "o {name}")?;
            let tris = &self.tris[range];
            for &i in tris.iter().flatten() {
                let i = i as usize;
                if remap[i] == 0 {
                    remap[i] = next;
                    next += 1;
                    let v = self.verts[i];
                    writeln!(w, "v {:.6} {:.6} {:.6}", v[0] - shift[0], v[1] - shift[1], v[2] - shift[2])?;
                    if has_uv {
                        writeln!(w, "vt {:.6} {:.6}", self.uvs[i][0], self.uvs[i][1])?;
                    }
                }
            }
            for t in tris {
                let (a, b, c) = (remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]);
                if has_uv {
                    writeln!(w, "f {a}/{a} {b}/{b} {c}/{c}")?;
                } else {
                    writeln!(w, "f {a} {b} {c}")?;
                }
            }
        }
        Ok(())
    }
}

pub fn parse_obj(path: &Path) -> Option<Mesh3D> {
    let txt = fs::read_to_string(path).ok()?;
    let mut verts = Vec::new();
    let mut tris = Vec::new();
    let mut edges = Vec::new();
    for line in txt.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                if let (Some(x), Some(y), Some(z)) = (
                    it.next().and_then(|s| s.parse::<f32>().ok()),
                    it.next().and_then(|s| s.parse::<f32>().ok()),
                    it.next().and_then(|s| s.parse::<f32>().ok()),
                ) {
                    verts.push([x, y, z]);
                }
            }
            Some("f") => {
                let idxs: Vec<u32> = it
                    .filter_map(|tok| tok.split('/').next()?.parse::<usize>().ok()?.checked_sub(1))
                    .filter(|&i| i < verts.len())
                    .map(|i| i as u32)
                    .collect();
                if idxs.len() >= 3 {
                    for k in 1..idxs.len() - 1 {
                        tris.push([idxs[0], idxs[k], idxs[k + 1]]);
                    }
                } else if idxs.len() == 2 {
                    edges.push((idxs[0], idxs[1]));
                }
            }
            _ => {}
        }
    }
    if verts.is_empty() { return None; }
    // flip external y up obj files into z up turntabl space
    // our own exports are already z-up ("# 6Stars" / "# PercyStar" = files from before the renames)
    if !(txt.starts_with("# Percy6Stars") || txt.starts_with("# 6Stars") || txt.starts_with("# PercyStar")) {
        for v in &mut verts {
            *v = [v[0], -v[2], v[1]];
        }
    }
    let tri_geom = vec![0u16; tris.len()];
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    Some(Mesh3D::from_parts(name, verts, Vec::new(), tris, tri_geom, edges, None, "OBJ".into(), 1, 0, "OBJ".into()))
}

pub fn extract_0drpz_mesh(name: &str, data: &[u8]) -> Option<Mesh3D> {
    let mut verts = Vec::new();
    let mut off = 0x40usize;
    while off + 12 <= data.len() && verts.len() < 12000 {
        let x = rf32(data, off);
        let y = rf32(data, off + 4);
        let z = rf32(data, off + 8);
        if x.is_finite() && y.is_finite() && z.is_finite()
            && x.abs() < 250.0 && y.abs() < 250.0 && z.abs() < 250.0
            && (x.abs() > 1e-4 || y.abs() > 1e-4 || z.abs() > 1e-4)
        {
            verts.push([x, y, z]);
            off += 12;
        } else {
            off += 4;
        }
    }
    if verts.len() < 12 { return None; }
    Some(Mesh3D::from_parts(name.to_string(), verts, Vec::new(), Vec::new(), Vec::new(), Vec::new(), None, "Points".into(), 1, 0, "Points".into()))
}

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
pub fn is_zlib_head(b: &[u8]) -> bool {
    b.len() >= 3 && b[0] == 0x78 && matches!(b[1], 0x01 | 0x5e | 0x9c | 0xda) && (b[2] & 0x06) != 0x06
}

#[inline]
fn vptr(q: u64) -> Option<usize> {
    (0x5000_0000..0x5800_0000).contains(&q).then(|| (q - 0x5000_0000) as usize)
}

#[inline]
fn pptr(q: u64) -> Option<usize> {
    (0x6000_0000..0x7000_0000).contains(&q).then(|| (q - 0x6000_0000) as usize)
}

// decomprss one zlib stream from memory slice returns false if incomplete
fn decompress_zlib_slice(src: &[u8], dec: &mut Decompress, out: &mut Vec<u8>) -> Option<usize> {
    dec.reset(true);
    let mut tmp = [0u8; 65536];
    let mut pos = 0usize;
    loop {
        let bi = dec.total_in();
        let bo = dec.total_out();
        let st = dec.decompress(&src[pos..], &mut tmp, FlushDecompress::None).ok()?;
        let cin = (dec.total_in() - bi) as usize;
        let cout = (dec.total_out() - bo) as usize;
        pos += cin;
        if cout > 0 {
            out.extend_from_slice(&tmp[..cout]);
        }
        if st == Status::StreamEnd {
            return Some(pos);
        }
        if cin == 0 && cout == 0 {
            return None;
        }
    }
}

fn decompress_zlib_at(f: &mut File, file_off: u64, max_out: usize) -> std::io::Result<(Vec<u8>, u64)> {
    f.seek(SeekFrom::Start(file_off))?;
    let mut dec = Decompress::new(true);
    let mut in_buf = [0u8; 65536];
    let mut out_chunk = [0u8; 65536];
    let mut out = Vec::with_capacity(65536);
    let mut total_consumed = 0u64;
    loop {
        let n = f.read(&mut in_buf)?;
        if n == 0 { break; }
        let mut pos = 0usize;
        while pos < n {
            let bi = dec.total_in();
            let bo = dec.total_out();
            let status = dec
                .decompress(&in_buf[pos..n], &mut out_chunk, FlushDecompress::None)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            let cin = (dec.total_in() - bi) as usize;
            let cout = (dec.total_out() - bo) as usize;
            pos += cin;
            total_consumed += cin as u64;
            if cout > 0 {
                out.extend_from_slice(&out_chunk[..cout]);
                if out.len() > max_out {
                    return Ok((out, file_off + total_consumed));
                }
            }
            if status == Status::StreamEnd {
                return Ok((out, file_off + total_consumed));
            }
            if cin == 0 && cout == 0 {
                break;
            }
        }
    }
    Ok((out, file_off + total_consumed))
}

fn find_next_zlib(f: &mut File, start_off: u64) -> std::io::Result<Option<u64>> {
    f.seek(SeekFrom::Start(start_off))?;
    let mut buf = [0u8; 4096];
    let n = f.read(&mut buf)?;
    for i in 0..n.saturating_sub(3) {
        if is_zlib_head(&buf[i..n]) {
            return Ok(Some(start_off + i as u64));
        }
    }
    Ok(None)
}

// reads all virtual chnks from a rage9 resource stopping right after blockmap
pub fn read_virtual_from_file(f: &mut File, file_off: u64) -> Result<(Vec<u8>, u64, usize), String> {
    let (mut v, mut next_off) = decompress_zlib_at(f, file_off, 16 * 1024 * 1024)
        .map_err(|e| format!("Virtual chunk 0 error: {e}"))?;
    if v.len() < 0x80 {
        return Err("Virtual chunk too small".into());
    }
    // Mid-resource virtual chunks can start directly with a grmGeometry struct
    // (vft == 0, +0x08 == 0x5002xxxx ptr, +0x60 == 0x1601 magic) — never a resource root.
    if ru64(&v, 0x60) == 0x1601 {
        return Err("Mid-resource geometry chunk (not a RAGE9 resource root)".into());
    }
    let Some(bmap_off) = vptr(ru64(&v, 8)) else {
        return Err("Not a valid RAGE9 resource (missing BlockMap)".into());
    };
    let mut v_chunks_read = 1usize;
    while v.len() <= bmap_off + 10 && v_chunks_read < 32 {
        let Some(zoff) = find_next_zlib(f, next_off).map_err(|e| e.to_string())? else { break };
        let (chunk, nxt) = decompress_zlib_at(f, zoff, 16 * 1024 * 1024).map_err(|e| e.to_string())?;
        v.extend_from_slice(&chunk);
        next_off = nxt;
        v_chunks_read += 1;
    }
    if bmap_off + 10 > v.len() {
        return Err("Invalid RAGE9 BlockMap offset".into());
    }
    let v_cnt = v[bmap_off + 8] as usize;
    let p_cnt = v[bmap_off + 9] as usize;
    // Meta-only 0x7a resources legitimately use p_cnt = 0 (no graphics pages).
    if !(1..=32).contains(&v_cnt) || p_cnt > 64 {
        return Err(format!("Invalid RAGE9 BlockMap counts (v={v_cnt}, p={p_cnt})"));
    }
    for _ in v_chunks_read..v_cnt {
        let Some(zoff) = find_next_zlib(f, next_off).map_err(|e| e.to_string())? else { break };
        let (chunk, nxt) = decompress_zlib_at(f, zoff, 16 * 1024 * 1024).map_err(|e| e.to_string())?;
        v.extend_from_slice(&chunk);
        next_off = nxt;
    }
    Ok((v, next_off, p_cnt))
}

// finds dr ft dd modle string in virtual buffr (also checks drawable +0xa8 name ptr!)
pub fn extract_rage9_model_name(v: &[u8]) -> Option<(String, &'static str)> {
    let mut i = 1usize;
    while i + 2 < v.len() {
        if v[i] == b'#' && v[i - 1] == b'.' {
            let ext = match (v[i + 1], v[i + 2]) {
                (b'd', b'r') => ".#dr",
                (b'f', b't') => ".#ft",
                (b'd', b'd') => ".#dd",
                _ => {
                    i += 1;
                    continue;
                }
            };
            let dot = i - 1;
            let mut start = dot;
            while start > 0 {
                let c = v[start - 1];
                if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' {
                    start -= 1;
                } else {
                    break;
                }
            }
            if dot - start >= 2 && dot - start <= 96 {
                let stem = String::from_utf8_lossy(&v[start..dot]);
                return Some((format!("{stem}{ext}"), ext));
            }
        }
        i += 1;
    }
    // Fallback: RAGE9 drawable name pointer at +0xa8 (catches 3,000+ models without .#dr suffix!)
    if v.len() >= 0xb0 {
        if let Some(no) = vptr(ru64(v, 0xa8)) {
            if no < v.len() {
                if let Some(rel) = v[no..].iter().position(|&b| b == 0) {
                    if (2..=96).contains(&rel) {
                        let s = &v[no..no + rel];
                        if s.iter().all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'#'))
                            && s.iter().any(|b| b.is_ascii_alphabetic())
                        {
                            let nm = String::from_utf8_lossy(s).trim_end_matches('.').to_string();
                            if nm.len() >= 2 {
                                let ext = if nm.ends_with(".#ft") { ".#ft" } else { ".#dr" };
                                let full = if nm.contains(".#") { nm } else { format!("{nm}.#dr") };
                                return Some((full, ext));
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

// checks if virtual resource is an actual vehicle and not a small tuning part
pub fn is_vehicle_virtual(v: &[u8], name: &str) -> bool {
    let lo = name.to_ascii_lowercase();
    if !lo.ends_with(".#ft") || v.len() < 28_000 {
        return false;
    }
    for part in [
        "_dmg_", "_livery", "_roof_", "_roofext_", "_spoiler", "_exhaust",
        "_bumf_", "_bumr_", "_skirt_", "_bonnet", "_wing_", "_door_",
        "_mirror", "_lights_", "_intakes_", "_platemount", "_towbar",
        "_speakermount", "_deliverybag", "_antennaset", "_radar_", "_horn",
        "_outboard", "_inboard", "_mast", "_mount", "_turret", "_weap_",
        "_wheeliebar", "_induction_", "_whiplight", "_rack_", "_sign_mesh",
        "_armour_plate", "_fairing_", "_rescuehoist", "_footboards", "_callsign",
        "_shorad", "_badge", "_trim", "_overlay",
    ] {
        if lo.contains(part) {
            return false;
        }
    }
    [b"wheel_lf".as_slice(), b"door_dside_f", b"chassis", b"bodyshell", b"cockpit", b"rudder", b"rotor_main"]
        .iter()
        .any(|needle| v.windows(needle.len()).any(|w| w == *needle))
}

// zero seek in memory probe falls back to disk if stream crosses chunk boundry
pub fn probe_large_rage9_model_in_buf(buf: &[u8], rpf_path: &Path, abs_off: u64) -> Option<(String, bool, usize)> {
    let mut dec = Decompress::new(true);
    let mut v = Vec::with_capacity(131072);
    let mut pos = match decompress_zlib_slice(buf, &mut dec, &mut v) {
        Some(n) => n,
        None => return probe_large_rage9_model(rpf_path, abs_off),
    };
    if v.len() < 0x80 || ru64(&v, 0x60) == 0x1601 { return None; }
    let bmap_off = vptr(ru64(&v, 8))?;
    if bmap_off > 4 * 1024 * 1024 { return None; }

    let mut v_chunks_read = 1usize;
    while v.len() <= bmap_off + 10 && v_chunks_read < 32 {
        while pos + 3 < buf.len() && !is_zlib_head(&buf[pos..]) {
            pos += 1;
        }
        if pos + 3 >= buf.len() {
            return probe_large_rage9_model(rpf_path, abs_off);
        }
        match decompress_zlib_slice(&buf[pos..], &mut dec, &mut v) {
            Some(n) => {
                pos += n;
                v_chunks_read += 1;
            }
            None => return probe_large_rage9_model(rpf_path, abs_off),
        }
    }
    if bmap_off + 10 > v.len() { return None; }
    let v_cnt = v[bmap_off + 8] as usize;
    let p_cnt = v[bmap_off + 9] as usize;
    if !(1..=32).contains(&v_cnt) || !(1..=64).contains(&p_cnt) {
        return None;
    }
    while v_chunks_read < v_cnt {
        while pos + 3 < buf.len() && !is_zlib_head(&buf[pos..]) {
            pos += 1;
        }
        if pos + 3 >= buf.len() {
            return probe_large_rage9_model(rpf_path, abs_off);
        }
        match decompress_zlib_slice(&buf[pos..], &mut dec, &mut v) {
            Some(n) => {
                pos += n;
                v_chunks_read += 1;
            }
            None => return probe_large_rage9_model(rpf_path, abs_off),
        }
    }
    if !has_rage9_meshlets(&v) {
        return None;
    }
    let (name, _) = extract_rage9_model_name(&v)?;
    Some((name.clone(), is_vehicle_virtual(&v, &name), v.len()))
}

pub fn probe_large_rage9_model(rpf_path: &Path, file_off: u64) -> Option<(String, bool, usize)> {
    let mut f = File::open(rpf_path).ok()?;
    let (v, _, _) = read_virtual_from_file(&mut f, file_off).ok()?;
    if !has_rage9_meshlets(&v) {
        return None;
    }
    let (name, _) = extract_rage9_model_name(&v)?;
    Some((name.clone(), is_vehicle_virtual(&v, &name), v.len()))
}

pub fn has_rage9_meshlets(v: &[u8]) -> bool {
    let mut off = 0x28usize;
    while off + 0x10 <= v.len() {
        if ru64(v, off - 8) == 0x1601
            && pptr(ru64(v, off)).is_some()
            && resolve_meshlet_buffer(v, ru64(v, off - 0x28)).is_some()
        {
            return true;
        }
        off += 8;
    }
    false
}

// shared meshlet link resolver so lod0 and fallback scanner stay dry
#[inline]
fn resolve_meshlet_buffer(v: &[u8], link_ptr: u64) -> Option<(usize, usize)> {
    let lo = vptr(link_ptr)?;
    if lo + 8 > v.len() { return None; }
    let mo = vptr(ru64(v, lo))?;
    if mo + 0x20 > v.len() { return None; }
    let mb_poff = pptr(ru64(v, mo + 0x18))?;
    let mb_words = ru32(v, mo + 0x08) as usize;
    Some((mb_poff, mb_words * 4))
}

/// One meshlet geometry: (vertex buffer phys offset, vertex buffer bytes, meshlet buffer phys offset, meshlet buffer bytes).
type GeomRef = (usize, usize, usize, usize);

/// Drawables keep four LOD slots (high, medium, low, very low) at +0x50/+0x58/+0x60/+0x68.
/// LOD selection ported from asm512's PercyStar fork (rpatty-dev/PercyStar#19).
pub const MAX_LODS: usize = 4;
pub const LOD_NAMES: [&str; MAX_LODS] = ["LOD0", "LOD1", "LOD2", "LOD3"];

/// Meshlet geometries of one LOD level (0 = high detail) of the drawable at `base`.
fn drawable_lod_geoms(v: &[u8], base: usize, lod: usize) -> Vec<GeomRef> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    if lod >= MAX_LODS || base + 0x80 > v.len() { return out; }
    let Some(lod0_off) = vptr(ru64(v, base + 0x50 + lod * 8)) else { return out };
    if lod0_off + 0x10 > v.len() { return out; }
    let Some(models_off) = vptr(ru64(v, lod0_off)) else { return out };
    let model_cnt = ru16(v, lod0_off + 8) as usize;
    if model_cnt == 0 || model_cnt > 128 || models_off + model_cnt * 8 > v.len() { return out; }

    for mi in 0..model_cnt {
        let Some(mp) = vptr(ru64(v, models_off + mi * 8)) else { continue };
        if mp + 0x18 > v.len() { continue; }
        let Some(garr) = vptr(ru64(v, mp + 0x08)) else { continue };
        let gcnt = ru16(v, mp + 0x10) as usize;
        if gcnt == 0 || gcnt > 256 || garr + gcnt * 8 > v.len() { continue; }

        for gi in 0..gcnt {
            let Some(gp) = vptr(ru64(v, garr + gi * 8)) else { continue };
            if gp + 0x70 > v.len() || ru64(v, gp + 0x60) != 0x1601 { continue; }
            let Some(vb_poff) = pptr(ru64(v, gp + 0x68)) else { continue };
            let Some((mb_poff, mb_bytes)) = resolve_meshlet_buffer(v, ru64(v, gp + 0x40)) else { continue };
            let vb_bytes = (ru32(v, gp + 0x58) as usize) * 4;
            if seen.insert((vb_poff, mb_poff)) {
                out.push((vb_poff, vb_bytes, mb_poff, mb_bytes));
            }
        }
    }
    out
}

fn collect_all_geoms(v: &[u8]) -> Vec<GeomRef> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut off = 0x28usize;
    while off + 0x10 <= v.len() {
        if ru64(v, off - 8) == 0x1601 {
            if let Some(vb_poff) = pptr(ru64(v, off)) {
                if let Some((mb_poff, mb_bytes)) = resolve_meshlet_buffer(v, ru64(v, off - 0x28)) {
                    let vb_bytes = (ru32(v, off - 16) as usize) * 4;
                    if seen.insert((vb_poff, mb_poff)) {
                        out.push((vb_poff, vb_bytes, mb_poff, mb_bytes));
                    }
                }
            }
        }
        off += 8;
    }
    out
}

pub fn model_detail_score(file: &mut File, offset: u64) -> Option<usize> {
    let (v, _, _) = read_virtual_from_file(file, offset).ok()?;
    let mut geoms = drawable_lod_geoms(&v, 0, 0);
    if geoms.is_empty() {
        geoms = collect_all_geoms(&v);
    }
    // Meshlet buffer sizes distinguish variants without reading GPU buffers or textures.
    let bytes = geoms.iter().map(|g| g.3).sum();
    (bytes > 0).then_some(bytes)
}

/// Maps each `(vb_poff, mb_poff)` of one LOD to its shader index and owning model's render mask.
fn drawable_lod_shader_map(v: &[u8], base: usize, lod: usize) -> HashMap<(usize, usize), (u16, u8)> {
    let mut out = HashMap::new();
    if lod >= MAX_LODS || base + 0x80 > v.len() { return out; }
    let Some(lod0_off) = vptr(ru64(v, base + 0x50 + lod * 8)) else { return out };
    if lod0_off + 0x10 > v.len() { return out; }
    let Some(models_off) = vptr(ru64(v, lod0_off)) else { return out };
    let model_cnt = ru16(v, lod0_off + 8) as usize;
    if model_cnt == 0 || model_cnt > 128 || models_off + model_cnt * 8 > v.len() { return out; }

    for mi in 0..model_cnt {
        let Some(mp) = vptr(ru64(v, models_off + mi * 8)) else { continue };
        if mp + 0x28 > v.len() { continue; }
        let Some(garr) = vptr(ru64(v, mp + 0x08)) else { continue };
        let gcnt = ru16(v, mp + 0x10) as usize;
        if gcnt == 0 || gcnt > 256 || garr + gcnt * 8 > v.len() { continue; }
        let smap_off = vptr(ru64(v, mp + 0x20)).filter(|&so| so + gcnt * 2 <= v.len());
        let render_mask = v.get(mp + 0x34).copied().unwrap_or(0xff);

        for gi in 0..gcnt {
            let Some(gp) = vptr(ru64(v, garr + gi * 8)) else { continue };
            if gp + 0x70 > v.len() || ru64(v, gp + 0x60) != 0x1601 { continue; }
            let Some(vb_poff) = pptr(ru64(v, gp + 0x68)) else { continue };
            let Some((mb_poff, _)) = resolve_meshlet_buffer(v, ru64(v, gp + 0x40)) else { continue };
            let sidx = smap_off.map(|so| ru16(v, so + gi * 2)).unwrap_or(gi as u16);
            // Shared GPU buffers can be referenced by both passes; prefer the visible binding.
            let binding = out.entry((vb_poff, mb_poff)).or_insert((sidx, render_mask));
            if binding.1 & 1 == 0 && render_mask & 1 != 0 {
                *binding = (sidx, render_mask);
            }
        }
    }
    out
}

// texture parameters of the shader at saddr as param name hash and bound 64 bit texture hash none when the slot is empty
fn shader_texture_params(v: &[u8], saddr: usize) -> Vec<(u32, Option<u64>)> {
    let mut out = Vec::new();
    if saddr + 0x28 > v.len() { return out; }
    let (Some(p2), Some(p4)) = (vptr(ru64(v, saddr + 0x10)), vptr(ru64(v, saddr + 0x20))) else { return out };
    if p4 + 8 > v.len() { return out; }
    let tcnt = (v[p4 + 1] as usize).min(48);
    for k in 0..tcnt {
        let ent = p4 + 8 + k * 8;
        if ent + 8 > v.len() { break; }
        let param_hash = ru32(v, ent);
        let slot = (ru32(v, ent + 4) / 4) as usize;
        let tex_hash = (|| {
            if p2 + slot * 8 + 8 > v.len() { return None; }
            let tp = vptr(ru64(v, p2 + slot * 8))?;
            if tp + 0x40 > v.len() || ru64(v, tp + 0x10) != 0x980 { return None; }
            let ref_off = vptr(ru64(v, tp + 0x30))?;
            if ref_off + 16 > v.len() { return None; }
            Some(ru64(v, ref_off + 8))
        })();
        out.push((param_hash, tex_hash));
    }
    out
}

// shader addresses of the shader group of drawable at base
fn drawable_shader_addrs(v: &[u8], base: usize) -> Vec<usize> {
    if base + 0x18 > v.len() { return Vec::new(); }
    let Some(sg) = vptr(ru64(v, base + 0x10)) else { return Vec::new() };
    if sg + 0x20 > v.len() { return Vec::new(); }
    let Some(sp) = vptr(ru64(v, sg + 0x10)) else { return Vec::new() };
    let scnt = ru16(v, sg + 0x18) as usize;
    if scnt == 0 || scnt > 512 || sp + scnt * 8 > v.len() { return Vec::new(); }
    // verify rage9 atem meta shader table tag
    let Some(s0) = vptr(ru64(v, sp)) else { return Vec::new() };
    if s0 + 8 > v.len() || (ru64(v, s0) >> 32) != 0x6d65_7461 { return Vec::new(); }
    (0..scnt).filter_map(|i| vptr(ru64(v, sp + i * 8))).collect()
}

// known shader texture parameter names by joaat hash
pub fn texture_param_name(param_hash: u32) -> Option<&'static str> {
    Some(match param_hash {
        0xc8e8_c282 => "diffuseTex",
        0x2c8d_7182 => "tintPaletteTex",
        0x8e0d_63ee => "emissiveTex",
        0x7485_dafb => "dirtDiffuseTex",
        0xcc15_5f4b => "dirtNoiseTex",
        0xfb03_ae36 => "DustTexture",
        _ => return None,
    })
}

// one external texture a model binds in its shaders
#[derive(Clone, Debug)]
pub struct ModelTexRef {
    pub hash64: u64,
    // shader parameters this texture is bound to
    pub params: Vec<u32>,
    // number of shaders that bind it
    pub shaders: usize,
    // location in the ttpf pools none when no pool holds this hash
    pub hit: Option<crate::tex::TtpfHashHit>,
}

impl ModelTexRef {
    // short role label from the parameter name or the pixel format
    pub fn role(&self) -> String {
        if let Some(n) = self.params.iter().find_map(|&p| texture_param_name(p)) {
            return n.to_string();
        }
        let kind = match &self.hit {
            Some(h) if h.is_normal_format() => "normal",
            Some(h) if h.is_srgb() => "color",
            Some(_) => "linear",
            None => "param",
        };
        format!("{kind}_{:08x}", self.params.first().copied().unwrap_or(0))
    }
}

// every external texture bound by the shaders of the model at file_off in first use order
pub fn list_model_texture_refs(rpf_path: &Path, file_off: u64) -> Result<Vec<ModelTexRef>, String> {
    let mut f = File::open(rpf_path).map_err(|e| format!("Open error: {e}"))?;
    let (v, _, _) = read_virtual_from_file(&mut f, file_off)?;
    let mut bases: Vec<usize> = drawable_dictionary_entries(&v).into_iter().map(|(_, b)| b).collect();
    if bases.is_empty() {
        bases.push(0);
        if v.len() >= 0x20 {
            if let Some(fo) = vptr(ru64(&v, 0x18)) {
                bases.push(fo);
            }
        }
    }
    let idx = crate::tex::ttpf_hash_index_for_rpf(rpf_path);
    let mut out: Vec<ModelTexRef> = Vec::new();
    let mut pos: HashMap<u64, usize> = HashMap::new();
    let mut seen_shaders = HashSet::new();
    for base in bases {
        for saddr in drawable_shader_addrs(&v, base) {
            if !seen_shaders.insert(saddr) { continue; }
            let mut in_shader = HashSet::new();
            for (param, tex_hash) in shader_texture_params(&v, saddr) {
                let Some(h64) = tex_hash else { continue };
                let i = *pos.entry(h64).or_insert_with(|| {
                    out.push(ModelTexRef {
                        hash64: h64,
                        params: Vec::new(),
                        shaders: 0,
                        hit: idx.as_ref().and_then(|m| m.get(&h64).cloned()),
                    });
                    out.len() - 1
                });
                if !out[i].params.contains(&param) { out[i].params.push(param); }
                if in_shader.insert(h64) { out[i].shaders += 1; }
            }
        }
    }
    Ok(out)
}

/// Composite a texture over an off-white eyeball color. `Color32` is premultiplied.
fn over_sclera(t: &Arc<Texture2D>) -> Arc<Texture2D> {
    const SCLERA: [u16; 3] = [236, 232, 226];
    if t.pixels.iter().all(|p| p.a() == 255) {
        return t.clone();
    }
    let pixels = t
        .pixels
        .iter()
        .map(|p| {
            let a = p.a() as u16;
            let mix = |c: u8, bg: u16| (c as u16 + (bg * (255 - a) + 127) / 255).min(255) as u8;
            Color32::from_rgb(mix(p.r(), SCLERA[0]), mix(p.g(), SCLERA[1]), mix(p.b(), SCLERA[2]))
        })
        .collect();
    Arc::new(Texture2D { w: t.w, h: t.h, pixels })
}

/// Colour texture a shader binds from the drawable's own embedded dictionary, with its
/// priority rank (0 = diffuseTex). Ped parts, props and small models embed their textures
/// instead of streaming them from the .ttpf pools.
fn embedded_shader_color(
    v: &[u8],
    shader: usize,
    embedded: &HashMap<usize, (u8, Arc<Texture2D>)>,
    uses_opacity: bool,
) -> Option<(usize, Arc<Texture2D>)> {
    const DIFFUSE_TEX: u32 = 0xc8e8_c282; // joaat("diffuseTex")
    const DIFFUSE_TEX2: u32 = 0x0df4_7048; // joaat("diffuseTex2")
    const ALT0_TEX: u32 = 0x3463_94ea; // vehicle body/livery/detail slot 0
    const EMISSIVE_TEX: u32 = 0x8e0d_63ee; // joaat("emissiveTex")
    if embedded.is_empty() || shader + 0x28 > v.len() { return None; }
    let params = vptr(ru64(v, shader + 0x20))?;
    let slots = vptr(ru64(v, shader + 0x10))?;
    if params + 8 > v.len() { return None; }
    let mut best: Option<(usize, Arc<Texture2D>)> = None;
    for k in 0..v[params + 1] as usize {
        let ent = params + 8 + k * 8;
        if ent + 8 > v.len() { break; }
        let rank = match ru32(v, ent) {
            DIFFUSE_TEX => 0,
            ALT0_TEX => 1,
            DIFFUSE_TEX2 => 2,
            EMISSIVE_TEX => 3,
            _ => continue, // An sRGB format alone does not make a map an albedo.
        };
        let code = ru32(v, ent + 4) as usize;
        if code % 4 != 0 || slots + code * 2 + 8 > v.len() { continue; }
        let Some(tp) = vptr(ru64(v, slots + code * 2)) else { continue };
        let Some((dxgi, tex)) = embedded.get(&tp) else { continue };
        let usable = !matches!(dxgi, 65 | 80 | 81 | 83 | 84) || (*dxgi == 65 && uses_opacity && rank == 0);
        if usable && best.as_ref().is_none_or(|(r, _)| rank < *r) {
            best = Some((rank, tex.clone()));
        }
    }
    best
}

/// Bind each geometry to its shader's textures: embedded ones directly, pooled ones as
/// switchable layers (upstream's layer system from asm512).
fn resolve_drawable_materials(
    v: &[u8],
    p: &[u8],
    base: usize,
    lod: usize,
    geoms: &[GeomRef],
    ttpf_idx: Option<&HashMap<u64, crate::tex::TtpfHashHit>>,
    is_vehicle: bool,
    layers: &mut Vec<TexLayer>,
    layer_pos: &mut HashMap<u64, u16>,
) -> Vec<GeomMaterial> {
    const DIFFUSE_TEX: u32 = 0xc8e8_c282; // diffuseTex
    const TINT_PALETTE_TEX: u32 = 0x2c8d_7182; // tintPaletteTex
    const ALT0_TEX: u32 = 0x3463_94ea; // vehicle body livery detail slot 0
    const EMISSIVE_TEX: u32 = 0x8e0d_63ee; // emissiveTex
    const DIRT_DIFFUSE_TEX: u32 = 0x7485_dafb; // dirtDiffuseTex
    const SHARED_LUT_PARAM: u32 = 0x19df_25d7; // shared 128x256 vehicle lookup texture
    const GLASS_SHADER: u32 = 0xe3ba_3129;
    const EYE_SHADER: u32 = 0x89a1_cf72; // ped and animal eyeballs

    let mut mats = vec![GeomMaterial::default(); geoms.len()];
    if base + 0x18 > v.len() { return mats; }
    let Some(sg) = vptr(ru64(v, base + 0x10)) else { return mats };
    if sg + 0x20 > v.len() { return mats; }
    let Some(sp) = vptr(ru64(v, sg + 0x10)) else { return mats };
    let scnt = ru16(v, sg + 0x18) as usize;
    if scnt == 0 || scnt > 512 || sp + scnt * 8 > v.len() { return mats; }
    let embedded: HashMap<_, _> = vptr(ru64(v, sg + 8))
        .into_iter()
        .flat_map(|txd| crate::tex::extract_txd_textures_by_ptr(v, p, txd))
        .map(|(ptr, tex)| (ptr, (tex.dxgi, Arc::new(crate::tex::to_model_tex(tex)))))
        .collect();
    // verify rage9 atem meta shader table tag
    let Some(s0) = vptr(ru64(v, sp)) else { return mats };
    if s0 + 8 > v.len() || (ru64(v, s0) >> 32) != 0x6d65_7461 { return mats; }

    let smap = drawable_lod_shader_map(v, base, lod);
    let has_main_pass = smap.values().any(|&(_, mask)| mask & 1 != 0);
    let mut shader_cache: HashMap<u16, GeomMaterial> = HashMap::new();

    for (gi, &(vb_poff, _, mb_poff, _)) in geoms.iter().enumerate() {
        let Some((sidx, mask)) = smap.get(&(vb_poff, mb_poff)).copied().or_else(|| (scnt == 1).then_some((0, 0xff))) else { continue };
        // Keep shadow proxies in raw mesh modes and exports, not the material preview.
        if has_main_pass && mask & 1 == 0 {
            mats[gi].auxiliary_pass = true;
            continue;
        }
        if sidx as usize >= scnt { continue; }
        if let Some(cached) = shader_cache.get(&sidx) {
            mats[gi] = cached.clone();
            continue;
        }
        let mut mat = GeomMaterial::default();
        if (sidx as usize) < scnt {
            if let Some(saddr) = vptr(ru64(v, sp + (sidx as usize) * 8)) {
                if saddr + 0x28 <= v.len() {
                    mat.pass = match v.get(saddr + 0x3c) {
                        Some(2) if ru32(v, saddr) == GLASS_SHADER => MaterialPass::Glass { texture_alpha: false },
                        Some(2) => MaterialPass::Alpha,
                        Some(4) => MaterialPass::Decal,
                        Some(8) => MaterialPass::Cutout,
                        _ => MaterialPass::Opaque,
                    };
                    let mut has_tint = false;
                    let mut diffuse_cand: Option<u16> = None;
                    let mut alt0_cand: Option<u16> = None;
                    let mut emissive_cand: Option<u16> = None;
                    let mut fallback_srgb: Option<u16> = None;
                    let uses_opacity = !matches!(mat.pass, MaterialPass::Opaque);
                    let mut layer_of = |h64: u64, hit: &crate::tex::TtpfHashHit, param: u32, kind: TexLayerKind| -> Option<u16> {
                        if let Some(&i) = layer_pos.get(&h64) {
                            return Some(i);
                        }
                        let i = u16::try_from(layers.len()).ok()?;
                        let label = match (texture_param_name(param), kind) {
                            (Some(n), _) => n.to_string(),
                            (None, _) if param == ALT0_TEX => "liveryDetail".to_string(),
                            (None, TexLayerKind::Color) => format!("color_{param:08x}"),
                            (None, TexLayerKind::Normal) => format!("normal_{param:08x}"),
                            (None, TexLayerKind::Overlay) => format!("overlay_{param:08x}"),
                        };
                        layers.push(TexLayer {
                            hash64: h64,
                            label,
                            kind,
                            w: hit.w,
                            h: hit.h,
                            source: format!("{} #{}", hit.path.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default(), hit.tex_idx),
                            mats: 0,
                            enabled: false,
                            default_on: false,
                            hit: hit.clone(),
                            tex: None,
                            failed: false,
                        });
                        layer_pos.insert(h64, i);
                        Some(i)
                    };

                    for (param_hash, tex_hash) in shader_texture_params(v, saddr) {
                        if param_hash == TINT_PALETTE_TEX {
                            has_tint = true;
                        }
                        let Some(idx_map) = ttpf_idx else { continue };
                        let Some(h64) = tex_hash else { continue };
                        let Some(hit) = idx_map.get(&h64) else { continue };
                        let big = hit.w >= 16 && hit.h >= 16;
                        if hit.is_normal_format() {
                            if big {
                                if let Some(i) = layer_of(h64, hit, param_hash, TexLayerKind::Normal) {
                                    if !mat.normal_layers.contains(&i) { mat.normal_layers.push(i); }
                                }
                            }
                            continue;
                        }
                        if hit.dxgi == 65 && !(uses_opacity && param_hash == DIFFUSE_TEX) { continue; }

                        if param_hash == DIFFUSE_TEX && hit.w >= 4 && hit.h >= 4 {
                            diffuse_cand = layer_of(h64, hit, param_hash, TexLayerKind::Color);
                        } else if param_hash == ALT0_TEX && big {
                            alt0_cand = layer_of(h64, hit, param_hash, TexLayerKind::Color);
                        } else if param_hash == EMISSIVE_TEX && big {
                            emissive_cand = layer_of(h64, hit, param_hash, TexLayerKind::Color);
                        } else if hit.is_srgb()
                            && big
                            && param_hash != DIRT_DIFFUSE_TEX
                            && param_hash != SHARED_LUT_PARAM
                            && fallback_srgb.is_none()
                        {
                            fallback_srgb = layer_of(h64, hit, param_hash, TexLayerKind::Color);
                        } else if big && param_hash != SHARED_LUT_PARAM {
                            if let Some(i) = layer_of(h64, hit, param_hash, TexLayerKind::Overlay) {
                                if !mat.overlay_layers.contains(&i) { mat.overlay_layers.push(i); }
                            }
                        }
                    }

                    // color candidates in priority order tinted vehicle bodies only take diffuse or livery detail
                    let tinted_body = has_tint && is_vehicle;
                    let extra = if tinted_body { [None, None] } else { [emissive_cand, fallback_srgb] };
                    for i in [diffuse_cand, alt0_cand].into_iter().chain(extra).flatten() {
                        if !mat.color_layers.contains(&i) { mat.color_layers.push(i); }
                    }
                    // a texture already used as color or normal is not also multiplied over the same material
                    mat.overlay_layers.retain(|i| !mat.color_layers.contains(i) && !mat.normal_layers.contains(i));
                    // An embedded texture wins unless a pooled one has a better rank.
                    if let Some((rank, tex)) = embedded_shader_color(v, saddr, &embedded, uses_opacity) {
                        let pooled_better = (rank > 0 && diffuse_cand.is_some())
                            || (rank > 1 && alt0_cand.is_some())
                            || (rank > 3 && emissive_cand.is_some());
                        let allowed = !tinted_body || rank <= 1;
                        if !pooled_better && allowed {
                            mat.embedded_color = Some(tex);
                        }
                    }
                    mat.eye = ru32(v, saddr) == EYE_SHADER;
                    mat.paint_capable = tinted_body && mat.pass == MaterialPass::Opaque && mat.embedded_color.is_none();
                }
            }
        }
        shader_cache.insert(sidx, mat.clone());
        mats[gi] = mat;
    }
    mats
}

/// One drawable inside a decoded resource. Plain drawables have a single part;
/// drawable dictionaries (pgDictionary<drawable>) have one per entry.
#[derive(Clone)]
pub struct MeshPart {
    pub name: String,
    pub geoms: usize,
    /// Triangle range in `Mesh3D::tris` (also indexes `tri_geom` / `normals`).
    pub tris: std::ops::Range<usize>,
}

/// Detects a RAGE9 drawable dictionary root and returns `(name, drawable_offset)` per entry.
///
/// Layout (pgDictionary): +0x20 hash array {ptr, u16 count}, +0x30 drawable ptr array
/// {ptr, u16 count}. Each entry is an independent drawable modelled around its own
/// origin, so entries must NOT be merged into one mesh (that stacks them on top of each
/// other). Map placement lives in ymap entities, which this resource does not carry.
fn drawable_dictionary_entries(v: &[u8]) -> Vec<(String, usize)> {
    if v.len() < 0x40 { return Vec::new(); }
    let (Some(hashes), Some(arr)) = (vptr(ru64(v, 0x20)), vptr(ru64(v, 0x30))) else { return Vec::new() };
    let cnt = ru16(v, 0x38) as usize;
    if !(2..=4096).contains(&cnt) || ru16(v, 0x28) as usize != cnt || hashes + cnt * 4 > v.len() || arr + cnt * 8 > v.len() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(cnt);
    for i in 0..cnt {
        let Some(d) = vptr(ru64(v, arr + i * 8)) else { return Vec::new() };
        if d + 0xb0 > v.len() || vptr(ru64(v, d + 0x50)).is_none() { return Vec::new(); }
        let name = vptr(ru64(v, d + 0xa8))
            .and_then(|n| {
                let len = v.get(n..)?.iter().take(128).position(|&b| b == 0)?;
                let s = &v[n..n + len];
                (len >= 2 && s.iter().all(|b| b.is_ascii_graphic())).then(|| String::from_utf8_lossy(s).into_owned())
            })
            .unwrap_or_else(|| format!("0x{:08x}", ru32(v, hashes + i * 4)));
        out.push((name, d));
    }
    out
}

/// The drawable inside a pgDictionary that holds exactly one entry (same layout as
/// [`drawable_dictionary_entries`]). Such files have no shader group at the root.
fn single_dictionary_drawable(v: &[u8]) -> Option<usize> {
    if v.len() < 0x40 || ru16(v, 0x28) != 1 || ru16(v, 0x38) != 1 { return None; }
    let hashes = vptr(ru64(v, 0x20))?;
    let arr = vptr(ru64(v, 0x30))?;
    v.get(hashes..hashes + 4)?;
    v.get(arr..arr + 8)?;
    let d = vptr(ru64(v, arr))?;
    (d != 0 && d + 0xb0 <= v.len() && vptr(ru64(v, d + 0x50)).is_some()).then_some(d)
}

#[derive(Default)]
struct MeshBuild {
    verts: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    tris: Vec<[u32; 3]>,
    tri_geom: Vec<u16>,
    meshlets: usize,
}

/// Decodes one meshlet geometry and returns whether it has usable UV0 data.
fn append_meshlet_geom(p: &[u8], (vb_off, vb_size, mb_off, _mb_size): GeomRef, geom_id: u16, b: &mut MeshBuild) -> bool {
    if mb_off + 0x10 > p.len() || vb_off + 0x80 > p.len() {
        return false;
    }
    let num_meshlets = (ru32(p, mb_off) & 0xFFFF) as usize;
    let tot_tris = ru32(p, mb_off + 4) as usize;
    let tot_vrefs = ru32(p, mb_off + 8) as usize;
    if num_meshlets == 0 || tot_tris == 0 || tot_vrefs == 0 {
        return false;
    }
    let tri_base = mb_off + 0x0c + num_meshlets * 16;
    let remap_base = tri_base + tot_tris * 4;
    if remap_base + tot_vrefs * 2 > p.len() {
        return false;
    }

    // Float3 positions (format 99) go through upstream's stream decoder (v1.3.8);
    // the packed 24/16-bit format below stays on the path the regressions cover.
    if vb_off + 4 <= p.len() && ru32(p, vb_off) & 127 == 99 {
        let Some(vs) = vb_off.checked_add(vb_size).and_then(|end| p.get(vb_off..end)).and_then(|bytes| vertex_stream::decode(bytes).ok()) else {
            return false;
        };
        return append_meshlet_tris(p, mb_off, num_meshlets, tot_tris, tot_vrefs, vs.positions.len(), geom_id, b, |b, v_cnt| {
            b.verts.extend_from_slice(&vs.positions[..v_cnt]);
            match &vs.uvs {
                Some(uvs) => {
                    b.uvs.extend_from_slice(&uvs[..v_cnt]);
                    true
                }
                None => {
                    b.uvs.resize(b.uvs.len() + v_cnt, [0.0; 2]);
                    false
                }
            }
        });
    }

    let mut hdrs = [0u32; 8];
    for (k, h) in hdrs.iter_mut().enumerate() {
        if vb_off + k * 4 + 4 <= p.len() {
            *h = ru32(p, vb_off + k * 4);
        }
    }
    let stream0_off = ((hdrs[0] >> 8) << 1) as usize;
    let mut stream0_end = vb_size;
    let mut uv_stream = None;
    for &h in &hdrs[1..] {
        if h == 0 { continue; }
        let so = ((h >> 8) << 1) as usize;
        let st = h & 0xff;
        if so > stream0_off && so < stream0_end {
            stream0_end = so;
        }
        if so > stream0_off && uv_stream.is_none() {
            // The first descriptor is packed position. Later 0x42 streams are float2 UVs;
            // 0x28 stores signed fixed-point UVs, including coordinates below zero.
            match st {
                0x28 => uv_stream = Some((so, 4)),
                0x42 => uv_stream = Some((so, 8)),
                _ => {}
            }
        }
    }
    if vb_off + stream0_off >= p.len() {
        return false;
    }
    let max_vb_verts = ((stream0_end.saturating_sub(stream0_off)) / 8).min((p.len() - (vb_off + stream0_off)) / 8);
    if max_vb_verts == 0 {
        return false;
    }

    let ox = rf32(p, vb_off + 0x60);
    let oy = rf32(p, vb_off + 0x64);
    let oz = rf32(p, vb_off + 0x68);
    let sx = rf32(p, vb_off + 0x70);
    let sy = rf32(p, vb_off + 0x74);
    let sz = rf32(p, vb_off + 0x78);
    // +0x6C picks which world axis sits in the 16-bit slot (the shortest extent); the two
    // 24-bit slots follow. Verified against drawable bboxes on 1.6k geoms (levels.rpf):
    //   0 -> slots (z, y, x)   1 -> slots (z, x, y)   2 -> slots (x, y, z)
    // Other values are a different vertex layout; keep the legacy (x, y, z) read for them.
    let slot_axis: [usize; 3] = match ru32(p, vb_off + 0x6c) {
        0 => [2, 1, 0],
        1 => [2, 0, 1],
        _ => [0, 1, 2],
    };

    append_meshlet_tris(p, mb_off, num_meshlets, tot_tris, tot_vrefs, max_vb_verts, geom_id, b, |b, v_cnt| {
        let uv_stream = uv_stream.filter(|&(off, stride)| {
            let end = hdrs.iter().map(|h| ((h >> 8) << 1) as usize)
                .filter(|&next| next > off)
                .fold(vb_size.min(p.len() - vb_off), usize::min);
            off + v_cnt * stride <= end
        });
        let mut has_uvs = uv_stream.is_some();
        let vbase = vb_off + stream0_off;
        b.verts.reserve(v_cnt);
        b.uvs.reserve(v_cnt);
        for i in 0..v_cnt {
            let o = vbase + i * 8;
            // packed rage9 24 bit xy and 16 bit z across 8 bytes
            let ix = (p[o + 1] as u32) | ((p[o + 2] as u32) << 8) | ((p[o + 3] as u32) << 16);
            let iy = (p[o] as u32) | ((p[o + 4] as u32) << 8) | ((p[o + 5] as u32) << 16);
            let iz = (p[o + 6] as u32) | ((p[o + 7] as u32) << 8);
            let slots = [ox + (ix as f32) * sx, oy + (iy as f32) * sy, oz + (iz as f32) * sz];
            let mut pos = [0.0f32; 3];
            for (slot, &axis) in slot_axis.iter().enumerate() {
                pos[axis] = slots[slot];
            }
            b.verts.push(pos);
            let uv = uv_stream
                .map(|(off, stride)| {
                    let uv = vb_off + off + i * stride;
                    if stride == 4 {
                        [ru16(p, uv) as i16 as f32 / 8192.0, ru16(p, uv + 2) as i16 as f32 / 8192.0]
                    } else {
                        [rf32(p, uv), rf32(p, uv + 4)]
                    }
                })
                .filter(|uv| uv.iter().all(|x| x.is_finite()));
            has_uvs &= uv.is_some();
            b.uvs.push(uv.unwrap_or([0.0, 0.0]));
        }
        has_uvs
    })
}

/// Appends one geometry's meshlet triangles (indices into `max_vb_verts` vertices), then lets
/// `push_verts` append the `v_cnt` vertices they use. Returns whether those had usable UVs.
#[allow(clippy::too_many_arguments)]
fn append_meshlet_tris(
    p: &[u8],
    mb_off: usize,
    num_meshlets: usize,
    tot_tris: usize,
    tot_vrefs: usize,
    max_vb_verts: usize,
    geom_id: u16,
    b: &mut MeshBuild,
    push_verts: impl FnOnce(&mut MeshBuild, usize) -> bool,
) -> bool {
    let tri_base = mb_off + 0x0c + num_meshlets * 16;
    let remap_base = tri_base + tot_tris * 4;
    let base_v = b.verts.len() as u32;
    let mut max_v_used = 0usize;
    let start_tri = b.tris.len();
    b.tris.reserve(tot_tris);

    for mi in 0..num_meshlets {
        let m_off = mb_off + 0x0c + mi * 16;
        let v_off = ru16(p, m_off + 4) as usize;
        let t_off = ru16(p, m_off + 6) as usize;
        let t_end = if mi + 1 < num_meshlets { ru16(p, mb_off + 0x0c + (mi + 1) * 16 + 6) as usize } else { tot_tris };
        for ti in t_off..t_end.min(tot_tris) {
            let w = ru32(p, tri_base + ti * 4);
            let r0 = v_off + (w & 0x7F) as usize;
            let r1 = v_off + ((w >> 7) & 0x7F) as usize;
            let r2 = v_off + ((w >> 14) & 0x7F) as usize;
            if r0 < tot_vrefs && r1 < tot_vrefs && r2 < tot_vrefs {
                let i0 = ru16(p, remap_base + r0 * 2) as usize;
                let i1 = ru16(p, remap_base + r1 * 2) as usize;
                let i2 = ru16(p, remap_base + r2 * 2) as usize;
                if i0 < max_vb_verts && i1 < max_vb_verts && i2 < max_vb_verts && i0 != i1 && i1 != i2 && i0 != i2 {
                    b.tris.push([base_v + i0 as u32, base_v + i1 as u32, base_v + i2 as u32]);
                    max_v_used = max_v_used.max(i0).max(i1).max(i2);
                }
            }
        }
    }

    let added_tris = b.tris.len() - start_tri;
    if added_tris == 0 {
        return false;
    }
    let v_cnt = (max_v_used + 1).min(max_vb_verts);
    let has_uvs = push_verts(b, v_cnt);
    b.tri_geom.resize(b.tri_geom.len() + added_tris, geom_id);
    b.meshlets += num_meshlets;
    has_uvs
}

/// Lays dictionary parts out on a square-ish XY grid so independent drawables sit side
/// by side instead of overlapping at the origin. Z (height) is left untouched.
fn layout_parts_in_grid(verts: &mut [[f32; 3]], part_verts: &[std::ops::Range<usize>]) {
    let n = part_verts.len();
    if n < 2 { return; }
    let bounds: Vec<([f32; 3], [f32; 3])> = part_verts
        .iter()
        .map(|r| {
            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
            for v in &verts[r.clone()] {
                for k in 0..3 {
                    lo[k] = lo[k].min(v[k]);
                    hi[k] = hi[k].max(v[k]);
                }
            }
            (lo, hi)
        })
        .collect();
    let cell = bounds
        .iter()
        .filter(|(lo, hi)| lo[0] <= hi[0])
        .map(|(lo, hi)| (hi[0] - lo[0]).max(hi[1] - lo[1]))
        .fold(1.0f32, f32::max)
        * 1.15;
    let cols = (n as f32).sqrt().ceil() as usize;
    for (i, (r, (lo, hi))) in part_verts.iter().zip(&bounds).enumerate() {
        if lo[0] > hi[0] { continue; }
        let tx = (i % cols) as f32 * cell - (lo[0] + hi[0]) * 0.5;
        let ty = -((i / cols) as f32) * cell - (lo[1] + hi[1]) * 0.5;
        for v in &mut verts[r.clone()] {
            v[0] += tx;
            v[1] += ty;
        }
    }
}

// reads rage9 crSkeletonData bone count from drawable+0x18 -> skel+0x6c
fn extract_rage9_bone_count(v: &[u8]) -> usize {
    let mut bases = vec![0usize];
    if v.len() >= 0x20 {
        if let Some(fo) = vptr(ru64(v, 0x18)) {
            if fo + 0x20 <= v.len() { bases.push(fo); }
        }
    }
    for base in bases {
        if base + 0x20 > v.len() { continue; }
        let Some(so) = vptr(ru64(v, base + 0x18)) else { continue };
        if so + 0x70 <= v.len() {
            let cnt = ru16(v, so + 0x6c) as usize;
            if (1..=1024).contains(&cnt) && vptr(ru64(v, so + 0x28)).is_some() {
                return cnt;
            }
        }
    }
    0
}

fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

fn quat_rotate(q: [f32; 4], p: [f32; 3]) -> [f32; 3] {
    let qv = [q[0], q[1], q[2]];
    let uv = [
        qv[1] * p[2] - qv[2] * p[1],
        qv[2] * p[0] - qv[0] * p[2],
        qv[0] * p[1] - qv[1] * p[0],
    ];
    let uuv = [
        qv[1] * uv[2] - qv[2] * uv[1],
        qv[2] * uv[0] - qv[0] * uv[2],
        qv[0] * uv[1] - qv[1] * uv[0],
    ];
    [
        p[0] + 2.0 * (q[3] * uv[0] + uuv[0]),
        p[1] + 2.0 * (q[3] * uv[1] + uuv[1]),
        p[2] + 2.0 * (q[3] * uv[2] + uuv[2]),
    ]
}

fn rage9_siren_slot(name: &str) -> Option<u16> {
    let lower = name.to_ascii_lowercase();
    let siren_at = lower.rfind("siren")?;
    lower[siren_at + "siren".len()..].parse::<u16>().ok()
}

// RAGE 9 vehicle ELS emitters are skeleton bones whose final token is siren<number>,
// for example siren1, ext_front_siren2, or ext_rear_1_roof_siren1.
// They are independent from emissiveTex, which is commonly the dashboard gauge atlas.
fn extract_rage9_skeleton_points(v: &[u8]) -> (Vec<ElsPoint>, Vec<(String, [f32; 3])>) {
    let mut bases = vec![0usize];
    if v.len() >= 0x20 {
        if let Some(fo) = vptr(ru64(v, 0x18)) {
            if fo + 0x20 <= v.len() { bases.push(fo); }
        }
    }
    for base in bases {
        let Some(so) = (base + 0x20 <= v.len()).then(|| vptr(ru64(v, base + 0x18))).flatten() else { continue };
        if so + 0x70 > v.len() { continue; }
        let count = ru16(v, so + 0x6c) as usize;
        let Some(bones) = vptr(ru64(v, so + 0x28)) else { continue };
        if !(1..=1024).contains(&count) || bones + count * 0x50 > v.len() { continue; }

        let mut world_rot = vec![[0.0, 0.0, 0.0, 1.0]; count];
        let mut world_pos = vec![[0.0; 3]; count];
        let mut world_scale = vec![[1.0; 3]; count];
        let mut out = Vec::new();
        let mut attach_points = Vec::new();
        for i in 0..count {
            let b = bones + i * 0x50;
            let mut rot = [rf32(v, b), rf32(v, b + 4), rf32(v, b + 8), rf32(v, b + 12)];
            let len = (rot.iter().map(|x| x * x).sum::<f32>()).sqrt();
            if len > 1e-6 {
                for x in &mut rot { *x /= len; }
            } else {
                rot = [0.0, 0.0, 0.0, 1.0];
            }
            let pos = [rf32(v, b + 0x10), rf32(v, b + 0x14), rf32(v, b + 0x18)];
            let scale = [rf32(v, b + 0x20), rf32(v, b + 0x24), rf32(v, b + 0x28)];
            let parent = ru16(v, b + 0x46) as usize;
            if parent < i {
                let scaled = [
                    pos[0] * world_scale[parent][0],
                    pos[1] * world_scale[parent][1],
                    pos[2] * world_scale[parent][2],
                ];
                let offset = quat_rotate(world_rot[parent], scaled);
                world_pos[i] = [
                    world_pos[parent][0] + offset[0],
                    world_pos[parent][1] + offset[1],
                    world_pos[parent][2] + offset[2],
                ];
                world_rot[i] = quat_mul(world_rot[parent], rot);
                world_scale[i] = [
                    world_scale[parent][0] * scale[0],
                    world_scale[parent][1] * scale[1],
                    world_scale[parent][2] * scale[2],
                ];
            } else {
                world_pos[i] = pos;
                world_rot[i] = rot;
                world_scale[i] = scale;
            }

            let Some(name_off) = vptr(ru64(v, b + 0x30)) else { continue };
            let end = v[name_off..].iter().position(|&c| c == 0).map(|n| name_off + n).unwrap_or(name_off);
            let Ok(name) = std::str::from_utf8(&v[name_off..end]) else { continue };
            attach_points.push((name.to_string(), world_pos[i]));
            let Some(number) = rage9_siren_slot(name) else { continue };
            out.push(ElsPoint {
                position: world_pos[i],
                bank: number % 2 == 0,
                slot: number.min(u8::MAX as u16) as u8,
            });
        }
        if !out.is_empty() || !attach_points.is_empty() {
            return (out, attach_points);
        }
    }
    (Vec::new(), Vec::new())
}

pub fn decode_rage9_model_from_rpf(rpf_path: &Path, file_off: u64, fallback_name: &str) -> Result<Mesh3D, String> {
    decode_rage9_model_from_rpf_lod(rpf_path, file_off, fallback_name, 0)
}

/// Decodes one LOD level (0 = high .. 3 = very low); falls back to the nearest populated level.
pub fn decode_rage9_model_from_rpf_lod(rpf_path: &Path, file_off: u64, fallback_name: &str, want_lod: usize) -> Result<Mesh3D, String> {
    let mut f = File::open(rpf_path).map_err(|e| format!("Open error: {e}"))?;
    let (v, mut next_off, p_cnt) = read_virtual_from_file(&mut f, file_off)?;

    let mut p = Vec::new();
    for _ in 0..p_cnt {
        let Some(zoff) = find_next_zlib(&mut f, next_off).map_err(|e| e.to_string())? else { break };
        let (chunk, nxt) = decompress_zlib_at(&mut f, zoff, 32 * 1024 * 1024).map_err(|e| e.to_string())?;
        p.extend_from_slice(&chunk);
        next_off = nxt;
    }
    if p.len() < 0x90 {
        return Err("Physical GPU buffer missing or empty".into());
    }

    let model_name = extract_rage9_model_name(&v)
        .map(|(n, _)| n)
        .unwrap_or_else(|| fallback_name.to_string());

    // Each group becomes one MeshPart: dictionary entries, or the single drawable.
    let dict = drawable_dictionary_entries(&v);
    // Drawable bases that carry LOD tables.
    let lod_bases: Vec<(String, usize)> = if !dict.is_empty() {
        dict.clone()
    } else {
        // A dictionary holding a single drawable (ped heads, eyes, animal parts) keeps
        // its shader group on that drawable, not at the root, so try it first.
        let mut bases: Vec<usize> = single_dictionary_drawable(&v).into_iter().collect();
        bases.push(0);
        if v.len() >= 0x20 {
            if let Some(fo) = vptr(ru64(&v, 0x18)) {
                if fo + 0x80 <= v.len() { bases.push(fo); }
            }
        }
        bases
            .into_iter()
            .find(|&b| (0..MAX_LODS).any(|l| !drawable_lod_geoms(&v, b, l).is_empty()))
            .map(|b| vec![(model_name.clone(), b)])
            .unwrap_or_default()
    };
    let mut lod_geoms = [0usize; MAX_LODS];
    for (l, slot) in lod_geoms.iter_mut().enumerate() {
        *slot = lod_bases.iter().map(|(_, b)| drawable_lod_geoms(&v, *b, l).len()).sum();
    }
    // The requested level, else the nearest more detailed one, else any populated one.
    let want_lod = want_lod.min(MAX_LODS - 1);
    let lod = (0..=want_lod).rev().chain(want_lod + 1..MAX_LODS).find(|&l| lod_geoms[l] > 0).unwrap_or(0);
    // (part name, drawable offset for material lookup, geometries of the chosen LOD)
    type Group = (String, Option<usize>, Vec<GeomRef>);
    let (groups, lod_label): (Vec<Group>, String) = if lod_geoms[lod] > 0 {
        let g: Vec<_> = lod_bases
            .iter()
            .map(|(nm, base)| (nm.clone(), Some(*base), drawable_lod_geoms(&v, *base, lod)))
            .filter(|(_, _, gs)| !gs.is_empty())
            .collect();
        let label = if dict.is_empty() {
            LOD_NAMES[lod].to_string()
        } else {
            format!("{} · dictionary of {}", LOD_NAMES[lod], dict.len())
        };
        (g, label)
    } else {
        (vec![(model_name.clone(), None, collect_all_geoms(&v))], "All".into())
    };
    let geom_count: usize = groups.iter().map(|(_, _, g)| g.len()).sum();
    if geom_count == 0 {
        return Err("No RAGE9 meshlet geometries found in resource".into());
    }

    let ttpf_idx = crate::tex::ttpf_hash_index_for_rpf(rpf_path);
    let is_vehicle = is_vehicle_virtual(&v, &model_name);
    let mut b = MeshBuild::default();
    let mut parts = Vec::with_capacity(groups.len());
    let mut part_verts = Vec::with_capacity(groups.len());
    let mut geom_materials: Vec<GeomMaterial> = Vec::with_capacity(geom_count);
    let mut tex_layers: Vec<TexLayer> = Vec::new();
    let mut layer_pos: HashMap<u64, u16> = HashMap::new();
    let mut gi_idx = 0u16;
    for (name, base_opt, geoms) in &groups {
        let mats = {
            if let Some(base) = *base_opt {
                resolve_drawable_materials(&v, &p, base, lod, geoms, ttpf_idx.as_deref(), is_vehicle, &mut tex_layers, &mut layer_pos)
            } else {
                vec![GeomMaterial::default(); geoms.len()]
            }
        };
        let (t0, v0) = (b.tris.len(), b.verts.len());
        for (&g, mut mat) in geoms.iter().zip(mats) {
            if !append_meshlet_geom(&p, g, gi_idx, &mut b) {
                mat.texture = None;
            }
            geom_materials.push(mat);
            gi_idx = gi_idx.wrapping_add(1);
        }
        if b.tris.len() > t0 {
            parts.push(MeshPart { name: name.clone(), geoms: geoms.len(), tris: t0..b.tris.len() });
            part_verts.push(v0..b.verts.len());
        }
    }
    if b.verts.is_empty() {
        return Err("Decoded 0 vertices from meshlets".into());
    }
    layout_parts_in_grid(&mut b.verts, &part_verts);

    // Optional vehicle fragments are often too small to pass the whole-vehicle
    // classifier but can still carry their own siren emitter bones.
    let (els_points, vehicle_attach_points) = extract_rage9_skeleton_points(&v);

    // default look applies the first color candidate of every drawn material
    for m in geom_materials.iter().filter(|m| !m.auxiliary_pass) {
        for &i in m.color_layers.iter().chain(&m.normal_layers).chain(&m.overlay_layers) {
            tex_layers[i as usize].mats += 1;
        }
        if let Some(&first) = m.color_layers.first() {
            tex_layers[first as usize].default_on = true;
        }
    }
    for l in &mut tex_layers {
        l.enabled = l.default_on;
    }

    let tex_status: String = if ttpf_idx.is_none() {
        "No .ttpf archives found; neutral preview".into()
    } else {
        "No usable color textures; neutral preview".into()
    };
    let mut mesh = Mesh3D::from_parts(
        model_name,
        b.verts,
        b.uvs,
        b.tris,
        b.tri_geom,
        Vec::new(),
        None,
        tex_status,
        geom_count,
        b.meshlets,
        lod_label,
    );
    mesh.geom_materials = geom_materials;
    mesh.tex_layers = tex_layers;
    mesh.has_els = !els_points.is_empty();
    mesh.els_points = els_points;
    mesh.vehicle_attach_points = vehicle_attach_points;
    mesh.apply_tex_layers();
    mesh.bone_count = extract_rage9_bone_count(&v);
    mesh.lod = lod;
    mesh.lod_geoms = lod_geoms;
    if parts.len() > 1 {
        mesh.parts = parts;
    }
    Ok(mesh)
}

#[inline]
fn draw_line_z(
    color_buf: &mut [Color32],
    z_buf: &mut [f32],
    w: usize,
    h: usize,
    p0: [f32; 3],
    p1: [f32; 3],
    col: Color32,
    z_bias: f32,
) {
    let dx = p1[0] - p0[0];
    let dy = p1[1] - p0[1];
    let steps = dx.abs().max(dy.abs());
    if steps < 0.5 || steps > 4000.0 {
        return;
    }
    let n = steps.ceil() as i32;
    let inv = 1.0 / (n as f32);
    let sx = dx * inv;
    let sy = dy * inv;
    let sz = (p1[2] - p0[2]) * inv;
    let mut x = p0[0];
    let mut y = p0[1];
    let mut z = p0[2] + z_bias;
    for _ in 0..=n {
        let xi = x as i32;
        let yi = y as i32;
        if xi >= 0 && yi >= 0 && (xi as usize) < w && (yi as usize) < h {
            let idx = (yi as usize) * w + (xi as usize);
            if z >= z_buf[idx] {
                z_buf[idx] = z;
                color_buf[idx] = col;
            }
        }
        x += sx;
        y += sy;
        z += sz;
    }
}

#[inline]
fn blend_preview_pixel(dst: &mut Color32, src: Color32, alpha: u8) {
    let a = alpha as u16;
    let mix = |s: u8, d: u8| ((s as u16 * a + d as u16 * (255 - a) + 127) / 255) as u8;
    *dst = Color32::from_rgb(mix(src.r(), dst.r()), mix(src.g(), dst.g()), mix(src.b(), dst.b()));
}

#[inline]
fn add_preview_pixel(dst: &mut Color32, src: Color32, strength: f32) {
    *dst = Color32::from_rgb(
        (dst.r() as f32 + src.r() as f32 * strength).min(255.0) as u8,
        (dst.g() as f32 + src.g() as f32 * strength).min(255.0) as u8,
        (dst.b() as f32 + src.b() as f32 * strength).min(255.0) as u8,
    );
}

fn rasterize_mesh(mesh: &mut Mesh3D, w: usize, h: usize) {
    let npix = w * h;
    if mesh.fb_w != w || mesh.fb_h != h {
        mesh.fb_w = w;
        mesh.fb_h = h;
        mesh.color_buf.resize(npix, Color32::from_rgb(13, 14, 18));
        mesh.z_buf.resize(npix, f32::NEG_INFINITY);
    }

    for y in 0..h {
        let t = (y as f32) / (h.max(1) as f32);
        let c = if mesh.sky_bg {
            if t < 0.56 {
                let s = t / 0.56;
                Color32::from_rgb(
                    (78.0 + s * 108.0) as u8,
                    (134.0 + s * 84.0) as u8,
                    (202.0 + s * 40.0) as u8,
                )
            } else {
                let s = (t - 0.56) / 0.44;
                Color32::from_rgb(
                    (52.0 - s * 22.0) as u8,
                    (78.0 - s * 28.0) as u8,
                    (102.0 - s * 30.0) as u8,
                )
            }
        } else {
            Color32::from_rgb(
                (11.0 + t * 9.0) as u8,
                (12.0 + t * 10.0) as u8,
                (17.0 + t * 14.0) as u8,
            )
        };
        mesh.color_buf[y * w..(y + 1) * w].fill(c);
    }
    mesh.z_buf.fill(f32::NEG_INFINITY);

    let (sy, cy) = mesh.yaw.sin_cos();
    let (sp, cp) = mesh.pitch.sin_cos();
    let min_dim = (w.min(h) as f32) * 0.44;
    let scale = (min_dim / mesh.radius) * mesh.zoom;
    let cx = (w as f32) * 0.5 + mesh.pan_x * (w as f32);
    let cy_s = (h as f32) * 0.5 + mesh.pan_y * (h as f32);
    let ctr = mesh.center;
    let persp_k = 1.0 / (mesh.radius * 4.5);

    mesh.proj_buf.resize(mesh.verts.len(), [0.0; 3]);
    mesh.persp_buf.resize(mesh.verts.len(), 1.0);
    for ((dst, pw), v) in mesh.proj_buf.iter_mut().zip(mesh.persp_buf.iter_mut()).zip(&mesh.verts) {
        let x = v[0] - ctr[0];
        let y = v[1] - ctr[1];
        let z = v[2] - ctr[2];
        let x1 = x * cy - y * sy;
        let y1 = x * sy + y * cy;
        let y2 = -y1 * cp + z * sp;
        let z2 = z * cp + y1 * sp;
        let persp = 1.0 / (1.0 - (y2 * persp_k).clamp(-0.65, 0.65));
        *dst = [cx + x1 * scale * persp, cy_s - z2 * scale * persp, y2];
        *pw = persp;
    }

    let wf = w as f32;
    let hf = h as f32;

    if !mesh.tris.is_empty() {
        let do_textured = mesh.shade_mode == ShadeMode::Textured;
        let do_solid = matches!(mesh.shade_mode, ShadeMode::Textured | ShadeMode::Solid | ShadeMode::SolidWire);
        let do_wire = matches!(mesh.shade_mode, ShadeMode::Wireframe | ShadeMode::SolidWire);
        let has_uvs = mesh.uvs.len() == mesh.verts.len() && !mesh.uvs.is_empty();

        const NAV_PALETTE: [[f32; 3]; 5] = [
            [0.82, 0.84, 0.88], [0.24, 0.26, 0.30], [0.18, 0.24, 0.32],
            [0.72, 0.75, 0.80], [0.34, 0.36, 0.40],
        ];
        let color_nav_surfaces = do_textured && mesh.lod_label == "NavMesh";
        let l1 = [0.42f32, 0.65f32, 0.63f32];
        let l2 = [-0.60f32, 0.25f32, -0.45f32];

        if do_solid {
            let vis = mesh.visible_tris();
            // A missing cutout mask should not cover other textured geometry with
            // a solid panel. Keep the clay fallback when nothing can be textured.
            let has_preview_texture = do_textured && has_uvs
                && mesh.geom_materials.iter().any(|m| m.pass == MaterialPass::Cutout && m.texture.is_none())
                && mesh.tri_geom.get(vis.clone()).is_some_and(|ids| {
                    ids.iter().any(|&g| mesh.geom_materials.get(g as usize).is_some_and(|m| m.texture.is_some()))
                });

            let use_blending = do_textured && mesh.texture.is_none()
                && mesh.geom_materials.iter().any(|m| m.blends(has_uvs));
            let blend_material = |ti| {
                if !use_blending { return None; }
                let g = mesh.tri_geom.get(ti).copied().unwrap_or(0) as usize;
                mesh.geom_materials.get(g).filter(|m| m.blends(has_uvs) && !m.auxiliary_pass)
            };
            mesh.blend_order.clear();
            if use_blending {
                for ti in vis.clone() {
                    if let Some(mat) = blend_material(ti) {
                        let [a, b, c] = mesh.tris[ti].map(|i| mesh.proj_buf[i as usize][2]);
                        mesh.blend_order.push((ti, a + b + c, mat.pass == MaterialPass::Decal));
                    }
                }
                // Decals go over opaque surfaces, then glass/alpha draws far to near.
                mesh.blend_order.sort_unstable_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.total_cmp(&b.1)));
            }
            let solid_tris = vis.filter(|&ti| blend_material(ti).is_none());
            for ti in solid_tris.chain(mesh.blend_order.iter().map(|&(ti, _, _)| ti)) {
                let t = mesh.tris[ti];
                let g = mesh.tri_geom.get(ti).copied().unwrap_or(0) as usize;
                let gmat = mesh.geom_materials.get(g);
                let active_tex: Option<&Texture2D> = if do_textured && has_uvs {
                    mesh.texture.as_ref().or_else(|| gmat.and_then(|m| m.texture.as_deref()))
                } else {
                    None
                };
                if do_textured && gmat.is_some_and(|m| m.auxiliary_pass
                    || (m.pass == MaterialPass::Cutout && active_tex.is_none() && has_preview_texture))
                {
                    continue;
                }
                // normal map and multiply overlays of the material only in textured mode without a manual texture
                let layered = do_textured && has_uvs && mesh.texture.is_none();
                let normal_tex: Option<&Texture2D> = if layered { gmat.and_then(|m| m.normal.as_deref()) } else { None };
                let overlay_texs: &[Arc<Texture2D>] = match gmat {
                    Some(m) if layered => &m.overlays,
                    _ => &[],
                };
                let blend = blend_material(ti).is_some();
                let pass = if do_textured && mesh.texture.is_some() {
                    MaterialPass::Cutout
                } else if do_textured {
                    gmat.map_or(MaterialPass::Opaque, |m| m.pass)
                } else {
                    MaterialPass::Opaque
                };
                let glass = matches!(pass, MaterialPass::Glass { .. });
                let depth_bias = if blend && pass == MaterialPass::Decal { mesh.radius * 0.0001 } else { 0.0 };
                let i0 = t[0] as usize;
                let i1 = t[1] as usize;
                let i2 = t[2] as usize;
                let p0 = mesh.proj_buf[i0];
                let p1 = mesh.proj_buf[i1];
                let p2 = mesh.proj_buf[i2];

                let min_x = p0[0].min(p1[0]).min(p2[0]);
                let max_x = p0[0].max(p1[0]).max(p2[0]);
                let min_y = p0[1].min(p1[1]).min(p2[1]);
                let max_y = p0[1].max(p1[1]).max(p2[1]);
                if max_x < 0.0 || min_x >= wf || max_y < 0.0 || min_y >= hf {
                    continue;
                }

                let area = (p1[0] - p0[0]) * (p2[1] - p0[1]) - (p1[1] - p0[1]) * (p2[0] - p0[0]);
                let abs_area = area.abs();
                if abs_area < 0.15 {
                    continue;
                }

                // world vector into the lighting frame
                let to_view = |n: [f32; 3]| {
                    let ny1 = n[0] * sy + n[1] * cy;
                    [n[0] * cy - n[1] * sy, -ny1 * cp + n[2] * sp, n[2] * cp + ny1 * sp]
                };
                // rgb shade factors plus the main light term and rim used by the paint clearcoat
                let shade_of = |nv: [f32; 3]| {
                    let ndl1 = (nv[0] * l1[0] + nv[1] * l1[1] + nv[2] * l1[2]).abs();
                    let ndl2 = (nv[0] * l2[0] + nv[1] * l2[1] + nv[2] * l2[2]).max(0.0);
                    let spec = ndl1.powi(10) * 0.35;
                    let rim = (1.0 - nv[1].abs()).powi(2) * 0.22;
                    (
                        [
                            (0.18 + ndl1 * 0.72 + spec + rim * 0.45).min(1.15),
                            (0.18 + ndl1 * 0.72 + ndl2 * 0.18 + spec + rim * 0.40).min(1.15),
                            (0.20 + ndl1 * 0.70 + ndl2 * 0.28 + spec + rim * 0.55).min(1.15),
                        ],
                        ndl1,
                        rim,
                    )
                };
                let n_view = to_view(mesh.normals[ti]);
                let ([shade_r, shade_g, shade_b], ndl1, rim) = shade_of(n_view);

                let is_paint = do_textured
                    && active_tex.is_none()
                    && gmat.map(|m| m.is_vehicle_paint).unwrap_or(false);
                let base_albedo = if is_paint {
                    mesh.paint_color
                } else if color_nav_surfaces {
                    NAV_PALETTE.get(g).copied().unwrap_or([0.68; 3])
                } else if glass {
                    [0.62, 0.71, 0.78]
                } else if do_textured {
                    [0.68; 3]
                } else {
                    [0.86, 0.88, 0.92]
                };
                let clearcoat = if is_paint { ndl1.powi(24) * 0.55 + rim * 0.14 } else { 0.0 };
                let flat_col = Color32::from_rgb(
                    ((base_albedo[0] * shade_r + clearcoat).min(1.0) * 255.0) as u8,
                    ((base_albedo[1] * shade_g + clearcoat).min(1.0) * 255.0) as u8,
                    ((base_albedo[2] * shade_b + clearcoat).min(1.0) * 255.0) as u8,
                );

                let ix0 = (min_x.floor() as i32).max(0) as usize;
                let ix1 = (max_x.ceil() as i32).min(w as i32 - 1) as usize;
                let iy0 = (min_y.floor() as i32).max(0) as usize;
                let iy1 = (max_y.ceil() as i32).min(h as i32 - 1) as usize;

                // flip sign so edge funcs stay positive inside triangls
                let sign = if area > 0.0 { -1.0 } else { 1.0 };
                let inv_abs = 1.0 / abs_area;

                let e01_dx = (p1[1] - p0[1]) * sign;
                let e01_dy = (p0[0] - p1[0]) * sign;
                let e12_dx = (p2[1] - p1[1]) * sign;
                let e12_dy = (p1[0] - p2[0]) * sign;
                let e20_dx = (p0[1] - p2[1]) * sign;
                let e20_dy = (p2[0] - p0[0]) * sign;

                let px_start = ix0 as f32 + 0.5;
                let py_start = iy0 as f32 + 0.5;

                let mut w0_row = (px_start - p1[0]) * e12_dx + (py_start - p1[1]) * e12_dy;
                let mut w1_row = (px_start - p2[0]) * e20_dx + (py_start - p2[1]) * e20_dy;
                let mut w2_row = (px_start - p0[0]) * e01_dx + (py_start - p0[1]) * e01_dy;
                let span_len = ix1 - ix0 + 1;

                if active_tex.is_some() || normal_tex.is_some() || !overlay_texs.is_empty() {
                    let uv0 = mesh.uvs[i0];
                    let uv1 = mesh.uvs[i1];
                    let uv2 = mesh.uvs[i2];
                    // Perspective-correct UVs: weight each corner by its 1/w (persp) so the
                    // texture stays pinned to the surface. Plain screen-space barycentrics
                    // (affine mapping) make the image slide along the quad diagonal as you orbit.
                    let (pq0, pq1, pq2) = (mesh.persp_buf[i0], mesh.persp_buf[i1], mesh.persp_buf[i2]);
                    // tangent frame of the triangle in the lighting frame for normal mapping
                    let tbn = normal_tex.and_then(|_| {
                        let (q0, q1, q2) = (mesh.verts[i0], mesh.verts[i1], mesh.verts[i2]);
                        let e1 = [q1[0] - q0[0], q1[1] - q0[1], q1[2] - q0[2]];
                        let e2 = [q2[0] - q0[0], q2[1] - q0[1], q2[2] - q0[2]];
                        let (du1, dv1, du2, dv2) = (uv1[0] - uv0[0], uv1[1] - uv0[1], uv2[0] - uv0[0], uv2[1] - uv0[1]);
                        let det = du1 * dv2 - du2 * dv1;
                        if det.abs() < 1e-12 {
                            return None;
                        }
                        let unit = |a: [f32; 3]| {
                            let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
                            (l > 1e-12).then(|| [a[0] / l, a[1] / l, a[2] / l])
                        };
                        let t = unit(std::array::from_fn(|k| (e1[k] * dv2 - e2[k] * dv1) / det))?;
                        let b = unit(std::array::from_fn(|k| (e2[k] * du1 - e1[k] * du2) / det))?;
                        Some((to_view(t), to_view(b)))
                    });
                    let flat_rgb = [base_albedo[0] * 255.0, base_albedo[1] * 255.0, base_albedo[2] * 255.0];
                    for y in iy0..=iy1 {
                        let row_start = y * w + ix0;
                        let z_row = &mut mesh.z_buf[row_start..row_start + span_len];
                        let c_row = &mut mesh.color_buf[row_start..row_start + span_len];
                        let (mut w0, mut w1, mut w2) = (w0_row, w1_row, w2_row);
                        for x in 0..span_len {
                            if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                                let (b0, b1, b2) = (w0 * inv_abs, w1 * inv_abs, w2 * inv_abs);
                                let z = b0 * p0[2] + b1 * p1[2] + b2 * p2[2];
                                if z + depth_bias > z_row[x] {
                                    // Perspective-correct blend (see q0..q2 above).
                                    let (c0, c1, c2) = (b0 * pq0, b1 * pq1, b2 * pq2);
                                    let inv_q = 1.0 / (c0 + c1 + c2);
                                    let u = (c0 * uv0[0] + c1 * uv1[0] + c2 * uv2[0]) * inv_q;
                                    let v = (c0 * uv0[1] + c1 * uv1[1] + c2 * uv2[1]) * inv_q;
                                    let tc = active_tex.map(|tex| tex.sample(u, v));
                                    let alpha = match (pass, tc) {
                                        (MaterialPass::Glass { .. }, None) | (MaterialPass::Glass { texture_alpha: false }, _) => GLASS_PREVIEW_ALPHA,
                                        (MaterialPass::Opaque, _) | (_, None) => 255,
                                        (MaterialPass::Cutout, Some(tc)) => if tc.a() >= 128 { 255 } else { 0 },
                                        (_, Some(tc)) => tc.a(),
                                    };
                                    // Holes leave both color and depth untouched.
                                    if alpha != 0 {
                                        let mut rgb = match tc {
                                            Some(tc) => {
                                                let [r, g, b, _] = if pass != MaterialPass::Opaque && tc.a() != 255 {
                                                    tc.to_srgba_unmultiplied()
                                                } else {
                                                    tc.to_array()
                                                };
                                                [r as f32, g as f32, b as f32]
                                            }
                                            None => flat_rgb,
                                        };
                                        for ov in overlay_texs {
                                            let oc = ov.sample(u, v);
                                            rgb[0] *= oc.r() as f32 / 255.0;
                                            rgb[1] *= oc.g() as f32 / 255.0;
                                            rgb[2] *= oc.b() as f32 / 255.0;
                                        }
                                        let (shade, ndl, rim_px) = match (normal_tex, tbn) {
                                            (Some(nt), Some((tv, bv))) => {
                                                let nc = nt.sample(u, v);
                                                let (tx, ty, tz) = (nc.r() as f32 / 127.5 - 1.0, nc.g() as f32 / 127.5 - 1.0, (nc.b() as f32 / 127.5 - 1.0).max(0.05));
                                                let nv: [f32; 3] = std::array::from_fn(|k| tv[k] * tx + bv[k] * ty + n_view[k] * tz);
                                                let len = (nv[0] * nv[0] + nv[1] * nv[1] + nv[2] * nv[2]).sqrt().max(1e-6);
                                                shade_of([nv[0] / len, nv[1] / len, nv[2] / len])
                                            }
                                            _ => ([shade_r, shade_g, shade_b], ndl1, rim),
                                        };
                                        let coat = if is_paint { (ndl.powi(24) * 0.55 + rim_px * 0.14) * 255.0 } else { 0.0 };
                                        let src = Color32::from_rgb(
                                            ((rgb[0] * shade[0] + coat).min(255.0)) as u8,
                                            ((rgb[1] * shade[1] + coat).min(255.0)) as u8,
                                            ((rgb[2] * shade[2] + coat).min(255.0)) as u8,
                                        );
                                        if blend {
                                            blend_preview_pixel(&mut c_row[x], src, alpha);
                                        } else {
                                            z_row[x] = z;
                                            c_row[x] = src;
                                        }
                                    }
                                }
                            }
                            w0 += e12_dx;
                            w1 += e20_dx;
                            w2 += e01_dx;
                        }
                        w0_row += e12_dy;
                        w1_row += e20_dy;
                        w2_row += e01_dy;
                    }
                    continue;
                }

                for y in iy0..=iy1 {
                    let row_start = y * w + ix0;
                    let z_row = &mut mesh.z_buf[row_start..row_start + span_len];
                    let c_row = &mut mesh.color_buf[row_start..row_start + span_len];
                    let (mut w0, mut w1, mut w2) = (w0_row, w1_row, w2_row);
                    for x in 0..span_len {
                        if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                            let z = (w0 * p0[2] + w1 * p1[2] + w2 * p2[2]) * inv_abs;
                            if z > z_row[x] {
                                if glass {
                                    blend_preview_pixel(&mut c_row[x], flat_col, GLASS_PREVIEW_ALPHA);
                                } else {
                                    z_row[x] = z;
                                    c_row[x] = flat_col;
                                }
                            }
                        }
                        w0 += e12_dx;
                        w1 += e20_dx;
                        w2 += e01_dx;
                    }
                    w0_row += e12_dy;
                    w1_row += e20_dy;
                    w2_row += e01_dy;
                }
            }
        }

        if do_wire {
            let wire_col = if do_solid { Color32::from_rgb(252, 175, 23) } else { Color32::from_rgb(72, 210, 255) };
            let z_bias = if do_solid { mesh.radius * 0.004 } else { 0.0 };
            let vis = mesh.visible_tris();
            let step = (vis.len() / 25_000).max(1);
            for t in mesh.tris[vis].iter().step_by(step) {
                let p0 = mesh.proj_buf[t[0] as usize];
                let p1 = mesh.proj_buf[t[1] as usize];
                let p2 = mesh.proj_buf[t[2] as usize];
                draw_line_z(&mut mesh.color_buf, &mut mesh.z_buf, w, h, p0, p1, wire_col, z_bias);
                draw_line_z(&mut mesh.color_buf, &mut mesh.z_buf, w, h, p1, p2, wire_col, z_bias);
                draw_line_z(&mut mesh.color_buf, &mut mesh.z_buf, w, h, p2, p0, wire_col, z_bias);
            }
        }
    } else if !mesh.edges.is_empty() {
        let col = Color32::from_rgb(72, 210, 255);
        for &(a, b) in mesh.edges.iter().take(60_000) {
            draw_line_z(&mut mesh.color_buf, &mut mesh.z_buf, w, h, mesh.proj_buf[a as usize], mesh.proj_buf[b as usize], col, 0.0);
        }
    } else {
        let col = Color32::from_rgb(252, 175, 23);
        for p0 in &mesh.proj_buf {
            let xi = p0[0] as i32;
            let yi = p0[1] as i32;
            if xi >= 1 && yi >= 1 && (xi as usize) + 1 < w && (yi as usize) + 1 < h {
                let idx = (yi as usize) * w + (xi as usize);
                if p0[2] > mesh.z_buf[idx] {
                    mesh.z_buf[idx] = p0[2];
                    mesh.color_buf[idx] = col;
                }
            }
        }
    }

    if mesh.els_enabled && !mesh.els_points.is_empty() {
        let glow_radius = ((w.min(h) as f32) * 0.014).clamp(5.0, 18.0);
        let core_radius = (glow_radius * 0.28).max(2.0);
        for light in &mesh.els_points {
            if mesh.els_flash && !rage9_siren_lit(light.slot, mesh.els_flash_phase) {
                continue;
            }
            let v = light.position;
            let x = v[0] - ctr[0];
            let y = v[1] - ctr[1];
            let z = v[2] - ctr[2];
            let x1 = x * cy - y * sy;
            let y1 = x * sy + y * cy;
            let y2 = -y1 * cp + z * sp;
            let z2 = z * cp + y1 * sp;
            let persp = 1.0 / (1.0 - (y2 * persp_k).clamp(-0.65, 0.65));
            let sx = cx + x1 * scale * persp;
            let sy = cy_s - z2 * scale * persp;
            if sx < -glow_radius || sy < -glow_radius || sx >= wf + glow_radius || sy >= hf + glow_radius {
                continue;
            }
            let center_x = sx.clamp(0.0, wf - 1.0) as usize;
            let center_y = sy.clamp(0.0, hf - 1.0) as usize;
            let center_depth = mesh.z_buf[center_y * w + center_x];
            if center_depth.is_finite() && y2 < center_depth - mesh.radius * 0.08 {
                continue;
            }
            let color = if light.bank {
                Color32::from_rgb(45, 115, 255)
            } else {
                Color32::from_rgb(255, 35, 28)
            };
            let x0 = (sx - glow_radius).floor().max(0.0) as usize;
            let x1 = (sx + glow_radius).ceil().min(wf - 1.0) as usize;
            let y0 = (sy - glow_radius).floor().max(0.0) as usize;
            let y1 = (sy + glow_radius).ceil().min(hf - 1.0) as usize;
            for py in y0..=y1 {
                for px in x0..=x1 {
                    let dx = px as f32 + 0.5 - sx;
                    let dy = py as f32 + 0.5 - sy;
                    let distance = (dx * dx + dy * dy).sqrt();
                    if distance <= glow_radius {
                        let strength = if distance <= core_radius {
                            1.0
                        } else {
                            let t = 1.0 - (distance - core_radius) / (glow_radius - core_radius);
                            t * t * 0.42
                        };
                        add_preview_pixel(&mut mesh.color_buf[py * w + px], color, strength);
                    }
                }
            }
        }
    }
}

// dropdown that switches individual material textures of the mesh on and off
pub fn tex_layer_menu(ui: &mut egui::Ui, mesh: &mut Mesh3D) {
    if mesh.tex_layers.is_empty() || mesh.texture.is_some() {
        return;
    }
    let on = mesh.tex_layers.iter().filter(|l| l.enabled).count();
    let mut changed = false;
    let mut preset: Option<fn(&TexLayer) -> bool> = None;
    ui.menu_button(format!("Applied textures {on}/{} ⏷", mesh.tex_layers.len()), |ui| {
        ui.set_min_width(430.0);
        ui.horizontal(|ui| {
            if ui.button("Default").on_hover_text("One color texture per material").clicked() { preset = Some(|l| l.default_on); }
            if ui.button("Color + normals").clicked() { preset = Some(|l| l.default_on || l.kind == TexLayerKind::Normal); }
            if ui.button("None").clicked() { preset = Some(|_| false); }
        });
        ui.label(
            RichText::new("Color: first ticked one per material is drawn. Normal: bends the lighting. Overlay: multiplied on top.")
                .size(10.5)
                .color(Color32::from_rgb(150, 160, 180)),
        );
        ui.separator();
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for (kind, title) in [
                (TexLayerKind::Color, "Color"),
                (TexLayerKind::Normal, "Normal maps"),
                (TexLayerKind::Overlay, "Overlays (dirt, dust, masks)"),
            ] {
                if !mesh.tex_layers.iter().any(|l| l.kind == kind) {
                    continue;
                }
                ui.label(RichText::new(title).strong().size(11.5).color(Color32::from_rgb(255, 168, 28)));
                for l in mesh.tex_layers.iter_mut().filter(|l| l.kind == kind) {
                    let text = format!("{}  ·  {}x{}  ·  {} mat{}", l.label, l.w, l.h, l.mats, if l.mats == 1 { "" } else { "s" });
                    let tip = format!("{}\nhash {:016x}{}", l.source, l.hash64, if l.failed { "\ncould not be decoded" } else { "" });
                    if ui.checkbox(&mut l.enabled, text).on_hover_text(tip).changed() {
                        changed = true;
                    }
                }
                ui.add_space(4.0);
            }
        });
    });
    if let Some(f) = preset {
        mesh.shade_mode = ShadeMode::Textured;
        mesh.set_tex_layers(f);
    } else if changed {
        mesh.shade_mode = ShadeMode::Textured;
        mesh.apply_tex_layers();
    }
}

/// Drawable picker for multi-part meshes (drawable dictionaries). No-op otherwise.
pub fn part_picker(ui: &mut egui::Ui, mesh: &mut Mesh3D) {
    if mesh.parts.len() < 2 {
        return;
    }
    let label = |m: &Mesh3D, i: Option<usize>| match i.and_then(|i| m.parts.get(i)) {
        Some(p) => format!("{} ({} geoms, {} tris)", p.name, p.geoms, p.tris.len()),
        None => format!("All {} drawables (grid)", m.parts.len()),
    };
    let mut sel = mesh.active_part;
    egui::ComboBox::from_id_salt("mesh_part_picker")
        .selected_text(label(mesh, sel))
        .width(260.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut sel, None, label(mesh, None));
            for i in 0..mesh.parts.len() {
                ui.selectable_value(&mut sel, Some(i), label(mesh, Some(i)));
            }
        })
        .response
        .on_hover_text("Drawable dictionary: independent models, each built around its own origin. Shown side by side; pick one to isolate it.");
    if sel != mesh.active_part {
        mesh.set_active_part(sel);
    }
}

pub fn render_mesh3d_viewport(ui: &mut egui::Ui, mesh: &mut Mesh3D) {
    ui.horizontal_wrapped(|ui| {
        let bone_s = if mesh.bone_count > 0 { format!("  ·  {} bones", mesh.bone_count) } else { String::new() };
        ui.label(
            RichText::new(format!(
                "{}  ·  {} verts  ·  {} tris  ·  {} geoms / {} meshlets ({}){}",
                mesh.name, mesh.verts.len(), mesh.tris.len(), mesh.geom_count, mesh.meshlet_count, mesh.lod_label, bone_s
            ))
            .strong()
            .color(Color32::from_rgb(252, 175, 23)),
        );
        part_picker(ui, mesh);
        ui.add_space(6.0);
        for (mode, label) in [
            (ShadeMode::Textured, "Textured/Mat"),
            (ShadeMode::Solid, "Clay Solid"),
            (ShadeMode::SolidWire, "Solid+Wire"),
            (ShadeMode::Wireframe, "Wire"),
        ] {
            if ui.selectable_label(mesh.shade_mode == mode, label).clicked() {
                mesh.shade_mode = mode;
                mesh.dirty = true;
            }
        }
        if ui.selectable_label(mesh.auto_spin, "Spin").clicked() {
            mesh.auto_spin = !mesh.auto_spin;
            mesh.dirty = true;
        }
        if ui.button("Load Texture...").clicked() {
            if let Some(tp) = rfd::FileDialog::new().add_filter("Textures", &["png", "bmp"]).pick_file() {
                if let Ok(img) = image::open(&tp) {
                    let rgba = img.to_rgba8();
                    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
                    let pixels: Vec<Color32> = rgba
                        .as_chunks::<4>().0.iter()
                        .map(|c| Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                        .collect();
                    mesh.texture = Some(Texture2D { w, h, pixels });
                    mesh.tex_status = format!("Loaded {} ({w}x{h})", tp.file_name().unwrap_or_default().to_string_lossy());
                    mesh.shade_mode = ShadeMode::Textured;
                    mesh.dirty = true;
                }
            }
        }
        if ui.button("Reset").clicked() {
            mesh.yaw = DEFAULT_YAW;
            mesh.pitch = DEFAULT_PITCH;
            mesh.zoom = 1.0;
            mesh.pan_x = 0.0;
            mesh.pan_y = 0.0;
            mesh.dirty = true;
        }
        if ui.button("Export .OBJ").clicked() {
            let safe_name = mesh.name.replace('#', "");
            if let Some(p) = rfd::FileDialog::new().set_file_name(format!("{safe_name}.obj")).save_file() {
                let _ = mesh.export_obj(&p);
            }
        }
        tex_layer_menu(ui, mesh);
        if mesh.has_paint_geom {
            ui.add_space(4.0);
            ui.label(RichText::new("Paint:").size(11.0).color(Color32::from_rgb(175, 185, 205)));
            for &(label, rgb) in &PAINT_PRESETS {
                let col = Color32::from_rgb(
                    (rgb[0] * 255.0) as u8,
                    (rgb[1] * 255.0) as u8,
                    (rgb[2] * 255.0) as u8,
                );
                let active = (mesh.paint_color[0] - rgb[0]).abs() < 0.02
                    && (mesh.paint_color[1] - rgb[1]).abs() < 0.02
                    && (mesh.paint_color[2] - rgb[2]).abs() < 0.02;
                let mut btn = egui::Button::new("  ").fill(col).min_size(egui::vec2(16.0, 16.0));
                if active {
                    btn = btn.stroke(egui::Stroke::new(2.0_f32, Color32::WHITE));
                }
                if ui.add(btn).on_hover_text(label).clicked() {
                    mesh.paint_color = rgb;
                    mesh.shade_mode = ShadeMode::Textured;
                    mesh.dirty = true;
                }
            }
            if ui.color_edit_button_rgb(&mut mesh.paint_color).changed() {
                mesh.shade_mode = ShadeMode::Textured;
                mesh.dirty = true;
            }
        }
        if mesh.has_els {
            ui.add_space(4.0);
            let label = if mesh.els_enabled { "ELS On" } else { "ELS Off" };
            if ui
                .selectable_label(mesh.els_enabled, label)
                .on_hover_text(format!("Preview {} RAGE 9 siren-bone emitters.", mesh.els_points.len()))
                .clicked()
            {
                mesh.els_enabled = !mesh.els_enabled;
                mesh.shade_mode = ShadeMode::Textured;
                mesh.dirty = true;
            }
            if ui
                .add_enabled(mesh.els_enabled, egui::Checkbox::new(&mut mesh.els_flash, "Flash"))
                .on_hover_text("Use the game-authored 32-step siren sequencers at 500 BPM.")
                .changed()
            {
                mesh.dirty = true;
            }
        }
        let status = ui.label(RichText::new(&mesh.tex_status).size(11.0).color(Color32::from_rgb(130, 145, 168)));
        if !mesh.geom_materials.is_empty() {
            status.on_hover_text("Textured/Mat uses available color textures, opacity masks and optional preview paint. \
                Untextured solid surfaces use a gray fallback. Use Clay Solid to see auxiliary geometry and \
                cutouts whose opacity mask could not be loaded. Glass transparency is approximate.");
        }
    });
    ui.add_space(4.0);

    let avail = ui.available_size();
    let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());

    let mut dragging = false;
    if resp.dragged_by(egui::PointerButton::Primary) {
        let d = resp.drag_delta();
        mesh.yaw += d.x * 0.01;
        mesh.pitch = (mesh.pitch + d.y * 0.01).clamp(-1.5, 1.5);
        mesh.dirty = true;
        dragging = true;
    } else if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
        let d = resp.drag_delta();
        mesh.pan_x += d.x / rect.width().max(1.0);
        mesh.pan_y += d.y / rect.height().max(1.0);
        mesh.dirty = true;
        dragging = true;
    }
    if resp.double_clicked() {
        mesh.yaw = DEFAULT_YAW;
        mesh.pitch = DEFAULT_PITCH;
        mesh.zoom = 1.0;
        mesh.pan_x = 0.0;
        mesh.pan_y = 0.0;
        mesh.dirty = true;
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            mesh.zoom = (mesh.zoom * (1.0 + scroll * 0.0022)).clamp(0.15, 25.0);
            mesh.dirty = true;
        }
        // codewalker-style wasd + q/e camera navigation
        let (fw, rt, up) = ui.input(|i| {
            let f = (i.key_down(egui::Key::W) as i32 - i.key_down(egui::Key::S) as i32) as f32;
            let r = (i.key_down(egui::Key::D) as i32 - i.key_down(egui::Key::A) as i32) as f32;
            let u = (i.key_down(egui::Key::E) as i32 - i.key_down(egui::Key::Q) as i32) as f32;
            (f, r, u)
        });
        if fw != 0.0 || rt != 0.0 || up != 0.0 {
            mesh.zoom = (mesh.zoom * (1.0 + fw * 0.03)).clamp(0.15, 25.0);
            mesh.pan_x -= rt * 0.015;
            mesh.pan_y += up * 0.015;
            mesh.dirty = true;
            ui.ctx().request_repaint();
        }
    }
    if mesh.auto_spin && !dragging {
        mesh.yaw += 0.014;
        mesh.dirty = true;
        ui.ctx().request_repaint();
    }
    if mesh.els_enabled && mesh.els_flash {
        let steps_per_second = RAGE9_SIREN_BPM / 60.0 * (RAGE9_SIREN_MULTIPLES * 2) as f64;
        let phase = (ui.input(|i| i.time) * steps_per_second) as u64;
        if phase != mesh.els_flash_phase {
            mesh.els_flash_phase = phase;
            mesh.dirty = true;
        }
        ui.ctx().request_repaint();
    } else if mesh.els_flash_phase != 0 {
        mesh.els_flash_phase = 0;
        mesh.dirty = true;
    }
    if mesh.last_dragging && !dragging {
        mesh.dirty = true;
    }
    mesh.last_dragging = dragging;

    let heavy = mesh.visible_tris().len() > 25_000;
    let ppp = ui.ctx().pixels_per_point();
    let res_scale = ppp * if dragging && heavy { 0.55 } else { 1.0 };
    let target_w = ((rect.width() * res_scale) as usize).clamp(160, 1920);
    let target_h = ((rect.height() * res_scale) as usize).clamp(120, 1200);

    if mesh.dirty || mesh.fb_w != target_w || mesh.fb_h != target_h || mesh.tex.is_none() {
        rasterize_mesh(mesh, target_w, target_h);
        let ci = ColorImage { size: [target_w, target_h], pixels: mesh.color_buf.clone() };
        if let Some(tex) = &mut mesh.tex {
            tex.set(ci, TextureOptions::LINEAR);
        } else {
            mesh.tex = Some(ui.ctx().load_texture("app_3d_fb", ci, TextureOptions::LINEAR));
        }
        mesh.dirty = false;
    }

    let p = ui.painter_at(rect);
    p.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(11, 12, 17));
    if let Some(tex) = &mesh.tex {
        p.image(tex.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    }
    p.text(
        egui::pos2(rect.min.x + 10.0, rect.max.y - 16.0),
        egui::Align2::LEFT_BOTTOM,
        "Left-drag: Orbit  ·  Right-drag: Pan  ·  Scroll: Zoom  ·  Double-click: Reset",
        egui::FontId::proportional(11.0),
        Color32::from_rgba_unmultiplied(180, 188, 204, 150),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rage9_siren_uses_game_sequencers_and_sub_flashes() {
        assert_eq!(rage9_siren_slot("siren1"), Some(1));
        assert_eq!(rage9_siren_slot("ext_front_siren2"), Some(2));
        assert_eq!(rage9_siren_slot("siren_bumper_r_bar4"), None);
        assert_eq!(rage9_siren_slot("siren_roof_round_1"), None);
        assert_eq!(rage9_siren_slot("siren_roof"), None);
        // siren1 starts with 1, siren4 with the inverse 0.
        assert!(rage9_siren_lit(1, 0));
        assert!(!rage9_siren_lit(4, 0));
        // Three flashes are emitted inside an active beat.
        assert!(!rage9_siren_lit(1, 1));
        assert!(rage9_siren_lit(1, 2));
        assert!(rage9_siren_lit(1, 4));
        // The next bit reverses the pair; unused and invalid slots stay dark.
        assert!(!rage9_siren_lit(1, 6));
        assert!(rage9_siren_lit(4, 6));
        assert!(!rage9_siren_lit(9, 0));
        assert!(!rage9_siren_lit(0, 0));
        assert!(!rage9_siren_lit(21, 0));
    }

    #[test]
    fn vehicle_parts_merge_in_parent_model_space() {
        let mut base = Mesh3D::from_parts(
            "car_hi".into(),
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0.0; 2]; 3],
            vec![[0, 1, 2]],
            vec![0],
            vec![],
            None,
            String::new(),
            1,
            1,
            "High".into(),
        );
        let mut lightbar = Mesh3D::from_parts(
            "car_lightbar_hi".into(),
            vec![[0.0, 0.0, 2.0], [1.0, 0.0, 2.0], [0.0, 1.0, 2.0]],
            vec![[0.0; 2]; 3],
            vec![[0, 1, 2]],
            vec![0],
            vec![],
            None,
            String::new(),
            1,
            1,
            "High".into(),
        );
        lightbar.has_els = true;
        lightbar.els_points.push(ElsPoint { position: [0.5, 0.5, 2.0], bank: false, slot: 1 });
        base.append_vehicle_part(lightbar);
        assert_eq!(base.verts.len(), 6);
        assert_eq!(base.tris, vec![[0, 1, 2], [3, 4, 5]]);
        assert_eq!(base.parts.len(), 2);
        assert_eq!(base.els_points.len(), 1);
        assert!(base.has_els);
    }

    #[test]
    fn upright_and_not_mirrored() {
        let verts = vec![[0.0, 0.0, -1.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -1.0, 0.0]];
        let mut m = Mesh3D::from_parts("t".into(), verts, vec![], vec![], vec![], vec![], None, String::new(), 1, 0, String::new());
        m.yaw = 0.0;
        m.pitch = 0.4;
        rasterize_mesh(&mut m, 200, 200);
        let p = &m.proj_buf;
        assert!(p[1][1] < p[0][1], "+Z must be screen-up");
        assert!(p[2][0] > 100.0, "+X must be screen-right (no mirror)");
        assert!(p[3][1] < p[4][1], "positive pitch views from above");
        assert!(p[4][2] > p[3][2], "-Y side is closer to camera");
    }

    #[test]
    fn textures_stay_pinned_while_orbiting() {
        // A quad facing the camera, u = (x + 1) / 2, plus one unreferenced probe vertex on
        // the surface off the diagonal. Turned sideways, the texel under the probe must
        // still be u = 0.5; affine (screen-space) UVs drift ~0.06 here, past the band.
        let verts = vec![
            [-1.0, 0.0, -1.0], [1.0, 0.0, -1.0], [1.0, 0.0, 1.0], [-1.0, 0.0, 1.0],
            [0.0, 0.0, -0.5],
        ];
        let uvs = verts.iter().map(|v: &[f32; 3]| [(v[0] + 1.0) * 0.5, 0.5]).collect();
        let mut m = Mesh3D::from_parts(
            "orbit".into(), verts, uvs, vec![[0, 1, 2], [2, 3, 0]],
            vec![0, 0], vec![], None, String::new(), 1, 0, String::new(),
        );
        m.geom_materials = vec![GeomMaterial {
            texture: Some(Arc::new(Texture2D {
                // green band for u in [0.47, 0.53), red before it, blue after (lighting scales
                // the channels, so test which channel wins rather than exact values)
                w: 256, h: 1, pixels: (0..256).map(|i| match i {
                    0..120 => Color32::RED,
                    120..136 => Color32::GREEN,
                    _ => Color32::BLUE,
                }).collect(),
            })),
            ..Default::default()
        }];
        m.pitch = 0.0;
        for yaw in [-1.2f32, -0.9, 0.9, 1.2] {
            m.yaw = yaw;
            rasterize_mesh(&mut m, 400, 400);
            let [px, py, _] = m.proj_buf[4];
            let c = m.color_buf[py as usize * 400 + px as usize];
            assert!(c.g() > c.r() && c.g() > c.b(), "yaw {yaw}: {c:?} under the probe, expected the green u = 0.5 band");
        }
    }

    #[test]
    fn cutout_holes_do_not_occlude_geometry() {
        let verts = vec![
            [-1.0, -0.1, -1.0], [1.0, -0.1, -1.0], [0.0, -0.1, 1.0],
            [-1.0, 0.1, -1.0], [1.0, 0.1, -1.0], [0.0, 0.1, 1.0],
        ];
        let mut m = Mesh3D::from_parts(
            "cutout".into(), verts, vec![[0.0; 2]; 6], vec![[0, 1, 2], [3, 4, 5]],
            vec![0, 1], vec![], None, String::new(), 2, 0, String::new(),
        );
        m.yaw = 0.0;
        m.pitch = 0.0;
        m.geom_materials = vec![
            GeomMaterial {
                texture: Some(Arc::new(Texture2D { w: 1, h: 1, pixels: vec![Color32::TRANSPARENT] })),
                pass: MaterialPass::Cutout,
                ..Default::default()
            },
            GeomMaterial {
                texture: Some(Arc::new(Texture2D { w: 1, h: 1, pixels: vec![Color32::GREEN] })),
                ..Default::default()
            },
        ];
        let center = 50 * 100 + 50;
        rasterize_mesh(&mut m, 100, 100);
        assert!(m.color_buf[center].g() > 0, "the back triangle shows through the hole");
        assert!(m.z_buf[center] < 0.0, "the hole must not write depth");
        Arc::get_mut(m.geom_materials[0].texture.as_mut().unwrap()).unwrap().pixels[0] = Color32::RED;
        rasterize_mesh(&mut m, 100, 100);
        assert!(m.color_buf[center].r() > 0 && m.z_buf[center] > 0.0, "the wire still writes depth");
    }

    #[test]
    fn blended_layers_respect_depth_and_decal_order() {
        // Submit the near glass first, the far glass last, and a decal in the
        // exact plane of the opaque surface.
        let verts = [-0.2, 0.0, 0.0, -0.1, 0.2].into_iter()
            .flat_map(|y| [[-1.0, y, -1.0], [1.0, y, -1.0], [0.0, y, 1.0]])
            .collect();
        let mut m = Mesh3D::from_parts(
            "layers".into(), verts, vec![[0.0; 2]; 15], (0..5).map(|i| [i * 3, i * 3 + 1, i * 3 + 2]).collect(),
            (0..5).collect(), vec![], None, String::new(), 5, 0, String::new(),
        );
        m.yaw = 0.0;
        m.pitch = 0.0;
        let glass = MaterialPass::Glass { texture_alpha: true };
        m.geom_materials = [
            ([255, 0, 0, 128], glass), ([0, 0, 255, 255], MaterialPass::Opaque),
            ([0, 255, 0, 128], MaterialPass::Decal), ([0, 255, 0, 128], glass),
            ([255, 255, 255, 255], glass),
        ].into_iter().map(|([r, g, b, a], pass)| GeomMaterial {
            texture: Some(Arc::new(Texture2D { w: 1, h: 1, pixels: vec![Color32::from_rgba_unmultiplied(r, g, b, a)] })),
            pass,
            ..Default::default()
        }).collect();
        let center = 50 * 100 + 50;
        rasterize_mesh(&mut m, 100, 100);
        let composited = m.color_buf[center];
        assert_eq!(m.z_buf[center], 0.0, "only the opaque layer writes depth");
        assert!(composited.r() > composited.g() && composited.b() > 0, "near red is over green, with blue still visible");
        m.geom_materials[4].texture = None;
        m.geom_materials[4].pass = MaterialPass::Opaque;
        rasterize_mesh(&mut m, 100, 100);
        assert_eq!(m.color_buf[center], composited, "the hidden layer must not affect the pixel");
        m.geom_materials[2].texture = None;
        m.geom_materials[2].auxiliary_pass = true;
        rasterize_mesh(&mut m, 100, 100);
        assert!(m.color_buf[center].g() < composited.g(), "a coplanar decal blends over its base");
    }

    #[test]
    fn model_name_and_vehicle_filter() {
        let mut buf = vec![0u8; 30_000];
        buf[100..116].copy_from_slice(b"test_vehicle.#ft");
        buf[500..508].copy_from_slice(b"wheel_lf");
        let (nm, ext) = extract_rage9_model_name(&buf).unwrap();
        assert_eq!(nm, "test_vehicle.#ft");
        assert_eq!(ext, ".#ft");
        assert!(is_vehicle_virtual(&buf, &nm));
        assert!(!is_vehicle_virtual(&buf, "test_vehicle_spoiler_01.#ft"));
    }
}

#[cfg(test)]
mod dictionary_decode_tests {
    use super::*;

    /// Props keep several LOD levels in one file; each decodes and detail never increases
    /// (test from asm512's fork, rpatty-dev/PercyStar#19).
    #[test]
    fn in_file_lod_levels_decode() {
        let Ok(rpf) = std::env::var("PERCYSTAR_LEVELS_RPF") else { return };
        let Some(root) = Path::new(&rpf).parent() else { return };
        let pack = root.join("pack").join("shared_minigame000").join("shared_minigame000.rpf");
        if !pack.exists() {
            return;
        }
        for (tag, off, want) in [
            ("td_resi_wall_windows_b_3", 798_629_904u64, [6usize, 6, 6, 5]),
            ("a_td_resdoor_double_l", 800_751_632u64, [8, 8, 8, 5]),
        ] {
            let base = decode_rage9_model_from_rpf(&pack, off, tag).expect("decode lod0");
            assert_eq!(base.lod, 0);
            assert_eq!(base.lod_geoms, want, "{tag} lod table");
            let mut prev_tris = base.tris.len();
            for l in 1..MAX_LODS {
                let m = decode_rage9_model_from_rpf_lod(&pack, off, tag, l).expect("decode lod");
                eprintln!("[{tag}] {} geoms={} tris={}", m.lod_label, m.geom_count, m.tris.len());
                assert_eq!((m.lod, m.geom_count), (l, want[l]));
                assert!(!m.tris.is_empty() && m.tris.len() <= prev_tris, "{tag} lod{l} gained triangles");
                prev_tris = m.tris.len();
            }
            assert!(prev_tris < base.tris.len(), "{tag} lowest lod should be lighter than lod0");
        }
    }

    /// Vehicles split detail across `_hi`, `_mid` and base files; the mid file adds a second level.
    #[test]
    fn vehicle_detail_files_decode() {
        let Ok(rpf) = std::env::var("PERCYSTAR_LEVELS_RPF") else { return };
        if !Path::new(&rpf).exists() {
            return;
        }
        let mut prev_tris = usize::MAX;
        for (tag, off, want) in [
            ("cheetah2_hi", 74_053_521_424u64, [35usize, 0, 0, 0]),
            ("cheetah2_mid", 74_058_629_136u64, [34, 30, 0, 0]),
            ("cheetah2", 74_051_276_816u64, [3, 0, 0, 0]),
        ] {
            let m = decode_rage9_model_from_rpf(Path::new(&rpf), off, tag).expect("decode vehicle");
            assert_eq!(m.lod_geoms, want, "{tag} lod table");
            assert!(m.tris.len() < prev_tris, "{tag} should be lighter than the file above");
            prev_tris = m.tris.len();
            // a missing level falls back to the nearest populated one
            let last = want.iter().rposition(|&n| n > 0).unwrap();
            let low = decode_rage9_model_from_rpf_lod(Path::new(&rpf), off, tag, 3).expect("fallback");
            eprintln!("[{tag}] lod0 tris={} lowest {} tris={}", m.tris.len(), low.lod_label, low.tris.len());
            assert_eq!(low.lod, last);
            if last > 0 {
                assert!(low.tris.len() < m.tris.len(), "{tag} lower level should be lighter");
                prev_tris = low.tris.len();
            } else {
                assert_eq!(low.tris.len(), m.tris.len());
            }
        }
    }

    #[test]
    fn glass_props_blend_without_filling_the_depth_buffer() {
        let Ok(rpf) = std::env::var("PERCYSTAR_LEVELS_RPF") else { return };
        let path = Path::new(&rpf);
        if !path.exists() { return; }
        for (tag, off, texture_alpha) in [
            ("glass_door", 68_851_257_360, true),
            ("glass_table", 69_061_394_448, false),
            // its 4x4 tint map is a constant white at 25% opacity
            ("wine_glass", 67_766_120_464, true),
        ] {
            let mut m = decode_rage9_model_from_rpf(path, off, tag).unwrap();
            assert!(m.geom_materials.iter().any(|g| g.pass == MaterialPass::Glass { texture_alpha }), "{tag}");
            rasterize_mesh(&mut m, 800, 600);
            let blended = m.color_buf.chunks_exact(800).zip(m.z_buf.chunks_exact(800))
                .map(|(row, depths)| row.iter().zip(depths).filter(|&(c, z)| !z.is_finite() && *c != row[0]).count())
                .sum::<usize>();
            assert!(blended > 1_000, "{tag} should show glass without any opaque surface at {blended} pixels");
            render_sample(m, tag);
        }
        let m = decode_rage9_model_from_rpf(path, 70_451_695_632, "floodlight").unwrap();
        assert!(m.geom_materials.iter().any(|g| g.pass == MaterialPass::Decal
            && g.texture.as_ref().is_some_and(|t| t.pixels.iter().any(|p| p.a() == 0))));
        render_sample(m, "floodlight");
    }

    #[test]
    fn chainlink_preview_excludes_shadow_meshes_and_keeps_wire_holes() {
        let Ok(rpf) = std::env::var("PERCYSTAR_LEVELS_RPF") else { return };
        let path = Path::new(&rpf);
        if !path.exists() { return; }
        for (tag, off, proxies) in [
            ("01e", 70_271_725_584, 2), ("01a", 70_270_861_328, 2), ("01h", 70_272_725_008, 2),
            ("02e", 70_274_981_904, 2), ("01e_batchlod", 70_267_998_224, 0),
        ] {
            let mut m = decode_rage9_model_from_rpf(path, off, tag).unwrap();
            assert!(m.name.starts_with(&format!("prop_am_fence_chainlink_{tag}.")), "{}", m.name);
            assert_eq!(m.geom_materials.iter().filter(|mat| mat.auxiliary_pass).count(), proxies, "{tag}");
            rasterize_mesh(&mut m, 800, 600);
            let textured = m.z_buf.iter().filter(|z| z.is_finite()).count();
            m.shade_mode = ShadeMode::Solid;
            rasterize_mesh(&mut m, 800, 600);
            let solid = m.z_buf.iter().filter(|z| z.is_finite()).count();
            assert!(textured > 1_000 && textured * 2 < solid, "{tag}: {textured} textured vs {solid} solid pixels");
            m.shade_mode = ShadeMode::Textured;
            render_sample(m, &format!("fence_{tag}"));
        }
    }

    fn render_sample(mut mesh: Mesh3D, tag: &str) {
        let Ok(dir) = std::env::var("PERCYSTAR_RENDER_DIR") else { return };
        fs::create_dir_all(&dir).unwrap();
        rasterize_mesh(&mut mesh, 800, 600);
        let rgba: Vec<_> = mesh.color_buf.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
        image::save_buffer(Path::new(&dir).join(format!("{tag}.png")), &rgba, 800, 600, image::ColorType::Rgba8)
            .expect("save render");
    }

    #[test]
    fn model_materials_find_shared_textures_from_pack_archives() {
        let Ok(levels) = std::env::var("PERCYSTAR_LEVELS_RPF") else { return };
        let Some(root) = Path::new(&levels).parent() else { return };
        let rpf = root.join("pack/shared_minigame000/shared_minigame000.rpf");
        if !rpf.exists() {
            return;
        }
        let mesh = decode_rage9_model_from_rpf(&rpf, 362_385_424, "pack model").unwrap();
        assert_eq!(mesh.name, "a_bfyb01_lgs_lvl1_bits.#dr");
        assert_eq!(mesh.geom_materials.iter().filter(|mat| mat.texture.is_some()).count(), 6, "{}", mesh.tex_status);
        assert!(mesh.uvs.iter().any(|uv| uv[0] < 0.0), "UV0 includes signed fixed-point coordinates");
        eprintln!("[pack model] {}", mesh.tex_status);
        render_sample(mesh, "pack_model");
    }

    #[test]
    fn single_entry_dictionary_points_at_its_drawable() {
        // root: +0x20 hash array {ptr, 1}, +0x30 drawable array {ptr, 1}; drawable +0x50 set
        let mut v = vec![0u8; 0x200];
        let put = |v: &mut Vec<u8>, at: usize, q: u64| v[at..at + 8].copy_from_slice(&q.to_le_bytes());
        put(&mut v, 0x20, 0x5000_0100);
        v[0x28] = 1;
        put(&mut v, 0x30, 0x5000_0108);
        v[0x38] = 1;
        put(&mut v, 0x108, 0x5000_0110);
        put(&mut v, 0x110 + 0x50, 0x5000_0100);
        assert_eq!(single_dictionary_drawable(&v), Some(0x110));
        assert!(drawable_dictionary_entries(&v).is_empty(), "one entry is handled as a plain drawable");
        put(&mut v, 0x30, 0x5000_01fc);
        assert_eq!(single_dictionary_drawable(&v), None, "truncated pointer array");
        put(&mut v, 0x30, 0x5000_0108);
        put(&mut v, 0x20, 0x5000_0200);
        assert_eq!(single_dictionary_drawable(&v), None, "out-of-range hash array");
        put(&mut v, 0x20, 0x5000_0100);
        v[0x38] = 2;
        assert_eq!(single_dictionary_drawable(&v), None);
    }

    /// Ped eyes (and heads, clothing, animal parts) are dictionaries holding one drawable,
    /// whose shader group lives on that drawable. The eye shader's diffuse is an iris on a
    /// transparent background, baked over a sclera colour for the preview.
    #[test]
    fn ped_eyes_get_their_iris_texture() {
        let Ok(levels) = std::env::var("PERCYSTAR_LEVELS_RPF") else { return };
        let Some(root) = Path::new(&levels).parent() else { return };
        let rpf = root.join("packs.rpf");
        if !rpf.exists() {
            return;
        }
        let mesh = decode_rage9_model_from_rpf(&rpf, 2_714_169_360, "eyes").unwrap();
        assert_eq!(mesh.name, "head_mr1_cau_000_m_eyes_full.#dr");
        let tex = mesh.geom_materials.iter().find_map(|m| m.texture.clone()).expect(&mesh.tex_status);
        let n = tex.pixels.len() as u64;
        let mean = tex.pixels.iter().map(|p| p.r() as u64 + p.g() as u64 + p.b() as u64).sum::<u64>() / (3 * n);
        assert!(mean > 150, "sclera should be light, mean {mean}");
        assert!(tex.pixels.iter().any(|p| p.r() < 40 && p.g() < 40 && p.b() < 40), "dark pupil");
        render_sample(mesh, "ped_eyes");
    }

    #[test]
    fn unused_embedded_thumbnail_does_not_replace_streamed_materials() {
        let Ok(rpf) = std::env::var("PERCYSTAR_LEVELS_RPF") else { return };
        if !Path::new(&rpf).exists() {
            return;
        }
        // The embedded 4x4 texture is not bound to these shaders.
        let mesh = decode_rage9_model_from_rpf(Path::new(&rpf), 74_960_211_984, "siren").unwrap();
        assert_eq!(mesh.name, "cmn_siren_int_rear_r_bar_01_x4_hi.#ft");
        assert!(mesh.texture.is_none(), "don't paint the whole asset with the embedded texture");
        assert!(mesh.geom_materials.iter().filter_map(|m| m.texture.as_ref()).any(|t| t.w > 4), "{}", mesh.tex_status);
        eprintln!("[siren] {}", mesh.tex_status);
        render_sample(mesh, "siren");
    }

    /// Regression for bk_02_road_str_slod.#dr (levels.rpf, Xbox build): a drawable dictionary
    /// of 8 Brickell SLOD blocks that used to render stacked at the origin, with towers on
    /// their side because the +0x6C position axis mode was ignored. Runs only when
    /// PERCYSTAR_LEVELS_RPF points at a levels.rpf you supply.
    #[test]
    fn brickell_slod_dictionary_splits_and_stands_upright() {
        let Ok(rpf) = std::env::var("PERCYSTAR_LEVELS_RPF") else { return };
        if !Path::new(&rpf).exists() {
            return;
        }
        let m = decode_rage9_model_from_rpf(Path::new(&rpf), 8_679_358_480, "x").expect("decode dictionary");
        assert_eq!(m.parts.len(), 8, "expected 8 drawables");
        assert_eq!(m.geom_count, 317);
        assert!(m.uvs.iter().flatten().any(|uv| (uv * 8192.0).fract() != 0.0), "expected float2 UV streams alongside fixed-point streams");
        let tower = m.parts.iter().find(|p| p.name == "bk_04_slod.#dr").expect("bk_04_slod part");
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for t in &m.tris[tower.tris.clone()] {
            for &i in t {
                for k in 0..3 {
                    lo[k] = lo[k].min(m.verts[i as usize][k]);
                    hi[k] = hi[k].max(m.verts[i as usize][k]);
                }
            }
        }
        // Drawable bbox: x 224 m, y 197 m, z 208 m (towers). Mis-decoded axes swap these.
        let ext = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
        assert!((ext[0] - 224.2).abs() < 2.0 && (ext[1] - 197.1).abs() < 2.0 && (ext[2] - 207.7).abs() < 2.0, "extents {ext:?}");
    }

    // texture layers start as one color texture per material and can be switched individually
    #[test]
    fn vehicle_texture_layers_toggle() {
        let Ok(rpf) = std::env::var("PERCYSTAR_LEVELS_RPF") else { return };
        if !Path::new(&rpf).exists() {
            return;
        }
        let out_dir = std::env::var("PERCYSTAR_RENDER_DIR").ok();
        let mut m = decode_rage9_model_from_rpf(Path::new(&rpf), 74_053_521_424, "cheetah2_hi").expect("decode");
        let count = |m: &Mesh3D, k: TexLayerKind| m.tex_layers.iter().filter(|l| l.kind == k).count();
        eprintln!(
            "layers color={} normal={} overlay={} default_on={}",
            count(&m, TexLayerKind::Color),
            count(&m, TexLayerKind::Normal),
            count(&m, TexLayerKind::Overlay),
            m.tex_layers.iter().filter(|l| l.default_on).count()
        );
        assert!(count(&m, TexLayerKind::Color) > 0 && count(&m, TexLayerKind::Normal) > 0 && count(&m, TexLayerKind::Overlay) > 0);
        assert!(m.tex_layers.iter().all(|l| l.enabled == l.default_on && l.mats > 0));
        let textured = |m: &Mesh3D| m.geom_materials.iter().filter(|g| g.texture.is_some()).count();
        let default_textured = textured(&m);
        assert_eq!(default_textured, 19, "default look keeps the 19 textured materials");
        assert!(m.geom_materials.iter().all(|g| g.normal.is_none() && g.overlays.is_empty()));
        let shot = |m: &mut Mesh3D, tag: &str| {
            rasterize_mesh(m, 800, 600);
            if let Some(d) = &out_dir {
                let _ = fs::create_dir_all(d);
                let rgba: Vec<u8> = m.color_buf.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
                image::save_buffer(Path::new(d).join(format!("layers_{tag}.png")), &rgba, 800, 600, image::ColorType::Rgba8).expect("save");
            }
            m.color_buf.clone()
        };
        let base = shot(&mut m, "default");

        m.set_tex_layers(|l| l.default_on || l.kind == TexLayerKind::Normal);
        assert!(m.geom_materials.iter().any(|g| g.normal.is_some()));
        assert_eq!(textured(&m), default_textured);
        let with_normals = shot(&mut m, "normals");
        assert!(with_normals != base, "normal maps should change the render");

        m.set_tex_layers(|l| l.default_on || l.label == "DustTexture");
        assert!(m.geom_materials.iter().any(|g| !g.overlays.is_empty()));
        assert!(shot(&mut m, "dust") != base, "an overlay should change the render");

        m.set_tex_layers(|_| false);
        assert_eq!(textured(&m), 0);
        assert!(m.geom_materials.iter().all(|g| g.normal.is_none() && g.overlays.is_empty()));
        assert!(m.has_paint_geom && m.geom_materials.iter().any(|g| g.is_vehicle_paint));
        shot(&mut m, "none");

        // one layer off only unbinds the materials that drew it
        m.set_tex_layers(|l| l.default_on);
        let draws = |m: &Mesh3D, li: usize| {
            let Some(t) = m.tex_layers[li].tex.as_ref() else { return 0 };
            m.geom_materials.iter().filter(|g| g.texture.as_ref().is_some_and(|x| Arc::ptr_eq(x, t))).count()
        };
        let first = (0..m.tex_layers.len()).find(|&li| draws(&m, li) > 0).expect("a drawn layer");
        m.tex_layers[first].enabled = false;
        m.apply_tex_layers();
        assert_eq!(draws(&m, first), 0, "a switched off layer is no longer drawn");
        m.set_tex_layers(|l| l.default_on);
        assert_eq!(textured(&m), default_textured);
        assert!(shot(&mut m, "default_again") == base, "returning to the default set reproduces the default render");
    }

    // vehicles bind their textures by hash into the ttpf pools
    #[test]
    fn vehicle_texture_refs_resolve() {
        let Ok(rpf) = std::env::var("PERCYSTAR_LEVELS_RPF") else { return };
        if !Path::new(&rpf).exists() {
            return;
        }
        for (tag, off) in [("cheetah2_hi", 74_053_521_424u64), ("cheetah2_mid", 74_058_629_136u64), ("cheetah2", 74_051_276_816u64)] {
            let refs = list_model_texture_refs(Path::new(&rpf), off).expect("list refs");
            let hits: Vec<_> = refs.iter().filter_map(|r| r.hit.as_ref().map(|h| (r, h))).collect();
            let bytes: usize = hits.iter().map(|(_, h)| h.w * h.h * 4).sum();
            eprintln!("[{tag}] unique={} resolved={} rgba_mb={}", refs.len(), hits.len(), bytes >> 20);
            if std::env::var("PERCYSTAR_TEX_DUMP").is_ok() {
                for r in &refs {
                    match &r.hit {
                        Some(h) => eprintln!("   {:<24} {:016x} {}x{} dxgi{} {} #{} x{}", r.role(), r.hash64, h.w, h.h, h.dxgi, h.path.file_name().unwrap().to_string_lossy(), h.tex_idx, r.shaders),
                        None => eprintln!("   {:<24} {:016x} unresolved x{}", r.role(), r.hash64, r.shaders),
                    }
                }
            }
            assert!(hits.len() >= 3, "{tag} should resolve textures");
            assert!(refs.iter().any(|r| r.role() == "diffuseTex" && r.hit.is_some()), "{tag} should resolve a diffuse texture");
            assert!(hits.iter().all(|(r, _)| r.shaders > 0 && !r.params.is_empty()));
            let (first, hit) = hits[0];
            let tex = crate::tex::decode_ttpf_texture(&hit.path, hit.tex_idx).expect("decode first ref");
            assert_eq!((tex.w, tex.h), (hit.w, hit.h), "{tag} {:016x}", first.hash64);
        }
    }

    #[test]
    fn vehicle_option_fragments_compose() {
        let Ok(rpf) = std::env::var("PERCYSTAR_LEVELS_RPF") else { return };
        if !Path::new(&rpf).exists() {
            return;
        }
        let mut car = decode_rage9_model_from_rpf(Path::new(&rpf), 73_817_919_504, "stanierg2_hi")
            .expect("decode base vehicle");
        let option = decode_rage9_model_from_rpf(Path::new(&rpf), 73_831_055_376, "stanierg2_siren_roofbar1_mount_01_hi")
            .expect("decode separate roofbar mount");
        let (verts, tris) = (car.verts.len(), car.tris.len());
        car.append_vehicle_part(option);
        assert!(car.verts.len() > verts && car.tris.len() > tris);
        assert!(car.parts.iter().any(|p| p.name.contains("siren_roofbar")));
        assert!(!car.has_els, "a mount point is not itself an emergency light");

        let roofbar = decode_rage9_model_from_rpf(Path::new(&rpf), 74_963_226_640, "cmn_siren_roofbar1_02_hi")
            .expect("decode shared roofbar");
        car.append_vehicle_part(roofbar);
        assert!(car.has_els && car.els_points.len() >= 12);
        assert!(car.els_points.iter().all(|p| p.position[2] > 0.7), "roofbar emitters attach at the vehicle mount");
    }

    // external ttpf material resolution on vehicle paint and world slod
    #[test]
    fn ttpf_vehicle_and_world_materials() {
        let Ok(rpf) = std::env::var("PERCYSTAR_LEVELS_RPF") else { return };
        if !Path::new(&rpf).exists() {
            return;
        }
        let cases = [
            ("cheetah2", 74_051_276_816u64),
            ("cheetah2_hi", 74_053_521_424u64),
            ("ambulance_hi", 71_757_271_056u64),
            ("firetruk2_hi", 74_492_911_632u64),
            ("brickell_slod", 8_679_358_480u64),
            ("fern", 71_430_041_616u64),
            ("dress", 67_927_576_592u64),
        ];
        for (tag, off) in cases {
            let mut m = decode_rage9_model_from_rpf(Path::new(&rpf), off, tag).expect("decode model");
            let tex_mats = m.geom_materials.iter().filter(|gm| gm.texture.is_some()).count();
            let paint_mats = m.geom_materials.iter().filter(|gm| gm.is_vehicle_paint).count();
            eprintln!("[{tag}] status='{}' geoms={} tex_mats={} paint_mats={}", m.tex_status, m.geom_count, tex_mats, paint_mats);
            if tag.starts_with("cheetah2") {
                assert!(tex_mats > 0, "{tag} should resolve external TTPF textures");
                assert!(paint_mats > 0 && m.has_paint_geom, "{tag} should detect vehicle paint geoms");
            } else if tag == "ambulance_hi" {
                assert_eq!(m.els_points.len(), 37, "ambulance should expose its siren1..37 bones");
            } else if tag == "firetruk2_hi" {
                assert!(!m.els_points.is_empty(), "firetruck should expose prefixed siren bones");
            } else {
                assert!(tex_mats > 0, "{tag} should resolve external TTPF textures");
            }
            if m.has_els {
                m.els_enabled = true;
                m.els_flash = false;
            }
            render_sample(m, tag);
        }
    }
}
