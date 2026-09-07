# 0097 — Parity between the terminal and the window is a test, not a convention

- Status: accepted
- Date: 2026-09-05
- Decision makers: Oscar González
- Related: ADR 0058 (layout of slots and roles), ADR 0066 (the SDK and the
  host boundary), ADR 0077 (a decision taken once, in `norte-frontend`),
  ADR 0096 (operated on vs pointed at), #252 (patch only what changed)

## Context

ADR 0077 already says the rule: a presentation decision taken twice diverges
silently. The rule is right and it is not enforced, so the divergences
accumulated anyway. The `..` row work of ADR 0096 walked into four of them by
accident — the details sheet written twice and already disagreeing about a
hostile attribute, the layout name resolved two ways, `[ui] parent_entry`
silently lost on session restore, and a panel with no way to refresh itself —
and that was one afternoon on one feature.

So we audited on purpose, across four axes: what can go stale in the window,
which decisions are written twice, which configuration keys each frontend
honours, and where the window's text escapes to the process locale. The
inventory is in
`docs/superpowers/plans/2026-09-05-paridad-tui-ventana.md`; it is long, and
the length is the point. What follows are the rules that would have caught
those items when they were written, rather than years later.

The single most useful thing the audit found is not a bug. It is that
**`norte-ui-host/tests/parity.rs` does not compare the two frontends.** It
compares the host against `norte-frontend` primitives through a harness whose
steps encode the *host's* answer — its `Entrar` step is
`selected().filter(|e| e.kind == Dir)`, which is what the window does and not
what the terminal does. `paridad.rs`, its neighbour, checks that catalogue
command *names* are classified. Between them they let every item in the
inventory through.

## Decision

1. **Parity is asserted between the two frontends, not between one frontend
   and the primitives.** A harness that asks the host what it does and calls
   the answer correct cannot fail. The comparison has to be: given the same
   listing, the same cursor and the same command, does `norte-tui` produce
   the same *semantic* answer as `norte-ui-host`? Never pixels — the target,
   the note key, the availability verdict, the dialog's item list.

   Where the answer is genuinely allowed to differ, the difference is named
   in a list with its reason, the way `paridad.rs::NO_APLICA` already names
   the commands a window will never have. Silence is indistinguishable from
   an oversight; that is the lesson `paridad.rs` learned for commands and
   nothing learned for behaviour.

2. **Every configuration key is classified on BOTH sides, at compile time.**
   The terminal already has this: `App::desde_config` destructures
   `CommonConfig` with no `..`, so a new key cannot be added without someone
   deciding what the terminal does with it. The window has no equivalent, and
   six of the audit's config findings are keys that would have had to be
   classified had it existed — including `openers.toml`, which is a whole
   documented feature the window ignores.

   The window gets the same guard. A key the window deliberately does not
   read is written down as such, next to why.

3. **In the window, state derived from the cursor or the listing needs a
   refresh path of its own.** The terminal recomputes its whole screen every
   frame and gets consistency for free. The window speaks in patches (#252),
   and a patch carries exactly the fields it names — a `rows` patch writes
   `generation`, `first_visible`, `rows` and `cursor`, and nothing else. Any
   other derived field is frozen until some *other* panel happens to force a
   full snapshot.

   That "happens to" is the bug generator: the details sheet worked only
   because the docked viewer dragged it along, and the shipped default layout
   (`orthodox`) places neither. So either the field travels in a patch, or
   its panel gets a probe beside `sondear_previews` / `sondear_hojas`, or it
   is written down as deliberately snapshot-only. Not by accident, and not by
   relying on a neighbour.

4. **Text for the window carries an explicit locale end to end, through
   shared helpers too.** `norte-ui-host` is already disciplined: all 131 of
   its i18n calls take `self.lang`. Every leak found was one level down, in a
   `norte-frontend` helper that translates internally with the process global
   — which is what `header_label` did until yesterday. A shared helper that
   returns user-visible text takes a `Lang`, or it has an `_in` sibling that
   does and the host calls that one.

5. **A configuration key is implemented, or removed — never merely
   advertised.** `[ui] font`, `mono_font`, `font_size` and `reduce_motion`
   have no reader anywhere in the workspace, and the window's own
   hot-reload exclusion list explains that they "travel in the startup
   catalogue and the stylesheet reads them once", which is not true: the
   stylesheet hard-codes the family and the size. `[profile.start]` is
   written by both frontends, parsed, validated, and read by nobody, while
   two files promise the reader that their next start comes from it. A key
   that is offered in the settings screen and does nothing is worse than a
   missing feature: it is a false statement about the program.

## Consequences

- The parity harness gets more expensive: it has to drive two frontends.
  That is the cost of the guarantee, and the audit is what it buys —
  seventeen already-diverged behaviours found by reading, none of which any
  test noticed.
- Some divergences are correct and will be written into the exception lists
  (a terminal has no window title; the window's F4 opening with the desktop
  handler is a decision ADR 0077 already records). Writing them down is the
  work, not an admission.
- The window's probe list grows one entry per follower panel. Cheap: a probe
  compares a value it already computes.
- Fixing the inventory is a multi-branch effort and is sequenced in the plan,
  not here. This ADR is about the guards; without them the same list grows
  back.

## Not decided here

Whether the window should hot-reload configuration at all. Today it does not:
`norte_config::watch` has exactly one production caller, in the terminal, so
every key the terminal re-applies live is startup-only in the window. That is
a design question about what a desktop window owes a config file, not a parity
defect to be fixed by symmetry, and it deserves its own ADR.
