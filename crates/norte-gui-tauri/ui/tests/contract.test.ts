import { describe, expect, it } from "vitest";

import { Screen } from "../src/render";
import { Session } from "../src/session";
import { BRIDGE_VERSION } from "../src/types";
import type { HostCatalog, UiAction, UiUpdate, ViewSnapshot } from "../src/types";
import { realCatalog, golden } from "./fixtures";

/** A screen mounted on a clean DOM, with the real catalogue. */
function mount(): { screen: Screen; sent: UiAction[]; root: HTMLElement } {
  document.body.replaceChildren();
  const nodes = Array.from({ length: 22 }, () => document.createElement("div"));
  const root = document.createElement("main");
  document.body.append(root, ...nodes);
  document.documentElement.style.setProperty("--cell-h", "20px");
  document.documentElement.style.setProperty("--cell-w", "8px");
  const catalog: HostCatalog = {
    bridge_version: BRIDGE_VERSION,
    instance_id: "host-1",
    locale: "es",
    strings: realCatalog(),
    theme: {},
    measure: false,
  };
  const sent: UiAction[] = [];
  const [
    menu,
    palette,
    whichkey,
    help,
    settings,
    extensions,
    theme,
    picker,
    profiles,
    layouts,
    columns,
    search,
    compare,
    sync,
    agents,
    pluginOutput,
    programOutput,
    viewer,
    dialogs,
    aiRename,
    organize,
    splash,
  ] = nodes;
  const panelBar = document.createElement("div");
  const screen = new Screen(
    root,
    menu as HTMLElement,
    panelBar,
    palette as HTMLElement,
    whichkey as HTMLElement,
    help as HTMLElement,
    settings as HTMLElement,
    extensions as HTMLElement,
    theme as HTMLElement,
    picker as HTMLElement,
    profiles as HTMLElement,
    layouts as HTMLElement,
    columns as HTMLElement,
    search as HTMLElement,
    compare as HTMLElement,
    sync as HTMLElement,
    agents as HTMLElement,
    pluginOutput as HTMLElement,
    programOutput as HTMLElement,
    viewer as HTMLElement,
    dialogs as HTMLElement,
    aiRename as HTMLElement,
    organize as HTMLElement,
    splash as HTMLElement,
    document.createElement("div"),
    catalog,
    (a: UiAction) => sent.push(a),
  );
  return { screen, sent, root };
}

describe("the contract with the host", () => {
  it("reads the snapshot from the golden corpus and paints it as state", () => {
    const updates = golden("updates.json");
    const snapshot = updates["snapshot"] as UiUpdate & { update: "snapshot" };
    const s = new Session();
    const out = s.receive({
      bridge_version: BRIDGE_VERSION,
      instance_id: "host-1",
      sequence: 0,
      payload: snapshot,
    });
    expect(out.kind).toBe("applied");
    const view = s.view() as ViewSnapshot;
    expect(view.layout.placements.length).toBeGreaterThan(0);
    expect(view.slots.length).toBeGreaterThan(0);
    // A hostile name arrives MARKED: that's the guarantee the renderer paints.
    const browser = view.slots.find((x) => x.kind === "browser");
    expect(browser?.kind).toBe("browser");
    if (browser?.kind === "browser") {
      expect(browser.rows.some((r) => r.hostile)).toBe(true);
    }
  });

  it("applies every change in the corpus without falling over on any of them", () => {
    const changes = golden("changes.json");
    for (const [name, change] of Object.entries(changes)) {
      const s = new Session();
      const updates = golden("updates.json");
      s.receive({
        bridge_version: BRIDGE_VERSION,
        instance_id: "host-1",
        sequence: 10,
        payload: updates["snapshot"] as UiUpdate,
      });
      const out = s.receive({
        bridge_version: BRIDGE_VERSION,
        instance_id: "host-1",
        sequence: 11,
        payload: {
          update: "patch",
          base_sequence: 10,
          changes: [change],
        } as UiUpdate,
      });
      expect(out.kind, `change ${name} applies`).toBe("applied");
    }
  });

  it("covers every change the host knows how to emit", () => {
    // 1:1 coverage with `ViewChange` variants: a new change in Rust that this
    // renderer doesn't know how to apply has to break HERE.
    const changes = Object.keys(golden("changes.json")).sort();
    expect(changes).toEqual([
      "agents",
      "ai_rename",
      "browser_header",
      "columns",
      "columns_picker",
      "compare",
      "connection",
      "cursor",
      "dialogs",
      "extensions",
      "goto",
      "help",
      "layout",
      "layouts",
      "menu",
      "organize",
      "palette",
      "panel_bar",
      "picker",
      "plugin_output",
      "profiles",
      "program_output",
      "rows",
      "search",
      "settings",
      "slot_state",
      "status",
      "status_items",
      "sync",
      "tasks",
      // Two `tasks` cases, not two variants: this list is the corpus cases'
      // NAMES, and the empty board is the only way its cursor comes out `null`.
      "tasks_vacio",
      "theme",
      "viewer",
      "which_key",
      "wizard",
    ]);
  });

  it("PAINTS the corpus snapshot, slot by slot and overlay by overlay", () => {
    // The contract isn't really checked until the renderer paints the host's
    // reference data. Before, it only ever went into the `Session`, so a
    // variant the renderer didn't know how to paint — `SlotView::Places` was
    // one, and on top of that the only one with a newtype inside an enum
    // tagged by `kind` — crossed the corpus untouched.
    const updates = golden("updates.json");
    const snapshot = updates["snapshot"] as UiUpdate & { update: "snapshot" };
    const s = new Session();
    s.receive({
      bridge_version: BRIDGE_VERSION,
      instance_id: "host-1",
      sequence: 0,
      payload: snapshot,
    });
    const view = s.view() as ViewSnapshot;
    const { screen, root } = mount();
    screen.paint(view);

    // Every slot in the corpus has to have been painted as WHAT IT IS.
    for (const slot of view.slots) {
      const el = root.querySelector(`[data-slot-id="${String(slot.slot_id)}"]`);
      expect(el, `slot ${String(slot.slot_id)} is painted`).not.toBeNull();
    }
    // And the sidebar with its three row classes, which was the one no test
    // crossed.
    expect(document.querySelectorAll(".places-row").length).toBeGreaterThan(0);
    // No catalogue key has slipped through untranslated.
    const text = document.body.textContent ?? "";
    for (const suspect of ["dialog-", "modal-", "host-", "msg-", "cmd-"]) {
      expect(
        text.includes(suspect),
        `something was painted as its Fluent key (${suspect}…): ${text.slice(0, 200)}`,
      ).toBe(false);
    }
  });

  it("speaks the same bridge version as the host", () => {
    const env = golden("envelope.json")["shutdown"] as { bridge_version: number };
    expect(env.bridge_version).toBe(BRIDGE_VERSION);
  });
});
