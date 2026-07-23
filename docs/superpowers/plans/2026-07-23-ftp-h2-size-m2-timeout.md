# FTP H2 (4 GiB size) + M2 (guest socket timeout) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the FTP guest's 4 GiB size ceiling on MLSD servers (self-parse `size` as `u64`) plus a LIST anti-overwrite safeguard (H2), and give the plugin adapter a per-op timeout that marks a hung provider dead instead of blocking forever (M2).

**Architecture:** H2 replaces `suppaftp`'s `usize`-parsing MLSD/MLST parse with a guest-local `parse_mlsd_facts` (u64 size, raw name, kind) and refactors `stat_remote` to return a guest-local `StatEntry{kind, size: Option<u64>}`; the no-MLSD `stat_remote` refuses (`Err(Io)`) when a name-matching `ls -l` line fails to parse, closing the silent-overwrite hole. M2 wraps each adapter `spawn_blocking` in `tokio::time::timeout(OP_TIMEOUT)` and sets a shared `Arc<AtomicBool> dead` on expiry so subsequent ops fail fast.

**Tech Stack:** `suppaftp` v10 sync in `wasm32-wasip2`; the embedded artifact `crates/norte-core/resources/ftp-provider.wasm` (`just build-ftp-wasm`); `tokio::time::timeout`; in-process `libunftp`.

**Reference:** `docs/superpowers/specs/2026-07-23-ftp-h2-size-m2-timeout-design.md`.

---

## Task 1: H2 — `parse_mlsd_facts` + `ls_l_name` (parser + unit tests)

**Files:**
- Modify: `crates/norte-plugin-host/examples-wasm/ftp-provider/src/lib.rs`

The guest compiles for the host under `cargo test` (verified), so these free fns get real unit tests in a `#[cfg(test)]` module. `just ci` does not run the guest crate, so Task 3's e2e is the ci-visible guard; these unit tests run via `cargo test` in the guest dir.

- [ ] **Step 1: Add the two parsers.** Place near the other free fns (after `parse_list_line`).

```rust
/// Parsea una línea MLSD/MLST (RFC 3659: `[facts] SP pathname`). Devuelve
/// `(kind, size, raw_name)`: `kind` del fact `type`, `size` como u64 (`None` si
/// el fact `size` falta o no es numérico — un dir lo omite), `raw_name` tras el
/// PRIMER espacio (crudo; el caller aplica el rechazo U+FFFD/`/`/NUL). Reemplaza
/// a `ListParser::parse_mlsd`/`parse_mlst` de suppaftp, que parsea el size a
/// `usize` (techo 4 GiB en wasm32) y truncaba el nombre en `;`. `None` si la
/// línea no tiene la forma `facts SP name` (sin espacio, o nombre vacío).
fn parse_mlsd_facts(line: &str) -> Option<(EntryKind, Option<u64>, &str)> {
    let (facts, name) = line.split_once(' ')?;
    if name.is_empty() {
        return None;
    }
    let mut kind = EntryKind::File; // default si falta `type`
    let mut size = None;
    for fact in facts.split(';') {
        let Some((key, value)) = fact.split_once('=') else {
            continue;
        };
        if key.eq_ignore_ascii_case("type") {
            kind = match value.to_ascii_lowercase().as_str() {
                "dir" | "cdir" | "pdir" => EntryKind::Dir,
                "file" => EntryKind::File,
                "link" => EntryKind::Symlink,
                _ => EntryKind::Other,
            };
        } else if key.eq_ignore_ascii_case("size") {
            size = value.parse::<u64>().ok();
        }
    }
    Some((kind, size, name))
}

/// Nombre de una línea `ls -l` de forma TOLERANTE, SÓLO para la salvaguarda
/// anti-overwrite (nunca como nombre real): `perms links owner group size mon day
/// time name` → el nombre es todo tras el 8º campo separado por whitespace.
/// `None` si la línea tiene <9 campos (p. ej. una cabecera `total N`). Los
/// nombres con espacio inicial se pierden (límite conocido de `ls -l`).
fn ls_l_name(line: &str) -> Option<&str> {
    // Localiza el inicio del 9º token saltando 8 (campo + whitespace siguiente).
    let mut rest = line;
    for _ in 0..8 {
        let trimmed = rest.trim_start();
        let end = trimmed.find(char::is_whitespace)?;
        rest = &trimmed[end..];
    }
    let name = rest.trim_start();
    if name.is_empty() { None } else { Some(name) }
}
```

- [ ] **Step 2: Add unit tests.** Append a `#[cfg(test)]` module at the end of the file (before or after `export!`).

```rust
#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn mlsd_facts_size_u64_beyond_4gib() {
        // 5 GiB = 5368709120 > u32::MAX: suppaftp lo rompía; aquí es u64 exacto.
        let line = "type=file;size=5368709120;modify=20200101000000; big.bin";
        let (kind, size, name) = parse_mlsd_facts(line).expect("parsea");
        assert_eq!(kind, EntryKind::File);
        assert_eq!(size, Some(5_368_709_120));
        assert_eq!(name, "big.bin");
    }

    #[test]
    fn mlsd_facts_dir_without_size() {
        let (kind, size, name) = parse_mlsd_facts("type=dir;modify=20200101000000; sub").expect("dir");
        assert_eq!(kind, EntryKind::Dir);
        assert_eq!(size, None);
        assert_eq!(name, "sub");
    }

    #[test]
    fn mlsd_facts_name_with_semicolon_survives() {
        // El nombre va tras el PRIMER espacio: un `;` en el nombre NO lo trunca
        // (el bug de suppaftp que `split(';')` causaba).
        let (_, _, name) = parse_mlsd_facts("type=file;size=1; a;b.txt").expect("parsea");
        assert_eq!(name, "a;b.txt");
    }

    #[test]
    fn mlsd_facts_missing_type_defaults_file_and_unknown_type_is_other() {
        assert_eq!(parse_mlsd_facts("size=1; f").unwrap().0, EntryKind::File);
        assert_eq!(parse_mlsd_facts("type=cdir; .").unwrap().0, EntryKind::Dir);
        assert_eq!(parse_mlsd_facts("type=os.unix=slink; x").unwrap().0, EntryKind::Other);
    }

    #[test]
    fn mlsd_facts_rejects_malformed() {
        assert!(parse_mlsd_facts("no-space-no-name").is_none());
        assert!(parse_mlsd_facts("type=file;size=1; ").is_none()); // nombre vacío
    }

    #[test]
    fn ls_l_name_extracts_after_eight_fields() {
        let n = ls_l_name("-rw-r--r-- 1 owner group 5368709120 Jan 12 10:00 big.bin");
        assert_eq!(n, Some("big.bin"));
        // Nombre con espacio interno: se conserva entero.
        let n2 = ls_l_name("-rw-r--r-- 1 o g 5 Jan 12 10:00 con espacios.txt");
        assert_eq!(n2, Some("con espacios.txt"));
    }

    #[test]
    fn ls_l_name_rejects_header_and_short() {
        assert_eq!(ls_l_name("total 8"), None);
        assert_eq!(ls_l_name(""), None);
    }
}
```

- [ ] **Step 3: Run the unit tests.**

Run: `cargo test --manifest-path crates/norte-plugin-host/examples-wasm/ftp-provider/Cargo.toml`
Expected: the 7 `parse_tests` PASS (compiles for host; `parse_mlsd_facts`/`ls_l_name` unused-warning is expected until Task 2 wires them — add `#[allow(dead_code)]` on both fns for this step, removed in Task 2, OR accept the warning since Task 2 follows immediately; prefer wiring in Task 2 without the allow to keep it clean — if running Task 1 standalone, add `#[allow(dead_code)]`).

- [ ] **Step 4: Commit.**

```bash
git add crates/norte-plugin-host/examples-wasm/ftp-provider/src/lib.rs
git commit -m "feat(plugin-host): #30 H2 — parse_mlsd_facts (size u64) + ls_l_name + tests"
```

---

## Task 2: H2 — wire into `stat_remote`/`list_dir` via `StatEntry`

**Files:**
- Modify: `crates/norte-plugin-host/examples-wasm/ftp-provider/src/lib.rs`

- [ ] **Step 1: Define `StatEntry` and change `stat_remote`'s return type.** Add near the top (after `CachedRead`):

```rust
/// Metadatos mínimos de una entrada, agnósticos del backend de parse (MLSD
/// self-parse o `ls -l` de suppaftp). Reemplaza el `File` de suppaftp en la
/// superficie de `stat_remote` para que el size sea u64 (no el `usize` de
/// suppaftp, techo 4 GiB en wasm32).
struct StatEntry {
    kind: EntryKind,
    size: Option<u64>,
}
```

Rewrite `stat_remote` to return `Result<Option<StatEntry>, VfsError>`:

```rust
fn stat_remote(
    ftp: &mut FtpStream,
    remote: &str,
    has_mlsd: bool,
    base: &str,
) -> Result<Option<StatEntry>, VfsError> {
    if has_mlsd {
        return match ftp.mlst(Some(remote)) {
            Ok(line) => match parse_mlsd_facts(&line) {
                Some((kind, size, _name)) => Ok(Some(StatEntry { kind, size })),
                None => Err(VfsError::Io), // MLST ilegible = anómalo
            },
            Err(e) => match map_err(&e) {
                VfsError::NotFound => Ok(None),
                other => Err(other),
            },
        };
    }
    if remote == base {
        return Ok(None);
    }
    let (parent, child) = match remote.rfind('/') {
        Some(0) => ("/", &remote[1..]),
        Some(i) => (&remote[..i], &remote[i + 1..]),
        None => return Ok(None),
    };
    if child.is_empty() {
        return Ok(None);
    }
    let lines = match ftp.list(Some(parent)) {
        Ok(l) => l,
        Err(e) => {
            return match map_err(&e) {
                VfsError::NotFound => Ok(None),
                other => Err(other),
            };
        }
    };
    for line in lines {
        match parse_list_line(&line) {
            Some(f) => {
                let n = f.name();
                if n.contains('\u{FFFD}') {
                    continue;
                }
                if n == child {
                    return Ok(Some(stat_entry_from_file(&f)));
                }
            }
            // Salvaguarda anti-overwrite (H2): una línea `ls -l` que NO parsea
            // (p. ej. size >= 4 GiB rompe el parse usize de suppaftp) pero cuyo
            // nombre casa `child` NO se descarta en silencio — se falla LOUD, así
            // `exists()` no dice "no existe" y write/rename/mkdir no sobrescriben
            // un fichero invisible. Cabeceras `total N` (ls_l_name = None) siguen
            // descartándose.
            None => {
                if let Some(n) = ls_l_name(&line) {
                    if !n.contains('\u{FFFD}') && n == child {
                        return Err(VfsError::Io);
                    }
                }
            }
        }
    }
    Ok(None)
}

/// `StatEntry` desde un `File` de suppaftp (rama LIST; size del `usize` de
/// suppaftp, límite 4 GiB aceptado para servidores sin MLSD).
fn stat_entry_from_file(f: &File) -> StatEntry {
    let kind = if f.is_symlink() {
        EntryKind::Symlink
    } else if f.is_directory() {
        EntryKind::Dir
    } else if f.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    let size = (kind == EntryKind::File).then(|| f.size() as u64);
    StatEntry { kind, size }
}
```

- [ ] **Step 2: Update `stat_remote`'s callers.**

`stat` (the `Some(f) => entry_from_file(...)` arm):
```rust
            match stat_remote(&mut s.ftp, &remote, s.has_mlsd, &s.base)? {
                Some(st) => Ok(Entry {
                    name: last_name(&segments),
                    kind: st.kind,
                    size: st.size,
                }),
                None => Err(VfsError::NotFound),
            }
```

`read` (dir check):
```rust
            match stat_remote(&mut s.ftp, &remote, s.has_mlsd, &s.base)? {
                None => return Err(VfsError::NotFound),
                Some(st) if st.kind == EntryKind::Dir => return Err(VfsError::Conflict),
                Some(_) => {}
            }
```

`remove` (dir check):
```rust
            let st =
                stat_remote(&mut s.ftp, &remote, s.has_mlsd, &s.base)?.ok_or(VfsError::NotFound)?;
            if st.kind == EntryKind::Dir {
                s.ftp.rmdir(&remote).map_err(|e| map_err(&e))
            } else {
                s.ftp.rm(&remote).map_err(|e| map_err(&e))
            }
```

`exists` is unchanged (`.is_some()` still works on `Option<StatEntry>`).

- [ ] **Step 3: Rewrite the `list_dir` MLSD branch** to use `parse_mlsd_facts`. Replace the block that computes `parsed`/`f`/`name` and pushes via `entry_from_file`. The MLSD branch now yields `(kind, size, name)` directly; the LIST branch keeps `parse_list_line` + `entry_from_file`. New loop body:

```rust
            for line in lines {
                if entries.len() >= MAX_LIST_ENTRIES {
                    return Err(VfsError::Io);
                }
                // (kind, size, raw_name) por rama.
                let (kind, size, name): (EntryKind, Option<u64>, &str) = if s.has_mlsd {
                    match parse_mlsd_facts(&line) {
                        Some(t) => t,
                        // MLSD machine-readable: una línea ilegible es anómala.
                        None => return Err(VfsError::Io),
                    }
                } else {
                    let Some(f) = parse_list_line(&line) else {
                        continue; // `total N` u otra no-entrada
                    };
                    let st = stat_entry_from_file(&f);
                    // `f.name()` toma prestado de `f`; se materializa abajo.
                    (st.kind, st.size, f.name_owned_hack())
                };
                if name == "." || name == ".." {
                    continue;
                }
                if name.contains('\u{FFFD}') || name.contains(['/', '\0']) {
                    return Err(VfsError::InvalidPath);
                }
                entries.push(Entry {
                    name: name.as_bytes().to_vec(),
                    kind,
                    size,
                });
            }
```

**Borrow problem:** in the LIST branch `f.name()` borrows `f`, but `f` is dropped at the end of the `else`. Do NOT invent `name_owned_hack`. Instead, keep the LIST and MLSD branches structurally separate so each owns its `name` lifetime. Use this shape instead (replaces the whole `for line in lines { ... }` body):

```rust
            for line in lines {
                if entries.len() >= MAX_LIST_ENTRIES {
                    return Err(VfsError::Io);
                }
                if s.has_mlsd {
                    let Some((kind, size, name)) = parse_mlsd_facts(&line) else {
                        return Err(VfsError::Io);
                    };
                    if name == "." || name == ".." {
                        continue;
                    }
                    if name.contains('\u{FFFD}') || name.contains(['/', '\0']) {
                        return Err(VfsError::InvalidPath);
                    }
                    entries.push(Entry { name: name.as_bytes().to_vec(), kind, size });
                } else {
                    let Some(f) = parse_list_line(&line) else {
                        continue;
                    };
                    let name = f.name();
                    if name == "." || name == ".." {
                        continue;
                    }
                    if name.contains('\u{FFFD}') || name.contains(['/', '\0']) {
                        return Err(VfsError::InvalidPath);
                    }
                    let st = stat_entry_from_file(&f);
                    entries.push(Entry { name: name.as_bytes().to_vec(), kind: st.kind, size: st.size });
                }
            }
```

- [ ] **Step 4: Remove the now-unused `entry_from_file`.** After Step 3, `entry_from_file` (the old `File`→`Entry`) has no callers (`stat`/`list_dir` build `Entry` directly; `stat_remote` uses `stat_entry_from_file`). Delete the `fn entry_from_file`. If the compiler reports it still used, keep it; otherwise remove to avoid a dead-code warning.

- [ ] **Step 5: Build the guest.**

Run: `cargo build --release --target wasm32-wasip2 --manifest-path crates/norte-plugin-host/examples-wasm/ftp-provider/Cargo.toml`
Expected: compiles clean (no unused-fn warnings). Fix borrow/type errors per the shapes above.

- [ ] **Step 6: Refresh the embedded artifact + run the full contract.**

Run: `just build-ftp-wasm && cargo nextest run -p norte-core --test ftp_provider_contract`
Expected: 46/46 PASS (MLSD self-parse is behaviourally identical for normal sizes; the hostile-name/`;` cases still pass).

- [ ] **Step 7: Commit.**

```bash
git add crates/norte-plugin-host/examples-wasm/ftp-provider/src/lib.rs
git add -f crates/norte-core/resources/ftp-provider.wasm
git commit -m "feat(plugin-host): #30 H2 — StatEntry u64 + salvaguarda LIST anti-overwrite"
```

---

## Task 3: H2 — 5 GiB MLSD e2e (ci-visible)

**Files:**
- Modify: `crates/norte-plugin-host/tests/ftp_plugin_e2e.rs`

- [ ] **Step 1: Add the test.** Uses the `configured_ftp_provider` helper (from the M1 work). Seeds a SPARSE 5 GiB file with `set_len` (no real disk use). libunftp advertises MLSD, so this drives `parse_mlsd_facts`.

```rust
#[test]
fn ftp_mlsd_size_mayor_de_4gib() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Fichero DISPERSO de 5 GiB (set_len no escribe bloques): > u32::MAX.
    let f = std::fs::File::create(dir.path().join("huge.bin")).expect("crear");
    f.set_len(5 * 1024 * 1024 * 1024).expect("set_len 5 GiB");
    drop(f);
    let Some((_rt, mut inst)) = configured_ftp_provider(dir.path().to_path_buf()) else {
        return;
    };
    // stat: size == 5 GiB exacto (no truncado a usize/u32, no NotFound/Io).
    let st = inst
        .stat(&[b"huge.bin".to_vec()])
        .expect("stat sin trap")
        .expect("huge.bin existe");
    assert_eq!(st.size, Some(5 * 1024 * 1024 * 1024), "size u64 sin truncar");
    // list: la entrada aparece con su size, la página NO falla.
    let page = inst
        .list_dir(&[], None)
        .expect("list sin trap")
        .expect("raíz lista");
    let e = page
        .entries
        .iter()
        .find(|e| e.name == b"huge.bin")
        .expect("huge.bin listado");
    assert_eq!(e.size, Some(5 * 1024 * 1024 * 1024));
}
```

- [ ] **Step 2: Run it.**

Run: `cargo nextest run -p norte-plugin-host --test ftp_plugin_e2e -E 'test(ftp_mlsd_size)'`
Expected: PASS (SKIP without the wasm target). If libunftp does NOT return `size=` in its MLSD for the file, adjust the assertion to accept `None` only if the size fact is genuinely absent — but libunftp does emit `size` for files, so `Some(5 GiB)` is expected.

- [ ] **Step 3: Commit.**

```bash
git add crates/norte-plugin-host/tests/ftp_plugin_e2e.rs
git commit -m "test(plugin-host): #30 H2 — e2e MLSD size 5 GiB sin truncar"
```

---

## Task 4: M2 — per-op timeout + dead flag in the adapter

**Files:**
- Modify: `crates/norte-core/src/plugin_provider.rs`
- Test: `crates/norte-core/tests/plugin_timeout_e2e.rs` (create)

- [ ] **Step 1: Add the timeout const, the `dead`/`op_timeout` fields, and a test override.** In `plugin_provider.rs`, near the top:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Timeout por operación del guest (M2, ADR 0033): una llamada bloqueada en un
/// socket wasip2 no la corta el epoch deadline (sólo traba CPU del guest). Al
/// expirar, la op falla y el provider se marca muerto. 30 s = orden del connect.
const OP_TIMEOUT: Duration = Duration::from_secs(30);
```

Add to `PluginProvider`:
```rust
    /// `true` cuando una op expiró (M2): el hilo `spawn_blocking` colgado retiene
    /// el `Mutex` para siempre, así que toda op futura falla rápido sin tocarlo.
    dead: Arc<AtomicBool>,
    /// Timeout por op (override en tests para no esperar 30 s).
    op_timeout: Duration,
```

Initialise both in `from_instance` (`dead: Arc::new(AtomicBool::new(false))`, `op_timeout: OP_TIMEOUT`). Add a builder for tests:
```rust
    /// Fija un timeout por op corto (tests de M2). Doc-hidden: no es API de prod.
    #[doc(hidden)]
    #[must_use]
    pub fn with_op_timeout(mut self, timeout: Duration) -> Self {
        self.op_timeout = timeout;
        self
    }
```

- [ ] **Step 2: Guard `PluginProvider::call`** with the dead check + timeout. Replace the body:

```rust
    async fn call<T, F>(&self, f: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(&mut ProviderInstance) -> Result<T, Error> + Send + 'static,
    {
        if self.dead.load(Ordering::Relaxed) {
            return Err(Error::ProviderUnavailable { retryable: true });
        }
        let inst = Arc::clone(&self.inst);
        let dead = Arc::clone(&self.dead);
        let fut = tokio::task::spawn_blocking(move || {
            let mut guard = inst.lock().map_err(|_| Error::Internal { panic: true })?;
            f(&mut guard)
        });
        match tokio::time::timeout(self.op_timeout, fut).await {
            Ok(join) => join.map_err(|_| Error::Internal { panic: true })?,
            Err(_) => {
                // El hilo sigue colgado reteniendo el Mutex: marca muerto el
                // provider para que las ops futuras no bloqueen (M2, leak aceptado).
                dead.store(true, Ordering::Relaxed);
                Err(Error::ProviderUnavailable { retryable: true })
            }
        }
    }
```

- [ ] **Step 3: Guard the `read` `try_unfold` `spawn_blocking`.** In `PluginProvider::read`, the stream state must carry `dead` and `op_timeout`. Thread them into the `try_unfold` seed and apply the same dead-check + timeout around the inner `spawn_blocking`. Change the seed tuple from `(inst, segs, offset, 0u64)` to `(inst, dead, timeout, segs, offset, 0u64)` and the closure:

```rust
        let inst = Arc::clone(&self.inst);
        let dead = Arc::clone(&self.dead);
        let timeout = self.op_timeout;
        let stream = stream::try_unfold(
            (inst, dead, timeout, segs, offset, 0u64),
            move |(inst, dead, timeout, segs, off, done)| async move {
                const CHUNK: u64 = 64 * 1024;
                let want = match limit {
                    Some(l) => {
                        let remaining = l.saturating_sub(done);
                        if remaining == 0 {
                            return Ok(None);
                        }
                        remaining.min(CHUNK)
                    }
                    None => CHUNK,
                };
                if dead.load(Ordering::Relaxed) {
                    return Err(Error::ProviderUnavailable { retryable: true });
                }
                let inst2 = Arc::clone(&inst);
                let segs2 = segs.clone();
                let fut = tokio::task::spawn_blocking(move || {
                    let mut g = inst2.lock().map_err(|_| Error::Internal { panic: true })?;
                    g.read(&segs2, off, want)
                        .map_err(|e| map_runtime_error(&e))?
                        .map_err(map_vfs_error)
                });
                let chunk: Vec<u8> = match tokio::time::timeout(timeout, fut).await {
                    Ok(join) => join.map_err(|_| Error::Internal { panic: true })??,
                    Err(_) => {
                        dead.store(true, Ordering::Relaxed);
                        return Err(Error::ProviderUnavailable { retryable: true });
                    }
                };
                if chunk.is_empty() {
                    return Ok(None);
                }
                let mut chunk = chunk;
                if chunk.len() as u64 > want {
                    chunk.truncate(usize::try_from(want).unwrap_or(usize::MAX));
                }
                let n = chunk.len() as u64;
                Ok(Some((
                    Bytes::from(chunk),
                    (inst, dead, timeout, segs, off.saturating_add(n), done + n),
                )))
            },
        );
        Ok(stream.boxed())
```

- [ ] **Step 4: Guard `PluginByteSink`.** Give it `dead: Arc<AtomicBool>` and `op_timeout: Duration`, populate them in `write()` (clone from `self.dead`, copy `self.op_timeout`), and apply the same dead-check + timeout in `PluginByteSink::call`. Update the struct:

```rust
struct PluginByteSink {
    inst: Arc<Mutex<ProviderInstance>>,
    dead: Arc<AtomicBool>,
    op_timeout: Duration,
    writer: Option<norte_plugin_host::WriterHandle>,
}
```

In `PluginProvider::write`, build it with `dead: Arc::clone(&self.dead), op_timeout: self.op_timeout,`. Rewrite `PluginByteSink::call` (the static helper) to a method taking `&self` (so it can read `self.dead`/`self.op_timeout`), or pass them in. Simplest: make it an associated fn taking the three pieces:

```rust
    async fn call<F>(
        inst: &Arc<Mutex<ProviderInstance>>,
        dead: &Arc<AtomicBool>,
        op_timeout: Duration,
        f: F,
    ) -> Result<(), Error>
    where
        F: FnOnce(&mut ProviderInstance) -> Result<(), Error> + Send + 'static,
    {
        if dead.load(Ordering::Relaxed) {
            return Err(Error::ProviderUnavailable { retryable: true });
        }
        let inst = Arc::clone(inst);
        let dead = Arc::clone(dead);
        let fut = tokio::task::spawn_blocking(move || {
            let mut guard = inst.lock().map_err(|_| Error::Internal { panic: true })?;
            f(&mut guard)
        });
        match tokio::time::timeout(op_timeout, fut).await {
            Ok(join) => join.map_err(|_| Error::Internal { panic: true })?,
            Err(_) => {
                dead.store(true, Ordering::Relaxed);
                Err(Error::ProviderUnavailable { retryable: true })
            }
        }
    }
```

Update the three call sites in `ByteSink for PluginByteSink` (`write`/`commit`/`abort`) from `Self::call(&self.inst, move |g| ...)` to `Self::call(&self.inst, &self.dead, self.op_timeout, move |g| ...)`. The `Drop` impl (best-effort `try_lock` abort) is unchanged — it does not block, so it needs no timeout.

- [ ] **Step 5: Build core.**

Run: `cargo build -p norte-core`
Expected: compiles. Fix any field-init or borrow error.

- [ ] **Step 6: Write the M2 timeout test.** Create `crates/norte-core/tests/plugin_timeout_e2e.rs`. A TCP listener that accepts and never speaks makes the ftp guest's `configure` (USER/login) block; a short `op_timeout` trips it.

```rust
//! M2 (#30, ADR 0033): un servidor que acepta y CALLA cuelga la op del guest en
//! el socket; el timeout por-op del adapter la corta y marca el provider muerto,
//! y la SIGUIENTE op falla rápido (no espera otro timeout).
#![cfg(target_os = "linux")]

use std::time::Duration;

use norte_core::plugin_provider::PluginProvider;
use norte_plugin_host::{Capabilities as HostCaps, PluginRuntime};

const FTP_WASM: &[u8] = include_bytes!("../resources/ftp-provider.wasm");

#[tokio::test]
async fn op_bloqueada_expira_y_marca_muerto() {
    // Listener que acepta y jamás responde: el login del guest se cuelga leyendo.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        // Acepta y retiene la conexión abierta sin hablar.
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept() {
            held.push(sock);
        }
    });

    let runtime = PluginRuntime::new().expect("runtime");
    let provider = PluginProvider::from_bytes(
        runtime,
        FTP_WASM,
        HostCaps::with_net(vec!["127.0.0.1".to_owned()]),
        "ftp",
    )
    .expect("provider")
    .with_op_timeout(Duration::from_millis(300));

    // configure() cuelga en el login → expira ~300 ms → ProviderUnavailable.
    let t0 = std::time::Instant::now();
    let err = provider
        .configure(
            format!("127.0.0.1:{port}"),
            "anonymous".to_owned(),
            "anonymous".to_owned(),
            "/".to_owned(),
        )
        .await
        .expect_err("configure debe expirar");
    assert!(
        matches!(err, norte_proto::Error::ProviderUnavailable { .. }),
        "fue {err:?}"
    );
    assert!(t0.elapsed() < Duration::from_secs(5), "expiró rápido, no colgó");

    // 2ª op: el provider está muerto → falla INMEDIATA (no otro timeout de 300ms).
    let t1 = std::time::Instant::now();
    let err2 = provider
        .configure(
            format!("127.0.0.1:{port}"),
            "anonymous".to_owned(),
            "anonymous".to_owned(),
            "/".to_owned(),
        )
        .await
        .expect_err("2ª op también falla");
    assert!(matches!(err2, norte_proto::Error::ProviderUnavailable { .. }));
    assert!(
        t1.elapsed() < Duration::from_millis(100),
        "la 2ª op fue inmediata (dead flag), tardó {:?}",
        t1.elapsed()
    );
}
```

- [ ] **Step 7: Run the M2 test.**

Run: `cargo nextest run -p norte-core --test plugin_timeout_e2e`
Expected: PASS. If `configure`'s guest `connect` itself blocks before login (the listener accepts, so the TCP connect succeeds and the block is at the login read — correct), the timeout still fires.

- [ ] **Step 8: Commit.**

```bash
git add crates/norte-core/src/plugin_provider.rs crates/norte-core/tests/plugin_timeout_e2e.rs
git commit -m "feat(core): #30 M2 — timeout por-op del plugin + dead-flag (fail-fast)"
```

---

## Task 5: Docs, full CI, reviewers

- [ ] **Step 1: Update ADR 0033 debt notes.** In `docs/adr/0033-ftp-provider-as-plugin.md`, amend the H2 and M2 debt bullets: H2 is now fixed for MLSD (u64 self-parse) with the LIST ≥4 GiB *visibility* residual (overwrite hazard closed); M2 is now bounded by `OP_TIMEOUT` with the leaked-thread cost documented. Keep the residuals explicit.

- [ ] **Step 2: `just ci`.**

Run: `cargo llvm-cov clean --workspace && just ci`
Expected: EXIT=0.

- [ ] **Step 3: Reviewers.**
  - **rust-reviewer** — the `stat_remote` `StatEntry` refactor (all callers updated, no lost `is_directory`/size semantics), the `list_dir` borrow split, the M2 timeout wrapper (leak correctness, dead-flag races, all three `spawn_blocking` sites covered incl. the read stream and the sink), `Ordering::Relaxed` adequacy.
  - **encoding-auditor** — `parse_mlsd_facts` name extraction (raw, after first space, `;` in name survives, U+FFFD/`/`/NUL still rejected downstream), `ls_l_name` leniency (headers rejected, internal-space names), and that the LIST safeguard compares bytes correctly (`n == child`).
  - **security-reviewer** — M2: does the dead-flag/leak create any bypass or resource-exhaustion vector (a hostile server tripping many timeouts leaks many threads)? Is `OP_TIMEOUT` per-op DoS-appropriate? H2 LIST safeguard: does failing loud on a name match leak server-controlled data or enable a probe?

- [ ] **Step 4: Apply feedback** (`superpowers:receiving-code-review`), re-run `just ci`, commit.

- [ ] **Step 5: Finish the branch** (`superpowers:finishing-a-development-branch`).

---

## Self-review

- **Spec coverage:** H2 MLSD self-parse → T1 (`parse_mlsd_facts`) + T2 (wired into `stat_remote`/`list_dir`, `StatEntry`); LIST safeguard → T2 `stat_remote` `None`-arm with `ls_l_name`; H2 e2e 5 GiB → T3; M2 timeout+dead across all three spawn_blocking sites → T4 (call, read, sink) + test → T4 s6; docs → T5. Non-goals (no WIT change, no `ls -l` full re-parse, no thread cancellation) respected — no task adds them.
- **Placeholder scan:** the `name_owned_hack` in T2 s3 is explicitly called out as "do NOT invent" with the correct branch-split shape following it — the engineer implements the second shape. No other placeholders.
- **Type consistency:** `StatEntry { kind: EntryKind, size: Option<u64> }` used identically in `stat_remote`, `stat_entry_from_file`, and all three callers. `parse_mlsd_facts -> Option<(EntryKind, Option<u64>, &str)>` consistent between T1 def, T1 tests, and T2 usage. `ls_l_name -> Option<&str>` consistent. M2: `dead: Arc<AtomicBool>` + `op_timeout: Duration` consistent across `PluginProvider`, `PluginByteSink`, and the `read` seed tuple. `with_op_timeout` returns `Self`.
