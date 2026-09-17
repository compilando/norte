# 0119 — The extension manager's Tab ring walks what the frame painted

- Status: accepted
- Date: 2026-09-17
- Decision makers: Oscar González
- Related: ADR 0102 (focus rings: two rings, and revealing rather than
  re-anchoring), ADR 0104 (the extension manager's card and its buttons, and
  the button that goes through the key's path), ADR 0077 (the same command
  means the same thing in both frontends), memory
  `gestor-tui-mudo-al-raton` (the manager was born mute to the mouse, and the
  fix was to derive what is clickable from what is painted),
  `menus-semanticos-y-shift-explicito`

## Context and problem statement

Oscar's report: *"quiero que mejores la navegación con Tabs por la página de
plugins, ahora no funciona muy bien y es necesario el ratón"*.

The diagnosis was not that `tab` was unbound. `tab` resolves to `dialog.pane`
in `orthodox`, `cua` and `vim`, the command is live in the catalogue, and its
rustdoc in the help overlay calls it "the only way INTO the body". The key
arrived at `on_extensions_key`, the resolver produced the command — and
`screens/extensions.rs` handled `dialog.up/down/cancel/approve/toggle-enabled/
remove/confirm` and nothing else. The command fell through the allowlist and
died in silence.

That silence had a shape. ADR 0104 gave the manager a card beside the list,
with a row of buttons — enable, approve, settings, uninstall, help — and made
each button fire *the same command as its key*, so the mouse could never grow
a second manager. What it did not give was a way to reach those buttons *as
buttons* from the keyboard. Every verb still had its own letter, so nothing
was unreachable; but a reader who walks a screen with `tab`, which is what
the row of buttons invites, pressed it and got nothing, and finished the job
with the mouse.

## Decision

**1. The ring's stops are the buttons the LAST FRAME painted, not the buttons
the card would have.** `mouse::painted_extension_buttons` reads
`app.mouse.extension_zones` — the very list a click resolves against — and the
ring walks it. Below `EXTENSIONS_WIDE_MIN` (64 usable cells) there is no card
at all, so there are no stops and `tab` does nothing; a button that did not
fit the card's width is not a stop either.

This is the rule that made the manager clickable in the first place (memory
`gestor-tui-mudo-al-raton`: what is pressable comes out of what is painted),
applied to the keyboard. The alternative — asking `extension_buttons(p)` what
buttons exist — computes the answer a second time, from the model instead of
the screen, and the two disagree exactly when the terminal is narrow. A focus
that lands on something nobody can see is the same class of bug as a click
that marks the neighbouring file.

**2. `Enter` on a focused button fires that button by SUBSTITUTING the
command before the allowlist, not by dispatching a second time.** One of the
buttons *is* `dialog.confirm` — settings — so re-entering the dispatcher would
loop. Substituted once, `dialog.confirm` means again what it means in the
list, which is exactly what that button does.

**3. A focus pointing past the painted buttons fires NOTHING.** The card can
shrink between the frame and the keystroke, and a plugin without a help page
has one button fewer. Firing "the fourth button" of a card that now has three
would run a verb the reader never read — and one of those verbs is uninstall,
which deletes files and revokes consent. `boton_enfocado` returns `None`
instead, and `Enter` falls back to its list meaning.

**4. Moving the list cursor returns the focus to the list.** The buttons
belong to the selected extension; one still focused while the cursor walks
away is a button for something the reader stopped looking at. Arrow keys and
a click on a row both reset it.

**5. The focused button is painted as a cursor (`Selection`), the others as
buttons (`Role::Button`), and the list's own cursor dims to
`SelectionUnfocused` while the focus is away.** That role exists for this
exact sentence, written in its own rustdoc: *"dos cursores igual de vivos no
dicen cuál recibe las teclas"*. Without it `tab` would move something
invisible, which is the state this ADR is undoing.

**6. `tab` is bound and dispatched, but NOT spelled out in the footer** — the
manager is the only one of these footers with an exclusion of its own, and
this is the part of the decision that contradicts a standing invariant, so it
is written down rather than left in a diff. `ALLOW_EXTENSIONS` is a single
source for dispatch AND for the generated hint, deliberately (#24: a rebind
can never desync the hint). At 80 columns that footer already held five verbs
in 67 of its 71 cells. A sixth did not widen the box — the box is clamped to
the frame — it cut the fifth mid-word: `[Tab] otr┘`. That is precisely the
MAJOR-1 that produced `without_navigation`, whose own rationale settles it:
*"the keys still work, they are just not spelled out in the footer"*.

`dialog.pane` does not join that SHARED exclusion list, because six other
allowlists bind it with the meaning "the other pane", where it fits and is
needed. The exclusion is one filter at the manager's build site, with the
reason beside it. What tells the reader instead: the `plugins` help topic, in
both locales, and the button itself, which lights up when the focus arrives.

## Consequences

- Four of the seven presets (`krusader`, `far`, `norton`, `total-commander`)
  do not bind `tab` to `dialog.pane`, and nothing here changes that: the
  binding is not new, and those four are transcriptions whose sources do not
  attest it. Their readers lose the accelerator, not the capability — every
  button keeps the key it always had, in all seven.
- The window needs no change and gets none. Its buttons are real
  `<button type="button">` elements inside a `role="group"`, so the browser's
  own focus ring already walks them. Parity here is a property of the
  platform, not of a shared decision — which is why the *behaviour* is
  documented in the shared help topic, where a future divergence would show.
- The button ORDER still differs between the two frontends (terminal:
  enable, approve, settings, uninstall, help; window: approve, enable, help,
  uninstall). That is a genuine ADR 0077 divergence, it predates this change,
  and this ADR does not fix it — it records it so the next person does not
  discover it the hard way.
- `ExtensionManager` gained a `foco` field, and `relistar_extensiones` carries
  it across a relist BY HAND, exactly as it already carries `cursor` and
  `config`. That relist is the one that runs right after a button is pressed;
  dropping the focus there would throw the keyboard back to the list at the
  precise moment the reader is using the card.
