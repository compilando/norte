//! The wrappers are shell code, so they are tested by running the shell.
//! A stub `ntc` on PATH stands in for the binary: it writes hostile bytes to
//! the cd-file, which is exactly the contract the wrapper has to survive.
//!
//! Three separate tests, not a loop over the three shells: a loop reports
//! only the first failure and hides the rest, and these three fail for
//! different reasons (bash/zsh share the wrapper text but not the binary;
//! fish's is entirely different syntax). bash is expected to exist on any
//! CI/dev box this suite runs on; zsh and fish print a skip line and return
//! early when absent, in the style of the repo's existing wasm/MinIO skips —
//! never silently.

use std::io::Write;
use std::process::Command;

fn shell_available(sh: &str) -> bool {
    Command::new(sh)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Builds a temp dir holding: a stub `ntc` that writes `<target>\0` to the
/// path given by `--cd-file`, and the target directory itself.
fn stub_env(tmp: &std::path::Path, target: &str) {
    let bin = tmp.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("ntc");
    let mut f = std::fs::File::create(&stub).unwrap();
    // `$2` is the cd-file: the wrapper always passes `--cd-file <path>` first.
    writeln!(f, "#!/bin/sh\nprintf '%s\\0' \"{target}\" > \"$2\"").unwrap();
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::create_dir_all(target).unwrap();
}

#[test]
fn bash_wrapper_lands_in_a_directory_whose_name_is_hostile() {
    if !shell_available("bash") {
        eprintln!("skip: no bash on this machine");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // A space and a newline: `$(...)` would destroy the second one.
    let target = tmp.path().join("we ird\ndir");
    stub_env(tmp.path(), target.to_str().unwrap());
    let script = format!("{}\nntc\npwd", norte_frontend::shell::Shell::Bash.wrapper());
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env(
            "PATH",
            format!(
                "{}/bin:{}",
                tmp.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .output()
        .unwrap();
    let pwd = String::from_utf8_lossy(&out.stdout);
    assert!(
        pwd.trim_end().ends_with("dir"),
        "the wrapper did not cd: {pwd:?} / {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn zsh_wrapper_lands_in_a_directory_whose_name_is_hostile() {
    if !shell_available("zsh") {
        eprintln!("skip: no zsh on this machine");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("we ird\ndir");
    stub_env(tmp.path(), target.to_str().unwrap());
    let script = format!("{}\nntc\npwd", norte_frontend::shell::Shell::Zsh.wrapper());
    let out = Command::new("zsh")
        .arg("-c")
        .arg(&script)
        .env(
            "PATH",
            format!(
                "{}/bin:{}",
                tmp.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .output()
        .unwrap();
    let pwd = String::from_utf8_lossy(&out.stdout);
    assert!(
        pwd.trim_end().ends_with("dir"),
        "the wrapper did not cd: {pwd:?} / {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn fish_wrapper_lands_in_a_directory_whose_name_is_hostile() {
    if !shell_available("fish") {
        eprintln!("skip: no fish on this machine");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("we ird\ndir");
    stub_env(tmp.path(), target.to_str().unwrap());
    let script = format!("{}\nntc\npwd", norte_frontend::shell::Shell::Fish.wrapper());
    let out = Command::new("fish")
        .arg("-c")
        .arg(&script)
        .env(
            "PATH",
            format!(
                "{}/bin:{}",
                tmp.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .output()
        .unwrap();
    let pwd = String::from_utf8_lossy(&out.stdout);
    assert!(
        pwd.trim_end().ends_with("dir"),
        "the wrapper did not cd: {pwd:?} / {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
