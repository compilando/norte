# H3e — plugins bring their own help page

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A plugin ships a `help.md`, the host bounds it, the wire carries it on
demand, and the help overlay shows it as one more page — with its commands as
runnable rows that dim when the plugin is not active.

**Architecture:** One protocol bump (`PluginInfo.has_help` + `plugin.help`),
host-side bounding in `PluginRegistry` reusing the SAME caps `norte-help`
already applies to hostile input, and a frontend that re-parses the bounded
markdown with `parse_untrusted` and folds in the flags the sender learnt. The
plugin-state cache H3d deferred lands as a SNAPSHOT taken when the overlay
opens — the same freezing decision H3d already made for capabilities and
connection state, so no new invalidation problem enters `App`.

**Tech Stack:** Rust, `serde`/`schemars` (wire + goldens), `norte-help`
(hostile-mode parser, already built in H3a), ratatui (overlay), Fluent (i18n).

---

## Decisions taken before writing this plan

**1. `PluginInfo.settings` is OFF the table — the design entry is obsolete.**
The 2026-08-04 design proposed carrying `PluginInfo.settings` "opportunistically,
same bump", to close the display debt P2 deferred. That debt was already closed:
proto **0.28.0** (G3c, ADR 0037) shipped `plugin.get_config` / `plugin.set_config`,
which expose `[config]` with its SCHEMA rather than a flattened display map. Adding
`settings` now would be a second, weaker way to read the same thing. H3e carries
`has_help` and `plugin.help` and nothing else. Record it as an amendment (Task 9).

**2. Plugin state is a snapshot taken when the help opens** (user decision).
`plugins_list()` is awaited once in the `F1` path and frozen alongside
`App::freeze_help_facts`. No `App`-wide cache, no refresh events, no round trip
while painting a frame. It dies with the overlay.

**3. The palette keeps hiding inactive plugin commands** (user decision).
`norte_frontend::palette::plugin_rows` still filters on `approved && enabled`.
`Reason::PluginInactive` gets its surface in the help page — where a reader is
LOOKING at a plugin's documentation and needs to be told why its rows will not
run — and nowhere else.

**4. A plugin never shadows a built-in page.** `parse_untrusted` forces the topic
id to the host-assigned `fallback_id`, which is the plugin's catalogue id, so a
plugin published as `copying` would collide with the corpus topic of that name.
`HelpState::set_plugins` DROPS a node whose id resolves in the corpus (or equals
the synthetic `keys` id), and `norte doctor` reports it. Fail-closed: the corpus
always wins, and the loss is visible.

**5. Body-level `{{cmd:}}` refusals are not reported, header-level ones are.**
`take_mark` degrades a foreign command mark to literal text with no counter, and
threading one through `blocks_of` would touch every span signature for a
diagnostic. `doctor` reports foreign command ids declared in the FRONT MATTER —
where an author declares the runnable rows and where the loss is silent — and its
rustdoc says plainly that body marks are not counted.

---

## File structure

| File | Responsibility |
|---|---|
| `crates/norte-proto/src/methods.rs` | `PluginInfo.has_help`, `PLUGIN_HELP`, `PluginHelpParams`/`PluginHelpResult`, version history, `PROTOCOL_VERSION = 0.34.0` |
| `crates/norte-proto/tests/golden/types/methods.json` | wire freeze: 4 new fixtures |
| `crates/norte-proto/tests/golden_types.rs` | the Rust cases behind those fixtures + the fixture count |
| `docs/schema/proto.schema.json` | regenerated artifact |
| `crates/norte-help/src/parse.rs` | `sanitize_untrusted` (the cap, shared with the host) + `foreign_commands` (the doctor's signal) |
| `crates/norte-plugin-host/src/catalog.rs` | `PluginEntry.has_help` — one `is_file` at discovery, no read |
| `crates/norte-core/src/plugins.rs` | `PluginRegistry::help_of` — reads and bounds `help.md` on demand |
| `crates/norte-core/src/daemon/server.rs` | `handle_plugin_help`, dispatch arm |
| `crates/norte-core/src/backend.rs` | `Backend::plugin_help`, embedded + remote |
| `crates/norte-cli/src/doctor.rs` | `plugin-help` findings |
| `crates/norte-frontend/src/help.rs` | `PluginNode`, `set_plugins`, `install_plugin_topic`, `current_topic` |
| `crates/norte-frontend/src/availability.rs` | `verdict_with_plugins` for `plugin:` dispatch keys |
| `crates/norte-tui/src/help.rs` | `TuiChords::with_plugins` |
| `crates/norte-tui/src/help_render.rs` | plugin badge line, `into_static` |
| `crates/norte-tui/src/main.rs` | snapshot on open, fetch on demand, extension-manager affordance |
| `crates/norte-i18n/i18n/{en,es}.ftl` | group label, badges, doctor details |

---

### Task 1: proto 0.34.0 — `has_help` and `plugin.help`

**Files:**
- Modify: `crates/norte-proto/src/methods.rs`
- Modify: `crates/norte-proto/tests/golden_types.rs`
- Modify: `crates/norte-proto/tests/golden/types/methods.json`
- Modify: `docs/schema/proto.schema.json` (regenerated, never hand-edited)

- [ ] **Step 1: Write the failing golden cases**

In `crates/norte-proto/tests/golden_types.rs`, inside the plugin family block
(next to the existing `plugin_info` / `plugin_info_with_commands` cases), add:

```rust
    check_one(
        &fixtures,
        "plugin_info_with_help",
        &PluginInfo {
            id: "acme.ftp".to_owned(),
            name: "FTP".to_owned(),
            publisher: "ACME".to_owned(),
            version: "0.1.0".to_owned(),
            category: "provider".to_owned(),
            capabilities: vec!["fs-read".to_owned()],
            approved: true,
            enabled: true,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            has_help: true,
        },
    );
    check_one(
        &fixtures,
        "plugin_help_params",
        &PluginHelpParams {
            id: "acme.ftp".to_owned(),
        },
    );
    check_one(
        &fixtures,
        "plugin_help_result",
        &PluginHelpResult {
            markdown: "+++\ntitle = \"FTP\"\n+++\nBody.".to_owned(),
            truncated: false,
            lossy: false,
        },
    );
    check_one(
        &fixtures,
        "plugin_help_result_flags",
        &PluginHelpResult {
            markdown: "cut".to_owned(),
            truncated: true,
            lossy: true,
        },
    );
```

And in the method-name pin test (the one that already asserts
`methods::PLUGIN_GET_CONFIG == "plugin.get_config"`, `golden_types.rs:1812`):

```rust
    assert_eq!(methods::PLUGIN_HELP, "plugin.help");
```

Add the fixtures to `crates/norte-proto/tests/golden/types/methods.json`, in the
file's alphabetical position:

```json
  "plugin_help_params": {
    "id": "acme.ftp"
  },
  "plugin_help_result": {
    "markdown": "+++\ntitle = \"FTP\"\n+++\nBody.",
    "truncated": false,
    "lossy": false
  },
  "plugin_help_result_flags": {
    "markdown": "cut",
    "truncated": true,
    "lossy": true
  },
  "plugin_info_with_help": {
    "id": "acme.ftp",
    "name": "FTP",
    "publisher": "ACME",
    "version": "0.1.0",
    "category": "provider",
    "capabilities": ["fs-read"],
    "approved": true,
    "enabled": true,
    "commands": [],
    "columns": [],
    "has_help": true
  },
```

Bump the fixture count guard (`golden_types.rs:456`) from `106` to `110`.

**The existing `plugin_info` and `plugin_info_with_commands` fixtures must NOT
change** — that is the point of `skip_serializing_if` below, and their staying
byte-identical is what proves the field is additive in the strong sense.

- [ ] **Step 2: Run to verify it fails**

Run: `just t norte-proto`
Expected: FAIL — `PluginHelpParams` / `PluginHelpResult` / `has_help` do not exist.

- [ ] **Step 3: Implement the wire**

In `crates/norte-proto/src/methods.rs`, add to `PluginInfo` (after `columns`):

```rust
    /// `true` si el plugin trae un `help.md` junto a su `plugin.toml`
    /// (H3e, 0.34.0). Es DISCOVERY barato: decide si el nodo del plugin
    /// aparece en la barra de temas de la ayuda, y evita que 64 KiB por
    /// plugin viajen en cada `plugin.list` — el contenido se pide aparte
    /// con [`PLUGIN_HELP`], bajo demanda.
    ///
    /// El host lo calcula con UN `is_file` al descubrir: no lee el fichero,
    /// no lo parsea, y por tanto un `help.md` presente pero ilegible o vacío
    /// sale `true` aquí y se degrada al pedirlo (markdown vacío), que es la
    /// dirección correcta — la ayuda es cosmética y jamás tumba un plugin.
    ///
    /// `skip_serializing_if` sobre `false`: un plugin sin ayuda produce un
    /// payload IDÉNTICO byte a byte al de 0.33 (mismo criterio aditivo
    /// fuerte que los `attrs` de 0.30). Un peer N-1 que construye su propio
    /// `PluginInfo` no lo emite y aquí se toma por `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_help: bool,
```

Add the method constant next to `PLUGIN_GET_CONFIG`/`PLUGIN_SET_CONFIG`:

```rust
/// `plugin.help` — la página de ayuda de UN plugin (H3e, 0.34.0), BAJO
/// DEMANDA: el host devuelve el `help.md` ya ACOTADO (tope de bytes de
/// `norte_help::Limits::untrusted`) y ya decodificado a UTF-8 válido, con
/// dos banderas que cuentan qué pasó al acotarlo. ABIERTO como
/// [`PLUGIN_LIST`]: leer documentación no consiente nada.
///
/// El texto es de TERCEROS y no está enmascarado: el frontend lo vuelve a
/// parsear con `norte_help::parse_untrusted`, que enmascara al construir el
/// modelo. Parsear en los dos lados es deliberado — host-side para que
/// `norte doctor` y el catálogo puedan reportar problemas sin un frontend
/// delante, cliente-side porque el wire lleva TEXTO, no un árbol.
pub const PLUGIN_HELP: &str = "plugin.help";
```

And the two types, next to `PluginGetConfigParams`:

```rust
/// Params de [`PLUGIN_HELP`].
///
/// ```
/// use norte_proto::methods::PluginHelpParams;
/// let p: PluginHelpParams = serde_json::from_str(r#"{"id":"acme.ftp"}"#).unwrap();
/// assert_eq!(p.id, "acme.ftp");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginHelpParams {
    /// Id del plugin cuyo `help.md` se pide. Es una CLAVE DE BÚSQUEDA
    /// contra el catálogo: el host la resuelve contra los plugins que
    /// descubrió y jamás la compone en una ruta de fichero.
    pub id: String,
}

/// Result de [`PLUGIN_HELP`]: el `help.md` acotado y qué se perdió al
/// acotarlo.
///
/// ```
/// use norte_proto::methods::PluginHelpResult;
/// let r: PluginHelpResult =
///     serde_json::from_str(r#"{"markdown":"body","truncated":true,"lossy":false}"#)
///         .unwrap();
/// assert!(r.truncated && !r.lossy);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginHelpResult {
    /// El `help.md` del plugin, ya acotado y ya UTF-8 VÁLIDO (el host
    /// decodifica y sustituye lo irrecuperable). Sin `help.md` legible:
    /// cadena vacía, nunca un error — la ayuda es cosmética.
    pub markdown: String,
    /// El fichero superaba el tope y se cortó. Viaja porque el receptor NO
    /// puede deducirlo: el texto le llega ya corto, así que su propio parseo
    /// saldría limpio y la insignia — la mitigación entera frente a un
    /// `help.md` hostil — se apagaría en silencio.
    #[serde(default)]
    pub truncated: bool,
    /// Algún byte no decodificó bajo ninguna lectura y salió `U+FFFD`.
    /// Viaja por la misma razón que `truncated`.
    #[serde(default)]
    pub lossy: bool,
}
```

Bump `PROTOCOL_VERSION` to `"0.34.0"` and append to the version history, right
after the 0.33.0 paragraph:

```rust
/// 0.34.0 (H3e): la ayuda de los PLUGINS por el wire. [`PluginInfo`] gana
/// `has_help: bool` (discovery barato, `skip_serializing_if` sobre `false` —
/// un plugin sin ayuda produce el MISMO payload que en 0.33) y aparece el
/// método [`PLUGIN_HELP`] ([`PluginHelpParams`] → [`PluginHelpResult`]), que
/// entrega el `help.md` ya acotado y decodificado más las banderas
/// `truncated`/`lossy` que el receptor no puede deducir. Ventana
/// N=0.34.x / N-1=0.33.x: un cliente 0.33 ignora el campo desconocido, no
/// emite `has_help` (default `false` aquí) y jamás llama al método nuevo; un
/// daemon 0.33 responde `MethodNotFound`, que el frontend trata como «este
/// plugin no tiene página», nunca como un fallo.
```

- [ ] **Step 4: Regenerate the schema artifact and run**

```bash
NORTE_UPDATE_SCHEMA=1 cargo test -p norte-proto --features schema --test schema
just t norte-proto && just c norte-proto
```
Expected: PASS. `git diff docs/schema/proto.schema.json` must show ONLY the two
new types plus `has_help`.

Add the two new types to `ProtocolSchema` in `crates/norte-proto/tests/schema.rs`
(alphabetical position, next to `plugin_get_config_params`):

```rust
    plugin_help_params: PluginHelpParams,
    plugin_help_result: PluginHelpResult,
```

(The `todo_tipo_con_schema_esta_en_el_artefacto` guard fails without this.)

- [ ] **Step 5: Fix the construction sites**

`PluginInfo` is constructed in `norte-core` (`plugins.rs::list`), in
`norte-tui/src/app.rs` tests, and in `norte-frontend` tests. Compile and add
`has_help: false` at each until green:

```bash
cargo check --workspace --all-targets 2>&1 | grep -E "^error" | head
```

(Task 3 replaces the `false` in `plugins.rs::list` with the real value.)

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/norte-proto docs/schema crates/norte-core crates/norte-tui crates/norte-frontend
git commit -m "feat(proto): plugin help on the wire — has_help and plugin.help (H3e)"
```

- [ ] **Step 7: protocol-guardian review — MANDATORY**

Dispatch `protocol-guardian` over this diff. Ask specifically: (a) is
`skip_serializing_if` on `has_help` right, or should the field always be emitted;
(b) does the N-1 window statement hold in BOTH directions; (c) are the goldens
enough to freeze `plugin.help` (params, result, and the flag-carrying result).
Apply what it returns before Task 2.

---

### Task 2: `norte-help` — the cap the host shares, and the doctor's signal

**Files:**
- Modify: `crates/norte-help/src/parse.rs`
- Modify: `crates/norte-help/src/lib.rs` (re-exports)

The host must bound a `help.md` for the wire WITHOUT producing a `Topic`: the
protocol carries text. Duplicating the byte cap and the decoding in `norte-core`
is how two caps drift apart, so the cut is exposed here, and `parse_untrusted`
becomes its first caller.

- [ ] **Step 1: Write the failing tests**

In `crates/norte-help/src/parse.rs`, in the `mod tests` block:

```rust
    #[test]
    fn sanitizar_recorta_en_el_mismo_tope_que_el_parser() {
        let gordo = vec![b'a'; Limits::untrusted().max_bytes + 100];
        let s = sanitize_untrusted(&gordo);
        assert!(s.truncated, "pasarse del tope se declara");
        assert!(!s.lossy, "ascii no es lossy");
        assert!(
            s.markdown.len() <= Limits::untrusted().max_bytes,
            "el texto sale acotado: {}",
            s.markdown.len()
        );
    }

    #[test]
    fn sanitizar_devuelve_utf8_valido_de_bytes_rotos() {
        // Lo que el wire promete: `String`, siempre. Un byte irrecuperable
        // sale `U+FFFD` y la bandera lo dice.
        let s = sanitize_untrusted(b"hola \xFF\xFE mundo");
        assert!(s.lossy, "un byte irrecuperable se declara");
        assert!(s.markdown.contains('\u{FFFD}'));
    }

    #[test]
    fn sanitizar_y_reparsear_da_el_mismo_cuerpo_que_parsear_directo() {
        // El salto de wire de H3e: host sanitiza, frontend parsea. El modelo
        // resultante tiene que ser el mismo que el del parseo directo, salvo
        // las banderas, que el emisor vuelve a poner con `fold_flags`.
        let src = b"+++\ntitle = \"FTP\"\n+++\nCuerpo con {{cmd:plugin:acme.ftp:sync}}.";
        let directo = parse_untrusted(src, "acme.ftp", None);
        let s = sanitize_untrusted(src);
        let por_el_wire = parse_untrusted(s.markdown.as_bytes(), "acme.ftp", None)
            .fold_flags(s.truncated, s.lossy);
        assert_eq!(directo.topic.blocks, por_el_wire.topic.blocks);
        assert_eq!(directo.topic.title, por_el_wire.topic.title);
    }

    #[test]
    fn comandos_ajenos_del_encabezado_se_reportan() {
        let src = "+++\ncommands = [\"plugin:acme.ftp:sync\", \"fs.copy\", \
                   \"plugin:otro:borrar\"]\n+++\ncuerpo";
        let ajenos = foreign_commands(src, "acme.ftp");
        assert_eq!(ajenos, vec!["fs.copy".to_owned(), "plugin:otro:borrar".to_owned()]);
    }

    #[test]
    fn sin_encabezado_no_hay_comandos_ajenos() {
        assert!(foreign_commands("solo cuerpo", "acme.ftp").is_empty());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `just t norte-help`
Expected: FAIL — `sanitize_untrusted` and `foreign_commands` not found.

- [ ] **Step 3: Implement**

In `crates/norte-help/src/parse.rs`, above `parse_untrusted`:

```rust
/// A plugin `help.md` cut down to what the wire may carry.
///
/// It is the FIRST half of `parse_untrusted`, exposed on its own because the
/// host needs exactly that half: the protocol carries markdown TEXT, not a
/// parsed tree ([`crate::parse_untrusted`] runs again on the receiving side).
/// Keeping the cut here rather than in `norte-core` is what stops the byte cap
/// and the encoding detection from existing twice and drifting apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sanitized {
    /// The text, cut at a UTF-8 boundary and decoded — ALWAYS valid UTF-8.
    pub markdown: String,
    /// The source was longer than [`Limits::untrusted`] allows.
    pub truncated: bool,
    /// Some byte did not decode under any reading and came out `U+FFFD`.
    pub lossy: bool,
}

/// Cuts and decodes untrusted `help.md` bytes for the wire.
///
/// Never fails: oversize truncates, undecodable bytes become `U+FFFD`, and both
/// losses are reported in the flags rather than raised as an error. Help is
/// cosmetic and must never brick an approved plugin.
///
/// ```
/// use norte_help::sanitize_untrusted;
///
/// let s = sanitize_untrusted(b"+++\ntitle = \"FTP\"\n+++\nbody");
/// assert!(!s.truncated && !s.lossy);
/// assert!(s.markdown.ends_with("body"));
/// ```
#[must_use]
pub fn sanitize_untrusted(bytes: &[u8]) -> Sanitized {
    let limits = Limits::untrusted();
    let truncated = bytes.len() > limits.max_bytes;
    let head = cut_at_boundary(bytes, limits.max_bytes);
    let (markdown, lossy) = decode_source(bytes, head);
    Sanitized {
        markdown,
        truncated,
        lossy,
    }
}

/// Command ids a plugin's front matter declares that are NOT its own.
///
/// The rule is `parse_untrusted`'s: a plugin documents `plugin:{id}:{command}`
/// and nothing else. Those entries are silently dropped from the model — which
/// is the right runtime behaviour and a terrible authoring experience, so
/// `norte doctor` reports them and this is where it gets them.
///
/// Only the HEADER is examined. A `{{cmd:…}}` written in the BODY that names a
/// foreign command degrades to literal text and is not counted here: the
/// parser refuses it inside the span cutter, with no counter to thread out, and
/// adding one would touch every span signature for a diagnostic.
///
/// ```
/// use norte_help::foreign_commands;
///
/// let src = "+++\ncommands = [\"fs.copy\"]\n+++\nbody";
/// assert_eq!(foreign_commands(src, "acme.ftp"), vec!["fs.copy".to_owned()]);
/// ```
#[must_use]
pub fn foreign_commands(source: &str, plugin_id: &str) -> Vec<String> {
    let Ok((fm, _)) = front_matter::split(source) else {
        return Vec::new();
    };
    fm.commands
        .into_iter()
        .filter(|c| !is_own_command(c, plugin_id))
        .collect()
}
```

Then rewrite the head of `parse_untrusted` to consume it (everything below the
`front_matter::split` call is unchanged):

```rust
pub fn parse_untrusted(bytes: &[u8], fallback_id: &str, publisher: Option<String>) -> Parsed {
    let limits = Limits::untrusted();
    let Sanitized {
        markdown: text,
        truncated: mut truncated,
        lossy,
    } = sanitize_untrusted(bytes);
```

Re-export from `crates/norte-help/src/lib.rs` next to `parse_untrusted`:

The line is today (`lib.rs:68`):

```rust
pub use parse::{Limits, ParseError, Parsed, parse_trusted, parse_untrusted}; // tasks 4, 5 and 6
```

It becomes:

```rust
pub use parse::{
    Limits, ParseError, Parsed, Sanitized, foreign_commands, parse_trusted, parse_untrusted,
    sanitize_untrusted,
}; // tasks 4, 5 and 6; H3e adds the wire seam
```

- [ ] **Step 4: Verify**

```bash
just t norte-help && just c norte-help
```
Expected: PASS, including the pre-existing hostile-corpus suite — the refactor
must not move a single one of its assertions.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/norte-help
git commit -m "feat(help): expose the untrusted cut the host needs for the wire (H3e)"
```

---

### Task 3: the host — `has_help` at discovery, `help.md` on demand

**Files:**
- Modify: `crates/norte-plugin-host/src/catalog.rs`
- Modify: `crates/norte-core/src/plugins.rs`
- Modify: `crates/norte-core/Cargo.toml` (new dep: `norte-help`)

**Dependency justification (rule 8):** `norte-core` gains a workspace-internal
dependency on `norte-help`. Benefit: one definition of the untrusted byte cap and
the encoding detection, shared by the host that bounds and the frontend that
renders; size: none (already in the graph, pulled by the frontends); maintenance:
ours; alternative considered: re-implementing the cut in `norte-core`, rejected
because two caps drift and the whole hostile-input story rests on there being one.
No cycle: `norte-help` depends on nothing from `norte-core`.

- [ ] **Step 1: Write the failing tests**

In `crates/norte-plugin-host/src/catalog.rs` tests:

```rust
    #[test]
    fn descubrir_marca_el_plugin_que_trae_help_md() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("acme.ftp");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("plugin.toml"), MANIFEST_MINIMO).expect("manifiesto");
        std::fs::write(dir.join("help.md"), "+++\ntitle = \"FTP\"\n+++\ncuerpo")
            .expect("help.md");
        let cat = Catalog::load_dir(tmp.path());
        assert!(cat.plugins[0].has_help, "el help.md descubierto se anuncia");
    }

    #[test]
    fn sin_help_md_no_se_anuncia_ayuda() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("acme.ftp");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("plugin.toml"), MANIFEST_MINIMO).expect("manifiesto");
        let cat = Catalog::load_dir(tmp.path());
        assert!(!cat.plugins[0].has_help);
    }
```

(`MANIFEST_MINIMO` is the minimal valid manifest string the module's existing
tests already use — reuse that constant or the helper that writes one.)

In `crates/norte-core/src/plugins.rs` tests:

```rust
    #[test]
    fn help_of_devuelve_el_markdown_acotado_del_plugin() {
        let tmp = tempfile::tempdir().expect("tmp");
        write_plugin(tmp.path(), "acme.ftp", MANIFIESTO_DEMO);
        std::fs::write(
            tmp.path().join("plugins/acme.ftp/help.md"),
            "+++\ntitle = \"FTP\"\n+++\ncuerpo",
        )
        .expect("help.md");
        let reg = PluginRegistry::discover(tmp.path()).expect("discover");
        assert!(reg.list().plugins[0].has_help, "list lo anuncia");
        let help = reg.help_of("acme.ftp").expect("hay página");
        assert!(help.markdown.contains("cuerpo"));
        assert!(!help.truncated && !help.lossy);
    }

    #[test]
    fn help_of_de_un_id_desconocido_es_none() {
        // Fail-closed: el id viene del WIRE. Se resuelve contra el catálogo y
        // jamás se compone en una ruta — un `../` no llega a tocar el FS.
        let tmp = tempfile::tempdir().expect("tmp");
        write_plugin(tmp.path(), "acme.ftp", MANIFIESTO_DEMO);
        let reg = PluginRegistry::discover(tmp.path()).expect("discover");
        assert!(reg.help_of("../../etc/passwd").is_none());
        assert!(reg.help_of("otro.plugin").is_none());
    }

    #[test]
    fn help_of_acota_un_help_md_enorme_y_lo_declara() {
        let tmp = tempfile::tempdir().expect("tmp");
        write_plugin(tmp.path(), "acme.ftp", MANIFIESTO_DEMO);
        let gordo = "a".repeat(norte_help::Limits::untrusted().max_bytes + 4096);
        std::fs::write(tmp.path().join("plugins/acme.ftp/help.md"), &gordo).expect("help.md");
        let reg = PluginRegistry::discover(tmp.path()).expect("discover");
        let help = reg.help_of("acme.ftp").expect("hay página");
        assert!(help.truncated, "un fichero por encima del tope se declara");
        assert!(help.markdown.len() <= norte_help::Limits::untrusted().max_bytes);
    }

    #[test]
    fn un_help_md_ilegible_es_pagina_vacia_no_error() {
        // La ayuda es cosmética: un `help.md` que no se puede leer nunca
        // tumba el plugin ni la llamada.
        let tmp = tempfile::tempdir().expect("tmp");
        write_plugin(tmp.path(), "acme.ftp", MANIFIESTO_DEMO);
        std::fs::create_dir_all(tmp.path().join("plugins/acme.ftp/help.md")).expect("dir");
        let reg = PluginRegistry::discover(tmp.path()).expect("discover");
        let help = reg.help_of("acme.ftp").expect("el plugin existe");
        assert_eq!(help.markdown, "");
    }
```

(`write_plugin` and a `MANIFIESTO_DEMO` constant already exist in that test
module — `plugins.rs:1114`. Reuse them; if the constant is named differently,
use the existing name.)

- [ ] **Step 2: Run to verify failure**

```bash
just t norte-plugin-host
just t norte-core
```
Expected: FAIL — `has_help` / `help_of` do not exist.

- [ ] **Step 3: Implement**

`crates/norte-plugin-host/src/catalog.rs` — add the field to `PluginEntry`:

```rust
    /// El directorio trae un `help.md` junto al `plugin.toml` (H3e).
    ///
    /// Un `is_file` al descubrir, NUNCA una lectura: el catálogo se recorre
    /// entero en cada `plugin.list` (el registro es efímero por llamada), y
    /// leer 64 KiB por plugin ahí pagaría el contenido en cada listado para
    /// una bandera que solo decide si se pinta un nodo en la barra lateral.
    /// El contenido se lee bajo demanda, en `plugin.help`.
    pub has_help: bool,
```

and set it where the entry is built (inside the `Ok(settings) =>` arm):

```rust
                    Ok(settings) => cat.plugins.push(PluginEntry {
                        has_help: dir.join("help.md").is_file(),
                        manifest,
                        dir,
                        enabled: false,
                        approved: false,
                        settings,
                    }),
```

`crates/norte-core/Cargo.toml` — add under `[dependencies]`:

```toml
# H3e: el host ACOTA el `help.md` de un plugin con el MISMO tope y la misma
# detección de encoding que el parser hostil del frontend. La alternativa era
# repetir el corte aquí, y dos topes que empiezan iguales no siguen iguales.
norte-help.workspace = true
```

`crates/norte-core/src/plugins.rs` — set the wire field in `list()`, next to
`columns`:

```rust
                    // (H3e, 0.34.0) discovery barato: el catálogo ya sabe si
                    // hay `help.md` porque lo miró al descubrir. NO gateado
                    // por approved/enabled — la documentación de un plugin es
                    // justo lo que un humano lee ANTES de aprobarlo, mismo
                    // criterio que `capabilities`/`commands`/`columns`.
                    has_help: e.has_help,
```

and add the reader:

```rust
    /// El `help.md` de `id`, ACOTADO para el wire (H3e).
    ///
    /// `None` si `id` no está en el catálogo. Eso es lo que hace segura la
    /// llamada: `id` viene del WIRE y se usa como CLAVE DE BÚSQUEDA contra los
    /// plugins descubiertos, nunca compuesta en una ruta — la ruta sale del
    /// `dir` que el catálogo guardó al descubrir, así que un `../` en el id no
    /// llega a tocar el sistema de ficheros, solo falla el lookup.
    ///
    /// Un plugin conocido SIEMPRE devuelve `Some`, aunque su `help.md` falte o
    /// no se pueda leer: en ese caso `markdown` es la cadena vacía. La ayuda es
    /// cosmética y no tiene por qué distinguirse de "página en blanco" — lo
    /// que sí distingue es `norte doctor`, que reporta el fichero ausente o
    /// ilegible como un hallazgo `plugin-help`.
    ///
    /// I/O SÍNCRONA: el llamador async va por `spawn_blocking` (regla 2),
    /// igual que el resto de este registro.
    #[must_use]
    pub fn help_of(&self, id: &str) -> Option<norte_proto::methods::PluginHelpResult> {
        let entry = self.catalog.plugins.iter().find(|e| e.manifest.id == id)?;
        let bytes = std::fs::read(entry.dir.join("help.md")).unwrap_or_default();
        let s = norte_help::sanitize_untrusted(&bytes);
        Some(norte_proto::methods::PluginHelpResult {
            markdown: s.markdown,
            truncated: s.truncated,
            lossy: s.lossy,
        })
    }
```

- [ ] **Step 4: Verify**

```bash
just t norte-plugin-host && just t norte-core && just c norte-core
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/norte-plugin-host crates/norte-core
git commit -m "feat(core): the host bounds a plugin's help.md and announces it (H3e)"
```

---

### Task 4: the wire hop — daemon handler and `Backend::plugin_help`

**Files:**
- Modify: `crates/norte-core/src/daemon/server.rs`
- Modify: `crates/norte-core/src/backend.rs`
- Test: `crates/norte-core/tests/daemon.rs`

- [ ] **Step 1: Write the failing test**

In `crates/norte-core/tests/daemon.rs`, modelled on the existing
`plugin.get_config` tests (search `PLUGIN_GET_CONFIG` there for the harness that
writes a plugin into a temp config dir and drives a connection):

```rust
#[tokio::test]
async fn plugin_help_devuelve_la_pagina_acotada_del_plugin() {
    let (cfg, _tmp) = config_dir_con_plugin_demo();
    let h = daemon_con_config(&cfg).await;
    let mut c = h.connect_initialized().await;
    let res = c
        .call(
            methods::PLUGIN_HELP,
            serde_json::json!({ "id": "org.norte.demo" }),
        )
        .await
        .expect("plugin.help responde");
    let help: methods::PluginHelpResult = serde_json::from_value(res).expect("result");
    assert!(help.markdown.contains("demo"), "llega el cuerpo: {help:?}");
    assert!(!help.truncated && !help.lossy);
}

#[tokio::test]
async fn plugin_help_de_un_id_desconocido_es_invalid_params() {
    let (cfg, _tmp) = config_dir_con_plugin_demo();
    let h = daemon_con_config(&cfg).await;
    let mut c = h.connect_initialized().await;
    let err = c
        .call(methods::PLUGIN_HELP, serde_json::json!({ "id": "no.existe" }))
        .await
        .expect_err("un plugin fantasma no tiene página");
    assert_eq!(err.code, norte_proto::wire::INVALID_PARAMS);
}

#[tokio::test]
async fn plugin_help_esta_abierto_a_un_agente() {
    // Mismo criterio que `plugin.list`: leer documentación no consiente nada.
    let (cfg, _tmp) = config_dir_con_plugin_demo();
    let h = daemon_con_config(&cfg).await;
    let mut c = h.connect_initialized_as_agent("s1").await;
    c.call(
        methods::PLUGIN_HELP,
        serde_json::json!({ "id": "org.norte.demo" }),
    )
    .await
    .expect("un agente puede leer la página de un plugin");
}
```

Adapt the helper names to the ones the file actually uses (`config_dir_con_plugin_demo`,
`daemon_con_config`, `connect_initialized*` are placeholders for the existing
harness — the `plugin.get_config` tests at `daemon.rs:2466` show the real ones);
the helper must write a `help.md` containing the word `demo` next to the demo
plugin's `plugin.toml`.

- [ ] **Step 2: Run to verify failure**

Run: `just t norte-core`
Expected: FAIL — `MethodNotFound` for `plugin.help`.

- [ ] **Step 3: Implement the handler**

In `crates/norte-core/src/daemon/server.rs`, next to `handle_plugin_get_config`:

```rust
/// `plugin.help` (H3e): la página de ayuda de un plugin, para CUALQUIER
/// conexión — mismo criterio que `plugin.list`/`plugin.get_config`: leer
/// documentación no consiente nada.
///
/// El `id` es una CLAVE contra el catálogo, jamás un componente de ruta
/// (`PluginRegistry::help_of` lo resuelve contra los plugins descubiertos), así
/// que un id con `../` falla el lookup en vez de salir del directorio. Un id
/// desconocido es `INVALID_PARAMS`, mismo trato que `plugin.set_approval` da a
/// un plugin fantasma.
// `skip_all` SIN el id: viene crudo del wire y no debe llegar al log antes de
// validarse contra el catálogo (mismo criterio que `handle_plugin_set_approval`).
#[tracing::instrument(skip_all)]
fn handle_plugin_help(
    params: Option<serde_json::Value>,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginHelpParams = parse_params(params)?;
    let help = shared
        .plugins
        .lock()
        .expect("plugins lock sano")
        .help_of(&p.id)
        .ok_or_else(|| RpcError::invalid_params("plugin desconocido"))?;
    to_value(&help)
}
```

(Use whatever constructor the file already uses for an `INVALID_PARAMS` error —
copy the exact call from `handle_plugin_set_approval`'s unknown-id branch.)

Dispatch arm, next to `PLUGIN_GET_CONFIG`:

```rust
        // plugin.help (H3e): ABIERTO, mismo criterio que plugin.list.
        methods::PLUGIN_HELP => handle_plugin_help(req.params, shared),
```

**Blocking I/O note (rule 2):** `help_of` reads a file synchronously while
holding the plugins lock, exactly as `handle_plugin_list`'s `list()` and
`handle_plugin_get_config` already do — the same bounded, local read the
catalogue does at discovery. If the reviewer objects, the fix is to lift the
`dir` out under the lock and read in `spawn_blocking`; do NOT hold the lock
across an `.await`.

- [ ] **Step 4: Implement the Backend**

In `crates/norte-core/src/backend.rs`, next to `plugin_get_config`:

```rust
    /// La página de ayuda de un plugin (H3e, 0.34.0), ya acotada por el host.
    /// Embebido: registro EFÍMERO por llamada en `spawn_blocking` (regla 2),
    /// mismo criterio de coste que [`Self::plugin_get_config`].
    ///
    /// # Errors
    /// Taxonomía del protocolo; [`Error::NotFound`] si `id` no está en el
    /// catálogo. Con el daemon caído, `ProviderUnavailable{retryable:true}`.
    pub async fn plugin_help(
        &self,
        id: &str,
    ) -> Result<norte_proto::methods::PluginHelpResult, Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                tokio::task::spawn_blocking(
                    move || -> Result<norte_proto::methods::PluginHelpResult, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        reg.help_of(&id).ok_or(Error::NotFound)
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_help(id).await,
        }
    }
```

and in the remote `impl` block, next to `plugin_get_config` (`backend.rs:2667`):

```rust
        /// `plugin.help` contra el daemon (H3e, 0.34.0). Sin fallback
        /// especial: un daemon N-1 (0.33, sin el handler) responde
        /// `MethodNotFound`, que `call_timed`/`to_taxonomy` degradan a un
        /// error genérico — el frontend lo trata como "este plugin no tiene
        /// página" y sigue pintando la ayuda, nunca como un fallo.
        pub(super) async fn plugin_help(
            &self,
            id: &str,
        ) -> Result<methods::PluginHelpResult, Error> {
            self.call_timed(
                methods::PLUGIN_HELP,
                &methods::PluginHelpParams { id: id.to_owned() },
            )
            .await
        }
```

(`Error::NotFound` may take fields in this codebase — copy the exact variant
construction from a neighbouring `NotFound` in the same file.)

- [ ] **Step 5: Verify and commit**

```bash
just t norte-core && just c norte-core
cargo fmt --all
git add crates/norte-core
git commit -m "feat(core): plugin.help over the wire, open like plugin.list (H3e)"
```

---

### Task 5: `norte doctor` — the `plugin-help` findings

**Files:**
- Modify: `crates/norte-cli/src/doctor.rs`
- Modify: `crates/norte-cli/Cargo.toml` (dep `norte-help`)
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`

- [ ] **Step 1: Write the failing tests**

In `crates/norte-cli/src/doctor.rs` tests (follow the shape of the existing
`plugin-digest-stale` test at `doctor.rs:1001`):

```rust
    #[test]
    fn un_help_md_recortado_sale_como_hallazgo() {
        let tmp = tempfile::tempdir().expect("tmp");
        escribe_plugin(tmp.path(), "acme.ftp");
        std::fs::write(
            tmp.path().join("plugins/acme.ftp/help.md"),
            "a".repeat(norte_help::Limits::untrusted().max_bytes + 10),
        )
        .expect("help.md");
        let f = check_plugins(tmp.path())
            .into_iter()
            .find(|f| f.code == "plugin-help-truncated")
            .expect("se reporta el recorte");
        assert_eq!(f.severity, Severity::Warn);
        assert!(f.detail.contains("acme.ftp"));
    }

    #[test]
    fn un_help_md_con_comandos_ajenos_sale_como_hallazgo() {
        let tmp = tempfile::tempdir().expect("tmp");
        escribe_plugin(tmp.path(), "acme.ftp");
        std::fs::write(
            tmp.path().join("plugins/acme.ftp/help.md"),
            "+++\ncommands = [\"fs.copy\"]\n+++\ncuerpo",
        )
        .expect("help.md");
        let f = check_plugins(tmp.path())
            .into_iter()
            .find(|f| f.code == "plugin-help-foreign-command")
            .expect("se reporta el comando ajeno");
        assert_eq!(f.severity, Severity::Warn);
        assert!(f.detail.contains("fs.copy"), "detail: {}", f.detail);
    }

    #[test]
    fn un_plugin_cuyo_id_choca_con_una_pagina_del_corpus_sale_como_hallazgo() {
        // El nodo se DESCARTA en la barra lateral (el corpus gana), así que el
        // autor tiene que enterarse por algún sitio.
        let tmp = tempfile::tempdir().expect("tmp");
        escribe_plugin(tmp.path(), "copying");
        std::fs::write(tmp.path().join("plugins/copying/help.md"), "cuerpo").expect("help.md");
        let f = check_plugins(tmp.path())
            .into_iter()
            .find(|f| f.code == "plugin-help-shadows-topic")
            .expect("se reporta la colisión");
        assert_eq!(f.severity, Severity::Warn);
    }

    #[test]
    fn un_plugin_sin_help_md_no_genera_ruido() {
        let tmp = tempfile::tempdir().expect("tmp");
        escribe_plugin(tmp.path(), "acme.ftp");
        assert!(
            !check_plugins(tmp.path())
                .iter()
                .any(|f| f.code.starts_with("plugin-help")),
            "no documentarse no es un defecto"
        );
    }
```

(`escribe_plugin` stands for whatever helper the module's tests already use to
lay down a plugin dir with a valid manifest — reuse it; the `copying` case needs
the plugin's manifest `id` to be `copying`.)

- [ ] **Step 2: Run to verify failure**

Run: `just t norte-cli`
Expected: FAIL — no `plugin-help-*` findings exist.

- [ ] **Step 3: Implement**

`crates/norte-cli/Cargo.toml`, under `[dependencies]`:

```toml
# H3e: `doctor` juzga un `help.md` con el MISMO tope y las MISMAS reglas de
# pertenencia de comandos que aplican en el host y en el frontend.
norte-help.workspace = true
```

In `check_plugins`, inside the `for p in &list.plugins` loop, after the
`plugin-no-binary` block:

```rust
        if p.has_help {
            findings.extend(check_plugin_help(&registry, &p.id));
        }
```

and the new function below `check_plugins`:

```rust
/// Hallazgos `plugin-help` de UN plugin que anuncia `help.md` (H3e).
///
/// Cuatro cosas, y ninguna es un error duro — la ayuda es cosmética y jamás
/// impide cargar un plugin aprobado:
///
/// * `plugin-help-truncated`: el fichero pasa del tope y se sirve cortado.
/// * `plugin-help-lossy`: hay bytes que no decodifican bajo ninguna lectura.
/// * `plugin-help-foreign-command`: el encabezado declara comandos que no son
///   suyos. Se descartan del modelo en silencio, así que aquí es donde el autor
///   se entera. Solo se miran los del ENCABEZADO: un `{{cmd:…}}` ajeno en el
///   CUERPO degrada a texto literal dentro del cortador de spans, sin contador
///   que sacar, y añadir uno tocaría todas las firmas de spans por un
///   diagnóstico.
/// * `plugin-help-shadows-topic`: el id del plugin es también el de una página
///   del corpus. El frontend descarta el nodo (el corpus gana, fail-closed),
///   así que sin este hallazgo la página del plugin desaparecería sin decir
///   por qué.
///
/// El texto de terceros que entra en un `detail` pasa por
/// [`masked_and_capped`], igual que en `plugin-config`.
fn check_plugin_help(registry: &norte_core::plugins::PluginRegistry, id: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let Some(help) = registry.help_of(id) else {
        return out;
    };
    if help.truncated {
        out.push(Finding {
            section: "plugins",
            severity: Severity::Warn,
            code: "plugin-help-truncated",
            detail: id.to_owned(),
        });
    }
    if help.lossy {
        out.push(Finding {
            section: "plugins",
            severity: Severity::Warn,
            code: "plugin-help-lossy",
            detail: id.to_owned(),
        });
    }
    for ajeno in norte_help::foreign_commands(&help.markdown, id) {
        out.push(Finding {
            section: "plugins",
            severity: Severity::Warn,
            code: "plugin-help-foreign-command",
            detail: format!("{id}: {}", masked_and_capped(&ajeno)),
        });
    }
    // La colisión se juzga contra el corpus INGLÉS: las dos localizaciones
    // tienen los mismos ids por el test de paridad, así que preguntar por las
    // dos reportaría el mismo defecto dos veces.
    if norte_help::topic(norte_help::Lang::En, id).is_some() {
        out.push(Finding {
            section: "plugins",
            severity: Severity::Warn,
            code: "plugin-help-shadows-topic",
            detail: id.to_owned(),
        });
    }
    out
}
```

Add the rendering strings. `crates/norte-i18n/i18n/en.ftl`:

```
cli-doctor-detail-plugin-help-truncated = its help.md is over the size limit and is served cut short
cli-doctor-detail-plugin-help-lossy = its help.md has bytes that do not decode; they render as replacement characters
cli-doctor-detail-plugin-help-foreign-command = its help.md declares a command it does not own; that row is dropped
cli-doctor-detail-plugin-help-shadows-topic = its id is also a built-in help page; the plugin page is not shown
```

`crates/norte-i18n/i18n/es.ftl`:

```
cli-doctor-detail-plugin-help-truncated = su help.md pasa del tope y se sirve cortado
cli-doctor-detail-plugin-help-lossy = su help.md tiene bytes que no decodifican; se pintan como caracteres de reemplazo
cli-doctor-detail-plugin-help-foreign-command = su help.md declara un comando que no es suyo; esa fila se descarta
cli-doctor-detail-plugin-help-shadows-topic = su id es también una página de ayuda del binario; la del plugin no se muestra
```

Wire the four codes into the doctor's text renderer wherever
`cli-doctor-detail-plugin-digest-stale` is consumed (same `match` over `code`).

- [ ] **Step 4: Verify and commit**

```bash
just t norte-cli && just c norte-cli
cargo fmt --all
git add crates/norte-cli crates/norte-i18n
git commit -m "feat(cli): doctor judges a plugin's help.md (H3e)"
```

---

### Task 6: `HelpState` — plugin nodes and plugin pages

**Files:**
- Modify: `crates/norte-frontend/src/help.rs`

- [ ] **Step 1: Write the failing tests**

In `crates/norte-frontend/src/help.rs` tests:

```rust
    fn nodo(id: &str) -> PluginNode {
        PluginNode {
            id: id.to_owned(),
            title: format!("Título de {id}"),
            has_help: true,
            active: true,
        }
    }

    #[test]
    fn los_plugins_con_ayuda_ponen_una_fila_en_la_barra() {
        let mut help = HelpState::new(Lang::En, "Keyboard".to_owned());
        help.set_plugins(vec![nodo("acme.ftp")]);
        assert!(
            help.rows().iter().any(|r| matches!(
                r,
                SidebarRow::Topic { id, .. } if id.as_str() == "acme.ftp"
            )),
            "el nodo del plugin está en la barra: {:?}",
            help.rows()
        );
    }

    #[test]
    fn un_plugin_sin_ayuda_no_pone_fila() {
        let mut help = HelpState::new(Lang::En, "Keyboard".to_owned());
        let mut n = nodo("acme.ftp");
        n.has_help = false;
        help.set_plugins(vec![n]);
        assert!(
            !help.rows().iter().any(|r| matches!(
                r,
                SidebarRow::Topic { id, .. } if id.as_str() == "acme.ftp"
            )),
            "sin página no hay nodo que abrir"
        );
    }

    #[test]
    fn un_plugin_no_tapa_una_pagina_del_corpus() {
        // Fail-closed: el corpus gana. Si no, un plugin publicado como
        // `copying` se comería la página de copiar y todos los [[copying]]
        // del corpus aterrizarían en prosa de terceros.
        let mut help = HelpState::new(Lang::En, "Keyboard".to_owned());
        help.set_plugins(vec![nodo("copying")]);
        help.open(&TopicId::new("copying"));
        let abierto = help.current_topic().expect("hay página");
        assert_eq!(abierto.origin, norte_help::Origin::BuiltIn);
    }

    #[test]
    fn la_pagina_de_un_plugin_se_abre_cuando_llega_del_wire() {
        let mut help = HelpState::new(Lang::En, "Keyboard".to_owned());
        help.set_plugins(vec![nodo("acme.ftp")]);
        help.open(&TopicId::new("acme.ftp"));
        assert_eq!(help.plugin_needs_fetch(), Some("acme.ftp"), "aún no llegó");
        assert!(help.current_topic().is_none(), "no se inventa cuerpo");

        let parsed = norte_help::parse_untrusted(
            b"+++\ntitle = \"FTP\"\ncommands = [\"plugin:acme.ftp:sync\"]\n+++\ncuerpo",
            "acme.ftp",
            None,
        );
        help.install_plugin_topic(parsed.topic);
        assert_eq!(help.plugin_needs_fetch(), None, "ya está instalada");
        assert_eq!(help.current_topic().expect("hay página").title, "FTP");
        assert_eq!(
            help.actions().first(),
            Some(&Action::Run("plugin:acme.ftp:sync".to_owned())),
            "sus comandos son filas ejecutables"
        );
    }

    #[test]
    fn cambiar_de_plugins_olvida_las_paginas_que_ya_no_estan() {
        let mut help = HelpState::new(Lang::En, "Keyboard".to_owned());
        help.set_plugins(vec![nodo("acme.ftp")]);
        let parsed = norte_help::parse_untrusted(b"cuerpo", "acme.ftp", None);
        help.install_plugin_topic(parsed.topic);
        help.set_plugins(vec![nodo("otro.plugin")]);
        help.open(&TopicId::new("acme.ftp"));
        assert_ne!(
            help.current().as_str(),
            "acme.ftp",
            "un plugin que ya no está en el catálogo no se abre"
        );
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `just t norte-frontend`
Expected: FAIL — `PluginNode` and the four methods do not exist.

- [ ] **Step 3: Implement**

In `crates/norte-frontend/src/help.rs`:

```rust
/// Tag the plugin pages are grouped under.
const PLUGINS_TAG: &str = "extensions";

/// A plugin that can have a page in the sidebar (H3e).
///
/// The frontend builds these from `plugin.list`; this model never talks to a
/// backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginNode {
    /// Catalogue id. A LOOKUP KEY: byte-exact, never masked (masking is not
    /// injective, so it would collapse two distinct plugins onto one row).
    pub id: String,
    /// `PluginInfo.name`, ALREADY masked and capped by the caller — this
    /// model does no masking, it only carries what the frontend prepared.
    pub title: String,
    /// The plugin ships a `help.md` (`PluginInfo.has_help`). Without one there
    /// is no page and therefore no row: a node that opens nothing is a dead
    /// end the reader pays for with a keystroke.
    pub has_help: bool,
    /// Approved AND enabled. It does not decide whether the page shows — a
    /// human reads a plugin's documentation precisely to decide whether to
    /// enable it — only whether its command rows are runnable.
    pub active: bool,
}
```

Add to `HelpState`'s fields:

```rust
    /// Plugin nodes, in catalogue order, minus the ones dropped by
    /// [`HelpState::set_plugins`].
    plugins: Vec<PluginNode>,
    /// Pages already fetched, keyed by plugin id. Owned, because they are
    /// parsed at runtime — the corpus is `'static` and these are not.
    plugin_topics: BTreeMap<String, Topic>,
```

and the methods:

```rust
    /// Installs the plugin nodes the sidebar offers (H3e).
    ///
    /// Two kinds are DROPPED, and both drops are deliberate:
    ///
    /// * a plugin with no `help.md` — its node would open nothing;
    /// * a plugin whose id is also a corpus topic id (or the synthetic keys
    ///   page). `parse_untrusted` assigns the topic the host-supplied id, so a
    ///   plugin published as `copying` would otherwise sit in the same id space
    ///   as the built-in page of that name and every `[[copying]]` in the
    ///   corpus could land in third-party prose. The corpus wins; `norte
    ///   doctor` reports the collision so the loss is not silent.
    ///
    /// Any page already fetched for a plugin that is no longer in the list is
    /// forgotten with it — the catalogue is the truth about what exists.
    pub fn set_plugins(&mut self, nodes: Vec<PluginNode>) {
        self.plugins = nodes
            .into_iter()
            .filter(|n| n.has_help)
            .filter(|n| n.id != KEYS_ID && norte_help::topic(self.lang, &n.id).is_none())
            .collect();
        self.plugin_topics
            .retain(|id, _| self.plugins.iter().any(|n| &n.id == id));
        self.rebuild_rows();
    }

    /// Installs a page parsed from `plugin.help` (H3e). Ignored when the
    /// plugin is not a current node — a late answer for a plugin that has
    /// since left the catalogue must not resurrect it.
    pub fn install_plugin_topic(&mut self, topic: Topic) {
        let Origin::Plugin { id, .. } = &topic.origin else {
            return;
        };
        if !self.plugins.iter().any(|n| &n.id == id) {
            return;
        }
        self.plugin_topics.insert(id.clone(), topic);
        self.rebuild_actions();
    }

    /// The open page, whatever it is: the corpus FIRST, then a plugin's. The
    /// order is the shadowing guard's second half — even if a node slipped
    /// past [`set_plugins`], the built-in page would still win here.
    ///
    /// `None` for the synthetic keyboard page, and for a plugin node whose
    /// page has not arrived yet (see [`plugin_needs_fetch`](Self::plugin_needs_fetch)).
    #[must_use]
    pub fn current_topic(&self) -> Option<&Topic> {
        norte_help::topic(self.lang, self.current.as_str())
            .or_else(|| self.plugin_topics.get(self.current.as_str()))
    }

    /// The plugin id whose page the reader is on and the state does not have
    /// yet — what the frontend must ask `plugin.help` for. `None` when the
    /// open page is a corpus page, the keyboard page, or an already-fetched
    /// plugin page.
    #[must_use]
    pub fn plugin_needs_fetch(&self) -> Option<&str> {
        let id = self.current.as_str();
        (norte_help::topic(self.lang, id).is_none()
            && !self.plugin_topics.contains_key(id)
            && self.plugins.iter().any(|n| n.id == id))
        .then_some(id)
    }

    /// Whether the plugin owning the open page is active (approved AND
    /// enabled). `true` for anything that is not a plugin page: a corpus page
    /// has no plugin to be inactive.
    #[must_use]
    pub fn current_plugin_active(&self) -> bool {
        self.plugins
            .iter()
            .find(|n| n.id == self.current.as_str())
            .is_none_or(|n| n.active)
    }
```

Then:

- `known()` (`help.rs:499`) accepts a plugin node id as well:
  `id.as_str() == KEYS_ID || norte_help::topic(self.lang, id.as_str()).is_some() || self.plugins.iter().any(|n| n.id == id.as_str())`
- the row builder (`help.rs:539`) appends, after the corpus groups, a
  `SidebarRow::Group { tag: PLUGINS_TAG.to_owned() }` followed by one
  `SidebarRow::Topic { id: TopicId::new(&n.id), title: n.title.clone() }` per
  node — only when `self.plugins` is non-empty.
- wherever actions are recomputed from `self.topic()`, use `current_topic()` so a
  plugin page contributes its own `Run` rows. A plugin topic has no `see_also`
  (`parse_untrusted` drops it), so the links half is naturally empty.

Add `use norte_help::Origin;` and `use std::collections::BTreeMap;` as needed.
`rebuild_rows`/`rebuild_actions` stand for the private helpers that already
exist in this file under whatever names it uses — call those, do not invent new
ones.

- [ ] **Step 4: Verify and commit**

```bash
just t norte-frontend && just c norte-frontend
cargo fmt --all
git add crates/norte-frontend
git commit -m "feat(frontend): the help sidebar carries a node per plugin (H3e)"
```

---

### Task 7: availability — `plugin:` keys against the frozen snapshot

**Files:**
- Modify: `crates/norte-frontend/src/availability.rs`

`Facts` is `Copy` and must stay so (it is passed by value all over both
frontends). The plugin set therefore rides beside it rather than inside it.

- [ ] **Step 1: Write the failing tests**

In `crates/norte-frontend/src/availability.rs` tests:

```rust
    fn activos(ids: &[&str]) -> std::collections::BTreeSet<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn el_comando_de_un_plugin_apagado_se_atenua() {
        let v = verdict_with_plugins(
            "plugin:acme.ftp:sync",
            &facts_normales(),
            &activos(&["otro.plugin"]),
        );
        assert_eq!(v.reason(), Some(Reason::PluginInactive));
    }

    #[test]
    fn el_comando_de_un_plugin_activo_se_ofrece() {
        let v = verdict_with_plugins(
            "plugin:acme.ftp:sync",
            &facts_normales(),
            &activos(&["acme.ftp"]),
        );
        assert!(v.is_available());
    }

    #[test]
    fn un_comando_del_binario_no_mira_los_plugins() {
        let v = verdict_with_plugins("pane.copy", &facts_normales(), &activos(&[]));
        assert!(v.is_available(), "el prefijo `plugin:` es lo que decide");
    }

    #[test]
    fn una_clave_de_plugin_malformada_no_se_ofrece() {
        // `plugin:` sin id ni comando no identifica nada: fail-closed, porque
        // el único despacho posible sería contra un plugin que no existe.
        let v = verdict_with_plugins("plugin:", &facts_normales(), &activos(&["acme.ftp"]));
        assert_eq!(v.reason(), Some(Reason::PluginInactive));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `just t norte-frontend`
Expected: FAIL — `verdict_with_plugins` not found.

- [ ] **Step 3: Implement**

```rust
/// The plugin id inside a palette dispatch key, `plugin:{id}:{command}`.
///
/// `None` for anything that is not one of those keys. A key with the prefix
/// but no `id:command` after it returns `None` too, and the caller treats that
/// as inactive: a malformed key names no plugin, so there is nothing that
/// could run it.
#[must_use]
pub fn plugin_of_command(command: &str) -> Option<&str> {
    let rest = command.strip_prefix("plugin:")?;
    let (id, cmd) = rest.split_once(':')?;
    (!id.is_empty() && !cmd.is_empty()).then_some(id)
}

/// [`verdict`], plus the arm for plugin-contributed commands (H3e).
///
/// `active` is the set of plugin ids that are approved AND enabled — the
/// frontend's SNAPSHOT, taken when the help opened. A `plugin:` key whose
/// plugin is not in it is [`Reason::PluginInactive`]: the row stays visible,
/// because the reader is looking at that plugin's own page and "it is here but
/// switched off" is the answer they came for, and it dims because
/// `plugin.run_command` would refuse it.
///
/// Malformed `plugin:` keys are inactive rather than available: the fail-OPEN
/// default of [`verdict`] exists for commands this table does not KNOW, and a
/// key that names no plugin is not unknown, it is broken.
///
/// ```
/// use norte_frontend::availability::{verdict_with_plugins, Facts};
/// use norte_help::Reason;
///
/// let facts = Facts {
///     enterable: false,
///     viewable: false,
///     rename_single: true,
///     source_read_only: false,
///     dest_read_only: false,
///     degraded: false,
/// };
/// let activos = std::collections::BTreeSet::new();
/// assert_eq!(
///     verdict_with_plugins("plugin:acme.ftp:sync", &facts, &activos).reason(),
///     Some(Reason::PluginInactive)
/// );
/// assert!(verdict_with_plugins("app.quit", &facts, &activos).is_available());
/// ```
#[must_use]
pub fn verdict_with_plugins(
    command: &str,
    facts: &Facts,
    active: &std::collections::BTreeSet<String>,
) -> Availability {
    if command.starts_with("plugin:") {
        let ok = plugin_of_command(command).is_some_and(|id| active.contains(id));
        return gated(ok, Reason::PluginInactive);
    }
    verdict(command, facts)
}
```

- [ ] **Step 4: Verify and commit**

```bash
just t norte-frontend && just c norte-frontend
cargo fmt --all
git add crates/norte-frontend
git commit -m "feat(frontend): a plugin command dims when its plugin is off (H3e)"
```

---

### Task 8: the TUI — snapshot on open, page on demand, badge on the page

**Files:**
- Modify: `crates/norte-tui/src/help.rs` (`TuiChords`)
- Modify: `crates/norte-tui/src/help_render.rs` (badge + `into_static`)
- Modify: `crates/norte-tui/src/app.rs` (`HelpView` plumbing)
- Modify: `crates/norte-tui/src/main.rs` (snapshot, fetch, manager affordance)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`

- [ ] **Step 1: Write the failing tests**

In `crates/norte-tui/src/help.rs` tests:

```rust
    #[test]
    fn un_comando_de_plugin_apagado_llega_atenuado_a_la_pagina() {
        let r = resolver_con(facts_normales()).with_plugins(std::collections::BTreeSet::new());
        assert_eq!(
            r.availability("plugin:acme.ftp:sync").reason(),
            Some(norte_help::Reason::PluginInactive)
        );
    }

    #[test]
    fn un_comando_de_plugin_encendido_no_se_atenua() {
        let r = resolver_con(facts_normales())
            .with_plugins(["acme.ftp".to_owned()].into_iter().collect());
        assert!(r.availability("plugin:acme.ftp:sync").is_available());
    }
```

In `crates/norte-tui/src/help_render.rs` tests:

```rust
    #[test]
    fn una_pagina_de_plugin_lleva_su_insignia() {
        let parsed = norte_help::parse_untrusted(b"cuerpo", "acme.ftp", Some("ACME".to_owned()))
            .fold_flags(true, true);
        let out = render_topic(&parsed.topic, Lang::En, &Vetado, 60, &theme());
        let texto = text_of(&out.lines);
        assert!(texto.contains("ACME"), "el publicador se nombra: {texto}");
        assert!(
            texto.contains(&norte_i18n::t("help-plugin-truncated")),
            "un cuerpo cortado se declara: {texto}"
        );
        assert!(
            texto.contains(&norte_i18n::t("help-plugin-lossy")),
            "una decodificación con pérdida se declara: {texto}"
        );
    }

    #[test]
    fn una_pagina_de_plugin_hostil_sale_enmascarada() {
        let parsed = norte_help::parse_untrusted(
            "+++\ntitle = \"a\u{202E}gpj.exe\"\n+++\ncuerpo con \u{200B}truco".as_bytes(),
            "acme.ftp",
            None,
        );
        let out = render_topic(&parsed.topic, Lang::En, &Vetado, 60, &theme());
        let texto = text_of(&out.lines);
        assert!(!texto.contains('\u{202E}'), "sin override bidi: {texto:?}");
        assert!(!texto.contains('\u{200B}'), "sin invisibles: {texto:?}");
    }
```

(`Vetado`, `theme()` and `text_of` are the helpers the module's H3d tests already
use.)

- [ ] **Step 2: Run to verify failure**

Run: `just t norte-tui`
Expected: FAIL — `with_plugins` missing; no badge line.

- [ ] **Step 3: Implement**

`crates/norte-tui/src/help.rs` — `TuiChords` gains

```rust
    /// Plugin ids that are approved AND enabled, snapshotted when the overlay
    /// opened (H3e). Empty means "nothing active" and dims every plugin row,
    /// which is the right answer both when there are no plugins and when the
    /// snapshot could not be taken: offering a row `plugin.run_command` would
    /// refuse is the worse mistake.
    active_plugins: std::collections::BTreeSet<String>,
```

with a builder mirroring `with_facts`:

```rust
    /// The same resolver, carrying the plugin snapshot (H3e).
    #[must_use]
    pub fn with_plugins(&self, active: std::collections::BTreeSet<String>) -> Self {
        let mut out = self.clone();
        out.active_plugins = active;
        out
    }
```

and `availability` delegating:

```rust
    fn availability(&self, command: &str) -> Availability {
        match self.facts {
            Some(facts) => norte_frontend::availability::verdict_with_plugins(
                command,
                &facts,
                &self.active_plugins,
            ),
            None => Availability::Available,
        }
    }
```

(Keep whatever shape the existing `availability` has for the no-facts case — the
point is only that it now routes through `verdict_with_plugins`.)

`crates/norte-tui/src/help_render.rs` — in `render_topic`, right after the title
and its rule, when `topic.origin` is a plugin:

```rust
    if let Origin::Plugin {
        publisher,
        truncated,
        lossy,
        ..
    } = &topic.origin
    {
        let mut marcas: Vec<String> = Vec::new();
        if let Some(p) = publisher {
            marcas.push(p.clone());
        }
        if *truncated {
            marcas.push(t("help-plugin-truncated"));
        }
        if *lossy {
            marcas.push(t("help-plugin-lossy"));
        }
        if !marcas.is_empty() {
            lines.push(Line::from(Span::styled(
                marcas.join(" · "),
                theme.role(Role::Hint),
            )));
        }
    }
```

and, for the TUI's `'static` body slot, a converter:

```rust
/// Detaches a rendering from the topic it borrowed.
///
/// `render_topic` borrows the topic's strings, which is free for the corpus
/// (`'static`) and impossible for a plugin page: that one is OWNED by
/// `HelpState`, and a `HelpView` holding both the state and a rendering
/// borrowed from it would be a self-referential struct. Cloning the visible
/// page's spans is the cost of not building one — it happens once per layout,
/// and only for plugin pages.
#[must_use]
pub fn into_static(r: Rendered<'_>) -> Rendered<'static> {
    Rendered {
        lines: r
            .lines
            .into_iter()
            .map(|l| {
                Line::from(
                    l.spans
                        .into_iter()
                        .map(|s| Span::styled(s.content.into_owned(), s.style))
                        .collect::<Vec<_>>(),
                )
                .alignment_or_default(l.alignment)
            })
            .collect(),
        action_lines: r.action_lines,
    }
}
```

(`alignment_or_default` is shorthand: preserve `l.alignment` and `l.style` with
whatever ratatui version this repo pins — check `Line`'s fields and carry them
all, do not drop styling on the floor.)

`crates/norte-tui/src/app.rs` — `HelpView::refresh` picks the topic from
`state.current_topic()` instead of `state.topic()`, and passes the result through
`into_static` when the topic is not from the corpus:

```rust
        let rendered = match self.state.current_topic() {
            Some(topic) => {
                let out = crate::help_render::render_topic(topic, lang, chords, width, theme);
                crate::help_render::into_static(out)
            }
            None => /* the synthetic keys page, unchanged */,
        };
```

`crates/norte-tui/src/main.rs`:

1. `open_contextual_help` takes the snapshot as a parameter and stays SYNC (its
   callers are async and its tests are not):

```rust
fn open_contextual_help(
    app: &mut App,
    lang: norte_help::Lang,
    help_lines: &[String],
    plugins: Option<&norte_proto::methods::PluginListResult>,
) {
```

   and after building `app.help`:

```rust
    // H3e: el estado de los plugins se CONGELA con el resto de los hechos. Es
    // una foto: nace al abrir la ayuda y muere con ella, así que no hay caché
    // que invalidar en `App` ni una ida y vuelta al daemon mientras se pinta.
    if let (Some(list), Some(help)) = (plugins, app.help.as_mut()) {
        let activos: std::collections::BTreeSet<String> = list
            .plugins
            .iter()
            .filter(|p| p.approved && p.enabled)
            .map(|p| p.id.clone())
            .collect();
        help.state.set_plugins(
            list.plugins
                .iter()
                .map(|p| norte_frontend::help::PluginNode {
                    id: p.id.clone(),
                    // Texto de TERCEROS: enmascarado y acotado AQUÍ, en el
                    // punto de entrada, igual que hace la paleta con el título
                    // de un comando de plugin.
                    title: norte_frontend::display_name(p.name.as_bytes()).0,
                    has_help: p.has_help,
                    active: activos.contains(&p.id),
                })
                .collect(),
        );
        help.chords = help.chords.with_plugins(activos);
    }
```

   (Adapt to how `HelpView` holds its resolver — if the resolver lives in `App`
   rather than in `HelpView`, set it there; the requirement is that the resolver
   the renderer uses carries the snapshot.)

2. The `Command::AppHelp` dispatch arm takes the snapshot first:

```rust
        Command::AppHelp => {
            // Una sola llamada, en el camino de abrir — nunca al pintar.
            let plugins = backend.plugins_list().await.ok();
            open_contextual_help(app, lang, &help_lines, plugins.as_ref());
        }
```

3. After any key handled by the help overlay, fetch a page that is missing:

```rust
/// Fetches the page of the plugin node the reader just opened (H3e).
///
/// On demand and once: 64 KiB per plugin must not ride every `plugin.list`, and
/// a page already installed is never asked for again within one overlay.
///
/// A failure is SILENT on purpose — an empty page with the plugin's name is a
/// better answer than an error toast over a help overlay, and a daemon N-1
/// without the handler lands here too.
async fn fetch_plugin_page(backend: &Backend, app: &mut App) {
    let Some(help) = app.help.as_mut() else {
        return;
    };
    let Some(id) = help.state.plugin_needs_fetch().map(str::to_owned) else {
        return;
    };
    let Ok(res) = backend.plugin_help(&id).await else {
        return;
    };
    let Some(help) = app.help.as_mut() else {
        return; // la ayuda se cerró mientras se pedía
    };
    let parsed = norte_help::parse_untrusted(res.markdown.as_bytes(), &id, None)
        // Las banderas las sabe el EMISOR: el texto llega ya corto y ya
        // decodificado, así que este parseo saldría limpio y la insignia se
        // apagaría en silencio.
        .fold_flags(res.truncated, res.lossy);
    help.state.install_plugin_topic(parsed.topic);
}
```

   called right after the help key handling in the run loop, next to where
   `refresh_help` is already invoked.

4. Extension manager affordance: in the manager's key handling, `F1` (or the
   `app.help` chord) on a plugin row with `has_help` opens the overlay directly
   on that plugin's page, using the list the manager already holds:

```rust
            // H3e: la ayuda del plugin bajo el cursor, desde el gestor.
            if let Some(p) = app.extensions.as_ref().and_then(ExtensionsView::selected)
                && p.has_help
            {
                let id = p.id.clone();
                let plugins = backend.plugins_list().await.ok();
                open_contextual_help(app, lang, &help_lines, plugins.as_ref());
                if let Some(help) = app.help.as_mut() {
                    help.state.open(&norte_help::TopicId::new(&id));
                }
                fetch_plugin_page(&backend, app).await;
            }
```

   (`ExtensionsView` / `app.extensions` stand for the manager's real field and
   type — `app.rs:1126` onwards.)

Fluent strings, `crates/norte-i18n/i18n/en.ftl`:

```
help-group-extensions = Extensions
help-plugin-truncated = cut short
help-plugin-lossy = some bytes did not decode
```

`crates/norte-i18n/i18n/es.ftl`:

```
help-group-extensions = Extensiones
help-plugin-truncated = recortada
help-plugin-lossy = hay bytes que no decodifican
```

- [ ] **Step 4: Verify**

```bash
just t norte-tui && just c norte-tui && just c norte-frontend
```
Expected: PASS. Snapshot tests of the overlay may need re-approval — inspect
each diff before accepting it; a changed line in a page with no plugins is a
regression, not a snapshot to bless.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/norte-tui crates/norte-i18n
git commit -m "feat(tui): a plugin's own page in the help overlay (H3e)"
```

---

### Task 9: drive it, write it down, gate it

- [ ] **Step 1: Drive the real app**

Build a plugin with help into a scratch config dir and drive the TUI:

```bash
cargo build -q -p norte-tui
export NORTE_CONFIG_DIR=/tmp/claude-1000/h3e-config   # if the binary honours it;
# otherwise copy the demo plugin into ~/.config/norte/plugins/ and remove it after
mkdir -p "$NORTE_CONFIG_DIR/plugins/org.norte.demo"
cp crates/norte-plugin-host/examples/command-demo/plugin.toml \
   "$NORTE_CONFIG_DIR/plugins/org.norte.demo/" 2>/dev/null || true
printf '+++\ntitle = "Demo"\ncommands = ["plugin:org.norte.demo:greet"]\n+++\nThis plugin greets you.\n' \
  > "$NORTE_CONFIG_DIR/plugins/org.norte.demo/help.md"
tmux kill-session -t h3e 2>/dev/null
tmux new-session -d -s h3e -x 113 -y 30 -c "$PWD" "NORTE_LANG=es target/debug/norte-tui"
sleep 2
tmux send-keys -t h3e F1
sleep 1
tmux capture-pane -t h3e -p
```

(Find the demo plugin's real path first — `ls crates/norte-plugin-host/examples`
or wherever the P5 guests live. If the manifest id differs, use that id for the
directory and for the `{{cmd:}}` prefix.)

Check three things and paste the captures: the **Extensions** group appears with
the plugin's node; opening it shows the plugin's own prose; its command row is
DIM with `reason-plugin-inactive` while the plugin is unapproved, and stops
being dim after approving and enabling it in the extension manager.

Then the hostile pass: rewrite `help.md` with a bidi override in the title and a
`{{cmd:fs.copy}}` in the body, reopen, and confirm the title is masked and the
foreign mark renders as literal text — never as a runnable row.

- [ ] **Step 2: Changelog**

Under `## [Unreleased]` → `### Added`, in the file's voice: a plugin can ship a
`help.md` and it becomes a page in the help overlay, with its own commands as
rows that dim when the plugin is off; the host bounds the file at the same cap
the hostile parser uses and says so with a badge when it had to cut or replace
bytes; `norte doctor` reports a help file that is oversized, undecodable, claims
a command the plugin does not own, or takes the id of a built-in page.

Say plainly what is NOT done: the palette still hides inactive plugin commands
rather than dimming them (the fast gesture stays fast); a `{{cmd:}}` in the BODY
naming a foreign command degrades to literal text without a doctor finding; and
`PluginInfo.settings` from the design was dropped as obsolete — 0.28.0 already
put `[config]` on the wire with its schema.

- [ ] **Step 3: Amend the design doc**

Append to `docs/superpowers/specs/2026-08-04-help-system-redesign-design.md`:

```markdown
## Amendment 2026-08-06 (H3e): what the bump actually carries

`PluginInfo.settings` is dropped. The design proposed it to close the P2 display
debt "with whichever future change bumps `PROTOCOL_VERSION`"; that debt was
closed first, by 0.28.0 (G3c), which shipped `plugin.get_config`/`set_config` —
the schema plus the effective value, not a flattened display map. Carrying
`settings` now would be a second, weaker way to read the same thing.

The plugin state H3d deferred is a SNAPSHOT taken when the overlay opens, not a
cache in `App`: one `plugin.list` on the F1 path, frozen beside the capability
and connection facts, dead when the overlay closes. The palette keeps filtering
inactive plugin commands out entirely — `Reason::PluginInactive` surfaces on the
plugin's own page, where a reader is asking about that plugin.
```

- [ ] **Step 4: Full gate**

```bash
just ci > /tmp/ci-h3e.log 2>&1; echo "EXIT=$status"; grep -E "^error|Summary" /tmp/ci-h3e.log | tail -4
just gui-ci 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"
```

Coverage sits near the 85% gate (last release check: 85.26%) and this change
touches `norte-proto` and `norte-core`, both under it. If `cov` fails, the
missing tests are host-side: `help_of`'s error paths.

- [ ] **Step 5: Reviewers**

Three, and none is optional:

- `protocol-guardian` — already run in Task 1; re-run over the FULL diff, because
  Task 4 added the handler the bump only described.
- `security-reviewer` — ask specifically: (a) can a `plugin.help` id escape the
  catalogue lookup and reach the filesystem; (b) is opening the method to agents
  right, given it returns third-party text an agent could otherwise not read;
  (c) does the blocking read under the plugins lock create a stall a hostile
  `help.md` on a slow mount could exploit.
- `encoding-auditor` — ask specifically: (a) does the round trip
  `sanitize_untrusted` → wire → `parse_untrusted` preserve the masking guarantee
  for every string in the model; (b) does `fold_flags` at the receiving side
  actually reach the badge; (c) is the plugin `name` masked before it becomes a
  sidebar title, and is the id left byte-exact.

- [ ] **Step 6: Commit**

```bash
git add CHANGELOG.md docs/superpowers/specs
git commit -m "docs(changelog,spec): plugin help on the wire (H3e)"
```

---

## Self-review notes

- **Spec coverage.** `has_help`: Tasks 1, 3. `plugin.help`: Tasks 1, 3, 4.
  Registry `help.md`: Task 3. Doctor: Task 5. Extension manager: Task 8 step 3.4.
  Digest exclusion: NOTHING to do — `help.md` is not part of `Manifest`, and
  `approval_digest` hashes manifest fields only, so editing help cannot reset an
  approval by construction. Task 3's tests should include a pin for that; add
  `un_help_md_editado_no_mueve_el_digest` if the plugin-host suite does not
  already assert the digest inputs exhaustively.
- **`PluginInfo.settings`: dropped, with reasons, in the plan header and in the
  design amendment.** Not an oversight.
- **Body-level foreign `{{cmd:}}` is not reported by doctor.** Stated in the
  rustdoc of `foreign_commands` and `check_plugin_help`, and in the changelog.
- **Known risk 1: the `Rendered<'static>` seam.** `HelpView` holds a rendering
  and now the state holds the topic it borrows. `into_static` (Task 8) is the
  escape; if it turns out ratatui's `Line`/`Span` carry fields this plan does not
  copy, the rendering loses styling silently. The badge test only checks text —
  add a styling assertion if the conversion is written by hand.
- **Known risk 2: the snapshot can go stale within one overlay.** A human who
  approves a plugin in the extension manager, then opens the help WITHOUT
  closing it in between, sees the old verdict. The manager path in Task 8 step
  3.4 re-fetches, so the common route is covered; the residual case is a second
  session or the daemon changing state underneath, which the frozen-facts
  decision already accepts everywhere else.
- **Known risk 3: `just ci` does not compile the GUI.** Task 6 and 7 change
  `norte-frontend`, which the GUI consumes. `just gui-ci` in Task 9 is not
  optional.
