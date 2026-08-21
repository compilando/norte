import { describe, expect, it } from "vitest";

import { Session } from "../src/session";
import { BRIDGE_VERSION } from "../src/types";
import type { UiUpdate, ViewSnapshot } from "../src/types";
import { golden } from "./fixtures";

describe("el contrato con el host", () => {
  it("lee el snapshot del corpus golden y lo pinta como estado", () => {
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
    // Un nombre hostil llega MARCADO: es la garantía que el renderer pinta.
    const browser = view.slots.find((x) => x.kind === "browser");
    expect(browser?.kind).toBe("browser");
    if (browser?.kind === "browser") {
      expect(browser.rows.some((r) => r.hostile)).toBe(true);
    }
  });

  it("aplica todos los cambios del corpus sin caerse en ninguno", () => {
    const changes = golden("changes.json");
    for (const [nombre, change] of Object.entries(changes)) {
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
      expect(out.kind, `el cambio ${nombre} se aplica`).toBe("applied");
    }
  });

  it("cubre todos los cambios que el host sabe emitir", () => {
    // Cobertura 1:1 con las variantes de `ViewChange`: un cambio nuevo en
    // Rust que este renderer no sepa aplicar tiene que romper AQUÍ.
    const changes = Object.keys(golden("changes.json")).sort();
    expect(changes).toEqual([
      "columns",
      "connection",
      "cursor",
      "dialogs",
      "extensions",
      "help",
      "layout",
      "palette",
      "rows",
      "settings",
      "slot_state",
      "status",
      "tasks",
      "viewer",
      "which_key",
    ]);
  });

  it("habla la misma versión del bridge que el host", () => {
    const env = golden("envelope.json")["shutdown"] as { bridge_version: number };
    expect(env.bridge_version).toBe(BRIDGE_VERSION);
  });
});
