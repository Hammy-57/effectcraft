//! Render boxes: a second, windowless EffectCraft on the same machine as the user's app
//! (`effectcraft-cli serve --follow PORT`) that mirrors the app's project, so agents render and
//! lint there instead of on the GPU and UI thread the user plays back on.
//!
//! - App side, the control method `app.sync {follower, have?}` ([`pull`]): what changed since the
//!   app revision the follower holds, as a CR3 [`Patch`] (a pointer diff of comps and layers,
//!   cheap on the UI thread), or the whole project when it holds none or another one; with the
//!   user's and the agents' cursors (active comp and its time), so a render without `comp` or
//!   `time` shows what it would in the app. The bytes ([`encode_msg`], base64) are made by
//!   [`Pull::into_reply`], which the app runs off its UI thread.
//! - Box side ([`Replica::apply`]): the reply applied to the box's session. A patch that doesn't
//!   start where the box is, or whose hashes differ, leaves the session as it was and resets the
//!   replica: the next pull brings the whole project.
//! - Older apps without `app.sync`: the follower saves a copy (`file.saveCopy`) and loads it
//!   ([`Replica::load`]).

use std::collections::BTreeMap;
use std::sync::Arc;

use effectcraft_project::{ItemId, Project};
use effectcraft_time::Tick;
use serde_json::{Value, json};

use crate::agents::Cursor;
use crate::remote::{FrameMsg, Patch, SyncCache, decode_msg, encode_msg};
use crate::{Session, remote_sync};

/// Followers one session keeps a replica image for (the least recently used is dropped).
pub const MAX_FOLLOWERS: usize = 4;
/// Longest follower name.
pub const MAX_NAME: usize = 128;

/// What the session last sent each follower (`app.sync`).
#[derive(Default)]
pub struct Followers {
    map: BTreeMap<String, Sent>,
    clock: u64,
}

struct Sent {
    revision: u64,
    project: Arc<Project>,
    cache: SyncCache,
    used: u64,
}

impl Followers {
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// One `app.sync` answer before its bytes are made ([`Pull::into_reply`]).
pub struct Pull {
    revision: u64,
    body: Body,
    cursors: Value,
    path: Option<String>,
}

enum Body {
    /// The follower has this revision.
    Same,
    Patch(Patch),
    Full(Arc<Project>),
}

/// `app.sync {follower, have?}` on the session's thread: decide what the follower needs and note
/// it as sent. `have` is the app revision the follower holds (none: it has no project yet).
pub fn pull(s: &mut Session, p: &Value) -> Result<Pull, String> {
    let follower = p
        .get("follower")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty() && f.len() <= MAX_NAME)
        .ok_or_else(|| format!("app.sync: `follower` must be a name of 1 to {MAX_NAME} bytes"))?;
    let have = p.get("have").and_then(Value::as_u64);
    let revision = s.revision;
    let f = &mut s.followers;
    f.clock = f.clock.saturating_add(1);
    let used = f.clock;
    let body = match f.map.get_mut(follower) {
        Some(sent) if have == Some(sent.revision) && sent.revision == revision => Body::Same,
        Some(sent) if have == Some(sent.revision) => match remote_sync::diff_projects(&sent.project, &s.project, &mut sent.cache) {
            Ok(ops) => Body::Patch(Patch { from_rev: sent.revision, to_rev: revision, ops }),
            Err(_) => Body::Full(s.project.clone()),
        },
        _ => Body::Full(s.project.clone()),
    };
    let sent = f.map.entry(follower.to_string()).or_insert_with(|| Sent { revision, project: s.project.clone(), cache: SyncCache::default(), used });
    if matches!(body, Body::Full(_)) {
        sent.cache.clear();
    }
    sent.revision = revision;
    sent.project = s.project.clone();
    sent.used = used;
    sent.cache.trim();
    while f.map.len() > MAX_FOLLOWERS {
        let Some(oldest) = f.map.iter().min_by_key(|(_, v)| v.used).map(|(k, _)| k.clone()) else { break };
        f.map.remove(&oldest);
    }
    Ok(Pull { revision, body, cursors: cursors(s), path: s.path.clone() })
}

impl Pull {
    /// Whether making the reply takes work worth another thread (anything but "no change").
    pub fn heavy(&self) -> bool {
        !matches!(self.body, Body::Same)
    }

    /// The reply: `{revision, kind: same|patch|project, msg?: base64 of encode_msg, bytes,
    /// cursors, path}`.
    pub fn into_reply(self) -> Value {
        let (kind, msg) = match self.body {
            Body::Same => ("same", None),
            Body::Patch(patch) => ("patch", Some(encode_msg(&FrameMsg::Patch(patch)))),
            Body::Full(p) => match serde_json::to_string(&*p) {
                Ok(project) => ("project", Some(encode_msg(&FrameMsg::Project { revision: self.revision, project }))),
                // The follower keeps asking with the revision it has, which no longer matches:
                // the next pull sends the whole project again.
                Err(e) => return json!({"revision": self.revision, "kind": "error", "error": e.to_string()}),
            },
        };
        let mut r = json!({"revision": self.revision, "kind": kind, "cursors": self.cursors, "path": self.path});
        if let Some(m) = msg {
            r["bytes"] = json!(m.len());
            r["msg"] = json!(effectcraft_track::roto::rle::base64_encode(&m));
        }
        r
    }
}

/// `{user: {comp, time}, agents: {name: {comp, time}}}` (`time`: the active comp's, in ticks).
fn cursors(s: &Session) -> Value {
    let one = |comp: Option<ItemId>, times: &BTreeMap<ItemId, Tick>| match comp {
        Some(c) => json!({"comp": c.0, "time": times.get(&c).copied().unwrap_or(Tick::ZERO)}),
        None => Value::Null,
    };
    let agents: serde_json::Map<String, Value> = s.cursors.clients().into_iter().map(|(name, c)| (name.to_string(), one(c.active_comp, &c.times))).collect();
    json!({"user": one(s.state.active_comp, &s.state.times), "agents": agents})
}

/// The box's image of the app's project.
#[derive(Default)]
pub struct Replica {
    /// The app revision the session holds (none: nothing loaded, or it can't be trusted).
    app_revision: Option<u64>,
    cache: SyncCache,
    /// The session's project was loaded from the app at least once.
    loaded: bool,
}

/// What [`Replica::apply`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    Same,
    Patch,
    Project,
}

impl Replica {
    /// The app revision held (`app.sync`'s `have`).
    pub fn have(&self) -> Option<u64> {
        self.app_revision
    }

    /// Ask for the whole project next time.
    pub fn reset(&mut self) {
        self.app_revision = None;
        self.cache.clear();
    }

    /// Apply an `app.sync` reply to `s`. On an error the session is unchanged and the replica
    /// reset.
    pub fn apply(&mut self, s: &mut Session, reply: &Value) -> Result<Applied, String> {
        let r = self.apply_inner(s, reply);
        if r.is_err() {
            self.reset();
        }
        if let Some(c) = reply.get("cursors") {
            apply_cursors(s, c);
        }
        r
    }

    fn apply_inner(&mut self, s: &mut Session, reply: &Value) -> Result<Applied, String> {
        let revision = reply.get("revision").and_then(Value::as_u64).ok_or("app.sync reply without `revision`")?;
        match reply.get("kind").and_then(Value::as_str) {
            Some("same") if self.app_revision == Some(revision) => return Ok(Applied::Same),
            Some("same") => return Err(format!("the app is at {revision}, the box at {:?}", self.app_revision)),
            Some("patch" | "project") => {}
            Some("error") => return Err(reply.get("error").and_then(Value::as_str).unwrap_or("the app could not send the project").to_string()),
            other => return Err(format!("app.sync reply of unknown kind {other:?}")),
        }
        let b64 = reply.get("msg").and_then(Value::as_str).ok_or("app.sync reply without `msg`")?;
        let bytes = effectcraft_track::roto::rle::base64_decode(b64).ok_or("app.sync: bad base64")?;
        match decode_msg(&bytes)? {
            FrameMsg::Project { revision, project } => {
                let p = remote_sync::read_project(project.as_bytes())?;
                self.load(s, revision, p);
                Ok(Applied::Project)
            }
            FrameMsg::Patch(Patch { from_rev, to_rev, ops }) => {
                if self.app_revision != Some(from_rev) {
                    return Err(format!("the patch starts at {from_rev}, the box is at {:?}", self.app_revision));
                }
                // On a copy: a failed patch leaves the session as it was.
                let mut p = s.project.clone();
                remote_sync::apply(&mut p, &ops, &mut self.cache)?;
                s.project = p;
                s.bump();
                self.app_revision = Some(to_rev);
                Ok(Applied::Patch)
            }
            _ => Err("app.sync: the message is neither a project nor a patch".into()),
        }
    }

    /// Load the app's whole project `p` at `app_revision` (an exact copy: no repairs or
    /// upgrades, so later patches apply to what the app has). Caches stay: the renderer keys
    /// them by content.
    pub fn load(&mut self, s: &mut Session, app_revision: u64, p: Project) {
        if !self.loaded {
            // A clean slate the first time (the session's own project, tabs and history).
            s.replace_project(Project::default(), None);
            self.loaded = true;
        }
        self.cache.clear();
        s.project = Arc::new(p);
        s.claims.clear();
        s.sanitize_state();
        s.bump();
        self.app_revision = Some(app_revision);
    }
}

/// Put the app's cursors (`{user, agents}`, see [`pull`]) into the box's session: the user's
/// active comp and time, and each agent's.
pub fn apply_cursors(s: &mut Session, c: &Value) {
    let parse = |v: &Value| -> Option<(ItemId, Tick)> {
        let comp = ItemId(v.get("comp")?.as_u64()?);
        let time = v.get("time").and_then(|t| serde_json::from_value::<Tick>(t.clone()).ok()).unwrap_or(Tick::ZERO);
        Some((comp, time))
    };
    let exists = |s: &Session, comp: ItemId| s.project.comp(comp).is_some();
    if let Some((comp, time)) = c.get("user").and_then(parse).filter(|(comp, _)| exists(s, *comp)) {
        s.state.active_comp = Some(comp);
        if !s.state.open_comps.contains(&comp) {
            s.state.open_comps.push(comp);
        }
        s.state.times.insert(comp, time);
    }
    let Some(agents) = c.get("agents").and_then(Value::as_object) else { return };
    for (name, v) in agents {
        let Some((comp, time)) = parse(v).filter(|(comp, _)| exists(s, *comp)) else { continue };
        if name.is_empty() || name.len() > crate::agents::MAX_CLIENT_LEN {
            continue;
        }
        let mut cursor = s.cursors.take(name).unwrap_or_else(|| Cursor::of(&s.state));
        cursor.active_comp = Some(comp);
        cursor.times.insert(comp, time);
        s.cursors.put(name.clone(), cursor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> Session {
        let mut s = Session::default();
        s.execute("comp.new", json!({"name": "A", "duration": 10})).unwrap();
        s.execute("layer.newSolid", json!({"name": "Red", "color": [1, 0, 0, 1]})).unwrap();
        s
    }

    fn sync(app: &mut Session, boxed: &mut Session, replica: &mut Replica) -> Result<Applied, String> {
        let reply = pull(app, &json!({"follower": "box", "have": replica.have()}))?.into_reply();
        replica.apply(boxed, &reply)
    }

    fn same(a: &Session, b: &Session) -> bool {
        serde_json::to_value(&*a.project).unwrap() == serde_json::to_value(&*b.project).unwrap()
    }

    #[test]
    fn a_box_gets_the_project_then_patches_then_nothing() {
        let (mut a, mut b, mut r) = (app(), Session::default(), Replica::default());
        assert_eq!(sync(&mut a, &mut b, &mut r), Ok(Applied::Project));
        assert!(same(&a, &b));
        assert_eq!(b.state.active_comp, a.state.active_comp, "the user's comp follows");
        a.execute("layer.newSolid", json!({"name": "Blue", "color": [0, 0, 1, 1]})).unwrap();
        a.execute("prop.set", json!({"layer": "Blue", "path": "transform/opacity", "value": 40})).unwrap();
        let rev = b.revision;
        assert_eq!(sync(&mut a, &mut b, &mut r), Ok(Applied::Patch));
        assert!(same(&a, &b));
        assert!(b.revision > rev, "the box's caches see a change");
        assert_eq!(sync(&mut a, &mut b, &mut r), Ok(Applied::Same));
        assert_eq!(r.have(), Some(a.revision));
    }

    #[test]
    fn undo_and_time_reach_the_box() {
        let (mut a, mut b, mut r) = (app(), Session::default(), Replica::default());
        sync(&mut a, &mut b, &mut r).unwrap();
        a.execute("edit.clear", json!({"layers": ["Red"]})).unwrap();
        sync(&mut a, &mut b, &mut r).unwrap();
        assert!(same(&a, &b));
        a.execute("edit.undo", json!({})).unwrap();
        a.set_time(Tick::from_seconds_f64(2.5));
        sync(&mut a, &mut b, &mut r).unwrap();
        assert!(same(&a, &b));
        assert!(a.time() > Tick::ZERO);
        assert_eq!(b.time(), a.time());
    }

    #[test]
    fn a_box_that_lost_step_gets_the_whole_project_again() {
        let (mut a, mut b, mut r) = (app(), Session::default(), Replica::default());
        sync(&mut a, &mut b, &mut r).unwrap();
        // A reply the box never got: the app moved its image on, the box didn't.
        a.execute("layer.newSolid", json!({"name": "Lost"})).unwrap();
        let _ = pull(&mut a, &json!({"follower": "box", "have": r.have()})).unwrap().into_reply();
        a.execute("layer.newSolid", json!({"name": "Next"})).unwrap();
        assert_eq!(sync(&mut a, &mut b, &mut r), Ok(Applied::Project), "`have` no longer matches: the whole project");
        assert!(same(&a, &b));
    }

    #[test]
    fn a_patch_from_elsewhere_leaves_the_box_unchanged_and_resets_it() {
        let (mut a, mut b, mut r) = (app(), Session::default(), Replica::default());
        sync(&mut a, &mut b, &mut r).unwrap();
        let before = b.project.clone();
        let bad = FrameMsg::Patch(Patch { from_rev: 999, to_rev: 1000, ops: vec![] });
        let reply = json!({"revision": 1000, "kind": "patch", "msg": effectcraft_track::roto::rle::base64_encode(&encode_msg(&bad))});
        assert!(r.apply(&mut b, &reply).is_err());
        assert!(Arc::ptr_eq(&before, &b.project));
        assert_eq!(r.have(), None);
        assert!(r.apply(&mut b, &json!({"revision": 3, "kind": "nonsense"})).is_err());
        assert!(r.apply(&mut b, &json!({"kind": "patch"})).is_err());
        assert_eq!(sync(&mut a, &mut b, &mut r), Ok(Applied::Project));
    }

    #[test]
    fn agents_cursors_follow_and_followers_are_bounded() {
        let (mut a, mut b, mut r) = (app(), Session::default(), Replica::default());
        let other = a.execute("comp.new", json!({"name": "B", "duration": 5, "client": "claude"})).unwrap()["comp"].as_u64().unwrap();
        a.execute("time.set", json!({"time": 1.0, "client": "claude"})).unwrap();
        sync(&mut a, &mut b, &mut r).unwrap();
        let c = b.cursors.get("claude").unwrap();
        assert_eq!(c.active_comp, Some(ItemId(other)));
        let want = a.cursors.get("claude").unwrap().times.get(&ItemId(other)).copied();
        assert!(want.is_some_and(|t| t > Tick::ZERO));
        assert_eq!(c.times.get(&ItemId(other)).copied(), want);
        for i in 0..MAX_FOLLOWERS + 3 {
            pull(&mut a, &json!({"follower": format!("f{i}")})).unwrap();
        }
        assert_eq!(a.followers.len(), MAX_FOLLOWERS);
        assert!(pull(&mut a, &json!({"follower": ""})).is_err());
        assert!(pull(&mut a, &json!({})).is_err());
    }
}
