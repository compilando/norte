//! `Engine::close_connection` (#140): disconnecting DROPS the session.
//!
//! Without this, "disconnect" only moved the pane elsewhere while the socket
//! stayed open until the session expired on its own.

use std::sync::Arc;

use norte_core::Engine;
use norte_proto::VPath;
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

/// A PROCESS provider — registered under its whole scheme, like the local one —
/// is not a session: there is nothing to drop, and answering yes would be
/// lying about something that stays exactly the same.
#[tokio::test]
async fn a_process_provider_does_not_close() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    assert!(
        !engine.close_connection(&vp("mem:///home")),
        "there was no session to close"
    );
    // And it still serves: closing something that was not a session must not
    // leave the scheme unusable.
    mem.mkdir(&vp("mem:///home")).await.expect("mkdir");
    assert!(engine.stat(&vp("mem:///home")).await.is_ok());
}

/// Closing what is not there is `false`, not an error: whoever disconnects
/// wants to end up without a connection, and already is.
#[tokio::test]
async fn closing_what_is_not_there_is_not_an_error() {
    let engine = Engine::new();
    assert!(!engine.close_connection(&vp("sftp://host/home")));
}
