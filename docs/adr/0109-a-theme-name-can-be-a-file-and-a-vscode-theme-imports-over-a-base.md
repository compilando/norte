# 0109 — A theme name can be a file you own, and a VSCode theme imports over a base

- Status: accepted
- Date: 2026-09-14
- Decision makers: Oscar González
- Amends: ADR 0020 (shared semantic themes)
- Related: ADR 0077 (TUI/window parity), ADR 0108 (chrome roles and the
  VSCode presets), spec `docs/superpowers/specs/2026-09-11-vscode-theme-design.md`
  (F5)

## Context and problem statement

ADR 0108 shipped `vscode-dark` and `vscode-light` and the roles a VSCode-like
window needs. It left F5 of the spec: letting someone use a theme that is not
bundled — in practice, one of the thousands of VSCode themes they already
like.

Two things stood in the way.

**A theme of your own was reachable only by path.** `[ui] theme` resolved a
bundled preset name or a filesystem path. A custom theme worked, but the
picker, the first-run wizard and the settings screen listed presets only, so
it could be set by typing a path into `norte.toml` and chosen nowhere. "Which
themes exist" was built six times, once per surface, from `preset_names()`.

**A VSCode theme JSON is not a norte theme, and not a complete palette
either.** Its `colors` block names editor surfaces, uses `#rrggbbaa` where
norte has no alpha, is JSON-with-comments, may `include` another file, and —
the finding from transcribing the presets — leaves list selection, hover and
the scrollbar to the editor's built-in colour registry. A literal conversion
produces a theme with twenty colours and the rest monochrome, which reads as a
broken importer.

## Decision 1 — a third resolver branch, in a fixed order

`[ui] theme` resolves, in this order: **a bundled preset**, then
**`<config>/themes/<name>.toml`** when the value is a plain name, then **the
value as a path**.

### Options

**(a) User directory first.** Lets a file override a preset — a "patched
nord". But a stale `themes/nord.toml` then silently changes what `nord` means
for everyone who reads a bug report, a screenshot or the docs.

**(b) Presets first (chosen).** A preset name means one thing everywhere. A
patched preset is still possible under another name.

**(c) A prefix (`user:mine`).** Unambiguous, but a new syntax to learn for
the common case, and every existing path-valued setting would still need the
third branch.

### Decision

(b). A "plain name" is letters, digits, `-`, `_`, `.`, not starting with a
dot, at most 64 bytes — anything else is a path, as before. The user themes
are read and parsed with the configuration (where the disk may already be
touched, rule 2) and travel in `FrontendConfig`, so the pickers list and
preview them without reading a file inside a keystroke. A file that does not
parse, or whose name is a preset, is left out of the list: the first would
break the picker, the second is unreachable by the order above.

One function, `norte_frontend::theme::theme_names`, replaces the six lists in
both frontends (ADR 0077).

## Decision 2 — `norte theme import`, pure parser, I/O in the binary

A CLI command converts a VSCode theme into a file in the directory of
Decision 1. The parser and the projection live in `norte_theme::vscode` and
do no I/O; the binary walks the `include` chain (relative to each file,
depth-capped at eight, cycle-checked on canonical paths, each file capped at
4 MiB and required to be a regular file) and writes the result.

### Options

**(a) Import in the frontends, at theme-selection time.** Accept `.json` in
`[ui] theme` directly. No extra step, but every theme load then runs a
converter with a chain walk, in two frontends, and the user never gets a file
they can read and adjust.

**(b) A one-shot command that writes an ordinary theme (chosen).** The result
is a normal TOML theme with a provenance header: inspectable, editable,
diffable, and loaded by the code that already loads themes.

### Decision

(b). Four choices inside it are load-bearing:

1. **The theme is painted over `vscode-dark` or `vscode-light`**, picked by
   its `"type"` (absent = dark, as in VSCode). What the theme does not
   define comes from the preset, which carries the registry defaults (ADR
   0108 Decision 5). So does everything VSCode has no id for:
   `[files.*]`, `mark`, `hostile-badge`.
2. **Alpha is composited, not dropped**, over the theme's `editor.background`
   (itself composited over the base's). Dropping it turns One Dark Pro's
   `#4e566660` slider into an opaque bright bar; compositing is what the
   presets' transcription already did by hand. `widget.shadow` is not
   imported, for the reason the presets omit it.
3. **One mapping table, ordered, where the later id wins.** That is how a
   fallback is written: `editor.foreground` before `foreground`, because One
   Dark Pro — among the most installed themes — defines only the former.
4. **A colour that does not parse is skipped and named**, not an error:
   VSCode ignores it too, and a marketplace theme with one odd value should
   still import. Only text that is not JSONC at all is an error.

JSONC (comments, trailing commas) is stripped by a thirty-line state machine
rather than a dependency (rule 8). A name that is a bundled preset is refused
— the file would never be read — and an existing theme is not replaced
without `--force`. `--use` writes `[ui] theme` through `persist_set`, which
keeps comments.

## Consequences

### Positive

- Any VSCode colour theme becomes a norte theme in one command, and comes out
  complete rather than half-painted.
- A theme of your own is a first-class choice in every picker, in both
  frontends, from one list.
- The imported file is plain TOML: nothing new to load, and the user can
  adjust what the mapping got wrong.

### Negative

- The mapping is ours and approximate. VSCode paints a list on the sidebar
  and norte paints it on a pane; a theme whose sidebar and editor
  backgrounds differ sharply will look different, and compositing everything
  over the editor background is exact only for colours seen there.
- An imported theme is a snapshot: updating the VSCode extension does not
  update it. Re-import with `--force`.
- `Theme::to_toml` is a hand-written serializer (inline tables, fixed order),
  held honest by a round-trip test over every preset.
- The chain walk and the atomic write live in the CLI, because it is the
  only surface that imports. The day a picker offers "import theme…", they
  move to `norte-frontend::theme` beside `load_user_themes` rather than being
  copied (ADR 0077).
- Everything from the theme file that reaches a terminal — the ids of
  skipped colours, the paths of the chain, the source name in the header — is
  written by whoever published the theme, and is masked with
  `mask_terminal_hazards` before it is printed. A UTF-16 file is decoded only
  when it carries a byte-order mark.
- The resolver now reads a second directory. A plain name that is neither a
  preset nor a user theme falls through to the path branch, so a relative
  path spelled like a name (`theme = "mine.toml"`) is looked up under
  `themes/` first.
