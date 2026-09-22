//! La barra de estado del pane con foco: sus segmentos de marcas y la línea que
//! los junta.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

use super::HOSTILE_BADGE;
use super::text::cells;
use crate::app::{App, Pane};
use norte_i18n::{t, ta};

/// El aviso de marcas podadas de la status bar (#103), con su sangrado. Lo
/// marcado en sí ya no va aquí: es el elemento `marks` de la mitad derecha
/// (ADR 0132); la PODA es un aviso y se queda en la izquierda.
pub(crate) fn pruned_segment(pane: &Pane) -> String {
    // La frase la REDACTA el crate compartido: la ventana pone la misma en
    // su cabecera, y dos redacciones del mismo hecho es de donde salió media
    // auditoría de paridad (ADR 0077). Aquí queda el espaciado, que sí es de
    // esta barra.
    let s = norte_frontend::notes::pruned_marks(pane.pruned_marks(), norte_i18n::active());
    if s.is_empty() { s } else { format!("  {s}") }
}

pub(crate) fn draw_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let c = compose(app, area);
    frame.render_widget(
        Paragraph::new(c.text).style(app.theme.role(Role::StatusBar)),
        area,
    );
}

/// Un elemento pulsable de la mitad derecha (ADR 0132): dónde cae y qué
/// comando corre, por el mismo despacho que su atajo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusItemZone {
    /// Fila.
    pub row: u16,
    /// Primera columna, inclusive.
    pub x0: u16,
    /// Última columna, inclusive.
    pub x1: u16,
    /// El comando, del catálogo.
    pub command: &'static str,
}

/// Las zonas de los elementos que se pulsan, para el frame `area`.
#[must_use]
pub fn status_item_zones(app: &App, area: Rect) -> Vec<StatusItemZone> {
    let Some(status) = status_rect(app, area) else {
        return Vec::new();
    };
    compose(app, status)
        .items
        .into_iter()
        .map(|(x0, x1, command)| StatusItemZone {
            row: status.y,
            x0,
            x1,
            command,
        })
        .collect()
}

/// Los hechos que la mitad derecha necesita, del pane con foco.
fn status_input(app: &App) -> norte_frontend::statusbar::StatusInput {
    norte_frontend::statusbar::StatusInput::from_pane(
        app.focused().state(),
        app.strip.view(app.now_ms()),
        app.notices_unread,
    )
}

/// Celdas entre dos elementos seguidos.
const SEP: usize = 2;

/// El rectángulo de la barra de estado del frame `area`, o `None` con un
/// overlay delante: entonces no es pulsable, por la misma regla que la
/// barra de paneles (`panel_bar_visible`).
fn status_rect(app: &App, area: Rect) -> Option<Rect> {
    use super::geometry::{body_rect, chrome_body, resolved_frame, slot_rect};
    if crate::mouse::overlay_open(app) || app.menu.is_some() {
        return None;
    }
    let res = resolved_frame(app, area);
    let body = body_rect(&res, &app.layout).unwrap_or_else(|| chrome_body(app, area));
    Some(slot_rect(&res, crate::panel::SLOT_STATUS).unwrap_or(Rect {
        x: body.x,
        y: body.y.saturating_add(body.height).saturating_sub(1),
        width: body.width,
        height: 1,
    }))
}

/// Lo que compone la barra: el texto, y las columnas de lo que se pulsa.
struct Composed {
    text: String,
    session: Option<(u16, u16)>,
    /// Los elementos pulsables de la derecha: columnas y comando.
    items: Vec<(u16, u16, &'static str)>,
}

/// Dónde cae el indicador de sesión suelta en el frame, si se está pintando.
///
/// Es la zona pulsable de la barra de estado: un clic encima abre la ayuda
/// en la página que explica qué significa. Sale de la MISMA composición que
/// pinta la línea (`compose`), así que solo existe cuando el indicador está
/// de verdad en pantalla — con un mensaje, una espera o una búsqueda viva
/// delante, la línea es otra y no hay nada que pulsar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionZone {
    /// Fila.
    pub row: u16,
    /// Primera columna, inclusive.
    pub x0: u16,
    /// Última columna, inclusive.
    pub x1: u16,
}

/// La zona del indicador de sesión para el frame `area`, si se pinta.
///
/// El rectángulo de la barra sale del mismo reparto que `draw_body`, y por
/// el mismo camino: un clic resuelto contra otra geometría caería en la
/// celda de al lado.
#[must_use]
pub fn session_zone(app: &App, area: Rect) -> Option<SessionZone> {
    // Con un overlay delante no hay zona (`status_rect`): el visor pinta su
    // propio pie y no esta barra, y con la ayuda o un modal encima la barra
    // no es pulsable. `handle_at` ya corta antes por `overlay_open`, pero
    // eso es un orden de comprobaciones, no una garantía de esta función.
    let status = status_rect(app, area)?;
    compose(app, status).session.map(|(x0, x1)| SessionZone {
        row: status.y,
        x0,
        x1,
    })
}

/// La línea de estado, y las columnas de lo que se pulsa en ella.
///
/// Dos mitades (ADR 0132). La DERECHA son los elementos de
/// `[ui] status_items` que caben en la mitad del ancho, descartados por
/// prioridad (`statusbar::fit`); la IZQUIERDA es la cadena de siempre
/// —espera, arrastre, mensaje, búsqueda, ruta con sus avisos— sobre el
/// ancho que quede, y es la que cede: recortada, nunca empujada fuera.
fn compose(app: &App, area: Rect) -> Composed {
    // Los de los plugins primero, a la izquierda de la mitad derecha (ADR
    // 0137); son los primeros en ceder, así que el orden no les da sitio.
    let mut lista = norte_frontend::statusbar::plugin_items(
        app.focused().state(),
        &app.status_plugins,
        norte_i18n::active(),
    );
    lista.extend(norte_frontend::statusbar::items(
        &status_input(app),
        app.chrome.status_items(),
        norte_i18n::active(),
    ));
    let ancho = usize::from(area.width);
    // Un aviso persistente (sesión suelta, journal) tiene que caber ENTERO
    // en la izquierda: la derecha es información y cede antes que un aviso.
    // Lo MÍNIMO que `compose_line` pinta con él es ` {aviso}{secuencia}`:
    // el margen, el aviso entero y lo tecleado a medias, que tampoco se
    // puede perder (ADR 0006). Y el presupuesto de la derecha descuenta sus
    // propios dos márgenes, que `fit` no cuenta.
    let reserva_aviso = app.persistent_banner().map_or(0, |w| {
        let seq = if app.pending.is_empty() {
            0
        } else {
            cells(&format!("  [{} …]", app.pending))
        };
        1 + cells(&w) + seq
    });
    let presupuesto = (ancho / 2)
        .min(ancho.saturating_sub(reserva_aviso))
        .saturating_sub(2);
    let elegidos = norte_frontend::statusbar::fit(&lista, presupuesto, SEP);
    let derecha: Vec<&norte_frontend::statusbar::StatusItemView> = elegidos.iter().collect();
    if derecha.is_empty() {
        return compose_line(app, area);
    }
    // Un espacio delante del primero y uno detrás del último, como los
    // márgenes de la izquierda.
    let ancho_der =
        derecha.iter().map(|v| v.cells()).sum::<usize>() + SEP * (derecha.len() - 1) + 2;
    let ancho_izq = ancho.saturating_sub(ancho_der);
    let izquierda = Rect {
        width: u16::try_from(ancho_izq).unwrap_or(u16::MAX),
        ..area
    };
    let mut c = compose_line(app, izquierda);
    let texto = super::text::take_width(&c.text, ancho_izq);
    let pad = ancho_izq.saturating_sub(cells(&texto));
    let mut linea = format!("{texto}{} ", " ".repeat(pad));
    let mut x = ancho_izq + 1;
    for (n, v) in derecha.iter().enumerate() {
        if n > 0 {
            linea.push_str(&" ".repeat(SEP));
            x += SEP;
        }
        linea.push_str(&v.text);
        // La barra ligera (ADR 0146) va detrás del texto, separada por un
        // espacio; `cells()` ya la cuenta en el reparto.
        if v.bar {
            linea.push(' ');
            linea.push_str(&norte_frontend::task_strip::bar_glyphs(
                v.progress.and_then(|p| p.percent),
                app.now_ms(),
            ));
        }
        let w = v.cells();
        if let Some(cmd) = v.command {
            let x0 = area.x.saturating_add(u16::try_from(x).unwrap_or(u16::MAX));
            let x1 = x0.saturating_add(u16::try_from(w).unwrap_or(u16::MAX).saturating_sub(1));
            c.items.push((x0, x1, cmd));
        }
        x += w;
    }
    linea.push(' ');
    c.text = linea;
    c
}

/// Una ruta de menos de tantas celdas no dice dónde estás: con un aviso
/// persistente que la dejara más corta, el aviso se queda la línea entera,
/// como hacía siempre.
const RUTA_LEGIBLE: usize = 12;

/// Las columnas del indicador de sesión cuando el aviso persistente acaba
/// en la celda `fin` (exclusiva, relativa a la barra): el indicador cierra
/// el aviso (`persistent_banner`), así que son sus últimas celdas. `None`
/// si no hay indicador o no cabe ENTERO: media palabra no es un indicador.
fn zona_de_sesion(app: &App, area: Rect, fin: usize) -> Option<(u16, u16)> {
    let badge = app.session_banner()?;
    let ancho = cells(&badge);
    let x0 = area
        .x
        .saturating_add(u16::try_from(fin.saturating_sub(ancho)).unwrap_or(u16::MAX));
    let x1 = area
        .x
        .saturating_add(u16::try_from(fin.saturating_sub(1)).unwrap_or(u16::MAX));
    (x1 < area.x.saturating_add(area.width)).then_some((x0, x1))
}

/// La línea de estado sin la insignia, y las columnas del indicador de
/// sesión si va en ella.
fn compose_line(app: &App, area: Rect) -> Composed {
    let mut session = None;
    let pane = app.focused();
    // La posición y lo marcado son elementos de la mitad derecha desde el
    // ADR 0132 (`position`, `marks`); aquí quedan la ruta y los AVISOS.
    let pruned = pruned_segment(pane);
    let (dir_text, dir_hostile) =
        norte_frontend::path_display_with(pane.dir(), pane.name_encoding());
    let mark = if dir_hostile { HOSTILE_BADGE } else { "" };
    // Sin chuleta de teclas: mentiría según el preset. La secuencia pendiente
    // SÍ se pinta (ADR 0006), y desde K3a el panel which-key
    // ([`draw_which_key`]) pinta encima de esta barra lo que puede SEGUIR a
    // esa secuencia. Este segmento no desaparece con él: es la única línea que
    // sobrevive a un panel recortado en un terminal bajo.
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
    // Un arrastre EN VUELO manda sobre todo lo demás mientras dure. Es lo
    // único de esta línea que anuncia una MUTACIÓN a punto de proponerse, y
    // el gesto pide la decisión (copiar o mover) ANTES de que el botón suba:
    // sin este renglón el usuario suelta a ciegas. Dura lo que dura el botón
    // pulsado y no consume nada — el mensaje que tape sigue ahí al soltar.
    // Una espera EN CURSO manda sobre todo lo demás: mientras dura, cualquier
    // otra cosa de esta línea —el mensaje de la operación anterior, el
    // contador— describe un estado que ya no es el actual, y el lector la está
    // mirando justo porque quiere saber si el programa sigue vivo. Se va sola
    // al acabar la espera (`App::busy` lo limpia quien esperó), así que no
    // consume ni tapa nada de forma permanente. Antes del umbral no entra
    // aquí: `visible()` decide por todas las superficies.
    let text = if let Some(busy) = app.busy.as_ref().filter(|b| b.visible()) {
        format!(
            " {} {}  {}",
            busy.frame(),
            t(busy.kind.key()),
            t("busy-cancel")
        )
    } else if let Some(drag) = crate::mouse::drop_hint(app) {
        format!(" {drag}")
    } else if let Some(msg) = &app.message {
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
                    let line = m.line.map_or_else(String::new, |l| format!(":{l}"));
                    let preview = m.preview.as_deref().map_or_else(String::new, |p| {
                        format!(" {}", crate::app::detail_for_bar(p))
                    });
                    format!("  [{line}{preview}]")
                });
            format!(
                " {}{hit}{seq}",
                ta(key, &[("n", &pane.entries().len().to_string())])
            )
        }
    } else if let Some(lua) = &app.lua_status {
        format!(" {lua}{seq}")
    } else {
        // #44: sesión remota degradada a texto plano, y #177: sesión que muta
        // sin quedar registrada en el journal. PERSISTENTES (como
        // `search-status-failed`): sobreviven a las teclas — sin `message`, sin
        // búsqueda viva y sin hook Lua siguen avisando en cada frame.
        // H3d: la frase se COMPONE aquí desde el valor estructurado (una
        // conexión: la nombra; varias: cuántas), en vez de guardarse ya escrita.
        //
        // Desde el 2026-09-11 el aviso NO sustituye la línea: va a la DERECHA
        // de la ruta y el contador, que siguen ahí. Sustituirla dejaba sin
        // «fichero x/x» a quien tenía una sesión suelta todo el día. Solo si
        // cabe con una ruta legible; si no, el aviso solo, como antes.
        let warn = app.persistent_banner();
        let reserva = warn.as_ref().map_or(0, |w| cells(w) + 2);
        // #93: el contenedor omitió entradas de su índice — el listado que
        // se ve NO es todo lo que el archivo contiene. Persistente mientras
        // el pane esté dentro (paralelo del badge hostil, jamás silencioso).
        let sangrado = |s: String| if s.is_empty() { s } else { format!("  {s}") };
        let lang = norte_i18n::active();
        let omitidas = sangrado(norte_frontend::notes::skipped(pane.skipped(), lang));
        // #57: modo de reinterpretación activo — PERSISTENTE mientras dure
        // (los nombres pintados no son los bytes; el usuario debe saberlo
        // en todo momento, no solo en el mensaje del toggle).
        let nombres = sangrado(norte_frontend::notes::names_encoding(
            pane.name_encoding(),
            lang,
        ));
        // #107: ocultación activa con entradas apartadas — misma disciplina
        // que `omitidas`: un listado que enseña menos de lo que hay jamás
        // es silencioso. Se calla con 0 apartadas (dir sin dotfiles) y con
        // la ocultación apagada. Va DETRÁS de `pruned` en la línea (#107
        // review MINOR-3): ocultar con marcas produce ambos, y el aviso de
        // poda es el que no puede recortarse primero.
        let ocultas = sangrado(norte_frontend::notes::hidden(pane.hidden_count(), lang));
        // Review MAJOR M3: los AVISOS (`omitidas` — listado incompleto,
        // "jamás silencioso" — y `nombres` — el badge de reinterpretación,
        // "el usuario debe saberlo en todo momento") van ANTES que el
        // contador informativo de marcas. La línea no tiene presupuesto de
        // ancho y ratatui recorta la cola: con `marked`/`pruned` primero (25+
        // celdas fácil) un path largo a 80 columnas empujaba el badge de
        // encoding fuera del recorte. Deuda real (#103): un presupuesto de
        // ancho que elipsise `dir_text` para que NINGÚN campo posterior se
        // recorte jamás, en vez de solo reordenar por prioridad.
        // La RUTA cede, y ceden ella sola: todo lo demás de esta línea es un
        // aviso o un contador, y recortar la cola —que es lo que hacía
        // ratatui— se llevaba lo que decía cuántas entradas hay o que el
        // listado está incompleto. Con una ruta larga, lo que se veía del
        // `pos/total` era un dígito suelto.
        //
        // `middle_ellipsis` recorta por el MEDIO: el principio de una ruta
        // dice dónde estás y el final dice qué carpeta es, y perder cualquiera
        // de los dos extremos es perder la mitad útil.
        let tail = format!("{omitidas}{nombres}{pruned}{ocultas}{seq}");
        let ancho = usize::from(area.width);
        let room = ancho
            .saturating_sub(cells(&tail))
            .saturating_sub(cells(mark))
            .saturating_sub(1) // el margen izquierdo
            .saturating_sub(reserva);
        match warn {
            Some(warn) if reserva > 0 && room < RUTA_LEGIBLE => {
                // El de la sesión cierra la línea (`persistent_banner`), así
                // que sus columnas son las últimas del aviso: es lo que el
                // ratón pulsa. Solo si cabe ENTERO: media palabra no es un
                // indicador.
                session = zona_de_sesion(app, area, 1 + cells(&warn));
                format!(" {warn}{seq}")
            }
            Some(warn) => {
                let dir_text = norte_frontend::middle_ellipsis(&dir_text, room);
                let base = format!(" {mark}{dir_text}{tail}");
                let pad = ancho.saturating_sub(cells(&base) + cells(&warn) + 1);
                session = zona_de_sesion(app, area, cells(&base) + pad + cells(&warn));
                format!("{base}{}{warn} ", " ".repeat(pad))
            }
            None => {
                let dir_text = norte_frontend::middle_ellipsis(&dir_text, room);
                format!(" {mark}{dir_text}{tail}")
            }
        }
    };
    Composed {
        text,
        session,
        items: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::draw_status;
    use crate::app::testutil::app_dos_panes;
    use norte_frontend::busy::{Busy, BusyKind, THRESHOLD};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn barra(app: &crate::app::App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(70, 1)).expect("terminal de test");
        terminal
            .draw(|f| draw_status(f, f.area(), app))
            .expect("draw");
        terminal.backend().to_string()
    }

    /// #323: mientras se espera, la barra dice QUÉ se espera y que Esc cancela,
    /// y eso GANA al mensaje anterior.
    ///
    /// El orden es la mitad del arreglo. El mensaje de la operación de antes
    /// describe un estado que ya no es el actual, y el lector está mirando esa
    /// línea justo porque quiere saber si el programa sigue vivo: dejar el
    /// texto viejo encima contesta a otra pregunta.
    #[test]
    fn la_espera_manda_sobre_el_mensaje_anterior() {
        let mut app = app_dos_panes();
        app.message = Some("copiado 1 fichero".to_string());
        assert!(barra(&app).contains("copiado 1 fichero"));

        let mut busy = Busy::new(BusyKind::Connecting, None, Some(0));
        busy.elapsed = THRESHOLD;
        let frame = busy.frame();
        app.busy = Some(busy);
        let linea = barra(&app);
        assert!(linea.contains(frame), "sin spinner: {linea}");
        assert!(
            !linea.contains("copiado 1 fichero"),
            "el mensaje viejo tapa la espera: {linea}"
        );
    }

    /// Los elementos (ADR 0132) van a la DERECHA, en el orden configurado,
    /// y el que se pulsa dice dónde cae y qué corre. Se contrasta contra el
    /// texto pintado.
    #[test]
    fn los_elementos_van_a_la_derecha_y_se_pulsan() {
        let mut app = app_dos_panes();
        app.notices_unread = 3;
        app.chrome.status_items =
            Some(norte_config::StatusItems::parse(&["position", "notices"]).expect("válida"));
        let area = ratatui::layout::Rect::new(0, 0, 70, 1);
        let c = super::compose(&app, area);
        let linea = barra(&app);
        assert!(c.text.trim_end().ends_with("!3"), "{:?}", c.text);
        assert!(c.text.contains('/'), "la posición: {:?}", c.text);
        assert_eq!(super::cells(&c.text), 70, "la línea llena el ancho");
        let (x0, x1, cmd) = c.items[0];
        assert_eq!(cmd, "layout.log");
        let pintado: String = linea
            .chars()
            .skip(1 + usize::from(x0))
            .take(usize::from(x1 - x0) + 1)
            .collect();
        assert_eq!(pintado, "!3", "{linea}");

        // Sin elementos, la línea es la de siempre y no hay nada que pulsar.
        app.chrome.status_items = Some(norte_config::StatusItems::parse::<&str>(&[]).unwrap());
        assert!(super::compose(&app, area).items.is_empty());
    }

    /// ADR 0137: el elemento de un plugin es el valor de su columna para la
    /// entrada bajo el cursor, va a la izquierda de la mitad derecha y no se
    /// pulsa.
    #[test]
    fn el_elemento_de_un_plugin_dice_su_columna_y_no_se_pulsa() {
        let mut app = app_dos_panes();
        app.chrome.status_items =
            Some(norte_config::StatusItems::parse(&["position"]).expect("válida"));
        app.status_plugins = vec![("git".to_owned(), "branch".to_owned())];
        let bajo_el_cursor = app.focused().selected().expect("hay entradas").path.clone();
        let mut valores = std::collections::HashMap::new();
        valores.insert(bajo_el_cursor, "main".to_owned());
        let mut columnas = std::collections::HashMap::new();
        columnas.insert("plugin:git/branch".to_owned(), valores);
        app.focused_mut().set_plugin_columns(columnas);
        let area = ratatui::layout::Rect::new(0, 0, 70, 1);
        let c = super::compose(&app, area);
        let rama = c.text.find("main").expect("la rama se pinta");
        let posicion = c.text.rfind('/').expect("y la posición");
        assert!(rama < posicion, "el del plugin va primero: {:?}", c.text);
        assert!(
            c.items.is_empty(),
            "ninguno de los dos se pulsa: {:?}",
            c.items
        );
    }

    /// Una ventana suelta lleva su indicador en la barra, y la barra sabe
    /// en qué columnas lo pintó: es lo que el ratón pulsa para pedir la
    /// explicación. Se contrasta contra el TEXTO pintado, no contra una
    /// aritmética paralela.
    #[test]
    fn el_indicador_de_sesion_dice_donde_cae() {
        use super::compose;
        use crate::ui::text::cells;
        let mut app = app_dos_panes();
        let area = ratatui::layout::Rect::new(0, 0, 70, 1);
        assert!(
            compose(&app, area).session.is_none(),
            "la dueña no tiene indicador"
        );

        app.session.detached = true;
        let c = compose(&app, area);
        let (linea, span) = (c.text, c.session);
        let badge = app.session_banner().expect("hay indicador");
        let (x0, x1) = span.expect("y la barra sabe dónde");
        let byte = linea.find(&badge).expect("el indicador está en la línea");
        assert_eq!(
            usize::from(x0),
            cells(&linea[..byte]),
            "empieza donde se pinta"
        );
        assert_eq!(
            usize::from(x1),
            cells(&linea[..byte]) + cells(&badge) - 1,
            "y acaba con su última celda"
        );
        assert!(barra(&app).contains(&badge), "y se ve: {}", barra(&app));

        // Con un mensaje delante la línea es otra y no hay nada que pulsar.
        app.message = Some("copiado 1 fichero".to_string());
        assert!(compose(&app, area).session.is_none());
    }

    /// Un aviso caduca a los `notice_seconds` tics (spec 2026-09-10): sale
    /// de la barra, la insignia `!n` cuenta uno más a la derecha y es
    /// pulsable; con `0` no caduca nunca; un mensaje NUEVO reinicia la
    /// cuenta; y abrir el panel de registro pone la insignia a cero.
    #[test]
    fn un_aviso_caduca_y_deja_una_insignia_pulsable() {
        use super::compose;
        let mut app = app_dos_panes();
        let area = ratatui::layout::Rect::new(0, 0, 70, 1);
        app.chrome.notice_seconds = Some(2);
        app.message = Some("copiado 1 fichero".to_string());
        app.tick_notices();
        assert!(app.message.is_some(), "un tic: sigue");
        app.message = Some("otro".to_string());
        app.tick_notices();
        assert!(app.message.is_some(), "un mensaje nuevo reinicia la cuenta");
        app.tick_notices();
        assert!(app.message.is_none(), "dos tics: caducó");
        assert_eq!(app.notices_unread, 1);
        let c = compose(&app, area);
        assert!(
            c.text.ends_with("!1 "),
            "la insignia a la derecha: {:?}",
            c.text
        );
        let zona = |c: &super::Composed| {
            c.items
                .iter()
                .find(|(_, _, cmd)| *cmd == "layout.log")
                .map(|(x0, x1, _)| (*x0, *x1))
        };
        assert_eq!(zona(&c), Some((67, 68)), "pulsable");
        assert!(barra(&app).contains("!1"));

        // Desde el ADR 0132 la insignia es un ELEMENTO de la derecha, y un
        // mensaje en la izquierda ya no la tapa: son mitades distintas.
        app.message = Some("nuevo".to_string());
        assert_eq!(zona(&compose(&app, area)), Some((67, 68)));
        // Con `0`, nada caduca.
        app.chrome.notice_seconds = Some(0);
        for _ in 0..5 {
            app.tick_notices();
        }
        assert!(app.message.is_some());
        // Abrir el registro deja la insignia a cero.
        app.message = None;
        assert_eq!(app.notices_unread, 1);
        app.toggle_log();
        app.tick_notices();
        assert_eq!(app.notices_unread, 0);
    }

    /// Con la sesión suelta la barra sigue diciendo la ruta y el `x/x`, y el
    /// aviso va a la derecha (2026-09-11: sustituía la línea entera y quien
    /// tenía la sesión suelta todo el día perdía el contador). En una barra
    /// estrecha, el aviso solo, como antes.
    #[test]
    fn el_aviso_persistente_no_tapa_la_ruta_ni_el_contador() {
        use super::compose;
        use crate::ui::text::cells;
        let mut app = app_dos_panes();
        app.session.detached = true;
        let badge = app.session_banner().expect("hay indicador");
        let area = ratatui::layout::Rect::new(0, 0, 70, 1);
        let c = compose(&app, area);
        // El contador es ahora el elemento `position` de la derecha (ADR
        // 0132), y el aviso cierra la mitad IZQUIERDA.
        assert!(c.text.contains("1/"), "el contador sigue: {:?}", c.text);
        assert!(c.text.contains(&badge), "el aviso está: {:?}", c.text);
        let (x0, x1) = c.session.expect("pulsable");
        let byte = c.text.find(&badge).expect("está");
        assert_eq!(
            usize::from(x0),
            cells(&c.text[..byte]),
            "la zona empieza donde el indicador"
        );
        assert_eq!(usize::from(x1), cells(&c.text[..byte]) + cells(&badge) - 1);
        // Sin sitio para una ruta legible: el aviso solo.
        let corto =
            ratatui::layout::Rect::new(0, 0, u16::try_from(cells(&badge) + 8).expect("cabe"), 1);
        let c = compose(&app, corto);
        assert!(c.text.contains(&badge), "{:?}", c.text);
        assert!(c.session.is_some(), "los elementos ceden ante el aviso");
    }

    /// REGRESIÓN (revisión de ADR 0132): con elementos que llenan JUSTO su
    /// presupuesto, el margen de la mitad derecha se comía la última celda
    /// del aviso. El aviso tiene que caber entero, con la secuencia
    /// pendiente detrás si la hay.
    #[test]
    fn los_elementos_no_recortan_un_aviso_persistente() {
        use super::compose;
        use crate::ui::text::cells;
        let mut app = app_dos_panes();
        app.session.detached = true;
        let badge = app.session_banner().expect("hay indicador");
        for pendiente in ["", "g"] {
            app.pending = pendiente.to_owned();
            // `1/1` (3 celdas) cabe exacto en lo que deja el aviso.
            for extra in 3..12 {
                let ancho = u16::try_from(cells(&badge) + extra).expect("cabe");
                let c = compose(&app, ratatui::layout::Rect::new(0, 0, ancho, 1));
                assert!(
                    c.text.contains(&badge),
                    "ancho {ancho}, pendiente {pendiente:?}: {:?}",
                    c.text
                );
                assert!(c.session.is_some(), "pulsable a {ancho}");
            }
        }
    }

    /// En un terminal estrecho el indicador se recorta, y un indicador que no
    /// cabe entero no es pulsable: media palabra no es un indicador.
    #[test]
    fn el_indicador_recortado_no_es_pulsable() {
        use super::compose;
        let mut app = app_dos_panes();
        app.session.detached = true;
        let badge = app.session_banner().expect("hay indicador");
        let ancho = crate::ui::text::cells(&badge);
        // Justo lo que ocupa con su margen: cabe.
        let justo = ratatui::layout::Rect::new(0, 0, u16::try_from(ancho + 1).expect("cabe"), 1);
        assert!(
            compose(&app, justo).session.is_some(),
            "cabe entero y se puede pulsar"
        );
        // Una celda menos: ya no.
        let corto = ratatui::layout::Rect::new(0, 0, u16::try_from(ancho).expect("cabe"), 1);
        assert!(
            compose(&app, corto).session.is_none(),
            "recortado, sin zona"
        );
    }

    /// Con un overlay delante no hay zona, aunque la ventana siga suelta: el
    /// visor pinta su propio pie, y sobre la ayuda la barra no se pulsa.
    #[test]
    fn con_un_overlay_delante_no_hay_zona() {
        use super::session_zone;
        let mut app = app_dos_panes();
        app.session.detached = true;
        let area = ratatui::layout::Rect::new(0, 0, 80, 24);
        assert!(session_zone(&app, area).is_some(), "sin overlay sí");
        app.help = Some(crate::app::HelpView::new(norte_i18n::Lang::Es, Vec::new()));
        assert!(
            session_zone(&app, area).is_none(),
            "con la ayuda delante no"
        );
    }

    /// Por debajo del umbral la barra no cambia: un destello en cada `cd`
    /// local es exactamente el ruido que hace que nadie mire el indicador.
    #[test]
    fn antes_del_umbral_la_barra_no_se_entera() {
        let mut app = app_dos_panes();
        app.message = Some("copiado 1 fichero".to_string());
        let mut busy = Busy::new(BusyKind::Connecting, None, Some(0));
        busy.elapsed = THRESHOLD
            .checked_sub(std::time::Duration::from_millis(1))
            .expect("el umbral es mayor que 1 ms");
        let frame = busy.frame();
        app.busy = Some(busy);
        let linea = barra(&app);
        assert!(!linea.contains(frame), "spinner antes de tiempo: {linea}");
        assert!(linea.contains("copiado 1 fichero"), "{linea}");
    }

    /// ADR 0146: con trabajo que ya dura, el item de tareas lleva su barra
    /// detrás, y la zona pulsable la cubre entera: un clic en la barra abre
    /// los procesos como un clic en el texto.
    #[test]
    fn el_item_de_tareas_pinta_su_barra_y_se_pulsa_entero() {
        use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};
        let mut app = app_dos_panes();
        app.chrome.status_items =
            Some(norte_config::StatusItems::parse(&["tasks"]).expect("válida"));
        let p = TaskProgress {
            task_id: TaskId::new(1),
            kind: TaskKind::Copy,
            state: TaskState::Running,
            bytes_done: 50,
            bytes_total: Some(100),
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let t = norte_frontend::task_strip::StripTask {
            progress: &p,
            operand: None,
            bps: None,
        };
        app.strip.update(0, [t]);
        app.strip.update(norte_frontend::task_strip::UMBRAL_MS, [t]);
        app.render_now_ms = Some(norte_frontend::task_strip::UMBRAL_MS);
        let area = ratatui::layout::Rect::new(0, 0, 70, 1);
        let c = super::compose(&app, area);
        assert_eq!(super::cells(&c.text), 70, "la línea llena el ancho");
        let barra = norte_frontend::task_strip::bar_glyphs(Some(50), 0);
        assert!(c.text.contains(&barra), "{:?}", c.text);
        let (x0, x1, cmd) = c.items[0];
        assert_eq!(cmd, "layout.processes");
        let zona: String = c
            .text
            .chars()
            .skip(usize::from(x0))
            .take(usize::from(x1 - x0) + 1)
            .collect();
        assert!(zona.starts_with('⟳') && zona.ends_with('▏'), "{zona:?}");
    }
}
