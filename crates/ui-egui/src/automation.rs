//! Automation registry: every interactive element registers a stable id and its screen rect each
//! frame, so agents can click/drag by id (`ui.click {id:"timeline.track.V1.lock"}`) and
//! `ui.inspect` can report the full on-screen widget tree.

use egui::Rect;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Element {
    pub id: String,
    pub label: String,
    pub rect: [f32; 4],
    /// The undocked panel group's OS window the element is in ([`crate::dock::Floating::id`]);
    /// `rect` is then in that window's coordinates. Absent for the main window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
}

#[derive(Default)]
pub struct Registry {
    pub elements: Vec<Element>,
    /// Last frame's elements (for lookups from control handlers between frames).
    pub previous: Vec<Element>,
    /// The OS window being drawn (elements added now are in it), `None` for the main window.
    pub window: Option<u64>,
}

impl Registry {
    pub fn add(&mut self, id: &str, rect: Rect, label: &str) {
        self.elements.push(Element {
            id: id.to_string(),
            label: label.to_string(),
            rect: [rect.min.x, rect.min.y, rect.width(), rect.height()],
            window: self.window,
        });
    }
    pub fn begin_frame(&mut self) {
        self.previous = std::mem::take(&mut self.elements);
    }
    pub fn find(&self, id: &str) -> Option<&Element> {
        self.previous.iter().chain(self.elements.iter()).find(|e| e.id == id)
    }
    /// Elements whose id starts with a prefix.
    pub fn query(&self, prefix: &str) -> Vec<&Element> {
        self.previous.iter().filter(|e| e.id.starts_with(prefix)).collect()
    }
}
