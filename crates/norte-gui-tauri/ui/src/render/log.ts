// `Screen` painters for log (wave W10): functions with `this: Screen`, hooked
// in as properties in `render.ts`. State stays in the class.

import type { Screen } from "../render";
import type { LogSlotView } from "../types";
import { note, chip, badge } from "./dom";
import type { SlotDom } from "./dom";

/**
 * The log panel (#326): what this process is logging.
 *
 * The header carries three things the panel cannot stay silent about. LEVEL
 * and FILTER, because a panel that looks empty with a filter set reads as a
 * broken panel. Whether it is stuck to the end, because "nothing is
 * happening" and "you have detached and this is history" are
 * indistinguishable without saying so. And which PROCESS the lines belong
 * to: the window starts its own daemon, so the daemon's own lines — the
 * providers, the journal, the policy — are NOT here, and whoever opens it
 * looking for the reason a connection failed will not find it.
 *
 * Lines dropped by the ring are also stated: a log with a silent hole lies
 * about what happened, because a missing line is indistinguishable from the
 * event never occurring.
 */
export function paintLog(this: Screen, dom: SlotDom, slot: LogSlotView): void {
  dom.root.setAttribute("aria-label", this.t("log-title"));
  dom.scroller.className = "log";
  dom.title.replaceChildren(
    document.createTextNode(this.t("log-title")),
    // The LABEL, not the wire id: the chip said `trace` while the buttons
    // next to it said "traza" and each line said `trace` again. `TRACE` is
    // what the terminal paints, what gets written in `RUST_LOG` and what
    // someone scans for by eye in a long list.
    chip(`${this.t("log-level")}: ${slot.level_label ?? slot.level}`),
    ...(slot.filter === "" ? [] : [chip(`/${slot.filter}`)]),
    ...(slot.following ? [] : [chip(this.t("log-detached"))]),
    // MORE is being kept than what is shown: whoever is looking has a right
    // to know, especially before taking a screenshot.
    ...(slot.capturing === "" ? [] : [chip(slot.capturing)]),
    ...(slot.dropped_note === "" ? [] : [chip(slot.dropped_note)]),
    // The source. With a daemon that serves its own log this is a SELECTOR
    // — a click cycles window, daemon and both; without one it is a label,
    // because a control between three views of the same ring promises
    // something that does not exist. The host already collapses `both` to
    // `window` in that case, so here only whether it can be clicked needs
    // deciding.
    slot.sources_available ? this.sourceSelector(slot) : chip(slot.source),
    // And whatever needs to be said about it: that the daemon does not
    // serve its log, or whose level is being shown.
    ...(slot.source_note === "" ? [] : [chip(slot.source_note)]),
  );
  // The controls block is REUSED as long as it is still the same slot. It
  // used to be created on every repaint, and since every filter keystroke
  // triggers a frame — i.e. a repaint — the field was destroyed on the first
  // character and focus and caret were lost. Same bug a dialog's field
  // already had, and the same cure: keep the node.
  let controls = this.logControls;
  if (controls === null || this.logControlsSlot !== slot.slot_id) {
    controls = this.createLogControls();
    this.logControls = controls;
    this.logControlsSlot = slot.slot_id;
  }
  for (const b of controls.querySelectorAll("button[data-level]")) {
    const el = b as HTMLElement;
    el.dataset["on"] = String(el.dataset["level"] === slot.level);
  }
  const filter = controls.querySelector(".log-filter");
  // Only if it is NOT being typed into: reseeding it while it has focus
  // would drop the host's projection on top of what the reader is typing.
  if (filter instanceof HTMLInputElement && document.activeElement !== filter) {
    filter.value = slot.filter;
  }
  const follow = controls.querySelector(".log-follow");
  if (follow instanceof HTMLButtonElement) {
    follow.disabled = slot.following;
  }

  const list = document.createElement("ul");
  list.className = "log-lines";
  list.setAttribute("role", "log");
  for (const l of slot.lines) {
    const row = document.createElement("li");
    row.className = "log-line";
    row.dataset["level"] = l.level;
    // Which process it came from. In the merged list this is what tells
    // apart "the provider failed" from "the window could not paint it",
    // which read the same and are two different failures.
    row.dataset["source"] = l.source;
    const time = document.createElement("span");
    time.className = "log-time";
    time.textContent = l.time;
    const level = document.createElement("span");
    level.className = "log-level";
    level.textContent = l.level_label ?? l.level;
    const target = document.createElement("span");
    target.className = "log-target";
    target.textContent = l.target;
    const msg = document.createElement("span");
    msg.className = "log-message";
    msg.textContent = l.message;
    row.append(time, level, target, msg);
    if (l.hostile) {
      row.append(badge(this.t("hostile-name")));
    }
    list.append(row);
  }
  // The wheel scrolls the log through the HOST, not the DOM: it decides the
  // visible window, and letting the browser scroll a chunk that only has the
  // visible lines would not get anywhere.
  dom.scroller.onwheel = (e) => {
    e.preventDefault();
    this.send({ action: "log_scroll", delta: e.deltaY > 0 ? 3 : -3 });
  };
  // An empty panel SAYS SO. Without this, "there is nothing", "the filter
  // eats everything" and "this process has no ring" all paint the same: a
  // blank box, which reads as a broken panel.
  const body: HTMLElement = slot.lines.length === 0 ? note(this.t("log-empty")) : list;
  dom.scroller.replaceChildren(controls, body);
  this.scheduleLogRows(dom);
}

/**
 * The log's source selector (#328).
 *
 * Created on every paint and not reused like the controls block: it has no
 * DOM state to lose — no focus, no caret — and its label changes with the
 * source, which is exactly what needs repainting.
 *
 * `data-source` carries the WIRE identifier and not the translated label: it
 * is what lets a test check which one is set without tying it to the
 * language, the same rule as the level buttons.
 */
export function sourceSelector(this: Screen, slot: LogSlotView): HTMLElement {
  const b = document.createElement("button");
  b.type = "button";
  b.className = "chip log-source";
  b.dataset["source"] = slot.source_mode;
  b.textContent = slot.source;
  b.addEventListener("click", () => {
    this.send({ action: "log_cycle_source" });
  });
  return b;
}

/**
 * The log's controls, built ONCE per slot.
 *
 * Separate from painting because they carry DOM state that cannot be thrown
 * away on every frame: the filter's focus and caret.
 */
export function createLogControls(this: Screen): HTMLElement {
  const controls = document.createElement("div");
  controls.className = "log-controls";
  // One button per value of the CLOSED vocabulary. Compared by the wire
  // identifier and not by its translated label: comparing translated
  // sentences would tie the level to the language.
  for (const level of ["error", "warn", "info", "debug", "trace"]) {
    const b = document.createElement("button");
    b.type = "button";
    b.dataset["level"] = level;
    b.textContent = this.t(`log-level-${level}`);
    b.addEventListener("click", () => {
      this.send({ action: "log_set_level", level });
    });
    controls.append(b);
  }
  const filter = document.createElement("input");
  filter.type = "text";
  filter.className = "log-filter";
  filter.placeholder = this.t("log-filter");
  filter.setAttribute("aria-label", this.t("log-filter"));
  filter.addEventListener("input", () => {
    this.send({ action: "log_set_filter", filter: filter.value });
  });
  controls.append(filter);
  const follow = document.createElement("button");
  follow.type = "button";
  follow.className = "log-follow";
  follow.textContent = this.t("log-follow");
  follow.addEventListener("click", () => {
    this.send({ action: "log_follow" });
  });
  controls.append(follow);
  return controls;
}

/**
 * How many lines fit, measured from the DOM and sent to the host.
 *
 * The host cannot guess it, and while nobody told it, it stayed at its
 * startup value — ONE row — so the panel showed one truncated line inside a
 * box for twelve, and the wheel skipped two per notch. Same measurement the
 * listing makes and for the same reason: the visible window is decided by
 * whoever paints it.
 */
export function scheduleLogRows(this: Screen, dom: SlotDom): void {
  if (this.pendingLogRows !== null) {
    return;
  }
  this.pendingLogRows = requestAnimationFrame(() => {
    this.pendingLogRows = null;
    const { h } = this.cell();
    const body = dom.scroller.querySelector(".log-lines, .slot-note");
    const height =
      body instanceof HTMLElement ? body.clientHeight : dom.scroller.clientHeight;
    const rows = Math.max(1, Math.floor(height / h));
    if (this.logRows === rows) {
      return;
    }
    this.logRows = rows;
    this.send({ action: "log_set_visible_range", rows });
  });
}
