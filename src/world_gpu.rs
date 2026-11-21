use crate::model3d::{Mesh3D, Texture2D};
use crate::worldmap::WorldMapData;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec3};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Instant;
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GpuVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GpuInstance {
    pub model_0: [f32; 4],
    pub model_1: [f32; 4],
    pub model_2: [f32; 4],
    pub model_3: [f32; 4],
    pub color: [f32; 4],
}

impl GpuInstance {
    pub fn from_mat4(m: Mat4, color: [f32; 4]) -> Self {
        let cols = m.to_cols_array_2d();
        Self {
            model_0: cols[0],
            model_1: cols[1],
            model_2: cols[2],
            model_3: cols[3],
            color,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameUniforms {
    view_proj: [[f32; 4]; 4],
    light_vp: [[f32; 4]; 4],
    cam_pos: [f32; 4],
    sun_dir: [f32; 4],
    sky_zenith: [f32; 4],
    sky_horizon: [f32; 4],
    flags: [f32; 4], // x=rt_on, y=time_hours, z=map_min_x, w=map_span_x
    map_y: [f32; 4], // x=map_min_y, y=map_span_y, z=viewport_w, w=viewport_h
    hud: [f32; 4],   // x=load_progress, y=loading_active, z=elapsed_sec, w=sgd5_count
}

pub struct WorldBatchInput {
    pub mesh: Mesh3D,
    pub instances: Vec<(Vec3, Quat, Vec3, [f32; 4])>,
}

pub struct WorldLaunchConfig {
    pub terrain: Option<Arc<WorldMapData>>,
    pub batches: Vec<WorldBatchInput>,
    pub levels_rpf: Option<std::path::PathBuf>,
    pub map_count: usize,
    pub start_pos: [f32; 3],
}

pub enum WorldStreamMsg {
    Batch(WorldBatchInput),
    MapCount(usize),
    TotalArchetypes(usize),
    ArchetypeDone,
    Sgd5Count(usize),
}

fn count_sgd5_shaders_in_rpf(shaders_rpf: &Path) -> usize {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(shaders_rpf) else { return 0 };
    let mut buf = vec![0u8; 8 * 1024 * 1024];
    let mut total = 0usize;
    loop {
        let Ok(n) = f.read(&mut buf) else { break };
        if n < 16 { break; }
        for w in buf[..n].windows(4) {
            if w == b"SGD5" {
                total += 1;
            }
        }
    }
    total
}

pub fn launch_world_gpu_process(common_rpf: Option<&Path>, levels_rpf: Option<&Path>) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--gpu-world");
    if let Some(c) = common_rpf {
        cmd.arg("--common").arg(c);
    }
    if let Some(l) = levels_rpf {
        cmd.arg("--levels").arg(l);
    }
    cmd.spawn().map_err(|e| format!("Failed to launch 3D GPU World: {e}"))?;
    Ok(())
}

pub fn run_gpu_world_cli(common_rpf: Option<&Path>, levels_rpf: Option<&Path>) {
    let fallback_dir = crate::load_last_folder();
    let resolved_common = common_rpf.map(Path::to_path_buf).or_else(|| {
        fallback_dir.as_ref().map(|d| d.join("common.rpf")).filter(|p| p.exists())
    });
    let resolved_levels = levels_rpf.map(Path::to_path_buf).or_else(|| {
        fallback_dir.as_ref().map(|d| d.join("levels.rpf")).filter(|p| p.exists())
    });
    let terrain = resolved_common
        .as_deref()
        .and_then(|p| crate::worldmap::load_mhiu_from_rpf(p).ok())
        .map(Arc::new);
    let cfg = WorldLaunchConfig {
        terrain,
        batches: Vec::new(),
        levels_rpf: resolved_levels,
        map_count: 0,
        start_pos: [-2351.0, -5533.0, 140.0],
    };
    let Ok(event_loop) = EventLoop::builder().build() else {
        return;
    };
    let (tx, rx) = mpsc::channel();
    if let Some(lp) = cfg.levels_rpf.clone() {
        let start = cfg.start_pos;
        std::thread::spawn(move || {
            // 1. fast-load brickell slod city blocks immediately
            if let Ok(mesh) = crate::model3d::decode_rage9_model_from_rpf(&lp, 8_679_358_480, "brickell_slod") {
                let _ = tx.send(WorldStreamMsg::Batch(WorldBatchInput {
                    mesh,
                    instances: vec![(Vec3::ZERO, Quat::IDENTITY, Vec3::ONE, [1.0; 4])],
                }));
            }
            // 2. build/load cached 0DRPz map placements + model index and stream all levels nearest-first
            eprintln!("[world-gpu] loading/building world index from {}", lp.display());
            let ix = crate::mapdata::build_or_load_world_index(&lp);
            eprintln!("[world-gpu] index ready: {} maps, {} models, {} entities", ix.map_count, ix.models.len(), ix.entities.len());
            let _ = tx.send(WorldStreamMsg::MapCount(ix.map_count));
            let mut arch_groups: Vec<(u32, u64, f32, Vec<(Vec3, Quat, Vec3, [f32; 4])>)> = Vec::new();
            let mut grouped: HashMap<u32, (f32, Vec<(Vec3, Quat, Vec3, [f32; 4])>)> = HashMap::new();
            for e in &ix.entities {
                if !ix.models.contains_key(&e.archetype_hash) {
                    continue;
                }
                let dx = e.pos[0] - start[0];
                let dy = e.pos[1] - start[1];
                let d2 = dx * dx + dy * dy;
                let q = Quat::from_xyzw(e.rot[0], e.rot[1], e.rot[2], e.rot[3]).normalize();
                let sc = Vec3::new(e.scale_xy, e.scale_xy, e.scale_z);
                let entry = grouped.entry(e.archetype_hash).or_insert_with(|| (d2, Vec::new()));
                if d2 < entry.0 {
                    entry.0 = d2;
                }
                entry.1.push((Vec3::from_array(e.pos), q, sc, [1.0; 4]));
            }
            for (h, (d2, insts)) in grouped {
                if let Some(&moff) = ix.models.get(&h) {
                    arch_groups.push((h, moff, d2, insts));
                }
            }
            eprintln!("[world-gpu] matched {} unique drawable archetypes to stream", arch_groups.len());
            arch_groups.sort_by(|a, b| a.2.total_cmp(&b.2));
            let _ = tx.send(WorldStreamMsg::TotalArchetypes(arch_groups.len()));
            if let Some(sp) = lp.parent().map(|d| d.join("shaders.rpf")).filter(|p| p.exists()) {
                let tx_s = tx.clone();
                std::thread::spawn(move || {
                    let n = count_sgd5_shaders_in_rpf(&sp);
                    eprintln!("[world-gpu] indexed {} SGD5 shader packs from {}", n, sp.display());
                    let _ = tx_s.send(WorldStreamMsg::Sgd5Count(n));
                });
            }
            let groups = Arc::new(arch_groups);
            let cursor = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let n_workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8).clamp(2, 8);
            std::thread::scope(|s| {
                for _ in 0..n_workers {
                    let groups = Arc::clone(&groups);
                    let cursor = Arc::clone(&cursor);
                    let tx = tx.clone();
                    let lp = lp.clone();
                    s.spawn(move || loop {
                        let i = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= groups.len() {
                            break;
                        }
                        let (h, moff, _, ref insts) = groups[i];
                        if let Ok(mesh) = crate::model3d::decode_rage9_model_from_rpf(&lp, moff, &format!("arch_{h:08x}")) {
                            let lo = mesh.name.to_ascii_lowercase();
                            if !(lo.ends_with("_shadow") || lo.ends_with("_pxy") || lo.ends_with("_lit_proxy")) {
                                if tx.send(WorldStreamMsg::Batch(WorldBatchInput { mesh, instances: insts.clone() })).is_err() {
                                    break;
                                }
                            }
                        }
                        if tx.send(WorldStreamMsg::ArchetypeDone).is_err() {
                            break;
                        }
                    });
                }
            });
        });
    }
    let mut app = GpuWorldApp::new(cfg, rx);
    let _ = event_loop.run_app(&mut app);
}

const WORLD_WGSL: &str = r#"
struct Uniforms {
    view_proj: mat4x4<f32>,
    light_vp: mat4x4<f32>,
    cam_pos: vec4<f32>,
    sun_dir: vec4<f32>,
    sky_zenith: vec4<f32>,
    sky_horizon: vec4<f32>,
    flags: vec4<f32>,
    map_y: vec4<f32>,
    hud: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

@group(1) @binding(0) var albedo_tex: texture_2d<f32>;
@group(1) @binding(1) var albedo_samp: sampler;

@group(2) @binding(0) var shadow_tex: texture_depth_2d;
@group(2) @binding(1) var shadow_samp: sampler_comparison;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) m0: vec4<f32>,
    @location(4) m1: vec4<f32>,
    @location(5) m2: vec4<f32>,
    @location(6) m3: vec4<f32>,
    @location(7) inst_col: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_n: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) inst_col: vec4<f32>,
    @location(4) shadow_coord: vec4<f32>,
};

@vertex
fn vs_shadow(in: VsIn) -> @builtin(position) vec4<f32> {
    let model = mat4x4<f32>(in.m0, in.m1, in.m2, in.m3);
    let wp = model * vec4<f32>(in.pos, 1.0);
    return u.light_vp * wp;
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let model = mat4x4<f32>(in.m0, in.m1, in.m2, in.m3);
    let wp = (model * vec4<f32>(in.pos, 1.0)).xyz;
    let wn = normalize((model * vec4<f32>(in.normal, 0.0)).xyz);
    var out: VsOut;
    out.clip_pos = u.view_proj * vec4<f32>(wp, 1.0);
    out.world_pos = wp;
    out.world_n = wn;
    out.uv = in.uv;
    out.inst_col = in.inst_col;
    out.shadow_coord = u.light_vp * vec4<f32>(wp, 1.0);
    return out;
}

fn sample_shadow(sc: vec4<f32>, ndl: f32) -> f32 {
    let proj = sc.xyz / max(sc.w, 0.0001);
    let uv = proj.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || proj.z > 1.0) {
        return 1.0;
    }
    let bias = max(0.0015 * (1.0 - ndl), 0.0003);
    let texel = 1.0 / 2048.0;
    var sum = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            sum += textureSampleCompare(
                shadow_tex,
                shadow_samp,
                uv + vec2<f32>(f32(x), f32(y)) * texel,
                proj.z - bias
            );
        }
    }
    return sum / 9.0;
}

fn aces_tonemap(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn neutral_col(c: vec3<f32>, k: f32) -> vec3<f32> {
    return mix(c, vec3<f32>(dot(c, vec3<f32>(0.2126, 0.7152, 0.0722))), k);
}

// cook-torrance pbr lighting adapted from sixng shader.wgsl
@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let tex_c = textureSample(albedo_tex, albedo_samp, in.uv);
    if (tex_c.a < 0.4) {
        discard;
    }
    let albedo = tex_c.rgb * in.inst_col.rgb;
    let n = normalize(in.world_n);
    let l = normalize(u.sun_dir.xyz);
    let v = normalize(u.cam_pos.xyz - in.world_pos);
    let h = normalize(l + v);

    let ndl = max(dot(n, l), 0.0);
    let ndh = max(dot(n, h), 0.0);
    let ndv = max(dot(n, v), 0.001);

    var sh = sample_shadow(in.shadow_coord, ndl);
    let rt_on = u.flags.x > 0.5;
    if (rt_on) {
        sh = smoothstep(0.08, 0.92, sh);
    }

    let rough = 0.62;
    let metal = 0.06;
    let a2 = rough * rough * rough * rough;
    let dd = ndh * ndh * (a2 - 1.0) + 1.0;
    let d_ggx = a2 / (3.14159 * dd * dd);
    let k = (rough + 1.0) * (rough + 1.0) / 8.0;
    let g_smith = (ndl / (ndl * (1.0 - k) + k)) * (ndv / (ndv * (1.0 - k) + k));
    let f0 = mix(vec3<f32>(0.04), albedo, metal);
    let fr = f0 + (1.0 - f0) * pow(1.0 - max(dot(h, v), 0.0), 5.0);
    let spec = d_ggx * g_smith * fr / max(4.0 * ndl * ndv, 0.001);
    let kd = (1.0 - fr) * (1.0 - metal);

    let sun_elev = clamp(l.z * 2.2, 0.0, 1.0);
    let sun_col = mix(vec3<f32>(1.0, 0.48, 0.22), vec3<f32>(1.05, 0.98, 0.90), sun_elev) * (2.2 * sun_elev + 0.18);
    let direct = (kd * albedo / 3.14159 + spec) * sun_col * ndl * sh * 2.4;

    let hemi = n.z * 0.5 + 0.5;
    let amb_d = mix(neutral_col(u.sky_horizon.rgb, 0.78) * 0.55, neutral_col(u.sky_zenith.rgb, 0.78), hemi);
    let fr_v = f0 + (max(vec3<f32>(1.0 - rough), f0) - f0) * pow(1.0 - ndv, 5.0);
    let env = mix(u.sky_horizon.rgb, u.sky_zenith.rgb, 0.5) * 0.45;
    let rt_bounce = select(0.0, 0.14 * (1.0 - ndv) * (0.4 + 0.6 * hemi), rt_on);

    let lit = direct + kd * albedo * amb_d + env * fr_v + amb_d * rt_bounce;

    let dist = length(u.cam_pos.xyz - in.world_pos);
    let fog = 1.0 - exp(-dist * 0.000028);
    let fog_col = mix(u.sky_horizon.rgb, u.sky_zenith.rgb, 0.25);
    let col = mix(lit, fog_col, clamp(fog, 0.0, 0.55));
    return vec4<f32>(aces_tonemap(col), 1.0);
}

@vertex
fn vs_sky(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
    var pts = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0)
    );
    return vec4<f32>(pts[vid], 0.9999, 1.0);
}

@fragment
fn fs_sky(@builtin(position) frag_pos: vec4<f32>) -> @location(0) vec4<f32> {
    let t = clamp(1.0 - frag_pos.y / max(u.map_y.w, 1.0), 0.0, 1.0);
    let col = mix(u.sky_horizon.rgb, u.sky_zenith.rgb, pow(t, 0.7));
    return vec4<f32>(aces_tonemap(col), 1.0);
}

struct MinimapOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_minimap(@builtin(vertex_index) vid: u32) -> MinimapOut {
    var quad = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0)
    );
    let q = quad[vid];
    let w = max(u.map_y.z, 640.0);
    let h = max(u.map_y.w, 480.0);
    let px = 18.0 + q.x * 190.0;
    let py = h - 18.0 - 190.0 + q.y * 190.0;
    let ndc_x = (px / w) * 2.0 - 1.0;
    let ndc_y = 1.0 - (py / h) * 2.0;
    var out: MinimapOut;
    out.pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = vec2<f32>(q.x, 1.0 - q.y);
    return out;
}

@fragment
fn fs_minimap(in: MinimapOut) -> @location(0) vec4<f32> {
    let c = textureSample(albedo_tex, albedo_samp, in.uv);
    let border = in.uv.x < 0.02 || in.uv.x > 0.98 || in.uv.y < 0.02 || in.uv.y > 0.98;
    if (border) {
        return vec4<f32>(0.92, 0.94, 0.98, 0.95);
    }
    let player_u = (u.cam_pos.x - u.flags.z) / max(u.flags.w, 1.0);
    let player_v = 1.0 - (u.cam_pos.y - u.map_y.x) / max(u.map_y.y, 1.0);
    let d = length(in.uv - vec2<f32>(player_u, player_v));
    if (d < 0.028) {
        return vec4<f32>(1.0, 0.66, 0.11, 1.0);
    }
    return vec4<f32>(c.rgb, 0.92);
}

struct WaterOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
};

@vertex
fn vs_water(@builtin(vertex_index) vid: u32) -> WaterOut {
    var quad = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0)
    );
    let p = quad[vid] * 26000.0;
    let wp = vec3<f32>(p.x, p.y, -0.5);
    var out: WaterOut;
    out.clip_pos = u.view_proj * vec4<f32>(wp, 1.0);
    out.world_pos = wp;
    return out;
}

@fragment
fn fs_water(in: WaterOut) -> @location(0) vec4<f32> {
    let dist = length(u.cam_pos.xyz - in.world_pos);
    let wave_fade = exp(-dist * 0.0015);
    let t = u.hud.z;
    let w1 = sin(in.world_pos.x * 0.11 + t * 1.4) * cos(in.world_pos.y * 0.13 - t * 1.1);
    let w2 = sin((in.world_pos.x * 0.07 - in.world_pos.y * 0.09) - t * 0.8);
    let n = normalize(vec3<f32>((w1 + w2) * 0.035 * wave_fade, (w1 - w2) * 0.035 * wave_fade, 1.0));
    let v = normalize(u.cam_pos.xyz - in.world_pos);
    let l = normalize(u.sun_dir.xyz);
    let h = normalize(l + v);
    let fres = pow(1.0 - max(dot(n, v), 0.0), 4.0);
    let sun_elev = clamp(l.z * 2.2, 0.0, 1.0);
    let deep = vec3<f32>(0.02, 0.11, 0.19) * (0.35 + 0.65 * sun_elev);
    let sky_r = mix(u.sky_horizon.rgb, u.sky_zenith.rgb, 0.45);
    let glint = pow(max(dot(n, h), 0.0), 140.0) * 1.6 * sun_elev;
    let water_col = mix(deep, sky_r, clamp(fres * 0.85 + 0.08, 0.0, 0.92)) + vec3<f32>(glint);
    let fog = clamp(1.0 - exp(-dist * 0.00010), 0.0, 0.90);
    let fog_col = mix(u.sky_horizon.rgb, u.sky_zenith.rgb, 0.25);
    return vec4<f32>(aces_tonemap(mix(water_col, fog_col, fog)), 1.0);
}

struct ProgOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_progress(@builtin(vertex_index) vid: u32) -> ProgOut {
    var quad = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0)
    );
    let q = quad[vid];
    let w = max(u.map_y.z, 640.0);
    let h = max(u.map_y.w, 480.0);
    let bw = min(460.0, w * 0.5);
    let bh = 14.0;
    let px = (w - bw) * 0.5 + q.x * bw;
    let py = 18.0 + q.y * bh;
    var out: ProgOut;
    out.pos = vec4<f32>((px / w) * 2.0 - 1.0, 1.0 - (py / h) * 2.0, 0.0, 1.0);
    out.uv = q;
    return out;
}

@fragment
fn fs_progress(in: ProgOut) -> @location(0) vec4<f32> {
    if (u.hud.y < 0.5) {
        discard;
    }
    let border = in.uv.x < 0.006 || in.uv.x > 0.994 || in.uv.y < 0.12 || in.uv.y > 0.88;
    if (border) {
        return vec4<f32>(0.85, 0.90, 0.96, 0.85);
    }
    if (in.uv.x <= clamp(u.hud.x, 0.0, 1.0)) {
        return vec4<f32>(0.22, 0.80, 0.98, 0.95);
    }
    return vec4<f32>(0.08, 0.10, 0.14, 0.80);
}
"#;

struct GpuMeshBatch {
    vb: wgpu::Buffer,
    ib: wgpu::Buffer,
    inst_buf: wgpu::Buffer,
    index_count: u32,
    inst_count: u32,
    tex_bind_group: wgpu::BindGroup,
    bound_center: Vec3,
    bound_radius: f32,
}

struct GpuState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    depth_view: wgpu::TextureView,
    shadow_view: wgpu::TextureView,
    uniform_buf: wgpu::Buffer,
    uniform_bg: wgpu::BindGroup,
    shadow_bg: wgpu::BindGroup,
    tex_bgl: wgpu::BindGroupLayout,
    tex_samp: wgpu::Sampler,
    white_bg: wgpu::BindGroup,
    shadow_pipeline: wgpu::RenderPipeline,
    main_pipeline: wgpu::RenderPipeline,
    sky_pipeline: wgpu::RenderPipeline,
    water_pipeline: wgpu::RenderPipeline,
    minimap_pipeline: wgpu::RenderPipeline,
    progress_pipeline: wgpu::RenderPipeline,
    minimap_bg: wgpu::BindGroup,
    batches: Vec<GpuMeshBatch>,
    tri_count: usize,
    instance_count: usize,
    model_count: usize,
    map_count: usize,
    stream_done: usize,
    stream_total: usize,
    sgd5_count: usize,
    start_instant: Instant,
    has_hw_rt: bool,
    map_min: [f32; 2],
    map_span: [f32; 2],
}

fn create_depth_view(device: &wgpu::Device, w: u32, h: u32) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("world_depth"),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

fn upload_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bgl: &wgpu::BindGroupLayout,
    samp: &wgpu::Sampler,
    tex: &Texture2D,
) -> wgpu::BindGroup {
    let mut raw = Vec::with_capacity(tex.w * tex.h * 4);
    for c in &tex.pixels {
        raw.extend_from_slice(&c.to_array());
    }
    let gpu_tex = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("albedo"),
            size: wgpu::Extent3d { width: tex.w.max(1) as u32, height: tex.h.max(1) as u32, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &raw,
    );
    let view = gpu_tex.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("tex_bg"),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(samp) },
        ],
    })
}

fn compute_smooth_normals(mesh: &Mesh3D) -> Vec<[f32; 3]> {
    let mut accum = vec![Vec3::ZERO; mesh.verts.len()];
    for (ti, &[a, b, c]) in mesh.tris.iter().enumerate() {
        let n = mesh.normals.get(ti).copied().unwrap_or([0.0, 0.0, 1.0]);
        let nv = Vec3::from_array(n);
        if (a as usize) < accum.len() { accum[a as usize] += nv; }
        if (b as usize) < accum.len() { accum[b as usize] += nv; }
        if (c as usize) < accum.len() { accum[c as usize] += nv; }
    }
    accum
        .into_iter()
        .map(|n| {
            let nn = n.normalize_or_zero();
            if nn.length_squared() > 0.01 { nn.to_array() } else { [0.0, 0.0, 1.0] }
        })
        .collect()
}

impl GpuState {
    fn new(window: Arc<Window>, cfg: &WorldLaunchConfig) -> Self {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let surface = instance.create_surface(Arc::clone(&window)).unwrap();
        let adapter = pollster_block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("No GPU adapter found");

        let has_hw_rt = adapter.features().contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY);
        let mut req_features = wgpu::Features::empty();
        if has_hw_rt {
            req_features |= wgpu::Features::EXPERIMENTAL_RAY_QUERY;
        }
        let (device, queue) = pollster_block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("percystar_world_device"),
                required_features: req_features,
                required_limits: wgpu::Limits::default(),
                memory_hints: Default::default(),
            },
            None,
        ))
        .unwrap();

        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let depth_view = create_depth_view(&device, config.width, config.height);
        let shadow_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sun_shadow_map"),
            size: wgpu::Extent3d { width: 2048, height: 2048, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_view = shadow_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let shadow_samp = device.create_sampler(&wgpu::SamplerDescriptor {
            compare: Some(wgpu::CompareFunction::LessEqual),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame_uniforms"),
            size: std::mem::size_of::<FrameUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let u_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("u_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let uniform_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("u_bg"),
            layout: &u_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform_buf.as_entire_binding() }],
        });

        let shadow_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let shadow_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow_bg"),
            layout: &shadow_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&shadow_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&shadow_samp) },
            ],
        });

        let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tex_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let tex_samp = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world_shader"),
            source: wgpu::ShaderSource::Wgsl(WORLD_WGSL.into()),
        });

        let v_layouts = [
            wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<GpuVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2],
            },
            wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<GpuInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4],
            },
        ];

        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow_pl"),
            bind_group_layouts: &[&u_bgl, &tex_bgl],
            push_constant_ranges: &[],
        });

        let main_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("main_pl"),
            bind_group_layouts: &[&u_bgl, &tex_bgl, &shadow_bgl],
            push_constant_ranges: &[],
        });

        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow_pipe"),
            layout: Some(&shadow_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_shadow"), buffers: &v_layouts, compilation_options: Default::default() },
            fragment: None,
            primitive: wgpu::PrimitiveState { cull_mode: Some(wgpu::Face::Back), ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });

        let main_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("main_pipe"),
            layout: Some(&main_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_main"), buffers: &v_layouts, compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });

        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sky_pipe"),
            layout: Some(&main_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_sky"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_sky"),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });

        let water_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("water_pipe"),
            layout: Some(&main_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_water"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_water"),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });

        let minimap_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("minimap_pipe"),
            layout: Some(&main_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_minimap"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_minimap"),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });

        let progress_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("progress_pipe"),
            layout: Some(&main_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_progress"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_progress"),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });

        let white_tex = Texture2D { w: 1, h: 1, pixels: vec![eframe::egui::Color32::from_rgb(190, 195, 205)] };
        let white_bg = upload_texture(&device, &queue, &tex_bgl, &tex_samp, &white_tex);
        let minimap_bg = upload_texture(&device, &queue, &tex_bgl, &tex_samp, &white_tex);

        let mut st = Self {
            window,
            surface,
            device,
            queue,
            config,
            depth_view,
            shadow_view,
            uniform_buf,
            uniform_bg,
            shadow_bg,
            tex_bgl,
            tex_samp,
            white_bg,
            shadow_pipeline,
            main_pipeline,
            sky_pipeline,
            water_pipeline,
            minimap_pipeline,
            progress_pipeline,
            minimap_bg,
            batches: Vec::new(),
            tri_count: 0,
            instance_count: 0,
            model_count: 0,
            map_count: cfg.map_count,
            stream_done: 0,
            stream_total: 0,
            sgd5_count: 0,
            start_instant: Instant::now(),
            has_hw_rt,
            map_min: [-13000.0, -9000.0],
            map_span: [20000.0, 20000.0],
        };

        if let Some(ref terrain) = cfg.terrain {
            st.map_min = [terrain.bmin[0], terrain.bmin[1]];
            st.map_span = [terrain.bmax[0] - terrain.bmin[0], terrain.bmax[1] - terrain.bmin[1]];
            let tmesh = terrain.build_world_terrain_mesh(2);
            if let Some(ref t) = tmesh.texture {
                st.minimap_bg = upload_texture(&st.device, &st.queue, &st.tex_bgl, &st.tex_samp, t);
            }
            st.add_batch(&WorldBatchInput {
                mesh: tmesh,
                instances: vec![(Vec3::ZERO, Quat::IDENTITY, Vec3::ONE, [1.0; 4])],
            });
            st.model_count = st.model_count.saturating_sub(1);
        }

        for b in &cfg.batches {
            st.add_batch(b);
        }
        st
    }

    // splits mesh3d by tri_geom material so every ttpf texture and paint group renders cleanly
    fn add_batch(&mut self, b: &WorldBatchInput) {
        if b.mesh.verts.is_empty() || b.mesh.tris.is_empty() || b.instances.is_empty() {
            return;
        }
        let normals = compute_smooth_normals(&b.mesh);
        let insts: Vec<GpuInstance> = b
            .instances
            .iter()
            .map(|&(pos, rot, scale, col)| GpuInstance::from_mat4(Mat4::from_scale_rotation_translation(scale, rot, pos), col))
            .collect();
        let inst_buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("m_inst"),
            contents: bytemuck::cast_slice(&insts),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let mut by_geom: HashMap<u16, Vec<u32>> = HashMap::new();
        for (ti, &[i0, i1, i2]) in b.mesh.tris.iter().enumerate() {
            let g = if b.mesh.texture.is_some() { 0 } else { b.mesh.tri_geom.get(ti).copied().unwrap_or(0) };
            let v = by_geom.entry(g).or_default();
            v.push(i0);
            v.push(i1);
            v.push(i2);
        }

        let gv: Vec<GpuVertex> = b
            .mesh
            .verts
            .iter()
            .enumerate()
            .map(|(i, &pos)| GpuVertex {
                pos,
                normal: normals[i],
                uv: b.mesh.uvs.get(i).copied().unwrap_or([0.0, 0.0]),
            })
            .collect();
        let vb = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("m_vb"),
            contents: bytemuck::cast_slice(&gv),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let mut lmin = Vec3::splat(f32::MAX);
        let mut lmax = Vec3::splat(f32::MIN);
        for v in &b.mesh.verts {
            let p = Vec3::from_array(*v);
            lmin = lmin.min(p);
            lmax = lmax.max(p);
        }
        let local_center = (lmin + lmax) * 0.5;
        let local_rad = ((lmax - lmin) * 0.5).length();
        let mut wmin = Vec3::splat(f32::MAX);
        let mut wmax = Vec3::splat(f32::MIN);
        for &(pos, rot, scale, _) in &b.instances {
            let c = pos + rot * (local_center * scale);
            let r = local_rad * scale.abs().max_element();
            wmin = wmin.min(c - Vec3::splat(r));
            wmax = wmax.max(c + Vec3::splat(r));
        }
        let bound_center = (wmin + wmax) * 0.5;
        let bound_radius = ((wmax - wmin) * 0.5).length().max(1.0);

        for (g, indices) in by_geom {
            let ib = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("m_ib"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
            let tex_bg = b
                .mesh
                .texture
                .as_ref()
                .or_else(|| b.mesh.geom_materials.get(g as usize).and_then(|m| m.texture.as_deref()))
                .map(|t| upload_texture(&self.device, &self.queue, &self.tex_bgl, &self.tex_samp, t))
                .unwrap_or_else(|| self.white_bg.clone());

            self.batches.push(GpuMeshBatch {
                vb: vb.clone(),
                ib,
                inst_buf: inst_buf.clone(),
                index_count: indices.len() as u32,
                inst_count: insts.len() as u32,
                tex_bind_group: tex_bg,
                bound_center,
                bound_radius,
            });
        }

        self.tri_count += b.mesh.tris.len() * insts.len();
        self.instance_count += insts.len();
        self.model_count += 1;
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
        self.depth_view = create_depth_view(&self.device, w, h);
    }
}

struct GpuWorldApp {
    cfg: Option<WorldLaunchConfig>,
    terrain_ref: Option<Arc<WorldMapData>>,
    stream_rx: Receiver<WorldStreamMsg>,
    gpu: Option<GpuState>,
    cam_pos: Vec3,
    vel_z: f32,
    yaw: f32,
    pitch: f32,
    time_h: f32,
    auto_sun: bool,
    rt_on: bool,
    walk_mode: bool,
    keys: [bool; 8], // W A S D Q E Shift Space
    dragging: bool,
    last_cursor: Option<PhysicalPosition<f64>>,
    last_frame: Instant,
    last_title: Instant,
}

impl GpuWorldApp {
    fn new(cfg: WorldLaunchConfig, stream_rx: Receiver<WorldStreamMsg>) -> Self {
        Self {
            cam_pos: Vec3::from_array(cfg.start_pos),
            terrain_ref: cfg.terrain.clone(),
            cfg: Some(cfg),
            stream_rx,
            gpu: None,
            vel_z: 0.0,
            yaw: 0.4,
            pitch: -0.18,
            time_h: 10.50,
            auto_sun: false,
            rt_on: true,
            walk_mode: false,
            keys: [false; 8],
            dragging: false,
            last_cursor: None,
            last_frame: Instant::now(),
            last_title: Instant::now(),
        }
    }

    fn sample_terrain_z(&self, x: f32, y: f32) -> f32 {
        let Some(ref t) = self.terrain_ref else { return 0.0 };
        let u = ((x - t.bmin[0]) / (t.bmax[0] - t.bmin[0]).max(1.0)).clamp(0.0, 1.0);
        let v = ((y - t.bmin[1]) / (t.bmax[1] - t.bmin[1]).max(1.0)).clamp(0.0, 1.0);
        let ix = ((u * (t.w.saturating_sub(1)) as f32) as usize).min(t.w - 1);
        let iy = ((v * (t.h.saturating_sub(1)) as f32) as usize).min(t.h - 1);
        let h = t.heights[iy * t.w + ix] as f32 / 255.0;
        t.bmin[2] + h * (t.bmax[2] - t.bmin[2])
    }
}

impl ApplicationHandler for GpuWorldApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("PercyStar 3D World (wgpu)")
            .with_inner_size(winit::dpi::LogicalSize::new(1440.0, 860.0));
        let Ok(win) = event_loop.create_window(attrs) else {
            return;
        };
        let win = Arc::new(win);
        let cfg = self.cfg.take().unwrap_or(WorldLaunchConfig {
            terrain: None,
            batches: Vec::new(),
            levels_rpf: None,
            map_count: 0,
            start_pos: [0.0, 0.0, 120.0],
        });
        self.gpu = Some(GpuState::new(win, &cfg));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(sz) => {
                if let Some(g) = &mut self.gpu {
                    g.resize(sz.width, sz.height);
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left | MouseButton::Right, .. } => {
                self.dragging = state == ElementState::Pressed;
                if !self.dragging {
                    self.last_cursor = None;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.dragging {
                    if let Some(prev) = self.last_cursor {
                        let dx = (position.x - prev.x) as f32;
                        let dy = (position.y - prev.y) as f32;
                        self.yaw -= dx * 0.004;
                        self.pitch = (self.pitch - dy * 0.004).clamp(-1.45, 1.45);
                    }
                    self.last_cursor = Some(position);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let down = event.state == ElementState::Pressed;
                if let PhysicalKey::Code(code) = event.physical_key {
                    match code {
                        KeyCode::KeyW => self.keys[0] = down,
                        KeyCode::KeyA => self.keys[1] = down,
                        KeyCode::KeyS => self.keys[2] = down,
                        KeyCode::KeyD => self.keys[3] = down,
                        KeyCode::KeyQ => self.keys[4] = down,
                        KeyCode::KeyE => self.keys[5] = down,
                        KeyCode::ShiftLeft | KeyCode::ShiftRight => self.keys[6] = down,
                        KeyCode::Space => self.keys[7] = down,
                        KeyCode::F6 if down => self.auto_sun = !self.auto_sun,
                        KeyCode::KeyR if down => self.rt_on = !self.rt_on,
                        KeyCode::KeyG if down => {
                            self.walk_mode = !self.walk_mode;
                            self.vel_z = 0.0;
                        }
                        KeyCode::BracketRight if down => self.time_h = (self.time_h + 0.5) % 24.0,
                        KeyCode::BracketLeft if down => self.time_h = (self.time_h + 23.5) % 24.0,
                        _ => {}
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt = (now - self.last_frame).as_secs_f32().min(0.1);
                self.last_frame = now;

                if let Some(g) = &mut self.gpu {
                    for _ in 0..24 {
                        let Ok(msg) = self.stream_rx.try_recv() else { break };
                        match msg {
                            WorldStreamMsg::Batch(batch) => g.add_batch(&batch),
                            WorldStreamMsg::MapCount(mc) => g.map_count = mc,
                            WorldStreamMsg::TotalArchetypes(t) => g.stream_total = t,
                            WorldStreamMsg::ArchetypeDone => g.stream_done += 1,
                            WorldStreamMsg::Sgd5Count(n) => g.sgd5_count = n,
                        }
                    }
                }

                if self.auto_sun {
                    self.time_h = (self.time_h + dt * 0.35) % 24.0;
                }
                let fwd = Vec3::new(self.yaw.cos() * self.pitch.cos(), self.yaw.sin() * self.pitch.cos(), self.pitch.sin()).normalize();
                let fwd_flat = Vec3::new(self.yaw.cos(), self.yaw.sin(), 0.0).normalize();
                let right = fwd.cross(Vec3::Z).normalize_or(Vec3::Y);
                let move_fwd = if self.walk_mode { fwd_flat } else { fwd };
                let speed = if self.walk_mode {
                    if self.keys[6] { 45.0 } else { 14.0 }
                } else if self.keys[6] {
                    650.0
                } else {
                    140.0
                } * dt;
                if self.keys[0] { self.cam_pos += move_fwd * speed; }
                if self.keys[2] { self.cam_pos -= move_fwd * speed; }
                if self.keys[3] { self.cam_pos += right * speed; }
                if self.keys[1] { self.cam_pos -= right * speed; }
                if self.walk_mode {
                    let ground_z = self.sample_terrain_z(self.cam_pos.x, self.cam_pos.y) + 1.8;
                    self.vel_z -= 18.0 * dt;
                    self.cam_pos.z += self.vel_z * dt;
                    if self.cam_pos.z <= ground_z {
                        self.cam_pos.z = ground_z;
                        self.vel_z = if self.keys[7] { 8.5 } else { 0.0 };
                    }
                } else {
                    if self.keys[5] { self.cam_pos.z += speed; }
                    if self.keys[4] { self.cam_pos.z -= speed; }
                }

                let Some(g) = &mut self.gpu else { return };
                let az = (self.time_h / 24.0) * std::f32::consts::TAU - 1.2;
                let el = ((self.time_h - 6.0) / 12.0 * std::f32::consts::PI).sin() * 1.15;
                let sun_dir = Vec3::new(az.cos() * el.cos(), az.sin() * el.cos(), el.sin()).normalize();

                if now - self.last_title >= std::time::Duration::from_millis(120) {
                    self.last_title = now;
                    let rt_lbl = if !self.rt_on { "OFF" } else if g.has_hw_rt { "HW-RT" } else { "SS-RT/GI" };
                    let mode_lbl = if self.walk_mode { "WALK(G)" } else { "FLY(G)" };
                    let load_lbl = if g.stream_total > 0 && g.stream_done < g.stream_total {
                        format!(" | Load {}/{} ({:.0}%)", g.stream_done, g.stream_total, (g.stream_done as f32 / g.stream_total as f32) * 100.0)
                    } else if g.stream_total > 0 {
                        " | World Ready".to_string()
                    } else {
                        " | Indexing...".to_string()
                    };
                    let sgd_lbl = if g.sgd5_count > 0 { format!(" | SGD5: {}", g.sgd5_count) } else { String::new() };
                    g.window.set_title(&format!(
                        "PercyStar 3D World | pos {:.0} {:.0} {:.0} | {} | Sun az {:.1} el {:+.1} (F6) | Time {:.2}h | {} maps | {} models | {} instances | {:.2} M tris | RT: {}{}{}",
                        self.cam_pos.x, self.cam_pos.y, self.cam_pos.z, mode_lbl,
                        az.to_degrees().rem_euclid(360.0), el.to_degrees(),
                        self.time_h, g.map_count, g.model_count, g.instance_count,
                        g.tri_count as f64 / 1e6, rt_lbl, load_lbl, sgd_lbl
                    ));
                }

                let aspect = g.config.width as f32 / g.config.height.max(1) as f32;
                let proj = Mat4::perspective_rh(60f32.to_radians(), aspect, 0.5, 45000.0);
                let view = Mat4::look_to_rh(self.cam_pos, fwd, Vec3::Z);
                let view_proj = proj * view;

                let light_target = self.cam_pos + fwd * 300.0;
                let light_eye = light_target + sun_dir * 3500.0;
                let light_view = Mat4::look_at_rh(light_eye, light_target, Vec3::Z);
                let light_proj = Mat4::orthographic_rh(-1800.0, 1800.0, -1800.0, 1800.0, 50.0, 9000.0);
                let light_vp = light_proj * light_view;

                let day_fac = (sun_dir.z * 2.5).clamp(0.0, 1.0);
                let sky_zenith = [0.06 + day_fac * 0.20, 0.09 + day_fac * 0.42, 0.16 + day_fac * 0.72, 1.0];
                let sky_horizon = [0.14 + day_fac * 0.54, 0.16 + day_fac * 0.62, 0.24 + day_fac * 0.68, 1.0];
                let load_prog = if g.stream_total > 0 { (g.stream_done as f32 / g.stream_total as f32).clamp(0.0, 1.0) } else { 0.02 };
                let load_active = if g.stream_total == 0 || g.stream_done < g.stream_total { 1.0 } else { 0.0 };

                let uni = FrameUniforms {
                    view_proj: view_proj.to_cols_array_2d(),
                    light_vp: light_vp.to_cols_array_2d(),
                    cam_pos: [self.cam_pos.x, self.cam_pos.y, self.cam_pos.z, 1.0],
                    sun_dir: [sun_dir.x, sun_dir.y, sun_dir.z, 0.0],
                    sky_zenith,
                    sky_horizon,
                    flags: [if self.rt_on { 1.0 } else { 0.0 }, self.time_h, g.map_min[0], g.map_span[0]],
                    map_y: [g.map_min[1], g.map_span[1], g.config.width as f32, g.config.height as f32],
                    hud: [load_prog, load_active, g.start_instant.elapsed().as_secs_f32(), g.sgd5_count as f32],
                };
                g.queue.write_buffer(&g.uniform_buf, 0, bytemuck::bytes_of(&uni));

                let Ok(frame) = g.surface.get_current_texture() else { return };
                let view_tex = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
                let mut enc = g.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("world_enc") });

                {
                    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("shadow_pass"),
                        color_attachments: &[],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &g.shadow_view,
                            depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                            stencil_ops: None,
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    rp.set_pipeline(&g.shadow_pipeline);
                    rp.set_bind_group(0, &g.uniform_bg, &[]);
                    for b in &g.batches {
                        if b.bound_radius < 5000.0 && (b.bound_center - light_target).length_squared() > (2200.0 + b.bound_radius).powi(2) {
                            continue;
                        }
                        rp.set_bind_group(1, &b.tex_bind_group, &[]);
                        rp.set_vertex_buffer(0, b.vb.slice(..));
                        rp.set_vertex_buffer(1, b.inst_buf.slice(..));
                        rp.set_index_buffer(b.ib.slice(..), wgpu::IndexFormat::Uint32);
                        rp.draw_indexed(0..b.index_count, 0, 0..b.inst_count);
                    }
                }

                {
                    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("main_pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view_tex,
                            resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                        })],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &g.depth_view,
                            depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                            stencil_ops: None,
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    rp.set_pipeline(&g.sky_pipeline);
                    rp.set_bind_group(0, &g.uniform_bg, &[]);
                    rp.set_bind_group(1, &g.minimap_bg, &[]);
                    rp.set_bind_group(2, &g.shadow_bg, &[]);
                    rp.draw(0..3, 0..1);

                    rp.set_pipeline(&g.water_pipeline);
                    rp.draw(0..6, 0..1);

                    rp.set_pipeline(&g.main_pipeline);
                    for b in &g.batches {
                        if b.bound_radius < 5000.0 {
                            let to_b = b.bound_center - self.cam_pos;
                            if to_b.dot(fwd) < -(b.bound_radius + 150.0) {
                                continue;
                            }
                            if to_b.length_squared() > (7500.0 + b.bound_radius).powi(2) {
                                continue;
                            }
                        }
                        rp.set_bind_group(1, &b.tex_bind_group, &[]);
                        rp.set_vertex_buffer(0, b.vb.slice(..));
                        rp.set_vertex_buffer(1, b.inst_buf.slice(..));
                        rp.set_index_buffer(b.ib.slice(..), wgpu::IndexFormat::Uint32);
                        rp.draw_indexed(0..b.index_count, 0, 0..b.inst_count);
                    }

                    rp.set_pipeline(&g.minimap_pipeline);
                    rp.set_bind_group(1, &g.minimap_bg, &[]);
                    rp.draw(0..6, 0..1);

                    rp.set_pipeline(&g.progress_pipeline);
                    rp.draw(0..6, 0..1);
                }

                g.queue.submit(Some(enc.finish()));
                frame.present();
                g.window.request_redraw();
            }
            _ => {}
        }
    }
}

fn pollster_block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    fn dummy_raw() -> RawWaker {
        fn no_op(_: *const ()) {}
        fn clone(p: *const ()) -> RawWaker { RawWaker::new(p, &VTABLE) }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    let waker = unsafe { Waker::from_raw(dummy_raw()) };
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    loop {
        if let Poll::Ready(res) = fut.as_mut().poll(&mut cx) {
            return res;
        }
        std::thread::yield_now();
    }
}
