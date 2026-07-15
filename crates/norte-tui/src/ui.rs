//! Render ratatui del estado (`app`): cero lógica de negocio — pinta lo que
//! hay. El marcado de nombres hostiles sigue la spec §6 (lossy y MARCADO). Los
//! colores salen del tema resuelto (`app.theme`, ADR 0020): un frontend sin
//! tema ve el fallback monocromo de M1.

use norte_proto::EntryKind;
use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::app::{App, Pane, display_name, path_display};
use crate::theme::TuiTheme;
use norte_i18n::t;

/// Badge de nombre hostil: PREFIJO en columna fija (al final moriría en el
/// truncado por ancho de ratatui y el nombre se pintaría "limpio") y en
/// ASCII (`⚠` es ambiguous-width: 2 celdas en muchos terminales). Va
/// estilado (rol `hostile-badge`) — fuera de banda: un archivo llamado "! x"
/// no lo imita.
const HOSTILE_BADGE: &str = "!";

/// Pinta el frame completo: panes (o viewer) + panel de tasks + barra de
/// estado + modal por encima.
pub fn draw(frame: &mut Frame<'_>, app: &App) {
    if let Some(viewer) = &app.viewer {
        draw_viewer(frame, viewer, app);
        return;
    }
    let tasks_h = u16::try_from(app.board.rows().len().min(6)).unwrap_or(6);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(tasks_h),
            Constraint::Length(1),
        ])
        .split(frame.area());
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);
    for (i, pane) in app.panes.iter().enumerate() {
        draw_pane(frame, cols[i], pane, app.focus() == i, &app.theme);
    }
    draw_tasks(frame, rows[1], app);
    draw_status(frame, rows[2], app);
    if let Some(modal) = &app.modal {
        draw_modal(frame, modal, &app.theme);
    }
    if let Some(help) = &app.help {
        draw_help(frame, help, &app.theme);
    }
}

/// Overlay de ayuda a pantalla (casi) completa, por encima de todo.
fn draw_help(frame: &mut Frame<'_>, help: &crate::app::Help, theme: &TuiTheme) {
    let area = centered(
        frame.area(),
        frame.area().width.saturating_sub(4).max(20),
        frame.area().height.saturating_sub(2).max(6),
    );
    frame.render_widget(ratatui::widgets::Clear, area);
    let inner_h = area.height.saturating_sub(2) as usize;
    let lines: Vec<Line<'_>> = help
        .lines
        .iter()
        .skip(help.scroll)
        .take(inner_h)
        .map(|l| Line::raw(l.as_str()))
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} — {} ", t("help-title"), t("help-hint")))
                .title_style(theme.role(Role::Title))
                .border_style(theme.role(Role::ModalBorder)),
        ),
        area,
    );
}

/// Viewer a pantalla completa: contenido + status propia (encoding, EOL,
/// pérdidas, truncado — el usuario SIEMPRE sabe qué mira, spec §6).
fn draw_viewer(frame: &mut Frame<'_>, viewer: &crate::viewer::Viewer, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());
    let (title, hostil) = path_display(&viewer.path);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(if hostil {
            format!("{HOSTILE_BADGE} {title}")
        } else {
            title
        })
        .title_style(app.theme.role(Role::Title))
        .border_style(app.theme.role(Role::BorderFocus));
    let inner_h = rows[0].height.saturating_sub(2) as usize;
    let lines: Vec<Line<'_>> = viewer.rows(inner_h).into_iter().map(Line::raw).collect();
    frame.render_widget(Paragraph::new(lines).block(block), rows[0]);
    let pos = format!(
        "{}/{}",
        (viewer.scroll + 1).min(viewer.total_rows().max(1)),
        viewer.total_rows().max(1)
    );
    let text = match &app.message {
        Some(msg) => format!(" {msg}"),
        None => format!(" {}  {pos}", viewer.status()),
    };
    frame.render_widget(
        Paragraph::new(text).style(app.theme.role(Role::StatusBar)),
        rows[1],
    );
}

fn draw_tasks(frame: &mut Frame<'_>, area: Rect, app: &App) {
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
            let pct = match (p.bytes_total, p.entries_total) {
                (Some(total), _) if total > 0 => {
                    (p.bytes_done.saturating_mul(100) / total).min(100)
                }
                (_, Some(total)) if total > 0 => {
                    (p.entries_done.saturating_mul(100) / total).min(100)
                }
                _ => 0,
            };
            // Por CATEGORÍA (Display estable), jamás Debug de cara al usuario.
            // El estado se colorea por rol (error rojo, hecho info).
            let (estado, role) = match &p.state {
                norte_proto::TaskState::Completed => ("✓".to_owned(), Some(Role::Info)),
                norte_proto::TaskState::Cancelled => (t("task-cancelled"), Some(Role::Warning)),
                norte_proto::TaskState::Failed { error } => {
                    (format!("✗ {error}"), Some(Role::Error))
                }
                _ => (format!("{pct}%"), None),
            };
            let kind = match p.kind {
                norte_proto::TaskKind::Copy => "copy",
                norte_proto::TaskKind::Move => "move",
                norte_proto::TaskKind::Delete => "delete",
            };
            let head = Span::raw(format!(" {kind} #{} ", p.task_id.get()));
            let tail = match role {
                Some(r) => Span::styled(estado, app.theme.role(r)),
                None => Span::raw(estado),
            };
            Line::from(vec![head, tail])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Caja centrada del modal.
fn draw_modal(frame: &mut Frame<'_>, modal: &crate::app::Modal, theme: &TuiTheme) {
    use crate::app::{Modal, TransferKind};
    let (titulo, cuerpo): (String, String) = match modal {
        Modal::ConfirmDelete { target, permanent } => (
            if *permanent {
                t("modal-delete-permanent-title")
            } else {
                t("modal-trash-title")
            },
            format!(
                "{}
{}
{}",
                path_display(target).0,
                if *permanent {
                    t("modal-delete-permanent-warning")
                } else {
                    t("modal-trash-note")
                },
                t("modal-confirm-keys")
            ),
        ),
        Modal::ConfirmTransfer { kind, from, to } => (
            match kind {
                TransferKind::Copy => t("modal-copy-title"),
                TransferKind::Move => t("modal-move-title"),
            },
            format!(
                "{}
→ {}
{}",
                path_display(from).0,
                path_display(to).0,
                t("modal-confirm-keys")
            ),
        ),
        Modal::Collision { retry } => (
            t("modal-collision-title"),
            format!(
                "{}
{}
{}",
                t("modal-collision-body"),
                path_display(&retry.to).0,
                t("modal-collision-keys")
            ),
        ),
    };
    // Un borrado PERMANENTE tiñe el borde de aviso (rol `warning`).
    let permanent = matches!(
        modal,
        Modal::ConfirmDelete {
            permanent: true,
            ..
        }
    );
    let border = if permanent {
        theme.role(Role::Warning)
    } else {
        theme.role(Role::ModalBorder)
    };
    let area = centered(frame.area(), 60, 6);
    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Paragraph::new(cuerpo).block(
            Block::default()
                .borders(Borders::ALL)
                .title(titulo)
                .title_style(theme.role(Role::Title))
                .border_style(border),
        ),
        area,
    );
}

fn centered(base: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(base.width);
    let h = h.min(base.height);
    Rect {
        x: base.x + (base.width - w) / 2,
        y: base.y + (base.height - h) / 2,
        width: w,
        height: h,
    }
}

fn draw_pane(frame: &mut Frame<'_>, area: Rect, pane: &Pane, focused: bool, theme: &TuiTheme) {
    let border_style = if focused {
        theme.role(Role::BorderFocus)
    } else {
        theme.role(Role::BorderUnfocused)
    };
    let (title, title_hostil) = path_display(&pane.dir);
    let mut title = if title_hostil {
        format!("{HOSTILE_BADGE} {title}")
    } else {
        title
    };
    // Un listado RELLENÁNDOSE (paginación, ADR 0017) se marca SIEMPRE: un
    // listado incompleto jamás es silencioso.
    if pane.loading {
        use std::fmt::Write as _;
        let _ = write!(
            title,
            " [{}]",
            norte_i18n::ta("pane-loading", &[("n", &pane.entries.len().to_string())])
        );
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title_style(theme.role(Role::Title))
        .title(title);
    let items: Vec<ListItem<'_>> = pane.entries.iter().map(|e| entry_item(e, theme)).collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    if !pane.entries.is_empty() {
        state.select(Some(pane.cursor));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn entry_item<'a>(entry: &'a norte_proto::Entry, theme: &TuiTheme) -> ListItem<'a> {
    let name = entry.path.file_name().map_or(&[][..], |n| n.as_bytes());
    let (texto, hostil) = display_name(name);
    let marker = match entry.kind {
        EntryKind::Dir => "/",
        EntryKind::Symlink => "@",
        EntryKind::File | EntryKind::Other => " ",
    };
    let badge = Span::styled(
        if hostil { HOSTILE_BADGE } else { " " },
        theme.role(Role::HostileBadge),
    );
    // Color por tipo/extensión de la entrada (ADR 0020 D2).
    let body = Span::styled(format!("{marker}{texto}"), theme.entry(name, entry.kind));
    ListItem::new(Line::from(vec![badge, body]))
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let pane = app.focused();
    let total = pane.entries.len();
    let pos = if total == 0 { 0 } else { pane.cursor + 1 };
    let (dir_texto, dir_hostil) = path_display(&pane.dir);
    let marca = if dir_hostil { HOSTILE_BADGE } else { "" };
    // Sin chuleta de teclas: mentiría según el preset (el which-key overlay
    // llega en fase 5). La secuencia pendiente SÍ se pinta (ADR 0006).
    let seq = if app.pending.is_empty() {
        String::new()
    } else {
        format!("  [{} …]", app.pending)
    };
    // Un mensaje pendiente (error por categoría, resultado) desplaza al
    // resto de la barra hasta la siguiente tecla (issue #20).
    let text = match &app.message {
        Some(msg) => format!(" {msg}"),
        None => format!(" {marca}{dir_texto}  {pos}/{total}{seq}"),
    };
    frame.render_widget(
        Paragraph::new(text).style(app.theme.role(Role::StatusBar)),
        area,
    );
}
