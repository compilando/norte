//! The side panels: tree, processes, metadata and tasks, plus the viewer,
//! the preview and the places list.
//!
//! Each one occupies a slot of the layout and paints what is in its model;
//! none decides where it goes.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use super::text::{head, middle, two_fields, with_badge};
use super::{HOSTILE_BADGE, placed_of_kind, rect_del_visor, resolved_for, visor_split};
use crate::app::{App, display_name};
use crate::theme::TuiTheme;
use norte_i18n::{t, ta};

/// The two scrollbars of a framed viewer, over its borders.
///
/// The viewer used to say "1/4813" in the status bar and nothing else, so
/// whether there was more ABOVE or to the RIGHT could only be known by
/// counting. The horizontal one matters twice as much: the viewer does not
/// wrap, and a file clipped on the right reads as a short file.
///
/// `area` is the WHOLE frame; the bars are painted over its borders and
/// span only the interior, so the thumb's position matches the first and
/// last row of text. Neither is painted when everything fits
/// ([`super::help::render_scrollbar`]).
///
/// `with_horizontal` is `false` in the DOCKED viewer: its bottom border
/// carries the status line — encoding, EOL, losses, truncation — which is
/// the only place that slot says WHAT is being viewed. Covering it with a
/// bar would trade a fact for a hint.
fn viewer_scrollbars(
    frame: &mut Frame<'_>,
    area: Rect,
    viewer: &crate::viewer::Viewer,
    theme: &TuiTheme,
    with_horizontal: bool,
) {
    // With no room for the frame there is no interior to report on.
    if area.width < 3 || area.height < 3 {
        return;
    }
    let height = usize::from(area.height - 2);
    let width = usize::from(area.width - 2);
    super::help::render_scrollbar(
        frame,
        Rect {
            x: area.x + area.width - 1,
            y: area.y + 1,
            width: 1,
            height: area.height - 2,
        },
        theme,
        viewer.total_rows(),
        viewer.scroll,
        height,
    );
    if with_horizontal {
        super::help::render_hscrollbar(
            frame,
            Rect {
                x: area.x + 1,
                y: area.y + area.height - 1,
                width: area.width - 2,
                height: 1,
            },
            theme,
            viewer.max_cols(),
            viewer.hscroll(),
            width,
        );
    }
}

/// Full-screen viewer: content + its own status (encoding, EOL, losses,
/// truncation — the user ALWAYS knows what they are looking at, spec §6).
pub(crate) fn draw_viewer(frame: &mut Frame<'_>, viewer: &crate::viewer::Viewer, app: &App) {
    // Over the BODY's area, not the frame's: the viewer paints full-screen
    // and does not go through the slot layout, so with the menu bar pinned
    // it used to land underneath it and the bar covered its first row. The
    // same subtraction the layout does, in the one other place that paints
    // full-screen. `visor_split` — not a copy of `Layout::split` — is the
    // SAME calculation the run loop (T4) uses to know where the pixels go:
    // two calculations of the same slot diverge silently.
    let (content_area, status_area) = visor_split(app, frame.area());
    let (title, hostile) =
        norte_frontend::path_display_with(&viewer.path, app.focused().name_encoding());
    let title = if hostile {
        format!("{HOSTILE_BADGE} {title}")
    } else {
        title
    };
    // M4-P5: "via <plugin>" indicator when the view comes from a plugin
    // preview (plugin_name already arrives masked from the viewer).
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(app.theme.role(Role::Title))
        .border_style(app.theme.role(Role::BorderFocus));
    if let Some(plugin) = viewer.preview_plugin() {
        // #101: when the host-side decoding was LOSSY, a warning (role
        // Warning) FOLLOWS the "via …" — same honesty as the raw viewer's
        // encoding status, and same order as the GUI (`viewer_header`).
        // ASCII (`⚠` is ambiguous-width).
        let mut spans = vec![Span::styled(
            ta("viewer-plugin-preview", &[("plugin", plugin)]),
            app.theme.role(Role::Info),
        )];
        if viewer.preview_lossy() {
            spans.push(Span::styled(
                format!(" {}", t("viewer-plugin-preview-lossy")),
                app.theme.role(Role::Warning),
            ));
        }
        block = block.title(Line::from(spans).right_aligned());
    }
    // T5 (phase 5 WOW): with no plugin preview, a PNG in `Modo::Blocks`
    // falls back to hexview just like a file nobody knows how to interpret
    // — nothing on screen told the two cases apart until the pilot found
    // it.
    //
    // Branch review, finding 3: `modo` is NOT recomputed here — it is read
    // from `App::viewer_modo`, the same value `viewer_open::open_viewer`
    // resolved when the viewer OPENED. Recomputing it every frame against
    // `app.chrome.images()` (what the first version did) is what produced
    // the bug: `[ui] images` reloads LIVE
    // (`config_reload::reload_config` reassigns `app.chrome` whole,
    // `applies_live` in `norte-frontend::settings`), so a change from
    // `blocks` to `kitty` with the viewer already open made this `match`
    // switch to `Modo::Kitty` for a file that NEVER asked for a thumbnail —
    // the "the thumbnail extension needs approving" warning on a file that
    // never asked for one, lying about what is actually needed (reopening,
    // not approving anything). `App::viewer_modo` only changes when the
    // viewer opens again, or when `reload_config` drops a `Kitty` thumbnail
    // that stopped being one (see there for why it is one-directional).
    //
    // The placement loop (T4, `event_loop.rs`) does not need to look at
    // `modo` on its own: `ui::image_to_place` only sees something to
    // place while `App::viewer_imagen` stays alive, and `reload_config`
    // already drops it the moment the mode stops being `Kitty` — a single
    // cut point instead of a check repeated every frame.
    //
    // It does NOT go in the header title even though the "via …" above
    // lives there: the right title right-aligns WITHOUT clipping when it
    // does not fit, so the warning's long text (with the F12 hint) used to
    // eat the whole left title — a real regression, caught by
    // `snapshot_viewer_text_and_hex`. The bottom status bar is full width and
    // already yields the whole spot to `app.message` when there is one;
    // this warning follows the same pattern.
    //
    // Task 5b (T6 review finding): the same hole existed in `Modo::Kitty` —
    // with no `thumbnail` plugin approved, `viewer_for_width` never places
    // `App::viewer_imagen` and the viewer falls back to hexview as silently
    // as `Modo::Blocks` with no previewer. Which condition makes the
    // warning unnecessary depends on the mode — `Modo::Blocks`'s
    // `previewer` and `Modo::Kitty`'s `thumbnail` are two different
    // plugins, with separate documentation in
    // `viewer_open::no_need_to_warn_about_image` /
    // `no_need_to_warn_about_thumbnail` (with the "simplify it to"
    // `preview_plugin().is_some()` trap written on the first one); that
    // `match` decides which applies.
    let modo = app.viewer_modo;
    let no_warning_needed = match modo {
        crate::viewer_open::Modo::Kitty => {
            crate::viewer_open::no_need_to_warn_about_thumbnail(viewer, app.viewer_imagen.as_ref())
        }
        crate::viewer_open::Modo::Blocks | crate::viewer_open::Modo::Nothing => {
            crate::viewer_open::no_need_to_warn_about_image(viewer)
        }
    };
    // Against the viewer's path: a thumbnail rejected for the PREVIOUS file
    // says nothing about this one (the same trap
    // `no_need_to_warn_about_thumbnail` documents for the already-placed
    // image).
    let foreign_format = app
        .viewer_thumbnail_foreign
        .as_ref()
        .is_some_and(|p| *p == viewer.path);
    let image_warning = crate::viewer_open::image_notice(modo, no_warning_needed, foreign_format);
    // `rect_del_visor` — the SAME function the run loop uses for the APC,
    // not a hand-written subtraction — is what guarantees that the slot
    // left blank below and the slot where the pixels land are structurally
    // the same one (review, CRITICAL 2).
    let inner_h = rect_del_visor(app, frame.area()).height as usize;
    // T4 (phase 5 WOW): with the image PLACED (the run loop paints its
    // pixels after this frame, outside ratatui), the lines go EMPTY. The
    // terminal is going to paint OVER this slot, and text there would show
    // UNDER the pixels or flicker as it alternates with them every frame.
    // The frame, the title and the scrollbars below keep painting the same
    // — none of this changes because an image is placed.
    // Branch review, finding 2: `image_to_place` (not a check separate
    // from the `path`) is the SAME function the run loop uses to decide
    // whether to place pixels — this line used to only look at `path`, and
    // the run loop additionally added `!something_above_the_viewer` and that the
    // rect not be empty; with an overlay that does not cover the whole
    // screen (the menu, which-key…) this painter used to blank the slot as
    // always while the run loop refused to place pixels over it: neither
    // image nor hexview.
    let has_image = super::image_to_place(app, frame.area()).is_some();

    // #29/G3a (ADR 0037): a plugin preview carries color, either through
    // sanitized ANSI-SGR (`fg` only) or through structured WIT (`role`
    // VALIDATED + fallback `fg`). `role` WINS over `fg` when both are
    // present (the user's theme takes precedence over a plugin's fixed
    // color, ADR 0037 decision 3) — it is resolved through the theme
    // (`app.theme.role`), not as raw RGB. With neither, the theme's default
    // color (no `.style()`).
    let lines: Vec<Line<'_>> = if has_image {
        Vec::new()
    } else {
        match viewer.plugin_styled_rows(inner_h) {
            Some(styled) => styled
                .into_iter()
                .map(|line| {
                    Line::from(
                        line.iter()
                            .map(|span| {
                                Span::raw(span.text.clone()).style(span_style(span, &app.theme))
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect(),
            None => viewer.rows(inner_h).into_iter().map(Line::raw).collect(),
        }
    };
    frame.render_widget(Paragraph::new(lines).block(block), content_area);
    viewer_scrollbars(frame, content_area, viewer, &app.theme, true);
    let pos = format!(
        "{}/{}",
        (viewer.scroll + 1).min(viewer.total_rows().max(1)),
        viewer.total_rows().max(1)
    );
    let text: Line<'_> = match &app.message {
        Some(msg) => Line::styled(format!(" {msg}"), app.theme.role(Role::StatusBar)),
        None => match image_warning {
            // Fix round 1, IMPORTANT 2: with no previewer approved is the
            // DEFAULT state of any install, so this branch is the common
            // case, not the rare one — losing `pos` here loses it exactly
            // where it is noticed most (a big PNG in hexview, scrolling
            // with no guide but the bar's thumb).
            Some(notice) => Line::styled(format!(" {notice}  {pos}"), app.theme.role(Role::Info)),
            None => Line::styled(
                format!(" {}  {pos}", crate::viewer::status(viewer)),
                app.theme.role(Role::StatusBar),
            ),
        },
    };
    frame.render_widget(Paragraph::new(text), status_area);
}

/// The DOCKED viewer (L3): the file under the cursor, in its slot.
///
/// The same renderer as [`draw_viewer`] — same rows, same plugin preview —
/// inside a block the size of the slot instead of the whole screen. The
/// viewer's status line (encoding, EOL, losses, truncation) goes on the
/// bottom border: it is the only place that says WHAT is being viewed, and
/// a viewer that does not say so lies by omission.
///
/// With no file, the slot carries text: a directory, an empty listing, or
/// the reason the read could not be done. A denial is PAINTED here and
/// opens nothing — the preview follows the cursor, so a dialog per
/// keystroke would turn going down a directory into a burst of modals.
pub(crate) fn draw_preview(
    frame: &mut Frame<'_>,
    area: Rect,
    preview: &crate::preview::Preview,
    with_keyboard: bool,
    app: &App,
) {
    let border = if with_keyboard {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let Some(viewer) = preview.viewer() else {
        let text = preview.note().unwrap_or_default().to_owned();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", t("preview-title")))
            .title_style(app.theme.role(Role::Title))
            .border_style(app.theme.role(border));
        frame.render_widget(
            Paragraph::new(Line::styled(text, app.theme.role(Role::Info))).block(block),
            area,
        );
        return;
    };
    let (title, hostile) =
        norte_frontend::path_display_with(&viewer.path, app.focused().name_encoding());
    let title = if hostile {
        format!("{HOSTILE_BADGE} {title}")
    } else {
        title
    };
    let width = usize::from(area.width.saturating_sub(2));
    let mut block = Block::default()
        .borders(Borders::ALL)
        // The path is clipped in the MIDDLE: in a narrow slot what
        // identifies a file is its name, i.e. the tail.
        .title(norte_frontend::middle_ellipsis(&title, width))
        .title_style(app.theme.role(Role::Title))
        .border_style(app.theme.role(border))
        .title_bottom(Line::raw(norte_frontend::middle_ellipsis(
            &crate::viewer::status(viewer),
            width,
        )));
    if let Some(plugin) = viewer.preview_plugin() {
        block = block.title(
            Line::from(Span::styled(
                ta("viewer-plugin-preview", &[("plugin", plugin)]),
                app.theme.role(Role::Info),
            ))
            .right_aligned(),
        );
    }
    let inner_h = area.height.saturating_sub(2) as usize;
    let lines: Vec<Line<'_>> = match viewer.plugin_styled_rows(inner_h) {
        Some(styled) => styled
            .into_iter()
            .map(|line| {
                Line::from(
                    line.iter()
                        .map(|span| {
                            Span::raw(span.text.clone()).style(span_style(span, &app.theme))
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect(),
        None => viewer.rows(inner_h).into_iter().map(Line::raw).collect(),
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
    // Vertical only: the bottom border belongs to the status line.
    viewer_scrollbars(frame, area, viewer, &app.theme, false);
}

/// The style of a styled span, whether from a preview, a viewer or a
/// plugin panel.
///
/// The ROLE beats the color (ADR 0037, decision 3): the reader's theme
/// takes precedence over a fixed color a third party asks for. No role
/// commands the background (proto 0.66.0, D4), because a half block with no
/// background is half an image.
///
/// A single copy: this was written word for word in the viewer and in the
/// preview, and the plugin panel would have been the third — three places
/// to change a role's precedence.
fn span_style(span: &norte_frontend::ansi::StyledSpan, theme: &TuiTheme) -> Style {
    let mut style = if let Some(role) = span.role {
        theme.role(role)
    } else if let Some((r, g, b)) = span.fg {
        Style::default().fg(Color::Rgb(r, g, b))
    } else {
        Style::default()
    };
    if let Some((r, g, b)) = span.bg {
        style = style.bg(Color::Rgb(r, g, b));
    }
    style
}

/// The panel that paints a PLUGIN (phase 3): its frame, inside a house
/// border.
///
/// The border, the title and the focus are set by norte; what is inside is
/// described by the guest. That boundary is what keeps a plugin from
/// pretending to be another panel: it cannot paint its own border nor write
/// in the title.
///
/// With no frame yet — the first request is still in flight, or the plugin
/// failed — the slot is painted EMPTY with its border: it is known to be
/// there and whose it is. What it never does is flicker, because a frame
/// that arrived is kept while the next one is requested.
pub(crate) fn draw_plugin_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    id: norte_frontend::layout::SlotId,
    with_keyboard: bool,
) {
    let border = if with_keyboard {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    // The kind comes from the TREE — a layout file, `--layout` or the
    // session — and nobody has demanded an alphabet from it there:
    // `validate` looks at the shape and keeps the kinds this binary does
    // not know (ADR 0059). The alphabet is demanded when it is DECLARED,
    // which is a different path. So it is masked like any name coming from
    // a file.
    let title = app
        .layout
        .kind_of(id)
        .and_then(|k| crate::panelplugin::parts(k.as_str()).map(|(_, kind)| kind.to_owned()))
        .map(|k| norte_frontend::display_name(k.as_bytes()).0)
        .unwrap_or_default();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .title_style(app.theme.role(Role::Title))
        .border_style(app.theme.role(border));
    let lines: Vec<Line<'_>> = app
        .panels
        .get(id)
        .and_then(|p| p.frame.as_ref())
        .map(|f| frame_lines(f, &app.theme))
        .unwrap_or_default();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// A [`StyledFrame`](norte_frontend::frame::StyledFrame) converted into
/// `ratatui` lines.
///
/// ONE conversion and two callers — a plugin's panel and the disk map —
/// because a second hand-written one is exactly how a path that skipped the
/// masking slipped through (see `crate::ansi::span_de_wire`). What comes in
/// already arrives sanitized; all this does is style.
fn frame_lines<'a>(marco: &norte_frontend::frame::StyledFrame, theme: &TuiTheme) -> Vec<Line<'a>> {
    marco
        .lines
        .iter()
        .map(|line| {
            Line::from(
                line.iter()
                    .map(|span| Span::raw(span.text.clone()).style(span_style(span, theme)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// The disk map (phase 4): what the directory is made of, in rectangles.
///
/// The frame is laid out by [`norte_frontend::treemap::squarify`] with the
/// width and height INSIDE the border: the layout does not know where the
/// slot landed, just like a plugin's guest does not, and whoever paints
/// does the arithmetic.
///
/// While it is measuring, whatever has arrived is painted — a map builds up
/// gradually — and the title says so. A half-finished map that does not say
/// so reads as a small directory, which is the wrong answer.
pub(crate) fn draw_disk_map(
    frame: &mut Frame<'_>,
    area: Rect,
    map: &norte_frontend::diskmap::DiskMap,
    app: &App,
    with_keyboard: bool,
) {
    use norte_frontend::diskmap::State;

    let border = if with_keyboard {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    // The title carries the STATUS, which is half the information:
    // "measuring" over a half-finished map is what keeps it from being read
    // as a total.
    let the_state = match map.state() {
        // Idle and Done add nothing, and it is the SAME result on purpose:
        // one is "nobody has asked for anything" and the other "it is
        // already done", and in both the title is enough on its own. What
        // needs to show is when it is NOT finished, because a half-finished
        // map that does not say so reads as a total.
        State::Idle | State::Done => String::new(),
        State::Measuring(_) => format!(" — {}", t("disk-map-measuring")),
        State::Failure(motivo) => format!(" — {motivo}"),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {}{the_state} ", t("disk-map-title")))
        .title_style(app.theme.role(Role::Title))
        .border_style(app.theme.role(border));
    let inside = block.inner(area);
    let marco =
        norte_frontend::treemap::squarify(&map.report().children, inside.width, inside.height);
    let lines = frame_lines(&marco, &app.theme);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The places sidebar (L3): drives and favorites in a panel that stays put.
///
/// Everything the platform gives us — a volume's label, its mount point,
/// the name the user gave a favorite — goes through the same masking as the
/// drives popup (`display_name`/`path_display`): a `fuse.<subtype>` is
/// chosen by an unprivileged user, and a FAT label is as hostile as a file
/// name.
///
/// A broken favorite is painted DIMMED and with its translated reason,
/// never hidden: a favorite that disappears is only an invisible config
/// failure.
pub(crate) fn draw_places(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &norte_frontend::places::PlacesState,
    with_keyboard: bool,
    theme: &TuiTheme,
) {
    use norte_frontend::places::PlaceRow;

    let border = if with_keyboard {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("places-title")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let width = inner.width as usize;
    let items: Vec<ListItem<'_>> = state
        .rows()
        .iter()
        .map(|row| match row {
            PlaceRow::Header { section, folded } => {
                let arrow = if *folded { '▸' } else { '▾' };
                ListItem::new(Line::styled(
                    head(&format!("{arrow} {}", t(section.label_key())), width),
                    theme.role(Role::Title),
                ))
            }
            PlaceRow::Drive {
                label, mount, free, ..
            } => {
                // The SHORT name, same as the window's (2026-09-21
                // screenshot): the whole clipped mount used to be five
                // "/home/oscar/…" rows that could not be told apart.
                let (entry_name, hostile) = norte_frontend::places::drive_name(label, mount);
                // Short and with no decimals: fourteen cells have to carry
                // the mount's name AND its space. A `?` when the filesystem
                // did not answer — never a zero, which would read as
                // "full" (the whole word is still said by the popup, which
                // does have room).
                let is_free =
                    free.map_or_else(|| "?".to_owned(), norte_frontend::human_bytes_short);
                // A mount's name is clipped in the MIDDLE: what identifies
                // `/home/oscar/.cache` is the tail, and with six mounts
                // under `/home` a list clipped from the front is six rows
                // saying the same thing.
                ListItem::new(Line::raw(two_fields(
                    &with_badge(&entry_name, hostile),
                    &is_free,
                    width,
                    middle,
                )))
            }
            PlaceRow::Favorite { name, target } => {
                let (text, hostile) = display_name(name.as_bytes());
                let left = with_badge(&text, hostile);
                match target {
                    Ok(_) => ListItem::new(Line::raw(head(&format!(" {left}"), width))),
                    // Broken: a `!` mark and a DIMMED row. The whole reason
                    // does not fit in fourteen cells — "invalid path" is
                    // thirteen — and clipping it would leave half a word
                    // saying nothing, so the row says THAT it is broken and
                    // the status bar says why when the cursor lands on it.
                    // What is not done is hiding it: a favorite that
                    // disappears is only an invisible config failure.
                    Err(_) => ListItem::new(Line::styled(
                        two_fields(&left, "!", width, head),
                        theme.role(Role::Info),
                    )),
                }
            }
        })
        .collect();
    let list = List::new(items).highlight_style(theme.role(Role::Selection));
    // The scroll is computed HERE and not decided by the widget, so that
    // `places_zones` can say which model row each screen row corresponds
    // to (#226). It is the same number ratatui chose on its own — minimum
    // scroll so the cursor is visible, starting from zero every frame — so
    // the screen does not change; what changes is that there is now ONE
    // source and the mouse can read it.
    let mut the_state = ListState::default().with_offset(if with_keyboard {
        places_offset(state.cursor(), inner.height as usize)
    } else {
        0
    });
    the_state.select(with_keyboard.then(|| state.cursor()));
    frame.render_stateful_widget(list, inner, &mut the_state);
}

/// First model row that is visible, for a cursor and a height.
///
/// MINIMUM scroll so the cursor is in view, starting from zero: it is what
/// the widget did with a fresh `ListState` every frame, written down so the
/// mouse's hit test does not have to guess it.
pub(crate) const fn places_offset(cursor: usize, height: usize) -> usize {
    cursor.saturating_sub(height.saturating_sub(1))
}

/// A clickable row of the places sidebar, in `area`'s frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceZone {
    /// Screen row.
    pub row: u16,
    /// First column, inclusive.
    pub x0: u16,
    /// Last column, inclusive.
    pub x1: u16,
    /// Index within [`norte_frontend::places::PlacesState::rows`].
    pub index: usize,
}

/// The sidebar's clickable rows, in `area`'s frame.
///
/// Lives next to the painting and shares its layout and scroll with it —
/// same as [`super::tab_zones`] and for the same reason: measuring on one
/// side and painting on the other is how a click ends up activating the row
/// next door.
#[must_use]
pub fn places_zones(app: &App, area: Rect) -> Vec<PlaceZone> {
    let res = resolved_for(app, area);
    let Some((id, rect)) = placed_of_kind(&res, &app.layout, "places") else {
        return Vec::new();
    };
    let Some(state) = app.panes.places(id) else {
        return Vec::new();
    };
    // The block's interior: the frame is not clickable.
    let inner = Block::default().borders(Borders::ALL).inner(rect);
    if inner.width == 0 || inner.height == 0 {
        return Vec::new();
    }
    let offset = if app.key_owner() == crate::app::KeyOwner::Places {
        places_offset(state.cursor(), inner.height as usize)
    } else {
        0
    };
    (0..inner.height as usize)
        .filter_map(|row| {
            let index = offset.checked_add(row)?;
            if index >= state.rows().len() {
                return None;
            }
            Some(PlaceZone {
                row: inner
                    .y
                    .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
                x0: inner.x,
                x1: inner.x.saturating_add(inner.width).saturating_sub(1),
                index,
            })
        })
        .collect()
}

/// A clickable row of the tree, in `area`'s frame (#136).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeZone {
    /// Screen row.
    pub row: u16,
    /// First column, inclusive.
    pub x0: u16,
    /// Last column, inclusive.
    pub x1: u16,
    /// This row's MARK column (`▾`/`▸`/`·`), which folds and unfolds it.
    /// Indented by depth, same as `draw_tree` paints it.
    pub mark_x: u16,
    /// Index within [`norte_frontend::tree::Tree::rows`].
    pub index: usize,
}

/// The tree's clickable rows, in `area`'s frame.
///
/// Lives next to the painting and shares its layout and scroll with it,
/// same as [`places_zones`] and for the same reason: measuring on one side
/// and painting on the other is how a click ends up opening the branch next
/// door.
#[must_use]
pub fn tree_zones(app: &App, area: Rect) -> Vec<TreeZone> {
    let res = resolved_for(app, area);
    let Some((id, rect)) = placed_of_kind(&res, &app.layout, crate::tree::KIND) else {
        return Vec::new();
    };
    let Some(tree) = app.panes.tree(id) else {
        return Vec::new();
    };
    let inner = Block::default().borders(Borders::ALL).inner(rect);
    if inner.width == 0 || inner.height == 0 {
        return Vec::new();
    }
    let rows = tree.rows();
    // The tree ALWAYS paints its cursor, whether it has the keyboard or not
    // (unlike the sidebar), so its scroll does not depend on who is typing.
    let offset = places_offset(tree.cursor(), inner.height as usize);
    (0..inner.height as usize)
        .filter_map(|row| {
            let index = offset.checked_add(row)?;
            let the_row = rows.get(index)?;
            // `  ` per level, then the mark: the same mold as `draw_tree`.
            let indent = u16::try_from(the_row.depth.saturating_mul(2)).unwrap_or(u16::MAX);
            Some(TreeZone {
                row: inner
                    .y
                    .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
                x0: inner.x,
                x1: inner.x.saturating_add(inner.width).saturating_sub(1),
                mark_x: inner.x.saturating_add(indent),
                index,
            })
        })
        .collect()
}

/// A task's percentage, or `0` if it is not known yet.
///
/// The ARITHMETIC lives in `norte_frontend::tasks`: the graphical window
/// paints the same board, and two copies of the same calculation already
/// diverged once — the other one did not fall back to entries, so a delete
/// stayed at zero.
///
/// Here "not known" is painted as zero because the bar has to measure
/// something; the status next to it is what says whether the task is
/// alive.
pub(crate) fn progress_pct(p: &norte_proto::TaskProgress) -> u64 {
    norte_frontend::tasks::progress_pct(p).map_or(0, u64::from)
}

/// A task class's label, in the UI's language: the one both frontends use
/// (#375), for both surfaces that paint it — the bottom strip and the
/// processes panel.
pub(crate) fn kind_label(kind: norte_proto::TaskKind) -> String {
    norte_frontend::tasks::kind_label(norte_i18n::active(), kind)
}

/// What a board row acts ON, clipped to `max` cells.
///
/// The path is clipped in the MIDDLE because what identifies a file is its
/// name, i.e. the tail — the same criterion as the docked viewer's title.
/// Empty when the task published no entry (a search that has not touched
/// anything yet), and then the row is left with its class and its status,
/// which is what is known.
///
/// The name reinterpretation is the FOCUSED pane's. It is not exact — the
/// operand can come from the other pane — but a hostile name painted as raw
/// bytes cannot be read, and the badge next to it says there was a
/// reinterpretation.
fn operand_text(row: &crate::tasks::TaskRow, app: &App, max: usize) -> String {
    let Some(p) = row.operand.as_ref() else {
        return String::new();
    };
    let (text, hostile) = norte_frontend::path_display_with(p, app.focused().name_encoding());
    let text = if hostile {
        format!("{HOSTILE_BADGE} {text}")
    } else {
        text
    };
    norte_frontend::middle_ellipsis(&text, max)
}

/// The processes panel (phase A): one row per task, with a bar and status.
///
/// The rows come from the `TaskBoard` that already paints the strip — this
/// panel does not keep a second list — and the cursor is bounded HERE
/// against this frame's rows: a task can finish and disappear between two
/// paints.
/// The directory tree (#136).
///
/// One name per row, indented by depth, with a three-state indicator:
/// expanded, folded-with-children, and unread. The third one matters —
/// painting "leaf" on something that has not been listed yet would be
/// inventing the answer — and it is the same criterion as the rest of the
/// screen: what is not known is said, not filled in.
///
/// Names go through `display_name`, like the listing: a directory with bidi
/// or invisibles does not reorder this column.
pub(crate) fn draw_tree(
    frame: &mut Frame<'_>,
    area: Rect,
    tree: &crate::tree::Tree,
    app: &App,
    with_keyboard: bool,
) {
    let theme = &app.theme;
    let border = if with_keyboard {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("tree-title")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let rows = tree.rows();
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(t("tree-loading"), theme.role(Role::Title))),
            inner,
        );
        return;
    }
    let cursor = tree.cursor();
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .map(|r| {
            let mark = match (r.expanded, r.children) {
                (true, _) => "▾",
                (false, Some(true)) => "▸",
                // Read and with no children: a real leaf.
                (false, Some(false)) => " ",
                // Unread: neither leaf nor branch, yet.
                (false, None) => "·",
            };
            let (name, hostile) = display_name(
                r.path
                    .file_name()
                    .map_or(b"/".as_slice(), norte_proto::Segment::as_bytes),
            );
            let indent = "  ".repeat(r.depth);
            let text = if hostile {
                format!("{indent}{mark} {HOSTILE_BADGE} {name}")
            } else {
                format!("{indent}{mark} {name}")
            };
            ListItem::new(Line::raw(text))
        })
        .collect();
    // The scroll is computed HERE and not decided by the widget, so that
    // `tree_zones` can say which model row each screen row corresponds to.
    // It is the same number ratatui chose on its own — minimum scroll so
    // the cursor is visible, starting from zero every frame — so the screen
    // does not change; what changes is that there is now ONE source and the
    // mouse can read it (same fix as #226 in the sidebar).
    let mut list_state =
        ListState::default().with_offset(places_offset(cursor, inner.height as usize));
    list_state.select(Some(cursor));
    let list = List::new(items).highlight_style(theme.role(Role::Selection));
    frame.render_stateful_widget(list, inner, &mut list_state);
}

pub(crate) fn draw_processes(
    frame: &mut Frame<'_>,
    area: Rect,
    // By VALUE: since the type lives in `norte-frontend` it is a named
    // `usize`, and eight bytes are cheaper to copy than to reference.
    processes: crate::processes::Processes,
    app: &App,
    with_keyboard: bool,
) {
    let theme = &app.theme;
    let border = if with_keyboard {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("processes-title")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    // That this panel has the KEYBOARD used to be said only by the border's
    // color, and a reader who cannot tell that pair of colors apart — or
    // who does not know that pair means that — sees a file manager where
    // the arrows have stopped working and has nowhere to start. It is the
    // same lesson as #111: a color-only signal is not a signal.
    //
    // The footer says the way OUT, not the status: "has focus" helps
    // nobody, "Esc gives the keyboard back" does.
    if with_keyboard {
        block = block.title_bottom(Line::styled(
            format!(" {} ", t("processes-has-keyboard")),
            theme.role(Role::Info),
        ));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let rows = app.board.rows();
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(t("processes-empty"), theme.role(Role::Title))),
            inner,
        );
        return;
    }
    let cursor = processes.row_or_zero(&app.board.task_ids());
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let p = &row.last;
            let pct = progress_pct(p);
            // Ten cells of bar: it fits in a narrow panel and still says at
            // a glance how far along it is.
            let full = usize::try_from(pct / 10).unwrap_or(0).min(10);
            let bar: String = "█".repeat(full) + &"░".repeat(10 - full);
            let (state_txt, role) = match &p.state {
                norte_proto::TaskState::Completed => ("✓".to_owned(), Some(Role::Info)),
                norte_proto::TaskState::Cancelled => (t("task-cancelled"), Some(Role::Warning)),
                // Paused (ADR 0147): the percentage it stopped at, and that
                // it is stopped — a plain still number reads as stuck.
                norte_proto::TaskState::Paused => (format!("⏸ {pct}%"), Some(Role::Warning)),
                norte_proto::TaskState::Failed { .. } => (t("task-failed"), Some(Role::Error)),
                _ => (format!("{pct}%"), None),
            };
            // The task number is NOT painted: eighteen digits tell nobody
            // anything and eat the width the name needs. What was missing
            // was the pair "what kind of work" + "on what", which already
            // travels whole in the progress line.
            let kind = kind_label(p.kind);
            // Rate and time remaining (spec 2026-09-15, phase 2): neither
            // comes from the wire — the board estimates them from its own
            // snapshots — and both stay silent when unknown. A bar with no
            // speed says something is happening; with it, it says whether
            // waiting is worth it.
            let pace = norte_frontend::tasks::human_rate(row.rate.bps());
            let remains = norte_frontend::tasks::human_eta(row.rate.eta_secs(p));
            let measured = match (pace.is_empty(), remains.is_empty()) {
                (true, true) => String::new(),
                (false, true) => format!("{pace} "),
                (true, false) => format!("{remains} "),
                (false, false) => format!("{pace} · {remains} "),
            };
            // The row's fixed width: the mark, the class, the bar, the
            // status and the FOUR spaces that separate them. What is left
            // over belongs to the operand, and if nothing is left over it
            // stays empty instead of pushing anything out.
            //
            // `ratatui` clips the line to the width without saying
            // anything, so going one over does not break the paint: it eats
            // the trailing `✓`, which is exactly the fact the row exists to
            // give.
            let fixed = 1
                + 4
                + kind.chars().count()
                + 10
                + state_txt.chars().count()
                + measured.chars().count();
            let slot = usize::from(inner.width).saturating_sub(fixed);
            let operating = operand_text(row, app, slot);
            let mark = if i == cursor { '▶' } else { ' ' };
            let header = if operating.is_empty() {
                format!("{mark} {kind} {bar} {measured}")
            } else {
                format!("{mark} {kind} {operating} {bar} {measured}")
            };
            let tail = match role {
                Some(r) => Span::styled(state_txt, theme.role(r)),
                None => Span::raw(state_txt),
            };
            ListItem::new(Line::from(vec![Span::raw(header), tail]))
        })
        .collect();
    frame.render_widget(List::new(items), inner);
}

/// The attribute sheet (phase A): what is known about the entry under the
/// cursor.
///
/// Everything comes from the `Entry` the listing already had, so this
/// function cannot ask for anything even if it wanted to. WHICH rows go in
/// is decided by [`norte_frontend::metadata::sheet`], the same one the
/// window uses: this only paints them. When each frontend had its own copy
/// of the list they had already diverged — the fix that flags a hostile
/// attribute value was applied to only one — and that is exactly the
/// failure a copy produces.
pub(crate) fn draw_metadata(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: Option<&(norte_proto::Entry, bool)>,
    follows: Option<&(String, bool)>,
    app: &App,
    with_keyboard: bool,
) {
    let theme = &app.theme;
    let border = if with_keyboard {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    // The title says WHICH LISTING it follows, not just that it is the
    // sheet: with two listings open, a bare "Details" does not say whose
    // details they are, and the only way to find out was to move the
    // cursor and watch whether the sheet moved. The path is clipped in the
    // middle and with a mark, like any other path in this file: the
    // block's border does not warn of a clip.
    let title = match follows {
        Some((path, hostile)) => format!(
            " {} · {} ",
            t("metadata-title"),
            norte_frontend::middle_ellipsis(
                &with_badge(path, *hostile),
                (area.width as usize).saturating_sub(t("metadata-title").chars().count() + 6),
            )
        ),
        None => format!(" {} ", t("metadata-title")),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let Some((e, is_parent_row)) = entry else {
        frame.render_widget(
            Paragraph::new(Line::styled(t("metadata-empty"), theme.role(Role::Title))),
            inner,
        );
        return;
    };

    let catalog = app.attr_catalog(e.path.scheme());
    let width = inner.width as usize;
    let lines: Vec<Line<'_>> =
        norte_frontend::metadata::sheet(e, *is_parent_row, catalog, norte_i18n::active())
            .into_iter()
            .map(|f| {
                // The value is clipped in the MIDDLE and with a mark. A
                // `Paragraph` with no wrap clips on the right without
                // saying so, and the `Destination` field is a whole path:
                // in a narrow panel
                // `⟨file⟩/home/oscar/projects/norte-secret` used to be left
                // as `⟨file⟩/home/oscar/projects`, which is another
                // directory that also exists. It is the same rule as the
                // rest of this file's paths.
                let label = format!("{} ", f.label);
                let place = width.saturating_sub(crate::ui::text::cells(&label));
                let valor =
                    norte_frontend::middle_ellipsis(&with_badge(&f.value, f.hostile), place);
                Line::from(vec![
                    Span::styled(label, theme.role(Role::Title)),
                    Span::raw(valor),
                ])
            })
            .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

pub(crate) fn draw_tasks(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    let lines: Vec<Line<'_>> = app
        .board
        .rows()
        .iter()
        .rev()
        .take(area.height as usize)
        .map(|row| {
            let p = &row.last;
            let pct = progress_pct(p);
            // By CATEGORY (stable Display), never Debug facing the user.
            // The status is colored by role (error red, done info).
            let (state, role) = match &p.state {
                norte_proto::TaskState::Completed => ("✓".to_owned(), Some(Role::Info)),
                norte_proto::TaskState::Cancelled => (t("task-cancelled"), Some(Role::Warning)),
                // Paused (ADR 0147): the percentage it stopped at, and that
                // it is stopped — a plain still number reads as stuck.
                norte_proto::TaskState::Paused => (format!("⏸ {pct}%"), Some(Role::Warning)),
                norte_proto::TaskState::Failed { error } => {
                    (format!("✗ {error}"), Some(Role::Error))
                }
                _ => (format!("{pct}%"), None),
            };
            let kind = kind_label(p.kind);
            // Same as the panel: the class and the operand, not the id. The
            // strip used to say " copy #7318349021 45% ", which is the same
            // line for any copy of anything.
            let fixed = 1 + kind.chars().count() + 2 + state.chars().count();
            let slot = usize::from(area.width).saturating_sub(fixed);
            let operating = operand_text(row, app, slot);
            let head = if operating.is_empty() {
                Span::raw(format!(" {kind} "))
            } else {
                Span::raw(format!(" {kind} {operating} "))
            };
            let tail = match role {
                Some(r) => Span::styled(state, app.theme.role(r)),
                None => Span::raw(state),
            };
            Line::from(vec![head, tail])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// A log line's time, from the SHARED module.
///
/// It used to live here until the window needed the same one (#326): two
/// ideas of what time it is in each frontend's log panel is the kind of
/// difference nobody notices until they compare two screenshots.
use norte_frontend::format::time_utc;

/// The log panel (#323): what is happening, without leaving the TUI.
/// The terminal panel (#362): the shell's grid inside its frame.
///
/// The content is FOREIGN — another program paints it — and that is why it
/// carries nothing of the theme on top: the colors are the ones the shell
/// asked for, and an index resolves it against the reader's emulator
/// palette, as if the program ran outside norte. The only thing that is
/// ours is the frame.
///
/// That nothing needs masking here is not an oversight: the grid guarantees
/// it, where a control byte cannot reach a cell.
pub(crate) fn draw_terminal(frame: &mut Frame<'_>, area: Rect, app: &App, with_keyboard: bool) {
    let theme = &app.theme;
    let border = if with_keyboard {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("panelbar-terminal")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    // The footer says how to GET OUT, and only when the keyboard is inside:
    // it is the only key the panel does not pass to the shell, so it is the
    // only one that has to be announced — and without announcing it, a
    // reader who comes in with every key taken has no way to deduce it.
    if with_keyboard && let Some(c) = app.terminal_chord {
        block = block.title_bottom(Line::styled(
            format!(" {c} · {} ", t("terminal-leave")),
            theme.role(Role::Muted),
        ));
    }
    let inside = block.inner(area);
    frame.render_widget(block, area);
    let Some(term) = app.terminal.as_ref() else {
        // With no shell the slot is still useful: it says there is none. An
        // empty panel with no explanation is what makes a panel distrusted.
        frame.render_widget(
            Paragraph::new(Line::styled(t("terminal-none"), theme.role(Role::Muted))),
            inside,
        );
        return;
    };
    let p = term.screen();
    frame.render_widget(Paragraph::new(crate::termpanel::rows(p)), inside);
    if let Some((x, y)) = crate::termpanel::cursor_en(p, inside, with_keyboard) {
        frame.set_cursor_position((x, y));
    }
}

pub(crate) fn draw_log(frame: &mut Frame<'_>, area: Rect, app: &App, with_keyboard: bool) {
    use std::fmt::Write as _;
    let theme = &app.theme;
    let border = if with_keyboard {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let panel = &app.log_panel;
    let source = crate::logview::source_effective(app);
    // The title says the level, the SOURCE and the filter: without that, a
    // panel that looks empty cannot tell "nothing has happened" apart from
    // "you are filtering it out" or from "you are looking at the other
    // process's log", which is the confusion that makes a log viewer
    // distrusted.
    //
    // The level is ALWAYS the one being SHOWN, across every source: it is
    // the one the key controls and the one that filters the list. Marking
    // here the one the daemon answered having set would be the worst
    // possible mistake for the panel — with the daemon on `trace` and the
    // panel on `info`, the header would say `trace` while every `debug`
    // line crossing the socket is silently dropped.
    let mut title = format!(" {} · {} ", t("log-title"), panel.level().label().trim());
    // The source only when there are two places a line could come from.
    // With no daemon there is no segment and nothing is missing: a plain
    // `ntc` has one process and one ring, and a sentence about the origin
    // would answer a question nobody asked — exactly the panel #326 left
    // behind.
    if let Some(source_txt) = crate::logview::source_label(app, source) {
        let _ = write!(title, "· {source_txt} ");
    }
    // If some ring is capturing MORE than what is shown, it is said, and
    // with both in view each part says whom it is talking about. Asking for
    // TRACE and going back to INFO leaves the process capturing TRACE for
    // the rest of the session — on purpose, so that going and coming back
    // does not erase what happened in between — and without this line that
    // shows up nowhere.
    let capture = crate::logview::capture_note(app, source);
    if !capture.is_empty() {
        let _ = write!(title, "· {capture} ");
    }
    if !panel.filter().is_empty() {
        let _ = write!(
            title,
            "· /{} ",
            norte_encoding::mask_terminal_hazards(panel.filter())
        );
    }
    // What was discarded is SAID, per ring: the local one counts what has
    // been evacuated since the process started, the daemon's what THIS
    // opening lost. They are different numbers and do not add up. A ring
    // that silently drops the old stuff makes the reader look for a line
    // that was there and no longer is, and conclude the log is lying.
    let descartes = crate::logview::discard_note(app, source);
    if !descartes.is_empty() {
        let _ = write!(title, "· {descartes} ");
    }
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    // The footer: either the filter being typed, or the keys. The field WINS
    // because while it is being typed it is the only thing that matters,
    // and because a cursor that cannot be seen is a field that does not
    // look like a field.
    if let Some(input) = &app.log_filter_input {
        block = block.title_bottom(Line::styled(
            format!(" /{}▏", norte_encoding::mask_terminal_hazards(input)),
            theme.role(Role::Match),
        ));
    } else if with_keyboard {
        // The source key is only offered when there IS a second one:
        // announcing a control that would cycle through three views of the
        // same ring is promising something that does not exist. It is the
        // same rule as in the window, where the selector simply is not
        // painted.
        let keys = if app.log_remote.service == crate::logview::Service::Serves {
            format!("{} · {}", t("log-keys"), t("log-keys-source"))
        } else {
            t("log-keys")
        };
        block = block.title_bottom(Line::styled(format!(" {keys} "), theme.role(Role::Info)));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if app.log_ring.is_none() && source == norte_frontend::logpanel::LogSource::Window {
        // With no ring installed (tests, or an embedder that did not mount
        // the subscriber) and no daemon serving its own, it is said,
        // instead of painting an empty panel that looks like nothing is
        // happening. It is not "nothing is being logged": the process keeps
        // writing to its file; what is missing is the in-memory ring, which
        // is what this panel reads.
        frame.render_widget(
            Paragraph::new(Line::styled(t("log-no-ring"), theme.role(Role::Warning))),
            inner,
        );
        return;
    }
    // Both sources, mixed by timestamp and already filtered (#328).
    // Borrowed, not cloned: the ring already cloned once in its `snapshot`
    // and here at most one screen is painted.
    let lines = crate::logview::snapshot(app);
    let visible = crate::logview::visible(app, &lines);
    let alto = usize::from(inner.height);
    let from = panel.window_start(visible.len(), alto);
    if visible.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(t("log-empty"), theme.role(Role::Title))),
            inner,
        );
        return;
    }
    let painted: Vec<Line<'_>> = visible
        .iter()
        .skip(from)
        .take(alto)
        .map(|(l, origin)| {
            let rol = match l.level {
                norte_config::logline::LogLevel::Error => Role::Error,
                norte_config::logline::LogLevel::Warn => Role::Warning,
                _ => Role::Info,
            };
            // The module and the message carry paths and host names chosen
            // by someone who is not the reader: they go through the same
            // masking as any other foreign text before touching the
            // terminal.
            let body =
                norte_encoding::mask_terminal_hazards(&format!("{}: {}", l.target, l.message));
            // A DAEMON line is flagged in the margin, and only with both
            // sources on screen: with just one there is nothing to tell
            // apart, and the rule would spend two columns per line to say
            // nothing. A rule and not a color, same as in the window: the
            // color is already taken by the level, which is what is
            // searched for at a glance.
            let margin = match (source, origin) {
                (
                    norte_frontend::logpanel::LogSource::Both,
                    norte_frontend::logpanel::LogSource::Daemon,
                ) => "│ ",
                (norte_frontend::logpanel::LogSource::Both, _) => "  ",
                _ => "",
            };
            Line::from(vec![
                Span::styled(margin, theme.role(Role::BorderUnfocused)),
                Span::raw(format!("{} ", time_utc(l.epoch_ms))),
                Span::styled(format!("{} ", l.level.label()), theme.role(rol)),
                Span::raw(body),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(painted), inner);
}

/// The color of a timeline row according to WHO did it (phase 7).
///
/// What it splits is "me" from "something in my name", which is the
/// distinction a human needs to read their own history: theirs can be
/// undone from here; an agent's or a plugin's cannot — that goes through
/// `policy.undo_session`, which is a different screen and a different
/// question.
#[must_use]
pub(crate) fn rol_de_actor(actor_kind: &str) -> Role {
    match actor_kind {
        norte_frontend::timeline::ACTOR_HUMAN => Role::Regular,
        "agent" => Role::Warning,
        _ => Role::Info,
    }
}

/// A timeline row, already paintable.
///
/// `time · dot · verb · name`, and behind that what sets it apart: how many
/// entries it carries if it is a batch, and whether it cannot be undone. The
/// dot is what carries the actor's color — the text stays legible and the
/// class reads at a glance through the column, which is what a timeline is
/// for.
fn timeline_line<'a>(
    the_row: &norte_frontend::timeline::TimelineRow,
    theme: &TuiTheme,
    width: usize,
) -> Line<'a> {
    use std::fmt::Write as _;

    let mut spans = vec![
        Span::styled(
            format!("{} ", norte_frontend::format::time_utc(the_row.ts_ms)),
            theme.role(Role::BorderUnfocused),
        ),
        Span::styled("● ", theme.role(rol_de_actor(&the_row.actor_kind))),
    ];
    // The badge, UP FRONT and in its own span, as on every surface where
    // something is decided: the server has already masked the text, and
    // this is what keeps it from being read as faithful.
    if the_row.hostile {
        spans.push(Span::styled(
            format!("{HOSTILE_BADGE} "),
            theme.role(Role::HostileBadge),
        ));
    }
    let mut cola = String::new();
    if the_row.members > 1 {
        let _ = write!(
            cola,
            " · {}",
            ta("timeline-batch", &[("n", &the_row.members.to_string())])
        );
    }
    if !the_row.reversible {
        let _ = write!(cola, " · {}", t("timeline-irreversible"));
    }
    let used = spans.iter().map(Span::width).sum::<usize>() + super::text::cells(&cola);
    let verb = format!("{} ", the_row.op);
    let place = width
        .saturating_sub(used + super::text::cells(&verb))
        .max(1);
    spans.push(Span::raw(verb));
    spans.push(Span::raw(norte_frontend::display::middle_ellipsis(
        &norte_frontend::timeline::path_label(&the_row.path),
        place,
    )));
    if !cola.is_empty() {
        spans.push(Span::styled(cola, theme.role(Role::BorderUnfocused)));
    }
    Line::from(spans)
}

/// The journal's timeline (phase 7): one row per mutation — or per batch —
/// from the newest to the oldest, with the cursor over the one that would
/// be the point to go back to.
pub(crate) fn draw_timeline(
    frame: &mut Frame<'_>,
    area: Rect,
    timeline: &norte_frontend::timeline::Timeline,
    app: &App,
    with_keyboard: bool,
) {
    let theme = &app.theme;
    let border = if with_keyboard {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("timeline-title")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    // The footer says what an Enter is going to take HERE, not in general:
    // it is the only number that matters before pressing it, and having it
    // in front while the cursor moves is what turns the list into a
    // decision.
    if with_keyboard && !timeline.is_empty() {
        let c = timeline.summary();
        let text = if c.no_does_nothing() {
            t("timeline-undo-nothing")
        } else {
            ta("timeline-undo-count", &[("n", &c.to_undo.to_string())])
        };
        block = block.title_bottom(Line::styled(format!(" {text} "), theme.role(Role::Info)));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if timeline.is_empty() {
        // "Nothing has been done yet" is only said once it HAS BEEN
        // CHECKED. A panel inherited from a saved layout has not asked yet,
        // and stating there that the journal is empty is the worst possible
        // mistake on a history screen.
        let text = if timeline.loaded() {
            t("timeline-empty")
        } else {
            t("timeline-loading")
        };
        frame.render_widget(
            Paragraph::new(Line::styled(text, theme.role(Role::Title))),
            inner,
        );
        return;
    }
    let alto = usize::from(inner.height);
    // The same window as any long list in this binary: what fits is
    // painted, not the whole history (`draw_pane`, and the reason measured
    // there).
    let window = super::pane::painted_rows(
        timeline.cursor().saturating_sub(alto.saturating_sub(1) / 2),
        timeline.len(),
        alto,
    );
    let width = usize::from(inner.width);
    let items: Vec<ListItem<'_>> = timeline
        .rows()
        .iter()
        .skip(window.start)
        .take(window.len())
        .map(|f| ListItem::new(timeline_line(f, theme, width)))
        .collect();
    let list = List::new(items).highlight_style(theme.role(if with_keyboard {
        Role::Selection
    } else {
        Role::SelectionUnfocused
    }));
    let mut state = ListState::default();
    state.select(timeline.cursor().checked_sub(window.start));
    *state.offset_mut() = 0;
    frame.render_stateful_widget(list, inner, &mut state);
}

#[cfg(test)]
mod draw_log_tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Paints the log panel and returns what was left in the buffer.
    fn painted(app: &App, width: u16, alto: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, alto)).expect("test terminal");
        terminal
            .draw(|f| draw_log(f, f.area(), app, true))
            .expect("draw");
        terminal.backend().to_string()
    }

    /// A painted row's two margin columns.
    ///
    /// Two things have to be peeled off first: the quote `TestBackend`'s
    /// `Display` adds, and the block's LEFT border, which is another `│`
    /// and which this test's first version confused with the daemon's
    /// rule — declaring a line from this terminal flagged as remote.
    fn margin(the_row: &str) -> String {
        the_row
            .chars()
            .skip_while(|c| *c == '"')
            .skip(1)
            .take(2)
            .collect()
    }

    /// With a separate daemon, BOTH sources reach the painted rows, and the
    /// daemon's is told apart by the margin rule (#328).
    ///
    /// It is the hole `ntc --socket` had: the providers, the journal, the
    /// policy and the reason a connection failed live in the other
    /// process, and this panel only showed the terminal's. The window
    /// already solved it, and fixing it in a single frontend is what makes
    /// them silently diverge (ADR 0077).
    #[test]
    fn the_panel_paints_the_terminal_and_the_daemon_and_tells_them_apart() {
        let mut app = crate::app::testutil::app_two_panes();
        let ring = norte_config::logring::LogRing::new(10);
        {
            use tracing_subscriber::layer::SubscriberExt as _;
            let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(&ring));
            tracing::subscriber::with_default(s, || tracing::info!("linea-de-la-terminal"));
        }
        let local_ms = ring.snapshot()[0].epoch_ms;
        app.log_ring = Some(ring);
        app.log_remote.hay_daemon = true;
        app.toggle_log();
        let epoch = app.log_remote.epoch;
        crate::logview::land_tail(
            &mut app,
            epoch,
            Ok(norte_proto::methods::LogTailResult {
                lines: vec![norte_proto::methods::LogLine {
                    epoch_ms: local_ms + 1,
                    level: "info".to_owned(),
                    target: "norte_core::daemon".to_owned(),
                    message: "linea-del-daemon".to_owned(),
                }],
                next: 7,
                lost: 0,
                // The daemon's level, HIGHER than the one the panel shows:
                // it is what makes the two-level rule visible.
                level: "trace".to_owned(),
                capacity: 2000,
            }),
        );

        let text = painted(&app, 120, 8);
        let row_local = text
            .lines()
            .find(|l| l.contains("linea-de-la-terminal"))
            .expect("this terminal's line was not painted");
        let row_daemon = text
            .lines()
            .find(|l| l.contains("linea-del-daemon"))
            .expect("the daemon's line was not painted");
        // The margin rule is what separates "the provider failed" from "the
        // terminal could not paint it", which read the same and are two
        // different failures.
        assert_eq!(
            margin(row_daemon),
            "│ ",
            "the daemon's line was not flagged in the margin: {row_daemon:?}"
        );
        assert_eq!(
            margin(row_local),
            "  ",
            "this terminal's line was flagged as the daemon's: {row_local:?}"
        );

        // The level that gets FLAGGED is the one being SHOWN, across every
        // source: the daemon's is said in the capture note, not in the
        // header. With the header saying `trace` while the filter stays on
        // `info`, every DEBUG line from the daemon would cross the socket
        // and be silently dropped.
        let header = text.lines().next().unwrap_or_default();
        assert!(
            header.contains(norte_config::logline::LogLevel::Info.label().trim()),
            "the header does not flag the level being shown: {header:?}"
        );
        assert!(
            text.contains(&norte_i18n::ta(
                "log-capturing-daemon",
                &[(
                    "level",
                    norte_config::logline::LogLevel::Trace.label().trim()
                )]
            )),
            "it does not say the daemon is capturing more than what is shown: {text}"
        );
    }

    /// With no daemon serving its log the source key is not offered:
    /// cycling through three views of the SAME ring is a control that
    /// promises something that does not exist.
    #[test]
    fn the_source_key_is_only_offered_when_there_are_two() {
        let mut app = crate::app::testutil::app_two_panes();
        app.log_ring = Some(norte_config::logring::LogRing::new(10));
        app.toggle_log();
        let sin = painted(&app, 120, 8);
        assert!(
            !sin.contains(&norte_i18n::t("log-keys-source")),
            "the source was offered with no second one to offer: {sin}"
        );

        app.log_remote.hay_daemon = true;
        app.log_remote.service = crate::logview::Service::Serves;
        let con = painted(&app, 120, 8);
        assert!(
            con.contains(&norte_i18n::t("log-keys-source")),
            "with a daemon, the source key is not announced: {con}"
        );
    }

    /// A plain `ntc` — with no daemon, which is the default startup —
    /// paints the panel EXACTLY as #326 left it: one process, one ring, and
    /// not a word about a source or a daemon.
    ///
    /// The segment's absence is the answer. Any phrase there would answer a
    /// question nobody asked, and the ones that existed — "the daemon logs
    /// separately", "this daemon does not serve its log" — talked about
    /// someone who does not exist.
    #[test]
    fn with_no_daemon_the_panel_is_326s() {
        let mut app = crate::app::testutil::app_two_panes();
        let ring = norte_config::logring::LogRing::new(10);
        {
            use tracing_subscriber::layer::SubscriberExt as _;
            let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(&ring));
            tracing::subscriber::with_default(s, || tracing::info!("una linea cualquiera"));
        }
        app.log_ring = Some(ring);
        app.toggle_log();

        let text = painted(&app, 120, 8);
        let header = text.lines().next().unwrap_or_default();
        assert!(
            text.contains("una linea cualquiera"),
            "the usual panel stopped painting: {text}"
        );
        for key in [
            "log-source-window",
            "log-source-both",
            "log-source-daemon",
            "log-source-unsupported",
            "log-source-daemon-level",
            "log-keys-source",
        ] {
            assert!(
                !text.contains(&norte_i18n::t(key)),
                "it talked about a daemon that does not exist ({key}): {text}"
            );
        }
        // And what does have to still be there: the title and the level.
        assert!(
            header.contains(&norte_i18n::t("log-title"))
                && header.contains(norte_config::logline::LogLevel::Info.label().trim()),
            "the title lost what was its own: {header:?}"
        );
    }
}
