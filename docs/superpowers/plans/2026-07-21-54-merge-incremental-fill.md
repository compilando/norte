# #54 Merge incremental del fill con claves persistidas — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminar los ~24 re-sorts completos del fill paginado: `PaneState::extend` pasa a un merge O(n+m) de dos runs ordenados con las claves NFC PERSISTIDAS junto a las entries.

**Architecture:** `PaneState` gana `sort_keys: Vec<SortKey>` índice-paralelo a `entries` (clave = `(not_dir, nfc)`; el desempate por bytes crudos se lee del propio `Entry`, sin tercera alloc). `new`/`set_listing`/`refill` normalizan internamente (computan claves + ordenan — el contrato "ordénalas antes" del caller deja de ser footgun); `extend` ordena SOLO el lote y mergea establemente (izquierda gana en empate = mismo orden que el sort estable actual). `sort_entries` pública queda intacta (GUI y primer render la siguen usando). Bench NUEVO que mide el camino real de extend por lotes (el bench existente drena+ordena UNA vez y no captura los re-sorts — el issue exige medir antes).

**Tech Stack:** norte-frontend (pane.rs, sort.rs), bench criterion en norte-tui (presupuestos.rs). Sin deps nuevas, sin wire.

**Método:** rama `feat/54-merge-incremental-fill` sobre main. Subagentes NO commitean; controller corre `just ci` y commitea. Reviewers: rust-reviewer + encoding-auditor (la clave NFC es zona de encoding).

---

### Task 1: Bench del camino extend (baseline ANTES de optimizar)

**Files:**
- Modify: `crates/norte-tui/benches/presupuestos.rs`

- [ ] **Step 1: Leer** `presupuestos.rs` entero (grupo `bench_list_100k`, helpers `listar`/`primera_pagina`, `FILL_BATCH` — si `FILL_BATCH=4096` vive en main.rs y no es accesible, usa el literal 4096 con comentario).

- [ ] **Step 2: Bench nuevo** en el mismo grupo (o grupo hermano `bench_fill_100k` si el setup FS no aplica — este bench es PURO CPU, sin FS):

```rust
/// #54: coste TOTAL del camino extend por lotes (lo que el bench de drenado
/// no captura: ahí se ordena UNA vez al final; el fill real re-ordenaba en
/// cada lote). 100k entries sintéticas en lotes de 4096 → ~24 extends.
fn bench_extend_100k(c: &mut Criterion) {
    use norte_frontend::PaneState;
    let dir = VPath::parse("mem:///bench").expect("wire");
    let all: Vec<Entry> = (0..100_000)
        .map(|i| Entry {
            // Mezcla dirs/files y nombres desordenados (peor caso del merge
            // que el orden de llegada del FS, ya semi-ordenado).
            path: VPath::parse(&format!("mem:///bench/f{:06}", (i * 7919) % 100_000))
                .expect("wire"),
            kind: if i % 8 == 0 { EntryKind::Dir } else { EntryKind::File },
            size: None,
            mtime_ms: None,
        })
        .collect();
    let mut group = c.benchmark_group("fill");
    group.sample_size(10);
    group.bench_function("cien_mil_extend_por_lotes", |b| {
        b.iter(|| {
            let mut pane = PaneState::new(dir.clone(), Vec::new());
            for chunk in all.chunks(4096) {
                pane.extend(chunk.to_vec());
            }
            std::hint::black_box(pane.entries().len())
        });
    });
    group.finish();
}
```

(Ajusta imports/criterion_group al estilo del fichero; si `PaneState`/`Entry` no están ya importados, añádelos. `norte-frontend` es dep de norte-tui — disponible.)

- [ ] **Step 3: Baseline.** Run: `cargo bench -p norte-tui --bench presupuestos -- cien_mil_extend` → anota el número (esperado: cientos de ms — ~24 sorts de tamaño creciente con claves recomputadas).

- [ ] **Step 4: Commit** (bench solo, aún sin optimización):

```bash
git add crates/norte-tui/benches/presupuestos.rs
git commit -m "bench(tui): #54 mide el camino extend por lotes (baseline pre-merge)"
```

### Task 2: Claves persistidas + merge en norte-frontend

**Files:**
- Modify: `crates/norte-frontend/src/sort.rs`
- Modify: `crates/norte-frontend/src/pane.rs`

- [ ] **Step 1: Tests primero** (pane.rs mod tests, junto a `extend_reordena_y_reancla_por_path`):

```rust
#[test]
fn extend_merge_equivale_a_sort_completo() {
    // #54: el merge incremental produce EXACTAMENTE el mismo orden que
    // sort_entries sobre el total (dirs primero, NFC, empate por bytes) —
    // incluidos NFD/NFC mezclados y no-UTF8.
    let lotes: Vec<Vec<Entry>> = vec![
        vec![e("mem:///zeta", EntryKind::File), e("mem:///Adir", EntryKind::Dir)],
        vec![e("mem:///an%CC%83o", EntryKind::File)], // NFD
        vec![e("mem:///a%C3%B1o2", EntryKind::File), e("mem:///%FF%FE", EntryKind::File)], // NFC + no-UTF8
        vec![e("mem:///Bdir", EntryKind::Dir)],
    ];
    let mut pane = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
    for lote in lotes.clone() {
        pane.extend(lote);
    }
    let mut plano: Vec<Entry> = lotes.into_iter().flatten().collect();
    crate::sort_entries(&mut plano);
    assert_eq!(pane.entries(), plano.as_slice(), "merge ≡ sort completo");
}

#[test]
fn set_listing_normaliza_aunque_llegue_desordenado() {
    // El contrato "ordénalas antes" deja de ser footgun: set_listing/new
    // normalizan internamente (claves + orden) — un caller desordenado ya
    // no rompe el invariante del merge.
    let mut pane = PaneState::new(VPath::parse("mem:///").unwrap(), Vec::new());
    pane.set_listing(
        VPath::parse("mem:///d").unwrap(),
        vec![e("mem:///d/z", EntryKind::File), e("mem:///d/a", EntryKind::File)],
    );
    assert_eq!(pane.entries()[0].path, VPath::parse("mem:///d/a").unwrap());
    // Y el extend posterior sigue mergeando bien sobre esa base.
    pane.extend(vec![e("mem:///d/m", EntryKind::File)]);
    let names: Vec<_> = pane.entries().iter().map(|x| x.path.clone()).collect();
    assert_eq!(names, vec![
        VPath::parse("mem:///d/a").unwrap(),
        VPath::parse("mem:///d/m").unwrap(),
        VPath::parse("mem:///d/z").unwrap(),
    ]);
}
```

Añade también un caso de EMPATE estable (dos entries con la misma clave NFC en lotes distintos — p. ej. `an%CC%83o` NFD en el lote 1 y `a%C3%B1o` NFC en el lote 2: misma nfc_key, distinto byte crudo → el orden lo decide el desempate por bytes, igual que sort_entries; y si los bytes también empatan es la misma entry — no aplica).

- [ ] **Step 2: Rojo.** `cargo nextest run -p norte-frontend extend_merge` → FAIL de compilación o de orden.

- [ ] **Step 3: sort.rs — clave persistible + merge.**

```rust
/// Clave de orden PERSISTIBLE de una entry (#54): grupo (dirs primero) +
/// forma NFC del nombre. El desempate por bytes crudos NO se materializa —
/// se lee del propio `Entry` al comparar (una alloc menos por entrada).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SortKey {
    not_dir: bool,
    nfc: Vec<u8>,
}

pub(crate) fn sort_key(e: &Entry) -> SortKey {
    SortKey {
        not_dir: e.kind != EntryKind::Dir,
        nfc: nfc_key(name_bytes(e)),
    }
}

/// Comparación total (clave, entry): clave y, en empate, bytes crudos del
/// nombre — EXACTAMENTE el mismo orden que [`sort_entries`].
pub(crate) fn cmp_keyed(a: (&SortKey, &Entry), b: (&SortKey, &Entry)) -> std::cmp::Ordering {
    (a.0.not_dir, &a.0.nfc)
        .cmp(&(b.0.not_dir, &b.0.nfc))
        .then_with(|| name_bytes(a.1).cmp(name_bytes(b.1)))
}

/// Ordena `entries` computando sus claves UNA vez y devuelve ambas
/// (índice-paralelas). Estable, mismo orden que [`sort_entries`].
pub(crate) fn sort_with_keys(entries: Vec<Entry>) -> (Vec<Entry>, Vec<SortKey>) {
    let mut pares: Vec<(SortKey, Entry)> =
        entries.into_iter().map(|e| (sort_key(&e), e)).collect();
    pares.sort_by(|a, b| cmp_keyed((&a.0, &a.1), (&b.0, &b.1)));
    pares.into_iter().map(|(k, e)| (e, k)).unzip()
}

/// Merge ESTABLE de dos runs ordenados (izquierda gana el empate — el run
/// existente conserva su posición relativa, como el sort estable de antes).
/// O(n+m) sin recomputar claves.
pub(crate) fn merge_keyed(
    entries: &mut Vec<Entry>,
    keys: &mut Vec<SortKey>,
    batch_entries: Vec<Entry>,
    batch_keys: Vec<SortKey>,
) {
    let mut out_e = Vec::with_capacity(entries.len() + batch_entries.len());
    let mut out_k = Vec::with_capacity(keys.len() + batch_keys.len());
    let mut left = std::mem::take(entries).into_iter().zip(std::mem::take(keys)).peekable();
    let mut right = batch_entries.into_iter().zip(batch_keys).peekable();
    loop {
        match (left.peek(), right.peek()) {
            (Some(l), Some(r)) => {
                // Izquierda gana el empate (estabilidad).
                if cmp_keyed((&l.1, &l.0), (&r.1, &r.0)) != std::cmp::Ordering::Greater {
                    let (e, k) = left.next().expect("peek == Some");
                    out_e.push(e);
                    out_k.push(k);
                } else {
                    let (e, k) = right.next().expect("peek == Some");
                    out_e.push(e);
                    out_k.push(k);
                }
            }
            (Some(_), None) => {
                for (e, k) in left.by_ref() {
                    out_e.push(e);
                    out_k.push(k);
                }
            }
            (None, Some(_)) => {
                for (e, k) in right.by_ref() {
                    out_e.push(e);
                    out_k.push(k);
                }
            }
            (None, None) => break,
        }
    }
    *entries = out_e;
    *keys = out_k;
}
```

(El `expect("peek == Some")` es invariante comentada — permitido. Si clippy protesta por el loop, reescribe con `match` sobre `(left.peek(), right.peek())` como arriba o su forma idiomática; `sort_entries` NO se toca.)

Añade en sort.rs un test unitario de equivalencia propietaria:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // proptest si el crate ya lo tiene como dev-dep; si no, test determinista
    // con un vector fijo que cubra: dirs/files, NFD vs NFC, no-UTF8, empates.
    #[test]
    fn merge_keyed_equivale_a_sort_entries() { /* dos mitades desordenadas →
        sort_with_keys cada una → merge_keyed → comparar contra sort_entries
        del total */ }
}
```

(Si `proptest` está en dev-deps de norte-frontend, hazlo proptest: lotes arbitrarios de nombres del generador de bytes → equivalencia. Verifica con `grep proptest crates/norte-frontend/Cargo.toml`.)

- [ ] **Step 4: pane.rs — claves persistidas.**

`PaneState` gana campo:

```rust
/// Claves de orden persistidas, índice-paralelas a `entries` (#54): el
/// fill mergea lotes O(n+m) sin recomputar la clave NFC de lo ya listado.
sort_keys: Vec<SortKey>,
```

- `new(dir, entries)`: `let (entries, sort_keys) = crate::sort::sort_with_keys(entries);` (normaliza — actualiza el rustdoc: ya no exige orden previo, lo garantiza).
- `set_listing`: ídem (normaliza internamente).
- `begin_loading`: `self.sort_keys = Vec::new();`.
- `extend`:

```rust
pub fn extend(&mut self, batch: Vec<Entry>) {
    if batch.is_empty() {
        return;
    }
    let quick_prev = self.quick_selected_path();
    let anchor = self.entries.get(self.cursor).map(|e| e.path.clone());
    let (batch, batch_keys) = crate::sort::sort_with_keys(batch);
    crate::sort::merge_keyed(&mut self.entries, &mut self.sort_keys, batch, batch_keys);
    // ... re-anclaje del cursor + q.refresh + quick_sync_jump SIN CAMBIOS ...
}
```

(actualiza el rustdoc de extend: merge O(n+m) con claves persistidas, mismo orden que sort_entries.)

- `refill(entries)`: normaliza también (`sort_with_keys`) — OJO: hoy `refill` NO ordenaba (asumía caller); verifica los callers de refill (grep en tui/gui… gui fuera del workspace) para confirmar que le pasan listados ya ordenados — normalizar es idempotente y cierra el footgun. Mantén la semántica del cursor por índice.
- `hydrate`: sin cambios (size/mtime no participan en la clave).
- Cualquier otro sitio que mute `entries` directamente: grep `self.entries` en pane.rs y confirma que todos mantienen `sort_keys` en sync (si aparece uno nuevo, sincronízalo).

- [ ] **Step 5: Verde.** `cargo nextest run -p norte-frontend -p norte-tui -p norte-cli` (el CLI no usa PaneState, pero barato). `cargo clippy --workspace --all-targets -- -D warnings`; `cargo fmt --all`.

- [ ] **Step 6: Bench después.** `cargo bench -p norte-tui --bench presupuestos -- cien_mil` → anota `cien_mil_extend_por_lotes` (esperado: caída drástica — de O(Σ nᵢ log nᵢ) con claves recomputadas a O(Σ nᵢ) con claves persistidas) y verifica `cien_mil_hasta_primer_render` y `cien_mil_drenado_completo` sin regresión.

- [ ] **Step 7: Commit.**

```bash
git add crates/norte-frontend/src/sort.rs crates/norte-frontend/src/pane.rs
git commit -m "perf(frontend): #54 merge incremental del fill con claves NFC persistidas

extend deja de re-ordenar el listado entero por lote: las claves
(not_dir, nfc) viven junto a entries y cada lote se ordena solo y se
mergea O(n+m) estable (izquierda gana el empate = mismo orden que el
sort estable). new/set_listing/refill normalizan internamente (el
contrato 'ordenalas antes' deja de ser footgun). Bench
cien_mil_extend_por_lotes: <ANTES> -> <DESPUES>."
```

### Task 3: Review + merge

- [ ] rust-reviewer (memoria: +1 Vec<u8> persistido por entrada — acotado y menor que la 3-tupla que recomputaba el sort; estabilidad del merge; ningún camino muta entries sin keys) + encoding-auditor (nfc_key intacta byte-a-byte; el desempate por bytes crudos se conserva; corpus NFD/NFC/no-UTF8 en el test de equivalencia).
- [ ] `just ci` EXIT=0. Merge a main. Cerrar #54 con los números del bench (antes/después).

## Self-review

- Cobertura: issue pide merge O(n+m) con claves persistidas + medir antes (Task 1 baseline) — cubierto. `sort_entries` pública intacta (GUI/primer render). Estabilidad = orden idéntico verificado por test de equivalencia.
- Tipos: `SortKey`/`sort_with_keys`/`merge_keyed`/`cmp_keyed` consistentes entre sort.rs y pane.rs; `pane.entries()` accessor existente para los asserts.
- Riesgo señalado: `refill`/`new`/`set_listing` pasan a normalizar internamente (cambio de contrato benigno-idempotente); callers verificados en Task 2 Step 4.
