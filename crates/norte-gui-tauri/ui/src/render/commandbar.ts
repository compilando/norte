// The command bar (ADR 0181, bridge 113): a `Screen` painter like the
// others, hooked in as a property in `render.ts`.

import type { Screen } from "../render";
import type { CommandBarView } from "../types";
import { cutStart } from "./dom";

/**
 * The command bar: one fixed row under the panels, reserved through
 * `--commandbar-h` the way the panel bar reserves its own, so the host is
 * told a smaller viewport and nothing is painted under it.
 *
 * The HOST owns the line, its cursor and its history; this paints them and
 * says when the bar is clicked. Everything is `textContent`: the line is the
 * reader's text and the prompt a folder's name.
 */
export function paintCommandBar(this: Screen, bar: CommandBarView | null): void {
  const height = bar === null ? "0px" : "var(--cell-h)";
  if (this.commandBarHeight !== height) {
    document.documentElement.style.setProperty("--commandbar-h", height);
    this.commandBarHeight = height;
    this.viewportDirty = true;
  }
  let root = document.getElementById("commandbar");
  if (bar === null) {
    if (root !== null) {
      root.dataset["open"] = "false";
      root.dataset["focused"] = "false";
      root.replaceChildren();
    }
    return;
  }
  if (root === null) {
    root = document.createElement("div");
    root.id = "commandbar";
    root.setAttribute("role", "textbox");
    // `mousedown`, not `click`: the press must not move the document's
    // focus away from where the keys are read.
    root.addEventListener("mousedown", (e) => {
      e.preventDefault();
      this.send({ action: "command_bar_focus" });
    });
    // A press anywhere else takes the keyboard back to where the pointer
    // went: the bar gives up its keys, as with `Escape`.
    document.addEventListener(
      "mousedown",
      (e) => {
        const bar = document.getElementById("commandbar");
        if (
          bar?.dataset["focused"] === "true" &&
          e.target instanceof Node &&
          !bar.contains(e.target)
        ) {
          this.send({
            action: "key",
            key: "Escape",
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
          });
        }
      },
      true,
    );
    // Its own paste: the search box's listener only exists while the box
    // is open. The first line only, decided by the host.
    document.addEventListener(
      "paste",
      (e) => {
        // The search box, when open, is in front of the bar and owns it.
        if (
          document.getElementById("commandbar")?.dataset["focused"] !== "true" ||
          document.getElementById("goto")?.dataset["open"] === "true"
        ) {
          return;
        }
        e.preventDefault();
        e.stopPropagation();
        this.send({
          action: "command_bar_paste",
          text: e.clipboardData?.getData("text/plain") ?? "",
        });
      },
      true,
    );
    document.body.append(root);
  }
  root.dataset["open"] = "true";
  root.dataset["focused"] = String(bar.focused);
  root.setAttribute("aria-label", bar.prompt);

  const prompt = document.createElement("span");
  prompt.className = "commandbar-prompt";
  cutStart(prompt).textContent = bar.prompt;
  const sign = document.createElement("span");
  sign.className = "commandbar-sign";
  sign.textContent = ">";

  const line = document.createElement("span");
  line.className = "commandbar-line";
  if (bar.text === "" && bar.reason !== "") {
    line.dataset["reason"] = "true";
    line.textContent = bar.reason;
  } else {
    const chars = Array.from(bar.text);
    const before = document.createElement("span");
    before.textContent = chars.slice(0, bar.cursor).join("");
    const caret = document.createElement("span");
    caret.className = "commandbar-caret";
    caret.textContent = chars[bar.cursor] ?? " ";
    const after = document.createElement("span");
    after.textContent = chars.slice(bar.cursor + 1).join("");
    line.append(before);
    if (bar.focused) {
      line.append(caret);
    } else {
      before.textContent += chars[bar.cursor] ?? "";
    }
    line.append(after);
  }
  root.replaceChildren(prompt, sign, line);
  // A line longer than the row scrolls to keep the cursor in view: typing
  // blind past the edge runs a tail nobody saw.
  const caret = line.querySelector(".commandbar-caret");
  if (caret instanceof HTMLElement && typeof caret.scrollIntoView === "function") {
    caret.scrollIntoView({ block: "nearest", inline: "nearest" });
  }
}
