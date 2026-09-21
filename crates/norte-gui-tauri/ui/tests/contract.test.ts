import { describe, expect, it } from "vitest";

import { Screen } from "../src/render";
import { Session } from "../src/session";
import { BRIDGE_VERSION } from "../src/types";
import type { HostCatalog, UiAction, UiUpdate, ViewSnapshot } from "../src/types";
import { catalogoReal, golden } from "./fixtures";

/** Una pantalla montada sobre un DOM limpio, con el catálogo de verdad. */
function montar(): { screen: Screen; enviadas: UiAction[]; root: HTMLElement } {
  document.body.replaceChildren();
  const nodos = Array.from({ length: 22 }, () => document.createElement("div"));
  const root = document.createElement("main");
  document.body.append(root, ...nodos);
  document.documentElement.style.setProperty("--cell-h", "20px");
  document.documentElement.style.setProperty("--cell-w", "8px");
  const catalog: HostCatalog = {
    bridge_version: BRIDGE_VERSION,
    instance_id: "host-1",
    locale: "es",
    strings: catalogoReal(),
    theme: {},
    measure: false,
  };
  const enviadas: UiAction[] = [];
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
  ] = nodos;
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
    (a: UiAction) => enviadas.push(a),
  );
  return { screen, enviadas, root };
}

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
      // Dos casos de `tasks`, no dos variantes: esta lista son los NOMBRES de
      // los casos del corpus, y el tablero vacío es la única forma en la que
      // su cursor sale `null`.
      "tasks_vacio",
      "theme",
      "viewer",
      "which_key",
      "wizard",
    ]);
  });

  it("PINTA el snapshot del corpus, hueco a hueco y overlay a overlay", () => {
    // El contrato no se comprueba de verdad hasta que el renderer pinta los
    // datos de referencia del host. Antes solo se metían en la `Session`, así
    // que una variante que el renderer no supiera pintar —`SlotView::Places`
    // era una, y encima es la única con newtype dentro de un enum etiquetado
    // por `kind`— cruzaba el corpus sin que nada la tocara.
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
    const { screen, root } = montar();
    screen.paint(view);

    // Cada hueco del corpus tiene que haberse pintado como LO QUE ES.
    for (const slot of view.slots) {
      const el = root.querySelector(`[data-slot-id="${String(slot.slot_id)}"]`);
      expect(el, `el hueco ${String(slot.slot_id)} se pinta`).not.toBeNull();
    }
    // Y la barra lateral con sus tres clases de fila, que es la que no
    // cruzaba por ningún test.
    expect(document.querySelectorAll(".places-row").length).toBeGreaterThan(0);
    // Ninguna clave del catálogo se ha escapado sin traducir.
    const texto = document.body.textContent ?? "";
    for (const sospechosa of ["dialog-", "modal-", "host-", "msg-", "cmd-"]) {
      expect(
        texto.includes(sospechosa),
        `algo se pintó como su clave Fluent (${sospechosa}…): ${texto.slice(0, 200)}`,
      ).toBe(false);
    }
  });

  it("habla la misma versión del bridge que el host", () => {
    const env = golden("envelope.json")["shutdown"] as { bridge_version: number };
    expect(env.bridge_version).toBe(BRIDGE_VERSION);
  });
});
