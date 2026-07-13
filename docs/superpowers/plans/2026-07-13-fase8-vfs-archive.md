# Fase 8 M2 — norte-vfs-archive (zip/tar read-only) — Plan de implementación

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Navegar zip/tar como directorios virtuales (READ-ONLY) desde cualquier provider interior (local/sftp/s3/mem), con nombres = bytes, anti zip-bomb y suite contractual propia.

**Architecture:** `ArchiveProvider` implementa `Provider` por COMPOSICIÓN: lee los bytes del archivo exterior a través de otro `Provider` (nunca `std::fs`). Direccionamiento por scheme compuesto `<formato>+<scheme-interior>` y segmento marcador `!`: `zip+file:///home/o/a.zip/!/docs/x.txt`. Índice por archivo cacheado (LRU) con invalidación por (mtime,size) del exterior. Parsing sync (`zip`/`tar` crates) dentro de `spawn_blocking` sobre un adaptador `Read+Seek` que hace range-reads al provider interior.

**Tech Stack:** Rust workspace existente; crates nuevos deps: `zip` (default-features off + deflate), `tar`. Sin cambios de wire salvo capability `READ_ONLY` (proto 0.8.0 → 0.9.0).

**Decisiones fijadas (van al ADR 0018, tarea 1):**
1. **Direccionamiento**: scheme compuesto `zip+file` / `tar+sftp` / … (la gramática de `Scheme` ya admite `+`). El PRIMER segmento igual a `!` separa path exterior (el archivo) de path interior (la entrada). Authority = la del provider interior. Un path exterior que contenga un segmento `!` literal → `InvalidPath` (inaddressable, documentado). Entradas interiores llamadas `!` no son ambiguas (se corta en el PRIMER marcador).
2. **v1 una sola capa**: scheme interior con otro `+` (anidamiento `zip+tar+file`) → `Unsupported` + issue. tar.gz/tgz → issue (capa de compresión ortogonal futura).
3. **Formatos v1**: tar plano (crate `tar`) y zip stored+deflate (crate `zip`, `by_index_raw`/`name_raw` para bytes crudos). Método de compresión no soportado o entrada cifrada: se LISTA (metadatos) pero `read` → `Unsupported`.
4. **Nombres = bytes**: el nombre de entrada se conserva crudo (bit 11 solo como metadato futuro; NO se decodifica cp437 a ciegas — la reinterpretación manual "reinterpretar como…" es feature de display futura, issue). Entradas cuyo nombre no mapea a segmentos `VPath` válidos (`..`, `.`, vacío, NUL, absoluto, componente `!`) se OMITEN del árbol con `tracing::warn!` + contador (zip-slip defense; encoding-auditor valida la política). Duplicados: última gana (semántica zip) + warn. Conflicto file-vs-dir sobre el mismo path: gana dir + warn.
5. **Anti-bomba**: límites en construcción de índice (`Limits { max_entries: 500_000, max_name_bytes: 4_096, max_depth: 64 }`, overridable para tests); superarlos → `Error::Io { retryable: false }` + warn. Anidamiento ya limitado por (2). Archivo corrupto/truncado → `Io { retryable: false }` (sin variante nueva de Error; issue para `Corrupt` dedicado).
6. **Capability nueva** `READ_ONLY = 1 << 8`: el core/UI vetan mutaciones sin round-trip. Toda mutación del provider responde `Unsupported`. Wire change → bump 0.9.0 + goldens + protocol-guardian.
7. **Caché**: LRU de índices dentro del provider (cap 8 archivos), clave = wire del path exterior, validación por `stat` exterior (mtime,size) en cada operación; cambio → rebuild transparente.
8. **Fixtures = código**: `zipsmith`/`tarsmith` en `norte-testkit` (builders puros deterministas de bytes zip/tar hostiles). Nada de binarios commiteados.

**Sub-bloques (commits, patrón fase 7):**
- 8a `docs(adr): 0018 provider archive` → protocol-guardian
- 8b `feat(proto): capability READ_ONLY + helpers archive_compose/split — proto 0.9.0` → protocol-guardian
- 8c `feat(vfs,testkit): readonly_provider_contract! + zipsmith/tarsmith`
- 8d `feat(vfs-archive): crate nuevo — tar read-only`
- 8e `feat(vfs-archive): zip read-only + corpus hostil` → encoding-auditor OBLIGATORIO
- 8f `feat(core,tui): composición de schemes archive + navegación` → rust-reviewer + E2E CLI
- 8g cierre: issues de deuda, memoria, `just ci`

---

### Task 1 (8a): ADR 0018 — provider archive

**Files:**
- Create: `docs/adr/0018-provider-archive.md`

- [ ] **Step 1:** Invocar skill `/adr` con las 8 decisiones de la cabecera (direccionamiento `!`, composición, v1 una capa, formatos, política de nombres hostiles, límites, READ_ONLY, caché). Contexto: spec §5 «Archivos como directorios», §6.1 fila ZIP, threat model (zip bomb). Alternativas consideradas: (a) authority sintética con handle de montaje (estado en daemon, rompe bookmarks) — descartada; (b) URI exterior percent-encoded en un segmento (los segmentos prohíben `/`) — inviable; (c) marcador `!` + scheme compuesto — elegida (stateless, gramática ya válida).
- [ ] **Step 2:** Agent protocol-guardian revisa el ADR (semántica de VPath es protocolo). Aplicar hallazgos.
- [ ] **Step 3:** Commit: `docs(adr): 0018 provider archive — direccionamiento !, composición, límites (fase 8a M2)`

### Task 2 (8b): proto — READ_ONLY + archive_compose/split, 0.9.0

**Files:**
- Modify: `crates/norte-proto/src/caps.rs` (flag + parser de nombres)
- Modify: `crates/norte-proto/src/vpath.rs` (helpers + `ArchiveRef`)
- Modify: goldens/versión (localizar: `grep -rn "0\.8\.0" crates/norte-proto`)

- [ ] **Step 1:** Test rojo caps: roundtrip serde de `READ_ONLY`, parser acepta `read_only`, rechaza hex/desconocidos (patrón de los flags existentes en caps.rs).
- [ ] **Step 2:** Añadir `const READ_ONLY = 1 << 8;` + doc + nombre en el parser propio. Correr golden: fallará → actualizar golden + bump versión protocolo a 0.9.0 (mismo procedimiento que fase 7f).
- [ ] **Step 3:** Tests rojos de helpers (en vpath.rs, `#[cfg(test)]` + doctests):

```rust
// zip+file:///home/o/a.zip/!/docs/x.txt
let outer = VPath::parse("file:///home/o/a.zip").unwrap();
let root = VPath::archive_compose("zip", &outer, &[]).unwrap();
assert_eq!(root.to_wire(), "zip+file:///home/o/a.zip/!");
let r = root.archive_split().unwrap().unwrap();
assert_eq!(r.format, "zip");
assert_eq!(r.outer, outer);
assert!(r.inner.is_empty());
// exterior con segmento `!` → InvalidByte-like error; scheme ya compuesto → error (v1 una capa)
assert!(VPath::archive_compose("zip", &VPath::parse("file:///a/!/b.zip").unwrap(), &[]).is_err());
assert!(VPath::archive_compose("zip", &root, &[]).is_err());
// split de un path sin `+` → Ok(None); con `+` sin marcador → Err
assert!(outer.archive_split().unwrap().is_none());
```

- [ ] **Step 4:** Implementar:

```rust
/// Referencia desmontada de un path de archivo-como-directorio (ADR 0018):
/// `<formato>+<scheme>://auth/<exterior>/!/<interior>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRef {
    /// Formato del contenedor (`zip`, `tar`).
    pub format: String,
    /// Path del ARCHIVO contenedor en su provider interior.
    pub outer: VPath,
    /// Segmentos interiores relativos a la raíz del archivo.
    pub inner: Vec<Segment>,
}

impl VPath {
    pub fn archive_compose(format: &str, outer: &VPath, inner: &[Segment]) -> Result<Self, VPathError>;
    pub fn archive_split(&self) -> Result<Option<ArchiveRef>, VPathError>;
}
```

Reglas (ADR 0018 revisado por guardian — NORMATIVAS):
- Whitelist de formatos en proto: `const ARCHIVE_FORMATS: &[&str] = &["zip", "tar"];` — la descomposición es por whitelist, NO por sintaxis (`s3+v2.x-y` es scheme de provider legítimo pinneado en goldens → `archive_split` = `Ok(None)`).
- `compose`: format ∈ whitelist; scheme exterior sin prefijo de formato; rechaza segmento `b"!"` en AMBOS lados (exterior E interior); scheme resultante `format + "+" + outer.scheme()`.
- `split`: prefijo hasta primer `+` ∉ whitelist → `Ok(None)`; interior que empiece a su vez por formato (`zip+tar+…`) → `Err` (v1 una capa); scheme compuesto SIN segmento `!` → `Err` (malformado); split en el PRIMER `!`; `!` extra en interior NO se rechaza (resuelve NotFound en el provider). Solo sobre segmentos parseados, jamás sobre el string wire.
- Goldens vpath nuevos: compuesto válido con marcador, alias `%21`≡`!`, marcador final (raíz), `!` múltiple. Ventana de versión: actualizar `tests/types.rs:621-632` y `golden_types.rs:731` (0.7.x sale de la ventana N/N-1).
Rustdoc con doctest en los 3 items públicos.
- [ ] **Step 5:** `cargo nextest run -p norte-proto && cargo test --doc -p norte-proto` verde; `cargo clippy -p norte-proto --all-targets -- -D warnings`.
- [ ] **Step 6:** Agent protocol-guardian sobre el diff de proto. Aplicar hallazgos.
- [ ] **Step 7:** Commit: `feat(proto): capability READ_ONLY + archive_compose/split — proto 0.9.0 (fase 8b M2)`

### Task 3 (8c): testkit zipsmith/tarsmith + readonly_provider_contract!

**Files:**
- Create: `crates/norte-testkit/src/smith.rs` (mod `zipsmith` + `tarsmith`)
- Modify: `crates/norte-testkit/src/lib.rs` (export)
- Create: `crates/norte-vfs/src/contract_ro.rs` (macro `readonly_provider_contract!`)
- Modify: `crates/norte-vfs/src/lib.rs` (mod + export)

- [ ] **Step 1:** `zipsmith`: builder determinista de bytes ZIP (stored only; local headers + central directory + EOCD; CRC32 real vía tabla propia ~20 líneas, sin dep). API:

```rust
pub struct ZipSmith { entries: Vec<(Vec<u8>, ZipEntryKind)> }
pub enum ZipEntryKind { File { data: Vec<u8>, utf8_flag: bool }, Dir }
impl ZipSmith {
    pub fn new() -> Self;
    pub fn file(self, name: &[u8], data: &[u8]) -> Self;          // bit11 off
    pub fn file_utf8(self, name: &[u8], data: &[u8]) -> Self;     // bit11 on
    pub fn dir(self, name: &[u8]) -> Self;                        // nombre con `/` final
    pub fn build(self) -> Vec<u8>;
    /// EOCD que declara `n` entradas sin que existan (bomba de índice barata).
    pub fn build_lying_eocd(self, n: u16) -> Vec<u8>;
}
```

`tarsmith` equivalente (headers ustar 512B, checksum octal; nombres >100 bytes: NO soportado, panic con mensaje — asimetría documentada, precedente MinIO-255). Tests unit del propio smith: un zip de zipsmith lo abre el crate `zip` (dev-dep de testkit SOLO aquí… NO: testkit no debe depender de `zip`; validarlo en los tests de norte-vfs-archive, Task 4/5 — aquí solo estructura: EOCD count, offsets coherentes).
- [ ] **Step 2:** Macro `readonly_provider_contract!` en `contract_ro.rs`, misma mecánica que `provider_contract!` (módulo generado, factory fresco por test). Parámetros: `mod`, `factory` (provider YA sembrado con el árbol canónico), `root`, `hostile_names`. Árbol canónico exigido al factory (documentado en el rustdoc de la macro):

```text
/docs/hello.txt      → b"hola norte\n"
/docs/sub/nested.bin → b"\x00\x01\x02\xff"
/vacio.txt           → b""
/hostile/<name>      → content = name bytes   (por cada hostile_name)
```

Casos (~12): stat root = Dir; stat/list/read coherentes con el árbol; read range (offset interior, `offset > EOF` → stream vacío, `len` recorta); stat inexistente → NotFound; read de dir → `Conflict{TypeMismatch}` o `Io` (tolerancia como la suite RW); roundtrip hostiles por LIST (los nombres aparecen byte-exactos como `file_name()`); caps contienen `READ_ONLY` y NO contienen `RENAME_ATOMIC|APPEND|RANDOM_WRITE|TRASH`; `write/mkdir/remove/rename/trash/symlink/open_resumable` → `Err(Unsupported)`; `copy_native` → `None`; `read_link` → `Unsupported|NotFound`.
- [ ] **Step 3:** Smoke de la macro dentro de norte-vfs: no hay provider RO aún — instanciarla en Task 4. Aquí: `cargo clippy -p norte-testkit -p norte-vfs --all-targets -- -D warnings` + nextest de ambos crates verdes (tests del smith).
- [ ] **Step 4:** Commit: `feat(vfs,testkit): readonly_provider_contract! + zipsmith/tarsmith (fase 8c M2)`

### Task 4 (8d): crate norte-vfs-archive — tar read-only

**Files:**
- Create: `crates/norte-vfs-archive/` (skill `/new-provider` para scaffolding: Cargo.toml Apache/MIT, lints, forbid unsafe)
- Create: `src/lib.rs`, `src/provider.rs`, `src/index.rs`, `src/blocking.rs`, `src/tar.rs`
- Create: `tests/contract.rs`, `tests/hostile.rs`
- Modify: `Cargo.toml` (workspace members) — el skill lo hace

- [ ] **Step 1:** Scaffolding con `/new-provider` (nombre `norte-vfs-archive`). Deps: `tar` (justificación regla 8 en descripción del commit: parser ustar/GNU/pax battle-tested, puro Rust, sin default features raras; alternativa hand-rolled descartada por pax/longnames). `zip` entra en Task 5.
- [ ] **Step 2:** `blocking.rs`: adaptador sync sobre provider interior:

```rust
/// `Read + Seek` sync sobre `Provider::read(range)` del interior, para
/// parsers de archivo dentro de `spawn_blocking`. Bloques de 256 KiB con
/// caché del último bloque (los parsers hacen ráfagas locales).
pub(crate) struct ProviderReader {
    handle: tokio::runtime::Handle,
    inner: Arc<dyn Provider>,
    path: VPath,
    len: u64,
    pos: u64,
    block: Option<(u64, Vec<u8>)>, // (offset de bloque, bytes)
}
```

`Read::read` = localizar bloque (si no cacheado: `handle.block_on(inner.read(path, Some(ByteRange{offset, len: Some(BLOCK)})))` + drenar stream), copiar. `Seek` aritmético. Errores → `std::io::Error::other`. Test unit contra `MemProvider`.
- [ ] **Step 3:** `index.rs`:

```rust
pub(crate) struct Limits { pub max_entries: usize, pub max_name_bytes: usize, pub max_depth: usize }
impl Default for Limits { /* 500_000 / 4_096 / 64 */ }

pub(crate) enum Locator { Tar { offset: u64, size: u64 }, /* Task 5: Zip { index: usize } */ }
pub(crate) struct Node { pub kind: EntryKind, pub size: Option<u64>, pub mtime_ms: Option<i64>, pub locator: Option<Locator> }
/// Árbol plano: clave = segmentos interiores; dirs implícitos materializados.
pub(crate) struct ArchiveIndex {
    pub nodes: HashMap<Vec<Vec<u8>>, Node>,
    pub children: HashMap<Vec<Vec<u8>>, Vec<Vec<u8>>>,
    pub skipped: u64,               // entradas hostiles omitidas (warn)
    pub generation: (Option<i64>, Option<u64>), // (mtime_ms, size) del exterior
}
```

`fn insert_entry(&mut self, raw_name: &[u8], node: Node, limits: &Limits)`: split por `b'/'`; rechaza (skip+warn+skipped++) si: componente vacío no-final, `.`/`..`, NUL, nombre absoluto (empieza por `/`), > max_name_bytes, > max_depth, componente inválido como `Segment`. `/` final = dir. Dup: reemplaza (última gana, warn). File-vs-dir: dir gana (warn). max_entries superado → `Err`. Tests unit exhaustivos de esta función (zip-slip `../evil`, `/etc/passwd`, `a//b`, `a/./b`, 0-bytes, dup, file-then-dir, dir-then-file).
- [ ] **Step 4:** `tar.rs`: `fn build_index(reader: ProviderReader, limits: &Limits) -> Result<ArchiveIndex, Error>` con crate `tar` (`Archive::new`, `entries()`, `path_bytes()`, `entry.raw_file_position()`, `header.entry_type()` → File/Dir; symlink de tar: kind Symlink sin locator, `read_link` → bytes de `link_name_bytes` — o v1: tratar como skip+warn; DECISIÓN: listar como `EntryKind::Symlink`, `read_link` devuelve target crudo, `read` → TypeMismatch). Truncado/corrupto → `Io{retryable:false}`.
- [ ] **Step 5:** `provider.rs`: `ArchiveProvider`:

```rust
pub struct ArchiveProvider {
    scheme: String,               // "tar+file", "zip+sftp"…
    format: Format,               // Tar | Zip
    inner: Arc<dyn Provider>,
    limits: Limits,
    cache: Mutex<LruIndexCache>,  // cap 8; clave wire del exterior; RAII como listings 7f
}
impl ArchiveProvider {
    pub fn new(inner: Arc<dyn Provider>, format: Format, scheme: impl Into<String>) -> Self;
    pub fn with_limits(..., limits: Limits) -> Self;  // pub(crate)? no: pub para tests → #[doc(hidden)] no; hacerla pub documentada
}
```

Toda operación: `p.archive_split()` → `ArchiveRef` (scheme/format deben cuadrar con self, si no `InvalidPath`); `inner.stat(outer)` (NotFound propaga; kind != File → `Conflict{TypeMismatch}`); cache lookup por (wire exterior, mtime, size) → índice; miss/stale → `spawn_blocking(build_index)`. `stat`: raíz interior = Dir sintético; nodo del índice → `Entry{ path: p.clone(), … }`. `list`: children → stream inmediato (índice ya en RAM). `read`: solo Tar v1: range-read DIRECTO al interior (`offset + range` recortado a size, contiguo en tar) — sin spawn_blocking. Mutaciones → `Unsupported`; caps = `READ_ONLY | CASE_SENSITIVE | CASE_PRESERVING`. `#[instrument]` en cada op con vpath redactado (patrón de los otros providers).
- [ ] **Step 6:** `tests/contract.rs`: factory = MemProvider + tarsmith del árbol canónico (helper `fn seed_tar() -> ArchiveProvider`; escribir bytes al Mem vía sink) + `readonly_provider_contract!`. Hostile names filtrados a ≤100 bytes (tarsmith). Root: `VPath::archive_compose("tar", &mem_path, &[])`.
- [ ] **Step 7:** `tests/hostile.rs` (tar): zip-slip tar (`../x`), tar truncado a mitad de header → `Io`, tar con 501k entradas sintéticas con `with_limits(max_entries: 100)` y 101 entradas → `Io`, entrada duplicada, symlink listado + `read_link` roundtrip, invalidación: reescribir el tar en el Mem (mtime/size cambian) → siguiente `list` refleja el nuevo contenido.
- [ ] **Step 8:** Test de cancelación (regla 3): `read` de entrada grande, tomar 1 chunk, drop del stream — para tar v1 el read es passthrough del interior (la cancelación es la del interior, ya contractual). Anotarlo en rustdoc; el test de cancelación real de descompresión llega con zip (Task 5).
- [ ] **Step 9:** `cargo nextest run -p norte-vfs-archive` + clippy + doc verdes. Agent rust-reviewer sobre el diff.
- [ ] **Step 10:** Commit: `feat(vfs-archive): crate norte-vfs-archive — tar read-only (fase 8d M2)`

### Task 5 (8e): zip read-only + corpus hostil + encoding-auditor

**Files:**
- Create: `crates/norte-vfs-archive/src/zip.rs`
- Modify: `src/provider.rs` (Format::Zip en read), `src/index.rs` (Locator::Zip), `Cargo.toml` (+`zip` default-features=false features=["deflate"])
- Create: `tests/hostile_zip.rs`
- Modify: `crates/norte-testkit/src/smith.rs` si faltan knobs (deflate real vía dev-dep flate2? NO — stored basta; deflate se testea con un zip generado por el crate `zip` writer en dev-deps de vfs-archive)

- [ ] **Step 1:** `zip.rs::build_index`: `ZipArchive::new(ProviderReader)` en spawn_blocking; iterar `by_index_raw(i)`, `name_raw()` crudo → `insert_entry`; dir = nombre con `/` final o `is_dir()`; `mtime` DOS → ms (helper; DOS time es local — documentar aproximación); `Locator::Zip{index}`. Entrada cifrada o método ≠ stored/deflate: se indexa con `Node{locator: None}` → `read` responde `Unsupported`. Zip corrupto (EOCD ausente/central dir truncado) → `Io{retryable:false}`. Límite `max_entries` ANTES de iterar (`archive.len()`).
- [ ] **Step 2:** `read` zip: spawn_blocking + `mpsc::channel(4)`: hilo abre `by_index(i)` (descomprime), lee chunks 64 KiB, aplica range (skip offset / take len) SOBRE los bytes descomprimidos, `blocking_send`; receptor → `ByteStream` (`ReceiverStream`). Drop del receptor → send falla → hilo termina (cancelación limpia).
- [ ] **Step 3:** Test cancelación (regla 3): entrada deflate de ~8 MiB (dev-dep `zip` writer o datos repetitivos vía zipsmith stored grande), leer 1 chunk, drop, `tokio::time::timeout` sobre un canal testigo de fin del hilo → termina < 2s, sin panics.
- [ ] **Step 4:** `tests/contract.rs`: segundo instanciamiento de `readonly_provider_contract!` (mod `zip_ro`) con factory zipsmith (corpus hostil COMPLETO, sin filtro 100B).
- [ ] **Step 5:** `tests/hostile_zip.rs`: bit11 off + bytes cp437 crudos (`b"CAF\x82.TXT"` — se listan byte-exactos); bit11 on con UTF-8 inválido (flag mentiroso: bytes crudos igual, sin pánico); `../evil` y `/abs` omitidos con `skipped > 0`; entrada `!` interior direccionable; dup última-gana; file-vs-dir; `build_lying_eocd(60_000)` con `with_limits(max_entries: 100)` → `Io` sin OOM; zip vacío; zip de 0 bytes → `Io`; stored+deflate roundtrip de contenido byte-exacto; range read sobre deflate (offset 1, len 3).
- [ ] **Step 6:** Agent encoding-auditor sobre TODO el crate (OBLIGATORIO — política de nombres, bit11, cp437). Aplicar hallazgos; fixtures nuevas que proponga → zipsmith cases.
- [ ] **Step 7:** Agent rust-reviewer. `just ci` (ojo trampa: caché clippy → `touch` si sospechoso).
- [ ] **Step 8:** Commit: `feat(vfs-archive): zip read-only — bit11/cp437, límites anti-bomba (fase 8e M2)`

### Task 6 (8f): core wiring + navegación TUI + E2E CLI

**Files:**
- Modify: `crates/norte-core/src/engine.rs` (`provider_for`: rama archive)
- Modify: `crates/norte-core/Cargo.toml` (+norte-vfs-archive)
- Modify: `crates/norte-tui/src/…` (Enter sobre `*.zip`/`*.tar`: localizar handler con `grep -rn "EntryKind::File" crates/norte-tui/src` y el sitio de navegación; componer con `VPath::archive_compose`; Backspace/left en raíz interior → volver al dir del archivo vía `archive_split`)
- Modify: `crates/norte-cli` solo si el routing lo exige (los paths compuestos ya viajan como VPath normales)

- [ ] **Step 1:** Test rojo en core (tests de engine existentes como plantilla): registrar MemProvider (scheme `mem`), sembrar `a.tar` con tarsmith, `engine.list(zip… no: "tar+mem://…/a.tar/!")` → entradas. Segundo test: `write` dentro del archivo → `Unsupported`; tercero: scheme anidado `zip+tar+mem` → `Unsupported`.
- [ ] **Step 2:** Implementar rama en `provider_for` ANTES del fallback a connector: si `p.archive_split()?` es `Some(ref)`: resolver provider del exterior con la lógica plana existente (sin recursión: `ref.outer` no lleva `+`), `Format` por `ref.format` (`zip`/`tar`; otro → `Unsupported`), `ArchiveProvider::new`, cachear bajo `provider_key(p)`. `archive_split` Err → `InvalidPath`.
- [ ] **Step 3:** TUI: Enter sobre File cuyo `file_name()` termina (ASCII case-insensitive) en `.zip`/`.tar` → navegar a `archive_compose`; si `list` falla, error normal de navegación (strings existentes; si hace falta uno nuevo → Fluent `t!`). Salida: en raíz interior, «subir» → `archive_split().outer.parent()`… coherente con el stack de navegación existente (reusar el mecanismo actual de historial si lo hay — descubrir en el grep).
- [ ] **Step 4:** E2E CLI (patrón fase 7e): `cargo build -p norte-cli` (¡trampa binario rancio!); script: crear zip real con `python3 -m zipfile` o zipsmith vía test-bin, `norte ls "zip+file:///tmp/…/fix.zip/!"`, `norte cat`/read de una entrada, exit codes SIN pipe (trampa `$?`).
- [ ] **Step 5:** Agent rust-reviewer (frontends sin lógica de negocio: la composición del vpath en TUI es navegación, no negocio — justificarlo en el commit). `just ci` verde.
- [ ] **Step 6:** Commit: `feat(core,tui,cli): archivos como directorios — composición zip+/tar+ y navegación (fase 8f M2)`

### Task 7 (8g): cierre

- [ ] **Step 1:** Issues de deuda (gh): tar.gz/tgz capa compresión; anidamiento multi-capa; «reinterpretar nombres como…» (chardetng override display); variante `Error::Corrupt`; límites configurables por config; surfacing de `skipped` al frontend (badge); NodeId para archives; mtime DOS timezone.
- [ ] **Step 2:** Bench opcional: listar zip de 100k entradas (presupuesto spec <200ms primer render) — si excede, issue, no bloquea fase.
- [ ] **Step 3:** Actualizar memoria (`proyecto-norte-estado.md`: fase 8 completa, deuda; trampas nuevas si las hubo).
- [ ] **Step 4:** `just ci` final + revisar que ningún commit arrastró `landing/` (trampa `git add -A`).

---

**Self-review hecho:** cobertura spec §5 archivos-como-dirs (Tasks 2/4/5/6), §6.1 fila ZIP (Task 5 + auditor; reinterpretación manual → issue explícito), threat model zip-bomb (Limits Task 4.3/5.5), M2 read-only ✓, anidamiento/`tar.gz` explícitamente diferidos con issue (spec los permite: «límite de profundidad configurable» — v1 profundidad 1). Tipos coherentes: `ArchiveRef{format,outer,inner}` usado en Tasks 2/4/6; `Limits` en 4/5; `readonly_provider_contract!` firma única (mod/factory/root/hostile_names) en 3/4/5.
