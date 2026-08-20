//! El contrato del bridge, clavado.
//!
//! Un renderer que no está escrito en Rust no comparte tipos con el host:
//! comparte JSON. Estas fixtures son ese acuerdo, y romperlas sin querer es
//! exactamente el fallo del que protegen — un campo renombrado en Rust que
//! deja al renderer leyendo `null` sin que nada se ponga rojo.
//!
//! La cobertura es 1:1 en los dos sentidos: cada variante tiene su fixture y
//! cada fixture su variante, así que añadir una acción sin clavarla también
//! falla.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::Path;

use norte_ui_host::action::UiAction;
use norte_ui_host::bridge::{ActionAck, BridgeEnvelope, InstanceId, ModalId, RowKey, StaleAction};
use norte_ui_host::dto::{
    BrowserSlotView, CellView, ConnectionView, DialogChoice, DialogView, RowKind, RowView,
    SlotState, SlotView, StatusView, TaskStateView, TaskView, UiNotice, UiUpdate, ViewChange,
    ViewPatch, ViewSnapshot,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn load(name: &str) -> BTreeMap<String, Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("no se pudo leer {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("fixture JSON válida")
}

/// Cobertura 1:1 entre fixtures y casos, y match exacto en ambos sentidos.
fn check_family<T>(file: &str, cases: &[(&str, T)])
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let fixtures = load(file);
    let fixture_names: Vec<&str> = fixtures.keys().map(String::as_str).collect();
    let mut case_names: Vec<&str> = cases.iter().map(|(n, _)| *n).collect();
    case_names.sort_unstable();
    assert_eq!(
        fixture_names, case_names,
        "[{file}] los casos Rust y las fixtures deben cubrirse 1:1"
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
        assert_eq!(&back, value, "[{file}/{name}] deserialize == construido");
    }
}

fn fila(key: u64, nombre: &str, hostile: bool) -> RowView {
    RowView {
        key: RowKey(key),
        display_name: nombre.to_owned(),
        hostile,
        kind: RowKind::File,
        selected: false,
        marked: false,
        cells: vec![CellView {
            column: "size".to_owned(),
            text: Some("1.2 KiB".to_owned()),
        }],
    }
}

#[test]
fn acciones() {
    check_family(
        "actions.json",
        &[
            (
                "activate",
                UiAction::Activate {
                    slot_id: 1,
                    key: RowKey(9),
                },
            ),
            ("cancel_task", UiAction::CancelTask { task_id: 42 }),
            (
                "dialog",
                UiAction::Dialog {
                    id: ModalId(3),
                    choice: "confirm".to_owned(),
                },
            ),
            (
                "dialog_input",
                UiAction::DialogInput {
                    id: ModalId(3),
                    text: "carpeta nueva".to_owned(),
                },
            ),
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
                "move_cursor",
                UiAction::MoveCursor {
                    slot_id: 1,
                    delta: -1,
                },
            ),
            ("parent", UiAction::Parent { slot_id: 1 }),
            ("resync", UiAction::Resync),
            (
                "select_row",
                UiAction::SelectRow {
                    slot_id: 1,
                    key: RowKey(7),
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
                },
            ),
        ],
    );
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

/// El snapshot de referencia: una pantalla con un listado (una fila hostil),
/// un hueco que este host aún no proyecta, un diálogo y una task viva.
fn snapshot_de_referencia() -> ViewSnapshot {
    ViewSnapshot {
        connection: ConnectionView::Connected,
        slots: vec![
            SlotView::Browser(BrowserSlotView {
                slot_id: 1,
                generation: 4,
                path_display: "⟨file⟩/home/oscar".to_owned(),
                path_hostile: false,
                total_rows: Some(2),
                first_visible: 0,
                rows: vec![
                    fila(1, "notas.txt", false),
                    fila(2, "caf\u{FFFD}.txt", true),
                ],
                cursor: Some(RowKey(1)),
                marks: 0,
                state: SlotState::Ready,
                quick: Some(norte_ui_host::dto::QuickView {
                    query: "no".to_owned(),
                    mode: "filter".to_owned(),
                    matches: 1,
                }),
            }),
            SlotView::Unsupported {
                slot_id: 2,
                kind_name: "processes".to_owned(),
            },
        ],
        focus: Some(1),
        status: StatusView {
            message: Some("2 entradas".to_owned()),
            banners: vec![],
            pending: Some(norte_ui_host::dto::PendingView {
                chords: "ctrl+x".to_owned(),
                count: Some(12),
            }),
        },
        dialogs: vec![DialogView {
            id: ModalId(3),
            title_key: "modal-mkdir-title".to_owned(),
            body: vec!["/home/oscar".to_owned()],
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
        }],
        tasks: vec![TaskView {
            task_id: 7,
            kind: "copy".to_owned(),
            state: TaskStateView::Running,
            percent: Some(40),
            detail: Some("notas.txt".to_owned()),
            foreign: false,
        }],
        locale: "es".to_owned(),
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
                        },
                        ViewChange::SlotState {
                            slot_id: 1,
                            state: SlotState::Loading,
                        },
                    ],
                }),
            ),
            ("snapshot", UiUpdate::Snapshot(snapshot)),
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

/// Una acción con una etiqueta que este host no conoce NO se interpreta.
#[test]
fn una_accion_desconocida_se_rechaza() {
    let crudo = r#"{"action":"format_disk","slot_id":1}"#;
    let out: Result<UiAction, _> = serde_json::from_str(crudo);
    assert!(out.is_err(), "una acción desconocida no se acepta");
}

/// Un sobre de una versión futura se detecta ANTES de mirar el payload.
#[test]
fn un_sobre_futuro_no_se_interpreta() {
    let e: BridgeEnvelope<UiUpdate> = serde_json::from_str(
        r#"{"bridge_version":9999,"instance_id":"host-1","sequence":0,
            "payload":{"update":"notice","notice":"shutdown","incomplete":false}}"#,
    )
    .expect("el sobre se lee");
    assert!(!e.is_supported(), "otra versión del contrato no se aplica");
}

/// Lo serializado no lleva rutas nativas ni representaciones de depuración:
/// el renderer no puede recibir autoridad sobre un path por accidente.
#[test]
fn nada_serializado_lleva_una_ruta_cruda() {
    let json = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/updates.json"),
    )
    .expect("fixture");
    for prohibido in ["VPath", "PathBuf", "OsString", "wire:", "file:///"] {
        assert!(
            !json.contains(prohibido),
            "el bridge no debe llevar {prohibido}"
        );
    }
}
