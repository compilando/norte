# Translation: the wire surface

Everything below is a contract with something outside the source tree: a
client, a plugin, a config file on a user's disk, a script. The translation
must not change any of it unless a row here says so.

**Status: waiting for validation.** Nothing in this list has been touched.

## Finding: the external surface is already English

The Spanish in this repository lives in comments, rustdoc, test names, local
identifiers and test fixtures. It does not live in the contracts. So almost
every row is option **(c) do not touch**: there is nothing to translate, and
option (a) (rename in code, keep the external name with `serde(rename)`) has
nothing to act on.

How that was checked (2026-09-24, `main` at `0298850f`):

- The property names and enum values of the three generated schemas —
  `docs/schema/proto.schema.json` (291 properties, 149 enum values),
  `norte.schema.json` (75, 4) and `keymap.schema.json` (5) — matched against
  a list of ~150 common Spanish words and against accented letters. Nothing
  matched.
- Every `#[derive(Serialize|Deserialize)]` type in `crates/` (including the
  ones no schema covers: the `norte-ui-host` bridge DTOs, the plugin
  manifest, the journal) — field and variant names, same test. The only hits
  were English words that look like Spanish (`panel_bar`, `menu`,
  `first_visible`).
- Every `serde(rename|alias = "…")` literal (15): all English.
- CLI flags and subcommands of `ntc` and `norte`, MCP tool names, the
  command catalogue (`crates/norte-proto/tests/golden/catalogo.tsv`, ids such
  as `archive.pack`), Fluent message ids (1854), environment variables: all
  English.

## The rows

| surface | where | examples | option |
| --- | --- | --- | --- |
| JSON-RPC method names | `crates/norte-proto/src/methods.rs`, golden `catalogo.tsv` | `fs.copy`, `archive.pack`, `ai.rename_plan` | **(c)** already English |
| JSON-RPC params and results | `norte-proto` types, `proto.schema.json` | `path`, `dest`, `dest_trash` | **(c)** |
| MCP tool names and schemas | `crates/norte-mcp/src/bridge.rs` | `list_dir`, `stat`, `read_file`, `delete`, `task_status`, `request_scope`, `compare`, `sync_plan` | **(c)** |
| Plugin WIT interface | `crates/norte-plugin-host/wit/norte-plugin.wit` | interface, record and function names | **(c)** identifiers English; its `//` comments are Spanish and ARE translated (they are not part of the ABI) |
| Plugin manifest (`plugin.toml`) | `crates/norte-plugin-host/src/manifest.rs` | `[contributions] panel` | **(c)** |
| Config keys | `norte.toml`, `keymap.toml`, `connections.toml`, `policy.toml`, `openers.toml`, `plugins-state.toml`, `lua-trust.toml` | `[ui] row_stripes`, `[[hotlist]]` | **(c)** |
| Theme files | `crates/norte-theme` (serde), user `themes/*.toml` | role names `hostile-badge`, `[files.kind]` | **(c)** — the pilot changed only comments |
| Keymap command ids and contexts | `crates/norte-frontend/presets/keymap/*.toml` | `pane.switch`, `browse`, `dialog` | **(c)** |
| UI bridge (Rust → webview) | `crates/norte-ui-host/src/{bridge,dto,action}.rs` ↔ `ui/src/types.ts` | `ViewChange::panel_bar` | **(c)** |
| Environment variables | across crates | `NORTE_LANG`, `NORTE_CONFIG_DIR`, `NORTE_NO_SPLASH`, `NORTE_NO_WIZARD`, `NORTE_SECRETS_KEY`, `NORTE_KNOWN_HOSTS`, `NORTE_UPDATE_GOLDEN`, `NORTE_UPDATE_SCHEMA` | **(c)** |
| CLI flags and subcommands | `crates/norte-cli/src/main.rs`, `norte-tui` | `--preset`, `--layout`, `--no-splash`, `norte mcp serve` | **(c)** |
| Files written to disk | config, state and data dirs | `journal.db`, `journal-anchors.jsonl`, `session.json`, `.norte-partial.*`, `.norte-renames.log`, `secrets.key`, `sync-spools/`, `layouts/`, `profiles/`, `themes/` | **(c)** |
| Fluent message ids | `crates/norte-i18n/i18n/{en,es}.ftl` | `modal-trash-title` | **(c)** the ids; the `es` values stay Spanish by design |
| Help topic ids and files | `crates/norte-help/topics/{en,es}/*.md` | `archives.md` | **(c)** already localized |

## What looks like wire and is not

These are Spanish and may be translated freely; listed so nobody stops on them:

- `Debug` field labels (`.field("estado", …)` in `norte-core/src/embedded.rs`):
  output for developers, not a contract.
- File names that only tests create (`destino.txt`, `norte/secretos.age`,
  `themes/noche.toml`): fixtures inside a temp dir. **Exception:** a hostile or
  encoding fixture in `norte-testkit` keeps its bytes — its name IS the test.
- Test data strings that exercise non-ASCII (`"dice \"hola\"\ny adiós"` in
  `norte-theme`): kept, the accent is the point.
- `NORTE_SECRET_TRABAJO`, `NORTE_SECRET_MI_SERVER_1`: derived in tests from a
  connection's name. Not a variable norte reads by that name.

## The language option the brief asked for

It already exists and needs no new flag:

- `NORTE_LANG` overrides the system locale, which is read from `LC_ALL`, then
  `LC_MESSAGES`, then `LANG` (`crates/norte-i18n/src/lib.rs`). Both locales
  are shipped; `en` is the fallback.
- **Open question, the only decision here:** do you also want a config key
  (`[ui] lang`) and/or a `--lang` flag? It would be a NEW row in this table,
  option (b) by nature (it adds a key), and it is not translation — it is a
  feature. My proposal: a separate branch, not this one.

## The one real naming decision: the test fixture file name

`crates/norte-proto/tests/golden/catalogo.tsv` is a Spanish file name inside a
test tree, read by the test `crates/norte-proto/tests/catalogo.rs` (also a
Spanish name). Neither is wire — the catalogue's *content* is — so the brief's
step 4 (rename files) renames both to `catalogue.{tsv,rs}` in the `norte-proto`
batch. Only the paths change; the content is identical.
