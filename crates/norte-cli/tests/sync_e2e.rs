//! `norte sync`: the plan is printed IN FULL before there is a question, and
//! `--dry-run` is the plan without the apply.

use assert_cmd::Command;

/// THIS test process's state directory, and never the one running the suite
/// (same criterion as `smoke.rs::test_config_dir`).
fn test_config_dir() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        tempfile::TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).expect("state tempdir")
    })
    .path()
}

/// `--dry-run` shows what it would do and does NOT touch the destination.
#[test]
fn dry_run_shows_the_plan_and_does_not_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("new.txt"), b"content").expect("write");

    let assert = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .args([
            "sync",
            "--mode",
            "update",
            "--dry-run",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(1);

    let out = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8");
    assert!(out.contains("new.txt"), "the plan names the file: {out}");

    assert!(
        !dst.join("new.txt").exists(),
        "--dry-run does not write to the destination"
    );
}

/// Nothing to do: 0, and no plan to show.
#[test]
fn no_differences_exits_with_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("same.txt"), b"x").expect("write src");
    std::fs::write(dst.join("same.txt"), b"x").expect("write dst");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .args([
            "sync",
            "--mode",
            "update",
            "--dry-run",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(0);
}

/// `--yes` applies, and the destination ends up with what the plan promised.
#[test]
fn with_yes_it_applies_and_the_destination_gets_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("new.txt"), b"content").expect("write");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .args([
            "sync",
            "--mode",
            "update",
            "--yes",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(1);

    assert_eq!(
        std::fs::read(dst.join("new.txt")).expect("the destination received the file"),
        b"content"
    );
}

/// Without a terminal there is NO question to ask, so none is asked: it
/// refuses BEFORE, with the "nothing happened" code (2) and pointing at
/// `--yes`.
///
/// The two codes that matter are the ones it CANNOT return. `0` would say
/// "the trees are already in sync" — which is what a `norte sync src dst &&
/// echo ok` in a cron job would read — having written nothing; and `1` would
/// say "it was resolved". An empty answer, an EOF and a closed stdin are the
/// same fact: nobody consented.
#[test]
fn without_a_terminal_it_asks_nothing_and_applies_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("new.txt"), b"content").expect("write");

    let assert = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .args([
            "sync",
            "--mode",
            "update",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .write_stdin("\n")
        .assert()
        .code(2);

    let err = String::from_utf8(assert.get_output().stderr.clone()).expect("utf8");
    assert!(
        err.contains("--yes"),
        "the message has to say what the remedy is: {err}"
    );
    assert!(
        !dst.join("new.txt").exists(),
        "without consent nothing is applied"
    );
}

/// A BLOCKED plan is not "nothing to do".
///
/// `src/x` is a DIRECTORY and `dst/x` a FILE: the transducer blocks
/// (`TypeMismatchDir`) instead of turning a file into a tree, and a blocked
/// plan comes with `executable: false` and — by wire invariant — WITHOUT
/// steps. Reading that empty list as "the trees already match" and answering
/// 0 is exactly the failure the third code exists to avoid.
#[test]
fn a_blocked_plan_does_not_say_there_is_nothing_to_do() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(src.join("x")).expect("mkdir src/x");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("x/inside.txt"), b"a").expect("write");
    std::fs::write(dst.join("x"), b"i am a file").expect("write");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .args([
            "sync",
            "--mode",
            "update",
            "--yes",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(2);

    assert_eq!(
        std::fs::read(dst.join("x")).expect("the file is still there"),
        b"i am a file",
        "a blocked plan does not write"
    );
}

/// The CLI UNMOUNTS its spool on exit, through every path.
///
/// The daemon sweeps on startup and drops each connection when it closes it;
/// the CLI has neither, so a `--dry-run` — which applies nothing — would
/// leave in the state directory a file naming BOTH trees that nobody would
/// pick up. Its own state directory: this is where the spool is LOOKED AT,
/// and the shared one may be in use by another test.
#[test]
fn dry_run_leaves_no_spool_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("mkdir state");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("new.txt"), b"content").expect("write");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", &state)
        .args([
            "sync",
            "--mode",
            "update",
            "--dry-run",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(1);

    let left: Vec<String> = std::fs::read_dir(state.join("sync-spools"))
        .map(|it| {
            it.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        left.is_empty(),
        "the spool has to be empty on exit: {left:?}"
    );
}

/// Encoding audit from the C2 branch review, MAJOR-2: the plan's row joined
/// its fields IN-BAND, on the screen where `y` is typed.
///
/// `→` and `  (…)` are ordinary printables that `display_name_with` does not
/// mask, so a name carrying them inside arrives WITHOUT `rel_marked`'s `!` and
/// fakes a whole row: `a → mem_b.txt` (corpus `arrow_join_spoof`) simulates a
/// source→destination pair that does not exist. What is checked here is what
/// closes that — one field per LINE —, because a newline IS a separator a
/// name cannot forge: `\n` is Cc and `is_terminal_hazard` masks it to
/// `U+FFFD`.
///
/// This file had NO hostile-name test at all, and that absence is why the
/// CLI fell behind when the GUI and the TUI were fixed.
#[test]
fn a_name_with_an_arrow_does_not_fake_a_pair_in_the_plan() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    // The corpus's name, as is: legal on ext4 and APFS.
    let hostile = "a \u{2192} mem_b.txt";
    std::fs::write(src.join(hostile), b"content").expect("write");

    let assert = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .args([
            "sync",
            "--mode",
            "update",
            "--dry-run",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(1);

    let out = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8");
    let row = out
        .lines()
        .find(|l| l.contains("mem_b.txt"))
        .unwrap_or_else(|| panic!("the plan names the file: {out}"));

    // The whole name is on ONE line, arrow included: that is the name, not a
    // pair. What must not exist is a SECOND spelling on that same line, which
    // is what the in-band ` → ` used to manufacture.
    assert!(row.contains(hostile), "the name goes whole: {row:?}");
    assert_eq!(
        row.matches('\u{2192}').count(),
        1,
        "a single arrow, the NAME's: {row:?}"
    );
    // And the destination's spelling, when there is one, goes on its own line
    // with its own label — never glued to the name.
    assert!(
        !row.contains("  ("),
        "the reason is not joined in-band either: {row:?}"
    );
}

// ---------- Ctrl+C during `norte sync` (#180, #187) ----------

/// `kill(pid, SIGINT)` with no dependencies: the same helper as
/// `smoke.rs::unsafe_free_kill`, duplicated on purpose — each test file is its
/// own binary and there is no support crate shared between them (same
/// criterion as the duplication of `test_config_dir`).
#[cfg(unix)]
fn unsafe_free_kill(pid: u32) {
    let status = std::process::Command::new("kill")
        .arg("-INT")
        .arg(pid.to_string())
        .status()
        .expect("kill available");
    assert!(status.success(), "kill -INT failed");
}

/// #180: a Ctrl+C DURING planning must not leave an orphaned spool `.part`.
///
/// Before the fix, `sync_plan_show_apply` drained the `sync.plan` stream in a
/// `while let` with no Ctrl+C handler: the SIGINT killed the WHOLE process via
/// the OS's default behaviour, without giving `run_sync_plan` a chance to see
/// its `CancellationToken`, close the spool and remove the `.part`
/// (`SpoolWriter::finish`/`Drop`). The TTL only sweeps CLOSED plans, so that
/// file stayed forever.
///
/// A tree with many entries (not bytes: what makes PLANNING slow is listing
/// and comparing rows, not writing content — that is the apply) gives enough
/// time to check that the `.part` exists before signalling. If planning ends
/// before that is detected — a legitimate race, the same one
/// `cp_sigint_cancels_cleanly` tolerates in `smoke.rs` — the assertion below
/// still holds trivially: nothing is left in the spool.
#[cfg(unix)]
#[test]
fn sigint_during_planning_leaves_no_part_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("mkdir state");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    for i in 0..20_000 {
        std::fs::write(src.join(format!("f{i:05}")), b"").expect("write");
    }

    let bin = assert_cmd::cargo::cargo_bin("norte");
    let spool_dir = state.join("sync-spools");

    // The cooperative handler is ARMED in a worker of the child, and seeing
    // it write the `.part` does not prove that worker already had its turn:
    // under load — this suite runs with one process per core — the SIGINT can
    // arrive while the OS default is still in effect, which kills the process
    // raw and skips the `Drop` that removes the `.part`.
    //
    // The 50 ms floor that used to be here was a CLOCK GUESS, and under `just
    // ci-fast` it loses: red across the whole suite, green in isolation. This
    // makes it causal instead — the process's death SAYS which of the two
    // cases it was (exit code = cooperative, raw signal = the handler was not
    // there yet) — and only retries the case that proved nothing. Three
    // attempts without a single cooperative death is not noise: it is a
    // handler that never arms, and then the test fails saying so.
    let mut raw_deaths = 0;
    for attempt in 1..=3 {
        let mut child = std::process::Command::new(&bin)
            .env("NORTE_CONFIG_DIR", &state)
            .env("NORTE_LANG", "en")
            .arg("sync")
            .arg("--mode")
            .arg("update")
            .arg("--yes")
            .arg(&src)
            .arg(&dst)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn norte sync");

        // Waits for the `.part` to exist: planning started and the spool was
        // created, which is what this test needs there to be to clean up.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut started = false;
        loop {
            if child.try_wait().expect("try_wait").is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            let has_part = std::fs::read_dir(&spool_dir).is_ok_and(|it| {
                it.flatten()
                    .any(|e| e.file_name().to_string_lossy().ends_with(".part"))
            });
            if has_part {
                started = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let alive = child.try_wait().expect("try_wait").is_none();
        assert!(
            started,
            "planning did not get to write a `.part` in 15 s (attempt {attempt}; \
             is the child still alive? {alive}; in the spool: {:?}): there was nothing to cancel",
            std::fs::read_dir(&spool_dir).map(|it| it
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>())
        );
        unsafe_free_kill(child.id());
        let status = child.wait().expect("wait");

        // RAW death: the OS default SIGINT beat the handler. The `Drop` did
        // not run because it could not run — that is not what this test
        // asserts, so it cleans up and retries.
        if status.code().is_none() {
            raw_deaths += 1;
            for e in std::fs::read_dir(&spool_dir)
                .into_iter()
                .flatten()
                .flatten()
            {
                let _ = std::fs::remove_file(e.path());
            }
            continue;
        }

        let left: Vec<String> = std::fs::read_dir(&spool_dir)
            .map(|it| {
                it.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            left.is_empty(),
            "a Ctrl+C during planning must not leave anything in the spool: {left:?} \
             (attempt {attempt}, exited with {:?}, raw deaths so far: {raw_deaths})",
            status.code()
        );
        return;
    }
    panic!(
        "three attempts and all three died by RAW signal ({raw_deaths}): the cooperative handler never arms"
    );
}

/// #187: a CANCELLED `norte sync` reaches its report, and the exit code stays
/// at 2 — never the 0 of `run_task` (which for `sync.apply` would be a lie:
/// the application was cut short) nor a "clean destination" message, which is
/// false for a `Mirror` cut halfway through (what was applied before the cut
/// stays, journaled, hard rule 4).
///
/// A tree that is large in BYTES (not just entry count) so the apply
/// phase — which does write — lasts long enough to be signalled after the
/// plan has already been approved with `--yes`.
#[cfg(unix)]
#[test]
fn sigint_during_apply_asks_for_the_report_and_does_not_say_clean_destination() {
    use std::io::Write as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("mkdir state");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    let payload = vec![0x42u8; 64 * 1024];
    for i in 0..400 {
        let mut f = std::fs::File::create(src.join(format!("f{i:04}"))).expect("create");
        f.write_all(&payload).expect("write");
    }

    let bin = assert_cmd::cargo::cargo_bin("norte");
    let mut child = std::process::Command::new(bin)
        .env("NORTE_CONFIG_DIR", &state)
        .env("NORTE_LANG", "en")
        .arg("sync")
        .arg("--mode")
        .arg("update")
        .arg("--yes")
        .arg(&src)
        .arg(&dst)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn norte sync");

    // #201: the two pipes are DRAINED on threads, right away. Without this the
    // test could silently defeat itself: a plan that does not fit the pipe's
    // buffer (64 KiB on Linux) blocks the child BEFORE applying, `dst` never
    // fills, no signal is ever sent and the whole run exits through the race's
    // arm without having proven anything. Today's fixture fits; raising it
    // crossed that threshold without a word.
    //
    // Its twin `sigint_after_planning_ends_the_process` depends on exactly
    // that block to reach ITS window. Same mechanism: here it gets in the
    // way, there it is the subject. Whoever touches one should read both.
    let mut child_out = child.stdout.take().expect("stdout piped");
    let mut child_err = child.stderr.take().expect("stderr piped");
    let out_drain = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut child_out, &mut buf);
        buf
    });
    let err_drain = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut child_err, &mut buf);
        buf
    });

    // The apply STARTED as soon as the destination receives its first entry:
    // planning writes nothing there. The floor is the same caution as
    // `sigint_during_planning_leaves_no_part_behind`: seeing files in `dst`
    // does not prove `watch_ctrl_c` already got its first `poll` in the child
    // process, so under load a `kill` sent too soon can hit the OS default
    // SIGINT instead of the cooperative handler.
    let start = std::time::Instant::now();
    let floor = std::time::Duration::from_millis(50);
    let deadline = start + std::time::Duration::from_secs(15);
    let mut started = false;
    loop {
        if child.try_wait().expect("try_wait").is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        let has_something = std::fs::read_dir(&dst).is_ok_and(|it| it.flatten().next().is_some());
        if has_something && start.elapsed() >= floor {
            started = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // #201: that the signal WAS SENT is the test's premise, not a lucky
    // coincidence. Without this, any run where the apply never started would
    // pass without exercising the fix.
    assert!(
        started,
        "the apply never got to write to {} in 15 s: the signal was never sent and this test proved nothing",
        dst.display()
    );
    unsafe_free_kill(child.id());
    let status = child.wait().expect("wait");
    let stdout = out_drain.join().expect("stdout drain");
    let stderr = err_drain.join().expect("stderr drain");

    match status.code() {
        Some(2) => {
            let out = String::from_utf8_lossy(&stdout);
            let err = String::from_utf8_lossy(&stderr);
            assert!(
                out.contains("applied:"),
                "a cancelled application DOES have a report: stdout={out}"
            );
            assert!(
                !err.contains("destination clean"),
                "what was applied before the cut is NOT a clean destination: stderr={err}"
            );
        }
        // Legitimate race that IS ALSO CHECKED: the apply finished before the
        // signal arrived. Then it finished COMPLETELY — a `0` with half the
        // files would be an application lying about its outcome, and this arm
        // was where that used to slip through unnoticed.
        Some(0) => {
            let copied = std::fs::read_dir(&dst).expect("read dst").count();
            assert_eq!(
                copied, 400,
                "exited 0 (complete) with {copied} of 400 files in the destination"
            );
        }
        Some(1) => {
            let err = String::from_utf8_lossy(&stderr);
            assert!(
                !err.contains("destination clean"),
                "a failure does not leave a \"clean destination\" either: stderr={err}"
            );
        }
        other => panic!("unexpected exit code after SIGINT: {other:?}"),
    }
}

/// **BLOCKER from the W2 branch review.** `Ctrl+C` AFTER planning.
///
/// Registering `ctrl_c()` in tokio is PROCESS-wide and permanent: tokio's docs
/// say dropping the `Signal` does not restore the default behaviour. With one
/// watcher per phase, the `abort()` at the end of planning killed the
/// listener and left the registration in place — so from then on tokio
/// consumed the signal, nobody handled it, and `Ctrl+C` did NOTHING for the
/// whole window from the end of the plan to the start of the apply: printing
/// the plan, the blockers, the `[y/N]` prompt and `--dry-run`'s output.
///
/// The window is provoked here with `--dry-run` over a large tree and a
/// `stdout` that NOBODY drains: the child blocks writing the plan against a
/// full pipe, which is exactly the state "planning finished, nothing alive to
/// cancel". Before the fix the process stayed there forever.
///
/// The real prompt cannot be tested without a PTY — this CLI does not ask
/// without a terminal, and there is a test that pins that — so the SAME
/// window is tested from the side that IS reachable. The other two signal
/// tests do not see it: both pass `--yes`.
#[test]
fn sigint_after_planning_ends_the_process() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("mkdir state");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    // Enough files that the printed plan does NOT fit the pipe's buffer
    // (64 KiB on Linux): that way the child stays blocked writing it.
    for i in 0..20_000 {
        std::fs::write(src.join(format!("f{i:05}")), b"").expect("write");
    }

    let bin = assert_cmd::cargo::cargo_bin("norte");
    let mut child = std::process::Command::new(bin)
        .env("NORTE_CONFIG_DIR", &state)
        .env("NORTE_LANG", "en")
        .arg("sync")
        .arg("--mode")
        .arg("update")
        .arg("--dry-run")
        .arg(&src)
        .arg(&dst)
        // Piped and NEVER read: the pipe fills and the child blocks there.
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn norte sync");

    // Floor before signalling, for the same reason as the other two: the
    // handler lives in a different worker than the one planning. Here it also
    // has to let planning FINISH and the child get stuck writing.
    std::thread::sleep(std::time::Duration::from_millis(2500));

    // The same helper as the other two: `kill -INT` by pid, without adding
    // `libc` as a dependency just for one test (rule 8).
    unsafe_free_kill(child.id());

    // And it HAS to die. Before the fix it stayed blocked forever: tokio
    // consumed the signal and nobody was listening.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                assert!(
                    !status.success(),
                    "a sync interrupted after planning does not exit successfully: {status:?}"
                );
                return;
            }
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            None => {
                let _ = child.kill();
                panic!("Ctrl+C after planning did nothing: the process is still alive");
            }
        }
    }
}
