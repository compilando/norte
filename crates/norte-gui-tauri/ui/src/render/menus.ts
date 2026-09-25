// `Screen` painters for menus (wave W10): functions with `this: Screen`,
// hooked in as properties in `render.ts`. State stays in the class.

import type { Screen } from "../render";
import type {
  BrowserSlotView,
  ChromeButtonView,
  ColumnHeader,
  MenuView,
  PanelBarView,
  WizardView,
  GotoView,
  PaletteView,
  TabGroupView,
  WhichKeyView,
  WindowVerb,
} from "../types";
import { badge, colVar, revealInView, unchanged } from "./dom";
import type { SlotDom } from "./dom";
import { badgeCount, icon as panelIcon } from "./icons";
import { makeDraggable } from "./move";

/**
 * The panel bar (#324): one button per panel that opens and closes, with its
 * state and its novelty mark.
 *
 * The buttons come DECIDED by the host — what there is, in what order, with
 * what letter — because the decision belongs to `norte-frontend` and the TUI
 * paints the same one (ADR 0077). Here they are only painted and clicked; a
 * click comes back as the button's index, never as a command (ADR 0069).
 *
 * Reserves its spot the same way the menu bar does: the host lays things out
 * over what this renderer declares, and a floating bar would cover the
 * listing's first row — or its first column.
 *
 * Two shapes (bridge 84, `[ui] panel_bar_position`): the ROW under the menu,
 * with letter and name as in the TUI, or the COLUMN on the left edge, VS
 * Code's activity bar, with an icon per panel and its novelty count. Which
 * one applies is decided by the host; here only the matching height or width
 * is reserved.
 */
export function paintPanelBar(this: Screen, bar: PanelBarView): void {
  const column = bar.bar && bar.vertical === true;
  const height = bar.bar && !column ? "var(--cell-h)" : "0px";
  const width = column ? "var(--activity-size)" : "0px";
  if (this.panelBarHeight !== height || this.activityWidth !== width) {
    document.documentElement.style.setProperty("--panelbar-h", height);
    document.documentElement.style.setProperty("--activity-w", width);
    this.panelBarHeight = height;
    this.activityWidth = width;
    this.viewportDirty = true;
  }
  if (unchanged(this.panelBarRoot, JSON.stringify(bar))) {
    return;
  }
  if (!bar.bar) {
    this.panelBarRoot.replaceChildren();
    return;
  }
  const row = document.createElement("nav");
  row.className = "panelbar";
  row.setAttribute("role", "toolbar");
  row.setAttribute("aria-label", this.t("panelbar-label"));
  row.setAttribute("aria-orientation", column ? "vertical" : "horizontal");
  row.dataset["vertical"] = String(column);
  // `[ui] panel_bar_style`: with names or just the letter. The name stays in
  // the button's title either way.
  row.dataset["names"] = String(bar.names !== false);
  for (const [i, b] of bar.buttons.entries()) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "panelbar-button";
    button.dataset["kind"] = b.kind;
    button.dataset["state"] = b.state;
    // `aria-pressed` is what a screen reader understands as "this panel is
    // open"; keyboard focus is separate, in the state.
    button.setAttribute("aria-pressed", String(b.state !== "closed"));
    button.title = b.chord === "—" ? b.label : `${b.label} (${b.chord})`;
    const icon = column ? panelIcon(document, b.kind) : null;
    if (icon !== null) {
      // In column mode there is no visible text: the name goes in the label,
      // which is what a screen reader hears, and in the title on hover.
      button.setAttribute("aria-label", b.label);
      button.append(icon);
    } else {
      const letter = document.createElement("span");
      letter.className = "panelbar-letter";
      letter.textContent = b.letter;
      button.append(letter);
      if (column) {
        button.setAttribute("aria-label", b.label);
      } else {
        const name = document.createElement("span");
        name.className = "panelbar-name";
        name.textContent = b.label;
        button.append(name);
      }
    }
    if (b.attention) {
      // The mark is a SEPARATE span and the button keeps its state's style:
      // painting it whole as a warning would take away the reader's answer
      // to "where do my keys go?" right when they need it most. With a
      // count if the host sends one; an older host only says "something".
      const mark = document.createElement("span");
      mark.className = "panelbar-attention";
      const n = b.count ?? 0;
      mark.textContent = n > 0 ? badgeCount(n) : "·";
      mark.setAttribute(
        "aria-label",
        n > 0
          ? `${this.t("panelbar-attention")}: ${String(n)}`
          : this.t("panelbar-attention"),
      );
      button.append(mark);
    }
    button.addEventListener("click", () => {
      this.send({ action: "panel_bar_activate", button: i });
    });
    row.append(button);
  }
  this.panelBarRoot.replaceChildren(row);
}

/**
 * The first-run wizard (bridge 63): the step's title, the question, the rows
 * with the cursor, and the key line. Everything arrives already translated;
 * a click on a row chooses it and confirms it. Its root is looked up by id
 * and, if the document does not have it, created at the end of the body: it
 * is a full-screen veil, and document order does not matter to it.
 */
export function paintWizard(this: Screen, wizard: WizardView | null): void {
  const doc = this.root.ownerDocument;
  let root = doc.getElementById("wizard");
  if (root === null) {
    root = doc.createElement("div");
    root.id = "wizard";
    doc.body.append(root);
  }
  if (wizard === null) {
    root.replaceChildren();
    root.dataset["open"] = "false";
    return;
  }
  root.dataset["open"] = "true";
  const box = doc.createElement("section");
  box.className = "wizard";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", wizard.title);
  const title = doc.createElement("h2");
  title.className = "wizard-title";
  title.textContent = wizard.title;
  const question = doc.createElement("p");
  question.className = "wizard-question";
  question.textContent = wizard.question;
  const list = doc.createElement("ul");
  list.className = "wizard-rows";
  list.setAttribute("role", "listbox");
  for (const [i, text] of wizard.rows.entries()) {
    const row = doc.createElement("li");
    row.className = "wizard-row";
    row.id = `wizard-row-${String(i)}`;
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", String(wizard.cursor === i));
    row.textContent = text;
    row.addEventListener("click", () => {
      this.send({ action: "wizard_activate_row", row: i });
    });
    list.append(row);
  }
  list.setAttribute("aria-activedescendant", `wizard-row-${String(wizard.cursor)}`);
  const hint = doc.createElement("p");
  hint.className = "wizard-hint";
  hint.textContent = wizard.hint;
  box.append(title, question, list, hint);
  root.replaceChildren(box);
}

/** The window's three buttons, in desktop order. */
const WINDOW_VERBS = [
  ["minimize", "window-minimize"],
  ["toggle_maximize", "window-maximize"],
  ["close", "window-close"],
] as const;

/**
 * The menu bar as the TITLE bar (ADR 0136): the free space drags the window,
 * a double click maximizes it, and at the end come minimize, maximize and
 * close.
 *
 * Only the bar's own free space drags: starting a drag over a menu title or
 * a button would eat its click. None of this goes through the host — it is
 * not screen state — but through `window_control`, a binary command with a
 * closed verb; the webview's capability still carries no window permissions
 * (D11).
 */
function titleBar(this: Screen, bar: HTMLElement, hasActions: boolean): void {
  mountTitleBar(
    bar,
    (k) => this.t(k),
    (v) => {
      this.windowControl(v);
    },
    hasActions,
  );
}

/**
 * The same thing without `Screen`: the free space's drag and the three
 * buttons, over `bar`. Kept separate because the FATAL error screen also
 * needs it — it covers the menu bar, and without the desktop's, a window
 * with a dead daemon could not be moved nor closed with the mouse.
 */
export function mountTitleBar(
  bar: HTMLElement,
  t: (key: string) => string,
  ask: (verb: WindowVerb) => void,
  hasActions: boolean,
): void {
  const doc = bar.ownerDocument;
  bar.dataset["titlebar"] = "true";
  bar.addEventListener("mousedown", (e) => {
    if (e.button === 0 && e.target === bar && e.detail === 1) {
      ask("drag");
    }
  });
  bar.addEventListener("dblclick", (e) => {
    if (e.target === bar) {
      ask("toggle_maximize");
    }
  });
  const window_ = doc.createElement("div");
  window_.className = "window-controls";
  // With no layout buttons, nothing pushes the window's to the edge.
  window_.dataset["alone"] = String(!hasActions);
  window_.setAttribute("role", "toolbar");
  window_.setAttribute("aria-label", t("window-controls-label"));
  for (const [verb, key] of WINDOW_VERBS) {
    const button = doc.createElement("button");
    button.type = "button";
    button.className = "window-control";
    button.dataset["verb"] = verb;
    button.title = t(key);
    button.setAttribute("aria-label", t(key));
    const drawing = panelIcon(doc, `window:${verb}`);
    if (drawing !== null) {
      button.append(drawing);
    }
    button.addEventListener("click", () => {
      ask(verb);
    });
    window_.append(button);
  }
  bar.append(window_);
}

/**
 * The menu bar, and the dropdown if one is open.
 *
 * The same commands as the keyboard, sorted by topic. It adds no
 * capabilities: it adds a way to find them, for whoever does not know the
 * name of what they are looking for.
 *
 * A disabled entry STILL shows, dimmed: hiding what this window does not do
 * would turn a limitation into a mystery.
 */
export function paintMenu(
  this: Screen,
  menu: MenuView,
  buttons: ChromeButtonView[] = [],
): void {
  // The row the bar occupies comes from the CSS and enters the layout: the
  // host lays out over the height this renderer declares to it, so if the
  // bar did not reserve its row it would cover the listing's first one —
  // the same bug the TUI had with the full-screen viewer.
  //
  // With its OWN title bar (ADR 0136) the row always exists, with or without
  // menus: it is the only thing that drags and closes the window, and hiding
  // it would leave a window with no way to move it nor close it with the
  // mouse.
  const custom = document.documentElement.dataset["titlebar"] === "custom";
  const hasBar = menu.bar || custom;
  const height = hasBar ? "var(--cell-h)" : "0px";
  if (this.menuBarHeight !== height) {
    document.documentElement.style.setProperty("--menubar-h", height);
    this.menuBarHeight = height;
    this.viewportDirty = true;
  }
  if (unchanged(this.menuRoot, JSON.stringify({ menu, buttons, own: custom }))) {
    return;
  }
  if (!hasBar && menu.open === null) {
    this.menuRoot.replaceChildren();
    this.menuRoot.dataset["open"] = "false";
    return;
  }
  this.menuRoot.dataset["open"] = String(menu.open !== null);
  const bar = document.createElement("nav");
  bar.className = "menubar";
  bar.setAttribute("role", "menubar");
  bar.setAttribute("aria-label", this.t("menu-bar-label"));
  for (const [i, title] of (menu.bar ? menu.titles : []).entries()) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "menubar-title";
    button.id = `menu-title-${String(i)}`;
    button.textContent = title;
    button.setAttribute("role", "menuitem");
    button.setAttribute("aria-haspopup", "true");
    button.setAttribute("aria-expanded", String(menu.open === i));
    button.addEventListener("click", () => {
      this.send({ action: "menu_open", menu: i });
    });
    bar.append(button);
  }
  // The layout buttons (ADR 0133), on the right edge: one icon per command,
  // with its name and its shortcut on hover. A click comes back as the id;
  // the host runs the command (ADR 0069).
  if (buttons.length > 0) {
    const actions = document.createElement("div");
    actions.className = "menubar-actions";
    actions.setAttribute("role", "toolbar");
    actions.setAttribute("aria-label", this.t("layout-buttons-label"));
    for (const b of buttons) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "menubar-action";
      button.dataset["id"] = b.id;
      button.title = b.chord === "—" ? b.label : `${b.label} (${b.chord})`;
      button.setAttribute("aria-label", b.label);
      const icon = panelIcon(document, `layout:${b.id}`);
      if (icon !== null) {
        button.append(icon);
      } else {
        button.textContent = b.label;
      }
      button.addEventListener("click", () => {
        this.send({ action: "layout_button_activate", id: b.id });
      });
      actions.append(button);
    }
    bar.append(actions);
  }
  if (custom) {
    titleBar.call(this, bar, buttons.length > 0);
  }
  const box = document.createElement("div");
  box.className = "menu";
  box.append(bar);

  if (menu.open !== null) {
    const list = document.createElement("ul");
    list.className = "menu-items";
    list.setAttribute("role", "menu");
    // The dropdown hangs from ITS OWN title, not from the window's edge: a
    // menu that always opens to the left does not say whose it is.
    list.style.setProperty("--menu-open", String(menu.open));
    for (const [i, item] of menu.items.entries()) {
      // A section starts HERE (bridge 74): a rule, with its label if it has
      // one. It is a `separator` and not an entry, so the cursor, which
      // counts entries, does not see it.
      if (item.section !== null) {
        const rule = document.createElement("li");
        rule.className = "menu-section";
        rule.setAttribute("role", "separator");
        if (item.section !== "") {
          rule.textContent = item.section;
          rule.dataset["titled"] = "true";
        }
        list.append(rule);
      }
      const row = document.createElement("li");
      row.className = "menu-item";
      row.id = `menu-item-${String(i)}`;
      row.setAttribute("role", "menuitem");
      row.setAttribute("aria-disabled", String(!item.enabled));
      row.dataset["enabled"] = String(item.enabled);
      row.dataset["current"] = String(menu.cursor === i);
      row.dataset["role"] = item.role;
      const label = document.createElement("span");
      label.className = "menu-label";
      label.textContent = item.label;
      const chord = document.createElement("span");
      chord.className = "menu-chord";
      chord.textContent = item.chord;
      row.append(label, chord);
      row.addEventListener("mousemove", () => {
        this.send({ action: "menu_point_row", row: i });
      });
      row.addEventListener("click", () => {
        this.send({ action: "menu_activate_row", row: i });
      });
      list.append(row);
    }
    list.setAttribute("aria-activedescendant", `menu-item-${String(menu.cursor)}`);
    box.append(list);
    // A click OUTSIDE closes it, which is what a menu does everywhere. The
    // veil goes BEHIND the dropdown in the DOM and with no `z-index`, same
    // as the rest of this screen.
    const veil = document.createElement("div");
    veil.className = "menu-veil";
    veil.addEventListener("click", () => {
      this.send({ action: "menu_close" });
    });
    box.prepend(veil);
  }
  this.menuRoot.replaceChildren(box);
  if (menu.open !== null) {
    // The dropdown hangs from the PAINTED title, measured once it is
    // mounted: titles are painted with padding in pixels and do not all
    // measure the same, so a count in cells drifted more the further right
    // the menu was. Measured after `replaceChildren` because there is no
    // geometry before that; forcing a layout here is cheap, a menu is
    // opened by hand.
    const title = this.menuRoot.querySelector(`#menu-title-${String(menu.open)}`);
    const list = this.menuRoot.querySelector(".menu-items");
    if (title instanceof HTMLElement && list instanceof HTMLElement) {
      const x = title.getBoundingClientRect().left;
      list.style.setProperty("--menu-left", `${String(Math.max(0, x))}px`);
    }
  }
}

/** The command palette. */
export function paintPalette(this: Screen, palette: PaletteView | null): void {
  if (palette === null) {
    this.paletteRoot.replaceChildren();
    this.paletteRoot.dataset["open"] = "false";
    return;
  }
  this.paletteRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "palette";
  // Modal: while it is open, the keys are its own — and the host knows it,
  // so the screen reader has to know it too.
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", this.t("palette-title"));

  const query = document.createElement("div");
  query.className = "palette-query";
  query.textContent = palette.query;
  const count = document.createElement("span");
  count.className = "palette-count";
  count.textContent = `${String(palette.rows.length)}/${String(palette.total)}`;
  query.append(count);
  box.append(query);

  const list = document.createElement("ul");
  list.className = "palette-rows";
  list.setAttribute("role", "listbox");
  for (const [i, r] of palette.rows.entries()) {
    const row = document.createElement("li");
    row.className = "palette-row";
    row.id = `palette-row-${String(i)}`;
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", String(palette.cursor === i));
    row.dataset["enabled"] = String(r.enabled);
    row.dataset["hostile"] = String(r.hostile);
    row.dataset["recent"] = String(r.recent === true);
    // The human label first and whole, the id dimmed, the chord on the
    // right: that is the reading order. The id stays in the DOM because it
    // is what a reader who already knows it types.
    const desc = document.createElement("span");
    desc.className = "palette-desc";
    desc.textContent = r.desc;
    const text = document.createElement("span");
    text.className = "palette-text";
    text.textContent = r.text;
    const chord = document.createElement("span");
    chord.className = "palette-chord";
    chord.textContent = r.chord;
    row.append(desc, text, chord);
    if (r.hostile) {
      // Only a PLUGIN row can be, and this is the screen where you choose
      // what third-party code to run: masked text that travels without
      // saying so reads as trustworthy.
      row.append(badge(this.t("hostile-name")));
    }
    list.append(row);
  }
  if (palette.cursor !== null) {
    list.setAttribute("aria-activedescendant", `palette-row-${String(palette.cursor)}`);
  }
  if (palette.rows.length === 0) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent = this.t("palette-empty");
    list.append(empty);
  }
  box.append(list);
  this.paletteRoot.replaceChildren(box);
}

/**
 * "Go to anywhere" (#357, bridge 77): the query and the lines in order —
 * section headers and rows — with the cursor's marked. What is in each
 * section and in what order is decided by the host with the shared model;
 * here it is only painted.
 */
export function paintGoto(this: Screen, goto: GotoView | null): void {
  if (goto === null) {
    this.gotoRoot.replaceChildren();
    this.gotoRoot.dataset["open"] = "false";
    return;
  }
  this.gotoRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  // The palette's classes: it is the same screen shape — a query and a list
  // that narrows — and two stylesheets for the same thing drift apart.
  box.className = "palette goto";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", this.t("goto-title"));

  const query = document.createElement("div");
  query.className = "palette-query";
  query.textContent = goto.query;
  box.append(query);

  const list = document.createElement("ul");
  list.className = "palette-rows";
  list.setAttribute("role", "listbox");
  for (const [i, l] of goto.lines.entries()) {
    const item = document.createElement("li");
    if (l.line === "header") {
      // A header is not an option: it does not get the cursor nor is it
      // announced as selectable.
      item.className = "goto-header";
      item.setAttribute("role", "presentation");
      item.textContent = l.title;
      list.append(item);
      continue;
    }
    item.className = "palette-row";
    item.id = `goto-row-${String(i)}`;
    item.setAttribute("role", "option");
    item.setAttribute("aria-selected", String(goto.cursor === i));
    item.dataset["hostile"] = String(l.hostile);
    const text = document.createElement("span");
    text.className = "palette-text";
    text.textContent = l.text;
    const desc = document.createElement("span");
    desc.className = "palette-desc";
    desc.textContent = l.desc;
    item.append(text, desc);
    if (l.hostile) {
      // A masked name on the screen where you choose where to go: the reader
      // has to know it is not really called that.
      item.append(badge(this.t("hostile-name")));
    }
    list.append(item);
  }
  if (goto.cursor !== null) {
    list.setAttribute("aria-activedescendant", `goto-row-${String(goto.cursor)}`);
  }
  if (goto.lines.length === 0) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent = goto.empty;
    list.append(empty);
  }
  box.append(list);
  this.gotoRoot.replaceChildren(box);
  // With an empty query, the sections can go past the box's height, and the
  // list is rebuilt on every patch with the scroll at the top: without this,
  // going down moved an invisible cursor and Enter went to a spot the reader
  // could not see.
  revealInView(list.querySelector('[aria-selected="true"]') ?? undefined);
}

/** What can follow a half-finished prefix. */
export function paintWhichKey(this: Screen, panel: WhichKeyView | null): void {
  if (panel === null) {
    this.whichKeyRoot.replaceChildren();
    this.whichKeyRoot.dataset["open"] = "false";
    return;
  }
  this.whichKeyRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "whichkey";
  // Not a dialog: it does not capture focus nor wait for an answer. It is a
  // hint that shows up while the sequence is half-finished.
  box.setAttribute("role", "group");
  box.setAttribute("aria-label", panel.title);

  const title = document.createElement("header");
  title.className = "whichkey-title";
  title.textContent = panel.title;
  box.append(title);

  const list = document.createElement("ul");
  list.className = "whichkey-rows";
  for (const r of panel.rows) {
    const row = document.createElement("li");
    row.className = "whichkey-row";
    row.dataset["enabled"] = String(r.enabled);
    const chord = document.createElement("span");
    chord.className = "whichkey-chord";
    chord.textContent = r.chord;
    const label = document.createElement("span");
    label.className = "whichkey-label";
    // `opens_sequence` is MARKED instead of naming a command the key does
    // not run; a disabled shortcut's reason already arrives translated.
    label.textContent = r.opens_sequence ? `${r.label}…` : r.label;
    row.append(chord, label);
    if (!r.enabled && r.reason !== "") {
      const reason = document.createElement("span");
      reason.className = "whichkey-reason";
      reason.textContent = r.reason;
      row.append(reason);
    }
    list.append(row);
  }
  box.append(list);
  this.whichKeyRoot.replaceChildren(box);
}

/**
 * A slot's TAB bar, if it is in a group.
 *
 * Painted even when only one's content is visible: what is behind it is
 * still open, and a window that does not say so hides work. The label
 * arrives already masked by the host — a hostile directory inside a tab is
 * as hostile as inside a listing — with its flag alongside.
 */
export function paintTabs(
  this: Screen,
  dom: SlotDom,
  group: TabGroupView | undefined,
): void {
  if (unchanged(dom.tabs, JSON.stringify(group ?? null))) {
    return;
  }
  if (group === undefined) {
    if (dom.tabs.dataset["open"] === "true") {
      dom.tabs.replaceChildren();
      dom.tabs.dataset["open"] = "false";
    }
    return;
  }
  dom.tabs.dataset["open"] = "true";
  const list = document.createElement("ul");
  list.className = "tabs";
  list.setAttribute("role", "tablist");
  for (const [i, t] of group.tabs.entries()) {
    const li = document.createElement("li");
    li.className = "tab";
    li.setAttribute("role", "tab");
    li.setAttribute("aria-selected", String(i === group.active));
    li.dataset["active"] = String(i === group.active);
    li.dataset["hostile"] = String(t.title_hostile);
    li.textContent = t.title;
    if (t.title_hostile) {
      li.append(badge(this.t("hostile-name")));
    }
    li.addEventListener("click", () => {
      // By SLOT and not by position: the list may have moved between the
      // paint and the click, and the host refuses a slot that is no longer
      // in any group instead of getting it right by chance.
      this.send({ action: "select_tab", slot_id: t.slot_id });
    });
    // Dragging the tab takes THAT slot out of the group (ADR 0138).
    makeDraggable(this, li, t.slot_id);
    // Close THIS tab (ADR 0133): each one's `×`, visible on the active one
    // and on hover, as in VS Code. The host chooses it and then closes it,
    // through `pane.tab-close`'s dispatch.
    const close = document.createElement("button");
    close.type = "button";
    close.className = "tab-close";
    close.textContent = "×";
    close.title = this.t("menu-item-pane-tab-close");
    close.setAttribute("aria-label", this.t("menu-item-pane-tab-close"));
    close.addEventListener("click", (e) => {
      // Without this the click would also select the tab, making it two
      // commands.
      e.stopPropagation();
      this.send({ action: "tab_action", slot_id: t.slot_id, verb: "close" });
    });
    if (group.panels !== true) {
      li.append(close);
    }
    list.append(li);
  }
  // A group of PANELS (ADR 0134) carries no `+` nor `×`: they open and close
  // as listings. Its panels open and close from the activity bar, like VS
  // Code's panel views.
  dom.tabs.dataset["panels"] = String(group.panels === true);
  if (group.panels === true) {
    dom.tabs.replaceChildren(list);
    return;
  }
  // Opening a tab IN THIS GROUP: the group's active one is chosen and it
  // opens behind it, same as the TUI bar's `[+]`.
  const add = document.createElement("button");
  add.type = "button";
  add.className = "tab-new";
  add.textContent = "+";
  add.title = this.t("menu-item-pane-tab-new");
  add.setAttribute("aria-label", this.t("menu-item-pane-tab-new"));
  const active = group.tabs[group.active] ?? group.tabs[0];
  if (active !== undefined) {
    add.addEventListener("click", () => {
      this.send({ action: "tab_action", slot_id: active.slot_id, verb: "new" });
    });
  }
  dom.tabs.replaceChildren(list, add);
}

/**
 * The header: labels and sort mark, both resolved in Rust.
 *
 * Each column's fixed width and alignment (bridge 64) are written as
 * variables on the slot's ROOT, not on every cell: rows already painted
 * read them without repainting, and dragging the grip only changes one
 * variable. The name's, never: it is the one that grows.
 */
export function paintHeader(this: Screen, dom: SlotDom, slot: BrowserSlotView): void {
  if (unchanged(dom.header, JSON.stringify(slot.columns))) {
    // Same columns: the nodes and the width variables are already there.
    // What CANNOT be skipped is the drop check, which depends on the slot's
    // width and not on the columns — a slot that widened gets back the
    // column it dropped when narrow, and that is why it is decided from
    // scratch again.
    for (const c of slot.columns) {
      if (c.id !== "name") {
        dom.root.style.removeProperty(`${colVar(c.id)}-show`);
      }
    }
    dropOverflowingColumns(dom, slot, this.cell().w);
    return;
  }
  const nodes = slot.columns.map((c) => {
    const el = document.createElement("span");
    el.className = c.id === "name" ? "col col-name" : "col";
    el.setAttribute("role", "columnheader");
    el.dataset["column"] = c.id;
    // `aria-sort` goes on the column that sorts and no other.
    el.setAttribute("aria-sort", c.sort === null ? "none" : `${c.sort}ending`);
    el.textContent = c.label;
    if (c.sort !== null) {
      const mark = document.createElement("span");
      mark.className = "sort-mark";
      mark.textContent = c.sort === "asc" ? "▲" : "▼";
      el.append(mark);
    }
    if (c.sortable) {
      el.dataset["sortable"] = "true";
      el.setAttribute("tabindex", "-1");
    }
    const v = colVar(c.id);
    if (c.id === "name") {
      dom.root.style.removeProperty(v);
      dom.root.style.removeProperty(`${v}-align`);
      return el;
    }
    if (c.width === null) {
      dom.root.style.removeProperty(v);
    } else {
      dom.root.style.setProperty(v, `calc(var(--cell-w) * ${String(c.width)})`);
    }
    dom.root.style.setProperty(`${v}-align`, c.align === "right" ? "right" : "left");
    el.style.width = `var(${v}, auto)`;
    el.style.textAlign = `var(${v}-align, left)`;
    el.style.display = `var(${v}-show, block)`;
    // Decided again on every paint: a slot that widened gets back the column
    // it dropped when it was narrow.
    dom.root.style.removeProperty(`${v}-show`);
    const grip = document.createElement("span");
    grip.className = "col-grip";
    grip.dataset["grip"] = c.id;
    el.append(grip);
    return el;
  });
  dom.header.replaceChildren(...nodes);
  dropOverflowingColumns(dom, slot, this.cell().w);
}

/**
 * The shared layout's rule 2 (`columns::layout`): if the columns do not
 * leave the name its floor, they are dropped starting from the RIGHTMOST
 * until they fit. Done here and not in the host because only whoever paints
 * knows the slot's usable width in pixels — borders, padding, grips.
 *
 * The name's floor comes from the HOST in the header itself (the `name`
 * column's `width` is `NAME_MIN`, not a width): a number living here too
 * would also drift from Rust's without anyone seeing it. And it counts ALL
 * columns, not just the fixed ones: an `auto` or `flex` one weighs whatever
 * its already-painted header measures. With no measurement (a document with
 * no layout, like the tests') nothing is dropped: an extra column beats a
 * listing with none.
 */
function dropOverflowingColumns(
  dom: SlotDom,
  slot: BrowserSlotView,
  cellW: number,
): void {
  const total = dom.root.clientWidth;
  if (total <= 0 || cellW <= 0) {
    return;
  }
  const floor = slot.columns.find((c) => c.id === "name")?.width ?? 10;
  const headers = [...dom.header.querySelectorAll<HTMLElement>(".col")];
  const widthOf = (c: ColumnHeader, i: number): number =>
    c.width === null ? (headers[i]?.getBoundingClientRect().width ?? 0) : c.width * cellW;
  // Row padding (6px each side), the slot's border, the mark checkbox
  // (1.1em ≈ a cell and a half) and one gap cell per column.
  let free = total - 14 - cellW * 1.5 - cellW * slot.columns.length;
  const rest = slot.columns.map((c, i) => ({ c, i })).filter(({ c }) => c.id !== "name");
  for (const { c, i } of rest) {
    free -= widthOf(c, i);
  }
  const minimum = floor * cellW;
  for (let k = rest.length - 1; k >= 0 && free < minimum; k -= 1) {
    const entry = rest[k];
    if (entry === undefined) {
      break;
    }
    dom.root.style.setProperty(`${colVar(entry.c.id)}-show`, "none");
    free += widthOf(entry.c, entry.i) + cellW;
  }
}
