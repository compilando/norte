//! The window's chrome: the menu bar with its click zones, and each side's
//! tab strip.
//!
//! Both follow the same shape: one function MEASURES the zones (`menu_zones`,
//! `tab_zones`) and another PAINTS, because whoever routes a click needs the
//! geometry without having painted anything.

use norte_frontend::panelbar::figure;
use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::clear_themed;
use super::geometry::{pane_rects, tab_strip_for};
use crate::app::App;
use crate::theme::TuiTheme;

/// A pane's tabs: each one's title and which is active.
///
/// Titles arrive already SANITIZED (`display_name`): a hostile directory
/// name inside a tab is as hostile as inside a listing.
pub struct TabStrip {
    /// Each tab's title, in order.
    pub titles: Vec<String>,
    /// Which one is active.
    pub active: usize,
}

/// What can be clicked in the menu bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuHit {
    /// A title: opens it.
    Title(usize),
    /// An item of the open menu: runs it.
    Item(usize),
}

/// A clickable zone of the menu bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuZone {
    /// Row.
    pub row: u16,
    /// First column, inclusive.
    pub x0: u16,
    /// Last column, inclusive.
    pub x1: u16,
    /// What clicking it does.
    pub hit: MenuHit,
}

/// The menu's geometry: titles with their range and the dropdown with its
/// own.
///
/// ONE source for what is painted and what is clicked, for the same reason
/// as the tab bar: measuring it twice is how a click opens the menu next
/// door.
pub(crate) struct MenuGeom {
    /// The dropdown's box.
    drop: Rect,
    /// The dropdown's lines, top to bottom: commands and separators.
    lines: Vec<MenuLine>,
}

/// A line of the dropdown.
pub(crate) enum MenuLine {
    /// The start of a section: a rule, with its label if it has one.
    Section(Option<String>),
    /// A command: its index in the menu's flat list, label, chord and role.
    Item {
        index: usize,
        label: String,
        chord: String,
        role: norte_frontend::menu::ItemRole,
    },
}

/// The mark for a command an AI model performs.
pub(crate) const AI_MARK: &str = " ✦";

/// Width cap of the dropdown: a menu is a list of short labels, so a wide
/// one is always a symptom. The cap keeps a long translation from covering
/// the screen again, which is what happened when the labels were
/// `help-cmd-*`'s sentences.
pub(crate) const DROP_MAX: u16 = 44;

/// Computes the open menu's geometry, or `None` if there is none.
/// The bar's titles and where each lands, OPEN OR NOT.
///
/// Separated from [`menu_geom`] because that one returns via `?` as soon as
/// the menu is closed — it has to: with no menu open there is no dropdown to
/// measure — and with the bar pinned that left the row blank. Titles do not
/// depend on anything being open; the dropdown does.
pub(crate) fn menu_titles(area: Rect) -> Vec<(String, u16, u16)> {
    let names: Vec<String> = norte_frontend::menu::MENUS
        .iter()
        .map(|m| norte_i18n::t(m.title))
        .collect();
    // Two spaces between titles when they fit, one when they do not. With
    // ten menus the bar in Spanish measures 82 columns at double spacing,
    // and on an 80-wide terminal the last one — "Help", exactly the one a
    // new reader looks for — vanished. Tightening the bar beats amputating
    // it: what it has to say is which menus THERE ARE.
    let width = |sep: usize| -> usize {
        names
            .iter()
            .map(|n| UnicodeWidthStr::width(n.as_str()) + sep)
            .sum()
    };
    let roomy = width(2) <= usize::from(area.width);
    let mut titles = Vec::new();
    let mut x = area.x;
    for name in names {
        let label = if roomy {
            format!(" {name} ")
        } else {
            format!(" {name}")
        };
        let w = u16::try_from(UnicodeWidthStr::width(label.as_str())).unwrap_or(0);
        let x1 = x.saturating_add(w).saturating_sub(1);
        titles.push((label, x, x1));
        x = x.saturating_add(w);
    }
    titles
}

pub(crate) fn menu_geom(app: &App, area: Rect) -> Option<MenuGeom> {
    let titles = menu_titles(area);
    let st = app.menu.as_ref()?;
    let m = norte_frontend::menu::MENUS.get(st.menu())?;
    let mut lines: Vec<MenuLine> = Vec::new();
    for (index, id) in m.items().enumerate() {
        if let Some(title) = m.section_at(index) {
            lines.push(MenuLine::Section(title.map(norte_i18n::t)));
        }
        // The label is SHORT and its own (`menu-item-*`), not the
        // `help-cmd-*` sentence: that is a description, and using it made
        // the dropdown seventy columns wide and covered both panes. Piloting
        // the TUI in tmux uncovered it, not the suite.
        let label = norte_i18n::t(&format!("menu-item-{}", id.replace('.', "-")));
        // With no key, nothing: a dash on every command with no shortcut
        // was noise that read as "disabled."
        let chord = app
            .palette_rows
            .iter()
            .find(|r| r.key == id)
            .map_or_else(String::new, |r| r.chord.clone());
        lines.push(MenuLine::Item {
            index,
            label,
            chord,
            role: norte_frontend::menu::role(id),
        });
    }
    // A menu that does not fit in height drops the rules before the
    // commands: first the unnamed separators, then the labels. The commands
    // all stay, which is what the menu is for.
    let max_height = usize::from(area.height.saturating_sub(3));
    if lines.len() > max_height {
        lines.retain(|l| !matches!(l, MenuLine::Section(None)));
    }
    if lines.len() > max_height {
        lines.retain(|l| matches!(l, MenuLine::Item { .. }));
    }
    // Width: the longest label, its key, two borders and the gap between
    // both columns; and the longest section label with its rules.
    let text_width = lines
        .iter()
        .map(|l| match l {
            MenuLine::Item {
                label, chord, role, ..
            } => {
                let mark = if *role == norte_frontend::menu::ItemRole::Ai {
                    UnicodeWidthStr::width(AI_MARK)
                } else {
                    0
                };
                UnicodeWidthStr::width(label.as_str())
                    + mark
                    + UnicodeWidthStr::width(chord.as_str())
                    + 3
            }
            MenuLine::Section(Some(t)) => UnicodeWidthStr::width(t.as_str()) + 4,
            MenuLine::Section(None) => 0,
        })
        .max()
        .unwrap_or(10);
    let w = u16::try_from(text_width + 2)
        .unwrap_or(u16::MAX)
        .min(area.width)
        .min(DROP_MAX);
    let h = u16::try_from(lines.len() + 2)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(1));
    let x0 = titles
        .get(st.menu())
        .map_or(area.x, |(_, x0, _)| *x0)
        .min(area.x.saturating_add(area.width).saturating_sub(w));
    Some(MenuGeom {
        drop: Rect {
            x: x0,
            y: area.y.saturating_add(1),
            width: w,
            height: h,
        },
        lines,
    })
}

/// The menu bar's clickable zones.
#[must_use]
pub fn menu_zones(app: &App, area: Rect) -> Vec<MenuZone> {
    // TITLES are clickable whenever the bar is on screen, whether the menu
    // is open or not. They used to come from `menu_geom`, which returns
    // `None` with the menu closed — it has to, with nothing open there is
    // no dropdown to measure — so with the bar pinned it was visible and
    // could not be clicked: a bar that exists so you can find the menu, on
    // which the click does nothing.
    let mut out: Vec<MenuZone> = if app.menu_bar || app.menu.is_some() {
        menu_titles(area)
            .iter()
            .enumerate()
            .map(|(i, (_, x0, x1))| MenuZone {
                row: area.y,
                x0: *x0,
                x1: *x1,
                hit: MenuHit::Title(i),
            })
            .collect()
    } else {
        Vec::new()
    };
    let Some(g) = menu_geom(app, area) else {
        return out;
    };
    // By PAINTED LINE, not by command: with sections, command `i` no longer
    // lands on row `i`, and a rule is not clickable.
    for (row, line) in g.lines.iter().enumerate() {
        let row = g
            .drop
            .y
            .saturating_add(1)
            .saturating_add(u16::try_from(row).unwrap_or(0));
        if row >= g.drop.y.saturating_add(g.drop.height).saturating_sub(1) {
            break;
        }
        let MenuLine::Item { index, .. } = line else {
            continue;
        };
        out.push(MenuZone {
            row,
            x0: g.drop.x.saturating_add(1),
            x1: g.drop.x.saturating_add(g.drop.width).saturating_sub(2),
            hit: MenuHit::Item(*index),
        });
    }
    out
}

/// Where the layout buttons land (ADR 0133): on the RIGHT edge of the menu
/// bar, if they fit whole without stepping on a title. ONE measurement for
/// painting and for the mouse.
pub(crate) fn layout_button_cells(
    app: &App,
    area: Rect,
) -> Vec<(u16, &'static norte_frontend::layoutbar::LayoutButton)> {
    // With an overlay in front or the menu dropped, they are neither
    // painted nor clickable: overlays do not cover row 0, so painted they
    // would be visible and dead (ADR 0133 review; the panel bar had the
    // same BLOCKER). The check lives HERE so painting and the mouse cannot
    // drift apart.
    if !app.menu_bar || app.menu.is_some() || crate::mouse::overlay_open(app) {
        return Vec::new();
    }
    let used: usize = menu_titles(area)
        .iter()
        .filter(|(_, _, x1)| *x1 < area.x.saturating_add(area.width))
        .map(|(l, _, _)| UnicodeWidthStr::width(l.as_str()))
        .sum();
    // A title is worth more than a button: what is left, minus a
    // separating space, decides which fit whole — and the first to yield is
    // rotate (ADR 0138), the shared rule.
    let buttons =
        norte_frontend::layoutbar::fitting(usize::from(area.width).saturating_sub(used + 1));
    let total = norte_frontend::layoutbar::width_of(&buttons);
    let mut x = area
        .x
        .saturating_add(area.width)
        .saturating_sub(u16::try_from(total).unwrap_or(u16::MAX));
    let mut out = Vec::new();
    for b in buttons {
        out.push((x, b));
        x = x.saturating_add(u16::try_from(b.glyph.len() + 1).unwrap_or(u16::MAX));
    }
    out
}

/// Paints the layout buttons (ADR 0133) on `bar`'s right edge and returns
/// how many cells they reserve, separator included — what the menu
/// shortcut's hint has to leave them.
fn draw_layout_buttons(frame: &mut Frame<'_>, app: &App, area: Rect, bar: Rect) -> u16 {
    let buttons = layout_button_cells(app, area);
    for (x, b) in &buttons {
        let w = u16::try_from(b.glyph.len()).unwrap_or(0);
        frame.render_widget(
            Paragraph::new(ratatui::text::Line::styled(
                b.glyph,
                app.theme.role(Role::Title),
            )),
            Rect {
                x: *x,
                width: w,
                ..bar
            },
        );
    }
    let painted: Vec<&norte_frontend::layoutbar::LayoutButton> =
        buttons.iter().map(|(_, b)| *b).collect();
    if painted.is_empty() {
        0
    } else {
        u16::try_from(norte_frontend::layoutbar::width_of(&painted) + 1).unwrap_or(u16::MAX)
    }
}

/// The layout buttons' zones: the SAME cells that get painted
/// (`layout_button_cells`), which already stays quiet with an overlay in
/// front or the menu open.
#[must_use]
pub fn layout_zones(app: &App, area: Rect) -> Vec<PanelZone> {
    layout_button_cells(app, area)
        .into_iter()
        .map(|(x0, b)| PanelZone {
            row: area.y,
            x0,
            x1: x0.saturating_add(u16::try_from(b.glyph.len()).unwrap_or(u16::MAX) - 1),
            command: b.command.to_owned(),
        })
        .collect()
}

/// A clickable box in the panel bar (#324).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelZone {
    /// Row.
    pub row: u16,
    /// First column, inclusive.
    pub x0: u16,
    /// Last column, inclusive.
    pub x1: u16,
    /// The command clicking it triggers.
    pub command: String,
}

/// A clickable cell of the key bar (spec 2026-09-10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyZone {
    /// Row.
    pub row: u16,
    /// First column, inclusive.
    pub x0: u16,
    /// Last column, inclusive.
    pub x1: u16,
    /// The function key, `1`..=`10`.
    pub key: u8,
}

/// The key bar's clickable cells: the SAME layout as the painting
/// (`keybar::layout`), so they measure the same thing. An empty cell — a key
/// bound to nothing on this screen — is not a zone: clicking it would do
/// nothing, and a zone that does nothing is confusing.
#[must_use]
pub fn key_zones(app: &App, area: Rect) -> Vec<KeyZone> {
    let Some(bar) = crate::ui::geometry::key_bar_area(app, area) else {
        return Vec::new();
    };
    let cells = app.key_bar_cells();
    norte_frontend::keybar::layout(usize::from(bar.width))
        .into_iter()
        .zip(cells)
        .filter(|(_, c)| c.command.is_some())
        .map(|((x0, w), c)| KeyZone {
            row: bar.y,
            x0: bar.x.saturating_add(u16::try_from(x0).unwrap_or(u16::MAX)),
            x1: bar
                .x
                .saturating_add(u16::try_from(x0 + w).unwrap_or(u16::MAX))
                .saturating_sub(1),
            key: c.key,
        })
        .collect()
}

/// Paints the key bar: ten cells with the number and what each key does on
/// the screen that has the keyboard. The number carries the status bar's
/// style and the label the inverted selection one, as in mc: two tones so
/// they read as ten buttons and not as a sentence.
pub(crate) fn draw_key_bar(frame: &mut Frame<'_>, app: &App) {
    let Some(bar) = crate::ui::geometry::key_bar_area(app, frame.area()) else {
        return;
    };
    clear_themed(frame, bar, &app.theme);
    let cells = app.key_bar_cells();
    let number_style = app.theme.role(Role::Regular);
    // `Button` and not `StatusBar` (2026-09-11): in the stock theme the
    // status bar and the cursor carry the same color pair, and the key row
    // above the status one read as a single stripe. A cell of this bar is a
    // button, and that role already exists for the ones in modals.
    let label_style = app.theme.role(Role::Button);
    let mut spans: Vec<ratatui::text::Span<'static>> = Vec::new();
    for ((_, w), c) in norte_frontend::keybar::layout(usize::from(bar.width))
        .into_iter()
        .zip(cells)
    {
        let text = norte_frontend::keybar::cell_text(c, w);
        let n = c.key.to_string().len();
        let (num, label) = text.split_at(n);
        spans.push(ratatui::text::Span::styled(num.to_owned(), number_style));
        // An empty cell keeps the base background: a key that does nothing
        // is not painted as a button.
        let style = if c.command.is_some() {
            label_style
        } else {
            number_style
        };
        spans.push(ratatui::text::Span::styled(label.to_owned(), style));
    }
    frame.render_widget(Paragraph::new(ratatui::text::Line::from(spans)), bar);
}

/// Are panel NAMES painted? `[ui] panel_bar_style = "names"` and all of them
/// have to fit the row; otherwise, letters (spec 2026-09-10). One single
/// answer for painting and for the mouse zones, so they measure the same
/// thing.
fn shows_names(app: &App, buttons: &[norte_frontend::panelbar::PanelButton], bar: Rect) -> bool {
    app.chrome.panel_bar_style().shows_names()
        && norte_frontend::panelbar::names_fit(buttons, usize::from(bar.width))
}

/// The kind of the panel that has the keyboard, if a panel has it.
///
/// Translates `KeyOwner` to a kind: the bar reasons in kinds because that
/// is what the registry gives it, and `KeyOwner` is the TUI's own business.
/// The borrow is from `app` and not `'static` since phase 3: a plugin
/// panel's kind is `plugin:<id>:<kind>`, a string that lives in the tree and
/// is not known at compile time.
fn kind_with_keyboard(app: &App) -> Option<&str> {
    match app.key_owner() {
        crate::app::KeyOwner::Panes => None,
        crate::app::KeyOwner::Places => Some("places"),
        crate::app::KeyOwner::Preview => Some(crate::preview::KIND),
        crate::app::KeyOwner::Processes => Some(crate::processes::KIND),
        crate::app::KeyOwner::Tree => Some(crate::tree::KIND),
        crate::app::KeyOwner::Log => Some(crate::logview::KIND),
        crate::app::KeyOwner::DiskMap => Some(crate::diskmap::KIND),
        crate::app::KeyOwner::Timeline => Some(crate::timeline::KIND),
        crate::app::KeyOwner::Terminal => Some(crate::termpanel::KIND),
        // Which one it is is said by the layout, not the enum: there is at
        // most one visible.
        crate::app::KeyOwner::Panel => app.panel_kind(),
    }
}

/// The panel bar's buttons, with what the `App` knows.
///
/// The COLLECTING-the-state part lives here and not in `norte-frontend`;
/// WHAT they are and their ORDER are decided by `panelbar::buttons`, shared
/// with the window.
#[must_use]
pub fn panel_buttons(app: &App, area: Rect) -> Vec<norte_frontend::panelbar::PanelButton> {
    // From the LAYOUT and not the tree (#331). It took two steps, and both
    // were needed: #329 swapped `slot_ids` for `visible_slot_ids` because a
    // slot behind an inactive tab exists and is not seen; but
    // `visible_slot_ids` answers which tab is active, not what FITS. A
    // panel whose tab IS active and that the layout drops for lack of room
    // was still painted as open. Placements are literally what gets
    // painted, so they cover both questions at once.
    //
    // Costs one more layout per frame, like `tab_zones` and its neighbors:
    // it is the price of chrome telling the truth about a body that already
    // got laid out.
    let res = crate::ui::geometry::resolved_frame(app, area);
    // In SCREEN ORDER, which is the buttons' order: top to bottom and, at
    // the same height, left to right. The layout gives them in the order it
    // walks the tree, which almost always matches and does not guarantee
    // it; and "almost always" in a row learned by finger is not good
    // enough.
    let mut placed: Vec<_> = res.placements.iter().collect();
    placed.sort_by_key(|(_, r)| (r.y, r.x));
    let open: Vec<&str> = placed
        .iter()
        .map(|(id, _)| *id)
        .filter_map(|id| {
            app.layout
                .kind_of(id)
                .map(norte_frontend::layout::KindId::as_str)
        })
        .collect();
    let focus = kind_with_keyboard(app);
    // News: the log with unseen errors, and processes with live tasks. It
    // is what makes someone look at the bar instead of just remembering it.
    let mut attention: Vec<(&str, u32)> = Vec::new();
    // With the panel IN VIEW you are already seeing it: the mark is
    // redundant, and on top of that it stole the style from the status bar
    // for as long as the task lasted. Same criterion as the log, right
    // below.
    //
    // "In view" and not "existing" since #329: hidden in a tab you are not
    // seeing it, and silencing the mark there muted the warning right in
    // the case where it is useful. It asks `open`, which already IS the set
    // of visible kinds: walking the tree again would cost two more passes
    // per frame and would leave the same question answered in two places,
    // free to drift apart.
    if !open.contains(&crate::processes::KIND) {
        attention.push((crate::processes::KIND, figure(app.board.rows().len())));
    }
    // Errors or warnings in the log the reader has not had in front of
    // them: if the panel is open they are already seeing them, so the mark
    // is redundant.
    //
    // `count_at_or_above` and not `snapshot`: this runs every frame, and
    // cloning the whole ring to count warnings was two thousand lines with
    // their two `String`s each, ten times a second.
    if !open.contains(&crate::logview::KIND)
        && let Some(r) = app.log_ring.as_ref()
    {
        attention.push((
            crate::logview::KIND,
            figure(r.count_at_or_above(norte_config::logline::LogLevel::Warn)),
        ));
    }
    norte_frontend::panelbar::buttons(
        &app.kinds,
        norte_frontend::panelbar::PanelBarInput {
            open: &open,
            focused: focus,
            attention: &attention,
        },
    )
}

/// The panel bar's clickable boxes.
///
/// And the menu bar's layout buttons' (ADR 0133): they are the same thing —
/// a chrome box that runs a command through its shortcut's dispatch — so
/// the mouse resolves them through the same path.
#[must_use]
pub fn panel_zones(app: &App, area: Rect) -> Vec<PanelZone> {
    let mut out = panel_bar_zones(app, area);
    out.extend(layout_zones(app, area));
    out.extend(hidden_tab_zones(app, area));
    out
}

/// Paints the panel groups' tab strips (ADR 0134): the one in front with
/// the title style and underlined, the others dimmed.
pub(crate) fn draw_pane_strips(frame: &mut Frame<'_>, app: &App) {
    for (row, tabs) in crate::ui::geometry::panel_tab_strips(app, frame.area()) {
        clear_themed(frame, row, &app.theme);
        let spans: Vec<ratatui::text::Span<'static>> = tabs
            .into_iter()
            .map(|p| {
                let style = if p.active {
                    app.theme
                        .role(Role::Title)
                        .add_modifier(ratatui::style::Modifier::UNDERLINED)
                } else {
                    app.theme
                        .role(Role::Regular)
                        .add_modifier(ratatui::style::Modifier::DIM)
                };
                ratatui::text::Span::styled(p.text, style)
            })
            .collect();
        frame.render_widget(Paragraph::new(ratatui::text::Line::from(spans)), row);
    }
}

/// The HIDDEN tabs of the panel groups as clickable zones: clicking one runs
/// its panel's command, which with the panel hidden SHOWS it (#329). The
/// one in front is not a zone — clicking it would close it.
fn hidden_tab_zones(app: &App, area: Rect) -> Vec<PanelZone> {
    if crate::mouse::overlay_open(app) || app.menu.is_some() {
        return Vec::new();
    }
    let buttons = panel_buttons(app, area);
    let mut out = Vec::new();
    for (row, tabs) in crate::ui::geometry::panel_tab_strips(app, area) {
        for p in tabs.into_iter().filter(|p| !p.active) {
            let Some(kind) = app.layout.kind_of(p.slot) else {
                continue;
            };
            // With no button there is no command to bring it forward:
            // `layout.<kind>` does not exist for a plugin's panel, and a
            // zone that dispatches an unknown command is a dead click with
            // a warning.
            let Some(command) = buttons
                .iter()
                .find(|b| b.kind == kind.as_str())
                .map(|b| b.command.clone())
            else {
                continue;
            };
            out.push(PanelZone {
                row: row.y,
                x0: p.x0,
                x1: p.x1,
                command,
            });
        }
    }
    out
}

/// The rows of the panel COLUMN (ADR 0140): one per button, and with air —
/// a blank row between two, and another on top — if they all fit that way,
/// like VS Code's activity bar; tight otherwise. One count for painting and
/// for the mouse.
fn rail_rows(n: usize, bar: Rect) -> Vec<u16> {
    let height = usize::from(bar.height);
    let (from, step) = if n > 0 && 2 * n <= height {
        (1, 2)
    } else if n > 0 && 2 * n - 1 <= height {
        (0, 2)
    } else {
        (0, 1)
    };
    (0..n)
        .map(|i| from + i * step)
        .take_while(|f| *f < height)
        .map(|f| bar.y.saturating_add(u16::try_from(f).unwrap_or(u16::MAX)))
        .collect()
}

/// The panel bar's boxes, alone.
fn panel_bar_zones(app: &App, area: Rect) -> Vec<PanelZone> {
    let Some(bar) = crate::ui::geometry::panel_bar_visible(app, area) else {
        return Vec::new();
    };
    let mut x = bar.x;
    let mut out = Vec::new();
    let buttons = panel_buttons(app, area);
    // In a column, one button per row and the whole rail's width: the same
    // rows `draw_panel_bar` paints.
    if crate::ui::geometry::bar_in_column(app) {
        for (y, b) in rail_rows(buttons.len(), bar).into_iter().zip(buttons) {
            out.push(PanelZone {
                row: y,
                x0: bar.x,
                x1: bar.x.saturating_add(bar.width).saturating_sub(1),
                command: b.command,
            });
        }
        return out;
    }
    let names = shows_names(app, &buttons, bar);
    for b in buttons {
        let width = u16::try_from(norte_frontend::panelbar::button_cell(&b, names).width)
            .unwrap_or(u16::MAX);
        let end = x.saturating_add(width);
        // A button that does not fit WHOLE is neither painted nor
        // clickable: half a letter is not a button. Same criterion as the
        // menu titles.
        if end > bar.x.saturating_add(bar.width) {
            break;
        }
        out.push(PanelZone {
            row: bar.y,
            x0: x,
            x1: end.saturating_sub(1),
            command: b.command,
        });
        x = end;
    }
    out
}

/// Paints the panel bar: which panels there are, how they are, and with
/// which key.
pub(crate) fn draw_panel_bar(frame: &mut Frame<'_>, app: &App) {
    use norte_frontend::panelbar::PanelState;
    let Some(bar) = crate::ui::geometry::panel_bar_visible(app, frame.area()) else {
        return;
    };
    clear_themed(frame, bar, &app.theme);
    if crate::ui::geometry::bar_in_column(app) {
        draw_rail(frame, app, bar);
        return;
    }
    let mut spans: Vec<ratatui::text::Span<'static>> = Vec::new();
    let mut width = 0_u16;
    let buttons = panel_buttons(app, frame.area());
    let names = shows_names(app, &buttons, bar);
    // In a COLUMN (spec 2026-09-21) each button is a line with its letter
    // cell — `" S·"`, three wide — and names do not fit.
    let column = crate::ui::geometry::bar_in_column(app);
    let names = names && !column;
    let mut lines: Vec<ratatui::text::Line<'static>> = Vec::new();
    for (i, b) in buttons.into_iter().enumerate() {
        let cell = norte_frontend::panelbar::button_cell(&b, names);
        let button_w = u16::try_from(cell.width).unwrap_or(u16::MAX);
        if column {
            if u16::try_from(i).unwrap_or(u16::MAX) >= bar.height {
                break;
            }
        } else if width.saturating_add(button_w) > bar.width {
            break;
        }
        let from = spans.len();
        // Three styles for three states. A panel having the KEYBOARD is not
        // the same as it being open, and it is half of what is being asked
        // when looking at the bar: where are my keys going to go.
        let style = match b.state {
            PanelState::Focused => app.theme.role(Role::Selection),
            PanelState::Open => app.theme.role(Role::Title),
            // OFF, not another color: the bar's base text, dimmed.
            //
            // It used to be `Role::StatusBar`, which is the STATUS BAR's
            // style — in half the themes, a bright background and dark
            // text. This bar clears with the base background, so CLOSED
            // buttons came out as lit blocks on it and OPEN ones as normal
            // text: the visual weight, exactly backwards. Looking at it
            // answered the opposite of what you asked, which is what makes
            // it seem like state is acting on its own.
            //
            // The menu next to it never fell into this: it uses `Title` for
            // what is not open and `Selection` for what is, and never
            // another surface's role.
            PanelState::Closed => app
                .theme
                .role(Role::Regular)
                .add_modifier(ratatui::style::Modifier::DIM),
        };
        // The letter ALWAYS keeps its state's style, and the attention mark
        // is a separate span. Painting the whole button as a warning — as
        // the first version did — took away the reader's answer to "where
        // are my keys going to go?" right while something was happening,
        // which is when it is asked the most.
        //
        // The mark goes INSIDE the button's width (it takes the space on
        // the right) so the row does not change size depending on what
        // happens: a bar that dances reads worse than a fixed one.
        //
        // With names, the access letter is UNDERLINED inside the name
        // (spec 2026-09-10): three spans — before, the letter, after — and
        // the same state style on all three.
        let underlined = style.add_modifier(ratatui::style::Modifier::UNDERLINED);
        let before: String = cell.text.chars().take(cell.letter_at).collect();
        let letter: String = cell.text.chars().skip(cell.letter_at).take(1).collect();
        let after: String = cell.text.chars().skip(cell.letter_at + 1).collect();
        spans.push(ratatui::text::Span::styled(format!(" {before}"), style));
        spans.push(ratatui::text::Span::styled(letter, underlined));
        spans.push(ratatui::text::Span::styled(after, style));
        spans.push(if b.attention > 0 {
            ratatui::text::Span::styled("·", app.theme.role(Role::Warning))
        } else {
            ratatui::text::Span::styled(" ", style)
        });
        if column {
            lines.push(ratatui::text::Line::from(spans.split_off(from)));
        }
        width = width.saturating_add(button_w);
    }
    if !column {
        lines.push(ratatui::text::Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), bar);
}

/// The panel COLUMN (ADR 0140), the way VS Code's activity bar does it:
/// three cells per row — the focus rule, the icon and the badge — and air
/// between icons if it fits ([`rail_rows`]).
///
/// - The panel with the KEYBOARD carries the `▎` rule in the focus color
///   and a lit icon; an open one, the lit icon with no rule; a closed one,
///   a dimmed icon. The same scale as the window.
/// - The badge is the COUNT (tasks, warnings) in the warning color, and `+`
///   past nine: one cell does not allow for more.
/// - The icon comes from `[ui] panel_bar_style`: Unicode with `names` or
///   `icons`, Nerd Fonts with `nerd`, the letter with `letters`. A panel
///   with no icon — a plugin's — paints its letter.
fn draw_rail(frame: &mut Frame<'_>, app: &App, bar: Rect) {
    use norte_frontend::panelbar::{IconSet, PanelState};
    use ratatui::style::Modifier;
    use ratatui::text::{Line, Span};
    let buttons = panel_buttons(app, frame.area());
    let set = match app.chrome.panel_bar_style() {
        norte_config::PanelBarStyle::Letters => None,
        norte_config::PanelBarStyle::Nerd => Some(IconSet::Nerd),
        norte_config::PanelBarStyle::Names | norte_config::PanelBarStyle::Icons => {
            Some(IconSet::Unicode)
        }
    };
    let rows = rail_rows(buttons.len(), bar);
    for (y, b) in rows.into_iter().zip(buttons) {
        let off = app.theme.role(Role::Regular).add_modifier(Modifier::DIM);
        let on = app.theme.role(Role::Title).add_modifier(Modifier::BOLD);
        let (rule, icon_style) = match b.state {
            PanelState::Focused => (Span::styled("▎", app.theme.role(Role::BorderFocus)), on),
            PanelState::Open => (Span::raw(" "), on),
            PanelState::Closed => (Span::raw(" "), off),
        };
        let glyph = set
            .and_then(|j| norte_frontend::panelbar::icon(&b.kind, j))
            .map_or_else(|| b.letter.to_string(), str::to_owned);
        let badge = match b.attention {
            0 => Span::raw(" "),
            n @ 1..=9 => Span::styled(n.to_string(), app.theme.role(Role::Warning)),
            _ => Span::styled("+", app.theme.role(Role::Warning)),
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                rule,
                Span::styled(glyph, icon_style),
                badge,
            ])),
            Rect {
                y,
                height: 1,
                ..bar
            },
        );
    }
}

/// Paints the menu bar and its dropdown.
pub(crate) fn draw_menu(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    // The BAR is painted with the menu open or closed: pinned, its job is
    // to say the menu exists. The dropdown, obviously, only when open — and
    // that is why titles are measured separately, without going through
    // `menu_geom`.
    let open = app.menu.as_ref();
    let titles = menu_titles(area);
    let bar = Rect { height: 1, ..area };
    clear_themed(frame, bar, &app.theme);
    // A title that does not fit WHOLE is not painted halfway: at forty
    // columns the bar used to end in "Bus," which is not a menu, it is
    // noise. The last one that does not fit is dropped and that is it —
    // what the bar has to say is that a menu EXISTS, and the first ones are
    // enough for that.
    let spans: Vec<ratatui::text::Span<'static>> = titles
        .iter()
        .filter(|(_, _, x1)| *x1 < area.x.saturating_add(area.width))
        .enumerate()
        .map(|(i, (label, _, _))| {
            let style = if open.is_some_and(|st| i == st.menu()) {
                app.theme.role(Role::Selection)
            } else {
                app.theme.role(Role::Title)
            };
            ratatui::text::Span::styled(label.clone(), style)
        })
        .collect();
    frame.render_widget(Paragraph::new(ratatui::text::Line::from(spans)), bar);

    let reserve = draw_layout_buttons(frame, app, area, bar);

    // And the key that opens it, on the right, PULLED FROM THE LIVE KEYMAP.
    //
    // A bar that shows seven titles and does not say how to enter them
    // leaves the reader with the mouse as the only door. The key is not
    // written by hand — it is `alt+m` in some presets and something else in
    // whichever ones someone rebound — so it comes from wherever the
    // dropdown items' come from.
    //
    // Only with the menu CLOSED: open, the key is no longer needed and that
    // slot is wanted by the title further to the right.
    if open.is_none()
        && let Some(chord) = app
            .palette_rows
            .iter()
            .find(|r| r.key == "app.menu")
            .map(|r| r.chord.clone())
    {
        let text = format!("{chord} ");
        let w = u16::try_from(UnicodeWidthStr::width(text.as_str())).unwrap_or(0);
        let used = titles
            .iter()
            .filter(|(_, _, x1)| *x1 < area.x.saturating_add(area.width))
            .map(|(l, _, _)| u16::try_from(UnicodeWidthStr::width(l.as_str())).unwrap_or(0))
            .sum::<u16>();
        // Only if it fits WITHOUT stepping on the titles: a menu's name is
        // worth more than its shortcut, and half a shortcut is worth
        // nothing.
        // To the left of the buttons, if there are any.
        if bar.width > used.saturating_add(w).saturating_add(reserve) {
            let hint = Rect {
                x: bar
                    .x
                    .saturating_add(bar.width)
                    .saturating_sub(w)
                    .saturating_sub(reserve),
                width: w,
                ..bar
            };
            frame.render_widget(
                Paragraph::new(ratatui::text::Line::styled(
                    text,
                    app.theme.role(Role::Info),
                )),
                hint,
            );
        }
    }

    let (Some(st), Some(g)) = (open, menu_geom(app, area)) else {
        return;
    };
    clear_themed(frame, g.drop, &app.theme);
    let inner = Block::default().borders(Borders::ALL).inner(g.drop);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(app.theme.role(Role::BorderFocus)),
        g.drop,
    );
    let width = usize::from(inner.width);
    let lines: Vec<ratatui::text::Line<'static>> = g
        .lines
        .iter()
        .map(|line| menu_line(app, line, width, st.item()))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    // A section's rule joins the border (`├───┤`), as in mc: floating
    // between two `│` it reads as an underline, not as a division.
    let right = g.drop.x.saturating_add(g.drop.width).saturating_sub(1);
    for (i, line) in g.lines.iter().enumerate() {
        let row = inner.y.saturating_add(u16::try_from(i).unwrap_or(u16::MAX));
        if !matches!(line, MenuLine::Section(_)) || row >= inner.y.saturating_add(inner.height) {
            continue;
        }
        let buf = frame.buffer_mut();
        for (x, s) in [(g.drop.x, "├"), (right, "┤")] {
            if let Some(c) = buf.cell_mut((x, row)) {
                c.set_symbol(s);
            }
        }
    }
}

/// A line of the dropdown, painted at `width` cells; `cursor` is the
/// highlighted command.
fn menu_line(
    app: &App,
    line: &MenuLine,
    width: usize,
    cursor: usize,
) -> ratatui::text::Line<'static> {
    match line {
        MenuLine::Section(None) => {
            ratatui::text::Line::styled("─".repeat(width), app.theme.role(Role::Separator))
        }
        // The label in the dimmed style, between rules: it reads as a group
        // header, not as one more command that does nothing.
        MenuLine::Section(Some(t)) => {
            let t = super::text::take_width(t, width.saturating_sub(4));
            let rest = width.saturating_sub(UnicodeWidthStr::width(t.as_str()) + 3);
            ratatui::text::Line::from(vec![
                ratatui::text::Span::styled("─ ", app.theme.role(Role::Separator)),
                ratatui::text::Span::styled(t, app.theme.role(Role::Muted)),
                ratatui::text::Span::styled(
                    format!(" {}", "─".repeat(rest)),
                    app.theme.role(Role::Separator),
                ),
            ])
        }
        MenuLine::Item {
            index,
            label,
            chord,
            role,
        } => {
            use norte_frontend::menu::ItemRole;
            let mark = if *role == ItemRole::Ai { AI_MARK } else { "" };
            let slot = width
                .saturating_sub(UnicodeWidthStr::width(label.as_str()))
                .saturating_sub(UnicodeWidthStr::width(mark))
                .saturating_sub(UnicodeWidthStr::width(chord.as_str()));
            let text = format!("{label}{mark}{}{chord}", " ".repeat(slot));
            // The danger color on what deletes, except under the cursor:
            // there the selection wins, since that is what says WHERE you
            // are.
            let style = if *index == cursor {
                app.theme.role(Role::Selection)
            } else if *role == ItemRole::Destructive {
                app.theme.role(Role::Error)
            } else {
                app.theme.role(Role::Regular)
            };
            ratatui::text::Line::styled(text, style)
        }
    }
}

/// What can be clicked in a tab bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabAction {
    /// Go to tab `n` (0-based).
    Goto(usize),
    /// Open a tab.
    New,
    /// Close the active one.
    Close,
}

/// A clickable zone of a panel's tab bar.
///
/// Computed from the SAME spot that paints the bar, for the same reason as
/// the listing's geometry: a range guessed by eye resolves the click to the
/// tab next door, and that does not look like a mouse bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabZone {
    /// The panel's visible position.
    pub pane: usize,
    /// Row where the bar is.
    pub row: u16,
    /// First column, inclusive.
    pub x0: u16,
    /// Last column, inclusive.
    pub x1: u16,
    /// What clicking it does.
    pub action: TabAction,
}

/// The open-tab button. ASCII: a `+` in a box cannot measure two cells on
/// any given terminal, and a `⊕` can.
pub(crate) const TAB_NEW: &str = "[+]";

/// The button to close the active one.
pub(crate) const TAB_CLOSE: &str = "[x]";

/// The bar's pieces, each with its width and what clicking it does.
pub(crate) fn tab_pieces(t: &TabStrip) -> Vec<(String, TabAction)> {
    let mut v: Vec<(String, TabAction)> = t
        .titles
        .iter()
        .enumerate()
        .map(|(i, title)| (format!(" {title} "), TabAction::Goto(i)))
        .collect();
    v.push((TAB_NEW.to_owned(), TabAction::New));
    v.push((TAB_CLOSE.to_owned(), TabAction::Close));
    v
}

/// The clickable zones of panels with tabs, in `area`'s frame.
///
/// Lives next to the painting — and not in the mouse module — for the same
/// reason as [`super::geometry::pane_geometry`]: whoever knows where
/// everything landed is the `draw`.
#[must_use]
pub fn tab_zones(app: &App, area: Rect) -> Vec<TabZone> {
    let cols = pane_rects(app, area);
    let mut out = Vec::new();
    for (pane, rect) in cols.iter().enumerate() {
        let Some(t) = tab_strip_for(app, pane) else {
            continue;
        };
        // The bar is the block interior's FIRST row.
        let row = rect.y.saturating_add(1);
        let mut x = rect.x.saturating_add(1);
        let ceiling = rect.x.saturating_add(rect.width).saturating_sub(1);
        for (text, action) in tab_pieces(&t) {
            let w = u16::try_from(UnicodeWidthStr::width(text.as_str())).unwrap_or(0);
            if w == 0 || x >= ceiling {
                break;
            }
            let x1 = x.saturating_add(w).saturating_sub(1).min(ceiling - 1);
            out.push(TabZone {
                pane,
                row,
                x0: x,
                x1,
                action,
            });
            x = x.saturating_add(w);
        }
    }
    out
}

/// Mark for the TARGET panel in its title. ASCII on purpose, like the
/// hostile badge: a unicode arrow is ambiguous-width and would take two
/// cells on many terminals.
pub(crate) const TARGET_BADGE: &str = "->";

/// Paints the tab bar if there is one, and returns where the column header
/// and the listing land.
///
/// With tabs, the interior's FIRST row is the bar and everything else moves
/// down one: that is why `pane_chrome_rows` counts the same thing, and the
/// anchor test contrasts it against the buffer.
pub(crate) fn draw_tab_strip(
    frame: &mut Frame<'_>,
    inner: Rect,
    tabs: Option<&TabStrip>,
    theme: &TuiTheme,
) -> (Rect, Rect) {
    let bar = u16::from(tabs.is_some());
    if let Some(t) = tabs
        && inner.height > 0
    {
        let mut bar_area = inner;
        bar_area.height = 1;
        frame.render_widget(Paragraph::new(tab_strip_line(t, theme)), bar_area);
    }
    let mut header = inner;
    header.y = inner.y.saturating_add(bar);
    header.height = 1;
    let mut list = inner;
    list.y = inner.y.saturating_add(bar).saturating_add(1);
    list.height = inner.height.saturating_sub(bar).saturating_sub(1);
    (header, list)
}

/// The tab bar's line.
pub(crate) fn tab_strip_line<'a>(t: &TabStrip, theme: &TuiTheme) -> ratatui::text::Line<'a> {
    // The SAME pieces `tab_zones` measures: if both computed them on their
    // own, a click would resolve to the tab next door.
    let spans = tab_pieces(t)
        .into_iter()
        .map(|(text, action)| {
            let style = if action == TabAction::Goto(t.active) {
                theme.role(Role::Selection)
            } else {
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM)
            };
            ratatui::text::Span::styled(text, style)
        })
        .collect::<Vec<_>>();
    ratatui::text::Line::from(spans)
}
