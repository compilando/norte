//! La sesión de UI que el daemon guarda (L2).
//!
//! Se llama `ui_session` y no `session` porque el módulo `sessions` ya existe y
//! es otra cosa: las sesiones de AGENTE. Esta es la pantalla de un humano.
//!
//! El core la almacena, la versiona y la devuelve; **no la lee**. Es la misma
//! decisión de ADR 0058 —«una pantalla es un árbol que el core guarda y no
//! interpreta»— llevada al proceso de al lado: los tipos del cuerpo viven en
//! `norte-frontend`, que depende de `norte-proto` y no al revés.
//!
//! Lo único que este módulo hace valer son dos cosas, y las dos tratan de
//! protegerse a sí mismo: la `revision` (un escritor rancio no pisa al
//! vigente) y el tope de 1 MiB (un cliente con un bug no llena el disco).

use std::sync::Mutex;

use norte_proto::methods::{SESSION_BODY_MAX, Session};

/// Por qué se rehusó un `put`.
#[derive(Debug, thiserror::Error)]
pub enum PutError {
    /// La revisión que traía el cliente no es la vigente.
    #[error("revisión rancia; la vigente es {current}")]
    Conflict {
        /// La revisión vigente, para que el cliente re-lea contra ella.
        current: u64,
    },
    /// El cuerpo pasa de [`SESSION_BODY_MAX`].
    #[error("el cuerpo ocupa {bytes} bytes y el tope es {SESSION_BODY_MAX}")]
    TooLarge {
        /// Bytes serializados que traía.
        bytes: usize,
    },
    /// Quien escribe no es la conexión dueña. Lo construye el HANDLER, que es
    /// la capa que sabe qué conexión habla; vive aquí para que las dos capas
    /// nombren el rechazo igual.
    #[error("esta conexión no es la dueña de la sesión")]
    NotOwner,
}

/// La sesión viva del daemon: una por proceso, con su revisión y su dueña.
#[derive(Debug, Default)]
pub struct SessionStore {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    session: Session,
    /// Conexión dueña, si alguna la reclamó.
    owner: Option<u64>,
    /// Hay cambios sin volcar a disco.
    dirty: bool,
}

impl SessionStore {
    /// Un store que arranca con la sesión que venía de disco.
    #[must_use]
    pub fn new(session: Session) -> Self {
        Self {
            inner: Mutex::new(Inner {
                session,
                owner: None,
                dirty: false,
            }),
        }
    }

    /// La sesión vigente. Clonar es barato comparado con tener el lock
    /// tomado mientras se serializa a un socket.
    #[must_use]
    pub fn get(&self) -> Session {
        self.lock().session.clone()
    }

    /// Reemplaza la sesión entera. Devuelve la revisión NUEVA.
    ///
    /// # Errors
    ///
    /// [`PutError::TooLarge`] si el cuerpo pasa de [`SESSION_BODY_MAX`], y
    /// [`PutError::Conflict`] si la revisión que trae el cliente no es la
    /// vigente. En ambos casos lo almacenado se queda EXACTAMENTE como estaba:
    /// truncar un documento cuyo esquema no se conoce es peor que rechazarlo.
    pub fn put(
        &self,
        version: u32,
        revision: u64,
        body: serde_json::Value,
    ) -> Result<u64, PutError> {
        // El tope se mide en BYTES SERIALIZADOS, que es lo que ocupa en el
        // wire y en disco. Un cuerpo que ni siquiera serializa no cabe en
        // ningún sitio, así que cuenta como el peor caso posible.
        let bytes = serde_json::to_vec(&body).map_or(usize::MAX, |v| v.len());
        if bytes > SESSION_BODY_MAX {
            return Err(PutError::TooLarge { bytes });
        }
        let mut g = self.lock();
        if g.session.revision != revision {
            return Err(PutError::Conflict {
                current: g.session.revision,
            });
        }
        g.session.version = version;
        g.session.body = body;
        g.session.revision += 1;
        g.dirty = true;
        Ok(g.session.revision)
    }

    /// La primera conexión que la reclama se la queda; las demás reciben
    /// `false` y corren sueltas. Reclamar dos veces desde la misma conexión no
    /// es un error.
    pub fn claim(&self, conn: u64) -> bool {
        let mut g = self.lock();
        match g.owner {
            None => {
                g.owner = Some(conn);
                true
            }
            Some(actual) => actual == conn,
        }
    }

    /// Suelta la propiedad, si es de esta conexión. Soltar lo ajeno no hace
    /// nada: una conexión no desaloja a otra por desconectarse.
    pub fn release(&self, conn: u64) {
        let mut g = self.lock();
        if g.owner == Some(conn) {
            g.owner = None;
        }
    }

    /// Qué conexión manda, si alguna.
    #[must_use]
    pub fn owner(&self) -> Option<u64> {
        self.lock().owner
    }

    /// Hay cambios sin volcar.
    #[must_use]
    pub fn dirty(&self) -> bool {
        self.lock().dirty
    }

    /// Lo que consume el escritor a disco: devuelve la sesión UNA vez por
    /// cambio y limpia la marca. Sin cambios, `None` — y el escritor no toca
    /// el disco, que es lo que hace barato despertarse cada segundo.
    #[must_use]
    pub fn take_dirty(&self) -> Option<Session> {
        let mut g = self.lock();
        if !g.dirty {
            return None;
        }
        g.dirty = false;
        Some(g.session.clone())
    }

    /// El lock, recuperado de un envenenamiento: un panic en otro hilo
    /// mientras se clonaba una sesión no es razón para tirar el daemon, y el
    /// invariante que protege es «un campo consistente con otro», no memoria.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(n: usize) -> serde_json::Value {
        serde_json::json!({ "relleno": "x".repeat(n) })
    }

    /// Una sesión nunca escrita es revisión 0 sin esquema: el cliente que
    /// arranca contra un daemon limpio no distingue «no hay» de «falló».
    #[test]
    fn una_sesion_nueva_es_revision_cero() {
        let s = SessionStore::default();
        let g = s.get();
        assert_eq!(g.revision, 0);
        assert_eq!(g.version, 0, "sin esquema hasta que alguien escriba uno");
    }

    /// Cada `put` aceptado sube la revisión, y la que devuelve es la que el
    /// cliente tiene que traer la próxima vez.
    #[test]
    fn cada_put_sube_la_revision() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        assert_eq!(s.put(1, 0, body(1)).expect("primer put"), 1);
        assert_eq!(s.put(1, 1, body(1)).expect("segundo put"), 2);
        assert_eq!(s.get().revision, 2);
    }

    /// Una revisión rancia es `Conflict` CON la vigente: el cliente re-lee sin
    /// tener que preguntar otra vez para saber contra qué.
    #[test]
    fn una_revision_rancia_es_conflicto_y_no_escribe() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        s.put(1, 0, body(1)).expect("primer put");
        let e = s.put(1, 0, body(2)).expect_err("rancia");
        assert!(matches!(e, PutError::Conflict { current: 1 }), "{e:?}");
        assert_eq!(s.get().body, body(1), "lo almacenado no se toca");
    }

    /// Por encima del tope: `TooLarge`, y lo almacenado SIGUE EN PIE.
    #[test]
    fn por_encima_del_tope_no_se_trunca_se_rechaza() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        s.put(1, 0, body(10)).expect("cabe");
        let e = s
            .put(1, 1, body(SESSION_BODY_MAX + 1))
            .expect_err("no cabe");
        assert!(matches!(e, PutError::TooLarge { .. }), "{e:?}");
        assert_eq!(s.get().revision, 1, "la sesión almacenada se queda");
    }

    /// El tope se mide sobre los BYTES serializados, no sobre el número de
    /// claves ni la profundidad.
    #[test]
    fn el_tope_se_mide_en_bytes_serializados() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        let justo = serde_json::json!({ "x": "y".repeat(SESSION_BODY_MAX - 12) });
        let bytes = serde_json::to_vec(&justo).expect("serializa").len();
        assert!(bytes <= SESSION_BODY_MAX, "{bytes}");
        s.put(1, 0, justo).expect("justo por debajo entra");
    }

    /// La dueña es la PRIMERA que la reclama; soltarla la libera para la
    /// siguiente. Nunca dos escritores sobre un estado.
    #[test]
    fn solo_la_duena_escribe() {
        let s = SessionStore::default();
        assert!(s.claim(1), "la primera se la queda");
        assert!(!s.claim(2), "la segunda corre suelta");
        assert!(
            s.claim(1),
            "reclamar dos veces desde la misma no es un error"
        );
        assert_eq!(s.owner(), Some(1));
        s.put(1, 0, body(1)).expect("la dueña escribe");
        s.release(1);
        assert_eq!(s.owner(), None);
        assert!(s.claim(2), "al irse la dueña, la siguiente puede tomarla");
    }

    /// Soltar una propiedad que no se tiene no se la quita a nadie.
    #[test]
    fn soltar_lo_ajeno_no_hace_nada() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        s.release(2);
        assert_eq!(s.owner(), Some(1), "la 2 no puede desalojar a la 1");
    }

    /// `take_dirty` devuelve la sesión UNA vez por cambio, y nada si no ha
    /// cambiado nada desde la última.
    #[test]
    fn lo_sucio_se_consume_una_sola_vez() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        assert!(s.take_dirty().is_none(), "nada que escribir al arrancar");
        s.put(1, 0, body(1)).expect("put");
        assert!(s.dirty());
        assert!(s.take_dirty().is_some());
        assert!(!s.dirty(), "consumida");
        assert!(s.take_dirty().is_none());
    }

    /// La sesión que viene de disco arranca limpia: cargarla no es un cambio
    /// que haya que volver a escribir.
    #[test]
    fn la_sesion_cargada_de_disco_no_nace_sucia() {
        let s = SessionStore::new(Session {
            version: 1,
            revision: 9,
            body: body(1),
        });
        assert_eq!(s.get().revision, 9);
        assert!(!s.dirty());
    }
}
