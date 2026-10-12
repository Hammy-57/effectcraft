//! Fingerprints of whole comp frames: a hash of everything a frame's pixels are made of at its
//! time, so a viewer frame cached before an edit can be shown after it whenever the edit didn't
//! change that frame (a keyframe edit changes the frames between its neighbouring keyframes,
//! not the others), and undo finds the frames of the state it returns to by content.
//!
//! It is the comp's content key ([`Renderer::comp_key`]): its settings and layers (structure
//! without keyframes where nothing reads other times), each animated property's value at the
//! time (expressions included), which layers are active, the footage and its source frame,
//! nested comps by the same rules, the project settings and the render options. `None` where
//! that can't be proven: a time reader (time effects, motion blur, Auto-Orient along a path,
//! paint spans) meeting an expression, a frozen preview, too deep a nesting.

use std::hash::{Hash, Hasher};

use effectcraft_project::ItemId;
use effectcraft_time::Tick;

use crate::Renderer;
use crate::cache::{hash_debug, hasher};

impl Renderer<'_> {
    /// The fingerprint of the frame of comp `id` at `t` as this renderer draws it (see the
    /// module docs): two frames with the same fingerprint have the same pixels. `None` when it
    /// can't be told.
    pub fn frame_fingerprint(&self, id: ItemId, t: Tick) -> Option<u64> {
        // Frozen previews show cached pixels, nested renderers draw into another frame.
        if self.freeze.is_some() || self.depth != 0 || !self.top || self.layer_view.is_some() {
            return None;
        }
        // Which compositor draws the frame isn't part of what the frame shows.
        let r = Renderer { accel: None, cache: None, profile: None, ..*self };
        let key = r.comp_key(id, t)?;
        let mut h = hasher(0x0f1e_2d3c_4b5a_6978);
        key.hash(&mut h);
        id.hash(&mut h);
        t.0.hash(&mut h);
        hash_debug(&mut h, &self.opts);
        Some(h.finish())
    }
}
