//! Render ratatui del estado (`app`): cero lógica de negocio — pinta lo que
//! hay. El marcado de nombres hostiles sigue la spec §6 (lossy y MARCADO). Los
//! colores salen del tema resuelto (`app.theme`, ADR 0020): un frontend sin
//! tema ve el fallback monocromo de M1.

use norte_proto::EntryKind;
use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{AI_RENAME_PAIR_LIMIT, App, Pane, SEMANTIC_HIT_LIMIT, display_name};
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
    // El viewer sustituye a los panes, NUNCA a los overlays: antes este
    // brazo hacía `return` y CUALQUIER overlay abierto con el viewer
    // encima quedaba invisible aunque el run loop ya le hubiera dado la
    // tecla (su brazo va ANTES del viewer en la cadena) — la ayuda (F1),
    // el selector de tema, los ajustes, la palette y hasta un modal
    // asíncrono de aprobación se comían el teclado sin pintar un píxel:
    // el viewer parecía colgado y F1 "dejaba de funcionar". Los píxeles
    // deben decir quién manda (mismo criterio que el modal pintado el
    // último, más abajo).
    if let Some(viewer) = &app.viewer {
        draw_viewer(frame, viewer, app);
    } else {
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
        // #108 L5: `now` de las celdas de tiempo relativo — UNA lectura por
        // frame; los tests lo fijan (`App::render_now_ms`) para snapshots
        // estables.
        let now_ms = app.render_now_ms.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        });

        for (i, pane) in app.panes.iter().enumerate() {
            draw_pane(
                frame,
                cols[i],
                pane,
                app.focus() == i,
                &app.theme,
                now_ms,
                &app.columns,
                // #117 tarea 2: el catálogo cacheado del scheme del pane (hints
                // y cabeceras); sin él se pinta con defaults, jamás se espera.
                app.attr_catalog(pane.dir().scheme()),
            );
        }
        draw_tasks(frame, rows[1], app);
        draw_status(frame, rows[2], app);
    }
    if let Some(help) = &app.help {
        draw_help(frame, help, &app.theme);
    }
    if let Some(picker) = &app.theme_picker {
        draw_theme_picker(frame, picker, &app.theme, &app.dialog_hints.picker);
    }
    if let Some(p) = &app.columns_picker {
        draw_columns_picker(frame, p, &app.theme, &app.dialog_hints.columns);
    }
    if let Some(mgr) = &app.extensions {
        if let Some(panel) = &mgr.config {
            draw_plugin_config_panel(frame, panel, &app.theme, &app.dialog_hints.plugin_config);
        } else {
            draw_extensions(frame, mgr, &app.theme, &app.dialog_hints.extensions);
        }
    }
    if let Some(popup) = &app.nav_popup {
        draw_nav_popup(frame, popup, &app.theme, &app.dialog_hints.nav_list);
    }
    if let Some(dialog) = &app.search_dialog {
        draw_search_dialog(
            frame,
            dialog,
            app.focused().dir(),
            app.focused().name_encoding(),
            &app.theme,
        );
    }
    if let Some(palette) = &app.palette {
        draw_palette(frame, palette, &app.theme);
    }
    if let Some(settings) = &app.settings {
        draw_settings(frame, settings, &app.theme);
    }
    // Revisión S, M3: el modal se pinta ÚLTIMO, por encima de CUALQUIER otro
    // overlay — el enrutado de teclas ya lo trata como AUTORITATIVO en
    // presencia de la palette o el overlay de ajustes (`modal_preempts_
    // palette`/`modal_preempts_settings`, `main.rs`: un modal en vuelo p.ej.
    // una aprobación de policy async SIEMPRE gana la tecla). Antes se
    // pintaba justo tras la barra de estado, así que cualquier overlay
    // posterior en esta lista lo TAPABA visualmente — los píxeles mentían
    // sobre quién manda. Cierra la clase de H1 MINOR-4 (aceptada entonces
    // solo para la palette) para AMBOS overlays.
    if let Some(modal) = &app.modal {
        draw_modal(
            frame,
            modal,
            &app.theme,
            app.focused().name_encoding(),
            &app.dialog_hints,
        );
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
    enc: Option<norte_encoding::NameEncoding>,
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
    // #98/F4: la raíz del walk es superficie de decisión — sigue la
    // reinterpretación del pane (la barra de abajo pinta el mismo dir así).
    let (root_txt, root_hostil) = norte_frontend::path_display_with(root, enc);
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
/// la query del quick search: un paste hostil no pinta bidi crudo). `hint`
/// (H1 T3, #24) es el hint GENERADO (`app.dialog_hints.nav_list`) — el
/// historial no pinta footer, igual que antes de H1.
fn draw_nav_popup(
    frame: &mut Frame<'_>,
    popup: &crate::app::NavPopup,
    theme: &TuiTheme,
    hint: &str,
) {
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
        Some(Line::raw(format!(" {hint} ")))
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

/// Prefijo de `s` que cabe en `max` CELDAS (review MN2/MN3): recorte
/// consciente de ancho — un char de doble celda jamás desborda el
/// presupuesto (el recorte por `chars()` sí lo hacía).
fn take_width(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut usado = 0usize;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if usado + cw > max {
            break;
        }
        usado += cw;
        out.push(c);
    }
    out
}

/// La línea de cabecera (#108 L5): etiquetas Fluent (o la `header` custom
/// del spec, #108 7b — YA saneada y capada al resolver, aquí solo el
/// recorte por ancho), la del orden activo con `▲`/`▼`. Ancho fiel al de
/// las celdas de las filas; el `align` del estilo elige el lado del
/// relleno en las no-nombre, en paso con sus celdas.
fn column_header_line(
    cols: &[(
        norte_frontend::columns::ColumnId,
        u16,
        norte_frontend::columns::ColumnStyle,
    )],
    sort: norte_frontend::SortSpec,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> String {
    use norte_frontend::SortDir;
    use norte_frontend::columns::Align;
    let mut out = String::new();
    for (i, (col, w, style)) in cols.iter().enumerate() {
        // #117: etiqueta compartida TUI/GUI (header custom del spec →
        // Fluent → catálogo enmascarado → id). NO se re-enmascara aquí:
        // `header_label` ya devuelve texto seguro.
        let label = norte_frontend::columns::header_label(col, style, catalog);
        let activa = norte_frontend::columns::sort_column_id(col) == Some(sort.column);
        let w = usize::from(*w);
        let flecha = if sort.dir == SortDir::Asc {
            '▲'
        } else {
            '▼'
        };
        if i == 0 {
            // Nombre: alineado a la izquierda (deja el hueco del canalón).
            // La flecha se añade TRAS recortar (review MN2): el indicador
            // de dirección sobrevive a cualquier locale; recorte por ANCHO
            // (take_width), jamás por chars. El layout del nombre no lo
            // toca ningún `align` (#108 7b): su bloque manda.
            let budget = if activa { w.saturating_sub(1) } else { w };
            let mut cab = take_width(&label, budget);
            if activa {
                cab.push(flecha);
            }
            let pad = w.saturating_sub(cab.width());
            out.push_str(&cab);
            out.push_str(&" ".repeat(pad));
        } else {
            // No-nombre: el ancho incluye el separador — contenido dentro
            // de w-1, misma cuenta que la celda. Derecha: relleno delante.
            // Izquierda (#108 7b): el separador sigue ABRIENDO el ancho,
            // el contenido va tras él y el relleno cae a la derecha.
            let contenido = w.saturating_sub(1);
            let budget = if activa {
                contenido.saturating_sub(1)
            } else {
                contenido
            };
            let mut cab = take_width(&label, budget);
            if activa {
                cab.push(flecha);
            }
            match style.align {
                Align::Right => {
                    let pad = w.saturating_sub(cab.width());
                    out.push_str(&" ".repeat(pad));
                    out.push_str(&cab);
                }
                Align::Left => {
                    // m1 revisión 7b: emisión clampada a EXACTAMENTE `w`
                    // celdas — con `w == 1` y flecha activa, «espacio +
                    // flecha» emitía 2 y corría toda la cabecera a su
                    // derecha (el separador gana: abre el ancho, como en
                    // las celdas).
                    let clamped = take_width(&format!(" {cab}"), w);
                    let pad = w.saturating_sub(clamped.width());
                    out.push_str(&clamped);
                    out.push_str(&" ".repeat(pad));
                }
            }
        }
    }
    out
}

/// Overlay del catálogo de extensiones (M4-P3): la lista de plugins AGRUPADA
/// por categoría (una cabecera al cambiar de grupo, ya que llegan ordenados)
/// más los directorios que fallaron al cargar. CRÍTICO: `name` y `publisher`
/// son texto LIBRE de un tercero y esto es superficie de decisión de seguridad
/// (aprobar) — se pasan por [`display_name`] (mismo enmascarado de
/// controles/bidi/invisibles que los panes) antes de pintar. El id ya está
/// charset-validado en el core; name/publisher no. `hint` (H1 T3, #24) es
/// el hint GENERADO (`app.dialog_hints.extensions`).
fn draw_extensions(
    frame: &mut Frame<'_>,
    mgr: &crate::app::ExtensionManager,
    theme: &TuiTheme,
    hint: &str,
) {
    // MAJOR-1(c) H1 close: el ancho por CONTENIDO (igual que antes,
    // clamp(24, 80)) puede quedarse corto para el footer GENERADO — mismo
    // criterio de sizing que [`draw_nav_popup`] (medir el footer en CELDAS,
    // `Line::width`, y crecer si hace falta), tope en el ancho del frame.
    let footer_w = Line::raw(format!(" {hint} ")).width();
    let ancho_min = u16::try_from(footer_w.saturating_add(4)).unwrap_or(u16::MAX);
    let ancho = frame
        .area()
        .width
        .saturating_sub(6)
        .clamp(24, 80)
        .max(ancho_min)
        .min(frame.area().width);
    let area = centered(
        frame.area(),
        ancho,
        frame.area().height.saturating_sub(4).max(6),
    );
    frame.render_widget(ratatui::widgets::Clear, area);
    // Ancho útil para la segunda línea (description, P1): igual criterio que
    // `draw_palette` (borde + margen), NO el `ancho` de la caja completa.
    let inner = usize::from(area.width.saturating_sub(4));
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
            if let Some(desc_line) = plugin_description_line(p, theme, inner) {
                lines.push(desc_line);
            }
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
        .title_bottom(Line::raw(format!(" {hint} ")))
        .border_style(theme.role(Role::ModalBorder));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Panel de `[config]` de UN plugin (G3c, drill-down de
/// [`draw_extensions`]): una línea `<key>: <value>` por
/// [`norte_frontend::plugin_config::ConfigKeyRow`], la seleccionada
/// resaltada; si se está editando (`state.is_editing()`), el buffer RAW se
/// pinta bajo la fila con un cursor `_` (mismo idioma visual que un
/// name-input popup). `key`/`kind`/`value` son charset-safe o vocabulario
/// de norte (nunca texto libre del plugin — ver el rustdoc de
/// [`norte_frontend::plugin_config::ConfigKeyRow`]); `description` llega YA
/// enmascarada (`sanitize_config_keys`), se pinta como segunda línea
/// atenuada igual que [`plugin_description_line`].
fn draw_plugin_config_panel(
    frame: &mut Frame<'_>,
    panel: &crate::app::PluginConfigPanel,
    theme: &TuiTheme,
    hint: &str,
) {
    let footer_w = Line::raw(format!(" {hint} ")).width();
    let ancho_min = u16::try_from(footer_w.saturating_add(4)).unwrap_or(u16::MAX);
    let ancho = frame
        .area()
        .width
        .saturating_sub(6)
        .clamp(24, 80)
        .max(ancho_min)
        .min(frame.area().width);
    let area = centered(
        frame.area(),
        ancho,
        frame.area().height.saturating_sub(4).max(6),
    );
    frame.render_widget(ratatui::widgets::Clear, area);
    let mut lines: Vec<Line<'_>> = Vec::new();
    let rows = panel.state.rows();
    if rows.is_empty() {
        lines.push(Line::raw(t("ext-empty")));
    } else {
        for (i, row) in rows.iter().enumerate() {
            let selected = i == panel.state.cursor();
            let cursor = if selected { ">" } else { " " };
            let mut line = Line::raw(format!("{cursor} {}: {}", row.key, row.value));
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
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", panel.plugin_name))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(format!(" {hint} ")))
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

/// Segunda línea BAJO cada plugin con su `description` (P1), si la declara
/// — `None` si el plugin no tiene una. El camino normal (`main::dispatch`,
/// brazo `app.extensions`) ya llega con `description` clampada+enmascarada
/// por `app::clamp_plugin_descriptions` (P1 encoding audit F1: UNA vez por
/// plugin al ingest, no por frame) — pero este draw NO confía ciegamente en
/// eso: re-clampa+enmascara aquí también, self-contained como `plugin_line`
/// con `name`/`publisher` (y como `palette::plugin_rows`). Un control/bidi
/// crudo que llegara a `ratatui` sin pasar por [`display_name`] no se pinta
/// como `�` — un char de control/override es INVISIBLE en la celda, así que
/// desaparecería en silencio (justo lo que el enmascarado existe para
/// evitar); confiar ciegamente en el caller cambiaría "marcado" por
/// "silencioso" ante cualquier ruta que construya `ExtensionManager` sin
/// pasar por el ingest (tests, un futuro caller). Sobre un string YA
/// acotado (el caso normal) esto es barato e idempotente. Elipsis MEDIA
/// ([`middle_ellipsis`]) al ancho útil del popup para no desbordar la caja.
/// Sin badge de hostil (el badge es para diagnóstico de fallos de carga,
/// [`HOSTILE_BADGE`], no para cosmética de terceros — mismo criterio que
/// `plugin_line`). Estilo atenuado (`Role::BorderUnfocused`, "presente pero
/// no activo" — mismo criterio que documenta ese rol): es contexto, no el
/// dato principal de la fila.
fn plugin_description_line(
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
    let texto = format!("   {}", middle_ellipsis(&masked, inner.saturating_sub(3)));
    Some(Line::styled(texto, theme.role(Role::BorderUnfocused)))
}

/// Popup selector de tema: lista de presets con el vigente resaltado (ADR
/// 0020). El preview en vivo lo hace el bucle de eventos; aquí solo se
/// pinta. `hint` (H1 T3, #24) es el hint GENERADO (`app.dialog_hints.picker`).
/// MAJOR-1(c) H1 close: 34 columnas era un ancho FIJO que no crecía con el
/// hint generado (se cortaba en terminales angostas) — mismo criterio de
/// sizing que [`draw_nav_popup`]/[`draw_extensions`], footer en CELDAS
/// (`Line::width`), suelo 34 (el listado de nombres de preset ya cabía),
/// tope el ancho del frame.
fn draw_theme_picker(
    frame: &mut Frame<'_>,
    picker: &crate::app::ThemePicker,
    theme: &TuiTheme,
    hint: &str,
) {
    let footer_w = Line::raw(format!(" {hint} ")).width();
    let ancho = u16::try_from(footer_w.saturating_add(4))
        .unwrap_or(u16::MAX)
        .max(34)
        .min(frame.area().width);
    let rows = u16::try_from(picker.names.len()).unwrap_or(8) + 2;
    let area = centered(frame.area(), ancho, rows.min(frame.area().height.max(3)));
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
        .title_bottom(Line::raw(format!(" {hint} ")))
        .border_style(theme.role(Role::ModalBorder));
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(Some(picker.cursor));
    frame.render_stateful_widget(list, area, &mut state);
}

/// Overlay del picker de columnas (#108 7a): lista con cursor — checkbox,
/// etiqueta (Fluent para builtins; `label` del modelo para attr/plugin,
/// #117; el id CRUDO enmascarado para los que no parsean — texto de config
/// del usuario, #73: se pinta con `mask_terminal_hazards`) y la flecha del
/// sort en la fila de su columna. Mismo esqueleto que [`draw_theme_picker`] (Clear + centrado,
/// `List` + `ListState` con highlight `Role::Selection`, hint generado en
/// `title_bottom`, ancho por contenido en CELDAS con suelo del footer).
fn draw_columns_picker(
    frame: &mut Frame<'_>,
    p: &norte_frontend::columns_picker::ColumnsPicker,
    theme: &TuiTheme,
    hint: &str,
) {
    use norte_frontend::columns::{Builtin, sort_column};
    let target = if p.scheme_override() {
        p.scheme().to_owned()
    } else {
        t("columns-picker-target-default")
    };
    let titulo = ta("columns-picker-title", &[("target", &target)]);
    let filas: Vec<String> = p
        .rows()
        .iter()
        .map(|r| {
            let marca = if r.enabled { "[x]" } else { "[ ]" };
            let etiqueta = match r.builtin {
                Some(Builtin::Name) => t("col-header-name"),
                Some(Builtin::Size) => t("col-header-size"),
                Some(Builtin::Mtime) => t("col-header-mtime"),
                Some(Builtin::Kind) => t("col-header-kind"),
                // #117: attr/plugin traen `label` (header_label, YA
                // enmascarada al abrir); los que no parsean caen al id. El
                // re-enmascarado es cinturón, no el choke point; el cap
                // (encoding-audit L1, paridad GUI) evita que un id
                // kilométrico de config ensanche el overlay entero.
                None => norte_encoding::mask_terminal_hazards(r.label.as_deref().unwrap_or(&r.id))
                    .chars()
                    .take(norte_frontend::columns::HEADER_MAX_CHARS)
                    .collect(),
            };
            let flecha = match r.builtin.and_then(sort_column) {
                Some(sc) if sc == p.sort().column => {
                    if p.sort().dir == norte_frontend::SortDir::Asc {
                        " ▲"
                    } else {
                        " ▼"
                    }
                }
                _ => "",
            };
            // #108 7b: el formato vigente de la fila (vocabulario ASCII
            // cerrado — sin enmascarar), ciclable con `f`.
            let formato = r
                .format
                .as_deref()
                .map(|f| format!(" · {f}"))
                .unwrap_or_default();
            format!(" {marca} {etiqueta}{flecha}{formato}")
        })
        .collect();
    let footer_w = Line::raw(format!(" {hint} ")).width();
    let contenido_w = filas
        .iter()
        .map(|f| Line::raw(f.as_str()).width())
        .max()
        .unwrap_or(0);
    let ancho = u16::try_from(footer_w.max(contenido_w).saturating_add(4))
        .unwrap_or(u16::MAX)
        .max(34)
        .min(frame.area().width);
    // M2 revisión 7a: saturante — una config hostil de 65k ids desbordaría
    // el `+ 2` en debug; el `.min(alto del frame)` de abajo sigue clampando.
    let rows = u16::try_from(p.rows().len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered(frame.area(), ancho, rows.min(frame.area().height.max(3)));
    frame.render_widget(ratatui::widgets::Clear, area);
    let items: Vec<ListItem<'_>> = filas.into_iter().map(ListItem::new).collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {titulo} "))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(format!(" {hint} ")))
        .border_style(theme.role(Role::ModalBorder));
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(Some(p.cursor()));
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

/// Command palette (`Ctrl+P`/vim `:`, H1 T4, spec-promised): filtro libre
/// sobre TODOS los comandos, mismo idioma visual que [`draw_nav_popup`]
/// (centrado, input al pie, `Clear` antes de pintar) pero MÁS ancha (60
/// columnas: `{texto} {descripción} {chord}` no cabe en el ancho de un
/// popup normal). Una fila built-in ([`crate::palette::build_rows`]) trae
/// `text`/`desc`/`chord` CONFIABLES (constantes del binario + catálogo
/// Fluent) — este draw jamás los enmascara. Una fila de plugin (P1,
/// [`crate::palette::plugin_rows`]) trae texto de TERCEROS, pero YA
/// enmascarado en la fila misma (mismo criterio que `first_chord` con la
/// columna chord: el enmascarado vive donde se CONSTRUYE la fila, no aquí)
/// — este draw sigue sin diferenciar, solo pinta lo que ya es seguro. La
/// `key` de despacho (P1: puede llevar el `command_id` crudo de un plugin,
/// sin charset validado) NUNCA se lee aquí — [`crate::app::Palette::rows`]
/// solo se consulta por `text`/`desc`/`chord`. La query (tecleada por el
/// usuario) pasa por [`crate::app::Palette::query_display`] (mismo
/// contrato que `QuickSearch::query_display`: un paste hostil no pinta
/// bidi/invisibles crudos en el borde) + [`display_name`] (mismo doble
/// filtro que la barra de quick search del pane, línea de abajo). El hint
/// es ESTÁTICO (`palette-hint`): la palette NO resuelve por el contexto
/// `dialog` (decisión 8 del plan H1 — es un editor de filtro libre como el
/// diálogo de búsqueda), así que no hay hint GENERADO que mostrar aquí.
fn draw_palette(frame: &mut Frame<'_>, palette: &crate::app::Palette, theme: &TuiTheme) {
    let rows = u16::try_from(palette.visible().len().max(1))
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered(frame.area(), 60, rows.min(frame.area().height.max(3)));
    frame.render_widget(ratatui::widgets::Clear, area);
    let inner = usize::from(area.width.saturating_sub(3));
    let (items, selected): (Vec<ListItem<'_>>, Option<usize>) = if palette.visible().is_empty() {
        (vec![ListItem::new(Line::raw(" —"))], None)
    } else {
        (
            palette
                .visible()
                .iter()
                .map(|&i| {
                    let row = &palette.rows()[i];
                    let texto = format!(" {:<24} {:<32} {}", row.text, row.desc, row.chord);
                    ListItem::new(Line::raw(middle_ellipsis(&texto, inner)))
                })
                .collect(),
            Some(palette.cursor()),
        )
    };
    let (query, _) = display_name(palette.query_display().as_bytes());
    let footer = Line::raw(format!(" /{query}  {} ", t("palette-hint")));
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

/// Overlay de ajustes (`app.settings`, S3): mismo idioma visual que
/// [`draw_extensions`] (Paragraph con cabeceras de sección intercaladas,
/// NO `List`/`ListState` — hay DOS grupos heterogéneos, General y Plugins,
/// y `draw_extensions` ya resolvió ese patrón) más una línea de descripción
/// RESERVADA bajo la lista (la de la fila seleccionada, [`Settings::
/// selected_desc`]) y un footer que alterna entre el filtro (navegando) y el
/// buffer de edición inline (`Settings::is_editing`). Nombre/descripción son
/// Fluent — texto PROPIO del binario, jamás de un tercero (a diferencia de
/// `draw_extensions`, que sí enmascara `name`/`publisher` de un plugin): no
/// hace falta `display_name` aquí, solo `middle_ellipsis` por ancho. El
/// buffer de edición SÍ es entrada del usuario vía terminal (paste incluido)
/// — se enmascara igual que la query, mismo contrato que `NavPopup::
/// name_input`.
fn draw_settings(frame: &mut Frame<'_>, settings: &crate::app::Settings, theme: &TuiTheme) {
    let ancho = frame
        .area()
        .width
        .saturating_sub(6)
        .clamp(30, 80)
        .min(frame.area().width);
    let alto = frame.area().height.saturating_sub(4).max(6);
    let area = centered(frame.area(), ancho, alto);
    frame.render_widget(ratatui::widgets::Clear, area);

    let footer = if settings.is_editing() {
        let (buf, _) = display_name(settings.edit_buffer().unwrap_or_default().as_bytes());
        Line::raw(format!(" {buf}_  {} ", t("settings-edit-hint")))
    } else {
        let (query, _) = display_name(settings.query_display().as_bytes());
        Line::raw(format!(" /{query}  {} ", t("settings-hint")))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("settings-title")))
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

    let mut lines: Vec<Line<'_>> = Vec::new();
    if settings.visible().is_empty() {
        lines.push(Line::raw(" —"));
    } else {
        let mut general_header = false;
        let mut plugins_header = false;
        for (pos, &real) in settings.visible().iter().enumerate() {
            let row = &settings.rows()[real];
            if row.is_plugins_note() {
                if !plugins_header {
                    lines.push(Line::styled(
                        t("settings-section-plugins"),
                        theme.role(Role::Title),
                    ));
                    plugins_header = true;
                }
            } else if !general_header {
                lines.push(Line::styled(
                    t("settings-section-general"),
                    theme.role(Role::Title),
                ));
                general_header = true;
            }
            let selected = pos == settings.cursor();
            let cursor = if selected { ">" } else { " " };
            let texto = if row.is_plugins_note() {
                format!("{cursor} {}", row.name)
            } else {
                format!("{cursor} {:<28} {}", row.name, row.value)
            };
            let mut line = Line::raw(middle_ellipsis(&texto, inner_w));
            if selected {
                line = line.style(theme.role(Role::Selection));
            }
            lines.push(line);
        }
    }
    frame.render_widget(Paragraph::new(lines), split[0]);

    let desc = settings.selected_desc().unwrap_or_default();
    let desc_line = Line::raw(format!(
        " {}",
        middle_ellipsis(desc, inner_w.saturating_sub(1))
    ));
    frame.render_widget(
        Paragraph::new(desc_line).style(theme.role(Role::BorderUnfocused)),
        split[1],
    );
}

/// Viewer a pantalla completa: contenido + status propia (encoding, EOL,
/// pérdidas, truncado — el usuario SIEMPRE sabe qué mira, spec §6).
fn draw_viewer(frame: &mut Frame<'_>, viewer: &crate::viewer::Viewer, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());
    let (title, hostil) =
        norte_frontend::path_display_with(&viewer.path, app.focused().name_encoding());
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
        // #101: cuando la decodificación host-side fue LOSSY, un aviso (rol
        // Warning) SIGUE al «via …» — misma honestidad que el status de
        // encoding del viewer crudo, y mismo orden que la GUI
        // (`viewer_header`). ASCII (`⚠` es ambiguous-width).
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
    let inner_h = rows[0].height.saturating_sub(2) as usize;
    // #29/G3a (ADR 0037): un preview de plugin trae color, por ANSI-SGR
    // saneado (`fg` únicamente) o por WIT estructurado (`role` VALIDADO +
    // `fg` de respaldo). `role` GANA sobre `fg` cuando ambos están
    // presentes (el tema del usuario tiene precedencia sobre el color fijo
    // de un plugin, ADR 0037 decisión 3) — se resuelve por el tema
    // (`app.theme.role`), no como RGB crudo. Sin ninguno de los dos, el
    // color por defecto del tema (sin `.style()`).
    let lines: Vec<Line<'_>> = match viewer.plugin_styled_rows(inner_h) {
        Some(styled) => styled
            .into_iter()
            .map(|line| {
                Line::from(
                    line.iter()
                        .map(|span| {
                            let s = Span::raw(span.text.clone());
                            if let Some(role) = span.role {
                                s.style(app.theme.role(role))
                            } else if let Some((r, g, b)) = span.fg {
                                s.style(Style::default().fg(Color::Rgb(r, g, b)))
                            } else {
                                s
                            }
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect(),
        None => viewer.rows(inner_h).into_iter().map(Line::raw).collect(),
    };
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
                norte_proto::TaskKind::Index => "index",
                norte_proto::TaskKind::Mkdir => "mkdir",
                norte_proto::TaskKind::Embed => "embed",
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

/// Ancho del modal por CONTENIDO (H1 T3 follow-up): los pies GENERADOS
/// pueden superar las 60 col históricas — p. ej. colisión: `[esc] … [w] más
/// nuevo` — y truncarlos escondería teclas reales. Techo = ancho del frame
/// menos margen; suelo = las 60 históricas. MINOR-1 (H1 close): se mide en
/// CELDAS de terminal (`UnicodeWidthStr::width`, mismo idioma que
/// [`draw_nav_popup`]/[`middle_ellipsis`]), no en `chars` — un cuerpo con
/// CJK (dos celdas por char, p. ej. un path con `日本語`) desbordaba la caja
/// con el conteo de chars antiguo.
fn modal_width(titulo: &str, cuerpo: &str, frame_width: u16) -> u16 {
    let contenido_max = cuerpo
        .lines()
        .map(UnicodeWidthStr::width)
        .chain(std::iter::once(titulo.width() + 2))
        .max()
        .unwrap_or(0);
    u16::try_from(contenido_max + 4)
        .unwrap_or(u16::MAX)
        .clamp(60, frame_width.saturating_sub(4).max(60))
}

/// Alto del modal por variante (líneas de contenido + bordes).
fn modal_height(modal: &crate::app::Modal) -> u16 {
    use crate::app::Modal;
    match modal {
        Modal::ApproveAgentOp { req } => u16::try_from(req.paths.len())
            .unwrap_or(u16::MAX)
            .saturating_add(4),
        // #103 T10: una línea POR ítem listado (más la de resumen, si el
        // lote no cabe entero), más las dos fijas (destino/modo + teclas) y
        // los bordes — el mismo `body_lines + 3` que el resto. `centered`
        // recorta contra el frame: en un terminal enano el lote se ve a
        // medias, nunca desborda.
        Modal::ConfirmDelete { items, .. } | Modal::ConfirmTransfer { items, .. } => {
            let listadas = items.len().min(norte_frontend::MODAL_ITEM_LIMIT)
                + usize::from(items.len() > norte_frontend::MODAL_ITEM_LIMIT);
            u16::try_from(listadas)
                .unwrap_or(u16::MAX)
                .saturating_add(5)
        }
        // TrustHostKey: host + algo + fingerprint + nota + teclas (5 líneas)
        // + bordes. TransferName con error (#105): origen + dir destino +
        // campo + hint + teclas + error (6 líneas), +3.
        Modal::TrustHostKey { .. } | Modal::TransferName { error: Some(_), .. } => 9,
        // TrustLuaInit: un mensaje largo con wrap (~4 líneas a 58 cols) +
        // bordes. TransferName sin error: 5 líneas de cuerpo (origen y dir
        // destino incluidos), +3.
        Modal::TrustLuaInit { .. } | Modal::TransferName { .. } => 8,
        // Patrón/mkdir + hint + teclas (3 líneas) o + la línea de error (4),
        // más bordes (#103 T9: mismo cómputo `body_lines + 3` que el resto).
        // Sin error caen al comodín `6` de abajo (match_same_arms).
        Modal::MarkPattern { error: Some(_), .. }
        | Modal::Mkdir { error: Some(_), .. }
        | Modal::AiRenameInstruction { error: Some(_), .. }
        | Modal::SemanticQuery { error: Some(_), .. } => 7,
        // M4-IA: la línea del dir (audit MAJOR-1) + dos por pareja de la
        // VENTANA + el indicador (si el plan no cabe entero) + el hint, más
        // bordes — mismo cómputo dinámico `body_lines + 3` que
        // ConfirmDelete/ConfirmTransfer. Estable al scroll: la ventana
        // clampada siempre pinta `min(len, LIMIT)` parejas.
        Modal::AiRenamePlan { entries, .. } => {
            let lineas = 1
                + 2 * entries.len().min(AI_RENAME_PAIR_LIMIT)
                + usize::from(entries.len() > AI_RENAME_PAIR_LIMIT)
                + 1;
            u16::try_from(lineas).unwrap_or(u16::MAX).saturating_add(3)
        }
        // M4-IA-2: un hit POR LÍNEA de la ventana + el indicador (si el
        // lote no cabe entero) + el hint — mismo cómputo dinámico
        // `body_lines + 3` que el plan IA. Estable al scroll.
        Modal::SemanticHits { hits, .. } => {
            let lineas = hits.len().min(SEMANTIC_HIT_LIMIT)
                + usize::from(hits.len() > SEMANTIC_HIT_LIMIT)
                + 1;
            u16::try_from(lineas).unwrap_or(u16::MAX).saturating_add(3)
        }
        _ => 6,
    }
}

/// Si `modal` tiñe el borde de aviso (rol `warning`): un borrado PERMANENTE
/// o una decisión de seguridad (aprobar una op de agente, confiar en una
/// host key o en un `init.lua` de proyecto). Factorizado fuera de
/// `draw_modal` (clippy `too_many_lines`).
fn is_warning_modal(modal: &crate::app::Modal) -> bool {
    use crate::app::Modal;
    matches!(
        modal,
        Modal::ConfirmDelete {
            permanent: true,
            ..
        } | Modal::ApproveAgentOp { .. }
            | Modal::TrustHostKey { .. }
            | Modal::TrustLuaInit { .. }
    )
}

/// Caja centrada del modal.
/// `reinterpret` = enc del pane con FOCO al pintar: correcto para los
/// modales SÍNCRONOS (confirmar copy/move/delete se crea desde el pane con
/// foco y un modal abierto congela el foco — creación ≡ draw). Los ASYNC
/// (colisión) llevan su enc capturado al lanzar (`RetrySpec`, #98/M1). Los
/// paths de agentes (`ApproveAgentOp`) JAMÁS se reinterpretan: otra
/// frontera de confianza (van por `display_name` crudo a propósito). `hints`
/// (H1 T3, #24) trae los pies de página GENERADOS de cada modal — uno por
/// campo, ya resueltos del efectivo `dialog` vigente.
/// Título+cuerpo del modal activo, extraído de `draw_modal` (clippy
/// `too_many_lines` al crecer la familia de modales).
fn modal_title_body(
    modal: &crate::app::Modal,
    reinterpret: Option<norte_encoding::NameEncoding>,
    hints: &crate::hints::DialogHints,
) -> (String, String) {
    use crate::app::{Modal, TransferKind};
    match modal {
        // #103 T10: el lote va como LISTA — una ruta por línea, saneada y
        // truncada por la política COMPARTIDA con la GUI
        // (`norte_frontend::item_lines_with`), jamás dos rutas en la misma
        // línea (un nombre hostil fabricaría una entrada de la lista).
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
        Modal::ConfirmTransfer { kind, items, to } => (
            match kind {
                TransferKind::Copy => t("modal-copy-title"),
                TransferKind::Move => t("modal-move-title"),
            },
            [
                norte_frontend::item_lines_with(items, HOSTILE_BADGE, reinterpret),
                vec![
                    // El destino es un DIRECTORIO y va en SU línea, con la
                    // flecha FUERA de banda: ningún nombre de la lista de
                    // arriba puede imitar esta línea.
                    format!("→ {}", norte_frontend::path_display_with(to, reinterpret).0),
                    hints.confirm.clone(),
                ],
            ]
            .concat()
            .join("\n"),
        ),
        // #98/M1: la colisión llega ASYNC — usa el enc capturado al LANZAR
        // la operación (RetrySpec), jamás el del pane con foco al llegar.
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
        // S2 (`[ui] confirm_quit`): sin datos propios — un título+cuerpo
        // fijos más el hint (`hints.confirm`, ALLOW_CONFIRM reutilizado).
        Modal::ConfirmQuit => (
            t("modal-confirm-quit-title"),
            format!("{}\n{}", t("modal-confirm-quit-body"), hints.confirm),
        ),
        // #103 T9: ver `mark_pattern_modal_text` (enmascarado, no un texto
        // fijo — el patrón/error son de usuario).
        Modal::MarkPattern {
            mark,
            pattern,
            error,
        } => mark_pattern_modal_text(*mark, pattern, error.as_deref()),
        // #104: mismo enmascarado que el patrón — nombre y error son de
        // usuario (paste con bidi/invisibles incluido).
        Modal::Mkdir { name, error } => mkdir_modal_text(name, error.as_deref()),
        // M4-IA: mismo enmascarado que mkdir — instrucción y error son texto
        // de usuario (paste con bidi/invisibles incluido).
        Modal::AiRenameInstruction { instruction, error } => {
            ai_rename_modal_text(instruction, error.as_deref())
        }
        // M4-IA: dir objetivo + ventana de parejas from→to del plan
        // revisable (enmascarado defensivo, ver `ai_rename_plan_modal_text`).
        Modal::AiRenamePlan {
            dir,
            entries,
            offset,
        } => ai_rename_plan_modal_text(dir, entries, *offset),
        // M4-IA-2: mismo enmascarado que la instrucción IA — consulta y
        // error son texto de usuario.
        Modal::SemanticQuery { query, error } => semantic_query_modal_text(query, error.as_deref()),
        // M4-IA-2: ventana de hits con cursor (enmascarado defensivo, ver
        // `semantic_hits_modal_text`).
        Modal::SemanticHits {
            hits,
            offset,
            cursor,
        } => semantic_hits_modal_text(hits, *offset, *cursor),
        // #105: nombre de destino editable — dir destino + campo + error,
        // todo de usuario y todo enmascarado.
        Modal::TransferName {
            kind,
            from,
            to_dir,
            name,
            error,
            enc,
            ..
        } => transfer_name_modal_text(*kind, from, to_dir, name, error.as_deref(), *enc),
    }
}

/// Pinta el modal activo: borde (de aviso en las superficies de decisión
/// duras), título y cuerpo de `modal_title_body`. `reinterpret` es la
/// reinterpretación del pane con foco AL PINTAR — los modales que capturan
/// la suya al abrir (`Collision` #98/M1, `TransferName` #105) la ignoran a
/// favor de la capturada.
fn draw_modal(
    frame: &mut Frame<'_>,
    modal: &crate::app::Modal,
    theme: &TuiTheme,
    reinterpret: Option<norte_encoding::NameEncoding>,
    hints: &crate::hints::DialogHints,
) {
    use crate::app::Modal;
    let (titulo, cuerpo) = modal_title_body(modal, reinterpret, hints);
    // Un borrado PERMANENTE (o aprobar una mutación de agente) tiñe el borde
    // de aviso (rol `warning`).
    let border = if is_warning_modal(modal) {
        theme.role(Role::Warning)
    } else {
        theme.role(Role::ModalBorder)
    };
    // Altura: fija salvo la aprobación (una línea POR ruta, H2 del auditor).
    let alto = modal_height(modal);
    let area = centered(
        frame.area(),
        modal_width(&titulo, &cuerpo, frame.area().width),
        alto,
    );
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
fn approval_modal_text(
    req: &norte_proto::methods::PolicyApprovalRequired,
    hint: &str,
) -> (String, String) {
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
    lineas.push(hint.to_owned());
    (t("modal-approval-title"), lineas.join("\n"))
}

/// Título+cuerpo de `Modal::MarkPattern` (#103 T9), factorizado fuera de
/// `draw_modal` (clippy `too_many_lines`). Texto libre, NO una superficie de
/// decisión de seguridad — sigue la MISMA disciplina que el resto
/// (enmascarado con `display_name`, jamás crudo): un patrón llega por paste
/// tan fácil como tecleado, y `PatternError` EMBEBE el patrón verbatim en su
/// mensaje (rustdoc de `PatternError::Glob`) — el enmascarado alcanza
/// también a la línea de error.
fn mark_pattern_modal_text(mark: bool, pattern: &str, error: Option<&str>) -> (String, String) {
    let (masked, hostil) = display_name(pattern.as_bytes());
    // #103 T9 review MINOR: `PaneState::mark_glob` compila el patrón CRUDO,
    // no el enmascarado — aquí el display difiere de verdad de lo que
    // decide el match, así que un patrón hostil lleva el mismo badge que un
    // nombre de fichero hostil (mismo idioma que `draw_search_dialog`'s
    // root line).
    let campo = if hostil {
        format!("{HOSTILE_BADGE} {masked}_")
    } else {
        format!("{masked}_")
    };
    // #103 T9 review MINOR: este modal no pasa por `DialogHints` (texto
    // libre, sin ALLOWLIST que generar un pie de página) — como
    // `search-hint`/`palette-hint`, sus teclas van fijas en Fluent.
    let mut lines = vec![
        campo,
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

/// Título+cuerpo de `Modal::Mkdir` (#104): mismo contrato de enmascarado
/// que `mark_pattern_modal_text` — nombre y diagnóstico son texto de
/// usuario (el error de `Segment::new`/del engine puede embeber el nombre).
fn mkdir_modal_text(name: &str, error: Option<&str>) -> (String, String) {
    let (masked, hostil) = display_name(name.as_bytes());
    let campo = if hostil {
        format!("{HOSTILE_BADGE} {masked}_")
    } else {
        format!("{masked}_")
    };
    let mut lines = vec![campo, t("modal-mkdir-hint"), t("modal-mark-pattern-keys")];
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(masked_err);
    }
    (t("modal-mkdir"), lines.join("\n"))
}

/// Título+cuerpo de `Modal::AiRenameInstruction` (M4-IA): mismo contrato de
/// enmascarado que `mkdir_modal_text` — la instrucción y el diagnóstico son
/// texto de usuario.
fn ai_rename_modal_text(instruction: &str, error: Option<&str>) -> (String, String) {
    let (masked, hostil) = display_name(instruction.as_bytes());
    let campo = if hostil {
        format!("{HOSTILE_BADGE} {masked}_")
    } else {
        format!("{masked}_")
    };
    // FIX-A (review T4): misma línea de teclas compartida que
    // `mkdir_modal_text` — así este modal cuadra con el brazo de altura
    // conjunto (7 con error / 6 sin él) en vez de pintar una línea menos.
    let mut lines = vec![
        campo,
        t("modal-ai-rename-hint"),
        t("modal-mark-pattern-keys"),
    ];
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(masked_err);
    }
    (t("modal-ai-rename"), lines.join("\n"))
}

/// Prefija el badge hostil FUERA de la traducción (audit MINOR-5: el
/// mecanismo del badge no puede depender de que cada locale conserve un
/// `{ $badge }` — concatenación Rust-side, translation-proof).
fn badge_prefixed(hostil: bool, line: String) -> String {
    if hostil {
        format!("{HOSTILE_BADGE}{line}")
    } else {
        line
    }
}

/// Título+cuerpo de `Modal::AiRenamePlan` (M4-IA, doctrina encoding-auditor):
/// primera línea = el dir OBJETIVO etiquetado fuera de banda (audit MAJOR-1
/// — el humano decide sabiendo DÓNDE aterriza el plan); después la VENTANA
/// de [`AI_RENAME_PAIR_LIMIT`] parejas desde `offset` (audit MAJOR-3: el
/// plan entero es revisable por scroll). Cada nombre en SU línea — el `from`
/// con etiqueta numerada ABSOLUTA fuera de banda (audit MINOR-4, corpus
/// `arrow_join_spoof`: un nombre puede imitar la flecha, no el `n.` al
/// margen), el `→` del destino al INICIO de su línea — elipsis media (un
/// `from` kilométrico no expulsa el `to` de la caja) y enmascarado MARCADO
/// con badge ([`badge_prefixed`], Rust-side). El indicador de desbordamiento
/// lleva badge si alguna pareja OCULTA es hostil (lo escondido no se cuela
/// limpio). Aunque el engine garantiza UTF-8 en el wire, un daemon
/// N+1/comprometido podría mandar cualquier cosa — se pinta a la defensiva
/// SIEMPRE, como el modal de aprobación.
fn ai_rename_plan_modal_text(
    dir: &norte_proto::VPath,
    entries: &[norte_proto::methods::AiRenameEntry],
    offset: usize,
) -> (String, String) {
    // Cinturón de render: el clamp vive en `App::ai_plan_scroll`, pero un
    // offset fuera de rango jamás debe pintar una ventana vacía.
    let offset = offset.min(entries.len().saturating_sub(AI_RENAME_PAIR_LIMIT));
    let last = (offset + AI_RENAME_PAIR_LIMIT).min(entries.len());
    let (dir_txt, dir_hostil) = norte_frontend::path_display(dir);
    let mut lines = vec![badge_prefixed(
        dir_hostil,
        ta(
            "modal-ai-rename-dir",
            &[("dir", &middle_ellipsis(&dir_txt, 46))],
        ),
    )];
    for (i, e) in entries.iter().enumerate().take(last).skip(offset) {
        let (from, from_hostil) = display_name(e.from.as_bytes());
        let (to, to_hostil) = display_name(e.to.as_bytes());
        lines.push(badge_prefixed(
            from_hostil,
            ta(
                "modal-ai-rename-pair-from",
                &[
                    ("n", &(i + 1).to_string()),
                    ("from", &middle_ellipsis(&from, 46)),
                ],
            ),
        ));
        lines.push(badge_prefixed(
            to_hostil,
            ta(
                "modal-ai-rename-pair-to",
                &[("to", &middle_ellipsis(&to, 44))],
            ),
        ));
    }
    if entries.len() > AI_RENAME_PAIR_LIMIT {
        let hidden_hostil = entries.iter().enumerate().any(|(i, e)| {
            (i < offset || i >= last)
                && (display_name(e.from.as_bytes()).1 || display_name(e.to.as_bytes()).1)
        });
        lines.push(badge_prefixed(
            hidden_hostil,
            ta(
                "modal-ai-rename-more",
                &[
                    ("shown", &last.to_string()),
                    ("total", &entries.len().to_string()),
                ],
            ),
        ));
    }
    lines.push(t("modal-ai-rename-plan-hint"));
    (t("modal-ai-rename-plan"), lines.join("\n"))
}

/// Título+cuerpo de `Modal::SemanticQuery` (M4-IA-2): mismo contrato de
/// enmascarado que `ai_rename_modal_text` — la consulta y el diagnóstico son
/// texto de usuario. Misma línea de teclas compartida (FIX-A): el modal
/// cuadra con el brazo de altura conjunto (7 con error / 6 sin él).
fn semantic_query_modal_text(query: &str, error: Option<&str>) -> (String, String) {
    let (masked, hostil) = display_name(query.as_bytes());
    let campo = if hostil {
        format!("{HOSTILE_BADGE} {masked}_")
    } else {
        format!("{masked}_")
    };
    let mut lines = vec![
        campo,
        t("modal-semantic-hint"),
        t("modal-mark-pattern-keys"),
    ];
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(masked_err);
    }
    (t("modal-semantic"), lines.join("\n"))
}

/// Título+cuerpo de `Modal::SemanticHits` (M4-IA-2, doctrina
/// encoding-auditor, molde `ai_rename_plan_modal_text`): la VENTANA de
/// [`SEMANTIC_HIT_LIMIT`] hits desde `offset`, un hit POR LÍNEA con marcador
/// de cursor (`>`) y etiqueta numerada ABSOLUTA fuera de banda, path por
/// `norte_frontend::path_display` (mask + flag hostil) con badge Rust-side
/// ([`badge_prefixed`]) y elipsis media (un path kilométrico no expulsa el
/// score de la caja); el score `{:.2}` al final. El indicador de
/// desbordamiento lleva badge si algún hit OCULTO es hostil (lo escondido no
/// se cuela limpio). Aunque el engine garantiza el wire, un daemon
/// N+1/comprometido podría mandar cualquier cosa — se pinta a la defensiva
/// SIEMPRE.
fn semantic_hits_modal_text(
    hits: &[norte_proto::methods::SemanticHit],
    offset: usize,
    cursor: usize,
) -> (String, String) {
    // Cinturón de render: el clamp vive en `App::semantic_cursor`, pero un
    // offset fuera de rango jamás debe pintar una ventana vacía.
    let offset = offset.min(hits.len().saturating_sub(SEMANTIC_HIT_LIMIT));
    let last = (offset + SEMANTIC_HIT_LIMIT).min(hits.len());
    let mut lines = Vec::new();
    for (i, h) in hits.iter().enumerate().take(last).skip(offset) {
        let (path, hostil) = norte_frontend::path_display(&h.path);
        let line = badge_prefixed(
            hostil,
            ta(
                "modal-semantic-hit",
                &[
                    ("n", &(i + 1).to_string()),
                    ("path", &middle_ellipsis(&path, 44)),
                    ("score", &format!("{:.2}", h.score)),
                ],
            ),
        );
        // Marcador de cursor FUERA de banda, en columna fija ANTES del badge
        // (un path no puede imitarlo: va enmascarado y tras la etiqueta).
        lines.push(if i == cursor {
            format!("> {line}")
        } else {
            format!("  {line}")
        });
    }
    if hits.len() > SEMANTIC_HIT_LIMIT {
        let hidden_hostil = hits
            .iter()
            .enumerate()
            .any(|(i, h)| (i < offset || i >= last) && norte_frontend::path_display(&h.path).1);
        lines.push(badge_prefixed(
            hidden_hostil,
            ta(
                "modal-semantic-more",
                &[
                    ("shown", &last.to_string()),
                    ("total", &hits.len().to_string()),
                ],
            ),
        ));
    }
    lines.push(t("modal-semantic-hits-hint"));
    (t("modal-semantic-hits"), lines.join("\n"))
}

/// Título+cuerpo de `Modal::TransferName` (#105): mismo contrato de
/// enmascarado que `mkdir_modal_text` — el dir destino, el nombre y el
/// diagnóstico son texto/bytes de usuario. El dir va en su propia línea
/// (jamás un joiner in-band con el nombre — disciplina de los modales de
/// #103).
fn transfer_name_modal_text(
    kind: crate::app::TransferKind,
    from: &norte_proto::VPath,
    to_dir: &norte_proto::VPath,
    name: &str,
    error: Option<&str>,
    enc: Option<norte_encoding::NameEncoding>,
) -> (String, String) {
    let (masked, hostil) = display_name(name.as_bytes());
    let campo = if hostil {
        format!("{HOSTILE_BADGE} {masked}_")
    } else {
        format!("{masked}_")
    };
    // #105 review MAJOR-2/MINOR-1: origen y dir destino, cada uno en SU
    // línea con la flecha fuera de banda, bajo la reinterpretación
    // CAPTURADA al abrir (#98/M1 — jamás la del pane al pintar).
    let badge_line = |p: &norte_proto::VPath| {
        let (line, hostil) = norte_frontend::path_display_with(p, enc);
        if hostil {
            format!("{HOSTILE_BADGE} {line}")
        } else {
            line
        }
    };
    let mut lines = vec![
        badge_line(from),
        format!("→ {}", badge_line(to_dir)),
        campo,
        t("modal-transfer-name-hint"),
        t("modal-mark-pattern-keys"),
    ];
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(masked_err);
    }
    let title = match kind {
        crate::app::TransferKind::Copy => t("modal-transfer-name-copy"),
        crate::app::TransferKind::Move => t("modal-transfer-name-move"),
    };
    (title, lines.join("\n"))
}

#[cfg(test)]
mod entry_item_columns_tests {
    use super::*;
    use norte_proto::{EntryKind, VPath};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::{List, Widget as _};

    /// review #108-5 M2: una decoración CJK (16 celdas) con la columna del
    /// nombre a su mínimo NO desplaza las celdas — la decoración cae antes
    /// que romper la alineación, y el ancho total de la fila es EXACTO.
    #[test]
    fn una_decoracion_ancha_jamas_desplaza_las_columnas() {
        use norte_frontend::columns::{Builtin, ColumnId, ColumnStyle, LayoutItem, WidthPolicy};
        let entry = norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse("mem:///f.txt").unwrap(),
            kind: EntryKind::File,
            size: Some(7),
            mtime_ms: None,
        };
        let deco = norte_frontend::Decoration {
            badge: Some("全全全全全全全全".to_owned()),
            role: None,
        };
        let theme = TuiTheme::default();
        let widths = [
            (
                ColumnId::Builtin(Builtin::Name),
                10u16,
                ColumnStyle::default_for(Builtin::Name),
            ),
            (
                ColumnId::Builtin(Builtin::Size),
                11u16,
                ColumnStyle::default_for(Builtin::Size),
            ),
        ];
        let _ = LayoutItem {
            policy: WidthPolicy::Auto,
            measured: 0,
            is_name: false,
        };
        let item = entry_item(&entry, &theme, None, Some(&deco), false, &widths, None, 0);
        // Renderiza a un buffer del ancho EXACTO del presupuesto: si la
        // fila desbordara, la celda de tamaño perdería su cola.
        let area = Rect::new(0, 0, 21, 1);
        let mut buf = Buffer::empty(area);
        List::new(vec![item]).render(area, &mut buf);
        let fila: String = (0..21).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(fila, "   f.txt          7 B", "{fila:?}");
    }
}

#[cfg(test)]
mod draw_pane_attr_tests {
    use super::*;
    use norte_proto::{Segment, VPath};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn entry(dir: &VPath, name: &str) -> norte_proto::Entry {
        norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        }
    }

    /// #117 tarea 2: celdas attr con valores HOSTILES de un provider pintadas
    /// end-to-end por `draw_pane` (config resuelta → layout → celda): jamás
    /// un char peligroso crudo, lossy MARCADO (U+FFFD) para Bytes no-UTF8,
    /// ausencia = blanco y cabecera con el id como fallback (sin catálogo).
    #[test]
    fn celdas_attr_hostiles_enmascaradas_y_ausencia_en_blanco() {
        use norte_proto::attrs::AttrValue;
        // Config: name + attr:mem.owner (Bytes no-UTF8) + attr:mem.note
        // (bidi RTL + ZWJ).
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "attr:mem.owner".into(),
                "attr:mem.note".into(),
            ]),
            ..Default::default()
        };
        let settings = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
        let dir = VPath::parse("mem:///d").unwrap();
        let mut e1 = entry(&dir, "aaa");
        e1.attrs.insert(
            "mem.owner".into(),
            AttrValue::Bytes(b"due\xf1o-\xff\xfe".to_vec()),
        );
        e1.attrs.insert(
            "mem.note".into(),
            AttrValue::Text("\u{202e}at\u{f3}n\u{202c} a\u{200d}b".into()),
        );
        let e2 = entry(&dir, "bbb"); // SIN attrs: celdas en blanco
        let pane = Pane::new(dir, vec![e1, e2]);
        let theme = TuiTheme::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).expect("terminal de test");
        terminal
            .draw(|f| draw_pane(f, f.area(), &pane, true, &theme, 0, &settings, None))
            .expect("draw");
        let text = terminal.backend().to_string();
        // 1. Ninguna celda del buffer lleva un char peligroso crudo
        //    (controles, overrides bidi, invisibles — spec §6). Por línea:
        //    los `\n` que une `to_string` son del harness, no del buffer.
        assert!(
            text.lines()
                .all(|l| l.chars().all(|c| !norte_encoding::is_terminal_hazard(c))),
            "hazard crudo en el render: {text:?}"
        );
        // 2. La fila de e1 pinta el owner LOSSY y MARCADO (U+FFFD visible).
        let fila_e1 = text
            .lines()
            .find(|l| l.contains("aaa"))
            .expect("fila de aaa");
        assert!(
            fila_e1.contains('\u{FFFD}'),
            "owner lossy sin marcar: {fila_e1:?}"
        );
        // 3. La fila de e2 (sin attrs) pinta las columnas attr EN BLANCO:
        //    quitando el nombre, los bordes y los espacios no queda nada
        //    (blanco = AUSENTE, jamás un valor fabricado).
        let fila_e2 = text
            .lines()
            .find(|l| l.contains("bbb"))
            .expect("fila de bbb");
        // (Las comillas por línea las pone el Display de `TestBackend`.)
        let resto: String = fila_e2
            .replace("bbb", "")
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '│' && *c != '"')
            .collect();
        assert_eq!(resto, "", "ausencia debe ser blanco: {fila_e2:?}");
        // 4. La cabecera lleva el id como fallback (sin catálogo aquí).
        assert!(text.contains("mem.owner"), "cabecera sin id: {text}");
    }

    /// #117 encoding-audit L2: una celda attr ANCHA (CJK double-width + la
    /// familia emoji ZWJ del corpus — el valor `mem.wide` de `MemProvider`)
    /// JAMÁS desplaza la columna vecina: la x de la celda del tamaño es
    /// idéntica entre la fila ancha y una fila en blanco (espejo de
    /// `una_decoracion_ancha_jamas_desplaza_las_columnas`). Lo pineado es
    /// la alineación de celdas del buffer de ratatui; el colapso de ZWJ en
    /// un terminal real es la limitación preexistente que ya comparte la
    /// columna del nombre.
    #[test]
    fn celda_attr_ancha_jamas_desplaza_la_columna_vecina() {
        use norte_proto::attrs::AttrValue;
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["name".into(), "attr:mem.wide".into(), "size".into()]),
            ..Default::default()
        };
        let settings = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
        let dir = VPath::parse("mem:///d").unwrap();
        let mut e1 = entry(&dir, "aaa");
        e1.attrs.insert(
            "mem.wide".into(),
            AttrValue::Text("日本語👨\u{200d}👩\u{200d}👧\u{200d}👦".into()),
        );
        let e2 = entry(&dir, "bbb"); // SIN attrs: la celda ancha en blanco
        let pane = Pane::new(dir, vec![e1, e2]);
        let theme = TuiTheme::default();
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).expect("terminal de test");
        terminal
            .draw(|f| draw_pane(f, f.area(), &pane, true, &theme, 0, &settings, None))
            .expect("draw");
        let buf = terminal.backend().buffer();
        // La x (en CELDAS del buffer, no chars) del «1» del tamaño en la
        // fila que contiene `name`.
        let size_x = |name: &str| -> u16 {
            for y in 0..buf.area.height {
                let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                if row.contains(name) {
                    for x in 0..buf.area.width {
                        if buf[(x, y)].symbol() == "1" {
                            return x;
                        }
                    }
                }
            }
            panic!("fila {name} sin celda de tamaño");
        };
        assert_eq!(
            size_x("aaa"),
            size_x("bbb"),
            "la celda ancha desplazó la columna del tamaño"
        );
    }
}

#[cfg(test)]
mod transfer_name_modal_text_tests {
    use super::transfer_name_modal_text;
    use crate::app::TransferKind;
    use norte_proto::VPath;

    /// #105 review MINOR-2 (misma clase que el M4 del patrón): fn PURA — un
    /// RLO crudo en nombre y error sale enmascarado, y un byte hostil en el
    /// ORIGEN y el dir destino jamás llega crudo (`path_display` los enmascara
    /// y llevan badge).
    #[test]
    fn masks_every_user_surface() {
        let hostile = "abc\u{202E}rid";
        let from = VPath::parse("mem:///src/a%FF.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst%FE").unwrap();
        let (_, cuerpo) = transfer_name_modal_text(
            TransferKind::Move,
            &from,
            &to_dir,
            hostile,
            Some(hostile),
            None,
        );
        assert!(!cuerpo.contains('\u{202E}'), "{cuerpo:?}");
        assert!(
            cuerpo.matches('\u{FFFD}').count() >= 4,
            "nombre + error (RLO) y origen + destino (bytes): {cuerpo:?}"
        );
        assert!(
            cuerpo.matches(super::HOSTILE_BADGE).count() >= 2,
            "{cuerpo:?}"
        );
    }
}

#[cfg(test)]
mod mkdir_modal_text_tests {
    use super::mkdir_modal_text;

    /// Mismo pin que el del patrón (#103 M4): fn PURA — un RLO crudo en el
    /// nombre Y en el error sale enmascarado en AMBAS líneas.
    #[test]
    fn masks_a_raw_rtl_override_in_name_and_error() {
        let hostile = "abc\u{202E}rid";
        let (_, cuerpo) = mkdir_modal_text(hostile, Some(hostile));
        assert!(!cuerpo.contains('\u{202E}'), "{cuerpo:?}");
        assert_eq!(cuerpo.matches('\u{FFFD}').count(), 2, "{cuerpo:?}");
    }
}

#[cfg(test)]
mod mark_pattern_modal_text_tests {
    use super::mark_pattern_modal_text;

    /// Review MAJOR M4: `mark_pattern_modal_text` es pura — testear el
    /// enmascarado directamente en vez de a través de un buffer
    /// `TestBackend`, donde el renderer de párrafo de ratatui se COME los
    /// grafemas de ancho cero: U+202E jamás sobrevive AHÍ, enmascarado o
    /// no, así que una aserción de test de render contra él no puede fallar
    /// nunca (la clase de bug que motivó este test). Un patrón Y un error
    /// que llevan un RLO crudo deben salir enmascarados los DOS: ni un
    /// U+202E sobrevive, y U+FFFD aparece exactamente dos veces — una por
    /// línea enmascarada.
    #[test]
    fn masks_a_raw_rtl_override_in_both_the_pattern_and_the_error() {
        let hostile = "abc\u{202E}gpj.exe";
        let (_, cuerpo) = mark_pattern_modal_text(true, hostile, Some(hostile));
        assert!(
            !cuerpo.contains('\u{202E}'),
            "raw RTL override must not survive: {cuerpo:?}"
        );
        assert_eq!(
            cuerpo.matches('\u{FFFD}').count(),
            2,
            "one U+FFFD per masked line (pattern + error): {cuerpo:?}"
        );
    }

    /// Sin error, solo la línea del patrón se enmascara: un solo U+FFFD.
    #[test]
    fn masks_only_the_pattern_line_when_there_is_no_error() {
        let hostile = "abc\u{202E}gpj.exe";
        let (_, cuerpo) = mark_pattern_modal_text(true, hostile, None);
        assert_eq!(cuerpo.matches('\u{FFFD}').count(), 1);
    }
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
    hint: &str,
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
        hint.to_owned(),
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

/// Elipsis MEDIA por ancho de celda: AHORA vive en `norte-frontend`
/// (encoding audit M4-IA-2 H1) — el invariante «un path kilométrico jamás
/// expulsa el campo que va detrás» no es propio de un terminal, la GUI lo
/// necesitaba igual. Re-import local para que todo el módulo (y sus tests)
/// la llame por su nombre corto, sin cambiar una sola salida de render.
use norte_frontend::middle_ellipsis;

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

/// Columnas VIVAS de un pane con su estilo resuelto (#108 7b, #117 sobre
/// `ColumnId`): los anchos del layout compartido más `style_for_id`, UNA
/// vez por columna y por frame (`style_for_id` pliega mapas y clona el
/// header — por fila × columna sería O(filas × columnas) de lookups
/// idénticos). El catálogo viene del cache por scheme de `App` (#117
/// tarea 2): refina los defaults de las columnas attr (hint); `None` =
/// aún no llegó o falló — defaults Opaque, jamás bloquea el render.
fn styled_columns(
    settings: &norte_frontend::columns::ColumnsSettings,
    scheme: &str,
    inner_w: u16,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> Vec<(
    norte_frontend::columns::ColumnId,
    u16,
    norte_frontend::columns::ColumnStyle,
)> {
    norte_frontend::columns::column_widths(settings, scheme, inner_w)
        .into_iter()
        .map(|(id, w)| {
            let s = settings.style_for_id(scheme, &id, catalog);
            (id, w, s)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)] // wiring del render, no API
fn draw_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    pane: &Pane,
    focused: bool,
    theme: &TuiTheme,
    now_ms: i64,
    settings: &norte_frontend::columns::ColumnsSettings,
    catalog: Option<&norte_proto::AttrCatalog>,
) {
    let border_style = if focused {
        theme.role(Role::BorderFocus)
    } else {
        theme.role(Role::BorderUnfocused)
    };
    let (title, title_hostil) = norte_frontend::path_display_with(pane.dir(), pane.name_encoding());
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
    let reinterpret = pane.name_encoding();
    // #108 L5: anchos de columna del ancho INTERIOR del pane, una vez por
    // frame — las filas y la cabecera comparten el mismo layout (con el
    // estilo 7b resuelto por columna, ver `styled_columns`).
    let inner_w = block.inner(area).width;
    let cols = &styled_columns(settings, pane.dir().scheme(), inner_w, catalog);
    let (items, selected): (Vec<ListItem<'_>>, Option<usize>) = match pane.quick_visible() {
        Some(vis) => (
            vis.iter()
                .filter_map(|&i| pane.entries().get(i))
                .map(|e| {
                    entry_item(
                        e,
                        theme,
                        reinterpret,
                        pane.decoration_for(&e.path),
                        pane.is_marked(e),
                        cols,
                        Some(pane),
                        now_ms,
                    )
                })
                .collect(),
            pane.quick()
                .and_then(crate::nav::QuickSearch::selected_entry_index)
                .and_then(|s| vis.iter().position(|&i| i == s)),
        ),
        None => (
            pane.entries()
                .iter()
                .map(|e| {
                    entry_item(
                        e,
                        theme,
                        reinterpret,
                        pane.decoration_for(&e.path),
                        pane.is_marked(e),
                        cols,
                        Some(pane),
                        now_ms,
                    )
                })
                .collect(),
            (!pane.entries().is_empty()).then_some(pane.cursor()),
        ),
    };
    // #108 L5: bloque a mano — dentro, UNA línea de cabecera de columnas
    // (dim, con el indicador ▲/▼ del orden activo) y el listado debajo.
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let (header_area, list_area) = {
        let mut cab = inner;
        cab.height = 1;
        let mut lst = inner;
        lst.y = inner.y.saturating_add(1);
        lst.height = inner.height.saturating_sub(1);
        (cab, lst)
    };
    frame.render_widget(
        Paragraph::new(column_header_line(cols, pane.sort(), catalog))
            .style(ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM)),
        header_area,
    );
    let list = List::new(items).highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(list, list_area, &mut state);
}

#[allow(clippy::too_many_arguments)] // fila de render: cada arg es una fuente de pintado, no API
fn entry_item<'a>(
    entry: &'a norte_proto::Entry,
    theme: &TuiTheme,
    reinterpret: Option<norte_encoding::NameEncoding>,
    decoration: Option<&norte_frontend::Decoration>,
    marked: bool,
    cols: &[(
        norte_frontend::columns::ColumnId,
        u16,
        norte_frontend::columns::ColumnStyle,
    )],
    // #117-follow-up: fuente de las celdas `plugin:` (side-map del pane —
    // sus valores no viven en la `Entry`). `None` solo en tests de formato
    // sin columnas de plugin.
    plugin_cells: Option<&Pane>,
    now_ms: i64,
) -> ListItem<'a> {
    let name = entry.path.file_name().map_or(&[][..], |n| n.as_bytes());
    // #57: con reinterpretación activa, los nombres no-UTF8 se decodifican
    // con el encoding elegido (display-only; el badge hostil se conserva —
    // el texto pintado difiere de los bytes reales).
    let (texto, hostil) = norte_frontend::display_name_with(name, reinterpret);
    let kind_glyph = match entry.kind {
        EntryKind::Dir => "/",
        EntryKind::Symlink => "@",
        EntryKind::File | EntryKind::Other => " ",
    };
    let badge = Span::styled(
        if hostil { HOSTILE_BADGE } else { " " },
        theme.role(Role::HostileBadge),
    );
    // Color por tipo/extensión de la entrada (ADR 0020 D2).
    let body = Span::styled(
        format!("{kind_glyph}{texto}"),
        theme.entry(name, entry.kind),
    );
    // Canalón de marca (#103): señal TEXTUAL, jamás solo color — el fallback
    // monocromo de `Role::Mark` es `dim`, que por sí solo se lee «inactivo»,
    // no «seleccionado». Va ANTES del badge hostil para que ni el badge ni la
    // decoración cambien de columna respecto a como se pintaban.
    //
    // El ESTILO también debe ser condicional, no solo el glyph (review
    // BLOCKER): cada preset embarcado define `mark` como SOLO un `bg` (ver
    // `crates/norte-theme/presets/*.toml`), así que un `Span::styled`
    // incondicional pintaba esa franja de color en la columna 1 de CADA fila
    // sin marcar — una franja permanente, no una señal de marca.
    let gutter = if marked {
        Span::styled("*", theme.role(Role::Mark))
    } else {
        Span::raw(" ")
    };
    let mut spans = vec![gutter, badge, body];
    // G3b (ADR 0037): badge de decorator, TRAS el hueco del badge hostil —
    // ya SANEADO y acotado (`norte_frontend::sanitize_decoration`, aplicado
    // antes de llegar aquí). Sin decoración para esta entrada, ningún span
    // extra (ni siquiera un hueco): la fila se ve EXACTAMENTE igual que
    // antes de G3b para quien no usa decoradores.
    if let Some(badge_text) = decoration.and_then(|d| d.badge.as_deref()) {
        let style = match decoration.and_then(|d| d.role) {
            Some(role) => theme.role(role),
            // Sin rol reconocido: dim por defecto — visible pero discreto,
            // nunca el color "normal" de la entrada (se confundiría con el
            // nombre) ni un color inventado por este frontend (ADR 0037: el
            // tema del usuario manda, jamás un color crudo que no pidió).
            None => ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM),
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(badge_text.to_string(), style));
    }
    // #108 L5: celdas de columnas tras el nombre. El bloque del nombre
    // (canalón+badge+glyph+texto+decoración) se TRUNCA a su ancho de layout
    // (elipsis central, consciente de celdas — CJK/emoji no desbordan) y
    // se rellena; cada celda no-nombre va alineada según su estilo (#108
    // 7b, derecha por defecto) en su ancho, dim, con un espacio separador.
    // Ausencia = celda en blanco, jamás un 0 fabricado.
    if let Some((_, name_w, _)) = cols.first() {
        let name_w = usize::from(*name_w);
        // review #108-5 M2: la DECORACIÓN también entra en el presupuesto
        // del nombre — un badge CJK (8 chars = 16 celdas) desplazaba todas
        // las celdas de la fila. Si no cabe dejando ≥3 celdas de nombre,
        // fuera la decoración entera (separador incluido): el nombre manda.
        if spans.len() > 3 {
            let deco: usize = spans[3..].iter().map(|sp| sp.content.width()).sum();
            let fijos: usize = spans[..2].iter().map(|sp| sp.content.width()).sum();
            if fijos + deco + 3 > name_w {
                spans.truncate(3);
            }
        }
        let usado: usize = spans.iter().map(|sp| sp.content.width()).sum();
        if usado > name_w {
            // Recorta el TEXTO del nombre (el span del body, índice 2) con
            // elipsis central a lo que quede tras los demás spans — los
            // fijos (canalón/badge) y la decoración se quedan.
            let otros: usize = spans
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != 2)
                .map(|(_, sp)| sp.content.width())
                .sum();
            let body_w = name_w.saturating_sub(otros);
            let recortado = middle_ellipsis(&spans[2].content, body_w);
            spans[2] = Span::styled(recortado, spans[2].style);
        }
        let usado: usize = spans.iter().map(|sp| sp.content.width()).sum();
        debug_assert!(
            usado <= name_w,
            "el bloque del nombre desborda su columna: {usado} > {name_w}"
        );
        if usado < name_w {
            spans.push(Span::raw(" ".repeat(name_w - usado)));
        }
        for (col, w, style) in cols.iter().skip(1) {
            // #117-follow-up: las celdas `plugin:` salen del side-map del
            // pane (re-enmascaradas allí); el resto, de la Entry como
            // siempre. Ausencia = blanco en ambos caminos.
            let cell = match col {
                norte_frontend::columns::ColumnId::Plugin { .. } => plugin_cells
                    .and_then(|p| p.plugin_cell(&col.to_string(), &entry.path))
                    .unwrap_or_default(),
                _ => norte_frontend::columns::styled_cell(entry, col, now_ms, style)
                    .unwrap_or_default(),
            };
            // El ancho INCLUYE el separador (default_layout_items): el
            // contenido vive dentro de w-1 y siempre queda ≥1 espacio de
            // separador. Derecha (default): relleno delante. Izquierda
            // (#108 7b): el separador sigue ABRIENDO el presupuesto, el
            // contenido va tras él y el relleno cae a la derecha — la
            // misma cuenta, invertida.
            let w = usize::from(*w);
            let contenido = w.saturating_sub(1);
            let cw = cell.width();
            let recortada: String = if cw > contenido {
                take_width(&cell, contenido)
            } else {
                cell
            };
            let texto = match style.align {
                norte_frontend::columns::Align::Right => {
                    let pad = w.saturating_sub(recortada.width());
                    format!("{}{recortada}", " ".repeat(pad))
                }
                norte_frontend::columns::Align::Left => {
                    let pad = w.saturating_sub(recortada.width().saturating_add(1));
                    format!(" {recortada}{}", " ".repeat(pad))
                }
            };
            spans.push(Span::styled(
                texto,
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM),
            ));
        }
    }
    ListItem::new(Line::from(spans))
}

#[cfg(test)]
mod entry_item_tests {
    use super::{HOSTILE_BADGE, entry_item};
    use crate::theme::TuiTheme;
    use norte_proto::{Entry, EntryKind, VPath};
    use ratatui::widgets::ListItem;

    fn e(wire: &str, k: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).unwrap(),
            kind: k,
            size: None,
            mtime_ms: None,
        }
    }

    /// Nombre no-UTF8 (bytes crudos vía `Segment`): dispara el badge hostil
    /// sin pasar por reinterpretación.
    fn e_hostile() -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse("mem:///")
                .unwrap()
                .join(norte_proto::Segment::new(b"\xFF\xFE".to_vec()).unwrap()),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        }
    }

    /// `ListItem`'s span content is private to ratatui, so — like the
    /// crate's other render tests (`tests/theme_render.rs`,
    /// `tests/render.rs`) — this renders the row into a real `Buffer` and
    /// reads it back cell by cell. The gutter and the hostile badge are each
    /// exactly one cell wide by construction, so `span_texts()[0]` and `[1]`
    /// are the true first two spans' text; later cells belong to the
    /// (possibly multi-char) name span and are not meant to be compared
    /// one-for-one with spans.
    fn span_texts(item: &ListItem<'_>) -> Vec<String> {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::{List, Widget as _};
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        List::new(vec![item.clone()]).render(area, &mut buf);
        (0..area.width)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect()
    }

    fn first_span_text(item: &ListItem<'_>) -> String {
        span_texts(item).into_iter().next().unwrap_or_default()
    }

    /// A marked row carries a TEXTUAL cue, never colour alone: `Role::Mark`'s
    /// monochrome fallback is `dim`, which on its own reads as "inactive"
    /// rather than "selected" (#103).
    #[test]
    fn a_marked_row_starts_with_the_mark_gutter() {
        let entry = e("mem:///a", EntryKind::File);
        let theme = TuiTheme::default();
        let marked = entry_item(&entry, &theme, None, None, true, &[], None, 0);
        let plain = entry_item(&entry, &theme, None, None, false, &[], None, 0);
        assert_eq!(first_span_text(&marked), "*");
        assert_eq!(first_span_text(&plain), " ");
    }

    /// The gutter goes BEFORE the hostile badge, so the badge column and the
    /// decorator badge keep the positions they have today.
    #[test]
    fn the_gutter_precedes_the_hostile_badge() {
        let entry = e_hostile();
        let theme = TuiTheme::default();
        let item = entry_item(&entry, &theme, None, None, true, &[], None, 0);
        let texts = span_texts(&item);
        assert_eq!(texts[0], "*");
        assert_eq!(texts[1], HOSTILE_BADGE);
    }
}

/// Segmentos `(marked, pruned)` de la status bar sobre las marcas (#103).
/// Extraído de `draw_status` (que ya rozaba `too_many_lines`) — pura
/// composición de texto, sin efecto de render.
fn marks_status_segments(pane: &Pane) -> (String, String) {
    // Un refresh que se comió marcas JAMÁS es silencioso: con la selección
    // vacía, `marked_paths` cae al cursor, así que callarlo redirigiría la
    // siguiente op en masa a algo que nadie marcó.
    let pruned = if pane.pruned_marks() == 0 {
        String::new()
    } else {
        format!(
            "  {}",
            ta(
                "status-marks-pruned",
                &[("n", &pane.pruned_marks().to_string())]
            )
        )
    };
    // Cuántas marcas y cuánto pesan. Se calla con 0 marcas — la barra no
    // gana ruido para quien no marca nada.
    let marked = if pane.marks_len() == 0 {
        String::new()
    } else {
        let n = pane.marks_len().to_string();
        let size = norte_frontend::human_bytes(pane.marked_bytes());
        let dirs = pane.marked_dirs();
        if dirs == 0 {
            format!("  {}", ta("status-marked", &[("n", &n), ("size", &size)]))
        } else {
            format!(
                "  {}",
                ta(
                    "status-marked-with-dirs",
                    &[("n", &n), ("size", &size), ("dirs", &dirs.to_string())],
                )
            )
        }
    };
    (marked, pruned)
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
    let (marked, pruned) = marks_status_segments(pane);
    let (dir_texto, dir_hostil) =
        norte_frontend::path_display_with(pane.dir(), pane.name_encoding());
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
            // #81: contexto del match de contenido del hit BAJO EL CURSOR
            // (línea + preview — saneado en origen por el core; se pasa por
            // detail_for_bar como cinturón, mismo criterio que los errores).
            let hit = pane
                .entries()
                .get(pane.cursor())
                .and_then(|e| pane.search_matches.get(&e.path))
                .map_or_else(String::new, |m| {
                    let linea = m.line.map_or_else(String::new, |l| format!(":{l}"));
                    let preview = m.preview.as_deref().map_or_else(String::new, |p| {
                        format!(" {}", crate::app::detail_for_bar(p))
                    });
                    format!("  [{linea}{preview}]")
                });
            format!(
                " {}{hit}{seq}",
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
        let omitidas = match pane.skipped() {
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
        let nombres = match pane.name_encoding() {
            Some(enc) => format!("  {}", ta("status-names-encoding", &[("enc", enc.label())])),
            None => String::new(),
        };
        // #107: ocultación activa con entradas apartadas — misma disciplina
        // que `omitidas`: un listado que enseña menos de lo que hay jamás
        // es silencioso. Se calla con 0 apartadas (dir sin dotfiles) y con
        // la ocultación apagada. Va DETRÁS de `pruned` en la línea (#107
        // review MINOR-3): ocultar con marcas produce ambos, y el aviso de
        // poda es el que no puede recortarse primero.
        let ocultas = match pane.hidden_count() {
            0 => String::new(),
            n => format!("  {}", ta("status-hidden", &[("n", &n.to_string())])),
        };
        // Review MAJOR M3: los AVISOS (`omitidas` — listado incompleto,
        // "jamás silencioso" — y `nombres` — el badge de reinterpretación,
        // "el usuario debe saberlo en todo momento") van ANTES que el
        // contador informativo de marcas. La línea no tiene presupuesto de
        // ancho y ratatui recorta la cola: con `marked`/`pruned` primero (25+
        // celdas fácil) un path largo a 80 columnas empujaba el badge de
        // encoding fuera del recorte. Deuda real (#103): un presupuesto de
        // ancho que elipsise `dir_texto` para que NINGÚN campo posterior se
        // recorte jamás, en vez de solo reordenar por prioridad.
        format!(" {marca}{dir_texto}{pos_total}{omitidas}{nombres}{pruned}{ocultas}{marked}{seq}")
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

    /// P1 encoding audit F2 (LOW): un flood de combining marks (ancho CERO
    /// cada uno) desborda el caminante por celdas SIN nunca tocar su
    /// presupuesto — el early-return de ancho, o el propio caminante,
    /// podían devolver/procesar el string ENTERO sin acotar, con `max`
    /// celdas satisfecho pero el tamaño real sin tope. `nfd_e_acute` del
    /// corpus (`e` + combining acute) es el par base+combining canónico —
    /// aquí se inunda a 100 000× para ejercer el backstop por CUENTA de
    /// chars, no solo por ancho.
    #[test]
    fn flood_de_combining_marks_no_desborda() {
        let fixture = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "nfd_e_acute")
            .expect("fixture del corpus");
        let texto = String::from_utf8(fixture.bytes).expect("nfd_e_acute es UTF-8 válido");
        let (base, combining) = texto.split_at(1); // "e" + "\u{0301}"
        let flood: String = std::iter::once(base)
            .chain(std::iter::repeat_n(combining, 100_000))
            .collect();
        assert_eq!(flood.width(), 1, "control: el flood entero pesa 1 celda");
        let out = middle_ellipsis(&flood, 10);
        // Cota: el backstop pre-recorta a `char_cap = 4*max` chars, pero el
        // caminante de cabeza Y el de cola operan cada uno sobre TODO ese
        // precorte (no sobre mitades separadas) — con ancho cero ninguno
        // frena por presupuesto, así que cada uno puede consumirlo entero.
        // Bounded (2*char_cap + 1), no perfecto — lo que pide F2 (LOW) es
        // dejar de ser ILIMITADO, no una cota ajustada.
        let cota = 2 * (10 * 4) + 1;
        assert!(
            out.chars().count() <= cota,
            "el backstop de cuenta de chars no acotó la salida: {} chars (cota {cota})",
            out.chars().count()
        );
    }
}

#[cfg(test)]
mod ai_rename_plan_modal_tests {
    use super::{HOSTILE_BADGE, ai_rename_plan_modal_text, display_name, modal_height};
    use norte_proto::VPath;
    use norte_proto::methods::AiRenameEntry;

    fn dir() -> VPath {
        VPath::parse("mem:///proyecto").expect("wire válido")
    }

    fn entry(from: &str, to: &str) -> AiRenameEntry {
        AiRenameEntry {
            from: from.into(),
            to: to.into(),
        }
    }

    /// Audit MINOR-6a (corpus canónico, molde del sweep de `app.rs`): cada
    /// nombre hostil, en la posición `from` Y en la `to` — ningún char de
    /// `is_terminal_hazard` sobrevive en el texto pintado, y cuando el
    /// enmascarado altera el nombre la línea va MARCADA con el badge.
    #[test]
    fn barrido_corpus_ningun_hazard_sobrevive_y_el_enmascarado_marca() {
        for n in norte_testkit::corpus::hostile_names() {
            let name = String::from_utf8_lossy(&n.bytes).into_owned();
            let casos = [
                (name.clone(), "limpio.txt".to_owned()),
                ("limpio.txt".to_owned(), name.clone()),
            ];
            for (from, to) in casos {
                let hostil = display_name(from.as_bytes()).1 || display_name(to.as_bytes()).1;
                let (_, body) = ai_rename_plan_modal_text(&dir(), &[entry(&from, &to)], 0);
                // Por LÍNEA: el `\n` que separa las líneas del cuerpo es un
                // control legítimo del formato, no contenido pintado.
                assert!(
                    !body
                        .lines()
                        .any(|l| l.chars().any(norte_encoding::is_terminal_hazard)),
                    "corpus {}: un hazard sobrevivió al render: {body:?}",
                    n.id
                );
                if hostil {
                    assert!(
                        body.contains(HOSTILE_BADGE),
                        "corpus {}: enmascarado SIN badge: {body:?}",
                        n.id
                    );
                }
            }
        }
    }

    /// Audit MINOR-4 (corpus `arrow_join_spoof`): un `from` que IMITA la
    /// flecha no fabrica una pareja falsa — el `from` lleva su etiqueta
    /// numerada fuera de banda en SU línea y el destino REAL conserva la
    /// suya con la flecha al inicio.
    #[test]
    fn arrow_join_spoof_no_fabrica_pareja() {
        let spoof = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "arrow_join_spoof")
            .expect("fixture del corpus");
        let from = String::from_utf8_lossy(&spoof.bytes).into_owned();
        let (_, body) = ai_rename_plan_modal_text(&dir(), &[entry(&from, "real.txt")], 0);
        let lines: Vec<&str> = body.lines().collect();
        // dir + from + to + hint = 4 líneas exactas: el spoof no añade una.
        assert_eq!(lines.len(), 4, "{body:?}");
        assert!(lines[1].contains("1."), "etiqueta fuera de banda: {body:?}");
        assert!(
            lines[2].starts_with('→') && lines[2].contains("real.txt"),
            "el destino real conserva SU línea: {body:?}"
        );
    }

    /// Audit MINOR-6c: un destino hostil (RLO del corpus) se enmascara y su
    /// línea va marcada — el badge antecede incluso a la flecha.
    #[test]
    fn destino_hostil_enmascara_y_marca() {
        let rtl = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("fixture del corpus");
        let to = String::from_utf8_lossy(&rtl.bytes).into_owned();
        let (_, body) = ai_rename_plan_modal_text(&dir(), &[entry("limpio.txt", &to)], 0);
        let to_line = body.lines().nth(2).expect("línea del destino");
        assert!(to_line.starts_with(HOSTILE_BADGE), "{body:?}");
        assert!(to_line.contains('\u{FFFD}'), "{body:?}");
        assert!(
            !to_line.chars().any(norte_encoding::is_terminal_hazard),
            "{body:?}"
        );
    }

    /// Audit MAJOR-3: con 7 parejas la ventana pinta 5 desde `offset` con
    /// numeración ABSOLUTA, el indicador dice posición/total y el alto del
    /// modal cuadra con las líneas pintadas.
    #[test]
    fn plan_largo_ventana_indicador_y_alto() {
        let entries: Vec<AiRenameEntry> = (1..=7)
            .map(|i| entry(&format!("f{i}"), &format!("t{i}")))
            .collect();
        let (_, body) = ai_rename_plan_modal_text(&dir(), &entries, 0);
        let lines: Vec<&str> = body.lines().collect();
        // dir + 5 parejas × 2 + indicador + hint = 13.
        assert_eq!(lines.len(), 13, "{body:?}");
        assert!(
            lines[1].contains("1.") && lines[1].contains("f1"),
            "{body:?}"
        );
        assert!(lines[11].contains("5/7"), "indicador: {body:?}");
        assert!(!body.contains("f6"), "la cola espera al scroll: {body:?}");
        // offset 2 = parejas 3..=7, numeración absoluta, indicador al tope.
        let (_, body2) = ai_rename_plan_modal_text(&dir(), &entries, 2);
        let lines2: Vec<&str> = body2.lines().collect();
        assert_eq!(lines2.len(), 13, "alto ESTABLE al scroll: {body2:?}");
        assert!(
            lines2[1].contains("3.") && lines2[1].contains("f3"),
            "{body2:?}"
        );
        assert!(body2.contains("f7"), "{body2:?}");
        assert!(lines2[11].contains("7/7"), "{body2:?}");
        // Un offset desbocado se clampa en el render (cinturón).
        let (_, body3) = ai_rename_plan_modal_text(&dir(), &entries, 999);
        assert!(body3.contains("f7"), "{body3:?}");
        // Alto: 13 líneas de cuerpo + 3 de marco.
        let modal = crate::app::Modal::AiRenamePlan {
            dir: dir(),
            entries,
            offset: 0,
        };
        assert_eq!(modal_height(&modal), 16);
    }

    /// Audit MAJOR-3: el indicador de desbordamiento delata una pareja
    /// hostil OCULTA (lo no visible jamás se cuela "limpio"), y deja de
    /// marcar cuando el scroll la pone a la vista.
    #[test]
    fn indicador_marca_hostil_oculto() {
        let mut entries: Vec<AiRenameEntry> = (1..=6)
            .map(|i| entry(&format!("f{i}"), &format!("t{i}")))
            .collect();
        entries[5] = entry("x\u{202e}y", "limpio.txt");
        let (_, body) = ai_rename_plan_modal_text(&dir(), &entries, 0);
        let ind = body.lines().nth(11).expect("indicador");
        assert!(ind.starts_with(HOSTILE_BADGE), "{body:?}");
        // offset 1: la hostil entra en la ventana; la oculta (pareja 1) es
        // limpia — el indicador ya no marca.
        let (_, body2) = ai_rename_plan_modal_text(&dir(), &entries, 1);
        let ind2 = body2.lines().nth(11).expect("indicador");
        assert!(!ind2.starts_with(HOSTILE_BADGE), "{body2:?}");
    }
}

#[cfg(test)]
mod semantic_hits_modal_tests {
    use super::{HOSTILE_BADGE, modal_height, semantic_hits_modal_text};
    use norte_proto::methods::SemanticHit;
    use norte_proto::{Segment, VPath};

    fn hit(path: VPath, score: f64) -> SemanticHit {
        SemanticHit { path, score }
    }

    fn hits(n: u16) -> Vec<SemanticHit> {
        (1..=n)
            .map(|i| {
                hit(
                    VPath::parse(&format!("mem:///d/f{i}")).expect("wire válido"),
                    1.0 - f64::from(i) / 100.0,
                )
            })
            .collect()
    }

    /// M4-IA-2 (corpus canónico, molde del sweep del plan IA): cada nombre
    /// hostil como último segmento del path de un hit — ningún char de
    /// `is_terminal_hazard` sobrevive en el texto pintado, y cuando el
    /// enmascarado altera el path la línea va MARCADA con el badge.
    #[test]
    fn barrido_corpus_ningun_hazard_sobrevive_y_el_enmascarado_marca() {
        for n in norte_testkit::corpus::hostile_names() {
            let path = VPath::parse("mem:///d")
                .expect("wire válido")
                .join(Segment::new(n.bytes.clone()).expect("segmento del corpus"));
            let hostil = norte_frontend::path_display(&path).1;
            let (_, body) = semantic_hits_modal_text(&[hit(path, 0.5)], 0, 0);
            // Por LÍNEA: el `\n` que separa las líneas del cuerpo es un
            // control legítimo del formato, no contenido pintado.
            assert!(
                !body
                    .lines()
                    .any(|l| l.chars().any(norte_encoding::is_terminal_hazard)),
                "corpus {}: un hazard sobrevivió al render: {body:?}",
                n.id
            );
            if hostil {
                assert!(
                    body.contains(HOSTILE_BADGE),
                    "corpus {}: enmascarado SIN badge: {body:?}",
                    n.id
                );
            }
        }
    }

    /// Encoding audit M4-IA-2 S1 (fixture `score_spoof_inband`): un nombre
    /// que IMITA la columna de score (`informe · 0.99.txt`: middle dot +
    /// decimales, todo imprimible — NO hay badge que avise) jamás desplaza
    /// al score REAL. Se pinea en dos formas: el fixture tal cual (cabe
    /// entero, el score genuino queda el ÚLTIMO campo) y el fixture inflado
    /// a >120 chars (fuerza la elipsis media: el path se RECORTA, marcado,
    /// pero el score sigue ahí — jamás al revés).
    #[test]
    fn score_spoof_inband_jamas_desplaza_al_score_real() {
        let fixture = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "score_spoof_inband")
            .expect("fixture del corpus");
        let dir = VPath::parse("mem:///d").expect("wire válido");
        let señuelo = String::from_utf8(fixture.bytes.clone()).expect("el fixture es UTF-8");

        let path = dir
            .clone()
            .join(Segment::new(fixture.bytes.clone()).expect("segmento del corpus"));
        let (_, body) = semantic_hits_modal_text(&[hit(path, 0.91)], 0, 0);
        let linea = body.lines().next().expect("la línea del hit");
        assert!(
            linea.contains(&señuelo),
            "el señuelo se pinta tal cual (es un nombre legítimo): {linea:?}"
        );
        assert!(
            linea.trim_end().ends_with("0.91"),
            "el score REAL es el campo FINAL: {linea:?}"
        );

        // Inflado: el señuelo al final de un nombre kilométrico. El recorte
        // se come el PATH (elipsis media, marcada), nunca el score.
        let mut largo = b"x".repeat(120);
        largo.extend_from_slice(&fixture.bytes);
        let path = dir.join(Segment::new(largo).expect("segmento válido"));
        let (_, body) = semantic_hits_modal_text(&[hit(path, 0.91)], 0, 0);
        let linea = body.lines().next().expect("la línea del hit");
        assert!(
            linea.trim_end().ends_with("0.91"),
            "path kilométrico: el score REAL sigue siendo el campo FINAL: {linea:?}"
        );
        assert!(
            linea.contains('…'),
            "el recorte del path se MARCA (spec §6): {linea:?}"
        );
    }

    /// M4-IA-2: con 12 hits la ventana pinta 10 desde `offset` con
    /// numeración ABSOLUTA y marcador `>` en la fila del cursor; el
    /// indicador dice posición/total, el score va al final de la línea y el
    /// alto del modal cuadra con las líneas pintadas.
    #[test]
    fn hits_largos_ventana_cursor_indicador_y_alto() {
        let hits = hits(12);
        let (_, body) = semantic_hits_modal_text(&hits, 0, 3);
        let lines: Vec<&str> = body.lines().collect();
        // 10 hits + indicador + hint = 12.
        assert_eq!(lines.len(), 12, "{body:?}");
        assert!(
            lines[0].contains("1.") && lines[0].contains("f1"),
            "{body:?}"
        );
        assert!(
            lines[3].starts_with("> ") && lines[3].contains("4."),
            "marcador en la fila del cursor: {body:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| l.starts_with("> ")).count(),
            1,
            "un solo cursor: {body:?}"
        );
        assert!(lines[0].contains("0.99"), "score al final: {body:?}");
        assert!(lines[10].contains("10/12"), "indicador: {body:?}");
        assert!(!body.contains("f11"), "la cola espera al scroll: {body:?}");
        // La ventana sigue al cursor: offset 2 = hits 3..=12, numeración
        // absoluta, cursor al fondo visible.
        let (_, body2) = semantic_hits_modal_text(&hits, 2, 11);
        let lines2: Vec<&str> = body2.lines().collect();
        assert_eq!(lines2.len(), 12, "alto ESTABLE al scroll: {body2:?}");
        assert!(
            lines2[0].contains("3.") && lines2[0].contains("f3"),
            "{body2:?}"
        );
        assert!(
            lines2[9].starts_with("> ") && lines2[9].contains("12."),
            "{body2:?}"
        );
        assert!(lines2[10].contains("12/12"), "{body2:?}");
        // Un offset desbocado se clampa en el render (cinturón).
        let (_, body3) = semantic_hits_modal_text(&hits, 999, 0);
        assert!(body3.contains("f12"), "{body3:?}");
        // Alto: 12 líneas de cuerpo + 3 de marco.
        let modal = crate::app::Modal::SemanticHits {
            hits,
            offset: 0,
            cursor: 0,
        };
        assert_eq!(modal_height(&modal), 15);
    }

    /// M4-IA-2: el indicador de desbordamiento delata un hit hostil OCULTO
    /// (lo no visible jamás se cuela "limpio"), y deja de marcar cuando el
    /// scroll lo pone a la vista.
    #[test]
    fn indicador_marca_hostil_oculto() {
        let mut hits = hits(11);
        hits[10] = hit(
            VPath::parse("mem:///d")
                .expect("wire válido")
                .join(Segment::new(b"x\xe2\x80\xaey".to_vec()).expect("segmento")),
            0.1,
        );
        let (_, body) = semantic_hits_modal_text(&hits, 0, 0);
        let ind = body.lines().nth(10).expect("indicador");
        assert!(ind.starts_with(HOSTILE_BADGE), "{body:?}");
        // offset 1: el hostil entra en la ventana; el oculto (hit 1) es
        // limpio — el indicador ya no marca.
        let (_, body2) = semantic_hits_modal_text(&hits, 1, 10);
        let ind2 = body2.lines().nth(10).expect("indicador");
        assert!(!ind2.starts_with(HOSTILE_BADGE), "{body2:?}");
    }
}

#[cfg(test)]
mod column_header_line_tests {
    use super::column_header_line;
    use norte_frontend::columns::{Align, Builtin, ColumnId, ColumnStyle};
    use norte_frontend::{SortColumn, SortDir, SortSpec};
    use unicode_width::UnicodeWidthStr;

    fn estilo(b: Builtin, align: Align, header: &str) -> ColumnStyle {
        ColumnStyle {
            align,
            header: Some(header.to_owned()),
            ..ColumnStyle::default_for(b)
        }
    }

    /// m1 revisión 7b: columna IZQUIERDA de una celda con la flecha del
    /// sort activa — la emisión queda clampada a exactamente `w` (antes
    /// «espacio + flecha» eran 2 celdas y corrían toda la cabecera a su
    /// derecha; con 2 celdas la flecha sí cabe tras el separador).
    #[test]
    fn header_izquierda_de_una_celda_con_flecha_no_desborda() {
        let sort = SortSpec {
            column: SortColumn::Size,
            dir: SortDir::Asc,
            dirs_first: true,
        };
        let cols = [
            (
                ColumnId::Builtin(Builtin::Name),
                6,
                estilo(Builtin::Name, Align::Left, "N"),
            ),
            (
                ColumnId::Builtin(Builtin::Size),
                1,
                estilo(Builtin::Size, Align::Left, "S"),
            ),
        ];
        let linea = column_header_line(&cols, sort, None);
        assert_eq!(linea.width(), 7, "exactamente la suma de anchos: {linea:?}");
        assert_eq!(linea, "N      ");
        let cols = [
            (
                ColumnId::Builtin(Builtin::Name),
                6,
                estilo(Builtin::Name, Align::Left, "N"),
            ),
            (
                ColumnId::Builtin(Builtin::Size),
                2,
                estilo(Builtin::Size, Align::Left, "S"),
            ),
        ];
        let linea = column_header_line(&cols, sort, None);
        assert_eq!(linea.width(), 8, "{linea:?}");
        assert_eq!(linea, "N      ▲");
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
        }
    }

    fn line_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn sin_description_es_none() {
        let p = sample_plugin(None);
        assert!(plugin_description_line(&p, &TuiTheme::default(), 100).is_none());
    }

    /// P1 encoding audit F1 (MEDIUM): un daemon hostil/comprometido puede
    /// mandar una `description` sin tope por el wire — este draw NO confía
    /// en que el caller (`main::dispatch`'s ingest,
    /// `app::clamp_plugin_descriptions`) ya la haya clampado, y la acota
    /// aquí también (self-contained, como `plugin_line`). Con un `inner`
    /// GRANDE (que no fuerce elipsis por ancho) el contenido final refleja
    /// EXACTAMENTE `PLUGIN_DESCRIPTION_WIRE_CAP` caracteres del original —
    /// ni uno más, sin pasar por el layout del popup.
    #[test]
    fn clampa_al_tope_del_wire_incluso_sin_ingest() {
        let p = sample_plugin(Some(&"a".repeat(50_000)));
        let line =
            plugin_description_line(&p, &TuiTheme::default(), 10_000).expect("hay description");
        let texto = line_text(&line);
        assert_eq!(
            texto.chars().filter(|&c| c == 'a').count(),
            crate::app::PLUGIN_DESCRIPTION_WIRE_CAP,
            "el draw procesó más de PLUGIN_DESCRIPTION_WIRE_CAP chars del original: {texto:?}"
        );
    }

    /// Un override RTL crudo (sin pasar por ingest) se enmascara a U+FFFD
    /// AQUÍ — nunca llega intacto a `ratatui` (donde un control/override es
    /// invisible: desaparecería en silencio en vez de marcarse).
    #[test]
    fn enmascara_override_rtl_incluso_sin_ingest() {
        let p = sample_plugin(Some("abc\u{202E}gpj.exe"));
        let line =
            plugin_description_line(&p, &TuiTheme::default(), 10_000).expect("hay description");
        let texto = line_text(&line);
        assert!(!texto.contains('\u{202E}'));
        assert!(texto.contains('\u{FFFD}'));
    }
}
