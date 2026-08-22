// El pintado: virtualización, estados del listado, accesibilidad y gestos.
//
// Todo con un bridge FALSO. No hace falta ni ventana ni WebKitGTK para
// comprobar lo que este renderer promete.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { beforeEach, describe, expect, it, vi } from "vitest";

import { Screen } from "../src/render";
import { catalogoReal } from "./fixtures";
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
    // El catálogo DE VERDAD, no dos claves inventadas: con un fixture
    // inventado, una clave que falta se pinta igual que una que está.
    strings: catalogoReal(),
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
    badge: "",
    badge_hostile: false,
    badge_role: "",
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
        skipped_note: "",
        columns: [
          { id: "name", label: "Nombre", sort: "asc", sortable: true },
          { id: "size", label: "Tamaño", sort: null, sortable: true },
        ],
        state: { state: "ready" },
        quick: null,
        ...browser,
      },
      { kind_name_hostile: false, kind: "unsupported", slot_id: 4, kind_name: "status" },
    ],
    focus: 1,
    status: { message: "2 entradas", banners: [], pending: null },
    dialogs: [],
    tasks: [],
    palette: null,
    whichkey: null,
    help: null,
    settings: null,
    extensions: null,
    theme: null,
    picker: null,
    layouts: null,
    columns: null,
    search: null,
    compare: null,
    sync: null,
    plugin_output: null,
    viewer: null,
    ai_rename: null,
    locale: "es",
  };
}

function montar(opciones: { imageBytes?: () => Promise<ArrayBuffer> } = {}): {
  screen: Screen;
  enviadas: UiAction[];
  root: HTMLElement;
} {
  document.body.replaceChildren();
  const root = document.createElement("main");
  const palette = document.createElement("div");
  const whichkey = document.createElement("div");
  const help = document.createElement("div");
  const settings = document.createElement("div");
  const extensions = document.createElement("div");
  const theme = document.createElement("div");
  const picker = document.createElement("div");
  const layouts = document.createElement("div");
  const columns = document.createElement("div");
  const search = document.createElement("div");
  const compare = document.createElement("div");
  const sync = document.createElement("div");
  const pluginOutput = document.createElement("div");
  const viewer = document.createElement("div");
  const dialogs = document.createElement("div");
  const aiRename = document.createElement("div");
  document.body.append(
    root,
    palette,
    whichkey,
    help,
    settings,
    extensions,
    theme,
    picker,
    layouts,
    columns,
    search,
    viewer,
    dialogs,
    aiRename,
  );
  document.documentElement.style.setProperty("--cell-h", `${CELL_H}px`);
  document.documentElement.style.setProperty("--cell-w", "8px");
  const enviadas: UiAction[] = [];
  const screen = new Screen(
    root,
    palette,
    whichkey,
    help,
    settings,
    extensions,
    theme,
    picker,
    layouts,
    columns,
    search,
    compare,
    sync,
    pluginOutput,
    viewer,
    dialogs,
    aiRename,
    catalogo(),
    (a: UiAction) => enviadas.push(a),
    opciones.imageBytes ?? (() => Promise.resolve(new ArrayBuffer(0))),
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
        subject: null,
        asker: null,
        deadline: null,
        destination: null,
        body: [{ text: "a.txt", hostile: false }],
        overflow_note: "",
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
        subject: null,
        asker: null,
        deadline: null,
        destination: null,
        body: [],
        overflow_note: "",
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
      preview_by: "",
      preview_lossy: false,
      image: null,
      image_refused: "",
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
      preview_by: "",
      preview_lossy: false,
      image: null,
      image_refused: "",
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
      preview_by: "",
      preview_lossy: false,
      image: null,
      image_refused: "",
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
        subject: null,
        asker: null,
        deadline: null,
        destination: null,
        body: [],
        overflow_note: "",
        choices: [{ id: "confirm", label_key: "dialog-confirm", destructive: false }],
        input,
        input_hostile: hostile,
      },
    ];
    return v;
  }

  // El campo VIVO, reconsultado del DOM.
  //
  // Capturarlo una vez no vale: el diálogo se repinta con
  // `replaceChildren`, así que la referencia vieja queda desconectada y una
  // aserción sobre ella pasa mientras el campo de la pantalla está vacío.
  // Eso es exactamente lo que tapó que cada tecla vaciaba el campo.
  function campoVivo(): HTMLInputElement {
    const input = document.querySelector(".dialog input");
    expect(input).not.toBeNull();
    expect(input?.isConnected).toBe(true);
    return input as HTMLInputElement;
  }

  it("no se pisa en cada repintado: lo tecleado manda", () => {
    const { screen } = montar();
    screen.paint(conDialogo("", false));
    // El usuario escribe; el host contesta con SU proyección.
    campoVivo().value = "carpeta nueva";
    screen.paint(conDialogo("carpeta nu…", false));
    expect(campoVivo().value).toBe("carpeta nueva");
  });

  it("sobrevive a una tecla por parche, que es como se teclea de verdad", () => {
    const { screen, enviadas } = montar();
    screen.paint(conDialogo("", false));
    // Cada tecla provoca un `dialog_input` y el host contesta con un parche,
    // o sea un repintado. Se teclea letra a letra, como una persona.
    const nombre = "informe";
    for (let i = 1; i <= nombre.length; i += 1) {
      const campo = campoVivo();
      campo.value = nombre.slice(0, i);
      campo.dispatchEvent(new Event("input", { bubbles: true }));
      screen.paint(conDialogo(nombre.slice(0, i), false));
    }
    expect(campoVivo().value).toBe("informe");
    const ultima = enviadas.at(-1);
    expect(ultima?.action).toBe("dialog_input");
    if (ultima?.action === "dialog_input") {
      expect(ultima.text).toBe("informe");
    }
  });

  it("los bytes hostiles que se teclean llegan enteros, y se avisa", () => {
    const { screen, enviadas } = montar();
    screen.paint(conDialogo("", true));
    // `rtl_override` del corpus: lo que se apruebe tiene que ser lo que se
    // teclea, no una reconstrucción de ello.
    const hostil = "fact\u202Egpj.exe";
    const campo = campoVivo();
    campo.value = hostil;
    campo.dispatchEvent(new Event("input", { bubbles: true }));
    // El host contesta con su proyección: enmascarada y distinta.
    screen.paint(conDialogo("fact\uFFFDgpj.exe", true));
    expect(campoVivo().value).toBe(hostil);
    const ultima = enviadas.at(-1);
    if (ultima?.action === "dialog_input") {
      expect(ultima.text).toBe(hostil);
    }
    expect(document.querySelector('.dialog [role="alert"]')).not.toBeNull();
  });

  it("un nombre que se pinta distinto de lo que es lo DICE", () => {
    const { screen } = montar();
    screen.paint(conDialogo("caf\ufffde.txt", true));
    const aviso = document.querySelector('.dialog [role="alert"]');
    expect(aviso).not.toBeNull();
  });
});

describe("la imagen del visor", () => {
  function conImagen(image: ViewSnapshot["viewer"]): ViewSnapshot {
    const v = vista({});
    v.viewer = image;
    return v;
  }

  const base = {
    path_display: "⟨file⟩/casa/foto.png",
    path_hostile: false,
    encoding: "binario",
    eol: "none",
    hex: true,
    forced: false,
    had_errors: false,
    truncated: false,
    total_rows: 1,
    first_line: 0,
    lines: ["00000000  89 50 4e 47"],
    preview_by: "",
    preview_lossy: false,
  };

  it("pide los bytes APARTE y los pinta como blob", async () => {
    const bytes = new Uint8Array([1, 2, 3, 4]).buffer;
    const { screen } = montar({ imageBytes: () => Promise.resolve(bytes) });
    screen.paint(
      conImagen({
        ...base,
        image: { format: "PNG", width: 800, height: 600 },
        image_refused: "",
      }),
    );
    // La promesa se resuelve en el siguiente turno.
    await Promise.resolve();
    await Promise.resolve();
    const img = document.querySelector<HTMLImageElement>(".viewer-image");
    expect(img).not.toBeNull();
    // `blob:`, jamás `file:` ni `data:` (ADR 0069).
    expect(img?.src.startsWith("blob:")).toBe(true);
    // Con el tamaño DECLARADO, para que la caja no salte al cargar.
    expect(img?.width).toBe(800);
    expect(img?.height).toBe(600);
  });

  it("y cerrar el visor REVOCA el blob", async () => {
    const revocadas: string[] = [];
    const revoke = URL.revokeObjectURL.bind(URL);
    URL.revokeObjectURL = (u: string) => {
      revocadas.push(u);
      revoke(u);
    };
    try {
      const bytes = new Uint8Array([1, 2, 3]).buffer;
      const { screen } = montar({ imageBytes: () => Promise.resolve(bytes) });
      screen.paint(
        conImagen({
          ...base,
          image: { format: "PNG", width: 10, height: 10 },
          image_refused: "",
        }),
      );
      await Promise.resolve();
      await Promise.resolve();
      screen.paint(conImagen(null));
      // Un object URL sin revocar retiene sus bytes mientras viva el
      // documento, y esto son megas.
      expect(revocadas).toHaveLength(1);
    } finally {
      URL.revokeObjectURL = revoke;
    }
  });

  it("una imagen RECHAZADA se dice, y no se pide nada", () => {
    let pedida = false;
    const { screen } = montar({
      imageBytes: () => {
        pedida = true;
        return Promise.resolve(new ArrayBuffer(0));
      },
    });
    screen.paint(
      conImagen({
        ...base,
        image: null,
        image_refused: "imagen demasiado grande para previsualizarla",
      }),
    );
    const no = document.querySelector(".viewer-image-refused");
    expect(no?.textContent).toContain("demasiado grande");
    // Anunciado, para quien no mira la pantalla: caer al hexview en silencio
    // parece norte roto, no norte prudente.
    expect(no?.getAttribute("role")).toBe("status");
    expect(pedida).toBe(false);
    expect(document.querySelector(".viewer-image")).toBeNull();
  });
});

describe("la preview de un plugin en el visor", () => {
  it("dice de quién es lo que enseña, y aparte del aviso de pérdida", () => {
    const { screen } = montar();
    const v = vista({});
    v.viewer = {
      path_display: "⟨file⟩/casa/informe.pdf",
      path_hostile: false,
      encoding: "UTF-8",
      eol: "lf",
      hex: false,
      forced: false,
      had_errors: false,
      truncated: false,
      total_rows: 2,
      first_line: 0,
      lines: ["Informe anual"],
      preview_by: "via PDF de ACME",
      preview_lossy: true,
      image: null,
      image_refused: "",
    };
    screen.paint(v);
    const via = document.querySelector(".viewer-via");
    expect(via?.textContent).toBe("via PDF de ACME");
    // El aviso de pérdida en su PROPIO nodo: `had_errors` es el de la vista
    // cruda y este es el de la decodificación que se le dio al previewer.
    // Son dos decodificaciones, y confundirlas culpa al fichero de lo que
    // hizo la lectura.
    const aviso = document.querySelector(".viewer-via-lossy");
    expect(aviso).not.toBeNull();
    expect(aviso?.textContent).toBe(catalogoReal()["viewer-plugin-preview-lossy"] ?? "");
    expect(via?.contains(aviso)).toBe(false);
  });

  it("la RUTA es lo que se recorta, no las marcas", () => {
    const { screen } = montar();
    const v = vista({});
    v.viewer = {
      path_display: "⟨file⟩/casa/informe.pdf",
      path_hostile: false,
      encoding: "UTF-8",
      eol: "lf",
      hex: false,
      forced: false,
      had_errors: false,
      truncated: false,
      total_rows: 1,
      first_line: 0,
      lines: ["x"],
      preview_by: "via PDF de ACME",
      preview_lossy: true,
      image: null,
      image_refused: "",
    };
    screen.paint(v);
    const head = document.querySelector(".viewer-head");
    const ruta = head?.querySelector(".viewer-path");
    // La ruta en su PROPIO nodo y las marcas como HERMANAS suyas. Suelta como
    // texto era un item de flex anónimo que no se encoge, así que empujaba
    // fuera de la vista todo lo que viniera detrás. jsdom no hace layout, así
    // que lo que se clava aquí es la estructura que lo hace imposible.
    expect(ruta?.textContent).toContain("informe.pdf");
    expect(head?.querySelector(".viewer-via")?.parentElement).toBe(head);
    expect(ruta?.querySelector(".viewer-via")).toBeNull();
  });

  it("y sin plugin no se atribuye nada a nadie", () => {
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
      truncated: false,
      total_rows: 1,
      first_line: 0,
      lines: ["hola"],
      preview_by: "",
      preview_lossy: false,
      image: null,
      image_refused: "",
    };
    screen.paint(v);
    expect(document.querySelector(".viewer-via")).toBeNull();
    expect(document.querySelector(".viewer-via-lossy")).toBeNull();
  });
});

describe("el selector de columnas", () => {
  function conColumnas(): ViewSnapshot {
    const v = vista({});
    v.columns = {
      title: "Columnas — sftp",
      cursor: 1,
      note: "se aplica a esta ventana; no se guarda",
      rows: [
        {
          id: "name",
          label: "Nombre",
          hostile: false,
          enabled: true,
          format: "",
          format_locked: false,
          fixed: true,
        },
        {
          id: "size",
          label: "Tamaño",
          hostile: false,
          enabled: true,
          format: "iec",
          format_locked: false,
          fixed: false,
        },
        {
          id: "attr:posix.mode",
          label: "Permisos",
          hostile: false,
          enabled: false,
          format: "symbolic",
          format_locked: true,
          fixed: false,
        },
      ],
    };
    return v;
  }

  it("dice qué está encendido, qué es fijo y qué formato tiene cada una", () => {
    const { screen } = montar();
    screen.paint(conColumnas());
    const filas = [...document.querySelectorAll(".columns-row")];
    expect(filas).toHaveLength(3);
    // Encendida o no, al lector de pantalla y no solo al que mira.
    expect(filas[0]?.getAttribute("aria-checked")).toBe("true");
    expect(filas[2]?.getAttribute("aria-checked")).toBe("false");
    // El NOMBRE es fijo: ni se apaga ni se mueve.
    expect(filas[0]?.getAttribute("data-fixed")).toBe("true");
    expect(filas[1]?.getAttribute("data-fixed")).toBe("false");
    // El formato bloqueado se PINTA apagado, no desaparece: una tecla que no
    // hace nada y no dice por qué es peor que una que dice que no.
    const fmt = filas[2]?.querySelector(".columns-format");
    expect(fmt?.textContent).toBe("symbolic");
    expect(fmt?.getAttribute("data-locked")).toBe("true");
    // Y una columna sin formato no inventa uno.
    expect(filas[0]?.querySelector(".columns-format")).toBeNull();
  });

  it("dice que lo elegido NO se guarda", () => {
    const { screen } = montar();
    screen.paint(conColumnas());
    const nota = document.querySelector(".columns-note");
    expect(nota?.textContent).toContain("no se guarda");
    // Con `role="note"`, para quien no mira la pantalla: creerse que uno
    // acaba de configurar norte y descubrir que no es peor que no poder.
    expect(nota?.getAttribute("role")).toBe("note");
  });

  it("y el alcance va en el TÍTULO, que es lo primero que se lee", () => {
    const { screen } = montar();
    screen.paint(conColumnas());
    const caja = document.querySelector(".columns-picker");
    expect(caja?.querySelector("h1")?.textContent).toContain("sftp");
    expect(caja?.getAttribute("aria-modal")).toBe("true");
  });
});

describe("las entradas que el provider se saltó", () => {
  it("se DICEN en la cabecera, que es donde el lector puede verlas", () => {
    const { screen } = montar();
    const v = vista({ skipped_note: "se saltaron 3 entradas" });
    screen.paint(v);
    const aviso = document.querySelector(".slot-skipped");
    expect(aviso?.textContent).toBe("se saltaron 3 entradas");
    // En la CABECERA: al final de la lista no serviría de nada, porque lo
    // que falta no está y no hay ninguna fila con la que tropezarse.
    expect(aviso?.closest(".slot-title")).not.toBeNull();
    // Y anunciado, para quien no mira la pantalla.
    expect(aviso?.getAttribute("role")).toBe("status");
  });

  it("la ruta es lo que se recorta, no el aviso", () => {
    const { screen } = montar();
    screen.paint(vista({ skipped_note: "se saltaron 3 entradas" }));
    const titulo = document.querySelector(".slot-title");
    const ruta = titulo?.querySelector(".title-path");
    const aviso = titulo?.querySelector(".slot-skipped");
    // La ruta en su PROPIO nodo y el aviso como HERMANO suyo. Con la ruta
    // como texto suelto de la cabecera, una larga empujaba el aviso fuera de
    // la vista y desaparecía en silencio. jsdom no hace layout, así que lo
    // que se puede clavar aquí es la estructura que lo hace imposible; el
    // recorte de verdad se comprueba pintando.
    expect(ruta?.textContent).toContain("casa");
    expect(aviso?.parentElement).toBe(titulo);
    expect(ruta?.contains(aviso ?? null)).toBe(false);
  });

  it("y un listado completo no dice nada", () => {
    const { screen } = montar();
    screen.paint(vista({}));
    expect(document.querySelector(".slot-skipped")).toBeNull();
  });
});

describe("la insignia de un plugin en una fila", () => {
  it("va en su propio nodo, con el rol del tema y sin tocar el nombre", () => {
    const { screen } = montar();
    const v = vista({});
    const slot = v.slots[0];
    if (slot?.kind === "browser") {
      slot.rows = [
        fila(1, "limpio.rs"),
        fila(2, "cambiado.rs", { badge: "M", badge_role: "warning" }),
      ];
      slot.total_rows = 2;
    }
    screen.paint(v);
    const filas = [...document.querySelectorAll(".row")];
    expect(filas[0]?.querySelector(".cell-badge")).toBeNull();
    const marca = filas[1]?.querySelector(".cell-badge");
    expect(marca?.textContent).toBe("M");
    // El ROL, no un color que el plugin elija.
    expect(marca?.getAttribute("data-role")).toBe("warning");
    // Y en su propio nodo: unirla al nombre deja que una reordene a la otra.
    expect(filas[1]?.querySelector(".cell-name")?.textContent).toBe("cambiado.rs");
  });

  it("una insignia que se pinta distinta de lo que es lo DICE", () => {
    const { screen } = montar();
    const v = vista({});
    const slot = v.slots[0];
    if (slot?.kind === "browser") {
      slot.rows = [
        fila(1, "x.rs", { badge: "a\uFFFDb", badge_hostile: true, badge_role: "error" }),
      ];
      slot.total_rows = 1;
    }
    screen.paint(v);
    const marca = document.querySelector(".cell-badge");
    expect(marca?.getAttribute("data-hostile")).toBe("true");
    expect(marca?.querySelector(".hostile-badge")).not.toBeNull();
  });
});

describe("el scroll de la ayuda", () => {
  function conAyuda(topic: string, cursor: number): ViewSnapshot {
    const v = vista({});
    v.help = {
      title: "Copiar",
      topic_id: topic,
      sidebar: [
        { row: "topic", title: "Copiar", current: true },
        { row: "topic", title: "Mover", current: false },
      ],
      cursor,
      blocks: [{ block: "paragraph", spans: [{ span: "text", text: "hola" }] }],
      actions: [],
      action_cursor: 0,
      badge: null,
      can_back: false,
      filter: "",
      filtering: false,
      focus: "body",
    };
    return v;
  }

  it("no vuelve arriba en cada parche: leer media página y bajar el cursor", () => {
    const { screen } = montar();
    screen.paint(conAyuda("copying", 0));
    const cuerpo = () => document.querySelector(".help-body") as HTMLElement;
    // El lector baja por la página. `scrollTop` en jsdom no se limita solo,
    // que es justo lo que hace falta para comprobar que se conserva.
    cuerpo().scrollTop = 120;
    // Un parche cualquiera —mover el cursor de la lateral lo es— repinta.
    screen.paint(conAyuda("copying", 1));
    expect(cuerpo().scrollTop).toBe(120);
  });

  it("y cambiar de PÁGINA empieza arriba, que es lo que hace un lector", () => {
    const { screen } = montar();
    screen.paint(conAyuda("copying", 0));
    const cuerpo = () => document.querySelector(".help-body") as HTMLElement;
    cuerpo().scrollTop = 120;
    screen.paint(conAyuda("moving", 0));
    expect(cuerpo().scrollTop).toBe(0);
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
            {
              chord: "F5",
              label: "copiar",
              label_hostile: false,
              enabled: true,
              reason: "",
            },
            {
              chord: "F6",
              label: "mover",
              label_hostile: false,
              enabled: false,
              reason: "aquí no",
            },
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
    // El motivo va en su PROPIA celda, no pegado a la etiqueta: compuestos
    // en banda quedan en la misma corrida bidi, y una etiqueta que acabe en
    // RTL fuerte se lleva el separador al lado que no es.
    expect(filas[1]?.querySelector(".help-key-label")?.textContent).toBe("mover");
    expect(filas[1]?.querySelector(".help-key-reason")?.textContent).toBe("aquí no");
  });

  it("cada dato del host dentro de la prosa es su propia corrida bidi", () => {
    const { screen } = montar();
    screen.paint(conAyuda());
    // Un `<p>` hecho de spans es UNA corrida: sin aislar, prosa RTL de un
    // tercero puede mover de sitio la tecla que la frase dice que se pulse,
    // y ahí no hay nada que enmascarar — son letras, no controles.
    //
    // Se comprueba sobre la HOJA como texto y no con `getComputedStyle`:
    // jsdom no aplica la hoja del documento, así que el valor calculado
    // sería vacío para todo y el test pasaría sin comprobar nada. Lo que
    // hay que impedir es que la regla desaparezca, y eso sí se ve aquí.
    const css = readFileSync(resolve(process.cwd(), "src/style.css"), "utf8");
    const bloque = css
      .split("}")
      .find((b) => b.includes("unicode-bidi: isolate") && b.includes(".help-chord"));
    expect(bloque, "no hay regla de aislamiento para la ayuda").toBeDefined();
    for (const clase of [
      ".help-chord",
      ".help-cmd",
      ".help-link",
      ".help-key-chord",
      ".help-key-label",
      ".help-key-reason",
      ".help-action-chord",
      ".help-action-label",
      ".help-action-reason",
    ]) {
      expect(bloque, `${clase} sin aislar`).toContain(clase);
    }
    // Y las clases existen de verdad en lo pintado, no solo en la hoja.
    for (const sel of [".help-chord", ".help-key-chord", ".help-action-chord"]) {
      expect(document.querySelector(sel), `falta ${sel}`).not.toBeNull();
    }
  });

  it("un enlace de la prosa no es un control ni lleva la clave de destino", () => {
    const { screen } = montar();
    const v = conAyuda();
    if (v.help !== null) {
      v.help.blocks = [{ block: "paragraph", spans: [{ span: "link", text: "Marcar" }] }];
    }
    screen.paint(v);
    const enlace = document.querySelector(".help-link");
    expect(enlace?.tagName).toBe("SPAN");
    // Una marca `[[topic]]` no está en la lista de acciones, así que no hay
    // nada que activar: un `button` que no hace nada es peor que un texto.
    expect(enlace?.getAttribute("data-topic")).toBeNull();
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

describe("los ajustes", () => {
  function conAjustes(): ViewSnapshot {
    const v = vista({});
    v.settings = {
      sections: [
        {
          section: "settings",
          title: "General",
          rows: [
            {
              id: "ui.confirm-quit",
              name: "Confirmar al salir",
              desc: "Pregunta antes de cerrar norte",
              value: "siempre",
              hostile: false,
              restart_required: true,
            },
          ],
        },
        {
          section: "paths",
          title: "Dónde vive cada cosa",
          rows: [
            {
              label: "Tu configuración",
              display: "/home/oscar/.config/norte",
              hostile: false,
              missing: false,
            },
            {
              label: "Configuración del proyecto",
              display: ".norte",
              hostile: false,
              missing: true,
            },
          ],
        },
      ],
      cursor: 1,
      read_only: true,
    };
    return v;
  }

  it("es modal, avisa de que no escribe y numera solo las filas elegibles", () => {
    const { screen } = montar();
    screen.paint(conAjustes());
    const caja = document.querySelector(".settings") as HTMLElement;
    expect(caja.getAttribute("aria-modal")).toBe("true");
    // El aviso es una NOTA, no un botón apagado: apagar un control invita a
    // probarlo, y esta ventana todavía no escribe ajustes.
    expect(caja.querySelector(".settings-note")?.getAttribute("role")).toBe("note");
    // Dos cabeceras, tres filas: el cursor cuenta filas, no cabeceras.
    expect(caja.querySelectorAll(".settings-group")).toHaveLength(2);
    const filas = [...caja.querySelectorAll(".settings-row")];
    expect(filas).toHaveLength(3);
    expect(filas.map((f) => f.id)).toEqual([
      "settings-row-0",
      "settings-row-1",
      "settings-row-2",
    ]);
    const lista = caja.querySelector(".settings-rows") as HTMLElement;
    expect(lista.getAttribute("aria-activedescendant")).toBe("settings-row-1");
  });

  it("una ubicación que falta lo dice, y una hostil se marca", () => {
    const { screen } = montar();
    const v = conAjustes();
    if (v.settings !== null) {
      const sec = v.settings.sections[1];
      if (sec?.section === "paths") {
        sec.rows[0] = {
          label: "Tu configuración",
          display: "/home/oscar/conf�gif",
          hostile: true,
          missing: false,
        };
      }
    }
    screen.paint(v);
    const filas = [...document.querySelectorAll(".settings-row")];
    expect(filas[1]?.querySelector(".settings-value")?.getAttribute("data-hostile")).toBe(
      "true",
    );
    expect(filas[2]?.querySelector(".settings-missing")).not.toBeNull();
    // La que está no se marca como que falta.
    expect(filas[1]?.querySelector(".settings-missing")).toBeNull();
  });

  it("si toda la sección pide reiniciar, se dice una vez y no cinco", () => {
    const { screen } = montar();
    const v = conAjustes();
    if (v.settings !== null) {
      const sec = v.settings.sections[0];
      if (sec?.section === "settings") {
        sec.rows.push({
          id: "ui.theme",
          name: "Tema",
          desc: "El tema",
          value: "tokyonight",
          hostile: false,
          restart_required: true,
        });
      }
    }
    screen.paint(v);
    const cabecera = document.querySelector(".settings-group");
    expect(cabecera?.textContent).toContain(
      catalogoReal()["settings-restart-badge"] ?? "",
    );
    // Y ninguna fila la repite.
    expect(document.querySelectorAll(".settings-row .settings-badge")).toHaveLength(0);
  });

  it("si solo algunas lo piden, la insignia va en la fila", () => {
    const { screen } = montar();
    const v = conAjustes();
    if (v.settings !== null) {
      const sec = v.settings.sections[0];
      if (sec?.section === "settings") {
        sec.rows.push({
          id: "ui.theme",
          name: "Tema",
          desc: "El tema",
          value: "tokyonight",
          hostile: false,
          restart_required: false,
        });
      }
    }
    screen.paint(v);
    expect(document.querySelectorAll(".settings-row .settings-badge")).toHaveLength(1);
    expect(document.querySelector(".settings-group")?.textContent).not.toContain(
      catalogoReal()["settings-restart-badge"] ?? "",
    );
  });

  it("un click pide ESA fila, contando por encima de las cabeceras", () => {
    const { screen, enviadas } = montar();
    screen.paint(conAjustes());
    const filas = [...document.querySelectorAll(".settings-row")];
    (filas[2] as HTMLElement).click();
    expect(enviadas).toEqual([{ action: "settings_select_row", row: 2 }]);
  });

  it("cerrados, no tapan nada", () => {
    const { screen } = montar();
    screen.paint(vista({}));
    expect(document.querySelector(".settings")).toBeNull();
  });
});

describe("el gestor de extensiones", () => {
  function conExtensiones(): ViewSnapshot {
    const v = vista({});
    v.extensions = {
      rows: [
        {
          id: "acme.ftp",
          name: "FTP de ACME",
          publisher: "ACME",
          version: "1.2.0",
          category: "provider",
          description: "Sirve ficheros por FTP",
          approved: true,
          enabled: true,
          has_help: true,
          commands: 2,
          columns: 0,
          capabilities: ["net", "fs-read"],
        },
        {
          id: "org.norte.demo",
          name: "Demo",
          publisher: "",
          version: "0.1.0",
          category: "previewer",
          description: "",
          approved: false,
          enabled: false,
          has_help: false,
          commands: 1,
          columns: 1,
          capabilities: ["fs-read"],
        },
      ],
      cursor: 0,
      detail: null,
      loading: false,
      errors: [],
    };
    return v;
  }

  it("enseña el estado como DOS hechos y las capabilities en la fila", () => {
    const { screen } = montar();
    screen.paint(conExtensiones());
    const filas = [...document.querySelectorAll(".extensions-row")];
    expect(filas).toHaveLength(2);
    const uno = filas[0]?.querySelector(".extensions-state");
    expect(uno?.getAttribute("data-approved")).toBe("true");
    expect(uno?.getAttribute("data-enabled")).toBe("true");
    const dos = filas[1]?.querySelector(".extensions-state");
    expect(dos?.getAttribute("data-approved")).toBe("false");
    // Las capabilities NO están escondidas tras un gesto: son la decisión.
    expect(filas[0]?.querySelectorAll(".extensions-cap")).toHaveLength(2);
  });

  it("no hay ni un control para aprobar o encender", () => {
    const { screen } = montar();
    screen.paint(conExtensiones());
    const caja = document.querySelector(".extensions") as HTMLElement;
    expect(caja.querySelectorAll("button")).toHaveLength(0);
    expect(caja.querySelectorAll("input")).toHaveLength(0);
  });

  it("«cargando» no se pinta igual que «ninguna»", () => {
    const { screen } = montar();
    const v = conExtensiones();
    if (v.extensions !== null) {
      v.extensions.loading = true;
      v.extensions.rows = [];
    }
    screen.paint(v);
    expect(document.querySelector(".extensions-note")?.getAttribute("role")).toBe(
      "status",
    );
    expect(document.querySelector(".extensions-note")?.textContent).toBe(
      catalogoReal()["ext-loading"] ?? "",
    );

    const vacio = conExtensiones();
    if (vacio.extensions !== null) {
      vacio.extensions.loading = false;
      vacio.extensions.rows = [];
    }
    screen.paint(vacio);
    expect(document.querySelector(".extensions-note")?.textContent).toBe(
      catalogoReal()["ext-empty"] ?? "",
    );
  });

  it("la ficha marca el valor que ya no es el del esquema", () => {
    const { screen } = montar();
    const v = conExtensiones();
    if (v.extensions !== null) {
      v.extensions.detail = {
        id: "acme.ftp",
        config: [
          {
            key: "timeout",
            kind: "int",
            value: "30",
            default: "10",
            description: "Segundos",
            domain: "entre 1 y 300",
            hostile: false,
            editable: true,
          },
          {
            key: "passive",
            kind: "bool",
            value: "true",
            default: "true",
            description: "",
            domain: "",
            hostile: false,
            editable: true,
          },
          {
            key: "mode",
            kind: "enum",
            value: "fast\uFFFD",
            default: "safe",
            description: "",
            domain: "safe · fast\uFFFD",
            hostile: true,
            editable: true,
          },
        ],
        commands: [],
        cursor: 0,
        editing: null,
        editing_hostile: false,
      };
    }
    screen.paint(v);
    const filas = [...document.querySelectorAll(".extensions-config tbody tr")];
    expect(filas).toHaveLength(3);
    expect(filas[0]?.getAttribute("data-changed")).toBe("true");
    expect(filas[1]?.getAttribute("data-changed")).toBe("false");
    // El valor que el PLUGIN escribe y se pinta distinto de lo que es lleva
    // su insignia, igual que un nombre de fichero.
    const valor = filas[2]?.querySelector(".extensions-key-value");
    expect(valor?.getAttribute("data-hostile")).toBe("true");
    expect(valor?.querySelector(".hostile-badge")).not.toBeNull();
    expect(filas[0]?.querySelector(".hostile-badge")).toBeNull();
    // Y el tipo y su dominio van en nodos SEPARADOS: unirlos en uno solo
    // deja que un valor de `enum` con letras RTL reordene el par entero.
    expect(filas[2]?.querySelector(".extensions-key-domain")?.textContent).toBe(
      "safe · fast\uFFFD",
    );
    expect(filas[0]?.querySelector(".extensions-key-kind")?.textContent).toContain(
      "entre 1 y 300",
    );
    // La ficha dice DE QUIÉN es: con la lista desplazada, la fila elegida
    // puede no estar a la vista.
    expect(document.querySelector(".extensions-detail-of")?.textContent).toBe(
      "FTP de ACME",
    );
  });

  it("un directorio que no cargó se dice, y uno hostil se marca", () => {
    const { screen } = montar();
    const v = conExtensiones();
    if (v.extensions !== null) {
      v.extensions.errors = [
        {
          dir: "/plugins/ro�to",
          hostile: true,
          reason: "el manifiesto no parsea",
          reason_hostile: false,
        },
        // El MOTIVO cita el manifiesto del plugin, así que tiene su propia
        // marca: una sola para las dos cadenas deja al lector sin saber cuál
        // de ellas está alterada.
        {
          dir: "/plugins/otro",
          hostile: false,
          reason: "clave desconocida: mo�do",
          reason_hostile: true,
        },
      ];
    }
    screen.paint(v);
    const err = document.querySelector(".extensions-error-dir");
    expect(err?.getAttribute("data-hostile")).toBe("true");
    expect(document.querySelector(".extensions-error-reason")?.textContent).toBe(
      "el manifiesto no parsea",
    );
    const motivos = [...document.querySelectorAll(".extensions-error-reason")];
    expect(motivos[0]?.getAttribute("data-hostile")).toBe("false");
    expect(motivos[1]?.getAttribute("data-hostile")).toBe("true");
    expect(motivos[1]?.querySelector(".hostile-badge")).not.toBeNull();
  });

  it("un click elige ESA extensión", () => {
    const { screen, enviadas } = montar();
    screen.paint(conExtensiones());
    const filas = [...document.querySelectorAll(".extensions-row")];
    (filas[1] as HTMLElement).click();
    expect(enviadas).toEqual([{ action: "extension_select_row", row: 1 }]);
  });

  it("cerrado, no tapa nada", () => {
    const { screen } = montar();
    screen.paint(vista({}));
    expect(document.querySelector(".extensions")).toBeNull();
  });
});

describe("el tema y el selector", () => {
  it("cada rol se ve, no solo se lee su hex", () => {
    const { screen } = montar();
    const v = vista({});
    v.theme = {
      name: "retro",
      roles: [
        { role: "selection-bg", color: "#2d4f8a" },
        { role: "error-fg", color: "#f7768e" },
      ],
      unsupported_effects: [
        { key: "crt", hostile: false },
        { key: "scanlines", hostile: false },
      ],
    };
    screen.paint(v);
    const filas = [...document.querySelectorAll(".theme-role")];
    expect(filas).toHaveLength(2);
    const muestra = filas[0]?.querySelector(".theme-swatch") as HTMLElement;
    // La muestra ES el dato: un `#2d4f8a` no dice nada hasta que se ve.
    expect(muestra.style.backgroundColor).not.toBe("");
    expect(filas[0]?.querySelector(".theme-role-hex")?.textContent).toBe("#2d4f8a");
    // Y los efectos que esta ventana no pinta se NOMBRAN.
    const aviso = document.querySelector(".theme-effects");
    expect(aviso?.getAttribute("role")).toBe("note");
    expect(aviso?.textContent).toContain("crt");
    expect(aviso?.textContent).toContain("scanlines");
  });

  it("un tema sin efectos no pinta el aviso", () => {
    const { screen } = montar();
    const v = vista({});
    v.theme = {
      name: "default",
      roles: [{ role: "fg", color: "#d4d8de" }],
      unsupported_effects: [],
    };
    screen.paint(v);
    expect(document.querySelector(".theme-effects")).toBeNull();
  });

  it("el selector marca un montaje hostil y dice por qué está vacío", () => {
    const { screen, enviadas } = montar();
    const v = vista({});
    v.picker = {
      title: "Volúmenes",
      rows: [
        { label: "⟨file⟩/", hostile: false, detail: "ext4 · 12 GiB libres de 100 GiB" },
        { label: "⟨file⟩/mnt/ro�to", hostile: true, detail: "ntfs · solo lectura" },
      ],
      cursor: 1,
      empty: "",
      generation: 1,
    };
    screen.paint(v);
    const filas = [...document.querySelectorAll(".picker-row")];
    expect(filas).toHaveLength(2);
    expect(filas[1]?.querySelector(".picker-label")?.getAttribute("data-hostile")).toBe(
      "true",
    );
    const lista = document.querySelector(".picker-rows") as HTMLElement;
    expect(lista.getAttribute("aria-activedescendant")).toBe("picker-row-1");
    (filas[0] as HTMLElement).click();
    expect(enviadas).toEqual([{ action: "picker_select_row", row: 0, generation: 1 }]);

    const vacio = vista({});
    vacio.picker = {
      title: "Volúmenes",
      rows: [],
      cursor: null,
      generation: 1,
      empty: "preguntando al host…",
    };
    screen.paint(vacio);
    const nota = document.querySelector(".picker-empty");
    expect(nota?.getAttribute("role")).toBe("status");
    expect(nota?.textContent).toBe("preguntando al host…");
  });

  it("cerrados, no tapan nada", () => {
    const { screen } = montar();
    screen.paint(vista({}));
    expect(document.querySelector(".theme")).toBeNull();
    expect(document.querySelector(".picker")).toBeNull();
  });
});

describe("los huecos que no son listados", () => {
  it("la hoja de atributos pinta etiqueta y valor, y marca un nombre hostil", () => {
    const { screen } = montar();
    const v = vista({});
    v.slots = [
      ...v.slots,
      {
        kind: "metadata",
        slot_id: 7,
        fields: [
          { label: "Nombre", value: "caf�.txt", hostile: true },
          { label: "Tamaño", value: "1,2 KiB (1258)", hostile: false },
        ],
        note: "",
      },
    ];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 0, y: 0, width: 30, height: 10, role: null, focus_index: 2 },
    ];
    screen.paint(v);
    const campos = [...document.querySelectorAll(".metadata-fields dt")];
    expect(campos.map((d) => d.textContent)).toEqual(["Nombre", "Tamaño"]);
    const valores = [...document.querySelectorAll(".metadata-fields dd")];
    expect(valores[0]?.getAttribute("data-hostile")).toBe("true");
    expect(valores[1]?.getAttribute("data-hostile")).toBe("false");
  });

  it("la hoja sin nada bajo el cursor lo DICE", () => {
    const { screen } = montar();
    const v = vista({});
    v.slots = [
      ...v.slots,
      { kind: "metadata", slot_id: 7, fields: [], note: "nada bajo el cursor" },
    ];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 0, y: 0, width: 30, height: 10, role: null, focus_index: 2 },
    ];
    screen.paint(v);
    expect(document.querySelector(".metadata .slot-note")?.textContent).toBe(
      "nada bajo el cursor",
    );
    expect(document.querySelector(".metadata-fields")).toBeNull();
  });

  it("el panel de procesos marca su cursor sobre las MISMAS tareas", () => {
    const { screen } = montar();
    const v = vista({});
    v.tasks = [
      {
        task_id: 1,
        kind: "copy",
        state: "running",
        percent: 40,
        detail: "a.txt",
        detail_hostile: false,
        foreign: false,
      },
      {
        task_id: 2,
        kind: "delete",
        state: "running",
        percent: 10,
        detail: "b.txt",
        detail_hostile: false,
        foreign: false,
      },
    ];
    v.slots = [...v.slots, { kind: "processes", slot_id: 7, cursor: 1 }];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 0, y: 0, width: 40, height: 10, role: null, focus_index: 2 },
    ];
    screen.paint(v);
    const filas = [...document.querySelectorAll(".processes-row")];
    expect(filas).toHaveLength(2);
    expect(filas[1]?.getAttribute("aria-selected")).toBe("true");
    const lista = document.querySelector(".processes-rows") as HTMLElement;
    expect(lista.getAttribute("aria-activedescendant")).toBe("process-row-1");
  });

  it("sin tareas, el panel lo dice en vez de quedarse en blanco", () => {
    const { screen } = montar();
    const v = vista({});
    v.slots = [...v.slots, { kind: "processes", slot_id: 7, cursor: null }];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 0, y: 0, width: 40, height: 10, role: null, focus_index: 2 },
    ];
    screen.paint(v);
    expect(document.querySelector(".processes .slot-note")?.textContent).toBe(
      catalogoReal()["processes-empty"] ?? "",
    );
  });
});

describe("la barra lateral de sitios", () => {
  function conSitios(cursor: number): ViewSnapshot {
    const v = vista({});
    v.slots = [
      ...v.slots,
      {
        kind: "places",
        slot_id: 7,
        rows: [
          { row: "header", label: "Unidades", folded: false },
          {
            row: "drive",
            label: "raíz",
            hostile: false,
            detail: "12 GiB libres de 100 GiB",
          },
          { row: "header", label: "Favoritos", folded: true },
          {
            row: "favorite",
            name: "casa",
            target: "⟨file⟩/home",
            hostile: false,
            broken: "",
          },
          {
            row: "favorite",
            name: "roto",
            target: "",
            hostile: false,
            broken: "la ruta no vale",
          },
        ],
        cursor,
        generation: 3,
      },
    ];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 0, y: 0, width: 20, height: 20, role: null, focus_index: 2 },
    ];
    return v;
  }

  it("una cabecera dice si está plegada, y un favorito roto dice por qué", () => {
    const { screen } = montar();
    screen.paint(conSitios(1));
    const filas = [...document.querySelectorAll(".places-row")];
    expect(filas).toHaveLength(5);
    expect(filas[0]?.getAttribute("aria-expanded")).toBe("true");
    expect(filas[2]?.getAttribute("aria-expanded")).toBe("false");
    // El roto se VE, y se ve que está roto.
    expect(filas[4]?.querySelector(".places-broken")?.textContent).toBe(
      "la ruta no vale",
    );
    expect(filas[3]?.querySelector(".places-broken")).toBeNull();
    const lista = document.querySelector(".places-rows") as HTMLElement;
    expect(lista.getAttribute("aria-activedescendant")).toBe("place-row-1");
  });

  it("un click ELIGE y ACTIVA: una barra lateral existe para ir a sitios", () => {
    const { screen, enviadas } = montar();
    screen.paint(conSitios(0));
    const filas = [...document.querySelectorAll(".places-row")];
    (filas[1] as HTMLElement).click();
    expect(enviadas).toEqual([{ action: "place_activate_row", row: 1, generation: 3 }]);
    // También sobre una cabecera: ahí activar es PLEGAR, y lo decide el host.
    (filas[2] as HTMLElement).click();
    expect(enviadas).toHaveLength(2);
  });
});

describe("el selector de disposiciones", () => {
  function conDisposiciones(cursor: number, problem = ""): ViewSnapshot {
    const v = vista({});
    v.layouts = {
      problem_hostile: false,
      title: "Disposiciones",
      rows: [
        {
          name: "orthodox",
          hostile: false,
          factory: true,
          shares_keymap_name: true,
          broken: false,
        },
        {
          name: "mia",
          hostile: false,
          factory: false,
          shares_keymap_name: false,
          broken: true,
        },
      ],
      cursor,
      preview: problem === "" ? ["··········", "·bbbbbbbb·"] : [],
      problem,
    };
    return v;
  }

  it("avisa del nombre compartido y marca la que no parsea", () => {
    const { screen } = montar();
    screen.paint(conDisposiciones(0));
    const filas = [...document.querySelectorAll(".layouts-row")];
    expect(filas).toHaveLength(2);
    // El aviso no es adorno: elegirla no cambia ninguna tecla.
    expect(filas[0]?.querySelector(".layouts-warn")).not.toBeNull();
    expect(filas[0]?.querySelector(".layouts-tag")?.textContent).toBe(
      catalogoReal()["layout-picker-factory"] ?? "",
    );
    expect(filas[1]?.getAttribute("data-broken")).toBe("true");
    expect(filas[1]?.querySelector(".layouts-tag")).toBeNull();
  });

  it("la miniatura llega hecha y se pone tal cual", () => {
    const { screen } = montar();
    screen.paint(conDisposiciones(0));
    const vista_previa = document.querySelector(".layouts-preview");
    expect(vista_previa?.tagName).toBe("PRE");
    expect(vista_previa?.textContent).toBe("··········\n·bbbbbbbb·");
    // Es decorativa: lo que dice ya está en el nombre de la fila.
    expect(vista_previa?.getAttribute("aria-hidden")).toBe("true");
  });

  it("una que no parsea enseña su motivo en vez de una miniatura", () => {
    const { screen } = montar();
    screen.paint(conDisposiciones(1, "no parsea: falta `kind`"));
    expect(document.querySelector(".layouts-preview")).toBeNull();
    expect(document.querySelector(".layouts-problem")?.textContent).toBe(
      "no parsea: falta `kind`",
    );
  });

  it("un click elige ESA disposición", () => {
    const { screen, enviadas } = montar();
    screen.paint(conDisposiciones(0));
    const filas = [...document.querySelectorAll(".layouts-row")];
    (filas[1] as HTMLElement).click();
    expect(enviadas).toEqual([{ action: "layout_activate_row", row: 1 }]);
  });
});

describe("la búsqueda", () => {
  function conBusqueda(running: boolean): ViewSnapshot {
    const v = vista({});
    v.search = {
      semantic: false,
      query: "*.rs",
      root: "⟨file⟩/home/oscar/work",
      root_hostile: false,
      rows: [
        {
          name: "main.rs",
          hostile: false,
          parent: "⟨file⟩/home/oscar/work/src",
          parent_hostile: false,
          is_dir: false,
          score: null,
        },
        {
          name: "caf�.rs",
          hostile: true,
          parent: "⟨file⟩/home/oscar/work",
          parent_hostile: false,
          is_dir: false,
          score: null,
        },
      ],
      cursor: 0,
      status: running ? "búsqueda: 2 hallazgos (buscando…)" : "búsqueda: 2 hallazgos",
      running,
    };
    return v;
  }

  it("dice en qué estado está, y lo anuncia sin robar el foco", () => {
    const { screen } = montar();
    screen.paint(conBusqueda(true));
    const estado = document.querySelector(".search-status") as HTMLElement;
    expect(estado.getAttribute("role")).toBe("status");
    expect(estado.getAttribute("aria-live")).toBe("polite");
    expect(estado.getAttribute("data-running")).toBe("true");
    expect(estado.textContent).toContain("buscando");

    screen.paint(conBusqueda(false));
    expect(document.querySelector(".search-status")?.getAttribute("data-running")).toBe(
      "false",
    );
  });

  it("cada fila dice el nombre y DÓNDE está, y marca lo hostil", () => {
    const { screen } = montar();
    screen.paint(conBusqueda(true));
    const filas = [...document.querySelectorAll(".search-row")];
    expect(filas).toHaveLength(2);
    expect(filas[0]?.querySelector(".search-name")?.textContent).toBe("main.rs");
    expect(filas[0]?.querySelector(".search-parent")?.textContent).toContain("src");
    expect(filas[1]?.querySelector(".search-name")?.getAttribute("data-hostile")).toBe(
      "true",
    );
  });

  it("un click va a ESE resultado, mandando un índice y no una ruta", () => {
    const { screen, enviadas } = montar();
    screen.paint(conBusqueda(false));
    const filas = [...document.querySelectorAll(".search-row")];
    (filas[1] as HTMLElement).click();
    expect(enviadas).toEqual([{ action: "search_activate_row", row: 1 }]);
  });

  it("cerrada, no tapa nada", () => {
    const { screen } = montar();
    screen.paint(vista({}));
    expect(document.querySelector(".search")).toBeNull();
  });
});

describe("un diálogo que pregunta por una operación", () => {
  it("pinta el destino FUERA del cuerpo, marca lo alterado y dice si recorta", () => {
    const { screen } = montar();
    const v = vista({});
    v.dialogs = [
      {
        id: 4,
        title_key: "modal-copy-title",
        subject: null,
        asker: null,
        deadline: null,
        // Un directorio que se llama `a → mem_b.txt`: la flecha es legítima,
        // no se enmascara y no se marca. Con el destino como primera línea
        // del cuerpo, la línea se leería como dos rutas.
        destination: { text: "⟨mem⟩/casa/a → mem_b.txt", hostile: false },
        body: [
          { text: "⟨mem⟩/casa/notas.txt", hostile: false },
          { text: "⟨mem⟩/casa/caf�.txt", hostile: true },
        ],
        overflow_note: "… se enseñan 2 de 240",
        choices: [
          { id: "confirm", label_key: "dialog-confirm", destructive: false },
          { id: "cancel", label_key: "dialog-cancel", destructive: false },
        ],
        input: null,
        input_hostile: false,
      },
    ];
    screen.paint(v);
    const dialog = document.querySelector('[role="dialog"]') as HTMLElement;

    const dest = dialog.querySelector(".dialog-destination") as HTMLElement;
    expect(dest).not.toBeNull();
    expect(dest.textContent).toContain("a → mem_b.txt");
    // Y no es una línea del cuerpo: el cuerpo es una lista NUMERADA aparte,
    // y el destino no está en ella.
    const cuerpo = [...dialog.querySelectorAll("ol.dialog-body li")];
    expect(cuerpo).toHaveLength(2);
    expect(cuerpo.map((p) => p.textContent ?? "").join(" ")).not.toContain("→");

    // La línea alterada lo dice, y la fiel no.
    expect((cuerpo[0] as HTMLElement | undefined)?.dataset["hostile"]).toBe("false");
    expect((cuerpo[1] as HTMLElement | undefined)?.dataset["hostile"]).toBe("true");
    expect(cuerpo[1]?.textContent ?? "").toContain("nombre alterado");

    // Y el recorte se pinta como aviso.
    const nota = dialog.querySelector(".dialog-overflow") as HTMLElement;
    expect(nota).not.toBeNull();
    expect(nota.getAttribute("role")).toBe("alert");
    expect(nota.textContent).toContain("240");
  });
});

describe("el tablero de tareas", () => {
  it("dice cuándo el fichero en curso se pinta distinto de lo que es", () => {
    const { screen, root } = montar();
    const v = vista({});
    v.layout.placements.push({
      slot_id: 9,
      x: 0,
      y: 30,
      width: 60,
      height: 4,
      role: null,
      focus_index: 2,
    });
    v.slots.push({ kind: "tasks", slot_id: 9 } as never);
    v.tasks = [
      {
        task_id: 100,
        kind: "copy",
        state: "running",
        percent: 40,
        detail: "⟨mem⟩/casa/caf�.txt",
        detail_hostile: true,
        foreign: false,
      },
    ];
    screen.paint(v);
    const task = root.querySelector(".task") as HTMLElement;
    expect(task).not.toBeNull();
    expect(task.textContent ?? "").toContain("nombre alterado");
  });
});

describe("la revisión de un plan de renombrado", () => {
  it("separa los dos nombres de una pareja SIN un glifo que un nombre pueda tener", () => {
    const { screen } = montar();
    const v = vista({});
    v.ai_rename = {
      dir: { text: "⟨mem⟩/casa/series", hostile: false },
      pairs: [
        // Un nombre de ORIGEN con una flecha dentro: en una sola línea
        // separada por `→`, la fila se leería como otra pareja.
        {
          from: { text: "cap 2 → final.mkv", hostile: false },
          to: { text: "ep0�2.mkv", hostile: true },
        },
      ],
      first_visible: 0,
      total: 7,
      status: "lote: aplicable",
      detail: [{ text: "✗ 3. ya existe: ep03.mkv", hostile: false }],
      more_note: "… 1/7 (desplazar: ↓/↑)",
      hidden_hostile: true,
      confirmable: true,
      real_steps_note: "se renombrarán 6 de verdad",
      seen_all: true,
    };
    screen.paint(v);
    const caja = document.querySelector(".ai-rename") as HTMLElement;
    expect(caja).not.toBeNull();
    expect(caja.getAttribute("aria-modal")).toBe("true");

    const from = caja.querySelector(".ai-rename-from") as HTMLElement;
    const to = caja.querySelector(".ai-rename-to") as HTMLElement;
    expect(from.textContent).toBe("cap 2 → final.mkv");
    expect(to.textContent).toContain("ep0�2.mkv");
    // El separador NO está en el texto: lo pinta el CSS, que un nombre no
    // puede escribir.
    expect(to.textContent?.startsWith("→")).toBe(false);
    expect(from.contains(to)).toBe(false);

    expect(to.dataset["hostile"]).toBe("true");
    expect(to.textContent).toContain("nombre alterado");
    expect(from.dataset["hostile"]).toBe("false");

    const estado = caja.querySelector(".ai-rename-status") as HTMLElement;
    expect(estado.dataset["confirmable"]).toBe("true");
    expect(caja.querySelector(".ai-rename-more")?.textContent).toContain("7");
  });

  it("un plan que el core no acepta lo dice en su estado", () => {
    const { screen } = montar();
    const v = vista({});
    v.ai_rename = {
      dir: { text: "⟨mem⟩/casa", hostile: false },
      pairs: [],
      first_visible: 0,
      total: 0,
      status: "lote: NO aplicable",
      detail: [],
      more_note: "",
      hidden_hostile: false,
      confirmable: false,
      real_steps_note: "lote: NO aplicable",
      seen_all: true,
    };
    screen.paint(v);
    const estado = document.querySelector(".ai-rename-status") as HTMLElement;
    expect(estado.dataset["confirmable"]).toBe("false");
  });

  it("sin plan no queda nada pintado", () => {
    const { screen } = montar();
    const v = vista({});
    v.ai_rename = {
      dir: { text: "⟨mem⟩/casa", hostile: false },
      pairs: [],
      first_visible: 0,
      total: 0,
      status: "…",
      detail: [],
      more_note: "",
      hidden_hostile: false,
      confirmable: false,
      real_steps_note: "lote: NO aplicable",
      seen_all: true,
    };
    screen.paint(v);
    expect(document.querySelector(".ai-rename")).not.toBeNull();
    v.ai_rename = null;
    screen.paint(v);
    expect(document.querySelector(".ai-rename")).toBeNull();
  });
});

describe("la búsqueda por significado", () => {
  it("se titula distinto, dice su alcance y pinta el parecido", () => {
    const { screen } = montar();
    const v = vista({});
    v.search = {
      semantic: true,
      query: "facturas del año pasado",
      root: "",
      root_hostile: false,
      rows: [
        {
          name: "a.md",
          hostile: false,
          parent: "⟨mem⟩/casa/docs",
          parent_hostile: false,
          is_dir: false,
          score: 0.9123,
        },
      ],
      cursor: 0,
      status: "1 resultado",
      running: false,
    };
    screen.paint(v);

    const caja = document.querySelector(".search") as HTMLElement;
    expect(caja.querySelector("h1")?.textContent ?? "").toContain(
      "facturas del año pasado",
    );
    // El parecido se ve, con dos decimales: sin él, el orden parece
    // arbitrario.
    expect(caja.querySelector(".search-score")?.textContent).toBe("0.91");
  });
});
