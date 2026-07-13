//! Puente sync→async: `Read + Seek` sobre `Provider::read(range)` del
//! provider interior, para parsers de archivo que corren en `spawn_blocking`
//! (regla 2 / ADR 0002: el hilo blocking puede bloquear en `block_on`; el
//! runtime jamás).

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use futures::StreamExt;
use norte_proto::{ByteRange, VPath};
use norte_vfs::Provider;

/// Tamaño de bloque de lectura: los parsers hacen ráfagas locales (headers,
/// central directory) — un bloque amortiza los round-trips al interior.
const BLOCK: u64 = 256 * 1024;

/// Lector sync posicionado sobre un archivo del provider interior, con caché
/// del último bloque. SOLO para hilos de `spawn_blocking`.
///
/// Nota de runtime: `Handle::block_on` no conduce los drivers de IO/tiempo
/// de un runtime `current_thread` salvo que su hilo esté dentro de
/// `Runtime::block_on`. En el daemon (multi-thread) es irrelevante; los
/// providers interiores puros (Mem) tampoco los necesitan.
pub(crate) struct ProviderReader {
    handle: tokio::runtime::Handle,
    inner: Arc<dyn Provider>,
    path: VPath,
    len: u64,
    pos: u64,
    /// (offset del bloque, bytes) — el último bloque leído.
    block: Option<(u64, Vec<u8>)>,
}

impl ProviderReader {
    /// `len` viene del `stat` del contenedor que el caller ya hizo (y que
    /// gobierna la invalidación del índice: misma generación, misma vista).
    pub(crate) fn new(
        handle: tokio::runtime::Handle,
        inner: Arc<dyn Provider>,
        path: VPath,
        len: u64,
    ) -> Self {
        Self {
            handle,
            inner,
            path,
            len,
            pos: 0,
            block: None,
        }
    }

    fn fetch_block(&mut self, block_off: u64) -> std::io::Result<()> {
        let want = BLOCK.min(self.len.saturating_sub(block_off));
        let range = ByteRange {
            offset: block_off,
            len: Some(want),
        };
        let inner = Arc::clone(&self.inner);
        let path = self.path.clone();
        let bytes: Result<Vec<u8>, norte_proto::Error> = self.handle.block_on(async move {
            let mut stream = inner.read(&path, Some(range)).await?;
            let mut out = Vec::with_capacity(usize::try_from(want).unwrap_or(0));
            while let Some(chunk) = stream.next().await {
                out.extend_from_slice(&chunk?);
            }
            Ok(out)
        });
        let bytes = bytes.map_err(std::io::Error::other)?;
        self.block = Some((block_off, bytes));
        Ok(())
    }
}

impl Read for ProviderReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let block_off = self.pos - (self.pos % BLOCK);
        let hit = self
            .block
            .as_ref()
            .is_some_and(|(off, _)| *off == block_off);
        if !hit {
            self.fetch_block(block_off)?;
        }
        let (off, bytes) = self.block.as_ref().expect("bloque recién cargado");
        let start = usize::try_from(self.pos - off).map_err(std::io::Error::other)?;
        if start >= bytes.len() {
            // El interior devolvió menos de lo esperado (contenedor mutado
            // bajo nuestros pies): EOF limpio; la invalidación por
            // generación hará el resto en la próxima operación.
            return Ok(0);
        }
        let n = buf.len().min(bytes.len() - start);
        buf[..n].copy_from_slice(&bytes[start..start + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for ProviderReader {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let target: i128 = match from {
            SeekFrom::Start(o) => i128::from(o),
            SeekFrom::End(d) => i128::from(self.len) + i128::from(d),
            SeekFrom::Current(d) => i128::from(self.pos) + i128::from(d),
        };
        let target =
            u64::try_from(target).map_err(|_| std::io::Error::other("seek antes del byte 0"))?;
        self.pos = target;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use norte_proto::Segment;
    use norte_testkit::MemProvider;

    async fn seed(content: &[u8]) -> (Arc<dyn Provider>, VPath) {
        let mem = MemProvider::new();
        let path = MemProvider::root().join(Segment::new(b"f.bin".to_vec()).expect("seg"));
        let mut sink = mem.write(&path).await.expect("write");
        sink.write(Bytes::copy_from_slice(content))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
        (Arc::new(mem), path)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lee_con_seek_y_cruza_bloques() {
        // > BLOCK para forzar dos bloques.
        let content: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let (mem, path) = seed(&content).await;
        let handle = tokio::runtime::Handle::current();
        let len = content.len() as u64;
        let content2 = content.clone();
        tokio::task::spawn_blocking(move || {
            let mut r = ProviderReader::new(handle, mem, path, len);
            // Lectura que CRUZA la frontera de bloque (256 KiB).
            r.seek(SeekFrom::Start(262_100)).expect("seek");
            let mut buf = [0u8; 100];
            r.read_exact(&mut buf).expect("read_exact");
            assert_eq!(&buf[..], &content2[262_100..262_200]);
            // SeekFrom::End y lectura de cola.
            r.seek(SeekFrom::End(-5)).expect("seek end");
            let mut cola = Vec::new();
            r.read_to_end(&mut cola).expect("cola");
            assert_eq!(cola, &content2[content2.len() - 5..]);
            // Past-EOF: Ok(0).
            r.seek(SeekFrom::Start(len + 10)).expect("seek past");
            let mut b = [0u8; 4];
            assert_eq!(r.read(&mut b).expect("read past-EOF"), 0);
        })
        .await
        .expect("hilo blocking");
    }
}
