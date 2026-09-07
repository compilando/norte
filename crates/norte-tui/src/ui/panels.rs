//! Los paneles laterales: árbol, procesos, metadatos y tareas, más el visor, la
//! previsualización y la lista de sitios.
//!
//! Cada uno ocupa un hueco del reparto y pinta lo que hay en su modelo; ninguno
//! decide dónde va.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use super::text::{head, middle, two_fields, with_badge};
use super::{HOSTILE_BADGE, placed_of_kind, resolved_for};
use crate::app::{App, display_name};
use crate::theme::TuiTheme;
use norte_i18n::{t, ta};

/// Viewer a pantalla completa: contenido + status propia (encoding, EOL,
/// pérdidas, truncado — el usuario SIEMPRE sabe qué mira, spec §6).
pub(crate) fn draw_viewer(frame: &mut Frame<'_>, viewer: &crate::viewer::Viewer, app: &App) {
    // Sobre el área del CUERPO, no la del frame: el visor se pinta a pantalla
    // completa y no pasa por el reparto de huecos, así que con la barra de
    // menú fijada se metía debajo de ella y la barra le tapaba la primera
    // fila. La misma resta que hace el reparto, en el único otro sitio que
    // pinta a pantalla completa.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(super::geometry::body_area(app, frame.area()));
    let (title, hostile) =
        norte_frontend::path_display_with(&viewer.path, app.focused().name_encoding());
    let title = if hostile {
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
                            let mut style = if let Some(role) = span.role {
                                app.theme.role(role)
                            } else if let Some((r, g, b)) = span.fg {
                                Style::default().fg(Color::Rgb(r, g, b))
                            } else {
                                Style::default()
                            };
                            // El fondo (proto 0.66.0, D4): ningún rol manda
                            // sobre él, y un medio bloque sin fondo es media
                            // imagen.
                            if let Some((r, g, b)) = span.bg {
                                style = style.bg(Color::Rgb(r, g, b));
                            }
                            s.style(style)
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

/// El visor ACOPLADO (L3): el fichero bajo el cursor, en su hueco.
///
/// El mismo renderer que [`draw_viewer`] —mismas filas, mismo preview de
/// plugin— dentro de un bloque del tamaño del hueco en vez de la pantalla
/// entera. La línea de estado del visor (encoding, EOL, pérdidas, truncado) va
/// en el borde de abajo: es el único sitio que dice QUÉ se está viendo, y un
/// visor que no lo dice miente por omisión.
///
/// Sin fichero, el hueco lleva un texto: un directorio, un listado vacío, o el
/// motivo por el que la lectura no pudo hacerse. Una denegación se PINTA aquí
/// y no abre nada — el preview sigue al cursor, así que un diálogo por
/// pulsación convertiría bajar por un directorio en una ráfaga de modales.
pub(crate) fn draw_preview(
    frame: &mut Frame<'_>,
    area: Rect,
    preview: &crate::preview::Preview,
    con_teclado: bool,
    app: &App,
) {
    let border = if con_teclado {
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
        // La ruta se recorta por el MEDIO: en un hueco estrecho lo que
        // identifica un fichero es su nombre, o sea la cola.
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
                            let s = Span::raw(span.text.clone());
                            let mut style = if let Some(role) = span.role {
                                app.theme.role(role)
                            } else if let Some((r, g, b)) = span.fg {
                                Style::default().fg(Color::Rgb(r, g, b))
                            } else {
                                Style::default()
                            };
                            // El fondo (proto 0.66.0, D4): ningún rol manda
                            // sobre él, y un medio bloque sin fondo es media
                            // imagen.
                            if let Some((r, g, b)) = span.bg {
                                style = style.bg(Color::Rgb(r, g, b));
                            }
                            s.style(style)
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect(),
        None => viewer.rows(inner_h).into_iter().map(Line::raw).collect(),
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// El sidebar de sitios (L3): discos y favoritos en un panel que se queda.
///
/// Todo lo que la plataforma nos da —la etiqueta de un volumen, su punto de
/// montaje, el nombre que el usuario le puso a un favorito— pasa por el mismo
/// enmascarado que el popup de unidades (`display_name`/`path_display`): un
/// `fuse.<subtype>` lo elige un usuario sin privilegios, y una etiqueta de
/// FAT es tan hostil como un nombre de fichero.
///
/// Un favorito roto se pinta ATENUADO y con su motivo traducido, nunca se
/// esconde: un favorito que desaparece solo es un fallo de config invisible.
pub(crate) fn draw_places(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &norte_frontend::places::PlacesState,
    con_teclado: bool,
    theme: &TuiTheme,
) {
    use norte_frontend::places::PlaceRow;

    let border = if con_teclado {
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
                let (nombre, hostile) = if label.is_empty() {
                    mount_name(mount)
                } else {
                    display_name(label)
                };
                // Corto y sin decimales: catorce celdas tienen que llevar el
                // nombre del montaje Y su espacio. Un `?` cuando el
                // filesystem no contestó — jamás un cero, que se leería como
                // «lleno» (la palabra entera la sigue diciendo el popup, que
                // sí tiene sitio).
                let libre = free.map_or_else(|| "?".to_owned(), norte_frontend::human_bytes_short);
                // El nombre de un montaje se recorta por el MEDIO: lo que
                // identifica `/home/oscar/.cache` es la cola, y con seis
                // montajes bajo `/home` una lista recortada por delante son
                // seis filas que ponen lo mismo.
                ListItem::new(Line::raw(two_fields(
                    &with_badge(&nombre, hostile),
                    &libre,
                    width,
                    middle,
                )))
            }
            PlaceRow::Favorite { name, target } => {
                let (text, hostile) = display_name(name.as_bytes());
                let left = with_badge(&text, hostile);
                match target {
                    Ok(_) => ListItem::new(Line::raw(head(&format!(" {left}"), width))),
                    // Roto: marca `!` y fila ATENUADA. El motivo entero no
                    // cabe en catorce celdas —«ruta inválida» son trece— y
                    // recortarlo dejaría media palabra diciendo nada, así que
                    // la fila dice QUE está roto y la barra de estado dice por
                    // qué cuando el cursor cae encima. Lo que no se hace es
                    // esconderla: un favorito que desaparece solo es un fallo
                    // de config invisible.
                    Err(_) => ListItem::new(Line::styled(
                        two_fields(&left, "!", width, head),
                        theme.role(Role::Info),
                    )),
                }
            }
        })
        .collect();
    let list = List::new(items).highlight_style(theme.role(Role::Selection));
    // El desplazamiento se calcula AQUÍ y no lo decide el widget, para que
    // `places_zones` pueda decir con qué fila del modelo se corresponde cada
    // fila de la pantalla (#226). Es el mismo número que ratatui elegía por su
    // cuenta —desplazamiento mínimo para que el cursor se vea, partiendo de
    // cero en cada frame—, así que la pantalla no cambia; lo que cambia es que
    // ahora hay UNA fuente y el ratón la puede leer.
    let mut estado = ListState::default().with_offset(if con_teclado {
        places_offset(state.cursor(), inner.height as usize)
    } else {
        0
    });
    estado.select(con_teclado.then(|| state.cursor()));
    frame.render_stateful_widget(list, inner, &mut estado);
}

/// Primera fila del modelo que se ve, para un cursor y un alto.
///
/// Desplazamiento MÍNIMO para que el cursor entre, empezando de cero: es lo
/// que hacía el widget con un `ListState` nuevo en cada frame, escrito para
/// que el hit test del ratón no tenga que adivinarlo.
pub(crate) const fn places_offset(cursor: usize, height: usize) -> usize {
    cursor.saturating_sub(height.saturating_sub(1))
}

/// Una fila pulsable del sidebar de sitios, en el frame de `area`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceZone {
    /// Fila de la pantalla.
    pub row: u16,
    /// Primera columna, inclusive.
    pub x0: u16,
    /// Última columna, inclusive.
    pub x1: u16,
    /// Índice dentro de [`norte_frontend::places::PlacesState::rows`].
    pub index: usize,
}

/// Las filas pulsables del sidebar, en el frame de `area`.
///
/// Vive junto al pintado y comparte con él el reparto y el desplazamiento
/// —igual que [`super::tab_zones`] y por lo mismo—: medir por un lado y pintar por
/// otro es cómo un click acaba activando la fila de al lado.
#[must_use]
pub fn places_zones(app: &App, area: Rect) -> Vec<PlaceZone> {
    let res = resolved_for(app, area);
    let Some((id, rect)) = placed_of_kind(&res, &app.layout, "places") else {
        return Vec::new();
    };
    let Some(state) = app.panes.places(id) else {
        return Vec::new();
    };
    // El interior del bloque: el marco no es pulsable.
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

/// Una fila pulsable del árbol, en el frame de `area` (#136).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeZone {
    /// Fila de la pantalla.
    pub row: u16,
    /// Primera columna, inclusive.
    pub x0: u16,
    /// Última columna, inclusive.
    pub x1: u16,
    /// La columna de la MARCA (`▾`/`▸`/`·`) de esta fila, que pliega y
    /// despliega. Sangrada por profundidad, igual que la pinta `draw_tree`.
    pub mark_x: u16,
    /// Índice dentro de [`norte_frontend::tree::Tree::rows`].
    pub index: usize,
}

/// Las filas pulsables del árbol, en el frame de `area`.
///
/// Vive junto al pintado y comparte con él el reparto y el desplazamiento, lo
/// mismo que [`places_zones`] y por lo mismo: medir por un lado y pintar por
/// otro es cómo un click acaba abriendo la rama de al lado.
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
    // El árbol pinta SIEMPRE su cursor, tenga el teclado o no (a diferencia
    // del sidebar), así que su desplazamiento no depende de quién teclea.
    let offset = places_offset(tree.cursor(), inner.height as usize);
    (0..inner.height as usize)
        .filter_map(|row| {
            let index = offset.checked_add(row)?;
            let fila = rows.get(index)?;
            // `  ` por nivel, y luego la marca: el mismo molde que `draw_tree`.
            let sangria = u16::try_from(fila.depth.saturating_mul(2)).unwrap_or(u16::MAX);
            Some(TreeZone {
                row: inner
                    .y
                    .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
                x0: inner.x,
                x1: inner.x.saturating_add(inner.width).saturating_sub(1),
                mark_x: inner.x.saturating_add(sangria),
                index,
            })
        })
        .collect()
}

/// Cómo se llama un punto de montaje en catorce celdas.
///
/// Sin el prefijo `⟨file⟩` de [`norte_frontend::path_display`] cuando el
/// esquema es local, que es SIEMPRE en `host.volumes`: en un panel de catorce
/// celdas ese prefijo se come la ruta entera y deja al lector mirando seis
/// filas que ponen lo mismo. El enmascarado no se pierde — cada segmento pasa
/// por `display_name` igual que hace `path_display`.
pub(crate) fn mount_name(mount: &norte_proto::VPath) -> (String, bool) {
    if mount.scheme() != "file" || mount.authority().is_some() {
        return norte_frontend::path_display(mount);
    }
    let mut text = String::new();
    let mut hostile = false;
    for seg in mount.segments() {
        let (t, h) = display_name(seg);
        text.push('/');
        text.push_str(&t);
        hostile |= h;
    }
    if text.is_empty() {
        text.push('/');
    }
    (text, hostile)
}

/// El porcentaje de una tarea, o `0` si todavía no se sabe.
///
/// La ARITMÉTICA vive en `norte_frontend::tasks`: la ventana gráfica pinta el
/// mismo tablero, y dos copias del mismo cálculo divergieron una vez ya —la
/// otra no caía a las entradas, así que un borrado se quedaba en cero—.
///
/// Aquí «no se sabe» se pinta como cero porque la barra tiene que medir algo;
/// el estado de al lado es el que dice si la tarea está viva.
pub(crate) fn progress_pct(p: &norte_proto::TaskProgress) -> u64 {
    norte_frontend::tasks::progress_pct(p).map_or(0, u64::from)
}

/// La etiqueta de una clase de task, por CATEGORÍA (nunca el `Debug`).
///
/// Una sola copia porque son dos las superficies que la pintan —la franja de
/// abajo y el panel de procesos— y una etiqueta que sale distinta en cada una
/// para la misma task es un bug que nadie reporta: se lee como si fueran dos
/// cosas diferentes.
pub(crate) fn kind_label(kind: norte_proto::TaskKind) -> &'static str {
    match kind {
        norte_proto::TaskKind::Copy => "copy",
        norte_proto::TaskKind::Move => "move",
        norte_proto::TaskKind::Delete => "delete",
        norte_proto::TaskKind::Undo => "undo",
        // Etiqueta mínima; el diálogo/pane virtual de Alt+F7 llega en
        // T6 de liveSearch — aquí solo evita el `match` no exhaustivo.
        norte_proto::TaskKind::Search => "search",
        norte_proto::TaskKind::Index => "index",
        norte_proto::TaskKind::Mkdir => "mkdir",
        norte_proto::TaskKind::Create => "create",
        norte_proto::TaskKind::Embed => "embed",
        norte_proto::TaskKind::RenameBatch => "rename",
        // Contar no muta nada, pero SALE en la franja como todo lo demás, y
        // caía al brazo genérico: «task 82 %» no dice que se está midiendo un
        // directorio.
        norte_proto::TaskKind::DirSize => "dir-size",
        // Etiqueta mínima, como la de `Search` en su día: el pane de
        // comparación llega en C7 de este mismo plan; esto solo evita
        // que una Task de `fs.compare` se pinte como genérica.
        norte_proto::TaskKind::Compare => "compare",
        // #311: mismo caso que `DirSize`. «task 40 %» no dice que lo que está
        // corriendo es el sha256 de lo que marcaste.
        norte_proto::TaskKind::Checksum => "checksum",
        // #314: y esta MUTA, así que menos todavía puede salir sin nombre.
        norte_proto::TaskKind::SetMode => "set-mode",
        // `Unknown` es la clase de un daemon N+1 que este proto YA
        // conocía como desconocida (vía `serde(other)`); el `_` es
        // `#[non_exhaustive]` (#126) — una variante de un norte-proto
        // más nuevo que este BINARIO no reconoce en absoluto. Mismo
        // caso de cara al usuario, misma etiqueta genérica.
        norte_proto::TaskKind::Unknown | _ => "task",
    }
}

/// Sobre QUÉ actúa una fila del tablero, recortada a `max` celdas.
///
/// La ruta se recorta por el MEDIO porque lo que identifica un fichero es su
/// nombre, o sea la cola — el mismo criterio que el título del visor acoplado.
/// Vacío cuando la task no publicó ninguna entrada (una búsqueda que todavía
/// no ha tocado nada), y entonces la fila se queda con su clase y su estado,
/// que es lo que se sabe.
///
/// La reinterpretación de nombres es la del pane con el FOCO. No es exacta —el
/// operando puede venir del otro pane— pero un nombre hostil pintado como
/// bytes crudos no se lee, y el badge de al lado dice que hubo reinterpretación.
fn operand_text(row: &crate::tasks::TaskRow, app: &App, max: usize) -> String {
    let Some(p) = row.operand.as_ref() else {
        return String::new();
    };
    let (texto, hostil) = norte_frontend::path_display_with(p, app.focused().name_encoding());
    let texto = if hostil {
        format!("{HOSTILE_BADGE} {texto}")
    } else {
        texto
    };
    norte_frontend::middle_ellipsis(&texto, max)
}

/// El panel de procesos (fase A): una fila por tarea, con barra y estado.
///
/// Las filas salen del `TaskBoard` que ya pinta la franja — este panel no
/// guarda una segunda lista — y el cursor se acota AQUÍ contra las filas de
/// este frame: una tarea puede terminar y desaparecer entre dos pinturas.
/// El árbol de directorios (#136).
///
/// Un nombre por fila, sangrado por profundidad, con un indicador de tres
/// estados: desplegada, plegada-con-hijos, y sin leer. El tercero importa —
/// pintar «hoja» a algo que todavía no se ha listado sería inventarse la
/// respuesta— y es el mismo criterio que el resto de la pantalla: lo que no se
/// sabe se dice, no se rellena.
///
/// Los nombres van por `display_name`, como el listado: un directorio con bidi
/// o invisibles no reordena esta columna.
pub(crate) fn draw_tree(
    frame: &mut Frame<'_>,
    area: Rect,
    tree: &crate::tree::Tree,
    app: &App,
    con_teclado: bool,
) {
    let theme = &app.theme;
    let border = if con_teclado {
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
                // Leída y sin hijos: una hoja de verdad.
                (false, Some(false)) => " ",
                // Sin leer: ni hoja ni rama, todavía.
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
    // El desplazamiento se calcula AQUÍ y no lo decide el widget, para que
    // `tree_zones` pueda decir con qué fila del modelo se corresponde cada fila
    // de la pantalla. Es el mismo número que ratatui elegía por su cuenta
    // —desplazamiento mínimo para que el cursor se vea, partiendo de cero en
    // cada frame—, así que la pantalla no cambia; lo que cambia es que ahora
    // hay UNA fuente y el ratón la puede leer (mismo arreglo que #226 en el
    // sidebar).
    let mut list_state =
        ListState::default().with_offset(places_offset(cursor, inner.height as usize));
    list_state.select(Some(cursor));
    let list = List::new(items).highlight_style(theme.role(Role::Selection));
    frame.render_stateful_widget(list, inner, &mut list_state);
}

pub(crate) fn draw_processes(
    frame: &mut Frame<'_>,
    area: Rect,
    processes: &crate::processes::Processes,
    app: &App,
    con_teclado: bool,
) {
    let theme = &app.theme;
    let border = if con_teclado {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("processes-title")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    // Que este panel tenga el TECLADO se decía solo con el color del borde, y
    // un lector que no distinga ese par de colores —o que no sepa que ese par
    // significa eso— ve un gestor de ficheros en el que las flechas han dejado
    // de funcionar y no tiene por dónde empezar. Es la misma lección de #111:
    // una señal solo-color no es una señal.
    //
    // El pie dice la salida, no el estado: «tiene el foco» no ayuda a nadie,
    // «Esc devuelve el teclado» sí.
    if con_teclado {
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
    let cursor = processes.cursor(rows.len());
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let p = &row.last;
            let pct = progress_pct(p);
            // Diez celdas de barra: cabe en un panel estrecho y sigue
            // diciendo de un vistazo por dónde va.
            let full = usize::try_from(pct / 10).unwrap_or(0).min(10);
            let bar: String = "█".repeat(full) + &"░".repeat(10 - full);
            let (state_txt, role) = match &p.state {
                norte_proto::TaskState::Completed => ("✓".to_owned(), Some(Role::Info)),
                norte_proto::TaskState::Cancelled => (t("task-cancelled"), Some(Role::Warning)),
                norte_proto::TaskState::Failed { .. } => (t("task-failed"), Some(Role::Error)),
                _ => (format!("{pct}%"), None),
            };
            // El número de task NO se pinta: dieciocho dígitos no le dicen
            // nada a nadie y se comen el ancho que necesita el nombre. Lo que
            // faltaba era el par «qué clase de trabajo» + «sobre qué», que ya
            // viaja entero en el progreso.
            let kind = kind_label(p.kind);
            // Ancho fijo de la fila: la marca, la clase, la barra, el estado y
            // los CUATRO espacios que los separan. Lo que sobra es del
            // operando, y si no sobra nada se queda vacío en vez de empujar
            // nada fuera.
            //
            // El `ratatui` recorta la línea al ancho sin decir nada, así que
            // pasarse de uno no rompe el pinta: se come el `✓` del final, que
            // es justo el dato que la fila existe para dar.
            let fijo = 1 + 4 + kind.chars().count() + 10 + state_txt.chars().count();
            let hueco = usize::from(inner.width).saturating_sub(fijo);
            let operando = operand_text(row, app, hueco);
            let marca = if i == cursor { '▶' } else { ' ' };
            let header = if operando.is_empty() {
                format!("{marca} {kind} {bar} ")
            } else {
                format!("{marca} {kind} {operando} {bar} ")
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

/// La hoja de atributos (fase A): lo que se sabe de la entrada bajo el cursor.
///
/// Todo sale de la `Entry` que el listado ya tenía, así que esta función no
/// puede pedir nada aunque quisiera. QUÉ filas van dentro lo decide
/// [`norte_frontend::metadata::sheet`], que es la misma que usa la ventana:
/// esto solo las pinta. Cuando cada frontend tenía su copia de la lista ya
/// habían divergido —el arreglo que marca un valor de atributo hostil se
/// aplicó en una sola— y ese es exactamente el fallo que una copia produce.
pub(crate) fn draw_metadata(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: Option<&(norte_proto::Entry, bool)>,
    sigue: Option<&(String, bool)>,
    app: &App,
    con_teclado: bool,
) {
    let theme = &app.theme;
    let border = if con_teclado {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    // El título dice a QUÉ LISTADO sigue, no solo que es la hoja: con dos
    // listados abiertos, «Detalles» a secas no dice de qué son los detalles,
    // y la única forma de averiguarlo era mover el cursor y mirar si la hoja
    // se movía. La ruta se recorta por el medio y con marca, como cualquier
    // otra ruta de este fichero: el borde del bloque no avisa de un corte.
    let titulo = match sigue {
        Some((ruta, hostil)) => format!(
            " {} · {} ",
            t("metadata-title"),
            norte_frontend::middle_ellipsis(
                &with_badge(ruta, *hostil),
                (area.width as usize).saturating_sub(t("metadata-title").chars().count() + 6),
            )
        ),
        None => format!(" {} ", t("metadata-title")),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(titulo)
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let Some((e, fila_de_subir)) = entry else {
        frame.render_widget(
            Paragraph::new(Line::styled(t("metadata-empty"), theme.role(Role::Title))),
            inner,
        );
        return;
    };

    let catalog = app.attr_catalog(e.path.scheme());
    let ancho = inner.width as usize;
    let lines: Vec<Line<'_>> =
        norte_frontend::metadata::sheet(e, *fila_de_subir, catalog, norte_i18n::active())
            .into_iter()
            .map(|f| {
                // El valor se recorta por el MEDIO y con marca. Un
                // `Paragraph` sin wrap corta por la derecha y sin decirlo, y
                // el campo `Destino` es una ruta entera: en un panel estrecho
                // `⟨file⟩/home/oscar/proyectos/norte-secreto` quedaba como
                // `⟨file⟩/home/oscar/proyectos`, que es otro directorio que
                // además existe. Es la misma regla que el resto de las rutas
                // de este fichero.
                let etiqueta = format!("{} ", f.label);
                let sitio = ancho.saturating_sub(crate::ui::text::cells(&etiqueta));
                let valor =
                    norte_frontend::middle_ellipsis(&with_badge(&f.value, f.hostile), sitio);
                Line::from(vec![
                    Span::styled(etiqueta, theme.role(Role::Title)),
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
            // Por CATEGORÍA (Display estable), jamás Debug de cara al usuario.
            // El estado se colorea por rol (error rojo, hecho info).
            let (state, role) = match &p.state {
                norte_proto::TaskState::Completed => ("✓".to_owned(), Some(Role::Info)),
                norte_proto::TaskState::Cancelled => (t("task-cancelled"), Some(Role::Warning)),
                norte_proto::TaskState::Failed { error } => {
                    (format!("✗ {error}"), Some(Role::Error))
                }
                _ => (format!("{pct}%"), None),
            };
            let kind = kind_label(p.kind);
            // Igual que el panel: la clase y el operando, no el id. La franja
            // decía « copy #7318349021 45 % », que es la misma línea para
            // cualquier copia de cualquier cosa.
            let fijo = 1 + kind.chars().count() + 2 + state.chars().count();
            let hueco = usize::from(area.width).saturating_sub(fijo);
            let operando = operand_text(row, app, hueco);
            let head = if operando.is_empty() {
                Span::raw(format!(" {kind} "))
            } else {
                Span::raw(format!(" {kind} {operando} "))
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

/// La hora de una línea de registro, del módulo COMPARTIDO.
///
/// Estaba aquí hasta que la ventana necesitó la misma (#326): dos ideas de qué
/// hora es en el panel de registro de cada frontend es la clase de diferencia
/// que nadie mira hasta que compara dos capturas de pantalla.
use norte_frontend::format::hora_utc;

/// El panel de registro (#323): lo que está pasando, sin salir de la TUI.
pub(crate) fn draw_log(frame: &mut Frame<'_>, area: Rect, app: &App, con_teclado: bool) {
    use std::fmt::Write as _;
    let theme = &app.theme;
    let border = if con_teclado {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let panel = &app.log_panel;
    let fuente = crate::logview::fuente_efectiva(app);
    // El título dice el nivel, la FUENTE y el filtro: sin eso, un panel que se
    // ve vacío no distingue «no ha pasado nada» de «lo estás filtrando fuera»
    // ni de «estás mirando el registro del otro proceso», que es la confusión
    // que hace desconfiar de un visor de logs.
    //
    // El nivel es SIEMPRE el que se ENSEÑA, en todas las fuentes: es el que la
    // tecla controla y el que filtra la lista. Marcar aquí el que el daemon
    // contestó tener puesto sería el peor error posible del panel — con el
    // daemon en `trace` y el panel en `info`, la cabecera diría `trace`
    // mientras cada línea `debug` que cruza el socket se tira en silencio.
    let mut titulo = format!(" {} · {} ", t("log-title"), panel.level().label().trim());
    // La fuente solo cuando hay dos sitios de los que pueda venir una línea.
    // Sin daemon no hay segmento y no falta nada: un `ntc` corriente tiene un
    // proceso y un anillo, y una frase sobre el origen contestaría una pregunta
    // que nadie se ha hecho — exactamente el panel que dejó #326.
    if let Some(fuente_txt) = crate::logview::etiqueta_de_fuente(app, fuente) {
        let _ = write!(titulo, "· {fuente_txt} ");
    }
    // Si algún anillo está capturando MÁS de lo que se enseña, se dice, y con
    // los dos a la vista cada parte dice de quién habla. Pedir TRACE y volver a
    // INFO deja el proceso capturando TRACE el resto de la sesión —a propósito,
    // para que ir y volver no borre lo de en medio— y sin esta línea eso no se
    // ve por ninguna parte.
    let captura = crate::logview::nota_de_captura(app, fuente);
    if !captura.is_empty() {
        let _ = write!(titulo, "· {captura} ");
    }
    if !panel.filter().is_empty() {
        let _ = write!(
            titulo,
            "· /{} ",
            norte_encoding::mask_terminal_hazards(panel.filter())
        );
    }
    // Lo descartado se DICE, y por anillo: el local cuenta lo evacuado desde
    // que arrancó el proceso, el del daemon lo que ESTA apertura se perdió. Son
    // números distintos y no se suman. Un anillo que tira lo viejo en silencio
    // hace que el lector busque una línea que estuvo y ya no está, y concluya
    // que el registro miente.
    let descartes = crate::logview::nota_de_descartes(app, fuente);
    if !descartes.is_empty() {
        let _ = write!(titulo, "· {descartes} ");
    }
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(titulo)
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    // El pie: o el filtro que se está tecleando, o las teclas. El campo GANA
    // porque mientras se escribe es lo único que importa, y porque un cursor
    // que no se ve es un campo que no parece un campo.
    if let Some(input) = &app.log_filter_input {
        block = block.title_bottom(Line::styled(
            format!(" /{}▏", norte_encoding::mask_terminal_hazards(input)),
            theme.role(Role::Match),
        ));
    } else if con_teclado {
        // La tecla de la fuente solo se ofrece cuando HAY una segunda: anunciar
        // un mando que recorrería tres vistas del mismo anillo es prometer algo
        // que no existe. Es la misma regla que en la ventana, donde el selector
        // simplemente no se pinta.
        let teclas = if app.log_remote.servicio == crate::logview::Servicio::Sirve {
            format!("{} · {}", t("log-keys"), t("log-keys-source"))
        } else {
            t("log-keys")
        };
        block = block.title_bottom(Line::styled(format!(" {teclas} "), theme.role(Role::Info)));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if app.log_ring.is_none() && fuente == norte_frontend::logpanel::LogSource::Window {
        // Sin anillo instalado (tests, o un embebedor que no montó el
        // subscriber) y sin daemon que sirva el suyo se dice, en vez de pintar
        // un panel vacío que parece que no pasa nada. No es «no se registra
        // nada»: el proceso sigue escribiendo a su fichero; lo que falta es el
        // anillo en memoria, que es lo que este panel lee.
        frame.render_widget(
            Paragraph::new(Line::styled(t("log-no-ring"), theme.role(Role::Warning))),
            inner,
        );
        return;
    }
    // Las dos fuentes, mezcladas por marca de tiempo y ya filtradas (#328).
    // Prestadas, no clonadas: el anillo ya clonó una vez en su `snapshot` y
    // aquí se pinta como mucho una pantalla.
    let lineas = crate::logview::instantanea(app);
    let visibles = crate::logview::visibles(app, &lineas);
    let alto = usize::from(inner.height);
    let desde = panel.window_start(visibles.len(), alto);
    if visibles.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(t("log-empty"), theme.role(Role::Title))),
            inner,
        );
        return;
    }
    let pintadas: Vec<Line<'_>> = visibles
        .iter()
        .skip(desde)
        .take(alto)
        .map(|(l, origen)| {
            let rol = match l.level {
                norte_config::logline::LogLevel::Error => Role::Error,
                norte_config::logline::LogLevel::Warn => Role::Warning,
                _ => Role::Info,
            };
            // El módulo y el mensaje llevan rutas y nombres de host que eligió
            // alguien que no es el lector: pasan por el mismo enmascarado que
            // cualquier otro texto ajeno antes de tocar la terminal.
            let cuerpo =
                norte_encoding::mask_terminal_hazards(&format!("{}: {}", l.target, l.message));
            // Una línea del DAEMON se marca al margen, y solo con las dos
            // fuentes en pantalla: con una sola no hay nada que distinguir, y
            // el filete gastaría dos columnas por línea para no decir nada. Un
            // filete y no un color, igual que en la ventana: el color ya lo
            // tiene tomado el nivel, que es lo que se busca de un vistazo.
            let margen = match (fuente, origen) {
                (
                    norte_frontend::logpanel::LogSource::Both,
                    norte_frontend::logpanel::LogSource::Daemon,
                ) => "│ ",
                (norte_frontend::logpanel::LogSource::Both, _) => "  ",
                _ => "",
            };
            Line::from(vec![
                Span::styled(margen, theme.role(Role::BorderUnfocused)),
                Span::raw(format!("{} ", hora_utc(l.epoch_ms))),
                Span::styled(format!("{} ", l.level.label()), theme.role(rol)),
                Span::raw(cuerpo),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(pintadas), inner);
}

#[cfg(test)]
mod draw_log_tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Pinta el panel de registro y devuelve lo que quedó en el buffer.
    fn pintado(app: &App, ancho: u16, alto: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(ancho, alto)).expect("terminal de test");
        terminal
            .draw(|f| draw_log(f, f.area(), app, true))
            .expect("draw");
        terminal.backend().to_string()
    }

    /// Las dos columnas de margen de una fila pintada.
    ///
    /// Hay que pelar dos cosas antes: la comilla que pone el `Display` de
    /// `TestBackend` y el BORDE izquierdo del bloque, que es otro `│` y que la
    /// primera versión de este test confundió con el filete del daemon —
    /// declarando marcada como remota una línea de esta terminal.
    fn margen(fila: &str) -> String {
        fila.chars()
            .skip_while(|c| *c == '"')
            .skip(1)
            .take(2)
            .collect()
    }

    /// Con un daemon aparte, las DOS fuentes llegan a las filas pintadas, y la
    /// del daemon se distingue por el filete del margen (#328).
    ///
    /// Es el agujero que `ntc --socket` tenía: los providers, el journal, la
    /// política y el motivo por el que una conexión falló están en el otro
    /// proceso, y este panel solo enseñaba lo de la terminal. La ventana ya lo
    /// resolvió, y arreglarlo en un solo frontend es lo que los hace divergir
    /// en silencio (ADR 0077).
    #[test]
    fn el_panel_pinta_la_terminal_y_el_daemon_y_los_distingue() {
        let mut app = crate::app::testutil::app_dos_panes();
        let anillo = norte_config::logring::LogRing::new(10);
        {
            use tracing_subscriber::layer::SubscriberExt as _;
            let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(&anillo));
            tracing::subscriber::with_default(s, || tracing::info!("linea-de-la-terminal"));
        }
        let local_ms = anillo.snapshot()[0].epoch_ms;
        app.log_ring = Some(anillo);
        app.log_remote.hay_daemon = true;
        app.toggle_log();
        let epoca = app.log_remote.epoca;
        crate::logview::aterrizar_tail(
            &mut app,
            epoca,
            Ok(norte_proto::methods::LogTailResult {
                lines: vec![norte_proto::methods::LogLine {
                    epoch_ms: local_ms + 1,
                    level: "info".to_owned(),
                    target: "norte_core::daemon".to_owned(),
                    message: "linea-del-daemon".to_owned(),
                }],
                next: 7,
                lost: 0,
                // El nivel del daemon, MÁS alto que el que el panel enseña:
                // es lo que hace visible la regla de los dos niveles.
                level: "trace".to_owned(),
                capacity: 2000,
            }),
        );

        let texto = pintado(&app, 120, 8);
        let fila_local = texto
            .lines()
            .find(|l| l.contains("linea-de-la-terminal"))
            .expect("la línea de esta terminal no se pintó");
        let fila_daemon = texto
            .lines()
            .find(|l| l.contains("linea-del-daemon"))
            .expect("la línea del daemon no se pintó");
        // El filete del margen es lo que separa «el provider falló» de «la
        // terminal no pudo pintarlo», que se leen igual y son dos averías
        // distintas.
        assert_eq!(
            margen(fila_daemon),
            "│ ",
            "la línea del daemon no se marcó al margen: {fila_daemon:?}"
        );
        assert_eq!(
            margen(fila_local),
            "  ",
            "la línea de esta terminal se marcó como del daemon: {fila_local:?}"
        );

        // El nivel que se MARCA es el que se ENSEÑA, en todas las fuentes: el
        // del daemon se dice en la nota de captura y no en la cabecera. Con la
        // cabecera diciendo `traza` mientras el filtro sigue en `info`, cada
        // línea DEBUG del daemon cruzaría el socket y se tiraría en silencio.
        let cabecera = texto.lines().next().unwrap_or_default();
        assert!(
            cabecera.contains(norte_config::logline::LogLevel::Info.label().trim()),
            "la cabecera no marca el nivel que se enseña: {cabecera:?}"
        );
        assert!(
            texto.contains(&norte_i18n::ta(
                "log-capturing-daemon",
                &[(
                    "level",
                    norte_config::logline::LogLevel::Trace.label().trim()
                )]
            )),
            "no se dice que el daemon captura más de lo que se ve: {texto}"
        );
    }

    /// Sin daemon que sirva su registro no se ofrece la tecla de la fuente:
    /// recorrer tres vistas del MISMO anillo es un mando que promete algo que
    /// no existe.
    #[test]
    fn la_tecla_de_la_fuente_solo_se_ofrece_cuando_hay_dos() {
        let mut app = crate::app::testutil::app_dos_panes();
        app.log_ring = Some(norte_config::logring::LogRing::new(10));
        app.toggle_log();
        let sin = pintado(&app, 120, 8);
        assert!(
            !sin.contains(&norte_i18n::t("log-keys-source")),
            "se ofreció la fuente sin una segunda que ofrecer: {sin}"
        );

        app.log_remote.hay_daemon = true;
        app.log_remote.servicio = crate::logview::Servicio::Sirve;
        let con = pintado(&app, 120, 8);
        assert!(
            con.contains(&norte_i18n::t("log-keys-source")),
            "con daemon, la tecla de la fuente no se anuncia: {con}"
        );
    }

    /// Un `ntc` corriente —sin daemon, que es el arranque por defecto— pinta el
    /// panel EXACTAMENTE como lo dejó #326: un proceso, un anillo, y ni una
    /// palabra sobre un origen ni sobre un daemon.
    ///
    /// La ausencia del segmento es la respuesta. Cualquier frase ahí contesta
    /// una pregunta que nadie se ha hecho, y las que había —«el daemon registra
    /// aparte», «este daemon no sirve su registro»— hablaban de alguien que no
    /// existe.
    #[test]
    fn sin_daemon_el_panel_es_el_de_326() {
        let mut app = crate::app::testutil::app_dos_panes();
        let anillo = norte_config::logring::LogRing::new(10);
        {
            use tracing_subscriber::layer::SubscriberExt as _;
            let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(&anillo));
            tracing::subscriber::with_default(s, || tracing::info!("una linea cualquiera"));
        }
        app.log_ring = Some(anillo);
        app.toggle_log();

        let texto = pintado(&app, 120, 8);
        let cabecera = texto.lines().next().unwrap_or_default();
        assert!(
            texto.contains("una linea cualquiera"),
            "el panel de siempre dejó de pintar: {texto}"
        );
        for clave in [
            "log-source-window",
            "log-source-both",
            "log-source-daemon",
            "log-source-unsupported",
            "log-source-daemon-level",
            "log-keys-source",
        ] {
            assert!(
                !texto.contains(&norte_i18n::t(clave)),
                "se habló de un daemon que no existe ({clave}): {texto}"
            );
        }
        // Y lo que sí tiene que seguir estando: el título y el nivel.
        assert!(
            cabecera.contains(&norte_i18n::t("log-title"))
                && cabecera.contains(norte_config::logline::LogLevel::Info.label().trim()),
            "el título perdió lo suyo: {cabecera:?}"
        );
    }
}
