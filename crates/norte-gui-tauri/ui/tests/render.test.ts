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
    palette: null,
    whichkey: null,
    help: null,
    viewer: null,
    locale: "es",
  };
}

function montar(): { screen: Screen; enviadas: UiAction[]; root: HTMLElement } {
  document.body.replaceChildren();
  const root = document.createElement("main");
  const palette = document.createElement("div");
  const whichkey = document.createElement("div");
  const help = document.createElement("div");
  const viewer = document.createElement("div");
  const dialogs = document.createElement("div");
  document.body.append(root, palette, whichkey, help, viewer, dialogs);
  document.documentElement.style.setProperty("--cell-h", `${CELL_H}px`);
  document.documentElement.style.setProperty("--cell-w", "8px");
  const enviadas: UiAction[] = [];
  const screen = new Screen(
    root,
    palette,
    whichkey,
    help,
    viewer,
    dialogs,
    catalogo(),
    (a: UiAction) => enviadas.push(a),
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

describe("which-key", () => {
  function conPanel() {
    const v = vista({});
    v.whichkey = {
      title: "ctrl+x",
      rows: [
        {
          chord: "g",
          label: "Ir al principio",
          enabled: true,
          opens_sequence: false,
          reason: "",
        },
        { chord: "s", label: "Más", enabled: true, opens_sequence: true, reason: "" },
        {
          chord: "p",
          label: "Empaquetar",
          enabled: false,
          opens_sequence: false,
          reason: "aquí no",
        },
      ],
    };
    return v;
  }

  it("enseña las continuaciones con su etiqueta, y marca las que abren otra secuencia", () => {
    const { screen } = montar();
    screen.paint(conPanel());
    const filas = document.querySelectorAll(".whichkey-row");
    expect(filas).toHaveLength(3);
    expect(filas[0]?.textContent).toBe("gIr al principio");
    // Una que abre secuencia se MARCA en vez de nombrar un comando que no
    // ejecuta.
    expect(filas[1]?.textContent).toContain("…");
  });

  it("una tecla que aquí no se puede dice por qué, y no se esconde", () => {
    const { screen } = montar();
    screen.paint(conPanel());
    const apagada = document.querySelectorAll('.whichkey-row[data-enabled="false"]');
    expect(apagada).toHaveLength(1);
    expect(apagada[0]?.textContent).toContain("aquí no");
  });

  it("no es un diálogo: no captura el foco", () => {
    const { screen } = montar();
    screen.paint(conPanel());
    const panel = document.querySelector(".whichkey");
    expect(panel?.getAttribute("role")).toBe("group");
    expect(panel?.getAttribute("aria-modal")).toBeNull();
  });

  it("sin prefijo a medias no hay panel", () => {
    const { screen } = montar();
    screen.paint(vista({}));
    expect(document.querySelector(".whichkey")).toBeNull();
  });
});

describe("la paleta", () => {
  function conPaleta(cursor: number | null) {
    const v = vista({});
    v.palette = {
      query: "cur",
      rows: [
        { text: "cursor.up", desc: "subir el cursor", chord: "Up", enabled: true },
        { text: "cursor.down", desc: "bajar el cursor", chord: "Down", enabled: true },
      ],
      cursor,
      total: 24,
    };
    return v;
  }

  it("es modal, dice cuánto acota y marca la selección", () => {
    const { screen } = montar();
    screen.paint(conPaleta(1));
    const caja = document.querySelector(".palette") as HTMLElement;
    expect(caja.getAttribute("aria-modal")).toBe("true");
    expect(document.querySelector(".palette-count")?.textContent).toBe("2/24");
    const lista = document.querySelector(".palette-rows") as HTMLElement;
    expect(lista.getAttribute("aria-activedescendant")).toBe("palette-row-1");
    const sel = document.querySelectorAll('.palette-row[aria-selected="true"]');
    expect(sel).toHaveLength(1);
    expect(sel[0]?.textContent).toContain("cursor.down");
  });

  it("cada fila enseña su atajo real", () => {
    const { screen } = montar();
    screen.paint(conPaleta(0));
    const chords = [...document.querySelectorAll(".palette-chord")].map(
      (c) => c.textContent,
    );
    expect(chords).toEqual(["Up", "Down"]);
  });

  it("sin coincidencias lo dice en vez de quedarse en blanco", () => {
    const { screen } = montar();
    const v = conPaleta(null);
    if (v.palette !== null) {
      v.palette.rows = [];
    }
    screen.paint(v);
    expect(document.querySelector(".palette-rows .empty")).not.toBeNull();
  });

  it("cerrada, no tapa nada", () => {
    const { screen } = montar();
    screen.paint(vista({}));
    expect(document.querySelector(".palette")).toBeNull();
  });
});

describe("la ayuda", () => {
  /** Una página con prosa, marcas ya resueltas y una fila apagada. */
  function conAyuda(): ViewSnapshot {
    const v = vista({});
    v.help = {
      title: "Copiar",
      topic_id: "copying",
      badge: null,
      sidebar: [
        { row: "group", label: "Lo básico" },
        { row: "topic", title: "Copiar", current: true },
        { row: "topic", title: "Marcar", current: false },
      ],
      cursor: 1,
      focus: "topics",
      blocks: [
        { block: "heading", level: 1, text: "Copiar ficheros" },
        {
          block: "paragraph",
          spans: [
            { span: "text", text: "Pulsa " },
            { span: "command", text: "F5", is_chord: true },
            { span: "text", text: " para copiar." },
          ],
        },
        {
          block: "bullets",
          items: [
            [{ span: "strong", text: "Ojo" }],
            [{ span: "emph", text: "con esto" }],
          ],
        },
        { block: "code", lang: "sh", text: "norte --help" },
        { block: "table", header: ["Tecla", "Qué hace"], rows: [["F5", "copiar"]] },
        {
          block: "callout",
          kind: "warn",
          spans: [{ span: "text", text: "Cuidado" }],
        },
        {
          block: "keys",
          rows: [
            { chord: "F5", label: "copiar", enabled: true, reason: "" },
            { chord: "F6", label: "mover", enabled: false, reason: "aquí no" },
          ],
        },
      ],
      actions: [
        { label: "copiar", chord: "F5", enabled: true, reason: "", opens_topic: false },
        {
          label: "mover",
          chord: "F6",
          enabled: false,
          reason: "aquí no",
          opens_topic: false,
        },
        { label: "Marcar", chord: "", enabled: true, reason: "", opens_topic: true },
      ],
      action_cursor: 0,
      filter: "",
      filtering: false,
      can_back: false,
    };
    return v;
  }

  it("es modal y estructura la página con encabezados y listas de verdad", () => {
    const { screen } = montar();
    screen.paint(conAyuda());
    const caja = document.querySelector(".help") as HTMLElement;
    expect(caja.getAttribute("role")).toBe("dialog");
    expect(caja.getAttribute("aria-modal")).toBe("true");
    // El título de la página es el `h1`; un encabezado del cuerpo baja un
    // nivel, así que la jerarquía no tiene dos raíces.
    expect(caja.querySelectorAll("h1")).toHaveLength(1);
    expect(caja.querySelector("h1")?.textContent).toBe("Copiar");
    expect(caja.querySelector("h2")?.textContent).toBe("Copiar ficheros");
    expect(caja.querySelectorAll(".help-bullets li")).toHaveLength(2);
    expect(caja.querySelector("pre code")?.textContent).toBe("norte --help");
    expect(caja.querySelectorAll(".help-table th")).toHaveLength(2);
    expect(caja.querySelector(".help-callout")?.getAttribute("data-kind")).toBe("warn");
  });

  it("una marca del corpus llega como TECLA y nunca como marcado", () => {
    const { screen } = montar();
    screen.paint(conAyuda());
    const kbd = document.querySelector(".help-body kbd");
    expect(kbd?.textContent).toBe("F5");
    // Ni una marca sin resolver ni un `{{cmd:`: el host las convierte.
    expect(document.querySelector(".help")?.textContent).not.toContain("{{cmd:");
  });

  it("NADA de lo que llega se interpreta como HTML", () => {
    const { screen } = montar();
    const v = conAyuda();
    if (v.help !== null) {
      // Texto de tercero: un `help.md` de un plugin. Si algo de esto se
      // pintara con `innerHTML`, aquí aparecería un nodo `<img>` y un
      // atributo `onerror` — que es exactamente el fallo del que protege
      // que el vocabulario de bloques sea cerrado.
      v.help.title = "<img src=x onerror=alert(1)>";
      v.help.blocks = [
        {
          block: "paragraph",
          spans: [{ span: "text", text: "<script>alert(1)</script>" }],
        },
        { block: "code", lang: null, text: "<b>no</b>" },
      ];
      v.help.sidebar = [{ row: "topic", title: "<i>x</i>", current: true }];
      v.help.actions = [];
    }
    screen.paint(v);
    const caja = document.querySelector(".help") as HTMLElement;
    expect(caja.querySelector("img")).toBeNull();
    expect(caja.querySelector("script")).toBeNull();
    expect(caja.querySelector("b")).toBeNull();
    expect(caja.querySelector("i")).toBeNull();
    // Y el texto SÍ está: escapado, no perdido.
    expect(caja.textContent).toContain("<script>alert(1)</script>");
    expect(caja.querySelector("h1")?.textContent).toBe("<img src=x onerror=alert(1)>");
  });

  it("una fila apagada dice por qué, y un click en una viva la activa", () => {
    const { screen, enviadas } = montar();
    screen.paint(conAyuda());
    const filas = [...document.querySelectorAll(".help-action")];
    expect(filas).toHaveLength(3);
    expect(filas[1]?.getAttribute("data-enabled")).toBe("false");
    expect(filas[1]?.textContent).toContain("aquí no");

    (filas[0] as HTMLElement).click();
    expect(enviadas).toEqual([{ action: "help_activate", index: 0 }]);
    // Una fila apagada no manda nada: el host ya dijo que no se puede.
    (filas[1] as HTMLElement).click();
    expect(enviadas).toHaveLength(1);
  });

  it("la hoja de teclado explica cada tecla que esta ventana no hace", () => {
    const { screen } = montar();
    screen.paint(conAyuda());
    const filas = [...document.querySelectorAll(".help-keys tbody tr")];
    expect(filas).toHaveLength(2);
    expect(filas[0]?.querySelector("th")?.textContent).toBe("F5");
    expect(filas[1]?.getAttribute("data-enabled")).toBe("false");
    expect(filas[1]?.textContent).toContain("aquí no");
  });

  it("un click en la lateral pide ESA página", () => {
    const { screen, enviadas } = montar();
    screen.paint(conAyuda());
    const lista = document.querySelector(".help-topic-rows") as HTMLElement;
    expect(lista.getAttribute("aria-activedescendant")).toBe("help-topic-1");
    const paginas = [...document.querySelectorAll(".help-topic")];
    (paginas[1] as HTMLElement).click();
    // La fila 0 es la CABECERA del grupo: la segunda página es la fila 2.
    expect(enviadas).toEqual([{ action: "help_select_topic", row: 2 }]);
  });

  it("cerrada, no tapa nada", () => {
    const { screen } = montar();
    screen.paint(vista({}));
    expect(document.querySelector(".help")).toBeNull();
  });
});
