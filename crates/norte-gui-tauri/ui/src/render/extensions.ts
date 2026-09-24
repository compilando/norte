// `Screen` painters for extensions (wave W10): functions with `this: Screen`,
// hooked in as properties in `render.ts`. State stays in the class.

import type { Screen } from "../render";
import type {
  ExtensionsView,
  ExtensionRowView,
  ExtensionErrorView,
  AgentsView,
  ExtensionCommandView,
  ExtensionOutputView,
  ProgramOutputView,
} from "../types";
import { revelar, badge } from "./dom";

/**
 * The extension manager (F12): the list on the left and, on the right, the
 * chosen one's card with its buttons (bridge 61).
 *
 * The capabilities go in the ROW and are not hidden behind a gesture: they
 * are the decision a human approves. The buttons do not make that decision
 * on their own: they send the SAME action as the key, and it is the host
 * that asks — granting enumerates the capabilities, uninstalling says what
 * is lost — before touching anything. Approve and uninstall are painted as
 * what they are, not as a notice's "OK".
 */
export function paintExtensions(this: Screen, ext: ExtensionsView | null): void {
  if (ext === null) {
    this.extensionsRoot.replaceChildren();
    this.extensionsRoot.dataset["open"] = "false";
    return;
  }
  this.extensionsRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "extensions";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", this.t("ext-title"));

  const header = document.createElement("header");
  header.className = "extensions-head";
  const title = document.createElement("h1");
  title.textContent = this.t("ext-title");
  header.append(title);
  const summary = document.createElement("span");
  summary.className = "extensions-summary";
  if (ext.loading) {
    // "Loading" and "none" are not the same, and an empty list without this
    // notice reads as the second one.
    summary.classList.add("extensions-note");
    summary.setAttribute("role", "status");
    summary.textContent = this.t("ext-loading");
  } else {
    // Two counts and not one: "7 installed" with "6 enabled" is the question
    // that brings someone to this screen.
    const on = ext.rows.filter((r) => r.approved && r.enabled).length;
    summary.textContent = `${String(ext.rows.length)} ${this.t("ext-installed")} · ${String(
      on,
    )} ${this.t("ext-enabled")}`;
  }
  header.append(summary);
  const close = document.createElement("button");
  close.type = "button";
  close.className = "extensions-close";
  close.setAttribute("aria-label", this.t("ext-close"));
  close.textContent = "×";
  close.addEventListener("click", () => {
    // The same key that closes: the host decides whether the first `esc`
    // closes a card or the manager, and a button deciding it separately
    // would diverge.
    this.send({
      action: "key",
      key: "Escape",
      ctrl: false,
      alt: false,
      shift: false,
      meta: false,
    });
  });
  header.append(close);
  box.append(header);

  const body = document.createElement("div");
  body.className = "extensions-body";

  const list = document.createElement("ul");
  list.className = "extensions-rows";
  list.setAttribute("role", "listbox");
  if (!ext.loading && ext.rows.length === 0) {
    const empty = document.createElement("p");
    empty.className = "extensions-note";
    empty.textContent = this.t("ext-empty");
    list.append(empty);
  }
  for (const [i, r] of ext.rows.entries()) {
    const row = document.createElement("li");
    row.className = "extensions-row";
    row.id = `extension-row-${String(i)}`;
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", String(ext.cursor === i));
    row.addEventListener("click", () => {
      this.send({ action: "extension_select_row", row: i });
    });

    const main = document.createElement("div");
    main.className = "extensions-row-main";
    const name = document.createElement("span");
    name.className = "extensions-name";
    name.textContent = r.name;
    const version = document.createElement("span");
    version.className = "extensions-version";
    version.textContent = r.version;
    main.append(name, version, estadoDe(r, this.t.bind(this)));
    row.append(main);

    const meta = document.createElement("span");
    meta.className = "extensions-meta";
    const parts = [r.category];
    if (r.publisher !== "") {
      parts.push(r.publisher);
    }
    meta.textContent = parts.join(" · ");
    row.append(meta);

    if (r.description !== "") {
      const desc = document.createElement("span");
      desc.className = "extensions-desc";
      desc.textContent = r.description;
      row.append(desc);
    }

    if (r.capabilities.length > 0) {
      row.append(capsDe(r.capabilities));
    }
    list.append(row);
  }
  if (ext.cursor < ext.rows.length) {
    list.setAttribute("aria-activedescendant", `extension-row-${String(ext.cursor)}`);
  }
  body.append(list);

  const panel = document.createElement("article");
  panel.className = "extensions-pane";
  const chosen = ext.rows[ext.cursor];
  // The ones that did not load go AFTER the loaded ones in the cursor's
  // count (bridge 79): row `rows.length + j` is `errors[j]`.
  const broken = ext.errors[ext.cursor - ext.rows.length];
  if (chosen !== undefined) {
    panel.append(this.extensionPaneHead(chosen, ext.cursor));
    if (ext.detail !== null && ext.detail.id === chosen.id) {
      panel.append(this.extensionDetail(ext.detail, chosen.name));
    } else {
      const hint = document.createElement("p");
      hint.className = "extensions-note extensions-detail-hint";
      hint.textContent = this.t("ext-detail-hint");
      panel.append(hint);
    }
  } else if (broken !== undefined) {
    panel.append(fichaDeRota(this, broken, ext.cursor));
  }
  body.append(panel);
  box.append(body);

  if (ext.errors.length > 0) {
    const title2 = document.createElement("h2");
    title2.className = "extensions-errors-title";
    title2.id = "extensions-errors-title";
    title2.textContent = this.t("ext-errors-title");
    box.append(title2);
    const errors = document.createElement("ul");
    errors.className = "extensions-errors";
    // It is a LIST of options, not a list of prose: its rows carry
    // `role="option"` and the cursor enters them. Without the `listbox` that
    // contains them, `option` is invalid ARIA and a screen reader announces
    // nothing on reaching a broken row — exactly for whoever has no mouse to
    // fall back on.
    errors.setAttribute("role", "listbox");
    errors.setAttribute("aria-labelledby", title2.id);
    if (ext.cursor >= ext.rows.length) {
      errors.setAttribute(
        "aria-activedescendant",
        `extension-row-${String(ext.cursor)}`,
      );
    }
    for (const [j, e] of ext.errors.entries()) {
      // One more row: pointed to with a click, and its card has the only
      // verb it has left. Without this, a broken extension could only be
      // removed by hand, by deleting its directory.
      const row = ext.rows.length + j;
      const li = document.createElement("li");
      li.className = "extensions-error";
      li.id = `extension-row-${String(row)}`;
      li.setAttribute("role", "option");
      li.setAttribute("aria-selected", String(ext.cursor === row));
      li.addEventListener("click", () => {
        this.send({ action: "extension_select_row", row });
      });
      const dir = document.createElement("span");
      dir.className = "extensions-error-dir";
      dir.dataset["hostile"] = String(e.hostile);
      dir.textContent = e.dir;
      if (e.hostile) {
        dir.append(badge(this.t("hostile-name")));
      }
      const reason = document.createElement("span");
      reason.className = "extensions-error-reason";
      reason.dataset["hostile"] = String(e.reason_hostile);
      reason.textContent = e.reason;
      if (e.reason_hostile) {
        // The reason is written by the core, but it QUOTES the plugin's
        // manifest and sometimes a `Path::display()`.
        reason.append(badge(this.t("hostile-name")));
      }
      li.append(dir, reason);
      errors.append(li);
    }
    box.append(errors);
  }
  this.extensionsRoot.replaceChildren(box);
  revelar(box.querySelector(`#extension-row-${String(ext.cursor)}`) ?? undefined);
}

/**
 * The card for an extension that did NOT load: where, why, and the only verb
 * it has left. With no id there is no button — the directory is not named
 * like an id and there is nothing to send for deletion — and it is said with
 * the host's sentence.
 */
function fichaDeRota(s: Screen, e: ExtensionErrorView, row: number): HTMLElement {
  const header = document.createElement("header");
  header.className = "extensions-pane-head";
  const name = document.createElement("h2");
  name.className = "extensions-pane-name";
  name.dataset["hostile"] = String(e.hostile);
  name.textContent = e.dir;
  if (e.hostile) {
    name.append(badge(s.t("hostile-name")));
  }
  const reason = document.createElement("p");
  reason.className = "extensions-error-reason";
  reason.dataset["hostile"] = String(e.reason_hostile);
  reason.textContent = e.reason;
  if (e.reason_hostile) {
    reason.append(badge(s.t("hostile-name")));
  }
  header.append(name, reason);

  const actions = document.createElement("div");
  actions.className = "extensions-actions";
  actions.setAttribute("role", "group");
  actions.setAttribute("aria-label", s.t("ext-actions"));
  const id = e.id;
  if (id !== null) {
    const uninstall = document.createElement("button");
    uninstall.type = "button";
    uninstall.className = "extensions-action extensions-action-uninstall";
    uninstall.textContent = s.t("ext-uninstall");
    uninstall.dataset["destructive"] = "true";
    uninstall.addEventListener("click", (ev) => {
      ev.stopPropagation();
      // The same action as the key: the host asks before deleting.
      s.send({ action: "extension_govern", row, id, change: "uninstall" });
    });
    actions.append(uninstall);
  } else {
    const note = document.createElement("p");
    note.className = "extensions-note";
    note.textContent = s.t("ext-broken-not-id");
    actions.append(note);
  }
  header.append(actions);
  return header;
}

/** The status pill: TWO independent facts, and both are stated. */
function estadoDe(r: ExtensionRowView, t: (key: string) => string): HTMLElement {
  const state = document.createElement("span");
  state.className = "extensions-state";
  state.dataset["approved"] = String(r.approved);
  state.dataset["enabled"] = String(r.enabled);
  state.textContent = r.approved
    ? t(r.enabled ? "ext-state-on" : "ext-state-off")
    : t("ext-unapproved");
  return state;
}

/** The capabilities as chips, one per node: a third party's text. */
function capsDe(capabilities: string[]): HTMLElement {
  const caps = document.createElement("ul");
  caps.className = "extensions-caps";
  for (const c of capabilities) {
    const cap = document.createElement("li");
    cap.className = "extensions-cap";
    cap.textContent = c;
    caps.append(cap);
  }
  return caps;
}

/**
 * The card's header (bridge 61): who it is, how it stands, and the buttons.
 *
 * Each button says what it is GOING to do, resolved from the state —
 * "Revoke" over an approved one, "Approve" over one that is not — and sends
 * the same action as the key; the host resolves the same way and asks
 * whatever needs asking. Enabling an unapproved one is not offered: the
 * button is disabled and its `title` says why, with the sentence the host
 * would answer.
 */
export function extensionPaneHead(
  this: Screen,
  r: ExtensionRowView,
  row: number,
): HTMLElement {
  const header = document.createElement("header");
  header.className = "extensions-pane-head";

  const name = document.createElement("h2");
  name.className = "extensions-pane-name";
  name.textContent = r.name;
  header.append(name);

  const meta = document.createElement("div");
  meta.className = "extensions-pane-meta";
  const version = document.createElement("span");
  version.className = "extensions-version";
  version.textContent = r.version;
  meta.append(version);
  if (r.publisher !== "") {
    const who = document.createElement("span");
    who.className = "extensions-publisher";
    who.textContent = r.publisher;
    meta.append(who);
  }
  const category = document.createElement("span");
  category.className = "extensions-category";
  category.textContent = r.category;
  meta.append(category, estadoDe(r, this.t.bind(this)));
  header.append(meta);

  if (r.description !== "") {
    const desc = document.createElement("p");
    desc.className = "extensions-pane-desc";
    desc.textContent = r.description;
    header.append(desc);
  }

  const actions = document.createElement("div");
  actions.className = "extensions-actions";
  actions.setAttribute("role", "group");
  actions.setAttribute("aria-label", this.t("ext-actions"));
  const button = (cls: string, text: string, send: () => void): HTMLButtonElement => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = `extensions-action ${cls}`;
    b.textContent = text;
    b.addEventListener("click", (ev) => {
      // The click does not bubble to the row: the action already points to
      // it, and a second selection order would step on the card the first
      // one asks for.
      ev.stopPropagation();
      send();
    });
    return b;
  };
  const approve = button(
    "extensions-action-approval",
    this.t(r.approved ? "ext-revoke" : "ext-approve"),
    () => {
      this.send({ action: "extension_govern", row, id: r.id, change: "approval" });
    },
  );
  // Granting deletes nothing, but it IS the security decision: it is marked
  // so it does not paint as a notice's "OK".
  approve.dataset["primary"] = String(!r.approved);
  actions.append(approve);

  const enable = button(
    "extensions-action-enabled",
    this.t(r.enabled ? "ext-disable" : "ext-enable"),
    () => {
      this.send({ action: "extension_govern", row, id: r.id, change: "enabled" });
    },
  );
  if (!r.approved && !r.enabled) {
    enable.disabled = true;
    enable.title = this.t("host-extension-not-approved");
  }
  actions.append(enable);

  if (r.has_help) {
    actions.append(
      button("extensions-action-help", this.t("ext-help"), () => {
        this.send({ action: "extension_help", row, id: r.id });
      }),
    );
  }

  const uninstall = button(
    "extensions-action-uninstall",
    this.t("ext-uninstall"),
    () => {
      this.send({ action: "extension_govern", row, id: r.id, change: "uninstall" });
    },
  );
  uninstall.dataset["destructive"] = "true";
  actions.append(uninstall);
  header.append(actions);

  if (r.capabilities.length > 0) {
    header.append(capsDe(r.capabilities));
  }

  // How much it contributes: the numbers the row has no room to state.
  const counts = document.createElement("p");
  counts.className = "extensions-counts";
  const parts: string[] = [];
  if (r.commands > 0) {
    parts.push(`${String(r.commands)} ${this.t("ext-counts-commands")}`);
  }
  if (r.columns > 0) {
    parts.push(`${String(r.columns)} ${this.t("ext-counts-columns")}`);
  }
  if (parts.length > 0) {
    counts.textContent = parts.join(" · ");
    header.append(counts);
  }
  return header;
}

/** An extension's card: its `[config]` keys with their values. */
export function extensionDetail(
  this: Screen,
  d: ExtensionsView["detail"],
  name: string,
): HTMLElement {
  const card = document.createElement("article");
  card.className = "extensions-detail";
  if (d === null) {
    return card;
  }
  const title = document.createElement("h2");
  title.textContent = this.t("ext-config-title");
  if (name !== "") {
    const owner = document.createElement("span");
    owner.className = "extensions-detail-of";
    owner.textContent = name;
    title.append(" · ", owner);
  }
  card.append(title);
  if (d.config.length === 0) {
    const none = document.createElement("p");
    none.className = "extensions-note";
    none.textContent = this.t("ext-config-none");
    card.append(none);
    return card;
  }
  const table = document.createElement("table");
  table.className = "extensions-config";
  const tbody = document.createElement("tbody");
  for (const [i, k] of d.config.entries()) {
    const tr = document.createElement("tr");
    tr.id = `extension-key-${String(i)}`;
    // Which one is chosen and which one can be edited: without the second,
    // the screen offers `Enter` on a key of a kind this build does not know,
    // and the reader concludes the write failed.
    tr.dataset["current"] = String(i === d.cursor);
    tr.dataset["editable"] = String(k.editable);
    // A value that is NOT the schema's default is marked: it is the only
    // thing that tells apart "it comes this way" from "you left it this
    // way".
    tr.dataset["changed"] = String(k.value !== k.default);
    const key = document.createElement("th");
    key.setAttribute("scope", "row");
    key.className = "extensions-key";
    key.textContent = k.key;
    const value = document.createElement("td");
    value.className = "extensions-key-value";
    value.dataset["hostile"] = String(k.hostile);
    if (i === d.cursor && d.editing !== null) {
      // What is being TYPED, in its own node and marked: it replaces the
      // value because it is what is about to be written, not what is there.
      const buf = document.createElement("span");
      buf.className = "extensions-key-editing";
      buf.dataset["hostile"] = String(d.editing_hostile);
      buf.textContent = d.editing;
      value.append(buf);
      if (d.editing_hostile) {
        value.append(badge(this.t("hostile-name")));
      }
    } else {
      value.textContent = k.value;
    }
    if (k.hostile && d.editing === null) {
      // What is painted differs from what it is, and it is written by the
      // plugin: it is said, same as with a file name.
      value.append(badge(this.t("hostile-name")));
    }
    const kind = document.createElement("td");
    kind.className = "extensions-key-kind";
    // The kind and the domain, each in its own node: joining them into one
    // lets an `enum` value with RTL letters reorder the whole pair, and the
    // container's `unicode-bidi: isolate` only separates SIBLINGS.
    const kindSpan = document.createElement("span");
    kindSpan.className = "extensions-key-kind-name";
    kindSpan.textContent = k.kind;
    kind.append(kindSpan);
    if (k.domain !== "") {
      const sep = document.createElement("span");
      sep.className = "sep";
      sep.textContent = " · ";
      const dom = document.createElement("span");
      dom.className = "extensions-key-domain";
      dom.textContent = k.domain;
      kind.append(sep, dom);
    }
    const desc = document.createElement("td");
    desc.className = "extensions-key-desc";
    desc.textContent = k.description;
    tr.append(key, value, kind, desc);
    tbody.append(tr);
  }
  table.append(tbody);
  card.append(table);
  card.append(this.extensionCommands(d.commands));
  return card;
}

/**
 * The commands an extension contributes.
 *
 * They are LISTED and not launched from here: the palette is the door — the
 * same one as in the TUI — and having two leaves two answers to what it
 * means for one to fail. The `id` is not painted: the manifest does not
 * validate its charset.
 */
export function extensionCommands(
  this: Screen,
  cmds: ExtensionCommandView[],
): HTMLElement {
  const box = document.createElement("div");
  box.className = "extensions-commands";
  if (cmds.length === 0) {
    return box;
  }
  const title = document.createElement("h3");
  title.textContent = this.t("ext-commands-title");
  const list = document.createElement("ul");
  for (const c of cmds) {
    const li = document.createElement("li");
    li.className = "extensions-command";
    li.dataset["hostile"] = String(c.hostile);
    li.textContent = c.title;
    if (c.hostile) {
      li.append(badge(this.t("hostile-name")));
    }
    list.append(li);
  }
  box.append(title, list);
  return box;
}

/**
 * The agent sessions this window has seen ask for permission.
 *
 * The NOTE goes inside the panel and not in the documentation: this list is
 * not the system's census of agents — there is no method that gives that —
 * and an empty list without that sentence reads as "no agent has touched
 * anything".
 */
export function paintAgents(this: Screen, agents: AgentsView | null): void {
  if (agents === null) {
    if (this.agentsRoot.dataset["open"] === "true") {
      this.agentsRoot.replaceChildren();
      this.agentsRoot.dataset["open"] = "false";
    }
    return;
  }
  this.agentsRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "agents";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", this.t("agents-title"));
  const title = document.createElement("h2");
  title.textContent = this.t("agents-title");
  const note = document.createElement("p");
  note.className = "agents-note";
  note.textContent = agents.note;
  box.append(title, note);
  if (agents.forgotten > 0) {
    // What was FORGOTTEN is stated: the session id is chosen by the agent,
    // so flooding the list to push a specific one out is within its reach,
    // and a trimmed list presented as complete is what turns that into "that
    // session does not exist".
    const trimmed = document.createElement("p");
    trimmed.className = "agents-forgotten";
    trimmed.setAttribute("role", "status");
    trimmed.textContent = String(agents.forgotten);
    trimmed.dataset["forgotten"] = String(agents.forgotten);
    box.append(trimmed);
  }
  if (agents.rows.length === 0) {
    // The sentence is composed by the HOST: an empty list means different
    // things depending on whether this window is listening to requests.
    const empty = document.createElement("p");
    empty.className = "agents-empty";
    empty.textContent = agents.empty;
    box.append(empty);
    this.agentsRoot.replaceChildren(box);
    return;
  }
  const list = document.createElement("ul");
  list.className = "agents-rows";
  list.setAttribute("role", "listbox");
  for (const [i, r] of agents.rows.entries()) {
    const li = document.createElement("li");
    li.className = "agents-row";
    li.id = `agent-row-${String(i)}`;
    li.setAttribute("role", "option");
    li.setAttribute("aria-selected", String(agents.cursor === i));
    li.addEventListener("click", () => {
      // The generation travels with the click: the list reorders on its
      // own, and a click against the old one chooses a different row —
      // here "this row" is whose work gets undone.
      this.send({
        action: "agent_select_row",
        row: i,
        generation: agents.generation,
      });
    });
    // The id and the last op, each isolated and with its own flag: the id is
    // an opaque daemon key and can carry RTL letters that would reorder the
    // whole row.
    const id = document.createElement("span");
    id.className = "agents-session";
    id.dataset["hostile"] = String(r.session_hostile);
    id.textContent = r.session;
    li.append(id);
    if (r.session_hostile) {
      li.append(badge(this.t("hostile-name")));
    }
    const op = document.createElement("span");
    op.className = "agents-op";
    op.dataset["hostile"] = String(r.last_op_hostile);
    op.textContent = r.last_op;
    li.append(op);
    if (r.last_op_hostile) {
      li.append(badge(this.t("hostile-name")));
    }
    // Asked for N and was granted M: they are not the same when another
    // window answered, when it was denied, or when it expired.
    const counts = document.createElement("span");
    counts.className = "agents-counts";
    counts.textContent = r.counts;
    li.append(counts);
    li.dataset["undoing"] = String(r.undoing);
    list.append(li);
  }
  list.setAttribute("aria-activedescendant", `agent-row-${String(agents.cursor)}`);
  box.append(list);
  this.agentsRoot.replaceChildren(box);
  revelar(list.querySelector(`#agent-row-${String(agents.cursor)}`) ?? undefined);
}

/**
 * What an extension command printed.
 *
 * Everything here is written by a third party, and all three things are
 * stated: who printed it, which command, and whether the output was cut off
 * — which the reader cannot deduce, because the text arrives already short.
 */
export function paintPluginOutput(
  this: Screen,
  output: ExtensionOutputView | null,
): void {
  if (output === null) {
    if (this.pluginOutputRoot.dataset["open"] === "true") {
      this.pluginOutputRoot.replaceChildren();
      this.pluginOutputRoot.dataset["open"] = "false";
    }
    return;
  }
  this.pluginOutputRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "plugin-output";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", this.t("plugin-output-title"));
  const title = document.createElement("h2");
  title.textContent = this.t("plugin-output-title");
  const who = document.createElement("p");
  who.className = "plugin-output-who";
  // Who and what, each in its own node and with ITS OWN flag: joining them
  // into one sentence lets a third party's title with RTL letters reorder
  // the whole pair, and a single flag for both ends up describing the wrong
  // one.
  const plugin = document.createElement("span");
  plugin.className = "plugin-output-plugin";
  plugin.dataset["hostile"] = String(output.plugin.hostile);
  plugin.textContent = output.plugin.text;
  who.append(plugin);
  if (output.plugin.hostile) {
    who.append(badge(this.t("hostile-name")));
  }
  // The reverse-DNS id, which the core DOES validate: two extensions can be
  // named the same and the name is written by the manifest.
  const ident = document.createElement("span");
  ident.className = "plugin-output-id";
  ident.textContent = output.plugin_id;
  who.append(ident);
  if (output.command.text !== "") {
    const cmd = document.createElement("span");
    cmd.className = "plugin-output-command";
    cmd.dataset["hostile"] = String(output.command.hostile);
    cmd.textContent = output.command.text;
    who.append(cmd);
    if (output.command.hostile) {
      who.append(badge(this.t("hostile-name")));
    }
  }
  const body = document.createElement("pre");
  body.className = "plugin-output-text";
  body.dataset["hostile"] = String(output.text_hostile);
  // Empty is STATED: a blank panel reads as if it never got to run.
  body.textContent =
    output.lines.length === 0 ? this.t("plugin-output-empty") : output.lines.join("\n");
  box.append(title, who, body);
  if (output.text_hostile) {
    box.append(badge(this.t("hostile-name")));
  }
  if (output.truncated) {
    const cut = document.createElement("p");
    cut.className = "plugin-output-truncated";
    cut.setAttribute("role", "status");
    cut.textContent = this.t("plugin-output-truncated");
    box.append(cut);
  }
  this.pluginOutputRoot.replaceChildren(box);
}

/**
 * The output of a program the host ran and waited on (#312): the two-file
 * comparator. The same box as an extension's output — it is the same kind
 * of text, from another program — with the command that ran and, if it did
 * not start, saying so.
 */
export function paintProgramOutput(this: Screen, output: ProgramOutputView | null): void {
  if (output === null) {
    if (this.programOutputRoot.dataset["open"] === "true") {
      this.programOutputRoot.replaceChildren();
      this.programOutputRoot.dataset["open"] = "false";
    }
    return;
  }
  this.programOutputRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "plugin-output program-output";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", this.t(output.title_key));
  const title = document.createElement("h2");
  title.textContent = this.t(output.title_key);
  const who = document.createElement("p");
  who.className = "plugin-output-who";
  const cmd = document.createElement("span");
  cmd.className = "plugin-output-command program-output-command";
  cmd.dataset["hostile"] = String(output.command.hostile);
  cmd.textContent = output.command.text;
  who.append(cmd);
  if (output.command.hostile) {
    who.append(badge(this.t("hostile-name")));
  }
  box.append(title, who);
  if (output.failed) {
    const no = document.createElement("p");
    no.className = "program-output-failed";
    no.setAttribute("role", "alert");
    no.textContent = this.t("program-output-failed");
    box.append(no);
  }
  const body = document.createElement("pre");
  body.className = "plugin-output-text";
  body.dataset["hostile"] = String(output.text_hostile);
  body.textContent =
    output.lines.length === 0 ? this.t("plugin-output-empty") : output.lines.join("\n");
  box.append(body);
  if (output.text_hostile) {
    box.append(badge(this.t("hostile-name")));
  }
  if (output.truncated) {
    const cut = document.createElement("p");
    cut.className = "plugin-output-truncated";
    cut.setAttribute("role", "status");
    cut.textContent = this.t("plugin-output-truncated");
    box.append(cut);
  }
  this.programOutputRoot.replaceChildren(box);
}
