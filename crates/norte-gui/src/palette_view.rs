//! GUI command palette overlay (G3c, `ctrl+p`): mirrors the TUI's H1
//! design — free filter always active, rows = GUI COMMANDS × Fluent help
//! text × chords, PLUS plugin command rows (approved+enabled, masked,
//! `[extension]`-prefixed). Built on `norte_frontend::palette` (hoisted
//! G3c, the SAME pure `Row`/`plugin_rows` the TUI's `palette.rs` now
//! re-exports) — this file owns the GUI's OWN `build_rows`-equivalent
//! (COMMANDS/help ids are GUI-specific — the TUI keeps its own too, same
//! split as `settings_view`/`norte_frontend::settings`) plus a pure
//! filter/cursor state and keyboard routing. `main.rs` owns the async
//! plugin-row fetch, rendering, and Enter dispatch (built-in vs
//! `plugin:`-prefixed key) — same split as `settings_view::on_key` here /
//! `NorteGui::render_settings`+`commit_settings_write` there.
//!
//! Unlike the TUI (whose palette can open FROM the viewer, `[global]`
//! merges into both screens), the GUI's viewer captures ALL keys through
//! its OWN resolver while open (`on_key`, viewer branch checked BEFORE
//! dual-pane) and `"app.palette"` is not a `VIEWER_COMMANDS` entry — so the
//! palette is unreachable while the viewer is open, and its rows never
//! need `norte_frontend::palette::rows_for_context`'s viewer filter (there
//! are no `viewer.*` rows in [`crate::keymap::COMMANDS`] to begin with).

use norte_frontend::keymap::Effective;
use norte_frontend::palette::{Row, first_chord};
use norte_i18n::t;

use crate::keymap::{COMMANDS, help_id};
use crate::keys::typed_char;

/// Built-in rows: one per [`COMMANDS`] entry, chord from the first
/// resolving binding of `browse` (the GUI has no separate `viewer`
/// fallback to chain — see the module doc for why).
#[must_use]
pub fn build_rows(browse: &Effective) -> Vec<Row> {
    COMMANDS
        .iter()
        .map(|&cmd| {
            let desc = t(&help_id(cmd));
            let chord = first_chord(cmd, browse).unwrap_or_else(|| "—".to_owned());
            Row {
                key: cmd.to_owned(),
                text: cmd.to_owned(),
                desc,
                chord,
            }
        })
        .collect()
}

/// Pure filter+cursor state over a [`Row`] snapshot — mirrors the TUI's
/// `Palette` (`norte-tui/src/app.rs`), free-text filter always active, no
/// inline edit mode (the palette never edits anything, only dispatches).
#[derive(Debug, Clone)]
pub struct PaletteView {
    rows: Vec<Row>,
    query: Vec<u8>,
    visible: Vec<usize>,
    cursor: usize,
}

impl PaletteView {
    /// Opens over `rows` (a [`build_rows`] snapshot — plugin rows are
    /// appended LATER via [`Self::extend`], once the async `plugin.list`
    /// answers).
    #[must_use]
    pub fn new(rows: Vec<Row>) -> Self {
        let mut s = Self {
            rows,
            query: Vec::new(),
            visible: Vec::new(),
            cursor: 0,
        };
        s.recompute();
        s
    }

    /// Appends MORE rows (plugin command rows, arriving async after
    /// `plugin.list`) and re-filters with the CURRENT query — same
    /// "extend, don't replace" criterion `PaneState::extend_listing` uses,
    /// so a query the user already typed keeps matching newly-arrived
    /// plugin rows too.
    pub fn extend(&mut self, rows: impl IntoIterator<Item = Row>) {
        self.rows.extend(rows);
        self.recompute();
    }

    fn recompute(&mut self) {
        self.visible = if self.query.is_empty() {
            (0..self.rows.len()).collect()
        } else {
            let q = norte_frontend::nav::fold(&self.query);
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    norte_frontend::nav::fold(format!("{} {} {}", r.key, r.text, r.desc).as_bytes())
                        .contains(&q)
                })
                .map(|(i, _)| i)
                .collect()
        };
        if self.visible.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len() - 1;
        }
    }

    /// Appends a character to the filter query and recomputes.
    pub fn push_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.query
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        self.recompute();
    }

    /// Removes the last complete UTF-8 char from the query.
    pub fn backspace(&mut self) {
        if self.query.is_empty() {
            return;
        }
        let mut cut = self.query.len() - 1;
        while cut > 0 && (self.query[cut] & 0b1100_0000) == 0b1000_0000 {
            cut -= 1;
        }
        self.query.truncate(cut);
        self.recompute();
    }

    /// Moves the selection up (clamped at the top).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the selection down (clamped at the end).
    pub fn down(&mut self) {
        if self.cursor + 1 < self.visible.len() {
            self.cursor += 1;
        }
    }

    /// REAL indices into [`Self::rows`] visible under the current query.
    #[must_use]
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    /// All rows — `rows()[visible()[i]]` paints the `i`-th filtered row.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Selection position within [`Self::visible`].
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Query text ready to paint (lossy, masked — same contract as
    /// `SettingsState::query_display`).
    #[must_use]
    pub fn query_display(&self) -> String {
        String::from_utf8_lossy(&self.query)
            .chars()
            .map(|c| {
                if norte_encoding::is_terminal_hazard(c) {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect()
    }

    /// The dispatch `key` of the row under the cursor, if any — NEVER
    /// painted (see [`Row`]'s doc); `main.rs` decides what to do with it
    /// (a built-in command name, or a `plugin:{id}:{command}` key).
    #[must_use]
    pub fn selected_key(&self) -> Option<&str> {
        self.visible
            .get(self.cursor)
            .map(|&i| self.rows[i].key.as_str())
    }
}

/// What the caller (`main.rs`) must do after a key.
#[derive(Debug)]
pub enum PaletteOutcome {
    /// Nothing to do beyond a repaint.
    None,
    /// Esc: close the overlay.
    Close,
    /// Enter over a row: its dispatch `key`.
    Run(String),
}

/// Keyboard routing (GPUI key names) — mirrors `settings_view::on_key`'s
/// non-editing branch (the palette has no inline edit mode, only
/// filter+select+dispatch). The caller (`NorteGui::on_palette_key`) has
/// already gated ctrl/alt/platform modifiers out before calling this, same
/// as the settings view.
#[must_use]
pub fn on_key(view: &mut PaletteView, key: &str, key_char: Option<&str>) -> PaletteOutcome {
    match key {
        "escape" => PaletteOutcome::Close,
        "backspace" => {
            view.backspace();
            PaletteOutcome::None
        }
        "up" => {
            view.up();
            PaletteOutcome::None
        }
        "down" => {
            view.down();
            PaletteOutcome::None
        }
        "enter" => match view.selected_key() {
            Some(k) => PaletteOutcome::Run(k.to_owned()),
            None => PaletteOutcome::None,
        },
        _ => {
            if let Some(c) = typed_char(key, key_char) {
                view.push_char(c);
            }
            PaletteOutcome::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coverage (mirrors the TUI's `todo_comando_tiene_ayuda_traducida`,
    /// scoped to the GUI's OWN `COMMANDS`): every entry resolves a REAL
    /// Fluent message in both locales, never falls back to the raw id.
    #[test]
    fn todo_comando_gui_tiene_ayuda_traducida_en_ambos_locales() {
        use norte_i18n::{Lang, t_in};
        for &cmd in COMMANDS {
            let id = help_id(cmd);
            for lang in [Lang::Es, Lang::En] {
                assert_ne!(
                    t_in(lang, &id),
                    id,
                    "falta help-cmd-* para {cmd:?} en {lang:?} (id={id})"
                );
            }
        }
    }

    fn browse_eff() -> Effective {
        crate::keymap::build_effectives_preset_only("orthodox").0
    }

    #[test]
    fn build_rows_una_fila_por_comando() {
        let rows = build_rows(&browse_eff());
        assert_eq!(rows.len(), COMMANDS.len());
        let quit = rows.iter().find(|r| r.key == "app.quit").unwrap();
        assert_eq!(quit.key, quit.text);
        assert_ne!(quit.chord, "—");
    }

    #[test]
    fn palette_view_filtra_y_navega() {
        let mut v = PaletteView::new(build_rows(&browse_eff()));
        for c in "app.quit".chars() {
            v.push_char(c);
        }
        assert_eq!(v.visible().len(), 1);
        assert_eq!(v.selected_key(), Some("app.quit"));
        v.backspace();
        assert!(!v.visible().is_empty());
    }

    #[test]
    fn palette_view_extend_conserva_query_y_suma_filas_de_plugin() {
        let mut v = PaletteView::new(build_rows(&browse_eff()));
        let before = v.rows().len();
        let plugin = norte_proto::methods::PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "0.1.0".into(),
            category: "command".into(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: None,
            commands: vec![norte_proto::methods::PluginCommandInfo {
                id: "greet".into(),
                title: "Greet".into(),
            }],
            columns: Vec::new(),
            has_help: false,
        };
        v.extend(norte_frontend::palette::plugin_rows(&[plugin]));
        assert_eq!(v.rows().len(), before + 1);
        assert_eq!(v.rows().last().unwrap().key, "plugin:org.norte.demo:greet");
    }

    #[test]
    fn on_key_escape_cierra_y_enter_despacha() {
        let mut v = PaletteView::new(build_rows(&browse_eff()));
        assert!(matches!(
            on_key(&mut v, "escape", None),
            PaletteOutcome::Close
        ));
        for c in "app.quit".chars() {
            v.push_char(c);
        }
        match on_key(&mut v, "enter", None) {
            PaletteOutcome::Run(k) => assert_eq!(k, "app.quit"),
            other => panic!("esperaba Run, vino {other:?}"),
        }
    }
}
