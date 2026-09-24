//! `provider_contract!` over the real FS (tempdir): the same suite
//! `MemProvider` passes, against disk. Runs on all 3 OSes in CI (phase 12).

use norte_vfs_local::LocalProvider;

fn hostile() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect()
}

norte_vfs::provider_contract! {
    mod local_fs,
    factory: {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().to_path_buf();
        // The trash, INSIDE the provider's root: this way the contract can
        // require the recoverable destination to be a path this provider
        // resolves, and no test ends up in the developer's real trash
        // along the way. It creates itself the first time something is
        // buried, so the contract's other tests see no extra entry.
        LocalProvider::rooted(base.clone())
            .with_trash_home(base.join(".xdg"))
            .with_guard(Box::new(dir))
    },
    root: LocalProvider::root(),
    hostile_names: hostile(),
}
