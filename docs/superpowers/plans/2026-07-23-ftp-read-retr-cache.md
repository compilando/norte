# FTP guest RETR cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the FTP provider guest reuse one live RETR data connection across the host's sequential chunked reads, turning O(N²) per-chunk RETR into O(N), with no WIT/wire change.

**Architecture:** Cache the in-progress RETR `DataStream` on the guest's `Session` keyed by `(remote, next_offset)`. A sequential `read` whose offset matches the cache continues the live stream; any mismatch or any control-issuing op first flushes (drains + `finalize_retr_stream`) the cache, so an FTP `226` transfer reply is never left pending while another command runs.

**Tech Stack:** `suppaftp` v10 sync (`FtpStream`, `retr_as_stream`/`finalize_retr_stream`) compiled to `wasm32-wasip2`; the embedded artifact at `crates/norte-core/resources/ftp-provider.wasm` (rebuilt via `just build-ftp-wasm`); in-process `libunftp` test server.

**Reference:** `docs/superpowers/specs/2026-07-23-ftp-read-retr-cache-design.md`.

---

## Task 1: Regression-guard tests (correctness must survive the optimization)

**Files:**
- Modify: `crates/norte-plugin-host/tests/ftp_plugin_e2e.rs` (add a driver test against `ProviderInstance` + libunftp)

These tests assert correctness that already holds on the current (slow) code — they are the guard that the caching in Task 2 does not desync the control connection. They must pass BEFORE (baseline) and AFTER.

- [ ] **Step 1: Add a helper + the tests.** Append to `crates/norte-plugin-host/tests/ftp_plugin_e2e.rs`. Reuse the existing `spawn_ftp_server` and `build_guest` in that file. The guest is driven through `ProviderInstance` (the low-level host binding), configuring it against the server, then exercising reads.

```rust
/// Configura un ProviderInstance del guest ftp-provider contra un libunftp sobre
/// `home`, listo para leer. Devuelve la instancia (o SKIP-None sin target wasm).
fn configured_ftp_provider(home: std::path::PathBuf) -> Option<norte_plugin_host::ProviderInstance> {
    let wasm = build_guest("ftp-provider")?;
    let port = spawn_ftp_server(home);
    let rt = Box::leak(Box::new(PluginRuntime::new().expect("runtime")));
    let mut inst = rt
        .instantiate_provider(&wasm, Capabilities::with_net(vec!["127.0.0.1".to_owned()]))
        .expect("instanciar provider");
    let cfg = norte_plugin_host::provider_iface::ProviderConfig {
        endpoint: format!("127.0.0.1:{port}"),
        user: "anonymous".to_owned(),
        password: "anonymous".to_owned(),
        base: "/".to_owned(),
    };
    inst.configure(&cfg).expect("configure sin trap").expect("configure ok");
    Some(inst)
}

/// Lee `path` por chunks de `chunk` bytes vía el guest (como hace el adapter),
/// reensamblando; para en el primer chunk corto (EOF). Devuelve los bytes.
fn read_all_chunked(
    inst: &mut norte_plugin_host::ProviderInstance,
    segments: &[Vec<u8>],
    chunk: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut off = 0u64;
    loop {
        let c = inst
            .read(segments, off, chunk)
            .expect("read sin trap")
            .expect("read ok");
        if c.is_empty() {
            break;
        }
        off += c.len() as u64;
        let short = (c.len() as u64) < chunk;
        out.extend_from_slice(&c);
        if short {
            break;
        }
    }
    out
}

#[test]
fn ftp_secuencial_grande_byte_exacto() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 512 KiB > varios chunks de 64 KiB: ejercita el reuso del RETR.
    let content: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.path().join("big.bin"), &content).expect("sembrar");
    let Some(mut inst) = configured_ftp_provider(dir.path().to_path_buf()) else {
        return;
    };
    let got = read_all_chunked(&mut inst, &[b"big.bin".to_vec()], 64 * 1024);
    assert_eq!(got, content, "lectura secuencial byte-exacta");
}

#[test]
fn ftp_intercalar_stat_no_desincroniza() {
    let dir = tempfile::tempdir().expect("tempdir");
    let content: Vec<u8> = (0..200 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.path().join("f.bin"), &content).expect("sembrar f");
    std::fs::write(dir.path().join("otro.txt"), b"hola").expect("sembrar otro");
    let Some(mut inst) = configured_ftp_provider(dir.path().to_path_buf()) else {
        return;
    };
    // Lee MEDIO fichero (un chunk), ABANDONA (no llega a EOF), luego stat de otro
    // path: si la caché no se drenara, el 226 pendiente desincronizaría el stat.
    let half = inst
        .read(&[b"f.bin".to_vec()], 0, 64 * 1024)
        .expect("read sin trap")
        .expect("read ok");
    assert_eq!(half.len(), 64 * 1024);
    let st = inst
        .stat(&[b"otro.txt".to_vec()])
        .expect("stat sin trap")
        .expect("otro.txt existe");
    assert_eq!(st.size, Some(4), "stat tras lectura abandonada NO desincroniza");
    // Y una relectura entera del primero sigue byte-exacta.
    let got = read_all_chunked(&mut inst, &[b"f.bin".to_vec()], 64 * 1024);
    assert_eq!(got, content, "relectura entera byte-exacta");
}

#[test]
fn ftp_rango_luego_list_ok() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("r.bin"), b"0123456789").expect("sembrar");
    let Some(mut inst) = configured_ftp_provider(dir.path().to_path_buf()) else {
        return;
    };
    // Rango acotado (offset 2, 3 bytes) = "234"; deja la caché viva.
    let slice = inst
        .read(&[b"r.bin".to_vec()], 2, 3)
        .expect("read sin trap")
        .expect("read ok");
    assert_eq!(slice, b"234");
    // list_dir de la raíz debe funcionar (flush de la caché antes del comando).
    let page = inst
        .list_dir(&[], None)
        .expect("list sin trap")
        .expect("raíz lista");
    assert!(
        page.entries.iter().any(|e| e.name == b"r.bin"),
        "list tras rango ve el fichero: la caché se drenó limpio"
    );
}
```

- [ ] **Step 2: Run against the CURRENT guest (baseline).**

Run: `cargo nextest run -p norte-plugin-host --test ftp_plugin_e2e -E 'test(ftp_secuencial) or test(ftp_intercalar) or test(ftp_rango)'`
Expected: PASS (current code is correct, just slow) — establishes the guard. (SKIP if `wasm32-wasip2` absent.)

- [ ] **Step 3: Commit the tests.**

```bash
git add crates/norte-plugin-host/tests/ftp_plugin_e2e.rs
git commit -m "test(plugin-host): #30 M1 — guardas de correcta lectura FTP (secuencial/intercalado/rango)"
```

---

## Task 2: The RETR cache in the guest

**Files:**
- Modify: `crates/norte-plugin-host/examples-wasm/ftp-provider/src/lib.rs`

- [ ] **Step 1: Add the cache field to `Session` + the `CachedRead` struct.** Replace the `Session` struct (currently `ftp`, `base`, `has_mlsd`, `seq`) — add `cached_read`. Put `CachedRead` right after it.

```rust
/// Sesión FTP establecida: conexión de control + estado de la raíz remota.
struct Session {
    ftp: FtpStream,
    /// Raíz remota absoluta bajo la que vive todo. Sin `..`, sin barra final.
    base: String,
    /// El servidor soporta MLSD/MLST (machine-readable). Si no, se degrada a
    /// `LIST` (`ls -l`), universal pero frágil con nombres hostiles (ADR 0014 C).
    has_mlsd: bool,
    /// Contador de staging (nombre efímero único para `open_writer`).
    seq: u64,
    /// RETR en curso reutilizable entre lecturas secuenciales (#30 M1): evita el
    /// re-RETR por chunk (O(n²)→O(n)). `None` = sin lectura en vuelo.
    cached_read: Option<CachedRead>,
}

/// Un RETR vivo cacheado: la conexión de datos + el path y el siguiente offset
/// que entregará. El `reader` es la conexión de DATOS (independiente del
/// control); se drena y finaliza vía [`flush_cached_read`] antes de cualquier
/// comando de control, para que el `226` pendiente jamás se intercale.
struct CachedRead {
    remote: String,
    next_offset: u64,
    reader: Box<dyn std::io::Read>,
}
```

- [ ] **Step 2: Set `cached_read: None` in `configure`.** In `Guest::configure`, the `SESSION.set(Some(Session { ... }))` gains the field:

```rust
        SESSION.set(Some(Session {
            ftp,
            base,
            has_mlsd,
            seq: 0,
            cached_read: None,
        }));
```

- [ ] **Step 3: Add `flush_cached_read`.** Place it next to `with_session` (a free fn taking `&mut Session`).

```rust
/// Drena y finaliza el RETR cacheado (si hay), dejando el control LIMPIO para el
/// siguiente comando. Best-effort e idempotente (`None` = no-op). Todo op que
/// emita un comando de control lo llama ANTES (invariante #30 M1: jamás un
/// comando con un `226` pendiente).
fn flush_cached_read(s: &mut Session) {
    let Some(mut cr) = s.cached_read.take() else {
        return;
    };
    // Drena el resto de la conexión de datos (RETR va offset→EOF; parar sin
    // drenar desincronizaría el control), luego lee la respuesta de transferencia.
    let mut scratch = [0u8; 8192];
    loop {
        match cr.reader.read(&mut scratch) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let _ = s.ftp.finalize_retr_stream(cr.reader);
}
```

- [ ] **Step 4: Rewrite `read` to use the cache.** Replace the whole `fn read` body (the one that currently does stat + REST + `retr_as_stream` + drain + finalize each call).

```rust
    fn read(segments: Vec<Vec<u8>>, offset: u64, len: u64) -> Result<Vec<u8>, VfsError> {
        with_session(|s| {
            let remote = remote(&s.base, &segments)?;
            // Reusa el RETR cacheado si casa el path Y el offset secuencial; si no,
            // finaliza el anterior y abre uno nuevo (stat + REST + RETR).
            let hit = s
                .cached_read
                .as_ref()
                .is_some_and(|cr| cr.remote == remote && cr.next_offset == offset);
            if !hit {
                flush_cached_read(s);
                match stat_remote(&mut s.ftp, &remote, s.has_mlsd, &s.base)? {
                    None => return Err(VfsError::NotFound),
                    Some(f) if f.is_directory() => return Err(VfsError::Conflict),
                    Some(_) => {}
                }
                if offset > 0 {
                    let off = usize::try_from(offset).map_err(|_| VfsError::Io)?;
                    s.ftp.resume_transfer(off).map_err(|e| map_err(&e))?;
                }
                let reader = s.ftp.retr_as_stream(remote.as_str()).map_err(|e| map_err(&e))?;
                s.cached_read = Some(CachedRead {
                    remote: remote.clone(),
                    next_offset: offset,
                    reader: Box::new(reader),
                });
            }
            // Lee hasta `len` bytes del reader cacheado.
            let want = usize::try_from(len).unwrap_or(usize::MAX);
            let cr = s.cached_read.as_mut().expect("caché instalada arriba");
            let mut out = Vec::new();
            let mut buf = [0u8; 8192];
            let mut eof = false;
            let mut read_err = false;
            while out.len() < want {
                match cr.reader.read(&mut buf) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => {
                        let take = n.min(want - out.len());
                        out.extend_from_slice(&buf[..take]);
                        // `take < n` no puede ocurrir: `buf` se llenó con `n<=8192`
                        // y `want-out.len()` sólo limita al final, donde el chunk de
                        // 8 KiB ya cabe salvo el último; si limita, los bytes de más
                        // quedan en el socket para la siguiente `read` — pero el
                        // adapter pide múltiplos, y aun si no, el offset siguiente
                        // no casaría y se re-RETRea. Para evitar perder bytes del
                        // socket, `want` se alinea a 8 KiB abajo (ver nota).
                    }
                    Err(_) => {
                        read_err = true;
                        break;
                    }
                }
            }
            cr.next_offset += out.len() as u64;
            if eof || read_err {
                // EOF o error: finaliza la caché (limpio o best-effort).
                flush_cached_read(s);
            }
            if read_err {
                return Err(VfsError::Io);
            }
            Ok(out)
        })
    }
```

**Correctness note on the 8 KiB inner buffer vs `want`:** if `want` is not a
multiple of 8 KiB, the loop's last `read` could return more bytes than `want -
out.len()`, and `take` would drop the surplus already pulled off the socket —
corrupting the next sequential read (whose offset would then be wrong and force a
re-RETR, but the dropped bytes are gone from THIS read's window). To avoid that,
read directly into a right-sized tail slice instead of a fixed 8 KiB buffer. Use
this loop body instead (replaces the `while out.len() < want` block above):

```rust
            let want = usize::try_from(len).unwrap_or(usize::MAX);
            let cr = s.cached_read.as_mut().expect("caché instalada arriba");
            let mut out = vec![0u8; want.min(1 << 20)]; // techo por si want=usize::MAX
            let cap = out.len();
            let mut filled = 0usize;
            let mut eof = false;
            let mut read_err = false;
            while filled < cap {
                match cr.reader.read(&mut out[filled..]) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => filled += n,
                    Err(_) => {
                        read_err = true;
                        break;
                    }
                }
            }
            out.truncate(filled);
            cr.next_offset += filled as u64;
```

Reading into `out[filled..]` never pulls more than requested off the socket, so no
bytes are lost. The `1 << 20` ceiling caps a hostile `len == u64::MAX` (the adapter
sends 64 KiB, so this never bites in practice). Delete the fixed-`buf` version;
keep only this slice-based loop plus the `eof/read_err` handling and the `Ok(out)`.

- [ ] **Step 5: Add `flush_cached_read(s)` to every control-issuing op.** At the very start of each `with_session(|s| { ... })` closure body in `stat`, `list_dir`, `open_writer`, `make_dir`, `remove`, `rename`, and in `FtpWriter::{write, commit, abort}`, insert `flush_cached_read(s);` as the first statement. Example for `stat`:

```rust
    fn stat(segments: Vec<Vec<u8>>) -> Result<Entry, VfsError> {
        with_session(|s| {
            flush_cached_read(s);
            // ... resto igual ...
```

Do the same first-line insertion in `list_dir`, `open_writer`, `make_dir`,
`remove`, `rename`. For `FtpWriter::write`/`commit`/`abort`, the body is
`with_session(|s| { ... })` — insert `flush_cached_read(s);` as the first line of
that closure (before `append_with_stream`/`rename`/`rm`). `configure` needs no
flush (fresh session).

- [ ] **Step 6: Update the module doc.** The current header documents the per-chunk
RETR as debt. Replace that paragraph:

```rust
//! LECTURA (#30 M1): la interfaz WIT `read(segs, offset, len)` es acotada, pero el
//! guest CACHEA el `DataStream` del RETR en la sesión y lo reutiliza mientras las
//! lecturas sean secuenciales (offset = fin del chunk anterior) → un solo RETR por
//! fichero, O(n). Cualquier otra op (o un offset no secuencial) drena y finaliza la
//! caché ANTES de emitir su comando de control (`flush_cached_read`), así el `226`
//! pendiente jamás se intercala. DEUDA (timeout/cancelación, ADR 0033): una lectura
//! bloqueada en el socket no la corta el epoch deadline; el hilo `spawn_blocking`
//! del host queda retenido — mitigación futura: `tokio::time::timeout` en el adapter.
```

- [ ] **Step 7: Rebuild the guest and refresh the embedded artifact.**

Run: `just build-ftp-wasm`
Expected: compiles clean; `crates/norte-core/resources/ftp-provider.wasm` updated.

- [ ] **Step 8: Run the Task 1 guards + the full FTP contract.**

Run: `cargo nextest run -p norte-plugin-host --test ftp_plugin_e2e -E 'test(ftp_secuencial) or test(ftp_intercalar) or test(ftp_rango)' && cargo nextest run -p norte-core --test ftp_provider_contract`
Expected: all PASS (guards still hold; the 44 contract cases stay green).

- [ ] **Step 9: Commit.**

```bash
git add crates/norte-plugin-host/examples-wasm/ftp-provider/src/lib.rs
git add -f crates/norte-core/resources/ftp-provider.wasm
git commit -m "perf(plugin-host): #30 M1 — caché RETR guest-side (lectura FTP O(n))"
```

---

## Task 3: Full CI + reviewers

- [ ] **Step 1: `just ci`.**

Run: `cargo llvm-cov clean --workspace && just ci`
Expected: EXIT=0. Fix any coverage/format issue.

- [ ] **Step 2: Reviewers.** This changes guest FTP control/data-connection sequencing (correctness + desync risk) but touches no wire. Dispatch:
  - **rust-reviewer** — the cache borrow flow (`Session.ftp` vs `cached_read`), the flush-before-every-op invariant completeness (did any control-issuing path miss a flush?), the slice-based read loop (no lost socket bytes), error/EOF finalize paths, `Box<dyn Read>` lifetime.
  - **encoding-auditor** — confirm the read path still returns byte-exact content across chunk boundaries and that the cache keying (`remote` string equality) matches the same `remote()` used elsewhere (no divergence on hostile names).

- [ ] **Step 3: Apply feedback** (`superpowers:receiving-code-review`), re-run `just ci`, commit.

- [ ] **Step 4: Finish the branch** (`superpowers:finishing-a-development-branch`).

---

## Self-review

- **Spec coverage:** `CachedRead` + `Session.cached_read` (T2 s1/s2), `flush_cached_read` (T2 s3), `read` reuse-on-match (T2 s4), flush before every control op (T2 s5), rebuild artifact (T2 s7), all four test kinds — sequential large, interleave, range-then-op, cancellation-flush (T1: `ftp_secuencial_grande`, `ftp_intercalar_stat`, `ftp_rango_luego_list`; the interleave test doubles as the cancellation-flush guard since it abandons a read then runs `stat`). No WIT/adapter/mem change (spec non-goals) — none appear in any task.
- **Placeholder scan:** none — every step has full code.
- **Type consistency:** `CachedRead { remote: String, next_offset: u64, reader: Box<dyn std::io::Read> }` used identically in the struct def, `configure` (via `None`), `flush_cached_read`, and `read`. `flush_cached_read(s: &mut Session)` signature consistent across all call sites. The read loop uses the slice-based version (the fixed-`buf` version is explicitly deleted in T2 s4).
