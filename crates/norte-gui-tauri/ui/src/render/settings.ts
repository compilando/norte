// `Screen` painters for settings (wave W10): functions with `this: Screen`,
// hooked in as properties in `render.ts`. State stays in the class.

import type { Screen } from "../render";
import type {
  ColumnsPickerView,
  LayoutPickerView,
  ProfilePickerView,
  PickerView,
  SettingRowView,
  SettingsView,
  ThemeView,
} from "../types";
import { revelar, badge } from "./dom";

/**
 * Settings (F11).
 *
 * Two kinds of section and no decision here: the host sends the registry
 * with its value already resolved and the locations already sanitized. The
 * only things this method knows are that a missing path row is stated, that
 * the list is a `listbox` with a cursor the host keeps, and that a double
 * click on a row activates it — whether to cycle it or ask for its value is
 * decided by the host.
 */
export function paintSettings(this: Screen, settings: SettingsView | null): void {
  if (settings === null) {
    this.settingsRoot.replaceChildren();
    this.settingsRoot.dataset["open"] = "false";
    return;
  }
  // The scroll position BEFORE rebuilding the list: the `<ul>` gets replaced
  // whole on every paint, and without this the wheel resets to zero every
  // time the host sends a patch.
  const previous = this.settingsRoot.querySelector(".settings-rows");
  const scroll = previous instanceof HTMLElement ? previous.scrollTop : 0;
  // The focus, BEFORE touching anything. Moving the bar to the new box
  // already takes the field out of the DOM for an instant, and that unfocuses
  // it: checking afterward would always see "it did not have it".
  const field = this.settingsBarra?.querySelector(".settings-search");
  const focused = field instanceof HTMLInputElement && document.activeElement === field;
  const caret: [number | null, number | null] =
    field instanceof HTMLInputElement
      ? [field.selectionStart, field.selectionEnd]
      : [null, null];
  this.settingsRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "settings";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", this.t("settings-title"));

  const title = document.createElement("h1");
  title.textContent = this.t("settings-title");
  box.append(title);

  // The search box. A real `<input>` and not a key that travels: printable
  // characters do not reach the host, which is why this screen had no filter
  // until now.
  //
  // REUSED between paints. Every keystroke triggers a host patch, i.e. a
  // repaint: a field that got recreated would be destroyed on the first
  // character and take the focus and the caret with it. Same bug and same
  // cure as the log's filter and a dialog's field.
  let bar = this.settingsBarra;
  if (bar === null) {
    bar = document.createElement("div");
    bar.className = "settings-search-bar";
    const input = document.createElement("input");
    input.className = "settings-search";
    input.type = "search";
    input.setAttribute("aria-label", this.t("settings-title"));
    input.addEventListener("input", () => {
      this.send({ action: "settings_query", text: input.value });
    });
    const count = document.createElement("span");
    count.className = "settings-count";
    bar.append(input, count);
    this.settingsBarra = bar;
  }
  const search = bar.querySelector(".settings-search");
  // Re-seeding it while it is being typed into would put the host's
  // projection back over what the reader is typing.
  if (search instanceof HTMLInputElement && !focused) {
    search.value = settings.query;
  }
  const count = bar.querySelector(".settings-count");
  if (count instanceof HTMLElement) {
    count.textContent = `${String(settings.shown)} / ${String(settings.total)}`;
  }
  box.append(bar);

  const body = document.createElement("div");
  body.className = "settings-body";
  body.append(indiceDeSecciones.call(this, settings));

  const list = document.createElement("ul");
  list.className = "settings-rows";
  list.setAttribute("role", "listbox");
  // The cursor counts SELECTABLE rows: headers do not count, so the index is
  // tracked apart from the sections' walk.
  let i = 0;
  for (const sec of settings.sections) {
    const header = document.createElement("li");
    header.className = "settings-group";
    header.setAttribute("role", "presentation");
    header.textContent = sec.title;
    // If ALL of a section's rows ask for a restart, it is said ONCE in its
    // header. Five identical badges inform of nothing: they make noise right
    // over what does vary, which is the value.
    const all =
      sec.section === "settings" &&
      sec.rows.length > 0 &&
      sec.rows.every((r) => r.restart_required);
    if (all) {
      const mark = document.createElement("span");
      mark.className = "settings-badge";
      mark.textContent = this.t("settings-restart-badge");
      header.append(" ", mark);
    }
    list.append(header);
    // The `switch` goes OUTSIDE the row loop: inside it, TypeScript cannot
    // narrow the row's type from the section, and a path row and a setting
    // row do not share a single field.
    if (sec.section === "settings") {
      for (const r of sec.rows) {
        const row = this.settingsRow(i, settings.cursor);
        // FOUR fixed cells and in this order, always: dot, name, value,
        // actions. The grid has four columns, so hanging one extra child
        // sends the overflow to a new row — that is how "Reset" used to
        // come out as a full-width box with the dot loose against the
        // right edge.
        const dot = document.createElement("span");
        dot.className = "settings-dot";
        dot.dataset["on"] = String(r.modified);
        if (r.modified) {
          // A dot WITH a label, not just color: color is not information
          // for whoever cannot tell it apart.
          dot.setAttribute("aria-label", this.t("settings-modified"));
          dot.textContent = "●";
        }
        const name = document.createElement("span");
        name.className = "settings-name";
        name.textContent = r.name;
        const value = controlDeAjuste.call(this, r);
        // The actions go TOGETHER in one cell: today, if it is touched, the
        // button that undoes it. The restart badge is NOT here: it is
        // information that only matters when changing that row, and
        // repeated across six lines at once it stops being readable (see
        // `settings-desc`).
        const actions = document.createElement("span");
        actions.className = "settings-actions";
        if (r.modified) {
          const revert = document.createElement("button");
          revert.className = "settings-reset";
          revert.type = "button";
          // An ICON with its label and its title: the text used to eat
          // thirty cells of every row to say the same thing as an undo
          // arrow, and it was on every touched row at once.
          revert.textContent = "↺";
          revert.setAttribute("aria-label", this.t("settings-reset"));
          revert.title = this.t("settings-reset");
          // THIS row's index, copied: `i` is ONE loop variable, and a
          // closure reading it on click would see the last one.
          const which = i;
          revert.addEventListener("click", (e) => {
            // No bubbling: the `<li>` carries a click that SELECTS and a
            // double click that activates, and resetting is neither.
            e.stopPropagation();
            this.send({ action: "settings_reset", row: which });
          });
          actions.append(revert);
        }
        // The description ALWAYS shows, dimmed and under the name, not only
        // on the chosen row: it is what says what a setting does, and
        // hiding it forces scanning the whole list to read it. With it, and
        // at the end, what used to be a pill repeated on every row.
        const desc = document.createElement("span");
        desc.className = "settings-desc";
        desc.textContent = r.desc;
        if (r.restart_required && !all) {
          const when = document.createElement("span");
          when.className = "settings-when";
          when.textContent = this.t("settings-restart-badge");
          desc.append(" ", when);
        }
        row.append(dot, name, value, actions, desc);
        list.append(row);
        i += 1;
      }
    } else {
      for (const r of sec.rows) {
        const row = this.settingsRow(i, settings.cursor);
        // The SAME four cells as a settings row, so both row kinds form a
        // column: a location has no dot, so its own goes empty instead of
        // missing.
        const dot = document.createElement("span");
        dot.className = "settings-dot";
        dot.dataset["on"] = "false";
        const name = document.createElement("span");
        name.className = "settings-name";
        name.textContent = r.label;
        const value = document.createElement("span");
        value.className = "settings-value";
        value.dataset["hostile"] = String(r.hostile);
        value.textContent = r.display;
        if (r.hostile) {
          value.append(badge(this.t("hostile-name")));
        }
        const actions = document.createElement("span");
        actions.className = "settings-actions";
        if (r.missing) {
          // A location not existing is a diagnostic FACT and not an error: a
          // layer nobody has created is normal.
          const missing = document.createElement("span");
          missing.className = "settings-missing";
          missing.textContent = this.t("settings-path-missing");
          actions.append(missing);
        }
        row.append(dot, name, value, actions);
        list.append(row);
        i += 1;
      }
    }
  }
  list.setAttribute("aria-activedescendant", `settings-row-${String(settings.cursor)}`);
  // Which half has the keyboard. BOTH cursors are always painted and the one
  // that does not have it is dimmed (ADR 0128): only one alive, or none, is
  // what makes it impossible to know where the focus is.
  list.dataset["focused"] = String(settings.focus === "list");
  body.append(list);
  box.append(body);
  // Keeping the node is NOT enough: moving it to the new box takes it out of
  // the DOM for an instant, and that already unfocuses it. It is given back
  // the focus — and the caret — it had at the start. Same cure a dialog's
  // field needed for the same reason.
  this.settingsRoot.replaceChildren(box);
  if (focused && search instanceof HTMLInputElement) {
    search.focus();
    if (caret[0] !== null && caret[1] !== null) {
      search.setSelectionRange(caret[0], caret[1]);
    }
  }
  list.scrollTop = scroll;
  revelar(objetivoRevelado(list, settings.cursor));
}

/**
 * One icon per section, by its STABLE key.
 *
 * Plain Unicode, not Nerd Font: the index has to read on a machine with no
 * icon fonts installed, and a section whose key is not here simply carries
 * no icon — the label, which is what gets read, is still there.
 */
const ICONO_DE_SECCION: Record<string, string> = {
  appearance: "◐",
  panes: "▤",
  "open-with": "↗",
  input: "⌨",
  behavior: "⚙",
  plugins: "✦",
  paths: "⌂",
};

/**
 * A row's CONTROL: switch, dropdown, number or text field, according to
 * what the host says it is.
 *
 * The class is sent by the host (`control`) and so are the accepted values
 * (`choices`, already resolved): nothing about what a setting accepts is
 * decided here, and nothing is validated. What gets typed or chosen is sent
 * with `settings_set` and accepted or rejected by the shared editor, the
 * same one the terminal's keyboard uses.
 *
 * None of these controls lets its interaction bubble: the `<li>` carries a
 * click that SELECTS and a double click that ACTIVATES, and flipping a
 * switch is neither.
 */
function controlDeAjuste(this: Screen, r: SettingRowView): HTMLElement {
  const box = document.createElement("span");
  box.className = "settings-value";
  box.dataset["control"] = r.control;
  const set = (value: string): void => {
    this.send({ action: "settings_set", id: r.id, value });
  };

  if (r.control === "toggle") {
    const sw = document.createElement("button");
    sw.className = "settings-switch";
    sw.type = "button";
    sw.setAttribute("role", "switch");
    const on = r.value === "true";
    sw.setAttribute("aria-checked", String(on));
    sw.dataset["on"] = String(on);
    // The state is said with TEXT in addition to position: a switch that is
    // only told apart by where the knob is does not read without seeing it.
    const knob = document.createElement("span");
    knob.className = "settings-switch-knob";
    sw.append(knob);
    sw.addEventListener("click", (e) => {
      e.stopPropagation();
      set(on ? "false" : "true");
    });
    box.append(sw);
    return box;
  }

  if (r.control === "choice" && r.choices.length > 0) {
    const sel = document.createElement("select");
    sel.className = "settings-select";
    for (const c of r.choices) {
      const op = document.createElement("option");
      op.value = c;
      op.textContent = c;
      op.selected = c === r.value;
      sel.append(op);
    }
    // A value the file carries that the list no longer recognizes — a
    // deleted theme, a renamed preset — is ADDED at the end instead of
    // disappearing: a dropdown that shows something other than what is set
    // lies about the configuration.
    if (!r.choices.includes(r.value) && r.value !== "") {
      const orphan = document.createElement("option");
      orphan.value = r.value;
      orphan.textContent = r.value;
      orphan.selected = true;
      sel.append(orphan);
    }
    sel.addEventListener("click", (e) => {
      e.stopPropagation();
    });
    sel.addEventListener("change", () => {
      set(sel.value);
    });
    box.append(sel);
    return box;
  }

  if (r.control === "number") {
    const num = document.createElement("input");
    num.className = "settings-number";
    num.type = "number";
    num.value = r.value;
    num.placeholder = r.default;
    if (r.min !== null) {
      num.min = String(r.min);
    }
    if (r.max !== null) {
      num.max = String(r.max);
    }
    campoQueGuarda.call(this, num, r, set);
    box.append(num);
    return box;
  }

  if (r.control === "text" || r.control === "args") {
    const field = document.createElement("input");
    field.className = "settings-text";
    field.type = "text";
    // Empty is NOT a gap: it is the factory value, and WHICH one is stated.
    // A sentence ("what norte ships with") takes the data's spot without
    // giving it.
    field.placeholder = r.default;
    // The value being EDITED is the real one, not the masked one: what is
    // painted in a list goes through the mask, but a field that returned the
    // sanitized text would save the replacement into the file.
    field.value = r.value;
    campoQueGuarda.call(this, field, r, set);
    box.append(field);
    return box;
  }

  // What is not edited from here — a plugin's summary — stays as text, with
  // its mark if the value came hostile.
  box.dataset["hostile"] = String(r.hostile);
  box.textContent = r.value;
  if (r.hostile) {
    box.append(badge(this.t("hostile-name")));
  }
  return box;
}

/**
 * A field that saves on blur or with Enter, and gives up with Escape.
 *
 * Not on every keystroke: every keypress would be a write to `norte.toml`
 * and a reload of the whole configuration. And it does not bubble: the
 * `<li>` underneath moves the cursor with the click.
 */
function campoQueGuarda(
  this: Screen,
  field: HTMLInputElement,
  r: SettingRowView,
  set: (v: string) => void,
): void {
  field.addEventListener("click", (e) => {
    e.stopPropagation();
  });
  field.addEventListener("blur", () => {
    if (field.value !== r.value) {
      set(field.value);
    }
  });
  field.addEventListener("keydown", (e) => {
    // A field's keys are ITS OWN: without this, the arrows move the list's
    // cursor behind it while typing, and `esc` closes all of settings
    // instead of giving up on the field.
    e.stopPropagation();
    if (e.key === "Enter") {
      field.blur();
    } else if (e.key === "Escape") {
      field.value = r.value;
      field.blur();
    }
  });
}

/**
 * The index on the left: every section this surface has, with how many of
 * its rows are visible.
 *
 * One the filter emptied stays here, dimmed: an index that changes length
 * while you type cannot be used as a map. What travels back on a click is
 * its STABLE key, so the jump does not depend on the language.
 */
function indiceDeSecciones(this: Screen, settings: SettingsView): HTMLElement {
  // Which section the cursor falls in. Counted over the same sections that
  // are painted, and matched by KEY: matching by the translated label would
  // break the day two are named similarly or someone tweaks a string.
  let seen = 0;
  let current: string | null = null;
  for (const sec of settings.sections) {
    const n = sec.rows.length;
    if (settings.cursor < seen + n) {
      current = sec.section === "settings" ? sec.key : "paths";
      break;
    }
    seen += n;
  }
  const nav = document.createElement("nav");
  nav.className = "settings-index";
  nav.setAttribute("aria-label", this.t("settings-title"));
  nav.dataset["focused"] = String(settings.focus === "index");
  for (const s of settings.index) {
    const item = document.createElement("button");
    item.className = "settings-index-item";
    item.type = "button";
    item.dataset["key"] = s.key;
    item.dataset["empty"] = String(s.visible === 0);
    item.disabled = s.visible === 0;
    if (s.key === current) {
      // THIS side's cursor. Always painted; the CSS dims it when the
      // keyboard is in the list.
      item.setAttribute("aria-current", "true");
    }
    // The icon is DECORATION: the label sits next to it and is what gets
    // read. That is why `aria-hidden` — a screen reader saying "palette,
    // Appearance" would be reading the same thing twice, the second time
    // wrong.
    const icon = document.createElement("span");
    icon.className = "settings-index-icon";
    icon.setAttribute("aria-hidden", "true");
    icon.textContent = ICONO_DE_SECCION[s.key] ?? "";
    const title = document.createElement("span");
    title.className = "settings-index-title";
    title.textContent = s.title;
    item.append(icon);
    const count = document.createElement("span");
    count.className = "settings-index-count";
    count.textContent = String(s.visible);
    item.append(title, count);
    item.addEventListener("click", () => {
      this.send({ action: "settings_jump_section", section: s.key });
    });
    nav.append(item);
  }
  return nav;
}

/**
 * WHAT has to be kept in view for the cursor: its row, or its section's
 * HEADER when the row is the first one in it.
 *
 * Revealing only the row leaves the header right above the edge, and the
 * reader loses the only label that says where they are: scrolling all the
 * way down and back up left "General" out forever. The terminal has the
 * same rule in its window reconciliation
 * (`SettingsState::reconcile_viewport`), written there because there the
 * scroll is ours and here it is the browser's.
 *
 * Kept separate and exported because `scrollIntoView` does not exist in
 * jsdom: what the tests can check is the CHOICE, not the scrolling.
 */
export function objetivoRevelado(
  lista: Element,
  cursor: number,
): HTMLElement | undefined {
  const row = lista.querySelector(`#settings-row-${String(cursor)}`);
  if (!(row instanceof HTMLElement)) {
    return undefined;
  }
  const previous = row.previousElementSibling;
  if (previous instanceof HTMLElement && previous.classList.contains("settings-group")) {
    return previous;
  }
  return row;
}

/**
 * The theme from the inside (F9).
 *
 * Each role with its color as a SWATCH, not as text: a `#2d4f8a` tells
 * nobody anything until it is seen next to the square it paints.
 */
export function paintTheme(this: Screen, theme: ThemeView | null): void {
  if (theme === null) {
    this.themeRoot.replaceChildren();
    this.themeRoot.dataset["open"] = "false";
    return;
  }
  this.themeRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "theme";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", this.t("theme-title"));

  const title = document.createElement("h1");
  title.textContent = `${this.t("theme-title")} · ${theme.name}`;
  box.append(title);

  // The theme list, with the cursor. Moving through it previews LIVE: the
  // whole window's colors have already changed by the time this paints, so
  // what is underneath is the pointed-to theme.
  if (theme.choices.length > 0) {
    const choose = document.createElement("ul");
    choose.className = "theme-choices";
    choose.setAttribute("role", "listbox");
    for (const [i, name] of theme.choices.entries()) {
      const row = document.createElement("li");
      row.className = "theme-choice";
      row.id = `theme-choice-${String(i)}`;
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", String(theme.cursor === i));
      row.textContent = name;
      choose.append(row);
    }
    choose.setAttribute("aria-activedescendant", `theme-choice-${String(theme.cursor)}`);
    box.append(choose);
  }

  if (theme.unsupported_effects.length > 0) {
    // They are NAMED. A retro theme that looks identical to the rest reads
    // as broken, and the user goes looking for the bug where it is not.
    const notice = document.createElement("p");
    notice.className = "theme-effects";
    notice.setAttribute("role", "note");
    const hostile = theme.unsupported_effects.some((e) => e.hostile);
    notice.textContent = `${this.t("theme-effects-unsupported")} ${theme.unsupported_effects
      .map((e) => e.key)
      .join(" · ")}`;
    notice.dataset["hostile"] = String(hostile);
    if (hostile) {
      // The keys come from the theme file: if they were masked, it is said.
      notice.classList.add("hostile");
      notice.append(badge(this.t("hostile-name")));
    }
    box.append(notice);
  }

  const sub = document.createElement("h2");
  sub.textContent = this.t("theme-roles");
  box.append(sub);

  const list = document.createElement("ul");
  list.className = "theme-roles";
  for (const r of theme.roles) {
    const row = document.createElement("li");
    row.className = "theme-role";
    const swatch = document.createElement("span");
    swatch.className = "theme-swatch";
    // Through CSSOM and not the `style` attribute: the CSP blocks it.
    swatch.style.setProperty("background-color", r.color);
    const name = document.createElement("span");
    name.className = "theme-role-name";
    name.textContent = r.role;
    const hex = document.createElement("span");
    hex.className = "theme-role-hex";
    hex.textContent = r.color;
    row.append(swatch, name, hex);
    list.append(row);
  }
  box.append(list);
  this.themeRoot.replaceChildren(box);
}

/**
 * The layout picker, with the SHAPE of the chosen one alongside.
 *
 * The thumbnail arrives as text lines painted by the same engine that lays
 * out the real screen, so it cannot lie about what will come out. Here it is
 * only put into a `<pre>`.
 */
/**
 * The COLUMNS picker: what gets painted, in what order and with what format.
 *
 * States its SCOPE in the title — one scheme or all — and in its footer that
 * the choice applies to THIS window and is not saved: this phase does not
 * write configuration, and staying quiet about it would leave the user
 * believing they just configured norte.
 */
export function paintColumns(this: Screen, columns: ColumnsPickerView | null): void {
  if (columns === null) {
    this.columnsRoot.replaceChildren();
    this.columnsRoot.dataset["open"] = "false";
    return;
  }
  this.columnsRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "columns-picker";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", columns.title);

  const title = document.createElement("h1");
  title.textContent = columns.title;
  box.append(title);

  const list = document.createElement("ul");
  list.className = "columns-rows";
  list.setAttribute("role", "listbox");
  for (const [i, r] of columns.rows.entries()) {
    const row = document.createElement("li");
    row.className = "columns-row";
    row.id = `columns-row-${String(i)}`;
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", String(columns.cursor === i));
    // On or not, and whether it can be touched: both to the screen reader,
    // not only to whoever is looking.
    row.setAttribute("aria-checked", String(r.enabled));
    row.dataset["enabled"] = String(r.enabled);
    row.dataset["fixed"] = String(r.fixed);

    const mark = document.createElement("span");
    mark.className = "columns-check";
    mark.textContent = r.enabled ? "☑" : "☐";
    const name = document.createElement("span");
    name.className = "columns-label";
    name.dataset["hostile"] = String(r.hostile);
    name.textContent = r.label;
    if (r.hostile) {
      name.append(badge(this.t("hostile-name")));
    }
    row.append(mark, name);
    if (r.format !== "") {
      // The current format. Locked = a scheme setting fixes it and it is not
      // cycled here; painted dimmed instead of disappearing, because a key
      // that does nothing and does not say why is worse.
      const fmt = document.createElement("span");
      fmt.className = "columns-format";
      fmt.dataset["locked"] = String(r.format_locked);
      fmt.textContent = r.format;
      row.append(fmt);
    }
    list.append(row);
  }
  if (columns.cursor < columns.rows.length) {
    list.setAttribute("aria-activedescendant", `columns-row-${String(columns.cursor)}`);
  }
  box.append(list);

  const note = document.createElement("p");
  note.className = "columns-note";
  note.setAttribute("role", "note");
  note.textContent = columns.note;
  box.append(note);

  const footer = document.createElement("footer");
  footer.className = "columns-hint";
  // From the HOST (#287): `dialog.*` verbs can be rebound, and a string here
  // naming specific keys stops being true the moment someone does.
  footer.textContent = columns.hint;
  box.append(footer);
  this.columnsRoot.replaceChildren(box);
}

/**
 * The PROFILE picker (ADR 0079).
 *
 * A row that cannot load is SHOWN with its reason instead of disappearing:
 * hiding a directory the reader created is worse than showing it broken.
 * And the two warnings the spec asks for by name — what else is named the
 * same, and which profile cannot save state — go on the row, not in a
 * footnote nobody connects to it.
 */
export function paintProfiles(this: Screen, profiles: ProfilePickerView | null): void {
  if (profiles === null) {
    this.profilesRoot.replaceChildren();
    this.profilesRoot.dataset["open"] = "false";
    return;
  }
  this.profilesRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "profiles";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", this.t("profile-picker-title"));

  const title = document.createElement("h1");
  title.textContent = this.t("profile-picker-title");
  box.append(title);

  const list = document.createElement("ul");
  list.className = "profiles-rows";
  list.setAttribute("role", "listbox");
  for (const [i, r] of profiles.rows.entries()) {
    const row = document.createElement("li");
    row.className = "profiles-row";
    row.id = `profile-row-${String(i)}`;
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", String(profiles.cursor === i));
    row.dataset["active"] = String(r.active);
    row.dataset["broken"] = String(r.problem !== "");
    row.addEventListener("click", () => {
      this.send({
        action: "profile_activate_row",
        row: i,
        generation: profiles.generation,
      });
    });
    const name = document.createElement("span");
    name.className = "profiles-name";
    name.textContent = r.name;
    if (r.name_hostile) {
      // The name is a directory's bytes: if it was masked, it is said.
      name.append(badge(this.t("hostile-name")));
    }
    row.append(name);
    if (r.title !== null) {
      const t = document.createElement("span");
      t.className = "profiles-title";
      t.textContent = r.title;
      row.append(t);
    }
    // The notes' order is the terminal's: first why it does NOT load, then
    // what it will not be able to save, and last the name clash. From most
    // to least serious.
    const note =
      r.problem !== ""
        ? r.problem
        : r.no_state
          ? this.t("profile-picker-no-state")
          : r.clash;
    if (note !== "") {
      const n = document.createElement("span");
      n.className = "profiles-note";
      n.textContent = note;
      row.append(n);
    }
    list.append(row);
  }
  list.setAttribute("aria-activedescendant", `profile-row-${String(profiles.cursor)}`);
  if (profiles.rows.length === 0) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent = this.t("profile-picker-empty");
    list.append(empty);
  }
  box.append(list);
  this.profilesRoot.replaceChildren(box);
}

export function paintLayouts(this: Screen, layouts: LayoutPickerView | null): void {
  if (layouts === null) {
    this.layoutsRoot.replaceChildren();
    this.layoutsRoot.dataset["open"] = "false";
    return;
  }
  this.layoutsRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "layouts";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", layouts.title);

  const title = document.createElement("h1");
  title.textContent = layouts.title;
  box.append(title);

  const body = document.createElement("div");
  body.className = "layouts-body";
  const list = document.createElement("ul");
  list.className = "layouts-rows";
  list.setAttribute("role", "listbox");
  for (const [i, r] of layouts.rows.entries()) {
    const row = document.createElement("li");
    row.className = "layouts-row";
    row.id = `layout-row-${String(i)}`;
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", String(layouts.cursor === i));
    row.dataset["broken"] = String(r.broken);
    row.addEventListener("click", () => {
      this.send({ action: "layout_activate_row", row: i });
    });
    const name = document.createElement("span");
    name.className = "layouts-name";
    name.dataset["hostile"] = String(r.hostile);
    name.textContent = r.name;
    if (r.hostile) {
      name.append(badge(this.t("hostile-name")));
    }
    row.append(name);
    if (r.factory) {
      const mark = document.createElement("span");
      mark.className = "layouts-tag";
      mark.textContent = this.t("layout-picker-factory");
      row.append(mark);
    }
    if (r.shares_keymap_name) {
      // It is WARNED: choosing this layout does not change a single key, and
      // without the line the name match is a trap.
      const notice = document.createElement("span");
      notice.className = "layouts-warn";
      notice.textContent = this.t("layout-picker-shares-keymap");
      row.append(notice);
    }
    list.append(row);
  }
  list.setAttribute("aria-activedescendant", `layout-row-${String(layouts.cursor)}`);
  body.append(list);

  if (layouts.problem === "") {
    const preview = document.createElement("pre");
    preview.className = "layouts-preview";
    preview.setAttribute("aria-hidden", "true");
    preview.textContent = layouts.preview.join("\n");
    body.append(preview);
  } else {
    const broken = document.createElement("p");
    broken.className = "layouts-problem";
    broken.textContent = layouts.problem;
    broken.dataset["hostile"] = String(layouts.problem_hostile);
    if (layouts.problem_hostile) {
      broken.classList.add("hostile");
      broken.append(badge(this.t("hostile-name")));
    }
    body.append(broken);
  }
  box.append(body);
  this.layoutsRoot.replaceChildren(box);
  revelar(list.querySelector(`#layout-row-${String(layouts.cursor)}`) ?? undefined);
}

/** The volume picker. */
export function paintPicker(this: Screen, picker: PickerView | null): void {
  if (picker === null) {
    this.pickerRoot.replaceChildren();
    this.pickerRoot.dataset["open"] = "false";
    return;
  }
  this.pickerRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "picker";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", picker.title);

  const title = document.createElement("h1");
  title.textContent = picker.title;
  box.append(title);

  if (picker.empty !== "") {
    // The sentence is written by the host: it tells apart "still asking"
    // from "there are none", the distinction an empty list swallows.
    const empty = document.createElement("p");
    empty.className = "picker-empty";
    empty.setAttribute("role", "status");
    empty.textContent = picker.empty;
    box.append(empty);
  }

  const list = document.createElement("ul");
  list.className = "picker-rows";
  list.setAttribute("role", "listbox");
  for (const [i, r] of picker.rows.entries()) {
    const row = document.createElement("li");
    row.className = "picker-row";
    row.id = `picker-row-${String(i)}`;
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", String(picker.cursor === i));
    row.addEventListener("click", () => {
      // THIS paint's generation: if the list changed between the paint and
      // the click, the host rejects it instead of choosing a different row.
      this.send({
        action: "picker_select_row",
        row: i,
        generation: picker.generation,
      });
    });
    const label = document.createElement("span");
    label.className = "picker-label";
    label.dataset["hostile"] = String(r.hostile);
    label.textContent = r.label;
    if (r.hostile) {
      label.append(badge(this.t("hostile-name")));
    }
    const detail = document.createElement("span");
    detail.className = "picker-detail";
    detail.textContent = r.detail;
    row.append(label, detail);
    list.append(row);
  }
  if (picker.cursor !== null) {
    list.setAttribute("aria-activedescendant", `picker-row-${String(picker.cursor)}`);
    revelar(list.querySelector(`#picker-row-${String(picker.cursor)}`) ?? undefined);
  }
  box.append(list);
  this.pickerRoot.replaceChildren(box);
}

/**
 * A settings row's `<li>`, with its cursor, its click and its double click.
 *
 * The click SELECTS and the double click ACTIVATES, as in a listing: the
 * first moves the cursor and the second does what `enter` does. Both
 * travel — a double click is also a click — and the host orders them.
 */
export function settingsRow(this: Screen, i: number, cursor: number): HTMLElement {
  const row = document.createElement("li");
  row.className = "settings-row";
  row.id = `settings-row-${String(i)}`;
  row.setAttribute("role", "option");
  row.setAttribute("aria-selected", String(cursor === i));
  row.addEventListener("click", () => {
    this.send({ action: "settings_select_row", row: i });
  });
  row.addEventListener("dblclick", () => {
    this.send({ action: "settings_activate", row: i });
  });
  return row;
}
