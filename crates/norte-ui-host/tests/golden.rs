//! The bridge's contract, nailed down.
//!
//! A renderer not written in Rust does not share types with the host: it
//! shares JSON. These fixtures are that agreement, and breaking them by
//! accident is exactly the failure they protect against — a field renamed in
//! Rust that leaves the renderer reading `null` with nothing turning red.
//!
//! Coverage is 1:1 between this file's fixtures and cases, and ON TOP OF
//! THAT `tag_de_accion` is an exhaustive `match` with no wildcard: adding a
//! variant to `UiAction` stops this from compiling. Without that the promise
//! was false — the JSON was compared against a hand-written list, not
//! against the enum — and `search_activate_row` had already slipped through,
//! crossing the wire with no fixture while this very header said that was
//! impossible.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::Path;

use norte_ui_host::action::{DialogFieldValue, ExtensionChange, UiAction};
use norte_ui_host::bridge::{ActionAck, BridgeEnvelope, InstanceId, ModalId, RowKey, StaleAction};
use norte_ui_host::dto::{
    BrowserSlotView, CellView, ColumnHeader, ConnectionView, DialogChoice, DialogView, LayoutView,
    RowKind, RowView, SlotPlacement, SlotRole, SlotState, SlotView, StatusView, TaskStateView,
    TaskView, UiNotice, UiUpdate, ViewChange, ViewPatch, ViewSnapshot,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn load(name: &str) -> BTreeMap<String, Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("valid fixture JSON")
}

/// 1:1 coverage between fixtures and cases, and an exact match both ways.
/// Rewrites a family's fixtures from the Rust cases.
///
/// Only with `NORTE_BLESS=1`, and on purpose: the corpus IS the agreement
/// with a renderer that shares no types, so regenerating it has to be an
/// explicit act that shows up in the diff. Without this, a new field forced
/// hand-patching the JSON — and by hand is where values matching no case
/// slip in.
fn bless<T: Serialize>(file: &str, cases: &[(&str, T)]) {
    let mut mapa = serde_json::Map::new();
    for (nombre, valor) in cases {
        mapa.insert(
            (*nombre).to_owned(),
            serde_json::to_value(valor).expect("serializable"),
        );
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(file);
    let texto = serde_json::to_string_pretty(&Value::Object(mapa)).expect("json");
    std::fs::write(&path, texto + "\n").expect("write the fixture");
}

fn check_family<T>(file: &str, cases: &[(&str, T)])
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    if std::env::var_os("NORTE_BLESS").is_some_and(|v| v == "1") {
        bless(file, cases);
    }
    let fixtures = load(file);
    let fixture_names: Vec<&str> = fixtures.keys().map(String::as_str).collect();
    let mut case_names: Vec<&str> = cases.iter().map(|(n, _)| *n).collect();
    case_names.sort_unstable();
    assert_eq!(
        fixture_names, case_names,
        "[{file}] Rust cases and fixtures must cover each other 1:1"
    );
    for (name, value) in cases {
        let expected = &fixtures[*name];
        assert_eq!(
            &serde_json::to_value(value).expect("serializable"),
            expected,
            "[{file}/{name}] serialize"
        );
        let back: T = serde_json::from_value(expected.clone())
            .unwrap_or_else(|e| panic!("[{file}/{name}] deserialize: {e}"));
        assert_eq!(&back, value, "[{file}/{name}] deserialize == constructed");
    }
}

fn fila(key: u64, nombre: &str, hostile: bool) -> RowView {
    RowView {
        key: RowKey(key),
        display_name: nombre.to_owned(),
        hostile,
        // No task works on this row (bridge 69).
        progress: None,
        kind: RowKind::File,
        selected: false,
        marked: false,
        cells: vec![CellView {
            column: "size".to_owned(),
            text: Some("1.2 KiB".to_owned()),
        }],
        badge: String::new(),
        badge_hostile: false,
        badge_role: String::new(),
        icon: String::new(),
        icon_hostile: false,
        name_color: String::new(),
        name_bold: false,
        name_dim: false,
        name_italic: false,
        name_underline: false,
    }
}

/// The same row, with the badge one plugin put on it, and the icon another
/// one put on it (bridge 62): both slots in one row.
///
/// The badge, its role and the icon cross JSON here and nowhere else: they
/// are what a THIRD PARTY paints attached to a file name.
fn fila_adornada(key: u64, nombre: &str) -> RowView {
    RowView {
        badge: "M".to_owned(),
        badge_hostile: false,
        badge_role: "warning".to_owned(),
        icon: "🦀".to_owned(),
        icon_hostile: false,
        ..fila(key, nombre, false)
    }
}

#[test]
fn acciones() {
    let mut casos = acciones_de_fila();
    casos.extend(acciones_de_overlay());
    casos.extend(acciones_de_pantalla());
    // Each case is named after its variant: that is what makes the COMPILER,
    // not a list, watch over coverage.
    for (nombre, accion) in &casos {
        assert_eq!(
            *nombre,
            tag_de_accion(accion),
            "case `{nombre}` is not named after its variant"
        );
    }
    check_family("actions.json", &casos);
}

/// Each action's tag, in an EXHAUSTIVE `match` with no wildcard.
///
/// It is the guard that was missing. `check_family` compares the fixtures
/// against a hand-written list of cases, so a new variant with no case would
/// pass with nothing saying anything — and it did: `SearchActivateRow`
/// crossed the wire with no fixture. With this, adding a variant breaks this
/// file's compilation, which is where it needs to be noticed.
fn tag_de_accion(a: &UiAction) -> &'static str {
    match a {
        UiAction::MoveCursor { .. } => "move_cursor",
        UiAction::SelectRow { .. } => "select_row",
        UiAction::ToggleMark { .. } => "toggle_mark",
        UiAction::MarkRange { .. } => "mark_range",
        UiAction::Activate { .. } => "activate",
        UiAction::Parent { .. } => "parent",
        UiAction::BreadcrumbActivate { .. } => "breadcrumb_activate",
        UiAction::History { .. } => "history",
        UiAction::SetVisibleRange { .. } => "set_visible_range",
        UiAction::SortBy { .. } => "sort_by",
        UiAction::ResizeColumn { .. } => "resize_column",
        UiAction::FocusSlot { .. } => "focus_slot",
        UiAction::Dialog { .. } => "dialog",
        UiAction::DialogInput { .. } => "dialog_input",
        UiAction::DialogField { .. } => "dialog_field",
        UiAction::DirectoryPicked { .. } => "directory_picked",
        UiAction::WindowFocus { .. } => "window_focus",
        UiAction::FilesDropped { .. } => "files_dropped",
        UiAction::PanelClick { .. } => "panel_click",
        UiAction::TreeActivateRow { .. } => "tree_activate_row",
        UiAction::TreeToggleRow { .. } => "tree_toggle_row",
        UiAction::RefreshSlot { .. } => "refresh_slot",
        UiAction::LogSetLevel { .. } => "log_set_level",
        UiAction::LogSetFilter { .. } => "log_set_filter",
        UiAction::LogScroll { .. } => "log_scroll",
        UiAction::ProgramFinished { .. } => "program_finished",
        UiAction::PreviewScroll { .. } => "preview_scroll",
        UiAction::ViewerScroll { .. } => "viewer_scroll",
        UiAction::LogFollow => "log_follow",
        UiAction::LogCycleSource => "log_cycle_source",
        UiAction::LogSetVisibleRange { .. } => "log_set_visible_range",
        UiAction::CancelTask { .. } => "cancel_task",
        UiAction::CompareSelectRow { .. } => "compare_select_row",
        UiAction::CompareActivateRow { .. } => "compare_activate_row",
        UiAction::CompareToggleFilter { .. } => "compare_toggle_filter",
        UiAction::CompareSetVisibleRange { .. } => "compare_set_visible_range",
        UiAction::SetViewport { .. } => "set_viewport",
        UiAction::SetColorScheme { .. } => "set_color_scheme",
        UiAction::Key(_) => "key",
        UiAction::SetViewerRows { .. } => "set_viewer_rows",
        UiAction::SetViewerCols { .. } => "set_viewer_cols",
        UiAction::HelpSelectTopic { .. } => "help_select_topic",
        UiAction::HelpActivate { .. } => "help_activate",
        UiAction::SettingsSelectRow { .. } => "settings_select_row",
        UiAction::SettingsActivate { .. } => "settings_activate",
        UiAction::SettingsQuery { .. } => "settings_query",
        UiAction::SettingsSet { .. } => "settings_set",
        UiAction::SettingsJumpSection { .. } => "settings_jump_section",
        UiAction::SettingsReset { .. } => "settings_reset",
        UiAction::ExtensionSelectRow { .. } => "extension_select_row",
        UiAction::ExtensionGovern { .. } => "extension_govern",
        UiAction::ExtensionHelp { .. } => "extension_help",
        UiAction::AgentSelectRow { .. } => "agent_select_row",
        UiAction::SelectTab { .. } => "select_tab",
        UiAction::PickerSelectRow { .. } => "picker_select_row",
        UiAction::PlaceActivateRow { .. } => "place_activate_row",
        UiAction::LayoutActivateRow { .. } => "layout_activate_row",
        UiAction::SearchActivateRow { .. } => "search_activate_row",
        UiAction::AiRenameDecide { .. } => "ai_rename_decide",
        UiAction::OrganizeDecide { .. } => "organize_decide",
        UiAction::OrganizeScroll { .. } => "organize_scroll",
        UiAction::HandoffFailed { .. } => "handoff_failed",
        UiAction::MenuOpen { .. } => "menu_open",
        UiAction::MenuPointRow { .. } => "menu_point_row",
        UiAction::MenuActivateRow { .. } => "menu_activate_row",
        UiAction::MenuClose => "menu_close",
        UiAction::MenuToggle => "menu_toggle",
        UiAction::WizardOpen => "wizard_open",
        UiAction::SplashOpen => "splash_open",
        UiAction::SplashClose => "splash_close",
        UiAction::SplashActivateRow { .. } => "splash_activate_row",
        UiAction::WizardActivateRow { .. } => "wizard_activate_row",
        UiAction::PanelBarActivate { .. } => "panel_bar_activate",
        UiAction::StatusItemActivate { .. } => "status_item_activate",
        UiAction::LayoutButtonActivate { .. } => "layout_button_activate",
        UiAction::TabAction { .. } => "tab_action",
        UiAction::ResizeSlot { .. } => "resize_slot",
        UiAction::MoveSlot { .. } => "move_slot",
        UiAction::ProfileActivateRow { .. } => "profile_activate_row",
        UiAction::Resync => "resync",
        UiAction::RequestQuit => "request_quit",
    }
}

/// The ones that name a row: they carry key AND generation (ADR 0068).
///
/// Grows with every new action, and that is what it should do: like
/// [`slots_de_referencia`], it is ONE list of literals with no logic inside,
/// and splitting it up would hide exactly what this file shows at a glance.
#[expect(clippy::too_many_lines, reason = "list of literals, no logic")]
fn acciones_de_fila() -> Vec<(&'static str, UiAction)> {
    vec![
        (
            "activate",
            UiAction::Activate {
                slot_id: 1,
                key: RowKey(9),
                generation: 4,
            },
        ),
        ("cancel_task", UiAction::CancelTask { task_id: 42 }),
        // #326: the log panel's five. On a bridge that bumps its number, new
        // wire names are the first thing that needs nailing down — and
        // `tag_de_accion` is not enough: with the case list and the fixtures
        // BOTH empty, `check_family` covers them 1:1 and says nothing.
        ("refresh_slot", UiAction::RefreshSlot { slot_id: 1 }),
        (
            "log_set_level",
            UiAction::LogSetLevel {
                level: "debug".to_owned(),
            },
        ),
        (
            "log_set_filter",
            UiAction::LogSetFilter {
                filter: "connect".to_owned(),
            },
        ),
        ("log_scroll", UiAction::LogScroll { delta: -3 }),
        (
            "panel_click",
            UiAction::PanelClick {
                slot_id: 13,
                // Cells INSIDE the frame: the renderer has already
                // subtracted the border, and the host works out which zone
                // it was from them. The command does not travel.
                row: 2,
                col: 7,
            },
        ),
        (
            "program_finished",
            UiAction::ProgramFinished {
                title_key: "program-output-compare".to_owned(),
                command: "/usr/bin/diff -u a.txt b.txt".to_owned(),
                output: b"--- a.txt\n+++ b.txt\n".to_vec(),
                truncated: false,
                failed: false,
            },
        ),
        (
            "preview_scroll",
            UiAction::PreviewScroll {
                slot_id: 11,
                delta: 3,
            },
        ),
        ("log_follow", UiAction::LogFollow),
        ("log_cycle_source", UiAction::LogCycleSource),
        (
            "log_set_visible_range",
            UiAction::LogSetVisibleRange { rows: 12 },
        ),
        ("compare_select_row", UiAction::CompareSelectRow { id: 7 }),
        (
            "compare_activate_row",
            UiAction::CompareActivateRow { id: 7 },
        ),
        (
            "compare_toggle_filter",
            UiAction::CompareToggleFilter {
                category: "different".to_owned(),
            },
        ),
        (
            "compare_set_visible_range",
            UiAction::CompareSetVisibleRange {
                first: 40,
                count: 20,
            },
        ),
        (
            "dialog",
            UiAction::Dialog {
                id: ModalId(3),
                choice: "confirm".to_owned(),
                secret: None,
            },
        ),
        (
            "dialog_input",
            UiAction::DialogInput {
                id: ModalId(3),
                text: "carpeta nueva".to_owned(),
            },
        ),
        // A FORM field (bridge 91). The text one is the one that carries a
        // payload; the switch and the cycle only say they were touched.
        (
            "dialog_field",
            UiAction::DialogField {
                id: ModalId(3),
                field: "min-size".to_owned(),
                value: DialogFieldValue::Text {
                    text: "1M".to_owned(),
                },
            },
        ),
        // The desktop picker's return trip (#284). One case per variant,
        // which is the rule above; the `path: null` of a picker closed
        // without choosing is covered by `un_selector_cancelado_viaja_como_null`.
        (
            "directory_picked",
            UiAction::DirectoryPicked {
                path: Some("/home/oscar/destino".to_owned()),
            },
        ),
        // `false` and not `true`: it is the value that CHANGES something.
        // With the window focused, the host behaves as it did before #285.
        ("window_focus", UiAction::WindowFocus { focused: false }),
        // What arrives from a desktop drop (#283): a LIST, because dragging
        // several at once is the normal case, and native text without
        // conversion — the host does the `VPath` conversion.
        (
            "files_dropped",
            UiAction::FilesDropped {
                paths: vec![
                    "/home/oscar/uno.txt".to_owned(),
                    "/home/oscar/dos.txt".to_owned(),
                ],
            },
        ),
    ]
}

/// The three changes of `extension_govern` (bridge 61), by their wire name.
/// Kept out of the golden family because there only one case per variant
/// fits, and the other two are just as easy to get wrong.
#[test]
fn los_tres_cambios_de_una_extension_viajan_por_su_nombre() {
    for (change, nombre) in [
        (ExtensionChange::Approval, "approval"),
        (ExtensionChange::Enabled, "enabled"),
        (ExtensionChange::Uninstall, "uninstall"),
    ] {
        let a = UiAction::ExtensionGovern {
            row: 0,
            id: "acme.ftp".to_owned(),
            change,
        };
        let json = serde_json::to_value(&a).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "action": "extension_govern", "row": 0, "id": "acme.ftp", "change": nombre
            })
        );
        let back: UiAction = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, a);
    }
}

/// Closing the picker without choosing travels as `null`, and comes back as
/// `None` (#284). Kept out of the golden family because there only one case
/// per variant fits — but the shape on the wire matters just the same: a
/// renderer that sent `""` would be naming the root.
#[test]
fn un_selector_cancelado_viaja_como_null() {
    let a = UiAction::DirectoryPicked { path: None };
    let json = serde_json::to_value(&a).expect("serialize");
    assert_eq!(
        json,
        serde_json::json!({"action": "directory_picked", "path": null})
    );
    let vuelta: UiAction = serde_json::from_value(json).expect("deserialize");
    assert!(matches!(vuelta, UiAction::DirectoryPicked { path: None }));
}

/// The ones that name an OVERLAY row by its index.
///
/// Two carry generation — the sidebar and the picker fill from a background
/// task, so their list changes without the user touching anything — and the
/// rest do not, because they cannot change without a gesture from them.
fn acciones_de_overlay() -> Vec<(&'static str, UiAction)> {
    vec![
        (
            "extension_select_row",
            UiAction::ExtensionSelectRow { row: 1 },
        ),
        // Bridge 61: the button. `uninstall` and not `approval` because it
        // is the value whose wire name is hardest to fix later: a renderer
        // that misspells it uninstalls nothing, silently.
        (
            "extension_govern",
            UiAction::ExtensionGovern {
                row: 1,
                id: "org.norte.demo".to_owned(),
                change: ExtensionChange::Uninstall,
            },
        ),
        (
            "extension_help",
            UiAction::ExtensionHelp {
                row: 1,
                id: "org.norte.demo".to_owned(),
            },
        ),
        ("select_tab", UiAction::SelectTab { slot_id: 3 }),
        (
            "agent_select_row",
            UiAction::AgentSelectRow {
                row: 1,
                generation: 4,
            },
        ),
        (
            "picker_select_row",
            UiAction::PickerSelectRow {
                row: 0,
                generation: 3,
            },
        ),
        (
            "place_activate_row",
            UiAction::PlaceActivateRow {
                row: 1,
                generation: 4,
            },
        ),
        (
            "tree_activate_row",
            UiAction::TreeActivateRow {
                row: 2,
                generation: 6,
            },
        ),
        (
            "tree_toggle_row",
            UiAction::TreeToggleRow {
                row: 2,
                generation: 6,
            },
        ),
        (
            "layout_activate_row",
            UiAction::LayoutActivateRow { row: 0 },
        ),
        (
            "search_activate_row",
            UiAction::SearchActivateRow { row: 2 },
        ),
        ("help_select_topic", UiAction::HelpSelectTopic { row: 3 }),
        ("help_activate", UiAction::HelpActivate { index: 1 }),
        (
            "profile_activate_row",
            UiAction::ProfileActivateRow {
                row: 1,
                generation: 3,
            },
        ),
    ]
    .into_iter()
    .chain(acciones_de_ajustes())
    .chain(acciones_de_cromo())
    .collect()
}

/// The SETTINGS screen's, kept apart because there are six of them and the
/// overlay list was going over the lint's line cap.
fn acciones_de_ajustes() -> Vec<(&'static str, UiAction)> {
    vec![
        (
            "settings_select_row",
            UiAction::SettingsSelectRow { row: 2 },
        ),
        ("settings_activate", UiAction::SettingsActivate { row: 2 }),
        (
            "settings_query",
            UiAction::SettingsQuery {
                text: "@modified fira".to_owned(),
            },
        ),
        (
            "settings_jump_section",
            UiAction::SettingsJumpSection {
                section: "open-with".to_owned(),
            },
        ),
        ("settings_reset", UiAction::SettingsReset { row: 2 }),
        // SET, not cycle: by the catalogue's id and with the value inside.
        (
            "settings_set",
            UiAction::SettingsSet {
                id: "ui.theme".to_owned(),
                value: "nord".to_owned(),
            },
        ),
    ]
}

/// The CHROME's: menus, bars, the wizard and the splash screen.
///
/// Separated from the overlay ones only by size — a hundred-line list is not
/// readable — and along that seam and no other: these hang off something
/// that is always in view, not off a screen that opens.
fn acciones_de_cromo() -> Vec<(&'static str, UiAction)> {
    vec![
        ("menu_open", UiAction::MenuOpen { menu: 2 }),
        ("menu_point_row", UiAction::MenuPointRow { row: 3 }),
        ("menu_activate_row", UiAction::MenuActivateRow { row: 3 }),
        ("menu_close", UiAction::MenuClose),
        ("wizard_open", UiAction::WizardOpen),
        ("splash_open", UiAction::SplashOpen),
        ("splash_close", UiAction::SplashClose),
        (
            "splash_activate_row",
            UiAction::SplashActivateRow { number: 3 },
        ),
        (
            "wizard_activate_row",
            UiAction::WizardActivateRow { row: 1 },
        ),
        (
            "panel_bar_activate",
            UiAction::PanelBarActivate { button: 2 },
        ),
        (
            "status_item_activate",
            UiAction::StatusItemActivate {
                id: "sort".to_owned(),
            },
        ),
        (
            "layout_button_activate",
            UiAction::LayoutButtonActivate {
                id: "split-h".to_owned(),
            },
        ),
        (
            "tab_action",
            UiAction::TabAction {
                slot_id: 3,
                verb: norte_ui_host::TabVerb::Close,
            },
        ),
        (
            "resize_slot",
            UiAction::ResizeSlot {
                slot_id: 1,
                cells: 42,
            },
        ),
        (
            "move_slot",
            UiAction::MoveSlot {
                slot_id: 1,
                target: 2,
                zone: norte_frontend::layout::DropZone::Center,
            },
        ),
    ]
}

/// The rest: screen, keyboard, dialogs and tasks.
#[expect(clippy::too_many_lines, reason = "list of literals, no logic")]
fn acciones_de_pantalla() -> Vec<(&'static str, UiAction)> {
    vec![
        ("focus_slot", UiAction::FocusSlot { slot_id: 2 }),
        (
            "key",
            UiAction::Key(norte_ui_host::keys::KeyInput {
                key: "ArrowDown".to_owned(),
                ctrl: false,
                alt: false,
                shift: false,
                meta: false,
            }),
        ),
        (
            "history",
            UiAction::History {
                slot_id: 1,
                back: true,
            },
        ),
        (
            "mark_range",
            UiAction::MarkRange {
                slot_id: 1,
                from: RowKey(2),
                to: RowKey(5),
                generation: 4,
            },
        ),
        ("set_viewer_rows", UiAction::SetViewerRows { rows: 40 }),
        // The wheel over the viewer, with both axes: one single gesture
        // produces them (`shift` goes sideways).
        (
            "viewer_scroll",
            UiAction::ViewerScroll { lines: 3, cols: -8 },
        ),
        ("set_viewer_cols", UiAction::SetViewerCols { cols: 110 }),
        (
            "set_viewport",
            UiAction::SetViewport {
                width: 120,
                height: 40,
            },
        ),
        ("set_color_scheme", UiAction::SetColorScheme { dark: true }),
        // Here and not next to the rest of the menu's: `acciones_de_overlay`
        // is at clippy's line cap, and this list's order does not matter —
        // the corpus is written up by name.
        ("menu_toggle", UiAction::MenuToggle),
        (
            "sort_by",
            UiAction::SortBy {
                slot_id: 1,
                column: "size".to_owned(),
            },
        ),
        (
            "resize_column",
            UiAction::ResizeColumn {
                slot_id: 1,
                column: "size".to_owned(),
                cells: 12,
            },
        ),
        (
            "breadcrumb_activate",
            UiAction::BreadcrumbActivate {
                slot_id: 1,
                depth: 1,
                generation: 4,
            },
        ),
        (
            "move_cursor",
            UiAction::MoveCursor {
                slot_id: 1,
                delta: -1,
            },
        ),
        ("parent", UiAction::Parent { slot_id: 1 }),
        (
            "ai_rename_decide",
            UiAction::AiRenameDecide { approve: true },
        ),
        (
            "organize_decide",
            UiAction::OrganizeDecide { approve: true },
        ),
        ("organize_scroll", UiAction::OrganizeScroll { down: true }),
        (
            "handoff_failed",
            UiAction::HandoffFailed { no_terminal: true },
        ),
        ("resync", UiAction::Resync),
        ("request_quit", UiAction::RequestQuit),
        (
            "select_row",
            UiAction::SelectRow {
                slot_id: 1,
                key: RowKey(7),
                generation: 4,
            },
        ),
        (
            "set_visible_range",
            UiAction::SetVisibleRange {
                slot_id: 1,
                first: 100,
                count: 40,
            },
        ),
        (
            "toggle_mark",
            UiAction::ToggleMark {
                slot_id: 1,
                key: RowKey(7),
                generation: 4,
            },
        ),
    ]
}

#[test]
fn acuses() {
    check_family(
        "acks.json",
        &[
            ("applied", ActionAck::Applied { sequence: 12 }),
            (
                "stale_generation",
                ActionAck::Stale {
                    reason: StaleAction::Generation,
                },
            ),
            (
                "stale_instance",
                ActionAck::Stale {
                    reason: StaleAction::Instance,
                },
            ),
            (
                "stale_modal",
                ActionAck::Stale {
                    reason: StaleAction::Modal,
                },
            ),
            (
                "unavailable",
                ActionAck::Unavailable {
                    reason_key: "cmd-unavailable-read-only".to_owned(),
                },
            ),
        ],
    );
}

/// The reference snapshot: a screen with a listing (one hostile row), a slot
/// this host does not project yet, a dialog and a live task.
/// The dialog the fixtures nail down.
fn dialogo_de_referencia() -> DialogView {
    DialogView {
        id: ModalId(3),
        title_key: "modal-mkdir-title".to_owned(),
        destination: Some(norte_ui_host::dto::DialogLine {
            text: "/home/oscar/destino".to_owned(),
            hostile: false,
        }),
        subject: Some(norte_ui_host::dto::DialogLine {
            text: "delete".to_owned(),
            hostile: false,
        }),
        asker: Some(norte_ui_host::dto::DialogLine {
            text: "agente-1".to_owned(),
            hostile: false,
        }),
        deadline: Some("caduca en 30 s".to_owned()),
        // A FIXED instant in the fixture: what the golden freezes is the
        // field's shape, and a real `now + 30 s` would make the file
        // different on every run.
        deadline_at_ms: Some(1_700_000_030_000),
        body: vec![norte_ui_host::dto::DialogLine {
            text: "/home/oscar".to_owned(),
            hostile: true,
        }],
        overflow_note: "… se enseñan 1 de 3".to_owned(),
        // And that one of the TWO not shown would be painted as altered: the
        // corpus fixes the interesting case, not the empty one.
        overflow_hostile: true,
        choices: vec![
            DialogChoice {
                id: "confirm".to_owned(),
                label_key: "dialog-confirm".to_owned(),
                destructive: false,
            },
            DialogChoice {
                id: "cancel".to_owned(),
                label_key: "dialog-cancel".to_owned(),
                destructive: false,
            },
        ],
        input: Some(String::new()),
        input_hostile: false,
        input_secret: false,
        fields: Vec::new(),
        dest_check: norte_ui_host::dto::DestCheckView::NotAsked,
    }
}

/// The rename plan the fixtures nail down.
fn plan_ia_de_referencia() -> norte_ui_host::dto::AiRenameView {
    use norte_ui_host::dto::{AiRenamePairView, AiRenameView, DialogLine};
    AiRenameView {
        dir: DialogLine {
            text: "⟨file⟩/home/oscar/series".to_owned(),
            hostile: false,
        },
        pairs: vec![AiRenamePairView {
            from: DialogLine {
                text: "ep1.mkv".to_owned(),
                hostile: false,
            },
            to: DialogLine {
                text: "caf\u{FFFD}.mkv".to_owned(),
                hostile: true,
            },
        }],
        first_visible: 0,
        total: 3,
        status: "lote: aplicable — una task, un deshacer".to_owned(),
        detail: vec![DialogLine {
            text: "✗ 2. ya existe: otro.mkv".to_owned(),
            hostile: false,
        }],
        more_note: "… 1/3 (desplazar: ↓/↑)".to_owned(),
        hidden_hostile: true,
        confirmable: true,
        real_steps_note: "se renombrarán 2 de verdad".to_owned(),
        seen_all: false,
    }
}

/// The organize tree the fixtures nail down (phase 8).
///
/// With a NEW folder, one that already existed and a file inside: the three
/// line classes in the same snapshot, which is what makes it impossible for
/// a renderer to collapse them without this noticing. And with an altered
/// name, because the mark is what separates "what is read" from "what is
/// there".
fn arbol_de_organizar_de_referencia() -> norte_ui_host::dto::OrganizeView {
    use norte_ui_host::dto::{DialogLine, OrganizeLineKind, OrganizeLineView, OrganizeView};
    OrganizeView {
        dir: DialogLine {
            text: "⟨file⟩/home/oscar/descargas".to_owned(),
            hostile: false,
        },
        lines: vec![
            OrganizeLineView {
                depth: 0,
                text: DialogLine {
                    text: "facturas".to_owned(),
                    hostile: false,
                },
                kind: OrganizeLineKind::ExistingDir,
            },
            OrganizeLineView {
                depth: 1,
                text: DialogLine {
                    text: "2026".to_owned(),
                    hostile: false,
                },
                kind: OrganizeLineKind::NewDir,
            },
            OrganizeLineView {
                depth: 2,
                text: DialogLine {
                    text: "caf\u{FFFD}.pdf".to_owned(),
                    hostile: true,
                },
                kind: OrganizeLineKind::Moved,
            },
        ],
        first_visible: 0,
        total: 5,
        more_note: "… 3/5 (desplazar: ↓/↑)".to_owned(),
        hidden_hostile: true,
        summary: "crea 1 carpetas y mueve 2 ficheros".to_owned(),
        seen_all: false,
    }
}

/// The task the fixtures nail down.
fn task_de_referencia() -> TaskView {
    TaskView {
        task_id: 7,
        kind: "copy".to_owned(),
        state: TaskStateView::Running,
        percent: Some(40),
        // Rate and ETA (bridge 69): with a single snapshot they are not
        // known, so the row stays silent instead of making up a number.
        rate: String::new(),
        eta: String::new(),
        detail: Some("notas.txt".to_owned()),
        detail_hostile: false,
        foreign: false,
    }
}

/// The layout the fixtures nail down: two slots, roles assigned.
fn disposicion_de_referencia() -> LayoutView {
    LayoutView {
        cells: (120, 40),
        // The reference carries THREE listings, so the destination mark
        // means something: with two it is "the other one" and is not
        // painted. It is half the contract a corpus with `false` would not
        // pin down.
        mark_target: true,
        // A group of TABS, with one whose name is painted differently from
        // what it is: a hostile directory inside a tab is as hostile as
        // inside a listing.
        tabs: vec![norte_ui_host::dto::TabGroupView {
            slot_id: 1,
            tabs: vec![
                norte_ui_host::dto::TabView {
                    slot_id: 1,
                    title: "trabajo".to_owned(),
                    title_hostile: false,
                },
                norte_ui_host::dto::TabView {
                    slot_id: 9,
                    title: "caf\u{fffd}".to_owned(),
                    title_hostile: true,
                },
            ],
            active: 0,
            panels: false,
        }],
        placements: colocaciones_de_referencia(),
    }
}

/// Where each of the corpus's slots lands.
///
/// Kept apart from the snapshot because there are thirteen of them, and it
/// grows with every new slot: like [`slots_de_referencia`] and
/// [`acciones_de_fila`], it is ONE list of literals with no logic inside,
/// and splitting it up would hide exactly what this file shows at a glance —
/// where each slot lands, whole and in one place.
#[expect(clippy::too_many_lines, reason = "list of literals, no logic")]
fn colocaciones_de_referencia() -> Vec<SlotPlacement> {
    vec![
        SlotPlacement {
            slot_id: 1,
            x: 0,
            y: 0,
            width: 60,
            height: 38,
            role: Some(SlotRole::Active),
            focus_index: 0,
        },
        SlotPlacement {
            slot_id: 2,
            x: 60,
            y: 0,
            width: 60,
            height: 38,
            role: Some(SlotRole::Target),
            focus_index: 1,
        },
        // The other slots from `slots` are also PLACED. Without this the
        // corpus described a screen that names six slots and places two, so
        // a renderer could pass the contract without knowing how to paint
        // the sidebar, the attributes sheet, the processes, or the tree.
        SlotPlacement {
            slot_id: 5,
            x: 0,
            y: 38,
            width: 40,
            height: 2,
            role: None,
            focus_index: 2,
        },
        SlotPlacement {
            slot_id: 6,
            x: 40,
            y: 38,
            width: 40,
            height: 2,
            role: None,
            focus_index: 3,
        },
        SlotPlacement {
            slot_id: 7,
            x: 80,
            y: 38,
            width: 40,
            height: 2,
            role: None,
            focus_index: 4,
        },
        // The two PLUGIN panels (phase 3) are also placed, for the same
        // reason as their neighbors: the contract is checked by painting,
        // and a slot the corpus names and does not place is a slot the
        // renderer never attempts to paint — it would pass the test without
        // knowing how.
        SlotPlacement {
            slot_id: 13,
            x: 120,
            y: 0,
            width: 30,
            height: 20,
            role: None,
            focus_index: 11,
        },
        // And the one that still has no frame, which is the shape seen on
        // every startup until its guest answers.
        SlotPlacement {
            slot_id: 14,
            x: 120,
            y: 20,
            width: 30,
            height: 18,
            role: None,
            focus_index: 12,
        },
        SlotPlacement {
            slot_id: 8,
            x: 0,
            y: 36,
            width: 24,
            height: 2,
            role: None,
            focus_index: 5,
        },
        SlotPlacement {
            slot_id: 10,
            x: 24,
            y: 36,
            width: 96,
            height: 2,
            role: None,
            focus_index: 6,
        },
        // Los dos huecos de preview (#291): con fichero y con nota.
        SlotPlacement {
            slot_id: 11,
            x: 0,
            y: 34,
            width: 60,
            height: 2,
            role: None,
            focus_index: 7,
        },
        SlotPlacement {
            slot_id: 12,
            x: 60,
            y: 34,
            width: 60,
            height: 2,
            role: None,
            focus_index: 8,
        },
    ]
}

/// The viewer the fixtures nail down.
fn visor_de_referencia() -> norte_ui_host::dto::ViewerView {
    norte_ui_host::dto::ViewerView {
        path_display: "⟨file⟩/home/oscar/notas.txt".to_owned(),
        path_hostile: false,
        // A zoom that is NOT the default one (bridge 80): with 100 the
        // golden would not distinguish "it is set" from "the field does not
        // exist".
        image_zoom: 150,
        encoding: "UTF-8".to_owned(),
        eol: "lf".to_owned(),
        hex: false,
        forced: false,
        had_errors: false,
        truncated: true,
        total_rows: 120,
        first_line: 4,
        // And how much there is WIDTHWISE, with the window already scrolled
        // sideways: the viewer does not wrap, so without this a file cut off
        // on the right reads as a short file (bridge 59).
        total_cols: 320,
        first_col: 12,
        lines: vec!["quinta línea".to_owned()],
        // Shown by a PREVIEWER, and it says whose it is: a plugin can show
        // anything — that is its job — and whoever is looking has the right
        // to know they are not seeing the file's bytes.
        preview_by: "via PDF de ACME".to_owned(),
        preview_lossy: true,
        image: Some(norte_ui_host::dto::ImageView {
            format: "PNG".to_owned(),
            width: 1920,
            height: 1080,
        }),
        image_refused: String::new(),
        // The same line as `lines`, split into its fragments: one with a
        // role (and an `fg` the role hides), another with only color,
        // another plain.
        styled: vec![vec![
            norte_ui_host::dto::SpanView {
                text: "quinta".to_owned(),
                role: Some("title".to_owned()),
                fg: Some("#ff0000".to_owned()),
                bg: None,
            },
            norte_ui_host::dto::SpanView {
                text: " lín".to_owned(),
                role: None,
                fg: Some("#00ff00".to_owned()),
                bg: Some("#000080".to_owned()),
            },
            norte_ui_host::dto::SpanView {
                text: "ea".to_owned(),
                role: None,
                fg: None,
                bg: None,
            },
        ]],
    }
}

/// The reference snapshot's slots: a listing, the attributes sheet, the
/// processes panel, the places bar, the tree, the log panel with its two
/// sources, and one of a kind this host does not project.
///
/// Long on purpose, and it grows with every new `SlotView`: it is ONE list
/// of literals with no logic inside, and splitting it up would hide exactly
/// what this file exists to show at a glance — every variant's wire shape,
/// whole and in one place.
#[expect(clippy::too_many_lines, reason = "list of literals, no logic")]
fn slots_de_referencia() -> Vec<SlotView> {
    vec![
        SlotView::Browser(Box::new(BrowserSlotView {
            slot_id: 1,
            generation: 4,
            // With the thin line set (ADR 0148, bridge 94): a copy arriving
            // at this directory.
            progress: Some(62),
            path_display: "⟨file⟩/home/oscar".to_owned(),
            path_hostile: false,
            total_rows: Some(3),
            first_visible: 0,
            rows: vec![
                fila(1, "notas.txt", false),
                fila(2, "caf\u{FFFD}.txt", true),
                fila_adornada(3, "cambiado.rs"),
            ],
            icon_column: true,
            cursor: Some(RowKey(1)),
            // TWO, as `marked_note` says: the number and the phrase are two
            // views of the same fact, and a reference that contradicts them
            // shows exactly the opposite of what the DTO promises.
            marks: 2,
            // The two marks fall on different stretches of the ruler.
            mark_ruler: vec![0, 170],
            // The provider skipped two: it is SAID. A listing missing
            // entries that does not warn about it lies by omission.
            skipped_note: "⚠ 2 entradas omitidas (nombres hostiles/límites)".to_owned(),
            // Both notices at once, which is the real case: a provider that
            // skipped entries AND an active hiding filter. If the renderer
            // glued them into the same node, this fixture would show it.
            hidden_note: "3 ocultas".to_owned(),
            // And the four the window did not have, ALL at once and in the
            // same snapshot: it is the case the renderer has to know how to
            // stack without gluing them into a single node, and the one
            // that fixes their ORDER — notices before the marks counter.
            names_note: "nombres: cp866".to_owned(),
            filling_note: "cargando… (3)".to_owned(),
            pruned_note: "2 marcas caídas, sus entradas ya no están".to_owned(),
            marked_note: "2 marcadas, 4,0 kB".to_owned(),
            footer: "1 dirs · 2 ficheros · 4,0 kB · 2 marcadas, 4,0 kB · 120 GiB libres".to_owned(),
            path_segments: vec!["⟨file⟩".to_owned(), "home".to_owned(), "oscar".to_owned()],
            used_ratio: Some(0.35),
            columns: vec![
                ColumnHeader {
                    id: "name".to_owned(),
                    label: "Nombre".to_owned(),
                    sort: Some("asc".to_owned()),
                    sortable: true,
                    width: None,
                    align: "left".to_owned(),
                },
                ColumnHeader {
                    id: "size".to_owned(),
                    label: "Tamaño".to_owned(),
                    sort: None,
                    sortable: true,
                    width: Some(9),
                    align: "right".to_owned(),
                },
            ],
            state: SlotState::Ready,
            quick: Some(norte_ui_host::dto::QuickView {
                query: "no".to_owned(),
                mode: "filter".to_owned(),
                matches: 1,
            }),
        })),
        SlotView::Metadata(Box::new(norte_ui_host::dto::MetadataSlotView {
            slot_id: 5,
            fields: vec![norte_ui_host::dto::MetadataFieldView {
                label: "Nombre".to_owned(),
                value: "caf\u{fffd}.txt".to_owned(),
                hostile: true,
            }],
            note: String::new(),
            follows_display: "⟨mem⟩/casa".to_owned(),
            follows_hostile: false,
        })),
        SlotView::Processes {
            slot_id: 6,
            cursor: Some(0),
        },
        // The sidebar crosses JSON HERE and nowhere else so far, and it is
        // the only `SlotView` variant with a newtype inside an enum tagged
        // by `kind`: its wire shape does not look like its siblings' and
        // nothing was nailing it down. All three of its row classes go in,
        // including the broken one with its reason.
        SlotView::Places(Box::new(norte_ui_host::dto::PlacesSlotView {
            slot_id: 7,
            rows: vec![
                norte_ui_host::dto::PlaceRowView::Header {
                    label: "Unidades".to_owned(),
                    folded: false,
                },
                norte_ui_host::dto::PlaceRowView::Drive {
                    label: "\u{27e8}file\u{27e9}/".to_owned(),
                    hostile: false,
                    detail: "ext4 · 12 GiB libres de 100 GiB".to_owned(),
                    free: "12G".to_owned(),
                    mount: "/".to_owned(),
                    kind: "fixed".to_owned(),
                },
                norte_ui_host::dto::PlaceRowView::Header {
                    label: "Favoritos".to_owned(),
                    folded: false,
                },
                norte_ui_host::dto::PlaceRowView::Favorite {
                    name: "caf\u{fffd}".to_owned(),
                    target: "\u{27e8}file\u{27e9}/home/oscar/caf\u{fffd}".to_owned(),
                    hostile: true,
                    broken: String::new(),
                },
                norte_ui_host::dto::PlaceRowView::Favorite {
                    name: "rota".to_owned(),
                    target: String::new(),
                    hostile: false,
                    broken: "esa ruta no parsea".to_owned(),
                },
            ],
            cursor: 1,
            generation: 5,
        })),
        // The tree: its three `children` states are three different things
        // to the reader — an open branch, a leaf, and not looked at yet — so
        // all three cross the wire here.
        SlotView::Tree(Box::new(norte_ui_host::dto::TreeSlotView {
            slot_id: 8,
            rows: vec![
                norte_ui_host::dto::TreeRowView {
                    label: "\u{27e8}file\u{27e9}/home/oscar".to_owned(),
                    hostile: false,
                    depth: 0,
                    expanded: true,
                    children: Some(true),
                },
                norte_ui_host::dto::TreeRowView {
                    label: "caf\u{fffd}".to_owned(),
                    hostile: true,
                    depth: 1,
                    expanded: false,
                    children: None,
                },
                norte_ui_host::dto::TreeRowView {
                    label: "vacia".to_owned(),
                    hostile: false,
                    depth: 1,
                    expanded: true,
                    children: Some(false),
                },
            ],
            cursor: 1,
            generation: 3,
        })),
        // The log panel, with BOTH sources in view (#328, bridge 48). The
        // combination is not decorative: it is the only one where the
        // selector (`sources_available`), the effective source, the phrase
        // saying whose level it is, one line from each process, and the two
        // drop counts — which are different numbers and are not added
        // together for that reason — are all visible at once. Without it,
        // the four fields bridge 48 added had nothing pinning them down and
        // the renderer could drift apart in silence.
        SlotView::Log(Box::new(norte_ui_host::dto::LogSlotView {
            slot_id: 10,
            lines: vec![
                norte_ui_host::dto::LogLineView {
                    time: "12:00:00".to_owned(),
                    level: "error".to_owned(),
                    // The label sits next to the id, and in a DIFFERENT
                    // shape: that is what makes it visible in the corpus
                    // that they are two things — one is compared and the
                    // other is read.
                    level_label: "ERROR".to_owned(),
                    target: "norte_core::connect".to_owned(),
                    message: "no se pudo conectar".to_owned(),
                    hostile: false,
                    source: "daemon".to_owned(),
                },
                norte_ui_host::dto::LogLineView {
                    time: "12:00:01".to_owned(),
                    level: "info".to_owned(),
                    level_label: "INFO".to_owned(),
                    target: "norte_ui_host".to_owned(),
                    // With the canonical replacement and MARKED: a log
                    // message can carry a name inside that someone chose.
                    message: "abriendo caf\u{fffd}.txt".to_owned(),
                    hostile: true,
                    source: "window".to_owned(),
                },
            ],
            // The one that is ALWAYS SHOWN: it is the one the buttons
            // control.
            level: "info".to_owned(),
            level_label: "INFO".to_owned(),
            filter: "connect".to_owned(),
            following: false,
            total: 2,
            first_visible: 0,
            // These phrases go in LITERAL and not through `t()`, which is
            // what makes the golden a snapshot of the wire; they are copied
            // from `i18n/es.ftl` by hand, so they have to be kept up to
            // date — they said "the window" until #328 also made them
            // `ntc`'s, which is not a window.
            dropped_note:
                "este proceso descartó 17 líneas viejas · te perdiste 4 líneas del daemon"
                    .to_owned(),
            capturing: "este proceso captura debug · el daemon captura trace".to_owned(),
            source: "de este proceso y del daemon".to_owned(),
            source_mode: "both".to_owned(),
            sources_available: true,
            source_note: "el nivel es el del daemon: global a sus clientes y solo sube".to_owned(),
        })),
        // The docked viewer (#291, bridge 51): the SAME `ViewerView` as the
        // big one, inside a slot, and its twin with no file and the note.
        SlotView::Preview(Box::new(norte_ui_host::dto::PreviewSlotView {
            slot_id: 11,
            viewer: Some(visor_de_referencia()),
            note: String::new(),
        })),
        SlotView::Preview(Box::new(norte_ui_host::dto::PreviewSlotView {
            slot_id: 12,
            viewer: None,
            note: "directorio".to_owned(),
        })),
        // The timeline (#359, bridge 78): one row from the human and one
        // agent batch with no undo path with the name masked, because the
        // renderer paints the two differently.
        SlotView::Timeline(Box::new(norte_ui_host::dto::TimelineSlotView {
            slot_id: 14,
            title: "Línea de tiempo".to_owned(),
            rows: vec![
                norte_ui_host::dto::TimelineRowView {
                    time: "12:00".to_owned(),
                    actor: "user".to_owned(),
                    op: "renamed".to_owned(),
                    path: "/casa/a.txt".to_owned(),
                    hostile: false,
                    tail: String::new(),
                },
                norte_ui_host::dto::TimelineRowView {
                    time: "11:58".to_owned(),
                    actor: "agent".to_owned(),
                    op: "removed".to_owned(),
                    path: "/casa/caf\u{fffd}".to_owned(),
                    hostile: true,
                    tail: "3 de golpe · sin vuelta".to_owned(),
                },
            ],
            cursor: Some(1),
            empty: "todavía no se ha hecho nada".to_owned(),
            footer: "1 entradas se deshacen".to_owned(),
        })),
        // A PLUGIN's panel (phase 3): styled spans and zones WITHOUT their
        // command — the renderer says where it was clicked and the host
        // works out what it was, so nothing executable travels over the
        // wire.
        SlotView::Panel(Box::new(norte_ui_host::dto::PanelSlotView {
            slot_id: 13,
            title: "status".to_owned(),
            lines: vec![vec![
                norte_ui_host::dto::SpanView {
                    text: "rama ".to_owned(),
                    role: Some("muted".to_owned()),
                    fg: None,
                    bg: None,
                },
                norte_ui_host::dto::SpanView {
                    text: "main".to_owned(),
                    role: None,
                    fg: Some("#7fd88f".to_owned()),
                    bg: None,
                },
            ]],
            hits: vec![norte_ui_host::dto::HitView {
                row: 0,
                col: 5,
                width: 4,
            }],
        })),
        // And one still WITHOUT a frame: the first request in flight, or a
        // plugin that failed. It is the shape the renderer has to know how
        // to paint — border and title, nothing inside — and the one seen on
        // every startup.
        SlotView::Panel(Box::new(norte_ui_host::dto::PanelSlotView {
            slot_id: 14,
            title: "status".to_owned(),
            lines: Vec::new(),
            hits: Vec::new(),
        })),
        SlotView::Unsupported {
            slot_id: 2,
            kind_name: "compare".to_owned(),
            kind_name_hostile: false,
        },
    ]
}

/// "Go to" (bridge 77): a header and two rows, one of them marked hostile,
/// because the renderer paints the three differently.
fn ir_a_de_referencia() -> norte_ui_host::dto::GotoView {
    use norte_ui_host::dto::GotoLineView;
    norte_ui_host::dto::GotoView {
        query: "doc".to_owned(),
        lines: vec![
            GotoLineView::Header {
                title: "Historia".to_owned(),
            },
            GotoLineView::Row {
                text: "/home/ana/docs".to_owned(),
                desc: String::new(),
                hostile: false,
            },
            GotoLineView::Row {
                text: "caf\u{fffd}".to_owned(),
                desc: "/srv/caf\u{fffd}".to_owned(),
                hostile: true,
            },
        ],
        cursor: Some(1),
        empty: "nada casa con eso".to_owned(),
    }
}

/// The profile picker: one active and another that fails to load, because
/// the two rows say different things and the renderer paints them
/// differently.
fn perfiles_de_referencia() -> norte_ui_host::dto::ProfilePickerView {
    norte_ui_host::dto::ProfilePickerView {
        rows: vec![
            norte_ui_host::dto::ProfileRowView {
                name: "fotos".to_owned(),
                name_hostile: false,
                title: Some("Fotos".to_owned()),
                active: true,
                clash: String::new(),
                no_state: false,
                problem: String::new(),
            },
            norte_ui_host::dto::ProfileRowView {
                name: "far".to_owned(),
                name_hostile: false,
                title: None,
                active: false,
                clash: "también es un preset de teclado".to_owned(),
                no_state: true,
                problem: "línea 3: falta `]`".to_owned(),
            },
        ],
        cursor: 0,
        generation: 4,
    }
}

/// The menu bar with one DROPPED DOWN: the fixture has to carry both
/// halves, because those are the two the renderer paints.
fn asistente_de_referencia() -> norte_ui_host::dto::WizardView {
    norte_ui_host::dto::WizardView {
        title: "Bienvenido a norte · 1/3 · teclas".to_owned(),
        question: "¿Qué gestor de ficheros tienes en los dedos?".to_owned(),
        rows: vec!["orthodox — estilo mc".to_owned(), "vim — hjkl".to_owned()],
        cursor: 0,
        hint: "[Intro] siguiente · [Esc] salir".to_owned(),
    }
}

/// One clickable, one not, and the tasks one with its bar (bridge 92): the
/// shapes the renderer paints.
fn elementos_de_estado_de_referencia() -> Vec<norte_ui_host::dto::StatusItemView> {
    use norte_ui_host::dto::{StatusItemView, StatusProgressView};
    vec![
        StatusItemView {
            id: "tasks".to_owned(),
            text: "⟳ copiando foto.jpg 62 % · 48 MiB/s".to_owned(),
            tooltip: "1 tareas en marcha. Pulsa para ver los procesos".to_owned(),
            clickable: true,
            progress: Some(StatusProgressView {
                percent: Some(62),
                phase: "running".to_owned(),
            }),
        },
        StatusItemView {
            id: "position".to_owned(),
            text: "3/120".to_owned(),
            tooltip: "Posición del cursor en el listado".to_owned(),
            clickable: false,
            progress: None,
        },
        StatusItemView {
            id: "sort".to_owned(),
            text: "Nombre ↑".to_owned(),
            tooltip: "Orden del listado. Pulsa para cambiarlo".to_owned(),
            clickable: true,
            progress: None,
        },
    ]
}

fn barra_de_paneles_de_referencia() -> norte_ui_host::dto::PanelBarView {
    use norte_ui_host::dto::{PanelButtonState, PanelButtonView};
    norte_ui_host::dto::PanelBarView {
        bar: true,
        names: true,
        // A column in the reference (bridge 84): a boolean the golden fixes
        // to `false` would not distinguish "it is set" from "it does not
        // exist".
        vertical: true,
        buttons: vec![
            PanelButtonView {
                kind: "places".to_owned(),
                label: "Sitios".to_owned(),
                letter: "S".to_owned(),
                chord: "alt+p".to_owned(),
                state: PanelButtonState::Open,
                attention: false,
                count: 0,
            },
            PanelButtonView {
                kind: "log".to_owned(),
                label: "Registro".to_owned(),
                letter: "R".to_owned(),
                chord: "—".to_owned(),
                state: PanelButtonState::Closed,
                attention: true,
                count: 3,
            },
        ],
    }
}

fn menu_de_referencia() -> norte_ui_host::dto::MenuView {
    norte_ui_host::dto::MenuView {
        bar: true,
        titles: vec!["Archivo".to_owned(), "Paneles".to_owned()],
        open: Some(1),
        items: vec![
            norte_ui_host::dto::MenuItemView {
                label: "Cambiar de panel".to_owned(),
                chord: "tab".to_owned(),
                enabled: true,
                section: None,
                role: "normal".to_owned(),
            },
            norte_ui_host::dto::MenuItemView {
                label: "Desconectar".to_owned(),
                chord: String::new(),
                enabled: false,
                section: Some("Sitios".to_owned()),
                role: "normal".to_owned(),
            },
            norte_ui_host::dto::MenuItemView {
                label: "Borrar".to_owned(),
                chord: "F8".to_owned(),
                enabled: true,
                section: Some(String::new()),
                role: "destructive".to_owned(),
            },
        ],
        cursor: 1,
    }
}

fn snapshot_de_referencia() -> ViewSnapshot {
    ViewSnapshot {
        compare: None,
        sync: None,
        // Sin pantalla de arranque puesta (puente 69).
        splash: None,
        slots: slots_de_referencia(),
        connection: ConnectionView::Connected,
        layout: disposicion_de_referencia(),
        focus: Some(1),
        status: StatusView {
            message: Some("2 entradas".to_owned()),
            banners: vec![],
            notices_unread: 2,
            pending: Some(norte_ui_host::dto::PendingView {
                chords: "ctrl+x".to_owned(),
                count: Some(12),
            }),
        },
        dialogs: vec![dialogo_de_referencia()],
        tasks: vec![task_de_referencia()],
        menu: menu_de_referencia(),
        panel_bar: barra_de_paneles_de_referencia(),
        status_items: elementos_de_estado_de_referencia(),
        layout_buttons: vec![norte_ui_host::dto::ChromeButtonView {
            id: "split-h".to_owned(),
            label: "Partir lado a lado".to_owned(),
            chord: "—".to_owned(),
        }],
        // The pajama ON in the reference (bridge 80): a boolean that the
        // golden pins to `false` does not distinguish "it commands it" from
        // "it does not exist".
        row_stripes: true,
        profiles: Some(perfiles_de_referencia()),
        wizard: Some(asistente_de_referencia()),
        palette: Some(norte_ui_host::dto::PaletteView {
            query: "orde".to_owned(),
            rows: vec![norte_ui_host::dto::PaletteRowView {
                text: "pane.sort-name".to_owned(),
                desc: "Ordenar por nombre".to_owned(),
                chord: "ctrl+f3".to_owned(),
                enabled: true,
                hostile: false,
                recent: false,
            }],
            cursor: Some(0),
            total: 42,
        }),
        goto: Some(ir_a_de_referencia()),
        whichkey: Some(norte_ui_host::dto::WhichKeyView {
            title: "ctrl+x".to_owned(),
            rows: vec![
                norte_ui_host::dto::WhichKeyRowView {
                    chord: "g".to_owned(),
                    label: "Ir al principio".to_owned(),
                    enabled: true,
                    opens_sequence: false,
                    reason: String::new(),
                },
                norte_ui_host::dto::WhichKeyRowView {
                    chord: "s".to_owned(),
                    label: "Sincronizar".to_owned(),
                    enabled: false,
                    opens_sequence: false,
                    reason: "aquí no".to_owned(),
                },
            ],
        }),
        help: Some(ayuda_de_referencia()),
        settings: Some(ajustes_de_referencia()),
        extensions: Some(extensiones_de_referencia()),
        agents: Some(agentes_de_referencia()),
        plugin_output: Some(salida_de_referencia()),
        program_output: Some(programa_de_referencia()),
        theme: Some(tema_de_referencia()),
        search: Some(busqueda_de_referencia()),
        layouts: Some(disposiciones_de_referencia()),
        columns: Some(columnas_de_referencia()),
        picker: Some(selector_de_referencia()),
        viewer: Some(visor_de_referencia()),
        // With a plan, like the rest of this snapshot's overlays: if it went
        // to `None`, nobody would pin down the field's place inside the
        // snapshot.
        ai_rename: Some(plan_ia_de_referencia()),
        organize: Some(arbol_de_organizar_de_referencia()),
        locale: "es".to_owned(),
    }
}

/// The reference theme: two roles and one effect this renderer does not
/// paint.
fn tema_de_referencia() -> norte_ui_host::dto::ThemeView {
    use norte_ui_host::dto::{ThemeRoleView, ThemeView};
    ThemeView {
        name: "tokyonight".to_owned(),
        roles: vec![
            ThemeRoleView {
                role: "selection-bg".to_owned(),
                color: "#2d4f8a".to_owned(),
            },
            ThemeRoleView {
                role: "error-fg".to_owned(),
                color: "#f7768e".to_owned(),
            },
        ],
        unsupported_effects: vec![norte_ui_host::dto::ThemeEffectView {
            key: "crt".to_owned(),
            hostile: false,
        }],
        choices: vec!["default".to_owned(), "tokyonight".to_owned()],
        cursor: 1,
    }
}

/// The reference search: two hits, one with a hostile name, and still
/// running.
fn busqueda_de_referencia() -> norte_ui_host::dto::SearchView {
    use norte_ui_host::dto::{SearchRowView, SearchView};
    SearchView {
        semantic: false,
        query: "*.rs".to_owned(),
        root: "⟨file⟩/home/oscar/work".to_owned(),
        root_hostile: false,
        rows: vec![
            SearchRowView {
                name: "main.rs".to_owned(),
                hostile: false,
                parent: "⟨file⟩/home/oscar/work/src".to_owned(),
                parent_hostile: false,
                is_dir: false,
                score: None,
            },
            SearchRowView {
                name: "caf\u{fffd}.rs".to_owned(),
                hostile: true,
                parent: "⟨file⟩/home/oscar/work".to_owned(),
                parent_hostile: false,
                is_dir: false,
                score: None,
            },
        ],
        cursor: Some(0),
        status: "búsqueda: 2 hallazgos (buscando…)".to_owned(),
        running: true,
    }
}

/// The reference plan: a copy and a tree delete, with the mode in view and
/// a lock.
fn sincronizacion_de_referencia() -> norte_ui_host::dto::SyncView {
    use norte_ui_host::dto::{SyncStepView, SyncView};
    SyncView {
        source: norte_ui_host::dto::DialogLine {
            text: "\u{27e8}file\u{27e9}/home/oscar/a".to_owned(),
            hostile: false,
        },
        dest: norte_ui_host::dto::DialogLine {
            text: "\u{27e8}file\u{27e9}/home/oscar/b".to_owned(),
            hostile: false,
        },
        mode: "update".to_owned(),
        steps: vec![
            SyncStepView {
                id: 1,
                kind: "copiar".to_owned(),
                reason: String::new(),
                undo: "se deshace".to_owned(),
                anchor: "source".to_owned(),
                anchor_label: String::new(),
                path: "docs/a.md".to_owned(),
                path_hostile: false,
                dest_path: None,
                dest_path_hostile: false,
                twins: false,
            },
            SyncStepView {
                id: 2,
                kind: "borrar".to_owned(),
                reason: "no hay papelera en el destino".to_owned(),
                undo: "no vuelve".to_owned(),
                anchor: "dest".to_owned(),
                anchor_label: "en el destino".to_owned(),
                path: "caf\u{fffd}.txt".to_owned(),
                path_hostile: true,
                dest_path: Some("cafe\u{301}.txt".to_owned()),
                dest_path_hostile: false,
                twins: true,
            },
        ],
        first_visible: 0,
        total: 2,
        summary: vec![
            "2 pasos: 1 copiar, 1 borrar".to_owned(),
            "1 no se puede deshacer".to_owned(),
        ],
        blockers: vec![norte_ui_host::dto::SyncBlockerView {
            label: "el destino es de solo lectura".to_owned(),
            // The ROOT is said, not kept quiet: a lock on the whole tree
            // with an empty path does not say where it happens.
            path: "todo el \u{e1}rbol".to_owned(),
            path_hostile: false,
        }],
        blockers_total: 900,
        cancel_requested: false,
        confirming: Some("esto borra 2 \u{e1}rboles enteros. \u{bf}seguro? (y/n)".to_owned()),
        failures: vec![norte_ui_host::dto::SyncFailureView {
            cause: "permiso denegado".to_owned(),
            path: "docs/a.md".to_owned(),
            path_hostile: false,
            // `either` is PAINTED: in a pane where an unqualified path
            // means "from the source", keeping it quiet asserts the source.
            anchor: "either".to_owned(),
            // `either` is SAID: keeping it quiet in a pane where an
            // unqualified path means "from the source" asserts the source.
            anchor_label: "en cualquiera de los dos".to_owned(),
        }],
        status: "2 pasos \u{b7} este plan no se puede aprobar".to_owned(),
        hint: "\u{2191}\u{2193} mover \u{b7} Esc cerrar".to_owned(),
        can_approve: false,
        running: false,
    }
}

/// The reference comparison: one matching row and one orphan on the left
/// with a hostile name, and one hidden category.
fn comparacion_de_referencia() -> norte_ui_host::dto::CompareView {
    use norte_ui_host::dto::{CompareFaceView, CompareFilterView, CompareRowView, CompareView};
    let cara = |name: &str, hostile: bool, size: &str| CompareFaceView {
        name: name.to_owned(),
        hostile,
        size: size.to_owned(),
        mtime: "2026-08-22 10:00".to_owned(),
        is_dir: false,
    };
    CompareView {
        left: "\u{27e8}file\u{27e9}/home/oscar/a".to_owned(),
        left_hostile: false,
        right: "\u{27e8}file\u{27e9}/home/oscar/b".to_owned(),
        right_hostile: true,
        rows: vec![
            CompareRowView {
                id: 1,
                verdict: "igual".to_owned(),
                category: "same".to_owned(),
                confidence: "cierto".to_owned(),
                criterion: "size".to_owned(),
                reason: None,
                left: Some(cara("notas.txt", false, "1,2 kB")),
                right: Some(cara("notas.txt", false, "1,2 kB")),
                paired_under: None,
            },
            CompareRowView {
                id: 2,
                verdict: "solo a la izquierda".to_owned(),
                category: "only-left".to_owned(),
                confidence: "cierto".to_owned(),
                criterion: "presence".to_owned(),
                reason: None,
                // No size: an unhydrated orphan does not know it, and that
                // travels as ABSENCE, not as a manufactured zero.
                left: Some(cara("caf\u{fffd}.txt", true, "")),
                right: None,
                paired_under: Some("los dos nombres se escriben distinto".to_owned()),
            },
        ],
        first_visible: 0,
        total: 9,
        selected: Some(2),
        filters: vec![CompareFilterView {
            id: "same".to_owned(),
            label: "iguales".to_owned(),
            count: 4,
            hidden: true,
        }],
        status: "comparaci\u{f3}n: 9 filas".to_owned(),
        running: false,
    }
}

/// The reference layout picker: a factory one that shares a name with a
/// keyboard preset, and a user one that fails to parse.
fn disposiciones_de_referencia() -> norte_ui_host::dto::LayoutPickerView {
    use norte_ui_host::dto::{LayoutPickerView, LayoutRowView};
    LayoutPickerView {
        title: "Disposiciones".to_owned(),
        rows: vec![
            LayoutRowView {
                name: "orthodox".to_owned(),
                hostile: false,
                factory: true,
                shares_keymap_name: true,
                broken: false,
            },
            LayoutRowView {
                name: "mia".to_owned(),
                hostile: false,
                factory: false,
                shares_keymap_name: false,
                broken: true,
            },
        ],
        cursor: 0,
        preview: vec!["··········".to_owned(), "·bbbbbbbb·".to_owned()],
        problem_hostile: false,
        problem: String::new(),
    }
}

/// The reference COLUMNS picker.
///
/// Its four rows are the four cases the model distinguishes: the fixed one
/// — the name —, a builtin with a cyclable format, an `attr:` whose format
/// is pinned down by the schema, and an id that does NOT parse, which is
/// preserved because it is the user's configuration intent.
fn columnas_de_referencia() -> norte_ui_host::dto::ColumnsPickerView {
    use norte_ui_host::dto::{ColumnsPickerRowView, ColumnsPickerView};
    ColumnsPickerView {
        title: "Columnas — sftp".to_owned(),
        rows: vec![
            ColumnsPickerRowView {
                id: "name".to_owned(),
                label: "Nombre".to_owned(),
                hostile: false,
                enabled: true,
                format: String::new(),
                format_locked: false,
                fixed: true,
            },
            ColumnsPickerRowView {
                id: "size".to_owned(),
                label: "Tamaño".to_owned(),
                hostile: false,
                enabled: true,
                format: "iec".to_owned(),
                format_locked: false,
                fixed: false,
            },
            ColumnsPickerRowView {
                id: "attr:posix.mode".to_owned(),
                label: "Permisos".to_owned(),
                hostile: false,
                enabled: false,
                format: "symbolic".to_owned(),
                format_locked: true,
                fixed: false,
            },
            ColumnsPickerRowView {
                id: "esto-no-parsea".to_owned(),
                label: "esto-no-parsea".to_owned(),
                hostile: false,
                enabled: true,
                format: String::new(),
                format_locked: false,
                fixed: false,
            },
        ],
        cursor: 1,
        note: "se aplica a esta ventana; no se guarda".to_owned(),
        // The footer comes from the KEYMAP (#287), so a painted one goes
        // here: what the corpus pins down is that it travels over the wire,
        // not which keys this preset binds.
        hint: "Espacio activa · Shift+↑ mueve · Ctrl+S ordena".to_owned(),
    }
}

/// A reference picker: volumes, with one read-only.
fn selector_de_referencia() -> norte_ui_host::dto::PickerView {
    use norte_ui_host::dto::{PickerRowView, PickerView};
    PickerView {
        title: "Volúmenes".to_owned(),
        rows: vec![PickerRowView {
            label: "⟨file⟩/".to_owned(),
            hostile: false,
            detail: "ext4 · 12 GiB libres de 100 GiB".to_owned(),
        }],
        cursor: Some(0),
        empty: String::new(),
        generation: 2,
    }
}

/// The agent sessions panel: one session whose id is painted differently
/// from what it is — it is an opaque key from the daemon, not a charset
/// identifier — and another clean one.
fn agentes_de_referencia() -> norte_ui_host::dto::AgentsView {
    norte_ui_host::dto::AgentsView {
        rows: vec![
            norte_ui_host::dto::AgentRowView {
                session: "agente\u{fffd}1".to_owned(),
                session_hostile: true,
                // With an undo IN PROGRESS: the row says so, and `u` on it
                // is refused.
                undoing: true,
                counts: "pidió 7, aprobadas desde aquí 3".to_owned(),
                last_op: "delete".to_owned(),
                last_op_hostile: false,
            },
            norte_ui_host::dto::AgentRowView {
                session: "agente-2".to_owned(),
                session_hostile: false,
                undoing: false,
                counts: "pidió 1, aprobadas desde aquí 0".to_owned(),
                last_op: "copy".to_owned(),
                last_op_hostile: false,
            },
        ],
        cursor: 0,
        generation: 4,
        // With one FORGOTTEN: the cap exists, and saying so is what keeps a
        // trimmed list from reading as complete.
        forgotten: 2,
        note: "solo las sesiones que esta ventana ha visto".to_owned(),
        empty: "ningún agente ha pedido permiso".to_owned(),
    }
}

/// An extension command's output: what a third party printed, already
/// masked, capped, and saying it was cut off.
/// A program's output (#312, bridge 52): the comparator, with one masked
/// line and the output cut off, which are the two fields the renderer
/// paints differently.
fn programa_de_referencia() -> norte_ui_host::dto::ProgramOutputView {
    norte_ui_host::dto::ProgramOutputView {
        title_key: "program-output-compare".to_owned(),
        command: norte_ui_host::dto::MaskedTextView {
            text: "/usr/bin/diff -u a.txt b.txt".to_owned(),
            hostile: false,
        },
        lines: vec![
            "--- a.txt".to_owned(),
            "+++ b.txt".to_owned(),
            "-hola\u{fffd}".to_owned(),
            "+hola".to_owned(),
        ],
        text_hostile: true,
        truncated: true,
        failed: false,
    }
}

fn salida_de_referencia() -> norte_ui_host::dto::ExtensionOutputView {
    norte_ui_host::dto::ExtensionOutputView {
        // The extension's name masked AND marked, with CLEAN text: it is the
        // case a single flag for the three strings could not express — the
        // flag leaked from the text, so a hostile manifest with ASCII output
        // was painted with no badge.
        plugin: norte_ui_host::dto::MaskedTextView {
            text: "ACME\u{fffd}FTP".to_owned(),
            hostile: true,
        },
        plugin_id: "acme.ftp".to_owned(),
        command: norte_ui_host::dto::MaskedTextView {
            text: "Saludar".to_owned(),
            hostile: false,
        },
        // And by LINE: a line break is a C0 control, so masking the whole
        // output marked any output longer than one line as hostile.
        lines: vec!["hola".to_owned(), "mundo".to_owned()],
        text_hostile: false,
        truncated: true,
    }
}

/// The reference extensions manager: one approved and enabled extension,
/// another that is not, a directory that failed to load and an open card.
fn extensiones_de_referencia() -> norte_ui_host::dto::ExtensionsView {
    use norte_ui_host::dto::{
        ExtensionConfigRowView, ExtensionDetailView, ExtensionErrorView, ExtensionRowView,
        ExtensionsView,
    };
    ExtensionsView {
        rows: vec![
            ExtensionRowView {
                id: "acme.ftp".to_owned(),
                name: "FTP de ACME".to_owned(),
                publisher: "ACME".to_owned(),
                version: "1.2.0".to_owned(),
                category: "provider".to_owned(),
                description: "Sirve ficheros por FTP".to_owned(),
                approved: true,
                enabled: true,
                has_help: true,
                commands: 2,
                columns: 0,
                capabilities: vec!["net".to_owned(), "fs-read".to_owned()],
            },
            ExtensionRowView {
                id: "org.norte.demo".to_owned(),
                name: "Demo".to_owned(),
                publisher: String::new(),
                version: "0.1.0".to_owned(),
                category: "previewer".to_owned(),
                description: String::new(),
                approved: false,
                enabled: false,
                has_help: false,
                commands: 1,
                columns: 1,
                capabilities: vec!["fs-read".to_owned()],
            },
        ],
        cursor: 0,
        detail: Some(ExtensionDetailView {
            commands: vec![norte_ui_host::dto::ExtensionCommandView {
                id: "greet".to_owned(),
                title: "Saludar".to_owned(),
                hostile: false,
            }],
            cursor: 0,
            editing: None,
            editing_hostile: false,
            id: "acme.ftp".to_owned(),
            config: vec![
                ExtensionConfigRowView {
                    key: "timeout".to_owned(),
                    kind: "int".to_owned(),
                    value: "30".to_owned(),
                    default: "10".to_owned(),
                    description: "Segundos antes de rendirse".to_owned(),
                    domain: "entre 1 y 300".to_owned(),
                    hostile: false,
                    editable: true,
                },
                // An `enum` whose domain carries plugin text with a bidi
                // override inside: it arrives masked AND flagged, and the
                // `·` that joins it cannot be manufactured from `plugin.toml`.
                ExtensionConfigRowView {
                    key: "mode".to_owned(),
                    kind: "enum".to_owned(),
                    value: "safe".to_owned(),
                    default: "safe".to_owned(),
                    description: String::new(),
                    domain: "safe · fast\u{fffd} · read-only".to_owned(),
                    hostile: true,
                    editable: true,
                },
            ],
        }),
        loading: false,
        errors: vec![ExtensionErrorView {
            dir: "/home/oscar/.config/norte/plugins/roto".to_owned(),
            hostile: false,
            reason: "el manifiesto no parsea".to_owned(),
            reason_hostile: false,
            id: None,
        }],
    }
}

/// The reference settings: a registry entry with its effective value, and a
/// locations section with one missing.
/// The reference view's two settings sections, kept apart because the whole
/// function was going over the lint's line cap.
fn secciones_de_ajustes_de_referencia() -> Vec<norte_ui_host::dto::SettingsSectionView> {
    use norte_ui_host::dto::{PathRowView, SettingRowView, SettingsSectionView};
    vec![
        SettingsSectionView::Settings {
            key: "behavior".to_owned(),
            title: "Comportamiento".to_owned(),
            rows: vec![SettingRowView {
                id: "ui.confirm-quit".to_owned(),
                name: "Confirmar al salir".to_owned(),
                desc: "Pregunta antes de cerrar norte".to_owned(),
                value: "siempre".to_owned(),
                default: "auto".to_owned(),
                hostile: false,
                restart_required: true,
                // A CLOSED list: the renderer paints a dropdown and does
                // not need to know where the values come from.
                control: "choice".to_owned(),
                choices: vec!["auto".to_owned(), "always".to_owned(), "never".to_owned()],
                min: None,
                max: None,
                modified: false,
            }],
        },
        SettingsSectionView::Settings {
            key: "appearance".to_owned(),
            title: "Apariencia".to_owned(),
            // A value the USER wrote in their `norte.toml` with a bidi
            // override inside: it arrives masked, marked, and with the dot
            // that says "this is not factory".
            rows: vec![SettingRowView {
                id: "ui.font".to_owned(),
                name: "Tipografía".to_owned(),
                desc: "La fuente de la ventana".to_owned(),
                value: "Fira\u{fffd}Code".to_owned(),
                // Empty by factory default: the window shows it as a
                // placeholder, and this pins down that a default can be
                // empty.
                default: String::new(),
                hostile: true,
                restart_required: true,
                control: "text".to_owned(),
                choices: Vec::new(),
                min: None,
                max: None,
                modified: true,
            }],
        },
        SettingsSectionView::Paths {
            title: "Dónde vive cada cosa".to_owned(),
            rows: vec![
                PathRowView {
                    label: "Tu configuración".to_owned(),
                    display: "/home/oscar/.config/norte".to_owned(),
                    hostile: false,
                    missing: false,
                },
                PathRowView {
                    label: "Configuración del proyecto".to_owned(),
                    display: ".norte".to_owned(),
                    hostile: false,
                    missing: true,
                },
            ],
        },
    ]
}

fn ajustes_de_referencia() -> norte_ui_host::dto::SettingsView {
    use norte_ui_host::dto::{SectionIndexView, SettingsView};
    SettingsView {
        sections: secciones_de_ajustes_de_referencia(),
        index: vec![
            SectionIndexView {
                key: "appearance".to_owned(),
                title: "Apariencia".to_owned(),
                visible: 1,
            },
            SectionIndexView {
                key: "behavior".to_owned(),
                title: "Comportamiento".to_owned(),
                visible: 1,
            },
            // A section the filter emptied out: it stays in the index, dim.
            SectionIndexView {
                key: "open-with".to_owned(),
                title: "Abrir con".to_owned(),
                visible: 0,
            },
        ],
        // The keyboard on the INDEX: it is the side the renderer has to
        // know how to paint live, and the other one dim.
        focus: "index".to_owned(),
        cursor: 0,
        query: "fira".to_owned(),
        shown: 2,
        total: 34,
    }
}

/// The reference help: a page with prose, a live mark already resolved, a
/// link, the keyboard sheet, and a row this frontend does not execute.
fn ayuda_de_referencia() -> norte_ui_host::dto::HelpView {
    use norte_ui_host::dto::{
        HelpActionView, HelpBlockView, HelpFocusView, HelpKeyRowView, HelpSidebarRowView,
        HelpSpanView, HelpView,
    };
    HelpView {
        title: "Copiar".to_owned(),
        topic_id: "copying".to_owned(),
        badge: None,
        sidebar: vec![
            HelpSidebarRowView::Group {
                label: "Lo básico".to_owned(),
            },
            HelpSidebarRowView::Topic {
                title: "Copiar".to_owned(),
                current: true,
            },
        ],
        cursor: 1,
        focus: HelpFocusView::Body,
        blocks: vec![
            HelpBlockView::Heading {
                level: 2,
                text: "Copiar".to_owned(),
            },
            HelpBlockView::Paragraph {
                spans: vec![
                    HelpSpanView::Text {
                        text: "Pulsa ".to_owned(),
                    },
                    HelpSpanView::Command {
                        text: "F5".to_owned(),
                        is_chord: true,
                    },
                    HelpSpanView::Link {
                        text: "Marcar".to_owned(),
                        // The row that follows it: the view's second action.
                        action: Some(1),
                    },
                ],
            },
            HelpBlockView::Bullets {
                items: vec![vec![HelpSpanView::Strong {
                    text: "Ojo".to_owned(),
                }]],
            },
            HelpBlockView::Code {
                lang: Some("sh".to_owned()),
                text: "norte --help".to_owned(),
            },
            HelpBlockView::Table {
                header: vec!["Tecla".to_owned()],
                rows: vec![vec!["F5".to_owned()]],
            },
            HelpBlockView::Callout {
                kind: norte_ui_host::dto::HelpCalloutView::Warn,
                spans: vec![HelpSpanView::Emph {
                    text: "Cuidado".to_owned(),
                }],
            },
            HelpBlockView::Keys {
                rows: vec![HelpKeyRowView {
                    label_hostile: false,
                    chord: "F5".to_owned(),
                    label: "copiar".to_owned(),
                    enabled: false,
                    reason: "aquí no".to_owned(),
                }],
            },
        ],
        actions: vec![
            HelpActionView {
                label: "copiar".to_owned(),
                chord: "F5".to_owned(),
                enabled: false,
                reason: "aquí no".to_owned(),
                opens_topic: false,
            },
            HelpActionView {
                label: "Marcar".to_owned(),
                chord: String::new(),
                enabled: true,
                reason: String::new(),
                opens_topic: true,
            },
        ],
        action_cursor: Some(0),
        filter: "cop".to_owned(),
        filtering: true,
        can_back: true,
        // Bridge 76: the request to scroll the body, with its number.
        scroll: Some(norte_ui_host::dto::HelpScrollView {
            to: norte_ui_host::dto::HelpScrollTo::PageDown,
            seq: 3,
        }),
    }
}

#[test]
fn actualizaciones() {
    let snapshot = snapshot_de_referencia();
    check_family(
        "updates.json",
        &[
            (
                "notice_fatal",
                UiUpdate::Notice(UiNotice::Fatal {
                    key: "host-internal-error".to_owned(),
                }),
            ),
            (
                "notice_message",
                UiUpdate::Notice(UiNotice::Message {
                    key: "msg-connection-lost".to_owned(),
                    detail: None,
                }),
            ),
            (
                "notice_shutdown",
                UiUpdate::Notice(UiNotice::Shutdown { incomplete: false }),
            ),
            (
                "patch_cursor",
                UiUpdate::Patch(ViewPatch {
                    base_sequence: 11,
                    changes: vec![ViewChange::Cursor {
                        slot_id: 1,
                        generation: 4,
                        cursor: Some(RowKey(2)),
                    }],
                }),
            ),
            (
                "patch_rows",
                UiUpdate::Patch(ViewPatch {
                    base_sequence: 12,
                    changes: vec![
                        ViewChange::Rows {
                            slot_id: 1,
                            generation: 5,
                            first_visible: 40,
                            rows: vec![fila(41, "otro.txt", false)],
                            icon_column: false,
                            total_rows: Some(120),
                        },
                        ViewChange::SlotState {
                            slot_id: 1,
                            state: SlotState::Loading {
                                verb_key: "busy-listing".to_owned(),
                                target_display: "⟨mem⟩/casa/docs".to_owned(),
                                target_hostile: false,
                            },
                        },
                    ],
                }),
            ),
            (
                "patch_layout",
                UiUpdate::Patch(ViewPatch {
                    base_sequence: 13,
                    changes: vec![ViewChange::Layout(disposicion_de_referencia())],
                }),
            ),
            ("snapshot", UiUpdate::Snapshot(Box::new(snapshot))),
        ],
    );
}

#[test]
fn el_sobre() {
    let e = BridgeEnvelope::new(
        InstanceId::new("host-1"),
        0,
        UiUpdate::Notice(UiNotice::Shutdown { incomplete: true }),
    );
    check_family("envelope.json", &[("shutdown", e)]);
}

/// An action with a tag this host does not know is NOT interpreted.
#[test]
fn una_accion_desconocida_se_rechaza() {
    let crudo = r#"{"action":"format_disk","slot_id":1}"#;
    let out: Result<UiAction, _> = serde_json::from_str(crudo);
    assert!(out.is_err(), "an unknown action is not accepted");
}

/// An envelope from a future version is detected BEFORE looking at the
/// payload.
#[test]
fn un_sobre_futuro_no_se_interpreta() {
    let e: BridgeEnvelope<UiUpdate> = serde_json::from_str(
        r#"{"bridge_version":9999,"instance_id":"host-1","sequence":0,
            "payload":{"update":"notice","notice":"shutdown","incomplete":false}}"#,
    )
    .expect("the envelope reads");
    assert!(
        !e.is_supported(),
        "a different contract version does not apply"
    );
}

/// What gets serialized carries no native paths nor debug representations:
/// the renderer must not receive authority over a path by accident.
#[test]
fn nada_serializado_lleva_una_ruta_cruda() {
    let json = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/updates.json"),
    )
    .expect("fixture");
    for prohibido in ["VPath", "PathBuf", "OsString", "wire:", "file:///"] {
        assert!(
            !json.contains(prohibido),
            "the bridge must not carry {prohibido}"
        );
    }
}

/// ALL the changes, one by one.
///
/// The `updates.json` family nails down two example patches, and that left
/// [`ViewChange`] variants that were never serialized in any test — which is
/// how one of them can end up IMPOSSIBLE to serialize with nothing turning
/// red. Here, 1:1 coverage is against the list of variants.
#[test]
fn cada_cambio_cruza_el_bridge() {
    let mut casos = cambios_del_listado();
    casos.extend(cambios_de_pantalla());
    check_family("changes.json", &casos);
}

/// The ones that describe a LISTING.
fn cambios_del_listado() -> Vec<(&'static str, ViewChange)> {
    vec![
        (
            "connection",
            ViewChange::Connection(ConnectionView::Lost {
                reason_key: "conn-lost".to_owned(),
            }),
        ),
        (
            "cursor",
            ViewChange::Cursor {
                slot_id: 1,
                generation: 4,
                cursor: Some(RowKey(2)),
            },
        ),
        (
            "dialogs",
            ViewChange::Dialogs {
                dialogs: vec![dialogo_de_referencia()],
            },
        ),
        (
            "columns",
            ViewChange::Columns {
                slot_id: 1,
                columns: vec![ColumnHeader {
                    id: "size".to_owned(),
                    label: "Tamaño".to_owned(),
                    sort: Some("desc".to_owned()),
                    sortable: true,
                    width: None,
                    align: "right".to_owned(),
                }],
            },
        ),
    ]
}

/// The changes that describe an OVERLAY: every surface that opens on top.
fn cambios_de_overlay() -> Vec<(&'static str, ViewChange)> {
    vec![
        (
            "settings",
            ViewChange::Settings {
                settings: Some(ajustes_de_referencia()),
            },
        ),
        (
            "extensions",
            ViewChange::Extensions {
                extensions: Some(extensiones_de_referencia()),
            },
        ),
        (
            "agents",
            ViewChange::Agents {
                agents: Some(agentes_de_referencia()),
            },
        ),
        (
            "plugin_output",
            ViewChange::PluginOutput {
                output: Some(salida_de_referencia()),
            },
        ),
        (
            "program_output",
            ViewChange::ProgramOutput {
                output: Some(programa_de_referencia()),
            },
        ),
        (
            "theme",
            ViewChange::Theme {
                theme: Some(tema_de_referencia()),
            },
        ),
        (
            "picker",
            ViewChange::Picker {
                picker: Some(selector_de_referencia()),
            },
        ),
        (
            "layouts",
            ViewChange::Layouts {
                layouts: Some(disposiciones_de_referencia()),
            },
        ),
        (
            "columns_picker",
            ViewChange::ColumnsPicker {
                columns: Some(columnas_de_referencia()),
            },
        ),
        (
            "search",
            ViewChange::Search {
                search: Some(busqueda_de_referencia()),
            },
        ),
        (
            "compare",
            ViewChange::Compare {
                compare: Some(comparacion_de_referencia()),
            },
        ),
        (
            "sync",
            ViewChange::Sync {
                sync: Some(sincronizacion_de_referencia()),
            },
        ),
        (
            "help",
            ViewChange::Help {
                help: Some(ayuda_de_referencia()),
            },
        ),
    ]
}

/// The ones that describe the SCREEN: layout, overlays and global state.
fn cambios_de_pantalla() -> Vec<(&'static str, ViewChange)> {
    let mut casos = cambios_de_overlay();
    casos.extend(cambios_de_listado());
    casos.extend(cambios_del_resto());
    casos
}

/// The ones that describe a LISTING: its rows and its header.
///
/// Kept apart from the rest because `cambios_de_pantalla` was going over a
/// hundred lines once the header was added, and because these two travel
/// together in the same patch.
fn cambios_de_listado() -> Vec<(&'static str, ViewChange)> {
    vec![
        (
            "rows",
            ViewChange::Rows {
                slot_id: 1,
                generation: 5,
                first_visible: 40,
                rows: vec![fila(41, "otro.txt", false)],
                // With the icon column OPEN, which is how icons land: a rows
                // patch is what opens it in the renderer.
                icon_column: true,
                total_rows: Some(120),
            },
        ),
        (
            // The header travels with the rows, and its four texts come
            // from THIRD PARTIES — a path, two phrases with a number, and a
            // reinterpreted name — so its wire shape is pinned down here.
            "browser_header",
            ViewChange::BrowserHeader {
                slot_id: 1,
                path_display: "⟨mem⟩/casa/caf\u{fffd}".to_owned(),
                path_hostile: true,
                skipped_note: "⚠ 2 entradas omitidas (nombres hostiles/límites)".to_owned(),
                hidden_note: "3 ocultas".to_owned(),
                names_note: "nombres: cp866".to_owned(),
                filling_note: "cargando… (3)".to_owned(),
                pruned_note: "2 marcas caídas, sus entradas ya no están".to_owned(),
                marked_note: "2 marcadas, 4,0 kB".to_owned(),
                footer: "1 dirs · 2 ficheros · 4,0 kB · 2 marcadas, 4,0 kB · 120 GiB libres"
                    .to_owned(),
                path_segments: vec![
                    "⟨mem⟩".to_owned(),
                    "casa".to_owned(),
                    "caf\u{fffd}".to_owned(),
                ],
                used_ratio: None,
                marks: 4,
                mark_ruler: vec![3, 90, 255],
            },
        ),
    ]
}

/// Everything else about the screen that can change.
#[expect(clippy::too_many_lines, reason = "list of literals, no logic")]
fn cambios_del_resto() -> Vec<(&'static str, ViewChange)> {
    vec![
        ("layout", ViewChange::Layout(disposicion_de_referencia())),
        (
            "slot_state",
            ViewChange::SlotState {
                slot_id: 1,
                state: SlotState::Loading {
                    verb_key: "busy-listing".to_owned(),
                    target_display: "⟨mem⟩/casa/docs".to_owned(),
                    target_hostile: false,
                },
            },
        ),
        (
            "status",
            ViewChange::Status(StatusView {
                message: Some("2 entradas".to_owned()),
                banners: Vec::new(),
                notices_unread: 0,
                pending: None,
            }),
        ),
        (
            "menu",
            ViewChange::Menu {
                menu: menu_de_referencia(),
            },
        ),
        (
            "wizard",
            ViewChange::Wizard {
                wizard: Some(asistente_de_referencia()),
            },
        ),
        (
            "panel_bar",
            ViewChange::PanelBar {
                panel_bar: barra_de_paneles_de_referencia(),
            },
        ),
        (
            "status_items",
            ViewChange::StatusItems {
                status_items: elementos_de_estado_de_referencia(),
            },
        ),
        (
            "profiles",
            ViewChange::Profiles {
                profiles: Some(perfiles_de_referencia()),
            },
        ),
        (
            "palette",
            ViewChange::Palette {
                palette: Some(norte_ui_host::dto::PaletteView {
                    query: "orde".to_owned(),
                    rows: vec![norte_ui_host::dto::PaletteRowView {
                        text: "pane.sort-name".to_owned(),
                        desc: "Ordenar por nombre".to_owned(),
                        chord: "ctrl+f3".to_owned(),
                        hostile: false,
                        enabled: true,
                        recent: false,
                    }],
                    cursor: Some(0),
                    total: 42,
                }),
            },
        ),
        (
            "goto",
            ViewChange::Goto {
                goto: Some(ir_a_de_referencia()),
            },
        ),
        (
            "which_key",
            ViewChange::WhichKey {
                whichkey: Some(norte_ui_host::dto::WhichKeyView {
                    title: "ctrl+x".to_owned(),
                    rows: vec![norte_ui_host::dto::WhichKeyRowView {
                        chord: "g".to_owned(),
                        label: "Ir al principio".to_owned(),
                        enabled: true,
                        opens_sequence: false,
                        reason: String::new(),
                    }],
                }),
            },
        ),
        (
            "viewer",
            ViewChange::Viewer {
                viewer: Some(visor_de_referencia()),
            },
        ),
        (
            "ai_rename",
            ViewChange::AiRename {
                ai_rename: Some(plan_ia_de_referencia()),
            },
        ),
        (
            "organize",
            ViewChange::Organize {
                organize: Some(arbol_de_organizar_de_referencia()),
            },
        ),
        (
            "tasks",
            ViewChange::Tasks {
                tasks: vec![task_de_referencia()],
                // The processes panel's cursor travels WITH the board: a row
                // expiring shifts the rest, and the corpus has to pin both
                // down together.
                cursor: Some(0),
            },
        ),
        // The EMPTY board, which is the only shape in which that cursor
        // comes out `null`. Kept apart and not a duplicate: `null` is what
        // the renderer has to distinguish from "I was not told", and
        // without a case pinning it down, a `skip_serializing_if =
        // "Option::is_none"` — the most natural cleanup there is — would
        // turn every empty board into an ABSENT field and leave the
        // highlight lit on nothing, with the whole suite green.
        (
            "tasks_vacio",
            ViewChange::Tasks {
                tasks: Vec::new(),
                cursor: None,
            },
        ),
    ]
}

/// No number from the corpus goes past where an `f64` is exact (#258).
///
/// The renderer receives them as JavaScript's `number`, which is an `f64`:
/// above 2^53 two different integers are the same one. Today they are all
/// small counters and this passes with plenty of room; it exists so that the
/// day someone puts a hash or a random id into a bridge `u64`, the corpus
/// turns red before two rows collide in silence.
#[test]
fn ningun_numero_del_puente_pasa_de_donde_f64_es_exacto() {
    /// 2^53: the last integer an `f64` represents with no lost neighbors.
    const TOPE: u64 = 1 << 53;

    fn recorre(v: &Value, donde: &str, malos: &mut Vec<String>) {
        match v {
            Value::Number(n) => {
                if let Some(u) = n.as_u64()
                    && u > TOPE
                {
                    malos.push(format!("{donde} = {u}"));
                }
            }
            Value::Array(xs) => {
                for (i, x) in xs.iter().enumerate() {
                    recorre(x, &format!("{donde}[{i}]"), malos);
                }
            }
            Value::Object(m) => {
                for (k, x) in m {
                    recorre(x, &format!("{donde}.{k}"), malos);
                }
            }
            _ => {}
        }
    }

    let mut malos = Vec::new();
    for fichero in [
        "updates.json",
        "changes.json",
        "actions.json",
        "acks.json",
        "envelope.json",
    ] {
        for (caso, valor) in load(fichero) {
            recorre(&valor, &format!("{fichero}/{caso}"), &mut malos);
        }
    }
    assert!(
        malos.is_empty(),
        "a bridge number goes past 2^53 and the renderer would round it off: {malos:?}"
    );
}

/// The corpus's SHAPE, summarized into a number, and that number lives next
/// to `BRIDGE_VERSION`.
///
/// The renderer's contract test already catches the two version constants —
/// Rust's and TypeScript's — drifting apart. What nobody caught was
/// re-blessing the corpus WITHOUT bumping either one: on this bridge every
/// new shape is incompatible, because the version is compared by exact
/// equality and a renderer on a different one is shut out with a fatal
/// screen. So a field added, renamed or removed with no bump is an old
/// renderer silently reading `undefined`.
///
/// What is summarized is the SHAPE, not the content: the set of key paths,
/// with array indices flattened. A value that changes — a different sample
/// file name, a different number — obligates nothing; a field that appears
/// or leaves does.
///
/// When this turns red, the fix is NOT just updating the number: it is
/// bumping `BRIDGE_VERSION` (and its mirror in `ui/src/types.ts`), writing
/// what changed in `bridge.rs`'s version log, and only then updating it.
#[test]
fn la_forma_del_corpus_no_cambia_sin_subir_el_puente() {
    /// The blessed summary. Updated BY HAND and in the same commit as the
    /// bump, which is exactly the stop this test exists to force.
    // Bridge 70: the panel that paints a PLUGIN (`SlotView::Panel` with its
    // `lines`/`hits`) and clicking one of its zones (`UiAction::PanelClick`,
    // which sends the CELL and not a command; phase 3).
    // Bridge 74: `MenuItemView.section` and `.role` (ADR 0125).
    // Bridge 75: `HelpSpanView::Link.action`, the row the link follows.
    // Bridge 76: `HelpView.scroll`, the request to scroll the body.
    // Bridge 77: `GotoView` ("go to", #357) in the snapshot and its change.
    // Bridge 78: `SlotView::Timeline` (the timeline, #359).
    // Bridge 79: an extension that failed to load carries the id it is
    // uninstalled with (`ExtensionErrorView.id`, ADR 0113).
    // Bridge 80: `View::row_stripes` (the listing's striping) and
    // `ViewerView::image_zoom` (an image's zoom).
    // Bridge 81: settings by section — `SettingsView.index` (the left-hand
    // index), `.query`, `.shown` and `.total` (the search box and its two
    // figures), and `SettingRowView.modified` (the dot for "this is not
    // factory"), plus the `settings_query`, `settings_jump_section` and
    // `settings_reset` actions.
    // Bridge 82: `SettingsView.focus` (which half has the keyboard) and
    // `SettingsSectionView::Settings.key` (the stable key, to pair a section
    // with its index row without matching translated labels).
    // Bridge 83: the CONTROLS. `SettingRowView` gains `control`, `choices`,
    // `min` and `max` — what a switch, a dropdown or a numeric field need to
    // know — and the `settings_set` action puts a concrete value instead of
    // cycling.
    //   And `default`: the factory value, which the window shows as the
    //   placeholder for an empty field — "empty" is not a gap, it is that
    //   value.
    // Bridge 84: the key bar goes away (`View::key_bar`, the `key_bar`
    // change); the panel bar gains `vertical` (column or row) and each
    // button gains `count`, its badge's figure.
    // Bridge 85: `View::status_items` and its change, the status bar's
    // right half (ADR 0132).
    // Bridge 86: `View::layout_buttons`, the layout buttons (ADR 0133).
    // Bridge 87: the places bar's drive gains `free`, `mount` and `kind`
    // (captured 2026-09-21).
    // Bridge 88: `TabGroupView.panels`, the tab group (ADR 0134).
    // Bridge 89: `mark_ruler` in the listing and its header (ADR 0135).
    // (90 does not appear: it added an ACTION — `move_slot` — and this
    // summary only looks at the SHAPES that travel inside a snapshot.)
    // Bridge 91: a dialog can be a FORM (ADR 0143).
    // `DialogView.fields`, with `DialogFieldView` and its three control
    // classes (`text`, `toggle`, `cycle`); on the way back, `dialog_field`
    // with `DialogFieldValue`. Absent and empty = the usual dialog, so a
    // dialog with no form still crosses byte for byte as in 90.
    //
    // 92 (ADR 0146): `StatusItemView.progress`, with `StatusProgressView`
    // (`percent`, `phase`). Absent in the other items, which cross as in 91.
    //
    // 93 (ADR 0147): `TaskStateView::Paused`, one more value of the same
    // field.
    // 94 (ADR 0148): `BrowserSlotView.progress` and the `slot_progress`
    // change.
    const FORMA: u64 = 7_145_087_327_109_203_787;

    let mut rutas: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for fichero in ["changes.json", "updates.json", "variants.json", "acks.json"] {
        for (caso, valor) in load(fichero) {
            // The CASE's name does not count: adding one more case of an
            // already-known shape does not change the contract with the
            // renderer.
            let _ = caso;
            formas(&valor, fichero, &mut rutas);
        }
    }
    let calculada = resumen(&rutas);
    assert_eq!(
        calculada, FORMA,
        "the corpus's shape changed. If it is a new bridge field: bump \
         `BRIDGE_VERSION` and its mirror in `ui/src/types.ts`, write why in \
         `bridge.rs`'s log, and put {calculada} here."
    );
}

/// Every key path of a JSON, with array indices flattened.
fn formas(v: &Value, prefijo: &str, out: &mut std::collections::BTreeSet<String>) {
    match v {
        Value::Object(m) => {
            for (k, hijo) in m {
                let ruta = format!("{prefijo}.{k}");
                out.insert(ruta.clone());
                formas(hijo, &ruta, out);
            }
        }
        Value::Array(xs) => {
            for x in xs {
                formas(x, &format!("{prefijo}[]"), out);
            }
        }
        // A scalar contributes no shape: its PATH was already noted above.
        _ => {}
    }
}

/// A stable summary of a set of strings. FNV-1a: it does not need to be
/// cryptographic — this catches slip-ups, not attacks — and it does need to
/// give the same number on any machine and Rust version, which
/// `DefaultHasher` does not promise.
fn resumen(rutas: &std::collections::BTreeSet<String>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for r in rutas {
        for b in r.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^= u64::from(b'\n');
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Every VARIANT of the bridge's enums crosses at least once (#257).
///
/// `UiAction`'s coverage is already watched over by the compiler
/// (`tag_de_accion` is an exhaustive `match` with no wildcard). The enums
/// that travel INSIDE a snapshot did not have that: `SlotState::Error` — a
/// listing's error path — was never serialized, `TaskStateView` only ever
/// crossed as `running`, and `RowKind` only as `file`, while the renderer
/// decides the folder affordance by looking at `dir`. Here each family is
/// named with an exhaustive `match`, so a new variant does not compile until
/// someone gives it its fixture.
mod variantes {
    use super::{RowKind, SlotState, TaskStateView, check_family, fila};
    use norte_ui_host::dto::{
        CellView, ConnectionView, DialogChoice, DialogView, QuickView, SlotPlacement, SlotRole,
    };

    fn nombre_estado(s: &SlotState) -> &'static str {
        match s {
            SlotState::Ready => "slot_state_ready",
            SlotState::Loading { .. } => "slot_state_loading",
            SlotState::Error { .. } => "slot_state_error",
        }
    }

    fn nombre_conexion(c: &ConnectionView) -> &'static str {
        match c {
            ConnectionView::Connected => "connection_connected",
            ConnectionView::Reconnecting => "connection_reconnecting",
            ConnectionView::Lost { .. } => "connection_lost",
        }
    }

    fn nombre_task(t: TaskStateView) -> &'static str {
        match t {
            TaskStateView::Queued => "task_queued",
            TaskStateView::Running => "task_running",
            TaskStateView::Paused => "task_paused",
            TaskStateView::Done => "task_done",
            TaskStateView::Failed => "task_failed",
            TaskStateView::Cancelled => "task_cancelled",
        }
    }

    fn nombre_clase(k: RowKind) -> &'static str {
        match k {
            RowKind::Dir => "row_kind_dir",
            RowKind::File => "row_kind_file",
            RowKind::Symlink => "row_kind_symlink",
            RowKind::Other => "row_kind_other",
        }
    }

    /// The EMPTY SHAPES, which are the ones a renderer misreads with nothing
    /// complaining: a `None` is painted the same as a field that never
    /// arrived.
    fn formas_vacias() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            // The destination check's three states, and all three in the
            // corpus on purpose: they are the only thing in the dialog where
            // the ABSENCE of a line asserts something (that the destination
            // confines), so a renderer that confused `checking` with a
            // warning-free `done` would do it in silence. The full variant
            // also pins down the vector's wire name, which with all the
            // fixtures empty did not appear in the corpus.
            (
                "dest_check_not_asked",
                serde_json::to_value(norte_ui_host::dto::DestCheckView::NotAsked).expect("json"),
            ),
            // A form field's three classes (bridge 91): the switch and the
            // cycle carry data inside the tag, so their wire shape does not
            // appear in any other fixture.
            (
                "dialog_field_kind_text",
                serde_json::to_value(norte_ui_host::dto::DialogFieldKind::Text).expect("json"),
            ),
            (
                "dialog_field_kind_toggle",
                serde_json::to_value(norte_ui_host::dto::DialogFieldKind::Toggle { on: true })
                    .expect("json"),
            ),
            (
                "dialog_field_kind_cycle",
                serde_json::to_value(norte_ui_host::dto::DialogFieldKind::Cycle {
                    value_key: "search-kinds-files".to_owned(),
                })
                .expect("json"),
            ),
            // And the three ways of TOUCHING a field. Here and not among the
            // actions, because that corpus is named after the action's wire
            // tag — one per `UiAction` variant, and `dialog_field` is only
            // one — so the two that carry no payload would have nowhere to
            // appear: a slip in their `tag` would pass the whole Rust suite
            // and only break inside the window.
            (
                "dialog_field_value_text",
                serde_json::to_value(norte_ui_host::action::DialogFieldValue::Text {
                    text: "1M".to_owned(),
                })
                .expect("json"),
            ),
            (
                "dialog_field_value_toggled",
                serde_json::to_value(norte_ui_host::action::DialogFieldValue::Toggled)
                    .expect("json"),
            ),
            (
                "dialog_field_value_cycled",
                serde_json::to_value(norte_ui_host::action::DialogFieldValue::Cycled)
                    .expect("json"),
            ),
            (
                "dest_check_checking",
                serde_json::to_value(norte_ui_host::dto::DestCheckView::Checking).expect("json"),
            ),
            (
                "dest_check_done_vacio",
                serde_json::to_value(norte_ui_host::dto::DestCheckView::Done {
                    warnings: Vec::new(),
                })
                .expect("json"),
            ),
            (
                "dest_check_done_con_avisos",
                serde_json::to_value(norte_ui_host::dto::DestCheckView::Done {
                    warnings: vec![
                        "4,2 GB a escribir y 1,1 GB libres en el destino".to_owned(),
                        "este destino no puede confinar las escrituras".to_owned(),
                    ],
                })
                .expect("json"),
            ),
            // The FULL splash screen (bridge 69). In the corpus it only ever
            // appeared as `null` — no case opens it — and a shape that is
            // not painted in any fixture is not watched by the guard: its
            // rows could be renamed or its deadline dropped with nothing
            // turning red. With this case, its fields are a contract like
            // the rest.
            (
                "splash_lleno",
                serde_json::to_value(norte_ui_host::dto::SplashView {
                    art: vec!["   ·   ".to_owned()],
                    version: "0.1.0".to_owned(),
                    revision: "abcdef1".to_owned(),
                    daemon: "hablando con el core embebido".to_owned(),
                    hint: "una tecla la quita; 1-9 abre".to_owned(),
                    sections: vec![norte_ui_host::dto::SplashSectionView {
                        title: "A dónde sueles ir".to_owned(),
                        rows: vec![norte_ui_host::dto::SplashRowView {
                            number: 1,
                            label: "casa".to_owned(),
                            detail: "12".to_owned(),
                        }],
                    }],
                    close_after_ms: Some(1_200),
                })
                .expect("json"),
            ),
            (
                "cell_text_none",
                serde_json::to_value(CellView {
                    column: "size".to_owned(),
                    text: None,
                })
                .expect("json"),
            ),
        ]
        .into_iter()
        .chain(formas_vacias_de_reparto())
        .collect()
    }

    /// The empty shapes of LAYOUT and of the listing: placements, cells
    /// with no value, the quick jump, and what a dialog leaves unset.
    ///
    /// Separated from the previous ones only by size: a hundred-line list
    /// is not readable, and the cut falls where the topic changes.
    fn formas_vacias_de_reparto() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            (
                "placement_role_none",
                serde_json::to_value(SlotPlacement {
                    slot_id: 9,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 4,
                    role: None,
                    focus_index: 0,
                })
                .expect("json"),
            ),
            (
                "placement_role_target",
                serde_json::to_value(SlotPlacement {
                    slot_id: 2,
                    x: 10,
                    y: 0,
                    width: 10,
                    height: 4,
                    role: Some(SlotRole::Target),
                    focus_index: 1,
                })
                .expect("json"),
            ),
            (
                "quick_jump",
                serde_json::to_value(QuickView {
                    query: "ca".to_owned(),
                    mode: "jump".to_owned(),
                    matches: 3,
                })
                .expect("json"),
            ),
            (
                "dialog_input_none",
                serde_json::to_value(DialogView {
                    id: norte_ui_host::ModalId(1),
                    title_key: "modal-delete-title".to_owned(),
                    destination: None,
                    subject: None,
                    asker: None,
                    deadline: None,
                    deadline_at_ms: None,
                    body: Vec::new(),
                    overflow_note: String::new(),
                    overflow_hostile: false,
                    choices: vec![DialogChoice {
                        id: "cancel".to_owned(),
                        label_key: "choice-cancel".to_owned(),
                        destructive: false,
                    }],
                    input: None,
                    input_hostile: false,
                    input_secret: false,
                    fields: Vec::new(),
                    dest_check: norte_ui_host::dto::DestCheckView::NotAsked,
                })
                .expect("json"),
            ),
        ]
    }

    #[test]
    fn cada_variante_de_enum_tiene_su_fixture() {
        let estados = vec![
            SlotState::Ready,
            // WITH a destination, which is the half that makes it readable
            // that the body keeps showing the previous listing while it
            // waits.
            SlotState::Loading {
                verb_key: "busy-listing".to_owned(),
                target_display: "⟨mem⟩/casa/docs".to_owned(),
                target_hostile: false,
            },
            SlotState::Error {
                reason_key: "err-permission-denied".to_owned(),
                detail: Some("EACCES".to_owned()),
            },
        ];
        let conexiones = vec![
            ConnectionView::Connected,
            ConnectionView::Reconnecting,
            ConnectionView::Lost {
                reason_key: "err-daemon-gone".to_owned(),
            },
        ];
        let tasks = vec![
            TaskStateView::Queued,
            TaskStateView::Running,
            TaskStateView::Paused,
            TaskStateView::Done,
            TaskStateView::Failed,
            TaskStateView::Cancelled,
        ];
        let clases = vec![
            RowKind::Dir,
            RowKind::File,
            RowKind::Symlink,
            RowKind::Other,
        ];

        let mut casos: Vec<(&str, serde_json::Value)> = Vec::new();
        for e in &estados {
            casos.push((nombre_estado(e), serde_json::to_value(e).expect("json")));
        }
        for c in &conexiones {
            casos.push((nombre_conexion(c), serde_json::to_value(c).expect("json")));
        }
        for t in &tasks {
            casos.push((nombre_task(*t), serde_json::to_value(t).expect("json")));
        }
        for k in &clases {
            let mut f = fila(1, "x", false);
            f.kind = *k;
            casos.push((nombre_clase(*k), serde_json::to_value(&f).expect("json")));
        }
        casos.extend(formas_vacias());
        casos.sort_by(|a, b| a.0.cmp(b.0));
        check_family("variants.json", &casos);
    }
}
