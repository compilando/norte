//! `norte run '<order>'` (ADR 0185): the box's grammar from a script, with
//! the same matching the panes use.

use assert_cmd::Command;

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
    c.env("NORTE_LANG", "en");
    c
}

fn tree() -> tempfile::TempDir {
    let d = tempfile::tempdir().expect("tempdir");
    std::fs::write(d.path().join("a.pdf"), vec![0u8; 2048]).expect("write");
    std::fs::write(d.path().join("b.pdf"), b"x").expect("write");
    std::fs::write(d.path().join("c.txt"), b"x").expect("write");
    d
}

#[test]
fn dry_run_lists_the_matches_and_touches_nothing() {
    let src = tree();
    let dst = tempfile::tempdir().expect("tempdir");
    let order = format!("sel ext:pdf size>1K | cp >{}", dst.path().display());
    let out = norte()
        .args(["run", "--dry-run", "--cwd"])
        .arg(src.path())
        .arg(&order)
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("a.pdf"), "{text}");
    assert!(!text.contains("b.pdf"), "{text}");
    assert_eq!(std::fs::read_dir(dst.path()).expect("dir").count(), 0);
}

#[test]
fn run_copies_the_matches() {
    let src = tree();
    let dst = tempfile::tempdir().expect("tempdir");
    let order = format!("sel ext:pdf | cp >{}", dst.path().display());
    norte()
        .args(["run", "--cwd"])
        .arg(src.path())
        .arg(&order)
        .assert()
        .success();
    assert!(dst.path().join("a.pdf").exists());
    assert!(dst.path().join("b.pdf").exists());
    assert!(!dst.path().join("c.txt").exists());
}

#[test]
fn other_is_refused_without_a_pane() {
    let src = tree();
    let out = norte()
        .args(["run", "--cwd"])
        .arg(src.path())
        .arg("sel * | cp >other")
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn a_parse_error_says_where_and_exits_2() {
    let out = norte()
        .args(["run", "sel size>12Q"])
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("byte 4"));
}

#[test]
fn zero_matches_runs_nothing() {
    let src = tree();
    let out = norte()
        .args(["run", "--cwd"])
        .arg(src.path())
        .arg("sel ext:zip | trash")
        .output()
        .expect("runs");
    assert!(out.status.success());
    assert_eq!(std::fs::read_dir(src.path()).expect("dir").count(), 3);
}

#[test]
fn json_output_is_stable() {
    let src = tree();
    let out = norte()
        .args(["run", "--dry-run", "--json", "--cwd"])
        .arg(src.path())
        .arg("sel ext:pdf | trash")
        .output()
        .expect("runs");
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(v["action"], "trash");
    let mut names: Vec<String> = v["matched"]
        .as_array()
        .expect("matched")
        .iter()
        .map(|p| {
            p.as_str()
                .expect("wire")
                .rsplit('/')
                .next()
                .expect("name")
                .to_owned()
        })
        .collect();
    names.sort();
    assert_eq!(names, ["a.pdf", "b.pdf"]);
}

/// Like a pane with hidden files off — and like a shell's `*` — a dotfile is
/// not matched unless asked for.
#[test]
fn dotfiles_are_left_out_unless_hidden_is_asked() {
    let src = tree();
    std::fs::write(src.path().join(".env"), b"secret").expect("write");
    let names = |extra: &[&str]| -> Vec<String> {
        let out = norte()
            .args(["run", "--dry-run", "--json", "--cwd"])
            .arg(src.path())
            .args(extra)
            .arg("sel * | trash")
            .output()
            .expect("runs");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
        v["matched"]
            .as_array()
            .expect("matched")
            .iter()
            .map(|p| {
                p.as_str()
                    .expect("wire")
                    .rsplit('/')
                    .next()
                    .expect("name")
                    .to_owned()
            })
            .collect()
    };
    assert!(!names(&[]).contains(&".env".to_owned()));
    assert!(names(&["--hidden"]).contains(&".env".to_owned()));
}

/// A script that parses `--json` gets JSON even when nothing matched.
#[test]
fn zero_matches_still_answer_json() {
    let src = tree();
    let out = norte()
        .args(["run", "--dry-run", "--json", "--cwd"])
        .arg(src.path())
        .arg("sel ext:zip | trash")
        .output()
        .expect("runs");
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(v["matched"].as_array().map(Vec::len), Some(0));
}

/// The glob error quotes the pattern; it reaches stderr masked.
#[test]
fn a_bad_glob_never_reaches_the_terminal_raw() {
    let src = tree();
    let out = norte()
        .args(["run", "--cwd"])
        .arg(src.path())
        .arg(r"sel \x1b]2;pwned\x07\xe2\x80\xae[")
        .output()
        .expect("runs");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains('\u{1b}') && !err.contains('\u{202e}'),
        "{err:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_non_utf8_name_survives_sel_and_cp() {
    use std::os::unix::ffi::OsStrExt;
    let src = tempfile::tempdir().expect("tempdir");
    let name = std::ffi::OsStr::from_bytes(b"caf\xe9.pdf");
    std::fs::write(src.path().join(name), b"x").expect("write");
    let dst = tempfile::tempdir().expect("tempdir");
    norte()
        .args(["run", "--cwd"])
        .arg(src.path())
        .arg(format!(r"sel caf\xe9* | cp >{}", dst.path().display()))
        .assert()
        .success();
    assert!(dst.path().join(name).exists());
}
