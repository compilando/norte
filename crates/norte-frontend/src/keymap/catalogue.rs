//! The command vocabulary, shared. Before this table each frontend owned a
//! private `COMMANDS` list and passed it to the engine as `known_commands`, so
//! the same preset resolved differently in the TUI and the GUI, silently —
//! that is how F1 did nothing in the GUI for several releases (see the H3f
//! comment in `norte-gui/src/keymap.rs`).
//!
//! A frontend still declares WHICH of these it implements. What it no longer
//! does is decide which names EXIST.

/// One command in the shared vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandDef {
    /// Stable name, e.g. `pane.copy` — what a preset binds, the palette shows
    /// and `help_id` mangles into a Fluent id.
    pub name: &'static str,
    /// Whether a numeric count prefix means anything here (K2 consumes it:
    /// `5j` moves five, `5` before `app.quit` is nonsense). Declared with the
    /// command because that is where the answer is known.
    pub counts: bool,
    /// Why a binding to this name may resolve to nothing.
    pub status: Status,
}

/// Whether norte has built this command at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// At least one frontend implements it. WHICH ones is not this table's
    /// business — each frontend declares its own set.
    Live,
    /// A preset may legitimately bind it; norte has not built it yet. Carries
    /// the reason a user is owed and the issue that tracks it.
    Planned {
        /// Fluent id of the short, user-facing reason — NOT the prose itself.
        /// The catalogue must not carry a locale; the frontend translates it
        /// when it prints the message.
        reason: &'static str,
        /// The GitHub issue. Never zero, never invented — pinned by test.
        issue: u32,
    },
}

const fn live(name: &'static str, counts: bool) -> CommandDef {
    CommandDef {
        name,
        counts,
        status: Status::Live,
    }
}

const fn planned(name: &'static str, reason: &'static str, issue: u32) -> CommandDef {
    CommandDef {
        name,
        counts: false,
        status: Status::Planned { reason, issue },
    }
}

/// Every command either frontend knows, plus the ones a preset may honestly
/// bind before norte builds them.
pub const CATALOGUE: &[CommandDef] = &[
    // --- app ---
    live("app.quit", false),
    live("app.help", false),
    live("app.theme", false),
    live("app.settings", false),
    live("app.extensions", false),
    live("app.palette", false),
    // --- pane ---
    live("pane.switch", false),
    live("pane.mirror", false),
    live("pane.pull", false),
    live("pane.swap", false),
    live("pane.copy", false),
    live("pane.move", false),
    live("pane.delete", false),
    live("pane.delete-permanent", false),
    live("pane.mkdir", false),
    live("pane.rename", false),
    live("pane.refresh", false),
    live("pane.view", false),
    live("pane.open", false),
    live("pane.quick-search", false),
    live("pane.history", false),
    live("pane.hotlist", false),
    live("pane.search", false),
    live("pane.names-encoding", false),
    live("pane.toggle-hidden", false),
    live("pane.columns", false),
    live("pane.ai-rename", false),
    live("pane.semantic-search", false),
    live("pane.copy-path", false),
    // --- cursor (the count-aware family) ---
    live("cursor.up", true),
    live("cursor.down", true),
    live("cursor.page-up", true),
    live("cursor.page-down", true),
    live("cursor.top", false),
    live("cursor.bottom", false),
    // --- nav ---
    live("nav.enter", false),
    live("nav.parent", false),
    live("nav.back", true),
    live("nav.forward", true),
    // --- mark ---
    live("mark.toggle", false),
    live("mark.all", false),
    live("mark.invert", false),
    live("mark.clear", false),
    live("mark.pattern-add", false),
    live("mark.pattern-remove", false),
    // --- task ---
    live("task.cancel", false),
    live("task.next", false),
    live("task.prev", false),
    live("task.dismiss", false),
    // --- viewer ---
    live("viewer.close", false),
    live("viewer.up", true),
    live("viewer.down", true),
    live("viewer.page-up", true),
    live("viewer.page-down", true),
    live("viewer.top", false),
    live("viewer.bottom", false),
    live("viewer.encoding", false),
    live("viewer.encoding-auto", false),
    live("viewer.hex", false),
    // --- dialog ---
    live("dialog.confirm", false),
    live("dialog.cancel", false),
    live("dialog.approve", false),
    live("dialog.deny", false),
    live("dialog.overwrite", false),
    live("dialog.skip", false),
    live("dialog.rename", false),
    live("dialog.newer", false),
    live("dialog.up", true),
    live("dialog.down", true),
    live("dialog.page-up", true),
    live("dialog.page-down", true),
    live("dialog.add", false),
    live("dialog.toggle-enabled", false),
    live("dialog.remove", false),
    live("dialog.move-up", false),
    live("dialog.move-down", false),
    live("dialog.sort", false),
    live("dialog.cycle-format", false),
    live("dialog.pane", false),
    live("dialog.back", false),
    live("dialog.filter", false),
    // --- planned: named by a preset, not built yet ---
    planned("pane.select-drive", "keymap-reason-volume-enumeration", 131),
];

/// The entry for `name`, or `None` if the vocabulary has never heard of it —
/// which is a typo, and Task 3 keeps failing the load on it.
///
/// ```
/// use norte_frontend::keymap::catalogue::{Status, lookup};
///
/// assert_eq!(lookup("cursor.down").map(|d| d.counts), Some(true));
/// assert_eq!(lookup("app.quit").map(|d| d.counts), Some(false));
/// assert!(matches!(
///     lookup("pane.select-drive").map(|d| d.status),
///     Some(Status::Planned { issue: 131, .. })
/// ));
/// assert!(lookup("pane.no-existe-jamas").is_none());
/// ```
#[must_use]
pub fn lookup(name: &str) -> Option<&'static CommandDef> {
    CATALOGUE.iter().find(|d| d.name == name)
}

#[cfg(test)]
mod tests {
    use super::{CATALOGUE, Status, lookup};

    /// A duplicated name would make `lookup` order-dependent, and the table is
    /// hand-maintained: pin it.
    #[test]
    fn no_hay_nombres_duplicados() {
        let mut names: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "nombre duplicado en CATALOGUE");
    }

    /// A `Planned` entry with an empty reason or a zero issue is a promise
    /// nobody can chase — the exact failure this state exists to prevent.
    #[test]
    fn todo_planned_tiene_motivo_e_issue() {
        for d in CATALOGUE {
            if let Status::Planned { reason, issue } = d.status {
                assert!(!reason.is_empty(), "{} sin motivo", d.name);
                assert!(issue > 0, "{} sin issue", d.name);
            }
        }
    }

    /// A reason id with no Fluent message renders as the raw id — an unbuilt
    /// key would then explain itself with `keymap-reason-...`, which is worse
    /// than saying nothing. Pin both locales.
    #[test]
    fn todo_motivo_planned_esta_traducido_en_ambos_locales() {
        for d in CATALOGUE {
            if let Status::Planned { reason, .. } = d.status {
                for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                    let s = norte_i18n::t_in(lang, reason);
                    assert_ne!(s, reason, "{} sin traducir en {lang:?}", d.name);
                }
            }
        }
    }

    #[test]
    fn lookup_encuentra_y_falla_bien() {
        assert!(lookup("pane.copy").is_some());
        assert!(lookup("pane.no-existe-jamas").is_none());
    }
}
