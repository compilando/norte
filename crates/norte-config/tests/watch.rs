//! Integration tests for the `watch` feature (ADR 0007): polling detects
//! changes and cleans up on drop (rule 3), and the polling snapshot covers
//! every layer file — including `openers.toml`.

use norte_config::{Layer, Layers, watch_polling};

fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().expect("tempdir");
    for (name, content) in files {
        std::fs::write(d.path().join(name), content).expect("write");
    }
    d
}

/// Rule 3 for the watcher's poll: it detects changes and dropping the
/// `Watch` CANCELS it cleanly (the task drops its sender → the channel
/// closes).
#[tokio::test]
async fn polling_detects_changes_and_cancels_cleanly() {
    let d = dir_with(&[("norte.toml", "[keymap]\npreset = \"vim\"\n")]);
    let layers = Layers {
        dirs: vec![(d.path().to_path_buf(), Layer::User)],
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let watch = watch_polling(&layers, tx, std::time::Duration::from_millis(20));

    // The poll takes its base snapshot in its own task, so there is no
    // "you can change the file now" to wait for: sleeping a multiple of the
    // period was betting that the base was taken before the write, and it
    // was lost under load (wave W9, task 2.2). Instead, a distinct change
    // (mtime AND size) is written on every round until a tick sees it: if
    // the base landed after the first write, the second one gives it away.
    let mut seen = None;
    for i in 0..15u32 {
        let mut content = String::from("[keymap]\npreset = \"orthodox\"\n");
        for _ in 0..i {
            content.push_str("# round\n");
        }
        std::fs::write(d.path().join("norte.toml"), content).unwrap();
        if let Ok(Some(())) =
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
        {
            seen = Some(());
            break;
        }
    }
    assert!(seen.is_some(), "the poll sees the change");

    drop(watch);
    // After cancelling, the task ends and drops the sender: recv → None
    // (draining any events left in flight).
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await {
            Ok(Some(())) => {}
            Ok(None) => break,
            Err(timeout) => {
                panic!("the polling task did not end after dropping the Watch: {timeout}")
            }
        }
    }
}

/// Pin (TDD): the fix applied when `snapshot` was copied to this crate added
/// `"openers.toml"` to the array of files the polling fallback watches —
/// before, it only covered `norte.toml`/`keymap.toml`, so an edit to
/// `openers.toml` under pure polling (native watcher down) was missed.
/// Verified by TDD: removing `"openers.toml"` from `snapshot`'s array in
/// `src/watch.rs` makes this test fail (timeout, no tick arrives); restoring
/// it passes it again.
#[tokio::test]
async fn polling_detects_changes_in_openers_toml() {
    let d = dir_with(&[(
        "openers.toml",
        "[[opener]]\nmime = \"text/*\"\ncommand = [\"less\", \"%f\"]\n",
    )]);
    let layers = Layers {
        dirs: vec![(d.path().to_path_buf(), Layer::User)],
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let watch = watch_polling(&layers, tx, std::time::Duration::from_millis(20));

    // The poll takes its base snapshot in its own task, so there is no
    // "you can change the file now" to wait for: sleeping a multiple of the
    // period was betting that the base was taken before the write, and it
    // was lost under load (wave W9, task 2.2). Instead, a distinct change
    // (mtime AND size) is written on every round until a tick sees it: if
    // the base landed after the first write, the second one gives it away.
    let mut seen = None;
    for i in 0..15u32 {
        let mut content =
            String::from("[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n");
        for _ in 0..i {
            content.push_str("# round\n");
        }
        std::fs::write(d.path().join("openers.toml"), content).unwrap();
        if let Ok(Some(())) =
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
        {
            seen = Some(());
            break;
        }
    }
    assert!(seen.is_some(), "the poll sees the openers.toml change");

    drop(watch);
}

/// The NATIVE watcher watches the WHOLE DIRECTORY (`RecursiveMode::
/// NonRecursive`), so any neighboring file triggered a full hot-reload. The
/// user's config dir does not hold only TOMLs: `index.db` (semantic index)
/// and `journal.db`/`journal.db-shm` live there too, which `SQLite` writes
/// while the app runs — and every write reloaded the config, closing the
/// open help (F1) and palette and painting "config reloaded". Only the three
/// TOML layers count, same as in the polling snapshot.
#[tokio::test]
async fn watcher_ignores_neighbor_files_in_the_config_dir() {
    let d = dir_with(&[("norte.toml", "[keymap]\npreset = \"vim\"\n")]);
    let layers = Layers {
        dirs: vec![(d.path().to_path_buf(), Layer::User)],
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let watch = norte_config::watch(&layers, tx).await;

    // Any neighbor (what SQLite does with index.db/journal.db-shm): not even
    // a tick. The window is deliberately short — Notify mode's fallback poll
    // takes 10s, and under pure polling this file is not in the snapshot
    // either, so the assertion holds in both modes.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    std::fs::write(d.path().join("index.db"), b"sqlite-ish").unwrap();
    // And READING the layers does not either: the reload itself rereads
    // them, so an `Access(Open)` that counted as a change would feed back
    // into the cycle (reload → open norte.toml → reload…).
    let _ = std::fs::read(d.path().join("norte.toml")).unwrap();
    let seen = tokio::time::timeout(std::time::Duration::from_millis(700), rx.recv()).await;
    assert!(
        seen.is_err(),
        "neither a dir neighbor nor a READ reloads the config: {seen:?}"
    );

    // And the real layer does still trigger it.
    std::fs::write(
        d.path().join("norte.toml"),
        "[keymap]\npreset = \"orthodox\"\n",
    )
    .unwrap();
    let seen = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await;
    assert!(
        matches!(seen, Ok(Some(()))),
        "a real edit to norte.toml does reload: {seen:?}"
    );

    drop(watch);
}
