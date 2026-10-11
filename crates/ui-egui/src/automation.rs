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
    /// Previous paint pass (for lookups while the current pass is being registered).
    pub previous: Vec<Element>,
    /// The elements of two frames ago, whose strings `add` reuses (no two allocations per
    /// widget per frame).
    spare: Vec<Element>,
    /// The OS window being drawn (elements added now are in it), `None` for the main window.
    pub window: Option<u64>,
}

impl Registry {
    pub fn add(&mut self, id: &str, rect: Rect, label: &str) {
        let rect = [rect.min.x, rect.min.y, rect.width(), rect.height()];
        let window = self.window;
        let e = match self.spare.pop() {
            Some(mut e) => {
                e.id.clear();
                e.id.push_str(id);
                e.label.clear();
                e.label.push_str(label);
                e.rect = rect;
                e.window = window;
                e
            }
            None => Element { id: id.to_string(), label: label.to_string(), rect, window },
        };
        self.elements.push(e);
    }
    pub fn begin_frame(&mut self) {
        let mut recycled = std::mem::replace(&mut self.previous, std::mem::take(&mut self.elements));
        self.spare.append(&mut recycled);
        self.elements = recycled;
    }
    /// Look up a widget during painting, before the current registry is complete.
    pub fn find(&self, id: &str) -> Option<&Element> {
        self.previous.iter().chain(self.elements.iter()).find(|e| e.id == id)
    }
    /// Look up a widget after painting completes (control dispatch and headless input).
    /// Removed widgets must not fall back to rectangles from an older frame.
    pub fn find_current(&self, id: &str) -> Option<&Element> {
        self.elements.iter().find(|e| e.id == id)
    }

    /// The latest completed paint pass's widgets, for between-frame control queries.
    pub fn query_current(&self, prefix: &str) -> Vec<&Element> {
        self.elements.iter().filter(|e| e.id.starts_with(prefix)).collect()
    }

    /// Elements whose id starts with a prefix.
    pub fn query(&self, prefix: &str) -> Vec<&Element> {
        self.previous.iter().filter(|e| e.id.starts_with(prefix)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Registry;
    use egui::{Rect, pos2};

    #[test]
    fn frames_reuse_the_element_strings_and_keep_last_frames_elements() {
        let mut r = Registry::default();
        let rect = Rect::from_min_max(pos2(1.0, 2.0), pos2(4.0, 6.0));
        for frame in 0..4 {
            r.begin_frame();
            r.add(&format!("a.{frame}"), rect, "A");
            r.add("b", rect, &format!("B {frame}"));
            if frame > 0 {
                assert_eq!(r.previous.len(), 2);
                assert_eq!(r.previous[0].id, format!("a.{}", frame - 1));
                assert_eq!(r.find("b").map(|e| e.label.clone()), Some(format!("B {}", frame - 1)));
            }
            assert_eq!(r.elements.len(), 2);
            assert_eq!(r.elements[1].rect, [1.0, 2.0, 3.0, 4.0]);
        }
        // From the third frame on, nothing new is allocated: the spares cover every element.
        assert!(r.spare.is_empty());
        r.begin_frame();
        assert_eq!(r.spare.len(), 2);
    }
}
