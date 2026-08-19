//! Los overlays que se pintan por encima de todo: which-key, la paleta de
//! comandos, los ajustes, el editor de atajos y el gestor de extensiones.

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
use norte_frontend::middle_ellipsis;
use norte_i18n::{t, ta};

/// Overlay del catálogo de extensiones (M4-P3): la lista de plugins AGRUPADA
/// por categoría (una cabecera al cambiar de grupo, ya que llegan ordenados)
/// más los directorios que fallaron al cargar. CRÍTICO: `name` y `publisher`
/// son texto LIBRE de un tercero y esto es superficie de decisión de seguridad
/// (aprobar) — se pasan por [`display_name`] (mismo enmascarado de
/// controles/bidi/invisibles que los panes) antes de pintar. El id ya está
/// charset-validado en el core; name/publisher no. `hint` (H1 T3, #24) es
/// el hint GENERADO (`app.dialog_hints.extensions`).
pub(crate) fn draw_extensions(
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
    // Ancho útil para la segunda línea (description, P1): igual criterio que
    // `draw_palette` (borde + margen), NO el `width` de la caja completa.
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
pub(crate) fn plugin_line<'a>(
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

/// Popup selector de tema: lista de presets con el vigente resaltado (ADR
/// 0020). El preview en vivo lo hace el bucle de eventos; aquí solo se
/// pinta. `hint` (H1 T3, #24) es el hint GENERADO (`app.dialog_hints.picker`).
/// MAJOR-1(c) H1 close: 34 columnas era un ancho FIJO que no crecía con el
/// hint generado (se cortaba en terminales angostas) — mismo criterio de
/// sizing que `draw_nav_popup`/`draw_extensions`, footer en CELDAS
/// (`Line::width`), suelo 34 (el listado de nombres de preset ya cabía),
/// tope el ancho del frame.
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

/// Command palette (`Ctrl+P`/vim `:`, H1 T4, spec-promised): filtro libre
/// sobre TODOS los comandos, mismo idioma visual que [`draw_nav_popup`]
/// (centrado, input al pie, `Clear` antes de pintar) pero MÁS ancha (60
/// columnas: `{text} {descripción} {chord}` no cabe en el ancho de un
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
///
/// Ese pie se une con `palette-hint-help` (H3c: `F1` sobre una fila abre la
/// página que documenta su comando). Van en dos claves y se juntan AQUÍ porque
/// `palette-hint` lo pinta también la GUI, que todavía no tiene overlay de
/// ayuda (fase H3f): una sola cadena le haría anunciar una tecla inerte.
pub(crate) fn draw_palette(frame: &mut Frame<'_>, palette: &crate::app::Palette, theme: &TuiTheme) {
    let rows = u16::try_from(palette.visible().len().max(1))
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered(frame.area(), 60, rows.min(frame.area().height.max(3)));
    clear_themed(frame, area, theme);
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
                    let text = format!(" {:<24} {:<32} {}", row.text, row.desc, row.chord);
                    ListItem::new(Line::raw(middle_ellipsis(&text, inner)))
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
pub(crate) fn draw_settings(
    frame: &mut Frame<'_>,
    settings: &crate::app::Settings,
    theme: &TuiTheme,
) {
    let width = frame
        .area()
        .width
        .saturating_sub(6)
        .clamp(30, 80)
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
            let text = if row.is_plugins_note() {
                format!("{cursor} {}", row.name)
            } else {
                format!("{cursor} {:<28} {}", row.name, row.value)
            };
            let mut line = Line::raw(middle_ellipsis(&text, inner_w));
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

/// Column the label starts at, in CELLS — a chord wider than this pushes it
/// right instead of overlapping, same rule as the generated keys page.
pub(crate) const SHORTCUT_CHORD_COLUMN: usize = 16;

/// La cabecera de sección de una pantalla, la MISMA que la página de teclas
/// generada (`crate::help::build`): dos superficies que listan lo mismo no
/// pueden llamarlo distinto.
pub(crate) fn shortcuts_section(screen: norte_frontend::keymap::Screen) -> String {
    match screen {
        norte_frontend::keymap::Screen::Browse => t("help-section-browse"),
        norte_frontend::keymap::Screen::Viewer => t("help-section-viewer"),
        norte_frontend::keymap::Screen::Dialog => t("help-section-dialog"),
    }
}

/// Editor de atajos (`app.shortcuts`, K3c): mismo idioma visual que
/// `draw_settings` —Paragraph con cabeceras de sección, filtro en el pie,
/// línea de detalle reservada abajo— con dos diferencias que son el editor:
///
/// - la lista SCROLLEA. Ajustes cabe en una pantalla; esto son todas las
///   teclas de las tres pantallas MÁS cada comando que no pulsa ninguna, y una
///   lista sin ventana dejaría el cursor fuera de la caja a las veinte filas.
/// - la línea de detalle lleva el VEREDICTO mientras se captura, que es lo que
///   el lector necesita ANTES de confirmar, y el resto del tiempo lleva las dos
///   verdades de esta terminal: `esc` cancela (así que es el único chord que no
///   se puede capturar aquí) y `mod+` es Ctrl, porque crossterm no entrega ⌘.
///
/// Chords y etiquetas ya vienen pintados y traducidos del modelo compartido
/// (`norte_frontend::shortcuts`), incluido el enmascarado de
/// [`paint_chord`](norte_frontend::keymap::paint_chord) — una capa de proyecto
/// puede bindear cualquier codepoint suelto y esto va a una terminal. Aquí solo
/// queda el ancho.
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
            // Un comando sin tecla NO se atenúa: se puede ejecutar, es solo que
            // nada lo pulsa — y esa es justo la fila que el lector vino a
            // buscar. Atenuada se leería como «no disponible», que es la otra
            // cosa.
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
            // La selección se PARCHEA sobre el atenuado, no lo sustituye: un
            // `Line::style` reemplaza el estilo entero, y una fila no
            // construida bajo el cursor dejaría de parecerlo justo cuando el
            // lector está a punto de actuar sobre ella.
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
    // Ventana alrededor del cursor: sin ella la fila seleccionada desaparece
    // por debajo del borde en cuanto la lista pasa del alto de la caja.
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
            has_help: false,
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
        let text = line_text(&line);
        assert_eq!(
            text.chars().filter(|&c| c == 'a').count(),
            crate::app::PLUGIN_DESCRIPTION_WIRE_CAP,
            "el draw procesó más de PLUGIN_DESCRIPTION_WIRE_CAP chars del original: {text:?}"
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
        // Este test afirma los strings del corpus INGLÉS. Sin fijar el idioma
        // resolvía por entorno (`LANG`), así que era verde en CI y rojo en
        // cualquier máquina con `LANG=es_*` — la misma línea que el resto de
        // los tests de render de este crate ya llevaba.
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let theme = TuiTheme::default();
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("terminal de test");
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
        let mut terminal = Terminal::new(TestBackend::new(24, 5)).expect("terminal de test");
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
        let mut squeezed = Terminal::new(TestBackend::new(24, 4)).expect("terminal de test");
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
            let mut tiny = Terminal::new(TestBackend::new(w, h)).expect("terminal de test");
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

    // `pane.move` no lo bindea nadie: es la fila SIN TECLA que la hoja de
    // referencia no puede tener.
    const BINDABLE: &[&str] = &["pane.copy", "pane.mkdir", "pane.move"];

    fn eff() -> Effective {
        // Un chord HOSTIL (U+202E RIGHT-TO-LEFT OVERRIDE) bindeado como
        // codepoint suelto: legal, y sin confianza — una capa de proyecto
        // llega con un repositorio clonado.
        let src = "[pane]\nkeymap = [\n  { on = [\"f5\"], run = \"pane.copy\" },\n  { on = [\"alt+f5\"], run = \"pane.pack\" },\n  { on = [\"\u{202e}\"], run = \"pane.mkdir\" },\n]\n";
        let preset = parse_keymap(src).expect("fixture parsea");
        Effective::build_for(&preset, &[], BINDABLE, Screen::Browse).expect("fixture construye")
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
        let mut terminal = Terminal::new(TestBackend::new(90, 14)).expect("terminal de test");
        terminal
            .draw(|f| draw_shortcuts(f, sc, &TuiTheme::default()))
            .expect("draw");
        terminal.backend().to_string()
    }

    /// La pantalla DICE las dos cosas que esta terminal no puede hacer: `esc`
    /// cancela (así que es el único chord no capturable) y `mod+` es Ctrl,
    /// porque crossterm no entrega ⌘ sin el protocolo de Kitty. Sin esa línea
    /// el lector descubre ambas cosas pulsando.
    #[test]
    fn la_pantalla_dice_lo_que_esta_terminal_no_puede_capturar() {
        let eff = eff();
        let text = painted(&state(&eff));
        assert!(text.contains("esc"), "{text}");
        assert!(text.contains("mod+"), "{text}");
    }

    /// Un comando sin tecla se ve (la fila que la hoja de referencia no puede
    /// tener), y una tecla que este build no puede ejecutar se ve con su
    /// razón — nada se cae en silencio.
    ///
    /// El ejemplo de «no ejecutable» era una capacidad `Planned` con su número
    /// de issue. Con #132 construido no quedan: la razón que se pinta ahora es
    /// la del comando que existe y este frontend no implementa, que es la otra
    /// mitad de lo mismo — y sigue siendo una fila con explicación en vez de
    /// una tecla que no hace nada.
    #[test]
    fn se_ven_la_fila_sin_tecla_y_la_no_construida() {
        let eff = eff();
        let text = painted(&state(&eff));
        assert!(text.contains(&norte_i18n::t("shortcuts-no-key")), "{text}");
        assert!(
            text.contains(&norte_i18n::t("keymap-short-not-here")),
            "la razón de la fila que este build no ejecuta: {text}"
        );
    }

    /// El veredicto se pinta ANTES de confirmar, y el chord capturado va
    /// PINTADO: un codepoint hostil no llega crudo a la terminal por la línea
    /// de detalle más de lo que llega por la lista.
    #[test]
    fn el_veredicto_se_pinta_y_los_chords_van_enmascarados() {
        let eff = eff();
        let mut sc = state(&eff);
        assert!(sc.begin_capture());
        sc.capture_chord(parse_chord("\u{202e}").expect("chord"), &eff);
        let text = painted(&sc);
        // Por LÍNEA: los `\n` que une `to_string` son del harness, no del
        // buffer (mismo criterio que el resto de tests de render de aquí).
        assert!(
            text.lines()
                .all(|l| !l.chars().any(norte_encoding::is_terminal_hazard)),
            "{text}"
        );
        // `Replaces`: el codepoint hostil ya está ligado a `pane.mkdir`, y el
        // veredicto que se pinta es EL del modelo, no una frase paralela.
        // Fluent aísla sus argumentos con marcas de dirección (U+2066..U+2069)
        // que el buffer de ratatui, de ancho cero, no llega a pintar: se
        // quitan para comparar, en vez de comparar contra otra cosa.
        let verdict = norte_frontend::shortcuts::verdict_message(
            sc.capture()
                .and_then(norte_frontend::shortcuts::Capture::verdict)
                .expect("hay veredicto"),
            norte_i18n::active(),
        );
        let want: String = verdict
            .chars()
            .filter(|c| !('\u{2066}'..='\u{2069}').contains(c))
            .collect();
        assert!(text.contains(&want), "{want:?} en {text}");
    }

    /// Geometrías degeneradas: ni pánico ni pintar fuera del frame. La caja
    /// tiene una ventana sobre la lista, y una ventana mal calculada es la
    /// forma habitual de salirse por abajo.
    #[test]
    fn geometrias_degeneradas_no_revientan() {
        let eff = eff();
        let mut sc = state(&eff);
        for _ in 0..20 {
            sc.down();
        }
        for (w, h) in [(4_u16, 1_u16), (1, 3), (2, 2), (1, 10), (3, 12), (30, 5)] {
            let mut tiny = Terminal::new(TestBackend::new(w, h)).expect("terminal de test");
            tiny.draw(|f| draw_shortcuts(f, &sc, &TuiTheme::default()))
                .expect("draw");
        }
    }
}
