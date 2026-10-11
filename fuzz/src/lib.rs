//! The bodies of the fuzz targets (`fuzz_targets/*.rs` call one each), kept here so the seed
//! generator and a quick replay can share them. Each takes untrusted bytes the way the app does
//! and must never panic, hang or run out of memory: a crash found here is a bug (AGENTS.md "Never
//! crash"), fixed with a regression test in the crate that crashed.
//!
//! Nothing here touches the disk, the network or other processes: file services deny every
//! access, and the command target only runs commands that edit the project in memory.

use std::sync::{Arc, OnceLock};

use effectcraft_engine::Session;
use effectcraft_engine::color::Label;
use effectcraft_project::{Comp, ItemId, ItemKind, LayerId, LayerSource, Project, Solid};
use effectcraft_time::{FrameRate, Tick};
use serde_json::Value;

/// Largest input a target looks at: bigger files only cost time, not coverage.
const MAX_INPUT: usize = 1 << 20;

/// File services that refuse everything (no reads, writes, listings or converters).
struct NoFiles;

impl effectcraft_engine::Services for NoFiles {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("fuzzing: no file access ({path})")))
    }
    fn write_file(&self, path: &str, _: &[u8]) -> std::io::Result<()> {
        Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("fuzzing: no file access ({path})")))
    }
    fn exists(&self, _: &str) -> bool {
        false
    }
    fn store_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        self.write_file(path, data)
    }
    fn list_dir(&self, _: &str) -> Vec<String> {
        vec![]
    }
    fn is_dir(&self, _: &str) -> bool {
        false
    }
}

fn sandboxed_session() -> Session {
    let mut s = Session::new();
    s.services = Arc::new(NoFiles);
    s.config = None;
    s
}

fn text(data: &[u8]) -> Option<&str> {
    std::str::from_utf8(data.get(..data.len().min(MAX_INPUT))?).ok()
}

/// `.ecproj` (JSON or the XML form) → project → the upgrade `file.open` runs → loaded into a
/// session the way `file.open` does it.
pub fn project_open(data: &[u8]) {
    let Some(t) = text(data) else { return };
    let json;
    let t = if effectcraft_engine::xml_project::is_xml(t) {
        match effectcraft_engine::xml_project::from_xml(t) {
            Ok(j) => {
                json = j;
                json.as_str()
            }
            Err(_) => return,
        }
    } else {
        t
    };
    let Ok(p) = Project::from_json(t) else { return };
    let mut s = sandboxed_session();
    s.replace_project(p, None);
    // Render the first comp's first frame small: the loaded values must be usable, not only parsed.
    if let Some(cid) = s.project.comps().map(|(id, _)| *id).next() {
        let _ = s.render_rgba8_alpha(cid, Tick::ZERO, 64, false);
    }
}

/// Lottie JSON or `.lottie` → project.
pub fn lottie_import(data: &[u8]) {
    let mut p = Project::default();
    let _ = effectcraft_lottie::import(&mut p, data.get(..data.len().min(MAX_INPUT)).unwrap_or_default(), "fuzz", "", &mut |_| None);
}

/// SVG → document → a small raster.
pub fn svg_parse(data: &[u8]) {
    if let Ok(doc) = effectcraft_svg::parse(data.get(..data.len().min(MAX_INPUT)).unwrap_or_default()) {
        let _ = effectcraft_svg::rasterize(&doc, 32, 32, 0.25);
    }
}

/// PSD → layer tree, composite and every layer's pixels.
pub fn psd_parse(data: &[u8]) {
    let Ok(psd) = effectcraft_psd::Psd::parse(data.get(..data.len().min(MAX_INPUT)).unwrap_or_default().to_vec()) else { return };
    let tree = psd.tree();
    let _ = psd.composite();
    for i in 0..tree.len().min(8) {
        let _ = psd.layer_pixels(i, false);
    }
}

/// PDF / AI / EPS → vector document → a small raster.
pub fn pdf_parse(data: &[u8]) {
    if let Ok(doc) = effectcraft_pdf::parse(data.get(..data.len().min(MAX_INPUT)).unwrap_or_default()) {
        let _ = effectcraft_svg::rasterize(&doc, 32, 32, 0.1);
    }
}

/// Timelines: the first byte picks Final Cut Pro 7 XML, FCPXML, OpenTimelineIO, EDL or detection.
pub fn interchange_import(data: &[u8]) {
    use effectcraft_interchange::{ImportOptions, TimelineFormat};
    let Some((&pick, rest)) = data.split_first() else { return };
    let (format, name) = match pick % 5 {
        0 => (Some(TimelineFormat::Fcp7Xml), "fuzz.xml"),
        1 => (Some(TimelineFormat::Fcpxml), "fuzz.fcpxml"),
        2 => (Some(TimelineFormat::Otio), "fuzz.otio"),
        3 => (Some(TimelineFormat::Edl), "fuzz.edl"),
        _ => (None, "fuzz"),
    };
    let opts = ImportOptions { name: name.into(), edl_frame_rate: (pick & 0x80 != 0).then_some(23.976), ..Default::default() };
    let mut p = Project::default();
    let _ = effectcraft_interchange::import(&mut p, rest.get(..rest.len().min(MAX_INPUT)).unwrap_or_default(), format, &opts, &mut |_| None);
}

/// One comp with one solid layer, made once: what an expression is evaluated on.
fn expr_fixture() -> &'static (Project, ItemId, LayerId) {
    static FX: OnceLock<(Project, ItemId, LayerId)> = OnceLock::new();
    FX.get_or_init(|| {
        let mut p = Project::default();
        let comp = Comp::new(200, 100, FrameRate::FPS_30, Tick::from_seconds_f64(10.0));
        let cid = p.add_item("Main", Label::None, None, ItemKind::Comp(comp.clone().into()));
        let sid = p.add_item("S", Label::Red, None, ItemKind::Solid(Solid { color: [1.0, 0.0, 0.0], width: 20, height: 20, pixel_aspect: 1.0 }));
        let l = effectcraft_project::build::layer(&mut p, &comp, "A", LayerSource::Solid { item: sid }, (20, 20), None);
        let lid = l.id;
        if let Some(c) = p.comp_mut(cid) {
            c.layers.push(l);
        }
        (p, cid, lid)
    })
}

/// Expressions: the first byte picks the property, the rest is the expression text. Loops are
/// bounded by the runtime's iteration limit, not by time: run with libFuzzer's `-timeout`.
pub fn expr_eval(data: &[u8]) {
    let Some((&pick, rest)) = data.split_first() else { return };
    let Ok(src) = std::str::from_utf8(rest.get(..rest.len().min(4096)).unwrap_or_default()) else { return };
    let path = ["transform/rotation", "transform/position", "transform/opacity", "transform/scale", "transform/anchorPoint"][usize::from(pick) % 5];
    let (p, cid, lid) = expr_fixture();
    let _ = effectcraft_expr::check_syntax(src);
    let _ = effectcraft_expr::eval_standalone(p, *cid, *lid, path, src, f64::from(pick) / 10.0);
}

/// Footage: the first byte picks the file extension (which steers probing), the rest is the file;
/// probe it, decode its first frame and a little audio.
pub fn media_probe(data: &[u8]) {
    let Some((&pick, rest)) = data.split_first() else { return };
    const EXT: &[&str] = &["png", "jpg", "exr", "gif", "webp", "tif", "bmp", "avif", "mp4", "mov", "wav", "mp3", "psd", "svg", "pdf", "glb"];
    let path = format!("/fuzz/footage.{}", EXT[usize::from(pick) % EXT.len()]);
    let bytes: Arc<[u8]> = Arc::from(rest.get(..rest.len().min(MAX_INPUT)).unwrap_or_default());
    let Ok(f) = effectcraft_media::probe_bytes(&path, bytes.clone()) else { return };
    let pool = effectcraft_media::MediaPool::new();
    pool.add_bytes(&path, bytes);
    let _ = pool.frame_at(&f, Tick::ZERO);
    let _ = pool.audio_samples(&f, Tick::ZERO, 256, 48_000);
}

/// Command families that only edit the project in memory (no files, processes or downloads).
const SAFE_PREFIXES: &[&str] = &[
    "layer.",
    "keys.",
    "comp.",
    "camera.",
    "time.",
    "prop.",
    "mask.",
    "project.",
    "effect.",
    "text.",
    "path.",
    "paint.",
    "anim.",
    "warp.",
    "puppet.",
    "shape.",
    "material.",
    "markers.",
    "expr.",
    "liquify.",
    "item.",
    "essential.",
    "edit.",
    "view.",
];

fn safe_commands() -> &'static [&'static str] {
    static IDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    IDS.get_or_init(|| {
        let mut ids: Vec<&'static str> =
            effectcraft_engine::command_specs().iter().map(|c| c.id).filter(|id| SAFE_PREFIXES.iter().any(|p| id.starts_with(p))).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    })
}

/// Engine commands as agents and the control channel send them: on the demo project, each line
/// is one command (first byte: which, rest: JSON parameters), checked like the control channel's.
pub fn engine_command(data: &[u8]) {
    let ids = safe_commands();
    if ids.is_empty() {
        return;
    }
    let mut s = sandboxed_session();
    s.replace_project(effectcraft_engine::demo::demo_project(), None);
    for line in data.get(..data.len().min(64 << 10)).unwrap_or_default().split(|&b| b == b'\n').take(16) {
        let Some((&pick, json)) = line.split_first() else { continue };
        let id = ids[usize::from(pick) % ids.len()];
        let params = serde_json::from_slice::<Value>(json).unwrap_or(Value::Object(Default::default()));
        let _ = s.execute_checked(id, params);
    }
}
