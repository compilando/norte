//! A source that delivers its content ON REQUEST, to open by hand the window
//! a test needs (#367, #368).
//!
//! Copying or syncing a WHOLE TREE gives the window for free: with four
//! thousand entries there is always work left behind the moment the test
//! steps in. With one file, or a one-step plan, nothing is left — and making
//! the file huge trades one race for another, and it is expensive besides.
//!
//! So the window is opened by hand. This provider signals that it is
//! already being read and STOPS until the test tells it to continue. No
//! deadlines and no sizes: there is a fact ("it already started") and an
//! order ("continue"), which is what this repository asks of a wait.
//!
//! Wraps `MemProvider` because what these tests check is on the DESTINATION
//! side, which does have to be the real local provider: the failure exists
//! because a descriptor survives a `rename`, and that cannot be simulated.
//!
//! Lives in a shared module because two test files use it, and duplicating
//! it would mean having two things to change at once.

use std::sync::Arc;

use norte_proto::{Error, VPath};
use norte_vfs::Provider;

/// The scheme it registers under. Deliberately not `file`: these tests'
/// destination IS local, and both are needed at the same time.
pub const SCHEME: &str = "slow";

pub struct OnRequestSource {
    inner: Arc<norte_testkit::MemProvider>,
    started: tokio::sync::mpsc::UnboundedSender<()>,
    follows: Arc<tokio::sync::Semaphore>,
}

/// What the test needs to handle it: how it learns that it started, and how
/// it gives it permission to continue.
pub struct Command {
    pub started: tokio::sync::mpsc::UnboundedReceiver<()>,
    pub follows: Arc<tokio::sync::Semaphore>,
}

impl Command {
    /// Waits until the read has REALLY started. `false` = it never arrived,
    /// and then the test proved nothing and has to say so.
    pub async fn started(&mut self) -> bool {
        tokio::time::timeout(std::time::Duration::from_secs(30), self.started.recv())
            .await
            .is_ok_and(|v| v.is_some())
    }

    /// Releases the read. Generous on purpose: the point is that it never
    /// stops again, not counting permits.
    pub fn follows(&self) {
        self.follows.add_permits(1024);
    }
}

impl OnRequestSource {
    /// Wraps `inner` and returns the provider and its remote.
    pub fn wrap(inner: Arc<norte_testkit::MemProvider>) -> (Arc<dyn Provider>, Command) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let follows = Arc::new(tokio::sync::Semaphore::new(0));
        let provider = Arc::new(Self {
            inner,
            started: tx,
            follows: Arc::clone(&follows),
        }) as Arc<dyn Provider>;
        (
            provider,
            Command {
                started: rx,
                follows: Arc::clone(&follows),
            },
        )
    }
}

#[async_trait::async_trait]
impl Provider for OnRequestSource {
    fn scheme(&self) -> &str {
        SCHEME
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, Error> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
        self.inner.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        let inner = self.inner.read(p, range).await?;
        let started = self.started.clone();
        let follows = Arc::clone(&self.follows);
        // `then` awaits the future BEFORE delivering the item, so the stop
        // happens before the FIRST chunk, not after: by the time the test
        // finds out, the read is open and parked, still having published
        // nothing. It works either way — what is needed is that it has not
        // finished — but it is not what it looks like, hence this comment:
        // someone who believed the opposite could "simplify" the setup on a
        // wrong model.
        let mut first = true;
        Ok(Box::pin(futures::StreamExt::then(inner, move |chunk| {
            let (started, follows) = (started.clone(), Arc::clone(&follows));
            let was_the_first = std::mem::replace(&mut first, false);
            async move {
                if was_the_first {
                    let _ = started.send(());
                    // Released as soon as the test gives permission. No
                    // deadline: if it never arrives, the outcome's `timeout`
                    // turns it into a red test, not a hung one.
                    let _ = follows.acquire().await;
                }
                chunk
            }
        })))
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        self.inner.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        self.inner.mkdir(p).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        self.inner.remove(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.inner.rename(from, to).await
    }
}
