# 0099 — The window reloads what a profile switch reloads, and says the rest

- Status: accepted
- Date: 2026-09-07
- Decision makers: Oscar González
- Related: ADR 0020 (theme resolution), ADR 0066 (SDK and host boundary),
  ADR 0077 (a decision taken once, in `norte-frontend`), ADR 0079 (a profile
  declares, it does not execute), ADR 0097 (parity is a test), ADR 0098
  (`[profile.start]`)

## Context

The terminal watches its configuration layers (`norte_config::watch`, with a
polling fallback it announces) and applies a change without restarting. The
window does not watch anything: every key is read once at startup.

The parity audit of 2026-09-05 listed this as the one class-A item it would not
close, on the grounds that it is not a parity defect but a design question:
what does a desktop window owe a file that changed under it? Every other item
in that plan has since landed, so this is the last open question in the
frontend, and leaving it open is now the thing producing the divergence rather
than protecting against it.

Three facts make the question different in the window than in the terminal.

**There is a third party.** Colours and appearance do not live in the host's
view; they cross to the webview in `HostCatalog`, and the renderer plugs them
in as CSS variables. Reloading them means rebuilding that package and telling
the renderer to re-fetch it — a path that already exists, because it is how the
theme changes today.

**Some of it is measured, not just painted.** `[ui] font_size` moves the cell
grid: the window is laid out in cells, so the renderer has to re-measure the
font before the host's next placement means anything.

**And one key genuinely cannot come back.** `norte_i18n::force` runs once per
process, so `[ui] lang` cannot be re-applied in either frontend. The terminal
already says so, by name, on every profile switch.

What is *not* new is the machinery. A profile switch already reloads the
configuration and re-applies theme, keymap, columns, favourites and layout
(`Estado::aplicar_perfil`), and already announces what it could not apply
(`fuera_de_alcance_en_caliente`). A configuration reload is that same operation
with the active profile unchanged.

## Decision

1. **The window reloads on a file change, through the door a profile switch
   already uses.** Not a second apply path: the same one, with the profile name
   left alone. Two paths that both mean "the configuration changed" is exactly
   the shape ADR 0077 exists to stop, and this one would diverge on the
   interesting half — which keys are honoured.

2. **The watcher is the shared one** (`norte_config::watch`), including its
   polling fallback and the announcement the terminal already makes when it
   falls back. It runs in the shell (`norte-gui-tauri`), not in the host: the
   host is toolkit-independent and takes no filesystem watches (ADR 0066), and
   a reload arrives at it as one more message on its mailbox, like everything
   else.

3. **What cannot be applied is said, by name, every time** — the rule ADR 0079
   D8 already sets for a profile switch. The exclusion list stays a per-frontend
   function, because it is a statement about *this process*: `[ui] lang` is out
   in both, and a terminal additionally applies no fonts at all.

4. **The catalogue is rebuilt when what it carries changes**, and only then.
   The renderer is told to re-fetch through the event that already exists for
   the theme. It re-measures the cell as part of applying appearance, so a
   font change reaches the grid rather than only the glyphs.

5. **A write from the settings screen is not special.** It lands in a file, the
   watcher sees it, and the reload happens like any other. Suppressing our own
   writes would mean the screen and the file could disagree, which is worse
   than applying the same value twice.

6. **The session and the layout are not configuration.** A reload never moves a
   slot's directory and never reseeds `[profile.start]`: that key is where a
   slot opens the first time (ADR 0098), and re-applying it whenever the file
   is touched would drag the reader back to the start every time they edit it.

## Consequences

- The window stops being the surface where "I changed my config and nothing
  happened" is the expected answer.
- The four appearance keys become live, which removes the caveat this session
  had to write into `fuera_de_alcance_en_caliente` — they applied at startup and
  not on a profile switch, because only the theme rebuilt the catalogue.
- One more watcher per window process. On a filesystem without inotify it
  degrades to polling and says so, which is the terminal's behaviour and its
  reasoning, not a new one.
- The reload is a mailbox message, so it serialises with everything else the
  host does. A configuration that changes mid-operation cannot interleave.

## What this does not decide

Whether the window reloads `openers.toml` and `keymap.toml` from disk on the
same trigger. They are separate files with their own layer rules, the terminal
reloads them, and the honest answer is that they follow the same door — but
they are the part with the most surface (a broken keymap must not leave the
window without keys, which is why the profile path keeps the previous one), so
they are worth landing after the scalars and with their own tests.

## Alternatives considered

- **Keep it startup-only and say so in the settings screen.** Cheapest, and it
  was the status quo. Rejected because a desktop window is open for days: the
  answer "restart it" is affordable in a terminal you reopen constantly and is
  not here, and the settings screen would have to grow a per-frontend caveat on
  most of its entries.
- **Reload only the theme.** That is what happens today by accident, and it is
  the worst of both: one key behaves and the rest do not, with nothing saying
  which is which.
- **A "reload configuration" command instead of a watcher.** Rejected for
  parity: the terminal does not require one, so the same edit would need a
  different gesture on each surface — and a reader who does not know the
  command reads the window as broken.
