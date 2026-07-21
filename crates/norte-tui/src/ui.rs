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
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Pane, display_name, path_display};
use crate::theme::TuiTheme;
use norte_i18n::{t, ta};

/// Badge de nombre hostil: PREFIJO en columna fija (al final moriría en el
/// truncado por ancho de ratatui y el nombre se pintaría "limpio") y en
/// ASCII (`⚠` es ambiguous-width: 2 celdas en muchos terminales). Va
/// estilado (rol `hostile-badge`) — fuera de banda: un archivo llamado "! x"
/// no lo imita. EXCEPCIÓN documentada: los popups de navegación llevan el
/// badge in-band dentro del display del item (como los títulos de modal);
/// un favorito llamado "! x" puede imitarlo — superficie de solo-lectura
/// propia del usuario, riesgo aceptado.
pub(crate) const HOSTILE_BADGE: &str = "!";

/// Pinta el frame completo: panes (o viewer) + panel de tasks + barra de
/// estado + modal por encima.
pub fn draw(frame: &mut Frame<'_>, app: &App) {
    // Fondo BASE del tema (ADR 0020): se pinta primero; los estilos de texto
    // (solo fg) lo conservan. Sin `background` en el tema = fondo del terminal.
    frame.render_widget(
        Block::default().style(app.theme.role(Role::Background)),
        frame.area(),
    );
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
    if let Some(picker) = &app.theme_picker {
        draw_theme_picker(frame, picker, &app.theme);
    }
    if let Some(mgr) = &app.extensions {
        draw_extensions(frame, mgr, &app.theme);
    }
    if let Some(popup) = &app.nav_popup {
        draw_nav_popup(frame, popup, &app.theme);
    }
    if let Some(dialog) = &app.search_dialog {
        draw_search_dialog(frame, dialog, app.focused().dir(), &app.theme);
    }
}

/// Diálogo de búsqueda viva (`Alt+F7`, liveSearch T6): dos campos de texto
/// (nombre/contenido) con un `_` en el activo, los dos toggles regex/case y la
/// raíz del walk (el `cwd` del pane, no editable) — todo saneado, jamás
/// bidi/controles crudos (los campos pasan por [`display_name`], la raíz por
/// [`path_display`]; un paste hostil no pinta invisibles en el borde).
fn draw_search_dialog(
    frame: &mut Frame<'_>,
    dialog: &crate::app::SearchDialog,
    root: &norte_proto::VPath,
    theme: &TuiTheme,
) {
    use crate::app::SearchField;
    let on_txt = |b: bool| if b { t("on-yes") } else { t("on-no") };
    let field = |label: &str, value: &str, active: bool| {
        let (masked, _) = display_name(value.as_bytes());
        let cursor = if active { "_" } else { "" };
        format!("{label} {masked}{cursor}")
    };
    let name_active = dialog.field == SearchField::Name;
    let (root_txt, root_hostil) = path_display(root);
    let root_line = if root_hostil {
        format!("{HOSTILE_BADGE} {root_txt}")
    } else {
        root_txt
    };
    let cuerpo = [
        field(&t("search-name"), &dialog.name, name_active),
        field(&t("search-content"), &dialog.content, !name_active),
        ta("search-regex", &[("on", &on_txt(dialog.regex))]),
        ta("search-case", &[("on", &on_txt(dialog.case))]),
        middle_ellipsis(&root_line, 56),
        t("search-hint"),
    ]
    .join("\n");
    let area = centered(frame.area(), 60, 8);
    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Paragraph::new(cuerpo).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", t("search-title")))
                .title_style(theme.role(Role::Title))
                .border_style(theme.role(Role::ModalBorder)),
        ),
        area,
    );
}

/// Popup de navegación (spec 2026-07-18): historial `Alt+↓` / hotlist
/// `Ctrl+D`, calcando [`draw_theme_picker`]. Los items llegan YA saneados
/// de [`crate::app::App::open_nav_popup`] — aquí solo se pintan. El footer
/// de teclas solo aplica a hotlist (`a`/`d`); con el input de nombre activo
/// lo sustituye la línea `nombre: …` (el input pasa por el MISMO mask que
/// la query del quick search: un paste hostil no pinta bidi crudo).
fn draw_nav_popup(frame: &mut Frame<'_>, popup: &crate::app::NavPopup, theme: &TuiTheme) {
    use crate::app::NavPopupKind;
    let title = match popup.kind {
        NavPopupKind::History => t("history-title"),
        NavPopupKind::Hotlist => t("hotlist-title"),
    };
    // El footer se construye ANTES para dimensionar el popup con su ancho
    // REAL (celdas unicode vía `Line::width`, no bytes): 64 de mínimo — el
    // footer de teclas de hotlist en ES son 60 celdas y a 60 el borde lo
    // truncaría («cerra…») — y crece si el footer (p.ej. un nombre largo en
    // el input) lo necesita.
    let footer: Option<Line<'_>> = if let Some(input) = &popup.name_input {
        let (masked, _) = display_name(input.as_bytes());
        Some(Line::raw(format!(
            " {} {masked}_ ",
            t("hotlist-name-prompt")
        )))
    } else if popup.kind == NavPopupKind::Hotlist {
        Some(Line::raw(format!(" {} ", t("hotlist-keys"))))
    } else {
        None
    };
    let footer_w = footer.as_ref().map_or(0, Line::width);
    let ancho = u16::try_from(footer_w.saturating_add(4))
        .unwrap_or(u16::MAX)
        .max(64);
    let rows = u16::try_from(popup.items().len().max(1)).unwrap_or(8) + 2;
    let area = centered(frame.area(), ancho, rows.min(frame.area().height.max(3)));
    frame.render_widget(ratatui::widgets::Clear, area);
    // Items largos: elipsis MEDIA (cabeza + cola, como los modales de
    // rutas) al ancho interior — el truncado derecho de ratatui haría
    // indistinguibles dos rutas con prefijo común (BAJA-3).
    let inner = usize::from(area.width.saturating_sub(3));
    let (items, selected): (Vec<ListItem<'_>>, Option<usize>) = if popup.items().is_empty() {
        let empty = match popup.kind {
            NavPopupKind::History => t("history-empty"),
            NavPopupKind::Hotlist => t("hotlist-empty"),
        };
        (vec![ListItem::new(Line::raw(format!(" {empty}")))], None)
    } else {
        (
            popup
                .items()
                .iter()
                .map(|it| {
                    ListItem::new(Line::raw(format!(
                        " {}",
                        middle_ellipsis(&it.display, inner)
                    )))
                })
                .collect(),
            Some(popup.cursor()),
        )
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(Role::ModalBorder));
    if let Some(footer) = footer {
        block = block.title_bottom(footer);
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

/// Overlay del catálogo de extensiones (M4-P3): la lista de plugins AGRUPADA
/// por categoría (una cabecera al cambiar de grupo, ya que llegan ordenados)
/// más los directorios que fallaron al cargar. CRÍTICO: `name` y `publisher`
/// son texto LIBRE de un tercero y esto es superficie de decisión de seguridad
/// (aprobar) — se pasan por [`display_name`] (mismo enmascarado de
/// controles/bidi/invisibles que los panes) antes de pintar. El id ya está
/// charset-validado en el core; name/publisher no.
fn draw_extensions(frame: &mut Frame<'_>, mgr: &crate::app::ExtensionManager, theme: &TuiTheme) {
    let area = centered(
        frame.area(),
        frame.area().width.saturating_sub(6).clamp(24, 80),
        frame.area().height.saturating_sub(4).max(6),
    );
    frame.render_widget(ratatui::widgets::Clear, area);
    let mut lines: Vec<Line<'_>> = Vec::new();
    if mgr.plugins.is_empty() && mgr.errors.is_empty() {
        lines.push(Line::raw(t("ext-empty")));
    } else {
        let mut last_cat: Option<&str> = None;
        for (i, p) in mgr.plugins.iter().enumerate() {
            if last_cat != Some(p.category.as_str()) {
                last_cat = Some(p.category.as_str());
                let (cat, _) = display_name(p.category.as_bytes());
                lines.push(Line::styled(cat, theme.role(Role::Title)));
            }
            lines.push(plugin_line(p, i == mgr.cursor, theme));
        }
        for e in &mgr.errors {
            let (dir, _) = display_name(e.dir.as_bytes());
            let (reason, _) = display_name(e.reason.as_bytes());
            lines.push(Line::styled(
                format!(" {HOSTILE_BADGE} {dir}: {reason}"),
                theme.role(Role::Error),
            ));
        }
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("ext-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(format!(" {} ", t("ext-hint"))))
        .border_style(theme.role(Role::ModalBorder));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Una línea de plugin: `<nombre> v<version> [<badges>] <estado>`. `name` y
/// `publisher` van enmascarados ([`display_name`]); badges = capabilities
/// unidas (o `-` si vacío); estado = `✓` si activo y aviso `⚠` (rol Warning)
/// si NO está aprobado. La línea seleccionada se resalta como el theme picker.
fn plugin_line<'a>(
    p: &'a norte_proto::methods::PluginInfo,
    selected: bool,
    theme: &TuiTheme,
) -> Line<'a> {
    let (name, _) = display_name(p.name.as_bytes());
    let (version, _) = display_name(p.version.as_bytes());
    let badges = if p.capabilities.is_empty() {
        "-".to_owned()
    } else {
        p.capabilities.join(" ")
    };
    let cursor = if selected { ">" } else { " " };
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
    if selected {
        line = line.style(theme.role(Role::Selection));
    }
    line
}

/// Popup selector de tema: lista de presets con el vigente resaltado (ADR
/// 0020). El preview en vivo lo hace el bucle de eventos; aquí solo se pinta.
fn draw_theme_picker(frame: &mut Frame<'_>, picker: &crate::app::ThemePicker, theme: &TuiTheme) {
    let rows = u16::try_from(picker.names.len()).unwrap_or(8) + 2;
    let area = centered(frame.area(), 34, rows.min(frame.area().height.max(3)));
    frame.render_widget(ratatui::widgets::Clear, area);
    let items: Vec<ListItem<'_>> = picker
        .names
        .iter()
        .map(|n| ListItem::new(Line::raw(format!(" {n}"))))
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("theme-picker-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(format!(" {} ", t("theme-picker-hint"))))
        .border_style(theme.role(Role::ModalBorder));
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(Some(picker.cursor));
    frame.render_stateful_widget(list, area, &mut state);
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
    let title = if hostil {
        format!("{HOSTILE_BADGE} {title}")
    } else {
        title
    };
    // M4-P5: indicador «via <plugin>» cuando la vista viene de un preview de
    // plugin (el plugin_name ya viene enmascarado desde el viewer).
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(app.theme.role(Role::Title))
        .border_style(app.theme.role(Role::BorderFocus));
    if let Some(plugin) = viewer.preview_plugin() {
        block = block.title(
            Span::styled(
                ta("viewer-plugin-preview", &[("plugin", plugin)]),
                app.theme.role(Role::Info),
            )
            .into_right_aligned_line(),
        );
    }
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
        None => format!(" {}  {pos}", crate::viewer::status(viewer)),
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
                norte_proto::TaskKind::Undo => "undo",
                // Etiqueta mínima; el diálogo/pane virtual de Alt+F7 llega en
                // T6 de liveSearch — aquí solo evita el `match` no exhaustivo.
                norte_proto::TaskKind::Search => "search",
                // Clase de un daemon N+1: etiqueta genérica, no rompe la UI.
                norte_proto::TaskKind::Unknown => "task",
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
        Modal::ApproveAgentOp { req } => approval_modal_text(req),
        Modal::TrustHostKey {
            host,
            port,
            algo,
            fingerprint,
            ..
        } => trust_host_modal_text(host, *port, algo, fingerprint),
        // TOFU Lua (M4): `path` viene YA saneado por el constructor del
        // modal (`detail_for_bar`); el cuerpo es un solo mensaje largo y el
        // Paragraph de este modal lleva wrap (abajo).
        Modal::TrustLuaInit { path, hash_abbrev } => (
            t("modal-lua-trust-title"),
            ta(
                "modal-lua-trust-body",
                &[("path", path.as_str()), ("hash", hash_abbrev.as_str())],
            ),
        ),
    };
    // Un borrado PERMANENTE (o aprobar una mutación de agente) tiñe el borde
    // de aviso (rol `warning`).
    let permanent = matches!(
        modal,
        Modal::ConfirmDelete {
            permanent: true,
            ..
        } | Modal::ApproveAgentOp { .. }
            | Modal::TrustHostKey { .. }
            | Modal::TrustLuaInit { .. }
    );
    let border = if permanent {
        theme.role(Role::Warning)
    } else {
        theme.role(Role::ModalBorder)
    };
    // Altura: fija salvo la aprobación (una línea POR ruta, H2 del auditor).
    let alto = match modal {
        Modal::ApproveAgentOp { req } => u16::try_from(req.paths.len())
            .unwrap_or(u16::MAX)
            .saturating_add(4),
        // host + algo + fingerprint + nota + teclas (5 líneas) + bordes.
        Modal::TrustHostKey { .. } => 9,
        // Un mensaje largo con wrap (~4 líneas a 58 cols) + bordes.
        Modal::TrustLuaInit { .. } => 8,
        _ => 6,
    };
    let area = centered(frame.area(), 60, alto);
    frame.render_widget(ratatui::widgets::Clear, area);
    let mut cuerpo = Paragraph::new(cuerpo).block(
        Block::default()
            .borders(Borders::ALL)
            .title(titulo)
            .title_style(theme.role(Role::Title))
            .border_style(border),
    );
    // Solo este modal envuelve: su cuerpo es UN mensaje largo; el resto ya
    // viene troceado por líneas (y el wrap podría partir un path por
    // cualquier char, cosa que los modales de rutas evitan con elipsis).
    if matches!(modal, Modal::TrustLuaInit { .. }) {
        cuerpo = cuerpo.wrap(ratatui::widgets::Wrap { trim: false });
    }
    frame.render_widget(cuerpo, area);
}

/// Bytes de la sesión para display; `None` = `?`. Sin colisión con una sesión
/// literal `"?"`: el daemon valida el charset `[A-Za-z0-9._-]` en el
/// handshake, así que `?` no es un id alcanzable.
fn session_bytes(req: &norte_proto::methods::PolicyApprovalRequired) -> &[u8] {
    req.session.as_deref().map_or(b"?", str::as_bytes)
}

/// Recorta a `max` CHARS (no bytes) con `…` final. Para strings ya
/// enmascarados que aún podrían ser kilométricos (clamp de layout, H1).
/// Texto `(título, cuerpo)` del modal de aprobación de agente (M3-3b T5).
/// TODO lo interpolado lo controla el AGENTE (encoding-auditor H1/H2/H3) y
/// esto es una decisión humana de seguridad: session y rutas pasan por el
/// MISMO enmascarado que los nombres de pane (controles/bidi/invisibles → �)
/// MÁS clamp; cada ruta va en SU línea con etiqueta fuera de banda (jamás un
/// joiner in-band que un nombre pueda imitar) y elipsis media (un `from`
/// kilométrico no expulsa el destino de la caja); el enmascarado se MARCA con
/// el badge (spec §6).
fn approval_modal_text(req: &norte_proto::methods::PolicyApprovalRequired) -> (String, String) {
    let session = clamp_chars(&display_name(session_bytes(req)).0, 40);
    let op = clamp_chars(&display_name(req.op.as_bytes()).0, 16);
    let mut lineas = vec![ta(
        "modal-approval-body",
        &[("session", &session), ("op", &op)],
    )];
    for (i, p) in req.paths.iter().enumerate() {
        let (texto, hostil) = display_name(p.as_bytes());
        lineas.push(ta(
            "modal-approval-path",
            &[
                ("badge", if hostil { HOSTILE_BADGE } else { "" }),
                ("n", &(i + 1).to_string()),
                ("path", &middle_ellipsis(&texto, 46)),
            ],
        ));
    }
    lineas.push(t("modal-approval-keys"));
    (t("modal-approval-title"), lineas.join("\n"))
}

/// Texto `(título, cuerpo)` del modal TOFU (#45). host/algo/fingerprint
/// vienen del SERVIDOR REMOTO (no confiable) y esto es una decisión de
/// seguridad: mismo enmascarado que las rutas de agente (controles/bidi/
/// invisibles → �) + clamp. El fingerprint legítimo es ASCII
/// (`SHA256:<base64>`), así que el enmascarado es un no-op salvo que el
/// server intente ocultar caracteres — en cuyo caso el � DELATA la
/// manipulación.
fn trust_host_modal_text(
    host: &str,
    port: Option<u16>,
    algo: &str,
    fingerprint: &str,
) -> (String, String) {
    let (host_txt, host_hostil) = display_name(host.as_bytes());
    let hostport = match port {
        Some(p) => format!("{}:{p}", clamp_chars(&host_txt, 48)),
        None => clamp_chars(&host_txt, 48),
    };
    let (algo_disp, algo_hostil) = display_name(algo.as_bytes());
    let algo_txt = clamp_chars(&algo_disp, 24);
    let (fp_txt, fp_hostil) = display_name(fingerprint.as_bytes());
    let lineas = [
        ta(
            "modal-trust-host-host",
            &[
                ("badge", if host_hostil { HOSTILE_BADGE } else { "" }),
                ("host", &hostport),
            ],
        ),
        ta(
            "modal-trust-host-algo",
            &[
                ("badge", if algo_hostil { HOSTILE_BADGE } else { "" }),
                ("algo", &algo_txt),
            ],
        ),
        ta(
            "modal-trust-host-fp",
            &[
                ("badge", if fp_hostil { HOSTILE_BADGE } else { "" }),
                ("fingerprint", &clamp_chars(&fp_txt, 52)),
            ],
        ),
        t("modal-trust-host-note"),
        t("modal-trust-host-keys"),
    ];
    (t("modal-trust-host-title"), lineas.join("\n"))
}

fn clamp_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Elipsis MEDIA a `max` CELDAS de terminal: conserva cabeza (scheme) y cola
/// (nombre) —lo que identifica la ruta ante un humano— y marca el recorte con
/// `…`. Presupuesta por ANCHO DE CELDA (CJK/emoji ocupan 2 columnas), no por
/// chars: contar chars desbordaba `max` con nombres densos y ratatui
/// re-truncaba por la DERECHA, comiéndose justo la cola que la elipsis media
/// existe para preservar (#79). Para ASCII (celdas == chars) el resultado es
/// idéntico al anterior.
fn middle_ellipsis(s: &str, max: usize) -> String {
    let cell = |c: char| UnicodeWidthChar::width(c).unwrap_or(0);
    if s.chars().map(cell).sum::<usize>() <= max {
        return s.to_owned();
    }
    // Una celda para el `…`; el resto se reparte cabeza/cola. Cada mitad
    // acumula chars mientras el siguiente QUEPA entero en su presupuesto: un
    // char ancho que no cabe se descarta (nunca se parte una celda).
    let budget = max.saturating_sub(1);
    let head_budget = budget / 2;
    let tail_budget = budget - head_budget;

    let mut head = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let w = cell(c);
        if used + w > head_budget {
            break;
        }
        used += w;
        head.push(c);
    }

    let mut tail: Vec<char> = Vec::new();
    let mut used_tail = 0usize;
    for c in s.chars().rev() {
        let w = cell(c);
        if used_tail + w > tail_budget {
            break;
        }
        used_tail += w;
        tail.push(c);
    }
    tail.reverse();

    let mut out = head;
    out.push('…');
    out.extend(tail);
    out
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
    let (title, title_hostil) = path_display(pane.dir());
    let mut title = if title_hostil {
        format!("{HOSTILE_BADGE} {title}")
    } else {
        title
    };
    // Un listado RELLENÁNDOSE (paginación, ADR 0017) se marca SIEMPRE: un
    // listado incompleto jamás es silencioso.
    if pane.loading() {
        use std::fmt::Write as _;
        let _ = write!(
            title,
            " [{}]",
            norte_i18n::ta("pane-loading", &[("n", &pane.entries().len().to_string())])
        );
    }
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title_style(theme.role(Role::Title))
        .title(title);
    // Quick search activo (spec 2026-07-18): línea de input al pie del pane
    // `/{query} n/m` (+ «parcial» si el fill sigue: filtra sobre lo YA
    // drenado, jamás en silencio). La query pasa por el MISMO mask que los
    // nombres (review MINOR-1 T4): «la tecleó el usuario» se rompe con un
    // PASTE — sin bracketed paste llega como stream de Chars y un nombre
    // hostil pegado pintaría bidi/invisibles crudos en el borde.
    if let Some(q) = pane.quick() {
        let (query, _) = display_name(q.query_display().as_bytes());
        let mut input = format!(" /{} {}/{}", query, q.visible().len(), pane.entries().len());
        if pane.loading() {
            input.push(' ');
            input.push_str(&t("quicksearch-partial"));
        }
        input.push(' ');
        block = block.title_bottom(Line::styled(input, theme.role(Role::Title)));
    }
    // Filtro activo: SOLO los índices visibles, con el cursor visual en la
    // posición DENTRO del filtrado. En Jump (quick_visible = None) el
    // listado va entero y manda el cursor real.
    let reinterpret = pane.name_encoding;
    let (items, selected): (Vec<ListItem<'_>>, Option<usize>) = match pane.quick_visible() {
        Some(vis) => (
            vis.iter()
                .filter_map(|&i| pane.entries().get(i))
                .map(|e| entry_item(e, theme, reinterpret))
                .collect(),
            pane.quick()
                .and_then(crate::nav::QuickSearch::selected_entry_index)
                .and_then(|s| vis.iter().position(|&i| i == s)),
        ),
        None => (
            pane.entries()
                .iter()
                .map(|e| entry_item(e, theme, reinterpret))
                .collect(),
            (!pane.entries().is_empty()).then_some(pane.cursor()),
        ),
    };
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

fn entry_item<'a>(
    entry: &'a norte_proto::Entry,
    theme: &TuiTheme,
    reinterpret: Option<norte_encoding::NameEncoding>,
) -> ListItem<'a> {
    let name = entry.path.file_name().map_or(&[][..], |n| n.as_bytes());
    // #57: con reinterpretación activa, los nombres no-UTF8 se decodifican
    // con el encoding elegido (display-only; el badge hostil se conserva —
    // el texto pintado difiere de los bytes reales).
    let (texto, hostil) = norte_frontend::display_name_with(name, reinterpret);
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
    let total = pane.entries().len();
    let pos = if total == 0 { 0 } else { pane.cursor() + 1 };
    // Con el FILTRO activo la selección no es el cursor real: un `pos/total`
    // sería engañoso (review MINOR-2 T4) — se suprime; el pie del pane ya
    // da el contador honesto `n/m`.
    let pos_total = if pane.quick_visible().is_some() {
        String::new()
    } else {
        format!("  {pos}/{total}")
    };
    let (dir_texto, dir_hostil) = path_display(pane.dir());
    let marca = if dir_hostil { HOSTILE_BADGE } else { "" };
    // Sin chuleta de teclas: mentiría según el preset (el which-key overlay
    // llega en fase 5). La secuencia pendiente SÍ se pinta (ADR 0006).
    let seq = if app.pending.is_empty() {
        String::new()
    } else {
        format!("  [{} …]", app.pending)
    };
    // Un mensaje pendiente (error por categoría, resultado) desplaza al
    // resto de la barra hasta la siguiente tecla (issue #20). Sin mensaje: un
    // pane de búsqueda viva (liveSearch T6) pinta `search-status-*` (los hits
    // = `entries.len()`); si no, el hook Lua de statusbar (M4, ya saneado por
    // el host) sustituye la línea default del pane con foco.
    let text = if let Some(msg) = &app.message {
        format!(" {msg}")
    } else if pane.virtual_search {
        use crate::app::SearchState;
        // `Failed` es PERSISTENTE (review MINOR-2): tras limpiarse
        // `app.message`, el pane sigue pintando `search-status-failed` con la
        // categoría del error (guardada en `search_error`) — un fallo jamás
        // degrada a «done» en la siguiente tecla.
        if pane.search_state == SearchState::Failed {
            format!(
                " {}{seq}",
                ta(
                    "search-status-failed",
                    &[("error", pane.search_error.as_deref().unwrap_or(""))],
                )
            )
        } else {
            let key = match pane.search_state {
                SearchState::Running => "search-status-running",
                SearchState::Truncated => "search-status-truncated",
                SearchState::Cancelled => "search-status-cancelled",
                // `Failed` ya se trató arriba; `Done` es el resto.
                SearchState::Done | SearchState::Failed => "search-status-done",
            };
            format!(
                " {}{seq}",
                ta(key, &[("n", &pane.entries().len().to_string())])
            )
        }
    } else if let Some(lua) = &app.lua_status {
        format!(" {lua}{seq}")
    } else if let Some(warn) = &app.connection_warning {
        // #44: sesión remota degradada a texto plano. PERSISTENTE (como
        // `search-status-failed`): sobrevive a las teclas — sin `message`, sin
        // búsqueda viva y sin hook Lua sigue avisando en cada frame.
        format!(" {warn}{seq}")
    } else {
        // #93: el contenedor omitió entradas de su índice — el listado que
        // se ve NO es todo lo que el archivo contiene. Persistente mientras
        // el pane esté dentro (paralelo del badge hostil, jamás silencioso).
        let omitidas = match pane.skipped {
            Some(n) if n > 0 => {
                format!(
                    "  {}",
                    ta("status-archive-skipped", &[("n", &n.to_string())])
                )
            }
            _ => String::new(),
        };
        // #57: modo de reinterpretación activo — PERSISTENTE mientras dure
        // (los nombres pintados no son los bytes; el usuario debe saberlo
        // en todo momento, no solo en el mensaje del toggle).
        let nombres = match pane.name_encoding {
            Some(enc) => format!("  {}", ta("status-names-encoding", &[("enc", enc.label())])),
            None => String::new(),
        };
        format!(" {marca}{dir_texto}{pos_total}{omitidas}{nombres}{seq}")
    };
    frame.render_widget(
        Paragraph::new(text).style(app.theme.role(Role::StatusBar)),
        area,
    );
}

#[cfg(test)]
mod ellipsis_tests {
    use super::middle_ellipsis;
    use unicode_width::UnicodeWidthStr;

    /// Una cadena que ya cabe en `max` celdas vuelve intacta.
    #[test]
    fn cabe_intacta() {
        assert_eq!(middle_ellipsis("file:///d/a.txt", 46), "file:///d/a.txt");
    }

    /// ASCII que desborda: comportamiento idéntico al anterior (celdas==chars),
    /// cabeza + `…` + cola, sin exceder `max`.
    #[test]
    fn ascii_conserva_cabeza_y_cola() {
        let s = "file:///muy/larga/ruta/hacia/un/archivo/final.txt";
        let out = middle_ellipsis(s, 20);
        assert!(out.contains('…'));
        assert!(out.starts_with("file:"), "conserva el scheme (cabeza)");
        let cola = out.rsplit_once('…').expect("hay elipsis").1;
        assert!(
            !cola.is_empty() && s.ends_with(cola),
            "la cola es un sufijo REAL del original: {out:?}"
        );
        assert!(out.width() <= 20, "no excede el ancho: {out:?}");
    }

    /// CJK (cada char = 2 celdas): NUNCA excede `max` celdas y CONSERVA la
    /// cola —el bug #79 la perdía porque presupuestaba por chars—.
    #[test]
    fn cjk_no_excede_y_conserva_cola() {
        let s = "日本語".repeat(20); // 60 chars, 120 celdas
        let out = middle_ellipsis(&s, 21);
        assert!(out.width() <= 21, "ancho {} > 21 en {out:?}", out.width());
        assert!(out.contains('…'));
        assert!(out.ends_with('語'), "la cola sobrevive: {out:?}");
        assert!(out.starts_with('日'), "la cabeza sobrevive: {out:?}");
    }

    /// Emoji ancho (2 celdas): tampoco desborda.
    #[test]
    fn emoji_no_excede() {
        let s = "a😀b😀c😀d😀e😀f😀g";
        let out = middle_ellipsis(s, 9);
        assert!(out.width() <= 9, "ancho {} en {out:?}", out.width());
        assert!(out.contains('…'));
    }

    /// `max` menor que un solo char ancho: no se parte la celda → solo `…`.
    #[test]
    fn max_menor_que_un_char_ancho() {
        let out = middle_ellipsis("日本", 1);
        assert_eq!(out, "…");
        assert!(out.width() <= 1);
    }
}
