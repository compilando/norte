# On-disk compatibility corpus

One directory per release. Each holds what that release left in a user's
config directory, frozen, and what it answered about it. The tests load
every directory with the current code:

- `crates/norte-cli/tests/disk_compat.rs`: `norte doctor` (config, keymaps,
  layout, columns, connections, policy), the journal read back entry for
  entry, a new entry appended to the old journal with the chain intact, and
  an index query.
- `crates/norte-frontend/tests/disk_compat.rs`: openers, shells, user
  commands, themes and profiles, through the real loaders.

The promise is one way: **a newer norte opens an older norte's files.** An
older binary reading a newer file is not covered (`norte.toml` is
`deny_unknown_fields`).

## Layout

```
<version>/config/     a NORTE_CONFIG_DIR, never edited after the release
<version>/expected/   journal.jsonl            `norte audit export --format jsonl`
                      index-query-copia.txt    `norte index query /srv/norte/w copia`
                      doctor-warnings.txt      warning codes doctor may raise
```

## When a test here fails

Something a user already has stopped loading. Do not edit the corpus to
make it pass. Read the old value as the new one (a serde alias, a
migration), or record the break in the CHANGELOG and in an ADR.

## Freezing a release

`just release <tag>` does it with the published binary
(`scripts/release/freeze-disk-compat.sh`; `just disk-compat-freeze` by
hand). The script:

1. copies the previous corpus's text configs (not its databases);
2. writes `journal.db` and `index.db` with the release binary inside
   `bwrap`, so the journal records `/srv/norte/...` and not a home;
3. writes `expected/` from that binary's answers.

Then, by hand:

- add to `config/` whatever the release made new (a key, a file kind),
  each set to a non-default value;
- commit the directory.

`expected/commands.txt` comes from the TAG's catalogue source: the
command names a user's keymap may still use. A rename adds a row to
`RENAMED` in `keymap/catalogue.rs`; the test fails otherwise.

The 0.3.0-alpha.12 text files were written by hand from the schema and
accepted by the released binary's `norte doctor`. The files doctor does not
read were checked by the in-tree loaders at `004f1c43` (alpha.12 plus one
keymap commit).
