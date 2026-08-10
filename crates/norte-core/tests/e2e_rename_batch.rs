//! The exit criterion of the batch rename executor (spec §17, ADR 0042), over
//! the REAL local provider.
//!
//! `rename_batch.rs` proves the wiring against `MemProvider`, which is a map
//! from bytes to bytes and therefore agrees with the planner by construction. A
//! real directory does not: it holds an NFD spelling and an NFC spelling as TWO
//! files, it hands back names that are not UTF-8, and a rename there is a
//! syscall that can refuse. What is proved here is that the plan the core made
//! survives contact with it, and that `undo_session` puts every byte back where
//! it started.
//!
//! **Every assertion is about CONTENTS under a name.** Each file's content is a
//! marker with nothing to do with its name, so "`2.txt` holds the first
//! episode" is a claim that a rename happened; "`2.txt` exists" would pass on a
//! directory nobody touched. The comparison is against the WHOLE directory as a
//! map, so a leftover temporary or an extra file fails it too.
//!
//! **Linux only, and not out of laziness.** These tests do not merely prefer a
//! Linux filesystem, they encode two of its properties as the thing under test:
//! a name that is not valid UTF-8 can exist at all (APFS answers `EILSEQ` and
//! the seed would panic before any assertion ran), and an NFD spelling and an
//! NFC spelling are two files (APFS has been normalisation-insensitive since
//! 10.13, so the second seed would open the first file). Neither can be
//! rewritten into a portable shape without giving up the hostile case; running
//! them elsewhere would report a red test for a filesystem behaving correctly.
//! `norte-vfs-local`'s own ext4-semantics tests are gated the same way.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

use norte_core::Engine;
use norte_core::journal::{Actor, SqliteJournal};
use norte_proto::{ConflictKind, Error, TaskState, VPath};
use norte_testkit::corpus::hostile_names;
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

/// One name from the canonical hostile corpus, by id. Every hostile name these
/// tests use comes from there rather than from a literal written here: an
/// adversary each crate invents for itself is an adversary nobody maintains.
fn corpus_name(id: &str) -> Vec<u8> {
    hostile_names()
        .into_iter()
        .find(|n| n.id == id)
        .unwrap_or_else(|| panic!("the corpus has no fixture `{id}`"))
        .bytes
}

/// Everything one of these tests needs.
struct Fixture {
    engine: Engine,
    journal: Arc<SqliteJournal>,
    /// The directory under test. HELD, not leaked: dropping it erases the
    /// directory the assertions read.
    dir: tempfile::TempDir,
    /// Where the journal file lives — deliberately NOT `dir`, because a journal
    /// inside the directory would be one more entry in the listing the planner
    /// sees and one more name every assertion would have to know about.
    _state: tempfile::TempDir,
    /// The provider root, which is `dir` seen through `file://`.
    root: VPath,
}

/// An engine over a real local provider rooted at a fresh temporary directory,
/// seeded with `NAME → CONTENT`, and a file-backed journal.
async fn fixture(seed: &[(Vec<u8>, Vec<u8>)]) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, content) in seed {
        std::fs::write(dir.path().join(OsStr::from_bytes(name)), content).expect("seed a file");
    }
    let state = tempfile::tempdir().expect("tempdir");
    let journal = Arc::new(
        SqliteJournal::open(&state.path().join("journal.db"))
            .await
            .expect("a file-backed journal"),
    );
    let engine = Engine::with_journal(Arc::clone(&journal));
    engine.register_provider(Arc::new(LocalProvider::rooted(dir.path())) as Arc<dyn Provider>);
    Fixture {
        engine,
        journal,
        dir,
        _state: state,
        root: LocalProvider::root(),
    }
}

/// The whole directory as `NAME bytes → CONTENT bytes`, read with `std::fs` and
/// not through the engine: the assertion has to answer what is ON DISK, not
/// what the core believes it put there.
fn on_disk(dir: &Path) -> BTreeMap<Vec<u8>, Vec<u8>> {
    std::fs::read_dir(dir)
        .expect("read_dir")
        .map(|entry| {
            let entry = entry.expect("a directory entry");
            (
                entry.file_name().as_bytes().to_vec(),
                std::fs::read(entry.path()).expect("the file's bytes"),
            )
        })
        .collect()
}

fn seeded(pairs: &[(&[u8], &[u8])]) -> Vec<(Vec<u8>, Vec<u8>)> {
    pairs
        .iter()
        .map(|(n, c)| ((*n).to_vec(), (*c).to_vec()))
        .collect()
}

/// Runs `pairs` inside `root` as one batch and asserts the task completed and
/// the report is clean. Returns the plan, so a caller can assert about the
/// steps the core CHOSE and not only about where the bytes ended up — the two
/// are different questions, and a mutation can get the second right by luck.
async fn run_batch(fx: &Fixture, pairs: &[(Vec<u8>, Vec<u8>)]) -> norte_core::rename::DirPlan {
    let plan = fx
        .engine
        .rename_batch_plan(&fx.root, pairs)
        .await
        .expect("the directory can be planned");
    assert!(
        plan.executable(),
        "the plan must be executable: {:?}",
        plan.plan().collisions
    );
    let steps = u64::try_from(plan.plan().steps.len()).expect("a plan has few steps");
    let (handle, report) = fx
        .engine
        .rename_batch(&fx.root, pairs, plan.hash())
        .await
        .expect("the batch is accepted");
    assert_eq!(handle.join().await, TaskState::Completed);
    let report = report.lock().expect("report lock").clone();
    assert_eq!(report.applied, steps, "every step applied");
    assert_eq!(report.rolled_back, 0, "nothing was rolled back");
    assert!(report.stuck.is_none(), "{:?}", report.stuck);
    assert!(report.uncertain.is_none(), "{:?}", report.uncertain);
    plan
}

/// Undoes the session as the human and asserts the undo went through whole.
///
/// `undone` is passed in and asserted rather than merely checked for absence of
/// failure: an undo with nothing to do reports exactly the same clean report as
/// an undo that reverted a batch, so "nothing went wrong" is not the same claim
/// as "something happened".
async fn undo_everything(fx: &Fixture, undone: u64) {
    let (handle, report) = fx
        .engine
        .undo_session(Actor::User)
        .await
        .expect("the undo is accepted");
    assert_eq!(handle.join().await, TaskState::Completed);
    let report = report.lock().expect("report lock").clone();
    assert!(report.blocked.is_none(), "{:?}", report.blocked);
    assert!(report.batch_stuck.is_none(), "{:?}", report.batch_stuck);
    assert_eq!(report.compensations_lost, 0);
    assert_eq!(
        report.undone, undone,
        "every entry of the batch was reverted"
    );
}

/// A five-cycle over a real directory: every file both gives and receives a
/// name, so nothing can land without the detour through a planner-owned
/// temporary, and two of the five names are ones a filesystem is allowed to
/// hold and a program is likely to get wrong.
///
/// One is not UTF-8 (`lossy_collapse_ff`), and it is on BOTH sides of the
/// detour. The other (`rename_temp_lookalike`) is a file of the user's shaped
/// exactly like this feature's own machinery, `.norte-rename-<8 hex>-<n>` —
/// which is not the name any plan emits, because the tag is a digest of that
/// plan's pairs, but is close enough to catch anything that decides what to
/// skip, hide or sweep by looking at the prefix. It is renamed here like any
/// other file: journalled, undone, and reported.
///
/// This is the case the old per-move loop could never do. `fs.move` one pair at
/// a time refuses the first step — `1.txt`'s destination is occupied — and the
/// batch of renames that "number these episodes correctly" produces is almost
/// always exactly this shape.
#[tokio::test]
async fn a_permutation_with_a_hostile_name_lands_and_undoes_byte_exact() {
    let hostile = corpus_name("lossy_collapse_ff");
    let lookalike = corpus_name("rename_temp_lookalike");
    let seed = seeded(&[
        (b"1.txt", b"the first episode"),
        (b"2.txt", b"the second episode"),
        (b"3.txt", b"the third episode"),
        (&hostile, b"the hostile one"),
        (&lookalike, b"mine, not the tool's"),
    ]);
    let before: BTreeMap<Vec<u8>, Vec<u8>> = seed.iter().cloned().collect();
    let fx = fixture(&seed).await;
    assert_eq!(on_disk(fx.dir.path()), before, "the seed is what it says");

    let pairs = vec![
        (b"1.txt".to_vec(), b"2.txt".to_vec()),
        (b"2.txt".to_vec(), b"3.txt".to_vec()),
        (b"3.txt".to_vec(), hostile.clone()),
        (hostile.clone(), lookalike.clone()),
        (lookalike.clone(), b"1.txt".to_vec()),
    ];
    let plan = run_batch(&fx, &pairs).await;
    let steps = &plan.plan().steps;
    assert_eq!(
        steps.len(),
        6,
        "five renames plus the one detour that opens the cycle: {steps:?}",
    );
    assert_eq!(
        steps.iter().filter(|s| s.temp).count(),
        2,
        "one cycle is opened and closed by the SAME temporary, so two steps \
         carry the flag — parking a file under it and landing it again",
    );

    // Contents, not names: each file is identified by what is inside it. The
    // names are the same FIVE before and after — only the contents under them
    // moved — which is what makes this the assertion the undo one leans on: an
    // undo that did nothing at all cannot satisfy both.
    let after: BTreeMap<Vec<u8>, Vec<u8>> = seeded(&[
        (b"1.txt", b"mine, not the tool's"),
        (b"2.txt", b"the first episode"),
        (b"3.txt", b"the second episode"),
        (&hostile, b"the third episode"),
        (&lookalike, b"the hostile one"),
    ])
    .into_iter()
    .collect();
    assert_ne!(after, before, "a permutation is not the identity");
    assert_eq!(
        on_disk(fx.dir.path()),
        after,
        "the whole permutation landed, and no temporary survived it",
    );

    // One journal batch, and the chain still verifies over a REAL file. The
    // `all` is the load-bearing half: a set of the ids present stays at one
    // even if an entry carries none, and an entry outside the batch is an
    // entry `undo_units` would hand to the undo on its own.
    let entries = fx.journal.journal().entries().await.expect("entries");
    assert_eq!(entries.len(), 6, "one entry per step, temporaries included");
    let batch = entries[0].batch_id.expect("the batch is grouped");
    assert!(
        entries.iter().all(|e| e.batch_id == Some(batch)),
        "six renames, one undoable unit: {:?}",
        entries.iter().map(|e| e.batch_id).collect::<Vec<_>>(),
    );

    undo_everything(&fx, 6).await;
    assert_eq!(
        on_disk(fx.dir.path()),
        before,
        "undo put every byte back under its original name",
    );
    assert!(
        fx.journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
    );
    assert!(
        fx.journal
            .journal()
            .revertible_for(&Actor::User)
            .await
            .expect("revertible")
            .is_empty(),
        "an undone batch leaves nothing for a second undo",
    );
}

/// The corpus's NFD and NFC spellings of `é` are ONE name to the planner's
/// collision equality and TWO FILES to ext4. This test seeds both into a real
/// directory, which is the thing `MemProvider` cannot stage.
///
/// **Forward**, renaming the NFD one has to pick it by BYTES: a planner that
/// resolved a source through the folded key alone would take whichever twin the
/// listing happened to yield first and silently move the wrong file. The NFC
/// neighbour is never named by the batch and comes out untouched. In the same
/// batch, a two-cycle between two names that are not UTF-8 swaps through a
/// planner-owned temporary, so those bytes make the round trip name →
/// temporary → name.
///
/// **Backward, the undo now RUNS WHOLE.** This used to be pinned as a
/// refusal: `undo::feasible` asked whether the NFD name was free through the
/// FOLDED key alone, and the surviving NFC twin appeared to own it, so
/// restoring `plain.txt` read as a conflict against a file the rename never
/// touched, and the whole batch — including the swap, which would have
/// reverted cleanly — was left exactly as it was.
///
/// <https://github.com/compilando/norte/issues/128> changed that: `feasible`
/// now tracks the directory's exact bytes SEPARATELY from its folded keys,
/// the same two-tier shape `rename::plan::Listing` already used on the
/// forward path. A step is blocked only by an occupant that owns the exact
/// bytes of its destination; a fold-only twin does not block, because that is
/// exactly what the executor's own no-clobber compares (`renameat2
/// (RENAME_NOREPLACE)` on local, an exact-path `stat`/`exists` check on sftp
/// and object — see `undo::feasible`'s rustdoc). This test used to pin the
/// refusal; it now pins the restore.
#[tokio::test]
async fn a_twin_directory_moves_the_right_file_and_the_undo_runs_whole() {
    let nfd = corpus_name("nfd_e_acute");
    let nfc = corpus_name("nfc_e_acute");
    let ff = corpus_name("lossy_collapse_ff");
    let fe = corpus_name("lossy_collapse_fe");
    assert_ne!(nfd, nfc, "two spellings, two files");
    assert_ne!(ff, fe, "two hostile names, two files");

    let seed = seeded(&[
        (&nfd, b"decomposed"),
        (&nfc, b"composed"),
        (&ff, b"the ff one"),
        (&fe, b"the fe one"),
    ]);
    let before: BTreeMap<Vec<u8>, Vec<u8>> = seed.iter().cloned().collect();
    let fx = fixture(&seed).await;
    assert_eq!(
        on_disk(fx.dir.path()).len(),
        4,
        "ext4 really does hold both spellings as separate files",
    );

    let pairs = vec![
        (nfd.clone(), b"plain.txt".to_vec()),
        (ff.clone(), fe.clone()),
        (fe.clone(), ff.clone()),
    ];
    let plan = run_batch(&fx, &pairs).await;
    let steps = &plan.plan().steps;
    assert_eq!(
        steps.len(),
        4,
        "one rename plus a swap opened by one detour: {steps:?}",
    );
    // The plan named the NFD twin BYTE for byte. This is the assertion the
    // headline claim rests on: where the bytes ended up cannot separate "picked
    // the right twin" from "picked the first one the directory happened to
    // yield", because on tmpfs that is the same twin.
    assert_eq!(
        steps[0].from, nfd,
        "the source is the twin that was asked for, not its neighbour",
    );

    let after: BTreeMap<Vec<u8>, Vec<u8>> = seeded(&[
        (b"plain.txt", b"decomposed"),
        (&nfc, b"composed"),
        (&ff, b"the fe one"),
        (&fe, b"the ff one"),
    ])
    .into_iter()
    .collect();
    assert_ne!(after, before, "the batch really did something");
    assert_eq!(
        on_disk(fx.dir.path()),
        after,
        "the NFD twin moved, the NFC one did not, and the hostile pair swapped",
    );

    // The undo of THIS batch now runs whole (#128): the surviving NFC twin
    // shares the NFD spelling's folded key but not its bytes, and a fold-only
    // twin no longer blocks.
    let entries_before_undo = fx.journal.journal().entries().await.expect("entries");
    let batch_len = u64::try_from(entries_before_undo.len()).expect("a batch has few entries");
    undo_everything(&fx, batch_len).await;
    assert_eq!(
        on_disk(fx.dir.path()),
        before,
        "the NFD twin came back byte-exact under its own name, the NFC twin \
         was never touched, and the hostile swap reverted too",
    );
    assert!(
        fx.journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
    );
    assert!(
        fx.journal
            .journal()
            .revertible_for(&Actor::User)
            .await
            .expect("revertible")
            .is_empty(),
        "an undone batch leaves nothing for a second undo",
    );
}

/// The narrow claim behind the test above, isolated: a fold-only twin (same
/// [`name_key`](norte_core::rename::plan::name_key), different bytes) does
/// not block the undo that would restore the other twin's own name.
///
/// A single rename, no swap, no hostile bytes — the smallest directory that
/// still has an NFC/NFD pair, so a failure here points straight at
/// `undo::feasible` rather than at plan-batch machinery this test does not
/// exercise.
#[tokio::test]
async fn a_fold_only_twin_does_not_block_the_undo() {
    let nfd = corpus_name("nfd_e_acute");
    let nfc = corpus_name("nfc_e_acute");
    assert_ne!(nfd, nfc, "two spellings, two files");

    let seed = seeded(&[(&nfd, b"decomposed"), (&nfc, b"composed")]);
    let before: BTreeMap<Vec<u8>, Vec<u8>> = seed.iter().cloned().collect();
    let fx = fixture(&seed).await;
    assert_eq!(
        on_disk(fx.dir.path()).len(),
        2,
        "ext4 really does hold both spellings as separate files",
    );

    let pairs = vec![(nfd.clone(), b"plain.txt".to_vec())];
    let plan = run_batch(&fx, &pairs).await;
    assert_eq!(
        plan.plan().steps.len(),
        1,
        "a free destination needs no detour: {:?}",
        plan.plan().steps,
    );

    let after: BTreeMap<Vec<u8>, Vec<u8>> =
        seeded(&[(b"plain.txt", b"decomposed"), (&nfc, b"composed")])
            .into_iter()
            .collect();
    assert_eq!(
        on_disk(fx.dir.path()),
        after,
        "the NFD twin moved, the NFC one did not",
    );

    undo_everything(&fx, 1).await;
    assert_eq!(
        on_disk(fx.dir.path()),
        before,
        "the NFD name came back byte-exact, next to its untouched NFC twin",
    );
}

/// The other direction, unchanged: an occupant that owns the exact BYTES the
/// undo wants still blocks, and still blocks the whole unit.
///
/// Not a twin at all — a plain name that something OTHER than this batch
/// recreated between the batch landing and the undo running, which is the
/// realistic way a byte-exact occupant appears at undo time (the batch itself
/// cannot leave one behind: it plans against a listing and refuses a
/// destination that is already taken).
#[tokio::test]
async fn an_exact_bytes_occupant_still_blocks_the_undo() {
    let seed = seeded(&[(b"orig.txt", b"original content")]);
    let fx = fixture(&seed).await;

    let pairs = vec![(b"orig.txt".to_vec(), b"renamed.txt".to_vec())];
    run_batch(&fx, &pairs).await;
    assert_eq!(
        on_disk(fx.dir.path()),
        seeded(&[(b"renamed.txt", b"original content")])
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
    );

    // Something else claims the vacated name, byte for byte, before the undo
    // runs — the engine never sees this write.
    std::fs::write(
        fx.dir.path().join(OsStr::from_bytes(b"orig.txt")),
        b"someone else's file",
    )
    .expect("plant an unrelated occupant");
    let after_plant: BTreeMap<Vec<u8>, Vec<u8>> = seeded(&[
        (b"renamed.txt", b"original content"),
        (b"orig.txt", b"someone else's file"),
    ])
    .into_iter()
    .collect();
    assert_eq!(on_disk(fx.dir.path()), after_plant);

    let (handle, report) = fx
        .engine
        .undo_session(Actor::User)
        .await
        .expect("the undo is accepted");
    assert_eq!(handle.join().await, TaskState::Completed);
    let report = report.lock().expect("report lock").clone();
    let (_seq, error) = report
        .blocked
        .clone()
        .expect("the exact-bytes occupant blocks the restore");
    assert_eq!(
        error,
        Error::Conflict {
            conflict: ConflictKind::Exists
        },
    );
    assert_eq!(report.undone, 0, "nothing of the batch was reverted");
    assert!(report.batch_stuck.is_none(), "{:?}", report.batch_stuck);
    assert_eq!(
        on_disk(fx.dir.path()),
        after_plant,
        "the occupant was never clobbered, and nothing else moved either",
    );
    assert_eq!(
        fx.journal
            .journal()
            .revertible_for(&Actor::User)
            .await
            .expect("revertible")
            .len(),
        1,
        "a blocked undo stays revertible",
    );
}
