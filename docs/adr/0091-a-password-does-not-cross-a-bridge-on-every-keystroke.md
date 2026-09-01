# 0091 — A password does not cross a bridge on every keystroke

- Status: accepted
- Date: 2026-09-01
- Issue: #327
- Bridge: 45

## Context

The window had to grow the dialog the TUI has had since #325: a connection with
`secret = "prompt"` whose three sources all came up empty suspends the
navigation and asks. Until this, `norte-gui` painted the text of
`err-secret-needed` — which names an environment variable — and that was the end
of the road. It is the parity hole ADR 0077 exists to close.

The obvious implementation was to reuse the machinery already there. Every other
dialog with a field works like this: the renderer owns the caret, so on each
keystroke it sends the **whole field** to the host, the host stores it and sends
back its masked projection for painting. That is correct for a filename — the
host needs the bytes, because it is going to create a file with them.

Applied to a password it produces two problems, and the second one is the
interesting one.

The shallow problem is painting: what the host sends back must never be the
text. That is easy — send one dot per character.

The deep problem is that **"the whole field on every keystroke" means every
prefix of the password crosses the IPC and stays.** Typing a twenty-character
password produces, in the window process, roughly sixty to eighty heap blocks
holding `h`, `hu`, `hun`, … — the WebKit IPC string, serde's parse buffer, the
action, the box — none of them overwritten, all of them freed for whatever
reuses that memory next. A `Zeroizing` buffer at the end of that chain wipes one
copy out of eighty. It is a guarantee that reads well and does almost nothing.

## Decision

**The host does not know what is being typed. The password crosses once, with
the answer.**

- While typing, the renderer sends **nothing**. It does not need to: the field
  is masked by the browser's own `input type="password"`, so there are no dots
  for the host to count and nothing for it to paint.
- On confirm — the button, or Enter inside the field — the renderer sends the
  value in `UiAction::Dialog::secret`, alongside the choice.
- The host wraps it in a `TypedSecret` on arrival, hands it to the task, and the
  task drops it when the core answers.

`Tecleado::Secreto` therefore carries **no data**. It exists as a type barrier,
not as storage: `texto()` returns nothing for it, so a text-shaped pendiente
that landed on this dialog by a wiring mistake cannot read a secret — there is
none to read.

## Consequences

**What the host does not have cannot leak from it.** Not through a `Debug`, not
through a snapshot, not through a log, not through the golden fixtures. That is
a stronger property than "it is stored in a type that redacts", and it costs
less code.

**A key cannot answer this dialog.** The keyboard path in the host resolves a
chord to a choice and calls `responder_dialogo` with no secret, which on a
password dialog is inert. The only door that hands anything over is the
renderer's, which has the value. This is a consequence, not a workaround — and
it is why Enter is intercepted *inside the field*, where the value is, rather
than left to travel as a chord.

**The empty field is judged on what just arrived**, not on stored state. That
turned out to matter: with a buffer in the host the two could disagree. A
dialog stacked on top discards the field's DOM node, and on returning the
renderer paints an empty field over a host buffer that was not empty — so
"confirming an empty field does nothing" stopped being true exactly where it had
been promised. With nothing stored, the field the reader sees is the field that
is evaluated.

**A password too long to fit is refused, not truncated.** Handing over the first
256 characters of a longer passphrase fails authentication with no indication of
why, and the reader cannot suspect it, because the field is masked.

**The shared type keeps its job for the copy that remains.** `TypedSecret` moved
from `norte-tui` to `norte-frontend` in this change: it is a security type — a
`Debug` that redacts, a wipe on drop, capacity reserved up front — and two
implementations are two places for one of the three to be forgotten. The reserve
is now in **bytes**, which is the bug the review found: it was counting
characters while the cap counted characters too, so 150 accented letters fit the
cap and not the reserve, and `String` reallocated, copied and freed the old
block **without wiping it** — half the password left on the heap, which is
precisely what the type promises cannot happen.

**And the window can finally edit a text field at all.** The document-level key
handler called `preventDefault()` on everything that was not a single printable
character, which cancelled the field's own Backspace and paste — a pre-existing
bug, barely noticeable on a `mkdir` prompt and disqualifying on a forty-character
access key with no visual feedback, where the only escape from a typo was
abandoning the navigation. The predicate now lives in `keys.ts`, exported, and
is tested; inside the handler there was no way to test it, and it was not
tested.

## Alternatives considered

**Send the length on each keystroke instead of the text.** Enough for the host
to paint dots, and it leaks nothing but a length. Rejected because the host does
not need to paint dots at all — the browser masks the field — so the action
would exist only to keep a mirror the renderer already is.

**Keep sending the text and zeroize the host's copy.** That is what the first
implementation did, and it is the version this ADR exists to reject: it wipes
one copy in eighty and reads like a guarantee.

**Encrypt the value over the bridge.** The bridge is an in-process IPC inside
one application; a key exchange between two halves of the same program buys
nothing an attacker with that memory could not already take.
