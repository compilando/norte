// The end-to-end harness's probes (`scripts/gui-e2e`): what each one reads.

import { describe, expect, it } from "vitest";

import { traceProbes } from "../src/probes";

function el(
  tag: string,
  attrs: Record<string, string>,
  text: string,
  ...children: HTMLElement[]
): HTMLElement {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    e.setAttribute(k, v);
  }
  e.textContent = text;
  e.append(...children);
  return e;
}

describe("trace probes", () => {
  it("read the active slot, its cursor row, the overlays and the tone", () => {
    const button = el("button", { class: "ok", "aria-label": "Create" }, "Create");
    document.body.replaceChildren(
      el(
        "div",
        { class: "slot", "data-role": "active" },
        "",
        el("header", { class: "slot-title" }, "~/data/a"),
        el("div", { class: "row", "aria-selected": "false" }, "x"),
        el("div", { class: "row", "aria-selected": "true" }, "informe.pdf"),
      ),
      el(
        "div",
        { class: "slot", "data-role": "target" },
        "",
        el("header", { class: "slot-title" }, "~/data"),
      ),
      el(
        "div",
        { id: "menu", "data-open": "true" },
        "",
        el(
          "div",
          { class: "menu" },
          "",
          el("nav", { class: "menubar" }, "File Mark"),
          el("ul", { class: "menu-items" }, "View\n   Quit"),
        ),
      ),
      el("div", { id: "help", "data-open": "false" }, ""),
      el("span", { class: "palette-query" }, ":sel  ext"),
      button,
    );
    document.body.style.background = "rgb(250, 250, 250)";
    button.focus();
    const p = traceProbes(document);
    expect(p["active"]).toBe("~/data/a");
    expect(p["cursor"]).toBe("informe.pdf");
    expect(p["slots"]).toBe("~/data/a | ~/data");
    expect(p["menu"]).toBe("View Quit");
    expect(p["open"]).toBe("menu");
    // Exact, spaces kept: `expect-shown` checks them.
    expect(p["goto"]).toBe(":sel  ext");
    expect(p["focus"]).toBe("button.ok[Create]");
    expect(p["tone"]).toBe("light");
    expect(p["dialog"]).toBe("");
  });
});
