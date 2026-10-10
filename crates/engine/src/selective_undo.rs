//! Undo your own step when others worked after it.
//!
//! With agents and the user in one project, a plain Undo reverts whoever made the last step:
//! the user's ⌘Z would silently remove an agent's work, and after a few agent steps the user's
//! own history would be out of reach. `edit.undo` therefore undoes the caller's own latest step
//! (the user's, or a `client`'s): when someone else made the last step, it reverts only what the
//! caller's step changed, as a new step, and only where nobody changed the same thing since.
//! Otherwise it refuses, naming who did, and changes nothing.
//!
//! What a step changed is compared per project item (a composition, a footage item, a folder)
//! and for the project's own settings; comps and their layers are shared between states, so
//! comparing them is cheap.

use std::collections::BTreeSet;
use std::sync::Arc;

use effectcraft_project::{Item, ItemId, ItemKind, Project};

use crate::{EngineError, Result, Session};

/// Whether two versions of an item are the same (a shared composition compares by pointer).
fn item_eq(a: Option<&Item>, b: Option<&Item>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            let kind = match (&a.kind, &b.kind) {
                (ItemKind::Comp(x), ItemKind::Comp(y)) => Arc::ptr_eq(x, y) || x == y,
                (x, y) => x == y,
            };
            kind && a.id == b.id && a.name == b.name && a.label == b.label && a.comment == b.comment && a.parent == b.parent && a.proxy == b.proxy
        }
        _ => false,
    }
}

/// The project without its items and id counter (its settings, render queue, …).
fn rest(p: &Project) -> Project {
    let mut q = Project { items: Default::default(), ..p.clone() };
    q.next_id = 0;
    q
}

fn who(author: Option<&str>) -> String {
    author.map_or_else(|| "the user".to_string(), |a| format!("client `{a}`"))
}

/// Undo `me`'s (`None`: the user's) latest step on the undo line as a new step. `Ok(None)`: the
/// line holds no step of theirs. Refused, changing nothing, when someone changed the same item
/// (or the project settings) after it.
pub fn undo_own(s: &mut Session, me: Option<&str>) -> Result<Option<String>> {
    let n = s.history.undo.len();
    // State k + 1 is what step k made (the last one made the current state).
    let made: Vec<Arc<Project>> = (0..n).map(|k| s.history.undo.get(k + 1).map_or_else(|| s.project.clone(), |(_, p)| p.clone())).collect();
    let Some(k) = (0..n).rev().find(|&k| made.get(k).is_some_and(|after| s.history.author(after) == me)) else { return Ok(None) };
    let (Some((label, before)), Some(after)) = (s.history.undo.get(k).cloned(), made.get(k).cloned()) else { return Ok(None) };
    let cur = s.project.clone();
    let later: Vec<(String, Option<String>)> =
        (k + 1..n).filter_map(|j| Some((s.history.undo.get(j)?.0.clone(), s.history.author(made.get(j)?).map(str::to_string)))).collect();

    let mut items = cur.items.clone();
    let mut clashes: Vec<String> = vec![];
    let ids: BTreeSet<ItemId> = before.items.keys().chain(after.items.keys()).copied().collect();
    for id in ids {
        let (b, a) = (before.items.get(&id), after.items.get(&id));
        if item_eq(b, a) {
            continue;
        }
        if !item_eq(cur.items.get(&id), a) {
            let name = a.or(b).map(|i| i.name.clone()).unwrap_or_default();
            clashes.push(format!("“{name}”"));
            continue;
        }
        match b {
            Some(b) => items.insert(id, b.clone()),
            None => items.remove(&id),
        };
    }
    let settings = rest(&before) != rest(&after);
    if settings && rest(&cur) != rest(&after) {
        clashes.push("the project settings".into());
    }
    if !clashes.is_empty() {
        let (last, last_by) = later.last().cloned().unwrap_or_default();
        let mine = me.map_or_else(|| "you".to_string(), |m| format!("`{m}`"));
        let others: BTreeSet<String> = later.iter().filter(|(_, a)| a.as_deref() != me).map(|(_, a)| who(a.as_deref())).collect();
        let way = if me.is_some() {
            "undo it there, or undo without `client`"
        } else {
            "use the History panel, or edit.undo {any: true} to undo the last change whoever made it"
        };
        return Err(EngineError::Other(format!(
            "the last change (“{last}”) was made by {}, not {mine}, and {} changed {} since your “{label}”; {way}",
            who(last_by.as_deref()),
            others.into_iter().collect::<Vec<_>>().join(", "),
            clashes.join(", "),
        )));
    }
    let mut next = if settings { (*before).clone() } else { (*cur).clone() };
    next.items = items;
    next.next_id = cur.next_id;
    s.edit(&format!("Undo {label}"), None, move |p, _| {
        *p = next;
        Ok(())
    })?;
    s.sanitize_state();
    Ok(Some(label))
}
