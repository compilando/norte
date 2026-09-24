//! Manufacturing files: pack, test, split and combine (#132).
//!
//! The four operations the presets' keys ask for and norte did not have.
//! None of them write INSIDE a container —`norte-vfs-archive` is still
//! `READ_ONLY` (ADR 0018)—: all four read through one provider and
//! **manufacture new files** through another, which can be any provider.
//!
//! Unpacking is not here because it does not need to be: the copy engine
//! already accepts the inside of an archive as a SOURCE, so unpacking is an
//! `fs.copy` from `<container>/!/` and inherits the journal, the undo, the
//! collision policy and the cancellation the copy already has.

use std::sync::Arc;

use futures::StreamExt as _;
use norte_proto::{EntryKind, Error, VPath, methods};
use norte_vfs::Provider;
use norte_vfs_archive::write::{ArchiveWriter, PackEntry, PackFormat};

use crate::observer::{Mutation, MutationObserver};
use crate::scheduler::TaskCtx;

/// Cap on the byte length of an entry's name, the SAME one the read index
/// uses to look at an archive (`norte_vfs_archive::Limits`).
///
/// Writing above it produces entries that this very program will skip when
/// opening it back: an archive that silently swallows files.
const MAX_ENTRY_NAME_BYTES: usize = 4_096;

/// Cap on an entry's number of components, for the same reason.
const MAX_ENTRY_DEPTH: usize = 64;

/// Default compression level when the client does not say one.
const DEFAULT_LEVEL: u8 = 6;

/// The wire's format, translated into the writer's.
fn pack_format(f: methods::ArchiveFormat) -> PackFormat {
    match f {
        methods::ArchiveFormat::Zip => PackFormat::Zip,
        methods::ArchiveFormat::Tar => PackFormat::Tar,
        methods::ArchiveFormat::TarGz => PackFormat::TarGz,
    }
}

/// The name a path has INSIDE the archive: whatever lies between `base` and
/// `p`, in raw bytes and separated by `/`.
///
/// `None` if `p` does not hang off `base` — the caller rejects it instead of
/// making up a name, because a made-up name ends up in an archive that
/// someone unpacks on top of something else.
fn relative_name(base: &VPath, p: &VPath) -> Option<Vec<u8>> {
    if p.scheme() != base.scheme() || p.authority() != base.authority() {
        return None;
    }
    let base_segs: Vec<&[u8]> = base.segments().collect();
    let segs: Vec<&[u8]> = p.segments().collect();
    if segs.len() <= base_segs.len() || !segs.starts_with(&base_segs) {
        return None;
    }
    let tail = &segs[base_segs.len()..];
    // The archive marker CANNOT be an entry's name: the read index skips
    // exactly that component (ADR 0018), so writing it would produce an
    // entry that norte cannot address again.
    if tail.iter().any(|s| *s == b"!") {
        return None;
    }
    let mut out = Vec::new();
    for (i, s) in tail.iter().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(s);
    }
    Some(out)
}

/// The format token a container's NAME suggests, with the aliases people
/// actually write (`.tgz`, `.tar.gz`).
///
/// Twin of the one the TUI uses to decide whether `Enter` goes into a file
/// (`nav::archive_root_for`), and here for the same reason as there: it is
/// presentation sugar over proto's whitelist, not validation — `archive_compose`
/// handles that.
pub(crate) fn name_format(name: &[u8]) -> Option<&'static str> {
    const ALIAS: &[(&[u8], &str)] = &[(b".tar.gz", "tar+gz"), (b".tgz", "tar+gz")];
    let ends_with = |suf: &[u8]| {
        name.len() >= suf.len() && name[name.len() - suf.len()..].eq_ignore_ascii_case(suf)
    };
    ALIAS
        .iter()
        .find(|(suf, _)| ends_with(suf))
        .map(|(_, f)| *f)
        .or_else(|| {
            norte_proto::ARCHIVE_FORMATS
                .iter()
                .find(|f| ends_with(format!(".{f}").as_bytes()))
                .copied()
        })
}

/// WHAT can actually be checked in each format.
///
/// It goes into the result and not into the documentation because "passes"
/// means different things: a zip carries a CRC-32 per entry and a `tar.gz`
/// one for the whole stream, but a plain tar carries no content checksum at
/// all — the only thing verifiable there is that each declared size is
/// reached. A client that painted "intact" over that would be claiming what
/// the format cannot back up.
pub(crate) fn that_is_checked(token: &str) -> Vec<String> {
    let v = match token {
        "zip" => "crc",
        "tar+gz" => "gzip_crc",
        // Plain tar and delegated rar: only that the sizes are reached.
        _ => "sizes",
    };
    vec![v.to_owned()]
}

/// Where to write: the provider and the path.
///
/// A type and not two loose parameters because `pack` already carried eight,
/// and the two that go together are exactly these: the provider is the ONE
/// FOR that path.
pub(crate) struct Dest {
    /// The destination's provider, which may not be the sources' one.
    pub(crate) provider: Arc<dyn Provider>,
    /// The file being created.
    pub(crate) dest: VPath,
}

/// How to pack: against which base the entries are named, in which format
/// and with how much compression.
pub(crate) struct Packed {
    /// The directory the stored names hang off of.
    pub(crate) base: VPath,
    /// Format, decided by the client.
    pub(crate) format: methods::ArchiveFormat,
    /// Level 0..=9, or the core's own.
    pub(crate) level: Option<u8>,
}

/// One entry of the walk: what to write and where to read it from.
struct Piece {
    provider: Arc<dyn Provider>,
    path: VPath,
    entry: PackEntry,
}

/// Walks the roots and enumerates EVERYTHING that is going into the archive,
/// before writing a single byte.
///
/// Enumerating first costs one walk and buys three things: the total entry
/// count for the progress bar (a bar with no total is a bar that does not
/// inform), rejecting an impossible name BEFORE the destination has been
/// created, and a stable order.
/// The risk classes this daemon KNOWS how to look for, which is what travels
/// in `ArchivePackReportResult::checked` (#250).
///
/// It is a list and not a loose constant because it has to be able to grow,
/// and because what makes a clean report useful is exactly this: without it,
/// "I found nothing" reads as "there is nothing", and there are classes
/// —`<`, `>`, `"`, `|`, `?`, `*`, all illegal on Windows— that are not looked
/// at here.
// TODO(translation): review — this doc block seems to merge two unrelated
// notes (one about the walk performed by `walk_sources` below, one about
// this constant) with no blank line between them, so rustdoc attaches all of
// it to `CHECKED_RISKS`; translated as found, structure unchanged.
const CHECKED_RISKS: &[&str] = &["separator", "stream", "reserved", "trailing"];

/// Windows-reserved names, without extension and case-insensitive. They
/// cannot be extracted there AT ALL — it is not that they get renamed: the
/// call fails, because a device has the name taken.
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// What this pack stores that MEANS something else outside here (#250).
///
/// `a\b` is a directory separator in 7-Zip and in Explorer; `f:ads` opens an
/// alternate stream in NTFS; `CON` cannot be extracted on Windows at all; a
/// trailing dot or space gets eaten by Windows without saying so. Our own
/// reader round-trips exactly these four, which is precisely why the
/// round-trip test does not see any of them.
///
/// **This WARNS, it does not reject, and its sibling above DOES reject.** Two
/// entries that fold to the same name are not packed (see the check in
/// [`walk_sources`]): extracted somewhere else, one of the two DISAPPEARS.
/// This is something else — `a\b.txt` extracted on Linux is still `a\b.txt`,
/// and on Windows it is a `b.txt` inside an `a` folder. Nothing is lost; it
/// is placed differently. Rejecting it would take down legitimate Unix trees
/// to prevent something that is not even a loss.
fn name_report(names: &[Vec<u8>]) -> methods::ArchivePackReportResult {
    let mut out = methods::ArchivePackReportResult {
        entries: names.len() as u64,
        // What is actually checked, and nothing more. `<`, `>`, `"`, `|`, `?`
        // and `*` are also illegal on Windows and are NOT here: a clean
        // report that did not say what it checked would be claiming the
        // archive travels intact anywhere, which is more than anyone
        // verified.
        checked: CHECKED_RISKS.iter().map(|s| (*s).to_owned()).collect(),
        ..Default::default()
    };
    for n in names {
        let Some(risk) = name_risk(n) else {
            continue;
        };
        if out.risky.len() >= methods::ARCHIVE_PACK_REPORT_MAX {
            out.truncated = true;
            break;
        }
        out.risky.push(methods::PackRiskyName {
            path: name_to_wire(n),
            name: String::from_utf8_lossy(n).into_owned(),
            risk: risk.to_owned(),
        });
    }
    out
}

/// The stored name in WIRE form: percent-encoding over the bytes, which is
/// the only thing that preserves a name that is not UTF-8 (rule 1). The slash
/// is left as is: it separates components inside the archive, and hiding it
/// would make unreadable exactly the name that needs to be found.
fn name_to_wire(name: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(name.len());
    for b in name {
        match b {
            b'/' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            b if b.is_ascii_alphanumeric() => out.push(*b as char),
            // `write!` to a `String` does not fail; the `_` is not hiding a
            // real error.
            b => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// What happens to this name outside here, or `None` if it travels intact.
///
/// ONE answer per name and in this order: what breaks EXTRACTION before what
/// distorts it. A name with two problems is counted once — the report is
/// meant to be read, and two rows for the same file read as two files.
fn name_risk(name: &[u8]) -> Option<&'static str> {
    if name.contains(&b'\\') {
        return Some("separator");
    }
    if name.contains(&b':') {
        return Some("stream");
    }
    for component in name.split(|b| *b == b'/') {
        // Without the extension: on Windows `CON.txt` is just as taken as
        // `CON`.
        let stem = component.split(|b| *b == b'.').next().unwrap_or(component);
        let stem = String::from_utf8_lossy(stem).to_ascii_lowercase();
        if WINDOWS_RESERVED_NAMES.contains(&stem.as_str()) {
            return Some("reserved");
        }
    }
    for component in name.split(|b| *b == b'/') {
        if matches!(component.last(), Some(b'.' | b' ')) {
            return Some("trailing");
        }
    }
    None
}

async fn walk_sources(
    sources: Vec<(Arc<dyn Provider>, VPath)>,
    base: &VPath,
    ctx: &TaskCtx,
) -> Result<Vec<Piece>, Error> {
    // What this ACTOR cannot walk (#209, and before that #165): the read gate
    // looks at the REQUEST's ROOT and nothing else, so packing `$HOME` is
    // legitimate and used to take down the daemon's state directory along
    // with it — `journal.db`, `secrets.age`, `connections.toml`,
    // `session.json`. And an archive is worse than a comparison: the agent
    // reads it back entry by entry through the archive provider, over a file
    // that is in its own scope. An `fs.read` of any of those files is denied;
    // without this, `archive.pack` laundered them all.
    //
    // Comes from the SAME place as the exclusions for `fs.search` and
    // `fs.compare`: two lists of what an agent cannot walk would be two lists
    // that drift apart.
    let excluded = crate::policy::walk_exclusions(&ctx.actor);
    let mut out = Vec::new();
    for (provider, root) in sources {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut pending = vec![root];
        while let Some(p) = pending.pop() {
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            // Checked BEFORE the `stat`: whether the entry exists is not the
            // business either of someone who cannot walk it.
            if excluded
                .iter()
                .any(|root| crate::policy::is_under(root, &p))
            {
                tracing::debug!("archive.pack: subtree excluded for this actor");
                continue;
            }
            let e = provider.stat(&p).await?;
            let name = relative_name(base, &p).ok_or(Error::InvalidPath)?;
            // The SAME caps as the read index (ADR 0018): a name longer than
            // `max_name_bytes` or a depth above `max_depth` gets SKIPPED when
            // reading, so writing them produces an archive whose entries
            // norte never sees again — the same hole the marker `!` rejection
            // closes, through a different door. `split` and not a byte
            // counter: these are path names, not a stream, and clippy's
            // suggestion (pulling in `bytecount`) is a whole dependency to
            // count slashes in 4 KiB.
            let depth = name.split(|b| *b == b'/').count();
            if name.len() > MAX_ENTRY_NAME_BYTES || depth > MAX_ENTRY_DEPTH {
                tracing::warn!("archive.pack: an entry would not fit in the read index");
                return Err(Error::InvalidPath);
            }
            match e.kind {
                EntryKind::Dir => {
                    out.push(Piece {
                        provider: Arc::clone(&provider),
                        path: p.clone(),
                        entry: PackEntry::dir(name),
                    });
                    let mut stream = provider.list(&p).await?;
                    while let Some(child) = stream.next().await {
                        pending.push(child?.path);
                    }
                }
                EntryKind::File => {
                    let mut pe = PackEntry::file(name, e.size.unwrap_or(0));
                    pe.mtime_ms = e.mtime_ms;
                    out.push(Piece {
                        provider: Arc::clone(&provider),
                        path: p,
                        entry: pe,
                    });
                }
                // A symlink is neither followed nor stored as a link: storing
                // the target would be copying what it points to without
                // saying so, and storing the link asks for an entry type the
                // writer does not have yet. It is SKIPPED with a warning,
                // which is what the read index does with what it cannot
                // represent.
                EntryKind::Symlink | EntryKind::Other => {
                    tracing::warn!("archive.pack: entry skipped for its type");
                }
            }
        }
    }
    // Directories first within each level, and stable: an archive whose order
    // depends on the provider's listing order is not reproducible.
    out.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
    // **Two entries with the SAME stored name are not written.** It happens
    // with overlapping roots —`sources: ["/p/a", "/p/a/b"]`, which the wire
    // accepts even though the TUI's marks never form it— and the resulting
    // archive carries the entry twice, with its content twice: our index
    // resolves it as "the last one wins" and other tools extract it twice.
    // Sorted as it is, finding it is one comparison.
    if out.windows(2).any(|p| p[0].entry.name == p[1].entry.name) {
        tracing::warn!("archive.pack: two sources give the same name inside the archive");
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::Exists,
        });
    }
    // **And two that FOLD to the same name, neither** (#250). The case above
    // is that the bytes match; this one is that they match wherever the
    // archive ends up being extracted, which is what an archive cannot know:
    // `café.txt` in NFD and in NFC are two files on ext4 and one on APFS,
    // `µ` and `μ` are two here and one on NTFS, and `straße` and `strasse`
    // are two almost everywhere and one on an ext4 `+F`. Extracted there, one
    // of the two disappears without a word.
    //
    // It folds with the WIDEST mode on purpose: an archive's destination is
    // by definition unknown —it gets sent elsewhere—, so the question is not
    // "do they collide on this machine?" but "do they collide on any
    // machine?". The price is rejecting a pair that is legitimate here; the
    // price of not doing it is a file silently lost on someone else's
    // machine, and that is the direction ADR 0005 says not to take.
    let mut keys: Vec<Vec<u8>> = out
        .iter()
        .map(|p| {
            norte_encoding::name_key(&p.entry.name, norte_encoding::FoldMode::Full).into_owned()
        })
        .collect();
    keys.sort_unstable();
    if keys.windows(2).any(|k| k[0] == k[1]) {
        tracing::warn!("archive.pack: two entries would be the same name when extracted");
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::Exists,
        });
    }
    Ok(out)
}

/// `archive.pack`: manufactures the archive.
///
/// The destination is written through a [`norte_vfs::ByteSink`], so
/// cancellation leaves the destination CLEAN —`abort` takes the staging with
/// it— and not a half-finished file that looks like an archive. The journal
/// entry is emitted after the `commit`, which is when the node truly exists.
pub(crate) async fn pack(
    sources: Vec<(Arc<dyn Provider>, VPath)>,
    destination: Dest,
    spec: Packed,
    observer: Arc<dyn MutationObserver>,
    report: Arc<std::sync::Mutex<methods::ArchivePackReportResult>>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let Dest {
        provider: provider_dest,
        dest,
    } = destination;
    let Packed {
        base,
        format,
        level,
    } = spec;
    if sources.is_empty() {
        return Err(Error::InvalidPath);
    }
    // The journal's verdict, fixed before the first effect (#205).
    let observer = crate::observer::pin_for_task(observer).await?;
    // The destination is NOT overwritten: manufacturing an archive on top of
    // a file that already exists is silent data loss, and the caller already
    // knows how to ask.
    if provider_dest.stat(&dest).await.is_ok() {
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::Exists,
        });
    }
    let pieces = walk_sources(sources, &base, ctx).await?;
    // The report is computed over what is GOING to be stored and BEFORE
    // writing a byte (#250): that way it exists even if the Task is cancelled
    // halfway through, and what it says stays true of the half-finished
    // archive — the colliding entries do so whether they are all there or
    // only the first ones.
    {
        let names: Vec<Vec<u8>> = pieces.iter().map(|p| p.entry.name.clone()).collect();
        let computed = name_report(&names);
        *report
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = computed;
    }
    let total_bytes: u64 = pieces.iter().map(|p| p.entry.size).sum();
    ctx.progress.update(|p| {
        p.entries_total = Some(pieces.len() as u64);
        p.bytes_total = Some(total_bytes);
    });

    let mut w = ArchiveWriter::new(
        pack_format(format),
        u32::from(level.unwrap_or(DEFAULT_LEVEL)),
    );
    let mut sink = provider_dest.write(&dest).await?;
    let mut bytes_read: u64 = 0;
    let mut done: u64 = 0;

    // A closure so the "release the sink without publishing" is not repeated
    // for every exit path: without this, a `?` in the middle would leave the
    // staging hanging.
    macro_rules! abort_with {
        ($e:expr) => {{
            let err = $e;
            let _ = sink.abort().await;
            return Err(err);
        }};
    }

    for piece in pieces {
        // The writer goes and COMES BACK: `one_piece` moves it to the
        // blocking pool to compress (#250). On the error path it does not
        // come back, and it does not need to — any error here aborts the
        // whole archive.
        w = match one_piece(&piece, w, &mut *sink, &mut bytes_read, ctx).await {
            Ok(w) => w,
            Err(e) => abort_with!(e),
        };
        done = done.saturating_add(1);
        ctx.progress.update(|p| p.entries_done = done);
    }
    if let Err(e) = w.finish() {
        abort_with!(from_pack_error(e));
    }
    let output = w.take();
    if !output.is_empty()
        && let Err(e) = sink.write(bytes::Bytes::from(output)).await
    {
        abort_with!(e);
    }
    sink.commit().await?;
    // After the commit: before it, the journal would point at a node that
    // does not exist yet (rule 4), and the identity would be the staging's.
    let node = crate::ops::identity_of(&*provider_dest, &dest, &observer).await;
    observer
        .on_mutation(&Mutation::Created { path: &dest, node }, &ctx.actor)
        .await?;
    Ok(())
}

/// ONE entry: it is opened, the source's bytes are fed to it in chunks, and
/// whatever the writer produces along the way is drained to the sink as it
/// comes out.
///
/// Split off from [`pack`]'s loop so that neither one goes past a hundred
/// lines, and because it is the unit that reads whole in one glance: open,
/// copy, close. The sink's owner is the caller — an error here ABORTS the
/// archive, it does not skip the entry.
///
/// Takes the writer by VALUE and returns it (#250): compressing is CPU, not
/// blocking I/O, but a level-9 `deflate` over a large tree holds a runtime
/// thread for long bursts — and the runtime's threads are the ones serving
/// every other client of the daemon. Each chunk is compressed on the
/// blocking pool, which is where that work does not take anyone else's spot.
/// On the error path the writer does not come back, and it does not need to:
/// any error here aborts the whole archive.
async fn one_piece(
    piece: &Piece,
    mut w: ArchiveWriter,
    sink: &mut dyn norte_vfs::ByteSink,
    bytes_read: &mut u64,
    ctx: &TaskCtx,
) -> Result<ArchiveWriter, Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    ctx.progress
        .update(|p| p.current = Some(piece.path.clone()));
    w.begin(&piece.entry).map_err(from_pack_error)?;
    if !piece.entry.dir {
        let mut stream = piece.provider.read(&piece.path, None).await?;
        let mut written: u64 = 0;
        while let Some(chunk) = stream.next().await {
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let chunk = chunk?;
            // The size is recorded BEFORE moving the chunk to the pool: the
            // progress counts bytes READ from the source, not the compressed
            // ones.
            let n = chunk.len() as u64;
            written = written.saturating_add(n);
            // Compress and drain, both on the pool: a `take` without `data`
            // would be a round trip for nothing.
            let (writer, output, res) = crate::blocking::spawn_blocking(move || {
                let res = w.data(&chunk);
                let output = w.take();
                (w, output, res)
            })
            .await
            .map_err(|_| Error::Internal { panic: true })?;
            w = writer;
            res.map_err(from_pack_error)?;
            *bytes_read = bytes_read.saturating_add(n);
            let bytes_so_far = *bytes_read;
            ctx.progress.update(|p| p.bytes_done = bytes_so_far);
            if !output.is_empty() {
                sink.write(bytes::Bytes::from(output)).await?;
            }
        }
        // The source changed between the `stat` and the read. In tar that is
        // a header lying about what comes after, so the whole archive
        // becomes unreadable past that entry: it is aborted instead of
        // publishing something like that.
        if written != piece.entry.size {
            tracing::warn!("archive.pack: the source changed size while being read");
            return Err(Error::Conflict {
                conflict: norte_proto::ConflictKind::TypeMismatch,
            });
        }
    }
    // Closing an entry flushes the compressor's buffer, so it is also CPU
    // work: to the pool, like the rest.
    let (w, output, res) = crate::blocking::spawn_blocking(move || {
        let res = w.end();
        let output = w.take();
        (w, output, res)
    })
    .await
    .map_err(|_| Error::Internal { panic: true })?;
    res.map_err(from_pack_error)?;
    if !output.is_empty() {
        sink.write(bytes::Bytes::from(output)).await?;
    }
    Ok(w)
}

/// A failure from the writer, in the wire's taxonomy.
fn from_pack_error(e: norte_vfs_archive::write::PackError) -> Error {
    use norte_vfs_archive::write::PackError as P;
    match e {
        // A name that does not fit the format is an impossible request, not
        // an I/O failure.
        P::Name => Error::InvalidPath,
        // NOT retryable: retrying produces exactly the same failure. An
        // `State` is a bug in this code and a `Size` is a source that
        // moved under our feet.
        P::Size | P::State | P::Io => Error::Io { retryable: false },
    }
}

/// `archive.test`: reads each entry to the end and says what was checked.
///
/// The actual checking is done by the READER: the zip one verifies the
/// CRC-32 when an entry is read whole, and the `tar.gz` one the gzip's tail.
/// This op walks and collects; duplicating the verification here would mean
/// having two opinions about the same thing.
pub(crate) async fn test_archive(
    provider: Arc<dyn Provider>,
    root: VPath,
    checked: Vec<String>,
    report: Arc<std::sync::Mutex<methods::ArchiveTestResult>>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    {
        let mut i = report
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        i.checked = checked;
    }
    let mut pending = vec![root];
    let mut entries: u64 = 0;
    let mut bytes: u64 = 0;
    while let Some(dir) = pending.pop() {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut stream = provider.list(&dir).await?;
        while let Some(e) = stream.next().await {
            let e = e?;
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            match e.kind {
                EntryKind::Dir => pending.push(e.path),
                EntryKind::File => {
                    entries = entries.saturating_add(1);
                    ctx.progress.update(|p| {
                        p.entries_done = entries;
                        p.current = Some(e.path.clone());
                    });
                    if let Err(err) = read_whole(&*provider, &e.path, &mut bytes, ctx).await {
                        if matches!(err, Error::Cancelled) {
                            return Err(Error::Cancelled);
                        }
                        record_failure(&report, &e.path, &err);
                    }
                    ctx.progress.update(|p| p.bytes_done = bytes);
                }
                EntryKind::Symlink | EntryKind::Other => {
                    entries = entries.saturating_add(1);
                }
            }
        }
    }
    let mut i = report
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    i.entries = entries;
    Ok(())
}

/// Reads an entry whole, which is what triggers the reader's verification.
async fn read_whole(
    provider: &dyn Provider,
    path: &VPath,
    bytes: &mut u64,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let mut stream = provider.read(path, None).await?;
    while let Some(chunk) = stream.next().await {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        *bytes = bytes.saturating_add(chunk?.len() as u64);
    }
    Ok(())
}

/// Records a failure in the report, with the cap in place.
fn record_failure(
    report: &Arc<std::sync::Mutex<methods::ArchiveTestResult>>,
    path: &VPath,
    err: &Error,
) {
    let reason = match err {
        Error::Corrupt => "crc",
        Error::Unsupported => "unsupported",
        Error::NotFound => "truncated",
        _ => "io",
    };
    let mut i = report
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if i.failed.len() >= methods::ARCHIVE_TEST_MAX_FAILURES {
        i.truncated = true;
        return;
    }
    i.failed.push(methods::ArchiveTestFailure {
        // The WHOLE path, in wire form: it is the only thing that preserves
        // the bytes, and this report is the only place that names the entry
        // that failed. The lossy `name` goes separately, for display.
        path: path.to_wire(),
        name: path
            .file_name()
            .map(|s| String::from_utf8_lossy(s.as_bytes()).into_owned())
            .unwrap_or_default(),
        reason: reason.to_owned(),
    });
}

/// The name of piece `n` of a split: `<name>.001`.
fn piece_name(base: &[u8], n: u64) -> Vec<u8> {
    let mut v = base.to_vec();
    v.extend_from_slice(format!(".{n:03}").as_bytes());
    v
}

/// `file.split`: splits a file into numbered pieces.
pub(crate) async fn split(
    src: Arc<dyn Provider>,
    path: VPath,
    part_bytes: u64,
    provider_dest: Arc<dyn Provider>,
    dest_dir: VPath,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let observer = crate::observer::pin_for_task(observer).await?;
    let (name, total, piece_count) = measures_the_distribution(&*src, &path, part_bytes).await?;
    ctx.progress.update(|p| {
        p.bytes_total = Some(total);
        p.entries_total = Some(piece_count);
    });

    check_slots_free(&*provider_dest, &dest_dir, &name, piece_count).await?;

    // **No accumulating.** The first version gathered `part_bytes` into a
    // `Vec` and then drained it: a peak of twice the piece size, and with a
    // `part_bytes` the wire does not cap —`u64::MAX` is a legal value— any
    // client could take the daemon down with an OOM. Now it writes as it
    // arrives and the piece closes when it fills up, so memory is that of ONE
    // chunk from the provider. Along the way, cancellation is checked per
    // chunk and not per piece: with gigabyte pieces, waiting for the end of
    // the piece is not cancelling.
    let mut stream = src.read(&path, None).await?;
    let mut done: u64 = 0;
    let mut written: u64 = 0;
    let mut sink: Option<Box<dyn norte_vfs::ByteSink>> = None;
    let mut current: u64 = 0;
    let mut current_dest: Option<VPath> = None;
    // The ones already published, so they can be retracted if this gets cut
    // short: half a set of pieces is indistinguishable from a whole one (see
    // [`remove_pieces`]).
    let mut published: Vec<VPath> = Vec::new();

    macro_rules! undo_with {
        ($sink:expr, $published:expr, $e:expr) => {{
            if let Some(s) = $sink.take() {
                let _ = s.abort().await;
            }
            remove_pieces(&*provider_dest, &$published, &observer, ctx).await;
            return Err($e);
        }};
    }

    while let Some(chunk) = stream.next().await {
        if ctx.cancel.is_cancelled() {
            undo_with!(sink, published, Error::Cancelled);
        }
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => undo_with!(sink, published, e),
        };
        let mut rest = &chunk[..];
        while !rest.is_empty() {
            if sink.is_none() {
                if done >= methods::FILE_SPLIT_MAX_PARTS {
                    // The cap, against what is being WRITTEN and not against
                    // the `stat`'s estimate: a file that grows while it is
                    // being read used to pass the estimate with 800 pieces
                    // and write 1200, and nobody joins a `.1000` back
                    // together.
                    undo_with!(
                        sink,
                        published,
                        Error::LimitExceeded {
                            limit: "split-parts".to_owned(),
                        }
                    );
                }
                let dest_path = dest_dir.join(
                    norte_proto::Segment::new(piece_name(&name, done + 1))
                        .map_err(|_| Error::InvalidPath)?,
                );
                sink = Some(provider_dest.write(&dest_path).await?);
                current_dest = Some(dest_path);
                current = 0;
            }
            let fits = usize::try_from(part_bytes - current).unwrap_or(usize::MAX);
            let cut = fits.min(rest.len());
            let (head, tail) = rest.split_at(cut);
            let failure = match sink.as_mut() {
                Some(s) => s.write(bytes::Bytes::copy_from_slice(head)).await.err(),
                None => None,
            };
            if let Some(e) = failure {
                undo_with!(sink, published, e);
            }
            current += head.len() as u64;
            written = written.saturating_add(head.len() as u64);
            rest = tail;
            if current == part_bytes {
                let closed = close_piece(
                    &*provider_dest,
                    &mut sink,
                    current_dest.take(),
                    &observer,
                    &mut done,
                    written,
                    ctx,
                )
                .await;
                match closed {
                    Ok(Some(p)) => published.push(p),
                    Ok(None) => {}
                    Err(e) => undo_with!(sink, published, e),
                }
            }
        }
        ctx.progress.update(|p| p.bytes_done = written);
    }
    // The last one, which is almost never full. If the division was exact,
    // none is left open, and that is why an empty piece is NOT written at
    // the end.
    let closed = close_piece(
        &*provider_dest,
        &mut sink,
        current_dest.take(),
        &observer,
        &mut done,
        written,
        ctx,
    )
    .await;
    match closed {
        Ok(_) => Ok(()),
        Err(e) => undo_with!(sink, published, e),
    }
}

/// Removes the pieces already published from a split that got cut short.
///
/// Cancelling or failing halfway leaves a set that LOOKS complete, and that
/// is the trap this op exists not to lay: the pieces written are all of the
/// requested size, there is no gap, and joining the first three of ten gives
/// a short file that passes every guard. A tree copied halfway is visible at
/// a glance; half a set of pieces is not.
///
/// Every removal is recorded: the journal counts what is there, not what
/// there used to be. What cannot be removed is said in the log and not
/// retried — this path is already leaving because of an error.
async fn remove_pieces(
    provider_dest: &dyn Provider,
    published: &[VPath],
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) {
    for p in published.iter().rev() {
        match provider_dest.remove(p).await {
            Ok(()) => {
                let _ = observer
                    .on_mutation(&Mutation::Removed(p), &ctx.actor)
                    .await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "file.split: a half-finished piece could not be removed");
            }
        }
    }
}

/// The base name, the size and HOW MANY pieces are going to come out — or why
/// not.
///
/// Everything knowable before writing a byte, together: the piece is not
/// ridiculous, the source is a file, and the set fits the three-digit
/// convention. Finding out the last one at piece 1000 would leave a set
/// nobody can join back together.
pub(crate) async fn measures_the_distribution(
    src: &dyn Provider,
    path: &VPath,
    part_bytes: u64,
) -> Result<(Vec<u8>, u64, u64), Error> {
    if part_bytes < methods::FILE_SPLIT_MIN_BYTES {
        return Err(Error::InvalidPath);
    }
    let e = src.stat(path).await?;
    if e.kind != EntryKind::File {
        return Err(Error::InvalidPath);
    }
    let total = e.size.unwrap_or(0);
    let piece_count = total.div_ceil(part_bytes).max(1);
    if piece_count > methods::FILE_SPLIT_MAX_PARTS {
        return Err(Error::LimitExceeded {
            limit: "split-parts".to_owned(),
        });
    }
    let name = path
        .file_name()
        .map(|s| s.as_bytes().to_vec())
        .ok_or(Error::InvalidPath)?;
    Ok((name, total, piece_count))
}

/// No piece of the set may already exist.
///
/// Checked BEFORE writing the first one: finding out at the fourth leaves
/// three new pieces mixed in with the stale ones from an earlier batch, and
/// that set joins back together without anything creaking.
async fn check_slots_free(
    provider_dest: &dyn Provider,
    dest_dir: &VPath,
    name: &[u8],
    piece_count: u64,
) -> Result<(), Error> {
    for i in 1..=piece_count {
        let p = dest_dir
            .join(norte_proto::Segment::new(piece_name(name, i)).map_err(|_| Error::InvalidPath)?);
        if provider_dest.stat(&p).await.is_ok() {
            return Err(Error::Conflict {
                conflict: norte_proto::ConflictKind::Exists,
            });
        }
    }
    Ok(())
}

/// Publishes the open piece —if there is one— and records it in the journal.
///
/// The `Created` goes AFTER the commit, which is when the node exists (rule
/// 4).
async fn close_piece(
    provider: &dyn norte_vfs::Provider,
    sink: &mut Option<Box<dyn norte_vfs::ByteSink>>,
    dest: Option<VPath>,
    observer: &Arc<dyn MutationObserver>,
    done: &mut u64,
    written: u64,
    ctx: &TaskCtx,
) -> Result<Option<VPath>, Error> {
    let (Some(s), Some(dest)) = (sink.take(), dest) else {
        return Ok(None);
    };
    s.commit().await?;
    let node = crate::ops::identity_of(provider, &dest, observer).await;
    observer
        .on_mutation(&Mutation::Created { path: &dest, node }, &ctx.actor)
        .await?;
    *done += 1;
    let n = *done;
    ctx.progress.update(|p| {
        p.entries_done = n;
        p.bytes_done = written;
        p.current = Some(dest.clone());
    });
    Ok(Some(dest))
}

/// Is there any piece numbered ABOVE `through`?
///
/// This is the gap check, and it is done by listing: deriving the names one
/// by one up to 999 would be 999 `stat`s against a remote provider, and
/// stopping earlier is exactly the bug. The name is compared in BYTES against
/// `<base>.NNN` — nothing gets decoded (rule 1).
async fn has_pieces_above(
    src: &dyn Provider,
    dir: &VPath,
    base: &[u8],
    through: u64,
    ctx: &TaskCtx,
) -> Result<bool, Error> {
    let mut stream = src.list(dir).await?;
    while let Some(e) = stream.next().await {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let e = e?;
        let Some(name) = e.path.file_name().map(|s| s.as_bytes().to_vec()) else {
            continue;
        };
        // `<base>.NNN` and nothing else: `x.iso.001` counts, `x.iso.001.bak`
        // does not.
        let Some(tail) = name
            .strip_prefix(base)
            .and_then(|c| c.strip_prefix(b"."))
            .filter(|c| c.len() == 3 && c.iter().all(u8::is_ascii_digit))
        else {
            continue;
        };
        let n: u64 = std::str::from_utf8(tail)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if n > through {
            tracing::warn!("file.combine: an intermediate piece is missing");
            return Ok(true);
        }
    }
    Ok(false)
}

/// `file.combine`: joins the pieces of a split back together.
///
/// The pieces are enumerated and MEASURED before creating the destination:
/// that way a gap —or an intermediate piece shorter than the first, which is
/// a lost piece— is rejected without having written anything. A badly joined
/// file is a corrupt file that looks fine, and that is worse than an error.
pub(crate) async fn combine(
    src: Arc<dyn Provider>,
    first: VPath,
    provider_dest: Arc<dyn Provider>,
    dest: VPath,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let observer = crate::observer::pin_for_task(observer).await?;
    let name = first
        .file_name()
        .map(|s| s.as_bytes().to_vec())
        .ok_or(Error::InvalidPath)?;
    // `x.iso.001` → base `x.iso`.
    let base = name
        .len()
        .checked_sub(4)
        .filter(|n| name[*n] == b'.' && name[n + 1..].iter().all(u8::is_ascii_digit))
        .map(|n| name[..n].to_vec())
        .ok_or(Error::InvalidPath)?;
    let dir = first.parent().ok_or(Error::InvalidPath)?;
    let pieces = list_pieces(&*src, &dir, &base, ctx).await?;
    let total: u64 = pieces.iter().map(|(_, s)| *s).sum();
    ctx.progress.update(|p| {
        p.bytes_total = Some(total);
        p.entries_total = Some(pieces.len() as u64);
    });
    if provider_dest.stat(&dest).await.is_ok() {
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::Exists,
        });
    }
    write_combined(&*src, pieces, &*provider_dest, &dest, ctx).await?;
    let node = crate::ops::identity_of(&*provider_dest, &dest, &observer).await;
    observer
        .on_mutation(&Mutation::Created { path: &dest, node }, &ctx.actor)
        .await?;
    Ok(())
}

/// The set's pieces, in order and with their size — and every reason a set
/// does NOT join back together.
///
/// Split off from [`combine`] so that neither one goes past a hundred lines,
/// and because what is here is a single question with three ways to answer
/// no: a piece is missing, a piece is extra, or one in the middle is
/// half-finished.
async fn list_pieces(
    src: &dyn Provider,
    dir: &VPath,
    base: &[u8],
    ctx: &TaskCtx,
) -> Result<Vec<(VPath, u64)>, Error> {
    let mut pieces: Vec<(VPath, u64)> = Vec::new();
    let mut n = 1_u64;
    loop {
        let p = dir
            .join(norte_proto::Segment::new(piece_name(base, n)).map_err(|_| Error::InvalidPath)?);
        match src.stat(&p).await {
            Ok(e) if e.kind == EntryKind::File => {
                pieces.push((p, e.size.unwrap_or(0)));
                n += 1;
            }
            _ => break,
        }
        // Past the cap it is REFUSED, not cut short. Cutting here used to
        // join the first 999 of a set of 1200 —from 7-Zip, for example,
        // which numbers up to `.1000`— and publish a short file that passes
        // every guard: there is no gap and every piece collected is the same
        // size.
        if n > methods::FILE_SPLIT_MAX_PARTS {
            let next = dir.join(
                norte_proto::Segment::new(piece_name(base, n)).map_err(|_| Error::InvalidPath)?,
            );
            if src.stat(&next).await.is_ok() {
                return Err(Error::LimitExceeded {
                    limit: "split-parts".to_owned(),
                });
            }
            break;
        }
    }
    if pieces.is_empty() {
        return Err(Error::NotFound);
    }
    // **A GAP does not join across, and finding it takes LOOKING.** The walk
    // above stops at the first missing number, so a set `.001 .003 .004`
    // used to look like a single-piece one and would join: the task said
    // `Completed`, the journal recorded a `Created`, and on disk was left
    // 20% of an ISO that mounts as a corrupt image.
    if has_pieces_above(src, dir, base, pieces.len() as u64, ctx).await? {
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::TypeMismatch,
        });
    }
    // All but the last measure the same as the first. A shorter intermediate
    // one is a piece that was copied halfway, and joining across it gives a
    // file that looks whole.
    let first_size = pieces[0].1;
    if pieces[..pieces.len() - 1]
        .iter()
        .any(|(_, s)| *s != first_size)
    {
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::TypeMismatch,
        });
    }
    Ok(pieces)
}

/// Writes the destination from the pieces, in order.
async fn write_combined(
    src: &dyn Provider,
    pieces: Vec<(VPath, u64)>,
    provider_dest: &dyn Provider,
    dest: &VPath,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let mut sink = provider_dest.write(dest).await?;
    let mut written: u64 = 0;
    for (i, (p, _)) in pieces.iter().enumerate() {
        if ctx.cancel.is_cancelled() {
            let _ = sink.abort().await;
            return Err(Error::Cancelled);
        }
        ctx.progress.update(|q| q.current = Some(p.clone()));
        let mut stream = match src.read(p, None).await {
            Ok(s) => s,
            Err(e) => {
                let _ = sink.abort().await;
                return Err(e);
            }
        };
        while let Some(chunk) = stream.next().await {
            // Per CHUNK: a 700 MB piece cannot be a point where cancelling
            // does nothing for half a minute.
            if ctx.cancel.is_cancelled() {
                let _ = sink.abort().await;
                return Err(Error::Cancelled);
            }
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = sink.abort().await;
                    return Err(e);
                }
            };
            written = written.saturating_add(chunk.len() as u64);
            if let Err(e) = sink.write(chunk).await {
                let _ = sink.abort().await;
                return Err(e);
            }
            ctx.progress.update(|q| q.bytes_done = written);
        }
        ctx.progress.update(|q| q.entries_done = i as u64 + 1);
    }
    sink.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("test wire")
    }

    /// The report does NOT talk about folding collisions, and it is not an
    /// oversight: those **are not packed** — `walk_sources` rejects them, and
    /// there is a corpus test that pins it down
    /// (`dos_entradas_que_pliegan_al_mismo_nombre_no_se_empaquetan`, in
    /// `tests/engine_pack.rs`). An archive that EXISTS cannot carry them
    /// inside, so a list for them would never bring anything, and a list that
    /// never brings anything reads as "there is none".
    #[test]
    fn what_is_not_a_risk_is_not_reported() {
        let r = name_report(&[
            b"a.txt".to_vec(),
            b"b.txt".to_vec(),
            b"src/main.rs".to_vec(),
            // These two would not even get here in a real pack, and the
            // report stays quiet about them anyway: it is not its question.
            b"Makefile".to_vec(),
            b"makefile".to_vec(),
        ]);
        assert!(r.risky.is_empty());
        assert_eq!(r.entries, 5, "empty is a CLAIM: all five were looked at");
        assert!(!r.truncated);
    }

    /// The wire form of the stored name preserves the BYTES (rule 1), which
    /// is the only reason that codec exists: `name` carries the `U+FFFD` from
    /// displaying it and `path` is the only one the name is recovered from.
    #[test]
    fn the_stored_name_travels_by_its_bytes() {
        let r = name_report(&[b"malo\xff\\x.txt".to_vec()]);
        assert_eq!(r.risky.len(), 1);
        assert_eq!(
            r.risky[0].path, "malo%FF%5Cx.txt",
            "the byte that is not text comes out as %XX, and so does the backslash"
        );
        assert_eq!(
            r.risky[0].name, "malo\u{fffd}\\x.txt",
            "and the DISPLAY one is the usual one, with its loss"
        );
        // The slash separates components and is left readable; the rest is
        // escaped, so it stays unambiguous.
        let deep = name_report(&[b"dir/CON".to_vec()]);
        assert_eq!(deep.risky[0].path, "dir/CON");
        // And `%` gets escaped: without that the codec would not be
        // injective and two different names could travel identically.
        let percent = name_report(&[b"100%\\x".to_vec()]);
        assert_eq!(percent.risky[0].path, "100%25%5Cx");
    }

    /// The report SAYS which classes it looked at. Without that, a clean one
    /// would read as "the archive travels intact anywhere", which is more
    /// than has been verified: `<`, `>`, `"`, `|`, `?` and `*` are also
    /// illegal on Windows and are not looked at here.
    #[test]
    fn the_report_declares_what_it_checked() {
        let r = name_report(&[b"limpio.txt".to_vec()]);
        assert_eq!(
            r.checked,
            vec!["separator", "stream", "reserved", "trailing"]
        );
        assert!(r.risky.is_empty());
        let with_unchecked_illegal = name_report(&[b"pre<post.txt".to_vec()]);
        assert!(
            with_unchecked_illegal.risky.is_empty(),
            "today it is not checked, and that is why `checked` does not name it"
        );
    }

    /// The names that mean something else outside, one per class.
    #[test]
    fn names_that_mean_something_else_outside() {
        let r = name_report(&[
            b"a\\b.txt".to_vec(),
            b"f:ads".to_vec(),
            b"CON".to_vec(),
            b"name.".to_vec(),
            b"other ".to_vec(),
            b"normal.txt".to_vec(),
        ]);
        let by_risk = |which: &str| -> Vec<&str> {
            r.risky
                .iter()
                .filter(|x| x.risk == which)
                .map(|x| x.name.as_str())
                .collect()
        };
        assert_eq!(by_risk("separator"), vec!["a\\b.txt"]);
        assert_eq!(by_risk("stream"), vec!["f:ads"]);
        assert_eq!(by_risk("reserved"), vec!["CON"]);
        assert_eq!(by_risk("trailing").len(), 2, "the dot and the space");
        assert!(
            !r.risky.iter().any(|x| x.name == "normal.txt"),
            "an ordinary name does not go in"
        );
    }

    /// A reserved name is one PER COMPONENT and with the extension:
    /// `dir/CON.txt` cannot be extracted on Windows any more than `CON` can.
    #[test]
    fn reserved_is_checked_per_component_and_without_extension() {
        let r = name_report(&[
            b"dir/con.txt".to_vec(),
            b"dir/COM1".to_vec(),
            b"driver.rs".to_vec(),
            b"dir/NULLS.txt".to_vec(),
        ]);
        let names: Vec<&str> = r
            .risky
            .iter()
            .filter(|x| x.risk == "reserved")
            .map(|x| x.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["dir/con.txt", "dir/COM1"],
            "in the ORDER they were packed, which is how they are found"
        );
    }

    /// The caps do not lie about what they left out.
    #[test]
    fn a_truncated_report_says_so() {
        let many: Vec<Vec<u8>> = (0..methods::ARCHIVE_PACK_REPORT_MAX + 5)
            .map(|i| format!("d{i}/a\\b.txt").into_bytes())
            .collect();
        let r = name_report(&many);
        assert_eq!(r.risky.len(), methods::ARCHIVE_PACK_REPORT_MAX);
        assert!(r.truncated, "and it SAYS so");
        assert_eq!(
            r.entries,
            many.len() as u64,
            "the truncation is of the list, not of what was checked"
        );
    }

    /// The stored name comes from the BASE, and a path that does not hang off
    /// it has no name: making one up means putting into the archive something
    /// that unpacks where nobody expects it.
    #[test]
    fn the_stored_name_is_relative_to_the_base() {
        let base = vp("file:///proj");
        assert_eq!(
            relative_name(&base, &vp("file:///proj/src/main.rs")),
            Some(b"src/main.rs".to_vec())
        );
        assert_eq!(
            relative_name(&base, &vp("file:///proj/LEEME")),
            Some(b"LEEME".to_vec())
        );
        assert_eq!(relative_name(&base, &vp("file:///other/x")), None);
        assert_eq!(
            relative_name(&base, &base),
            None,
            "the base is not an entry"
        );
        assert_eq!(
            relative_name(&base, &vp("mem:///proj/x")),
            None,
            "nor from another provider"
        );
    }

    /// The `!` marker cannot be an entry's name: the read index skips that
    /// component, so writing it produces something norte cannot name again.
    #[test]
    fn the_marker_cannot_be_an_entry() {
        let base = vp("file:///proj");
        let with_marker = base.join(norte_proto::Segment::new(b"!".to_vec()).expect("seg"));
        assert_eq!(relative_name(&base, &with_marker), None);
    }

    /// What is checked depends on the format, and the report says so: saying
    /// "passes" about a plain tar would be claiming an integrity the format
    /// has nothing to back it up with.
    #[test]
    fn each_format_says_what_it_checks() {
        assert_eq!(that_is_checked("zip"), vec!["crc".to_owned()]);
        assert_eq!(that_is_checked("tar+gz"), vec!["gzip_crc".to_owned()]);
        assert_eq!(that_is_checked("tar"), vec!["sizes".to_owned()]);
        assert_eq!(that_is_checked("rar"), vec!["sizes".to_owned()]);
    }

    /// The container's format comes from its name, with the aliases people
    /// actually write: `.tgz` is `tar+gz`, and the case does not matter.
    #[test]
    fn the_container_format_comes_from_the_name() {
        assert_eq!(name_format(b"a.zip"), Some("zip"));
        assert_eq!(name_format(b"a.TGZ"), Some("tar+gz"));
        assert_eq!(name_format(b"a.tar.gz"), Some("tar+gz"));
        assert_eq!(name_format(b"a.tar"), Some("tar"));
        assert_eq!(
            name_format(b"a.rar"),
            Some("rar"),
            "reading it IS known how"
        );
        assert_eq!(name_format(b"leeme"), None);
    }

    /// Pieces are numbered with three digits starting from 001, which is the
    /// convention the users of these keys have.
    #[test]
    fn pieces_are_numbered_by_convention() {
        assert_eq!(piece_name(b"g.iso", 1), b"g.iso.001".to_vec());
        assert_eq!(piece_name(b"g.iso", 42), b"g.iso.042".to_vec());
        assert_eq!(piece_name(b"g.iso", 999), b"g.iso.999".to_vec());
    }
}
