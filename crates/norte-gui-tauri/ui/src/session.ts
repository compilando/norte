// The sequence's discipline, and the state the renderer has a right to keep:
// a COPY of the last thing the host said, so it can be painted.
//
// The three rules are decision D7's, and none of them is negotiable here: a
// version that is not recognized is not half-interpreted, a patch over
// another base is not applied, and a gap in the sequence is resolved by
// asking for a frame — never by guessing what was missing.

import { BRIDGE_VERSION } from "./types";
import type { BridgeEnvelope, UiUpdate, ViewSnapshot } from "./types";

/** What happened to a message. */
export type Outcome =
  | { kind: "applied" }
  | { kind: "ignored"; why: "old-sequence" | "other-instance" }
  | { kind: "gap"; expected: number; got: number }
  | { kind: "incompatible"; version: number }
  | { kind: "notice"; notice: Extract<UiUpdate, { update: "notice" }> };

export class Session {
  private snapshot: ViewSnapshot | null = null;
  private instance: string | null = null;
  private seq = -1;

  /** The screen that needs painting, or `null` if there is not one yet. */
  view(): ViewSnapshot | null {
    return this.snapshot;
  }

  sequence(): number {
    return this.seq;
  }

  instanceId(): string | null {
    return this.instance;
  }

  receive(env: BridgeEnvelope<UiUpdate>): Outcome {
    if (env.bridge_version !== BRIDGE_VERSION) {
      return { kind: "incompatible", version: env.bridge_version };
    }
    if (this.instance !== null && env.instance_id !== this.instance) {
      // Another life of the host. Not an error, and it mutates nothing.
      return { kind: "ignored", why: "other-instance" };
    }
    if (env.sequence <= this.seq) {
      return { kind: "ignored", why: "old-sequence" };
    }
    const p = env.payload;
    if (p.update === "snapshot") {
      // A frame REPLACES, so it closes any gap: accepted no matter where it
      // falls in the sequence.
      const { update: _u, ...view } = p;
      this.snapshot = view;
      this.instance = env.instance_id;
      this.seq = env.sequence;
      return { kind: "applied" };
    }
    if (this.snapshot === null || env.sequence !== this.seq + 1) {
      return { kind: "gap", expected: this.seq + 1, got: env.sequence };
    }
    if (p.update === "notice") {
      this.seq = env.sequence;
      return { kind: "notice", notice: p };
    }
    if (p.base_sequence !== this.seq) {
      // A patch over a different base does not apply "almost right": it
      // does not apply.
      return { kind: "gap", expected: this.seq, got: p.base_sequence };
    }
    for (const c of p.changes) {
      if (!this.applyChange(c)) {
        // A change this renderer does not recognize is NOT discarded by
        // advancing the sequence: that would leave a diverged copy of the
        // screen that passes every check that follows. A frame is requested
        // instead.
        return { kind: "gap", expected: this.seq, got: env.sequence };
      }
    }
    this.seq = env.sequence;
    return { kind: "applied" };
  }

  /** `false` if the change is not recognized: a frame has to be requested. */
  private applyChange(
    c: Extract<UiUpdate, { update: "patch" }>["changes"][number],
  ): boolean {
    const s = this.snapshot;
    if (s === null) {
      return false;
    }
    switch (c.change) {
      case "cursor": {
        const slot = browser(s, c.slot_id);
        if (slot === null || slot.generation !== c.generation) {
          return true;
        }
        slot.cursor = c.cursor;
        for (const r of slot.rows) {
          r.selected = c.cursor !== null && r.key === c.cursor;
        }
        return true;
      }
      case "rows": {
        const slot = browser(s, c.slot_id);
        if (slot === null) {
          return true;
        }
        slot.generation = c.generation;
        slot.first_visible = c.first_visible;
        slot.rows = c.rows;
        slot.icon_column = c.icon_column;
        // The TOTAL, which is the scroll's height. Without this the listing
        // kept the first page's (100) through the whole drain — afterwards
        // too, because the last batch is also a patch — and a five-thousand
        // file directory hit that ceiling.
        if (c.total_rows !== null) {
          slot.total_rows = c.total_rows;
        }
        const cur = c.rows.find((r) => r.selected);
        if (cur !== undefined) {
          slot.cursor = cur.key;
        }
        return true;
      }
      case "browser_header": {
        const slot = browser(s, c.slot_id);
        if (slot === null) {
          return true;
        }
        // The header used to only move with the whole frame, so
        // `pane.names-encoding` retranscribed the rows and left the title
        // with the old reading.
        slot.path_display = c.path_display;
        slot.path_hostile = c.path_hostile;
        slot.skipped_note = c.skipped_note;
        slot.hidden_note = c.hidden_note;
        // The four new ones by the same path: a header that only moved with
        // the whole frame would leave the notice with the old reading, which
        // is the bug this patch exists to not repeat.
        slot.names_note = c.names_note ?? "";
        slot.filling_note = c.filling_note ?? "";
        slot.pruned_note = c.pruned_note ?? "";
        slot.marked_note = c.marked_note ?? "";
        slot.footer = c.footer ?? "";
        // The breadcrumbs and the space indicator (bridge 65) travel with
        // the header: they change when the directory changes, and for the
        // same reason.
        slot.path_segments = c.path_segments ?? [];
        slot.used_ratio = c.used_ratio ?? null;
        slot.marks = c.marks;
        slot.mark_ruler = c.mark_ruler ?? [];
        return true;
      }
      case "slot_state": {
        const slot = browser(s, c.slot_id);
        if (slot !== null) {
          slot.state = c.state;
        }
        return true;
      }
      case "status": {
        s.status = { message: c.message, banners: c.banners, pending: c.pending };
        return true;
      }
      case "slot_progress": {
        // Two pixels on the border of the pane something is arriving into
        // (ADR 0148). Travels apart from the listing because progress runs
        // at 30 Hz.
        for (const slot of s.slots) {
          if (slot.kind === "browser" && slot.slot_id === c.slot_id) {
            slot.progress = c.progress;
          }
        }
        return true;
      }
      case "tasks": {
        s.tasks = c.tasks;
        // The process panel's cursor travels with the dashboard: a task that
        // expires removes a row and shifts the rest, and without this the
        // panel kept highlighting row N while the cancel key acted on
        // another one.
        //
        // WITH NO tolerance for it being missing, and not by accident: the
        // field always travels — `Option<u64>` without `skip_serializing_if`
        // writes `null` — and a host on another bridge does not even reach
        // here, because the version is compared above and a mismatch is a
        // fatal screen. `null` is "no row chosen", which is what an empty
        // dashboard says.
        for (const slot of s.slots) {
          if (slot.kind === "processes") {
            slot.cursor = c.cursor;
          }
        }
        return true;
      }
      case "dialogs":
        s.dialogs = c.dialogs;
        return true;
      case "connection": {
        const { change: _c, ...rest } = c;
        s.connection = rest;
        return true;
      }
      case "columns": {
        const slot = browser(s, c.slot_id);
        if (slot !== null) {
          slot.columns = c.columns;
        }
        return true;
      }
      case "ai_rename":
        s.ai_rename = c.ai_rename;
        return true;
      case "organize":
        s.organize = c.organize;
        return true;
      case "viewer":
        s.viewer = c.viewer;
        return true;
      case "which_key":
        s.whichkey = c.whichkey;
        return true;
      case "menu":
        s.menu = c.menu;
        return true;
      case "panel_bar":
        s.panel_bar = c.panel_bar;
        return true;
      case "status_items":
        s.status_items = c.status_items;
        return true;
      case "profiles":
        s.profiles = c.profiles;
        return true;
      case "wizard":
        s.wizard = c.wizard;
        return true;
      case "splash":
        s.splash = c.splash;
        return true;
      case "palette":
        s.palette = c.palette;
        return true;
      case "goto":
        s.goto = c.goto;
        return true;
      case "help":
        s.help = c.help;
        return true;
      case "settings":
        s.settings = c.settings;
        return true;
      case "agents":
        s.agents = c.agents;
        return true;
      case "plugin_output":
        s.plugin_output = c.output;
        return true;
      case "program_output":
        s.program_output = c.output;
        return true;
      case "extensions":
        s.extensions = c.extensions;
        return true;
      case "theme":
        s.theme = c.theme;
        return true;
      case "picker":
        s.picker = c.picker;
        return true;
      case "layouts":
        s.layouts = c.layouts;
        return true;
      case "columns_picker":
        s.columns = c.columns;
        return true;
      case "search":
        s.search = c.search;
        return true;
      case "compare":
        s.compare = c.compare;
        return true;
      case "sync":
        s.sync = c.sync;
        return true;
      case "layout": {
        s.layout = { cells: c.cells, placements: c.placements, tabs: c.tabs };
        // Focus belongs to whoever holds the `active` role, and the host
        // says who that is.
        const active = c.placements.find((p) => p.role === "active");
        s.focus = active === undefined ? null : active.slot_id;
        return true;
      }
      default:
        return false;
    }
  }
}

function browser(
  s: ViewSnapshot,
  slotId: number,
): Extract<ViewSnapshot["slots"][number], { kind: "browser" }> | null {
  for (const slot of s.slots) {
    if (slot.kind === "browser" && slot.slot_id === slotId) {
      return slot;
    }
  }
  return null;
}
