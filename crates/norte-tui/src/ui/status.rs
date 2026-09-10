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

/// Segmentos `(marked, pruned)` de la status bar sobre las marcas (#103).
/// Extraído de `draw_status` (que ya rozaba `too_many_lines`) — pura
/// composición de texto, sin efecto de render.
pub(crate) fn marks_status_segments(pane: &Pane) -> (String, String) {
    // Las dos frases las REDACTA el crate compartido: la ventana pone las
    // mismas en su cabecera, y dos redacciones del mismo hecho es de donde
    // salió media auditoría de paridad (ADR 0077). Aquí queda el espaciado,
    // que sí es de esta barra.
    let sangrado = |s: String| if s.is_empty() { s } else { format!("  {s}") };
    let lang = norte_i18n::active();
    let pruned = sangrado(norte_frontend::notes::pruned_marks(
        pane.pruned_marks(),
        lang,
    ));
    let marked = sangrado(norte_frontend::notes::marked(
        pane.marks_len(),
        pane.marked_bytes(),
        pane.marked_dirs(),
        lang,
    ));
    (marked, pruned)
}

pub(crate) fn draw_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let c = compose(app, area);
    frame.render_widget(
        Paragraph::new(c.text).style(app.theme.role(Role::StatusBar)),
        area,
    );
}

/// La zona de la insignia de avisos sin leer (spec 2026-09-10): un clic
/// abre el panel de registro, que es donde fueron a parar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoticeZone {
    /// Fila.
    pub row: u16,
    /// Primera columna, inclusive.
    pub x0: u16,
    /// Última columna, inclusive.
    pub x1: u16,
}

/// La zona de la insignia de avisos para el frame `area`, si se pinta.
#[must_use]
pub fn notices_zone(app: &App, area: Rect) -> Option<NoticeZone> {
    let status = status_rect(app, area)?;
    compose(app, status).notices.map(|(x0, x1)| NoticeZone {
        row: status.y,
        x0,
        x1,
    })
}

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
    notices: Option<(u16, u16)>,
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
fn compose(app: &App, area: Rect) -> Composed {
    let mut c = compose_line(app, area);
    // La insignia de avisos sin leer (spec 2026-09-10), a la DERECHA de lo
    // que haya, salvo que lo que haya sea el propio mensaje o una espera:
    // ahí la barra ya está diciendo lo más nuevo. Solo si cabe entera.
    let tapa = app.message.is_some() || app.busy.as_ref().is_some_and(|b| b.visible());
    if app.notices_unread > 0 && !tapa {
        let badge = format!("!{}", app.notices_unread);
        let ancho = cells(&badge);
        let fin = usize::from(area.width);
        let usado = cells(&c.text);
        if usado + 2 + ancho <= fin {
            let pad = fin - usado - ancho - 1;
            c.text = format!("{}{}{badge} ", c.text, " ".repeat(pad));
            let x0 = area
                .x
                .saturating_add(u16::try_from(fin - ancho - 1).unwrap_or(u16::MAX));
            c.notices = Some((
                x0,
                x0.saturating_add(u16::try_from(ancho).unwrap_or(u16::MAX))
                    .saturating_sub(1),
            ));
        }
    }
    c
}

/// La línea de estado sin la insignia, y las columnas del indicador de
/// sesión si va en ella.
fn compose_line(app: &App, area: Rect) -> Composed {
    let mut session = None;
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
    } else if let Some(warn) = app.persistent_banner() {
        // #44: sesión remota degradada a texto plano, y #177: sesión que muta
        // sin quedar registrada en el journal. PERSISTENTES (como
        // `search-status-failed`): sobreviven a las teclas — sin `message`, sin
        // búsqueda viva y sin hook Lua siguen avisando en cada frame.
        // H3d: la frase se COMPONE aquí desde el valor estructurado (una
        // conexión: la nombra; varias: cuántas), en vez de guardarse ya escrita.
        // El de la sesión cierra la línea (`persistent_banner`), así que sus
        // columnas son las últimas del aviso: es lo que el ratón pulsa.
        if let Some(badge) = app.session_banner() {
            let ancho = cells(&badge);
            let fin = 1 + cells(&warn);
            let x0 = area
                .x
                .saturating_add(u16::try_from(fin - ancho).unwrap_or(u16::MAX));
            let x1 = area
                .x
                .saturating_add(u16::try_from(fin - 1).unwrap_or(u16::MAX));
            // Solo si cabe ENTERO: media palabra no es un indicador.
            if x1 < area.x.saturating_add(area.width) {
                session = Some((x0, x1));
            }
        }
        format!(" {warn}{seq}")
    } else {
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
        let tail = format!("{pos_total}{omitidas}{nombres}{pruned}{ocultas}{marked}{seq}");
        let room = usize::from(area.width)
            .saturating_sub(cells(&tail))
            .saturating_sub(cells(mark))
            .saturating_sub(1); // el margen izquierdo
        let dir_text = norte_frontend::middle_ellipsis(&dir_text, room);
        format!(" {mark}{dir_text}{tail}")
    };
    Composed {
        text,
        session,
        notices: None,
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
        let (x0, x1) = c.notices.expect("pulsable");
        assert_eq!((x0, x1), (67, 68));
        assert!(barra(&app).contains("!1"));

        // Con un mensaje delante, la barra dice lo más nuevo y no la insignia.
        app.message = Some("nuevo".to_string());
        assert!(compose(&app, area).notices.is_none());
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
}
