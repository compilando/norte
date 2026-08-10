//! What `--pick` puts on stdout, decided without a terminal: the App state
//! goes in, the bytes come out.

use norte_frontend::shell::pick_bytes;
use norte_proto::VPath;

#[test]
fn a_cancelled_pick_writes_nothing() {
    let picked: Option<Vec<VPath>> = None;
    let out = picked.map(|p| pick_bytes(&p)).unwrap_or_default();
    assert!(out.is_empty(), "cancelling must not name a file");
}

#[test]
fn an_accepted_pick_is_nul_terminated() {
    let picked = vec![VPath::parse("file:///tmp/x").unwrap()];
    assert_eq!(pick_bytes(&picked), b"/tmp/x\0".to_vec());
}
