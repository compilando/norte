// `Screen` painters for search (wave W10): functions with `this: Screen`,
// hooked in as properties in `render.ts`. State stays in the class.

import type { Screen } from "../render";
import type { SearchView } from "../types";
import { revelar, badge } from "./dom";

export function paintSearch(this: Screen, search: SearchView | null): void {
  if (search === null) {
    this.searchRoot.replaceChildren();
    this.searchRoot.dataset["open"] = "false";
    return;
  }
  this.searchRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "search";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  // A search by meaning does not walk a subtree: its scope is the whole
  // index, and titling it like the other one would promise what is not
  // there.
  const label = search.semantic
    ? this.t("search-title-semantic")
    : this.t("search-title");
  box.setAttribute("aria-label", label);

  const title = document.createElement("h1");
  title.textContent = `${label} · ${search.query}`;
  box.append(title);

  if (search.semantic) {
    const scope = document.createElement("p");
    scope.className = "search-root";
    scope.textContent = this.t("modal-semantic-scope");
    box.append(scope);
  } else {
    const where = document.createElement("p");
    where.className = "search-root";
    where.dataset["hostile"] = String(search.root_hostile);
    where.textContent = search.root;
    if (search.root_hostile) {
      where.append(badge(this.t("hostile-name")));
    }
    box.append(where);
  }

  const status = document.createElement("p");
  status.className = "search-status";
  status.dataset["running"] = String(search.running);
  // `status` while it runs: a screen reader announces progress without
  // stealing focus from whatever the user is doing.
  status.setAttribute("role", "status");
  status.setAttribute("aria-live", "polite");
  status.textContent = search.status;
  box.append(status);

  const list = document.createElement("ul");
  list.className = "search-rows";
  list.setAttribute("role", "listbox");
  for (const [i, r] of search.rows.entries()) {
    const row = document.createElement("li");
    row.className = "search-row";
    row.id = `search-row-${String(i)}`;
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", String(search.cursor === i));
    row.dataset["dir"] = String(r.is_dir);
    row.addEventListener("click", () => {
      this.send({ action: "search_activate_row", row: i });
    });
    const name = document.createElement("span");
    name.className = "search-name";
    name.dataset["hostile"] = String(r.hostile);
    name.textContent = r.name;
    if (r.hostile) {
      name.append(badge(this.t("hostile-name")));
    }
    const parent = document.createElement("span");
    parent.className = "search-parent";
    parent.dataset["hostile"] = String(r.parent_hostile);
    parent.textContent = r.parent;
    row.append(name, parent);
    if (r.score !== null) {
      // The similarity, in its own cell: without it, a 0.91 and a 0.42 read
      // as equally good and the order looks arbitrary. Two decimals, which
      // is enough to tell them apart without faking precision.
      const score = document.createElement("span");
      score.className = "search-score";
      score.textContent = r.score.toFixed(2);
      row.append(score);
    }
    list.append(row);
  }
  if (search.cursor !== null) {
    list.setAttribute("aria-activedescendant", `search-row-${String(search.cursor)}`);
  }
  box.append(list);
  this.searchRoot.replaceChildren(box);
  if (search.cursor !== null) {
    revelar(list.querySelector(`#search-row-${String(search.cursor)}`) ?? undefined);
  }
}
