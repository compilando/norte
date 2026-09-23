//! Un origen que entrega su contenido A PETICIÓN, para abrir a mano la
//! ventana que un test necesita (#367, #368).
//!
//! Copiar o sincronizar un ÁRBOL da la ventana gratis: con cuatro mil entradas
//! siempre queda trabajo detrás del momento en que el test interviene. Con un
//! fichero, o con un plan de un paso, no queda nada — y hacer el fichero
//! enorme es cambiar una carrera por otra, además de cara.
//!
//! Así que la ventana se abre a mano. Este provider avisa de que ya le están
//! leyendo y se PARA hasta que el test le dice que siga. No hay plazos ni
//! tamaños: hay un hecho («ya empezó») y una orden («sigue»), que es lo que
//! este repositorio pide de una espera.
//!
//! Envuelve a `MemProvider` porque lo que estos tests prueban está en el lado
//! del DESTINO, que sí tiene que ser el provider local de verdad: el fallo
//! existe porque un descriptor sobrevive a un `rename`, y eso no se simula.
//!
//! Vive en un módulo compartido porque lo usan dos ficheros de test, y
//! duplicarlo sería tener dos cosas que hay que cambiar a la vez.

use std::sync::Arc;

use norte_proto::{Error, VPath};
use norte_vfs::Provider;

/// El esquema por el que se registra. No es `file` a propósito: el destino de
/// estos tests sí es local, y hacen falta los dos a la vez.
pub const ESQUEMA: &str = "lento";

pub struct OrigenAPeticion {
    inner: Arc<norte_testkit::MemProvider>,
    empezo: tokio::sync::mpsc::UnboundedSender<()>,
    sigue: Arc<tokio::sync::Semaphore>,
}

/// Lo que el test necesita para manejarlo: por dónde se entera de que empezó,
/// y por dónde le da permiso para seguir.
pub struct Mando {
    pub empezo: tokio::sync::mpsc::UnboundedReceiver<()>,
    pub sigue: Arc<tokio::sync::Semaphore>,
}

impl Mando {
    /// Espera a que la lectura haya empezado DE VERDAD. `false` = no llegó, y
    /// entonces el test no ha probado nada y tiene que decirlo.
    pub async fn empezo(&mut self) -> bool {
        tokio::time::timeout(std::time::Duration::from_secs(30), self.empezo.recv())
            .await
            .is_ok_and(|v| v.is_some())
    }

    /// Suelta la lectura. Generoso a propósito: lo que se quiere es que no
    /// vuelva a pararse, no contar permisos.
    pub fn sigue(&self) {
        self.sigue.add_permits(1024);
    }
}

impl OrigenAPeticion {
    /// Envuelve `inner` y devuelve el provider y su mando.
    pub fn nuevo(inner: Arc<norte_testkit::MemProvider>) -> (Arc<dyn Provider>, Mando) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let sigue = Arc::new(tokio::sync::Semaphore::new(0));
        let provider = Arc::new(Self {
            inner,
            empezo: tx,
            sigue: Arc::clone(&sigue),
        }) as Arc<dyn Provider>;
        (
            provider,
            Mando {
                empezo: rx,
                sigue: Arc::clone(&sigue),
            },
        )
    }
}

#[async_trait::async_trait]
impl Provider for OrigenAPeticion {
    fn scheme(&self) -> &str {
        ESQUEMA
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
        let interno = self.inner.read(p, range).await?;
        let empezo = self.empezo.clone();
        let sigue = Arc::clone(&self.sigue);
        // `then` espera al future ANTES de entregar el elemento, así que la
        // parada ocurre antes del PRIMER trozo, no después: cuando el test se
        // entera, la lectura está abierta y aparcada, todavía sin publicar
        // nada. Sirve igual —lo que hace falta es que no haya terminado— pero
        // no es lo que parece, y de ahí este comentario: alguien que lo creyera
        // al revés podría «simplificar» el montaje sobre un modelo equivocado.
        let mut primero = true;
        Ok(Box::pin(futures::StreamExt::then(interno, move |chunk| {
            let (empezo, sigue) = (empezo.clone(), Arc::clone(&sigue));
            let era_el_primero = std::mem::replace(&mut primero, false);
            async move {
                if era_el_primero {
                    let _ = empezo.send(());
                    // Se suelta en cuanto el test da el permiso. Sin plazo: si
                    // nunca llega, el `timeout` del desenlace lo convierte en
                    // un test rojo y no en uno colgado.
                    let _ = sigue.acquire().await;
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
