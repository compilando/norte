# Live search (Alt+F7) — plan de implementación

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `Alt+F7` busca por nombre (glob/regex) y contenido (literal multi-encoding / regex) bajo el subtree del pane, streaming a un pane virtual operable; `fs.search` = Task cancelable + notificación `search.hits` SOLO al dueño.

**Architecture:** proto 0.18.0 (`fs.search`, `search.hits`, `TaskKind::Search`); matchers puros en `norte-core/src/search.rs` (aguja transcodificada, jamás el pajar); `Engine::search` devuelve `(TaskHandle, mpsc::Receiver<SearchHits>)` — embebido consume el canal directo, el daemon lo bombea como notif al conn dueño; `Backend::search` simétrico (remoto enruta por task_id en pump_loop, patrón watches/approvals); TUI reusa el molde `Fill` para drenar hits a `extend_listing`. Spec: `docs/superpowers/specs/2026-07-18-live-search-design.md`.

**Tech Stack:** deps nuevas `regex` + `globset` + `memchr` (familia ripgrep, MIT/Apache — el workspace ya las arrastra transitivas; pasan a directas con justificación en Cargo.toml); norte-encoding (`detect`, `reload_cycle`).

**Convenciones (cada task):** TDD rojo verificado → verde → clippy `-D warnings` + `cargo fmt --all` → commit convencional, español, sin unwrap fuera de tests, UI por Fluent. **Regla de la casa: T1 toca norte-proto → protocol-guardian OBLIGATORIO antes de seguir (el controller lo orquesta tras el commit de T1).**

**Datos del recon (verificados, úsalos tal cual):** `PROTOCOL_VERSION` en methods.rs:77 (test congelado en golden_types.rs:1029; goldens en `tests/golden/types/methods.json`, contador 58 en golden_types.rs:357, familia fs en `check_methods_fs`:660, nombres en `method_names_frozen`:990). `TaskKind` task.rs:45 con `#[serde(other)] Unknown` (añadir variante = bump, ventana N-1 pasa a 0.17.x). Notifs: patrón `PolicyApprovalRequired` methods.rs:574 + `wire::Notification` envelope.rs:109. Engine: `copy_with_as` engine.rs:351 (gate → provider_for → sched.submit con TaskBody; TaskCtx{cancel, progress, actor} scheduler.rs:32; ProgressReporter::update progress.rs:60 coalescido 33ms). `Provider::list -> EntryStream`, `read -> ByteStream` (provider.rs:90/97). Daemon: `dispatch_fs_task` server.rs:1975, `register_task` 2184 (bomba de progreso 2242 — serializa `task.progress` y `broadcast_task_progress` por owner), `Subscriber{tx, actor}` server.rs:215 con conn_id como CLAVE del map subscribers (server.rs:142) — **no hay envío dirigido por conn_id: se añade en T4**. Backend: `Inner` backend.rs:739 (watches por task_id 745, approvals_tx 755), `pump_loop` 1431, `own_task` 1244, `TaskRef` 30-89, canales por-instancia `take_*` 785-794 (clon nace None). TUI: bomba approvals main.rs:391-401; molde streaming `FillMsg`/`Fill`/`spawn_fill` main.rs:58/69/1952 + `extend_listing`; input editable = patrón `NavPopup.name_input` app.rs:458. norte-encoding: `detect(bytes)->Detection{Text{encoding,bom}|Binary}` lib.rs:38 (binario = ≥1 NUL sin BOM), `reload_cycle()` lib.rs:199 = los 10 encodings candidatos, `decode` lib.rs:84.

---

### Task 1: proto 0.18.0 — `fs.search` + `search.hits` + `TaskKind::Search`

**Files:**
- Modify: `crates/norte-proto/src/methods.rs`, `crates/norte-proto/src/task.rs`, `crates/norte-proto/tests/golden_types.rs`, `crates/norte-proto/tests/golden/types/methods.json`

- [ ] **Step 1: tipos** (methods.rs, junto a la familia fs):

```rust
/// `fs.search` — búsqueda viva bajo un subtree (spec §17.1a): nombre por
/// glob O regex, contenido por literal O regex. Devuelve una Task
/// (`TaskKind::Search`); los hits llegan por la notificación
/// [`SEARCH_HITS`] SOLO a la conexión que la lanzó. Cancelable con
/// `task.cancel`. Al menos un criterio; glob y regex EXCLUYENTES por eje.
pub const FS_SEARCH: &str = "fs.search";
/// `search.hits` — notificación server→client con un LOTE de resultados.
pub const SEARCH_HITS: &str = "search.hits";
/// Tope de entries por notificación `search.hits` (coalescing server-side).
pub const SEARCH_HITS_MAX_BATCH: usize = 256;

/// Params de [`FS_SEARCH`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsSearchParams {
    /// Raíz del walk (subtree entero).
    pub root: VPath,
    /// Glob sobre el NOMBRE (último segmento), p.ej. `*.rs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_glob: Option<String>,
    /// Regex sobre el nombre. Excluyente con `name_glob`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_regex: Option<String>,
    /// Texto literal a buscar en el CONTENIDO (multi-encoding: la aguja se
    /// transcodifica, el pajar jamás se decodifica entero).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Regex sobre el contenido (solo ficheros que el detector dé como
    /// texto). Excluyente con `content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_regex: Option<String>,
    /// Sensible a mayúsculas (default false; el matching de nombre es
    /// sobre el lossy en NFC — misma disciplina que el quick search).
    #[serde(default)]
    pub case_sensitive: bool,
    /// Tope de hits: alcanzado, la Task completa con `truncated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hits: Option<u32>,
}

/// Un lote de resultados de [`SEARCH_HITS`]. `matches` alineado 1:1 con
/// `entries` cuando la búsqueda es de contenido (None si es solo nombre).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHits {
    /// Task dueña (correlación con `fs.search` → `task_id`).
    pub task_id: TaskId,
    /// Entradas que casan.
    pub entries: Vec<Entry>,
    /// Contexto del match de contenido, alineado con `entries`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matches: Option<Vec<MatchInfo>>,
}

/// Contexto de UN match de contenido.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchInfo {
    /// Línea (1-based) del primer match, si se computó.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// La línea del match decodificada lossy y RECORTADA server-side
    /// (tope fijo; el TUI la sanea igualmente con `detail_for_bar`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}
```
Resultado de `fs.search` = el `FsTaskResult` EXISTENTE (`{task_id}`, methods.rs:354) — documentarlo en el rustdoc de FS_SEARCH, cero struct nuevo.

- [ ] **Step 2: TaskKind::Search** (task.rs:45): variante `Search` con rustdoc y el MISMO aviso de compat que `Undo` (task.rs:54-62: clientes 0.17.x la ven como `Unknown`).
- [ ] **Step 3: bump** — methods.rs:77 → `"0.18.0"` + bloque de changelog en el docstring («0.18.0 (M4 live search): fs.search + search.hits + TaskKind::Search»). Ventana N-1 → 0.17.x (busca dónde se testea la ventana: grep `N-1\|0.17` en daemon/proto tests y ajusta como hicieron los bumps anteriores — `git log --oneline --grep="0.17.0"` enseña el patrón).
- [ ] **Step 4: goldens** — `method_names_frozen` += FS_SEARCH/SEARCH_HITS; golden_types.rs:1029 → 0.18.0; `check_methods_fs` (:660) += casos `fs_search_params` (con TODOS los Option poblados), `search_hits` (con matches), `match_info`; fixture JSON correspondiente en methods.json (contador 58 → el que dé; ajústalo en :357). Corre `cargo nextest run -p norte-proto` ROJO antes de escribir las fixtures (el contador y frozen fallan), luego verde. También `task_kind` goldens si existen (grep task_kind en golden_types.rs).
- [ ] **Step 5:** `cargo nextest run -p norte-proto` + fuzz de framing si es rápido (`cargo nextest run -p norte-proto framing`) + clippy + fmt.
- [ ] **Step 6: Commit** — `feat(proto): fs.search + search.hits + TaskKind::Search, bump 0.18.0 (liveSearch T1)`

**El controller pasa protocol-guardian sobre este commit ANTES de T2.**

---

### Task 2: matchers puros — `norte-core/src/search.rs` (deps nuevas)

**Files:**
- Modify: `Cargo.toml` (workspace), `crates/norte-core/Cargo.toml`, `deny.toml` si hace falta (no debería: MIT/Apache ya permitidas)
- Create: `crates/norte-core/src/search.rs` (+ `pub mod search;` en lib.rs)

- [ ] **Step 1: deps** en `[workspace.dependencies]` con comentario de justificación (regla 8: familia ripgrep, mantenida, sin unsafe relevante; ya transitivas en el árbol):
```toml
regex = "1"      # matchers de fs.search (nombre/contenido); size_limit anti-ReDoS
globset = "0.4"  # glob de nombre de fs.search (la lib de ripgrep; glob a mano = bugs)
memchr = "2"     # memmem para la aguja multi-encoding de fs.search
```
`norte-core/Cargo.toml`: los tres `.workspace = true`. `norte-encoding` ya es dep de… VERIFICA (`grep norte-encoding crates/norte-core/Cargo.toml`); si no lo es, añádela (workspace-interna, permisiva).
`cargo deny check licenses` → ok.

- [ ] **Step 2: tests rojos** (mod tests de search.rs). Matcher de NOMBRE:
```rust
#[test]
fn glob_e_insensibilidad_nfc() {
    let m = NameMatcher::glob("*.RS", false).expect("glob");
    assert!(m.matches(b"main.rs"));
    // NFD vs NFC: "año.rs" con la ñ descompuesta casa con el glob "a\u{00F1}o*".
    let m = NameMatcher::glob("año*", false).expect("glob");
    assert!(m.matches("an\u{0303}o.rs".as_bytes()));
    // Bytes no-UTF8: no panic, matchea sobre el lossy.
    let m = NameMatcher::glob("*", false).expect("glob");
    assert!(m.matches(b"\xFF\xFE"));
}

#[test]
fn regex_de_nombre_con_size_limit() {
    assert!(NameMatcher::regex("^ma.n\\.rs$", false).expect("re").matches(b"main.rs"));
    // Regex bomba: el size_limit la rechaza al COMPILAR, no cuelga.
    assert!(NameMatcher::regex(&"(a|aa)".repeat(2000), false).is_err());
}

#[test]
fn aguja_literal_multiencoding() {
    let n = ContentNeedle::literal("año", false);
    // UTF-8:
    assert!(n.find_in(&mut Overlap::default(), "hay un año aquí".as_bytes()).is_some());
    // Latin-1 (0xF1 = ñ):
    assert!(n.find_in(&mut Overlap::default(), b"hay un a\xF1o aqu\xED").is_some());
    // Y en dos chunks partiendo la aguja por la mitad (solape):
    let mut ov = Overlap::default();
    assert!(n.find_in(&mut ov, "hay un a".as_bytes()).is_none());
    assert!(n.find_in(&mut ov, "\u{00F1}o aqu\u{00ED}".as_bytes()).is_some());
}

#[test]
fn case_insensitive_de_contenido_ascii() {
    let n = ContentNeedle::literal("AÑO", false);
    assert!(n.find_in(&mut Overlap::default(), "el año".as_bytes()).is_some());
}
```
Confirma FAIL de compilación.

- [ ] **Step 3: implementación** (contratos exactos; el detalle mecánico contra las APIs de globset/regex/memchr):
```rust
//! Matchers PUROS de fs.search (spec 2026-07-18 live search): sin I/O, sin
//! Tasks. Nombre: lossy→NFC (+lowercase si case-insensitive) contra glob o
//! regex — misma disciplina que el quick search del TUI (identidad jamás se
//! normaliza; esto es matching de display). Contenido: la AGUJA se
//! transcodifica a los encodings candidatos de norte-encoding
//! (`reload_cycle()`), el pajar se busca bytes-contra-bytes con memmem por
//! chunks con SOLAPE — jamás decodificar el fichero entero (spec §17.1a).

pub struct NameMatcher { /* Glob(globset::GlobMatcher) | Regex(regex::Regex), fold interno */ }
impl NameMatcher {
    pub fn glob(pattern: &str, case_sensitive: bool) -> Result<Self, SearchError>;
    pub fn regex(pattern: &str, case_sensitive: bool) -> Result<Self, SearchError>;  // RegexBuilder con .size_limit(1<<20) y .case_insensitive(!case_sensitive)
    pub fn matches(&self, name_bytes: &[u8]) -> bool;  // fold: lossy→nfc(→lowercase si insensitive)→nfc — CALCA el fold de norte-tui/src/nav.rs (la 2ª NFC importa: J̌/ǰ)
}

pub struct ContentNeedle { needles: Vec<Vec<u8>>, max_len: usize }  // aguja codificada a cada encoding de reload_cycle() que la represente sin pérdida (encode con encoding_rs; descarta encodings donde hay unmappables); case-insensitive = añade variantes lower/upper de la aguja ANTES de codificar (fold simple, documentado)
pub struct Overlap { tail: Vec<u8> }  // retiene max_len-1 bytes del chunk anterior
impl ContentNeedle {
    pub fn literal(text: &str, case_sensitive: bool) -> Self;
    pub fn find_in(&self, ov: &mut Overlap, chunk: &[u8]) -> Option<usize>;  // busca en tail+chunk con memchr::memmem, actualiza tail
}

pub struct ContentRegex { re: regex::Regex }  // para content_regex: se aplica sobre TEXTO decodificado por chunks con solape de línea — la decodificación vive en el walker (T3), aquí solo compila con size_limit
#[derive(Debug, thiserror::Error)]
pub enum SearchError { /* BadGlob, BadRegex — mapean a Error::InvalidPath? NO: a INVALID_PARAMS en el daemon; en engine devuelve proto Error::Unsupported? Decisión: variante local con conversión a proto::Error::Internal NO — usa un error propio y el caller del daemon lo convierte a INVALID_PARAMS con detalle saneado */ }
```
- [ ] **Step 4:** verde + clippy + fmt.
- [ ] **Step 5: Commit** — `feat(core): matchers de fs.search — nombre NFC, aguja multi-encoding con solape (liveSearch T2)`

---

### Task 3: `Engine::search` — walker Task con canal de hits

**Files:**
- Modify: `crates/norte-core/src/engine.rs`, `crates/norte-core/src/search.rs` (el walk vive aquí: `search::run_walk`)
- Test: `crates/norte-core/tests/engine_search.rs` (nuevo; harness Engine+MemProvider como engine_journal.rs / backend_embedded.rs)

- [ ] **Step 1: tests rojos** (los 6 contratos):
```rust
// 1. solo_nombre_encuentra_recursivo: árbol mem:///a/{x.rs,sub/{y.rs,z.txt}} + glob "*.rs" → 2 hits (paths COMPLETOS), Task Completed.
// 2. contenido_multiencoding: f1 UTF-8 con "año", f2 Latin-1 con b"a\xF1o", f3 binario (con NUL) que CONTIENE los bytes de la aguja → hits = f1 y f2, f3 saltado; matches con line/preview poblados.
// 3. cancelacion_limpia: árbol grande (200 entradas) + faults latency; cancel a mitad → Cancelled, el canal de hits se CIERRA, sin task colgada (regla 3).
// 4. max_hits_trunca: max_hits=3 sobre 10 candidatos → exactamente 3 hits, Completed (el progress final lleva el truncado — assert sobre entries_done vs entries_total o el campo que se elija; documenta).
// 5. errores_por_entrada_no_abortan: un subdir que lista Err (faults del testkit) → la búsqueda sigue, progress cuenta el saltado, Completed.
// 6. criterios_invalidos: cero criterios → Err ANTES de crear la Task; glob Y regex de nombre a la vez → Err.
```
Rojo confirmado.

- [ ] **Step 2: implementación**
  - Firma (engine.rs, junto a copy_with_as):
```rust
/// Búsqueda viva (spec §17.1a): Task cancelable + canal de hits en LOTES.
/// Lectura pura — sin journal (regla 4 no aplica); gate de policy como las
/// lecturas: mismo tratamiento que fs.list para agentes (VERIFICA cómo se
/// gatea list para Actor::Agent — grep gate/PolicyOp en engine.rs y daemon;
/// replica EXACTAMENTE ese criterio y documenta cuál es).
pub async fn search_as(
    &self,
    params: norte_proto::methods::FsSearchParams,
    actor: crate::journal::Actor,
) -> Result<(TaskHandle, tokio::sync::mpsc::Receiver<norte_proto::methods::SearchHits>), Error>
```
  - Valida criterios (cero criterios / glob+regex simultáneos → `Error` — el mismo tipo que el daemon convierte a INVALID_PARAMS; compila matchers de T2 AQUÍ, antes de la Task: regex/glob inválidos fallan el request, no la Task).
  - Canal `mpsc::channel(8)` de `SearchHits` (lotes ya coalescidos: el walker acumula hasta `SEARCH_HITS_MAX_BATCH` o flush cada ~100ms — calca la lógica FILL_BATCH/FILL_INTERVAL del TUI).
  - `sched.submit(&key, TaskKind::Search, Priority::Normal, actor, body)` donde body = `search::run_walk(provider, root, matchers, tx, ctx)`:
    * Walker ITERATIVO con `VecDeque<VPath>` (BFS, sin recursión), `ctx.cancel.is_cancelled()` chequeado POR ENTRADA (y en cada chunk de contenido), symlinks NO seguidos (los providers listan el kind — Symlink se salta para descenso, cuenta como candidato de nombre; documenta).
    * Progreso: `ctx.progress.update(|p| p.entries_done += 1)` por entrada escaneada; hits en… decide: entries_done = escaneadas, bytes_done = hits (documenta el mapeo en el rustdoc de FS_SEARCH — el guardian lo verá). El truncado por max_hits: `current = None` + Completed; el TUI muestra «truncada» si hits == max_hits (documenta).
    * Contenido: `provider.read(path, None)` streaming por chunks; primer chunk → `norte_encoding::detect` (Binary → skip contenido); literal → `ContentNeedle::find_in` con Overlap; regex → decodifica chunk (encoding del detect) acumulando resto de línea partida (solape textual), aplica ContentRegex por líneas completas; line/preview: cuenta `\n` hasta el offset del match + extrae la línea, `preview` recortado a 160 chars (tope server-side, spec).
    * Errores por entrada (list/read Err): cuenta y sigue (skipped en un contador local; expónlo como quieras en progress — documenta).
    * Envío de lote: `tx.send(batch).await` — si el receiver murió (TUI se fue), termina la Task LIMPIA (Completed temprano; documenta: dueño desaparecido = nadie escucha).
- [ ] **Step 3:** verde (los 6) + suite norte-core entera + clippy + fmt.
- [ ] **Step 4: Commit** — `feat(core): Engine::search_as — walker BFS cancelable con hits en lotes (liveSearch T3)`

---

### Task 4: daemon — `fs.search` + `search.hits` al conn dueño

**Files:**
- Modify: `crates/norte-core/src/daemon/server.rs`
- Test: donde vivan los tests del daemon (grep los de policy/scope en server.rs o tests/)

- [ ] **Step 1: tests rojos**:
```rust
// 1. search_round_trip: conexión A lanza fs.search sobre mem:// sembrado → recibe FsTaskResult{task_id}, luego ≥1 notif search.hits con sus entries, luego task.progress terminal Completed.
// 2. hits_solo_al_dueno: conexiones A y B suscritas; A busca → B NO recibe ninguna search.hits (sí puede ver task.progress según may_observe — pinnea el comportamiento).
// 3. search_params_invalidos: cero criterios → INVALID_PARAMS con detalle, sin Task creada.
// 4. cancel_por_wire: A busca (faults latency), A manda task.cancel → terminal Cancelled, el canal server-side no fuga (la task sale de shared.tasks).
```
- [ ] **Step 2: implementación**
  - Envío dirigido: método nuevo en `Shared`:
```rust
/// Envía un frame a UNA conexión concreta (los hits de una búsqueda son
/// del que la lanzó — jamás broadcast). No-op si la conexión murió.
fn send_to_conn(&self, conn_id: u64, frame: &Arc<[u8]>)
```
    (usa `subscribers.get(&conn_id)` + `try_send`; mismo criterio de expulsión-si-no-drena que broadcast_where o documenta por qué no).
  - Arm `FS_SEARCH` en `dispatch_fs_task` (server.rs:1975): parsea `FsSearchParams`, `engine.search_as(params, actor)` → en Err de validación responde INVALID_PARAMS (detalle del SearchError saneado — SIN el patrón crudo del usuario en el mensaje… sí puede ir el diagnóstico de regex compilada: decisión: detalle = mensaje del error de regex/glob, es diagnóstico del propio input del requester; OK incluirlo); en Ok → `register_task` (bomba de progreso gratis) + spawnea la bomba de HITS: task que drena el receiver y por cada `SearchHits` construye `Notification{method: SEARCH_HITS, params}`, `encode_frame`, `shared.send_to_conn(conn_id, &frame)`. `conn_id` está disponible en el dispatch (verifica cómo llega — handle_value lo recibe; pásalo a dispatch_fs_task si no llega ya).
  - Muerte de la conexión dueña: el receiver-side de la bomba muere cuando la task acaba; la task NO se cancela sola si el peer muere a mitad (#64 cubre el drop del dispatch, pero la Task ya está registrada) — comportamiento: igual que una copia (sigue hasta terminal, gobernada, cancelable por task.cancel). Documenta.
- [ ] **Step 3:** verde + suite daemon + clippy + fmt.
- [ ] **Step 4: Commit** — `feat(core): daemon fs.search — hits dirigidos al conn dueño (liveSearch T4)`

---

### Task 5: `Backend::search` (embebido + remoto)

**Files:**
- Modify: `crates/norte-core/src/backend.rs`
- Test: `crates/norte-core/tests/backend_embedded.rs` + `backend_remote.rs`

- [ ] **Step 1: tests rojos**:
```rust
// backend_embedded: embedded_search_stream_de_hits — backend.search(params) → (TaskRef, rx); drena rx → entries esperadas; task.join() Completed.
// backend_remote: remote_search_como_el_embebido — mismo árbol vía daemon; hits llegan por la notif enrutados al rx; y remote_search_de_otro_task_id_no_se_cruza (dos búsquedas concurrentes en la misma conexión no mezclan lotes).
```
- [ ] **Step 2: implementación**
```rust
/// Búsqueda viva. Devuelve la Task y el stream de lotes de hits.
///
/// # Errors
/// Criterios inválidos (INVALID_PARAMS del daemon / validación del engine);
/// taxonomía del protocolo; daemon caído = ProviderUnavailable{retryable}.
pub async fn search(&self, params: FsSearchParams) -> Result<(TaskRef, mpsc::Receiver<SearchHits>), Error>
```
  - Embedded: `engine.search_as(params, Actor::User)` → `(TaskRef::from_handle(&h), rx)`.
  - Remote: `call_timed(FS_SEARCH, &params)` → `FsTaskResult` → `own_task(task_id, TaskKind::Search)`; canal de hits: `Inner` gana `search_routes: Mutex<HashMap<u64, mpsc::Sender<SearchHits>>>`; `pump_loop` gana rama `SEARCH_HITS` (parsea `SearchHits`, enruta por `task_id` — desconocido = descarta con debug); el sender se retira cuando la Task llega a terminal (en `route`, al ver terminal de ese id, o guard RAII en el TaskRef… lo simple: la rama de pump que ve un task.progress terminal para un id con route lo retira). Documenta el ciclo de vida.
  - Los clones de RemoteBackend comparten `search_routes` vía Inner (correcto: es enrutado, no canal one-shot take_*).
- [ ] **Step 3:** verde ambos ficheros + clippy + fmt.
- [ ] **Step 4: Commit** — `feat(core): Backend::search — stream de hits embebido y remoto (liveSearch T5)`

---

### Task 6: TUI — diálogo Alt+F7 + pane virtual

**Files:**
- Modify: `crates/norte-tui/src/app.rs`, `main.rs`, `ui.rs`, `keymap.rs` (COMMANDS), `keymap_presets/*.toml`, `crates/norte-i18n/i18n/{en,es}.ftl`
- Test: app.rs tests + `tests/snapshots_ui.rs`

- [ ] **Step 1:** comando `pane.search` en COMMANDS (rojo del test de ayuda) + binding `alt+f7` en los TRES presets (verifica colisiones con grep; f7 suele estar libre — mkdir clásico de TC NO existe aún) + claves ftl (en + es equivalentes):
```
help-cmd-pane-search = search by name/content (Alt+F7)
search-title = Search
search-name = name (glob):
search-content = content:
search-regex = [F2] regex: { $on }
search-case = [F3] case: { $on }
search-hint = [tab] field · [enter] search · [esc] cancel
search-status-running = search: { $n } hits (searching…)
search-status-done = search: { $n } hits
search-status-truncated = search: { $n } hits (truncated)
search-status-cancelled = search: { $n } hits (cancelled)
search-status-failed = search failed: { $error }
on-yes = on
on-no = off
```
- [ ] **Step 2: diálogo** (app.rs): `SearchDialog { name: String, content: String, field: SearchField{Name,Content}, regex: bool, case: bool }` + `App.search_dialog: Option<SearchDialog>` + input estilo name_input (imprimibles/backspace al campo activo, Tab alterna, F2/F3 toggles, Enter lanza si ALGÚN criterio no vacío — si ambos vacíos, no-op con aviso, Esc cierra). Tests de la lógica (tab/toggles/validación). Render `draw_search_dialog` (ui.rs, patrón modal centrado; muestra root = cwd del pane).
- [ ] **Step 3: pane virtual** (main.rs + app.rs):
  - Estado del run loop: `search_run: Option<SearchRun>` con `SearchRun { task: TaskRef, rx: mpsc::Receiver<SearchHits>, pane: usize, prev_dir: VPath, hits: usize, state: SearchState }`.
  - Lanzar: construye `FsSearchParams` del diálogo (root = cwd; regex toggle decide name_glob vs name_regex y content vs content_regex; case), `backend.search(...)` — Err → barra con detalle por `detail_for_bar` (search-status-failed), diálogo sigue abierto. Ok → cierra diálogo, guarda prev_dir, `pane.set_listing(root.clone(), vec![])` + marca virtual (campo nuevo `Pane.virtual_search: bool` — con él: draw_status usa search-status-*, y el cd/Ctrl+R restaura).
  - Drenaje en el select! (brazo como fill): `batch = search_run.rx.recv()` → `extend_listing(batch.entries)` (los VPaths completos; el pane los muestra con path relativo… NO: v1 muestra el `name` como cualquier listing — el path completo está en la entry y F5/Enter lo usan; documenta que la vista es plana estilo feed-to-listbox). `hits += n`. `None` del canal → estado final desde `task.progress()`/join no-bloqueante (borrow_and_update) → SearchState::{Done,Truncated(max alcanzado),Cancelled,Failed}.
  - Esc con search_run vivo (browse, sin modal/quick/popup): `task.cancel()` — los hits se conservan, estado Cancelled. Ctrl+R o cualquier cd: sale del modo virtual (restaura prev_dir con cd normal), y si la task vive → cancel. Enter sobre un hit: cd al PADRE del hit + cursor sobre él (usa el mismo re-anclaje por path de extend_listing/cd — mira cómo el cd posiciona cursor; si no hay mecanismo, tras el cd busca el índice por path y fija cursor).
  - F5/F8/F3 funcionan SOLOS (selected() da la Entry con VPath completo — cero código).
- [ ] **Step 4:** snapshot `snapshot_search_pane_virtual` (pane virtual con hits y status «searching…») + tests de app (diálogo, estado virtual).
- [ ] **Step 5:** verde suite entera + clippy workspace + fmt.
- [ ] **Step 6: Commit** — `feat(tui): Alt+F7 — diálogo de búsqueda y pane virtual streaming (liveSearch T6)`

---

### Task 7: E2E + spec

**Files:**
- Create: `crates/norte-tui/tests/search_e2e.rs`
- Modify: spec live-search (desviaciones + estado)

- [ ] **Step 1: E2E** (harness backend_mem de tests/lua_fs.rs):
```rust
// criterio_de_salida_año: árbol con f1 UTF-8 "un año", f2 Latin-1 b"a\xF1o", f3.rs vacío, sub/f4 UTF-8 "año" →
//   search content="año" → drena (TaskRef+rx) → 3 hits (f1,f2,f4) con matches.line/preview; luego
//   F5-equivalente: backend.copy del primer hit al otro dir → byte-exacto (read).
// cancel_conserva_lo_llegado: faults latency, cancel tras el primer lote → Cancelled, hits parciales ≥1.
// nombre_hostil: fichero mem:///%FF%FE casa glob "*" → su Entry llega con bytes crudos intactos.
```
- [ ] **Step 2:** spec → estado IMPLEMENTADO + sección desviaciones (lo que haya: mapeo de progress, vista plana del pane virtual, etc.).
- [ ] **Step 3:** verde + clippy + fmt.
- [ ] **Step 4: Commit** — `test(tui): E2E live search — criterio de salida M4 búsqueda (liveSearch T7)`

---

### Task 8: cierre — reviewers + gate + push (orquesta el controller)

- [ ] protocol-guardian ya pasó en T1; reviewers globales sobre el rango: security (send_to_conn, gate de agentes, ReDoS), encoding-auditor (aguja multi-encoding, solape, preview lossy, corpus), rust (reglas duras, canales/ciclos de vida). Aplicar hallazgos con TDD.
- [ ] `just ci` (nohup + log — el runner mata jobs largos; precedente M4/navTC). Docs: OJO links rustdoc a items privados (dos cierres seguidos tropezaron ahí — revisa `[`item_privado`]` antes).
- [ ] Push de todo el rango + actualizar memoria del proyecto.

---

## Self-review del plan (hecho)

- Cobertura spec: wire completo (T1: params/notif/TaskKind/goldens/bump/guardian), matchers encoding-aware con solape (T2), walker BFS+cancelación+límites+errores-por-entrada (T3), hits solo-al-dueño (T4: send_to_conn nuevo), Backend simétrico con enrutado por task_id (T5), diálogo+pane virtual operable+Esc/Ctrl+R/Enter (T6), E2E criterio de salida «año» (T7), reviewers/gate/push (T8). Fuera de alcance de la spec respetado (sin FTS5, sin filtros fecha/tamaño, sin tool MCP — deuda al cerrar).
- Tipos consistentes: `FsSearchParams`/`SearchHits`/`MatchInfo`/`FS_SEARCH`/`SEARCH_HITS`/`SEARCH_HITS_MAX_BATCH` (T1) usados idénticos en T3-T6; `NameMatcher`/`ContentNeedle`/`Overlap`/`ContentRegex`/`SearchError` (T2) consumidos en T3; `search_as` (T3) en T4-T5; `Backend::search`/`search_routes` (T5) en T6; `SearchRun`/`SearchDialog`/`Pane.virtual_search` (T6) en T7.
- Decisiones delegadas con contrato explícito (no placeholders): gate de lectura para agentes («replica el criterio de fs.list y documenta»), mapeo de contadores de progress (documentar en rustdoc de FS_SEARCH — el guardian lo revisa), retirada del route al terminal (T5, «documenta el ciclo de vida»).
