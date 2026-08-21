// La disciplina de la secuencia, y el estado que el renderer tiene derecho a
// guardar: una COPIA de lo último que el host dijo, para pintarla.
//
// Las tres reglas son las de la decisión D7, y aquí no se negocia ninguna:
// una versión que no se reconoce no se interpreta a medias, un parche sobre
// otra base no se aplica, y un hueco en la secuencia se resuelve pidiendo una
// foto — jamás adivinando lo que faltó.

import { BRIDGE_VERSION } from "./types";
import type { BridgeEnvelope, UiUpdate, ViewSnapshot } from "./types";

/** Qué pasó con un mensaje. */
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

  /** La pantalla que hay que pintar, o `null` si todavía no hay ninguna. */
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
      // Otra vida del host. No es un error y no muta nada.
      return { kind: "ignored", why: "other-instance" };
    }
    if (env.sequence <= this.seq) {
      return { kind: "ignored", why: "old-sequence" };
    }
    const p = env.payload;
    if (p.update === "snapshot") {
      // Una foto REEMPLAZA, así que cierra cualquier hueco: se acepta venga
      // de donde venga en la secuencia.
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
      // Un parche sobre otra base no se aplica «casi bien»: no se aplica.
      return { kind: "gap", expected: this.seq, got: p.base_sequence };
    }
    for (const c of p.changes) {
      if (!this.applyChange(c)) {
        // Un cambio que este renderer no conoce NO se descarta avanzando la
        // secuencia: eso deja una copia divergente de la pantalla que pasa
        // todas las comprobaciones posteriores. Se pide una foto.
        return { kind: "gap", expected: this.seq, got: env.sequence };
      }
    }
    this.seq = env.sequence;
    return { kind: "applied" };
  }

  /** `false` si el cambio no se reconoce: hay que pedir una foto. */
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
        const cur = c.rows.find((r) => r.selected);
        if (cur !== undefined) {
          slot.cursor = cur.key;
        }
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
      case "tasks":
        s.tasks = c.tasks;
        return true;
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
      case "viewer":
        s.viewer = c.viewer;
        return true;
      case "which_key":
        s.whichkey = c.whichkey;
        return true;
      case "palette":
        s.palette = c.palette;
        return true;
      case "help":
        s.help = c.help;
        return true;
      case "settings":
        s.settings = c.settings;
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
      case "search":
        s.search = c.search;
        return true;
      case "layout": {
        s.layout = { cells: c.cells, placements: c.placements };
        // El foco es de quien tenga el papel `active`, y lo dice el host.
        const activo = c.placements.find((p) => p.role === "active");
        s.focus = activo === undefined ? null : activo.slot_id;
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
