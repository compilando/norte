//! Hostile cases for the zip provider: name encoding (bit 11, cp437),
//! zip-slip, a lying EOCD, encryption/odd methods, real deflate and ranges.

mod common;

use std::io::Write as _;

use futures::StreamExt;
use norte_proto::{ByteRange, Error, Segment, VPath};
use norte_testkit::ZipSmith;
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Limits};

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("seg")
}

async fn list_names(p: &ArchiveProvider, dir: &VPath) -> Vec<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = p
        .list(dir)
        .await
        .expect("list")
        .map(|e| {
            e.expect("entry ok")
                .path
                .file_name()
                .expect("has a name")
                .as_bytes()
                .to_vec()
        })
        .collect()
        .await;
    names.sort();
    names
}

async fn read_all(p: &ArchiveProvider, f: &VPath, range: Option<ByteRange>) -> Vec<u8> {
    let mut stream = p.read(f, range).await.expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    out
}

/// A REAL deflate zip: flate2 produces the compressed bytes and `ZipSmith`
/// forges the structure (#59: without the `zip` crate, not even in tests).
fn deflate_zip(name: &[u8], data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).expect("deflate");
    let deflated = enc.finish().expect("finish");
    ZipSmith::new().file_deflate(name, data, &deflated).build()
}

#[tokio::test]
async fn raw_cp437_with_bit11_off_lists_byte_exact() {
    // CAFÉ.TXT in cp437 (É = 0x90): raw bytes, never decoded.
    let zip = ZipSmith::new().file(b"CAF\x90.TXT", b"1980s").build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(list_names(&p, &root).await, vec![b"CAF\x90.TXT".to_vec()]);
    assert_eq!(
        read_all(&p, &root.join(seg(b"CAF\x90.TXT")), None).await,
        b"1980s"
    );
}

#[tokio::test]
async fn a_lying_bit11_neither_decodes_nor_panics() {
    // Bit 11 says "UTF-8" but the bytes are NOT: the flag is an
    // announcement, the bytes rule (rule 1).
    let zip = ZipSmith::new()
        .file_utf8(b"lie-\xff\xfe.txt", b"liar")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"lie-\xff\xfe.txt".to_vec()]
    );
    assert_eq!(
        read_all(&p, &root.join(seg(b"lie-\xff\xfe.txt")), None).await,
        b"liar"
    );
}

#[tokio::test]
async fn zip_slip_and_the_marker_are_omitted() {
    let zip = ZipSmith::new()
        .file(b"../evil", b"slip")
        .file(b"/etc/passwd", b"abs")
        .file(b"a/../b", b"dotdot")
        .file(b"!", b"marker")
        .file(b"a/!/b", b"marker-inside")
        .file(b"ok.txt", b"fine")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    // Only `ok.txt` and the implicit `a` dir survive (from `a/../b`
    // nothing: the whole entry is omitted; `a` exists because of `a/!/b`…
    // that one too gets omitted whole. Checks exactly what's left.)
    assert_eq!(list_names(&p, &root).await, vec![b"ok.txt".to_vec()]);
}

#[tokio::test]
async fn duplicate_and_file_vs_dir() {
    let zip = ZipSmith::new()
        .file(b"x", b"one")
        .file(b"x", b"two!!")
        .file(b"d", b"i-am-a-file")
        .dir(b"d")
        .file(b"d/child", b"alive")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(read_all(&p, &root.join(seg(b"x")), None).await, b"two!!");
    assert_eq!(
        read_all(&p, &root.join(seg(b"d")).join(seg(b"child")), None).await,
        b"alive",
        "the dir wins over the same-named file and the subtree survives"
    );
}

#[tokio::test]
async fn a_lying_eocd_cuts_without_paying_for_the_index() {
    let zip = ZipSmith::new().file(b"x", b"1").build_lying_eocd(60_000);
    let limits = Limits {
        max_entries: 100,
        ..Limits::default()
    };
    let (p, root) = common::zip_provider_with_limits(&zip, limits).await;
    // #95.3: the cut from the EOCD's announcement is LimitExceeded, not
    // Corrupt — at this point it's unknown whether the EOCD is lying or
    // the zip is legitimate and huge (and it precisely refuses to pay for
    // the index to find out).
    match p.list(&root).await.map(|_| ()) {
        Err(Error::LimitExceeded { limit }) if limit == "entries" => {}
        other => {
            panic!("expected LimitExceeded(entries) from an over-announcing EOCD, got {other:?}")
        }
    }
}

#[tokio::test]
async fn an_empty_zip_and_garbage() {
    // A zip with no entries is VALID: an empty root.
    let (p, root) = common::zip_provider(&ZipSmith::new().build()).await;
    assert!(list_names(&p, &root).await.is_empty());
    // Garbage with no EOCD: Corrupt.
    let (p, root) = common::zip_provider(b"i am not a zip").await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::Corrupt) => {}
        other => panic!("expected Corrupt, got {other:?}"),
    }
    // A 0-byte container: Corrupt.
    let (p, root) = common::zip_provider(b"").await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::Corrupt) => {}
        other => panic!("expected Corrupt with 0 bytes, got {other:?}"),
    }
}

#[tokio::test]
async fn encrypted_and_odd_method_list_but_do_not_read() {
    let zip = ZipSmith::new()
        .file_raw(b"secret.bin", b"garbage", 0, 1) // bit 0: encrypted
        .file_raw(b"exotic.bin", b"garbage", 12, 0) // method 12 (bzip2, no feature)
        .file(b"normal.txt", b"ok")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![
            b"exotic.bin".to_vec(),
            b"normal.txt".to_vec(),
            b"secret.bin".to_vec()
        ],
        "encrypted/exotic ones ARE LISTED (metadata)"
    );
    for name in [b"secret.bin".as_slice(), b"exotic.bin"] {
        match p.read(&root.join(seg(name)), None).await.map(|_| ()) {
            Err(Error::Unsupported) => {}
            other => panic!("expected Unsupported on {name:?}, got {other:?}"),
        }
    }
    assert_eq!(
        read_all(&p, &root.join(seg(b"normal.txt")), None).await,
        b"ok"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn real_deflate_roundtrip_and_ranges() {
    // Compressible, big data (several 64 KiB reader chunks).
    let data: Vec<u8> = (0..500_000u32).map(|i| (i % 7) as u8).collect();
    let zip = deflate_zip(b"big.bin", &data);
    let (p, root) = common::zip_provider(&zip).await;
    let f = root.join(seg(b"big.bin"));
    assert_eq!(
        p.stat(&f).await.expect("stat").size,
        Some(data.len() as u64)
    );
    assert_eq!(read_all(&p, &f, None).await, data);
    // Range over DECOMPRESSED bytes.
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 1,
                len: Some(3)
            })
        )
        .await,
        &data[1..4]
    );
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 499_998,
                len: None
            })
        )
        .await,
        &data[499_998..]
    );
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 999_999,
                len: Some(1)
            })
        )
        .await,
        b"",
        "decompressed past-EOF: empty stream"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_stream_cancels_the_decompression() {
    let data: Vec<u8> = (0..4_000_000u32).map(|i| (i % 13) as u8).collect();
    let zip = deflate_zip(b"huge.bin", &data);
    let (p, root) = common::zip_provider(&zip).await;
    let f = root.join(seg(b"huge.bin"));
    let mut stream = p.read(&f, None).await.expect("read");
    let first = stream.next().await.expect("there's a chunk").expect("ok");
    assert!(!first.is_empty());
    drop(stream); // the blocking thread dies on the next send (closed channel)
    // Rule 3: nothing hangs — the test ends; the orphaned thread is bounded
    // to 4 in-flight chunks. Re-reading it whole verifies nothing broke.
    assert_eq!(read_all(&p, &f, None).await, data);
}

#[tokio::test]
async fn a_trailing_backslash_is_a_readable_file() {
    // H2 (8e audit): a trailing `\` is NEITHER a separator NOR a dir
    // marker — the zip crate decodes it as a dir; norte decides by BYTES.
    let zip = ZipSmith::new().file(b"trailing\\", b"data").build();
    let (p, root) = common::zip_provider(&zip).await;
    let f = root.join(seg(b"trailing\\"));
    let e = p.stat(&f).await.expect("stat");
    assert_eq!(e.kind, norte_proto::EntryKind::File);
    assert_eq!(read_all(&p, &f, None).await, b"data");
}

#[tokio::test]
async fn a_file_then_a_child_ascends_it_to_a_dir() {
    // H4 (8e audit): `file a` + `a/child` — the file ascends to a dir and
    // the subtree stays visible (before: TypeMismatch and an orphaned
    // subtree).
    let zip = ZipSmith::new()
        .file(b"a", b"i-am-a-file")
        .file(b"a/child", b"alive")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    let a = root.join(seg(b"a"));
    assert_eq!(
        p.stat(&a).await.expect("stat").kind,
        norte_proto::EntryKind::Dir
    );
    assert_eq!(list_names(&p, &a).await, vec![b"child".to_vec()]);
    assert_eq!(read_all(&p, &a.join(seg(b"child")), None).await, b"alive");
}

#[tokio::test]
async fn a_name_with_nul_is_omitted() {
    // H7: the only possible NUL in a zip comes forged — it's cleanly omitted.
    let zip = ZipSmith::new()
        .file(b"nul\x00byte.txt", b"x")
        .file(b"ok.txt", b"fine")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(list_names(&p, &root).await, vec![b"ok.txt".to_vec()]);
}

#[tokio::test]
async fn a_fake_eocd_signature_in_the_comment_does_not_break() {
    // H9: a comment that CONTAINS an EOCD signature (with a huge counter)
    // must not make the preflight reject a valid zip.
    let mut fake = b"PK\x05\x06".to_vec();
    fake.extend_from_slice(&[0u8; 6]);
    fake.extend_from_slice(&60_000u16.to_le_bytes()); // fake count_total
    fake.extend_from_slice(&60_000u16.to_le_bytes());
    fake.extend_from_slice(&[0u8; 6]);
    let zip = ZipSmith::new()
        .file(b"real.txt", b"ok")
        .comment(&fake)
        .build();
    let limits = Limits {
        max_entries: 100,
        ..Limits::default()
    };
    let (p, root) = common::zip_provider_with_limits(&zip, limits).await;
    assert_eq!(list_names(&p, &root).await, vec![b"real.txt".to_vec()]);
}

#[tokio::test]
async fn h1_names_that_collapsed_in_lossy_no_longer_collapse() {
    // H1 (#59): the `zip` crate indexed the CD by the DECODED name — two
    // different raw names that decode to the same U+FFFD used to collapse
    // into ONE entry (last one wins) before norte ever saw them. With the
    // CD's own parser, both names live byte-exact.
    let zip = ZipSmith::new()
        .file_utf8(b"lossy-\xff.txt", b"FIRST")
        .file_utf8(b"lossy-\xfe.txt", b"SECOND")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"lossy-\xfe.txt".to_vec(), b"lossy-\xff.txt".to_vec()],
        "both entries list byte-exact, no lossy collapse"
    );
    assert_eq!(
        p.list_skipped(&root).await.expect("skipped"),
        Some(0),
        "nothing was omitted or lost"
    );
    assert_eq!(
        read_all(&p, &root.join(seg(b"lossy-\xff.txt")), None).await,
        b"FIRST"
    );
    assert_eq!(
        read_all(&p, &root.join(seg(b"lossy-\xfe.txt")), None).await,
        b"SECOND"
    );
}

/// A VALID 0x7075 extra field (Info-ZIP unicode path): version 1 + the
/// header name's crc32 + an alternate unicode name.
fn extra_7075(unicode: &[u8], header_name: &[u8]) -> Vec<u8> {
    let mut crc = flate2::Crc::new();
    crc.update(header_name);
    let mut body = vec![1u8]; // version
    body.extend_from_slice(&crc.sum().to_le_bytes());
    body.extend_from_slice(unicode);
    let mut out = 0x7075u16.to_le_bytes().to_vec();
    out.extend_from_slice(
        &u16::try_from(body.len())
            .expect("extra too short")
            .to_le_bytes(),
    );
    out.extend_from_slice(&body);
    out
}

#[tokio::test]
async fn a_valid_extra_7075_never_replaces_the_name() {
    // H3 (#59): the `zip` crate USED TO REPLACE the CD's name with the
    // 0x7075 extra's one when its crc validated. The own parser ignores it
    // by design: the CD's bytes rule (rule 1).
    let extra = extra_7075(b"impostor.txt", b"cd-name.txt");
    let zip = ZipSmith::new()
        .file_with_extra(b"cd-name.txt", b"content", &extra)
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"cd-name.txt".to_vec()],
        "the CD's RAW name rules; the 0x7075 is ignored"
    );
    assert_eq!(
        read_all(&p, &root.join(seg(b"cd-name.txt")), None).await,
        b"content"
    );
}

#[tokio::test]
async fn an_invalid_extra_7075_does_not_kill_the_archive() {
    // H3 (#59): a malformed 0x7075 (a size that overflows the blob) used
    // to abort the WHOLE archive in the `zip` crate. Now: a truncated
    // record just stops walking the blob and the entry survives with its
    // CD name.
    let mut extra = 0x7075u16.to_le_bytes().to_vec();
    extra.extend_from_slice(&200u16.to_le_bytes()); // promises 200, there are 3
    extra.extend_from_slice(&[1, 2, 3]);
    let zip = ZipSmith::new()
        .file_with_extra(b"survivor.txt", b"alive", &extra)
        .file(b"neighbor.txt", b"too")
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"neighbor.txt".to_vec(), b"survivor.txt".to_vec()]
    );
    assert_eq!(
        read_all(&p, &root.join(seg(b"survivor.txt")), None).await,
        b"alive"
    );
}

#[tokio::test]
async fn zip64_eocd_counts_and_reads() {
    // zip64 (#59): an EOCD with markers -> locator -> EOCD64. The entry
    // lists and reads byte-exact.
    let zip = ZipSmith::new()
        .file(b"z64.txt", b"content-64")
        .build_zip64();
    let (p, root) = common::zip_provider(&zip).await;
    assert_eq!(list_names(&p, &root).await, vec![b"z64.txt".to_vec()]);
    assert_eq!(
        read_all(&p, &root.join(seg(b"z64.txt")), None).await,
        b"content-64"
    );

    // And a lying zip64 count above max_entries cuts BEFORE paying for
    // the CD (the preflight's u16 gap stays closed, #59).
    let liar = ZipSmith::new()
        .file(b"z64.txt", b"x")
        .build_zip64_lying_count(1_000_000);
    let limits = Limits {
        max_entries: 100,
        ..Limits::default()
    };
    let (p, root) = common::zip_provider_with_limits(&liar, limits).await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::LimitExceeded { limit }) if limit == "entries" => {}
        other => panic!("expected LimitExceeded(entries) from a lying EOCD64, got {other:?}"),
    }
}

#[tokio::test]
async fn a_lying_crc_in_a_full_read_is_corrupt() {
    // #59: the FULL read (the copy path) verifies the CD's CRC over the
    // served bytes — a lying CD ends in Err(Corrupt) as the LAST item of
    // the stream, never silently corrupt data.
    let mut bytes = ZipSmith::new().file(b"lie.bin", b"content").build();
    let cd = bytes
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .expect("CD signature");
    bytes[cd + 16..cd + 20].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // fake crc
    let (p, root) = common::zip_provider(&bytes).await;
    let f = root.join(seg(b"lie.bin"));
    let (seen, failure) = read_until_failure(&p, &f).await;
    match failure {
        Some(Error::Corrupt) => {}
        other => panic!("expected Corrupt from a lying CRC (seen {seen}), got {other:?}"),
    }
    // A PARTIAL range of the same entry can't be verified without
    // decompressing the whole entry: it serves the bytes with no CRC
    // (documented).
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 1,
                len: Some(3)
            })
        )
        .await,
        b"ont"
    );
}

/// Drains a read expecting the stream to FAIL; returns (`bytes_ok`, error).
async fn read_until_failure(p: &ArchiveProvider, f: &VPath) -> (usize, Option<Error>) {
    let mut stream = p.read(f, None).await.expect("read opens");
    let mut seen = 0usize;
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => seen += chunk.len(),
            Err(e) => return (seen, Some(e)),
        }
    }
    (seen, None)
}

/// Robustness: an entry whose CD promises more bytes than the CONTAINER
/// has fails `Corrupt` (here the `zip` crate's own CRC already caught it
/// when the reader ran out — the silent path's pin is the test below).
#[tokio::test]
async fn a_zip_entry_promising_more_bytes_than_the_container_is_corrupt() {
    let mut bytes = ZipSmith::new().file(b"short.bin", &[9u8; 100]).build();
    // Surgery on the CD (the only entry): comp/uncomp become 1 MiB — way
    // past the container's end. The local header stays as-is (the `zip`
    // crate reads with the CD's sizes).
    let cd = bytes
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .expect("CD signature");
    let lie = (1u32 << 20).to_le_bytes();
    bytes[cd + 20..cd + 24].copy_from_slice(&lie); // compressed size
    bytes[cd + 24..cd + 28].copy_from_slice(&lie); // uncompressed size
    let (p, root) = common::zip_provider(&bytes).await;
    let (seen, failure) = read_until_failure(&p, &root.join(seg(b"short.bin"))).await;
    match failure {
        Some(Error::Corrupt) => {}
        other => panic!("expected Corrupt (saw {seen} bytes), got {other:?}"),
    }
}

/// #95.4 — THE silent path (parity with targz's FIX-1): a deflate stream
/// that ends CLEANLY (a valid final block) before the `uncompressed_size`
/// the CD promises, with a CRC consistent with the SHORT data. The `zip`
/// crate has nothing to object to (valid deflate, CRC ok) -> the take-loop
/// sees `Ok(0)` with `remaining > 0`. Before the fix it returned a partial
/// file with NO WARNING; now it's `Corrupt`.
#[tokio::test]
async fn short_zip_deflate_with_consistent_crc_is_corrupt_not_short_data() {
    // A VALID raw deflate of just 2 bytes.
    let short = b"AB";
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(short).expect("deflate");
    let deflated = enc.finish().expect("finish");
    // ZipSmith writes comp=uncomp=len(deflated) and the crc of the raw
    // bytes: surgery on LOCAL and CD — uncomp lies 100, crc = crc32(decompressed).
    let mut crc = flate2::Crc::new();
    crc.update(short);
    let crc_ok = crc.sum().to_le_bytes();
    let mut bytes = ZipSmith::new()
        .file_raw(b"short.bin", &deflated, 8, 0)
        .build();
    let lie = 100u32.to_le_bytes();
    let local = bytes
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x03, 0x04])
        .expect("local signature");
    bytes[local + 14..local + 18].copy_from_slice(&crc_ok);
    bytes[local + 22..local + 26].copy_from_slice(&lie); // uncomp (local)
    let cd = bytes
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .expect("CD signature");
    bytes[cd + 16..cd + 20].copy_from_slice(&crc_ok);
    bytes[cd + 24..cd + 28].copy_from_slice(&lie); // uncomp (CD)
    let (p, root) = common::zip_provider(&bytes).await;
    let (seen, failure) = read_until_failure(&p, &root.join(seg(b"short.bin"))).await;
    match failure {
        Some(Error::Corrupt) => {}
        other => {
            panic!("expected Corrupt, got {other:?} with {seen} bytes — silently short data")
        }
    }
}

/// #95.3 (review MAJOR-1): the zip's skipped-entries budget is a LOCAL
/// limit — many hostile entries with a tight `max_entries` must fail
/// `LimitExceeded("entries")`, not `Corrupt` (parity with tar/targz).
#[tokio::test]
async fn zips_skipped_budget_is_limit_exceeded() {
    let mut smith = ZipSmith::new();
    for i in 0..5u32 {
        // Traversal: each one gets OMITTED (counted in skipped, not in the index).
        smith = smith.file(format!("../evil{i}").as_bytes(), b"x");
    }
    let limits = Limits {
        max_entries: 2,
        ..Limits::default()
    };
    let (p, root) = common::zip_provider_with_limits(&smith.build(), limits).await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::LimitExceeded { limit }) if limit == "entries" => {}
        other => panic!("expected LimitExceeded(entries) from omitted ones, got {other:?}"),
    }
}

/// enc MAJOR-1 (review #59, bug CONFIRMED pre-fix): a STORED entry whose
/// CD lies `uncomp > comp` used to serve NEIGHBORING bytes from the
/// container on a ranged read (the local header next door came out as
/// content, silently). APPNOTE demands comp == uncomp for stored: fail
/// loud `Corrupt` on ANY read, never foreign data attributed to the entry.
#[tokio::test]
async fn stored_with_a_lying_uncomp_is_corrupt_never_neighboring_bytes() {
    let mut zip = ZipSmith::new().file(b"small.txt", b"hi").build();
    // Surgery: inflates the CD's uncomp_size (offset +24 of the
    // 0x02014b50 record) from 2 to 40. The comp_size (+20) stays at 2:
    // the bug's exact lie.
    let cd = zip
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .expect("CD record");
    assert_eq!(
        u32::from_le_bytes(zip[cd + 24..cd + 28].try_into().expect("u32")),
        2,
        "original uncomp"
    );
    zip[cd + 24..cd + 28].copy_from_slice(&40u32.to_le_bytes());

    let (provider, root) = common::zip_provider(&zip).await;
    let path = root.join(Segment::new(b"small.txt".to_vec()).expect("seg"));
    // A ranged read BEYOND the real data: it used to return bytes from the
    // neighboring local header with err=None; now Corrupt.
    let mut stream = provider
        .read(
            &path,
            Some(ByteRange {
                offset: 10,
                len: Some(8),
            }),
        )
        .await
        .expect("read opens (the plan is validated in the thread)");
    let mut err = None;
    let mut got = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(b) => got.extend_from_slice(&b),
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    assert!(matches!(err, Some(Error::Corrupt)), "got {err:?}");
    assert!(
        got.is_empty(),
        "not one neighboring byte in silence: {got:x?}"
    );
}

/// rust MAJOR-1 (review #59, rule 3): dropping the stream during the
/// DISCARD phase of a ranged deflate (which sends nothing to the channel)
/// cuts the blocking thread — without the closed-channel check, it would
/// keep decompressing the whole skip for nobody.
#[tokio::test]
async fn dropping_the_stream_during_the_discard_cuts_the_thread() {
    // 4 MiB INCOMPRESSIBLE (deterministic xorshift): comp ≈ uncomp, so
    // discarding ~4 MiB decompressed demands reading ~16 256 KiB blocks
    // of the container — a signal measurable via Faults::read_calls.
    let mut data = vec![0u8; 4 * 1024 * 1024];
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    for b in &mut data {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "xorshift: the low byte is what's wanted"
        )]
        {
            *b = x as u8;
        }
    }
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(&data).expect("deflate");
    let deflated = enc.finish().expect("finish");
    let zip = ZipSmith::new()
        .file_deflate(b"big.bin", &data, &deflated)
        .build();

    let (mem, path) = common::seed_container(b"fixture.zip", &zip).await;
    let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
    let provider = ArchiveProvider::with_limits(
        std::sync::Arc::clone(&mem) as std::sync::Arc<dyn Provider>,
        norte_vfs_archive::Format::Zip,
        "zip+mem",
        Limits::default(),
    );
    let entry = root.join(Segment::new(b"big.bin".to_vec()).expect("seg"));
    // Warms up the index (its reads don't count toward the assertion).
    let _ = provider.stat(&entry).await.expect("stat");
    let base = mem.faults().read_calls();

    // A ranged read with a DEEP skip… and an immediate drop of the stream.
    let stream = provider
        .read(
            &entry,
            Some(ByteRange {
                offset: 3_900_000,
                len: Some(16),
            }),
        )
        .await
        .expect("read opens");
    drop(stream);

    // Waits for the counter to SETTLE (the blocking thread dies on its own).
    let mut last = mem.faults().read_calls();
    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let now = mem.faults().read_calls();
        if now == last {
            break;
        }
        last = now;
    }
    let spent = last.saturating_sub(base);
    // With the fix: data_offset (1 block) + at most a couple of discard
    // iterations before seeing the closed channel. Without the fix: ~16
    // blocks of the container (the whole discard).
    assert!(
        spent <= 6,
        "the discard kept going after the drop: {spent} reads"
    );
}

/// Pins #59's decision (rust MAJOR-3 / enc MINOR-4): a zip with PREPENDED
/// data (a self-extractor) gets rejected as `Corrupt` — acceptance
/// demands the EOCD's exact self-consistency (`cd_off + cd_size == pos`),
/// which is precisely what makes rejecting fake signatures in the comment
/// (H9) solid. A conscious decision, not an accident: Info-ZIP's "offset
/// fudge" stays a future feature if real demand shows up.
#[tokio::test]
async fn a_zip_with_prepended_data_is_rejected_documented() {
    let zip = ZipSmith::new().file(b"a.txt", b"x").build();
    let mut sfx = b"#!/bin/sh\necho stub\n".to_vec();
    sfx.extend_from_slice(&zip);
    let (provider, root) = common::zip_provider(&sfx).await;
    // The failure can come out of the direct list or of the stream: both are accepted.
    if let Err(e) = provider.list(&root).await {
        assert!(matches!(e, Error::Corrupt), "got {e:?}");
        return;
    }
    let mut stream = provider.list(&root).await.expect("list");
    let mut got_err = None;
    while let Some(item) = stream.next().await {
        if let Err(e) = item {
            got_err = Some(e);
            break;
        }
    }
    assert!(matches!(got_err, Some(Error::Corrupt)), "got {got_err:?}");
}

// ---------- zip attrs (#108 block 2) ----------

#[tokio::test]
async fn attrs_zip_method_packed_crc() {
    use norte_proto::AttrValue;
    use norte_vfs::{AttrRequest, ListOptions};

    // A normal entry (store) and an ENCRYPTED one (bit 0: no locator).
    let zip = ZipSmith::new()
        .file(b"normal.txt", b"content")
        .file_raw(b"encrypted.txt", b"xxxx", 8, 1)
        .build();
    let (p, root) = common::zip_provider(&zip).await;
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(
            ["archive.method", "archive.packed_size", "archive.crc32"].map(str::to_owned),
        ),
    };
    let e = p
        .stat_with(&root.join(seg(b"normal.txt")), &opt)
        .await
        .expect("stat_with");
    assert_eq!(
        e.attrs.get("archive.method"),
        Some(&AttrValue::Text("store".to_owned()))
    );
    assert_eq!(
        e.attrs.get("archive.packed_size"),
        Some(&AttrValue::Uint(b"content".len() as u64))
    );
    assert!(matches!(
        e.attrs.get("archive.crc32"),
        Some(AttrValue::Uint(_))
    ));

    // The ENCRYPTED one (unreadable, locator None) DOES keep its attrs:
    // method is precisely more interesting there.
    let enc = p
        .stat_with(&root.join(seg(b"encrypted.txt")), &opt)
        .await
        .expect("stat_with of the encrypted one");
    assert_eq!(
        enc.attrs.get("archive.method"),
        Some(&AttrValue::Text("deflate".to_owned()))
    );

    // list_with carries the same per entry; when not requested -> nothing.
    let mut s = p.list_with(&root, &opt).await.expect("list_with");
    while let Some(e) = s.next().await {
        assert!(e.expect("entry").attrs.contains_key("archive.method"));
    }
    assert!(
        p.stat(&root.join(seg(b"normal.txt")))
            .await
            .expect("stat")
            .attrs
            .is_empty()
    );
}
