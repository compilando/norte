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

use crate::app::Trail;
use crate::dispatch::dispatch;
use crate::keymap::Command;
use crate::navigate::{apply_cd, cd_in};
use crate::refresh::reap_search_run;
use crate::screens::drain_places_drives;
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

/// Un borde arrastrable entre dos huecos del reparto.
///
/// Lo lleva el hueco de la IZQUIERDA (o el de ARRIBA), que es el que
/// `Node::drag_border` sabe nombrar: el borde es «el suyo con el siguiente».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeBorder {
    /// El hueco de la izquierda o de arriba.
    pub slot: norte_frontend::layout::SlotId,
    /// En qué dirección reparte el `Split` que los contiene.
    pub dir: norte_frontend::layout::Dir,
    /// La columna (o fila) del borde.
    pub linea: u16,
    /// Desde dónde hasta dónde llega el borde, en el otro eje.
    pub desde: u16,
    /// Fin (exclusivo) del tramo del borde.
    pub hasta: u16,
    /// Dónde empieza la PAREJA en el eje del reparto.
    pub inicio: u16,
    /// Cuánto ocupan los dos juntos. Es lo que convierte una columna del
    /// puntero en una fracción.
    pub largo: u16,
}

impl ResizeBorder {
    /// ¿Cae `(col, row)` sobre este borde?
    ///
    /// El borde son DOS columnas y no una: en el TUI cada hueco pinta su
    /// propio marco, así que entre dos vecinos hay la derecha de uno y la
    /// izquierda del otro. Agarrar solo una de las dos deja media línea
    /// muerta, y la que se muere es la que el ojo ve primero.
    #[must_use]
    pub const fn hit(&self, col: u16, row: u16) -> bool {
        let (eje, otro) = match self.dir {
            norte_frontend::layout::Dir::Horizontal => (col, row),
            norte_frontend::layout::Dir::Vertical => (row, col),
        };
        (eje + 1 == self.linea || eje == self.linea) && otro >= self.desde && otro < self.hasta
    }
}

/// Un hueco del reparto y el rectángulo que ocupó, en celdas.
///
/// Sirve para una sola pregunta —¿qué panel hay bajo el puntero?— y por eso
/// no guarda ni el kind ni quién tomaría el teclado: eso lo contesta
/// [`App::focus_slot`] con el registro que los dos frontends comparten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelSlot {
    /// El hueco.
    pub slot: norte_frontend::layout::SlotId,
    /// Columna izquierda, borde incluido.
    pub x: u16,
    /// Fila superior, borde incluido.
    pub y: u16,
    /// Ancho, bordes incluidos.
    pub width: u16,
    /// Alto, bordes incluidos.
    pub height: u16,
}

impl PanelSlot {
    /// ¿Cae `(col, row)` dentro de este hueco?
    #[must_use]
    pub const fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.x
            && col < self.x.saturating_add(self.width)
            && row >= self.y
            && row < self.y.saturating_add(self.height)
    }
}

/// La geometría PINTADA de un pane, en celdas de la terminal.
///
/// La rellena [`crate::ui::pane_geometry`] después de cada frame y la guarda
/// el modelo (#124): el hit test resuelve contra la última pantalla que el
/// usuario vio de verdad, no contra un layout recalculado a mano que puede
/// haber cambiado ya.
///
/// Deliberadamente SIN tipos de ratatui: es estado del modelo, y el modelo no
/// conoce el motor de render.
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Validity {
    /// [`crate::app::Pane::listing_epoch`] de cada pane VISIBLE, en orden.
    ///
    /// Longitud variable desde P6: con splits hay más de dos, y un vector que
    /// CAMBIA DE LONGITUD también invalida el gesto — que es lo correcto,
    /// porque abrir o cerrar un panel mueve todo lo demás de sitio.
    epochs: Vec<u64>,
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
    geometry: Option<Vec<PaneGeometry>>,
    /// Las zonas pulsables de la barra de menús del último frame.
    menu_zones: Vec<crate::ui::MenuZone>,
    panel_zones: Vec<crate::ui::PanelZone>,
    /// Las celdas pulsables de la barra de teclas del último frame (spec
    /// 2026-09-10). Vacío = barra apagada, o sin fila donde pintarla.
    key_zones: Vec<crate::ui::KeyZone>,
    /// Los botones del modal activo del último frame (spec 2026-09-10).
    /// Vacío = sin modal, o su línea de teclas pintada como pista.
    modal_zones: Vec<crate::ui::ModalZone>,
    /// Las zonas pulsables de las barras de pestañas del último frame.
    ///
    /// Vacío = ningún panel tiene pestañas, que es el caso de siempre.
    tab_zones: Vec<crate::ui::TabZone>,
    /// Las filas pulsables del sidebar de sitios del último frame (#226).
    /// Vacío = sidebar cerrado, o sin sitio donde pintarlo.
    places_zones: Vec<crate::ui::PlaceZone>,
    /// Las filas pulsables del árbol del último frame, por lo mismo.
    /// Vacío = árbol cerrado, o sin sitio donde pintarlo.
    tree_zones: Vec<crate::ui::TreeZone>,
    /// Las filas y los botones del gestor de extensiones del último frame.
    /// Vacío = gestor cerrado.
    extension_zones: Vec<crate::ui::ExtensionZone>,
    /// La lateral, el cuerpo y las páginas pulsables de la ayuda del último
    /// frame. `None` = ayuda cerrada.
    help_zones: Option<crate::ui::HelpZones>,
    /// El indicador de sesión suelta de la barra de estado del último frame.
    /// `None` = la ventana es la dueña, o la barra estaba diciendo otra cosa.
    session_zone: Option<crate::ui::SessionZone>,
    /// Los elementos pulsables de la barra de estado del último frame
    /// (ADR 0132).
    status_item_zones: Vec<crate::ui::StatusItemZone>,
    /// Los bordes arrastrables del último frame.
    borders: Vec<ResizeBorder>,
    /// Los huecos que se colocaron en el último frame, para saber qué panel
    /// hay bajo un click.
    slots: Vec<PanelSlot>,
    /// El borde que se está arrastrando AHORA, si hay alguno.
    ///
    /// Se congela al agarrarlo y no se vuelve a buscar mientras dure el
    /// gesto: el reparto cambia bajo el puntero en cada movimiento —para eso
    /// es un arrastre— y volver a preguntar «qué borde hay aquí» acabaría
    /// agarrando el de al lado en cuanto uno pasara por encima del otro.
    resizing: Option<ResizeBorder>,
    /// La columna cuyo ancho se está arrastrando AHORA, congelada al
    /// agarrarla por lo mismo que `resizing`.
    columna: Option<ColumnDrag>,
    /// Lo que dejó el último arrastre de columna al soltar —`(id, celdas)`—,
    /// para que el run loop lo guarde. [`After`] es `Copy` y no puede
    /// llevarlo dentro.
    ancho_soltado: Option<(String, u16)>,
    /// La máquina de gestos compartida (`norte-frontend`).
    drag: Drag,
    /// `(cuándo, dónde)` del último click izquierdo, para el doble.
    last_click: Option<(Instant, Spot)>,
    /// La [`Validity`] del frame anterior, para detectar el cambio.
    validity: Validity,
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
    /// La geometría del último frame, un `PaneGeometry` por panel visible.
    #[must_use]
    pub fn geometry(&self) -> Option<&[PaneGeometry]> {
        self.geometry.as_deref()
    }

    /// El ancho que dejó el último arrastre de columna, `(id, celdas)`, para
    /// guardarlo. Se consume al leerlo: un ancho se escribe una vez.
    pub fn take_column_width(&mut self) -> Option<(String, u16)> {
        self.ancho_soltado.take()
    }

    /// El rectángulo con el que se pintó el hueco `id` en el último frame, si
    /// se pintó.
    ///
    /// No es solo del ratón: lo pregunta también quien va a PARTIR un hueco,
    /// que necesita saber si lo que hay ahí da para dos. El tamaño de verdad
    /// solo lo sabe el frame —el reparto depende del terminal, del cromo y de
    /// los pesos—, y este es el sitio donde el frame lo dejó dicho.
    #[must_use]
    pub fn slot_rect(
        &self,
        id: norte_frontend::layout::SlotId,
    ) -> Option<norte_frontend::layout::Rect> {
        self.slots
            .iter()
            .find(|s| s.slot == id)
            .map(|s| norte_frontend::layout::Rect {
                x: s.x,
                y: s.y,
                width: s.width,
                height: s.height,
            })
    }

    /// Suelta el gesto armado y el click a medio emparejar.
    ///
    /// Las marcas que un barrido ya aplicó SE QUEDAN: soltar el gesto no es
    /// deshacerlo (contrato de [`Drag::cancel`]).
    fn invalidate(&mut self) {
        self.drag.cancel();
        self.last_click = None;
        // El arrastre de COLUMNA no se suelta aquí: sus bordes no dependen de
        // qué filas hay, y un listado que se rellena por páginas o un refresco
        // del vigilante lo cortaban a medias — el ancho se veía en pantalla y
        // no se guardaba nunca.
    }
}

/// Todo lo PULSABLE que el frame recién pintado dejó en la pantalla.
///
/// Una struct y no seis argumentos sueltos: cinco de los seis campos son un
/// `Vec` y una llamada que los cruzara compilaría — el ratón resolvería las
/// pestañas contra las filas del sidebar sin decir nada. Es la misma razón por
/// la que [`Press`] es una struct.
#[derive(Debug, Default)]
pub struct FrameZones {
    /// Las zonas de las barras de pestañas.
    pub tabs: Vec<crate::ui::TabZone>,
    /// Las zonas de la barra de menús.
    pub menus: Vec<crate::ui::MenuZone>,
    /// Las casillas de la barra de paneles (#324).
    pub panels: Vec<crate::ui::PanelZone>,
    /// Las celdas de la barra de teclas (spec 2026-09-10).
    pub keys: Vec<crate::ui::KeyZone>,
    /// Los botones del modal activo (spec 2026-09-10).
    pub modal: Vec<crate::ui::ModalZone>,
    /// Las filas del sidebar de sitios (#226).
    pub places: Vec<crate::ui::PlaceZone>,
    /// Las filas del árbol (#136).
    pub tree: Vec<crate::ui::TreeZone>,
    /// Las filas y los botones del gestor de extensiones, si está abierto.
    pub extensions: Vec<crate::ui::ExtensionZone>,
    /// Las zonas de la ayuda, si está abierta.
    pub help: Option<crate::ui::HelpZones>,
    /// El indicador de sesión suelta de la barra de estado, si se pintó.
    pub session: Option<crate::ui::SessionZone>,
    /// Los elementos pulsables de la barra de estado (ADR 0132).
    pub status_items: Vec<crate::ui::StatusItemZone>,
    /// Los bordes arrastrables.
    pub borders: Vec<ResizeBorder>,
    /// Los huecos colocados, para saber qué panel hay bajo un click.
    pub slots: Vec<PanelSlot>,
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
pub fn after_frame(app: &mut App, geometry: Option<Vec<PaneGeometry>>, zones: FrameZones) {
    let FrameZones {
        tabs: tab_zones,
        menus: menu_zones,
        panels: panel_zones,
        keys: key_zones,
        modal: modal_zones,
        places: places_zones,
        tree: tree_zones,
        extensions: extension_zones,
        help: help_zones,
        session: session_zone,
        status_items: status_item_zones,
        borders,
        slots,
    } = zones;
    let validity = Validity {
        epochs: app
            .panes
            .iter()
            .map(crate::app::Pane::listing_epoch)
            .collect(),
        swap: app.swap_seq(),
        overlay: overlay_open(app),
    };
    // Sin panes pintados (visor abierto) tampoco hay dónde soltar.
    if validity != app.mouse.validity || geometry.is_none() {
        app.mouse.invalidate();
    }
    app.mouse.validity = validity;
    app.mouse.geometry = geometry;
    app.mouse.tab_zones = tab_zones;
    app.mouse.menu_zones = menu_zones;
    app.mouse.panel_zones = panel_zones;
    app.mouse.key_zones = key_zones;
    app.mouse.modal_zones = modal_zones;
    app.mouse.places_zones = places_zones;
    app.mouse.tree_zones = tree_zones;
    app.mouse.extension_zones = extension_zones;
    app.mouse.help_zones = help_zones;
    app.mouse.session_zone = session_zone;
    app.mouse.status_item_zones = status_item_zones;
    app.mouse.borders = borders;
    app.mouse.slots = slots;
}

/// Los comandos de los botones del gestor de extensiones que el ÚLTIMO
/// frame pintó, en el orden en que se pintaron.
///
/// El anillo de `tab` recorre ESTA lista, la misma que resuelve un clic, y
/// no la que `extension_buttons` construiría: la ficha no se pinta con la
/// caja estrecha, y un botón que no cupo por el ancho tampoco está. Sacar
/// las paradas del teclado de lo PINTADO es la misma regla que hizo
/// pulsable el gestor, y la que impide que `tab` mueva un foco a un sitio
/// donde no hay nada.
#[must_use]
pub fn painted_extension_buttons(app: &App) -> Vec<&'static str> {
    app.mouse
        .extension_zones
        .iter()
        .filter_map(|z| match z.hit {
            crate::ui::ExtensionHit::Button(cmd) => Some(cmd),
            crate::ui::ExtensionHit::Row(_) => None,
        })
        .collect()
}

/// El comando del elemento de la barra de estado bajo `ev` en el último
/// frame (ADR 0132), si hay uno pulsable ahí.
fn elemento_de_estado_en(app: &App, ev: MouseEvent) -> Option<&'static str> {
    app.mouse
        .status_item_zones
        .iter()
        .find(|z| z.row == ev.row && ev.column >= z.x0 && ev.column <= z.x1)
        .map(|z| z.command)
}

/// Si `ev` es el botón izquierdo cayendo sobre el indicador de sesión suelta
/// del último frame.
fn pulsa_indicador_de_sesion(app: &App, ev: MouseEvent) -> bool {
    matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        && app
            .mouse
            .session_zone
            .is_some_and(|z| z.row == ev.row && ev.column >= z.x0 && ev.column <= z.x1)
}

/// La página de la ayuda que explica el indicador de sesión suelta: la de
/// perfiles y sesión, que es donde se cuenta dónde estaba cada panel. Vivió
/// en `panes` hasta que esa página se partió.
///
/// Un id del corpus y no un contexto: el indicador no es una pantalla en la
/// que el lector esté, es un hecho sobre esta ventana, y su página es fija.
/// El test del ratón ata el id a una página real en ambos idiomas, y a que
/// esa página HABLE de la sesión: que exista no bastaba —al partir `panes`,
/// la página seguía existiendo sin una línea sobre la sesión—.
pub const SESSION_HELP_TOPIC: &str = "profiles";

/// Qué debe hacer el run loop tras un evento de ratón. Todo lo que se puede
/// hacer sobre el modelo ya está hecho al volver; esto es solo lo que
/// necesita al backend o a la terminal, que este módulo no tiene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum After {
    /// Nada: el evento se resolvió entero aquí.
    #[default]
    Nothing,
    /// Se pulsó un botón de la barra de paneles (#324): el run loop despacha
    /// `App::pending_panel_command` por el MISMO camino que su atajo. Igual
    /// que el menú, y por lo mismo: despachar es asíncrono y este módulo no
    /// tiene el backend.
    PanelBar,
    /// Se pulsó un elemento del menú: el run loop debe ejecutarlo, por el
    /// mismo camino que `Enter`. Este módulo no puede: despachar es asíncrono
    /// y necesita el backend.
    MenuAccept,
    /// Doble click sobre una fila: despacha `nav.enter`, EL MISMO comando
    /// del teclado (jamás un segundo camino que entre en directorios por su
    /// cuenta).
    Enter,
    /// Se plegó o desplegó una sección del sidebar de sitios (#226):
    /// desplegar las unidades es el momento de volver a pedirlas, y es el
    /// MISMO camino que toma la tecla.
    PlacesFolded,
    /// Se activó una fila del sidebar: hay que llevar el listado a donde
    /// diga `App::places_activate`, por el flujo de `cd` de siempre.
    PlacesActivate,
    /// Se activó una rama del árbol (#136): mismo trato que la fila del
    /// sidebar, y el destino lo dice `App::tree_activate`.
    TreeActivate,
    /// Se pulsó el indicador de sesión suelta de la barra de estado: el run
    /// loop abre la ayuda en la página que lo explica. Aquí no se puede: la
    /// ayuda se abre con la hoja de teclas y el idioma, que son del run loop.
    SessionHelp,
    /// Se pulsó un botón de la ficha del gestor de extensiones, o la fila
    /// ya elegida: el run loop despacha este comando por el MISMO camino
    /// que su tecla (`on_extensions_click`). Aquí no se puede: encender,
    /// aprobar o desinstalar hablan con el backend.
    Extension(&'static str),
    /// Se pulsó una celda de la barra de teclas o un botón de un modal
    /// (spec 2026-09-10): la tecla queda en `App::pending_key` y el run loop
    /// la despacha por `on_key`, que es el ÚNICO camino con los tres
    /// resolvers a mano. Un clic ahí ES pulsar la tecla; no hay un segundo
    /// despacho que pueda divergir.
    SynthKey,
    /// Se soltó el borde de una columna tras arrastrarlo: el ancho ya está
    /// aplicado y pintado, y el run loop lo guarda con lo que dejó en
    /// [`MouseState::take_column_width`]. Aquí no se puede: escribir
    /// `norte.toml` es trabajo de disco y va fuera del bucle (regla 2).
    ColumnWidth,
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
    let (to_txt, hostile) = norte_frontend::path_display_with(
        app.panes[to_pane].dir(),
        app.panes[to_pane].name_encoding(),
    );
    let to_txt = if hostile {
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
pub(crate) fn overlay_open(app: &App) -> bool {
    app.modal.is_some()
        || app.viewer.is_some()
        || app.help.is_some()
        || app.palette.is_some()
        || app.wizard.is_some()
        || app.settings.is_some()
        // K3c: el editor de atajos. Hoy está siempre detrás de `settings`, que
        // ya está en esta lista, pero eso es una propiedad de CÓMO se abre y no
        // del tipo — y la GUI (c4) lo abrirá por su cuenta.
        || app.shortcuts.is_some()
        || app.theme_picker.is_some()
        || app.columns_picker.is_some()
        || app.extensions.is_some()
        || app.nav_popup.is_some()
        || app.search_dialog.is_some()
        // El panel de diferencias SUSTITUYE a los dos panes en pantalla, así
        // que un click ahí caía sobre un listado que ya no se ve — y un doble
        // click hacía un `cd` de verdad en un pane invisible, dejando el
        // panel abierto sobre unas raíces que ya no describen a nadie
        // (review MAJOR-1). `keyboard_owner` ya lo declara dueño del teclado;
        // esto es la otra mitad de la misma pieza.
        || app.compare.is_some()
}

/// Un click con la barra de menús abierta.
///
/// Fuera de toda zona la CIERRA: es lo que hace cualquier menú, y dejarla
/// abierta tras pulsar en otra parte convierte un click de más en un menú
/// pegado a la pantalla.
///
/// Pulsar un elemento NO lo ejecuta aquí: solo lo resalta y devuelve
/// [`After::MenuAccept`], porque ejecutar un comando es asíncrono y este módulo
/// no tiene el backend. El run loop lo remata por el mismo camino que `Enter`.
fn menu_click(app: &mut App, col: u16, row: u16) -> After {
    let zone = app
        .mouse
        .menu_zones
        .iter()
        .find(|z| z.row == row && col >= z.x0 && col <= z.x1)
        .copied();
    match zone.map(|z| z.hit) {
        Some(crate::ui::MenuHit::Title(i)) => {
            // Con el menú CERRADO esto lo abre: es el clic que hace usable la
            // barra fijada. Antes solo movía el menú ya abierto de un título a
            // otro, así que la barra se veía y no se podía pulsar.
            if let Some(m) = &mut app.menu {
                m.open(i);
            } else {
                let mut m = norte_frontend::menu::MenuState::new();
                m.open(i);
                app.menu = Some(m);
            }
            After::Nothing
        }
        Some(crate::ui::MenuHit::Item(i)) => {
            if let Some(m) = &mut app.menu {
                m.point_at(i);
            }
            After::MenuAccept
        }
        None => {
            app.close_menu();
            After::Nothing
        }
    }
}

/// El ratón dentro del gestor de extensiones.
///
/// La rueda mueve el cursor de la lista. Un clic en una fila la elige; en la
/// fila YA elegida abre sus ajustes, que es lo que su pie promete («pulsa
/// Intro, o la fila»). Un clic en un botón de la ficha dispara el comando
/// del botón — el MISMO que su tecla, nunca un segundo camino. Fuera de
/// toda zona no pasa nada: el gestor es modal y un clic perdido no lo
/// cierra, igual que una tecla que no está en su allowlist.
///
/// Elegir otra fila con los ajustes de la anterior abiertos los CIERRA:
/// sin esto la ficha enseñaría un plugin y los ajustes de otro, y con la
/// caja estrecha el panel de ajustes taparía la lista que se acaba de
/// pulsar.
fn extensions_mouse(app: &mut App, ev: MouseEvent) -> After {
    let Some(mgr) = &mut app.extensions else {
        return After::Nothing;
    };
    match ev.kind {
        MouseEventKind::ScrollUp => mgr.up(),
        MouseEventKind::ScrollDown => mgr.down(),
        MouseEventKind::Down(MouseButton::Left) => {
            let zona = app
                .mouse
                .extension_zones
                .iter()
                .find(|z| z.row == ev.row && ev.column >= z.x0 && ev.column <= z.x1)
                .copied();
            match zona.map(|z| z.hit) {
                Some(crate::ui::ExtensionHit::Row(i)) if i == mgr.cursor => {
                    return After::Extension("dialog.confirm");
                }
                Some(crate::ui::ExtensionHit::Row(i)) => {
                    if i < mgr.plugins.len() + mgr.errors.len() {
                        mgr.cursor = i;
                        // Elegir una fila devuelve el foco a la lista, como
                        // hacen las flechas: los botones son los del plugin
                        // elegido.
                        mgr.foco = crate::app::ExtFoco::Lista;
                        let otro = mgr.config.as_ref().is_some_and(|c| {
                            mgr.plugins.get(i).is_none_or(|p| p.id != c.plugin_id)
                        });
                        if otro {
                            mgr.config = None;
                        }
                    }
                }
                Some(crate::ui::ExtensionHit::Button(cmd)) => return After::Extension(cmd),
                None => {}
            }
        }
        _ => {}
    }
    After::Nothing
}

/// La zona de barra de pestañas bajo `(col, row)`, si hay alguna.
fn tab_zone_at(app: &App, col: u16, row: u16) -> Option<crate::ui::TabZone> {
    app.mouse
        .tab_zones
        .iter()
        .find(|z| z.row == row && col >= z.x0 && col <= z.x1)
        .copied()
}

/// La fila del sidebar de sitios bajo `(col, row)`, si la hay (#226).
fn place_zone_at(app: &App, col: u16, row: u16) -> Option<crate::ui::PlaceZone> {
    app.mouse
        .places_zones
        .iter()
        .find(|z| z.row == row && col >= z.x0 && col <= z.x1)
        .copied()
}

/// La fila del árbol bajo `(col, row)`, si la hay (#136).
fn tree_zone_at(app: &App, col: u16, row: u16) -> Option<crate::ui::TreeZone> {
    app.mouse
        .tree_zones
        .iter()
        .find(|z| z.row == row && col >= z.x0 && col <= z.x1)
        .copied()
}

/// Aplica lo que hace pulsar una zona de la barra de pestañas.
fn apply_tab_zone(app: &mut App, z: crate::ui::TabZone) {
    // El panel de la barra pulsada pasa a tener el foco: pulsar una pestaña
    // del otro lado y que la orden la reciba este sería lo contrario de lo
    // que el dedo dijo.
    app.set_focus(z.pane);
    match z.action {
        crate::ui::TabAction::Goto(i) => app.tab_goto(i + 1),
        crate::ui::TabAction::New => app.tab_new(),
        crate::ui::TabAction::Close => app.tab_close(),
    }
}

/// El gesto de redimensionar: agarrar un borde, moverlo y soltarlo.
///
/// `None` = este evento no es del gesto y sigue su camino. Los tres tiempos
/// están aquí juntos a propósito: un arrastre es una máquina de tres estados,
/// y repartirla por el despachador es como se acaba arrastrando con el botón
/// levantado.
///
/// El tamaño se escribe en el ÁRBOL, que es lo que la sesión guarda: por eso
/// un borde movido sigue donde se dejó al volver a abrir, sin nada más.
fn resize_gesture(app: &mut App, ev: MouseEvent) -> Option<After> {
    match ev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let borde = *app
                .mouse
                .borders
                .iter()
                .find(|b| b.hit(ev.column, ev.row))?;
            // Agarrar un borde no es un click en nada: se cancela lo que
            // hubiera armado, o al soltar se leería como una selección.
            app.mouse.drag.cancel();
            app.mouse.last_click = None;
            app.mouse.resizing = Some(borde);
            Some(After::Nothing)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let borde = app.mouse.resizing?;
            let eje = match borde.dir {
                norte_frontend::layout::Dir::Horizontal => ev.column,
                norte_frontend::layout::Dir::Vertical => ev.row,
            };
            if borde.largo == 0 {
                return Some(After::Nothing);
            }
            let dentro = f32::from(eje.saturating_sub(borde.inicio));
            let frac = dentro / f32::from(borde.largo);
            app.layout = app.layout.drag_border(borde.slot, frac, borde.largo);
            Some(After::Nothing)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            // Solo se come el evento si de verdad había un arrastre: un `Up`
            // cualquiera tiene sus propios dueños más abajo.
            app.mouse.resizing.take().map(|_| After::Nothing)
        }
        _ => None,
    }
}

/// Un arrastre de columna en vuelo.
#[derive(Debug, Clone)]
struct ColumnDrag {
    /// El id de la columna, con la forma que escribe `persist_column_width`
    /// y que usa la ventana (`ColumnId` en texto).
    column: String,
    /// El ancho con que se agarró.
    inicio: u16,
    /// La celda donde se agarró. El final de la columna no se mueve durante
    /// el gesto —el nombre, que es quien crece, absorbe la diferencia—, así
    /// que el ancho es `inicio` más lo que el puntero se haya movido a la
    /// izquierda desde AQUÍ. Medir contra el borde y no contra el agarre hacía
    /// saltar una celda al primer movimiento a quien agarraba la celda de
    /// antes del separador.
    agarre: u16,
    /// Hubo movimiento. Sin él, soltar no es un ancho nuevo sino un clic.
    movido: bool,
}

impl ColumnDrag {
    /// El ancho con el puntero en la celda `col`.
    const fn ancho_en(&self, col: u16) -> u16 {
        self.inicio.saturating_add(self.agarre).saturating_sub(col)
    }
}

/// El borde de columna bajo `(col, row)`, si lo hay.
///
/// Se agarra el borde que ABRE cada columna salvo el nombre —la celda del
/// separador y la de antes, dos como en los bordes de panel—, y no el que la
/// cierra: el nombre crece hacia la derecha y la última columna acaba en el
/// marco del panel, que ya es el borde que reparte paneles.
///
/// Los anchos salen de `column_widths` con el ancho interior de la geometría
/// PINTADA, la misma llamada que la cabecera: dos repartos son un borde que
/// se agarra en un sitio y se mueve desde otro.
fn column_border_at(app: &App, col: u16, row: u16) -> Option<ColumnDrag> {
    let geometry = app.mouse.geometry()?;
    for (i, g) in geometry.iter().enumerate() {
        let cabecera = g.first_list_row.checked_sub(1);
        if g.list_rows == 0
            || cabecera != Some(row)
            || col < g.x
            || col >= g.x.saturating_add(g.width)
        {
            continue;
        }
        let pane = app.panes.get(i)?;
        // El MISMO catálogo que usa la pintura: dos respuestas distintas aquí
        // harían que el borde que agarra el ratón fuese el de otra columna.
        let anchos = crate::ui::pane_columns(
            &app.columns,
            pane,
            g.width.saturating_sub(2),
            app.attr_catalog(pane.dir().scheme()),
        );
        let mut x = g.x.saturating_add(1);
        for (k, f) in anchos.iter().enumerate() {
            if k > 0 && (col == x || col.saturating_add(1) == x) {
                return Some(ColumnDrag {
                    column: f.id.to_string(),
                    inicio: f.width,
                    agarre: col,
                    movido: false,
                });
            }
            x = x.saturating_add(f.width);
        }
    }
    None
}

/// El arrastre del borde de una columna, en los tres tiempos del gesto, como
/// [`resize_gesture`].
///
/// Mientras dura, el ancho se aplica EN MEMORIA en cada movimiento —la
/// cabecera y las filas lo pintan en el frame siguiente— y solo al soltar se
/// pide guardarlo: escribir `norte.toml` en cada celda que cruza el puntero
/// sería el fichero reescrito cuarenta veces por gesto.
fn column_gesture(app: &mut App, ev: MouseEvent) -> Option<After> {
    match ev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let agarre = column_border_at(app, ev.column, ev.row)?;
            app.mouse.drag.cancel();
            app.mouse.last_click = None;
            // Pulsar la cabecera enfocaba el panel antes de que el borde fuera
            // agarrable; que ahora sea un borde no le quita eso.
            enfocar_lo_pulsado(app, ev.column, ev.row);
            app.mouse.columna = Some(agarre);
            Some(After::Nothing)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let agarre = app.mouse.columna.as_mut()?;
            agarre.movido = true;
            let cells = agarre.ancho_en(ev.column);
            let column = agarre.column.clone();
            app.columns.apply_width(&column, cells);
            Some(After::Nothing)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let agarre = app.mouse.columna.take()?;
            if !agarre.movido {
                return Some(After::Nothing);
            }
            let cells = app
                .columns
                .apply_width(&agarre.column, agarre.ancho_en(ev.column));
            app.mouse.ancho_soltado = Some((agarre.column, cells));
            Some(After::ColumnWidth)
        }
        _ => None,
    }
}

/// Lo que se atiende ANTES de los paneles: el menú, el visor, el gestor de
/// extensiones y el cerrojo de los demás overlays. `Some` = el evento ya
/// tiene dueño y los listados no lo ven.
fn por_encima_de_los_paneles(app: &mut App, ev: MouseEvent) -> Option<After> {
    let clic = matches!(ev.kind, MouseEventKind::Down(MouseButton::Left));
    // La barra de menús se atiende ANTES de todo: es un overlay, así que
    // mientras está abierta nada de detrás debe recibir un click, y sus propias
    // zonas tienen que poder pulsarse.
    if app.menu.is_some() {
        return Some(if clic {
            menu_click(app, ev.column, ev.row)
        } else {
            After::Nothing
        });
    }
    // Con el menú CERRADO pero la barra fijada, un clic en la fila de la barra
    // la abre. Va aquí y no más abajo porque esa fila no pertenece a ningún
    // panel: sin este brazo el clic caía en el hit-test de los listados, que
    // devuelve `None` para ella, y no pasaba nada.
    // Salvo en un botón de disposición (ADR 0133), que vive en esa misma fila
    // y corre su orden por el despacho de su atajo, como la barra de paneles.
    if app.menu_bar && ev.row == 0 && clic {
        if let Some(cmd) = app
            .mouse
            .panel_zones
            .iter()
            .find(|z| z.row == 0 && ev.column >= z.x0 && ev.column <= z.x1)
            .map(|z| z.command.clone())
        {
            app.mouse.drag.cancel();
            app.mouse.last_click = None;
            app.pending_panel_command = Some(cmd);
            return Some(After::PanelBar);
        }
        return Some(menu_click(app, ev.column, ev.row));
    }
    // La barra de teclas (spec 2026-09-10): una celda pulsada es la tecla
    // pulsada, y se despacha como tal. ANTES del cerrojo de los overlays por
    // lo mismo que los botones de un modal de abajo: las zonas ya vienen
    // vacías cuando no hay nada que pulsar (`key_zones`), y este orden no
    // depende de que alguien se acuerde. Su fila no es de ningún panel.
    if clic
        && let Some(key) = app
            .mouse
            .key_zones
            .iter()
            .find(|z| z.row == ev.row && ev.column >= z.x0 && ev.column <= z.x1)
            .map(|z| z.key)
    {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        app.pending_key = Some(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::F(key),
            KeyModifiers::NONE,
        ));
        return Some(After::SynthKey);
    }
    // Un botón de un modal (spec 2026-09-10), por el mismo camino: el chord
    // pintado se sintetiza y `on_key` lo resuelve contra el diálogo, igual
    // que si el terminal lo hubiera entregado. Un chord que la TUI no puede
    // entregar (no hay ninguno en un preset) se ignora.
    if clic
        && let Some(chord) = app
            .mouse
            .modal_zones
            .iter()
            .find(|z| z.row == ev.row && ev.column >= z.x0 && ev.column <= z.x1)
            .map(|z| z.chord.clone())
        // La inversa de `paint_chord`, no un `to_lowercase`: `Alt+Shift+C`
        // vuelve a `alt+C` y una `K` suelta sigue siendo `K`, que el keymap
        // distingue de `k` (revisión M2).
        && let Ok(chord) = norte_frontend::keymap::parse_chord(
            &norte_frontend::keymap::unpaint_chord(&chord),
        )
        && let Some((mods, code)) = crate::keymap::crossterm_from_chord(chord)
    {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        app.pending_key = Some(crossterm::event::KeyEvent::new(code, mods));
        return Some(After::SynthKey);
    }
    // La ayuda ANTES que el visor: se pinta por encima de todo, visor
    // incluido, así que lo que hay bajo el puntero es ella.
    if raton_en_la_ayuda(app, ev) {
        return Some(After::Nothing);
    }
    if rueda_en_el_visor(app, ev) {
        return Some(After::Nothing);
    }
    // El gestor de extensiones ANTES del cerrojo de los overlays: es un
    // overlay, y hasta aquí eso significaba «el ratón no existe». Sus filas
    // y sus botones se pintan; se pulsan.
    if app.extensions.is_some() {
        return Some(extensions_mouse(app, ev));
    }
    overlay_open(app).then_some(After::Nothing)
}

/// Un evento de ratón de crossterm, con el reloj real.
pub fn handle(app: &mut App, ev: MouseEvent) -> After {
    handle_at(app, ev, Instant::now())
}

/// Como [`handle`] con el instante inyectado: el doble click es una ventana
/// de tiempo, y un test que dependiera del reloj de la máquina sería un
/// test que falla en CI un martes.
pub fn handle_at(app: &mut App, ev: MouseEvent, now: Instant) -> After {
    if let Some(after) = por_encima_de_los_paneles(app, ev) {
        return after;
    }
    // #324: la barra de paneles, por el mismo motivo que la de menús — esa
    // fila no pertenece a ningún panel, así que sin este brazo el clic caía en
    // el hit-test de los listados y no pasaba nada. Los paneles laterales
    // nacieron mudos al ratón una vez (#290) y no se repite.
    //
    // DEBAJO del `overlay_open`, y eso fue un BLOCKER de revisión: la barra se
    // pinta antes que los overlays, así que con la ayuda abierta un clic en su
    // barra de título —fila 1— caía en un botón y abría o cerraba un panel que
    // el lector no estaba viendo. Ahora las zonas también se vacían con un
    // overlay delante (`panel_bar_visible`), así que esto es el segundo
    // cinturón del mismo invariante: pintada y pulsable son lo mismo.
    if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(cmd) = app
            .mouse
            .panel_zones
            .iter()
            .find(|z| z.row == ev.row && ev.column >= z.x0 && ev.column <= z.x1)
            .map(|z| z.command.clone())
    {
        // Y se suelta el gesto en vuelo, como hacen el sidebar y el árbol: sin
        // esto, un clic en una fila, otro en la barra y otro en la misma fila
        // dentro de la ventana del doble clic se leían como un doble clic, y
        // norte entraba en un directorio que el lector solo había señalado.
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        app.pending_panel_command = Some(cmd);
        return After::PanelBar;
    }
    // El indicador de sesión suelta, en la barra de estado: pulsarlo pide la
    // explicación, que es la página de la ayuda que la tiene. Un indicador
    // discreto solo lo es si hay una forma igual de discreta de saber qué
    // significa. Detrás de `overlay_open` por lo mismo que la barra de
    // paneles: con la ayuda delante, la barra no es pulsable.
    if pulsa_indicador_de_sesion(app, ev) {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        return After::SessionHelp;
    }
    // Un elemento de la barra de estado (ADR 0132): corre su comando por el
    // MISMO despacho que su atajo y que un botón de la barra de paneles. La
    // insignia de avisos es uno de ellos (`notices` → `layout.log`).
    if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(cmd) = elemento_de_estado_en(app, ev)
    {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        app.pending_panel_command = Some(cmd.to_owned());
        return After::PanelBar;
    }
    // El ARRASTRE de un borde va antes que todo lo del listado, y en los tres
    // tiempos del gesto: mientras dura, el puntero se sale del borde y no por
    // eso deja de arrastrarlo.
    if let Some(after) = resize_gesture(app, ev) {
        return after;
    }
    // Y el de un borde de COLUMNA, por lo mismo: la cabecera es cromo para el
    // listado, y el puntero sale de la celda del borde en el primer
    // movimiento.
    if let Some(after) = column_gesture(app, ev) {
        return after;
    }
    if let Some(after) = pulsar_panel(app, ev) {
        return after;
    }
    // Las barras de pestañas se atienden ANTES: sus celdas son cromo para el
    // hit test del listado, así que un click ahí caería en «este panel,
    // ninguna fila» y el botón no haría nada.
    if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(z) = tab_zone_at(app, ev.column, ev.row)
    {
        apply_tab_zone(app, z);
        return After::Nothing;
    }
    // El sidebar de sitios, por lo mismo: sus celdas no son de ningún
    // listado, así que un click ahí caía en «fuera de los panes» y no hacía
    // nada — el panel se pintaba y no se podía tocar (#226). El arrastre se
    // cancela: desde aquí no se arrastra nada.
    if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(z) = place_zone_at(app, ev.column, ev.row)
    {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        return match app.places_click(z.index) {
            crate::app::PlacesClick::Focused => After::Nothing,
            crate::app::PlacesClick::Folded => After::PlacesFolded,
            crate::app::PlacesClick::Activate => After::PlacesActivate,
        };
    }
    // Y el árbol, por lo mismo: sus celdas tampoco son de ningún listado, así
    // que el click caía en «fuera de los panes» y el panel se pintaba sin
    // poder tocarse (#136). Pulsar la MARCA pliega o despliega; el resto de la
    // fila selecciona, y la segunda pulsación activa.
    if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(z) = tree_zone_at(app, ev.column, ev.row)
    {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        let spot = if ev.column == z.mark_x {
            crate::app::TreeSpot::Mark
        } else {
            crate::app::TreeSpot::Row
        };
        return match app.tree_click(z.index, spot) {
            crate::app::TreeClick::Focused => After::Nothing,
            crate::app::TreeClick::Activate => After::TreeActivate,
        };
    }
    let hit = hit_test(app, ev.column, ev.row);
    let m = mods(ev.modifiers);
    app.mouse.last_mods = m;
    match ev.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let abajo = matches!(ev.kind, MouseEventKind::ScrollDown);
            // El visor ACOPLADO primero: su hueco no es un listado, así que el
            // hit-test devuelve `None` y la rueda se perdía. Un panel que se
            // pinta y no se puede rodar es la misma avería que un panel que no
            // se puede pulsar (#226, #290).
            if !rueda_en_preview(app, ev.column, ev.row, abajo) {
                scroll(app, hit, abajo);
            }
        }
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

/// Pulsar un panel: le da el TECLADO y, si cayó en una zona de un panel de
/// plugin, ejecuta su comando (fase 3).
///
/// El foco va ANTES que todos los caminos especializados de `handle_at`: de
/// quién es el teclado lo decide el hueco que hay bajo el puntero, no lo que
/// cada camino sepa hacer después con el click. Puesto en cada camino, el panel
/// al que ninguno atiende —el visor acoplado, la hoja, el árbol— se quedaría
/// sin poder recibirlo.
///
/// La zona va por el MISMO camino que un botón de la barra de paneles, que es
/// el mismo que su atajo de teclado: un plugin no ejecuta nada por su cuenta
/// —nombra un comando del catálogo y lo despacha norte—, así que un clic aquí
/// no puede hacer nada que una tecla no pudiera. La policy queda intacta
/// (regla dura 9).
fn pulsar_panel(app: &mut App, ev: MouseEvent) -> Option<After> {
    if !matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
        return None;
    }
    enfocar_lo_pulsado(app, ev.column, ev.row);
    // El mapa de disco primero: su rectángulo nombra un HIJO, no un comando,
    // así que no puede pasar por el camino de las zonas de plugin. Pulsar uno
    // lo elige Y entra, que es el gesto entero — en un mapa señalar y abrir es
    // el mismo acto, como un doble clic en un listado.
    if let Some(arg) = hijo_del_mapa_en(app, ev.column, ev.row) {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        if let Ok(seg) = norte_proto::Segment::parse_wire(&arg) {
            let slot = app.disk_map_slot();
            let es_dir = slot
                .and_then(|s| app.panes.disk_map(s))
                .and_then(|m| {
                    m.informe()
                        .children
                        .iter()
                        .find(|c| c.name == seg)
                        .map(|c| c.kind == norte_proto::EntryKind::Dir)
                })
                .unwrap_or(false);
            if let Some(s) = slot
                && let Some(m) = app.panes.disk_map_mut(s)
            {
                m.elegir(&seg);
            }
            // Entrar SOLO en un directorio: el mapa enseña las dos cosas, y
            // «entrar» en un fichero no es navegar.
            if es_dir {
                app.pending_disk_map_enter = Some(seg);
            }
        }
        return Some(After::PanelBar);
    }
    let cmd = zona_de_panel_en(app, ev.column, ev.row)?;
    // Y se suelta el gesto en vuelo, como la barra: sin esto, un clic en una
    // fila y otro en la zona dentro de la ventana del doble clic se leían como
    // uno solo.
    app.mouse.drag.cancel();
    app.mouse.last_click = None;
    app.pending_panel_command = Some(cmd);
    Some(After::PanelBar)
}

/// El comando de la zona pulsable del panel de plugin que hay en `(col, row)`,
/// si la hay (fase 3).
///
/// Las coordenadas del marco son las de DENTRO del borde: el guest describe su
/// contenido y no sabe dónde cayó su hueco, así que la cuenta —restar el
/// origen del hueco y el borde— la hace quien pintó, que es esta casa.
///
/// El argumento del `Hit` todavía no viaja: el catálogo de comandos del
/// terminal no toma parámetros, así que una zona ejecuta su comando y nada
/// más. Cuando exista un comando con operando, entra por aquí.
fn zona_de_panel_en(app: &App, col: u16, row: u16) -> Option<String> {
    let slot = app.panel_slot()?;
    let rect = app.mouse.slots.iter().find(|s| s.slot == slot)?;
    if !rect.contains(col, row) {
        return None;
    }
    // DENTRO del borde por los cuatro lados. `checked_sub` solo cuida el
    // arriba-izquierda: sin acotar por el otro lado, un clic en el borde
    // derecho daba la columna siguiente a la última de dentro, y una zona que
    // ocupa el ancho entero —el caso normal de una etiqueta pulsable— se
    // disparaba al pulsar el propio marco, por ejemplo yendo a arrastrarlo.
    let dentro_x = col
        .checked_sub(rect.x.saturating_add(1))
        .filter(|x| *x < rect.width.saturating_sub(2))?;
    let dentro_y = row
        .checked_sub(rect.y.saturating_add(1))
        .filter(|y| *y < rect.height.saturating_sub(2))?;
    let marco = app.paneles.get(slot)?.frame.as_ref()?;
    marco
        .hit_at(dentro_y, dentro_x)
        .map(|h| h.command.clone())
        // El comando lo elige el PLUGIN, igual que la etiqueta, y nada los ata:
        // una zona que pone «Actualizar» podía nombrar `pane.unpack`. El filtro
        // deja el clic con el mismo alcance que la tecla de un panel enfocado
        // (`ALLOW_PANEL`), que es lo que esta casa promete.
        .filter(|c| norte_frontend::frame::zona_puede(c))
}

/// El NOMBRE del hijo cuyo rectángulo hay en `(col, row)` del mapa de disco.
///
/// Gemelo de [`zona_de_panel_en`] y con dos diferencias que importan, las dos
/// porque el marco es NUESTRO y no de un tercero:
///
/// 1. **Devuelve el `arg`, no el comando.** En un panel de plugin el argumento
///    se tira —el catálogo del terminal no toma parámetros— y la zona solo
///    ejecuta su comando. Aquí el argumento ES la respuesta: en qué hijo se
///    entra. Se devuelve en su forma WIRE, que es la reversible; lo que se
///    pinta va enmascarado y no nombra ningún fichero.
/// 2. **No pasa por `zona_puede`.** Ese filtro existe porque en un panel de
///    plugin la etiqueta y el comando los elige un tercero y nada los ata. Los
///    rectángulos de aquí los pone `squarify`, así que filtrarlos sería
///    protegerse de uno mismo — y dejaría el mapa sin su único gesto.
fn hijo_del_mapa_en(app: &App, col: u16, row: u16) -> Option<String> {
    let slot = app.disk_map_slot()?;
    let rect = app.mouse.slots.iter().find(|s| s.slot == slot)?;
    if !rect.contains(col, row) {
        return None;
    }
    // DENTRO del borde por los cuatro lados, con la misma cuenta —y el mismo
    // motivo— que el panel de plugin.
    let dentro_x = col
        .checked_sub(rect.x.saturating_add(1))
        .filter(|x| *x < rect.width.saturating_sub(2))?;
    let dentro_y = row
        .checked_sub(rect.y.saturating_add(1))
        .filter(|y| *y < rect.height.saturating_sub(2))?;
    let mapa = app.panes.disk_map(slot)?;
    let marco = norte_frontend::treemap::squarify(
        &mapa.informe().children,
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    );
    marco.hit_at(dentro_y, dentro_x).and_then(|h| h.arg.clone())
}

/// Le da el teclado al panel que hay bajo `(col, row)`.
///
/// Solo con el botón IZQUIERDO abajo: la rueda mueve el listado bajo el
/// puntero sin robarle el foco a nadie (ver [`scroll`]), y un arrastre que
/// cruza el panel de al lado no puede llevarse el teclado a media faena.
///
/// Un hueco que no toma teclas no cambia nada, y un click fuera de todo hueco
/// —no hay ninguno: el reparto cubre la pantalla entera— tampoco.
fn enfocar_lo_pulsado(app: &mut App, col: u16, row: u16) {
    let Some(slot) = app
        .mouse
        .slots
        .iter()
        .find(|s| s.contains(col, row))
        .map(|s| s.slot)
    else {
        return;
    };
    app.focus_slot(slot);
}

/// La rueda sobre el VISOR a pantalla completa: lo desplaza y dice que sí.
///
/// Se atiende ANTES del corte de los overlays porque el visor es uno de ellos,
/// así que hasta ahora rodar sobre un fichero abierto no hacía absolutamente
/// nada. Es el gesto más obvio que tiene un visor, y lo único que hay debajo es
/// un listado que no se ve — desplazar ESE habría sido peor.
///
/// Los dos EJES, como en la ventana: el visor no envuelve, así que a lo ancho
/// hace tanta falta como a lo alto. `shift+rueda` es el gesto de siempre para
/// el eje horizontal, y algunos terminales mandan además una rueda horizontal
/// propia.
///
/// `true` también cuando el visor está abierto y el evento no es una rueda: con
/// un fichero delante, ningún otro gesto del ratón tiene dueño.
fn rueda_en_el_visor(app: &mut App, ev: MouseEvent) -> bool {
    let shift = mods(ev.modifiers).shift;
    let Some(v) = app.viewer.as_mut() else {
        return false;
    };
    match ev.kind {
        MouseEventKind::ScrollUp if shift => v.scroll_left(WHEEL_ROWS),
        MouseEventKind::ScrollDown if shift => v.scroll_right(WHEEL_ROWS),
        MouseEventKind::ScrollUp => v.scroll_up(WHEEL_ROWS),
        MouseEventKind::ScrollDown => v.scroll_down(WHEEL_ROWS),
        MouseEventKind::ScrollLeft => v.scroll_left(WHEEL_ROWS),
        MouseEventKind::ScrollRight => v.scroll_right(WHEEL_ROWS),
        _ => {}
    }
    true
}

/// El ratón con la ayuda abierta: la rueda desplaza lo que hay debajo del
/// puntero —la lateral pasa de página, el cuerpo baja— y un clic elige una
/// página o una acción, lo mismo que en la ventana.
///
/// Hasta ahora la ayuda era un overlay más para el cerrojo de
/// [`overlay_open`], o sea que con ella abierta el ratón no existía: ni la
/// rueda bajaba por el texto. `true` siempre que la ayuda esté abierta, porque
/// con ella delante ningún otro gesto tiene dueño.
///
/// El clic en una acción la SELECCIONA y no la ejecuta: correr un comando que
/// toca ficheros por un clic en un texto que se está leyendo es demasiado
/// fácil de hacer sin querer. `Enter` la corre, como con el teclado.
fn raton_en_la_ayuda(app: &mut App, ev: MouseEvent) -> bool {
    if app.help.is_none() {
        return false;
    }
    let zonas = app.mouse.help_zones.clone();
    let (Some(help), Some(z)) = (app.help.as_mut(), zonas) else {
        return true;
    };
    let dentro = |r: ratatui::layout::Rect| {
        ev.column >= r.x
            && ev.column < r.x.saturating_add(r.width)
            && ev.row >= r.y
            && ev.row < r.y.saturating_add(r.height)
    };
    match ev.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let abajo = matches!(ev.kind, MouseEventKind::ScrollDown);
            if dentro(z.sidebar) {
                if abajo {
                    help.state.down();
                } else {
                    help.state.up();
                }
            } else if dentro(z.body) {
                let filas = isize::try_from(WHEEL_ROWS).unwrap_or(1);
                help.state.scroll_body(if abajo { filas } else { -filas });
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if dentro(z.sidebar) {
                if let Some(&(_, fila)) = z.rows.iter().find(|(y, _)| *y == ev.row) {
                    help.state.click_row(fila);
                }
            } else if dentro(z.body) {
                let linea = help.state.body_scroll() + usize::from(ev.row - z.body.y);
                if let Some(i) = help.body().1.iter().position(|&l| l == linea) {
                    help.state.click_action(i);
                }
            }
        }
        _ => {}
    }
    true
}

/// La rueda sobre un hueco de visor ACOPLADO: lo desplaza y dice que sí.
///
/// `false` cuando bajo el puntero no hay uno, y entonces la rueda sigue su
/// camino normal hacia el listado.
fn rueda_en_preview(app: &mut App, col: u16, row: u16, abajo: bool) -> bool {
    let Some(slot) = app
        .mouse
        .slots
        .iter()
        .find(|s| s.contains(col, row))
        .map(|s| s.slot)
    else {
        return false;
    };
    let Some(v) = app
        .panes
        .preview_mut(slot)
        .and_then(crate::preview::Preview::viewer_mut)
    else {
        return false;
    };
    if abajo {
        v.scroll_down(WHEEL_ROWS);
    } else {
        v.scroll_up(WHEEL_ROWS);
    }
    true
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

/// Despacha por nombre el comando que un CLIC eligió, por el mismo camino que
/// su tecla.
///
/// Uno para el menú y para la barra de paneles (#324): los dos hacen lo mismo
/// con distinto origen, y tenerlo dos veces es cómo el menú y la barra acaban
/// abriendo un panel de dos maneras que se separan en cuanto una crece un
/// detalle. Es la lección de ADR 0077 aplicada dentro de un solo frontend.
#[expect(clippy::too_many_arguments, reason = "wiring del bucle, no API")]
/// Despacha un comando NOMBRADO pedido por el ratón, y remata todo lo que ese
/// comando deje pedido.
///
/// «Todo» son tres cosas y las tres van AQUÍ, no en cada brazo del ratón: el
/// desenlace del `cd`, la cosecha de la búsqueda viva y **el opener que el
/// comando haya dejado armado**. El último faltaba en el brazo del doble
/// click, así que un doble click sobre un `.jpg` corría `nav.enter`, éste
/// resolvía el programa del escritorio en `pending_open`… y nadie lo lanzaba:
/// el gesto no hacía absolutamente nada, ni decía por qué. El rustdoc de
/// [`on_mouse`] prometía justo eso desde el principio («lanzar el opener que un
/// doble click dejó resuelto»), y el cable no estaba.
///
/// Compartir la salida es el arreglo, no añadir la línea que faltaba: dos
/// brazos que rematan a mano son dos sitios donde olvidarse del tercero.
async fn despachar_clic(
    app: &mut crate::app::App,
    backend: &norte_core::backend::Backend,
    events: &mut crate::console::Console<'_>,
    capture: &mut Capture,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    lang: norte_i18n::Lang,
    quick_mode: crate::nav::Mode,
    confirm_quit: crate::config::ConfirmQuit,
    cfg: &crate::config::LoadedConfig,
    work: &mut crate::jobs::InFlight,
    id: &str,
) {
    let Some(cmd) = Command::parse(id) else {
        return;
    };
    let outcome = dispatch(
        app,
        backend,
        events,
        help_lines,
        lang,
        quick_mode,
        confirm_quit,
        cfg,
        cmd,
    )
    .await;
    apply_cd(
        &app.panes,
        &mut work.fill,
        &mut work.decorate,
        &mut work.probed,
        &mut work.search,
        outcome,
    );
    reap_search_run(app, &mut work.search);
    crate::event_loop::launch_pending(app, events, capture).await;
}

/// Escribe en `[ui.columns]` el ancho que dejó el último arrastre.
///
/// Fuera del bucle (`spawn_blocking`, regla 2) y al PERFIL activo si lo hay,
/// como el selector de columnas: escribirlo en la capa de usuario con un
/// perfil que también fija el ancho lo dejaría guardado y sin efecto. Solo el
/// fallo se dice; un ancho que se guarda bien ya se está viendo.
async fn guardar_ancho_de_columna(app: &mut crate::app::App) {
    let Some((column, cells)) = app.mouse.take_column_width() else {
        return;
    };
    let Some(dir) = app.config_write_dir() else {
        app.message = Some(norte_i18n::t("msg-settings-no-config-dir"));
        return;
    };
    let res = tokio::task::spawn_blocking(move || {
        crate::config::persist_column_width(&dir, &column, cells)
    })
    .await;
    match res {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            app.message = Some(norte_i18n::ta(
                "msg-settings-save-failed",
                &[("error", &crate::app::io_error_category(&e))],
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "la tarea que guarda el ancho de columna no terminó");
            app.message = Some(norte_i18n::t("msg-settings-save-crashed"));
        }
    }
}

/// Aplica un evento de ratón y remata lo que el gesto deje pedido.
///
/// La semántica del gesto —qué marca, qué barre, qué transfiere— vive en
/// `norte-frontend` (regla 7) y la resuelve [`handle`]; lo que queda aquí es
/// lo que solo el bucle puede hacer: despachar el comando de un elemento de
/// menú, navegar, o lanzar el opener que un doble click dejó resuelto.
///
/// Es el gemelo de [`crate::keys::on_key`]: un gesto es OTRA entrada, y toma
/// exactamente los mismos caminos que la tecla equivalente — que es lo que
/// impide que el ratón y el teclado diverjan.
#[expect(clippy::too_many_arguments, reason = "wiring del bucle, no API")]
pub async fn on_mouse(
    app: &mut crate::app::App,
    backend: &norte_core::backend::Backend,
    capture: &mut Capture,
    // La terminal viaja dentro (`events.terminal()`): un clic también puede
    // abrir una navegación larga, y la terminal tiene un solo dueño.
    events: &mut crate::console::Console<'_>,
    resolver: &mut crate::keymap::Resolver,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    lang: norte_i18n::Lang,
    quick_mode: crate::nav::Mode,
    confirm_quit: crate::config::ConfirmQuit,
    cfg: &crate::config::LoadedConfig,
    work: &mut crate::jobs::InFlight,
    me: crossterm::event::MouseEvent,
) {
    match self::handle(app, me) {
        // `SynthKey`: la tecla sintetizada la despacha el bucle por
        // `on_key`, justo después de este gesto — aquí no están los tres
        // resolvers. Nada que hacer, como con `Nothing`.
        self::After::Nothing | self::After::SynthKey => {}
        // Soltar el borde de una columna: el ancho ya está en memoria y
        // pintado; queda escribirlo con la MISMA función que usa la ventana.
        self::After::ColumnWidth => guardar_ancho_de_columna(app).await,
        // El indicador de sesión suelta: la explicación está en la ayuda, y
        // se abre por el MISMO constructor que `F1` sobre una fila de la
        // paleta — una página en mano, no un contexto que resolver.
        self::After::SessionHelp => {
            crate::overlays::open_help_topic(app, lang, help_lines, SESSION_HELP_TOPIC);
        }
        // Un botón del gestor de extensiones va por el MISMO despacho que su
        // tecla: encender, aprobar, desinstalar y abrir los ajustes son
        // decisiones del gestor, y el ratón solo las señala.
        self::After::Extension(cmd) => {
            crate::screens::on_extensions_click(app, backend, lang, help_lines, cmd).await;
        }
        // #324: un botón de la barra de paneles va por el MISMO despacho que
        // su atajo. Dos caminos para abrir el mismo panel divergen en cuanto
        // uno de los dos crece un detalle — es la lección de ADR 0077 aplicada
        // dentro de un solo frontend.
        self::After::PanelBar => {
            if let Some(id) = app.pending_panel_command.take() {
                despachar_clic(
                    app,
                    backend,
                    events,
                    capture,
                    help_lines,
                    lang,
                    quick_mode,
                    confirm_quit,
                    cfg,
                    work,
                    &id,
                )
                .await;
            }
        }
        // Pulsar un elemento del menú: el ratón ya
        // dejó el cursor encima; ejecutarlo es
        // asíncrono y necesita el backend, así que se
        // remata aquí — el MISMO camino que `Enter`,
        // que es lo que hace que un menú y una tecla no
        // puedan divergir.
        self::After::MenuAccept => {
            if let Some(id) = app.take_menu_choice() {
                despachar_clic(
                    app,
                    backend,
                    events,
                    capture,
                    help_lines,
                    lang,
                    quick_mode,
                    confirm_quit,
                    cfg,
                    work,
                    &id,
                )
                .await;
            }
        }
        // Doble click = `nav.enter`, por el MISMO `dispatch`
        // que la tecla: mismo cd, mismo relleno paginado,
        // misma cosecha de la búsqueda viva. Un segundo
        // camino para entrar en un directorio sería un
        // segundo sitio donde arreglar cada bug de cd.
        //
        // Y por el mismo remate que el menú (`despachar_clic`),
        // que es lo que faltaba: sobre un FICHERO, `nav.enter`
        // resuelve el programa del escritorio y lo deja
        // armado, así que sin lanzarlo un doble click en un
        // `.jpg` no hacía nada.
        self::After::Enter => {
            // K3a: un gesto es OTRA entrada. La secuencia que
            // el lector estuviera tecleando se abandona con su
            // panel — no la completa el ratón, y dejarla
            // armada haría que la siguiente tecla disparase un
            // comando pedido antes de cambiar de directorio.
            app.abandon_pending(resolver);
            despachar_clic(
                app,
                backend,
                events,
                capture,
                help_lines,
                lang,
                quick_mode,
                confirm_quit,
                cfg,
                work,
                "nav.enter",
            )
            .await;
        }
        // #226: el sidebar con el ratón toma los MISMOS
        // caminos que su teclado. Desplegar las unidades
        // es el momento de volver a pedirlas —y plegarlas,
        // el de no pedirlas—, así que el ratón no puede
        // ser un cuarto disparador de refresco: es este.
        self::After::PlacesFolded => drain_places_drives(app, backend).await,
        // Y activar una fila lleva el listado por el
        // flujo de `cd` de siempre, igual que `Enter`
        // dentro del sidebar.
        self::After::PlacesActivate => {
            app.abandon_pending(resolver);
            if let Some(path) = app.places_activate() {
                let pane = app.focus();
                let outcome = cd_in(app, backend, events, pane, path, Trail::Record).await;
                apply_cd(
                    &app.panes,
                    &mut work.fill,
                    &mut work.decorate,
                    &mut work.probed,
                    &mut work.search,
                    outcome,
                );
            }
        }
        // Y la rama del árbol, por el MISMO flujo de `cd` que su `Enter`
        // (#136): el árbol despliega la rama y manda ahí el listado enfocado.
        self::After::TreeActivate => {
            app.abandon_pending(resolver);
            if let Some(path) = app.tree_activate() {
                let pane = app.focus();
                let outcome = cd_in(app, backend, events, pane, path, Trail::Record).await;
                apply_cd(
                    &app.panes,
                    &mut work.fill,
                    &mut work.decorate,
                    &mut work.probed,
                    &mut work.search,
                    outcome,
                );
            }
        }
    }
}
