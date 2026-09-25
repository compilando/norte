//! `norte compare`: the verdict rides on the EXIT CODE, which is what a
//! script reads without parsing anything.

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

/// Two identical trees: 0, like `diff`.
#[test]
fn two_identical_trees_exit_with_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    std::fs::write(a.join("x.txt"), b"same").expect("write a");
    std::fs::write(b.join("x.txt"), b"same").expect("write b");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .args([
            "compare",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .assert()
        .code(0);
}

/// A file that exists on only one side: 1. It is NOT an error — it is the
/// answer.
#[test]
fn two_different_trees_exit_with_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    std::fs::write(a.join("only-here.txt"), b"x").expect("write a");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .args([
            "compare",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .assert()
        .code(1);
}

/// `--json` comes out untranslated and one line per row, so a script does not
/// have to guess the language of whoever runs it.
#[test]
fn json_carries_no_language() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    std::fs::write(a.join("only-here.txt"), b"x").expect("write a");

    let out = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .env("NORTE_LANG", "es")
        .args([
            "compare",
            "--json",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).expect("utf8");
    let first = text.lines().next().expect("at least one row");
    let row: serde_json::Value = serde_json::from_str(first).expect("json per line");
    assert!(
        row.get("verdict").is_some(),
        "each line is a serialized CompareRow: {first}"
    );
}

/// Hostile corpus (CLAUDE.md: test-first on encoding). `norte compare` prints
/// the name of the OTHER side's tree, which this process does not control: a
/// raw ESC or an RTL override (`control_escape`/`rtl_override` in the corpus)
/// would spoof the output if they arrived unmasked — the check is against the
/// REAL output of `main.rs::compare_cmd`, not against
/// `norte_frontend::display_name` in isolation (that is already tested by
/// `norte-frontend`).
#[test]
#[cfg(unix)]
fn hostile_names_come_out_masked_and_marked() {
    use std::os::unix::ffi::OsStrExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");

    let mut written = 0;
    for id in ["rtl_override", "control_escape"] {
        let name = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("{id} must be in the corpus"));
        let path = a.join(std::ffi::OsStr::from_bytes(&name.bytes));
        // The OS may reject the name; that is not what this test checks (see
        // the doc of `list_lazy_stat_hydrates_corpus_names` in
        // `norte-vfs-local` for the same skip criterion).
        if std::fs::write(&path, b"x").is_ok() {
            written += 1;
        }
    }
    assert!(
        written > 0,
        "at least one hostile fixture must survive this filesystem"
    );

    let out = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .args([
            "compare",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out)
        .expect("the human output is valid UTF-8 — masking makes it so even when the name is not");

    assert!(
        !text.contains('\u{1b}'),
        "ESC must not reach the terminal raw: {text:?}"
    );
    assert!(
        !text.contains('\u{202e}'),
        "the RTL override must not reach the terminal raw: {text:?}"
    );
    for line in text.lines() {
        // `<` verdict (left side only) + `!` confidence (`Certain`: presence
        // is proven) + space + the masking's `!`, which is what this test
        // watches.
        assert!(
            line.starts_with("<! !"),
            "a hostile name comes out MARKED with '!', as in `ai_cmd`: {line:?}"
        );
    }
}

/// `--json` is the WIRE form (percent-encoded, lossless, ADR 0001): not even a
/// NON-UTF8 name gets converted lossily, unlike the human output above.
#[test]
#[cfg(unix)]
fn json_keeps_the_non_utf8_name_losslessly() {
    use std::os::unix::ffi::OsStrExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");

    let name = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "latin1_e_acute")
        .expect("latin1_e_acute must be in the corpus");
    let path = a.join(std::ffi::OsStr::from_bytes(&name.bytes));
    std::fs::write(&path, b"x").expect("a stray byte is a valid name on ext4");

    let out = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .args([
            "compare",
            "--json",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).expect("--json is valid JSON, hence UTF-8");
    let row: serde_json::Value =
        serde_json::from_str(text.lines().next().expect("one row")).expect("json per line");
    let path_wire = row["left"]["path"].as_str().expect("left.path is text");
    // The stray byte 0xE9 percent-encodes to `%E9` (UPPERCASE hex) in the
    // wire form — `vpath_codec::encode_segment`.
    assert!(
        path_wire.contains("%E9"),
        "the non-UTF8 byte must survive percent-encoded on the wire: {path_wire}"
    );
}

/// M1: an UNREADABLE subdirectory is "could not tell", and that answer
/// outranks the row right next to it that does differ. An incomplete
/// comparison that answered 1 ("they differ") would lie by omission just as
/// much as answering 0: what was not read could have been anything.
#[cfg(unix)]
#[test]
fn an_unreadable_subdirectory_exits_with_two() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(a.join("sub")).expect("mkdir a/sub");
    std::fs::create_dir_all(b.join("sub")).expect("mkdir b/sub");
    std::fs::write(a.join("sub/inside.txt"), b"x").expect("write");
    // A genuine difference RIGHT NEXT TO the hole: without it the test could
    // not tell "unknown wins" apart from "there were no more rows".
    std::fs::write(a.join("only-here.txt"), b"x").expect("write");

    let closed = a.join("sub");
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    if std::fs::read_dir(&closed).is_ok() {
        // root ignores the mode: nothing to check here.
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        return;
    }

    let output = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .args([
            "compare",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .output()
        .expect("run");
    // BEFORE the assert: a `TempDir` cannot delete a directory without
    // permissions, and a panic here would leave garbage in /tmp forever.
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    assert_eq!(
        output.status.code(),
        Some(2),
        "an unreadable listing cannot be summed up as either \"equal\" or \"differ\""
    );
}

/// M1: CONFIDENCE decides too. Two sockets of the same name are of the same
/// kind and that is the end of what is known (`Same`/`Unknown`,
/// `cascade.rs`): their content was not compared, so answering 0 — "the
/// trees match" — would assert what nobody looked at.
#[cfg(unix)]
#[test]
fn a_pair_of_sockets_exits_with_two() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    // `bind` creates the socket file; the listener is dropped at the end of
    // the test and the node stays behind, which is exactly what is needed.
    let _left = std::os::unix::net::UnixListener::bind(a.join("s")).expect("socket a");
    let _right = std::os::unix::net::UnixListener::bind(b.join("s")).expect("socket b");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", test_config_dir())
        .args([
            "compare",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .assert()
        .code(2);
}
