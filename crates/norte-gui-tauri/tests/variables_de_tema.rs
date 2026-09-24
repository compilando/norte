//! Every color `var(--x)` in the stylesheet is fed by someone, and every
//! color the host projects is spent by someone.
//!
//! A variable nobody writes does not look broken: it falls back to its
//! default value and stays there forever. `--warn-fg` was exactly that — a
//! typo for `warning-fg`, which is what `theme_roles` really projects —
//! and the log panel's warnings had been ignoring the theme since it was
//! written, painted in a `#fc6` sewn into the CSS.
//!
//! Lives in the RENDERER and not in whoever hosts it, on purpose: `style.css`
//! belongs to the webview, and ADR 0066 forbids `norte-ui-host` from knowing
//! a painting toolkit. Having the renderer check its own stylesheet against
//! what the host projects to it is the correct direction of knowledge.
//!
//! **This test is run by NEITHER `just ci` NOR `just ci-fast`**: `core_pkgs`
//! excludes `norte-gui-tauri` from the portable gate because building it
//! requires `WebKitGTK`. It is run by `just gui-test` (the loop) and
//! `just gui-ci` (its gate, which is what `.github/workflows/gui.yml` runs).
//! Said here because this test's value is catching the next typo, and the
//! next one will run `just ci`.

use std::collections::BTreeSet;

/// GEOMETRY and font variables: they never come from the theme, and that is
/// why they do not have to be in `theme_roles`.
const NO_SON_COLOR: &[&str] = &[
    "cell-w",
    "cell-h",
    "menubar-h",
    "panelbar-h",
    "activity-w",
    "activity-size",
    "depth",
    "busy-delay",
    "menu-left",
    "menu-open",
    "mono",
    "ui-font",
    "ui-font-size",
    "font-mono",
    "font-ui",
    "dialog-backdrop",
    // An OPACITY, not a color: how much the content of the pane without the
    // keyboard dims (ADR 0115). Asking the theme for a role would be asking
    // it to choose a color for something that paints none.
    "inactive-dim",
    // A WIDTH as a percentage: how much of the border's thin line is filled
    // (ADR 0148). The color is set by `border-focus`; this only says how far
    // it reaches.
    "slot-progress",
    // And a WIDTH: how much of that row's task is done, as a percentage. Set
    // by the renderer row by row, not by the theme.
    "pct",
    // Where a plugin panel's clickable zone starts and how big it is, in
    // CELLS (phase 3). Set by the renderer zone by zone, from what the guest
    // said: the frame is text, and a zone is a region of that text. Asking
    // the theme for a role would be asking for a color for a coordinate.
    "hit-col",
    "hit-width",
    // A scale FACTOR: an image's zoom, the percentage the host already
    // carries divided by a hundred (bridge 80). `1` is fitted. Asking the
    // theme for a role would be asking for a color for a multiplier.
    "zoom",
    // An IMAGE: the mark rule already composed (ADR 0135), a gradient with
    // one band per run of marked spans. Its color comes from `--mark-bg` and
    // `--fg`, which ARE the theme's.
    "mark-ruler",
];

/// KNOWN orphans, with an owner and a date. Empty since the `muted` and
/// `badge` roles started feeding what used to be `--dim-fg` and `--chip-bg`.
/// Stays as a constant — and is not deleted — because the mechanism has to
/// exist for the next one: a list of exceptions with an owner is a plan, one
/// without an owner is a leak, and not having a list forces a choice between
/// the two worse options (turning off the test, or leaving the variable
/// unwritten).
const HUERFANAS_CONOCIDAS: &[&str] = &[];

/// Names the agreement has and the sheet does not SPEND yet. **Empty**: since
/// task 7 of the `2026-09-11-vscode-theme.md` plan none is left, and the
/// guard's two directions are alive with no exception beyond geometry. Stays
/// for the same reason as `HUERFANAS_CONOCIDAS`: the mechanism has to exist
/// for the next one that arrives with an owner and a date.
const PENDING_TO_SPEND: &[&str] = &[];

/// The `var(--…)` names that appear in the sheet.
///
/// Searched over the WHOLE text and not line by line because the sheet
/// splits font stacks: `var(` ends up on one line and `--ui-font` on the
/// next.
fn sheet_variables() -> BTreeSet<String> {
    let css = include_str!("../ui/src/style.css");
    let mut out = BTreeSet::new();
    let mut rest = css;
    while let Some(i) = rest.find("var(") {
        rest = &rest[i + "var(".len()..];
        let t = rest.trim_start();
        if let Some(name) = t.strip_prefix("--") {
            let fin = name
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .unwrap_or(name.len());
            if fin > 0 {
                out.insert(name[..fin].to_owned());
            }
        }
    }
    out
}

/// The AGREEMENT: the variable names the host knows, whether or not the
/// color exists in a specific theme.
///
/// It is `theme_names` and not `theme_roles(preset_default())` for a
/// reason that cost a badly written test: the ten chrome roles are not in
/// `Role::CORE`, so the default preset SILENCES them and the sheet derives
/// them — asking a specific theme would have read that silence as "nobody
/// feeds that variable" and would have declared as orphans the nine the spec
/// had just added.
fn acordadas() -> BTreeSet<String> {
    norte_ui_host::pickers::theme_names()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_color_variable_is_fed_by_the_theme() {
    let acordadas = acordadas();
    let huerfanas: Vec<String> = sheet_variables()
        .into_iter()
        .filter(|v| !acordadas.contains(v))
        .filter(|v| !NO_SON_COLOR.contains(&v.as_str()))
        .filter(|v| !HUERFANAS_CONOCIDAS.contains(&v.as_str()))
        .collect();
    assert!(
        huerfanas.is_empty(),
        "color variables nobody feeds: {huerfanas:?}"
    );
}

#[test]
fn every_agreed_color_is_spent_by_the_sheet() {
    let usadas = sheet_variables();
    let unspent: Vec<String> = acordadas()
        .into_iter()
        .filter(|k| !usadas.contains(k))
        .filter(|k| !PENDING_TO_SPEND.contains(&k.as_str()))
        .collect();
    assert!(
        unspent.is_empty(),
        "colors the host projects that the sheet does not paint: {unspent:?}"
    );
}
