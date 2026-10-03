//! File-to-file copy without the bytes leaving the kernel (ADR 0165): a
//! reflink first, then `copy_file_range`. Linux only; elsewhere every call
//! answers "cannot" and the caller streams.

use std::fs::File;

use norte_proto::Error;

/// Bytes per `copy_file_range` call: between two calls the copy reports
/// progress and can be stopped. Small enough that a slow USB stick still
/// answers a cancel in about a second.
#[cfg(target_os = "linux")]
const BLOCK: usize = 16 * 1024 * 1024;

/// Copies `src`, from its offset to its end, into the EMPTY `dst` at its
/// offset. `Ok(None)` = the kernel cannot do it here and NOTHING was
/// written. `progress(done)` returning `false` stops the copy with
/// [`Error::Cancelled`].
#[cfg(target_os = "linux")]
pub(crate) fn copy(
    src: &File,
    dst: &File,
    progress: &(dyn Fn(u64) -> bool + Send + Sync),
) -> Result<Option<u64>, Error> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let (src_fd, dst_fd) = (src.as_raw_fd(), dst.as_raw_fd());
    let md = src.metadata().map_err(|e| crate::provider::map_io(&e))?;
    // A size of 0 is not to be trusted: `/proc` and `/sys` files, and some
    // FUSE ones, say 0 and have content, and the kernel copies up to the
    // size it is told. Streaming reads them as they are.
    if md.len() == 0 {
        return Ok(None);
    }
    // A reflink shares the source's blocks: instant, and no space taken
    // until one side is written — holes included. Whole-file only, so only
    // from offset 0.
    if src_offset(src) == Some(0) {
        #[allow(unsafe_code)]
        // SAFETY: both descriptors are open and owned by `src`/`dst` for the
        // whole call; FICLONE takes the source fd as its argument by value
        // and touches no memory of ours.
        let cloned = unsafe { libc::ioctl(dst_fd, libc::FICLONE, src_fd) } == 0;
        if cloned {
            // The clone's size, not the source's re-read one, which may have
            // grown since; and the descriptor's offset moved past it, which
            // FICLONE leaves at 0.
            let len = dst
                .metadata()
                .map_err(|e| crate::provider::map_io(&e))?
                .len();
            let mut d = dst;
            std::io::Seek::seek(&mut d, std::io::SeekFrom::Start(len))
                .map_err(|e| crate::provider::map_io(&e))?;
            if !progress(len) {
                return Err(Error::Cancelled);
            }
            return Ok(Some(len));
        }
    }
    // Without a reflink, `copy_file_range` on ext4 or tmpfs writes a sparse
    // source's holes as zeros: a 50 GB disk image with 2 GB of data would
    // take 50 GB. Streaming skips them (#222), so a sparse file streams.
    if md.blocks().saturating_mul(512) < md.len() {
        return Ok(None);
    }
    let mut done: u64 = 0;
    loop {
        #[allow(unsafe_code)]
        // SAFETY: valid descriptors as above; null offsets make the kernel
        // use and advance each file's own offset, so no pointer of ours is
        // read or written.
        let n = unsafe {
            libc::copy_file_range(
                src_fd,
                std::ptr::null_mut(),
                dst_fd,
                std::ptr::null_mut(),
                BLOCK,
                0,
            )
        };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if done == 0 && refused(&e) {
                return Ok(None);
            }
            return Err(crate::provider::map_io(&e));
        }
        if n == 0 {
            // Nothing at all from a file that said it had bytes: the kernel
            // did not copy it here, and the caller streams. A refusal AFTER
            // some bytes is an error instead: the staging holds them.
            return Ok((done > 0).then_some(done));
        }
        done += u64::try_from(n).map_err(|_| Error::Io { retryable: false })?;
        if !progress(done) {
            return Err(Error::Cancelled);
        }
    }
}

/// Elsewhere the kernel copy is not attempted.
#[cfg(not(target_os = "linux"))]
pub(crate) fn copy(
    _src: &File,
    _dst: &File,
    _progress: &(dyn Fn(u64) -> bool + Send + Sync),
) -> Result<Option<u64>, Error> {
    Ok(None)
}

#[cfg(target_os = "linux")]
fn src_offset(f: &File) -> Option<u64> {
    use std::io::Seek;
    let mut f = f;
    f.stream_position().ok()
}

/// The errors that mean "not here" rather than "this copy failed": another
/// filesystem, one that does not support it, an old kernel or a seccomp
/// filter, or a staging opened `O_APPEND` to resume.
#[cfg(target_os = "linux")]
fn refused(e: &std::io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(
            libc::EXDEV
                | libc::EOPNOTSUPP
                | libc::ENOSYS
                | libc::EINVAL
                | libc::EBADF
                | libc::EPERM
        )
    )
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::copy;

    /// The bytes arrive whole and in order, and progress ends at the size —
    /// over two blocks' worth would take 128 MiB, so one block's tail and
    /// an empty file stand for the loop's two exits.
    #[test]
    fn copies_every_byte_and_reports_the_size() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.path().join("src"), &data).expect("write");
        let src = std::fs::File::open(dir.path().join("src")).expect("open");
        let dst = std::fs::File::create(dir.path().join("dst")).expect("create");
        let last = std::sync::atomic::AtomicU64::new(0);
        let got = copy(&src, &dst, &|done| {
            last.store(done, std::sync::atomic::Ordering::SeqCst);
            true
        })
        .expect("copy");
        // A tmpfs or an old kernel may refuse: then nothing was written.
        match got {
            Some(n) => {
                assert_eq!(n, data.len() as u64);
                assert_eq!(last.into_inner(), n);
                assert_eq!(std::fs::read(dir.path().join("dst")).expect("read"), data);
            }
            None => assert_eq!(
                std::fs::metadata(dir.path().join("dst")).expect("md").len(),
                0
            ),
        }

        std::fs::write(dir.path().join("empty"), b"").expect("write");
        let src = std::fs::File::open(dir.path().join("empty")).expect("open");
        let dst = std::fs::File::create(dir.path().join("dst2")).expect("create");
        assert!(matches!(copy(&src, &dst, &|_| true), Ok(Some(0) | None)));
    }

    /// Stopping from the progress callback is a cancellation, not a
    /// partial success.
    #[test]
    fn a_progress_that_says_stop_cancels() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("src"), vec![7u8; 4096]).expect("write");
        let src = std::fs::File::open(dir.path().join("src")).expect("open");
        let dst = std::fs::File::create(dir.path().join("dst")).expect("create");
        match copy(&src, &dst, &|_| false) {
            Err(norte_proto::Error::Cancelled) | Ok(None) => {}
            other => panic!("expected a cancellation, got {other:?}"),
        }
    }

    /// A file that says it is empty and is not — `/proc` — is left to
    /// streaming: the kernel would copy nothing and call it done.
    #[test]
    fn a_proc_file_is_left_to_streaming() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = std::fs::File::open("/proc/self/status").expect("proc");
        let dst = std::fs::File::create(dir.path().join("dst")).expect("create");
        assert!(matches!(copy(&src, &dst, &|_| true), Ok(None)));
    }

    /// A sparse file keeps its holes: either a reflink (which shares them)
    /// or streaming, never `copy_file_range` writing them as zeros.
    #[test]
    fn a_sparse_file_does_not_come_out_full() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let f = std::fs::File::create(dir.path().join("src")).expect("create");
        f.set_len(64 * 1024 * 1024).expect("hole");
        drop(f);
        let src = std::fs::File::open(dir.path().join("src")).expect("open");
        if src.metadata().expect("md").blocks() * 512 >= 64 * 1024 * 1024 {
            return; // this filesystem has no holes to keep
        }
        let dst = std::fs::File::create(dir.path().join("dst")).expect("create");
        if let Ok(Some(_)) = copy(&src, &dst, &|_| true) {
            let blocks = std::fs::metadata(dir.path().join("dst"))
                .expect("md")
                .blocks();
            assert!(blocks * 512 < 64 * 1024 * 1024, "the holes were filled");
        }
    }

    /// Through the real local sink, on a filesystem that takes it (the
    /// tempdir's), the kernel DOES fill the staging: without this the e2e
    /// tests would pass the same with the fast path never running.
    #[tokio::test]
    async fn the_local_sink_fills_in_the_kernel() {
        use norte_vfs::Provider;
        let dir = tempfile::tempdir().expect("tempdir");
        let data = vec![9u8; 200_000];
        std::fs::write(dir.path().join("src"), &data).expect("write");
        let p = crate::LocalProvider::rooted(dir.path());
        let to = crate::LocalProvider::root()
            .join(norte_proto::Segment::new(b"dst".to_vec()).expect("segment"));
        let mut sink = p.write(&to).await.expect("sink");
        let src = std::fs::File::open(dir.path().join("src")).expect("open");
        let filled = sink
            .fill_from(src, std::sync::Arc::new(|_| true))
            .await
            .expect("the kernel copies within one filesystem")
            .expect("copy");
        assert_eq!(filled, data.len() as u64);
        sink.commit().await.expect("commit");
        assert_eq!(std::fs::read(dir.path().join("dst")).expect("read"), data);
    }

    /// A staging opened `O_APPEND` (a resume) is refused before anything is
    /// written, so the caller can stream into it.
    #[test]
    fn an_append_staging_is_refused_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("src"), b"abc").expect("write");
        std::fs::write(dir.path().join("dst"), b"").expect("write");
        let src = std::fs::File::open(dir.path().join("src")).expect("open");
        let dst = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("dst"))
            .expect("open");
        assert!(matches!(copy(&src, &dst, &|_| true), Ok(None)));
        assert_eq!(std::fs::read(dir.path().join("dst")).expect("read"), b"");
    }
}
