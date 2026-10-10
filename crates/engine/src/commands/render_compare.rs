//! Render compare for agents: keep named rendered frames (`render.snapshot`) and compare any two
//! frames, snapshots or image files (`render.compare`) with error metrics and a diff heatmap.
//! Queries (not journaled, not undoable): they read the project and never change it.
//!
//! The metrics and resampling are pure math in [`effectcraft_raster::compare`]; this module
//! renders, decodes, encodes and keeps the session's snapshots.
//!
//! Size rule: with `max_side`, every image is first scaled down (area average) so its longest
//! side fits. Two images of different sizes but the same aspect ratio (a half-resolution
//! reference, say) are compared at the smaller size, the larger one scaled down to it; different
//! aspect ratios are an error naming both sizes.
//!
//! `region: [x, y, w, h]` is in pixels of the compared frames (comp pixels unless `max_side` or
//! the size rule scaled them): the metrics, `diffBounds` and the heatmap cover only that rectangle,
//! clipped to the frame. `diffBounds` is the box of the pixels over the threshold, in the same
//! frame coordinates (null when nothing changed).

use std::collections::VecDeque;

use effectcraft_raster::compare::{self as cmp, CompareOpts, Rgba8};
use effectcraft_time::Tick;
use serde_json::{Value, json};

use super::{CommandSpec, bad, str_p};
use crate::{EngineError, Result, Session};

/// At most this many snapshots are kept; the oldest goes first.
pub const MAX_SNAPSHOTS: usize = 16;
/// And at most this many bytes of pixels (eight 4K frames).
pub const MAX_SNAPSHOT_BYTES: usize = 8 * 3840 * 2160 * 4;
/// Largest image side decoded from a file.
const MAX_DECODE_SIDE: u32 = 16384;

/// One kept frame (straight RGBA8).
#[derive(Clone, Debug)]
pub struct FrameSnapshot {
    pub name: String,
    pub comp: u64,
    pub time: f64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The session's named frames (`render.snapshot`), oldest first; bounded by count and bytes.
#[derive(Clone, Debug, Default)]
pub struct FrameSnapshots {
    items: VecDeque<FrameSnapshot>,
}

impl FrameSnapshots {
    /// Keep `snap` (replacing one of the same name); returns the names evicted to make room.
    pub fn insert(&mut self, snap: FrameSnapshot) -> Vec<String> {
        self.items.retain(|s| s.name != snap.name);
        self.items.push_back(snap);
        let mut evicted = vec![];
        while self.items.len() > MAX_SNAPSHOTS || (self.items.len() > 1 && self.bytes() > MAX_SNAPSHOT_BYTES) {
            match self.items.pop_front() {
                Some(s) => evicted.push(s.name),
                None => break,
            }
        }
        evicted
    }
    pub fn get(&self, name: &str) -> Option<&FrameSnapshot> {
        self.items.iter().find(|s| s.name == name)
    }
    pub fn names(&self) -> Vec<String> {
        self.items.iter().map(|s| s.name.clone()).collect()
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn clear(&mut self) {
        self.items.clear();
    }
    fn bytes(&self) -> usize {
        self.items.iter().map(|s| s.rgba.len()).sum()
    }
}

/// A decoded or rendered image plus where it came from.
struct Frame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    source: Value,
}

impl Frame {
    fn view(&self) -> std::result::Result<Rgba8<'_>, cmp::CompareError> {
        Rgba8::new(self.width, self.height, &self.rgba)
    }
    /// Scale to `w`×`h` (no-op when already that size).
    fn resized(self, w: u32, h: u32, cmd: &str) -> Result<Frame> {
        if (self.width, self.height) == (w, h) {
            return Ok(self);
        }
        let rgba = cmp::resample(&self.view().map_err(|e| bad(cmd, e.to_string()))?, w, h).map_err(|e| bad(cmd, e.to_string()))?;
        Ok(Frame { width: w, height: h, rgba, source: self.source })
    }
    /// Just the `[x, y, w, h]` rectangle (inside the frame).
    fn cropped(self, r: [u32; 4], cmd: &str) -> Result<Frame> {
        let rgba = cmp::crop(&self.view().map_err(|e| err(cmd, e))?, r).map_err(|e| err(cmd, e))?;
        Ok(Frame { width: r[2], height: r[3], rgba, source: self.source })
    }
}

fn err(cmd: &str, e: impl std::fmt::Display) -> EngineError {
    bad(cmd, e.to_string())
}

/// Render `comp` (`p.comp`, default active) at `p.time` seconds (default that comp's current time).
fn render(s: &Session, p: &Value, max_side: u32, cmd: &str) -> Result<Frame> {
    let cid = super::comp_id(s, p)?;
    let t = match p.get("time") {
        None | Some(Value::Null) => s.state.times.get(&cid).copied().unwrap_or(Tick::ZERO),
        Some(v) => Tick::from_seconds_f64(v.as_f64().filter(|t| t.is_finite()).ok_or_else(|| bad(cmd, "`time` must be a number of seconds"))?),
    };
    let transparent = p.get("transparent").and_then(Value::as_bool).unwrap_or(false);
    let (width, height, rgba) = s.render_rgba8_alpha(cid, t, max_side, transparent)?;
    Ok(Frame { width, height, rgba, source: json!({"comp": cid.0, "time": t.seconds()}) })
}

fn max_side_p(p: &Value, cmd: &str) -> Result<u32> {
    match p.get("max_side") {
        None | Some(Value::Null) => Ok(0),
        Some(v) => v.as_u64().and_then(|m| u32::try_from(m).ok()).ok_or_else(|| bad(cmd, "`max_side` must be a whole number of pixels (0 = full size)")),
    }
}

/// Decode a PNG or JPEG (sizes capped) to straight RGBA8.
pub(crate) fn decode_image(bytes: &[u8]) -> std::result::Result<(u32, u32, Vec<u8>), String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_SIDE);
    limits.max_image_height = Some(MAX_DECODE_SIDE);
    limits.max_alloc = Some(2 << 30);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| e.to_string())?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err("empty image".into());
    }
    Ok((w, h, img.into_raw()))
}

/// One side of a comparison: `"name"` / `{snapshot}` (a kept frame), `{path}` (a PNG/JPEG file)
/// or `{comp?, time?, transparent?}` (render now).
fn source(s: &Session, v: Option<&Value>, which: &str, max_side: u32, cmd: &str) -> Result<Frame> {
    let v = v.filter(|v| !v.is_null()).ok_or_else(|| bad(cmd, format!("missing `{which}`: {{snapshot: name}}, {{path: file.png}} or {{comp?, time?}}")))?;
    let snapshot = |name: &str| -> Result<Frame> {
        let f = s.frame_snapshots.get(name).ok_or_else(|| {
            let have = s.frame_snapshots.names();
            let have = if have.is_empty() { "none; take one with render.snapshot".to_string() } else { have.join(", ") };
            bad(cmd, format!("{which}: no snapshot `{name}` (have: {have})"))
        })?;
        Ok(Frame { width: f.width, height: f.height, rgba: f.rgba.clone(), source: json!({"snapshot": name, "comp": f.comp, "time": f.time}) })
    };
    let o = match v {
        Value::String(name) => return snapshot(name),
        Value::Object(o) => o,
        _ => return Err(bad(cmd, format!("`{which}` must be a snapshot name or an object"))),
    };
    let keys: Vec<&str> = o.keys().map(String::as_str).collect();
    let only = |allowed: &[&str]| -> Result<()> {
        match keys.iter().find(|k| !allowed.contains(k)) {
            Some(k) => Err(bad(cmd, format!("{which}: unexpected `{k}` (give one of {{snapshot}}, {{path}}, {{comp?, time?, transparent?}})"))),
            None => Ok(()),
        }
    };
    if let Some(n) = o.get("snapshot") {
        only(&["snapshot"])?;
        return snapshot(n.as_str().ok_or_else(|| bad(cmd, format!("{which}.snapshot must be a name")))?);
    }
    if let Some(path) = o.get("path") {
        only(&["path"])?;
        let path = path.as_str().filter(|p| !p.is_empty()).ok_or_else(|| bad(cmd, format!("{which}.path must be a file path")))?;
        let bytes = s.services.read_file(path).map_err(|e| bad(cmd, format!("{which}: cannot read {path}: {e}")))?;
        let (width, height, rgba) = decode_image(&bytes).map_err(|e| bad(cmd, format!("{which}: {path} is not a readable PNG/JPEG: {e}")))?;
        return Ok(Frame { width, height, rgba, source: json!({"path": path}) });
    }
    only(&["comp", "time", "transparent"])?;
    render(s, v, max_side, cmd)
}

/// `render.snapshot {name, comp?, time?, max_side?, transparent?}`
fn snapshot(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "render.snapshot";
    let name = str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(CMD, "missing `name`"))?.to_string();
    let f = render(s, p, max_side_p(p, CMD)?, CMD)?;
    let (comp, time) = (f.source["comp"].as_u64().unwrap_or(0), f.source["time"].as_f64().unwrap_or(0.0));
    let (width, height) = (f.width, f.height);
    let evicted = s.frame_snapshots.insert(FrameSnapshot { name: name.clone(), comp, time, width, height, rgba: f.rgba });
    let mut out = json!({"name": name, "comp": comp, "time": time, "width": width, "height": height, "snapshots": s.frame_snapshots.names()});
    if !evicted.is_empty() {
        out["evicted"] = json!(evicted);
    }
    Ok(out)
}

/// `render.snapshots {clear?}`: the kept frames.
fn snapshots(s: &mut Session, p: &Value) -> Result<Value> {
    if p.get("clear").and_then(Value::as_bool) == Some(true) {
        s.frame_snapshots.clear();
    }
    let list: Vec<Value> =
        s.frame_snapshots.items.iter().map(|f| json!({"name": f.name, "comp": f.comp, "time": f.time, "width": f.width, "height": f.height})).collect();
    Ok(json!({"snapshots": list, "max": MAX_SNAPSHOTS}))
}

fn round(v: f64, digits: i32) -> Value {
    if !v.is_finite() {
        return json!("inf");
    }
    let k = 10f64.powi(digits);
    json!((v * k).round() / k)
}

/// `render.compare {a, b, threshold?, min_psnr?, max_side?, region?, amplify?, diff_path?, diff_inline?}`
fn compare(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "render.compare";
    let max_side = max_side_p(p, CMD)?;
    let num = |k: &str, lo: f64, hi: f64| -> Result<Option<f64>> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_f64()
                .filter(|x| x.is_finite() && (lo..=hi).contains(x))
                .map(Some)
                .ok_or_else(|| bad(CMD, format!("`{k}` must be a number from {lo} to {hi}"))),
        }
    };
    let threshold = num("threshold", 0.0, 255.0)?.unwrap_or(CompareOpts::default().threshold);
    let min_psnr = num("min_psnr", 0.0, 1000.0)?;
    let amplify = num("amplify", 1.0, 255.0)?.unwrap_or(CompareOpts::default().amplify);
    let opts = CompareOpts { threshold, amplify };
    let a = source(s, p.get("a"), "a", max_side, CMD)?;
    let b = source(s, p.get("b"), "b", max_side, CMD)?;
    let sizes = json!({"a": [a.width, a.height], "b": [b.width, b.height]});
    // Fit `max_side`, then meet at the smaller size when only the scale differs.
    let (aw, ah) = cmp::fit_max_side(a.width, a.height, max_side);
    let (bw, bh) = cmp::fit_max_side(b.width, b.height, max_side);
    let (a, b) = (a.resized(aw, ah, CMD)?, b.resized(bw, bh, CMD)?);
    let mut resampled = Value::Null;
    let (a, b) = if (aw, ah) == (bw, bh) {
        (a, b)
    } else if cmp::same_aspect((aw, ah), (bw, bh)) {
        if (aw as u64) * (ah as u64) <= (bw as u64) * (bh as u64) {
            resampled = json!("b");
            (a, b.resized(aw, ah, CMD)?)
        } else {
            resampled = json!("a");
            (a.resized(bw, bh, CMD)?, b)
        }
    } else {
        return Err(bad(
            CMD,
            format!("size mismatch: a is {aw}x{ah}, b is {bw}x{bh} (different aspect ratios; render or export both at the same size, or crop one)"),
        ));
    };
    let (width, height) = (a.width, a.height);
    let region = match p.get("region") {
        None | Some(Value::Null) => None,
        // Whole pixels clipped to the frame, so the casts are exact.
        Some(v) => Some(crate::render_view::region_p(v, width, height, CMD)?.map(|n| n as u32)),
    };
    let (a, b) = match region {
        Some(r) => (a.cropped(r, CMD)?, b.cropped(r, CMD)?),
        None => (a, b),
    };
    let (ox, oy) = region.map_or((0, 0), |r| (r[0], r[1]));
    let (va, vb) = (a.view().map_err(|e| err(CMD, e))?, b.view().map_err(|e| err(CMD, e))?);
    let st = cmp::compare(&va, &vb, &opts).map_err(|e| err(CMD, e))?;
    let pass = st.pass(min_psnr);
    let diff_bounds = st.diff_bounds.map(|[x, y, w, h]| [x.saturating_add(ox), y.saturating_add(oy), w, h]);
    let mut out = json!({
        "width": width,
        "height": height,
        "max_abs": round(st.max_abs, 3),
        "mean_abs": round(st.mean_abs, 4),
        "alpha_max_abs": round(st.alpha_max_abs, 3),
        "psnr_db": round(st.psnr_db, 3),
        "ssim": round(st.ssim, 5),
        "changed_pixels": st.changed_pixels,
        "changed_pct": round(st.changed_pct, 4),
        "threshold": threshold,
        "pass": pass,
        "diffBounds": diff_bounds,
        "a": a.source,
        "b": b.source,
    });
    if let Some(db) = min_psnr {
        out["min_psnr"] = json!(db);
    }
    if let Some(r) = region {
        out["region"] = json!(r);
    }
    if !resampled.is_null() {
        out["resampled"] = resampled;
        out["sizes"] = sizes;
    }
    let diff_path = str_p(p, "diff_path").filter(|d| !d.is_empty());
    let inline = p.get("diff_inline").and_then(Value::as_bool).unwrap_or(false);
    if diff_path.is_some() || inline {
        let heat = cmp::diff_heatmap(&va, &vb, &opts).map_err(|e| err(CMD, e))?;
        let png = super::comp_more::encode_png(&heat, st.width, st.height).map_err(|e| bad(CMD, format!("diff png: {e}")))?;
        if let Some(path) = diff_path {
            s.services.write_file(path, &png).map_err(|e| bad(CMD, format!("cannot write {path}: {e}")))?;
            out["diff_path"] = json!(path);
        }
        if inline {
            out["diff_png"] = json!(effectcraft_track::roto::rle::base64_encode(&png));
        }
    }
    Ok(out)
}

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        crate::query!(
            "render.snapshot",
            "Render Snapshot",
            "{name, comp?, time? (comp s, default the CTI), max_side?, transparent?} → keeps the rendered frame under `name` for render.compare (16 kept, oldest dropped)",
            snapshot
        ),
        crate::query!("render.snapshots", "Render Snapshots", "{clear?} → the kept snapshots {name, comp, time, width, height}", snapshots),
        crate::query!(
            "render.compare",
            "Render Compare",
            "{a: name|{snapshot}|{path}|{comp?, time?, transparent?}, b: (same), threshold? (levels, default 1), min_psnr? (dB), max_side?, region? ([x, y, w, h] frame px: compare only that), amplify? (heatmap gain, default 8), diff_path?, diff_inline?} → {width, height, max_abs, mean_abs, alpha_max_abs, psnr_db, ssim, changed_pixels, changed_pct, pass, diffBounds ([x, y, w, h] of changed px | null), region?, diff_path?, diff_png? (base64)}",
            compare
        ),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::Session;

    fn session() -> Session {
        let mut s = Session::default();
        s.execute("comp.new", json!({"name": "C", "width": 64, "height": 36, "frameRate": 30, "duration": 2})).unwrap();
        s.execute("layer.newSolid", json!({"name": "Box", "color": "#ff0000", "width": 16, "height": 16})).unwrap();
        s
    }

    fn tmp(name: &str) -> String {
        std::env::temp_dir().join(format!("ec-compare-{}-{name}", std::process::id())).to_string_lossy().to_string()
    }

    #[test]
    fn identical_frames_pass_with_infinite_psnr() {
        let mut s = session();
        let r = s.execute("render.snapshot", json!({"name": "before"})).unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(64), Some(36)));
        let r = s.execute("render.compare", json!({"a": {"snapshot": "before"}, "b": {}})).unwrap();
        assert_eq!(r["pass"], true, "{r}");
        assert_eq!(r["psnr_db"], "inf");
        assert_eq!(r["changed_pixels"], 0);
        assert_eq!(r["ssim"], 1.0);
        // Queries don't journal or touch the undo history.
        assert!(s.journal.iter().all(|(id, _)| !id.starts_with("render.")));
    }

    #[test]
    fn moved_layer_shows_up_and_undo_restores_it() {
        let mut s = session();
        s.execute("render.snapshot", json!({"name": "before"})).unwrap();
        s.execute("prop.set", json!({"layer": "#1", "path": "transform/position", "value": [33, 18]})).unwrap();
        let diff = tmp("diff.png");
        let r = s.execute("render.compare", json!({"a": "before", "b": {"comp": "C"}, "diff_path": diff, "diff_inline": true})).unwrap();
        assert_eq!(r["pass"], false, "{r}");
        // A 16×16 red square moved 1 px right: one column leaves, one arrives (2 × 16 pixels).
        assert_eq!(r["changed_pixels"], 32, "{r}");
        assert_eq!(r["max_abs"], 255.0);
        let png = std::fs::read(&diff).unwrap();
        let (w, h, _) = super::decode_image(&png).unwrap();
        assert_eq!((w, h), (64, 36));
        assert!(r["diff_png"].as_str().is_some_and(|b| b.len() > 20));
        let _ = std::fs::remove_file(&diff);
        // min_psnr judges closeness instead of exactness.
        let r = s.execute("render.compare", json!({"a": "before", "b": {}, "min_psnr": 10})).unwrap();
        assert_eq!(r["pass"], true, "{r}");
        s.execute("edit.undo", json!({})).unwrap();
        let r = s.execute("render.compare", json!({"a": "before", "b": {}})).unwrap();
        assert_eq!(r["pass"], true, "{r}");
    }

    #[test]
    fn diff_bounds_and_region() {
        let mut s = session();
        s.execute("render.snapshot", json!({"name": "before"})).unwrap();
        let r = s.execute("render.compare", json!({"a": "before", "b": {}})).unwrap();
        assert_eq!(r["diffBounds"], json!(null), "identical frames: {r}");
        // The 16×16 box (x 24..40, y 10..26) moves 10 px right: the box spans x 24..50.
        s.execute("prop.set", json!({"layer": "#1", "path": "transform/position", "value": [42, 18]})).unwrap();
        let r = s.execute("render.compare", json!({"a": "before", "b": {}})).unwrap();
        assert_eq!((r["diffBounds"].clone(), r["changed_pixels"].as_u64()), (json!([24, 10, 26, 16]), Some(320)), "{r}");
        assert!(r.get("region").is_none());
        // A region restricts the metrics, the box and the heatmap; coordinates stay frame pixels.
        let diff = tmp("region-diff.png");
        let r = s.execute("render.compare", json!({"a": "before", "b": {}, "region": [0, 0, 30, 36], "diff_path": diff})).unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64(), r["region"].clone()), (Some(64), Some(36), json!([0, 0, 30, 36])), "{r}");
        assert_eq!((r["diffBounds"].clone(), r["changed_pixels"].as_u64()), (json!([24, 10, 6, 16]), Some(96)), "{r}");
        assert_eq!(super::decode_image(&std::fs::read(&diff).unwrap()).unwrap().0, 30);
        let _ = std::fs::remove_file(&diff);
        // Clipped to the frame; a region away from the change passes with no box.
        let r = s.execute("render.compare", json!({"a": "before", "b": {}, "region": [40.5, -10, 100, 100]})).unwrap();
        assert_eq!(r["region"], json!([40, 0, 24, 36]), "{r}");
        assert_eq!(r["diffBounds"], json!([40, 10, 10, 16]), "{r}");
        let r = s.execute("render.compare", json!({"a": "before", "b": {}, "region": [0, 28, 64, 8]})).unwrap();
        assert_eq!((r["pass"].as_bool(), r["diffBounds"].clone()), (Some(true), json!(null)), "{r}");
        // Bad regions are errors.
        for (region, want) in [
            (json!([100, 0, 10, 10]), "outside"),
            (json!([0, 0, 0, 10]), "positive"),
            (json!([0, 0, 10]), "[x, y, width, height]"),
            (json!([0, 0, "a", 10]), "[x, y, width, height]"),
            (json!("0,0,8,8"), "[x, y, width, height]"),
            (json!([1e308, 1e308, 1e308, 1e308]), "outside"),
        ] {
            let e = s.execute("render.compare", json!({"a": "before", "b": {}, "region": region})).unwrap_err().to_string();
            assert!(e.contains(want), "{region}: {e}");
        }
        // Hostile but valid: a huge region is the whole frame.
        let r = s.execute("render.compare", json!({"a": "before", "b": {}, "region": [-1e300, -1e300, 1e308, 1e308]})).unwrap();
        assert_eq!(r["region"], json!([0, 0, 64, 36]), "{r}");
        // With max_side the region is in the scaled frame's pixels.
        let r = s.execute_checked("render.compare", json!({"a": "before", "b": {}, "max_side": 32, "region": [0, 0, 16, 18]})).unwrap();
        assert_eq!((r["width"].as_u64(), r["region"].clone()), (Some(32), json!([0, 0, 16, 18])), "{r}");
    }

    #[test]
    fn files_and_size_mismatch() {
        let mut s = session();
        s.execute("render.snapshot", json!({"name": "full"})).unwrap();
        let half = tmp("half.png");
        let (w, h, px) = s.render_rgba8(s.active_comp_id().unwrap(), effectcraft_time::Tick::ZERO, 32).unwrap();
        std::fs::write(&half, super::super::comp_more::encode_png(&px, w, h).unwrap()).unwrap();
        // Same aspect, half the size: compared at the smaller size.
        let r = s.execute("render.compare", json!({"a": "full", "b": {"path": half}})).unwrap();
        assert_eq!((r["width"].as_u64(), r["resampled"].as_str()), (Some(32), Some("a")), "{r}");
        assert!(r["psnr_db"].as_f64().is_none_or(|db| db > 20.0), "{r}");
        // max_side brings both down.
        let r = s.execute("render.compare", json!({"a": "full", "b": {}, "max_side": 16})).unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64(), r["pass"].as_bool()), (Some(16), Some(9), Some(true)));
        // A different aspect ratio is an error naming both sizes.
        let square = tmp("square.png");
        std::fs::write(&square, super::super::comp_more::encode_png(&[0u8; 20 * 20 * 4], 20, 20).unwrap()).unwrap();
        let e = s.execute("render.compare", json!({"a": "full", "b": {"path": square}})).unwrap_err().to_string();
        assert!(e.contains("size mismatch: a is 64x36, b is 20x20"), "{e}");
        let _ = (std::fs::remove_file(&half), std::fs::remove_file(&square));
    }

    #[test]
    fn bad_sources_are_errors_not_crashes() {
        let mut s = session();
        let e = s.execute("render.compare", json!({"a": "nope", "b": {}})).unwrap_err().to_string();
        assert!(e.contains("no snapshot `nope`") && e.contains("render.snapshot"), "{e}");
        s.execute("render.snapshot", json!({"name": "x"})).unwrap();
        let e = s.execute("render.compare", json!({"a": {"snapshot": "y"}, "b": "x"})).unwrap_err().to_string();
        assert!(e.contains("(have: x)"), "{e}");
        assert!(s.execute("render.compare", json!({"b": "x"})).unwrap_err().to_string().contains("missing `a`"));
        assert!(s.execute("render.compare", json!({"a": "x", "b": {"path": "x.png", "time": 1}})).is_err());
        assert!(s.execute("render.compare", json!({"a": "x", "b": "x", "threshold": -1})).is_err());
        assert!(s.execute("render.compare", json!({"a": "x", "b": "x", "max_side": -3})).is_err());
        assert!(s.execute_checked("render.compare", json!({"a": "x", "b": "x", "bogus": 1})).is_err());
        assert!(s.execute("render.snapshot", json!({"name": " "})).is_err());
        assert!(s.execute("render.snapshot", json!({"name": "t", "time": "soon"})).is_err());
        // Hostile files: missing, garbage, a truncated PNG, a PNG claiming a huge size.
        assert!(s.execute("render.compare", json!({"a": "x", "b": {"path": "/definitely/not/here.png"}})).unwrap_err().to_string().contains("cannot read"));
        let mut png = super::super::comp_more::encode_png(&[7u8; 8 * 8 * 4], 8, 8).unwrap();
        for (i, bytes) in [b"not an image at all".to_vec(), png[..png.len() / 2].to_vec(), {
            png[16..20].copy_from_slice(&0x7fff_ffffu32.to_be_bytes());
            png.clone()
        }]
        .into_iter()
        .enumerate()
        {
            let f = tmp(&format!("bad{i}.png"));
            std::fs::write(&f, bytes).unwrap();
            let e = s.execute("render.compare", json!({"a": "x", "b": {"path": f}})).unwrap_err().to_string();
            assert!(e.contains("not a readable PNG/JPEG"), "{e}");
            let _ = std::fs::remove_file(&f);
        }
    }

    #[test]
    fn snapshots_are_bounded_and_listed() {
        let mut s = session();
        let mut evicted = vec![];
        for i in 0..super::MAX_SNAPSHOTS + 2 {
            let r = s.execute("render.snapshot", json!({"name": format!("s{i}"), "max_side": 8})).unwrap();
            evicted.extend(r["evicted"].as_array().cloned().unwrap_or_default());
        }
        assert_eq!(evicted, vec![json!("s0"), json!("s1")]);
        assert_eq!(s.frame_snapshots.len(), super::MAX_SNAPSHOTS);
        // Re-taking a name replaces it (and makes it the newest).
        s.execute("render.snapshot", json!({"name": "s2"})).unwrap();
        assert_eq!(s.frame_snapshots.len(), super::MAX_SNAPSHOTS);
        assert_eq!(s.frame_snapshots.names().last().map(String::as_str), Some("s2"));
        let r = s.execute("render.snapshots", json!({})).unwrap();
        assert_eq!(r["snapshots"].as_array().map(Vec::len), Some(super::MAX_SNAPSHOTS));
        let r = s.execute("render.snapshots", json!({"clear": true})).unwrap();
        assert_eq!(r["snapshots"], json!([]));
    }
}
