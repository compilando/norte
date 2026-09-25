//! The shape of `HostCatalog` on the wire, pinned.
//!
//! It is the fifth message that crosses to the webview and the only one that
//! does not live in `norte-ui-host`, so the bridge's golden corpus did not
//! cover it: a field renamed here left the renderer reading `undefined`
//! without anything turning red (#259). It still lives in this crate — what
//! it carries is already-translated strings and colors, the painter's
//! business — but now with a net.

use norte_gui_tauri::catalog::HostCatalog;

/// The fixture: the exact fields `ui/src/types.ts` declares.
const FIXTURE: &str = r##"{
  "bridge_version": 36,
  "instance_id": "host-1",
  "locale": "es",
  "strings": { "hostile-name": "nombre alterado" },
  "theme": { "bg": "#101216" },
  "measure": false,
  "busy_threshold_ms": 250,
  "appearance": {
    "font": "Inter",
    "mono_font": "Iosevka",
    "font_size": 15.0,
    "reduce_motion": true,
    "custom_titlebar": true
  },
  "first_run": false,
  "no_splash": false
}"##;

/// Round trip: the JSON's names are the contract, not Rust's.
#[test]
fn the_catalog_goes_and_returns_with_the_same_fields() {
    let read: HostCatalog = serde_json::from_str(FIXTURE).expect("the fixture deserializes");
    assert_eq!(read.instance_id, "host-1");
    assert_eq!(read.locale, "es");
    assert_eq!(read.strings["hostile-name"], "nombre alterado");
    assert_eq!(read.theme["bg"], "#101216");
    assert!(!read.measure);
    // The wait threshold travels: writing it into the CSS would be a third
    // place where the same number lives, and the first place to forget it.
    assert_eq!(
        u128::from(read.busy_threshold_ms),
        norte_frontend::busy::THRESHOLD.as_millis(),
        "the catalogue carries the SHARED threshold, not a copy"
    );

    // The four `[ui]` keys that were loaded, validated, offered on the
    // settings screen and read by nobody. `reduce_motion` is also an
    // accessibility commitment from spec §17.
    assert_eq!(read.appearance.font.as_deref(), Some("Inter"));
    assert_eq!(read.appearance.mono_font.as_deref(), Some("Iosevka"));
    assert_eq!(read.appearance.font_size, Some(15.0));
    assert_eq!(read.appearance.reduce_motion, Some(true));
    // The window's own title bar (ADR 0136): from startup, and the renderer
    // needs it to turn the menu bar into the title bar.
    assert!(read.appearance.custom_titlebar);

    let return_: serde_json::Value = serde_json::to_value(&read).expect("serializes");
    let expected: serde_json::Value = serde_json::from_str(FIXTURE).expect("json");
    assert_eq!(
        return_, expected,
        "the catalogue has to come back with the SAME fields: a rename \
         here leaves the renderer reading `undefined`"
    );
}

/// The number the catalogue carries is the host's, not a hand copy.
///
/// It is informational — compatibility is decided by the renderer over the
/// ENVELOPE it is about to interpret — but a number that falls behind in the
/// one message a human looks at when diagnosing is worse than not carrying
/// it at all.
#[test]
fn the_catalog_carries_the_host_version() {
    let theme = norte_theme::Theme::default();
    let instance = norte_ui_host::InstanceId::new("host-1".to_owned());
    let cat = norte_gui_tauri::catalog::catalog(&instance, norte_i18n::Lang::Es, &theme);
    assert_eq!(cat.bridge_version, norte_ui_host::BRIDGE_VERSION);
}

/// The mark rule's spans (ADR 0135): the renderer divides them with the same
/// number the host used to split the listing. With a different one, marks
/// paint offset and nothing turns red.
#[test]
fn the_marks_rule_counts_the_same_segments_on_both_sides() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let tipos = std::fs::read_to_string(root.join("ui/src/types.ts")).expect("types.ts");
    let expected = format!(
        "export const MARK_RULER_SPANS = {};",
        norte_ui_host::dto::MARK_RULER_SPANS
    );
    assert!(
        tipos.contains(&expected),
        "`ui/src/types.ts` does not declare `{expected}`"
    );
}
