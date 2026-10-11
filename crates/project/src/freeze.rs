//! Smart Freeze intent and immutable cache references. Authoring layers are never replaced.

use serde::{Deserialize, Serialize};

use crate::{ItemId, LayerId};
use effectcraft_time::Tick;

pub const MAX_SECTIONS: usize = 1024;
pub const MAX_FRAMES: u64 = 216_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FreezeState {
    #[serde(default)]
    pub sections: Vec<Section>,
    #[serde(default)]
    pub namespace: String,
    #[serde(default = "default_budget")]
    pub budget_bytes: u64,
    #[serde(default = "yes")]
    pub retire_old: bool,
}

fn default_budget() -> u64 {
    500 * 1024 * 1024 * 1024
}

impl Default for FreezeState {
    fn default() -> Self {
        Self { sections: vec![], namespace: String::new(), budget_bytes: default_budget(), retire_old: true }
    }
}

impl FreezeState {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// Precomp mode targets the source comp, so instance transforms/effects remain live.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Target {
    Comp { comp: ItemId },
    Precomp { comp: ItemId },
    Selection { comp: ItemId, layers: Vec<LayerId> },
}

impl Target {
    pub fn comp(&self) -> ItemId {
        match self {
            Self::Comp { comp } | Self::Precomp { comp } | Self::Selection { comp, .. } => *comp,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub id: u64,
    pub name: String,
    pub target: Target,
    pub start: Tick,
    pub end: Tick,
    /// Default UI freezes follow later composition-duration edits; explicit ranges stay fixed.
    #[serde(default)]
    pub follow_comp_range: bool,
    /// Explicit Edit turns this off and suppresses automatic Undo reuse.
    pub enabled: bool,
    #[serde(default)]
    pub auto: bool,
    #[serde(default = "yes")]
    pub quick_draft: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<Version>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous: Vec<Version>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Version {
    /// An owned cache directory; never a source-media path.
    pub directory: String,
    pub identity: String,
    pub draft: bool,
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub bytes: u64,
    pub created_unix: u64,
    pub render_options: String,
    pub frame_rate: effectcraft_time::FrameRate,
    #[serde(default)]
    pub sample: Option<Sample>,
    #[serde(default)]
    pub reused_frames: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub frames: u32,
    pub live_ms: f64,
    pub cached_ms: f64,
    pub render_seconds: f64,
}

impl Sample {
    pub fn worth_freezing(&self, rate: effectcraft_time::FrameRate) -> bool {
        rate.num > 0
            && rate.den > 0
            && self.frames > 0
            && self.live_ms.is_finite()
            && self.cached_ms.is_finite()
            && self.live_ms > 1000.0 / rate.as_f64()
            && self.cached_ms >= 0.0
            && self.live_ms > self.cached_ms * 1.2
            && self.live_ms - self.cached_ms > 1.0
    }
}

impl Section {
    pub fn range(&self, duration: Tick) -> Result<(Tick, Tick), String> {
        let (start, end) = if self.follow_comp_range { (Tick::ZERO, duration) } else { (self.start, self.end) };
        if start < Tick::ZERO || end <= start || end > duration {
            return Err("Smart Freeze range must lie inside the composition; configure the range or freeze the composition again".into());
        }
        Ok((start, end))
    }
    /// Called at the command/cache boundary even for sections loaded from disk.
    pub fn validate(&self) -> Result<(), String> {
        if self.previous.len() > 32 {
            return Err("Smart Freeze section has too many previous versions".into());
        }
        if self.id == 0 || self.target.comp().0 == 0 {
            return Err("Smart Freeze section and composition IDs must be nonzero".into());
        }
        if self.start < Tick::ZERO || self.end <= self.start {
            return Err("Smart Freeze needs a nonempty, nonnegative time range".into());
        }
        if let Target::Selection { layers, .. } = &self.target {
            if layers.is_empty() || layers.len() > 10_000 {
                return Err("Smart Freeze selection must contain 1–10000 layers".into());
            }
            let mut seen = std::collections::BTreeSet::new();
            if layers.iter().any(|id| id.0 == 0 || !seen.insert(*id)) {
                return Err("Smart Freeze selection has invalid or duplicate layer IDs".into());
            }
        }
        if let Some(c) = &self.cache
            && (c.frames == 0
                || c.frames > MAX_FRAMES
                || c.width == 0
                || c.height == 0
                || u64::from(c.width).saturating_mul(u64::from(c.height)) > 33_554_432
                || c.directory.is_empty()
                || c.directory.len() > 4096
                || c.render_options.len() > 4096
                || c.frame_rate.num <= 0
                || c.frame_rate.den <= 0
                || c.identity.is_empty()
                || c.identity.len() > 128)
        {
            return Err("Smart Freeze cache metadata exceeds the supported bounds".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section() -> Section {
        Section {
            id: 1,
            name: "Cards".into(),
            target: Target::Selection { comp: ItemId(2), layers: vec![LayerId(3), LayerId(4)] },
            start: Tick::ZERO,
            end: Tick(100),
            follow_comp_range: false,
            enabled: true,
            auto: false,
            quick_draft: true,
            cache: None,
            previous: vec![],
        }
    }

    #[test]
    fn freeze_intent_roundtrips_without_authoring_changes() {
        let mut p = crate::Project::default();
        p.freeze.sections.push(section());
        let saved = serde_json::to_string(&p).unwrap();
        let restored: crate::Project = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.freeze, p.freeze);
        assert!(restored.items.is_empty());
        assert!(!restored.freeze.sections[0].auto);
        let mut edited = p.clone();
        edited.freeze.sections[0].enabled = false;
        assert!(!p.same_content(&edited), "Edit and cache attachment must create real undo steps");
        let old: crate::Project = serde_json::from_str(&saved.replace(&format!(",\"freeze\":{}", serde_json::to_string(&p.freeze).unwrap()), "")).unwrap();
        assert!(old.freeze.is_empty());
    }

    #[test]
    fn malformed_sections_return_errors() {
        let mut s = section();
        assert!(s.validate().is_ok());
        s.end = Tick::ZERO;
        assert!(s.validate().is_err());
        s.end = Tick(100);
        s.target = Target::Selection { comp: ItemId(2), layers: vec![LayerId(3), LayerId(3)] };
        assert!(s.validate().is_err());
    }
}
