# Columns block 2 — VFS provider attributes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Providers advertise and produce per-entry attributes (`attrs()` + `list_with`/`stat_with` with `ListOptions`), the daemon validates/forwards/enforces them on the wire that block 1 already shipped (proto 0.30.0, ADR 0039), and the conformance suite makes type agreement a contract.

**Architecture:** The `Provider` trait gains three **defaulted** members so no existing provider (incl. the WASM bridge) breaks: `attrs() -> &[AttrInfo]`, `list_with(p, &ListOptions)`, `stat_with(p, &ListOptions)`. `ListOptions { attrs: AttrRequest }` is a struct so future options don't churn signatures; `AttrRequest` is a sanitized id set (valid, deduped first-wins, ≤16). Each producing provider implements an internal `*_inner(p, req)` that both `list`/`list_with` call (never default-delegation cycles). The daemon rejects malformed/over-cap requested ids (`-32602`), forwards **only ids the target provider advertises**, retains the request in `OpenListing` for paginated continuations, and enforces emit caps per entry (belt against a buggy provider). The embedded path hands every catalog to `AttrCatalog::new` (ADR 0039 §4 mandate).

**Tech Stack:** existing proto 0.30.0 attr types (no proto change, **no version bump**), `std::os::unix::fs::MetadataExt`, russh-sftp `FileAttributes`, opendal 0.58 `Metadata`, archive index `Locator::Zip`.

**Spec:** `docs/superpowers/specs/2026-07-24-columns-design.md` Layer 3. ADR 0039. Issue #108.

**Scope decisions (recorded):**
- **`sftp.owner`/`sftp.group` (Bytes) are OUT.** russh-sftp 2.3 hardcodes `user: None`/`group: None` on decode (SFTP v3 wire carries only uid/gid) and drops the `longname` before it reaches the client API. Shipping them needs upstream/vendored work adjacent to issue #37 (raw `SSH_FXP_NAME` bytes). Debt issue filed in Task 9. sftp ships `posix.mode`/`posix.uid`/`posix.gid`, all free on the already-parsed `SSH_FXP_ATTRS`.
- **`s3.storage_class` is OUT.** opendal 0.58 discards `<StorageClass>` at parse time (`ListObjectsOutputContent` deserialises only key/size/last_modified/ETag) and `Metadata` has no field for it. Debt issue filed in Task 9. object ships `s3.etag` (free on list+stat) and `s3.content_type` (stat only — absent on list pages is contract-legal "absence means absence"; the UI hydration re-stat fills it).
- **archive:** zip ships `archive.method` (Text), `archive.packed_size` (Uint/Size), `archive.crc32` (Uint). The data already lives in `Locator::Zip` but is lost for encrypted/unsupported-method entries (`locator: None`) — exactly where `method` matters most — so a `ZipExtra` field lands on `Node`, populated for **every** zip entry. tar/tar.gz advertise **nothing** (no per-entry method/crc exists; per-entry packed size in a solid gz stream is meaningless). tar header mode/uid/gid stay out (would be `posix.*` on a read-only provider; not in the spec table).
- **local list stays lazy (#52).** `list()` untouched; `list_with` with no advertised id requested delegates to the same fast path. Requesting any `posix.*` promotes each entry to one `DirEntry::metadata()` (lstat-equivalent on unix, no follow) inside the existing blocking producer — which also fills `size`/`mtime_ms` for free.
- **Frontend rendering of `attr:` columns is NOT this block** (later block; `columns-no-renderer` doctor finding stays). Client surface here: CLI `ls --attrs` (ADR 0039 §4 names it for block 2), `Backend::*_with` variants, `Backend::attr_catalog`. `--json` exposes `Entry.attrs` automatically.
- Requested-id validation lives in the **daemon** (`-32602`); `AttrRequest::sanitized` *filters* (it never errors) — it is the provider-side belt, not the wire gate.
- `mode` values are the raw `st_mode` (type bits included) as `Uint`; formatters already landed in block 3 handle presentation.

---

### Task 1: `norte-vfs` — `AttrRequest`, `ListOptions`, trait members, SessionProvider delegation

**Files:**
- Create: `crates/norte-vfs/src/options.rs`
- Modify: `crates/norte-vfs/src/lib.rs` (module + re-exports)
- Modify: `crates/norte-vfs/src/provider.rs` (trait members, doctest)
- Modify: `crates/norte-core/src/sessions.rs` (delegation + completeness test)

- [ ] **Step 1: Failing tests** — `crates/norte-vfs/src/options.rs` bottom `mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_filtra_invalidos_dedup_primero_gana_y_trunca() {
        let ids = vec![
            "posix.mode".to_owned(),
            "MAYUS.no".to_owned(),        // inválido: mayúsculas
            "sindot".to_owned(),          // inválido: sin punto
            "posix.mode".to_owned(),      // duplicado
            "s3.etag".to_owned(),
        ];
        let req = AttrRequest::sanitized(ids);
        assert_eq!(req.iter().collect::<Vec<_>>(), ["posix.mode", "s3.etag"]);
        assert!(req.wants("posix.mode"));
        assert!(!req.wants("mayus.no"));

        // Truncado al tope del wire: 20 ids válidos → 16.
        let many = (0..20).map(|i| format!("a.b{i}"));
        assert_eq!(AttrRequest::sanitized(many).iter().count(), norte_proto::ATTRS_MAX_REQUEST);
    }

    #[test]
    fn default_es_vacio_y_list_options_lo_envuelve() {
        assert!(AttrRequest::default().is_empty());
        assert!(ListOptions::default().attrs.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo nextest run -p norte-vfs options` → compile error (module missing).

- [ ] **Step 3: Implement `options.rs`**

```rust
//! Listing/stat options ([`ListOptions`]) and the sanitized attribute
//! request ([`AttrRequest`]) they carry (#108 block 2, ADR 0039).

use norte_proto::ATTRS_MAX_REQUEST;

/// Requested attribute ids, sanitized: every id valid per
/// [`norte_proto::is_valid_attr_id`], deduplicated (first wins), at most
/// [`ATTRS_MAX_REQUEST`]. This type FILTERS — rejecting a malformed request
/// with `-32602` is the daemon's job, *before* it builds one of these.
///
/// ```
/// use norte_vfs::AttrRequest;
/// let req = AttrRequest::sanitized(["posix.mode".to_owned(), "BAD".to_owned()]);
/// assert!(req.wants("posix.mode"));
/// assert!(!req.wants("BAD"));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttrRequest(Vec<String>);

impl AttrRequest {
    /// Builds a request by filtering: invalid ids dropped, duplicates
    /// dropped (first wins), truncated to [`ATTRS_MAX_REQUEST`].
    #[must_use]
    pub fn sanitized<I: IntoIterator<Item = String>>(ids: I) -> Self {
        let mut out: Vec<String> = Vec::new();
        for id in ids {
            if out.len() == ATTRS_MAX_REQUEST {
                break;
            }
            if norte_proto::is_valid_attr_id(&id) && !out.contains(&id) {
                out.push(id);
            }
        }
        Self(out)
    }

    /// No attribute requested: the provider must take its bare fast path.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Is `id` requested? Providers gate each materialisation on this.
    #[must_use]
    pub fn wants(&self, id: &str) -> bool {
        self.0.iter().any(|have| have == id)
    }

    /// Requested ids, in request order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

/// Options for [`Provider::list_with`]/[`Provider::stat_with`]. A struct so
/// future listing options extend it without churning every signature again.
///
/// ```
/// use norte_vfs::ListOptions;
/// assert!(ListOptions::default().attrs.is_empty());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListOptions {
    /// Attributes to materialise per entry. Empty = bare entries.
    pub attrs: AttrRequest,
}
```

`lib.rs`: `mod options;` + `pub use options::{AttrRequest, ListOptions};` (keep alphabetical order with existing re-exports). Note: `norte-vfs` enables `#![warn(missing_docs)]` — every public item above already carries rustdoc + doctest.

- [ ] **Step 4: Trait members in `provider.rs`** — after `list_skipped` (line ~116), with `use norte_proto::AttrInfo;` added to imports and `use crate::options::ListOptions;`:

```rust
    /// Catalog of per-entry attributes this provider can materialise
    /// (#108 block 2, ADR 0039). Default: none. A provider that returns a
    /// non-empty catalog MUST override [`Self::list_with`] and
    /// [`Self::stat_with`] — the conformance suite enforces the pairing
    /// and that values match the declared [`AttrType`](norte_proto::AttrType).
    ///
    /// The ids/labels here are provider-side; the daemon wraps them in
    /// `AttrCatalog::new` (sanitizing) before they touch the wire, and the
    /// embedded backend must do the same (ADR 0039 §4).
    fn attrs(&self) -> &[AttrInfo] {
        &[]
    }

    /// [`Self::list`] plus options. Default ignores the options and yields
    /// bare entries — correct for any provider with an empty catalog.
    /// Absence means absence: an unknown or unproducible requested id is
    /// omitted from `Entry::attrs`, never fabricated.
    async fn list_with(&self, p: &VPath, opt: &ListOptions) -> Result<EntryStream, Error> {
        let _ = opt;
        self.list(p).await
    }

    /// [`Self::stat`] plus options. Same contract as [`Self::list_with`].
    async fn stat_with(&self, p: &VPath, opt: &ListOptions) -> Result<Entry, Error> {
        let _ = opt;
        self.stat(p).await
    }
```

Extend the `NullProvider` doctest at the top of the file with nothing (defaults compile) — but add two doctest assertions exercising the defaults:

```rust
/// // Defaults del bloque 2 (#108): catálogo vacío, list_with ≡ list.
/// assert!(NullProvider.attrs().is_empty());
```

- [ ] **Step 5: SessionProvider delegation** — `crates/norte-core/src/sessions.rs`, inside `impl Provider for SessionProvider` (after `capabilities`, line ~529, and after `list`, line ~535):

```rust
    fn attrs(&self) -> &[norte_proto::AttrInfo] {
        self.inner.attrs()
    }
    async fn list_with(
        &self,
        p: &norte_proto::VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<norte_vfs::EntryStream, Error> {
        self.observe(self.inner.list_with(p, opt).await)
    }
    async fn stat_with(
        &self,
        p: &norte_proto::VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<norte_proto::Entry, Error> {
        self.observe(self.inner.stat_with(p, opt).await)
    }
```

In the completeness test (`wrapper_observa_todos_los_metodos`, sessions.rs:786): `AllPu` gains `list_with`/`stat_with` overrides returning `Err(pu())` (and an `attrs` returning a non-empty static slice so delegation is observable), plus two new `evicta!` entries:

```rust
    // Dentro de impl Provider for AllPu:
    fn attrs(&self) -> &[norte_proto::AttrInfo] {
        static UNO: std::sync::LazyLock<Vec<norte_proto::AttrInfo>> =
            std::sync::LazyLock::new(|| {
                vec![norte_proto::AttrInfo {
                    id: "allpu.x".into(),
                    label: "x".into(),
                    ty: norte_proto::AttrType::Bool,
                    hint: norte_proto::AttrHint::Opaque,
                }]
            });
        &UNO
    }
    async fn list_with(
        &self,
        _p: &VPath,
        _opt: &norte_vfs::ListOptions,
    ) -> Result<norte_vfs::EntryStream, Error> {
        Err(pu())
    }
    async fn stat_with(
        &self,
        _p: &VPath,
        _opt: &norte_vfs::ListOptions,
    ) -> Result<norte_proto::Entry, Error> {
        Err(pu())
    }
```

and in the test body: `evicta!` for both (mirroring the `list`/`stat` lines) **and** `assert_eq!(wrapper.attrs().len(), 1)` pinning pass-through of the non-`Result` method (the `capabilities` gap the review noted — don't repeat it for `attrs`).

- [ ] **Step 6: Run** — `cargo nextest run -p norte-vfs && cargo nextest run -p norte-core sessions` → PASS. `cargo clippy -p norte-vfs -p norte-core --all-targets` clean. Doctests: `cargo test --doc -p norte-vfs`.

- [ ] **Step 7: Commit** — `feat(vfs,core): Provider attrs()/list_with/stat_with + ListOptions (#108 block 2)`

---

### Task 2: conformance — attributes contract in both suites

**Files:**
- Modify: `crates/norte-vfs/src/contract.rs` (new tests at the end of the generated module, before the closing brace; imports at :50-56)
- Modify: `crates/norte-vfs/src/contract_ro.rs` (read-only variant)

- [ ] **Step 1: Extend macro prelude imports** — in `contract.rs` add to the `norte_proto` import list: `AttrType`, `AttrValue`, `is_valid_attr_id`, and the caps consts; add `$crate::{AttrRequest, ListOptions}`:

```rust
use $crate::__private::norte_proto::{
    ATTR_BYTES_MAX, ATTR_TEXT_MAX, ATTRS_MAX_ADVERTISED, AttrType, AttrValue,
    CapabilityFlags, ConflictKind, EntryKind, Error, Segment, VPath, is_valid_attr_id,
};
use $crate::{AttrRequest, ByteSink, ListOptions, Provider};
```

Shared helper inside the generated module:

```rust
fn attr_type_matches(ty: AttrType, v: &AttrValue) -> bool {
    matches!(
        (ty, v),
        (AttrType::Uint, AttrValue::Uint(_))
            | (AttrType::Int, AttrValue::Int(_))
            | (AttrType::Text, AttrValue::Text(_))
            | (AttrType::Bytes, AttrValue::Bytes(_))
            | (AttrType::TimeMs, AttrValue::TimeMs(_))
            | (AttrType::Bool, AttrValue::Bool(_))
    )
}

fn assert_attrs_contract(
    catalog: &[$crate::__private::norte_proto::AttrInfo],
    requested: &AttrRequest,
    entry: &$crate::__private::norte_proto::Entry,
) {
    for (id, v) in &entry.attrs {
        assert!(
            requested.wants(id),
            "attr NO pedido en {:?}: {id:?}",
            entry.path.display_lossy()
        );
        let info = catalog
            .iter()
            .find(|a| &a.id == id)
            .unwrap_or_else(|| panic!("attr no anunciado: {id:?}"));
        assert!(
            attr_type_matches(info.ty, v),
            "tipo declarado {:?} no casa con {v:?} para {id:?}",
            info.ty
        );
        match v {
            AttrValue::Text(s) => assert!(s.len() <= ATTR_TEXT_MAX, "Text sobre tope: {id:?}"),
            AttrValue::Bytes(b) => assert!(b.len() <= ATTR_BYTES_MAX, "Bytes sobre tope: {id:?}"),
            _ => {}
        }
    }
}
```

- [ ] **Step 2: The four contract tests** (append in `contract.rs`'s macro body):

```rust
// ---------- attrs (#108 bloque 2, ADR 0039) ----------

#[tokio::test]
async fn contract_attrs_catalog_is_sane() {
    let p = $factory;
    let catalog = p.attrs();
    assert!(catalog.len() <= ATTRS_MAX_ADVERTISED, "catálogo sobre tope");
    let mut seen = std::collections::BTreeSet::new();
    for info in catalog {
        assert!(is_valid_attr_id(&info.id), "id inválido en catálogo: {:?}", info.id);
        assert!(seen.insert(info.id.clone()), "id duplicado: {:?}", info.id);
    }
}

#[tokio::test]
async fn contract_attrs_values_match_declared_types() {
    let p = $factory;
    if p.attrs().is_empty() {
        eprintln!("skip: catálogo de attrs vacío");
        return;
    }
    let root: VPath = $root;
    write_all(&p, &child(&root, b"attrs-probe.txt"), b"contenido de prueba").await;
    let ids: Vec<String> = p.attrs().iter().map(|a| a.id.clone()).collect();
    let opt = ListOptions { attrs: AttrRequest::sanitized(ids) };
    let catalog = p.attrs().to_vec();

    // stat_with sobre el archivo sembrado.
    let e = p
        .stat_with(&child(&root, b"attrs-probe.txt"), &opt)
        .await
        .expect("stat_with");
    assert_attrs_contract(&catalog, &opt.attrs, &e);

    // list_with sobre la raíz: TODA entrada cumple.
    let mut stream = p.list_with(&root, &opt).await.expect("list_with");
    let mut n = 0usize;
    while let Some(e) = stream.next().await {
        let e = e.expect("entrada del listado");
        assert_attrs_contract(&catalog, &opt.attrs, &e);
        n += 1;
    }
    assert!(n >= 1, "el listado debe contener el probe");
}

#[tokio::test]
async fn contract_attrs_empty_request_yields_bare_entries() {
    let p = $factory;
    let root: VPath = $root;
    write_all(&p, &child(&root, b"attrs-bare.txt"), b"x").await;
    let opt = ListOptions::default();
    let e = p
        .stat_with(&child(&root, b"attrs-bare.txt"), &opt)
        .await
        .expect("stat_with");
    assert!(e.attrs.is_empty(), "sin petición no hay attrs");
    let mut stream = p.list_with(&root, &opt).await.expect("list_with");
    while let Some(e) = stream.next().await {
        assert!(e.expect("entrada").attrs.is_empty(), "sin petición no hay attrs");
    }
}

#[tokio::test]
async fn contract_attrs_unknown_requested_id_is_absent_not_error() {
    let p = $factory;
    let root: VPath = $root;
    write_all(&p, &child(&root, b"attrs-unk.txt"), b"x").await;
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(["zz.does-not-exist".to_owned()]),
    };
    let e = p
        .stat_with(&child(&root, b"attrs-unk.txt"), &opt)
        .await
        .expect("id desconocido jamás es error");
    assert!(!e.attrs.contains_key("zz.does-not-exist"));
}
```

- [ ] **Step 3: Read-only variant** — `contract_ro.rs` gets the same four, adapted: no `write_all` (the canonical pre-seeded tree provides entries); `ro_attrs_values_match_declared_types` runs `list_with` on the root and `stat_with` on the first `File` entry found. Same helper fns. Same import additions.

- [ ] **Step 4: Run the whole workspace suite** — `cargo nextest run --workspace`. All 11 instantiations pass: every provider still has an empty catalog, so the value tests auto-skip and the bare/unknown tests exercise the trait defaults. Expected: PASS.

**Known limit (recorded):** the contract cannot prove "advertises ⟹ overrode `*_with`" from outside — a legitimate value can be absent (e.g. services-fs without etag), which is indistinguishable from the ignore-the-request default. The contract pins type agreement, request-scoping, caps and no-error-on-unknown; **materialisation** is pinned by each provider's own unit test in Tasks 3-7 (each asserts at least one concrete value).

- [ ] **Step 5: Commit** — `test(vfs): conformance contract for provider attributes (#108 block 2)`

---

### Task 3: MemProvider — synthetic hostile attributes

**Files:**
- Modify: `crates/norte-testkit/src/mem.rs`
- Modify: `crates/norte-testkit/tests/contract.rs` (one new instantiation)

- [ ] **Step 1: Failing test** — new instantiation in `crates/norte-testkit/tests/contract.rs`:

```rust
norte_vfs::provider_contract! {
    mod mem_attrs,
    factory: norte_testkit::MemProvider::new().with_synthetic_attrs(),
    root: norte_testkit::MemProvider::root(),
    hostile_names: norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect(),
}
```

Plus a direct unit test in `mem.rs`'s test module pinning the hostile values round-trip:

```rust
#[tokio::test]
async fn synthetic_attrs_hostiles_y_deterministas() {
    use norte_vfs::{AttrRequest, ListOptions, Provider};
    let p = MemProvider::new().with_synthetic_attrs();
    let root = MemProvider::root();
    let f = root.join(seg(b"f.txt"));
    escribe(&p, &f, b"x").await; // helper existente del módulo de tests
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(
            ["mem.owner", "mem.note", "mem.mode", "mem.stamp"].map(str::to_owned),
        ),
    };
    let e = p.stat_with(&f, &opt).await.expect("stat_with");
    // Dueño no-UTF-8: BYTES crudos, jamás String (regla 1).
    assert_eq!(
        e.attrs.get("mem.owner"),
        Some(&norte_proto::AttrValue::Bytes(b"due\xf1o-\xff\xfe".to_vec()))
    );
    // Texto hostil: RTL override + ZWJ, dentro del tope.
    let norte_proto::AttrValue::Text(note) = e.attrs.get("mem.note").expect("note") else {
        panic!("mem.note debe ser Text");
    };
    assert!(note.contains('\u{202e}') && note.contains('\u{200d}'));
    assert_eq!(e.attrs.get("mem.mode"), Some(&norte_proto::AttrValue::Uint(0o100_644)));
    assert!(matches!(e.attrs.get("mem.stamp"), Some(norte_proto::AttrValue::TimeMs(_))));
    // Sin pedir → sin attrs.
    let bare = p.stat(&f).await.expect("stat");
    assert!(bare.attrs.is_empty());
}
```

- [ ] **Step 2: Run to verify failure** — `cargo nextest run -p norte-testkit synthetic_attrs` → FAIL (`with_synthetic_attrs` missing).

- [ ] **Step 3: Implement.** In `mem.rs`:

Field + builder (next to `with_list_skipped`, :207):

```rust
    /// Catálogo sintético de attrs (vacío = provider sin attrs, el default).
    attr_defs: Vec<norte_proto::AttrInfo>,
```

```rust
    /// Deterministic synthetic attributes (#108 block 2) with deliberately
    /// hostile values: non-UTF-8 owner (`Bytes`), RTL-override + ZWJ text.
    /// For render/plumbing tests without a real provider.
    #[must_use]
    pub fn with_synthetic_attrs(mut self) -> Self {
        use norte_proto::{AttrHint, AttrInfo, AttrType};
        let mk = |id: &str, label: &str, ty, hint| AttrInfo {
            id: id.to_owned(),
            label: label.to_owned(),
            ty,
            hint,
        };
        self.attr_defs = vec![
            mk("mem.owner", "Owner", AttrType::Bytes, AttrHint::Identity),
            mk("mem.note", "Note", AttrType::Text, AttrHint::Opaque),
            mk("mem.mode", "Mode", AttrType::Uint, AttrHint::Mode),
            mk("mem.stamp", "Stamp", AttrType::TimeMs, AttrHint::Timestamp),
        ];
        self
    }
```

Value function (associated fn near `entry_for_child`):

```rust
/// Valores sintéticos DETERMINISTAS por nodo: función de (kind, mtime).
fn synthetic_attrs(
    defs: &[norte_proto::AttrInfo],
    req: &norte_vfs::AttrRequest,
    node: &Node,
) -> std::collections::BTreeMap<String, norte_proto::AttrValue> {
    use norte_proto::AttrValue;
    let mut out = std::collections::BTreeMap::new();
    let quiere = |id: &str| defs.iter().any(|d| d.id == id) && req.wants(id);
    if quiere("mem.owner") {
        out.insert("mem.owner".into(), AttrValue::Bytes(b"due\xf1o-\xff\xfe".to_vec()));
    }
    if quiere("mem.note") {
        // RTL override + ZWJ: humo para el masking de los frontends.
        out.insert("mem.note".into(), AttrValue::Text("\u{202e}atón\u{202c} a\u{200d}b".into()));
    }
    if quiere("mem.mode") {
        let mode = match node {
            Node::Dir { .. } => 0o040_755,
            Node::File { .. } => 0o100_644,
            Node::Symlink { .. } => 0o120_777,
        };
        out.insert("mem.mode".into(), AttrValue::Uint(mode));
    }
    if quiere("mem.stamp") {
        let mtime = match node {
            Node::File { mtime, .. } | Node::Dir { mtime, .. } | Node::Symlink { mtime, .. } => *mtime,
        };
        out.insert("mem.stamp".into(), AttrValue::TimeMs(mtime));
    }
    out
}
```

Wiring: `entry_for_child`/`entry_for`/root-entry take a `req: &AttrRequest` parameter (existing callers pass `&AttrRequest::default()`), filling `attrs: Self::synthetic_attrs(&self.attr_defs, req, node)`. Trait impl:

```rust
    fn attrs(&self) -> &[norte_proto::AttrInfo] {
        &self.attr_defs
    }
    async fn list_with(&self, p: &VPath, opt: &ListOptions) -> Result<EntryStream, Error> {
        self.list_inner(p, opt.attrs.clone()).await
    }
    async fn stat_with(&self, p: &VPath, opt: &ListOptions) -> Result<Entry, Error> {
        self.stat_inner(p, &opt.attrs).await
    }
```

with `list`/`stat` refactored to call `list_inner(p, AttrRequest::default())` / `stat_inner(p, &AttrRequest::default())` — **never** the trait defaults (recursion).

- [ ] **Step 4: Run** — `cargo nextest run -p norte-testkit` → PASS, including the new `mem_attrs` contract instantiation now exercising real values (skip lines gone).

- [ ] **Step 5: Commit** — `feat(testkit): MemProvider synthetic hostile attrs (#108 block 2)`

---

### Task 4: local provider — `posix.*` (unix) / `win.attributes` (windows)

**Files:**
- Modify: `crates/norte-vfs-local/src/provider.rs`

- [ ] **Step 1: Failing test** — in the crate's test module (or `tests/`), unix-gated:

```rust
#[cfg(unix)]
#[tokio::test]
async fn attrs_posix_en_stat_y_list() {
    use norte_vfs::{AttrRequest, ListOptions, Provider};
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("a.txt"), b"hola").expect("seed");
    let p = proveedor_local(); // helper/constructor existente del módulo de tests
    let dir = vpath_de(tmp.path());
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(
            ["posix.mode", "posix.uid", "posix.gid", "posix.nlink", "posix.ctime_ms"]
                .map(str::to_owned),
        ),
    };
    let e = p.stat_with(&dir.join(seg(b"a.txt")), &opt).await.expect("stat_with");
    use norte_proto::AttrValue;
    let md = std::fs::symlink_metadata(tmp.path().join("a.txt")).expect("md");
    use std::os::unix::fs::MetadataExt;
    assert_eq!(e.attrs.get("posix.mode"), Some(&AttrValue::Uint(u64::from(md.mode()))));
    assert_eq!(e.attrs.get("posix.uid"), Some(&AttrValue::Uint(u64::from(md.uid()))));
    assert_eq!(e.attrs.get("posix.gid"), Some(&AttrValue::Uint(u64::from(md.gid()))));
    assert_eq!(e.attrs.get("posix.nlink"), Some(&AttrValue::Uint(md.nlink())));
    assert!(matches!(e.attrs.get("posix.ctime_ms"), Some(AttrValue::TimeMs(_))));

    // list_with promociona: attrs presentes Y size/mtime hidratados de paso.
    use futures::StreamExt;
    let mut s = p.list_with(&dir, &opt).await.expect("list_with");
    let le = s.next().await.expect("una entrada").expect("ok");
    assert!(le.attrs.contains_key("posix.mode"));
    assert!(le.size.is_some(), "la promoción a metadata llena size");

    // Camino rápido intacto (#52): sin petición, lazy como siempre.
    let mut s = p.list(&dir).await.expect("list");
    let le = s.next().await.expect("una entrada").expect("ok");
    assert!(le.attrs.is_empty() && le.size.is_none());
}
```

(Adapt `proveedor_local`/`vpath_de`/`seg` to the file's existing test helpers — the crate's tests already construct the provider and VPaths; copy the neighbouring pattern.)

- [ ] **Step 2: Run to verify failure** — `cargo nextest run -p norte-vfs-local attrs_posix` → FAIL (attrs empty).

- [ ] **Step 3: Implement.**

Catalog (module level):

```rust
#[cfg(unix)]
fn catalogo_posix() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        let mk = |id: &str, label: &str, ty, hint| AttrInfo {
            id: id.to_owned(),
            label: label.to_owned(),
            ty,
            hint,
        };
        vec![
            mk("posix.mode", "Mode", AttrType::Uint, AttrHint::Mode),
            mk("posix.uid", "UID", AttrType::Uint, AttrHint::Identity),
            mk("posix.gid", "GID", AttrType::Uint, AttrHint::Identity),
            mk("posix.nlink", "Links", AttrType::Uint, AttrHint::Opaque),
            mk("posix.ctime_ms", "Changed", AttrType::TimeMs, AttrHint::Timestamp),
        ]
    });
    &CAT
}

#[cfg(windows)]
fn catalogo_win() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        vec![AttrInfo {
            id: "win.attributes".to_owned(),
            label: "Attributes".to_owned(),
            ty: AttrType::Uint,
            hint: AttrHint::Opaque,
        }]
    });
    &CAT
}
```

Materialisation from an in-hand `Metadata` (zero extra syscalls):

```rust
fn attrs_from_md(
    md: &std::fs::Metadata,
    req: &norte_vfs::AttrRequest,
) -> std::collections::BTreeMap<String, norte_proto::AttrValue> {
    use norte_proto::AttrValue;
    let mut out = std::collections::BTreeMap::new();
    if req.is_empty() {
        return out;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if req.wants("posix.mode") {
            out.insert("posix.mode".into(), AttrValue::Uint(u64::from(md.mode())));
        }
        if req.wants("posix.uid") {
            out.insert("posix.uid".into(), AttrValue::Uint(u64::from(md.uid())));
        }
        if req.wants("posix.gid") {
            out.insert("posix.gid".into(), AttrValue::Uint(u64::from(md.gid())));
        }
        if req.wants("posix.nlink") {
            out.insert("posix.nlink".into(), AttrValue::Uint(md.nlink()));
        }
        if req.wants("posix.ctime_ms") {
            // ctime en ms con redondeo hacia -∞ también en pre-1970.
            let ms = md.ctime() * 1000 + md.ctime_nsec() / 1_000_000;
            out.insert("posix.ctime_ms".into(), AttrValue::TimeMs(ms));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if req.wants("win.attributes") {
            out.insert(
                "win.attributes".into(),
                AttrValue::Uint(u64::from(md.file_attributes())),
            );
        }
    }
    out
}
```

(Pre-1970 note: `ctime()*1000 + ctime_nsec()/1_000_000` — `ctime_nsec` is non-negative in practice on unix; keep the straightforward form and let the encoding reviewer weigh in.)

Trait impl:
- `fn attrs(&self)` → `#[cfg(unix)] catalogo_posix()` / `#[cfg(windows)] catalogo_win()` (cfg-else empty).
- `entry_from(vpath, md)` gains `req: &AttrRequest` → `attrs: attrs_from_md(md, req)`; existing callers pass `&AttrRequest::default()`.
- `stat_with`: copy of `stat` passing `&opt.attrs` to `entry_from` (refactor `stat` body into `stat_inner(p, req)`).
- `list_with`: if none of the requested ids is advertised (`!opt.attrs.iter().any(|id| self.attrs().iter().any(|a| a.id == id))`) → delegate to `self.list(p)` (fast path #52 intact). Else: refactor the `read_dir` producer into `list_inner(p, req: AttrRequest)`; in the promoted branch replace the `d.file_type()`-only entry with:

```rust
// Promoción (#108 bloque 2): attrs pedidos → un lstat por entrada
// (DirEntry::metadata NO sigue symlinks en unix), que además hidrata
// size/mtime de gratis. Sigue dentro del productor bloqueante.
let md = d.metadata().map_err(|e| map_io(&e))?;
Ok(entry_from(base_vpath.join(seg), &md, &req))
```

(`entry_from` already derives kind/size/mtime from a `Metadata`.)

- [ ] **Step 4: Run** — `cargo nextest run -p norte-vfs-local` → PASS (unit + the `local_fs` contract instantiation now exercises values).

- [ ] **Step 5: Commit** — `feat(vfs-local): posix.*/win.attributes provider attrs (#108 block 2)`

---

### Task 5: sftp provider — `posix.mode`/`posix.uid`/`posix.gid`

**Files:**
- Modify: `crates/norte-vfs-sftp/src/provider.rs`

- [ ] **Step 1: Failing test** — the crate's in-proc harness (russh dev-dep server, see `tests/contract.rs:42` factory) — unit test next to existing ones:

```rust
#[tokio::test]
async fn attrs_posix_desde_file_attributes() {
    // Harness in-proc existente (mismo factory que el contract).
    let (p, root) = harness_inproc().await;
    use norte_vfs::{AttrRequest, ListOptions, Provider};
    escribe(&p, &root.join(seg(b"f")), b"x").await;
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(["posix.mode", "posix.uid", "posix.gid"].map(str::to_owned)),
    };
    let e = p.stat_with(&root.join(seg(b"f")), &opt).await.expect("stat_with");
    assert!(matches!(e.attrs.get("posix.mode"), Some(norte_proto::AttrValue::Uint(_))));
    // uid/gid: presentes si el server los manda; si Some, son Uint.
    for id in ["posix.uid", "posix.gid"] {
        if let Some(v) = e.attrs.get(id) {
            assert!(matches!(v, norte_proto::AttrValue::Uint(_)), "{id} debe ser Uint");
        }
    }
    // Sin pedir → nada.
    assert!(p.stat(&root.join(seg(b"f"))).await.expect("stat").attrs.is_empty());
}
```

- [ ] **Step 2: Run to verify failure** — `cargo nextest run -p norte-vfs-sftp attrs_posix` → FAIL.

- [ ] **Step 3: Implement.** Catalog:

```rust
fn catalogo_sftp() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        let mk = |id: &str, label: &str, hint| AttrInfo {
            id: id.to_owned(),
            label: label.to_owned(),
            ty: AttrType::Uint,
            hint,
        };
        vec![
            mk("posix.mode", "Mode", AttrHint::Mode),
            mk("posix.uid", "UID", AttrHint::Identity),
            mk("posix.gid", "GID", AttrHint::Identity),
        ]
    });
    &CAT
}
// NOTA (deuda, issue creado en Task 9): `sftp.owner`/`sftp.group` como Bytes
// exigen el longname crudo de SSH_FXP_NAME; russh-sftp 2.3 lo descarta y
// decodifica user/group a None SIEMPRE en v3 — adyacente a #37.
```

`entry_from(path, md, req)`:

```rust
let mut attrs = std::collections::BTreeMap::new();
if let Some(perm) = md.permissions
    && req.wants("posix.mode")
{
    attrs.insert("posix.mode".into(), norte_proto::AttrValue::Uint(u64::from(perm)));
}
if let Some(uid) = md.uid
    && req.wants("posix.uid")
{
    attrs.insert("posix.uid".into(), norte_proto::AttrValue::Uint(u64::from(uid)));
}
if let Some(gid) = md.gid
    && req.wants("posix.gid")
{
    attrs.insert("posix.gid".into(), norte_proto::AttrValue::Uint(u64::from(gid)));
}
```

Trait impl: `attrs()` → `catalogo_sftp()`; `stat_with`/`list_with` thread the request to `entry_from` (both call sites, :281-291 and :325); `stat`/`list` pass `&AttrRequest::default()`. The listing is already materialised into a `Vec` before streaming, so threading is a plain parameter.

- [ ] **Step 4: Run** — `cargo nextest run -p norte-vfs-sftp` → PASS (contract instantiation `sftp_inproc` now exercises values).

- [ ] **Step 5: Commit** — `feat(vfs-sftp): posix.mode/uid/gid provider attrs (#108 block 2)`

---

### Task 6: object provider — `s3.etag` + `s3.content_type`

**Files:**
- Modify: `crates/norte-vfs-object/src/provider.rs`

- [ ] **Step 1: Failing test** — against the in-proc `services-fs` harness the crate's tests already use:

```rust
#[tokio::test]
async fn attrs_s3_etag_y_content_type() {
    use norte_vfs::{AttrRequest, ListOptions, Provider};
    let (p, root) = harness_fs().await; // helper existente de los tests
    escribe(&p, &root.join(seg(b"o.txt")), b"x").await;
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(["s3.etag", "s3.content_type"].map(str::to_owned)),
    };
    let e = p.stat_with(&root.join(seg(b"o.txt")), &opt).await.expect("stat_with");
    // services-fs puede no dar etag/content_type: si están, son Text acotado.
    for id in ["s3.etag", "s3.content_type"] {
        if let Some(v) = e.attrs.get(id) {
            let norte_proto::AttrValue::Text(s) = v else { panic!("{id} debe ser Text") };
            assert!(s.len() <= norte_proto::ATTR_TEXT_MAX);
        }
    }
    // Sin pedir → nada; y dirs jamás llevan attrs de objeto.
    assert!(p.stat(&root.join(seg(b"o.txt"))).await.expect("stat").attrs.is_empty());
}
```

(The nightly MinIO harness gives real etag/content_type coverage; the fs-service test pins shape, absence-tolerance, and the no-request path.)

- [ ] **Step 2: Run to verify failure** — `cargo nextest run -p norte-vfs-object attrs_s3` → FAIL.

- [ ] **Step 3: Implement.** Catalog:

```rust
fn catalogo_s3() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        let mk = |id: &str, label: &str| AttrInfo {
            id: id.to_owned(),
            label: label.to_owned(),
            ty: AttrType::Text,
            hint: AttrHint::Opaque,
        };
        // `s3.content_type` solo se materializa en stat (HeadObject);
        // en listados va AUSENTE — opendal no lo trae en ListObjectsV2.
        // `s3.storage_class`: imposible con opendal 0.58 (lo descarta al
        // parsear) — deuda, issue creado en Task 9.
        vec![mk("s3.etag", "ETag"), mk("s3.content_type", "Content-Type")]
    });
    &CAT
}

fn attrs_from_meta(
    m: &opendal::Metadata,
    req: &norte_vfs::AttrRequest,
) -> std::collections::BTreeMap<String, norte_proto::AttrValue> {
    use norte_proto::AttrValue;
    let mut out = std::collections::BTreeMap::new();
    let mut texto = |id: &str, v: Option<&str>| {
        if let Some(s) = v
            && req.wants(id)
            && s.len() <= norte_proto::ATTR_TEXT_MAX
        {
            out.insert(id.to_owned(), AttrValue::Text(s.to_owned()));
        }
    };
    texto("s3.etag", m.etag());
    texto("s3.content_type", m.content_type());
    out
}
```

`file_entry(path, m, req)` → `attrs: attrs_from_meta(m, req)`. Dir entries (inline at :353/:363/:431) keep empty attrs. Trait impl: `attrs()` → `catalogo_s3()`; `stat_with` refactors `stat` into `stat_inner(p, req)`; `list_with` refactors `list` into `list_inner(p, req)` — the lazy `try_filter_map` closure captures `req` (clone into the stream).

- [ ] **Step 4: Run** — `cargo nextest run -p norte-vfs-object` → PASS.

- [ ] **Step 5: Commit** — `feat(vfs-object): s3.etag/content_type provider attrs (#108 block 2)`

---

### Task 7: archive provider — `archive.method`/`packed_size`/`crc32` (zip)

**Files:**
- Modify: `crates/norte-vfs-archive/src/index.rs` (`Node` + `entry_for`)
- Modify: `crates/norte-vfs-archive/src/zip_format.rs` (populate `ZipExtra` incl. unreadable entries)
- Modify: `crates/norte-vfs-archive/src/provider.rs` (catalog per format, `stat_with`/`list_with`)

- [ ] **Step 1: Failing test** — in the crate's tests (ZipSmith fixtures are code):

```rust
#[tokio::test]
async fn attrs_zip_method_packed_crc() {
    use norte_vfs::{AttrRequest, ListOptions, Provider};
    // ZipSmith: un deflate normal y una entrada CIFRADA (locator None).
    let (p, root) = harness_zip_con_cifrada().await; // componer con ZipSmith existente
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(
            ["archive.method", "archive.packed_size", "archive.crc32"].map(str::to_owned),
        ),
    };
    let e = p.stat_with(&root.join(seg(b"normal.txt")), &opt).await.expect("stat_with");
    assert_eq!(
        e.attrs.get("archive.method"),
        Some(&norte_proto::AttrValue::Text("deflate".into()))
    );
    assert!(matches!(e.attrs.get("archive.packed_size"), Some(norte_proto::AttrValue::Uint(_))));
    assert!(matches!(e.attrs.get("archive.crc32"), Some(norte_proto::AttrValue::Uint(_))));

    // La CIFRADA (no legible, locator None) SÍ conserva sus attrs:
    // justo donde method más importa.
    let enc = p.stat_with(&root.join(seg(b"cifrada.txt")), &opt).await.expect("stat_with");
    assert!(enc.attrs.contains_key("archive.method"));

    // tar: catálogo vacío (sin method/crc per-entry en el formato).
    let (pt, _) = harness_tar().await;
    assert!(pt.attrs().is_empty());
}
```

- [ ] **Step 2: Run to verify failure** — `cargo nextest run -p norte-vfs-archive attrs_zip` → FAIL.

- [ ] **Step 3: Implement.**

`index.rs` — `Node` gains:

```rust
    /// Metadatos zip por entrada (#108 bloque 2), presentes TAMBIÉN cuando
    /// `locator` es `None` (cifradas / método no soportado): method es
    /// precisamente más interesante ahí. `None` en tar/tar.gz y dirs.
    pub zip: Option<ZipExtra>,
```

```rust
/// Tripleta zip retenida para attrs (independiente del `Locator`, que solo
/// existe para entradas LEGIBLES).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ZipExtra {
    pub method: u16,
    pub crc32: u32,
    pub comp_size: u64,
}
```

`zip_format.rs` (:53-67 area): where `Node` is built, alongside the `readable.then_some(locator)` computation, always set `zip: Some(ZipExtra { method: cd.method, crc32: cd.crc32, comp_size: cd.comp_size })` for file entries (dirs/symlinks `None`). tar/targz format builders set `zip: None` (struct-literal update everywhere `Node` is built — the compiler drives this).

`entry_for(at, inner, req)`:

```rust
let attrs = node.zip.map_or_else(std::collections::BTreeMap::new, |z| {
    let mut out = std::collections::BTreeMap::new();
    if req.wants("archive.method") {
        out.insert("archive.method".into(), AttrValue::Text(zip_method_name(z.method)));
    }
    if req.wants("archive.packed_size") {
        out.insert("archive.packed_size".into(), AttrValue::Uint(z.comp_size));
    }
    if req.wants("archive.crc32") {
        out.insert("archive.crc32".into(), AttrValue::Uint(u64::from(z.crc32)));
    }
    out
});
```

```rust
/// Nombre humano del método zip; desconocido = "method-N" (jamás falla).
fn zip_method_name(m: u16) -> String {
    match m {
        0 => "store".into(),
        8 => "deflate".into(),
        9 => "deflate64".into(),
        12 => "bzip2".into(),
        14 => "lzma".into(),
        93 => "zstd".into(),
        95 => "xz".into(),
        99 => "aes".into(),
        n => format!("method-{n}"),
    }
}
```

`provider.rs`: catalog gated on format —

```rust
fn catalogo_zip() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        vec![
            AttrInfo {
                id: "archive.method".into(),
                label: "Method".into(),
                ty: AttrType::Text,
                hint: AttrHint::Opaque,
            },
            AttrInfo {
                id: "archive.packed_size".into(),
                label: "Packed".into(),
                ty: AttrType::Uint,
                hint: AttrHint::Size,
            },
            AttrInfo {
                id: "archive.crc32".into(),
                label: "CRC-32".into(),
                ty: AttrType::Uint,
                hint: AttrHint::Opaque,
            },
        ]
    });
    &CAT
}
```

`fn attrs(&self)` matches the provider's format enum: zip → `catalogo_zip()`, tar/targz → `&[]`. `stat_with`/`list_with` thread `req` into `entry_for` (call sites :896 and :935); `stat`/`list` pass default. The read-only conformance instantiations (`zip_ro`) now exercise values; `tar_ro`/`targz_ro` auto-skip.

- [ ] **Step 4: Run** — `cargo nextest run -p norte-vfs-archive` → PASS.

- [ ] **Step 5: Commit** — `feat(vfs-archive): zip method/packed_size/crc32 attrs (#108 block 2)`

---

### Task 8: engine + daemon — catalog on `fs.capabilities`, `-32602`, forward-advertised-only, emit caps

**Files:**
- Modify: `crates/norte-core/src/engine.rs` (three methods, after `list` :388)
- Modify: `crates/norte-core/src/daemon/server.rs` (`handle_fs_list` :1002, `continue_listing` :1094, `OpenListing` :897, `fs.stat` :2716, `fs.capabilities` :2863, helpers)
- Modify: `crates/norte-core/src/ops.rs` (:1776-1783 TODO resolution)
- Test: `crates/norte-core/tests/` daemon suite (same harness as existing daemon tests)

- [ ] **Step 1: Failing daemon tests** — in the existing daemon test file (UDS harness over a tempdir/local engine; on unix the local provider advertises `posix.*`):

```rust
#[tokio::test]
async fn fs_capabilities_publica_el_catalogo_del_provider() {
    // harness existente: daemon sobre engine local + tempdir
    let r: FsCapabilitiesResult = llama("fs.capabilities", json!({"path": dir_uri})).await;
    #[cfg(unix)]
    assert!(r.attrs.iter().any(|a| a.id == "posix.mode"));
}

#[tokio::test]
async fn fs_list_attrs_malformado_o_sobre_tope_es_invalid_params() {
    // id malformado
    let err = llama_err("fs.list", json!({"path": dir_uri, "attrs": ["MAYUS.no"]})).await;
    assert_eq!(err.code, codes::INVALID_PARAMS);
    // 17 ids válidos (testigo del deserializador: materializa 16+1)
    let ids: Vec<String> = (0..17).map(|i| format!("a.b{i}")).collect();
    let err = llama_err("fs.list", json!({"path": dir_uri, "attrs": ids})).await;
    assert_eq!(err.code, codes::INVALID_PARAMS);
}

#[cfg(unix)]
#[tokio::test]
async fn fs_stat_devuelve_solo_lo_pedido_y_anunciado() {
    let r: FsStatResult = llama(
        "fs.stat",
        json!({"path": file_uri, "attrs": ["posix.mode", "zz.desconocido"]}),
    )
    .await;
    assert!(matches!(r.entry.attrs.get("posix.mode"), Some(AttrValue::Uint(_))));
    assert!(!r.entry.attrs.contains_key("zz.desconocido")); // ausente, no error
    assert_eq!(r.entry.attrs.len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn fs_list_paginado_conserva_los_attrs_del_arranque() {
    // 3 archivos, limit=1: las TRES páginas llevan posix.mode aunque la
    // continuación no re-mande attrs (el stream nació con las opciones).
    // Sembrar a.txt/b.txt/c.txt en el tempdir del harness antes.
    let mut r: FsListResult = llama(
        "fs.list",
        json!({"path": dir_uri, "limit": 1, "attrs": ["posix.mode"]}),
    )
    .await;
    let mut paginas = 1;
    loop {
        assert_eq!(r.entries.len(), 1, "página {paginas}");
        assert!(
            r.entries[0].attrs.contains_key("posix.mode"),
            "página {paginas} sin posix.mode"
        );
        let Some(cursor) = r.next_cursor.clone() else { break };
        // La continuación NO re-manda attrs: el stream retenido ya los lleva.
        r = llama("fs.list", json!({"path": dir_uri, "limit": 1, "cursor": cursor})).await;
        paginas += 1;
    }
    assert_eq!(paginas, 3);
}
```

(Adapt `llama`/`llama_err` to the harness's real call helpers — mirror the neighbouring `fs.list` pagination tests.)

- [ ] **Step 2: Run to verify failure** — `cargo nextest run -p norte-core fs_capabilities_publica` etc. → FAIL.

- [ ] **Step 3: Engine methods** (`engine.rs`, after `list`):

```rust
    /// [`Self::list`] con opciones (#108 bloque 2): atributos por entrada.
    pub async fn list_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<EntryStream, Error> {
        self.provider_for(p).await?.list_with(p, opt).await
    }

    /// [`Self::stat`] con opciones (#108 bloque 2).
    pub async fn stat_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<Entry, Error> {
        self.provider_for(p).await?.stat_with(p, opt).await
    }

    /// Catálogo de attrs del provider de `p`, SANEADO: `AttrCatalog::new`
    /// es el único camino al wire y también el del backend embebido
    /// (ADR 0039 §4).
    pub async fn attr_catalog(&self, p: &VPath) -> Result<norte_proto::AttrCatalog, Error> {
        Ok(norte_proto::AttrCatalog::new(
            self.provider_for(p).await?.attrs().to_vec(),
        ))
    }
```

- [ ] **Step 4: Daemon helpers** (`server.rs`, near `handle_fs_list`):

```rust
/// Valida la petición de attrs del wire (ADR 0039 §4): id malformado o más
/// de `ATTRS_MAX_REQUEST` (el deserializador materializa 16+1 como testigo)
/// = `-32602`. Pedir un id VÁLIDO pero desconocido NO es error.
fn validate_attr_request(ids: &[String]) -> Result<(), RpcError> {
    if ids.len() > norte_proto::ATTRS_MAX_REQUEST {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            format!("attrs: at most {} ids per call", norte_proto::ATTRS_MAX_REQUEST),
        ));
    }
    if let Some(bad) = ids.iter().find(|id| !norte_proto::is_valid_attr_id(id)) {
        // `escape_debug`: el id inválido es entrada hostil — jamás crudo
        // en un mensaje de error (controles, RTL).
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            format!("attrs: malformed id \"{}\"", bad.escape_debug()),
        ));
    }
    Ok(())
}

/// Cinturón de emisión (ADR 0039 §5): solo ids pedidos, y Text/Bytes dentro
/// de tope. Un provider con bug pierde la celda, jamás rompe la página.
fn enforce_attr_caps(entry: &mut norte_proto::Entry, allowed: &norte_vfs::AttrRequest) {
    entry.attrs.retain(|id, v| {
        allowed.wants(id)
            && match v {
                norte_proto::AttrValue::Text(s) => s.len() <= norte_proto::ATTR_TEXT_MAX,
                norte_proto::AttrValue::Bytes(b) => b.len() <= norte_proto::ATTR_BYTES_MAX,
                _ => true,
            }
    });
}
```

- [ ] **Step 5: Wire the handlers.**

`fs.capabilities` (:2863-2880): replace the hardcoded default:

```rust
let capabilities = shared.engine.capabilities(&p.path).await.map_err(RpcError::from)?;
let attrs = shared.engine.attr_catalog(&p.path).await.map_err(RpcError::from)?;
to_value(&methods::FsCapabilitiesResult { capabilities, attrs })
```

`handle_fs_list` (:1002): after the `limit == 0` check —

```rust
// Solo ids que el provider ANUNCIA viajan a list_with; el resto se cae
// aquí (el daemon jamás inventa celdas). Helper compartido con fs.stat,
// definido en el Step 5.
let request = resolve_attr_request(&p.attrs, &p.path, shared).await?;
let opt = norte_vfs::ListOptions { attrs: request.clone() };
```

then `shared.engine.list_with(&p.path, &opt)` instead of `.list(...)` (:1037). Each drained entry passes through `enforce_attr_caps(&mut e, &request)` before pushing (both the fresh path and inside `continue_listing`). `OpenListing` gains `attrs: norte_vfs::AttrRequest` (stored at :1070-1086), and `continue_listing` receives it from the map entry to enforce caps per page — the continuation **ignores** `p.attrs` (the stream was born with its options; validation still runs). Document this on `handle_fs_list`'s rustdoc.

`fs.stat` (:2716-2721):

```rust
methods::FS_STAT => {
    let p: methods::FsStatParams = parse_params(req.params)?;
    read_gate(&actor, &p.path, shared)?; // #80
    validate_attr_request(&p.attrs)?;
    let request = /* mismo bloque intersección-con-anunciados que fs.list */;
    let mut entry = shared
        .engine
        .stat_with(&p.path, &norte_vfs::ListOptions { attrs: request.clone() })
        .await
        .map_err(RpcError::from)?;
    enforce_attr_caps(&mut entry, &request);
    to_value(&methods::FsStatResult { entry })
}
```

The intersection block is shared — one helper, used verbatim by both handlers (so `fs.stat`'s `let request = ...` above is a call to this, not a re-implementation):

```rust
/// Valida `p.attrs` y lo cruza con el catálogo del provider de `path`:
/// devuelve la petición que de verdad viaja al provider. Id válido pero no
/// anunciado se CAE aquí (ausencia, no error — ADR 0039 §1).
async fn resolve_attr_request(
    ids: &[String],
    path: &norte_proto::VPath,
    shared: &Arc<Shared>,
) -> Result<norte_vfs::AttrRequest, RpcError> {
    validate_attr_request(ids)?;
    if ids.is_empty() {
        return Ok(norte_vfs::AttrRequest::default());
    }
    let advertised = shared.engine.attr_catalog(path).await.map_err(RpcError::from)?;
    Ok(norte_vfs::AttrRequest::sanitized(
        ids.iter()
            .filter(|id| advertised.iter().any(|a| &a.id == *id))
            .cloned(),
    ))
}
```

`ops.rs` :1776-1783: resolve the TODO — the synthetic dir `PlanEntry` propagates **no** attrs (decision: a plan entry is core-internal; attrs are a presentation concern of listings). Replace the TODO comment with the decision; no `TODO` left without an issue link.

- [ ] **Step 6: Run** — `cargo nextest run -p norte-core` → PASS. `cargo clippy -p norte-core --all-targets -- -D warnings`.

- [ ] **Step 7: Commit** — `feat(core): daemon validates, forwards and caps provider attrs (#108 block 2)`

---

### Task 9: Backend + CLI `ls --attrs` + smoke + debt issues + changelog

**Files:**
- Modify: `crates/norte-core/src/backend.rs`
- Modify: `crates/norte-cli/src/main.rs` (`Cmd::Ls` :45-53, `ls` :1558)
- Test: `crates/norte-cli/tests/smoke.rs`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Failing smoke test** (`smoke.rs`, next to `ls_json_lists_entries`):

```rust
#[cfg(unix)]
#[test]
fn ls_attrs_posix_via_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), b"hola").expect("seed");
    let out = bin()
        .args(["ls", "--json", "--attrs", "posix.mode"])
        .arg(dir.path())
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("json válido");
    let attrs = &v[0]["attrs"];
    assert!(attrs["posix.mode"].is_object() || attrs["posix.mode"].is_number(),
        "posix.mode presente: {attrs}");
}
```

(Pin the exact wire shape — `{"uint": N}` per the block-1 goldens — once observed; the assertion above is the failing skeleton.)

- [ ] **Step 2: Run to verify failure** — `cargo nextest run -p norte-cli ls_attrs` → FAIL (`--attrs` unknown flag).

- [ ] **Step 3: Backend plumbing** (`backend.rs`):

- `pub async fn list_stream_with(&self, dir: &VPath, attrs: &[String]) -> Result<(EntryStream, Option<u64>), Error>` — embedded arm: `engine.list_with(dir, &ListOptions { attrs: AttrRequest::sanitized(attrs.to_vec()) })` (plus the existing skipped call); remote arm: thread `attrs.to_vec()` into `FsListParams` (:1774-1780) **and** into `PageState` so every `page_step` (:1327-1334) re-sends them (a reconnect mid-pagination re-lists with the same request).
- `list_stream` delegates: `self.list_stream_with(dir, &[])`.
- `pub async fn list_with_skipped_attrs(&self, dir: &VPath, attrs: &[String])` — same relationship to `list_with_skipped`.
- `pub async fn stat_attrs(&self, path: &VPath, attrs: &[String]) -> Result<Entry, Error>` — embedded: `engine.stat_with`; remote: `FsStatParams { attrs: attrs.to_vec(), .. }` (:1811-1814).
- `pub async fn attr_catalog(&self, path: &VPath) -> Result<norte_proto::AttrCatalog, Error>` — embedded: `engine.attr_catalog` (the ADR 0039 §4 embedded mandate); remote: stop discarding `r.attrs` in the internal `capabilities` (:1797-1805) — split it into `capabilities_full` returning the whole `FsCapabilitiesResult`, with `capabilities`/`attr_catalog` façades over it.

Embedded-path unit test (backend tests, MemProvider engine):

```rust
#[tokio::test]
async fn backend_embebido_sanea_catalogo_y_pide_attrs() {
    let backend = backend_mem_con(MemProvider::new().with_synthetic_attrs());
    let cat = backend.attr_catalog(&raiz).await.expect("catálogo");
    assert!(cat.iter().any(|a| a.id == "mem.owner"));
    let e = backend
        .stat_attrs(&archivo, &["mem.owner".to_owned()])
        .await
        .expect("stat_attrs");
    assert!(matches!(e.attrs.get("mem.owner"), Some(AttrValue::Bytes(_))));
}
```

- [ ] **Step 4: CLI** (`main.rs`):

```rust
    /// Per-entry provider attributes to request (repeatable). See
    /// `fs.capabilities` for what the target provider advertises.
    #[arg(long = "attrs", value_name = "ID")]
    attrs: Vec<String>,
```

`ls()` takes `attrs: &[String]`, calls `backend.list_with_skipped_attrs(&target, attrs)`; the `--json` path needs nothing else (`Entry` serialises `attrs`). Human path: after the existing name cell, append requested attrs present on the entry as ` id=value`:

```rust
fn render_attr_valor(v: &norte_proto::AttrValue) -> String {
    use norte_proto::AttrValue as V;
    match v {
        V::Uint(n) => n.to_string(),
        V::Int(n) | V::TimeMs(n) => n.to_string(),
        V::Bool(b) => b.to_string(),
        // Texto/bytes son de terceros: escape_debug neutraliza controles,
        // RTL e invisibles; bytes pasan por lossy ANTES (regla 1: jamás
        // asumir UTF-8, la pérdida es explícita y solo de presentación).
        V::Text(s) => s.escape_debug().to_string(),
        V::Bytes(b) => String::from_utf8_lossy(b).escape_debug().to_string(),
        V::Unknown => "?".to_owned(),
    }
}
```

The hydration loop (:1585-1592) stays untouched (it re-stats size/mtime lazies; attrs ride the listing).

- [ ] **Step 5: Run** — `cargo nextest run -p norte-cli -p norte-core` → PASS. Note the recurring-trap: smoke tests run the **built** binary — `cargo build -p norte-cli` first if nextest surprises with a stale `norte`.

- [ ] **Step 6: Debt issues + changelog.**

```bash
gh issue create --title "sftp: owner/group names as attrs need raw SSH_FXP_NAME longname" \
  --body "Block 2 (#108) shipped posix.mode/uid/gid only. russh-sftp 2.3 hardcodes user/group to None on decode (v3 wire has only uid/gid) and drops the longname before the client API. Adjacent to #37 (raw name bytes). Options: upstream PR keeping longname, or vendored readdir parse. Spec table row: sftp.owner/sftp.group as Bytes."
gh issue create --title "object: s3.storage_class attr blocked on opendal" \
  --body "Block 2 (#108) shipped s3.etag + s3.content_type. opendal 0.58 discards <StorageClass> in ListObjectsOutputContent and Metadata has no field for it. Needs upstream opendal support (or raw S3 path). Also: s3.content_type is stat-only (absent on list pages) — HeadObject per entry would be needed; revisit only if a real UI need appears."
```

`CHANGELOG.md`: under Unreleased — `feat: providers advertise and produce per-entry attributes (posix.*, s3.etag/content_type, zip method/packed/crc32); daemon validates and forwards them; CLI ls --attrs (#108 block 2)`.

- [ ] **Step 7: Commit** — `feat(core,cli): attrs through Backend + ls --attrs (#108 block 2)`

---

### Task 10: reviews + full gate + close-out

- [ ] **Step 1: Reviews (parallel):**
  - **protocol-guardian** — daemon handler changes (`fs.list`/`fs.stat`/`fs.capabilities` semantics: `-32602` cases, advertised-only forwarding, continuation ignoring re-sent attrs, emit caps). No wire-shape change, no version bump — confirm that judgment.
  - **rust-reviewer** — full diff vs CLAUDE.md hard rules (blocking I/O placement in the local promotion path, no unwrap outside tests, typed errors, defaulted-trait recursion hazards).
  - **encoding-auditor** — attrs are third-party bytes end-to-end: MemProvider hostile values, CLI `escape_debug` path, `validate_attr_request` error message, zip method names, non-UTF-8 owner Bytes round-trip.
- [ ] **Step 2: Apply findings** (fix + targeted re-test per finding; commit as `fix(...)` referencing the block).
- [ ] **Step 3: Full gate** — `just ci` (touches proto-adjacent core + vfs → coverage gate applies). Expected EXIT=0, coverage ≥ 85%. Remember `cargo llvm-cov clean` if numbers look stale.
- [ ] **Step 4: Close-out** — comment on #108 (block 2 landed: what shipped, scope decisions, debt issue numbers, deferred 7c note), update memory (`proyecto-norte-estado`).
