# Keymaps

norte ships seven keymap presets. Each one is a TOML file in
`crates/norte-frontend/presets/keymap/`, embedded in the binary. The default
is `orthodox`.

## Choose a preset

Set `preset` in the `[keymap]` section of your user `norte.toml`:

```toml
[keymap]
preset = "krusader"
```

A project's `./.norte.toml` cannot choose the preset. The presets disagree
about what a key deletes, so a foreign repository must not pick it for you
(#260). System, user and profile files can.

## The presets

| preset | imitates | notable choices |
| --- | --- | --- |
| `orthodox` | Midnight Commander; the default | F5 copy, F7 mkdir, F8 delete, Shift+F8 permanent delete, `q` quits |
| `vim` | vim | numeric prefixes (`5j`); no pairs like `d d`; `?` opens help |
| `cua` | desktop conventions | arrows and Ctrl+Q; Ctrl+C stays quit, not copy (copy is F5) |
| `krusader` | Krusader | transcribed from KDE's key list; Delete deletes, Ctrl+Delete deletes permanently; typing a letter jumps to a name; F9 is the terminal |
| `far` | Far Manager | transcribed from Far's help sources; Shift+F8 and Shift+Delete delete, Alt+Delete deletes permanently; no bare letter is bound |
| `norton` | Norton Commander | the F-key row and the uncontroversial core only; no first-hand source survives, so it is unverified against a running copy |
| `total-commander` | Total Commander 11.58 | transcribed from its `KEYBOARD.TXT`; F8 delete, Shift+F8 permanent delete; no bare letter is bound |

The header of each preset file lists what it leaves unbound and why. That is
the authoritative record of where a preset diverges from its original.

## Customize

A user layer adds to a preset with `prepend_keymap` and `append_keymap`; it
cannot replace the preset's `keymap`. See `docs/schema/keymap.schema.json`
for the format. The in-app help (F1) and the command palette (Ctrl+P) show
the key each command has in your effective keymap.
