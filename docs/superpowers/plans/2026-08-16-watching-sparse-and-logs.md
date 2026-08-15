# Watching, sparse files and logs: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: use `superpowers:subagent-driven-development`
> or `superpowers:executing-plans` to implement this plan task by task. Steps
> use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The graphical frontend sees external changes (#106); a copy leaves
holes in the destination instead of writing megabytes of zeros; and the two
frontends a user actually runs stop discarding every diagnostic they produce.

**Architecture:** Spec is
`docs/superpowers/specs/2026-08-15-watching-sparse-and-logs-design.md` — read it
first, it carries the reasoning this plan does not repeat. Three independent
pieces, none of which touches `norte-proto`. Item 7 moves a working module to a
shared crate and wires a second consumer. Item 8 adds one function to the write
path of the local provider. Item 9 adds a second layer to the tracing registry
and a second entry point for frontends that cannot write to stderr.

**Tech stack:** Rust 2024, `notify` 8, `tokio` (`sync`/`rt`/`time`/`fs`),
`tracing-subscriber` + `tracing-appender` (new), GPUI for the graphical side,
`nextest` via `just`.

**Order:** 7 → 8 → 9, by risk. The move proves itself by its own tests passing
in a new crate. Sparse touches the write path every copy in the product goes
through and wants the freshest attention. Logging is additive.

---

## File map

| file | what it becomes responsible for |
| --- | --- |
| `crates/norte-frontend/src/watch.rs` | `DirWatch`, moved verbatim from the terminal crate |
| `crates/norte-frontend/Cargo.toml` | `notify` and `tokio` as direct deps |
| `crates/norte-tui/src/lib.rs`, `main.rs` | the module goes; the two call sites re-point |
| `crates/norte-gui/src/main.rs` | owns a `DirWatch`, re-watches on `cd`, refreshes on its channel |
| `crates/norte-vfs-local/src/provider.rs` | `write_maybe_sparse`, used by `LocalSink` |
| `crates/norte-vfs-local/src/confined.rs` | the same helper, used by `ConfinedSink` |
| `crates/norte-config/src/dirs.rs` | `state_dir`, hoisted out of the terminal frontend |
| `crates/norte-config/src/load.rs` | `[log] dir` and `[log] retain` in `CommonConfig` |
| `crates/norte-core/src/logging.rs` | the rolling file layer and `init_to_file` |
| `crates/norte-cli/src/doctor.rs` | the `logs` section |

---

## Task 1: `DirWatch` moves to the shared frontend crate

**Files:**
- Create: `crates/norte-frontend/src/watch.rs` (moved)
- Modify: `crates/norte-frontend/src/lib.rs`, `crates/norte-frontend/Cargo.toml`
- Modify: `crates/norte-tui/src/lib.rs`, `crates/norte-tui/src/main.rs`, `crates/norte-tui/Cargo.toml`

- [ ] **Step 1: move the file with `git mv`, so the history follows it**

```bash
git mv crates/norte-tui/src/watch.rs crates/norte-frontend/src/watch.rs
```

Do NOT retype the file. It is 508 lines with its own tests and the move must be
provably a move: `git log --follow` has to keep working, and the tests passing
unchanged in a new crate is the whole assertion of this task.

- [ ] **Step 2: declare it and give the crate what it needs**

In `crates/norte-frontend/src/lib.rs`, keeping the file's alphabetical order —
`theme`, `viewer`, `viewport`, so this goes last:

```rust
pub mod watch;
```

In `crates/norte-frontend/Cargo.toml`, in `[dependencies]`:

```toml
# Vigilancia de los dirs visibles (#106), compartida por los dos frontends: la
# TUI la estrenó y la GUI la necesita igual, y duplicarla sería duplicar
# también el pitfall de inotify que ya está resuelto una vez. `notify` ya es
# dependencia del workspace (la usa `norte-config` para recargar la config), y
# `tokio` ya está en el grafo de este crate por `norte-vfs-local` — las dos son
# una ARISTA nueva, no código nuevo (regla 8).
notify.workspace = true
tokio = { workspace = true, features = ["fs", "macros", "rt", "sync", "time"] }
```

and in `[dev-dependencies]`:

```toml
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "test-util"] }
```

The five features are exactly what the module uses: `sync` for the channels,
`rt` for `tokio::spawn`, `time` for the debounce deadline, `fs` for the poller's
`metadata`, `macros` for `select!`.

- [ ] **Step 3: re-point the terminal frontend**

In `crates/norte-tui/src/lib.rs`, delete line 23:

```rust
pub mod watch;
```

In `crates/norte-tui/src/main.rs:2452`:

```rust
    let mut dir_watch = norte_frontend::watch::DirWatch::new();
```

Then remove `notify` from `crates/norte-tui/Cargo.toml` — nothing else in that
crate uses it. Verify before deleting:

```bash
grep -rn "notify::" crates/norte-tui/src
```
Expected: no output.

- [ ] **Step 4: the tests pass unchanged in their new home**

```bash
just t norte-frontend && just t norte-tui
```
Expected: PASS on both. The `norte-frontend` count grows by the watcher's tests;
`norte-tui`'s drops by the same number and nothing else changes. If a test needed
editing to compile, the move was not a move — stop and find out why.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-frontend crates/norte-tui
git commit -m "refactor(frontend,tui): the directory watcher moves to the shared crate"
```

---

## Task 2: the graphical frontend watches its panes

**Files:**
- Modify: `crates/norte-gui/src/main.rs`
- Modify: `crates/norte-gui/Cargo.toml`
- Test: `crates/norte-gui/src/main.rs` (its `#[cfg(test)] mod tests`)

`norte-gui` is OUTSIDE the workspace and has its own gate: `just gui-ci`, never
`just t`.

- [ ] **Step 1: write the failing test**

In the existing test module of `crates/norte-gui/src/main.rs`, next to
`a_cd_to_the_same_dir_is_not_a_refresh` (which is the closest neighbour and
shows how this suite builds a root view):

```rust
/// #106: un evento del watcher refresca los DOS panes por el camino de
/// `refresh_dir` — el mismo que usa el read-after-write de una mutación, que
/// conserva las marcas (`refill`) en vez de limpiarlas como hace un `cd`.
///
/// Lo que se comprueba es el efecto observable: sale una `SessionCmd::List`
/// por pane, con el dir en el que cada uno YA está. Un refresco que mandara
/// otra cosa sería un `cd` disfrazado.
#[gpui::test]
async fn un_evento_del_watcher_relista_los_dos_panes(cx: &mut gpui::TestAppContext) {
    let (view, cmds) = root_con_canal(cx, vp("file:///a"), vp("file:///b"));

    view.update(cx, |this, cx| this.on_watch_event(cx));

    let enviados: Vec<_> = std::iter::from_fn(|| cmds.try_recv().ok()).collect();
    let dirs: Vec<VPath> = enviados
        .iter()
        .filter_map(|c| match c {
            SessionCmd::List { dir, .. } => Some(dir.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        dirs,
        vec![vp("file:///a"), vp("file:///b")],
        "un List por pane, cada uno sobre SU dir: {enviados:?}"
    );
}
```

`root_con_canal` stands in for whatever this suite already calls its root-view
constructor with an observable command channel — reuse it, do not add a second
one. If it does not exist, build it from what
`a_cd_to_the_same_dir_is_not_a_refresh` does and give it that name.

- [ ] **Step 2: run and watch it fail**

```bash
just gui-ci
```
Expected: FAIL, `no method named on_watch_event`.

- [ ] **Step 3: implement**

Add `norte-frontend`'s watcher to the root view. Three pieces:

1. A `dir_watch: norte_frontend::watch::DirWatch` field on the root view,
   created in its constructor.
2. `fn on_watch_event(&mut self, cx: &mut Context<Self>)`: for each of the two
   panes, if its directory is a local non-virtual `file://` path, call the
   EXISTING `self.refresh_dir(pane, dir, cx)`. Nothing new — that function
   already coalesces (#84) and already conserves marks.
3. A `cx.spawn` for the life of the window, modelled on the session event pump
   at `main.rs:1444`, that awaits `dir_watch.rx.recv()` and calls
   `this.update(cx, |this, cx| this.on_watch_event(cx))`. When the channel
   closes, the loop returns — same drop-based cancellation the terminal side
   relies on.

Call `rewatch` with both panes' directories wherever the view already reacts to
a directory change (the `cd` path), passing `None` for a pane whose scheme is
not `file` or whose path has an authority — the same predicate the terminal side
uses in `watch_targets`. Hoist that predicate into `norte_frontend::watch` as a
`pub fn watchable(dir: &VPath) -> Option<PathBuf>` rather than writing it twice.

Surface `take_degraded_notice()` once, in the status bar, the first time it
returns `true`.

`crates/norte-gui/Cargo.toml` needs no new dependency: `norte-frontend` is
already there.

- [ ] **Step 4: run the tests**

```bash
just gui-ci
```
Expected: PASS.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-gui crates/norte-frontend
git commit -m "feat(gui): the graphical frontend sees changes it did not make"
```

---

## Task 3: a copy leaves holes instead of writing zeros

**Files:**
- Modify: `crates/norte-vfs-local/src/provider.rs` (`LocalSink::write`, ~1916)
- Modify: `crates/norte-vfs-local/src/confined.rs` (`ConfinedSink::write`)
- Test: `crates/norte-vfs-local/tests/local.rs`

- [ ] **Step 1: write the failing tests**

In `crates/norte-vfs-local/tests/local.rs`:

```rust
/// #17 del roadmap: un fichero con un agujero se copia SIN materializarlo.
///
/// Las dos aserciones dicen cosas distintas y las dos hacen falta: los bytes
/// son idénticos (que es la corrección) y los BLOQUES no (que es lo único que
/// demuestra que la optimización ocurrió). Sin la segunda, el test pasaría con
/// la implementación de antes.
#[cfg(unix)]
#[tokio::test]
async fn un_destino_con_agujero_no_se_materializa() {
    use std::os::unix::fs::MetadataExt as _;

    let (p, root, dir) = provider();
    // 64 MiB de agujero y un byte al final: el caso de la imagen de VM.
    const HUECO: u64 = 64 * 1024 * 1024;
    let origen = dir.path().join("origen.img");
    let f = std::fs::File::create(&origen).expect("crear");
    f.set_len(HUECO).expect("agujero");
    drop(f);

    let mut sink = p.write(&child(&root, b"destino.img")).await.expect("write");
    let mut leido = p
        .read(&child(&root, b"origen.img"), None)
        .await
        .expect("read");
    while let Some(chunk) = futures::StreamExt::next(&mut leido).await {
        sink.write(chunk.expect("chunk")).await.expect("escribe");
    }
    sink.commit().await.expect("commit");

    let destino = dir.path().join("destino.img");
    let md = std::fs::metadata(&destino).expect("stat");
    assert_eq!(md.len(), HUECO, "el tamaño LÓGICO se conserva entero");
    assert_eq!(
        std::fs::read(&destino).expect("leer"),
        vec![0u8; usize::try_from(HUECO).expect("cabe")],
        "y los bytes que se leen son los mismos"
    );
    assert!(
        md.blocks() * 512 < HUECO / 8,
        "pero el disco no los guarda: {} bloques para {HUECO} bytes",
        md.blocks()
    );
}

/// El caso que la optimización NO puede romper: ceros que el usuario escribió
/// a propósito, en medio de datos. Se permite que el destino sea disperso; lo
/// que no se permite es que un byte cambie.
#[tokio::test]
async fn unos_ceros_en_medio_se_leen_igual() {
    let (p, root, _dir) = provider();
    let mut contenido = vec![b'a'; 1024];
    contenido.extend(std::iter::repeat_n(0u8, 256 * 1024));
    contenido.extend(std::iter::repeat_n(b'z', 1024));

    let mut sink = p.write(&child(&root, b"mixto.bin")).await.expect("write");
    sink.write(bytes::Bytes::from(contenido.clone()))
        .await
        .expect("escribe");
    sink.commit().await.expect("commit");

    let mut leido = p.read(&child(&root, b"mixto.bin"), None).await.expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = futures::StreamExt::next(&mut leido).await {
        out.extend_from_slice(&chunk.expect("chunk"));
    }
    assert_eq!(out, contenido, "byte a byte, sin excepciones");
}

/// Y la reanudación sigue sabiendo por dónde iba: el agujero tiene que contar
/// en la LONGITUD del staging desde que se abre, no desde el commit — es lo
/// que leen `open_resumable` (su `already`) y `partial_digest`.
#[tokio::test]
async fn un_agujero_cuenta_en_el_offset_de_reanudacion() {
    let (p, root, _dir) = provider();
    let destino = child(&root, b"resume.bin");

    let (mut sink, already) = p.open_resumable(&destino).await.expect("abre");
    assert_eq!(already, 0, "staging fresco");
    sink.write(bytes::Bytes::from(vec![0u8; 128 * 1024]))
        .await
        .expect("todo ceros");
    sink.keep().await.expect("conserva el parcial");

    let (_sink, already) = p.open_resumable(&destino).await.expect("reabre");
    assert_eq!(
        already,
        128 * 1024,
        "el agujero YA cuenta: reanudar desde 0 recopiaría lo hecho"
    );
}
```

`provider()` and `child()` are this file's existing helpers.

- [ ] **Step 2: run and watch them fail**

```bash
just t norte-vfs-local
```
Expected: the first FAILS on the `blocks()` assertion (the copy materialises the
hole); the other two PASS already and are the regression net.

- [ ] **Step 3: implement the shared helper**

In `crates/norte-vfs-local/src/provider.rs`, next to the other free functions:

```rust
/// Escribe `chunk` dejando AGUJERO donde es todo ceros (roadmap ítem 8).
///
/// Un chunk entero de ceros no se escribe: se salta la posición y se fija la
/// longitud. El filesystem decide si eso es un agujero de verdad —ext4, XFS y
/// APFS sí; uno sin agujeros asigna al escribir y sale igual de correcto—, y
/// lo que se lee después son los mismos bytes en los dos casos.
///
/// **La longitud se fija AQUÍ y no en el commit**, y esa es la parte que no es
/// obvia: `open_resumable` deriva su `already` del tamaño del staging y
/// `partial_digest` lee sus primeros `len` bytes. Con la longitud aplazada, un
/// parcial que acabara en agujero diría que tiene menos bytes de los que
/// tiene, y la reanudación recopiaría encima.
///
/// Límite honesto: la unidad es el CHUNK. Un agujero más pequeño que un chunk,
/// o desalineado con él, se materializa — esto no busca huecos dentro de los
/// datos, solo se abstiene de escribir los que ya vienen enteros.
///
/// INVARIANTE: el sink escribe secuencialmente desde el final, así que la
/// posición tras el salto es siempre mayor que la longitud actual y el
/// `set_len` solo puede EXTENDER. Un sink que retrocediera truncaría.
fn write_maybe_sparse(file: &mut std::fs::File, chunk: &[u8]) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom, Write as _};
    if chunk.is_empty() {
        return Ok(());
    }
    if chunk.iter().any(|&b| b != 0) {
        return file.write_all(chunk);
    }
    let salto = i64::try_from(chunk.len()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "chunk mayor que i64")
    })?;
    let pos = file.seek(SeekFrom::Current(salto))?;
    file.set_len(pos)
}
```

Make it `pub(crate)` so `confined.rs` uses the same one.

Then `LocalSink::write` (`provider.rs:1916`) swaps its `file.write_all(&chunk)`
for `write_maybe_sparse(&mut file, &chunk)`, and `ConfinedSink::write` in
`confined.rs` does the same. Nothing else changes in either: the staging, the
publish and `Drop` are untouched.

- [ ] **Step 4: run the tests**

```bash
just t norte-vfs-local && just t norte-core
```
Expected: PASS. `norte-core` too, because the copy engine's whole resume suite
runs through this sink.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-vfs-local
git commit -m "feat(vfs-local): a written run of zeros becomes a hole"
```

---

## Task 4: `state_dir` becomes shared

**Files:**
- Modify: `crates/norte-config/src/dirs.rs`
- Modify: `crates/norte-tui/src/main.rs:7634`
- Test: `crates/norte-config/src/dirs.rs` (its test module)

- [ ] **Step 1: write the failing tests**

In `crates/norte-config/src/dirs.rs`'s test module, following the shape of the
existing `user_config_dir_on` tests:

```rust
/// Precedencia del directorio de ESTADO, con el entorno inyectado — así la
/// rama de Windows la fija una suite que solo corre en Linux, igual que hace
/// `user_config_dir_on`.
#[test]
fn state_dir_sigue_su_precedencia() {
    let env = |vars: &[(&str, &str)]| {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k: &str| {
            vars.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| std::ffi::OsString::from(v))
        }
    };

    // XDG gana al HOME.
    assert_eq!(
        state_dir_on(false, &env(&[("XDG_STATE_HOME", "/x"), ("HOME", "/h")])),
        Some(std::path::PathBuf::from("/x/norte"))
    );
    // Vacío es AUSENTE, no una ruta a la raíz — mismo criterio que la config.
    assert_eq!(
        state_dir_on(false, &env(&[("XDG_STATE_HOME", ""), ("HOME", "/h")])),
        Some(std::path::PathBuf::from("/h/.local/state/norte"))
    );
    // Windows no mira XDG.
    assert_eq!(
        state_dir_on(true, &env(&[("XDG_STATE_HOME", "/x"), ("LOCALAPPDATA", r"C:\s")])),
        Some(std::path::PathBuf::from(r"C:\s").join("norte").join("state"))
    );
    // Y un entorno pelado no inventa nada: el caller degrada con aviso.
    assert_eq!(state_dir_on(false, &env(&[])), None);
}
```

- [ ] **Step 2: run and watch them fail**

```bash
just t norte-config
```
Expected: FAIL, `cannot find function state_dir_on`.

- [ ] **Step 3: implement**

Move the body of `norte-tui/src/main.rs:7634` into `dirs.rs` as
`pub fn state_dir_on(windows: bool, env: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf>`
plus a `pub fn state_dir() -> Option<PathBuf>` wrapper that passes
`cfg!(windows)` and `std::env::var_os` — the exact pair `user_config_dir_on` /
`user_config_dir` already forms in that file. Keep the rustdoc: it explains why
state is not config (the trust store is machine-local, it must not travel with
dotfiles), and that reasoning now also covers the log.

Delete the private copy in the terminal frontend and re-point its three callers
(`main.rs:7837`, `:7916`, and the function itself) at
`norte_config::dirs::state_dir`.

- [ ] **Step 4: run the tests**

```bash
just t norte-config && just t norte-tui
```
Expected: PASS.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-config crates/norte-tui
git commit -m "refactor(config): the state directory is resolved in one place"
```

---

## Task 5: `[log]` in the configuration

**Files:**
- Modify: `crates/norte-config/src/load.rs` (`CommonConfig`, ~1313)
- Modify: `crates/norte-config/src/schema.rs`
- Test: `crates/norte-config/tests/` (the file that already covers `[daemon]`)

- [ ] **Step 1: write the failing test**

```rust
/// `[log]` se lee de las capas de máquina y de usuario, JAMÁS de la de
/// proyecto: un `norte.toml` que viaja con un repositorio ajeno no puede
/// decidir dónde escribe sus logs este daemon. Es la misma regla
/// fail-closed que `[daemon]` (review MAJOR-1), y por la misma razón:
/// redirigir una escritura no es presentación.
#[test]
fn la_capa_de_proyecto_no_decide_donde_van_los_logs() {
    let layers = layers_con(&[
        (Layer::User, "[log]\ndir = \"/de-usuario\"\nretain = 3\n"),
        (Layer::Project, "[log]\ndir = \"/del-repo\"\nretain = 99\n"),
    ]);
    let cfg = load(&layers).expect("carga");
    assert_eq!(cfg.log_dir.as_deref(), Some(std::path::Path::new("/de-usuario")));
    assert_eq!(cfg.log_retain, Some(3));
}
```

`layers_con` stands in for the helper that file already uses to build layers
from inline TOML.

- [ ] **Step 2: run and watch it fail**

```bash
just t norte-config
```
Expected: FAIL, no field `log_dir`.

- [ ] **Step 3: implement**

Two fields on `CommonConfig`, next to `daemon_socket` and documented in the same
voice:

```rust
    /// `[log] dir` (last-wins; None = `<state_dir>/logs`; never from Project —
    /// fail-closed, same reasoning as `[daemon]`: choosing where a process
    /// writes is not presentation).
    pub log_dir: Option<std::path::PathBuf>,
    /// `[log] retain` (last-wins; None = the appender's default). How many
    /// rotated files survive.
    pub log_retain: Option<usize>,
```

Merge them in `load` exactly where the `[daemon]` scalars are merged, filtered
to non-Project layers. Add the section to `schema.rs` so the published JSON
Schema and its golden test know about it.

No level key: `RUST_LOG` already does that, and two mechanisms for one setting
is how they drift apart.

- [ ] **Step 4: run the tests**

```bash
just t norte-config
```
Expected: PASS. If the schema golden test fails, read the diff before accepting
it — it is the wire-adjacent artefact of this task.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-config
git commit -m "feat(config): [log] says where the log goes and how much is kept"
```

---

## Task 6: the rolling file layer, and an entry point for the frontends

**Files:**
- Modify: `crates/norte-core/src/logging.rs`
- Modify: `crates/norte-core/Cargo.toml`
- Test: `crates/norte-core/src/logging.rs` (its test module)

- [ ] **Step 1: write the failing tests**

Extend the existing test module. The first test is the feature; the second is
the security cap, and it is not optional — it is the reason this module exists
in the shape it does.

```rust
/// El appender escribe de verdad en el directorio que se le da.
#[test]
fn el_log_aterriza_en_un_fichero() {
    let dir = tempfile::tempdir().expect("tmp");
    let (layer, _guard) = file_layer(dir.path(), 3).expect("appender");
    let sub = tracing_subscriber::registry().with(layer).with(filter_from(None));
    tracing::subscriber::with_default(sub, || {
        tracing::info!(target: "norte_core::prueba", "una linea");
    });
    drop(_guard);

    let ficheros: Vec<_> = std::fs::read_dir(dir.path())
        .expect("listar")
        .map(|e| e.expect("entrada").path())
        .collect();
    assert_eq!(ficheros.len(), 1, "un fichero de log: {ficheros:?}");
    let texto = std::fs::read_to_string(&ficheros[0]).expect("leer");
    assert!(texto.contains("una linea"), "el evento está: {texto}");
}

/// **El cap de `suppaftp` cubre el fichero igual que cubre stderr.** Se filtra
/// en el registry, antes de cualquier capa, así que debería seguirse de la
/// arquitectura — y por eso mismo se comprueba: «debería seguirse» no es una
/// prueba, y lo que está en juego es una contraseña en un fichero que persiste
/// (regla dura 10).
#[test]
fn la_password_de_ftp_no_llega_al_fichero() {
    let dir = tempfile::tempdir().expect("tmp");
    let (layer, _guard) = file_layer(dir.path(), 3).expect("appender");
    let sub = tracing_subscriber::registry()
        .with(layer)
        .with(filter_from(Some("suppaftp=trace")));
    tracing::subscriber::with_default(sub, || {
        tracing::trace!(target: "suppaftp", "PASS hunter2");
    });
    drop(_guard);

    let ficheros: Vec<_> = std::fs::read_dir(dir.path())
        .expect("listar")
        .map(|e| e.expect("entrada").path())
        .collect();
    let texto: String = ficheros
        .iter()
        .map(|f| std::fs::read_to_string(f).unwrap_or_default())
        .collect();
    assert!(
        !texto.contains("hunter2"),
        "la password llegó al fichero: {texto}"
    );
}
```

- [ ] **Step 2: run and watch them fail**

```bash
just t norte-core
```
Expected: FAIL, `cannot find function file_layer`.

- [ ] **Step 3: implement**

`crates/norte-core/Cargo.toml`:

```toml
# Appender rotatorio del log local (roadmap ítem 9). Es del proyecto tokio y es
# el appender alrededor del que `tracing-subscriber` está diseñado; la
# alternativa es escribir a mano la rotación, el nombrado, la poda y el guard
# del writer no bloqueante, que es más código que la propia feature (regla 8).
tracing-appender = "0.2"
```

In `logging.rs`:

```rust
/// La capa de fichero y su guard.
///
/// El guard hay que SOSTENERLO: el writer es no bloqueante y su hilo vacía la
/// cola al soltarlo. Un `let _ = ...` aquí perdería las últimas líneas, que
/// son justamente las del fallo que se está diagnosticando.
fn file_layer<S>(dir: &Path, retain: usize) -> std::io::Result<(impl Layer<S>, WorkerGuard)>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
```

Daily rotation, `max_log_files(retain)`, filename prefix `norte.log`, ANSI off
(a file is not a terminal). Then:

| function | layers | who calls it |
| --- | --- | --- |
| `init()` | stderr + file | `norte-cli`, the daemon |
| `init_to_file()` | file only | the terminal and graphical frontends |

Both build the identical `EnvFilter` through the existing `filter_from`, cap
included. Both return the `WorkerGuard` for the caller to hold, or `None` when
there is no state directory — in which case `init` still installs stderr and
`init_to_file` installs nothing, with one `eprintln!` saying so. **Logging that
fails to start must never stop the program.**

The directory comes from `CommonConfig::log_dir`, defaulting to
`norte_config::dirs::state_dir()?.join("logs")`, created with
`create_dir_all` on first use.

- [ ] **Step 4: run the tests**

```bash
just t norte-core && cargo test -p norte-core --doc
```
Expected: PASS.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-core
git commit -m "feat(core): the log rotates into a file, cap and all"
```

---

## Task 7: the frontends install it

**Files:**
- Modify: `crates/norte-tui/src/main.rs` (near `:2218`)
- Modify: `crates/norte-gui/src/main.rs`
- Test: `crates/norte-tui/tests/`

- [ ] **Step 1: write the failing test**

```rust
/// El frontend de terminal deja rastro en el FICHERO y ni un byte en stderr:
/// las dos mitades son la aserción. La primera es la feature; la segunda es
/// exactamente el motivo por el que este binario no instalaba subscriber.
#[test]
fn el_tui_loguea_al_fichero_y_no_a_la_pantalla() {
    let dir = tempfile::tempdir().expect("tmp");
    let salida = std::process::Command::new(env!("CARGO_BIN_EXE_ntc"))
        .arg("--version")
        .env("XDG_STATE_HOME", dir.path())
        .env("RUST_LOG", "debug")
        .output()
        .expect("ejecuta");

    assert!(
        String::from_utf8_lossy(&salida.stderr).is_empty(),
        "stderr tiene que quedar limpio: {}",
        String::from_utf8_lossy(&salida.stderr)
    );
    let logs = dir.path().join("norte/logs");
    assert!(logs.is_dir(), "el directorio de logs existe: {logs:?}");
}
```

- [ ] **Step 2: run and watch it fail**

```bash
just t norte-tui
```
Expected: FAIL — no log directory is created.

- [ ] **Step 3: implement**

In the terminal frontend's startup, before anything that could log, call
`norte_core::logging::init_to_file()` and **hold the guard for the whole life of
the process** (bind it in `main`, not in a helper that returns).

Then delete the comment at `main.rs:2218` that explains why a `tracing::warn!`
there would be discarded, and convert that `eprintln!` diagnostic to
`tracing::warn!`. That comment documents a limitation this task removes; leaving
it would leave the file lying about itself.

Same in the graphical frontend's `main`.

- [ ] **Step 4: run the tests**

```bash
just t norte-tui && just gui-ci
```
Expected: PASS.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-tui crates/norte-gui
git commit -m "feat(tui,gui): the frontends stop throwing their diagnostics away"
```

---

## Task 8: `norte doctor` says where the log is

**Files:**
- Modify: `crates/norte-cli/src/doctor.rs`
- Modify: `crates/norte-cli/src/main.rs` (call it with the others)
- Test: `crates/norte-cli/tests/`

- [ ] **Step 1: write the failing test**

```rust
/// El log es lo que hace posible un reporte de bug de alguien que no somos
/// nosotros, así que `doctor` tiene que decir DÓNDE está.
#[test]
fn doctor_nombra_el_fichero_de_log() {
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::create_dir_all(dir.path().join("logs")).expect("logs");
    let hallazgos = check_logs(Some(dir.path()));
    let f = hallazgos.iter().find(|f| f.section == "logs").expect("hay fila");
    assert_eq!(f.severity, Severity::Ok);
    assert!(
        f.detail.contains(&dir.path().join("logs").display().to_string()),
        "la fila lleva la RUTA: {}",
        f.detail
    );
}

/// Y un directorio de estado que no existe es una DEGRADACIÓN, no una avería:
/// la máquina funciona, simplemente no deja rastro. `Error` haría que
/// `norte doctor` saliera distinto de cero por algo que no rompe nada.
#[test]
fn sin_directorio_de_estado_es_aviso_y_no_error() {
    let hallazgos = check_logs(None);
    let f = hallazgos.iter().find(|f| f.section == "logs").expect("hay fila");
    assert_eq!(f.severity, Severity::Warn);
}
```

- [ ] **Step 2: run and watch it fail**

```bash
just t norte-cli
```
Expected: FAIL, `cannot find function check_logs`.

- [ ] **Step 3: implement**

```rust
/// El estado del log local: dónde está, cuánto ocupa, y si se puede escribir.
pub fn check_logs(dir: Option<&Path>) -> Vec<Finding>
```

Stable codes, following the existing convention in that file: `logs-ok`,
`logs-no-state-dir`, `logs-unwritable`. `Warn` for the last two, `Ok` for the
first with the path and the total size in the detail. Wire it into the same
place `check_connections` and `check_plugins` are called.

- [ ] **Step 4: run the tests**

```bash
just t norte-cli
```
Expected: PASS.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-cli
git commit -m "feat(cli): doctor names the log, which is what makes a bug report possible"
```

---

## Task 9: close the branch

- [ ] **Step 1: file the two Windows halves of item 8**

```bash
gh issue create --title "[vfs-local] Windows reparse points are neither followed nor refused deliberately" --body "..."
gh issue create --title "[vfs-local] A locked file on Windows needs bounded retry (ERROR_SHARING_VIOLATION)" --body "..."
```

Both bodies: what the behaviour should be, why it is not written (no machine
here can run one line of it, and writing platform code blind is how #25 and #33
became issues nobody can close), and that they are blocked on CI returning.
Point at the spec.

- [ ] **Step 2: changelog**

One entry per item, in the user's terms:

- the graphical frontend now notices changes it did not make, with the same
  limits the terminal one has (local panes; degraded polling misses a write
  inside an existing file);
- copying a file with holes no longer fills them in;
- there is a log, where it is, and that nothing leaves the machine.

Say that symlink policy and cycle detection were already done, so nobody reads
the roadmap's item 8 later and thinks they are missing.

- [ ] **Step 3: dispatch the reviewers**

`rust-reviewer` on the whole diff. `security-reviewer` on task 6 specifically,
and give it the real question: **the log is new persistence, and the events it
persists were written when nothing was reading them.** Ask it to look for lines
that leak a path, a host, a credential or a filename byte-for-byte — the
`suppaftp` cap and `engine::span_path` are the two defences that exist, and
whether they are enough is exactly what is unknown here.

No `protocol-guardian`: nothing in this plan touches the wire.

Apply BLOCKER and MAJOR in one pass. Say which MINORs were skipped and why.

- [ ] **Step 4: the one full gate run**

```bash
just ci
```
Run it in the FOREGROUND, one recipe at a time (`lint`, `test`, `docs`,
`check-gui`, `cov`), never through `| tail`.

- [ ] **Step 5: close and merge**

```bash
gh issue close 106 --comment "..."
```
Then use `superpowers:finishing-a-development-branch`.

---

## What this plan does NOT do

- **Watching over the protocol.** Reasoned in the spec; revisit if a remote
  daemon ever ships.
- **Sparse on the READ side.** Needs `ByteStream` to carry holes, which is a
  trait change and a wire concept for providers that have none.
- **Holes smaller than a chunk.** The unit is the chunk, deliberately.
- **A bug-report bundle.** Deferred with its reason: "redacted" needs a pass
  with a security reviewer present.
- **An audit of every existing log line.** Named as the security review's job in
  task 9 instead.
