// `Screen` painters for help (wave W10): functions with `this: Screen`,
// hooked in as properties in `render.ts`. State stays in the class.

import type { Screen } from "../render";
import type { HelpBlockView, HelpScrollTo, HelpSpanView, HelpView } from "../types";
import { revelar } from "./dom";

/**
 * Help (F1).
 *
 * Everything painted here arrives already resolved: the blocks are a CLOSED
 * vocabulary, the corpus's marks come converted to THIS reader's key, and a
 * disabled row's reasons come translated. That is why every block is built
 * with `createElement` and `textContent` and never with `innerHTML`: a
 * plugin's `help.md` is a third party's text, and the only reason it can be
 * painted at all is that it is never interpreted as markup.
 */
export function paintHelp(this: Screen, help: HelpView | null): void {
  if (help === null) {
    this.helpRoot.replaceChildren();
    this.helpRoot.dataset["open"] = "false";
    this.helpBodyFocused = false;
    this.helpPintada = null;
    // Every opening numbers its requests from 1 (the host creates a fresh
    // help): without this, the next opening's first one would be mistaken
    // for stale.
    this.helpScrollSeq = 0;
    return;
  }
  // Where it was reading, to give it back. The body is rebuilt whole on
  // EVERY patch — and moving the sidebar's cursor is a patch — so without
  // this, reading half a page and pressing `↓` reset the scroll to zero.
  // Only within the SAME page: changing pages starts at the top, which is
  // what any reader does.
  const scroll =
    this.helpPintada === help.topic_id
      ? (this.helpRoot.querySelector(".help-body")?.scrollTop ?? 0)
      : 0;
  this.helpPintada = help.topic_id;
  this.helpRoot.dataset["open"] = "true";
  this.helpBodyFocused = help.focus === "body";
  const box = document.createElement("section");
  box.className = "help";
  // Modal: while it is open, the keys are its own — and the host knows it,
  // so the screen reader has to know it too.
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", this.t("help-title"));

  box.append(this.helpSidebar(help), this.helpBody(help));

  const footer = document.createElement("footer");
  footer.className = "help-hint";
  footer.textContent = this.t("help-hint-gui");
  box.append(footer);
  this.helpRoot.replaceChildren(box);
  if (scroll > 0) {
    const body = this.helpRoot.querySelector(".help-body");
    if (body instanceof HTMLElement) {
      body.scrollTop = scroll;
    }
  }
  // The scroll request, ONCE: a patch that repaints help for another reason
  // carries the same request, with the same number.
  if (help.scroll !== null && help.scroll.seq > this.helpScrollSeq) {
    this.helpScrollSeq = help.scroll.seq;
    this.desplazarAyuda(help.scroll.to);
  }
}

/**
 * Scrolls help's body toward `to` (bridge 76).
 *
 * WHICH key means what is decided by the HOST, with the reader's keymap: the
 * key reaches it like any other screen and it answers with a request in
 * `HelpView.scroll`. The renderer used to handle `PageDown`, `Home`, `[`… as
 * fixed keys, and a rebind changed the terminal and not this window.
 *
 * HOW MUCH is a line, a page or where a section starts is measured by this
 * box, which is the only one that knows it (#267). And the renderer applies
 * it, not native scroll: that needs the document's focus, and the body is
 * rebuilt on every patch with nobody giving it back.
 */
export function desplazarAyuda(this: Screen, to: HelpScrollTo): void {
  const body = this.helpRoot.querySelector(".help-body");
  if (!(body instanceof HTMLElement)) {
    return;
  }
  // One line of prose, and a page with two lines of overlap so as not to
  // lose your place on the jump.
  const line = parseFloat(getComputedStyle(body).lineHeight) || 16;
  const page = Math.max(line, body.clientHeight - 2 * line);
  switch (to) {
    case "line_down":
      body.scrollTop += line;
      return;
    case "line_up":
      body.scrollTop -= line;
      return;
    case "page_down":
      body.scrollTop += page;
      return;
    case "page_up":
      body.scrollTop -= page;
      return;
    case "top":
      body.scrollTop = 0;
      return;
    case "bottom":
      body.scrollTop = body.scrollHeight;
      return;
    case "section_next":
    case "section_prev": {
      const forward = to === "section_next";
      const from = body.scrollTop;
      // The corpus's THREE levels (`helpBlock` paints them as h2..h4): the
      // terminal stops on all of them, and the window has to stop on the
      // same ones.
      const sections = [...body.querySelectorAll("h2, h3, h4")].filter(
        (h): h is HTMLElement => h instanceof HTMLElement,
      );
      const target = forward
        ? sections.find((h) => h.offsetTop > from + 1)
        : sections.reverse().find((h) => h.offsetTop < from - 1);
      body.scrollTop = target?.offsetTop ?? (forward ? body.scrollHeight : 0);
      return;
    }
  }
}

/** The sidebar: group headers and pages. */
export function helpSidebar(this: Screen, help: HelpView): HTMLElement {
  const nav = document.createElement("nav");
  nav.className = "help-topics";
  nav.dataset["focused"] = String(help.focus === "topics");
  if (help.filtering) {
    const filter = document.createElement("div");
    filter.className = "help-filter";
    filter.textContent = `/${help.filter}`;
    nav.append(filter);
  }
  const list = document.createElement("ul");
  list.setAttribute("role", "listbox");
  list.className = "help-topic-rows";
  for (const [i, r] of help.sidebar.entries()) {
    const row = document.createElement("li");
    row.id = `help-topic-${String(i)}`;
    if (r.row === "group") {
      // A header is NOT selectable: `presentation` takes it out of the
      // option count a screen reader announces.
      row.className = "help-group";
      row.setAttribute("role", "presentation");
      row.textContent = r.label;
    } else {
      row.className = "help-topic";
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", String(help.cursor === i));
      row.dataset["current"] = String(r.current);
      row.textContent = r.title;
      // The sidebar ellipsis-truncates long titles; the full one, on hover.
      // `title` is text: it is never interpreted as markup.
      row.title = r.title;
      row.addEventListener("click", () => {
        this.send({ action: "help_select_topic", row: i });
      });
    }
    list.append(row);
  }
  list.setAttribute("aria-activedescendant", `help-topic-${String(help.cursor)}`);
  nav.append(list);
  // The sidebar is longer than its box: without this, scrolling past the
  // fold moves a cursor that is not visible.
  revelar(list.children[help.cursor]);
  return nav;
}

/** The body: the page's prose and what can be run from it. */
export function helpBody(this: Screen, help: HelpView): HTMLElement {
  const body = document.createElement("article");
  body.className = "help-body";
  body.dataset["focused"] = String(help.focus === "body");
  // Focusable: this is what makes the page keys scroll THIS box and not the
  // window. `-1` because tab order is entered with the key help itself uses
  // to switch halves.
  body.setAttribute("tabindex", "-1");

  const title = document.createElement("h1");
  title.textContent = help.title;
  body.append(title);
  if (help.badge !== null) {
    // A third party page's provenance. Always visible on a plugin page: a
    // line that only sometimes shows up teaches the opposite of the truth
    // when it is missing.
    const badge = document.createElement("p");
    badge.className = "help-badge";
    badge.textContent = help.badge;
    body.append(badge);
  }
  const blocks = help.blocks.map((b) => this.helpBlock(b));
  // The PAGE's table of contents, at the top: its sections, each one a
  // button that brings it into view. Only with three or more — with one or
  // two, the index takes up more than it saves.
  const sections = blocks.filter((el) => el.tagName === "H2");
  if (sections.length >= 3) {
    const toc = document.createElement("nav");
    toc.className = "help-toc";
    toc.setAttribute("aria-label", this.t("help-toc"));
    for (const h of sections) {
      const go = document.createElement("button");
      go.type = "button";
      go.className = "help-toc-item";
      go.textContent = h.textContent;
      go.addEventListener("click", () => {
        body.scrollTop = h.offsetTop;
      });
      toc.append(go);
    }
    body.append(toc);
  }
  body.append(...blocks);
  if (help.actions.length > 0) {
    const list = document.createElement("ul");
    list.className = "help-actions";
    list.setAttribute("role", "listbox");
    for (const [i, a] of help.actions.entries()) {
      const row = document.createElement("li");
      row.className = "help-action";
      row.id = `help-action-${String(i)}`;
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", String(help.action_cursor === i));
      row.dataset["enabled"] = String(a.enabled);
      const chord = document.createElement("span");
      chord.className = "help-action-chord";
      chord.textContent = a.chord;
      const label = document.createElement("span");
      label.className = "help-action-label";
      label.textContent = a.label;
      row.append(chord, label);
      if (a.opens_topic) {
        // The arrow is the ONLY thing that tells apart "opens a page" from
        // "runs a command", so it goes in its own node — glued to the text
        // it ends up in the same bidi run as the label and can end up in
        // front — but INSIDE the label: as its sibling, the flex layout sent
        // it to the other end of the row, far from what it qualifies.
        const arrow = document.createElement("span");
        arrow.className = "help-action-opens";
        arrow.textContent = "→";
        label.append(arrow);
      }
      if (!a.enabled && a.reason !== "") {
        const reason = document.createElement("span");
        reason.className = "help-action-reason";
        reason.textContent = a.reason;
        row.append(reason);
      }
      if (a.enabled) {
        row.addEventListener("click", () => {
          this.send({ action: "help_activate", index: i });
        });
      }
      list.append(row);
    }
    if (help.action_cursor !== null) {
      list.setAttribute(
        "aria-activedescendant",
        `help-action-${String(help.action_cursor)}`,
      );
      revelar(list.children[help.action_cursor]);
    }
    body.append(list);
  }
  return body;
}

/** A corpus block, in its semantic element. */
export function helpBlock(this: Screen, b: HelpBlockView): HTMLElement {
  switch (b.block) {
    case "heading": {
      // The level arrives bounded to 1..=3 by the host, and the page's title
      // already occupies `h1`: a body heading starts at `h2`.
      const level = Math.min(3, Math.max(1, b.level)) + 1;
      const h = document.createElement(`h${String(level)}`);
      h.textContent = b.text;
      return h;
    }
    case "paragraph": {
      const p = document.createElement("p");
      p.append(...b.spans.map((s) => this.helpSpan(s)));
      return p;
    }
    case "bullets": {
      const ul = document.createElement("ul");
      ul.className = "help-bullets";
      for (const item of b.items) {
        const li = document.createElement("li");
        li.append(...item.map((s) => this.helpSpan(s)));
        ul.append(li);
      }
      return ul;
    }
    case "code": {
      const pre = document.createElement("pre");
      pre.className = "help-code";
      if (b.lang !== null) {
        pre.dataset["lang"] = b.lang;
      }
      const code = document.createElement("code");
      code.textContent = b.text;
      pre.append(code);
      return pre;
    }
    case "table": {
      const table = document.createElement("table");
      table.className = "help-table";
      const thead = document.createElement("thead");
      const headRow = document.createElement("tr");
      for (const c of b.header) {
        const th = document.createElement("th");
        th.setAttribute("scope", "col");
        th.textContent = c;
        headRow.append(th);
      }
      thead.append(headRow);
      const tbody = document.createElement("tbody");
      for (const r of b.rows) {
        const tr = document.createElement("tr");
        for (const c of r) {
          const td = document.createElement("td");
          td.textContent = c;
          tr.append(td);
        }
        tbody.append(tr);
      }
      table.append(thead, tbody);
      return table;
    }
    case "callout": {
      const aside = document.createElement("aside");
      aside.className = "help-callout";
      aside.dataset["kind"] = b.kind;
      const label = document.createElement("span");
      label.className = "help-callout-kind";
      label.textContent = this.t(`help-callout-${b.kind}`);
      aside.append(label);
      aside.append(...b.spans.map((s) => this.helpSpan(s)));
      return aside;
    }
    case "keys": {
      const table = document.createElement("table");
      table.className = "help-keys";
      const tbody = document.createElement("tbody");
      for (const r of b.rows) {
        const tr = document.createElement("tr");
        tr.dataset["enabled"] = String(r.enabled);
        const chord = document.createElement("th");
        chord.setAttribute("scope", "row");
        chord.className = "help-key-chord";
        chord.textContent = r.chord;
        const label = document.createElement("td");
        label.className = "help-key-label";
        label.textContent = r.label;
        tr.append(chord, label);
        // Dimming without saying why leaves the reader guessing whether the
        // window is broken. The reason goes in its OWN cell and not glued to
        // the text: run together, the dash and the reason end up in the same
        // bidi run as the label, and a label ending in strong RTL carries
        // them to the wrong side.
        if (!r.enabled && r.reason !== "") {
          const reason = document.createElement("td");
          reason.className = "help-key-reason";
          reason.textContent = r.reason;
          tr.append(reason);
        }
        tbody.append(tr);
      }
      table.append(tbody);
      return table;
    }
  }
}

/** An inline fragment. */
export function helpSpan(this: Screen, s: HelpSpanView): HTMLElement {
  switch (s.span) {
    case "text": {
      const span = document.createElement("span");
      span.textContent = s.text;
      return span;
    }
    case "strong": {
      const el = document.createElement("strong");
      el.textContent = s.text;
      return el;
    }
    case "emph": {
      const el = document.createElement("em");
      el.textContent = s.text;
      return el;
    }
    case "code": {
      const el = document.createElement("code");
      el.textContent = s.text;
      return el;
    }
    case "command": {
      // `kbd` only when it is a REAL key: when the command has no shortcut,
      // what travels is its name, and painting it as a key would show one
      // that does not exist.
      const el = document.createElement(s.is_chord ? "kbd" : "span");
      el.className = s.is_chord ? "help-chord" : "help-cmd";
      el.textContent = s.text;
      return el;
    }
    case "link": {
      // Since bridge 75 a `[[link]]` in the prose IS one of the page's action
      // rows, and clicking it activates that row: the same as Enter on it,
      // through the same host path. What travels is the INDEX, not the
      // target's key. With no row (`null`) it stays text: a control that
      // does nothing is worse than text that reads as a link.
      const el = document.createElement("span");
      el.className = "help-link";
      el.textContent = s.text;
      const row = s.action;
      if (row !== null) {
        el.setAttribute("role", "link");
        el.dataset["live"] = "true";
        el.addEventListener("click", () => {
          this.send({ action: "help_activate", index: row });
        });
      }
      return el;
    }
  }
}
