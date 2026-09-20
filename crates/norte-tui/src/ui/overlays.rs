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
use norte_frontend::display::cells;
use norte_frontend::middle_ellipsis;
use norte_frontend::settings::Section;
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
    // Dos columnas, como la ventana (ADR 0104): la lista a la izquierda y la
    // FICHA de la elegida a la derecha —estado, descripción, capabilities,
    // comandos y sus ajustes—. Con menos de [`EXTENSIONS_WIDE_MIN`] celdas
    // útiles no caben dos columnas legibles y se pinta la lista de siempre,
    // con la descripción bajo cada fila y los ajustes en su propia caja.
    let Some((lista_area, ficha_area)) = extensions_columns(mgr, inner_area) else {
        let inner = usize::from(inner_area.width.saturating_sub(2));
        let (lines, _) = extensions_list_lines(mgr, theme, inner, true);
        frame.render_widget(Paragraph::new(lines), inner_area);
        return;
    };
    let (lista, _) = extensions_list_lines(mgr, theme, usize::from(lista_area.width), false);
    frame.render_widget(Paragraph::new(lista), lista_area);
    let borde = Block::default()
        .borders(Borders::LEFT)
        .border_style(theme.role(Role::BorderUnfocused));
    frame.render_widget(
        borde,
        Rect {
            x: ficha_area.x.saturating_sub(1),
            width: 1,
            ..ficha_area
        },
    );
    // La elegida puede ser una que NO cargó: su ficha dice dónde y por qué,
    // y su único botón es desinstalar.
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
    if let Some((botones, ficha)) = pane {
        // Los BOTONES en la primera fila de la ficha, como en la ventana:
        // son lo que el lector busca, y en una fila fija —no dentro del
        // párrafo, cuyo ajuste de línea movería cada uno según lo largo
        // que sea el nombre— para que el ratón los encuentre donde se
        // pintaron. Debajo, una fila en blanco y la ficha.
        let mut spans = Vec::new();
        let mut x = 0usize;
        for (n, (etiqueta, _)) in botones.iter().enumerate() {
            let w = UnicodeWidthStr::width(etiqueta.as_str());
            if x + w > usize::from(ficha_area.width) {
                break;
            }
            // El que tiene el foco de `tab` se pinta como el cursor de una
            // lista; los demás, como los botones que son. Sin esta
            // diferencia `tab` movería algo que no se ve, que es como
            // estaba antes de que el anillo existiera.
            let rol = if mgr.foco == crate::app::ExtFoco::Boton(n) {
                Role::Selection
            } else {
                Role::Button
            };
            spans.push(Span::styled(etiqueta.clone(), theme.role(rol)));
            spans.push(Span::raw(" "));
            x += w + 1;
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect {
                height: 1.min(ficha_area.height),
                ..ficha_area
            },
        );
        let cuerpo = Rect {
            y: ficha_area.y.saturating_add(BUTTON_ROWS),
            height: ficha_area.height.saturating_sub(BUTTON_ROWS),
            ..ficha_area
        };
        frame.render_widget(
            Paragraph::new(ficha).wrap(ratatui::widgets::Wrap { trim: false }),
            cuerpo,
        );
    }
}

/// Filas que la fila de botones y su blanco le quitan a la ficha.
const BUTTON_ROWS: u16 = 2;

/// Celdas útiles a partir de las que el gestor pinta la ficha al lado de la
/// lista. Por debajo, la lista de siempre.
pub(crate) const EXTENSIONS_WIDE_MIN: u16 = 64;

/// La caja del gestor en un frame de `frame_area`, con `hint` en el pie.
///
/// MAJOR-1(c) H1 close: el ancho por CONTENIDO (igual que antes, clamp(24,
/// 80)) puede quedarse corto para el footer GENERADO — mismo criterio de
/// sizing que [`draw_nav_popup`] (medir el footer en CELDAS, `Line::width`,
/// y crecer si hace falta), tope en el ancho del frame. Con ficha (ADR
/// 0104, nivelación con la ventana) el tope sube a 120: dos columnas en 80
/// son dos columnas estrechas.
///
/// Una función y no un cálculo dentro del pintor porque el ratón la
/// necesita: medir por un lado y pintar por otro es cómo un click acaba
/// en la fila de al lado.
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

/// `(lista, ficha)` dentro de `inner_area`, o `None` cuando no caben dos
/// columnas y el gestor pinta la lista de siempre. La ficha ya viene sin
/// la columna de su borde izquierdo.
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
    let ficha = Block::default().borders(Borders::LEFT).inner(cols[1]);
    Some((cols[0], ficha))
}

/// Los botones de la ficha para `p`: `(etiqueta, comando)`, en el orden en
/// que se pintan. Los mismos verbos y las mismas etiquetas que los botones
/// de la ventana (`ext-*`), y cada uno dispara EL MISMO comando que su
/// tecla: un botón que hiciera otra cosa que la tecla sería dos gestores.
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

/// Los botones de la ficha de una extensión que NO cargó: desinstalar, si su
/// directorio se llama como un id, y nada más —no hay capabilities que
/// aprobar ni nada que encender—. El mismo comando que su tecla.
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

/// La ficha de una extensión que NO cargó: dónde y por qué, enmascarados, y
/// —si no se puede desinstalar desde aquí— por qué no.
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

/// Qué hay bajo una celda pulsable del gestor de extensiones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionHit {
    /// La fila del plugin `index` de la lista.
    Row(usize),
    /// Un botón de la ficha: el comando `dialog.*`/`app.help` que dispara,
    /// el mismo que su tecla.
    Button(&'static str),
}

/// Una celda pulsable del gestor de extensiones, en el frame pintado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtensionZone {
    /// Fila de la pantalla.
    pub row: u16,
    /// Primera columna, inclusive.
    pub x0: u16,
    /// Última columna, inclusive.
    pub x1: u16,
    /// Qué hay ahí.
    pub hit: ExtensionHit,
}

/// Las zonas pulsables del gestor de extensiones en el frame de `area`, o
/// nada si no está abierto —o si lo que se ve es la caja de ajustes
/// estrecha, que no tiene ratón—.
///
/// Comparte con el pintor del gestor la caja, el reparto en columnas y las
/// líneas de la lista, y por lo mismo que [`super::places_zones`]: el
/// gestor de la TUI nació mudo al ratón —un clic en una fila o donde la
/// ventana tiene sus botones no hacía nada— y la forma de que no vuelva a
/// pasar es que lo pulsable salga de lo pintado.
#[must_use]
pub fn extension_zones(app: &crate::app::App, area: Rect) -> Vec<ExtensionZone> {
    let Some(mgr) = &app.extensions else {
        return Vec::new();
    };
    let Some(hint) = super::extensions_footer(app, mgr, area.width) else {
        return Vec::new();
    };
    let caja = extensions_area(area, hint);
    let inner_area = Block::default().borders(Borders::ALL).inner(caja);
    let mut zonas = Vec::new();
    let (lista_area, ficha, ancho, con_descripcion) = match extensions_columns(mgr, inner_area) {
        Some((lista, ficha)) => (lista, Some(ficha), usize::from(lista.width), false),
        None => (
            inner_area,
            None,
            usize::from(inner_area.width.saturating_sub(2)),
            true,
        ),
    };
    let (_, filas) = extensions_list_lines(mgr, &app.theme, ancho, con_descripcion);
    for (i, index) in filas.iter().enumerate() {
        let Some(index) = index else { continue };
        let Ok(offset) = u16::try_from(i) else { break };
        if offset >= lista_area.height {
            break;
        }
        zonas.push(ExtensionZone {
            row: lista_area.y.saturating_add(offset),
            x0: lista_area.x,
            x1: lista_area
                .x
                .saturating_add(lista_area.width)
                .saturating_sub(1),
            hit: ExtensionHit::Row(*index),
        });
    }
    let botones = match mgr.plugins.get(mgr.cursor) {
        Some(p) => Some(extension_buttons(p)),
        None => mgr
            .selected_broken()
            .map(|e| broken_buttons(e, &mgr.plugins)),
    };
    if let Some(ficha) = ficha
        && ficha.height > 0
        && let Some(botones) = botones
    {
        let mut x = usize::from(ficha.x);
        let tope = usize::from(ficha.x) + usize::from(ficha.width);
        for (etiqueta, cmd) in botones {
            let w = UnicodeWidthStr::width(etiqueta.as_str());
            if x + w > tope {
                break;
            }
            let (Ok(x0), Ok(x1)) = (u16::try_from(x), u16::try_from(x + w - 1)) else {
                break;
            };
            zonas.push(ExtensionZone {
                row: ficha.y,
                x0,
                x1,
                hit: ExtensionHit::Button(cmd),
            });
            x += w + 1;
        }
    }
    zonas
}

/// Las líneas de la LISTA del gestor: cabeceras de categoría, una fila por
/// plugin y los directorios que no cargaron al final. `con_descripcion`
/// mete la descripción bajo cada fila —la lista estrecha, sin ficha— o la
/// deja para la ficha.
///
/// Devuelve también, por línea, el índice del plugin cuya fila es —`None`
/// para cabeceras, descripciones y errores—: es lo que el ratón necesita
/// para saber qué fila pulsó, y sale de la MISMA lista que se pinta.
fn extensions_list_lines<'a>(
    mgr: &'a crate::app::ExtensionManager,
    theme: &TuiTheme,
    inner: usize,
    con_descripcion: bool,
) -> (Vec<Line<'a>>, Vec<Option<usize>>) {
    let mut lines: Vec<Line<'_>> = Vec::new();
    let mut filas: Vec<Option<usize>> = Vec::new();
    if mgr.plugins.is_empty() && mgr.errors.is_empty() {
        lines.push(Line::raw(t("ext-empty")));
        filas.push(None);
        return (lines, filas);
    }
    // Con el foco en un botón de la ficha, el cursor de la lista se apaga:
    // dos cursores igual de vivos no dicen cuál recibe las teclas, que es
    // para lo que existe `SelectionUnfocused`.
    let rol_cursor = if mgr.foco == crate::app::ExtFoco::Lista {
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
            filas.push(None);
        }
        if con_descripcion {
            lines.push(plugin_line(
                p,
                (i == mgr.cursor).then_some(rol_cursor),
                theme,
            ));
            filas.push(Some(i));
            if let Some(desc_line) = plugin_description_line(p, theme, inner) {
                lines.push(desc_line);
                filas.push(None);
            }
        } else {
            lines.push(plugin_row_compact(
                p,
                (i == mgr.cursor).then_some(rol_cursor),
                theme,
                inner,
            ));
            filas.push(Some(i));
        }
    }
    for (j, e) in mgr.errors.iter().enumerate() {
        // Los BYTES si el peer los manda (#265): la cadena `dir` viene de
        // un `to_string_lossy` del core, así que un directorio llamado
        // `caf\xff` llegaría por ahí ya convertido. La insignia de abajo
        // va SIEMPRE —una fila de error de carga es, por definición, algo
        // que no se pudo leer bien— así que aquí lo que cambia es el
        // nombre, no la marca.
        let (dir, _) = display_name(e.dir_bytes.as_deref().unwrap_or(e.dir.as_bytes()));
        let (reason, _) = display_name(e.reason.as_bytes());
        // Una fila más, detrás de los plugins: se señala y se pulsa, y su
        // único verbo es desinstalar.
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
        filas.push(Some(index));
    }
    (lines, filas)
}

/// Una fila COMPACTA de la lista con ficha: `> nombre v1.0 ✓` o `⚠`. Las
/// capabilities no van aquí: van en la ficha, que es donde se leen enteras.
/// Recortada al ancho de la columna, que es la mitad de la caja.
fn plugin_row_compact<'a>(
    p: &norte_proto::methods::PluginInfo,
    selected: Option<Role>,
    theme: &TuiTheme,
    inner: usize,
) -> Line<'a> {
    let (name, _) = display_name(p.name.as_bytes());
    let (version, _) = display_name(p.version.as_bytes());
    let cursor = if selected.is_some() { ">" } else { " " };
    // Sin aprobar se DICE en la fila, no solo en la ficha: es lo que hay que
    // mirar, y la ficha solo habla de la elegida.
    let aviso = if p.approved {
        0
    } else {
        Line::raw(format!("⚠ {}", t("ext-unapproved"))).width() + 1
    };
    let texto = middle_ellipsis(
        &format!("{name} v{version}"),
        inner.saturating_sub(4 + aviso),
    );
    let mut spans = vec![Span::raw(format!("{cursor} {texto} "))];
    if !p.approved {
        spans.push(Span::styled(
            format!("⚠ {}", t("ext-unapproved")),
            theme.role(Role::Warning),
        ));
    } else if p.enabled {
        spans.push(Span::styled("✓", theme.role(Role::Info)));
    }
    let mut line = Line::from(spans);
    if let Some(rol) = selected {
        line = line.style(theme.role(rol));
    }
    line
}

/// La FICHA de una extensión (nivelación con la ventana, ADR 0104): quién
/// es, cómo está, qué hace, qué pide, qué aporta, y —si está abierta— la
/// tabla de sus ajustes con su cursor. Todo lo que escribe el plugin pasa
/// por [`display_name`], como en la lista.
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
    // El estado son DOS hechos, y se dicen los dos: aprobada y apagada no es
    // lo mismo que sin aprobar.
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
        // Cada capability es texto de un TERCERO y va en su propio span,
        // entre corchetes, para que una no pueda fingir ser dos.
        let mut spans = Vec::new();
        for c in &p.capabilities {
            let (cap, _) = display_name(c.as_bytes());
            spans.push(Span::styled(format!("[{cap}]"), theme.role(Role::Warning)));
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
    }
    let mut cuentas = Vec::new();
    if !p.commands.is_empty() {
        cuentas.push(format!("{} {}", p.commands.len(), t("ext-counts-commands")));
    }
    if !p.columns.is_empty() {
        cuentas.push(format!("{} {}", p.columns.len(), t("ext-counts-columns")));
    }
    if p.has_help {
        cuentas.push(t("ext-help"));
    }
    if !cuentas.is_empty() {
        lines.push(Line::styled(cuentas.join(" · "), dim));
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

/// Las líneas de la tabla `[config]` de un plugin: una por clave, la
/// elegida resaltada, y bajo ella el buffer que se teclea o su descripción.
/// Las pinta la ficha (con ancho) y la caja propia (sin él): UNA definición.
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
    let lines = plugin_config_lines(panel, theme);
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
    if let Some(rol) = selected {
        line = line.style(theme.role(rol));
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
    // Tres columnas, por CELDAS (spec 2026-09-10): la etiqueta humana
    // primero y entera —es lo que se lee—, el id atenuado, y el chord a la
    // derecha. El recorte cae sobre la etiqueta y sobre el id, cada uno en
    // su columna; antes se recortaba la línea compuesta y un id largo se
    // comía la etiqueta hasta dejar «sw…ane».
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
    let sin_consulta = palette.query_display().is_empty();
    let (items, selected): (Vec<ListItem<'_>>, Option<usize>) = if palette.visible().is_empty() {
        (vec![ListItem::new(Line::raw(" —"))], None)
    } else {
        (
            palette
                .visible()
                .iter()
                .map(|&i| {
                    let row = &palette.rows()[i];
                    // Una reciente se marca solo mientras va arriba por
                    // serlo: con consulta, el orden es el de lo que casa.
                    let mark = if sin_consulta && palette.is_recent(i) {
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

/// «Ir a cualquier sitio» (fase 6): una caja con secciones tituladas y una
/// fila por destino.
///
/// Mismo idioma visual que la paleta —caja centrada, consulta en el pie,
/// cursor de selección— con una diferencia que es la razón de existir de
/// esta pantalla: aquí las filas vienen de SITIOS distintos, y una lista
/// que mezcla una conexión con un comando sin decir cuál es cuál no se
/// puede leer. De ahí las cabeceras, que no reciben el cursor (de eso se
/// encarga el modelo: [`norte_frontend::goto::Goto::up`]/`down`).
///
/// La altura sale de lo que hay, acotada al frame; el ancho es fijo y más
/// generoso que el de la paleta porque lo que se pinta son RUTAS, que se
/// leen por el final y se recortan por el medio.
pub(crate) fn draw_goto(
    frame: &mut Frame<'_>,
    goto: &norte_frontend::goto::Goto,
    theme: &TuiTheme,
) {
    use norte_frontend::goto::GotoLine;

    /// Lo que se le quita a cada fila por la izquierda: la insignia de
    /// texto hostil, o los dos espacios que la sustituyen cuando no la hay.
    const SANGRIA_GOTO: usize = 2;

    let alto = u16::try_from(goto.lines().len().max(1))
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let ancho = frame.area().width.saturating_sub(8).clamp(40, 88);
    let area = centered(frame.area(), ancho, alto.min(frame.area().height.max(3)));
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
            .map(|linea| match linea {
                GotoLine::Header(s) => {
                    ListItem::new(Line::styled(t(s.title_key), theme.role(Role::Title)))
                }
                GotoLine::Row(i) => {
                    let row = &goto.rows()[*i];
                    // La insignia va DELANTE y en su propio span, como en
                    // todas las superficies de decisión: lo que se pinta
                    // distinto de lo que dicen los bytes se dice, no se
                    // deja adivinar.
                    let mut spans = Vec::new();
                    if row.hostile {
                        spans.push(Span::styled(
                            format!("{HOSTILE_BADGE} "),
                            theme.role(Role::HostileBadge),
                        ));
                    } else {
                        spans.push(Span::raw("  "));
                    }
                    // `SANGRIA_GOTO`: la insignia ocupa lo mismo que los
                    // dos espacios que la sustituyen, para que los textos
                    // queden alineados lleven bandera o no.
                    let detalle = cells(&row.desc).min(inner / 2);
                    let texto_w = inner.saturating_sub(SANGRIA_GOTO + detalle + 2).max(1);
                    spans.push(Span::raw(middle_ellipsis(&row.text, texto_w)));
                    if !row.desc.is_empty() {
                        spans.push(Span::styled(
                            format!("  {}", middle_ellipsis(&row.desc, detalle)),
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
        // `>` y no la `/` de la paleta: aquí lo escrito PUEDE ser una ruta,
        // y una barra de prompt pegada a una ruta absoluta se lee como
        // parte de ella (`//etc`).
        .title_bottom(Line::raw(format!(" ❯{query}_ ")))
        .border_style(theme.role(Role::ModalBorder));
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    // El cursor del modelo indexa LÍNEAS, que es lo que se pinta: filas y
    // cabeceras. Convertirlo a «índice de fila» aquí sería la misma cuenta
    // dos veces y la ocasión de que difieran.
    state.select((!goto.is_empty()).then_some(goto.cursor()));
    frame.render_stateful_widget(list, area, &mut state);
}

/// El asistente de primer arranque (spec 2026-09-10): una caja con el
/// título del paso, la pregunta, las filas con el cursor y la línea de
/// teclas. Mismo idioma visual que la paleta.
/// La pantalla de arranque (spec 2026-09-15, fase 2): la brújula, qué build
/// corre y contra qué core, y —en `home`— las filas numeradas de a dónde ir.
///
/// Una CAPA sobre el listado y no un modal: lo que hay detrás ya está pintado,
/// y cualquier tecla la quita. Por eso el pie dice cómo se sale, que es lo
/// único que un lector necesita saber de ella.
///
/// El arte y las secciones vienen del modelo COMPARTIDO
/// ([`norte_frontend::splash`]), así que la ventana enseña lo mismo; aquí solo
/// se decide dónde caen las celdas.
pub(crate) fn draw_splash(
    frame: &mut Frame<'_>,
    splash: &norte_frontend::splash::SplashView,
    theme: &TuiTheme,
) {
    use norte_frontend::splash::numbered;

    // PORTADA: `brief` viene SIN secciones a propósito, y sin lista que
    // enmarcar una caja centrada es un marco alrededor de nada. El modo no
    // viaja en la vista —no hace falta—, porque «no hay secciones» es la
    // misma señal que este pintor ya usa para elegir el pie.
    if splash.sections.is_empty() {
        draw_splash_cover(frame, splash, theme);
        return;
    }

    let lang = norte_i18n::active();
    let numeradas = numbered(&splash.sections);
    let arte = splash.art.len();
    // Arte + versión + daemon + aire + (título + filas) por sección + pie, y
    // los dos bordes.
    let filas_secciones: usize = splash
        .sections
        .iter()
        .map(|s| s.rows.len().saturating_add(1))
        .sum();
    let alto = u16::try_from(arte + 3 + filas_secciones + 2).unwrap_or(u16::MAX);
    let ancho = 60.min(frame.area().width.max(20));
    let area = centered(frame.area(), ancho, alto.min(frame.area().height.max(3)));
    clear_themed(frame, area, theme);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("splash-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::styled(
            format!(
                " {} ",
                if numeradas.is_empty() {
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
    let mut lineas: Vec<Line<'_>> = splash
        .art
        .iter()
        .map(|l| Line::styled((*l).to_owned(), theme.role(Role::Title)))
        .collect();
    lineas.push(Line::raw(format!("{} {}", splash.version, splash.revision)));
    lineas.push(Line::styled(
        norte_i18n::t_in(lang, splash.daemon.key()),
        theme.role(Role::Info),
    ));
    lineas.push(Line::raw(String::new()));
    let mut n = 0usize;
    for seccion in &splash.sections {
        lineas.push(Line::styled(
            norte_i18n::t_in(lang, seccion.title_key),
            theme.role(Role::Title),
        ));
        for fila in &seccion.rows {
            n += 1;
            // El número solo hasta donde hay tecla que lo llame: más allá, la
            // fila se lee y no se promete.
            let marca = if n <= numeradas.len() {
                format!("{n} ")
            } else {
                "  ".to_owned()
            };
            let ancho_util = usize::from(inner.width).saturating_sub(marca.len());
            let texto = middle_ellipsis(&fila.label, ancho_util);
            lineas.push(Line::raw(format!("{marca}{texto}")));
        }
    }
    frame.render_widget(Paragraph::new(lineas), inner);
}

/// La portada: el logo ocupando la pantalla, con la versión y el core debajo.
///
/// Sin marco y sin título de diálogo, al revés que su hermana con lista: una
/// portada que se quita con la primera tecla no es algo que el lector tenga
/// que cerrar, así que no se le pinta el cromo de una cosa que se cierra.
fn draw_splash_cover(
    frame: &mut Frame<'_>,
    splash: &norte_frontend::splash::SplashView,
    theme: &TuiTheme,
) {
    let lang = norte_i18n::active();
    let area = frame.area();
    clear_themed(frame, area, theme);

    let mut lineas: Vec<Line<'_>> = splash
        .art
        .iter()
        .map(|l| Line::styled((*l).to_owned(), theme.role(Role::Title)))
        .collect();
    lineas.push(Line::raw(String::new()));
    lineas.push(Line::raw(
        format!("{} {}", splash.version, splash.revision)
            .trim()
            .to_owned(),
    ));
    lineas.push(Line::styled(
        norte_i18n::t_in(lang, splash.daemon.key()),
        theme.role(Role::Info),
    ));
    lineas.push(Line::raw(String::new()));
    lineas.push(Line::styled(t("splash-hint"), theme.role(Role::Info)));

    // Centrada tambien a lo alto: el aire de arriba es la mitad de lo que
    // sobra. Con una terminal más baja que el logo se pinta desde arriba y se
    // recorta por abajo, que es mejor que empezar por la mitad del logo.
    let alto = u16::try_from(lineas.len()).unwrap_or(u16::MAX);
    let sobra = area.height.saturating_sub(alto);
    let dentro = ratatui::layout::Rect {
        x: area.x,
        y: area.y.saturating_add(sobra / 2),
        width: area.width,
        height: alto.min(area.height),
    };
    frame.render_widget(
        Paragraph::new(lineas).alignment(ratatui::layout::Alignment::Center),
        dentro,
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
    // Pregunta + aire + filas + aire + teclas, más los dos bordes.
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
/// Una línea de la lista de ajustes: una cabecera de sección o una fila.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsLine {
    /// La cabecera de una sección. Lleva la SECCIÓN y no su clave Fluent:
    /// quien sabe cómo se llama una sección es ella.
    Header(Section),
    /// Una fila, por su posición entre las VISIBLES.
    Row(usize),
}

/// La sección de la fila bajo el cursor: la que va CLAVADA arriba.
///
/// `None` solo si no hay ninguna fila visible.
pub(crate) fn settings_cursor_section(settings: &crate::app::Settings) -> Option<Section> {
    let &real = settings.visible().get(settings.cursor())?;
    Some(settings.rows()[real].section)
}

/// Las líneas que pinta la lista de ajustes, en orden: cabeceras y filas.
///
/// Existe aparte por la ventana: el cursor cuenta FILAS y la pantalla LÍNEAS,
/// y las cabeceras de sección caen entre medias. Quien concilia la ventana
/// (`geometry`) y quien pinta tienen que contar igual, así que cuentan con
/// esto — una segunda copia de «dónde van las cabeceras» es una ventana que
/// se desincroniza del dibujo en cuanto alguien añada una sección.
pub(crate) fn settings_line_plan(settings: &crate::app::Settings) -> Vec<SettingsLine> {
    let mut plan = Vec::new();
    let mut actual: Option<Section> = None;
    for (pos, &real) in settings.visible().iter().enumerate() {
        let seccion = settings.rows()[real].section;
        if actual != Some(seccion) {
            plan.push(SettingsLine::Header(seccion));
            actual = Some(seccion);
        }
        plan.push(SettingsLine::Row(pos));
    }
    plan
}

/// Cuántas líneas caben en la lista de ajustes con la pantalla de `alto`
/// filas: la caja (`alto - 4`, mínimo 6) menos sus dos bordes y la línea de
/// descripción reservada abajo.
///
/// Compartida por quien pinta y quien concilia la ventana, por lo mismo que
/// [`settings_line_plan`]: un alto adivinado rompe el scroll en silencio.
pub(crate) fn settings_list_rows(alto: u16) -> usize {
    let caja = alto.saturating_sub(4).max(6);
    // Dos bordes, la línea de descripción reservada abajo, y la CABECERA
    // CLAVADA de arriba: esa no scrollea, así que no es de la lista.
    usize::from(caja.saturating_sub(2).saturating_sub(1).saturating_sub(1))
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
        // La cuenta va SIEMPRE, no solo filtrando: sin la segunda cifra,
        // «no hay nada» y «lo tapé con una letra» se leen igual.
        let cuenta = ta(
            "settings-count",
            &[
                ("shown", &settings.shown().to_string()),
                ("total", &settings.total().to_string()),
            ],
        );
        Line::raw(format!(" /{query}  {cuenta}  {} ", t("settings-hint")))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("settings-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(footer)
        .border_style(theme.role(Role::ModalBorder));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Tres franjas: la cabecera CLAVADA, la lista que scrollea, y la línea
    // de descripción. La cabecera de arriba es la de la sección del cursor y
    // no se mueve: es el único rótulo que dice dónde estás, y una que
    // scrollea se va por el borde en cuanto bajas tres filas.
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    let inner_w = usize::from(inner.width);

    let clavada = settings_cursor_section(settings).map_or_else(String::new, |s| t(s.label_key()));
    frame.render_widget(
        Paragraph::new(Line::styled(
            middle_ellipsis(&clavada, inner_w),
            theme.role(Role::Title),
        )),
        split[0],
    );

    let mut lines: Vec<Line<'_>> = Vec::new();
    if settings.visible().is_empty() {
        lines.push(Line::raw(" —"));
    } else {
        // La cabecera que la clavada ya enseña arriba se pinta EN BLANCO en
        // la lista, no se quita: quitarla movería las filas una línea cada
        // vez que el cursor cruza de sección, y la cuenta de líneas dejaría
        // de cuadrar con la que concilió `geometry`.
        let plan = settings_line_plan(settings);
        let tapada = settings_cursor_section(settings).filter(|s| {
            plan.get(settings.viewport_offset()).copied() == Some(SettingsLine::Header(*s))
        });
        for item in plan {
            match item {
                SettingsLine::Header(seccion) => {
                    if tapada == Some(seccion) {
                        lines.push(Line::raw(""));
                        continue;
                    }
                    lines.push(Line::styled(
                        t(seccion.label_key()),
                        theme.role(Role::Title),
                    ));
                }
                SettingsLine::Row(pos) => {
                    let row = &settings.rows()[settings.visible()[pos]];
                    let selected = pos == settings.cursor();
                    let cursor = if selected { ">" } else { " " };
                    // El punto de «esto lo has tocado tú» es un CARÁCTER, no
                    // un color: un color a secas no es información para
                    // quien no lo distingue.
                    let punto = if row.modified { "•" } else { " " };
                    let text = if row.is_plugins_note() {
                        format!("{cursor}{punto}{}", row.name)
                    } else {
                        format!("{cursor}{punto}{:<28} {}", row.name, row.value)
                    };
                    let mut line = Line::raw(middle_ellipsis(&text, inner_w));
                    if selected {
                        line = line.style(theme.role(Role::Selection));
                    }
                    lines.push(line);
                }
            }
        }
    }
    // La VENTANA que concilió `geometry` antes de este frame. Sin ella la
    // lista se pintaba desde arriba siempre, y el cursor se salía por abajo
    // en cuanto los ajustes dejaron de caber en una pantalla. El `min` es el
    // cinturón: una ventana que no se concilió nunca no puede dejar la lista
    // en blanco.
    let desde = settings
        .viewport_offset()
        .min(lines.len().saturating_sub(1));
    frame.render_widget(
        Paragraph::new(lines).scroll((u16::try_from(desde).unwrap_or(u16::MAX), 0)),
        split[1],
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
mod wizard_hint_tests {
    /// El pie del asistente cabe ENTERO en su caja (70 de ancho, 66 dentro,
    /// uno de margen), en los dos idiomas. Recortado por la mitad —con
    /// `middle_ellipsis`— se perdía justo `[Esc]`: la única tecla que dice cómo
    /// saltarse las preguntas, en la primera pantalla que ve alguien nuevo.
    #[test]
    fn el_pie_del_asistente_cabe_entero() {
        for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
            let hint = norte_i18n::t_in(lang, "wizard-hint");
            let ancho = unicode_width::UnicodeWidthStr::width(hint.as_str());
            assert!(
                ancho <= 65,
                "{lang:?}: {ancho} celdas no caben en 65: {hint}"
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
