//! GUI column-picker overlay state (#108 block 7c, `alt+c`): a thin PURE
//! wrapper over the shared `norte_frontend::columns_picker::ColumnsPicker`
//! — the SAME machine the TUI's Alt+C overlay drives (rule 7: every
//! decision lives in the model; both frontends only paint and route keys).
//! No GPUI here: testable bare. `main.rs` owns rendering and the async
//! persist (same split as `palette_view`/`settings_view`).
//!
//! Keys mirror the TUI's `dialog.*` defaults instead of resolving through
//! the keymap: the GUI's overlay precedent (palette/settings/extensions)
//! matches keystrokes directly, and the `dialog.*` verbs are a TUI keymap
//! concept the GUI never registers.

use norte_frontend::SortSpec;
use norte_frontend::columns_picker::{ColumnsPicker, Picked, PickerRow};

/// Outcome of one keystroke over the panel.
#[derive(Debug, PartialEq)]
pub enum ColumnsOutcome {
    /// Consumed (or ignored): the panel stays open. A modal overlay never
    /// lets keys fall through to the panes below.
    None,
    /// Close WITHOUT applying (Esc discards, same as the TUI).
    Close,
    /// Enter: apply in-session and persist.
    Apply(Picked),
}

/// The panel: the shared model and nothing else (rows/cursor live in it).
#[derive(Debug, Clone)]
pub struct ColumnsView {
    pub picker: ColumnsPicker,
}

impl ColumnsView {
    pub fn new(picker: ColumnsPicker) -> Self {
        Self { picker }
    }

    /// Route one key — TUI-default chords: up/down move the cursor,
    /// space/e toggle, shift+up/down (and shift+k/j) reorder, s sorts by
    /// the cursor's column (ctrl+s accepted too, TUI parity), f cycles the
    /// format, enter applies, escape discards.
    pub fn on_key(&mut self, key: &str, shift: bool, ctrl: bool) -> ColumnsOutcome {
        // ctrl solo existe como sinónimo documentado de sort (ctrl+s);
        // cualquier otro chord ctrl se CONSUME sin efecto — sin esto,
        // ctrl+enter aplicaría y ctrl+e/f mutarían por alias de base-key
        // (review 7c MINOR-1).
        if ctrl && key != "s" {
            return ColumnsOutcome::None;
        }
        match (key, shift) {
            ("escape", _) => return ColumnsOutcome::Close,
            ("enter", _) => return ColumnsOutcome::Apply(self.picker.finish()),
            ("up", true) | ("k", true) => self.picker.move_up(),
            ("down", true) | ("j", true) => self.picker.move_down(),
            ("up", false) => self.picker.up(),
            ("down", false) => self.picker.down(),
            ("space" | "e", _) => self.picker.toggle(),
            ("s", _) => self.picker.sort_current(),
            ("f", false) => self.picker.cycle_format(),
            _ => {}
        }
        ColumnsOutcome::None
    }
}

/// Tope de caracteres del label de un id opaco YA enmascarado (encoding 7c
/// H2): `mask_terminal_hazards` no capa longitud y el label entero llega al
/// `aria_label` — un id kilométrico de un `./.norte` ajeno se leería entero
/// por el lector de pantalla. Mismo valor que `HEADER_MAX_CHARS` (7b).
const OPAQUE_LABEL_MAX_CHARS: usize = 24;

/// Texto pintable de una fila del picker: `(línea, label)` — la línea es
/// `[x] label ▲ · fmt`, el label va aparte para el `aria_label`. TODO texto
/// de terceros pasa por aquí (encoding 7c H1: choke point testeable fuera
/// del render `&self`): builtins vía Fluent, ids opacos ENMASCARADOS y
/// capados; el formato es vocabulario ASCII cerrado (tablas estáticas de
/// `columns.rs`, pineado allí) — seguro en crudo.
#[must_use]
pub fn row_display(row: &PickerRow, sort: SortSpec) -> (String, String) {
    use norte_frontend::columns::{Builtin, sort_column};
    let label = match row.builtin {
        Some(Builtin::Name) => norte_i18n::t("col-header-name"),
        Some(Builtin::Size) => norte_i18n::t("col-header-size"),
        Some(Builtin::Mtime) => norte_i18n::t("col-header-mtime"),
        Some(Builtin::Kind) => norte_i18n::t("col-header-kind"),
        None => norte_encoding::mask_terminal_hazards(&row.id)
            .chars()
            .take(OPAQUE_LABEL_MAX_CHARS)
            .collect(),
    };
    let mark = if row.enabled { "[x]" } else { "[ ]" };
    let arrow = match row.builtin.and_then(sort_column) {
        Some(sc) if sc == sort.column => {
            if sort.dir == norte_frontend::SortDir::Desc {
                " ▼"
            } else {
                " ▲"
            }
        }
        _ => "",
    };
    let fmt = row
        .format
        .as_deref()
        .map(|f| format!(" · {f}"))
        .unwrap_or_default();
    (format!("{mark} {label}{arrow}{fmt}"), label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_frontend::SortSpec;
    use norte_frontend::columns::ColumnsSettings;

    fn view() -> ColumnsView {
        let settings = ColumnsSettings::default();
        ColumnsView::new(ColumnsPicker::open(&settings, "file", SortSpec::default()))
    }

    #[test]
    fn esc_descarta_enter_aplica() {
        let mut v = view();
        assert_eq!(v.on_key("escape", false, false), ColumnsOutcome::Close);
        let mut v = view();
        let ColumnsOutcome::Apply(picked) = v.on_key("enter", false, false) else {
            panic!("enter debe aplicar");
        };
        // The default set always carries name first (pinned).
        assert_eq!(picked.ids.first().map(String::as_str), Some("name"));
    }

    #[test]
    fn toggle_y_reorden_mueven_el_modelo() {
        let mut v = view();
        v.on_key("down", false, false); // cursor to row 1
        let id = v.picker.rows()[1].id.clone();
        let antes = v.picker.rows()[1].enabled;
        v.on_key("space", false, false);
        assert_eq!(v.picker.rows()[1].enabled, !antes, "toggle {id}");
        v.on_key("down", true, false); // shift+down reorders
        assert_eq!(v.picker.rows()[2].id, id, "reordered downward");
    }

    #[test]
    fn sort_y_formato_delegan() {
        let mut v = view();
        v.on_key("down", false, false);
        v.on_key("s", false, false);
        // after_click over row 1's column (size in the default set).
        assert_ne!(v.picker.sort(), SortSpec::default());
        let antes = v.picker.format_of_cursor();
        v.on_key("f", false, false);
        assert_ne!(v.picker.format_of_cursor(), antes, "f cycles the format");
    }

    #[test]
    fn teclas_ajenas_se_consumen_sin_efecto() {
        let mut v = view();
        let filas = v.picker.rows().len();
        assert_eq!(v.on_key("x", false, false), ColumnsOutcome::None);
        assert_eq!(v.picker.rows().len(), filas);
    }

    /// MINOR-1 (review 7c): ctrl solo vale como ctrl+s; el resto de chords
    /// ctrl se consume sin mutar (ctrl+enter NO aplica, ctrl+f NO cicla).
    #[test]
    fn ctrl_solo_es_sinonimo_de_sort() {
        let mut v = view();
        v.on_key("down", false, false);
        let antes = v.picker.format_of_cursor();
        assert_eq!(v.on_key("enter", false, true), ColumnsOutcome::None);
        assert_eq!(v.on_key("f", false, true), ColumnsOutcome::None);
        assert_eq!(v.picker.format_of_cursor(), antes, "ctrl+f no cicla");
        v.on_key("s", false, true); // el sinónimo documentado sí muta
        assert_ne!(v.picker.sort(), SortSpec::default());
    }

    /// H1+H2 (encoding 7c): ids opacos HOSTILES del corpus entran por el
    /// camino real (config→settings→picker) y la línea pintable sale sin
    /// hazards crudos y con el label capado.
    #[test]
    fn row_display_neutraliza_el_corpus_hostil_y_capa() {
        for fixture in norte_testkit::corpus::hostile_names() {
            // Solo los UTF-8: un id de columna es String de config.
            let Ok(id) = String::from_utf8(fixture.bytes.clone()) else {
                continue;
            };
            let cfg = norte_config::ColumnsConfig {
                default_columns: Some(vec![id.clone()]),
                ..Default::default()
            };
            let settings = ColumnsSettings::resolve(&cfg);
            let p = ColumnsPicker::open(&settings, "file", SortSpec::default());
            for row in p.rows() {
                let (line, label) = row_display(row, p.sort());
                assert!(
                    !line.chars().any(norte_encoding::is_terminal_hazard),
                    "hazard crudo en línea para {:?}: {line:?}",
                    fixture.id
                );
                // Los labels Fluent de builtins son cortos; el opaco va
                // capado — todos caben en el tope (H2).
                assert!(
                    label.chars().count() <= OPAQUE_LABEL_MAX_CHARS,
                    "label sin capar para {:?}: {label:?}",
                    fixture.id
                );
            }
        }
    }
}
