// El pintado: virtualización, estados del listado, accesibilidad y gestos.
//
// Todo con un bridge FALSO. No hace falta ni ventana ni WebKitGTK para
// comprobar lo que este renderer promete.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { beforeEach, describe, expect, it, vi } from "vitest";

import { Screen } from "../src/render";
import { OVERSCAN } from "../src/render/dom";
import { catalogoReal } from "./fixtures";
import { BRIDGE_VERSION } from "../src/types";
import type {
  BrowserSlotView,
  HostCatalog,
  LogSlotView,
  PanelSlotView,
  RowView,
  UiAction,
  ViewSnapshot,
} from "../src/types";

const CELL_H = 20;

function catalogo(): HostCatalog {
  return {
    // De la constante, NUNCA un literal: éste decía 5 durante tres bumps
    // sin que nadie lo notara, que es la misma clase de rancio contra la
    // que existe el resto de este fichero (#259).
    bridge_version: BRIDGE_VERSION,
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
    icon: "",
    icon_hostile: false,
    name_color: "",
    name_bold: false,
    name_dim: false,
    name_italic: false,
    name_underline: false,
    ...extra,
  };
}

function vista(browser: Partial<BrowserSlotView>): ViewSnapshot {
  return {
    connection: { state: "connected" },
    layout: {
      tabs: [],
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
        icon_column: false,
        cursor: 0,
        marks: 0,
        skipped_note: "",
        hidden_note: "",
        columns: [
          {
            id: "name",
            label: "Nombre",
            sort: "asc",
            sortable: true,
            width: null,
            align: "left",
          },
          {
            id: "size",
            label: "Tamaño",
            sort: null,
            sortable: true,
            width: 9,
            align: "right",
          },
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
    menu: { bar: true, titles: ["Archivo", "Paneles"], open: null, items: [], cursor: 0 },
    panel_bar: {
      bar: true,
      buttons: [
        {
          kind: "places",
          label: "Sitios",
          letter: "S",
          chord: "alt+p",
          state: "open",
          attention: false,
        },
        {
          kind: "log",
          label: "Registro",
          letter: "R",
          chord: "—",
          state: "closed",
          attention: true,
        },
      ],
    },
    profiles: null,
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
    agents: null,
    plugin_output: null,
    program_output: null,
    viewer: null,
    ai_rename: null,
    organize: null,
    locale: "es",
  };
}

describe("la pantalla de arranque", () => {
  /** Una pantalla con una sección de una fila numerada. */
  function conSplash(closeAfterMs: number | null): ViewSnapshot {
    const v = vista({});
    v.splash = {
      art: ["   ·   "],
      version: "0.1.0",
      revision: "abcdef1",
      daemon: "hablando con el core embebido",
      hint: "una tecla la quita; 1-9 abre",
      sections: [
        {
          title: "A dónde sueles ir",
          rows: [{ number: 1, label: "casa", detail: "12" }],
        },
      ],
      close_after_ms: closeAfterMs,
    };
    return v;
  }

  it("pinta el arte, las secciones y sus filas numeradas", () => {
    const { screen } = montar();
    screen.paint(conSplash(null));
    const caja = document.querySelector(".splash");
    expect(caja).not.toBeNull();
    // El arte NO se lee en voz alta: una brújula de barras y guiones se
    // deletrea como ruido.
    expect(document.querySelector(".splash-art")?.getAttribute("aria-hidden")).toBe(
      "true",
    );
    expect(document.querySelector(".splash-section")?.textContent).toBe(
      "A dónde sueles ir",
    );
    expect(document.querySelector(".splash-number")?.textContent).toBe("1");
    expect(document.querySelector(".splash-label")?.textContent).toBe("casa");
  });

  // La PORTADA: `brief` viene sin secciones, y entonces la pantalla de
  // arranque deja de ser una caja centrada para ocupar el hueco entero. La
  // señal es la misma que usa el terminal —no hay secciones—, así que las dos
  // superficies deciden igual sin que el modo tenga que viajar por el puente.
  it("sin secciones se pinta como portada", () => {
    const { screen } = montar();
    const v = conSplash(null);
    if (v.splash) {
      v.splash.sections = [];
    }
    screen.paint(v);
    const caja = document.querySelector(".splash") as HTMLElement;
    expect(caja.dataset["cover"]).toBe("true");
  });

  // Y con lista sigue siendo una caja: las filas numeradas se leen y se
  // pulsan, y sueltas sobre el fondo pierden el marco que las delimita.
  it("con secciones sigue siendo una caja", () => {
    const { screen } = montar();
    screen.paint(conSplash(null));
    const caja = document.querySelector(".splash") as HTMLElement;
    expect(caja.dataset["cover"]).toBeUndefined();
  });

  it("un clic en cualquier sitio la quita", () => {
    const { screen, enviadas } = montar();
    screen.paint(conSplash(null));
    const raiz =
      document.getElementById("splash") ??
      document.querySelector(".splash")?.parentElement;
    (raiz as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(enviadas.at(-1)).toEqual({ action: "splash_close" });
  });

  it("un clic en una fila numerada la abre, y no cuenta además como cierre", () => {
    const { screen, enviadas } = montar();
    screen.paint(conSplash(null));
    const fila = document.querySelector(".splash-row") as HTMLElement;
    fila.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(enviadas.at(-1)).toEqual({ action: "splash_activate_row", number: 1 });
    // Una sola acción: el clic de la fila no burbujea hasta el velo, o el
    // host recibiría «ábrela» y «quítala» y la navegación se perdería.
    expect(enviadas).toHaveLength(1);
  });

  it("el plazo del modo breve la quita sola, sin que nadie toque nada", () => {
    vi.useFakeTimers();
    try {
      const { screen, enviadas } = montar();
      screen.paint(conSplash(1200));
      expect(enviadas).toHaveLength(0);
      vi.advanceTimersByTime(1199);
      expect(enviadas).toHaveLength(0);
      vi.advanceTimersByTime(1);
      expect(enviadas.at(-1)).toEqual({ action: "splash_close" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("el plazo NO se rearma en cada repintado", () => {
    vi.useFakeTimers();
    try {
      const { screen, enviadas } = montar();
      const v = conSplash(1200);
      screen.paint(v);
      vi.advanceTimersByTime(600);
      // Otro parche cualquiera: el host manda la vista ENTERA cada vez, y
      // durante el arranque no paran de llegar. Si cada pintada rearmara el
      // plazo, «1,2 segundos» sería «1,2 segundos tras el último parche», y
      // con una tarea en marcha la pantalla no se iría nunca.
      screen.paint(JSON.parse(JSON.stringify(v)) as ViewSnapshot);
      vi.advanceTimersByTime(600);
      expect(enviadas.at(-1)).toEqual({ action: "splash_close" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("al quitarse, el plazo pendiente se desarma", () => {
    vi.useFakeTimers();
    try {
      const { screen, enviadas } = montar();
      screen.paint(conSplash(1200));
      // El host ya la quitó (una tecla): el temporizador que quedaba vivo
      // mandaría un cierre de una pantalla que ya no está.
      screen.paint(vista({}));
      vi.advanceTimersByTime(5000);
      expect(enviadas).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("el panel sin el teclado se atenúa", () => {
  // El atenuado es CSS y cuelga de `.scroller`: solo un listado conserva esa
  // clase, y los paneles laterales la sustituyen por la suya. Por rol no se
  // pueden distinguir —un listado sin destino que marcar se queda sin rol,
  // igual que un lateral—, así que este invariante es lo único que impide
  // que el registro o el propio panel de procesos se pinten apagados
  // mientras tienen el teclado.
  it("un panel lateral no conserva la clase del listado", () => {
    const { screen, root } = montar();
    const v = vista({});
    v.slots = [...v.slots, { kind: "processes", slot_id: 7, cursor: null } as never];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 0, y: 0, width: 40, height: 10, role: null, focus_index: 2 },
    ];
    screen.paint(v);
    expect(root.querySelectorAll(".scroller")).toHaveLength(1);
    expect(root.querySelectorAll(".processes").length).toBeGreaterThan(0);
  });
});

describe("repintar sin cambios (el parpadeo al desplazarse)", () => {
  // Cada respuesta a un scroll trae la vista ENTERA. Rehacer los nodos que no
  // cambiaron se veía como un parpadeo sutil en WebKitGTK: lo que se pinta
  // igual tiene que quedarse siendo el MISMO nodo.
  it("la misma foto conserva barras, título, cabecera y filas", () => {
    const { screen, root } = montar();
    const v = vista({});
    screen.paint(v);
    const antes = {
      columna: root.querySelector(".slot-columns .col"),
      ruta: root.querySelector(".title-path"),
      celda: root.querySelector(".row .cell-name"),
      paneles: document.querySelector(".panelbar"),
      menu: document.querySelector(".menubar"),
    };
    expect(antes.columna).not.toBeNull();
    expect(antes.celda).not.toBeNull();
    screen.paint(JSON.parse(JSON.stringify(v)) as ViewSnapshot);
    expect(root.querySelector(".slot-columns .col")).toBe(antes.columna);
    expect(root.querySelector(".title-path")).toBe(antes.ruta);
    expect(root.querySelector(".row .cell-name")).toBe(antes.celda);
    expect(document.querySelector(".panelbar")).toBe(antes.paneles);
    expect(document.querySelector(".menubar")).toBe(antes.menu);
  });

  it("pero lo que SÍ cambió se repinta", () => {
    const { screen, root } = montar();
    screen.paint(vista({}));
    screen.paint(
      vista({
        rows: [fila(0, "a.txt", { marked: true }), fila(1, "c.txt")],
        path_display: "⟨file⟩/otra",
      }),
    );
    const filas = root.querySelectorAll<HTMLElement>(".row");
    expect(filas[0]?.dataset["marked"]).toBe("true");
    expect(root.textContent).toContain("c.txt");
    expect(root.querySelector(".title-path")?.textContent).toContain("otra");
  });

  it("el aviso de espera sigue dentro del título aunque el título no se rehaga", () => {
    const { screen, root } = montar();
    screen.paint(vista({}));
    screen.paint(vista({ state: { state: "loading" } }));
    const busy = root.querySelector(".slot-busy");
    expect(busy?.parentElement?.classList.contains("slot-title")).toBe(true);
    expect((busy as HTMLElement | null)?.hidden).toBe(false);
  });
});

function montar(opciones: { imageBytes?: () => Promise<ArrayBuffer> } = {}): {
  screen: Screen;
  enviadas: UiAction[];
  root: HTMLElement;
} {
  document.body.replaceChildren();
  const root = document.createElement("main");
  const menu = document.createElement("div");
  const panelBar = document.createElement("div");
  const profiles = document.createElement("div");
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
  const agents = document.createElement("div");
  const pluginOutput = document.createElement("div");
  const programOutput = document.createElement("div");
  const viewer = document.createElement("div");
  const dialogs = document.createElement("div");
  const aiRename = document.createElement("div");
  const organize = document.createElement("div");
  const splash = document.createElement("div");
  document.body.append(
    root,
    panelBar,
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
    viewer,
    dialogs,
    aiRename,
    organize,
    splash,
  );
  document.documentElement.style.setProperty("--cell-h", `${CELL_H}px`);
  document.documentElement.style.setProperty("--cell-w", "8px");
  const enviadas: UiAction[] = [];
  const screen = new Screen(
    root,
    menu,
    panelBar,
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

  it("los tiradores de los bordes quedan POR ENCIMA de los huecos", () => {
    const { screen, root } = montar();
    const v = vista({});
    v.layout.placements = [
      { slot_id: 1, x: 0, y: 0, width: 60, height: 38, role: "active", focus_index: 0 },
      { slot_id: 2, x: 60, y: 0, width: 60, height: 38, role: null, focus_index: 1 },
    ];
    v.slots = [v.slots[0]!, { ...(v.slots[0] as BrowserSlotView), slot_id: 2 }];
    screen.paint(v);

    const tiradores = [...root.querySelectorAll(".resize-handle")];
    expect(tiradores.length).toBeGreaterThan(0);

    // Esta hoja de estilos no usa `z-index` en ninguna parte a propósito: el
    // apilado lo da el ORDEN del documento. Los tiradores se insertaban ANTES
    // que los huecos, así que cada panel —que también es `absolute`— los
    // tapaba y el `pointerdown` no les llegaba nunca. O sea que no se podía
    // redimensionar con el ratón.
    const clases = Array.from(root.children).map((n) => n.className);
    const ultimoHueco = clases.lastIndexOf("slot");
    const primerTirador = clases.findIndex((c) => c.startsWith("resize-handle"));
    expect(ultimoHueco).toBeGreaterThanOrEqual(0);
    expect(primerTirador).toBeGreaterThan(ultimoHueco);
  });

  it("marca el destino solo cuando el host dice que dice algo", () => {
    const { screen, root } = montar();
    // El UMBRAL lo decide Rust (`layout::target_worth_marking`) y llega en
    // `mark_target`: contarlo aquí sería repetir en TypeScript un número que
    // ya vive en el crate compartido, o sea la misma decisión en dos sitios.
    // Lo que este test fija es que el renderer OBEDECE la bandera y no se
    // inventa el rol a partir de `role`.
    const conBandera = (marcar: boolean): ViewSnapshot => {
      const v = vista({});
      v.layout.mark_target = marcar;
      v.layout.placements = [
        { slot_id: 1, x: 0, y: 0, width: 20, height: 10, role: "active", focus_index: 0 },
        { slot_id: 4, x: 0, y: 0, width: 20, height: 10, role: "target", focus_index: 1 },
      ];
      return v;
    };

    // El rol viaja igual —es el modelo— y aun así no se pinta: con dos
    // listados el destino es «el otro», y una marca que sale siempre deja de
    // leerse justo el día que hay tres y hace falta (ADR 0058 D7).
    screen.paint(conBandera(false));
    expect(root.querySelectorAll('[data-role="target"]')).toHaveLength(0);
    expect(root.querySelectorAll('[data-role="active"]')).toHaveLength(1);

    screen.paint(conBandera(true));
    expect(root.querySelectorAll('[data-role="target"]')).toHaveLength(1);
  });

  it("esperando: dice el verbo, a dónde va, y marca una ruta alterada", () => {
    const { screen, root } = montar();
    // Un refresco: no va a ninguna parte, así que no se inventa un sitio.
    screen.paint(vista({ state: { state: "loading", verb_key: "busy-listing" } }));
    const aviso = root.querySelector(".slot-busy") as HTMLElement;
    expect(aviso.hidden).toBe(false);
    expect(aviso.querySelector(".slot-busy-target")).toBeNull();

    // Yendo a un sitio, y con la ruta pintada distinta de lo que es: es la
    // que el lector mira mientras espera.
    screen.paint(
      vista({
        state: {
          state: "loading",
          verb_key: "busy-connecting",
          target_display: "⟨sftp⟩casa/caf�",
          target_hostile: true,
        },
      }),
    );
    const destino = root.querySelector(".slot-busy-target");
    expect(destino?.textContent).toBe("⟨sftp⟩casa/caf�");
    expect(root.querySelector(".slot-busy .hostile-badge")).not.toBeNull();

    // Con el listado ya puesto se ESCONDE, y es el MISMO nodo: su umbral es
    // un `animation-delay`, y recrearlo lo reiniciaría en cada pintada hasta
    // no aparecer nunca — que es justo en los casos lentos.
    const antes = root.querySelector(".slot-busy");
    screen.paint(vista({}));
    expect((root.querySelector(".slot-busy") as HTMLElement).hidden).toBe(true);
    expect(root.querySelector(".slot-busy")).toBe(antes);
  });

  it("apila en la cabecera todo lo que dice que el listado no es lo que parece", () => {
    const { screen, root } = montar();
    screen.paint(
      vista({
        filling_note: "cargando… (3)",
        skipped_note: "⚠ 2 entradas omitidas",
        names_note: "nombres: cp866",
        pruned_note: "1 marca caída",
        hidden_note: "3 ocultas",
        marked_note: "2 marcadas, 4,0 kB",
      }),
    );
    const notas = Array.from(
      root.querySelectorAll(
        ".slot-filling, .slot-skipped, .slot-names, .slot-pruned, .slot-hidden, .slot-marked",
      ),
    );
    expect(notas).toHaveLength(6);
    // Cada una en SU nodo: pegadas en uno solo, un lector de pantalla lee una
    // frase sola y el recorte se las lleva todas juntas.
    expect(notas.map((n) => n.className)).toEqual([
      "slot-filling",
      "slot-skipped",
      "slot-names",
      "slot-pruned",
      "slot-hidden",
      "slot-marked",
    ]);
    // El ORDEN es la decisión: los AVISOS antes que el CONTADOR de marcas. El
    // sitio se acaba, y un aviso recortado deja de avisar mientras que un
    // contador recortado solo deja de contar.
    expect(notas[notas.length - 1]?.className).toBe("slot-marked");
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

  it("el tema colorea el nombre de una entrada", () => {
    const { screen, root } = montar();
    screen.paint(
      vista({
        rows: [fila(0, "src", { kind: "dir", name_color: "#4daafc", name_bold: true })],
      }),
    );
    const name = root.querySelector(".cell-name") as HTMLElement;
    // El navegador normaliza a rgb(): se compara lo que de verdad computa.
    expect(name.style.color).toBe("rgb(77, 170, 252)");
    expect(name.style.fontWeight).toBe("bold");
  });

  it("bajo el CURSOR manda el color de la selección, no el del fichero", () => {
    // Réplica del terminal, donde `highlight_style` pisa el estilo del item
    // cuando el tema da primer plano a `selection` — y los diez presets se lo
    // dan. Sin esto, un directorio azul oscuro sobre el #04395e de
    // vscode-dark sería ilegible justo en la fila que se está mirando.
    const { screen, root } = montar();
    screen.paint(
      vista({
        rows: [
          fila(0, "src", {
            kind: "dir",
            selected: true,
            name_color: "#4daafc",
            name_bold: true,
          }),
        ],
      }),
    );
    const name = root.querySelector(".cell-name") as HTMLElement;
    expect(name.style.color).toBe("");
    // La negrita SÍ se conserva: dice qué ES la entrada, no de qué color, y
    // no compite con el fondo de la selección.
    expect(name.style.fontWeight).toBe("bold");
  });

  it("los atributos del tema llegan, no solo el color", () => {
    // `retro-crt` atenúa zip/tar/gz con `dim = true`: llevando solo `fg`
    // salían apagados en el terminal y a plena luz aquí.
    const { screen, root } = montar();
    screen.paint(
      vista({
        rows: [
          fila(0, "backup.zip", {
            name_color: "#d75f5f",
            name_dim: true,
            name_italic: true,
            name_underline: true,
          }),
        ],
      }),
    );
    const name = root.querySelector(".cell-name") as HTMLElement;
    expect(name.style.opacity).toBe("0.6");
    expect(name.style.fontStyle).toBe("italic");
    expect(name.style.textDecoration).toBe("underline");
  });

  it("el icono lleva el color de su entrada (ADR 0105)", () => {
    // Un icono dice qué ES la fila, no en qué estado está: sigue al color de
    // su nombre, como en el terminal.
    const { screen, root } = montar();
    screen.paint(
      vista({
        icon_column: true,
        rows: [fila(0, "src", { kind: "dir", icon: "📁", name_color: "#4daafc" })],
      }),
    );
    const icono = root.querySelector(".cell-icon") as HTMLElement;
    expect(icono.style.color).toBe("rgb(77, 170, 252)");
  });

  it("un tema que no dice nada de una entrada no le pone color", () => {
    const { screen, root } = montar();
    screen.paint(vista({ rows: [fila(0, "notas.txt")] }));
    const name = root.querySelector(".cell-name") as HTMLElement;
    expect(name.style.color).toBe("");
    expect(name.style.fontWeight).toBe("");
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

  it("un click señala la fila; dos seguidos sobre la misma la abren", () => {
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
    // El SEGUNDO `mousedown` sobre la misma fila: se cuenta aquí, sin
    // esperar al evento `dblclick` del motor — que es lo que fallaba.
    fila1.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(enviadas.at(-1)).toEqual({
      action: "activate",
      slot_id: 1,
      key: 1,
      generation: 1,
    });
  });

  it("los botones laterales del ratón son atrás y adelante en la historia", () => {
    // Spec 2026-09-15 D1: la convención de escritorio. Se escuchan en
    // `mouseup` y se cancelan, para que el webview no los tome por navegación
    // de la PÁGINA.
    const { screen, enviadas, root } = montar();
    screen.paint(vista({}));
    const fila = root.querySelectorAll(".row")[0] as HTMLElement;
    const atras = new MouseEvent("mouseup", {
      bubbles: true,
      cancelable: true,
      button: 3,
    });
    fila.dispatchEvent(atras);
    expect(enviadas.at(-1)).toEqual({ action: "history", slot_id: 1, back: true });
    expect(atras.defaultPrevented).toBe(true);
    fila.dispatchEvent(
      new MouseEvent("mouseup", { bubbles: true, cancelable: true, button: 4 }),
    );
    expect(enviadas.at(-1)).toEqual({ action: "history", slot_id: 1, back: false });
    // El botón principal no toca el rastro.
    const antes = enviadas.length;
    fila.dispatchEvent(
      new MouseEvent("mouseup", { bubbles: true, cancelable: true, button: 0 }),
    );
    expect(enviadas).toHaveLength(antes);
  });

  it("dos clics en filas DISTINTAS no abren nada, y el tercero de una ráfaga tampoco", () => {
    const { screen, enviadas, root } = montar();
    screen.paint(vista({ rows: [fila(0, "a.txt"), fila(1, "b.txt")] }));
    const filas = root.querySelectorAll(".row");
    (filas[0] as HTMLElement).dispatchEvent(
      new MouseEvent("mousedown", { bubbles: true }),
    );
    (filas[1] as HTMLElement).dispatchEvent(
      new MouseEvent("mousedown", { bubbles: true }),
    );
    expect(enviadas.some((a) => a.action === "activate")).toBe(false);
    // Dos sobre la misma: abre UNA vez.
    (filas[1] as HTMLElement).dispatchEvent(
      new MouseEvent("mousedown", { bubbles: true }),
    );
    (filas[1] as HTMLElement).dispatchEvent(
      new MouseEvent("mousedown", { bubbles: true }),
    );
    expect(enviadas.filter((a) => a.action === "activate")).toHaveLength(1);
  });

  it("dos clics separados en el tiempo son dos clics, no un doble", () => {
    const { screen, enviadas, root } = montar();
    screen.paint(vista({}));
    const fila1 = root.querySelectorAll(".row")[1] as HTMLElement;
    const reloj = vi.spyOn(Date, "now");
    reloj.mockReturnValue(1_000);
    fila1.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    reloj.mockReturnValue(1_000 + 900);
    fila1.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    reloj.mockRestore();
    expect(enviadas.some((a) => a.action === "activate")).toBe(false);
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
      // 400/20 = fila 20 arriba y 200/20 = 10 visibles, con el margen por
      // cada lado. De la constante: con el 8 escrito aquí, subir el margen
      // rompía el test sin que el comportamiento cambiara.
      expect(ultima.first).toBe(Math.max(0, 20 - OVERSCAN));
      expect(ultima.count).toBe(10 + OVERSCAN * 2);
    }
  });

  it("la barra de estado se anuncia sin robar el foco", () => {
    const { screen, root } = montar();
    screen.paint(vista({}));
    const status = root.querySelectorAll(".slot")[1]?.querySelector(".statusbar");
    expect(status?.getAttribute("aria-live")).toBe("polite");
    expect(status?.textContent).toContain("2 entradas");
  });

  it("una orden que el host rechaza en la frontera se ve en la barra de estado", () => {
    // Una acción que no deserializa muere en `dispatch`, antes de que el
    // host la vea: nadie salvo el renderer puede decirlo. Así estuvo la
    // barra de paneles entera, con el error solo en la consola.
    const { screen, root } = montar();
    screen.paint(vista({}));
    const barra = () => root.querySelectorAll(".slot")[1]?.querySelector(".statusbar");
    screen.rejected(
      { action: "panel_bar_activate", button: 1 },
      new Error("unknown variant"),
    );
    expect(barra()?.textContent).toContain("panel_bar_activate");
    expect(barra()?.querySelector(".banner.rejected")).not.toBeNull();
    // Sigue viéndose en el siguiente repintado del host: el aviso es local.
    screen.paint(vista({}));
    expect(barra()?.textContent).toContain("panel_bar_activate");
    // La primera orden aceptada lo retira.
    expect(screen.accepted()).toBe(true);
    expect(screen.accepted()).toBe(false);
    screen.paint(vista({}));
    expect(barra()?.textContent).not.toContain("panel_bar_activate");
    expect(barra()?.textContent).toContain("2 entradas");
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
        input_secret: false,
      },
    ];
    screen.paint(v);
    const dialog = document.querySelector('[role="dialog"]') as HTMLElement;
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.getAttribute("aria-labelledby")).toBeTruthy();
    const destructivo = dialog.querySelector('button[data-destructive="true"]');
    expect(destructivo).not.toBeNull();
  });

  it("un destino a medio comprobar lo DICE, y sus avisos salen antes de los botones", () => {
    const { screen } = montar();
    const base = {
      id: 4,
      title_key: "modal-copy-title",
      subject: null,
      asker: null,
      deadline: null,
      destination: { text: "/casa/docs", hostile: false },
      body: [{ text: "a.txt", hostile: false }],
      overflow_note: "",
      choices: [
        { id: "confirm", label_key: "dialog-confirm", destructive: false },
        { id: "cancel", label_key: "dialog-cancel", destructive: false },
      ],
      input: null,
      input_hostile: false,
      input_secret: false,
    };

    // Mientras se pregunta se DICE. Sin esta línea la ausencia de la de #164
    // se leería como «este destino confina», que es una afirmación.
    const preguntando = vista({});
    preguntando.dialogs = [{ ...base, dest_check: { state: "checking" } }];
    screen.paint(preguntando);
    expect(document.querySelector(".dialog-checking")).not.toBeNull();
    expect(document.querySelectorAll(".dialog-warning")).toHaveLength(0);

    // Contestado y con avisos: uno por línea, cada uno como alerta.
    const conAvisos = vista({});
    conAvisos.dialogs = [
      {
        ...base,
        dest_check: {
          state: "done",
          warnings: ["no cabe", "no puede confinar"],
        },
      },
    ];
    screen.paint(conAvisos);
    const avisos = Array.from(document.querySelectorAll(".dialog-warning"));
    expect(avisos).toHaveLength(2);
    const primero = avisos[0] as HTMLElement;
    expect(primero.textContent).toBe("no cabe");
    expect(primero.getAttribute("role")).toBe("alert");
    expect(document.querySelector(".dialog-checking")).toBeNull();
    // Y ANTES de los botones: un aviso que aterrizara debajo movería lo que
    // hay bajo el puntero de quien ya iba a pulsar.
    const dialogo = document.querySelector('[role="dialog"]') as HTMLElement;
    const clases = Array.from(dialogo.children).map((n) => n.className);
    const ultimoAviso = clases.lastIndexOf("dialog-warning");
    const botones = clases.indexOf("choices");
    expect(ultimoAviso).toBeGreaterThanOrEqual(0);
    expect(botones).toBeGreaterThan(ultimoAviso);

    // Contestado y limpio: ni una línea. Que quepa y que confine no se
    // anuncian — una línea en cada copia enseña a saltarse la línea.
    const limpio = vista({});
    limpio.dialogs = [{ ...base, dest_check: { state: "done", warnings: [] } }];
    screen.paint(limpio);
    expect(document.querySelectorAll(".dialog-warning")).toHaveLength(0);
    expect(document.querySelector(".dialog-checking")).toBeNull();
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
        input_secret: false,
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
      slot.columns = [
        {
          id: "plugin:x/y",
          label: "X",
          sort: null,
          sortable: false,
          width: null,
          align: "left",
        },
      ];
    }
    screen.paint(v);
    const col = root.querySelector(".slot-columns .col") as HTMLElement;
    col.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(enviadas.some((a) => a.action === "sort_by")).toBe(false);
  });

  it("un ancho fijo se declara en la raíz del hueco y las celdas lo leen", () => {
    const { screen, root } = montar();
    screen.paint(vista({ rows: [fila(1, "a.txt")] }));
    const hueco = root.querySelector(".slot") as HTMLElement;
    // `size` viene con 9 celdas y a la derecha; `name` no lleva variable.
    expect(hueco.style.getPropertyValue("--colw-size")).toBe("calc(var(--cell-w) * 9)");
    expect(hueco.style.getPropertyValue("--colw-size-align")).toBe("right");
    expect(hueco.style.getPropertyValue("--colw-name")).toBe("");
    const celda = root.querySelector(".row .cell") as HTMLElement;
    expect(celda.style.width).toBe("var(--colw-size, auto)");
    // El nombre no tiene tirador; el tamaño sí.
    const cols = root.querySelectorAll(".slot-columns .col");
    expect(cols[0]?.querySelector(".col-grip")).toBeNull();
    expect(cols[1]?.querySelector(".col-grip")).not.toBeNull();
  });

  it("arrastrar el tirador manda el ancho en CELDAS al soltar, y no ordena", () => {
    const { screen, enviadas, root } = montar();
    document.documentElement.style.setProperty("--cell-w", "8px");
    screen.paint(vista({}));
    const grip = root.querySelector(".col-grip") as HTMLElement;
    grip.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, clientX: 100 }));
    expect(enviadas.some((a) => a.action === "sort_by")).toBe(false);
    document.dispatchEvent(new MouseEvent("mousemove", { clientX: 140 }));
    // Mientras se arrastra, solo cambia la variable: ningún envío.
    const hueco = root.querySelector(".slot") as HTMLElement;
    expect(hueco.style.getPropertyValue("--colw-size")).toBe("40px");
    expect(enviadas.some((a) => a.action === "resize_column")).toBe(false);
    document.dispatchEvent(new MouseEvent("mouseup"));
    expect(enviadas.at(-1)).toEqual({
      action: "resize_column",
      slot_id: 1,
      column: "size",
      cells: 5,
    });
  });
});

describe("las migas, el indicador de espacio y el toast", () => {
  it("cada tramo de la ruta es un botón que navega a su profundidad, salvo el actual", () => {
    const { screen, enviadas, root } = montar();
    screen.paint(
      vista({
        path_segments: ["⟨file⟩", "home", "oscar"],
        path_display: "⟨file⟩/home/oscar",
      }),
    );
    const migas = root.querySelectorAll(".title-path .crumb");
    expect([...migas].map((m) => m.textContent)).toEqual(["⟨file⟩", "home", "oscar"]);
    expect((migas[2] as HTMLButtonElement).disabled).toBe(true);
    (migas[1] as HTMLButtonElement).click();
    expect(enviadas.at(-1)).toEqual({
      action: "breadcrumb_activate",
      slot_id: 1,
      depth: 1,
      generation: 1,
    });
    // La ruta entera sigue disponible de una pieza.
    expect(root.querySelector(".title-path")?.getAttribute("title")).toBe(
      "⟨file⟩/home/oscar",
    );
  });

  it("sin migas, la ruta va como texto, igual que antes", () => {
    const { screen, root } = montar();
    screen.paint(vista({}));
    expect(root.querySelector(".title-path")?.textContent).toBe("⟨file⟩/casa");
    expect(root.querySelector(".crumb")).toBeNull();
  });

  it("el pie lleva el indicador solo con dato, y dice su nivel", () => {
    const { screen, root } = montar();
    screen.paint(vista({ footer: "2 ficheros", used_ratio: 0.92 }));
    const gauge = root.querySelector(".slot-footer .slot-gauge") as HTMLElement;
    expect(gauge.getAttribute("aria-valuenow")).toBe("92");
    expect(gauge.dataset["level"]).toBe("critical");
    expect((gauge.firstElementChild as HTMLElement).style.width).toBe("92%");
    screen.paint(vista({ footer: "2 ficheros", used_ratio: null }));
    expect(root.querySelector(".slot-gauge")).toBeNull();
  });

  it("el mensaje efímero va en su nodo de toast", () => {
    const { screen, root } = montar();
    screen.paint(vista({}));
    expect(root.querySelector(".statusbar .status-message")?.textContent).toBe(
      "2 entradas",
    );
  });
});

describe("la casilla de marca", () => {
  it("cada fila lleva la casilla, dice si está marcada, y pulsarla alterna la marca", () => {
    const { screen, enviadas, root } = montar();
    screen.paint(vista({ rows: [fila(1, "a.txt"), fila(2, "b.txt", { marked: true })] }));
    const casillas = root.querySelectorAll(".row .row-check");
    expect([...casillas].map((c) => c.textContent)).toEqual(["☐", "☑"]);
    casillas[0]?.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(enviadas.at(-1)).toMatchObject({ action: "toggle_mark", slot_id: 1, key: 1 });
  });
});

describe("el visor", () => {
  it("tapa la pantalla y dice con qué encoding está leyendo", () => {
    const { screen, enviadas } = montar();
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
      total_cols: 0,
      first_col: 0,
      lines: ["primera", "segunda"],
      preview_by: "",
      preview_lossy: false,
      image: null,
      image_refused: "",
      styled: [],
    };
    screen.paint(v);
    const doc = document.querySelector('[role="document"]') as HTMLElement;
    expect(doc.getAttribute("aria-label")).toContain("notas.txt");
    expect(doc.querySelector(".viewer-body")?.textContent).toBe("primera\nsegunda");
    expect(doc.querySelector(".viewer-meta")?.textContent).toContain("UTF-8");
    // Quien pinta declara el tamaño del cuerpo: filas Y columnas (puente
    // 53), que es lo que el previewer recibe la próxima vez.
    expect(enviadas.some((a) => a.action === "set_viewer_rows")).toBe(true);
    expect(enviadas.some((a) => a.action === "set_viewer_cols")).toBe(true);
  });

  it("dice que hay más a lo ancho, y la rueda lo mueve", () => {
    const { screen, enviadas } = montar();
    const v = vista({});
    const base = {
      path_display: "⟨file⟩/casa/pagina.html",
      path_hostile: false,
      encoding: "UTF-8",
      eol: "lf",
      hex: false,
      forced: false,
      had_errors: false,
      truncated: false,
      total_rows: 2,
      first_line: 0,
      lines: ["<html>", "</html>"],
      preview_by: "",
      preview_lossy: false,
      image: null,
      image_refused: "",
      styled: [],
    };

    // Cabe a lo ancho: ninguna barra que arrastrar.
    v.viewer = { ...base, total_cols: 0, first_col: 0 };
    screen.paint(v);
    expect(document.querySelector(".viewer-bar-h")).toBe(null);

    // No cabe: barra, y con la posición dentro.
    v.viewer = { ...base, total_cols: 800, first_col: 400 };
    screen.paint(v);
    const barra = document.querySelector(".viewer-bar-h") as HTMLElement;
    expect(barra).not.toBe(null);
    // La barra es un INDICADOR y va `aria-hidden`: `role="scrollbar"` promete
    // un control que no existe. La posición se lee en las marcas de la
    // cabecera, con palabras, que es lo que llega a quien no la ve.
    expect(barra.getAttribute("aria-hidden")).toBe("true");
    expect(document.querySelector(".viewer-meta")?.textContent).toContain("401/800");

    // Y la rueda desplaza por el HOST: con `shift`, de lado.
    const caja = document.querySelector(".viewer") as HTMLElement;
    caja.dispatchEvent(new WheelEvent("wheel", { deltaY: 120, bubbles: true }));
    const abajo = enviadas.find((a) => a.action === "viewer_scroll");
    expect(abajo).toBeDefined();
    expect(abajo?.action === "viewer_scroll" && abajo.lines > 0).toBe(true);
    expect(abajo?.action === "viewer_scroll" && abajo.cols === 0).toBe(true);

    caja.dispatchEvent(
      new WheelEvent("wheel", { deltaY: 120, shiftKey: true, bubbles: true }),
    );
    const lado = enviadas.filter((a) => a.action === "viewer_scroll").at(-1);
    expect(lado?.action === "viewer_scroll" && lado.lines === 0).toBe(true);
    expect(lado?.action === "viewer_scroll" && lado.cols > 0).toBe(true);
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
      total_cols: 0,
      first_col: 0,
      lines: ["00000000  00 01 02 ff"],
      preview_by: "",
      preview_lossy: false,
      image: null,
      image_refused: "",
      styled: [],
    };
    screen.paint(v);
    expect(document.querySelector(".viewer-body")?.classList.contains("hexview")).toBe(
      true,
    );
    expect(document.querySelector(".viewer-meta")?.textContent).toContain("hex");
  });

  it("una preview de plugin se pinta por fragmentos, con su rol o su color", () => {
    const { screen } = montar();
    const v = vista({});
    v.viewer = {
      path_display: "⟨file⟩/casa/main.rs",
      path_hostile: false,
      encoding: "UTF-8",
      eol: "lf",
      hex: false,
      forced: false,
      had_errors: false,
      truncated: false,
      total_rows: 2,
      first_line: 0,
      total_cols: 0,
      first_col: 0,
      lines: ["fn main", "plano"],
      preview_by: "via Syntax",
      preview_lossy: false,
      image: null,
      image_refused: "",
      styled: [
        [
          { text: "fn", role: "title", fg: "#ff0000", bg: null },
          { text: " main", role: null, fg: "#0080ff", bg: "#00ff00" },
        ],
        [{ text: "plano", role: null, fg: null, bg: null }],
      ],
    };
    screen.paint(v);
    const body = document.querySelector(".viewer-body") as HTMLElement;
    const lineas = body.querySelectorAll(".viewer-line");
    expect(lineas.length).toBe(2);
    const spans = (lineas[0] as Element).querySelectorAll<HTMLElement>(".viewer-span");
    expect(spans.length).toBe(2);
    const fn_ = spans[0] as HTMLElement;
    const main = spans[1] as HTMLElement;
    expect(fn_.textContent).toBe("fn");
    // El rol manda: va en `data-role` y el color fijo del plugin NO se aplica.
    expect(fn_.dataset["role"]).toBe("title");
    expect(fn_.style.color).toBe("");
    // Sin rol, el color propio del plugin sí.
    expect(main.dataset["role"]).toBeUndefined();
    expect(main.style.color).toBe("rgb(0, 128, 255)");
    // Y el fondo, cuando viene (puente 50).
    expect(main.style.backgroundColor).toBe("rgb(0, 255, 0)");
    expect(fn_.style.backgroundColor).toBe("");
    // El texto sigue siendo TEXTO.
    expect(body.textContent).toBe("fn mainplano");
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
      total_cols: 0,
      first_col: 0,
      lines: ["<script>alert(1)</script>"],
      preview_by: "",
      preview_lossy: false,
      image: null,
      image_refused: "",
      styled: [],
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
        input_secret: false,
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

  it("el campo conserva el FOCO a través del parche que provoca cada tecla", () => {
    // Reusar el nodo no bastaba: la caja se rehace y el campo se mueve a la
    // nueva, y mover un nodo lo saca del documento un instante — ahí perdía
    // el foco. Borrar un número en «Tamaño de letra» dejaba el campo sin
    // foco y la siguiente tecla se iba al host como un acorde.
    const { screen } = montar();
    screen.paint(conDialogo("10", false));
    const campo = campoVivo();
    campo.focus();
    expect(document.activeElement).toBe(campo);
    campo.value = "1";
    campo.dispatchEvent(new Event("input", { bubbles: true }));
    screen.paint(conDialogo("1", false));
    expect(campoVivo()).toBe(campo);
    expect(document.activeElement).toBe(campo);
  });

  it("una contraseña se pinta como contraseña y no se resiembra con los puntos", () => {
    const { screen, enviadas } = montar();
    const v = conDialogo("", false);
    v.dialogs[0]!.title_key = "modal-ask-secret-title";
    v.dialogs[0]!.input_secret = true;
    screen.paint(v);

    const campo = campoVivo();
    expect(campo.type).toBe("password");
    // Ni el gestor de contraseñas del navegador lo ofrece ni lo guarda: esto
    // es para ESTA sesión, que es lo que el cuerpo del diálogo promete.
    // `new-password` y no `off`: Chromium y WebView2 IGNORAN `off` en un campo
    // de contraseña a propósito, y este es el valor que sí respetan.
    expect(campo.autocomplete).toBe("new-password");

    campo.value = "s3cr3t";
    campo.dispatchEvent(new Event("input", { bubbles: true }));

    // Teclear NO manda nada: por `dialog_input` cruzarían `s`, `s3`, `s3c`… y
    // cada prefijo se quedaría en un trozo de heap que nadie pisa.
    expect(enviadas.some((a) => a.action === "dialog_input")).toBe(false);

    // Y un repintado no resiembra el campo: hacerlo convertiría la contraseña
    // del usuario en lo que mandara el host.
    screen.paint(v);
    expect(campoVivo().value).toBe("s3cr3t");

    // Cruza UNA vez, con la respuesta.
    const boton = document.querySelector(".dialog .choices button");
    (boton as HTMLButtonElement).click();
    const ultima = enviadas.at(-1);
    expect(ultima?.action).toBe("dialog");
    if (ultima?.action === "dialog") {
      expect(ultima.choice).toBe("confirm");
      expect(ultima.secret).toBe("s3cr3t");
    }
  });

  it("Enter dentro del campo de contraseña confirma y lleva el valor", () => {
    const { screen, enviadas } = montar();
    const v = conDialogo("", false);
    v.dialogs[0]!.title_key = "modal-ask-secret-title";
    v.dialogs[0]!.input_secret = true;
    screen.paint(v);

    const campo = campoVivo();
    campo.value = "s3cr3t";
    campo.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));

    // Sin esto, Enter salía al host como el acorde `dialog.confirm` — que
    // sobre un diálogo de contraseña no lleva nada y por tanto es inerte—,
    // así que la forma más natural de contestar no habría hecho nada.
    const ultima = enviadas.at(-1);
    expect(ultima?.action).toBe("dialog");
    if (ultima?.action === "dialog") {
      expect(ultima.secret).toBe("s3cr3t");
    }
  });

  it("cancelar no lleva la contraseña", () => {
    const { screen, enviadas } = montar();
    const v = conDialogo("", false);
    v.dialogs[0]!.title_key = "modal-ask-secret-title";
    v.dialogs[0]!.input_secret = true;
    v.dialogs[0]!.choices = [
      { id: "confirm", label_key: "dialog-confirm", destructive: false },
      { id: "cancel", label_key: "dialog-cancel", destructive: false },
    ];
    screen.paint(v);
    campoVivo().value = "s3cr3t";

    const botones = document.querySelectorAll(".dialog .choices button");
    (botones[1] as HTMLButtonElement).click();
    const ultima = enviadas.at(-1);
    expect(ultima?.action).toBe("dialog");
    if (ultima?.action === "dialog") {
      expect(ultima.choice).toBe("cancel");
      expect(ultima.secret).toBeUndefined();
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
    total_cols: 0,
    first_col: 0,
    lines: ["00000000  89 50 4e 47"],
    preview_by: "",
    preview_lossy: false,
    styled: [],
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
      total_cols: 0,
      first_col: 0,
      lines: ["Informe anual"],
      preview_by: "via PDF de ACME",
      preview_lossy: true,
      image: null,
      image_refused: "",
      styled: [],
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
      total_cols: 0,
      first_col: 0,
      lines: ["x"],
      preview_by: "via PDF de ACME",
      preview_lossy: true,
      image: null,
      image_refused: "",
      styled: [],
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
      total_cols: 0,
      first_col: 0,
      lines: ["hola"],
      preview_by: "",
      preview_lossy: false,
      image: null,
      image_refused: "",
      styled: [],
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
      // El pie lo pinta el HOST desde el keymap (#287).
      hint: "Espacio activa · Enter aplica",
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

  it("el icono va a la IZQUIERDA del nombre, y la columna se abre para todas las filas", () => {
    const { screen } = montar();
    screen.paint(
      vista({
        rows: [
          fila(1, "src", { kind: "dir", icon: "📁" }),
          fila(2, "main.rs", { icon: "🦀", badge: "M", badge_role: "warning" }),
          fila(3, "sin-icono"),
        ],
        icon_column: true,
      }),
    );
    const filas = [...document.querySelectorAll(".row")];
    // Las tres llevan la celda: la que no tiene icono, vacía, para que los
    // nombres sigan alineados.
    for (const f of filas) {
      expect(f.querySelector(".cell-icon")).not.toBeNull();
    }
    expect(filas[0]?.querySelector(".cell-icon")?.textContent).toBe("📁");
    expect(filas[2]?.querySelector(".cell-icon")?.textContent).toBe("");
    // Antes del nombre; la insignia, detrás. Los dos huecos en una fila.
    const bloque = filas[1]?.querySelector(".name-block");
    const hijos = [...(bloque?.children ?? [])].map((c) => c.className);
    expect(hijos).toEqual(["cell-icon", "cell-name", "cell-badge"]);
  });

  it("la columna la abre el HOST, no las filas visibles", () => {
    const { screen } = montar();
    // Sin iconos a la vista pero con la columna abierta —una página sin
    // iconos de un listado que sí los tiene—: la celda sigue, para que los
    // nombres no se corran al desplazarse.
    screen.paint(vista({ rows: [fila(1, "a.rs"), fila(2, "b.rs")], icon_column: true }));
    expect(document.querySelectorAll(".cell-icon")).toHaveLength(2);
    // Y al revés: un icono en una fila con la columna cerrada no la abre.
    screen.paint(vista({ rows: [fila(1, "a.rs", { icon: "🦀" })], icon_column: false }));
    expect(document.querySelector(".cell-icon")).toBeNull();
  });

  it("un icono que se pinta distinto de lo que es lo DICE", () => {
    const { screen } = montar();
    screen.paint(
      vista({
        rows: [fila(1, "x", { icon: "�", icon_hostile: true })],
        icon_column: true,
      }),
    );
    const icono = document.querySelector(".cell-icon");
    expect(icono?.getAttribute("data-hostile")).toBe("true");
    expect(icono?.querySelector(".hostile-badge")).not.toBeNull();
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

describe("la barra de menús", () => {
  function conMenu(open: number | null) {
    const v = vista({});
    v.menu = {
      bar: true,
      titles: ["Archivo", "Paneles"],
      open,
      items:
        open === null
          ? []
          : [
              {
                label: "Cambiar de panel",
                chord: "tab",
                enabled: true,
                section: null,
                role: "normal",
              },
              {
                label: "Desconectar",
                chord: "",
                enabled: false,
                section: "Sitios",
                role: "normal",
              },
              {
                label: "Borrar",
                chord: "F8",
                enabled: true,
                section: "",
                role: "destructive",
              },
            ],
      cursor: 1,
    };
    return v;
  }

  it("pinta las secciones sin contarlas como entradas", () => {
    const { screen } = montar();
    screen.paint(conMenu(1));
    const secciones = [...document.querySelectorAll(".menu-section")];
    expect(secciones.map((s) => s.textContent)).toEqual(["Sitios", ""]);
    expect(secciones.every((s) => s.getAttribute("role") === "separator")).toBe(true);
    // El cursor sigue nombrando ENTRADAS: la 1 es «Desconectar», no la raya.
    const actual = document.querySelector('.menu-item[data-current="true"]');
    expect(actual?.id).toBe("menu-item-1");
    const borrar = document.querySelector("#menu-item-2");
    expect(borrar?.getAttribute("data-role")).toBe("destructive");
  });

  it("pinta los títulos y reserva su fila", () => {
    const { screen } = montar();
    screen.paint(conMenu(null));
    const titulos = [...document.querySelectorAll(".menubar-title")].map(
      (t) => t.textContent,
    );
    expect(titulos).toEqual(["Archivo", "Paneles"]);
    // La fila se RESERVA: el reparto del host se calcula sobre el alto que
    // este renderer declara, y una barra flotante taparía la primera fila.
    expect(document.documentElement.style.getPropertyValue("--menubar-h")).toBe(
      "var(--cell-h)",
    );
    expect(document.querySelector(".menu-items")).toBeNull();
  });

  it("con la barra apagada no reserva nada", () => {
    const { screen } = montar();
    const v = conMenu(null);
    v.menu.bar = false;
    screen.paint(v);
    expect(document.documentElement.style.getPropertyValue("--menubar-h")).toBe("0px");
    expect(document.querySelector(".menubar")).toBeNull();
  });

  // #324: la barra de paneles ENSEÑA los paneles — estado, novedad, y un
  // click que vuelve como índice, nunca como comando (ADR 0069).
  it("la barra de paneles pinta cada botón con su estado y reserva su fila", () => {
    const { screen, enviadas } = montar();
    screen.paint(vista({}));
    expect(document.documentElement.style.getPropertyValue("--panelbar-h")).toBe(
      "var(--cell-h)",
    );
    const botones = [
      ...document.querySelectorAll(".panelbar-button"),
    ] as HTMLButtonElement[];
    expect(botones.map((b) => b.dataset["kind"])).toEqual(["places", "log"]);
    expect(botones[0]?.dataset["state"]).toBe("open");
    expect(botones[0]?.getAttribute("aria-pressed")).toBe("true");
    expect(botones[0]?.title).toBe("Sitios (alt+p)");
    expect(botones[1]?.dataset["state"]).toBe("closed");
    expect(botones[1]?.getAttribute("aria-pressed")).toBe("false");
    expect(botones[1]?.title).toBe("Registro");
    // La novedad es una marca APARTE, no un cambio de estilo del botón.
    expect(botones[0]?.querySelector(".panelbar-attention")).toBeNull();
    expect(botones[1]?.querySelector(".panelbar-attention")).not.toBeNull();
    // Y la fila tiene su landmark, traducido del catálogo real.
    expect(document.querySelector(".panelbar")?.getAttribute("aria-label")).toBe(
      "Barra de paneles",
    );

    botones[1]?.click();
    expect(enviadas).toEqual([{ action: "panel_bar_activate", button: 1 }]);
  });

  it("con la barra de paneles apagada no reserva nada", () => {
    const { screen } = montar();
    const v = vista({});
    v.panel_bar.bar = false;
    screen.paint(v);
    expect(document.documentElement.style.getPropertyValue("--panelbar-h")).toBe("0px");
    expect(document.querySelector(".panelbar")).toBeNull();
  });

  it("el desplegado marca su título, su cursor y lo que no se puede hacer", () => {
    const { screen } = montar();
    screen.paint(conMenu(1));
    const abierto = document.querySelectorAll('.menubar-title[aria-expanded="true"]');
    expect(abierto).toHaveLength(1);
    expect(abierto[0]?.textContent).toBe("Paneles");
    const lista = document.querySelector(".menu-items") as HTMLElement;
    expect(lista.getAttribute("aria-activedescendant")).toBe("menu-item-1");
    const filas = [...document.querySelectorAll(".menu-item")];
    expect(filas[0]?.textContent).toBe("Cambiar de paneltab");
    // Una entrada que esta ventana no ejecuta SIGUE saliendo: el menú es
    // donde se ve qué existe.
    expect(filas[1]?.getAttribute("aria-disabled")).toBe("true");
  });

  it("el desplegable cuelga del título PINTADO, no de una cuenta en celdas", () => {
    // Los títulos se pintan con relleno en píxeles y no miden lo mismo: una
    // cuenta a `12ch` por título se desviaba más cuanto más a la derecha, y
    // «Ayuda» abría su desplegable un título más allá. jsdom no maqueta, así
    // que la geometría del título se finge: lo que se comprueba es que la
    // medida del título es lo que coloca la lista.
    const { screen } = montar();
    const medida = vi
      .spyOn(HTMLElement.prototype, "getBoundingClientRect")
      .mockImplementation(function (this: HTMLElement): DOMRect {
        const left = this.id === "menu-title-1" ? 123 : 0;
        return new DOMRect(left, 0, 0, 0);
      });
    try {
      screen.paint(conMenu(1));
    } finally {
      medida.mockRestore();
    }
    const lista = document.querySelector(".menu-items") as HTMLElement;
    expect(lista.style.getPropertyValue("--menu-left")).toBe("123px");
  });

  it("el ratón despliega, señala, ejecuta y cierra", () => {
    const { screen, enviadas } = montar();
    screen.paint(conMenu(null));
    (document.querySelectorAll(".menubar-title")[1] as HTMLElement).click();
    screen.paint(conMenu(1));
    const fila = document.querySelectorAll(".menu-item")[0] as HTMLElement;
    fila.dispatchEvent(new MouseEvent("mousemove", { bubbles: true }));
    fila.click();
    (document.querySelector(".menu-veil") as HTMLElement).click();
    expect(enviadas).toEqual([
      { action: "menu_open", menu: 1 },
      { action: "menu_point_row", row: 0 },
      { action: "menu_activate_row", row: 0 },
      { action: "menu_close" },
    ]);
  });
});

describe("la paleta", () => {
  function conPaleta(cursor: number | null) {
    const v = vista({});
    v.palette = {
      query: "cur",
      rows: [
        {
          text: "cursor.up",
          desc: "subir el cursor",
          chord: "Up",
          enabled: true,
          hostile: false,
        },
        {
          text: "cursor.down",
          desc: "bajar el cursor",
          chord: "Down",
          enabled: true,
          hostile: false,
        },
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

  it("el cuerpo del visor se pinta en orden lógico, como un terminal", () => {
    // El recorte horizontal lo hace el HOST en orden lógico, una sola vez, en
    // el modelo que comparte con el terminal. Un navegador que reordenara por
    // el algoritmo bidi dejaría la misma `first_col` enseñando cosas distintas
    // en las dos superficies — y sin ninguna marca, porque una línea de LETRAS
    // árabes o hebreas no lleva controles: nada se enmascara y `had_errors` es
    // falso. Es la misma renuncia que ya hace `must_mask` con los aislantes
    // legítimos: honestidad de rejilla por encima de tipografía.
    //
    // Sobre la HOJA como texto y por el mismo motivo que el bloque de la
    // ayuda: jsdom no la aplica, así que `getComputedStyle` no comprobaría
    // nada. Lo que hay que impedir es que la regla desaparezca.
    const css = readFileSync(resolve(process.cwd(), "src/style.css"), "utf8");
    const bloque = css
      .split("}")
      .find((b) => b.includes(".viewer-body {") && b.includes("unicode-bidi"));
    expect(bloque, "el cuerpo del visor sin regla bidi").toBeDefined();
    expect(bloque).toContain("bidi-override");
    expect(bloque).toContain("direction: ltr");
  });

  it("un enlace de la prosa se pulsa y activa SU fila, sin llevar la clave", () => {
    const { screen, enviadas } = montar();
    const v = conAyuda();
    if (v.help !== null) {
      v.help.blocks = [
        { block: "paragraph", spans: [{ span: "link", text: "Marcar", action: 1 }] },
      ];
    }
    screen.paint(v);
    const enlace = document.querySelector(".help-link") as HTMLElement;
    expect(enlace.getAttribute("role")).toBe("link");
    // Viaja el índice de la fila, nunca el id del destino.
    expect(enlace.getAttribute("data-topic")).toBeNull();
    enlace.click();
    expect(enviadas).toEqual([{ action: "help_activate", index: 1 }]);
  });

  it("un enlace sin fila sigue siendo texto", () => {
    const { screen, enviadas } = montar();
    const v = conAyuda();
    if (v.help !== null) {
      v.help.blocks = [
        { block: "paragraph", spans: [{ span: "link", text: "Marcar", action: null }] },
      ];
    }
    screen.paint(v);
    const enlace = document.querySelector(".help-link") as HTMLElement;
    expect(enlace.getAttribute("role")).toBeNull();
    enlace.click();
    expect(enviadas).toEqual([]);
  });

  it("las teclas de desplazar mueven el CUERPO sin depender del foco del DOM", () => {
    // El cuerpo se reconstruye en cada parche y nadie le devolvía el foco del
    // documento, así que el scroll nativo de `PgDn` no hacía nada hasta un
    // clic dentro. Ahora las consume el renderer.
    const { screen } = montar();
    screen.paint(conAyuda());
    const cuerpo = document.querySelector(".help-body") as HTMLElement;
    for (const tecla of ["PageDown", "PageUp", "Home", "End"]) {
      expect(screen.desplazarAyuda(tecla), tecla).toBe(true);
    }
    cuerpo.scrollTop = 50;
    screen.desplazarAyuda("Home");
    expect(cuerpo.scrollTop).toBe(0);
    expect(screen.desplazarAyuda("x"), "una letra no es suya").toBe(false);
  });

  it("las flechas desplazan solo una página SIN acciones", () => {
    // Con acciones, las flechas eligen una y son del host; sin ellas (la hoja
    // de teclado) son la única forma de leer línea a línea.
    const { screen } = montar();
    const v = conAyuda();
    screen.paint(v);
    const conFilas = document.querySelector(".help-actions") !== null;
    expect(screen.desplazarAyuda("ArrowDown")).toBe(!conFilas);
    if (v.help !== null) {
      v.help.actions = [];
    }
    screen.paint(v);
    expect(screen.desplazarAyuda("ArrowDown")).toBe(true);
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
    };
    return v;
  }

  it("es modal, no avisa de nada y numera solo las filas elegibles", () => {
    const { screen } = montar();
    screen.paint(conAjustes());
    const caja = document.querySelector(".settings") as HTMLElement;
    expect(caja.getAttribute("aria-modal")).toBe("true");
    // La nota de «no escribe» se fue con el puente 60: esta ventana escribe.
    expect(caja.querySelector(".settings-note")).toBeNull();
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

  it("un doble click ACTIVA esa fila, que es lo que hace enter", () => {
    const { screen, enviadas } = montar();
    screen.paint(conAjustes());
    const filas = [...document.querySelectorAll(".settings-row")];
    (filas[0] as HTMLElement).dispatchEvent(
      new MouseEvent("dblclick", { bubbles: true }),
    );
    expect(enviadas).toEqual([{ action: "settings_activate", row: 0 }]);
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

  it("la ficha dice quién está elegida y cuenta las instaladas y las encendidas", () => {
    const { screen } = montar();
    screen.paint(conExtensiones());
    expect(document.querySelector(".extensions-pane-name")?.textContent).toBe(
      "FTP de ACME",
    );
    // Dos cuentas: «2 instaladas · 1 encendidas», sin marcadores Fluent.
    const resumen = document.querySelector(".extensions-summary")?.textContent ?? "";
    expect(resumen).toContain("2 ");
    expect(resumen).toContain("1 ");
    expect(resumen).not.toContain("$");
    // Sin ficha pedida, se dice cómo pedirla en vez de dejar el hueco.
    expect(document.querySelector(".extensions-detail-hint")).not.toBeNull();
  });

  it("los botones dicen lo que van a hacer y mandan la acción de la fila elegida", () => {
    const { screen, enviadas } = montar();
    screen.paint(conExtensiones());
    const acciones = document.querySelector(".extensions-actions") as HTMLElement;
    // La elegida está aprobada y encendida: revocar, apagar, ayuda, y
    // desinstalar; nunca «alternar».
    const etiquetas = [...acciones.querySelectorAll("button")].map((b) => b.textContent);
    expect(etiquetas).toEqual([
      catalogoReal()["ext-revoke"],
      catalogoReal()["ext-disable"],
      catalogoReal()["ext-help"],
      catalogoReal()["ext-uninstall"],
    ]);
    (acciones.querySelector(".extensions-action-uninstall") as HTMLButtonElement).click();
    (acciones.querySelector(".extensions-action-enabled") as HTMLButtonElement).click();
    (acciones.querySelector(".extensions-action-help") as HTMLButtonElement).click();
    expect(enviadas).toEqual([
      { action: "extension_govern", row: 0, id: "acme.ftp", change: "uninstall" },
      { action: "extension_govern", row: 0, id: "acme.ftp", change: "enabled" },
      { action: "extension_help", row: 0, id: "acme.ftp" },
    ]);
    // Desinstalar se pinta como lo que es, y el clic sobre un botón no
    // vuelve a seleccionar la fila.
    expect(
      acciones
        .querySelector(".extensions-action-uninstall")
        ?.getAttribute("data-destructive"),
    ).toBe("true");
    expect(enviadas.some((a) => a.action === "extension_select_row")).toBe(false);
  });

  it("sobre una sin aprobar, aprobar es el botón principal y encender no se ofrece", () => {
    const { screen, enviadas } = montar();
    const v = conExtensiones();
    if (v.extensions !== null) {
      v.extensions.cursor = 1;
    }
    screen.paint(v);
    const acciones = document.querySelector(".extensions-actions") as HTMLElement;
    const aprobar = acciones.querySelector(
      ".extensions-action-approval",
    ) as HTMLButtonElement;
    expect(aprobar.textContent).toBe(catalogoReal()["ext-approve"]);
    expect(aprobar.getAttribute("data-primary")).toBe("true");
    const encender = acciones.querySelector(
      ".extensions-action-enabled",
    ) as HTMLButtonElement;
    expect(encender.disabled).toBe(true);
    // Y dice por qué, con la frase que el host contestaría.
    expect(encender.title).toBe(catalogoReal()["host-extension-not-approved"]);
    // Sin página de ayuda, sin botón de ayuda.
    expect(acciones.querySelector(".extensions-action-help")).toBeNull();
    aprobar.click();
    expect(enviadas).toEqual([
      { action: "extension_govern", row: 1, id: "org.norte.demo", change: "approval" },
    ]);
  });

  it("cerrar manda la misma tecla que cierra", () => {
    const { screen, enviadas } = montar();
    screen.paint(conExtensiones());
    (document.querySelector(".extensions-close") as HTMLButtonElement).click();
    expect(enviadas).toHaveLength(1);
    expect(enviadas[0]).toMatchObject({ action: "key", key: "Escape" });
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

describe("el selector de perfiles", () => {
  function conPerfiles() {
    const v = vista({});
    v.profiles = {
      rows: [
        {
          name: "fotos",
          name_hostile: false,
          title: "Fotos",
          active: true,
          clash: "",
          no_state: false,
          problem: "",
        },
        {
          name: "far",
          name_hostile: false,
          title: null,
          active: false,
          clash: "también es un preset de teclado",
          no_state: false,
          problem: "",
        },
        {
          name: "roto",
          name_hostile: false,
          title: null,
          active: false,
          clash: "",
          no_state: false,
          problem: "línea 3: falta `]`",
        },
      ],
      cursor: 1,
      generation: 7,
    };
    return v;
  }

  it("marca el activo, el cursor, y ENSEÑA el que no carga", () => {
    const { screen } = montar();
    screen.paint(conPerfiles());
    const filas = [...document.querySelectorAll(".profiles-row")];
    expect(filas).toHaveLength(3);
    expect(filas[0]?.getAttribute("data-active")).toBe("true");
    expect(filas[1]?.getAttribute("aria-selected")).toBe("true");
    // Una fila rota no desaparece: se enseña con su motivo.
    expect(filas[2]?.getAttribute("data-broken")).toBe("true");
    expect(filas[2]?.textContent).toContain("falta");
    // Y el choque de nombre se dice, que si no es una trampa.
    expect(filas[1]?.textContent).toContain("preset de teclado");
  });

  it("un click lo activa, con la generación con la que se pintó", () => {
    const { screen, enviadas } = montar();
    screen.paint(conPerfiles());
    (document.querySelectorAll(".profiles-row")[1] as HTMLElement).click();
    expect(enviadas).toEqual([{ action: "profile_activate_row", row: 1, generation: 7 }]);
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
      choices: ["default", "retro"],
      cursor: 1,
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
      choices: ["default"],
      cursor: 0,
    };
    screen.paint(v);
    expect(document.querySelector(".theme-effects")).toBeNull();
  });

  it("la lista de temas marca el que está bajo el cursor", () => {
    const { screen } = montar();
    const v = vista({});
    v.theme = {
      name: "retro",
      roles: [{ role: "fg", color: "#d4d8de" }],
      unsupported_effects: [],
      choices: ["default", "retro", "nord"],
      cursor: 1,
    };
    screen.paint(v);
    const lista = document.querySelector(".theme-choices") as HTMLElement;
    expect(lista.getAttribute("aria-activedescendant")).toBe("theme-choice-1");
    const marcada = document.querySelectorAll('.theme-choice[aria-selected="true"]');
    expect(marcada).toHaveLength(1);
    expect(marcada[0]?.textContent).toBe("retro");
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
        follows_display: "⟨file⟩/home/oscar/Downloads",
        follows_hostile: false,
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

  // «Detalles» a secas no dice de QUÉ son los detalles: con dos listados
  // abiertos, la única forma de saber cuál se estaba describiendo era mover
  // el cursor y ver si la hoja se movía.
  it("la hoja titula con el listado al que sigue", () => {
    const { screen } = montar();
    const v = vista({});
    v.slots = [
      ...v.slots,
      {
        kind: "metadata",
        slot_id: 7,
        fields: [{ label: "Nombre", value: "notas.txt", hostile: false }],
        note: "",
        follows_display: "⟨file⟩/home/oscar/Downloads",
        follows_hostile: false,
      },
    ];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 0, y: 0, width: 30, height: 10, role: null, focus_index: 2 },
    ];
    screen.paint(v);
    const titulo = document.querySelector('[data-slot-id="7"] .slot-title');
    expect(titulo?.textContent).toContain("⟨file⟩/home/oscar/Downloads");
    // La ruta va en SU nodo: `.slot-title` recorta sin puntos suspensivos, y
    // una ruta cortada en seco nombra otro directorio que además existe.
    expect(
      document.querySelector('[data-slot-id="7"] .slot-title .title-path')?.textContent,
    ).toBe("⟨file⟩/home/oscar/Downloads");
  });

  // #291: el visor acoplado es el MISMO cuerpo que el grande, en un hueco.
  it("el visor acoplado pinta las líneas del fichero bajo el cursor con su ruta", () => {
    const { screen } = montar();
    const v = vista({});
    v.slots = [
      ...v.slots,
      {
        kind: "preview",
        slot_id: 7,
        note: "",
        viewer: {
          path_display: "⟨file⟩/casa/notas.md",
          path_hostile: false,
          encoding: "UTF-8",
          eol: "lf",
          hex: false,
          forced: false,
          had_errors: false,
          truncated: false,
          total_rows: 2,
          first_line: 0,
          total_cols: 0,
          first_col: 0,
          lines: ["Título", "texto"],
          preview_by: "via Markdown",
          preview_lossy: false,
          image: null,
          image_refused: "",
          styled: [
            [{ text: "Título", role: "title", fg: null, bg: null }],
            [{ text: "texto", role: null, fg: "#ff0000", bg: "#000040" }],
          ],
        },
      },
    ];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 60, y: 0, width: 60, height: 38, role: null, focus_index: 2 },
    ];
    screen.paint(v);
    const hueco = document.querySelector(".preview") as HTMLElement;
    expect(hueco.querySelector(".viewer-via")?.textContent).toBe("via Markdown");
    const lineas = [...hueco.querySelectorAll(".viewer-line")];
    expect(lineas).toHaveLength(2);
    expect(lineas[0]?.querySelector(".viewer-span")?.getAttribute("data-role")).toBe(
      "title",
    );
    const segundo = lineas[1]?.querySelector(".viewer-span") as HTMLElement;
    expect(segundo.style.color).toBe("rgb(255, 0, 0)");
    expect(segundo.style.backgroundColor).toBe("rgb(0, 0, 64)");
    // Y el visor GRANDE no se abrió: es un hueco, no un overlay.
    expect(document.querySelector('[role="document"]')).toBeNull();
  });

  it("la rueda sobre el visor acoplado desplaza por el HOST", () => {
    const { screen, enviadas } = montar();
    const v = vista({});
    v.slots = [
      ...v.slots,
      {
        kind: "preview",
        slot_id: 7,
        note: "",
        viewer: {
          path_display: "⟨file⟩/casa/notas.txt",
          path_hostile: false,
          encoding: "UTF-8",
          eol: "lf",
          hex: false,
          forced: false,
          had_errors: false,
          truncated: false,
          total_rows: 200,
          first_line: 0,
          total_cols: 0,
          first_col: 0,
          lines: ["una", "dos"],
          preview_by: "",
          preview_lossy: false,
          image: null,
          image_refused: "",
          styled: [],
        },
      },
    ];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 60, y: 0, width: 60, height: 38, role: null, focus_index: 2 },
    ];
    screen.paint(v);
    const caja = document.querySelector(".preview") as HTMLElement;
    caja.dispatchEvent(new WheelEvent("wheel", { deltaY: 120, bubbles: true }));
    expect(enviadas.at(-1)).toEqual({ action: "preview_scroll", slot_id: 7, delta: 3 });
  });

  it("el visor acoplado sin fichero DICE por qué", () => {
    const { screen } = montar();
    const v = vista({});
    v.slots = [
      ...v.slots,
      { kind: "preview", slot_id: 7, note: "directorio", viewer: null },
    ];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 60, y: 0, width: 60, height: 38, role: null, focus_index: 2 },
    ];
    screen.paint(v);
    expect(document.querySelector(".preview .slot-note")?.textContent).toBe("directorio");
    expect(document.querySelector(".preview .viewer-body")).toBeNull();
  });

  it("la hoja sin nada bajo el cursor lo DICE", () => {
    const { screen } = montar();
    const v = vista({});
    v.slots = [
      ...v.slots,
      {
        kind: "metadata",
        slot_id: 7,
        fields: [],
        note: "nada bajo el cursor",
        follows_display: "⟨file⟩/home/oscar",
        follows_hostile: false,
      },
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
        rate: "",
        eta: "",
        detail: "a.txt",
        detail_hostile: false,
        foreign: false,
      },
      {
        task_id: 2,
        kind: "delete",
        state: "running",
        percent: 10,
        rate: "",
        eta: "",
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

describe("el panel de un plugin", () => {
  function conPanel(
    lines: PanelSlotView["lines"],
    hits: PanelSlotView["hits"],
  ): ViewSnapshot {
    const v = vista({});
    v.slots = [...v.slots, { kind: "panel", slot_id: 7, title: "status", lines, hits }];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 0, y: 0, width: 40, height: 10, role: null, focus_index: 2 },
    ];
    return v;
  }

  it("pinta lo que el guest describió, con el título del panel", () => {
    const { screen } = montar();
    screen.paint(conPanel([[{ text: "rama main", role: null, fg: null, bg: null }]], []));
    expect(document.querySelector(".panel-line")?.textContent).toBe("rama main");
  });

  // Un marco que todavía no llegó —la primera petición en vuelo, o un plugin
  // que falló— deja el hueco con su borde y su título: se sabe que el panel
  // está y de quién es, en vez de un hueco mudo.
  it("sin marco todavía, el hueco sigue siendo suyo", () => {
    const { screen } = montar();
    screen.paint(conPanel([], []));
    expect(document.querySelectorAll(".panel-line")).toHaveLength(0);
    expect(document.querySelector(".panel-plugin")).not.toBeNull();
  });

  // Lo que viaja es la CELDA. El comando lo resuelve el host contra el marco
  // que él tiene, con el mismo filtro que el terminal: si el comando cruzara
  // el cable, podría mandarlo cualquiera que hable con el renderer.
  it("una zona manda la celda que se pulsó, nunca un comando", () => {
    const { screen, enviadas } = montar();
    screen.paint(
      conPanel(
        [[{ text: "rama main", role: null, fg: null, bg: null }]],
        [{ row: 0, col: 5, width: 4 }],
      ),
    );
    const zona = document.querySelector(".panel-hit") as HTMLElement;
    expect(zona).not.toBeNull();
    zona.click();
    expect(enviadas.at(-1)).toEqual({
      action: "panel_click",
      slot_id: 7,
      row: 0,
      col: 5,
    });
  });
});

describe("el panel de registro", () => {
  function conRegistro(extra: Partial<LogSlotView> = {}): ViewSnapshot {
    const v = vista({});
    v.slots = [
      ...v.slots,
      {
        kind: "log",
        slot_id: 7,
        lines: [
          {
            time: "12:00:00",
            level: "error",
            target: "norte_core::connect",
            message: "no se pudo conectar",
            hostile: false,
            source: "daemon",
          },
          {
            time: "12:00:01",
            level: "info",
            target: "norte_core",
            message: "listado",
            hostile: false,
            source: "window",
          },
        ],
        level: "info",
        filter: "",
        following: true,
        total: 2,
        first_visible: 0,
        dropped_note: "",
        capturing: "",
        source: "de esta ventana",
        source_mode: "window",
        sources_available: false,
        source_note: "",
        ...extra,
      },
    ];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 0, y: 0, width: 80, height: 12, role: null, focus_index: 2 },
    ];
    return v;
  }

  it("pinta las líneas y colorea por NIVEL, no por posición", () => {
    const { screen } = montar();
    screen.paint(conRegistro());
    const filas = [...document.querySelectorAll(".log-line")];
    expect(filas).toHaveLength(2);
    // El nivel colorea la línea entera: leer un registro es buscar los
    // errores, y un color solo en la etiqueta no se ve de un vistazo.
    expect((filas[0] as HTMLElement).dataset["level"]).toBe("error");
    expect((filas[1] as HTMLElement).dataset["level"]).toBe("info");
    expect(filas[0]?.textContent).toContain("no se pudo conectar");
  });

  it("el nivel se PINTA con la etiqueta y se COMPARA con la identidad", () => {
    const { screen } = montar();
    const v = conRegistro();
    const log = v.slots.find((s) => s.kind === "log");
    if (log?.kind !== "log") {
      throw new Error("la fixture trae el panel de registro");
    }
    log.level_label = "INFO";
    log.lines[0]!.level_label = "ERROR";
    log.lines[1]!.level_label = "INFO";
    screen.paint(v);

    // La ventana pintaba `error` en la línea, `info` en el chip del título y
    // «info» traducido en los botones: tres vocabularios del mismo nivel, los
    // tres a la vez en pantalla. Lo que se lee es la etiqueta, que es la que
    // pinta el terminal y la que se escribe en `RUST_LOG`.
    const etiquetas = [...document.querySelectorAll(".log-level")].map(
      (n) => n.textContent,
    );
    expect(etiquetas).toEqual(["ERROR", "INFO"]);
    // Y la IDENTIDAD sigue siendo la de cable: es con la que se colorea y con
    // la que se marca qué botón está puesto, y traducirla rompería las dos.
    const filas = [...document.querySelectorAll(".log-line")];
    expect((filas[0] as HTMLElement).dataset["level"]).toBe("error");
  });

  it("dice de qué PROCESO son las líneas", () => {
    // La ventana arranca su propio daemon, así que aquí NO está lo del
    // daemon. Callarlo haría que el panel pareciera roto: alguien lo abre
    // mientras una conexión falla y no encuentra la línea que lo explica.
    const { screen } = montar();
    screen.paint(conRegistro());
    expect(document.body.textContent).toContain("de esta ventana");
  });

  it("dice cuando está DESPEGADO del final y cuando tiró líneas", () => {
    // «No pasa nada» y «te has despegado y esto es historia» son
    // indistinguibles sin decirlo; y un registro con un agujero silencioso
    // miente sobre lo que pasó.
    const { screen } = montar();
    screen.paint(
      conRegistro({ following: false, dropped_note: "17 líneas viejas descartadas" }),
    );
    // La cabecera DEL hueco de registro: hay varias en la pantalla, y la
    // primera es la del listado.
    const caja = document.querySelector(".log");
    const cabecera = caja?.parentElement?.querySelector(".slot-title")?.textContent ?? "";
    expect(cabecera).toContain(catalogoReal()["log-detached"] ?? "");
    expect(cabecera).toContain("17 líneas viejas descartadas");
  });

  it("el nivel PUESTO se marca, y pulsar otro lo pide por su id de wire", () => {
    const { screen, enviadas } = montar();
    screen.paint(conRegistro());
    const botones = [...document.querySelectorAll(".log-controls button")];
    const puesto = botones.find((b) => (b as HTMLElement).dataset["on"] === "true");
    expect(puesto?.textContent).toBe(catalogoReal()["log-level-info"] ?? "");

    const debug = botones.find(
      (b) => b.textContent === (catalogoReal()["log-level-debug"] ?? ""),
    );
    (debug as HTMLButtonElement).click();
    const ultima = enviadas.at(-1);
    expect(ultima?.action).toBe("log_set_level");
    if (ultima?.action === "log_set_level") {
      // El identificador de WIRE, no la etiqueta traducida: comparar frases
      // traducidas ataría el nivel al idioma.
      expect(ultima.level).toBe("debug");
    }
  });

  it("sin una segunda fuente NO hay selector que pulsar", () => {
    // Un mando entre tres vistas de un mismo anillo promete algo que no
    // existe: el daemon de esta ventana puede no servir su registro, y
    // entonces la fuente es una etiqueta y no un botón.
    const { screen } = montar();
    screen.paint(conRegistro());
    expect(document.querySelector(".log-source")).toBeNull();
    // Pero se sigue DICIENDO de dónde son las líneas: eso no era opcional.
    expect(document.body.textContent).toContain("de esta ventana");
  });

  it("con daemon el selector recorre las tres fuentes de una pulsación", () => {
    const { screen, enviadas } = montar();
    screen.paint(
      conRegistro({
        sources_available: true,
        source_mode: "both",
        source: "de la ventana y del daemon",
      }),
    );
    const selector = document.querySelector(".log-source");
    expect(selector).not.toBeNull();
    // El identificador de WIRE, no la frase traducida: comparar frases ataría
    // la prueba al idioma.
    expect((selector as HTMLElement).dataset["source"]).toBe("both");
    (selector as HTMLButtonElement).click();
    expect(enviadas.at(-1)?.action).toBe("log_cycle_source");
  });

  it("cada línea dice de qué proceso salió", () => {
    // En una lista mezclada es la mitad de la información: «el provider falló»
    // y «la ventana no pudo pintarlo» se leen igual sin saber quién lo
    // escribió, y son dos averías distintas.
    const { screen } = montar();
    screen.paint(conRegistro({ sources_available: true, source_mode: "both" }));
    const filas = [...document.querySelectorAll(".log-line")];
    expect((filas[0] as HTMLElement).dataset["source"]).toBe("daemon");
    expect((filas[1] as HTMLElement).dataset["source"]).toBe("window");
  });

  it("dice cuando el daemon NO sirve su registro", () => {
    // La mitad de #326 aplicada a la otra orilla: el panel vuelve al anillo
    // local y lo dice, en vez de quedarse mudo y parecer roto.
    const { screen } = montar();
    const nota = catalogoReal()["log-source-unsupported"] ?? "";
    // La clave TIENE que existir: sin esto, borrarla del catálogo dejaría el
    // `toContain("")` de abajo pasando siempre y la prueba diría que sí a nada.
    expect(nota).not.toBe("");
    screen.paint(conRegistro({ source_note: nota }));
    const caja = document.querySelector(".log");
    const cabecera = caja?.parentElement?.querySelector(".slot-title")?.textContent ?? "";
    expect(cabecera).toContain(nota);
  });

  it("la rueda desplaza por el HOST, no por el DOM", () => {
    // La ventana visible la decide el host: dejar que el navegador desplace un
    // trozo que solo tiene las líneas visibles no llegaría a ninguna parte.
    const { screen, enviadas } = montar();
    screen.paint(conRegistro());
    const caja = document.querySelector(".log") as HTMLElement;
    caja.dispatchEvent(new WheelEvent("wheel", { deltaY: 120, bubbles: true }));
    const ultima = enviadas.at(-1);
    expect(ultima?.action).toBe("log_scroll");
    if (ultima?.action === "log_scroll") {
      expect(ultima.delta).toBeGreaterThan(0);
    }
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
        input_secret: false,
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
        rate: "",
        eta: "",
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

describe("la revisión de un plan de organizar", () => {
  /** Un árbol con las tres clases de línea y un nombre alterado. */
  function arbol(): NonNullable<ViewSnapshot["organize"]> {
    return {
      dir: { text: "⟨mem⟩/casa/descargas", hostile: false },
      lines: [
        {
          depth: 0,
          text: { text: "facturas", hostile: false },
          kind: "existing_dir",
        },
        { depth: 1, text: { text: "2026", hostile: false }, kind: "new_dir" },
        {
          depth: 2,
          text: { text: "caf�.pdf", hostile: true },
          kind: "moved",
        },
      ],
      first_visible: 0,
      total: 12,
      more_note: "… 3/12 (desplazar: ↓/↑)",
      hidden_hostile: true,
      summary: "crea 1 carpetas y mueve 2 ficheros",
      seen_all: false,
    };
  }

  it("marca cada clase de línea de dos formas y sangra con un dato, no con texto", () => {
    const { screen } = montar();
    const v = vista({});
    v.organize = arbol();
    screen.paint(v);
    const caja = document.querySelector(".organize") as HTMLElement;
    expect(caja).not.toBeNull();
    expect(caja.getAttribute("aria-modal")).toBe("true");
    // El recuento va ANTES del árbol: es lo que se lee para decidir.
    const cuerpo = Array.from(caja.children).map((e) => e.className);
    expect(cuerpo.indexOf("organize-summary")).toBeLessThan(
      cuerpo.indexOf("organize-tree"),
    );

    const filas = Array.from(caja.querySelectorAll<HTMLElement>(".organize-line"));
    expect(filas).toHaveLength(3);
    // La clase dice qué es, Y el marcador lo dice otra vez: el color no
    // sobrevive a un tema monocromo.
    expect(filas[0]?.classList.contains("organize-existing-dir")).toBe(true);
    expect(filas[1]?.classList.contains("organize-new-dir")).toBe(true);
    expect(filas[0]?.querySelector(".organize-mark")?.textContent).toBe("·");
    expect(filas[1]?.querySelector(".organize-mark")?.textContent).toBe("+");
    expect(filas[2]?.querySelector(".organize-mark")?.textContent).toBe("→");
    // El sangrado es una variable del estilo, no espacios en el nombre: un
    // nombre que empiece por espacios no puede fingir estar más adentro.
    expect(filas[2]?.style.getPropertyValue("--depth")).toBe("2");
    const nombre = filas[2]?.querySelector(".organize-name") as HTMLElement;
    expect(nombre.textContent?.startsWith(" ")).toBe(false);
    expect(nombre.dataset["hostile"]).toBe("true");
    expect(nombre.textContent).toContain("nombre alterado");
  });

  it("no deja aprobar hasta haberlo leído entero", () => {
    const { screen } = montar();
    const v = vista({});
    v.organize = arbol();
    screen.paint(v);
    const botones = Array.from(
      document.querySelectorAll<HTMLButtonElement>(".organize .choices button"),
    );
    expect(botones[0]?.disabled).toBe(true);
    // Descartar SIEMPRE se puede: quien no quiere esto tiene que poder
    // quitárselo de encima.
    expect(botones[1]?.disabled).toBe(false);

    v.organize = { ...arbol(), seen_all: true };
    screen.paint(v);
    const despues = document.querySelector(
      ".organize .choices button",
    ) as HTMLButtonElement;
    expect(despues.disabled).toBe(false);
  });

  it("sin plan no queda nada pintado", () => {
    const { screen } = montar();
    const v = vista({});
    v.organize = arbol();
    screen.paint(v);
    expect(document.querySelector(".organize")).not.toBeNull();
    v.organize = null;
    screen.paint(v);
    expect(document.querySelector(".organize")).toBeNull();
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
