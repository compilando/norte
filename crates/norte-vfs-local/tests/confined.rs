//! Writes that can't escape their root (#164, ADR 0054).
//!
//! The case that names the issue — a symlink in an INTERMEDIATE component
//! that redirects a `Copy` or a `CreateDir` outside the destination — and
//! the two ways this provider has to resolve it: `openat2(RESOLVE_BENEATH)`
//! and, where that syscall isn't available, the component-by-component
//! walk. Both are exercised on the same machine: the second with the seam
//! that disables the first.

#![cfg(unix)]

use std::os::unix::ffi::OsStrExt as _;

use bytes::Bytes;
use norte_proto::{ConflictKind, Error, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("valid segment")
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(seg(name))
}

/// Provider rooted in a tempdir, with `dest/` inside and a sibling `outside/`.
fn scenario() -> (LocalProvider, VPath, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let inside = base.join("dest");
    let outside = base.join("outside");
    std::fs::create_dir(&inside).expect("dest");
    std::fs::create_dir(&outside).expect("outside");
    let p = LocalProvider::rooted(base).with_guard(Box::new(dir));
    let root_path = child(&LocalProvider::root(), b"dest");
    (p, root_path, inside, outside)
}

async fn write_to(
    root: &dyn norte_vfs::ConfinedRoot,
    rel: &[Segment],
    bytes: &[u8],
) -> Result<(), Error> {
    let mut sink = root.write(rel).await?;
    sink.write(Bytes::copy_from_slice(bytes)).await?;
    sink.commit().await
}

/// THE test for #164: the intermediate component is a symlink pointing
/// outside, and the write doesn't land there.
#[tokio::test]
async fn an_intermediate_symlink_does_not_redirect_the_write_outside_the_root() {
    let (p, root_path, inside, outside) = scenario();
    std::os::unix::fs::symlink(&outside, inside.join("sub")).expect("hostile symlink");

    let root = p.open_root(&root_path).await.expect("confined root");
    let err = write_to(root.as_ref(), &[seg(b"sub"), seg(b"loot.txt")], b"x")
        .await
        .expect_err("has to refuse");

    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::EscapesRoot
            }
        ),
        "answered {err:?}"
    );
    assert!(
        !outside.join("loot.txt").exists(),
        "and above all: it did not write outside"
    );
}

/// `CreateDir` has the same hole and the same answer.
#[tokio::test]
async fn an_intermediate_symlink_does_not_redirect_a_mkdir_either() {
    let (p, root_path, inside, outside) = scenario();
    std::os::unix::fs::symlink(&outside, inside.join("sub")).expect("hostile symlink");

    let root = p.open_root(&root_path).await.expect("confined root");
    let err = root
        .mkdir(&[seg(b"sub"), seg(b"new")])
        .await
        .expect_err("has to refuse");

    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::EscapesRoot
            }
        ),
        "answered {err:?}"
    );
    assert!(!outside.join("new").exists(), "did not create outside");
}

/// What's forbidden is ESCAPING, not having symlinks: a RELATIVE one
/// pointing somewhere else INSIDE the root is followed. Forbidding it
/// would break an ordinary `dst/data -> storage` without gaining any
/// security.
#[tokio::test]
async fn a_relative_symlink_that_does_not_escape_the_root_is_followed() {
    let (p, root_path, inside, _outside) = scenario();
    std::fs::create_dir(inside.join("real")).expect("real");
    std::os::unix::fs::symlink("real", inside.join("sub")).expect("internal symlink");

    let root = p.open_root(&root_path).await.expect("confined root");
    write_to(root.as_ref(), &[seg(b"sub"), seg(b"ok.txt")], b"hi")
        .await
        .expect("an internal symlink is not an escape");

    assert_eq!(
        std::fs::read(inside.join("real/ok.txt")).expect("read"),
        b"hi"
    );
}

/// The ordinary case still works.
#[tokio::test]
async fn a_normal_nested_write_works() {
    let (p, root_path, inside, _outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");

    root.mkdir(&[seg(b"sub")]).await.expect("mkdir");
    write_to(root.as_ref(), &[seg(b"sub"), seg(b"ok.txt")], b"hi")
        .await
        .expect("write");

    assert_eq!(
        std::fs::read(inside.join("sub/ok.txt")).expect("read"),
        b"hi"
    );
    let e = root
        .stat(&[seg(b"sub"), seg(b"ok.txt")])
        .await
        .expect("stat");
    assert_eq!(e.kind, norte_proto::EntryKind::File);
    assert_eq!(e.size, Some(2));
}

/// The emulation walk (kernel <5.6, seccomp, macOS) gives the SAME
/// verdicts as the syscall. Forced with the test seam, so both branches
/// get exercised on the same machine.
#[tokio::test]
async fn the_emulation_walk_gives_the_same_verdicts() {
    let _forced = LocalProvider::force_component_walk_for_test();
    let (p, root_path, inside, outside) = scenario();
    std::os::unix::fs::symlink(&outside, inside.join("sub")).expect("hostile symlink");
    std::fs::create_dir(inside.join("real")).expect("real");
    std::os::unix::fs::symlink("real", inside.join("inside_link")).expect("internal symlink");

    let root = p.open_root(&root_path).await.expect("confined root");

    let err = write_to(root.as_ref(), &[seg(b"sub"), seg(b"loot.txt")], b"x")
        .await
        .expect_err("the walk also refuses");
    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::EscapesRoot
            }
        ),
        "answered {err:?}"
    );
    assert!(!outside.join("loot.txt").exists());

    write_to(root.as_ref(), &[seg(b"inside_link"), seg(b"ok.txt")], b"hi")
        .await
        .expect("and follows the symlink that does not escape");
    assert_eq!(
        std::fs::read(inside.join("real/ok.txt")).expect("read"),
        b"hi"
    );
}

/// PUBLICATION is confined just like the write: the staging is created
/// with `openat` in the already-resolved directory and published with
/// `renameat` on that same descriptor, so a symlink slipped in midway
/// doesn't send the rename somewhere else.
#[tokio::test]
async fn publication_is_not_diverted_by_a_symlink_slipped_in_midway() {
    let (p, root_path, inside, outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");

    let mut sink = root.write(&[seg(b"f.txt")]).await.expect("write");
    sink.write(Bytes::from_static(b"content"))
        .await
        .expect("chunk");
    // Race: someone replaces the destination with a symlink BEFORE the commit.
    std::os::unix::fs::symlink(outside.join("other.txt"), inside.join("f.txt"))
        .expect("hostile symlink");
    let err = sink
        .commit()
        .await
        .expect_err("the publish does not overwrite it");

    assert!(matches!(err, Error::Conflict { .. }), "answered {err:?}");
    assert!(
        !outside.join("other.txt").exists(),
        "and it did not write on the other side"
    );
}

/// Cancelling leaves the destination clean or a `.norte-partial`, never an
/// unmarked partial (the usual promise, on this path too).
#[tokio::test]
async fn an_abort_leaves_no_unmarked_partial() {
    let (p, root_path, inside, _outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");

    let mut sink = root.write(&[seg(b"g.txt")]).await.expect("write");
    sink.write(Bytes::from_static(b"halfway"))
        .await
        .expect("chunk");
    sink.abort().await.expect("abort");

    let leftover: Vec<_> = std::fs::read_dir(&inside)
        .expect("list")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert!(
        leftover.is_empty(),
        "abort sweeps its staging: {leftover:?}"
    );
}

/// A root that doesn't exist isn't opened.
#[tokio::test]
async fn a_root_that_does_not_exist_is_not_opened() {
    let (p, _root_path, _inside, _outside) = scenario();
    // `Box<dyn ConfinedRoot>` carries no `Debug`, so `expect_err` won't do.
    let Err(err) = p
        .open_root(&child(&LocalProvider::root(), b"does-not-exist"))
        .await
    else {
        panic!("there is no root to open and it still got opened")
    };
    assert_eq!(err, Error::NotFound);
}

/// And whoever opens it DECLARES it for that location.
#[tokio::test]
async fn confinement_is_announced_in_the_locations_capabilities() {
    let (p, root_path, _inside, _outside) = scenario();
    assert!(
        p.capabilities_at(&root_path)
            .await
            .expect("answers")
            .flags
            .contains(norte_proto::CapabilityFlags::CONFINED_WRITES),
        "on Linux and macOS it's confined, and it's said"
    );
    assert!(
        !p.capabilities()
            .flags
            .contains(norte_proto::CapabilityFlags::CONFINED_WRITES),
        "and never without a location: it depends on the mount and the kernel"
    );
}

/// An ABSOLUTE symlink is rejected even if it points inside the root:
/// that's what `RESOLVE_BENEATH` does, and the emulation walk has to say
/// the same thing or confinement would depend on the kernel version.
#[tokio::test]
async fn an_absolute_symlink_is_rejected_even_if_it_points_inside() {
    for force_walk in [false, true] {
        let _forced = force_walk.then(LocalProvider::force_component_walk_for_test);
        let (p, root_path, inside, _outside) = scenario();
        std::fs::create_dir(inside.join("real")).expect("real");
        std::os::unix::fs::symlink(inside.join("real"), inside.join("abs")).expect("abs symlink");

        let root = p.open_root(&root_path).await.expect("confined root");
        let err = write_to(root.as_ref(), &[seg(b"abs"), seg(b"x.txt")], b"x")
            .await
            .expect_err("absolute is rejected");

        assert!(
            matches!(
                err,
                Error::Conflict {
                    conflict: ConflictKind::EscapesRoot
                }
            ),
            "walk={force_walk}: answered {err:?}"
        );
        assert!(
            !inside.join("real/x.txt").exists(),
            "walk={force_walk}: and it did not write"
        );
    }
}

/// Copying a symlink is CREATING one at the destination, and that
/// creation is confined just like the other two: the hostile intermediate
/// component doesn't get to carry it away.
#[tokio::test]
async fn a_symlink_is_not_created_on_the_other_side_of_a_hostile_component() {
    let (p, root_path, inside, outside) = scenario();
    std::os::unix::fs::symlink(&outside, inside.join("sub")).expect("hostile symlink");

    let root = p.open_root(&root_path).await.expect("confined root");
    let err = root
        .symlink(
            &[seg(b"sub"), seg(b"link")],
            b"/etc/passwd",
            norte_vfs::SymlinkKind::Unknown,
        )
        .await
        .expect_err("has to refuse");

    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::EscapesRoot
            }
        ),
        "answered {err:?}"
    );
    assert!(
        outside.join("link").symlink_metadata().is_err(),
        "did not plant the link outside"
    );
}

/// And inside the root it's created as is, with the target's BYTES
/// untouched: what's confined is WHERE the link lands, not where it
/// points.
#[tokio::test]
async fn a_symlink_inside_the_root_keeps_its_raw_target() {
    let (p, root_path, inside, _outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");

    root.mkdir(&[seg(b"sub")]).await.expect("mkdir");
    root.symlink(
        &[seg(b"sub"), seg(b"link")],
        b"../caf\xe9",
        norte_vfs::SymlinkKind::Unknown,
    )
    .await
    .expect("symlink");

    let read = std::fs::read_link(inside.join("sub/link")).expect("read_link");
    assert_eq!(
        read.as_os_str().as_bytes(),
        b"../caf\xe9",
        "the target's bytes come out as is, never going through UTF-8"
    );

    // And a name already taken is a conflict, not an overwritten link.
    let err = root
        .symlink(
            &[seg(b"sub"), seg(b"link")],
            b"other",
            norte_vfs::SymlinkKind::Unknown,
        )
        .await
        .expect_err("occupied");
    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::Exists
            }
        ),
        "answered {err:?}"
    );
}

/// `Provider::write`'s contract over the confined root: an already
/// occupied destination is known AT OPEN TIME, not after having
/// transferred the file.
#[tokio::test]
async fn an_occupied_destination_is_a_conflict_when_opening_the_sink() {
    let (p, root_path, inside, _outside) = scenario();
    std::fs::write(inside.join("already.txt"), b"what was there").expect("occupant");

    let root = p.open_root(&root_path).await.expect("confined root");
    let err = root
        .write(&[seg(b"already.txt")])
        .await
        .err()
        .expect("the sink never gets to open");

    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::Exists
            }
        ),
        "answered {err:?}"
    );
    assert_eq!(
        std::fs::read(inside.join("already.txt")).expect("read"),
        b"what was there",
        "and it did not touch what was there"
    );
}

/// The emulation walk used to resolve a component in two syscalls: `lstat`
/// to ask if it was a symlink and, if not, an `openat` WITHOUT
/// `O_NOFOLLOW`. A substitution fits between the two, and the `openat`
/// used to follow the link that had just appeared without checking where
/// it landed.
///
/// The test doesn't win the race by hand — it can't, it's nanoseconds —:
/// it puts the tree in the state the race PRODUCES (the component is
/// already a symlink by the time it's resolved) and checks the verdict,
/// which is what the `openat` without `O_NOFOLLOW` used to answer wrong.
/// With `openat2` the first branch doesn't even run, so the walk is forced.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn the_walk_does_not_follow_a_component_that_became_a_symlink_under_its_feet() {
    let _forced = LocalProvider::force_component_walk_for_test();
    let (p, root_path, inside, outside) = scenario();
    // A real directory in the middle, which is what the race's `lstat`
    // would have seen…
    std::fs::create_dir(inside.join("sub")).expect("real sub");
    let root = p.open_root(&root_path).await.expect("confined root");
    // …and by the time it's opened it's already a bridge to `outside`.
    std::fs::remove_dir(inside.join("sub")).expect("remove sub");
    std::os::unix::fs::symlink(&outside, inside.join("sub")).expect("hostile symlink");

    let err = write_to(root.as_ref(), &[seg(b"sub"), seg(b"loot.txt")], b"x")
        .await
        .expect_err("the walk has to refuse");

    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::EscapesRoot
            }
        ),
        "answered {err:?}"
    );
    assert!(
        !outside.join("loot.txt").exists(),
        "and above all: it did not write outside"
    );
}

/// The root is opened by RESOLVING a path, symlinks included — a
/// `~/backups -> /mnt/disk/backups` is a legitimate destination and
/// refusing it would break real trees. That leaves a window: if between
/// validating the path and opening it someone replaces it with a link,
/// what gets opened is another tree and everything that comes after is
/// perfectly confined to the wrong place.
///
/// What the core compares to rule this out is IDENTITY, and this checks
/// that both sides of that comparison exist and are distinguishable: the
/// opened node's (`root_id`) and the one an `lstat` of the path gives when
/// it's a link.
#[tokio::test]
async fn the_roots_identity_gives_away_a_path_replaced_by_a_link() {
    use norte_vfs::FollowLinks;

    let (p, _root_path, inside, outside) = scenario();
    // `dest/bridge` is a link to `outside`, which is what the switcheroo
    // would produce. Opening through it gives `outside`'s root…
    std::os::unix::fs::symlink(&outside, inside.join("bridge")).expect("symlink");
    let via_link = child(&child(&LocalProvider::root(), b"dest"), b"bridge");

    let root = p.open_root(&via_link).await.expect("opens: follows it");
    let opened = root.root_id().await.expect("opened root's id");

    // …and the path's `lstat` gives the LINK's, which is something else.
    let on_path = p
        .node_id(&via_link, FollowLinks::No)
        .await
        .expect("node_id");
    assert!(opened.is_some() && on_path.is_some(), "both sides exist");
    assert_ne!(
        opened, on_path,
        "a path that is a link does not have the identity of the tree it opens"
    );

    // And over a real directory, both identities are the SAME: the check
    // can't give false positives in the ordinary case.
    let direct = child(&LocalProvider::root(), b"dest");
    let root = p.open_root(&direct).await.expect("root");
    assert_eq!(
        root.root_id().await.expect("id"),
        p.node_id(&direct, FollowLinks::No).await.expect("node_id"),
        "a real directory matches itself"
    );
}

/// #218 — the DESTRUCTIVE half also goes through the descriptor.
///
/// `Overwrite` and `Newer` delete before writing, and that deletion used
/// to go by path while the write went confined. With `sub` replaced by a
/// bridge to the outside, the `unlink` used to take a file from ANOTHER
/// tree and only then would the write refuse: a destroyed file, nothing
/// written in its place, and a journal entry naming a place that wasn't it.
#[tokio::test]
async fn an_intermediate_symlink_does_not_redirect_a_delete_outside_the_root() {
    let (p, root_path, inside, outside) = scenario();
    let victim = outside.join("victim.txt");
    std::fs::write(&victim, b"don't delete me").expect("victim");
    std::os::unix::fs::symlink(&outside, inside.join("sub")).expect("hostile symlink");

    let root = p.open_root(&root_path).await.expect("confined root");
    let err = root
        .remove(&[seg(b"sub"), seg(b"victim.txt")])
        .await
        .expect_err("has to refuse");

    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::EscapesRoot
            }
        ),
        "answered {err:?}"
    );
    assert!(victim.exists(), "and above all: it did not delete outside");
}

/// And inside the root it deletes, which is what it exists for.
#[tokio::test]
async fn confined_removal_removes_whats_inside() {
    let (p, root_path, inside, _outside) = scenario();
    let leaf = inside.join("leaf.txt");
    std::fs::write(&leaf, b"x").expect("leaf");

    let root = p.open_root(&root_path).await.expect("confined root");
    root.remove(&[seg(b"leaf.txt")]).await.expect("removes");
    assert!(!leaf.exists());
}

/// A DIRECTORY isn't removed through here: replacing a dir with a leaf is
/// `TypeMismatch`, which is an answer and not a policy. `unlinkat` without
/// `AT_REMOVEDIR` answers `EISDIR` without having touched anything, which
/// is exactly that.
#[tokio::test]
async fn confined_removal_does_not_take_a_directory() {
    let (p, root_path, inside, _outside) = scenario();
    let sub = inside.join("a_dir");
    std::fs::create_dir(&sub).expect("a_dir");
    std::fs::write(sub.join("inside.txt"), b"x").expect("content");

    let root = p.open_root(&root_path).await.expect("confined root");
    let err = root
        .remove(&[seg(b"a_dir")])
        .await
        .expect_err("a dir is not a leaf");
    assert!(
        !matches!(err, Error::NotFound),
        "the error has to say it's a dir, not that it's missing: {err:?}"
    );
    assert!(
        sub.exists(),
        "and the directory is still there with its content"
    );
}

// ---------------------------------------------------------------------------
// Resuming under a confined root (#297).
// ---------------------------------------------------------------------------

/// A confined root RESUMES: its staging carries the stable name, so a
/// later `open_resumable` finds it again and continues after its bytes.
///
/// Until #219 a leaf went WITHOUT confinement and therefore did resume;
/// confining it left it without resume right where it matters most — a
/// large file cut off by just one dropped link — and that was a
/// regression, not a decision.
#[tokio::test]
async fn a_confined_root_resumes_its_own_partial() {
    let (p, root_path, inside, _outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");
    assert!(
        root.resumes(),
        "and it SAYS so, which is what `keep` depends on"
    );

    // First half, and it's KEPT.
    let (mut sink, already) = root
        .open_resumable(&[seg(b"big.bin")])
        .await
        .expect("opens");
    assert_eq!(already, 0, "there was nothing to continue");
    sink.write(Bytes::from_static(b"12345"))
        .await
        .expect("half");
    sink.keep().await.expect("keeps");

    // Second: the partial is found again and it says how many bytes there
    // already are.
    let (mut sink, already) = root
        .open_resumable(&[seg(b"big.bin")])
        .await
        .expect("reopens");
    assert_eq!(already, 5, "the stable staging was found again");
    sink.write(Bytes::from_static(b"67890"))
        .await
        .expect("rest");
    sink.commit().await.expect("publishes");

    assert_eq!(
        std::fs::read(inside.join("big.bin")).expect("published"),
        b"1234567890",
        "and the file is the sum of the two halves, in order"
    );
    // And no partial is left behind.
    let leftover: Vec<_> = std::fs::read_dir(&inside)
        .expect("list")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().as_bytes().starts_with(b".norte-partial"))
        .collect();
    assert!(
        leftover.is_empty(),
        "the publish took the staging with it: {leftover:?}"
    );
}

/// An `abort` over a stable staging DOES delete it: aborting is "I don't
/// want this", and keeping it would leave a partial nobody asked for.
#[tokio::test]
async fn aborting_a_stable_partial_deletes_it() {
    let (p, root_path, inside, _outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");
    let (mut sink, _) = root
        .open_resumable(&[seg(b"big.bin")])
        .await
        .expect("opens");
    sink.write(Bytes::from_static(b"12345"))
        .await
        .expect("half");
    sink.abort().await.expect("aborts");

    let leftover: Vec<_> = std::fs::read_dir(&inside)
        .expect("list")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().as_bytes().starts_with(b".norte-partial"))
        .collect();
    assert!(
        leftover.is_empty(),
        "aborting leaves no partial: {leftover:?}"
    );
}

/// And resume does NOT escape the root either: the staging is opened with
/// `O_NOFOLLOW` in the already-resolved directory, so a hostile
/// intermediate component can't carry it away — same as the normal write.
#[tokio::test]
async fn resume_does_not_escape_via_an_intermediate_symlink_either() {
    let (p, root_path, inside, outside) = scenario();
    std::os::unix::fs::symlink(&outside, inside.join("sub")).expect("hostile symlink");

    let root = p.open_root(&root_path).await.expect("confined root");
    let Err(err) = root.open_resumable(&[seg(b"sub"), seg(b"big.bin")]).await else {
        panic!("has to refuse");
    };

    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::EscapesRoot
            }
        ),
        "answered {err:?}"
    );
    let leftover: Vec<_> = std::fs::read_dir(&outside)
        .expect("list")
        .filter_map(Result::ok)
        .collect();
    assert!(leftover.is_empty(), "left nothing outside: {leftover:?}");
}

/// #296 — the confined `rmdir`, twin of `mkdir`.
///
/// A `Mirror`'s post-order deletion asks for it, reaching each directory
/// once it's already empty. Separate from `remove` for the same reason
/// `unlinkat` has `AT_REMOVEDIR`: they're two distinct effects.
#[tokio::test]
async fn confined_rmdir_does_not_escape_the_root() {
    let (p, root_path, inside, outside) = scenario();
    let victim = outside.join("folder");
    std::fs::create_dir(&victim).expect("victim");
    std::os::unix::fs::symlink(&outside, inside.join("sub")).expect("hostile symlink");

    let root = p.open_root(&root_path).await.expect("confined root");
    let err = root
        .rmdir(&[seg(b"sub"), seg(b"folder")])
        .await
        .expect_err("has to refuse");

    assert!(
        matches!(
            err,
            Error::Conflict {
                conflict: ConflictKind::EscapesRoot
            }
        ),
        "answered {err:?}"
    );
    assert!(
        victim.exists(),
        "and it did not delete the outside directory"
    );
}

/// And inside it deletes the EMPTY directory, which is what it exists for.
/// One with content, NOT: that's an error, not a policy.
#[tokio::test]
async fn confined_rmdir_removes_the_empty_and_refuses_the_full() {
    let (p, root_path, inside, _outside) = scenario();
    std::fs::create_dir(inside.join("empty")).expect("empty");
    std::fs::create_dir(inside.join("full")).expect("full");
    std::fs::write(inside.join("full/x.txt"), b"x").expect("content");

    let root = p.open_root(&root_path).await.expect("confined root");
    root.rmdir(&[seg(b"empty")])
        .await
        .expect("deletes the empty one");
    assert!(!inside.join("empty").exists());

    let err = root
        .rmdir(&[seg(b"full")])
        .await
        .expect_err("one with content, no");
    assert!(!matches!(err, Error::NotFound), "{err:?}");
    assert!(inside.join("full/x.txt").exists(), "and took nothing");
}

// ---------------------------------------------------------------------------
// The stable staging is a PREDICTABLE name, so what's on the other side may
// have been put there by someone else. The three cases `O_NOFOLLOW` doesn't
// cover.
// ---------------------------------------------------------------------------

/// The staging's name, exactly as computed by anyone who knows the
/// destination name.
fn staging_name(final_name: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let d = Sha256::digest(final_name);
    let mut hex = String::new();
    for b in &d[..16] {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    format!(".norte-partial.{hex}")
}

/// A REGULAR FILE planted with the staging's name is NOT resumed.
///
/// `O_NOFOLLOW` rules out a link and nothing more. Resuming over someone
/// else's file publishes under the legitimate name an inode that isn't
/// ours: with its content ahead of ours, with its owner and its
/// permissions, and with the attacker's write descriptor still open on
/// top.
#[tokio::test]
async fn a_staging_planted_by_someone_else_is_not_resumed() {
    let (p, root_path, inside, _outside) = scenario();
    let planted = inside.join(staging_name(b"big.bin"));
    std::fs::write(&planted, b"FOREIGN CONTENT").expect("planted");

    let root = p.open_root(&root_path).await.expect("confined root");
    // A HARDLINK is planted, which is what makes "this inode has another
    // name" observable without depending on the uid (the test runs as the
    // same user).
    let other_name = inside.join("mine.txt");
    std::fs::hard_link(&planted, &other_name).expect("hardlink");

    let Err(err) = root.open_resumable(&[seg(b"big.bin")]).await else {
        panic!("resumed over a file that isn't ours");
    };
    assert!(
        matches!(err, Error::Conflict { .. }),
        "it has to be a conflict, not an I/O error: {err:?}"
    );
    assert_eq!(
        std::fs::read(&planted).expect("still there"),
        b"FOREIGN CONTENT",
        "and nothing got appended to it"
    );
}

/// A FIFO with the staging's name doesn't work either: without
/// `O_NONBLOCK` the `openat` stays hung FOREVER inside the blocking pool,
/// and the cancellation token can't interrupt an in-progress `openat`.
#[tokio::test]
async fn a_fifo_with_the_stagings_name_does_not_hang_the_open() {
    let (p, root_path, inside, _outside) = scenario();
    let fifo = inside.join(staging_name(b"big.bin"));
    let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("cstring");
    // SAFETY: `c` is a live, NUL-terminated CString; `mkfifo` requires no
    // privilege and only writes to the filesystem.
    let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o666) };
    assert_eq!(rc, 0, "mkfifo: {}", std::io::Error::last_os_error());

    let root = p.open_root(&root_path).await.expect("confined root");
    // With a deadline: what this test checks is that it does NOT hang.
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        root.open_resumable(&[seg(b"big.bin")]),
    )
    .await
    .expect("the open has to return, not hang");
    let Err(_err) = r else {
        panic!("a FIFO is not a staging");
    };
    // Whatever the error is doesn't matter and isn't pinned down: with
    // `O_NONBLOCK`, a FIFO with no reader answers `ENXIO` and doesn't even
    // reach the `fstat`. What this test asserts is that it RETURNS —
    // without the flag, the `openat` stays inside the blocking pool
    // waiting for a reader that's never going to show up, and there no
    // cancellation token is worth anything.
}

/// And the normal path still works: a staging we created gets resumed.
/// The defense can't cost the operation it exists to protect.
#[tokio::test]
async fn a_staging_of_our_own_is_resumed_after_the_check() {
    let (p, root_path, _inside, _outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");
    let (mut sink, already) = root
        .open_resumable(&[seg(b"big.bin")])
        .await
        .expect("opens");
    assert_eq!(already, 0);
    sink.write(Bytes::from_static(b"abc")).await.expect("half");
    sink.keep().await.expect("keeps");

    let (_sink, already) = root
        .open_resumable(&[seg(b"big.bin")])
        .await
        .expect("reopens its own");
    assert_eq!(already, 3, "ours passes the check and gets continued");
}

// ---------------------------------------------------------------------------
// The partial's digest, VIA THE DESCRIPTOR (#297 revision).
//
// It's the only verification of the bytes being resumed
// (`VerifyPolicy::Hash`), so it has to look at the SAME file
// `open_resumable` would continue and with the same checks. Another
// file's digest verifies nothing.
// ---------------------------------------------------------------------------

/// SHA-256 of `bytes`, to compare against what the root answers.
fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(bytes).into()
}

/// The partial we left ourselves gives the digest of its first `len` bytes.
#[tokio::test]
async fn the_digest_of_our_own_partial_is_that_of_its_prefix() {
    let (p, root_path, _inside, _outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");
    let (mut sink, _) = root
        .open_resumable(&[seg(b"big.bin")])
        .await
        .expect("opens");
    sink.write(Bytes::from_static(b"abcdef"))
        .await
        .expect("half");
    sink.keep().await.expect("keeps");

    let d = root
        .partial_digest(&[seg(b"big.bin")], 3)
        .await
        .expect("digest");
    assert_eq!(d, Some(sha256(b"abc")));
}

/// With no partial there's no digest, and it isn't an error: the caller
/// degrades to `Length`.
#[tokio::test]
async fn without_a_partial_there_is_no_digest() {
    let (p, root_path, _inside, _outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");
    let d = root
        .partial_digest(&[seg(b"big.bin")], 3)
        .await
        .expect("not an error");
    assert_eq!(d, None);
}

/// Asking for more bytes than the partial has isn't a digest of that
/// prefix: it would be that of a shorter one, and comparing it would give
/// a false "match".
#[tokio::test]
async fn a_partial_shorter_than_requested_gives_no_digest() {
    let (p, root_path, inside, _outside) = scenario();
    std::fs::write(inside.join(staging_name(b"big.bin")), b"ab").expect("partial");
    let root = p.open_root(&root_path).await.expect("confined root");
    let d = root
        .partial_digest(&[seg(b"big.bin")], 3)
        .await
        .expect("digest");
    assert_eq!(d, None);
}

/// A symlink with the staging's name isn't followed: its digest would be
/// that of the file it points at, which may be outside the root.
#[tokio::test]
async fn the_digest_does_not_follow_a_symlink_with_the_stagings_name() {
    let (p, root_path, inside, outside) = scenario();
    let victim = outside.join("secret");
    std::fs::write(&victim, b"abcdef").expect("victim");
    std::os::unix::fs::symlink(&victim, inside.join(staging_name(b"big.bin")))
        .expect("hostile symlink");
    let root = p.open_root(&root_path).await.expect("confined root");
    let d = root.partial_digest(&[seg(b"big.bin")], 3).await;
    assert!(
        !matches!(d, Ok(Some(_))),
        "the digest of what's on the other side is not returned: {d:?}"
    );
}

/// An inode with another name (hardlink) isn't a partial of ours: it's
/// the same single-link check `open_resumable` does.
#[tokio::test]
async fn the_digest_of_a_partial_with_another_link_is_not_returned() {
    let (p, root_path, inside, outside) = scenario();
    let foreign = outside.join("foreign");
    std::fs::write(&foreign, b"abcdef").expect("foreign");
    std::fs::hard_link(&foreign, inside.join(staging_name(b"big.bin"))).expect("hardlink");
    let root = p.open_root(&root_path).await.expect("confined root");
    let d = root
        .partial_digest(&[seg(b"big.bin")], 3)
        .await
        .expect("not an error");
    assert_eq!(d, None);
}

/// An intermediate component that's a symlink to the outside doesn't give
/// the digest of whatever partial is on the other side: it's the case
/// that motivated doing this by descriptor.
#[tokio::test]
async fn the_digest_does_not_cross_an_intermediate_symlink() {
    let (p, root_path, inside, outside) = scenario();
    std::fs::write(outside.join(staging_name(b"big.bin")), b"abcdef").expect("outside partial");
    std::os::unix::fs::symlink(&outside, inside.join("sub")).expect("hostile symlink");
    let root = p.open_root(&root_path).await.expect("confined root");
    let d = root
        .partial_digest(&[seg(b"sub"), seg(b"big.bin")], 3)
        .await;
    assert!(
        !matches!(d, Ok(Some(_))),
        "the outside partial is not verified as if it were ours: {d:?}"
    );
}

/// **Identity via the descriptor says the same thing as identity via path**
/// (#369, ADR 0152).
///
/// They're two distinct readings of the same node — `fstatat` over the
/// root's fd here, `symlink_metadata` there — and undo compares one
/// against the other: the copy notes it via the descriptor, undo reads it
/// via path. If they ever stopped agreeing, EVERY undo of a local copy
/// would get blocked, and the only symptom would be an undo claiming
/// there's something else there.
///
/// That it describes the LINK and not its target lives in the same test
/// because it's the other half of the same promise: this interface's
/// `stat` doesn't follow it either.
#[tokio::test]
async fn identity_via_the_descriptor_is_the_same_as_via_path() {
    use norte_vfs::FollowLinks;

    let (p, root_path, inside, _outside) = scenario();
    std::fs::write(inside.join("f"), b"content").expect("f");
    std::os::unix::fs::symlink("f", inside.join("link")).expect("link");

    let root = p.open_root(&root_path).await.expect("root");

    for name in [&b"f"[..], b"link"] {
        let via_fd = root.node_id(&[seg(name)]).await.expect("node_id via fd");
        let via_path = p
            .node_id(&child(&root_path, name), FollowLinks::No)
            .await
            .expect("node_id via path");
        assert_eq!(
            via_fd,
            via_path,
            "both readings of {} have to give the same node",
            String::from_utf8_lossy(name)
        );
        assert!(via_fd.is_some(), "the local provider DOES have identity");
    }

    // And the link is not its target: following it would give `f`'s identity.
    assert_ne!(
        root.node_id(&[seg(b"link")]).await.expect("link"),
        root.node_id(&[seg(b"f")]).await.expect("f"),
        "describes the LINK, never its target"
    );
}

/// **And it keeps answering when the root is no longer where it was**
/// (#369).
///
/// This is the whole reason the method exists. A descriptor survives a
/// `rename` — that's why a copy kept filling an already-deleted folder —
/// so asking about it keeps working when asking about the path no longer
/// does. Without this, the journal entries that need identity the most
/// are exactly the ones left without it.
#[tokio::test]
async fn identity_via_the_descriptor_survives_the_root_being_moved() {
    use norte_vfs::FollowLinks;

    let (p, root_path, inside, _outside) = scenario();
    let root = p.open_root(&root_path).await.expect("root");
    write_to(&*root, &[seg(b"f")], b"written via the descriptor")
        .await
        .expect("writes");
    let before = root.node_id(&[seg(b"f")]).await.expect("before");

    // What norte's deletion does: move the folder somewhere else.
    std::fs::rename(&inside, inside.with_file_name("trash")).expect("to the trash");

    // By path there's nothing left…
    assert!(
        matches!(
            p.node_id(&child(&root_path, b"f"), FollowLinks::No).await,
            Err(Error::NotFound)
        ),
        "the path no longer leads there, which is the premise"
    );
    // …and via the descriptor it's still there, with the same identity.
    assert_eq!(
        root.node_id(&[seg(b"f")]).await.expect("after"),
        before,
        "the descriptor still sees its node even though the folder is called something else"
    );
}
