# RAR, read-only, by delegation — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Browse and read `.rar` archives as directories through an installed
`7z` or `unrar`, with no non-free code in the dependency graph and no path to
the user's filesystem for the child process.

**Architecture:** A new crate `norte-vfs-rar` holds an archive **path**, not an
inner provider, so it cannot reach a remote byte and cannot know about other
providers. `Engine::provider_for` enforces "local inner only" at dispatch. The
crate splits three ways: a pure listing **parser** over delegate stdout bytes, a
**process runner** that owns rule-9 hardening, and the `Provider` impl over an
index cache. Fixtures are *generated* by a minimal RAR5 stored-entry writer in
`norte-testkit`.

**Tech stack:** Rust, `tokio::process`, `async_trait`, the existing
`norte_vfs::readonly_contract!` macro, nextest.

**Design:** `docs/superpowers/specs/2026-08-16-rar-by-delegation-design.md` —
read it first, especially the measured delegate table.

---

## What was already established, so you do not rediscover it

Measured on this machine against a real fixture (`unrar` 7.23, `7z` 26.02):

- **`7z` preserves raw name bytes; `unrar` truncates a non-UTF-8 name at the
  first invalid byte.** `cp437-\xa4\xa5.txt` lists as `cp437-` under `unrar` —
  extension lost — and as `cp437-\244\245.txt` under `7z`. Hence 7z is preferred.
- **Both treat an entry name as a glob.** `star?name.txt` extracts two entries
  from both. The refusal in Task 6 is required, not defensive.
- **`7z e -so` accepts raw non-UTF-8 bytes as the entry argument** and returns
  that entry's content.
- **A RAR5 archive with stored entries can be written in ~120 lines** and both
  delegates read it. The prototype is described byte-for-byte in Task 1.

## Gate budget (CLAUDE.md)

`just t <crate>` freely during RED→GREEN. **One** `just ci-fast` after Task 6.
**One** `just ci` before the merge. Never use the gate as a debugger.

## File structure

| file | responsibility |
| --- | --- |
| `crates/norte-testkit/src/smith.rs` | `RarSmith`: forge RAR5 bytes with stored entries |
| `crates/norte-vfs-rar/src/lib.rs` | crate docs, re-exports, `RarLimits` |
| `crates/norte-vfs-rar/src/delegate.rs` | discovery, argv construction, process spawn, rule 9 |
| `crates/norte-vfs-rar/src/listing.rs` | **pure** parsers: `7z -slt` and `unrar vt` bytes → entries |
| `crates/norte-vfs-rar/src/index.rs` | entry tree, name safety, skipped counter, cache |
| `crates/norte-vfs-rar/src/provider.rs` | `Provider` impl: stat/list/read/range, ambiguity refusal |
| `crates/norte-proto/src/vpath.rs` | `ARCHIVE_FORMATS` gains `"rar"` |
| `crates/norte-core/src/engine.rs` | dispatch arm + local-only refusal |
| `crates/norte-config/src/schema.rs`, `load.rs` | `[archive] rar_delegate`, user layer only |

---

### Task 1: A RAR5 writer for stored entries, in the testkit

Without this there are no fixtures at all: the compressor is the non-free half,
so nothing in the tree can produce a `.rar`. Storing raw bytes inside the
documented container does not touch the compression algorithm.

**Files:**
- Modify: `crates/norte-testkit/src/smith.rs` (it already forges ZIP and TAR
  fixture bytes and owns the `crc32` helper — RAR belongs beside them, not in a
  new module)
- Modify: `crates/norte-testkit/src/lib.rs` (re-export)

**DONE (2026-08-16).** The API is `RarSmith::new().file(name, content).build() ->
Vec<u8>`, matching `ZipSmith`/`TarSmith`, plus `which_7z()`. A real `7z` lists
what it forges, raw non-UTF-8 name bytes included.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// El writer produce un archivo que el DELEGADO REAL sabe leer. Sin esto,
    /// un writer "correcto" según nuestra propia lectura no demuestra nada.
    #[test]
    fn un_delegado_real_lista_lo_que_escribimos() {
        let Some(sevenz) = which_7z() else {
            eprintln!("sin 7z instalado: test retirado");
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.rar");
        let bytes = RarSmith::new()
            .file(b"hello.txt", b"hola norte\n")
            .file("ñandú.txt".as_bytes(), b"utf8\n")
            .file(CRUDO, b"bytes\n")
            .build();
        std::fs::write(&path, &bytes).expect("escribe");

        let out = std::process::Command::new(sevenz)
            .args(["l", "-slt", "-p", "--"])
            .arg(&path)
            .output()
            .expect("7z corre");
        assert!(out.status.success(), "7z falló: {:?}", out);
        // Los BYTES crudos del nombre no-UTF8 sobreviven al listado.
        assert!(
            out.stdout
                .windows(15)
                .any(|w| w == b"cp437-\xa4\xa5.txt"),
            "el nombre crudo no aparece en el listado"
        );
    }

    #[test]
    fn vint_codifica_multibyte() {
        assert_eq!(vint(0), vec![0x00]);
        assert_eq!(vint(0x7f), vec![0x7f]);
        assert_eq!(vint(0x80), vec![0x80, 0x01]);
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `just t norte-testkit`
Expected: FAIL — `write_rar5`, `Rar5Entry`, `vint`, `which_7z` do not exist.

- [ ] **Step 3: Implement, using this verified byte layout**

Signatures:

```rust
/// Forja de bytes RAR5 con entradas ALMACENADAS, hermana de `ZipSmith`.
pub struct RarSmith { /* entries, mtime fijo */ }
impl RarSmith {
    pub fn new() -> Self;
    pub fn file(self, name: &[u8], content: &[u8]) -> Self;   // nombre en BYTES
    pub fn dir(self, name: &[u8]) -> Self;
    pub fn build(self) -> Vec<u8>;
}

/// El ejecutable `7z` si está en PATH (los tests se retiran si no).
pub fn which_7z() -> Option<std::path::PathBuf>;

fn vint(mut n: u64) -> Vec<u8>;   // 7 bits por byte, bit alto = continúa
```

The layout, verified working against `unrar` 7.23 and `7z` 26.02:

```text
signature: 52 61 72 21 1A 07 01 00

every block:
    u32 LE crc32(  vint(header_size) ++ inner  )
    vint(header_size)                          // longitud de `inner`
    inner = vint(head_type) ++ vint(head_flags)
            ++ [vint(extra_size)  if head_flags & 0x0001]
            ++ [vint(data_size)   if head_flags & 0x0002]
            ++ body

main header:  head_type = 1, head_flags = 0, body = vint(0)   // ArchiveFlags
end header:   head_type = 5, head_flags = 0, body = vint(0)   // EndFlags

file header:  head_type = 2, head_flags = 0x0002 (data_size = len(content))
    body = vint(file_flags)         // 0x0002 mtime presente | 0x0004 crc presente
        ++ vint(unpacked_size)
        ++ vint(0x20)               // attributes
        ++ u32 LE mtime             // unix
        ++ u32 LE crc32(content)
        ++ vint(0)                  // CompressionInfo: versión 0, método 0, dict 0
        ++ vint(1)                  // HostOS: 1 = unix
        ++ vint(name.len()) ++ name
    seguido INMEDIATAMENTE por los `content` bytes crudos.
```

Directories: a file header with `attributes` carrying the directory bit and no
data. v1 fixtures do not need them — `dir/nested.txt` gives the tree its shape,
which is how ZIP fixtures already do it.

- [ ] **Step 4: Green**

Run: `just t norte-testkit`
Expected: PASS (or the 7z test retiring itself with its message on a machine
without `7z`).

- [ ] **Step 5: Commit**

```bash
git add crates/norte-testkit/src/rar5.rs crates/norte-testkit/src/lib.rs
git commit -m "test(testkit): a RAR5 fixture writer, because the compressor is the non-free half"
```

---

### Task 2: The crate, and finding a delegate

**Files:**
- Create: `crates/norte-vfs-rar/` (use the `new-crate` skill — it wires the
  workspace lints, the license headers and the `#![warn(missing_docs)]` this
  repository requires)
- Create: `crates/norte-vfs-rar/src/delegate.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn sin_delegado_el_error_NOMBRA_el_ejecutable() {
    let err = Delegate::discover_in(&[]).expect_err("sin candidatos falla");
    let msg = err.to_string();
    assert!(msg.contains("7z") && msg.contains("unrar"), "el error debe decir QUÉ instalar: {msg}");
}

#[test]
fn se_prefiere_7z_a_unrar() {
    // Orden medido, no gusto: unrar TRUNCA un nombre no-UTF8 en el listado.
    let found = Delegate::discover_in(&[
        ("unrar", PathBuf::from("/usr/bin/unrar")),
        ("7z", PathBuf::from("/usr/bin/7z")),
    ])
    .expect("hay candidatos");
    assert!(matches!(found, Delegate::SevenZip(_)), "7z gana a unrar");
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-vfs-rar`
Expected: FAIL — `Delegate` does not exist.

- [ ] **Step 3: Implement**

```rust
/// El programa externo que hace de lector de RAR.
#[derive(Debug, Clone)]
pub enum Delegate { SevenZip(PathBuf), Unrar(PathBuf) }

impl Delegate {
    /// Sondea PATH: `7z`, `7zz`, `unrar`, en ese orden.
    pub fn discover() -> Result<Self, RarError>;
    /// Testable: los candidatos ya resueltos, en orden de preferencia.
    pub fn discover_in(candidates: &[(&str, PathBuf)]) -> Result<Self, RarError>;
}
```

- [ ] **Step 4: Green.** Run: `just t norte-vfs-rar`

- [ ] **Step 5: Commit**

```bash
git add crates/norte-vfs-rar Cargo.toml
git commit -m "feat(vfs-rar): the crate, and a delegate chosen by what it can carry"
```

---

### Task 3: `[archive] rar_delegate`, and the layer it must never come from

A key that names an executable, honoured from a `.norte.toml` inside a
repository, is arbitrary code execution on `cd`. `[archive]` already carries
"never from Project" for its limits (`crates/norte-config/src/load.rs:1365`).

**Files:**
- Modify: `crates/norte-config/src/schema.rs:63-80` (the `[archive]` section)
- Modify: `crates/norte-config/src/load.rs:1464-1490` (the merge function)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn rar_delegate_del_layer_project_se_IGNORA() {
    // Un repo que trae su propio .norte.toml no elige QUÉ binario se ejecuta.
    let user = r#"[archive]
rar_delegate = "/usr/bin/7z"
"#;
    let project = r#"[archive]
rar_delegate = "/tmp/evil"
"#;
    let merged = merge_layers_for_test(&[(Layer::User, user), (Layer::Project, project)]);
    assert_eq!(
        merged.archive_rar_delegate.as_deref(),
        Some("/usr/bin/7z"),
        "el layer Project jamás elige el ejecutable"
    );
}
```

(Use whatever the neighbouring `[archive]` limit tests already use to build
layers; copy their shape rather than inventing one.)

- [ ] **Step 2: Run and watch it fail.** Run: `just t norte-config`

- [ ] **Step 3: Implement** — `pub rar_delegate: Option<String>` in the archive
section, merged last-wins **only** for non-Project layers, exactly as
`archive_max_nesting` is.

- [ ] **Step 4: Green.** Run: `just t norte-config`

- [ ] **Step 5: Commit**

```bash
git add crates/norte-config
git commit -m "feat(config): [archive] rar_delegate, and never from a repository"
```

---

### Task 4: The listing parser, pure over bytes

The parser never spawns anything. That is what makes the encoding rules
testable without a delegate installed.

**Files:**
- Create: `crates/norte-vfs-rar/src/listing.rs`

- [ ] **Step 1: Write the failing tests, with REAL recorded output**

```rust
/// Salida real de `7z l -slt` (7-Zip 26.02) recortada a dos entradas.
const SEVENZ_SLT: &[u8] = b"\
Listing archive: t.rar\n\
\n\
--\n\
Path = t.rar\n\
Type = Rar5\n\
Solid = -\n\
\n\
----------\n\
Path = hello.txt\n\
Folder = -\n\
Size = 11\n\
Modified = 2021-01-14 10:25:36\n\
Encrypted = -\n\
Solid = -\n\
CRC = DB187CE4\n\
\n\
Path = cp437-\xa4\xa5.txt\n\
Folder = -\n\
Size = 16\n\
Modified = 2021-01-14 10:25:36\n\
Encrypted = -\n\
Solid = -\n\
";

#[test]
fn slt_ignora_la_cabecera_del_ARCHIVO_y_conserva_bytes_crudos() {
    let out = parse_7z_slt(SEVENZ_SLT);
    // `Path = t.rar` es el archivo mismo, no una entrada: va antes del `----------`.
    assert_eq!(out.entries.len(), 2, "la cabecera no es una entrada");
    assert_eq!(out.entries[0].name, b"hello.txt");
    assert_eq!(out.entries[0].size, 11);
    assert_eq!(out.entries[1].name, b"cp437-\xa4\xa5.txt", "bytes crudos, no lossy");
}

/// Salida real de `unrar vt` (UNRAR 7.23). El nombre no-UTF8 llega TRUNCADO
/// por el propio unrar: `cp437-` sin extensión. No es un bug del parser.
const UNRAR_VT: &[u8] = b"\
\n\
Archive: t.rar\n\
Details: RAR 5\n\
\n\
        Name: hello.txt\n\
        Type: File\n\
        Size: 11\n\
       mtime: 2021-01-14 09:25:36,000000000\n\
  Attributes: ----r-----\n\
\n\
        Name: dir/nested.txt\n\
        Type: Directory\n\
        Size: 0\n\
       mtime: 2021-01-14 09:25:36,000000000\n\
  Attributes: ----r-----\n\
";

#[test]
fn vt_lee_nombre_tipo_y_tamano() {
    let out = parse_unrar_vt(UNRAR_VT);
    assert_eq!(out.entries.len(), 2);
    assert_eq!(out.entries[0].name, b"hello.txt");
    assert!(!out.entries[0].is_dir);
    assert!(out.entries[1].is_dir, "Type: Directory");
}

#[test]
fn un_nombre_con_salto_de_linea_se_SALTA_y_se_CUENTA() {
    // Una salida por líneas no puede llevar un `\n` dentro de un nombre sin
    // adivinar. Adivinar aquí es enseñar un fichero que no es ese fichero.
    let raw = b"----------\nPath = ok.txt\nSize = 1\n\nPath = mal\nnombre.txt\nSize = 2\n";
    let out = parse_7z_slt(raw);
    assert_eq!(out.entries.len(), 1);
    assert_eq!(out.skipped, 1, "saltada y CONTADA, como ADR 0018");
}
```

- [ ] **Step 2: Run and watch it fail.** Run: `just t norte-vfs-rar`

- [ ] **Step 3: Implement**

```rust
/// Una entrada tal y como la IMPRIMIÓ el delegado (aún sin validar).
pub struct RawEntry {
    pub name: Vec<u8>, pub size: u64, pub is_dir: bool,
    pub mtime: Option<i64>, pub encrypted: bool, pub solid: bool,
}
/// Resultado de un parse: entradas + cuántas se saltaron (ADR 0018).
pub struct Listing { pub entries: Vec<RawEntry>, pub skipped: u64 }

pub fn parse_7z_slt(stdout: &[u8]) -> Listing;
pub fn parse_unrar_vt(stdout: &[u8]) -> Listing;
```

Rules both parsers share: split on `\n`; a record ends at a blank line; a field
is `<key> = <value>` (7z) or `<key>: <value>` after leading spaces (unrar); a
record whose continuation lines do not parse as fields is **skipped and
counted**; values are `Vec<u8>`, never `String`.

- [ ] **Step 4: Green.** Run: `just t norte-vfs-rar`

- [ ] **Step 5: Commit**

```bash
git add crates/norte-vfs-rar/src/listing.rs
git commit -m "feat(vfs-rar): parse what the delegate printed, as bytes"
```

---

### Task 5: The child process, and rule 9

**Files:**
- Modify: `crates/norte-vfs-rar/src/delegate.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn argv_de_listado_lleva_separador_y_sin_password() {
    let d = Delegate::SevenZip(PathBuf::from("/usr/bin/7z"));
    let argv = d.list_argv(Path::new("/tmp/a.rar"));
    assert!(argv.contains(&OsString::from("--")), "todo argumento va tras `--`");
    assert!(argv.iter().any(|a| a == "-p"), "password vacía: jamás una pregunta por stdin");
    assert_eq!(argv.last().unwrap(), "/tmp/a.rar");
}

#[test]
fn argv_de_lectura_pasa_el_nombre_en_BYTES() {
    use std::os::unix::ffi::OsStrExt;
    let d = Delegate::SevenZip(PathBuf::from("/usr/bin/7z"));
    let argv = d.read_argv(Path::new("/tmp/a.rar"), b"cp437-\xa4\xa5.txt");
    assert_eq!(argv.last().unwrap().as_bytes(), b"cp437-\xa4\xa5.txt");
}

/// La propiedad es "no se cuelga". Con stdin ABIERTO este test tarda para
/// siempre; con stdin a null, el hijo muere solo.
#[tokio::test]
async fn el_hijo_NUNCA_espera_en_stdin() {
    let d = Delegate::Unrar(PathBuf::from("/bin/cat")); // `cat` lee stdin hasta EOF
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        d.run_capture(&[OsString::from("--")], Duration::from_secs(30)),
    )
    .await;
    assert!(out.is_ok(), "stdin abierto: el hijo se quedó esperando");
}

#[tokio::test]
async fn cancelar_mata_al_hijo() {
    let token = CancellationToken::new();
    let d = Delegate::Unrar(PathBuf::from("/bin/cat"));
    let handle = tokio::spawn({
        let token = token.clone();
        async move { d.run_stream(&[], token).await }
    });
    token.cancel();
    let r = tokio::time::timeout(Duration::from_secs(5), handle).await;
    assert!(r.is_ok(), "el hijo sobrevivió a la cancelación");
}
```

- [ ] **Step 2: Run and watch them fail.** Run: `just t norte-vfs-rar`

- [ ] **Step 3: Implement**

```rust
impl Delegate {
    pub fn list_argv(&self, archive: &Path) -> Vec<OsString>;
    pub fn read_argv(&self, archive: &Path, entry: &[u8]) -> Vec<OsString>;
    async fn command(&self, argv: &[OsString]) -> tokio::process::Command;
    pub async fn run_capture(&self, argv: &[OsString], timeout: Duration) -> Result<Vec<u8>, RarError>;
    pub async fn run_stream(&self, argv: &[OsString], cancel: CancellationToken) -> Result<ByteStream, RarError>;
}
```

`command` is where rule 9 lives, and every one of these is load-bearing:

```rust
cmd.stdin(Stdio::null())            // una pregunta de password no puede ocurrir
   .stdout(Stdio::piped())
   .stderr(Stdio::piped())
   .current_dir(empty_dir)          // nunca el árbol del usuario
   .env_clear()
   .kill_on_drop(true);
```

Argv per delegate (measured working):

| | list | read one entry to stdout |
| --- | --- | --- |
| 7z | `l -slt -p -- <archive>` | `e -so -bd -y -p -- <archive> <entry>` |
| unrar | `vt -p- -- <archive>` | `p -inul -p- -- <archive> <entry>` |

Also: a semaphore bounding concurrent children, and a wall-clock timeout that
kills.

- [ ] **Step 4: Green.** Run: `just t norte-vfs-rar`

- [ ] **Step 5: Commit**

```bash
git add crates/norte-vfs-rar/src/delegate.rs
git commit -m "feat(vfs-rar): a child that gets a path, a name and a pipe"
```

---

### Task 6: The provider, the index, and the wildcard refusal

**Files:**
- Create: `crates/norte-vfs-rar/src/index.rs`, `crates/norte-vfs-rar/src/provider.rs`

- [ ] **Step 1: Write the failing tests**

```rust
/// MEDIDO: `star?name.txt` saca DOS entradas de los dos delegados. Como el
/// índice completo ya está en memoria, la ambigüedad se decide ANTES de
/// arrancar un proceso, contra nuestro propio índice.
#[test]
fn un_nombre_que_es_GLOB_de_otro_se_RECHAZA() {
    let idx = ArchiveIndex::from_raw(vec![
        raw(b"star?name.txt", 7),
        raw(b"starXname.txt", 7),
    ]);
    assert!(matches!(idx.addressable(b"star?name.txt"), Err(RarError::AmbiguousForDelegate)));
    // El gemelo literal NO es ambiguo: no contiene metacaracteres.
    assert!(idx.addressable(b"starXname.txt").is_ok());
}

#[test]
fn un_nombre_inseguro_se_SALTA_y_se_CUENTA() {
    let idx = ArchiveIndex::from_raw(vec![
        raw(b"ok.txt", 1), raw(b"../fuera.txt", 1), raw(b"/abs.txt", 1), raw(b"con\0nul", 1),
    ]);
    assert_eq!(idx.len(), 1);
    assert_eq!(idx.skipped(), 3, "ADR 0018: saltadas y contadas, jamás fatales");
}

#[tokio::test]
async fn listar_y_leer_contra_un_delegado_real() {
    let Some(_) = norte_testkit::which_7z() else { eprintln!("sin 7z: retirado"); return };
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("t.rar");
    std::fs::write(&archive, norte_testkit::RarSmith::new()
        .file(b"docs/hello.txt", b"hola norte\n")
        .build()).unwrap();

    let p = RarProvider::new(archive, Delegate::discover().unwrap(), RarLimits::default());
    let root = VPath::parse("rar+file:///t.rar/!/").unwrap();
    let entries = collect(p.list(&root).await.unwrap()).await;
    assert_eq!(entries.len(), 1, "un directorio `docs`");

    let leaf = VPath::parse("rar+file:///t.rar/!/docs/hello.txt").unwrap();
    let bytes = read_all(p.read(&leaf, None).await.unwrap()).await;
    assert_eq!(bytes, b"hola norte\n");
}

#[tokio::test]
async fn un_rango_no_relee_el_archivo_entero() {
    // ... mismo montaje; read(&leaf, Some(ByteRange{off:5,len:5}))  == b" nort"
}

#[tokio::test]
async fn una_entrada_CIFRADA_se_lista_y_se_niega_a_leerse() {
    // El flag viene del parser (`Encrypted = +` / `*` en el nombre): no hace
    // falta un archivo cifrado de verdad para fijar la política.
    let idx = ArchiveIndex::from_raw(vec![raw_encrypted(b"secreto.txt", 10)]);
    let p = RarProvider::with_index_for_test(idx);
    let leaf = VPath::parse("rar+file:///t.rar/!/secreto.txt").unwrap();
    assert!(p.stat(&leaf).await.is_ok(), "cifrada pero VISIBLE");
    assert!(matches!(p.read(&leaf, None).await, Err(Error::Unsupported)),
        "leerla es lo que no se puede, y se dice");
}

#[tokio::test]
async fn tocar_el_archivo_INVALIDA_el_indice_cacheado() {
    // Misma invalidación que norte-vfs-archive: (mtime, size). Un índice
    // rancio enseña ficheros que ya no están.
    let (p, archive) = provider_con_una_entrada().await;
    assert_eq!(count(p.list(&root()).await.unwrap()).await, 1);
    reescribe_con_dos_entradas(&archive);
    assert_eq!(count(p.list(&root()).await.unwrap()).await, 2, "el índice se reconstruyó");
}
```

- [ ] **Step 2: Run and watch them fail.** Run: `just t norte-vfs-rar`

- [ ] **Step 3: Implement**

```rust
pub struct ArchiveIndex { /* árbol, contador de saltadas */ }
impl ArchiveIndex {
    pub fn from_raw(raw: Vec<RawEntry>) -> Self;         // aplica seguridad de nombres
    pub fn skipped(&self) -> u64;
    /// `Ok(())` si el nombre puede pedirse al delegado SIN ambigüedad: se
    /// prueba como glob contra este mismo índice.
    pub fn addressable(&self, name: &[u8]) -> Result<(), RarError>;
}

pub struct RarProvider { /* archivo, delegado, caché de índice por (mtime,size), límites */ }
impl RarProvider {
    pub fn new(archive: PathBuf, delegate: Delegate, limits: RarLimits) -> Self;
}
#[async_trait] impl Provider for RarProvider { /* scheme, capabilities, stat, list,
   list_skipped, read, y TODA mutación -> Error::Unsupported */ }
```

Name safety reuses ADR 0018's rules verbatim: absolute, empty component, `.`,
`..`, NUL, `!`, over-long, over-deep → skipped and counted. Capabilities:
`READ_ONLY | CASE_SENSITIVE | CASE_PRESERVING`. Index cached by (path, mtime,
size), same invalidation as `norte-vfs-archive`.

- [ ] **Step 4: Green.** Run: `just t norte-vfs-rar`

- [ ] **Step 5: Wire the read-only contract suite**

`crates/norte-vfs-rar/tests/contract_ro.rs` invokes `norte_vfs::readonly_contract!`
with a factory that writes the canonical tree the macro documents
(`crates/norte-vfs/src/contract_ro.rs:21-30`) using `norte_testkit::RarSmith`, and
the hostile-name subset the RAR5 writer can carry. Read that macro's doc comment
before writing the factory.

- [ ] **Step 6: Green.** Run: `just t norte-vfs-rar`

- [ ] **Step 7: Commit**

```bash
git add crates/norte-vfs-rar
git commit -m "feat(vfs-rar): a read-only provider, and a name it refuses to guess"
```

---

### Task 7: The wire, and where "local only" is enforced

**Files:**
- Modify: `crates/norte-proto/src/vpath.rs:70` (`ARCHIVE_FORMATS`)
- Modify: `crates/norte-core/src/engine.rs:690-735` (the dispatch)
- Modify: the protocol version and its golden tests

- [ ] **Step 1: Write the failing tests**

```rust
// norte-proto
#[test]
fn rar_es_un_formato_de_archivo() {
    let p = VPath::parse("rar+file:///a.rar/!/x.txt").unwrap();
    let r = p.archive_split().unwrap().unwrap();
    assert_eq!(r.format, "rar");
    assert_eq!(r.outer.to_wire(), "file:///a.rar");
}

// norte-core
#[tokio::test]
async fn rar_sobre_un_interior_REMOTO_se_niega_con_motivo() {
    // El delegado necesita una ruta local. Traerse el archivo entero es una
    // descarga que nadie pidió: se niega ANTES de componer un provider.
    let engine = test_engine();
    let p = VPath::parse("rar+sftp://host/a.rar/!/x.txt").unwrap();
    let err = engine.stat(&p).await.expect_err("un interior remoto no compone");
    assert!(matches!(err, Error::Unsupported));
}

#[tokio::test]
async fn rar_anidado_en_otro_archivo_tampoco_es_local() {
    let engine = test_engine();
    let p = VPath::parse("rar+zip+file:///o.zip/!/a.rar/!/x.txt").unwrap();
    assert!(matches!(engine.stat(&p).await, Err(Error::Unsupported)));
}
```

- [ ] **Step 2: Run and watch them fail.** Run: `just t norte-proto` and `just t norte-core`

- [ ] **Step 3: Implement** — `"rar"` in `ARCHIVE_FORMATS`; a `"rar" =>` arm in
`provider_for` that extracts an OS path from `aref.outer` **only** when its
scheme is `file` and it has no authority, and returns `Error::Unsupported`
otherwise. Bump the protocol minor version and update the goldens and the
`methods.rs` version history comment the way every previous bump did.

- [ ] **Step 4: Green.** Run: `just t norte-proto`, `just t norte-core`

- [ ] **Step 5: Dispatch reviewers before committing** (CLAUDE.md: the agent
doing the work dispatches its own). `protocol-guardian` is **mandatory** — this
task changes a wire whitelist. Give it the commit range, and ask specifically
whether an older core against a newer proto still answers honestly through the
existing `_ => Unsupported` arm. Also `security-reviewer` on Tasks 3+5 (a
config key that names an executable, and a spawned child) and `encoding-auditor`
on Tasks 4+6 (names as bytes end to end).

- [ ] **Step 6: Apply the findings, then commit**

```bash
git diff --cached --stat      # nunca un commit vacío (CLAUDE.md)
git add crates/norte-proto crates/norte-core
git commit -m "feat(proto,core): rar is a format, and only over a local file"
```

- [ ] **Step 7: One `just ci-fast`.** This is the plan's single budgeted run
before the close. Expected: green.

---

### Task 8: Documentation, the ADR, and the close

**Files:**
- Create: `docs/adr/0056-a-provider-that-delegates-to-an-external-program.md`
  (use the `adr` skill so the numbering and MADR shape are right)
- Modify: `CHANGELOG.md`, `ARCHITECTURE.md` (the crate map), `docs/spec/norte-spec.md`
  if product decision 5 needs its status updating

- [ ] **Step 1: Write the ADR.** It records: the rule-9 boundary (a path, an
entry name, a pipe, empty cwd, closed stdin, no shell); why the WASM sandbox
cannot host this at all (`exec` is permanently `none`, `capability.rs:3`); why
"local inner only" is enforced in the engine rather than inside the crate; the
**measured** delegate table and why `7z` outranks `unrar`; the skipped-and-counted
rule extended to names a line-oriented listing cannot carry; and the
user-layer-only config key.

- [ ] **Step 2: Changelog entry**, in the voice the file already uses.

- [ ] **Step 3: Note the gap honestly.** Open an issue: these fixtures are RAR5,
where names are UTF-8 by format; a RAR4 archive with an OEM-code-page name is
what a decade of downloads actually contains, and nothing in this tree can
produce one to test against.

- [ ] **Step 4: One `just ci`.** Foreground, never through `| tail`. Expected:
green.

- [ ] **Step 5: Commit and finish the branch** with the
`superpowers:finishing-a-development-branch` skill.

```bash
git add docs CHANGELOG.md ARCHITECTURE.md
git commit -m "docs(adr,changelog): a provider may delegate to a program it does not trust"
```
