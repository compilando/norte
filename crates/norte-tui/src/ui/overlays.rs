//! The overlays painted over everything: which-key, the command palette,
//! settings, the shortcuts editor and the extension manager.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::{HOSTILE_BADGE, centered, clear_themed};
use crate::app::display_name;
use crate::theme::TuiTheme;
use norte_frontend::display::cells;
use norte_frontend::middle_ellipsis;
use norte_frontend::settings::{Focus, Section};
use norte_i18n::{t, ta};

/// Extension catalogue overlay (M4-P3): the plugin list GROUPED by category
/// (a header on every group change, since they arrive sorted) plus the
/// directories that failed to load. CRITICAL: `name` and `publisher` are
/// FREE third-party text and this is a security-decision surface (approve)
/// — they go through [`display_name`] (the same masking of
/// controls/bidi/invisibles as the panes) before painting. The id is already
/// charset-validated in the core; name/publisher are not. `hint` (H1 T3,
/// #24) is the GENERATED hint (`app.dialog_hints.extensions`).
pub(crate) fn draw_extensions(
    frame: &mut Frame<'_>,
    mgr: &crate::app::ExtensionManager,
    theme: &TuiTheme,
    hint: &str,
) {
    let area = extensions_area(frame.area(), hint);
    clear_themed(frame, area, theme);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("ext-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(format!(" {hint} ")))
        .border_style(theme.role(Role::ModalBorder));
    let inner_area = block.inner(area);
    frame.render_widget(block, area);
    // Two columns, like the window (ADR 0104): the list on the left and the
    // chosen one's CARD on the right — status, description, capabilities,
    // commands and their settings. With fewer than [`EXTENSIONS_WIDE_MIN`]
    // useful cells, two legible columns do not fit and the usual list is
    // painted, with the description under each row and settings in their
    // own box.
    let Some((list_area, card_area)) = extensions_columns(mgr, inner_area) else {
        let inner = usize::from(inner_area.width.saturating_sub(2));
        let (lines, _) = extensions_list_lines(mgr, theme, inner, true);
        frame.render_widget(Paragraph::new(lines), inner_area);
        return;
    };
    let (list, _) = extensions_list_lines(mgr, theme, usize::from(list_area.width), false);
    frame.render_widget(Paragraph::new(list), list_area);
    let border = Block::default()
        .borders(Borders::LEFT)
        .border_style(theme.role(Role::BorderUnfocused));
    frame.render_widget(
        border,
        Rect {
            x: card_area.x.saturating_sub(1),
            width: 1,
            ..card_area
        },
    );
    // The chosen one may be one that did NOT load: its card says where and
    // why, and its only button is uninstall.
    let pane = match mgr.plugins.get(mgr.cursor) {
        Some(p) => Some((
            extension_buttons(p),
            extension_pane_lines(p, mgr.config.as_ref(), theme),
        )),
        None => mgr.selected_broken().map(|e| {
            (
                broken_buttons(e, &mgr.plugins),
                broken_pane_lines(e, &mgr.plugins, theme),
            )
        }),
    };
    if let Some((buttons, card)) = pane {
        // The BUTTONS on the card's first row, as in the window: they are
        // what the reader looks for, and on a fixed row — not inside the
        // paragraph, whose line wrap would move each one depending on how
        // long the name is — so the mouse finds them where they were
        // painted. Below, a blank row and the card.
        let mut spans = Vec::new();
        let mut x = 0usize;
        for (n, (label, _)) in buttons.iter().enumerate() {
            let w = UnicodeWidthStr::width(label.as_str());
            if x + w > usize::from(card_area.width) {
                break;
            }
            // The one with `tab`'s focus is painted like a list cursor; the
            // rest, like the buttons they are. Without this difference
            // `tab` would move something invisible, which is how it was
            // before the ring existed.
            let role = if mgr.foco == crate::app::ExtFoco::Boton(n) {
                Role::Selection
            } else {
                Role::Button
            };
            spans.push(Span::styled(label.clone(), theme.role(role)));
            spans.push(Span::raw(" "));
            x += w + 1;
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect {
                height: 1.min(card_area.height),
                ..card_area
            },
        );
        let body = Rect {
            y: card_area.y.saturating_add(BUTTON_ROWS),
            height: card_area.height.saturating_sub(BUTTON_ROWS),
            ..card_area
        };
        frame.render_widget(
            Paragraph::new(card).wrap(ratatui::widgets::Wrap { trim: false }),
            body,
        );
    }
}

/// Rows the button row and its blank line take away from the card.
const BUTTON_ROWS: u16 = 2;

/// Useful cells above which the manager paints the card beside the list.
/// Below it, the usual list.
pub(crate) const EXTENSIONS_WIDE_MIN: u16 = 64;

/// The manager's box in a `frame_area` frame, with `hint` in the footer.
///
/// MAJOR-1(c) H1 close: width by CONTENT (same as before, clamp(24, 80)) can
/// fall short for the GENERATED footer — same sizing criterion as
/// [`draw_nav_popup`] (measure the footer in CELLS, `Line::width`, and grow
/// if needed), capped at the frame's width. With a card (ADR 0104, parity
/// with the window) the cap rises to 120: two columns in 80 are two narrow
/// columns.
///
/// A function and not a calculation inside the painter because the mouse
/// needs it: measuring on one side and painting on another is how a click
/// ends up on the row next door.
fn extensions_area(frame_area: Rect, hint: &str) -> Rect {
    let footer_w = Line::raw(format!(" {hint} ")).width();
    let min_width = u16::try_from(footer_w.saturating_add(4)).unwrap_or(u16::MAX);
    let width = frame_area
        .width
        .saturating_sub(6)
        .clamp(24, 120)
        .max(min_width)
        .min(frame_area.width);
    centered(
        frame_area,
        width,
        frame_area.height.saturating_sub(4).max(6),
    )
}

/// `(list, card)` inside `inner_area`, or `None` when two columns do not fit
/// and the manager paints the usual list. The card already comes without
/// its left border's column.
fn extensions_columns(
    mgr: &crate::app::ExtensionManager,
    inner_area: Rect,
) -> Option<(Rect, Rect)> {
    if inner_area.width < EXTENSIONS_WIDE_MIN || mgr.plugins.is_empty() {
        return None;
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Min(1)])
        .split(inner_area);
    let card = Block::default().borders(Borders::LEFT).inner(cols[1]);
    Some((cols[0], card))
}

/// The card's buttons for `p`: `(label, command)`, in painting order. The
/// same verbs and the same labels as the window's buttons (`ext-*`), and
/// each one fires THE SAME command as its key: a button that did something
/// other than its key would be two managers.
fn extension_buttons(p: &norte_proto::methods::PluginInfo) -> Vec<(String, &'static str)> {
    let mut out = vec![
        (
            format!(
                "[{}]",
                t(if p.enabled {
                    "ext-disable"
                } else {
                    "ext-enable"
                })
            ),
            "dialog.toggle-enabled",
        ),
        (
            format!(
                "[{}]",
                t(if p.approved {
                    "ext-revoke"
                } else {
                    "ext-approve"
                })
            ),
            "dialog.approve",
        ),
        (format!("[{}]", t("ext-settings")), "dialog.confirm"),
        (format!("[{}]", t("ext-uninstall")), "dialog.remove"),
    ];
    if p.has_help {
        out.push((format!("[{}]", t("ext-help")), "app.help"));
    }
    out
}

/// The buttons of a card for an extension that did NOT load: uninstall, if
/// its directory is named like an id, and nothing else — there are no
/// capabilities to approve nor anything to turn on. The same command as its
/// key.
fn broken_buttons(
    e: &norte_proto::methods::PluginLoadError,
    loaded: &[norte_proto::methods::PluginInfo],
) -> Vec<(String, &'static str)> {
    if norte_frontend::broken_plugin::uninstallable_id(e, loaded).is_some() {
        vec![(format!("[{}]", t("ext-uninstall")), "dialog.remove")]
    } else {
        Vec::new()
    }
}

/// The card of an extension that did NOT load: where and why, masked, and —
/// if it cannot be uninstalled from here — why not.
fn broken_pane_lines(
    e: &norte_proto::methods::PluginLoadError,
    loaded: &[norte_proto::methods::PluginInfo],
    theme: &TuiTheme,
) -> Vec<Line<'static>> {
    let (dir, _) = display_name(e.dir_bytes.as_deref().unwrap_or(e.dir.as_bytes()));
    let (reason, _) = display_name(e.reason.as_bytes());
    let mut lines = vec![
        Line::styled(format!("{HOSTILE_BADGE} {dir}"), theme.role(Role::Title)),
        Line::styled(t("ext-errors-title"), theme.role(Role::Error)),
        Line::raw(""),
        Line::raw(reason),
    ];
    if norte_frontend::broken_plugin::uninstallable_id(e, loaded).is_none() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            t("ext-broken-not-id"),
            theme.role(Role::Warning),
        ));
    }
    lines
}

/// What is under a clickable cell of the extension manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionHit {
    /// The list's plugin row `index`.
    Row(usize),
    /// A card button: the `dialog.*`/`app.help` command it fires, the same
    /// as its key.
    Button(&'static str),
}

/// A clickable cell of the extension manager, in the painted frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtensionZone {
    /// Screen row.
    pub row: u16,
    /// First column, inclusive.
    pub x0: u16,
    /// Last column, inclusive.
    pub x1: u16,
    /// What is there.
    pub hit: ExtensionHit,
}

/// The extension manager's clickable zones in `area`'s frame, or nothing if
/// it is not open — or if what shows is the narrow settings box, which has
/// no mouse.
///
/// Shares the box, the column layout and the list lines with the manager's
/// painter, for the same reason as [`super::places_zones`]: the TUI's
/// manager was born deaf to the mouse — a click on a row or where the
/// window has its buttons did nothing — and the way to keep that from
/// happening again is for what is clickable to come from what is painted.
#[must_use]
pub fn extension_zones(app: &crate::app::App, area: Rect) -> Vec<ExtensionZone> {
    let Some(mgr) = &app.extensions else {
        return Vec::new();
    };
    let Some(hint) = super::extensions_footer(app, mgr, area.width) else {
        return Vec::new();
    };
    let outer = extensions_area(area, hint);
    let inner_area = Block::default().borders(Borders::ALL).inner(outer);
    let mut zones = Vec::new();
    let (list_area, card, width, with_description) = match extensions_columns(mgr, inner_area) {
        Some((list, card)) => (list, Some(card), usize::from(list.width), false),
        None => (
            inner_area,
            None,
            usize::from(inner_area.width.saturating_sub(2)),
            true,
        ),
    };
    let (_, rows) = extensions_list_lines(mgr, &app.theme, width, with_description);
    for (i, index) in rows.iter().enumerate() {
        let Some(index) = index else { continue };
        let Ok(offset) = u16::try_from(i) else { break };
        if offset >= list_area.height {
            break;
        }
        zones.push(ExtensionZone {
            row: list_area.y.saturating_add(offset),
            x0: list_area.x,
            x1: list_area
                .x
                .saturating_add(list_area.width)
                .saturating_sub(1),
            hit: ExtensionHit::Row(*index),
        });
    }
    let buttons = match mgr.plugins.get(mgr.cursor) {
        Some(p) => Some(extension_buttons(p)),
        None => mgr
            .selected_broken()
            .map(|e| broken_buttons(e, &mgr.plugins)),
    };
    if let Some(card) = card
        && card.height > 0
        && let Some(buttons) = buttons
    {
        let mut x = usize::from(card.x);
        let ceiling = usize::from(card.x) + usize::from(card.width);
        for (label, cmd) in buttons {
            let w = UnicodeWidthStr::width(label.as_str());
            if x + w > ceiling {
                break;
            }
            let (Ok(x0), Ok(x1)) = (u16::try_from(x), u16::try_from(x + w - 1)) else {
                break;
            };
            zones.push(ExtensionZone {
                row: card.y,
                x0,
                x1,
                hit: ExtensionHit::Button(cmd),
            });
            x += w + 1;
        }
    }
    zones
}

/// The manager's LIST lines: category headers, one row per plugin and the
/// directories that did not load at the end. `with_description` puts the
/// description under each row — the narrow list, with no card — or leaves
/// it for the card.
///
/// Also returns, per line, the index of the plugin whose row it is — `None`
/// for headers, descriptions and errors: it is what the mouse needs to know
/// which row it clicked, and it comes from the SAME list that is painted.
fn extensions_list_lines<'a>(
    mgr: &'a crate::app::ExtensionManager,
    theme: &TuiTheme,
    inner: usize,
    with_description: bool,
) -> (Vec<Line<'a>>, Vec<Option<usize>>) {
    let mut lines: Vec<Line<'_>> = Vec::new();
    let mut rows: Vec<Option<usize>> = Vec::new();
    if mgr.plugins.is_empty() && mgr.errors.is_empty() {
        lines.push(Line::raw(t("ext-empty")));
        rows.push(None);
        return (lines, rows);
    }
    // With focus on a card button, the list's cursor dims: two cursors
    // equally bright do not say which one receives the keys, which is what
    // `SelectionUnfocused` exists for.
    let cursor_role = if mgr.foco == crate::app::ExtFoco::Lista {
        Role::Selection
    } else {
        Role::SelectionUnfocused
    };
    let mut last_cat: Option<&str> = None;
    for (i, p) in mgr.plugins.iter().enumerate() {
        if last_cat != Some(p.category.as_str()) {
            last_cat = Some(p.category.as_str());
            let (cat, _) = display_name(p.category.as_bytes());
            lines.push(Line::styled(cat, theme.role(Role::Title)));
            rows.push(None);
        }
        if with_description {
            lines.push(plugin_line(
                p,
                (i == mgr.cursor).then_some(cursor_role),
                theme,
            ));
            rows.push(Some(i));
            if let Some(desc_line) = plugin_description_line(p, theme, inner) {
                lines.push(desc_line);
                rows.push(None);
            }
        } else {
            lines.push(plugin_row_compact(
                p,
                (i == mgr.cursor).then_some(cursor_role),
                theme,
                inner,
            ));
            rows.push(Some(i));
        }
    }
    for (j, e) in mgr.errors.iter().enumerate() {
        // The BYTES if the peer sends them (#265): the `dir` string comes
        // from a `to_string_lossy` in the core, so a directory named
        // `caf\xff` would arrive already converted through it. The badge
        // below goes ALWAYS — a load-error row is, by definition, something
        // that could not be read cleanly — so what changes here is the
        // name, not the mark.
        let (dir, _) = display_name(e.dir_bytes.as_deref().unwrap_or(e.dir.as_bytes()));
        let (reason, _) = display_name(e.reason.as_bytes());
        // One more row, behind the plugins: it is pointed at and clicked,
        // and its only verb is uninstall.
        let index = mgr.plugins.len() + j;
        let selected = index == mgr.cursor;
        let cursor = if selected { ">" } else { " " };
        let mut line = Line::styled(
            format!("{cursor}{HOSTILE_BADGE} {dir}: {reason}"),
            theme.role(Role::Error),
        );
        if selected {
            line = line.style(theme.role(Role::Selection));
        }
        lines.push(line);
        rows.push(Some(index));
    }
    (lines, rows)
}

/// A COMPACT row of the list-with-card: `> name v1.0 ✓` or `⚠`. Capabilities
/// do not go here: they go in the card, where they are read in full. Cut to
/// the column's width, which is half the box.
fn plugin_row_compact<'a>(
    p: &norte_proto::methods::PluginInfo,
    selected: Option<Role>,
    theme: &TuiTheme,
    inner: usize,
) -> Line<'a> {
    let (name, _) = display_name(p.name.as_bytes());
    let (version, _) = display_name(p.version.as_bytes());
    let cursor = if selected.is_some() { ">" } else { " " };
    // Unapproved is SAID in the row, not only in the card: it is what has to
    // be looked at, and the card only talks about the chosen one.
    let warning = if p.approved {
        0
    } else {
        Line::raw(format!("⚠ {}", t("ext-unapproved"))).width() + 1
    };
    let text = middle_ellipsis(
        &format!("{name} v{version}"),
        inner.saturating_sub(4 + warning),
    );
    let mut spans = vec![Span::raw(format!("{cursor} {text} "))];
    if !p.approved {
        spans.push(Span::styled(
            format!("⚠ {}", t("ext-unapproved")),
            theme.role(Role::Warning),
        ));
    } else if p.enabled {
        spans.push(Span::styled("✓", theme.role(Role::Info)));
    }
    let mut line = Line::from(spans);
    if let Some(role) = selected {
        line = line.style(theme.role(role));
    }
    line
}

/// An extension's CARD (parity with the window, ADR 0104): who it is, its
/// status, what it does, what it asks for, what it contributes, and — if
/// open — the table of its settings with their cursor. Everything the
/// plugin writes goes through [`display_name`], as in the list.
fn extension_pane_lines(
    p: &norte_proto::methods::PluginInfo,
    config: Option<&crate::app::PluginConfigPanel>,
    theme: &TuiTheme,
) -> Vec<Line<'static>> {
    let (name, _) = display_name(p.name.as_bytes());
    let (version, _) = display_name(p.version.as_bytes());
    let (publisher, _) = display_name(p.publisher.as_bytes());
    let (category, _) = display_name(p.category.as_bytes());
    let dim = theme.role(Role::BorderUnfocused);
    let mut lines: Vec<Line<'static>> = vec![Line::styled(name, theme.role(Role::Title))];
    let mut meta = vec![format!("v{version}")];
    if !publisher.is_empty() {
        meta.push(publisher);
    }
    meta.push(category);
    lines.push(Line::styled(meta.join(" · "), dim));
    // Status is TWO facts, and both are said: approved-and-off is not the
    // same as unapproved.
    lines.push(if !p.approved {
        Line::styled(
            format!("⚠ {}", t("ext-unapproved")),
            theme.role(Role::Warning),
        )
    } else if p.enabled {
        Line::styled(format!("✓ {}", t("ext-state-on")), theme.role(Role::Info))
    } else {
        Line::styled(t("ext-state-off"), dim)
    });
    if let Some(raw) = p.description.as_deref() {
        let clamped: String = raw
            .chars()
            .take(crate::app::PLUGIN_DESCRIPTION_WIRE_CAP)
            .collect();
        let (masked, _) = display_name(clamped.as_bytes());
        if !masked.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::raw(masked));
        }
    }
    if !p.capabilities.is_empty() {
        lines.push(Line::raw(""));
        // Each capability is THIRD-PARTY text and goes in its own span,
        // between brackets, so one cannot pretend to be two.
        let mut spans = Vec::new();
        for c in &p.capabilities {
            let (cap, _) = display_name(c.as_bytes());
            spans.push(Span::styled(format!("[{cap}]"), theme.role(Role::Warning)));
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
    }
    let mut counts = Vec::new();
    if !p.commands.is_empty() {
        counts.push(format!("{} {}", p.commands.len(), t("ext-counts-commands")));
    }
    if !p.columns.is_empty() {
        counts.push(format!("{} {}", p.columns.len(), t("ext-counts-columns")));
    }
    if p.has_help {
        counts.push(t("ext-help"));
    }
    if !counts.is_empty() {
        lines.push(Line::styled(counts.join(" · "), dim));
    }
    lines.push(Line::raw(""));
    match config {
        Some(panel) if panel.plugin_id == p.id => {
            lines.push(Line::styled(t("ext-config-title"), theme.role(Role::Title)));
            lines.extend(plugin_config_lines(panel, theme));
        }
        _ => lines.push(Line::styled(t("ext-detail-hint"), dim)),
    }
    if !p.commands.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            t("ext-commands-title"),
            theme.role(Role::Title),
        ));
        for c in &p.commands {
            let (title, _) = display_name(c.title.as_bytes());
            lines.push(Line::raw(format!("  · {title}")));
        }
    }
    lines
}

/// The lines of a plugin's `[config]` table: one per key, the chosen one
/// highlighted, and under it the buffer being typed or its description.
/// Painted by the card (with a width) and by its own box (without one): ONE
/// definition.
fn plugin_config_lines(
    panel: &crate::app::PluginConfigPanel,
    theme: &TuiTheme,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let rows = panel.state.rows();
    if rows.is_empty() {
        lines.push(Line::raw(t("ext-config-none")));
        return lines;
    }
    for (i, row) in rows.iter().enumerate() {
        let selected = i == panel.state.cursor();
        let cursor = if selected { ">" } else { " " };
        let mut line = Line::raw(format!("{cursor} {}: {}", row.key, row.display.value));
        if selected {
            line = line.style(theme.role(Role::Selection));
        }
        lines.push(line);
        if selected && panel.state.is_editing() {
            let buf = panel.state.edit_buffer().unwrap_or_default();
            lines.push(Line::styled(
                format!("   {buf}_"),
                theme.role(Role::BorderUnfocused),
            ));
        } else if !row.description.is_empty() {
            lines.push(Line::styled(
                format!("   {}", row.description),
                theme.role(Role::BorderUnfocused),
            ));
        }
    }
    lines
}

/// ONE plugin's `[config]` panel (G3c, drill-down from [`draw_extensions`]):
/// one `<key>: <value>` line per
/// [`norte_frontend::plugin_config::ConfigKeyRow`], the selected one
/// highlighted; if it is being edited (`state.is_editing()`), the RAW
/// buffer is painted under the row with an `_` cursor (same visual idiom as
/// a name-input popup). `key`/`kind`/`value` are charset-safe or norte's own
/// vocabulary (never free plugin text — see the rustdoc of
/// [`norte_frontend::plugin_config::ConfigKeyRow`]); `description` arrives
/// ALREADY masked (`sanitize_config_keys`), painted as a dimmed second line
/// just like [`plugin_description_line`].
pub(crate) fn draw_plugin_config_panel(
    frame: &mut Frame<'_>,
    panel: &crate::app::PluginConfigPanel,
    theme: &TuiTheme,
    hint: &str,
) {
    let footer_w = Line::raw(format!(" {hint} ")).width();
    let min_width = u16::try_from(footer_w.saturating_add(4)).unwrap_or(u16::MAX);
    let width = frame
        .area()
        .width
        .saturating_sub(6)
        .clamp(24, 80)
        .max(min_width)
        .min(frame.area().width);
    let area = centered(
        frame.area(),
        width,
        frame.area().height.saturating_sub(4).max(6),
    );
    clear_themed(frame, area, theme);
    let lines = plugin_config_lines(panel, theme);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", panel.plugin_name))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(format!(" {hint} ")))
        .border_style(theme.role(Role::ModalBorder));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// A plugin line: `<name> v<version> [<badges>] <state>`. `name` and
/// `publisher` are masked ([`display_name`]); badges = joined capabilities
/// (or `-` if empty); state = `✓` if enabled and a `⚠` warning (Warning
/// role) if NOT approved. The selected line is highlighted like the theme
/// picker's.
pub(crate) fn plugin_line<'a>(
    p: &'a norte_proto::methods::PluginInfo,
    selected: Option<Role>,
    theme: &TuiTheme,
) -> Line<'a> {
    let (name, _) = display_name(p.name.as_bytes());
    let (version, _) = display_name(p.version.as_bytes());
    let badges = if p.capabilities.is_empty() {
        "-".to_owned()
    } else {
        p.capabilities.join(" ")
    };
    let cursor = if selected.is_some() { ">" } else { " " };
    let mut spans = vec![Span::raw(format!("{cursor} {name} v{version} [{badges}] "))];
    if p.enabled {
        spans.push(Span::styled("✓", theme.role(Role::Info)));
        spans.push(Span::raw(" "));
    }
    if !p.approved {
        spans.push(Span::styled(
            format!("⚠ {}", t("ext-unapproved")),
            theme.role(Role::Warning),
        ));
    }
    let mut line = Line::from(spans);
    if let Some(role) = selected {
        line = line.style(theme.role(role));
    }
    line
}

/// Second line UNDER each plugin with its `description` (P1), if it declares
/// one — `None` if the plugin has none. The normal path (`main::dispatch`,
/// `app.extensions` arm) already arrives with `description` clamped+masked
/// by `app::clamp_plugin_descriptions` (P1 encoding audit F1: ONCE per
/// plugin at ingest, not per frame) — but this draw does NOT blindly trust
/// that: it re-clamps+masks here too, self-contained like `plugin_line` with
/// `name`/`publisher` (and like `palette::plugin_rows`). A raw control/bidi
/// character that reached `ratatui` without going through [`display_name`]
/// is not painted as `�` — a control/override char is INVISIBLE in the
/// cell, so it would vanish silently (exactly what masking exists to
/// avoid); blindly trusting the caller would turn "marked" into "silent" for
/// any path that builds `ExtensionManager` without going through ingest
/// (tests, a future caller). Over a string ALREADY bounded (the normal
/// case) this is cheap and idempotent. MIDDLE ellipsis
/// ([`middle_ellipsis`]) at the popup's useful width so it does not
/// overflow the box. No hostile badge (the badge is for load-failure
/// diagnostics, [`HOSTILE_BADGE`], not for third-party cosmetics — same
/// criterion as `plugin_line`). Dimmed style (`Role::BorderUnfocused`,
/// "present but not active" — same criterion that role documents): it is
/// context, not the row's main data.
#[must_use]
pub fn plugin_description_line(
    p: &norte_proto::methods::PluginInfo,
    theme: &TuiTheme,
    inner: usize,
) -> Option<Line<'static>> {
    let raw = p.description.as_deref()?;
    let clamped: String = raw
        .chars()
        .take(crate::app::PLUGIN_DESCRIPTION_WIRE_CAP)
        .collect();
    let (masked, _) = display_name(clamped.as_bytes());
    let text = format!("   {}", middle_ellipsis(&masked, inner.saturating_sub(3)));
    Some(Line::styled(text, theme.role(Role::BorderUnfocused)))
}

/// Theme picker popup: list of presets with the current one highlighted (ADR
/// 0020). The live preview is done by the event loop; here it is only
/// painted. `hint` (H1 T3, #24) is the GENERATED hint
/// (`app.dialog_hints.picker`). MAJOR-1(c) H1 close: 34 columns was a FIXED
/// width that did not grow with the generated hint (it got cut on narrow
/// terminals) — same sizing criterion as `draw_nav_popup`/`draw_extensions`,
/// footer in CELLS (`Line::width`), floor 34 (the preset name list already
/// fit), capped at the frame's width.
/// The which-key panel (K3a): while a chord sequence is PENDING, what can
/// follow it — every continuation, the unavailable ones included and dimmed,
/// with the reason they do nothing.
///
/// It appears with the keystroke that leaves the prefix pending and vanishes
/// with the one that ends it. No delay, ever: ADR 0006's resolution is
/// timing-free, and a panel on a 400 ms timer would make the same keystrokes
/// show different things depending on how fast they were typed.
///
/// Anchored at the bottom left, just above the status bar, where the pending
/// segment it explains is already painted — the panes stay readable above it.
/// It takes NO keys: the resolver keeps the keyboard, so the reader carries on
/// typing the sequence and watches the panel narrow.
///
/// Every string it paints is masked at the source: chords come through
/// `paint_chord` (a project layer can bind any lone codepoint) and the rest is
/// Fluent text or a catalogue command name.
pub fn draw_which_key(
    frame: &mut Frame<'_>,
    wk: &norte_frontend::whichkey::WhichKeyRows,
    theme: &TuiTheme,
) {
    use norte_frontend::keymap::Availability;

    if wk.is_empty() {
        return;
    }
    let base = frame.area();
    // The status bar owns the last line and the panel never paints over it:
    // that segment is what survives when there is no room for the box, which
    // is exactly the case below. FOUR rows above the bar and not three,
    // because with three the only line inside the borders would be the
    // "… 0/12" counter — three of the reader's rows spent saying that there
    // was no room to say anything. The floor is "at least one real key".
    let outside = base.height.saturating_sub(1);
    if outside < 4 {
        return;
    }
    let cap = usize::from(outside.saturating_sub(2)); // lines inside the borders
    let total = wk.rows.len();
    // A dropped row is a key the panel does not mention, so the count of what
    // was dropped COSTS a line of its own: taking `cap` rows and then adding
    // the count on top is how the count itself gets clipped, and a box that
    // just ends implies the list ended with it.
    let (shown, truncated) = if total <= cap {
        (total, false)
    } else {
        (cap.saturating_sub(1), true)
    };
    let body = shown + usize::from(truncated);
    let chord_w = wk
        .rows
        .iter()
        .take(shown)
        .map(|r| r.chord.width())
        .max()
        .unwrap_or(0);
    let text = |r: &norte_frontend::whichkey::WhichKeyRow| {
        let sep = if r.reason.is_empty() {
            String::new()
        } else {
            format!(" — {}", r.reason)
        };
        let tail = if r.opens_sequence { " …" } else { "" };
        format!("{}{}{}", r.label, tail, sep)
    };
    let mut lines: Vec<Line<'_>> = wk
        .rows
        .iter()
        .take(shown)
        .map(|r| {
            let pad = " ".repeat(chord_w.saturating_sub(r.chord.width()));
            let chord = Span::styled(format!(" {}{pad}  ", r.chord), theme.role(Role::Title));
            // Dimmed, not hidden: the key IS bound, it just cannot run — the
            // panel says why instead of pretending the key does not exist.
            let style = if r.avail == Availability::Here {
                Style::default()
            } else {
                Style::default().add_modifier(ratatui::style::Modifier::DIM)
            };
            Line::from(vec![chord, Span::styled(text(r), style)])
        })
        .collect();
    if truncated {
        lines.push(Line::raw(format!(
            " {}",
            ta(
                "whichkey-truncated",
                &[("shown", &shown.to_string()), ("total", &total.to_string()),],
            )
        )));
    }
    let title = format!(" {} … ", wk.title);
    let content = lines.iter().map(Line::width).max().unwrap_or(0);
    let width = u16::try_from(content.max(title.width()).saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(base.width);
    let height = u16::try_from(body.saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(outside.max(1));
    let area = Rect {
        x: base.x,
        y: base.y + outside.saturating_sub(height),
        width,
        height,
    };
    clear_themed(frame, area, theme);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(Role::ModalBorder));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Command palette (`Ctrl+P`/vim `:`, H1 T4, spec-promised): free filter over
/// ALL commands, same visual idiom as [`draw_nav_popup`] (centered, input at
/// the foot, `Clear` before painting) but WIDER (60 columns: `{text}
/// {description} {chord}` does not fit in a normal popup's width). A
/// built-in row ([`crate::palette::build_rows`]) brings TRUSTED
/// `text`/`desc`/`chord` (binary constants + Fluent catalogue) — this draw
/// never masks them. A plugin row (P1, [`crate::palette::plugin_rows`])
/// brings THIRD-PARTY text, but ALREADY masked in the row itself (same
/// criterion as `first_chord` with the chord column: masking lives where
/// the row is BUILT, not here) — this draw still does not distinguish, it
/// only paints what is already safe. The dispatch `key` (P1: can carry a
/// plugin's raw `command_id`, with no validated charset) is NEVER read here
/// — [`crate::app::Palette::rows`] is only consulted for
/// `text`/`desc`/`chord`. The query (typed by the user) goes through
/// [`crate::app::Palette::query_display`] (same contract as
/// `QuickSearch::query_display`: a hostile paste does not paint raw
/// bidi/invisibles on the border) + [`display_name`] (same double filter as
/// the pane's quick search bar, line below). The hint is STATIC
/// (`palette-hint`): the palette does NOT resolve through the `dialog`
/// context (decision 8 of the H1 plan — it is a free-filter editor like the
/// search dialog), so there is no GENERATED hint to show here.
///
/// That footer joins with `palette-hint-help` (H3c: `F1` over a row opens
/// the page documenting its command). They go in two keys and are joined
/// HERE because `palette-hint` is also painted by the GUI, which does not
/// yet have a help overlay (phase H3f): a single string would make it
/// announce an inert key.
pub(crate) fn draw_palette(frame: &mut Frame<'_>, palette: &crate::app::Palette, theme: &TuiTheme) {
    let rows = u16::try_from(palette.visible().len().max(1))
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered(frame.area(), 60, rows.min(frame.area().height.max(3)));
    clear_themed(frame, area, theme);
    let inner = usize::from(area.width.saturating_sub(3));
    // Three columns, by CELLS (spec 2026-09-10): the human label first and
    // whole — it is what is read —, the dimmed id, and the chord on the
    // right. The truncation falls on the label and on the id, each in its
    // own column; before, the composed line was truncated and a long id ate
    // into the label until it left "sw…ane".
    let chord_w = palette
        .visible()
        .iter()
        .map(|&i| cells(&palette.rows()[i].chord))
        .max()
        .unwrap_or(1)
        .max(1);
    let id_w = 22.min(inner.saturating_sub(chord_w + 3) / 3);
    let label_w = inner.saturating_sub(id_w + chord_w + 4).max(1);
    let fit = |s: &str, w: usize| {
        let s = middle_ellipsis(s, w);
        let pad = w.saturating_sub(cells(&s));
        format!("{s}{}", " ".repeat(pad))
    };
    let dim = ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM);
    let no_query = palette.query_display().is_empty();
    let (items, selected): (Vec<ListItem<'_>>, Option<usize>) = if palette.visible().is_empty() {
        (vec![ListItem::new(Line::raw(" —"))], None)
    } else {
        (
            palette
                .visible()
                .iter()
                .map(|&i| {
                    let row = &palette.rows()[i];
                    // A recent one is marked only while it is on top for
                    // being one: with a query, the order is whatever
                    // matches.
                    let mark = if no_query && palette.is_recent(i) {
                        "•"
                    } else {
                        " "
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(mark.to_owned(), theme.role(Role::Info)),
                        Span::raw(format!("{} ", fit(&row.desc, label_w))),
                        Span::styled(format!("{} ", fit(&row.text, id_w)), dim),
                        Span::raw(format!("{:>chord_w$}", row.chord)),
                    ]))
                })
                .collect(),
            Some(palette.cursor()),
        )
    };
    let (query, _) = display_name(palette.query_display().as_bytes());
    let footer = Line::raw(format!(
        " /{query}  {} · {} ",
        t("palette-hint"),
        t("palette-hint-help")
    ));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("palette-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(footer)
        .border_style(theme.role(Role::ModalBorder));
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

/// "Go to anywhere" (phase 6): a box with titled sections and one row per
/// destination.
///
/// Same visual idiom as the palette — centered box, query at the foot,
/// selection cursor — with one difference that is why this screen exists:
/// here the rows come from DIFFERENT places, and a list mixing a connection
/// with a command without saying which is which cannot be read. Hence the
/// headers, which do not take the cursor (the model handles that:
/// [`norte_frontend::goto::Goto::up`]/`down`).
///
/// The height comes from what there is, bounded by the frame; the width is
/// fixed and more generous than the palette's because what is painted are
/// PATHS, which are read by their end and truncated in the middle.
pub(crate) fn draw_goto(
    frame: &mut Frame<'_>,
    goto: &norte_frontend::goto::Goto,
    theme: &TuiTheme,
) {
    use norte_frontend::goto::GotoLine;

    /// What is taken from each row on the left: the hostile-text badge, or
    /// the two spaces that replace it when there is none.
    const GOTO_INDENT: usize = 2;

    let height = u16::try_from(goto.lines().len().max(1))
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let width = frame.area().width.saturating_sub(8).clamp(40, 88);
    let area = centered(frame.area(), width, height.min(frame.area().height.max(3)));
    clear_themed(frame, area, theme);
    let inner = usize::from(area.width.saturating_sub(3));
    let dim = theme.role(Role::BorderUnfocused);
    let items: Vec<ListItem<'_>> = if goto.is_empty() {
        vec![ListItem::new(Line::styled(
            format!(" {}", t("goto-empty")),
            dim,
        ))]
    } else {
        goto.lines()
            .iter()
            .map(|line| match line {
                GotoLine::Header(s) => {
                    ListItem::new(Line::styled(t(s.title_key), theme.role(Role::Title)))
                }
                GotoLine::Row(i) => {
                    let row = &goto.rows()[*i];
                    // The badge goes IN FRONT and in its own span, as in
                    // every decision surface: what is painted differently
                    // from what the bytes say is said, not left to guess.
                    let mut spans = Vec::new();
                    if row.hostile {
                        spans.push(Span::styled(
                            format!("{HOSTILE_BADGE} "),
                            theme.role(Role::HostileBadge),
                        ));
                    } else {
                        spans.push(Span::raw("  "));
                    }
                    // `GOTO_INDENT`: the badge takes the same room as the
                    // two spaces that replace it, so the texts line up
                    // whether they carry a flag or not.
                    let detail = cells(&row.desc).min(inner / 2);
                    let text_w = inner.saturating_sub(GOTO_INDENT + detail + 2).max(1);
                    spans.push(Span::raw(middle_ellipsis(&row.text, text_w)));
                    if !row.desc.is_empty() {
                        spans.push(Span::styled(
                            format!("  {}", middle_ellipsis(&row.desc, detail)),
                            dim,
                        ));
                    }
                    ListItem::new(Line::from(spans))
                }
            })
            .collect()
    };
    let (query, _) = display_name(goto.query().as_bytes());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("goto-title")))
        .title_style(theme.role(Role::Title))
        // `>` and not the palette's `/`: here what is written CAN be a
        // path, and a prompt slash glued to an absolute path reads as part
        // of it (`//etc`).
        .title_bottom(Line::raw(format!(" ❯{query}_ ")))
        .border_style(theme.role(Role::ModalBorder));
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    // The model's cursor indexes LINES, which is what is painted: rows and
    // headers. Converting it to a "row index" here would be the same count
    // twice and the chance for them to drift apart.
    state.select((!goto.is_empty()).then_some(goto.cursor()));
    frame.render_stateful_widget(list, area, &mut state);
}

/// The first-launch wizard (spec 2026-09-10): a box with the step's title,
/// the question, the rows with the cursor and the key line. Same visual
/// idiom as the palette.
/// The startup screen (spec 2026-09-15, phase 2): the compass, which build
/// runs and against which core, and — on `home` — the numbered rows of
/// where to go.
///
/// A LAYER over the listing and not a modal: what is behind it is already
/// painted, and any key removes it. That is why the footer says how to
/// leave, which is the only thing a reader needs to know about it.
///
/// The art and the sections come from the SHARED model
/// ([`norte_frontend::splash`]), so the window shows the same thing; here
/// only where the cells land is decided.
pub(crate) fn draw_splash(
    frame: &mut Frame<'_>,
    splash: &norte_frontend::splash::SplashView,
    theme: &TuiTheme,
) {
    use norte_frontend::splash::numbered;

    // COVER: `brief` deliberately comes with NO sections, and with no list
    // to frame, a centered box is a frame around nothing. Mode does not
    // travel in the view — no need — because "there are no sections" is the
    // same signal this painter already uses to pick the footer.
    if splash.sections.is_empty() {
        draw_splash_cover(frame, splash, theme);
        return;
    }

    let lang = norte_i18n::active();
    let numbered_rows = numbered(&splash.sections);
    let art = splash.art.len();
    // Art + version + daemon + air + (title + rows) per section + footer,
    // and the two borders.
    let section_rows: usize = splash
        .sections
        .iter()
        .map(|s| s.rows.len().saturating_add(1))
        .sum();
    let height = u16::try_from(art + 3 + section_rows + 2).unwrap_or(u16::MAX);
    let width = 60.min(frame.area().width.max(20));
    let area = centered(frame.area(), width, height.min(frame.area().height.max(3)));
    clear_themed(frame, area, theme);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("splash-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::styled(
            format!(
                " {} ",
                if numbered_rows.is_empty() {
                    t("splash-hint")
                } else {
                    t("splash-hint-home")
                }
            ),
            theme.role(Role::Info),
        ))
        .border_style(theme.role(Role::ModalBorder));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let mut lines: Vec<Line<'_>> = splash
        .art
        .iter()
        .map(|l| Line::styled((*l).to_owned(), theme.role(Role::Title)))
        .collect();
    lines.push(Line::raw(format!("{} {}", splash.version, splash.revision)));
    lines.push(Line::styled(
        norte_i18n::t_in(lang, splash.daemon.key()),
        theme.role(Role::Info),
    ));
    lines.push(Line::raw(String::new()));
    let mut n = 0usize;
    for section in &splash.sections {
        lines.push(Line::styled(
            norte_i18n::t_in(lang, section.title_key),
            theme.role(Role::Title),
        ));
        for row in &section.rows {
            n += 1;
            // The number only as far as there is a key to call it: beyond
            // that, the row is read and not promised.
            let mark = if n <= numbered_rows.len() {
                format!("{n} ")
            } else {
                "  ".to_owned()
            };
            let usable_w = usize::from(inner.width).saturating_sub(mark.len());
            let text = middle_ellipsis(&row.label, usable_w);
            lines.push(Line::raw(format!("{mark}{text}")));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The cover: the logo filling the screen, with the version and the core
/// below.
///
/// No frame and no dialog title, unlike its sister with a list: a cover
/// that is dismissed with the first key is not something the reader has to
/// close, so it is not painted with the chrome of something that closes.
fn draw_splash_cover(
    frame: &mut Frame<'_>,
    splash: &norte_frontend::splash::SplashView,
    theme: &TuiTheme,
) {
    let lang = norte_i18n::active();
    let area = frame.area();
    clear_themed(frame, area, theme);

    let mut lines: Vec<Line<'_>> = splash
        .art
        .iter()
        .map(|l| Line::styled((*l).to_owned(), theme.role(Role::Title)))
        .collect();
    lines.push(Line::raw(String::new()));
    lines.push(Line::raw(
        format!("{} {}", splash.version, splash.revision)
            .trim()
            .to_owned(),
    ));
    lines.push(Line::styled(
        norte_i18n::t_in(lang, splash.daemon.key()),
        theme.role(Role::Info),
    ));
    lines.push(Line::raw(String::new()));
    lines.push(Line::styled(t("splash-hint"), theme.role(Role::Info)));

    // Centered vertically too: the air on top is half of what is left over.
    // With a terminal shorter than the logo it is painted from the top and
    // truncated at the bottom, which is better than starting halfway
    // through the logo.
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let extra = area.height.saturating_sub(height);
    let inside = ratatui::layout::Rect {
        x: area.x,
        y: area.y.saturating_add(extra / 2),
        width: area.width,
        height: height.min(area.height),
    };
    frame.render_widget(
        Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center),
        inside,
    );
}

pub(crate) fn draw_wizard(
    frame: &mut Frame<'_>,
    wizard: &norte_frontend::wizard::Wizard,
    theme: &TuiTheme,
) {
    let lang = norte_i18n::active();
    let rows = wizard.rows(lang);
    let question = wizard.question(lang);
    let hint = t("wizard-hint");
    let width = 70.min(frame.area().width.max(20));
    let inner = usize::from(width.saturating_sub(4));
    // Question + air + rows + air + keys, plus the two borders.
    let height = u16::try_from(rows.len())
        .unwrap_or(u16::MAX)
        .saturating_add(6)
        .min(frame.area().height.max(3));
    let area = centered(frame.area(), width, height);
    clear_themed(frame, area, theme);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", wizard.title(lang)))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(Role::ModalBorder));
    let inner_area = block.inner(area);
    frame.render_widget(block, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner_area);
    frame.render_widget(
        Paragraph::new(format!(" {}", middle_ellipsis(&question, inner))),
        chunks[0],
    );
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .map(|r| ListItem::new(Line::raw(format!(" {}", middle_ellipsis(r, inner)))))
        .collect();
    let mut state = ListState::default();
    state.select(Some(wizard.cursor()));
    frame.render_stateful_widget(
        List::new(items).highlight_style(theme.role(Role::Selection)),
        chunks[2],
        &mut state,
    );
    frame.render_widget(
        Paragraph::new(format!(" {}", middle_ellipsis(&hint, inner))).style(theme.role(Role::Info)),
        chunks[3],
    );
}

/// Settings overlay (`app.settings`, S3): same visual idiom as
/// [`draw_extensions`] (a Paragraph with interspersed section headers, NOT
/// `List`/`ListState` — there are TWO heterogeneous groups, General and
/// Plugins, and `draw_extensions` already solved that pattern) plus a
/// RESERVED description line under the list (the selected row's,
/// [`Settings::selected_desc`]) and a footer that alternates between the
/// filter (navigating) and the inline edit buffer (`Settings::is_editing`).
/// Name/description are Fluent — text OWNED by the binary, never by a third
/// party (unlike `draw_extensions`, which does mask a plugin's
/// `name`/`publisher`): `display_name` is not needed here, only
/// `middle_ellipsis` for width. The edit buffer IS user input via the
/// terminal (paste included) — it is masked like the query, same contract
/// as `NavPopup::name_input`.
/// A line of the settings list: a section header or a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsLine {
    /// A section's header. Carries the SECTION and not its Fluent key:
    /// whoever knows a section's name is the section itself.
    Header(Section),
    /// A row, by its position among the VISIBLE ones.
    Row(usize),
}

/// What the settings overlay's section index takes up.
const SETTINGS_INDEX_WIDTH: u16 = 20;

/// And the interior width above which it fits. Below it, the list wins: a
/// six-cell index is not an index.
const SETTINGS_INDEX_MIN_WIDTH: u16 = 60;

/// The settings list's lines, headers included, BEFORE scrolling them.
///
/// The header the pinned one above is already showing is painted BLANK
/// instead of removed: removing it would move the rows one line every time
/// the cursor crosses sections, and the line count would stop matching what
/// `geometry` reconciled.
fn settings_list_lines<'a>(
    settings: &'a crate::app::Settings,
    theme: &TuiTheme,
    inner_w: usize,
) -> Vec<Line<'a>> {
    if settings.visible().is_empty() {
        return vec![Line::raw(" —")];
    }
    let plan = settings_line_plan(settings);
    let covered = settings_cursor_section(settings).filter(|s| {
        plan.get(settings.viewport_offset()).copied() == Some(SettingsLine::Header(*s))
    });
    plan.into_iter()
        .map(|item| match item {
            SettingsLine::Header(section) if covered == Some(section) => Line::raw(""),
            SettingsLine::Header(section) => {
                Line::styled(t(section.label_key()), theme.role(Role::Title))
            }
            SettingsLine::Row(pos) => {
                let row = &settings.rows()[settings.visible()[pos]];
                let selected = pos == settings.cursor();
                let cursor = if selected { ">" } else { " " };
                // The "you touched this" dot is a CHARACTER, not a color: a
                // bare color is not information to whoever cannot tell it
                // apart.
                let dot = if row.modified { "•" } else { " " };
                let text = if row.is_plugins_note() {
                    format!("{cursor}{dot}{}", row.name)
                } else {
                    format!("{cursor}{dot}{:<28} {}", row.name, row.value)
                };
                let line = Line::raw(middle_ellipsis(&text, inner_w));
                if selected {
                    // The cursor is ALWAYS painted, whether it has the
                    // keyboard or not, and dimmed when it does not: the
                    // same rule as help's two halves (ADR 0128). Two live
                    // cursors, or none, is what makes you lose track of
                    // where you are.
                    line.style(if settings.focus() == Focus::List {
                        theme.role(Role::Selection)
                    } else {
                        theme.role(Role::SelectionUnfocused)
                    })
                } else {
                    line
                }
            }
        })
        .collect()
}

/// The settings overlay's section index, to the left of the list: each
/// section with how many of its rows are visible.
///
/// A section this surface does NOT have is not listed — the terminal does
/// not project locations, and announcing a section that will never have
/// anything promises something that does not hold — but one the FILTER
/// emptied is, dimmed: an index that changes length while you type cannot
/// be used as a map.
fn draw_settings_index(
    frame: &mut Frame<'_>,
    settings: &crate::app::Settings,
    theme: &TuiTheme,
    area: Rect,
) {
    let width = usize::from(area.width);
    let current = settings_cursor_section(settings);
    let rows: Vec<Line<'_>> = settings
        .sections()
        .into_iter()
        .filter(|v| v.total > 0)
        .map(|v| {
            let text = format!("{} {}", v.title, v.visible);
            let style = if Some(v.section) == current {
                // Same as the list: this side's cursor is always painted,
                // and dimmed when the keyboard is on the other one.
                if settings.focus() == Focus::Index {
                    theme.role(Role::Selection)
                } else {
                    theme.role(Role::SelectionUnfocused)
                }
            } else if v.visible == 0 {
                theme.role(Role::BorderUnfocused)
            } else {
                theme.role(Role::Info)
            };
            // Two cells of air on the right: without them the longest label
            // sticks to the first row's cursor and they read as a single
            // word.
            Line::styled(middle_ellipsis(&text, width.saturating_sub(2)), style)
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), area);
}

/// The section of the row under the cursor: the one that goes PINNED at the
/// top.
///
/// `None` only if there is no visible row at all.
pub(crate) fn settings_cursor_section(settings: &crate::app::Settings) -> Option<Section> {
    let &real = settings.visible().get(settings.cursor())?;
    Some(settings.rows()[real].section)
}

/// The lines the settings list paints, in order: headers and rows.
///
/// Exists on its own because of the window: the cursor counts ROWS and the
/// screen LINES, and section headers fall in between. Whoever reconciles the
/// window (`geometry`) and whoever paints have to count the same way, so
/// they count with this — a second copy of "where the headers go" is a
/// window that drifts out of sync with the drawing as soon as someone adds a
/// section.
pub(crate) fn settings_line_plan(settings: &crate::app::Settings) -> Vec<SettingsLine> {
    let mut plan = Vec::new();
    let mut current: Option<Section> = None;
    for (pos, &real) in settings.visible().iter().enumerate() {
        let section = settings.rows()[real].section;
        if current != Some(section) {
            plan.push(SettingsLine::Header(section));
            current = Some(section);
        }
        plan.push(SettingsLine::Row(pos));
    }
    plan
}

/// How many lines fit in the settings list with a `height`-row screen: the
/// box (`height - 4`, minimum 6) minus its two borders and the description
/// line reserved at the bottom.
///
/// Shared by whoever paints and whoever reconciles the window, for the same
/// reason as [`settings_line_plan`]: a guessed height breaks scroll
/// silently.
pub(crate) fn settings_list_rows(height: u16) -> usize {
    let box_height = height.saturating_sub(4).max(6);
    // Two borders, the description line reserved at the bottom, and the
    // PINNED header on top: that one does not scroll, so it is not part of
    // the list.
    usize::from(
        box_height
            .saturating_sub(2)
            .saturating_sub(1)
            .saturating_sub(1),
    )
}

pub(crate) fn draw_settings(
    frame: &mut Frame<'_>,
    settings: &crate::app::Settings,
    theme: &TuiTheme,
) {
    let width = frame
        .area()
        .width
        .saturating_sub(6)
        .clamp(30, 100)
        .min(frame.area().width);
    let height = frame.area().height.saturating_sub(4).max(6);
    let area = centered(frame.area(), width, height);
    clear_themed(frame, area, theme);

    let footer = if settings.is_editing() {
        let (buf, _) = display_name(settings.edit_buffer().unwrap_or_default().as_bytes());
        Line::raw(format!(" {buf}_  {} ", t("settings-edit-hint")))
    } else {
        let (query, _) = display_name(settings.query_display().as_bytes());
        Line::raw(format!(" /{query}  {} ", t("settings-hint")))
    };
    // The count goes in the TITLE, not the footer, and always: without the
    // second figure "there is nothing" and "I covered it with a letter"
    // read the same, and in the footer it ate the keys' spot, which at 80
    // columns came out truncated.
    let count = ta(
        "settings-count",
        &[
            ("shown", &settings.shown().to_string()),
            ("total", &settings.total().to_string()),
        ],
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} · {count} ", t("settings-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(footer)
        .border_style(theme.role(Role::ModalBorder));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Three bands: the PINNED header, the scrolling list, and the
    // description line. The header on top is the cursor's section's and
    // does not move: it is the only label saying where you are, and one
    // that scrolls goes off the border as soon as you go down three rows.
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    // The INDEX, on the left, when there is room. Below 60 cells the list
    // wins and the index disappears: the same degradation a panel's columns
    // do, and for the same reason — a column that does not fit does not
    // shrink until illegible, it leaves.
    let with_index = inner.width >= SETTINGS_INDEX_MIN_WIDTH;
    let (index_area, body) = if with_index {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(SETTINGS_INDEX_WIDTH), Constraint::Min(1)])
            .split(split[1]);
        (Some(cols[0]), cols[1])
    } else {
        (None, split[1])
    };
    let inner_w = usize::from(body.width);

    if let Some(idx_area) = index_area {
        draw_settings_index(frame, settings, theme, idx_area);
    }

    let pinned = settings_cursor_section(settings).map_or_else(String::new, |s| t(s.label_key()));
    frame.render_widget(
        Paragraph::new(Line::styled(
            middle_ellipsis(&pinned, usize::from(inner.width)),
            theme.role(Role::Title),
        )),
        split[0],
    );

    let lines = settings_list_lines(settings, theme, inner_w);
    // The WINDOW `geometry` reconciled before this frame. Without it the
    // list always painted from the top, and the cursor ran off the bottom
    // as soon as settings stopped fitting on one screen. The `min` is the
    // belt: a window that was never reconciled cannot leave the list blank.
    let from = settings
        .viewport_offset()
        .min(lines.len().saturating_sub(1));
    frame.render_widget(
        Paragraph::new(lines).scroll((u16::try_from(from).unwrap_or(u16::MAX), 0)),
        body,
    );

    let desc = settings.selected_desc().unwrap_or_default();
    let desc_line = Line::raw(format!(
        " {}",
        middle_ellipsis(desc, inner_w.saturating_sub(1))
    ));
    frame.render_widget(
        Paragraph::new(desc_line).style(theme.role(Role::BorderUnfocused)),
        split[2],
    );
}

/// Column the label starts at, in CELLS — a chord wider than this pushes it
/// right instead of overlapping, same rule as the generated keys page.
pub(crate) const SHORTCUT_CHORD_COLUMN: usize = 16;

/// A screen's section header, the SAME one as the generated keys page
/// (`crate::help::build`): two surfaces that list the same thing cannot
/// call it differently.
pub(crate) fn shortcuts_section(screen: norte_frontend::keymap::Screen) -> String {
    match screen {
        norte_frontend::keymap::Screen::Browse => t("help-section-browse"),
        norte_frontend::keymap::Screen::Viewer => t("help-section-viewer"),
        norte_frontend::keymap::Screen::Dialog => t("help-section-dialog"),
    }
}

/// Shortcuts editor (`app.shortcuts`, K3c): same visual idiom as
/// `draw_settings` — a Paragraph with section headers, filter at the foot,
/// a detail line reserved at the bottom — with two differences that are the
/// editor's reason for being:
///
/// - the list SCROLLS. Settings fits on one screen; this is every key of the
///   three screens PLUS every command not bound by any, and a list with no
///   window would leave the cursor outside the box at twenty rows.
/// - the detail line carries the VERDICT while capturing, which is what the
///   reader needs BEFORE confirming, and the rest of the time it carries this
///   terminal's two truths: `esc` cancels (so it is the only chord that
///   cannot be captured here) and `mod+` is Ctrl, because crossterm does not
///   deliver ⌘.
///
/// Chords and labels arrive already painted and translated from the shared
/// model (`norte_frontend::shortcuts`), masking included
/// ([`paint_chord`](norte_frontend::keymap::paint_chord) — a project layer
/// can bind any lone codepoint and this goes to a terminal). Only the width
/// is left here.
pub fn draw_shortcuts(frame: &mut Frame<'_>, sc: &crate::app::Shortcuts, theme: &TuiTheme) {
    let width = frame
        .area()
        .width
        .saturating_sub(4)
        .clamp(30, 92)
        .min(frame.area().width);
    let height = frame.area().height.saturating_sub(2).max(6);
    let area = centered(frame.area(), width, height);
    clear_themed(frame, area, theme);

    let capture = sc.capture();
    let footer = match capture {
        Some(c) if c.is_waiting() => Line::raw(format!(" {} ", t("shortcuts-capture-hint"))),
        Some(_) => Line::raw(format!(" {} ", t("shortcuts-confirm-hint"))),
        None => {
            let (query, _) = display_name(sc.query_display().as_bytes());
            Line::raw(format!(" /{query}  {} ", t("shortcuts-hint")))
        }
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("shortcuts-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(footer)
        .border_style(theme.role(Role::ModalBorder));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let inner_w = usize::from(inner.width);

    let mut items: Vec<Line<'static>> = Vec::new();
    let mut cursor_line = 0usize;
    if sc.visible().is_empty() {
        items.push(Line::raw(" —"));
    } else {
        let mut last: Option<norte_frontend::keymap::Screen> = None;
        for (pos, &real) in sc.visible().iter().enumerate() {
            let row = &sc.rows()[real];
            if last != Some(row.screen) {
                items.push(Line::styled(
                    format!("── {} ──", shortcuts_section(row.screen)),
                    theme.role(Role::Title),
                ));
                last = Some(row.screen);
            }
            let selected = pos == sc.cursor();
            if selected {
                cursor_line = items.len();
            }
            let marker = if selected { ">" } else { " " };
            // A command with no key is NOT dimmed: it CAN be run, it is just
            // that nothing presses it — and that is exactly the row the
            // reader came looking for. Dimmed it would read as
            // "unavailable," which is the other thing.
            let chord = if row.is_bound() {
                row.chord.clone()
            } else {
                t("shortcuts-no-key")
            };
            let pad = " ".repeat(SHORTCUT_CHORD_COLUMN.saturating_sub(chord.width()));
            let text = if row.reason.is_empty() {
                format!("{marker} {chord}{pad} {}", row.label)
            } else {
                format!("{marker} {chord}{pad} {} — {}", row.label, row.reason)
            };
            let mut line = Line::raw(middle_ellipsis(&text, inner_w));
            // Selection is PATCHED onto the dimmed style, not replacing it:
            // a `Line::style` replaces the whole style, and a row not built
            // under the cursor would stop looking like it right when the
            // reader is about to act on it.
            if selected {
                line = line.patch_style(theme.role(Role::Selection));
            }
            if row.avail != norte_frontend::keymap::Availability::Here {
                line =
                    line.patch_style(Style::default().add_modifier(ratatui::style::Modifier::DIM));
            }
            items.push(line);
        }
    }
    // Window around the cursor: without it the selected row disappears
    // below the border as soon as the list exceeds the box's height.
    let h = usize::from(split[0].height).max(1);
    let start = cursor_line
        .saturating_sub(h / 2)
        .min(items.len().saturating_sub(h));
    let end = (start + h).min(items.len());
    frame.render_widget(Paragraph::new(items[start..end].to_vec()), split[0]);

    let detail = match capture {
        Some(c) => {
            let target = norte_frontend::whichkey::pending_title(c.seq(), None);
            match c.verdict() {
                Some(v) => format!(
                    "{target} → {}",
                    norte_frontend::shortcuts::verdict_message(v, norte_i18n::active())
                ),
                None => t("shortcuts-capture-hint"),
            }
        }
        None => t("shortcuts-capture-note"),
    };
    let detail_line = Line::raw(format!(
        " {}",
        middle_ellipsis(&detail, inner_w.saturating_sub(1))
    ));
    frame.render_widget(
        Paragraph::new(detail_line).style(theme.role(Role::BorderUnfocused)),
        split[1],
    );
}

#[cfg(test)]
mod wizard_hint_tests {
    /// The wizard's footer fits WHOLE in its box (70 wide, 66 inside, one of
    /// margin), in both languages. Truncated in the middle — with
    /// `middle_ellipsis` — it lost exactly `[Esc]`: the only key that says
    /// how to skip the questions, on the first screen anyone new sees.
    #[test]
    fn the_wizards_footer_fits_whole() {
        for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
            let hint = norte_i18n::t_in(lang, "wizard-hint");
            let width = unicode_width::UnicodeWidthStr::width(hint.as_str());
            assert!(
                width <= 65,
                "{lang:?}: {width} cells do not fit in 65: {hint}"
            );
            assert!(hint.contains("[Esc]"), "{lang:?}: {hint}");
        }
    }
}

#[cfg(test)]
mod plugin_description_line_tests {
    use super::plugin_description_line;
    use crate::theme::TuiTheme;

    fn sample_plugin(description: Option<&str>) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "1.0.0".into(),
            category: "previewer".into(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: description.map(str::to_owned),
            commands: Vec::new(),
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: false,
            manifest_digest: None,
        }
    }

    fn line_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn no_description_is_none() {
        let p = sample_plugin(None);
        assert!(plugin_description_line(&p, &TuiTheme::default(), 100).is_none());
    }

    /// P1 encoding audit F1 (MEDIUM): a hostile/compromised daemon can send
    /// a `description` with no cap over the wire — this draw does NOT trust
    /// that the caller (`main::dispatch`'s ingest,
    /// `app::clamp_plugin_descriptions`) has already clamped it, and bounds
    /// it here too (self-contained, like `plugin_line`). With a LARGE
    /// `inner` (that does not force ellipsis by width) the final content
    /// reflects EXACTLY `PLUGIN_DESCRIPTION_WIRE_CAP` characters of the
    /// original — not one more, without going through the popup's layout.
    #[test]
    fn clamps_to_the_wire_cap_even_with_no_ingest() {
        let p = sample_plugin(Some(&"a".repeat(50_000)));
        let line = plugin_description_line(&p, &TuiTheme::default(), 10_000)
            .expect("there is a description");
        let text = line_text(&line);
        assert_eq!(
            text.chars().filter(|&c| c == 'a').count(),
            crate::app::PLUGIN_DESCRIPTION_WIRE_CAP,
            "the draw processed more than PLUGIN_DESCRIPTION_WIRE_CAP chars of the original: {text:?}"
        );
    }

    /// A raw RTL override (not going through ingest) is masked to U+FFFD
    /// HERE — it never reaches `ratatui` intact (where a control/override
    /// is invisible: it would vanish silently instead of being marked).
    #[test]
    fn masks_a_raw_rtl_override_even_with_no_ingest() {
        let p = sample_plugin(Some("abc\u{202E}gpj.exe"));
        let line = plugin_description_line(&p, &TuiTheme::default(), 10_000)
            .expect("there is a description");
        let text = line_text(&line);
        assert!(!text.contains('\u{202E}'));
        assert!(text.contains('\u{FFFD}'));
    }
}

#[cfg(test)]
mod which_key_render_tests {
    use super::draw_which_key;
    use crate::theme::TuiTheme;
    use norte_frontend::keymap::{Effective, Screen, parse_chord, parse_keymap};
    use norte_frontend::whichkey::WhichKeyRows;
    use norte_i18n::Lang;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

    fn panel(count: Option<u32>) -> WhichKeyRows {
        let src = r#"
counts = true

[pane]
keymap = [
    { on = ["g", "g"], run = "cursor.top" },
    { on = ["g", "p"], run = "pane.pack" },
    { on = ["g", "a", "b"], run = "mark.all" },
]
"#;
        let preset = parse_keymap(src).expect("fixture parses");
        let known = ["cursor.top", "mark.all"];
        let eff =
            Effective::build_for(&preset, &[], &known, Screen::Browse).expect("fixture builds");
        WhichKeyRows::build(&eff, &[parse_chord("g").expect("chord")], count, Lang::En)
    }

    /// The panel paints its title (count included), one row per continuation,
    /// and the unavailable row DIMMED with its reason — the reader is told why
    /// the key does nothing instead of not finding the key at all.
    #[test]
    fn the_panel_paints_every_continuation_and_dims_the_unavailable_one() {
        // This test asserts the ENGLISH corpus strings. Without pinning the
        // language it resolved by environment (`LANG`), so it was green in
        // CI and red on any machine with `LANG=es_*` — the same line the
        // rest of this crate's render tests already carried.
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let theme = TuiTheme::default();
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("test terminal");
        terminal
            .draw(|f| draw_which_key(f, &panel(Some(12)), &theme))
            .expect("draw");
        let text = terminal.backend().to_string();
        assert!(text.contains("12 g"), "the count in flight: {text}");
        assert!(text.contains("go to top"), "the available row: {text}");
        // The unavailable row still names its command and says why. It used
        // to be a `Planned` one, with its issue number; #132 built the last of
        // those, so what is unavailable now is a command this frontend does
        // not implement — the row and the reason work the same way, which is
        // the property under test.
        assert!(
            text.contains("pack into an archive"),
            "the unavailable row: {text}"
        );
        assert!(!text.contains("help-cmd-"), "a raw Fluent id: {text}");
        assert!(
            text.contains(&norte_i18n::t("keymap-short-not-here")),
            "and why: {text}"
        );
        assert!(text.contains('…'), "the row that opens more keys: {text}");

        // The `p` row is dimmed; the `g` row is not.
        let buf = terminal.backend().buffer();
        let dim_of = |needle: &str| -> bool {
            for y in 0..buf.area.height {
                let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                if row.contains(needle) {
                    return (0..buf.area.width)
                        .any(|x| buf[(x, y)].modifier.contains(Modifier::DIM));
                }
            }
            panic!("no row painted {needle}");
        };
        assert!(
            dim_of("pack into an archive"),
            "an unavailable row is dimmed"
        );
        assert!(!dim_of("go to top"), "an available one is not");
    }

    /// A terminal too short for the rows keeps the status line free, stays
    /// inside the frame and COUNTS what it dropped: a box that just ends
    /// implies the list ended with it.
    #[test]
    fn a_short_terminal_truncates_with_a_count_and_never_overflows() {
        let theme = TuiTheme::default();
        // Five rows: one for the status bar, two borders, and two lines
        // inside — one real key and the count of the two that did not fit.
        let mut terminal = Terminal::new(TestBackend::new(24, 5)).expect("test terminal");
        terminal
            .draw(|f| draw_which_key(f, &panel(None), &theme))
            .expect("draw");
        let text = terminal.backend().to_string();
        assert!(text.contains("1/3"), "the rows it could not show: {text}");
        // The last line — the status bar's — was not painted over.
        let last = text.lines().last().expect("a last line").to_owned();
        assert!(
            last.chars().all(|c| c.is_whitespace() || c == '"'),
            "the status line was overwritten: {last:?}"
        );

        // Too short for even one real key: the panel does not open at all, and
        // the bar's pending segment is what the reader is left with — a box
        // whose one line says "… 0/3" would spend three rows saying nothing.
        let mut squeezed = Terminal::new(TestBackend::new(24, 4)).expect("test terminal");
        squeezed
            .draw(|f| draw_which_key(f, &panel(None), &theme))
            .expect("draw");
        let painted = squeezed.backend().to_string();
        assert!(
            painted.chars().all(|c| c.is_whitespace() || c == '"'),
            "nothing is painted: {painted}"
        );

        // Degenerate geometry must not panic or paint outside the frame. The
        // narrow-but-TALL one is the interesting case: it is the only one that
        // reaches the drawing code, with `width` clamped to a box that is all
        // border and no inside.
        for (w, h) in [(4_u16, 1_u16), (1, 3), (2, 2), (1, 10), (3, 12)] {
            let mut tiny = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
            tiny.draw(|f| draw_which_key(f, &panel(None), &theme))
                .expect("draw");
        }
    }
}

#[cfg(test)]
mod draw_shortcuts_tests {
    use super::{TuiTheme, draw_shortcuts};
    use norte_frontend::keymap::{Effective, Screen, parse_chord, parse_keymap};
    use norte_frontend::shortcuts::{ScreenKeys, ShortcutsState, build_rows};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    // Nobody binds `pane.move`: it is the row WITH NO KEY the reference
    // sheet cannot have.
    const BINDABLE: &[&str] = &["pane.copy", "pane.mkdir", "pane.move"];

    fn eff() -> Effective {
        // A HOSTILE chord (U+202E RIGHT-TO-LEFT OVERRIDE) bound as a lone
        // codepoint: legal, and untrusted — a project layer arrives with a
        // cloned repository.
        let src = "[pane]\nkeymap = [\n  { on = [\"f5\"], run = \"pane.copy\" },\n  { on = [\"alt+f5\"], run = \"pane.pack\" },\n  { on = [\"\u{202e}\"], run = \"pane.mkdir\" },\n]\n";
        let preset = parse_keymap(src).expect("fixture parses");
        Effective::build_for(&preset, &[], BINDABLE, Screen::Browse).expect("fixture builds")
    }

    fn state(eff: &Effective) -> ShortcutsState {
        ShortcutsState::new(build_rows(
            &[ScreenKeys {
                screen: Screen::Browse,
                eff,
                bindable: BINDABLE,
            }],
            norte_i18n::active(),
        ))
    }

    fn painted(sc: &ShortcutsState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(90, 14)).expect("test terminal");
        terminal
            .draw(|f| draw_shortcuts(f, sc, &TuiTheme::default()))
            .expect("draw");
        terminal.backend().to_string()
    }

    /// The screen SAYS the two things this terminal cannot do: `esc`
    /// cancels (so it is the only chord that cannot be captured) and `mod+`
    /// is Ctrl, because crossterm does not deliver ⌘ without the Kitty
    /// protocol. Without that line the reader discovers both by pressing.
    #[test]
    fn the_screen_says_what_this_terminal_cannot_capture() {
        let eff = eff();
        let text = painted(&state(&eff));
        assert!(text.contains("esc"), "{text}");
        assert!(text.contains("mod+"), "{text}");
    }

    /// A command with no key is visible (the row the reference sheet cannot
    /// have), and a key this build cannot run is visible with its reason —
    /// nothing falls silently.
    ///
    /// The "not runnable" example used to be a `Planned` capability with its
    /// issue number. With #132 built, none are left: the reason painted now
    /// is the one for a command that exists and this frontend does not
    /// implement, which is the other half of the same thing — and it is
    /// still a row with an explanation instead of a key that does nothing.
    #[test]
    fn the_keyless_row_and_the_unbuilt_one_are_both_visible() {
        let eff = eff();
        let text = painted(&state(&eff));
        assert!(text.contains(&norte_i18n::t("shortcuts-no-key")), "{text}");
        assert!(
            text.contains(&norte_i18n::t("keymap-short-not-here")),
            "the reason for the row this build does not run: {text}"
        );
    }

    /// The verdict is painted BEFORE confirming, and the captured chord is
    /// painted MASKED: a hostile codepoint does not reach the terminal raw
    /// through the detail line any more than it does through the list.
    #[test]
    fn the_verdict_is_painted_and_chords_come_masked() {
        let eff = eff();
        let mut sc = state(&eff);
        assert!(sc.begin_capture());
        sc.capture_chord(parse_chord("\u{202e}").expect("chord"), &eff);
        let text = painted(&sc);
        // By LINE: the `\n`s `to_string` joins are the harness's, not the
        // buffer's (same criterion as this file's other render tests).
        assert!(
            text.lines()
                .all(|l| !l.chars().any(norte_encoding::is_terminal_hazard)),
            "{text}"
        );
        // `Replaces`: the hostile codepoint is already bound to
        // `pane.mkdir`, and the verdict painted is the MODEL's, not a
        // parallel sentence. Fluent isolates its arguments with direction
        // marks (U+2066..U+2069) ratatui's zero-width buffer does not
        // paint: they are stripped for comparison, instead of comparing
        // against something else.
        let verdict = norte_frontend::shortcuts::verdict_message(
            sc.capture()
                .and_then(norte_frontend::shortcuts::Capture::verdict)
                .expect("there is a verdict"),
            norte_i18n::active(),
        );
        let want: String = verdict
            .chars()
            .filter(|c| !('\u{2066}'..='\u{2069}').contains(c))
            .collect();
        assert!(text.contains(&want), "{want:?} in {text}");
    }

    /// Degenerate geometries: no panic and no painting outside the frame.
    /// The box has a window over the list, and a badly computed window is
    /// the usual way to run off the bottom.
    #[test]
    fn degenerate_geometries_do_not_crash() {
        let eff = eff();
        let mut sc = state(&eff);
        for _ in 0..20 {
            sc.down();
        }
        for (w, h) in [(4_u16, 1_u16), (1, 3), (2, 2), (1, 10), (3, 12), (30, 5)] {
            let mut tiny = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
            tiny.draw(|f| draw_shortcuts(f, &sc, &TuiTheme::default()))
                .expect("draw");
        }
    }
}
