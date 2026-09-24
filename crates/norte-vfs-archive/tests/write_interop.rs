//! What this crate writes, read by FOREIGN TOOLS (#250).
//!
//! `write_roundtrip.rs` reads what was written with our own index, which
//! is a good encoder↔decoder consistency check and BLIND to every place
//! where the rest of the world and we disagree — which is exactly where
//! the two format blockers of the #132 branch lived. During development
//! the zip writer was validated by hand against Python's `zipfile`, and
//! that's how the central directory layout bug came out. This turns that
//! manual check into a test.
//!
//! **It SKIPS with a message when the tool isn't there** (the same
//! convention as the wasm e2e tests): a machine without `unzip` can't say
//! anything about interoperability, and failing there would be calling a
//! defect something that isn't one. What it doesn't do is pass silently.

use std::io::Write as _;
use std::process::Command;

use norte_vfs_archive::write::{ArchiveWriter, PackEntry, PackFormat};

/// Packs `entries` and returns the archive's bytes.
fn pack(format: PackFormat, entries: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut w = ArchiveWriter::new(format, 6);
    let mut out = Vec::new();
    for (name, data) in entries {
        let mut e = PackEntry::file(name.clone(), data.len() as u64);
        e.mtime_ms = Some(1_700_000_000_000);
        w.begin(&e).expect("opens");
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

/// `true` if the tool is on the PATH. When it isn't, it's SAID.
fn has_tool(program: &str) -> bool {
    let ok = Command::new(program)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok();
    if !ok {
        eprintln!("skipped: `{program}` is not on the PATH");
    }
    ok
}

/// Writes `bytes` to a temp file and returns its path.
fn to_disk(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    let mut f = std::fs::File::create(&p).expect("create");
    f.write_all(bytes).expect("write");
    p
}

/// One of our zips is VERIFIED by `unzip -t`, which has the final word on
/// whether the central directory is where the rest of the world looks for it.
#[test]
fn our_zip_is_verified_by_unzip() {
    if !has_tool("unzip") {
        return;
    }
    let dir = tempfile::tempdir().expect("tmp");
    let bytes = pack(
        PackFormat::Zip,
        &[
            (b"one.txt".to_vec(), b"content one".to_vec()),
            (b"two/three.txt".to_vec(), b"and the one inside".to_vec()),
            // A non-ASCII name: bit 11 says it's in UTF-8, and whoever
            // reads it from outside is the only one who can confirm it
            // believes it.
            ("cafe\u{301}.txt".as_bytes().to_vec(), b"nfd".to_vec()),
        ],
    );
    let zip = to_disk(dir.path(), "n.zip", &bytes);

    let output = Command::new("unzip")
        .arg("-t")
        .arg(&zip)
        .output()
        .expect("unzip runs");
    assert!(
        output.status.success(),
        "unzip -t rejects our zip:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // And that it EXTRACTS with the same bytes inside, which is the real
    // question: `-t` validates the checksums, not that the content is ours.
    let outside = dir.path().join("outside");
    let output = Command::new("unzip")
        .arg("-q")
        .arg(&zip)
        .arg("-d")
        .arg(&outside)
        .output()
        .expect("unzip runs");
    assert!(output.status.success(), "unzip does not extract");
    assert_eq!(
        std::fs::read(outside.join("one.txt")).expect("one"),
        b"content one"
    );
    assert_eq!(
        std::fs::read(outside.join("two/three.txt")).expect("three"),
        b"and the one inside"
    );
}

/// One of our tars is listed by `tar -tvf`, and extracted with the same
/// bytes inside.
///
/// Includes a name OVER 100 bytes: it's classic `ustar`'s boundary, where
/// the writer has to emit a GNU `L` header — and our reader understands it
/// because we wrote it ourselves, which is exactly the circular argument
/// this file exists to break.
#[test]
fn our_tar_is_read_by_gnu_tar() {
    if !has_tool("tar") {
        return;
    }
    let dir = tempfile::tempdir().expect("tmp");
    let long_name = format!("{}.txt", "a".repeat(120));
    let bytes = pack(
        PackFormat::Tar,
        &[
            (b"one.txt".to_vec(), b"content one".to_vec()),
            (long_name.as_bytes().to_vec(), b"long header".to_vec()),
        ],
    );
    let tar = to_disk(dir.path(), "n.tar", &bytes);

    let output = Command::new("tar")
        .arg("-tvf")
        .arg(&tar)
        .output()
        .expect("tar runs");
    assert!(
        output.status.success(),
        "tar -tvf rejects our tar:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(listing.contains("one.txt"), "lists it: {listing}");
    assert!(
        listing.contains(&long_name),
        "and the whole long name, not truncated to 100: {listing}"
    );

    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir");
    let output = Command::new("tar")
        .arg("-xf")
        .arg(&tar)
        .arg("-C")
        .arg(&outside)
        .output()
        .expect("tar runs");
    assert!(
        output.status.success(),
        "tar does not extract:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(outside.join("one.txt")).expect("one"),
        b"content one"
    );
    assert_eq!(
        std::fs::read(outside.join(&long_name)).expect("the long one"),
        b"long header"
    );
}

/// And one of our `.tar.gz`, which adds a layer `gzip` has to recognize.
#[test]
fn our_targz_is_read_by_gnu_tar() {
    if !has_tool("tar") {
        return;
    }
    let dir = tempfile::tempdir().expect("tmp");
    let bytes = pack(
        PackFormat::TarGz,
        &[(b"one.txt".to_vec(), b"compressed".to_vec())],
    );
    let tgz = to_disk(dir.path(), "n.tar.gz", &bytes);

    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir");
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(&tgz)
        .arg("-C")
        .arg(&outside)
        .output()
        .expect("tar runs");
    assert!(
        output.status.success(),
        "tar -xzf rejects our tar.gz:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(outside.join("one.txt")).expect("one"),
        b"compressed"
    );
}
