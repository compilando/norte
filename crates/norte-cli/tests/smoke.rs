//! Smoke tests for the CLI (`assert_cmd`): the four subcommands against a
//! real tempdir, including SIGINT cancellation (unix).

use assert_cmd::Command;

/// THIS test process's state directory, and never the one running the suite.
///
/// Since #167 an embedded `norte cp`/`mv`/`rm`/`mkdir` opens
/// `<state>/journal.db` on mutation (#177: not before) and keeps its
/// EXCLUSIVE lock while it lives. Without this override that would be the
/// real developer's journal: the suite would write rows into its hash chain
/// and would fight its TUI or its daemon for the lock. Same criterion as the
/// three shell tests (7b0655c) — the subject is the binary, never the
/// configuration of whoever runs it.
///
/// One per process: nextest gives one process per test, so each test ends up
/// with its own. Under `cargo test` (several tests per process) they share
/// it, and the second one to arrive does not wait: it gets `Busy` at 250 ms
/// and keeps going unregistered, which is what this change tolerates by
/// design.
///
/// Lives under `CARGO_TARGET_TMPDIR` and not under `/tmp`: a `static` does
/// not run `Drop`, so the directory survives the process — `cargo clean` and
/// `just prune` sweep it there, nobody sweeps it in `/tmp`.
fn test_config_dir() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        tempfile::TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).expect("state tempdir")
    })
    .path()
}

fn norte() -> Command {
    let mut c = Command::cargo_bin("norte").expect("norte binary compiled");
    c.env("NORTE_CONFIG_DIR", test_config_dir());
    c
}

/// #177: browsing does NOT open — does not even create — the journal, and
/// mutating DOES.
///
/// This is the process-level proof of what #177 fixes: while an embedded
/// frontend only looked, `journal.db` stayed its own and neither `norte
/// daemon run` could start nor `norte audit` could read. The two commands run
/// in the same test on purpose: "it does not open it" only means something if
/// it is shown next to the one that does.
///
/// With its OWN state directory, not the shared one from `test_config_dir`:
/// what is asserted is that a file does NOT exist, and under `cargo test` —
/// one process for the whole suite — another test's `cp` would already have
/// created it.
#[test]
fn ls_does_not_open_the_journal_and_mkdir_does() {
    let state = tempfile::tempdir().unwrap();
    let tree = tempfile::tempdir().unwrap();
    let journal = state.path().join("journal.db");

    let out = Command::cargo_bin("norte")
        .expect("norte binary compiled")
        .env("NORTE_CONFIG_DIR", state.path())
        .arg("ls")
        .arg(tree.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !journal.exists(),
        "listing a directory cannot end up owning the journal (#177)"
    );

    let out = Command::cargo_bin("norte")
        .expect("norte binary compiled")
        .env("NORTE_CONFIG_DIR", state.path())
        .arg("mkdir")
        .arg(tree.path().join("new"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        journal.exists(),
        "creating a directory IS recorded (hard rule 4, #167)"
    );
}

#[test]
fn ls_json_lists_entries() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("one.txt"), b"1").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();

    let out = norte()
        .arg("ls")
        .arg(dir.path())
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let entries = parsed.as_array().expect("array of entries");
    assert_eq!(entries.len(), 2);
    let kinds: Vec<&str> = entries
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"file") && kinds.contains(&"dir"));
    // Paths go out in wire form.
    assert!(
        entries
            .iter()
            .all(|e| e["path"].as_str().unwrap().starts_with("file:///"))
    );
}

/// #52: the local listing is lazy (`size`/`mtime_ms` are `None`); `ls`
/// hydrates with a serial stat before printing — MAJOR-2, restores the
/// pre-#52 output. Covers --json (non-null field) and text (non-empty
/// column).
#[test]
fn ls_hydrates_lazy_size() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("one.txt"), b"content").unwrap();

    let json_out = norte()
        .arg("ls")
        .arg(dir.path())
        .arg("--json")
        .output()
        .unwrap();
    assert!(json_out.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&json_out.stdout).expect("valid JSON");
    let entries = parsed.as_array().expect("array of entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["size"].as_u64(),
        Some(7),
        "hydrated size, not null: {entries:?}"
    );

    let text_out = norte().arg("ls").arg(dir.path()).output().unwrap();
    assert!(text_out.status.success());
    let stdout = String::from_utf8_lossy(&text_out.stdout);
    let line = stdout.lines().next().expect("one line of output");
    let cols: Vec<&str> = line.split('\t').collect();
    assert_eq!(cols.first(), Some(&"-"), "File marker");
    assert_eq!(cols.get(1), Some(&"7"), "non-empty size column: {line}");
}

#[test]
fn ls_missing_dir_fails() {
    let dir = tempfile::tempdir().unwrap();
    let out = norte()
        .arg("ls")
        .arg(dir.path().join("does-not-exist"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not found"), "stderr: {stderr}");
}

#[test]
fn cp_file_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source.bin");
    let dst = dir.path().join("copy.bin");
    let content = vec![0xC5u8; 100_000];
    std::fs::write(&src, &content).unwrap();

    norte().arg("cp").arg(&src).arg(&dst).assert().success();
    assert_eq!(std::fs::read(&dst).unwrap(), content);
    assert_eq!(std::fs::read(&src).unwrap(), content, "the source stays");
}

#[test]
fn cp_collision_refused_and_dest_intact() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a");
    let dst = dir.path().join("b");
    std::fs::write(&src, b"new").unwrap();
    std::fs::write(&dst, b"previous").unwrap();

    let out = norte().arg("cp").arg(&src).arg(&dst).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("conflict"), "stderr: {stderr}");
    assert_eq!(std::fs::read(&dst).unwrap(), b"previous");
}

#[test]
fn cp_dir_recursive() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("tree");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("f1"), b"1").unwrap();
    std::fs::write(src.join("sub/f2"), b"2").unwrap();
    let dst = dir.path().join("copy");

    norte().arg("cp").arg(&src).arg(&dst).assert().success();
    assert_eq!(std::fs::read(dst.join("f1")).unwrap(), b"1");
    assert_eq!(std::fs::read(dst.join("sub/f2")).unwrap(), b"2");
}

#[test]
fn mv_moves_and_rm_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::write(&a, b"x").unwrap();

    norte().arg("mv").arg(&a).arg(&b).assert().success();
    assert!(!a.exists());
    assert_eq!(std::fs::read(&b).unwrap(), b"x");

    norte().arg("rm").arg(&b).assert().success();
    assert!(!b.exists());
}

#[test]
fn rm_recursive_tree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tree");
    std::fs::create_dir_all(root.join("s1/s2")).unwrap();
    std::fs::write(root.join("s1/s2/f"), b"x").unwrap();

    norte().arg("rm").arg(&root).assert().success();
    assert!(!root.exists());
}

#[cfg(unix)]
#[test]
fn cp_sigint_cancels_cleanly() {
    use std::io::Write;
    // Large tree so the copy lasts long enough to signal it.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("large");
    std::fs::create_dir(&src).unwrap();
    let payload = vec![0x42u8; 64 * 1024];
    for i in 0..400 {
        let mut f = std::fs::File::create(src.join(format!("f{i:04}"))).unwrap();
        f.write_all(&payload).unwrap();
    }
    let dst = dir.path().join("copy");

    let bin = assert_cmd::cargo::cargo_bin("norte");
    let mut child = std::process::Command::new(bin)
        // This test does NOT go through `norte()` (it needs `spawn`, not
        // `assert`), so the state-directory override is repeated HERE.
        // Without it the `cp` would open the real journal of whoever runs the
        // suite — see `test_config_dir`.
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .arg("cp")
        .arg(&src)
        .arg(&dst)
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // Waits for the copy to have STARTED (dst appears) before signalling: a
    // fixed sleep was flaky — under load, SIGINT could arrive BEFORE the CLI
    // installed its Ctrl-C handler, killing it by signal (no exit code). Once
    // dst exists, the process booted and the handler is alive.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut started = false;
    loop {
        if dst.exists() {
            started = true;
            break;
        }
        // Did it already finish (400 small files: legitimate race)?
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if started {
        // SIGINT to the process, like a real Ctrl-C.
        // Invariant: the pid of a just-created child is always valid.
        unsafe_free_kill(child.id());
    }
    let status = child.wait().unwrap();

    match status.code() {
        Some(130) => {
            // Cancelled: there may be a partial tree, but NEVER orphaned
            // staging.
            let mut pending = vec![dir.path().to_path_buf()];
            while let Some(d) = pending.pop() {
                for e in std::fs::read_dir(&d).unwrap().flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    assert!(
                        !name.contains(".norte-partial"),
                        "orphaned staging after SIGINT: {name}"
                    );
                    if e.file_type().unwrap().is_dir() {
                        pending.push(e.path());
                    }
                }
            }
        }
        Some(0) => {
            // Legitimate race: the copy finished before the signal.
        }
        other => panic!("unexpected exit code after SIGINT: {other:?}"),
    }
}

/// `kill(pid, SIGINT)` with no dependencies: `/proc` is no good for signals,
/// so the system `kill` command is used (portable on unix).
#[cfg(unix)]
fn unsafe_free_kill(pid: u32) {
    let status = std::process::Command::new("kill")
        .arg("-INT")
        .arg(pid.to_string())
        .status()
        .expect("kill available");
    assert!(status.success(), "kill -INT failed");
}

/// ADR 0104 left it written as a gap: `norte plugin uninstall` deleted from
/// disk behind a live daemon's back, which discovers the catalog on startup
/// and does not watch the directory. ADR 0113: with `--daemon` it goes
/// THROUGH it; without it, it deletes in its own directory and WARNS — and
/// never touches the daemon's, which can be a different one: the default
/// socket does not depend on `NORTE_CONFIG_DIR`.
///
/// One BROKEN plugin is enough: the daemon counts it in `errors` and `plugin
/// list` says so over stderr ("`norte doctor` says why", in both languages).
/// The assertion made BEFORE is what makes the one after mean anything.
#[cfg(unix)]
#[test]
fn plugin_uninstall_goes_through_the_daemon_only_with_daemon() {
    /// The daemon dies with the test, even if an assertion blows up.
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    // Two directories with the SAME plugin: the daemon's and the CLI's.
    let with_broken = || {
        let dir = tempfile::TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).expect("state tempdir");
        let broken = dir.path().join("plugins").join("org.test.broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("plugin.toml"), "this is not a manifest").unwrap();
        (dir, broken)
    };
    let (daemon_dir, daemon_broken) = with_broken();
    let (cli_dir, cli_broken) = with_broken();
    // The socket in `/tmp` and not under the target: `sun_path` has 108
    // bytes.
    let run = tempfile::tempdir().unwrap();
    let socket = run.path().join("d.sock");

    let bin = assert_cmd::cargo::cargo_bin("norte");
    let _daemon = ChildGuard(
        std::process::Command::new(&bin)
            .env("NORTE_CONFIG_DIR", daemon_dir.path())
            .args(["daemon", "run", "--idle-timeout", "0", "--socket"])
            .arg(&socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    // Until it ACCEPTS, not until the file exists: a `--daemon` that does not
    // connect starts another daemon on its own, and the test would measure
    // that one instead.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "the daemon did not accept in 15 s"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let norte_in = |dir: &std::path::Path| {
        let mut c = Command::cargo_bin("norte").expect("norte binary compiled");
        c.env("NORTE_CONFIG_DIR", dir);
        c.arg("--socket").arg(&socket);
        c
    };
    let broken_per_the_daemon = || {
        let out = norte_in(daemon_dir.path())
            .args(["--daemon", "plugin", "list"])
            .assert()
            .success();
        String::from_utf8_lossy(&out.get_output().stderr).contains("doctor")
    };

    assert!(
        broken_per_the_daemon(),
        "the daemon had to count the broken plugin beforehand"
    );

    // Without `--daemon`, from ANOTHER directory: deletes its own, warns, and
    // the daemon's — with the same id — is still there. Before ADR 0113 this
    // command went through whichever daemon was listening and deleted in ITS
    // directory.
    let without = norte_in(cli_dir.path())
        .args(["plugin", "uninstall", "org.test.broken"])
        .assert()
        .success();
    assert!(!cli_broken.exists(), "its own plugin is still on disk");
    assert!(
        daemon_broken.exists(),
        "without --daemon it deleted in the daemon's directory"
    );
    assert!(
        String::from_utf8_lossy(&without.get_output().stderr).contains("--daemon"),
        "warns that the daemon will keep listing it"
    );

    // With `--daemon`: through it, in its directory, and it stops announcing
    // it.
    norte_in(daemon_dir.path())
        .args(["--daemon", "plugin", "uninstall", "org.test.broken"])
        .assert()
        .success();
    assert!(
        !daemon_broken.exists(),
        "the daemon's plugin is still on disk"
    );
    assert!(
        !broken_per_the_daemon(),
        "the daemon is still announcing an uninstalled plugin"
    );

    // And what the daemon no longer has is said against ITS catalog.
    norte_in(cli_dir.path())
        .args(["--daemon", "plugin", "uninstall", "org.test.broken"])
        .assert()
        .failure();
}

/// M3-4 T6: `policy grant` without a daemon running fails CLEANLY (no
/// auto-start — granting a scope to a daemon that does not exist makes no
/// sense). The socket points at a dead path in a tempdir.
#[cfg(unix)]
#[test]
fn policy_grant_without_daemon_fails_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("dead.sock");
    let out = norte()
        .arg("--socket")
        .arg(&socket)
        .arg("policy")
        .arg("grant")
        .arg("1")
        .output()
        .unwrap();
    assert!(!out.status.success(), "without a daemon it must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("daemon") || err.contains("socket"),
        "an informative error, was: {err}"
    );
}

/// `mcp serve --help` lists the session option (the subcommand exists and is
/// coherent without needing a daemon).
#[cfg(unix)]
#[test]
fn mcp_serve_help_mentions_session() {
    let out = norte()
        .arg("mcp")
        .arg("serve")
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("--session"), "help: {help}");
}

// ---------- ls --attrs (#108 block 2) ----------

#[cfg(unix)]
#[test]
fn ls_attrs_posix_via_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), b"hello").expect("seed");
    let out = norte()
        .args(["ls", "--json", "--attrs", "posix.mode"])
        .arg(dir.path())
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("valid json");
    // Block 1's wire form: {"posix.mode": {"uint": N}}.
    let mode = &v[0]["attrs"]["posix.mode"]["uint"];
    assert!(mode.is_u64(), "posix.mode uint present: {v}");

    // And in human form: a `posix.mode=N` column at the end of the line.
    let out = norte()
        .args(["ls", "--attrs", "posix.mode"])
        .arg(dir.path())
        .assert()
        .success();
    let text = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(text.contains("posix.mode="), "human column: {text}");
}

// ---------- embedded semantic index wires the provider (M4-IA-2) ----------

/// Fix follow-up M4-IA-2: `run()`'s embedded path WIRES the embeddings
/// provider. Contrast over two runs of the same binary:
/// (a) without `[ai]` → the engine has no provider: "Unsupported";
/// (b) with `[ai]` + `embed_provider` pointing at a DEAD endpoint → the
///     failure comes from the PROVIDER (already wired), never "Unsupported".
///     The secret goes through env (`NORTE_SECRET_AI_EMB`) so as not to touch
///     the OS keyring in CI.
#[test]
fn index_semantic_embedded_wires_the_provider() {
    // (a) empty config dir: without [ai] there is no provider → Unsupported.
    let empty = tempfile::tempdir().expect("tempdir");
    let out = norte()
        .env("NORTE_CONFIG_DIR", empty.path())
        .args(["index", "semantic", "hello"])
        .output()
        .expect("run");
    assert!(!out.status.success(), "without [ai] it must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_ascii_lowercase().contains("unsupported"),
        "without [ai] the engine has no provider: {stderr}"
    );

    // (b) [ai] + embed_provider pointing at a dead endpoint: the wiring
    // installs the provider and the error is ITS OWN (connection), not
    // Unsupported. HERMETIC port: bind to :0 (the OS picks a free one) and
    // drop immediately — the later connect is a deterministic, fast refusal,
    // without depending on a fixed port being free (or worse, listening) on
    // the CI machine.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    let addr = dead.local_addr().expect("addr");
    drop(dead);
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        cfg.path().join("norte.toml"),
        format!(
            "[ai]\nenabled = true\nembed_provider = \"emb\"\n\n\
             [ai.providers.emb]\nkind = \"ollama\"\nmodel = \"m\"\n\
             base_url = \"http://{addr}\"\n"
        ),
    )
    .expect("norte.toml");
    let out = norte()
        .env("NORTE_CONFIG_DIR", cfg.path())
        .env("NORTE_SECRET_AI_EMB", "x")
        .args(["index", "semantic", "hello"])
        .output()
        .expect("run");
    assert!(!out.status.success(), "a dead endpoint must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.to_ascii_lowercase().contains("unsupported"),
        "with [ai]+embed_provider the provider IS WIRED (the failure is the \
         provider's, not Unsupported): {stderr}"
    );
}

/// `norte paths` answers with the directory that IS IN CHARGE, not the usual
/// one.
///
/// That is the whole point of the command: whoever asks where their config is
/// usually asks precisely because it is not where they thought. A `paths`
/// that ignored `NORTE_CONFIG_DIR` would give the nice, wrong answer, which is
/// worse than having no command. Checked at the PROCESS level because the
/// resolution lives in the environment, which is the one thing a unit test
/// cannot touch.
#[test]
fn paths_respects_the_environments_config_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("connections.toml"), "").unwrap();

    let out = Command::cargo_bin("norte")
        .expect("norte binary compiled")
        .env("NORTE_CONFIG_DIR", dir.path())
        .args(["paths", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let row = |id: &str| {
        json.as_array()
            .expect("list")
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("missing row «{id}»: {json}"))
            .clone()
    };

    let connections = row("connections");
    assert_eq!(
        connections["path"].as_str().expect("path"),
        dir.path().join("connections.toml").display().to_string(),
        "the path must come from NORTE_CONFIG_DIR, not the user's dir"
    );
    // `exists` is a checked fact: the one that was written shows up, the one
    // that was not, does not. Without this the column could be a constant
    // decoration.
    assert_eq!(connections["exists"], serde_json::json!(true));
    assert_eq!(row("policy")["exists"], serde_json::json!(false));
    // Asking creates nothing (same criterion as `doctor`).
    assert!(
        !dir.path().join("policy.toml").exists(),
        "`paths` cannot create what it says is missing"
    );
}

/// `norte daemon run` starts end to end — journal, spool, policy, index,
/// `bind` — and `norte daemon stop` stops it cleanly.
///
/// This is the process test for `norte_core::daemon::compose`: the daemon's
/// composition lived in the CLI (rule 7) and no test exercised it whole. It
/// waits by READING the line that says where it listens, not with a `sleep`:
/// that line comes out after the `bind`, so its arrival is the exact signal.
#[cfg(unix)]
#[test]
fn daemon_run_starts_and_stop_stops_it() {
    use std::io::BufRead as _;
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let socket = config.path().join("d.sock");
    let bin = assert_cmd::cargo::cargo_bin("norte");
    let mut child = std::process::Command::new(&bin)
        .env("NORTE_CONFIG_DIR", config.path())
        .env("XDG_STATE_HOME", state.path())
        .args(["daemon", "run", "--idle-timeout", "0", "--socket"])
        .arg(&socket)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("starts");
    let stderr = child.stderr.take().expect("stderr");
    let (tx, rx) = std::sync::mpsc::channel();
    // Read ALL THE WAY TO THE END, not just to the line being looked for:
    // dropping the pipe earlier would make the daemon's next `eprintln!` fail
    // and bring it down.
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            let _ = tx.send(line);
        }
    });
    let mut seen = Vec::new();
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(l) if l.contains(&socket.display().to_string()) => break,
            Ok(l) => seen.push(l),
            Err(e) => {
                let _ = child.kill();
                panic!("the daemon never got to listen ({e}): {seen:#?}");
            }
        }
    }
    let stop = norte()
        .env("NORTE_CONFIG_DIR", config.path())
        .args(["daemon", "stop", "--socket"])
        .arg(&socket)
        .output()
        .expect("stop");
    assert!(
        stop.status.success(),
        "stop: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    let end = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the daemon exits after the stop")
        .expect("wait");
    assert!(end.success(), "exits cleanly: {end:?}");
}

/// The embedded CLI reads `[archive]` like the daemon and the TUI. Before, it
/// did not even look at it: a `norte ls` inside a zip used the default limits
/// even if `norte.toml` set others. Broken, it WARNS and continues — which is
/// the only thing that, from outside, distinguishes "it read it" from "it
/// never looked".
#[test]
fn a_broken_archive_warns_and_ls_continues() {
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        cfg.path().join("norte.toml"),
        "[archive]\nmax_entries = \"many\"\n",
    )
    .expect("norte.toml");
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), "a").expect("a");
    let out = Command::cargo_bin("norte")
        .expect("norte binary compiled")
        .env("NORTE_CONFIG_DIR", cfg.path())
        .arg("ls")
        .arg(dir.path())
        .output()
        .expect("run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "ls continues: {stderr}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("a.txt"),
        "and it lists"
    );
    assert!(stderr.contains("[archive]"), "warns: {stderr}");
}

/// A fake Ollama on `127.0.0.1:0` that answers ONE `/api/chat` request with
/// `content` as its only delta, then closes.
///
/// Reads the whole request (headers and the body's `Content-Length`) before
/// answering: closing with unread bytes still in the socket makes the kernel
/// send an RST, and the client would see a connection error instead of the
/// response.
fn fake_ollama(content: &str) -> std::net::SocketAddr {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    let addr = listener.local_addr().expect("addr");
    let line = serde_json::json!({ "message": { "content": content } }).to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut length = 0usize;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).expect("header");
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().expect("content-length");
            }
        }
        let mut request = vec![0u8; length];
        reader.read_exact(&mut request).expect("body");
        let body = format!("{line}\n{{\"done\":true}}\n");
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("response");
    });
    addr
}

/// `norte ai rename` applies the plan as ONE batch from the core, not entry by
/// entry (rule 7).
///
/// An `a↔b` swap is the case that tells them apart: the batch planner breaks
/// it with a temp file; a loop of `move_` tries `a → b` with `b` still there
/// and either fails or overwrites `b` before moving it. This is what the TUI
/// and the window already did; the CLI was the only path that did not.
#[test]
fn ai_rename_applies_a_swap_as_one_batch() {
    let plan = r#"[{"from":"a.txt","to":"b.txt"},{"from":"b.txt","to":"a.txt"}]"#;
    let addr = fake_ollama(plan);
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        cfg.path().join("norte.toml"),
        format!(
            "[ai]\nenabled = true\nrename_provider = \"loc\"\n\n\
             [ai.providers.loc]\nkind = \"ollama\"\nmodel = \"m\"\n\
             base_url = \"http://{addr}\"\n"
        ),
    )
    .expect("norte.toml");
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), "was a").expect("a");
    std::fs::write(dir.path().join("b.txt"), "was b").expect("b");

    let out = Command::cargo_bin("norte")
        .expect("norte binary compiled")
        .env("NORTE_CONFIG_DIR", cfg.path())
        .env("NORTE_SECRET_AI_LOC", "x")
        .args(["ai", "rename"])
        .arg(dir.path())
        .args(["swap", "--yes"])
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "the swap is applied: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).expect("a"),
        "was b"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("b.txt")).expect("b"),
        "was a"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("ls").count(),
        2,
        "no temp file is left"
    );
    // One name per line, like the TUI's modal: `a → b` on one line let a file
    // named `x → y` fake the whole pair, on the screen read before answering
    // "yes" (`arrow_join_spoof`).
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().map(str::trim).collect();
    for expected in ["1. a.txt", "→ b.txt", "2. b.txt", "→ a.txt"] {
        assert!(
            lines.contains(&expected),
            "missing line {expected:?}: {stdout}"
        );
    }
}
