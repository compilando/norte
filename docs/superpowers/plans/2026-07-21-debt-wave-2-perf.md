# Debt Wave 2 — Perf (#77, #61.1+.2, #52) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cerrar tres deudas de perf: cache del fold NFC del quick search (#77), single-flight del indexado + caché del CD zip en vfs-archive (#61 partes 1 y 2), y listado lazy de vfs-local con hidratación en copy engine y TUI (#52).

**Architecture:** Tres partes INDEPENDIENTES, cada una en su rama sobre `main`, mergeada por separado (una PR = un propósito, <400 líneas netas). Parte A toca solo `norte-frontend` (+call-sites). Parte B toca `norte-testkit` (contador) + `norte-vfs-archive`. Parte C toca `norte-vfs-local` + `norte-core::ops` + `norte-tui`. Sin cambios de wire en ninguna (la parte 3 de #61 — skipped al frontend — queda FUERA: toca protocolo, issue aparte).

**Tech Stack:** Rust workspace, cargo nextest, tokio (Mutex/oneshot), zip 5.1.1, criterion (`just bench` como vara de #52).

**Método (memoria de sesiones previas):** controller = único escritor; los subagentes NO commitean — dejan cambios, el controller corre tests + commitea. `cargo llvm-cov clean` antes de medir cobertura. Reviewers antes de merge: rust-reviewer siempre; encoding-auditor en A (matching de texto) y B/C (providers); `just ci` verde antes de cada merge a main.

---

## Parte A — #77: cache del fold NFC del quick search

**Rama:** `feat/77-quick-fold-cache`

**Diseño:** el fold (lossy→NFC→lowercase→NFC) se precomputa UNA vez por mutación del listado (en `QuickSearch::new` y `refresh`, misma cadencia que el `sort_entries` de `extend`) y vive DENTRO de `QuickSearch` como `Vec<String>` índice-paralelo a `entries`. El camino caliente (keystroke: `push_char`/`backspace`) deja de foldear N entradas — solo foldea la query. NO se toca `Entry` (proto) ni `PaneState.entries`. `nav::matches` pública queda con la misma firma (conveniencia sin cache). NOTA: `norte-gui` está excluido del workspace y consume `norte-frontend` — el cambio de firma de `push_char`/`backspace` es deuda de sync GUI (anotarlo en el cuerpo del commit).

### Task A1: folds cacheados en `QuickSearch`

**Files:**
- Modify: `crates/norte-frontend/src/nav.rs`
- Modify: `crates/norte-frontend/src/pane.rs` (call-sites `quick_char`/`quick_backspace`)

- [ ] **Step 1: Tests primero (rojo por compilación).** En `crates/norte-frontend/src/nav.rs` mod tests: cambiar TODAS las llamadas `q.push_char(c, &entries)` → `q.push_char(c)` y `q.backspace(&entries)` → `q.backspace()` (tests afectados: `estado_filtro_navega_y_confirma`, `modo_salto_tab_con_wrap`, `reaplicar_tras_lote_nuevo_conserva_seleccion_si_sobrevive`, `refresh_sobrevive_a_un_resort`, `query_display_enmascara_hazards`). Añadir test nuevo:

```rust
#[test]
fn push_char_usa_los_folds_del_ultimo_refresh() {
    // El cache de folds (#77) debe renovarse en refresh: una entrada que
    // llega en un lote POSTERIOR tiene que casar con el siguiente keystroke.
    let mut entries = vec![e("mem:///zzz")];
    let mut q = QuickSearch::new(Mode::Filter, &entries);
    entries.push(e("mem:///nuevo.txt")); // lote del fill
    q.refresh(&entries, None);
    q.push_char('n');
    assert_eq!(q.visible(), &[1], "el fold de la entrada nueva está en el cache");
}
```

- [ ] **Step 2: Verificar que falla.** Run: `cargo nextest run -p norte-frontend` → FAIL de compilación (firmas viejas).

- [ ] **Step 3: Implementación en nav.rs.**

Añadir tras `fold`:

```rust
/// Folds precomputados de `entries` (índice-paralelo). Ver [`fold`].
fn fold_names(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .map(|e| fold(e.path.file_name().map_or(&b""[..], |s| s.as_bytes())))
        .collect()
}

/// Matching sobre folds YA precomputados (camino caliente del keystroke).
fn matches_folded(query_folded: &str, folds: &[String]) -> Vec<usize> {
    folds
        .iter()
        .enumerate()
        .filter(|(_, f)| f.contains(query_folded))
        .map(|(i, _)| i)
        .collect()
}
```

Reimplementar `matches` sobre las dos (misma firma pública, doc: conveniencia sin cache — el estado con cache es `QuickSearch`):

```rust
#[must_use]
pub fn matches(query: &[u8], entries: &[Entry]) -> Vec<usize> {
    matches_folded(&fold(query), &fold_names(entries))
}
```

`QuickSearch`: campo nuevo `folds: Vec<String>` (doc: «Claves de comparación por entrada, recomputadas UNA vez por mutación del listado (`new`/`refresh`), no por keystroke (#77)»). Cambios:

```rust
pub fn new(mode: Mode, entries: &[Entry]) -> Self {
    let mut q = Self {
        query: Vec::new(),
        mode,
        folds: fold_names(entries),
        visible: Vec::new(),
        pos: 0,
    };
    q.recompute();
    q
}

fn recompute(&mut self) {
    self.visible = if self.query.is_empty() {
        (0..self.folds.len()).collect()
    } else {
        matches_folded(&fold(&self.query), &self.folds)
    };
}

pub fn push_char(&mut self, c: char) { /* igual, sin param entries, recompute() */ }
pub fn backspace(&mut self) { /* igual, sin param entries, recompute() */ }

pub fn refresh(&mut self, entries: &[Entry], prev_selected: Option<&VPath>) {
    self.folds = fold_names(entries);
    self.recompute();
    // ... re-anclaje por identidad SIN CAMBIOS ...
}
```

Actualizar el docstring de `fold` (párrafo «Coste»): el fold se cachea en `QuickSearch::folds` (#77 cerrado), un recompute por mutación de listado; mantener EXPLÍCITA la nota de `to_lowercase` = case-folding simple, no full Unicode (decisión consciente, pedida en el issue).

- [ ] **Step 4: Call-sites en pane.rs.** `quick_char` y `quick_backspace` (≈pane.rs:166-174): quitar el argumento `&self.entries`. `refresh`/`refresh_quick` no cambian de firma. Verificar que no hay más callers: `grep -rn "push_char\|\.backspace(" crates/norte-frontend crates/norte-tui` → solo pane.rs + tests de nav.rs.

- [ ] **Step 5: Verde.** Run: `cargo nextest run -p norte-frontend -p norte-tui` → PASS. `cargo clippy -p norte-frontend -p norte-tui --all-targets -- -D warnings` + `cargo fmt --all`.

- [ ] **Step 6: Commit.**

```bash
git add crates/norte-frontend/src/nav.rs crates/norte-frontend/src/pane.rs
git commit -m "perf(frontend): #77 cachea el fold NFC del quick search por listado

El keystroke deja de foldear N entradas: los folds viven en QuickSearch
y se recomputan una vez por new/refresh (misma cadencia que el sort del
extend). to_lowercase sigue siendo case-folding simple, consciente.
Deuda: norte-gui (fuera del workspace) debe sincronizar las firmas de
push_char/backspace cuando se re-integre."
```

### Task A2: review + merge

- [ ] rust-reviewer + encoding-auditor sobre el diff (el fold es matching de texto sobre nombres hostiles; el corpus de nav.rs debe seguir verde tal cual — ninguna fixture cambia de resultado).
- [ ] `just ci` → EXIT=0. Merge a main (`git merge --no-ff feat/77-quick-fold-cache`). Cerrar #77 con `gh issue close 77 --comment "..."`.

---

## Parte B — #61.1+.2: single-flight del indexado + caché del ZipArchive

**Rama:** `feat/61-archive-index-perf`

**Diseño:** (1) single-flight: un lock async por clave de contenedor en `ArchiveProvider`; el primero construye, los concurrentes esperan y releen la caché (double-check). (2) el `ZipArchive` que `build_index` ya parseó se conserva junto al índice en la caché LRU (misma clave, misma generación) y cada `read` lo CLONA (`zip::ZipArchive` deriva Clone con `R: Clone` y comparte el CD parseado por `Arc` interno) — se elimina el `ZipArchive::new` por lectura. La parte 3 del issue (skipped al frontend) NO va aquí: toca protocolo.

### Task B1: contador de reads en testkit (observabilidad)

**Files:**
- Modify: `crates/norte-testkit/src/faults.rs`
- Modify: `crates/norte-testkit/src/mem.rs` (una línea en `read`)

- [ ] **Step 1: Test primero.** En `crates/norte-testkit/src/mem.rs` mod tests (o donde vivan los tests de Mem — buscar `mod tests` en mem.rs):

```rust
#[tokio::test]
async fn faults_cuenta_las_llamadas_a_read() {
    let mem = MemProvider::new();
    let p = MemProvider::root().join(Segment::new(b"f".to_vec()).expect("seg"));
    let mut sink = mem.write(&p).await.expect("write");
    sink.write(bytes::Bytes::from_static(b"data")).await.expect("chunk");
    sink.commit().await.expect("commit");
    let faults = mem.faults();
    assert_eq!(faults.read_calls(), 0);
    let _ = mem.read(&p, None).await.expect("read");
    let _ = mem.read(&p, None).await.expect("read");
    assert_eq!(faults.read_calls(), 2);
}
```

- [ ] **Step 2: Rojo.** `cargo nextest run -p norte-testkit` → FAIL compilación (`read_calls` no existe).

- [ ] **Step 3: Implementar.** En `FaultState`: campo `read_calls: u64`. En `Faults`:

```rust
/// Nº de llamadas a `Provider::read` atendidas (no bytes): observabilidad
/// para tests de coalescing/caché (#61).
#[must_use]
pub fn read_calls(&self) -> u64 {
    self.lock().read_calls
}

pub(crate) fn count_read(&self) {
    self.lock().read_calls += 1;
}
```

En `MemProvider::read` (mem.rs, el método async que arranca con `self.faults.op_gate().await?` cerca de la línea 600): añadir `self.faults.count_read();` justo tras el `op_gate`.

- [ ] **Step 4: Verde + commit.** `cargo nextest run -p norte-testkit` → PASS.

```bash
git add crates/norte-testkit/src/faults.rs crates/norte-testkit/src/mem.rs
git commit -m "feat(testkit): #61 contador de reads en Faults (observabilidad de cache)"
```

### Task B2: single-flight de `index_for`

**Files:**
- Modify: `crates/norte-vfs-archive/src/provider.rs`
- Test: `crates/norte-vfs-archive/tests/index_cache.rs` (nuevo)

- [ ] **Step 1: Test primero.** Nuevo `tests/index_cache.rs`. LEER ANTES `tests/common/mod.rs` (helpers `seed_container`/`zip_provider`, common/mod.rs:18-61) y `tests/contract.rs:87-92` (patrón `fresh_zip`) y calcar el seeding — el test necesita el handle de faults del MemProvider interior, así que si `zip_provider` no lo expone, construir el provider a mano igual que hace el helper pero conservando `Arc<MemProvider>`:

```rust
//! #61.1: N operaciones concurrentes sobre el mismo contenedor frío
//! construyen UN índice, no N (single-flight).
mod common;

use std::sync::Arc;
use norte_testkit::{MemProvider, ZipSmith};
use norte_vfs::Provider;

#[tokio::test(flavor = "multi_thread")]
async fn indexado_concurrente_coalesce_en_un_build() {
    let bytes = ZipSmith::new().file(b"a.txt", b"hola").build();
    // Seeding calcado de tests/common/mod.rs, conservando el Arc<MemProvider>
    // para leer los faults. Latencia por op: garantiza el solapamiento (los
    // 8 llegan al miss ANTES de que el primero termine de construir).
    let (provider, mem) = /* zip provider sobre MemProvider con el contenedor sembrado */;
    mem.faults().set_latency_per_op(Some(std::time::Duration::from_millis(5)));

    // Baseline: un build frío en solitario.
    let root = /* VPath raíz del archivo, mismo wire que usa contract.rs */;
    provider.list(&root).await.expect("list fría").collect::<Vec<_>>().await;
    let baseline = mem.faults().read_calls();

    // Provider FRESCO (caché vacía), mismos bytes: 8 lists concurrentes.
    let (provider2, mem2) = /* ídem */;
    mem2.faults().set_latency_per_op(Some(std::time::Duration::from_millis(5)));
    let p = Arc::new(provider2);
    let mut handles = Vec::new();
    for _ in 0..8 {
        let p = Arc::clone(&p);
        let root = root.clone();
        handles.push(tokio::spawn(async move {
            p.list(&root).await.expect("list").collect::<Vec<_>>().await;
        }));
    }
    for h in handles { h.await.expect("join"); }
    assert_eq!(
        mem2.faults().read_calls(),
        baseline,
        "8 lists concurrentes frías = los reads de UN solo build (single-flight)"
    );
}
```

(Los `/* ... */` se resuelven copiando el seeding real de common/mod.rs — es la única pieza dependiente del harness.)

- [ ] **Step 2: Rojo.** `cargo nextest run -p norte-vfs-archive indexado_concurrente` → FAIL (hoy 8 builds → read_calls > baseline).

- [ ] **Step 3: Implementar.** En `ArchiveProvider` (provider.rs:93-99), campo nuevo:

```rust
/// Single-flight de construcción de índice (#61): un builder por clave;
/// los concurrentes esperan el lock y releen la caché. El map se poda
/// cuando el último interesado suelta su Arc.
building: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
```

(inicializar `building: Mutex::new(HashMap::new())` en `with_limits`). En `index_for`, tras el fast-path de caché (provider.rs:165-170):

```rust
let build_lock = {
    let mut building = self.building.lock().expect("building lock sano");
    Arc::clone(
        building
            .entry(key.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    )
};
let build_guard = build_lock.lock().await;
// Double-check: otro caller pudo construir mientras esperábamos.
{
    let mut cache = self.cache.lock().expect("cache lock sano");
    if let Some(hit) = cache.get(&key, generation) {
        drop(build_guard);
        self.prune_building(&key, &build_lock);
        return Ok(hit);
    }
}
```

El build existente (spawn_blocking + guard de cancelación) queda igual. El manejo del resultado pasa a explícito para no fugar la entrada de `building` en el camino de error, y el `put` ocurre ANTES de soltar el guard (si no, un esperador despierta, hace double-check ANTES del put y reconstruye):

```rust
let index = match joined.map_err(|e| {
    if e.is_panic() {
        Error::Internal { panic: true }
    } else {
        Error::Cancelled
    }
}).and_then(|r| r) {
    Ok(i) => i,
    Err(e) => {
        drop(build_guard);
        self.prune_building(&key, &build_lock);
        return Err(e);
    }
};
let index = Arc::new(index);
if generation.0.is_some() {
    self.cache.lock().expect("cache lock sano").put(&key, Arc::clone(&index));
}
drop(build_guard);
self.prune_building(&key, &build_lock);
Ok(index)
```

Helper:

```rust
/// Poda la entrada de `building` si nadie más la retiene (2 = el map + el
/// caller). Si el caller muere cancelado con el guard tomado, la entrada
/// sobrevive hasta la próxima poda — acotado por claves activas, no fuga.
fn prune_building(&self, key: &str, lock: &Arc<tokio::sync::Mutex<()>>) {
    let mut building = self.building.lock().expect("building lock sano");
    if Arc::strong_count(lock) == 2 {
        building.remove(key);
    }
}
```

- [ ] **Step 4: Verde.** `cargo nextest run -p norte-vfs-archive` completo (contract + hostile + fuzz siguen verdes: single-flight no cambia semántica).

- [ ] **Step 5: Commit.**

```bash
git add crates/norte-vfs-archive/src/provider.rs crates/norte-vfs-archive/tests/index_cache.rs
git commit -m "perf(vfs-archive): #61 single-flight del indexado por contenedor

N operaciones concurrentes sobre el mismo contenedor frio esperaban antes
N builds (ultima put gana); ahora el primero construye y el resto relee la
cache (double-check bajo lock por clave, poda por strong_count)."
```

### Task B3: caché del `ZipArchive` (CD parseado) por generación

**Files:**
- Modify: `crates/norte-vfs-archive/src/blocking.rs` (Clone)
- Modify: `crates/norte-vfs-archive/src/zip_format.rs` (build_index devuelve el archive; read_entry lo recibe)
- Modify: `crates/norte-vfs-archive/src/provider.rs` (CachedContainer, read)
- Test: `crates/norte-vfs-archive/tests/index_cache.rs` (añadir)

- [ ] **Step 1: Test primero** (en index_cache.rs). Contenedor con `a.txt` pequeño (datos en el bloque 0) + `relleno.bin` de 300 KB stored (empuja el central directory más allá de BLOCK=256 KiB, bloque ≥1):

```rust
#[tokio::test(flavor = "multi_thread")]
async fn read_caliente_no_reparsea_el_central_directory() {
    let bytes = ZipSmith::new()
        .file(b"a.txt", b"hola")
        .file(b"relleno.bin", &vec![0u8; 300_000])
        .build();
    let (provider, mem) = /* seeding como arriba */;
    let entry_path = /* VPath de a.txt dentro del archivo */;
    // Calienta el índice (y con él, el CD cacheado).
    provider.stat(&entry_path).await.expect("stat");
    let antes = mem.faults().read_calls();
    let data: Vec<_> = provider.read(&entry_path, None).await.expect("read")
        .collect().await;
    assert!(data.iter().all(Result::is_ok));
    let delta = mem.faults().read_calls() - antes;
    // a.txt vive en el bloque 0; el CD vive en la cola (bloque >=1). Sin
    // cache, ZipArchive::new relee la cola en CADA read -> delta >= 2.
    assert_eq!(delta, 1, "read caliente = solo el bloque de datos, sin CD");
}
```

- [ ] **Step 2: Rojo.** `cargo nextest run -p norte-vfs-archive read_caliente` → FAIL (hoy delta ≥ 2).

- [ ] **Step 3: `ProviderReader: Clone`** (blocking.rs):

```rust
impl Clone for ProviderReader {
    /// Lector independiente sobre el MISMO contenedor: posición a 0 y caché
    /// de bloque VACÍA (clonar no arrastra hasta 256 KiB de bloque).
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            inner: Arc::clone(&self.inner),
            path: self.path.clone(),
            len: self.len,
            pos: 0,
            block: None,
        }
    }
}
```

- [ ] **Step 4: zip_format.** `build_index` devuelve el archive que ya parseó: firma → `Result<(ArchiveIndex, zip::ZipArchive<R>), Error>` (el `ZipArchive::new` interno de zip_format.rs:95 se conserva; al final se devuelve `(index, archive)`). `read_entry` deja de construirlo: firma → `pub(crate) fn read_entry<R: Read + Seek>(mut archive: zip::ZipArchive<R>, entry_index: usize, skip: u64, take: u64, tx: &...)` (borrar el `ZipArchive::new` de zip_format.rs:186-189). Helper nuevo para el fallback del provider (caché fría/generación desconocida):

```rust
/// Abre el archive en el hilo blocking; si el CD está roto, reporta por el
/// canal y devuelve None (el caller retorna).
pub(crate) fn open_archive<R: Read + Seek>(
    reader: R,
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>,
) -> Option<zip::ZipArchive<R>> {
    match zip::ZipArchive::new(reader) {
        Ok(a) => Some(a),
        Err(e) => {
            send_err(tx, corrupt(&e));
            None
        }
    }
}
```

Actualizar tests internos de zip_format que llamen a `build_index`/`read_entry` con las firmas nuevas (los de tar no cambian).

- [ ] **Step 5: provider.rs.** La caché pasa a guardar el archive junto al índice:

```rust
/// Índice + CD del zip parseado (clonable por read; None en tar). Misma
/// clave y misma generación: la invalidación existente los gobierna juntos.
#[derive(Clone)]
struct CachedContainer {
    index: Arc<ArchiveIndex>,
    zip: Option<zip::ZipArchive<ProviderReader>>,
}

// zip::ZipArchive comparte el CD por Arc interno; Clone con R: Clone es la
// base del cache (#61). Si una subida de `zip` lo rompe, que lo diga el
// compilador aquí y no un perf-regression silencioso.
const _: () = {
    const fn assert_clone<T: Clone>() {}
    assert_clone::<zip::ZipArchive<ProviderReader>>();
};
```

`IndexCache.map: HashMap<String, CachedContainer>`; `get`/`put` migran (la generación se lee de `hit.index.generation`). `index_for` devuelve `CachedContainer`; el closure del spawn_blocking pasa a devolver `Result<(ArchiveIndex, Option<zip::ZipArchive<ProviderReader>>), Error>` (brazo tar: `(idx, None)`; brazo zip: `(idx, Some(archive))`). Call-sites de `index_for` (`stat` 261, `list` 280, `read` 307, `read_link` 375): usar `.index` donde hoy usan el Arc directo. El brazo zip de `read` (provider.rs:350-368):

```rust
Locator::Zip { index: entry_index } => {
    let cached_zip = cached.zip.clone();
    let reader = ProviderReader::new(
        tokio::runtime::Handle::current(),
        Arc::clone(&self.inner),
        aref.outer.clone(),
        cached.index.generation.1.unwrap_or(0),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    drop(tokio::task::spawn_blocking(move || {
        let archive = match cached_zip {
            Some(a) => a, // CD ya parseado: clon barato, cero re-parse
            None => match crate::zip_format::open_archive(reader, &tx) {
                Some(a) => a,
                None => return,
            },
        };
        crate::zip_format::read_entry(archive, entry_index, req_off, req_len, &tx);
    }));
    Ok(futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed())
}
```

- [ ] **Step 6: Verde.** `cargo nextest run -p norte-vfs-archive` COMPLETO (contract, hostile_zip, zip_fuzz — la mitigación del colapso lossy y los límites anti-bomba no se tocan, deben seguir idénticos).

- [ ] **Step 7: Commit.**

```bash
git add crates/norte-vfs-archive/src crates/norte-vfs-archive/tests/index_cache.rs
git commit -m "perf(vfs-archive): #61 cachea el ZipArchive (CD parseado) por generacion

read() dejaba de pagar el indice pero re-parseaba el central directory
entero en CADA lectura (ZipArchive::new por read). El archive que
build_index ya parseo viaja con el indice en la cache LRU y cada read lo
clona (CD compartido por Arc interno; ProviderReader::clone = lector
independiente sin bloque). Fallback sin cache: open_archive en el hilo
blocking, como antes."
```

### Task B4: review + merge

- [ ] rust-reviewer (locks: ningún `std::sync::MutexGuard` cruza un await — los de `cache`/`building` son scoped; el guard async sí, es el punto) + encoding-auditor (no cambia decodificación de nombres — confirmar que el corpus hostil sigue byte-idéntico) + security-reviewer opcional (anti-DoS: `building` acotado por poda; latencia de esperadores acotada por el TTL natural del build).
- [ ] `just ci` → EXIT=0. Merge a main. `gh issue comment 61` (partes 1-2 hechas; parte 3 skipped-al-frontend queda abierta → retitular el issue o abrir uno nuevo y cerrar #61).

---

## Parte C — #52: listado lazy en vfs-local + hidratación

**Rama:** `feat/52-local-list-lazy`

**Diseño:** `list()` local deja de statear cada entrada: `kind` sale de `DirEntry::file_type()` (d_type de readdir en Linux; std cae a lstat solo con DT_UNKNOWN) y `size`/`mtime_ms` quedan `None` (`Entry` ya los tiene `Option`; contrato documentado: «None = el provider no lo sabe»). Dos consumidores se hidratan on-demand: (1) el copy engine statea las hojas File/Symlink del plan ANTES de `bytes_total` (la barra de progreso y `CollisionPolicy::Newer` y la conservación de mtime del symlink lo necesitan — ops.rs:411, :682, :1613); (2) el TUI lanza una sonda one-shot `backend.stat` para la entrada enfocada sin size (statusbar `selected_bytes`, main.rs:1565). Orden de commits: C1 (hidratación core — no-op mientras list siga trayendo size) → C2 (list lazy) → C3 (sonda TUI), para que ningún commit intermedio rompa el progreso.

### Task C1: hidratación de hojas en el copy engine

**Files:**
- Modify: `crates/norte-core/src/ops.rs`
- Test: donde vivan los tests de engine+local de norte-core (`ls crates/norte-core/tests/` y calcar el harness del test de copia existente)

- [ ] **Step 1: Implementar `hydrate_plan`** (ops.rs, junto a `copy_tree`):

```rust
/// #52: el listado local es lazy (size/mtime None). El progreso
/// (`bytes_total`), `CollisionPolicy::Newer` y la conservación del mtime
/// del symlink necesitan los metadatos ANTES de copiar: stat-ea SOLO las
/// hojas File/Symlink a las que les falte algo. Un stat fallido deja None
/// (el copy real reportará el error de verdad al tocar esa hoja): barra
/// subestimada ≠ copia rota.
async fn hydrate_plan(
    src: &dyn Provider,
    plan: &mut [PlanEntry],
    cancel: &CancellationToken,
) -> Result<(), Error> {
    for pe in plan.iter_mut() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let e = &mut pe.entry;
        if matches!(e.kind, EntryKind::File | EntryKind::Symlink)
            && (e.size.is_none() || e.mtime_ms.is_none())
            && let Ok(st) = src.stat(&e.path).await
        {
            e.size = e.size.or(st.size);
            e.mtime_ms = e.mtime_ms.or(st.mtime_ms);
        }
    }
    Ok(())
}
```

Llamarlo en los TRES call-sites de `copy_tree` (ops.rs:641, :655, :1276 — el move también), convirtiendo el plan en mutable:

```rust
let mut plan = plan_for(&*src, &from, opts, &ctx.cancel).await?;
hydrate_plan(&*src, &mut plan, &ctx.cancel).await?;
copy_tree(&src, &dst, &from, &to, &plan, opts, &observer, ctx).await
```

(ídem con `walk_following` en 640-641 y el sitio del move en ~1276; `copy_tree` sigue tomando `&[PlanEntry]`).

- [ ] **Step 2: Verde (regresión).** `cargo nextest run -p norte-core` → PASS (con Mem el listado trae size ⇒ hidratación no-op; nada cambia).

- [ ] **Step 3: Commit.**

```bash
git add crates/norte-core/src/ops.rs
git commit -m "feat(core): #52 hidrata size/mtime de hojas del plan antes de copiar

Prepara el listado lazy de vfs-local: bytes_total, Newer y el mtime del
symlink dejan de depender de que list() traiga metadatos. Cancelacion
chequeada en el loop (regla 3). No-op con providers que ya los traen."
```

### Task C2: list lazy en vfs-local

**Files:**
- Modify: `crates/norte-vfs-local/src/provider.rs:686-693`
- Modify: `crates/norte-vfs-local/tests/local.rs` (mtime test) + test nuevo
- Test integración: el de copia de C1 ahora con LocalProvider

- [ ] **Step 1: Tests primero.** En `crates/norte-vfs-local/tests/local.rs`:
  - `mtime_is_recent_and_positive` (línea ~231): la parte que asevera mtime EN EL LISTADO pasa a asertar `mtime_ms.is_none()`; la garantía de mtime vive en `stat` (mantener/añadir el assert vía `stat`).
  - Test nuevo:

```rust
#[tokio::test]
async fn list_es_lazy_y_stat_hidrata() {
    let (p, root) = provider();
    let f = child(&root, b"datos.bin");
    // ...escribir 5 bytes con el helper de escritura del fichero de tests...
    let entries: Vec<_> = p.list(&root).await.expect("list")
        .map(|r| r.expect("entry")).collect().await;
    let e = entries.iter().find(|e| e.path == f).expect("está");
    assert_eq!(e.kind, EntryKind::File);
    assert!(e.size.is_none() && e.mtime_ms.is_none(), "listado lazy (#52)");
    let st = p.stat(&f).await.expect("stat");
    assert_eq!(st.size, Some(5));
    assert!(st.mtime_ms.is_some());
}
```

  - `symlink_stat_never_follows` (~169): NO tocar los asserts de kind — deben seguir verdes (d_type reporta DT_LNK).

- [ ] **Step 2: Rojo.** `cargo nextest run -p norte-vfs-local list_es_lazy` → FAIL (hoy trae size).

- [ ] **Step 3: Implementar** (provider.rs:686-693):

```rust
let item = dent.map_err(|e| map_io(&e)).and_then(|d| {
    let seg = Segment::new(os_to_bytes(&d.file_name()))
        .map_err(|_| Error::InvalidPath)?;
    // #52: kind por d_type del readdir (std solo statea con DT_UNKNOWN);
    // size/mtime LAZY (None = "no lo sé", contrato de Entry) — el copy
    // engine hidrata sus hojas y la UI sondea la enfocada.
    let ft = d.file_type().map_err(|e| map_io(&e))?;
    let kind = if ft.is_symlink() {
        EntryKind::Symlink
    } else if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    Ok(Entry {
        path: base_vpath.join(seg),
        kind,
        size: None,
        mtime_ms: None,
    })
});
```

(`entry_from` queda como está: lo usa `stat`.)

- [ ] **Step 4: Test de integración bytes_total.** En norte-core tests (harness de engine+LocalProvider calcado del test de copia existente): copiar un dir con 2 archivos (3 y 4 bytes) creado en tempdir y asertar al terminar la Task que `bytes_total == Some(7)` en el TaskProgress final (el watch del scheduler) y que el contenido llegó byte-exacto. Este test PRUEBA la coordinación C1+C2: sin hydrate_plan daría `Some(0)`.

- [ ] **Step 5: Verde total + bench.** `cargo nextest run -p norte-vfs-local -p norte-core` → PASS. `just bench` → `cien_mil_drenado_completo` debe BAJAR de ~250 ms de forma drástica (esperado <50 ms); `cien_mil_hasta_primer_render` sin regresión (~0.65 ms). Anotar cifras en el commit.

- [ ] **Step 6: Commit.**

```bash
git add crates/norte-vfs-local/src/provider.rs crates/norte-vfs-local/tests/local.rs crates/norte-core/tests/
git commit -m "perf(vfs-local): #52 listado lazy: kind por d_type, size/mtime None

100k lstat fuera del drenado del listado: cien_mil_drenado_completo pasa
de ~250ms a <XX>ms (cien_mil_hasta_primer_render intacto). El copy engine
hidrata sus hojas (commit anterior); stat() sigue trayendo todo."
```

### Task C3: sonda stat on-focus en el TUI

**Files:**
- Modify: `crates/norte-frontend/src/pane.rs` (hydrate)
- Modify: `crates/norte-tui/src/app.rs` (delegación + focused_needs_stat)
- Modify: `crates/norte-tui/src/main.rs` (sonda + brazo del select)

- [ ] **Step 1: Test primero (frontend).** En pane.rs mod tests:

```rust
#[test]
fn hydrate_rellena_sin_reordenar_y_es_noop_si_no_esta() {
    // entries con size None; hydrate por path rellena; path desconocido no-op.
    // size/mtime no participan del sort: el orden no cambia.
}
```

(código completo calcando el estilo de los tests vecinos `extend_*` de pane.rs:678-729.)

- [ ] **Step 2: `PaneState::hydrate`** (pane.rs):

```rust
/// Hidrata size/mtime de la entrada `path` (stat on-demand, #52). No-op si
/// la entrada ya no está (un refresh la pisó). No reordena: size/mtime no
/// participan en el sort.
pub fn hydrate(&mut self, path: &VPath, size: Option<u64>, mtime_ms: Option<i64>) {
    if let Some(e) = self.entries.iter_mut().find(|e| &e.path == path) {
        e.size = e.size.or(size);
        e.mtime_ms = e.mtime_ms.or(mtime_ms);
    }
}
```

Delegación en `Pane` (app.rs, junto a `extend_listing` ~249): `pub fn hydrate(&mut self, path: &VPath, size: Option<u64>, mtime_ms: Option<i64>) { self.state.hydrate(path, size, mtime_ms); }`. Helper en `App`:

```rust
/// (índice, path) de la entrada File enfocada sin `size`: candidata a la
/// sonda de stat on-focus (#52).
#[must_use]
pub fn focused_needs_stat(&self) -> Option<(usize, VPath)> {
    let e = self.focused().selected()?;
    (e.kind == EntryKind::File && e.size.is_none())
        .then(|| (self.focus(), e.path.clone()))
}
```

- [ ] **Step 3: Sonda en main.rs.** Struct + spawn junto a `Fill` (~main.rs:92-105):

```rust
/// Sonda one-shot de stat on-focus (#52): hidrata size/mtime de la entrada
/// seleccionada cuando el listado lazy los dejó en None. A lo sumo UNA en
/// vuelo; dedup por path (un stat fallido no se reintenta hasta cambiar la
/// selección — sin martillear un provider roto).
struct StatProbe {
    pane: usize,
    path: VPath,
    rx: tokio::sync::oneshot::Receiver<Option<Entry>>,
}

fn spawn_stat_probe(backend: &Backend, pane: usize, path: VPath) -> StatProbe {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    let p = path.clone();
    tokio::spawn(async move {
        let _ = tx.send(b.stat(&p).await.ok());
    });
    StatProbe { pane, path, rx }
}
```

En el run loop (~main.rs:420): locals `let mut stat_probe: Option<StatProbe> = None; let mut last_probed: Option<VPath> = None;`. Antes del `tokio::select!` (tras el draw):

```rust
// #52: listado lazy — la entrada enfocada sin size se hidrata con una
// sonda one-shot (máx. una en vuelo; dedup por path).
if stat_probe.is_none()
    && let Some((pane_idx, path)) = app.focused_needs_stat()
    && last_probed.as_ref() != Some(&path)
{
    stat_probe = Some(spawn_stat_probe(backend, pane_idx, path.clone()));
    last_probed = Some(path);
}
```

Brazo nuevo del select (patrón de los brazos `Option` existentes, main.rs:495-505):

```rust
res = async {
    match &mut stat_probe {
        Some(pr) => (&mut pr.rx).await.ok().flatten(),
        None => std::future::pending().await,
    }
} => {
    if let Some(pr) = stat_probe.take()
        && let Some(entry) = res
    {
        app.panes[pr.pane].hydrate(&pr.path, entry.size, entry.mtime_ms);
    }
}
```

- [ ] **Step 4: Verde.** `cargo nextest run -p norte-frontend -p norte-tui` → PASS. Clippy + fmt workspace.

- [ ] **Step 5: Prueba manual** (skill `run` si hace falta): abrir la TUI en un dir grande; la statusbar muestra el size de la entrada enfocada al posarse (antes: instantáneo por el listado; ahora: un frame después vía sonda).

- [ ] **Step 6: Commit.**

```bash
git add crates/norte-frontend/src/pane.rs crates/norte-tui/src/app.rs crates/norte-tui/src/main.rs
git commit -m "feat(tui): #52 sonda stat on-focus hidrata la entrada seleccionada

selected_bytes de la statusbar vuelve a ser real con el listado lazy:
una sonda one-shot por seleccion (dedup por path, max una en vuelo) via
Backend::stat — funciona en embebido y remoto (fs.stat ya existe en wire)."
```

### Task C4: review + merge

- [ ] rust-reviewer (regla 2: ningún std::fs nuevo fuera de vfs-local; regla 3: cancel en hydrate_plan) + encoding-auditor (list local: `os_to_bytes` intacto, cero decodificación nueva) + test-engineer si el harness de progreso se resiste.
- [ ] `just ci` → EXIT=0 (ojo `cargo llvm-cov clean` si la cobertura da números raros). Merge a main. Cerrar #52 con las cifras del bench.

---

## Self-review (hecho al escribir)

- **Cobertura**: #77 completo (cache + doc lowercase). #61: partes 1 y 2; parte 3 explícitamente fuera (wire → issue aparte al cerrar). #52: los 3 frentes del issue (d_type, lazy, coordinación bytes_total) + Newer/symlink-mtime detectados en ops.rs:411/:1613 que el issue no mencionaba.
- **Tipos**: `read_calls()` (B1) usado en B2/B3; `CachedContainer.index/.zip` consistente en B3; `hydrate` (C3) misma firma en PaneState/Pane; `hydrate_plan` toma `&mut [PlanEntry]` y los callers construyen `let mut plan`.
- **Huecos asumidos**: seeding exacto del harness archive (leer common/mod.rs en B2 Step 1) y helper de escritura en local.rs (C2 Step 1) — referenciados a código existente, no inventados.
