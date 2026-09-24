//! The OTHER decision `walk_trail` depends on that nobody was poking at:
//! which navigations enter the trail. The `trail == Trail::Record` guard is
//! the only line stopping `nav.back` from feeding off its own trail.

use norte_frontend::history::Popular;
use norte_proto::VPath;
use norte_tui::app::{Trail, TrailStep};
use norte_tui::nav;
use norte_tui::navigate::record_step;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("test wire")
}

/// A USER navigation leaves a mark in all three structures: the trail
/// `nav.back` walks, the MRU the popup paints, and the populars.
#[test]
fn a_user_navigation_enters_the_trail_and_the_mru() {
    let (mut h, mut p) = (nav::History::default(), Popular::default());
    record_step(
        &mut h,
        &mut p,
        &vp("mem:///a"),
        &vp("mem:///b"),
        Trail::Record,
    );
    assert_eq!(h.back_len(), 1, "one step in the trail");
    assert!(h.entries().contains(&vp("mem:///a")), "and in the MRU");
    assert_eq!(
        p.entries()[0].path,
        vp("mem:///b"),
        "and a visit to where it lands"
    );
}

/// THE guard. A `Replay` is the trail walking itself: if it recorded, going
/// back from B to A would log "was at B", the next back would return to B,
/// and the reader would oscillate between two directories forever. Delete
/// `|| trail != Trail::Record` from `record_visit` and this test goes red.
#[test]
fn a_replay_does_not_feed_the_trail() {
    let (mut h, mut p) = (nav::History::default(), Popular::default());
    record_step(
        &mut h,
        &mut p,
        &vp("mem:///b"),
        &vp("mem:///a"),
        Trail::Replay(TrailStep::Back),
    );
    assert_eq!(h.back_len(), 0, "a step back never produces a trail entry");
    assert!(
        h.entries().is_empty(),
        "nor does it enter the MRU: going back is not visiting a new place"
    );
    assert!(p.entries().is_empty(), "nor does it count as a visit");
}

/// A cd to the SAME dir (refresh-like) is not a step the reader took:
/// recording it would make the next `nav.back` do nothing visible.
#[test]
fn a_cd_to_the_same_dir_is_not_a_step() {
    let (mut h, mut p) = (nav::History::default(), Popular::default());
    record_step(
        &mut h,
        &mut p,
        &vp("mem:///a"),
        &vp("mem:///a"),
        Trail::Record,
    );
    assert_eq!(h.back_len(), 0);
    assert!(h.entries().is_empty());
    assert!(p.entries().is_empty());
}
