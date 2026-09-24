// `Screen` painters for dialogs (wave W10): functions with `this: Screen`,
// hooked in as properties in `render.ts`. State stays in the class.

import type { Screen } from "../render";
import type { DialogLine, DialogView } from "../types";
import { badge } from "./dom";

/** A dialog's labeled field: the label out of band and the value with its
 *  mark if what is painted differs from what is there. */
export function campoDeDialogo(
  this: Screen,
  labelText: string,
  line: DialogLine,
): HTMLElement {
  const p = document.createElement("p");
  p.className = "dialog-field";
  const label = document.createElement("span");
  label.className = "dialog-field-label";
  label.textContent = labelText;
  const value = document.createElement("span");
  value.textContent = line.text;
  value.dataset["hostile"] = String(line.hostile);
  p.append(label, value);
  if (line.hostile) {
    value.classList.add("hostile");
    p.append(badge(this.t("hostile-name")));
  }
  return p;
}

export function paintDialogs(this: Screen, dialogs: DialogView[]): void {
  if (dialogs.length === 0) {
    this.dialogsRoot.replaceChildren();
    this.dialogoPintado = null;
    this.dialogoInput = null;
    this.dialogoCampos.clear();
    return;
  }
  const top = dialogs[dialogs.length - 1];
  if (top === undefined) {
    return;
  }
  // The field to return focus to once the new box is mounted.
  let refocus: HTMLInputElement | null = null;
  // And, in a FORM, which of its fields had it and where the caret was
  // (bridge 91): the box is rebuilt whole on every patch — and every
  // keystroke produces one — so without this, typing a letter drops the
  // focus.
  const active = document.activeElement;
  let focusedField: string | null = null;
  let caret = 0;
  if (active instanceof HTMLInputElement && active.dataset["campo"] !== undefined) {
    focusedField = active.dataset["campo"];
    caret = active.selectionStart ?? active.value.length;
  }
  const box = document.createElement("div");
  box.className = "dialog";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  const h = document.createElement("h2");
  h.id = `dialog-title-${String(top.id)}`;
  h.textContent = this.t(top.title_key);
  box.setAttribute("aria-labelledby", h.id);
  box.append(h);
  if (top.destination !== null) {
    // The destination, in its own element and with its translated label. NOT
    // as a body line with an arrow in front: a directory can be named
    // `docs → /home/DELETE`, that arrow is legitimate and is not masked, so
    // the line would read as two paths and whoever confirms would believe
    // they were sending their files to the second one.
    const dest = document.createElement("p");
    dest.className = "dialog-destination";
    const label = document.createElement("span");
    label.className = "dialog-destination-label";
    label.textContent = this.t("dialog-destination");
    const value = document.createElement("span");
    value.textContent = top.destination.text;
    value.dataset["hostile"] = String(top.destination.hostile);
    dest.append(label, value);
    if (top.destination.hostile) {
      value.classList.add("hostile");
      dest.append(badge(this.t("hostile-name")));
    }
    box.append(dest);
  }
  // What is asked for and who is asking, each labeled and OUTSIDE the path
  // list: among path lines, a file name saying the same thing would be
  // indistinguishable.
  if (top.subject !== null) {
    box.append(this.campoDeDialogo(this.t("dialog-subject"), top.subject));
  }
  if (top.asker !== null) {
    box.append(this.campoDeDialogo(this.t("dialog-asker"), top.asker));
  }
  if (top.body.length > 0) {
    // Numbered by POSITION, with an ordered list: the label is structural
    // and no file name can write it.
    const list = document.createElement("ol");
    list.className = "dialog-body";
    for (const line of top.body) {
      const li = document.createElement("li");
      li.textContent = line.text;
      li.dataset["hostile"] = String(line.hostile);
      if (line.hostile) {
        // This is the screen where deleting, copying or moving a name gets
        // approved. A name that paints different from what it is and does
        // not say so reads as trustworthy, and the approval is for something
        // else.
        li.classList.add("hostile");
        li.append(badge(this.t("hostile-name")));
      }
      list.append(li);
    }
    box.append(list);
  }
  if (top.deadline !== null) {
    // The deadline, in its own element: with `ttl_ms == 0` there is no
    // deadline line to paint, and then a file named "expires in 3600 s"
    // would be the only thing that looked like one.
    const deadline = document.createElement("p");
    deadline.className = "dialog-deadline";
    deadline.setAttribute("role", "status");
    deadline.textContent = top.deadline;
    // And if the host said WHEN it expires, it is counted for real (#279).
    // The host's sentence used to be computed on open and then frozen: a
    // modal that had been up for four minutes still said "expires in 300 s".
    //
    // The host keeps composing the text — only the number inside it gets
    // substituted here — because the sentence is its own and is translated:
    // the renderer does not know how to say "expires in" in this window's
    // language.
    const dueAt = top.deadline_at_ms;
    if (dueAt !== undefined && dueAt !== null) {
      const template = top.deadline;
      const paint = (): boolean => {
        const remaining = Math.max(0, Math.ceil((dueAt - Date.now()) / 1000));
        deadline.textContent = template.replace(/\d+/, String(remaining));
        return remaining > 0;
      };
      paint();
      const tick = window.setInterval(() => {
        // It stops on its own when it reaches zero: the host closes the
        // dialog when it expires, and counting into the negative on
        // something that is no longer there is noise.
        if (!paint() || !deadline.isConnected) {
          window.clearInterval(tick);
        }
      }, 1000);
    }
    box.append(deadline);
  }
  if (top.overflow_note !== "") {
    // The list is truncated, and saying so is the only thing that keeps
    // someone from confirming an operation on two hundred files thinking
    // there are sixteen.
    const note = document.createElement("p");
    note.className = "dialog-overflow";
    note.setAttribute("role", "alert");
    note.textContent = top.overflow_note;
    // And whether something NOT shown would paint altered. The badge cannot
    // talk about a specific path — it is not in front of us — but it can say
    // there is something like that out there, which is what decides whether
    // expanding before approving is worth it. The terminal said so and this
    // window did not.
    if (top.overflow_hostile === true) {
      note.append(" ", badge(this.t("hostile-name")));
    }
    box.append(note);
  }
  const check = top.dest_check ?? { state: "not_asked" };
  if (check.state === "checking") {
    // It is SAID that a question is in flight, and the spot stays reserved:
    // a notice that lands abruptly on top of the buttons moves them under
    // the pointer of someone who was already about to click. And above all,
    // while this reads "checking", the absence of line #164 cannot be read
    // as "this destination confines".
    const waiting = document.createElement("p");
    waiting.className = "dialog-checking";
    waiting.textContent = this.t("dialog-checking-destination");
    box.append(waiting);
  }
  if (check.state === "done") {
    for (const warning of check.warnings) {
      // About the DESTINATION: that it does not fit, that it cannot confine.
      // Already translated and with not a single string controlled by a
      // third party, so they go in their own block and not among the body's
      // lines — where a file name could impersonate them.
      const line = document.createElement("p");
      line.className = "dialog-warning";
      line.setAttribute("role", "alert");
      line.textContent = warning;
      box.append(line);
    }
  }
  if (top.input_hostile) {
    // It is the ONLY surface where a name gets approved: if what is painted
    // differs from what will be created, it is said here.
    const notice = document.createElement("p");
    notice.className = "hostile";
    notice.setAttribute("role", "alert");
    notice.textContent = this.t("hostile-name");
    box.append(notice);
  }
  if (top.input === null) {
    this.dialogoInput = null;
  } else {
    // The field is REUSED as long as it is the same dialog. It used to be
    // created anew on every repaint and left with no value set — so as not
    // to give back the host's projection, masked and bounded, which the next
    // event would have sent back as if it were what was typed — so the field
    // came out EMPTY. And since every keystroke triggers a patch, every
    // keystroke emptied it: what reached `fs.mkdir` was the last character.
    // Reusing the node also keeps the cursor and the selection.
    const previous = this.dialogoPintado === top.id ? this.dialogoInput : null;
    // Reusing the node is not enough: the dialog box is rebuilt on every
    // repaint and the field gets MOVED to the new one, and moving a node
    // takes it out of the document for an instant, which is what strips its
    // focus. Every keystroke triggers a patch, so every keystroke left the
    // field unfocused and the next one went to the host as a chord. Whether
    // it had focus is recorded and given back at the end, once the box is
    // mounted.
    const hadFocus = previous !== null && document.activeElement === previous;
    if (hadFocus) {
      refocus = previous;
    }
    let input = previous;
    if (input === null) {
      input = document.createElement("input");
      // #327: a password paints as a password. What arrives in `top.input`
      // is DOTS — the host never sends the text — so seeding the field with
      // that would write literal dots inside it: it is seeded empty, which
      // is what the dialog just opened with.
      input.type = top.input_secret ? "password" : "text";
      input.value = top.input_secret ? "" : top.input;
      if (top.input_secret) {
        // `new-password` and not `off`: Chromium and WebView2 deliberately
        // IGNORE `off` on a password field, and this is the value they do
        // respect. This is not saved anywhere, which is exactly what the
        // dialog's body promises.
        input.autocomplete = "new-password";
        input.setAttribute("autocorrect", "off");
        input.spellcheck = false;
      }
      const live = input;
      live.addEventListener("input", () => {
        // A PASSWORD is not sent while typing (#327): the host does not keep
        // what gets typed, the field is masked by the browser itself, and
        // over this path `h`, `hu`, `hun`… would cross — one prefix per
        // keystroke, each in a piece of heap nobody wipes. It crosses once,
        // on confirming.
        if (top.input_secret) {
          return;
        }
        this.send({ action: "dialog_input", id: top.id, text: live.value });
      });
      if (top.input_secret) {
        // Enter INSIDE the field confirms, and carries the value. Without
        // this, the key goes out to the host as a `dialog.confirm` chord —
        // which over a password dialog carries nothing and is therefore
        // inert — so the most natural way to answer would have done nothing.
        live.addEventListener("keydown", (e) => {
          if (e.key !== "Enter") {
            return;
          }
          e.preventDefault();
          e.stopPropagation();
          this.send({
            action: "dialog",
            id: top.id,
            choice: "confirm",
            secret: live.value,
          });
        });
      }
      queueMicrotask(() => {
        live.focus();
      });
    }
    input.setAttribute("aria-labelledby", h.id);
    this.dialogoInput = input;
    box.append(input);
  }
  // A form's FIELDS (bridge 91), in the order they arrive: the host orders
  // them, and reordering them here would tell a different story.
  const fields = top.fields ?? [];
  if (fields.length > 0) {
    // A different dialog starts from scratch; the SAME one reuses its nodes.
    if (this.dialogoPintado !== top.id) {
      this.dialogoCampos.clear();
    }
    const justBorn = this.dialogoCampos.size === 0;
    const box2 = document.createElement("div");
    box2.className = "dialog-fields";
    let first: HTMLInputElement | null = null;
    for (const f of fields) {
      const row = document.createElement("p");
      row.className = "dialog-field";
      const label = document.createElement("label");
      label.className = "dialog-field-label";
      label.textContent = this.t(f.label_key);
      label.htmlFor = `dialog-field-${f.id}`;
      row.append(label);
      const previous = this.dialogoCampos.get(f.id) ?? null;
      if (f.kind.kind === "text") {
        // **The node is REUSED and its value is NEVER re-seeded.** What the
        // host sends is its PROJECTION — masked and bounded — so re-seeding
        // it would make the next keystroke send it back as if it were what
        // was typed: an on-screen `U+FFFD` would end up being the pattern
        // being searched for. Same discipline as the single field above,
        // and the host adds the other belt by rejecting `U+FFFD`.
        let text = previous instanceof HTMLInputElement ? previous : null;
        if (text === null) {
          text = document.createElement("input");
          text.type = "text";
          text.id = `dialog-field-${f.id}`;
          text.value = f.value;
          text.dataset["campo"] = f.id;
          const live = text;
          live.addEventListener("input", () => {
            this.send({
              action: "dialog_field",
              id: top.id,
              field: f.id,
              value: { set: "text", text: live.value },
            });
          });
          this.dialogoCampos.set(f.id, live);
        }
        text.dataset["hostile"] = String(f.hostile);
        text.classList.toggle("hostile", f.hostile);
        first ??= text;
        row.append(text);
        if (f.hostile) {
          row.append(badge(this.t("hostile-name")));
        }
      } else if (f.kind.kind === "toggle") {
        // A toggle IS re-seeded: its state belongs to the HOST and there is
        // nothing typed to overwrite.
        let checkbox = previous instanceof HTMLInputElement ? previous : null;
        if (checkbox === null) {
          checkbox = document.createElement("input");
          checkbox.type = "checkbox";
          checkbox.id = `dialog-field-${f.id}`;
          checkbox.dataset["campo"] = f.id;
          checkbox.addEventListener("change", () => {
            // No value: it says it was TOUCHED, and which state it goes to
            // is decided by the host. Sending the destination would let two
            // quick clicks step on each other, the second one born from a
            // stale frame.
            this.send({
              action: "dialog_field",
              id: top.id,
              field: f.id,
              value: { set: "toggled" },
            });
          });
          this.dialogoCampos.set(f.id, checkbox);
        }
        checkbox.checked = f.kind.on;
        row.append(checkbox);
      } else {
        let button = previous instanceof HTMLButtonElement ? previous : null;
        if (button === null) {
          button = document.createElement("button");
          button.type = "button";
          button.id = `dialog-field-${f.id}`;
          button.dataset["campo"] = f.id;
          button.addEventListener("click", () => {
            this.send({
              action: "dialog_field",
              id: top.id,
              field: f.id,
              value: { set: "cycled" },
            });
          });
          this.dialogoCampos.set(f.id, button);
        }
        button.textContent = this.t(f.kind.value_key);
        row.append(button);
      }
      box2.append(row);
    }
    box.append(box2);
    // A freshly opened form takes focus to its first field, like the
    // single-field dialog: without this it opens and typing does nothing
    // until someone clicks inside.
    if (justBorn && focusedField === null && first !== null) {
      const target = first;
      queueMicrotask(() => {
        target.focus();
      });
    }
  }
  const choices = document.createElement("div");
  choices.className = "choices";
  for (const c of top.choices) {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = this.t(c.label_key);
    b.dataset["destructive"] = String(c.destructive);
    b.addEventListener("click", () => {
      // The password travels WITH the affirmative answer, and only with it
      // (#327): canceling delivers nothing. It is read from the live field
      // at this instant, which is what the reader is looking at — the host
      // keeps no copy it could disagree with.
      if (top.input_secret && c.id === "confirm") {
        this.send({
          action: "dialog",
          id: top.id,
          choice: c.id,
          secret: this.dialogoInput?.value ?? "",
        });
        return;
      }
      this.send({ action: "dialog", id: top.id, choice: c.id });
    });
    choices.append(b);
  }
  box.append(choices);
  this.dialogsRoot.replaceChildren(box);
  this.dialogoPintado = top.id;
  if (refocus !== null) {
    refocus.focus();
  }
  // And the form field that had it, with its caret where it was. Looked up
  // by `data-campo` and not with a composite selector: the host sets the id,
  // but composing a selector with someone else's text is a habit that
  // eventually gets used with text that is not.
  if (focusedField !== null) {
    const target = Array.from(box.querySelectorAll("input, button")).find(
      (n) => n instanceof HTMLElement && n.dataset["campo"] === focusedField,
    );
    if (target instanceof HTMLElement) {
      target.focus();
      if (target instanceof HTMLInputElement && target.type === "text") {
        const where = Math.min(caret, target.value.length);
        target.setSelectionRange(where, where);
      }
    }
  }
}
