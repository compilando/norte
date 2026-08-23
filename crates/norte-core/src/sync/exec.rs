//! El ejecutor: un plan aprobado se convierte en escrituras, en UNA unidad
//! deshacible del journal, y en un informe que dice qué pasó con cada paso.
//!
//! `crates/norte-core/src/rename/exec.rs` es el pariente y conviene leerlo
//! antes: una Task para muchos pasos, un `StepJournal` con un lote que comparte
//! `batch_id`, un informe tras un `Mutex` y la cancelación mirada ENTRE pasos.
//! Lo que aquí es distinto —y es lo único importante— es que **no hay
//! rollback**.
//!
//! # Media sincronización es un estado real; media permutación no
//!
//! Y **medio borrado también lo es**: un `DeleteTree` cortado a mitad escribe
//! su entrada por lo que llegó a quitar (#186). La condición que se la saltaba
//! ante `Err(Cancelled)` excluía justo el caso en que la entrada importa —
//! `remove_tree` mira el token ENTRE entradas, así que para cuando devuelve
//! `Cancelled` ya han caído un número arbitrario de nodos, y contra un destino
//! sin papelera han caído para siempre.
//!
//! El lote de renames se desanda entero ante el primer fallo porque un
//! directorio a medio renombrar no es ni el de antes ni el de después. Un árbol
//! a medio sincronizar sí es algo: es el árbol de antes con cuarenta mil
//! ficheros ya actualizados. Desandarlo automáticamente le quitaría al usuario
//! un trabajo que salió bien para dejarlo donde estaba, y encima cada paso
//! desandado es otra escritura que puede fallar. Así que **un paso que falla es
//! una fila del informe y la Task sigue**, y lo aplicado se queda —journalizado
//! bajo su lote, o sea deshacible a mano por quien quiera deshacerlo—.
//!
//! # La revalidación es lo único entre el TTL y un fichero perdido
//! Entre que un humano aprueba un plan y que se aplica pasan hasta
//! [`SYNC_PLAN_TTL_MS`](norte_proto::methods::SYNC_PLAN_TTL_MS) milisegundos.
//! Antes de CADA paso destructivo —un `Overwrite`, un `DeleteTree`— se hace un
//! `stat` y se compara con lo que la comparación vio en su día
//! ([`DestWitness`]); si no cuadra, el paso NO se ejecuta y sale en el informe
//! como [`SyncFailureCause::Conflict`]. Sin eso, aprobar un plan sería firmar un
//! cheque al portador sobre el árbol de destino durante diez minutos.
//!
//! # Regla dura 4: un plan sin journal no se aplica
//! `sync.apply` EXIGE journal ([`Engine::sync_apply_as`](crate::Engine::sync_apply_as)
//! contesta `Unsupported` sin él). El lote de renames tolera no tenerlo porque
//! un rename se deshace mirando el directorio; aquí se sobrescribe y se entierra,
//! y cada paso del plan lleva prometida una [`StepReversal`] que solo el journal
//! puede cumplir.
//!
//! Desde #167 el transporte embebido SÍ tiene el journal del directorio de
//! estado —y desde #177 lo abre aquí mismo, cuando `sync.apply` se lo pide—,
//! así que este `Unsupported` dejó de ser el caso corriente: queda para el
//! engine que de verdad no puede tenerlo (otro proceso con el lock, o un
//! embebedor que construyó `Engine::new()` a mano).
//!
//! # Cómo se journaliza cada clase (tabla normativa de la spec)
//!
//! | paso | entradas | reversa |
//! | --- | --- | --- |
//! | `CreateDir`, `Copy` | `created` | `Delete` |
//! | `Overwrite` con papelera | `trashed` + `created` | `RestoreTrash` + `Delete` |
//! | `Overwrite` sin papelera | `created` | `Irreversible` |
//! | `DeleteTree` con papelera | `trashed` (UNA, por el árbol entero) | `RestoreTrash` |
//! | `DeleteTree` sin papelera | `removed` (UNA, por lo que se llegara a quitar) | `Irreversible` |
//! | `Skip` | ninguna | — |
//!
//! **«Sin papelera» aquí es «sin reversa», y son dos cosas distintas.** Un
//! destino con papelera que no NOMBRA lo que entierra
//! (`Provider::trash_restorable` en `false`) produce un plan cuyos pasos son
//! todos `Irreversible`, así que cae en las filas de «sin papelera» de la tabla
//! — pero el borrado SIGUE yendo a la papelera (`destroy_leaf`/`destroy_tree`
//! miran `delete_mode`, que sale de si el destino tiene papelera). Lo que se
//! pierde es el undo, no la papelera del usuario.
//!
//! El undo recorre `seq` descendente, así que dentro de la pareja de un
//! `Overwrite` borra lo creado ANTES de restaurar lo enterrado. El orden correcto
//! sale del mecanismo que ya había, no de cuidado puesto aquí.
//!
//! **El `Overwrite` sin papelera no lleva una entrada propia por el borrado**, y
//! es deliberado: la única entrada dice `created` con reversa `Irreversible`,
//! que es la verdad entera —«esta ruta tiene contenido nuevo y lo anterior no se
//! puede recuperar»—. Con dos entradas (`removed` irreversible + `created`
//! deshacible) el undo del lote borraría el fichero nuevo sin poder restaurar el
//! viejo, y dejaría la ruta VACÍA donde el usuario tenía algo: peor que el
//! estado que venía a arreglar.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _};
use norte_proto::methods::RelPath;
use norte_proto::methods::{
    DestTrash, SYNC_MAX_FAILURES_REPORTED, StepReversal, SyncFailure, SyncFailureCause,
    SyncReportResult, SyncStep, SyncStepKind,
};
use norte_proto::{ConflictKind, Entry, EntryKind, Error, VPath};
use norte_sync::DestWitness;
use norte_vfs::{Provider, SymlinkKind};

use crate::journal::{Actor, NewEntry, Reversal, SqliteJournal};
use crate::observer::NoopObserver;
use crate::scheduler::TaskCtx;
use crate::sync::spool::SpoolStep;

/// Las dos raíces y sus providers, resueltos una vez.
///
/// Las raíces salen del ENCABEZADO DEL SPOOL, que es el único sitio donde
/// están: `sync.apply` lleva un hash y nada más.
pub(crate) struct SyncTargets {
    /// Provider de la raíz de origen.
    pub source: Arc<dyn Provider>,
    /// Provider de la raíz de destino. Puede ser el mismo objeto.
    pub dest: Arc<dyn Provider>,
    /// De dónde se lee.
    pub source_root: VPath,
    /// Dónde se escribe.
    ///
    /// **Ninguna RUTA sale de aquí**: cada una se compone pegándole a esta raíz
    /// una [`RelPath`], cuyos segmentos no pueden ser `..` ni `.` ni contener
    /// `/` ni NUL — lo impide [`norte_proto::Segment`] al construirse, y la
    /// deserialización lo vuelve a impedir después de decodificar, así que un
    /// `%2E%2E` tampoco cuela.
    ///
    /// **Que los BYTES se queden dentro lo garantiza [`Self::dest_confined`]**,
    /// no esta composición: la resolución la hace el sistema de ficheros, y un
    /// symlink puesto en un componente INTERMEDIO entre aprobar y aplicar
    /// redirigiría la escritura fuera del árbol con las credenciales del daemon
    /// (#164). Los pasos destructivos lo esquivan de rebote —`stat` es un
    /// `lstat`, así que la revalidación ve un `Symlink` donde el testigo decía
    /// `Dir` y contesta conflicto; y `walk` no desciende symlinks—; el `Copy` y
    /// el `CreateDir` se defienden con la raíz confinada, cuando el destino sabe
    /// darla.
    pub dest_root: VPath,
    /// La raíz de destino ABIERTA, cuando este destino sabe confinarse
    /// (ADR 0054).
    ///
    /// Se abre UNA vez por Task, aquí, y a partir de ahí cada paso que crea algo
    /// direcciona segmentos relativos a ella en vez de una ruta: sin ruta que
    /// recomponer no hay ventana entre comprobar y escribir.
    ///
    /// `None` = este destino no sabe (`file://` en Windows, SFTP, un bucket).
    /// Entonces se escribe por ruta, como siempre, y se dice en el log: negarse
    /// dejaría sin sincronizar a los destinos que no pueden dar esa defensa,
    /// que es un precio mucho más alto que el riesgo que evita.
    pub dest_confined: Option<Box<dyn norte_vfs::ConfinedRoot>>,
    /// El gate de policy, consultado paso a paso sobre la ruta REAL.
    ///
    /// El gate de la raíz que `sync.apply` pide antes de empezar resuelve la
    /// frontera de scope de un agente (es por raíz, y `is_under` es transitivo),
    /// pero **no** una regla `deny` de `policy.toml` sobre una ruta de DENTRO
    /// del árbol: esas reglas casan por contención de la ruta consultada, así que
    /// consultando solo la raíz no se ven. Sin esto, un plan `Mirror` con un
    /// origen vacío borraría un subárbol que `fs.delete` rehúsa — o sea, una
    /// autorización de borrado más floja que la que ya existe.
    pub policy: Arc<dyn crate::policy::PolicyGate>,
    /// Qué clase de borrado va a ocurrir de verdad, para preguntarlo por lo que
    /// es. Sale de `dest_has_trash` del plan, que es lo mismo de lo que sale la
    /// reversa de cada paso.
    pub delete_mode: norte_proto::DeleteMode,
}

impl SyncTargets {
    /// Abre la raíz de destino confinada, si este destino sabe darla.
    ///
    /// Se hace DENTRO de la Task, no al construir: aquí hay `task_id` con el
    /// que decir en el log de qué operación se está hablando cuando el destino
    /// no sabe confinarse y hay que degradar.
    /// # Errors
    /// El del `open_root` cuando el destino declaró que sabía confinar y no
    /// pudo. NO se degrada: ver [`crate::ops::open_dest_root`], donde está el
    /// razonamiento de por qué eso sería la llave que reabre #164.
    pub(crate) async fn with_dest_confined(mut self, task_id: u64) -> Result<Self, Error> {
        self.dest_confined =
            crate::ops::open_dest_root(self.dest.as_ref(), &self.dest_root, task_id).await?;
        Ok(self)
    }

    /// El destino de un paso: la ruta real, más el relativo bajo la raíz
    /// confinada si la hay.
    ///
    /// `to` viene de [`dest_path`], que ya lo compuso pegando el relativo del
    /// paso a [`Self::dest_root`], así que el relativo se recupera de ahí y
    /// vuelve a salir el MISMO — y si no cayera dentro (no puede, pero el tipo
    /// no lo sabe), el paso se degrada al camino por ruta en vez de escribir en
    /// un sitio que la raíz no cubre.
    fn dest_at(&self, to: &VPath) -> crate::ops::Dest<'_> {
        match self
            .dest_confined
            .as_deref()
            .zip(crate::ops::rel_under(&self.dest_root, to))
        {
            Some((root, rel)) => {
                crate::ops::Dest::under(self.dest.as_ref(), Some(root), rel, to.clone())
            }
            None => crate::ops::Dest::plain(self.dest.as_ref(), to.clone()),
        }
    }

    /// ¿Autoriza la policy ESTE paso sobre ESTA ruta?
    ///
    /// Un `Deny` es una fila del informe ([`SyncFailureCause::Denied`]), no el
    /// final de la Task: es exactamente lo que un paso que falla significa.
    ///
    /// Un `Ask` también se rehúsa, y no se pregunta. Un modal por paso en un plan
    /// de medio millón no es una interfaz, y el gate de la raíz ya preguntó una
    /// vez por el lote entero; rehusar es el lado seguro del intercambio.
    fn allows(
        &self,
        op: crate::policy::PolicyOp,
        path: &VPath,
        actor: &Actor,
    ) -> Result<(), Error> {
        use crate::policy::{Decision, DenyReason};
        match self.policy.evaluate(actor, op, &[path]) {
            Decision::Allow => Ok(()),
            Decision::Deny(reason) => Err(Error::PolicyDenied {
                rule: reason.rule_id().to_owned(),
            }),
            Decision::Ask => Err(Error::PolicyDenied {
                rule: DenyReason::NotApproved.rule_id().to_owned(),
            }),
        }
    }
}

/// Cómo se registra UN efecto de este lote.
///
/// Dos métodos y no un `Mutation`: [`crate::observer::Mutation`] no lleva
/// `batch_id` más que en el rename, y lo que hace de estas escrituras UNA unidad
/// deshacible es justamente que todas compartan el suyo.
#[async_trait]
pub(crate) trait StepJournal: Send + Sync {
    /// Un nodo que este lote CREÓ. `reversal` es
    /// [`Reversal::Delete`] salvo en la sobrescritura sin papelera, que es
    /// [`Reversal::Irreversible`] (ver la tabla del módulo).
    ///
    /// # Errors
    /// El error del journal. Regla dura 4: un efecto cuya entrada no queda
    /// durable deja el árbol fuera del journal, así que el llamante para la Task
    /// en vez de seguir produciendo más.
    async fn created(&self, path: &VPath, reversal: Reversal) -> Result<(), Error>;

    /// Un nodo que este lote ENTERRÓ en la papelera. `dest` es la ruta
    /// recuperable de una papelera lógica, y es lo que el undo necesita para
    /// saber DE DÓNDE sacarlo.
    ///
    /// # Errors
    /// El error del journal, como en [`StepJournal::created`].
    async fn trashed(&self, path: &VPath, dest: Option<&VPath>) -> Result<(), Error>;

    /// Un nodo que este lote borró PERMANENTEMENTE (un `DeleteTree` sobre un
    /// destino sin papelera). Siempre [`Reversal::Irreversible`].
    ///
    /// # Errors
    /// El error del journal, como en [`StepJournal::created`].
    async fn removed(&self, path: &VPath) -> Result<(), Error>;
}

/// Registra en el journal bajo UN `batch_id`.
pub(crate) struct BatchJournal {
    /// El journal, que es también el observer del engine.
    pub journal: Arc<SqliteJournal>,
    /// Quién lo causó.
    pub actor: Actor,
    /// El id que comparten todas las entradas de esta aplicación: lo que las
    /// hace UNA unidad deshacible ([`crate::journal::Journal::alloc_batch`]).
    pub batch_id: i64,
}

impl BatchJournal {
    /// El insert, con el lote puesto.
    async fn record(
        &self,
        op: &str,
        path: &VPath,
        reversal: Reversal,
        reversal_ref: Option<&VPath>,
    ) -> Result<(), Error> {
        let path = path.to_wire().into_bytes();
        let reference = reversal_ref.map(|p| p.to_wire().into_bytes());
        self.journal
            .journal()
            .record_entry(&NewEntry {
                op,
                path: &path,
                path_to: None,
                reversal,
                reversal_ref: reference.as_deref(),
                actor: &self.actor,
                undoes_seq: None,
                batch_id: Some(self.batch_id),
            })
            .await
            .map_err(|e| {
                tracing::error!(error = %e, op, "sync.apply: fallo al escribir el journal");
                Error::from(e)
            })?;
        Ok(())
    }
}

#[async_trait]
impl StepJournal for BatchJournal {
    async fn created(&self, path: &VPath, reversal: Reversal) -> Result<(), Error> {
        self.record("created", path, reversal, None).await
    }

    async fn trashed(&self, path: &VPath, dest: Option<&VPath>) -> Result<(), Error> {
        self.record("trashed", path, Reversal::RestoreTrash, dest)
            .await
    }

    async fn removed(&self, path: &VPath) -> Result<(), Error> {
        self.record("removed", path, Reversal::Irreversible, None)
            .await
    }
}

/// Un informe recién abierto, con su lote ya puesto.
///
/// La papelera del destino entra AQUÍ, al abrirlo (#170), y no al cerrarlo: sale
/// de las opciones del plan que se está aplicando —las mismas de las que salió
/// el [`DestTrash`] del `sync.plan_done`—, así que el informe no puede acabar
/// diciendo una cosa distinta de la que se aprobó.
#[must_use]
pub(crate) fn new_report(batch_id: i64, dest_trash: DestTrash) -> SyncReportResult {
    SyncReportResult {
        done: 0,
        failed: 0,
        skipped: 0,
        bytes: 0,
        failures: Vec::new(),
        batch_id: Some(batch_id),
        dest_trash,
    }
}

/// Qué le pasó a UN paso.
#[derive(Debug)]
enum Applied {
    /// Se ejecutó, moviendo estos bytes.
    Wrote(u64),
    /// No tocó nada: el plan ya lo traía como [`SyncStepKind::Skip`].
    Skipped,
}

/// Lo que se enterró y NO quedó registrado, con todo lo que hace falta para
/// encontrarlo a mano.
///
/// Existe porque #160 se dio EN VIVO y lo único que quedaba de él era una línea
/// de log: `sync.apply` enterró el destino, la fila de journal no llegó, y la
/// ruta de la papelera —el único sitio donde está el fichero ahora— vivía en un
/// campo de `tracing` que el llamante no puede leer. La regla 6 pide un error
/// tipado, y este es el dato que ese error tiene que llevar.
#[derive(Debug)]
pub(crate) struct Unrecorded {
    /// La ruta que se enterró: lo que el usuario cree que sigue ahí.
    ///
    /// Dos caminos llegan aquí y los dos dejan el mismo hueco: el `trashed`
    /// que no se pudo escribir y tampoco compensar (#160), y el `created` que
    /// falla DESPUÉS de un `trashed` que sí quedó (#206) — ahí el fichero
    /// nuevo está puesto, el viejo está en la papelera, y el lote no se puede
    /// deshacer porque le falta la mitad de su par.
    pub buried: VPath,
    /// Dónde fue a parar, si la papelera del destino NOMBRA lo que se lleva.
    /// `None` con `DestTrash::Opaque` (macOS, Windows), y entonces no hay
    /// adónde apuntar a nadie.
    pub at: Option<VPath>,
    /// El error del journal que empezó todo esto.
    pub source: Error,
}

/// Por qué paró una aplicación de plan.
///
/// Dos variantes y no un [`Error`] a secas porque los dos estados en que puede
/// quedar el árbol son distintos y el llamante tiene que poder distinguirlos:
/// con [`Self::Stopped`] lo aplicado está journalizado y el paso que falló no
/// dejó rastro; con [`Self::Unrecorded`] hay un fichero movido que el journal
/// no conoce.
#[derive(Debug)]
pub(crate) enum ApplyError {
    /// Lo de siempre: la cancelación, el journal, un spool que dejó de leerse.
    /// Lo aplicado hasta aquí está registrado.
    Stopped(Error),
    /// Regla dura 4 rota, y no se pudo compensar (#160). Lleva la ruta
    /// enterrada y su destino en la papelera para que quien lo reciba lo pueda
    /// DECIR — que es lo que un `tracing::error!` no permite.
    Unrecorded(Box<Unrecorded>),
}

impl ApplyError {
    /// La forma que el wire entiende, DESPUÉS de dejar dicho lo que no cabe en
    /// ella.
    ///
    /// El detalle de [`Self::Unrecorded`] no tiene categoría en la taxonomía
    /// —no existe «tu fichero está en la papelera y nadie lo apuntó»— así que
    /// se pierde en la conversión, y por eso esta función lo escribe en el log
    /// del operador antes de perderlo. **Y por eso no hay `From`**: un `?` en
    /// un llamador futuro convertiría en silencio y volvería a dejar el estado
    /// sin decir, que es #160 otra vez.
    pub(crate) fn into_wire(self) -> Error {
        match self {
            Self::Stopped(e) => e,
            Self::Unrecorded(u) => {
                // La única línea que nombra las dos rutas. `bury` ya no la
                // escribe para este caso: dos `error!` por un mismo hecho es
                // ruido, y el que sirve es este, que sale cuando la Task muere.
                tracing::error!(
                    error = %u.source,
                    enterrado = %crate::engine::span_path(&u.buried),
                    en = u.at.as_ref().map(crate::engine::span_path),
                    salida = if u.at.is_some() {
                        "sácalo a mano desde la ruta de `en`"
                    } else {
                        "esta papelera no dice adónde se lo llevó (macOS/Windows): búscalo en la \
                         papelera del sistema"
                    },
                    "sync.apply: PARADO — el destino se enterró, su entrada de journal NO llegó \
                     y devolverlo tampoco funcionó",
                );
                u.source
            }
        }
    }
}

/// Por qué se paró un paso, y si eso para también la Task.
#[derive(Debug)]
enum StepError {
    /// El paso no ocurrió. Fila del informe; la Task sigue.
    Failed(Error),
    /// Nada puede seguir: una cancelación, o el journal.
    Fatal(Error),
    /// Nada puede seguir Y el árbol no es el que era: se enterró algo, su fila
    /// no llegó, y devolverlo tampoco se pudo. Se separa de [`Self::Fatal`]
    /// porque el estado es distinto —ahí el destino quedó intacto, aquí no— y
    /// porque solo esta lleva adónde mirar.
    ///
    /// `Box` porque dos `VPath` y un `Error` hacen de esta variante cuatro
    /// veces el tamaño de las otras dos, y `StepError` es el `Err` de una
    /// función que se llama una vez por paso de un plan de medio millón: el
    /// camino frío no le paga el tamaño al camino de siempre.
    Unrecoverable(Box<Unrecorded>),
}

impl StepError {
    /// El error de un PROVIDER: fila del informe, salvo que sea la cancelación
    /// —que no es un fallo del paso, es el final de la Task—.
    fn from_provider(e: Error) -> Self {
        if matches!(e, Error::Cancelled) {
            StepError::Fatal(e)
        } else {
            StepError::Failed(e)
        }
    }
}

/// La causa que va al informe.
///
/// [`SyncFailureCause::IllegalName`] merece la suya: la legalidad de un nombre
/// bajo la raíz de DESTINO no se valida al planificar (ver el rustdoc de la
/// variante), así que es la familia de fallos que aflora aquí de serie, la que
/// el usuario puede arreglar él solo, y la que se repetirá idéntica en cada
/// intento hasta que la arregle. Un `Io` genérico no le diría nada de eso.
fn cause_of(e: &Error) -> SyncFailureCause {
    match e {
        Error::PermissionDenied | Error::PolicyDenied { .. } => SyncFailureCause::Denied,
        Error::InvalidPath => SyncFailureCause::IllegalName,
        // «Ya no está como el plan lo vio»: el destino cambió, el origen
        // desapareció, o algo ocupa el sitio.
        Error::NotFound | Error::Conflict { .. } => SyncFailureCause::Conflict,
        _ => SyncFailureCause::Io,
    }
}

/// La ruta absoluta de `rel` bajo `root`.
///
/// No puede salirse de `root`: un [`norte_proto::Segment`] no puede ser `..` ni
/// `.` ni llevar `/` ni NUL, y [`RelPath`] los valida al construirse y al
/// deserializar. Es la propiedad en la que se apoya que ningún paso escriba
/// fuera de la raíz de destino, y por eso se compone así y jamás concatenando
/// cadenas.
fn under(root: &VPath, rel: &RelPath) -> VPath {
    let mut path = root.clone();
    for segment in rel.segments() {
        path = path.join(segment.clone());
    }
    path
}

/// Dónde cae este paso EN EL DESTINO.
///
/// `dest_root + dest_rel.unwrap_or(rel)`, que es la regla normativa de
/// [`SyncStep::dest_rel`](norte_proto::methods::SyncStep::dest_rel): se escribe
/// sobre el fichero que EXISTE, no sobre el que el origen deletrea. Un `café`
/// NFC del origen contra un `café` NFD del destino son la misma pareja, y pegar
/// la ortografía del origen sobre ext4 crearía un SEGUNDO fichero al lado del
/// que se quería sobrescribir — con una promesa de `RestoreTrash` sobre algo que
/// nadie enterró.
fn dest_path(targets: &SyncTargets, record: &SpoolStep) -> VPath {
    let rel = record.step.dest_rel.as_ref().unwrap_or(&record.step.rel);
    under(&targets.dest_root, rel)
}

/// `stat` con reintentos, distinguiendo «no está» de «no se pudo mirar».
async fn stat(
    provider: &dyn Provider,
    path: &VPath,
    ctx: &TaskCtx,
) -> Result<Option<Entry>, Error> {
    use futures::FutureExt as _;
    match crate::ops::with_retry(&ctx.cancel, || provider.stat(path).boxed()).await {
        Ok(entry) => Ok(Some(entry)),
        Err(Error::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

/// ¿Sigue el destino como el plan lo vio?
///
/// El `stat` de la spec, y lo único que hay entre el TTL del plan y un fichero
/// perdido. Se compara contra el [`DestWitness`] que la comparación anotó:
///
/// - **No está** → conflicto. El paso se aprobó sobre algo que ya no existe.
/// - **Cambió de clase** → conflicto, siempre.
/// - **Cambió de tamaño o de fecha** → conflicto. Solo se compara lo que las DOS
///   fotos traen: un provider que lista sin tamaño (`file://` es uno) deja el
///   testigo a medias, y declarar un conflicto por eso rechazaría el plan entero
///   en el sistema de ficheros más común.
///
/// # Un `DeleteTree` mira también CUÁNTAS cosas hay dentro (#176)
/// El `stat` de un directorio solo se mueve cuando cambian sus hijos DIRECTOS,
/// así que un subárbol que ganó cien ficheros dos niveles más abajo entre
/// aprobar y aplicar revalidaba limpio y se borraba entero: el paso con más
/// radio de acción con la comprobación más floja.
///
/// Ahora el testigo de un borrado de árbol lleva el RECUENTO de su primer
/// nivel y aquí se vuelve a contar. Lo pone el cableado del core al
/// planificar, no el transductor —que es puro y no tiene provider—, y por eso
/// cuesta un listado por paso destructivo al planificar y otro al aplicar,
/// sobre el paso que iba a listarlo entero de todas formas.
///
/// **Lo que sigue sin ver**: un cambio en un NIETO. Un fichero añadido tres
/// niveles más abajo no mueve ni el `stat` del directorio ni la cuenta de su
/// primer nivel. Cerrar eso pide un recorrido de revalidación, que es
/// exactamente el coste que el diseño de «un huérfano es UNA entrada de
/// journal» evita. La frase de aprobación lo dice en las cuatro variantes de
/// borrado: un árbol se vuelve a comprobar por fuera, no hoja por hoja.
///
/// Y con un testigo sin tamaño ni fecha esto se queda en «sigue
/// existiendo y sigue siendo de la misma clase». Es menos de lo que la spec
/// promete y es lo que el plan puede saber: la pareja de un `Overwrite` sí viene
/// hidratada (la cascada necesita tamaño y fecha para decidir), así que el caso
/// que de verdad importa está cubierto. Y una fecha con resolución de segundo
/// deja una ventana de un segundo en la que un cambio del mismo tamaño pasa
/// desapercibido.
async fn revalidate(
    provider: &dyn Provider,
    path: &VPath,
    witness: Option<DestWitness>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    // Sin foto no se destruye. El transductor la pone SIEMPRE en las dos clases
    // que llaman aquí, así que una que falte no es un provider parco: es un
    // fichero de spool que este binario no escribió como lo escribe. Y como el
    // testigo NO entra en el `plan_hash` —es de dónde salió la conclusión, no la
    // conclusión—, borrarlo es justo la edición que el digest no ve; exigirlo es
    // lo que la vuelve inútil.
    let Some(before) = witness else {
        tracing::error!("sync.apply: un paso destructivo sin testigo del destino");
        return Err(Error::Conflict {
            conflict: ConflictKind::Unknown,
        });
    };
    let Some(now) = stat(provider, path, ctx).await? else {
        return Err(Error::NotFound);
    };
    if now.kind != before.kind {
        return Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        });
    }
    let size_moved = matches!((before.size, now.size), (Some(a), Some(b)) if a != b);
    let mtime_moved = matches!((before.mtime_ms, now.mtime_ms), (Some(a), Some(b)) if a != b);
    if size_moved || mtime_moved {
        return Err(Error::Conflict {
            conflict: ConflictKind::Exists,
        });
    }
    // Y el recuento del primer nivel, cuando el plan lo anotó (#176): el
    // `stat` de un directorio solo se mueve cuando cambian sus hijos DIRECTOS,
    // así que sin esto un subárbol que ganó ficheros entre aprobar y aplicar
    // revalidaba limpio. Solo se compara si las DOS fotos lo traen, igual que
    // el tamaño y la fecha — un `None` es «no se sabe», jamás «cero».
    if let Some(antes) = before.entries {
        let ahora = contar_primer_nivel(provider, path, ctx).await;
        if ahora != Some(antes) {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
    }
    Ok(())
}

/// Cuántas entradas tiene el primer nivel de `path`, o `None` si no se pudo
/// contar barato (#176).
///
/// Mismo tope que al planificar: por encima el plan tampoco anotó nada, así
/// que las dos fotos coinciden en no saber.
async fn contar_primer_nivel(provider: &dyn Provider, path: &VPath, ctx: &TaskCtx) -> Option<u64> {
    use futures::StreamExt as _;

    let mut stream = provider.list(path).await.ok()?;
    let mut n = 0_u64;
    loop {
        if ctx.cancel.is_cancelled() {
            return None;
        }
        match stream.next().await {
            Some(Ok(_)) => {
                n += 1;
                if n > crate::sync::CONTEO_MAX {
                    return None;
                }
            }
            Some(Err(_)) => return None,
            None => return Some(n),
        }
    }
}

/// Copia UNA hoja del origen al destino y devuelve los bytes que movió.
///
/// El destino tiene que estar LIBRE: el `write` de un provider es create-new, así
/// que un destino ocupado sale como `Conflict` en vez de pisar nada por
/// sorpresa. Quien sobrescribe ya lo ha vaciado antes, a conciencia y con su
/// entrada de journal.
///
/// **La entrada de journal NO la pone `ops`**, y por eso se le da un observer
/// que no hace nada: `Mutation::Created` no lleva `batch_id`, así que la copia
/// quedaría fuera del lote y el undo no la vería. La pone el llamante, con el
/// lote y con la reversa que a esa clase de paso le toca.
async fn copy_leaf(
    targets: &SyncTargets,
    from: &VPath,
    to: &VPath,
    entry: &Entry,
    ctx: &TaskCtx,
) -> Result<u64, Error> {
    use norte_proto::{CollisionPolicy, ResumePolicy, SymlinkPolicy, VerifyPolicy};

    if entry.kind == EntryKind::Symlink {
        let target = crate::ops::with_retry(&ctx.cancel, || {
            use futures::FutureExt as _;
            targets.source.read_link(from).boxed()
        })
        .await?;
        // `Unknown`: el kind lo resuelve el provider destino contra su propio
        // árbol, igual que en la copia normal (issue #18).
        crate::ops::symlink_retrying(
            &targets.dest_at(to),
            &target,
            SymlinkKind::Unknown,
            &ctx.cancel,
        )
        .await?;
        return Ok(0);
    }
    let opts = crate::TransferOptions {
        // Nunca se lee por este camino —`copy_file_retrying` solo mira `resume` y
        // `verify`—, y va explícito para que quede dicho: quien resuelve las
        // colisiones es el ejecutor, revalidando antes de destruir. Lo que hace
        // que un destino ocupado salga como conflicto es que el `write` de un
        // provider es create-new.
        on_collision: CollisionPolicy::Fail,
        symlinks: SymlinkPolicy::Preserve,
        // Cancelar deja el destino LIMPIO, que es el contrato de M1 y lo que
        // esta rama elige: un `.norte-partial` por fichero interrumpido queda en
        // el árbol de destino, y la siguiente comparación lo vería como huérfano
        // —que bajo `Mirror` es un `DeleteTree`—. Reanudar una sincronización
        // grande es una mejora que se puede añadir después; dejar basura que el
        // propio modo se lleva por delante, no.
        resume: ResumePolicy::Off,
        verify: VerifyPolicy::default(),
    };
    let before = ctx.progress.snapshot().bytes_done;
    let observer: Arc<dyn crate::observer::MutationObserver> = Arc::new(NoopObserver);
    crate::ops::copy_file_retrying(
        targets.source.as_ref(),
        &targets.dest_at(to),
        from,
        entry.size,
        opts,
        &observer,
        ctx,
    )
    .await?;
    Ok(ctx.progress.snapshot().bytes_done.saturating_sub(before))
}

/// El origen de un paso que copia, mirado ANTES de tocar el destino.
///
/// **El orden es la mitad de la seguridad de un `Overwrite`.** Mirando el origen
/// DESPUÉS de enterrar el destino, un origen que desapareció entre aprobar y
/// aplicar —o que cambió a directorio— deja la ruta de destino VACÍA: enterrado
/// lo que había y sin nada con que sustituirlo, y con un informe que dice
/// «conflicto» sobre una destrucción que ya ocurrió. Mirándolo primero, el paso
/// falla sin haber tocado nada.
async fn source_leaf(
    targets: &SyncTargets,
    record: &SpoolStep,
    ctx: &TaskCtx,
) -> Result<(VPath, Entry), StepError> {
    let from = under(&targets.source_root, &record.step.rel);
    let entry = stat(targets.source.as_ref(), &from, ctx)
        .await
        .map_err(StepError::from_provider)?
        .ok_or(StepError::Failed(Error::NotFound))?;
    if entry.kind == EntryKind::Dir {
        // El plan nombró una HOJA. Un directorio aquí significa que el origen
        // cambió de forma, y copiarlo en recursivo metería en el lote un
        // subárbol entero que nadie aprobó.
        return Err(StepError::Failed(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        }));
    }
    Ok((from, entry))
}

/// Copia una hoja ya mirada sobre un destino LIBRE, y la journaliza con
/// `reversal`.
async fn place(
    targets: &SyncTargets,
    from: &VPath,
    entry: &Entry,
    recorder: &dyn StepJournal,
    to: &VPath,
    reversal: Reversal,
    ctx: &TaskCtx,
) -> Result<Applied, StepError> {
    let bytes = copy_leaf(targets, from, to, entry, ctx)
        .await
        .map_err(StepError::from_provider)?;
    recorder
        .created(to, reversal)
        .await
        .map_err(StepError::Fatal)?;
    Ok(Applied::Wrote(bytes))
}

/// Borra `path` y todo lo que cuelgue de él, PERMANENTEMENTE.
///
/// Las entradas de dentro NO se journalizan una a una: el paso es UNO —«este
/// árbol ya no está»— y su entrada también, igual que la papelera entierra el
/// árbol de una pieza.
async fn remove_tree(
    targets: &SyncTargets,
    path: &VPath,
    ctx: &TaskCtx,
) -> (u64, Result<(), Error>) {
    let provider = targets.dest.as_ref();
    // El `stat` no es de adorno y es el mismo que hace `ops::delete_task`: el
    // walk empieza por un `list`, y un `list` sobre un fichero es
    // `Conflict{TypeMismatch}`. Un `DeleteTree` nombra la entrada huérfana, que
    // la mayoría de las veces es un FICHERO — sin esto, `Mirror` contra un
    // destino sin papelera (un bucket, un SFTP) contestaría «conflicto» por cada
    // fichero que sobra y no borraría ninguno.
    // `quitados` cuenta lo que YA NO ESTÁ, y se devuelve pase lo que pase — es
    // lo único que distingue «no se llegó a tocar el árbol» de «el árbol está a
    // medio destruir», y de esa distinción cuelga si hay que journalizar (#186).
    let mut quitados = 0u64;
    let entry = match stat(provider, path, ctx).await {
        Ok(Some(e)) => e,
        Ok(None) => return (quitados, Err(Error::NotFound)),
        Err(e) => return (quitados, Err(e)),
    };
    if entry.kind == EntryKind::Dir {
        // El walk emite cada padre antes que sus hijos, así que recorrerlo al
        // revés ES el post-orden y todo directorio llega vacío a su `remove`.
        let entries = match crate::ops::walk(provider, path, &ctx.cancel).await {
            Ok(e) => e,
            Err(e) => return (quitados, Err(e)),
        };
        for entry in entries.iter().rev() {
            if ctx.cancel.is_cancelled() {
                return (quitados, Err(Error::Cancelled));
            }
            // Cada nodo por el DESCRIPTOR cuando hay raíz (#296), y diciendo
            // su clase: el post-orden llega a directorios ya vacíos y a hojas,
            // y `unlinkat` necesita saber cuál de las dos cosas borra.
            match targets
                .dest_at(&entry.path)
                .remove_kind(entry.kind == EntryKind::Dir, &ctx.cancel)
                .await
            {
                Ok(()) => quitados += 1,
                // Un fallo TRAS un transitorio deja el nodo en duda: el
                // `remove` pudo llegar al bucket y perderse la respuesta. Cuenta
                // como quitado, porque el error de contarlo es una fila de más
                // y el de no contarlo es un objeto borrado para siempre sin
                // fila ninguna (#186, revisión de seguridad MAJOR-1).
                Err((e, crate::ops::Ambiguity::MaybeApplied)) => {
                    return (quitados + 1, Err(e));
                }
                Err((e, crate::ops::Ambiguity::NotApplied)) => return (quitados, Err(e)),
            }
        }
    }
    // Y la raíz del árbol, por el mismo camino y diciendo su clase: el `stat`
    // de arriba ya dijo si es un directorio o una hoja suelta.
    match targets
        .dest_at(path)
        .remove_kind(entry.kind == EntryKind::Dir, &ctx.cancel)
        .await
    {
        Ok(()) => (quitados + 1, Ok(())),
        Err((e, crate::ops::Ambiguity::MaybeApplied)) => (quitados + 1, Err(e)),
        Err((e, crate::ops::Ambiguity::NotApplied)) => (quitados, Err(e)),
    }
}

/// Un id de papelera determinista para UN paso, estable en todo reintento (#99).
///
/// **Por VÍCTIMA y no por Task**, que es donde esto se separa de
/// `ops::delete_task`: allí una Task entierra exactamente una cosa y le basta el
/// `task_id` como contador. Aquí una Task entierra cientos, y una papelera
/// LÓGICA nombra su carpeta con el id (`.norte-trash/<ms>-<contador>/`) — con el
/// contador fijo, dos pasos enterrados en el mismo milisegundo chocan, y el
/// segundo sale como `Conflict{Exists}`, o sea como una deriva del destino que
/// nunca ocurrió. `SyncStep::id` es monótono dentro de UN plan, que es
/// exactamente el ámbito que hace falta.
fn trash_id(step_id: u64) -> norte_vfs::trash::TrashId {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    norte_vfs::trash::TrashId::new(now_ms, step_id)
}

/// Destruye una HOJA de la forma que el destino permita: a la papelera si la
/// tiene, para siempre si no.
///
/// **Que el UNDO no pueda deshacer el paso no es razón para negarle al humano su
/// papelera.** Un destino cuya papelera no NOMBRA lo que entierra deja al plan
/// sin reversa —el journal se queda sin `reversal_ref` y adivinar restauraría el
/// fichero equivocado— pero la papelera sigue estando ahí, y lo enterrado se
/// saca a mano desde ella. Borrar permanente lo que se podía enterrar sería
/// destruir de más por un problema de contabilidad.
///
/// No journaliza: quien llama escribe la entrada que le toca (una sola,
/// `Irreversible`).
async fn destroy_leaf(
    targets: &SyncTargets,
    to: &VPath,
    step_id: u64,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if targets.delete_mode == norte_proto::DeleteMode::Trash {
        crate::ops::trash_retrying(targets.dest.as_ref(), to, &trash_id(step_id), &ctx.cancel)
            .await?;
        return Ok(());
    }
    // Por el DESCRIPTOR cuando la raíz está (#296): la teníamos en la mano y
    // el borrado seguía resolviendo la ruta. Está mucho más tapado que una
    // copia —`revalidate` exige que la clase, el tamaño y el mtime del testigo
    // sigan casando antes de destruir nada— pero tapado no es confinado.
    targets
        .dest_at(to)
        .remove_kind(false, &ctx.cancel)
        .await
        .map_err(|(e, _)| e)
}

/// Lo mismo para un ÁRBOL: la papelera se lo lleva de una pieza (por eso ni
/// siquiera hace falta el walk), y sin papelera se recorre en post-orden.
async fn destroy_tree(
    targets: &SyncTargets,
    to: &VPath,
    step_id: u64,
    ctx: &TaskCtx,
) -> (u64, Result<(), Error>) {
    if targets.delete_mode == norte_proto::DeleteMode::Trash {
        // La papelera se lleva el árbol de una pieza: o cero nodos o «este
        // árbol», que a efectos del journal es UNA entrada igual que antes.
        return match crate::ops::trash_retrying_amb(
            targets.dest.as_ref(),
            to,
            &trash_id(step_id),
            &ctx.cancel,
        )
        .await
        {
            Ok(_) => (1, Ok(())),
            // Igual que arriba: un `trash` en duda cuenta como llevado.
            Err((e, crate::ops::Ambiguity::MaybeApplied)) => (1, Err(e)),
            Err((e, crate::ops::Ambiguity::NotApplied)) => (0, Err(e)),
        };
    }
    remove_tree(targets, to, ctx).await
}

/// Entierra `to` en la papelera y lo journaliza.
///
/// # La fila que no llega
/// Si el journal falla DESPUÉS del entierro, el efecto ocurrió y su registro no
/// (regla dura 4 rota en vivo — #160). Se compensa: `restore_from` devuelve lo
/// enterrado a su ruta y el paso muere con el destino intacto. Dos límites, y
/// los dos van al log:
///
/// - `restore_from` puede fallar a su vez, y entonces la línea de log es todo
///   lo que queda;
/// - con una papelera que no NOMBRA lo que se lleva (`DestTrash::Opaque`:
///   macOS, Windows) no hay adónde apuntar y no hay compensación posible.
///
/// `restore_from` hereda el contrato no-replace de [`Provider::rename`]: un
/// provider que lo cumpla al pie de la letra falla con `Conflict` en vez de
/// pisar algo que haya llegado a `to` en la ventana entre el entierro y la
/// compensación. `norte-vfs-sftp` documenta esa ventana como TOCTOU (su
/// `rename` comprueba y renombra, no es atómico) — el mismo riesgo que ya
/// corre [`crate::undo`] al restaurar un `Trashed` con `reversal_ref`, del
/// que esto no es más que otro llamador.
///
/// **Y cuando la compensación NO llega**, el paso sale como
/// [`StepError::Unrecoverable`] y no como un `Fatal` cualquiera: son dos
/// estados distintos del árbol —uno intacto, el otro con un fichero movido que
/// el journal no conoce— y solo el segundo puede decir adónde mirar. La ruta
/// enterrada y su destino en la papelera viajan DENTRO del error (regla 6),
/// que es lo que #160 no tenía: vivían en campos de `tracing`, así que
/// «¿dónde está mi fichero?» solo lo contestaba quien estuviera leyendo el log
/// del daemon en ese instante.
///
/// **La otra mitad del par de un `Overwrite` tampoco se compensa, pero ya se
/// DICE (#206).** Un `created` que falla después de un `trashed` que sí quedó
/// deja un lote cuyo undo bloquea: baja por `seq` y tendría que borrar un
/// fichero del que no tiene fila antes de poder desenterrar el otro.
/// Devolverlo pediría borrar la copia recién puesta Y desenterrar la vieja —
/// dos mutaciones más por el camino en el que el journal ya demostró no
/// funcionar, y ninguna de las dos quedaría registrada tampoco. Así que no se
/// compensa: sale como [`StepError::Unrecoverable`] con la ruta enterrada y su
/// sitio en la papelera dentro, que es la diferencia entre «falló» y «tu
/// fichero está aquí».
async fn bury(
    targets: &SyncTargets,
    recorder: &dyn StepJournal,
    to: &VPath,
    step_id: u64,
    ctx: &TaskCtx,
) -> Result<Option<VPath>, StepError> {
    let buried =
        crate::ops::trash_retrying(targets.dest.as_ref(), to, &trash_id(step_id), &ctx.cancel)
            .await
            .map_err(StepError::from_provider)?;
    if let Err(e) = recorder.trashed(to, buried.as_ref()).await {
        // Regla dura 4 al revés: el efecto ocurrió y su fila no. Lo único que
        // deja el árbol como estaba es DESHACERLO aquí, y desde task 11b se
        // puede: `trash()` devuelve la ruta exacta de lo enterrado y
        // `restore_from` la devuelve a su sitio.
        //
        // Solo cuando la papelera NOMBRA lo que se llevó. Con `DestTrash::Opaque`
        // (macOS, Windows) no hay a qué apuntar y la línea de log sigue siendo
        // toda la respuesta.
        let devuelto = match buried.as_ref() {
            Some(en) => targets.dest.restore_from(en, to).await,
            None => Err(Error::Unsupported),
        };
        // El `reversal_ref` es la ÚNICA pista de dónde fue a parar el fichero, y
        // acaba de no quedar en el journal. Se escribe en el log del operador
        // antes de morir: sin esto, «¿dónde está mi fichero?» no lo contesta
        // nadie. Se dice ADEMÁS si la compensación llegó — un `restore_from` que
        // también falla deja el fichero enterrado, y eso el operador lo necesita
        // saber en la misma línea.
        // La diferencia que un `error!` no sabía hacer. Si la compensación
        // llegó, el árbol es el de antes y esto es un fallo de journal como
        // cualquier otro: se dice aquí y se acabó. Si no, el fichero está en
        // otro sitio y nada lo registra — eso es un ESTADO, no un fallo, y sale
        // TIPADO con la ruta enterrada y su destino dentro, para que el llamante
        // pueda decirlo (y para que un test pueda comprobarlo, que es lo que
        // #160 no tenía). Ese caso NO se loguea aquí: lo hace
        // `ApplyError::into_wire`, una vez, cuando la Task muere.
        if devuelto.is_ok() {
            tracing::error!(
                error = %e,
                enterrado = %crate::engine::span_path(to),
                en = buried.as_ref().map(crate::engine::span_path),
                "sync.apply: se enterró el destino, su entrada de journal NO llegó, y se devolvió \
                 a su sitio",
            );
            return Err(StepError::Fatal(e));
        }
        return Err(StepError::Unrecoverable(Box::new(Unrecorded {
            buried: to.clone(),
            at: buried,
            source: e,
        })));
    }
    // Adónde fue a parar, para quien tenga que decirlo si la OTRA mitad del
    // par falla (#206).
    Ok(buried)
}

/// Una reversa que este core no emite para esta clase de paso.
///
/// Inalcanzable: el transductor deriva la reversa de la clase y de la papelera, y
/// el spool rehúsa al leer un paso cuya forma no se sostiene. Si llegara, NO
/// destruir es la única respuesta.
fn unexpected_reversal(kind: SyncStepKind, reversal: Option<StepReversal>) -> StepError {
    tracing::error!(
        ?kind,
        ?reversal,
        "sync.apply: un paso con una reversa que este core no emite"
    );
    StepError::Failed(Error::Conflict {
        conflict: ConflictKind::Unknown,
    })
}

/// `CreateDir`: crea el directorio, o falla si algo ya ocupa el sitio.
async fn create_dir(
    targets: &SyncTargets,
    recorder: &dyn StepJournal,
    to: &VPath,
    ctx: &TaskCtx,
) -> Result<Applied, StepError> {
    // Pre-stat: es el contrato de `mkdir_retrying` (sin él no puede distinguir su
    // propio directorio fantasma de uno ajeno tras un fallo transitorio), y de
    // paso es la revalidación que a esta clase le toca. Algo ya puesto ahí NO se
    // adopta: reclamar como nuestro un directorio ajeno haría que el undo lo
    // mandara a la papelera con su contenido.
    let pre = stat(targets.dest.as_ref(), to, ctx)
        .await
        .map_err(StepError::from_provider)?;
    if pre.is_some() {
        return Err(StepError::Failed(Error::Conflict {
            conflict: ConflictKind::Exists,
        }));
    }
    crate::ops::mkdir_retrying(&targets.dest_at(to), &ctx.cancel)
        .await
        .map_err(StepError::from_provider)?;
    recorder
        .created(to, Reversal::Delete)
        .await
        .map_err(StepError::Fatal)?;
    Ok(Applied::Wrote(0))
}

/// `Overwrite`: revalidar, mirar el origen, vaciar el destino y copiar.
///
/// Ese orden, y no otro: revalidar antes de destruir es lo que protege del TTL, y
/// mirar el origen antes de destruir es lo que impide dejar la ruta vacía cuando
/// el origen ya no está (ver [`source_leaf`]).
async fn overwrite(
    targets: &SyncTargets,
    record: &SpoolStep,
    recorder: &dyn StepJournal,
    to: &VPath,
    ctx: &TaskCtx,
) -> Result<Applied, StepError> {
    revalidate(targets.dest.as_ref(), to, record.dest, ctx)
        .await
        .map_err(StepError::from_provider)?;
    let (from, entry) = source_leaf(targets, record, ctx).await?;
    match record.step.reversal {
        Some(StepReversal::RestoreTrash) => {
            let at = bury(targets, recorder, to, record.step.id, ctx).await?;
            // A mano y no por `place` (#206): en cuanto el `trashed` quedó
            // escrito, un `created` que falle deja un LOTE cuyo undo bloquea —
            // el undo baja por `seq` y tendría que borrar un fichero del que no
            // tiene fila antes de poder desenterrar el otro. Compensarlo
            // pediría dos mutaciones más justo por el camino en el que el
            // journal ya demostró no funcionar, así que no se compensa: se
            // DICE, con la ruta enterrada y su sitio en la papelera dentro del
            // error, que es lo que convierte «no se pudo deshacer» en «esto
            // está aquí».
            let bytes = copy_leaf(targets, &from, to, &entry, ctx)
                .await
                .map_err(StepError::from_provider)?;
            if let Err(source) = recorder.created(to, Reversal::Delete).await {
                return Err(StepError::Unrecoverable(Box::new(Unrecorded {
                    buried: to.clone(),
                    at,
                    source,
                })));
            }
            Ok(Applied::Wrote(bytes))
        }
        Some(StepReversal::Irreversible) => {
            destroy_leaf(targets, to, record.step.id, ctx)
                .await
                .map_err(StepError::from_provider)?;
            // UNA entrada, irreversible: ver la nota del módulo sobre por qué el
            // borrado no lleva la suya CUANDO LA COPIA LLEGA.
            let placed = place(
                targets,
                &from,
                &entry,
                recorder,
                to,
                Reversal::Irreversible,
                ctx,
            )
            .await;
            if placed.is_err() {
                // Y por qué SÍ la lleva cuando no llega: lo de antes ya no está,
                // lo nuevo no se escribió, y sin esta entrada la destrucción se
                // habría quedado fuera del journal entero (regla dura 4 pide una
                // entrada o una clasificación `Irreversible` explícita; esta es
                // las dos cosas). El informe la nombra, pero el informe es de la
                // Task y se va con ella.
                //
                // Que esa entrada falle es irreparable por lo mismo que en
                // `delete_tree`: el borrado fue permanente. Sale tipada.
                if let Err(e) = recorder.removed(to).await {
                    return Err(StepError::Unrecoverable(Box::new(Unrecorded {
                        buried: to.clone(),
                        at: None,
                        source: e,
                    })));
                }
            }
            placed
        }
        other => Err(unexpected_reversal(record.step.kind, other)),
    }
}

/// `DeleteTree`: revalidar y quitar el árbol de una pieza, a la papelera o para
/// siempre.
async fn delete_tree(
    targets: &SyncTargets,
    record: &SpoolStep,
    recorder: &dyn StepJournal,
    to: &VPath,
    ctx: &TaskCtx,
) -> Result<Applied, StepError> {
    revalidate(targets.dest.as_ref(), to, record.dest, ctx)
        .await
        .map_err(StepError::from_provider)?;
    match record.step.reversal {
        Some(StepReversal::RestoreTrash) => {
            bury(targets, recorder, to, record.step.id, ctx).await?;
            Ok(Applied::Wrote(0))
        }
        Some(StepReversal::Irreversible) => {
            let (quitados, removed) = destroy_tree(targets, to, record.step.id, ctx).await;
            if quitados > 0 && removed.is_err() {
                // Ni el journal ni el informe saben decir «a medias»: la fila
                // dice `removed <raíz>` y el informe no llega a tener fila. La
                // única forma que queda de distinguir «ya no está» de
                // «abollado» es decirlo aquí.
                tracing::warn!(
                    quitados,
                    path = %crate::engine::span_path(to),
                    "sync.apply: el árbol se quedó a MEDIO borrar; su fila de journal dice la \
                     raíz, no cuánto cayó",
                );
            }
            // La entrada se escribe aunque el borrado se haya quedado a medias:
            // «este árbol ya no está entero» es una mutación irreversible tanto
            // si el `remove` llegó al final como si murió en el fichero 40 000, y
            // sin ella la parte destruida se queda fuera del journal (regla dura
            // 4).
            //
            // **La cancelación NO es la excepción, y creerlo era el bug (#186).**
            // `remove_tree` mira el token ENTRE entradas, así que cuando
            // devuelve `Cancelled` ya han caído un número arbitrario de nodos —
            // para un `Mirror` sobre un destino sin papelera, permanentemente.
            // Saltarse la fila ahí dejaba un subárbol medio borrado sin entrada
            // de journal (sin undo) y sin fila de informe (el bucle sale con
            // `Err` en el acto), o sea sin rastro alguno; y encima tres
            // frontends afirman en prosa que lo aplicado queda journalizado.
            //
            // Lo que decide es `quitados`, no la clase del error: cero nodos es
            // «no se llegó a tocar el árbol» —la revalidación que ya salió por
            // arriba, un `walk` que falló, una cancelación antes del primer
            // borrado— y eso sí que no merece entrada.
            if quitados > 0 {
                // Y si ESTA fila tampoco llega, es la misma avería que `bury`
                // pero peor: lo borrado es permanente y no hay compensación
                // posible, así que el error tiene que llevar al menos la ruta.
                // `at: None` porque no hay papelera adonde apuntar — eso es lo
                // que significa `Irreversible` aquí.
                if let Err(e) = recorder.removed(to).await {
                    return Err(StepError::Unrecoverable(Box::new(Unrecorded {
                        buried: to.clone(),
                        at: None,
                        source: e,
                    })));
                }
            }
            removed.map_err(StepError::from_provider)?;
            Ok(Applied::Wrote(0))
        }
        other => Err(unexpected_reversal(record.step.kind, other)),
    }
}

/// La policy, preguntada por la ruta REAL de un paso que va a actuar.
///
/// Se pregunta por lo que el paso HACE: crear un directorio es `mkdir`, copiar es
/// `copy`, y una sobrescritura es las dos —destruye lo que había y escribe encima—
/// así que pasa por las dos puertas.
fn gate_step(
    targets: &SyncTargets,
    kind: SyncStepKind,
    to: &VPath,
    actor: &Actor,
) -> Result<(), StepError> {
    use crate::policy::PolicyOp;
    let del = PolicyOp::Delete {
        mode: targets.delete_mode,
    };
    let ops: &[PolicyOp] = match kind {
        SyncStepKind::CreateDir => &[PolicyOp::Mkdir],
        SyncStepKind::Copy => &[PolicyOp::Copy],
        SyncStepKind::Overwrite => &[del, PolicyOp::Copy],
        SyncStepKind::DeleteTree => &[del],
        // Un `Skip` no actúa y una clase desconocida no llega a actuar.
        _ => &[],
    };
    for op in ops {
        targets.allows(*op, to, actor).map_err(StepError::Failed)?;
    }
    Ok(())
}

/// Ejecuta UN paso.
async fn execute(
    targets: &SyncTargets,
    record: &SpoolStep,
    recorder: &dyn StepJournal,
    ctx: &TaskCtx,
) -> Result<Applied, StepError> {
    let to = dest_path(targets, record);
    if record.step.kind != SyncStepKind::Skip {
        // La raíz de destino NO es un paso. El transductor ya lo impide
        // (`SyncError::RootIsNotAStep`) y la `rel` entra en el `plan_hash`, así
        // que hoy no se alcanza; la invariante se comprueba AQUÍ porque aquí es
        // donde un fallo significa «se borró el árbol de destino entero» y el
        // guard vive en otro crate.
        if to == targets.dest_root {
            tracing::error!("sync.apply: un paso que actúa nombra la raíz de destino");
            return Err(StepError::Failed(Error::InvalidPath));
        }
        gate_step(targets, record.step.kind, &to, &ctx.actor)?;
    }
    match record.step.kind {
        SyncStepKind::Skip => Ok(Applied::Skipped),
        SyncStepKind::CreateDir => create_dir(targets, recorder, &to, ctx).await,
        SyncStepKind::Copy => {
            let (from, entry) = source_leaf(targets, record, ctx).await?;
            place(targets, &from, &entry, recorder, &to, Reversal::Delete, ctx).await
        }
        SyncStepKind::Overwrite => overwrite(targets, record, recorder, &to, ctx).await,
        SyncStepKind::DeleteTree => delete_tree(targets, record, recorder, &to, ctx).await,
        // El spool rechaza al leer un paso de clase desconocida, así que esto no
        // se alcanza desde un fichero que escribiéramos nosotros. `SyncStepKind`
        // es `#[non_exhaustive]`, así que el comodín es obligatorio, y que caiga
        // del lado de NO tocar nada es lo único seguro: una clase que este
        // binario no sabe nombrar tampoco sabe deshacer.
        _ => Err(StepError::Failed(Error::Unsupported)),
    }
}

/// Anota un paso fallido. La LISTA tiene tope; el CONTADOR no.
fn record_failure(report: &Mutex<SyncReportResult>, step: &SyncStep, cause: SyncFailureCause) {
    // INVARIANTE: el `Mutex` solo se envenena si otro hilo entró en pánico
    // sosteniéndolo, que es irrecuperable — el mismo criterio que el resto de los
    // locks del core.
    let mut report = report.lock().expect("sync report lock");
    report.failed = report.failed.saturating_add(1);
    if report.failures.len() < SYNC_MAX_FAILURES_REPORTED {
        report.failures.push(SyncFailure {
            rel: step.rel.clone(),
            // La CLASE del paso, que el core tiene delante y hasta 0.41.0 tiraba
            // (#195). Es lo que dice de qué raíz cuelga `rel` —un `DeleteTree`
            // habla siempre del destino— sin que quien lee el informe, que no
            // tiene el plan, lo deduzca de la presencia de `dest_rel`.
            kind: step.kind,
            // La ortografía del DESTINO viaja con el fallo: sin ella, el caso
            // estrella de `IllegalName` —un nombre que revienta `NAME_MAX` al
            // recomponerse en NFD— se enseñaría con la grafía del origen, que es
            // la corta y la legal.
            dest_rel: step.dest_rel.clone(),
            cause,
        });
    }
}

/// Ejecuta el plan: un paso detrás de otro, EN EL ORDEN EN QUE VIENE.
///
/// El walk es pre-orden, así que un `CreateDir` precede siempre a toda copia
/// dentro de él: **no se ordena nada**.
///
/// La cancelación se mira ENTRE pasos (regla dura 3), y también la ven los
/// `ops` de dentro de un paso —una copia de un GiB no espera a terminar—. Lo que
/// se aplicó se queda journalizado bajo su lote; NO se desanda (ver la nota del
/// módulo).
///
/// **Lo que el informe NO distingue.** Una fila `Conflict` puede ser «no se tocó
/// nada» (la revalidación lo cazó a tiempo) o «se enterró el destino y la copia
/// no llegó», y la acción que le toca al usuario no es la misma: mirar la
/// papelera, o replanificar. Distinguirlas cuesta una causa de wire más, que es
/// vocabulario cerrado daemon→cliente; hoy lo dice el log del daemon.
///
/// `steps` puede fallar A MITAD, con pasos ya ejecutados: un spool truncado o
/// editado. No es lo mismo que un paso que falla —no se sabe qué venía después,
/// así que no hay nada que anotar como fila— y se responde parando la Task. El
/// lote queda cerrado y deshacible en ese punto, que es lo que importa.
///
/// # Errors
/// [`Error::Cancelled`], el error del journal, o el que trajera el flujo de
/// pasos. Un paso que falla NO sale por aquí: sale en `report`.
///
/// # Panics
/// Solo si el `Mutex` del informe está envenenado (otro hilo entró en pánico
/// sosteniéndolo), que es lo mismo que hace el resto del core con sus locks.
#[tracing::instrument(
    skip_all,
    fields(
        task_id = ctx.progress.snapshot().task_id.get(),
        dest = %crate::engine::span_path(&targets.dest_root),
    )
)]
pub(crate) async fn run<S>(
    targets: SyncTargets,
    recorder: &dyn StepJournal,
    steps: S,
    ctx: &TaskCtx,
    report: &Mutex<SyncReportResult>,
) -> Result<(), ApplyError>
where
    S: Stream<Item = Result<SpoolStep, Error>>,
{
    if ctx.cancel.is_cancelled() {
        return Err(ApplyError::Stopped(Error::Cancelled));
    }
    // La raíz de destino se abre AQUÍ, una vez para toda la Task (#164): por eso
    // los `targets` entran por valor, y no prestados como todo lo demás. Que no
    // se pueda abrir un destino que dijo saber confinar PARA la Task entera, y
    // para antes de tocar nada — no es una fila del informe, es que la defensa
    // que el humano vio anunciada no está.
    let targets = &targets
        .with_dest_confined(ctx.progress.snapshot().task_id.get())
        .await
        .map_err(ApplyError::Stopped)?;
    let mut steps = std::pin::pin!(steps);
    loop {
        if ctx.cancel.is_cancelled() {
            return Err(ApplyError::Stopped(Error::Cancelled));
        }
        let Some(next) = steps.next().await else {
            return Ok(());
        };
        let record = match next {
            Ok(record) => record,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "sync.apply: el plan dejó de poder leerse a mitad de la ejecución"
                );
                return Err(ApplyError::Stopped(e));
            }
        };
        match execute(targets, &record, recorder, ctx).await {
            Ok(Applied::Wrote(bytes)) => {
                let mut report = report.lock().expect("sync report lock");
                report.done = report.done.saturating_add(1);
                report.bytes = report.bytes.saturating_add(bytes);
            }
            Ok(Applied::Skipped) => {
                let mut report = report.lock().expect("sync report lock");
                report.skipped = report.skipped.saturating_add(1);
            }
            Err(StepError::Failed(e)) => {
                let cause = cause_of(&e);
                tracing::debug!(?cause, "sync.apply: un paso no ocurrió");
                record_failure(report, &record.step, cause);
            }
            // La cancelación y el journal paran la Task. El journal, porque
            // seguir produciría más efectos fuera de él (regla dura 4); la
            // cancelación, porque es lo que se pidió.
            Err(StepError::Fatal(e)) => return Err(ApplyError::Stopped(e)),
            // Y esta para por lo mismo, pero llevándose ADÓNDE MIRAR: el
            // destino está enterrado, su fila no llegó y devolverlo falló.
            //
            // Con FILA DE INFORME, y es la mitad que #160 pedía por su nombre
            // («report the step as failed rather than leaving the task in an
            // unstated state»). Sin ella el informe sale `done: 0, failed: 0`
            // cuando la avería es en el primer paso, todos los frontends pintan
            // ceros y el humano lee «no pasó nada» sobre un fichero que está en
            // la papelera. El log del daemon no le llega a nadie que no lo esté
            // mirando.
            Err(StepError::Unrecoverable(u)) => {
                record_failure(report, &record.step, SyncFailureCause::Io);
                return Err(ApplyError::Unrecorded(u));
            }
        }
        ctx.progress.update(|p| p.entries_done += 1);
    }
}

/// La ruta que un paso lee del ORIGEN, para el gate y para los tests.
#[cfg(test)]
fn source_path(targets: &SyncTargets, record: &SpoolStep) -> VPath {
    under(&targets.source_root, &record.step.rel)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, StepReversal, SyncStep, SyncStepKind,
    };
    use norte_proto::{CapabilityFlags, TaskKind};
    use norte_testkit::MemProvider;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::progress::ProgressReporter;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire")
    }

    fn rel(wire: &str) -> RelPath {
        RelPath::parse_wire(wire).expect("rel")
    }

    fn ctx(cancel: CancellationToken) -> TaskCtx {
        TaskCtx {
            cancel,
            progress: Arc::new(
                ProgressReporter::new(norte_proto::TaskId::new(1), TaskKind::Sync).0,
            ),
            actor: Actor::User,
        }
    }

    async fn write(mem: &MemProvider, wire: &str, content: &[u8]) {
        let mut sink = mem.write(&vp(wire)).await.expect("write");
        sink.write(Bytes::copy_from_slice(content))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    async fn read(mem: &MemProvider, wire: &str) -> Vec<u8> {
        let mut stream = mem.read(&vp(wire), None).await.expect("read");
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.expect("chunk"));
        }
        out
    }

    /// Un recorder que solo apunta lo que le pidieron: los tests de journal de
    /// verdad viven en el fichero de integración, con `SQLite` detrás.
    /// Una entrada apuntada: op, ruta, reversa y referencia de papelera.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Anotada {
        op: &'static str,
        path: String,
        reversal: Reversal,
        reversal_ref: Option<String>,
    }

    #[derive(Default)]
    struct Recorder {
        entries: Mutex<Vec<Anotada>>,
    }

    #[async_trait]
    impl StepJournal for Recorder {
        async fn created(&self, path: &VPath, reversal: Reversal) -> Result<(), Error> {
            self.entries.lock().expect("lock").push(Anotada {
                op: "created",
                path: path.to_wire(),
                reversal,
                reversal_ref: None,
            });
            Ok(())
        }
        async fn trashed(&self, path: &VPath, dest: Option<&VPath>) -> Result<(), Error> {
            self.entries.lock().expect("lock").push(Anotada {
                op: "trashed",
                path: path.to_wire(),
                reversal: Reversal::RestoreTrash,
                reversal_ref: dest.map(VPath::to_wire),
            });
            Ok(())
        }
        async fn removed(&self, path: &VPath) -> Result<(), Error> {
            self.entries.lock().expect("lock").push(Anotada {
                op: "removed",
                path: path.to_wire(),
                reversal: Reversal::Irreversible,
                reversal_ref: None,
            });
            Ok(())
        }
    }

    /// Un recorder que falla EXACTAMENTE en `trashed`, que es el arma del #160:
    /// la papelera se llevó el fichero y la fila no llegó.
    #[derive(Default)]
    struct TrashedFalla;

    #[async_trait]
    impl StepJournal for TrashedFalla {
        async fn created(&self, _path: &VPath, _reversal: Reversal) -> Result<(), Error> {
            Ok(())
        }
        async fn trashed(&self, _path: &VPath, _dest: Option<&VPath>) -> Result<(), Error> {
            Err(Error::Io { retryable: false })
        }
        async fn removed(&self, _path: &VPath) -> Result<(), Error> {
            Ok(())
        }
    }

    /// Un recorder que falla EXACTAMENTE en `created`, que es el arma del
    /// #206: el `trashed` quedó escrito y su pareja no.
    #[derive(Default)]
    struct CreatedFalla;

    #[async_trait]
    impl StepJournal for CreatedFalla {
        async fn created(&self, _path: &VPath, _reversal: Reversal) -> Result<(), Error> {
            Err(Error::Io { retryable: false })
        }
        async fn trashed(&self, _path: &VPath, _dest: Option<&VPath>) -> Result<(), Error> {
            Ok(())
        }
        async fn removed(&self, _path: &VPath) -> Result<(), Error> {
            Ok(())
        }
    }

    fn step(kind: SyncStepKind, rel_wire: &str, reversal: Option<StepReversal>) -> SyncStep {
        SyncStep {
            id: 1,
            kind,
            rel: rel(rel_wire),
            dest_rel: None,
            size: None,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            reversal,
            reason: reversal
                .filter(|r| *r == StepReversal::Irreversible)
                .map(|_| norte_proto::methods::SyncReason::NoTrashOnTarget),
        }
    }

    /// Un paso `Overwrite` con papelera sobre `nombre`, con el testigo que la
    /// revalidación exige tomado del destino REAL.
    async fn sobrescribe(mem: &Arc<MemProvider>, nombre: &str) -> SpoolStep {
        let entry = mem
            .stat(&vp(&format!("mem:///d/{nombre}")))
            .await
            .expect("stat del destino");
        SpoolStep {
            step: step(
                SyncStepKind::Overwrite,
                nombre,
                Some(StepReversal::RestoreTrash),
            ),
            dest: Some(DestWitness::of(&entry)),
        }
    }

    fn targets(mem: &Arc<MemProvider>) -> SyncTargets {
        SyncTargets {
            source: Arc::clone(mem) as Arc<dyn Provider>,
            dest: Arc::clone(mem) as Arc<dyn Provider>,
            source_root: vp("mem:///s"),
            dest_root: vp("mem:///d"),
            policy: Arc::new(crate::policy::AllowAll),
            delete_mode: norte_proto::DeleteMode::Trash,
            dest_confined: None,
        }
    }

    /// `dest_rel` manda sobre `rel`: se escribe sobre el fichero que EXISTE, no
    /// sobre el que el origen deletrea. Es la mitad de #152 que sí está cerrada,
    /// y sin ella un `Overwrite` de un `café` NFC contra un `café` NFD crearía
    /// sobre ext4 un SEGUNDO fichero al lado del que se quería sobrescribir.
    #[tokio::test]
    async fn el_destino_lo_nombra_dest_rel_cuando_lo_hay() {
        let mem = Arc::new(MemProvider::new());
        let t = targets(&mem);
        let mut s = step(
            SyncStepKind::Copy,
            "caf%C3%A9.txt",
            Some(StepReversal::Delete),
        );
        s.dest_rel = Some(rel("cafe%CC%81.txt"));
        let record = SpoolStep {
            step: s,
            dest: None,
        };
        assert_eq!(dest_path(&t, &record).to_wire(), "mem:///d/cafe\u{301}.txt");
        assert_eq!(source_path(&t, &record).to_wire(), "mem:///s/caf\u{e9}.txt");
    }

    /// #176: un `DeleteTree` mira también CUÁNTAS cosas hay dentro.
    ///
    /// El `stat` de un directorio solo se mueve cuando cambian sus hijos
    /// DIRECTOS —y en `MemProvider` ni eso—, así que sin el recuento un
    /// subárbol que ganó ficheros entre aprobar y aplicar revalidaba limpio y
    /// se borraba entero: el paso con más radio de acción con la comprobación
    /// más floja.
    #[tokio::test]
    async fn un_borrado_de_arbol_cuenta_su_primer_nivel() {
        let mem = Arc::new(MemProvider::new());
        let c = ctx(CancellationToken::new());
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d/sub")).await.expect("mkdir");
        write(&mem, "mem:///d/sub/a.txt", b"a").await;

        let entry = mem.stat(&vp("mem:///d/sub")).await.expect("stat");
        let foto = DestWitness::of(&entry).with_entries(Some(1));
        // Como se planificó: una entrada dentro.
        revalidate(mem.as_ref(), &vp("mem:///d/sub"), Some(foto), &c)
            .await
            .expect("nada cambió");

        // Alguien mete algo mientras el humano decide.
        write(&mem, "mem:///d/sub/b.txt", b"b").await;
        let err = revalidate(mem.as_ref(), &vp("mem:///d/sub"), Some(foto), &c)
            .await
            .expect_err("el árbol ya no es el que se aprobó");
        assert_eq!(cause_of(&err), SyncFailureCause::Conflict);

        // Y un testigo SIN recuento no puede declarar conflicto por eso: es
        // «no se sabe», jamás «cero» (mismo criterio que el tamaño).
        let sin_cuenta = DestWitness::of(&entry);
        revalidate(mem.as_ref(), &vp("mem:///d/sub"), Some(sin_cuenta), &c)
            .await
            .expect("sin recuento, no hay nada que comparar");
    }

    /// La revalidación mira lo que las DOS fotos traen. Un tamaño que se movió
    /// es un conflicto; un testigo sin tamaño no puede serlo (`file://` lista
    /// así de serie y rechazaría el plan entero).
    #[tokio::test]
    async fn la_revalidacion_solo_compara_lo_que_las_dos_fotos_traen() {
        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///d/a.txt", b"12345").await;
        let c = ctx(CancellationToken::new());
        let entry = mem.stat(&vp("mem:///d/a.txt")).await.expect("stat");

        let igual = DestWitness::of(&entry);
        revalidate(mem.as_ref(), &vp("mem:///d/a.txt"), Some(igual), &c)
            .await
            .expect("no ha cambiado");

        let otro_tamano = DestWitness {
            size: Some(99),
            ..igual
        };
        let err = revalidate(mem.as_ref(), &vp("mem:///d/a.txt"), Some(otro_tamano), &c)
            .await
            .expect_err("cambió de tamaño");
        assert_eq!(cause_of(&err), SyncFailureCause::Conflict);

        let sin_medidas = DestWitness {
            kind: entry.kind,
            size: None,
            mtime_ms: None,
            entries: None,
        };
        revalidate(mem.as_ref(), &vp("mem:///d/a.txt"), Some(sin_medidas), &c)
            .await
            .expect("un provider que no mide no produce conflictos");

        let otra_clase = DestWitness {
            kind: EntryKind::Dir,
            ..igual
        };
        let err = revalidate(mem.as_ref(), &vp("mem:///d/a.txt"), Some(otra_clase), &c)
            .await
            .expect_err("cambió de clase");
        assert_eq!(cause_of(&err), SyncFailureCause::Conflict);
    }

    /// La taxonomía del informe, y en particular la causa que existe porque la
    /// legalidad de un nombre bajo el destino NO se valida al planificar.
    #[test]
    fn cada_error_cae_en_la_causa_que_le_toca() {
        assert_eq!(
            cause_of(&Error::InvalidPath),
            SyncFailureCause::IllegalName,
            "un nombre que el destino no admite tiene nombre propio"
        );
        assert_eq!(cause_of(&Error::PermissionDenied), SyncFailureCause::Denied);
        assert_eq!(
            cause_of(&Error::PolicyDenied {
                rule: "out-of-scope".to_owned()
            }),
            SyncFailureCause::Denied,
            "«no puedes» y «así no se puede llamar» llevan a acciones distintas"
        );
        assert_eq!(cause_of(&Error::NotFound), SyncFailureCause::Conflict);
        assert_eq!(
            cause_of(&Error::Io { retryable: true }),
            SyncFailureCause::Io
        );
    }

    /// Un paso destructivo SIN testigo se rehúsa. El testigo no entra en el
    /// `plan_hash`, así que borrarlo del spool es justo la edición que el digest
    /// no ve; exigirlo es lo que la deja sin efecto.
    #[tokio::test]
    async fn un_paso_destructivo_sin_testigo_no_destruye_nada() {
        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///d/a.txt", b"12345").await;
        let c = ctx(CancellationToken::new());
        let err = revalidate(mem.as_ref(), &vp("mem:///d/a.txt"), None, &c)
            .await
            .expect_err("sin foto no se destruye");
        assert_eq!(cause_of(&err), SyncFailureCause::Conflict);
    }

    /// Y el destino que ya no está también es conflicto, no un error a secas: es
    /// lo que hace que un `DeleteTree` cuyo árbol alguien borró antes no cuente
    /// como avería.
    #[tokio::test]
    async fn un_destino_que_desaparecio_es_conflicto() {
        let mem = Arc::new(MemProvider::new());
        let c = ctx(CancellationToken::new());
        let entry = Entry {
            path: vp("mem:///d/no-esta"),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        let err = revalidate(
            mem.as_ref(),
            &vp("mem:///d/no-esta"),
            Some(DestWitness::of(&entry)),
            &c,
        )
        .await
        .expect_err("no está");
        assert_eq!(cause_of(&err), SyncFailureCause::Conflict);
    }

    /// Un `Overwrite` sin papelera deja UNA entrada `created` irreversible, y no
    /// una pareja `removed`+`created`: con la pareja, el undo del lote borraría
    /// el fichero nuevo sin poder restaurar el viejo y dejaría la ruta vacía.
    #[tokio::test]
    async fn una_sobrescritura_sin_papelera_es_una_entrada_irreversible() {
        // `MemProvider` sin `TRASH`, que es lo que declara de serie.
        let mem = Arc::new(MemProvider::with_flags(CapabilityFlags::CASE_SENSITIVE));
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///s/a.txt", b"nuevo").await;
        write(&mem, "mem:///d/a.txt", b"viejo").await;
        let entry = mem.stat(&vp("mem:///d/a.txt")).await.expect("stat");

        // Sin papelera en el destino, el borrado es PERMANENTE: es lo que
        // `engine` deriva de las capabilities y lo que el gate autorizó.
        let t = SyncTargets {
            delete_mode: norte_proto::DeleteMode::Permanent,
            ..targets(&mem)
        };
        let recorder = Recorder::default();
        let record = SpoolStep {
            step: step(
                SyncStepKind::Overwrite,
                "a.txt",
                Some(StepReversal::Irreversible),
            ),
            dest: Some(DestWitness::of(&entry)),
        };
        let report = Mutex::new(new_report(7, DestTrash::Restorable));
        run(
            t,
            &recorder,
            futures::stream::iter(vec![Ok(record)]),
            &ctx(CancellationToken::new()),
            &report,
        )
        .await
        .expect("la aplicación termina");

        assert_eq!(read(&mem, "mem:///d/a.txt").await, b"nuevo");
        let entries = recorder.entries.lock().expect("lock").clone();
        assert_eq!(entries.len(), 1, "una sola entrada: {entries:?}");
        assert_eq!(entries[0].op, "created");
        assert_eq!(entries[0].reversal, Reversal::Irreversible);
        assert_eq!(report.lock().expect("lock").done, 1);
    }

    /// **Irreversible NO quiere decir «borra permanente».** Un destino cuya
    /// papelera no NOMBRA lo que entierra produce pasos irreversibles —el undo
    /// no puede acertar— pero la papelera sigue existiendo, y lo sobrescrito
    /// tiene que acabar dentro de ella: el humano lo saca a mano. Borrarlo
    /// para siempre sería destruir de más por un problema de contabilidad.
    #[tokio::test]
    async fn un_paso_irreversible_con_papelera_entierra_en_vez_de_borrar() {
        let mem = Arc::new(
            MemProvider::with_flags(CapabilityFlags::CASE_SENSITIVE | CapabilityFlags::TRASH)
                .with_logical_trash(),
        );
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///s/a.txt", b"nuevo").await;
        write(&mem, "mem:///d/a.txt", b"viejo").await;
        let entry = mem.stat(&vp("mem:///d/a.txt")).await.expect("stat");

        let t = targets(&mem);
        let recorder = Recorder::default();
        let record = SpoolStep {
            step: step(
                SyncStepKind::Overwrite,
                "a.txt",
                Some(StepReversal::Irreversible),
            ),
            dest: Some(DestWitness::of(&entry)),
        };
        let report = Mutex::new(new_report(7, DestTrash::Restorable));
        run(
            t,
            &recorder,
            futures::stream::iter(vec![Ok(record)]),
            &ctx(CancellationToken::new()),
            &report,
        )
        .await
        .expect("la aplicación termina");

        assert_eq!(read(&mem, "mem:///d/a.txt").await, b"nuevo");
        // Sigue habiendo UNA entrada irreversible (el journal no promete undo)…
        let entries = recorder.entries.lock().expect("lock").clone();
        assert_eq!(entries.len(), 1, "una sola entrada: {entries:?}");
        assert_eq!(entries[0].reversal, Reversal::Irreversible);
        // …y sin embargo lo viejo está en la papelera, no aniquilado.
        assert!(
            mem.stat(&vp("mem:///.norte-trash")).await.is_ok(),
            "lo sobrescrito se enterró en vez de borrarse para siempre"
        );
    }

    /// Un flujo que se rompe A MITAD no es un paso que falla: lo ya ejecutado se
    /// queda (y journalizado), lo que venía después no se sabe, y la Task para.
    #[tokio::test]
    async fn un_plan_que_deja_de_leerse_a_mitad_para_la_task_sin_anotar_fila() {
        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///s/a.txt", b"x").await;
        let t = targets(&mem);
        let recorder = Recorder::default();
        let report = Mutex::new(new_report(7, DestTrash::Restorable));
        let flujo = futures::stream::iter(vec![
            Ok(SpoolStep {
                step: step(SyncStepKind::Copy, "a.txt", Some(StepReversal::Delete)),
                dest: None,
            }),
            Err(Error::Io { retryable: false }),
        ]);
        let err = run(t, &recorder, flujo, &ctx(CancellationToken::new()), &report)
            .await
            .expect_err("el plan dejó de leerse");
        assert!(
            matches!(err, ApplyError::Stopped(Error::Io { .. })),
            "{err:?}"
        );

        let (done, failed) = {
            let report = report.lock().expect("lock");
            (report.done, report.failed)
        };
        assert_eq!(done, 1, "lo aplicado se queda");
        assert_eq!(failed, 0, "no hay paso al que atribuirlo");
        assert_eq!(read(&mem, "mem:///d/a.txt").await, b"x");
        assert_eq!(recorder.entries.lock().expect("lock").len(), 1);
    }

    /// #160: si la fila de journal NO llega DESPUÉS de haber enterrado el
    /// destino, el fichero está movido y sin registrar — regla dura 4 rota en
    /// vivo. La compensación es sacarlo de la papelera: el paso falla, la Task
    /// para, y el destino se queda con sus bytes originales.
    #[tokio::test]
    async fn un_journal_que_falla_tras_enterrar_devuelve_el_fichero_a_su_sitio() {
        let mem = Arc::new(MemProvider::new().with_logical_trash());
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///d/a.txt", b"viejo").await;
        let t = targets(&mem);

        let err = super::bury(
            &t,
            &TrashedFalla,
            &vp("mem:///d/a.txt"),
            1,
            &ctx(CancellationToken::new()),
        )
        .await
        .expect_err("el journal falló");
        assert!(
            matches!(err, StepError::Fatal(Error::Io { .. })),
            "el fallo de journal para la Task: {err:?}"
        );

        assert_eq!(
            read(&mem, "mem:///d/a.txt").await,
            b"viejo",
            "el destino volvió de la papelera con sus bytes"
        );
    }

    /// Un `MemProvider` que CANCELA en cuanto ha borrado algo.
    ///
    /// Es lo que hace determinista el test de #186: `remove_tree` mira el token
    /// ENTRE entradas, así que «cancelado a mitad» solo se reproduce si el
    /// token cae entre dos `remove`. Con una tarea que cancela por reloj eso es
    /// una carrera, y una carrera en la suite es un test que un día se pone
    /// rojo y no dice nada.
    struct CancelaAlBorrar {
        inner: Arc<MemProvider>,
        cancel: CancellationToken,
    }

    #[async_trait]
    impl Provider for CancelaAlBorrar {
        fn scheme(&self) -> &str {
            self.inner.scheme()
        }
        fn capabilities(&self) -> norte_proto::Capabilities {
            self.inner.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
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
            self.inner.read(p, range).await
        }
        async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
            self.inner.write(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
            self.inner.mkdir(p).await
        }
        async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
            self.inner.rename(from, to).await
        }
        /// Borra de verdad, y ENTONCES cancela: al volver, el árbol ya está a
        /// medias y el token ya está caído.
        async fn remove(&self, p: &VPath) -> Result<(), Error> {
            self.inner.remove(p).await?;
            self.cancel.cancel();
            Ok(())
        }
    }

    /// Un `MemProvider` cuyo `remove` APLICA el efecto y contesta un fallo
    /// transitorio, cancelando de paso.
    ///
    /// Es el «timeout tras commit» de un remoto (issue #17) atrapado por una
    /// cancelación: el objeto ya no está en el bucket, la respuesta se perdió,
    /// y el usuario —que lleva un rato mirando el parón— pulsa `Ctrl+K`. Sin
    /// esto no hay forma determinista de llegar a esa esquina.
    struct BorraYMiente {
        inner: Arc<MemProvider>,
        cancel: CancellationToken,
    }

    #[async_trait]
    impl Provider for BorraYMiente {
        fn scheme(&self) -> &str {
            self.inner.scheme()
        }
        fn capabilities(&self) -> norte_proto::Capabilities {
            self.inner.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
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
            self.inner.read(p, range).await
        }
        async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
            self.inner.write(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
            self.inner.mkdir(p).await
        }
        async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
            self.inner.rename(from, to).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), Error> {
            self.inner.remove(p).await?;
            self.cancel.cancel();
            Err(Error::Io { retryable: true })
        }
    }

    /// **#186 por la otra puerta, la que la primera versión del arreglo dejaba
    /// abierta.** Un `remove` que se aplica y muere en un transitorio cuenta
    /// como quitado.
    ///
    /// El objeto ya no está en el bucket y la respuesta se perdió; si ese es el
    /// PRIMER nodo del post-orden, contarlo como «no quitado» deja el árbol
    /// dentado, borrado para siempre, y sin fila de journal — que es
    /// exactamente el agujero, alcanzado sin cancelar entre entradas. Todo el
    /// árbol resuelve la duda hacia «lo hicimos»: el error de esa elección es
    /// una fila de más, el de la contraria es un efecto sin fila.
    #[tokio::test]
    async fn un_borrado_ambiguo_cuenta_como_borrado() {
        let mem = Arc::new(MemProvider::with_flags(CapabilityFlags::CASE_SENSITIVE));
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d/sub")).await.expect("mkdir");
        write(&mem, "mem:///d/sub/a.txt", b"uno").await;
        let entry = mem.stat(&vp("mem:///d/sub")).await.expect("stat");

        let cancel = CancellationToken::new();
        let dest = Arc::new(BorraYMiente {
            inner: Arc::clone(&mem),
            cancel: cancel.clone(),
        });
        let t = SyncTargets {
            dest: dest as Arc<dyn Provider>,
            delete_mode: norte_proto::DeleteMode::Permanent,
            ..targets(&mem)
        };
        let recorder = Recorder::default();
        let err = delete_tree(
            &t,
            &SpoolStep {
                step: step(
                    SyncStepKind::DeleteTree,
                    "sub",
                    Some(StepReversal::Irreversible),
                ),
                dest: Some(DestWitness::of(&entry)),
            },
            &recorder,
            &vp("mem:///d/sub"),
            &ctx(cancel),
        )
        .await
        .expect_err("el remove murió en un transitorio");
        assert!(
            matches!(err, StepError::Failed(Error::Io { .. })),
            "{err:?}"
        );

        assert!(
            mem.stat(&vp("mem:///d/sub/a.txt")).await.is_err(),
            "el efecto SÍ se aplicó, aunque la respuesta se perdiera"
        );
        assert_eq!(
            recorder.entries.lock().expect("lock").len(),
            1,
            "y por tanto hay fila: un efecto en duda se resuelve hacia «ocurrió»"
        );
    }

    /// **#186.** Un `DeleteTree` irreversible cancelado A MITAD escribe la fila
    /// de lo que SÍ borró.
    ///
    /// `remove_tree` mira el token de cancelación ENTRE entradas, así que
    /// cuando devuelve `Cancelled` ya han caído un número arbitrario de nodos —
    /// y para un `Mirror` contra un destino sin papelera (un bucket, un SFTP,
    /// un pincho FAT), permanentemente. La condición de antes se saltaba la
    /// fila justo en ese caso, y dejaba un subárbol medio borrado con: sin
    /// entrada de journal (sin undo), sin fila de informe (el bucle sale con
    /// `Err` en el acto) y sin nada que decirle al usuario. Los tres frontends
    /// afirman en prosa que lo aplicado queda journalizado.
    #[tokio::test]
    async fn un_borrado_de_arbol_cancelado_a_medias_deja_su_fila() {
        // Sin papelera en el destino: el borrado es PERMANENTE, que es el caso
        // en el que la fila es lo único que queda.
        let mem = Arc::new(MemProvider::with_flags(CapabilityFlags::CASE_SENSITIVE));
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d/sub")).await.expect("mkdir");
        write(&mem, "mem:///d/sub/a.txt", b"uno").await;
        write(&mem, "mem:///d/sub/b.txt", b"dos").await;
        let entry = mem.stat(&vp("mem:///d/sub")).await.expect("stat");

        let cancel = CancellationToken::new();
        let dest = Arc::new(CancelaAlBorrar {
            inner: Arc::clone(&mem),
            cancel: cancel.clone(),
        });
        let t = SyncTargets {
            dest: dest as Arc<dyn Provider>,
            delete_mode: norte_proto::DeleteMode::Permanent,
            ..targets(&mem)
        };
        let recorder = Recorder::default();
        let record = SpoolStep {
            step: step(
                SyncStepKind::DeleteTree,
                "sub",
                Some(StepReversal::Irreversible),
            ),
            dest: Some(DestWitness::of(&entry)),
        };
        let report = Mutex::new(new_report(7, DestTrash::Restorable));
        let err = run(
            t,
            &recorder,
            futures::stream::iter(vec![Ok(record)]),
            &ctx(cancel),
            &report,
        )
        .await
        .expect_err("cancelado a mitad del árbol");
        assert!(
            matches!(err, ApplyError::Stopped(Error::Cancelled)),
            "{err:?}"
        );

        // Algo cayó, y para siempre.
        assert!(
            mem.stat(&vp("mem:///d/sub/b.txt")).await.is_err(),
            "el primer hijo del post-orden se borró"
        );
        assert!(
            mem.stat(&vp("mem:///d/sub")).await.is_ok(),
            "y el árbol NO llegó a caer entero: esto es media destrucción"
        );

        // Y eso está en el journal, que es lo único que #186 pide.
        assert_eq!(
            recorder.entries.lock().expect("lock").as_slice(),
            [Anotada {
                op: "removed",
                path: "mem:///d/sub".to_owned(),
                reversal: Reversal::Irreversible,
                reversal_ref: None,
            }],
            "un árbol a medio destruir es un estado real, y la regla dura 4 no lo exime"
        );
    }

    /// Y su gemela, que es la que impide que el arreglo se pase de largo: una
    /// cancelación que llega ANTES del primer borrado no deja fila.
    ///
    /// Cero nodos quitados es «no se llegó a tocar el árbol», y anotarlo diría
    /// que se destruyó algo que sigue entero — un `removed` irreversible sobre
    /// un directorio intacto es peor que no anotar nada.
    #[tokio::test]
    async fn un_borrado_cancelado_antes_de_tocar_nada_no_deja_fila() {
        let mem = Arc::new(MemProvider::with_flags(CapabilityFlags::CASE_SENSITIVE));
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d/sub")).await.expect("mkdir");
        write(&mem, "mem:///d/sub/a.txt", b"uno").await;
        let entry = mem.stat(&vp("mem:///d/sub")).await.expect("stat");

        let t = SyncTargets {
            delete_mode: norte_proto::DeleteMode::Permanent,
            ..targets(&mem)
        };
        let recorder = Recorder::default();
        let cancel = CancellationToken::new();
        cancel.cancel();
        // Por `delete_tree` y no por `destroy_tree`: el recorder solo llega al
        // primero, así que afirmarlo sobre el segundo era una aserción que no
        // podía fallar.
        let err = delete_tree(
            &t,
            &SpoolStep {
                step: step(
                    SyncStepKind::DeleteTree,
                    "sub",
                    Some(StepReversal::Irreversible),
                ),
                dest: Some(DestWitness::of(&entry)),
            },
            &recorder,
            &vp("mem:///d/sub"),
            &ctx(cancel),
        )
        .await
        .expect_err("cancelado");
        assert!(matches!(err, StepError::Fatal(Error::Cancelled)), "{err:?}");
        assert!(
            mem.stat(&vp("mem:///d/sub/a.txt")).await.is_ok(),
            "el árbol sigue entero"
        );
        assert!(
            recorder.entries.lock().expect("lock").is_empty(),
            "y por tanto no hay nada que anotar: un `removed` irreversible sobre un \
             directorio intacto sería peor que el silencio"
        );
    }

    /// **#160, y la razón de que exista [`Unrecorded`].** Cuando la fila no
    /// llega Y la compensación tampoco, el error que sube NOMBRA lo enterrado y
    /// su destino en la papelera — y para la Task antes del paso siguiente.
    ///
    /// Antes esto era un `tracing::error!` y un `Fatal(Io)` pelado: el
    /// llamante no podía distinguir «el destino quedó intacto» de «tu fichero
    /// está en la papelera y nadie lo apuntó», y `¿dónde está mi fichero?` no
    /// lo contestaba nadie que no estuviera leyendo el log del daemon en ese
    /// momento. Es lo que se vio en vivo al final de la tarea 13 del plan de
    /// sincronización.
    #[tokio::test]
    async fn un_journal_que_falla_sin_poder_devolver_nombra_lo_enterrado() {
        // Papelera OPACA: `trash()` no dice adónde se llevó el fichero, así que
        // `restore_from` no tiene a qué apuntar y la compensación es imposible.
        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///d/a.txt", b"viejo").await;
        write(&mem, "mem:///d/b.txt", b"tambien viejo").await;
        write(&mem, "mem:///s/a.txt", b"nuevo").await;
        write(&mem, "mem:///s/b.txt", b"nuevo tambien").await;
        let t = targets(&mem);

        // DOS pasos, para poder comprobar que el segundo no llega a correr.
        let pasos = vec![
            Ok(sobrescribe(&mem, "a.txt").await),
            Ok(sobrescribe(&mem, "b.txt").await),
        ];
        let report = Mutex::new(new_report(7, DestTrash::Restorable));
        let err = run(
            t,
            &TrashedFalla,
            futures::stream::iter(pasos),
            &ctx(CancellationToken::new()),
            &report,
        )
        .await
        .expect_err("el journal falló tras enterrar");

        let ApplyError::Unrecorded(u) = err else {
            panic!("el estado tiene que salir tipado, no en una línea de log: {err:?}");
        };
        assert_eq!(u.buried, vp("mem:///d/a.txt"), "nombra lo enterrado");
        assert!(
            matches!(u.source, Error::Io { .. }),
            "y el error que lo causó: {:?}",
            u.source
        );
        assert_eq!(
            read(&mem, "mem:///d/b.txt").await,
            b"tambien viejo",
            "y la Task paró: el paso siguiente no llegó a correr"
        );
    }

    /// #206: un `created` que falla DESPUÉS de un `trashed` que sí quedó deja
    /// un LOTE cuyo undo bloquea, y eso se DICE con la ruta y su sitio en la
    /// papelera dentro del error.
    ///
    /// No se compensa, y esa es la decisión: devolverlo pediría borrar la
    /// copia recién puesta Y desenterrar la vieja, o sea dos mutaciones más
    /// por el camino en el que el journal ya demostró no funcionar — y
    /// ninguna de las dos quedaría registrada tampoco. Lo que sí se puede es
    /// no dejar al operador buscando: sale como `Unrecoverable`, no como un
    /// `Fatal` cualquiera, que es la diferencia entre «falló» y «tu fichero
    /// está AQUÍ».
    #[tokio::test]
    async fn un_created_que_falla_tras_enterrar_dice_donde_quedo_todo() {
        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///s/a.txt", b"nuevo").await;
        write(&mem, "mem:///d/a.txt", b"viejo").await;
        let t = targets(&mem);
        let record = SpoolStep {
            step: step(
                SyncStepKind::Overwrite,
                "a.txt",
                Some(StepReversal::RestoreTrash),
            ),
            dest: Some(DestWitness::of(
                &mem.stat(&vp("mem:///d/a.txt")).await.expect("stat"),
            )),
        };

        let err = super::overwrite(
            &t,
            &record,
            &CreatedFalla,
            &vp("mem:///d/a.txt"),
            &ctx(CancellationToken::new()),
        )
        .await
        .expect_err("la fila del `created` no llegó");
        let StepError::Unrecoverable(u) = err else {
            panic!("el lote quedó sin poder deshacerse: eso es un ESTADO, no un fallo: {err:?}");
        };
        assert_eq!(
            u.buried,
            vp("mem:///d/a.txt"),
            "el error nombra QUÉ quedó sin poder deshacerse"
        );
        assert_eq!(
            u.at, None,
            "y adónde fue, si la papelera lo nombra: ésta no (mismo `None` que \
             el brazo opaco de #160)"
        );
        // Y el árbol quedó como quedó: el nuevo puesto, el viejo enterrado.
        assert_eq!(read(&mem, "mem:///d/a.txt").await, b"nuevo");
    }

    /// #160, el otro brazo: una papelera "vanish" (macOS/Windows, `Opaque`) no
    /// nombra lo que se llevó — `buried` llega `None` y no hay adónde apuntar
    /// `restore_from`. La compensación no es posible, así que el paso sale como
    /// [`StepError::Unrecoverable`] y no como un `Fatal` cualquiera: el árbol NO
    /// es el que era, y lo enterrado viaja en el error para que alguien pueda
    /// decirlo. Sin destino de papelera que ofrecer, eso sí — es justo lo que
    /// esta papelera no sabe.
    #[tokio::test]
    async fn una_papelera_opaca_no_compensa_pero_sigue_fallando_alto() {
        let mem = Arc::new(MemProvider::new());
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///d/a.txt", b"viejo").await;
        let t = targets(&mem);

        let err = super::bury(
            &t,
            &TrashedFalla,
            &vp("mem:///d/a.txt"),
            1,
            &ctx(CancellationToken::new()),
        )
        .await
        .expect_err("el journal falló");
        let StepError::Unrecoverable(u) = err else {
            panic!("sin compensación posible el estado es OTRO, y sale tipado: {err:?}");
        };
        assert_eq!(u.buried, vp("mem:///d/a.txt"));
        assert!(
            u.at.is_none(),
            "una papelera opaca no dice adónde se lo llevó"
        );

        assert!(
            mem.stat(&vp("mem:///d/a.txt")).await.is_err(),
            "sin `reversal_ref` no hay compensación posible: el fichero sigue \
             fuera de su sitio, y eso lo dice el log, no un `stat` que vuelva a \
             encontrarlo"
        );
    }
}
