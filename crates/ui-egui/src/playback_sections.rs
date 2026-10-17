//! Where a comp's sections start, for playback prefetch. A frame where a layer starts (a new
//! precomp, a PDF to decode, a stack of layers to upload) can take seconds, and every frame
//! after it waits on the same work: on an RTX 3090 a cold section entry held a playing preview
//! for 0.5–4.9 s, while playback asks for frames only about half a second ahead. So a playing
//! preview also asks for the frame where the next section starts, up to [`LEAD`] seconds
//! early ([`Sections::ahead`]); rendering it warms what that section's frames share.
//!
//! A section starts where a video layer of the comp starts, or a layer of a comp nested in it
//! (mapped through the precomp layers' start times and stretch, within their in and out
//! points; not below a layer with Time Remap, whose nested times move).

use effectcraft_engine::project::{Comp, ItemId, LayerSource, Project};
use effectcraft_engine::time::{FrameRate, Tick};

/// Seconds ahead a playing preview asks for the frame where a section starts.
pub const LEAD: f64 = 3.0;
/// Section starts asked for ahead of the regular look-ahead at once, at most.
pub const AT_ONCE: usize = 2;
/// Nesting levels looked into.
const MAX_DEPTH: usize = 4;
/// Layers looked at per comp, at most (nested layers included).
const MAX_LAYERS: usize = 20_000;

/// The frames of `comp` where a section starts (sorted, no repeats).
pub fn start_frames(project: &Project, comp: ItemId) -> Vec<i64> {
    let Some(c) = project.comp(comp) else { return Vec::new() };
    let mut starts = Vec::new();
    let mut budget = MAX_LAYERS;
    walk(project, c, Map { offset: 0.0, scale: 1.0 }, (f64::NEG_INFINITY, f64::INFINITY), 0, &mut budget, &mut starts);
    let mut frames: Vec<i64> = starts.into_iter().filter(|t| t.is_finite()).map(|t| first_frame_at_or_after(c.frame_rate, t)).collect();
    frames.sort_unstable();
    frames.dedup();
    frames
}

/// Top comp time (ticks) of a nested comp's time: `offset + scale × t`.
#[derive(Clone, Copy)]
struct Map {
    offset: f64,
    scale: f64,
}

impl Map {
    fn at(self, t: Tick) -> f64 {
        self.offset + self.scale * t.0 as f64
    }
}

/// Collect the layer starts of `c` (mapped by `map`, inside `window`, in top comp ticks).
fn walk(project: &Project, c: &Comp, map: Map, window: (f64, f64), depth: usize, budget: &mut usize, out: &mut Vec<f64>) {
    for l in &c.layers {
        let Some(left) = budget.checked_sub(1) else { return };
        *budget = left;
        if !l.has_video() {
            continue;
        }
        let (a, b) = (map.at(l.in_point).max(window.0), map.at(l.out_point).min(window.1));
        if a >= b {
            continue;
        }
        out.push(a);
        if let LayerSource::Comp { item } = l.source
            && depth < MAX_DEPTH
            && l.props.get("timeRemap").is_none()
            && let Some(nc) = project.comp(item)
        {
            let nested = Map { offset: map.at(l.start_time), scale: map.scale * l.stretch / 100.0 };
            if nested.scale.is_finite() && nested.scale > 0.0 {
                walk(project, nc, nested, (a, b), depth + 1, budget, out);
            }
        }
    }
}

/// The first frame whose time is at or after `ticks` (a layer is active from its in-point).
fn first_frame_at_or_after(fr: FrameRate, ticks: f64) -> i64 {
    let t = Tick(ticks.ceil().clamp(i64::MIN as f64, i64::MAX as f64) as i64);
    let f = fr.frame_at(t);
    if fr.tick_of(f) < t { f.saturating_add(1) } else { f }
}

/// [`start_frames`] of the comp that plays, kept until the project changes.
#[derive(Default)]
pub struct Sections {
    of: Option<(ItemId, u64, Vec<i64>)>,
}

impl Sections {
    /// The section starts a playing preview at `cur` reaches after `from` (its regular
    /// look-ahead) and within `lead` frames: at most [`AT_ONCE`], nearest first, each moved
    /// onto the preview's frame grid (`step`) and kept within `end`.
    pub fn ahead(&mut self, project: &Project, revision: u64, comp: ItemId, cur: i64, from: i64, lead: i64, step: i64, end: i64) -> Vec<i64> {
        if !matches!(&self.of, Some((c, r, _)) if *c == comp && *r == revision) {
            self.of = Some((comp, revision, start_frames(project, comp)));
        }
        let Some((_, _, starts)) = &self.of else { return Vec::new() };
        let step = step.max(1);
        let last = cur.saturating_add(lead).min(end);
        let first = starts.partition_point(|&f| f <= from);
        let mut out: Vec<i64> = Vec::new();
        for &f in starts.iter().skip(first) {
            // The first frame the preview shows at or after the start.
            let g = cur.saturating_add(f.saturating_sub(cur).saturating_add(step - 1) / step * step);
            if g > last || out.len() >= AT_ONCE {
                break;
            }
            if g > from && out.last() != Some(&g) {
                out.push(g);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use effectcraft_engine::Session;
    use serde_json::json;

    fn secs(s: f64) -> Tick {
        Tick::from_seconds_f64(s)
    }

    fn comp(s: &mut Session, name: &str, duration: f64) -> ItemId {
        ItemId(s.execute("comp.new", json!({"name": name, "width": 64, "height": 36, "frameRate": 30, "duration": duration})).unwrap()["comp"].as_u64().unwrap())
    }

    /// A layer added to the active comp, then timed by `f`.
    fn add(s: &mut Session, comp: ItemId, cmd: &str, params: serde_json::Value, f: &dyn Fn(&mut effectcraft_engine::project::Layer)) {
        let id = s.execute(cmd, params).unwrap()["layer"].as_u64().unwrap();
        let c = s.project.comp_mut(comp).unwrap();
        f(c.layers.iter_mut().find(|l| l.id.0 == id).unwrap());
    }

    /// A 30 fps main comp: a solid from 2 s, a precomp layer placed at 4 s (in-point 5 s)
    /// whose nested comp has a solid starting 3 s in (so at 7 s in main) and one starting before
    /// the precomp layer's in-point (counted at 5 s), and a stretched (200 %) precomp layer at
    /// 10 s whose nested solids start 0.5 s and 3 s in (11 s and 16 s in main).
    #[test]
    fn sections_start_where_layers_start_nested_ones_included() {
        let mut s = Session::default();
        let inner = comp(&mut s, "Inner", 10.0);
        add(&mut s, inner, "layer.newSolid", json!({"name": "N1"}), &|l| l.in_point = secs(3.0));
        add(&mut s, inner, "layer.newSolid", json!({"name": "N2"}), &|l| l.in_point = secs(0.5));
        let main = comp(&mut s, "Main", 20.0);
        add(&mut s, main, "layer.newSolid", json!({"name": "A"}), &|l| l.in_point = secs(2.0));
        add(&mut s, main, "layer.addItem", json!({"item": inner.0}), &|l| {
            l.start_time = secs(4.0);
            l.in_point = secs(5.0);
            l.out_point = secs(9.0);
        });
        add(&mut s, main, "layer.addItem", json!({"item": inner.0}), &|l| {
            l.start_time = secs(10.0);
            l.stretch = 200.0;
            l.in_point = secs(10.0);
            l.out_point = secs(20.0);
        });
        let f = start_frames(&s.project, main);
        assert_eq!(f, vec![60, 150, 210, 300, 330, 480], "a nested start before its precomp layer's in-point counts at the in-point");
        // Playing at 1 s with 16 frames of regular look-ahead and 3 s of lead.
        let mut sections = Sections::default();
        let rev = s.revision;
        assert_eq!(sections.ahead(&s.project, rev, main, 30, 46, 90, 1, 599), vec![60]);
        assert_eq!(sections.ahead(&s.project, rev, main, 140, 156, 90, 1, 599), vec![210]);
        // On a skip grid of 4 frames from 141: 150 → 153, 210 → 213.
        assert_eq!(sections.ahead(&s.project, rev, main, 141, 145, 90, 4, 599), vec![153, 213]);
        // Not past the preview's end.
        assert_eq!(sections.ahead(&s.project, rev, main, 140, 156, 90, 1, 200), Vec::<i64>::new());
    }

    #[test]
    fn deep_or_cyclic_nesting_stays_bounded() {
        let mut s = Session::default();
        let b = comp(&mut s, "B", 5.0);
        let a = comp(&mut s, "A", 5.0);
        s.execute("layer.addItem", json!({"item": b.0})).unwrap();
        // B nesting A back is refused (a cycle) or bounded by the depth limit.
        s.execute("comp.open", json!({"comp": b.0})).unwrap();
        let _ = s.execute("layer.addItem", json!({"item": a.0}));
        assert_eq!(start_frames(&s.project, a), vec![0]);
        assert!(start_frames(&s.project, ItemId(u64::MAX)).is_empty());
    }
}
