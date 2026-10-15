//! Picture regression check for every built-in panel: each panel is rendered maximized (egui_kittest
//! with wgpu) and reduced to a coarse fingerprint, a 32×20 grid of average grey levels, compared with
//! the pinned values in `ui_fingerprints.txt` with a tolerance. Small icon, font or antialiasing
//! changes pass; a blank, clipped, collapsed or rearranged panel fails. The menu bar and toolbar rows are not compared.
//!
//! Pinned for aarch64 macOS; elsewhere it only checks that no panel renders as a flat image. Skips
//! without a GPU adapter. On a mismatch the rendered panel is written to
//! `target/tmp/ui-fingerprints/<panel>.png`. Re-pin after an intended UI change with
//! `UI_FINGERPRINT_PRINT=1 cargo test -p effectcraft-ui-egui --test ui ui_fingerprints::`.
//! Original fixture; renders are review outputs, not shipped assets.
use effectcraft_engine::Session;
use effectcraft_ui_egui::{EffectcraftApp, dock::PanelKind};
use egui_kittest::Harness;
use serde_json::json;

const W: u32 = 32;
const H: u32 = 20;
const SIZE: (f32, f32) = (1280.0, 800.0);
/// Rows skipped at the top: the menu bar and toolbar are shared by every panel, so an icon tweak
/// there would fail all of them at once.
const SKIP_ROWS: u32 = 2;
/// Mean absolute difference per compared cell, in grey levels (0–255).
const MAX_MEAN: f64 = 1.0;
/// A cell counts as changed when it differs by more than this many grey levels…
const CELL_DIFF: u8 = 8;
/// …and at most this many cells may change. Unchanged code renders within 1 grey level; an
/// empty Wiggler panel changes 24 cells by up to 21.
const MAX_CHANGED: usize = 4;
const PINNED: &str = include_str!("ui_fingerprints.txt");

/// Average-pools the image to `W`×`H` grey cells (Rec. 601 luma).
fn fingerprint(img: &image::RgbaImage) -> Vec<u8> {
    let (iw, ih) = img.dimensions();
    let mut out = Vec::with_capacity((W * H) as usize);
    for cy in 0..H {
        for cx in 0..W {
            let (x0, x1) = (cx * iw / W, (cx + 1) * iw / W);
            let (y0, y1) = (cy * ih / H, (cy + 1) * ih / H);
            let (mut sum, mut n) = (0.0f64, 0.0f64);
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = img.get_pixel(x, y).0;
                    sum += 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]);
                    n += 1.0;
                }
            }
            out.push((sum / n.max(1.0)).round() as u8);
        }
    }
    out
}

fn hex(v: &[u8]) -> String {
    v.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

/// A fixed virtual directory: a user's changing home folder is not a UI regression fixture.
struct FixtureBrowser(effectcraft_engine::media_browser::VirtualTree);
impl effectcraft_engine::media_browser::Browser for FixtureBrowser {
    fn home(&self) -> String {
        "/Fixtures".into()
    }
    fn list(&self, dir: &str, importable_only: bool) -> Result<Vec<effectcraft_engine::media_browser::Entry>, String> {
        self.0.list(dir, importable_only)
    }
}

/// Renders every panel maximized; `None` without a GPU adapter.
fn render_panels() -> Option<Vec<(String, image::RgbaImage)>> {
    let _gpu = crate::gpu_lock();
    let Some(probe) = effectcraft_gpu::Gpu::headless() else {
        eprintln!("no GPU adapter: skipping the UI fingerprints");
        return None;
    };
    drop(probe);
    let mut s = Session::default();
    let mut browser = effectcraft_engine::media_browser::VirtualTree::default();
    for folder in ["Footage", "Images", "Projects"] {
        browser.add_dir(&format!("/Fixtures/{folder}"));
    }
    browser.add_file("/Fixtures/Example.mov", "/Fixtures/Example.mov", 1024, None);
    s.browser = Some(std::sync::Arc::new(FixtureBrowser(browser)));
    s.execute("comp.new", json!({"name":"UI Review","width":640,"height":360,"duration":4})).unwrap();
    s.execute("layer.newText", json!({"text":"EffectCraft","name":"Title"})).unwrap();
    s.execute("layer.newSolid", json!({"name":"Plate","color":"#406080","width":160,"height":120})).unwrap();
    s.execute("renderQueue.add", json!({"format":"png","output":"review-[#####].png"})).unwrap();
    std::sync::Arc::make_mut(&mut s.project).settings.gpu_acceleration = false;
    let mut h = Harness::builder().with_size(egui::vec2(SIZE.0, SIZE.1)).with_pixels_per_point(1.0).build_eframe(|_| EffectcraftApp::new(s));
    let mut out = Vec::new();
    for p in PanelKind::ALL {
        h.state_mut().show_panel(p);
        h.state_mut().ui.maximized = Some(p);
        h.run_steps(4);
        // Let the viewer's background frame land before the picture.
        for _ in 0..200 {
            if h.state().frames.inflight() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            h.step();
        }
        h.run_steps(2);
        assert!(h.state().auto.find(&format!("panel.{}", p.id())).is_some(), "{} opens", p.title());
        out.push((p.id().to_string(), h.render().unwrap()));
    }
    Some(out)
}

#[test]
fn panel_fingerprints_match() {
    let Some(panels) = render_panels() else { return };
    let size = format!("{}x{}", SIZE.0, SIZE.1);
    let got: Vec<(String, Vec<u8>)> = panels.iter().map(|(n, img)| (n.clone(), fingerprint(img))).collect();
    let dump = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("ui-fingerprints");
    let save = |name: &str| {
        std::fs::create_dir_all(&dump).unwrap();
        let path = dump.join(format!("{name}.png"));
        panels.iter().find(|(n, _)| n == name).unwrap().1.save(&path).unwrap();
        path
    };
    if std::env::var_os("UI_FINGERPRINT_PRINT").is_some() {
        let text: String = got.iter().map(|(n, f)| format!("{n} {size} {}\n", hex(f))).collect();
        std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/ui_fingerprints.txt"), &text).unwrap();
        println!("{text}");
        for (name, _) in &got {
            save(name);
        }
        eprintln!("re-pinned; renders in {}", dump.display());
        return;
    }
    let mut bad = Vec::new();
    for (name, img) in &panels {
        // Everywhere: the panel area (below the menu, toolbar and tab rows) is not one flat colour.
        // Full-resolution pixels, so a nearly empty panel with one line of text still passes.
        let luma = |p: &image::Rgba<u8>| (u16::from(p.0[0]) * 3 + u16::from(p.0[1]) * 6 + u16::from(p.0[2])) / 10;
        let body = img.enumerate_pixels().filter(|(_, y, _)| *y >= img.height() / 8).map(|(_, _, p)| luma(p));
        let (lo, hi) = body.fold((u16::MAX, 0), |(lo, hi), l| (lo.min(l), hi.max(l)));
        let spread = hi.saturating_sub(lo);
        if spread < 16 {
            bad.push(format!("{name}: renders flat (grey spread {spread}), see {}", save(name).display()));
        }
    }
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        for (name, f) in &got {
            let Some(line) = PINNED.lines().find(|l| l.split(' ').next() == Some(name.as_str())) else {
                bad.push(format!("{name}: not pinned"));
                continue;
            };
            let mut parts = line.split(' ').skip(1);
            let (Some(psize), Some(phex)) = (parts.next(), parts.next()) else {
                bad.push(format!("{name}: malformed pinned line"));
                continue;
            };
            let want = unhex(phex);
            if psize != size || want.len() != f.len() {
                bad.push(format!("{name}: pinned at {psize}, rendered at {size}"));
                continue;
            }
            let skip = (SKIP_ROWS * W) as usize;
            let diffs: Vec<u8> = f.iter().zip(&want).skip(skip).map(|(a, b)| a.abs_diff(*b)).collect();
            let mean = diffs.iter().map(|&d| f64::from(d)).sum::<f64>() / diffs.len() as f64;
            let changed = diffs.iter().filter(|&&d| d > CELL_DIFF).count();
            if mean > MAX_MEAN || changed > MAX_CHANGED {
                bad.push(format!(
                    "{name}: mean grey difference {mean:.2} (max {MAX_MEAN}), {changed} cells changed by >{CELL_DIFF} (max {MAX_CHANGED}); rendered: {}",
                    save(name).display()
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "UI panel fingerprints changed:\n{}\nIf the change is intended, re-pin with \
         `UI_FINGERPRINT_PRINT=1 cargo test -p effectcraft-ui-egui --test ui ui_fingerprints::`.",
        bad.join("\n")
    );
}
