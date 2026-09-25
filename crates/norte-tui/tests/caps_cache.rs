//! The capabilities cache by `(scheme, authority)`: what reuses it, what
//! invalidates it and where its cap is. A `cd`'s first page is what brings
//! them in, and `apply_cd` is what stores them.

use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::navigate::{cache_capabilities, needs_capabilities};

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("test wire")
}

fn test_app_at(dir: &VPath) -> App {
    App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir.clone(), Vec::new()),
    )
}

/// H3d: `fs.capabilities` returns catalog AND flags in one response, and
/// both halves get cached. The one that used to be thrown away was the
/// flags half, and throwing it away cost an extra network round trip the
/// next time anyone asked whether the pane was read-only.
#[test]
fn both_halves_of_a_response_are_cached() {
    let dir = vp("mem:///");
    let mut app = test_app_at(&dir);
    let caps = norte_proto::Capabilities {
        flags: norte_proto::CapabilityFlags::READ_ONLY,
        max_path: None,
    };
    let catalog = norte_proto::AttrCatalog::new(Vec::new());

    cache_capabilities(&mut app, &dir, (caps, catalog));

    assert_eq!(app.caps(&dir), Some(&caps), "the flags stayed");
    assert!(
        app.attr_catalog("mem").is_some(),
        "and the catalog, which is the half that was already being saved"
    );
    // And the effect help consumes: with the flag set, the pane is
    // read-only without asking anyone again.
    assert!(app.pane_read_only(0));
}

/// MAJOR-1, the other half: the gate that decides whether to ask has to ask
/// the SAME thing the cache answers. Gated only by the catalog —which is
/// per scheme—, a `cd` to a second `sftp` host never called again, so the
/// first one's caps answered for it for the rest of the session.
#[test]
fn another_authority_of_the_same_scheme_asks_again() {
    let a = vp("sftp://a.org/");
    let b = vp("sftp://b.org/");
    let mut app = test_app_at(&a);
    assert!(
        needs_capabilities(&app, &a),
        "with nothing cached, it is asked"
    );

    cache_capabilities(
        &mut app,
        &a,
        (
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::READ_ONLY,
                max_path: None,
            },
            norte_proto::AttrCatalog::new(Vec::new()),
        ),
    );

    assert!(
        !needs_capabilities(&app, &a),
        "the same host is not asked twice"
    );
    assert!(
        needs_capabilities(&app, &b),
        "b.org has never answered: IT has to be asked"
    );
}

/// #215: another DIRECTORY of the same backend asks again.
///
/// Since ADR 0054 the daemon answers by LOCATION, and the cache was still
/// indexing by connection: under one `file://` there are mounts —an exFAT
/// stick that boxes itself in, an ext4 subtree in `+F`, a read-only bind—
/// and `/home`'s answer was being served for all of them.
#[test]
fn another_directory_of_the_same_backend_asks_again() {
    let home = vp("file:///home/me");
    let usb_stick = vp("file:///media/usb-stick");
    let mut app = test_app_at(&home);

    cache_capabilities(
        &mut app,
        &home,
        (
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::empty(),
                max_path: None,
            },
            norte_proto::AttrCatalog::new(Vec::new()),
        ),
    );

    assert!(!needs_capabilities(&app, &home));
    assert!(
        needs_capabilities(&app, &usb_stick),
        "a different mount answers on its own"
    );
    assert!(
        app.caps(&usb_stick).is_none(),
        "and until it answers, there is no answer of its own to serve"
    );
}

/// The cache has a CAP: a key per directory is no longer bounded by the
/// seven schemes that exist, and walking a large tree would make it grow
/// without end. The oldest one is evicted, arrival order.
#[test]
fn the_capabilities_cache_has_a_cap() {
    let first_dir = vp("file:///d0");
    let mut app = test_app_at(&first_dir);
    let caps = norte_proto::Capabilities {
        flags: norte_proto::CapabilityFlags::empty(),
        max_path: None,
    };
    for i in 0..200 {
        app.insert_caps(&vp(&format!("file:///d{i}")), caps);
    }
    assert!(
        app.caps(&first_dir).is_none(),
        "the first one left once it filled up"
    );
    assert!(
        app.caps(&vp("file:///d199")).is_some(),
        "and the last one is still there"
    );
}

/// The whole seam, against a REAL backend: `first_page` with the gate open
/// brings the caps and the `cd` saves them.
///
/// Without this, the wiring could silently revert and the suite would
/// stay green: `App::pane_read_only` falls back to the SYNTACTIC criterion
/// of the scheme when there are no caps, and today the two agree on every
/// provider that exists. No other test tells "the flags arrived" apart
/// from "the scheme looked like it".
#[tokio::test]
async fn the_first_page_brings_the_caps_and_the_cd_saves_them() {
    use norte_core::backend::Backend;
    use std::sync::Arc;

    let engine = norte_core::Engine::new();
    engine.register_provider(Arc::new(norte_testkit::MemProvider::new()));
    let backend = Backend::Embedded(Arc::new(engine));
    let dir = vp("mem:///");

    let (_first, _stream, _skipped, both) =
        norte_tui::navigate::first_page(&backend, &dir, &[], true)
            .await
            .expect("the memory provider's listing");
    let both = both.expect("with the gate open both HALVES arrive");

    let mut app = test_app_at(&dir);
    assert!(app.caps(&dir).is_none());
    cache_capabilities(&mut app, &dir, both);
    assert!(
        app.caps(&dir).is_some(),
        "the response's caps have to stay in the cache"
    );

    // And with the gate CLOSED it is not asked: the fourth element is None.
    let (_f, _s, _k, none_result) = norte_tui::navigate::first_page(&backend, &dir, &[], false)
        .await
        .expect("the same listing");
    assert!(
        none_result.is_none(),
        "with the gate closed there is no extra round trip"
    );
}
