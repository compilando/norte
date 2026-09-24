//! What this crate WRITES (#132), read by what this crate already knew
//! how to read.
//!
//! The reader here is the writer's oracle and not the other way around:
//! if the round trip closes, the archive that comes out is at least as
//! good as the one norte accepts from outside. What the round trip can't
//! say — whether some other `unzip` opens it — is covered by bit 11,
//! asserted separately because our reader keeps the raw bytes and doesn't
//! look at it.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt as _;
use norte_proto::{Segment, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;
use norte_vfs_archive::write::{ArchiveWriter, PackEntry, PackFormat};
use norte_vfs_archive::{ArchiveProvider, Format};

/// Packs `entries` and returns the archive's bytes.
fn pack(format: PackFormat, level: u32, entries: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut w = ArchiveWriter::new(format, level);
    let mut out = Vec::new();
    for (name, data) in entries {
        let mut e = PackEntry::file(name.clone(), data.len() as u64);
        e.mtime_ms = Some(1_700_000_000_000);
        w.begin(&e).expect("opens");
        // In pieces, which is how they arrive from a provider: the writer
        // has to behave the same with one chunk as with twenty.
        for chunk in data.chunks(7) {
            w.data(chunk).expect("data");
            out.extend(w.take());
        }
        w.end().expect("closes");
        out.extend(w.take());
    }
    w.finish().expect("finishes");
    out.extend(w.take());
    out
}

/// Reads an archive with this crate's provider: `(name, content)` of each
/// file in the tree.
async fn read_archive(format: Format, scheme: &str, bytes: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mem = Arc::new(MemProvider::new());
    let path = MemProvider::root().join(Segment::new(b"c.bin".to_vec()).expect("seg"));
    let mut sink = mem.write(&path).await.expect("write");
    sink.write(Bytes::copy_from_slice(bytes))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
    let token = match format {
        Format::Zip => "zip",
        Format::Tar => "tar",
        Format::TarGz => "tar+gz",
    };
    let root = VPath::archive_compose(token, &path, &[]).expect("compose");
    let p = ArchiveProvider::new(mem, format, scheme);
    let mut out = Vec::new();
    let mut stream = p.list(&root).await.expect("list");
    while let Some(e) = stream.next().await {
        let e = e.expect("entry");
        if e.kind != norte_proto::EntryKind::File {
            continue;
        }
        let name = e
            .path
            .file_name()
            .map(|s| s.as_bytes().to_vec())
            .expect("name");
        let mut data = Vec::new();
        let mut bs = p.read(&e.path, None).await.expect("read");
        while let Some(chunk) = bs.next().await {
            data.extend_from_slice(&chunk.expect("chunk"));
        }
        out.push((name, data));
    }
    out.sort();
    out
}

/// The normal case, in all three formats and with content that compresses
/// and content that doesn't.
#[tokio::test]
async fn all_three_formats_close_the_round_trip() {
    let entries = vec![
        (b"hi.txt".to_vec(), b"hi there".to_vec()),
        (b"repeat.txt".to_vec(), b"a".repeat(10_000)),
        (b"empty.txt".to_vec(), Vec::new()),
    ];
    let mut expected: Vec<(Vec<u8>, Vec<u8>)> = entries.clone();
    expected.sort();

    for (pf, f, scheme, level) in [
        (PackFormat::Zip, Format::Zip, "zip+mem", 6),
        (PackFormat::Zip, Format::Zip, "zip+mem", 0),
        (PackFormat::Tar, Format::Tar, "tar+mem", 0),
        (PackFormat::TarGz, Format::TarGz, "tar+gz+mem", 6),
    ] {
        let bytes = pack(pf, level, &entries);
        assert_eq!(
            read_archive(f, scheme, &bytes).await,
            expected,
            "{pf:?} level {level}"
        );
    }
}

/// **The test rule 1 demands.** Every hostile name from the canonical
/// corpus survives packing byte for byte, in zip and in tar.
///
/// A name is packed one at a time: the corpus has twins that would
/// collide with each other in the same archive, and what's tested here is
/// the name's trip, not the collision policy.
#[tokio::test]
async fn hostile_names_survive_packing() {
    for name in norte_testkit::corpus::hostile_names() {
        // Names addressing itself rejects never become entries of a
        // norte archive: the index already OMITS them at read time (ADR
        // 0018 C2, "omit with a signal"), so asking the writer to keep
        // them would be asking for a round trip the reader doesn't
        // promise. The `!` marker is the interesting case — it's a valid
        // `Segment` and still doesn't address inside an archive, hence it
        // gets checked separately. Packing a file named that is the
        // caller's business, and it refuses instead of writing an
        // unreachable entry.
        if Segment::new(name.bytes.clone()).is_err() || name.bytes == b"!" {
            continue;
        }
        let entries = vec![(name.bytes.clone(), b"content".to_vec())];
        for (pf, f, scheme) in [
            (PackFormat::Zip, Format::Zip, "zip+mem"),
            (PackFormat::Tar, Format::Tar, "tar+mem"),
        ] {
            let bytes = pack(pf, 6, &entries);
            let read_back = read_archive(f, scheme, &bytes).await;
            assert_eq!(read_back, entries, "{} did not survive {pf:?}", name.id);
        }
    }
}

/// Bit 11 tells the truth, both ways.
///
/// Our reader doesn't look at it — it keeps the raw bytes —, so the round
/// trip above would pass just the same with the bit always set. But other
/// programs DO decode by it, and setting it on a name that isn't UTF-8
/// turns the user's name into replacement characters in any unzip in the world.
#[test]
fn bit_11_is_only_set_when_the_name_is_utf8() {
    /// The local header's flags: bytes 6..8 of the file.
    fn first_header_flags(bytes: &[u8]) -> u16 {
        u16::from_le_bytes([bytes[6], bytes[7]])
    }
    const BIT_UTF8: u16 = 1 << 11;

    let utf8 = pack(
        PackFormat::Zip,
        6,
        &[(b"caf\xc3\xa9.txt".to_vec(), b"x".to_vec())],
    );
    assert_ne!(
        first_header_flags(&utf8) & BIT_UTF8,
        0,
        "a UTF-8 name is announced as such"
    );

    let raw = pack(
        PackFormat::Zip,
        6,
        &[(b"caf\xe9.txt".to_vec(), b"x".to_vec())],
    );
    assert_eq!(
        first_header_flags(&raw) & BIT_UTF8,
        0,
        "and one that isn't, NOT: lying here loses the name in any foreign reader"
    );
}

/// A name over 100 bytes is common and tar carries it in 100: without the
/// GNU extension it would get trimmed, and a trimmed name is a lost name.
#[tokio::test]
async fn a_long_name_survives_tar() {
    let long_name = format!("{}.txt", "n".repeat(200)).into_bytes();
    let entries = vec![(long_name.clone(), b"inside".to_vec())];
    let bytes = pack(PackFormat::Tar, 0, &entries);
    assert_eq!(read_archive(Format::Tar, "tar+mem", &bytes).await, entries);
}

/// Many entries: the central directory and its offsets have to line up
/// beyond the single-entry case.
#[tokio::test]
async fn a_zip_with_many_entries_reads_whole() {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = (0..500)
        .map(|i| {
            (
                format!("f{i:04}.txt").into_bytes(),
                format!("content {i}").into_bytes(),
            )
        })
        .collect();
    let mut expected = entries.clone();
    expected.sort();
    let bytes = pack(PackFormat::Zip, 6, &entries);
    assert_eq!(read_archive(Format::Zip, "zip+mem", &bytes).await, expected);
}
