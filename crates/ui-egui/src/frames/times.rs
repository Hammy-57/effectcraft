//! The render times of rendered viewer frames, for the Timeline's Render Time pane: the whole
//! frame and each layer (see [`effectcraft_engine::render::LayerTimes`]). They are kept by frame,
//! so a cached frame shows the times from when it was rendered, as in After Effects.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use effectcraft_engine::project::{ItemId, LayerId};

use super::FrameKey;

/// One rendered frame's times (ms).
#[derive(Clone, Debug, Default)]
pub struct FrameTimes {
    /// The whole frame.
    pub frame_ms: f64,
    /// Each layer that rendered, by comp and layer: the frame's comp and the comps nested in it.
    pub layers: HashMap<(ItemId, LayerId), f64>,
    /// Rendered by a remote renderer (the cloud, CR5): its times there, not this machine's.
    pub remote: bool,
}

impl FrameTimes {
    /// The time `layer` of `comp` took on this frame.
    pub fn layer(&self, comp: ItemId, layer: LayerId) -> Option<f64> {
        self.layers.get(&(comp, layer)).copied()
    }
}

/// Layer times kept at most (about 40 bytes each): the oldest frames' times go first.
const BUDGET: usize = 1 << 18;

/// The times of the frames rendered lately, by frame (its content, not its revision).
#[derive(Default)]
pub struct RenderTimes(Mutex<Kept>);

#[derive(Default)]
struct Kept {
    map: HashMap<FrameId, Arc<FrameTimes>>,
    /// Oldest first.
    order: VecDeque<FrameId>,
    layers: usize,
}

type FrameId = (u64, u64, i64, u32, u64, u64);

impl RenderTimes {
    /// Keep `times` as frame `key`'s (replacing older ones).
    pub fn put(&self, key: &FrameKey, times: FrameTimes) {
        let id = key.id();
        let mut k = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(old) = k.map.remove(&id) {
            k.layers = k.layers.saturating_sub(old.layers.len());
            k.order.retain(|o| *o != id);
        }
        k.layers = k.layers.saturating_add(times.layers.len());
        k.map.insert(id, Arc::new(times));
        k.order.push_back(id);
        while k.layers > BUDGET && k.order.len() > 1 {
            let Some(oldest) = k.order.pop_front() else { break };
            if let Some(old) = k.map.remove(&oldest) {
                k.layers = k.layers.saturating_sub(old.layers.len());
            }
        }
    }

    /// Frame `key`'s times, if it rendered lately.
    pub fn get(&self, key: &FrameKey) -> Option<Arc<FrameTimes>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).map.get(&key.id()).cloned()
    }

    /// Forget every frame's times.
    pub fn clear(&self) {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Kept::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(frame: i64) -> FrameKey {
        FrameKey { revision: 1, content: 1, comp: 1, frame, scale: 1000, view: 0, opts: 0 }
    }

    fn times(n: u64) -> FrameTimes {
        FrameTimes { frame_ms: 1.0, layers: (0..n).map(|i| ((ItemId(1), LayerId(i)), 1.0)).collect(), remote: false }
    }

    #[test]
    fn a_frame_keeps_its_latest_times_and_old_frames_go_past_the_budget() {
        let t = RenderTimes::default();
        t.put(&key(0), times(2));
        t.put(&FrameKey { revision: 2, ..key(0) }, times(3));
        // Another revision with the same content is the same frame.
        assert_eq!(t.get(&key(0)).map(|f| f.layers.len()), Some(3));
        let per = 1 << 16;
        for f in 1..=4 {
            t.put(&key(f), times(per));
        }
        assert!(t.get(&key(0)).is_none(), "the oldest frame's times go first");
        assert!(t.get(&key(4)).is_some());
        assert_eq!(t.get(&key(4)).and_then(|f| f.layer(ItemId(1), LayerId(7))), Some(1.0));
    }
}
