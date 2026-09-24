//! `Engine::set_mode` integration (#314): changing POSIX permissions is a
//! MUTATION, with everything that drags along — a journal with a reversal,
//! policy, and a cancelable Task.
//!
//! In-memory `MemProvider` → deterministic, without touching disk. It
//! publishes `posix.mode` and writes it, which is what makes undo checkable.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::journal::Actor;
use norte_core::{Engine, Journal, SqliteJournal, UndoReport};
use norte_proto::methods::FsSetModeParams;
use norte_proto::{Error as ProtoError, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

async fn write_file(mem: &MemProvider, wire: &str) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::from_static(b"x")).await.expect("chunk");
    sink.commit().await.expect("commit");
}

async fn setup() -> (Engine, Arc<MemProvider>, Arc<SqliteJournal>) {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem, journal)
}

/// The mode `p` has RIGHT NOW, read from the attribute the provider publishes.
async fn mode(mem: &MemProvider, wire: &str) -> u32 {
    let req = norte_vfs::AttrRequest::sanitized(vec!["posix.mode".to_owned()]);
    let opt = norte_vfs::ListOptions { attrs: req };
    let e = mem.stat_with(&vp(wire), &opt).await.expect("stat");
    match e
        .attrs
        .get("posix.mode")
        .expect("the provider publishes it")
    {
        norte_proto::AttrValue::Uint(m) => u32::try_from(*m).expect("fits"),
        other => panic!("posix.mode is not a uint: {other:?}"),
    }
}

fn params(paths: &[&str], mode: u32) -> FsSetModeParams {
    FsSetModeParams {
        paths: paths.iter().map(|p| vp(p)).collect(),
        mode,
        recursive: false,
        dir_mode: None,
    }
}

/// The same, recursive and with the directories' mode set apart (#315).
fn recursive_params(paths: &[&str], mode: u32, dir_mode: Option<u32>) -> FsSetModeParams {
    FsSetModeParams {
        recursive: true,
        dir_mode,
        ..params(paths, mode)
    }
}

async fn run_undo(engine: &Engine, actor: Actor) -> (TaskState, UndoReport) {
    let (h, report) = engine.undo_session(actor).await.expect("undo submit");
    let state = h.join().await;
    let r = report.lock().expect("lock").clone();
    (state, r)
}

/// The basics: changes a batch's mode, and the listing shows it.
#[tokio::test]
async fn changes_a_batchs_mode() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///a.sh").await;
    write_file(&mem, "mem:///b.sh").await;

    let h = engine
        .set_mode(params(&["mem:///a.sh", "mem:///b.sh"], 0o755))
        .await
        .expect("launches");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(mode(&mem, "mem:///a.sh").await, 0o755);
    assert_eq!(mode(&mem, "mem:///b.sh").await, 0o755);
}

/// **The reversal is the PREVIOUS mode**, and undoing returns it. Without
/// this, a permission change would be the only norte mutation with no way
/// back, and there is no reason for it to be: the previous twelve bits fit in
/// the journal.
#[tokio::test]
async fn undoing_returns_the_previous_permissions() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///a.sh").await;
    let before = mode(&mem, "mem:///a.sh").await;

    let h = engine
        .set_mode(params(&["mem:///a.sh"], 0o700))
        .await
        .expect("launches");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(mode(&mem, "mem:///a.sh").await, 0o700);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1);
    assert_eq!(
        mode(&mem, "mem:///a.sh").await,
        before,
        "undo returns the mode it had, not a made-up one"
    );
}

/// A batch leaves ONE entry per path, so undoing a batch of three undoes all
/// three — and not "the batch", which does not exist as a thing.
#[tokio::test]
async fn a_batch_leaves_one_entry_per_path() {
    let (engine, mem, _j) = setup().await;
    for n in ["a", "b", "c"] {
        write_file(&mem, &format!("mem:///{n}.sh")).await;
    }
    let h = engine
        .set_mode(params(
            &["mem:///a.sh", "mem:///b.sh", "mem:///c.sh"],
            0o750,
        ))
        .await
        .expect("launches");
    assert_eq!(h.join().await, TaskState::Completed);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 3, "one entry per path, and all three are undone");
    assert_eq!(mode(&mem, "mem:///a.sh").await, 0o644);
    assert_eq!(mode(&mem, "mem:///c.sh").await, 0o644);
}

/// Rule 3: it cancels cleanly and the state says so.
#[tokio::test]
async fn changing_permissions_cancels_and_says_so() {
    let (engine, mem, _j) = setup().await;
    let mut paths = Vec::new();
    for i in 0..400 {
        let wire = format!("mem:///f{i}");
        write_file(&mem, &wire).await;
        paths.push(wire);
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let h = engine
        .set_mode(params(&refs, 0o600))
        .await
        .expect("launches");
    h.cancel();
    assert_eq!(h.join().await, TaskState::Cancelled);
}

/// Without paths there is no request, and it is rejected BEFORE creating any
/// Task: it is a REQUEST error, not the failure of something already launched.
#[tokio::test]
async fn no_paths_creates_no_task() {
    let (engine, _mem, _j) = setup().await;
    let Err(err) = engine.set_mode(params(&[], 0o644)).await else {
        panic!("changing the mode of nothing is not a request");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");
}

/// The bits that are NOT permission bits say what kind the node is, and that
/// is not changed. They are rejected instead of trimmed: trimming would leave
/// a permission nobody asked for, and with the face of having obeyed on top.
#[tokio::test]
async fn a_mode_with_class_bits_is_rejected() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///a.sh").await;
    // 0o100644: regular file + 644. The one above is what is extra.
    let Err(err) = engine.set_mode(params(&["mem:///a.sh"], 0o100_644)).await else {
        panic!("the class bits are not a permission to set");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");
    assert_eq!(
        mode(&mem, "mem:///a.sh").await,
        0o644,
        "and nothing was touched"
    );
}

/// Above the cap it is REJECTED, not trimmed: half a selection with the old
/// permissions and no way to say which one this rejection avoids.
#[tokio::test]
async fn above_the_cap_it_is_rejected() {
    let (engine, _mem, _j) = setup().await;
    let n = norte_proto::methods::FS_SET_MODE_MAX_PATHS + 1;
    let many: Vec<VPath> = (0..n).map(|i| vp(&format!("mem:///f{i}"))).collect();
    let Err(err) = engine
        .set_mode(FsSetModeParams {
            paths: many,
            mode: 0o644,
            recursive: false,
            dir_mode: None,
        })
        .await
    else {
        panic!("above the cap has to be rejected");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");
}

/// **setuid and setgid, only by hand** (ADR 0081).
///
/// Not because those bits are the danger — a `chmod 0777` over `~/.ssh` does
/// much more harm and carries neither — but because they are the ones whoever
/// approves CANNOT SEE: the approval request carries the op and the paths, not
/// the mode. The human does set them, from a dialog that does show them.
#[tokio::test]
async fn setuid_and_setgid_are_not_set_by_an_agent() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///a.sh").await;
    let agent = Actor::Agent {
        session: "s1".into(),
    };
    for special in [0o4755, 0o2755, 0o6755] {
        let Err(err) = engine
            .set_mode_as(params(&["mem:///a.sh"], special), agent.clone())
            .await
        else {
            panic!("{special:o} has to be refused for an agent");
        };
        assert!(matches!(err, ProtoError::PolicyDenied { .. }), "{err:?}");
    }
    assert_eq!(
        mode(&mem, "mem:///a.sh").await,
        0o644,
        "and it touched nothing"
    );

    // The sticky bit (0o1000) does NOT count among those: it grants nobody a
    // privilege, and on a directory it is what makes `/tmp` work.
    let h = engine
        .set_mode_as(params(&["mem:///a.sh"], 0o1755), agent)
        .await
        .expect("sticky is not a privilege bit");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(mode(&mem, "mem:///a.sh").await, 0o1755);
}

/// **A SYMLINK is not touched**, and this is not squeamishness.
///
/// `chmod(2)` FOLLOWS the link while the `stat` used to read the reversal does
/// NOT follow it (lstat, the trait's contract). So the mode saved as "the
/// previous one" would be the LINK's — always `0o777` on Linux — and undoing
/// would leave the DESTINATION wide open. And there is something worse than
/// the reversal: the destination can be outside the scope someone approved, so
/// a chmod on a link is a write that escapes its root.
#[tokio::test]
async fn a_symlink_is_not_touched() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///real.txt").await;
    mem.symlink(
        &vp("mem:///link"),
        b"real.txt",
        norte_vfs::SymlinkKind::File,
    )
    .await
    .expect("link");

    let h = engine
        .set_mode(params(&["mem:///link"], 0o777))
        .await
        .expect("launches");
    let prog = h.progress();
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        mode(&mem, "mem:///real.txt").await,
        0o644,
        "the link's target keeps its permissions"
    );
    // And the progress counts it as not done, which is what the frontend says.
    assert_eq!(prog.borrow().unreadable, Some(1));
}

/// **Recursive: the folder and what is inside it** (#315).
///
/// This is the gap ADR 0081 deliberately postponed: `fs.set_mode` changed
/// EXACTLY the paths given to it, and the three reference managers offer
/// "apply to subfolders" from their properties dialog.
#[tokio::test]
async fn recursive_changes_the_folder_and_what_is_inside_it() {
    let (engine, mem, _j) = setup().await;
    mem.mkdir(&vp("mem:///tree")).await.expect("tree");
    mem.mkdir(&vp("mem:///tree/sub")).await.expect("sub");
    write_file(&mem, "mem:///tree/a.txt").await;
    write_file(&mem, "mem:///tree/sub/b.txt").await;

    let h = engine
        .set_mode(recursive_params(&["mem:///tree"], 0o600, None))
        .await
        .expect("launches");
    assert_eq!(h.join().await, TaskState::Completed);
    for p in [
        "mem:///tree",
        "mem:///tree/sub",
        "mem:///tree/a.txt",
        "mem:///tree/sub/b.txt",
    ] {
        assert_eq!(mode(&mem, p).await, 0o600, "{p}");
    }
}

/// **And the mode of DIRECTORIES can be different**, which is what avoids
/// leaving the tree unusable: `chmod -R 644` strips the execute bit from
/// folders, and a folder without `x` cannot even be entered.
#[tokio::test]
async fn the_mode_of_directories_is_set_apart() {
    let (engine, mem, _j) = setup().await;
    mem.mkdir(&vp("mem:///tree")).await.expect("tree");
    mem.mkdir(&vp("mem:///tree/sub")).await.expect("sub");
    write_file(&mem, "mem:///tree/a.txt").await;

    let h = engine
        .set_mode(recursive_params(&["mem:///tree"], 0o644, Some(0o755)))
        .await
        .expect("launches");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(mode(&mem, "mem:///tree").await, 0o755, "the root is a dir");
    assert_eq!(mode(&mem, "mem:///tree/sub").await, 0o755);
    assert_eq!(mode(&mem, "mem:///tree/a.txt").await, 0o644);
}

/// Without `recursive`, a directory changes ITS OWN mode and nothing else:
/// this is the 0.60 behavior, and what a client that does not send the field
/// expects.
#[tokio::test]
async fn without_recursive_a_directory_does_not_drag_its_content() {
    let (engine, mem, _j) = setup().await;
    mem.mkdir(&vp("mem:///tree")).await.expect("tree");
    write_file(&mem, "mem:///tree/a.txt").await;
    let before = mode(&mem, "mem:///tree/a.txt").await;

    let h = engine
        .set_mode(params(&["mem:///tree"], 0o700))
        .await
        .expect("launches");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(mode(&mem, "mem:///tree").await, 0o700);
    assert_eq!(
        mode(&mem, "mem:///tree/a.txt").await,
        before,
        "what is inside is not touched"
    );
}

/// Every node in the tree leaves its journal entry with its reversal, so undo
/// returns the WHOLE tree to what it was (hard rule 4).
#[tokio::test]
async fn undoing_a_recursive_returns_the_whole_tree() {
    let (engine, mem, _j) = setup().await;
    mem.mkdir(&vp("mem:///tree")).await.expect("tree");
    write_file(&mem, "mem:///tree/a.txt").await;
    write_file(&mem, "mem:///tree/b.txt").await;
    let before: Vec<u32> = {
        let mut v = Vec::new();
        for p in ["mem:///tree", "mem:///tree/a.txt", "mem:///tree/b.txt"] {
            v.push(mode(&mem, p).await);
        }
        v
    };

    let h = engine
        .set_mode(recursive_params(&["mem:///tree"], 0o600, Some(0o700)))
        .await
        .expect("launches");
    assert_eq!(h.join().await, TaskState::Completed);

    let (state, report) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.undone, 3, "the three nodes: {report:?}");
    for (p, m) in ["mem:///tree", "mem:///tree/a.txt", "mem:///tree/b.txt"]
        .into_iter()
        .zip(before)
    {
        assert_eq!(mode(&mem, p).await, m, "{p} went back to what it was");
    }
}

/// A recursive's entries share a BATCH: they were one action by the human, and
/// a hundred thousand entries nobody can put back together read as a hundred
/// thousand actions (#315).
#[tokio::test]
async fn a_recursives_entries_share_a_batch() {
    let (engine, mem, journal) = setup().await;
    mem.mkdir(&vp("mem:///tree")).await.expect("tree");
    write_file(&mem, "mem:///tree/a.txt").await;
    write_file(&mem, "mem:///tree/b.txt").await;

    let h = engine
        .set_mode(recursive_params(&["mem:///tree"], 0o600, None))
        .await
        .expect("launches");
    assert_eq!(h.join().await, TaskState::Completed);

    let entries = journal.journal().entries().await.expect("entries");
    let batches: std::collections::BTreeSet<Option<i64>> =
        entries.iter().map(|e| e.batch_id).collect();
    assert_eq!(entries.len(), 3, "one per node: {entries:?}");
    assert_eq!(
        batches.len(),
        1,
        "and all under the SAME batch: {batches:?}"
    );
    assert!(
        batches.iter().next().expect("one").is_some(),
        "that it exists"
    );
}

/// And without recursive there is NO batch: a lone change is a one-entry
/// action, and giving it a batch id would make undo treat it as a multi-unit.
#[tokio::test]
async fn a_lone_set_mode_carries_no_batch() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///a.sh").await;
    let h = engine
        .set_mode(params(&["mem:///a.sh"], 0o700))
        .await
        .expect("launches");
    assert_eq!(h.join().await, TaskState::Completed);

    let entries = journal.journal().entries().await.expect("entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].batch_id, None);
}

/// `dir_mode` WITHOUT recursive is not applied, which is what the wire
/// promises: without descending the tree there are no directories to apply it
/// to, and using it on the requested paths would give them a permission
/// nobody asked for.
#[tokio::test]
async fn a_dir_mode_without_recursive_is_not_applied() {
    let (engine, mem, _j) = setup().await;
    mem.mkdir(&vp("mem:///tree")).await.expect("tree");

    let h = engine
        .set_mode(FsSetModeParams {
            dir_mode: Some(0o777),
            ..params(&["mem:///tree"], 0o700)
        })
        .await
        .expect("launches");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        mode(&mem, "mem:///tree").await,
        0o700,
        "the requested mode, not the directories' one"
    );
}

/// A LINK inside the tree is not touched either: `chmod(2)` would follow it,
/// and the mode saved as the reversal would be the link's, not the
/// destination's. It is the same rule as outside the tree, and here it is
/// easier to forget.
#[tokio::test]
async fn a_link_inside_the_tree_is_not_touched_either() {
    let (engine, mem, _j) = setup().await;
    mem.mkdir(&vp("mem:///tree")).await.expect("tree");
    write_file(&mem, "mem:///real.txt").await;
    mem.symlink(
        &vp("mem:///tree/link"),
        b"real.txt",
        norte_vfs::SymlinkKind::File,
    )
    .await
    .expect("symlink");

    let h = engine
        .set_mode(recursive_params(&["mem:///tree"], 0o600, None))
        .await
        .expect("launches");
    let prog = h.progress();
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        mode(&mem, "mem:///real.txt").await,
        0o644,
        "the link's target keeps its permissions"
    );
    assert_eq!(prog.borrow().unreadable, Some(1), "and it is counted");
}

/// A path that fails does not bring down the batch: a selection of fifty is
/// not lost over the file that is no longer there.
#[tokio::test]
async fn a_failing_path_does_not_bring_down_the_batch() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///good.sh").await;
    let h = engine
        .set_mode(params(&["mem:///does-not-exist", "mem:///good.sh"], 0o700))
        .await
        .expect("launches");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(mode(&mem, "mem:///good.sh").await, 0o700);
}
