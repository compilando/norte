//! The body of each modal, and how it is painted.
//!
//! A `*_modal_text` does not paint: it RETURNS the already-composed body, and
//! that is why it can be asserted on without a test backend.
//!
//! **The height comes from the body** ([`body_height`]). It used to be
//! declared by `modal_height`, a table of formulas written by hand per
//! variant, and this rustdoc warned that the two halves drift apart and the
//! modal gets clipped — with not a single check. When one was finally
//! written, for one variant, it turned out the formulas did not even agree
//! among themselves: some added 2 to the line count, others 3, others 4.
//! Deriving it, there are no two numbers that can disagree. The only
//! exception is the modal that WRAPS, and it is marked as such.
//!
//! Each line also declares its ROLE ([`LineKind`]) and the theme decides how
//! to paint it. A modal that declares nothing still comes out as plain text,
//! which is how all of them used to come out: the migration is per modal
//! (ADR 0103).

use norte_theme::Role;
use ratatui::Frame;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use ratatui::layout::Rect;

use super::text::{badge_prefixed, clamp_chars, tail_window};
use super::{HOSTILE_BADGE, centered, clear_themed};
use crate::app::{AI_RENAME_PAIR_LIMIT, App, SEMANTIC_HIT_LIMIT, display_name};
use crate::theme::TuiTheme;
use norte_frontend::middle_ellipsis;
use norte_i18n::{t, ta};

/// Budget in CHARS for a path inside a modal, before middle ellipsis. The
/// same one the approval modal and the collision modal already used:
/// `modal_width` grows up to the frame width, so the clipping is done by
/// the content — never by the box border, which cuts flush.
pub(crate) const MODAL_PATH_CHARS: usize = 46;

/// What a line of a modal's body IS, so it can be painted differently.
///
/// The body used to be ONE string and `draw_modal` painted it as a plain
/// paragraph, so the editable field, the paths, the hint and the keys all
/// came out in the same color and the same weight: a modal with no
/// hierarchy, where the last thing you find is the only thing you can touch.
///
/// It is SEMANTIC, not a color: what is declared here is the line's role,
/// and the theme decides how to paint it. A modal that declares nothing
/// still comes out as plain text, which is how all 26 used to come out — the
/// migration is one line at a time, not a big bang.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LineKind {
    /// A normal piece of data.
    #[default]
    Plain,
    /// A label, a hint or the row of keys: accompanies the data and is not
    /// the data. Dimmed so it does not compete with it.
    Dim,
    /// What has to be read before saying yes: the destination of a copy.
    Strong,
    /// The EDITABLE field. It is the only line the user can change, and
    /// until now there was no way to tell by looking.
    Field,
    /// Something you need to know before confirming, that does not block
    /// confirming.
    Warning,
    /// Why this cannot be done yet.
    Error,
    /// The row of keys painted as BUTTONS (spec 2026-09-10,
    /// `[ui] dialog_buttons`): each `[chord] verb` is a button with the
    /// `button` role, and a mouse zone that synthesizes its chord. If the
    /// line does not have that shape or does not fit, it is painted as
    /// `Dim`.
    Buttons,
}

/// A line of a modal's body.
#[derive(Debug, Clone)]
pub(crate) struct ModalLine {
    /// The text, already masked and bounded by whoever composed it. WITHOUT
    /// the altered-name mark: that goes separately ([`Self::hostile`]).
    pub(crate) text: String,
    /// Which role it plays.
    pub(crate) kind: LineKind,
    /// What is painted DIFFERS from the real bytes, and the line carries the
    /// mark that says so up front (spec §6).
    ///
    /// Separate from the text, and that is not for convenience: the mark is
    /// painted with `Role::HostileBadge`, the only role in this modal with
    /// GUARANTEED contrast (the `norte-theme` gate measures it at 4.5:1 in
    /// all eight presets). Put inside the text it inherited the line's role
    /// style, and a dimmed line left the spec §6 signal at 2.3:1 on a light
    /// theme — a warning that cannot be read is not a warning.
    pub(crate) hostile: bool,
}

impl ModalLine {
    /// A line of a given role.
    pub(crate) fn new(text: impl Into<String>, kind: LineKind) -> Self {
        Self {
            text: text.into(),
            kind,
            hostile: false,
        }
    }

    /// The same line, marked as altered.
    pub(crate) fn hostile(mut self, hostile: bool) -> Self {
        self.hostile = hostile;
        self
    }

    /// A line with no declared role: plain text, as everything used to come
    /// out.
    pub(crate) fn plain(text: impl Into<String>) -> Self {
        Self::new(text, LineKind::Plain)
    }

    /// What it takes up when painted, in CELLS: the text plus the mark, if
    /// it carries one. This is what `modal_width` measures, and that is why
    /// it lives next to the model — a width that does not count the mark
    /// leaves the box short.
    pub(crate) fn width(&self) -> usize {
        self.text.as_str().width() + if self.hostile { badge_width() } else { 0 }
    }
}

/// What the altered-name mark takes up, with its space.
fn badge_width() -> usize {
    HOSTILE_BADGE.width() + 1
}

/// A modal's body: its lines, in order.
pub(crate) type ModalBody = Vec<ModalLine>;

/// A body composed as a string is split by lines and comes out plain.
///
/// This is what lets the 55 variants that still compose a `String` stay
/// untouched: declaring roles is a change PER MODAL, not a requirement to
/// compile.
pub(crate) fn plain_body(cuerpo: &str) -> ModalBody {
    cuerpo.lines().map(ModalLine::plain).collect()
}

/// The style each role is painted with.
///
/// Lives here and not in each modal for the usual reason: two places
/// deciding what color a hint gets end up with two hints in different
/// colors.
fn line_style(kind: LineKind, theme: &TuiTheme) -> ratatui::style::Style {
    use ratatui::style::Style;
    match kind {
        LineKind::Plain => Style::default(),
        // `Info`, NOT `BorderUnfocused`. The border role looked natural
        // — "it's there, it's legible, and it doesn't claim the view" — but
        // it is a role meant for CHROME: light themes make it very pale and
        // it also carries `dim`. Measured against its own theme's
        // background it gives 2.30:1 in `catppuccin-latte` and 2.45:1 in
        // `gruvbox-light`, when WCAG AA asks for 4.5 for text. `Info` gives
        // 4.34 and 5.82 on those same two. Buttons are painted in spans
        // with `Role::Button`; the LINE style is a hint's, for whatever
        // falls outside a button.
        LineKind::Dim | LineKind::Buttons => theme.role(Role::Info),
        LineKind::Strong => theme.role(Role::Title),
        // The background of a selected row: that is exactly what a field
        // is — what you have "grabbed" — and it already means that in the
        // listing.
        LineKind::Field => theme.role(Role::Selection),
        LineKind::Warning => theme.role(Role::Warning),
        LineKind::Error => theme.role(Role::Error),
    }
}

/// Terminal CELLS (`UnicodeWidthStr::width`, same language as
/// [`draw_nav_popup`]/[`middle_ellipsis`]), not `chars` — a body with CJK
/// (two cells per char, e.g. a path with `日本語`) overflowed the box with
/// the old char count.
pub(crate) fn modal_width(titulo: &str, cuerpo: &ModalBody, frame_width: u16) -> u16 {
    let content_max = cuerpo
        .iter()
        .map(ModalLine::width)
        .chain(std::iter::once(titulo.width() + 2))
        .max()
        .unwrap_or(0);
    u16::try_from(content_max + 4)
        .unwrap_or(u16::MAX)
        .clamp(60, frame_width.saturating_sub(4).max(60))
}

/// If `modal` tints the border as a warning (role `warning`): a PERMANENT
/// delete or a security decision (approving an agent op, trusting a host
/// key or a project `init.lua`). Factored out of `draw_modal` (clippy
/// `too_many_lines`).
pub(crate) fn is_warning_modal(modal: &crate::app::Modal) -> bool {
    use crate::app::Modal;
    matches!(
        modal,
        Modal::ConfirmDelete {
            permanent: true,
            ..
        } | Modal::ConfirmPluginUninstall { .. }
            // Undo reverts work already done: it is painted with the care
            // of a permanent delete, not with that of just any "are you
            // sure?".
            | Modal::ConfirmUndoAfter { .. }
            | Modal::ApproveAgentOp { .. }
            | Modal::TrustHostKey { .. }
            | Modal::TrustLuaInit { .. }
    )
}

/// Centered box of the modal.
/// `reinterpret` = the encoding of the pane with FOCUS when painting:
/// correct for SYNCHRONOUS modals (confirming copy/move/delete is created
/// from the focused pane, and an open modal freezes focus — creation ≡
/// draw). ASYNC ones (collision) carry their own encoding captured at
/// launch (`RetrySpec`, #98/M1). Agent paths (`ApproveAgentOp`) are NEVER
/// reinterpreted: another trust boundary (they go through the raw
/// `display_name` on purpose). `hints` (H1 T3, #24) carries each modal's
/// GENERATED footers — one per field, already resolved from the effective
/// current `dialog`.
/// Title+body of the active modal, extracted out of `draw_modal` (clippy
/// `too_many_lines` as the modal family grew).
///
/// And with S4 (#135) it goes over the limit again, this time with nowhere
/// left to extract to: what remains is a modal→text TABLE, one arm per
/// variant and exhaustive on purpose (a new modal does not compile until
/// someone decides how it is painted). Splitting it into two halves would
/// only move the boundary to an arbitrary point and make it harder to see
/// that none is missing. Same criterion, and same exception, as `main.rs`'s
/// dispatch table.
/// Title and body of each modal, ALREADY WITH ROLES.
///
/// Two doors on purpose. A modal that wants hierarchy — dimmed labels, a
/// field that looks like a field, the destination highlighted — is handled
/// up here and composes its [`ModalLine`]s. Everything else comes out of
/// the usual text table and is converted to plain lines, exactly as it used
/// to be painted.
///
/// This way declaring roles is a change PER MODAL. The alternative —
/// touching all 26 variants at once — was a diff of thousands of lines for
/// an improvement that shows in five.
pub(crate) fn modal_title_body(
    modal: &crate::app::Modal,
    reinterpret: Option<norte_encoding::NameEncoding>,
    hints: &crate::hints::DialogHints,
) -> (String, ModalBody) {
    let (title, mut body) = modal_title_body_raw(modal, reinterpret, hints);
    // `[ui] dialog_buttons` (spec 2026-09-10): the row of keys becomes
    // buttons. It is the LAST line of the body by construction in every
    // modal — the one generated by `dialog_hints` and the ones written in
    // Fluent (`[enter] confirm · [esc] cancel`) — and only if it parses
    // whole as `[key] verb`. Last AND shaped: a file name `[y] delete` in
    // the middle of the list cannot disguise itself as a button, and a body
    // whose last line is a field or prose stays as it is. With help
    // covering the modal (`modals_inert`) there are no buttons: their keys
    // do nothing, and a button that does nothing is the lie `hints` exists
    // not to tell. Works for the two bodies with roles and for the 55
    // variants that compose `String`, without touching any of them.
    // Free-text modals push their ERROR below the row of keys, so it is
    // searched for from the end, skipping warnings and errors (m8 review):
    // a visible error must not turn off the buttons.
    if hints.buttons && !hints.modals_inert {
        let keys = body
            .iter_mut()
            .rev()
            .find(|l| !matches!(l.kind, LineKind::Error | LineKind::Warning));
        if let Some(line) = keys
            && line.kind != LineKind::Field
            && crate::hints::hint_buttons(&line.text).is_some()
        {
            line.kind = LineKind::Buttons;
        }
    }
    (title, body)
}

/// A button on the row of keys, already placed: where it starts (cell
/// relative to the box's interior), what is painted and what chord it
/// synthesizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ButtonCell {
    /// Interior cell where it starts.
    pub x0: usize,
    /// The button's text, with its padding: ` Enter Confirm `.
    pub text: String,
    /// The chord painted (`Enter`, `F5`…), the one to synthesize.
    pub chord: String,
}

/// The buttons of a row of keys within `interior` cells, separated by a
/// space, or `None` if the line is not a row of keys or they do not all
/// fit: half a button is not a button, and then the line is painted as a
/// hint.
#[must_use]
pub(crate) fn button_cells(text: &str, interior: usize) -> Option<Vec<ButtonCell>> {
    let mut x = 0;
    let mut out = Vec::new();
    for b in crate::hints::hint_buttons(text)? {
        let painted = format!(" {} {} ", b.chord, b.label);
        let w = UnicodeWidthStr::width(painted.as_str());
        if x + w > interior {
            return None;
        }
        out.push(ButtonCell {
            x0: x,
            text: painted,
            chord: b.chord,
        });
        x += w + 1;
    }
    Some(out)
}

/// A modal's box, measured once for whoever paints and whoever resolves a
/// click: the same body, the same width, the same place.
pub(crate) struct ModalFrame {
    /// The title.
    pub title: String,
    /// The body with roles.
    pub body: ModalBody,
    /// Where the box falls, borders included.
    pub area: Rect,
    /// Only `TrustLuaInit` wraps its body; the rest come by lines.
    pub wraps: bool,
    /// The interior: the width minus the two borders.
    pub interior: usize,
}

/// Measures the modal the way [`draw_modal`] paints it.
#[must_use]
pub(crate) fn modal_frame(
    modal: &crate::app::Modal,
    reinterpret: Option<norte_encoding::NameEncoding>,
    hints: &crate::hints::DialogHints,
    frame_area: Rect,
) -> ModalFrame {
    let (title, body) = modal_title_body(modal, reinterpret, hints);
    let wraps = matches!(modal, crate::app::Modal::TrustLuaInit { .. });
    let width = modal_width(&title, &body, frame_area.width);
    let interior = usize::from(width.saturating_sub(2));
    let height = if wraps {
        WRAPPED_HEIGHT
    } else {
        body_height(&body)
    };
    ModalFrame {
        title,
        body,
        area: centered(frame_area, width, height),
        wraps,
        interior,
    }
}

/// A clickable button of a modal (spec 2026-09-10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModalZone {
    /// Row.
    pub row: u16,
    /// First column, inclusive.
    pub x0: u16,
    /// Last column, inclusive.
    pub x1: u16,
    /// The chord painted that the button synthesizes (`Enter`, `Esc`, `F5`).
    pub chord: String,
}

/// The zones of the active modal's buttons, from the SAME calculation as
/// the painting. Empty with no modal, with the wrapped body (its rows are
/// not the body's) or with the line painted as a hint for not fitting.
#[must_use]
pub fn modal_zones(app: &App, area: Rect) -> Vec<ModalZone> {
    let Some(modal) = &app.modal else {
        return Vec::new();
    };
    let inert = app
        .help
        .as_ref()
        .is_some_and(|help| help.over_modal)
        .then(|| app.dialog_hints.with_modals_inert());
    let hints = inert.as_ref().unwrap_or(&app.dialog_hints);
    let f = modal_frame(modal, app.focused().name_encoding(), hints, area);
    if f.wraps {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, line) in f.body.iter().enumerate() {
        if line.kind != LineKind::Buttons {
            continue;
        }
        let Some(cells) = button_cells(&line.text, f.interior) else {
            continue;
        };
        let row = f
            .area
            .y
            .saturating_add(1)
            .saturating_add(u16::try_from(i).unwrap_or(u16::MAX));
        for c in cells {
            let w = UnicodeWidthStr::width(c.text.as_str());
            let x0 = f
                .area
                .x
                .saturating_add(1)
                .saturating_add(u16::try_from(c.x0).unwrap_or(u16::MAX));
            out.push(ModalZone {
                row,
                x0,
                x1: x0
                    .saturating_add(u16::try_from(w).unwrap_or(u16::MAX))
                    .saturating_sub(1),
                chord: c.chord,
            });
        }
    }
    out
}

fn modal_title_body_raw(
    modal: &crate::app::Modal,
    reinterpret: Option<norte_encoding::NameEncoding>,
    hints: &crate::hints::DialogHints,
) -> (String, ModalBody) {
    use crate::app::Modal;
    if let Modal::ConfirmTransfer {
        kind,
        items,
        to,
        space,
        confine,
    } = modal
    {
        return confirm_transfer_modal(
            *kind,
            items,
            to,
            reinterpret,
            DestNotices {
                space: space.as_deref(),
                confine: confine.as_deref(),
            },
            &hints.confirm,
        );
    }
    if let Modal::TransferName {
        kind,
        from,
        to_dir,
        name,
        enc,
        error,
        space,
        confine,
        ..
    } = modal
    {
        return transfer_name_modal(
            *kind,
            from,
            to_dir,
            name,
            error.as_deref(),
            // The reinterpretation CAPTURED at opening, never the pane's at
            // paint time (#98/M1): it is the same one the text table
            // already used.
            *enc,
            DestNotices {
                space: space.as_deref(),
                confine: confine.as_deref(),
            },
        );
    }
    // Phase 8: the organize tree needs roles — a NEW folder is not just
    // another line of the list — and that is why it comes in through this
    // door and not through the text table.
    if let Modal::OrganizePlan {
        dir, lines, offset, ..
    } = modal
    {
        return organize_plan_modal(dir, lines, *offset, hints);
    }
    let (titulo, cuerpo) = modal_title_text(modal, reinterpret, hints);
    (titulo, plain_body(&cuerpo))
}

#[expect(clippy::too_many_lines, reason = "modal→text table, not logic")]
fn modal_title_text(
    modal: &crate::app::Modal,
    reinterpret: Option<norte_encoding::NameEncoding>,
    hints: &crate::hints::DialogHints,
) -> (String, String) {
    use crate::app::Modal;
    match modal {
        // #103 T10: the batch goes as a LIST — one path per line, sanitized
        // and truncated by the policy SHARED with the GUI
        // (`norte_frontend::item_lines_with`), never two paths on the same
        // line (a hostile name would fabricate a list entry). The
        // capabilities go ONE PER LINE, each with its flag if its text
        // differs from the real one: they are third-party text, and a list
        // glued into a sentence lets one pretend to be another (#280).
        Modal::ConfirmPluginApproval {
            name,
            name_hostile,
            caps,
            ..
        } => (
            t("modal-plugin-approval-title"),
            [
                vec![format!(
                    "{name}{}",
                    if *name_hostile { HOSTILE_BADGE } else { "" }
                )],
                caps.iter()
                    .map(|(texto, hostil)| {
                        format!("  · {texto}{}", if *hostil { HOSTILE_BADGE } else { "" })
                    })
                    .collect(),
                vec![t("modal-plugin-approval-note"), hints.approval.clone()],
            ]
            .concat()
            .join("\n"),
        ),
        // Uninstall (ADR 0104): the name apart from the sentence, with its
        // flag, and the note that says the TWO things being lost.
        // And the id, which is the ONLY thing the core validates: two
        // extensions can share a name, and the name is written by the
        // manifest.
        Modal::ConfirmPluginUninstall {
            id,
            name,
            name_hostile,
        } => (
            t("modal-extension-uninstall-title"),
            [
                format!("{name}{}", if *name_hostile { HOSTILE_BADGE } else { "" }),
                format!("  {id}"),
                t("modal-extension-uninstall-note"),
                hints.uninstall.clone(),
            ]
            .join("\n"),
        ),
        // Undo to a point (phase 7): the body is the COUNT, and the three
        // numbers go on different lines because they mean different things
        // and do not add up. What is going to be skipped and what is not
        // the reader's are only said if there are any: a line saying "0 are
        // not yours" is noise that pushes down what actually matters.
        Modal::ConfirmUndoAfter {
            a_deshacer,
            irreversibles,
            ajenas,
            ..
        } => {
            let mut lineas = vec![
                t("timeline-undo-body"),
                ta("timeline-undo-count", &[("n", &a_deshacer.to_string())]),
            ];
            if *irreversibles > 0 {
                lineas.push(ta(
                    "timeline-undo-skipped",
                    &[("n", &irreversibles.to_string())],
                ));
            }
            if *ajenas > 0 {
                lineas.push(ta("timeline-undo-foreign", &[("n", &ajenas.to_string())]));
            }
            lineas.push(hints.uninstall.clone());
            (t("timeline-undo-title"), lineas.join("\n"))
        }
        Modal::ConfirmDelete { items, permanent } => (
            if *permanent {
                t("modal-delete-permanent-title")
            } else {
                t("modal-trash-title")
            },
            [
                norte_frontend::item_lines_with(items, HOSTILE_BADGE, reinterpret),
                vec![
                    if *permanent {
                        t("modal-delete-permanent-warning")
                    } else {
                        t("modal-trash-note")
                    },
                    hints.confirm.clone(),
                ],
            ]
            .concat()
            .join("\n"),
        ),
        // #98/M1: the collision arrives ASYNC — uses the encoding captured
        // at LAUNCH of the operation (RetrySpec), never the focused pane's
        // on arrival.
        Modal::Collision { retry } => (
            t("modal-collision-title"),
            format!(
                "{}
{}
{}",
                t("modal-collision-body"),
                norte_frontend::path_display_with(&retry.to, retry.name_encoding).0,
                hints.collision
            ),
        ),
        Modal::ApproveAgentOp { req } => approval_modal_text(req, &hints.approval),
        Modal::TrustHostKey {
            host,
            port,
            algo,
            fingerprint,
            ..
        } => trust_host_modal_text(host, *port, algo, fingerprint, &hints.trust_host),
        // #325: the connection name comes from `connections.toml` — the
        // user wrote it themselves, not a server — but it is sanitized all
        // the same: a connections file can come from someone else's
        // dotfile.
        Modal::AskSecret {
            conn,
            endpoint,
            input,
            ..
        } => ask_secret_modal_text(conn, endpoint, input, &hints.ask_secret),
        // Lua TOFU (M4): `path` arrives ALREADY sanitized by the modal's
        // constructor (`detail_for_bar`); the body is a single long message
        // and this modal's Paragraph carries wrap (below).
        Modal::TrustLuaInit { path, hash_abbrev } => (
            t("modal-lua-trust-title"),
            ta(
                "modal-lua-trust-body",
                &[("path", path.as_str()), ("hash", hash_abbrev.as_str())],
            ),
        ),
        // S2 (`[ui] confirm_quit`): no data of its own — a fixed
        // title+body plus the hint (`hints.confirm`, ALLOW_CONFIRM reused).
        // #139: the properties come from the LISTING — nothing to ask for —
        // except a folder's count, which is the only thing a listing does
        // not know.
        Modal::Properties {
            entry,
            size,
            size_task,
        } => properties_modal_text(entry, *size, size_task.is_some()),
        // The footer is confirm's (Enter/Esc close, `dialog_action`):
        // without it nothing said how to get out, and without its row of
        // buttons the mouse had nowhere to click.
        Modal::Report { kind, lines } => (
            t(kind.title_key()),
            format!("{}\n{}", report_text(lines), hints.confirm),
        ),
        Modal::ConfirmQuit => (
            t("modal-confirm-quit-title"),
            format!("{}\n{}", t("modal-confirm-quit-body"), hints.confirm),
        ),
        // #311: one line per file, with its checksum and — when verifying —
        // its verdict. The name goes through the usual sanitizing: a
        // checksums file names files, and a name can carry bidi inside.
        Modal::Checksums {
            title_key,
            rows,
            offset,
        } => checksums_modal_text(title_key, rows, *offset),
        // #103 T9: see `mark_pattern_modal_text` (masked, not a fixed text
        // — the pattern/error are user text).
        Modal::MarkPattern {
            mark,
            pattern,
            error,
        } => mark_pattern_modal_text(*mark, pattern, error.as_deref()),
        // #104: same masking as the pattern — name and error are user text
        // (paste with bidi/invisibles included).
        Modal::Mkdir { name, error } => {
            free_text_modal_text("modal-mkdir", "modal-mkdir-hint", name, error.as_deref())
        }
        // #306: the same mold for a PROFILE's name. The footer says that
        // what is saved is what is shown, which is the question whoever
        // opens it has.
        Modal::ProfileSaveAs { name, error } => free_text_modal_text(
            "modal-profile-save-as",
            "modal-profile-save-as-hint",
            name,
            error.as_deref(),
        ),
        // #290: the same mold with the other kind of node. The name is
        // asked for because the daemon creates it, not the editor.
        Modal::EditNew { name, error, .. } => free_text_modal_text(
            "modal-new-file",
            "modal-new-file-hint",
            name,
            error.as_deref(),
        ),
        // #132: same masking and same mold. The pack modal's footer says
        // what format comes out of the TYPED name, not the suggested one:
        // it is the only way for the user to see the decision before
        // confirming it.
        Modal::Pack { name, error } => free_text_modal_text(
            "modal-pack",
            match crate::app::format_by_name(name.as_bytes()) {
                Some(norte_proto::methods::ArchiveFormat::Zip) => "modal-pack-hint-zip",
                Some(norte_proto::methods::ArchiveFormat::Tar) => "modal-pack-hint-tar",
                Some(norte_proto::methods::ArchiveFormat::TarGz) => "modal-pack-hint-targz",
                None => "modal-pack-hint-unknown",
            },
            name,
            error.as_deref(),
        ),
        Modal::Split { size, error } => {
            free_text_modal_text("modal-split", "modal-split-hint", size, error.as_deref())
        }
        // #314: the mode in octal, with HOW MANY entries it is going to
        // change in the title. The number matters: typing a mode with fifty
        // files marked while believing it applies to one is the error this
        // dialog has to make hard.
        Modal::Chmod {
            mode,
            targets,
            error,
        } => {
            let (_, cuerpo) =
                free_text_modal_text("modal-chmod", "modal-chmod-hint", mode, error.as_deref());
            // The singular has its own id: i18n args are strings, and a
            // plural selector over a string never picks anything.
            let titulo = if targets.len() == 1 {
                t("modal-chmod-one")
            } else {
                norte_i18n::ta("modal-chmod", &[("n", &targets.len().to_string())])
            };
            (titulo, cuerpo)
        }
        // Same masking: the typed address and its diagnostic are user text,
        // and an address arrives by paste as easily as a name.
        Modal::TransferDest { kind, input, error } => free_text_modal_text(
            match kind {
                crate::app::TransferKind::Copy => "modal-transfer-dest-copy",
                crate::app::TransferKind::Move => "modal-transfer-dest-move",
            },
            "modal-transfer-dest-hint",
            input,
            error.as_deref(),
        ),
        // M4-IA: same masking as mkdir — instruction and error are user
        // text (paste with bidi/invisibles included).
        // #135: same masking as the AI instruction — the command line and
        // its diagnostic are user text.
        Modal::CommandLine { command, error } => free_text_modal_text(
            "modal-command-line",
            "modal-command-line-hint",
            command,
            error.as_deref(),
        ),
        Modal::AiRenameInstruction { instruction, error } => free_text_modal_text(
            "modal-ai-rename",
            "modal-ai-rename-hint",
            instruction,
            error.as_deref(),
        ),
        // #310: the batch's template. Same free-text mold and the same
        // masking: what is typed can arrive by paste with bidi inside.
        Modal::RenameBatchPattern { pattern, error } => free_text_modal_text(
            "modal-rename-batch",
            "modal-rename-batch-hint",
            pattern,
            error.as_deref(),
        ),
        // M4-IA: target dir + window of from→to pairs of the reviewable
        // plan (defensive masking, see `ai_rename_plan_modal_text`).
        Modal::AiRenamePlan {
            dir,
            entries,
            offset,
            plan,
            // Whether it has been seen does NOT change what is painted: it
            // gates confirming (`dialog_action`) and the footer says so.
            seen: _,
        } => ai_rename_plan_modal_text(dir, entries, *offset, hints, plan),
        // M4-IA-2: same masking as the AI instruction — query and error are
        // user text.
        Modal::SemanticQuery { query, error } => free_text_modal_text(
            "modal-semantic",
            "modal-semantic-hint",
            query,
            error.as_deref(),
        ),
        // M4-IA-2: window of hits with a cursor (defensive masking, see
        // `semantic_hits_modal_text`).
        Modal::SemanticHits {
            hits,
            offset,
            cursor,
        } => semantic_hits_modal_text(hits, *offset, *cursor, hints),
        // The ALREADY MIGRATED ones: `modal_title_body` handles them, which
        // composes its lines WITH ROLES. The arm stays because the `match`
        // is exhaustive on purpose — a new modal does not compile until
        // someone decides how it is painted — and that net is not lost by
        // migrating one.
        //
        // The empty body is unreachable through the one caller, which
        // diverts these variants earlier. An `unreachable!` would be a
        // panic in release (rule 6); the `debug_assert` turns it red in
        // tests if someone adds a second caller and skips the diversion.
        Modal::TransferName { .. } | Modal::ConfirmTransfer { .. } | Modal::OrganizePlan { .. } => {
            debug_assert!(
                false,
                "modal with roles requested from the text table: it is \
                 handled in `modal_title_body`"
            );
            (String::new(), String::new())
        }
    }
}

/// The height that needs to be reserved for an ALREADY-COMPOSED body.
///
/// **Replaces `modal_height`, which was a table of formulas written by
/// hand, one per variant.** This module's rustdoc had been warning since the
/// start that the two halves drift apart and the modal gets clipped, and
/// there was not a single check; when one was finally written — for one
/// variant — it turned out the formulas did not even agree among
/// themselves: some added 2 to the line count, others 3, others 4, and
/// `TrustHostKey` declared 9 fixed for "five lines".
///
/// Deriving it from the body, drifting apart stops being POSSIBLE. No test
/// needs to watch for it: there are no two numbers that can disagree.
///
/// `+3` is the two borders and one row of air below — the majority
/// convention (`body_lines + 3`) and the one that reads best. The variants
/// that declared `+2` gain that row; the ones that declared `+4` lose it.
///
/// **Does not work for the modal that WRAPS.** There, one body line takes up
/// several rows and `cuerpo.len()` does not count them. The obvious
/// division — `width / interior` rounding up — falls short: `ratatui` wraps
/// by words, so a long word cuts the row short before filling it. Asking it
/// directly would be the right thing (`Paragraph::line_count`) but it is an
/// UNSTABLE ratatui feature, and turning it on for one modal is not worth
/// it. That case declares its height by hand, and a test checks that its
/// message fits (`el_mensaje_que_se_envuelve_cabe_en_su_caja`).
fn body_height(cuerpo: &ModalBody) -> u16 {
    u16::try_from(cuerpo.len())
        .unwrap_or(u16::MAX)
        .saturating_add(3)
}

/// The height of the one modal whose body WRAPS.
///
/// By hand and not derived, because its body is one line that ratatui
/// splits into several, and counting those rows requires its wrapping rule.
/// Four rows of message at ~74 columns, one of air and the two borders.
const WRAPPED_HEIGHT: u16 = 7;

/// Paints the active modal: border (as a warning on hard decision
/// surfaces), title and body from `modal_title_body`. `reinterpret` is the
/// focused pane's reinterpretation AT PAINT TIME — modals that capture their
/// own when opened (`Collision` #98/M1, `TransferName` #105) ignore it in
/// favor of the captured one.
pub(crate) fn draw_modal(
    frame: &mut Frame<'_>,
    modal: &crate::app::Modal,
    theme: &TuiTheme,
    reinterpret: Option<norte_encoding::NameEncoding>,
    hints: &crate::hints::DialogHints,
) {
    // A PERMANENT delete (or approving an agent mutation) tints the border
    // as a warning (role `warning`).
    let border = if is_warning_modal(modal) {
        theme.role(Role::Warning)
    } else {
        theme.role(Role::ModalBorder)
    };
    // The box is MEASURED in `modal_frame`, which is also what the mouse
    // reads: the height is derived from the body and the width from the
    // title and the lines, so there are no two numbers that can disagree.
    let ModalFrame {
        title,
        body,
        area,
        wraps,
        interior,
    } = modal_frame(modal, reinterpret, hints, frame.area());
    clear_themed(frame, area, theme);
    let lineas: Vec<ratatui::text::Line<'_>> = body
        .iter()
        .map(|l| {
            // The buttons (spec 2026-09-10): one span per button with the
            // `button` role, separated by a space; if they do not fit, the
            // usual hint.
            if l.kind == LineKind::Buttons
                && let Some(cells) = button_cells(&l.text, interior)
            {
                let mut spans = Vec::new();
                let mut x = 0;
                for c in cells {
                    if c.x0 > x {
                        spans.push(ratatui::text::Span::raw(" ".repeat(c.x0 - x)));
                    }
                    x = c.x0 + UnicodeWidthStr::width(c.text.as_str());
                    spans.push(ratatui::text::Span::styled(
                        c.text,
                        theme.role(Role::Button),
                    ));
                }
                return ratatui::text::Line::from(spans);
            }
            // A field is padded to the border. With the background ending
            // where the text ends it looks like HIGHLIGHTED text, not a
            // place to write — and on top of that you cannot see how much
            // it holds. This is a painting decision, and that is why it
            // lives here: whoever composes the body does not yet know how
            // wide the box will come out.
            let texto = if l.kind == LineKind::Field {
                let hueco = interior.saturating_sub(l.width());
                format!("{}{}", l.text, " ".repeat(hueco))
            } else {
                l.text.clone()
            };
            let estilo = line_style(l.kind, theme);
            // The altered-name mark, in its own span and with ITS role: it
            // is the only signal in this box whose contrast is guaranteed
            // (spec §6), and inheriting a dimmed line's style left it
            // illegible right where it matters most.
            if l.hostile {
                ratatui::text::Line::from(vec![
                    ratatui::text::Span::styled(
                        format!("{HOSTILE_BADGE} "),
                        theme.role(Role::HostileBadge),
                    ),
                    ratatui::text::Span::styled(texto, estilo),
                ])
            } else {
                ratatui::text::Line::styled(texto, estilo)
            }
        })
        .collect();
    let mut body = Paragraph::new(lineas).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_style(theme.role(Role::Title))
            .border_style(border),
    );
    // The wrap, with the SAME condition that already measured the height:
    // the rest of the modals come chopped into lines, and there the wrap
    // could split a path at any char (which the path modals avoid with
    // ellipsis).
    if wraps {
        body = body.wrap(ratatui::widgets::Wrap { trim: false });
    }
    frame.render_widget(body, area);
}

/// Session bytes for display; `None` = `?`. No collision with a literal `"?"`
/// session: the daemon validates the `[A-Za-z0-9._-]` charset at the
/// handshake, so `?` is not a reachable id.
pub(crate) fn session_bytes(req: &norte_proto::methods::PolicyApprovalRequired) -> &[u8] {
    req.session.as_deref().map_or(b"?", str::as_bytes)
}

/// Clips to `max` CHARS (not bytes) with a trailing `…`. For already-masked
/// strings that could still be huge (layout clamp, H1).
/// `(title, body)` text of the agent-approval modal (M3-3b T5).
/// TODO everything interpolated is controlled by the AGENT
/// (encoding-auditor H1/H2/H3) and this is a human security decision:
/// session and paths go through the SAME masking as pane names
/// (controls/bidi/invisibles → �) PLUS a clamp; each path goes on ITS OWN
/// line with an out-of-band label (never an in-band joiner a name could
/// imitate) and middle ellipsis (a mile-long `from` does not push the
/// destination out of the box); the masking is MARKED with the badge (spec
/// §6).
///
/// The list is WINDOWED to [`norte_frontend::MODAL_ITEM_LIMIT`] paths plus a
/// summary line, like `ConfirmDelete`/`ConfirmTransfer` (review H3c
/// MINOR-5). How many paths the request carries is the AGENT's choice, and
/// without a cap the height grew with them: `centered` clips against the
/// frame, so the extra lines were not painted — including the LAST one,
/// which under H3c is the only explanation for why the modal's keys do not
/// respond. The summary carries a badge if any HIDDEN path is hostile (same
/// doctrine as the AI plan and the semantic hits: what is hidden never
/// slips through "clean").
pub(crate) fn approval_modal_text(
    req: &norte_proto::methods::PolicyApprovalRequired,
    hint: &str,
) -> (String, String) {
    let session = clamp_chars(&display_name(session_bytes(req)).0, 40);
    let op = clamp_chars(&display_name(req.op.as_bytes()).0, 16);
    let mut lines = vec![ta(
        "modal-approval-body",
        &[("session", &session), ("op", &op)],
    )];
    // How much time is left. A decision with an expiry date that does not
    // show it reads as one that waits forever, and whoever comes back later
    // presses approve on something the daemon has already denied. The
    // window said so and this terminal did not, with the same fields in
    // front of it.
    //
    // `ttl_ms == 0` is UNKNOWN — a pending item rebuilt by the
    // `policy.pending` resync does not carry the remaining deadline — and it
    // is said, instead of staying silent: staying silent leaves the dialog
    // there inviting approval on an id the daemon may have already reaped a
    // while ago.
    lines.push(if req.ttl_ms > 0 {
        ta(
            "modal-approval-ttl",
            &[("s", &req.ttl_ms.div_ceil(1000).to_string())],
        )
    } else {
        t("modal-approval-ttl-unknown")
    });
    // #314: what the op ADDS to the question. For all but one there is
    // nothing: the op and the paths are the decision. A `set-mode` does add
    // something, because two ops with the same paths and different modes
    // mean opposite things, and without this line the human did not know
    // whether they were saying yes to `0600` or to `4777`.
    if let Some(mode) = req.detail.mode {
        lines.push(ta(
            "modal-approval-mode",
            &[("mode", &norte_frontend::chmod::format_mode(mode))],
        ));
    }
    // #315: and the SCOPE, which the human also could not see without this
    // line. A recursive op on a root was asked as "1 path", and what got
    // approved was all of its descendants — the same hole the mode line
    // came to close, one size bigger.
    if req.detail.recursive {
        lines.push(match req.detail.dir_mode {
            Some(dir) => ta(
                "modal-approval-recursive-dirs",
                &[("mode", &norte_frontend::chmod::format_mode(dir))],
            ),
            None => t("modal-approval-recursive"),
        });
    }
    let limit = norte_frontend::MODAL_ITEM_LIMIT;
    for (i, p) in req.paths.iter().take(limit).enumerate() {
        let (text, hostile) = display_name(p.as_bytes());
        lines.push(ta(
            "modal-approval-path",
            &[
                ("badge", if hostile { HOSTILE_BADGE } else { "" }),
                ("n", &(i + 1).to_string()),
                ("path", &middle_ellipsis(&text, 46)),
            ],
        ));
    }
    // How many the DECISION covers, not how many arrived: the server clips
    // the notification (a batch of renames gates thousands of paths), and
    // without `paths_total` the modal would show 32 innocent paths as if
    // they were all of them — which is approving blind while believing you
    // are approving what you can see. `0` = an N-1 server that did not send
    // it: then what was received IS everything there was.
    let total = usize::try_from(req.paths_total)
        .unwrap_or(usize::MAX)
        .max(req.paths.len());
    let shown = req.paths.len().min(limit);
    if total > shown {
        // The badge can only speak about what CAN be looked at: the paths
        // the server clipped are not here to inspect. What is not left
        // unsaid is the NUMBER, which is what decides consent.
        //
        // The SHARED crate answers the question: the window did not do it
        // over the same paths, and a security decision written in a single
        // frontend is half the product without it (ADR 0077).
        let hidden_hostile = norte_frontend::overflow_hostile_redacted(&req.paths, shown);
        // Key SHARED with `item_lines_with` (`ConfirmDelete`'s): the summary
        // says the same thing in both places, or the reader learns two
        // phrases for one single fact.
        lines.push(badge_prefixed(
            hidden_hostile,
            ta("gui-modal-more", &[("n", &(total - shown).to_string())]),
        ));
    }
    lines.push(hint.to_owned());
    (t("modal-approval-title"), lines.join("\n"))
}

/// Title+body of `Modal::MarkPattern` (#103 T9), factored out of
/// `draw_modal` (clippy `too_many_lines`). Free text, NOT a security decision
/// surface — follows the SAME discipline as the rest (masked with
/// `display_name`, never raw): a pattern arrives by paste as easily as
/// typed, and `PatternError` EMBEDS the pattern verbatim in its message
/// (`PatternError::Glob`'s rustdoc) — the masking reaches the error line
/// too.
/// The text of the properties dialog (#139).
///
/// The name and the attribute values are FILE data, so they go masked with
/// the same `display_name` as the listing: a name with bidi or invisibles
/// does not reorder this dialog.
/// The body of the checksums modal (#311): one line per file.
///
/// The name goes through [`display_name`] like any other that gets painted —
/// a checksums file is text from OUTSIDE and can name things with bidi
/// inside — and the digest is clipped to twelve characters: what fits in a
/// modal box is not a 64-char line, and whoever wants the whole hash copies
/// it with `Enter`.
pub(crate) fn checksums_modal_text(
    title_key: &str,
    rows: &[crate::app::ChecksumRow],
    offset: usize,
) -> (String, String) {
    /// What fits of a name in the box. The modal does not wrap, so without
    /// this `ratatui` clips on the right WITHOUT A MARK: two long names with
    /// the same start are painted identically, and the row you are reading
    /// to decide whether an ISO is the right one does not say which is
    /// which.
    const NAME_MAX: usize = 44;

    let mut lines = Vec::with_capacity(rows.len().min(AI_RENAME_PAIR_LIMIT) + 2);
    for row in rows.iter().skip(offset).take(AI_RENAME_PAIR_LIMIT) {
        let (name, hostile) = display_name(&row.name);
        let name = norte_frontend::middle_ellipsis(&name, NAME_MAX);
        let name = if hostile {
            format!("{HOSTILE_BADGE} {name}")
        } else {
            name
        };
        let estado = match (row.verdict, &row.digest) {
            (Some(v), _) => t(v.label_key()),
            (None, Some(d)) => d.chars().take(12).collect::<String>(),
            (None, None) => t("checksum-unreadable"),
        };
        lines.push(format!("{estado}  {name}"));
    }
    // What is left BELOW the window, which with `offset` is not the same
    // thing as "the ones that do not fit": scrolling down, this number has
    // to go down with it.
    let restantes = rows.len().saturating_sub(offset + AI_RENAME_PAIR_LIMIT);
    if restantes > 0 {
        lines.push(norte_i18n::ta(
            "modal-checksums-more",
            &[("n", &restantes.to_string())],
        ));
    }
    // Copy is only offered if there is something to copy: a verify pass
    // carries verdicts and no digest, and the `sha256sum -c` that would come
    // out of that would be an empty file. Promising the key anyway ended in
    // "nothing to copy", which is a dialog showing a key that does nothing.
    let copiable = rows.iter().any(|r| r.digest.is_some());
    lines.push(t(if copiable {
        "modal-checksums-hint"
    } else {
        "modal-checksums-hint-verify"
    }));
    (t(title_key), lines.join("\n"))
}

pub(crate) fn properties_modal_text(
    entry: &norte_proto::Entry,
    size: Option<(u64, u64)>,
    counting: bool,
) -> (String, String) {
    use norte_proto::EntryKind;

    let (name, hostile) = display_name(
        entry
            .path
            .file_name()
            .map_or(b"".as_slice(), norte_proto::Segment::as_bytes),
    );
    let title = if hostile {
        format!("{HOSTILE_BADGE} {name}")
    } else {
        name
    };
    let class = match entry.kind {
        EntryKind::Dir => t("props-kind-dir"),
        EntryKind::File => t("props-kind-file"),
        EntryKind::Symlink => t("props-kind-symlink"),
        EntryKind::Other => t("props-kind-other"),
    };
    let mut lines = vec![format!("{}: {}", t("props-kind"), class)];
    // The size of a FOLDER does not come from the listing: either it has
    // been counted, or it is being counted, or — if nobody asked for it —
    // it says it can be requested. Faking a zero would be the only clearly
    // false answer.
    let size_text = match (entry.kind, size, counting) {
        (_, Some((bytes, entries)), _) => format!(
            "{} ({})",
            norte_frontend::human_bytes(bytes),
            ta("props-entries", &[("count", &entries.to_string())])
        ),
        (EntryKind::Dir, None, true) => t("props-counting"),
        (EntryKind::Dir, None, false) => t("props-count-hint"),
        (_, None, _) => entry
            .size
            .map_or_else(|| t("props-size-unknown"), norte_frontend::human_bytes),
    };
    lines.push(format!("{}: {}", t("props-size"), size_text));
    lines.push(format!(
        "{}: {}",
        t("props-modified"),
        entry.mtime_ms.map_or_else(
            || t("props-mtime-unknown"),
            |ms| norte_frontend::columns::format_mtime(
                ms,
                norte_frontend::columns::TimeFormat::Iso,
                0
            )
        )
    ));
    let (path_text, path_hostile) = display_name(entry.path.to_wire().as_bytes());
    lines.push(format!(
        "{}: {}{}",
        t("props-path"),
        if path_hostile {
            format!("{HOSTILE_BADGE} ")
        } else {
            String::new()
        },
        path_text
    ));
    // The attributes the provider reported, as-is: they are painted by
    // whoever requested them, and this window does not ask for any extra
    // ones.
    for (id, value) in &entry.attrs {
        let (v, v_hostile) = attr_text(value);
        lines.push(format!(
            "{id}: {}{v}",
            if v_hostile {
                format!("{HOSTILE_BADGE} ")
            } else {
                String::new()
            }
        ));
    }
    lines.push(t("props-hint"));
    (title, lines.join("\n"))
}

/// The body of a report (batch or undo): one phrase or one path per line.
///
/// The path goes ALONE on its line and with the badge if it had to be
/// masked: it is the name the reader is going to search for (or type) by
/// hand, and stuck inside a phrase another one could impersonate it (#273).
pub(crate) fn report_text(lines: &[norte_frontend::ReportLine]) -> String {
    lines
        .iter()
        .map(|l| match l {
            norte_frontend::ReportLine::Phrase(texto) => texto.clone(),
            norte_frontend::ReportLine::Path(p) => {
                let (path_text, hostile) = norte_frontend::path_display(p);
                format!("  {}", badge_prefixed(hostile, path_text))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// An attribute's value, ready to paint, and whether it had to be masked.
///
/// The two THIRD-PARTY ones — text and bytes — go through `display_name`,
/// the same lossy-with-badge path as a file name: an `owner` with bidi does
/// not reorder this dialog, and the original bytes are not touched (rule
/// 1).
pub(crate) fn attr_text(v: &norte_proto::AttrValue) -> (String, bool) {
    use norte_proto::AttrValue;
    match v {
        AttrValue::Uint(n) => (n.to_string(), false),
        AttrValue::Int(i) => (i.to_string(), false),
        AttrValue::TimeMs(ms) => (
            norte_frontend::columns::format_mtime(*ms, norte_frontend::columns::TimeFormat::Iso, 0),
            false,
        ),
        AttrValue::Bool(b) => (t(if *b { "col-cell-yes" } else { "col-cell-no" }), false),
        AttrValue::Text(s) => display_name(s.as_bytes()),
        AttrValue::Bytes(b) => display_name(b),
        // Present-but-unpaintable: a visible "?". Blank stays reserved for
        // ABSENT, as in the listing's cells.
        AttrValue::Unknown => ("?".to_owned(), false),
    }
}

pub(crate) fn mark_pattern_modal_text(
    mark: bool,
    pattern: &str,
    error: Option<&str>,
) -> (String, String) {
    let (masked, hostile) = display_name(pattern.as_bytes());
    // #103 T9 review MINOR: `PaneState::mark_glob` compiles the RAW pattern,
    // not the masked one — here the display genuinely differs from what
    // decides the match, so a hostile pattern carries the same badge as a
    // hostile file name (same language as `draw_search_dialog`'s root
    // line).
    let field = if hostile {
        format!("{HOSTILE_BADGE} {masked}_")
    } else {
        format!("{masked}_")
    };
    // #103 T9 review MINOR: this modal does not go through `DialogHints`
    // (free text, no ALLOWLIST to generate a footer from) — like
    // `search-hint`/`palette-hint`, its keys are fixed in Fluent.
    let mut lines = vec![
        field,
        t("modal-mark-pattern-hint"),
        t("modal-mark-pattern-keys"),
    ];
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(masked_err);
    }
    let title = if mark {
        t("modal-mark-pattern-add")
    } else {
        t("modal-mark-pattern-remove")
    };
    (title, lines.join("\n"))
}

/// Title+body of ANY single-line free-text prompt: masked field + hint +
/// the shared row of keys + the diagnostic if there is one.
///
/// The three prompts that existed (`Mkdir` #104, `AiRenameInstruction`
/// M4-IA, `SemanticQuery` M4-IA-2) were already THE SAME function with
/// different ids, and S4 (#135) brought a fourth: four copies are four
/// places to forget the masking, which is the only thing that matters here
/// (the field and the diagnostic are USER text — a paste with
/// bidi/invisibles reaches a query as easily as a name, and the engine's
/// error can embed the name). The row of keys is shared on purpose (FIX-A
/// from the T4 review): this way all four line up with the combined height
/// arm (7 with an error / 6 without) instead of one of them painting one
/// line short.
pub(crate) fn free_text_modal_text(
    title_id: &str,
    hint_id: &str,
    value: &str,
    error: Option<&str>,
) -> (String, String) {
    let (masked, hostile) = display_name(value.as_bytes());
    // Window anchored to the RIGHT (S4 review, M4): the modal's body is a
    // `Paragraph` with no wrap and a bounded width, so a long value painted
    // only its head and left the `_` cursor off screen — with a command
    // line that means pressing Enter without seeing what runs. It is
    // clipped from the front, marking the cut, which is what any single-line
    // editor does.
    let visible = tail_window(&masked, FREE_TEXT_FIELD_MAX);
    let field = if hostile {
        format!("{HOSTILE_BADGE} {visible}_")
    } else {
        format!("{visible}_")
    };
    let mut lines = vec![field, t(hint_id), t("modal-mark-pattern-keys")];
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(masked_err);
    }
    (t(title_id), lines.join("\n"))
}

/// Visible chars of a free-text prompt's field. Same budget as
/// [`MODAL_PATH_CHARS`] (the modal box measures 60 and the borders take
/// four columns), with slack for the badge and the cut mark.
pub(crate) const FREE_TEXT_FIELD_MAX: usize = 50;

/// Title+body of `Modal::AiRenamePlan` (M4-IA, encoding-auditor doctrine):
/// first line = the TARGET dir labeled out of band (audit MAJOR-1 — the
/// human decides knowing WHERE the plan lands); then the WINDOW of
/// [`AI_RENAME_PAIR_LIMIT`] pairs from `offset` (audit MAJOR-3: the whole
/// plan is reviewable by scrolling). Each name on ITS OWN line — the `from`
/// with an ABSOLUTE numbered label out of band (audit MINOR-4, corpus
/// `arrow_join_spoof`: a name can imitate the arrow, not the `n.` in the
/// margin), the destination's `→` at the START of its line — middle
/// ellipsis (a mile-long `from` does not push the `to` out of the box) and
/// masking MARKED with a badge ([`badge_prefixed`], Rust-side). The overflow
/// indicator carries a badge if any HIDDEN pair is hostile (what is hidden
/// does not slip through clean). Even though the engine guarantees UTF-8 on
/// the wire, an N+1/compromised daemon could send anything — it is painted
/// defensively ALWAYS, like the approval modal.
///
/// Below the pairs goes the BATCH's verdict (spec §17): the status of the
/// plan that `fs.rename_batch_plan` answered (in flight / applicable / not
/// applicable), how many steps are the planner's machinery — the NUMBER,
/// not the `.norte-rename-…` names, which nobody asked for — and the
/// collisions, ONE PER LINE with the offending name as the LAST field (a
/// clip can never eat into the verdict) and its ABSOLUTE pair index out of
/// band, which is what makes the guilty row pointable-to. A verdict from a
/// newer daemon degrades THAT line to a generic label, never the whole
/// modal.
pub(crate) fn ai_rename_plan_modal_text(
    dir: &norte_proto::VPath,
    entries: &[norte_proto::methods::AiRenameEntry],
    offset: usize,
    dialog_hints: &crate::hints::DialogHints,
    plan: &norte_frontend::BatchPlan,
) -> (String, String) {
    // Render belt-and-braces: the clamp lives in `App::ai_plan_scroll`, but
    // an out-of-range offset must never paint an empty window.
    let offset = offset.min(entries.len().saturating_sub(AI_RENAME_PAIR_LIMIT));
    let last = (offset + AI_RENAME_PAIR_LIMIT).min(entries.len());
    let (dir_txt, dir_hostile) = norte_frontend::path_display(dir);
    let mut lines = vec![badge_prefixed(
        dir_hostile,
        ta(
            "modal-ai-rename-dir",
            &[("dir", &middle_ellipsis(&dir_txt, 46))],
        ),
    )];
    // The batch's VERDICT goes up top, right after the dir and BEFORE the
    // pairs (§17): a modal taller than the terminal gets clipped by
    // `centered` from the BOTTOM, and of every line in the body this is the
    // one that cannot be lost — it is the one that says whether this is
    // going to rename anything.
    lines.push(t(plan.status_key()));
    for (i, e) in entries.iter().enumerate().take(last).skip(offset) {
        let (from, from_hostile) = display_name(e.from.as_bytes());
        let (to, to_hostile) = display_name(e.to.as_bytes());
        lines.push(badge_prefixed(
            from_hostile,
            ta(
                "modal-ai-rename-pair-from",
                &[
                    ("n", &(i + 1).to_string()),
                    ("from", &middle_ellipsis(&from, 46)),
                ],
            ),
        ));
        lines.push(badge_prefixed(
            to_hostile,
            ta(
                "modal-ai-rename-pair-to",
                &[("to", &middle_ellipsis(&to, 44))],
            ),
        ));
    }
    if entries.len() > AI_RENAME_PAIR_LIMIT {
        let hidden_hostile = entries.iter().enumerate().any(|(i, e)| {
            (i < offset || i >= last)
                && (display_name(e.from.as_bytes()).1 || display_name(e.to.as_bytes()).1)
        });
        lines.push(badge_prefixed(
            hidden_hostile,
            ta(
                "modal-ai-rename-more",
                &[
                    ("shown", &last.to_string()),
                    ("total", &entries.len().to_string()),
                ],
            ),
        ));
    }
    // The detail's sanitizing (masking, ellipsis, pair index, collision cap)
    // lives in `norte-frontend` so that EVERY surface shares it: these are
    // names an attacker controls, and a policy duplicated per frontend
    // drifts in one of them without anything warning about it. This
    // frontend only adds ITS badge.
    // In PARTS, with the name on its own line (#273): composing
    // `✗ 3. already exists: <name>` let a file named
    // `✗ 4. already exists: otro.txt` fabricate a list entry. Here there
    // are no sibling elements to separate the cause from the name, so the
    // LINE BREAK separates them, and only the name carries the badge.
    for parte in plan.detail_parts(entries.len(), norte_i18n::active()) {
        match parte {
            norte_frontend::DetailPart::Temp { count } => {
                lines.push(ta("modal-rename-batch-temp", &[("n", &count.to_string())]));
            }
            norte_frontend::DetailPart::Collision {
                index,
                kind_key,
                name,
                hostile,
            } => {
                let kind = t(kind_key);
                lines.push(match index {
                    Some(n) => ta(
                        "modal-rename-batch-collision-prefix",
                        &[("n", &n.to_string()), ("kind", &kind)],
                    ),
                    None => ta(
                        "modal-rename-batch-collision-prefix-unindexed",
                        &[("kind", &kind)],
                    ),
                });
                lines.push(format!("  {}", badge_prefixed(hostile, name)));
            }
            norte_frontend::DetailPart::More {
                shown,
                total,
                hostile,
            } => lines.push(badge_prefixed(
                hostile,
                ta(
                    "modal-rename-batch-collision-more",
                    &[("shown", &shown.to_string()), ("total", &total.to_string())],
                ),
            )),
        }
    }
    // H3c: with help on top, `y`/`n` do not respond — the footer says so
    // instead of offering them (twin of `DialogHints::with_modals_inert`,
    // for the two modals whose hint is prose and not a generated one).
    //
    // Without help on top, the footer follows `dialog_action`'s gate: with a
    // plan that cannot be applied, confirm is mute and offering it would be
    // a footer that lies (same doctrine as `modals_inert`).
    lines.push(if dialog_hints.modals_inert {
        t("modal-hint-help-open")
    } else if plan.confirmable() {
        t("modal-ai-rename-plan-hint")
    } else {
        t("modal-rename-batch-plan-hint-blocked")
    });
    (t("modal-ai-rename-plan"), lines.join("\n"))
}

/// The marker for a folder the plan CREATES.
const ORGANIZE_NEW: &str = "+ ";
/// The one for a folder that already existed.
const ORGANIZE_EXISTING: &str = "· ";
/// The one for a file being moved.
const ORGANIZE_FILE: &str = "→ ";

/// Title and body of `Modal::OrganizePlan` (phase 8), with roles.
///
/// **Why a tree and not pairs.** A rename plan is reviewed as `from → to`
/// because that is what it is. An organize plan changes the SHAPE of the
/// directory, and forty rows of `a.pdf → invoices/2026/a.pdf` do not let you
/// see that shape: not how many new folders appear, nor which, nor what
/// ends up inside each one. The tree is computed by
/// [`norte_frontend::organize::tree_lines`], shared with the window — which
/// folder is new cannot be decided twice, because what the human believes
/// is going to happen depends on it.
///
/// **The summary goes BEFORE the tree**, right after the dir: "creates 3
/// folders and moves 12 files" is what is needed to decide without counting
/// lines, and a modal taller than the terminal gets clipped by `centered`
/// from the bottom.
///
/// **The markers always appear, and that is why they are legible.** Each
/// line carries one (`+`/`·`/`→`) after its indent; a name that starts with
/// `→` — legitimate, it is not a terminal hazard, it is not masked — is
/// painted after ours (`→ → a.pdf`) instead of replacing it. The ROLE (new
/// in `Strong`, existing in `Dim`) says the same thing in color, for anyone
/// who does not want to count glyphs; the marker is there because color does
/// not survive a monochrome theme.
///
/// Defensive masking ALWAYS, like the other reviewable plans: the names are
/// proposed by a producer (a model or a plugin) and the engine validates
/// them, but an N+1 or compromised daemon can send anything.
pub(crate) fn organize_plan_modal(
    dir: &norte_proto::VPath,
    lineas: &[norte_frontend::organize::TreeLine],
    offset: usize,
    dialog_hints: &crate::hints::DialogHints,
) -> (String, ModalBody) {
    use norte_frontend::organize::{ORGANIZE_LINE_LIMIT, TreeKind};
    // Render belt-and-braces: the clamp lives in
    // `App::organize_plan_scroll`, but an out-of-range offset must never
    // paint an empty window.
    let offset = offset.min(lineas.len().saturating_sub(ORGANIZE_LINE_LIMIT));
    let last = (offset + ORGANIZE_LINE_LIMIT).min(lineas.len());
    let (dir_txt, dir_hostile) = norte_frontend::path_display(dir);
    let (dirs, files) = norte_frontend::organize::resumen(lineas);
    let mut body: ModalBody = vec![
        ModalLine::new(
            ta(
                "modal-ai-rename-dir",
                &[("dir", &middle_ellipsis(&dir_txt, 46))],
            ),
            LineKind::Dim,
        )
        .hostile(dir_hostile),
        ModalLine::new(
            ta(
                "modal-organize-summary",
                &[("dirs", &dirs.to_string()), ("files", &files.to_string())],
            ),
            LineKind::Strong,
        ),
    ];
    for l in lineas.iter().take(last).skip(offset) {
        let (texto, hostil) = display_name(l.text.as_bytes());
        // The indent is bounded: the depth is validated by the proto
        // (`ORGANIZE_MAX_DEPTH`), but a painted body does not depend on the
        // other end having validated anything.
        let indent = "  ".repeat(l.depth.min(norte_proto::methods::ORGANIZE_MAX_DEPTH));
        let (marca, papel) = match l.kind {
            TreeKind::NewDir => (ORGANIZE_NEW, LineKind::Strong),
            TreeKind::ExistingDir => (ORGANIZE_EXISTING, LineKind::Dim),
            TreeKind::Moved => (ORGANIZE_FILE, LineKind::Plain),
        };
        let width = 44usize.saturating_sub(indent.width()).max(8);
        body.push(
            ModalLine::new(
                format!("{indent}{marca}{}", middle_ellipsis(&texto, width)),
                papel,
            )
            .hostile(hostil),
        );
    }
    if lineas.len() > ORGANIZE_LINE_LIMIT {
        // What is hidden does not slip through clean: if any line OUTSIDE
        // the window is painted different from its bytes, the indicator
        // says so.
        let oculto_hostil = lineas
            .iter()
            .enumerate()
            .any(|(i, l)| (i < offset || i >= last) && display_name(l.text.as_bytes()).1);
        body.push(
            ModalLine::new(
                ta(
                    "modal-ai-rename-more",
                    &[
                        ("shown", &last.to_string()),
                        ("total", &lineas.len().to_string()),
                    ],
                ),
                LineKind::Dim,
            )
            .hostile(oculto_hostil),
        );
    }
    // H3c: with help on top the modal's keys do not respond, and the footer
    // stops offering them — same doctrine as the rename plan.
    body.push(ModalLine::new(
        if dialog_hints.modals_inert {
            t("modal-hint-help-open")
        } else {
            dialog_hints.approval.clone()
        },
        LineKind::Dim,
    ));
    (t("modal-organize-plan"), body)
}

/// Title+body of `Modal::SemanticHits` (M4-IA-2, encoding-auditor doctrine,
/// same mold as `ai_rename_plan_modal_text`): the WINDOW of
/// [`SEMANTIC_HIT_LIMIT`] hits from `offset`, one hit PER LINE with a cursor
/// marker (`>`) and an ABSOLUTE numbered label out of band, path through
/// `norte_frontend::path_display` (mask + hostile flag) with a Rust-side
/// badge ([`badge_prefixed`]) and middle ellipsis (a mile-long path does not
/// push the score out of the box); the score `{:.2}` at the end. The
/// overflow indicator carries a badge if any HIDDEN hit is hostile (what is
/// hidden does not slip through clean). Even though the engine guarantees
/// the wire, an N+1/compromised daemon could send anything — it is painted
/// defensively ALWAYS.
pub(crate) fn semantic_hits_modal_text(
    hits: &[norte_proto::methods::SemanticHit],
    offset: usize,
    cursor: usize,
    dialog_hints: &crate::hints::DialogHints,
) -> (String, String) {
    // Render belt-and-braces: the clamp lives in `App::semantic_cursor`, but
    // an out-of-range offset must never paint an empty window.
    let offset = offset.min(hits.len().saturating_sub(SEMANTIC_HIT_LIMIT));
    let last = (offset + SEMANTIC_HIT_LIMIT).min(hits.len());
    let mut lines = Vec::new();
    for (i, h) in hits.iter().enumerate().take(last).skip(offset) {
        let (path, hostile) = norte_frontend::path_display(&h.path);
        let line = badge_prefixed(
            hostile,
            ta(
                "modal-semantic-hit",
                &[
                    ("n", &(i + 1).to_string()),
                    ("path", &middle_ellipsis(&path, 44)),
                    ("score", &format!("{:.2}", h.score)),
                ],
            ),
        );
        // Cursor marker OUT of band, in a fixed column BEFORE the badge (a
        // path cannot imitate it: it is masked and comes after the label).
        lines.push(if i == cursor {
            format!("> {line}")
        } else {
            format!("  {line}")
        });
    }
    if hits.len() > SEMANTIC_HIT_LIMIT {
        let hidden_hostile = hits
            .iter()
            .enumerate()
            .any(|(i, h)| (i < offset || i >= last) && norte_frontend::path_display(&h.path).1);
        lines.push(badge_prefixed(
            hidden_hostile,
            ta(
                "modal-semantic-more",
                &[
                    ("shown", &last.to_string()),
                    ("total", &hits.len().to_string()),
                ],
            ),
        ));
    }
    // H3c: see `ai_rename_plan_modal_text` — same reason, same string.
    lines.push(if dialog_hints.modals_inert {
        t("modal-hint-help-open")
    } else {
        t("modal-semantic-hits-hint")
    });
    (t("modal-semantic-hits"), lines.join("\n"))
}

/// Title+body of `Modal::TransferName` (#105): same masking contract as
/// `mkdir_modal_text` — the destination dir, the name and the diagnostic are
/// user text/bytes. The dir goes on its own line (never an in-band joiner
/// with the name — the #103 modals' discipline).
/// What is known about the DESTINATION of a transfer, for painting it.
///
/// The two together because they are the same class of line — a fact about
/// the destination worth knowing before saying yes — and because they
/// appear one after the other, in that order.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DestNotices<'a> {
    /// "Does not fit" (#149). `None` = it fits, or how much it takes is
    /// unknown.
    pub space: Option<&'a str>,
    /// "This destination cannot confine the writes" (#164, #219).
    pub confine: Option<&'a str>,
}

/// Title and body of `Modal::ConfirmTransfer`, with roles.
///
/// **Why the destination stops being marked with an arrow.** Its line was
/// `→ ⟨file⟩/other/path`, above a list of other NAMES, and the comment
/// claimed no name could imitate it. That rested on two things: that a name
/// cannot carry `/` — true on all three OSes — and on the scheme prefix.
/// The second is no longer there for local paths, and the first has
/// homoglyphs: `∕` (U+2215), `⁄` (U+2044) and `／` (U+FF0F) are legal on
/// ext4, APFS and NTFS, are not a terminal hazard, are not masked and are
/// not flagged. A file named `→ ∕srv∕public` fabricates that whole line.
///
/// The role now lives in the STYLE — the destination is `Strong`, the list
/// rows are `Plain` — and that a name cannot fabricate, because it does not
/// write it. The label is the same one `transfer_name_modal` uses.
fn confirm_transfer_modal(
    kind: crate::app::TransferKind,
    items: &[norte_proto::VPath],
    to: &norte_proto::VPath,
    reinterpret: Option<norte_encoding::NameEncoding>,
    dest: DestNotices<'_>,
    keys: &str,
) -> (String, ModalBody) {
    let title = match kind {
        crate::app::TransferKind::Copy => t("modal-copy-title"),
        crate::app::TransferKind::Move => t("modal-move-title"),
    };
    let mut lines: ModalBody = norte_frontend::item_lines_with(items, HOSTILE_BADGE, reinterpret)
        .into_iter()
        .map(ModalLine::plain)
        .collect();
    let (path_text, hostile) = norte_frontend::path_display_with(to, reinterpret);
    lines.push(
        ModalLine::new(
            format!("{}  {}", t("modal-transfer-to"), path_text),
            LineKind::Strong,
        )
        .hostile(hostile),
    );
    // #149/#164: the two facts about the destination, below it and above the
    // keys — the last thing read before deciding. Neither blocks anything.
    lines.extend(dest.space.map(|s| ModalLine::new(s, LineKind::Warning)));
    lines.extend(dest.confine.map(|s| ModalLine::new(s, LineKind::Warning)));
    lines.push(ModalLine::new(keys, LineKind::Dim));
    (title, lines)
}

/// Title and body of `Modal::TransferName` (#105), with roles.
///
/// **What is read and in what order**, which is what this modal used to get
/// wrong: the editable field went third, with nothing to set it apart from the text
/// next to it, and its label BELOW — meaning you read a name, and only
/// afterwards found out it was the only thing you could change. Now it goes
/// from-where-to-where, then the label, then the field, and the keys at the
/// end.
///
/// **The origin shows the DIRECTORY, not the file.** The name appeared
/// twice — truncated up top and whole in the field — and in a five-line
/// body that was repeating 40% of it. The name lives in the field, which is
/// where it is edited.
///
/// **Out-of-band labels, no arrow.** The destination used to be marked with
/// a `→` at the start of its line; `→` (U+2192) is legitimate in a name, is
/// not a terminal hazard and therefore is not masked nor flagged, so a
/// directory named `docs → /home/DELETE` fabricated a line that reads as
/// two paths. That is what the corpus's `arrow_join_spoof` fixture says and
/// what the host already did in `DialogView::destination`: the label goes
/// in its own column, never inside the text.
///
/// Same masking contract as `mkdir_modal_text` — the destination dir, the
/// name and the diagnostic are user text/bytes.
pub(crate) fn transfer_name_modal(
    kind: crate::app::TransferKind,
    from: &norte_proto::VPath,
    to_dir: &norte_proto::VPath,
    name: &str,
    error: Option<&str>,
    enc: Option<norte_encoding::NameEncoding>,
    dest: DestNotices<'_>,
) -> (String, ModalBody) {
    let (masked, hostile) = display_name(name.as_bytes());
    // The TAIL, with the cut mark: it is the same thing every free-text
    // prompt does (`free_text_modal_text`) and for the same reason, which
    // this modal never got. Unbounded, a name wider than the box was cut
    // flush against the border: the tail was lost, the cursor was lost, and
    // from then on typing changed nothing on screen — i.e. confirming a
    // name you cannot see. The field's background, which now reaches the
    // border, hid it even better.
    let visible = tail_window(&masked, FREE_TEXT_FIELD_MAX);
    // `_` marks where what was typed ends. `▏` (U+258F) was tried to avoid
    // being confused with an underscore in the name itself, and it was
    // worse: it is East_Asian_Width=Ambiguous, so on a CJK terminal with
    // ambiguous-width doubled it takes up TWO cells, `modal_width` budgets
    // for one, and the end-of-field mark is the first thing clipped. `_` is
    // `Na`, unambiguous, and is what the other five fields use.
    //
    // The altered mark is NOT put in here: it travels on the line, which
    // paints it with its own role and guaranteed contrast.
    let field = format!("{visible}_");
    // The path goes with MIDDLE ELLIPSIS, as in the approval modal and the
    // collision one: `modal_width` caps against the frame's width and
    // `draw_modal`'s `Paragraph` does not wrap, so a deep path used to be
    // cut flush against the border and push the destination's TAIL out of
    // the box — exactly what the user needs to see to know where the copy
    // lands — without even a `…` to give it away. Under the encoding
    // CAPTURED at opening (#98/M1), never the pane's at paint time.
    let label_from = t("modal-transfer-from");
    let label_to = t("modal-transfer-to");
    let width = label_from.width().max(label_to.width());
    // The mark comes SEPARATE from the text: the line carries it in its own
    // field and it is painted with `Role::HostileBadge`. Inside the text it
    // inherited the role's style, and with the origin line dimmed the spec
    // §6 signal was left at 2.3:1 on a light theme.
    let with_label = |label: &str, p: &norte_proto::VPath| {
        let (line, hostile) = norte_frontend::path_display_with(p, enc);
        let line = middle_ellipsis(&line, MODAL_PATH_CHARS);
        let pad = " ".repeat(width.saturating_sub(label.width()));
        (format!("{label}{pad}  {line}"), hostile)
    };
    // Where it comes FROM. The DIRECTORY when the name is in the field,
    // because then showing the whole path would repeat it. But a RENAME
    // opens with `to_dir = from.parent()` — it is the same folder, nothing
    // moves — so there the directory does not name anything: as soon as the
    // user types, the field becomes the NEW name and the thing being
    // renamed does not appear anywhere on screen. That is blindly
    // confirming a mutation whose operand cannot be seen, exactly what ADR
    // 0070 forbids. With the origin and the destination in the same folder,
    // the "From" line carries the WHOLE path.
    let is_rename = from.parent().as_ref() == Some(to_dir);
    let origin = if is_rename {
        from.clone()
    } else {
        from.parent().unwrap_or_else(|| from.clone())
    };
    let (from_text, from_hostile) = with_label(&label_from, &origin);
    let (to_text, to_hostile) = with_label(&label_to, to_dir);
    let mut lines = vec![
        ModalLine::new(from_text, LineKind::Dim).hostile(from_hostile),
        // The destination is what has to be read before saying yes.
        ModalLine::new(to_text, LineKind::Strong).hostile(to_hostile),
        ModalLine::plain(""),
        ModalLine::new(t("modal-transfer-name-hint"), LineKind::Dim),
        ModalLine::new(field, LineKind::Field).hostile(hostile),
        ModalLine::plain(""),
    ];
    // The two destination notices, in the same place and the same order as
    // in `ConfirmTransfer`: below the destination and above the keys, which
    // is the last thing read before deciding (#149, #164). Copying a
    // SINGLE file did not have them, and that is why a lone sheet was
    // copied without knowing whether the destination confines its writes
    // (#343).
    lines.extend(dest.space.map(|s| ModalLine::new(s, LineKind::Warning)));
    lines.extend(dest.confine.map(|s| ModalLine::new(s, LineKind::Warning)));
    lines.push(ModalLine::new(t("modal-mark-pattern-keys"), LineKind::Dim));
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(ModalLine::new(masked_err, LineKind::Error));
    }
    let title = match kind {
        crate::app::TransferKind::Copy => t("modal-transfer-name-copy"),
        crate::app::TransferKind::Move => t("modal-transfer-name-move"),
    };
    (title, lines)
}

#[cfg(test)]
mod transfer_name_modal_text_tests {
    use super::{LineKind, ModalBody, transfer_name_modal};
    use crate::app::TransferKind;
    use norte_proto::VPath;

    /// The body as a single string, for the masking assertions.
    fn body_text(body: &ModalBody) -> String {
        body.iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// #105 review MINOR-2 (same class as the pattern's M4): PURE fn — a raw
    /// RLO in the name and the error comes out masked, and a hostile byte in
    /// the ORIGIN and the destination dir never arrives raw (`path_display`
    /// masks them and they carry a badge).
    #[test]
    fn masks_every_user_surface() {
        let hostile = "abc\u{202E}rid";
        // The hostile byte goes in the origin DIRECTORY, which is what this
        // modal shows now: the file name lives in the field.
        let from = VPath::parse("mem:///src%FF/a.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst%FE").unwrap();
        let (_, body) = transfer_name_modal(
            TransferKind::Move,
            &from,
            &to_dir,
            hostile,
            Some(hostile),
            None,
            super::DestNotices::default(),
        );
        let body = body_text(&body);
        assert!(!body.contains('\u{202E}'), "{body:?}");
        assert!(
            body.matches('\u{FFFD}').count() >= 4,
            "name + error (RLO) and origin + destination (bytes): {body:?}"
        );
    }

    /// The altered mark goes in the line's FIELD, not inside the text: it is
    /// painted with `Role::HostileBadge`, whose contrast the `norte-theme`
    /// gate guarantees across all eight presets. Inside the text it
    /// inherited its line's role style, and a dimmed line left the spec §6
    /// signal at 2.3:1 on a light theme.
    #[test]
    fn altered_mark_stays_apart_from_the_text() {
        let hostile = "abc\u{202E}rid";
        let from = VPath::parse("mem:///src%FF/a.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst%FE").unwrap();
        let (_, body) = transfer_name_modal(
            TransferKind::Move,
            &from,
            &to_dir,
            hostile,
            None,
            None,
            super::DestNotices::default(),
        );
        assert!(
            !body_text(&body).contains(super::HOSTILE_BADGE),
            "the mark does not go into the text: {:?}",
            body_text(&body)
        );
        // Origin, destination and field: all three carry altered bytes and
        // all three say so.
        assert_eq!(body.iter().filter(|l| l.hostile).count(), 3, "{body:?}");
        // And the line's width counts the mark: otherwise the box comes out
        // short exactly on the lines that carry one.
        let marked = body
            .iter()
            .find(|l| l.hostile)
            .expect("there is a marked one");
        assert!(marked.width() > unicode_width::UnicodeWidthStr::width(marked.text.as_str()));
    }

    /// **The field declares itself FIELD, and exactly one line is.**
    ///
    /// It is the only thing the user can change, and with no role it came
    /// out the same color as the text next to it: you read a name and
    /// nothing said it was editable. Its label goes RIGHT ABOVE — it used to
    /// be below, so you read the value before knowing what it was.
    #[test]
    fn the_field_declares_itself_field_and_its_label_sits_above() {
        let from = VPath::parse("mem:///src/a.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst").unwrap();
        let (_, body) = transfer_name_modal(
            TransferKind::Copy,
            &from,
            &to_dir,
            "a.txt",
            None,
            None,
            super::DestNotices::default(),
        );

        let fields: Vec<usize> = body
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind == LineKind::Field)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(fields.len(), 1, "exactly one line is the field");
        let i = fields[0];
        assert!(body[i].text.contains("a.txt"));
        assert_eq!(
            body[i - 1].kind,
            LineKind::Dim,
            "and right above it goes its label, dimmed"
        );

        // The destination stands out; the origin does not compete with it.
        assert_eq!(body[0].kind, LineKind::Dim, "where it comes from");
        assert_eq!(body[1].kind, LineKind::Strong, "where it goes");
    }

    /// **The file name is NOT repeated**: up top goes the origin
    /// DIRECTORY, because the name is in the field. It used to appear in
    /// both, and in a five-line body that was repeating 40% of it.
    #[test]
    fn the_origin_shows_the_directory_not_the_file() {
        let from = VPath::parse("mem:///src/unico.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst").unwrap();
        let (_, body) = transfer_name_modal(
            TransferKind::Copy,
            &from,
            &to_dir,
            "unico.txt",
            None,
            None,
            super::DestNotices::default(),
        );
        assert_eq!(
            body_text(&body).matches("unico.txt").count(),
            1,
            "once, in the field: {:?}",
            body_text(&body)
        );
        assert!(body[0].text.contains("/src"), "{:?}", body[0].text);
    }

    /// **A rename NAMES the file it renames** (ADR 0070).
    ///
    /// A rename opens with `to_dir = from.parent()`: the same folder,
    /// nothing moves. Showing only the directory, "From" and "To" came out
    /// identical and the name being changed did not appear ANYWHERE as soon
    /// as the user typed — the field becomes the new name. That is blindly
    /// confirming a mutation whose operand cannot be seen, with the mark
    /// consumed on submit.
    #[test]
    fn a_rename_names_the_file_it_renames() {
        let from = VPath::parse("mem:///casa/docs/contrato-final.pdf").unwrap();
        let to_dir = from.parent().expect("parent");
        let (_, body) = transfer_name_modal(
            TransferKind::Move,
            &from,
            &to_dir,
            // Already typed: the field is the NEW name.
            "contrato-v2.pdf",
            None,
            None,
            super::DestNotices::default(),
        );
        assert!(
            body[0].text.contains("contrato-final.pdf"),
            "the operand has to be written: {:?}",
            body[0].text
        );
        assert_ne!(
            body[0].text, body[1].text,
            "and the two lines cannot say the same thing"
        );
    }

    /// **Not a single arrow inside the text.** `→` is legitimate in a name
    /// and is not masked, so it used to mark the destination with a
    /// character a directory can carry: `docs → /home/DELETE` fabricated a
    /// line that reads as two paths (fixture `arrow_join_spoof`). The label
    /// goes in its own column.
    #[test]
    fn the_destination_label_is_out_of_band() {
        let from = VPath::parse("mem:///src/a.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst").unwrap();
        let (_, body) = transfer_name_modal(
            TransferKind::Copy,
            &from,
            &to_dir,
            "a.txt",
            None,
            None,
            super::DestNotices::default(),
        );
        assert!(!body_text(&body).contains('→'), "{:?}", body_text(&body));
    }

    /// **A name wider than the box shows its TAIL, with the mark.**
    ///
    /// Unbounded it was cut flush against the border: the tail was lost, the
    /// `_` that says where you are typing was lost, and from then on typing
    /// changed nothing on screen. The background fill up to the border hid
    /// it even better — the box looks complete and what is inside is not the
    /// name. It is the same fix the other five fields already had
    /// (`tail_window`), and this one had not received it.
    #[test]
    fn a_name_wider_than_the_box_shows_its_tail() {
        let long = "a".repeat(101);
        let from = VPath::parse("mem:///src/x").unwrap();
        let to_dir = VPath::parse("mem:///dst").unwrap();
        let (_, body) = transfer_name_modal(
            TransferKind::Copy,
            &from,
            &to_dir,
            &long,
            None,
            None,
            super::DestNotices::default(),
        );
        let field = body
            .iter()
            .find(|l| l.kind == LineKind::Field)
            .expect("there is a field");
        assert!(
            field.text.contains('…'),
            "the cut is SAID: {:?}",
            field.text
        );
        assert!(
            field.text.ends_with('_'),
            "and the cursor survives the clip: {:?}",
            field.text
        );
        assert!(
            field.width() <= super::FREE_TEXT_FIELD_MAX + 2,
            "bounded like the other five fields: {}",
            field.width()
        );
    }

    /// **The height covers the body, and it can no longer fail to.**
    ///
    /// `modal_height` was a second source of truth — a hand-written formula
    /// per variant — and the module's rustdoc had been saying since the
    /// start that if the two halves drift apart the modal gets clipped,
    /// with not a single test tying them together. There are no longer two:
    /// the height is DERIVED from the body.
    ///
    /// The test stays because it still says something — that the optional
    /// lines are counted — and because it is where it would show if someone
    /// puts a hand-written number back in.
    #[test]
    fn the_height_covers_the_body() {
        use crate::app::Modal;
        let from = VPath::parse("mem:///src/a.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst").unwrap();
        for error in [None, Some("bad".to_owned())] {
            for space in [None, Some("does not fit".to_owned())] {
                for confine in [None, Some("does not confine".to_owned())] {
                    let modal = Modal::TransferName {
                        kind: TransferKind::Copy,
                        from: from.clone(),
                        to_dir: to_dir.clone(),
                        name: "a.txt".to_owned(),
                        original: b"a.txt".to_vec(),
                        touched: false,
                        from_marks: false,
                        enc: None,
                        error: error.clone(),
                        space: space.clone(),
                        confine: confine.clone(),
                    };
                    let (_, body) = super::modal_title_body(
                        &modal,
                        None,
                        &crate::hints::DialogHints::default(),
                    );
                    let height = super::body_height(&body);
                    assert!(
                        usize::from(height) >= body.len() + 2,
                        "the body ({} lines) does not fit in {height} rows with its \
                         borders: the last one gets clipped without saying so",
                        body.len()
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod free_text_modal_text_tests {
    use super::{free_text_modal_text, tail_window};

    /// Each free-text prompt uses ITS OWN ids and all four exist in both
    /// locales (S4 review, m5). Without this, an id with a typo would paint
    /// verbatim on screen — Fluent falls back to the id itself — with the
    /// gate green.
    #[test]
    fn each_prompt_resolves_its_title_and_its_hint() {
        let cases = [
            ("modal-mkdir", "modal-mkdir-hint"),
            ("modal-command-line", "modal-command-line-hint"),
            ("modal-ai-rename", "modal-ai-rename-hint"),
            ("modal-semantic", "modal-semantic-hint"),
        ];
        for (titulo, hint) in cases {
            let (t, body) = free_text_modal_text(titulo, hint, "x", None);
            assert_ne!(t, titulo, "{titulo} untranslated: the raw id comes out");
            let hint_line = body.lines().nth(1).expect("hint");
            assert_ne!(hint_line, hint, "{hint} untranslated: the raw id comes out");
        }
    }

    /// The field shows the TAIL, with the cut mark, so the cursor is always
    /// in view: a command whose end cannot be seen is a command run blind.
    #[test]
    fn a_long_value_shows_its_tail_and_marks_the_cut() {
        let long = "a".repeat(300);
        let (_, body) =
            free_text_modal_text("modal-command-line", "modal-command-line-hint", &long, None);
        let field = body.lines().next().expect("field");
        assert!(field.starts_with('…'), "the cut is marked: {field:?}");
        assert!(field.ends_with('_'), "and the cursor is visible: {field:?}");
        assert!(
            field.chars().count() <= 52,
            "bounded: {}",
            field.chars().count()
        );
    }

    /// **`tail_window` counts terminal CELLS.**
    ///
    /// It used to count chars, and for what this budget protects — that the
    /// field fits its box — that is the wrong measure: fifty CJK chars are
    /// a hundred cells, so a Japanese name overflowed just the same and the
    /// clip against the border took the end cursor with it. The transfer
    /// modal's first screenshot exposed it.
    #[test]
    fn the_tail_window_counts_cells() {
        assert_eq!(tail_window("abc", 10), "abc");
        assert_eq!(tail_window("abcdef", 3), "…ef");

        // Eight ideographs are SIXTEEN cells: with a budget of four, only
        // the `…` and one more fit, not four.
        let cjk = "日本語のファイル";
        let w = tail_window(cjk, 4);
        assert!(w.starts_with('…'), "{w:?}");
        assert!(
            norte_frontend::cells(&w) <= 4,
            "{w:?} measures {} cells",
            norte_frontend::cells(&w)
        );

        // And what fits whole is untouched, no matter how it is measured.
        assert_eq!(tail_window(cjk, 16), cjk);
    }

    /// Same pin as the pattern's (#103 M4): PURE fn — a raw RLO in the name
    /// AND in the error comes out masked on BOTH lines.
    #[test]
    fn masks_a_raw_rtl_override_in_name_and_error() {
        let hostile = "abc\u{202E}rid";
        let (_, body) =
            free_text_modal_text("modal-mkdir", "modal-mkdir-hint", hostile, Some(hostile));
        assert!(!body.contains('\u{202E}'), "{body:?}");
        assert_eq!(body.matches('\u{FFFD}').count(), 2, "{body:?}");
    }
}

#[cfg(test)]
mod mark_pattern_modal_text_tests {
    use super::mark_pattern_modal_text;

    /// Review MAJOR M4: `mark_pattern_modal_text` is pure — test the
    /// masking directly instead of through a `TestBackend` buffer, where
    /// ratatui's paragraph renderer EATS zero-width graphemes: U+202E never
    /// survives THERE, masked or not, so a render-test assertion against it
    /// can never fail (the class of bug that motivated this test). A
    /// pattern AND an error carrying a raw RLO must both come out masked:
    /// not a single U+202E survives, and U+FFFD appears exactly twice — one
    /// per masked line.
    #[test]
    fn masks_a_raw_rtl_override_in_both_the_pattern_and_the_error() {
        let hostile = "abc\u{202E}gpj.exe";
        let (_, body) = mark_pattern_modal_text(true, hostile, Some(hostile));
        assert!(
            !body.contains('\u{202E}'),
            "raw RTL override must not survive: {body:?}"
        );
        assert_eq!(
            body.matches('\u{FFFD}').count(),
            2,
            "one U+FFFD per masked line (pattern + error): {body:?}"
        );
    }

    /// With no error, only the pattern line is masked: a single U+FFFD.
    #[test]
    fn masks_only_the_pattern_line_when_there_is_no_error() {
        let hostile = "abc\u{202E}gpj.exe";
        let (_, body) = mark_pattern_modal_text(true, hostile, None);
        assert_eq!(body.matches('\u{FFFD}').count(), 1);
    }
}

/// `(title, body)` text of the TOFU modal (#45). host/algo/fingerprint come
/// from the REMOTE SERVER (not trusted) and this is a security decision:
/// same masking as agent paths (controls/bidi/invisibles → �) + clamp. A
/// legitimate fingerprint is ASCII (`SHA256:<base64>`), so the masking is a
/// no-op unless the server tries to hide characters — in which case the �
/// GIVES AWAY the manipulation.
pub(crate) fn trust_host_modal_text(
    host: &str,
    port: Option<u16>,
    algo: &str,
    fingerprint: &str,
    hint: &str,
) -> (String, String) {
    let (host_txt, host_hostile) = display_name(host.as_bytes());
    let hostport = match port {
        Some(p) => format!("{}:{p}", clamp_chars(&host_txt, 48)),
        None => clamp_chars(&host_txt, 48),
    };
    let (algo_disp, algo_hostile) = display_name(algo.as_bytes());
    let algo_txt = clamp_chars(&algo_disp, 24);
    let (fp_txt, fp_hostile) = display_name(fingerprint.as_bytes());
    let lines = [
        ta(
            "modal-trust-host-host",
            &[
                ("badge", if host_hostile { HOSTILE_BADGE } else { "" }),
                ("host", &hostport),
            ],
        ),
        ta(
            "modal-trust-host-algo",
            &[
                ("badge", if algo_hostile { HOSTILE_BADGE } else { "" }),
                ("algo", &algo_txt),
            ],
        ),
        ta(
            "modal-trust-host-fp",
            &[
                ("badge", if fp_hostile { HOSTILE_BADGE } else { "" }),
                ("fingerprint", &clamp_chars(&fp_txt, 52)),
            ],
        ),
        t("modal-trust-host-note"),
        hint.to_owned(),
    ];
    (t("modal-trust-host-title"), lines.join("\n"))
}

/// How many dots at most [`crate::app::Modal::AskSecret`]'s field paints.
///
/// A cap and not the real length because a 200-character passphrase would
/// overflow the box. **It does not hide the length**: below the cap there is
/// one dot per character, which is exactly the length — and it stays that
/// way on purpose, because seeing a dot appear is the only confirmation
/// that the keystroke landed in a field that shows nothing.
const SECRET_DOTS_MAX: usize = 32;

/// Title and body of the password dialog (#325).
///
/// Paints the connection, **where it connects to** and one dot per typed
/// character (up to [`SECRET_DOTS_MAX`]). It is the only function in this
/// family that receives a secret, and the only thing it does with it is
/// count it.
///
/// The endpoint is not decoration: a password dialog that only says
/// `connection: work` cannot be answered with judgment — the name was
/// chosen by `connections.toml`, which can come from someone else's
/// dotfiles or from an edited line, and `work` does not say whether that
/// entry points today where it pointed yesterday. It is the same reason the
/// host-key TOFU shows the fingerprint. It arrives from the core already
/// redacted (no userinfo) and is sanitized here like everything else.
pub(crate) fn ask_secret_modal_text(
    conn: &str,
    endpoint: &str,
    input: &crate::app::TypedSecret,
    hint: &str,
) -> (String, String) {
    let (conn_txt, conn_hostile) = display_name(conn.as_bytes());
    let (ep_txt, ep_hostile) = display_name(endpoint.as_bytes());
    let lines = [
        ta(
            "modal-ask-secret-conn",
            &[
                ("badge", if conn_hostile { HOSTILE_BADGE } else { "" }),
                ("conn", &clamp_chars(&conn_txt, 48)),
            ],
        ),
        ta(
            "modal-ask-secret-endpoint",
            &[
                ("badge", if ep_hostile { HOSTILE_BADGE } else { "" }),
                ("endpoint", &clamp_chars(&ep_txt, 52)),
            ],
        ),
        ta(
            "modal-ask-secret-field",
            &[("dots", &"•".repeat(input.chars().min(SECRET_DOTS_MAX)))],
        ),
        t("modal-ask-secret-note"),
        hint.to_owned(),
    ];
    (t("modal-ask-secret-title"), lines.join("\n"))
}

#[cfg(test)]
mod ai_rename_plan_modal_tests {
    use super::{HOSTILE_BADGE, ai_rename_plan_modal_text, display_name};
    use norte_proto::methods::{
        AiRenameEntry, FsRenameBatchPlanResult, PlanHash, RenameCollision, RenameCollisionKind,
        RenameStep,
    };
    use norte_proto::{Segment, VPath};

    fn dir() -> VPath {
        VPath::parse("mem:///proyecto").expect("valid wire")
    }

    fn entry(from: &str, to: &str) -> AiRenameEntry {
        AiRenameEntry {
            from: from.into(),
            to: to.into(),
        }
    }

    fn seg(b: &[u8]) -> Segment {
        Segment::new(b.to_vec()).expect("segment")
    }

    fn hash() -> PlanHash {
        PlanHash::parse(&"0".repeat(64)).expect("64 hex")
    }

    /// The NORMAL case: the core answered that the batch can be executed.
    fn plan_ok() -> norte_frontend::BatchPlan {
        listo(FsRenameBatchPlanResult {
            steps: vec![RenameStep {
                from: seg(b"a"),
                to: seg(b"b"),
                temp: false,
            }],
            collisions: vec![],
            executable: true,
            plan_hash: hash(),
        })
    }

    /// Wraps a core plan in the "already answered" state.
    fn listo(p: FsRenameBatchPlanResult) -> norte_frontend::BatchPlan {
        norte_frontend::BatchPlan::Ready(Box::new(p))
    }

    /// A batch STOPPED by a verdict (`steps` empty: the proto's invariant —
    /// an unexecutable plan never comes half-sorted).
    fn plan_con_colision(kind: RenameCollisionKind, name: &[u8]) -> norte_frontend::BatchPlan {
        listo(FsRenameBatchPlanResult {
            steps: vec![],
            collisions: vec![RenameCollision {
                pair_index: 0,
                name: seg(name),
                kind,
            }],
            executable: false,
            plan_hash: hash(),
        })
    }

    /// H3c: with help open ON TOP, the modal's keys do not respond, so its
    /// footer cannot keep offering them.
    ///
    /// This modal and the semantic-hits one are the only two whose hint is
    /// Fluent PROSE instead of a generated hint, and that is why they need
    /// this branch: the generated ones are already replaced by
    /// `DialogHints::with_modals_inert`. They are NOT the only two a help
    /// screen can cover — that is decided by
    /// `help_context::help_over_modal_allowed`, and it includes agent
    /// approval and the host-key TOFU. Without this branch, a reader with
    /// help in front saw "y/Enter: apply" and neither did anything: a
    /// footer that lies, exactly what `hints.rs`'s design exists not to
    /// have.
    #[test]
    fn the_plans_footer_does_not_offer_inert_keys_under_help() {
        use norte_i18n::t;
        let alive = crate::hints::DialogHints::default();
        let (_, normal) =
            ai_rename_plan_modal_text(&dir(), &[entry("a", "b")], 0, &alive, &plan_ok());
        assert!(
            normal.contains(&t("modal-ai-rename-plan-hint")),
            "with no help on top, the footer offers its keys: {normal}"
        );

        let inert = alive.with_modals_inert();
        let (_, covered) =
            ai_rename_plan_modal_text(&dir(), &[entry("a", "b")], 0, &inert, &plan_ok());
        assert!(
            !covered.contains(&t("modal-ai-rename-plan-hint")),
            "with help on top it must NOT offer y/n: {covered}"
        );
        assert!(
            covered.contains(&t("modal-hint-help-open")),
            "and it has to say why: {covered}"
        );
    }

    /// Audit MINOR-6a (canonical corpus, `app.rs`'s sweep mold): every
    /// hostile name, in the `from` position AND the `to` one — no
    /// `is_terminal_hazard` char survives in the painted text, and when the
    /// masking alters the name the line is MARKED with the badge.
    #[test]
    fn corpus_sweep_no_hazard_survives_and_masking_flags_it() {
        for n in norte_testkit::corpus::hostile_names() {
            let name = String::from_utf8_lossy(&n.bytes).into_owned();
            let cases = [
                (name.clone(), "limpio.txt".to_owned()),
                ("limpio.txt".to_owned(), name.clone()),
            ];
            for (from, to) in cases {
                let hostile = display_name(from.as_bytes()).1 || display_name(to.as_bytes()).1;
                let (_, body) = ai_rename_plan_modal_text(
                    &dir(),
                    &[entry(&from, &to)],
                    0,
                    &crate::hints::DialogHints::default(),
                    &plan_ok(),
                );
                // PER LINE: the `\n` that separates the body's lines is a
                // legitimate format control, not painted content.
                assert!(
                    !body
                        .lines()
                        .any(|l| l.chars().any(norte_encoding::is_terminal_hazard)),
                    "corpus {}: a hazard survived rendering: {body:?}",
                    n.id
                );
                if hostile {
                    assert!(
                        body.contains(HOSTILE_BADGE),
                        "corpus {}: masked WITHOUT a badge: {body:?}",
                        n.id
                    );
                }
            }
        }
    }

    /// Audit MINOR-4 (`arrow_join_spoof` corpus): a `from` that IMITATES the
    /// arrow does not fabricate a fake pair — the `from` carries its
    /// numbered label out of band on ITS line and the REAL destination
    /// keeps its own with the arrow at the start.
    #[test]
    fn arrow_join_spoof_does_not_fabricate_a_pair() {
        let spoof = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "arrow_join_spoof")
            .expect("corpus fixture");
        let from = String::from_utf8_lossy(&spoof.bytes).into_owned();
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry(&from, "real.txt")],
            0,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let lines: Vec<&str> = body.lines().collect();
        // dir + status + from + to + hint = exactly 5 lines: the spoof does
        // not add one.
        assert_eq!(lines.len(), 5, "{body:?}");
        assert!(lines[2].contains("1."), "out-of-band label: {body:?}");
        assert!(
            lines[3].starts_with('→') && lines[3].contains("real.txt"),
            "the real destination keeps ITS line: {body:?}"
        );
    }

    /// Audit MINOR-6c: a hostile destination (corpus RLO) is masked and its
    /// line is flagged — the badge comes even before the arrow.
    #[test]
    fn hostile_destination_is_masked_and_flagged() {
        let rtl = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("corpus fixture");
        let to = String::from_utf8_lossy(&rtl.bytes).into_owned();
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("limpio.txt", &to)],
            0,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let to_line = body.lines().nth(3).expect("destination line");
        assert!(to_line.starts_with(HOSTILE_BADGE), "{body:?}");
        assert!(to_line.contains('\u{FFFD}'), "{body:?}");
        assert!(
            !to_line.chars().any(norte_encoding::is_terminal_hazard),
            "{body:?}"
        );
    }

    /// Audit MAJOR-3: with 7 pairs the window paints 5 from `offset` with
    /// ABSOLUTE numbering, the indicator says position/total and the
    /// modal's height matches the painted lines.
    #[test]
    fn long_plan_window_indicator_and_height() {
        let entries: Vec<AiRenameEntry> = (1..=7)
            .map(|i| entry(&format!("f{i}"), &format!("t{i}")))
            .collect();
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            0,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let lines: Vec<&str> = body.lines().collect();
        // dir + batch status + 5 pairs × 2 + indicator + hint = 14.
        assert_eq!(lines.len(), 14, "{body:?}");
        assert!(
            lines[2].contains("1.") && lines[2].contains("f1"),
            "{body:?}"
        );
        assert!(lines[12].contains("5/7"), "indicator: {body:?}");
        assert!(!body.contains("f6"), "the tail waits for scroll: {body:?}");
        // offset 2 = pairs 3..=7, absolute numbering, indicator at the cap.
        let (_, body2) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            2,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let lines2: Vec<&str> = body2.lines().collect();
        assert_eq!(lines2.len(), 14, "STABLE height across scroll: {body2:?}");
        assert!(
            lines2[2].contains("3.") && lines2[2].contains("f3"),
            "{body2:?}"
        );
        assert!(body2.contains("f7"), "{body2:?}");
        assert!(lines2[12].contains("7/7"), "{body2:?}");
        // A runaway offset is clamped at render time (belt-and-braces).
        let (_, body3) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            999,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        assert!(body3.contains("f7"), "{body3:?}");
        // Fourteen lines: the dir, ten pairs × ... — what is FIXED is the
        // body, because the height comes from it (`body_height`). It used
        // to assert the height (17 = 14 + 3) and that replicated the
        // formula instead of checking it.
        assert_eq!(body.lines().count(), 14, "{body:?}");
    }

    /// Audit MAJOR-3: the overflow indicator gives away a HIDDEN hostile
    /// pair (what is not visible never slips through "clean"), and stops
    /// flagging once scrolling brings it into view.
    #[test]
    fn indicator_flags_a_hidden_hostile_pair() {
        let mut entries: Vec<AiRenameEntry> = (1..=6)
            .map(|i| entry(&format!("f{i}"), &format!("t{i}")))
            .collect();
        entries[5] = entry("x\u{202e}y", "limpio.txt");
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            0,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let ind = body.lines().nth(12).expect("indicator");
        assert!(ind.starts_with(HOSTILE_BADGE), "{body:?}");
        // offset 1: the hostile one enters the window; the hidden one
        // (pair 1) is clean — the indicator no longer flags.
        let (_, body2) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            1,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let ind2 = body2.lines().nth(12).expect("indicator");
        assert!(!ind2.starts_with(HOSTILE_BADGE), "{body2:?}");
    }

    /// §17: a collision is VISIBLE with its verdict and its pair index, and
    /// the modal says the plan CANNOT be applied — a human cannot confirm
    /// a batch that is going to bounce without knowing why.
    #[test]
    fn a_collision_is_painted_and_the_plan_is_marked_unapplicable() {
        use norte_i18n::t;
        let plan = plan_con_colision(RenameCollisionKind::External, b"z.txt");
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a.txt", "z.txt")],
            0,
            &crate::hints::DialogHints::default(),
            &plan,
        );
        let lines: Vec<&str> = body.lines().collect();
        // dir + status + pair × 2 + cause + name + hint = 7. The cause and
        // the name go SEPARATE since #273.
        assert_eq!(lines.len(), 7, "{body:?}");
        assert_eq!(lines[1], t("modal-rename-batch-not-applicable"), "{body:?}");
        assert!(
            lines[4].contains(&t("modal-rename-batch-collision-external")),
            "the verdict is shown: {body:?}"
        );
        assert!(
            lines[5].contains("z.txt"),
            "and the offending name: {body:?}"
        );
        assert!(
            !lines[5].contains(&t("modal-rename-batch-collision-external")),
            "but the name does NOT carry the cause inside: {body:?}"
        );
        // `pair_index` 0 is painted 1-based, like the `from`'s label: the
        // guilty row is pointable-to.
        assert!(lines[4].contains("1."), "{body:?}");
        // The footer does NOT offer a mute key.
        assert_eq!(
            lines[6],
            t("modal-rename-batch-plan-hint-blocked"),
            "{body:?}"
        );
        assert!(!body.contains(&t("modal-ai-rename-plan-hint")), "{body:?}");
    }

    /// A verdict from a NEWER daemon degrades ONE line to a generic label,
    /// never the whole modal: the rest of the plan keeps reading fine and
    /// the batch stays marked as not applicable.
    #[test]
    fn an_unknown_verdict_degrades_one_line_not_the_modal() {
        use norte_i18n::t;
        let future: RenameCollisionKind =
            serde_json::from_str(r#""clase_del_futuro""#).expect("fallback");
        assert_eq!(future, RenameCollisionKind::Unknown);
        let plan = plan_con_colision(future, b"z.txt");
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a.txt", "z.txt")],
            0,
            &crate::hints::DialogHints::default(),
            &plan,
        );
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 7, "the modal stays whole: {body:?}");
        assert!(lines[2].contains("a.txt"), "the pairs are still visible");
        assert!(
            lines[4].contains(&t("modal-rename-batch-collision-unknown")),
            "{body:?}"
        );
        assert_eq!(lines[1], t("modal-rename-batch-not-applicable"), "{body:?}");
    }

    /// A temp step is planner MACHINERY: it says HOW MANY there are, never
    /// what they are called. A `.norte-rename-…` among the pairs would make
    /// the human believe norte is going to leave that name on disk.
    #[test]
    fn a_temp_step_is_counted_never_named() {
        use norte_i18n::t;
        let plan = listo(FsRenameBatchPlanResult {
            steps: vec![
                RenameStep {
                    from: seg(b"a"),
                    to: seg(b".norte-rename-0a1b2c3d-0"),
                    temp: true,
                },
                RenameStep {
                    from: seg(b"b"),
                    to: seg(b"a"),
                    temp: false,
                },
                RenameStep {
                    from: seg(b".norte-rename-0a1b2c3d-0"),
                    to: seg(b"b"),
                    temp: true,
                },
            ],
            collisions: vec![],
            executable: true,
            plan_hash: hash(),
        });
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a", "b"), entry("b", "a")],
            0,
            &crate::hints::DialogHints::default(),
            &plan,
        );
        assert!(
            !body.contains(".norte-rename-"),
            "a temp step is never painted as a proposal: {body:?}"
        );
        // Both HALVES of the cycle carry `temp`, and both are machinery.
        assert!(
            body.contains(&norte_i18n::ta("modal-rename-batch-temp", &[("n", "2")])),
            "{body:?}"
        );
        // Applicable: the detour is not a collision.
        assert!(
            body.contains(&t("modal-rename-batch-applicable")),
            "{body:?}"
        );
        assert!(body.contains(&t("modal-ai-rename-plan-hint")), "{body:?}");
    }

    /// The plan in flight (`None`): the modal opens with the pairs and says
    /// it is checking — without offering a confirm key that is mute.
    #[test]
    fn with_no_plan_yet_the_footer_does_not_offer_confirm() {
        use norte_i18n::t;
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a", "b")],
            0,
            &crate::hints::DialogHints::default(),
            &norte_frontend::BatchPlan::Pending,
        );
        assert!(body.contains(&t("modal-rename-batch-pending")), "{body:?}");
        assert!(!body.contains(&t("modal-ai-rename-plan-hint")), "{body:?}");
        // What is painted: dir + pair × 2 + status + hint. The height comes
        // from here.
        assert_eq!(body.lines().count(), 5, "{body:?}");
    }

    /// Corpus sweep over a collision's OFFENDING NAME: no hazard survives,
    /// the masking FLAGS the line, and the verdict — which is the
    /// actionable part — is never eaten by the name.
    #[test]
    fn corpus_sweep_on_the_collision_name() {
        use norte_i18n::t;
        let verdict = t("modal-rename-batch-collision-internal");
        for n in norte_testkit::corpus::hostile_names() {
            let plan = plan_con_colision(RenameCollisionKind::Internal, &n.bytes);
            let (_, body) = ai_rename_plan_modal_text(
                &dir(),
                &[entry("a", "b")],
                0,
                &crate::hints::DialogHints::default(),
                &plan,
            );
            assert!(
                !body
                    .lines()
                    .any(|l| l.chars().any(norte_encoding::is_terminal_hazard)),
                "corpus {}: a hazard survived rendering: {body:?}",
                n.id
            );
            // TWO lines per collision since #273: the cause and the name go
            // separate, because a name containing the whole cause used to
            // fabricate a list entry that does not exist. They are still
            // exactly two: a name cannot fabricate a third.
            assert_eq!(body.lines().count(), 7, "corpus {}: {body:?}", n.id);
            let causa = body.lines().nth(4).expect("cause line");
            let nombre = body.lines().nth(5).expect("name line");
            assert!(
                causa.contains(&verdict),
                "corpus {}: the verdict goes on ITS OWN line: {body:?}",
                n.id
            );
            assert!(
                !nombre.contains(&verdict),
                "corpus {}: the name does not carry the cause inside: {body:?}",
                n.id
            );
            if display_name(&n.bytes).1 {
                assert!(
                    nombre.trim_start().starts_with(HOSTILE_BADGE),
                    "corpus {}: masked WITHOUT a badge: {body:?}",
                    n.id
                );
            }
        }
    }

    /// A batch with MANY collisions does not overflow the modal: up to
    /// [`norte_frontend::RENAME_COLLISION_LIMIT`] are painted and the
    /// summary says how many are left out — and flags if any HIDDEN one is
    /// hostile (what is hidden does not slip through clean).
    #[test]
    fn many_collisions_are_summarized_and_a_hidden_hostile_one_is_flagged() {
        let mut collisions: Vec<RenameCollision> = (0..8)
            .map(|i| RenameCollision {
                pair_index: i,
                name: seg(format!("f{i}").as_bytes()),
                kind: RenameCollisionKind::Internal,
            })
            .collect();
        collisions[7].name = seg("x\u{202e}y".as_bytes());
        let total = collisions.len();
        let plan = listo(FsRenameBatchPlanResult {
            steps: vec![],
            collisions,
            executable: false,
            plan_hash: hash(),
        });
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a", "b")],
            0,
            &crate::hints::DialogHints::default(),
            &plan,
        );
        let lines: Vec<&str> = body.lines().collect();
        // dir + status + pair × 2 + 5 collisions × 2 (cause and name go
        // separate since #273) + summary + hint = 16.
        assert_eq!(lines.len(), 16, "{body:?}");
        let summary = lines[14];
        assert!(
            summary.contains(&norte_frontend::RENAME_COLLISION_LIMIT.to_string())
                && summary.contains(&total.to_string()),
            "the summary does not stay silent about how many are left out: {body:?}"
        );
        assert!(
            summary.starts_with(HOSTILE_BADGE),
            "a hidden hostile collision flags the summary: {body:?}"
        );
        assert!(!body.contains("f7"), "the tail is summarized: {body:?}");
    }
}

#[cfg(test)]
mod semantic_hits_modal_tests {
    use super::{HOSTILE_BADGE, semantic_hits_modal_text};
    use norte_proto::methods::SemanticHit;
    use norte_proto::{Segment, VPath};

    fn hit(path: VPath, score: f64) -> SemanticHit {
        SemanticHit { path, score }
    }

    fn hits(n: u16) -> Vec<SemanticHit> {
        (1..=n)
            .map(|i| {
                hit(
                    VPath::parse(&format!("mem:///d/f{i}")).expect("valid wire"),
                    1.0 - f64::from(i) / 100.0,
                )
            })
            .collect()
    }

    /// M4-IA-2 (canonical corpus, same mold as the AI plan's sweep): every
    /// hostile name as the last segment of a hit's path — no
    /// `is_terminal_hazard` char survives in the painted text, and when the
    /// masking alters the path the line is MARKED with the badge.
    #[test]
    fn corpus_sweep_no_hazard_survives_and_masking_flags_it() {
        for n in norte_testkit::corpus::hostile_names() {
            let path = VPath::parse("mem:///d")
                .expect("valid wire")
                .join(Segment::new(n.bytes.clone()).expect("corpus segment"));
            let hostile = norte_frontend::path_display(&path).1;
            let (_, body) = semantic_hits_modal_text(
                &[hit(path, 0.5)],
                0,
                0,
                &crate::hints::DialogHints::default(),
            );
            // PER LINE: the `\n` that separates the body's lines is a
            // legitimate format control, not painted content.
            assert!(
                !body
                    .lines()
                    .any(|l| l.chars().any(norte_encoding::is_terminal_hazard)),
                "corpus {}: a hazard survived rendering: {body:?}",
                n.id
            );
            if hostile {
                assert!(
                    body.contains(HOSTILE_BADGE),
                    "corpus {}: masked WITHOUT a badge: {body:?}",
                    n.id
                );
            }
        }
    }

    /// Encoding audit M4-IA-2 S1 (`score_spoof_inband` fixture): a name that
    /// IMITATES the score column (`report · 0.99.txt`: middle dot +
    /// decimals, all printable — NO badge would warn) never displaces the
    /// REAL score. Pinned in two shapes: the fixture as-is (fits whole, the
    /// genuine score stays the LAST field) and the fixture inflated to
    /// >120 chars (forces middle ellipsis: the path gets CLIPPED, flagged,
    /// but the score is still there — never the other way around).
    #[test]
    fn score_spoof_inband_never_displaces_the_real_score() {
        let fixture = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "score_spoof_inband")
            .expect("corpus fixture");
        let dir = VPath::parse("mem:///d").expect("valid wire");
        let decoy = String::from_utf8(fixture.bytes.clone()).expect("the fixture is UTF-8");

        let path = dir
            .clone()
            .join(Segment::new(fixture.bytes.clone()).expect("corpus segment"));
        let (_, body) = semantic_hits_modal_text(
            &[hit(path, 0.91)],
            0,
            0,
            &crate::hints::DialogHints::default(),
        );
        let line = body.lines().next().expect("the hit's line");
        assert!(
            line.contains(&decoy),
            "the decoy is painted as-is (it is a legitimate name): {line:?}"
        );
        assert!(
            line.trim_end().ends_with("0.91"),
            "the REAL score is the FINAL field: {line:?}"
        );

        // Inflated: the decoy at the end of a mile-long name. The clip eats
        // the PATH (middle ellipsis, flagged), never the score.
        let mut long = b"x".repeat(120);
        long.extend_from_slice(&fixture.bytes);
        let path = dir.join(Segment::new(long).expect("valid segment"));
        let (_, body) = semantic_hits_modal_text(
            &[hit(path, 0.91)],
            0,
            0,
            &crate::hints::DialogHints::default(),
        );
        let line = body.lines().next().expect("the hit's line");
        assert!(
            line.trim_end().ends_with("0.91"),
            "mile-long path: the REAL score is still the FINAL field: {line:?}"
        );
        assert!(
            line.contains('…'),
            "the path's clip is FLAGGED (spec §6): {line:?}"
        );
    }

    /// M4-IA-2: with 12 hits the window paints 10 from `offset` with
    /// ABSOLUTE numbering and a `>` marker on the cursor's row; the
    /// indicator says position/total, the score goes at the end of the line
    /// and the modal's height matches the painted lines.
    #[test]
    fn long_hits_window_cursor_indicator_and_height() {
        let hits = hits(12);
        let (_, body) =
            semantic_hits_modal_text(&hits, 0, 3, &crate::hints::DialogHints::default());
        let lines: Vec<&str> = body.lines().collect();
        // 10 hits + indicator + hint = 12.
        assert_eq!(lines.len(), 12, "{body:?}");
        assert!(
            lines[0].contains("1.") && lines[0].contains("f1"),
            "{body:?}"
        );
        assert!(
            lines[3].starts_with("> ") && lines[3].contains("4."),
            "marker on the cursor's row: {body:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| l.starts_with("> ")).count(),
            1,
            "a single cursor: {body:?}"
        );
        assert!(lines[0].contains("0.99"), "score at the end: {body:?}");
        assert!(lines[10].contains("10/12"), "indicator: {body:?}");
        assert!(!body.contains("f11"), "the tail waits for scroll: {body:?}");
        // The window follows the cursor: offset 2 = hits 3..=12, absolute
        // numbering, cursor at the visible bottom.
        let (_, body2) =
            semantic_hits_modal_text(&hits, 2, 11, &crate::hints::DialogHints::default());
        let lines2: Vec<&str> = body2.lines().collect();
        assert_eq!(lines2.len(), 12, "STABLE height across scroll: {body2:?}");
        assert!(
            lines2[0].contains("3.") && lines2[0].contains("f3"),
            "{body2:?}"
        );
        assert!(
            lines2[9].starts_with("> ") && lines2[9].contains("12."),
            "{body2:?}"
        );
        assert!(lines2[10].contains("12/12"), "{body2:?}");
        // A runaway offset is clamped at render time (belt-and-braces).
        let (_, body3) =
            semantic_hits_modal_text(&hits, 999, 0, &crate::hints::DialogHints::default());
        assert!(body3.contains("f12"), "{body3:?}");
        // Twelve body lines, which is where the height comes from.
        assert_eq!(body.lines().count(), 12, "{body:?}");
    }

    /// M4-IA-2: the overflow indicator gives away a HIDDEN hostile hit (what
    /// is not visible never slips through "clean"), and stops flagging once
    /// scrolling brings it into view.
    #[test]
    fn indicator_flags_a_hidden_hostile_hit() {
        let mut hits = hits(11);
        hits[10] = hit(
            VPath::parse("mem:///d")
                .expect("valid wire")
                .join(Segment::new(b"x\xe2\x80\xaey".to_vec()).expect("segment")),
            0.1,
        );
        let (_, body) =
            semantic_hits_modal_text(&hits, 0, 0, &crate::hints::DialogHints::default());
        let ind = body.lines().nth(10).expect("indicator");
        assert!(ind.starts_with(HOSTILE_BADGE), "{body:?}");
        // offset 1: the hostile one enters the window; the hidden one
        // (hit 1) is clean — the indicator no longer flags.
        let (_, body2) =
            semantic_hits_modal_text(&hits, 1, 10, &crate::hints::DialogHints::default());
        let ind2 = body2.lines().nth(10).expect("indicator");
        assert!(!ind2.starts_with(HOSTILE_BADGE), "{body2:?}");
    }
}

/// The agent approval modal: the list of paths and its HEIGHT (H3c
/// MINOR-5 review).
#[cfg(test)]
mod approval_modal_tests {
    use super::{HOSTILE_BADGE, approval_modal_text, body_height, plain_body};

    fn req(paths: Vec<String>) -> norte_proto::methods::PolicyApprovalRequired {
        norte_proto::methods::PolicyApprovalRequired {
            approval_id: 1,
            session: Some("s1".into()),
            op: "copy".into(),
            paths_total: paths.len() as u64,
            paths,
            ttl_ms: 60_000,
            detail: norte_proto::methods::ApprovalDetail::default(),
        }
    }

    fn rutas(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("mem:///proj/f{i}.txt")).collect()
    }

    /// The SERVER's clipping is also counted (0.36.0). A batch of renames
    /// gates thousands of paths and the daemon broadcasts only the first
    /// ones: if the modal painted `paths.len()` as if it were everything,
    /// the human would approve 32 innocent paths without knowing the
    /// decision covered eight thousand. That is not an informed approval,
    /// it is a deceived one.
    #[test]
    fn the_servers_clipping_is_told_to_the_human() {
        let mut r = req(rutas(3));
        r.paths_total = 8192;
        let (_, body) = approval_modal_text(&r, "PIE");
        let lines: Vec<&str> = body.lines().collect();
        // header + deadline + 3 paths + summary + footer.
        assert_eq!(lines.len(), 7, "{body:?}");
        assert!(
            lines[5].contains(&(8192 - 3).to_string()),
            "the summary counts what the DECISION covers and cannot be seen: {body:?}"
        );
        assert_eq!(
            lines[6], "PIE",
            "and the footer is still the last line: {body:?}"
        );
        // The height counts the summary line that just appeared. It is
        // derived from the body, so the assertion is about the body: there
        // used to be a hand-written `9` that replicated a formula, and the
        // two disagreed by one row of air.
        assert_eq!(body_height(&plain_body(&body)), 10);
    }

    /// `paths_total: 0` is an N-1 server that did not send it: what was
    /// received IS everything there was, and no summary that would lie the
    /// other way is invented.
    #[test]
    fn with_no_paths_total_no_clipping_is_invented() {
        let mut r = req(rutas(2));
        r.paths_total = 0;
        let (_, body) = approval_modal_text(&r, "PIE");
        assert_eq!(
            body.lines().count(),
            5,
            "header + deadline + 2 paths + footer: {body:?}"
        );
    }

    /// Review MINOR-5: the number of paths is the AGENT's choice, and the
    /// height could not grow with it without a cap.
    ///
    /// `centered` clips against the frame, so the extra lines simply were
    /// not painted — including the LAST one, which under H3c is the only
    /// explanation for why `y`/`n` do nothing. A `paths` of 400 entries
    /// erased the notice from the screen. Now it is windowed like
    /// `ConfirmDelete`: `MODAL_ITEM_LIMIT` paths plus a summary line.
    #[test]
    fn the_path_list_is_windowed_and_the_footer_always_fits() {
        let limit = norte_frontend::MODAL_ITEM_LIMIT;
        let total = limit + 7;
        let (_, body) = approval_modal_text(&req(rutas(total)), "PIE-DEL-MODAL");
        let lines: Vec<&str> = body.lines().collect();

        // header + deadline + LIMIT paths + summary + footer.
        assert_eq!(lines.len(), limit + 4, "{body:?}");
        assert!(lines[2].contains("f1.txt"), "{body:?}");
        assert!(
            lines[limit + 1].contains(&format!("f{limit}.txt")),
            "the window's last path: {body:?}"
        );
        assert!(
            !body.contains(&format!("f{}.txt", limit + 1)),
            "the tail is NOT painted: {body:?}"
        );
        assert!(
            lines[limit + 2].contains(&(total - limit).to_string()),
            "the summary says how many are left out: {body:?}"
        );
        assert_eq!(
            lines[limit + 3],
            "PIE-DEL-MODAL",
            "and the footer is the LAST line, always present: {body:?}"
        );

        // **The agent does not choose the height.** That is the only thing
        // this block had to say, and now it is said about the body: the
        // height is derived from it, so it is enough for the list to be
        // BOUNDED. 17 paths and 400 paint the same.
        let alto_de = |n: usize| {
            let (_, cuerpo) = approval_modal_text(&req(rutas(n)), "PIE-DEL-MODAL");
            body_height(&plain_body(&cuerpo))
        };
        assert_eq!(
            alto_de(total),
            u16::try_from(limit + 4).expect("fits") + 3,
            "bounded body + frame"
        );
        assert_eq!(
            alto_de(400),
            alto_de(total),
            "the agent does not choose the height: 17 paths and 400 measure the same"
        );
        assert!(
            alto_de(1) < alto_de(total),
            "and a batch that fits takes less, not the same"
        );
    }

    /// A batch that FITS is painted whole and with no summary line:
    /// windowing cannot invent an "and N more" that does not exist.
    #[test]
    fn a_batch_that_fits_carries_no_summary() {
        let (_, body) = approval_modal_text(&req(rutas(2)), "PIE");
        let lines: Vec<&str> = body.lines().collect();
        // Header + DEADLINE + 2 paths + footer. The deadline always appears
        // since this modal says how much time is left, like the window's.
        assert_eq!(lines.len(), 5, "{body:?}");
        assert!(body.contains("f2.txt"), "{body:?}");
        assert_eq!(lines[4], "PIE", "{body:?}");
    }

    /// And what is HIDDEN does not slip through clean (same doctrine as the
    /// AI plan and the semantic hits): if any path outside the window is
    /// hostile, the summary line is FLAGGED — the human decides knowing
    /// there is something odd they are not seeing.
    #[test]
    fn the_summary_flags_a_hidden_hostile_path() {
        let limit = norte_frontend::MODAL_ITEM_LIMIT;
        let mut paths = rutas(limit + 2);
        paths[limit + 1] = "mem:///proj/x\u{202e}y.txt".to_owned();
        let (_, body) = approval_modal_text(&req(paths), "PIE");
        let summary = body.lines().nth(limit + 2).expect("summary");
        assert!(
            summary.starts_with(HOSTILE_BADGE),
            "the summary gives away the hidden hostile one: {body:?}"
        );

        // With ALL the hidden ones clean, it does not flag (or the badge
        // would say nothing).
        let (_, clean) = approval_modal_text(&req(rutas(limit + 2)), "PIE");
        let clean_summary = clean.lines().nth(limit + 2).expect("summary");
        assert!(!clean_summary.starts_with(HOSTILE_BADGE), "{clean:?}");
    }

    /// Phase 8: the organize tree is painted with the COUNT up front and
    /// with a mark per line, and a folder that already existed is not
    /// painted as new. The first is what gets read to decide; the second is
    /// the comfortable lie — a plan more spectacular than it is — and the
    /// `Strong` role alone does not prevent it on a monochrome terminal.
    #[test]
    fn the_organize_tree_carries_a_count_and_a_mark_per_line() {
        use super::{LineKind, ORGANIZE_EXISTING, ORGANIZE_FILE, ORGANIZE_NEW};
        use crate::app::Modal;
        use norte_proto::VPath;
        use norte_proto::methods::{OrganizeMove, PlanHash};

        let dir = VPath::parse("mem:///descargas").unwrap();
        let moves = vec![
            OrganizeMove {
                current: "a.pdf".to_owned(),
                proposed_rel: "facturas/a.pdf".to_owned(),
            },
            OrganizeMove {
                current: "b.txt".to_owned(),
                proposed_rel: "nueva/b.txt".to_owned(),
            },
        ];
        let lines = norte_frontend::organize::tree_lines(&moves, &["facturas".to_owned()]);
        let modal = Modal::OrganizePlan {
            dir,
            moves,
            lines,
            plan_hash: PlanHash::parse(&"ab".repeat(32)).expect("hex"),
            offset: 0,
            seen: 0,
        };
        let (_, body) =
            super::modal_title_body(&modal, None, &crate::hints::DialogHints::default());
        // [0] the dir, [1] the count, and from there on the tree.
        assert!(
            body[1].text.contains('1') && body[1].kind == LineKind::Strong,
            "the count goes ahead of the tree and stands out: {:?}",
            body[1]
        );
        let facturas = body
            .iter()
            .find(|l| l.text.contains("facturas"))
            .expect("is there");
        assert_eq!(
            facturas.kind,
            LineKind::Dim,
            "the folder that ALREADY existed is not painted as new: {facturas:?}"
        );
        assert!(facturas.text.starts_with(ORGANIZE_EXISTING), "{facturas:?}");
        let nueva = body
            .iter()
            .find(|l| l.text.contains("nueva"))
            .expect("is there");
        assert_eq!(nueva.kind, LineKind::Strong, "{nueva:?}");
        assert!(nueva.text.starts_with(ORGANIZE_NEW), "{nueva:?}");
        // And a file is indented under its folder, with ITS mark.
        let fichero = body
            .iter()
            .find(|l| l.text.contains("a.pdf"))
            .expect("is there");
        assert!(
            fichero.text.starts_with(&format!("  {ORGANIZE_FILE}")),
            "{fichero:?}"
        );
    }
}
