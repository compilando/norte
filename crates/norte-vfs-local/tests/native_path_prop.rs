//! Property-based tests for the native path boundary: a name's bytes
//! survive the segment → OS → segment trip, and the conversions never
//! panic on garbage.

use futures::StreamExt;
use norte_proto::Segment;
use norte_testkit::strategies::{arb_hostile_filename, arb_segment_bytes};
use norte_vfs_local::LocalProvider;
use proptest::prelude::*;

proptest! {
    /// Roundtrip over the REAL FS: create a file with arbitrary name bytes
    /// and recover them intact via list. On unix any valid segment byte is
    /// a valid name byte; if the OS rejects it (e.g. APFS with non-UTF8),
    /// a clean rejection — never corruption.
    #[test]
    fn prop_filename_bytes_survive_fs(bytes in arb_segment_bytes()) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let p = LocalProvider::rooted(dir.path().to_path_buf());
            let root = LocalProvider::root();
            let seg = Segment::new(bytes.clone()).expect("the strategy generates valid segments");
            let path = root.join(seg);
            // If the OS rejects the name (e.g. APFS with non-UTF8): acceptable.
            if let Ok(mut sink) = norte_vfs::Provider::write(&p, &path).await {
                sink.write(bytes::Bytes::from_static(b"x"))
                    .await
                    .expect("chunk");
                // With the short staging (issue #4) the OS's rejection of
                // the FINAL name arrives at the commit rename: a clean
                // rejection = skip, any other error is a real failure.
                match sink.commit().await {
                    Ok(()) => {}
                    Err(norte_proto::Error::InvalidPath | norte_proto::Error::Conflict { .. }) => {
                        return Ok(());
                    }
                    Err(e) => panic!("commit: {e:?}"),
                }
                // The real test: the bytes the FS returns when LISTING
                // (`stat` echoes the input path; that proves nothing).
                let listed: Vec<Vec<u8>> = norte_vfs::Provider::list(&p, &root)
                    .await
                    .expect("list")
                    .map(|e| {
                        e.expect("ok entry")
                            .path
                            .file_name()
                            .expect("name")
                            .as_bytes()
                            .to_vec()
                    })
                    .collect()
                    .await;
                let exact = listed.iter().filter(|n| n.as_slice() == bytes.as_slice()).count();
                prop_assert_eq!(exact, 1, "the FS must return the exact bytes exactly once");
            }
            Ok(())
        })?;
    }

    /// Converting hostile names never panics, whether the OS accepts them or not.
    #[test]
    fn prop_hostile_names_never_panic(bytes in arb_hostile_filename()) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let p = LocalProvider::rooted(dir.path().to_path_buf());
            let root = LocalProvider::root();
            let seg = Segment::new(bytes).expect("the strategy generates valid segments");
            let _ = norte_vfs::Provider::stat(&p, &root.join(seg)).await;
        });
    }
}
