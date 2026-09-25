//! The remote listing, page by page.
//!
//! `fs.list` answers with a page and a cursor; the stream the caller sees
//! ([`super::RemoteBackend::list_stream`]) keeps asking for the next one as
//! it is consumed, so a directory of half a million entries does not travel
//! in a single frame nor wait to be whole before painting the first row.

use norte_proto::methods::{FsListParams, FsListResult};
use norte_proto::{Entry, Error, VPath, methods};

use super::RemoteBackend;

/// Entries per page when listing a remote dir (ADR 0017): bounds the
/// response frame and the time of ONE call.
pub(super) const LIST_PAGE: u32 = 1000;

/// State of the `try_unfold` that paginates a remote listing: the current
/// page's buffer and the next cursor.
pub(super) struct PageState {
    pub(super) backend: RemoteBackend,
    pub(super) dir: VPath,
    pub(super) buffer: std::collections::VecDeque<Entry>,
    pub(super) cursor: Option<String>,
    pub(super) done: bool,
    /// Attr ids from the START (#108 block 2): the daemon ignores them on a
    /// continuation (the retained stream was born with them), but they are
    /// resent regardless — if the cursor expires and the client restarts,
    /// the new listing asks for the same ones.
    pub(super) attrs: Vec<String>,
}

/// One step of the paginated stream: serves from the buffer or asks for the
/// next page.
pub(super) async fn page_step(mut st: PageState) -> Result<Option<(Entry, PageState)>, Error> {
    loop {
        if let Some(e) = st.buffer.pop_front() {
            return Ok(Some((e, st)));
        }
        if st.done {
            return Ok(None);
        }
        let cursor = st.cursor.take();
        let page: FsListResult = st
            .backend
            .call_timed_guarded(
                methods::FS_LIST,
                &FsListParams {
                    path: st.dir.clone(),
                    limit: Some(LIST_PAGE),
                    cursor,
                    attrs: st.attrs.clone(),
                },
            )
            .await?;
        // A broken server returning an empty page WITH next_cursor would
        // cause an infinite loop: it is cut off (precedent of fs.read's
        // guard).
        if page.entries.is_empty() && page.next_cursor.is_some() {
            return Err(Error::Internal { panic: false });
        }
        st.buffer.extend(page.entries);
        match page.next_cursor {
            Some(c) => st.cursor = Some(c),
            None => st.done = true,
        }
    }
}
