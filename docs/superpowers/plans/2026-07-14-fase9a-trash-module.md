# Fase 9a — Papelera lógica: ADR + módulo puro `norte-vfs::trash` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the pure, I/O-free foundation for logical `.norte-trash/` — an ADR plus a fully unit-tested `norte-vfs::trash` module that builds trash-entry paths and encodes/decodes restore metadata — with no provider wiring yet.

**Architecture:** A new module `crates/norte-vfs/src/trash.rs` exposes pure helpers: `trash_id` (sortable id), `plan` (builds the `.norte-trash/<id>/{payload,.norte-info}` paths under a connection root), and `info_encode`/`info_decode` (restore metadata using `VPath::to_wire()`, which is lossless percent-encoded ASCII → line-safe, no base64). Providers (9b/9c) call these; the module never touches I/O and adds no new dependency. The `Provider::trash` trait signature is left unchanged (cancellation stays drop-based, per the design's revised §3).

**Tech Stack:** Rust, `norte-proto` (`VPath`, `Segment`, `Error`), `cargo nextest`.

---

## File Structure

- Create: `docs/adr/0019-papelera-logica-remota.md` — the ADR for the `.norte-trash/` design.
- Create: `crates/norte-vfs/src/trash.rs` — the pure module (helpers + unit tests inline).
- Modify: `crates/norte-vfs/src/lib.rs:13` — declare and re-export `pub mod trash;`.
- Reference (read only, do not modify): `crates/norte-proto/src/vpath.rs` (`VPath::{parse,to_wire,file_name,parent,join}`, `Segment::new`), `crates/norte-proto/src/error.rs` (`Error::{Unsupported,InvalidPath}`), `docs/superpowers/specs/2026-07-14-papelera-logica-remota-design.md` (the approved spec).

Note on scope: no `Cargo.toml` change (no base64, no tokio-util). No provider or config change — those are 9b (sftp) and 9c (object), each their own plan.

---

### Task 1: ADR 0019 — logical trash design

**Files:**
- Create: `docs/adr/0019-papelera-logica-remota.md`
- Modify: `docs/adr/README.md` (append the 0019 row to the index table)

- [ ] **Step 1: Write the ADR**

Create `docs/adr/0019-papelera-logica-remota.md` in MADR format, mirroring the style of `docs/adr/0009-trash.md`. Content (Spanish, matching repo convention):

```markdown
# 0019 — Papelera lógica `.norte-trash/` en providers remotos

- Estado: accepted
- Fecha: 2026-07-14
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: ADR 0009 (papelera nativa), 0016 (object), 0013 (sftp),
  spec §5. Diseño: `docs/superpowers/specs/2026-07-14-papelera-logica-remota-design.md`.

## Contexto y problema

ADR 0009 entregó papelera nativa (crate `trash`) para local/Mem y dejó
la «papelera lógica `.norte-trash/`» de la spec §5 para M2 con los
remotos. sftp y object no tienen trash del OS: necesitan borrado
recuperable propio sin degradar en silencio a permanente.

## Decisión

- **Opt-in por conexión, default OFF** (`logical_trash: bool`,
  `#[serde(default)]` = false). Off → el provider NO declara
  `CapabilityFlags::TRASH` → el frontend cae en la degradación B2 de ADR
  0009 (aviso «PERMANENTE», reenvía `Permanent`). Evita el coste sorpresa
  de copiar en S3 al borrar.
- **Layout** en la raíz del provider: `.norte-trash/<id>/{<basename>,
  .norte-info}`. `<id>` = `<epoch_ms>-<counter>` (monótono por sesión).
  `.norte-info` guarda la ruta original como `VPath::to_wire()`
  (percent-encoded ASCII, lossless, line-safe) + `deleted-ms`.
- **Sin cambio de firma del trait**: `Provider::trash(&self, p)` intacto.
  El trait no recibe `CancellationToken` (modelo por drop, sin dep
  `tokio-util` en el crate fundacional; rule 8). La garantía de cero
  pérdida viene del orden **copiar-todo → borrar-todo** en object.
- **Relocalización por provider**: sftp = `create_dir` + `rename` +
  `write(info)` (un tiro, `entries_total = 1`). object/S3 = copy-all →
  delete-all (cancelable sin pérdida en cualquier punto).
- **Módulo compartido `norte-vfs::trash`** (puro, sin I/O): construcción
  de id/paths y encode/decode del `.norte-info`. Los providers no se
  conocen entre sí; solo conocen el trait + este módulo.

## Consecuencias

Positivas: borrado recuperable en remotos con layout estable (M3 restaura
leyendo `.norte-info`); sin dep nuevo; default seguro (sin sorpresas de
coste). Negativas / deuda: cancelación de grano fino a mitad del walk S3
sigue siendo drop-based (deuda junto a #51); crash a mitad de la fase de
borrado deja estado duplicado (origen parcial + copia completa en trash),
recuperable, coherente con la no-atomicidad de S3 (ADR 0016). Decomposición
en 9a (este módulo + ADR), 9b (sftp), 9c (object).
```

- [ ] **Step 2: Add the index row**

In `docs/adr/README.md`, append a row to the table (immediately after the `0018` row), matching the existing column format:

```markdown
| [0019](0019-papelera-logica-remota.md) | Papelera lógica `.norte-trash/` en providers remotos | accepted |
```

- [ ] **Step 3: Commit**

```bash
git add docs/adr/0019-papelera-logica-remota.md docs/adr/README.md
git commit -m "docs(adr): 0019 papelera lógica .norte-trash/ remota (fase 9a)"
```

---

### Task 2: `trash_id` — sortable, valid-segment id

**Files:**
- Create: `crates/norte-vfs/src/trash.rs`
- Modify: `crates/norte-vfs/src/lib.rs`

- [ ] **Step 1: Create the module skeleton + failing test**

Create `crates/norte-vfs/src/trash.rs` with the module doc, the constants, `trash_id`, and a first test:

```rust
//! Papelera lógica `.norte-trash/` para providers sin trash nativo
//! (ADR 0019). Helpers PUROS, sin I/O: los providers construyen las rutas
//! y los metadatos con estas funciones y ejecutan la relocalización con
//! sus propios primitivos (rename en sftp, copy+delete en object).

use norte_proto::{Error, Segment, VPath};

/// Directorio raíz de la papelera lógica dentro de una conexión.
pub const TRASH_DIR: &[u8] = b".norte-trash";
/// Fichero de metadatos de restauración dentro de cada entrada.
pub const INFO_NAME: &[u8] = b".norte-info";
/// Cabecera de versión del fichero `.norte-info`.
const INFO_HEADER: &str = "norte-trash-info v1";

/// Identificador único y ordenable de una entrada de papelera:
/// `<deleted_ms>-<counter>`. `counter` es monótono por sesión para
/// desempatar borrados en el mismo milisegundo. Siempre un [`Segment`]
/// válido (solo dígitos y `-`).
#[must_use]
pub fn trash_id(deleted_ms: u64, counter: u64) -> String {
    format!("{deleted_ms}-{counter}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_id_is_sortable_and_valid_segment() {
        assert_eq!(trash_id(1_726_000_000_123, 0), "1726000000123-0");
        // Ordena lexicográficamente igual que numéricamente para mismo ancho.
        assert!(trash_id(1_726_000_000_123, 0) < trash_id(1_726_000_000_124, 0));
        // Siempre construye un Segment válido (sin `/`, sin NUL, no `.`/`..`).
        assert!(Segment::new(trash_id(1, 2).into_bytes()).is_ok());
    }
}
```

Declare the module in `crates/norte-vfs/src/lib.rs`. Add `mod trash;`'s public form right after line 13 (`mod sink;`):

```rust
pub mod trash;
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo nextest run -p norte-vfs trash::`
Expected: PASS (`trash_id_is_sortable_and_valid_segment`).

- [ ] **Step 3: Commit**

```bash
git add crates/norte-vfs/src/trash.rs crates/norte-vfs/src/lib.rs
git commit -m "feat(vfs): módulo trash — trash_id (fase 9a)"
```

---

### Task 3: `plan` — build `.norte-trash/<id>/{payload,info}` paths

**Files:**
- Modify: `crates/norte-vfs/src/trash.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/norte-vfs/src/trash.rs`:

```rust
    #[test]
    fn plan_builds_entry_under_provider_root() {
        let p = VPath::parse("sftp://host/deep/nested/victim.txt").unwrap();
        let paths = plan(&p, "1726000000123-0").unwrap();
        assert_eq!(
            paths.dir.to_wire(),
            "sftp://host/.norte-trash/1726000000123-0"
        );
        assert_eq!(
            paths.payload.to_wire(),
            "sftp://host/.norte-trash/1726000000123-0/victim.txt"
        );
        assert_eq!(
            paths.info.to_wire(),
            "sftp://host/.norte-trash/1726000000123-0/.norte-info"
        );
    }

    #[test]
    fn plan_preserves_hostile_basename() {
        // Segmento final no-UTF8 (0xFF 0xFE): el payload conserva sus bytes.
        let p = VPath::parse("sftp://host/dir/%FF%FE").unwrap();
        let paths = plan(&p, "1-0").unwrap();
        assert_eq!(
            paths.payload.to_wire(),
            "sftp://host/.norte-trash/1-0/%FF%FE"
        );
    }

    #[test]
    fn plan_refuses_provider_root() {
        let root = VPath::parse("sftp://host/").unwrap();
        assert!(matches!(plan(&root, "1-0"), Err(Error::Unsupported)));
    }

    #[test]
    fn plan_rejects_bad_id() {
        let p = VPath::parse("sftp://host/x").unwrap();
        // Un id con `/` no es un Segment válido.
        assert!(matches!(plan(&p, "bad/id"), Err(Error::InvalidPath)));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p norte-vfs trash::`
Expected: FAIL to compile — `plan` and `TrashPaths` not defined.

- [ ] **Step 3: Implement `plan` + `TrashPaths`**

Add above the `tests` module in `crates/norte-vfs/src/trash.rs`:

```rust
/// Rutas absolutas de una entrada de papelera para un path a borrar.
#[derive(Debug, Clone)]
pub struct TrashPaths {
    /// El directorio de la entrada: `.norte-trash/<id>/`.
    pub dir: VPath,
    /// El payload movido: `.norte-trash/<id>/<basename-original>`.
    pub payload: VPath,
    /// Los metadatos: `.norte-trash/<id>/.norte-info`.
    pub info: VPath,
}

/// Construye las rutas de papelera para `p` bajo la raíz de su conexión.
///
/// `id` debe venir de [`trash_id`] (o cualquier [`Segment`] válido).
///
/// # Errors
/// - [`Error::Unsupported`] si `p` es la raíz del provider (sin basename):
///   la raíz de la conexión no se papeleriza.
/// - [`Error::InvalidPath`] si `id` no es un segmento válido.
pub fn plan(p: &VPath, id: &str) -> Result<TrashPaths, Error> {
    let basename = p.file_name().ok_or(Error::Unsupported)?.clone();
    let id_seg = Segment::new(id.as_bytes().to_vec()).map_err(|_| Error::InvalidPath)?;
    let dir = provider_root(p).join(seg_const(TRASH_DIR)).join(id_seg);
    let payload = dir.join(basename);
    let info = dir.join(seg_const(INFO_NAME));
    Ok(TrashPaths { dir, payload, info })
}

/// La raíz de la conexión de `p` (mismo scheme+authority, sin segmentos).
fn provider_root(p: &VPath) -> VPath {
    let mut r = p.clone();
    while let Some(parent) = r.parent() {
        r = parent;
    }
    r
}

/// Segmento desde bytes de una constante del módulo (`TRASH_DIR`,
/// `INFO_NAME`). Invariante: son literales válidos; un pánico aquí es un
/// bug del módulo, jamás entrada de usuario.
fn seg_const(bytes: &[u8]) -> Segment {
    Segment::new(bytes.to_vec()).expect("constante de papelera es un Segment válido")
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p norte-vfs trash::`
Expected: PASS (all `plan_*` tests + `trash_id`).

- [ ] **Step 5: Commit**

```bash
git add crates/norte-vfs/src/trash.rs
git commit -m "feat(vfs): trash::plan — rutas .norte-trash/<id>/{payload,info} (fase 9a)"
```

---

### Task 4: `info_encode` / `info_decode` — restore metadata

**Files:**
- Modify: `crates/norte-vfs/src/trash.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/norte-vfs/src/trash.rs`:

```rust
    #[test]
    fn info_roundtrips_hostile_path() {
        // Ruta con byte no-UTF8 (0xFF) Y un byte de control newline (0x0A)
        // dentro de un segmento: to_wire los escapa a %FF/%0A → line-safe.
        let p = VPath::parse("sftp://host/a/%FF/x%0Ay").unwrap();
        let bytes = info_encode(&p, 1_726_000_000_123);

        // Line-safe: exactamente 3 líneas, ningún newline dentro del valor.
        let text = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(text.lines().count(), 3);

        let info = info_decode(&bytes).unwrap();
        assert_eq!(info.original, p);
        assert_eq!(info.deleted_ms, 1_726_000_000_123);
    }

    #[test]
    fn info_decode_rejects_corrupt() {
        assert!(matches!(info_decode(b"garbage"), Err(Error::InvalidPath)));
        assert!(matches!(
            info_decode(b"norte-trash-info v1\npath: sftp://host/x\n"),
            Err(Error::InvalidPath) // falta deleted-ms
        ));
        assert!(matches!(
            info_decode(b"norte-trash-info v1\npath: not-a-wire-path\ndeleted-ms: 5\n"),
            Err(Error::InvalidPath) // wire no parsea
        ));
        assert!(matches!(
            info_decode(b"norte-trash-info v1\npath: sftp://host/x\ndeleted-ms: NaN\n"),
            Err(Error::InvalidPath) // ms no numérico
        ));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p norte-vfs trash::`
Expected: FAIL to compile — `info_encode`, `info_decode`, `TrashInfo` not defined.

- [ ] **Step 3: Implement encode/decode + `TrashInfo`**

Add above the `tests` module in `crates/norte-vfs/src/trash.rs`:

```rust
/// Metadatos de restauración de una entrada de papelera.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashInfo {
    /// Ruta original, reconstruida desde su forma wire.
    pub original: VPath,
    /// Instante de borrado, ms desde epoch.
    pub deleted_ms: u64,
}

/// Serializa los metadatos de restauración a los bytes de un `.norte-info`.
/// La ruta va como [`VPath::to_wire`] (percent-encoded ASCII, lossless,
/// sin controles ni newlines) → el resultado es line-safe.
#[must_use]
pub fn info_encode(original: &VPath, deleted_ms: u64) -> Vec<u8> {
    format!(
        "{INFO_HEADER}\npath: {}\ndeleted-ms: {deleted_ms}\n",
        original.to_wire()
    )
    .into_bytes()
}

/// Parsea el contenido de un `.norte-info`.
///
/// # Errors
/// [`Error::InvalidPath`] si el contenido no es UTF-8, le falta la
/// cabecera o un campo, la ruta wire no parsea, o el timestamp no es un
/// `u64`.
pub fn info_decode(bytes: &[u8]) -> Result<TrashInfo, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::InvalidPath)?;
    let mut lines = text.lines();
    if lines.next() != Some(INFO_HEADER) {
        return Err(Error::InvalidPath);
    }
    let wire = lines
        .next()
        .and_then(|l| l.strip_prefix("path: "))
        .ok_or(Error::InvalidPath)?;
    let ms = lines
        .next()
        .and_then(|l| l.strip_prefix("deleted-ms: "))
        .ok_or(Error::InvalidPath)?;
    let original = VPath::parse(wire).map_err(|_| Error::InvalidPath)?;
    let deleted_ms = ms.parse::<u64>().map_err(|_| Error::InvalidPath)?;
    Ok(TrashInfo {
        original,
        deleted_ms,
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p norte-vfs trash::`
Expected: PASS (all trash tests).

- [ ] **Step 5: Commit**

```bash
git add crates/norte-vfs/src/trash.rs
git commit -m "feat(vfs): trash::info_encode/decode — metadatos .norte-info (fase 9a)"
```

---

### Task 5: Gate check — clippy, fmt, docs, full CI

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Expected: no diff, or apply formatting.

- [ ] **Step 2: Clippy (pedantic gate)**

Run: `cargo clippy -p norte-vfs --all-targets -- -D warnings`
Expected: PASS, no warnings. If clippy flags `to_vec()` on the const slices or similar, address per its suggestion without changing behavior.

- [ ] **Step 3: Doctest / missing_docs**

Run: `cargo test -p norte-vfs --doc`
Expected: PASS. `norte-vfs` has `#![warn(missing_docs)]` — every new `pub` item (`TRASH_DIR`, `INFO_NAME`, `trash_id`, `TrashPaths` + fields, `plan`, `TrashInfo` + fields, `info_encode`, `info_decode`) already carries a doc comment; confirm no `missing_docs` warning.

- [ ] **Step 4: Full local CI**

Run: `just ci`
Expected: green (fmt + clippy + nextest + deny + coverage gate). This is the release gate — GitHub CI is disabled (billing).

- [ ] **Step 5: Final commit if CI made changes**

```bash
git add -A
git commit -m "style: cargo fmt + clippy fase 9a" # only if fmt/clippy changed files
```

---

## Self-Review Notes

- **Spec coverage:** ADR (Task 1) ← spec «Decisiones tomadas» + arquitectura; `trash_id` (Task 2) ← `<id>` layout; `plan` (Task 3) ← layout `.norte-trash/<id>/{payload,info}` + provider-root + refuse-root; `info_encode`/`decode` (Task 4) ← `.norte-info` format on `to_wire`, hostile-path line-safety. Provider relocation + config field are explicitly out of 9a (spec decomposition → 9b/9c).
- **No trait/Cargo change** confirmed: matches revised spec §3 (no token, drop-based) and «SIN base64».
- **Type consistency:** `TrashPaths{dir,payload,info}`, `TrashInfo{original,deleted_ms}`, `plan(&VPath,&str)->Result<TrashPaths,Error>`, `info_encode(&VPath,u64)->Vec<u8>`, `info_decode(&[u8])->Result<TrashInfo,Error>` used identically across tasks.
- **Error variants** (`Unsupported`, `InvalidPath`) verified present in `crates/norte-proto/src/error.rs`.
- **Follow-ups:** 9b (sftp trash + `logical_trash` config + contract/cancel tests), 9c (object copy-all/delete-all + cancel tests) get their own plans.
