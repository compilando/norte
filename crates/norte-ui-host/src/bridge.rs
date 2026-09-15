//! El sobre en el que viaja TODO lo que cruza al renderer, y sus topes.
//!
//! Un renderer —una webview, un shell Flutter, un test headless— no comparte
//! memoria con el host: recibe mensajes. Este módulo dice cómo son esos
//! mensajes y qué se le exige a cada uno, que es lo que hace que un renderer
//! escrito por otro no pueda interpretar medio mensaje y seguir como si nada.

use serde::{Deserialize, Serialize};

/// Versión del contrato del bridge.
///
/// No es la del protocolo del daemon: son dos fronteras distintas y se mueven
/// por motivos distintos. Un renderer que no reconoce esta versión NO
/// interpreta el mensaje: enseña una pantalla de incompatibilidad (ADR 0066).
///
/// **La regla de cuándo se mueve el número está en el ADR 0068**, sección
/// «And the version rule, written down», y es de UN SOLO NIVEL: cualquier
/// cambio de forma —también uno puramente aditivo— sube el número, y no hay
/// nivel compatible. Es deliberado y tiene precondiciones escritas: un nivel
/// compatible solo es honesto si un peer viejo puede decodificar un payload
/// nuevo, lo que exige `#[serde(default)]` en cada campo añadido y un
/// renderer que se resincronice ante un parche que no conoce en vez de
/// tirarlo. Ninguna de las dos se cumple hoy, y poner los `default` sin la
/// otra dejaría que un payload viejo decodificara como uno nuevo con la
/// pantalla a medias — que es peor que una que dice que no sabe leerla.
/// Reabrir la regla es un ADR nuevo, no un parche aquí.
///
/// - **50**: un fragmento de preview lleva FONDO (`SpanView::bg`, proto
///   0.66.0, D4): el previewer de imagen pinta medios bloques con dos píxeles
///   por celda, y sin fondo la mitad de la imagen no existe. Y el host manda
///   al previewer el ancho del visor en celdas para que encoja a medida.
/// - **49**: el visor lleva los FRAGMENTOS de una preview de plugin
///   (`ViewerView::styled`): texto, rol del tema o color propio, una entrada
///   por fila de `lines`. La TUI pintaba roles y colores desde ADR 0037 y la
///   ventana los aplanaba a texto; ahora los dos frontends enseñan lo mismo.
/// - **48**: el panel de registro lee TAMBIÉN el del daemon (#328). El hueco
///   dice qué fuente se está enseñando (`source_mode`), si hay de verdad una
///   segunda que ofrecer (`sources_available`) y qué hay que decir sobre ella
///   (`source_note`); cada línea dice de qué proceso salió, y `log_cycle_source`
///   recorre las tres.
/// - **47**: un hueco en ERROR se puede reintentar por si solo
///   (`refresh_slot`). El caso corriente al reabrir es una conexion remota que
///   pide su contrasena, y sin nada que pulsar la unica salida era navegar a
///   otro sitio para poder volver.
/// - **46**: la pantalla puede llevar el PANEL DE REGISTRO (#326): la ventana
///   visible del anillo en memoria, con su nivel, su filtro y si sigue el
///   final. Dice de qué PROCESO son las líneas, porque la ventana arranca su
///   propio daemon y las suyas no son las de él.
/// - **45**: un diálogo puede pedir una CONTRASEÑA (#327). El campo se marca
///   como secreto y lo que viaja del host al renderer son PUNTOS, jamás el
///   texto: el renderer no lo pinta, no lo resiembra y no lo puede registrar.
/// - **44**: el renderer puede ARRASTRAR el borde entre dos huecos. Manda
///   dónde está el puntero en celdas de layout; qué pareja se reparte y
///   cuánto le toca a cada uno lo decide el host, que es quien tiene el
///   reparto y los mínimos.
/// - **43**: la pantalla puede llevar el selector de PERFILES (ADR 0079), con
///   el activo marcado, lo que no carga dicho por su motivo, y los dos avisos
///   que la spec pide por su nombre: qué otra cosa se llama igual y qué perfil
///   no puede guardar estado.
/// - **42**: la pantalla del tema ELIGE: lleva la lista de temas y el cursor,
///   y moverse por ella previsualiza en vivo. Antes solo enseñaba el que
///   había, porque quien hospeda resolvía el tema una vez al arrancar; ahora
///   el host se lo dice por el canal nativo y el catálogo vuelve a cruzar.
/// - **41**: la pantalla lleva la BARRA DE MENÚS —los títulos, el menú
///   desplegado y sus entradas con su atajo— y el renderer puede desplegar,
///   señalar, ejecutar y cerrar. Los menús son `norte_frontend::menu`, el
///   mismo modelo que pinta el TUI.
/// - **25**: la revisión de un plan dice lo que le faltaba para poder
///   aprobarse a conciencia: cuánto se ve de cuánto hay y cuántos renombrados
///   hará DE VERDAD —los dos ya traducidos, porque el catálogo no sustituye
///   variables—, si hay un nombre alterado FUERA de la ventana, y si el
///   lector ha recorrido el plan entero. Y se puede contestar con el ratón.
/// - **36**: un listado dice cuántas entradas está APARTANDO por ocultas, de
///   forma permanente y no como mensaje que la siguiente tecla pisa. Un
///   listado que enseña menos de lo que hay no puede quedarse mudo (#107,
///   #293).
/// - **35**: la disposición lleva sus grupos de PESTAÑAS —qué hay detrás de
///   lo que se pinta, con el rótulo de cada una y su bandera—, porque una
///   pestaña inactiva no se coloca y sin esto la ventana enseñaba la de
///   delante sin decir que había otras abiertas.
/// - **34**: el panel de agentes lleva GENERACIÓN —la lista se reordena sola,
///   así que un clic tiene que decir contra cuál habla—, cuántas sesiones se
///   han olvidado por el tope, qué decir cuando está vacío (que no es siempre
///   lo mismo), y si una sesión ya tiene un deshacer en marcha.
/// - **33**: la ventana lleva las sesiones de AGENTE que ha visto pedir
///   permiso, con cuántas pidió cada una y cuántas se le aprobaron desde
///   aquí. Es de donde sale el operando del deshacer de una sesión entera
///   (#276): elegido de una lista, jamás tecleado.
/// - **32**: lo que las revisiones de la 6.4 cambiaron de forma: la salida de
///   un comando viaja por LÍNEAS y con una bandera por cadena —quién, qué y lo
///   impreso, cada uno con la suya, más el id reverse-DNS de la extensión—, y
///   una fila de la paleta dice si lo pintado difiere de lo que declara quien
///   la aporta.
/// - **31**: el gestor de extensiones GOBIERNA: la ficha lleva el editor de
///   `[config]` (qué clave está elegida, qué se está tecleando y qué claves
///   este build sabe editar), los comandos que aporta la extensión, y la
///   salida del último que se ejecutó.
/// - **30**: el panel de sincronización APLICA: la segunda pregunta, los
///   fallos del informe y el acuse de la cancelación cruzan, y el ancla de una
///   ruta puede valer `either`. Un renderer de 29 leería `undefined` donde
///   ahora hay una lista.
/// - **29**: la pantalla puede llevar un PLAN de sincronización: sus pasos con
///   la perspectiva del deshacer, su resumen, lo que lo bloquea y si se puede
///   aprobar.
/// - **28**: la pantalla puede llevar el panel de DIFERENCIAS: filas
///   emparejadas por el core, sus filtros por categoría y una ventana de las
///   que se ven.
/// - **27**: una búsqueda puede ser SEMÁNTICA, y entonces sus filas llevan
///   cuánto se parecen.
/// - **26**: lo que se enmascara se DICE también en los avisos persistentes,
///   en el diagnóstico de una disposición, en el nombre de un kind sin
///   proyectar, en las claves de efectos de un tema y en la etiqueta de una
///   tecla; y una aprobación dice qué pide, quién lo pide y hasta cuándo, en
///   campos propios.
/// - **24**: la pantalla puede llevar un PLAN DE RENOMBRADO en revisión: las
///   parejas que el modelo propone (de nombre a nombre, cada uno entero y por
///   separado — nunca concatenados con una flecha), el veredicto del core y
///   sus colisiones. Llega en dos tiempos: primero el plan, y el veredicto
///   después, porque comprobarlo contra el directorio es otro viaje.
/// - **23**: un diálogo deja de ser texto plano. Su cuerpo son LÍNEAS
///   ([`crate::dto::DialogLine`]), cada una diciendo si lo pintado difiere de
///   lo real; el DESTINO viaja en su propio campo y no como una línea con una
///   flecha, porque un directorio puede llamarse `docs → /casa/BORRAR` y esa
///   flecha es legítima; y si el cuerpo enseña menos elementos de los que la
///   operación toca, lo dice. Una task dice también si el fichero que lleva
///   en curso se pinta distinto de lo que es.
/// - **22**: el visor dice si lo que enseña es una IMAGEN pintable y cuánto
///   dice medir, o por qué se niega a pintarla. Sus bytes NO viajan en la
///   foto: se piden aparte (ADR 0069).
/// - **21**: el visor dice si lo que enseña lo produjo un PLUGIN, y si la
///   decodificación que se le dio fue con pérdida.
/// - **20**: la pantalla puede llevar el SELECTOR DE COLUMNAS: qué se pinta,
///   en qué orden, con qué formato y sobre qué esquema.
/// - **19**: un listado dice cuántas entradas se SALTÓ el provider.
/// - **18**: una fila puede llevar la INSIGNIA que un plugin le puso, con el
///   rol del tema con el que pintarla, y una columna `plugin:` trae su valor.
///   Se piden solo para la VENTANA visible.
/// - **17**: la barra lateral y el selector llevan GENERACIÓN, y un click con
///   una que no case se rechaza. Rompe: los volúmenes llegan de una tarea de
///   fondo y se insertan en medio de la lista, así que un índice desnudo
///   podía navegar a un sitio que nadie pulsó.
/// - **16**: la pantalla puede llevar una BÚSQUEDA por el subárbol, con sus
///   hallazgos llegando en lotes mientras corre.
/// - **15**: la pantalla puede llevar el SELECTOR DE DISPOSICIONES, con la
///   forma de cada una pintada por el mismo motor que reparte la de verdad.
/// - **14**: un hueco puede ser la BARRA LATERAL DE SITIOS.
/// - **13**: un hueco puede ser la HOJA DE ATRIBUTOS o el PANEL DE PROCESOS,
///   en vez de un rectángulo gris con el nombre de su tipo.
/// - **12**: la pantalla puede llevar el TEMA por dentro (rol a rol, con los
///   efectos que este renderer no pinta) y el SELECTOR DE VOLÚMENES.
/// - **11**: la pantalla puede llevar el GESTOR DE EXTENSIONES en solo
///   lectura: qué hay instalado, qué pide cada una y qué se le ha
///   configurado.
/// - **10**: la pantalla puede llevar los AJUSTES en solo lectura: el
///   registro compartido con su valor efectivo, y dónde vive cada cosa.
/// - **9**: la pantalla puede llevar la AYUDA: el corpus en bloques cerrados,
///   con sus marcas vivas ya resueltas contra el keymap del lector, y la hoja
///   de teclado generada del mapa efectivo.
/// - **8**: la pantalla puede llevar la PALETA de comandos.
/// - **7**: un prefijo a medias lleva sus CONTINUACIONES (qué teclas siguen,
///   qué hace cada una y cuáles no se pueden hacer aquí).
/// - **6**: las cabeceras y el visor viajan como PARCHE, y el renderer
///   declara cuántas líneas caben en el visor.
/// - **5**: toda acción que nombra una fila lleva TAMBIÉN la generación en
///   la que el renderer la vio, y el host la compara con la época del
///   listado. Sin ese par la clave es un índice, y un índice de la pantalla
///   anterior nombra otro fichero.
/// - **4**: la pantalla puede llevar un VISOR (texto decodificado en líneas,
///   o hexadecimal si el contenido es binario).
/// - **3**: cada listado lleva sus CABECERAS (etiqueta traducida, columna
///   que ordena y sentido), y se puede pedir orden por columna.
/// - **2**: el snapshot lleva el reparto de la pantalla
///   ([`crate::dto::LayoutView`]) y va COMPLETO (diálogos y tablero
///   incluidos); un cambio de foco viaja como parche y no como foto.
/// - **1**: el contrato inicial de la fase 2.
///
/// - **51**: el snapshot lleva la BARRA DE PANELES (#324): botones derivados
///   del registro de kinds, con estado y novedad, y una acción por índice
///   para pulsarlos. Va también como parche (`ViewChange::PanelBar`) en
///   cualquier envío que la cambie.
/// - **52**: la SALIDA DE UN PROGRAMA (#312): lo que imprimió un programa
///   que quien hospeda corrió esperándolo —el comparador de dos ficheros—,
///   como foto y como parche, y la acción con la que quien hospeda la
///   devuelve.
/// - **53**: el renderer declara cuántas COLUMNAS tiene el cuerpo del visor
///   (`SetViewerCols`), como ya declaraba las filas: es el ancho que el
///   previewer recibe.
/// - **54**: un diálogo de transferencia dice en qué punto está la
///   COMPROBACIÓN DE SU DESTINO ([`crate::dto::DestCheckView`]): si cabe
///   (#149) y si sabe sujetar sus escrituras (#164). Sube el número aunque el
///   campo lleve `serde(default)`, y esa es la razón de subirlo: un renderer
///   viejo emparejado con este host no conoce el campo, no pintaría la línea
///   de #164 y no lo diría — y la ausencia de esa línea SIGNIFICA que el
///   destino confina. El webview va embebido en el binario, así que ese
///   emparejamiento es lo que sale de olvidarse de `just link-gui`.
/// - **55**: la cabecera de un listado lleva las CUATRO marcas que le
///   faltaban y que el terminal tiene desde siempre: que se está rellenando
///   —y cuántas van—, que los nombres se reinterpretan (#57), que un refresco
///   se comió marcas, y cuántas hay marcadas y cuánto pesan. Todas bajo la
///   misma regla: un listado que enseña menos de lo que hay, o que no enseña
///   lo que hay, jamás es silencioso.
/// - **56**: un hueco que está CARGANDO dice a dónde va (#323). El cuerpo
///   sigue enseñando el listado anterior hasta que llegue el nuevo —a
///   propósito, para que un fallo deje al lector donde estaba—, y sin el
///   destino esa mezcla no se puede leer. El umbral de 250 ms lo pone el
///   renderer, que es donde un retardo puramente visual no cuesta nada.
/// - **57**: el parche del tablero de tasks lleva TAMBIÉN qué fila del panel
///   de procesos está elegida. Viaja con el tablero por lo mismo que
///   `total_rows` viaja con las filas de un listado: es la extensión de lo que
///   va al lado y las dos se mueven a la vez. Una task que caduca a los diez
///   segundos quita una fila y desplaza el resto, y antes ese cursor solo
///   viajaba en la foto entera — o sea que el panel resaltaba la fila N, que
///   ya era otra tarea o ninguna, mientras la tecla de cancelar actuaba sobre
///   la que el host tiene acotada. Resaltar una y parar otra es la avería.
/// - **58**: un diálogo con la lista recortada dice si alguna de las que NO
///   enseña se pintaría alterada. El badge de una ruta visible dice «lo que
///   lees no son los bytes que hay»; sobre lo recortado no se puede decir eso
///   —no está delante—, pero sí que ahí fuera hay algo así, que es lo que
///   decide si merece la pena ampliar antes de aprobar. El terminal lo decía
///   en su resumen desde siempre y esta ventana no, sobre las mismas rutas.
/// - **59**: el visor dice cuánto hay A LO ANCHO (`total_cols`) y por dónde va
///   (`first_col`). El visor no envuelve, así que sin esto un HTML minificado
///   se pintaba recortado y la ventana no tenía con qué dibujar una barra
///   horizontal: un fichero cortado por la derecha se leía como un fichero
///   corto. Las `lines` ya vienen recortadas —el recorte lo hace el modelo
///   compartido, una sola vez— y estos dos campos son la otra mitad: qué se ve
///   y cuánto hay. Con ellos llega `viewer_scroll`, que es la RUEDA sobre el
///   visor: una rueda no es una tecla, y fabricar flechas para expresarla
///   dejaba el gesto atado a que nadie reatara esas flechas.
/// - **60**: los ajustes (F11) se ESCRIBEN desde la ventana. `SettingsView`
///   pierde `read_only`, que era una promesa de fase 4 y ya no es verdad; y
///   llega `settings_activate`, el doble clic sobre una fila, que hace lo que
///   `enter`: girar lo que gira y pedir en un diálogo lo que se teclea. El
///   editor es el compartido con el terminal (`norte_frontend::settings`), y
///   lo escrito se relee y se aplica por el mismo camino que un cambio de
///   perfil.
/// - **61**: el gestor de extensiones (F12) se GOBIERNA con el ratón.
///   Llegan `extension_govern` —aprobar o revocar, encender o apagar, y
///   desinstalar (ADR 0104), la fila señalada y el mismo camino que el
///   verbo del teclado, preguntas incluidas— y `extension_help`, la página
///   de ayuda de una extensión. Ningún DTO cambia: lo que la ventana pinta
///   con botones ya viajaba.
/// - **62**: la columna de iconos (ADR 0105). `RowView` gana `icon` e
///   `icon_hostile`: lo que un decorador de hueco `icon` puso, a la
///   IZQUIERDA del nombre; la insignia sigue a la derecha, y los dos
///   coexisten. `BrowserSlotView` y el parche `rows` ganan `icon_column`:
///   si la columna está abierta lo decide el host desde el listado entero,
///   no el renderer desde las filas que ve, o desplazarse a una página sin
///   iconos la cerraría y correría todos los nombres.
/// - **63**: la ola de usabilidad (spec 2026-09-10), en UN salto.
///   `PaletteRowView.recent`: la fila va arriba por ser de las últimas
///   lanzadas, solo con la consulta vacía. `PanelBarView.names`: si los
///   botones enseñan su nombre (`[ui] panel_bar_style`) o solo la letra.
///   `BrowserSlotView.footer` y `BrowserHeader.footer`: el pie del listado
///   (cuentas, marcado, espacio libre), ya redactado; vacío con
///   `[ui] pane_footer` apagado. `ViewSnapshot.key_bar` y el cambio
///   `key_bar`: la barra de teclas de función, derivada del keymap de la
///   pantalla que tiene el teclado; `key_bar_activate` la pulsa y el host
///   sintetiza la tecla. `StatusView.notices_unread`: avisos que caducaron
///   (`[ui] notice_seconds`) sin que nadie abriera el registro; la
///   insignia abre el registro por su botón de la barra de paneles.
///   `ViewSnapshot.wizard`, el cambio `wizard` y las acciones `wizard_open`
///   y `wizard_activate_row`: el asistente de primer arranque, que el
///   renderer pide cuando el catálogo dice `first_run` y el host escribe
///   por el camino de los ajustes.
/// - **64**: anchos de columna (spec 2026-09-11, V2). `ColumnHeader` gana
///   `width` —el ancho FIJO en celdas que `[ui.columns] spec.width`
///   configura, `None` para `auto`/`flex`— y `align`, la alineación
///   configurada, la misma que el terminal aplica. Llega `resize_column`:
///   arrastrar el borde de una cabecera fija el ancho de esa columna en
///   memoria y en el `norte.toml`, y vuelve la cabecera de todos los
///   huecos, porque el ancho es de la columna y no del hueco.
/// - **65**: migas e indicador de espacio (spec 2026-09-11, V5).
///   `BrowserSlotView` y `BrowserHeader` ganan `path_segments` —la raíz y
///   un tramo por directorio, cada uno enmascarado— y `used_ratio`, cuánto
///   del volumen está ocupado. Llega `breadcrumb_activate { slot_id,
///   depth, generation }`: navega al directorio con los primeros `depth`
///   tramos, por profundidad y no por nombre, porque un tramo enmascarado
///   no vuelve a ser un nombre; con la generación del listado que pintó
///   las migas, para que una miga rancia no se reinterprete sobre otra
///   ruta. `ColumnHeader.width` de la columna `name` pasa a ser su suelo.
/// - **66**: el tema colorea las ENTRADAS (spec 2026-09-11). `RowView` gana
///   `name_color` —el `#rrggbb` que `[files.ext]` (gana) o `[files.kind]`
///   dan al nombre, vacío si el tema no dice nada— y `name_bold`,
///   `name_dim`, `name_italic`, `name_underline`. Viajan RESUELTOS y no como
///   nombre de regla porque las extensiones son un conjunto ABIERTO: un tema
///   colorea las que quiera, así que el renderer no puede tener clases para
///   ellas, al revés que con `badge_role`. Cierra una divergencia con el
///   terminal que llevaba desde que existe la ventana: `[files.kind]` y
///   `[files.ext]` —la mitad de lo que declara un fichero de tema— no se
///   pintaban, y un listado monocromo no se lee como un tema pobre, se lee
///   como un tema roto.
///
///   De los seis atributos de `norte_theme::Style` cruzan CUATRO. `bg` y
///   `reverse` se quedan fuera a propósito: el fondo de una fila ya lo
///   disputan el cursor, el hover y la marca, y un quinto dueño dejaría que
///   el tema tapase dónde está el cursor. Un tema que pinte `bg` en
///   `[files.ext]` lo verá en el terminal y no aquí, y eso está escrito
///   también en `docs/theming.md`.
/// - **67**: el esquema del escritorio cruza. Llega `set_color_scheme {
///   dark }`, que el renderer manda al arrancar y en cada cambio de
///   `prefers-color-scheme`.
///
///   Hace falta por el puente 66 y no antes: las VARIABLES CSS de la
///   variante (V6) las enchufa el renderer por su cuenta y de forma
///   síncrona, para no parpadear con la paleta equivocada, así que hasta
///   ahora el host no necesitaba saber el esquema. Con el color de la
///   entrada cocido en la fila sí: con `theme_dark = "vscode-dark"` y
///   `theme_light = "vscode-light"`, pasar el escritorio a claro repintaba
///   el cromo con la variante clara y dejaba los NOMBRES con los colores de
///   la oscura — `dir` en #4daafc sobre blanco, 2,6:1, por debajo del suelo
///   que esos presets prometen en su cabecera.
///
///   La regla «la variante de ese lado si la hay, y `theme` si no» queda
///   escrita en los dos lados (`themeFor` en `ui/src/main.ts`,
///   `HostTheme::para_esquema` en Rust) porque cada uno necesita una cosa
///   distinta —variables el renderer, `Theme` entero el host, que es el
///   único que puede resolver `[files.ext]`—. Lo que impide que diverjan es
///   `la_regla_de_variante_es_la_del_renderer`, que pinea los tres casos.
/// - **68**: llega `menu_toggle`, el Alt pulsado y soltado solo. Pliega el
///   menú abierto o lo abre como `app.menu`, y no hace nada con una pantalla
///   que se queda las teclas delante. Cruza como acción propia porque un
///   modificador solo no es un chord del keymap.
/// - **69**: una extensión que no cargó es una fila del gestor.
///   `ExtensionErrorView` lleva `id` —el de su directorio, si se llama como
///   uno— y el cursor de `ExtensionsView` sigue detrás de `rows` por
///   `errors`. `extension_select_row` y `extension_govern` nombran esas filas
///   por la misma cuenta, y sobre una rota el único cambio que se atiende es
///   `uninstall`. ADR 0104 lo había dejado escrito como hueco: el handler la
///   borraba y la ventana no tenía cómo pedírselo.
pub const BRIDGE_VERSION: u32 = 69;

/// Tope de una cadena que cruza al renderer, en bytes.
///
/// Todo lo pintable está acotado en Rust y no en el renderer: un nombre
/// hostil de 700 KB no puede convertirse en el problema de quien pinta.
pub const MAX_STRING_BYTES: usize = 4096;

/// Filas que puede llevar UN mensaje.
pub const MAX_ROWS_PER_BATCH: usize = 2048;

/// Avisos vivos a la vez; los más viejos se caen.
pub const MAX_NOTICES: usize = 32;

/// Tasks proyectadas a la vez.
pub const MAX_TASKS: usize = 256;

/// Tasks RETENIDAS a la vez, proyectadas o no (#271).
///
/// [`MAX_TASKS`] acota lo que cruza el puente; esto acota lo que el host
/// guarda. No son el mismo número porque no son la misma pregunta: una fila
/// que se cae de la proyección sigue teniendo un progreso que bombear y un
/// directorio que relistar cuando termine, y tirarla por no caber en la
/// pantalla perdería el refresco.
///
/// El desalojo de `registrar_task` solo puede tirar tasks TERMINALES, así que
/// sin este segundo tope un lote de tres mil copias encoladas —ninguna
/// terminal todavía— retenía las tres mil. 512 es lo que un daemon acepta
/// vivas a la vez (`MAX_LIVE_TASKS`), o sea el techo real del otro lado.
pub const MAX_TASKS_RETAINED: usize = 512;

/// Entradas que admite UNA transferencia (#271).
///
/// `pane.copy` opera sobre las marcas, y marcar no tiene tope: un lote se
/// encolaba entero y se descubría el límite cuando el daemon empezaba a
/// rechazar por `MAX_LIVE_TASKS`, o sea a mitad, con la mitad hecha y sin
/// nada que dijera dónde se cortó. Decirlo ANTES es más honesto que
/// descubrirlo a medias.
pub const MAX_TRANSFER_BATCH: usize = 512;

/// Diálogos apilados a la vez.
///
/// La pila era de gestos humanos y por eso no tenía techo. Desde la tarea 5.3
/// la alimenta el WIRE: una aprobación por cada op de agente, y un informe
/// por cada lote o undo terminal que dejó algo a medias —también los de otro
/// cliente de la misma sesión—. Otro frontend corriendo doscientos lotes
/// atascados apilaba doscientos diálogos, cada uno pidiendo dos respuestas, y
/// cada parche de diálogos CLONA la pila entera.
///
/// Ocho es lo que una persona puede contestar sin perder el hilo; al llegar
/// al techo se cae el más viejo NO reconocido —lo que nadie ha llegado a
/// mirar— y jamás el de arriba, que es el que se está contestando.
pub const MAX_DIALOGS: usize = 8;

/// Bytes de una previsualización que cruzan al renderer.
pub const MAX_PREVIEW_BYTES: usize = 256 * 1024;

/// Identidad de UNA instancia del host.
///
/// Ordena todo lo demás: una `sequence` solo significa algo dentro de la
/// instancia que la emitió, y una acción que llega con otra instancia es de
/// una vida anterior del host (un reattach tras reiniciar) y no muta nada.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstanceId(String);

impl InstanceId {
    /// Construye la identidad. La fabrica el host al arrancar.
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// La cadena opaca.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// La clave de una FILA, opaca para el renderer.
///
/// Solo vale dentro de `(instancia, hueco, generación)`. Cuando el host
/// re-lista, la generación sube: un click que llega con la anterior se
/// responde [`StaleAction::Generation`] y no hace nada. Es lo que impide que
/// un doble click tardío actúe sobre el fichero que ocupó esa fila DESPUÉS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RowKey(pub u64);

/// La identidad de un diálogo abierto.
///
/// Confirmar es idempotente por esto: un segundo `Confirm` con el mismo id no
/// vuelve a lanzar la operación, y uno con un id viejo no cierra el diálogo
/// que hay AHORA.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModalId(pub u64);

/// El testigo de una petición en vuelo.
///
/// NO cruza al renderer: es la contabilidad interna del host para descartar
/// en Rust la respuesta de algo que ya no interesa. Vive en este módulo por
/// vecindad histórica, y se queda porque moverlo sería un cambio de nombres
/// sin lector; que no está en el cable lo dice el corpus, donde no aparece.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestToken(pub u64);

/// El sobre de todo mensaje del host hacia el renderer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeEnvelope<T> {
    /// Versión del contrato ([`BRIDGE_VERSION`]).
    pub bridge_version: u32,
    /// Quién emite.
    pub instance_id: InstanceId,
    /// Orden dentro de esta instancia. Empieza en 0 y no salta.
    pub sequence: u64,
    /// Lo que se envía.
    pub payload: T,
}

impl<T> BridgeEnvelope<T> {
    /// Mete `payload` en un sobre de ESTA versión.
    pub fn new(instance_id: InstanceId, sequence: u64, payload: T) -> Self {
        Self {
            bridge_version: BRIDGE_VERSION,
            instance_id,
            sequence,
            payload,
        }
    }

    /// ¿Puede este renderer interpretar el sobre?
    ///
    /// Es una pregunta de todo o nada a propósito: media interpretación de un
    /// contrato que no se conoce es peor que una pantalla que lo dice.
    ///
    /// ```
    /// use norte_ui_host::{BridgeEnvelope, InstanceId, BRIDGE_VERSION};
    ///
    /// let mut e = BridgeEnvelope::new(InstanceId::new("host-1"), 0, 7u32);
    /// assert!(e.is_supported());
    /// e.bridge_version = BRIDGE_VERSION + 1;
    /// assert!(!e.is_supported(), "una versión futura NO se interpreta");
    /// ```
    #[must_use]
    pub fn is_supported(&self) -> bool {
        self.bridge_version == BRIDGE_VERSION
    }
}

/// Por qué una acción no hizo nada, sin que sea un error.
///
/// Las tres son carreras normales entre un renderer que pinta y un host que
/// ya cambió de estado, y ninguna es culpa de nadie: se responden y se
/// ignoran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleAction {
    /// La acción venía de OTRA instancia del host.
    ///
    /// RESERVADA: hoy es inalcanzable, porque las acciones viajan sin sobre
    /// y por tanto sin instancia. Un renderer que sobreviva a un reinicio del
    /// host es la situación que la justifica, y entonces habrá que envolver
    /// también la dirección de entrada. Se declara para que el renderer que
    /// la reciba algún día ya sepa qué significa.
    Instance,
    /// La fila (o el hueco) es de una generación anterior: hubo un re-listado.
    Generation,
    /// El diálogo al que responde ya no está abierto.
    Modal,
}

/// La respuesta a una acción.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ActionAck {
    /// Aplicada. `sequence` es la primera actualización que la refleja.
    Applied {
        /// La actualización en la que se verá.
        sequence: u64,
    },
    /// No se aplicó, y no pasa nada: ver [`StaleAction`].
    Stale {
        /// Cuál de las tres carreras fue.
        reason: StaleAction,
    },
    /// La acción no está disponible AHORA (comando atenuado, sin permiso,
    /// sin conexión). Lleva la clave Fluent del motivo, no la frase: quien
    /// traduce es el renderer con el catálogo del host.
    Unavailable {
        /// Clave Fluent del porqué.
        reason_key: String,
    },
}

/// Recorta una cadena al tope del bridge sin partir un clúster.
///
/// Se DICE (`…`), que es la misma regla que el resto del proyecto aplica a lo
/// pintable: nunca se pierde algo en silencio.
///
/// ```
/// use norte_ui_host::bridge::{clamp_display, MAX_STRING_BYTES};
///
/// // Lo que cabe viaja intacto.
/// assert_eq!(clamp_display("café.txt".to_owned()), "café.txt");
///
/// // Lo que no, se recorta Y se marca.
/// let largo = clamp_display("a".repeat(MAX_STRING_BYTES * 2));
/// assert!(largo.len() <= MAX_STRING_BYTES);
/// assert!(largo.ends_with('…'));
/// ```
#[must_use]
pub fn clamp_display(s: String) -> String {
    if s.len() <= MAX_STRING_BYTES {
        return s;
    }
    // El recorte es el COMPARTIDO. Una frontera de carácter no basta: corta
    // dentro de un clúster y deja una marca combinante huérfana que se
    // compone con el `…`. Esa regla ya estaba resuelta y probada contra el
    // corpus en `norte-frontend`; tener aquí una segunda era tener dos.
    norte_frontend::display::ellipsis_at_bytes(&s, MAX_STRING_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_sobre_de_otra_version_no_se_interpreta() {
        let mut e = BridgeEnvelope::new(InstanceId::new("i"), 0, 7u32);
        assert!(e.is_supported());
        e.bridge_version = BRIDGE_VERSION + 1;
        assert!(!e.is_supported(), "una versión futura NO se interpreta");
    }

    #[test]
    fn una_cadena_larga_se_recorta_y_se_dice() {
        let larga = "a".repeat(MAX_STRING_BYTES * 2);
        let out = clamp_display(larga);
        assert!(out.len() <= MAX_STRING_BYTES);
        assert!(out.ends_with('…'), "el recorte se ve");
    }

    /// El recorte jamás parte un carácter multibyte por la mitad.
    #[test]
    fn el_recorte_respeta_los_caracteres() {
        let larga = "é".repeat(MAX_STRING_BYTES);
        let out = clamp_display(larga);
        assert!(out.len() <= MAX_STRING_BYTES);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    /// Una cadena que ya cabe no se toca (ni se le añade el aviso).
    #[test]
    fn lo_que_cabe_viaja_intacto() {
        let s = String::from("café.txt");
        assert_eq!(clamp_display(s.clone()), s);
    }
}
