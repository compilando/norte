//! Scalar merge of `norte.toml` across layers (ADR 0007/0035) — the
//! [`CommonConfig`] every frontend loads at startup — plus the persistence
//! helpers that write back user preferences (theme, hotlist) with
//! `toml_edit` so comments and formatting survive.
//!
//! This module deliberately reads configuration with `std::fs`; using
//! providers would be circular because configuration selects how a frontend
//! starts.

use std::path::{Path, PathBuf};

use norte_proto::VPath;

use crate::dirs::{Layer, Layers};
use crate::schema::{self, ConfigError, NorteToml, toml_diag};

/// The scalar config layer: the file every `persist_*` helper below writes.
const NORTE_TOML: &str = "norte.toml";

/// The keymap layer (ADR 0006/0007) — a SECOND writable config file since
/// K3c ([`persist_keymap_append`]/[`persist_keymap_remove`]). It has its own
/// lock and its own tmp sibling; see [`ConfigFileLock`] for why sharing
/// `norte.toml`'s would be a bug rather than a saving.
const KEYMAP_TOML: &str = "keymap.toml";

/// Sets `[ui].theme = name` in the user's `norte.toml`, PRESERVING comments
/// and formatting (`toml_edit`). Creates the file/directory if they do not
/// exist. Returns the written path.
///
/// # Errors
/// [`std::io::Error`] if there is no user dir, the existing TOML does not
/// parse, or the I/O fails.
pub fn persist_ui_theme(name: &str) -> std::io::Result<PathBuf> {
    let dir = crate::dirs::user_config_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no user config directory")
    })?;
    persist_ui_theme_to(&dir, name)
}

/// Like [`persist_ui_theme`] but into an explicit `dir` (not depending on the
/// environment — the testable base). A thin wrapper over [`persist_set`]
/// (S2): it keeps its own signature (`name: &str`, not `toml_edit::Value`)
/// because it is the historical entry point, but the write is done entirely
/// by `persist_set(dir, "ui", "theme", …)` — behavior proven by the same
/// `#[test]` as before (`hotlist_tests`), unchanged.
///
/// # Errors
/// [`std::io::Error`] if the existing TOML does not parse or the I/O fails.
pub fn persist_ui_theme_to(dir: &std::path::Path, name: &str) -> std::io::Result<PathBuf> {
    persist_set(dir, "ui", "theme", toml_edit::Value::from(name))
}

/// Sets `[section] key = value` in the `norte.toml` of `dir`, PRESERVING
/// comments and formatting (`toml_edit`) — the GENERIC form behind
/// [`persist_ui_theme_to`] (S2, the EXACT same pattern: reads-or-creates a
/// `DocumentMut`, the section table is born EXPLICIT — never implicit, so a
/// new file reads clean — sets `key`, writes). Creates the dir/file if they
/// do not exist. `value` already comes typed by the caller
/// (`toml_edit::Value`): a string escapes itself (the same mechanism
/// `toml_edit::value(name)` used to use here), a bool/int are written
/// natively.
///
/// Shape CONTRACT (review S, I1): every `[section]` of `norte.toml` must be a
/// flat table (a real `[section]` or an inline `section = { .. }`) — NEVER a
/// scalar (`ui = 3`) nor an array-of-tables (`[[ui]]`). `load` only WARNS if
/// a section has an unexpected shape (degrades that section, keeps
/// starting); this writer, on the other hand, must explicitly REFUSE a
/// section with scalar shape — indexing a scalar `toml_edit::Item` by key
/// (`item[key] = ..`) creates nothing: `panic!("index not found")` (`toml_edit`'s
/// `IndexMut` implementation for a non-table `Item::Value` returns `None`
/// internally and the index operator `.expect()`s it). Reachable with a
/// config hand-edited between sessions (the TUI only WARNS about it on
/// reload, it does not block it) — a `panic` here would bring down the
/// background thread (GUI: takes the process with it; TUI: a silent
/// `JoinError` after a `spawn_blocking`). Checked with `Item::is_table_like`
/// (the same rule `toml_edit`'s internal `IndexMut` uses to decide whether it
/// can index) — so the guard never refuses a shape the library itself would
/// accept.
///
/// Since #116 the write is SAFE across processes: it takes the
/// `norte.toml.lock` advisory lock BEFORE reading (it may block while another
/// process is persisting — see `lock_config_file`) and replaces the file via
/// tmp + atomic `rename` (`write_config_file`): a concurrent reader never
/// sees a half-written file. Applies to the WHOLE `persist_*` family.
///
/// # Errors
/// [`std::io::Error`] if there is no user dir, the existing TOML does not
/// parse, the existing section is not a table (unexpected shape, see the
/// CONTRACT above), or the I/O fails.
pub fn persist_set(
    dir: &std::path::Path,
    section: &str,
    key: &str,
    value: toml_edit::Value,
) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    std::fs::create_dir_all(dir)?;
    // #116: lock BEFORE reading — the whole RMW is the critical section.
    let lock = lock_config_file(dir, NORTE_TOML)?;
    let path = lock.target().to_path_buf();
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s.parse::<toml_edit::DocumentMut>().map_err(|_| {
            // security review item 4 (C1, NIT F4): `toml_edit`'s parse
            // error `Display` quotes the offending document line — a
            // hostile `name`/`path` persisted earlier (#73 discipline)
            // would otherwise reach whatever shows this `io::Error`
            // (the TUI status bar). Name the file, never the content.
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{} does not parse: invalid TOML; fix it or delete it",
                    path.display()
                ),
            )
        })?,
        Err(e) if e.kind() == ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(e) => return Err(e),
    };
    // Shape guard (review S, I1) — BEFORE touching anything: an existing
    // section that is not a table (scalar, array-of-tables…) would index
    // into a panic further below (see the rustdoc's CONTRACT). `get` creates
    // nothing (unlike `entry`), so this check is read-only.
    if let Some(existing) = doc.as_table().get(section)
        && !existing.is_table_like()
    {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "{}: [{section}] is not a table (unexpected shape); fix it or delete it",
                path.display()
            ),
        ));
    }
    // A freshly created `[section]` table would be IMPLICIT (it would be
    // emitted as `section.key = …` instead of under `[section]`): it is
    // created EXPLICIT so a new file reads with a legible section; an
    // already-existing one is respected.
    let table = doc.as_table_mut().entry(section).or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        t.set_implicit(false);
        toml_edit::Item::Table(t)
    });
    table[key] = toml_edit::Item::Value(value);
    write_config_file(&lock, &doc)?;
    Ok(path)
}

/// What a configuration write did: where, and whether it changed anything.
#[derive(Debug, Clone)]
pub struct ConfigWrite {
    /// The `norte.toml` that was worked on.
    pub path: PathBuf,
    /// `false` = there was nothing to do and the file was not touched.
    pub changed: bool,
}

/// Removes `key` from `[section]` in the `norte.toml` of `dir` — the reverse
/// of [`persist_set`], and what is behind "reset" on the settings screen.
///
/// Removes the key from whichever layer it is given, which is the WRITE one.
/// Careful about what that means to the user: if system, profile or project
/// set the same key, the value changes and **is still not the factory one**.
/// This function does not know that and cannot; whoever calls it compares
/// afterwards and says so.
///
/// A file that is not there, a section that is not there or a key that is
/// not there are a **documented no-op**, not an error: there is nothing to
/// remove. And it creates nothing — no `create_dir_all`, same as
/// [`persist_keymap_unbind`]: a removal that creates a directory is a removal
/// that leaves a trace.
///
/// A `[section]` that ends up empty **is kept**. Deleting it changes the file
/// more than was asked, and an empty table means nothing different from an
/// absent one.
///
/// BLOCKING: synchronous FS I/O — the caller MUST wrap it in `spawn_blocking`
/// (rule 2), same pattern as [`persist_set`].
///
/// # Errors
/// [`std::io::Error`] if the existing TOML does not parse, if `[section]`
/// exists with a non-table shape, or if the I/O fails.
pub fn persist_unset(dir: &Path, section: &str, key: &str) -> std::io::Result<ConfigWrite> {
    use std::io::{Error, ErrorKind};
    let declared = dir.join(NORTE_TOML);
    // With no file there is nothing to remove, and the lock must not create it.
    let lock = match lock_config_file(dir, NORTE_TOML) {
        Ok(l) => l,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Ok(ConfigWrite {
                path: declared,
                changed: false,
            });
        }
        Err(e) => return Err(e),
    };
    let path = lock.target().to_path_buf();
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s.parse::<toml_edit::DocumentMut>().map_err(|_| {
            // Names the file, never the content: `toml_edit`'s error
            // `Display` quotes the offending line, and a hostile name
            // persisted earlier could be there (#73).
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{} does not parse: invalid TOML; fix it or delete it",
                    path.display()
                ),
            )
        })?,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Ok(ConfigWrite {
                path,
                changed: false,
            });
        }
        Err(e) => return Err(e),
    };
    // The same shape guard as `persist_set`, and for the same reason: a
    // `[section]` that is not a table would index into a panic.
    let Some(existing) = doc.as_table().get(section) else {
        return Ok(ConfigWrite {
            path,
            changed: false,
        });
    };
    if !existing.is_table_like() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "{}: [{section}] is not a table (unexpected shape); fix it or delete it",
                path.display()
            ),
        ));
    }
    let removed = doc
        .as_table_mut()
        .get_mut(section)
        .and_then(toml_edit::Item::as_table_like_mut)
        .is_some_and(|t| t.remove(key).is_some());
    if removed {
        write_config_file(&lock, &doc)?;
    }
    Ok(ConfigWrite {
        path,
        changed: removed,
    })
}

/// The `sort` to persist (#108 7a) — a deliberate mirror of [`SortChoice`]:
/// this writer serializes EXACTLY the vocabulary `load` parses in
/// `parse_sort_section` (`column` = `name`|`size`|`mtime`, `dir` =
/// `asc`|`desc`, `dirs_first` bool), pinned by the round-trip test next to
/// it. Takes raw strings/bools (not [`SortChoice`]) because the caller is a
/// frontend that already speaks the columns' Display vocabulary.
#[derive(Debug, Clone, Copy)]
pub struct PersistSort<'a> {
    /// `"name"` | `"size"` | `"mtime"` (load's closed vocabulary).
    pub column: &'a str,
    /// `true` = descending (serialized as `dir = "desc"`).
    pub descending: bool,
    /// Directories first.
    pub dirs_first: bool,
}

/// Writes the column picker's selection (#108 7a) into the `norte.toml` of
/// `dir`, PRESERVING comments and formatting (`toml_edit`, same pattern as
/// [`persist_set`]): `scheme = None` sets `[ui.columns]`'s `default` +
/// `sort`; `Some(s)` sets `[ui.columns.scheme.<s>]`'s `columns` + `sort`.
/// The persister's first NESTED write — `persist_set` only knows about
/// `[section] key = scalar` — and first ARRAY value: the intermediate tables
/// are born implicit (they do not emit empty headers), the leaf is born
/// EXPLICIT (a readable `[ui.columns]`, same rule as `persist_set`).
///
/// Shape CONTRACT: [`persist_set`]'s `is_table_like` guard is applied level
/// by level BEFORE mutating anything — an indexed scalar level (`ui = 3`)
/// would panic (see `persist_set`'s CONTRACT) and bring down the caller's
/// background thread. BLOCKING: synchronous FS I/O — the caller MUST wrap it
/// in `spawn_blocking` (rule 2), same pattern as `persist_hotlist_add`.
///
/// CONTRACT: `scheme` must be a VALIDATED `VPath` scheme
/// (`[a-z][a-z0-9+.-]*`), as the current callers deliver it
/// (`VPath::scheme()`). `toml_edit` escapes the key regardless — there is no
/// injection — but an arbitrary string would travel in the shape guard's
/// error `Display`, turning it into a carrier of hostile content (#73).
///
/// # Errors
/// [`std::io::Error`] if the existing TOML does not parse, an existing level
/// of the chain is not a table (unexpected shape), or the I/O fails.
///
/// `sort = None` does NOT touch whatever `sort` key is there: it is what is
/// passed when the chosen order has no shape in this file — an order by
/// ATTRIBUTE (ADR 0144) is valid for the session, and `[ui] sort` only names
/// built-ins. Writing `name` in its place would silently overwrite what the
/// reader had saved.
pub fn persist_columns(
    dir: &std::path::Path,
    scheme: Option<&str>,
    ids: &[String],
    sort: Option<PersistSort<'_>>,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    // #116: lock BEFORE reading — the whole RMW is the critical section.
    let lock = lock_config_file(dir, NORTE_TOML)?;
    let path = lock.target().to_path_buf();
    let mut doc = open_config_toml(&path)?;
    let segs: Vec<&str> = match scheme {
        None => vec!["ui", "columns"],
        Some(s) => vec!["ui", "columns", "scheme", s],
    };
    let t = nested_table_mut(&mut doc, &path, &segs)?;
    let mut arr = toml_edit::Array::new();
    for id in ids {
        // Ids AS IS (an open set — `attr:`/`plugin:`/unparseable): `toml_edit`
        // escapes, it never injects TOML (pinned in `persist_set`).
        arr.push(id.as_str());
    }
    // The list's key differs by schema design (#108 block 4): `default`
    // under `[ui.columns]`, `columns` in a scheme override.
    let list_key = if scheme.is_none() {
        "default"
    } else {
        "columns"
    };
    t.insert(
        list_key,
        toml_edit::Item::Value(toml_edit::Value::Array(arr)),
    );
    if let Some(sort) = sort {
        let mut sort_tbl = toml_edit::InlineTable::new();
        sort_tbl.insert("column", sort.column.into());
        sort_tbl.insert("dir", if sort.descending { "desc" } else { "asc" }.into());
        sort_tbl.insert("dirs_first", sort.dirs_first.into());
        t.insert(
            "sort",
            toml_edit::Item::Value(toml_edit::Value::InlineTable(sort_tbl)),
        );
    }
    write_config_file(&lock, &doc)?;
    Ok(path)
}

/// Cross-process advisory lock for ONE config file (#116): `flock`/
/// `LockFileEx` on the DEDICATED sibling `<file>.lock` — never on the file
/// itself: the atomic write replaces it via `rename` (a new inode) and a
/// lock on the old inode would not exclude the next writer. Writers take it
/// BEFORE reading: the critical section is the WHOLE read-modify-write cycle
/// (closes the lost update, across processes too — GUI + TUI over the same
/// file). Released on dropping the guard (closing the descriptor); the OS
/// releases it just the same if the process dies — no stale locks after a
/// crash.
///
/// K3c: there are TWO writable files (`norte.toml` and `keymap.toml`) and
/// each has its OWN lock — sharing one would serialize two files that do not
/// touch each other and, much worse, would let a future writer replace a
/// file while holding the OTHER's lock, believing itself protected. That
/// being impossible is structural, not a convention: the guard carries its
/// [`ConfigFileLock::target`] and [`write_config_file`] takes the path it
/// writes from there, so the only file a writer can name is the one it
/// locked.
struct ConfigFileLock {
    /// Keeps the locked descriptor alive; drop = close = unlock.
    _file: std::fs::File,
    /// The config file this lock protects (`dir/<file>`), NOT the sibling
    /// `.lock`.
    target: PathBuf,
}

impl ConfigFileLock {
    /// The protected config file — the ONLY one its holder may write (see
    /// the type's doc).
    fn target(&self) -> &Path {
        &self.target
    }
}

/// Takes (blocking) the writers' lock for `file` inside `dir`. BLOCKING like
/// the rest of the persister (rule 2: the caller already wraps it in
/// `spawn_blocking`); writers are short — holding the lock for milliseconds
/// — and there are no nested locks, so the wait has no bound: a peer that is
/// ALIVE but hung while holding it is the only pathological case (decision:
/// block plainly; the OS releases it when the process dies).
///
/// The open is WITHOUT truncating (review #116 MAJOR-1): `File::create`
/// (`CREATE_ALWAYS`) on a lockfile another process holds under `LockFileEx`
/// can FAIL on Windows (sharing/lock violation) instead of reaching the
/// `lock()` call that waits — exactly the GUI+TUI contention this lock
/// closes. On POSIX truncating an empty file was harmless, but the canonical
/// way to open a lockfile is to never touch it.
///
/// `file` is ALWAYS a constant of this module ([`NORTE_TOML`],
/// [`KEYMAP_TOML`]) — never a name coming from the user.
fn lock_config_file(dir: &Path, file: &str) -> std::io::Result<ConfigFileLock> {
    let handle = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(format!("{file}.lock")))?;
    handle.lock()?;
    Ok(ConfigFileLock {
        _file: handle,
        target: dir.join(file),
    })
}

/// ATOMIC write of the file `lock` protects (#116): sibling tmp + `rename`
/// (atomic on POSIX; `std::fs::rename` also replaces on Windows). A
/// concurrent reader sees the old file or the COMPLETE new one — never a
/// half-truncated one that parses "fine" and from which a later writer
/// reconstructs the document losing sections it did not own. `sync_all`
/// before the rename avoids the empty-file-after-crash window; the rename
/// itself can be lost in a power cut (deliberately no dir fsync): the OLD
/// config reappears — consistent, just stale. An orphaned tmp from a crash
/// is harmless: the next write (same name, under the lock) overwrites it.
/// The existing file's permissions are COPIED to the tmp (review #116
/// MINOR-2: without this, a user's `chmod 600` widened to the umask on
/// replacement). Known Windows limitation: an external process (editor, AV)
/// with the file open without `FILE_SHARE_DELETE` makes the rename fail with
/// a sharing violation — the persist fails visibly, with no retry (Rust
/// std's readers share in full mode).
///
/// K3c: the path comes from `lock` (not from the caller) and the tmp is
/// DERIVED from that path's name. Hardcoding `norte.toml.tmp` was harmless
/// with a single writable file; with two, two writers of different files
/// would compete for the same tmp and the rename would land with the
/// other's content. The name is composed in `OsString` (rule 1: names are
/// bytes, UTF-8 is never assumed).
fn write_config_file(lock: &ConfigFileLock, doc: &toml_edit::DocumentMut) -> std::io::Result<()> {
    use std::io::Write;
    let path = lock.target();
    let mut tmp_name = path
        .file_name()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "config path with no file name",
            )
        })?
        .to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(doc.to_string().as_bytes())?;
        match std::fs::metadata(path) {
            Ok(meta) => f.set_permissions(meta.permissions())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Reads (or creates, if absent) the config file at `path` as a `toml_edit`
/// document, with the parse error SANITIZED — `toml_edit`'s `Display` quotes
/// the offending line, and a hostile `name`/`path` persisted earlier would
/// reach whoever shows this `io::Error` (the status bar, #73): the file is
/// named, never the content. Shared by the column writers
/// (`persist_columns`/`persist_column_format`) and the keymap ones
/// (`persist_keymap_append`/`persist_keymap_remove`).
fn open_config_toml(path: &std::path::Path) -> std::io::Result<toml_edit::DocumentMut> {
    use std::io::{Error, ErrorKind};
    match std::fs::read_to_string(path) {
        Ok(s) => s.parse::<toml_edit::DocumentMut>().map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{} does not parse: invalid TOML; fix it or delete it",
                    path.display()
                ),
            )
        }),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(toml_edit::DocumentMut::new()),
        Err(e) => Err(e),
    }
}

/// Walks/creates the `segs` chain of tables under `doc` (#108 7a/7b — TWO
/// callers: [`persist_columns`] and [`persist_column_format`]) and returns
/// the mutable leaf.
///
/// Level-by-level shape guard, READ-ONLY and BEFORE touching anything (same
/// `is_table_like` rule as `persist_set`): a level that cuts off (does not
/// exist) makes creating everything below it safe. The mutation walks with
/// `entry` over `TableLike` — NOT with the index operator, whose `IndexMut`
/// in this `toml_edit` materializes missing levels as INLINE tables with
/// dotted-keys (`ui = { columns.default = … }`), unreadable for a
/// hand-editable file. Each new level is born `Item::Table`: intermediates
/// IMPLICIT (with no header of their own), the leaf EXPLICIT (a readable
/// `[ui.columns]`, same rule as `persist_set`); an already-existing
/// `Item::Table` leaf is forced explicit; an inline one
/// (`ui = { columns = {…} }`) is respected as is.
fn nested_table_mut<'d>(
    doc: &'d mut toml_edit::DocumentMut,
    path: &std::path::Path,
    segs: &[&str],
) -> std::io::Result<&'d mut dyn toml_edit::TableLike> {
    use std::io::{Error, ErrorKind};
    let shape = |up_to: usize| {
        Error::new(
            ErrorKind::InvalidData,
            format!(
                "{}: [{}] is not a table (unexpected shape); fix it or delete it",
                path.display(),
                segs[..=up_to].join(".")
            ),
        )
    };
    {
        let mut level: &dyn toml_edit::TableLike = doc.as_table();
        for (i, seg) in segs.iter().enumerate() {
            let Some(item) = level.get(seg) else { break };
            if !item.is_table_like() {
                return Err(shape(i));
            }
            let Some(t) = item.as_table_like() else {
                // Unreachable: `is_table_like` just passed (it is literally
                // `as_table_like().is_some()`).
                break;
            };
            level = t;
        }
    }
    // Safe after the guard: there is no longer any existing non-table level.
    let mut t: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for (i, seg) in segs.iter().enumerate() {
        let is_leaf = i + 1 == segs.len();
        let item = t.entry(seg).or_insert_with(|| {
            let mut nt = toml_edit::Table::new();
            nt.set_implicit(!is_leaf);
            toml_edit::Item::Table(nt)
        });
        if is_leaf && let Some(tab) = item.as_table_mut() {
            tab.set_implicit(false);
        }
        // Unreachable `Err`: the guard already refused every existing
        // non-table level and new ones are born `Item::Table` — but a clean
        // `Err` beats an `unwrap` (rule 6).
        t = item.as_table_like_mut().ok_or_else(|| shape(i))?;
    }
    Ok(t)
}

/// Sets (or creates) the `format` of the `[[ui.columns.spec]]` for id `id`
/// (#108 7b): a BY-ID replacement that PRESERVES the entry's other fields
/// (`header`/`width`/`align`) and the file's comments — [`persist_hotlist_add`]'s
/// `ArrayOfTables` precedent, including its shape guard (an existing `spec`
/// that is not an array of tables = a clean error, never a panic).
///
/// CONTRACT: `id` comes from the picker's vocabulary (builtins' Display ids)
/// and `format` from the CLOSED vocabulary of formats already validated
/// against its column; `toml_edit` escapes it regardless. BLOCKING:
/// synchronous FS I/O — the caller MUST wrap it in `spawn_blocking` (rule 2).
///
/// # Errors
/// [`std::io::Error`] if the existing TOML does not parse, a level of
/// `[ui.columns]` is not a table, `spec` exists with a different shape, or
/// the I/O fails.
pub fn persist_column_format(dir: &Path, id: &str, format: &str) -> std::io::Result<PathBuf> {
    persist_column_spec_field(dir, id, "format", toml_edit::value(format))
}

/// Persists a column's WIDTH as `width = <cells>` in its
/// `[[ui.columns.spec]]` (spec 2026-09-11, V2: dragging a header's border in
/// the window). Same mechanics and same guards as [`persist_column_format`]:
/// replacement by id, the other fields and comments survive, every
/// duplicate entry is updated.
///
/// CONTRACT: `cells` is already in `[1, 64]` — what the loader accepts — or
/// the next `load` will reject it entirely; the caller (the host) bounds it
/// beforehand. BLOCKING: synchronous FS I/O — wrap in `spawn_blocking` (rule
/// 2).
///
/// # Errors
/// The same as [`persist_column_format`].
pub fn persist_column_width(dir: &Path, id: &str, cells: u16) -> std::io::Result<PathBuf> {
    // The shape the loader reads for a fixed width: `width = { fixed = N }`
    // (`WidthSection::Fixed`); a bare integer is not any variant.
    let mut fixed = toml_edit::InlineTable::new();
    fixed.insert("fixed", toml_edit::Value::from(i64::from(cells)));
    persist_column_spec_field(dir, id, "width", toml_edit::value(fixed))
}

/// The common writer for ONE field of a `[[ui.columns.spec]]` by id.
fn persist_column_spec_field(
    dir: &Path,
    id: &str,
    key: &str,
    value: toml_edit::Item,
) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    std::fs::create_dir_all(dir)?;
    // #116: lock BEFORE reading — the whole RMW is the critical section.
    let lock = lock_config_file(dir, NORTE_TOML)?;
    let path = lock.target().to_path_buf();
    let mut doc = open_config_toml(&path)?;
    let t = nested_table_mut(&mut doc, &path, &["ui", "columns"])?;
    let arr = t
        .entry("spec")
        .or_insert_with(|| toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{}: `spec` is not an array of tables (unexpected shape); fix it or delete it",
                    path.display()
                ),
            )
        })?;
    // EVERY entry for the id, not just the first (MAJOR review 7b): the
    // loader merges intra-layer duplicates LAST-wins per field — writing
    // only the first would leave the session and the file diverging after a
    // reload (the later duplicate overrides what was just saved, with the
    // toast already shown). Updating all of them self-heals the divergence.
    let mut any = false;
    for tb in arr
        .iter_mut()
        .filter(|tb| tb.get("id").and_then(|v| v.as_str()) == Some(id))
    {
        tb[key] = value.clone();
        any = true;
    }
    if !any {
        let mut tb = toml_edit::Table::new();
        tb["id"] = toml_edit::value(id);
        tb[key] = value;
        arr.push(tb);
    }
    write_config_file(&lock, &doc)?;
    Ok(path)
}

/// Adds (or replaces if `name` already exists) a `[[hotlist]]` entry in the
/// `norte.toml` of `dir`, PRESERVING comments and formatting (same
/// `toml_edit` pattern as [`persist_ui_theme_to`]). `wire_path` is saved AS
/// IS — validation to [`VPath`] happens on reread (`load`), not here:
/// persisting must not refuse a path `norte` itself does not yet know how to
/// interpret (e.g. a future provider's new scheme).
///
/// BLOCKING: does synchronous FS I/O. The caller (T5) MUST wrap it in
/// `tokio::task::spawn_blocking` — the runtime is never blocked (rule 2),
/// same pattern as `persist_ui_theme` in `main.rs`.
///
/// # Errors
/// [`std::io::Error`] if the existing TOML does not parse, `hotlist` exists
/// but is not an array of tables, or the I/O fails.
pub fn persist_hotlist_add(dir: &Path, name: &str, wire_path: &str) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    std::fs::create_dir_all(dir)?;
    // #116: lock BEFORE reading — the whole RMW is the critical section.
    let lock = lock_config_file(dir, NORTE_TOML)?;
    let path = lock.target().to_path_buf();
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s.parse::<toml_edit::DocumentMut>().map_err(|_| {
            // security review item 4 (C1, NIT F4): `toml_edit`'s parse
            // error `Display` quotes the offending document line — a
            // hostile `name`/`path` persisted earlier (#73 discipline)
            // would otherwise reach whatever shows this `io::Error`
            // (the TUI status bar). Name the file, never the content.
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{} does not parse: invalid TOML; fix it or delete it",
                    path.display()
                ),
            )
        })?,
        Err(e) if e.kind() == ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(e) => return Err(e),
    };
    let arr = doc
        .as_table_mut()
        .entry("hotlist")
        .or_insert_with(|| toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "`hotlist` is not an array of tables",
            )
        })?;
    // add REPLACES if the name already exists (spec: "add replaces if name
    // exists") — same behavior as renaming/updating the favorite without
    // leaving an orphaned old entry.
    if let Some(existing) = arr
        .iter_mut()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
    {
        existing["path"] = toml_edit::value(wire_path);
    } else {
        let mut t = toml_edit::Table::new();
        t["name"] = toml_edit::value(name);
        t["path"] = toml_edit::value(wire_path);
        arr.push(t);
    }
    write_config_file(&lock, &doc)?;
    Ok(path)
}

/// Removes the `[[hotlist]]` entry named `name` from the `norte.toml` of
/// `dir`, PRESERVING comments and formatting. A nonexistent `name` (or a
/// missing `norte.toml`/`hotlist`) is a documented NO-OP: there is nothing to
/// delete, it is not an error — and crucially it does NOT rewrite the file
/// (review MINOR-1: writing with no changes touches the mtime → `config::watch`'s
/// watcher confuses it with a real edit and triggers a phantom hot-reload).
///
/// BLOCKING: does synchronous FS I/O. The caller (T5) MUST wrap it in
/// `tokio::task::spawn_blocking` — the runtime is never blocked (rule 2),
/// same pattern as `persist_ui_theme` in `main.rs`.
///
/// # Errors
/// [`std::io::Error`] if the existing TOML does not parse or the I/O fails.
pub fn persist_hotlist_remove(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    // #116: lock BEFORE reading. This writer creates no dir (it only
    // deletes): a nonexistent dir = nothing to delete = the same documented
    // no-op as the absent `norte.toml` below — and that no-op needs the path
    // BEFORE the guard exists, the only reason it is composed by hand here
    // (the rest of the family takes it from `lock.target()`, which cannot
    // diverge from the locked file).
    let lock = match lock_config_file(dir, NORTE_TOML) {
        Ok(l) => l,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(dir.join(NORTE_TOML)),
        Err(e) => return Err(e),
    };
    let path = lock.target().to_path_buf();
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s.parse::<toml_edit::DocumentMut>().map_err(|_| {
            // security review item 4 (C1, NIT F4): `toml_edit`'s parse
            // error `Display` quotes the offending document line — a
            // hostile `name`/`path` persisted earlier (#73 discipline)
            // would otherwise reach whatever shows this `io::Error`
            // (the TUI status bar). Name the file, never the content.
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{} does not parse: invalid TOML; fix it or delete it",
                    path.display()
                ),
            )
        })?,
        // Documented no-op: nothing to delete.
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(path),
        Err(e) => return Err(e),
    };
    // Only writes if `retain` REALLY removed something — compare lengths
    // before/after instead of writing unconditionally.
    if let Some(arr) = doc
        .as_table_mut()
        .get_mut("hotlist")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
    {
        let before = arr.len();
        arr.retain(|t| t.get("name").and_then(|v| v.as_str()) != Some(name));
        if arr.len() != before {
            write_config_file(&lock, &doc)?;
        }
    }
    Ok(path)
}

/// The keymap CONTEXTS a binding can be written into (ADR 0006): the four
/// sections of `keymap.toml`, and a CLOSED vocabulary. Mirror of
/// `KeymapFile`'s fields in `norte-frontend` (`keymap/layer.rs`), which is
/// `deny_unknown_fields`: a section this writer invented would not be
/// ignored, it would make the WHOLE layer fail to parse — and a layer that
/// fails to parse reverts the user's entire keymap on the next reload, with
/// the editor having reported success. `norte-config` cannot call that parser
/// (the dependency runs frontend → config, never back), so the vocabulary is
/// mirrored here and pinned from the other side by
/// `norte_frontend::config::tests::un_binding_persistido_carga_y_resuelve`.
const KEYMAP_SECTIONS: [&str; 4] = ["global", "pane", "viewer", "dialog"];

/// Which of a user layer's two binding lists a write goes into (ADR 0006).
///
/// NOT interchangeable, and choosing the wrong one fails SILENTLY. The merge
/// order is: every layer's `prepend_keymap`, then the preset's `keymap`, then
/// every layer's `append_keymap` (`merge_ctx`, `norte-frontend`
/// `keymap/layer.rs`), and the FIRST binding of a sequence wins
/// (`Effective::build_for`). So a chord the preset already binds IN THE SAME
/// context is overridden only by a [`KeymapList::Prepend`]: an append for it
/// parses, loads, validates — and never fires, while the editor reports
/// success. ADR 0006 states the rule for what an append DOES win: "a user
/// append in `pane` overrides a preset binding in `global`" — across
/// contexts, not within one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeymapList {
    /// Wins over the preset: what a REBIND has to use.
    Prepend,
    /// Loses to the preset: for a chord the preset leaves free, where the
    /// binding says "also this" rather than "mine instead".
    Append,
}

impl KeymapList {
    /// The TOML key of this list — the only two a user layer may declare
    /// (`check_layer_keys` refuses the preset's `keymap` in a layer).
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Prepend => "prepend_keymap",
            Self::Append => "append_keymap",
        }
    }

    /// Both lists, in merge order — what [`persist_keymap_unbind`] walks.
    const BOTH: [Self; 2] = [Self::Prepend, Self::Append];
}

/// What a `keymap.toml` write did (K3c).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapWrite {
    /// The `keymap.toml` that was written, or would have been.
    pub path: PathBuf,
    /// Whether the file's bytes actually changed. `false` means NOTHING was
    /// written: a bind whose chord already ran that command, or an unbind that
    /// matched nothing. The file keeps its bytes AND its mtime, so the config
    /// watcher does not see a phantom edit and reload for nothing (the rule
    /// [`persist_hotlist_remove`] already documents), and a double confirm in
    /// the editor cannot write the same binding twice.
    pub changed: bool,
}

/// Refuses a binding whose section/chords/command could not make a legal
/// `keymap.toml` entry, BEFORE anything is opened or locked.
///
/// The diagnostics never quote `section`, a chord or `command` (#73): all
/// three are caller data, and this error travels to a status bar.
///
/// CONTRACT — read this before wiring a UI onto these writers. What is
/// rejected here is only what is invalid under ANY chord grammar: an empty
/// sequence, an empty token, an empty command. The chord grammar itself
/// (`keymap::parse_chord`), the command catalogue, ADR 0006's prefix-free
/// rule, the sacred keys and the digit-under-`counts` rule ALL live in
/// `norte-frontend`, which is ABOVE this crate — `norte-config` cannot call
/// them, and a binding that breaks any of them makes `Effective::build_for`
/// fail after the file has loaded perfectly. The caller must run
/// `rebind_check` (K3c c2) against the merged map first; these functions
/// guard the FILE, not the keymap.
fn check_keymap_binding(section: &str, chords: &[String], command: &str) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    if !KEYMAP_SECTIONS.contains(&section) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "unknown keymap section; the contexts are: {}",
                KEYMAP_SECTIONS.join(", ")
            ),
        ));
    }
    if chords.is_empty() || chords.iter().any(String::is_empty) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "a binding needs at least one non-empty chord",
        ));
    }
    if command.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "a binding needs a command to run",
        ));
    }
    Ok(())
}

/// The bindings of a binding list, in EITHER shape TOML (and therefore serde)
/// admits: the inline-table array the bundled presets use
/// (`append_keymap = [ { on = […], run = "…" } ]`) and the array of tables a
/// hand-written file may well use (`[[pane.append_keymap]]`). A writer that
/// only knew the first would refuse a file it should extend — or, worse, miss
/// the binding that is already there and write a duplicate.
///
/// `None` = not a list of bindings at all (a scalar, or an array with a
/// non-table element): the caller turns that into a clean shape error instead
/// of writing into something the loader will reject.
fn binding_list(item: &toml_edit::Item) -> Option<Vec<&dyn toml_edit::TableLike>> {
    if let Some(arr) = item.as_array() {
        let mut out: Vec<&dyn toml_edit::TableLike> = Vec::with_capacity(arr.len());
        for v in arr {
            out.push(v.as_inline_table()?);
        }
        return Some(out);
    }
    if let Some(aot) = item.as_array_of_tables() {
        return Some(aot.iter().map(|t| t as &dyn toml_edit::TableLike).collect());
    }
    None
}

/// Is this entry's `on` EXACTLY `chords`? Byte-exact, with no normalisation of
/// any kind: a chord token is a wire vocabulary and two tokens that differ by
/// a byte are two chords — a rebind must never silently replace an entry the
/// user wrote differently (NFC/NFD twins included, the same rule the hotlist
/// keys follow).
fn chord_seq_is(t: &dyn toml_edit::TableLike, chords: &[String]) -> bool {
    t.get("on")
        .and_then(toml_edit::Item::as_array)
        .is_some_and(|on| {
            on.len() == chords.len()
                && on
                    .iter()
                    .zip(chords)
                    .all(|(v, c)| v.as_str() == Some(c.as_str()))
        })
}

/// Does this entry bind `chords` to `command`? Both fields, byte-exact (see
/// [`chord_seq_is`]).
fn binding_is(t: &dyn toml_edit::TableLike, chords: &[String], command: &str) -> bool {
    t.get("run").and_then(toml_edit::Item::as_str) == Some(command) && chord_seq_is(t, chords)
}

/// The shape error for a binding list that is not one. `section` and the list
/// key are both from closed vocabularies when this is reached, so the message
/// quotes only our own words, never caller data (#73).
fn bad_binding_list(path: &Path, section: &str, list: KeymapList) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!(
            "{}: [{section}] {} is not a list of bindings (unexpected shape); fix it or delete it",
            path.display(),
            list.key()
        ),
    )
}

/// Refuses a `keymap.toml` that is not a legal USER LAYER before writing into
/// it. `counts = true`, `dialog_from` and a non-empty `keymap` list are
/// PRESET-only, and `check_layer_keys` (`norte-frontend`, `keymap/layer.rs`)
/// makes each of them a LOAD error — which costs the user their whole keymap.
/// This writer can never produce one, but it must not extend a file that
/// already carries one either: the new binding would land in a file that
/// cannot load and the editor would have said "saved".
///
/// Each rule mirrors the loader's EXACTLY, value and all — `counts = false`
/// and `keymap = []` are legal there, so they are legal here. A writer
/// stricter than the loader refuses to save into a file the app itself
/// accepted, and blames the user for it.
///
/// Deliberately NOT a re-implementation of the whole grammar: an unknown key
/// elsewhere in the file also breaks the load (`deny_unknown_fields`), but
/// refusing it here would buy nothing — the file was already broken and this
/// write cannot make it worse — at the price of a second copy of the schema
/// that would rot. What is checked is what this writer is ADJACENT to: the
/// keys that live in, or next to, the sections it writes into.
fn check_user_layer_shape(doc: &toml_edit::DocumentMut, path: &Path) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    let refuse = |key: &str| {
        Error::new(
            ErrorKind::InvalidData,
            format!(
                "{}: `{key}` is a PRESET key, not legal in a user layer (ADR 0006); fix it or delete it",
                path.display()
            ),
        )
    };
    // The loader refuses `if layer.counts` — the VALUE, not the key. An
    // explicit `counts = false` is a legal layer that loads today.
    if doc
        .as_table()
        .get("counts")
        .is_some_and(|it| it.as_bool() != Some(false))
    {
        return Err(refuse("counts"));
    }
    // TOML has no null, so presence means `Some(..)`: the same thing
    // `check_layer_keys` refuses.
    if doc.as_table().contains_key("dialog_from") {
        return Err(refuse("dialog_from"));
    }
    for section in KEYMAP_SECTIONS {
        let Some(full) = doc
            .as_table()
            .get(section)
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|t| t.get("keymap"))
        else {
            continue;
        };
        match binding_list(full) {
            // `check_layer_keys` refuses a NON-EMPTY `keymap` only.
            Some(l) if l.is_empty() => {}
            Some(_) => return Err(refuse("keymap")),
            // Not a list at all: it cannot load either, but calling it a
            // preset key would send the reader to the wrong ADR.
            None => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "{}: [{section}] keymap is not a list of bindings (unexpected shape); fix it or delete it",
                        path.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Binds `chords` to `command` in `[<section>] <list>` of the `keymap.toml` of
/// `dir` (K3c — the shortcut editor's writer), creating the directory, the
/// file, the section and the list as needed, PRESERVING comments and
/// formatting (`toml_edit`) like the rest of the `persist_*` family.
/// `section` is one of `global`/`pane`/`viewer`/`dialog`; `list` decides
/// whether the binding wins over the preset or loses to it — read
/// [`KeymapList`], the difference is silent.
///
/// INSERT-OR-REPLACE, by chord sequence: an entry in that list already bound
/// to `chords` has its `run` REPLACED in place, keeping its position and the
/// comment beside it. Appending instead would leave two entries for one chord
/// with the OLDER one winning (first wins, in file order), so the second
/// rebind of a key would do nothing — the same silent failure as writing into
/// the wrong list. A later duplicate of the same chord is left alone: it was
/// already inert, and deleting it would take the comment TOML attaches to the
/// element after it.
///
/// IDEMPOTENT: if that entry already runs `command`, nothing is written and
/// [`KeymapWrite::changed`] is `false` — see the field's doc for why an
/// identical rewrite would not be equivalent.
///
/// The two lists are the ONLY ones a user layer may declare; a `keymap` list
/// there is a load error, which is also why a file already carrying a
/// preset-only key is refused rather than extended (`check_user_layer_shape`).
/// This does NOT make every write safe to load: see the CONTRACT on
/// `check_keymap_binding` — the chord grammar, the command catalogue and
/// ADR 0006's whole-map rules live above this crate and are the caller's to
/// check with `rebind_check` (K3c c2) BEFORE calling.
///
/// Cross-process safe like the rest of the family, and with its OWN lock:
/// `keymap.toml.lock`, never `norte.toml.lock` — the private `ConfigFileLock`
/// carries its own target, so writing a file whose lock is not held is
/// unrepresentable.
/// BLOCKING: synchronous FS I/O — the caller MUST wrap it in
/// `spawn_blocking` (rule 2), the same as `persist_hotlist_add`.
///
/// # Errors
/// [`std::io::Error`] if `section` is not a keymap context or the binding is
/// empty ([`std::io::ErrorKind::InvalidInput`]); if the existing TOML does
/// not parse, the section is not a table, the list is not a list of bindings,
/// or the file carries a preset-only key
/// ([`std::io::ErrorKind::InvalidData`]); or if the I/O fails.
pub fn persist_keymap_bind(
    dir: &Path,
    section: &str,
    list: KeymapList,
    chords: &[String],
    command: &str,
) -> std::io::Result<KeymapWrite> {
    check_keymap_binding(section, chords, command)?;
    std::fs::create_dir_all(dir)?;
    // #116: lock BEFORE reading — the whole RMW is the critical section.
    let lock = lock_config_file(dir, KEYMAP_TOML)?;
    let path = lock.target().to_path_buf();
    let mut doc = open_config_toml(&path)?;
    check_user_layer_shape(&doc, &path)?;
    // Read-only pass FIRST: it validates the list's shape before anything is
    // mutated, and it decides idempotence before anything is CREATED — a
    // repeated bind must not materialise an empty section on its way to
    // "nothing changed" and touch the mtime for it.
    if let Some(item) = doc
        .as_table()
        .get(section)
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|t| t.get(list.key()))
    {
        let entries = binding_list(item).ok_or_else(|| bad_binding_list(&path, section, list))?;
        if entries
            .iter()
            .find(|t| chord_seq_is(**t, chords))
            .is_some_and(|t| binding_is(*t, chords, command))
        {
            return Ok(KeymapWrite {
                path,
                changed: false,
            });
        }
    }
    let table = nested_table_mut(&mut doc, &path, &[section])?;
    let item = table.entry(list.key()).or_insert_with(|| {
        let mut fresh = toml_edit::Array::new();
        // A list born here reads like the bundled presets: one binding per
        // line, trailing comma, so the next hand edit has nothing to reflow.
        fresh.set_trailing("\n");
        fresh.set_trailing_comma(true);
        toml_edit::Item::Value(toml_edit::Value::Array(fresh))
    });
    bind_in_list(item, chords, command).ok_or_else(|| bad_binding_list(&path, section, list))?;
    write_config_file(&lock, &doc)?;
    Ok(KeymapWrite {
        path,
        changed: true,
    })
}

/// Replaces the `run` of the FIRST entry bound to `chords`, or pushes a new
/// entry, in whichever of the two legal shapes the list already has (see
/// [`binding_list`]). `None` = not a binding list; the caller has already
/// validated that, so it is the belt to that braces (rule 6: a clean `Err`
/// rather than an `unwrap` on "cannot happen").
///
/// Chords and command go in TAL CUAL: `toml_edit` escapes, it never injects
/// TOML (the pin lives in `persist_set`'s hostile round-trip test).
fn bind_in_list(item: &mut toml_edit::Item, chords: &[String], command: &str) -> Option<()> {
    let mut on = toml_edit::Array::new();
    for c in chords {
        on.push(c.as_str());
    }
    if let Some(arr) = item.as_array_mut() {
        for v in arr.iter_mut() {
            let existing = v.as_inline_table_mut()?;
            if chord_seq_is(existing, chords) {
                existing.insert("run", command.into());
                return Some(());
            }
        }
        let mut inline = toml_edit::InlineTable::new();
        inline.insert("on", toml_edit::Value::Array(on));
        inline.insert("run", command.into());
        // The array's `trailing` is everything between the last comma and the
        // `]`, INCLUDING a trailing comment on the last binding. Pushing after
        // it would hand the user's comment to the new binding, so the comment
        // travels as the new element's prefix — it stays on the line of the
        // binding it annotates.
        let carried = arr
            .trailing()
            .as_str()
            .filter(|t| !t.trim().is_empty())
            .map(str::to_owned);
        let prefix = match &carried {
            Some(t) => format!("{t}    "),
            None => "\n    ".to_owned(),
        };
        if carried.is_some() {
            arr.set_trailing("\n");
        }
        // `push_formatted`, not `push`: `push` applies default formatting and
        // would drop the prefix, packing a growing list onto one unreadable
        // line and dropping the carried comment with it.
        arr.push_formatted(toml_edit::Value::InlineTable(inline).decorated(prefix, ""));
        return Some(());
    }
    if let Some(aot) = item.as_array_of_tables_mut() {
        for existing in aot.iter_mut() {
            if chord_seq_is(existing, chords) {
                existing["run"] = toml_edit::value(command);
                return Some(());
            }
        }
        let mut tb = toml_edit::Table::new();
        tb["on"] = toml_edit::Item::Value(toml_edit::Value::Array(on));
        tb["run"] = toml_edit::value(command);
        aot.push(tb);
        return Some(());
    }
    None
}

/// Removes the binding `chords` → `command` from BOTH of `[<section>]`'s user
/// lists in the `keymap.toml` of `dir` (K3c) — the inverse of
/// [`persist_keymap_bind`], and the reason the editor can fix a mistake
/// instead of only making them. Matches both fields byte-exactly, so it only
/// ever deletes the row the editor showed; every copy of it goes, in both
/// lists, because leaving one behind would leave the key firing after the
/// editor said it was unbound.
///
/// A binding that is not there — or a missing `keymap.toml`, section or list —
/// is a documented NO-OP: [`KeymapWrite::changed`] is `false` and the file is
/// not rewritten (an identical rewrite would still move the mtime and wake the
/// watcher). An emptied value array stays as `<list> = []` rather than being
/// deleted, so a comment attached to the key survives; an emptied array of
/// tables has no such carrier and disappears with its last `[[…]]` header.
/// Note that removing an entry can take a comment written between it and the
/// PREVIOUS binding with it: TOML attaches such a comment to the element that
/// follows it. Nothing else in the file is touched.
///
/// Unlike the bind, a layer carrying a preset-only key is NOT refused here: a
/// removal cannot introduce an illegal shape, and refusing would leave a user
/// whose file has a stray `counts` unable to undo anything through the editor.
///
/// BLOCKING: synchronous FS I/O — the caller MUST wrap it in
/// `spawn_blocking` (rule 2).
///
/// # Errors
/// [`std::io::Error`] if `section` is not a keymap context or the binding is
/// empty ([`std::io::ErrorKind::InvalidInput`]), if the existing TOML does
/// not parse ([`std::io::ErrorKind::InvalidData`]), or if the I/O fails.
pub fn persist_keymap_unbind(
    dir: &Path,
    section: &str,
    chords: &[String],
    command: &str,
) -> std::io::Result<KeymapWrite> {
    check_keymap_binding(section, chords, command)?;
    // No `create_dir_all`: a removal creates nothing. A missing dir is the
    // same documented no-op as a binding that is not there.
    let declared = dir.join(KEYMAP_TOML);
    let lock = match lock_config_file(dir, KEYMAP_TOML) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(KeymapWrite {
                path: declared,
                changed: false,
            });
        }
        Err(e) => return Err(e),
    };
    let path = lock.target().to_path_buf();
    // A missing file parses as an empty document here, which matches nothing
    // and therefore writes nothing — the no-op above, reached by walking.
    let mut doc = open_config_toml(&path)?;
    let mut changed = false;
    for list in KeymapList::BOTH {
        let Some(item) = doc
            .as_table_mut()
            .get_mut(section)
            .and_then(toml_edit::Item::as_table_like_mut)
            .and_then(|t| t.get_mut(list.key()))
        else {
            continue;
        };
        if let Some(arr) = item.as_array_mut() {
            let before = arr.len();
            arr.retain(|v| {
                !v.as_inline_table()
                    .is_some_and(|t| binding_is(t, chords, command))
            });
            changed |= arr.len() != before;
        } else if let Some(aot) = item.as_array_of_tables_mut() {
            let before = aot.len();
            aot.retain(|t| !binding_is(t, chords, command));
            changed |= aot.len() != before;
        }
        // Any other shape: nothing to remove, and nothing this function can
        // fix — the no-op stands (see the rustdoc).
    }
    if changed {
        write_config_file(&lock, &doc)?;
    }
    Ok(KeymapWrite { path, changed })
}

/// A hotlist entry ALREADY merged and validated by [`load`]. `target` is
/// `Err` if `norte.toml`'s `path` does not parse as [`VPath`] — the entry is
/// KEPT (shown with an error badge in the popup, T5) instead of bringing
/// down the whole load: the hotlist is the user's data, not structural
/// config (spec 2026-07-18, decision 3). The error key is STABLE
/// (`"err-invalid-path"`, not the parser's raw `VPath` message): the popup
/// translates it via Fluent, and a hostile path (bidi, mile-long) never
/// reaches the bar intact (same caution as #73).
#[derive(Debug, Clone)]
pub struct HotlistItem {
    /// Displayed name.
    pub name: String,
    /// Already-parsed destination, or the stable error key.
    pub target: Result<VPath, String>,
}

/// STABLE error key for a hotlist `path` that does not parse as [`VPath`]
/// (see [`HotlistItem`]'s doc).
const ERR_INVALID_PATH: &str = "err-invalid-path";

/// Validates `entry.path` to [`VPath`] and merges it into `items`: if there is
/// already an entry with the same `name`, it replaces it — whether from an
/// EARLIER layer (the later layer wins, same as the rest of the config), or
/// from a PREVIOUS `[[hotlist]]` within the SAME layer (TOML does not stop
/// `name` repeating in an array of tables; `load` calls this function once
/// per entry, in order of appearance, so the LAST one wins intra-layer too).
/// Keeps the original position so the popup's order does not jump when only
/// an existing favorite's `path` is edited. If it did not exist, it is
/// appended at the end. Keys (`name`) compare byte-exact with NO
/// normalization (identity is never normalized); NFC/NFD twins coexist as
/// distinct rows — a deliberate decision.
fn merge_hotlist_entry(items: &mut Vec<HotlistItem>, entry: schema::HotlistEntry) {
    let target = VPath::parse(&entry.path).map_err(|_| ERR_INVALID_PATH.to_owned());
    if let Some(existing) = items.iter_mut().find(|it| it.name == entry.name) {
        existing.target = target;
    } else {
        items.push(HotlistItem {
            name: entry.name,
            target,
        });
    }
}

/// Quick-search behaviour of `/` (`[ui] quick_search`). The frontend maps
/// this onto its own navigation mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QuickSearch {
    /// Narrow the listing (default).
    #[default]
    Filter,
    /// Move the cursor without changing the listing.
    Jump,
}

/// `[ui] confirm_quit` behaviour (S2): whether `app.quit` opens a
/// confirmation modal before closing. Each frontend interprets `Auto`'s
/// "pending work" against its own model (TUI: active task-board rows; GUI:
/// tasks/marks) — this type only carries the mode, not the predicate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConfirmQuit {
    /// Confirm only when work is pending (the behavior before this setting
    /// existed, preserved as the default). Default.
    #[default]
    Auto,
    /// Always confirm, even with nothing pending.
    Always,
    /// Never confirm; `app.quit` closes immediately.
    Never,
}

impl ConfirmQuit {
    /// The wire string this variant round-trips from/to (`"auto"`,
    /// `"always"`, `"never"`) — used by the settings registry to display the
    /// current value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

/// `[ui] panel_bar_style`: how the panel bar names its buttons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PanelBarStyle {
    /// The localized panel name with its access letter underlined. Default.
    #[default]
    Names,
    /// Only the access letter — the row's original form.
    Letters,
    /// Unicode symbols that any terminal font draws in one cell (ADR 0140).
    /// In a row it is the same as `names`; in the terminal's column it is
    /// what `names` already draws there, since names do not fit.
    Icons,
    /// Nerd Font glyphs, closer to VS Code's icons, for a font that has
    /// them (ADR 0140).
    Nerd,
}

impl PanelBarStyle {
    /// The wire string this variant round-trips from/to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Names => "names",
            Self::Letters => "letters",
            Self::Icons => "icons",
            Self::Nerd => "nerd",
        }
    }

    /// Does a ROW paint the names? Everything but `letters`: the icon
    /// styles change the terminal's column, not the row.
    #[must_use]
    pub fn shows_names(self) -> bool {
        !matches!(self, Self::Letters)
    }
}

/// One item of the status bar's right half (`[ui] status_items`).
///
/// Informative only: what the listing IS (position, marks, order, name
/// encoding) and what is running. Warnings are not items — they live in the
/// left half, where no configuration can hide them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusItem {
    /// Cursor position over the listing, `3/120`.
    Position,
    /// What is marked, `2 marked · 4 MiB`.
    Marks,
    /// The order of the listing, `Name ↑`.
    Sort,
    /// How names are decoded, `UTF-8` / `CP437`.
    Encoding,
    /// How many tasks are running.
    Tasks,
    /// Notices that expired unread.
    Notices,
}

impl StatusItem {
    /// Every item, in the default order.
    pub const ALL: [Self; 6] = [
        Self::Position,
        Self::Marks,
        Self::Sort,
        Self::Encoding,
        Self::Tasks,
        Self::Notices,
    ];

    /// The id written in `norte.toml`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Position => "position",
            Self::Marks => "marks",
            Self::Sort => "sort",
            Self::Encoding => "encoding",
            Self::Tasks => "tasks",
            Self::Notices => "notices",
        }
    }

    /// The item an id names, if any.
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|i| i.as_str() == id)
    }
}

/// The status bar's right half, in screen order: at most one of each
/// [`StatusItem`]. A fixed array so [`UiChrome`] stays `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusItems {
    ids: [StatusItem; StatusItem::ALL.len()],
    len: u8,
}

impl StatusItems {
    /// All six, in [`StatusItem::ALL`] order.
    pub const DEFAULT: Self = Self {
        ids: StatusItem::ALL,
        len: 6,
    };

    /// Parses a list of ids, rejecting unknown and repeated ones.
    ///
    /// ```
    /// use norte_config::{StatusItem, StatusItems};
    /// let s = StatusItems::parse(&["tasks", "position"]).unwrap();
    /// assert_eq!(s.iter().collect::<Vec<_>>(), [StatusItem::Tasks, StatusItem::Position]);
    /// assert!(StatusItems::parse(&["tasks", "tasks"]).is_err());
    /// assert!(StatusItems::parse(&["git"]).is_err());
    /// assert_eq!(StatusItems::parse(&[] as &[&str]).unwrap().iter().count(), 0);
    /// ```
    ///
    /// # Errors
    /// The message to show, naming the valid ids (never the raw value).
    pub fn parse<S: AsRef<str>>(ids: &[S]) -> Result<Self, &'static str> {
        const MSG: &str = "invalid [ui] status_items: each id once, from \"position\", \
                           \"marks\", \"sort\", \"encoding\", \"tasks\" and \"notices\"";
        let mut out = Self {
            ids: StatusItem::ALL,
            len: 0,
        };
        for id in ids {
            let item = StatusItem::parse(id.as_ref()).ok_or(MSG)?;
            if out.iter().any(|i| i == item) {
                return Err(MSG);
            }
            // It fits: `ALL` has one of each, and repeats were already rejected.
            out.ids[usize::from(out.len)] = item;
            out.len += 1;
        }
        Ok(out)
    }

    /// The items, in screen order.
    pub fn iter(&self) -> impl Iterator<Item = StatusItem> + '_ {
        self.ids[..usize::from(self.len)].iter().copied()
    }

    /// The list as `norte.toml` and the settings screen spell it.
    #[must_use]
    pub fn to_ids(&self) -> Vec<&'static str> {
        self.iter().map(StatusItem::as_str).collect()
    }
}

/// `[ui] titlebar`: who draws the window's title bar (ADR 0136).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Titlebar {
    /// The desktop's own. Default: it is the one every other window has,
    /// and it works with whatever the window manager does.
    #[default]
    Native,
    /// None from the desktop: the menu bar doubles as the title bar.
    Custom,
}

impl Titlebar {
    /// The wire string this variant round-trips from/to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Custom => "custom",
        }
    }
}

/// `[ui] panel_bar_position`: where the panel bar sits.
///
/// `Auto` is not a third place: it is "what this frontend does best", and
/// the two answer differently on purpose. A terminal is short on width, so
/// its bar is a row on top; the window is short on height, so its bar is an
/// activity rail on the left, as in VS Code (spec 2026-09-21).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PanelBarPosition {
    /// Top in the terminal, left in the window. Default.
    #[default]
    Auto,
    /// A row under the menu bar.
    Top,
    /// A column on the left edge.
    Left,
}

impl PanelBarPosition {
    /// The wire string this variant round-trips from/to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Top => "top",
            Self::Left => "left",
        }
    }

    /// Whether the bar is a column, given the frontend's own answer for
    /// `Auto`.
    ///
    /// ```
    /// use norte_config::PanelBarPosition;
    /// assert!(!PanelBarPosition::Auto.vertical(false));
    /// assert!(PanelBarPosition::Auto.vertical(true));
    /// assert!(PanelBarPosition::Left.vertical(false));
    /// assert!(!PanelBarPosition::Top.vertical(true));
    /// ```
    #[must_use]
    pub fn vertical(self, auto_is_vertical: bool) -> bool {
        match self {
            Self::Auto => auto_is_vertical,
            Self::Top => false,
            Self::Left => true,
        }
    }
}

/// `[ui] date_format`: the default format of the `mtime` column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DateFormat {
    /// The time today, day and time this year, the date before that. Default.
    #[default]
    Smart,
    /// `11h ago`.
    Relative,
    /// `2026-09-10 14:02`.
    Iso,
}

impl DateFormat {
    /// The wire string this variant round-trips from/to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smart => "smart",
            Self::Relative => "relative",
            Self::Iso => "iso",
        }
    }
}

/// `[ui] splash`: what the startup screen does (spec 2026-09-15, phase 2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SplashMode {
    /// A cover over the first frame that any key — or the first listing plus a
    /// moment — takes away. Default: it says which build is running without
    /// standing between the reader and their files.
    #[default]
    Brief,
    /// No splash at all.
    Off,
    /// A start screen that stays until a key: recent and popular directories,
    /// bookmarks and profiles, each reachable by number.
    Home,
}

impl SplashMode {
    /// The wire string this variant round-trips from/to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Brief => "brief",
            Self::Off => "off",
            Self::Home => "home",
        }
    }
}

/// `[ui] processes_panel`: whether the processes panel opens by itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProcessesPanel {
    /// Opens when a task starts and closes when the last one is gone.
    /// Default: a panel that says "nothing running" is a third of the screen
    /// saying nothing.
    #[default]
    Auto,
    /// Only the command and the panel bar open or close it.
    Manual,
}

impl ProcessesPanel {
    /// The wire string this variant round-trips from/to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

/// `[ui] images`, validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Images {
    /// The terminal's protocol if it has one; otherwise the previewer.
    #[default]
    Auto,
    /// The terminal's protocol, even if the probe said no.
    Kitty,
    /// Half blocks from the previewer, even if the terminal knew better.
    Blocks,
    /// Neither one: the viewer stays on hexview.
    Off,
}

impl Images {
    /// The wire string this variant round-trips from/to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Kitty => "kitty",
            Self::Blocks => "blocks",
            Self::Off => "off",
        }
    }
}

/// `[ui] dir_indicator`: the `/` a directory row is prefixed with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DirIndicator {
    /// The slash only when the icon column is closed. Default: an icon
    /// already says what the row is, and then the slash is noise.
    #[default]
    Auto,
    /// Always, the way it has always been painted.
    Slash,
    /// Never.
    None,
}

impl DirIndicator {
    /// The wire string this variant round-trips from/to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Slash => "slash",
            Self::None => "none",
        }
    }
}

/// The `[ui]` keys that shape the CHROME around the listings (the key bar,
/// the panel bar's labels, the pane footer, the date format, notice expiry
/// and dialog buttons). All presentation-only, so every layer including
/// Project is honored, last-present-wins per key. One struct rather than six
/// more fields because the coverage sweeps destructure `CommonConfig` field
/// by field and clippy caps the merge helpers' argument lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UiChrome {
    /// `[ui] key_bar` (None = pinned).
    pub key_bar: Option<bool>,
    /// `[ui] panel_bar_style` (None = names), validated.
    pub panel_bar_style: Option<PanelBarStyle>,
    /// `[ui] panel_bar_position` (None = auto), validated.
    pub panel_bar_position: Option<PanelBarPosition>,
    /// `[ui] titlebar` (None = native), validated.
    pub titlebar: Option<Titlebar>,
    /// `[ui] status_items` (None = [`StatusItems::DEFAULT`]), validated.
    pub status_items: Option<StatusItems>,
    /// `[ui] pane_footer` (None = shown).
    pub pane_footer: Option<bool>,
    /// `[ui] row_stripes` (None = off): the listing's «pyjama».
    pub row_stripes: Option<bool>,
    /// `[ui] date_format` (None = smart), validated.
    pub date_format: Option<DateFormat>,
    /// `[ui] notice_seconds` (None = 8; 0 = until the next key), at most 600.
    pub notice_seconds: Option<u32>,
    /// `[ui] dialog_buttons` (None = buttons).
    pub dialog_buttons: Option<bool>,
    /// `[ui] history_size` (None = 30), within `5..=64`.
    pub history_size: Option<u32>,
    /// `[ui] splash` (None = brief), validated.
    pub splash: Option<SplashMode>,
    /// `[ui] splash_ms` (None = 4000): how long `brief` covers the first
    /// frame, in milliseconds, within `200..=60_000`.
    ///
    /// A cover that a reader cannot finish reading is a cover that only gets
    /// in the way, and 1200 ms — what this used to be, fixed — was not enough
    /// to take in the build and the core it talks to. Whoever wants it gone
    /// has `off`; whoever wants it to stay has `home`.
    pub splash_ms: Option<u32>,
    /// `[ui] processes_panel` (None = auto), validated.
    pub processes_panel: Option<ProcessesPanel>,
    /// `[ui] images` (None = auto), validated. A TERMINAL key: the GUI paints
    /// images through its own webview and never reads it.
    pub images: Option<Images>,
    /// `[ui] dir_indicator` (None = auto), validated.
    pub dir_indicator: Option<DirIndicator>,
}

impl UiChrome {
    /// The lowest `history_size`: below it `nav.back` stops being a trail.
    pub const MIN_HISTORY_SIZE: u32 = 5;
    /// The highest `history_size`: what the saved session keeps per panel
    /// (`norte_frontend::session::HISTORY_CAP`, pinned equal by a test there).
    pub const MAX_HISTORY_SIZE: u32 = 64;
    /// What `history_size` means when absent.
    pub const DEFAULT_HISTORY_SIZE: u32 = 30;

    /// Effective `history_size` (absent = 30).
    #[must_use]
    pub fn history_size(self) -> usize {
        self.history_size.unwrap_or(Self::DEFAULT_HISTORY_SIZE) as usize
    }

    /// The shortest `splash_ms`: below it the cover is a flash, not a screen.
    pub const MIN_SPLASH_MS: u32 = 200;
    /// The longest `splash_ms`: a minute of cover is `home` with extra steps,
    /// and `home` is the mode that stays until a key.
    pub const MAX_SPLASH_MS: u32 = 60_000;
    /// What `splash_ms` means when absent.
    ///
    /// It is also what `norte_frontend::splash::BRIEF_MS` reports, derived
    /// from here rather than written twice: two numbers that must agree, with
    /// nothing forcing them to, is how they stop agreeing.
    pub const DEFAULT_SPLASH_MS: u32 = 4_000;

    /// Effective `splash` (absent = brief).
    #[must_use]
    pub fn splash(self) -> SplashMode {
        self.splash.unwrap_or_default()
    }

    /// Effective `splash_ms` (absent = 4000).
    #[must_use]
    pub fn splash_ms(self) -> u32 {
        self.splash_ms.unwrap_or(Self::DEFAULT_SPLASH_MS)
    }

    /// Effective `processes_panel` (absent = auto).
    #[must_use]
    pub fn processes_panel(self) -> ProcessesPanel {
        self.processes_panel.unwrap_or_default()
    }

    /// Effective `dir_indicator` (absent = auto).
    #[must_use]
    pub fn dir_indicator(self) -> DirIndicator {
        self.dir_indicator.unwrap_or_default()
    }

    /// Effective `images` (absent = auto). A TERMINAL key: the GUI paints
    /// images through its own webview and never calls this.
    #[must_use]
    pub fn images(self) -> Images {
        self.images.unwrap_or_default()
    }

    /// The upper bound of `notice_seconds`: ten minutes is already "never
    /// goes away on its own" in practice, and a larger number is a typo.
    pub const MAX_NOTICE_SECONDS: u32 = 600;
    /// What `notice_seconds` means when absent.
    pub const DEFAULT_NOTICE_SECONDS: u32 = 8;

    /// Effective `key_bar` (absent = pinned).
    #[must_use]
    pub fn key_bar(self) -> bool {
        self.key_bar.unwrap_or(true)
    }
    /// Effective `panel_bar_style` (absent = names).
    #[must_use]
    pub fn panel_bar_style(self) -> PanelBarStyle {
        self.panel_bar_style.unwrap_or_default()
    }
    /// Effective `panel_bar_position` (absent = auto).
    #[must_use]
    pub fn panel_bar_position(self) -> PanelBarPosition {
        self.panel_bar_position.unwrap_or_default()
    }
    /// Effective `titlebar` (absent = native).
    #[must_use]
    pub fn titlebar(self) -> Titlebar {
        self.titlebar.unwrap_or_default()
    }
    /// Effective `status_items` (absent = all six).
    #[must_use]
    pub fn status_items(self) -> StatusItems {
        self.status_items.unwrap_or(StatusItems::DEFAULT)
    }
    /// Effective `pane_footer` (absent = shown).
    #[must_use]
    pub fn pane_footer(self) -> bool {
        self.pane_footer.unwrap_or(true)
    }
    /// Effective `row_stripes` (absent = off).
    #[must_use]
    pub fn row_stripes(self) -> bool {
        self.row_stripes.unwrap_or(false)
    }
    /// Effective `date_format` (absent = smart).
    #[must_use]
    pub fn date_format(self) -> DateFormat {
        self.date_format.unwrap_or_default()
    }
    /// Effective `notice_seconds` (absent = 8).
    #[must_use]
    pub fn notice_seconds(self) -> u32 {
        self.notice_seconds.unwrap_or(Self::DEFAULT_NOTICE_SECONDS)
    }
    /// Effective `dialog_buttons` (absent = buttons).
    #[must_use]
    pub fn dialog_buttons(self) -> bool {
        self.dialog_buttons.unwrap_or(true)
    }
}

/// `[ai]` already merged across layers and validated (ADR 0035 decision 3:
/// scalars last-present-wins; `denied_prefixes` union; providers merge by
/// name, later layer wins).
#[derive(Debug, Clone, Default)]
pub struct AiSettings {
    /// AI enabled (default false).
    pub enabled: bool,
    /// Local-only mode (default false).
    pub local_only: bool,
    /// Validated denied prefixes (union of all non-project layers).
    pub denied_prefixes: Vec<norte_proto::VPath>,
    /// Provider selected for rename.
    pub rename_provider: Option<String>,
    /// Provider selected for embeddings (`index.embed` /
    /// `index.search_semantic`). `None` = no embeddings.
    pub embed_provider: Option<String>,
    /// Providers by name (`BTreeMap` keeps deterministic order).
    pub providers: std::collections::BTreeMap<String, crate::schema::AiProviderEntry>,
}

/// `[ui.columns] sort`, resolved and VALIDATED (#108): closed vocabulary —
/// an invalid value is a load error with the culprit path (`quick_search`
/// pattern). The default reproduces the historical order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortChoice {
    /// Sort column.
    pub column: SortColumnKey,
    /// Direction.
    pub descending: bool,
    /// Directories first.
    pub dirs_first: bool,
}

impl Default for SortChoice {
    fn default() -> Self {
        Self {
            column: SortColumnKey::Name,
            descending: false,
            dirs_first: true,
        }
    }
}

/// Sort column from config's CLOSED vocabulary (#108). The frontend maps it
/// onto its own `SortSpec`; kept separate so the dependency direction is not
/// reversed (config does not know the frontend).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumnKey {
    /// Name.
    Name,
    /// Size.
    Size,
    /// Modification date.
    Mtime,
    /// Name extension (#138).
    Extension,
}

/// `[ui.columns]`, resolved (#108): RAW ids (an open set — the frontend
/// parses them, doctor reports them) + validated sort + per-scheme
/// overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnsConfig {
    /// Column ids in paint order; `None` = built-in default.
    pub default_columns: Option<Vec<String>>,
    /// Global sort; `None` = historical (name/asc/dirs-first).
    pub sort: Option<SortChoice>,
    /// Per-scheme overrides (the list REPLACES, never merges).
    pub schemes: std::collections::BTreeMap<String, SchemeColumns>,
    /// Global specs by column id (#108 7b), already validated.
    pub specs: std::collections::BTreeMap<String, ColumnSpec>,
}

/// A scheme's override inside [`ColumnsConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemeColumns {
    /// The scheme's column list; `None` = inherits the default.
    pub columns: Option<Vec<String>>,
    /// The scheme's sort; `None` = inherits the global one.
    pub sort: Option<SortChoice>,
    /// The scheme's specs by id (#108 7b); WIN over the global ones when
    /// resolving.
    pub specs: std::collections::BTreeMap<String, ColumnSpec>,
}

/// Width chosen in a spec (#108 7b), already validated to `[1, 64]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidthChoice {
    /// Width of the widest cell on the page (the frontend's ceiling).
    Auto,
    /// Fixed, in cells.
    Fixed(u16),
    /// Share by weight, with a floor.
    Flex {
        /// Floor in cells.
        min: u16,
        /// Share weight.
        weight: u16,
    },
}

/// Alignment chosen in a spec (#108 7b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignChoice {
    /// Left.
    Left,
    /// Right.
    Right,
}

/// A `[[ui.columns.spec]]`, resolved (#108 7b): vocabularies ALREADY
/// validated (a typo is a load error, the sort pattern); `format` stays a
/// string — whether it FITS the column is the frontend's call (doctor
/// reports it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnSpec {
    /// Width, if the spec sets it.
    pub width: Option<WidthChoice>,
    /// Alignment, if the spec sets it.
    pub align: Option<AlignChoice>,
    /// Format (closed global vocabulary; per-column fit is the frontend's
    /// call).
    pub format: Option<String>,
    /// Custom header (free text; the frontend sanitizes and caps it).
    pub header: Option<String>,
}

/// The merged `norte.toml` scalars — everything that is NOT a frontend-only
/// pass (keymap layers, openers). Core consumers read `archive_*`/`ai`;
/// frontends wrap this in their own loaded-config type.
#[derive(Debug, Clone)]
pub struct CommonConfig {
    /// Effective keymap preset (last-wins; compiled default).
    pub preset: String,
    /// `[ui] lang` (last-wins; None = environment).
    pub ui_lang: Option<String>,
    /// `[ui] theme` (last-wins; None = default preset).
    pub ui_theme: Option<String>,
    /// `[ui] theme_light` (last-wins; None = no light variant): the theme
    /// the window paints when the desktop prefers a light scheme. The
    /// terminal ignores it: a terminal has no scheme to ask.
    pub ui_theme_light: Option<String>,
    /// `[ui] theme_dark` (last-wins; None = no dark variant); see
    /// [`Self::ui_theme_light`].
    pub ui_theme_dark: Option<String>,
    /// `[ui] quick_search`, validated (invalid value = load error).
    pub quick_search: QuickSearch,
    /// `[ui] font` (last-wins; None = platform default). Honored from ALL
    /// layers including Project — presentation-only, same reasoning as the
    /// other `[ui]` scalars above.
    pub ui_font: Option<String>,
    /// `[ui] mono_font` (last-wins; None = bundled mono). Honored from ALL
    /// layers including Project — presentation-only, same reasoning as the
    /// other `[ui]` scalars above.
    pub ui_mono_font: Option<String>,
    /// `[ui] font_size`, validated to `[8.0, 32.0]` (invalid value = load
    /// error; None = platform/frontend default). Honored from ALL layers
    /// including Project — presentation-only, same reasoning as the other
    /// `[ui]` scalars above.
    pub ui_font_size: Option<f32>,
    /// `[ui] reduce_motion` (last-wins; None = motion allowed, spec §17 a11y
    /// / GUI phase G2). Honored from ALL layers including Project —
    /// presentation-only, same class as `ui_theme`/`ui_lang` above.
    pub ui_reduce_motion: Option<bool>,
    /// `[ui] confirm_quit`, validated (S2, invalid value = load error, same
    /// pattern as `quick_search`). Honored from ALL layers including
    /// Project — presentation-only, same class as the other `[ui]` scalars
    /// above.
    pub ui_confirm_quit: ConfirmQuit,
    /// `[ui.columns]` (#108, last-wins PER FIELD; schemes merge by key with
    /// the last one winning). Presentation-only: every layer.
    pub ui_columns: ColumnsConfig,
    /// `[ui] show_hidden` (#107, last-wins; None = show everything). Honored
    /// from ALL layers including Project — presentation-only, same class as
    /// the other `[ui]` scalars above: hiding dotfiles cannot launch, write,
    /// or redirect anything.
    pub ui_show_hidden: Option<bool>,
    /// `[ui] layout` (last-wins; None = the `orthodox` preset). Name of a
    /// file in `layouts/`. Presentation-only, like the rest of the `[ui]`
    /// scalars: laying out the screen cannot launch, write, or redirect
    /// anything.
    pub ui_layout: Option<String>,
    /// `[ui] mouse` (last-wins; None = captured). Honored from ALL layers
    /// including Project — presentation-only, same class as the other
    /// `[ui]` scalars above: capturing (or not capturing) the pointer
    /// cannot launch, write, or redirect anything.
    pub ui_mouse: Option<bool>,
    /// `[ui] alt_menu` (last-wins; None = off). Presentation-only, every
    /// layer: asking the terminal for a keyboard protocol cannot launch,
    /// write, or redirect anything.
    pub ui_alt_menu: Option<bool>,
    /// `[ui] menu_bar` (last-wins; None = PINNED). Presentation-only, every
    /// layer: a menu bar cannot launch, write, or redirect anything.
    ///
    /// On by default because the menu was the only door to several commands
    /// and there was nothing on screen saying it existed: whoever does not
    /// already know `Alt+M` cannot find what they cannot see.
    pub ui_menu_bar: Option<bool>,
    /// `[ui] panel_bar` (last-wins; None = PINNED). Presentation-only, every
    /// layer, same rule as the menu bar's.
    ///
    /// On by default for the same reason: the side panels opened by
    /// shortcut, by menu or by palette, and all three require KNOWING the
    /// panel exists. A panel contributed by a plugin, on top of that, nobody
    /// would discover.
    pub ui_panel_bar: Option<bool>,
    /// `[ui] parent_entry` (last-wins; None = ON). Presentation-only, every
    /// layer: a row that goes up a directory cannot launch, write, or
    /// redirect anything.
    ///
    /// On by default because that is what anyone coming from any manager in
    /// the family expects. Never an OPERAND: with the cursor over it nothing
    /// is marked, so a copy or a delete has nothing to act on instead of
    /// acting on the parent directory.
    pub ui_parent_entry: Option<bool>,
    /// `[ui] editor` (last-wins; None = `$VISUAL`/`$EDITOR`/POSIX fallback).
    ///
    /// Argv template with `openers.toml`'s field codes (`%f` the file, `%d`
    /// the pane's directory). **Never from the project layer**: it names a
    /// program that gets run, so a foreign repo does not choose what runs
    /// when F4 is pressed — same fail-closed rule as `[daemon]` and
    /// `openers.toml`.
    pub ui_editor: Option<Vec<String>>,
    /// `[ui] editor_detached` (last-wins; None = `false`): that editor opens
    /// its OWN WINDOW, so the frontend does not suspend waiting for it. Same
    /// fail-closed layer as [`Self::ui_editor`].
    pub ui_editor_detached: Option<bool>,
    /// `[ui] diff` (last-wins; None = `diff -u`, waiting for a key).
    ///
    /// Argv template with `openers.toml`'s field codes — `%F` is BOTH files,
    /// `%d` the pane's directory. Same fail-closed layer as
    /// [`Self::ui_editor`]: it names a program that gets run.
    pub ui_diff: Option<Vec<String>>,
    /// `[ui] diff_detached` (last-wins; None = `false`): that comparator opens
    /// its OWN WINDOW. Same fail-closed layer as [`Self::ui_diff`].
    pub ui_diff_detached: Option<bool>,
    /// The `[ui]` chrome keys (key bar, panel bar style, pane footer, date
    /// format, notice expiry, dialog buttons), last-wins per key from ALL
    /// layers including Project: presentation-only, like the scalars above.
    pub ui_chrome: UiChrome,
    /// `[ui] status_plugins` (ADR 0137), validated `(plugin, column)` pairs
    /// in screen order; last-present-wins. Empty = none. From every layer:
    /// it only chooses what to SHOW, and a plugin runs only if the user
    /// approved it, whoever names it.
    pub ui_status_plugins: Vec<(String, String)>,
    /// `[daemon]` merged (last-wins per key; never from Project or a profile
    /// — fail-closed, review MAJOR-1). Startup only.
    pub daemon: crate::DaemonSettings,
    /// Hotlist merged from every layer except Project.
    pub hotlist: Vec<HotlistItem>,
    /// `[archive]` merged (last-wins per key; never from Project or a
    /// profile — `rar_delegate` names an executable).
    pub archive: crate::ArchiveSettings,
    /// `[log]` merged (last-wins per key; never from Project or a profile —
    /// choosing where a process writes is not presentation).
    pub log: crate::LogSettings,
    /// `[ai]` merged (never from Project).
    pub ai: AiSettings,
    /// Files that participated (watcher + diagnostics).
    pub sources: Vec<std::path::PathBuf>,
    /// PROJECT layers that failed to load, with their reason already stated.
    ///
    /// A broken `.norte.toml` in a repository cannot leave whoever `cd`s
    /// there with no file manager at all (#260): the layer is skipped and
    /// startup continues with the rest, which is what the user already had.
    /// Empty = everything loaded. Whoever paints it shows it; staying silent
    /// would leave a project configuration the reader believes is active when
    /// it is not.
    pub project_warnings: Vec<String>,
    /// Keys a PROFILE layer declared that it may not set (spec 2026-08-26,
    /// D2), with their reason already stated.
    ///
    /// Kept apart from [`Self::project_warnings`] on purpose: these are two
    /// different sources, and a reader cannot react the same way to "your
    /// profile asks for something a profile does not decide" as to "this
    /// repository brings a config that does not apply". Empty = the profile
    /// only asked for its own.
    ///
    /// Staying silent about these would be the serious part: a profile is
    /// chosen from a LIST while the program is running, not the way a config
    /// layer is edited, and a picker that silently grants is a privilege
    /// escalator.
    pub profile_warnings: Vec<String>,
    /// `[profile] title` of the active profile layer, to display. The
    /// profile's IDENTITY is its directory, not this.
    pub profile_title: Option<String>,
    /// `[profile.start]`, already parsed: where each slot opens when the
    /// profile has no saved state yet.
    ///
    /// The keys are slot ids of the PROFILE's own layout; the values,
    /// [`VPath`]s in WIRE form, which is what [`crate::save_profile`] writes.
    /// Whatever does not parse — neither the key as an id nor the value as a
    /// path — is dropped and reported in [`Self::profile_warnings`].
    ///
    /// A [`VPath`] and not a `PathBuf`: a profile's slot can sit on sftp or
    /// inside a container, and this was written for months in wire form while
    /// being read as a system path, with nobody noticing because nobody read
    /// it.
    pub profile_start: std::collections::BTreeMap<u32, norte_proto::VPath>,
}

/// Merges one layer's `[ai]` section (already filtered to non-Project by the
/// caller) into `ai`: scalars last-present-wins, `denied_prefixes` is a
/// UNION (each entry validated as a [`VPath`]), providers merge by name with
/// the later layer winning. Extracted out of [`load`] to stay under
/// clippy's line-count cap (ADR 0035 decision 3: the fail-closed Project
/// carve-out lives in the caller).
///
/// # Errors
/// [`ConfigError::Toml`] if a `denied_prefixes` entry does not parse as a
/// [`VPath`].
fn merge_ai_layer(
    ai: &mut AiSettings,
    a: crate::schema::AiSection,
    norte: &Path,
) -> Result<(), ConfigError> {
    if let Some(v) = a.enabled {
        ai.enabled = v;
    }
    if let Some(v) = a.local_only {
        ai.local_only = v;
    }
    if let Some(v) = a.rename_provider {
        ai.rename_provider = Some(v);
    }
    if let Some(v) = a.embed_provider {
        ai.embed_provider = Some(v);
    }
    for (i, p) in a.denied_prefixes.iter().enumerate() {
        let vp = VPath::parse(p).map_err(|_| ConfigError::Toml {
            path: norte.to_path_buf(),
            // Same #73 caution as quick_search: never quote the raw
            // (possibly hostile) value in the diagnostic — the 1-based
            // index is enough to locate the offending entry.
            message: format!(
                "[ai] denied_prefixes: entry {} does not parse as a VPath",
                i + 1
            ),
        })?;
        if !ai.denied_prefixes.contains(&vp) {
            ai.denied_prefixes.push(vp);
        }
    }
    ai.providers.extend(a.providers);
    Ok(())
}

/// Merges one layer's already-parsed `[ui] font`/`mono_font`/`font_size`/
/// `reduce_motion` values into the accumulators (last-present-wins),
/// validating `font_size` against `[8.0, 32.0]` (GP:
/// `docs/superpowers/specs/2026-07-23-gui-visual-plugins-design.md`;
/// `reduce_motion` is G2, spec §17 a11y — no validation needed, any bool is
/// valid). Extracted out of [`load`] to stay under clippy's line-count cap,
/// same pattern as [`merge_ai_layer`].
///
/// # Errors
/// [`ConfigError::Toml`] if `font_size` is outside `[8.0, 32.0]`.
#[expect(
    clippy::too_many_arguments,
    reason = "field-by-field merge of the UI's fonts"
)]
fn merge_ui_fonts(
    ui_font: &mut Option<String>,
    ui_mono_font: &mut Option<String>,
    ui_font_size: &mut Option<f32>,
    ui_reduce_motion: &mut Option<bool>,
    font: Option<String>,
    mono_font: Option<String>,
    font_size: Option<f32>,
    reduce_motion: Option<bool>,
    norte: &Path,
) -> Result<(), ConfigError> {
    if let Some(f) = font {
        *ui_font = Some(f);
    }
    if let Some(mf) = mono_font {
        *ui_mono_font = Some(mf);
    }
    if let Some(fs) = font_size {
        if !(8.0..=32.0).contains(&fs) {
            return Err(ConfigError::Toml {
                path: norte.to_path_buf(),
                // #73: never quote the raw value in the diagnostic.
                message: "[ui] font_size out of range [8, 32]".to_owned(),
            });
        }
        *ui_font_size = Some(fs);
    }
    if let Some(rm) = reduce_motion {
        *ui_reduce_motion = Some(rm);
    }
    Ok(())
}

/// Merges one layer's `[ui]` BOOLEANS (`show_hidden`, `mouse`) into the
/// accumulators (last-present-wins). No validation: any bool is valid, and
/// both are presentation-only, so every layer including Project is honored
/// (same class as `theme`/`lang`).
///
/// A function of its own for the same reason as [`merge_ui_fonts`]: [`load`]
/// is a single pass over the layers and clippy caps its length, so each new
/// key has to bring its own merge rather than another line in the loop.
#[expect(
    clippy::too_many_arguments,
    reason = "one accumulator per `[ui]` key: folding them into a struct would move the problem to `load`"
)]
fn merge_ui_flags(
    ui_show_hidden: &mut Option<bool>,
    ui_mouse: &mut Option<bool>,
    ui_alt_menu: &mut Option<bool>,
    ui_menu_bar: &mut Option<bool>,
    ui_panel_bar: &mut Option<bool>,
    ui_parent_entry: &mut Option<bool>,
    ui_layout: &mut Option<String>,
    ui: &crate::schema::UiSection,
) {
    *ui_show_hidden = ui.show_hidden.or(*ui_show_hidden);
    *ui_mouse = ui.mouse.or(*ui_mouse);
    *ui_alt_menu = ui.alt_menu.or(*ui_alt_menu);
    *ui_menu_bar = ui.menu_bar.or(*ui_menu_bar);
    *ui_panel_bar = ui.panel_bar.or(*ui_panel_bar);
    *ui_parent_entry = ui.parent_entry.or(*ui_parent_entry);
    *ui_layout = ui.layout.clone().or(ui_layout.take());
}

/// At most this many plugin status items (ADR 0137): each one is a plugin
/// call per listing, and the bar is one row.
pub const STATUS_PLUGINS_MAX: usize = 4;

/// `[ui] status_plugins` (ADR 0137): `"plugin:<plugin>/<column>"` ids into
/// `(plugin, column)` pairs, in order. The same shape as a plugin column id
/// in `[columns]`; the characters are not restricted here because the pair
/// is only ever compared against what an approved plugin declares.
///
/// # Errors
/// A neutral message (never the raw value, #73) on a malformed, repeated
/// or excess id.
fn parse_status_plugins(ids: &[String]) -> Result<Vec<(String, String)>, &'static str> {
    const MSG: &str = "invalid [ui] status_plugins: each id once, shaped like \
                       \"plugin:<plugin>/<column>\", and at most four";
    if ids.len() > STATUS_PLUGINS_MAX {
        return Err(MSG);
    }
    let mut out: Vec<(String, String)> = Vec::with_capacity(ids.len());
    for id in ids {
        let (plugin, column) = id
            .strip_prefix("plugin:")
            .and_then(|r| r.split_once('/'))
            .filter(|(p, c)| !p.is_empty() && !c.is_empty())
            .ok_or(MSG)?;
        if out.iter().any(|(p, c)| p == plugin && c == column) {
            return Err(MSG);
        }
        out.push((plugin.to_owned(), column.to_owned()));
    }
    Ok(out)
}

/// The panel bar's two enums, `panel_bar_style` and `panel_bar_position`.
/// Split out of [`merge_ui_chrome`] only for length; the error is the
/// message, and the caller attaches the file.
fn merge_panel_bar(acc: &mut UiChrome, ui: &crate::schema::UiSection) -> Result<(), &'static str> {
    if let Some(raw) = &ui.panel_bar_style {
        acc.panel_bar_style = Some(match raw.as_str() {
            "names" => PanelBarStyle::Names,
            "letters" => PanelBarStyle::Letters,
            "icons" => PanelBarStyle::Icons,
            "nerd" => PanelBarStyle::Nerd,
            _ => {
                return Err(
                    "invalid [ui] panel_bar_style: only \"names\", \"letters\", \"icons\" or \"nerd\" are accepted",
                );
            }
        });
    }
    if let Some(raw) = &ui.panel_bar_position {
        acc.panel_bar_position = Some(match raw.as_str() {
            "auto" => PanelBarPosition::Auto,
            "top" => PanelBarPosition::Top,
            "left" => PanelBarPosition::Left,
            _ => {
                return Err(
                    "invalid [ui] panel_bar_position: only \"auto\", \"top\" or \"left\" are accepted",
                );
            }
        });
    }
    if let Some(raw) = &ui.titlebar {
        acc.titlebar = Some(match raw.as_str() {
            "native" => Titlebar::Native,
            "custom" => Titlebar::Custom,
            _ => return Err("invalid [ui] titlebar: only \"native\" or \"custom\" are accepted"),
        });
    }
    Ok(())
}

/// Merges one layer's `[ui]` CHROME keys into the accumulator
/// (last-present-wins per key), validating the enums and the bound of
/// `notice_seconds` so the diagnostic can name the source file. Same
/// #73 caution as `parse_confirm_quit`: the message names the valid values,
/// never the raw one.
///
/// # Errors
/// [`ConfigError::Toml`] on an unknown `panel_bar_style`,
/// `panel_bar_position` or `date_format`, or a `notice_seconds` above
/// [`UiChrome::MAX_NOTICE_SECONDS`].
fn merge_ui_chrome(
    acc: &mut UiChrome,
    ui: &crate::schema::UiSection,
    norte: &Path,
) -> Result<(), ConfigError> {
    let bad = |message: &str| ConfigError::Toml {
        path: norte.to_path_buf(),
        message: message.to_owned(),
    };
    acc.key_bar = ui.key_bar.or(acc.key_bar);
    acc.pane_footer = ui.pane_footer.or(acc.pane_footer);
    acc.row_stripes = ui.row_stripes.or(acc.row_stripes);
    acc.dialog_buttons = ui.dialog_buttons.or(acc.dialog_buttons);
    merge_panel_bar(acc, ui).map_err(bad)?;
    if let Some(ids) = &ui.status_items {
        acc.status_items = Some(StatusItems::parse(ids).map_err(bad)?);
    }
    if let Some(raw) = &ui.date_format {
        acc.date_format = Some(match raw.as_str() {
            "smart" => DateFormat::Smart,
            "relative" => DateFormat::Relative,
            "iso" => DateFormat::Iso,
            _ => {
                return Err(bad(
                    "invalid [ui] date_format: only \"smart\", \"relative\" or \"iso\" are accepted",
                ));
            }
        });
    }
    if let Some(n) = ui.notice_seconds {
        if n > UiChrome::MAX_NOTICE_SECONDS {
            return Err(bad("invalid [ui] notice_seconds: the maximum is 600"));
        }
        acc.notice_seconds = Some(n);
    }
    if let Some(n) = ui.history_size {
        if !(UiChrome::MIN_HISTORY_SIZE..=UiChrome::MAX_HISTORY_SIZE).contains(&n) {
            return Err(bad("invalid [ui] history_size: between 5 and 64"));
        }
        acc.history_size = Some(n);
    }
    if let Some(n) = ui.splash_ms {
        if !(UiChrome::MIN_SPLASH_MS..=UiChrome::MAX_SPLASH_MS).contains(&n) {
            return Err(bad("invalid [ui] splash_ms: between 200 and 60000"));
        }
        acc.splash_ms = Some(n);
    }
    if let Some(raw) = &ui.splash {
        acc.splash = Some(match raw.as_str() {
            "brief" => SplashMode::Brief,
            "off" => SplashMode::Off,
            "home" => SplashMode::Home,
            _ => {
                return Err(bad(
                    "invalid [ui] splash: only \"brief\", \"off\" or \"home\" are accepted",
                ));
            }
        });
    }
    if let Some(raw) = &ui.processes_panel {
        acc.processes_panel = Some(match raw.as_str() {
            "auto" => ProcessesPanel::Auto,
            "manual" => ProcessesPanel::Manual,
            _ => {
                return Err(bad(
                    "invalid [ui] processes_panel: only \"auto\" or \"manual\" are accepted",
                ));
            }
        });
    }
    if let Some(raw) = &ui.images {
        acc.images = Some(match raw.as_str() {
            "auto" => Images::Auto,
            "kitty" => Images::Kitty,
            "blocks" => Images::Blocks,
            "off" => Images::Off,
            _ => {
                return Err(bad(
                    "invalid [ui] images: only \"auto\", \"kitty\", \"blocks\" or \"off\" are accepted",
                ));
            }
        });
    }
    if let Some(raw) = &ui.dir_indicator {
        acc.dir_indicator = Some(match raw.as_str() {
            "auto" => DirIndicator::Auto,
            "slash" => DirIndicator::Slash,
            "none" => DirIndicator::None,
            _ => {
                return Err(bad(
                    "invalid [ui] dir_indicator: only \"auto\", \"slash\" or \"none\" are accepted",
                ));
            }
        });
    }
    Ok(())
}

/// Merges a `[ui.columns]` layer onto the accumulator (#108): last-wins per
/// field; schemes merge by key (the last one wins per field).
fn merge_ui_columns(
    acc: &mut ColumnsConfig,
    cols: &crate::schema::UiColumnsSection,
    norte: &Path,
) -> Result<(), ConfigError> {
    if let Some(d) = &cols.default {
        acc.default_columns = Some(d.clone());
    }
    if let Some(sort) = &cols.sort {
        acc.sort = Some(parse_sort_section(sort, norte)?);
    }
    if let Some(spec) = &cols.spec {
        for (id, s) in parse_spec_entries(spec, norte, "[ui.columns]")? {
            fold_spec_into(&mut acc.specs, id, s);
        }
    }
    if let Some(schemes) = &cols.scheme {
        for (k, v) in schemes {
            let entry = acc.schemes.entry(k.clone()).or_default();
            if let Some(c) = &v.columns {
                entry.columns = Some(c.clone());
            }
            if let Some(sort) = &v.sort {
                entry.sort = Some(parse_sort_section(sort, norte)?);
            }
            if let Some(spec) = &v.spec {
                for (id, s) in parse_spec_entries(spec, norte, "[ui.columns.scheme]")? {
                    fold_spec_into(&mut entry.specs, id, s);
                }
            }
        }
    }
    Ok(())
}

/// Validates the `[[ui.columns.spec]]` entries of ONE layer (#108 7b): the
/// `id` is an open set (the frontend parses it, doctor reports it), but each
/// vocabulary is CLOSED — a typo is a load error with the culprit path (sort
/// pattern), never a silent skip. An `id` repeated within the layer merges
/// last-wins PER FIELD, same as across layers (same rule as the intra-layer
/// hotlist). The diagnostic never quotes the raw value (#73).
fn parse_spec_entries(
    raw: &[schema::ColumnSpecSection],
    norte: &Path,
    label: &str,
) -> Result<std::collections::BTreeMap<String, ColumnSpec>, ConfigError> {
    let mut out = std::collections::BTreeMap::new();
    for entry in raw {
        if entry.id.is_empty() {
            return Err(ConfigError::Toml {
                path: norte.to_path_buf(),
                message: format!("{label} spec.id: cannot be empty"),
            });
        }
        let width = entry
            .width
            .as_ref()
            .map(|w| parse_spec_width(w, norte, label))
            .transpose()?;
        let align = match entry.align.as_deref() {
            None => None,
            Some("left") => Some(AlignChoice::Left),
            Some("right") => Some(AlignChoice::Right),
            Some(_) => {
                return Err(ConfigError::Toml {
                    path: norte.to_path_buf(),
                    message: format!("{label} spec.align: left | right"),
                });
            }
        };
        let format = match entry.format.as_deref() {
            None => None,
            Some(f @ ("exact" | "iec" | "si" | "relative" | "iso" | "smart" | "octal" | "rwx")) => {
                Some(f.to_owned())
            }
            Some(_) => {
                return Err(ConfigError::Toml {
                    path: norte.to_path_buf(),
                    message: format!(
                        "{label} spec.format: exact | iec | si | relative | iso | octal | rwx"
                    ),
                });
            }
        };
        fold_spec_into(
            &mut out,
            entry.id.clone(),
            ColumnSpec {
                width,
                align,
                format,
                header: entry.header.clone(),
            },
        );
    }
    Ok(out)
}

/// Validates a spec's `width` (#108 7b): keyword only `"auto"`; `fixed`/`min`
/// bounded to `[1, 64]` (0 cells paints nothing and >64 eats the pane).
/// `weight` is left free (0 = never grows, documented).
fn parse_spec_width(
    raw: &schema::WidthSection,
    norte: &Path,
    label: &str,
) -> Result<WidthChoice, ConfigError> {
    let range_err = || ConfigError::Toml {
        path: norte.to_path_buf(),
        message: format!("{label} spec.width: fixed/min out of range [1, 64]"),
    };
    match raw {
        schema::WidthSection::Keyword(s) if s == "auto" => Ok(WidthChoice::Auto),
        schema::WidthSection::Keyword(_) => Err(ConfigError::Toml {
            path: norte.to_path_buf(),
            message: format!(
                "{label} spec.width: \"auto\" | {{ fixed = n }} | {{ min = n, weight = m }}"
            ),
        }),
        schema::WidthSection::Fixed { fixed } => (1..=64)
            .contains(fixed)
            .then_some(WidthChoice::Fixed(*fixed))
            .ok_or_else(range_err),
        schema::WidthSection::Flex { min, weight } => (1..=64)
            .contains(min)
            .then_some(WidthChoice::Flex {
                min: *min,
                weight: *weight,
            })
            .ok_or_else(range_err),
    }
}

/// Merges `spec` onto `map[id]` last-wins PER FIELD (`Some` overrides, `None`
/// keeps the previous layer) — the same rule in the intra-layer merge and
/// across layers.
fn fold_spec_into(
    map: &mut std::collections::BTreeMap<String, ColumnSpec>,
    id: String,
    spec: ColumnSpec,
) {
    let e = map.entry(id).or_default();
    if let Some(w) = spec.width {
        e.width = Some(w);
    }
    if let Some(a) = spec.align {
        e.align = Some(a);
    }
    if let Some(f) = spec.format {
        e.format = Some(f);
    }
    if let Some(h) = spec.header {
        e.header = Some(h);
    }
}

/// Validates a [`crate::schema::SortSection`] (#108): CLOSED vocabulary,
/// invalid = error with the path (`quick_search` pattern). Absent fields
/// fall back to the historical default.
fn parse_sort_section(
    raw: &crate::schema::SortSection,
    norte: &Path,
) -> Result<SortChoice, ConfigError> {
    let column = match raw.column.as_deref() {
        None | Some("name") => SortColumnKey::Name,
        Some("size") => SortColumnKey::Size,
        Some("mtime") => SortColumnKey::Mtime,
        Some("extension") => SortColumnKey::Extension,
        Some(_) => {
            return Err(ConfigError::Toml {
                path: norte.to_path_buf(),
                message: "[ui.columns] sort.column: name | size | mtime | extension".to_owned(),
            });
        }
    };
    let descending = match raw.dir.as_deref() {
        None | Some("asc") => false,
        Some("desc") => true,
        Some(_) => {
            return Err(ConfigError::Toml {
                path: norte.to_path_buf(),
                message: "[ui.columns] sort.dir: asc | desc".to_owned(),
            });
        }
    };
    Ok(SortChoice {
        column,
        descending,
        dirs_first: raw.dirs_first.unwrap_or(true),
    })
}

/// Parses `[ui] quick_search`'s raw string into [`QuickSearch`]. Same #73
/// caution as `toml_diag`: a hostile TOML could stuff a bidi/kilometric
/// string into anything, and this is a two-value field — naming the
/// offending file plus the two valid values is enough context, no need to
/// reflect `raw` itself. Extracted out of [`load`] to stay under clippy's
/// line-count cap, same pattern as the `merge_*` helpers above.
///
/// # Errors
/// [`ConfigError::Toml`] if `raw` isn't `"filter"`/`"jump"`.
fn parse_quick_search(raw: &str, norte: &Path) -> Result<QuickSearch, ConfigError> {
    match raw {
        "filter" => Ok(QuickSearch::Filter),
        "jump" => Ok(QuickSearch::Jump),
        _ => Err(ConfigError::Toml {
            path: norte.to_path_buf(),
            message: "invalid [ui] quick_search: only \"filter\" or \"jump\" are accepted"
                .to_owned(),
        }),
    }
}

/// Parses `[ui] confirm_quit`'s raw string into [`ConfirmQuit`] (S2). Same
/// #73 caution as `quick_search`'s inline match: the diagnostic never quotes
/// the raw value — this field only has three valid values, so naming them is
/// enough context. Extracted out of [`load`] to stay under clippy's
/// line-count cap, same pattern as the `merge_*` helpers above.
///
/// # Errors
/// [`ConfigError::Toml`] if `raw` isn't `"auto"`/`"always"`/`"never"`.
fn parse_confirm_quit(raw: &str, norte: &Path) -> Result<ConfirmQuit, ConfigError> {
    match raw {
        "auto" => Ok(ConfirmQuit::Auto),
        "always" => Ok(ConfirmQuit::Always),
        "never" => Ok(ConfirmQuit::Never),
        _ => Err(ConfigError::Toml {
            path: norte.to_path_buf(),
            message:
                "invalid [ui] confirm_quit: only \"auto\", \"always\" or \"never\" are accepted"
                    .to_owned(),
        }),
    }
}

/// This layer's `[ui] quick_search`, or the previous value if the layer is a
/// PROJECT one and the value is not valid — with its warning.
///
/// Same rule as [`parse_layer`]: a foreign repository does not leave whoever
/// `cd`s there with no file manager (#260).
fn merge_quick_search(
    current: QuickSearch,
    value: &str,
    path: &std::path::Path,
    kind: Layer,
    warnings: &mut Vec<String>,
) -> Result<QuickSearch, ConfigError> {
    match parse_quick_search(value, path) {
        Ok(v) => Ok(v),
        Err(e) if kind == Layer::Project => {
            warnings.push(e.to_string());
            Ok(current)
        }
        Err(e) => Err(e),
    }
}

/// The error a `norte.toml` that fails to parse produces, with its
/// diagnostic.
fn layer_error(raw: &str, path: &std::path::Path) -> ConfigError {
    match toml::from_str::<NorteToml>(raw) {
        Ok(_) => ConfigError::Toml {
            path: path.to_path_buf(),
            message: String::new(),
        },
        Err(e) => ConfigError::Toml {
            path: path.to_path_buf(),
            message: toml_diag(raw, &e),
        },
    }
}

/// Parses a layer. `Ok(None)` = it was a PROJECT one and it does not parse,
/// so it is skipped.
///
/// Any unknown key is fatal under `deny_unknown_fields`, so a `.norte.toml`
/// with a typo broke the whole file manager on `cd`-ing into that repository
/// — and whoever wrote it may not be the one who suffers it (#260). User and
/// system layers stay fatal: those ARE the reader's own, and starting while
/// silently ignoring them would be worse than not starting.
fn parse_layer(
    raw: &str,
    path: &std::path::Path,
    kind: Layer,
) -> Result<Option<NorteToml>, ConfigError> {
    match toml::from_str::<NorteToml>(raw) {
        Ok(p) => Ok(Some(p)),
        Err(_) if kind == Layer::Project => Ok(None),
        Err(e) => Err(ConfigError::Toml {
            path: path.to_path_buf(),
            message: toml_diag(raw, &e),
        }),
    }
}

/// Which layers may set what is NOT presentation.
///
/// Written in the POSITIVE on purpose. The previous version was
/// `*kind != Layer::Project`, and with it adding a variant to [`Layer`]
/// SILENTLY granted the transport, AI, logs and anti-bomb limits to the new
/// layer. An exhaustive `match` forces the decision to be made when the
/// variant is added, which is when someone is actually thinking about it.
const fn governs_outside_presentation(kind: Layer) -> bool {
    match kind {
        Layer::System | Layer::User => true,
        Layer::Profile | Layer::Project => false,
    }
}

/// Whether the layer is a USER file, in the sense that matters here: written
/// by whoever is going to live with it.
///
/// System, user and profile are; a `./.norte` from a foreign repository is
/// not. It is the line `keymap.preset` draws (#260 — choosing which key
/// deletes is not presentation) and the one `[[hotlist]]` draws (a foreign
/// repo does not inject favorites into anyone's session), and both draw it in
/// the same place.
const fn is_user_owned_layer(kind: Layer) -> bool {
    match kind {
        Layer::System | Layer::User | Layer::Profile => true,
        Layer::Project => false,
    }
}

/// The warnings for a PROFILE or PROJECT layer that asked for something that
/// layer does not decide (D2).
///
/// An ABSENT section warns of nothing: what gets said is what the file
/// declared and is not going to be applied. Until now only a profile warned,
/// and not even about everything: the editor and the comparator (programs
/// that get run) were silently dropped, and a repository's `.norte.toml`
/// stayed silent entirely. It failed closed — nothing was applied — but the
/// file's owner had no way to know why their change did nothing.
///
/// The SYSTEM and USER layers decide everything, so they do not warn.
fn carve_out_warnings(parsed: &NorteToml, path: &std::path::Path, kind: Layer) -> Vec<String> {
    let who = match kind {
        Layer::Profile => "a profile",
        Layer::Project => "a project",
        Layer::System | Layer::User => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut say = |section: &str, reason: &str| {
        out.push(format!(
            "{}: [{section}] is not decided by {who} ({reason})",
            path.display()
        ));
    };
    let ui = &parsed.ui;
    if ui.editor.is_some()
        || ui.editor_detached.is_some()
        || ui.diff.is_some()
        || ui.diff_detached.is_some()
    {
        say(
            "ui",
            "`editor` and `diff` choose a program that gets run, and that is not \
             presentation",
        );
    }
    // What a profile DOES decide and a foreign repository does not: the key
    // preset (#260, choosing which key deletes is not presentation) and the
    // favorites (a repo does not inject places into anyone's session).
    if kind == Layer::Project {
        if parsed.keymap.preset.is_some() {
            say("keymap", "choosing which key deletes is not presentation");
        }
        if !parsed.hotlist.is_empty() {
            say(
                "hotlist",
                "a repository does not add favorites to anyone's session",
            );
        }
    }
    // `declara` destructures each section with no `..`: a new key that
    // neither one looks at does not compile. The field-by-field list that
    // used to be here left out `[log] format`, and the four-section test did
    // not catch it because its profile carried `dir`.
    if crate::DaemonSettings::declara(&parsed.daemon) {
        say("daemon", "does not redirect the core's transport");
    }
    if parsed.ai != crate::schema::AiSection::default() {
        say("ai", "does not enable AI or redirect its providers");
    }
    if crate::LogSettings::declara(&parsed.log) {
        say("log", "does not decide where or how this process writes");
    }
    if crate::ArchiveSettings::declara(&parsed.archive) {
        say("archive", "does not raise the anti-bomb limits");
    }
    out
}

/// Merges the `[profile]` of a PROFILE layer (spec 2026-08-26, D3).
///
/// Last-wins like everything else. A `start` key that does not parse as a
/// slot id — or a value that does not parse as [`VPath`] — is DROPPED with its
/// warning, instead of bringing startup down: the file is the user's own, but
/// a typo in an id does not earn a refusal to start, and the whole layer
/// would be lost over one line.
fn merge_profile_section(
    title: &mut Option<String>,
    start: &mut std::collections::BTreeMap<u32, norte_proto::VPath>,
    section: &crate::schema::ProfileSection,
    path: &std::path::Path,
    warnings: &mut Vec<String>,
) {
    if let Some(t) = &section.title {
        *title = Some(t.clone());
    }
    for (key, value) in &section.start {
        let Ok(id) = key.parse::<u32>() else {
            warnings.push(format!(
                "{}: [profile.start] \"{key}\" is not a slot id",
                path.display()
            ));
            continue;
        };
        // A VPath in WIRE form, which is what `save_profile` writes. Not a
        // system path: a profile's slot can be on sftp or inside a
        // container, and a `PathBuf` cannot say so. It also removes the
        // question of what a `~` or a relative path would resolve against —
        // a profile is used across machines and across days, and "depends on
        // where you launched it from" is not an answer.
        match norte_proto::VPath::parse(value) {
            Ok(v) => {
                start.insert(id, v);
            }
            // Says WHICH slot is left unseeded, which is the actionable
            // part, not the value: repeating it does not help fix it —
            // whoever wrote it has it in front of them — and these strings
            // end up in the log panel, where one extra path is one path too
            // many. Only the COUNT reaches the message bar, which is what
            // #73 bounds.
            Err(_) => warnings.push(format!(
                "{}: [profile.start] slot {id} does not carry a valid path",
                path.display()
            )),
        }
    }
}

/// Loads and merges every layer (ADR 0007/0035).
///
/// # Errors
/// [`ConfigError`] naming the offending file; an ABSENT layer is not an
/// error.
// `too_many_lines`: this is a merge by LAYERS, and the order of the
// assignments IS the semantics (the last layer wins, except for what the
// project carve-out excludes). Splitting it into helpers that passed around
// fifteen output parameters would hide exactly that, and would trade one
// lint for another (`too_many_arguments`). What HAS been pulled out are the
// decisions with a name of their own: `parse_layer`, `merge_quick_search` and
// the `merge_*_layer`s.
#[expect(
    clippy::too_many_lines,
    reason = "one pass per layer; what has a name of its own is already out"
)]
pub fn load(layers: &Layers) -> Result<CommonConfig, ConfigError> {
    let mut preset: Option<String> = None;
    let mut ui_lang: Option<String> = None;
    let mut ui_theme: Option<String> = None;
    let mut ui_theme_light: Option<String> = None;
    let mut ui_theme_dark: Option<String> = None;
    let mut quick_search = QuickSearch::default();
    let mut ui_font: Option<String> = None;
    let mut ui_mono_font: Option<String> = None;
    let mut ui_font_size: Option<f32> = None;
    let mut ui_reduce_motion: Option<bool> = None;
    let mut ui_confirm_quit = ConfirmQuit::default();
    let (mut ui_show_hidden, mut ui_mouse, mut ui_menu_bar, mut ui_panel_bar) =
        (None, None, None, None);
    let mut ui_parent_entry = None;
    let mut ui_alt_menu = None;
    let mut ui_editor: Option<Vec<String>> = None;
    let mut ui_editor_detached: Option<bool> = None;
    let mut ui_diff: Option<Vec<String>> = None;
    let mut ui_diff_detached: Option<bool> = None;
    let mut ui_layout: Option<String> = None;
    let mut ui_columns = ColumnsConfig::default();
    let mut ui_chrome = UiChrome::default();
    let mut ui_status_plugins: Vec<(String, String)> = Vec::new();
    let mut daemon = crate::DaemonSettings::default();
    let mut log = crate::LogSettings::default();
    let mut hotlist: Vec<HotlistItem> = Vec::new();
    let mut archive = crate::ArchiveSettings::default();
    let mut ai = AiSettings::default();
    let mut sources = Vec::new();
    let mut project_warnings: Vec<String> = Vec::new();
    let mut profile_warnings: Vec<String> = Vec::new();
    let mut profile_title: Option<String> = None;
    let mut profile_start: std::collections::BTreeMap<u32, norte_proto::VPath> =
        std::collections::BTreeMap::new();
    for (dir, kind) in &layers.dirs {
        let norte = dir.join("norte.toml");
        if let Some(raw) = schema::read_optional(&norte)? {
            let parsed: NorteToml = match parse_layer(&raw, &norte, *kind) {
                Ok(Some(p)) => p,
                // Una capa de PROYECTO que no parsea se SALTA, con su motivo.
                Ok(None) => {
                    project_warnings.push(layer_error(&raw, &norte).to_string());
                    continue;
                }
                Err(e) => return Err(e),
            };
            if *kind == Layer::Project {
                project_warnings.extend(carve_out_warnings(&parsed, &norte, *kind));
            }
            if *kind == Layer::Profile {
                profile_warnings.extend(carve_out_warnings(&parsed, &norte, *kind));
                merge_profile_section(
                    &mut profile_title,
                    &mut profile_start,
                    &parsed.profile,
                    &norte,
                    &mut profile_warnings,
                );
            } else if parsed.profile.title.is_some() || !parsed.profile.start.is_empty() {
                profile_warnings.push(format!(
                    "{}: [profile] solo significa algo dentro de profiles/<nombre>/",
                    norte.display()
                ));
            }
            merge_ui_flags(
                &mut ui_show_hidden,
                &mut ui_mouse,
                &mut ui_alt_menu,
                &mut ui_menu_bar,
                &mut ui_panel_bar,
                &mut ui_parent_entry,
                &mut ui_layout,
                &parsed.ui,
            );
            // `keymap.preset` is NOT honored from a project (#260). It is
            // bounded to the seven built-in presets, so it is not code
            // execution — but the presets DISAGREE about what each key does:
            // `far` binds `shift+delete` to `pane.delete` and `orthodox`
            // binds `shift+f8` to `pane.delete-permanent`. A hostile
            // repository would silently choose which key deletes, and
            // "choosing the keyboard layout" is not presentation: it is
            // deciding what happens when the reader presses something. A
            // PROFILE does choose it: it is the user's own file, not a
            // foreign repository's.
            if is_user_owned_layer(*kind)
                && let Some(p) = parsed.keymap.preset
            {
                preset = Some(p);
            }
            // Before everything that MOVES fields out of `parsed.ui`.
            merge_ui_chrome(&mut ui_chrome, &parsed.ui, &norte)?;
            if let Some(ids) = &parsed.ui.status_plugins {
                ui_status_plugins =
                    parse_status_plugins(ids).map_err(|message| ConfigError::Toml {
                        path: norte.clone(),
                        message: message.to_owned(),
                    })?;
            }
            if let Some(l) = parsed.ui.lang {
                ui_lang = Some(l);
            }
            if let Some(th) = parsed.ui.theme {
                ui_theme = Some(th);
            }
            if let Some(th) = parsed.ui.theme_light {
                ui_theme_light = Some(th);
            }
            if let Some(th) = parsed.ui.theme_dark {
                ui_theme_dark = Some(th);
            }
            if let Some(qs) = &parsed.ui.quick_search {
                quick_search =
                    merge_quick_search(quick_search, qs, &norte, *kind, &mut project_warnings)?;
            }
            merge_ui_fonts(
                &mut ui_font,
                &mut ui_mono_font,
                &mut ui_font_size,
                &mut ui_reduce_motion,
                parsed.ui.font,
                parsed.ui.mono_font,
                parsed.ui.font_size,
                parsed.ui.reduce_motion,
                &norte,
            )?;
            if let Some(cq) = &parsed.ui.confirm_quit {
                ui_confirm_quit = parse_confirm_quit(cq, &norte)?;
            }
            if let Some(cols) = &parsed.ui.columns {
                merge_ui_columns(&mut ui_columns, cols, &norte)?;
            }
            // TODO what follows is OUT of the project layer's scope, and used
            // to be the same `if` written five times with its reason
            // repeated; once, with the five reasons together:
            //
            // - **hotlist** (spec 2026-07-18, decision 3): a foreign repo
            //   does not inject favorites into the user's session. A PROFILE
            //   does bring them (spec 2026-08-26, D2), so it goes through
            //   `is_user_owned_layer` and not the `if` below.
            // - **`[archive]`** (#95.2): these are the anti-bomb limits, and
            //   RAISING them disarms the protection right where hostile
            //   containers live.
            // - **`[daemon]`** (review MAJOR-1): does not redirect the
            //   core's transport to a foreign socket.
            // - **`[ai]`** (ADR 0035 decision 3): does not enable AI or
            //   redirect its providers.
            // - **`[log]`** (roadmap item 9): does not decide where this
            //   process writes.
            //
            // The UI SCALARS (quick_search, theme, lang) ARE honored from a
            // project: they are presentation, and none of them launches,
            // writes, or redirects anything. That is the line, and
            // `keymap.preset` falls on the other side of it (#260): choosing
            // which key deletes is not presentation. It is filtered where it
            // is read, above.
            //
            // And the four sections below go through
            // `governs_outside_presentation`, which is written in the
            // POSITIVE: the `!= Layer::Project` that used to be here granted
            // all of this to any new `Layer` variant with nobody deciding to.
            // The **hotlist** is split off from the rest in 0079: a profile
            // DOES bring its own favorites — it is the user's own file, and
            // carrying them is half the reason a workspace exists — while
            // the four sections below remain nobody's but system and user's.
            if is_user_owned_layer(*kind) {
                for entry in parsed.hotlist {
                    merge_hotlist_entry(&mut hotlist, entry);
                }
            }
            if governs_outside_presentation(*kind) {
                // The EDITOR too, and for the same reason as `[daemon]`: it
                // names a program that gets run, so a foreign repo does not
                // choose what runs when F4 is pressed. Same line that keeps
                // `keymap.preset` out — choosing which key deletes is not
                // presentation, and choosing which binary launches, even
                // less so.
                ui_editor = parsed.ui.editor.clone().or(ui_editor);
                ui_editor_detached = parsed.ui.editor_detached.or(ui_editor_detached);
                // The COMPARATOR (#312) comes in through the same door as
                // the editor: it is another program that gets run.
                ui_diff = parsed.ui.diff.clone().or(ui_diff);
                ui_diff_detached = parsed.ui.diff_detached.or(ui_diff_detached);
                archive.merge(parsed.archive);
                daemon.merge(parsed.daemon);
                log.merge(parsed.log);
                merge_ai_layer(&mut ai, parsed.ai, &norte)?;
            }
            sources.push(norte);
        }
    }
    Ok(CommonConfig {
        preset: preset.unwrap_or_else(|| schema::DEFAULT_PRESET.to_owned()),
        ui_lang,
        ui_theme,
        ui_theme_light,
        ui_theme_dark,
        quick_search,
        ui_font,
        ui_mono_font,
        ui_font_size,
        ui_reduce_motion,
        ui_confirm_quit,
        ui_show_hidden,
        ui_layout,
        ui_mouse,
        ui_alt_menu,
        ui_menu_bar,
        ui_panel_bar,
        ui_parent_entry,
        ui_editor,
        ui_editor_detached,
        ui_diff,
        ui_diff_detached,
        ui_columns,
        ui_chrome,
        ui_status_plugins,
        daemon,
        log,
        hotlist,
        archive,
        ai,
        sources,
        project_warnings,
        profile_warnings,
        profile_title,
        profile_start,
    })
}

/// History+hotlist tests (spec 2026-07-18, navTC T2) + `[ai]` (ADR
/// 0035): a new mod alongside `schema.rs`'s `toml_diag_tests` (do not
/// reuse it — that mod is only for the compact `#73` diagnostic).
/// `persist_unset`: the reverse of `persist_set`, and what is behind
/// "reset" on the settings screen.
#[cfg(test)]
mod unset_tests {
    use super::*;

    #[test]
    fn removing_a_key_deletes_it_and_leaves_its_neighbors() {
        let dir = tempfile::tempdir().unwrap();
        persist_set(dir.path(), "ui", "theme", toml_edit::Value::from("nord")).unwrap();
        persist_set(dir.path(), "ui", "font_size", toml_edit::Value::from(18)).unwrap();
        let out = persist_unset(dir.path(), "ui", "theme").unwrap();
        assert!(out.changed);
        let s = std::fs::read_to_string(&out.path).unwrap();
        assert!(!s.contains("theme"), "{s}");
        assert!(s.contains("font_size"), "the neighbor stays: {s}");
    }

    #[test]
    fn removing_what_is_not_there_writes_nothing_and_creates_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = persist_unset(dir.path(), "ui", "theme").unwrap();
        assert!(!out.changed, "a file that is not there is a no-op");
        assert!(!out.path.exists(), "and it does not create it");
        // With a file, but without that key: same thing.
        persist_set(dir.path(), "ui", "font_size", toml_edit::Value::from(18)).unwrap();
        let out = persist_unset(dir.path(), "ui", "theme").unwrap();
        assert!(!out.changed);
        let out = persist_unset(dir.path(), "keymap", "preset").unwrap();
        assert!(!out.changed, "a section that is not there either");
    }

    /// Setting and removing leaves the file as it was. If this fails,
    /// resetting dirties `norte.toml` a little on every round.
    #[test]
    fn setting_and_removing_is_the_identity() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "# my config\n[ui]\nfont_size = 18 # the size\n",
        )
        .unwrap();
        let before = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        persist_set(dir.path(), "ui", "theme", toml_edit::Value::from("nord")).unwrap();
        persist_unset(dir.path(), "ui", "theme").unwrap();
        let after = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(before, after, "comments and formatting included");
    }

    /// A section that ends up empty is KEPT: deleting it changes the file
    /// more than was asked.
    #[test]
    fn the_empty_section_stays() {
        let dir = tempfile::tempdir().unwrap();
        persist_set(dir.path(), "ui", "theme", toml_edit::Value::from("nord")).unwrap();
        persist_unset(dir.path(), "ui", "theme").unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(s.contains("[ui]"), "{s}");
    }

    /// The same shape guard as `persist_set`: a `[ui]` that is not a table is
    /// refused instead of being indexed, which would panic the caller's
    /// background thread.
    #[test]
    fn a_section_that_is_not_a_table_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "ui = 3\n").unwrap();
        let e = persist_unset(dir.path(), "ui", "theme").unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    }
}

#[cfg(test)]
mod hotlist_tests {
    use super::*;

    #[test]
    fn hotlist_round_trip_preserves_comments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "# my config\n[ui]\ntheme = \"nord\" # theme\n",
        )
        .unwrap();
        persist_hotlist_add(dir.path(), "work", "file:///home/o/work").unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(s.contains("# my config"), "comments intact: {s}");
        assert!(s.contains("[[hotlist]]"), "{s}");
        persist_hotlist_remove(dir.path(), "work").unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(!s.contains("work"), "{s}");
    }

    #[test]
    fn hotlist_add_replaces_if_the_name_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        persist_hotlist_add(dir.path(), "work", "file:///a").unwrap();
        persist_hotlist_add(dir.path(), "work", "file:///b").unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(
            s.matches("work").count(),
            1,
            "a single entry, not duplicated: {s}"
        );
        assert!(s.contains("file:///b"), "{s}");
        assert!(!s.contains("file:///a"), "{s}");
    }

    #[test]
    fn hotlist_remove_of_a_nonexistent_name_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        persist_hotlist_add(dir.path(), "work", "file:///a").unwrap();
        let path = dir.path().join("norte.toml");
        // Comment by hand: if the no-op rewrote the file, toml_edit could
        // reformat it identically — the strong test is not "does not fail",
        // it is "the CONTENT does not change by a single byte" (review
        // MINOR-1: mtime is flaky due to FS granularity, content is not).
        let mut s = std::fs::read_to_string(&path).unwrap();
        s.push_str("# manual note\n");
        std::fs::write(&path, &s).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        // Must not fail even though "ghost" does not exist (documented:
        // no-op).
        persist_hotlist_remove(dir.path(), "ghost").unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            before, after,
            "no-op does not rewrite: byte-identical content"
        );
        assert!(after.contains("work"), "{after}");
    }

    #[test]
    fn hotlist_remove_with_no_hotlist_section_is_a_no_op_and_does_not_rewrite() {
        // `norte.toml` exists but WITHOUT any `[[hotlist]]` at all: the no-op
        // must not touch the file either (same MINOR-1).
        let dir = tempfile::tempdir().unwrap();
        let content = "# no hotlist\n[ui]\ntheme = \"nord\"\n";
        std::fs::write(dir.path().join("norte.toml"), content).unwrap();
        persist_hotlist_remove(dir.path(), "whatever").unwrap();
        let after = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(content, after, "with no `hotlist`: content untouched");
    }

    #[test]
    fn hotlist_loads_from_every_layer_except_project() {
        // Two dirs: `User` layer with one entry, `Project` layer with
        // another — the project one must NOT get in (spec: "a foreign repo
        // does not inject favorites"), and the user one must.
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[[hotlist]]\nname = \"home\"\npath = \"file:///home/o\"\n",
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("norte.toml"),
            "[[hotlist]]\nname = \"foreign-repo\"\npath = \"file:///tmp/x\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (user.path().to_path_buf(), Layer::User),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.hotlist.len(), 1, "only the user's: {:?}", cfg.hotlist);
        assert_eq!(cfg.hotlist[0].name, "home");
        assert_eq!(
            cfg.hotlist[0].target.as_ref().unwrap(),
            &VPath::parse("file:///home/o").unwrap()
        );
        assert!(
            cfg.project_warnings.iter().any(|w| w.contains("[hotlist]")),
            "and it IS SAID that it does not get in: {:?}",
            cfg.project_warnings
        );
    }

    /// What a foreign repository asks for and is not applied gets SAID, same
    /// as in a profile. It used to be silently dropped: it failed closed, but
    /// the `.norte.toml`'s owner had no way to know why their transport,
    /// their editor or their key preset were not changing.
    #[test]
    fn a_project_that_asks_for_what_it_does_not_decide_warns() {
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("norte.toml"),
            "[daemon]\nsocket = \"/tmp/foreign.sock\"\n\
             [keymap]\npreset = \"vim\"\n\
             [ui]\ndiff = [\"meld\"]\ntheme = \"nord\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (user.path().to_path_buf(), Layer::User),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.daemon.socket, None, "not applied");
        for what in ["[daemon]", "keymap", "diff"] {
            assert!(
                cfg.project_warnings.iter().any(|w| w.contains(what)),
                "missing the warning for {what}: {:?}",
                cfg.project_warnings
            );
        }
        assert!(
            !cfg.project_warnings.iter().any(|w| w.contains("theme")),
            "presentation IS decided by a project, and no warning is raised about it"
        );
    }

    #[test]
    fn hotlist_invalid_entry_degrades_per_entry() {
        // A path that does not parse as a VPath (missing scheme) does not
        // bring down the load: the entry survives with `target = Err(...)`,
        // and the other entries of the same layer load normally.
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[[hotlist]]\nname = \"broken\"\npath = \"not-a-wire-path\"\n\n\
             [[hotlist]]\nname = \"healthy\"\npath = \"file:///ok\"\n",
        )
        .unwrap();
        // The entry goes in a `User` layer (which DOES contribute hotlist);
        // the `Project` layer (a foreign repo) is added empty to check that
        // its absence of favorites does not alter the result.
        let project = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (user.path().to_path_buf(), Layer::User),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("the load does NOT fail over one broken entry");
        assert_eq!(cfg.hotlist.len(), 2);
        let broken = cfg.hotlist.iter().find(|h| h.name == "broken").unwrap();
        assert_eq!(
            broken.target.as_ref().err().map(String::as_str),
            Some(ERR_INVALID_PATH)
        );
        let healthy = cfg.hotlist.iter().find(|h| h.name == "healthy").unwrap();
        assert!(healthy.target.is_ok());
    }

    #[test]
    fn hotlist_duplicate_name_across_layers_the_later_layer_wins() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[[hotlist]]\nname = \"work\"\npath = \"file:///old\"\n",
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[[hotlist]]\nname = \"work\"\npath = \"file:///new\"\n",
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.hotlist.len(), 1, "same name, one entry");
        assert_eq!(
            cfg.hotlist[0].target.as_ref().unwrap(),
            &VPath::parse("file:///new").unwrap(),
            "the later layer (user) wins over system"
        );
    }

    /// review MINOR-2: dedup by `name` also applies WITHIN the SAME layer —
    /// TOML does not stop `[[hotlist]] name = "..."` repeating twice in the
    /// same array; the last appearance wins (see `merge_hotlist_entry`'s
    /// rustdoc).
    #[test]
    fn hotlist_duplicate_name_within_the_same_layer_the_last_appearance_wins() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[[hotlist]]\nname = \"work\"\npath = \"file:///old\"\n\n\
             [[hotlist]]\nname = \"work\"\npath = \"file:///new\"\n",
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (user.path().to_path_buf(), Layer::User),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.hotlist.len(), 1, "same name intra-layer, one entry");
        assert_eq!(
            cfg.hotlist[0].target.as_ref().unwrap(),
            &VPath::parse("file:///new").unwrap(),
            "the last appearance within the layer wins"
        );
    }

    /// The Project layer NEVER chooses which executable gets launched: a
    /// repository bringing its own `.norte.toml` with `[archive] rar_delegate`
    /// would be arbitrary code execution just by entering the directory. Same
    /// fail-closed rule as the rest of `[archive]`, and sharper here.
    #[test]
    fn rar_delegate_from_the_project_layer_is_ignored() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[archive]\nrar_delegate = \"/usr/bin/7z\"\n",
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("norte.toml"),
            "[archive]\nrar_delegate = \"/tmp/evil\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (user.path().to_path_buf(), Layer::User),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(
            cfg.archive.rar_delegate.as_deref(),
            Some("/usr/bin/7z"),
            "the Project layer never chooses the executable"
        );
    }

    /// Encoding pin LOW-1a: a hostile `name` (a quote, a newline, an embedded
    /// `[[hotlist]]` and a bidi override) survives the add → load round trip
    /// BYTE-IDENTICAL as ONE single entry — `toml_edit` escapes, it never
    /// injects TOML — and that same key removes it with remove.
    #[test]
    fn hotlist_round_trip_hostile_name_byte_identical() {
        let user = tempfile::tempdir().unwrap();
        let name = "fa\"vo\n[[hotlist]]\u{202E}rito";
        persist_hotlist_add(user.path(), name, "file:///x").unwrap();
        let project = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (user.path().to_path_buf(), Layer::User),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("the hostile name does not break the TOML");
        assert_eq!(cfg.hotlist.len(), 1, "ONE entry, no injection");
        assert_eq!(cfg.hotlist[0].name, name, "byte-identical name");
        persist_hotlist_remove(user.path(), name).unwrap();
        let cfg = load(&layers).expect("loads after remove");
        assert!(cfg.hotlist.is_empty(), "the hostile key removes its entry");
    }

    /// Encoding pin LOW-1b: the wire form of a `VPath` with a non-UTF8
    /// segment (0xFF 0xFE) round-trips add → load with `target` Ok and exact
    /// bytes.
    #[test]
    fn hotlist_round_trip_non_utf8_path_exact_bytes() {
        let user = tempfile::tempdir().unwrap();
        let vp = VPath::parse("file:///%FF%FE").unwrap();
        persist_hotlist_add(user.path(), "bin", &vp.to_wire()).unwrap();
        let project = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (user.path().to_path_buf(), Layer::User),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("loads");
        let target = cfg.hotlist[0].target.as_ref().expect("target Ok");
        assert_eq!(target, &vp);
        assert_eq!(
            target.file_name().unwrap().as_bytes(),
            &[0xFF, 0xFE],
            "the raw bytes survive the round trip through TOML"
        );
    }

    #[test]
    fn quick_search_valid_values() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nquick_search = \"jump\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.quick_search, QuickSearch::Jump);
    }

    #[test]
    fn quick_search_default_is_filter() {
        let cfg = load(&Layers { dirs: vec![] }).expect("loads");
        assert_eq!(cfg.quick_search, QuickSearch::Filter);
    }

    #[test]
    fn ui_fonts_load_and_validate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nfont = \"Inter\"\nmono_font = \"JetBrains Mono\"\nfont_size = 15.5\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.ui_font.as_deref(), Some("Inter"));
        assert_eq!(cfg.ui_mono_font.as_deref(), Some("JetBrains Mono"));
        assert!((cfg.ui_font_size.unwrap() - 15.5).abs() < f32::EPSILON);
    }

    /// `[ui.columns]` (#108): validated sort (closed vocabulary, invalid =
    /// error with the path), raw ids last-wins, schemes merged by key with
    /// the last one winning per field.
    #[test]
    fn ui_columns_loads_validates_and_merges() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[ui.columns]\ndefault = [\"name\", \"size\"]\nsort = { column = \"mtime\", dir = \"desc\" }\n[ui.columns.scheme.sftp]\ncolumns = [\"name\", \"attr:posix.mode\"]\n",
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[ui.columns.scheme.sftp]\nsort = { column = \"size\" }\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(
            cfg.ui_columns.default_columns.as_deref(),
            Some(&["name".to_owned(), "size".to_owned()][..])
        );
        assert_eq!(
            cfg.ui_columns.sort,
            Some(SortChoice {
                column: SortColumnKey::Mtime,
                descending: true,
                dirs_first: true
            })
        );
        let sftp = &cfg.ui_columns.schemes["sftp"];
        assert_eq!(
            sftp.columns.as_deref(),
            Some(&["name".to_owned(), "attr:posix.mode".to_owned()][..]),
            "the user layer did not override it (it only brought sort)"
        );
        assert_eq!(
            sftp.sort,
            Some(SortChoice {
                column: SortColumnKey::Size,
                descending: false,
                dirs_first: true
            })
        );

        // Closed vocabulary: an invalid sort column is a load error.
        let bad = tempfile::tempdir().unwrap();
        std::fs::write(
            bad.path().join("norte.toml"),
            "[ui.columns]\nsort = { column = \"colour\" }\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(bad.path().to_path_buf(), Layer::User)],
        };
        assert!(load(&layers).is_err());
    }

    /// Loads a single User layer from a TOML string (compact harness for the
    /// `[[ui.columns.spec]]` tests; same skeleton as
    /// `ui_columns_loads_validates_and_merges`).
    fn load_one_layer_result(toml: &str) -> Result<CommonConfig, ConfigError> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), toml).unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        load(&layers)
    }

    fn load_one_layer(toml: &str) -> CommonConfig {
        load_one_layer_result(toml).expect("loads")
    }

    /// `[[ui.columns.spec]]` (#108 7b): parsing a single layer — global spec
    /// by id + scheme spec that coexists (precedence at RESOLVE time is the
    /// frontend's; here it is only pinned that both maps arrive).
    #[test]
    fn ui_columns_spec_loads_valid_and_scheme_precedence() {
        let toml = r#"
[[ui.columns.spec]]
id = "size"
format = "si"
header = "Weight"
width = { fixed = 9 }

[[ui.columns.spec]]
id = "kind"
align = "left"

[[ui.columns.scheme.sftp.spec]]
id = "size"
format = "exact"
"#;
        let cfg = load_one_layer(toml);
        let g = cfg.ui_columns.specs.get("size").expect("global size spec");
        assert_eq!(g.format.as_deref(), Some("si"));
        assert_eq!(g.header.as_deref(), Some("Weight"));
        assert_eq!(g.width, Some(WidthChoice::Fixed(9)));
        assert_eq!(
            cfg.ui_columns.specs.get("kind").and_then(|s| s.align),
            Some(AlignChoice::Left)
        );
        let sc = cfg.ui_columns.schemes.get("sftp").expect("scheme");
        assert_eq!(
            sc.specs.get("size").and_then(|s| s.format.as_deref()),
            Some("exact")
        );
    }

    /// The spec's CLOSED vocabularies (#108 7b): a typo/range is a load error
    /// (sort pattern), never a silent skip.
    #[test]
    fn ui_columns_spec_closed_vocabularies_fail_to_load() {
        for toml in [
            "[[ui.columns.spec]]\nid = \"size\"\nformat = \"sise\"\n",
            "[[ui.columns.spec]]\nid = \"size\"\nalign = \"middle\"\n",
            "[[ui.columns.spec]]\nid = \"size\"\nwidth = \"hugee\"\n",
            "[[ui.columns.spec]]\nid = \"size\"\nwidth = { fixed = 0 }\n",
            "[[ui.columns.spec]]\nid = \"size\"\nwidth = { fixed = 200 }\n",
            "[[ui.columns.spec]]\nformat = \"iec\"\n", // no id
        ] {
            assert!(
                load_one_layer_result(toml).is_err(),
                "should have failed: {toml}"
            );
        }
    }

    /// Serde behavior pin (#108 7b): `WidthSection` is `untagged`, and serde
    /// IGNORES `deny_unknown_fields` inside an untagged enum's struct
    /// variants — an extra field next to `fixed` is silently ignored (not an
    /// error nor a panic). Documented in `schema::WidthSection`'s rustdoc; if
    /// serde changes, this test warns.
    #[test]
    fn width_fixed_with_extra_field_is_serdes_behavior() {
        let cfg = load_one_layer(
            "[[ui.columns.spec]]\nid = \"size\"\nwidth = { fixed = 9, extra = 1 }\n",
        );
        assert_eq!(
            cfg.ui_columns.specs.get("size").and_then(|s| s.width),
            Some(WidthChoice::Fixed(9)),
            "extra field ignored, fixed survives"
        );
    }

    /// Merging specs across layers (#108 7b): last-wins PER FIELD by id —
    /// same rule as the rest of `[ui.columns]`.
    #[test]
    fn ui_columns_spec_merge_by_id_last_wins_per_field() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[[ui.columns.spec]]\nid = \"size\"\nformat = \"iec\"\nheader = \"A\"\n",
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[[ui.columns.spec]]\nid = \"size\"\nformat = \"si\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("loads");
        let s = cfg.ui_columns.specs.get("size").expect("size spec");
        assert_eq!(s.format.as_deref(), Some("si"), "user layer wins the field");
        assert_eq!(
            s.header.as_deref(),
            Some("A"),
            "a field not re-declared keeps the previous layer's"
        );
    }

    /// `[ui] show_hidden` (#107): last-wins, every layer — same
    /// presentation-only class as `reduce_motion`. Absent = None (the
    /// frontend shows everything).
    #[test]
    fn ui_show_hidden_loads_last_wins_and_absent_is_none() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[ui]\nshow_hidden = true\n",
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[ui]\nshow_hidden = false\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.ui_show_hidden, Some(false), "last-wins");

        let empty = tempfile::tempdir().unwrap();
        std::fs::write(empty.path().join("norte.toml"), "").unwrap();
        let layers = Layers {
            dirs: vec![(empty.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.ui_show_hidden, None, "absent = None (show everything)");
    }

    /// `[ui] mouse`: last-wins, every layer — same presentation-only class as
    /// `show_hidden`. Absent = None, which the frontend reads as CAPTURED
    /// (the default lives in the frontend, not here: the config
    /// distinguishes "did not say" from "said true", and only that way can an
    /// explicit `mouse = true` beat a `false` from an earlier layer).
    #[test]
    fn ui_mouse_loads_last_wins_and_absent_is_none() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(system.path().join("norte.toml"), "[ui]\nmouse = false\n").unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(user.path().join("norte.toml"), "[ui]\nmouse = true\n").unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.ui_mouse, Some(true), "last-wins");

        let empty = tempfile::tempdir().unwrap();
        std::fs::write(empty.path().join("norte.toml"), "").unwrap();
        let layers = Layers {
            dirs: vec![(empty.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.ui_mouse, None, "absent = None (the frontend captures)");
    }

    /// `[ui] reduce_motion` (G2 a11y override, spec §17): last-wins, honored
    /// from EVERY layer including Project — same presentation-only class as
    /// `ui_theme`/`ui_lang`, not the security-sensitive fail-closed carve-out
    /// `[archive]`/`[ai]`/hotlist get.
    #[test]
    fn ui_reduce_motion_loads_last_wins_every_layer() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[ui]\nreduce_motion = true\n",
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("norte.toml"),
            "[ui]\nreduce_motion = false\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(
            cfg.ui_reduce_motion,
            Some(false),
            "the Project layer wins (last-wins) and IS honored (presentation, not security)"
        );
    }

    #[test]
    fn ui_reduce_motion_absent_is_none() {
        let cfg = load(&Layers { dirs: vec![] }).expect("loads");
        assert_eq!(cfg.ui_reduce_motion, None);
    }

    /// S2: `[ui] confirm_quit` accepts the three documented values.
    #[test]
    fn confirm_quit_valid_values() {
        for (raw, expected) in [
            ("auto", ConfirmQuit::Auto),
            ("always", ConfirmQuit::Always),
            ("never", ConfirmQuit::Never),
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("norte.toml"),
                format!("[ui]\nconfirm_quit = \"{raw}\"\n"),
            )
            .unwrap();
            let layers = Layers {
                dirs: vec![(dir.path().to_path_buf(), Layer::User)],
            };
            let cfg = load(&layers).expect("loads");
            assert_eq!(cfg.ui_confirm_quit, expected, "raw={raw}");
        }
    }

    /// `[ui] status_plugins` (ADR 0137): pairs in order, last layer wins,
    /// and a malformed, repeated or fifth id names the file.
    #[test]
    fn status_plugins_loads_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nstatus_plugins = [\"plugin:git/branch\", \"plugin:net.x/status\"]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let c = load(&layers).expect("loads");
        assert_eq!(
            c.ui_status_plugins,
            vec![
                ("git".to_owned(), "branch".to_owned()),
                ("net.x".to_owned(), "status".to_owned())
            ]
        );
        assert!(
            load(&Layers { dirs: vec![] })
                .expect("loads")
                .ui_status_plugins
                .is_empty()
        );
        for bad in [
            "status_plugins = [\"git/branch\"]",
            "status_plugins = [\"plugin:git\"]",
            "status_plugins = [\"plugin:/branch\"]",
            "status_plugins = [\"plugin:git/\"]",
            "status_plugins = [\"plugin:git/a\", \"plugin:git/a\"]",
            "status_plugins = [\"plugin:a/a\", \"plugin:a/b\", \"plugin:a/c\", \
             \"plugin:a/d\", \"plugin:a/e\"]",
        ] {
            std::fs::write(dir.path().join("norte.toml"), format!("[ui]\n{bad}\n")).unwrap();
            let err = load(&layers).expect_err(bad);
            assert!(err.to_string().contains("norte.toml"), "{bad}: {err}");
        }
    }

    /// `[ui]` chrome: the six keys land, the two enums validate, the project
    /// layer is honored (presentation-only), and a bad value names the file.
    #[test]
    fn ui_chrome_loads_validates_and_honors_project() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[ui]\nkey_bar = false\npanel_bar_style = \"letters\"\ndate_format = \"iso\"\n\
             panel_bar_position = \"left\"\nstatus_items = [\"tasks\", \"position\"]\n\
             notice_seconds = 30\nhistory_size = 12\nsplash = \"home\"\n\
             processes_panel = \"manual\"\nimages = \"blocks\"\ndir_indicator = \"slash\"\n\
             titlebar = \"custom\"\n",
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("norte.toml"),
            "[ui]\npane_footer = false\ndialog_buttons = false\ndate_format = \"relative\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (user.path().to_path_buf(), Layer::User),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let c = load(&layers).expect("loads").ui_chrome;
        assert_eq!(c.key_bar, Some(false));
        assert_eq!(c.panel_bar_style(), PanelBarStyle::Letters);
        assert_eq!(c.panel_bar_position(), PanelBarPosition::Left);
        assert_eq!(c.titlebar(), Titlebar::Custom);
        assert_eq!(c.status_items().to_ids(), ["tasks", "position"]);
        assert_eq!(c.date_format(), DateFormat::Relative, "the last layer wins");
        assert_eq!(c.notice_seconds(), 30);
        assert_eq!(c.history_size(), 12);
        assert_eq!(c.splash(), SplashMode::Home);
        assert_eq!(c.processes_panel(), ProcessesPanel::Manual);
        assert_eq!(c.images(), Images::Blocks);
        assert_eq!(c.dir_indicator(), DirIndicator::Slash);
        assert!(!c.pane_footer());
        assert!(!c.dialog_buttons());

        let empty = load(&Layers { dirs: vec![] }).expect("loads").ui_chrome;
        assert_eq!(empty, UiChrome::default());
        assert!(empty.key_bar() && empty.pane_footer() && empty.dialog_buttons());
        assert_eq!(empty.panel_bar_style(), PanelBarStyle::Names);
        assert_eq!(empty.panel_bar_position(), PanelBarPosition::Auto);
        assert_eq!(
            empty.titlebar(),
            Titlebar::Native,
            "the desktop's own, by default"
        );
        assert_eq!(empty.status_items(), StatusItems::DEFAULT);
        assert_eq!(empty.date_format(), DateFormat::Smart);
        assert_eq!(empty.notice_seconds(), 8);
        assert_eq!(empty.history_size(), 30);
        assert_eq!(empty.splash(), SplashMode::Brief);
        assert_eq!(empty.processes_panel(), ProcessesPanel::Auto);
        assert_eq!(empty.images(), Images::Auto);
        assert_eq!(empty.dir_indicator(), DirIndicator::Auto);

        for bad in [
            "panel_bar_style = \"emoji\"",
            "panel_bar_position = \"right\"",
            "titlebar = \"frameless\"",
            "status_items = [\"git\"]",
            "status_items = [\"marks\", \"marks\"]",
            "date_format = \"unix\"",
            "notice_seconds = 601",
            "history_size = 4",
            "history_size = 65",
            "splash = \"always\"",
            "processes_panel = \"si\"",
            "images = \"si\"",
            "dir_indicator = \"arrow\"",
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("norte.toml"), format!("[ui]\n{bad}\n")).unwrap();
            let layers = Layers {
                dirs: vec![(dir.path().to_path_buf(), Layer::User)],
            };
            let err = load(&layers).expect_err(bad);
            assert!(matches!(err, ConfigError::Toml { .. }), "{bad}");
        }
    }

    #[test]
    fn images_reads_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "[ui]\nimages = \"kitty\"\n").unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let c = load(&layers).expect("loads").ui_chrome;
        assert_eq!(c.images(), Images::Kitty);
    }

    #[test]
    fn images_absent_is_auto() {
        let empty = load(&Layers { dirs: vec![] }).expect("loads").ui_chrome;
        assert_eq!(empty.images(), Images::Auto);
    }

    #[test]
    fn images_invalid_is_refused_with_a_reason() {
        // A value not on the list is NOT silently ignored: whoever wrote
        // "yes" wanted something, and starting as if nothing had been
        // written turns their mistake into a preference they never chose.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "[ui]\nimages = \"yes\"\n").unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let err = load(&layers).expect_err("must refuse an unknown value");
        let msg = err.to_string();
        assert!(msg.contains("images"), "the reason names the key: {msg}");
    }

    #[test]
    fn confirm_quit_default_is_auto() {
        let cfg = load(&Layers { dirs: vec![] }).expect("loads");
        assert_eq!(cfg.ui_confirm_quit, ConfirmQuit::Auto);
    }

    #[test]
    fn confirm_quit_invalid_value_is_a_load_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nconfirm_quit = \"sometimes\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let err = load(&layers).expect_err("broken config is an error (ADR 0007)");
        assert!(matches!(err, ConfigError::Toml { .. }));
    }

    /// `as_str` round-trips the three wire strings.
    #[test]
    fn confirm_quit_as_str() {
        assert_eq!(ConfirmQuit::Auto.as_str(), "auto");
        assert_eq!(ConfirmQuit::Always.as_str(), "always");
        assert_eq!(ConfirmQuit::Never.as_str(), "never");
    }

    /// ADR 0007: invalid config is a startup error WITH the culprit file — a
    /// `font_size` outside [8, 32] is not silently clamped (a different
    /// contract from [effects], which is theme data and does clamp).
    #[test]
    fn ui_font_size_out_of_range_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "[ui]\nfont_size = 4.0\n").unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        assert!(matches!(load(&layers), Err(ConfigError::Toml { .. })));
    }

    #[test]
    fn quick_search_invalid_value_is_a_load_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nquick_search = \"fly\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let err = load(&layers).expect_err("broken config is an error (ADR 0007)");
        assert!(matches!(err, ConfigError::Toml { .. }));
    }

    /// ADR 0035: [ai] merges across layers — scalars last-wins, denied
    /// prefixes UNION (a system deny survives a user layer) AND deduped (a
    /// prefix repeated across layers is not a distinct entry).
    #[test]
    fn ai_merge_scalars_last_wins_and_denied_union() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[ai]\nenabled = true\nlocal_only = true\n\
             denied_prefixes = [\"file:///etc\", \"file:///shared\"]\n",
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[ai]\nlocal_only = false\n\
             denied_prefixes = [\"file:///home/u/secret\", \"file:///shared\"]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert!(cfg.ai.enabled, "absent in user layer: inherits system");
        assert!(!cfg.ai.local_only, "present in user layer: user wins");
        let etc = VPath::parse("file:///etc").unwrap();
        let secret = VPath::parse("file:///home/u/secret").unwrap();
        let shared = VPath::parse("file:///shared").unwrap();
        assert_eq!(
            cfg.ai.denied_prefixes.len(),
            3,
            "UNION deduped: 3 distinct entries, `file:///shared` not doubled"
        );
        assert!(
            cfg.ai.denied_prefixes.contains(&etc),
            "system deny survives"
        );
        assert!(cfg.ai.denied_prefixes.contains(&secret), "user deny added");
        assert!(
            cfg.ai.denied_prefixes.contains(&shared),
            "shared deny present exactly once"
        );
    }

    /// [ai] from the project layer is ignored fail-closed — a hostile repo
    /// must not enable AI, declare a provider, nor add a denied prefix.
    #[test]
    fn ai_from_project_is_ignored() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("norte.toml"),
            "[ai]\nenabled = true\ndenied_prefixes = [\"file:///x\"]\n\n\
             [ai.providers.p]\nkind = \"ollama\"\nmodel = \"m\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(project.path().to_path_buf(), Layer::Project)],
        };
        let cfg = load(&layers).expect("loads");
        assert!(!cfg.ai.enabled);
        assert!(cfg.ai.providers.is_empty(), "project provider ignored");
        assert!(
            cfg.ai.denied_prefixes.is_empty(),
            "project denied_prefixes ignored"
        );
    }

    /// ADR 0035: providers merge BY NAME — a later layer redeclaring an
    /// existing name replaces just that entry, an untouched name from a
    /// lower layer survives, and `rename_provider` (a plain scalar) is
    /// last-present-wins independent of the providers map.
    #[test]
    fn ai_providers_merge_by_name_later_layer_wins() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[ai]\nrename_provider = \"x\"\n\n\
             [ai.providers.x]\nkind = \"ollama\"\nmodel = \"old\"\n\n\
             [ai.providers.y]\nkind = \"ollama\"\nmodel = \"system-only\"\n",
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[ai]\nrename_provider = \"y\"\n\n\
             [ai.providers.x]\nkind = \"ollama\"\nmodel = \"new\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.ai.providers.len(), 2, "both names survive");
        assert_eq!(
            cfg.ai.providers.get("x").unwrap().model,
            "new",
            "user layer redeclares x: later layer wins"
        );
        assert_eq!(
            cfg.ai.providers.get("y").unwrap().model,
            "system-only",
            "y untouched by user layer: survives"
        );
        assert_eq!(
            cfg.ai.rename_provider,
            Some("y".to_owned()),
            "scalar last-present-wins, independent of the providers map"
        );
    }

    /// `embed_provider` (M4-IA-2) is a plain `Option` scalar like
    /// `rename_provider`: last-present-wins across layers, and absent in
    /// every layer means `None` (no embeddings).
    #[test]
    fn ai_embed_provider_last_layer_wins() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[ai]\nembed_provider = \"ollama-local\"\n",
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[ai]\nembed_provider = \"other\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(
            cfg.ai.embed_provider,
            Some("other".to_owned()),
            "scalar last-present-wins"
        );

        // Absent in every layer: stays `None`.
        let empty = tempfile::tempdir().unwrap();
        std::fs::write(empty.path().join("norte.toml"), "[ai]\nenabled = true\n").unwrap();
        let layers = Layers {
            dirs: vec![(empty.path().to_path_buf(), Layer::System)],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.ai.embed_provider, None, "absent in all layers => None");
    }

    /// Review MAJOR-1: `[daemon]` from the project layer must NOT be
    /// honored — a hostile repo must not redirect the core transport (mode
    /// or socket) to an attacker-controlled endpoint.
    #[test]
    fn daemon_from_project_is_ignored() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("norte.toml"),
            "[daemon]\nmode = \"daemon\"\nsocket = \"/tmp/evil.sock\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(project.path().to_path_buf(), Layer::Project)],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(cfg.daemon.mode, None, "project mode ignored");
        assert_eq!(cfg.daemon.socket, None, "project socket ignored");
    }

    /// `[log]` is read from the machine and user layers, NEVER from the
    /// project one: a `norte.toml` arriving with a foreign repository cannot
    /// decide where this process writes its logs. Same fail-closed rule as
    /// `[daemon]` (review MAJOR-1) and for the same reason — redirecting a
    /// write is not presentation.
    #[test]
    fn log_from_project_is_ignored() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[log]\ndir = \"/from-user\"\nretain = 3\n",
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("norte.toml"),
            "[log]\ndir = \"/from-the-repo\"\nretain = 99\nformat = \"json\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (user.path().to_path_buf(), Layer::User),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("loads");
        assert_eq!(
            cfg.log.dir.as_deref(),
            Some(std::path::Path::new("/from-user")),
            "the user layer wins; the project one is not even looked at"
        );
        assert_eq!(cfg.log.retain, Some(3));
        assert_eq!(
            cfg.log.format,
            crate::schema::LogFormat::Text,
            "a repository does not decide the log's format either"
        );
    }

    /// Security review item 4 (C1, NIT F4): the persist helpers' "existing
    /// TOML doesn't parse" error must name the file but never echo
    /// `toml_edit`'s parse-error `Display`, which quotes the offending
    /// document line — a hostile line persisted earlier (#73 discipline)
    /// must not resurface verbatim in whatever shows this `io::Error` (the
    /// TUI status bar). Pinned across all three persist helpers with a
    /// planted line containing a bidi override + an embedded fake TOML
    /// header, deliberately broken syntax so the parse fails.
    #[test]
    fn persist_helpers_do_not_quote_toml_edits_raw_error() {
        let hostile = "not toml \u{202E}[[hotlist]]\u{202C} = [unterminated\n";
        for helper in ["theme", "hotlist_add", "hotlist_remove"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("norte.toml"), hostile).unwrap();
            let err = match helper {
                "theme" => persist_ui_theme_to(dir.path(), "nord").unwrap_err(),
                "hotlist_add" => persist_hotlist_add(dir.path(), "n", "file:///x").unwrap_err(),
                _ => persist_hotlist_remove(dir.path(), "n").unwrap_err(),
            };
            let msg = err.to_string();
            assert!(
                !msg.contains("hotlist") && !msg.contains('\u{202E}'),
                "{helper}: error must not echo the hostile document content: {msg:?}"
            );
            assert!(
                msg.contains("norte.toml"),
                "{helper}: error should still name the offending file: {msg:?}"
            );
        }
    }
}

/// Tests for [`persist_set`] (S2): the GENERIC form behind
/// `persist_ui_theme_to` — checks what that wrapper alone does not exercise
/// (an arbitrary section, a new file, a hostile value), WHILE
/// `hotlist_tests::persist_helpers_do_not_quote_toml_edits_raw_error` above
/// is the behavior proof that the wrapper is still identical to how it was.
#[cfg(test)]
mod persist_set_tests {
    use super::*;

    /// Round trip preserving already-existing comments — same rule as
    /// `hotlist_round_trip_preserves_comments`.
    #[test]
    fn persist_set_preserves_existing_comments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "# my config\n[ui]\ntheme = \"nord\" # theme\n",
        )
        .unwrap();
        persist_set(dir.path(), "ui", "lang", toml_edit::Value::from("es")).unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(s.contains("# my config"), "file comment: {s}");
        assert!(s.contains("# theme"), "key comment: {s}");
        assert!(s.contains("lang = \"es\""), "{s}");
        assert!(s.contains("theme = \"nord\""), "previous value intact: {s}");
    }

    /// ABSENT file: `persist_set` creates it (and the dir, if that is
    /// missing too).
    #[test]
    fn persist_set_creates_the_file_if_it_does_not_exist() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("subdir/does-not-exist-yet");
        let path = persist_set(&dir, "keymap", "preset", toml_edit::Value::from("vim")).unwrap();
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("preset = \"vim\""), "{s}");
    }

    /// The `[section]` table is born EXPLICIT in a new file — not
    /// `section.key = …` as an implicit dotted-key, which would be
    /// unreadable/non-idiomatic for a file the user may edit by hand.
    #[test]
    fn persist_set_creates_the_section_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let path = persist_set(dir.path(), "ai", "enabled", toml_edit::Value::from(true)).unwrap();
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("[ai]"), "EXPLICIT section: {s}");
        assert!(
            !s.contains("ai.enabled"),
            "must not degrade to an implicit dotted-key: {s}"
        );
    }

    /// Encoding pin: a hostile string value (a quote, a newline, an embedded
    /// TOML header and a bidi override) round-trips escaped and
    /// byte-identical — `toml_edit` escapes, it never injects TOML — same
    /// rule as `hotlist_round_trip_hostile_name_byte_identical`.
    #[test]
    fn persist_set_hostile_value_round_trips_escaped() {
        let dir = tempfile::tempdir().unwrap();
        let hostile = "fa\"vo\n[[evil]]\u{202E}rito";
        persist_set(dir.path(), "ui", "font", toml_edit::Value::from(hostile)).unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("the hostile value does not break the TOML");
        assert_eq!(
            cfg.ui_font.as_deref(),
            Some(hostile),
            "byte-identical value after the round trip"
        );
    }

    /// An already-existing `[section]` key is REPLACED (same key, new
    /// value) — a single occurrence in the final file, not a duplicate.
    #[test]
    fn persist_set_replaces_an_existing_key() {
        let dir = tempfile::tempdir().unwrap();
        persist_set(dir.path(), "ui", "theme", toml_edit::Value::from("nord")).unwrap();
        persist_set(
            dir.path(),
            "ui",
            "theme",
            toml_edit::Value::from("gruvbox-dark"),
        )
        .unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(s.matches("theme").count(), 1, "a single key: {s}");
        assert!(s.contains("gruvbox-dark"), "{s}");
        assert!(!s.contains("\"nord\""), "{s}");
    }

    /// Review S, I1: an existing `[section]` but with SCALAR shape (`ui = 3`,
    /// e.g. a `norte.toml` hand-edited between sessions) is a CLEAN
    /// `Err(InvalidData)` — before this fix, `toml_edit` indexed that entry
    /// and panicked (`IndexMut` on a non-table `Item::Value` returns `None`
    /// internally, `.expect()`d by the index operator). A panic here would
    /// bring down the background thread calling `persist_set` (GUI: takes the
    /// process with it; TUI: a silent `JoinError`). The file stays INTACT
    /// (the guard is read-only, before any write).
    #[test]
    fn persist_set_scalar_section_is_err_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "ui = 3\n").unwrap();
        let err = persist_set(dir.path(), "ui", "theme", toml_edit::Value::from("nord"))
            .expect_err("a scalar [ui] must be refused, not panic");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(s, "ui = 3\n", "the file is untouched on the error path");
    }

    /// Same guard, array-of-tables shape (`[[ui]]`) — just as "not a table"
    /// for our purposes even though `toml_edit` models it as its own type
    /// (`Item::ArrayOfTables`), not as a scalar `Item::Value`.
    #[test]
    fn persist_set_array_of_tables_section_is_err_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "[[ui]]\nx = 1\n").unwrap();
        let err = persist_set(dir.path(), "ui", "theme", toml_edit::Value::from("nord"))
            .expect_err("a [[ui]] array-of-tables must be refused, not panic");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// The guard's positive counterpart: an inline `[section]`
    /// (`ui = { theme = "x" }`) IS table-like for `toml_edit` — the guard
    /// must not refuse it (pin: keeps an overly strict guard from breaking a
    /// shape the library indexes without a problem).
    #[test]
    fn persist_set_inline_table_section_is_not_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "ui = { theme = \"nord\" }\n").unwrap();
        persist_set(dir.path(), "ui", "lang", toml_edit::Value::from("es"))
            .expect("inline table: the guard must not refuse it");
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(s.contains("lang"), "{s}");
    }
}

/// Tests de [`persist_columns`] (#108 7a): el PRIMER valor array que el
/// persister writes ever — the round trip through the real `load` is the
/// contract's pin (the sort's key names are EXACTLY what
/// `parse_sort_section` parses: `column`/`dir`/`dirs_first`).
#[cfg(test)]
mod persist_columns_tests {
    use super::*;

    /// No scheme → `[ui.columns] default + sort`, and the real `load`
    /// rereads it identically (opaque ids included — the picker never cleans
    /// up the config).
    #[test]
    fn persist_columns_default_round_trips_through_load() {
        let dir = tempfile::tempdir().unwrap();
        persist_columns(
            dir.path(),
            None,
            &[
                "name".to_owned(),
                "mtime".to_owned(),
                "attr:posix.mode".to_owned(),
            ],
            Some(PersistSort {
                column: "mtime",
                descending: true,
                dirs_first: true,
            }),
        )
        .expect("write");
        // The leaf is emitted EXPLICIT (a readable `[ui.columns]`), not as
        // implicit dotted-keys — same rule as `persist_set`.
        let text = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(text.contains("[ui.columns]"), "explicit leaf: {text}");
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("load");
        assert_eq!(
            cfg.ui_columns.default_columns.as_deref(),
            Some(
                &[
                    "name".to_owned(),
                    "mtime".to_owned(),
                    "attr:posix.mode".to_owned()
                ][..]
            )
        );
        assert_eq!(
            cfg.ui_columns.sort,
            Some(SortChoice {
                column: SortColumnKey::Mtime,
                descending: true,
                dirs_first: true
            })
        );
    }

    /// Pin M1/L1 (review 7a): the persister's FIRST ARRAY write path with a
    /// hostile id (an embedded quote + newline + section header + RLO).
    /// `toml_edit` escapes it (multi-line escapes), never injects TOML: the
    /// real `load` rereads the list BYTE-IDENTICAL, the hostile one is still
    /// ONE element, and the config gains no artifacts (schemes intact). The
    /// existing hostile pin
    /// (`hotlist_round_trip_hostile_name_byte_identical`) only covered
    /// scalar Values.
    #[test]
    fn persist_columns_hostile_id_round_trips_through_load() {
        let dir = tempfile::tempdir().unwrap();
        let hostile = "x\"]\n[evil]\u{202E}";
        persist_columns(
            dir.path(),
            None,
            &["name".to_owned(), hostile.to_owned()],
            Some(PersistSort {
                column: "name",
                descending: false,
                dirs_first: true,
            }),
        )
        .expect("write");
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("the hostile id does not break the TOML");
        assert_eq!(
            cfg.ui_columns.default_columns.as_deref(),
            Some(&["name".to_owned(), hostile.to_owned()][..]),
            "the list round-trips byte-identical, no injection"
        );
        assert!(
            cfg.ui_columns.schemes.is_empty(),
            "no injected artifacts: {:?}",
            cfg.ui_columns.schemes
        );
    }

    /// With scheme → `[ui.columns.scheme.<s>] columns + sort`, preserving
    /// the file's comments and its previous content (`toml_edit`).
    #[test]
    fn persist_columns_scheme_writes_the_override_and_preserves_comments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "# my config\n[ui]\ntheme = \"default\"\n",
        )
        .expect("seed");
        persist_columns(
            dir.path(),
            Some("sftp"),
            &["name".to_owned(), "size".to_owned()],
            Some(PersistSort {
                column: "size",
                descending: false,
                dirs_first: true,
            }),
        )
        .expect("write");
        let text = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(text.contains("# my config"), "comments preserved: {text}");
        assert!(
            text.contains("theme = \"default\""),
            "previous content intact: {text}"
        );
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("load");
        let sc = cfg.ui_columns.schemes.get("sftp").expect("sftp override");
        assert_eq!(
            sc.columns.as_deref(),
            Some(&["name".to_owned(), "size".to_owned()][..])
        );
        assert_eq!(
            sc.sort,
            Some(SortChoice {
                column: SortColumnKey::Size,
                descending: false,
                dirs_first: true
            })
        );
    }

    /// ADR 0144: with no order to save (the reader sorted by an attribute the
    /// file cannot name), the list is written and the `sort` key that was
    /// already there stays EXACTLY as it was.
    #[test]
    fn persist_columns_with_no_order_does_not_touch_the_previous_sort() {
        let dir = tempfile::tempdir().unwrap();
        persist_columns(
            dir.path(),
            None,
            &["name".to_owned()],
            Some(PersistSort {
                column: "mtime",
                descending: true,
                dirs_first: false,
            }),
        )
        .expect("first write");
        persist_columns(
            dir.path(),
            None,
            &["name".to_owned(), "size".to_owned()],
            None,
        )
        .expect("second write");
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("load");
        assert_eq!(
            cfg.ui_columns.default_columns.as_deref(),
            Some(&["name".to_owned(), "size".to_owned()][..]),
            "the list IS written"
        );
        assert_eq!(
            cfg.ui_columns.sort,
            Some(SortChoice {
                column: SortColumnKey::Mtime,
                descending: true,
                dirs_first: false
            }),
            "the previous order intact"
        );
    }

    /// The guard's positive counterpart (a pin of the `TableLike` walk): a
    /// `[ui]` in INLINE shape (`ui = { theme = "nord" }`) passes
    /// `is_table_like` and the writer must WRITE THROUGH it — a walk via
    /// `as_table_mut` (only `Item::Table`) would refuse it — without losing
    /// the previous value.
    #[test]
    fn persist_columns_writes_through_an_inline_ui() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "ui = { theme = \"nord\" }\n").expect("seed");
        persist_columns(
            dir.path(),
            None,
            &["name".to_owned()],
            Some(PersistSort {
                column: "name",
                descending: false,
                dirs_first: true,
            }),
        )
        .expect("inline table: the guard must not refuse it");
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("load");
        assert_eq!(
            cfg.ui_columns.default_columns.as_deref(),
            Some(&["name".to_owned()][..])
        );
        assert_eq!(
            cfg.ui_theme.as_deref(),
            Some("nord"),
            "the previous inline value survives the write"
        );
    }

    /// Level-by-level shape guard (same rule as `persist_set`): a scalar
    /// level (`ui = 3`) is a CLEAN `Err(InvalidData)`, not a panic that would
    /// bring down the background thread — and the file stays intact.
    #[test]
    fn persist_columns_refuses_a_non_table_ui_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "ui = 3\n").expect("seed");
        let err = persist_columns(
            dir.path(),
            None,
            &["name".to_owned()],
            Some(PersistSort {
                column: "name",
                descending: false,
                dirs_first: true,
            }),
        )
        .expect_err("unexpected shape");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(s, "ui = 3\n", "the file is untouched on the error path");
    }

    /// #108 7b: with no previous entry, `persist_column_format` creates the
    /// `[[ui.columns.spec]]` with `id` + `format`.
    #[test]
    fn persist_column_format_creates_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        persist_column_format(dir.path(), "size", "si").expect("write");
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(
            s.contains("[[ui.columns.spec]]"),
            "AoT under [ui.columns]: {s}"
        );
        assert!(s.contains(r#"id = "size""#), "{s}");
        assert!(s.contains(r#"format = "si""#), "{s}");
    }

    /// Replacement BY ID: the existing entry keeps its OTHER fields (header)
    /// and the file's comments; a second entry for the same id never comes
    /// into being.
    #[test]
    fn persist_column_format_replaces_by_id_preserving_fields() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "# my config\n[[ui.columns.spec]]\nid = \"size\"\nheader = \"Weight\"\nformat = \"iec\"\n",
        )
        .expect("seed");
        persist_column_format(dir.path(), "size", "exact").expect("write");
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(s.contains("# my config"), "comments preserved: {s}");
        assert!(
            s.contains(r#"header = "Weight""#),
            "the other fields survive: {s}"
        );
        assert!(s.contains(r#"format = "exact""#), "{s}");
        assert!(!s.contains(r#"format = "iec""#), "no old entry: {s}");
        assert_eq!(
            s.matches(r#"id = "size""#).count(),
            1,
            "ONE entry per id: {s}"
        );
    }

    /// The real `load` rereads what was written: two ids → two specs with
    /// their format.
    #[test]
    fn persist_column_format_round_trips_through_load() {
        let dir = tempfile::tempdir().unwrap();
        persist_column_format(dir.path(), "size", "si").expect("size");
        persist_column_format(dir.path(), "mtime", "iso").expect("mtime");
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("load");
        assert_eq!(
            cfg.ui_columns
                .specs
                .get("size")
                .and_then(|sp| sp.format.as_deref()),
            Some("si")
        );
        assert_eq!(
            cfg.ui_columns
                .specs
                .get("mtime")
                .and_then(|sp| sp.format.as_deref()),
            Some("iso")
        );
    }

    /// `[ui] theme_light` / `theme_dark` (spec 2026-09-11, V6): load like
    /// `theme` — strings not validated here, the frontend resolves them —
    /// and a higher layer wins per key, without dragging the other one
    /// along.
    #[test]
    fn theme_light_and_theme_dark_load_and_the_higher_layer_wins_per_key() {
        let system = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[ui]\ntheme = \"nord\"\ntheme_light = \"gruvbox-light\"\ntheme_dark = \"gruvbox-dark\"\n",
        )
        .unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[ui]\ntheme_dark = \"catppuccin-mocha\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("load");
        assert_eq!(cfg.ui_theme.as_deref(), Some("nord"));
        assert_eq!(cfg.ui_theme_light.as_deref(), Some("gruvbox-light"));
        assert_eq!(
            cfg.ui_theme_dark.as_deref(),
            Some("catppuccin-mocha"),
            "the user layer only overrides the key it writes"
        );
    }

    /// Width shares its writer with format: it goes into the SAME entry for
    /// the id (a second one is not born), keeps the format that was there,
    /// and the real `load` returns it as `WidthChoice::Fixed`.
    #[test]
    fn persist_column_width_round_trips_through_load_and_keeps_the_format() {
        let dir = tempfile::tempdir().unwrap();
        persist_column_format(dir.path(), "size", "si").expect("format");
        persist_column_width(dir.path(), "size", 12).expect("width");
        persist_column_width(dir.path(), "size", 14).expect("width again");
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(s.matches(r#"id = "size""#).count(), 1, "ONE entry: {s}");
        assert!(s.contains("fixed = 14"), "{s}");
        assert!(!s.contains("fixed = 12"), "no old value: {s}");
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("load");
        let spec = cfg.ui_columns.specs.get("size").expect("spec");
        assert_eq!(spec.width, Some(WidthChoice::Fixed(14)));
        assert_eq!(spec.format.as_deref(), Some("si"));
    }

    /// MAJOR review 7b: with TWO hand-edited entries of the same id, the
    /// loader honors the LAST one (intra-layer last-wins-per-field merge) —
    /// the writer must update ALL of them or the reload reverts what was
    /// just saved. After persisting, both carry the new format and the real
    /// `load` returns the persisted value.
    #[test]
    fn persist_column_format_updates_every_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[[ui.columns.spec]]\nid = \"size\"\nformat = \"iec\"\n\n\
             [[ui.columns.spec]]\nid = \"size\"\nformat = \"exact\"\n",
        )
        .expect("seed");
        persist_column_format(dir.path(), "size", "si").expect("write");
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(
            s.matches(r#"format = "si""#).count(),
            2,
            "EVERY duplicate carries the new format: {s}"
        );
        assert!(!s.contains(r#"format = "iec""#), "{s}");
        assert!(!s.contains(r#"format = "exact""#), "{s}");
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("load");
        assert_eq!(
            cfg.ui_columns
                .specs
                .get("size")
                .and_then(|sp| sp.format.as_deref()),
            Some("si"),
            "the reload returns the persisted value, not the stale duplicate"
        );
    }

    /// The hotlist precedent's shape guard: a scalar `spec` = clean error,
    /// no panic and no touching the file.
    #[test]
    fn persist_column_format_refuses_a_non_array_spec_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "[ui.columns]\nspec = 3\n").expect("seed");
        let err = persist_column_format(dir.path(), "size", "si").expect_err("unexpected shape");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(
            s, "[ui.columns]\nspec = 3\n",
            "the file is untouched on the error path"
        );
    }
}

#[cfg(test)]
mod persist_atomicity_tests {
    use super::*;

    /// #116: writers take the cross-process advisory lock (`norte.toml.lock`)
    /// BEFORE reading. With the lock in "another process"'s hands (another
    /// descriptor — the same `flock`/`LockFileEx` mechanism), `persist_set`
    /// BLOCKS until it is released; without the lock, two RMWs interleave
    /// and the second one writes over a stale read (lost update).
    #[test]
    fn persist_set_waits_on_another_writers_lock() {
        let dir = tempfile::tempdir().unwrap();
        // Same open WITHOUT truncating as `lock_config_file` (review MAJOR-1).
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.path().join("norte.toml.lock"))
            .unwrap();
        holder.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let d = dir.path().to_path_buf();
        let writer = std::thread::spawn(move || {
            let r = persist_set(&d, "ui", "theme", toml_edit::Value::from("nord"));
            let _ = tx.send(());
            r
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "persist_set must NOT complete while another writer holds the lock"
        );
        // Belt (review MINOR-6): besides not completing, it has not WRITTEN —
        // the lock is taken before reading, so neither the tmp nor the final
        // file can exist yet.
        assert!(
            !dir.path().join("norte.toml").exists(),
            "nothing written while the lock is in someone else's hands"
        );
        drop(holder); // flock/LockFileEx releases on closing the descriptor
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("lock released, the writer completes");
        writer.join().unwrap().expect("write");
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(s.contains("theme"), "the write landed after the lock");
    }

    /// #116: two concurrent RMW writers over different keys do not stomp on
    /// each other — both keys survive with their last value in the final
    /// file (without the lock, a stale read discards the other's key).
    #[test]
    fn concurrent_writers_do_not_lose_keys() {
        let dir = tempfile::tempdir().unwrap();
        let d1 = dir.path().to_path_buf();
        let d2 = dir.path().to_path_buf();
        let a = std::thread::spawn(move || {
            for i in 0..25 {
                persist_set(&d1, "ui", "alpha", toml_edit::Value::from(i)).expect("a");
            }
        });
        let b = std::thread::spawn(move || {
            for i in 0..25 {
                persist_set(&d2, "ui", "beta", toml_edit::Value::from(i)).expect("b");
            }
        });
        a.join().unwrap();
        b.join().unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        let doc: toml_edit::DocumentMut = s.parse().expect("the final file parses");
        let ui = doc["ui"].as_table_like().expect("[ui] present");
        assert_eq!(
            ui.get("alpha").and_then(toml_edit::Item::as_integer),
            Some(24),
            "the last write of `alpha` survives"
        );
        assert_eq!(
            ui.get("beta").and_then(toml_edit::Item::as_integer),
            Some(24),
            "the last write of `beta` survives"
        );
    }

    /// #116 (pin): the write is sibling tmp + rename — after persisting, no
    /// residual temp is left in the dir (a half-finished crash leaves at
    /// most an orphaned tmp the next write overwrites; never a truncated
    /// `norte.toml`).
    #[test]
    fn persisting_leaves_no_residual_tmp() {
        let dir = tempfile::tempdir().unwrap();
        persist_set(dir.path(), "ui", "theme", toml_edit::Value::from("nord")).expect("write");
        persist_hotlist_add(dir.path(), "docs", "file:///docs").expect("hotlist");
        let residual: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(residual.is_empty(), "residual tmp: {residual:?}");
    }
}

/// Tests for the `keymap.toml` writer (K3c c1) — the SECOND writable config
/// file, and the first value shape that is an array of inline TABLES. The
/// load-bearing ones are the lock (a keymap write must never take
/// `norte.toml`'s), idempotence (a double confirm cannot double-write),
/// replace-in-place (a SECOND rebind of the same key must take effect) and
/// the refusal to extend a file that would not load.
#[cfg(test)]
mod persist_keymap_tests {
    use super::*;

    /// `&["g", "g"]` as the writer wants it.
    fn seq(cs: &[&str]) -> Vec<String> {
        cs.iter().map(|c| (*c).to_owned()).collect()
    }

    /// Reads back `dir/keymap.toml`.
    fn read(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("keymap.toml")).expect("keymap.toml")
    }

    /// Neither the file nor the dir exist: both are created, and the binding
    /// lands under an EXPLICIT `[pane]` with the presets' one-per-line shape.
    #[test]
    fn bind_creates_the_file_the_dir_and_the_section() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("subdir/not-yet");
        let w = persist_keymap_bind(
            &dir,
            "pane",
            KeymapList::Prepend,
            &seq(&["g", "g"]),
            "cursor.top",
        )
        .unwrap();
        assert!(w.changed, "the first bind writes");
        assert_eq!(w.path, dir.join("keymap.toml"));
        let s = std::fs::read_to_string(&w.path).unwrap();
        assert!(s.contains("[pane]"), "explicit section: {s}");
        assert!(
            s.contains(r#"{ on = ["g", "g"], run = "cursor.top" }"#),
            "{s}"
        );
        let doc: toml_edit::DocumentMut = s.parse().expect("the written file parses");
        assert_eq!(doc["pane"]["prepend_keymap"].as_array().unwrap().len(), 1);
    }

    /// The list is the caller's choice and it is not cosmetic: a rebind needs
    /// `Prepend` (it wins over the preset), `Append` loses to it. Each writes
    /// into ITS key and neither touches the other.
    #[test]
    fn each_list_is_written_under_its_own_key() {
        let dir = tempfile::tempdir().unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["f5"]),
            "pane.copy",
        )
        .unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Append,
            &seq(&["f6"]),
            "pane.move",
        )
        .unwrap();
        let doc: toml_edit::DocumentMut = read(dir.path()).parse().unwrap();
        let prepend = doc["pane"]["prepend_keymap"].as_array().unwrap();
        let append = doc["pane"]["append_keymap"].as_array().unwrap();
        assert_eq!(prepend.len(), 1, "{prepend}");
        assert_eq!(append.len(), 1, "{append}");
        assert!(prepend.to_string().contains("pane.copy"));
        assert!(append.to_string().contains("pane.move"));
    }

    /// A repeated bind (the double confirm) writes NOTHING: same bytes, and
    /// `changed == false` says so instead of the caller having to diff.
    #[test]
    fn bind_is_idempotent_and_rewrites_nothing() {
        let dir = tempfile::tempdir().unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g", "g"]),
            "cursor.top",
        )
        .unwrap();
        let before = read(dir.path());
        let w = persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g", "g"]),
            "cursor.top",
        )
        .unwrap();
        assert!(!w.changed, "the binding was already there");
        assert_eq!(read(dir.path()), before, "not one byte moved");
        assert_eq!(before.matches("cursor.top").count(), 1, "{before}");
    }

    /// A SECOND rebind of the same chord REPLACES the first in place. Appending
    /// instead would leave two entries for one chord with the older one
    /// winning (first wins, in file order): the user's second choice would do
    /// nothing, with the editor reporting success.
    #[test]
    fn rebinding_a_chord_replaces_it_in_place() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "[pane]\nprepend_keymap = [\n    { on = [\"f9\"], run = \"pane.mkdir\" },\n    { on = [\"g\"], run = \"cursor.top\" },\n]\n",
        )
        .unwrap();
        let w = persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.bottom",
        )
        .unwrap();
        assert!(w.changed);
        let s = read(dir.path());
        assert!(!s.contains("cursor.top"), "the old command is gone: {s}");
        assert_eq!(s.matches("[\"g\"]").count(), 1, "ONE entry for `g`: {s}");
        let doc: toml_edit::DocumentMut = s.parse().unwrap();
        let arr = doc["pane"]["prepend_keymap"].as_array().unwrap();
        assert_eq!(arr.len(), 2, "the sibling stays");
        // Position preserved: `g` is still the second row, not moved to the end.
        assert_eq!(
            arr.get(1).unwrap().as_inline_table().unwrap()["run"].as_str(),
            Some("cursor.bottom")
        );
    }

    /// Same chord, other section or other list: different binding, no replace.
    #[test]
    fn a_chord_bound_elsewhere_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        for (section, list) in [
            ("pane", KeymapList::Prepend),
            ("pane", KeymapList::Append),
            ("viewer", KeymapList::Prepend),
            ("global", KeymapList::Prepend),
            ("dialog", KeymapList::Prepend),
        ] {
            assert!(
                persist_keymap_bind(dir.path(), section, list, &seq(&["g"]), "cursor.top")
                    .unwrap()
                    .changed,
                "{section}/{}",
                list.key()
            );
        }
        let s = read(dir.path());
        assert_eq!(s.matches("cursor.top").count(), 5, "{s}");
    }

    /// Binding into an existing layer keeps its comments, its formatting and
    /// every binding that was already there — and the comment that annotated
    /// the LAST binding stays on that binding's line instead of being handed
    /// to the new one.
    #[test]
    fn bind_keeps_comments_on_the_binding_they_annotate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "# my keymap\n[pane]\nprepend_keymap = [\n    { on = [\"f9\"], run = \"pane.mkdir\" }, # mine\n]\n",
        )
        .unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g", "g"]),
            "cursor.top",
        )
        .unwrap();
        let s = read(dir.path());
        assert!(s.contains("# my keymap"), "file comment: {s}");
        assert!(s.contains("pane.mkdir"), "previous binding intact: {s}");
        assert!(s.contains("cursor.top"), "{s}");
        let annotated = s
            .lines()
            .find(|l| l.contains("# mine"))
            .expect("the comment survives");
        assert!(
            annotated.contains("pane.mkdir"),
            "the comment stays on the binding it annotates, it does not migrate: {s}"
        );
    }

    /// The array-of-tables shape (`[[pane.append_keymap]]`) is legal TOML and
    /// serde reads it: this writer must extend it in place, see the binding
    /// that is already there instead of writing a duplicate in the other
    /// shape, and replace in place there too.
    #[test]
    fn an_array_of_tables_layer_is_written_in_its_own_shape() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "[[pane.prepend_keymap]]\non = [\"f9\"]\nrun = \"pane.mkdir\"\n",
        )
        .unwrap();
        assert!(
            !persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&["f9"]),
                "pane.mkdir"
            )
            .unwrap()
            .changed,
            "already bound, in the other shape"
        );
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g", "g"]),
            "cursor.top",
        )
        .unwrap();
        let s = read(dir.path());
        assert_eq!(
            s.matches("[[pane.prepend_keymap]]").count(),
            2,
            "extended in its own shape: {s}"
        );
        // Replace in place reaches the AoT shape too.
        assert!(
            persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&["f9"]),
                "pane.pack"
            )
            .unwrap()
            .changed
        );
        let s = read(dir.path());
        assert!(!s.contains("pane.mkdir"), "{s}");
        assert_eq!(s.matches("[[pane.prepend_keymap]]").count(), 2, "{s}");
        // And the unbind reaches it there.
        assert!(
            persist_keymap_unbind(dir.path(), "pane", &seq(&["f9"]), "pane.pack")
                .unwrap()
                .changed
        );
        assert!(!read(dir.path()).contains("pane.pack"));
    }

    /// A section outside the CLOSED vocabulary of keymap contexts is refused
    /// before anything is opened: `KeymapFile` is `deny_unknown_fields`, so
    /// `[panel]` would not be ignored — it would make the whole layer fail to
    /// load and cost the user their keymap.
    #[test]
    fn an_unknown_section_is_refused_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let err = persist_keymap_bind(
            dir.path(),
            "panel",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .expect_err("`panel` is not a keymap context");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            !dir.path().join("keymap.toml").exists(),
            "nothing is created on the refusal path"
        );
        assert!(
            !dir.path().join("keymap.toml.lock").exists(),
            "not even the lock: the vocabulary is checked first"
        );
    }

    /// An empty chord sequence, an empty token or an empty command are
    /// refused: none of them can make a binding under ANY chord grammar.
    #[test]
    fn an_empty_binding_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        for (chords, command) in [
            (Vec::new(), "cursor.top"),
            (seq(&["g", ""]), "cursor.top"),
            (seq(&["g"]), ""),
        ] {
            let err =
                persist_keymap_bind(dir.path(), "pane", KeymapList::Prepend, &chords, command)
                    .expect_err("empty binding");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
            let err = persist_keymap_unbind(dir.path(), "pane", &chords, command)
                .expect_err("empty binding");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        }
    }

    /// A `[pane]` that is not a table (a hand-edited file) is a CLEAN error,
    /// never the `toml_edit` index panic — same guard, and same reasoning, as
    /// `persist_set_seccion_escalar_es_err_no_panic`. The file is untouched.
    #[test]
    fn a_non_table_section_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keymap.toml"), "pane = 3\n").unwrap();
        let err = persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .expect_err("[pane] scalar must be refused, not panic");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(read(dir.path()), "pane = 3\n", "file untouched");
    }

    /// A binding list that is not a list of bindings is a clean error too —
    /// writing into it would produce a file the loader rejects.
    #[test]
    fn a_non_list_binding_list_is_an_error() {
        for src in [
            "[pane]\nprepend_keymap = 3\n",
            "[pane]\nprepend_keymap = [3]\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("keymap.toml"), src).unwrap();
            let err = persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&["g"]),
                "cursor.top",
            )
            .expect_err("not a list of bindings");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(read(dir.path()), src, "untouched");
        }
    }

    /// A file that already carries a PRESET-only key is REFUSED rather than
    /// extended: `check_layer_keys` (norte-frontend) makes `counts = true`,
    /// `dialog_from` and a non-empty `keymap` list load errors, and a layer
    /// that fails to load costs the user their whole keymap — with the editor
    /// having reported success.
    #[test]
    fn a_layer_carrying_a_preset_key_is_refused() {
        for broken in [
            "counts = true\n",
            "dialog_from = \"orthodox\"\n",
            "[pane]\nkeymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
            "[pane]\nkeymap = 3\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("keymap.toml"), broken).unwrap();
            let err = persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&["g"]),
                "cursor.top",
            )
            .expect_err("preset key in a user layer");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(read(dir.path()), broken, "file untouched: {broken}");
        }
    }

    /// The mirror of the rule above, and the reason each guard checks the
    /// VALUE the loader checks: `counts = false` and an empty `keymap = []`
    /// are legal layers that load today. A writer stricter than the loader
    /// would refuse to save into a file the app itself accepted.
    #[test]
    fn a_layer_the_loader_accepts_is_not_refused() {
        for legal in ["counts = false\n", "[pane]\nkeymap = []\n"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("keymap.toml"), legal).unwrap();
            persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&["g"]),
                "cursor.top",
            )
            .unwrap_or_else(|e| panic!("the loader accepts `{legal}`, so must the writer: {e}"));
        }
    }

    /// THE risk of reusing the `norte.toml` writers as-is: a keymap write must
    /// take `keymap.toml.lock` and nothing else. Holding `norte.toml`'s lock
    /// does not delay it, and it never creates that lock either.
    #[test]
    fn a_keymap_write_does_not_take_the_norte_toml_lock() {
        let dir = tempfile::tempdir().unwrap();
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.path().join("norte.toml.lock"))
            .unwrap();
        holder.lock().unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .expect("norte.toml's lock must not serialise a keymap write");
        drop(holder);
        assert!(dir.path().join("keymap.toml.lock").exists(), "its own lock");
    }

    /// And the other half: a keymap write DOES wait for another writer of
    /// `keymap.toml` — the whole read-modify-write is the critical section,
    /// so two editors cannot lose each other's binding.
    #[test]
    fn a_keymap_write_waits_for_the_keymap_lock() {
        let dir = tempfile::tempdir().unwrap();
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.path().join("keymap.toml.lock"))
            .unwrap();
        holder.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let d = dir.path().to_path_buf();
        let writer = std::thread::spawn(move || {
            let r =
                persist_keymap_bind(&d, "pane", KeymapList::Prepend, &seq(&["g"]), "cursor.top");
            let _ = tx.send(());
            r
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "must not complete while another writer holds the lock"
        );
        assert!(
            !dir.path().join("keymap.toml").exists(),
            "nothing written while the lock is held elsewhere"
        );
        drop(holder);
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the lock released, the writer completes");
        writer.join().unwrap().expect("write");
        assert!(read(dir.path()).contains("cursor.top"));
    }

    /// Two concurrent editors do not lose each other's binding (the lock
    /// covers the read, not just the write).
    #[test]
    fn concurrent_keymap_writers_do_not_lose_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let d1 = dir.path().to_path_buf();
        let d2 = dir.path().to_path_buf();
        let a = std::thread::spawn(move || {
            for i in 0..15 {
                persist_keymap_bind(
                    &d1,
                    "pane",
                    KeymapList::Prepend,
                    &seq(&[&format!("f{i}")]),
                    "cursor.top",
                )
                .expect("a");
            }
        });
        let b = std::thread::spawn(move || {
            for i in 0..15 {
                persist_keymap_bind(
                    &d2,
                    "viewer",
                    KeymapList::Append,
                    &seq(&[&format!("g{i}")]),
                    "viewer.close",
                )
                .expect("b");
            }
        });
        a.join().unwrap();
        b.join().unwrap();
        let doc: toml_edit::DocumentMut = read(dir.path()).parse().expect("parses");
        assert_eq!(doc["pane"]["prepend_keymap"].as_array().unwrap().len(), 15);
        assert_eq!(doc["viewer"]["append_keymap"].as_array().unwrap().len(), 15);
    }

    /// The tmp sibling is DERIVED from the target: a keymap write goes through
    /// `keymap.toml.tmp`. Observed deterministically — a DIRECTORY where the
    /// tmp would go makes `File::create` fail, so the write fails only if it
    /// is that name the writer picked.
    #[test]
    fn the_tmp_sibling_is_the_keymap_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("keymap.toml.tmp")).unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .expect_err("the tmp is keymap.toml.tmp");

        // ...and `norte.toml.tmp` is NOT in its way: hardcoding that name (as
        // the writer did while `norte.toml` was the only writable file) would
        // have two writers racing over one tmp.
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir(other.path().join("norte.toml.tmp")).unwrap();
        persist_keymap_bind(
            other.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .expect("norte.toml's tmp is not the keymap writer's");
    }

    /// A keymap write leaves no residual tmp, and does not touch `norte.toml`.
    #[test]
    fn a_keymap_write_leaves_no_tmp_and_does_not_touch_norte_toml() {
        let dir = tempfile::tempdir().unwrap();
        persist_set(dir.path(), "ui", "theme", toml_edit::Value::from("nord")).unwrap();
        let norte = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .unwrap();
        persist_keymap_unbind(dir.path(), "pane", &seq(&["g"]), "cursor.top").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("norte.toml")).unwrap(),
            norte,
            "the scalar layer is untouched"
        );
        let residual: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(residual.is_empty(), "residual tmp: {residual:?}");
    }

    /// Unbind takes the binding out of BOTH user lists and leaves everything
    /// else where it was. Both, because "unbound" must be true afterwards: a
    /// copy left in the other list would keep the key firing.
    #[test]
    fn unbind_reaches_both_lists_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "# my keymap\n[pane]\nprepend_keymap = [{ on = [\"g\"], run = \"cursor.top\" }]\nappend_keymap = [\n    { on = [\"g\"], run = \"cursor.top\" },\n    { on = [\"f9\"], run = \"pane.mkdir\" },\n]\n",
        )
        .unwrap();
        let w = persist_keymap_unbind(dir.path(), "pane", &seq(&["g"]), "cursor.top").unwrap();
        assert!(w.changed);
        let s = read(dir.path());
        assert!(!s.contains("cursor.top"), "gone from both lists: {s}");
        assert!(s.contains("pane.mkdir"), "the sibling stays: {s}");
        assert!(s.contains("# my keymap"), "the comment stays: {s}");
    }

    /// Every duplicate goes: leaving one behind would leave the binding in
    /// force after the editor said it was unbound.
    #[test]
    fn unbind_takes_out_every_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "[pane]\nappend_keymap = [\n    { on = [\"g\"], run = \"cursor.top\" },\n    { on = [\"g\"], run = \"cursor.top\" },\n]\n",
        )
        .unwrap();
        assert!(
            persist_keymap_unbind(dir.path(), "pane", &seq(&["g"]), "cursor.top")
                .unwrap()
                .changed
        );
        assert!(!read(dir.path()).contains("cursor.top"));
    }

    /// Unbind matches BOTH fields: it can only ever delete the row the editor
    /// showed, never another command that happens to share the chord.
    #[test]
    fn unbind_of_something_absent_does_not_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .unwrap();
        let before = read(dir.path());
        let mtime = std::fs::metadata(dir.path().join("keymap.toml"))
            .unwrap()
            .modified()
            .unwrap();
        for (section, chords, command) in [
            ("pane", seq(&["g"]), "cursor.bottom"),
            ("pane", seq(&["h"]), "cursor.top"),
            ("viewer", seq(&["g"]), "cursor.top"),
        ] {
            let w = persist_keymap_unbind(dir.path(), section, &chords, command).unwrap();
            assert!(!w.changed, "nothing matched");
        }
        assert_eq!(read(dir.path()), before);
        assert_eq!(
            std::fs::metadata(dir.path().join("keymap.toml"))
                .unwrap()
                .modified()
                .unwrap(),
            mtime,
            "not even the mtime moved"
        );
    }

    /// No file, no dir, nothing to remove: a documented no-op that creates
    /// neither the file nor the lock's directory.
    #[test]
    fn unbind_without_a_file_is_a_no_op() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("never-existed");
        let w = persist_keymap_unbind(&dir, "pane", &seq(&["g"]), "cursor.top").unwrap();
        assert!(!w.changed);
        assert_eq!(w.path, dir.join("keymap.toml"));
        assert!(!dir.exists(), "a removal creates nothing");

        let empty = tempfile::tempdir().unwrap();
        assert!(
            !persist_keymap_unbind(empty.path(), "pane", &seq(&["g"]), "cursor.top")
                .unwrap()
                .changed
        );
        assert!(!empty.path().join("keymap.toml").exists());
    }

    /// Encoding pin (#73 discipline, rule 1): a hostile chord token — quote,
    /// newline, an embedded TOML header and a bidi override — round-trips
    /// ESCAPED and byte-identical. `toml_edit` escapes; it never injects TOML.
    /// The chord grammar lives in the frontend, so this writer's job is only
    /// that nothing the caller hands it can break the document.
    #[test]
    fn a_hostile_chord_round_trips_escaped() {
        let dir = tempfile::tempdir().unwrap();
        let hostile = "ctrl+\"x\n[[evil]]\u{202e}y";
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&[hostile]),
            "cursor.top",
        )
        .unwrap();
        let doc: toml_edit::DocumentMut = read(dir.path()).parse().expect("still parses");
        assert_eq!(
            doc["pane"]["prepend_keymap"][0]["on"][0].as_str(),
            Some(hostile),
            "byte-identical after the round trip"
        );
        // And it is found again: idempotence compares the same bytes.
        assert!(
            !persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&[hostile]),
                "cursor.top"
            )
            .unwrap()
            .changed
        );
        assert!(
            persist_keymap_unbind(dir.path(), "pane", &seq(&[hostile]), "cursor.top")
                .unwrap()
                .changed
        );
    }

    /// #73, same discipline as `persist_helpers_no_citan_el_error_crudo_de_toml_edit`
    /// for the scalar layer: an unparseable `keymap.toml` names the FILE and
    /// never quotes its content — `toml_edit`'s own `Display` cites the
    /// offending line, and a hostile chord persisted earlier would ride it
    /// into whatever shows this `io::Error` (the status bar).
    #[test]
    fn an_unparseable_layer_names_the_file_not_its_content() {
        let hostile = "not toml \u{202e}[[pane.append_keymap]]\u{202c} = [unterminated\n";
        for op in ["bind", "unbind"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("keymap.toml"), hostile).unwrap();
            let err = if op == "bind" {
                persist_keymap_bind(
                    dir.path(),
                    "pane",
                    KeymapList::Prepend,
                    &seq(&["g"]),
                    "cursor.top",
                )
                .unwrap_err()
            } else {
                persist_keymap_unbind(dir.path(), "pane", &seq(&["g"]), "cursor.top").unwrap_err()
            };
            let msg = err.to_string();
            assert!(
                !msg.contains("append_keymap") && !msg.contains('\u{202e}'),
                "{op}: the error must not echo the hostile document: {msg:?}"
            );
            assert!(msg.contains("keymap.toml"), "{op}: names the file: {msg:?}");
            assert_eq!(read(dir.path()), hostile, "{op}: file untouched");
        }
    }

    /// Two more section shapes a hand-written layer may legally use, and that
    /// serde reads: a DOTTED key (`pane.prepend_keymap = [...]`) and an INLINE
    /// table (`pane = { … }`). `persist_set` has the same positive pin
    /// (`persist_set_seccion_inline_table_no_se_rechaza`): a guard stricter
    /// than `toml_edit` would refuse a file the library indexes happily, and
    /// a write that reflowed either shape could stop it parsing — the
    /// top-ranked failure of this writer.
    #[test]
    fn dotted_and_inline_sections_are_extended_without_breaking() {
        for src in [
            "pane.prepend_keymap = [{ on = [\"f9\"], run = \"pane.mkdir\" }]\n",
            "pane = { prepend_keymap = [{ on = [\"f9\"], run = \"pane.mkdir\" }] }\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("keymap.toml"), src).unwrap();
            // Seen through the shape: no duplicate written.
            assert!(
                !persist_keymap_bind(
                    dir.path(),
                    "pane",
                    KeymapList::Prepend,
                    &seq(&["f9"]),
                    "pane.mkdir"
                )
                .unwrap()
                .changed,
                "{src}"
            );
            persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&["g"]),
                "cursor.top",
            )
            .unwrap();
            let s = read(dir.path());
            let doc: toml_edit::DocumentMut = s
                .parse()
                .unwrap_or_else(|e| panic!("the extended file must still parse ({src}): {e}\n{s}"));
            let arr = doc["pane"]["prepend_keymap"]
                .as_array()
                .unwrap_or_else(|| panic!("still a binding list ({src}): {s}"));
            assert_eq!(arr.len(), 2, "{s}");
            // And `toml` (the loader's parser, not the editor's) agrees.
            let v: toml::Value = toml::from_str(&s).unwrap_or_else(|e| panic!("{e}\n{s}"));
            assert_eq!(
                v["pane"]["prepend_keymap"].as_array().unwrap().len(),
                2,
                "{s}"
            );
        }
    }
}

/// Lo que una capa de PERFIL puede y no puede decidir (spec 2026-08-26, D2).
#[cfg(test)]
mod profile_layer_tests {
    use super::*;

    fn capas(usuario: &std::path::Path, perfil: &std::path::Path) -> Layers {
        Layers {
            dirs: vec![
                (usuario.to_path_buf(), Layer::User),
                (perfil.to_path_buf(), Layer::Profile),
            ],
        }
    }

    /// D2: el recorte de proyecto estaba escrito como `!= Layer::Project`, así
    /// que una cuarta variante heredaba EN SILENCIO todo lo del usuario. Este
    /// test es el que impide que eso vuelva: un perfil no redirige el
    /// transporte, no enciende la IA, no elige dónde se escriben los logs y no
    /// sube los límites anti-bomba.
    #[test]
    fn un_perfil_no_puede_tocar_daemon_ai_log_ni_archive() {
        let usuario = tempfile::tempdir().expect("tempdir");
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            r#"
[daemon]
socket = "/tmp/ajeno.sock"
[ai]
enabled = true
[log]
dir = "/tmp/logs-ajenos"
[archive]
max_entries = 999999999
"#,
        )
        .expect("write");

        let cfg = load(&capas(usuario.path(), perfil.path())).expect("carga");

        assert_eq!(cfg.daemon.socket, None, "el transporte no se redirige");
        assert!(!cfg.ai.enabled, "la IA no se enciende sola");
        assert_eq!(cfg.log.dir, None, "los logs no se mudan");
        assert_eq!(
            cfg.archive.max_entries, None,
            "los límites anti-bomba no suben"
        );
        assert_eq!(
            cfg.profile_warnings.len(),
            4,
            "y las cuatro se DICEN: callarlas convierte el selector en un \
             escalador de permisos"
        );
        for seccion in ["daemon", "ai", "log", "archive"] {
            assert!(
                cfg.profile_warnings.iter().any(|w| w.contains(seccion)),
                "falta el aviso de [{seccion}]: {:?}",
                cfg.profile_warnings
            );
        }
    }

    /// El editor y el comparador son programas que se EJECUTAN: un perfil no
    /// los elige, y hasta ahora los descartaba sin decirlo.
    #[test]
    fn un_perfil_que_pide_editor_o_comparador_avisa() {
        let usuario = tempfile::tempdir().expect("tempdir");
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            "[ui]\neditor = [\"vim\"]\ndiff_detached = true\n",
        )
        .expect("write");
        let cfg = load(&capas(usuario.path(), perfil.path())).expect("carga");
        assert_eq!(cfg.ui_editor, None, "no se aplica");
        assert!(
            cfg.profile_warnings.iter().any(|w| w.contains("editor")),
            "y se dice: {:?}",
            cfg.profile_warnings
        );
    }

    /// Cada CLAVE de esas secciones avisa sola, no sólo las que había cuando
    /// se escribió el aviso. `[log] format` llegó después (ADR 0127), y un
    /// perfil que sólo traía esa clave se descartaba sin decir nada.
    #[test]
    fn un_perfil_que_solo_pide_el_formato_del_log_tambien_avisa() {
        let usuario = tempfile::tempdir().expect("tempdir");
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            "[log]\nformat = \"json\"\n",
        )
        .expect("write");

        let cfg = load(&capas(usuario.path(), perfil.path())).expect("carga");

        assert_eq!(
            cfg.log.format,
            crate::schema::LogFormat::Text,
            "no se aplica"
        );
        assert!(
            cfg.profile_warnings.iter().any(|w| w.contains("[log]")),
            "y se dice: {:?}",
            cfg.profile_warnings
        );
    }

    /// Y lo que SÍ puede: presentación entera, más el preset de keymap y los
    /// favoritos, que la capa de proyecto no puede y ésta sí — un perfil es del
    /// usuario, un repositorio ajeno no.
    #[test]
    fn un_perfil_pisa_presentacion_keymap_y_favoritos() {
        let usuario = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[ui]\ntheme = \"nord\"\nlayout = \"orthodox\"\n[keymap]\npreset = \"orthodox\"\n",
        )
        .expect("write");
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            r#"
[ui]
theme = "solarized"
layout = "explorer"
[keymap]
preset = "far"
[[hotlist]]
name = "src"
path = "/home/u/src"
"#,
        )
        .expect("write");

        let cfg = load(&capas(usuario.path(), perfil.path())).expect("carga");

        assert_eq!(cfg.ui_theme.as_deref(), Some("solarized"));
        assert_eq!(cfg.ui_layout.as_deref(), Some("explorer"));
        assert_eq!(cfg.preset, "far");
        assert_eq!(cfg.hotlist.len(), 1);
        assert!(cfg.profile_warnings.is_empty());
    }

    /// D3: `[profile.start]` es lo que hace útil un perfil recién creado. Las
    /// claves son ids de hueco TAL Y COMO los escribe la disposición del
    /// perfil, y los valores son [`VPath`]s en forma de cable — que es lo que
    /// escribe `save_profile`, y lo que permite que un hueco de un perfil
    /// abra en sftp o dentro de un contenedor.
    #[test]
    fn profile_start_se_lee_con_sus_ids() {
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            "[profile]\ntitle = \"Trabajo\"\n\n[profile.start]\n\
             1 = \"file:///home/u/src\"\n2 = \"sftp://maquina/srv\"\n",
        )
        .expect("write");
        let layers = Layers {
            dirs: vec![(perfil.path().to_path_buf(), Layer::Profile)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.profile_title.as_deref(), Some("Trabajo"));
        assert_eq!(
            cfg.profile_start.get(&1).map(norte_proto::VPath::to_wire),
            Some("file:///home/u/src".to_owned())
        );
        assert_eq!(
            cfg.profile_start.get(&2).map(|v| v.scheme().to_owned()),
            Some("sftp".to_owned()),
            "un hueco de un perfil no tiene por qué ser local"
        );
        assert_eq!(cfg.profile_start.len(), 2);
    }

    /// Una ruta SIN esquema se tira con su aviso, igual que una clave mala.
    ///
    /// Ese aviso llega a la pantalla (`profile_warnings`), que es lo que hace
    /// que esto sea una regla y no una trampa: quien escriba `/tmp` a mano lo
    /// ve, en vez de quedarse con un hueco que abre donde le parece.
    #[test]
    fn una_ruta_de_start_sin_esquema_se_avisa_y_se_tira() {
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            // Un valor con una palabra que no puede salir de ninguna otra
            // parte del aviso: el `tempdir` de este test vive DENTRO de /tmp,
            // así que buscar «/tmp» habría dado un falso positivo con la ruta
            // del propio fichero.
            "[profile.start]\n1 = \"/secreto-del-lector\"\n2 = \"file:///home/u\"\n",
        )
        .expect("write");
        let layers = Layers {
            dirs: vec![(perfil.path().to_path_buf(), Layer::Profile)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.profile_start.len(), 1, "el bueno sobrevive");
        assert!(cfg.profile_start.contains_key(&2));
        let aviso = cfg.profile_warnings.join(" ");
        assert!(
            aviso.contains("hueco 1"),
            "se dice qué hueco se quedó sin sembrar: {aviso}"
        );
        assert!(
            !aviso.contains("secreto-del-lector"),
            "y NO se cita el valor, que es una ruta y esto va a la barra: {aviso}"
        );
    }

    /// Una clave que no es un id de hueco no rompe el arranque: se tira y se
    /// dice. El fichero es del usuario, pero un dedazo en un id no vale una
    /// negativa a arrancar.
    #[test]
    fn una_clave_de_start_que_no_es_un_id_se_avisa_y_se_tira() {
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            "[profile.start]\nizquierda = \"file:///tmp\"\n1 = \"file:///home/u\"\n",
        )
        .expect("write");
        let layers = Layers {
            dirs: vec![(perfil.path().to_path_buf(), Layer::Profile)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.profile_start.len(), 1, "el bueno sobrevive");
        assert!(
            cfg.profile_warnings.iter().any(|w| w.contains("izquierda")),
            "y el malo se dice por su nombre: {:?}",
            cfg.profile_warnings
        );
    }

    /// `[profile]` en una capa que NO es de perfil no significa nada, y decirlo
    /// evita que alguien lo escriba en su norte.toml y espere que pase algo.
    #[test]
    fn profile_fuera_de_un_perfil_se_ignora_con_aviso() {
        let usuario = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[profile]\ntitle = \"no soy un perfil\"\n",
        )
        .expect("write");
        let layers = Layers {
            dirs: vec![(usuario.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.profile_title, None);
        assert_eq!(cfg.profile_warnings.len(), 1);
    }

    /// Y el proyecto sigue mandando sobre el perfil (D1): esto es lo que hace
    /// que ADR 0026 y #260 no cambien de significado.
    #[test]
    fn proyecto_sigue_pisando_al_perfil_en_presentacion() {
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            "[ui]\ntheme = \"solarized\"\n",
        )
        .expect("write");
        let proyecto = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            proyecto.path().join("norte.toml"),
            "[ui]\ntheme = \"nord\"\n",
        )
        .expect("write");

        let layers = Layers {
            dirs: vec![
                (perfil.path().to_path_buf(), Layer::Profile),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.ui_theme.as_deref(), Some("nord"));
    }
}

/// Lo que una capa de PROYECTO puede y no puede decidir (#260).
#[cfg(test)]
mod project_layer_tests {
    use super::*;

    fn capas(sistema: &std::path::Path, proyecto: &std::path::Path) -> Layers {
        Layers {
            dirs: vec![
                (sistema.to_path_buf(), Layer::User),
                (proyecto.to_path_buf(), Layer::Project),
            ],
        }
    }

    /// Un repositorio NO elige el preset de teclado.
    ///
    /// Está acotado a los siete de fábrica, así que no es ejecución de
    /// código — pero los presets discrepan sobre qué hace cada tecla:
    /// `far` ata `shift+delete` a `pane.delete` y `orthodox` ata `shift+f8`
    /// a `pane.delete-permanent`. Elegir cuál borra no es presentación.
    #[test]
    fn una_capa_de_proyecto_no_elige_el_preset() {
        let usuario = tempfile::tempdir().unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[keymap]\npreset = \"orthodox\"\n",
        )
        .unwrap();
        std::fs::write(
            proyecto.path().join("norte.toml"),
            "[keymap]\npreset = \"far\"\n",
        )
        .unwrap();

        let cfg = load(&capas(usuario.path(), proyecto.path())).expect("carga");
        assert_eq!(
            cfg.preset, "orthodox",
            "el preset lo elige el usuario, no el repositorio"
        );
    }

    /// Y un `.norte.toml` roto no deja a nadie sin gestor de ficheros.
    ///
    /// Cualquier clave desconocida es fatal bajo `deny_unknown_fields`, así
    /// que una errata en un repositorio ajeno rompía el arranque al hacer
    /// `cd` ahí. Ahora la capa se salta, se DICE, y lo demás sigue.
    #[test]
    fn una_capa_de_proyecto_rota_se_salta_y_se_dice() {
        let usuario = tempfile::tempdir().unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[ui]\ntheme = \"nord\"\n",
        )
        .unwrap();
        std::fs::write(proyecto.path().join("norte.toml"), "[ui]\nno_existe = 1\n").unwrap();

        let cfg = load(&capas(usuario.path(), proyecto.path())).expect("arranca igual");
        assert_eq!(
            cfg.ui_theme.as_deref(),
            Some("nord"),
            "lo del usuario sigue"
        );
        assert_eq!(cfg.project_warnings.len(), 1, "y se dice por qué");
        assert!(
            cfg.project_warnings[0].contains("no_existe")
                || cfg.project_warnings[0].contains("norte.toml"),
            "el aviso nombra el problema: {:?}",
            cfg.project_warnings
        );
    }

    /// La capa del USUARIO sigue siendo fatal: ésa sí es suya, y arrancar
    /// ignorándola en silencio sería peor que no arrancar.
    #[test]
    fn una_capa_de_usuario_rota_sigue_siendo_fatal() {
        let usuario = tempfile::tempdir().unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(usuario.path().join("norte.toml"), "[ui]\nno_existe = 1\n").unwrap();

        assert!(
            load(&capas(usuario.path(), proyecto.path())).is_err(),
            "una config del usuario rota se dice a gritos"
        );
    }
}
