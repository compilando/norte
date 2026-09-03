//! Lo que el renderer PIDE.
//!
//! Son acciones SEMÁNTICAS, no métodos del backend: «mueve el cursor», no
//! «llama a `fs.list` con este cursor». La diferencia importa porque lo que
//! se expone es lo que un renderer puede hacer, y un renderer no debe poder
//! pedir un `rpc(method, params)` arbitrario (ADR 0066, decisión D11).
//!
//! Ninguna acción nombra un path. Se actúa sobre filas por su [`RowKey`], y
//! toda acción que nombre una fila lleva TAMBIÉN la generación en la que el
//! renderer la vio. Sin ese par la clave no dice nada: es un índice, y un
//! índice de una pantalla anterior nombra otro fichero. El host compara la
//! generación con la época del listado y responde
//! [`crate::ActionAck::Stale`] cuando no coinciden — que es lo que impide que
//! un click tardío actúe sobre lo que ocupó esa fila DESPUÉS.
//!
//! Y ninguna acción acepta una cadena de ruta, ni la aceptará: lo que el
//! renderer puede nombrar es lo que el host le dio.

use serde::{Deserialize, Serialize};

use crate::bridge::{ModalId, RowKey};
use crate::keys::KeyInput;

/// Una petición del renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum UiAction {
    /// Mueve el cursor del hueco. `delta` en filas; negativo hacia arriba.
    ///
    /// Es la acción que más se repite (una tecla mantenida) y el host NO la
    /// fusiona: aplica una por una y emite un parche de cursor por cada una.
    /// Con el renderer de referencia no hay nada que fusionar —serializa sus
    /// llamadas, así que como mucho hay una en el buzón—, y fusionar sin
    /// necesidad complica el punto donde se contestan los acuses. Un renderer
    /// que mande en lotes hará que valga la pena; hasta entonces, esto
    /// describe lo que pasa y no lo que estaría bien.
    MoveCursor {
        /// Hueco.
        slot_id: u32,
        /// Filas a mover.
        delta: i64,
    },
    /// Pone el cursor en una fila concreta (un click).
    SelectRow {
        /// Hueco.
        slot_id: u32,
        /// Fila.
        key: RowKey,
        /// La generación en la que el renderer vio esa fila.
        generation: u64,
    },
    /// Marca o desmarca una fila.
    ToggleMark {
        /// Hueco.
        slot_id: u32,
        /// Fila.
        key: RowKey,
        /// La generación en la que el renderer vio esa fila.
        generation: u64,
    },
    /// Marca TODO el rango entre dos filas, extremos incluidos.
    ///
    /// Un barrido con el ratón (shift+click, arrastre) es UNA acción y no una
    /// ristra de `ToggleMark`: qué entra en un rango —y qué no, como `..`—
    /// es una regla de selección, y esas viven en `norte-frontend`, no en el
    /// renderer (ADR 0066, decisión D14). El orden de los extremos da igual.
    MarkRange {
        /// Hueco.
        slot_id: u32,
        /// Un extremo.
        from: RowKey,
        /// El otro.
        to: RowKey,
        /// La generación en la que el renderer vio esas filas.
        generation: u64,
    },
    /// Abre lo que haya bajo esa fila: entra en el directorio, o abre el
    /// fichero por el camino de siempre.
    Activate {
        /// Hueco.
        slot_id: u32,
        /// Fila.
        key: RowKey,
        /// La generación en la que el renderer vio esa fila.
        generation: u64,
    },
    /// Sube al directorio padre.
    Parent {
        /// Hueco.
        slot_id: u32,
    },
    /// Atrás y adelante en el rastro de navegación.
    History {
        /// Hueco.
        slot_id: u32,
        /// `true` = atrás.
        back: bool,
    },
    /// La ventana visible cambió (scroll o resize).
    ///
    /// Llega DEBOUNCED desde el renderer: el pintado del scroll es suyo, y lo
    /// único que cruza es qué filas hacen falta.
    SetVisibleRange {
        /// Hueco.
        slot_id: u32,
        /// Primera fila visible.
        first: u64,
        /// Cuántas caben.
        count: u32,
    },
    /// Ordena el listado por una columna (un click en su cabecera).
    ///
    /// La columna va por su ID, no por su posición ni por su etiqueta: qué
    /// significa ordenar por ella —y si se invierte o empieza de nuevo— lo
    /// decide la regla compartida (`SortSpec::after_click`), no el renderer.
    SortBy {
        /// Hueco.
        slot_id: u32,
        /// Id de la columna, tal como viajó en su cabecera.
        column: String,
    },
    /// Cambia el foco de teclado de hueco.
    FocusSlot {
        /// Hueco.
        slot_id: u32,
    },
    /// Responde a un diálogo.
    ///
    /// El `choice` es uno de los ids que el propio diálogo publicó. Un id que
    /// no esté en la lista no se interpreta: no hay respuestas implícitas.
    Dialog {
        /// Diálogo.
        id: ModalId,
        /// Respuesta elegida.
        choice: String,
        /// La contraseña, y SOLO para un diálogo que la pide (#327).
        ///
        /// Viaja aquí y no por [`Self::DialogInput`] a propósito. Ese manda el
        /// campo ENTERO en cada pulsación, que para un nombre de fichero está
        /// bien y para una contraseña significa que `h`, `hu`, `hun`… cruzan el
        /// IPC y se quedan, cada uno en su trozo de heap que nadie pisa: una
        /// contraseña de veinte caracteres deja veinte prefijos suyos por el
        /// camino. Con esto cruza UNA vez, en el instante en que el lector
        /// decide entregarla.
        ///
        /// El corolario es que **el host no sabe lo que se está tecleando**
        /// hasta ese momento, y no le hace falta: el campo lo enmascara el
        /// propio `input type=password` del renderer, así que no hay puntos
        /// que contar. Lo que el host no tiene no se le puede escapar.
        ///
        /// `None` en todos los demás diálogos, y en uno de secreto significa
        /// campo vacío: confirmar así es INERTE (ver `responder_dialogo`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret: Option<String>,
    },
    /// Vuelve a listar UN hueco: el reintento de uno que quedó en error.
    ///
    /// Por hueco y no `pane.refresh`, que relista todos los visibles y actúa
    /// sobre el foco: esto lo dispara un clic SOBRE el error de un hueco
    /// concreto, y refrescar los otros de paso sería hacer más de lo que se
    /// pidió.
    ///
    /// Es el gesto que convierte un panel parado en la pregunta que
    /// corresponda —la contraseña de una conexión, típicamente—: el host no
    /// pregunta solo al arrancar, porque restaurar una sesión no es pedir
    /// conectarse.
    RefreshSlot {
        /// El hueco que se reintenta.
        slot_id: u32,
    },
    /// Enseñar el registro hasta este nivel (#326).
    ///
    /// Sube el del ANILLO si hace falta y nunca lo baja: filtrar en la
    /// pantalla lo que nunca se registró es imposible, y dejar de capturar al
    /// bajar dejaría un agujero del tamaño del rato que se estuvo abajo.
    LogSetLevel {
        /// Vocabulario CERRADO: `error`, `warn`, `info`, `debug`, `trace`.
        /// Uno que no se conoce se DICE, no cae en `info`.
        level: String,
    },
    /// El filtro de texto del registro, sobre módulo y mensaje.
    LogSetFilter {
        /// Lo tecleado. Vacío = todo.
        filter: String,
    },
    /// Sube (`delta` negativo) o baja por el registro, despegándose del final.
    LogScroll {
        /// Líneas. El renderer manda las que su rueda o su tecla signifiquen.
        delta: i64,
    },
    /// Desplaza el visor ACOPLADO de un hueco (#291): la rueda sobre él. Las
    /// teclas no pasan por aquí — van por el keymap del visor cuando el hueco
    /// tiene el foco, como en la TUI.
    PreviewScroll {
        /// Qué hueco.
        slot_id: u32,
        /// Líneas, negativo hacia arriba.
        delta: i64,
    },
    /// Vuelve a pegar el registro al final y sigue lo que llega.
    LogFollow,
    /// Recorre la FUENTE del registro: los dos → esta ventana → el daemon
    /// (#328).
    ///
    /// UN mando y no tres, y sin parámetro: son tres estados de una misma
    /// pregunta —«¿de quién quiero leer?»— y un `set` con vocabulario abierto
    /// obligaría a validar en el host una cadena que el renderer no tiene
    /// motivo para componer.
    ///
    /// No hace nada visible cuando no hay una segunda fuente: entonces el
    /// renderer ni siquiera pinta el selector (`sources_available`).
    LogCycleSource,
    /// Cuántas filas de registro cabían en el último frame.
    ///
    /// La pone el renderer, como la ventana del listado: adivinarla en el host
    /// es lo que en la TUI hizo que cada página se saltara dos líneas y la
    /// primera cuatro, y lo que ninguna de las dos ventanas enseñaba no se
    /// podía leer de ninguna manera.
    LogSetVisibleRange {
        /// Filas visibles. Cero se trata como una.
        rows: u32,
    },
    /// La ventana ganó o perdió el foco del escritorio (#285).
    ///
    /// El host lo necesita para no avisar por fuera de lo que ya se está
    /// viendo: con la ventana delante, la barra y el tablero cuentan lo mismo
    /// que contaría una notificación, y duplicarlo es ruido.
    ///
    /// Se asume ENFOCADA mientras nadie diga lo contrario: un renderer que no
    /// mande esto se comporta como antes de #285 —avisa siempre— en vez de
    /// callarse, que sería perder avisos sin que nadie lo note.
    WindowFocus {
        /// `true` si la ventana está delante.
        focused: bool,
    },
    /// El lector eligió un directorio en el selector del ESCRITORIO, o lo
    /// cerró sin elegir (#284).
    ///
    /// La ruta viene del renderer, así que se trata como todo lo que viene de
    /// ahí: se valida, y sobre todo se ENSEÑA en la confirmación antes de
    /// tocar nada. Lo que NO viene en este mensaje es qué se copia — eso sigue
    /// siendo del estado del host, que es la regla de ADR 0069.
    DirectoryPicked {
        /// La ruta NATIVA elegida, o `None` si se cerró el selector. Es texto
        /// del sistema de ficheros, no un `VPath`: convertirla es del host.
        path: Option<String>,
    },
    /// Un programa que se corrió esperándolo (`NativeEffect::RunProgram`)
    /// terminó (#312): lo que imprimió, en bruto. El host lo enmascara,
    /// lo parte en líneas y lo acota antes de enseñarlo — es texto de otro
    /// programa sobre ficheros que nombró cualquiera.
    ProgramFinished {
        /// La clave del título que viajó en el efecto.
        title_key: String,
        /// El argv que corrió, ya en texto para decirlo (lossy: es para
        /// enseñarlo, no para volver a correrlo).
        command: String,
        /// stdout y stderr, en ese orden, hasta el tope del que hospeda.
        output: Vec<u8>,
        /// Quien hospeda cortó la salida.
        truncated: bool,
        /// No arrancó, o se pasó del plazo.
        failed: bool,
    },
    /// El lector SOLTÓ ficheros del escritorio sobre la ventana (#283).
    ///
    /// Solo entra: arrastrar hacia FUERA no se ofrece, porque eso es publicar
    /// las rutas de lo marcado a cualquier aplicación que acepte el drop, y
    /// ese es otro diseño (ADR 0074).
    ///
    /// Las rutas vienen de OTRO proceso —el emisor compone la lista a mano si
    /// quiere—, así que no se copia nada por recibirlas: abren la misma
    /// confirmación que copiar, con los nombres enmascarados. Un drop es un
    /// gesto sin confirmación por naturaleza y esta ventana pregunta antes de
    /// escribir; la pregunta es justamente lo que acota que la lista sea
    /// ajena.
    FilesDropped {
        /// Rutas NATIVAS de esta máquina, tal cual las manda el escritorio.
        /// Texto del sistema de ficheros, no `VPath`: convertirlas es del
        /// host, y la que no convierta se descarta diciéndolo.
        paths: Vec<String>,
    },
    /// Teclea en el campo de texto del diálogo abierto.
    DialogInput {
        /// Diálogo.
        id: ModalId,
        /// Texto completo tras la edición (no un delta: el renderer es dueño
        /// del caret, y mandar el texto entero evita reconstruirlo en Rust).
        text: String,
    },
    /// Elige una fila del panel de diferencias, POR SU ID.
    ///
    /// Por id y no por índice: un filtro esconde filas y las renumeraría, y
    /// la selección tiene que seguir nombrando la misma.
    CompareSelectRow {
        /// El id que la fila trajo.
        id: u64,
    },
    /// Abre la fila elegida: navega al directorio del lado ACTIVO.
    CompareActivateRow {
        /// El id de la fila.
        id: u64,
    },
    /// Enseña o esconde una categoría entera del panel de diferencias.
    CompareToggleFilter {
        /// Id estable de la categoría (`same`, `different`…).
        category: String,
    },
    /// Dice qué ventana de filas está pintando el renderer.
    ///
    /// La comparación no tiene tope —un tope convertiría «¿son iguales?» en
    /// media respuesta— así que lo que cruza el puente es una ventana, y esto
    /// es lo que la mueve.
    CompareSetVisibleRange {
        /// Índice, entre las VISIBLES, de la primera fila pintada.
        first: u64,
        /// Cuántas caben.
        count: u32,
    },
    /// Pide cancelar una task.
    CancelTask {
        /// Id de la task.
        task_id: u64,
    },
    /// El tamaño de la ventana cambió.
    ///
    /// En CELDAS de layout, no en píxeles: los mínimos de cada panel están
    /// declarados así y se comparten con el TUI, de modo que «esto no cabe»
    /// significa lo mismo en las dos superficies. Redimensionar reparte otra
    /// vez; jamás reescribe la disposición guardada, que es la intención del
    /// usuario y no una función del tamaño de su ventana.
    SetViewport {
        /// Ancho en celdas.
        width: u16,
        /// Alto en celdas.
        height: u16,
    },
    /// Una tecla.
    ///
    /// El renderer manda la tecla NORMALIZADA y nada más: quién resuelve un
    /// contador, un prefijo a medias o qué comando lleva ligado es Rust, con
    /// el mismo resolver y los mismos presets que el TUI. Dos keymaps serían
    /// dos sitios donde divergir sin que nadie lo note.
    Key(KeyInput),
    /// Cuántas líneas caben en el visor.
    ///
    /// El host no puede saberlo: su rejilla son celdas de disposición y el
    /// cromo del visor lo pinta el renderer. Adivinarlo hacía dos cosas mal a
    /// la vez —mandar más líneas de las que caben, que se recortan sin
    /// decirlo, y avanzar una página por un número distinto del que se ve—,
    /// así que cada página saltaba en silencio lo recortado.
    SetViewerRows {
        /// Líneas visibles.
        rows: u32,
    },
    /// Cuántas CELDAS de ancho tiene el cuerpo del visor, medidas por quien
    /// pinta. Es lo que se le dice al previewer (proto 0.66.0) la próxima
    /// vez que se abra: el viewport entero contaba el cromo, y una imagen
    /// encogida a él se salía por la derecha.
    SetViewerCols {
        /// Celdas de ancho del cuerpo.
        cols: u32,
    },
    /// Pone el cursor de la lateral de la ayuda en esa fila y ENSEÑA lo que
    /// haya (un click).
    ///
    /// Enseñar y no navegar, que es lo que hace la misma tecla de flecha:
    /// recorrer el índice no debe dejarle al lector un paso de vuelta que
    /// tenga que deshacer con `⌫` antes de poder cerrar. Una cabecera de
    /// grupo y una fila fuera de rango no hacen nada.
    HelpSelectTopic {
        /// Fila de la lateral, tal como viajó en el orden de `sidebar`.
        row: u32,
    },
    /// Actúa sobre una fila ejecutable del cuerpo de la ayuda (un click):
    /// corre el comando, o abre la página enlazada.
    ///
    /// Va por el MISMO camino que `enter`, y ese por el mismo que una tecla:
    /// la ayuda es otra puerta al catálogo, no un segundo despachador.
    HelpActivate {
        /// Índice dentro de `actions`.
        index: u32,
    },
    /// Pone el cursor de los ajustes en esa fila (un click).
    ///
    /// Solo mueve. Esta ventana no edita ajustes todavía, así que no hay una
    /// acción para activar una fila: no habría nada que activar.
    SettingsSelectRow {
        /// Fila, contando TODAS las de todas las secciones en orden.
        row: u32,
    },
    /// Pone delante la pestaña de este hueco (un click).
    SelectTab {
        /// El hueco que hay dentro de la pestaña elegida.
        slot_id: u32,
    },

    /// Elige una sesión de agente por posición (un click).
    AgentSelectRow {
        /// Fila dentro de la lista pintada.
        row: u32,
        /// La generación de la lista que el renderer estaba pintando.
        ///
        /// La lista cambia SIN gesto —una petición de permiso la reordena—,
        /// así que un clic contra la de antes elige otra fila. Fuera de
        /// generación se rehúsa: aquí «esta fila» es de quién se deshace el
        /// trabajo.
        generation: u64,
    },

    /// Elige una extensión del gestor (un click) y pide su ficha.
    ExtensionSelectRow {
        /// Fila, en el orden en que viajaron.
        row: u32,
    },
    /// Pone el cursor de un selector en esa fila (un click).
    PickerSelectRow {
        /// Fila, en el orden en que viajaron.
        row: u32,
        /// La generación con la que se pintó esa fila. El selector de
        /// volúmenes se abre vacío y se llena después: misma carrera.
        generation: u64,
    },
    /// Elige una fila de la barra lateral de sitios (un click) y la ACTIVA:
    /// navega a ella, o pliega su sección si es una cabecera.
    ///
    /// Selecciona y activa a la vez, al contrario que las otras listas: una
    /// barra lateral existe para ir a sitios, y un click que solo mueve un
    /// cursor obliga a rematar con el teclado.
    PlaceActivateRow {
        /// Fila, en el orden en que viajaron.
        row: u32,
        /// La generación con la que se pintó esa fila.
        ///
        /// Obligatoria porque esta lista CAMBIA sola: los volúmenes llegan de
        /// una tarea de fondo y se insertan antes que los favoritos, así que
        /// un índice sin generación puede nombrar una fila que ya no es la
        /// que se pulsó. Una que no case se rechaza.
        generation: u64,
    },
    /// Elige una rama del árbol (un click) y NAVEGA a ella: el listado
    /// enfocado va a ese directorio, y la rama queda desplegada.
    ///
    /// Desplegar *y* navegar, las dos: quien pulsa sobre una rama quiere ver
    /// qué hay dentro, y verlo en el listado es la respuesta completa. El
    /// árbol se queda donde está, que es lo que hace útil tenerlo abierto.
    TreeActivateRow {
        /// Fila, en el orden en que viajaron.
        row: u32,
        /// La generación con la que se pintó. Obligatoria por lo mismo que en
        /// la barra de sitios: desplegar pide un listado, y ese listado inserta
        /// filas EN MEDIO cuando llega.
        generation: u64,
    },
    /// Pliega o despliega la rama, sin navegar a ninguna parte.
    TreeToggleRow {
        /// Fila, en el orden en que viajaron.
        row: u32,
        /// La generación con la que se pintó.
        generation: u64,
    },
    /// Elige una disposición del selector (un click) y la APLICA.
    LayoutActivateRow {
        /// Fila, en el orden en que viajaron.
        row: u32,
    },
    /// Elige un resultado de la búsqueda (un click) y VA a él: el panel
    /// navega a su directorio y el cursor queda encima.
    ///
    /// El renderer manda un ÍNDICE, nunca una ruta: la ruta exacta la tiene
    /// el host desde que el daemon la mandó, y reconstruirla desde un texto
    /// pintado es como se acaba abriendo otro fichero.
    SearchActivateRow {
        /// Fila, en el orden en que viajaron.
        row: u32,
    },
    /// Contesta a la revisión de un plan de renombrado: aplicarlo o
    /// descartarlo.
    ///
    /// Existe además de las teclas porque la revisión se abre SOLA y se queda
    /// el teclado: sin ella, la única forma de contestar era una tecla, y un
    /// lector con el ratón no podía ni quitársela de encima. Y a diferencia de
    /// una tecla, un clic en un botón es un gesto DIRIGIDO a esta pantalla —
    /// no puede ser una tecla que iba a otro sitio.
    AiRenameDecide {
        /// `true` = aplicar. `false` = descartar.
        approve: bool,
    },
    /// Despliega un menú de la barra por su índice, o cierra el que hubiera
    /// si ya era ese (un click en el título abierto lo pliega).
    MenuOpen {
        /// Qué menú, en el orden en que viajaron sus títulos.
        menu: u32,
    },
    /// Mueve el cursor dentro del menú desplegado (el ratón por encima).
    MenuPointRow {
        /// Qué entrada, en el orden en que viajaron.
        row: u32,
    },
    /// Ejecuta una entrada del menú desplegado (un click).
    ///
    /// Lleva la fila y no el comando: lo que el renderer sabe es dónde pulsó
    /// el lector, y el comando lo resuelve el host contra el menú que él
    /// mismo tiene abierto. Un id de comando que viniera del renderer sería
    /// un despachador paralelo al keymap (ADR 0069).
    MenuActivateRow {
        /// Qué entrada, en el orden en que viajaron.
        row: u32,
    },
    /// Cierra el menú desplegado sin ejecutar nada (un click fuera).
    MenuClose,
    /// Pulsa un botón de la barra de paneles (#324, puente 51): abre el
    /// panel si está cerrado y lo cierra si está abierto.
    ///
    /// Lleva el índice y no el comando, por lo mismo que el menú: el host
    /// resuelve el botón contra la barra que él mismo mandó, y el panel se
    /// abre por el MISMO despacho que su atajo (ADR 0069, ADR 0077).
    PanelBarActivate {
        /// Qué botón, en el orden en que viajaron.
        button: u32,
    },
    /// Arrastra el borde que hay entre `slot_id` y el hueco de al lado.
    ///
    /// `cells` es DÓNDE está el puntero en el eje del reparto, en celdas de
    /// layout — no un tamaño ni un delta. El renderer sabe convertir píxeles a
    /// celdas porque ya lo hace para declarar su viewport; lo que significa
    /// esa posición —qué pareja se reparte, cuánto le toca a cada uno, qué
    /// mínimos hay— lo decide el host con el reparto que él mismo calculó
    /// (ADR 0069).
    ResizeSlot {
        /// El hueco de la IZQUIERDA del borde (o el de ARRIBA).
        slot_id: u32,
        /// La posición del puntero en el eje del reparto, en celdas.
        cells: u16,
    },
    /// Elige una fila del selector de PERFILES y la activa (un click).
    ///
    /// Selecciona y activa a la vez, como la barra lateral: un selector de
    /// perfiles existe para cambiar de perfil, y un click que solo mueve un
    /// cursor obliga a rematar con el teclado.
    ProfileActivateRow {
        /// Fila, en el orden en que viajaron.
        row: u32,
        /// La generación con la que se pintó. La lista se llena desde una
        /// tarea de fondo: sin esto, un índice nombra otro perfil.
        generation: u64,
    },
    /// Pide un snapshot completo: el renderer perdió el hilo de la secuencia.
    Resync,
}
