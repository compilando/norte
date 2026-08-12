//! El **spool**: el plan aprobado, retenido en un fichero (ADR 0049).
//!
//! `sync.apply` no lleva más que un `plan_hash` —no hay un segundo parámetro
//! por el que pudiera llegar otra intención—, así que lo que se ejecuta es lo
//! que se aprobó por la FORMA del wire y no por una comprobación que alguien
//! pueda olvidar. Para que eso funcione el daemon tiene que RETENER el plan, y
//! esto es dónde lo retiene.
//!
//! # Un fichero por plan, y su nombre es la llave
//! `<state_dir>/sync-spools/<conn_id>-<plan_hash>.jsonl`. El id de la conexión
//! va **en el nombre**, así que «nadie aplica un plan que no produjo» es una
//! propiedad de la BÚSQUEDA: [`Spool::open`] compone el nombre con el `conn_id`
//! de quien pregunta, y una conexión no puede nombrar el fichero de otra
//! aunque conozca su hash. Ni el `conn_id` (un `u64` en decimal) ni el
//! `plan_hash` ([`PlanHash`] valida 64 hex minúscula al deserializar) pueden
//! contener un separador de rutas: el nombre es seguro por construcción, no
//! por saneado.
//!
//! # El nombre no basta, y aquí está lo que lo acompaña
//! Un nombre no es un secreto: quien pueda LISTAR el directorio lo lee, y quien
//! pueda ESCRIBIR en él puede fabricar un fichero con ese nombre. El
//! `plan_hash` tampoco ayuda solo, porque [`PlanHasher`] no lleva clave: quien
//! escriba un plan cualquiera puede calcular su digest y ponérselo de nombre.
//! Sin nada más, un fichero colocado en el directorio sería un plan aprobado
//! que nadie aprobó — con el diálogo de aprobación saltado entero.
//!
//! Así que el nombre va acompañado de dos cosas:
//!
//! 1. **Un registro EN MEMORIA de lo que este proceso emitió.**
//!    [`SpoolWriter::finish`] apunta el `(conn_id, plan_hash)` en el
//!    [`Spool`], y [`Spool::open`] lo exige antes de tocar el disco. Un fichero
//!    que este daemon no escribió no se abre aunque esté ahí, se llame como se
//!    llame — y un plan de un ARRANQUE anterior tampoco, que es lo que cierra
//!    del todo el reciclado de `conn_id` (empiezan por cero en cada arranque).
//! 2. **El digest se RECALCULA al abrir.** El resumen del fichero dice un hash,
//!    pero eso es el fichero hablando de sí mismo; [`Spool::open`] vuelve a
//!    hashear los pasos con la semilla de la cabecera y compara. Editar un
//!    `kind` o una `rel` de un plan ya aprobado deja de colar.
//!
//! Y ese mismo registro en memoria es lo que hace el plan de **un solo uso**:
//! `open` se lo LLEVA. Dos `sync.apply` del mismo hash a la vez ejecutarían el
//! plan dos veces contra el mismo destino, con dos `batch_id` distintos y un
//! undo que ya no describe ningún estado por el que se haya pasado.
//!
//! Lo que esto NO defiende: quien pueda escribir en el directorio corre con el
//! uid del daemon, y con ese uid puede reescribir `policy.toml`. La integridad
//! del spool es la del directorio de estado y ni un gramo más.
//!
//! # Qué hay dentro, y qué no
//! Una línea JSON por registro:
//!
//! | línea | registro |
//! | --- | --- |
//! | primera | [`SpoolHeader`]: las dos raíces, el modo y las opciones de comparación con las que se planificó |
//! | intermedias | un [`SyncStep`] cada una, en orden de plan |
//! | última | [`SpoolSummary`]: el hash, los contadores, los bloqueos y `executable` |
//!
//! **Jamás contenido.** Rutas, tamaños y veredictos: exactamente lo que el
//! humano vio en el diálogo de aprobación. Un fichero que autoriza escrituras
//! no puede ser además el dato.
//!
//! Escribir y leer son en STREAMING —el buffer de escritura tiene tope y la
//! lectura va por trozos—, así que un plan de medio millón de pasos cuesta lo
//! mismo en memoria que uno de tres.
//!
//! # Cuatro formas de morir
//! 1. **Aplicado** — [`Spool::remove`], que la Task de `sync.apply` llama al
//!    terminar, en cualquier estado. **Sin llamador todavía: lo cablea la tarea
//!    9.** El derecho a aplicar, en cambio, se consume en [`Spool::open`], así
//!    que un plan no se puede ejecutar dos veces ni aunque el fichero siga ahí.
//! 2. **TTL** — [`SYNC_PLAN_TTL_MS`] contra el mtime, comprobado en cada
//!    [`Spool::open`], que además BORRA el caducado según lo encuentra.
//! 3. **Conexión cerrada** — [`Spool::drop_connection`], desde el desmontaje de
//!    la conexión en el daemon (tarea 8).
//! 4. **Arranque del daemon** — [`Spool::sweep`], porque un cierre violento
//!    deja ficheros detrás y nadie más los va a recoger. Se cablea en
//!    `norte-cli`, junto al journal.
//!
//! Y una quinta que no es una muerte sino un no-nacimiento: un plan que no
//! llega a [`SpoolWriter::finish`] no existe. Se escribe con nombre `.part` y
//! solo el `rename` final le da el nombre por el que se puede abrir, así que un
//! plan cancelado o un daemon que se muere a mitad **no dejan nada que parezca
//! aprobable**. El registro terminador no sobra por eso: el `rename` es atómico
//! respecto al *directorio*, pero no promete que los datos estén en disco tras
//! un corte de corriente, y un fichero truncado con el nombre bueno se detecta
//! porque su última línea no es el terminador.
//!
//! Un [`SpoolReader`] SOBREVIVE a las cuatro: se queda con el descriptor
//! abierto, así que en unix un borrado por debajo no le corta la lectura, y el
//! TTL no se vuelve a mirar a mitad de una ejecución. Es deliberado — lo que
//! protege al destino mientras se aplica es el `stat` de revalidación por paso
//! (ADR 0049), no el TTL, y abortar a mitad dejaría un lote del journal abierto
//! a cambio de nada.
//!
//! # Permisos
//! El directorio se crea `0o700` y cada fichero `0o600` **de nacimiento** (unix),
//! con `mode` en la propia llamada de creación. Hacer `chmod` después deja una
//! ventana en la que el plan es legible por todo el mundo, y esa ventana es el
//! bug entero.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::StreamExt as _;
use futures::stream::FusedStream;
use norte_proto::methods::{
    PlanHash, SYNC_MAX_BLOCKERS_REPORTED, SYNC_PLAN_TTL_MS, SyncBlocker, SyncCompareOptions,
    SyncCounts, SyncStep,
};
use norte_sync::{PlanHasher, PlanItem, SyncOptions};
use serde::{Deserialize, Serialize};

/// El subdirectorio del estado del daemon donde viven los spools. Hermano de
/// `journal.db` y `policy.toml`.
pub const SPOOL_DIR_NAME: &str = "sync-spools";

/// Versión del FORMATO del fichero. No es una promesa de compatibilidad: un
/// spool lo escribe y lo lee el mismo binario dentro de la ventana del TTL, así
/// que un número distinto significa «este fichero es de otro norte» y el plan
/// se declara rancio ([`SpoolError::Malformed`]), no se migra.
pub const SPOOL_FORMAT: u32 = 1;

/// Tope de UN registro. El terminador es el grande: hasta
/// [`SYNC_MAX_BLOCKERS_REPORTED`] bloqueos con su `rel`. Existe para que un
/// fichero corrupto (o de otro programa) no pueda pedir memoria sin límite.
const SPOOL_MAX_RECORD: usize = 8 << 20;

/// Cuánto se acumula en memoria antes de bajar al disco. Un `write` por paso
/// serían medio millón de `spawn_blocking`.
const WRITE_BUFFER_BYTES: usize = 64 * 1024;

/// Cuánto lee UN `spawn_blocking` de la lectura, por el mismo motivo.
const READ_CHUNK_BYTES: usize = 64 * 1024;

/// Qué le pasó al plan retenido.
///
/// [`SpoolError::NotFound`], [`SpoolError::Expired`] y
/// [`SpoolError::Malformed`] son la MISMA respuesta de cara al cliente —
/// `Error::PlanStale`, ver [`SpoolError::is_stale`]— porque las tres dicen «no
/// hay un plan vivo con ese hash». Solo [`SpoolError::Io`] es un fallo del
/// daemon.
///
/// El texto de `Malformed` es para el LOG del daemon y no para el wire: se
/// queda aquí y `PlanStale` no lleva nada. Hoy solo trae desplazamientos de
/// serde, pero está a un refactor de traer un trozo de ruta.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SpoolError {
    /// No hay ningún plan retenido con ese hash para esa conexión. Incluye el
    /// caso «lo produjo otra conexión»: el nombre del fichero no coincide.
    #[error("no hay plan retenido con ese hash para esta conexión")]
    NotFound,
    /// Lo hubo y se pasó de [`SYNC_PLAN_TTL_MS`]. Ya está borrado.
    #[error("el plan retenido caducó")]
    Expired,
    /// El fichero está ahí y no se puede leer como un plan: truncado, de otra
    /// versión del formato, manipulado (el digest recalculado no cuadra), o
    /// escrito por un binario que no conocía un campo que hoy es obligatorio.
    ///
    /// Es DELIBERADO que esto no sea recuperable. Los contadores nuevos de
    /// [`SyncCounts`] no llevan `serde(default)`: un spool de un binario viejo
    /// falla al deserializar en vez de leer un cero silencioso, porque un
    /// diálogo que aprobó «340 ficheros sin medir» y una ejecución que cree que
    /// no hay ninguno no son el mismo acto.
    #[error("el spool no se puede leer como un plan: {0}")]
    Malformed(String),
    /// El flujo del plan no llegó a su fin —cancelación, o un fallo del
    /// planificador— y por tanto no hay plan que retener. El `.part` ya está
    /// borrado.
    #[error("el plan se interrumpió antes de terminar")]
    Interrupted,
    /// I/O de verdad sobre el spool.
    #[error("I/O sobre el spool: {0}")]
    Io(#[from] io::Error),
}

impl SpoolError {
    /// ¿Es de las que significan «no hay plan vivo»?
    ///
    /// Quien sirve `sync.apply` traduce `true` a
    /// [`Error::PlanStale`](norte_proto::Error::PlanStale) y `false` a un fallo
    /// interno. Un fichero ilegible es un plan rancio, **no una Task muerta**:
    /// el cliente puede volver a planificar, que es exactamente lo que la
    /// respuesta le está diciendo.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        matches!(
            self,
            SpoolError::NotFound | SpoolError::Expired | SpoolError::Malformed(_)
        )
    }
}

/// La primera línea: con qué se planificó.
///
/// El ejecutor la necesita entera. `sync.apply` no lleva las raíces —lleva el
/// hash y nada más—, así que si no estuvieran aquí no estarían en ningún sitio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpoolHeader {
    /// [`SPOOL_FORMAT`] cuando se escribió.
    pub format: u32,
    /// La conexión que planificó. Redundante con el nombre del fichero, y ahí
    /// está la gracia: [`Spool::open`] comprueba que coinciden, así que un
    /// fichero renombrado a mano no se abre.
    pub conn_id: u64,
    /// Las raíces, el modo, `on_unknown`, el lado del origen y los dos
    /// booleanos de capacidades del destino.
    pub options: SyncOptions,
    /// Con qué criterios se comparó. **No** viven en [`SyncOptions`] y entran
    /// igualmente en el `plan_hash`: un plan hecho con `hash` encendido no es
    /// el mismo que uno hecho solo con tamaño aunque los pasos salgan iguales,
    /// porque se aprobó otra cosa.
    pub compare: SyncCompareOptions,
}

/// La última línea: a cuánto sumó el plan.
///
/// Es lo que [`SpoolWriter::finish`] devuelve y lo que [`Spool::open`] lee sin
/// recorrer los pasos, para que el ejecutor pueda rehusar un plan no ejecutable
/// **antes** del primer paso.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpoolSummary {
    /// La huella de lo que se aprobó, y la mitad de la llave del fichero.
    pub plan_hash: PlanHash,
    /// Los contadores del plan entero.
    pub counts: SyncCounts,
    /// Los bloqueos, recortados a [`SYNC_MAX_BLOCKERS_REPORTED`]. Es la
    /// explicación; quien decide es `executable`.
    pub blockers: Vec<SyncBlocker>,
    /// Cuántos hubo de verdad. Sin tope: el de
    /// [`SyncBlockerKind::TypeMismatchDir`](norte_proto::methods::SyncBlockerKind::TypeMismatchDir)
    /// crece con el árbol.
    pub blockers_total: u64,
    /// `true` cuando el plan se puede ejecutar tal cual, o sea cuando no hubo
    /// NINGÚN bloqueo. Lo calcula [`SpoolWriter::finish`] y nadie más: es el
    /// campo normativo de `sync.plan_done` y quien lo derive por su cuenta
    /// tarde o temprano lo derivará distinto.
    pub executable: bool,
}

/// Un registro del fichero. Etiquetado ADYACENTEMENTE (`{"r":…,"v":…}`) para
/// que la etiqueta no pueda chocar nunca con un campo del tipo que envuelve.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "r", content = "v", deny_unknown_fields)]
enum Record {
    #[serde(rename = "head")]
    Head(SpoolHeader),
    #[serde(rename = "step")]
    Step(SyncStep),
    #[serde(rename = "end")]
    End(SpoolSummary),
}

/// El directorio de spools de un daemon, y el registro de lo que emitió.
///
/// Se construye con el directorio de estado —el mismo de `journal.db`, o sea
/// [`crate::connect::config_dir`]— y cuelga [`SPOOL_DIR_NAME`] de él.
///
/// # Uno por daemon, clonado, jamás construido dos veces
/// El registro de planes emitidos vive DETRÁS de un [`Arc`], así que un clon
/// comparte el mismo. Eso hace que dos handles del mismo daemon se vean, y
/// —igual de importante— que dos PROCESOS no se vean: un `Spool::new` nuevo
/// nace con el registro vacío y no puede abrir nada que él no haya escrito.
///
/// De ahí la regla, y no es negociable: **el daemon construye UN `Spool` y lo
/// clona**. Un segundo `Spool::new` sobre el mismo directorio no es otro handle
/// del mismo spool, es un spool que no reconoce ni un plan — y si alguien lo usa
/// para planificar, sus planes serán los únicos que pueda aplicar.
///
/// El efecto secundario es bueno: dos procesos que compartan el directorio de
/// estado (el motor embebido de la CLI, que no abre journal y por tanto no toma
/// su lock exclusivo) no se pueden aplicar los planes el uno al otro, ni
/// siquiera con el mismo `conn_id`.
#[derive(Debug, Clone)]
pub struct Spool {
    dir: PathBuf,
    /// Todo lo que este PROCESO sabe de sus planes, bajo un solo lock: sin él,
    /// «¿está viva la conexión?» y «¿se apunta el plan?» serían dos decisiones
    /// con un hueco en medio, que es exactamente donde caben las carreras que
    /// [`Registry`] existe para cerrar.
    reg: Arc<Mutex<Registry>>,
}

/// Lo que este proceso sabe de sus propios planes. Todo junto y bajo un lock.
#[derive(Debug, Default)]
struct Registry {
    /// Los `(conn_id, plan_hash)` que ESTE proceso emitió y todavía nadie
    /// aplicó. Es a la vez la prueba de emisión y el derecho de un solo uso.
    issued: HashSet<(u64, PlanHash)>,
    /// Los que alguien está aplicando AHORA: [`Spool::open`] se llevó el
    /// derecho y [`Spool::remove`] todavía no ha pasado.
    ///
    /// Existe porque replanificar el mismo árbol con las mismas opciones da el
    /// MISMO hash, y sin esto [`SpoolWriter::finish`] volvería a acuñar un
    /// derecho que un `open` acababa de consumir: dos ejecuciones del mismo plan
    /// contra el mismo destino, con dos lotes del journal y un undo que ya no
    /// describe ningún estado por el que se haya pasado.
    applying: HashSet<(u64, PlanHash)>,
    /// Cuántos planes tiene EN VUELO cada conexión (writers abiertos). Es lo que
    /// acota `dead`: solo se recuerda a una conexión muerta mientras alguno de
    /// sus planes siga escribiéndose.
    planning: HashMap<u64, usize>,
    /// Conexiones que se cerraron TENIENDO un plan a medias.
    ///
    /// Sin esto, un plan que termina después del desmontaje de su conexión
    /// renombra su fichero y se apunta como emitido DESPUÉS de que la única
    /// muerte que le tocaba —«conexión cerrada»— ya haya pasado: queda un plan
    /// retenido que nadie puede aplicar y que nadie va a recoger. Y el caso no
    /// necesita ninguna carrera para darse: un plan que no emite NI UN paso
    /// (dos árboles idénticos) nunca toca el canal, así que jamás se entera de
    /// que su dueño se fue.
    dead: HashSet<u64>,
}

impl Spool {
    /// Ancla el spool bajo `state_dir`. No toca el disco: el directorio se crea
    /// en el primer [`Spool::create`].
    ///
    /// Ver la nota del tipo: esto se llama UNA vez por daemon y el handle se
    /// clona. Llamarlo dos veces crea dos registros de emisión que no se ven.
    #[must_use]
    pub fn new(state_dir: impl AsRef<Path>) -> Self {
        Self {
            dir: state_dir.as_ref().join(SPOOL_DIR_NAME),
            reg: Arc::new(Mutex::new(Registry::default())),
        }
    }

    /// El registro, o `None` si el lock está envenenado. Un lock roto se trata
    /// como «no sé nada»: fail-closed en todos los usos de abajo.
    fn reg(&self) -> Option<std::sync::MutexGuard<'_, Registry>> {
        self.reg.lock().ok()
    }

    /// Apunta un plan como emitido, salvo que ya no proceda. Lo llama
    /// [`SpoolWriter::finish`] DESPUÉS del rename y bajo el mismo lock que mira
    /// las dos razones para no hacerlo, que es lo que lo hace atómico frente a
    /// un cierre de conexión o un `open` simultáneos.
    ///
    /// `false` = no se apuntó, y el fichero recién renombrado hay que quitarlo.
    fn record_issued(&self, conn_id: u64, hash: &PlanHash) -> bool {
        let Some(mut reg) = self.reg() else {
            return false;
        };
        if reg.dead.contains(&conn_id) || reg.applying.contains(&(conn_id, hash.clone())) {
            return false;
        }
        reg.issued.insert((conn_id, hash.clone()));
        true
    }

    /// Se LLEVA el derecho a aplicar `hash` y lo pasa a «aplicándose». `false`
    /// si no lo había: o no lo emitió este proceso, o alguien ya lo aplicó.
    fn claim_issued(&self, conn_id: u64, hash: &PlanHash) -> bool {
        let Some(mut reg) = self.reg() else {
            return false;
        };
        let key = (conn_id, hash.clone());
        if !reg.issued.remove(&key) {
            return false;
        }
        reg.applying.insert(key);
        true
    }

    /// Olvida los planes de una conexión (o todos, con `None`).
    ///
    /// Con `Some`, marca además la conexión como MUERTA si todavía tenía algún
    /// plan escribiéndose: ese plan no puede terminar en un plan aprobable.
    fn forget_issued(&self, conn_id: Option<u64>) {
        let Some(mut reg) = self.reg() else {
            return;
        };
        let Some(id) = conn_id else {
            reg.issued.clear();
            reg.applying.clear();
            reg.dead.clear();
            return;
        };
        reg.issued.retain(|(c, _)| *c != id);
        reg.applying.retain(|(c, _)| *c != id);
        if reg.planning.contains_key(&id) {
            reg.dead.insert(id);
        }
    }

    /// Suelta la marca de «aplicándose». Lo llama [`Spool::remove`], que es lo
    /// que la Task de `sync.apply` invoca al terminar en cualquier estado.
    fn release_applying(&self, conn_id: u64, hash: &PlanHash) {
        if let Some(mut reg) = self.reg() {
            reg.applying.remove(&(conn_id, hash.clone()));
        }
    }

    /// ¿Se cerró la conexión de este plan mientras se escribía?
    fn is_dead(&self, conn_id: u64) -> bool {
        self.reg().is_some_and(|reg| reg.dead.contains(&conn_id))
    }

    /// Apunta un writer abierto para `conn_id`.
    fn open_writer(&self, conn_id: u64) {
        if let Some(mut reg) = self.reg() {
            *reg.planning.entry(conn_id).or_insert(0) += 1;
        }
    }

    /// Cierra un writer. Con el último de una conexión se olvida también su
    /// lápida: `dead` no crece con el contador de conexiones, solo con las que
    /// tienen un plan a medias justo cuando se caen.
    fn close_writer(&self, conn_id: u64) {
        let Some(mut reg) = self.reg() else {
            return;
        };
        if let Some(n) = reg.planning.get_mut(&conn_id) {
            *n -= 1;
            if *n == 0 {
                reg.planning.remove(&conn_id);
                reg.dead.remove(&conn_id);
            }
        }
    }

    /// Cuántos planes RETENIDOS (emitidos y sin aplicar) tiene una conexión.
    ///
    /// Lo consulta el daemon antes de aceptar otro `sync.plan`: un plan retenido
    /// es un fichero en disco con el listado de dos árboles, y nada más que el
    /// cierre de la conexión lo recoge mientras siga viva.
    #[must_use]
    pub fn retained_for(&self, conn_id: u64) -> usize {
        self.reg().map_or(0, |reg| {
            reg.issued.iter().filter(|(c, _)| *c == conn_id).count()
        })
    }

    /// El directorio, para quien lo quiera loguear.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Abre un plan NUEVO para `conn_id`.
    ///
    /// El fichero nace con nombre `.part` porque el `plan_hash` —la otra mitad
    /// de su nombre— todavía no existe: se calcula en streaming sobre los
    /// elementos del plan y no se sabe hasta que el flujo termina. Solo
    /// [`SpoolWriter::finish`] le pone el nombre bueno.
    ///
    /// # El TTL se cobra AQUÍ, y no hay ningún otro sitio donde se cobre
    /// [`SYNC_PLAN_TTL_MS`] se comprueba en [`Spool::open`], pero un plan que
    /// nadie abre no se abre nunca: sin esto, «el TTL» no sería una de las
    /// cuatro muertes sino una comprobación que solo corre cuando ya no hace
    /// falta. Así que cada plan nuevo barre primero los `.jsonl` vencidos, que
    /// acota lo retenido a lo planificado en los últimos diez minutos sin
    /// necesidad de un hilo con temporizador.
    ///
    /// Un `.part` NO se toca por antigüedad: un plan sobre un árbol de red
    /// tarda horas legítimamente, y su fichero lleva su mtime original. De los
    /// `.part` huérfanos se encargan el `Drop` del writer y el barrido de
    /// arranque.
    ///
    /// # Errors
    /// [`SpoolError::Io`] si el directorio de estado no se puede crear o el
    /// fichero no se puede abrir.
    #[tracing::instrument(skip_all, fields(conn_id))]
    pub async fn create(
        &self,
        conn_id: u64,
        options: &SyncOptions,
        compare: &SyncCompareOptions,
    ) -> Result<SpoolWriter, SpoolError> {
        let header = SpoolHeader {
            format: SPOOL_FORMAT,
            conn_id,
            options: options.clone(),
            compare: compare.clone(),
        };
        let hasher = PlanHasher::new(options, compare);
        let mut line = encode(&Record::Head(header))?;
        let dir = self.dir.clone();
        // Regla 2: `std::fs` es bloqueante y esto es un contexto async.
        let (file, part) = tokio::task::spawn_blocking(move || {
            ensure_dir(&dir)?;
            reap_expired(&dir);
            let (mut file, part) = create_part(&dir, conn_id)?;
            file.write_all(&line)?;
            line.clear();
            Ok::<_, SpoolError>((file, part))
        })
        .await
        .map_err(joined)??;
        self.open_writer(conn_id);
        Ok(SpoolWriter {
            spool: self.clone(),
            part,
            conn_id,
            file: Some(file),
            buf: Vec::with_capacity(WRITE_BUFFER_BYTES),
            hasher: Some(hasher),
            counts: SyncCounts::default(),
            blockers: Vec::new(),
            blockers_total: 0,
            finished: false,
        })
    }

    /// Abre —y CONSUME— el plan `hash` de la conexión `conn_id`.
    ///
    /// Comprueba, en este orden:
    ///
    /// 1. Que este proceso emitió ese plan para esa conexión y que nadie se lo
    ///    ha llevado ya. Es lo primero a propósito: un plan que no emitimos no
    ///    merece ni que se le mire el `stat`. **El derecho se consume aquí**, así
    ///    que dos `sync.apply` del mismo hash no pueden ejecutarse a la vez —
    ///    ejecutarían el plan dos veces contra el mismo destino, con dos lotes
    ///    del journal y un undo que ya no describe ningún estado real. El
    ///    segundo recibe [`SpoolError::NotFound`], o sea `PlanStale`, que es la
    ///    respuesta verdadera.
    /// 2. Que el fichero existe ([`SpoolError::NotFound`]).
    /// 3. Que no ha caducado; si ha caducado lo BORRA y devuelve
    ///    [`SpoolError::Expired`]. El TTL se mide sobre el `fstat` del
    ///    descriptor ya abierto, no sobre la ruta: entre mirar la ruta y abrirla
    ///    cabe otro fichero. Y el borrado comprueba que la ruta sigue nombrando
    ///    ESE inodo, porque replanificar el mismo árbol produce el mismo hash y
    ///    borrar por nombre se llevaría por delante el plan recién aprobado.
    /// 4. Que la última línea es el terminador y que cabecera y terminador dicen
    ///    la misma conexión y el mismo hash que el nombre.
    /// 5. Que el digest **recalculado** sobre los pasos coincide con el nombre.
    ///    Cuesta una lectura secuencial más del fichero, que al lado de ejecutar
    ///    el plan no se nota, y es lo que convierte «se ejecuta lo aprobado» en
    ///    una propiedad en vez de una declaración del propio fichero.
    ///
    /// Un plan con bloqueos no se puede recalcular —la lista guardada está
    /// recortada a [`SYNC_MAX_BLOCKERS_REPORTED`] y el digest los cubre todos—,
    /// pero tampoco se puede ejecutar: se exige el invariante
    /// `executable == (blockers_total == 0)`, así que todo plan EJECUTABLE pasa
    /// por la comprobación del punto 5.
    ///
    /// # Errors
    /// Ver [`SpoolError`]. Las tres primeras variantes significan lo mismo de
    /// cara al cliente ([`SpoolError::is_stale`]).
    #[tracing::instrument(skip_all, fields(conn_id, plan_hash = hash.as_str()))]
    pub async fn open(&self, conn_id: u64, hash: &PlanHash) -> Result<SpoolReader, SpoolError> {
        if !self.claim_issued(conn_id, hash) {
            return Err(SpoolError::NotFound);
        }
        let path = self.dir.join(file_name(conn_id, hash));
        let want = hash.clone();
        let (file, header, summary) =
            tokio::task::spawn_blocking(move || open_blocking(&path, conn_id, &want))
                .await
                .map_err(joined)?
                .inspect_err(|e| {
                    if let SpoolError::Malformed(why) = e {
                        // Un fichero que escribimos nosotros hace minutos y que
                        // ya no se deja leer es la señal de que alguien lo ha
                        // tocado. El cliente solo verá `PlanStale`.
                        tracing::warn!(conn = conn_id, why, "spool ilegible");
                    }
                })?;
        Ok(SpoolReader {
            file,
            header,
            summary,
        })
    }

    /// Borra el plan `hash` de `conn_id`, lo haya o no. Es lo que llama la Task
    /// de `sync.apply` al terminar, en cualquier estado (tarea 9).
    ///
    /// **Hay que llamarlo**, y no solo por higiene de disco: mientras no se
    /// llame, el plan sigue contando como «aplicándose» y replanificar ese mismo
    /// árbol con las mismas opciones —que da el mismo digest— se rehúsa. Es el
    /// lado seguro del intercambio (antes que acuñar dos veces el derecho a
    /// escribir el mismo destino), pero es un fallo visible para el usuario.
    ///
    /// # Errors
    /// [`SpoolError::Io`] solo si el borrado falla por algo que no sea «no
    /// estaba».
    #[tracing::instrument(skip_all, fields(conn_id, plan_hash = hash.as_str()))]
    pub async fn remove(&self, conn_id: u64, hash: &PlanHash) -> Result<(), SpoolError> {
        self.claim_issued(conn_id, hash);
        self.release_applying(conn_id, hash);
        let path = self.dir.join(file_name(conn_id, hash));
        tokio::task::spawn_blocking(move || match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SpoolError::Io(e)),
        })
        .await
        .map_err(joined)?
    }

    /// Borra TODOS los planes de una conexión, terminados o a medias. Se llama
    /// cuando la conexión se cae: un plan sin dueño no lo puede aplicar nadie.
    /// Lo llama el desmontaje de la conexión en el daemon.
    ///
    /// # Errors
    /// [`SpoolError::Io`] si el directorio no se puede listar. Un fichero que no
    /// se puede borrar NO aborta el barrido, pero sale en
    /// [`SweepReport::failed`]: el derecho a aplicarlo ya se ha olvidado en
    /// memoria de todos modos, así que lo que queda es basura en disco y no un
    /// plan vivo.
    #[tracing::instrument(skip_all, fields(conn_id))]
    pub async fn drop_connection(&self, conn_id: u64) -> Result<SweepReport, SpoolError> {
        self.forget_issued(Some(conn_id));
        let dir = self.dir.clone();
        let prefix = format!("{conn_id}-");
        tokio::task::spawn_blocking(move || {
            remove_matching(&dir, |name| name.starts_with(prefix.as_bytes()))
        })
        .await
        .map_err(joined)?
    }

    /// Barre el directorio ENTERO. Se llama al arrancar el daemon, junto al
    /// journal.
    ///
    /// Se lleva todos los spools, no solo los caducados, y esa es la parte
    /// importante: al arrancar no hay ninguna conexión viva, así que **todo
    /// spool que exista es de una conexión muerta** y no lo puede aplicar
    /// nadie. Además los `conn_id` vuelven a empezar por cero en cada arranque,
    /// así que dejar uno fresco sería dejar un fichero que autoriza escrituras
    /// a nombre de un id que el daemon está a punto de repartir otra vez.
    ///
    /// **No es de lo que depende esa seguridad**, y conviene tenerlo claro: lo
    /// que impide aplicar un plan de un arranque anterior es que el registro de
    /// planes emitidos vive en memoria y nace vacío, así que un barrido que
    /// falle deja basura en disco —y las rutas de dos árboles legibles por quien
    /// pueda leer el directorio de estado— pero no un plan aplicable. Por eso
    /// [`SweepReport::failed`] se avisa y no aborta el arranque: un daemon que
    /// se niega a arrancar por un fichero que no se deja borrar es peor fallo
    /// que el que evita.
    ///
    /// Con un solo daemon por directorio de estado —lo que el journal ya impone
    /// con su lock exclusivo sobre `journal.db` (ADR 0024)— tampoco se lleva por
    /// delante el spool de nadie vivo.
    ///
    /// # Errors
    /// [`SpoolError::Io`] si el directorio existe y no se puede listar. Que no
    /// exista no es un error: es lo normal en el primer arranque.
    #[tracing::instrument(skip_all)]
    pub async fn sweep(&self) -> Result<SweepReport, SpoolError> {
        self.forget_issued(None);
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || remove_matching(&dir, |_| true))
            .await
            .map_err(joined)?
    }
}

/// Qué se llevó un barrido, y qué se le resistió.
///
/// `failed` existe porque un contador de borrados a secas MIENTE: un `Ok(2)`
/// con tres ficheros todavía en disco es indistinguible de un barrido limpio, y
/// el sitio donde eso ocurre —un `remove_file` que falla— es justo donde nadie
/// mira.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Ficheros borrados.
    pub removed: usize,
    /// Ficheros que estaban y no se dejaron borrar.
    pub failed: usize,
}

impl SweepReport {
    /// ¿Se llevó todo lo que encontró?
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.failed == 0
    }
}

/// Escribe un plan mientras se planifica.
///
/// Traga [`PlanItem`], que es lo que produce `norte_sync::plan`, y hace tres
/// cosas con cada uno **a la vez**: lo hashea, lo cuenta y (si es un paso) lo
/// escribe. Que sea el mismo sitio no es comodidad: el `plan_hash` tiene que
/// resumir EXACTAMENTE los elementos que el humano ve, o sea los de después del
/// `include` de la petición, y con un solo embudo no hay forma de hashear una
/// secuencia y enseñar otra.
///
/// Quien lo usa debe empujar aquí los MISMOS elementos que manda al cliente por
/// `sync.steps`, y en el mismo orden.
///
/// # Solo se cierra un flujo que TERMINÓ, y por eso hay que decirlo
/// [`SpoolWriter::finish`] exige un [`PlanOutcome`]. No es ceremonia: el digest
/// parcial de un plan cortado a la mitad es indistinguible del de un plan
/// completo más corto, así que cerrar uno cancelado produce un `plan_hash`
/// perfectamente válido para un plan que dice sincronizar un árbol que se
/// recorrió un tercio. El humano aprueba «412 ficheros», se copian 412, y los
/// 400 000 que faltaban no se copian nunca sin que nada lo diga.
///
/// El bucle que lo destruye es este, y es el que sale solo:
///
/// ```ignore
/// while let Some(Ok(item)) = items.next().await { w.push(&item).await?; }
/// let s = w.finish(PlanOutcome::Ended).await?;   // MENTIRA si hubo un Err
/// ```
///
/// `Some(Err(_))` sale por el mismo sitio que `None`. Solo el brazo `None` puede
/// pasar [`PlanOutcome::Ended`]; el de error pasa
/// [`PlanOutcome::Interrupted`], que borra el `.part` y no devuelve hash
/// ninguno.
#[derive(Debug)]
pub struct SpoolWriter {
    /// El spool que lo creó: `finish` apunta ahí el plan como emitido, que es
    /// lo que después deja que se abra.
    spool: Spool,
    part: PathBuf,
    conn_id: u64,
    /// `None` solo mientras un `spawn_blocking` lo tiene prestado, y después de
    /// [`SpoolWriter::finish`].
    file: Option<std::fs::File>,
    buf: Vec<u8>,
    /// `Option` por lo mismo que `file`: `SpoolWriter` implementa `Drop`, así
    /// que no se puede sacar un campo de él sin dejar algo en su sitio.
    hasher: Option<PlanHasher>,
    counts: SyncCounts,
    blockers: Vec<SyncBlocker>,
    blockers_total: u64,
    finished: bool,
}

/// Cómo terminó el flujo del plan. Lo exige [`SpoolWriter::finish`] para que
/// nadie pueda cerrar un plan a medias sin haberlo escrito.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanOutcome {
    /// El flujo devolvió `None`: el plan está entero. **Solo desde ese brazo.**
    Ended,
    /// El flujo se cortó — cancelación, o un [`norte_sync::SyncError`]. El spool
    /// se borra y no hay hash.
    Interrupted,
}

impl SpoolWriter {
    /// Traga un elemento del plan.
    ///
    /// Un paso se hashea, se cuenta y se escribe. Un bloqueo se hashea y se
    /// cuenta —todos, sin tope, porque el hash tiene que distinguir dos planes
    /// que difieren en el bloqueo 257— pero solo los primeros
    /// [`SYNC_MAX_BLOCKERS_REPORTED`] se guardan para poder nombrarlos.
    ///
    /// # Errors
    /// [`SpoolError::Io`] si la escritura falla. Un error aquí deja el plan sin
    /// terminar, que es lo mismo que no haberlo hecho.
    pub async fn push(&mut self, item: &PlanItem) -> Result<(), SpoolError> {
        let Some(hasher) = self.hasher.as_mut() else {
            return Err(SpoolError::Malformed("el spool ya está cerrado".to_owned()));
        };
        hasher.item(item);
        match item {
            PlanItem::Step(step) => {
                self.counts.add(step);
                let line = encode(&Record::Step(step.clone()))?;
                self.buf.extend_from_slice(&line);
                if self.buf.len() >= WRITE_BUFFER_BYTES {
                    self.flush().await?;
                }
            }
            PlanItem::Blocker(blocker) => {
                self.blockers_total = self.blockers_total.saturating_add(1);
                if self.blockers.len() < SYNC_MAX_BLOCKERS_REPORTED {
                    self.blockers.push(blocker.clone());
                }
            }
        }
        Ok(())
    }

    /// Cierra el plan: escribe el terminador, le pone al fichero su nombre
    /// definitivo (`<conn_id>-<plan_hash>.jsonl`) y lo apunta como emitido.
    ///
    /// Hasta ese `rename` el plan no tiene el nombre por el que se busca, y
    /// hasta ese apunte no hay derecho a aplicarlo: las dos cosas juntas son lo
    /// que hace que un plan cancelado —o un daemon que se muere a mitad— no deje
    /// nada aprobable.
    ///
    /// `outcome` no es decoración: ver la nota del tipo. Con
    /// [`PlanOutcome::Interrupted`] esto borra el `.part` y devuelve
    /// [`SpoolError::Interrupted`] sin escribir terminador ninguno.
    ///
    /// Si el futuro se suelta EN el `rename`, la tarea bloqueante puede
    /// completarlo igual y dejar un spool con nombre bueno cuyo hash el llamante
    /// nunca supo. Nadie lo puede aplicar —no llegó a apuntarse como emitido— y
    /// se lo lleva el barrido.
    ///
    /// # Otras dos formas de NO cerrar, además de `outcome`
    /// Las dos devuelven [`SpoolError::Interrupted`], y las dos son cosas que
    /// pasaron mientras el plan se escribía y que quien lo escribe no puede ver:
    ///
    /// - **Su conexión se cerró.** El desmontaje se llevó los planes de esa
    ///   conexión, así que uno que se apuntase DESPUÉS quedaría retenido sin
    ///   dueño y sin nadie que lo recoja. No hace falta ninguna carrera para
    ///   llegar aquí: un plan sobre dos árboles idénticos no emite ni un paso, no
    ///   toca el canal y por tanto jamás se entera de que su dueño se fue.
    /// - **Un `open` se llevó el derecho de ESE hash.** Replanificar el mismo
    ///   árbol con las mismas opciones da el mismo digest; volver a acuñar el
    ///   derecho mientras alguien lo aplica es autorizar una segunda ejecución
    ///   del mismo plan contra el mismo destino, con dos lotes del journal.
    ///
    /// La comprobación buena es la de DESPUÉS del rename, que se hace bajo el
    /// mismo lock que la decisión de apuntar el plan como emitido; la de
    /// antes solo ahorra el trabajo.
    ///
    /// # Errors
    /// [`SpoolError::Interrupted`] si `outcome` lo dice, si la conexión murió o
    /// si el plan se está aplicando, y [`SpoolError::Io`] si la escritura o el
    /// `rename` fallan.
    #[tracing::instrument(skip_all, fields(conn_id = self.conn_id, ?outcome))]
    pub async fn finish(mut self, outcome: PlanOutcome) -> Result<SpoolSummary, SpoolError> {
        if outcome == PlanOutcome::Interrupted || self.spool.is_dead(self.conn_id) {
            self.abandon_inner().await;
            return Err(SpoolError::Interrupted);
        }
        let Some(hasher) = self.hasher.take() else {
            return Err(SpoolError::Malformed("el spool ya está cerrado".to_owned()));
        };
        let summary = SpoolSummary {
            plan_hash: hasher.finish(),
            counts: self.counts,
            blockers: std::mem::take(&mut self.blockers),
            blockers_total: self.blockers_total,
            executable: self.blockers_total == 0,
        };
        let line = encode(&Record::End(summary.clone()))?;
        self.buf.extend_from_slice(&line);
        self.flush().await?;

        let file = self.file.take();
        let part = self.part.clone();
        let target = self
            .spool
            .dir
            .join(file_name(self.conn_id, &summary.plan_hash));
        let landed = target.clone();
        tokio::task::spawn_blocking(move || {
            // Cerrar ANTES del rename: en Windows un fichero abierto no se
            // renombra, y en unix no cuesta nada.
            drop(file);
            std::fs::rename(&part, &target)
        })
        .await
        .map_err(joined)??;
        self.finished = true;
        // DESPUÉS del rename: apuntar un plan cuyo fichero no llegó a tener su
        // nombre sería prometer un `open` que después no encuentra nada. Y bajo
        // el lock, que es lo que decide si todavía procede — ver la nota de
        // arriba sobre las otras dos formas de no cerrar.
        if !self.spool.record_issued(self.conn_id, &summary.plan_hash) {
            tokio::task::spawn_blocking(move || {
                let _ = std::fs::remove_file(&landed);
            })
            .await
            .map_err(joined)?;
            return Err(SpoolError::Interrupted);
        }
        Ok(summary)
    }

    /// Tira el plan a medio escribir, sin devolver nada. Es
    /// `finish(PlanOutcome::Interrupted)` sin el error, para quien ya sabe que
    /// no va a haber plan.
    ///
    /// No falla: un borrado que no se puede hacer lo recoge el barrido de
    /// arranque, y el fichero no tiene el nombre por el que se busca de todos
    /// modos.
    pub async fn abandon(mut self) {
        self.abandon_inner().await;
    }

    /// El cuerpo de [`SpoolWriter::abandon`], por `&mut` para que
    /// [`SpoolWriter::finish`] lo pueda usar en el camino interrumpido.
    async fn abandon_inner(&mut self) {
        let file = self.file.take();
        let part = self.part.clone();
        self.hasher = None;
        self.finished = true;
        let _ = tokio::task::spawn_blocking(move || {
            drop(file);
            let _ = std::fs::remove_file(&part);
        })
        .await;
    }

    /// Baja el buffer al disco. El `File` viaja al hilo bloqueante y vuelve.
    async fn flush(&mut self) -> Result<(), SpoolError> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let mut file = self.file.take().ok_or_else(|| {
            // `Malformed` y no `Io`: significa que este writer ya no puede
            // producir un plan, que de cara al cliente es un plan rancio
            // ([`SpoolError::is_stale`]) y no un fallo del daemon.
            SpoolError::Malformed("el spool ya está cerrado".to_owned())
        })?;
        let chunk = std::mem::take(&mut self.buf);
        let (file, mut chunk, res) = tokio::task::spawn_blocking(move || {
            let res = file.write_all(&chunk);
            (file, chunk, res)
        })
        .await
        .map_err(joined)?;
        self.file = Some(file);
        // Se recupera la capacidad: el buffer se reusa plan entero.
        chunk.clear();
        self.buf = chunk;
        res.map_err(SpoolError::Io)
    }
}

impl Drop for SpoolWriter {
    fn drop(&mut self) {
        // SIEMPRE, cerrase como se cerrase: es el contador que acota la lápida
        // de una conexión muerta a las que de verdad tienen un plan a medias.
        self.spool.close_writer(self.conn_id);
        if self.finished {
            return;
        }
        // Un plan sin terminar no es abrible —el `.part` no tiene el nombre por
        // el que se busca y nunca se apuntó como emitido—, así que esto es
        // higiene y no una garantía: quien la da es el barrido de arranque.
        //
        // Un `unlink` SÍNCRONO, a sabiendas de la regla 2. La alternativa era
        // `Handle::spawn_blocking`, que entra en pánico cuando el runtime ya
        // está apagándose (y `try_current` sigue devolviendo `Ok` en esa
        // ventana, porque el drop ocurre dentro del contexto): un pánico en un
        // `Drop` durante un desenrollado aborta el proceso. Cambiar un abort
        // por un `unlink` que no puede bloquear de forma apreciable es el
        // intercambio correcto.
        let part = std::mem::take(&mut self.part);
        drop(self.file.take());
        let _ = std::fs::remove_file(&part);
    }
}

/// Un plan retenido, ya validado: la cabecera y el resumen están leídos y los
/// pasos se piden en streaming.
#[derive(Debug)]
pub struct SpoolReader {
    file: std::fs::File,
    header: SpoolHeader,
    summary: SpoolSummary,
}

impl SpoolReader {
    /// Con qué se planificó. El ejecutor saca de aquí las dos raíces.
    #[must_use]
    pub fn header(&self) -> &SpoolHeader {
        &self.header
    }

    /// A cuánto sumó. Se lee sin recorrer un solo paso, así que un plan no
    /// ejecutable se rehúsa antes de empezar.
    #[must_use]
    pub fn summary(&self) -> &SpoolSummary {
        &self.summary
    }

    /// Los pasos, en orden de plan.
    ///
    /// El orden es el del walk (pre-orden), así que un `CreateDir` precede a
    /// toda copia dentro de él: **no se ordena**, se ejecuta como viene.
    ///
    /// El flujo está FUSIONADO: pedirle otro elemento después del final
    /// devuelve `None` en vez de entrar en pánico, así que un `select!` con un
    /// tick de progreso encima es legal.
    ///
    /// Termina en el registro terminador, y exige que después no haya NADA: si
    /// hubiera un segundo terminador, [`Spool::open`] habría leído el último y
    /// esto ejecutaría hasta el primero — el resumen aprobado y el plan
    /// ejecutado serían dos cosas distintas. Si el fichero se acaba antes
    /// —alguien lo truncó después de abrirlo— sale un [`SpoolError::Malformed`]
    /// y no un plan a medias.
    ///
    /// El error puede llegar A MITAD, con pasos ya ejecutados: quien lo consuma
    /// necesita su lote del journal cerrado y deshacible en ese punto, no solo
    /// en el de cancelación.
    #[must_use]
    pub fn steps(self) -> impl FusedStream<Item = Result<SyncStep, SpoolError>> {
        struct State {
            reader: Option<BufReader<std::fs::File>>,
            queue: VecDeque<SyncStep>,
        }
        let state = State {
            reader: Some(BufReader::new(self.file)),
            queue: VecDeque::new(),
        };
        futures::stream::try_unfold(state, |mut state| async move {
            loop {
                if let Some(step) = state.queue.pop_front() {
                    return Ok(Some((step, state)));
                }
                let Some(mut reader) = state.reader.take() else {
                    return Ok(None);
                };
                let (reader, batch, ended) = tokio::task::spawn_blocking(move || {
                    let mut batch = Vec::new();
                    let mut read = 0usize;
                    let mut line = Vec::new();
                    let ended = loop {
                        if read >= READ_CHUNK_BYTES {
                            break false;
                        }
                        let n = read_capped_line(&mut reader, &mut line)?;
                        if n == 0 {
                            return Err(SpoolError::Malformed(
                                "el spool se acaba sin su terminador".to_owned(),
                            ));
                        }
                        read += n;
                        match decode(&line)? {
                            Record::Step(step) => batch.push(validated_step(step)?),
                            Record::End(_) => {
                                // Nada después del terminador. Con dos, `open`
                                // valida el último y esto ejecuta hasta el
                                // primero: dos planes en un fichero.
                                if read_capped_line(&mut reader, &mut line)? != 0 {
                                    return Err(SpoolError::Malformed(
                                        "hay registros después del terminador".to_owned(),
                                    ));
                                }
                                break true;
                            }
                            Record::Head(_) => {
                                return Err(SpoolError::Malformed(
                                    "una cabecera en mitad del spool".to_owned(),
                                ));
                            }
                        }
                    };
                    Ok::<_, SpoolError>((reader, batch, ended))
                })
                .await
                .map_err(joined)??;
                state.queue = batch.into();
                if !ended {
                    state.reader = Some(reader);
                }
            }
        })
        .fuse()
    }
}

// ---------------------------------------------------------------- bloqueante

/// Un paso leído del disco, comprobado.
///
/// En el WIRE, `SyncStepKind` degrada a `Unknown` y un paso mal formado no mata
/// un lote de 256 (ADR 0049) — ahí la compatibilidad hacia delante vale más. En
/// un fichero que escribió ESTE binario hace minutos no hay compatibilidad que
/// defender: una clase que no reconocemos o una forma que no se sostiene solo
/// pueden ser corrupción o manipulación, y un paso así estaría a punto de
/// autorizar una escritura.
///
/// `shape_is_consistent` es gratis aquí y es la regla 4 comprobada donde se
/// puede: un `Overwrite` que dice deshacerse borrando haría que el journal
/// apuntase una reversa falsa.
fn validated_step(step: SyncStep) -> Result<SyncStep, SpoolError> {
    if step.kind == norte_proto::methods::SyncStepKind::Unknown {
        return Err(SpoolError::Malformed(
            "un paso de clase desconocida en un spool que escribimos nosotros".to_owned(),
        ));
    }
    if !step.shape_is_consistent() {
        return Err(SpoolError::Malformed(
            "un paso cuya clase, reversa y motivo no concuerdan".to_owned(),
        ));
    }
    Ok(step)
}

/// Crea el directorio con permisos de dueño y nada más.
fn ensure_dir(dir: &Path) -> Result<(), SpoolError> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        // `recursive` + `mode` también para el directorio de estado, que es lo
        // que hace `Journal::open` con el suyo: crearlo con el umask lo dejaría
        // en 0755 según quién llegue primero.
        builder.mode(0o700);
    }
    if let Some(parent) = dir.parent() {
        let mut parents = std::fs::DirBuilder::new();
        parents.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            parents.mode(0o700);
        }
        parents.create(parent)?;
    }
    match builder.create(dir) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    // Ya existía. Que sea un directorio de verdad y no un enlace a otro sitio,
    // y que sus permisos sigan siendo los que decimos que son: un spool en un
    // directorio legible por todos es un plan legible por todos.
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(SpoolError::Io(io::Error::new(
            ErrorKind::InvalidInput,
            "el directorio de spools no es un directorio",
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o777 != 0o700 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// Contador de planes de este proceso. Con el pid delante, basta para que dos
/// planes en vuelo de la misma conexión no se peleen por el nombre.
static NEXT_PART: AtomicU64 = AtomicU64::new(0);

/// Abre el `.part`, `0o600` de nacimiento.
fn create_part(dir: &Path, conn_id: u64) -> Result<(std::fs::File, PathBuf), SpoolError> {
    let pid = std::process::id();
    for _ in 0..8 {
        let seq = NEXT_PART.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{conn_id}-{pid}-{seq}.part"));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            // `mode` en la LLAMADA, no un `chmod` después: entre crear y
            // cambiar los permisos hay una ventana en la que el plan es legible
            // por cualquiera, y esa ventana es el bug entero.
            opts.mode(0o600);
        }
        match opts.open(&path) {
            Ok(file) => return Ok((file, path)),
            // `create_new` es también la garantía de que jamás escribimos
            // dentro de un fichero que ya estaba: se reintenta con otro nombre.
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(SpoolError::Io(io::Error::new(
        ErrorKind::AlreadyExists,
        "no hay nombre libre para el spool",
    )))
}

/// El nombre por el que se busca un plan. Ni el `conn_id` ni el hash pueden
/// llevar un separador de rutas, así que no hay nada que sanear.
fn file_name(conn_id: u64, hash: &PlanHash) -> String {
    format!("{conn_id}-{}.jsonl", hash.as_str())
}

fn open_blocking(
    path: &Path,
    conn_id: u64,
    want: &PlanHash,
) -> Result<(std::fs::File, SpoolHeader, SpoolSummary), SpoolError> {
    let mut file = match std::fs::OpenOptions::new().read(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(SpoolError::NotFound),
        Err(e) => return Err(e.into()),
    };
    // `fstat` del descriptor abierto, no `stat` de la ruta: entre mirar la ruta
    // y abrirla cabe otro fichero.
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(SpoolError::NotFound);
    }
    if expired(&meta) {
        drop(file);
        remove_if_same_inode(path, &meta);
        return Err(SpoolError::Expired);
    }

    // La ÚLTIMA línea primero: sin terminador el fichero no es un plan, y así
    // no se recorre entero para descubrirlo.
    let tail = read_last_line(&mut file, meta.len())?;
    let Record::End(summary) = decode(&tail)? else {
        return Err(SpoolError::Malformed(
            "la última línea del spool no es su terminador".to_owned(),
        ));
    };

    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let head_len = read_capped_line(&mut reader, &mut line)?;
    if head_len == 0 {
        return Err(SpoolError::Malformed("spool vacío".to_owned()));
    }
    let Record::Head(header) = decode(&line)? else {
        return Err(SpoolError::Malformed(
            "la primera línea del spool no es su cabecera".to_owned(),
        ));
    };

    // El nombre del fichero dice una conexión y un hash; el CONTENIDO tiene que
    // decir los mismos. Un fichero renombrado a mano no se abre.
    if header.format != SPOOL_FORMAT {
        return Err(SpoolError::Malformed(format!(
            "formato de spool {} (este binario escribe {SPOOL_FORMAT})",
            header.format
        )));
    }
    if header.conn_id != conn_id || &summary.plan_hash != want {
        return Err(SpoolError::Malformed(
            "el spool no dice la conexión y el hash de su nombre".to_owned(),
        ));
    }
    // El invariante de `SyncPlanDone`, comprobado aquí porque es lo que decide
    // si el punto siguiente se puede hacer: un plan sin bloqueos es ejecutable y
    // se le recalcula el digest; uno con bloqueos no es ninguna de las dos
    // cosas. Sin esto, un fichero podría declararse ejecutable Y traer
    // bloqueos, y colarse por el hueco sin que nadie le recalcule nada.
    if summary.executable != (summary.blockers_total == 0) {
        return Err(SpoolError::Malformed(
            "`executable` no concuerda con el número de bloqueos".to_owned(),
        ));
    }

    let mut file = reader.into_inner();
    if summary.executable {
        verify_digest(&mut file, head_len, &header, &summary.plan_hash)?;
    }
    file.seek(SeekFrom::Start(head_len as u64))?;
    Ok((file, header, summary))
}

/// Recalcula el `plan_hash` sobre los pasos del fichero y lo compara.
///
/// El resumen guardado dice un hash, pero eso es el fichero hablando de sí
/// mismo: [`PlanHasher`] no lleva clave, así que quien pueda escribir en el
/// directorio puede escribir un plan cualquiera Y su digest. Lo que hace que el
/// nombre valga algo es el registro en memoria de [`Spool`]; lo que hace que el
/// CONTENIDO valga algo es esto. Sin ello, editar un `kind` de un plan que el
/// humano ya aprobó no lo detecta nadie.
///
/// Cuesta una lectura secuencial más. Al lado de ejecutar el plan —una operación
/// de provider por paso— es ruido.
///
/// Solo se llama sobre planes sin bloqueos: la lista guardada está recortada a
/// [`SYNC_MAX_BLOCKERS_REPORTED`] y el digest los cubre TODOS, así que uno con
/// bloqueos no se puede recalcular. Tampoco se puede ejecutar.
fn verify_digest(
    file: &mut std::fs::File,
    head_len: usize,
    header: &SpoolHeader,
    want: &PlanHash,
) -> Result<(), SpoolError> {
    file.seek(SeekFrom::Start(head_len as u64))?;
    let mut reader = BufReader::new(file);
    let mut hasher = PlanHasher::new(&header.options, &header.compare);
    let mut line = Vec::new();
    loop {
        if read_capped_line(&mut reader, &mut line)? == 0 {
            return Err(SpoolError::Malformed(
                "el spool se acaba sin su terminador".to_owned(),
            ));
        }
        match decode(&line)? {
            Record::Step(step) => hasher.step(&validated_step(step)?),
            Record::End(_) => {
                // Nada después del terminador, y se comprueba AQUÍ y no solo en
                // `steps()`: con dos terminadores IGUALES el digest cuadra —
                // `read_last_line` leyó el segundo y esto paró en el primero—,
                // así que sin esto el fichero se abriría y reventaría a mitad de
                // la ejecución en vez de antes de empezar.
                if read_capped_line(&mut reader, &mut line)? != 0 {
                    return Err(SpoolError::Malformed(
                        "hay registros después del terminador".to_owned(),
                    ));
                }
                break;
            }
            Record::Head(_) => {
                return Err(SpoolError::Malformed(
                    "una cabecera en mitad del spool".to_owned(),
                ));
            }
        }
    }
    if &hasher.finish() != want {
        return Err(SpoolError::Malformed(
            "el digest recalculado no es el del nombre: el spool se ha tocado".to_owned(),
        ));
    }
    Ok(())
}

/// Borra `path` solo si sigue nombrando el inodo que se miró.
///
/// Replanificar el mismo árbol con las mismas opciones produce el MISMO hash, y
/// `finish` renombra encima. Sin esta comprobación, un `open` que llega tarde
/// con el descriptor del fichero viejo borraría por nombre el plan recién
/// aprobado, y el humano recibiría «tu plan caducó» sobre uno de hace segundos.
fn remove_if_same_inode(path: &Path, opened: &std::fs::Metadata) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        match std::fs::symlink_metadata(path) {
            Ok(now) if now.dev() == opened.dev() && now.ino() == opened.ino() => {}
            // Ya no está, o ya es otro fichero: en los dos casos no es nuestro.
            _ => return,
        }
    }
    let _ = std::fs::remove_file(path);
}

/// ¿Se pasó del TTL? Un mtime en el FUTURO cuenta como fresco: un reloj que
/// anda hacia atrás no es motivo para tirar un plan que alguien está mirando, y
/// el techo lo pone igualmente el barrido de arranque.
fn expired(meta: &std::fs::Metadata) -> bool {
    let Ok(modified) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age > Duration::from_millis(SYNC_PLAN_TTL_MS))
}

/// La última línea completa, leyendo hacia atrás por ventanas.
///
/// El techo es [`SPOOL_MAX_RECORD`] **más dos**: para encontrar una línea de N
/// bytes de contenido hace falta ver su propio fin de línea y el de la anterior,
/// así que un techo de N justos rechazaría un terminador que `read_capped_line`
/// sí acepta — y `encode` sí escribe.
fn read_last_line(file: &mut std::fs::File, len: u64) -> Result<Vec<u8>, SpoolError> {
    if len == 0 {
        return Err(SpoolError::Malformed("spool vacío".to_owned()));
    }
    let ceiling = SPOOL_MAX_RECORD as u64 + 2;
    let mut window: u64 = 8 * 1024;
    loop {
        let start = len.saturating_sub(window);
        let take = usize::try_from(len - start)
            .map_err(|_| SpoolError::Malformed("spool inabarcable".to_owned()))?;
        file.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0u8; take];
        file.read_exact(&mut buf).map_err(truncated)?;
        let body = buf.strip_suffix(b"\n").unwrap_or(&buf);
        if let Some(pos) = body.iter().rposition(|b| *b == b'\n') {
            return Ok(body[pos + 1..].to_vec());
        }
        if start == 0 {
            return Ok(body.to_vec());
        }
        if window >= ceiling {
            return Err(SpoolError::Malformed(
                "el último registro del spool pasa del tope".to_owned(),
            ));
        }
        window = (window * 2).min(ceiling);
    }
}

/// Un fichero que se acorta bajo nuestros pies es un spool roto —o sea un plan
/// rancio—, no un fallo de I/O del daemon: `is_stale` distingue las dos cosas y
/// el cliente merece la primera respuesta.
fn truncated(e: io::Error) -> SpoolError {
    if e.kind() == ErrorKind::UnexpectedEof {
        SpoolError::Malformed("el spool se ha truncado mientras se leía".to_owned())
    } else {
        SpoolError::Io(e)
    }
}

/// Lee UNA línea con tope. `Ok(0)` es fin de fichero.
fn read_capped_line(reader: &mut impl BufRead, out: &mut Vec<u8>) -> Result<usize, SpoolError> {
    out.clear();
    let n = reader
        .by_ref()
        .take(SPOOL_MAX_RECORD as u64 + 1)
        .read_until(b'\n', out)
        .map_err(truncated)?;
    if n == 0 {
        return Ok(0);
    }
    if out.last() != Some(&b'\n') {
        return Err(SpoolError::Malformed(
            "un registro sin fin de línea: spool truncado, o el registro pasa del tope".to_owned(),
        ));
    }
    out.pop();
    Ok(n)
}

/// Borra los ficheros del directorio que cumplan `pred`, sobre los BYTES del
/// nombre (regla 1: un nombre de fichero no es una `String`).
///
/// Un borrado que falla NO aborta el barrido y NO se calla: va a
/// [`SweepReport::failed`], porque un contador de borrados a secas no distingue
/// «no había nada» de «no se pudo con nada».
fn remove_matching(dir: &Path, pred: impl Fn(&[u8]) -> bool) -> Result<SweepReport, SpoolError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // Que no exista es lo normal en el primer arranque.
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(SweepReport::default()),
        Err(e) => return Err(e.into()),
    };
    let mut report = SweepReport::default();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if !pred(name_bytes(&name)) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => report.removed += 1,
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => {
                report.failed += 1;
                tracing::warn!(path = %entry.path().display(), error = %e,
                    "no se pudo borrar un spool");
            }
        }
    }
    Ok(report)
}

/// Se lleva los planes CERRADOS que se pasaron de [`SYNC_PLAN_TTL_MS`].
///
/// Lo llama [`Spool::create`], y ese es el único reloj que el TTL tiene: la
/// comprobación de [`Spool::open`] solo alcanza a los planes que alguien abre, y
/// un plan que nadie abre es justamente el que sobra. Sin esto, un cliente que
/// planifica en bucle variando `include` —cada selección da otro digest, o sea
/// otro fichero— llena el directorio de estado, que es donde vive `journal.db`.
///
/// **Solo `.jsonl`.** Un `.part` es un plan EN CURSO y puede tardar horas
/// legítimamente sobre un árbol de red; de los huérfanos se encargan el `Drop`
/// del writer y el barrido de arranque.
///
/// No devuelve nada y no falla hacia arriba: es mantenimiento oportunista, y que
/// un fichero se resista no es motivo para no dejar planificar. El registro en
/// memoria sigue siendo lo que decide qué es aplicable.
fn reap_expired(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !name_bytes(&entry.file_name()).ends_with(b".jsonl") {
            continue;
        }
        if entry.metadata().is_ok_and(|m| expired(&m)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Los bytes de un nombre de fichero. En unix son los de verdad; fuera, lo que
/// se pueda — los nombres de spool son ASCII por construcción (dígitos, `-` y
/// hex minúscula), así que ninguno se pierde.
fn name_bytes(name: &std::ffi::OsStr) -> &[u8] {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        name.as_bytes()
    }
    #[cfg(not(unix))]
    {
        name.to_str().unwrap_or("").as_bytes()
    }
}

/// Un registro serializado, con su fin de línea.
///
/// El tope se comprueba AQUÍ y no solo al leer. Un terminador con 256 bloqueos
/// de `rel` muy largos puede pasarse de [`SPOOL_MAX_RECORD`], y si se escribe,
/// `finish` devuelve un hash y un `executable: true` para un plan que ningún
/// `open` posterior podrá volver a leer: el cliente aprobaría un plan que
/// responde «rancio» para siempre. Fallar al escribirlo pone el error donde se
/// puede ver.
fn encode(record: &Record) -> Result<Vec<u8>, SpoolError> {
    let mut line = serde_json::to_vec(record)
        .map_err(|e| SpoolError::Malformed(format!("no se pudo serializar el registro: {e}")))?;
    if line.len() > SPOOL_MAX_RECORD {
        return Err(SpoolError::Malformed(format!(
            "un registro de {} bytes pasa del tope de {SPOOL_MAX_RECORD}",
            line.len()
        )));
    }
    line.push(b'\n');
    Ok(line)
}

fn decode(line: &[u8]) -> Result<Record, SpoolError> {
    serde_json::from_slice(line).map_err(|e| SpoolError::Malformed(e.to_string()))
}

/// Un `spawn_blocking` que no vuelve es un fallo del runtime, no del plan.
fn joined(e: tokio::task::JoinError) -> SpoolError {
    SpoolError::Io(io::Error::other(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt as _;
    use norte_proto::VPath;
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, OnUnknown, RelPath, Side, StepReversal,
        SyncBlockerKind, SyncMode, SyncReason, SyncStepKind,
    };

    // ------------------------------------------------------------ fixtures

    fn opts() -> SyncOptions {
        SyncOptions {
            source_root: VPath::parse("mem:///origen").expect("path"),
            dest_root: VPath::parse("mem:///destino").expect("path"),
            mode: SyncMode::Update,
            on_unknown: OnUnknown::Copy,
            source_side: Side::Left,
            dest_has_trash: true,
            dest_writable: true,
        }
    }

    fn compare_opts() -> SyncCompareOptions {
        SyncCompareOptions::default()
    }

    fn rel(wire: &str) -> RelPath {
        RelPath::parse_wire(wire).expect("rel")
    }

    fn copy_step(id: u64, name: &str, size: u64) -> SyncStep {
        SyncStep {
            id,
            kind: SyncStepKind::Copy,
            rel: rel(name),
            dest_rel: None,
            size: Some(size),
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            reversal: Some(StepReversal::Delete),
            reason: None,
        }
    }

    /// Un plan de mesa con las cuatro formas que el ejecutor tiene que
    /// distinguir: una copia, un directorio, una sobrescritura irreversible y
    /// un salto. Con un nombre no UTF-8 dentro, que es lo que la regla 1 pide.
    fn steps_fixture() -> Vec<SyncStep> {
        vec![
            SyncStep {
                id: 1,
                kind: SyncStepKind::CreateDir,
                rel: rel("sub"),
                dest_rel: None,
                size: None,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Certain,
                reversal: Some(StepReversal::Delete),
                reason: None,
            },
            copy_step(2, "sub/informe%FF%FE.dat", 12),
            SyncStep {
                id: 3,
                kind: SyncStepKind::Overwrite,
                rel: rel("NOTAS/a.txt"),
                dest_rel: Some(rel("notas/a.txt")),
                size: Some(40),
                criterion: CompareCriterion::Mtime,
                confidence: CompareConfidence::Probable,
                reversal: Some(StepReversal::Irreversible),
                reason: Some(SyncReason::NoTrashOnTarget),
            },
            SyncStep {
                id: 4,
                kind: SyncStepKind::Skip,
                rel: rel("ilegible"),
                dest_rel: None,
                size: None,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Unknown,
                reversal: None,
                reason: Some(SyncReason::Unreadable),
            },
        ]
    }

    /// Escribe un plan entero y devuelve su hash.
    async fn write_plan(spool: &Spool, conn_id: u64, steps: &[SyncStep]) -> PlanHash {
        let mut w = spool
            .create(conn_id, &opts(), &compare_opts())
            .await
            .expect("create");
        for s in steps {
            w.push(&PlanItem::Step(s.clone())).await.expect("push");
        }
        w.finish(PlanOutcome::Ended)
            .await
            .expect("finish")
            .plan_hash
    }

    fn spool_files(spool: &Spool) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(spool.dir()) else {
            return Vec::new();
        };
        let mut v: Vec<PathBuf> = rd.map(|e| e.expect("entry").path()).collect();
        v.sort();
        v
    }

    /// Envejece el mtime de un fichero. `std::fs::FileTimes` en vez de una
    /// dependencia nueva.
    fn age(path: &Path, ms: u64) {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("abrir para envejecer");
        let when = SystemTime::now() - Duration::from_millis(ms);
        f.set_times(std::fs::FileTimes::new().set_modified(when))
            .expect("set_times");
    }

    // --------------------------------------------------------------- tests

    #[tokio::test]
    async fn a_written_plan_reads_back_step_for_step() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;

        let reader = spool.open(1, &hash).await.expect("open");
        assert_eq!(reader.header().options, opts());
        assert_eq!(reader.header().compare, compare_opts());
        assert_eq!(reader.summary().counts.copy, 1);
        assert_eq!(reader.summary().counts.overwrite, 1);
        assert_eq!(reader.summary().counts.create_dir, 1);
        assert_eq!(reader.summary().counts.skip, 1);
        assert_eq!(reader.summary().counts.irreversible, 1);
        assert!(reader.summary().executable);

        let read: Vec<SyncStep> = reader.steps().try_collect().await.expect("steps");
        assert_eq!(
            read,
            steps_fixture(),
            "byte a byte, el nombre hostil incluido"
        );
    }

    #[tokio::test]
    async fn another_connection_cannot_open_it() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        assert!(
            matches!(spool.open(2, &hash).await, Err(SpoolError::NotFound)),
            "nadie aplica un plan que no produjo, ni conociendo su hash"
        );
        assert!(spool.open(1, &hash).await.is_ok(), "el dueño sí");
    }

    #[tokio::test]
    async fn a_spool_renamed_into_another_connection_still_does_not_open() {
        // Dos barreras, y la de memoria salta primero: este proceso no emitió
        // ningún plan para la conexión 2, así que ni se mira el disco.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        std::fs::rename(
            spool.dir().join(file_name(1, &hash)),
            spool.dir().join(file_name(2, &hash)),
        )
        .expect("rename");
        let e = spool.open(2, &hash).await.expect_err("rehusado");
        assert!(matches!(e, SpoolError::NotFound));
        assert!(e.is_stale());
    }

    #[test]
    fn the_second_barrier_is_the_file_itself_saying_another_connection() {
        // Y si la de memoria no estuviera: la cabecera lleva el `conn_id` y el
        // terminador el hash, y `open` los compara con los del NOMBRE.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let hash = rt.block_on(write_plan(&spool, 1, &steps_fixture()));
        std::fs::rename(
            spool.dir().join(file_name(1, &hash)),
            spool.dir().join(file_name(2, &hash)),
        )
        .expect("rename");
        let e =
            open_blocking(&spool.dir().join(file_name(2, &hash)), 2, &hash).expect_err("rehusado");
        assert!(matches!(e, SpoolError::Malformed(_)), "{e:?}");
    }

    #[tokio::test]
    async fn a_plan_this_process_did_not_issue_cannot_be_opened() {
        // La forja: `PlanHasher` no lleva clave, así que quien pueda escribir en
        // el directorio puede escribir un plan Y calcular su digest. Lo que lo
        // impide es que este proceso no lo emitió.
        let dir = tempfile::tempdir().expect("tmp");
        let escritor = Spool::new(dir.path());
        let hash = write_plan(&escritor, 1, &steps_fixture()).await;
        assert!(
            escritor.open(1, &hash).await.is_ok(),
            "el que lo emitió, sí"
        );

        // Otro proceso (otro `Spool`) sobre el MISMO directorio: el fichero está
        // ahí, con su nombre correcto y su digest correcto.
        let ajeno = Spool::new(dir.path());
        assert!(
            spool_files(&ajeno).len() == 1,
            "el fichero sigue en el disco"
        );
        assert!(matches!(
            ajeno.open(1, &hash).await,
            Err(SpoolError::NotFound)
        ));
    }

    #[tokio::test]
    async fn a_plan_can_only_be_applied_once() {
        // Dos `sync.apply` del mismo hash ejecutarían el plan dos veces contra
        // el mismo destino, con dos lotes del journal.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        assert!(spool.open(1, &hash).await.is_ok());
        assert!(
            matches!(spool.open(1, &hash).await, Err(SpoolError::NotFound)),
            "el derecho a aplicar se consume al abrir"
        );
    }

    #[tokio::test]
    async fn a_tampered_step_is_caught_by_the_recomputed_digest() {
        // El resumen dice un hash, pero eso es el fichero hablando de sí mismo.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let text = std::fs::read_to_string(&path).expect("leer");
        // Una copia pasa a ser una sobrescritura: mismo tamaño de fichero,
        // resumen intacto, otra cosa completamente distinta sobre el destino.
        let tocado = text.replacen("\"kind\":\"copy\"", "\"kind\":\"overwrite\"", 1);
        assert_ne!(tocado, text, "había un paso que tocar");
        std::fs::write(&path, tocado).expect("escribir");

        let e = spool.open(1, &hash).await.expect_err("rehusado");
        assert!(matches!(e, SpoolError::Malformed(_)), "{e:?}");
        assert!(e.is_stale());
    }

    #[tokio::test]
    async fn a_step_of_an_unknown_kind_on_disk_is_refused_not_degraded() {
        // En el wire, una clase desconocida degrada para no matar un lote de
        // 256. En un fichero que escribimos nosotros hace minutos no hay
        // compatibilidad que defender: solo puede ser corrupción.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let text = std::fs::read_to_string(&path).expect("leer");
        std::fs::write(
            &path,
            text.replacen("\"kind\":\"copy\"", "\"kind\":\"teleport\"", 1),
        )
        .expect("escribir");
        assert!(matches!(
            spool.open(1, &hash).await,
            Err(SpoolError::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn records_after_the_terminator_are_refused() {
        // Con dos terminadores, `open` valida el ÚLTIMO y `steps()` pararía en
        // el primero: el resumen aprobado y el plan ejecutado serían dos cosas.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let mut text = std::fs::read_to_string(&path).expect("leer");
        let terminador = text.lines().last().expect("terminador").to_owned();
        text.push_str(&terminador);
        text.push('\n');
        std::fs::write(&path, text).expect("escribir");

        // Y se rehúsa al ABRIR, no a mitad de la ejecución: con dos
        // terminadores iguales el digest cuadraría, así que la comprobación no
        // puede ser solo la del digest.
        let e = spool.open(1, &hash).await.expect_err("rehusado");
        assert!(matches!(e, SpoolError::Malformed(_)), "{e:?}");
        assert!(e.is_stale());
    }

    #[tokio::test]
    async fn a_second_different_terminator_cannot_swap_the_summary() {
        // El caso con dientes: `open` valida el ÚLTIMO terminador y `steps()`
        // pararía en el primero. El digest recalculado lo caza porque solo cubre
        // los pasos de antes del primero.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let otro = write_plan(&spool, 2, &[copy_step(9, "otro.txt", 1)]).await;
        let path = spool.dir().join(file_name(1, &hash));

        let mio = std::fs::read_to_string(&path).expect("leer");
        let ajeno = std::fs::read_to_string(spool.dir().join(file_name(2, &otro))).expect("leer");
        let cabeza: Vec<&str> = ajeno.lines().collect();
        let mut cosido: Vec<&str> = mio.lines().collect();
        cosido.push(cabeza.last().expect("terminador ajeno"));
        std::fs::write(&path, cosido.join("\n") + "\n").expect("escribir");

        // El nombre sigue siendo el del plan aprobado, pero la última línea ya
        // no lo es: ni el hash del nombre cuadra con el terminador nuevo.
        let e = spool.open(1, &hash).await.expect_err("rehusado");
        assert!(matches!(e, SpoolError::Malformed(_)), "{e:?}");
    }

    #[tokio::test]
    async fn a_spool_of_another_format_version_is_stale() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let text = std::fs::read_to_string(&path).expect("leer");
        std::fs::write(&path, text.replacen("\"format\":1", "\"format\":2", 1)).expect("escribir");
        let e = spool.open(1, &hash).await.expect_err("rehusado");
        assert!(matches!(e, SpoolError::Malformed(_)), "{e:?}");
        assert!(e.is_stale(), "otra versión del formato es un plan rancio");
    }

    #[tokio::test]
    async fn a_header_in_the_middle_is_refused() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let text = std::fs::read_to_string(&path).expect("leer");
        let cabecera = text.lines().next().expect("cabecera").to_owned();
        let mut lineas: Vec<&str> = text.lines().collect();
        lineas.insert(2, &cabecera);
        std::fs::write(&path, lineas.join("\n") + "\n").expect("escribir");
        assert!(matches!(
            spool.open(1, &hash).await,
            Err(SpoolError::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn an_interrupted_plan_leaves_nothing_and_yields_no_hash() {
        // El bucle que se lleva un `Some(Err(_))` por delante cerraría un plan
        // a un tercio con un hash perfectamente válido. Por eso `finish` exige
        // decir cómo terminó el flujo.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        for s in steps_fixture() {
            w.push(&PlanItem::Step(s)).await.expect("push");
        }
        assert!(matches!(
            w.finish(PlanOutcome::Interrupted).await,
            Err(SpoolError::Interrupted)
        ));
        assert!(spool_files(&spool).is_empty(), "ni el .part");
    }

    #[tokio::test]
    async fn dropping_a_writer_inside_a_runtime_removes_its_part() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        {
            let mut w = spool
                .create(1, &opts(), &compare_opts())
                .await
                .expect("create");
            w.push(&PlanItem::Step(copy_step(1, "a.txt", 1)))
                .await
                .expect("push");
        }
        assert!(spool_files(&spool).is_empty(), "el Drop se lo lleva");
    }

    #[test]
    fn dropping_a_writer_outside_a_runtime_also_removes_its_part() {
        // El `Drop` borra SÍNCRONAMENTE a propósito: `Handle::spawn_blocking`
        // entra en pánico si el runtime se está apagando, y un pánico en un
        // `Drop` durante un desenrollado aborta el proceso.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let w = rt
            .block_on(spool.create(1, &opts(), &compare_opts()))
            .expect("create");
        assert_eq!(spool_files(&spool).len(), 1, "el .part está");
        drop(rt); // el runtime se va ANTES que el writer
        drop(w);
        assert!(spool_files(&spool).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_sweep_that_cannot_delete_says_so_instead_of_counting_zero() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        write_plan(&spool, 1, &steps_fixture()).await;
        // Un directorio sin permiso de escritura: el fichero está y no se deja
        // desenlazar.
        std::fs::set_permissions(spool.dir(), std::fs::Permissions::from_mode(0o500))
            .expect("chmod");
        let report = spool.sweep().await.expect("sweep");
        std::fs::set_permissions(spool.dir(), std::fs::Permissions::from_mode(0o700))
            .expect("rechmod");
        assert_eq!(
            (report.removed, report.failed),
            (0, 1),
            "un contador de borrados a secas habría dicho 0 y parecido limpio"
        );
        assert!(!report.is_clean());
    }

    // ------------------------------------------------- la lectura hacia atrás

    /// Escribe `lineas` en un fichero temporal y devuelve su última línea según
    /// [`read_last_line`].
    fn last_line_of(lineas: &[String]) -> Result<Vec<u8>, SpoolError> {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("f");
        let mut cuerpo = String::new();
        for l in lineas {
            cuerpo.push_str(l);
            cuerpo.push('\n');
        }
        std::fs::write(&path, &cuerpo).expect("escribir");
        let mut f = std::fs::File::open(&path).expect("abrir");
        let len = f.metadata().expect("meta").len();
        read_last_line(&mut f, len)
    }

    #[test]
    fn the_backwards_read_finds_the_last_line_whatever_its_size() {
        // La ventana empieza en 8 KiB y se dobla. Estos casos la obligan a
        // doblar una vez, dos, y ninguna.
        for tam in [1usize, 4 * 1024, 12 * 1024, 20 * 1024] {
            let ultima = "z".repeat(tam);
            let leidas = last_line_of(&["a".to_owned(), "bb".to_owned(), ultima.clone()])
                .expect("última línea");
            assert_eq!(leidas, ultima.as_bytes(), "con una última línea de {tam} B");
        }
    }

    #[test]
    fn the_backwards_read_handles_a_single_line_and_a_boundary() {
        // Un fichero de una sola línea: no hay salto anterior que encontrar.
        let sola = "solo".to_owned();
        assert_eq!(
            last_line_of(std::slice::from_ref(&sola)).expect("línea"),
            sola.as_bytes()
        );
        // Y una última línea que empieza JUSTO en el borde de la primera
        // ventana: 8 KiB de contenido previo + su salto.
        let previa = "p".repeat(8 * 1024 - 1);
        let ultima = "u".repeat(16);
        assert_eq!(
            last_line_of(&[previa, ultima.clone()]).expect("línea"),
            ultima.as_bytes()
        );
    }

    #[test]
    fn a_last_record_over_the_cap_is_malformed_not_an_oom() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("f");
        let mut cuerpo = vec![b'a', b'\n'];
        cuerpo.extend(std::iter::repeat_n(b'z', SPOOL_MAX_RECORD + 8));
        cuerpo.push(b'\n');
        std::fs::write(&path, &cuerpo).expect("escribir");
        let mut f = std::fs::File::open(&path).expect("abrir");
        let len = f.metadata().expect("meta").len();
        assert!(matches!(
            read_last_line(&mut f, len),
            Err(SpoolError::Malformed(_))
        ));
    }

    #[test]
    fn a_record_the_reader_could_never_accept_fails_when_it_is_written() {
        // Un terminador con bloqueos de `rel` enormes se pasa del tope. Si se
        // escribiera, `finish` daría un hash para un plan que ningún `open`
        // podría volver a leer: rancio para siempre, sin recuperación.
        let enorme: Vec<SyncBlocker> = (0..SYNC_MAX_BLOCKERS_REPORTED)
            .map(|i| SyncBlocker {
                rel: rel(&format!("{}{i}", "x".repeat(60_000))),
                kind: SyncBlockerKind::TypeMismatchDir,
                side: Some(Side::Right),
            })
            .collect();
        let gordo = Record::End(SpoolSummary {
            plan_hash: PlanHash::parse(&"a".repeat(64)).expect("hash"),
            counts: SyncCounts::default(),
            blockers: enorme,
            blockers_total: SYNC_MAX_BLOCKERS_REPORTED as u64,
            executable: false,
        });
        assert!(matches!(encode(&gordo), Err(SpoolError::Malformed(_))));
    }

    #[tokio::test]
    async fn an_unfinished_spool_cannot_be_opened() {
        // Un daemon que se muere a mitad no deja nada que parezca aprobable.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hasher = PlanHasher::new(&opts(), &compare_opts());
        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        let step = copy_step(1, "a.txt", 1);
        w.push(&PlanItem::Step(step.clone())).await.expect("push");
        std::mem::forget(w); // como un `kill -9`: ni `finish` ni `Drop`.

        // El hash que ese plan HABRÍA tenido: ni con él se abre.
        let mut h = hasher;
        h.step(&step);
        let hash = h.finish();
        assert!(matches!(
            spool.open(1, &hash).await,
            Err(SpoolError::NotFound)
        ));
    }

    #[tokio::test]
    async fn abandoning_a_writer_leaves_nothing_behind() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        w.push(&PlanItem::Step(copy_step(1, "a.txt", 1)))
            .await
            .expect("push");
        w.abandon().await;
        assert!(spool_files(&spool).is_empty(), "ni siquiera el .part");
    }

    #[tokio::test]
    async fn a_plan_past_its_ttl_is_gone() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        age(&spool.dir().join(file_name(1, &hash)), SYNC_PLAN_TTL_MS + 1);

        assert!(matches!(
            spool.open(1, &hash).await,
            Err(SpoolError::Expired)
        ));
        assert!(
            spool_files(&spool).is_empty(),
            "open borra el caducado según lo encuentra"
        );
    }

    #[tokio::test]
    async fn a_plan_just_inside_its_ttl_still_opens() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        age(&spool.dir().join(file_name(1, &hash)), SYNC_PLAN_TTL_MS / 2);
        assert!(spool.open(1, &hash).await.is_ok());
    }

    #[tokio::test]
    async fn closing_a_connection_drops_its_plans_and_only_its_plans() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let a = write_plan(&spool, 1, &steps_fixture()).await;
        let b = write_plan(&spool, 2, &[copy_step(1, "otro.txt", 3)]).await;
        // Y un `.part` de la misma conexión, que también es suyo.
        let _abierto = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");

        let report = spool.drop_connection(1).await.expect("drop");
        assert_eq!((report.removed, report.failed), (2, 0));
        assert!(matches!(spool.open(1, &a).await, Err(SpoolError::NotFound)));
        assert!(spool.open(2, &b).await.is_ok());
    }

    #[tokio::test]
    async fn dropping_connection_1_does_not_touch_connection_12() {
        // `"1-"` no es prefijo de `"12-…"`, y conviene que siga sin serlo.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let uno = write_plan(&spool, 1, &steps_fixture()).await;
        let doce = write_plan(&spool, 12, &steps_fixture()).await;
        assert_eq!(spool.drop_connection(1).await.expect("drop").removed, 1);
        assert!(matches!(
            spool.open(1, &uno).await,
            Err(SpoolError::NotFound)
        ));
        assert!(spool.open(12, &doce).await.is_ok());
    }

    #[tokio::test]
    async fn the_startup_sweep_collects_everything_a_crash_left_behind() {
        // Fresco o caducado da igual: al arrancar no hay ninguna conexión viva,
        // así que TODO spool que exista es de una conexión muerta — y los
        // `conn_id` vuelven a empezar por cero, así que dejar uno fresco sería
        // dejarlo a nombre de un id que el daemon está a punto de repartir.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let viejo = write_plan(&spool, 1, &steps_fixture()).await;
        let fresco = write_plan(&spool, 2, &steps_fixture()).await;
        age(
            &spool.dir().join(file_name(1, &viejo)),
            SYNC_PLAN_TTL_MS + 1,
        );

        let report = spool.sweep().await.expect("sweep");
        assert_eq!((report.removed, report.failed), (2, 0));
        assert!(report.is_clean());
        assert!(matches!(
            spool.open(2, &fresco).await,
            Err(SpoolError::NotFound)
        ));
        assert!(spool_files(&spool).is_empty());
    }

    #[tokio::test]
    async fn sweeping_a_directory_that_was_never_created_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        assert_eq!(spool.sweep().await.expect("sweep"), SweepReport::default());
        assert_eq!(
            spool.drop_connection(7).await.expect("drop"),
            SweepReport::default()
        );
    }

    #[tokio::test]
    async fn removing_an_applied_plan_is_idempotent() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        spool.remove(1, &hash).await.expect("remove");
        spool.remove(1, &hash).await.expect("remove otra vez");
        assert!(matches!(
            spool.open(1, &hash).await,
            Err(SpoolError::NotFound)
        ));
    }

    #[tokio::test]
    async fn a_plan_bigger_than_the_write_buffer_round_trips() {
        // Cruza el flush de 64 KiB en escritura y el trozo de 64 KiB en lectura
        // varias veces: es donde un plan de medio millón de pasos vive.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let steps: Vec<SyncStep> = (0..4_000)
            .map(|i| copy_step(i, &format!("dir{}/f{i}.bin", i % 7), i))
            .collect();
        let hash = write_plan(&spool, 1, &steps).await;
        let reader = spool.open(1, &hash).await.expect("open");
        assert_eq!(reader.summary().counts.copy, 4_000);
        let read: Vec<SyncStep> = reader.steps().try_collect().await.expect("steps");
        assert_eq!(read, steps);
    }

    #[tokio::test]
    async fn the_steps_stream_is_fused() {
        use futures::StreamExt as _;
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &[copy_step(1, "a", 1)]).await;
        let mut s = Box::pin(spool.open(1, &hash).await.expect("open").steps());
        assert!(s.next().await.is_some());
        assert!(s.next().await.is_none());
        assert!(s.next().await.is_none(), "un select! con tick es legal");
    }

    #[tokio::test]
    async fn blockers_make_the_plan_not_executable_and_the_list_is_capped() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        let total = SYNC_MAX_BLOCKERS_REPORTED + 10;
        for i in 0..total {
            w.push(&PlanItem::Blocker(SyncBlocker {
                rel: rel(&format!("x{i}")),
                kind: SyncBlockerKind::TypeMismatchDir,
                side: Some(Side::Right),
            }))
            .await
            .expect("push");
        }
        let summary = w.finish(PlanOutcome::Ended).await.expect("finish");
        assert!(!summary.executable);
        assert_eq!(summary.blockers.len(), SYNC_MAX_BLOCKERS_REPORTED);
        assert_eq!(summary.blockers_total, total as u64);

        let reader = spool.open(1, &summary.plan_hash).await.expect("open");
        assert_eq!(
            reader.summary(),
            &summary,
            "lo que se lee es lo que se cerró"
        );
        assert!(
            reader
                .steps()
                .try_collect::<Vec<_>>()
                .await
                .expect("steps")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn the_stored_hash_is_the_hash_of_the_items_that_were_spooled() {
        // El embudo es uno solo: no se puede hashear una secuencia y guardar
        // otra. Aquí se comprueba contra el hasher desnudo.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let items: Vec<PlanItem> = steps_fixture().into_iter().map(PlanItem::Step).collect();

        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        for i in &items {
            w.push(i).await.expect("push");
        }
        let summary = w.finish(PlanOutcome::Ended).await.expect("finish");

        let mut h = PlanHasher::new(&opts(), &compare_opts());
        for i in &items {
            h.item(i);
        }
        assert_eq!(summary.plan_hash, h.finish());
    }

    #[tokio::test]
    async fn a_plan_hashed_with_other_compare_options_is_another_plan() {
        // Lo que la tarea 6 dejó dicho: `hash` encendido no es lo mismo que
        // solo tamaño aunque los pasos salgan iguales. Por eso las opciones de
        // comparación van en la cabecera y en la semilla del hash.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let a = write_plan(&spool, 1, &steps_fixture()).await;

        let mut otras = compare_opts();
        otras.mtime_tolerance_ms = 5_000;
        let mut w = spool.create(1, &opts(), &otras).await.expect("create");
        for s in steps_fixture() {
            w.push(&PlanItem::Step(s)).await.expect("push");
        }
        let b = w
            .finish(PlanOutcome::Ended)
            .await
            .expect("finish")
            .plan_hash;
        assert_ne!(a, b, "mismos pasos, otra pregunta, otro plan");
    }

    #[tokio::test]
    async fn a_spool_from_a_binary_that_did_not_know_a_counter_is_stale_not_fatal() {
        // Los contadores nuevos NO llevan `serde(default)` a propósito: un cero
        // silencioso convertiría «340 ficheros sin medir» en «ninguno». La
        // respuesta correcta es «plan rancio», no una Task muerta.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let text = std::fs::read_to_string(&path).expect("leer");
        let mutilado = text.replace(",\"unmeasured_steps\":0", "");
        assert_ne!(mutilado, text, "el contador estaba donde se cree");
        std::fs::write(&path, mutilado).expect("escribir");

        let e = spool.open(1, &hash).await.expect_err("no se lee");
        assert!(matches!(e, SpoolError::Malformed(_)));
        assert!(e.is_stale(), "se responde PlanStale, no un fallo interno");
    }

    #[tokio::test]
    async fn a_spool_whose_terminator_was_cut_off_is_not_approvable() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let text = std::fs::read_to_string(&path).expect("leer");
        let mut sin_final = String::new();
        for l in text.lines().take(text.lines().count() - 1) {
            sin_final.push_str(l);
            sin_final.push('\n');
        }
        std::fs::write(&path, sin_final).expect("escribir");

        let e = spool.open(1, &hash).await.expect_err("no se lee");
        assert!(e.is_stale());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_spool_directory_and_its_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;

        let mode = |p: &Path| std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode(spool.dir()), 0o700);
        assert_eq!(mode(&spool.dir().join(file_name(1, &hash))), 0o600);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_spool_directory_that_was_left_world_readable_is_tightened() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        std::fs::create_dir_all(spool.dir()).expect("mkdir");
        std::fs::set_permissions(spool.dir(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");

        write_plan(&spool, 1, &steps_fixture()).await;
        assert_eq!(
            std::fs::metadata(spool.dir())
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "se aprieta ANTES de crear el primer fichero dentro"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_spool_directory_that_is_a_symlink_is_refused() {
        let dir = tempfile::tempdir().expect("tmp");
        let otro = tempfile::tempdir().expect("tmp2");
        let spool = Spool::new(dir.path());
        std::os::unix::fs::symlink(otro.path(), spool.dir()).expect("symlink");
        assert!(spool.create(1, &opts(), &compare_opts()).await.is_err());
    }

    /// El único test que no puede ser una tautología: hace pasar una
    /// comparación DE VERDAD por el planificador y por el spool, y busca en los
    /// bytes del fichero el contenido de los ficheros comparados.
    #[tokio::test]
    async fn a_spool_holds_no_content_only_paths_and_verdicts() {
        use futures::StreamExt as _;
        use norte_compare::{CompareOptions, compare};
        use norte_vfs::Provider as _;
        use tokio_util::sync::CancellationToken;

        const SECRETO: &[u8] = b"clave-de-la-caja-fuerte-4815162342";

        async fn seed(mem: &norte_testkit::MemProvider, name: &[u8], content: &[u8]) {
            let at = norte_testkit::MemProvider::root()
                .join(norte_proto::Segment::new(name).expect("segmento"));
            let mut sink = mem.write(&at).await.expect("write");
            sink.write(bytes::Bytes::copy_from_slice(content))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }

        let origen = norte_testkit::MemProvider::new();
        let destino = norte_testkit::MemProvider::new();
        seed(&origen, b"secretos.txt", SECRETO).await;
        seed(&destino, b"secretos.txt", b"otra cosa distinta").await;
        seed(&origen, b"solo-aqui.txt", SECRETO).await;

        let raiz = norte_testkit::MemProvider::root();
        let rows = compare(
            &origen,
            &raiz,
            &destino,
            &raiz,
            CompareOptions::cheap(),
            CancellationToken::new(),
        );
        let plan_opts = SyncOptions {
            source_root: raiz.clone(),
            dest_root: raiz,
            ..opts()
        };

        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let mut w = spool
            .create(1, &plan_opts, &compare_opts())
            .await
            .expect("create");
        let mut items = Box::pin(norte_sync::plan(rows, plan_opts, CancellationToken::new()));
        while let Some(item) = items.next().await {
            w.push(&item.expect("item")).await.expect("push");
        }
        let summary = w.finish(PlanOutcome::Ended).await.expect("finish");
        assert!(
            summary.counts.copy + summary.counts.overwrite >= 2,
            "hubo plan"
        );

        let bytes = std::fs::read(spool.dir().join(file_name(1, &summary.plan_hash)))
            .expect("leer el spool");
        assert!(
            !bytes.windows(SECRETO.len()).any(|w| w == SECRETO),
            "un fichero que autoriza escrituras no puede ser además el dato"
        );
        // Y la otra mitad, que es la que impide que esto sea una tautología: lo
        // que SÍ tiene que estar, está — o sea, la búsqueda de arriba habría
        // encontrado el secreto si hubiera estado.
        assert!(
            bytes
                .windows(b"solo-aqui.txt".len())
                .any(|w| w == b"solo-aqui.txt"),
            "las rutas sí viajan: el buscador de arriba funciona"
        );
    }

    // ------------------------------------- lo que NO se deja cerrar (tarea 8)

    #[tokio::test]
    async fn un_plan_cuya_conexion_se_cerro_no_llega_a_ser_aprobable() {
        // El caso NO necesita ninguna carrera: un plan sobre dos árboles
        // idénticos no emite un solo paso, así que jamás toca su canal y jamás
        // se entera de que su dueño se fue. Si `finish` lo cerrase igual,
        // quedaría un plan retenido después de la única muerte que le tocaba.
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::new(dir.path());
        let w = spool
            .create(7, &opts(), &compare_opts())
            .await
            .expect("create");

        spool.drop_connection(7).await.expect("drop");

        let e = w
            .finish(PlanOutcome::Ended)
            .await
            .expect_err("su dueño ya no está");
        assert!(matches!(e, SpoolError::Interrupted), "fue {e:?}");
        assert!(
            spool_files(&spool).is_empty(),
            "no puede quedar nada, ni `.part` ni `.jsonl`"
        );
    }

    #[tokio::test]
    async fn la_lapida_de_una_conexion_muerta_no_sobrevive_a_sus_planes() {
        // `dead` está acotado por los planes EN VUELO, no por el contador de
        // conexiones: en cuanto el último writer de esa conexión se cierra, la
        // marca se va y una conexión con ese id podría volver a planificar.
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::new(dir.path());
        let w = spool
            .create(7, &opts(), &compare_opts())
            .await
            .expect("create");
        spool.drop_connection(7).await.expect("drop");
        assert!(spool.is_dead(7));
        w.abandon().await;
        assert!(
            !spool.is_dead(7),
            "sin planes en vuelo no hay nada que marcar"
        );
    }

    #[tokio::test]
    async fn replanificar_lo_que_se_esta_aplicando_no_vuelve_a_acunar_el_derecho() {
        // Replanificar el mismo árbol con las mismas opciones da el MISMO hash.
        // Sin esto, `finish` volvería a apuntar un derecho que un `open` acababa
        // de consumir: dos ejecuciones del mismo plan contra el mismo destino,
        // con dos lotes del journal y un undo que ya no describe ningún estado
        // por el que se haya pasado.
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let _reader = spool.open(1, &hash).await.expect("se aplica");

        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        for s in steps_fixture() {
            w.push(&PlanItem::Step(s)).await.expect("push");
        }
        let e = w
            .finish(PlanOutcome::Ended)
            .await
            .expect_err("ese plan se está aplicando");
        assert!(matches!(e, SpoolError::Interrupted), "fue {e:?}");
        assert!(!spool.claim_issued(1, &hash), "el derecho no volvió");

        // Y en cuanto la aplicación termina y llama a `remove`, se puede volver
        // a planificar con normalidad.
        spool.remove(1, &hash).await.expect("remove");
        let otra = write_plan(&spool, 1, &steps_fixture()).await;
        assert_eq!(otra, hash);
        assert!(spool.open(1, &otra).await.is_ok());
    }

    #[tokio::test]
    async fn planificar_barre_los_planes_vencidos() {
        // El TTL solo se comprobaba dentro de `open`, y un plan que nadie abre
        // no se abre nunca: sin este barrido, «diez minutos» no era una de las
        // cuatro muertes sino una comprobación que corría cuando ya no hacía
        // falta.
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::new(dir.path());
        let viejo = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &viejo));
        age(&path, SYNC_PLAN_TTL_MS + 60_000);

        // Un plan NUEVO de otra conexión: barre al pasar.
        let mut w = spool
            .create(2, &opts(), &compare_opts())
            .await
            .expect("create");
        assert!(!path.exists(), "el vencido se fue al planificar");
        w.push(&PlanItem::Step(copy_step(1, "a.txt", 1)))
            .await
            .expect("push");
        w.finish(PlanOutcome::Ended).await.expect("finish");
    }

    #[tokio::test]
    async fn un_plan_en_curso_no_lo_barre_su_propia_antiguedad() {
        // Un `.part` es un plan EN CURSO: sobre un árbol de red puede tardar
        // horas legítimamente, y su mtime es el de cuando empezó.
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::new(dir.path());
        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        let part = spool_files(&spool).first().cloned().expect("hay .part");
        age(&part, SYNC_PLAN_TTL_MS + 60_000);

        let w2 = spool
            .create(2, &opts(), &compare_opts())
            .await
            .expect("create");
        assert!(part.exists(), "un plan en curso no es un plan vencido");
        w2.abandon().await;
        w.push(&PlanItem::Step(copy_step(1, "a.txt", 1)))
            .await
            .expect("push");
        w.finish(PlanOutcome::Ended).await.expect("finish");
    }
}
