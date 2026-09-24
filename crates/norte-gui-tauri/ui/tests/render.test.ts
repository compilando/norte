// Painting: virtualization, listing states, accessibility and gestures.
//
// All with a FAKE bridge. No window nor WebKitGTK is needed to check what
// this renderer promises.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Screen } from "../src/render";
import { MARK_RULER_COLOR, OVERSCAN, markRulerImage } from "../src/render/dom";
import { zoneOf } from "../src/render/move";
import { revealTarget } from "../src/render/settings";
import { realCatalog } from "./fixtures";
import { BRIDGE_VERSION } from "../src/types";
import type {
  BrowserSlotView,
  HostCatalog,
  LogSlotView,
  PanelSlotView,
  RowView,
  UiAction,
  ViewSnapshot,
  WindowVerb,
} from "../src/types";

const CELL_H = 20;

function catalog(): HostCatalog {
  return {
    // From the constant, NEVER a literal: this one said 5 for three bumps
    // without anyone noticing, which is the same kind of staleness the rest
    // of this file exists against (#259).
    bridge_version: BRIDGE_VERSION,
    instance_id: "host-1",
    locale: "es",
    // The REAL catalogue, not two made-up keys: with an invented fixture, a
    // missing key paints the same as one that exists.
    strings: realCatalog(),
    theme: {},
    measure: false,
  };
}

function row(key: number, name: string, extra: Partial<RowView> = {}): RowView {
  return {
    key,
    display_name: name,
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

function view(browser: Partial<BrowserSlotView>): ViewSnapshot {
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
        rows: [row(0, "a.txt"), row(1, "b.txt")],
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

describe("go to anywhere (#357)", () => {
  it("paints headers that aren't selectable and rows with the cursor and their badge", () => {
    const { screen } = mount();
    const v = view({});
    v.goto = {
      query: "doc",
      lines: [
        { line: "header", title: "Historia" },
        { line: "row", text: "/casa/docs", desc: "", hostile: false },
        { line: "row", text: "caf�", desc: "/srv", hostile: true },
      ],
      cursor: 2,
      empty: "nada casa con eso",
    };
    screen.paint(v);
    const header = document.querySelector(".goto-header");
    expect(header?.textContent).toBe("Historia");
    // A header is not an option: the screen reader doesn't offer it.
    expect(header?.getAttribute("role")).toBe("presentation");
    const rows = document.querySelectorAll(".goto .palette-row");
    expect(rows.length).toBe(2);
    expect(rows[1]?.getAttribute("aria-selected")).toBe("true");
    expect(rows[1]?.getAttribute("data-hostile")).toBe("true");
  });

  it("with no lines says nothing matches, and closed leaves nothing", () => {
    const { screen } = mount();
    const v = view({});
    v.goto = { query: "zzz", lines: [], cursor: null, empty: "nada casa con eso" };
    screen.paint(v);
    expect(document.querySelector(".goto .empty")?.textContent).toBe("nada casa con eso");
    v.goto = null;
    screen.paint(v);
    expect(document.querySelector(".goto")).toBeNull();
  });
});

describe("the splash screen", () => {
  /** A screen with one section holding one numbered row. */
  function withSplash(closeAfterMs: number | null): ViewSnapshot {
    const v = view({});
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

  it("paints the art, the sections and their numbered rows", () => {
    const { screen } = mount();
    screen.paint(withSplash(null));
    const box = document.querySelector(".splash");
    expect(box).not.toBeNull();
    // The art is NOT read aloud: a compass of bars and dashes spells out as
    // noise.
    expect(document.querySelector(".splash-art")?.getAttribute("aria-hidden")).toBe(
      "true",
    );
    expect(document.querySelector(".splash-section")?.textContent).toBe(
      "A dónde sueles ir",
    );
    expect(document.querySelector(".splash-number")?.textContent).toBe("1");
    expect(document.querySelector(".splash-label")?.textContent).toBe("casa");
  });

  // The COVER: `brief` comes with no sections, and then the splash screen
  // stops being a centered box and takes up the whole slot. The signal is
  // the same one the terminal uses — there are no sections — so both
  // surfaces decide the same way without the mode having to travel over the
  // bridge.
  it("with no sections it paints as a cover", () => {
    const { screen } = mount();
    const v = withSplash(null);
    if (v.splash) {
      v.splash.sections = [];
    }
    screen.paint(v);
    const box = document.querySelector(".splash") as HTMLElement;
    expect(box.dataset["cover"]).toBe("true");
  });

  // And with a list it's still a box: the numbered rows are read and
  // pressed, and loose over the background they'd lose the frame that
  // bounds them.
  it("with sections it's still a box", () => {
    const { screen } = mount();
    screen.paint(withSplash(null));
    const box = document.querySelector(".splash") as HTMLElement;
    expect(box.dataset["cover"]).toBeUndefined();
  });

  it("a click anywhere dismisses it", () => {
    const { screen, sent } = mount();
    screen.paint(withSplash(null));
    const root =
      document.getElementById("splash") ??
      document.querySelector(".splash")?.parentElement;
    (root as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(sent.at(-1)).toEqual({ action: "splash_close" });
  });

  it("a click on a numbered row opens it, and it doesn't also count as dismissal", () => {
    const { screen, sent } = mount();
    screen.paint(withSplash(null));
    const row = document.querySelector(".splash-row") as HTMLElement;
    row.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(sent.at(-1)).toEqual({ action: "splash_activate_row", number: 1 });
    // A single action: the row's click doesn't bubble up to the veil, or the
    // host would receive "open it" and "dismiss it" and navigation would be
    // lost.
    expect(sent).toHaveLength(1);
  });

  it("the brief mode's timeout dismisses it on its own, with nobody touching anything", () => {
    vi.useFakeTimers();
    try {
      const { screen, sent } = mount();
      screen.paint(withSplash(1200));
      expect(sent).toHaveLength(0);
      vi.advanceTimersByTime(1199);
      expect(sent).toHaveLength(0);
      vi.advanceTimersByTime(1);
      expect(sent.at(-1)).toEqual({ action: "splash_close" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("the timeout does NOT re-arm on every repaint", () => {
    vi.useFakeTimers();
    try {
      const { screen, sent } = mount();
      const v = withSplash(1200);
      screen.paint(v);
      vi.advanceTimersByTime(600);
      // Any other patch: the host sends the WHOLE view every time, and
      // during startup they keep arriving nonstop. If every paint re-armed
      // the timeout, "1.2 seconds" would become "1.2 seconds after the last
      // patch", and with a task running the screen would never leave.
      screen.paint(JSON.parse(JSON.stringify(v)) as ViewSnapshot);
      vi.advanceTimersByTime(600);
      expect(sent.at(-1)).toEqual({ action: "splash_close" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("on dismissal, a pending timeout is disarmed", () => {
    vi.useFakeTimers();
    try {
      const { screen, sent } = mount();
      screen.paint(withSplash(1200));
      // The host already dismissed it (a key): a timer left alive would send
      // a close for a screen that's no longer there.
      screen.paint(view({}));
      vi.advanceTimersByTime(5000);
      expect(sent).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("a panel without the keyboard dims", () => {
  // The dimming is CSS and hangs off `.scroller`: only a listing keeps that
  // class, and the side panels replace it with their own. They can't be
  // told apart by role — a listing with nothing to mark as a target is left
  // without a role, same as a side panel — so this invariant is the only
  // thing stopping the log or the processes panel itself from painting
  // dimmed while they hold the keyboard.
  it("a side panel doesn't keep the listing's class", () => {
    const { screen, root } = mount();
    const v = view({});
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

describe("repainting without changes (the flicker while scrolling)", () => {
  // Every response to a scroll carries the WHOLE view. Rebuilding the nodes
  // that didn't change showed up as a subtle flicker in WebKitGTK: whatever
  // paints the same has to stay being the SAME node.
  it("the same snapshot keeps the bars, title, header and rows", () => {
    const { screen, root } = mount();
    const v = view({});
    screen.paint(v);
    const before = {
      column: root.querySelector(".slot-columns .col"),
      path: root.querySelector(".title-path"),
      cell: root.querySelector(".row .cell-name"),
      panels: document.querySelector(".panelbar"),
      menu: document.querySelector(".menubar"),
    };
    expect(before.column).not.toBeNull();
    expect(before.cell).not.toBeNull();
    screen.paint(JSON.parse(JSON.stringify(v)) as ViewSnapshot);
    expect(root.querySelector(".slot-columns .col")).toBe(before.column);
    expect(root.querySelector(".title-path")).toBe(before.path);
    expect(root.querySelector(".row .cell-name")).toBe(before.cell);
    expect(document.querySelector(".panelbar")).toBe(before.panels);
    expect(document.querySelector(".menubar")).toBe(before.menu);
  });

  it("but what DID change gets repainted", () => {
    const { screen, root } = mount();
    screen.paint(view({}));
    screen.paint(
      view({
        rows: [row(0, "a.txt", { marked: true }), row(1, "c.txt")],
        path_display: "⟨file⟩/otra",
      }),
    );
    const rows = root.querySelectorAll<HTMLElement>(".row");
    expect(rows[0]?.dataset["marked"]).toBe("true");
    expect(root.textContent).toContain("c.txt");
    expect(root.querySelector(".title-path")?.textContent).toContain("otra");
  });

  it("the busy notice stays inside the title even when the title isn't rebuilt", () => {
    const { screen, root } = mount();
    screen.paint(view({}));
    screen.paint(view({ state: { state: "loading" } }));
    const busy = root.querySelector(".slot-busy");
    expect(busy?.parentElement?.classList.contains("slot-title")).toBe(true);
    expect((busy as HTMLElement | null)?.hidden).toBe(false);
  });
});

function mount(
  options: {
    imageBytes?: () => Promise<ArrayBuffer>;
    windowControl?: (verb: WindowVerb) => void;
  } = {},
): {
  screen: Screen;
  sent: UiAction[];
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
  const goto = document.createElement("div");
  document.body.append(
    goto,
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
  const sent: UiAction[] = [];
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
    goto,
    catalog(),
    (a: UiAction) => sent.push(a),
    options.imageBytes ?? (() => Promise.resolve(new ArrayBuffer(0))),
    options.windowControl,
  );
  return { screen, sent, root };
}

describe("Screen", () => {
  beforeEach(() => {
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      cb(0);
      return 0;
    });
  });

  it("places every slot where the host said, in cell pixels", () => {
    const { screen, root } = mount();
    screen.paint(view({}));
    const slots = root.querySelectorAll(".slot");
    expect(slots).toHaveLength(2);
    const first = slots[0] as HTMLElement;
    expect(first.style.width).toBe(`${60 * 8}px`);
    expect(first.dataset["role"]).toBe("active");
  });

  it("the border handles stay ABOVE the slots", () => {
    const { screen, root } = mount();
    const v = view({});
    v.layout.placements = [
      { slot_id: 1, x: 0, y: 0, width: 60, height: 38, role: "active", focus_index: 0 },
      { slot_id: 2, x: 60, y: 0, width: 60, height: 38, role: null, focus_index: 1 },
    ];
    v.slots = [v.slots[0]!, { ...(v.slots[0] as BrowserSlotView), slot_id: 2 }];
    screen.paint(v);

    const handles = [...root.querySelectorAll(".resize-handle")];
    expect(handles.length).toBeGreaterThan(0);

    // This stylesheet uses `z-index` nowhere on purpose: stacking comes from
    // document ORDER. The handles used to be inserted BEFORE the slots, so
    // every panel — also `absolute` — covered them and `pointerdown` never
    // reached them. Meaning you couldn't resize with the mouse.
    const classes = Array.from(root.children).map((n) => n.className);
    const lastSlot = classes.lastIndexOf("slot");
    const firstHandle = classes.findIndex((c) => c.startsWith("resize-handle"));
    expect(lastSlot).toBeGreaterThanOrEqual(0);
    expect(firstHandle).toBeGreaterThan(lastSlot);
  });

  it("marks the target only when the host says it says something", () => {
    const { screen, root } = mount();
    // The THRESHOLD is decided by Rust (`layout::target_worth_marking`) and
    // arrives in `mark_target`: repeating it here would restate in
    // TypeScript a number that already lives in the shared crate, i.e. the
    // same decision in two places. What this test pins down is that the
    // renderer OBEYS the flag and doesn't make up the role from `role`.
    const withFlag = (mark: boolean): ViewSnapshot => {
      const v = view({});
      v.layout.mark_target = mark;
      v.layout.placements = [
        { slot_id: 1, x: 0, y: 0, width: 20, height: 10, role: "active", focus_index: 0 },
        { slot_id: 4, x: 0, y: 0, width: 20, height: 10, role: "target", focus_index: 1 },
      ];
      return v;
    };

    // The role travels the same either way — it's the model — and still
    // doesn't get painted: with two listings the target is "the other one",
    // and a mark that always shows up stops being readable exactly the day
    // there are three and it's actually needed (ADR 0058 D7).
    screen.paint(withFlag(false));
    expect(root.querySelectorAll('[data-role="target"]')).toHaveLength(0);
    expect(root.querySelectorAll('[data-role="active"]')).toHaveLength(1);

    screen.paint(withFlag(true));
    expect(root.querySelectorAll('[data-role="target"]')).toHaveLength(1);
  });

  it("waiting: says the verb, where it's going, and marks an altered path", () => {
    const { screen, root } = mount();
    // A refresh: it's not going anywhere, so no destination is made up.
    screen.paint(view({ state: { state: "loading", verb_key: "busy-listing" } }));
    const notice = root.querySelector(".slot-busy") as HTMLElement;
    expect(notice.hidden).toBe(false);
    expect(notice.querySelector(".slot-busy-target")).toBeNull();

    // Going somewhere, with the painted path different from what it is: it's
    // the one the reader looks at while waiting.
    screen.paint(
      view({
        state: {
          state: "loading",
          verb_key: "busy-connecting",
          target_display: "⟨sftp⟩casa/caf�",
          target_hostile: true,
        },
      }),
    );
    const target = root.querySelector(".slot-busy-target");
    expect(target?.textContent).toBe("⟨sftp⟩casa/caf�");
    expect(root.querySelector(".slot-busy .hostile-badge")).not.toBeNull();

    // With the listing already in place it HIDES, and it's the SAME node:
    // its threshold is an `animation-delay`, and recreating it would reset
    // it on every paint until it never shows up — right in the slow cases.
    const before = root.querySelector(".slot-busy");
    screen.paint(view({}));
    expect((root.querySelector(".slot-busy") as HTMLElement).hidden).toBe(true);
    expect(root.querySelector(".slot-busy")).toBe(before);
  });

  it("stacks in the header everything that says the listing isn't what it looks like", () => {
    const { screen, root } = mount();
    screen.paint(
      view({
        filling_note: "cargando… (3)",
        skipped_note: "⚠ 2 entradas omitidas",
        names_note: "nombres: cp866",
        pruned_note: "1 marca caída",
        hidden_note: "3 ocultas",
        marked_note: "2 marcadas, 4,0 kB",
      }),
    );
    const notes = Array.from(
      root.querySelectorAll(
        ".slot-filling, .slot-skipped, .slot-names, .slot-pruned, .slot-hidden, .slot-marked",
      ),
    );
    expect(notes).toHaveLength(6);
    // Each in ITS OWN node: glued into one, a screen reader reads a single
    // sentence and truncation takes them all together.
    expect(notes.map((n) => n.className)).toEqual([
      "slot-filling",
      "slot-skipped",
      "slot-names",
      "slot-pruned",
      "slot-hidden",
      "slot-marked",
    ]);
    // The ORDER is the decision: WARNINGS before the mark COUNTER. Space
    // runs out, and a truncated warning stops warning while a truncated
    // counter merely stops counting.
    expect(notes[notes.length - 1]?.className).toBe("slot-marked");
  });

  it("doesn't paint a hundred thousand rows to show forty", () => {
    const { screen, root } = mount();
    const rows = Array.from({ length: 40 }, (_, i) => row(1000 + i, `f${String(i)}.txt`));
    screen.paint(view({ total_rows: 100_000, first_visible: 1000, rows }));
    expect(root.querySelectorAll(".row")).toHaveLength(40);
    const canvas = root.querySelector(".canvas") as HTMLElement;
    // The height IS the whole directory's: the scrollbar doesn't lie.
    expect(canvas.style.height).toBe(`${100_000 * CELL_H}px`);
    const first = root.querySelector(".row") as HTMLElement;
    expect(first.style.top).toBe(`${1000 * CELL_H}px`);
    expect(first.getAttribute("aria-rowindex")).toBe("1001");
  });

  it("a hostile name gets MARKED, never hidden", () => {
    const { screen, root } = mount();
    screen.paint(view({ rows: [row(0, "caf�.txt", { hostile: true })] }));
    const name = root.querySelector(".cell-name") as HTMLElement;
    expect(name.classList.contains("hostile")).toBe(true);
    expect(name.textContent).toContain("caf�.txt");
  });

  it("a name with HTML inside is TEXT, not markup", () => {
    const { screen, root } = mount();
    screen.paint(view({ rows: [row(0, "<img src=x onerror=alert(1)>")] }));
    const name = root.querySelector(".cell-name") as HTMLElement;
    expect(name.querySelector("img")).toBeNull();
    expect(name.textContent).toBe("<img src=x onerror=alert(1)>");
  });

  it("the theme colors an entry's name", () => {
    const { screen, root } = mount();
    screen.paint(
      view({
        rows: [row(0, "src", { kind: "dir", name_color: "#4daafc", name_bold: true })],
      }),
    );
    const name = root.querySelector(".cell-name") as HTMLElement;
    // The browser normalizes to rgb(): what's actually computed is compared.
    expect(name.style.color).toBe("rgb(77, 170, 252)");
    expect(name.style.fontWeight).toBe("bold");
  });

  it("under the CURSOR the selection's color wins, not the file's", () => {
    // Mirrors the terminal, where `highlight_style` overrides the item's
    // style when the theme gives `selection` a foreground — and all ten
    // presets do. Without this, a dark blue directory over vscode-dark's
    // #04395e would be illegible in exactly the row being looked at.
    const { screen, root } = mount();
    screen.paint(
      view({
        rows: [
          row(0, "src", {
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
    // Bold IS kept: it says what the entry IS, not what color, and it
    // doesn't compete with the selection's background.
    expect(name.style.fontWeight).toBe("bold");
  });

  it("the theme's attributes come through, not just the color", () => {
    // `retro-crt` dims zip/tar/gz with `dim = true`: carrying only `fg` they
    // came out dim in the terminal and at full brightness here.
    const { screen, root } = mount();
    screen.paint(
      view({
        rows: [
          row(0, "backup.zip", {
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

  it("the icon carries its entry's color (ADR 0105)", () => {
    // An icon says what the row IS, not what state it's in: it follows its
    // name's color, same as the terminal.
    const { screen, root } = mount();
    screen.paint(
      view({
        icon_column: true,
        rows: [row(0, "src", { kind: "dir", icon: "📁", name_color: "#4daafc" })],
      }),
    );
    const icon = root.querySelector(".cell-icon") as HTMLElement;
    expect(icon.style.color).toBe("rgb(77, 170, 252)");
  });

  it("a theme that says nothing about an entry gives it no color", () => {
    const { screen, root } = mount();
    screen.paint(view({ rows: [row(0, "notas.txt")] }));
    const name = root.querySelector(".cell-name") as HTMLElement;
    expect(name.style.color).toBe("");
    expect(name.style.fontWeight).toBe("");
  });

  it("the mark ruler paints with marks and clears without them (ADR 0135)", () => {
    const { screen, root } = mount();
    screen.paint(view({ marks: 3, mark_ruler: [0, 1, 128] }));
    const grid = root.querySelector(".scroller") as HTMLElement;
    expect(grid.dataset["ruler"]).toBe("true");
    const ruler = grid.style.getPropertyValue("--mark-ruler");
    // Stretches 0 and 1 are ONE band; 128 is another, halfway down the
    // listing.
    const c = MARK_RULER_COLOR;
    expect(ruler).toContain(`${c} 0%`);
    expect(ruler).toContain(`${c} 0.7813%`);
    expect(ruler).toContain(`${c} 50%`);
    screen.paint(view({ marks: 0, mark_ruler: [] }));
    expect(grid.dataset["ruler"]).toBeUndefined();
    expect(grid.style.getPropertyValue("--mark-ruler")).toBe("");
  });

  it("markRulerImage: runs within a band, nothing with no stretches", () => {
    expect(markRulerImage([], 256)).toBe("");
    expect(markRulerImage([3], 0)).toBe("");
    const img = markRulerImage([2, 3, 4, 10], 20);
    expect(img.startsWith("linear-gradient(to bottom, transparent 0%")).toBe(true);
    // [2..=4] runs from 10% to 25%; [10] from 50% to 55%.
    const c = MARK_RULER_COLOR;
    expect(img).toContain(`${c} 10%, ${c} 25%`);
    expect(img).toContain(`${c} 50%, ${c} 55%`);
    expect(img.match(/transparent/g)).toHaveLength(5);
  });

  it("an empty listing says so", () => {
    const { screen, root } = mount();
    screen.paint(view({ total_rows: 0, rows: [], cursor: null }));
    expect(root.querySelector(".empty")?.textContent).toBe("vacío");
  });

  it("loading is announced, and a failure is reported with its reason", () => {
    const { screen, root } = mount();
    screen.paint(view({ state: { state: "loading" } }));
    expect(root.querySelector(".scroller")?.getAttribute("aria-busy")).toBe("true");
    screen.paint(
      view({
        state: { state: "error", reason_key: "listing-failed", detail: "EACCES" },
      }),
    );
    const err = root.querySelector(".error") as HTMLElement;
    expect(err.getAttribute("role")).toBe("alert");
    expect(err.textContent).toContain("EACCES");
  });

  it("rows carry roles and state for the screen reader", () => {
    const { screen, root } = mount();
    screen.paint(view({ rows: [row(0, "a.txt", { selected: true }), row(1, "b.txt")] }));
    const grid = root.querySelector(".scroller") as HTMLElement;
    expect(grid.getAttribute("role")).toBe("grid");
    expect(grid.getAttribute("aria-rowcount")).toBe("2");
    const selected = root.querySelector('.row[aria-selected="true"]') as HTMLElement;
    expect(grid.getAttribute("aria-activedescendant")).toBe(selected.id);
  });

  it("the cursor patch, which mutates `selected` in place, gets repainted", () => {
    // A row is skipped by IDENTITY if nothing changed; the cursor is the one
    // thing the session changes without replacing the row, and it can't be
    // lost.
    const { screen, root } = mount();
    const v = view({ rows: [row(0, "a.txt", { selected: true }), row(1, "b.txt")] });
    screen.paint(v);
    const slot = v.slots[0];
    if (slot?.kind !== "browser") {
      throw new Error("expected a listing");
    }
    const [a, b] = slot.rows;
    if (a === undefined || b === undefined) {
      throw new Error("expected two rows");
    }
    a.selected = false;
    b.selected = true;
    slot.cursor = 1;
    screen.paint(v);
    const rows = root.querySelectorAll(".row");
    expect(rows[0]?.getAttribute("aria-selected")).toBe("false");
    expect(rows[1]?.getAttribute("aria-selected")).toBe("true");
  });

  it("a click marks the row; two in a row on the same one open it", () => {
    const { screen, sent, root } = mount();
    screen.paint(view({}));
    const row1 = root.querySelectorAll(".row")[1] as HTMLElement;
    row1.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(sent.at(-1)).toEqual({
      action: "select_row",
      slot_id: 1,
      key: 1,
      generation: 1,
    });
    // The SECOND `mousedown` on the same row: it counts here, without
    // waiting for the engine's `dblclick` event — which is what used to
    // fail.
    row1.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(sent.at(-1)).toEqual({
      action: "activate",
      slot_id: 1,
      key: 1,
      generation: 1,
    });
  });

  it("the mouse's side buttons are back and forward in history", () => {
    // Spec 2026-09-15 D1: the desktop convention. Listened for on `mouseup`
    // and cancelled, so the webview doesn't mistake them for PAGE
    // navigation.
    const { screen, sent, root } = mount();
    screen.paint(view({}));
    const row = root.querySelectorAll(".row")[0] as HTMLElement;
    const back = new MouseEvent("mouseup", {
      bubbles: true,
      cancelable: true,
      button: 3,
    });
    row.dispatchEvent(back);
    expect(sent.at(-1)).toEqual({ action: "history", slot_id: 1, back: true });
    expect(back.defaultPrevented).toBe(true);
    row.dispatchEvent(
      new MouseEvent("mouseup", { bubbles: true, cancelable: true, button: 4 }),
    );
    expect(sent.at(-1)).toEqual({ action: "history", slot_id: 1, back: false });
    // The main button doesn't touch the trail.
    const before = sent.length;
    row.dispatchEvent(
      new MouseEvent("mouseup", { bubbles: true, cancelable: true, button: 0 }),
    );
    expect(sent).toHaveLength(before);
  });

  it("two clicks on DIFFERENT rows open nothing, and neither does a burst's third", () => {
    const { screen, sent, root } = mount();
    screen.paint(view({ rows: [row(0, "a.txt"), row(1, "b.txt")] }));
    const rows = root.querySelectorAll(".row");
    (rows[0] as HTMLElement).dispatchEvent(
      new MouseEvent("mousedown", { bubbles: true }),
    );
    (rows[1] as HTMLElement).dispatchEvent(
      new MouseEvent("mousedown", { bubbles: true }),
    );
    expect(sent.some((a) => a.action === "activate")).toBe(false);
    // Two on the same one: opens ONCE.
    (rows[1] as HTMLElement).dispatchEvent(
      new MouseEvent("mousedown", { bubbles: true }),
    );
    (rows[1] as HTMLElement).dispatchEvent(
      new MouseEvent("mousedown", { bubbles: true }),
    );
    expect(sent.filter((a) => a.action === "activate")).toHaveLength(1);
  });

  it("two clicks apart in time are two clicks, not a double", () => {
    const { screen, sent, root } = mount();
    screen.paint(view({}));
    const row1 = root.querySelectorAll(".row")[1] as HTMLElement;
    const clock = vi.spyOn(Date, "now");
    clock.mockReturnValue(1_000);
    row1.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    clock.mockReturnValue(1_000 + 900);
    row1.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    clock.mockRestore();
    expect(sent.some((a) => a.action === "activate")).toBe(false);
  });

  it("shift+click sends ONE range: who's in it is the host's call", () => {
    const { screen, sent, root } = mount();
    screen.paint(
      view({ rows: [row(0, "a", { selected: true }), row(1, "b"), row(2, "c")] }),
    );
    const third = root.querySelectorAll(".row")[2] as HTMLElement;
    third.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, shiftKey: true }));
    expect(sent.at(-1)).toEqual({
      action: "mark_range",
      slot_id: 1,
      from: 0,
      to: 2,
      generation: 1,
    });
  });

  it("ctrl+click marks just one", () => {
    const { screen, sent, root } = mount();
    screen.paint(view({}));
    const row0 = root.querySelector(".row") as HTMLElement;
    row0.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, ctrlKey: true }));
    expect(sent.at(-1)).toEqual({
      action: "toggle_mark",
      slot_id: 1,
      key: 0,
      generation: 1,
    });
  });

  it("scrolling doesn't cross a threshold: it crosses WHICH rows are needed", () => {
    const { screen, sent, root } = mount();
    screen.paint(view({ total_rows: 10_000 }));
    const scroller = root.querySelector(".scroller") as HTMLElement;
    Object.defineProperty(scroller, "scrollTop", { value: 400, configurable: true });
    Object.defineProperty(scroller, "clientHeight", { value: 200, configurable: true });
    scroller.dispatchEvent(new Event("scroll"));
    const last = sent.at(-1);
    expect(last?.action).toBe("set_visible_range");
    if (last?.action === "set_visible_range") {
      // 400/20 = row 20 at the top and 200/20 = 10 visible, plus the margin
      // on each side. From the constant: with the 8 written here, raising
      // the margin broke the test without the behavior changing.
      expect(last.first).toBe(Math.max(0, 20 - OVERSCAN));
      expect(last.count).toBe(10 + OVERSCAN * 2);
    }
  });

  it("the status bar is announced without stealing focus", () => {
    const { screen, root } = mount();
    screen.paint(view({}));
    const status = root.querySelectorAll(".slot")[1]?.querySelector(".statusbar");
    expect(status?.getAttribute("aria-live")).toBe("polite");
    expect(status?.textContent).toContain("2 entradas");
  });

  it("a command the host rejects at the boundary shows up in the status bar", () => {
    // An action that fails to deserialize dies in `dispatch`, before the
    // host ever sees it: nobody but the renderer can say so. That's how the
    // whole panel bar was, with the error only in the console.
    const { screen, root } = mount();
    screen.paint(view({}));
    const bar = () => root.querySelectorAll(".slot")[1]?.querySelector(".statusbar");
    screen.rejected(
      { action: "panel_bar_activate", button: 1 },
      new Error("unknown variant"),
    );
    expect(bar()?.textContent).toContain("panel_bar_activate");
    expect(bar()?.querySelector(".banner.rejected")).not.toBeNull();
    // Still visible on the host's next repaint: the notice is local.
    screen.paint(view({}));
    expect(bar()?.textContent).toContain("panel_bar_activate");
    // The first accepted command withdraws it.
    expect(screen.accepted()).toBe(true);
    expect(screen.accepted()).toBe(false);
    screen.paint(view({}));
    expect(bar()?.textContent).not.toContain("panel_bar_activate");
    expect(bar()?.textContent).toContain("2 entradas");
  });

  it("a dialog is modal, has a name and says which answer destroys", () => {
    const { screen } = mount();
    const v = view({});
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
    const destructive = dialog.querySelector('button[data-destructive="true"]');
    expect(destructive).not.toBeNull();
  });

  it("a form paints its fields and each one sends ITS id", () => {
    const { screen, sent } = mount();
    const v = view({});
    v.dialogs = [
      {
        id: 7,
        title_key: "modal-search-title",
        subject: null,
        asker: null,
        deadline: null,
        destination: null,
        body: [{ text: "/casa", hostile: false }],
        overflow_note: "",
        choices: [
          { id: "confirm", label_key: "dialog-confirm", destructive: false },
          { id: "cancel", label_key: "dialog-cancel", destructive: false },
        ],
        input: null,
        input_hostile: false,
        input_secret: false,
        fields: [
          {
            id: "name",
            label_key: "search-name",
            value: "*.rs",
            hostile: false,
            kind: { kind: "text" },
          },
          {
            id: "min-size",
            label_key: "search-min-size",
            value: "",
            hostile: false,
            kind: { kind: "text" },
          },
          {
            id: "recursive",
            label_key: "search-toggle-recursive",
            value: "",
            hostile: false,
            kind: { kind: "toggle", on: true },
          },
          {
            id: "kinds",
            label_key: "search-toggle-kinds",
            value: "",
            hostile: false,
            kind: { kind: "cycle", value_key: "search-kinds-any" },
          },
        ],
      },
    ];
    screen.paint(v);
    const dialog = document.querySelector('[role="dialog"]') as HTMLElement;
    const texts = dialog.querySelectorAll<HTMLInputElement>('input[type="text"]');
    expect(texts.length).toBe(2);
    expect(texts[0]?.value).toBe("*.rs");
    expect(dialog.querySelectorAll('input[type="checkbox"]').length).toBe(1);

    // Typing in the SECOND field sends the second one's id, not the first's:
    // that's why fields are named by id and not by position.
    const second = texts[1];
    expect(second).toBeDefined();
    if (second !== undefined) {
      second.value = "1M";
      second.dispatchEvent(new Event("input"));
    }
    expect(sent.at(-1)).toEqual({
      action: "dialog_field",
      id: 7,
      field: "min-size",
      value: { set: "text", text: "1M" },
    });

    // A toggle says it was TOUCHED, not which state it's going to: the host
    // decides the destination, so two quick presses don't step on each
    // other.
    const checkbox = dialog.querySelector<HTMLInputElement>('input[type="checkbox"]');
    checkbox?.dispatchEvent(new Event("change"));
    expect(sent.at(-1)).toEqual({
      action: "dialog_field",
      id: 7,
      field: "recursive",
      value: { set: "toggled" },
    });

    // And the cycle, which is a button.
    const button = dialog.querySelector<HTMLButtonElement>('button[data-campo="kinds"]');
    button?.click();
    expect(sent.at(-1)).toEqual({
      action: "dialog_field",
      id: 7,
      field: "kinds",
      value: { set: "cycled" },
    });

    // **A repaint does NOT reseed a text field.**
    //
    // What the host sends is its PROJECTION — masked and bounded — so
    // seeding it back would make the next keystroke return it as if it were
    // what was typed: an on-screen `U+FFFD` would end up being the pattern
    // being searched for. The node gets reused, which is what the
    // single-field dialog already does.
    const before = dialog.querySelector<HTMLInputElement>('input[data-campo="name"]');
    expect(before).not.toBeNull();
    if (before !== null) {
      before.value = "a medio escribir";
    }
    const v2 = view({});
    v2.dialogs = v.dialogs;
    screen.paint(v2);
    const after = document.querySelector<HTMLInputElement>('input[data-campo="name"]');
    expect(after).toBe(before);
    expect(after?.value).toBe("a medio escribir");
  });

  it("a half-checked destination SAYS SO, and its warnings come out before the buttons", () => {
    const { screen } = mount();
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

    // While it's being checked, it SAYS SO. Without this line, the absence
    // of #164's would read as "this destination confines", which is an
    // assertion.
    const checking = view({});
    checking.dialogs = [{ ...base, dest_check: { state: "checking" } }];
    screen.paint(checking);
    expect(document.querySelector(".dialog-checking")).not.toBeNull();
    expect(document.querySelectorAll(".dialog-warning")).toHaveLength(0);

    // Answered and with warnings: one per line, each one as an alert.
    const withWarnings = view({});
    withWarnings.dialogs = [
      {
        ...base,
        dest_check: {
          state: "done",
          warnings: ["no cabe", "no puede confinar"],
        },
      },
    ];
    screen.paint(withWarnings);
    const warnings = Array.from(document.querySelectorAll(".dialog-warning"));
    expect(warnings).toHaveLength(2);
    const first = warnings[0] as HTMLElement;
    expect(first.textContent).toBe("no cabe");
    expect(first.getAttribute("role")).toBe("alert");
    expect(document.querySelector(".dialog-checking")).toBeNull();
    // And BEFORE the buttons: a warning landing below would move whatever's
    // under the pointer of someone who was already about to click.
    const dialog = document.querySelector('[role="dialog"]') as HTMLElement;
    const classes = Array.from(dialog.children).map((n) => n.className);
    const lastWarning = classes.lastIndexOf("dialog-warning");
    const buttons = classes.indexOf("choices");
    expect(lastWarning).toBeGreaterThanOrEqual(0);
    expect(buttons).toBeGreaterThan(lastWarning);

    // Answered and clean: not a single line. That it fits and that it
    // confines aren't announced — a line for each would teach people to
    // skip the line.
    const clean = view({});
    clean.dialogs = [{ ...base, dest_check: { state: "done", warnings: [] } }];
    screen.paint(clean);
    expect(document.querySelectorAll(".dialog-warning")).toHaveLength(0);
    expect(document.querySelector(".dialog-checking")).toBeNull();
  });

  it("answering a dialog sends its id, not a position", () => {
    const { screen, sent } = mount();
    const v = view({});
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
    const button = document.querySelector(".choices button") as HTMLButtonElement;
    button.click();
    expect(sent.at(-1)).toEqual({ action: "dialog", id: 7, choice: "cancel" });
  });
});

describe("the header", () => {
  it("paints the labels that came from Rust and marks the one sorting", () => {
    const { screen, root } = mount();
    screen.paint(view({}));
    const cols = root.querySelectorAll(".slot-columns .col");
    expect([...cols].map((c) => c.textContent)).toEqual(["Nombre▲", "Tamaño"]);
    expect(cols[0]?.getAttribute("aria-sort")).toBe("ascending");
    expect(cols[1]?.getAttribute("aria-sort")).toBe("none");
  });

  it("a click on the header sends the column's ID, not its position", () => {
    const { screen, sent, root } = mount();
    screen.paint(view({}));
    const size = root.querySelectorAll(".slot-columns .col")[1] as HTMLElement;
    size.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(sent.at(-1)).toEqual({ action: "sort_by", slot_id: 1, column: "size" });
  });

  it("a column that doesn't sort doesn't offer the gesture", () => {
    const { screen, sent, root } = mount();
    const v = view({});
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
    expect(sent.some((a) => a.action === "sort_by")).toBe(false);
  });

  it("a fixed width is declared on the slot's root and the cells read it", () => {
    const { screen, root } = mount();
    screen.paint(view({ rows: [row(1, "a.txt")] }));
    const slot = root.querySelector(".slot") as HTMLElement;
    // `size` comes with 9 cells and right-aligned; `name` carries no variable.
    expect(slot.style.getPropertyValue("--colw-size")).toBe("calc(var(--cell-w) * 9)");
    expect(slot.style.getPropertyValue("--colw-size-align")).toBe("right");
    expect(slot.style.getPropertyValue("--colw-name")).toBe("");
    const cell = root.querySelector(".row .cell") as HTMLElement;
    expect(cell.style.width).toBe("var(--colw-size, auto)");
    // The name has no grip; the size does.
    const cols = root.querySelectorAll(".slot-columns .col");
    expect(cols[0]?.querySelector(".col-grip")).toBeNull();
    expect(cols[1]?.querySelector(".col-grip")).not.toBeNull();
  });

  it("dragging the grip sends the width in CELLS on release, and doesn't sort", () => {
    const { screen, sent, root } = mount();
    document.documentElement.style.setProperty("--cell-w", "8px");
    screen.paint(view({}));
    const grip = root.querySelector(".col-grip") as HTMLElement;
    grip.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, clientX: 100 }));
    expect(sent.some((a) => a.action === "sort_by")).toBe(false);
    document.dispatchEvent(new MouseEvent("mousemove", { clientX: 140 }));
    // While dragging, only the variable changes: no dispatch.
    const slot = root.querySelector(".slot") as HTMLElement;
    expect(slot.style.getPropertyValue("--colw-size")).toBe("40px");
    expect(sent.some((a) => a.action === "resize_column")).toBe(false);
    document.dispatchEvent(new MouseEvent("mouseup"));
    expect(sent.at(-1)).toEqual({
      action: "resize_column",
      slot_id: 1,
      column: "size",
      cells: 5,
    });
  });
});

describe("breadcrumbs, the space indicator and the toast", () => {
  it("every path segment is a button that navigates to its depth, except the current one", () => {
    const { screen, sent, root } = mount();
    screen.paint(
      view({
        path_segments: ["⟨file⟩", "home", "oscar"],
        path_display: "⟨file⟩/home/oscar",
      }),
    );
    const crumbs = root.querySelectorAll(".title-path .crumb");
    expect([...crumbs].map((m) => m.textContent)).toEqual(["⟨file⟩", "home", "oscar"]);
    expect((crumbs[2] as HTMLButtonElement).disabled).toBe(true);
    // Only the root carries the mark that dims it (spec 2026-09-21, phase D).
    expect([...crumbs].map((m) => (m as HTMLElement).dataset["root"])).toEqual([
      "true",
      "false",
      "false",
    ]);
    (crumbs[1] as HTMLButtonElement).click();
    expect(sent.at(-1)).toEqual({
      action: "breadcrumb_activate",
      slot_id: 1,
      depth: 1,
      generation: 1,
    });
    // The whole path is still available as one piece.
    expect(root.querySelector(".title-path")?.getAttribute("title")).toBe(
      "⟨file⟩/home/oscar",
    );
  });

  it("with no crumbs, the path goes as text, same as before", () => {
    const { screen, root } = mount();
    screen.paint(view({}));
    expect(root.querySelector(".title-path")?.textContent).toBe("⟨file⟩/casa");
    expect(root.querySelector(".crumb")).toBeNull();
  });

  it("the footer carries the gauge only with data, and states its level", () => {
    const { screen, root } = mount();
    screen.paint(view({ footer: "2 ficheros", used_ratio: 0.92 }));
    const gauge = root.querySelector(".slot-footer .slot-gauge") as HTMLElement;
    expect(gauge.getAttribute("aria-valuenow")).toBe("92");
    expect(gauge.dataset["level"]).toBe("critical");
    expect((gauge.firstElementChild as HTMLElement).style.width).toBe("92%");
    screen.paint(view({ footer: "2 ficheros", used_ratio: null }));
    expect(root.querySelector(".slot-gauge")).toBeNull();
  });

  it("the ephemeral message goes in its own toast node", () => {
    const { screen, root } = mount();
    screen.paint(view({}));
    expect(root.querySelector(".statusbar .status-message")?.textContent).toBe(
      "2 entradas",
    );
  });
});

describe("the mark checkbox", () => {
  it("every row carries the checkbox, says if it's marked, and pressing it toggles the mark", () => {
    const { screen, sent, root } = mount();
    screen.paint(view({ rows: [row(1, "a.txt"), row(2, "b.txt", { marked: true })] }));
    const checks = root.querySelectorAll(".row .row-check");
    expect([...checks].map((c) => c.textContent)).toEqual(["☐", "☑"]);
    checks[0]?.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(sent.at(-1)).toMatchObject({ action: "toggle_mark", slot_id: 1, key: 1 });
  });
});

describe("the viewer", () => {
  it("covers the screen and says which encoding it's reading with", () => {
    const { screen, sent } = mount();
    const v = view({});
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
      image_zoom: 100,
      styled: [],
    };
    screen.paint(v);
    const doc = document.querySelector('[role="document"]') as HTMLElement;
    expect(doc.getAttribute("aria-label")).toContain("notas.txt");
    expect(doc.querySelector(".viewer-body")?.textContent).toBe("primera\nsegunda");
    expect(doc.querySelector(".viewer-meta")?.textContent).toContain("UTF-8");
    // Whoever paints declares the body's size: rows AND columns (bridge 53),
    // which is what the previewer receives next time.
    expect(sent.some((a) => a.action === "set_viewer_rows")).toBe(true);
    expect(sent.some((a) => a.action === "set_viewer_cols")).toBe(true);
  });

  it("says there's more sideways, and the wheel moves it", () => {
    const { screen, sent } = mount();
    const v = view({});
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
      image_zoom: 100,
      styled: [],
    };

    // It fits sideways: no bar to drag.
    v.viewer = { ...base, total_cols: 0, first_col: 0 };
    screen.paint(v);
    expect(document.querySelector(".viewer-bar-h")).toBe(null);

    // It doesn't fit: bar, with the position inside it.
    v.viewer = { ...base, total_cols: 800, first_col: 400 };
    screen.paint(v);
    const bar = document.querySelector(".viewer-bar-h") as HTMLElement;
    expect(bar).not.toBe(null);
    // The bar is an INDICATOR and gets `aria-hidden`: `role="scrollbar"`
    // promises a control that doesn't exist. The position is read from the
    // header's marks, in words, which is what reaches someone who can't see
    // it.
    expect(bar.getAttribute("aria-hidden")).toBe("true");
    expect(document.querySelector(".viewer-meta")?.textContent).toContain("401/800");

    // And the wheel scrolls through the HOST: with `shift`, sideways.
    const box = document.querySelector(".viewer") as HTMLElement;
    box.dispatchEvent(new WheelEvent("wheel", { deltaY: 120, bubbles: true }));
    const down = sent.find((a) => a.action === "viewer_scroll");
    expect(down).toBeDefined();
    expect(down?.action === "viewer_scroll" && down.lines > 0).toBe(true);
    expect(down?.action === "viewer_scroll" && down.cols === 0).toBe(true);

    box.dispatchEvent(
      new WheelEvent("wheel", { deltaY: 120, shiftKey: true, bubbles: true }),
    );
    const sideways = sent.filter((a) => a.action === "viewer_scroll").at(-1);
    expect(sideways?.action === "viewer_scroll" && sideways.lines === 0).toBe(true);
    expect(sideways?.action === "viewer_scroll" && sideways.cols > 0).toBe(true);
  });

  it("a patch that doesn't touch the viewer doesn't rebuild it", () => {
    const { screen } = mount();
    const v = view({});
    v.viewer = {
      path_display: "⟨file⟩/casa/largo.txt",
      path_hostile: false,
      encoding: "UTF-8",
      eol: "lf",
      hex: false,
      forced: false,
      had_errors: false,
      truncated: false,
      total_rows: 3,
      first_line: 0,
      total_cols: 0,
      first_col: 0,
      lines: ["a", "b", "c"],
      preview_by: "",
      preview_lossy: false,
      image: null,
      image_refused: "",
      image_zoom: 100,
      styled: [],
    };
    screen.paint(v);
    const before = document.querySelector(".viewer");
    expect(before).not.toBe(null);
    // Something else changes — the state, like a task does as it progresses
    // — and the viewer is the same object: its DOM stays.
    v.status = { ...v.status, message: "copiando" };
    screen.paint(v);
    expect(document.querySelector(".viewer")).toBe(before);
    // SCROLLING (a new viewer for the same file) changes the body and the
    // marks in place: the box and the header stay.
    const head = document.querySelector(".viewer-head");
    v.viewer = { ...v.viewer, first_line: 1, lines: ["b", "c"] };
    screen.paint(v);
    expect(document.querySelector(".viewer")).toBe(before);
    expect(document.querySelector(".viewer-head")).toBe(head);
    expect(document.querySelector(".viewer-body")?.textContent).toBe("b\nc");
    expect(document.querySelector(".viewer-meta")?.textContent).toContain("2/3");
    // Sideways, the horizontal bar appears, just one, and it moves.
    v.viewer = { ...v.viewer, total_cols: 800, first_col: 0 };
    screen.paint(v);
    v.viewer = { ...v.viewer, first_col: 400 };
    screen.paint(v);
    expect(document.querySelectorAll(".viewer-bar-h")).toHaveLength(1);
    expect(document.querySelector(".viewer-meta")?.textContent).toContain("401/800");
    v.viewer = { ...v.viewer, total_cols: 0, first_col: 0 };
    // Another FILE rebuilds the whole box.
    v.viewer = { ...v.viewer, path_display: "⟨file⟩/casa/otro.txt" };
    screen.paint(v);
    expect(document.querySelector(".viewer")).not.toBe(before);
    expect(document.querySelector(".viewer")?.getAttribute("aria-label")).toContain(
      "otro.txt",
    );
    expect(document.querySelector(".viewer-bar-h")).toBe(null);
    // Closing it and reopening the SAME object paints it again.
    const same = v.viewer;
    v.viewer = null;
    screen.paint(v);
    expect(document.querySelector(".viewer")).toBe(null);
    v.viewer = same;
    screen.paint(v);
    expect(document.querySelector(".viewer-body")?.textContent).toBe("b\nc");
  });

  it("a binary paints as hexadecimal and says so", () => {
    const { screen } = mount();
    const v = view({});
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
      image_zoom: 100,
      styled: [],
    };
    screen.paint(v);
    expect(document.querySelector(".viewer-body")?.classList.contains("hexview")).toBe(
      true,
    );
    expect(document.querySelector(".viewer-meta")?.textContent).toContain("hex");
  });

  it("a plugin preview paints in fragments, with its role or its color", () => {
    const { screen } = mount();
    const v = view({});
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
      image_zoom: 100,
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
    const lines = body.querySelectorAll(".viewer-line");
    expect(lines.length).toBe(2);
    const spans = (lines[0] as Element).querySelectorAll<HTMLElement>(".viewer-span");
    expect(spans.length).toBe(2);
    const fn_ = spans[0] as HTMLElement;
    const main = spans[1] as HTMLElement;
    expect(fn_.textContent).toBe("fn");
    // The role wins: it goes in `data-role` and the plugin's fixed color is
    // NOT applied.
    expect(fn_.dataset["role"]).toBe("title");
    expect(fn_.style.color).toBe("");
    // With no role, the plugin's own color does apply.
    expect(main.dataset["role"]).toBeUndefined();
    expect(main.style.color).toBe("rgb(0, 128, 255)");
    // And the background, when it comes (bridge 50).
    expect(main.style.backgroundColor).toBe("rgb(0, 255, 0)");
    expect(fn_.style.backgroundColor).toBe("");
    // The text is still TEXT.
    expect(body.textContent).toBe("fn mainplano");
  });

  it("with no viewer open, there's nothing covering the screen", () => {
    const { screen } = mount();
    screen.paint(view({}));
    expect(document.querySelector('[role="document"]')).toBeNull();
  });

  it("a file's content is TEXT, never markup", () => {
    const { screen } = mount();
    const v = view({});
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
      image_zoom: 100,
      styled: [],
    };
    screen.paint(v);
    const body = document.querySelector(".viewer-body") as HTMLElement;
    expect(body.querySelector("script")).toBeNull();
    expect(body.textContent).toBe("<script>alert(1)</script>");
  });
});

describe("the generation", () => {
  it("travels with every row gesture, and it's the one that was PAINTED", () => {
    const { screen, sent, root } = mount();
    const v = view({ generation: 7 });
    screen.paint(v);
    const row = root.querySelector(".row") as HTMLElement;
    row.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    const action = sent.at(-1);
    expect(action?.action).toBe("select_row");
    if (action?.action === "select_row") {
      expect(action.generation).toBe(7);
    }
  });

  it("updates on repaint: a later gesture carries the new one", () => {
    const { screen, sent, root } = mount();
    screen.paint(view({ generation: 7 }));
    screen.paint(view({ generation: 8 }));
    const row = root.querySelector(".row") as HTMLElement;
    row.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    const action = sent.at(-1);
    if (action?.action === "select_row") {
      expect(action.generation).toBe(8);
    }
  });
});

describe("a dialog's text field", () => {
  function withDialog(input: string, hostile: boolean) {
    const v = view({});
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

  // The LIVE field, re-queried from the DOM.
  //
  // Capturing it once doesn't work: the dialog repaints with
  // `replaceChildren`, so the old reference is left disconnected and an
  // assertion on it passes while the field on screen is empty. That's
  // exactly what hid every key emptying the field.
  function liveField(): HTMLInputElement {
    const input = document.querySelector(".dialog input");
    expect(input).not.toBeNull();
    expect(input?.isConnected).toBe(true);
    return input as HTMLInputElement;
  }

  it("doesn't get overwritten on every repaint: what was typed wins", () => {
    const { screen } = mount();
    screen.paint(withDialog("", false));
    // The user types; the host answers with ITS projection.
    liveField().value = "carpeta nueva";
    screen.paint(withDialog("carpeta nu…", false));
    expect(liveField().value).toBe("carpeta nueva");
  });

  it("survives one key per patch, which is how typing really happens", () => {
    const { screen, sent } = mount();
    screen.paint(withDialog("", false));
    // Every key triggers a `dialog_input` and the host answers with a patch,
    // i.e. a repaint. It's typed letter by letter, like a person.
    const name = "informe";
    for (let i = 1; i <= name.length; i += 1) {
      const field = liveField();
      field.value = name.slice(0, i);
      field.dispatchEvent(new Event("input", { bubbles: true }));
      screen.paint(withDialog(name.slice(0, i), false));
    }
    expect(liveField().value).toBe("informe");
    const last = sent.at(-1);
    expect(last?.action).toBe("dialog_input");
    if (last?.action === "dialog_input") {
      expect(last.text).toBe("informe");
    }
  });

  it("the field keeps FOCUS across the patch each key triggers", () => {
    // Reusing the node wasn't enough: the box gets rebuilt and the field
    // moves to the new one, and moving a node takes it out of the document
    // for an instant — that's where it lost focus. Deleting a digit in
    // "Font size" left the field unfocused and the next key went to the
    // host as a chord.
    const { screen } = mount();
    screen.paint(withDialog("10", false));
    const field = liveField();
    field.focus();
    expect(document.activeElement).toBe(field);
    field.value = "1";
    field.dispatchEvent(new Event("input", { bubbles: true }));
    screen.paint(withDialog("1", false));
    expect(liveField()).toBe(field);
    expect(document.activeElement).toBe(field);
  });

  it("a password paints as a password and doesn't get reseeded with dots", () => {
    const { screen, sent } = mount();
    const v = withDialog("", false);
    v.dialogs[0]!.title_key = "modal-ask-secret-title";
    v.dialogs[0]!.input_secret = true;
    screen.paint(v);

    const field = liveField();
    expect(field.type).toBe("password");
    // Neither the browser's password manager offers it nor saves it: this is
    // for THIS session, which is what the dialog's body promises.
    // `new-password` and not `off`: Chromium and WebView2 IGNORE `off` on a
    // password field on purpose, and this is the value they do respect.
    expect(field.autocomplete).toBe("new-password");

    field.value = "s3cr3t";
    field.dispatchEvent(new Event("input", { bubbles: true }));

    // Typing sends NOTHING: through `dialog_input` `s`, `s3`, `s3c`… would
    // cross, and every prefix would sit in a chunk of heap nobody overwrites.
    expect(sent.some((a) => a.action === "dialog_input")).toBe(false);

    // And a repaint doesn't reseed the field: doing so would turn the user's
    // password into whatever the host sent.
    screen.paint(v);
    expect(liveField().value).toBe("s3cr3t");

    // It crosses ONCE, with the answer.
    const button = document.querySelector(".dialog .choices button");
    (button as HTMLButtonElement).click();
    const last = sent.at(-1);
    expect(last?.action).toBe("dialog");
    if (last?.action === "dialog") {
      expect(last.choice).toBe("confirm");
      expect(last.secret).toBe("s3cr3t");
    }
  });

  it("Enter inside the password field confirms and carries the value", () => {
    const { screen, sent } = mount();
    const v = withDialog("", false);
    v.dialogs[0]!.title_key = "modal-ask-secret-title";
    v.dialogs[0]!.input_secret = true;
    screen.paint(v);

    const field = liveField();
    field.value = "s3cr3t";
    field.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));

    // Without this, Enter went out to the host as the `dialog.confirm`
    // chord — which over a password dialog carries nothing and so is inert
    // — so the most natural way to answer would have done nothing.
    const last = sent.at(-1);
    expect(last?.action).toBe("dialog");
    if (last?.action === "dialog") {
      expect(last.secret).toBe("s3cr3t");
    }
  });

  it("cancelling doesn't carry the password", () => {
    const { screen, sent } = mount();
    const v = withDialog("", false);
    v.dialogs[0]!.title_key = "modal-ask-secret-title";
    v.dialogs[0]!.input_secret = true;
    v.dialogs[0]!.choices = [
      { id: "confirm", label_key: "dialog-confirm", destructive: false },
      { id: "cancel", label_key: "dialog-cancel", destructive: false },
    ];
    screen.paint(v);
    liveField().value = "s3cr3t";

    const buttons = document.querySelectorAll(".dialog .choices button");
    (buttons[1] as HTMLButtonElement).click();
    const last = sent.at(-1);
    expect(last?.action).toBe("dialog");
    if (last?.action === "dialog") {
      expect(last.choice).toBe("cancel");
      expect(last.secret).toBeUndefined();
    }
  });

  it("hostile bytes that get typed arrive whole, and a warning shows", () => {
    const { screen, sent } = mount();
    screen.paint(withDialog("", true));
    // `rtl_override` from the corpus: what gets approved has to be what was
    // typed, not a reconstruction of it.
    const hostile = "fact\u202Egpj.exe";
    const field = liveField();
    field.value = hostile;
    field.dispatchEvent(new Event("input", { bubbles: true }));
    // The host answers with its projection: masked and different.
    screen.paint(withDialog("fact\uFFFDgpj.exe", true));
    expect(liveField().value).toBe(hostile);
    const last = sent.at(-1);
    if (last?.action === "dialog_input") {
      expect(last.text).toBe(hostile);
    }
    expect(document.querySelector('.dialog [role="alert"]')).not.toBeNull();
  });

  it("a name that paints differently from what it is SAYS SO", () => {
    const { screen } = mount();
    screen.paint(withDialog("caf\ufffde.txt", true));
    const notice = document.querySelector('.dialog [role="alert"]');
    expect(notice).not.toBeNull();
  });
});

describe("the viewer's image", () => {
  function withImage(image: ViewSnapshot["viewer"]): ViewSnapshot {
    const v = view({});
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

  it("requests the bytes SEPARATELY and paints them as a blob", async () => {
    const bytes = new Uint8Array([1, 2, 3, 4]).buffer;
    const { screen } = mount({ imageBytes: () => Promise.resolve(bytes) });
    screen.paint(
      withImage({
        ...base,
        image: { format: "PNG", width: 800, height: 600 },
        image_refused: "",
        image_zoom: 100,
      }),
    );
    // The promise resolves on the next turn.
    await Promise.resolve();
    await Promise.resolve();
    const img = document.querySelector<HTMLImageElement>(".viewer-image");
    expect(img).not.toBeNull();
    // `blob:`, never `file:` nor `data:` (ADR 0069).
    expect(img?.src.startsWith("blob:")).toBe(true);
    // With the DECLARED size, so the box doesn't jump on load.
    expect(img?.width).toBe(800);
    expect(img?.height).toBe(600);
  });

  it("and closing the viewer REVOKES the blob", async () => {
    const revoked: string[] = [];
    const revoke = URL.revokeObjectURL.bind(URL);
    URL.revokeObjectURL = (u: string) => {
      revoked.push(u);
      revoke(u);
    };
    try {
      const bytes = new Uint8Array([1, 2, 3]).buffer;
      const { screen } = mount({ imageBytes: () => Promise.resolve(bytes) });
      screen.paint(
        withImage({
          ...base,
          image: { format: "PNG", width: 10, height: 10 },
          image_refused: "",
          image_zoom: 100,
        }),
      );
      await Promise.resolve();
      await Promise.resolve();
      screen.paint(withImage(null));
      // An unrevoked object URL keeps its bytes alive as long as the
      // document lives, and these are megabytes.
      expect(revoked).toHaveLength(1);
    } finally {
      URL.revokeObjectURL = revoke;
    }
  });

  it("a REFUSED image says so, and nothing is requested", () => {
    let requested = false;
    const { screen } = mount({
      imageBytes: () => {
        requested = true;
        return Promise.resolve(new ArrayBuffer(0));
      },
    });
    screen.paint(
      withImage({
        ...base,
        image: null,
        image_refused: "imagen demasiado grande para previsualizarla",
        image_zoom: 100,
      }),
    );
    const no = document.querySelector(".viewer-image-refused");
    expect(no?.textContent).toContain("demasiado grande");
    // Announced, for whoever isn't looking at the screen: silently falling
    // back to the hexview looks like norte is broken, not norte being
    // careful.
    expect(no?.getAttribute("role")).toBe("status");
    expect(requested).toBe(false);
    expect(document.querySelector(".viewer-image")).toBeNull();
  });
});

describe("a plugin's preview in the viewer", () => {
  it("says whose the shown content is, apart from the loss notice", () => {
    const { screen } = mount();
    const v = view({});
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
      image_zoom: 100,
      styled: [],
    };
    screen.paint(v);
    const via = document.querySelector(".viewer-via");
    expect(via?.textContent).toBe("via PDF de ACME");
    // The loss notice in its OWN node: `had_errors` is the raw view's and
    // this one is the decoding the previewer was given. They're two
    // decodings, and mixing them up blames the file for what the reading
    // did.
    const notice = document.querySelector(".viewer-via-lossy");
    expect(notice).not.toBeNull();
    expect(notice?.textContent).toBe(realCatalog()["viewer-plugin-preview-lossy"] ?? "");
    expect(via?.contains(notice)).toBe(false);
  });

  it("the PATH is what gets truncated, not the marks", () => {
    const { screen } = mount();
    const v = view({});
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
      image_zoom: 100,
      styled: [],
    };
    screen.paint(v);
    const head = document.querySelector(".viewer-head");
    const path = head?.querySelector(".viewer-path");
    // The path in its OWN node and the marks as its SIBLINGS. Loose as text
    // it was an anonymous flex item that doesn't shrink, so it pushed
    // everything that came after it out of view. jsdom doesn't do layout, so
    // what's pinned here is the structure that makes it impossible.
    expect(path?.textContent).toContain("informe.pdf");
    expect(head?.querySelector(".viewer-via")?.parentElement).toBe(head);
    expect(path?.querySelector(".viewer-via")).toBeNull();
  });

  it("and with no plugin, nothing is attributed to anyone", () => {
    const { screen } = mount();
    const v = view({});
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
      image_zoom: 100,
      styled: [],
    };
    screen.paint(v);
    expect(document.querySelector(".viewer-via")).toBeNull();
    expect(document.querySelector(".viewer-via-lossy")).toBeNull();
  });
});

describe("the column selector", () => {
  function withColumns(): ViewSnapshot {
    const v = view({});
    v.columns = {
      title: "Columnas — sftp",
      cursor: 1,
      note: "se aplica a esta ventana; no se guarda",
      // The footer is painted by the HOST from the keymap (#287).
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

  it("says what's on, what's fixed and what format each one has", () => {
    const { screen } = mount();
    screen.paint(withColumns());
    const rows = [...document.querySelectorAll(".columns-row")];
    expect(rows).toHaveLength(3);
    // On or off, to the screen reader too, not just to whoever looks.
    expect(rows[0]?.getAttribute("aria-checked")).toBe("true");
    expect(rows[2]?.getAttribute("aria-checked")).toBe("false");
    // NAME is fixed: it's neither turned off nor moved.
    expect(rows[0]?.getAttribute("data-fixed")).toBe("true");
    expect(rows[1]?.getAttribute("data-fixed")).toBe("false");
    // A locked format PAINTS dimmed, it doesn't disappear: a key that does
    // nothing and doesn't say why is worse than one that says no.
    const fmt = rows[2]?.querySelector(".columns-format");
    expect(fmt?.textContent).toBe("symbolic");
    expect(fmt?.getAttribute("data-locked")).toBe("true");
    // And a column with no format doesn't make one up.
    expect(rows[0]?.querySelector(".columns-format")).toBeNull();
  });

  it("says the choice is NOT saved", () => {
    const { screen } = mount();
    screen.paint(withColumns());
    const note = document.querySelector(".columns-note");
    expect(note?.textContent).toContain("no se guarda");
    // With `role="note"`, for whoever isn't looking at the screen: thinking
    // you just configured norte and finding out you didn't is worse than not
    // being able to.
    expect(note?.getAttribute("role")).toBe("note");
  });

  it("and the scope goes in the TITLE, which is the first thing read", () => {
    const { screen } = mount();
    screen.paint(withColumns());
    const box = document.querySelector(".columns-picker");
    expect(box?.querySelector("h1")?.textContent).toContain("sftp");
    expect(box?.getAttribute("aria-modal")).toBe("true");
  });
});

describe("entries the provider skipped", () => {
  it("are SAID in the header, which is where the reader can see them", () => {
    const { screen } = mount();
    const v = view({ skipped_note: "se saltaron 3 entradas" });
    screen.paint(v);
    const notice = document.querySelector(".slot-skipped");
    expect(notice?.textContent).toBe("se saltaron 3 entradas");
    // In the HEADER: at the end of the list it would be useless, because
    // what's missing isn't there and there's no row to stumble on.
    expect(notice?.closest(".slot-title")).not.toBeNull();
    // And announced, for whoever isn't looking at the screen.
    expect(notice?.getAttribute("role")).toBe("status");
  });

  it("the path is what gets truncated, not the notice", () => {
    const { screen } = mount();
    screen.paint(view({ skipped_note: "se saltaron 3 entradas" }));
    const title = document.querySelector(".slot-title");
    const path = title?.querySelector(".title-path");
    const notice = title?.querySelector(".slot-skipped");
    // The path in its OWN node and the notice as its SIBLING. With the path
    // as loose text in the header, a long one pushed the notice out of view
    // and it disappeared in silence. jsdom doesn't do layout, so what can be
    // pinned here is the structure that makes it impossible; the actual
    // truncation is checked by painting.
    expect(path?.textContent).toContain("casa");
    expect(notice?.parentElement).toBe(title);
    expect(path?.contains(notice ?? null)).toBe(false);
  });

  it("and a complete listing says nothing", () => {
    const { screen } = mount();
    screen.paint(view({}));
    expect(document.querySelector(".slot-skipped")).toBeNull();
  });
});

describe("a plugin's badge on a row", () => {
  it("goes in its own node, with the theme's role and without touching the name", () => {
    const { screen } = mount();
    const v = view({});
    const slot = v.slots[0];
    if (slot?.kind === "browser") {
      slot.rows = [
        row(1, "limpio.rs"),
        row(2, "cambiado.rs", { badge: "M", badge_role: "warning" }),
      ];
      slot.total_rows = 2;
    }
    screen.paint(v);
    const rows = [...document.querySelectorAll(".row")];
    expect(rows[0]?.querySelector(".cell-badge")).toBeNull();
    const badge = rows[1]?.querySelector(".cell-badge");
    expect(badge?.textContent).toBe("M");
    // The ROLE, not a color the plugin picks.
    expect(badge?.getAttribute("data-role")).toBe("warning");
    // And in its own node: joining it to the name lets one reorder the
    // other.
    expect(rows[1]?.querySelector(".cell-name")?.textContent).toBe("cambiado.rs");
  });

  it("the icon goes to the LEFT of the name, and the column opens for every row", () => {
    const { screen } = mount();
    screen.paint(
      view({
        rows: [
          row(1, "src", { kind: "dir", icon: "📁" }),
          row(2, "main.rs", { icon: "🦀", badge: "M", badge_role: "warning" }),
          row(3, "sin-icono"),
        ],
        icon_column: true,
      }),
    );
    const rows = [...document.querySelectorAll(".row")];
    // All three carry the cell: the one with no icon, empty, so names stay
    // aligned.
    for (const f of rows) {
      expect(f.querySelector(".cell-icon")).not.toBeNull();
    }
    expect(rows[0]?.querySelector(".cell-icon")?.textContent).toBe("📁");
    expect(rows[2]?.querySelector(".cell-icon")?.textContent).toBe("");
    // Before the name; the badge, after. The two slots in one row.
    const block = rows[1]?.querySelector(".name-block");
    const children = [...(block?.children ?? [])].map((c) => c.className);
    expect(children).toEqual(["cell-icon", "cell-name", "cell-badge"]);
  });

  it("the HOST opens the column, not the visible rows", () => {
    const { screen } = mount();
    // No icons in sight but with the column open — a page with no icons from
    // a listing that does have them: the cell still shows, so names don't
    // shift while scrolling.
    screen.paint(view({ rows: [row(1, "a.rs"), row(2, "b.rs")], icon_column: true }));
    expect(document.querySelectorAll(".cell-icon")).toHaveLength(2);
    // And the other way: an icon on a row with the column closed doesn't
    // open it.
    screen.paint(view({ rows: [row(1, "a.rs", { icon: "🦀" })], icon_column: false }));
    expect(document.querySelector(".cell-icon")).toBeNull();
  });

  it("an icon that paints differently from what it is SAYS SO", () => {
    const { screen } = mount();
    screen.paint(
      view({
        rows: [row(1, "x", { icon: "�", icon_hostile: true })],
        icon_column: true,
      }),
    );
    const icon = document.querySelector(".cell-icon");
    expect(icon?.getAttribute("data-hostile")).toBe("true");
    expect(icon?.querySelector(".hostile-badge")).not.toBeNull();
  });

  it("a badge that paints differently from what it is SAYS SO", () => {
    const { screen } = mount();
    const v = view({});
    const slot = v.slots[0];
    if (slot?.kind === "browser") {
      slot.rows = [
        row(1, "x.rs", { badge: "a\uFFFDb", badge_hostile: true, badge_role: "error" }),
      ];
      slot.total_rows = 1;
    }
    screen.paint(v);
    const badge = document.querySelector(".cell-badge");
    expect(badge?.getAttribute("data-hostile")).toBe("true");
    expect(badge?.querySelector(".hostile-badge")).not.toBeNull();
  });
});

describe("help's scroll", () => {
  function withHelp(topic: string, cursor: number): ViewSnapshot {
    const v = view({});
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
      scroll: null,
      filter: "",
      filtering: false,
      focus: "body",
    };
    return v;
  }

  it("doesn't jump back to the top on every patch: read half a page and move the cursor down", () => {
    const { screen } = mount();
    screen.paint(withHelp("copying", 0));
    const body = () => document.querySelector(".help-body") as HTMLElement;
    // The reader scrolls down the page. jsdom's `scrollTop` doesn't clamp
    // itself, which is exactly what's needed to check it's preserved.
    body().scrollTop = 120;
    // Any patch — moving the sidebar's cursor is one — repaints.
    screen.paint(withHelp("copying", 1));
    expect(body().scrollTop).toBe(120);
  });

  it("and changing PAGE starts at the top, which is what a reader does", () => {
    const { screen } = mount();
    screen.paint(withHelp("copying", 0));
    const body = () => document.querySelector(".help-body") as HTMLElement;
    body().scrollTop = 120;
    screen.paint(withHelp("moving", 0));
    expect(body().scrollTop).toBe(0);
  });
});

describe("which-key", () => {
  function withPanel() {
    const v = view({});
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

  it("shows continuations with their label, and marks the ones that open another sequence", () => {
    const { screen } = mount();
    screen.paint(withPanel());
    const rows = document.querySelectorAll(".whichkey-row");
    expect(rows).toHaveLength(3);
    expect(rows[0]?.textContent).toBe("gIr al principio");
    // One that opens a sequence gets MARKED instead of naming a command it
    // doesn't run.
    expect(rows[1]?.textContent).toContain("…");
  });

  it("a key that can't be used here says why, and isn't hidden", () => {
    const { screen } = mount();
    screen.paint(withPanel());
    const disabled = document.querySelectorAll('.whichkey-row[data-enabled="false"]');
    expect(disabled).toHaveLength(1);
    expect(disabled[0]?.textContent).toContain("aquí no");
  });

  it("isn't a dialog: it doesn't capture focus", () => {
    const { screen } = mount();
    screen.paint(withPanel());
    const panel = document.querySelector(".whichkey");
    expect(panel?.getAttribute("role")).toBe("group");
    expect(panel?.getAttribute("aria-modal")).toBeNull();
  });

  it("with no prefix half-typed, there's no panel", () => {
    const { screen } = mount();
    screen.paint(view({}));
    expect(document.querySelector(".whichkey")).toBeNull();
  });
});

describe("moving a panel by dragging it (ADR 0138)", () => {
  /** Two listings side by side, 600x400 px each. */
  function twoListings(): ViewSnapshot {
    const v = view({});
    const first = v.slots[0];
    if (first === undefined || first.kind !== "browser") {
      throw new Error("the starting view carries a listing");
    }
    v.slots.push({ ...first, slot_id: 2 });
    v.layout.placements = [
      { slot_id: 1, x: 0, y: 0, width: 60, height: 38, role: "active", focus_index: 0 },
      { slot_id: 2, x: 60, y: 0, width: 60, height: 38, role: null, focus_index: 1 },
      { slot_id: 4, x: 0, y: 39, width: 120, height: 1, role: null, focus_index: 2 },
    ];
    return v;
  }

  beforeEach(() => {
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
      this: HTMLElement,
    ) {
      const id = this.dataset["slotId"];
      const x = id === "1" ? 0 : id === "2" ? 600 : id === "4" ? 0 : 0;
      const [w, h] =
        id === "1" || id === "2" ? [600, 400] : id === "4" ? [1200, 20] : [0, 0];
      const y = id === "4" ? 400 : 0;
      return new DOMRect(x, y, w, h);
    });
  });
  afterEach(async () => {
    vi.restoreAllMocks();
    // The click swallowed at the end of a drag is cleared on a zero
    // timer: without letting it run, it would eat the next test's first
    // click.
    await new Promise((r) => setTimeout(r, 0));
  });

  const pointer = (type: string, x: number, y: number): MouseEvent =>
    new MouseEvent(type, { button: 0, clientX: x, clientY: y, bubbles: true });

  // Regression (caught 2026-09-21): every step of dragging a border changes
  // the layout and rebuilds the handles; with the capture on the handle, the
  // drag died on the first step and the border wouldn't move either way.
  it("dragging a border survives the layout rebuilding the handles", () => {
    const { screen, sent } = mount();
    const v = twoListings();
    screen.paint(v);
    const handle = document.querySelector(".resize-handle.col") as HTMLElement;
    handle.dispatchEvent(pointer("pointerdown", 480, 100));
    window.dispatchEvent(pointer("pointermove", 400, 100));
    // The host answers with another layout: slots and handles get rebuilt.
    const other = twoListings();
    other.layout.placements = [
      { slot_id: 1, x: 0, y: 0, width: 50, height: 38, role: "active", focus_index: 0 },
      { slot_id: 2, x: 50, y: 0, width: 70, height: 38, role: null, focus_index: 1 },
      { slot_id: 4, x: 0, y: 39, width: 120, height: 1, role: null, focus_index: 2 },
    ];
    screen.paint(other);
    expect(document.body.contains(handle)).toBe(false);
    window.dispatchEvent(pointer("pointermove", 320, 100));
    window.dispatchEvent(pointer("pointermove", 321, 100));
    window.dispatchEvent(pointer("pointerup", 320, 100));
    window.dispatchEvent(pointer("pointermove", 200, 100));
    const steps = sent.filter((a) => a.action === "resize_slot");
    expect(steps).toEqual([
      { action: "resize_slot", slot_id: 1, cells: 50 },
      { action: "resize_slot", slot_id: 1, cells: 40 },
    ]);
    expect(document.documentElement.dataset["dragging"]).toBeUndefined();
  });

  it("zoneOf: the nearest side under a quarter away, else the center", () => {
    const r = { left: 0, top: 0, width: 100, height: 100 };
    expect(zoneOf(5, 50, r)).toBe("left");
    expect(zoneOf(95, 50, r)).toBe("right");
    expect(zoneOf(50, 3, r)).toBe("top");
    expect(zoneOf(50, 90, r)).toBe("bottom");
    expect(zoneOf(50, 50, r)).toBe("center");
  });

  it("dragging the title and dropping on another sends move_slot with the zone", () => {
    const { screen, sent } = mount();
    screen.paint(twoListings());
    sent.length = 0;
    const title = document.querySelector('[data-slot-id="1"] .slot-title') as HTMLElement;
    title.dispatchEvent(pointer("pointerdown", 10, 5));
    window.dispatchEvent(pointer("pointermove", 900, 200));
    expect(document.documentElement.dataset["dragging"]).toBe("slot");
    const veil = document.querySelector(".drop-target") as HTMLElement;
    expect(veil.hidden).toBe(false);
    expect(veil.dataset["zone"]).toBe("center");
    window.dispatchEvent(pointer("pointermove", 1150, 200));
    expect(veil.dataset["zone"]).toBe("right");
    window.dispatchEvent(pointer("pointerup", 1150, 200));
    expect(sent.filter((a) => a.action === "move_slot")).toEqual([
      { action: "move_slot", slot_id: 1, target: 2, zone: "right" },
    ]);
    expect(document.querySelector(".drop-target")).toBeNull();
    expect(document.documentElement.dataset["dragging"]).toBeUndefined();
  });

  it("a click, dropping on itself or on the chrome, and Esc move nothing", () => {
    const { screen, sent } = mount();
    screen.paint(twoListings());
    const title = document.querySelector('[data-slot-id="1"] .slot-title') as HTMLElement;
    const attempt = (steps: [number, number][], beforeDrop?: () => void): void => {
      title.dispatchEvent(pointer("pointerdown", 10, 5));
      for (const [x, y] of steps) {
        window.dispatchEvent(pointer("pointermove", x, y));
      }
      beforeDrop?.();
      window.dispatchEvent(pointer("pointerup", 0, 0));
    };
    attempt([[12, 6]]);
    attempt([[300, 200]]);
    attempt([[300, 410]]);
    attempt([[900, 200]], () => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });
    attempt([[900, 200]], () => {
      window.dispatchEvent(new Event("pointercancel"));
    });
    expect(sent.filter((a) => a.action === "move_slot")).toEqual([]);
    expect(document.querySelector(".drop-target")).toBeNull();
  });
});

describe("the menu bar", () => {
  function withMenu(open: number | null) {
    const v = view({});
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

  it("paints sections without counting them as entries", () => {
    const { screen } = mount();
    screen.paint(withMenu(1));
    const sections = [...document.querySelectorAll(".menu-section")];
    expect(sections.map((s) => s.textContent)).toEqual(["Sitios", ""]);
    expect(sections.every((s) => s.getAttribute("role") === "separator")).toBe(true);
    // The cursor still names ENTRIES: 1 is "Desconectar", not the divider.
    const current = document.querySelector('.menu-item[data-current="true"]');
    expect(current?.id).toBe("menu-item-1");
    const del = document.querySelector("#menu-item-2");
    expect(del?.getAttribute("data-role")).toBe("destructive");
  });

  it("paints the titles and reserves their row", () => {
    const { screen } = mount();
    screen.paint(withMenu(null));
    const titles = [...document.querySelectorAll(".menubar-title")].map(
      (t) => t.textContent,
    );
    expect(titles).toEqual(["Archivo", "Paneles"]);
    // The row is RESERVED: the host's layout is computed over the height
    // this renderer declares, and a floating bar would cover the first row.
    expect(document.documentElement.style.getPropertyValue("--menubar-h")).toBe(
      "var(--cell-h)",
    );
    expect(document.querySelector(".menu-items")).toBeNull();
  });

  describe("with its own title bar (ADR 0136)", () => {
    beforeEach(() => {
      document.documentElement.dataset["titlebar"] = "custom";
    });
    afterEach(() => {
      delete document.documentElement.dataset["titlebar"];
    });

    it("carries the three window buttons and each one requests its verb", () => {
      const requests: WindowVerb[] = [];
      const { screen, sent } = mount({ windowControl: (v) => requests.push(v) });
      screen.paint(withMenu(null));
      const buttons = [
        ...document.querySelectorAll(".menubar .window-controls .window-control"),
      ] as HTMLButtonElement[];
      expect(buttons.map((b) => b.dataset["verb"])).toEqual([
        "minimize",
        "toggle_maximize",
        "close",
      ]);
      expect(buttons.every((b) => b.querySelector("svg.panelbar-icon") !== null)).toBe(
        true,
      );
      expect(buttons[2]?.getAttribute("aria-label")).toBe("Cerrar");
      for (const b of buttons) {
        b.click();
      }
      expect(requests).toEqual(["minimize", "toggle_maximize", "close"]);
      // None of this belongs to the host: it isn't screen state.
      expect(sent).toEqual([]);
    });

    it("the free space drags and a double click maximizes; a title doesn't", () => {
      const requests: WindowVerb[] = [];
      const { screen } = mount({ windowControl: (v) => requests.push(v) });
      screen.paint(withMenu(null));
      const bar = document.querySelector(".menubar") as HTMLElement;
      bar.dispatchEvent(
        new MouseEvent("mousedown", { button: 0, detail: 1, bubbles: true }),
      );
      bar.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
      const title = document.querySelector(".menubar-title") as HTMLElement;
      title.dispatchEvent(
        new MouseEvent("mousedown", { button: 0, detail: 1, bubbles: true }),
      );
      title.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
      expect(requests).toEqual(["drag", "toggle_maximize"]);
    });

    it("with the menu off the row stays: it's the only thing that moves and closes", () => {
      const { screen } = mount();
      const v = withMenu(null);
      v.menu.bar = false;
      screen.paint(v);
      expect(document.documentElement.style.getPropertyValue("--menubar-h")).toBe(
        "var(--cell-h)",
      );
      expect(document.querySelectorAll(".menubar-title")).toHaveLength(0);
      expect(document.querySelectorAll(".window-control")).toHaveLength(3);
    });
  });

  it("with the native bar there are no window buttons", () => {
    const { screen } = mount();
    screen.paint(withMenu(null));
    expect(document.querySelector(".window-controls")).toBeNull();
    expect(
      (document.querySelector(".menubar") as HTMLElement).dataset["titlebar"],
    ).toBeUndefined();
  });

  it("with the bar off, nothing is reserved", () => {
    const { screen } = mount();
    const v = withMenu(null);
    v.menu.bar = false;
    screen.paint(v);
    expect(document.documentElement.style.getPropertyValue("--menubar-h")).toBe("0px");
    expect(document.querySelector(".menubar")).toBeNull();
  });

  // #324: the panel bar SHOWS the panels — state, novelty, and a click that
  // comes back as an index, never as a command (ADR 0069).
  it("the panel bar paints every button with its state and reserves its row", () => {
    const { screen, sent } = mount();
    screen.paint(view({}));
    expect(document.documentElement.style.getPropertyValue("--panelbar-h")).toBe(
      "var(--cell-h)",
    );
    const buttons = [
      ...document.querySelectorAll(".panelbar-button"),
    ] as HTMLButtonElement[];
    expect(buttons.map((b) => b.dataset["kind"])).toEqual(["places", "log"]);
    expect(buttons[0]?.dataset["state"]).toBe("open");
    expect(buttons[0]?.getAttribute("aria-pressed")).toBe("true");
    expect(buttons[0]?.title).toBe("Sitios (alt+p)");
    expect(buttons[1]?.dataset["state"]).toBe("closed");
    expect(buttons[1]?.getAttribute("aria-pressed")).toBe("false");
    expect(buttons[1]?.title).toBe("Registro");
    // Novelty is a SEPARATE mark, not a change to the button's style.
    expect(buttons[0]?.querySelector(".panelbar-attention")).toBeNull();
    expect(buttons[1]?.querySelector(".panelbar-attention")).not.toBeNull();
    // And the row has its landmark, translated from the real catalogue.
    expect(document.querySelector(".panelbar")?.getAttribute("aria-label")).toBe(
      "Barra de paneles",
    );

    buttons[1]?.click();
    expect(sent).toEqual([{ action: "panel_bar_activate", button: 1 }]);
  });

  it("layout buttons go to the right of the menu and come back by id", () => {
    const { screen, sent } = mount();
    const v = view({});
    v.layout_buttons = [
      { id: "split-h", label: "Partir lado a lado", chord: "ctrl+\\" },
      { id: "pick", label: "Disposición...", chord: "—" },
    ];
    screen.paint(v);
    const buttons = [
      ...document.querySelectorAll(".menubar .menubar-actions .menubar-action"),
    ] as HTMLButtonElement[];
    expect(buttons.map((b) => b.dataset["id"])).toEqual(["split-h", "pick"]);
    expect(buttons[0]?.querySelector("svg.panelbar-icon")).not.toBeNull();
    expect(buttons[0]?.title).toBe("Partir lado a lado (ctrl+\\)");
    expect(buttons[1]?.title).toBe("Disposición...");
    expect(buttons[1]?.getAttribute("aria-label")).toBe("Disposición...");
    buttons[1]?.click();
    expect(sent).toEqual([{ action: "layout_button_activate", id: "pick" }]);
  });

  it("a PANEL group carries neither + nor x and gets marked for its style", () => {
    const { screen, sent } = mount();
    const v = view({});
    v.layout.tabs = [
      {
        slot_id: 1,
        active: 1,
        panels: true,
        tabs: [
          { slot_id: 7, title: "Historial", title_hostile: false },
          { slot_id: 1, title: "Detalles", title_hostile: false },
        ],
      },
    ];
    screen.paint(v);
    expect(document.querySelector(".tab-new")).toBeNull();
    expect(document.querySelector(".tab-close")).toBeNull();
    const strip = document.querySelector(".slot-tabs") as HTMLElement;
    expect(strip.dataset["panels"]).toBe("true");
    (document.querySelectorAll(".tab")[0] as HTMLElement).click();
    expect(sent).toEqual([{ action: "select_tab", slot_id: 7 }]);
  });

  it("a group's tabs carry their x and the group its +, and the x doesn't select", () => {
    const { screen, sent } = mount();
    const v = view({});
    v.layout.tabs = [
      {
        slot_id: 1,
        active: 0,
        tabs: [
          { slot_id: 1, title: "casa", title_hostile: false },
          { slot_id: 7, title: "tmp", title_hostile: false },
        ],
      },
    ];
    screen.paint(v);
    const close = [
      ...document.querySelectorAll(".tab .tab-close"),
    ] as HTMLButtonElement[];
    expect(close).toHaveLength(2);
    close[1]?.click();
    // ONE command: the x doesn't let the click reach the tab and select it.
    expect(sent).toEqual([{ action: "tab_action", slot_id: 7, verb: "close" }]);
    sent.length = 0;
    (document.querySelector(".tab-new") as HTMLButtonElement).click();
    // `+` opens behind the group's ACTIVE tab.
    expect(sent).toEqual([{ action: "tab_action", slot_id: 1, verb: "new" }]);
  });

  it("the status bar's right half paints its items and they're clickable", () => {
    const { screen, sent } = mount();
    const v = view({});
    v.status_items = [
      { id: "position", text: "3/120", tooltip: "Posición", clickable: false },
      { id: "notices", text: "!2", tooltip: "Avisos", clickable: true },
    ];
    screen.paint(v);
    const right = document.querySelector(".statusbar .status-items") as HTMLElement;
    expect(right).not.toBeNull();
    const els = [...right.querySelectorAll(".status-item")] as HTMLElement[];
    expect(els.map((e) => e.textContent)).toEqual(["3/120", "!2"]);
    // What isn't clickable isn't a button: a reader doesn't announce it as
    // one.
    expect(els[0]?.tagName).toBe("SPAN");
    expect(els[1]?.tagName).toBe("BUTTON");
    expect(els[1]?.title).toBe("Avisos");
    els[1]?.click();
    expect(sent).toEqual([{ action: "status_item_activate", id: "notices" }]);
  });

  it("with the panel bar off, nothing is reserved", () => {
    const { screen } = mount();
    const v = view({});
    v.panel_bar.bar = false;
    screen.paint(v);
    expect(document.documentElement.style.getPropertyValue("--panelbar-h")).toBe("0px");
    expect(document.documentElement.style.getPropertyValue("--activity-w")).toBe("0px");
    expect(document.querySelector(".panelbar")).toBeNull();
  });

  it("in column layout it's the activity bar: reserves WIDTH, icon and count", () => {
    const { screen, sent } = mount();
    const v = view({});
    v.panel_bar.vertical = true;
    const log = v.panel_bar.buttons[1];
    if (log !== undefined) {
      log.count = 7;
    }
    v.panel_bar.buttons.push({
      kind: "plugin:git:status",
      label: "Git",
      letter: "G",
      chord: "—",
      state: "focused",
      attention: true,
      count: 150,
    });
    screen.paint(v);
    const root = document.documentElement.style;
    // Column: the top row reserves nothing and the left border does.
    expect(root.getPropertyValue("--panelbar-h")).toBe("0px");
    expect(root.getPropertyValue("--activity-w")).toBe("var(--activity-size)");
    expect(screen.takeViewportDirty()).toBe(true);
    const bar = document.querySelector(".panelbar") as HTMLElement;
    expect(bar.dataset["vertical"]).toBe("true");
    expect(bar.getAttribute("aria-orientation")).toBe("vertical");
    const buttons = [
      ...document.querySelectorAll(".panelbar-button"),
    ] as HTMLButtonElement[];
    // A built-in kind carries its icon; one norte doesn't know — a plugin's
    // — carries its letter, which is all that's known about it.
    expect(buttons[0]?.querySelector("svg.panelbar-icon")).not.toBeNull();
    expect(buttons[2]?.querySelector("svg")).toBeNull();
    expect(buttons[2]?.querySelector(".panelbar-letter")?.textContent).toBe("G");
    // With no visible text, the NAME is what a screen reader hears.
    expect(buttons[0]?.getAttribute("aria-label")).toBe("Sitios");
    expect(buttons[0]?.title).toBe("Sitios (alt+p)");
    // The count, bounded: a four-digit badge doesn't fit in 48 px.
    expect(buttons[0]?.querySelector(".panelbar-attention")).toBeNull();
    expect(buttons[1]?.querySelector(".panelbar-attention")?.textContent).toBe("7");
    expect(buttons[2]?.querySelector(".panelbar-attention")?.textContent).toBe("99+");
    expect(buttons[2]?.dataset["state"]).toBe("focused");

    buttons[2]?.click();
    expect(sent).toEqual([{ action: "panel_bar_activate", button: 2 }]);

    // And back to row layout returns the width: the reservation follows the
    // bar.
    v.panel_bar.vertical = false;
    screen.paint(v);
    expect(root.getPropertyValue("--activity-w")).toBe("0px");
    expect(root.getPropertyValue("--panelbar-h")).toBe("var(--cell-h)");
  });

  it("the open menu marks its title, its cursor and what can't be done", () => {
    const { screen } = mount();
    screen.paint(withMenu(1));
    const open = document.querySelectorAll('.menubar-title[aria-expanded="true"]');
    expect(open).toHaveLength(1);
    expect(open[0]?.textContent).toBe("Paneles");
    const list = document.querySelector(".menu-items") as HTMLElement;
    expect(list.getAttribute("aria-activedescendant")).toBe("menu-item-1");
    const rows = [...document.querySelectorAll(".menu-item")];
    expect(rows[0]?.textContent).toBe("Cambiar de paneltab");
    // An entry this window doesn't run STILL shows up: the menu is where you
    // see what exists.
    expect(rows[1]?.getAttribute("aria-disabled")).toBe("true");
  });

  it("the dropdown hangs off the PAINTED title, not a count in cells", () => {
    // Titles are painted with pixel padding and don't measure the same: a
    // count at `12ch` per title drifted further the more to the right, and
    // "Ayuda" opened its dropdown one title over. jsdom doesn't do layout, so
    // the title's geometry is faked: what's checked is that the title's
    // measurement is what places the list.
    const { screen } = mount();
    const measure = vi
      .spyOn(HTMLElement.prototype, "getBoundingClientRect")
      .mockImplementation(function (this: HTMLElement): DOMRect {
        const left = this.id === "menu-title-1" ? 123 : 0;
        return new DOMRect(left, 0, 0, 0);
      });
    try {
      screen.paint(withMenu(1));
    } finally {
      measure.mockRestore();
    }
    const list = document.querySelector(".menu-items") as HTMLElement;
    expect(list.style.getPropertyValue("--menu-left")).toBe("123px");
  });

  it("the mouse opens, points, activates and closes", () => {
    const { screen, sent } = mount();
    screen.paint(withMenu(null));
    (document.querySelectorAll(".menubar-title")[1] as HTMLElement).click();
    screen.paint(withMenu(1));
    const row = document.querySelectorAll(".menu-item")[0] as HTMLElement;
    row.dispatchEvent(new MouseEvent("mousemove", { bubbles: true }));
    row.click();
    (document.querySelector(".menu-veil") as HTMLElement).click();
    expect(sent).toEqual([
      { action: "menu_open", menu: 1 },
      { action: "menu_point_row", row: 0 },
      { action: "menu_activate_row", row: 0 },
      { action: "menu_close" },
    ]);
  });
});

describe("the palette", () => {
  function withPalette(cursor: number | null) {
    const v = view({});
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

  it("is modal, says how much it narrows down, and marks the selection", () => {
    const { screen } = mount();
    screen.paint(withPalette(1));
    const box = document.querySelector(".palette") as HTMLElement;
    expect(box.getAttribute("aria-modal")).toBe("true");
    expect(document.querySelector(".palette-count")?.textContent).toBe("2/24");
    const list = document.querySelector(".palette-rows") as HTMLElement;
    expect(list.getAttribute("aria-activedescendant")).toBe("palette-row-1");
    const sel = document.querySelectorAll('.palette-row[aria-selected="true"]');
    expect(sel).toHaveLength(1);
    expect(sel[0]?.textContent).toContain("cursor.down");
  });

  it("every row shows its real shortcut", () => {
    const { screen } = mount();
    screen.paint(withPalette(0));
    const chords = [...document.querySelectorAll(".palette-chord")].map(
      (c) => c.textContent,
    );
    expect(chords).toEqual(["Up", "Down"]);
  });

  it("with no matches it says so instead of staying blank", () => {
    const { screen } = mount();
    const v = withPalette(null);
    if (v.palette !== null) {
      v.palette.rows = [];
    }
    screen.paint(v);
    expect(document.querySelector(".palette-rows .empty")).not.toBeNull();
  });

  it("closed, it covers nothing", () => {
    const { screen } = mount();
    screen.paint(view({}));
    expect(document.querySelector(".palette")).toBeNull();
  });
});

describe("help", () => {
  /** A page with prose, already-resolved marks and one disabled row. */
  function withHelp(): ViewSnapshot {
    const v = view({});
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
      scroll: null,
    };
    return v;
  }

  it("is modal and structures the page with real headings and lists", () => {
    const { screen } = mount();
    screen.paint(withHelp());
    const box = document.querySelector(".help") as HTMLElement;
    expect(box.getAttribute("role")).toBe("dialog");
    expect(box.getAttribute("aria-modal")).toBe("true");
    // The page's title is the `h1`; a heading from the body drops a level,
    // so the hierarchy doesn't have two roots.
    expect(box.querySelectorAll("h1")).toHaveLength(1);
    expect(box.querySelector("h1")?.textContent).toBe("Copiar");
    expect(box.querySelector("h2")?.textContent).toBe("Copiar ficheros");
    expect(box.querySelectorAll(".help-bullets li")).toHaveLength(2);
    expect(box.querySelector("pre code")?.textContent).toBe("norte --help");
    expect(box.querySelectorAll(".help-table th")).toHaveLength(2);
    expect(box.querySelector(".help-callout")?.getAttribute("data-kind")).toBe("warn");
  });

  it("a mark from the corpus arrives as a KEY and never as markup", () => {
    const { screen } = mount();
    screen.paint(withHelp());
    const kbd = document.querySelector(".help-body kbd");
    expect(kbd?.textContent).toBe("F5");
    // Neither an unresolved mark nor a `{{cmd:`: the host converts them.
    expect(document.querySelector(".help")?.textContent).not.toContain("{{cmd:");
  });

  it("NOTHING that arrives is interpreted as HTML", () => {
    const { screen } = mount();
    const v = withHelp();
    if (v.help !== null) {
      // Third-party text: a plugin's `help.md`. If any of this were painted
      // with `innerHTML`, an `<img>` node and an `onerror` attribute would
      // show up here — which is exactly the failure the block vocabulary
      // being closed protects against.
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
    const box = document.querySelector(".help") as HTMLElement;
    expect(box.querySelector("img")).toBeNull();
    expect(box.querySelector("script")).toBeNull();
    expect(box.querySelector("b")).toBeNull();
    expect(box.querySelector("i")).toBeNull();
    // And the text IS there: escaped, not lost.
    expect(box.textContent).toContain("<script>alert(1)</script>");
    expect(box.querySelector("h1")?.textContent).toBe("<img src=x onerror=alert(1)>");
  });

  it("a disabled row says why, and a click on an active one activates it", () => {
    const { screen, sent } = mount();
    screen.paint(withHelp());
    const rows = [...document.querySelectorAll(".help-action")];
    expect(rows).toHaveLength(3);
    expect(rows[1]?.getAttribute("data-enabled")).toBe("false");
    expect(rows[1]?.textContent).toContain("aquí no");

    (rows[0] as HTMLElement).click();
    expect(sent).toEqual([{ action: "help_activate", index: 0 }]);
    // A disabled row sends nothing: the host already said it can't be done.
    (rows[1] as HTMLElement).click();
    expect(sent).toHaveLength(1);
  });

  it("the key sheet explains every key this window doesn't do", () => {
    const { screen } = mount();
    screen.paint(withHelp());
    const rows = [...document.querySelectorAll(".help-keys tbody tr")];
    expect(rows).toHaveLength(2);
    expect(rows[0]?.querySelector("th")?.textContent).toBe("F5");
    expect(rows[1]?.getAttribute("data-enabled")).toBe("false");
    // The reason goes in its OWN cell, not glued to the label: compounded in
    // one run they'd share the same bidi run, and a label ending strongly
    // RTL would drag the separator to the wrong side.
    expect(rows[1]?.querySelector(".help-key-label")?.textContent).toBe("mover");
    expect(rows[1]?.querySelector(".help-key-reason")?.textContent).toBe("aquí no");
  });

  it("every piece of host data inside the prose is its own bidi run", () => {
    const { screen } = mount();
    screen.paint(withHelp());
    // A `<p>` made of spans is ONE run: without isolation, a third party's
    // RTL prose can move the key the sentence says to press out of place,
    // and there's nothing to mask there — they're letters, not controls.
    //
    // Checked over the SHEET as text and not with `getComputedStyle`: jsdom
    // doesn't apply the document's stylesheet, so the computed value would
    // be empty for everything and the test would pass without checking
    // anything. What has to be prevented is the rule disappearing, and that
    // does show up here.
    const css = readFileSync(resolve(process.cwd(), "src/style.css"), "utf8");
    const block = css
      .split("}")
      .find((b) => b.includes("unicode-bidi: isolate") && b.includes(".help-chord"));
    expect(block, "no isolation rule for help").toBeDefined();
    for (const cls of [
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
      expect(block, `${cls} not isolated`).toContain(cls);
    }
    // And the classes really exist in what's painted, not just in the sheet.
    for (const sel of [".help-chord", ".help-key-chord", ".help-action-chord"]) {
      expect(document.querySelector(sel), `missing ${sel}`).not.toBeNull();
    }
  });

  it("the viewer's body paints in logical order, like a terminal", () => {
    // Horizontal truncation is done by the HOST in logical order, once, in
    // the model it shares with the terminal. A browser that reordered by the
    // bidi algorithm would leave the same `first_col` showing different
    // things on the two surfaces — with no mark at all, because a line of
    // Arabic or Hebrew LETTERS carries no controls: nothing gets masked and
    // `had_errors` is false. It's the same tradeoff `must_mask` already makes
    // with legitimate isolates: grid honesty over typography.
    //
    // Over the SHEET as text and for the same reason as help's block: jsdom
    // doesn't apply it, so `getComputedStyle` would check nothing. What has
    // to be prevented is the rule disappearing.
    const css = readFileSync(resolve(process.cwd(), "src/style.css"), "utf8");
    const block = css
      .split("}")
      .find((b) => b.includes(".viewer-body {") && b.includes("unicode-bidi"));
    expect(block, "the viewer's body has no bidi rule").toBeDefined();
    expect(block).toContain("bidi-override");
    expect(block).toContain("direction: ltr");
  });

  it("a link in the prose gets pressed and activates ITS row, without carrying the key", () => {
    const { screen, sent } = mount();
    const v = withHelp();
    if (v.help !== null) {
      v.help.blocks = [
        { block: "paragraph", spans: [{ span: "link", text: "Marcar", action: 1 }] },
      ];
    }
    screen.paint(v);
    const link = document.querySelector(".help-link") as HTMLElement;
    expect(link.getAttribute("role")).toBe("link");
    // The row's index travels, never the target's id.
    expect(link.getAttribute("data-topic")).toBeNull();
    link.click();
    expect(sent).toEqual([{ action: "help_activate", index: 1 }]);
  });

  it("a link with no row is still just text", () => {
    const { screen, sent } = mount();
    const v = withHelp();
    if (v.help !== null) {
      v.help.blocks = [
        { block: "paragraph", spans: [{ span: "link", text: "Marcar", action: null }] },
      ];
    }
    screen.paint(v);
    const link = document.querySelector(".help-link") as HTMLElement;
    expect(link.getAttribute("role")).toBeNull();
    link.click();
    expect(sent).toEqual([]);
  });

  it("the host's scroll request applies ONCE per number", () => {
    // Bridge 76: which key scrolls is decided by the host with the reader's
    // keymap; the renderer measures and scrolls. A patch that repaints help
    // for another reason carries the same request and doesn't repeat it.
    const { screen } = mount();
    const v = withHelp();
    screen.paint(v);
    const body = () => document.querySelector(".help-body") as HTMLElement;
    body().scrollTop = 50;
    if (v.help !== null) {
      v.help = { ...v.help, scroll: { to: "top", seq: 1 } };
    }
    screen.paint(v);
    expect(body().scrollTop, "applied").toBe(0);
    body().scrollTop = 40;
    // Another help patch (a new object, the way the session builds them)
    // with the same request.
    if (v.help !== null) {
      v.help = { ...v.help };
    }
    screen.paint(v);
    expect(body().scrollTop, "the same request doesn't repeat").toBe(40);
    // And a NEW opening numbers again from 1.
    screen.paint(view({}));
    screen.paint(v);
    expect(body().scrollTop, "another opening, another count").toBe(0);
  });

  it("keys are no longer consumed by the renderer: they go to the host", () => {
    const { screen } = mount();
    screen.paint(withHelp());
    // No path is left that handles `PageDown` without asking the keymap.
    expect("helpBodyScrolls" in screen).toBe(false);
  });

  it("a page with three sections or more carries its index up top", () => {
    const { screen } = mount();
    const v = withHelp();
    if (v.help !== null) {
      v.help.blocks = ["Uno", "Dos", "Tres"].flatMap((t) => [
        { block: "heading" as const, level: 1, text: t },
        { block: "paragraph" as const, spans: [{ span: "text" as const, text: "…" }] },
      ]);
    }
    screen.paint(v);
    const buttons = [...document.querySelectorAll(".help-toc-item")].map(
      (b) => b.textContent,
    );
    expect(buttons).toEqual(["Uno", "Dos", "Tres"]);
  });

  it("with fewer than three sections there's no page index", () => {
    const { screen } = mount();
    const v = withHelp();
    if (v.help !== null) {
      v.help.blocks = [{ block: "heading", level: 1, text: "Sola" }];
    }
    screen.paint(v);
    expect(document.querySelector(".help-toc")).toBeNull();
  });

  it("a click on the sidebar requests THAT page", () => {
    const { screen, sent } = mount();
    screen.paint(withHelp());
    const list = document.querySelector(".help-topic-rows") as HTMLElement;
    expect(list.getAttribute("aria-activedescendant")).toBe("help-topic-1");
    const pages = [...document.querySelectorAll(".help-topic")];
    (pages[1] as HTMLElement).click();
    // Row 0 is the group's HEADER: the second page is row 2.
    expect(sent).toEqual([{ action: "help_select_topic", row: 2 }]);
  });

  it("closed, it covers nothing", () => {
    const { screen } = mount();
    screen.paint(view({}));
    expect(document.querySelector(".help")).toBeNull();
  });
});

describe("settings", () => {
  function withSettings(): ViewSnapshot {
    const v = view({});
    v.settings = {
      index: [
        { key: "appearance", title: "Apariencia", visible: 2 },
        // Emptied by the filter: it stays in the index, dimmed.
        { key: "open-with", title: "Abrir con", visible: 0 },
        { key: "paths", title: "Dónde vive cada cosa", visible: 2 },
      ],
      query: "tema",
      shown: 2,
      total: 34,
      focus: "list",
      sections: [
        {
          section: "settings",
          key: "appearance",
          title: "Apariencia",
          rows: [
            {
              id: "ui.confirm-quit",
              name: "Confirmar al salir",
              desc: "Pregunta antes de cerrar norte",
              value: "siempre",
              hostile: false,
              default: "",
              restart_required: true,
              control: "choice",
              choices: ["auto", "siempre", "nunca"],
              min: null,
              max: null,
              modified: false,
            },
            {
              id: "ui.theme",
              name: "Tema",
              desc: "El tema de la ventana",
              value: "nord",
              hostile: false,
              default: "",
              restart_required: false,
              control: "choice",
              choices: ["default", "nord"],
              min: null,
              max: null,
              modified: true,
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

  it("is modal, warns of nothing, and numbers only eligible rows", () => {
    const { screen } = mount();
    screen.paint(withSettings());
    const box = document.querySelector(".settings") as HTMLElement;
    expect(box.getAttribute("aria-modal")).toBe("true");
    // The "doesn't write" note left with bridge 60: this window writes.
    expect(box.querySelector(".settings-note")).toBeNull();
    // Two headers, four rows: the cursor counts rows, not headers.
    expect(box.querySelectorAll(".settings-group")).toHaveLength(2);
    const rows = [...box.querySelectorAll(".settings-row")];
    expect(rows).toHaveLength(4);
    expect(rows.map((f) => f.id)).toEqual([
      "settings-row-0",
      "settings-row-1",
      "settings-row-2",
      "settings-row-3",
    ]);
    const list = box.querySelector(".settings-rows") as HTMLElement;
    expect(list.getAttribute("aria-activedescendant")).toBe("settings-row-1");
  });

  it("paints the index and sends the host the chosen section", () => {
    const { screen, sent } = mount();
    screen.paint(withSettings());
    const index = [...document.querySelectorAll(".settings-index-item")];
    expect(index.map((i) => (i as HTMLElement).dataset["key"])).toEqual([
      "appearance",
      "open-with",
      "paths",
    ]);
    // The one the filter emptied stays, dimmed and unclickable.
    const empty = index[1] as HTMLButtonElement;
    expect(empty.dataset["empty"]).toBe("true");
    expect(empty.disabled).toBe(true);
    (index[0] as HTMLElement).click();
    expect(sent.at(-1)).toEqual({
      action: "settings_jump_section",
      section: "appearance",
    });
  });

  it("the search box sends the text, not a key", () => {
    const { screen, sent } = mount();
    screen.paint(withSettings());
    const box = document.querySelector(".settings-search") as HTMLInputElement;
    expect(box.value).toBe("tema");
    box.value = "fuente";
    box.dispatchEvent(new Event("input", { bubbles: true }));
    expect(sent.at(-1)).toEqual({ action: "settings_query", text: "fuente" });
  });

  it("paints both cursors and dims the one on the side without the keyboard", () => {
    const { screen } = mount();
    const v = withSettings();
    screen.paint(v);
    // With the keyboard in the list: the list live, the index with its
    // section marked but dimmed by CSS.
    const list = document.querySelector(".settings-rows") as HTMLElement;
    const nav = document.querySelector(".settings-index") as HTMLElement;
    expect(list.dataset["focused"]).toBe("true");
    expect(nav.dataset["focused"]).toBe("false");
    // The cursor (row 1) lands on Apariencia, and the index says so by KEY.
    const marked = nav.querySelector('[aria-current="true"]') as HTMLElement;
    expect(marked.dataset["key"]).toBe("appearance");

    if (v.settings !== null) {
      v.settings = { ...v.settings, focus: "index" };
    }
    screen.paint(v);
    const list2 = document.querySelector(".settings-rows") as HTMLElement;
    const nav2 = document.querySelector(".settings-index") as HTMLElement;
    expect(list2.dataset["focused"]).toBe("false");
    expect(nav2.dataset["focused"]).toBe("true");
    // And the cursor's row is still marked: it always paints, dimmed.
    expect(list2.querySelector('[aria-selected="true"]')).not.toBeNull();
  });

  it("a dropdown sends the chosen value, not a cycle", () => {
    const { screen, sent } = mount();
    screen.paint(withSettings());
    const sel = document.querySelector("#settings-row-1 select") as HTMLSelectElement;
    expect([...sel.options].map((o) => o.value)).toEqual(["default", "nord"]);
    expect(sel.value).toBe("nord");
    sel.value = "default";
    sel.dispatchEvent(new Event("change", { bubbles: true }));
    // By ID and at once: `settings_activate` would have cycled.
    expect(sent.at(-1)).toEqual({
      action: "settings_set",
      id: "ui.theme",
      value: "default",
    });
  });

  /// A value the file carries that the list no longer recognizes — a deleted
  /// theme — gets ADDED to the dropdown: showing something else instead of
  /// what's actually set would be lying about the configuration.
  it("a value the list doesn't recognize is still there in the dropdown", () => {
    const { screen } = mount();
    const v = withSettings();
    if (v.settings !== null) {
      const sec = v.settings.sections[0];
      if (sec?.section === "settings") {
        const row = sec.rows[1];
        if (row !== undefined) {
          row.value = "un-tema-que-borre";
        }
      }
    }
    screen.paint(v);
    const sel = document.querySelector("#settings-row-1 select") as HTMLSelectElement;
    expect(sel.value).toBe("un-tema-que-borre");
    expect([...sel.options].map((o) => o.value)).toContain("un-tema-que-borre");
  });

  it("a toggle states its status and sends the opposite", () => {
    const { screen, sent } = mount();
    const v = withSettings();
    if (v.settings !== null) {
      const sec = v.settings.sections[0];
      if (sec?.section === "settings") {
        sec.rows = [
          {
            id: "ui.mouse",
            name: "Ratón",
            desc: "Captura el ratón",
            value: "true",
            hostile: false,
            default: "",
            restart_required: false,
            control: "toggle",
            choices: [],
            min: null,
            max: null,
            modified: false,
          },
        ];
      }
    }
    screen.paint(v);
    const sw = document.querySelector('[role="switch"]') as HTMLButtonElement;
    expect(sw.getAttribute("aria-checked")).toBe("true");
    sw.click();
    expect(sent.at(-1)).toEqual({
      action: "settings_set",
      id: "ui.mouse",
      value: "false",
    });
  });

  /// A field saves on BLUR, not on every key: every keystroke would be a
  /// write to `norte.toml` and a reload of the whole configuration.
  it("a text field saves on blur and gives up on escape", () => {
    const { screen, sent } = mount();
    const v = withSettings();
    if (v.settings !== null) {
      const sec = v.settings.sections[0];
      if (sec?.section === "settings") {
        sec.rows = [
          {
            id: "ui.editor",
            name: "Editor",
            desc: "Con qué se abre un fichero",
            value: "zed %f",
            hostile: false,
            default: "",
            restart_required: false,
            control: "args",
            choices: [],
            min: null,
            max: null,
            modified: true,
          },
        ];
      }
    }
    screen.paint(v);
    const field = document.querySelector(".settings-text") as HTMLInputElement;
    field.value = "vim";
    field.dispatchEvent(new KeyboardEvent("keydown", { key: "a", bubbles: true }));
    expect(sent.at(-1)).not.toMatchObject({ action: "settings_set" });
    field.dispatchEvent(new FocusEvent("blur"));
    expect(sent.at(-1)).toEqual({
      action: "settings_set",
      id: "ui.editor",
      value: "vim",
    });
  });

  /// An empty field says WHAT the factory value is, not a sentence about
  /// there being one: the placeholder takes the data's spot, so it has to be
  /// the data.
  it("an empty field shows the factory value as a placeholder", () => {
    const { screen } = mount();
    const v = withSettings();
    if (v.settings !== null) {
      const sec = v.settings.sections[0];
      if (sec?.section === "settings") {
        sec.rows = [
          {
            id: "ui.font",
            name: "Tipografía de la interfaz",
            desc: "La del cromo",
            value: "",
            default: "Inter",
            hostile: false,
            restart_required: true,
            control: "text",
            choices: [],
            min: null,
            max: null,
            modified: false,
          },
        ];
      }
    }
    screen.paint(v);
    const field = document.querySelector(".settings-text") as HTMLInputElement;
    expect(field.value).toBe("");
    expect(field.placeholder).toBe("Inter");
  });

  it("the search box keeps focus and the caret across repaints", () => {
    const { screen } = mount();
    screen.paint(withSettings());
    const box = document.querySelector(".settings-search") as HTMLInputElement;
    box.focus();
    expect(document.activeElement).toBe(box);
    box.value = "fue";
    box.setSelectionRange(3, 3);
    // The host answers every key with a patch, i.e. a repaint. If the field
    // were recreated, the reader couldn't type more than one letter.
    screen.paint(withSettings());
    const after = document.querySelector(".settings-search") as HTMLInputElement;
    expect(after).toBe(box);
    expect(document.activeElement).toBe(after);
    expect(after.value).toBe("fue");
    expect(after.selectionStart).toBe(3);
  });

  /// Every row carries EXACTLY four cells, in the same order, whether it has
  /// a dot and actions or not. The grid has four columns: one extra child
  /// sends the overflow to a new row, and that's how the reset button used
  /// to come out as a full-width box.
  it("every row carries the same four cells, in the same order", () => {
    const { screen } = mount();
    screen.paint(withSettings());
    for (const row of document.querySelectorAll(".settings-row")) {
      const cells = [...row.children].filter(
        (c) => !c.classList.contains("settings-desc"),
      );
      expect(cells.map((c) => c.className)).toEqual([
        "settings-dot",
        "settings-name",
        "settings-value",
        "settings-actions",
      ]);
    }
    // And what used to spill out of the row now lives INSIDE the actions.
    const touched = document.querySelector("#settings-row-1") as HTMLElement;
    expect(touched.querySelector(".settings-actions .settings-reset")).not.toBeNull();
  });

  it("a touched row carries a dot and a reset button", () => {
    const { screen, sent } = mount();
    screen.paint(withSettings());
    const rows = [...document.querySelectorAll(".settings-row")];
    // The first is at its factory value; the second isn't.
    expect(rows[0]?.querySelector(".settings-reset")).toBeNull();
    const button = rows[1]?.querySelector(".settings-reset") as HTMLButtonElement;
    expect(rows[1]?.querySelector(".settings-dot")?.getAttribute("aria-label")).toBe(
      realCatalog()["settings-modified"] ?? "",
    );
    button.click();
    expect(sent.at(-1)).toEqual({ action: "settings_reset", row: 1 });
  });

  it("keeps the scroll position on repaint", () => {
    const { screen } = mount();
    screen.paint(withSettings());
    const list = document.querySelector(".settings-rows") as HTMLElement;
    // jsdom doesn't do layout, so `scrollTop` is only preserved if the
    // renderer copies it by hand — which is exactly what's checked.
    Object.defineProperty(list, "scrollTop", { value: 120, writable: true });
    screen.paint(withSettings());
    const after = document.querySelector(".settings-rows") as HTMLElement;
    expect(after.scrollTop).toBe(120);
  });

  it("the section header reveals itself along with its first row", () => {
    const { screen } = mount();
    const v = withSettings();
    if (v.settings !== null) {
      v.settings.cursor = 0;
    }
    screen.paint(v);
    const list = document.querySelector(".settings-rows") as HTMLElement;
    // Row 0: opens "Apariencia", so what scrolls into view is the HEADER.
    // Revealing just the row left the label outside the box, which is how
    // the label went out of sight when scrolling back up.
    expect(revealTarget(list, 0)?.className).toBe("settings-group");
    expect(revealTarget(list, 0)?.textContent).toContain("Apariencia");
    // Row 1: opens nothing, it reveals itself.
    expect(revealTarget(list, 1)?.id).toBe("settings-row-1");
    // Row 2: opens the paths section, same rule as 0.
    expect(revealTarget(list, 2)?.className).toBe("settings-group");
  });

  it("a missing location says so, and a hostile one is marked", () => {
    const { screen } = mount();
    const v = withSettings();
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
    // The first two are settings; the paths come after.
    const rows = [...document.querySelectorAll(".settings-row")];
    expect(rows[2]?.querySelector(".settings-value")?.getAttribute("data-hostile")).toBe(
      "true",
    );
    expect(rows[3]?.querySelector(".settings-missing")).not.toBeNull();
    // The one that's present isn't marked as missing.
    expect(rows[2]?.querySelector(".settings-missing")).toBeNull();
  });

  it("if the whole section needs a restart, it's said once and not five times", () => {
    const { screen } = mount();
    const v = withSettings();
    if (v.settings !== null) {
      const sec = v.settings.sections[0];
      if (sec?.section === "settings") {
        // ALL of them: the fixture's row that didn't ask for it too.
        for (const r of sec.rows) {
          r.restart_required = true;
        }
      }
    }
    screen.paint(v);
    const header = document.querySelector(".settings-group");
    expect(header?.textContent).toContain(realCatalog()["settings-restart-badge"] ?? "");
    // And no row repeats it.
    expect(document.querySelectorAll(".settings-row .settings-badge")).toHaveLength(0);
  });

  it("if only some ask for it, the badge goes on the row", () => {
    const { screen } = mount();
    const v = withSettings();
    if (v.settings !== null) {
      const sec = v.settings.sections[0];
      if (sec?.section === "settings") {
        sec.rows.push({
          id: "ui.lang",
          name: "Idioma",
          desc: "El idioma de la ventana",
          value: "auto",
          hostile: false,
          default: "",
          restart_required: false,
          control: "text",
          choices: [],
          min: null,
          max: null,
          modified: false,
        });
      }
    }
    screen.paint(v);
    // In that row's DESCRIPTION, not as a pill: repeated as a label on six
    // rows at once it stopped being readable, and it only matters when that
    // row is touched.
    const when = [...document.querySelectorAll(".settings-desc .settings-when")];
    expect(when).toHaveLength(1);
    expect(when[0]?.textContent).toBe(realCatalog()["settings-restart-badge"] ?? "");
    expect(document.querySelectorAll(".settings-row .settings-badge")).toHaveLength(0);
    expect(document.querySelector(".settings-group")?.textContent).not.toContain(
      realCatalog()["settings-restart-badge"] ?? "",
    );
  });

  it("a click requests THAT row, counting past the headers", () => {
    const { screen, sent } = mount();
    screen.paint(withSettings());
    const rows = [...document.querySelectorAll(".settings-row")];
    (rows[2] as HTMLElement).click();
    expect(sent).toEqual([{ action: "settings_select_row", row: 2 }]);
  });

  it("a double click ACTIVATES that row, which is what enter does", () => {
    const { screen, sent } = mount();
    screen.paint(withSettings());
    const rows = [...document.querySelectorAll(".settings-row")];
    (rows[0] as HTMLElement).dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    expect(sent).toEqual([{ action: "settings_activate", row: 0 }]);
  });

  it("closed, they cover nothing", () => {
    const { screen } = mount();
    screen.paint(view({}));
    expect(document.querySelector(".settings")).toBeNull();
  });
});

describe("the extensions manager", () => {
  function withExtensions(): ViewSnapshot {
    const v = view({});
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

  it("shows status as TWO facts and the capabilities on the row", () => {
    const { screen } = mount();
    screen.paint(withExtensions());
    const rows = [...document.querySelectorAll(".extensions-row")];
    expect(rows).toHaveLength(2);
    const one = rows[0]?.querySelector(".extensions-state");
    expect(one?.getAttribute("data-approved")).toBe("true");
    expect(one?.getAttribute("data-enabled")).toBe("true");
    const two = rows[1]?.querySelector(".extensions-state");
    expect(two?.getAttribute("data-approved")).toBe("false");
    // Capabilities are NOT hidden behind a gesture: they're the decision.
    expect(rows[0]?.querySelectorAll(".extensions-cap")).toHaveLength(2);
  });

  it("the detail pane says which one is selected and counts installed and enabled", () => {
    const { screen } = mount();
    screen.paint(withExtensions());
    expect(document.querySelector(".extensions-pane-name")?.textContent).toBe(
      "FTP de ACME",
    );
    // Two counts: "2 installed · 1 enabled", with no Fluent placeholders.
    const summary = document.querySelector(".extensions-summary")?.textContent ?? "";
    expect(summary).toContain("2 ");
    expect(summary).toContain("1 ");
    expect(summary).not.toContain("$");
    // With no pane selected, it says how to select one instead of leaving a
    // gap.
    expect(document.querySelector(".extensions-detail-hint")).not.toBeNull();
  });

  it("buttons say what they're going to do and send the chosen row's action", () => {
    const { screen, sent } = mount();
    screen.paint(withExtensions());
    const actions = document.querySelector(".extensions-actions") as HTMLElement;
    // The selected one is approved and enabled: revoke, disable, help, and
    // uninstall; never "toggle".
    const labels = [...actions.querySelectorAll("button")].map((b) => b.textContent);
    expect(labels).toEqual([
      realCatalog()["ext-revoke"],
      realCatalog()["ext-disable"],
      realCatalog()["ext-help"],
      realCatalog()["ext-uninstall"],
    ]);
    (actions.querySelector(".extensions-action-uninstall") as HTMLButtonElement).click();
    (actions.querySelector(".extensions-action-enabled") as HTMLButtonElement).click();
    (actions.querySelector(".extensions-action-help") as HTMLButtonElement).click();
    expect(sent).toEqual([
      { action: "extension_govern", row: 0, id: "acme.ftp", change: "uninstall" },
      { action: "extension_govern", row: 0, id: "acme.ftp", change: "enabled" },
      { action: "extension_help", row: 0, id: "acme.ftp" },
    ]);
    // Uninstall paints as what it is, and clicking a button doesn't reselect
    // the row.
    expect(
      actions
        .querySelector(".extensions-action-uninstall")
        ?.getAttribute("data-destructive"),
    ).toBe("true");
    expect(sent.some((a) => a.action === "extension_select_row")).toBe(false);
  });

  it("a broken one is one more row: it's flagged and its pane only offers uninstall", () => {
    const { screen, sent } = mount();
    const v = withExtensions();
    if (v.extensions !== null) {
      v.extensions.errors = [
        {
          dir: "acme.roto",
          hostile: false,
          reason: "el manifiesto no parsea",
          reason_hostile: false,
          id: "acme.roto",
        },
        {
          dir: "no un id",
          hostile: false,
          reason: "el manifiesto no parsea",
          reason_hostile: false,
          id: null,
        },
      ];
      // Broken ones go after the two loaded ones.
      v.extensions = { ...v.extensions, cursor: 2 };
    }
    screen.paint(v);
    const broken = [...document.querySelectorAll(".extensions-error")];
    expect(broken).toHaveLength(2);
    expect(broken[0]?.getAttribute("aria-selected")).toBe("true");
    (broken[1] as HTMLElement).click();
    expect(sent).toEqual([{ action: "extension_select_row", row: 3 }]);
    sent.length = 0;

    const actions = document.querySelector(".extensions-actions") as HTMLElement;
    const labels = [...actions.querySelectorAll("button")].map((b) => b.textContent);
    expect(labels).toEqual([realCatalog()["ext-uninstall"]]);
    (actions.querySelector(".extensions-action-uninstall") as HTMLButtonElement).click();
    expect(sent).toEqual([
      { action: "extension_govern", row: 2, id: "acme.roto", change: "uninstall" },
    ]);

    // With no id there's no button: it says why, with the host's phrasing.
    if (v.extensions !== null) {
      v.extensions = { ...v.extensions, cursor: 3 };
    }
    screen.paint(v);
    expect(document.querySelector(".extensions-action-uninstall")).toBeNull();
    expect(document.querySelector(".extensions-pane")?.textContent).toContain(
      realCatalog()["ext-broken-not-id"],
    );
  });

  it("broken ones are their own listbox: the cursor on one gets announced", () => {
    const { screen } = mount();
    const v = withExtensions();
    if (v.extensions !== null) {
      v.extensions.errors = [
        {
          dir: "acme.roto",
          hostile: false,
          reason: "el manifiesto no parsea",
          reason_hostile: false,
          id: "acme.roto",
        },
      ];
      v.extensions = { ...v.extensions, cursor: 2 };
    }
    screen.paint(v);
    const errors = document.querySelector(".extensions-errors") as HTMLElement;
    // Without the `listbox` containing them, each row's `role="option"` is
    // invalid ARIA and a screen reader announces nothing on arrival.
    expect(errors.getAttribute("role")).toBe("listbox");
    expect(errors.getAttribute("aria-activedescendant")).toBe("extension-row-2");
    // And the loaded list drops its own: the cursor isn't there anymore.
    expect(
      document.querySelector(".extensions-rows")?.getAttribute("aria-activedescendant"),
    ).toBeNull();

    if (v.extensions !== null) {
      v.extensions = { ...v.extensions, cursor: 0 };
    }
    screen.paint(v);
    expect(
      document.querySelector(".extensions-errors")?.getAttribute("aria-activedescendant"),
    ).toBeNull();
    expect(
      document.querySelector(".extensions-rows")?.getAttribute("aria-activedescendant"),
    ).toBe("extension-row-0");
  });

  it("on an unapproved one, approve is the primary button and enable isn't offered", () => {
    const { screen, sent } = mount();
    const v = withExtensions();
    if (v.extensions !== null) {
      v.extensions = { ...v.extensions, cursor: 1 };
    }
    screen.paint(v);
    const actions = document.querySelector(".extensions-actions") as HTMLElement;
    const approve = actions.querySelector(
      ".extensions-action-approval",
    ) as HTMLButtonElement;
    expect(approve.textContent).toBe(realCatalog()["ext-approve"]);
    expect(approve.getAttribute("data-primary")).toBe("true");
    const enable = actions.querySelector(
      ".extensions-action-enabled",
    ) as HTMLButtonElement;
    expect(enable.disabled).toBe(true);
    // And says why, with the phrasing the host would answer with.
    expect(enable.title).toBe(realCatalog()["host-extension-not-approved"]);
    // With no help page, no help button.
    expect(actions.querySelector(".extensions-action-help")).toBeNull();
    approve.click();
    expect(sent).toEqual([
      { action: "extension_govern", row: 1, id: "org.norte.demo", change: "approval" },
    ]);
  });

  it("closing sends the same key that closes", () => {
    const { screen, sent } = mount();
    screen.paint(withExtensions());
    (document.querySelector(".extensions-close") as HTMLButtonElement).click();
    expect(sent).toHaveLength(1);
    expect(sent[0]).toMatchObject({ action: "key", key: "Escape" });
  });

  it('"loading" doesn\'t paint the same as "none"', () => {
    const { screen } = mount();
    const v = withExtensions();
    if (v.extensions !== null) {
      v.extensions.loading = true;
      v.extensions.rows = [];
    }
    screen.paint(v);
    expect(document.querySelector(".extensions-note")?.getAttribute("role")).toBe(
      "status",
    );
    expect(document.querySelector(".extensions-note")?.textContent).toBe(
      realCatalog()["ext-loading"] ?? "",
    );

    const empty = withExtensions();
    if (empty.extensions !== null) {
      empty.extensions.loading = false;
      empty.extensions.rows = [];
    }
    screen.paint(empty);
    expect(document.querySelector(".extensions-note")?.textContent).toBe(
      realCatalog()["ext-empty"] ?? "",
    );
  });

  it("the detail pane marks the value that's no longer the schema's", () => {
    const { screen } = mount();
    const v = withExtensions();
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
    const rows = [...document.querySelectorAll(".extensions-config tbody tr")];
    expect(rows).toHaveLength(3);
    expect(rows[0]?.getAttribute("data-changed")).toBe("true");
    expect(rows[1]?.getAttribute("data-changed")).toBe("false");
    // The value the PLUGIN writes and that paints differently from what it
    // is carries its badge, same as a file name.
    const value = rows[2]?.querySelector(".extensions-key-value");
    expect(value?.getAttribute("data-hostile")).toBe("true");
    expect(value?.querySelector(".hostile-badge")).not.toBeNull();
    expect(rows[0]?.querySelector(".hostile-badge")).toBeNull();
    // And the type and its domain go in SEPARATE nodes: joining them into
    // one lets an `enum` value with RTL letters reorder the whole pair.
    expect(rows[2]?.querySelector(".extensions-key-domain")?.textContent).toBe(
      "safe · fast\uFFFD",
    );
    expect(rows[0]?.querySelector(".extensions-key-kind")?.textContent).toContain(
      "entre 1 y 300",
    );
    // The detail pane says WHOSE it is: with the list scrolled, the chosen
    // row might not be in view.
    expect(document.querySelector(".extensions-detail-of")?.textContent).toBe(
      "FTP de ACME",
    );
  });

  it("a directory that didn't load says so, and a hostile one is marked", () => {
    const { screen } = mount();
    const v = withExtensions();
    if (v.extensions !== null) {
      v.extensions.errors = [
        {
          dir: "/plugins/ro�to",
          hostile: true,
          reason: "el manifiesto no parsea",
          reason_hostile: false,
          id: null,
        },
        // The REASON quotes the plugin's manifest, so it carries its own
        // mark: one for both strings would leave the reader unable to tell
        // which of them is altered.
        {
          dir: "/plugins/otro",
          hostile: false,
          reason: "clave desconocida: mo�do",
          reason_hostile: true,
          id: null,
        },
      ];
    }
    screen.paint(v);
    const err = document.querySelector(".extensions-error-dir");
    expect(err?.getAttribute("data-hostile")).toBe("true");
    expect(document.querySelector(".extensions-error-reason")?.textContent).toBe(
      "el manifiesto no parsea",
    );
    const reasons = [...document.querySelectorAll(".extensions-error-reason")];
    expect(reasons[0]?.getAttribute("data-hostile")).toBe("false");
    expect(reasons[1]?.getAttribute("data-hostile")).toBe("true");
    expect(reasons[1]?.querySelector(".hostile-badge")).not.toBeNull();
  });

  it("a click selects THAT extension", () => {
    const { screen, sent } = mount();
    screen.paint(withExtensions());
    const rows = [...document.querySelectorAll(".extensions-row")];
    (rows[1] as HTMLElement).click();
    expect(sent).toEqual([{ action: "extension_select_row", row: 1 }]);
  });

  it("closed, it covers nothing", () => {
    const { screen } = mount();
    screen.paint(view({}));
    expect(document.querySelector(".extensions")).toBeNull();
  });
});

describe("the profile selector", () => {
  function withProfiles() {
    const v = view({});
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

  it("marks the active one, the cursor, and SHOWS the one that fails to load", () => {
    const { screen } = mount();
    screen.paint(withProfiles());
    const rows = [...document.querySelectorAll(".profiles-row")];
    expect(rows).toHaveLength(3);
    expect(rows[0]?.getAttribute("data-active")).toBe("true");
    expect(rows[1]?.getAttribute("aria-selected")).toBe("true");
    // A broken row doesn't disappear: it's shown with its reason.
    expect(rows[2]?.getAttribute("data-broken")).toBe("true");
    expect(rows[2]?.textContent).toContain("falta");
    // And the name clash is stated, or it would be a trap.
    expect(rows[1]?.textContent).toContain("preset de teclado");
  });

  it("a click activates it, with the generation it was painted with", () => {
    const { screen, sent } = mount();
    screen.paint(withProfiles());
    (document.querySelectorAll(".profiles-row")[1] as HTMLElement).click();
    expect(sent).toEqual([{ action: "profile_activate_row", row: 1, generation: 7 }]);
  });
});

describe("the theme and the selector", () => {
  it("every role is SEEN, not just read as hex", () => {
    const { screen } = mount();
    const v = view({});
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
    const rows = [...document.querySelectorAll(".theme-role")];
    expect(rows).toHaveLength(2);
    const swatch = rows[0]?.querySelector(".theme-swatch") as HTMLElement;
    // The swatch IS the data: a `#2d4f8a` says nothing until it's seen.
    expect(swatch.style.backgroundColor).not.toBe("");
    expect(rows[0]?.querySelector(".theme-role-hex")?.textContent).toBe("#2d4f8a");
    // And effects this window doesn't paint get NAMED.
    const notice = document.querySelector(".theme-effects");
    expect(notice?.getAttribute("role")).toBe("note");
    expect(notice?.textContent).toContain("crt");
    expect(notice?.textContent).toContain("scanlines");
  });

  it("a theme with no effects doesn't paint the notice", () => {
    const { screen } = mount();
    const v = view({});
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

  it("the theme list marks the one under the cursor", () => {
    const { screen } = mount();
    const v = view({});
    v.theme = {
      name: "retro",
      roles: [{ role: "fg", color: "#d4d8de" }],
      unsupported_effects: [],
      choices: ["default", "retro", "nord"],
      cursor: 1,
    };
    screen.paint(v);
    const list = document.querySelector(".theme-choices") as HTMLElement;
    expect(list.getAttribute("aria-activedescendant")).toBe("theme-choice-1");
    const marked = document.querySelectorAll('.theme-choice[aria-selected="true"]');
    expect(marked).toHaveLength(1);
    expect(marked[0]?.textContent).toBe("retro");
  });

  it("the picker marks a hostile mount and says why it's empty", () => {
    const { screen, sent } = mount();
    const v = view({});
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
    const rows = [...document.querySelectorAll(".picker-row")];
    expect(rows).toHaveLength(2);
    expect(rows[1]?.querySelector(".picker-label")?.getAttribute("data-hostile")).toBe(
      "true",
    );
    const list = document.querySelector(".picker-rows") as HTMLElement;
    expect(list.getAttribute("aria-activedescendant")).toBe("picker-row-1");
    (rows[0] as HTMLElement).click();
    expect(sent).toEqual([{ action: "picker_select_row", row: 0, generation: 1 }]);

    const empty = view({});
    empty.picker = {
      title: "Volúmenes",
      rows: [],
      cursor: null,
      generation: 1,
      empty: "preguntando al host…",
    };
    screen.paint(empty);
    const note = document.querySelector(".picker-empty");
    expect(note?.getAttribute("role")).toBe("status");
    expect(note?.textContent).toBe("preguntando al host…");
  });

  it("closed, they cover nothing", () => {
    const { screen } = mount();
    screen.paint(view({}));
    expect(document.querySelector(".theme")).toBeNull();
    expect(document.querySelector(".picker")).toBeNull();
  });
});

describe("slots that aren't listings", () => {
  it("the attribute sheet paints label and value, and marks a hostile name", () => {
    const { screen } = mount();
    const v = view({});
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
    const fields = [...document.querySelectorAll(".metadata-fields dt")];
    expect(fields.map((d) => d.textContent)).toEqual(["Nombre", "Tamaño"]);
    const values = [...document.querySelectorAll(".metadata-fields dd")];
    expect(values[0]?.getAttribute("data-hostile")).toBe("true");
    expect(values[1]?.getAttribute("data-hostile")).toBe("false");
  });

  // "Details" on its own doesn't say WHAT the details are of: with two
  // listings open, the only way to know which one was being described was
  // to move the cursor and see if the sheet moved.
  it("the sheet titles itself with the listing it follows", () => {
    const { screen } = mount();
    const v = view({});
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
    const title = document.querySelector('[data-slot-id="7"] .slot-title');
    expect(title?.textContent).toContain("⟨file⟩/home/oscar/Downloads");
    // The path goes in ITS OWN node: `.slot-title` truncates with no
    // ellipsis, and a path cut off flat names another directory that
    // actually exists too.
    expect(
      document.querySelector('[data-slot-id="7"] .slot-title .title-path')?.textContent,
    ).toBe("⟨file⟩/home/oscar/Downloads");
  });

  // #291: the docked viewer is the SAME body as the big one, in a slot.
  it("the docked viewer paints the lines of the file under the cursor with its path", () => {
    const { screen } = mount();
    const v = view({});
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
          image_zoom: 100,
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
    const slot = document.querySelector(".preview") as HTMLElement;
    expect(slot.querySelector(".viewer-via")?.textContent).toBe("via Markdown");
    const lines = [...slot.querySelectorAll(".viewer-line")];
    expect(lines).toHaveLength(2);
    expect(lines[0]?.querySelector(".viewer-span")?.getAttribute("data-role")).toBe(
      "title",
    );
    const second = lines[1]?.querySelector(".viewer-span") as HTMLElement;
    expect(second.style.color).toBe("rgb(255, 0, 0)");
    expect(second.style.backgroundColor).toBe("rgb(0, 0, 64)");
    // And the BIG viewer didn't open: it's a slot, not an overlay.
    expect(document.querySelector('[role="document"]')).toBeNull();
  });

  it("the wheel over the docked viewer scrolls through the HOST", () => {
    const { screen, sent } = mount();
    const v = view({});
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
          image_zoom: 100,
          styled: [],
        },
      },
    ];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 60, y: 0, width: 60, height: 38, role: null, focus_index: 2 },
    ];
    screen.paint(v);
    const box = document.querySelector(".preview") as HTMLElement;
    box.dispatchEvent(new WheelEvent("wheel", { deltaY: 120, bubbles: true }));
    expect(sent.at(-1)).toEqual({ action: "preview_scroll", slot_id: 7, delta: 3 });
  });

  it("the docked viewer with no file SAYS why", () => {
    const { screen } = mount();
    const v = view({});
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

  it("the sheet with nothing under the cursor SAYS SO", () => {
    const { screen } = mount();
    const v = view({});
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

  it("the processes panel marks its cursor over the SAME tasks", () => {
    const { screen } = mount();
    const v = view({});
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
    const rows = [...document.querySelectorAll(".processes-row")];
    expect(rows).toHaveLength(2);
    expect(rows[1]?.getAttribute("aria-selected")).toBe("true");
    const list = document.querySelector(".processes-rows") as HTMLElement;
    expect(list.getAttribute("aria-activedescendant")).toBe("process-row-1");
  });

  it("with no tasks, the panel says so instead of staying blank", () => {
    const { screen } = mount();
    const v = view({});
    v.slots = [...v.slots, { kind: "processes", slot_id: 7, cursor: null }];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 0, y: 0, width: 40, height: 10, role: null, focus_index: 2 },
    ];
    screen.paint(v);
    expect(document.querySelector(".processes .slot-note")?.textContent).toBe(
      realCatalog()["processes-empty"] ?? "",
    );
  });
});

describe("a plugin's panel", () => {
  function withPanel(
    lines: PanelSlotView["lines"],
    hits: PanelSlotView["hits"],
  ): ViewSnapshot {
    const v = view({});
    v.slots = [...v.slots, { kind: "panel", slot_id: 7, title: "status", lines, hits }];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 7, x: 0, y: 0, width: 40, height: 10, role: null, focus_index: 2 },
    ];
    return v;
  }

  it("paints what the guest described, with the panel's title", () => {
    const { screen } = mount();
    screen.paint(
      withPanel([[{ text: "rama main", role: null, fg: null, bg: null }]], []),
    );
    expect(document.querySelector(".panel-line")?.textContent).toBe("rama main");
  });

  // A frame that hasn't arrived yet — the first request in flight, or a
  // plugin that failed — leaves the slot with its border and title: it's
  // known that the panel is there and whose it is, instead of a mute slot.
  it("with no frame yet, the slot is still theirs", () => {
    const { screen } = mount();
    screen.paint(withPanel([], []));
    expect(document.querySelectorAll(".panel-line")).toHaveLength(0);
    expect(document.querySelector(".panel-plugin")).not.toBeNull();
  });

  // What travels is the CELL. The host resolves the command against the
  // frame it has, with the same filter as the terminal: if the command
  // crossed the wire, anyone talking to the renderer could send it.
  it("a hit zone sends the clicked cell, never a command", () => {
    const { screen, sent } = mount();
    screen.paint(
      withPanel(
        [[{ text: "rama main", role: null, fg: null, bg: null }]],
        [{ row: 0, col: 5, width: 4 }],
      ),
    );
    const zone = document.querySelector(".panel-hit") as HTMLElement;
    expect(zone).not.toBeNull();
    zone.click();
    expect(sent.at(-1)).toEqual({
      action: "panel_click",
      slot_id: 7,
      row: 0,
      col: 5,
    });
  });
});

describe("the log panel", () => {
  function withLog(extra: Partial<LogSlotView> = {}): ViewSnapshot {
    const v = view({});
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

  it("paints the lines and colors by LEVEL, not by position", () => {
    const { screen } = mount();
    screen.paint(withLog());
    const rows = [...document.querySelectorAll(".log-line")];
    expect(rows).toHaveLength(2);
    // The level colors the whole line: reading a log means hunting for
    // errors, and a color only on the label doesn't show at a glance.
    expect((rows[0] as HTMLElement).dataset["level"]).toBe("error");
    expect((rows[1] as HTMLElement).dataset["level"]).toBe("info");
    expect(rows[0]?.textContent).toContain("no se pudo conectar");
  });

  it("the level is PAINTED with the label and COMPARED by identity", () => {
    const { screen } = mount();
    const v = withLog();
    const log = v.slots.find((s) => s.kind === "log");
    if (log?.kind !== "log") {
      throw new Error("the fixture carries the log panel");
    }
    log.level_label = "INFO";
    log.lines[0]!.level_label = "ERROR";
    log.lines[1]!.level_label = "INFO";
    screen.paint(v);

    // The window painted `error` on the line, `info` on the title's chip and
    // "info" translated on the buttons: three vocabularies for the same
    // level, all three on screen at once. What's read is the label, which is
    // the one the terminal paints and the one written into `RUST_LOG`.
    const labels = [...document.querySelectorAll(".log-level")].map((n) => n.textContent);
    expect(labels).toEqual(["ERROR", "INFO"]);
    // And the IDENTITY is still the wire one: it's what colors the line and
    // what marks which button is pressed, and translating it would break
    // both.
    const rows = [...document.querySelectorAll(".log-line")];
    expect((rows[0] as HTMLElement).dataset["level"]).toBe("error");
  });

  it("says which PROCESS the lines belong to", () => {
    // The window starts its own daemon, so there's NO daemon entry here.
    // Staying silent about it would make the panel look broken: someone
    // opens it while a connection fails and can't find the line that
    // explains it.
    const { screen } = mount();
    screen.paint(withLog());
    expect(document.body.textContent).toContain("de esta ventana");
  });

  it("says when it's DETACHED from the end and when it dropped lines", () => {
    // "Nothing's wrong" and "you've detached and this is history" are
    // indistinguishable without saying so; and a log with a silent gap lies
    // about what happened.
    const { screen } = mount();
    screen.paint(
      withLog({ following: false, dropped_note: "17 líneas viejas descartadas" }),
    );
    // The log slot's OWN header: there are several on screen, and the first
    // one is the listing's.
    const box = document.querySelector(".log");
    const header = box?.parentElement?.querySelector(".slot-title")?.textContent ?? "";
    expect(header).toContain(realCatalog()["log-detached"] ?? "");
    expect(header).toContain("17 líneas viejas descartadas");
  });

  it("the SET level is marked, and pressing another requests it by its wire id", () => {
    const { screen, sent } = mount();
    screen.paint(withLog());
    const buttons = [...document.querySelectorAll(".log-controls button")];
    const set = buttons.find((b) => (b as HTMLElement).dataset["on"] === "true");
    expect(set?.textContent).toBe(realCatalog()["log-level-info"] ?? "");

    const debug = buttons.find(
      (b) => b.textContent === (realCatalog()["log-level-debug"] ?? ""),
    );
    (debug as HTMLButtonElement).click();
    const last = sent.at(-1);
    expect(last?.action).toBe("log_set_level");
    if (last?.action === "log_set_level") {
      // The WIRE identifier, not the translated label: comparing translated
      // phrases would tie the level to the language.
      expect(last.level).toBe("debug");
    }
  });

  it("with no second source there's NO selector to press", () => {
    // A dial between three views of the same ring promises something that
    // doesn't exist: this window's daemon might not serve its log, and then
    // the source is a label, not a button.
    const { screen } = mount();
    screen.paint(withLog());
    expect(document.querySelector(".log-source")).toBeNull();
    // But it still SAYS where the lines are from: that wasn't optional.
    expect(document.body.textContent).toContain("de esta ventana");
  });

  it("with a daemon, the selector cycles the three sources in one press", () => {
    const { screen, sent } = mount();
    screen.paint(
      withLog({
        sources_available: true,
        source_mode: "both",
        source: "de la ventana y del daemon",
      }),
    );
    const selector = document.querySelector(".log-source");
    expect(selector).not.toBeNull();
    // The WIRE identifier, not the translated phrase: comparing phrases
    // would tie the test to the language.
    expect((selector as HTMLElement).dataset["source"]).toBe("both");
    (selector as HTMLButtonElement).click();
    expect(sent.at(-1)?.action).toBe("log_cycle_source");
  });

  it("every line says which process it came from", () => {
    // In a mixed list it's half the information: "the provider failed" and
    // "the window couldn't paint it" read the same without knowing who wrote
    // it, and they're two different failures.
    const { screen } = mount();
    screen.paint(withLog({ sources_available: true, source_mode: "both" }));
    const rows = [...document.querySelectorAll(".log-line")];
    expect((rows[0] as HTMLElement).dataset["source"]).toBe("daemon");
    expect((rows[1] as HTMLElement).dataset["source"]).toBe("window");
  });

  it("says when the daemon does NOT serve its log", () => {
    // Half of #326 applied to the other shore: the panel falls back to the
    // local ring and says so, instead of staying mute and looking broken.
    const { screen } = mount();
    const note = realCatalog()["log-source-unsupported"] ?? "";
    // The key HAS to exist: without this, deleting it from the catalogue
    // would leave the `toContain("")` below always passing, and the test
    // would say yes to nothing.
    expect(note).not.toBe("");
    screen.paint(withLog({ source_note: note }));
    const box = document.querySelector(".log");
    const header = box?.parentElement?.querySelector(".slot-title")?.textContent ?? "";
    expect(header).toContain(note);
  });

  it("the wheel scrolls through the HOST, not through the DOM", () => {
    // The visible window is decided by the host: letting the browser scroll
    // a chunk that only has the visible lines wouldn't lead anywhere.
    const { screen, sent } = mount();
    screen.paint(withLog());
    const box = document.querySelector(".log") as HTMLElement;
    box.dispatchEvent(new WheelEvent("wheel", { deltaY: 120, bubbles: true }));
    const last = sent.at(-1);
    expect(last?.action).toBe("log_scroll");
    if (last?.action === "log_scroll") {
      expect(last.delta).toBeGreaterThan(0);
    }
  });
});

describe("the places sidebar", () => {
  function withPlaces(cursor: number): ViewSnapshot {
    const v = view({});
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
            free: "12G",
            mount: "/",
            kind: "network",
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

  it("a header says whether it's folded, and a broken favorite says why", () => {
    const { screen } = mount();
    screen.paint(withPlaces(1));
    const rows = [...document.querySelectorAll(".places-row")];
    expect(rows).toHaveLength(5);
    expect(rows[0]?.getAttribute("aria-expanded")).toBe("true");
    expect(rows[2]?.getAttribute("aria-expanded")).toBe("false");
    // The broken one is SEEN, and seen to be broken.
    expect(rows[4]?.querySelector(".places-broken")?.textContent).toBe("la ruta no vale");
    expect(rows[3]?.querySelector(".places-broken")).toBeNull();
    const list = document.querySelector(".places-rows") as HTMLElement;
    expect(list.getAttribute("aria-activedescendant")).toBe("place-row-1");
  });

  it("a drive goes on ONE line: icon, short name and short free space", () => {
    const { screen } = mount();
    screen.paint(withPlaces(0));
    const drive = document.querySelectorAll(".places-row")[1] as HTMLElement;
    expect(drive.dataset["line"]).toBe("one");
    expect(drive.dataset["kind"]).toBe("network");
    expect(drive.querySelector("svg.places-icon")).not.toBeNull();
    expect(drive.querySelector(".places-name")?.textContent).toBe("raíz");
    expect(drive.querySelector(".places-detail")?.textContent).toBe("12G");
    // What doesn't fit in the row, in its title.
    expect(drive.title).toBe("/\n12 GiB libres de 100 GiB");
    // A healthy favorite, too: the target in the title.
    const favorite = document.querySelectorAll(".places-row")[3] as HTMLElement;
    expect(favorite.dataset["line"]).toBe("one");
    expect(favorite.title).toBe("⟨file⟩/home");
  });

  it("a click SELECTS and ACTIVATES: a sidebar exists to go to places", () => {
    const { screen, sent } = mount();
    screen.paint(withPlaces(0));
    const rows = [...document.querySelectorAll(".places-row")];
    (rows[1] as HTMLElement).click();
    expect(sent).toEqual([{ action: "place_activate_row", row: 1, generation: 3 }]);
    // Also on a header: there, activating means FOLDING, and the host
    // decides.
    (rows[2] as HTMLElement).click();
    expect(sent).toHaveLength(2);
  });
});

describe("the layouts selector", () => {
  function withLayouts(cursor: number, problem = ""): ViewSnapshot {
    const v = view({});
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

  it("warns about the shared name and marks the one that fails to parse", () => {
    const { screen } = mount();
    screen.paint(withLayouts(0));
    const rows = [...document.querySelectorAll(".layouts-row")];
    expect(rows).toHaveLength(2);
    // The warning isn't decoration: selecting it changes no key.
    expect(rows[0]?.querySelector(".layouts-warn")).not.toBeNull();
    expect(rows[0]?.querySelector(".layouts-tag")?.textContent).toBe(
      realCatalog()["layout-picker-factory"] ?? "",
    );
    expect(rows[1]?.getAttribute("data-broken")).toBe("true");
    expect(rows[1]?.querySelector(".layouts-tag")).toBeNull();
  });

  it("the thumbnail arrives ready-made and gets set as is", () => {
    const { screen } = mount();
    screen.paint(withLayouts(0));
    const preview = document.querySelector(".layouts-preview");
    expect(preview?.tagName).toBe("PRE");
    expect(preview?.textContent).toBe("··········\n·bbbbbbbb·");
    // It's decorative: what it says is already in the row's name.
    expect(preview?.getAttribute("aria-hidden")).toBe("true");
  });

  it("one that fails to parse shows its reason instead of a thumbnail", () => {
    const { screen } = mount();
    screen.paint(withLayouts(1, "no parsea: falta `kind`"));
    expect(document.querySelector(".layouts-preview")).toBeNull();
    expect(document.querySelector(".layouts-problem")?.textContent).toBe(
      "no parsea: falta `kind`",
    );
  });

  it("a click selects THAT layout", () => {
    const { screen, sent } = mount();
    screen.paint(withLayouts(0));
    const rows = [...document.querySelectorAll(".layouts-row")];
    (rows[1] as HTMLElement).click();
    expect(sent).toEqual([{ action: "layout_activate_row", row: 1 }]);
  });
});

describe("search", () => {
  function withSearch(running: boolean): ViewSnapshot {
    const v = view({});
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

  it("says what state it's in, and announces it without stealing focus", () => {
    const { screen } = mount();
    screen.paint(withSearch(true));
    const status = document.querySelector(".search-status") as HTMLElement;
    expect(status.getAttribute("role")).toBe("status");
    expect(status.getAttribute("aria-live")).toBe("polite");
    expect(status.getAttribute("data-running")).toBe("true");
    expect(status.textContent).toContain("buscando");

    screen.paint(withSearch(false));
    expect(document.querySelector(".search-status")?.getAttribute("data-running")).toBe(
      "false",
    );
  });

  it("every row says the name and WHERE it is, and marks the hostile one", () => {
    const { screen } = mount();
    screen.paint(withSearch(true));
    const rows = [...document.querySelectorAll(".search-row")];
    expect(rows).toHaveLength(2);
    expect(rows[0]?.querySelector(".search-name")?.textContent).toBe("main.rs");
    expect(rows[0]?.querySelector(".search-parent")?.textContent).toContain("src");
    expect(rows[1]?.querySelector(".search-name")?.getAttribute("data-hostile")).toBe(
      "true",
    );
  });

  it("a click goes to THAT result, sending an index and not a path", () => {
    const { screen, sent } = mount();
    screen.paint(withSearch(false));
    const rows = [...document.querySelectorAll(".search-row")];
    (rows[1] as HTMLElement).click();
    expect(sent).toEqual([{ action: "search_activate_row", row: 1 }]);
  });

  it("closed, it covers nothing", () => {
    const { screen } = mount();
    screen.paint(view({}));
    expect(document.querySelector(".search")).toBeNull();
  });
});

describe("a dialog that asks about an operation", () => {
  it("paints the destination OUTSIDE the body, marks what's altered, and says if it truncates", () => {
    const { screen } = mount();
    const v = view({});
    v.dialogs = [
      {
        id: 4,
        title_key: "modal-copy-title",
        subject: null,
        asker: null,
        deadline: null,
        // A directory called `a → mem_b.txt`: the arrow is legitimate, not
        // masked and not flagged. With the destination as the body's first
        // line, the line would read as two paths.
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
    // And it isn't a body line: the body is a separate NUMBERED list, and
    // the destination isn't in it.
    const body = [...dialog.querySelectorAll("ol.dialog-body li")];
    expect(body).toHaveLength(2);
    expect(body.map((p) => p.textContent ?? "").join(" ")).not.toContain("→");

    // The altered line says so, and the faithful one doesn't.
    expect((body[0] as HTMLElement | undefined)?.dataset["hostile"]).toBe("false");
    expect((body[1] as HTMLElement | undefined)?.dataset["hostile"]).toBe("true");
    expect(body[1]?.textContent ?? "").toContain("nombre alterado");

    // And the truncation paints as a notice.
    const note = dialog.querySelector(".dialog-overflow") as HTMLElement;
    expect(note).not.toBeNull();
    expect(note.getAttribute("role")).toBe("alert");
    expect(note.textContent).toContain("240");
  });
});

describe("the task board", () => {
  it("says when the file in progress paints differently from what it is", () => {
    const { screen, root } = mount();
    const v = view({});
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

describe("reviewing a rename plan", () => {
  it("separates a pair's two names WITHOUT a glyph a name could contain", () => {
    const { screen } = mount();
    const v = view({});
    v.ai_rename = {
      dir: { text: "⟨mem⟩/casa/series", hostile: false },
      pairs: [
        // A SOURCE name with an arrow inside: on a single line separated by
        // `→`, the row would read as another pair.
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
    const box = document.querySelector(".ai-rename") as HTMLElement;
    expect(box).not.toBeNull();
    expect(box.getAttribute("aria-modal")).toBe("true");

    const from = box.querySelector(".ai-rename-from") as HTMLElement;
    const to = box.querySelector(".ai-rename-to") as HTMLElement;
    expect(from.textContent).toBe("cap 2 → final.mkv");
    expect(to.textContent).toContain("ep0�2.mkv");
    // The separator is NOT in the text: it's painted by CSS, which a name
    // can't write.
    expect(to.textContent?.startsWith("→")).toBe(false);
    expect(from.contains(to)).toBe(false);

    expect(to.dataset["hostile"]).toBe("true");
    expect(to.textContent).toContain("nombre alterado");
    expect(from.dataset["hostile"]).toBe("false");

    const status = box.querySelector(".ai-rename-status") as HTMLElement;
    expect(status.dataset["confirmable"]).toBe("true");
    expect(box.querySelector(".ai-rename-more")?.textContent).toContain("7");
  });

  it("a plan the core doesn't accept says so in its status", () => {
    const { screen } = mount();
    const v = view({});
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
    const status = document.querySelector(".ai-rename-status") as HTMLElement;
    expect(status.dataset["confirmable"]).toBe("false");
  });

  it("with no plan, nothing is left painted", () => {
    const { screen } = mount();
    const v = view({});
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

describe("reviewing an organize plan", () => {
  /** A tree with the three line kinds and one altered name. */
  function tree(): NonNullable<ViewSnapshot["organize"]> {
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

  it("marks each line kind two ways and indents with data, not text", () => {
    const { screen } = mount();
    const v = view({});
    v.organize = tree();
    screen.paint(v);
    const box = document.querySelector(".organize") as HTMLElement;
    expect(box).not.toBeNull();
    expect(box.getAttribute("aria-modal")).toBe("true");
    // The tally goes BEFORE the tree: it's what gets read to decide.
    const body = Array.from(box.children).map((e) => e.className);
    expect(body.indexOf("organize-summary")).toBeLessThan(body.indexOf("organize-tree"));

    const rows = Array.from(box.querySelectorAll<HTMLElement>(".organize-line"));
    expect(rows).toHaveLength(3);
    // The class says what it is, AND the marker says it again: color doesn't
    // survive a monochrome theme.
    expect(rows[0]?.classList.contains("organize-existing-dir")).toBe(true);
    expect(rows[1]?.classList.contains("organize-new-dir")).toBe(true);
    expect(rows[0]?.querySelector(".organize-mark")?.textContent).toBe("·");
    expect(rows[1]?.querySelector(".organize-mark")?.textContent).toBe("+");
    expect(rows[2]?.querySelector(".organize-mark")?.textContent).toBe("→");
    // Indentation is a style variable, not spaces in the name: a name that
    // starts with spaces can't pretend to be nested deeper.
    expect(rows[2]?.style.getPropertyValue("--depth")).toBe("2");
    const name = rows[2]?.querySelector(".organize-name") as HTMLElement;
    expect(name.textContent?.startsWith(" ")).toBe(false);
    expect(name.dataset["hostile"]).toBe("true");
    expect(name.textContent).toContain("nombre alterado");
  });

  it("doesn't allow approval until it's been read in full", () => {
    const { screen } = mount();
    const v = view({});
    v.organize = tree();
    screen.paint(v);
    const buttons = Array.from(
      document.querySelectorAll<HTMLButtonElement>(".organize .choices button"),
    );
    expect(buttons[0]?.disabled).toBe(true);
    // Discarding is ALWAYS possible: whoever doesn't want this has to be
    // able to get rid of it.
    expect(buttons[1]?.disabled).toBe(false);

    v.organize = { ...tree(), seen_all: true };
    screen.paint(v);
    const after = document.querySelector(
      ".organize .choices button",
    ) as HTMLButtonElement;
    expect(after.disabled).toBe(false);
  });

  it("with no plan, nothing is left painted", () => {
    const { screen } = mount();
    const v = view({});
    v.organize = tree();
    screen.paint(v);
    expect(document.querySelector(".organize")).not.toBeNull();
    v.organize = null;
    screen.paint(v);
    expect(document.querySelector(".organize")).toBeNull();
  });
});

describe("search by meaning", () => {
  it("titles itself differently, states its scope, and paints the similarity", () => {
    const { screen } = mount();
    const v = view({});
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

    const box = document.querySelector(".search") as HTMLElement;
    expect(box.querySelector("h1")?.textContent ?? "").toContain(
      "facturas del año pasado",
    );
    // The similarity is visible, with two decimals: without it, the order
    // looks arbitrary.
    expect(box.querySelector(".search-score")?.textContent).toBe("0.91");
  });
});
