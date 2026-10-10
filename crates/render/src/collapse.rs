//! Collapse Transformations: a precomp layer's nested layers drawn straight into the comp
//! (concatenated transforms, its 3D layers in the comp's 3D space).

use crate::color::Region;
use crate::eval::with_transform_memo;
use crate::placement::Window;
use crate::{EvalCtx, Image, Renderer, color, styles, three_d};
use effectcraft_color::BlendMode;
use effectcraft_effects::Buf;
use effectcraft_geom::{Mat3, Mat4};
use effectcraft_project::{Comp, ItemId, Layer, LayerSource};
use effectcraft_raster::pool;
use effectcraft_time::Tick;
use rayon::prelude::*;

/// One collapsed precomp layer a nested comp is drawn through.
#[derive(Clone, Copy)]
pub(crate) struct Through<'a> {
    pub(crate) comp_id: ItemId,
    pub(crate) comp: &'a Comp,
    /// The containing comp's time at this frame.
    pub(crate) time: Tick,
    pub(crate) layer: &'a Layer,
    /// The precomp layer is motion blurred: its own motion blurs the nested layers.
    pub(crate) blur: bool,
}

/// Collapsed precomps within collapsed precomps that motion blur follows (more than
/// [`Renderer::collapsed`]'s nesting limit).
pub(crate) const MAX_THROUGH: usize = 16;

/// How a collapsed precomp's 3D layers are placed in the parent (see `Renderer::collapse_into`).
#[derive(Clone, Copy)]
pub(crate) struct Collapse3d<'a> {
    /// Nested comp world → parent world (the precomp layer's world transform).
    pub world: Mat4,
    /// The parent's camera.
    pub cam: three_d::CameraState,
    /// The outermost comp's context (its lights light the nested layers).
    pub parent: EvalCtx<'a>,
}

impl<'a> Renderer<'a> {
    /// The nested comp of a precomp layer whose transformations collapse into this comp: the
    /// Collapse Transformations switch is on and the layer has no masks, effects or layer
    /// styles (those force a flattened render of the precomp, as in After Effects).
    pub fn collapsed(&self, ctx: &EvalCtx, layer: &Layer) -> Option<ItemId> {
        if self.collapses_isolated(ctx, layer) {
            return None;
        }
        self.collapse_target(ctx, layer)
    }

    /// A collapsed precomp drawn as one isolated layer ([`Self::collapsed_fx`]): it has effects,
    /// or it is 3D and holds 2D layers (those are placed by the precomp layer's transform without
    /// the camera's perspective, so they cannot join this comp's 3D space as planes).
    fn collapses_isolated(&self, ctx: &EvalCtx, layer: &Layer) -> bool {
        let effects = layer.switches.effects && layer.effects().is_some_and(|fx| fx.groups().any(|g| g.enabled));
        effects || (layer.is_3d() && self.collapse_target(ctx, layer).is_some_and(|item| self.holds_2d(item)))
    }

    /// How a collapsed precomp layer places its nested comp's 2D layers: its transform, without
    /// the camera even when it is 3D (aerender, SceneYard Trump Photo Zoom In: the paper card
    /// keeps its size and place when the precomp moves in depth, and becomes as small as the 3D
    /// layers once made 3D). A 3D precomp without 2D layers keeps the camera's projection, which
    /// sizes its continuously rasterized layers.
    pub(crate) fn collapse_matrix(&self, ctx: &EvalCtx, layer: &Layer) -> Mat3 {
        let flat = layer.is_3d() && matches!(layer.source, LayerSource::Comp { item } if self.holds_2d(item));
        if !flat {
            return ctx.layer_to_comp(layer).0;
        }
        let m = &ctx.world_matrix(layer).0;
        Mat3([[m[0][0], m[0][1], m[0][3]], [m[1][0], m[1][1], m[1][3]], [0.0, 0.0, 1.0]])
    }

    /// Whether a comp has 2D layers that draw (not cameras, lights, adjustment or guide layers).
    fn holds_2d(&self, item: ItemId) -> bool {
        self.project
            .comp(item)
            .is_some_and(|c| c.layers.iter().any(|l| !l.is_3d() && l.has_video() && !l.switches.adjustment && !l.switches.guide && l.source.is_av()))
    }

    /// A collapsed precomp layer with effects, or a collapsed 3D precomp holding 2D layers:
    /// After Effects still collapses its nested layers (3D ones seen through this comp's camera,
    /// unclipped by the precomp's frame), then runs the effects on the result and composites it
    /// as one layer that does not intersect the comp's other 3D layers (aerender, SceneYard
    /// Trump Photo Zoom In: the street behind the collapsed scene fills the frame; flattened, it
    /// stopped at the precomp's edges).
    pub fn collapsed_fx(&self, ctx: &EvalCtx, layer: &Layer) -> Option<ItemId> {
        if !self.collapses_isolated(ctx, layer) {
            return None;
        }
        self.collapse_target(ctx, layer)
    }

    /// [`Self::collapsed`] without the effects condition.
    fn collapse_target(&self, ctx: &EvalCtx, layer: &Layer) -> Option<ItemId> {
        if !layer.switches.collapse || layer.switches.adjustment || self.depth > 12 {
            return None;
        }
        let LayerSource::Comp { item } = &layer.source else { return None };
        if self.project.comp_contains(*item, ctx.comp_id) || self.project.comp(*item).is_none() {
            return None;
        }
        if layer.masks().is_some_and(|m| !m.children.is_empty()) {
            return None;
        }
        if styles::active(ctx, layer) {
            return None;
        }
        // Instance overrides (Essential Properties) need the nested comp rendered on its own.
        if !effectcraft_project::essential::overridden(layer).is_empty() {
            return None;
        }
        Some(*item)
    }

    /// A renderer and context that draw a collapsed precomp's layers straight into this comp:
    /// their transforms are concatenated with the precomp layer's (one resample, no
    /// rasterisation at the precomp's bounds), their opacity multiplied by `opacity`, and their
    /// 3D layers use this comp's camera and lights.
    pub fn collapse_into(&self, ctx: &EvalCtx<'a>, layer: &'a Layer, item: ItemId, opacity: f32) -> Option<(Renderer<'a>, EvalCtx<'a>)> {
        let nc = self.project.comp(item)?;
        let t = ctx.nested_time(layer);
        if !nc.covers(t) {
            return None;
        }
        let nctx = EvalCtx { comp_id: item, comp: nc, time: t, ..*ctx };
        let world = ctx.world_matrix(layer);
        let l2c = self.collapse_matrix(ctx, layer);
        let outer = Some(self.outer.map_or(l2c, |o| o * l2c));
        let c3 = Collapse3d {
            world: self.collapse3d.map_or(world, |c| c.world * world),
            cam: three_d::compose::camera_for(self, ctx),
            parent: self.collapse3d.map_or(*ctx, |c| c.parent),
        };
        // Motion blur follows the precomp layer's own motion too.
        let link = Through { comp_id: ctx.comp_id, comp: ctx.comp, time: ctx.time, layer, blur: self.mb_on(ctx, layer) };
        let mut through = self.through;
        let slot = through.iter_mut().find(|t| t.is_none());
        let through_lost = self.through_lost || slot.is_none();
        if let Some(slot) = slot {
            *slot = Some(link);
        }
        Some((Renderer { depth: self.depth + 1, outer, opacity_mul: opacity, collapse3d: Some(c3), through, through_lost, ..*self }, nctx))
    }

    pub(crate) fn opacity_mul(&self) -> f32 {
        self.opacity_mul
    }

    /// Draw a collapsed 2D precomp layer: nested layers blend straight into `canvas` with their
    /// own modes (Normal precomp, no matte); otherwise they are drawn into an isolated buffer
    /// that then takes the precomp layer's track matte, Preserve Transparency and blend mode.
    /// Returns the region left to re-quantise (nested layers drawn straight into `canvas`
    /// quantise their own footprints).
    pub(crate) fn draw_collapsed(&self, ctx: &EvalCtx<'a>, layer: &Layer, item: ItemId, canvas: &mut Image, blank: bool) -> Region {
        let opacity = ctx.opacity(layer) as f32 * self.opacity_mul;
        if opacity <= 0.0 {
            return Region::Empty;
        }
        let matte = layer.track_matte.is_some_and(|tm| tm.layer != layer.id && ctx.comp.layer(tm.layer).is_some());
        if layer.blend_mode == BlendMode::Normal && !matte && !layer.preserve_transparency {
            if let Some((sub, nctx)) = self.collapse_into(ctx, layer, item, opacity) {
                with_transform_memo(self.project, || sub.draw_comp(&nctx, canvas, blank));
            }
            return Region::Empty;
        }
        let Some((sub, nctx)) = self.collapse_into(ctx, layer, item, 1.0) else { return Region::Empty };
        let mut iso = pool::zeroed(canvas.width, canvas.height);
        with_transform_memo(self.project, || sub.draw_comp(&nctx, &mut iso, true));
        self.composite_drawn(ctx, layer, iso, canvas, opacity)
    }

    /// Draw a collapsed precomp layer with effects ([`Self::collapsed_fx`]): the nested layers
    /// are drawn collapsed into an isolated frame, the precomp layer's effects run on it in comp
    /// space (as for an adjustment layer) and it takes the layer's matte, mode and opacity.
    pub fn draw_collapsed_fx(&self, ctx: &EvalCtx<'a>, layer: &'a Layer, item: ItemId, canvas: &mut Image) -> Region {
        let opacity = ctx.opacity(layer) as f32 * self.opacity_mul;
        if opacity <= 0.0 {
            return Region::Empty;
        }
        let Some((sub, nctx)) = self.collapse_into(ctx, layer, item, 1.0) else { return Region::Empty };
        let mut iso = pool::zeroed(canvas.width, canvas.height);
        with_transform_memo(self.project, || sub.draw_comp(&nctx, &mut iso, true));
        let offset = self.roi_offset().map(|(x, y, _, _)| [-x, -y]).unwrap_or([0.0; 2]);
        let mut buf = Buf { img: iso, offset, scale: self.opts.scale };
        // Effects run in the working space, not the linear blending space.
        if let Some(c) = self.pipe.from_blend() {
            color::convert(&mut buf.img, &c);
        }
        let mut done = self.apply_effects(ctx, layer, buf, true);
        if let Some(c) = self.pipe.to_blend() {
            color::convert(&mut done.img, &c);
        }
        if done.img.width != canvas.width || done.img.height != canvas.height {
            return Region::Empty;
        }
        self.composite_drawn(ctx, layer, done.img, canvas, opacity)
    }

    /// Composite a collapsed precomp's isolated canvas-sized render (`iso`) with the layer's
    /// matte, Preserve Transparency, mode and opacity, over only the window its non-zero pixels
    /// cover (outside it every pixel is transparent black, which a matte and Preserve
    /// Transparency keep transparent and non-stencil modes skip). Stencil modes, which clear
    /// the comp outside the layer, take the whole canvas. Returns the region it changed.
    fn composite_drawn(&self, ctx: &EvalCtx, layer: &Layer, iso: Image, canvas: &mut Image, opacity: f32) -> Region {
        let size = (canvas.width, canvas.height);
        #[cfg(test)]
        let whole_only = crate::tests_footprint::full_windows() || tests::WHOLE.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let whole_only = false;
        if whole_only || layer.blend_mode.is_stencil() || (iso.width, iso.height) != size {
            self.composite_iso(ctx, layer, iso, canvas, opacity);
            return Region::Full;
        }
        let win = drawn_window(&iso);
        if win.is_empty() {
            pool::recycle(iso);
            return Region::Empty;
        }
        if win.is_whole(size) {
            self.composite_iso(ctx, layer, iso, canvas, opacity);
            return Region::Full;
        }
        let part = pool::crop_of(&iso, win.x, win.y, win.w, win.h);
        pool::recycle(iso);
        self.composite_iso_at(ctx, layer, part, win, canvas, opacity, None);
        win.region(size)
    }
}

/// The smallest window of `img` holding every pixel that is not transparent black (a NaN
/// channel counts as drawn).
fn drawn_window(img: &Image) -> Window {
    let w = img.width.max(1) as usize;
    let drawn = |p: &[f32; 4]| p.iter().any(|c| *c != 0.0);
    let rows = img
        .data
        .par_chunks(w)
        .enumerate()
        .filter_map(|(y, row)| {
            let x0 = row.iter().position(drawn)?;
            let x1 = row.iter().rposition(drawn).unwrap_or(x0);
            Some((x0, x1 + 1, y, y + 1))
        })
        .reduce_with(|a, b| (a.0.min(b.0), a.1.max(b.1), a.2.min(b.2), a.3.max(b.3)));
    match rows {
        Some((x0, x1, y0, y1)) => Window { x: x0 as u32, y: y0 as u32, w: (x1 - x0) as u32, h: (y1 - y0) as u32 },
        None => Window { x: 0, y: 0, w: 0, h: 0 },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use effectcraft_color::{BlendMode, Label};
    use effectcraft_keyframe::{Keyframe, Value};
    use effectcraft_project::build::{self, Ids};
    use effectcraft_project::{BitDepth, Comp, ItemId, ItemKind, Layer, LayerSource, MatteKind, Project, Solid, TrackMatte};
    use effectcraft_time::{FrameRate, Tick};

    use super::drawn_window;
    use crate::{Image, NoFootage, RenderOpts, Renderer};

    thread_local! {
        /// Composite collapsed precomps over the whole canvas (the path before windows).
        pub(super) static WHOLE: Cell<bool> = const { Cell::new(false) };
    }

    fn solid(p: &mut Project, comp: &Comp, color: [f32; 3], (w, h): (u32, u32), pos: [f64; 2]) -> Layer {
        let sid = p.add_item("Solid", Label::Red, None, ItemKind::Solid(Solid { color, width: w, height: h, pixel_aspect: 1.0 }));
        let mut l = build::layer(p, comp, "Solid", LayerSource::Solid { item: sid }, (w, h), None);
        l.props.prop_mut("transform/position").unwrap().value = Value::Vec3([pos[0], pos[1], 0.0]);
        l
    }

    /// A 200×100 comp over a two-solid background with a collapsed precomp of two small solids
    /// (one moving), rotated and scaled, set up by `f` (mode, matte, effects…).
    fn scene(depth: BitDepth, f: &dyn Fn(&mut Project, &Comp, &mut Layer) -> Vec<Layer>) -> (Project, ItemId) {
        let mut p = Project::default();
        p.settings.bit_depth = depth;
        let outer = Comp::new(200, 100, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
        let inner = Comp::new(100, 100, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
        let a = solid(&mut p, &inner, [0.7, 0.2, 0.1], (30, 20), [40.0, 50.0]);
        let mut b = solid(&mut p, &inner, [0.1, 0.4, 0.8], (20, 30), [60.0, 40.0]);
        b.props.prop_mut("transform/position").unwrap().keys =
            vec![Keyframe::new(Tick::ZERO, Value::Vec3([60.0, 40.0, 0.0])), Keyframe::new(Tick::from_seconds_f64(1.0), Value::Vec3([20.0, 70.0, 0.0]))];
        b.props.prop_mut("transform/opacity").unwrap().value = Value::Scalar(75.0);
        let mut inner = inner;
        inner.layers = vec![b, a].into();
        let iid = p.add_item("Inner", Label::Sandstone, None, ItemKind::Comp(inner.into()));
        let mut pre = build::layer(&mut p, &outer, "Inner", LayerSource::Comp { item: iid }, (100, 100), None);
        pre.props.prop_mut("transform/position").unwrap().value = Value::Vec3([110.0, 45.0, 0.0]);
        pre.props.prop_mut("transform/rotation").unwrap().value = Value::Scalar(12.0);
        pre.props.prop_mut("transform/scale").unwrap().value = Value::Vec3([90.0, 110.0, 100.0]);
        pre.switches.collapse = true;
        let mut layers = f(&mut p, &outer, &mut pre);
        layers.push(pre);
        let mut bg = solid(&mut p, &outer, [0.2, 0.5, 0.3], (120, 100), [60.0, 50.0]);
        bg.props.prop_mut("transform/opacity").unwrap().value = Value::Scalar(70.0);
        layers.push(bg);
        layers.push(solid(&mut p, &outer, [0.9, 0.6, 0.2], (200, 100), [100.0, 50.0]));
        let mut c = outer;
        c.layers = layers.into();
        let cid = p.add_item("Outer", Label::Sandstone, None, ItemKind::Comp(c.into()));
        (p, cid)
    }

    fn render(p: &Project, cid: ItemId, t: f64, whole: bool) -> Image {
        WHOLE.with(|w| w.set(whole));
        let img = Renderer::new(p, &NoFootage, RenderOpts::default()).comp_frame(cid, Tick::from_seconds_f64(t));
        WHOLE.with(|w| w.set(false));
        img
    }

    fn assert_same(what: &str, f: &dyn Fn(&mut Project, &Comp, &mut Layer) -> Vec<Layer>) {
        for depth in [BitDepth::Bpc8, BitDepth::Bpc16, BitDepth::Bpc32] {
            let (p, cid) = scene(depth, f);
            for t in [0.0, 0.4] {
                let (a, b) = (render(&p, cid, t, false), render(&p, cid, t, true));
                assert!(a == b, "{what} {depth:?} t {t}: windowed collapsed composite differs from the whole-canvas one");
            }
        }
    }

    fn matte(p: &mut Project, c: &Comp) -> Layer {
        let mut m = solid(p, c, [0.8, 0.5, 0.8], (60, 40), [100.0, 50.0]);
        m.props.prop_mut("transform/rotation").unwrap().value = Value::Scalar(-9.0);
        m.switches.video = false;
        m
    }

    fn blur(p: &mut Project, l: &mut Layer, amount: f64) {
        let spec = effectcraft_effects::find("ec.blur.gaussian").unwrap();
        let mut next = p.next_id;
        let mut g = effectcraft_effects::instantiate(spec, &mut Ids(&mut next), spec.name, [100.0, 100.0]);
        p.next_id = next;
        g.prop_mut("blurriness").unwrap().value = Value::Scalar(amount);
        l.props.sub_mut("effects").unwrap().children.push(g.into());
    }

    /// Collapsed precomps composited in isolation (mode, matte, Preserve Transparency, effects)
    /// only over the window their pixels cover: the same frame as over the whole canvas.
    #[test]
    fn isolated_collapsed_precomps_composite_only_their_window() {
        for mode in [BlendMode::Multiply, BlendMode::Dissolve, BlendMode::Screen, BlendMode::StencilAlpha] {
            assert_same(&format!("{mode:?}"), &|_, _, l| {
                l.blend_mode = mode;
                l.props.prop_mut("transform/opacity").unwrap().value = Value::Scalar(60.0);
                vec![]
            });
        }
        for kind in [MatteKind::Alpha, MatteKind::LumaInverted] {
            assert_same(&format!("matte {kind:?}"), &|p, c, l| {
                let m = matte(p, c);
                l.track_matte = Some(TrackMatte { layer: m.id, kind });
                vec![m]
            });
        }
        assert_same("preserve", &|_, _, l| {
            l.preserve_transparency = true;
            vec![]
        });
        assert_same("effects", &|p, _, l| {
            blur(p, l, 8.0);
            vec![]
        });
        assert_same("effects matte multiply", &|p, c, l| {
            blur(p, l, 4.0);
            l.blend_mode = BlendMode::Multiply;
            let m = matte(p, c);
            l.track_matte = Some(TrackMatte { layer: m.id, kind: MatteKind::Alpha });
            vec![m]
        });
    }

    #[test]
    fn drawn_window_bounds_non_zero_pixels() {
        let mut img = Image::new(10, 8);
        assert!(drawn_window(&img).is_empty());
        img.set(3, 2, [0.0, 0.0, 0.0, 0.5]);
        img.set(6, 5, [0.1, 0.0, 0.0, 0.0]);
        let w = drawn_window(&img);
        assert_eq!((w.x, w.y, w.w, w.h), (3, 2, 4, 4));
        img.set(9, 7, [0.0, f32::NAN, 0.0, 0.0]);
        let w = drawn_window(&img);
        assert_eq!((w.x, w.y, w.w, w.h), (3, 2, 7, 6));
    }
}
