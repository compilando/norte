//! El contrato del bridge, clavado.
//!
//! Un renderer que no está escrito en Rust no comparte tipos con el host:
//! comparte JSON. Estas fixtures son ese acuerdo, y romperlas sin querer es
//! exactamente el fallo del que protegen — un campo renombrado en Rust que
//! deja al renderer leyendo `null` sin que nada se ponga rojo.
//!
//! La cobertura es 1:1 entre las fixtures y los casos de este fichero, y
//! ADEMÁS `tag_de_accion` es un `match` exhaustivo sin comodín: añadir una
//! variante a `UiAction` deja de compilar aquí. Sin eso la promesa era falsa
//! —se comparaba el JSON contra una lista escrita a mano, no contra el
//! enum— y ya se había colado `search_activate_row`, que cruzaba el cable
//! sin fixture mientras esta cabecera decía que era imposible.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::Path;

use norte_ui_host::action::UiAction;
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
        badge: String::new(),
        badge_hostile: false,
        badge_role: String::new(),
    }
}

/// La misma fila, con la insignia que le puso un plugin.
///
/// La insignia y su rol cruzan JSON aquí y en ningún otro sitio: son lo que
/// un TERCERO pinta pegado a un nombre de fichero.
fn fila_adornada(key: u64, nombre: &str) -> RowView {
    RowView {
        badge: "M".to_owned(),
        badge_hostile: false,
        badge_role: "warning".to_owned(),
        ..fila(key, nombre, false)
    }
}

#[test]
fn acciones() {
    let mut casos = acciones_de_fila();
    casos.extend(acciones_de_overlay());
    casos.extend(acciones_de_pantalla());
    // Cada caso se llama como su variante: es lo que hace que la cobertura la
    // vigile el COMPILADOR y no una lista.
    for (nombre, accion) in &casos {
        assert_eq!(
            *nombre,
            tag_de_accion(accion),
            "el caso `{nombre}` no se llama como su variante"
        );
    }
    check_family("actions.json", &casos);
}

/// El tag de cada acción, en un `match` EXHAUSTIVO y sin comodín.
///
/// Es el guardia que faltaba. `check_family` compara las fixtures con una
/// lista de casos escrita a mano, así que una variante nueva sin caso pasaba
/// sin que nada dijera nada —y pasó: `SearchActivateRow` cruzaba el cable sin
/// fixture—. Con esto, añadir una variante rompe la compilación de este
/// fichero, que es donde hay que enterarse.
fn tag_de_accion(a: &UiAction) -> &'static str {
    match a {
        UiAction::MoveCursor { .. } => "move_cursor",
        UiAction::SelectRow { .. } => "select_row",
        UiAction::ToggleMark { .. } => "toggle_mark",
        UiAction::MarkRange { .. } => "mark_range",
        UiAction::Activate { .. } => "activate",
        UiAction::Parent { .. } => "parent",
        UiAction::History { .. } => "history",
        UiAction::SetVisibleRange { .. } => "set_visible_range",
        UiAction::SortBy { .. } => "sort_by",
        UiAction::FocusSlot { .. } => "focus_slot",
        UiAction::Dialog { .. } => "dialog",
        UiAction::DialogInput { .. } => "dialog_input",
        UiAction::CancelTask { .. } => "cancel_task",
        UiAction::CompareSelectRow { .. } => "compare_select_row",
        UiAction::CompareActivateRow { .. } => "compare_activate_row",
        UiAction::CompareToggleFilter { .. } => "compare_toggle_filter",
        UiAction::CompareSetVisibleRange { .. } => "compare_set_visible_range",
        UiAction::SetViewport { .. } => "set_viewport",
        UiAction::Key(_) => "key",
        UiAction::SetViewerRows { .. } => "set_viewer_rows",
        UiAction::HelpSelectTopic { .. } => "help_select_topic",
        UiAction::HelpActivate { .. } => "help_activate",
        UiAction::SettingsSelectRow { .. } => "settings_select_row",
        UiAction::ExtensionSelectRow { .. } => "extension_select_row",
        UiAction::PickerSelectRow { .. } => "picker_select_row",
        UiAction::PlaceActivateRow { .. } => "place_activate_row",
        UiAction::LayoutActivateRow { .. } => "layout_activate_row",
        UiAction::SearchActivateRow { .. } => "search_activate_row",
        UiAction::AiRenameDecide { .. } => "ai_rename_decide",
        UiAction::Resync => "resync",
    }
}

/// Las que nombran una fila: llevan clave Y generación (ADR 0068).
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
        (
            "compare_select_row",
            UiAction::CompareSelectRow { id: 7 },
        ),
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
            },
        ),
        (
            "dialog_input",
            UiAction::DialogInput {
                id: ModalId(3),
                text: "carpeta nueva".to_owned(),
            },
        ),
    ]
}

/// Las que nombran una fila de un OVERLAY por su índice.
///
/// Dos llevan generación —la barra lateral y el selector se llenan desde una
/// tarea de fondo, así que su lista cambia sin que el usuario toque nada— y
/// las demás no, porque no pueden cambiar sin un gesto suyo.
fn acciones_de_overlay() -> Vec<(&'static str, UiAction)> {
    vec![
        (
            "settings_select_row",
            UiAction::SettingsSelectRow { row: 2 },
        ),
        (
            "extension_select_row",
            UiAction::ExtensionSelectRow { row: 1 },
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
            "layout_activate_row",
            UiAction::LayoutActivateRow { row: 0 },
        ),
        (
            "search_activate_row",
            UiAction::SearchActivateRow { row: 2 },
        ),
        ("help_select_topic", UiAction::HelpSelectTopic { row: 3 }),
        ("help_activate", UiAction::HelpActivate { index: 1 }),
    ]
}

/// Las demás: pantalla, teclado, diálogos y tasks.
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
        (
            "set_viewport",
            UiAction::SetViewport {
                width: 120,
                height: 40,
            },
        ),
        (
            "sort_by",
            UiAction::SortBy {
                slot_id: 1,
                column: "size".to_owned(),
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
        ("resync", UiAction::Resync),
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

/// El snapshot de referencia: una pantalla con un listado (una fila hostil),
/// un hueco que este host aún no proyecta, un diálogo y una task viva.
/// El diálogo que clavan las fixtures.
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
        body: vec![norte_ui_host::dto::DialogLine {
            text: "/home/oscar".to_owned(),
            hostile: true,
        }],
        overflow_note: "… se enseñan 1 de 3".to_owned(),
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
    }
}

/// El plan de renombrado que clavan las fixtures.
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

/// La task que clavan las fixtures.
fn task_de_referencia() -> TaskView {
    TaskView {
        task_id: 7,
        kind: "copy".to_owned(),
        state: TaskStateView::Running,
        percent: Some(40),
        detail: Some("notas.txt".to_owned()),
        detail_hostile: false,
        foreign: false,
    }
}

/// El reparto que las fixtures clavan: dos huecos, papeles puestos.
fn disposicion_de_referencia() -> LayoutView {
    LayoutView {
        cells: (120, 40),
        placements: vec![
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
            // Los otros tres huecos de `slots` también se COLOCAN. Sin esto
            // el corpus describía una pantalla que nombra cinco huecos y
            // pinta dos, así que un renderer podía pasar el contrato sin
            // saber pintar la barra lateral, la ficha ni los procesos.
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
        ],
    }
}

/// El visor que clavan las fixtures.
fn visor_de_referencia() -> norte_ui_host::dto::ViewerView {
    norte_ui_host::dto::ViewerView {
        path_display: "⟨file⟩/home/oscar/notas.txt".to_owned(),
        path_hostile: false,
        encoding: "UTF-8".to_owned(),
        eol: "lf".to_owned(),
        hex: false,
        forced: false,
        had_errors: false,
        truncated: true,
        total_rows: 120,
        first_line: 4,
        lines: vec!["quinta línea".to_owned()],
        // Lo enseña un PREVIEWER, y se dice de quién es: un plugin puede
        // enseñar cualquier cosa —ese es su trabajo— y quien mira tiene
        // derecho a saber que no está viendo los bytes del fichero.
        preview_by: "via PDF de ACME".to_owned(),
        preview_lossy: true,
        image: Some(norte_ui_host::dto::ImageView {
            format: "PNG".to_owned(),
            width: 1920,
            height: 1080,
        }),
        image_refused: String::new(),
    }
}

/// Los huecos de la foto de referencia: un listado, la hoja de atributos, el
/// panel de procesos y uno de un tipo que este host no proyecta.
fn slots_de_referencia() -> Vec<SlotView> {
    vec![
        SlotView::Browser(Box::new(BrowserSlotView {
            slot_id: 1,
            generation: 4,
            path_display: "⟨file⟩/home/oscar".to_owned(),
            path_hostile: false,
            total_rows: Some(3),
            first_visible: 0,
            rows: vec![
                fila(1, "notas.txt", false),
                fila(2, "caf\u{FFFD}.txt", true),
                fila_adornada(3, "cambiado.rs"),
            ],
            cursor: Some(RowKey(1)),
            marks: 0,
            // El provider se saltó dos: se DICE. Un listado al que le faltan
            // entradas y no lo avisa miente por omisión.
            skipped_note: "se saltaron 2 entradas".to_owned(),
            columns: vec![
                ColumnHeader {
                    id: "name".to_owned(),
                    label: "Nombre".to_owned(),
                    sort: Some("asc".to_owned()),
                    sortable: true,
                },
                ColumnHeader {
                    id: "size".to_owned(),
                    label: "Tamaño".to_owned(),
                    sort: None,
                    sortable: true,
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
        })),
        SlotView::Processes {
            slot_id: 6,
            cursor: Some(0),
        },
        // La barra lateral cruza JSON AQUÍ y en ningún otro sitio hasta hoy,
        // y es la única variante de `SlotView` con newtype dentro de un enum
        // etiquetado por `kind`: su forma en el cable no se parece a la de
        // sus hermanas y no había nada que la clavara. Sus tres clases de
        // fila van las tres, incluida la rota con su motivo.
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
        SlotView::Unsupported {
            slot_id: 2,
            kind_name: "compare".to_owned(),
            kind_name_hostile: false,
        },
    ]
}

fn snapshot_de_referencia() -> ViewSnapshot {
    ViewSnapshot {
        compare: None,
        slots: slots_de_referencia(),
        connection: ConnectionView::Connected,
        layout: disposicion_de_referencia(),
        focus: Some(1),
        status: StatusView {
            message: Some("2 entradas".to_owned()),
            banners: vec![],
            pending: Some(norte_ui_host::dto::PendingView {
                chords: "ctrl+x".to_owned(),
                count: Some(12),
            }),
        },
        dialogs: vec![dialogo_de_referencia()],
        tasks: vec![task_de_referencia()],
        palette: Some(norte_ui_host::dto::PaletteView {
            query: "orde".to_owned(),
            rows: vec![norte_ui_host::dto::PaletteRowView {
                text: "pane.sort-name".to_owned(),
                desc: "Ordenar por nombre".to_owned(),
                chord: "ctrl+f3".to_owned(),
                enabled: true,
            }],
            cursor: Some(0),
            total: 42,
        }),
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
        theme: Some(tema_de_referencia()),
        search: Some(busqueda_de_referencia()),
        layouts: Some(disposiciones_de_referencia()),
        columns: Some(columnas_de_referencia()),
        picker: Some(selector_de_referencia()),
        viewer: Some(visor_de_referencia()),
        // Con plan, como el resto de overlays de esta foto: si va a `None`,
        // el sitio del campo dentro del snapshot no lo clava nadie.
        ai_rename: Some(plan_ia_de_referencia()),
        locale: "es".to_owned(),
    }
}

/// El tema de referencia: dos roles y un efecto que este renderer no pinta.
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
    }
}

/// La búsqueda de referencia: dos hallazgos, uno con nombre hostil, y
/// todavía corriendo.
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

/// La comparación de referencia: una fila igual y un huérfano de la izquierda
/// con nombre hostil, y una categoría escondida.
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
                // Sin tama\u{f1}o: un hu\u{e9}rfano sin hidratar no lo sabe, y eso
                // viaja como AUSENCIA y no como un cero fabricado.
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

/// El selector de disposiciones de referencia: una de fábrica que comparte
/// nombre con un preset de teclado, y una del usuario que no parsea.
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

/// El selector de COLUMNAS de referencia.
///
/// Sus cuatro filas son los cuatro casos que el modelo distingue: la fija
/// —el nombre—, una builtin con formato ciclable, un `attr:` cuyo formato lo
/// clava el esquema, y un id que NO parsea, que se preserva porque es
/// intención de configuración del usuario.
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
    }
}

/// Un selector de referencia: volúmenes, con uno de solo lectura.
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

/// El gestor de extensiones de referencia: una extensión aprobada y
/// encendida, otra que no, un directorio que no cargó y una ficha abierta.
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
                },
                // Un `enum` cuyo dominio lleva texto del plugin con un
                // override bidi dentro: llega enmascarado Y marcado, y el
                // `·` que lo une no puede fabricarse desde el `plugin.toml`.
                ExtensionConfigRowView {
                    key: "mode".to_owned(),
                    kind: "enum".to_owned(),
                    value: "safe".to_owned(),
                    default: "safe".to_owned(),
                    description: String::new(),
                    domain: "safe · fast\u{fffd} · read-only".to_owned(),
                    hostile: true,
                },
            ],
        }),
        loading: false,
        errors: vec![ExtensionErrorView {
            dir: "/home/oscar/.config/norte/plugins/roto".to_owned(),
            hostile: false,
            reason: "el manifiesto no parsea".to_owned(),
            reason_hostile: false,
        }],
    }
}

/// Los ajustes de referencia: una entrada del registro con su valor efectivo,
/// y una sección de ubicaciones con una que falta.
fn ajustes_de_referencia() -> norte_ui_host::dto::SettingsView {
    use norte_ui_host::dto::{PathRowView, SettingRowView, SettingsSectionView, SettingsView};
    SettingsView {
        sections: vec![
            SettingsSectionView::Settings {
                title: "General".to_owned(),
                rows: vec![
                    SettingRowView {
                        id: "ui.confirm-quit".to_owned(),
                        name: "Confirmar al salir".to_owned(),
                        desc: "Pregunta antes de cerrar norte".to_owned(),
                        value: "siempre".to_owned(),
                        hostile: false,
                        restart_required: true,
                    },
                    // Un valor que el USUARIO escribió en su `norte.toml` con
                    // un override bidi dentro: llega enmascarado y marcado.
                    SettingRowView {
                        id: "ui.font".to_owned(),
                        name: "Tipografía".to_owned(),
                        desc: "La fuente de la ventana".to_owned(),
                        value: "Fira\u{fffd}Code".to_owned(),
                        hostile: true,
                        restart_required: true,
                    },
                ],
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
        ],
        cursor: 0,
        read_only: true,
    }
}

/// La ayuda de referencia: una página con prosa, una marca viva ya resuelta,
/// un enlace, la hoja de teclado y una fila que este frontend no ejecuta.
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

/// TODOS los cambios, uno a uno.
///
/// La familia de `updates.json` clava dos parches de ejemplo, y eso dejaba
/// variantes de [`ViewChange`] que jamás se serializaban en ningún test —
/// que es como una de ellas puede resultar IMPOSIBLE de serializar sin que
/// nada se ponga rojo. Aquí la cobertura 1:1 es contra la lista de variantes.
#[test]
fn cada_cambio_cruza_el_bridge() {
    let mut casos = cambios_del_listado();
    casos.extend(cambios_de_pantalla());
    check_family("changes.json", &casos);
}

/// Los que describen un LISTADO.
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
                }],
            },
        ),
    ]
}

/// Los cambios que describen un OVERLAY: cada superficie que se abre encima.
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
            "help",
            ViewChange::Help {
                help: Some(ayuda_de_referencia()),
            },
        ),
    ]
}

/// Los que describen la PANTALLA: disposición, overlays y estado global.
fn cambios_de_pantalla() -> Vec<(&'static str, ViewChange)> {
    let mut casos = cambios_de_overlay();
    casos.extend(vec![
        ("layout", ViewChange::Layout(disposicion_de_referencia())),
        (
            "rows",
            ViewChange::Rows {
                slot_id: 1,
                generation: 5,
                first_visible: 40,
                rows: vec![fila(41, "otro.txt", false)],
            },
        ),
        (
            "slot_state",
            ViewChange::SlotState {
                slot_id: 1,
                state: SlotState::Loading,
            },
        ),
        (
            "status",
            ViewChange::Status(StatusView {
                message: Some("2 entradas".to_owned()),
                banners: Vec::new(),
                pending: None,
            }),
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
                        enabled: true,
                    }],
                    cursor: Some(0),
                    total: 42,
                }),
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
            "tasks",
            ViewChange::Tasks {
                tasks: vec![task_de_referencia()],
            },
        ),
    ]);
    casos
}

