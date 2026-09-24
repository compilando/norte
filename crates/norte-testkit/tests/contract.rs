//! `provider_contract!` green against `MemProvider` in three configurations:
//! the full contract with different capabilities also exercises the
//! auto-skips (case-sensitive vs insensitive, with and without `SERVER_COPY`).

// `CapabilityFlags` reaches each invocation's scope via the generated
// module's imports (norte_proto re-exported by the macro).
use norte_testkit::MemProvider;

fn hostile() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect()
}

norte_vfs::provider_contract! {
    mod mem_unix_like,
    factory: MemProvider::new(),
    root: MemProvider::root(),
    hostile_names: hostile(),
}

norte_vfs::provider_contract! {
    mod mem_case_insensitive,
    factory: MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING,
    ),
    root: MemProvider::root(),
    hostile_names: hostile(),
}

norte_vfs::provider_contract! {
    mod mem_server_copy,
    factory: MemProvider::with_flags(
        CapabilityFlags::SERVER_COPY | CapabilityFlags::CASE_SENSITIVE,
    ),
    root: MemProvider::root(),
    hostile_names: hostile(),
}

// APFS simulation (issue #7): byte-preserving insensitive normalization.
// The hostile roundtrip exercises the skip path on normalization
// collisions (nfc_e_acute/nfd_e_acute) — what happens on real macOS.
norte_vfs::provider_contract! {
    mod mem_apfs_like,
    factory: MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC
            | CapabilityFlags::CASE_PRESERVING
            | CapabilityFlags::SYMLINKS,
    )
    .with_normalization(norte_testkit::Normalization::Insensitive),
    root: MemProvider::root(),
    hostile_names: hostile(),
}

// LOGICAL trash: it is the only testkit configuration where `trash()`
// returns `Some(dest)`, so without it the contract branch that checks "the
// recoverable destination exists and restores" is exercised by NOBODY
// (encoding-auditor MAJOR-4: the object storage provider returned `Some`
// without promising `trash_restorable`, and no contract saw it).
norte_vfs::provider_contract! {
    mod mem_logical_trash,
    factory: MemProvider::with_flags(
        CapabilityFlags::CASE_SENSITIVE | CapabilityFlags::TRASH,
    )
    .with_logical_trash(),
    root: MemProvider::root(),
    hostile_names: hostile(),
}

// Hostile synthetic attrs (#108 block 2): the attrs contract stops
// auto-skipping and exercises real values (non-UTF-8 Bytes, RTL, ZWJ).
norte_vfs::provider_contract! {
    mod mem_attrs,
    factory: MemProvider::new().with_synthetic_attrs(),
    root: MemProvider::root(),
    hostile_names: hostile(),
}
