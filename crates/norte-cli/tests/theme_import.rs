//! `norte theme import` end to end: the binary reads a VS Code theme with its
//! `include` chain and leaves a TOML that the frontends' resolver finds by
//! name.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use norte_theme::{Role, Theme};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/themes")
        .join(name)
}

/// The subject is the binary, never the configuration of whoever runs the
/// suite.
fn norte(config: &Path) -> Command {
    let mut c = Command::cargo_bin("norte").expect("norte binary compiled");
    c.env("NORTE_CONFIG_DIR", config);
    c.env("NORTE_LANG", "en");
    c
}

/// Importing produces a TOML that PARSES, resolves every core role and
/// carries the colors of BOTH the child and the parent: the whole trip, not
/// just the parser.
#[test]
fn importing_produces_a_complete_and_resolvable_theme() {
    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import"])
        .arg(fixture("hijo.jsonc"))
        .args(["--name", "night"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The color that is not a color is REPORTED, not swallowed.
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("badge.background"), "{err}");

    let written = std::fs::read_to_string(config.path().join("themes/night.toml")).unwrap();
    let t = Theme::from_toml(&written).expect("the written TOML parses");
    assert_eq!(t.name.as_deref(), Some("night"));
    for &role in Role::CORE {
        let s = t.style(role);
        assert!(
            s.fg.is_some() || s.bg.is_some(),
            "{role:?} has no color after importing"
        );
    }
    // From the child, which wins over the parent on `editor.background`.
    assert_eq!(t.style(Role::Background).bg.unwrap().to_hex(), "#101820");
    assert_eq!(t.style(Role::FocusBorder).fg.unwrap().to_hex(), "#ff8800");
    // From the parent, which the child does not define.
    assert_eq!(
        t.style(Role::PaneBackground).bg.unwrap().to_hex(),
        "#0a1018"
    );
    // With alpha: composited over the background, not opaque white.
    assert_ne!(
        t.style(Role::ScrollbarSlider).bg.unwrap().to_hex(),
        "#ffffff"
    );

    // And the frontends' resolver finds it by NAME.
    let resolved =
        norte_frontend::theme::resolve_theme_in(Some("night"), Some(config.path())).unwrap();
    assert_eq!(resolved.name.as_deref(), Some("night"));
}

/// Without `--name`, the name comes from the JSON's `name`.
#[test]
fn the_name_comes_from_the_json() {
    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import"])
        .arg(fixture("hijo.jsonc"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(config.path().join("themes/mi-tema-noche.toml").is_file());
}

/// A name that is already an embedded preset gets REJECTED: the resolver puts
/// the presets first, so the file would never be read and writing it would be
/// an operation that does nothing without saying so.
#[test]
fn importing_with_a_presets_name_is_rejected() {
    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import"])
        .arg(fixture("hijo.jsonc"))
        .args(["--name", "nord"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "should reject a preset's name");
    assert!(!config.path().join("themes/nord.toml").exists());
}

/// A name the resolver would treat as a path is not written either.
#[test]
fn a_name_that_is_a_path_is_rejected() {
    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import"])
        .arg(fixture("hijo.jsonc"))
        .args(["--name", "../outside"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!config.path().join("outside.toml").exists());
}

/// An existing theme is not overwritten without `--force`; with it, it is.
#[test]
fn does_not_overwrite_without_force() {
    let config = tempfile::tempdir().unwrap();
    let themes = config.path().join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    std::fs::write(themes.join("night.toml"), "name = \"mine\"\n").unwrap();

    let without = norte(config.path())
        .args(["theme", "import"])
        .arg(fixture("hijo.jsonc"))
        .args(["--name", "night"])
        .output()
        .unwrap();
    assert!(!without.status.success());
    assert_eq!(
        std::fs::read_to_string(themes.join("night.toml")).unwrap(),
        "name = \"mine\"\n",
        "the user's theme is still intact"
    );

    let with = norte(config.path())
        .args(["theme", "import", "--force", "--name", "night"])
        .arg(fixture("hijo.jsonc"))
        .output()
        .unwrap();
    assert!(
        with.status.success(),
        "{}",
        String::from_utf8_lossy(&with.stderr)
    );
    assert!(
        std::fs::read_to_string(themes.join("night.toml"))
            .unwrap()
            .contains("name = \"night\"")
    );
}

/// A theme that includes itself (a → b → a) is an ERROR, not a hang.
#[test]
fn an_include_cycle_is_an_error() {
    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "cycle"])
        .arg(fixture("ciclo-a.json"))
        .timeout(std::time::Duration::from_secs(30))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("loops back"), "{err}");
    assert!(!config.path().join("themes/cycle.toml").exists());
}

/// `--use` leaves the theme set in `[ui] theme` and respects what was already
/// in `norte.toml`, comments included.
#[test]
fn use_writes_ui_theme_without_losing_comments() {
    let config = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("norte.toml"),
        "# my config\n[ui]\ntheme = \"nord\"\n",
    )
    .unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--use", "--name", "night"])
        .arg(fixture("hijo.jsonc"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let toml = std::fs::read_to_string(config.path().join("norte.toml")).unwrap();
    assert!(toml.contains("# my config"), "{toml}");
    assert!(toml.contains("theme = \"night\""), "{toml}");
}

/// A chain of `n` includes in a temp directory: `t0` → `t1` → … → `tn`.
fn chain_of_includes(dir: &Path, n: usize) -> PathBuf {
    for i in 0..=n {
        let include = if i < n {
            format!(r#""include": "t{}.json", "#, i + 1)
        } else {
            String::new()
        };
        std::fs::write(
            dir.join(format!("t{i}.json")),
            format!(r#"{{ {include}"colors": {{}} }}"#),
        )
        .unwrap();
    }
    dir.join("t0.json")
}

/// Eight includes are followed; nine are not: the cap cuts a pathological
/// chain without rejecting the real ones (VS Code nests three).
#[test]
fn the_includes_cap() {
    let eight = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "eight"])
        .arg(chain_of_includes(eight.path(), 8))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let nine = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "nine"])
        .arg(chain_of_includes(nine.path(), 9))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("more than 8 includes"));
}

/// Does `s` carry anything a terminal would interpret, or that misrepresents
/// what is read?
fn has_hazard(s: &str) -> bool {
    // The newline that closes each message belongs to the CLI, not the theme.
    s.chars()
        .filter(|c| *c != '\n')
        .any(norte_encoding::is_terminal_hazard)
}

/// The `colors` keys are written by whoever published the theme, and the ones
/// that are not a color get NAMED on stderr. A key with ESC+OSC would rewrite
/// the terminal title of whoever imports it: it comes out masked.
#[test]
fn a_hostile_key_does_not_reach_the_terminal_raw() {
    let payload = norte_testkit::corpus::hostile_runs()
        .into_iter()
        .find(|r| r.id == "run_osc_title_injection")
        .expect("corpus fixture")
        .run;
    let dir = tempfile::tempdir().unwrap();
    let theme = dir.path().join("hostile.json");
    let json = serde_json::json!({ "type": "dark", "colors": { payload: "not a color" } });
    std::fs::write(&theme, json.to_string()).unwrap();

    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "hostile"])
        .arg(&theme)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("app.quit"), "the key is named: {err}");
    assert!(!has_hazard(&err), "raw ESC/BEL in stderr: {err:?}");
}

/// A file name with a RIGHT-TO-LEFT OVERRIDE neither breaks the TOML header
/// nor misrepresents it to whoever reads it, nor comes out raw in the
/// messages.
#[cfg(unix)]
#[test]
fn a_hostile_file_name_is_masked_in_header_and_messages() {
    use std::os::unix::ffi::OsStrExt as _;
    let name = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "rtl_override")
        .expect("corpus fixture");
    let dir = tempfile::tempdir().unwrap();
    let theme = dir.path().join(std::ffi::OsStr::from_bytes(&name.bytes));
    std::fs::copy(fixture("padre.json"), &theme).unwrap();

    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "rlo"])
        .arg(&theme)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!has_hazard(&String::from_utf8_lossy(&out.stdout)));
    let written = std::fs::read_to_string(config.path().join("themes/rlo.toml")).unwrap();
    let header: String = written.lines().take_while(|l| l.starts_with('#')).collect();
    assert!(!has_hazard(&header), "{header:?}");
}

/// A theme saved as UTF-16 with a BOM — what some Windows editors export — is
/// a theme, not "not a VS Code theme".
#[test]
fn a_utf16_theme_with_a_bom_is_imported() {
    let text = std::fs::read_to_string(fixture("padre.json")).unwrap();
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let dir = tempfile::tempdir().unwrap();
    let theme = dir.path().join("utf16.json");
    std::fs::write(&theme, bytes).unwrap();

    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "u16"])
        .arg(&theme)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t =
        Theme::from_toml(&std::fs::read_to_string(config.path().join("themes/u16.toml")).unwrap())
            .unwrap();
    assert_eq!(
        t.style(Role::PaneBackground).bg.unwrap().to_hex(),
        "#0a1018"
    );
}

/// Without a BOM, UTF-16 has NULs and cannot be told apart from a binary: the
/// error says so, instead of claiming the file is not a theme.
#[test]
fn utf16_without_bom_gives_an_error_that_names_the_encoding() {
    let mut bytes = Vec::new();
    for unit in "{\"colors\":{}}".encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let dir = tempfile::tempdir().unwrap();
    let theme = dir.path().join("no-bom.json");
    std::fs::write(&theme, bytes).unwrap();

    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "x"])
        .arg(&theme)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("UTF-16"), "{err}");
}
