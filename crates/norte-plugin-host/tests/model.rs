//! Plugin host model (ADR 0022, M4-P1): manifest, capabilities, catalog.
//! No WASM runtime (M4-P2).

use norte_plugin_host::{
    COMMAND_ID_MAX_CHARS, COMMAND_MAX_COUNT, COMMAND_TITLE_MAX_CHARS, CONFIG_DESCRIPTION_MAX_CHARS,
    CONFIG_ENUM_MAX_VALUES, CONFIG_MAX_KEYS, CONFIG_STRING_MAX_CHARS, Catalog, Category,
    ConfigKeySpec, HelpPresence, Manifest, ManifestError, Scope,
};

mod support;

const SYNTAX_PREVIEW: &str = r#"
[plugin]
id = "org.norte.syntax-preview"
name = "Syntax Preview"
publisher = "norte"
version = "0.1.0"
category = "previewer"

[contributions]
previewer = [{ mimetypes = ["text/*", "application/json"] }]

[capabilities]
fs-read = "scoped"
"#;

#[test]
fn a_complete_manifest_parses() {
    let m = Manifest::from_toml(SYNTAX_PREVIEW).unwrap();
    assert_eq!(m.id, "org.norte.syntax-preview");
    assert_eq!(m.category, Category::Previewer);
    assert_eq!(m.contributions.previewer.len(), 1);
    assert_eq!(
        m.contributions.previewer[0].mimetypes,
        vec!["text/*", "application/json"]
    );
    assert_eq!(m.capabilities.fs_read, Scope::Scoped);
    assert_eq!(m.capabilities.fs_write, norte_plugin_host::FsWriteCap::None);
    assert_eq!(m.capabilities.badges(), vec!["fs-read".to_owned()]);
}

#[test]
fn absent_capabilities_are_none() {
    let m = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.x.y"
        name = "Y"
        publisher = "x"
        version = "0.1.0"
        category = "command"
    "#,
    )
    .unwrap();
    assert_eq!(m.capabilities.fs_read, Scope::None);
    assert!(m.capabilities.badges().is_empty());
    assert!(m.capabilities.net.is_none());
}

#[test]
fn exec_other_than_none_is_rejected() {
    let src = r#"
        [plugin]
        id = "org.evil.plugin"
        name = "Evil"
        publisher = "evil"
        version = "0.1.0"
        category = "command"
        [capabilities]
        exec = "shell"
    "#;
    assert!(matches!(
        Manifest::from_toml(src),
        Err(ManifestError::ExecForbidden)
    ));
    // An explicit `exec = "none"` DOES work.
    let ok = src.replace(r#"exec = "shell""#, r#"exec = "none""#);
    assert!(Manifest::from_toml(&ok).is_ok());
}

/// Nobody runs a hook: the category is in the manifest, in the catalog and
/// in the UI, but there is no WIT interface, no world, no place on the
/// host to call it from. Accepting the manifest would install something
/// inert and the manager would paint it as just another plugin — the
/// worst of the three options, because the author finds out when nothing
/// happens.
///
/// Rejected while parsing, with the reason. The category is NOT removed:
/// spec §7.1 names hooks among the interfaces WIT must cover, so removing
/// it would move the code away from the specification instead of closer
/// to it.
#[test]
fn a_hook_listens_to_events_from_the_closed_vocabulary() {
    // An event outside the vocabulary is rejected WITH the value:
    // `before-*` does not exist on purpose (ADR 0100), and the error says
    // so.
    let unknown = r#"
        [plugin]
        id = "org.demo.hooker"
        name = "Hooker"
        publisher = "demo"
        version = "0.1.0"
        category = "hook"
        [[contributions.hook]]
        on = "before-copy"
    "#;
    assert!(matches!(
        Manifest::from_toml(unknown),
        Err(ManifestError::HookUnknownEvent(ref e)) if e == "before-copy"
    ));

    // Also as a contribution of a plugin of another category: it is the
    // declaration that gets validated, not the field that classifies it.
    let by_contribution = r#"
        [plugin]
        id = "org.demo.sneaky"
        name = "Sneaky"
        publisher = "demo"
        version = "0.1.0"
        category = "command"
        [[contributions.hook]]
        on = "after-copy"
    "#;
    assert!(matches!(
        Manifest::from_toml(by_contribution),
        Err(ManifestError::HookUnknownEvent(_))
    ));

    // And a VALID event on a plugin of another category does not get in
    // either: only `hook` ones get dispatched, so it would be an inert
    // promise.
    let on_other = by_contribution.replace("after-copy", "after-renamed");
    assert!(matches!(
        Manifest::from_toml(&on_other),
        Err(ManifestError::HookOnOtherCategory)
    ));

    // A hook with network access is rejected: it receives the path of
    // every mutation.
    let with_net = r#"
        [plugin]
        id = "org.demo.leak"
        name = "Leak"
        publisher = "demo"
        version = "0.1.0"
        category = "hook"
        [[contributions.hook]]
        on = "after-renamed"
        [capabilities]
        net = { hosts = ["203.0.113.5"] }
    "#;
    assert!(matches!(
        Manifest::from_toml(with_net),
        Err(ManifestError::HookWithNet)
    ));
}

/// A hook with two valid sidecars: the starting point of the `fs-write`
/// tests.
const HOOK_WITH_SIDECAR: &str = r#"
        [plugin]
        id = "org.demo.log"
        name = "Log"
        publisher = "demo"
        version = "0.1.0"
        category = "hook"
        [[contributions.hook]]
        on = "after-renamed"
        [capabilities]
        fs-write = { sidecar = [".norte-renames.log", "renames.json"] }
    "#;

/// `fs-write` (ADR 0101): sidecars, only for hooks, real names; the
/// reserved `"scoped"` is rejected saying what to put instead.
#[test]
fn fs_write_is_sidecars_and_only_for_hooks() {
    let hook_with_sidecar = HOOK_WITH_SIDECAR;
    let m = Manifest::from_toml(hook_with_sidecar).expect("valid sidecars");
    assert_eq!(
        m.capabilities.fs_write.sidecar_names(),
        &[".norte-renames.log".to_owned(), "renames.json".to_owned()]
    );
    assert_eq!(
        m.capabilities.badges(),
        vec![
            "fs-write:.norte-renames.log".to_owned(),
            "fs-write:renames.json".to_owned()
        ]
    );
    // A control character inside never becomes valid TOML, so it is
    // tested in the function: the manifest rejects it earlier by another
    // path. And with it, whatever is not portable ASCII: bidi, Windows
    // reserved names, trailing dot.
    for bad in [
        "x\u{1b}y",
        "log\u{202e}",
        "CON",
        "nul.txt",
        "COM1.log",
        "end.",
        "a:b",
        "ñ",
    ] {
        assert!(!norte_plugin_host::is_valid_sidecar_name(bad), "{bad:?}");
    }
    assert!(norte_plugin_host::is_valid_sidecar_name("CONTROL.log"));
    for bad in ["a/b", "..", ""] {
        let src = hook_with_sidecar.replace("renames.json", bad);
        assert!(
            matches!(
                Manifest::from_toml(&src),
                Err(ManifestError::SidecarName(_))
            ),
            "{bad:?}"
        );
    }
    let repeated = hook_with_sidecar.replace("renames.json", ".norte-renames.log");
    assert!(matches!(
        Manifest::from_toml(&repeated),
        Err(ManifestError::SidecarName(_))
    ));
}

/// `fs-write = "none"` still works (ADR 0022); `"scoped"`, an empty list
/// and an extra key say why not.
#[test]
fn fs_write_none_is_valid_and_the_errors_say_why() {
    let hook_with_sidecar = HOOK_WITH_SIDECAR;
    let reserved = hook_with_sidecar.replace(
        r#"fs-write = { sidecar = [".norte-renames.log", "renames.json"] }"#,
        r#"fs-write = "scoped""#,
    );
    assert!(matches!(
        Manifest::from_toml(&reserved),
        Err(ManifestError::FsWriteReserved(ref s)) if s == "scoped"
    ));
    // `"none"` (ADR 0022) still works: it is the same as absent, and
    // digests the same, so an existing approval does not move.
    let none = reserved.replace(r#"fs-write = "scoped""#, r#"fs-write = "none""#);
    let without = reserved.replace(r#"fs-write = "scoped""#, "");
    let m_none = Manifest::from_toml(&none).expect("none is valid");
    assert_eq!(
        m_none.capabilities.fs_write,
        norte_plugin_host::FsWriteCap::None
    );
    assert_eq!(
        m_none.approval_digest(),
        Manifest::from_toml(&without)
            .expect("absent is valid")
            .approval_digest()
    );
    // An empty or oversized list says how many it carried, not a name.
    let empty = hook_with_sidecar.replace(r#"[".norte-renames.log", "renames.json"]"#, "[]");
    assert!(matches!(
        Manifest::from_toml(&empty),
        Err(ManifestError::SidecarListSize { got: 0 })
    ));
    // And a key other than `sidecar` in the table is an invalid manifest.
    let extra = hook_with_sidecar.replace(
        r#"fs-write = { sidecar = [".norte-renames.log", "renames.json"] }"#,
        r#"fs-write = { sidecar = ["a.log"], grant = "all" }"#,
    );
    assert!(Manifest::from_toml(&extra).is_err());
    let on_previewer = hook_with_sidecar
        .replace(r#"category = "hook""#, r#"category = "previewer""#)
        .replace("[[contributions.hook]]\n        on = \"after-renamed\"", "");
    assert!(matches!(
        Manifest::from_toml(&on_previewer),
        Err(ManifestError::SidecarNotForCategory)
    ));

    // A hook that listens to nothing is inert, and it says so.
    let no_events = r#"
        [plugin]
        id = "org.demo.mute"
        name = "Mute"
        publisher = "demo"
        version = "0.1.0"
        category = "hook"
    "#;
    assert!(matches!(
        Manifest::from_toml(no_events),
        Err(ManifestError::HookWithoutEvents)
    ));

    // And with the five events that exist, it goes through; the code's
    // vocabulary is what the constant publishes.
    for on in norte_plugin_host::HOOK_EVENTS {
        let good = format!(
            r#"
            [plugin]
            id = "org.demo.listener"
            name = "Listener"
            publisher = "demo"
            version = "0.1.0"
            category = "hook"
            [[contributions.hook]]
            on = "{on}"
        "#
        );
        let m = Manifest::from_toml(&good).unwrap_or_else(|e| panic!("{on}: {e}"));
        assert_eq!(m.contributions.hook[0].on, *on);
    }
}

#[test]
fn a_non_reverse_dns_id_is_rejected() {
    let src = r#"
        [plugin]
        id = "nodot"
        name = "N"
        publisher = "p"
        version = "0.1.0"
        category = "command"
    "#;
    assert!(matches!(Manifest::from_toml(src), Err(ManifestError::Id)));
}

#[test]
fn strict_reverse_dns_id_charset() {
    // `id_literal` = the EXACT text of the TOML value (already escaped).
    // Allows sneaking in `\n` (TOML escape → real newline in the value) or
    // `\"` (a quote).
    let with_id = |id_literal: &str| {
        format!(
            r#"
        [plugin]
        id = {id_literal}
        name = "N"
        publisher = "p"
        version = "0.1.0"
        category = "command"
    "#
        )
    };

    // Valid ids: alphanumeric segments with hyphens, with at least one dot.
    assert!(Manifest::from_toml(&with_id(r#""org.norte.demo""#)).is_ok());
    assert!(Manifest::from_toml(&with_id(r#""org.foo-bar.baz""#)).is_ok());

    // Hostile ids that DO parse as TOML but fail the charset ⇒
    // `ManifestError::Id` (they never reach the log nor the T5 approval
    // modal):
    //  - `\n` (TOML escape) = a real newline in the value → log injection.
    //  - `\"` (TOML escape) = a quote in the value → dialog spoofing.
    //  - spaces, underscore, non-ASCII, leading/trailing dot, empty segment.
    for bad_literal in [
        r#""org.norte.de\nmo""#,
        r#""org.\"norte\".demo""#,
        r#""org norte demo""#,
        r#""org.norte.de_mo""#,
        r#""org.norte.デモ""#,
        r#"".org.norte""#,
        r#""org.norte.""#,
        r#""org..norte""#,
        r#""orgnorte""#,
    ] {
        assert!(
            matches!(
                Manifest::from_toml(&with_id(bad_literal)),
                Err(ManifestError::Id)
            ),
            "hostile id must be rejected as Id: {bad_literal}"
        );
    }

    // Total length > 128 is rejected.
    let long = format!(r#""org.norte.{}""#, "a".repeat(120));
    assert!(matches!(
        Manifest::from_toml(&with_id(&long)),
        Err(ManifestError::Id)
    ));
}

#[test]
fn absent_description_is_none() {
    let m = Manifest::from_toml(SYNTAX_PREVIEW).unwrap();
    assert_eq!(m.description, None);
}

#[test]
fn present_description_is_parsed() {
    let src = r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        description = "Generates inline Markdown previews."
    "#;
    let m = Manifest::from_toml(src).unwrap();
    assert_eq!(
        m.description.as_deref(),
        Some("Generates inline Markdown previews.")
    );
}

#[test]
fn a_280_char_description_is_the_exact_cap() {
    let d = "a".repeat(280);
    let src = format!(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        description = "{d}"
    "#
    );
    let m = Manifest::from_toml(&src).unwrap();
    assert_eq!(m.description.as_deref(), Some(d.as_str()));
}

#[test]
fn a_281_char_description_is_rejected() {
    let d = "a".repeat(281);
    let src = format!(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        description = "{d}"
    "#
    );
    assert!(matches!(
        Manifest::from_toml(&src),
        Err(ManifestError::DescriptionTooLong)
    ));
}

#[test]
fn description_counts_characters_not_bytes() {
    // 280 NON-ASCII characters (multi-byte in UTF-8): the cap is in CHARS,
    // not bytes, or a legitimate manifest in a non-ASCII language would be
    // rejected ahead of time.
    let d = "á".repeat(280);
    let src = format!(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        description = "{d}"
    "#
    );
    assert!(Manifest::from_toml(&src).is_ok());
}

#[test]
fn an_edited_description_does_not_move_the_approval_digest() {
    // Precedent from manifest.rs:296-307 (cosmetic name/publisher/version):
    // description is ALSO cosmetic — editing it must NOT reinvalidate
    // capabilities the human already approved.
    let base = |desc: Option<&str>| {
        let d = desc.map_or_else(String::new, |d| format!(r#"description = "{d}""#));
        Manifest::from_toml(&format!(
            r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        {d}
        [capabilities]
        fs-read = "scoped"
    "#
        ))
        .unwrap()
    };
    let without_desc = base(None);
    let with_desc = base(Some("Some description."));
    let with_another_desc = base(Some("A TOTALLY different description."));
    assert_eq!(
        without_desc.approval_digest(),
        with_desc.approval_digest(),
        "adding a description must not move the digest"
    );
    assert_eq!(
        with_desc.approval_digest(),
        with_another_desc.approval_digest(),
        "editing the description must not move the digest"
    );
}

/// P1 encoding audit M2: a manifest with ONE `contributions.command`,
/// parameterized `id`/`title` — to test the 120/64 (chars) caps without
/// repeating the `[plugin]` boilerplate.
fn manifest_with_command(id: &str, title: &str) -> Result<Manifest, ManifestError> {
    Manifest::from_toml(&format!(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        [contributions]
        command = [{{ id = "{id}", title = "{title}" }}]
    "#
    ))
}

#[test]
fn a_120_char_command_title_is_the_exact_cap() {
    let title = "a".repeat(COMMAND_TITLE_MAX_CHARS);
    let m = manifest_with_command("cmd", &title).unwrap();
    assert_eq!(m.contributions.command[0].title, title);
}

#[test]
fn a_121_char_command_title_is_rejected() {
    let title = "a".repeat(COMMAND_TITLE_MAX_CHARS + 1);
    assert!(matches!(
        manifest_with_command("cmd", &title),
        Err(ManifestError::CommandTitleTooLong)
    ));
}

#[test]
fn a_64_char_command_id_is_the_exact_cap() {
    let id = "a".repeat(COMMAND_ID_MAX_CHARS);
    let m = manifest_with_command(&id, "Title").unwrap();
    assert_eq!(m.contributions.command[0].id, id);
}

fn manifest_with_n_commands(n: usize) -> Result<Manifest, ManifestError> {
    let cmds: Vec<String> = (0..n)
        .map(|i| format!(r#"{{ id = "c{i}", title = "C{i}" }}"#))
        .collect();
    Manifest::from_toml(&format!(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        [contributions]
        command = [{}]
    "#,
        cmds.join(", ")
    ))
}

/// The cap is cut at the MANIFEST, not at each palette that paints it
/// (#281). The window already defends itself on its side (512 extensions,
/// 2048 rows), but that is the client protecting itself from the server.
#[test]
fn command_33_is_rejected() {
    assert!(matches!(
        manifest_with_n_commands(COMMAND_MAX_COUNT + 1),
        Err(ManifestError::TooManyCommands)
    ));
}

#[test]
fn command_32_is_the_exact_cap() {
    let m = manifest_with_n_commands(COMMAND_MAX_COUNT).unwrap();
    assert_eq!(m.contributions.command.len(), COMMAND_MAX_COUNT);
}

#[test]
fn a_65_char_command_id_is_rejected() {
    let id = "a".repeat(COMMAND_ID_MAX_CHARS + 1);
    assert!(matches!(
        manifest_with_command(&id, "Title"),
        Err(ManifestError::CommandIdTooLong)
    ));
}

/// The cap is a PARSING one, not a digest one: `title` DOES go into
/// `approval_digest` (it decides when/how the command fires), but that was
/// already tested by
/// `approval_digest_includes_category_and_contributions_not_just_capabilities`
/// — the NEW cap only rejects NEW manifests that exceed it, it never
/// reinterprets a digest already computed for an old one within the cap
/// (the digest hashes `title`'s VALUE, not the cap it was validated
/// against while parsing).
#[test]
fn a_command_within_the_cap_does_not_change_the_digests_criterion() {
    let a = manifest_with_command("cmd", "Title A").unwrap();
    let b = manifest_with_command("cmd", "Title B").unwrap();
    assert_ne!(
        a.approval_digest(),
        b.approval_digest(),
        "a different title DOES move the digest (it is not cosmetic like description)"
    );
}

#[test]
fn net_capability_lists_hosts() {
    let m = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.webdav"
        name = "WebDAV"
        publisher = "norte"
        version = "0.1.0"
        category = "provider"
        [contributions]
        provider = [{ scheme = "webdav" }]
        [capabilities]
        net = { hosts = ["dav.example.com"] }
    "#,
    )
    .unwrap();
    assert_eq!(m.contributions.provider[0].scheme, "webdav");
    assert_eq!(
        m.capabilities.net.as_ref().unwrap().hosts,
        ["dav.example.com"]
    );
    assert_eq!(m.capabilities.badges(), vec!["net".to_owned()]);
}

fn write_plugin(root: &std::path::Path, id: &str, toml: &str) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), toml).unwrap();
    // the real .wasm arrives in M4-P2; the catalog only requires the manifest.
}

/// Adds `config_dir/plugins/<id>/config.toml` to a plugin ALREADY written
/// with [`write_plugin`] (P2 Task 2).
fn write_config_values(root: &std::path::Path, id: &str, toml: &str) {
    std::fs::write(root.join(id).join("config.toml"), toml).unwrap();
}

#[test]
fn the_catalog_discovers_orders_and_groups() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);
    write_plugin(
        root.path(),
        "org.norte.bulk-rename",
        r#"
        [plugin]
        id = "org.norte.bulk-rename"
        name = "Bulk Rename"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        [capabilities]
        fs-read = "scoped"
    "#,
    );
    // An invalid one: it must not disappear silently, it goes to `errors`.
    write_plugin(
        root.path(),
        "org.bad.exec",
        r#"
        [plugin]
        id = "org.bad.exec"
        name = "Bad"
        publisher = "bad"
        version = "0.1.0"
        category = "command"
        [capabilities]
        exec = "shell"
    "#,
    );

    let cat = Catalog::load_dir(root.path());
    assert_eq!(cat.plugins.len(), 2, "two valid ones");
    assert_eq!(
        cat.errors.len(),
        1,
        "the exec-shell one goes to errors, it is not hidden"
    );

    // Grouped by category, in order (command before previewer would be
    // wrong — previewer comes first in the ORDER).
    let groups = cat.by_category();
    let cats: Vec<Category> = groups.iter().map(|(c, _)| *c).collect();
    assert_eq!(cats, vec![Category::Previewer, Category::Command]);
    assert_eq!(groups[1].1[0].manifest.id, "org.norte.bulk-rename");
}

#[test]
fn a_nonexistent_dir_is_an_empty_catalog() {
    let cat = Catalog::load_dir(std::path::Path::new("/does/not/exist/norte/for/sure"));
    assert!(cat.plugins.is_empty() && cat.errors.is_empty());
}

#[test]
fn approval_digest_includes_category_and_contributions_not_just_capabilities() {
    // Issue #69 (MINOR 1): the approval digest covers category +
    // contributions (when/how it fires), not just [capabilities]. A
    // re-edited manifest that changes those fields WHILE KEEPING the
    // capabilities must move the digest (→ fail-closed re-consent), or it
    // would start auto-firing without approval.
    let command = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        [capabilities]
        fs-read = "scoped"
    "#,
    )
    .unwrap();

    // SAME capabilities, but category previewer (with a mimetypes entry).
    let previewer = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "previewer"
        [contributions]
        previewer = [{ mimetypes = ["text/*"] }]
        [capabilities]
        fs-read = "scoped"
    "#,
    )
    .unwrap();

    // SAME category previewer + same capabilities, but WIDENED mimetypes.
    let previewer_wide = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "previewer"
        [contributions]
        previewer = [{ mimetypes = ["text/*", "application/*"] }]
        [capabilities]
        fs-read = "scoped"
    "#,
    )
    .unwrap();

    assert_eq!(
        command.capabilities.digest(),
        previewer.capabilities.digest(),
        "the capabilities are identical (test control)"
    );
    assert_ne!(
        command.approval_digest(),
        previewer.approval_digest(),
        "changing the category moves the approval digest"
    );
    assert_ne!(
        previewer.approval_digest(),
        previewer_wide.approval_digest(),
        "widening the entry's mimetypes moves the approval digest"
    );
    // Deterministic and stable for a given manifest.
    assert_eq!(command.approval_digest(), command.approval_digest());
}

#[test]
fn the_catalog_rejects_duplicate_ids_in_two_directories() {
    // Issue #69: two different directories declare the SAME `plugin.id`. A
    // second dir cannot claim the first one's approval to sneak in its
    // `plugin.wasm`. BOTH are rejected (fail-closed), "the first one" is
    // not chosen.
    let root = tempfile::tempdir().unwrap();
    let dupe = r#"
        [plugin]
        id = "org.norte.clash"
        name = "Clash"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
    "#;
    // Two subdirectories with different names but the same declared id.
    write_plugin(root.path(), "dir-a", dupe);
    write_plugin(root.path(), "dir-b", dupe);
    // And a legitimate one with a unique id: it must not be affected by
    // the unrelated collision.
    write_plugin(
        root.path(),
        "solo",
        r#"
        [plugin]
        id = "org.norte.solo"
        name = "Solo"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
    "#,
    );

    let cat = Catalog::load_dir(root.path());
    assert_eq!(
        cat.plugins.len(),
        1,
        "only the unique id loads; the colliding ones are rejected"
    );
    assert_eq!(cat.plugins[0].manifest.id, "org.norte.solo");
    assert_eq!(cat.errors.len(), 2, "both directories of the duplicate id");
    assert!(
        cat.errors
            .iter()
            .all(|e| matches!(&e.error, ManifestError::DuplicateId(id) if id == "org.norte.clash")),
        "both errors are DuplicateId of the colliding id"
    );
}

/// P2: non-regression pin. `SYNTAX_PREVIEW`'s digest (without `[config]`)
/// captured BEFORE introducing the `[config]` schema into the digest's
/// canonical form (a commit before this one). If this test breaks, P2's
/// extension moved the digest of a manifest WITHOUT `[config]` — that
/// would reset ALL existing human approvals of plugins that do not use
/// `[config]`, which is exactly what P2's plan decision 2 forbids.
#[test]
fn manifest_without_config_digests_identical_to_pre_p2() {
    const DIGEST_PRE_P2: &str = "9ba598fcee4cb10e91a2de3683287a11af83c9bdd7bc18ecb2df79570f9c0d5f";
    let m = Manifest::from_toml(SYNTAX_PREVIEW).unwrap();
    assert_eq!(
        m.approval_digest(),
        DIGEST_PRE_P2,
        "a manifest without [config] must digest THE SAME as before P2 \
         (or it resets existing approvals)"
    );
}

/// ADR 0057: requesting `location` CHANGES the digest — i.e. requires
/// approving again — and not requesting it leaves it intact. Both halves
/// are the same decision: a new capability does not sneak in without
/// consent, and adding it to the schema cannot invalidate consent that
/// already exists.
#[test]
fn declared_location_goes_into_the_approval_digest() {
    let without = Manifest::from_toml(COLUMNS_PLUGIN).unwrap();
    let with = Manifest::from_toml(
        &COLUMNS_PLUGIN.replace("[capabilities]", "[capabilities]\nlocation = \"read\""),
    )
    .unwrap();
    assert_ne!(
        without.approval_digest(),
        with.approval_digest(),
        "requesting a new capability REQUIRES approving it again"
    );
    assert!(with.capabilities.location.granted());
    assert!(
        with.capabilities
            .badges()
            .iter()
            .any(|b| b.starts_with("location")),
        "the badge names the location capability"
    );

    // And WITH a MARKER, the badge SAYS SO (#241): plain "location" reads
    // as "can read where I'm looking", and what is granted is the nearest
    // ancestor containing the marker — the whole project, not the folder.
    let with_marker = Manifest::from_toml(&COLUMNS_PLUGIN.replace(
        "[capabilities]",
        "[capabilities]\nlocation = \"read\"\nlocation-root-marker = \".git\"",
    ))
    .unwrap();
    assert!(
        with_marker
            .capabilities
            .badges()
            .contains(&"location-root:.git".to_owned()),
        "the badge says which marker opens the ancestor: {:?}",
        with_marker.capabilities.badges()
    );
}

/// CLOSED vocabulary, like `exec`: a made-up value is an invalid
/// manifest, never a capability silently ignored.
#[test]
fn an_unknown_location_value_is_a_manifest_error() {
    let m = COLUMNS_PLUGIN.replace("[capabilities]", "[capabilities]\nlocation = \"write\"");
    assert!(Manifest::from_toml(&m).is_err());
}

/// A columns manifest WITH `[capabilities]` but without `location`.
const COLUMNS_PLUGIN: &str = r#"
[plugin]
id = "org.norte.columns"
name = "Columns"
publisher = "norte"
version = "0.1.0"
category = "columns"

[capabilities]
fs-read = "none"

[[contributions.columns]]
id = "name-len"
header = "Length"
"#;

// --- P2: the manifest's `[config]` schema (inside the approval digest) ---

const WITH_CONFIG: &str = r#"
[plugin]
id = "org.norte.demo-config"
name = "Demo Config"
publisher = "norte"
version = "0.1.0"
category = "command"

[config.greeting]
type = "string"
default = "hola"
description = "Greeting shown on startup."

[config.enabled]
type = "bool"
default = true

[config.retries]
type = "int"
default = 3
min = 0
max = 10

[config.mode]
type = "enum"
default = "fast"
values = ["fast", "slow"]
"#;

/// Minimal `[plugin]` boilerplate + the `[config.*]` entries injected into
/// it, to test P2's caps without repeating the rest of the manifest.
fn manifest_with_config(entries: &str) -> Result<Manifest, ManifestError> {
    Manifest::from_toml(&format!(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        {entries}
    "#
    ))
}

#[test]
fn config_the_4_types_parse() {
    let m = Manifest::from_toml(WITH_CONFIG).unwrap();
    assert_eq!(m.config.len(), 4);
    assert_eq!(
        m.config.get("greeting"),
        Some(&ConfigKeySpec::String {
            default: "hola".into(),
            description: Some("Greeting shown on startup.".into()),
        })
    );
    assert_eq!(
        m.config.get("enabled"),
        Some(&ConfigKeySpec::Bool {
            default: true,
            description: None,
        })
    );
    assert_eq!(
        m.config.get("retries"),
        Some(&ConfigKeySpec::Int {
            default: 3,
            min: Some(0),
            max: Some(10),
            description: None,
        })
    );
    assert_eq!(
        m.config.get("mode"),
        Some(&ConfigKeySpec::Enum {
            default: "fast".into(),
            values: vec!["fast".into(), "slow".into()],
            description: None,
        })
    );
}

#[test]
fn absent_config_is_an_empty_map() {
    let m = Manifest::from_toml(SYNTAX_PREVIEW).unwrap();
    assert!(m.config.is_empty());
}

#[test]
fn config_33_keys_is_rejected() {
    use std::fmt::Write as _;
    let mut entries = String::new();
    for i in 0..=CONFIG_MAX_KEYS {
        let _ = write!(
            entries,
            "\n[config.k{i}]\ntype = \"bool\"\ndefault = true\n"
        );
    }
    assert!(matches!(
        manifest_with_config(&entries),
        Err(ManifestError::ConfigTooManyKeys)
    ));
}

#[test]
fn config_32_keys_is_the_exact_cap() {
    use std::fmt::Write as _;
    let mut entries = String::new();
    for i in 0..CONFIG_MAX_KEYS {
        let _ = write!(
            entries,
            "\n[config.k{i}]\ntype = \"bool\"\ndefault = true\n"
        );
    }
    let m = manifest_with_config(&entries).unwrap();
    assert_eq!(m.config.len(), CONFIG_MAX_KEYS);
}

#[test]
fn config_key_with_uppercase_is_rejected() {
    let entries = "\n[config.Bad]\ntype = \"bool\"\ndefault = true\n";
    assert!(matches!(
        manifest_with_config(entries),
        Err(ManifestError::ConfigKeyCharset)
    ));
}

#[test]
fn config_key_with_underscore_is_rejected() {
    let entries = "\n[config.has_underscore]\ntype = \"bool\"\ndefault = true\n";
    assert!(matches!(
        manifest_with_config(entries),
        Err(ManifestError::ConfigKeyCharset)
    ));
}

#[test]
fn a_33_char_config_key_is_rejected() {
    let key = "a".repeat(33);
    let entries = format!("\n[config.{key}]\ntype = \"bool\"\ndefault = true\n");
    assert!(matches!(
        manifest_with_config(&entries),
        Err(ManifestError::ConfigKeyCharset)
    ));
}

#[test]
fn a_32_char_config_key_is_the_exact_cap() {
    let key = "a".repeat(32);
    let entries = format!("\n[config.{key}]\ntype = \"bool\"\ndefault = true\n");
    let m = manifest_with_config(&entries).unwrap();
    assert!(m.config.contains_key(&key));
}

#[test]
fn a_281_char_config_description_is_rejected() {
    let d = "a".repeat(CONFIG_DESCRIPTION_MAX_CHARS + 1);
    let entries = format!(
        "\n[config.greeting]\ntype = \"string\"\ndefault = \"hi\"\ndescription = \"{d}\"\n"
    );
    assert!(matches!(
        manifest_with_config(&entries),
        Err(ManifestError::ConfigDescriptionTooLong)
    ));
}

#[test]
fn a_280_char_config_description_is_the_exact_cap() {
    let d = "a".repeat(CONFIG_DESCRIPTION_MAX_CHARS);
    let entries = format!(
        "\n[config.greeting]\ntype = \"string\"\ndefault = \"hi\"\ndescription = \"{d}\"\n"
    );
    let m = manifest_with_config(&entries).unwrap();
    assert_eq!(
        m.config.get("greeting"),
        Some(&ConfigKeySpec::String {
            default: "hi".into(),
            description: Some(d),
        })
    );
}

#[test]
fn a_281_char_string_default_is_rejected() {
    let d = "a".repeat(CONFIG_STRING_MAX_CHARS + 1);
    let entries = format!("\n[config.greeting]\ntype = \"string\"\ndefault = \"{d}\"\n");
    assert!(matches!(
        manifest_with_config(&entries),
        Err(ManifestError::ConfigDefaultTooLong)
    ));
}

#[test]
fn config_int_default_above_the_maximum_is_rejected() {
    let entries = "\n[config.retries]\ntype = \"int\"\ndefault = 20\nmin = 0\nmax = 10\n";
    assert!(matches!(
        manifest_with_config(entries),
        Err(ManifestError::ConfigIntDefaultOutOfRange)
    ));
}

#[test]
fn config_int_default_below_the_minimum_is_rejected() {
    let entries = "\n[config.retries]\ntype = \"int\"\ndefault = -1\nmin = 0\nmax = 10\n";
    assert!(matches!(
        manifest_with_config(entries),
        Err(ManifestError::ConfigIntDefaultOutOfRange)
    ));
}

#[test]
fn config_int_default_at_the_edge_is_valid() {
    let entries = "\n[config.retries]\ntype = \"int\"\ndefault = 10\nmin = 0\nmax = 10\n";
    let m = manifest_with_config(entries).unwrap();
    assert_eq!(
        m.config.get("retries"),
        Some(&ConfigKeySpec::Int {
            default: 10,
            min: Some(0),
            max: Some(10),
            description: None,
        })
    );
}

#[test]
fn config_enum_default_absent_from_values_is_rejected() {
    let entries =
        "\n[config.mode]\ntype = \"enum\"\ndefault = \"turbo\"\nvalues = [\"fast\", \"slow\"]\n";
    assert!(matches!(
        manifest_with_config(entries),
        Err(ManifestError::ConfigEnumDefaultNotInValues)
    ));
}

#[test]
fn config_enum_17_values_is_rejected() {
    let values: Vec<String> = (0..=CONFIG_ENUM_MAX_VALUES)
        .map(|i| format!("\"v{i}\""))
        .collect();
    let entries = format!(
        "\n[config.mode]\ntype = \"enum\"\ndefault = \"v0\"\nvalues = [{}]\n",
        values.join(", ")
    );
    assert!(matches!(
        manifest_with_config(&entries),
        Err(ManifestError::ConfigEnumTooManyValues)
    ));
}

#[test]
fn config_enum_16_values_is_the_exact_cap() {
    let values: Vec<String> = (0..CONFIG_ENUM_MAX_VALUES)
        .map(|i| format!("\"v{i}\""))
        .collect();
    let entries = format!(
        "\n[config.mode]\ntype = \"enum\"\ndefault = \"v0\"\nvalues = [{}]\n",
        values.join(", ")
    );
    let m = manifest_with_config(&entries).unwrap();
    match m.config.get("mode").unwrap() {
        ConfigKeySpec::Enum { values, .. } => assert_eq!(values.len(), CONFIG_ENUM_MAX_VALUES),
        other => panic!("expected Enum, got {other:?}"),
    }
}

#[test]
fn a_281_char_config_enum_value_is_rejected() {
    let long_value = "a".repeat(CONFIG_STRING_MAX_CHARS + 1);
    let entries = format!(
        "\n[config.mode]\ntype = \"enum\"\ndefault = \"{long_value}\"\nvalues = [\"{long_value}\"]\n"
    );
    assert!(matches!(
        manifest_with_config(&entries),
        Err(ManifestError::ConfigEnumValueTooLong)
    ));
}

#[test]
fn present_config_moves_the_approval_digest() {
    let without_config = manifest_with_config("").unwrap();
    let with_config =
        manifest_with_config("\n[config.greeting]\ntype = \"string\"\ndefault = \"hola\"\n")
            .unwrap();
    assert_ne!(
        without_config.approval_digest(),
        with_config.approval_digest(),
        "declaring [config] must move the approval digest (decision 2)"
    );
}

#[test]
fn a_different_config_default_moves_the_approval_digest() {
    let a = manifest_with_config("\n[config.greeting]\ntype = \"string\"\ndefault = \"hola\"\n")
        .unwrap();
    let b = manifest_with_config("\n[config.greeting]\ntype = \"string\"\ndefault = \"adios\"\n")
        .unwrap();
    assert_ne!(
        a.approval_digest(),
        b.approval_digest(),
        "a different default is different behavior: it must move the digest"
    );
}

#[test]
fn an_edited_config_description_does_not_move_the_approval_digest() {
    // Same criterion as `plugin.description` (cosmetic): editing it does
    // not reinvalidate already-approved capabilities.
    let a = manifest_with_config(
        "\n[config.greeting]\ntype = \"string\"\ndefault = \"hola\"\ndescription = \"one\"\n",
    )
    .unwrap();
    let b = manifest_with_config(
        "\n[config.greeting]\ntype = \"string\"\ndefault = \"hola\"\ndescription = \"two, very different\"\n",
    )
    .unwrap();
    assert_eq!(
        a.approval_digest(),
        b.approval_digest(),
        "editing a config key's description must not move the digest"
    );
}

#[test]
fn an_empty_config_table_digests_the_same_as_absent() {
    // decision 2: the `config:` section is only added to the digest if
    // the map is NOT empty — a `[config]` table present but with no keys
    // must digest the same as it being totally absent.
    let without_table = manifest_with_config("").unwrap();
    let empty_table = manifest_with_config("\n[config]\n").unwrap();
    assert_eq!(
        without_table.approval_digest(),
        empty_table.approval_digest()
    );
}

// --- P2 Task 2: `resolve_settings` wiring in `Catalog::load_dir` -------

#[test]
fn the_catalog_excludes_the_plugin_via_error_on_invalid_config_toml() {
    // A `config.toml` that does NOT validate against the manifest's
    // `[config]` schema (P2 decision 3) excludes the WHOLE plugin from
    // the catalog (fail-closed, same treatment as a broken `plugin.toml`
    // or a duplicate id): it goes to `errors`, never to `plugins` with
    // half-way values.
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.demo-config", WITH_CONFIG);
    write_config_values(root.path(), "org.norte.demo-config", "mode = \"turbo\"\n");
    // A healthy control plugin, without `[config]`.
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);

    let cat = Catalog::load_dir(root.path());
    assert_eq!(
        cat.plugins.len(),
        1,
        "the plugin with an invalid config.toml does NOT load"
    );
    assert_eq!(cat.plugins[0].manifest.id, "org.norte.syntax-preview");
    assert_eq!(
        cat.errors.len(),
        1,
        "the invalid config.toml goes to errors"
    );
    assert!(
        matches!(
            &cat.errors[0].error,
            ManifestError::ConfigValues(inner) if inner.to_string().contains("mode")
        ),
        "the error names the KEY (mode), not the value: {:?}",
        cat.errors[0].error
    );
}

#[test]
fn the_catalog_resolves_settings_in_the_entry_from_a_valid_config_toml() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.demo-config", WITH_CONFIG);
    write_config_values(root.path(), "org.norte.demo-config", "retries = 7\n");

    let cat = Catalog::load_dir(root.path());
    assert_eq!(cat.errors.len(), 0, "{:?}", cat.errors);
    assert_eq!(cat.plugins.len(), 1);
    let settings = &cat.plugins[0].settings;
    assert_eq!(settings.get("retries").map(String::as_str), Some("7"));
    // The rest stays at its default.
    assert_eq!(settings.get("greeting").map(String::as_str), Some("hola"));
}

#[test]
fn the_catalog_resolves_defaults_in_the_entry_without_config_toml() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.demo-config", WITH_CONFIG);
    // Without writing config.toml.

    let cat = Catalog::load_dir(root.path());
    assert_eq!(cat.errors.len(), 0, "{:?}", cat.errors);
    let settings = &cat.plugins[0].settings;
    assert_eq!(settings.len(), 4);
    assert_eq!(settings.get("mode").map(String::as_str), Some("fast"));
}

#[test]
fn a_catalog_plugin_without_config_has_empty_settings() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);

    let cat = Catalog::load_dir(root.path());
    assert!(cat.plugins[0].settings.is_empty());
}

// --- H3e: the catalog announces whether the plugin carries `help.md` ----

#[test]
fn discovery_flags_the_plugin_that_carries_help_md() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);
    std::fs::write(
        root.path().join("org.norte.syntax-preview").join("help.md"),
        "+++\nid = \"org.norte.syntax-preview\"\ntitle = \"Preview\"\n+++\nbody",
    )
    .unwrap();

    let cat = Catalog::load_dir(root.path());
    assert_eq!(
        cat.plugins[0].help,
        HelpPresence::Servable,
        "the discovered help.md is announced, and passes the guard"
    );
}

#[test]
fn without_help_md_no_help_is_announced() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);

    let cat = Catalog::load_dir(root.path());
    assert_eq!(cat.plugins[0].help, HelpPresence::Absent);
}

#[test]
fn a_help_md_that_is_a_directory_announces_no_help() {
    // `is_file`, not `exists`: a `help.md` that is a directory is not a
    // page, and announcing it would make the sidebar paint a node that
    // then opens empty.
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);
    std::fs::create_dir_all(root.path().join("org.norte.syntax-preview").join("help.md")).unwrap();

    let cat = Catalog::load_dir(root.path());
    assert_eq!(
        cat.plugins[0].help,
        HelpPresence::Absent,
        "neither present nor servable: a directory is not a page"
    );
}

/// The third state, the one that exists precisely so as not to collapse
/// with the other two (H3e): there is a `help.md` and the host will NOT
/// serve it. `Absent` would say the author never documented anything and
/// `Servable` would promise a page; only this state lets `norte doctor`
/// report "you put one there and it points outside".
#[cfg(unix)]
#[test]
fn a_help_md_that_escapes_the_directory_is_present_but_not_servable() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);
    let outside = root.path().join("outside.md");
    std::fs::write(&outside, "secret").unwrap();
    std::os::unix::fs::symlink(
        &outside,
        root.path().join("org.norte.syntax-preview").join("help.md"),
    )
    .unwrap();

    let cat = Catalog::load_dir(root.path());
    assert_eq!(cat.plugins[0].help, HelpPresence::Unservable);
    assert!(cat.plugins[0].help.is_present(), "the file is there");
    assert!(
        !cat.plugins[0].help.is_servable(),
        "and the wire does not announce it: announcing and serving empty is the oracle"
    );
}

// ---------------------------------------------------------------------
// ADR 0037 (G3b): `Category::Decorator` + `Contributions.decorator`.

/// A minimal `decorator` manifest: a new category, a single empty-marker
/// contribution.
const DECORATOR_MANIFEST: &str = r#"
[plugin]
id = "org.norte.decor"
name = "Decor"
publisher = "norte"
version = "0.1.0"
category = "decorator"

[[contributions.decorator]]
"#;

#[test]
fn a_decorator_manifest_parses() {
    let m = Manifest::from_toml(DECORATOR_MANIFEST).unwrap();
    assert_eq!(m.category, Category::Decorator);
    assert_eq!(m.contributions.decorator.len(), 1);
}

#[test]
fn category_decorator_as_str_is_kebab() {
    assert_eq!(Category::Decorator.as_str(), "decorator");
}

/// ADR 0037: `contributions.decorator` follows `[config]`'s OPTIONAL
/// pattern (`update_decorator_digest`) — a manifest WITHOUT
/// `[[contributions.decorator]]` must digest EXACTLY the same as a
/// manifest from before this category (no existing human approval is
/// reset just because the field exists).
#[test]
fn a_manifest_without_decorator_digests_the_same_as_before_the_field() {
    // `command` is the SAME shape as `TOCTOU_BEFORE`/`CMD_MANIFEST` used
    // in other suites: without `[[contributions.decorator]]`, the `Vec`
    // is empty because of `#[serde(default)]` — the general case for ANY
    // pre-existing manifest.
    let without_decorator = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        [capabilities]
        fs-read = "scoped"
    "#,
    )
    .unwrap();
    assert!(without_decorator.contributions.decorator.is_empty());
    // The digest does not depend on whether the type EXISTS, only on
    // whether the section gets populated: repeating the computation
    // (determinism) confirms there is no phantom byte sneaking in just
    // because the field is present in the struct.
    assert_eq!(
        without_decorator.approval_digest(),
        without_decorator.approval_digest()
    );
}

/// Phase 3 of the 2026-09-15 program: `contributions.panel` follows the
/// same OPTIONAL pattern as `decorator` and `[config]`.
///
/// A manifest WITHOUT `[[contributions.panel]]` has to digest exactly
/// what it digested before the category existed. Otherwise, the field's
/// mere appearance would reset the approvals the reader already gave,
/// and the manager would ask them to consent again to eight plugins that
/// have not changed.
#[test]
fn a_manifest_without_panel_digests_the_same_as_before_the_field() {
    let without_panel = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        [capabilities]
        fs-read = "scoped"
    "#,
    )
    .unwrap();
    assert!(without_panel.contributions.panel.is_empty());
    assert_eq!(
        without_panel.approval_digest(),
        without_panel.approval_digest()
    );
}

/// And declaring a panel DOES move it: which slot a plugin occupies, what
/// it is called in the bar and how much screen it asks for are part of
/// what gets approved.
#[test]
fn a_present_panel_moves_the_approval_digest() {
    let base = r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "panel"
    "#;
    let without = Manifest::from_toml(base).unwrap();
    let with = Manifest::from_toml(&format!(
        "{base}\n[[contributions.panel]]\nkind = \"git\"\ntitle = \"Git\"\nmin-cols = 24\nmin-rows = 6\n"
    ))
    .unwrap();
    assert_ne!(without.approval_digest(), with.approval_digest());

    // And the SIZE too: a panel that after approval asks for half the
    // screen is not the panel that was approved.
    let bigger = Manifest::from_toml(&format!(
        "{base}\n[[contributions.panel]]\nkind = \"git\"\ntitle = \"Git\"\nmin-cols = 60\nmin-rows = 6\n"
    ))
    .unwrap();
    assert_ne!(with.approval_digest(), bigger.approval_digest());
}

#[test]
fn a_present_decorator_moves_the_approval_digest() {
    let without = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "decorator"
    "#,
    )
    .unwrap();
    let with = Manifest::from_toml(
        DECORATOR_MANIFEST
            .replace("org.norte.decor", "org.norte.x")
            .as_str(),
    )
    .unwrap();
    assert_ne!(
        without.approval_digest(),
        with.approval_digest(),
        "declaring [[contributions.decorator]] must move the digest (new trigger, new surface)"
    );
}

/// ADR 0105: a written `slot = "badge"` digests THE SAME as without
/// `slot` (no earlier decorator changes its anchor); `slot = "icon"`
/// moves the digest, because it changes where the plugin paints; and the
/// slot goes WITH its position, because the core reads the first
/// contribution's and reordering two blocks cannot silently move an
/// approved plugin from one slot to the other.
#[test]
fn a_decorators_slot_digests_only_when_it_is_icon_and_with_its_position() {
    let with = |contribs: &str| {
        Manifest::from_toml(&format!(
            r#"
            [plugin]
            id = "org.norte.x"
            name = "X"
            publisher = "norte"
            version = "0.1.0"
            category = "decorator"
            {contribs}
        "#
        ))
        .unwrap()
        .approval_digest()
    };
    let without_slot = with("[[contributions.decorator]]");
    let badge = with("[[contributions.decorator]]\nslot = \"badge\"");
    let icon = with("[[contributions.decorator]]\nslot = \"icon\"");
    assert_eq!(
        without_slot, badge,
        "the usual slot does not move the anchor"
    );
    assert_ne!(without_slot, icon, "moving to the icon column does");
    let icon_first = with(
        "[[contributions.decorator]]\nslot = \"icon\"\n[[contributions.decorator]]\nslot = \"badge\"",
    );
    let icon_second = with(
        "[[contributions.decorator]]\nslot = \"badge\"\n[[contributions.decorator]]\nslot = \"icon\"",
    );
    assert_ne!(
        icon_first, icon_second,
        "reordering the blocks changes which slot the core reads, and the digest notices"
    );
    // And a slot this build does not know REJECTS the manifest: in the
    // manifest, we are strict about what a human wrote; on the wire,
    // tolerant of what a peer sends (there it falls back to `badge`).
    assert!(
        Manifest::from_toml(
            r#"
            [plugin]
            id = "org.norte.x"
            name = "X"
            publisher = "norte"
            version = "0.1.0"
            category = "decorator"
            [[contributions.decorator]]
            slot = "corner"
        "#
        )
        .is_err()
    );
}

#[test]
fn category_decorator_moves_the_approval_digest_versus_another_category() {
    // Same criterion as
    // `approval_digest_includes_category_and_contributions_not_just_capabilities`:
    // changing ONLY the category (without touching capabilities) must
    // move the digest.
    let command = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "command"
        [capabilities]
        fs-read = "scoped"
    "#,
    )
    .unwrap();
    let decorator = Manifest::from_toml(
        r#"
        [plugin]
        id = "org.norte.x"
        name = "X"
        publisher = "norte"
        version = "0.1.0"
        category = "decorator"
        [capabilities]
        fs-read = "scoped"
    "#,
    )
    .unwrap();
    assert_eq!(
        command.capabilities.digest(),
        decorator.capabilities.digest(),
        "the capabilities are identical (test control)"
    );
    assert_ne!(command.approval_digest(), decorator.approval_digest());
}

#[test]
fn catalog_by_category_includes_decorator() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.decor", DECORATOR_MANIFEST);
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);

    let cat = Catalog::load_dir(root.path());
    assert_eq!(cat.errors.len(), 0, "{:?}", cat.errors);
    let groups = cat.by_category();
    let cats: Vec<Category> = groups.iter().map(|(c, _)| *c).collect();
    assert_eq!(cats, vec![Category::Previewer, Category::Decorator]);
}

/// The `ai` capability used to parse, go into the digest and paint a
/// badge, and no host read it: there is no AI WIT interface nor a place
/// that links it. A human would approve "AI access" and grant nothing —
/// ADR 0088's lie, with the hooks' signature. Same remedy: rejected while
/// parsing, with the reason, and the field stays because spec §7.1 names
/// it.
#[test]
fn the_ai_capability_is_rejected_because_nobody_honors_it() {
    let src = r#"
        [plugin]
        id = "org.demo.oracle"
        name = "Oracle"
        publisher = "demo"
        version = "0.1.0"
        category = "command"
        [capabilities]
        ai = "chat"
    "#;
    assert!(matches!(
        Manifest::from_toml(src),
        Err(ManifestError::AiNotImplemented)
    ));
    // Without the promise, the same plugin goes through.
    let without_ai = src.replace(r#"ai = "chat""#, "");
    assert!(Manifest::from_toml(&without_ai).is_ok());
}

/// A provider plugin serves the scheme it declares — and that makes the
/// scheme a name that can SPOOF: `file`, `sftp` and `s3` are served by
/// the core and a plugin claiming them would be putting itself in front
/// of a provider with trash, resume and TLS. Schemes with `+` are archive
/// composition (ADR 0018) and are not given up either.
#[test]
fn a_provider_cannot_claim_a_core_scheme() {
    for reserved in [
        "file",
        "sftp",
        "ftp",
        "s3",
        "zip+sftp",
        "tar+gz+file",
        "rar",
        "foo+bar",
    ] {
        let src = format!(
            r#"
            [plugin]
            id = "org.demo.usurper"
            name = "Usurper"
            publisher = "demo"
            version = "0.1.0"
            category = "provider"
            [[contributions.provider]]
            scheme = "{reserved}"
        "#
        );
        assert!(
            matches!(
                Manifest::from_toml(&src),
                Err(ManifestError::ReservedScheme)
            ),
            "{reserved} should be reserved"
        );
    }
    // And a scheme that is not even a scheme name (uppercase, slashes,
    // empty) is rejected through the same gate: what reaches the
    // connector has to be what a VPath can carry.
    for bad in ["", "Web-DAV", "a/b", "x y"] {
        let src = format!(
            r#"
            [plugin]
            id = "org.demo.usurper"
            name = "Usurper"
            publisher = "demo"
            version = "0.1.0"
            category = "provider"
            [[contributions.provider]]
            scheme = "{bad}"
        "#
        );
        assert!(
            matches!(
                Manifest::from_toml(&src),
                Err(ManifestError::ReservedScheme)
            ),
            "{bad:?} is not a scheme"
        );
    }
}

/// A guest compiled against another WIT version does not load: it is
/// listed in `errors` with BOTH versions (ADR 0094). With the binary
/// intact it goes into `plugins`. The old one is built by rewriting
/// `@0.10.0` to `@0.70.0` in the real guest's bytes (a version the host
/// does not serve for any package; same length, so the sections stay
/// valid).
#[test]
fn a_guest_built_against_another_wit_is_listed_as_broken() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let bytes = std::fs::read(wasm).unwrap();
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);
    let wasm_path = root.path().join("org.norte.syntax-preview/plugin.wasm");

    std::fs::write(&wasm_path, &bytes).unwrap();
    let cat = Catalog::load_dir(root.path());
    assert!(cat.errors.is_empty(), "{:?}", cat.errors);
    assert_eq!(cat.plugins.len(), 1, "the current guest loads");

    let old = support::rewrite_bytes(&bytes, b"@0.10.0", b"@0.70.0");
    std::fs::write(&wasm_path, old).unwrap();
    let cat = Catalog::load_dir(root.path());
    assert!(cat.plugins.is_empty(), "it does not load");
    assert_eq!(cat.errors.len(), 1);
    match &cat.errors[0].error {
        ManifestError::WitMismatch {
            package,
            built_against,
            served,
        } => {
            assert_eq!(package, "norte:plugin");
            assert_eq!(built_against, "0.70.0");
            assert_eq!(served, "0.10.0");
        }
        other => panic!("expected WitMismatch, got {other:?}"),
    }
}

/// A `plugin.wasm` above the artifact cap is NOT read: it is listed as
/// broken with its size, without materializing it. A sparse file of
/// several GiB installs for free, and reading it whole on every
/// discovery would bring down the catalog, not a plugin.
#[test]
fn a_binary_above_the_cap_is_listed_as_broken_without_reading_it() {
    let root = tempfile::tempdir().unwrap();
    write_plugin(root.path(), "org.norte.syntax-preview", SYNTAX_PREVIEW);
    let wasm = root.path().join("org.norte.syntax-preview/plugin.wasm");
    let f = std::fs::File::create(&wasm).unwrap();
    // Sparse: takes up nothing, measures more.
    f.set_len(norte_plugin_host::MAX_ARTIFACT_BYTES + 1)
        .unwrap();
    drop(f);

    let cat = Catalog::load_dir(root.path());
    assert!(cat.plugins.is_empty());
    assert_eq!(cat.errors.len(), 1);
    match &cat.errors[0].error {
        ManifestError::ArtifactTooLarge { len, cap } => {
            assert_eq!(*len, norte_plugin_host::MAX_ARTIFACT_BYTES + 1);
            assert_eq!(*cap, norte_plugin_host::MAX_ARTIFACT_BYTES);
        }
        other => panic!("expected ArtifactTooLarge, got {other:?}"),
    }
}

/// A provider plugin receives network access to `ip:port`, never the
/// whole IP, and the host does not know a foreign scheme's default port:
/// the contribution declares it. It goes into the digest — changing it
/// changes what network access is granted.
#[test]
fn a_providers_default_port_is_declared_and_goes_into_the_digest() {
    let with = r#"
        [plugin]
        id = "org.demo.dav"
        name = "DAV"
        publisher = "demo"
        version = "0.1.0"
        category = "provider"
        [[contributions.provider]]
        scheme = "webdav"
        default-port = 8443
    "#;
    let m = Manifest::from_toml(with).unwrap();
    assert_eq!(m.contributions.provider[0].default_port, Some(8443));
    let without = with.replace("default-port = 8443", "");
    let m2 = Manifest::from_toml(&without).unwrap();
    assert_eq!(m2.contributions.provider[0].default_port, None);
    assert_ne!(m.approval_digest(), m2.approval_digest());
}
