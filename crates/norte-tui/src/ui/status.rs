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
    frame.render_widget(
        Paragraph::new(text).style(app.theme.role(Role::StatusBar)),
        area,
    );
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
