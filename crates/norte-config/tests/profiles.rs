//! Profiles against a real disk: what cannot be asked with an injected
//! environment.

use std::ffi::OsString;

/// Names are BYTES: a directory with non-UTF-8 bytes is LISTED (it exists,
/// the reader created it) instead of vanishing from the picker. #245/#246
/// were this bug twice, with layout names.
#[test]
#[cfg(unix)]
fn a_non_utf8_name_is_listed_as_is() {
    use std::os::unix::ffi::OsStringExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let hostile = OsString::from_vec(vec![b'w', 0xFF, b'k']);
    std::fs::create_dir(dir.path().join(&hostile)).expect("mkdir");
    std::fs::create_dir(dir.path().join("work")).expect("mkdir");
    std::fs::write(dir.path().join("not-a-dir"), b"x").expect("write");

    let mut got = norte_config::list_profiles(dir.path()).expect("list");
    got.sort();
    let mut want = vec![hostile, OsString::from("work")];
    want.sort();
    assert_eq!(got, want, "files are not profiles; the bytes are respected");
}

/// The WHOLE path: real layer resolver, real disk, real `load`.
///
/// The allowlist tests build `Layers` by hand, so they exercise the merge and
/// NOT the resolver. This one starts where the binary starts — a config
/// directory and a profile name — and checks that the profile's theme
/// reaches `CommonConfig`. Without it, a failure in the layer splice leaves
/// the suite green while the reader sees their user layer's theme with an
/// active profile that says otherwise.
#[test]
fn the_profiles_theme_arrives_through_the_whole_path() {
    let cfg = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        cfg.path().join("norte.toml"),
        "[ui]\ntheme = \"retro-crt\"\n",
    )
    .expect("write");
    let profile = cfg.path().join("profiles").join("photos");
    std::fs::create_dir_all(&profile).expect("mkdir");
    std::fs::write(
        profile.join("norte.toml"),
        "[profile]\ntitle = \"Photos\"\n\n[ui]\ntheme = \"gruvbox-light\"\n",
    )
    .expect("write");

    let root = cfg.path().to_path_buf();
    let get = move |k: &str| -> Option<OsString> {
        (k == "NORTE_CONFIG_DIR").then(|| OsString::from(root.as_os_str()))
    };
    let layers = norte_config::profiles::standard_layers_with_profile_on(
        false,
        &get,
        Some(std::ffi::OsStr::new("photos")),
    );
    assert!(
        layers
            .dirs
            .iter()
            .any(|(d, k)| *k == norte_config::Layer::Profile && d == &profile),
        "the profile layer is in the resolver: {:?}",
        layers.dirs
    );

    let merged = norte_config::load(&layers).expect("loads");
    assert_eq!(
        merged.ui_theme.as_deref(),
        Some("gruvbox-light"),
        "the PROFILE's theme overrides the user's"
    );
}

/// A profiles directory that does not exist is not an error: it is "you have
/// no profiles yet".
#[test]
fn with_no_profiles_directory_the_list_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let got = norte_config::list_profiles(&dir.path().join("does-not-exist")).expect("list");
    assert!(got.is_empty());
}
