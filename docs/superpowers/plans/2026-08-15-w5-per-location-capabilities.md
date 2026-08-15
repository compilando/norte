# W5 — a provider answers about a LOCATION: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: use `superpowers:subagent-driven-development`
> or `superpowers:executing-plans` to implement this plan task by task. Steps
> use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `Provider` stops answering only about itself — it answers about a
location (`capabilities_at`) and it can operate confined beneath a caller's
root (`open_root`). Closes #153, #145 and #164.

**Architecture:** Spec is
`docs/superpowers/specs/2026-08-15-per-location-provider-capabilities-design.md`
— read it first; it carries the reasoning this plan does not repeat. Phase A
adds an async, per-path refinement of the SAME `Capabilities` type, with a
trait default that answers what the provider already declares, plus a
read-only-first probe in `norte-vfs-local` and a `FoldMode` where `Sides` and
`NameCaps` currently hold a bool. Phase B adds a `ConfinedRoot` handle: the
caller opens the root once and addresses relative segments, so no path is ever
recomposed.

**Tech stack:** Rust 2024, `async_trait`, `bitflags` (hand-written serde, ADR
0004), `libc` (`statfs`, `ioctl`, `openat`, `openat2`), `tokio` +
`spawn_blocking` (hard rule 2), `nextest` via `just`.

**Order:** phase A lands and gates first. If phase B stalls in the platform
matrix, two issues are already done. Both phases share ONE protocol bump
(0.45.0) taken in task A2.

---

## File map

| file | what it becomes responsible for |
| --- | --- |
| `docs/adr/0054-*.md` | the doctrine: three classes of question a provider answers |
| `crates/norte-proto/src/caps.rs` | `FULL_FOLD`, `CONFINED_WRITES` |
| `crates/norte-proto/src/error.rs` | `ConflictKind::EscapesRoot` |
| `crates/norte-proto/src/methods.rs:586` | `PROTOCOL_VERSION = "0.45.0"` |
| `crates/norte-vfs/src/provider.rs` | `capabilities_at` (A) and `open_root` + `ConfinedRoot` (B) |
| `crates/norte-vfs/src/contract.rs` | conformance cases for both |
| `crates/norte-vfs-local/src/caps_at.rs` | **new**: the read-only probe ladder and its `(dev, ino)` cache |
| `crates/norte-vfs-local/src/confined.rs` | **new**: `openat2`/component-walk root handle and its sink |
| `crates/norte-testkit/src/mem.rs` | `MemProvider` answers a scripted `capabilities_at` |
| `crates/norte-compare/src/key.rs` | `Sides` carries a `FoldMode` per side |
| `crates/norte-core/src/rename/plan.rs` | `NameCaps` carries a `FoldMode` |
| `crates/norte-core/src/{compare.rs,engine.rs,sync/spool.rs}` | call `capabilities_at` |
| `crates/norte-core/src/ops.rs` | use a confined root when the destination has one |
| `crates/norte-frontend/src/confine.rs` | **new**: the sentence shown before an unconfined write |

Two new files in `norte-vfs-local` rather than more of `provider.rs` (1600
lines already): the probe ladder and the confined handle are each
self-contained, each platform-conditional, and each wants its own test module.

---

# PHASE A — the question about a location

### Task A1: the ADR

**Files:**
- Create: `docs/adr/0054-a-provider-answers-about-a-location.md`

- [ ] **Step 1: write the ADR**

Use the `/adr` project command so the number and MADR template come out right.
Content, taken from spec §1 (do not re-derive it):

- Context: `capabilities()` takes no path and is sync; `write`/`mkdir` take no
  root; #153, #145 and #164 are the same fact from three directions.
- Decision: three classes of question — about the backend (`capabilities()`),
  about a location (`capabilities_at`), operate confined (`open_root`).
  `capabilities()` stays the declared default; `capabilities_at`'s trait
  default returns it, so no provider breaks.
- Consequences: `Capabilities` cannot say "I do not know" — the degradation is
  today's behaviour, stated as a known limit. Windows confinement is out of
  scope and gets its own issue. No tamper-evident record of an unconfined
  write: it would need chain format 2 (`journal.rs:443`), which is a wave of
  its own.

- [ ] **Step 2: commit**

```bash
git add docs/adr/0054-a-provider-answers-about-a-location.md
git commit -m "docs(adr): 0054, a provider answers about a location"
```

---

### Task A2: the wire, all of it, once

Both phases' wire changes land in ONE commit and ONE bump, so
`protocol-guardian` reviews the surface once.

**Files:**
- Modify: `crates/norte-proto/src/caps.rs` (bitflags block, ~line 20)
- Modify: `crates/norte-proto/src/error.rs:23` (`ConflictKind`)
- Modify: `crates/norte-proto/src/methods.rs:586` (`PROTOCOL_VERSION`)
- Test: `crates/norte-proto/src/caps.rs` (test module), `crates/norte-proto/tests/schema.rs`

- [ ] **Step 1: write the failing tests**

In `crates/norte-proto/src/caps.rs`'s test module:

```rust
#[test]
fn full_fold_round_trips_on_the_wire() {
    let c = CapabilityFlags::CASE_PRESERVING | CapabilityFlags::FULL_FOLD;
    let json = serde_json::to_string(&c).expect("serializa");
    assert_eq!(json, "\"CASE_PRESERVING | FULL_FOLD\"");
    let back: CapabilityFlags = serde_json::from_str(&json).expect("deserializa");
    assert_eq!(back, c);
}

#[test]
fn confined_writes_round_trips_on_the_wire() {
    let c = CapabilityFlags::CONFINED_WRITES;
    let json = serde_json::to_string(&c).expect("serializa");
    assert_eq!(json, "\"CONFINED_WRITES\"");
    assert_eq!(
        serde_json::from_str::<CapabilityFlags>(&json).expect("deserializa"),
        c
    );
}
```

In `crates/norte-proto/src/error.rs`'s test module:

```rust
#[test]
fn escapes_root_round_trips_and_an_older_client_reads_unknown() {
    let json = serde_json::to_string(&ConflictKind::EscapesRoot).expect("serializa");
    assert_eq!(json, "\"escapes_root\"");
    // La política de N-1 (`#[serde(other)]`) sigue viva para un token futuro.
    let future: ConflictKind =
        serde_json::from_str("\"something_from_0_99\"").expect("token futuro");
    assert_eq!(future, ConflictKind::Unknown);
}
```

(Check the existing `ConflictKind` serde renaming convention before asserting
the literal — match whatever `CaseCollision` serialises to.)

- [ ] **Step 2: run them and watch them fail**

```
just t norte-proto
```
Expected: FAIL, `no variant or associated item named FULL_FOLD` /
`EscapesRoot`.

- [ ] **Step 3: add the three wire items**

- `caps.rs`: `const FULL_FOLD = 1 << 9;` and `const CONFINED_WRITES = 1 << 10;`
  with rustdoc. `FULL_FOLD`: "this directory's folding EXPANDS (ext4/f2fs `+F`,
  kernel table built from `CaseFolding.txt` status `C + F`); only meaningful
  with `CASE_SENSITIVE` absent". `CONFINED_WRITES`: "writes under this location
  can be confined beneath a caller root with a kernel guarantee; answered by
  `capabilities_at`, never by `capabilities()` — it depends on the mount, the
  platform and the running kernel".
- `error.rs`: `EscapesRoot` variant, placed BEFORE `Unknown` (the
  `#[serde(other)]` arm must stay last), with the `Display` arm "path escapes
  its root" and rustdoc saying why it is not `NotFound`: a caller that sees
  `NotFound` retries by creating the parent, which is what must not happen.
- `methods.rs:586`: `PROTOCOL_VERSION = "0.45.0"`.

- [ ] **Step 4: run the tests**

```
just t norte-proto
```
Expected: PASS, and the schema test FAILS — it compares against the committed
JSON Schema, which no longer matches.

- [ ] **Step 5: regenerate the schema and the goldens**

```
NORTE_UPDATE_SCHEMA=1 cargo nextest run -p norte-proto
just t norte-proto
```
Expected: PASS. Inspect the schema diff by eye: only the new flag names in the
`CapabilityFlags` description and the new `ConflictKind` enum value. Anything
else in that diff is a bug in this task.

- [ ] **Step 6: commit**

```bash
git add -A crates/norte-proto
git commit -m "feat(proto)!: capabilities can be answered per location (0.45.0)"
```

---

### Task A3: `capabilities_at` on the trait

**Files:**
- Modify: `crates/norte-vfs/src/provider.rs` (after `capabilities()`, ~line 97)
- Modify: `crates/norte-core/src/sessions.rs:527` (delegation)
- Modify: `crates/norte-testkit/src/mem.rs:645`
- Modify: `crates/norte-vfs/src/contract.rs`

- [ ] **Step 1: write the failing tests**

In `crates/norte-testkit/src/mem.rs`'s test module — the default answers the
declaration, a scripted override answers per path:

```rust
#[tokio::test]
async fn capabilities_at_defaults_to_the_declaration() {
    let p = MemProvider::new();
    let root = VPath::parse("mem:///").expect("raíz");
    assert_eq!(
        p.capabilities_at(&root).await.expect("responde"),
        p.capabilities()
    );
}

#[tokio::test]
async fn a_scripted_location_overrides_the_declaration() {
    let p = MemProvider::new();
    let usb = VPath::parse("mem:///usb").expect("ruta");
    p.mkdir(&usb).await.expect("dir");
    p.set_caps_at(
        &usb,
        Capabilities {
            flags: CapabilityFlags::CASE_PRESERVING | CapabilityFlags::FULL_FOLD,
            max_path: None,
        },
    );
    let at = p.capabilities_at(&usb).await.expect("responde");
    assert!(at.flags.contains(CapabilityFlags::FULL_FOLD));
    assert!(!p.capabilities().flags.contains(CapabilityFlags::FULL_FOLD));
}
```

In `crates/norte-core/src/sessions.rs`'s test module, beside the existing
delegation assertions (see the sftp double at line 667):

```rust
#[tokio::test]
async fn session_provider_delegates_capabilities_at() {
    // El doble responde FULL_FOLD SOLO en `capabilities_at`: si
    // `SessionProvider` sirviera el default del trait, esto sería falso.
    let (session, path) = doble_con_caps_at_full_fold();
    let at = session.capabilities_at(&path).await.expect("responde");
    assert!(at.flags.contains(norte_proto::CapabilityFlags::FULL_FOLD));
}
```

- [ ] **Step 2: run and watch them fail**

```
just t norte-testkit
```
Expected: FAIL, `no method named capabilities_at`.

- [ ] **Step 3: add the trait method**

In `crates/norte-vfs/src/provider.rs`, immediately after `capabilities()`:

```rust
/// Capabilities REFINED for `p`: the same declaration, corrected by whatever
/// the backend can find out about THAT location — a mount's case folding, an
/// ext4/f2fs `+F` directory, whether a write beneath it can be confined.
///
/// The default answers [`Self::capabilities`], which is correct for any
/// backend whose locations are alike; overriding it is for backends that
/// serve more than one filesystem behind one scheme.
///
/// Errors: whatever `p` itself produces ([`Error::NotFound`] for a path that
/// is not there). A probe that cannot answer is NOT an error — the
/// declaration is returned instead. `Capabilities` cannot express "unknown"
/// (ADR 0054), so an absent flag means absent.
async fn capabilities_at(&self, p: &VPath) -> Result<Capabilities, Error> {
    let _ = p;
    Ok(self.capabilities())
}
```

Then: delegate it in `SessionProvider` (`self.inner.capabilities_at(p).await` —
wrap in `self.observe(...)` exactly like the other async delegations, so a dead
session is still noticed), and add `MemProvider::set_caps_at(&self, p: &VPath,
caps: Capabilities)` backed by a `Mutex<BTreeMap<VPath, Capabilities>>` with a
`capabilities_at` override that looks the path up and falls back to `self.caps`.

- [ ] **Step 4: run the tests**

```
just t norte-testkit && just t norte-core
```
Expected: PASS.

- [ ] **Step 5: add the contract case**

In `crates/norte-vfs/src/contract.rs`, inside `provider_contract!`:

```rust
#[tokio::test]
async fn contract_capabilities_at_is_at_least_the_declaration() {
    let p = $factory;
    let root: VPath = $root;
    let at = p.capabilities_at(&root).await.expect("capabilities_at responde");
    // No se exige igualdad: un provider PUEDE refinar. Se exige que refinar no
    // sea contradecirse en lo que no depende de la ubicación.
    assert_eq!(at.flags.contains(CapabilityFlags::READ_ONLY),
               p.capabilities().flags.contains(CapabilityFlags::READ_ONLY),
               "READ_ONLY es del backend, no de la ubicación");
    assert!(!(at.flags.contains(CapabilityFlags::FULL_FOLD)
              && at.flags.contains(CapabilityFlags::CASE_SENSITIVE)),
            "FULL_FOLD solo tiene sentido sin CASE_SENSITIVE");
}
```

- [ ] **Step 6: run the whole provider set**

```
just t norte-vfs-local && just t norte-vfs-sftp && just t norte-vfs-object && just t norte-vfs-archive
```
Expected: PASS everywhere (all four still use the default).

- [ ] **Step 7: commit**

```bash
git add -A crates/norte-vfs crates/norte-core/src/sessions.rs crates/norte-testkit
git commit -m "feat(vfs): a provider can answer capabilities for a location"
```

---

### Task A4: the probe ladder in `norte-vfs-local`

**Files:**
- Create: `crates/norte-vfs-local/src/caps_at.rs`
- Modify: `crates/norte-vfs-local/src/lib.rs` (declare the module)
- Modify: `crates/norte-vfs-local/src/provider.rs` (impl `capabilities_at`, add the cache field)
- Test: `crates/norte-vfs-local/tests/local.rs`

- [ ] **Step 1: write the failing tests**

```rust
#[tokio::test]
async fn capabilities_at_answers_for_a_subdirectory_without_writing_to_it() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sub = tmp.path().join("sub");
    std::fs::create_dir(&sub).expect("mkdir");
    let p = LocalProvider::rooted(tmp.path());
    let vsub = vpath_de(&sub);

    let before: Vec<_> = std::fs::read_dir(&sub).expect("listar").collect();
    let _ = p.capabilities_at(&vsub).await.expect("responde");
    let after: Vec<_> = std::fs::read_dir(&sub).expect("listar").collect();

    assert_eq!(before.len(), after.len(), "la sonda de lectura no deja rastro");
    assert_eq!(after.len(), 0, "y no crea nada en un dir vacío");
}

#[tokio::test]
async fn a_read_only_directory_still_gets_an_answer() {
    // Un mount de solo lectura no se puede fabricar en CI; lo que sí se puede
    // es un directorio sin permiso de escritura, que es lo que rompía a la
    // sonda de escritura.
    let tmp = tempfile::tempdir().expect("tmp");
    let ro = tmp.path().join("ro");
    std::fs::create_dir(&ro).expect("mkdir");
    let mut perms = std::fs::metadata(&ro).expect("md").permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&ro, perms).expect("chmod");

    let p = LocalProvider::rooted(tmp.path());
    let caps = p.capabilities_at(&vpath_de(&ro)).await.expect("responde");
    // El FS de CI es case-sensitive en Linux; lo que se afirma es que
    // RESPONDE, no cuál es la respuesta (el CI de macOS diría lo contrario).
    assert_eq!(
        caps.flags.contains(CapabilityFlags::CASE_SENSITIVE),
        p.capabilities().flags.contains(CapabilityFlags::CASE_SENSITIVE)
    );
}

#[tokio::test]
async fn two_paths_to_one_directory_probe_once() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sub = tmp.path().join("sub");
    std::fs::create_dir(&sub).expect("mkdir");
    let p = LocalProvider::rooted(tmp.path());

    let directo = vpath_de(&sub);
    let rodeo = vpath_de(&tmp.path().join("sub/../sub"));
    let _ = p.capabilities_at(&directo).await.expect("responde");
    let _ = p.capabilities_at(&rodeo).await.expect("responde");

    assert_eq!(p.caps_at_probe_count(), 1, "la clave es (dev, ino), no la ruta");
}
```

`caps_at_probe_count()` is a `#[doc(hidden)]` test seam on `LocalProvider` (an
`AtomicU64` bumped inside the probe), in the style of the existing
`with_trash_home` seam — `PROBE_SEQ` shows the precedent for the counter.

- [ ] **Step 2: run and watch them fail**

```
just t norte-vfs-local
```
Expected: FAIL, `no method named capabilities_at` on `LocalProvider`'s inherent
path plus `caps_at_probe_count` missing.

- [ ] **Step 3: write `caps_at.rs`**

Public surface of the module (everything else is private):

```rust
/// What a probe found out about ONE directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocationCaps {
    pub(crate) case_sensitive: Option<bool>,
    pub(crate) full_fold: bool,
}

/// Probes `dir` WITHOUT mutating it where the platform allows, and falls back
/// to the write probe only when nothing else answered and `dir` is writable.
/// Blocking: call it from `spawn_blocking` (hard rule 2).
pub(crate) fn probe_location(dir: &Path) -> LocationCaps;
```

The ladder, in order, each rung `cfg`-gated:

1. Linux — `libc::statfs` on `dir`: `f_type` identifies the filesystem
   (`EXT4_SUPER_MAGIC 0xEF53`, `F2FS 0xF2F52010`, `MSDOS/vfat 0x4d44`,
   `EXFAT 0x2011BAB0`, `NTFS 0x5346544e`, `SMB2 0xFE534D42`, `CIFS 0xFF534D42`).
   Then `ioctl(fd, FS_IOC_GETFLAGS, &flags)` on an `O_RDONLY|O_DIRECTORY` fd for
   `dir`: `FS_CASEFOLD_FL` (`0x4000_0000`) set means this DIRECTORY is `+F` —
   `case_sensitive: Some(false)`, `full_fold: true`. `ENOTTY`/`EINVAL` from the
   ioctl is a normal answer for a filesystem without the flag, not an error.
   Unknown `f_type` and no ioctl answer → `case_sensitive: None`, fall through.
2. macOS — `case_sensitivity_from_os` already exists in `provider.rs:534`
   (`pathconf(_PC_CASE_SENSITIVE)`); move it into this module unchanged and
   call it for `dir` rather than for `base`. `full_fold: false` — no Apple
   filesystem expands.
3. Windows — `GetVolumeInformationW` for the volume holding `dir`;
   `FILE_CASE_SENSITIVE_SEARCH` in the flags. `full_fold: false`.
4. Last resort, all platforms — today's `probe_case_sensitivity(dir)` (write
   probe), and ONLY if the previous rungs answered `None`. It already returns
   `None` when the directory is not writable, which is now a real answer path
   rather than a dead end.

Each `unsafe` call gets its own `// SAFETY:` naming the invariant (hard rule 5:
this crate is the only one allowed, and the ADR's rule is a comment plus a
test).

- [ ] **Step 4: wire it into `LocalProvider`**

Add the cache field:

```rust
/// Probed capabilities per DIRECTORY, keyed by `(dev, ino)` so two paths to
/// one directory are one entry. Bounded: 256 entries, oldest evicted — a
/// comparison touches a handful of roots and a long session must not grow a
/// map for every directory it ever visited.
caps_at: Arc<Mutex<LruCache<(u64, u64), Capabilities>>>,
```

Use whatever small LRU the workspace already depends on; if there is none,
a `BTreeMap` plus an insertion-ordered `VecDeque` of keys is enough and needs
no new dependency (hard rule 8 wants a justification for one, and this does not
earn it).

`capabilities_at(&self, p)`: resolve the native path, then `spawn_blocking` a
closure that `stat`s it for `(dev, ino)` (on a FILE, probe its PARENT — the
question is always about the containing directory), looks the key up, probes on
a miss, and folds `LocationCaps` into a copy of `self.capabilities()`:
`CASE_SENSITIVE` set from `case_sensitive` when it answered, `FULL_FOLD` set
from `full_fold`. `CASE_PRESERVING` is untouched — it is the backend's answer.

- [ ] **Step 5: run the tests**

```
just t norte-vfs-local
```
Expected: PASS.

- [ ] **Step 6: check the doc surface the loop does not check**

```
cargo test -p norte-vfs-local --doc && cargo doc -p norte-vfs-local --no-deps
```
Expected: PASS both. (`just t` runs nextest, which skips doctests; `just c` does
not check intra-doc links — CLAUDE.md's three blind spots.)

- [ ] **Step 7: commit**

```bash
git add -A crates/norte-vfs-local
git commit -m "feat(vfs-local): capabilities are probed per directory, read-only first"
```

---

### Task A5: `Sides` carries a `FoldMode` per side

**Files:**
- Modify: `crates/norte-compare/src/key.rs:43-157`
- Test: same file's test module, plus `crates/norte-compare/tests/`

- [ ] **Step 1: write the failing tests**

```rust
#[test]
fn a_side_that_expands_makes_the_pair_expand() {
    let ext4_f = Capabilities {
        flags: CapabilityFlags::CASE_PRESERVING | CapabilityFlags::FULL_FOLD,
        max_path: None,
    };
    let ext4 = Capabilities { flags: CapabilityFlags::CASE_SENSITIVE, max_path: None };
    let sides = Sides::from_capabilities(ext4, ext4_f);
    assert_eq!(
        key_for("straße.txt".as_bytes(), sides).as_bytes(),
        key_for("strasse.txt".as_bytes(), sides).as_bytes(),
        "en un +F son UN nombre"
    );
}

#[test]
fn without_full_fold_they_are_still_two_names() {
    let apfs = Capabilities { flags: CapabilityFlags::CASE_PRESERVING, max_path: None };
    let ext4 = Capabilities { flags: CapabilityFlags::CASE_SENSITIVE, max_path: None };
    let sides = Sides::from_capabilities(ext4, apfs);
    assert_ne!(
        key_for("straße.txt".as_bytes(), sides).as_bytes(),
        key_for("strasse.txt".as_bytes(), sides).as_bytes(),
        "plegado SIMPLE no expande"
    );
}

#[test]
fn the_corpus_pair_pins_both_verdicts() {
    // `ext4_full_fold_es_zett` / `ext4_full_fold_ss` los dejó #145 puestos.
    let (a, b) = norte_testkit::corpus::pair("ext4_full_fold_es_zett", "ext4_full_fold_ss");
    let full = Sides::new(FoldMode::Full, FoldMode::None);
    let simple = Sides::new(FoldMode::Simple, FoldMode::None);
    assert_eq!(key_for(&a, full).as_bytes(), key_for(&b, full).as_bytes());
    assert_ne!(key_for(&a, simple).as_bytes(), key_for(&b, simple).as_bytes());
}
```

Check `norte-testkit`'s corpus accessor name before writing that third test —
use whatever the corpus module already exposes rather than inventing `pair`.

- [ ] **Step 2: run and watch them fail**

```
just t norte-compare
```
Expected: FAIL — `Sides::new` takes two bools today.

- [ ] **Step 3: change `Sides`**

```rust
pub struct Sides {
    left: FoldMode,
    right: FoldMode,
}

impl Sides {
    /// From each side's fold mode.
    pub fn new(left: FoldMode, right: FoldMode) -> Self { Self { left, right } }

    /// From the [`Capabilities`] of each side, as answered for the two ROOTS
    /// being compared (`Provider::capabilities_at`, not `capabilities()`).
    pub fn from_capabilities(left: Capabilities, right: Capabilities) -> Self;

    /// How the PAIR folds. Case folding is a property of the pair, not of a
    /// side: one side that cannot hold both spellings decides for both. And a
    /// side that EXPANDS decides likewise — `straße.txt` and `strasse.txt` are
    /// one name there, so a comparison that answers two would report no
    /// collision for a pair that collides.
    pub fn fold(self) -> FoldMode;
}
```

`fold()` returns `Full` if either side is `Full`, else `Simple` if either is
`Simple`, else `None`. Per-`Capabilities` mapping: `FULL_FOLD` → `Full`; no
`CASE_SENSITIVE` → `Simple`; otherwise `None`.

Keep `folds_case()` as `!matches!(self.fold(), FoldMode::None)` — the compare
engine reads it in several places and its meaning has not changed. Update
`both_case_sensitive()` / `left_case_insensitive()` / `right_case_insensitive()`
to build the new field, and `key_for` to use `sides.fold()` instead of its
`if`.

- [ ] **Step 4: run the tests**

```
just t norte-compare && cargo test -p norte-compare --doc
```
Expected: PASS — the doctests in `key.rs` construct `Sides` and will need
updating with the new constructor.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-compare
git commit -m "feat(compare): a side that expands makes the pair expand"
```

---

### Task A6: `NameCaps` carries a `FoldMode`

**Files:**
- Modify: `crates/norte-core/src/rename/plan.rs:27-31,203`
- Modify: `crates/norte-core/src/rename/{naming.rs,exec.rs,mod.rs}`, `crates/norte-core/src/undo.rs:711`
- Test: `crates/norte-core/src/rename/plan.rs` test module

- [ ] **Step 1: write the failing test**

```rust
#[test]
fn a_full_fold_directory_sees_the_collision_simple_folding_misses() {
    let caps = NameCaps { fold: FoldMode::Full };
    let listing = vec!["strasse.txt".as_bytes().to_vec()];
    let pairs = vec![(
        "otro.txt".as_bytes().to_vec(),
        "straße.txt".as_bytes().to_vec(),
    )];
    let plan = plan_batch(&pairs, &listing, caps);
    assert!(
        !plan.collisions.is_empty(),
        "en +F el destino YA existe con otra grafía"
    );
    assert_eq!(plan.collisions[0].kind, CollisionKind::Existing);
}
```

Check `CollisionKind`'s exact variant for "the destination already exists in
the directory" before asserting it — the enum is closed and mirrors
`RenameCollisionKind`.

- [ ] **Step 2: run and watch it fail**

```
just t norte-core
```
Expected: FAIL — `NameCaps` has no field `fold`.

- [ ] **Step 3: change `NameCaps`**

```rust
pub struct NameCaps {
    /// How this directory folds names. `FoldMode::None` = `Foo` and `foo` are
    /// two names here.
    pub fold: FoldMode,
}
```

with a constructor `NameCaps::from_capabilities(c: Capabilities) -> Self` using
the same mapping as `Sides` (`FULL_FOLD` → `Full`, no `CASE_SENSITIVE` →
`Simple`, else `None`), so the two places that read `Capabilities` agree by
construction rather than by memory. `name_key(name, caps)` passes
`caps.fold` straight to `norte_encoding::name_key`.

Every `NameCaps { case_sensitive: x }` in the crate becomes
`NameCaps { fold: if x { FoldMode::None } else { FoldMode::Simple } }` — note
the inversion, and note that the test constants (`SENSITIVE` at
`plan.rs:592`, `undo.rs:1438`) are the easiest place to get it backwards.

- [ ] **Step 4: run the tests**

```
just t norte-core && cargo test -p norte-core --doc
```
Expected: PASS. The `plan_batch` and `name_key` doctests construct `NameCaps`
and need the new field.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-core
git commit -m "feat(core): the rename planner folds the way the directory folds"
```

---

### Task A7: the callers ask about the location

**Files:**
- Modify: `crates/norte-core/src/compare.rs:121`
- Modify: `crates/norte-core/src/engine.rs:1272,2152`
- Modify: `crates/norte-core/src/sync/spool.rs:2548`
- Test: `crates/norte-core/tests/` (integration, with `MemProvider`)

- [ ] **Step 1: write the failing test**

```rust
#[tokio::test]
async fn a_compare_folds_the_way_the_destination_root_folds() {
    // Izquierda case-sensitive, derecha declarada sensible PERO con una raíz
    // que responde +F: si `fs.compare` preguntase a `capabilities()`, esta
    // pareja saldría como dos huérfanas en vez de como una pareja.
    let (core, left, right) = dos_raices_mem().await;
    core.provider_for(&right).set_caps_at(
        &right,
        Capabilities {
            flags: CapabilityFlags::CASE_PRESERVING | CapabilityFlags::FULL_FOLD,
            max_path: None,
        },
    );
    escribe(&left, "straße.txt", b"x").await;
    escribe(&right, "strasse.txt", b"x").await;

    let rows = compara(&core, &left, &right).await;
    assert_eq!(rows.len(), 1, "una pareja, no dos huérfanas");
}
```

Reuse whatever helpers `crates/norte-core/tests/` already has for building a
core over `MemProvider` roots; do not invent a second harness.

- [ ] **Step 2: run and watch it fail**

```
just t norte-core
```
Expected: FAIL — two rows.

- [ ] **Step 3: swap the four call sites**

Each becomes `provider.capabilities_at(&root).await?` with the root that call
site already has in hand:

- `compare.rs:121` — the two compare roots.
- `engine.rs:1272` — `source` and the destination whose `caps` it already
  computed.
- `sync/spool.rs:2548` — origin and destination roots.
- `engine.rs:2152` — the rename batch's DIRECTORY (the destination directory,
  which is what `NameCaps`'s rustdoc already promises it is).

All four are already in `async fn`s. Where a call site currently holds
`Capabilities` from an earlier step, take the per-location answer at the same
point the old one was taken — moving the question earlier would ask it before
the root is validated.

- [ ] **Step 4: run the tests**

```
just t norte-core && just t norte-compare
```
Expected: PASS.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-core
git commit -m "fix(core): compare and sync ask the ROOT, not the provider"
```

---

### Task A8: close phase A

- [ ] **Step 1: changelog**

Add the entry to `CHANGELOG.md` under unreleased: per-location capabilities,
`FULL_FOLD`, protocol 0.45.0, closes #153 and #145.

- [ ] **Step 2: dispatch the reviewers**

Before committing, dispatch in parallel and apply BLOCKER/MAJOR findings in ONE
pass: `protocol-guardian` (the wire grew — task A2's commit range),
`encoding-auditor` (folding and filenames — A5, A6), `rust-reviewer` (the whole
phase). Give each the commit range, what the change is for, and the specific
question you are unsure about. Reviewers do not compile.

- [ ] **Step 3: the one gate run for the phase**

```
just ci-fast
```
Expected: PASS. If it fails, reproduce the single failure with `just t <crate>`
and fix it there — never re-run the gate as a debugger.

- [ ] **Step 4: commit and close the issues**

```bash
git add -A
git commit -m "docs(changelog): per-location capabilities"
gh issue close 153 --comment "..."
gh issue close 145 --comment "..."
```

Each close comment states what shipped and what did NOT: `+F` detection is
Linux-only (the ioctl), and no CI machine has a `+F` volume, so the verdict is
pinned by the corpus pair and by `MemProvider`, not by a real filesystem.

---

# PHASE B — confining the write

### Task B1: `ConfinedRoot` on the trait

**Files:**
- Modify: `crates/norte-vfs/src/provider.rs`
- Modify: `crates/norte-vfs/src/contract.rs`
- Modify: `crates/norte-core/src/sessions.rs` (delegation)

- [ ] **Step 1: write the failing contract case**

In `provider_contract!`:

```rust
#[tokio::test]
async fn contract_open_root_is_unsupported_or_confines() {
    let p = $factory;
    let root: VPath = $root;
    match p.open_root(&root).await {
        Err(Error::Unsupported) => {} // backend sin openat: respuesta honesta
        Ok(_) => assert!(
            p.capabilities_at(&root).await.expect("caps").flags
                .contains(CapabilityFlags::CONFINED_WRITES),
            "quien abre raíz confinada lo DECLARA"
        ),
        Err(e) => panic!("open_root respondió {e:?}"),
    }
}
```

- [ ] **Step 2: run and watch it fail**

```
just t norte-vfs-local
```
Expected: FAIL, `no method named open_root`.

- [ ] **Step 3: add the trait surface**

```rust
/// Opens `root` as a CONFINED root: every operation on the returned handle
/// addresses segments RELATIVE to it and cannot escape, however the tree is
/// shaped underneath — an intermediate component that is a symlink out of the
/// root fails rather than redirecting the write (#164).
///
/// This is not a check before an open: there is no path to recompose, which
/// is what makes it free of the TOCTOU window a caller-side check has by
/// construction.
///
/// `Unsupported` (default) = this backend has no way to confine. The caller
/// degrades — it does not refuse — and says so; see `CONFINED_WRITES`.
async fn open_root(&self, root: &VPath) -> Result<Box<dyn ConfinedRoot>, Error> {
    let _ = root;
    Err(Error::Unsupported)
}
```

and, in the same module:

```rust
/// Operations beneath a root that cannot leave it. Obtained from
/// [`Provider::open_root`]; `rel` is always relative to that root, and an
/// empty `rel` is the root itself.
///
/// A `rel` that would escape answers [`Error::Conflict`] with
/// [`ConflictKind::EscapesRoot`] — never `NotFound`, which a caller answers by
/// creating the parent, i.e. by doing the thing this trait exists to prevent.
#[async_trait]
pub trait ConfinedRoot: Send + Sync {
    /// Creates a directory at `rel`. Same contract as [`Provider::mkdir`].
    async fn mkdir(&self, rel: &[Segment]) -> Result<(), Error>;

    /// Opens a sink for `rel`. Same contract as [`Provider::write`], including
    /// the staging-and-publish: the publish is confined too.
    async fn write(&self, rel: &[Segment]) -> Result<Box<dyn ByteSink>, Error>;

    /// Same contract as [`Provider::open_resumable`]. Default: no resume.
    async fn open_resumable(&self, rel: &[Segment]) -> Result<(Box<dyn ByteSink>, u64), Error> {
        Ok((self.write(rel).await?, 0))
    }

    /// Same contract as [`Provider::stat`]: describes the LINK, never its
    /// target.
    async fn stat(&self, rel: &[Segment]) -> Result<Entry, Error>;
}
```

Delegate `open_root` in `SessionProvider`. Re-export `ConfinedRoot` from
`norte-vfs`'s `lib.rs`.

- [ ] **Step 4: run the contract across every provider**

```
just t norte-vfs-local && just t norte-vfs-sftp && just t norte-vfs-object && just t norte-vfs-archive && just t norte-testkit
```
Expected: PASS — everyone takes the default and the case's first arm.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-vfs crates/norte-core/src/sessions.rs
git commit -m "feat(vfs): a provider can hand out a confined root"
```

---

### Task B2: the confined root in `norte-vfs-local`

**Files:**
- Create: `crates/norte-vfs-local/src/confined.rs`
- Modify: `crates/norte-vfs-local/src/lib.rs`, `provider.rs` (impl `open_root`)
- Test: `crates/norte-vfs-local/tests/confined.rs` (new)

- [ ] **Step 1: write the failing tests — this is the test that would have caught #164**

```rust
#[tokio::test]
async fn an_intermediate_symlink_cannot_redirect_a_write_out_of_the_root() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dentro = tmp.path().join("dest");
    let fuera = tmp.path().join("fuera");
    std::fs::create_dir(&dentro).expect("dest");
    std::fs::create_dir(&fuera).expect("fuera");
    // El componente INTERMEDIO es el ataque: dest/sub -> ../fuera
    std::os::unix::fs::symlink(&fuera, dentro.join("sub")).expect("symlink");

    let p = LocalProvider::rooted(tmp.path());
    let root = p.open_root(&vpath_de(&dentro)).await.expect("raíz confinada");
    let err = root
        .write(&[seg(b"sub"), seg(b"botin.txt")])
        .await
        .expect_err("tiene que negarse");

    assert!(
        matches!(err, Error::Conflict { kind: ConflictKind::EscapesRoot, .. }),
        "respondió {err:?}"
    );
    assert!(
        !fuera.join("botin.txt").exists(),
        "y sobre todo: no escribió fuera"
    );
}

#[tokio::test]
async fn a_symlink_INSIDE_the_root_is_not_an_escape() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dentro = tmp.path().join("dest");
    std::fs::create_dir_all(dentro.join("real")).expect("real");
    std::os::unix::fs::symlink(dentro.join("real"), dentro.join("sub")).expect("symlink");

    let p = LocalProvider::rooted(tmp.path());
    let root = p.open_root(&vpath_de(&dentro)).await.expect("raíz confinada");
    // RESOLVE_BENEATH rechaza TODO symlink, también el que no se sale. Es más
    // estricto que "no salir" y es la semántica que se documenta: un destino
    // por symlink es un caso raro, y negarlo no pierde datos.
    let err = root.write(&[seg(b"sub"), seg(b"x.txt")]).await.expect_err("estricto");
    assert!(matches!(err, Error::Conflict { kind: ConflictKind::EscapesRoot, .. }));
}

#[tokio::test]
async fn a_plain_nested_write_still_works() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dentro = tmp.path().join("dest");
    std::fs::create_dir(&dentro).expect("dest");

    let p = LocalProvider::rooted(tmp.path());
    let root = p.open_root(&vpath_de(&dentro)).await.expect("raíz confinada");
    root.mkdir(&[seg(b"sub")]).await.expect("mkdir");
    let mut sink = root.write(&[seg(b"sub"), seg(b"ok.txt")]).await.expect("write");
    sink.write(Bytes::from_static(b"hola")).await.expect("chunk");
    sink.commit().await.expect("commit");

    assert_eq!(std::fs::read(dentro.join("sub/ok.txt")).expect("leer"), b"hola");
}

#[tokio::test]
async fn the_component_walk_gives_the_same_verdicts() {
    // Fuerza el camino sin `openat2` (kernel <5.6 o seccomp): la costura de
    // test desactiva el syscall y el resto del caso es idéntico al primero.
    let _forzado = norte_vfs_local::force_component_walk_for_test();
    an_intermediate_symlink_cannot_redirect_a_write_out_of_the_root().await;
}
```

The second test is a design decision worth stating in the code: `RESOLVE_BENEATH`
refuses ALL symlinks, not only escaping ones, and the component walk with
`O_NOFOLLOW` matches that. Document it on `ConfinedRoot` in task B1 if it is
not already there.

- [ ] **Step 2: run and watch them fail**

```
just t norte-vfs-local
```
Expected: FAIL, `open_root` answers `Unsupported`.

- [ ] **Step 3: write `confined.rs`**

```rust
/// A directory fd plus the way to walk beneath it.
pub(crate) struct LocalRoot {
    fd: OwnedFd,          // O_PATH|O_DIRECTORY sobre la raíz
    root_display: VPath,  // solo para errores y tracing (redactado)
}
```

Resolution, one function, blocking (`spawn_blocking` at the call boundary):

- Linux, first choice: `openat2` via `libc::syscall(libc::SYS_openat2, ...)`
  with `open_how { flags, mode, resolve: RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS }`,
  one call per relative path.
- `ENOSYS` (kernel < 5.6, or seccomp) and every non-Linux unix: walk the
  segments, `openat(prev_fd, seg, O_NOFOLLOW | O_DIRECTORY | O_PATH)` for each
  intermediate one, keeping only the current fd. `ELOOP` from `O_NOFOLLOW` is
  the escape verdict. There is never a path to recompose, so the walk carries
  the same guarantee, one syscall at a time.
- Map `ELOOP`/`EXDEV`/`openat2`'s `EXDEV` to
  `Error::Conflict { kind: ConflictKind::EscapesRoot }`; everything else
  through the existing `map_io`.
- Cache nothing: an fd IS the cache, and a resolved-once path would reintroduce
  the window.

`force_component_walk_for_test()` is a `#[doc(hidden)]` seam returning a guard
that sets a process-global `AtomicBool` and clears it on `Drop` — the same
shape as the existing test seams, and it must be honoured by the resolver
BEFORE it tries the syscall.

Every `unsafe` block carries a `// SAFETY:` naming the invariant (the fd is
owned and open for the whole call; `open_how` is zeroed and its size passed
exactly; the `CString` outlives the syscall).

- [ ] **Step 4: run the tests**

```
just t norte-vfs-local
```
Expected: PASS, including the forced component walk.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-vfs-local
git commit -m "feat(vfs-local): a confined root refuses to leave itself"
```

---

### Task B3: the confined sink

**Files:**
- Modify: `crates/norte-vfs-local/src/confined.rs`
- Test: `crates/norte-vfs-local/tests/confined.rs`

- [ ] **Step 1: write the failing tests**

```rust
#[tokio::test]
async fn the_publish_is_confined_too() {
    // El staging se crea con `openat` en el dirfd y se publica con `renameat`
    // en el MISMO dirfd: entre abrir y publicar nadie puede meter un symlink
    // que mande el rename a otro sitio.
    let tmp = tempfile::tempdir().expect("tmp");
    let dentro = tmp.path().join("dest");
    std::fs::create_dir(&dentro).expect("dest");
    let p = LocalProvider::rooted(tmp.path());
    let root = p.open_root(&vpath_de(&dentro)).await.expect("raíz");

    let mut sink = root.write(&[seg(b"f.txt")]).await.expect("write");
    sink.write(Bytes::from_static(b"contenido")).await.expect("chunk");
    // Carrera: alguien sustituye el destino por un symlink ANTES del commit.
    std::os::unix::fs::symlink(tmp.path().join("otro.txt"), dentro.join("f.txt"))
        .expect("symlink hostil");
    let err = sink.commit().await.expect_err("el publish no lo pisa");

    assert!(!tmp.path().join("otro.txt").exists(), "no escribió al otro lado");
    assert!(matches!(err, Error::Conflict { .. }), "respondió {err:?}");
}

#[tokio::test]
async fn a_cancelled_write_leaves_no_unmarked_partial() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dentro = tmp.path().join("dest");
    std::fs::create_dir(&dentro).expect("dest");
    let p = LocalProvider::rooted(tmp.path());
    let root = p.open_root(&vpath_de(&dentro)).await.expect("raíz");

    let mut sink = root.write(&[seg(b"g.txt")]).await.expect("write");
    sink.write(Bytes::from_static(b"a medias")).await.expect("chunk");
    sink.abort().await.expect("abort");

    let restos: Vec<_> = std::fs::read_dir(&dentro).expect("listar")
        .map(|e| e.expect("entrada").file_name())
        .collect();
    assert!(
        restos.iter().all(|n| n.as_encoded_bytes().starts_with(b".norte-partial")),
        "solo staging marcado, jamás un parcial sin marcar: {restos:?}"
    );
}
```

- [ ] **Step 2: run and watch them fail**

```
just t norte-vfs-local
```

- [ ] **Step 3: implement the sink**

A `ByteSink` holding the root's `OwnedFd`, the staging name and the final
segment name. `write` → `openat(dirfd, ".norte-partial-<pid>-<seq>", O_CREAT |
O_EXCL | O_WRONLY | O_NOFOLLOW)`; `commit` → `renameat(dirfd, staging, dirfd,
final)` reusing `provider.rs`'s no-replace rename contract (`do_rename`'s
`rename_noreplace` path, at the `renameat2` level so the no-replace guarantee
survives); `abort` → `unlinkat(dirfd, staging)`. `open_resumable` reuses the
stable staging name the crate already computes (`provider.rs:221`), resolved
through the dirfd rather than through a path.

Reuse the existing staging naming and the existing `.norte-partial` prefix —
this is one more way to reach the same files, not a second convention (the
sweeper in `provider.rs:1281` must keep recognising them).

- [ ] **Step 4: run the tests**

```
just t norte-vfs-local && cargo test -p norte-vfs-local --doc
```
Expected: PASS.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-vfs-local
git commit -m "feat(vfs-local): a confined write publishes through the same fd"
```

---

### Task B4: the core uses the root when there is one

> **Rescoped after B3 landed, by reading the call sites.** Three things this
> task's original text did not know:
>
> 1. **`ConfinedRoot` needs a `symlink` method.** `sync::exec::copy_leaf` copies
>    a symlink entry by CREATING one at the destination
>    (`ops::symlink_retrying`), which composes a path exactly like the other two.
>    Without it, a `Copy` whose source is a symlink stays unconfined — and it is
>    one trait method plus a `symlinkat`.
> 2. **`ops::copy_file` reaches the destination in four places** — `copy_native`,
>    `open_resumable`, `write`, and the partial digest — and only the middle two
>    create anything. The seam that keeps this small is a destination *opener*
>    (an enum over `(&dyn Provider, VPath)` and `(&dyn ConfinedRoot, Vec<Segment>)`)
>    used for those two calls, leaving the `VPath` in place for progress,
>    journal and error text.
> 3. **The root is opened once per Task, in `SyncTargets`**, next to
>    `dest_root` — whose rustdoc is where #164 was written down, and which this
>    task should rewrite rather than leave describing a hole that is closed.
>
> Do NOT land half of it. A wave where `CreateDir` is confined and `Copy` is not
> closes the secondary vector and leaves the primary one open, while the
> capability says the location can be confined.

**Files:**
- Modify: `crates/norte-core/src/ops.rs:250` (`mkdir_retrying`), `:1159` (the copy sink)
- Modify: whatever calls them with a known root (`sync/`, the copy engine)
- Test: `crates/norte-core/tests/`

- [ ] **Step 1: write the failing test**

```rust
#[tokio::test]
async fn a_sync_does_not_follow_an_intermediate_symlink_out_of_its_destination() {
    // El caso de #164, end to end y contra el FS real.
    let tmp = tempfile::tempdir().expect("tmp");
    let (origen, destino, fuera) = tres_dirs(&tmp);
    std::fs::write(origen.join("sub/secreto.txt"), b"x").expect("origen");
    std::os::unix::fs::symlink(&fuera, destino.join("sub")).expect("symlink");

    let resultado = sincroniza(&origen, &destino).await;

    assert!(!fuera.join("secreto.txt").exists(), "no escribió fuera del destino");
    assert!(resultado.has_conflict_escapes_root(), "y lo contó como conflicto");
}
```

Build it on the existing sync integration harness; `tres_dirs`/`sincroniza`
stand in for whatever that harness already calls them.

- [ ] **Step 2: run and watch it fail**

```
just t norte-core
```
Expected: FAIL — the file appears in `fuera`.

- [ ] **Step 3: route the two operations through a root when the destination has one**

At the point where a recursive operation knows its destination root (the copy
engine's task setup and the sync executor's), call `dst.open_root(&dest_root)`
ONCE and carry the `Option<Box<dyn ConfinedRoot>>` alongside the provider:

- `Some(root)` → `root.mkdir(rel)` / `root.write(rel)` / `root.open_resumable(rel)`.
- `None` (`Unsupported`) → today's path, plus, once per operation and not once
  per step:

```rust
tracing::warn!(
    task_id = %ctx.task_id,
    dest = %redact(&dest_root),
    "destination cannot confine writes: an intermediate symlink could redirect \
     this operation outside its root (#164)"
);
```

Keep `mkdir_retrying`'s retry/ambiguity logic exactly as it is — it takes the
operation as a closure so both paths share it, rather than growing a second
copy of a subtle loop.

- [ ] **Step 4: run the tests**

```
just t norte-core
```
Expected: PASS.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-core
git commit -m "fix(core): a recursive write stays under its destination root"
```

---

### Task B5: `CONFINED_WRITES`, and saying it before the fact

**Files:**
- Modify: `crates/norte-vfs-local/src/caps_at.rs` (set the flag)
- Create: `crates/norte-frontend/src/confine.rs`
- Modify: `crates/norte-frontend/src/lib.rs`, `crates/norte-tui/src/app.rs`
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`
- Test: `crates/norte-frontend/src/confine.rs`, `crates/norte-tui/tests/snapshots_ui.rs`

- [ ] **Step 1: write the failing tests**

```rust
// norte-frontend/src/confine.rs
#[test]
fn a_destination_that_confines_says_nothing() {
    let caps = Capabilities {
        flags: CapabilityFlags::CONFINED_WRITES, max_path: None,
    };
    assert_eq!(warning_for(caps), None);
}

#[test]
fn a_destination_that_cannot_confine_warns_once() {
    let caps = Capabilities { flags: CapabilityFlags::empty(), max_path: None };
    assert!(warning_for(caps).is_some(), "el humano decide, pero enterado");
}
```

- [ ] **Step 2: run and watch them fail**

```
just t norte-frontend
```

- [ ] **Step 3: implement**

- `caps_at.rs`: on Linux and macOS set `CONFINED_WRITES` when the resolver is
  available (Linux: always — `openat2` or the walk; macOS: always — the walk).
  On Windows: never, in this phase.
- `norte-frontend/src/confine.rs`: `pub fn warning_for(caps: Capabilities) ->
  Option<Warning>`, modelled on `space.rs` (#149) — same shape, same "state the
  fact, let the human proceed" contract. It NEVER blocks.
- `norte-tui/src/app.rs`: add the line to the transfer confirmation where
  `space.rs`'s line already goes.
- Fluent keys in both `en.ftl` and `es.ftl`; no hard-coded string (convention).

- [ ] **Step 4: run the tests**

```
just t norte-frontend && just t norte-tui
```
Expected: PASS. Update the TUI snapshot if the confirmation grew a line — read
the snapshot diff before accepting it.

- [ ] **Step 5: commit**

```bash
git add -A crates/norte-vfs-local crates/norte-frontend crates/norte-tui crates/norte-i18n
git commit -m "feat(frontend,tui): a copy says when its destination cannot be confined"
```

---

### Task B6: close the branch

- [ ] **Step 1: open the follow-up issue**

```bash
gh issue create --title "[vfs-local] Confined writes on Windows need NtCreateFile with a relative handle" --body "..."
```

Body: Windows has no `openat`; confinement there means `NtCreateFile` with a
relative handle and `FILE_FLAG_OPEN_REPARSE_POINT`. Until then
`capabilities_at` does not declare `CONFINED_WRITES` on Windows and every
recursive write there degrades with the warning. Point at ADR 0054 and at this
plan.

- [ ] **Step 2: changelog and docs**

`CHANGELOG.md`: confined roots, `CONFINED_WRITES`, #164 closed with the
platform matrix stated (Linux and macOS confine; Windows and the remote
providers degrade and say so).

- [ ] **Step 3: dispatch the reviewers**

`security-reviewer` (mandatory — this phase IS a security boundary; ask it
specifically about the `ENOSYS` fallback, the publish step, and whether the
degradation path can be reached silently), `rust-reviewer` (the `unsafe` and
the fd lifetimes), `protocol-guardian` (only if anything touched the wire after
task A2 — it should not have). Apply BLOCKER and MAJOR in one pass. Say which
MINORs were skipped and why.

- [ ] **Step 4: the one full gate run**

```
just ci
```
Expected: PASS. Run it in the FOREGROUND, one recipe at a time if it is close
to the timeout (`lint`, `test`, `docs`, `check-gui`, `cov`), and never through
`| tail`.

- [ ] **Step 5: close and merge**

```bash
gh issue close 164 --comment "..."
```
Then use `superpowers:finishing-a-development-branch`.

---

## What this plan does NOT do

- **Windows confinement** — its own issue, opened in B6.
- **`DeleteTree`/`rename` through a confined root** — they dodge the hole today
  for reasons written in the spec; widening the handle with no bug behind it is
  scope nobody asked for.
- **A tamper-evident record of an unconfined write** — needs journal chain
  format 2 (`journal.rs:443`). Spec §3 says why, and it is a wave of its own.
- **#122** — listed in W5's original wave file, shares no mechanism with these
  three issues, stays open.
