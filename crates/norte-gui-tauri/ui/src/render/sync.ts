// `Screen` painters for sync (wave W10): functions with `this: Screen`,
// hooked in as properties in `render.ts`. State stays in the class.

import type { Screen } from "../render";
import type { CompareFaceView, CompareView, SyncStepView, SyncView } from "../types";
import { badge, veredicto } from "./dom";

/** The sync panel: the PLAN. Painted in the same slot as the diff one —
 *  they are two whole screens and never overlap. */
export function paintSync(this: Screen, sync: SyncView | null): void {
  if (sync === null) {
    if (this.syncRoot.dataset["open"] === "true") {
      this.syncRoot.replaceChildren();
      this.syncRoot.dataset["open"] = "false";
    }
    return;
  }
  this.syncRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "sync";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", this.t("sync-title"));

  // The MODE, at the top and in its own element: a mirror deletes on the
  // destination and an update does not, and whoever approves has to see it
  // first.
  const mode = document.createElement("p");
  mode.className = "sync-mode";
  mode.dataset["mode"] = sync.mode;
  // LITERAL keys: an interpolated one is not seen by the sweep that checks
  // every key exists, and a missing key gets painted as its own identifier.
  mode.textContent =
    sync.mode === "mirror"
      ? this.t("gui-sync-mode-mirror")
      : this.t("gui-sync-mode-update");
  box.append(mode);

  const roots = document.createElement("div");
  roots.className = "sync-roots";
  for (const root of [sync.source, sync.dest]) {
    const r = document.createElement("span");
    r.className = "sync-root";
    r.dataset["hostile"] = String(root.hostile);
    r.textContent = root.text;
    if (root.hostile) {
      r.append(badge(this.t("hostile-name")));
    }
    roots.append(r);
  }
  box.append(roots);

  if (sync.summary.length > 0) {
    // The SUMMARY, at the top: how many steps cannot be undone, how many
    // bytes, what could not be read. It is what gets read before approving,
    // and below the list nobody reads it.
    const summary = document.createElement("ul");
    summary.className = "sync-summary";
    for (const line of sync.summary) {
      const li = document.createElement("li");
      li.textContent = line;
      summary.append(li);
    }
    box.append(summary);
  }
  if (sync.blockers.length > 0) {
    // What BLOCKS applying goes as an ALERT and at the top: a plan that
    // cannot run has to say why before showing its steps.
    const list = document.createElement("ul");
    list.className = "sync-blockers";
    list.setAttribute("role", "alert");
    for (const b of sync.blockers) {
      const li = document.createElement("li");
      const what = document.createElement("span");
      what.className = "sync-blocker-label";
      what.textContent = b.label;
      // The path in its own element: "the destination is read-only" without
      // saying WHICH one sends you hunting for the problem blind.
      const where = document.createElement("span");
      where.className = "sync-blocker-path";
      where.dataset["hostile"] = String(b.path_hostile);
      where.textContent = b.path;
      li.append(what, where);
      if (b.path_hostile) {
        li.append(badge(this.t("hostile-name")));
      }
      list.append(li);
    }
    if (sync.blockers_total > sync.blockers.length) {
      // The wire truncates the list: that there are forty thousand and two
      // hundred fifty-six are shown has to be said.
      const more = document.createElement("li");
      more.className = "sync-blockers-more";
      more.textContent = `${String(sync.blockers.length)} / ${String(sync.blockers_total)}`;
      list.append(more);
    }
    box.append(list);
  }

  const steps = document.createElement("ol");
  steps.className = "sync-steps";
  steps.setAttribute("role", "list");
  // The numbering starts where the WINDOW starts: the list is not the whole
  // plan, and painting it from one would pass it off as the whole thing.
  steps.setAttribute("start", String(sync.first_visible + 1));
  if (sync.total > sync.steps.length) {
    steps.dataset["window"] = `${String(sync.first_visible + 1)}-${String(
      sync.first_visible + sync.steps.length,
    )}/${String(sync.total)}`;
  }
  for (const p of sync.steps) {
    steps.append(this.syncStep(p));
  }
  box.append(steps);

  if (sync.failures.length > 0) {
    // What FAILED, one by one: the count is in the status, and "3 failed"
    // without saying which ones cannot be fixed.
    const failures = document.createElement("ul");
    failures.className = "sync-failures";
    failures.setAttribute("role", "alert");
    for (const f of sync.failures) {
      const li = document.createElement("li");
      li.dataset["anchor"] = f.anchor;
      const cause = document.createElement("span");
      cause.className = "sync-failure-cause";
      cause.textContent = f.cause;
      const path = document.createElement("span");
      path.className = "sync-failure-path";
      path.dataset["hostile"] = String(f.path_hostile);
      path.textContent = f.path;
      li.append(cause, path);
      if (f.anchor_label !== "") {
        const anchor = document.createElement("span");
        anchor.className = "sync-failure-anchor";
        anchor.textContent = f.anchor_label;
        li.append(anchor);
      }
      if (f.path_hostile) {
        li.append(badge(this.t("hostile-name")));
      }
      failures.append(li);
    }
    box.append(failures);
  }
  if (sync.confirming !== null) {
    // The SECOND question, as an alert and in its own element: it is the
    // last screen where saying no is still possible.
    const question = document.createElement("p");
    question.className = "sync-confirm";
    question.setAttribute("role", "alertdialog");
    question.textContent = sync.confirming;
    box.append(question);
  }
  const status = document.createElement("p");
  status.className = "sync-status";
  status.setAttribute("role", "status");
  status.setAttribute("aria-live", "polite");
  status.dataset["running"] = String(sync.running);
  status.dataset["approvable"] = String(sync.can_approve);
  status.dataset["cancelRequested"] = String(sync.cancel_requested);
  status.textContent = sync.status;
  box.append(status);

  const footer = document.createElement("p");
  footer.className = "sync-hint";
  footer.textContent = sync.hint;
  box.append(footer);
  this.syncRoot.replaceChildren(box);
}

/** A plan step: what it does, on what, and whether undo restores it. */
export function syncStep(this: Screen, p: SyncStepView): HTMLElement {
  const li = document.createElement("li");
  li.className = "sync-step";
  li.id = `sync-step-${String(p.id)}`;
  li.dataset["anchor"] = p.anchor;
  const kind = document.createElement("span");
  kind.className = "sync-step-kind";
  kind.textContent = p.kind;
  const path = document.createElement("span");
  path.className = "sync-step-path";
  path.dataset["hostile"] = String(p.path_hostile);
  path.textContent = p.path;
  li.append(kind, path);
  if (p.anchor_label !== "") {
    // The anchor is STATED, not deduced from a `data-anchor` nobody reads.
    const anchor = document.createElement("span");
    anchor.className = "sync-step-anchor";
    anchor.textContent = p.anchor_label;
    li.append(anchor);
  }
  if (p.path_hostile) {
    li.append(badge(this.t("hostile-name")));
  }
  if (p.dest_path !== null) {
    // The DESTINATION's spelling in its own element: the write lands on
    // THIS one, and merging them into one cell lets one name impersonate
    // another.
    const dest = document.createElement("span");
    dest.className = "sync-step-dest";
    dest.dataset["hostile"] = String(p.dest_path_hostile);
    dest.textContent = p.dest_path;
    li.append(dest);
    if (p.dest_path_hostile) {
      li.append(badge(this.t("hostile-name")));
    }
    if (p.twins) {
      // Both come out THE SAME: without saying so, the panel looks like it
      // is repeating itself.
      const twins = document.createElement("span");
      twins.className = "sync-step-twins";
      twins.textContent = this.t("sync-dest-twin");
      li.append(twins);
    }
  }
  const undo = document.createElement("span");
  undo.className = "sync-step-undo";
  undo.textContent = p.undo;
  li.append(undo);
  if (p.reason !== "") {
    const reason = document.createElement("span");
    reason.className = "sync-step-reason";
    reason.textContent = p.reason;
    li.append(reason);
  }
  return li;
}

/** The diff panel. Shares a slot with search: both are whole screens and
 *  are never painted at the same time. */
export function paintCompare(this: Screen, compare: CompareView | null): void {
  if (compare === null) {
    if (this.compareRoot.dataset["open"] === "true") {
      this.compareRoot.replaceChildren();
      this.compareRoot.dataset["open"] = "false";
    }
    return;
  }
  this.compareRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "compare";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", this.t("compare-title"));

  const header = document.createElement("div");
  header.className = "compare-roots";
  for (const [text, hostile] of [
    [compare.left, compare.left_hostile],
    [compare.right, compare.right_hostile],
  ] as [string, boolean][]) {
    const root = document.createElement("span");
    root.className = "compare-root";
    root.dataset["hostile"] = String(hostile);
    root.textContent = text;
    if (hostile) {
      root.append(badge(this.t("hostile-name")));
    }
    header.append(root);
  }
  box.append(header);

  const filters = document.createElement("div");
  filters.className = "compare-filters";
  for (const f of compare.filters) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "compare-filter";
    b.dataset["hidden"] = String(f.hidden);
    b.setAttribute("aria-pressed", String(!f.hidden));
    b.textContent = `${f.label} (${String(f.count)})`;
    b.addEventListener("click", () => {
      this.send({ action: "compare_toggle_filter", category: f.id });
    });
    filters.append(b);
  }
  box.append(filters);

  const list = document.createElement("ul");
  list.className = "compare-rows";
  list.setAttribute("role", "listbox");
  for (const r of compare.rows) {
    const row = document.createElement("li");
    row.className = "compare-row";
    // The id, not the position: it is the row's identity and what the host
    // expects back.
    row.id = `compare-row-${String(r.id)}`;
    row.dataset["category"] = r.category;
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", String(compare.selected === r.id));
    row.addEventListener("click", () => {
      this.send({ action: "compare_select_row", id: r.id });
    });
    row.addEventListener("dblclick", () => {
      this.send({ action: "compare_activate_row", id: r.id });
    });
    row.append(
      this.compareFace(r.left),
      veredicto(r, (k) => this.t(k)),
      this.compareFace(r.right),
    );
    if (r.paired_under !== null) {
      // A sentence on its own line, NEVER glued to the name: what gets glued
      // to a name can be forged by a name.
      const note = document.createElement("p");
      note.className = "compare-paired-under";
      note.textContent = r.paired_under;
      row.append(note);
    }
    list.append(row);
  }
  if (compare.selected !== null) {
    list.setAttribute("aria-activedescendant", `compare-row-${String(compare.selected)}`);
  }
  box.append(list);

  const status = document.createElement("p");
  status.className = "compare-status";
  status.setAttribute("role", "status");
  status.setAttribute("aria-live", "polite");
  status.dataset["running"] = String(compare.running);
  status.textContent = compare.status;
  box.append(status);
  this.compareRoot.replaceChildren(box);
}

/** One side of a compared row, or the gap for an orphan. */
export function compareFace(this: Screen, face: CompareFaceView | null): HTMLElement {
  const el = document.createElement("span");
  el.className = "compare-face";
  if (face === null) {
    // Empty and STATED: an orphan has nothing on this side, and a blank cell
    // with nothing else reads as a nameless file.
    el.dataset["absent"] = "true";
    el.textContent = "—";
    return el;
  }
  el.dataset["dir"] = String(face.is_dir);
  el.dataset["hostile"] = String(face.hostile);
  const name = document.createElement("span");
  name.className = "compare-name";
  name.textContent = face.name;
  el.append(name);
  if (face.hostile) {
    el.append(badge(this.t("hostile-name")));
  }
  // Size and date only when known: empty is ABSENCE, not zero.
  for (const [cls, text] of [
    ["compare-size", face.size],
    ["compare-mtime", face.mtime],
  ] as [string, string][]) {
    if (text === "") {
      continue;
    }
    const cell = document.createElement("span");
    cell.className = cls;
    cell.textContent = text;
    el.append(cell);
  }
  return el;
}
