---
description: Add a named command end to end — catalogue with its effect, both dispatchers, seven presets, i18n, help and goldens
argument-hint: <command name and what it does, for example "pane.touch — set the mtime of the marked files to now">
---
Add the command: $ARGUMENTS

A command is one name (ADR 0006) that every surface shares. Adding it means
touching each of these, in this order. Skipping one leaves a key that does
nothing and does not say why, which has shipped three times already.

1. **Does the core already do it?** If it needs a new RPC, run `/proto-change`
   first and put the behaviour in `norte-core`. A mutation needs a journal
   entry and an undo path, or an explicit `Irreversible` with a reason. A
   frontend never does business logic (hard rule 7).
2. **Catalogue row** — `crates/norte-frontend/src/keymap/catalogue.rs`:
   `live("<name>", counts, <Effect>)`, or `planned("<name>", reason, issue,
   <Effect>)` if a preset may bind it before it's built. The effect has no
   default (ADR 0126).
   Pick the one the reader must be warned about first: `Inert`,
   `ReadsContent`, `Launches`, `Writes`, `Destroys`, `SendsOut`. Anything that
   isn't `Inert` goes into the list in
   `el_conjunto_que_no_es_inerte_es_exactamente_este`. `counts: true` goes
   into `el_conjunto_con_contador_es_exactamente_este` (ADR 0044).
3. **Availability** — `crates/norte-frontend/src/availability.rs::verdict`:
   if the command writes, give it an arm that says WHERE it writes (source,
   destination) and in which order the vetoes are reported. Without an arm it
   is available everywhere, even inside a zip.
4. **TUI** — the `commands!` table in `crates/norte-tui/src/keymap.rs` (name →
   variant), then the arm in `crates/norte-tui/src/dispatch.rs`. The arm stays
   one to three lines and calls a function in its own module.
5. **Window** — `crates/norte-ui-host/src/commands.rs`: add it to
   `IMPLEMENTADOS` and `efecto_de`. Viewer commands go in
   `IMPLEMENTADOS_VISOR` and `efecto_visor_de`, and dialog verbs in
   `IMPLEMENTADOS_DIALOGO`. Those two lists are not filtered in read-only
   mode, so a test requires everything in them to be `Inert`. Then write the
   effect in
   `crates/norte-ui-host/src/controller/`. If the window can't do it yet, leave
   it out: the key then resolves to `NotHere` and says so. That is honest;
   binding it to a stub is not.
6. **Menu** — `crates/norte-frontend/src/menu.rs::MENUS`. Everything built
   goes in exactly one menu. Its colour comes from the effect, so don't add a
   role by hand.
7. **Seven presets** — `crates/norte-frontend/presets/keymap/*.toml`. Follow
   the CLAUDE.md section "A key change is not done until EVERY preset is
   done": bind it in all seven or explain the omission in that preset's
   header, check the imported four against their real documentation, never
   use `shift+<char>`, and bind it in the right screen.
8. **Strings** — `help-cmd-<name-with-dashes>` in BOTH
   `crates/norte-i18n/i18n/en.ftl` and `es.ftl`, and the command in the right
   topic under `crates/norte-help/topics/{en,es}/*.md`. It goes in both
   languages, and `norte-help/tests/corpus.rs` checks them.
9. **Goldens** — `NORTE_UPDATE_GOLDEN=1` for the `norte-cli` help golden and
   the `norte-ui-host` golden if the bridge shows it. Read the diff: it must
   show your command and nothing else.
10. **Loop** — `just t norte-frontend`, `just t norte-ui-host`,
    `just t norte-tui`, `just c`, and `cargo test -p norte-frontend --doc`.
    The gate runs once, at the end of the plan.
