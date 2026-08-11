//! El rung caro: el sha256 de UN fichero, leído en streaming a través de
//! [`Provider::read`].
//!
//! Es privado a propósito. La cascada no sabe hashear —le llega el resultado ya
//! hecho, en [`HashOutcome`](crate::cascade::HashOutcome)— y quien use el motor
//! desde fuera pide el rung con
//! [`CompareOptions::with_hash`](crate::CompareOptions::with_hash), no
//! llamando aquí. Publicar esto sería ofrecer una segunda forma de hashear un
//! fichero, y la de verdad —la del copy engine y la del índice— no vive en este
//! crate.
//!
//! # Qué se lee, y qué no
//!
//! El fichero ENTERO, en los trozos que dé el provider, y nunca se materializa
//! más de un trozo: un fichero de 40 GB cuesta 40 GB de red o de disco, no de
//! memoria. Un rango no serviría —el hash es de todo el contenido— y un
//! `read` con `range: None` es lo que todos los providers implementan.
//!
//! # Cancelación
//!
//! El token se mira **una vez por trozo**, no una vez por fichero (regla dura
//! 3): si se mirase por fichero, cancelar una comparación parada en un fichero
//! enorme esperaría a que terminase de leerlo entero. Ese es justo el caso en
//! el que un usuario cancela.

use futures::StreamExt;
use norte_proto::VPath;
use norte_vfs::{ByteStream, Provider};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

/// Un sha256 crudo. No se enseña a nadie: solo se compara con otro.
pub(crate) type Digest256 = [u8; 32];

/// Por qué no hay digest.
///
/// Las dos razones se tratan MUY distinto arriba: una lectura rota es una fila
/// de error y el walk sigue; una cancelación termina el flujo entero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HashFailure {
    /// El provider no pudo abrir o no pudo terminar de leer el fichero.
    Read,
    /// El token se disparó a mitad de la lectura. No hay nada que limpiar: no
    /// se ha escrito un solo byte.
    Cancelled,
}

/// El sha256 de `path`, leído en streaming.
pub(crate) async fn sha256_of(
    provider: &dyn Provider,
    path: &VPath,
    cancel: &CancellationToken,
) -> Result<Digest256, HashFailure> {
    // Antes de ABRIR: con el token ya disparado no se le pide al provider una
    // lectura que nadie va a usar (y un fichero vacío no tiene trozo en el que
    // mirarlo después).
    if cancel.is_cancelled() {
        return Err(HashFailure::Cancelled);
    }
    let stream = provider
        .read(path, None)
        .await
        .map_err(|_| HashFailure::Read)?;
    fold_digest(stream, cancel).await
}

/// El bucle que consume el flujo de bytes: separado de [`sha256_of`] para que
/// el chequeo del token se pueda probar sin un provider que colabore.
async fn fold_digest(
    mut stream: ByteStream,
    cancel: &CancellationToken,
) -> Result<Digest256, HashFailure> {
    let mut hasher = Sha256::new();
    while let Some(chunk) = stream.next().await {
        // Por TROZO. Ver la nota de cancelación de la cabecera del módulo.
        if cancel.is_cancelled() {
            return Err(HashFailure::Cancelled);
        }
        hasher.update(&chunk.map_err(|_| HashFailure::Read)?);
    }
    Ok(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use norte_proto::{Error, Segment};
    use norte_testkit::MemProvider;

    use super::*;

    fn path(name: &str) -> VPath {
        MemProvider::root().join(Segment::new(name.as_bytes().to_vec()).expect("segmento"))
    }

    async fn seed(mem: &MemProvider, name: &str, content: &[u8]) {
        let mut sink = mem.write(&path(name)).await.expect("write");
        sink.write(Bytes::copy_from_slice(content))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    fn chunks(items: Vec<Result<Bytes, Error>>) -> ByteStream {
        futures::stream::iter(items).boxed()
    }

    /// Mismo contenido, mismo digest; un byte distinto, digest distinto. Y el
    /// troceado no cuenta: el hash es del contenido, no de cómo llegó.
    #[tokio::test]
    async fn el_digest_es_del_contenido_y_no_del_troceado() {
        let mem = MemProvider::new();
        seed(&mem, "a", b"hola").await;
        seed(&mem, "b", b"hola").await;
        seed(&mem, "c", b"holA").await;
        let cancel = CancellationToken::new();
        let a = sha256_of(&mem, &path("a"), &cancel).await.expect("a");
        let b = sha256_of(&mem, &path("b"), &cancel).await.expect("b");
        let c = sha256_of(&mem, &path("c"), &cancel).await.expect("c");
        assert_eq!(a, b);
        assert_ne!(a, c);

        // El mismo contenido partido en dos trozos da el mismo digest.
        let partido = fold_digest(
            chunks(vec![
                Ok(Bytes::from_static(b"ho")),
                Ok(Bytes::from_static(b"la")),
            ]),
            &cancel,
        )
        .await
        .expect("digest");
        assert_eq!(partido, a);
    }

    /// Un fichero que no se puede abrir es [`HashFailure::Read`], no un panic
    /// ni un digest de cero bytes.
    #[tokio::test]
    async fn un_fichero_que_no_existe_es_un_fallo_de_lectura() {
        let mem = MemProvider::new();
        let outcome = sha256_of(&mem, &path("no-existe"), &CancellationToken::new()).await;
        assert_eq!(outcome, Err(HashFailure::Read));
    }

    /// Un error a MITAD del flujo también: medio fichero hasheado no es un
    /// digest, es una mentira del tamaño de un fichero.
    #[tokio::test]
    async fn un_flujo_que_se_rompe_a_mitad_es_un_fallo_de_lectura() {
        let outcome = fold_digest(
            chunks(vec![
                Ok(Bytes::from_static(b"ho")),
                Err(Error::Io { retryable: false }),
            ]),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(outcome, Err(HashFailure::Read));
    }

    /// El token se mira POR TROZO, y este test es lo único que lo demuestra.
    ///
    /// El flujo dispara el token al entregar su primer trozo: un bucle que
    /// solo mirase el token al empezar el fichero devolvería tan campante el
    /// digest de los dos trozos. Con 40 GB en vez de ocho bytes, esa
    /// diferencia es media hora de espera después de pulsar cancelar.
    #[tokio::test]
    async fn el_token_se_mira_una_vez_por_trozo() {
        let cancel = CancellationToken::new();
        let disparador = cancel.clone();
        let stream = futures::stream::iter(vec![
            Ok(Bytes::from_static(b"aaaa")),
            Ok(Bytes::from_static(b"bbbb")),
        ])
        .inspect(move |_| disparador.cancel())
        .boxed();
        assert_eq!(
            fold_digest(stream, &cancel).await,
            Err(HashFailure::Cancelled)
        );
    }

    /// Con el token ya disparado no se abre siquiera el fichero.
    #[tokio::test]
    async fn un_token_ya_disparado_no_abre_el_fichero() {
        let mem = MemProvider::new();
        seed(&mem, "a", b"hola").await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            sha256_of(&mem, &path("a"), &cancel).await,
            Err(HashFailure::Cancelled)
        );
        assert_eq!(mem.faults().read_calls(), 0, "no se abrió nada");
    }
}
