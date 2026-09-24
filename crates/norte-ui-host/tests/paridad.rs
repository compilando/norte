//! Phase 6's PARITY MATRIX, as a test.
//!
//! The shared catalogue is norte's command vocabulary, and the window
//! implements a part of it. What this file checks is not that the part is
//! large, but that it is CLASSIFIED: every live command the window does not
//! do is down here, either because it does not apply to a window, or pointed
//! at the issue that will close it.
//!
//! The failure this avoids has already happened twice. `pane.copy-path` was
//! declared live for two months without anyone implementing it — the GPUI
//! GUI did, and it was retired — and `task.next`/`prev`/`dismiss` the same,
//! until someone counted. The test that ties the two halves together checks
//! that everything in the TUI is in the catalogue; it did NOT check anything
//! in this direction.
//!
//! Adding a command to the catalogue breaks this test until someone says
//! which of the two lists it falls into. That is all it does, and it is
//! exactly what was missing.

/// Live commands a WINDOW will never have, and why.
const DOES_NOT_APPLY: &[&str] = &[
    // The TUI's command line is a terminal surface; the window's answer is
    // the palette.
    "pane.command-line",
    // `--pick` is a CLI mode: a window has no pipe to answer to.
    "app.pick-accept",
    // `app.quit` used to be here with "the window manager closes it", and it
    // was half true: the window manager does close it, but `F10` and `q` —
    // the quit keys of all seven presets, and the "Quit" menu entry — did
    // NOTHING in the window. Now they ask to quit through the same path as
    // the close button (`request_exit`), with the same `[ui] confirm_quit`
    // question.
    // "Full screen" is a terminal-shaped answer: hiding the panels to see
    // what is behind them means nothing in a window that IS the manager.
    //
    // `app.menu` used to be here and is NOT anymore: the window has a menu
    // bar, with the same menus and the same entries as the TUI, because the
    // model is `norte_frontend::menu` and not a copy.
    "app.toggle-panels",
];

/// Live commands DEFERRED, with the issue that closes them.
const DEFERRED: &[(&str, u32)] = &[
    // `dialog.remove` used to be here with a reason that was true: removing
    // a row only means something for a list that can be EDITED, and this
    // window's only one was read-only. Since #309 there is one that is —
    // favorites — so it left this list and entered
    // `IMPLEMENTADOS_DIALOG`.
    // `layout.preview` was here until #291: it was the only one of the seven
    // in ADR 0058 the window did not paint. Now the `viewer` slot follows
    // the cursor and shows the same viewer as the large one.
    // `layout.log` was here since #323 and left with #326: the window paints
    // the log, mounts the ring at startup, and SAYS which process the lines
    // belong to — which was the nuance the TUI does not have, because there
    // the embedded daemon is the same process. Carrying the daemon's over
    // the wire is still pending, and it is #328.
    // `pane.rename-batch` (#310) was here: the generator and the validation
    // were from the shared crate and the window was only missing the
    // template prompt. It has it now, and the plan enters through the same
    // review as the AI one.
    // Comparing two files (#312) was here: the window did not know how to
    // launch a program and wait for it. Since `NativeEffect::RunProgram`
    // whoever hosts it runs it — detached if it opens a window, capturing
    // its output otherwise — and the output comes back as an action and is
    // shown. The choice of operand and program are still the shared ones
    // (`diffpair`, `[ui] diff`).
    //
    // With this the list is EMPTY: every live command in the catalogue is
    // either done by the window or does not apply to a window.
    // Profiles (ADR 0079) are already in both: the picker, cycling through
    // the list and hot-swapping. What the window still does not do is
    // remember where you left each panel WITHIN each profile — the layout
    // comes from the profile's configuration, not its saved state — and
    // that is the only thing left of #307.
    // Settings (F11) are WRITTEN from the window since bridge 60, with the
    // shared editor. What it still does not have, and is not a catalogue
    // command so this list does not see it: the filter the terminal types
    // over the settings list (its overlay eats everything printable; here
    // printable keys never reach the host), and the plugins section, which
    // is informational in both frontends.
    //
    // "Go anywhere" (phase 6, ADR 0120) was here until #357, and the
    // journal's timeline (phase 7, ADR 0121) until #359: the window paints
    // them now, with `norte_frontend`'s shared models.
    //
    // The terminal panel (#362) was here between T3 and T4, for as long as
    // it took to write the other side. Not anymore: the window opens it,
    // paints it and sends it keys, with the SAME shell and the SAME grid as
    // the terminal (`norte-term`), so both show the same thing by
    // construction and not because someone compares two emulators.
    //
    // With this the list is EMPTY again.
];

/// Every live command is either implemented by the window or classified.
#[test]
fn every_live_command_is_classified() {
    use norte_frontend::keymap::catalogue::{CATALOGUE, Status};
    let done: std::collections::HashSet<&str> = norte_ui_host::commands::IMPLEMENTADOS
        .iter()
        .chain(norte_ui_host::commands::IMPLEMENTADOS_VISOR.iter())
        .chain(norte_ui_host::commands::IMPLEMENTADOS_DIALOG.iter())
        .copied()
        .collect();
    let does_not_apply: std::collections::HashSet<&str> = DOES_NOT_APPLY.iter().copied().collect();
    let deferred: std::collections::HashSet<&str> = DEFERRED.iter().map(|(c, _)| *c).collect();
    for def in CATALOGUE {
        if def.status != Status::Live {
            continue;
        }
        let classified = done.contains(def.name)
            || does_not_apply.contains(def.name)
            || deferred.contains(def.name);
        assert!(
            classified,
            "`{}` is live in the catalogue and the window does not do it: \
             classify it (DOES_NOT_APPLY) or point it at an issue (DEFERRED)",
            def.name
        );
    }
}

/// And the other way around: nothing classified that the window ALREADY
/// does.
///
/// A deferred list that is not cleaned up as the capability is built is a
/// list that lies in the other direction, and the plan's matrix is composed
/// from it.
#[test]
fn nothing_classified_is_built() {
    let done: std::collections::HashSet<&str> = norte_ui_host::commands::IMPLEMENTADOS
        .iter()
        .chain(norte_ui_host::commands::IMPLEMENTADOS_VISOR.iter())
        .chain(norte_ui_host::commands::IMPLEMENTADOS_DIALOG.iter())
        .copied()
        .collect();
    for c in DOES_NOT_APPLY.iter().chain(DEFERRED.iter().map(|(c, _)| c)) {
        assert!(
            !done.contains(c),
            "`{c}` is already done by the window: remove it from the classification"
        );
    }
}

/// BOTH frontends ask for the password; neither one is left painting the
/// error (#325/#327).
///
/// It is not a command, so it does not fall into the lists above — and for
/// that very reason it gets its own test. It is a DECISION duplicated across
/// frontends, which is the kind of thing ADR 0077 exists to keep from
/// diverging silently: the TUI made the call in #325 and the window took two
/// versions to make it, during which a `norte-gui` user over a
/// `secret = "prompt"` connection read the name of an environment variable
/// and got stuck there.
///
/// It is checked by CODE and not by behavior because they are two binaries
/// with two different loops; what this test prevents is someone deleting one
/// of the two arms while the other stays green.
#[test]
fn both_frontends_ask_for_the_secret() {
    let sites = [
        // The TUI: the `cd` that runs into the error opens its modal.
        ("norte-tui", "../norte-tui/src/navigate.rs"),
        // The window: the listing that comes back with the error opens its
        // dialog.
        ("norte-ui-host", "src/controller/listing.rs"),
    ];
    for (who, path) in sites {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        let src = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        assert!(
            src.contains("Error::SecretNeeded"),
            "{who} no longer reacts to `SecretNeeded` in {path}: either it \
             moved, or it once again left the reader facing an error it \
             cannot answer"
        );
    }
}

/// Every issue in the list is a real number.
#[test]
fn every_deferred_entry_has_an_issue() {
    for (c, issue) in DEFERRED {
        assert!(*issue > 0, "`{c}` has no issue");
    }
}
