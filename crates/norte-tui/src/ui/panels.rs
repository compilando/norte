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
/// puede pedir nada aunque quisiera. El tamaño va por `human_bytes_short`, que
/// redondea hacia ABAJO y no se recorta: un tamaño cortado por la cabeza es un
/// número FALSO, no una etiqueta truncada (la lección de L3).
pub(crate) fn draw_metadata(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: Option<&norte_proto::Entry>,
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
        .title(format!(" {} ", t("metadata-title")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let Some(e) = entry else {
        frame.render_widget(
            Paragraph::new(Line::styled(t("metadata-empty"), theme.role(Role::Title))),
            inner,
        );
        return;
    };

    let mut lines: Vec<Line<'_>> = Vec::new();
    let mut field = |clave: &str, value: String| {
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", t(clave)), theme.role(Role::Title)),
            Span::raw(value),
        ]));
    };

    let nombre = e
        .path
        .file_name()
        .map_or_else(Vec::new, |s| s.as_bytes().to_vec());
    let (text, hostile) = display_name(&nombre);
    field("metadata-name", with_badge(&text, hostile));
    field(
        "metadata-kind",
        t(match e.kind {
            norte_proto::EntryKind::Dir => "metadata-kind-dir",
            norte_proto::EntryKind::File => "metadata-kind-file",
            norte_proto::EntryKind::Symlink => "metadata-kind-symlink",
            norte_proto::EntryKind::Other => "metadata-kind-other",
        }),
    );
    if let Some(n) = e.size {
        field(
            "metadata-size",
            format!("{} ({n})", norte_frontend::human_bytes_short(n)),
        );
    }
    if let Some(ms) = e.mtime_ms {
        field(
            "metadata-mtime",
            norte_frontend::columns::format_mtime(ms, norte_frontend::columns::TimeFormat::Iso, ms),
        );
    }
    // Los atributos que el provider YA había traído con el listado. Se pintan
    // por la misma puerta que la columna equivalente —`styled_cell`, con el
    // estilo por defecto del id— para que la hoja y la columna no puedan
    // discrepar sobre lo que vale un atributo.
    let catalog = app.attr_catalog(e.path.scheme());
    let now = e.mtime_ms.unwrap_or(0);
    for id in e.attrs.keys() {
        let col: norte_frontend::columns::ColumnId =
            norte_frontend::columns::ColumnId::Attr(id.clone());
        let style = norte_frontend::columns::ColumnStyle::default_for_id(&col, catalog);
        let label = norte_frontend::columns::header_label(&col, &style, catalog);
        if let Some(celda) = norte_frontend::columns::styled_cell(e, &col, now, &style) {
            free_field(&mut lines, theme, &label, &celda);
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Una fila etiqueta/valor cuya etiqueta no sale de Fluent sino del catálogo.
pub(crate) fn free_field(
    lines: &mut Vec<Line<'static>>,
    theme: &TuiTheme,
    label: &str,
    value: &str,
) {
    lines.push(Line::from(vec![
        Span::styled(format!("{label} "), theme.role(Role::Title)),
        Span::raw(value.to_owned()),
    ]));
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
