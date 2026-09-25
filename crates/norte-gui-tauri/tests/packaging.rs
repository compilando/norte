//! What the PACKAGE promises, pinned (#256).
//!
//! These checks read the bundler's configuration and the desktop entry, not
//! the runtime, and that is the point: a broken package is not seen in any
//! behavior test. It is seen when installed on a clean machine — which is
//! when it is already too late — or it is seen here.
//!
//! Packaging was first exercised on 2026-08-25 and four things came out at
//! once: the description said "Vertical spike of the Tauri renderer", the
//! category was plain `Utility`, there was no `MimeType` (i.e. "open folder
//! with…" did not offer norte) and `Exec` handed over a URL to a binary that
//! expects a path. All of this kind: invisible until someone installs.

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn conf() -> serde_json::Value {
    let p = root().join("tauri.conf.json");
    let raw = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", p.display()));
    serde_json::from_str(&raw).expect("valid JSON")
}

fn desktop() -> String {
    let p = root().join("norte.desktop");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("could not read {}: {e}", p.display()))
}

/// A desktop entry key, without comments or spaces.
fn key(text: &str, k: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix(&format!("{k}=")))
        .map(str::to_owned)
}

/// **The package brings the DAEMON and the CLI, not just the window.**
///
/// Without them, on a clean install the window has nobody to ask for a
/// listing: `norte-gui` looks for `norte` as its own executable's SIBLING,
/// and if it is not there, there is no daemon to start. It was #256's open
/// question.
#[test]
fn the_package_brings_the_daemon_and_the_cli() {
    let cfg = conf();
    let external: Vec<&str> = cfg["bundle"]["externalBin"]
        .as_array()
        .expect("there are external binaries")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        external.iter().any(|b| b.ends_with("/norte")),
        "the daemon travels with the window: {external:?}"
    );
    assert!(
        external.iter().any(|b| b.ends_with("/ntc")),
        "and the terminal client: {external:?}"
    );
}

/// **The long description describes norte, not the scaffolding it was tested
/// with.**
///
/// It is what shows up in the package manager, and it used to say "Vertical
/// spike of the Tauri renderer over norte-ui-host" — an internal-log sentence
/// in front of whoever is deciding whether to install this.
/// **The identifier does not say `spike`.**
///
/// It is the application id: it names the webview's data directory, the
/// desktop entry and the package. While it said `dev.norte.gui.spike`,
/// anyone looking at what is installed read that this is an experiment — and
/// it was, until the go/no-go closed (ADR 0087).
///
/// Changing it DOES have a cost and that is why it was done now: an install
/// with the old id does not update on top, it stays alongside. In alpha the
/// cost is zero; after the first stable release it would have been a
/// migration.
#[test]
fn the_identifier_does_not_say_spike() {
    let cfg = conf();
    let id = cfg["identifier"].as_str().expect("there is an identifier");
    assert!(
        !id.contains("spike"),
        "the application id still says this is an experiment: {id}"
    );
    assert_eq!(
        id, "dev.norte.gui",
        "and it is the one the package promises"
    );
}

#[test]
fn the_description_does_not_speak_of_a_spike() {
    let cfg = conf();
    let long = cfg["bundle"]["longDescription"]
        .as_str()
        .expect("there is a long description")
        .to_lowercase();
    assert!(
        !long.contains("spike"),
        "the package's description is not a development note: {long:?}"
    );
    assert!(
        long.contains("ficheros"),
        "and it says what this is: {long:?}"
    );
}

/// **`Exec` hands over a PATH, not a URL.**
///
/// `%U` gives `file:///home`, and the binary puts it into a `PathBuf`: that
/// is a RELATIVE path that does not exist, so the window would open on an
/// error instead of on the folder the desktop just named. `%f` gives the
/// local path, which is what `norte-gui [DIR]` knows how to read.
#[test]
fn the_exec_delivers_a_path_and_not_a_url() {
    let d = desktop();
    let exec = key(&d, "Exec").expect("there is an Exec");
    assert!(
        !exec.contains("%U") && !exec.contains("%u"),
        "a URL is not a path: {exec:?}"
    );
    assert!(
        exec.contains("%f") || exec.contains("%F"),
        "and the folder is passed to it: {exec:?}"
    );
}

/// **It offers itself to open folders.**
///
/// Without `MimeType=inode/directory`, "open with…" on a folder does not list
/// norte. For a file manager that is the entire desktop integration, and its
/// absence shows up nowhere else.
#[test]
fn the_desktop_offers_it_to_open_directories() {
    let d = desktop();
    let mime = key(&d, "MimeType").unwrap_or_default();
    assert!(
        mime.contains("inode/directory"),
        "a file manager opens directories: {mime:?}"
    );
    let cats = key(&d, "Categories").unwrap_or_default();
    assert!(
        cats.contains("FileManager"),
        "and it declares itself as what it is: {cats:?}"
    );
    // ONE main category. With two, the application can show up TWICE in the
    // menu, and `desktop-file-validate` warns about that — it does not run in
    // the gate, so the check lives here.
    let principales = [
        "AudioVideo",
        "Audio",
        "Video",
        "Development",
        "Education",
        "Game",
        "Graphics",
        "Network",
        "Office",
        "Science",
        "Settings",
        "System",
        "Utility",
    ];
    let how_many = principales
        .iter()
        .filter(|p| cats.split(';').any(|c| c == **p))
        .count();
    assert_eq!(
        how_many, 1,
        "a single main category, or it shows up twice in the menu: {cats:?}"
    );
}

/// The desktop entry that gets packaged is OURS, not the one the bundler
/// generates.
///
/// Without this line in the configuration, everything above gets written to
/// a file nobody uses: the bundler composes its own and discards this one.
#[test]
fn the_desktop_entry_is_the_one_in_the_repo() {
    let cfg = conf();
    assert_eq!(
        cfg["bundle"]["linux"]["deb"]["desktopTemplate"].as_str(),
        Some("norte.desktop"),
        "the bundler has to use the repo's template"
    );
    assert!(
        root().join("norte.desktop").is_file(),
        "and that template exists"
    );
}
