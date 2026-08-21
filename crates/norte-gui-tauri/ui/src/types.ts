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
export const BRIDGE_VERSION = 21;

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
  columns: ColumnHeader[];
  state: SlotState;
  quick: QuickView | null;
}

export interface UnsupportedSlotView {
  kind: "unsupported";
  slot_id: number;
  kind_name: string;
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
}

export interface ProcessesSlotView {
  kind: "processes";
  slot_id: number;
  cursor: number | null;
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

export type SlotView =
  | BrowserSlotView
  | PlacesSlotView
  | MetadataSlotView
  | ProcessesSlotView
  | UnsupportedSlotView;

export interface PendingView {
  chords: string;
  count: number | null;
}

export interface StatusView {
  message: string | null;
  banners: string[];
  pending: PendingView | null;
}

export interface DialogChoice {
  id: string;
  label_key: string;
  destructive: boolean;
}

export interface DialogView {
  id: ModalId;
  title_key: string;
  body: string[];
  choices: DialogChoice[];
  input: string | null;
  input_hostile: boolean;
}

export type TaskStateView = "queued" | "running" | "done" | "failed" | "cancelled";

export interface TaskView {
  task_id: number;
  kind: string;
  state: TaskStateView;
  percent: number | null;
  detail: string | null;
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
}

export interface PaletteRowView {
  text: string;
  desc: string;
  chord: string;
  enabled: boolean;
}

export interface PaletteView {
  query: string;
  rows: PaletteRowView[];
  cursor: number | null;
  total: number;
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
}

export interface ExtensionDetailView {
  id: string;
  config: ExtensionConfigRowView[];
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
  unsupported_effects: string[];
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
  problem: string;
}

export interface SearchRowView {
  name: string;
  hostile: boolean;
  parent: string;
  parent_hostile: boolean;
  is_dir: boolean;
}

export interface SearchView {
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
  palette: PaletteView | null;
  whichkey: WhichKeyView | null;
  help: HelpView | null;
  settings: SettingsView | null;
  extensions: ExtensionsView | null;
  theme: ThemeView | null;
  search: SearchView | null;
  layouts: LayoutPickerView | null;
  columns: ColumnsPickerView | null;
  picker: PickerView | null;
  viewer: ViewerView | null;
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
  | { change: "which_key"; whichkey: WhichKeyView | null }
  | { change: "palette"; palette: PaletteView | null }
  | { change: "help"; help: HelpView | null }
  | { change: "settings"; settings: SettingsView | null }
  | { change: "extensions"; extensions: ExtensionsView | null }
  | { change: "theme"; theme: ThemeView | null }
  | { change: "picker"; picker: PickerView | null }
  | { change: "layouts"; layouts: LayoutPickerView | null }
  | { change: "columns_picker"; columns: ColumnsPickerView | null }
  | { change: "search"; search: SearchView | null };

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
  | { action: "dialog"; id: ModalId; choice: string }
  | { action: "dialog_input"; id: ModalId; text: string }
  | { action: "cancel_task"; task_id: number }
  | { action: "set_viewport"; width: number; height: number }
  | ({ action: "key" } & KeyInput)
  | { action: "set_viewer_rows"; rows: number }
  | { action: "help_select_topic"; row: number }
  | { action: "help_activate"; index: number }
  | { action: "settings_select_row"; row: number }
  | { action: "extension_select_row"; row: number }
  | { action: "picker_select_row"; row: number; generation: number }
  | { action: "place_activate_row"; row: number; generation: number }
  | { action: "layout_activate_row"; row: number }
  | { action: "search_activate_row"; row: number }
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
