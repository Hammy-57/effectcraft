//! Convert the persisted workspace schema to shared docking transactions.

use craft_ui::docking::{Action, Floating as SharedFloating, Layout as SharedLayout, Node, Placement, StackEntry as SharedStackEntry};
use craft_ui::layout::{SplitAxis, SplitSize as SharedSplitSize};

use crate::dock::{DockNode, Floating, Layout, PanelKind, SplitSize, StackEntry};

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 4096;
const MAX_PANELS: usize = 128;

fn encode_node(node: &DockNode, depth: usize, count: &mut usize) -> Result<Node<PanelKind>, String> {
    *count = count.saturating_add(1);
    if depth > MAX_DEPTH || *count > MAX_NODES {
        return Err("workspace dock tree exceeds supported limits".into());
    }
    Ok(match node {
        DockNode::Tabs { panels, active, .. } => {
            if panels.len() > MAX_PANELS {
                return Err("too many workspace tabs".into());
            }
            Node::Tabs { panels: panels.clone(), active: *active }
        }
        DockNode::Stack { entries, .. } => {
            if entries.len() > MAX_PANELS {
                return Err("too many workspace stack entries".into());
            }
            if entries.iter().any(|entry| entry.panels.is_empty() || entry.active >= entry.panels.len() || entry.panels.len() > MAX_PANELS) {
                return Err("invalid stacked tab selection".into());
            }
            Node::Stack {
                entries: entries
                    .iter()
                    .flat_map(|entry| entry.panels.iter().map(move |panel| SharedStackEntry { panel: *panel, open: entry.open, height: entry.height }))
                    .collect(),
            }
        }
        DockNode::Split { vertical, size, a, b } => Node::Split {
            axis: if *vertical { SplitAxis::Vertical } else { SplitAxis::Horizontal },
            size: match *size {
                SplitSize::Ratio(value) => SharedSplitSize::Ratio(value),
                SplitSize::FixedA(value) => SharedSplitSize::FixedFirst(value),
                SplitSize::FixedB(value) => SharedSplitSize::FixedSecond(value),
            },
            first: Box::new(encode_node(a, depth.saturating_add(1), count)?),
            second: Box::new(encode_node(b, depth.saturating_add(1), count)?),
        },
    })
}

fn decode_node(node: Node<PanelKind>) -> DockNode {
    match node {
        Node::Tabs { panels, active } => DockNode::Tabs { panels, active, small: false },
        Node::Stack { entries } => DockNode::Stack {
            entries: entries.into_iter().map(|entry| StackEntry::new(entry.panel, entry.open, entry.height)).collect(),
            small: false,
            solo: false,
        },
        Node::Split { axis, size, first, second } => DockNode::Split {
            vertical: axis == SplitAxis::Vertical,
            size: match size {
                SharedSplitSize::Ratio(value) => SplitSize::Ratio(value),
                SharedSplitSize::FixedFirst(value) => SplitSize::FixedA(value),
                SharedSplitSize::FixedSecond(value) => SplitSize::FixedB(value),
            },
            a: Box::new(decode_node(*first)),
            b: Box::new(decode_node(*second)),
        },
    }
}

pub(crate) fn encode(root: &DockNode, floating: &[Floating]) -> Result<SharedLayout<PanelKind>, String> {
    if floating.len() > MAX_PANELS || floating.iter().any(|group| group.panels.len() > MAX_PANELS) {
        return Err("too many floating workspace panels".into());
    }
    let root = if matches!(root, DockNode::Tabs { panels, active: 0, .. } if panels.is_empty()) { None } else { Some(encode_node(root, 0, &mut 0)?) };
    Ok(SharedLayout {
        root,
        floating: floating.iter().map(|group| SharedFloating { panels: group.panels.clone(), active: group.active, rect: group.rect }).collect(),
    })
}

pub(crate) fn decode(layout: SharedLayout<PanelKind>) -> Layout {
    Layout {
        root: layout.root.map(decode_node).unwrap_or_else(|| DockNode::Tabs { panels: vec![], active: 0, small: false }),
        floating: layout.floating.into_iter().map(|group| Floating::new(group.panels, group.active, group.rect)).collect(),
    }
}

/// Restore app-owned accordion tab frames after shared validation/transactions.
/// The shared model represents individual stack panels; grouping stays in the persisted app schema.
fn restore_frames(node: &mut DockNode, original: &DockNode) {
    fn collect(node: &DockNode, out: &mut Vec<StackEntry>) {
        match node {
            DockNode::Split { a, b, .. } => {
                collect(a, out);
                collect(b, out);
            }
            DockNode::Stack { entries, .. } => out.extend(entries.iter().cloned()),
            DockNode::Tabs { .. } => {}
        }
    }
    fn restore(node: &mut DockNode, frames: &[StackEntry]) {
        match node {
            DockNode::Split { a, b, .. } => {
                restore(a, frames);
                restore(b, frames);
            }
            DockNode::Stack { entries, .. } => {
                let mut result: Vec<StackEntry> = Vec::new();
                for entry in std::mem::take(entries) {
                    let Some(panel) = entry.shown() else { continue };
                    let old = frames.iter().find(|frame| frame.contains(panel));
                    if let Some(last) = result.last_mut()
                        && old.is_some_and(|frame| last.panels.iter().all(|p| frame.contains(*p)))
                    {
                        last.panels.push(panel);
                        if old.and_then(StackEntry::shown) == Some(panel) {
                            last.active = last.panels.len().saturating_sub(1);
                        }
                    } else {
                        result.push(entry);
                    }
                }
                for entry in &mut result {
                    if let Some(old) = frames.iter().find(|frame| entry.panels.iter().all(|panel| frame.contains(*panel))) {
                        entry.active = old
                            .shown()
                            .and_then(|shown| entry.panels.iter().position(|panel| *panel == shown))
                            .unwrap_or(old.active.min(entry.panels.len().saturating_sub(1)));
                    }
                }
                *entries = result;
            }
            DockNode::Tabs { .. } => {}
        }
    }
    let mut frames = Vec::new();
    collect(original, &mut frames);
    restore(node, &frames);
}

/// Restore the app-owned group settings the shared model doesn't carry: Small Tabs and Solo
/// Panels in Stack of each docked group, and each floating group's OS window (`id`, `screen`)
/// and Small Tabs. A group keeps the settings of the original group it shares the most panels
/// with; a new group starts with the defaults.
fn restore_group_settings(proposed: &mut Layout, original: &Layout) {
    struct Docked {
        panels: Vec<PanelKind>,
        stack: bool,
        small: bool,
        solo: bool,
    }
    fn collect(node: &DockNode, out: &mut Vec<Docked>) {
        match node {
            DockNode::Split { a, b, .. } => {
                collect(a, out);
                collect(b, out);
            }
            DockNode::Tabs { panels, small, .. } => out.push(Docked { panels: panels.clone(), stack: false, small: *small, solo: false }),
            DockNode::Stack { entries, small, solo } => {
                out.push(Docked { panels: entries.iter().flat_map(|e| e.panels.iter().copied()).collect(), stack: true, small: *small, solo: *solo })
            }
        }
    }
    fn overlap(a: &[PanelKind], b: &[PanelKind]) -> usize {
        a.iter().filter(|p| b.contains(p)).count()
    }
    fn best<'a>(groups: &'a [Docked], panels: &[PanelKind], stack: bool) -> Option<&'a Docked> {
        groups.iter().filter(|g| g.stack == stack).map(|g| (overlap(&g.panels, panels), g)).filter(|(n, _)| *n > 0).max_by_key(|(n, _)| *n).map(|(_, g)| g)
    }
    fn restore(node: &mut DockNode, groups: &[Docked], depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        match node {
            DockNode::Split { a, b, .. } => {
                restore(a, groups, depth.saturating_add(1));
                restore(b, groups, depth.saturating_add(1));
            }
            DockNode::Tabs { panels, small, .. } => {
                if let Some(g) = best(groups, panels, false) {
                    *small = g.small;
                }
            }
            DockNode::Stack { entries, small, solo } => {
                let panels: Vec<PanelKind> = entries.iter().flat_map(|e| e.panels.iter().copied()).collect();
                if let Some(g) = best(groups, &panels, true) {
                    *small = g.small;
                    *solo = g.solo;
                }
            }
        }
    }
    let mut docked = Vec::new();
    collect(&original.root, &mut docked);
    restore(&mut proposed.root, &docked, 0);
    // Floating groups: each original group (and so its OS window) goes to at most one new group,
    // the largest overlaps first.
    let mut pairs: Vec<(usize, usize, usize)> = Vec::new();
    for (i, new) in proposed.floating.iter().enumerate() {
        for (j, old) in original.floating.iter().enumerate() {
            let n = overlap(&new.panels, &old.panels);
            if n > 0 {
                pairs.push((n, i, j));
            }
        }
    }
    pairs.sort_by_key(|&(n, i, j)| (std::cmp::Reverse(n), i.abs_diff(j), i));
    let mut used_new = vec![false; proposed.floating.len()];
    let mut used_old = vec![false; original.floating.len()];
    for (_, i, j) in pairs {
        if used_new.get(i).copied().unwrap_or(true) || used_old.get(j).copied().unwrap_or(true) {
            continue;
        }
        let (Some(new), Some(old)) = (proposed.floating.get_mut(i), original.floating.get(j)) else { continue };
        new.id = old.id;
        new.screen = old.screen;
        new.small = old.small;
        if let Some(u) = used_new.get_mut(i) {
            *u = true;
        }
        if let Some(u) = used_old.get_mut(j) {
            *u = true;
        }
    }
}

pub(crate) fn apply(layout: &mut Layout, action: Action<PanelKind>) -> Result<(), String> {
    let mut shared = encode(&layout.root, &layout.floating)?;
    // Validate the same shared command first. Native stack insertion then retains tabs inside
    // their accordion frame, including the existing before/after insertion policy.
    shared.apply(action.clone()).map_err(|error| error.to_string())?;
    let mut proposed = decode(shared);
    restore_frames(&mut proposed.root, &layout.root);
    restore_group_settings(&mut proposed, layout);
    match action {
        Action::Move { panel, anchor, placement } | Action::OpenAt { panel, anchor, placement } if layout.root.contains(anchor) => {
            fn stacked(node: &DockNode, p: PanelKind) -> bool {
                match node {
                    DockNode::Split { a, b, .. } => stacked(a, p) || stacked(b, p),
                    DockNode::Stack { entries, .. } => entries.iter().any(|e| e.contains(p)),
                    _ => false,
                }
            }
            if stacked(&layout.root, anchor) {
                let mut native = layout.clone();
                native.unfloat(panel);
                let applied = match placement {
                    Placement::Tab { before } => native.root.insert_tab(panel, anchor, before),
                    Placement::Split(zone) => native.root.dock_panel(
                        panel,
                        anchor,
                        match zone {
                            craft_ui::docking::Zone::Left => crate::dock::Zone::Left,
                            craft_ui::docking::Zone::Right => crate::dock::Zone::Right,
                            craft_ui::docking::Zone::Top => crate::dock::Zone::Top,
                            craft_ui::docking::Zone::Bottom => crate::dock::Zone::Bottom,
                            craft_ui::docking::Zone::Center => crate::dock::Zone::Center,
                        },
                    ),
                };
                if !applied {
                    return Err("stacked panel move failed".into());
                }
                proposed = native;
            }
        }
        Action::Activate { panel } => {
            proposed.root.activate(panel);
        }
        Action::SetStackOpen { panel, open } => {
            fn set(node: &mut DockNode, p: PanelKind, open: bool) {
                match node {
                    DockNode::Split { a, b, .. } => {
                        set(a, p, open);
                        set(b, p, open);
                    }
                    DockNode::Stack { entries, solo, .. } => {
                        if let Some(i) = entries.iter().position(|e| e.contains(p)) {
                            if let Some(e) = entries.get_mut(i) {
                                e.open = open;
                            }
                            // Solo Panels in Stack: expanding one collapses the others.
                            if open {
                                crate::dock::solo_expand(entries, i, *solo);
                            }
                        }
                    }
                    _ => {}
                }
            }
            set(&mut proposed.root, panel, open);
        }
        Action::ResizeStack { panel, height } => {
            fn set(node: &mut DockNode, p: PanelKind, h: Option<f32>) {
                match node {
                    DockNode::Split { a, b, .. } => {
                        set(a, p, h);
                        set(b, p, h);
                    }
                    DockNode::Stack { entries, .. } => {
                        if let Some(e) = entries.iter_mut().find(|e| e.contains(p)) {
                            e.height = h;
                        }
                    }
                    _ => {}
                }
            }
            set(&mut proposed.root, panel, height);
        }
        _ => {}
    }
    encode(&proposed.root, &proposed.floating)?.validate().map_err(|error| error.to_string())?;
    *layout = proposed;
    Ok(())
}

pub(crate) fn apply_tree(root: &mut DockNode, action: Action<PanelKind>) -> bool {
    if !valid_tree(root) {
        return false;
    }
    let mut layout = Layout { root: root.clone(), floating: vec![] };
    if apply(&mut layout, action).is_err() {
        return false;
    }
    *root = layout.root;
    true
}

pub(crate) fn valid_tree(root: &DockNode) -> bool {
    encode(root, &[]).is_ok_and(|shared| shared.validate().is_ok())
}

pub(crate) fn valid_for_close(root: &DockNode) -> bool {
    fn normalize(node: &mut Node<PanelKind>) {
        match node {
            Node::Tabs { panels, active } => *active = (*active).min(panels.len().saturating_sub(1)),
            Node::Split { first, second, .. } => {
                normalize(first);
                normalize(second);
            }
            Node::Stack { .. } => {}
        }
    }
    let Ok(mut shared) = encode(root, &[]) else { return false };
    if let Some(root) = &mut shared.root {
        normalize(root);
    }
    shared.validate().is_ok()
}

pub(crate) fn unfloat(layout: &mut Layout, panel: PanelKind) -> bool {
    let Ok(mut shared) = encode(&layout.root, &layout.floating) else { return false };
    let mut found = false;
    for group in &mut shared.floating {
        if group.panels.contains(&panel) {
            group.active = group.active.min(group.panels.len().saturating_sub(1));
            found = true;
        }
    }
    if !found || shared.apply(Action::Close { panel }).is_err() {
        return false;
    }
    let mut proposed = decode(shared);
    restore_frames(&mut proposed.root, &layout.root);
    restore_group_settings(&mut proposed, layout);
    *layout = proposed;
    true
}

#[cfg(test)]
mod stacked_frame_tests {
    use super::*;
    use crate::dock::Zone;
    #[test]
    fn stacked_tabs_survive_shared_resize_activate_float_and_close() {
        let mut root = crate::dock::workspace("Default");
        assert!(root.dock_panel(PanelKind::Info, PanelKind::Properties, Zone::Center));
        fn group(node: &DockNode) -> Option<&StackEntry> {
            match node {
                DockNode::Split { a, b, .. } => group(a).or_else(|| group(b)),
                DockNode::Stack { entries, .. } => entries.iter().find(|e| e.contains(PanelKind::Properties)),
                _ => None,
            }
        }
        let before = group(&root).unwrap().clone();
        assert!(before.contains(PanelKind::Info));
        let mut layout = Layout { root, floating: vec![] };
        layout.apply(Action::ResizeSplit { path: vec![], size: SharedSplitSize::Ratio(0.6) }).unwrap();
        assert_eq!(group(&layout.root).unwrap(), &before);
        layout.apply(Action::Activate { panel: PanelKind::Properties }).unwrap();
        assert_eq!(group(&layout.root).unwrap().shown(), Some(PanelKind::Properties));
        let original_frame = group(&layout.root).unwrap().clone();
        layout.apply(Action::OpenAt { panel: PanelKind::History, anchor: PanelKind::Properties, placement: Placement::Tab { before: None } }).unwrap();
        let opened = group(&layout.root).unwrap();
        assert!(opened.contains(PanelKind::History));
        assert!(opened.contains(PanelKind::Info));
        assert_eq!(opened.shown(), Some(PanelKind::History));
        assert_eq!(opened.height, original_frame.height);
        layout.apply(Action::Close { panel: PanelKind::History }).unwrap();
        layout.apply(Action::Float { panel: PanelKind::Info, rect: [20.0, 20.0, 300.0, 300.0] }).unwrap();
        assert_eq!(group(&layout.root).unwrap().panels, vec![PanelKind::Properties]);
        layout.apply(Action::Close { panel: PanelKind::Info }).unwrap();
        assert!(layout.floating.is_empty());
        assert_eq!(group(&layout.root).unwrap().panels, vec![PanelKind::Properties]);
    }

    /// The fork's group settings (Small Tabs, Solo Panels in Stack, a floating group's OS
    /// window) aren't in the shared model: they survive shared transactions.
    #[test]
    fn group_settings_and_os_windows_survive_shared_transactions() {
        use crate::dock::Floating;
        let mut root = crate::dock::workspace("Default");
        assert!(root.set_small_tabs(PanelKind::Properties, true));
        assert!(root.set_solo(PanelKind::Properties, true));
        root.close(PanelKind::Info);
        root.close(PanelKind::Audio);
        let mut floating = Floating::new(vec![PanelKind::Info, PanelKind::Audio], 0, [10.0, 10.0, 300.0, 200.0]);
        floating.id = 7;
        floating.screen = Some([-900.0, 40.0, 300.0, 200.0]);
        floating.small = true;
        let mut layout = Layout { root, floating: vec![floating] };
        layout.apply(Action::ResizeSplit { path: vec![], size: SharedSplitSize::Ratio(0.6) }).unwrap();
        assert_eq!(layout.root.small_tabs(PanelKind::Properties), Some(true));
        assert_eq!(layout.root.solo(PanelKind::Properties), Some(true));
        // A docked tab moves into the floating group: same group, same OS window.
        layout.apply(Action::Move { panel: PanelKind::Project, anchor: PanelKind::Info, placement: Placement::Tab { before: None } }).unwrap();
        let f = &layout.floating[0];
        assert!(f.panels.contains(&PanelKind::Project));
        assert_eq!((f.id, f.screen, f.small), (7, Some([-900.0, 40.0, 300.0, 200.0]), true));
        // A tab floated out of it gets a new group (and later its own OS window).
        layout.apply(Action::Float { panel: PanelKind::Audio, rect: [400.0, 50.0, 300.0, 200.0] }).unwrap();
        assert_eq!(layout.floating.len(), 2);
        let ids: Vec<u64> = layout.floating.iter().map(|f| f.id).collect();
        assert!(ids.contains(&7) && ids.contains(&0), "{ids:?}");
        let new = layout.floating.iter().find(|f| f.panels == vec![PanelKind::Audio]).unwrap();
        assert_eq!((new.id, new.screen, new.small), (0, None, false));
    }

    /// Solo Panels in Stack: expanding one stacked panel through the shared route collapses
    /// the others.
    #[test]
    fn shared_stack_open_keeps_solo_panels() {
        let mut root = crate::dock::workspace("Default");
        assert!(root.set_solo(PanelKind::Properties, true));
        let mut layout = Layout { root, floating: vec![] };
        fn open_entries(node: &DockNode) -> Option<usize> {
            match node {
                DockNode::Split { a, b, .. } => open_entries(a).or_else(|| open_entries(b)),
                DockNode::Stack { entries, .. } if entries.iter().any(|e| e.contains(PanelKind::Properties)) => Some(entries.iter().filter(|e| e.open).count()),
                _ => None,
            }
        }
        fn open_all(node: &mut DockNode) {
            match node {
                DockNode::Split { a, b, .. } => {
                    open_all(a);
                    open_all(b);
                }
                DockNode::Stack { entries, .. } => entries.iter_mut().for_each(|e| e.open = true),
                DockNode::Tabs { .. } => {}
            }
        }
        layout.apply(Action::SetStackOpen { panel: PanelKind::Properties, open: false }).unwrap();
        open_all(&mut layout.root);
        assert!(open_entries(&layout.root).is_some_and(|n| n > 1), "the Default stack has several panels");
        layout.apply(Action::SetStackOpen { panel: PanelKind::Properties, open: false }).unwrap();
        layout.apply(Action::SetStackOpen { panel: PanelKind::Properties, open: true }).unwrap();
        assert!(layout.root.is_visible(PanelKind::Properties));
        assert_eq!(open_entries(&layout.root), Some(1));
    }
}
