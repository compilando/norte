//! El ratón de la TUI: captura de la terminal, geometría pintada, hit test
//! y traducción de eventos crossterm a los gestos COMPARTIDOS de
//! [`norte_frontend::mouse`].
//!
//! Aquí no vive ninguna regla de marcado: qué marca un arrastre, cuándo un
//! gesto es una transferencia y cuándo un barrido, y con qué modificadores,
//! lo decide `norte-frontend` para los dos frontends a la vez (regla 7).
//! Este módulo hace las tres cosas que SÍ son de la terminal: pedirle al
//! emulador que reporte el ratón, saber qué celda es qué fila, y aplicar
//! los [`Effect`] resultantes sobre el modelo.
//!
//! # La captura no es gratis
//!
//! Con la captura activa el TERMINAL deja de ver los botones que usa para
//! su propia selección de texto: seleccionar-y-pegar con el ratón deja de
//! funcionar como el usuario lo tiene aprendido. En casi todos los
//! emuladores mantener Mayús mientras se arrastra devuelve la selección
//! nativa, y `[ui] mouse = false` la devuelve del todo. Eso es información
//! de USUARIO, no un comentario: vive en el tema `mouse` de la ayuda y en
//! la descripción del ajuste `ui.mouse`.

use std::io::Write;
use std::time::{Duration, Instant};

#[cfg(windows)]
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use norte_frontend::mouse::{Drag, Effect, Mods, Pending, Press, Spot};

use crate::app::{App, TransferKind};
use crate::ui::HOSTILE_BADGE;

/// Ventana de un doble click. crossterm NO reporta dobles clicks (ningún
/// protocolo de ratón de terminal los tiene): los cuenta esta ventana sobre
/// la MISMA fila del MISMO pane, que es también la regla que evita que dos
/// clicks a filas distintas se lean como uno doble.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Filas que mueve un tacto de rueda. Tres es lo que usan casi todos los
/// terminales y navegadores; una sola fila hace la rueda inútil en un
/// listado largo y una página entera pierde el sitio.
const WHEEL_ROWS: usize = 3;

/// La geometría PINTADA de un pane, en celdas de la terminal.
///
/// La rellena [`crate::ui::pane_geometry`] después de cada frame y la
/// guarda el modelo (#124): el hit test resuelve contra la última pantalla
/// que el usuario vio de verdad, no contra un layout recalculado a mano que
/// puede haber cambiado ya.
///
/// Deliberadamente SIN tipos de ratatui: es estado del modelo, y el modelo
/// no conoce el motor de render.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaneGeometry {
    /// Columna izquierda del bloque (borde incluido).
    pub x: u16,
    /// Fila superior del bloque (borde incluido).
    pub y: u16,
    /// Ancho del bloque, bordes incluidos.
    pub width: u16,
    /// Alto del bloque, bordes incluidos.
    pub height: u16,
    /// Primera fila de LISTADO: `y + 2` (borde superior + cabecera de
    /// columnas). Se guarda calculada, no derivada en el hit test, para que
    /// el día que el pane gane o pierda una fila de cromo haya UN sitio que
    /// cambiar.
    pub first_list_row: u16,
    /// Cuántas filas de listado se pintaron. `0` = el pane no tiene sitio
    /// para ninguna (terminal diminuto): entonces NINGUNA fila resuelve.
    pub list_rows: u16,
    /// Primer índice PINTADO del listado (el scroll). En coordenadas de lo
    /// pintado: bajo un filtro de quick search es una posición dentro del
    /// subconjunto visible, no un índice de `entries`.
    pub offset: usize,
}

impl PaneGeometry {
    /// ¿Cae `(col, row)` dentro del bloque de este pane, bordes incluidos?
    #[must_use]
    pub const fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.x
            && col < self.x.saturating_add(self.width)
            && row >= self.y
            && row < self.y.saturating_add(self.height)
    }

    /// El índice PINTADO bajo `(col, row)`, o `None` si ahí no hay fila de
    /// listado.
    ///
    /// `None` cubre TODO el cromo, y cada caso está aquí a propósito porque
    /// el fallo natural sería saturar hacia una fila real: el borde
    /// superior con su título (la ruta del pane), la cabecera de columnas,
    /// el borde inferior (donde además se pinta el input del quick search),
    /// las dos columnas de los bordes laterales, y el hueco BAJO la última
    /// entrada de un listado corto. Un click en el vacío de un pane a
    /// medio llenar no debe marcar la última entrada.
    #[must_use]
    pub fn painted_row_at(&self, col: u16, row: u16) -> Option<usize> {
        if col == self.x || col.saturating_add(1) == self.x.saturating_add(self.width) {
            return None; // bordes laterales
        }
        let k = row.checked_sub(self.first_list_row)?; // borde superior + cabecera
        if k >= self.list_rows {
            return None; // borde inferior (y cualquier fila más allá)
        }
        Some(self.offset.saturating_add(usize::from(k)))
    }
}

/// Dónde cayó un click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Pane bajo el puntero (0 = izquierda).
    pub pane: usize,
    /// Índice ABSOLUTO en `entries` de la fila pulsada, o `None` si se
    /// pulsó cromo o el vacío bajo el listado. `None` sigue siendo un hit:
    /// la rueda y el foco quieren el pane aunque no haya fila.
    pub index: Option<usize>,
}

/// Todo lo que tiene que seguir siendo verdad para que un gesto en vuelo
/// signifique algo: los índices de cada pane, que los dos panes sigan del
/// lado en que estaban, y que nadie se haya puesto delante.
///
/// Un gesto solo lleva índices ([`Spot`]), y un índice nombra una fila del
/// listado que se pintó. Cuando ese listado se mueve —otro directorio, un
/// refill tras una mutación, una página de un relleno paginado, un
/// re-ordenado— el índice pasa a nombrar otro fichero, y el gesto ha dejado
/// de ser el que el usuario hizo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Vigencia {
    /// [`crate::app::Pane::listing_epoch`] de cada pane.
    epochs: [u64; 2],
    /// [`crate::app::App::swap_seq`]. Las épocas NO cubren un `pane.swap`:
    /// viajan con su pane, así que el intercambio se limita a cruzar los dos
    /// valores y, cuando empatan —lo normal recién arrancado—, la
    /// comparación por lado no ve nada moverse. El gesto, en cambio, guarda
    /// un índice de pane, y tras el cruce ese índice nombra el contenido del
    /// otro lado.
    swap: u64,
    /// Había un overlay/modal delante al pintar. Un modal que se abre a
    /// mitad de un arrastre se lleva el gesto por delante: cuando se cierre,
    /// el usuario ya está a otra cosa.
    overlay: bool,
}

/// Estado de ratón que vive en el modelo: la geometría del último frame, el
/// gesto armado, el instante del último click (para el doble) y la vigencia
/// de todo ello.
#[derive(Debug, Default)]
pub struct MouseState {
    /// `None` = el último frame no pintó panes (visor abierto) o todavía no
    /// hubo frame. Sin geometría no se resuelve NADA: un click contra una
    /// pantalla que no existe es peor que un click ignorado.
    geometry: Option<[PaneGeometry; 2]>,
    /// La máquina de gestos compartida (`norte-frontend`).
    drag: Drag,
    /// `(cuándo, dónde)` del último click izquierdo, para el doble.
    last_click: Option<(Instant, Spot)>,
    /// La [`Vigencia`] del frame anterior, para detectar el cambio.
    vigencia: Vigencia,
    /// Los modificadores del ÚLTIMO evento de ratón, para que [`drop_hint`]
    /// pueda preguntarle a [`Drag::pending`] qué haría soltar AHORA.
    ///
    /// Se recuerdan porque una terminal no reporta el teclado mientras el
    /// botón está pulsado: crossterm trae los modificadores DENTRO de cada
    /// evento de ratón, así que pulsar Mayús sin mover el puntero no llega
    /// hasta la siguiente celda que se cruce. El aviso se actualiza
    /// entonces, no antes — es un límite del protocolo, no una elección, y
    /// por eso el aviso nombra los dos desenlaces («con Mayús, mover») en
    /// vez de fiarlo todo a que el modificador se vea reflejado al instante.
    last_mods: Mods,
}

impl MouseState {
    /// La geometría del último frame.
    #[must_use]
    pub const fn geometry(&self) -> Option<&[PaneGeometry; 2]> {
        self.geometry.as_ref()
    }

    /// Suelta el gesto armado y el click a medio emparejar.
    ///
    /// Las marcas que un barrido ya aplicó SE QUEDAN: soltar el gesto no es
    /// deshacerlo (contrato de [`Drag::cancel`]).
    fn invalidate(&mut self) {
        self.drag.cancel();
        self.last_click = None;
    }
}

/// Cierra el frame: devuelve al modelo la geometría recién pintada (#124) y
/// suelta el gesto en vuelo si ha dejado de significar algo.
///
/// **Este es el ÚNICO sitio donde un gesto caduca**, y va aquí porque el run
/// loop pasa por aquí después de CADA frame, antes de atender ningún evento.
///
/// La alternativa era parchear los sitios que se comen eventos de ratón: el
/// `select!` interno del cd, `on_tick`, `refresh_panes`, el pump del
/// viewer… todos filtran `Event::Key` y tiran los demás, así que un release
/// que caiga ahí no llega nunca. El gesto se queda ARMADO y la siguiente
/// motion continúa un barrido que el usuario terminó hace rato; y un click
/// de antes de un cd se empareja con uno de después en un doble click que
/// entra en un directorio que nadie pidió. Pero esos pumps son cuatro hoy y
/// serán cinco mañana, y el quinto no tiene por qué acordarse. Lo que sí es
/// invariante es que un gesto vive de índices y los índices los mueve el
/// listado: comprobarlo aquí cubre los cuatro, y al quinto gratis.
pub fn after_frame(app: &mut App, geometry: Option<[PaneGeometry; 2]>) {
    let vigencia = Vigencia {
        epochs: [app.panes[0].listing_epoch(), app.panes[1].listing_epoch()],
        swap: app.swap_seq(),
        overlay: overlay_open(app),
    };
    // Sin panes pintados (visor abierto) tampoco hay dónde soltar.
    if vigencia != app.mouse.vigencia || geometry.is_none() {
        app.mouse.invalidate();
    }
    app.mouse.vigencia = vigencia;
    app.mouse.geometry = geometry;
}

/// Qué debe hacer el run loop tras un evento de ratón. Todo lo que se puede
/// hacer sobre el modelo ya está hecho al volver; esto es solo lo que
/// necesita al backend o a la terminal, que este módulo no tiene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum After {
    /// Nada: el evento se resolvió entero aquí.
    #[default]
    Nothing,
    /// Doble click sobre una fila: despacha `nav.enter`, EL MISMO comando
    /// del teclado (jamás un segundo camino que entre en directorios por su
    /// cuenta).
    Enter,
}

/// El índice ABSOLUTO en `entries` de una posición PINTADA del pane.
///
/// Bajo un filtro de quick search lo pintado es el subconjunto visible, así
/// que la posición se traduce por él; sin filtro, lo pintado ES `entries`.
/// Fuera de rango (listado más corto que la ventana, o listado que cambió
/// entre el frame y el click) devuelve `None` en vez de saturar.
fn absolute_index(pane: &crate::app::Pane, painted: usize) -> Option<usize> {
    match pane.quick_visible() {
        Some(vis) => vis.get(painted).copied(),
        None => (painted < pane.entries().len()).then_some(painted),
    }
}

/// Resuelve `(col, row)` contra la geometría del último frame.
///
/// `None` = fuera de los dos panes (panel de tasks, barra de estado) o sin
/// geometría (visor abierto).
#[must_use]
pub fn hit_test(app: &App, col: u16, row: u16) -> Option<Hit> {
    let geometry = app.mouse.geometry()?;
    let (pane, geom) = geometry
        .iter()
        .enumerate()
        .find(|(_, g)| g.contains(col, row))?;
    Some(Hit {
        pane,
        index: geom
            .painted_row_at(col, row)
            .and_then(|painted| absolute_index(&app.panes[pane], painted)),
    })
}

/// Lo que la barra de estado dice de un arrastre EN VUELO: cuántos ítems
/// viajarían, a qué directorio, y si soltar ahora COPIA o MUEVE. `None` = no
/// hay drop pendiente (no hay gesto, se está marcando, o el puntero sigue en
/// casa — soltar ahí es un no-op explícito y prometer una copia que no va a
/// ocurrir es peor que no prometer nada).
///
/// El aviso NO se calcula aparte: sale de [`Drag::pending`], la misma fuente
/// y las mismas reglas que lee [`Drag::release`], y cuenta los ítems con la
/// misma lectura que [`App::open_transfer`] (las marcas, o la fila
/// promovida). Un aviso derivado por su cuenta acabaría prometiendo una
/// copia mientras el drop mueve, o «3 elementos» mientras viaja uno.
///
/// Gemelo de `drop_hint` en la GUI, hasta la clave de Fluent.
#[must_use]
pub fn drop_hint(app: &App) -> Option<String> {
    let Some(Pending::Drop {
        from_pane,
        to_pane,
        move_files,
        promoted,
    }) = app.mouse.drag.pending(app.mouse.last_mods)
    else {
        return None;
    };
    let n = match promoted {
        Some(idx) => usize::from(app.panes[from_pane].entries().get(idx).is_some()),
        None => app.panes[from_pane].marked_paths().len(),
    };
    if n == 0 {
        return None;
    }
    // El dir destino, con el MISMO saneado que la cabecera del pane (regla
    // 1: display siempre lossy, y marcado si es hostil).
    let (to_txt, hostil) = norte_frontend::path_display_with(
        app.panes[to_pane].dir(),
        app.panes[to_pane].name_encoding(),
    );
    let to_txt = if hostil {
        format!("{HOSTILE_BADGE} {to_txt}")
    } else {
        to_txt
    };
    let key = if move_files { "drag-move" } else { "drag-copy" };
    Some(norte_i18n::ta(
        key,
        &[("n", &n.to_string()), ("to", &to_txt)],
    ))
}

/// Los dos modificadores que el marcado entiende. El resto (alt, super) es
/// asunto del keymap, no de estos gestos.
fn mods(m: KeyModifiers) -> Mods {
    Mods::new(
        m.contains(KeyModifiers::CONTROL),
        m.contains(KeyModifiers::SHIFT),
    )
}

/// ¿Hay un overlay comiéndose la interacción? Con uno abierto los panes
/// siguen pintados DEBAJO, así que la geometría sigue siendo válida y un
/// click resolvería una fila perfectamente — y movería el cursor de un
/// listado que el usuario no está mirando, bajo un modal que le está
/// preguntando algo. El teclado ya se enruta así (`modal_wins` y la cadena
/// de overlays del run loop); el ratón hace lo mismo, de una pieza.
fn overlay_open(app: &App) -> bool {
    app.modal.is_some()
        || app.viewer.is_some()
        || app.help.is_some()
        || app.palette.is_some()
        || app.settings.is_some()
        || app.theme_picker.is_some()
        || app.columns_picker.is_some()
        || app.extensions.is_some()
        || app.nav_popup.is_some()
        || app.search_dialog.is_some()
}

/// Un evento de ratón de crossterm, con el reloj real.
pub fn handle(app: &mut App, ev: MouseEvent) -> After {
    handle_at(app, ev, Instant::now())
}

/// Como [`handle`] con el instante inyectado: el doble click es una ventana
/// de tiempo, y un test que dependiera del reloj de la máquina sería un
/// test que falla en CI un martes.
pub fn handle_at(app: &mut App, ev: MouseEvent, now: Instant) -> After {
    if overlay_open(app) {
        return After::Nothing;
    }
    let hit = hit_test(app, ev.column, ev.row);
    let m = mods(ev.modifiers);
    app.mouse.last_mods = m;
    match ev.kind {
        MouseEventKind::ScrollUp => scroll(app, hit, false),
        MouseEventKind::ScrollDown => scroll(app, hit, true),
        MouseEventKind::Down(MouseButton::Left) => return press(app, hit, m, now),
        MouseEventKind::Drag(MouseButton::Left) => {
            // Una motion fuera de toda fila NO se reporta: pasar por encima
            // de la cabecera a mitad de un barrido no puede cancelarlo (lo
            // dice el contrato de `Drag::motion`).
            if let Some(spot) = spot(hit) {
                let fx = app.mouse.drag.motion(spot);
                apply(app, &fx);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let fx = app.mouse.drag.release(spot(hit), m);
            apply(app, &fx);
            for pane in &mut app.panes {
                pane.end_sweep();
            }
        }
        // Botón derecho: NADA todavía. El menú contextual es la tarea 4 del
        // plan; inventarle aquí un segundo menú sería garantizar que los dos
        // frontends acaben con menús distintos.
        _ => {}
    }
    After::Nothing
}

/// El `Spot` de un hit que cayó sobre una fila de verdad.
fn spot(hit: Option<Hit>) -> Option<Spot> {
    let hit = hit?;
    Some(Spot::new(hit.pane, hit.index?))
}

/// Rueda: desplaza el listado BAJO EL PUNTERO, tenga el foco o no — mirar
/// una cosa y rodar sobre otra es el gesto normal con dos paneles, y robarle
/// el foco al pane activo por pasar el ratón por encima sería peor que no
/// desplazar nada.
///
/// «Desplazar» aquí es mover el cursor de ese pane: el TUI no guarda scroll
/// independiente (ver `ui::list_offset`), la ventana pintada sale del
/// cursor. Con un filtro de quick search activo mueve la selección DEL
/// FILTRO, que es lo que está pintado.
fn scroll(app: &mut App, hit: Option<Hit>, down: bool) {
    let Some(hit) = hit else { return };
    let pane = &mut app.panes[hit.pane];
    if pane.quick().is_some() {
        for _ in 0..WHEEL_ROWS {
            if down {
                pane.quick_down();
            } else {
                pane.quick_up();
            }
        }
    } else if down {
        pane.move_down(WHEEL_ROWS);
    } else {
        pane.move_up(WHEEL_ROWS);
    }
}

/// Botón izquierdo abajo.
fn press(app: &mut App, hit: Option<Hit>, m: Mods, now: Instant) -> After {
    let Some(hit) = hit else {
        // Fuera de los panes (panel de tasks, barra de estado): el gesto
        // armado muere; nada de arrastrar desde ahí.
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        return After::Nothing;
    };
    let Some(index) = hit.index else {
        // Cromo del pane (bordes, cabecera, hueco bajo la última entrada):
        // enfoca ese pane y ya. Sigue siendo una acción útil —el título con
        // la ruta es un blanco grande— y no toca ni cursor ni marcas.
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        app.set_focus(hit.pane);
        return After::Nothing;
    };
    let at = Spot::new(hit.pane, index);
    // Doble click ANTES de la máquina de gestos: entrar en un directorio no
    // es un gesto de marcado, y con ctrl/shift pulsados lo que el usuario
    // pide es marcar, no navegar.
    if m == Mods::NONE
        && app
            .mouse
            .last_click
            .is_some_and(|(when, prev)| prev == at && now.duration_since(when) <= DOUBLE_CLICK)
    {
        app.mouse.last_click = None;
        app.mouse.drag.cancel();
        app.set_focus(hit.pane);
        app.panes[hit.pane].set_cursor(index);
        return After::Enter;
    }
    // Solo un click LIMPIO puede ser la primera mitad de un doble. Un
    // ctrl+click es un gesto discreto y completo; leerlo como primera mitad
    // hace que marcar una fila y volver a pulsarla enseguida —para
    // arrastrarla, que es justo lo que se hace después de marcar— entre en
    // el directorio en vez de arrancar el arrastre.
    app.mouse.last_click = (m == Mods::NONE).then_some((now, at));
    let marked = app.panes[hit.pane]
        .entries()
        .get(index)
        .is_some_and(|e| app.panes[hit.pane].is_marked(e));
    // El ancla de un shift+click es lo que el usuario VE resaltado, no el
    // cursor real: bajo un quick search en modo filtro el resaltado sale de
    // la selección del filtro y el cursor real puede estar en cualquier
    // parte del listado completo, así que tomarlo a él como ancla marca un
    // rango que empieza en una fila que nadie está mirando.
    let cursor = painted_anchor(&app.panes[hit.pane]);
    let fx = app.mouse.drag.press(Press {
        at,
        marked,
        cursor,
        mods: m,
    });
    apply(app, &fx);
    // Un click LIMPIO cierra el quick search del pane pulsado, y solo él.
    //
    // El orden importa y la excepción también. Con el filtro puesto se
    // pinta un SUBCONJUNTO: el resaltado sale de la selección del filtro,
    // así que mover el cursor real no movería nada visible y la siguiente
    // operación actuaría sobre la fila del filtro y no sobre la pulsada.
    // Cerrarlo arregla eso — el índice es ABSOLUTO y sobrevive a que
    // vuelva el listado entero.
    //
    // Pero cerrarlo ANTES de marcar sería mucho peor que no cerrarlo:
    // `mark_range`/`set_mark`/`apply_sweep` consultan el filtro para no
    // alcanzar lo que esconde (ver su rustdoc), y sin filtro un
    // shift+click marca TODOS los índices intermedios — los ocultos
    // incluidos — que es justo el ensanchamiento silencioso de la
    // siguiente copia o borrado que esos guards existen para impedir. Por
    // eso va DESPUÉS de `apply`, y por eso solo para el gesto que no marca
    // nada: la pulsación limpia arma el barrido pero no marca (contrato de
    // `Drag::press`), y para cuando llegue la primera motion el listado ya
    // se habrá repintado entero.
    if m == Mods::NONE {
        app.panes[hit.pane].quick_cancel();
    }
    After::Nothing
}

/// El índice ABSOLUTO de la fila RESALTADA de un pane: la selección del
/// quick search cuando filtra (que es lo que se pinta,
/// `ui::painted_len_and_selection`), el cursor real si no.
fn painted_anchor(pane: &crate::app::Pane) -> usize {
    pane.quick()
        .and_then(crate::nav::QuickSearch::selected_entry_index)
        .unwrap_or_else(|| pane.cursor())
}

/// Aplica los efectos que devuelve la máquina compartida. Cada uno mapea
/// sobre UNA operación que ya existía en `PaneState`: este módulo no
/// inventa ninguna.
fn apply(app: &mut App, effects: &[Effect]) {
    for effect in effects {
        match *effect {
            // Jamás toca el quick search: marcar con el filtro puesto es
            // lo que hace que el marcado no alcance lo que el filtro
            // esconde. Quien lo cierra es `press`, y solo para el click
            // limpio, DESPUÉS de aplicar los efectos (ver su comentario).
            Effect::MoveCursor { pane, index } => {
                app.set_focus(pane);
                app.panes[pane].set_cursor(index);
            }
            Effect::SetMark {
                pane,
                index,
                marked,
            } => app.panes[pane].set_mark(index, marked),
            Effect::MarkRange { pane, from, to } => {
                app.panes[pane].mark_range(from, to);
            }
            Effect::BeginSweep { pane } => app.panes[pane].begin_sweep(),
            Effect::SweepRange { pane, from, to } => {
                app.panes[pane].apply_sweep(from, to);
            }
            // El barrido cruzó al otro panel y la máquina lo promovió a
            // transferencia: devuelve lo que llevara marcado. Una promoción
            // cambia lo que el gesto HACE, no lo que está seleccionado.
            Effect::RevertSweep { pane } => app.panes[pane].revert_sweep(),
            // El drop. Abre EXACTAMENTE el mismo modal que la tecla de
            // copiar o mover (`App::open_transfer`, fuente única): misma
            // confirmación, mismo diálogo de colisión, misma entrada de
            // journal, mismo undo, misma puerta de policy. Un drop es una
            // mutación y no tiene un camino más silencioso que las demás.
            //
            // No hace falta guard de overlay: `handle_at` ya retorna antes
            // de tocar nada si hay uno delante, y `after_frame` caduca el
            // gesto en cuanto aparece.
            Effect::Transfer {
                from_pane,
                to_pane,
                move_files,
                promoted,
            } => {
                let kind = if move_files {
                    TransferKind::Move
                } else {
                    TransferKind::Copy
                };
                // El lote se consume del pane con FOCO (`consume_marks` tras
                // enviar), y el origen de un drop es el pane donde bajó el
                // botón. El foco YA está ahí —la pulsación lo puso— pero
                // dejarlo dicho convierte un invariante accidental en uno
                // escrito: si algún día una motion sobre el otro panel
                // moviera el foco, las marcas se consumirían del pane
                // equivocado en silencio.
                app.set_focus(from_pane);
                app.open_transfer(kind, from_pane, to_pane, promoted);
            }
        }
    }
}

/// La captura de ratón de la terminal, con su estado.
///
/// Un tipo y no un `bool` suelto porque las secuencias de activar y
/// desactivar tienen que ir emparejadas con lo que la terminal cree: pedir
/// dos veces la activación es inofensivo, pero DEJARLA puesta al salir (o
/// al ceder la terminal a otro programa) deja al usuario con un emulador
/// que escupe basura de escape en cuanto mueve el ratón.
#[derive(Debug, Default)]
pub struct Capture {
    active: bool,
}

impl Capture {
    /// Captura apagada (el estado de una terminal recién tomada).
    #[must_use]
    pub const fn new() -> Self {
        Self { active: false }
    }

    /// ¿Está pedida ahora mismo?
    #[must_use]
    pub const fn active(&self) -> bool {
        self.active
    }

    /// Pide (o retira) la captura si hace falta. Idempotente: es lo que deja
    /// que el hot-reload de `[ui] mouse` llame a esto en cada recarga sin
    /// mandarle a la terminal secuencias que no cambian nada.
    ///
    /// # Errors
    /// La de escribir en `out`.
    pub fn set(&mut self, want: bool, out: &mut impl Write) -> std::io::Result<()> {
        if want == self.active {
            return Ok(());
        }
        write_capture(want, out)?;
        self.active = want;
        Ok(())
    }
}

/// Los modos de ratón que se piden, y NO se usa
/// `crossterm::event::EnableMouseCapture` para pedirlos.
///
/// Ese comando añade `?1003h` (*any-event tracking*): la terminal reporta un
/// evento por CADA celda que cruza el puntero, con todos los botones
/// sueltos. Este módulo tira esos eventos ([`handle_at`], brazo `_`), pero
/// para entonces ya han despertado el run loop, que repinta el frame entero
/// en cada vuelta — y el frame cuesta lo que cuesta el listado (`draw_pane`
/// construye un `ListItem` por entrada, no por fila visible). Pasear el
/// ratón por encima de la ventana, sin pulsar nada, se convierte en cientos
/// de repintados: medido, ~1 ms de frame con 100 entradas y ~38 ms con
/// 20 000. Y lo pagaría también quien jamás toca el ratón.
///
/// Se piden entonces solo los tres modos que este módulo CONSUME: normal
/// (`?1000`, pulsar y soltar), button-event (`?1002`, movimiento SOLO con un
/// botón pulsado — de ahí salen los `Drag`) y SGR (`?1006`, coordenadas más
/// allá de la columna 223; sin él una terminal ancha reporta basura). Se
/// deja fuera `?1015` (modo rxvt) porque `?1006` lo sustituye y crossterm
/// entiende los dos.
#[cfg(not(windows))]
const CAPTURE_ON: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";

/// Los mismos modos, retirados en orden inverso (ver [`CAPTURE_ON`]).
#[cfg(not(windows))]
const CAPTURE_OFF: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";

/// Escribe la petición (o la retirada) de captura.
///
/// En Windows sigue yendo por crossterm: allí `EnableMouseCapture` no manda
/// ANSI NUNCA (su `is_ansi_code_supported` devuelve `false` siempre), sino
/// una llamada a la consola — escribir escapes a mano sería un
/// no-op silencioso en una consola legacy.
fn write_capture(want: bool, out: &mut impl Write) -> std::io::Result<()> {
    #[cfg(not(windows))]
    {
        out.write_all(if want { CAPTURE_ON } else { CAPTURE_OFF }.as_bytes())?;
        out.flush()
    }
    #[cfg(windows)]
    {
        if want {
            crossterm::execute!(out, EnableMouseCapture)
        } else {
            crossterm::execute!(out, DisableMouseCapture)
        }
    }
}

/// Suelta la captura antes de ceder la terminal a un programa externo
/// (`run_opener`), y devuelve si estaba puesta para poder restituirla.
///
/// Sin esto el programa lanzado hereda una terminal en modo ratón que él no
/// pidió: `less` o un editor recibirían las secuencias de cada movimiento
/// como si fueran teclas, y al salir el usuario tendría un terminal que ya
/// nadie está escuchando.
///
/// # Errors
/// La de escribir en `out`.
pub fn release_for_suspend(cap: &mut Capture, out: &mut impl Write) -> std::io::Result<bool> {
    let was = cap.active();
    cap.set(false, out)?;
    Ok(was)
}

/// Restituye la captura al volver del programa externo, si la había.
///
/// # Errors
/// La de escribir en `out`.
pub fn restore_after_suspend(
    cap: &mut Capture,
    was: bool,
    out: &mut impl Write,
) -> std::io::Result<()> {
    cap.set(was, out)
}
