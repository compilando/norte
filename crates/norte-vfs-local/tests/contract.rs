//! `provider_contract!` sobre el FS real (tempdir): la misma suite que pasa
//! `MemProvider`, contra disco. En CI corre en los 3 OS (fase 12).

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
        LocalProvider::rooted(base).with_guard(Box::new(dir))
    },
    root: LocalProvider::root(),
    hostile_names: hostile(),
}
