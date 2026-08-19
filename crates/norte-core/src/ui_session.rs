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

pub mod disk;

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
    /// La sesión está CERRADA: este proceso ya volcó por última vez.
    #[error("la sesión ya está cerrada; no queda quien la escriba")]
    Sealed,
    /// El cuerpo dice ser de un esquema que este core no sabe LEER (#247).
    ///
    /// Aceptarlo era el peor de los desenlaces: el core volcaba a disco un
    /// documento que su propio guard de carga rechaza, así que a partir del
    /// siguiente arranque `session.get` contestaba «del futuro», la sesión
    /// dejaba de tener dueña y la persistencia moría en silencio hasta que
    /// alguien borrara el fichero a mano. Un `ntc` más nuevo contra un `norte`
    /// más viejo —los dos `SCHEMA_VERSION` viven en crates distintos— es todo
    /// lo que hacía falta.
    #[error("el cuerpo es de la versión {version} y este core sabe {known}")]
    UnknownSchema {
        /// La que traía el cliente.
        version: u32,
        /// La más nueva que este core sabe leer.
        known: u32,
    },
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
    /// Ya no se admite nada más: el proceso se está apagando.
    sealed: bool,
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
                sealed: false,
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
        // Bajo el MISMO lock que la mutación, y no contra un token atómico
        // aparte: comprobar fuera y mutar dentro deja una ventana —un hilo
        // desalojado justo en medio— por la que un `put` entra DESPUÉS del
        // último volcado y se le contesta con una revisión que no va a llegar a
        // ningún disco. Es la misma lección que `pin_for_task` en #205.
        if g.sealed {
            return Err(PutError::Sealed);
        }
        if g.session.revision != revision {
            return Err(PutError::Conflict {
                current: g.session.revision,
            });
        }
        // Un esquema que este core no sabe leer NO se escribe (#247). El
        // guard vive aquí y no en el handler por lo mismo que el del tamaño:
        // lo comprueba quien va a guardarlo, que es el único que sabe qué
        // puede volver a leer. `0` es «el cliente no lo dijo» y también se
        // rechaza: la sesión almacenada la lee `disk::load`, que decide por
        // este número, y un cero acaba escrito junto a un cuerpo de verdad.
        if version == 0 || version > crate::ui_session::disk::SCHEMA_VERSION {
            return Err(PutError::UnknownSchema {
                version,
                known: crate::ui_session::disk::SCHEMA_VERSION,
            });
        }
        g.session.version = version;
        g.session.body = body;
        // `saturating_add`: la revisión ENTRA del fichero, y un fichero es algo
        // que cualquier proceso del mismo uid puede dejar puesto. En el tope,
        // seguir aceptando `put` y dejar de contar es lo peor que pasa; sumar
        // sin más era un panic en debug —dentro del mutex, envenenándolo— y una
        // vuelta a cero en release, que reabre justo la ventana de escritor
        // rancio que la revisión existe para cerrar.
        g.session.revision = g.session.revision.saturating_add(1);
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

    /// Cierra la sesión: a partir de aquí ningún `put` entra.
    ///
    /// Lo llama el apagado JUSTO antes del último volcado. Lo que llegue
    /// después recibe una negativa honesta en vez de un `Ok(revision)` sobre un
    /// fichero que ya no va a escribir nadie —y cuyo lock está a punto de
    /// soltarse—.
    pub fn seal(&self) {
        self.lock().sealed = true;
    }

    /// ¿Está cerrada?
    #[must_use]
    pub fn sealed(&self) -> bool {
        self.lock().sealed
    }

    /// Adopta el documento que hay EN DISCO al conseguir tarde el derecho a
    /// escribir.
    ///
    /// Se lleva el cuerpo Y la revisión, no solo el número. Mientras este
    /// proceso corría suelto, el que tenía el lock siguió guardando: su
    /// documento es el vigente, y quedarse solo con su revisión significaba
    /// contestarle al cliente su PROPIO cuerpo con el número del otro — y que
    /// la primera escritura tras el relevo pisara, sin conflicto y sin aviso,
    /// todo lo que el otro había guardado. Lo que el cliente quiera conservar
    /// de ese documento lo decide él, que es el único que sabe leerlo.
    ///
    /// Una revisión más baja no se adopta: el número no va hacia atrás.
    pub fn adopt_from_disk(&self, session: Session) {
        let mut g = self.lock();
        if session.revision >= g.session.revision {
            g.session = session;
            // Lo adoptado ya ESTÁ en disco: marcarlo sucio lo reescribiría
            // igual, y el primer volcado del relevo sería una copia.
            g.dirty = false;
        }
    }

    /// Sube la revisión a la que ya hay EN DISCO, si es más alta.
    ///
    /// Es para un caso concreto: un proceso que arrancó sin el derecho a
    /// escribir y lo consigue más tarde (la ventana que lo tenía se cerró).
    /// Mientras estaba suelto, la otra siguió subiendo la revisión del fichero,
    /// y volcar la nuestra tal cual la renumeraría HACIA ATRÁS — «la sube el
    /// core en cada put aceptado» dejaría de ser verdad para quien lea el
    /// fichero después.
    ///
    /// El cuerpo NO se toca: la pantalla que se guarda es la de esta ventana,
    /// que es la que sigue viva. Lo que el cliente tiene que hacer con lo que
    /// guardó la otra —conservarle los huecos que solo ella tenía— lo decide
    /// el cliente, que es el único que sabe leer el cuerpo.
    pub fn adopt_revision(&self, revision: u64) {
        let mut g = self.lock();
        if revision > g.session.revision {
            g.session.revision = revision;
        }
    }

    /// Vuelve a marcar sucio lo que [`Self::take_dirty`] se llevó y no se pudo
    /// escribir.
    ///
    /// Sin esto, un fallo de volcado —un disco lleno, un `EIO` de un momento—
    /// no se reintenta jamás: la marca ya estaba limpia, así que el tick
    /// siguiente no ve nada que hacer y la sesión se pierde hasta que el
    /// humano vuelva a mover algo. El precio de reintentar es un `open` por
    /// segundo mientras dure el fallo.
    pub fn mark_dirty(&self) {
        self.lock().dirty = true;
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

    /// Un cuerpo de un esquema que este core no sabe LEER no se escribe
    /// (#247).
    ///
    /// Aceptarlo era el peor desenlace posible: el core volcaba un documento
    /// que su propio guard de carga rechaza, así que desde el arranque
    /// siguiente la sesión quedaba «del futuro» para siempre, sin dueña y sin
    /// persistencia, hasta que alguien borrase el fichero a mano. Un `ntc` más
    /// nuevo contra un `norte` más viejo bastaba: los dos `SCHEMA_VERSION`
    /// viven en crates distintos.
    #[test]
    fn un_esquema_que_este_core_no_sabe_leer_no_se_escribe() {
        let s = SessionStore::default();
        assert!(matches!(
            s.put(disk::SCHEMA_VERSION + 1, 0, body(1)),
            Err(PutError::UnknownSchema { .. })
        ));
        assert_eq!(s.get().revision, 0, "y no cuenta como escritura");
        assert!(s.take_dirty().is_none(), "ni deja nada que volcar");
        // El cero es «el cliente no lo dijo», y la sesión almacenada se lee
        // POR ese número: escribirlo junto a un cuerpo de verdad es dejar un
        // fichero que no se sabe interpretar.
        assert!(matches!(
            s.put(0, 0, body(1)),
            Err(PutError::UnknownSchema { .. })
        ));
        // Y la versión que este core sabe leer sí entra.
        assert_eq!(s.put(disk::SCHEMA_VERSION, 0, body(1)).ok(), Some(1));
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

    /// El tope es un `<=`, y eso se comprueba en el byte exacto: un cuerpo de
    /// justo [`SESSION_BODY_MAX`] entra, y uno de un byte más no.
    #[test]
    fn el_tope_exacto_entra_y_uno_mas_no() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        // `{"x":"…"}` son 8 bytes de sobre.
        let justo = serde_json::json!({ "x": "y".repeat(SESSION_BODY_MAX - 8) });
        assert_eq!(
            serde_json::to_vec(&justo).expect("serializa").len(),
            SESSION_BODY_MAX,
            "la fixture tiene que medir el tope EXACTO"
        );
        s.put(1, 0, justo).expect("el tope exacto entra");
        let pasado = serde_json::json!({ "x": "y".repeat(SESSION_BODY_MAX - 7) });
        let e = s.put(1, 1, pasado).expect_err("uno más no");
        assert!(matches!(e, PutError::TooLarge { .. }), "{e:?}");
    }

    /// **#233**: cerrada la sesión, un `put` ya no entra — y el cierre se
    /// comprueba BAJO EL MISMO LOCK que la mutación.
    ///
    /// Contra un token atómico aparte quedaba la ventana entera: un hilo
    /// desalojado entre «¿se está apagando?» y el lock del almacén escribía
    /// DESPUÉS del último volcado, y se le contestaba con una revisión que no
    /// iba a llegar a ningún disco.
    #[test]
    fn cerrada_la_sesion_un_put_no_entra() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        s.put(1, 0, body(1)).expect("antes del cierre entra");
        assert!(!s.sealed());
        s.seal();
        assert!(s.sealed());
        let e = s.put(1, 1, body(2)).expect_err("después no");
        assert!(matches!(e, PutError::Sealed), "{e:?}");
        assert_eq!(s.get().body, body(1), "y lo almacenado se queda");
    }

    /// Al conseguir tarde el derecho a escribir se adopta el DOCUMENTO entero,
    /// no solo su número.
    ///
    /// Quedarse con la revisión y no con el cuerpo hacía que la primera
    /// escritura tras el relevo encajara sin conflicto y pisara, sin un aviso,
    /// todo lo que la otra ventana había guardado mientras esta corría suelta.
    #[test]
    fn adoptar_lo_de_disco_se_lleva_el_cuerpo_y_no_solo_la_revision() {
        let s = SessionStore::default();
        s.adopt_from_disk(Session {
            version: 1,
            revision: 42,
            body: body(3),
        });
        let g = s.get();
        assert_eq!(g.revision, 42);
        assert_eq!(g.body, body(3), "el cuerpo de la otra ventana");
        assert!(!s.dirty(), "lo adoptado ya está en disco");
        // Y no va hacia atrás: un fichero más viejo no desmonta lo vigente.
        s.adopt_from_disk(Session {
            version: 1,
            revision: 7,
            body: body(9),
        });
        assert_eq!(s.get().revision, 42);
    }

    /// Un volcado que falla vuelve a marcar sucio    /// Un volcado que falla vuelve a marcar sucio: sin esto la marca ya estaba
    /// limpia y el tick siguiente no reintentaba NADA.
    #[test]
    fn un_volcado_fallido_se_vuelve_a_marcar() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        s.put(1, 0, body(1)).expect("put");
        let llevada = s.take_dirty().expect("hay que escribir");
        assert!(!s.dirty());
        // Aquí el escritor falla (disco lleno, EIO…).
        s.mark_dirty();
        assert!(s.dirty(), "el siguiente tick lo reintenta");
        assert_eq!(s.take_dirty().expect("otra vez").body, llevada.body);
    }

    /// Dos escritores a la vez sobre el mismo store: las revisiones salen
    /// consecutivas y ninguna se pierde. El mutex es el que lo garantiza, y
    /// esto es lo que lo fija.
    #[test]
    fn dos_hilos_no_se_pisan_la_revision() {
        let s = std::sync::Arc::new(SessionStore::default());
        assert!(s.claim(1));
        let hilos: Vec<_> = (0..4)
            .map(|_| {
                let s = std::sync::Arc::clone(&s);
                std::thread::spawn(move || {
                    let mut hechos = 0_u32;
                    for _ in 0..50 {
                        let rev = s.get().revision;
                        if s.put(1, rev, body(1)).is_ok() {
                            hechos += 1;
                        }
                    }
                    hechos
                })
            })
            .collect();
        let aceptados: u32 = hilos.into_iter().map(|h| h.join().expect("hilo")).sum();
        assert_eq!(
            u64::from(aceptados),
            s.get().revision,
            "una revisión por put aceptado, ni una de más"
        );
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
