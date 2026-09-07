// El contrato con el host, en TypeScript.
//
// Es una TRANSCRIPCIÓN de `crates/norte-ui-host/src/{bridge,dto,action}.rs`, y
// no una segunda definición: quien manda es Rust. Lo que impide que se separen
// en silencio es `tests/contract.test.ts`, que lee el MISMO corpus golden que
// clava el lado Rust (`crates/norte-ui-host/tests/golden/*.json`).
//
// Aquí no hay lógica. Ni un comparador, ni un formateador, ni una regla de
// disponibilidad: eso vive en Rust (ADR 0066, decisión D14).

/** La versión del contrato que este renderer sabe leer. */
export const BRIDGE_VERSION = 53;

export type RowKey = number;
export type ModalId = number;

export interface BridgeEnvelope<T> {
  bridge_version: number;
  instance_id: string;
  sequence: number;
  payload: T;
}

export type ConnectionView =
  | { state: "connected" }
  | { state: "reconnecting" }
  | { state: "lost"; reason_key: string };

export type SlotRole = "active" | "target";

export interface SlotPlacement {
  slot_id: number;
  x: number;
  y: number;
  width: number;
  height: number;
  role: SlotRole | null;
  focus_index: number;
}

export interface LayoutView {
  cells: [number, number];
  placements: SlotPlacement[];
  /** Los grupos de PESTAÑAS que hay en pantalla. Aparte de los placements
   *  porque una pestaña inactiva no se coloca —no se pinta su contenido— y
   *  aun así hay que enseñar que está: una ventana con tres pestañas que solo
   *  muestra la de delante esconde trabajo abierto. */
  tabs: TabGroupView[];
}

export interface TabGroupView {
  /** El hueco COLOCADO al que pertenece el grupo: el de la pestaña activa. */
  slot_id: number;
  tabs: TabView[];
  /** Cuál está delante, como índice en `tabs`. */
  active: number;
}

export interface TabView {
  /** El hueco de dentro. Es lo que vuelve al elegirla con el ratón. */
  slot_id: number;
  /** Su rótulo, ya enmascarado: un directorio hostil dentro de una pestaña es
   *  tan hostil como dentro de un listado. */
  title: string;
  title_hostile: boolean;
}

export type RowKind = "dir" | "file" | "symlink" | "other";

export interface CellView {
  column: string;
  text: string | null;
}

export interface RowView {
  key: RowKey;
  display_name: string;
  hostile: boolean;
  kind: RowKind;
  selected: boolean;
  marked: boolean;
  cells: CellView[];
  /** La insignia que un plugin puso, ya enmascarada. Vacía = ninguna. */
  badge: string;
  /** La insignia se pinta DISTINTO de lo que es: la escribe un plugin. */
  badge_hostile: boolean;
  /**
   * El rol del tema con el que pintarla (`warning`, `error`…). Vacío =
   * ninguno. Es un vocabulario CERRADO: un plugin no elige su propio color.
   */
  badge_role: string;
}

export type SlotState =
  | { state: "ready" }
  | { state: "loading" }
  | { state: "error"; reason_key: string; detail: string | null };

export interface QuickView {
  query: string;
  mode: string;
  matches: number;
}

export interface ColumnHeader {
  id: string;
  label: string;
  sort: "asc" | "desc" | null;
  sortable: boolean;
}

export interface BrowserSlotView {
  kind: "browser";
  slot_id: number;
  generation: number;
  path_display: string;
  path_hostile: boolean;
  total_rows: number | null;
  first_visible: number;
  rows: RowView[];
  cursor: RowKey | null;
  marks: number;
  /**
   * Lo que el provider se SALTÓ, ya dicho en el idioma del lector. Vacío =
   * ninguna, o el provider no lleva la cuenta.
   */
  skipped_note: string;
  /**
   * Cuántas entradas aparta la ocultación, ya dicho. Vacío = ninguna. Va
   * en la cabecera y es PERMANENTE: un listado que enseña menos de lo que
   * hay no puede quedarse mudo en cuanto el lector cambie de tecla.
   */
  hidden_note: string;
  columns: ColumnHeader[];
  state: SlotState;
  quick: QuickView | null;
}

export interface UnsupportedSlotView {
  kind: "unsupported";
  slot_id: number;
  kind_name: string;
  kind_name_hostile: boolean;
}

export interface MetadataFieldView {
  label: string;
  value: string;
  hostile: boolean;
}

export interface MetadataSlotView {
  kind: "metadata";
  slot_id: number;
  fields: MetadataFieldView[];
  note: string;
  /** La ruta del listado al que esta hoja SIGUE. Va en el título. */
  follows_display: string;
  /** Esa ruta difiere de los bytes reales. */
  follows_hostile: boolean;
}

/**
 * El visor ACOPLADO (#291, puente 51): el fichero bajo el cursor del listado
 * al que este hueco sigue, leído solo, como en la TUI.
 */
export interface PreviewSlotView {
  kind: "preview";
  slot_id: number;
  /** El visor con lo leído, o `null` si no hay fichero que enseñar. */
  viewer: ViewerView | null;
  /** Por qué no hay fichero, YA DICHO: un directorio, nada bajo el cursor,
   *  un error de lectura. Vacío cuando hay visor. */
  note: string;
}

export interface ProcessesSlotView {
  kind: "processes";
  slot_id: number;
  cursor: number | null;
}

export interface LogLineView {
  /** `HH:MM:SS`, en UTC — este árbol no lleva base de datos de husos. */
  time: string;
  /** Vocabulario CERRADO: error, warn, info, debug, trace. Se colorea por él. */
  level: string;
  target: string;
  message: string;
  /** Lo pintado difiere de lo que hay, en el módulo o en el mensaje. */
  hostile: boolean;
  /** De qué PROCESO salió: `window` o `daemon` (#328). En una lista mezclada
   *  es la mitad de la información: «el provider falló» y «la ventana no pudo
   *  pintarlo» se leen igual sin saber quién lo escribió. */
  source: string;
}

export interface LogSlotView {
  kind: "log";
  slot_id: number;
  /** Solo la VENTANA visible, nunca el anillo entero. */
  lines: LogLineView[];
  level: string;
  filter: string;
  /** Pegado al final y siguiendo lo que llega. */
  following: boolean;
  total: number;
  first_visible: number;
  /** Líneas perdidas, YA DICHAS y con el número dentro: un renderer no traduce
   *  ni sustituye números. Vacío = ninguna. Un registro con un agujero
   *  silencioso miente sobre lo que pasó. Con las dos fuentes a la vista son
   *  DOS cuentas nombradas y no una suma: la de la ventana cuenta desde que
   *  arrancó el proceso, la del daemon lo que esta apertura se perdió. */
  dropped_note: string;
  /** Qué anillo está CAPTURANDO más de lo que se enseña, ya traducido. Vacío =
   *  ninguno. Bajar lo que se ve no deja de capturar, así que el panel puede
   *  decir «info» mientras se guarda TRACE — y quien mira tiene derecho a
   *  saberlo antes de hacer una captura de pantalla. Aquí sale también el
   *  nivel del DAEMON, nombrándolo: el suyo es global a sus clientes y nunca
   *  baja, así que no puede ir en `level`, que es el que filtra la lista. */
  capturing: string;
  /**
   * De qué PROCESO son estas líneas, ya traducido. La ventana arranca su
   * propio daemon, así que hasta #328 aquí NO estaba lo del daemon —los
   * providers, el journal, la política—; callarlo haría que el panel pareciera
   * roto.
   */
  source: string;
  /** La fuente EFECTIVA, en vocabulario cerrado: `window`, `daemon` o `both`.
   *  Efectiva y no la preferencia: sin un segundo anillo al otro lado, `both`
   *  se enseña como `window`, porque eso es lo que se está mirando. */
  source_mode: string;
  /** Hay de verdad una segunda fuente que ofrecer. `false` = el selector NO se
   *  pinta: un mando entre tres vistas de un mismo anillo promete algo que no
   *  existe. */
  sources_available: boolean;
  /** Lo que hay que decir sobre la fuente, ya traducido. Vacío = nada. */
  source_note: string;
}

export type PlaceRowView =
  | { row: "header"; label: string; folded: boolean }
  | { row: "drive"; label: string; hostile: boolean; detail: string }
  | {
      row: "favorite";
      name: string;
      target: string;
      hostile: boolean;
      broken: string;
    };

export interface PlacesSlotView {
  kind: "places";
  slot_id: number;
  rows: PlaceRowView[];
  cursor: number;
  /**
   * Sube cada vez que cambia el conjunto de filas. Va de vuelta en el click:
   * los volúmenes llegan de una tarea de fondo y se insertan EN MEDIO, así
   * que un índice sin generación puede nombrar la fila de al lado.
   */
  generation: number;
}

export interface TreeRowView {
  /** El nombre del directorio. La raíz lleva su ruta entera. */
  label: string;
  hostile: boolean;
  /** Niveles por debajo de la raíz. La raíz es 0. */
  depth: number;
  expanded: boolean;
  /**
   * Tiene hijos que enseñar. `null` = todavía no se ha mirado, y son tres
   * cosas distintas para quien lee: rama que se abre, hoja, y sin leer.
   * Pintar «hoja» a algo que no se ha leído es una respuesta inventada.
   */
  children: boolean | null;
}

export interface TreeSlotView {
  kind: "tree";
  slot_id: number;
  rows: TreeRowView[];
  cursor: number;
  /**
   * Sube cada vez que cambia el conjunto de ramas. Va de vuelta en el click,
   * por lo mismo que en la barra de sitios: desplegar una rama pide su
   * listado, y ese listado inserta filas EN MEDIO cuando llega.
   */
  generation: number;
}

export type SlotView =
  | BrowserSlotView
  | PlacesSlotView
  | TreeSlotView
  | MetadataSlotView
  | PreviewSlotView
  | ProcessesSlotView
  | LogSlotView
  | UnsupportedSlotView;

export interface PendingView {
  chords: string;
  count: number | null;
}

export interface StatusView {
  message: string | null;
  banners: BannerView[];
  pending: PendingView | null;
}

/** Un aviso persistente: la frase por un lado y la conexión por otro. */
export interface BannerView {
  text: string;
  subject: BannerSubjectView | null;
}

/** De qué conexión habla un aviso. Cada parte en su campo: montar
 *  `scheme://host` dentro de la frase deja que un host se lea como userinfo
 *  de otro. */
export interface BannerSubjectView {
  scheme: string;
  host: string;
  /** Por qué está degradada, ya traducido por el host. Un motivo que el host
   *  no conoce dice «motivo desconocido» y no hereda la frase del que sí. */
  reason: string;
  /** El detalle del wire, ya enmascarado y acotado por el host. Solo viene
   *  con un motivo desconocido. */
  detail?: string;
  hostile: boolean;
}

export interface DialogChoice {
  id: string;
  label_key: string;
  destructive: boolean;
}

/** Una línea del cuerpo de un diálogo: lo que se pinta, y si difiere de lo real. */
export interface DialogLine {
  text: string;
  hostile: boolean;
}

export interface DialogView {
  id: ModalId;
  title_key: string;
  /** A dónde va la operación. Campo propio: un nombre de directorio puede
   *  contener una flecha, así que etiquetar con un separador dentro del texto
   *  deja que una ruta simule otra. */
  destination: DialogLine | null;
  /** QUÉ se pregunta (la op de un agente). Fuera del cuerpo, por lo mismo
   *  que el destino. */
  subject: DialogLine | null;
  /** QUIÉN pregunta, si no es quien está delante. */
  asker: DialogLine | null;
  /** Cuándo deja de aceptarse la respuesta, ya traducido. */
  deadline: string | null;
  /** Cuándo vence, en epoch-ms, para poder contar de verdad. Ausente = no hay
   *  plazo o no se conoce, y entonces la frase se pinta tal cual. */
  deadline_at_ms?: number;
  /** Cuando son rutas, se numeran POR POSICIÓN: la etiqueta es estructural y
   *  ningún nombre de fichero puede escribirla. */
  body: DialogLine[];
  /** El cuerpo enseña menos de lo que la operación toca, ya traducido. */
  overflow_note: string;
  choices: DialogChoice[];
  input: string | null;
  input_hostile: boolean;
  /** El campo es una CONTRASEÑA (#327). Lo que llega en `input` son PUNTOS,
   *  uno por carácter, jamás el texto: el host guarda lo tecleado aparte, en
   *  un buffer que se pisa con ceros al soltarlo. El renderer pinta el campo
   *  como `password` y NUNCA lo resiembra con `input` — hacerlo convertiría
   *  la contraseña del usuario en una fila de puntos literales. */
  input_secret: boolean;
}

/** Una pareja del plan: de qué nombre a qué nombre. */
export interface AiRenamePairView {
  from: DialogLine;
  to: DialogLine;
}

/** El plan de renombrado que un modelo propuso, en revisión. */
export interface AiRenameView {
  dir: DialogLine;
  /** La ventana que viaja, NO el plan entero. */
  pairs: AiRenamePairView[];
  first_visible: number;
  total: number;
  /** Cuánto se ve de cuánto hay, ya traducido. Vacío = se ve todo. */
  more_note: string;
  /** Fuera de la ventana hay un nombre que se pinta distinto de lo que es. */
  hidden_hostile: boolean;
  /** El veredicto del core, ya traducido. */
  status: string;
  /** Maquinaria y colisiones, cada línea con su marca. */
  detail: DialogLine[];
  /** Aprobar puede hacer algo. Lo dice el core. */
  confirmable: boolean;
  /** Cuántos renombrados hará DE VERDAD, ya dicho y traducido. */
  real_steps_note: string;
  /** El lector ha recorrido el plan entero. Aprobar lo exige. */
  seen_all: boolean;
}

export type TaskStateView = "queued" | "running" | "done" | "failed" | "cancelled";

export interface TaskView {
  task_id: number;
  kind: string;
  state: TaskStateView;
  percent: number | null;
  detail: string | null;
  detail_hostile: boolean;
  foreign: boolean;
}

export interface ViewerView {
  path_display: string;
  path_hostile: boolean;
  encoding: string;
  eol: string;
  hex: boolean;
  forced: boolean;
  had_errors: boolean;
  truncated: boolean;
  total_rows: number;
  first_line: number;
  lines: string[]; /** «via ‹plugin›», ya traducido. Vacío = lo enseña norte, no un plugin. */
  preview_by: string;
  /** La decodificación que se le dio al previewer fue con PÉRDIDA. */
  preview_lossy: boolean;
  /**
   * Es una imagen PINTABLE y así de grande dice ser. `null` = no lo es, o es
   * una que el host se niega a pintar (y entonces lo dice en
   * `image_refused`). Los bytes NO vienen aquí: se piden aparte.
   */
  image: ImageView | null;
  /** Por qué NO se pinta una imagen reconocida, ya traducido. */
  image_refused: string;
  /**
   * Las líneas visibles CON ESTILO cuando lo que se enseña lo produjo un
   * previewer (puente 49): una entrada por fila de `lines`. Vacío en la
   * vista cruda, y entonces se pinta `lines`.
   */
  styled: SpanView[][];
}

/**
 * Un fragmento de una línea de preview con estilo. `role` GANA sobre `fg`
 * cuando vienen los dos: el tema del lector manda sobre el color fijo de un
 * plugin. El rol ya viene validado por el host.
 */
export interface SpanView {
  text: string;
  role: string | null;
  fg: string | null;
  /** El FONDO, `#rrggbb` (puente 50): medios bloques de un previewer de imagen. */
  bg: string | null;
}

/**
 * Una imagen reconocida y aceptada. El tamaño es el que DECLARA su cabecera:
 * nadie la ha decodificado todavía, y eso es el punto — el declarado es lo
 * que el host comparó con su presupuesto (ADR 0069).
 */
export interface ImageView {
  /** Reconocido por bytes MÁGICOS, jamás por la extensión. */
  format: string;
  width: number;
  height: number;
}

export interface PaletteRowView {
  text: string;
  desc: string;
  chord: string;
  enabled: boolean;
  /** Lo pintado DIFIERE de lo que declara quien aporta la fila. Solo puede
   *  ser cierto en una fila de PLUGIN, y esta es la pantalla donde se elige
   *  qué código de tercero correr. */
  hostile: boolean;
}

export interface PaletteView {
  query: string;
  rows: PaletteRowView[];
  cursor: number | null;
  total: number;
}

export interface ProfileRowView {
  name: string;
  name_hostile: boolean;
  title: string | null;
  active: boolean;
  /** Qué OTRA cosa se llama igual, ya traducido. Vacío = solo es un perfil. */
  clash: string;
  /** No puede guardar dónde dejaste cada panel (nombre no UTF-8). */
  no_state: boolean;
  /** Por qué no se puede cargar. Vacío = se puede. */
  problem: string;
}

export interface ProfilePickerView {
  rows: ProfileRowView[];
  cursor: number;
  generation: number;
}

export interface MenuItemView {
  label: string;
  chord: string;
  /** Esta ventana puede ejecutarla. Una apagada SIGUE saliendo: el menú es
   *  donde se ve qué existe. */
  enabled: boolean;
}

export interface MenuView {
  /** `[ui] menu_bar`: si la barra se pinta. Apagada, el menú sigue
   *  abriéndose por su tecla. */
  bar: boolean;
  titles: string[];
  /** Cuál está desplegado, si alguno. */
  open: number | null;
  /** Las entradas del desplegado; vacías si no hay ninguno. */
  items: MenuItemView[];
  cursor: number;
}

/** Cómo está el panel de un botón de la barra (puente 51). */
export type PanelButtonState = "closed" | "open" | "focused";

/** Un botón de la barra de paneles. */
export interface PanelButtonView {
  /** El kind que abre, ya enmascarado: acaba en un atributo del DOM. */
  kind: string;
  /** El nombre corto, en el idioma de la sesión. */
  label: string;
  /** La letra que la TUI pinta; aquí acompaña a la etiqueta. */
  letter: string;
  /** El atajo que hace lo mismo, o `—`. */
  chord: string;
  state: PanelButtonState;
  /** Tiene algo que contar sin estar a la vista. */
  attention: boolean;
}

/** La barra de paneles (#324, puente 51): qué paneles hay y cómo están. */
export interface PanelBarView {
  /** `[ui] panel_bar`: si la barra se pinta. */
  bar: boolean;
  /** Un click vuelve como el ÍNDICE aquí, nunca como un comando. */
  buttons: PanelButtonView[];
}

export interface WhichKeyRowView {
  chord: string;
  label: string;
  enabled: boolean;
  opens_sequence: boolean;
  reason: string;
}

export interface WhichKeyView {
  title: string;
  rows: WhichKeyRowView[];
}

export type HelpSpanView =
  | { span: "text"; text: string }
  | { span: "strong"; text: string }
  | { span: "emph"; text: string }
  | { span: "code"; text: string }
  | { span: "command"; text: string; is_chord: boolean }
  | { span: "link"; text: string };

export interface HelpKeyRowView {
  chord: string;
  label: string;
  label_hostile: boolean;
  enabled: boolean;
  reason: string;
}

export type HelpBlockView =
  | { block: "heading"; level: number; text: string }
  | { block: "paragraph"; spans: HelpSpanView[] }
  | { block: "bullets"; items: HelpSpanView[][] }
  | { block: "code"; lang: string | null; text: string }
  | { block: "table"; header: string[]; rows: string[][] }
  | { block: "callout"; kind: "note" | "warn" | "tip"; spans: HelpSpanView[] }
  | { block: "keys"; rows: HelpKeyRowView[] };

export type HelpSidebarRowView =
  { row: "group"; label: string } | { row: "topic"; title: string; current: boolean };

export interface HelpActionView {
  label: string;
  chord: string;
  enabled: boolean;
  reason: string;
  opens_topic: boolean;
}

export interface HelpView {
  title: string;
  topic_id: string;
  badge: string | null;
  sidebar: HelpSidebarRowView[];
  cursor: number;
  focus: "topics" | "body";
  blocks: HelpBlockView[];
  actions: HelpActionView[];
  action_cursor: number | null;
  filter: string;
  filtering: boolean;
  can_back: boolean;
}

export interface SettingRowView {
  id: string;
  name: string;
  desc: string;
  /** Ya enmascarado: sale del `norte.toml` que escribe el usuario. */
  value: string;
  /** El valor se pinta DISTINTO de lo que es. */
  hostile: boolean;
  restart_required: boolean;
}

export interface PathRowView {
  label: string;
  display: string;
  hostile: boolean;
  missing: boolean;
}

export type SettingsSectionView =
  | { section: "settings"; title: string; rows: SettingRowView[] }
  | { section: "paths"; title: string; rows: PathRowView[] };

export interface SettingsView {
  sections: SettingsSectionView[];
  cursor: number;
  read_only: boolean;
}

export interface ExtensionRowView {
  id: string;
  name: string;
  publisher: string;
  version: string;
  category: string;
  description: string;
  approved: boolean;
  enabled: boolean;
  has_help: boolean;
  commands: number;
  columns: number;
  capabilities: string[];
}

export interface ExtensionErrorView {
  dir: string;
  hostile: boolean;
  reason: string;
  /** El motivo se pinta distinto de lo que es: puede citar el manifiesto. */
  reason_hostile: boolean;
}

export interface ExtensionConfigRowView {
  key: string;
  kind: string;
  /** Ya enmascarado: lo escribe el plugin. */
  value: string;
  /** Ya enmascarado: lo escribe el plugin. */
  default: string;
  description: string;
  /** Ya enmascarado: los valores de un `enum` los escribe el plugin. */
  domain: string;
  /** Alguno de los tres se pinta DISTINTO de lo que es. */
  hostile: boolean;
  /** Este build sabe editar este `kind`. Un tipo de un peer más nuevo es de
   *  solo lectura: ofrecer `Enter` sobre lo que no va a cambiar hace creer
   *  que la escritura falló. */
  editable: boolean;
}

export interface ExtensionCommandView {
  /** Clave de despacho. NUNCA se pinta: el manifiesto no le valida charset. */
  id: string;
  title: string;
  hostile: boolean;
}

export interface ExtensionDetailView {
  id: string;
  config: ExtensionConfigRowView[];
  /** Los comandos que aporta, en orden de manifiesto. */
  commands: ExtensionCommandView[];
  /** Qué clave está elegida. */
  cursor: number;
  /** Lo que se está tecleando, YA enmascarado. `null` = no se edita nada. */
  editing: string | null;
  /** El buffer se pinta distinto de lo que se va a escribir. */
  editing_hostile: boolean;
}

/** Una cadena de tercero con su bandera AL LADO: una bandera suelta acaba
 *  describiendo a la cadena vecina. */
export interface MaskedTextView {
  text: string;
  hostile: boolean;
}

export interface AgentRowView {
  /** Su id, ya enmascarado: es una clave OPACA del daemon y puede llevar
   *  cualquier byte. Lo que viaja de vuelta es el crudo, no esto. */
  session: string;
  session_hostile: boolean;
  /** Cuántas pidió y cuántas se le aprobaron desde aquí, ya en una frase
   *  traducida: el catálogo que cruza no sustituye variables. */
  counts: string;
  /** Ya tiene un deshacer en marcha: se dice, y otro `u` se rehúsa. */
  undoing: boolean;
  last_op: string;
  last_op_hostile: boolean;
}

export interface AgentsView {
  rows: AgentRowView[];
  cursor: number;
  /** Cuántas veces ha cambiado esta lista. Vuelve con el clic: la lista se
   *  reordena SOLA —una petición de permiso sube a su sesión al primer
   *  puesto— y un clic contra la de antes elige otra fila. */
  generation: number;
  /** Cuántas sesiones se han olvidado por el tope. Se pinta cuando no es
   *  cero: una lista recortada que se presenta como completa es lo que
   *  convierte «inundar la lista» en «esa sesión no existe». */
  forgotten: number;
  /** Qué ES esta lista: lo visto por esta ventana, no el censo del sistema.
   *  Sin decirlo, una lista vacía se lee como «ningún agente ha tocado
   *  nada», que es una afirmación que esta ventana no puede hacer. */
  note: string;
  /** Qué decir cuando no hay filas, ya traducido: no es siempre lo mismo —
   *  una ventana sin efectos ni siquiera escucha las peticiones. */
  empty: string;
}

export interface ExtensionOutputView {
  /** De qué extensión: su nombre, ya enmascarado, con su bandera. */
  plugin: MaskedTextView;
  /** Su id reverse-DNS, que el core SÍ valida: el nombre no identifica. */
  plugin_id: string;
  /** Qué comando. `text` vacío si no se conocía su título. */
  command: MaskedTextView;
  /** Lo que imprimió, LÍNEA A LÍNEA, cada una enmascarada y acotada: un
   *  salto de línea es un control C0, así que enmascarar la salida entera
   *  marcaba como hostil cualquier salida de más de una línea. */
  lines: string[];
  /** Alguna línea se pinta distinta de lo que el plugin imprimió. */
  text_hostile: boolean;
  /** No cabía entera y se cortó. Viaja porque el receptor no puede
   *  deducirlo: el texto le llega ya corto. */
  truncated: boolean;
}

/** La salida de un programa que quien hospeda corrió esperándolo (#312). */
export interface ProgramOutputView {
  /** Clave Fluent del título: qué se hizo. */
  title_key: string;
  /** El programa y sus argumentos, ya enmascarados. */
  command: MaskedTextView;
  /** stdout y stderr, LÍNEA A LÍNEA, cada una enmascarada y acotada. */
  lines: string[];
  text_hostile: boolean;
  truncated: boolean;
  /** No arrancó, o se pasó del plazo. NO es «salió distinto de cero». */
  failed: boolean;
}

export interface ExtensionsView {
  rows: ExtensionRowView[];
  cursor: number;
  detail: ExtensionDetailView | null;
  loading: boolean;
  errors: ExtensionErrorView[];
}

export interface ThemeRoleView {
  role: string;
  color: string;
}

export interface ThemeView {
  name: string;
  roles: ThemeRoleView[];
  unsupported_effects: ThemeEffectView[];
  /** Entre qué temas se puede elegir. */
  choices: string[];
  /** Cuál está señalado. Moverse previsualiza en vivo. */
  cursor: number;
}

/** Un efecto declarado que este renderer no pinta. La clave sale del fichero
 *  de tema, así que va enmascarada y con su bandera. */
export interface ThemeEffectView {
  key: string;
  hostile: boolean;
}

export interface PickerRowView {
  label: string;
  hostile: boolean;
  detail: string;
}

export interface PickerView {
  title: string;
  rows: PickerRowView[];
  cursor: number | null;
  empty: string;
  /** Ver `PlacesSlotView.generation`: se abre vacío y se llena después. */
  generation: number;
}

export interface LayoutRowView {
  name: string;
  hostile: boolean;
  factory: boolean;
  shares_keymap_name: boolean;
  broken: boolean;
}

/**
 * El selector de COLUMNAS. Su título lleva ya el ALCANCE dentro —un esquema
 * o todos— y su nota dice que lo elegido vale para esta ventana y no se
 * guarda.
 */
export interface ColumnsPickerView {
  /** El pie con las teclas, ya pintado desde el keymap por el host. */
  hint: string;
  title: string;
  rows: ColumnsPickerRowView[];
  cursor: number;
  note: string;
}

export interface ColumnsPickerRowView {
  /** Su id de configuración. Identidad: entera o vacía. */
  id: string;
  /** Cómo se llama, ya traducido y saneado. */
  label: string;
  /** La etiqueta se pinta DISTINTA de lo que es. */
  hostile: boolean;
  enabled: boolean;
  /** Formato vigente, vocabulario ASCII cerrado. Vacío = no admite. */
  format: string;
  /** Lo fija un ajuste del esquema: aquí no se cicla. */
  format_locked: boolean;
  /** Ni se apaga ni se mueve. Es el NOMBRE. */
  fixed: boolean;
}

export interface LayoutPickerView {
  title: string;
  rows: LayoutRowView[];
  cursor: number;
  preview: string[];
  /** Por qué la elegida no tiene vista previa. CITA el fichero del usuario. */
  problem: string;
  /** El diagnóstico pintado difiere de lo que el fichero contiene. */
  problem_hostile: boolean;
}

export interface SearchRowView {
  name: string;
  hostile: boolean;
  parent: string;
  parent_hostile: boolean;
  is_dir: boolean;
  /** Cuánto se parece a lo que se preguntó, en `[-1, 1]`. `null` en una
   *  búsqueda por nombre: ahí no hay grados. */
  score: number | null;
}

/** El panel de diferencias: dos árboles comparados, fila a fila.
 *
 *  Ventana y no lista entera: el motor emite una fila por nombre emparejado
 *  de TODO el árbol y nada lo acota, así que viaja lo que se ve. */
/** El panel de sincronización: el PLAN, antes de que nada se escriba.
 *
 *  Ventana como el de diferencias: un plan de medio millón de pasos no cruza
 *  entero, y los pasos se nombran por su `id`. */
export interface SyncView {
  source: DialogLine;
  dest: DialogLine;
  /** `update` o `mirror`. Un espejo BORRA en el destino y una actualización
   *  no: se pinta antes de aprobar. */
  mode: string;
  steps: SyncStepView[];
  first_visible: number;
  total: number;
  /** El RESUMEN del plan: irreversibles, bytes, lo ilegible, y si la lista
   *  esconde pasos. Es lo que se lee antes de aprobar. */
  summary: string[];
  /** Lo que IMPIDE sincronizar, con su ruta. */
  blockers: SyncBlockerView[];
  /** Cuántos hay de verdad: el wire recorta la lista. */
  blockers_total: number;
  status: string;
  hint: string;
  /** La SEGUNDA pregunta, cuando el plan borra o deja algo sin vuelta atrás.
   *  Solo `y` la contesta que sí. */
  confirming: string | null;
  /** Los pasos que fallaron al aplicar. El recuento va en el estado. */
  failures: SyncFailureView[];
  /** Lo decide el modelo compartido: ofrecer aprobar lo que va a rechazar es
   *  la pantalla rota que esto evita. */
  can_approve: boolean;
  running: boolean;
  /** Ya se le pidió parar. El segundo `Escape` cierra el panel. */
  cancel_requested: boolean;
}

export interface SyncFailureView {
  cause: string;
  path: string;
  path_hostile: boolean;
  /** `source`, `dest` o `either`. HAY que pintarlo: callar un `either` en un
   *  panel donde una ruta sin calificar significa «del origen» es afirmar el
   *  origen. */
  anchor: string;
  /** El ancla YA DICHA, en el idioma de la sesión. Vacía cuando es el origen,
   *  que es lo que una ruta sin calificar significa aquí. */
  anchor_label: string;
}

export interface SyncBlockerView {
  label: string;
  /** Dónde. La raíz se dice «todo el árbol», no vacío. */
  path: string;
  path_hostile: boolean;
}

export interface SyncStepView {
  id: number;
  kind: string;
  reason: string;
  /** Si el deshacer lo devuelve. Nunca sale de `reversal` a secas. */
  undo: string;
  anchor: string;
  /** Como en el fallo: el ancla ya dicha, vacía cuando es el origen. */
  anchor_label: string;
  path: string;
  path_hostile: boolean;
  /** La ortografía del DESTINO cuando sus bytes difieren: la escritura cae
   *  sobre ESTA. */
  dest_path: string | null;
  dest_path_hostile: boolean;
  /** Las dos ortografías se rinden igual y hay que decirlo. */
  twins: boolean;
}

export interface CompareView {
  left: string;
  left_hostile: boolean;
  right: string;
  right_hostile: boolean;
  rows: CompareRowView[];
  first_visible: number;
  total: number;
  /** La fila elegida, POR SU ID: un filtro esconde filas, jamás las
   *  renumera. */
  selected: number | null;
  filters: CompareFilterView[];
  status: string;
  running: boolean;
}

export interface CompareFilterView {
  id: string;
  label: string;
  count: number;
  hidden: boolean;
}

export interface CompareRowView {
  id: number;
  verdict: string;
  category: string;
  confidence: string;
  criterion: string;
  reason: string | null;
  left: CompareFaceView | null;
  right: CompareFaceView | null;
  /** Por qué la fila enseña dos ortografías. Frase, no insignia pegada al
   *  nombre: lo que se pega a un nombre lo puede falsificar un nombre. */
  paired_under: string | null;
}

export interface CompareFaceView {
  name: string;
  hostile: boolean;
  /** Vacío cuando el provider no lo sabe: «no lo sé» y «cero bytes» son dos
   *  respuestas distintas. */
  size: string;
  mtime: string;
  is_dir: boolean;
}

export interface SearchView {
  /** Se preguntó por SIGNIFICADO contra el índice, no por nombre contra el
   *  árbol: el alcance es el índice entero y no `root`. */
  semantic: boolean;
  query: string;
  root: string;
  root_hostile: boolean;
  rows: SearchRowView[];
  cursor: number | null;
  status: string;
  running: boolean;
}

export interface ViewSnapshot {
  connection: ConnectionView;
  layout: LayoutView;
  slots: SlotView[];
  focus: number | null;
  status: StatusView;
  dialogs: DialogView[];
  tasks: TaskView[];
  menu: MenuView;
  panel_bar: PanelBarView;
  profiles: ProfilePickerView | null;
  palette: PaletteView | null;
  whichkey: WhichKeyView | null;
  help: HelpView | null;
  settings: SettingsView | null;
  extensions: ExtensionsView | null;
  agents: AgentsView | null;
  plugin_output: ExtensionOutputView | null;
  program_output: ProgramOutputView | null;
  theme: ThemeView | null;
  search: SearchView | null;
  compare: CompareView | null;
  sync: SyncView | null;
  layouts: LayoutPickerView | null;
  columns: ColumnsPickerView | null;
  picker: PickerView | null;
  viewer: ViewerView | null;
  ai_rename: AiRenameView | null;
  locale: string;
}

export type ViewChange =
  | { change: "cursor"; slot_id: number; generation: number; cursor: RowKey | null }
  | {
      change: "rows";
      slot_id: number;
      generation: number;
      first_visible: number;
      rows: RowView[];
    }
  | { change: "slot_state"; slot_id: number; state: SlotState }
  | ({ change: "status" } & StatusView)
  | { change: "tasks"; tasks: TaskView[] }
  | { change: "dialogs"; dialogs: DialogView[] }
  | ({ change: "connection" } & ConnectionView)
  | ({ change: "layout" } & LayoutView)
  | { change: "columns"; slot_id: number; columns: ColumnHeader[] }
  | { change: "viewer"; viewer: ViewerView | null }
  | { change: "ai_rename"; ai_rename: AiRenameView | null }
  | { change: "which_key"; whichkey: WhichKeyView | null }
  | { change: "menu"; menu: MenuView }
  | { change: "panel_bar"; panel_bar: PanelBarView }
  | { change: "profiles"; profiles: ProfilePickerView | null }
  | { change: "palette"; palette: PaletteView | null }
  | { change: "help"; help: HelpView | null }
  | { change: "settings"; settings: SettingsView | null }
  | { change: "extensions"; extensions: ExtensionsView | null }
  | { change: "agents"; agents: AgentsView | null }
  | { change: "plugin_output"; output: ExtensionOutputView | null }
  | { change: "program_output"; output: ProgramOutputView | null }
  | { change: "theme"; theme: ThemeView | null }
  | { change: "picker"; picker: PickerView | null }
  | { change: "layouts"; layouts: LayoutPickerView | null }
  | { change: "columns_picker"; columns: ColumnsPickerView | null }
  | { change: "search"; search: SearchView | null }
  | { change: "compare"; compare: CompareView | null }
  | { change: "sync"; sync: SyncView | null };

export interface ViewPatch {
  base_sequence: number;
  changes: ViewChange[];
}

export type UiNotice =
  | { notice: "message"; key: string; detail: string | null }
  | { notice: "shutdown"; incomplete: boolean }
  | { notice: "fatal"; key: string };

export type UiUpdate =
  | ({ update: "snapshot" } & ViewSnapshot)
  | ({ update: "patch" } & ViewPatch)
  | ({ update: "notice" } & UiNotice);

/** Una tecla ya normalizada. Quién la resuelve es Rust. */
export interface KeyInput {
  key: string;
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
}

export type UiAction =
  | { action: "move_cursor"; slot_id: number; delta: number }
  | { action: "select_row"; slot_id: number; key: RowKey; generation: number }
  | { action: "toggle_mark"; slot_id: number; key: RowKey; generation: number }
  | {
      action: "mark_range";
      slot_id: number;
      from: RowKey;
      to: RowKey;
      generation: number;
    }
  | { action: "activate"; slot_id: number; key: RowKey; generation: number }
  | { action: "parent"; slot_id: number }
  | { action: "history"; slot_id: number; back: boolean }
  | { action: "set_visible_range"; slot_id: number; first: number; count: number }
  | { action: "focus_slot"; slot_id: number }
  | { action: "sort_by"; slot_id: number; column: string }
  | {
      action: "dialog";
      id: ModalId;
      choice: string;
      /** La contraseña tecleada, SOLO en un diálogo con `input_secret`
       *  (#327). Va con la respuesta y no con cada pulsación: por
       *  `dialog_input` cruzarían `h`, `hu`, `hun`… y cada prefijo se queda
       *  en un trozo de heap que nadie pisa. Así cruza UNA vez, en el
       *  instante en que el lector decide entregarla. */
      secret?: string;
    }
  | { action: "dialog_input"; id: ModalId; text: string }
  | { action: "refresh_slot"; slot_id: number }
  | { action: "log_set_level"; level: string }
  | { action: "log_set_filter"; filter: string }
  | { action: "log_scroll"; delta: number }
  | { action: "preview_scroll"; slot_id: number; delta: number }
  | { action: "log_follow" }
  | { action: "log_cycle_source" }
  | { action: "log_set_visible_range"; rows: number }
  | { action: "cancel_task"; task_id: number }
  | { action: "compare_select_row"; id: number }
  | { action: "compare_activate_row"; id: number }
  | { action: "compare_toggle_filter"; category: string }
  | { action: "compare_set_visible_range"; first: number; count: number }
  | { action: "set_viewport"; width: number; height: number }
  | ({ action: "key" } & KeyInput)
  | { action: "set_viewer_rows"; rows: number }
  | { action: "set_viewer_cols"; cols: number }
  | { action: "help_select_topic"; row: number }
  | { action: "help_activate"; index: number }
  | { action: "settings_select_row"; row: number }
  | { action: "extension_select_row"; row: number }
  | { action: "agent_select_row"; row: number; generation: number }
  | { action: "select_tab"; slot_id: number }
  | { action: "picker_select_row"; row: number; generation: number }
  | { action: "place_activate_row"; row: number; generation: number }
  | { action: "tree_activate_row"; row: number; generation: number }
  | { action: "tree_toggle_row"; row: number; generation: number }
  | { action: "layout_activate_row"; row: number }
  | { action: "search_activate_row"; row: number }
  | { action: "ai_rename_decide"; approve: boolean }
  | { action: "menu_open"; menu: number }
  | { action: "menu_point_row"; row: number }
  | { action: "menu_activate_row"; row: number }
  | { action: "menu_close" }
  | { action: "panel_bar_activate"; button: number }
  | { action: "resize_slot"; slot_id: number; cells: number }
  | { action: "profile_activate_row"; row: number; generation: number }
  | { action: "resync" };

export type StaleReason = "instance" | "generation" | "modal";

export type ActionAck =
  | { status: "applied"; sequence: number }
  | { status: "stale"; reason: StaleReason }
  | { status: "unavailable"; reason_key: string };

/** Lo que el host proyecta UNA vez al arrancar: textos y colores, ya resueltos. */
export interface HostCatalog {
  bridge_version: number;
  instance_id: string;
  locale: string;
  /** Clave Fluent -> texto ya traducido EN RUST. */
  strings: Record<string, string>;
  /** Rol del tema -> variables CSS ya resueltas en Rust. */
  theme: Record<string, string>;
  /** Pasada de medición de la tarea 3.6 (`NORTE_GUI_MEASURE=1`). */
  measure: boolean;
}
