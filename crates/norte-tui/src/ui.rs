//! Render ratatui del estado (`app`): cero lógica de negocio — pinta lo que
//! hay. El marcado de nombres hostiles sigue la spec §6 (lossy y MARCADO).

use norte_proto::EntryKind;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::app::{App, Pane, display_name, path_display};

/// Badge de nombre hostil: PREFIJO en columna fija (al final moriría en el
/// truncado por ancho de ratatui y el nombre se pintaría "limpio") y en
/// ASCII (`⚠` es ambiguous-width: 2 celdas en muchos terminales). Va
/// estilado (bold) — fuera de banda: un archivo llamado "! x" no lo imita.
const HOSTILE_BADGE: &str = "!";

/// Pinta el frame completo: dos panes + barra de estado.
pub fn draw(frame: &mut Frame<'_>, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);
    for (i, pane) in app.panes.iter().enumerate() {
        draw_pane(frame, cols[i], pane, app.focus() == i);
    }
    draw_status(frame, rows[1], app);
}

fn draw_pane(frame: &mut Frame<'_>, area: Rect, pane: &Pane, focused: bool) {
    let border_style = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    let (title, title_hostil) = path_display(&pane.dir);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(if title_hostil {
            format!("{HOSTILE_BADGE} {title}")
        } else {
            title
        });
    let items: Vec<ListItem<'_>> = pane.entries.iter().map(entry_item).collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if !pane.entries.is_empty() {
        state.select(Some(pane.cursor));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn entry_item(entry: &norte_proto::Entry) -> ListItem<'_> {
    let name = entry.path.file_name().map_or(&[][..], |n| n.as_bytes());
    let (texto, hostil) = display_name(name);
    let marker = match entry.kind {
        EntryKind::Dir => "/",
        EntryKind::Symlink => "@",
        EntryKind::File | EntryKind::Other => " ",
    };
    let badge = Span::styled(
        if hostil { HOSTILE_BADGE } else { " " },
        Style::default().add_modifier(Modifier::BOLD),
    );
    ListItem::new(Line::from(vec![
        badge,
        Span::raw(format!("{marker}{texto}")),
    ]))
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let pane = app.focused();
    let total = pane.entries.len();
    let pos = if total == 0 { 0 } else { pane.cursor + 1 };
    // Strings de UI hardcodeados: Fluent llega en la fase 9 (issue #1).
    let (dir_texto, dir_hostil) = path_display(&pane.dir);
    let marca = if dir_hostil { HOSTILE_BADGE } else { "" };
    let text =
        format!(" {marca}{dir_texto}  {pos}/{total}  Tab:pane  Enter:entrar  Bksp:subir  q:salir");
    frame.render_widget(
        Paragraph::new(text).style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
    );
}
