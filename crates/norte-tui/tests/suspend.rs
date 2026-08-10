//! What the three keys of #135 DECIDE, tested without a terminal.
//!
//! The suspension itself — release the mouse, leave the alternate screen, run
//! the child, put everything back — needs a real controlling terminal, and
//! lives as an opt-in test next to `run_suspended` in the binary
//! (`NORTE_TTY_TESTS=1`). What is testable here is everything that decides
//! WHETHER and WITH WHAT to suspend, which is where the bugs that matter live:
//! a shell opened in the wrong directory, an argv with an empty program, a
//! level marker a parent process can poison.

use norte_frontend::shell::{login_shell_from, next_norte_level_from, terminal_candidates_from};

#[test]
fn a_remote_pane_refuses_to_open_a_shell() {
    let p = norte_proto::VPath::parse("sftp://host/x").unwrap();
    assert!(
        norte_vfs_local::vpath_to_native(&p).is_err(),
        "the refusal is this conversion failing; if it ever succeeds, the \
         dispatch arm silently opens a shell in the wrong place"
    );
}

/// The same refusal for the other two non-local kinds a pane can be showing:
/// inside an archive, and an object bucket. A shell has nowhere to sit in
/// either, and `file://` with an authority is somebody else's provider.
#[test]
fn nothing_but_a_plain_local_directory_yields_a_cwd() {
    for wire in [
        "s3://bucket/key",
        "zip+file:///tmp/a.zip/!/inner",
        "file://host/share",
    ] {
        let p = norte_proto::VPath::parse(wire).unwrap_or_else(|e| panic!("{wire}: {e}"));
        assert!(
            norte_vfs_local::vpath_to_native(&p).is_err(),
            "{wire} has no local directory a shell could sit in"
        );
    }
}

#[test]
fn a_local_pane_yields_the_cwd_the_shell_gets() {
    let p = norte_proto::VPath::parse("file:///tmp").unwrap();
    assert_eq!(
        norte_vfs_local::vpath_to_native(&p).unwrap(),
        std::path::PathBuf::from("/tmp")
    );
}

/// A directory whose name is hostile is still a directory: it becomes the
/// child's cwd BYTE-EXACTLY, through `current_dir`, and never through a
/// shell string where the bytes would have to be quoted.
#[cfg(unix)]
#[test]
fn a_hostile_directory_name_survives_into_the_cwd() {
    use std::os::unix::ffi::OsStrExt;
    // A newline, a quote, a `$` and a byte that is not UTF-8 at all.
    let p = norte_proto::VPath::parse("file:///tmp/we%0A%22ird%24%FF").unwrap();
    let native = norte_vfs_local::vpath_to_native(&p).expect("local");
    assert_eq!(native.as_os_str().as_bytes(), b"/tmp/we\n\"ird$\xFF");
}

/// The argv a shell suspension is built from can never start with an empty
/// program: `Command::new("")` is a spawn error several frames from here,
/// with nothing in it that says the environment was the problem.
#[test]
fn the_shell_argv_never_starts_empty() {
    for env in [None, Some("".as_ref()), Some("/bin/zsh".as_ref())] {
        assert!(!login_shell_from(env).as_os_str().is_empty());
    }
}

/// `NORTE_LEVEL` is inherited, so whatever launched norte controls it. It is
/// re-emitted as a small decimal or not at all.
#[test]
fn the_inherited_level_marker_is_never_passed_through() {
    for hostile in ["", "-3", "0x10", "$(id)", "1\n2", "18446744073709551616"] {
        let out = next_norte_level_from(Some(hostile.as_ref()));
        assert!(
            out.chars().all(|c| c.is_ascii_digit()),
            "{hostile:?} produced {out:?}"
        );
    }
}

/// The GUI's list is ordered and every entry names a program to probe. An
/// entry with an empty program would be probed as `""`, which is always
/// missing, so the report would name a terminal that does not exist.
#[test]
fn the_gui_terminal_list_is_probeable() {
    let dir = std::path::Path::new("/tmp");
    let all = terminal_candidates_from(None, dir);
    assert!(!all.is_empty(), "some candidate must exist to report on");
    for argv in &all {
        assert!(!argv.is_empty());
        assert!(!argv[0].is_empty());
    }
}

/// Every name of the canonical hostile corpus, driven through the whole
/// decision path a suspension takes: the pane's directory becomes a child's
/// cwd, and the refusal message goes on a screen.
///
/// Written as a sweep rather than as hand-picked literals (S4 encoding audit,
/// L5) because that is the difference between testing the cases someone
/// thought of and testing the corpus: the earlier version of this file
/// hand-rolled a blend of `control_newline` and `lossy_collapse_ff` and would
/// not have noticed a new fixture.
#[cfg(unix)]
#[test]
fn every_hostile_name_survives_as_a_cwd_and_is_masked_on_screen() {
    use norte_proto::{Segment, VPath};
    use std::os::unix::ffi::OsStrExt;

    let base = VPath::parse("file:///tmp").expect("base");
    for name in norte_testkit::corpus::hostile_names() {
        let Ok(seg) = Segment::new(name.bytes.clone()) else {
            // A segment the protocol itself refuses never reaches a pane.
            continue;
        };
        let dir = base.join(seg);

        // (a) The bytes reach `current_dir` untouched — no lossy hop.
        let native = norte_vfs_local::vpath_to_native(&dir)
            .unwrap_or_else(|e| panic!("{}: local path expected: {e}", name.id));
        let esperado: Vec<u8> = b"/tmp/".iter().copied().chain(name.bytes.clone()).collect();
        assert_eq!(
            native.as_os_str().as_bytes(),
            esperado.as_slice(),
            "{}: the cwd must be the name's bytes, not a decoding of them ({})",
            name.id,
            name.why
        );

        // (b) `child_cwd` hands unix the same path; the refusal branch is
        //     Windows-only and is unit-tested in `norte-frontend`.
        assert_eq!(
            norte_frontend::shell::child_cwd(&native).as_deref(),
            Some(native.as_path()),
            "{}: a unix directory is never refused",
            name.id
        );

        // (c) What a human is shown carries no terminal hazard. This is the
        //     line `msg-shell-remote` interpolates, and it lands on a
        //     terminal norte has already released.
        let (texto, _hostil) = norte_frontend::path_display(&dir);
        assert!(
            !texto.chars().any(norte_encoding::is_terminal_hazard),
            "{}: a hazard reached the status bar: {texto:?} ({})",
            name.id,
            name.why
        );
    }
}
