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
export const BRIDGE_VERSION = 6;

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
  columns: ColumnHeader[];
  state: SlotState;
  quick: QuickView | null;
}

export interface UnsupportedSlotView {
  kind: "unsupported";
  slot_id: number;
  kind_name: string;
}

export type SlotView = BrowserSlotView | UnsupportedSlotView;

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
  lines: string[];
}

export interface ViewSnapshot {
  connection: ConnectionView;
  layout: LayoutView;
  slots: SlotView[];
  focus: number | null;
  status: StatusView;
  dialogs: DialogView[];
  tasks: TaskView[];
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
  | { change: "viewer"; viewer: ViewerView | null };

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
