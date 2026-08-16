# Git status as the official columns plugin — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `org.norte.git-status`, a real columns plugin, by growing the
plugin interface until one can exist — which today it cannot.

**Architecture:** A new WIT package `norte:location@0.1.0` gives an approved
guest `read`/`stat`/`list` **relative to an opaque token** the host mints for
the directory being listed; the guest never learns the path. Confinement is the
kernel's (`openat2(RESOLVE_BENEATH)`), exposed as a read-only API from
`norte-vfs-local` — the only crate allowed to touch `std::fs` (hard rule 2). The
host pools instances instead of building one per page; freshness stays the
guest's problem, so the core never learns what a git repository is.

**Tech stack:** Rust, wasmtime + WIT component model, `wasm32-wasip2` guest,
nextest.

**Design:** `docs/superpowers/specs/2026-08-16-git-status-columns-design.md`.

**Order:** this plan runs **after** `2026-08-16-rar-by-delegation.md`.

---

## Gate budget (CLAUDE.md)

`just t <crate>` during RED→GREEN. **One** `just ci-fast` after Task 5. **One**
`just ci` before the merge. Reviewers never compile.

Two blind spots this plan will hit: `just t` does not run doctests and `just c`
does not check intra-doc links. Every task here touches documented public items,
so after each: `cargo test -p <crate> --doc` and, when a doc link is written,
`cargo doc -p <crate> --no-deps`. Seconds each.

## File structure

| file | responsibility |
| --- | --- |
| `crates/norte-vfs-local/src/location.rs` | `ConfinedRoot`: bounded read/stat/list beneath a directory |
| `crates/norte-plugin-host/wit/deps/location/location.wit` | the new `norte:location@0.1.0` package |
| `crates/norte-plugin-host/wit/norte-plugin.wit` | `column-values` gains the token; package → 0.8.0 |
| `crates/norte-plugin-host/src/capability.rs` | the `location` capability + its digest byte |
| `crates/norte-plugin-host/src/runtime.rs` | host impl of `location`, backed by an injected trait |
| `crates/norte-core/src/plugins.rs` | token minting, bounds, instance pool — **both** call sites |
| `plugins/git-status/` | the guest: index parser, ignore matcher, cell rendering |

---

### Task 1: A confined, read-only root in `norte-vfs-local`

The location capability cannot live anywhere else: hard rule 2 says only this
crate touches `std::fs`, and `LocalRoot` (the `openat2(RESOLVE_BENEATH)` opener
that closed #164) is `pub(crate)` today
(`crates/norte-vfs-local/src/confined.rs:60`).

**Files:**
- Create: `crates/norte-vfs-local/src/location.rs`
- Modify: `crates/norte-vfs-local/src/lib.rs` (export it)

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn un_dotdot_no_sale_de_la_raiz() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dentro.txt"), b"si").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let root = ConfinedRoot::open(dir.path(), Bounds::default()).unwrap();
    assert!(root.read(b"sub/../dentro.txt").is_ok(), "un `..` INTERIOR es legítimo");
    assert!(root.read(b"../fuera.txt").is_err(), "salir, no");
}

#[test]
fn un_symlink_que_apunta_fuera_lo_RECHAZA_EL_KERNEL() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secreto"), b"nope").unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path().join("secreto"), dir.path().join("escape")).unwrap();
    let root = ConfinedRoot::open(dir.path(), Bounds::default()).unwrap();
    assert!(root.read(b"escape").is_err(), "un symlink no es una puerta trasera");
}

#[test]
fn una_ruta_absoluta_no_es_relativa() {
    let dir = tempfile::tempdir().unwrap();
    let root = ConfinedRoot::open(dir.path(), Bounds::default()).unwrap();
    assert!(root.read(b"/etc/passwd").is_err());
}

#[test]
fn cada_tope_corta_en_su_borde() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("grande.bin"), vec![0u8; 4096]).unwrap();
    let bounds = Bounds { max_read_bytes: 1024, max_calls: 2, max_total_bytes: 2048, max_list_entries: 8 };
    let root = ConfinedRoot::open(dir.path(), bounds).unwrap();
    assert!(matches!(root.read(b"grande.bin"), Err(LocationError::TooLarge)));
    // El presupuesto de LLAMADAS se consume aunque la lectura falle: si no,
    // un guest sondea el árbol gratis fallando a propósito.
    root.stat(b"grande.bin").ok();
    assert!(matches!(root.stat(b"grande.bin"), Err(LocationError::Budget)));
}

#[test]
fn stat_trae_lo_que_git_guarda_en_su_indice() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f"), b"x").unwrap();
    let root = ConfinedRoot::open(dir.path(), Bounds::default()).unwrap();
    let m = root.stat(b"f").unwrap();
    assert_eq!(m.size, 1);
    assert!(m.ino != 0 && m.dev != 0, "git compara ino/dev, no solo mtime");
}
```

- [ ] **Step 2: Run and watch them fail.** Run: `just t norte-vfs-local`

- [ ] **Step 3: Implement**

```rust
/// Topes de una sesión de ubicación. Todos fail-closed.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub max_read_bytes: u64, pub max_calls: u32,
    pub max_total_bytes: u64, pub max_list_entries: u32,
}

/// Lo que `stat` devuelve: exactamente los campos que el índice de git guarda.
pub struct LocationMeta {
    pub kind: LocationKind, pub size: u64,
    pub mtime_sec: i64, pub mtime_nsec: u32,
    pub ctime_sec: i64, pub ctime_nsec: u32,
    pub ino: u64, pub dev: u64, pub mode: u32,
}

/// Lectura acotada BAJO un directorio. La confinación es del kernel
/// (`openat2(RESOLVE_BENEATH)`), no un chequeo de rutas.
pub struct ConfinedRoot { /* LocalRoot + Bounds + presupuesto consumido */ }
impl ConfinedRoot {
    pub fn open(dir: &Path, bounds: Bounds) -> Result<Self, LocationError>;
    pub fn read(&self, rel: &[u8]) -> Result<Vec<u8>, LocationError>;
    pub fn stat(&self, rel: &[u8]) -> Result<LocationMeta, LocationError>;
    pub fn list(&self, rel: &[u8]) -> Result<Vec<LocationDirent>, LocationError>;
}
```

Reuse `LocalRoot` rather than opening a second path to the same problem; widen
its visibility to `pub(crate)`-plus-this-module, not to the world.

- [ ] **Step 4: Green.** Run: `just t norte-vfs-local` then `cargo test -p norte-vfs-local --doc`

- [ ] **Step 5: Commit**

```bash
git add crates/norte-vfs-local
git commit -m "feat(vfs-local): a root a guest can read under and cannot leave"
```

---

### Task 2: The WIT, and the bump that breaks every compiled guest

**Files:**
- Create: `crates/norte-plugin-host/wit/deps/location/location.wit`
- Modify: `crates/norte-plugin-host/wit/norte-plugin.wit` (package 0.7.0 → 0.8.0)
- Modify: every guest under `crates/norte-plugin-host/examples-wasm/`
- Modify: `crates/norte-core/resources/ftp-provider.wasm` via `just build-ftp-wasm`

Read the header comment of `norte-plugin.wit` first: it records, twice and
empirically, that any package bump invalidates every `.wasm` compiled against
it — by the **import** side, not only the export side. Every in-tree guest is
recompiled **in this same commit**, as the two previous bumps did.

- [ ] **Step 1: Write the WIT**

```wit
package norte:location@0.1.0;

interface location {
    enum entry-kind { file, dir, symlink, other }
    record meta {
        kind: entry-kind, size: u64,
        mtime-sec: s64, mtime-nsec: u32,
        ctime-sec: s64, ctime-nsec: u32,
        ino: u64, dev: u64, mode: u32,
    }
    record dirent { name: list<u8>, kind: entry-kind }

    read:  func(token: string, rel: list<u8>) -> result<list<u8>, string>;
    stat:  func(token: string, rel: list<u8>) -> result<meta, string>;
    list:  func(token: string, rel: list<u8>) -> result<list<dirent>, string>;
}
```

`rel` is `list<u8>` and not `string`: hard rule 1 does not stop applying because
the path crosses an ABI.

- [ ] **Step 2: Change the columns interface and the world**

```wit
interface columns {
    column-values: func(id: string, location: option<string>,
                        entries: list<list<u8>>) -> list<option<string>>;
}

world norte-columns {
    import norte:host/host-log@0.1.0;
    import norte:host/host-config@0.1.0;
    import norte:location/location@0.1.0;
    export columns;
}
```

Package `norte:plugin@0.7.0` → `@0.8.0`, with a header comment entry in the same
voice as the previous four, saying what broke and why it was worth it.

- [ ] **Step 3: Recompile every in-tree guest and run the wasm e2e tests**

Run: `just t norte-plugin-host`
Expected: the existing `*_wasm_real` tests pass against freshly built guests. A
failure mentioning `a matching implementation was not found in the linker` means
a guest was not rebuilt — that is the exact symptom the header comment predicts.

- [ ] **Step 4: Commit**

```bash
git add crates/norte-plugin-host/wit crates/norte-plugin-host/examples-wasm crates/norte-core/resources
git commit -m "feat(plugin-host): a columns guest may be told where it is, without being told where it is"
```

---

### Task 3: The capability, and the host side of the interface

**Files:**
- Modify: `crates/norte-plugin-host/src/capability.rs` (the `Capabilities` struct
  and its digest)
- Modify: `crates/norte-plugin-host/src/manifest.rs` (parse + approval digest)
- Modify: `crates/norte-plugin-host/src/runtime.rs` (host functions)

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn location_declarada_entra_en_el_digest_de_aprobacion() {
    let sin = Manifest::from_toml(MANIFEST_COLUMNS).unwrap();
    let con = Manifest::from_toml(&MANIFEST_COLUMNS.replace(
        "[capabilities]", "[capabilities]\nlocation = \"read\"")).unwrap();
    assert_ne!(sin.approval_digest(), con.approval_digest(),
        "pedir una capacidad nueva EXIGE aprobarla de nuevo");
}

#[test]
fn un_valor_desconocido_de_location_es_ERROR_de_manifiesto() {
    let m = MANIFEST_COLUMNS.replace("[capabilities]", "[capabilities]\nlocation = \"write\"");
    assert!(Manifest::from_toml(&m).is_err(), "vocabulario CERRADO, como `exec`");
}

#[tokio::test]
async fn sin_la_capability_el_host_NIEGA_antes_de_tocar_nada() {
    // Mismo criterio que `read-scoped` (ADR 0022 D4): el enforcement vive en
    // el HOST, y ni siquiera se mira el token.
    let host = TestLocationHost::spy();
    let mut inst = runtime.instantiate_columns_with_location(
        &wasm, Capabilities::default() /* sin location */, host.clone()).unwrap();
    let out = inst.column_values("status", Some("tok"), &[b"a.txt".to_vec()]).unwrap();
    assert_eq!(out, vec![None]);
    assert_eq!(host.calls(), 0, "el host no resolvió ni un byte");
}
```

- [ ] **Step 2: Run and watch them fail.** Run: `just t norte-plugin-host`

- [ ] **Step 3: Implement**

```rust
/// Acceso de ubicación: nada, o lectura bajo el token que el host entrega.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocationCap { #[default] None, Read }
impl LocationCap { fn digest_tag(self) -> u8 { match self { Self::None => 0, Self::Read => 1 } } }
```

The host functions do **not** reach the filesystem themselves — `norte-plugin-host`
must not depend on `norte-vfs-local` (that dependency direction would put the
filesystem inside the sandbox crate). They call an injected trait:

```rust
/// Lo que el HOST consumidor (norte-core) sabe hacer con un token.
pub trait LocationHost: Send + Sync {
    fn read(&self, token: &str, rel: &[u8]) -> Result<Vec<u8>, String>;
    fn stat(&self, token: &str, rel: &[u8]) -> Result<LocationMetaWire, String>;
    fn list(&self, token: &str, rel: &[u8]) -> Result<Vec<LocationDirentWire>, String>;
}
```

Gate every one of them on `caps.location == LocationCap::Read` **before**
consulting the trait, the way `read_scoped` already gates on `fs_read`
(`runtime.rs:254`).

- [ ] **Step 4: Green.** Run: `just t norte-plugin-host`, then `cargo test -p norte-plugin-host --doc`

- [ ] **Step 5: Commit**

```bash
git add crates/norte-plugin-host
git commit -m "feat(plugin-host): a capability that has to be approved to exist"
```

---

### Task 4: Minting, pooling, and the two call sites

**Files:**
- Modify: `crates/norte-core/src/plugins.rs` (the shared implementation)
- Modify: `crates/norte-core/src/daemon/server.rs:2902-2950` (the daemon handler)
- Modify: `crates/norte-core/src/backend.rs:1984-2040` (the embedded backend)

A capability enforced on one path and not the other is the failure this
repository has already written down three times (#165, #201, #181). Minting,
bounds and pooling live in `plugins.rs`; both handlers call it.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn el_token_muere_con_la_llamada() {
    let mint = LocationMint::new(Bounds::default());
    let token = mint.mint_for(&dir_vpath).unwrap();
    drop(mint.session(&token));            // la llamada termina
    assert!(mint.resolve(&token).is_err(), "un token de la página anterior está muerto");
}

#[tokio::test]
async fn una_ubicacion_que_no_es_file_no_acuna_token() {
    let mint = LocationMint::new(Bounds::default());
    let remote = VPath::parse("sftp://host/dir").unwrap();
    assert!(mint.mint_for(&remote).is_none(), "sin ruta local no hay token");
}

#[tokio::test]
async fn el_directorio_de_estado_sigue_sin_leerse_por_aqui() {
    // ADR 0052: una raíz protegida no se rodea porque la pida un plugin.
    let mint = LocationMint::with_protected(state_dir.clone(), Bounds::default());
    assert!(mint.mint_for(&state_dir_vpath).is_none());
}

#[tokio::test]
async fn la_instancia_se_REUSA_entre_paginas() {
    let pool = ColumnsPool::new(PoolLimits::default());
    let a = pool.get(&plugin_id, &location).await.unwrap();
    let b = pool.get(&plugin_id, &location).await.unwrap();
    assert_eq!(a.generation(), b.generation(), "una instancia por (plugin, ubicación)");
}

#[tokio::test]
async fn pasado_el_deadline_la_pagina_sale_VACIA_y_no_como_error() {
    let values = column_values_with_deadline(slow_guest(), Duration::from_millis(1), 3).await;
    assert_eq!(values, vec![None, None, None], "fail-closed, igual que hoy");
}
```

- [ ] **Step 2: Run and watch them fail.** Run: `just t norte-core`

- [ ] **Step 3: Implement** — `LocationMint` (opaque random tokens, a live map,
per-call sessions holding a `ConfinedRoot`), `ColumnsPool` (LRU + TTL keyed by
`(plugin_id, location)`), and a columns-category epoch deadline read from
config. Both handlers stop calling `instantiate_columns` directly and go through
the pool. The location root is subject to `read_gate_all` and to protected roots
exactly as any other path.

- [ ] **Step 4: Green.** Run: `just t norte-core`

- [ ] **Step 5: Dispatch `security-reviewer`** before committing. Give it the
commit range and these questions: can a token outlive its call; can two
concurrent pages cross tokens; does the pool leak a `ConfinedRoot` past the
session; is the budget consumed on failures as well as successes.

- [ ] **Step 6: Apply findings, then commit**

```bash
git diff --cached --stat
git add crates/norte-core
git commit -m "feat(core): a token minted per call, and one instance per location"
```

- [ ] **Step 7: One `just ci-fast`.** Expected: green.

---

### Task 5: The badge on the wire

**Files:**
- Modify: `crates/norte-proto/src/methods.rs` (`PluginInfo`), version history, goldens
- Modify: the extension manager views in `crates/norte-tui` and `crates/norte-gui`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn plugin_info_declara_la_capacidad_de_ubicacion() {
    let json = r#"{"id":"org.norte.git-status","location":"read", ...}"#;
    let info: PluginInfo = serde_json::from_str(json).unwrap();
    assert_eq!(info.location.as_deref(), Some("read"));
    // Ausente = cliente/host viejo, y eso es un plugin sin la capacidad.
    let old: PluginInfo = serde_json::from_str(r#"{"id":"x", ...}"#).unwrap();
    assert_eq!(old.location, None);
}
```

- [ ] **Step 2: Run and watch it fail.** Run: `just t norte-proto`

- [ ] **Step 3: Implement** — additive field with `skip_serializing_if`, minor
bump, goldens regenerated, version-history comment written in the file's voice.
Both frontends paint the badge next to the existing capability badges.

- [ ] **Step 4: Green.** Run: `just t norte-proto`, `just t norte-tui`, `just gui-ci`

- [ ] **Step 5: Dispatch `protocol-guardian`** — mandatory for a `norte-proto`
change. Ask specifically about the N-1 window in both directions.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-proto crates/norte-tui crates/norte-gui
git commit -m "feat(proto,tui,gui): the badge for a plugin that can read where you are"
```

---

### Task 6: The guest — the index

**Files:**
- Create: `plugins/git-status/` (a crate **outside** the workspace, like
  `norte-gui`; add it to `.gitignore`'s exceptions and to the justfile)
- Create: `plugins/git-status/src/index.rs`

- [ ] **Step 1: Write the failing tests** (these run as ordinary host-side unit
tests of the parsing code — no wasm needed)

```rust
/// Un índice v2 mínimo, construido a mano: cabecera `DIRC`, versión 2, una
/// entrada. Los offsets están en la documentación del formato y este test es
/// la única fuente de verdad que el parser necesita.
fn index_v2_con(entradas: &[(&[u8], u64)]) -> Vec<u8> { /* helper del test */ }

#[test]
fn parsea_v2_y_conserva_los_bytes_del_nombre() {
    let raw = index_v2_con(&[(b"src/lib.rs", 10), (b"cp437-\xa4\xa5.txt", 3)]);
    let idx = GitIndex::parse(&raw).unwrap();
    assert_eq!(idx.len(), 2);
    assert_eq!(idx.entry(1).unwrap().path, b"cp437-\xa4\xa5.txt");
}

#[test]
fn la_version_4_se_RECHAZA_por_su_nombre() {
    let mut raw = index_v2_con(&[(b"a", 1)]);
    raw[7] = 4;
    assert!(matches!(GitIndex::parse(&raw), Err(IndexError::UnsupportedVersion(4))),
        "v4 comprime prefijos de ruta; decirlo es mejor que leer basura");
}

#[test]
fn el_indice_esta_ORDENADO_y_eso_es_lo_que_hace_barato_el_prefijo() {
    let raw = index_v2_con(&[(b"a/b.txt", 1), (b"a/c.txt", 1), (b"z.txt", 1)]);
    let idx = GitIndex::parse(&raw).unwrap();
    assert_eq!(idx.under_prefix(b"a/").count(), 2);
}
```

- [ ] **Step 2: Run and watch them fail.** Run: `cargo test -p git-status` (this
crate is outside the workspace, so it has its own `just` recipe — add
`just plugin-git-ci` alongside `gui-ci` and use nextest there too)

- [ ] **Step 3: Implement** `GitIndex::parse` (versions 2 and 3; the extension
blocks after the entries are skipped by length), `entry`, `under_prefix`.

- [ ] **Step 4: Green.**

- [ ] **Step 5: Commit**

```bash
git add plugins/git-status justfile
git commit -m "feat(git-status): read .git/index, versions 2 and 3, and say so about 4"
```

---

### Task 7: The guest — status, ignores and aggregation

**Files:**
- Create: `plugins/git-status/src/status.rs`, `plugins/git-status/src/ignore.rs`
- Create: `plugins/git-status/src/lib.rs` (the `columns` export)

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn stat_igual_al_indice_es_LIMPIO_y_no_lee_el_fichero() {
    let fs = FakeLocation::new().with(b"a.txt", b"hola", stat_igual_al_indice());
    let cells = status_for(&idx, &fs, &[b"a.txt".to_vec()]);
    assert_eq!(cells, vec![None]);           // celda vacía = limpio
    assert_eq!(fs.reads(), 0, "el stat basta: no se lee el contenido");
}

#[test]
fn mtime_igual_pero_tamano_distinto_es_MODIFICADO() {
    let fs = FakeLocation::new().with(b"a.txt", b"holaaa", stat_con_size(6));
    assert_eq!(status_for(&idx, &fs, &[b"a.txt".to_vec()]), vec![Some("M".into())]);
}

#[test]
fn el_caso_racy_lee_y_compara_sha1() {
    // Mismo mtime que el índice y mismo tamaño: el stat NO decide. Git hace
    // exactamente esto, y por eso `read` existe en la interfaz.
    let fs = FakeLocation::new().with(b"a.txt", b"otro", stat_racy());
    assert_eq!(status_for(&idx, &fs, &[b"a.txt".to_vec()]), vec![Some("M".into())]);
    assert_eq!(fs.reads(), 1);
}

#[test]
fn lo_no_rastreado_es_interrogante_y_lo_ignorado_es_cierre_de_admiracion() {
    let fs = FakeLocation::new()
        .with_ignore(b".gitignore", b"target/\n*.tmp\n")
        .with_dir(b"target").with(b"nuevo.rs", b"", stat_cualquiera())
        .with(b"basura.tmp", b"", stat_cualquiera());
    let cells = status_for(&idx, &fs, &[b"target".to_vec(), b"nuevo.rs".to_vec(), b"basura.tmp".to_vec()]);
    assert_eq!(cells, vec![Some("!".into()), Some("?".into()), Some("!".into())]);
}

#[test]
fn un_directorio_agrega_lo_mas_fuerte_que_hay_debajo() {
    // El índice está ordenado, así que esto es un barrido de prefijo.
    let cells = status_for(&idx_con_modificado_en(b"src/deep/x.rs"), &fs, &[b"src".to_vec()]);
    assert_eq!(cells, vec![Some("M".into())]);
}

#[test]
fn sin_repo_TODAS_las_celdas_son_none() {
    assert_eq!(status_for_no_repo(&[b"a".to_vec(), b"b".to_vec()]), vec![None, None]);
}
```

- [ ] **Step 2: Run and watch them fail.** Run: `just plugin-git-ci`

- [ ] **Step 3: Implement** — repository discovery by walking up with `stat`
(following a `.git` file's `gitdir:` once), the stat comparison, the bounded
SHA-1 fallback, a `.gitignore` matcher covering the directory's file, its
parents up to the repository root, and `.git/info/exclude`, and prefix
aggregation. The guest caches its parsed index and re-`stat`s `.git/index` to
decide freshness — the host does not know what any of this means.

- [ ] **Step 4: Green.** Run: `just plugin-git-ci`

- [ ] **Step 5: Commit**

```bash
git add plugins/git-status
git commit -m "feat(git-status): worktree against index, with the ignores that make it readable"
```

---

### Task 8: Installed like a stranger's plugin, and measured under load

**Files:**
- Modify: `justfile` (a `build-git-wasm` recipe next to `build-ftp-wasm`)
- Create: `crates/norte-core/tests/columns_git_e2e.rs`

- [ ] **Step 1: Write the failing tests**

```rust
/// El plugin se instala en config_dir/plugins/ COMO SE INSTALARÍA EL DE UN
/// TERCERO. Que ese camino funcione es el criterio de salida de M4; embeberlo
/// en el binario probaría otra cosa.
#[tokio::test]
async fn el_plugin_instalado_pinta_la_columna_wasm_real() {
    let Some(wasm) = built_git_status_wasm() else { eprintln!("sin wasm32-wasip2: retirado"); return };
    let home = tempfile::tempdir().unwrap();
    install_plugin(&home, "org.norte.git-status", &wasm);
    let repo = git_repo_fixture(&["limpio.txt", "sucio.txt", "nuevo.txt"]);
    touch_after_index(&repo, "sucio.txt");

    let values = engine_with(&home).plugin_column_values(
        Some("org.norte.git-status"), "status", &paths_in(&repo)).await.unwrap();
    assert_eq!(values, vec![None, Some("M".into()), Some("?".into())]);
}

/// La historia de rendimiento de la interfaz, que nunca se había ejercitado.
#[tokio::test]
async fn un_indice_grande_contesta_dentro_del_deadline_y_la_2a_pagina_usa_la_CACHE() {
    let repo = git_repo_with_index_entries(50_000);
    let t0 = Instant::now();
    let first = column_values_for_page(&repo, 0).await;
    let t1 = Instant::now();
    let second = column_values_for_page(&repo, 1).await;
    let t2 = Instant::now();
    assert!(first.iter().any(Option::is_some), "la primera página contesta de verdad");
    assert!(t1 - t0 < deadline(), "la primera página cabe en el deadline");
    assert!((t2 - t1) * 4 < (t1 - t0), "la segunda página NO reparsea el índice");
}
```

- [ ] **Step 2: Run and watch them fail.** Run: `just t norte-core`

- [ ] **Step 3: Implement** the `just build-git-wasm` recipe and the install
helper. Tests retire with a message when the `wasm32-wasip2` target is absent,
the same convention the existing wasm e2e tests use.

- [ ] **Step 4: Green.** Run: `just t norte-core`

- [ ] **Step 5: Commit**

```bash
git add justfile crates/norte-core/tests plugins
git commit -m "test(core): the official columns plugin, installed and under load"
```

---

### Task 9: ADRs, documentation, and the close

**Files:**
- Create: `docs/adr/0057-a-plugin-may-be-given-a-location-it-cannot-name.md` (use the `adr` skill)
- Modify: `docs/adr/0037-plugin-data-out-v2.md` (an addendum: `column-values` changed shape)
- Modify: `CHANGELOG.md`, `ARCHITECTURE.md`, `docs/spec/norte-spec.md` §17 status

- [ ] **Step 1: Write ADR 0057** — the opaque token; why the basename privacy
decision (`plugins.rs:194`) is preserved instead of reversed; kernel confinement
instead of a path check; the bounds and that the budget is spent on failures
too; the capability that makes it visible at approval; and why the host functions
sit behind an injected trait rather than a dependency from `norte-plugin-host` to
`norte-vfs-local`.

- [ ] **Step 2: Addendum to ADR 0037** naming the new `column-values` shape and
the package bump, so the next reader does not find two contradicting records.

- [ ] **Step 3: Changelog**, in the file's voice.

- [ ] **Step 4: Open the follow-ups honestly** — staged status (index versus
HEAD) needs an object-database reader in the guest; a location over a non-`file://`
provider mints no token; submodules are not handled.

- [ ] **Step 5: One `just ci`.** Foreground, one recipe at a time, never through
`| tail`. Expected: green.

- [ ] **Step 6: Whole-branch review.** This branch touches the wire, the sandbox
and a capability gate — the three surfaces CLAUDE.md names as worth an external
pass. Dispatch `security-reviewer` and `protocol-guardian` over the full range.

- [ ] **Step 7: Commit and finish** with `superpowers:finishing-a-development-branch`.

```bash
git add docs CHANGELOG.md ARCHITECTURE.md
git commit -m "docs(adr,changelog): a plugin may be told where it is without being told where it is"
```
