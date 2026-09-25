// Document order IS paint order: this window uses `z-index` nowhere, so
// whoever comes later in `index.html` covers whoever came before. These
// tests pin the order that matters for the keyboard: whatever keeps the keys
// has to paint over whatever loses them.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

// By vitest's root (the `ui/` directory) and not by `import.meta.url`: under
// jsdom that URL is `http://` and can't be read as a file.
const html = readFileSync(resolve(process.cwd(), "index.html"), "utf8");

function position(id: string): number {
  const i = html.indexOf(`id="${id}"`);
  expect(i, `#${id} exists in index.html`).toBeGreaterThanOrEqual(0);
  return i;
}

describe("document order", () => {
  it("dialogs come above settings and every selector", () => {
    // A dialog keeps the keyboard over whatever was there. Declared before
    // `#settings`, the prompt asking for a setting's value used to open
    // BELOW settings: you'd see the veil and no field.
    for (const below of [
      "settings",
      "agents",
      "extensions",
      "theme",
      "picker",
      "profiles",
      "layouts",
      "columns",
      "search",
      "compare",
      "sync",
      "ai-rename",
    ]) {
      expect(position("dialogs"), `#dialogs after #${below}`).toBeGreaterThan(
        position(below),
      );
    }
  });

  it("only help and the fatal notice come above dialogs", () => {
    expect(position("help")).toBeGreaterThan(position("dialogs"));
    expect(position("fatal")).toBeGreaterThan(position("help"));
  });

  it("a plugin's output stays below dialogs", () => {
    // A confirmation that still holds the keyboard can't be covered by the
    // output of a command whose timing the plugin chooses.
    expect(position("plugin-output")).toBeLessThan(position("dialogs"));
    expect(position("program-output")).toBeLessThan(position("dialogs"));
  });
});
