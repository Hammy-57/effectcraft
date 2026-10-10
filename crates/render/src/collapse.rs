//! Collapse Transformations: a precomp layer's nested layers drawn straight into the comp
//! (concatenated transforms, its 3D layers in the comp's 3D space).

use crate::color::Region;
use crate::{EvalCtx, Image, Renderer, color, styles, three_d};
use effectcraft_color::BlendMode;
use effectcraft_effects::Buf;
use effectcraft_geom::{Mat3, Mat4};
use effectcraft_project::{Comp, ItemId, Layer, LayerSource};
use effectcraft_time::Tick;

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
                sub.draw_comp(&nctx, canvas, blank);
            }
            return Region::Empty;
        }
        let Some((sub, nctx)) = self.collapse_into(ctx, layer, item, 1.0) else { return Region::Empty };
        let mut iso = effectcraft_raster::pool::zeroed(canvas.width, canvas.height);
        sub.draw_comp(&nctx, &mut iso, true);
        self.composite_iso(ctx, layer, iso, canvas, opacity);
        Region::Full
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
        let mut iso = effectcraft_raster::pool::zeroed(canvas.width, canvas.height);
        sub.draw_comp(&nctx, &mut iso, true);
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
        self.composite_iso(ctx, layer, done.img, canvas, opacity);
        Region::Full
    }
}
