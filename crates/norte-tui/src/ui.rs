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
use norte_i18n::{t, ta};

/// Badge de nombre hostil: PREFIJO en columna fija (al final moriría en el
/// truncado por ancho de ratatui y el nombre se pintaría "limpio") y en
/// ASCII (`⚠` es ambiguous-width: 2 celdas en muchos terminales). Va
/// estilado (rol `hostile-badge`) — fuera de banda: un archivo llamado "! x"
/// no lo imita.
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
    let rows = u16::try_from(popup.items().len().max(1)).unwrap_or(8) + 2;
    // 64 y no 60: el footer de teclas de hotlist en ES son 60 celdas y a 60
    // el borde lo truncaría («cerra…»).
    let area = centered(frame.area(), 64, rows.min(frame.area().height.max(3)));
    frame.render_widget(ratatui::widgets::Clear, area);
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
                .map(|it| ListItem::new(Line::raw(format!(" {}", it.display))))
                .collect(),
            Some(popup.cursor()),
        )
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(Role::ModalBorder));
    if let Some(input) = &popup.name_input {
        let (masked, _) = display_name(input.as_bytes());
        block = block.title_bottom(Line::raw(format!(
            " {} {masked}_ ",
            t("hotlist-name-prompt")
        )));
    } else if popup.kind == NavPopupKind::Hotlist {
        block = block.title_bottom(Line::raw(format!(" {} ", t("hotlist-keys"))));
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
                norte_proto::TaskKind::Undo => "undo",
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

/// Elipsis MEDIA a `max` chars: conserva cabeza (scheme) y cola (nombre) —
/// lo que identifica la ruta ante un humano — y marca el recorte con `…`.
fn middle_ellipsis(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_owned();
    }
    let keep = max.saturating_sub(1);
    let head = keep / 2;
    let tail = keep - head;
    let mut out: String = s.chars().take(head).collect();
    out.push('…');
    out.extend(s.chars().skip(n - tail));
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
    if let Some(q) = &pane.quick {
        let (query, _) = display_name(q.query_display().as_bytes());
        let mut input = format!(" /{} {}/{}", query, q.visible().len(), pane.entries.len());
        if pane.loading {
            input.push(' ');
            input.push_str(&t("quicksearch-partial"));
        }
        input.push(' ');
        block = block.title_bottom(Line::styled(input, theme.role(Role::Title)));
    }
    // Filtro activo: SOLO los índices visibles, con el cursor visual en la
    // posición DENTRO del filtrado. En Jump (quick_visible = None) el
    // listado va entero y manda el cursor real.
    let (items, selected): (Vec<ListItem<'_>>, Option<usize>) = match pane.quick_visible() {
        Some(vis) => (
            vis.iter()
                .filter_map(|&i| pane.entries.get(i))
                .map(|e| entry_item(e, theme))
                .collect(),
            pane.quick
                .as_ref()
                .and_then(crate::nav::QuickSearch::selected_entry_index)
                .and_then(|s| vis.iter().position(|&i| i == s)),
        ),
        None => (
            pane.entries.iter().map(|e| entry_item(e, theme)).collect(),
            (!pane.entries.is_empty()).then_some(pane.cursor),
        ),
    };
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(selected);
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
    // Con el FILTRO activo la selección no es el cursor real: un `pos/total`
    // sería engañoso (review MINOR-2 T4) — se suprime; el pie del pane ya
    // da el contador honesto `n/m`.
    let pos_total = if pane.quick_visible().is_some() {
        String::new()
    } else {
        format!("  {pos}/{total}")
    };
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
    // resto de la barra hasta la siguiente tecla (issue #20). Sin mensaje,
    // el hook Lua de statusbar (M4, ya saneado por el host) sustituye la
    // línea default del pane con foco.
    let text = match (&app.message, &app.lua_status) {
        (Some(msg), _) => format!(" {msg}"),
        (None, Some(lua)) => format!(" {lua}{seq}"),
        (None, None) => format!(" {marca}{dir_texto}{pos_total}{seq}"),
    };
    frame.render_widget(
        Paragraph::new(text).style(app.theme.role(Role::StatusBar)),
        area,
    );
}
