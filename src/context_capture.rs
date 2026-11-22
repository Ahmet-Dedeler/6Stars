// Percy6Stars by Ahmet Dedeler (https://github.com/Ahmet-Dedeler, https://ahmetdedeler.com)
// Fork of PercyStar by rpatty-dev (https://github.com/rpatty-dev/PercyStar).
// Source: https://github.com/Ahmet-Dedeler/Percy6Stars
// License: PercyStar Non-Commercial Attribution & Copyleft License v1.2 (see LICENSE)
// If you copy, vendor or port this file, keep this header and credit both projects in your README.
//! Offscreen stills of the context menu, the Related files panel and the copied AI context.
//!
//! Same approach as `demo_capture.rs`: the real `Rpf9App` in an `egui_kittest` harness with a
//! wgpu renderer, so no window opens, nothing takes focus and no sound plays. The listing is
//! made up (fictional archive and asset names) and no game files are read; the viewed model
//! is a generated box car handed to the preview the way a decoded file would be.
//!
//!   SIXSTARS_CONTEXT_OUT=/tmp/percy-context cargo test context_capture -- --ignored --nocapture
//!
//! Writes `context_menu.png`, `context_menu_multi.png`, `preview_menu.png`,
//! `related_panel.png`, `ai_context.md` and `ai_context.json`.

use crate::*;
use egui_kittest::kittest::{by, Queryable};
use egui_kittest::Harness;

const W: f32 = 1360.0;
const H: f32 = 820.0;
const PPP: f32 = 2.0;

fn to_points(r: egui::accesskit::Rect) -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(r.x0 as f32 / PPP, r.y0 as f32 / PPP), egui::pos2(r.x1 as f32 / PPP, r.y1 as f32 / PPP))
}

/// Box car: body, cabin and four wheels, Z up.
fn box_car(name: &str) -> Mesh3D {
    let boxes: [([f32; 3], [f32; 3]); 6] = [
        ([-2.2, -0.9, 0.35], [2.2, 0.9, 1.0]),
        ([-1.0, -0.8, 1.0], [1.0, 0.8, 1.55]),
        ([1.3, 0.85, 0.0], [1.95, 1.05, 0.65]),
        ([1.3, -1.05, 0.0], [1.95, -0.85, 0.65]),
        ([-1.95, 0.85, 0.0], [-1.3, 1.05, 0.65]),
        ([-1.95, -1.05, 0.0], [-1.3, -0.85, 0.65]),
    ];
    let (mut verts, mut tris, mut geoms) = (Vec::new(), Vec::new(), Vec::new());
    for (g, (a, b)) in boxes.iter().enumerate() {
        let base = verts.len() as u32;
        for i in 0..8u32 {
            verts.push([if i & 1 == 0 { a[0] } else { b[0] }, if i & 2 == 0 { a[1] } else { b[1] }, if i & 4 == 0 { a[2] } else { b[2] }]);
        }
        for f in [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]] {
            tris.push([base + f[0], base + f[1], base + f[2]]);
            tris.push([base + f[0], base + f[2], base + f[3]]);
            geoms.extend([g.min(1) as u16; 2]);
        }
    }
    let uvs = vec![[0.0, 0.0]; verts.len()];
    let mut m = Mesh3D::from_parts(name.into(), verts, uvs, tris, geoms, Vec::new(), None, String::new(), 2, 6, "LOD0".into());
    m.auto_spin = false;
    m
}

fn entries(root: &Path) -> Vec<RpfEntry> {
    let mut out = Vec::new();
    let mut push = |rpf: &str, name: &str, kind: &'static str, size: u64, tracks: u32| {
        out.push(RpfEntry {
            source_rpf: root.join(rpf),
            rpf_name: rpf.into(),
            name: name.into(),
            kind,
            offset: 0x40_0010 + out.len() as u64 * 0x1000,
            comp_size: size / 3,
            raw_size: size,
            tracks,
            chunks: Vec::new(),
        });
    };
    for (n, k, s) in [
        ("breeze_gt.#ft", "VEH", 2_312_448u64),
        ("breeze_gt_hi.#ft", "VEH", 3_901_440),
        ("breeze_gt_wheel_lf.#dr", "MODEL", 220_160),
        ("breeze_gt_wheel_lf_hi.#dr", "MODEL", 412_672),
        ("breeze_gt_bumper_f_01.#dr", "MODEL", 98_304),
        ("breeze_gt_spoiler_02.#dr", "MODEL", 64_512),
        ("breeze_gt_roofrack.#dr", "MODEL", 51_200),
        ("breeze_gt_batchlod.#dr", "MODEL", 30_720),
    ] {
        push("sample_vehicles.rpf", n, k, s, 0);
    }
    for (n, s) in [("prop_kiosk_01.#dr", 180_224u64), ("prop_kiosk_01_lod.#dr", 40_960), ("prop_kiosk_01_slod.#dr", 12_288), ("prop_bench_02.#dr", 66_560)] {
        push("sample_city.rpf", n, "MODEL", s, 0);
    }
    for n in [
        "cs_demo_head_fr1_000_m_full.#dr",
        "cs_demo_head_fr1_000_m_eyes_full.#dr",
        "cs_demo_hairbob_fr1_000_wavy_full.#dr",
        "cs_demo_jacket_fr1_002_slim_full.#dr",
        "jacket_fr1_002_slim_full.#dr",
        "char_bipedbounds_female_jacket_fr1_002_slim_full.#ft",
    ] {
        push("sample_people.rpf", n, "MODEL", 120_000, 0);
    }
    for i in 0..12u32 {
        push("sample_props.ttpf", &format!("tex_{i:05}_512x512_BC7_UNORM_SRGB.ttpf"), "TTPF", 262_144, i);
    }
    for i in 0..6u32 {
        push("sample_audio.rpf", &format!("sound_{:04}_{}st.awc", i + 1, 2 + i), "AWC", 400_000, 2 + i);
    }
    out
}

#[test]
#[ignore]
fn context_capture() {
    let Some(out) = std::env::var_os("SIXSTARS_CONTEXT_OUT").map(PathBuf::from) else {
        eprintln!("set SIXSTARS_CONTEXT_OUT to write the context menu / related panel stills");
        return;
    };
    fs::create_dir_all(&out).unwrap();
    let home = out.join("home");
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    let logo = image::load_from_memory(include_bytes!("../assets/logo.png")).ok().map(|img| {
        let rgba = img.to_rgba8();
        (rgba.as_raw().to_vec(), rgba.width(), rgba.height())
    });
    let mut h = Harness::builder()
        .with_size(egui::vec2(W, H))
        .with_pixels_per_point(PPP)
        .with_max_steps(8)
        .wgpu()
        .build_eframe(move |cc| Rpf9App::new(cc, logo.clone(), Vec::new()));
    let root = PathBuf::from("/Users/demo/Sample Dump");
    let list = entries(&root);
    {
        let app = h.state_mut();
        let mut st = app.state.lock().unwrap();
        let mut names: Vec<String> = list.iter().map(|e| e.rpf_name.clone()).collect();
        names.sort();
        names.dedup();
        st.header_info = format!("Folder: {}", root.display());
        st.status = format!("{} archives · {} entries (sample data)", names.len(), list.len());
        st.rpf_names = names;
        st.entries = list;
        st.scanning = false;
        st.ver += 1;
        drop(st);
        app.target_path = Some(root.clone());
        app.is_folder = true;
        app.view_key = None;
        app.update.lock().unwrap().available = None;
        app.tab = AppTab::Explorer;
    }
    h.step();

    // view the vehicle: a generated mesh stands in for the decoded file
    let car = h.state().state.lock().unwrap().entries.iter().position(|e| e.name == "breeze_gt.#ft").unwrap();
    {
        let app = h.state_mut();
        app.reset_selection();
        app.selected = Some(car);
        app.model_entry = Some(car);
        app.media.lock().unwrap().loaded_mesh = Some(box_car("breeze_gt"));
        app.ctx_ui.related_open = true;
    }
    for _ in 0..200 {
        h.step();
        if h.state().ctx_ui.related_ready(car) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    {
        let app = h.state_mut();
        app.show_3d = true;
        if let Some(m) = app.mesh3d.as_mut() {
            m.yaw = 2.45;
            m.pitch = 0.38;
            m.dirty = true;
        }
    }
    for _ in 0..4 {
        h.step();
    }
    h.render().unwrap().save(out.join("related_panel.png")).unwrap();

    let row_pos = |h: &Harness<'_, Rpf9App>, label: &str| -> egui::Pos2 {
        let r = h.query_all(by().label_contains(label)).filter_map(|n| n.bounding_box()).map(to_points).find(|r| r.left() < 700.0).expect("row");
        egui::pos2(r.left() + 60.0, r.center().y)
    };
    let right_click = |h: &mut Harness<'_, Rpf9App>, p: egui::Pos2| {
        h.input_mut().events.push(egui::Event::PointerMoved(p));
        h.step();
        for pressed in [true, false] {
            h.input_mut().events.push(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Secondary, pressed, modifiers: Default::default() });
            h.step();
        }
        for _ in 0..3 {
            h.step();
        }
    };
    let escape = |h: &mut Harness<'_, Rpf9App>| {
        h.input_mut().events.push(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
        h.step();
        h.input_mut().events.push(egui::Event::PointerButton { pos: egui::pos2(20.0, H - 10.0), button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        h.input_mut().events.push(egui::Event::PointerButton { pos: egui::pos2(20.0, H - 10.0), button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        for _ in 0..3 {
            h.step();
        }
    };

    // single-entry menu on an Explorer row
    let p = row_pos(&h, "breeze_gt_spoiler_02");
    right_click(&mut h, p);
    h.render().unwrap().save(out.join("context_menu.png")).unwrap();
    escape(&mut h);

    // multi-selection menu
    {
        let app = h.state_mut();
        let st = app.state.lock().unwrap();
        let picks: Vec<usize> = st.entries.iter().enumerate().filter(|(_, e)| e.name.starts_with("breeze_gt_") && e.kind == "MODEL").map(|(i, _)| i).collect();
        drop(st);
        app.multi_selected = picks.into_iter().collect();
    }
    h.step();
    let p = row_pos(&h, "breeze_gt_bumper_f_01");
    right_click(&mut h, p);
    h.render().unwrap().save(out.join("context_menu_multi.png")).unwrap();
    escape(&mut h);
    h.state_mut().multi_selected.clear();

    // right click in the 3D preview
    right_click(&mut h, egui::pos2(W * 0.45, H * 0.62));
    h.render().unwrap().save(out.join("preview_menu.png")).unwrap();
    escape(&mut h);

    // the AI context the menu copies, as Markdown and JSON
    for (json, file) in [(false, "ai_context.md"), (true, "ai_context.json")] {
        h.state_mut().ctx_ui.last_copied.clear();
        h.state_mut().ctx_ui.queue_ai_context(vec![car], json, !json);
        for _ in 0..300 {
            h.step();
            if !h.state().ctx_ui.last_copied.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        fs::write(out.join(file), &h.state().ctx_ui.last_copied).unwrap();
    }
    h.step();
    h.render().unwrap().save(out.join("toast.png")).unwrap();
    eprintln!("wrote stills to {}", out.display());
}
