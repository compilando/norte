//! The parsers against the REAL output of an installed delegate. If none is
//! present on the machine, the test bows out saying so: `listing.rs`'s pure
//! suite already covers the grammar.

use futures::StreamExt;
use norte_testkit::RarSmith;
use norte_vfs_rar::{Delegate, LIST_TIMEOUT, parse_7z_slt, parse_unrar_vt};

/// A `.rar` forged with the three shapes that matter: ASCII, a nested name
/// and a name that is NOT UTF-8.
fn forge(dir: &std::path::Path) -> std::path::PathBuf {
    let bytes = RarSmith::new()
        .file(b"hello.txt", b"hello norte\n")
        .dir(b"dir")
        .file(b"dir/nested.txt", b"nested\n")
        .file(b"cp437-\xa4\xa5.txt", b"bytes\n")
        .build();
    let path = dir.join("t.rar");
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn seven_zip_lists_what_we_forged_with_the_bytes_intact() {
    let Some(sevenz) = norte_testkit::which_7z() else {
        eprintln!("no 7z installed: test withdrawn");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let archive = forge(tmp.path());
    let out = std::process::Command::new(sevenz)
        .args(["l", "-slt", "-p", "--"])
        .arg(&archive)
        .output()
        .expect("7z starts");
    let listing = parse_7z_slt(&out.stdout);
    let names: Vec<&[u8]> = listing.entries.iter().map(|e| e.name.as_slice()).collect();
    assert!(
        names.contains(&b"hello.txt".as_slice()),
        "entries read: {names:?}"
    );
    assert!(
        names.contains(&b"cp437-\xa4\xa5.txt".as_slice()),
        "7z preserves the raw bytes: {names:?}"
    );
    assert_eq!(listing.skipped, 0, "no real record is discarded");
    let dir = listing
        .entries
        .iter()
        .find(|e| e.name == b"dir")
        .expect("the directory is listed");
    assert!(dir.is_dir, "`dir` is a directory");
    let hello = listing
        .entries
        .iter()
        .find(|e| e.name == b"hello.txt")
        .unwrap();
    assert_eq!(hello.size, 12);
    assert!(hello.mtime.is_some(), "Modified is read");
}

#[test]
fn unrar_lists_the_same_except_the_name_it_cannot_carry() {
    let Some(unrar) = which_unrar() else {
        eprintln!("no unrar installed: test withdrawn");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let archive = forge(tmp.path());
    let out = std::process::Command::new(unrar)
        .args(["vt", "-p-", "--"])
        .arg(&archive)
        .output()
        .expect("unrar starts");
    let listing = parse_unrar_vt(&out.stdout);
    let names: Vec<&[u8]> = listing.entries.iter().map(|e| e.name.as_slice()).collect();
    assert!(
        names.contains(&b"hello.txt".as_slice()),
        "entries read: {names:?}"
    );
    assert_eq!(listing.skipped, 0, "no real record is discarded");
    // Measured: unrar TRUNCATES the non-UTF8 name at the first invalid byte,
    // so `cp437-\xa4\xa5.txt` does NOT appear whole. That is why 7z goes
    // first.
    assert!(
        !names.contains(&b"cp437-\xa4\xa5.txt".as_slice()),
        "if unrar stopped truncating, the preference order could be revisited"
    );
}

/// The real-world OEM name: `папка.txt` in CP866, which is what comes out of
/// a Russian DOS/Windows machine — and what a decade of downloads contains.
const OEM_CP866: &[u8] = b"\xaf\xa0\xaf\xaa\xa0.txt";

/// A **RAR4** with the name in an OEM code page (#223).
fn forge_rar4(dir: &std::path::Path) -> std::path::PathBuf {
    let bytes = RarSmith::new()
        .file(b"hello.txt", b"hello norte\n")
        .file(OEM_CP866, b"oem bytes\n")
        .build_rar4();
    let path = dir.join("t4.rar");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// **The case RAR5 cannot write: a name in an OEM code page** (#223).
///
/// RAR5 stores names in UTF-8 by format, so the forge above cannot produce
/// this and the gap had been open since ADR 0056. RAR4 can: without
/// `LHD_UNICODE` the name is raw bytes.
///
/// What it MEASURES, which are the three questions the issue left open:
/// `7z -slt` delivers the OEM bytes as-is, and the name it prints works for
/// re-selecting the entry. That is what backs 7z being the preferred
/// delegate — until now it had only been measured over RAR5.
#[test]
fn seven_zip_preserves_an_oem_name_from_a_rar4() {
    let Some(sevenz) = norte_testkit::which_7z() else {
        eprintln!("no 7z installed: test withdrawn");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let archive = forge_rar4(tmp.path());
    let out = std::process::Command::new(&sevenz)
        .args(["l", "-slt", "-p", "--"])
        .arg(&archive)
        .output()
        .expect("7z starts");
    let listing = parse_7z_slt(&out.stdout);
    let names: Vec<&[u8]> = listing.entries.iter().map(|e| e.name.as_slice()).collect();
    assert!(
        names.contains(&b"hello.txt".as_slice()),
        "the forged RAR4 reads: {names:?}"
    );
    assert!(
        names.contains(&OEM_CP866),
        "7z delivers the RAW OEM bytes, without transcoding: {names:?}"
    );
}

/// And the flip side of the same measurement: **`unrar` does NOT preserve
/// those bytes.**
///
/// It does not truncate them —which is what it does with a non-UTF8
/// RAR5, pinned in the test above— but instead maps them into a private-use
/// range. Two different breakages of the same delegate, and both lead to the
/// same place: over names that are not UTF-8, `unrar` is not a source of
/// truth.
#[test]
fn unrar_does_not_preserve_an_oem_name_from_a_rar4() {
    let Some(unrar) = which_unrar() else {
        eprintln!("no unrar installed: test withdrawn");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let archive = forge_rar4(tmp.path());
    let out = std::process::Command::new(unrar)
        .args(["vt", "-p-", "--"])
        .arg(&archive)
        .output()
        .expect("unrar starts");
    let listing = parse_unrar_vt(&out.stdout);
    let names: Vec<&[u8]> = listing.entries.iter().map(|e| e.name.as_slice()).collect();
    assert!(
        names.contains(&b"hello.txt".as_slice()),
        "the forged RAR4 also reads with unrar: {names:?}"
    );
    assert!(
        !names.contains(&OEM_CP866),
        "if unrar started delivering the raw bytes, the delegates' \
         preference order could be revisited: {names:?}"
    );
}

fn which_unrar() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("unrar"))
        .find(|c| c.is_file())
}

/// Rule 9's hardening does not break the real delegate: with an EMPTY
/// environment, `stdin` set to null and `cwd` outside the user's tree, `7z`
/// still lists — and `run_stream` delivers ONE entry's content.
#[tokio::test]
async fn with_rule_9_in_place_the_real_delegate_still_reads() {
    let Some(sevenz) = norte_testkit::which_7z() else {
        eprintln!("no 7z installed: test withdrawn");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let archive = forge(tmp.path());
    let d = Delegate::SevenZip(sevenz);
    let stdout = d
        .run_capture(&d.list_argv(&archive), LIST_TIMEOUT)
        .await
        .expect("7z lists with an empty environment");
    let listing = parse_7z_slt(&stdout);
    assert!(
        listing.entries.iter().any(|e| e.name == b"hello.txt"),
        "listed with rule 9 in place"
    );

    let argv = d.read_argv(&archive, b"hello.txt");
    let stream = d
        .run_stream(&argv, tokio_util::sync::CancellationToken::new())
        .await
        .expect("7z extracts to stdout");
    let bytes: Vec<u8> = stream
        .map(|r| r.expect("the stream does not fail"))
        .fold(Vec::new(), |mut acc, c| async move {
            acc.extend_from_slice(&c);
            acc
        })
        .await;
    assert_eq!(bytes, b"hello norte\n", "the content arrives whole");
}
