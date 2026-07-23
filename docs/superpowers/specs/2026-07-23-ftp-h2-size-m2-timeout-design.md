# FTP H2 (4 GiB size ceiling) + M2 (guest socket timeout) — design

**Date:** 2026-07-23
**Status:** approved
**Related:** ADR 0033 (FTP provider as plugin, debts H2/M2), ADR 0032 (WIT provider interface)

Two independent #30 debts from ADR 0033, one guest-side (H2), one adapter-side (M2).

## H2 — wasm32 4 GiB size ceiling

### Problem

`suppaftp` parses FTP file sizes into `usize`. The FTP guest is always
`wasm32-wasip2` where `usize` is 32-bit, so a size ≥ 4 GiB fails to parse
(`ParseError::BadSize`) and the **entire** `File` parse returns `None`:

- MLSD (`ListParser::parse_mlsd`/`parse_mlst`, `list.rs:151`): the entry vanishes;
  `list_dir` fails the whole page with `Io`, `stat`/`mlst` of the file fails.
- LIST POSIX (`parse_posix`, `list.rs:280`): the line is dropped like a `total N`
  header, so the file becomes **invisible** — `stat` gives false `NotFound`,
  `exists()` returns false, and a `write`/`rename`/`mkdir` can silently land over
  the invisible ≥ 4 GiB file (data loss).

### Decision

**MLSD self-parse** (the modern path — `libunftp`/`vsftpd`/`proftpd` all advertise
MLSD) plus a **LIST anti-overwrite safeguard**. Full `ls -l` re-parsing (to list
≥ 4 GiB files on no-MLSD servers) is out of scope — locale-dependent, fragile, and
the no-MLSD-and-≥4-GiB case is rare.

#### 1. MLSD facts self-parser

New free fn in `ftp-provider/src/lib.rs`:

```rust
/// Parsea una línea MLSD/MLST (RFC 3659: `facts SP pathname`). Devuelve el kind,
/// el size como u64 (`None` si el fact `size` falta), y el nombre CRUDO (tras el
/// primer espacio). Reemplaza a `ListParser::parse_mlsd`/`parse_mlst` de suppaftp,
/// que parsea el size a `usize` (techo 4 GiB en wasm32) y que además truncaba el
/// nombre en `;`. `None` si la línea no tiene la forma `facts SP name`.
fn parse_mlsd_facts(line: &str) -> Option<(EntryKind, Option<u64>, &str)>
```

- Split on the FIRST space → `(facts, name)`. `name` raw (the caller still applies
  the `U+FFFD`/`/`/`\0` rejection). Empty name → `None`.
- Facts: `facts.split(';')`, each `key=value`. Case-insensitive key match:
  - `type`: `dir`/`cdir`/`pdir` → `Dir`, `file` → `File`, `link` → `Symlink`,
    anything else → `Other`. Missing `type` → `File` (RFC default is unusual, but
    matches suppaftp's `size:0` default; treat missing type as `File`).
  - `size`: `value.parse::<u64>().ok()` → the `Option<u64>`. A non-numeric or
    absent size → `None` size (not an error — dirs legitimately omit it).
- Used in `list_dir` (MLSD branch, replacing `ListParser::parse_mlsd(&line)` +
  `line.split_once(' ')`) and `stat_remote` (MLST branch, replacing
  `ListParser::parse_mlst`).

`Entry`/the guest's own `entry_from_file` path is bypassed for MLSD: the self-parser
yields `(kind, size, name)` directly, so `entry_from_file` (which reads
`File::size() -> usize`) is no longer on the MLSD path. `entry_from_file` stays for
the LIST path (small files; the ≥4 GiB limit there is the documented residual).

#### 2. LIST anti-overwrite safeguard

New lenient name extractor:

```rust
/// Nombre de una línea `ls -l` de forma TOLERANTE: 9+ campos separados por
/// whitespace, el nombre es todo tras el 8º campo (perms links owner group size
/// mon day time name). Los nombres con espacio inicial se pierden (límite conocido
/// de `ls -l`, igual que suppaftp); `None` si la línea no parece una entrada (p. ej.
/// una cabecera `total N`, <9 campos). Sólo para la salvaguarda anti-overwrite: NO
/// se usa como nombre real (eso sigue por `parse_list_line`).
fn ls_l_name(line: &str) -> Option<&str>
```

In `stat_remote`'s LIST loop: when `parse_list_line(line)` is `None`, try
`ls_l_name(line)`; if it equals `child` and contains no `U+FFFD`, return
`Err(VfsError::Io)` instead of `continue`. Effect: a ≥ 4 GiB target's line (which
fails suppaftp's size parse) is no longer silently skipped — `stat`/`exists` fail
loud, so `write`/`rename`/`mkdir` refuse rather than overwrite. `list_dir`'s LIST
branch still drops such lines (visibility limitation, documented debt — not data
loss).

### H2 residual debt (documented in ADR 0033)

`list_dir` over a no-MLSD server hides ≥ 4 GiB files (they don't appear in the
listing). The overwrite hazard is closed; the visibility gap remains until a full
`ls -l` self-parse.

## M2 — guest socket timeout

### Problem

The wasmtime epoch deadline only traps *guest CPU*, not a guest blocked inside a
wasip2 socket syscall (connect/read/write). A stalled or silent FTP server hangs
the adapter's `spawn_blocking` worker thread indefinitely; that thread holds the
`Mutex<ProviderInstance>`, so every subsequent op on the provider queues forever.
Task cancellation only drops the future. The RETR cache's drain-to-EOF (M1) widens
the window.

### Decision

Wrap every `spawn_blocking` in the adapter with `tokio::time::timeout`; on expiry,
mark the provider dead and fail fast thereafter.

- `PluginProvider` gains `dead: Arc<AtomicBool>`, shared into `PluginByteSink`.
- A const `OP_TIMEOUT: Duration = Duration::from_secs(30)` (matches the connect
  timeout order of magnitude). A test-only constructor override allows a short
  timeout so tests don't wait 30 s.
- The three `spawn_blocking` sites — `PluginProvider::call`, the `read`
  `try_unfold` inner `spawn_blocking`, and `PluginByteSink::call` — each:
  1. Check `dead` first; if set, return `Error::ProviderUnavailable { retryable:
     true }` without touching the mutex.
  2. Run `tokio::time::timeout(OP_TIMEOUT, spawn_blocking(...))`. On `Err(_)`
     (elapsed): set `dead = true`, return `Error::ProviderUnavailable { retryable:
     true }`.
- The timed-out `spawn_blocking` thread leaks (wasip2 socket I/O is not
  cancellable; the thread keeps the mutex). The `dead` flag ensures no future op
  blocks on that mutex. Documented as accepted.
- Applies to all plugin providers uniformly; the mem guests never trip it (fast,
  in-memory).

### M2 residual debt

The leaked worker thread and its held mutex persist until the whole
`PluginRuntime` is dropped. This is the accepted cost of an uncancellable blocking
call; the human reconnects (a fresh provider = fresh instance).

## Testing

- **H2 unit (no server):** `parse_mlsd_facts` on fixed lines — `type=file;size=5368709120;modify=...; big.bin` → `(File, Some(5_368_709_120), "big.bin")`; a dir line with no size → `(Dir, None, name)`; a name containing `;` survives (not truncated); a malformed line → `None`. `ls_l_name` on a real `ls -l` line, a `total 8` header (→ `None`), and a name with internal spaces.
- **H2 e2e (libunftp, wasm-gated):** seed a sparse ≥ 4 GiB file (`std::fs::File::set_len`), MLST/list it → `Entry.size == 5 GiB`, not `NotFound`/`Io`. (libunftp advertises MLSD, so this exercises the self-parser.)
- **M2:** a TCP listener that accepts and never speaks; a provider with a short test `OP_TIMEOUT` → the op returns `ProviderUnavailable` within the timeout, and a second op returns immediately (dead flag), not after another timeout.
- The shared `provider_contract!` (46 cases) and the M1 guard tests stay green.

## Non-goals

- No WIT change (H2 and M2 are both behind the existing interface).
- No `ls -l` full re-parse (H2 LIST ≥ 4 GiB visibility stays debt).
- No cancellation of the leaked thread (M2 accepts the leak).
