// The contract with the host, in TypeScript.
//
// It is a TRANSCRIPTION of `crates/norte-ui-host/src/{bridge,dto,action}.rs`,
// not a second definition: Rust is the one in charge. What keeps them from
// silently drifting apart is `tests/contract.test.ts`, which reads the SAME
// golden corpus that pins the Rust side
// (`crates/norte-ui-host/tests/golden/*.json`).
//
// There is no logic here. No comparator, no formatter, no availability rule:
// that lives in Rust (ADR 0066, decision D14).

/** The contract version this renderer knows how to read. */
export const BRIDGE_VERSION = 95;

/** Where a dragged pane is dropped over another (ADR 0138): on a side, or in
 *  the center to join it as a tab. */
export type DropZone = "left" | "right" | "top" | "bottom" | "center";

/** What the window's own title bar asks of its window (ADR 0136); the same
 *  closed vocabulary as `commands::WindowVerb` in Rust. */
export type WindowVerb = "minimize" | "toggle_maximize" | "close" | "drag";

/** How many spans the mark ruler splits a listing into; the same number as
 *  `norte_ui_host::dto::MARK_RULER_SPANS`. */
export const MARK_RULER_SPANS = 256;

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
  /** The TAB groups on screen. Separate from the placements because an
   *  inactive tab is not placed — its content is not painted — and it still
   *  has to be shown that it is there: a window with three tabs that only
   *  shows the front one hides open work. */
  tabs: TabGroupView[];
  /** Whether the TARGET mark says anything about the listings in view.
   *  Whether the role exists and whether it gets painted are two questions:
   *  `role` is the model and this is the paint, computed in Rust so as not
   *  to repeat the threshold. */
  mark_target?: boolean;
}

export interface TabGroupView {
  /** The PLACED slot the group belongs to: the active tab's. */
  slot_id: number;
  tabs: TabView[];
  /** Which one is in front, as an index into `tabs`. */
  active: number;
  /** A group of PANELS on the same edge (ADR 0134, bridge 88): with no `+`
   *  nor `×`, which open and close listings. Optional: an earlier host means
   *  listings. */
  panels?: boolean;
}

export interface TabView {
  /** The slot inside. It is what comes back when it is chosen with the
   *  mouse. */
  slot_id: number;
  /** Its label, already masked: a hostile directory inside a tab is as
   *  hostile as inside a listing. */
  title: string;
  title_hostile: boolean;
}

export type RowKind = "dir" | "file" | "symlink" | "other";

export interface CellView {
  column: string;
  text: string | null;
}

/** The splash screen (bridge 69): painted by the host, not the webview. */
export interface SplashView {
  art: string[];
  version: string;
  revision: string;
  daemon: string;
  hint: string;
  sections: SplashSectionView[];
  /**
   * How much longer it stays up, in milliseconds, or `null` if it stays
   * until someone dismisses it.
   *
   * The deadline is decided by the host and MET by this renderer: this is
   * where the timers are. It arrives as a duration and not as an instant
   * because the two clocks belong to different processes.
   */
  close_after_ms?: number | null;
}

export interface SplashSectionView {
  title: string;
  rows: SplashRowView[];
}

export interface SplashRowView {
  /** The number that opens it, or 0 if the row has no key. */
  number: number;
  label: string;
  detail: string;
}

export interface RowView {
  key: RowKey;
  display_name: string;
  hostile: boolean;
  /** How far along the task working on this row is, 0–100 (bridge 69). */
  progress?: number | null;
  kind: RowKind;
  selected: boolean;
  marked: boolean;
  cells: CellView[];
  /** The badge a plugin put there, already masked. Empty = none. */
  badge: string;
  /** The badge paints DIFFERENT from what it is: a plugin writes it. */
  badge_hostile: boolean;
  /**
   * The theme role to paint it with (`warning`, `error`…). Empty = none. It
   * is a CLOSED vocabulary: a plugin does not choose its own color.
   */
  badge_role: string;
  /**
   * The row's ICON (bridge 62): what a slot's `icon` decorator put there,
   * already masked. Empty = none. Goes to the LEFT of the name, in a column
   * that opens across every row of the slot as soon as one of them has one.
   */
  icon: string;
  /** The icon paints different from what it is: a plugin writes it. */
  icon_hostile: boolean;
  /**
   * The `#rrggbb` color the THEME paints this entry's name with, via
   * `[files.ext]` (wins) or `[files.kind]` (bridge 66). Empty = the theme
   * says nothing about it and the listing's normal color applies.
   *
   * Travels RESOLVED, unlike `badge_role`: extensions are an OPEN set — a
   * theme colors whichever ones it wants — so there are no classes this
   * renderer could know in advance.
   */
  name_color: string;
  /** The name is bold (a directory, an executable). */
  name_bold: boolean;
  /** Dimmed: how the retro presets paint compressed archives. */
  name_dim: boolean;
  /** Italic. */
  name_italic: boolean;
  /** Underlined. */
  name_underline: boolean;
}

export type SlotState =
  | { state: "ready" }
  /** With WHERE it is going: the body keeps showing the PREVIOUS listing
   *  until the new one arrives — on purpose, so a failure leaves the reader
   *  where they were — and without saying where that mix is going it cannot
   *  be read. Empty = a refresh, which goes nowhere. */
  | {
      state: "loading";
      /** The VERB's Fluent key, from the closed vocabulary the window shares
       *  with the terminal: `busy-connecting`, `busy-listing`,
       *  `busy-opening`. "Connecting" and "loading" are not the same, and
       *  the case that uncovered #323 was the first one. */
      verb_key?: string;
      target_display?: string;
      target_hostile?: boolean;
    }
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
  /** FIXED width in cells (bridge 64), or `null` if the column paints
   *  whatever size it measures. Dragging the header's border changes it. */
  width: number | null;
  /** `left` or `right`: the configured alignment, with effect only under a
   *  fixed width. */
  align: string;
}

export interface BrowserSlotView {
  kind: "browser";
  slot_id: number;
  generation: number;
  /**
   * What is arriving INTO this directory, 0–100 (ADR 0148, bridge 94): the
   * thin line on the border. Absent = nothing to paint.
   */
  progress?: number | null;
  path_display: string;
  path_hostile: boolean;
  total_rows: number | null;
  first_visible: number;
  rows: RowView[];
  /**
   * The icon column is open in this listing (bridge 62): some entry —
   * visible or not — has an icon, so every row carries the cell. Decided by
   * the host from the whole listing, not by this window from the rows it
   * can see.
   */
  icon_column: boolean;
  cursor: RowKey | null;
  marks: number;
  /**
   * The mark ruler (bridge 89, ADR 0135): which spans of the listing — out
   * of `MARK_RULER_SPANS` equal ones — carry a mark. Empty or absent = no
   * marks.
   */
  mark_ruler?: number[];
  /**
   * What the provider SKIPPED, already said in the reader's language. Empty
   * = none, or the provider does not keep count.
   */
  skipped_note: string;
  /**
   * How many entries hiding sets aside, already said. Empty = none. Goes in
   * the header and is PERMANENT: a listing that shows less than there is
   * cannot go silent the moment the reader presses another key.
   */
  hidden_note: string;
  /** Names are being REINTERPRETED with another encoding (#57). Empty = no.
   *  Permanent while it lasts: what is painted is not the bytes on disk, and
   *  that has to be knowable when deciding to copy or delete something. */
  names_note?: string;
  /** The listing is STILL FILLING, and how many so far. Empty = complete. */
  filling_note?: string;
  /** Marks the last refresh dropped because their entry is no longer there. */
  pruned_note?: string;
  /** How many are marked and how much they weigh, already said. Empty = no marks. */
  marked_note?: string;
  /** The listing's footer (counts, marked, free space), already worded.
   *  Empty or absent = `[ui] pane_footer` is off. Bridge 63. */
  footer?: string;
  /** The path's breadcrumbs (bridge 65): the root and one segment per
   *  directory, already masked. Clicking segment `i` navigates to that
   *  depth. Empty or absent = the whole path travels in `path_display`. */
  path_segments?: string[];
  /** How much of the volume is used, 0..1 (bridge 65); `null` or absent =
   *  unknown, and then the footer carries no indicator. */
  used_ratio?: number | null;
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
  /** The path of the listing this sheet FOLLOWS. Goes in the title. */
  follows_display: string;
  /** That path differs from the real bytes. */
  follows_hostile: boolean;
}

/**
 * The DOCKED viewer (#291, bridge 51): the file under the cursor of the
 * listing this slot follows, read-only, as in the TUI.
 */
export interface PreviewSlotView {
  kind: "preview";
  slot_id: number;
  /** The viewer with what was read, or `null` if there is no file to show. */
  viewer: ViewerView | null;
  /** Why there is no file, ALREADY SAID: a directory, nothing under the
   *  cursor, a read error. Empty when there is a viewer. */
  note: string;
}

export interface ProcessesSlotView {
  kind: "processes";
  slot_id: number;
  cursor: number | null;
}

export interface LogLineView {
  /** `HH:MM:SS`, in UTC — this tree carries no time zone database. */
  time: string;
  /** CLOSED vocabulary: error, warn, info, debug, trace. Colored by it. It
   *  is an IDENTITY: compared, not painted. */
  level: string;
  /** That level exactly as it is PAINTED (`TRACE`), which is what the
   *  terminal paints. Deliberately not translated: it is what gets written
   *  in `RUST_LOG` and what gets scanned for by eye. The level BUTTONS are
   *  translated — they are a control, not a datum. */
  level_label?: string;
  target: string;
  message: string;
  /** What is painted differs from what is there, in the module or the
   *  message. */
  hostile: boolean;
  /** Which PROCESS it came from: `window` or `daemon` (#328). In a merged
   *  list it is half the information: "the provider failed" and "the window
   *  could not paint it" read the same without knowing who wrote it. */
  source: string;
}

/** A plugin panel's clickable zone, in cells INSIDE the frame.
 *
 *  Without its command: the renderer sends the CELL (`panel_click`) and the
 *  host resolves which zone it was and which command applies. A command that
 *  traveled over the wire would be a command anyone talking to the renderer
 *  could send. */
export interface HitView {
  row: number;
  col: number;
  width: number;
}

/** The panel a PLUGIN paints (phase 3).
 *
 *  The guest does not draw: it DESCRIBES. The window sets the border, the
 *  title and the focus, which is what keeps a plugin from passing itself off
 *  as another panel. `lines` empty = there is no frame yet (the first
 *  request in flight, or the plugin failed): the border is painted with its
 *  title and nothing inside. */
export interface PanelSlotView {
  kind: "panel";
  slot_id: number;
  /** The `<kind>` the plugin declared, without the prefix. */
  title: string;
  lines: SpanView[][];
  hits: HitView[];
}

export interface DiskMapSlotView {
  kind: "disk_map";
  slot_id: number;
  /** The directory being described, masked and bounded. */
  title: string;
  title_hostile: boolean;
  /** The treemap already laid out by the host: the renderer computes
   *  nothing. */
  lines: SpanView[][];
  /** One rectangle per zone. No target: the host resolves the cell. */
  hits: HitView[];
  /** The measurement is still running; a half-finished map has to say so. */
  measuring: boolean;
}

/** A timeline row (bridge 78), already paintable. */
export interface TimelineRowView {
  time: string;
  /** `user`, `agent`, … — only for the dot's COLOR. */
  actor: string;
  op: string;
  path: string;
  hostile: boolean;
  /** Batch and "no undo", already translated; empty if neither. */
  tail: string;
}

/** The journal's timeline (#359, bridge 78). */
export interface TimelineSlotView {
  kind: "timeline";
  slot_id: number;
  title: string;
  rows: TimelineRowView[];
  cursor: number | null;
  /** What to say with no rows: empty if checked, loading if not, or the
   *  reason. */
  empty: string;
  /** What an `Enter` here would do; empty with no rows. */
  footer: string;
}

export interface LogSlotView {
  kind: "log";
  slot_id: number;
  /** Only the visible WINDOW, never the whole ring. */
  lines: LogLineView[];
  /** Identity: the renderer marks with it which button is set. */
  level: string;
  /** That same level exactly as it paints (`TRACE`). */
  level_label?: string;
  filter: string;
  /** Stuck to the end and following what arrives. */
  following: boolean;
  total: number;
  first_visible: number;
  /** Lines dropped, ALREADY SAID and with the number inside: a renderer does
   *  not translate nor substitute numbers. Empty = none. A log with a
   *  silent hole lies about what happened. With both sources in view these
   *  are TWO named counts and not a sum: the window's counts since the
   *  process started, the daemon's what this opening lost. */
  dropped_note: string;
  /** Which ring is CAPTURING more than what is shown, already translated.
   *  Empty = none. Lowering what is shown does not stop capturing, so the
   *  panel can say "info" while TRACE is being saved — and whoever is
   *  looking has a right to know before taking a screenshot. This also
   *  carries the DAEMON's level, naming it: its own is global to its
   *  clients and never lowers, so it cannot go in `level`, which is what
   *  filters the list. */
  capturing: string;
  /**
   * Which PROCESS these lines belong to, already translated. The window
   * starts its own daemon, so until #328 the daemon's own lines — the
   * providers, the journal, the policy — were NOT here; staying quiet about
   * it would make the panel look broken.
   */
  source: string;
  /** The EFFECTIVE source, in closed vocabulary: `window`, `daemon` or
   *  `both`. Effective and not the preference: with no second ring on the
   *  other side, `both` shows as `window`, because that is what is actually
   *  being looked at. */
  source_mode: string;
  /** There really is a second source to offer. `false` = the selector is
   *  NOT painted: a control between three views of the same ring promises
   *  something that does not exist. */
  sources_available: boolean;
  /** What needs to be said about the source, already translated. Empty =
   *  nothing. */
  source_note: string;
}

export type PlaceRowView =
  | { row: "header"; label: string; folded: boolean }
  | {
      row: "drive";
      /** The SHORT name (bridge 87): the label or the last segment. */
      label: string;
      hostile: boolean;
      /** The whole space sentence, for the title. */
      detail: string;
      /** The short free space (`159G`, `?`). Optional: a host older than
       *  87. */
      free?: string;
      /** The whole mount point, for the title. */
      mount?: string;
      /** `fixed` | `removable` | `network` | `unknown`: chooses the icon. */
      kind?: string;
    }
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
   * Goes up every time the set of rows changes. Travels back on the click:
   * volumes arrive from a background task and get inserted IN THE MIDDLE,
   * so an index with no generation could name the row next door.
   */
  generation: number;
}

export interface TreeRowView {
  /** The directory's name. The root carries its whole path. */
  label: string;
  hostile: boolean;
  /** Levels below the root. The root is 0. */
  depth: number;
  expanded: boolean;
  /**
   * Has children to show. `null` = not looked at yet, and these are three
   * different things for the reader: a branch that opens, a leaf, and
   * unread. Painting "leaf" for something that has not been read is a
   * made-up answer.
   */
  children: boolean | null;
}

export interface TreeSlotView {
  kind: "tree";
  slot_id: number;
  rows: TreeRowView[];
  cursor: number;
  /**
   * Goes up every time the set of branches changes. Travels back on the
   * click, for the same reason as the places sidebar: unfolding a branch
   * asks for its listing, and that listing inserts rows IN THE MIDDLE when
   * it arrives.
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
  | PanelSlotView
  | DiskMapSlotView
  | TimelineSlotView
  | TerminalSlotView
  | UnsupportedSlotView;

/** A color exactly as the shell SAID it, unresolved (bridge 95).
 *
 *  `indexed` stays an index on purpose: which blue "color 4" is is decided
 *  by the painter's palette, not the host. Resolving it there would have
 *  taken the decision away from the reader's theme, with no way to fix it
 *  from the theme. */
export type TerminalColorView =
  { kind: "indexed"; index: number } | { kind: "rgb"; hex: string };

/** A terminal row fragment: text with whatever the shell asked for. */
export interface TerminalSpanView {
  text: string;
  fg?: TerminalColorView;
  bg?: TerminalColorView;
  bold?: boolean;
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
  /** Reversed WHEN PAINTED: resolving it earlier would lose which color was
   *  which. */
  reverse?: boolean;
  strike?: boolean;
}

/** The terminal panel (#362, bridge 95): what the shell has painted.
 *
 *  It is FOREIGN content, and that is why it carries no theme role at all:
 *  what a program paints inside is its own. Ours is the frame. */
export interface TerminalSlotView {
  kind: "terminal";
  slot_id: number;
  /** ALWAYS the whole grid: a terminal does not scroll like a list, it
   *  repaints. */
  rows: TerminalSpanView[][];
  /** Row and column, from zero. Null = not painted, and it does not matter
   *  whether that is because the shell hid it or because the keyboard is
   *  not here. */
  cursor: [number, number] | null;
  /** There is no shell: it left, or it could not start. The slot stays. */
  no_shell?: boolean;
}

export interface PendingView {
  chords: string;
  count: number | null;
}

export interface StatusView {
  message: string | null;
  banners: BannerView[];
  /** Expired, unread notices (bridge 63): a badge while there is at least
   *  one; clicking it opens the log. Optional: an earlier host does not
   *  send it. */
  notices_unread?: number;
  pending: PendingView | null;
}

/** A persistent notice: the sentence on one side and the connection on the
 *  other. */
export interface BannerView {
  text: string;
  subject: BannerSubjectView | null;
}

/** Which connection a notice is about. Each part in its own field: mounting
 *  `scheme://host` inside the sentence lets a host read as another one's
 *  userinfo. */
export interface BannerSubjectView {
  scheme: string;
  host: string;
  /** Why it is degraded, already translated by the host. A reason the host
   *  does not know says "unknown reason" and does not inherit the sentence
   *  of one it does know. */
  reason: string;
  /** The wire's detail, already masked and bounded by the host. Only comes
   *  with an unknown reason. */
  detail?: string;
  hostile: boolean;
}

export interface DialogChoice {
  id: string;
  label_key: string;
  destructive: boolean;
}

/** A field of a FORM-dialog (bridge 91).
 *
 *  The renderer paints whatever `kind` says and decides nothing else: the
 *  label is a Fluent key, the value arrives already masked and bounded, and
 *  what values a cycle has is resolved by Rust before sending it. */
export interface DialogFieldView {
  /** Stable id: it is what comes back in `dialog_field`, not the index — a
   *  field inserted in the middle would renumber the ones below it. */
  id: string;
  label_key: string;
  /** Already masked and bounded; empty on the ones that are not text. */
  value: string;
  /** What is painted DIFFERS from the real thing. It is marked, never
   *  hidden. */
  hostile: boolean;
  kind: DialogFieldKind;
}

/** What kind of control a form field is. */
export type DialogFieldKind =
  | { kind: "text" }
  | { kind: "toggle"; on: boolean }
  | { kind: "cycle"; value_key: string };

/** What was done to a field. A toggle and a cycle carry no value: the
 *  renderer says they were touched and Rust decides which state they go to
 *  — sending the destination would let two quick presses step on each
 *  other. */
export type DialogFieldValue =
  { set: "text"; text: string } | { set: "toggled" } | { set: "cycled" };

/** What is known about a transfer's DESTINATION while the question is being
 *  asked.
 *
 *  Three states and not a list of warnings because silence has to mean ONE
 *  thing: the absence of line #164 means "this destination confines its
 *  writes", so "I don't know yet" cannot be painted the same as "I asked and
 *  it is clean". */
export type DestCheck =
  { state: "not_asked" } | { state: "checking" } | { state: "done"; warnings: string[] };

/** A dialog body line: what is painted, and whether it differs from the real thing. */
export interface DialogLine {
  text: string;
  hostile: boolean;
}

export interface DialogView {
  id: ModalId;
  title_key: string;
  /** Where the operation is going. Its own field: a directory name can
   *  contain an arrow, so labeling with a separator inside the text lets a
   *  path impersonate another one. */
  destination: DialogLine | null;
  /** WHAT is being asked (an agent's op). Outside the body, for the same
   *  reason as the destination. */
  subject: DialogLine | null;
  /** WHO is asking, when it is not whoever is sitting there. */
  asker: DialogLine | null;
  /** When the answer stops being accepted, already translated. */
  deadline: string | null;
  /** When it expires, in epoch-ms, so it can be counted for real. Absent =
   *  there is no deadline or it is unknown, and then the sentence paints
   *  as-is. */
  deadline_at_ms?: number;
  /** When they are paths, they are numbered BY POSITION: the label is
   *  structural and no file name can write it. */
  body: DialogLine[];
  /** The body shows less than the operation touches, already translated. */
  overflow_note: string;
  /** One of the ones NOT shown would paint altered. Absent = `false`, which
   *  means no mark: an extra badge on a truncated list teaches people to
   *  ignore it. */
  overflow_hostile?: boolean;
  /** What point the DESTINATION's check is at. Absent on an earlier bridge,
   *  and then it is `not_asked`. */
  dest_check?: DestCheck;
  choices: DialogChoice[];
  input: string | null;
  input_hostile: boolean;
  /** The FIELDS, when the dialog is a form (bridge 91). Absent or empty =
   *  the usual dialog, with at most one `input`. */
  fields?: DialogFieldView[];
  /** The field is a PASSWORD (#327). What arrives in `input` is DOTS, one
   *  per character, never the text: the host keeps what was typed
   *  separately, in a buffer that gets overwritten with zeros when it is
   *  released. The renderer paints the field as `password` and NEVER
   *  re-seeds it with `input` — doing so would turn the user's password
   *  into a row of literal dots. */
  input_secret: boolean;
}

/** A pair from the plan: from which name to which name. */
export interface AiRenamePairView {
  from: DialogLine;
  to: DialogLine;
}

/** The rename plan a model proposed, under review. */
/**
 * The ORGANIZE plan under review (bridge 72).
 *
 * `AiRenameView`'s twin with two differences: the body is a TREE — what
 * changes is the directory's shape — and there is no verdict to wait for,
 * because the plan's token traveled with it.
 */
export interface OrganizeView {
  dir: DialogLine;
  /** The window that travels, NOT the whole tree. */
  lines: OrganizeLineView[];
  first_visible: number;
  total: number;
  /** How much is visible out of how much there is, already translated.
   *  Empty = everything is visible. */
  more_note: string;
  /** Outside the window there is a name that paints different from what it
   *  is. */
  hidden_hostile: boolean;
  /** "Creates N folders and moves M files", already translated. Goes
   *  BEFORE the tree. */
  summary: string;
  /** The reader has gone through the whole tree. Approving requires it. */
  seen_all: boolean;
}

/** A line of the organize tree (bridge 72). */
export interface OrganizeLineView {
  /** How much it is indented: 0 is a direct child of the plan's directory. */
  depth: number;
  /** The name, already sanitized, with its mark if it differs from the real
   *  thing. */
  text: DialogLine;
  kind: OrganizeLineKind;
}

/**
 * What a tree line is.
 *
 * Arrives as DATA and not resolved to a color: the renderer decides how a
 * folder about to be created looks, and a monochrome theme needs to be able
 * to mark it another way.
 */
export type OrganizeLineKind = "new_dir" | "existing_dir" | "moved";

export interface AiRenameView {
  dir: DialogLine;
  /** The window that travels, NOT the whole plan. */
  pairs: AiRenamePairView[];
  first_visible: number;
  total: number;
  /** How much is visible out of how much there is, already translated.
   *  Empty = everything is visible. */
  more_note: string;
  /** Outside the window there is a name that paints different from what it
   *  is. */
  hidden_hostile: boolean;
  /** The core's verdict, already translated. */
  status: string;
  /** Machinery and collisions, each line with its mark. */
  detail: DialogLine[];
  /** Approving can do something. The core says so. */
  confirmable: boolean;
  /** How many it will REALLY rename, already stated and translated. */
  real_steps_note: string;
  /** The reader has gone through the whole plan. Approving requires it. */
  seen_all: boolean;
}

export type TaskStateView =
  "queued" | "running" | "paused" | "done" | "failed" | "cancelled";

export interface TaskView {
  task_id: number;
  kind: string;
  state: TaskStateView;
  percent: number | null;
  /** The rate, already written by the host (`1.2 MiB/s`); empty if unknown. */
  rate: string;
  /** What is left, already written (`1m 20s`); empty if unknown. */
  eta: string;
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
  /**
   * How much there is WIDTH-WISE, in cells, and how far along (bridge 59).
   * The viewer does not wrap, so without these two a minified HTML paints
   * truncated with nothing to draw the horizontal bar with. `total_cols` is
   * 0 in hexadecimal, which has a fixed width and does not scroll.
   */
  total_cols: number;
  first_col: number;
  lines: string[]; /** "via ‹plugin›", already translated. Empty = norte shows it, not a plugin. */
  preview_by: string;
  /** The decoding given to the previewer was LOSSY. */
  preview_lossy: boolean;
  /**
   * It is a PAINTABLE image and claims to be this big. `null` = it is not,
   * or it is one the host refuses to paint (and then it is said in
   * `image_refused`). The bytes do NOT come here: they are requested
   * separately.
   */
  image: ImageView | null;
  /** Why a recognized image is NOT painted, already translated. */
  image_refused: string;
  /**
   * The image's zoom, as a PERCENTAGE of what it would take up fitted
   * (bridge 80). `100` = fitted, which is how it opens.
   */
  image_zoom: number;
  /**
   * The visible lines WITH STYLE when what is shown was produced by a
   * previewer (bridge 49): one entry per `lines` row. Empty in the raw
   * view, and then `lines` is painted.
   */
  styled: SpanView[][];
}

/**
 * A styled preview line's fragment. `role` WINS over `fg` when both come:
 * the reader's theme rules over a plugin's fixed color. The role already
 * arrives validated by the host.
 */
export interface SpanView {
  text: string;
  role: string | null;
  fg: string | null;
  /** The BACKGROUND, `#rrggbb` (bridge 50): half-blocks from an image
   *  previewer. */
  bg: string | null;
}

/**
 * A recognized and accepted image. The size is what its header DECLARES:
 * nobody has decoded it yet, and that is the point — the declared one is
 * what the host compared against its budget (ADR 0069).
 */
export interface ImageView {
  /** Recognized by MAGIC bytes, never by the extension. */
  format: string;
  width: number;
  height: number;
}

/** The first-run wizard (bridge 63): a step, its rows and the cursor, all
 *  already translated. A click on a row chooses it and confirms it. */
export interface WizardView {
  title: string;
  question: string;
  rows: string[];
  cursor: number;
  hint: string;
}

export interface PaletteRowView {
  text: string;
  desc: string;
  chord: string;
  enabled: boolean;
  /** What is painted DIFFERS from what the row's source declares. Can only
   *  be true for a PLUGIN row, and this is the screen where you choose what
   *  third-party code to run. */
  hostile: boolean;
  /** Goes up top for being among the last launched (only with an empty
   *  query). Optional: a host older than bridge 63 does not send it. */
  recent?: boolean;
}

export interface PaletteView {
  query: string;
  rows: PaletteRowView[];
  cursor: number | null;
  total: number;
}

/** A "go to" line (bridge 77): a section header or a row. */
export type GotoLineView =
  | { line: "header"; title: string }
  | { line: "row"; text: string; desc: string; hostile: boolean };

/** "Go to anywhere" (#357, bridge 77). The cursor's index is into `lines`,
 *  and never lands on a header. */
export interface GotoView {
  query: string;
  lines: GotoLineView[];
  cursor: number | null;
  empty: string;
}

export interface ProfileRowView {
  name: string;
  name_hostile: boolean;
  title: string | null;
  active: boolean;
  /** What ELSE is named the same, already translated. Empty = it is just a
   *  profile. */
  clash: string;
  /** Cannot save where you left each pane (non-UTF-8 name). */
  no_state: boolean;
  /** Why it cannot load. Empty = it can. */
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
  /** This window can run it. A disabled one STILL shows: the menu is where
   *  what exists gets seen. */
  enabled: boolean;
  /** Whether this entry STARTS a section (bridge 74): `null` stays in the
   *  previous one's, `""` is a rule with no label, other text is the
   *  label. */
  section: string | null;
  /** `normal`, `destructive` or `ai`. */
  role: string;
}

export interface MenuView {
  /** `[ui] menu_bar`: whether the bar is painted. Off, the menu still opens
   *  from its key. */
  bar: boolean;
  titles: string[];
  /** Which one is dropped down, if any. */
  open: number | null;
  /** The dropdown's entries; empty if there is none. */
  items: MenuItemView[];
  cursor: number;
}

/** How a panel bar button's panel stands (bridge 51). */
export type PanelButtonState = "closed" | "open" | "focused";

/** A panel bar button. */
export interface PanelButtonView {
  /** The kind it opens, already masked: ends up in a DOM attribute. */
  kind: string;
  /** The short name, in the session's language. */
  label: string;
  /** The letter the TUI paints; here it accompanies the label. */
  letter: string;
  /** The shortcut that does the same thing, or `—`. */
  chord: string;
  state: PanelButtonState;
  /** Has something to report without being in view. */
  attention: boolean;
  /** How many things (bridge 84): the badge's figure. Optional: an earlier
   *  host does not send it, and then the mark shows with no figure. */
  count?: number;
}

/** A chrome button that runs a command (ADR 0133): the layout ones. */
export interface ChromeButtonView {
  /** The stable id: comes back with the click and chooses the icon. */
  id: string;
  /** Its short name, its menu entry's. */
  label: string;
  /** The shortcut, or `—`. */
  chord: string;
}

/** An element of the status bar's right half (ADR 0132). */
export interface StatusItemView {
  /** The stable id: comes back with the click. */
  id: string;
  text: string;
  tooltip: string;
  /** Whether clicking it does anything. */
  clickable: boolean;
  /**
   * The thin progress bar, BEHIND the text (ADR 0146, bridge 92). Only on
   * the `tasks` item with work in progress.
   */
  progress?: StatusProgressView;
}

/** The `tasks` item's bar (ADR 0146). */
export interface StatusProgressView {
  /** 0–100 of the total; `null` = unknown, and the bar animates. */
  percent: number | null;
  phase: "running" | "paused" | "done" | "failed";
}

/** The panel bar (#324, bridge 51): what panels there are and how they stand. */
export interface PanelBarView {
  /** `[ui] panel_bar`: whether the bar is painted. */
  bar: boolean;
  /** `[ui] panel_bar_style = "names"`: name with the letter marked, or just
   *  the letter. Optional: a host older than bridge 63 does not send it. */
  names?: boolean;
  /** `[ui] panel_bar_position` already resolved by the host (bridge 84):
   *  `true` = activity bar on the left edge; absent = row on top. */
  vertical?: boolean;
  /** A click comes back as the INDEX here, never as a command. */
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
  | { span: "link"; text: string; action: number | null };

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
  /** The last request to scroll the body (bridge 76). */
  scroll: HelpScrollView | null;
}

/** Where to scroll help's body toward. */
export type HelpScrollTo =
  | "line_up"
  | "line_down"
  | "page_up"
  | "page_down"
  | "top"
  | "bottom"
  | "section_prev"
  | "section_next";

/** A scroll request, numbered so it is applied only once. */
export interface HelpScrollView {
  to: HelpScrollTo;
  seq: number;
}

export interface SettingRowView {
  id: string;
  name: string;
  desc: string;
  /** Already masked: comes from the `norte.toml` the user writes. */
  value: string;
  /** The value paints DIFFERENT from what it is. */
  hostile: boolean;
  restart_required: boolean;
  /** Its FACTORY value: what an empty field shows as a placeholder. */
  default: string;
  /** What control this row calls for. `none` = not edited from here. */
  control: "toggle" | "choice" | "number" | "text" | "args" | "none";
  /** The accepted values if it is `choice`; empty if not. Already resolved. */
  choices: string[];
  /** A `number`'s bounds, both inclusive. */
  min: number | null;
  max: number | null;
  /** It is not the factory value: the point of "you touched this". */
  modified: boolean;
}

export interface PathRowView {
  label: string;
  display: string;
  hostile: boolean;
  missing: boolean;
}

export type SettingsSectionView =
  /** `key` is the STABLE key: pairs the section with its index row. */
  | { section: "settings"; key: string; title: string; rows: SettingRowView[] }
  | { section: "paths"; title: string; rows: PathRowView[] };

/** A section in the left-hand index. */
export interface SectionIndexView {
  /** Its STABLE key: what comes back in `settings_jump_section`. */
  key: string;
  title: string;
  /** How many of its rows are visible with the filter set. Zero = dimmed. */
  visible: number;
}

export interface SettingsView {
  /** Only the sections with rows to show. */
  sections: SettingsSectionView[];
  /** ALL the ones this surface has, whether or not the filter hides its
   *  rows. */
  index: SectionIndexView[];
  /** Which half has the keyboard: the other one paints its cursor dimmed. */
  focus: "index" | "list";
  cursor: number;
  query: string;
  shown: number;
  total: number;
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
  /** The reason paints different from what it is: it can quote the
   *  manifest. */
  reason_hostile: boolean;
  /** Which id it gets uninstalled with; `null` if the directory is not
   *  named like one. */
  id: string | null;
}

export interface ExtensionConfigRowView {
  key: string;
  kind: string;
  /** Already masked: written by the plugin. */
  value: string;
  /** Already masked: written by the plugin. */
  default: string;
  description: string;
  /** Already masked: an `enum`'s values are written by the plugin. */
  domain: string;
  /** One of the three paints DIFFERENT from what it is. */
  hostile: boolean;
  /** This build knows how to edit this `kind`. A newer peer's type is
   *  read-only: offering `Enter` on something that will not change makes it
   *  seem like the write failed. */
  editable: boolean;
}

export interface ExtensionCommandView {
  /** Dispatch key. NEVER painted: the manifest does not validate its
   *  charset. */
  id: string;
  title: string;
  hostile: boolean;
}

export interface ExtensionDetailView {
  id: string;
  config: ExtensionConfigRowView[];
  /** The commands it contributes, in manifest order. */
  commands: ExtensionCommandView[];
  /** Which key is chosen. */
  cursor: number;
  /** What is being typed, ALREADY masked. `null` = nothing is being edited. */
  editing: string | null;
  /** The buffer paints different from what is about to be written. */
  editing_hostile: boolean;
}

/** A third party's string with its flag ALONGSIDE it: a loose flag ends up
 *  describing the neighboring string. */
export interface MaskedTextView {
  text: string;
  hostile: boolean;
}

export interface AgentRowView {
  /** Its id, already masked: it is an OPAQUE daemon key and can carry any
   *  byte. What travels back is the raw one, not this. */
  session: string;
  session_hostile: boolean;
  /** How many it asked for and how many were granted from here, already in
   *  a translated sentence: the catalogue that crosses does not substitute
   *  variables. */
  counts: string;
  /** Already has an undo in progress: it is said, and another `u` is
   *  refused. */
  undoing: boolean;
  last_op: string;
  last_op_hostile: boolean;
}

export interface AgentsView {
  rows: AgentRowView[];
  cursor: number;
  /** How many times this list has changed. Comes back with the click: the
   *  list reorders ON ITS OWN — a permission request bumps its session to
   *  the top — and a click against the old one chooses a different row. */
  generation: number;
  /** How many sessions have been forgotten to the cap. Painted when it is
   *  not zero: a trimmed list presented as complete is what turns "flood
   *  the list" into "that session does not exist". */
  forgotten: number;
  /** What this list IS: what this window has seen, not the system's census.
   *  Without saying so, an empty list reads as "no agent has touched
   *  anything", a claim this window cannot make. */
  note: string;
  /** What to say when there are no rows, already translated: it is not
   *  always the same thing — a window with no effects does not even listen
   *  for requests. */
  empty: string;
}

export interface ExtensionOutputView {
  /** Which extension: its name, already masked, with its flag. */
  plugin: MaskedTextView;
  /** Its reverse-DNS id, which the core DOES validate: the name does not
   *  identify. */
  plugin_id: string;
  /** Which command. `text` empty if its title was unknown. */
  command: MaskedTextView;
  /** What it printed, LINE BY LINE, each one masked and bounded: a newline
   *  is a C0 control, so masking the whole output marked any output over
   *  one line as hostile. */
  lines: string[];
  /** Some line paints different from what the plugin printed. */
  text_hostile: boolean;
  /** Did not fit whole and was cut off. Travels because the receiver
   *  cannot deduce it: the text arrives already short. */
  truncated: boolean;
}

/** The output of a program the host ran and waited on (#312). */
export interface ProgramOutputView {
  /** The title's Fluent key: what was done. */
  title_key: string;
  /** The program and its arguments, already masked. */
  command: MaskedTextView;
  /** stdout and stderr, LINE BY LINE, each one masked and bounded. */
  lines: string[];
  text_hostile: boolean;
  truncated: boolean;
  /** Did not start, or ran past the deadline. NOT "exited nonzero". */
  failed: boolean;
}

/** What `extension_govern` changes (bridge 61): the manager's three verbs. */
export type ExtensionChange = "approval" | "enabled" | "uninstall";

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
  /** Which themes can be chosen from. */
  choices: string[];
  /** Which one is pointed to. Moving through it previews live. */
  cursor: number;
}

/** A declared effect this renderer does not paint. The key comes from the
 *  theme file, so it travels masked and with its flag. */
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
  /** See `PlacesSlotView.generation`: opens empty and fills afterward. */
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
 * The COLUMNS picker. Its title already carries the SCOPE inside — one
 * scheme or all — and its note says the choice applies to this window and
 * is not saved.
 */
export interface ColumnsPickerView {
  /** The footer with the keys, already painted from the keymap by the
   *  host. */
  hint: string;
  title: string;
  rows: ColumnsPickerRowView[];
  cursor: number;
  note: string;
}

export interface ColumnsPickerRowView {
  /** Its configuration id. Identity: whole or empty. */
  id: string;
  /** What it is called, already translated and sanitized. */
  label: string;
  /** The label paints DIFFERENT from what it is. */
  hostile: boolean;
  enabled: boolean;
  /** Current format, closed ASCII vocabulary. Empty = not applicable. */
  format: string;
  /** Fixed by a scheme setting: not cycled here. */
  format_locked: boolean;
  /** Neither turns off nor moves. It is the NAME. */
  fixed: boolean;
}

export interface LayoutPickerView {
  title: string;
  rows: LayoutRowView[];
  cursor: number;
  preview: string[];
  /** Why the chosen one has no preview. QUOTES the user's file. */
  problem: string;
  /** The painted diagnosis differs from what the file contains. */
  problem_hostile: boolean;
}

export interface SearchRowView {
  name: string;
  hostile: boolean;
  parent: string;
  parent_hostile: boolean;
  is_dir: boolean;
  /** How much it resembles what was asked, in `[-1, 1]`. `null` in a search
   *  by name: there are no degrees there. */
  score: number | null;
}

/** The diff panel: two trees compared, row by row.
 *
 *  A window and not the whole list: the engine emits one row per matched
 *  name across the WHOLE tree and nothing bounds it, so what travels is what
 *  is visible. */
/** The sync panel: the PLAN, before anything gets written.
 *
 *  A window like the diff one: a plan of half a million steps does not
 *  cross whole, and the steps are named by their `id`. */
export interface SyncView {
  source: DialogLine;
  dest: DialogLine;
  /** `update` or `mirror`. A mirror DELETES on the destination and an
   *  update does not: painted before approving. */
  mode: string;
  steps: SyncStepView[];
  first_visible: number;
  total: number;
  /** The plan's SUMMARY: irreversibles, bytes, the unreadable, and whether
   *  the list hides steps. It is what gets read before approving. */
  summary: string[];
  /** What BLOCKS syncing, with its path. */
  blockers: SyncBlockerView[];
  /** How many there really are: the wire truncates the list. */
  blockers_total: number;
  status: string;
  hint: string;
  /** The SECOND question, when the plan deletes or leaves something with no
   *  way back. Only `y` answers yes. */
  confirming: string | null;
  /** The steps that failed to apply. The count is in the status. */
  failures: SyncFailureView[];
  /** Decided by the shared model: offering to approve what is going to be
   *  rejected is the broken screen this prevents. */
  can_approve: boolean;
  running: boolean;
  /** Already asked to stop. The second `Escape` closes the panel. */
  cancel_requested: boolean;
}

export interface SyncFailureView {
  cause: string;
  path: string;
  path_hostile: boolean;
  /** `source`, `dest` or `either`. It HAS to be painted: staying quiet
   *  about an `either` in a panel where an unqualified path means "from the
   *  source" is asserting the source. */
  anchor: string;
  /** The anchor ALREADY SAID, in the session's language. Empty when it is
   *  the source, which is what an unqualified path means here. */
  anchor_label: string;
}

export interface SyncBlockerView {
  label: string;
  /** Where. The root is said as "the whole tree", not empty. */
  path: string;
  path_hostile: boolean;
}

export interface SyncStepView {
  id: number;
  kind: string;
  reason: string;
  /** Whether undo restores it. Never comes out of `reversal` plain. */
  undo: string;
  anchor: string;
  /** As in the failure: the anchor already said, empty when it is the
   *  source. */
  anchor_label: string;
  path: string;
  path_hostile: boolean;
  /** The DESTINATION's spelling when its bytes differ: the write lands on
   *  THIS one. */
  dest_path: string | null;
  dest_path_hostile: boolean;
  /** Both spellings render the same and it has to be said. */
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
  /** The chosen row, BY ITS ID: a filter hides rows, never renumbers them. */
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
  /** Why the row shows two spellings. A sentence, not a badge glued to the
   *  name: what gets glued to a name can be forged by a name. */
  paired_under: string | null;
}

export interface CompareFaceView {
  name: string;
  hostile: boolean;
  /** Empty when the provider does not know it: "I don't know" and "zero
   *  bytes" are two different answers. */
  size: string;
  mtime: string;
  is_dir: boolean;
}

export interface SearchView {
  /** Asked by MEANING against the index, not by name against the tree: the
   *  scope is the whole index and not `root`. */
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
  /** The status bar's right half (ADR 0132, bridge 85). Optional: an
   *  earlier host does not send it. */
  status_items?: StatusItemView[];
  /** The layout buttons (ADR 0133, bridge 86). Optional: an earlier host
   *  does not send them. */
  layout_buttons?: ChromeButtonView[];
  /** `[ui] row_stripes` (bridge 80): the listing's stripes. Optional: an
   *  earlier host does not send it, and then there is no band. */
  row_stripes?: boolean;
  profiles: ProfilePickerView | null;
  palette: PaletteView | null;
  /** "Go to anywhere" (bridge 77), if it is open. Optional: an earlier host
   *  does not send it. */
  goto?: GotoView | null;
  /** The first-run wizard (bridge 63), if it is open. Optional: an earlier
   *  host does not send it. */
  wizard?: WizardView | null;
  /** The splash screen (bridge 69, ADR 0115), if it is up. */
  splash?: SplashView | null;
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
  organize: OrganizeView | null;
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
      /** The icon column, WITH the rows: that is how icons land. */
      icon_column: boolean;
      /**
       * How many rows the WHOLE listing has. It is the scroll's height, and
       * paginated draining only sends row patches.
       */
      total_rows: number | null;
    }
  | {
      change: "browser_header";
      slot_id: number;
      path_display: string;
      path_hostile: boolean;
      skipped_note: string;
      hidden_note: string;
      names_note?: string;
      filling_note?: string;
      pruned_note?: string;
      marked_note?: string;
      footer?: string;
      path_segments?: string[];
      used_ratio?: number | null;
      marks: number;
      mark_ruler?: number[];
    }
  | { change: "slot_state"; slot_id: number; state: SlotState }
  | ({ change: "status" } & StatusView)
  /** The dashboard, and with it which process-panel row is chosen: a task
   *  that expires removes a row and shifts the rest. `null` = none, which
   *  is what an empty dashboard says. Never missing: this bridge tolerates
   *  no version mismatches, it rejects them. */
  | { change: "slot_progress"; slot_id: number; progress: number | null }
  | { change: "tasks"; tasks: TaskView[]; cursor: number | null }
  | { change: "dialogs"; dialogs: DialogView[] }
  | ({ change: "connection" } & ConnectionView)
  | ({ change: "layout" } & LayoutView)
  | { change: "columns"; slot_id: number; columns: ColumnHeader[] }
  | { change: "viewer"; viewer: ViewerView | null }
  | { change: "ai_rename"; ai_rename: AiRenameView | null }
  | { change: "organize"; organize: OrganizeView | null }
  | { change: "which_key"; whichkey: WhichKeyView | null }
  | { change: "menu"; menu: MenuView }
  | { change: "panel_bar"; panel_bar: PanelBarView }
  | { change: "status_items"; status_items: StatusItemView[] }
  | { change: "profiles"; profiles: ProfilePickerView | null }
  | { change: "palette"; palette: PaletteView | null }
  | { change: "goto"; goto: GotoView | null }
  | { change: "wizard"; wizard: WizardView | null }
  | { change: "splash"; splash: SplashView | null }
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

/** An already normalized key. Rust is the one that resolves it. */
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
  | { action: "resize_column"; slot_id: number; column: string; cells: number }
  | { action: "breadcrumb_activate"; slot_id: number; depth: number; generation: number }
  | {
      action: "dialog";
      id: ModalId;
      choice: string;
      /** The typed password, ONLY on a dialog with `input_secret` (#327).
       *  Travels with the answer and not with every keystroke: over
       *  `dialog_input`, `h`, `hu`, `hun`… would cross and each prefix would
       *  stay in a piece of heap nobody wipes. This way it crosses ONCE, the
       *  instant the reader decides to hand it over. */
      secret?: string;
    }
  | { action: "dialog_input"; id: ModalId; text: string }
  /** Touches a FORM-dialog field (bridge 91). Separate from `dialog_input`
   *  because it has to say WHICH of its fields was touched, and because a
   *  password never travels through here. */
  | {
      action: "dialog_field";
      id: ModalId;
      field: string;
      value: DialogFieldValue;
    }
  | { action: "refresh_slot"; slot_id: number }
  | { action: "log_set_level"; level: string }
  | { action: "log_set_filter"; filter: string }
  | { action: "log_scroll"; delta: number }
  | { action: "preview_scroll"; slot_id: number; delta: number }
  /**
   * A CELL of a plugin panel was clicked (phase 3). The cell travels, not a
   * command: the host has the frame and resolves which zone it was and
   * which command applies, with the same filter as the terminal. A command
   * that crossed the wire could be sent by anyone talking to the renderer.
   */
  | { action: "panel_click"; slot_id: number; row: number; col: number }
  /**
   * The WHEEL over the full-screen viewer (bridge 59). Both axes in one
   * action because a single gesture produces them: the plain wheel scrolls
   * down, with `shift` it goes sideways.
   */
  | { action: "viewer_scroll"; lines: number; cols: number }
  | { action: "log_follow" }
  | { action: "log_cycle_source" }
  | { action: "log_set_visible_range"; rows: number }
  | { action: "cancel_task"; task_id: number }
  | { action: "compare_select_row"; id: number }
  | { action: "compare_activate_row"; id: number }
  | { action: "compare_toggle_filter"; category: string }
  | { action: "compare_set_visible_range"; first: number; count: number }
  | { action: "set_viewport"; width: number; height: number }
  /**
   * The scheme the desktop asks for (`prefers-color-scheme`). The
   * variant's CSS variables are plugged in on this side; this is so the
   * HOST resolves entries' colors against the same variant, since bridge 66
   * it travels baked into the row.
   */
  | { action: "set_color_scheme"; dark: boolean }
  | ({ action: "key" } & KeyInput)
  | { action: "set_viewer_rows"; rows: number }
  | { action: "set_viewer_cols"; cols: number }
  | { action: "help_select_topic"; row: number }
  | { action: "help_activate"; index: number }
  | { action: "settings_select_row"; row: number }
  /** A double click on a settings row: what `enter` does (bridge 60). */
  | { action: "settings_activate"; row: number }
  /** The search box's WHOLE text: printable keys do not reach the host. */
  | { action: "settings_query"; text: string }
  /** By the section's STABLE key, not its translated label. */
  | { action: "settings_jump_section"; section: string }
  | { action: "settings_reset"; row: number }
  /** Sets a specific value: what a toggle or a dropdown sends. */
  | { action: "settings_set"; id: string; value: string }
  | { action: "extension_select_row"; row: number }
  /**
   * A manager BUTTON on a row (bridge 61): points to it and does what the
   * keyboard verb would do to it, questions included. `approval` grants or
   * revokes depending on how it stands; `enabled` turns it on or off;
   * `uninstall` deletes its files and withdraws its consent, after asking.
   * Travels with the `id` the row had: the catalogue reshuffles in the
   * background and a row deleted above shifts the ones below; the host
   * refuses if it no longer matches.
   */
  | { action: "extension_govern"; row: number; id: string; change: ExtensionChange }
  /** That row's extension's help page (bridge 61). */
  | { action: "extension_help"; row: number; id: string }
  | { action: "agent_select_row"; row: number; generation: number }
  | { action: "select_tab"; slot_id: number }
  | { action: "picker_select_row"; row: number; generation: number }
  | { action: "place_activate_row"; row: number; generation: number }
  | { action: "tree_activate_row"; row: number; generation: number }
  | { action: "tree_toggle_row"; row: number; generation: number }
  | { action: "layout_activate_row"; row: number }
  | { action: "search_activate_row"; row: number }
  | { action: "ai_rename_decide"; approve: boolean }
  | { action: "organize_decide"; approve: boolean }
  | { action: "organize_scroll"; down: boolean }
  /** A handoff's terminal did not open (bridge 73). Sent by whoever hosts
   *  it, not this renderer; it is here so the type covers the whole
   *  bridge. */
  | { action: "handoff_failed"; no_terminal: boolean }
  | { action: "menu_open"; menu: number }
  | { action: "menu_point_row"; row: number }
  | { action: "menu_activate_row"; row: number }
  | { action: "menu_close" }
  /** Alt pressed and released alone: folds or opens the menu (bridge 68). */
  | { action: "menu_toggle" }
  | { action: "wizard_open" }
  | { action: "wizard_activate_row"; row: number }
  | { action: "splash_open" }
  | { action: "splash_close" }
  | { action: "splash_activate_row"; number: number }
  | { action: "panel_bar_activate"; button: number }
  | { action: "status_item_activate"; id: string }
  | { action: "layout_button_activate"; id: string }
  | { action: "tab_action"; slot_id: number; verb: "new" | "close" }
  | { action: "resize_slot"; slot_id: number; cells: number }
  | { action: "move_slot"; slot_id: number; target: number; zone: DropZone }
  | { action: "profile_activate_row"; row: number; generation: number }
  | { action: "resync" };

export type StaleReason = "instance" | "generation" | "modal";

export type ActionAck =
  | { status: "applied"; sequence: number }
  | { status: "stale"; reason: StaleReason }
  | { status: "unavailable"; reason_key: string };

/** What the host projects ONCE on startup: strings and colors, already resolved. */
export interface HostCatalog {
  bridge_version: number;
  instance_id: string;
  locale: string;
  /** Fluent key -> text already translated IN RUST. */
  strings: Record<string, string>;
  /** Theme role -> CSS variables already resolved in Rust. */
  theme: Record<string, string>;
  /** Task 3.6's measurement pass (`NORTE_GUI_MEASURE=1`). */
  measure: boolean;
  /** How long to wait before SHOWING that it is waiting, in ms. Travels
   *  instead of being written in the CSS because it is a decision shared
   *  with the terminal (`norte_frontend::busy::THRESHOLD`), and a number
   *  repeated in a stylesheet is the third place to change it. */
  busy_threshold_ms?: number;
  /** `[ui] font`, `mono_font`, `font_size` and `reduce_motion`. */
  appearance?: Appearance;
  /** There is no user `norte.toml` yet (bridge 63): the renderer opens the
   *  first-run wizard when it paints the first frame. */
  first_run?: boolean;
  /** This window starts with no splash screen (bridge 69, ADR 0115):
   *  `--no-splash` or `NORTE_NO_SPLASH`. Decided by startup, which is the
   *  one that sees the command line and the environment; here the notice is
   *  only silenced. */
  no_splash?: boolean;
  /** `[ui] theme_light` / `theme_dark` already resolved to variables (spec
   *  2026-09-11, V6): the renderer applies the one that matches
   *  `prefers-color-scheme`, and `theme` when there is no variant for that
   *  side. Absent or `null` = `theme` only. */
  theme_light?: Record<string, string> | null;
  theme_dark?: Record<string, string> | null;
}

/** What this window paints that is not color. Each `null` field = the
 *  configuration does not say, and then whatever is already there rules
 *  (the stylesheet, or the desktop in the case of motion). */
export interface Appearance {
  font: string | null;
  mono_font: string | null;
  /** In px, already validated to [8, 32] in Rust. ALSO moves the grid: this
   *  window is laid out in cells, so a size that only changed the glyph
   *  would leave it overflowing its row. */
  font_size: number | null;
  reduce_motion: boolean | null;
  /** `[ui] titlebar = "custom"` (ADR 0136): the window does not carry the
   *  desktop's bar and the menu one acts as the title bar. Absent = the
   *  native one. */
  custom_titlebar?: boolean;
}
