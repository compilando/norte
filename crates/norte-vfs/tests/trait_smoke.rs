//! Smoke tests del contrato: objeto-seguridad, defaults del trait y uso de
//! los tipos de stream/sink con un provider nulo. La suite contractual real
//! (`provider_contract!`) llega en la fase 7 sobre `MemProvider`.

// La firma del trait es `-> &str` (un provider puede derivar su scheme de
// estado propio); devolver un literal aquí es correcto.
#![allow(clippy::unnecessary_literal_bound)]

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{Capabilities, CapabilityFlags, Entry, Error, VPath};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};

struct NullProvider;

#[async_trait]
impl Provider for NullProvider {
    fn scheme(&self) -> &str {
        "null"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            flags: CapabilityFlags::empty(),
            max_path: None,
        }
    }
    async fn stat(&self, _p: &VPath) -> Result<Entry, Error> {
        Err(Error::NotFound)
    }
    async fn list(&self, _p: &VPath) -> Result<EntryStream, Error> {
        // Directorio "vacío": stream sin elementos.
        Ok(futures::stream::empty().boxed())
    }
    async fn read(&self, _p: &VPath) -> Result<ByteStream, Error> {
        Ok(futures::stream::iter([Ok(Bytes::from_static(b"null"))]).boxed())
    }
    async fn write(&self, _p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        Ok(Box::new(NullSink { aborted: false }))
    }
    async fn mkdir(&self, _p: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    async fn remove(&self, _p: &VPath) -> Result<(), Error> {
        Err(Error::NotFound)
    }
    async fn rename(&self, _from: &VPath, _to: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

struct NullSink {
    aborted: bool,
}

#[async_trait]
impl ByteSink for NullSink {
    async fn write(&mut self, _chunk: Bytes) -> Result<(), Error> {
        Ok(())
    }
    async fn commit(self: Box<Self>) -> Result<(), Error> {
        Ok(())
    }
    async fn abort(mut self: Box<Self>) -> Result<(), Error> {
        self.aborted = true;
        Ok(())
    }
}

fn vpath(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

#[test]
fn provider_is_object_safe() {
    let boxed: Box<dyn Provider> = Box::new(NullProvider);
    assert_eq!(boxed.scheme(), "null");
    assert_eq!(boxed.capabilities().flags, CapabilityFlags::empty());
}

#[tokio::test]
async fn copy_native_defaults_to_none() {
    let p = NullProvider;
    let res = p
        .copy_native(&vpath("null:///a"), &vpath("null:///b"))
        .await;
    assert!(res.is_none(), "sin SERVER_COPY el default es None");
}

#[tokio::test]
async fn streams_and_sink_are_usable_through_the_trait() {
    let p: Box<dyn Provider> = Box::new(NullProvider);

    let mut entries = p.list(&vpath("null:///")).await.expect("list");
    assert!(entries.next().await.is_none(), "listado vacío");

    let mut bytes = p.read(&vpath("null:///f")).await.expect("read");
    let chunk = bytes.next().await.expect("un chunk").expect("sin error");
    assert_eq!(&chunk[..], b"null");

    let mut sink = p.write(&vpath("null:///g")).await.expect("write");
    sink.write(Bytes::from_static(b"data"))
        .await
        .expect("write chunk");
    sink.commit().await.expect("commit");

    let sink2 = p.write(&vpath("null:///h")).await.expect("write");
    sink2.abort().await.expect("abort idempotente");
}

#[tokio::test]
async fn errors_map_to_proto_taxonomy() {
    let p = NullProvider;
    assert_eq!(
        p.stat(&vpath("null:///x")).await.unwrap_err(),
        Error::NotFound
    );
    assert_eq!(
        p.mkdir(&vpath("null:///d")).await.unwrap_err(),
        Error::Unsupported
    );
}
