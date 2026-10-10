//! The UI command dispatcher. Each area file of `src/menus/` (`tool.rs`, `view.rs`, `window.rs`,
//! `timeline.rs`, …) lists the ids it handles in `pub(crate) fn handlers() -> &'static [Handler]`;
//! build.rs (`effectcraft-buildgen`) collects every such file into [`super::AREAS`], so adding a
//! UI command is adding an entry to its area file (or a new area file), never a central `match`.
//!
//! Two stages, as before the split: [`Stage::Invoke`] ids are run by `menus::invoke` (menus,
//! shortcuts, the control channel) after its cross-cutting guards (unsaved changes, panel focus…);
//! an id no handler claims goes to the engine (`menus::engine`). [`Stage::Frontend`] ids are the
//! `Event::Frontend` commands engine commands emit, run by `menus::frontend`.
//!
//! Precedence (deterministic, independent of file order): an exact [`Claim::Id`] first, then the
//! longest matching [`Claim::Prefix`]. `tests_dispatch.rs` pins the handled ids and checks that no
//! id is claimed by two area files (a prefix may cover an exact id only within its own file).

use serde_json::Value;

use crate::EffectcraftApp;

/// Runs one command: `(app, ctx, id, params)`. A handler whose condition doesn't hold (a dialog
/// that opens only without params…) hands the command on to `menus::engine` itself.
pub(crate) type Run = fn(&mut EffectcraftApp, &egui::Context, &str, Value) -> Result<Value, String>;

/// Which dispatcher a handler belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// `menus::invoke`: UI commands (menus, shortcuts, `ui.menu.invoke`).
    Invoke,
    /// `menus::frontend`: frontend events of engine commands.
    Frontend,
}

/// The ids a handler claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Claim {
    /// Exactly this id.
    Id(&'static str),
    /// Every id starting with this (`"tool."`).
    Prefix(&'static str),
}

/// One entry of an area file's `handlers()`.
pub(crate) struct Handler {
    pub stage: Stage,
    pub claim: Claim,
    pub run: Run,
}

impl Handler {
    /// A UI command id run by `menus::invoke`.
    pub(crate) const fn ui(id: &'static str, run: Run) -> Self {
        Self { stage: Stage::Invoke, claim: Claim::Id(id), run }
    }

    /// Every UI command id starting with `prefix`.
    pub(crate) const fn ui_prefix(prefix: &'static str, run: Run) -> Self {
        Self { stage: Stage::Invoke, claim: Claim::Prefix(prefix), run }
    }

    /// A frontend command id run by `menus::frontend`.
    pub(crate) const fn frontend(id: &'static str, run: Run) -> Self {
        Self { stage: Stage::Frontend, claim: Claim::Id(id), run }
    }
}

/// Every handler with its area file's name, in file-name order.
pub(crate) fn all() -> impl Iterator<Item = (&'static str, &'static Handler)> {
    super::AREAS.iter().flat_map(|(area, handlers)| handlers().iter().map(move |h| (*area, h)))
}

/// The handler of `id` in `stage`: an exact claim, else the longest matching prefix.
pub(crate) fn find(stage: Stage, id: &str) -> Option<&'static Handler> {
    let of_stage = || all().map(|(_, h)| h).filter(move |h| h.stage == stage);
    of_stage().find(|h| matches!(h.claim, Claim::Id(x) if x == id)).or_else(|| {
        of_stage()
            .filter_map(|h| match h.claim {
                Claim::Prefix(p) if id.starts_with(p) => Some((p.len(), h)),
                _ => None,
            })
            .max_by_key(|(n, _)| *n)
            .map(|(_, h)| h)
    })
}
