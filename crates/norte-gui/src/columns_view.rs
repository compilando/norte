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

use norte_frontend::columns_picker::{ColumnsPicker, Picked};

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
        let _ = ctrl; // ctrl+s and plain s both land on the "s" arm.
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
}
