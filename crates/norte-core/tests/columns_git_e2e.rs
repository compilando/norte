//! The official columns plugin, INSTALLED the way a third party's would be,
//! against a real git repository.
//!
//! That this path works is what is being tested: embedding the `.wasm` in the
//! binary would test something else. It also measures what a page costs,
//! because this interface's performance history had never been exercised.
//!
//! SKIP with a message if the `wasm32-wasip2` target is missing or if `git`
//! is not installed: the same convention as the rest of the wasm e2e tests.

use std::path::{Path, PathBuf};
use std::process::Command;

use norte_core::PluginRegistry;
use norte_plugin_host::PluginRuntime;
use norte_proto::VPath;

/// The plugin's real manifest, read from its directory: if the file that gets
/// distributed and the one that gets tested could diverge, this test would
/// not test the plugin but a copy of it.
fn manifest() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/git-status/plugin.toml");
    std::fs::read_to_string(path).expect("the plugin's manifest")
}

/// Compiles the plugin to `wasm32-wasip2`, or `None` if the target is absent.
fn build_git_status() -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return None;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/git-status");
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-guests");
    let status = Command::new(env!("CARGO"))
        .current_dir(&dir)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("cargo for the git plugin");
    assert!(status.success(), "the git plugin did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("git_status.wasm");
    assert!(wasm.exists(), "missing {}", wasm.display());
    Some(wasm)
}

fn target_installed(target: &str) -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l == target)
        })
}

/// Installs the plugin under `cfg/plugins/<id>/` and approves and enables it,
/// which is what a person would do in the extensions manager.
fn install_and_approve(cfg: &Path, wasm: &Path) -> PluginRegistry {
    let dir = cfg.join("plugins").join("org.norte.git-status");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("plugin.toml"), manifest()).expect("manifest");
    std::fs::copy(wasm, dir.join("plugin.wasm")).expect("copy wasm");
    let mut reg = PluginRegistry::discover(cfg).expect("discover");
    assert!(reg.set_approval_in_memory("org.norte.git-status", true));
    assert!(reg.set_enabled_in_memory("org.norte.git-status", true));
    reg
}

/// A repository with one commit: `clean.txt` and `dirty.txt` tracked,
/// `new.txt` untracked, `junk.tmp` ignored. `None` if there is no `git`.
fn repo_fixture(dir: &Path) -> Option<()> {
    repo_fixture_inheriting(dir, &[])
}

/// The variables git uses to decide WHICH repository it operates on, above the
/// working directory. Git exports them to its hooks (#364).
const GIT_REPO_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
];

/// `git <args>` in `dir` and ONLY in `dir`: without the [`GIT_REPO_ENV`]
/// variables, which would carry the command to another repository (#364).
/// `inherited` simulates a hook's environment; it is removed AFTER setting
/// it, which is the order a test running under one would find it in. `true`
/// if git finished successfully.
fn git_in(dir: &Path, inherited: &[(&str, &Path)], args: &[&str]) -> bool {
    let mut cmd = Command::new("git");
    cmd.envs(inherited.iter().copied());
    for var in GIT_REPO_ENV {
        cmd.env_remove(var);
    }
    cmd.current_dir(dir)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// [`repo_fixture`] with INHERITED environment variables, like the ones git
/// exports to its hooks: this is what the decoy test needs to simulate (#364).
fn repo_fixture_inheriting(dir: &Path, inherited: &[(&str, &Path)]) -> Option<()> {
    let git = |args: &[&str]| git_in(dir, inherited, args).then_some(());
    git(&["init", "-q"])?;
    git(&["config", "user.email", "t@t"])?;
    git(&["config", "user.name", "t"])?;
    std::fs::write(dir.join(".gitignore"), b"*.tmp\n").ok()?;
    std::fs::write(dir.join("clean.txt"), b"clean\n").ok()?;
    std::fs::write(dir.join("dirty.txt"), b"before\n").ok()?;
    git(&["add", "-A"])?;
    git(&["commit", "-qm", "one"])?;
    // AFTER the commit: what the index has not seen.
    std::fs::write(dir.join("dirty.txt"), b"after, and longer\n").ok()?;
    std::fs::write(dir.join("new.txt"), b"new\n").ok()?;
    std::fs::write(dir.join("junk.tmp"), b"junk\n").ok()?;
    Some(())
}

/// #364: git exports `GIT_DIR` to its hooks, and the pre-push runs the gate. A
/// fixture that inherited it did `init`, `config` and `commit` on the
/// repository being pushed: a commit "one" that wiped everything, and
/// `user.name = t` in its `.git/config`. The fixture only touches ITS
/// directory.
#[test]
fn the_fixture_does_not_write_to_the_repo_of_an_inherited_git_dir() {
    let decoy = tempfile::tempdir().expect("tempdir");
    if repo_fixture(decoy.path()).is_none() {
        eprintln!("SKIP: without `git` installed there is no repository to look at");
        return;
    }
    // What the incident touched: the config (identity), the index (`add -A`)
    // and the branch (`commit`). A single different byte in any of them is
    // the regression.
    let git_dir = decoy.path().join(".git");
    let snapshot = || {
        let head = std::fs::read_to_string(git_dir.join("HEAD")).expect("HEAD");
        let branch = head.trim().strip_prefix("ref: ").expect("symbolic HEAD");
        (
            std::fs::read(git_dir.join("config")).expect("config"),
            std::fs::read(git_dir.join("index")).expect("index"),
            std::fs::read(git_dir.join(branch)).expect("branch ref"),
        )
    };
    let before = snapshot();

    let repo = tempfile::tempdir().expect("tempdir");
    let done = repo_fixture_inheriting(repo.path(), &[("GIT_DIR", &git_dir)]);

    assert!(
        before == snapshot(),
        "the fixture wrote to the inherited GIT_DIR's repository"
    );
    assert!(done.is_some(), "the fixture could not set up ITS repo");
    assert!(
        repo.path().join(".git").is_dir(),
        "the fixture did not create ITS repo"
    );
}

fn vpath_of(path: &Path) -> VPath {
    norte_vfs_local::vpath_from_native(path).expect("vpath")
}

/// The whole path: installed like someone else's plugin, approved by a
/// person, running over a real repository and answering per page.
#[test]
fn the_installed_plugin_paints_the_column_real_wasm() {
    let Some(wasm) = build_git_status() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let repo = tempfile::tempdir().expect("tempdir");
    if repo_fixture(repo.path()).is_none() {
        eprintln!("SKIP: without `git` installed there is no repository to look at");
        return;
    }
    let reg = install_and_approve(cfg.path(), &wasm);
    let (_, _, wasm_path, caps, settings) = reg
        .resolve_columns_of(Some("org.norte.git-status"), "git-status")
        .expect("the plugin resolves once approved");
    assert!(
        caps.location.granted() && caps.location_root_marker.as_deref() == Some(".git"),
        "the real manifest asks for a location with the `.git` marker"
    );

    let names: Vec<Vec<u8>> = vec![
        b"clean.txt".to_vec(),
        b"dirty.txt".to_vec(),
        b"new.txt".to_vec(),
        b"junk.tmp".to_vec(),
    ];
    let runtime = PluginRuntime::new().expect("runtime");
    let values = norte_core::plugins::run_column_values_for_test(
        &runtime,
        (
            "org.norte.git-status".to_owned(),
            "Git status".to_owned(),
            wasm_path,
            caps,
            settings,
        ),
        "git-status",
        Some(&vpath_of(repo.path())),
        true,
        &names,
        names.len(),
    );
    assert_eq!(
        values,
        vec![
            None,
            Some("M".to_owned()),
            Some("?".to_owned()),
            Some("!".to_owned())
        ],
        "clean, modified, untracked, ignored"
    );
}

/// The plugin works the same INSIDE the repository, which is the case the
/// root marker exists to solve: the root the host opens is the repository and
/// the prefix locates the page.
#[test]
fn inside_a_subdirectory_too_real_wasm() {
    let Some(wasm) = build_git_status() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let repo = tempfile::tempdir().expect("tempdir");
    if repo_fixture(repo.path()).is_none() {
        eprintln!("SKIP: without `git` installed there is no repository to look at");
        return;
    }
    let sub = repo.path().join("src/deep");
    std::fs::create_dir_all(&sub).expect("mkdir");
    std::fs::write(sub.join("deep.txt"), b"deep\n").expect("write");

    let reg = install_and_approve(cfg.path(), &wasm);
    let (_, _, wasm_path, caps, settings) = reg
        .resolve_columns_of(Some("org.norte.git-status"), "git-status")
        .expect("resolves");
    let runtime = PluginRuntime::new().expect("runtime");
    let values = norte_core::plugins::run_column_values_for_test(
        &runtime,
        (
            "org.norte.git-status".to_owned(),
            "Git status".to_owned(),
            wasm_path,
            caps,
            settings,
        ),
        "git-status",
        Some(&vpath_of(&sub)),
        true,
        &[b"deep.txt".to_vec()],
        1,
    );
    assert_eq!(
        values,
        vec![Some("?".to_owned())],
        "a new file three levels deep is still `untracked`"
    );
}

/// What a SINGLE page costs, measured, because this interface's performance
/// history had never been exercised.
///
/// The cap is deliberately loose (two seconds for twenty cells over an index
/// of two thousand entries): what this test defends is not a figure but the
/// order of magnitude — if it ever becomes seconds per page, something broke,
/// and the measured number is printed to know where from.
#[test]
fn a_page_over_a_large_index_costs_what_it_should_real_wasm() {
    let Some(wasm) = build_git_status() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let repo = tempfile::tempdir().expect("tempdir");
    if repo_fixture(repo.path()).is_none() {
        eprintln!("SKIP: without `git` installed there is no repository to look at");
        return;
    }
    let many = repo.path().join("many");
    std::fs::create_dir_all(&many).expect("mkdir");
    for i in 0..2_000 {
        std::fs::write(many.join(format!("f{i:05}.txt")), b"x\n").expect("write");
    }
    let ok = git_in(repo.path(), &[], &["add", "-A"])
        && git_in(repo.path(), &[], &["commit", "-qm", "many"]);
    assert!(ok, "the fixture's commit");

    let reg = install_and_approve(cfg.path(), &wasm);
    let (_, _, wasm_path, caps, settings) = reg
        .resolve_columns_of(Some("org.norte.git-status"), "git-status")
        .expect("resolves");
    let names: Vec<Vec<u8>> = (0..20)
        .map(|i| format!("f{i:05}.txt").into_bytes())
        .collect();
    let runtime = PluginRuntime::new().expect("runtime");

    let t0 = std::time::Instant::now();
    let values = norte_core::plugins::run_column_values_for_test(
        &runtime,
        (
            "org.norte.git-status".to_owned(),
            "Git status".to_owned(),
            wasm_path,
            caps,
            settings,
        ),
        "git-status",
        Some(&vpath_of(&many)),
        true,
        &names,
        names.len(),
    );
    let cost = t0.elapsed();
    eprintln!("a page of 20 over 2000 entries: {cost:?}");
    assert_eq!(values.len(), 20);
    assert!(
        values.iter().all(Option::is_none),
        "just committed: all clean"
    );
    assert!(
        cost < std::time::Duration::from_secs(2),
        "a page took {cost:?}: that is no longer a column, it is a wait"
    );
}

/// The SECOND page of the same directory does not re-instantiate the
/// component nor re-parse the index from scratch (#224).
///
/// What is asserted is the FACT — the instance was reused — not the
/// stopwatch: a test that demanded "the second one takes half as long" would
/// go red the day the machine is under load, and that is noise, not a
/// regression. The time is measured and printed all the same, which is where
/// the issue's 167 ms came from.
///
/// And reuse has a limit that is also pinned here: changing directory does
/// NOT reuse. The location is part of the key because it is what the guest
/// caches internally, and a parsed `.git/index` is no good for another
/// project.
#[test]
fn the_second_page_of_the_same_directory_reuses_the_instance() {
    let Some(wasm) = build_git_status() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let repo = tempfile::tempdir().expect("tempdir");
    if repo_fixture(repo.path()).is_none() {
        eprintln!("SKIP: without `git` installed there is no repository to look at");
        return;
    }
    let dir = repo.path().join("many");
    std::fs::create_dir_all(&dir).expect("mkdir");
    for i in 0..200 {
        std::fs::write(dir.join(format!("f{i:05}.txt")), b"x\n").expect("write");
    }
    let other = repo.path().join("others");
    std::fs::create_dir_all(&other).expect("mkdir");
    std::fs::write(other.join("a.txt"), b"x\n").expect("write");

    let reg = install_and_approve(cfg.path(), &wasm);
    let resolved = |reg: &PluginRegistry| {
        let (_, _, wasm_path, caps, settings) = reg
            .resolve_columns_of(Some("org.norte.git-status"), "git-status")
            .expect("resolves");
        (
            "org.norte.git-status".to_owned(),
            "Git status".to_owned(),
            wasm_path,
            caps,
            settings,
        )
    };
    let runtime = PluginRuntime::new().expect("runtime");
    let pool = norte_core::plugins::ColumnPool::default();
    let page = |from: usize| -> Vec<Vec<u8>> {
        (from..from + 20)
            .map(|i| format!("f{i:05}.txt").into_bytes())
            .collect()
    };

    let first = page(0);
    let t0 = std::time::Instant::now();
    let v1 = pool.column_values_for_test(
        &runtime,
        resolved(&reg),
        "git-status",
        Some(&vpath_of(&dir)),
        true,
        &first,
        first.len(),
    );
    let cost1 = t0.elapsed();
    assert_eq!(v1.len(), 20);
    assert_eq!(pool.reused(), 0, "the first one cannot reuse anything");

    let second = page(20);
    let t1 = std::time::Instant::now();
    let v2 = pool.column_values_for_test(
        &runtime,
        resolved(&reg),
        "git-status",
        Some(&vpath_of(&dir)),
        true,
        &second,
        second.len(),
    );
    let cost2 = t1.elapsed();
    assert_eq!(v2.len(), 20);
    assert_eq!(
        pool.reused(),
        1,
        "the second page of the SAME directory has to hit the live instance"
    );
    eprintln!("page 1: {cost1:?} · page 2 (reusing): {cost2:?}");

    // A different directory, a different guest cache: not reused.
    let v3 = pool.column_values_for_test(
        &runtime,
        resolved(&reg),
        "git-status",
        Some(&vpath_of(&other)),
        true,
        &[b"a.txt".to_vec()],
        1,
    );
    assert_eq!(v3.len(), 1);
    assert_eq!(
        pool.reused(),
        1,
        "changing location instantiates again: the key carries the directory"
    );

    // And going back to the first one DOES reuse, which is what makes this a
    // pool and not a memory of the last call.
    let v4 = pool.column_values_for_test(
        &runtime,
        resolved(&reg),
        "git-status",
        Some(&vpath_of(&dir)),
        true,
        &first,
        first.len(),
    );
    assert_eq!(v4, v1, "the same directory gives the same values");
    assert_eq!(pool.reused(), 2);
}
