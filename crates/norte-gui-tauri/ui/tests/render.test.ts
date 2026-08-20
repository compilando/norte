// El pintado: virtualización, estados del listado, accesibilidad y gestos.
//
// Todo con un bridge FALSO. No hace falta ni ventana ni WebKitGTK para
// comprobar lo que este renderer promete.

import { beforeEach, describe, expect, it, vi } from "vitest";

import { Screen } from "../src/render";
import type {
  BrowserSlotView,
  HostCatalog,
  RowView,
  UiAction,
  ViewSnapshot,
} from "../src/types";

const CELL_H = 20;

function catalogo(): HostCatalog {
  return {
    bridge_version: 5,
    instance_id: "host-1",
    locale: "es",
    strings: { "listing-empty": "vacío", "hostile-name": "nombre hostil" },
    theme: {},
    measure: false,
  };
}

function fila(key: number, nombre: string, extra: Partial<RowView> = {}): RowView {
  return {
    key,
    display_name: nombre,
    hostile: false,
    kind: "file",
    selected: false,
    marked: false,
    cells: [{ column: "size", text: "1.2 KiB" }],
    ...extra,
  };
}

function vista(browser: Partial<BrowserSlotView>): ViewSnapshot {
  return {
    connection: { state: "connected" },
    layout: {
      cells: [120, 40],
      placements: [
        { slot_id: 1, x: 0, y: 0, width: 60, height: 38, role: "active", focus_index: 0 },
        { slot_id: 4, x: 0, y: 39, width: 120, height: 1, role: null, focus_index: 1 },
      ],
    },
    slots: [
      {
        kind: "browser",
        slot_id: 1,
        generation: 1,
        path_display: "⟨file⟩/casa",
        path_hostile: false,
        total_rows: 2,
        first_visible: 0,
        rows: [fila(0, "a.txt"), fila(1, "b.txt")],
        cursor: 0,
        marks: 0,
        columns: [
          { id: "name", label: "Nombre", sort: "asc", sortable: true },
          { id: "size", label: "Tamaño", sort: null, sortable: true },
        ],
        state: { state: "ready" },
        quick: null,
        ...browser,
      },
      { kind: "unsupported", slot_id: 4, kind_name: "status" },
    ],
    focus: 1,
    status: { message: "2 entradas", banners: [], pending: null },
    dialogs: [],
    tasks: [],
    viewer: null,
    locale: "es",
  };
}

function montar(): { screen: Screen; enviadas: UiAction[]; root: HTMLElement } {
  document.body.replaceChildren();
  const root = document.createElement("main");
  const viewer = document.createElement("div");
  const dialogs = document.createElement("div");
  document.body.append(root, viewer, dialogs);
  document.documentElement.style.setProperty("--cell-h", `${CELL_H}px`);
  document.documentElement.style.setProperty("--cell-w", "8px");
  const enviadas: UiAction[] = [];
  const screen = new Screen(root, viewer, dialogs, catalogo(), (a: UiAction) =>
    enviadas.push(a),
  );
  return { screen, enviadas, root };
}

describe("Screen", () => {
  beforeEach(() => {
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      cb(0);
      return 0;
    });
  });

  it("coloca cada hueco donde el host dijo, en píxeles de celda", () => {
    const { screen, root } = montar();
    screen.paint(vista({}));
    const slots = root.querySelectorAll(".slot");
    expect(slots).toHaveLength(2);
    const primero = slots[0] as HTMLElement;
    expect(primero.style.width).toBe(`${60 * 8}px`);
    expect(primero.dataset["role"]).toBe("active");
  });

  it("no pinta cien mil filas para enseñar cuarenta", () => {
    const { screen, root } = montar();
    const rows = Array.from({ length: 40 }, (_, i) =>
      fila(1000 + i, `f${String(i)}.txt`),
    );
    screen.paint(vista({ total_rows: 100_000, first_visible: 1000, rows }));
    expect(root.querySelectorAll(".row")).toHaveLength(40);
    const canvas = root.querySelector(".canvas") as HTMLElement;
    // El alto SÍ es el del directorio entero: la barra de scroll no miente.
    expect(canvas.style.height).toBe(`${100_000 * CELL_H}px`);
    const primera = root.querySelector(".row") as HTMLElement;
    expect(primera.style.top).toBe(`${1000 * CELL_H}px`);
    expect(primera.getAttribute("aria-rowindex")).toBe("1001");
  });

  it("un nombre hostil se MARCA, jamás se esconde", () => {
    const { screen, root } = montar();
    screen.paint(vista({ rows: [fila(0, "caf�.txt", { hostile: true })] }));
    const name = root.querySelector(".cell-name") as HTMLElement;
    expect(name.classList.contains("hostile")).toBe(true);
    expect(name.textContent).toContain("caf�.txt");
  });

  it("un nombre con HTML dentro es TEXTO, no marcado", () => {
    const { screen, root } = montar();
    screen.paint(vista({ rows: [fila(0, "<img src=x onerror=alert(1)>")] }));
    const name = root.querySelector(".cell-name") as HTMLElement;
    expect(name.querySelector("img")).toBeNull();
    expect(name.textContent).toBe("<img src=x onerror=alert(1)>");
  });

  it("un listado vacío lo dice", () => {
    const { screen, root } = montar();
    screen.paint(vista({ total_rows: 0, rows: [], cursor: null }));
    expect(root.querySelector(".empty")?.textContent).toBe("vacío");
  });

  it("cargando se anuncia, y un fallo se cuenta con su motivo", () => {
    const { screen, root } = montar();
    screen.paint(vista({ state: { state: "loading" } }));
    expect(root.querySelector(".scroller")?.getAttribute("aria-busy")).toBe("true");
    screen.paint(
      vista({
        state: { state: "error", reason_key: "listing-failed", detail: "EACCES" },
      }),
    );
    const err = root.querySelector(".error") as HTMLElement;
    expect(err.getAttribute("role")).toBe("alert");
    expect(err.textContent).toContain("EACCES");
  });

  it("las filas llevan roles y estado para el lector de pantalla", () => {
    const { screen, root } = montar();
    screen.paint(
      vista({ rows: [fila(0, "a.txt", { selected: true }), fila(1, "b.txt")] }),
    );
    const grid = root.querySelector(".scroller") as HTMLElement;
    expect(grid.getAttribute("role")).toBe("grid");
    expect(grid.getAttribute("aria-rowcount")).toBe("2");
    const seleccionada = root.querySelector('.row[aria-selected="true"]') as HTMLElement;
    expect(grid.getAttribute("aria-activedescendant")).toBe(seleccionada.id);
  });

  it("un click señala la fila; un doble click la abre", () => {
    const { screen, enviadas, root } = montar();
    screen.paint(vista({}));
    const fila1 = root.querySelectorAll(".row")[1] as HTMLElement;
    fila1.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(enviadas.at(-1)).toEqual({
      action: "select_row",
      slot_id: 1,
      key: 1,
      generation: 1,
    });
    fila1.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    expect(enviadas.at(-1)).toEqual({
      action: "activate",
      slot_id: 1,
      key: 1,
      generation: 1,
    });
  });

  it("shift+click manda UN rango: quién entra en él lo decide el host", () => {
    const { screen, enviadas, root } = montar();
    screen.paint(
      vista({ rows: [fila(0, "a", { selected: true }), fila(1, "b"), fila(2, "c")] }),
    );
    const tercera = root.querySelectorAll(".row")[2] as HTMLElement;
    tercera.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, shiftKey: true }));
    expect(enviadas.at(-1)).toEqual({
      action: "mark_range",
      slot_id: 1,
      from: 0,
      to: 2,
      generation: 1,
    });
  });

  it("ctrl+click marca una sola", () => {
    const { screen, enviadas, root } = montar();
    screen.paint(vista({}));
    const fila0 = root.querySelector(".row") as HTMLElement;
    fila0.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, ctrlKey: true }));
    expect(enviadas.at(-1)).toEqual({
      action: "toggle_mark",
      slot_id: 1,
      key: 0,
      generation: 1,
    });
  });

  it("el scroll no cruza: cruza QUÉ filas hacen falta", () => {
    const { screen, enviadas, root } = montar();
    screen.paint(vista({ total_rows: 10_000 }));
    const scroller = root.querySelector(".scroller") as HTMLElement;
    Object.defineProperty(scroller, "scrollTop", { value: 400, configurable: true });
    Object.defineProperty(scroller, "clientHeight", { value: 200, configurable: true });
    scroller.dispatchEvent(new Event("scroll"));
    const ultima = enviadas.at(-1);
    expect(ultima?.action).toBe("set_visible_range");
    if (ultima?.action === "set_visible_range") {
      expect(ultima.first).toBe(12); // 400/20 = 20, menos 8 de overscan
      expect(ultima.count).toBe(26); // 200/20 = 10, más 16 de overscan
    }
  });

  it("la barra de estado se anuncia sin robar el foco", () => {
    const { screen, root } = montar();
    screen.paint(vista({}));
    const status = root.querySelectorAll(".slot")[1]?.querySelector(".statusbar");
    expect(status?.getAttribute("aria-live")).toBe("polite");
    expect(status?.textContent).toContain("2 entradas");
  });

  it("un diálogo es modal, tiene nombre y dice cuál respuesta destruye", () => {
    const { screen } = montar();
    const v = vista({});
    v.dialogs = [
      {
        id: 3,
        title_key: "modal-delete-title",
        body: ["a.txt"],
        choices: [
          { id: "confirm", label_key: "dialog-confirm", destructive: true },
          { id: "cancel", label_key: "dialog-cancel", destructive: false },
        ],
        input: null,
        input_hostile: false,
      },
    ];
    screen.paint(v);
    const dialog = document.querySelector('[role="dialog"]') as HTMLElement;
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.getAttribute("aria-labelledby")).toBeTruthy();
    const destructivo = dialog.querySelector('button[data-destructive="true"]');
    expect(destructivo).not.toBeNull();
  });

  it("responder un diálogo manda su id, no una posición", () => {
    const { screen, enviadas } = montar();
    const v = vista({});
    v.dialogs = [
      {
        id: 7,
        title_key: "t",
        body: [],
        choices: [{ id: "cancel", label_key: "dialog-cancel", destructive: false }],
        input: null,
        input_hostile: false,
      },
    ];
    screen.paint(v);
    const boton = document.querySelector(".choices button") as HTMLButtonElement;
    boton.click();
    expect(enviadas.at(-1)).toEqual({ action: "dialog", id: 7, choice: "cancel" });
  });
});

describe("la cabecera", () => {
  it("pinta las etiquetas que vinieron de Rust y marca la que ordena", () => {
    const { screen, root } = montar();
    screen.paint(vista({}));
    const cols = root.querySelectorAll(".slot-columns .col");
    expect([...cols].map((c) => c.textContent)).toEqual(["Nombre▲", "Tamaño"]);
    expect(cols[0]?.getAttribute("aria-sort")).toBe("ascending");
    expect(cols[1]?.getAttribute("aria-sort")).toBe("none");
  });

  it("un click en la cabecera manda el ID de la columna, no su posición", () => {
    const { screen, enviadas, root } = montar();
    screen.paint(vista({}));
    const size = root.querySelectorAll(".slot-columns .col")[1] as HTMLElement;
    size.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(enviadas.at(-1)).toEqual({ action: "sort_by", slot_id: 1, column: "size" });
  });

  it("una columna que no ordena no ofrece el gesto", () => {
    const { screen, enviadas, root } = montar();
    const v = vista({});
    const slot = v.slots[0];
    if (slot?.kind === "browser") {
      slot.columns = [{ id: "plugin:x/y", label: "X", sort: null, sortable: false }];
    }
    screen.paint(v);
    const col = root.querySelector(".slot-columns .col") as HTMLElement;
    col.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(enviadas.some((a) => a.action === "sort_by")).toBe(false);
  });
});

describe("el visor", () => {
  it("tapa la pantalla y dice con qué encoding está leyendo", () => {
    const { screen } = montar();
    const v = vista({});
    v.viewer = {
      path_display: "⟨file⟩/casa/notas.txt",
      path_hostile: false,
      encoding: "UTF-8",
      eol: "lf",
      hex: false,
      forced: false,
      had_errors: false,
      truncated: true,
      total_rows: 120,
      first_line: 0,
      lines: ["primera", "segunda"],
    };
    screen.paint(v);
    const doc = document.querySelector('[role="document"]') as HTMLElement;
    expect(doc.getAttribute("aria-label")).toContain("notas.txt");
    expect(doc.querySelector(".viewer-body")?.textContent).toBe("primera\nsegunda");
    expect(doc.querySelector(".viewer-meta")?.textContent).toContain("UTF-8");
  });

  it("un binario se pinta como hexadecimal y lo dice", () => {
    const { screen } = montar();
    const v = vista({});
    v.viewer = {
      path_display: "⟨file⟩/casa/raro.bin",
      path_hostile: false,
      encoding: "binario",
      eol: "none",
      hex: true,
      forced: false,
      had_errors: false,
      truncated: false,
      total_rows: 1,
      first_line: 0,
      lines: ["00000000  00 01 02 ff"],
    };
    screen.paint(v);
    expect(document.querySelector(".viewer-body")?.classList.contains("hexview")).toBe(
      true,
    );
    expect(document.querySelector(".viewer-meta")?.textContent).toContain("hex");
  });

  it("sin visor abierto, no hay nada que tape la pantalla", () => {
    const { screen } = montar();
    screen.paint(vista({}));
    expect(document.querySelector('[role="document"]')).toBeNull();
  });

  it("el contenido de un fichero es TEXTO, nunca marcado", () => {
    const { screen } = montar();
    const v = vista({});
    v.viewer = {
      path_display: "x",
      path_hostile: false,
      encoding: "UTF-8",
      eol: "lf",
      hex: false,
      forced: false,
      had_errors: false,
      truncated: false,
      total_rows: 1,
      first_line: 0,
      lines: ["<script>alert(1)</script>"],
    };
    screen.paint(v);
    const body = document.querySelector(".viewer-body") as HTMLElement;
    expect(body.querySelector("script")).toBeNull();
    expect(body.textContent).toBe("<script>alert(1)</script>");
  });
});

describe("la generación", () => {
  it("viaja con cada gesto de fila, y es la que se PINTÓ", () => {
    const { screen, enviadas, root } = montar();
    const v = vista({ generation: 7 });
    screen.paint(v);
    const fila = root.querySelector(".row") as HTMLElement;
    fila.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    const accion = enviadas.at(-1);
    expect(accion?.action).toBe("select_row");
    if (accion?.action === "select_row") {
      expect(accion.generation).toBe(7);
    }
  });

  it("se actualiza al repintar: un gesto posterior lleva la nueva", () => {
    const { screen, enviadas, root } = montar();
    screen.paint(vista({ generation: 7 }));
    screen.paint(vista({ generation: 8 }));
    const fila = root.querySelector(".row") as HTMLElement;
    fila.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    const accion = enviadas.at(-1);
    if (accion?.action === "select_row") {
      expect(accion.generation).toBe(8);
    }
  });
});

describe("el campo de texto de un diálogo", () => {
  function conDialogo(input: string, hostile: boolean) {
    const v = vista({});
    v.dialogs = [
      {
        id: 9,
        title_key: "modal-mkdir-title",
        body: [],
        choices: [{ id: "confirm", label_key: "dialog-confirm", destructive: false }],
        input,
        input_hostile: hostile,
      },
    ];
    return v;
  }

  it("no se pisa en cada repintado: lo tecleado manda", () => {
    const { screen } = montar();
    screen.paint(conDialogo("", false));
    const input = document.querySelector(".dialog input") as HTMLInputElement;
    // El usuario escribe; el host contesta con SU proyección.
    input.value = "carpeta nueva";
    screen.paint(conDialogo("carpeta nu…", false));
    expect(input.value).toBe("carpeta nueva");
  });

  it("un nombre que se pinta distinto de lo que es lo DICE", () => {
    const { screen } = montar();
    screen.paint(conDialogo("caf\ufffde.txt", true));
    const aviso = document.querySelector('.dialog [role="alert"]');
    expect(aviso).not.toBeNull();
  });
});
