//! `norte plugin install` (A4): bringing a plugin into the config directory.
//!
//! What is tested here is not the copy — that is `std::fs::copy` — but the
//! three decisions around it: where the id comes from, that installing does
//! NOT consent, and that replacing WITHDRAWS consent.

use std::path::Path;

use norte_core::plugins::{
    InstallError, PluginRegistry, UninstallError, install, installed_provider_schemes, uninstall,
};

const MANIFEST: &str = r#"
[plugin]
id = "org.norte.demo"
name = "Demo"
publisher = "norte"
version = "0.1.0"
category = "previewer"

[contributions]
previewer = [{ mimetypes = ["text/*"] }]

[capabilities]
fs-read = "scoped"
"#;

/// A provider plugin: the CLI routes its scheme as soon as it is installed.
const PROVIDER_MANIFEST: &str = r#"
[plugin]
id = "org.norte.memplug"
name = "Mem plug"
publisher = "norte"
version = "0.1.0"
category = "provider"

[[contributions.provider]]
scheme = "memplug"
"#;

/// A source with a manifest and a `.wasm` (any bytes: install does not run it).
fn source(dir: &Path, manifest: &str, wasm: &[u8]) -> std::path::PathBuf {
    let src = dir.join("src-plugin");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("plugin.toml"), manifest).expect("manifest");
    std::fs::write(src.join("plugin.wasm"), wasm).expect("wasm");
    src
}

/// The destination comes from the MANIFEST'S ID, not the source directory's
/// name: that is what the discoverer will use, and letting the directory name
/// decide where it lands would be a way to overwrite another plugin.
#[test]
fn it_lands_under_the_manifests_id_not_the_directorys() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = source(cfg.path(), MANIFEST, b"\0asm");

    let rep = install(cfg.path(), &src, false).expect("installs");
    assert_eq!(rep.id, "org.norte.demo");
    assert!(!rep.replaced);

    let dest = cfg.path().join("plugins").join("org.norte.demo");
    assert!(dest.join("plugin.toml").is_file());
    assert!(dest.join("plugin.wasm").is_file());
    // The source directory was named `src-plugin` and left no trace.
    assert!(!cfg.path().join("plugins").join("src-plugin").exists());
}

/// Installing is NOT consenting: the plugin ends up discovered and
/// unapproved. An installer that approved on its own would turn "I bring this
/// file" into "I grant its capabilities", which is the whole decision.
#[test]
fn installing_does_not_approve_or_enable() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = source(cfg.path(), MANIFEST, b"\0asm");
    install(cfg.path(), &src, false).expect("installs");

    let reg = PluginRegistry::discover(cfg.path()).expect("discover");
    let listing = reg.list();
    let p = listing
        .plugins
        .iter()
        .find(|p| p.id == "org.norte.demo")
        .expect("discovered");
    assert!(!p.approved, "installing does not approve");
    assert!(!p.enabled, "installing does not enable");
}

/// Without `force`, an already-installed id is rejected. With a message, not
/// silently.
#[test]
fn it_does_not_overwrite_without_force() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = source(cfg.path(), MANIFEST, b"\0asm");
    install(cfg.path(), &src, false).expect("first");

    match install(cfg.path(), &src, false) {
        Err(InstallError::AlreadyInstalled(id)) => assert_eq!(id, "org.norte.demo"),
        other => panic!("expected AlreadyInstalled, got {other:?}"),
    }
}

/// THE test that matters: replacing WITHDRAWS consent.
///
/// The approval digest covers the manifest — capabilities, category,
/// contributions — and NOT the `.wasm`. Without withdrawing it, installing
/// over an approved plugin would leave a different binary running under the
/// permission a human gave another one, with an identical manifest so nothing
/// noticed. That is why this test leaves the manifest INTACT and only changes
/// the wasm.
#[test]
fn replacing_withdraws_consent() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = source(cfg.path(), MANIFEST, b"\0old-asm");
    install(cfg.path(), &src, false).expect("installs");

    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    reg.set_approval("org.norte.demo", true).expect("approves");
    assert!(
        reg.list()
            .plugins
            .iter()
            .any(|p| p.id == "org.norte.demo" && p.approved),
        "precondition: approved before replacing"
    );

    // SAME manifest, a different binary.
    std::fs::write(src.join("plugin.wasm"), b"\0new-asm").expect("new wasm");
    let rep = install(cfg.path(), &src, true).expect("replaces");
    assert!(rep.replaced);

    let reg = PluginRegistry::discover(cfg.path()).expect("re-discover");
    let p = reg
        .list()
        .plugins
        .into_iter()
        .find(|p| p.id == "org.norte.demo")
        .expect("still discovered");
    assert!(
        !p.approved,
        "the binary changed under an identical manifest: approval must NOT survive"
    );
    assert!(!p.enabled);
}

/// A manifest that does not validate copies nothing at all: it is rejected
/// beforehand, so a broken source leaves no half-plugin in the config
/// directory.
#[test]
fn an_invalid_manifest_leaves_no_trace() {
    let cfg = tempfile::tempdir().expect("tempdir");
    // A hook with no events is rejected (ADR 0100) — serves as a real invalid
    // manifest instead of a broken TOML, which would prove something else.
    let bad = MANIFEST.replace(r#"category = "previewer""#, r#"category = "hook""#);
    let src = source(cfg.path(), &bad, b"\0asm");

    assert!(matches!(
        install(cfg.path(), &src, false),
        Err(InstallError::Manifest(_))
    ));
    assert!(
        !cfg.path().join("plugins").exists(),
        "an invalid source creates no destination directory"
    );
}

/// Without a `.wasm` there is no plugin, and it is reported before copying the
/// manifest.
#[test]
fn without_wasm_it_does_not_install() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = cfg.path().join("src-plugin");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("plugin.toml"), MANIFEST).expect("manifest");

    assert!(matches!(
        install(cfg.path(), &src, false),
        Err(InstallError::NoWasm(_))
    ));
    assert!(!cfg.path().join("plugins").exists());
}

/// ADR 0057: the location capability travels to the frontend through the SAME
/// channel as the others — `PluginInfo::capabilities`, which is OPEN
/// vocabulary.
///
/// That is why this change does NOT touch the wire: a dedicated field for
/// `location` would be a second way of saying the same thing, with its own
/// bump and goldens, and an N-1 client would render it just as well by reading
/// the badge it already gets.
#[test]
fn the_location_badge_reaches_the_listing_without_touching_the_wire() {
    const WITH_LOCATION: &str = r#"
[plugin]
id = "org.norte.git-status"
name = "Git status"
publisher = "norte"
version = "0.1.0"
category = "columns"

[[contributions.columns]]
id = "git-status"
header = "Git"

[capabilities]
location = "read"
"#;
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = source(cfg.path(), WITH_LOCATION, b"\0asm");
    install(cfg.path(), &src, false).expect("installs");

    let reg = PluginRegistry::discover(cfg.path()).expect("discovers");
    let listing = reg.list();
    let info = listing
        .plugins
        .iter()
        .find(|p| p.id == "org.norte.git-status")
        .expect("the plugin is there");
    assert!(
        info.capabilities.iter().any(|c| c == "location"),
        "the human sees WHAT they are about to approve: {:?}",
        info.capabilities
    );
    assert!(!info.approved, "installing consents to nothing");
}

/// Uninstalling removes the directory and leaves the state entry TURNED OFF —
/// not deleted: `persist_state` merges over the file, so removing the key
/// would leave it intact, and a plugin reinstalled under the same id would
/// inherit its predecessor's consent.
#[test]
fn uninstalling_removes_the_directory_and_withdraws_consent() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = source(cfg.path(), MANIFEST, b"\0asm");
    install(cfg.path(), &src, false).expect("installs");
    {
        let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
        assert!(reg.set_approval("org.norte.demo", true).expect("approves"));
        assert!(reg.set_enabled("org.norte.demo", true).expect("enables"));
    }

    let rep = uninstall(cfg.path(), "org.norte.demo").expect("uninstalls");
    assert_eq!(rep.id, "org.norte.demo");
    assert!(rep.was_approved, "the report says there WAS consent");
    assert!(!cfg.path().join("plugins").join("org.norte.demo").exists());

    // Reinstalling the same id comes back WITHOUT consent.
    install(cfg.path(), &src, false).expect("reinstalls");
    let reg = PluginRegistry::discover(cfg.path()).expect("discover");
    let p = reg
        .list()
        .plugins
        .into_iter()
        .find(|p| p.id == "org.norte.demo")
        .expect("discovered");
    assert!(!p.approved && !p.enabled, "consent does not survive");
}

/// The CLI decides whether an argument is a URL by its scheme, and an
/// installed provider plugin adds its own: `webdav://x` has to stop being a
/// local file with a strange name as soon as someone serves it. INSTALLED
/// ones are counted, consented or not: routing the URL grants nothing, and
/// connecting is still fail-closed.
#[test]
fn the_schemes_of_installed_providers_are_listed() {
    let cfg = tempfile::tempdir().expect("tempdir");
    assert!(installed_provider_schemes(cfg.path()).is_empty());
    let src = source(cfg.path(), PROVIDER_MANIFEST, b"\0asm");
    install(cfg.path(), &src, false).expect("installs");
    assert_eq!(installed_provider_schemes(cfg.path()), vec!["memplug"]);
}

/// A `plugin.wasm` above the runtime's cap is not copied: neither would the
/// catalogue read it nor would the runtime instantiate it, and copying it
/// would leave a plugin that every discovery lists as broken.
#[test]
fn installing_rejects_a_binary_above_the_cap() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = source(cfg.path(), MANIFEST, b"\0asm");
    let f = std::fs::File::create(src.join("plugin.wasm")).expect("wasm");
    f.set_len(norte_plugin_host::MAX_ARTIFACT_BYTES + 1)
        .expect("sparse");
    drop(f);
    match install(cfg.path(), &src, false) {
        Err(InstallError::WasmTooLarge { len, cap }) => {
            assert_eq!(len, cap + 1);
        }
        other => panic!("expected WasmTooLarge, got {other:?}"),
    }
    assert!(!cfg.path().join("plugins/org.norte.demo").exists());
}

/// An id that is not installed is a named error, not an empty `Ok`.
#[test]
fn uninstalling_what_is_not_there_is_an_error() {
    let cfg = tempfile::tempdir().expect("tempdir");
    match uninstall(cfg.path(), "org.norte.nobody") {
        Err(UninstallError::NotInstalled(id)) => assert_eq!(id, "org.norte.nobody"),
        other => panic!("expected NotInstalled, got {other:?}"),
    }
}

/// The id comes from the command line and turns into a PATH under
/// `plugins/`: anything that is not a plugin id is rejected BEFORE touching
/// disk, or `../../..` would delete whatever it pointed to.
#[test]
fn uninstalling_rejects_an_id_that_is_not_an_id() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let outside = cfg.path().join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir");
    for bad in ["../outside", "plugins", ".", "", "org.norte.demo/.."] {
        assert!(
            matches!(uninstall(cfg.path(), bad), Err(UninstallError::InvalidId)),
            "{bad:?} should be rejected"
        );
    }
    assert!(outside.is_dir(), "nothing outside plugins/ is touched");
}
