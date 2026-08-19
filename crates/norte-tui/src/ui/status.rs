//! La barra de estado del pane con foco: sus segmentos de marcas y la línea que
//! los junta.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

use super::HOSTILE_BADGE;
use super::text::cells;
use crate::app::{App, Pane};
use norte_i18n::ta;

/// Segmentos `(marked, pruned)` de la status bar sobre las marcas (#103).
/// Extraído de `draw_status` (que ya rozaba `too_many_lines`) — pura
/// composición de texto, sin efecto de render.
pub(crate) fn marks_status_segments(pane: &Pane) -> (String, String) {
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
    let text = if let Some(drag) = crate::mouse::drop_hint(app) {
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
