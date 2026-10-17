//! CR6b: the playback stream's buffer (no network): chunks in and out of order, a late chunk,
//! adaptive resolution, edits while playing. CR6c: drafts stay, runs of missing frames.
#![cfg(all(test, not(target_arch = "wasm32")))]

use std::sync::Arc;

use crate::cloud_stream::{ChunkMeta, StreamBuffer, missing_run};
use crate::frames::FrameKey;

/// A 2×2 frame whose red channel says which copy it is.
fn img(tag: u8, side: usize) -> Arc<egui::ColorImage> {
    Arc::new(egui::ColorImage::new([side, side], vec![egui::Color32::from_rgb(tag, 0, 0); side * side]))
}

fn chunk(seq: u64, revision: u64, first: i64, frames: u32, scale: f32, tag: u8) -> (ChunkMeta, Vec<Arc<egui::ColorImage>>) {
    let side = if scale >= 1.0 { 4 } else { 2 };
    (ChunkMeta { seq, revision, first, frames, scale }, (0..frames).map(|_| img(tag, side)).collect())
}

/// The viewer's frames of comp content `content` at project revision `revision`.
fn series(content: u64, revision: u64) -> FrameKey {
    FrameKey { revision, content, comp: 1, frame: 0, scale: 1000, view: 0, opts: 0 }
}

fn tag_of(b: &mut StreamBuffer, s: &FrameKey, f: i64) -> Option<u8> {
    b.publish(s).into_iter().find(|p| p.key.frame == f).map(|p| p.img.pixels[0].r())
}

#[test]
fn chunks_in_order_and_out_of_order_land_with_the_newest_copy_winning() {
    let mut b = StreamBuffer::new(0, (0, 299, false), 1000);
    for (seq, first) in [(1u64, 0i64), (2, 15)] {
        let (m, i) = chunk(seq, 1, first, 15, 1.0, 10);
        b.arrived(seq, 1000);
        b.insert(m, i, 15.0);
    }
    assert!((0..30).all(|f| b.has(f)) && !b.has(30));
    assert!(b.stats.warmup_ms.is_some());
    assert_eq!((b.stats.chunks, b.stats.full, b.received), (2, 30, Some(2)));
    // Out of order: chunk 4 (frames 45–59) decodes before chunk 3 (30–44).
    let (m, i) = chunk(4, 1, 45, 15, 1.0, 11);
    b.insert(m, i, 15.0);
    assert!(b.has(45) && !b.has(30));
    let (m, i) = chunk(3, 1, 30, 15, 1.0, 12);
    b.insert(m, i, 15.0);
    assert!((0..60).all(|f| b.has(f)));
    // An upgrade (seq 9, full) decodes before the quarter copy it replaces (seq 5): the older
    // copy that lands later doesn't win.
    let (m, i) = chunk(9, 1, 60, 15, 1.0, 20);
    b.insert(m, i, 15.0);
    let (m, i) = chunk(5, 1, 60, 15, 0.25, 21);
    b.insert(m, i, 15.0);
    let s = series(7, 1);
    assert_eq!(tag_of(&mut b, &s, 60), Some(20));
    assert_eq!(b.stats.quarter, 0, "the late quarter copy was no first copy");
}

#[test]
fn a_late_chunk_is_counted_and_passed_frames_are_let_go() {
    let mut b = StreamBuffer::new(0, (0, 299, false), 1000);
    let (m, i) = chunk(1, 1, 0, 15, 1.0, 1);
    b.insert(m, i, 10.0);
    // Playback reached frame 15 and waits for it: the chunk holding it is late.
    b.set_playhead(15);
    assert!(!b.has(0), "frames behind the playhead go (non-looping)");
    let (m, i) = chunk(2, 1, 15, 15, 1.0, 1);
    b.insert(m, i, 10.0);
    assert_eq!(b.stats.late, 1);
    // A chunk well ahead is on time.
    let (m, i) = chunk(3, 1, 30, 15, 1.0, 1);
    b.insert(m, i, 10.0);
    assert_eq!(b.stats.late, 1);
    // Looping: frames before the playhead are reached again after the wrap, so they stay.
    let mut l = StreamBuffer::new(280, (0, 299, true), 1000);
    let (m, i) = chunk(1, 1, 0, 15, 1.0, 1);
    l.insert(m, i, 10.0);
    l.set_playhead(290);
    assert!(l.has(0));
    // ... unless further than the RAM preview holds.
    let mut small = StreamBuffer::new(280, (0, 299, true), 25);
    let (m, i) = chunk(1, 1, 0, 15, 1.0, 1);
    small.insert(m, i, 10.0);
    assert!(small.has(5) && !small.has(6), "only frames within reach are kept");
}

#[test]
fn low_resolution_frames_are_drafts_until_the_full_copy_and_stay_on_stop() {
    let mut b = StreamBuffer::new(0, (0, 299, false), 1000);
    let (m, i) = chunk(1, 1, 0, 15, 0.5, 1);
    b.insert(m, i, 10.0);
    let s = series(3, 1);
    let out = b.publish(&s);
    assert_eq!(out.len(), 15);
    assert_eq!(out[0].img.size, [2, 2], "a half frame is cached as it is, under the viewer's key");
    assert_eq!(out[0].key, FrameKey { frame: 0, ..s });
    assert!(out.iter().all(|p| p.draft && p.current), "a draft, and current (it goes to the disk cache)");
    assert!(b.publish(&s).is_empty(), "published once");
    // The full-size copy replaces it.
    let (m, i) = chunk(2, 1, 0, 15, 1.0, 2);
    b.insert(m, i, 10.0);
    assert_eq!(b.stats.upgrades, 1);
    let out = b.publish(&s);
    assert!(out.iter().all(|p| !p.draft && p.current && p.img.pixels[0].r() == 2));
    // Frames 15–29 at quarter size stay low: drafts, kept when the preview stops (CR6c).
    let (m, i) = chunk(3, 1, 15, 15, 0.25, 3);
    b.insert(m, i, 10.0);
    assert!(b.publish(&s).iter().all(|p| p.draft));
    assert!(b.take_dirty(1).is_empty(), "pausing keeps the drafts");
}

#[test]
fn pausing_right_after_an_edit_takes_out_the_frames_the_server_hasnt_answered_for() {
    let mut b = StreamBuffer::new(0, (0, 299, false), 1000);
    let (m, i) = chunk(1, 1, 0, 15, 1.0, 1);
    b.insert(m, i, 10.0);
    b.publish(&series(5, 1));
    // An edit (revision 2, new content key) before the server's `stale` notice: they show
    // (drafts), but may be old, so a stop takes them out.
    let out = b.publish(&series(6, 2));
    assert!(out.iter().all(|p| p.draft && !p.current));
    assert_eq!(b.take_dirty(2).len(), 15);
    // Once the server answered (frames before 20 didn't change), they stay.
    b.mark_stale(2, 20);
    b.publish(&series(7, 2));
    assert!(b.take_dirty(2).is_empty());
}

#[test]
fn a_stream_plays_the_first_run_of_missing_frames() {
    let order: Vec<i64> = (10..20).chain(0..5).collect();
    let held = |f: i64| !(12..15).contains(&f) && f != 2;
    assert_eq!(missing_run(&order, held), Some((12, 14)));
    // Up to the end of the range: the wrap starts a run of its own.
    let held = |f: i64| f < 18 && f != 0 && f != 1;
    assert_eq!(missing_run(&order, held), Some((18, 19)));
    assert_eq!(missing_run(&order, |_| true), None, "everything held: no stream");
}

#[test]
fn an_edit_marks_frames_stale_and_the_new_revision_replaces_them() {
    let mut b = StreamBuffer::new(0, (0, 299, false), 1000);
    for k in 0..4 {
        let (m, i) = chunk(k + 1, 1, k as i64 * 15, 15, 1.0, 1);
        b.insert(m, i, 10.0);
    }
    let before = series(100, 1);
    b.publish(&before);
    // The edit (revision 2) changes frames from 20 on.
    b.mark_stale(2, 20);
    assert!(b.fresh(19) && !b.fresh(20) && b.has(20), "stale frames keep playing until replaced");
    // A revision-1 chunk still on its way lands after the notice: stale on arrival, fills nothing new.
    let (m, i) = chunk(5, 1, 60, 15, 1.0, 1);
    b.insert(m, i, 10.0);
    assert!(b.has(60) && !b.fresh(60));
    // The comp's content key changed: every frame is published again under it.
    let after = series(200, 2);
    let out = b.publish(&after);
    assert_eq!(out.len(), b.len());
    assert!(out.iter().all(|p| p.key.content == 200));
    assert!(out.iter().all(|p| p.draft && !p.current), "frames of revision 1 under revision 2's key: drafts, never on disk");
    // New chunks of revision 2 replace the stale frames, even an older sequence number would not
    // undo that.
    let (m, i) = chunk(6, 2, 30, 15, 1.0, 9);
    b.insert(m, i, 10.0);
    assert!(b.fresh(30) && b.fresh(44) && !b.fresh(20));
    assert_eq!(tag_of(&mut b, &after, 30), Some(9));
    let (m, i) = chunk(4, 1, 30, 15, 1.0, 1);
    b.insert(m, i, 10.0);
    assert!(b.fresh(30), "an old copy never replaces a current one");
    // The stale copies are taken out of the cache when the preview stops; the fresh ones stay.
    let dirty: Vec<FrameKey> = b.take_dirty(2);
    assert!(dirty.iter().any(|k| k.frame == 20 && k.content == 200));
    assert!(!dirty.iter().any(|k| k.frame == 30 && k.content == 200));
    assert_eq!(b.stats.stale, 1);
}

#[test]
fn a_whole_project_change_invalidates_everything_and_a_failure_is_kept() {
    let mut b = StreamBuffer::new(0, (0, 99, false), 1000);
    let (m, i) = chunk(1, 1, 0, 15, 1.0, 1);
    b.insert(m, i, 10.0);
    b.mark_stale(2, i64::MIN);
    assert!(!b.fresh(0) && b.has(0));
    b.fail("the cloud connection dropped".into());
    b.fail("later".into());
    assert_eq!(b.failed.as_deref(), Some("the cloud connection dropped"));
    assert_eq!(b.stats.state, "failed");
}

#[test]
fn broken_chunks_are_an_error_not_a_crash() {
    assert!(crate::cloud_stream::decode(&[0, 0, 1, 0x65, 9, 9, 9]).is_ok_and(|f| f.is_empty()) || crate::cloud_stream::decode(&[0, 0, 1, 0x65]).is_err());
    assert!(crate::cloud_stream::decode(&[]).is_ok_and(|f| f.is_empty()));
}
