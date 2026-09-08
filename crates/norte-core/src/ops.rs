//! Operaciones compuestas del core (spec §5): copy/move/delete sobre el
//! contrato `Provider`. Los providers hacen operaciones simples; AQUÍ viven
//! la recursión, las políticas de colisión y symlinks (ADR 0005), los
//! reintentos con backoff y el chequeo de cancelación por chunk (regla 3).

use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use norte_proto::SymlinkPolicy;
use norte_proto::{
    CollisionPolicy, ConflictKind, DeleteMode, Entry, EntryKind, Error, Segment, VPath,
    VerifyPolicy,
};
use norte_vfs::{Provider, SymlinkKind};
use tokio_util::sync::CancellationToken;

use norte_vfs::{FollowLinks, NodeId};

use crate::engine::TransferOptions;
use crate::observer::{Mutation, MutationObserver};
use crate::scheduler::TaskCtx;

/// Reintentos máximos ante errores transitorios (ADR 0005).
const MAX_RETRIES: u32 = 3;
/// Base del backoff exponencial: 100 ms · 2^n, determinista (sin jitter).
const BACKOFF_BASE_MS: u64 = 100;

/// A DÓNDE escribe una operación, y por qué camino.
///
/// La ruta REAL sigue estando siempre (`path`): es la que va al journal, a la
/// barra de progreso y al texto de los errores, y es la que se lee para
/// desambiguar un reintento. Lo que cambia es cómo se ESCRIBE:
///
/// - sin raíz confinada, contra el provider y por ruta absoluta, que es el
///   comportamiento de siempre;
/// - con ella ([`norte_vfs::ConfinedRoot`]), por segmentos relativos, y
///   entonces un componente intermedio que sea un symlink hacia fuera no puede
///   desviar la escritura (#164, ADR 0054).
///
/// Se construye por DESTINO —no por operación— porque el relativo tiene que
/// corresponder a la ruta final, y la política de colisiones puede haberla
/// cambiado de nombre antes de que nadie escriba nada.
pub(crate) struct Dest<'a> {
    provider: &'a dyn Provider,
    path: VPath,
    /// La raíz y el relativo bajo ella, cuando el destino sabe confinarse.
    confined: Option<(&'a dyn norte_vfs::ConfinedRoot, Vec<Segment>)>,
}

impl<'a> Dest<'a> {
    /// Un destino sin confinar: ruta absoluta contra el provider.
    pub(crate) fn plain(provider: &'a dyn Provider, path: VPath) -> Self {
        Self {
            provider,
            path,
            confined: None,
        }
    }

    /// Un destino bajo `root`, si la hay. `rel` son los segmentos de `path`
    /// que cuelgan de la raíz; `None` en `root` deja el destino sin confinar,
    /// que es lo que le toca a un backend que no sabe hacerlo.
    pub(crate) fn under(
        provider: &'a dyn Provider,
        root: Option<&'a dyn norte_vfs::ConfinedRoot>,
        rel: Vec<Segment>,
        path: VPath,
    ) -> Self {
        // Un relativo VACÍO es la raíz misma, y ninguna de estas operaciones
        // actúa sobre ella: sin segmento final no hay nombre que crear, así que
        // se degrada al camino de siempre en vez de inventarse uno.
        let confined = root.filter(|_| !rel.is_empty()).map(|r| (r, rel));
        Self {
            provider,
            path,
            confined,
        }
    }

    /// La ruta real. Para journal, progreso, errores y lecturas.
    pub(crate) fn path(&self) -> &VPath {
        &self.path
    }

    /// El provider del destino. Para lo que este handle no cubre —leer,
    /// desambiguar, `copy_native`—, que no es lo que hay que confinar.
    pub(crate) fn provider(&self) -> &'a dyn Provider {
        self.provider
    }

    async fn mkdir(&self) -> Result<(), Error> {
        match &self.confined {
            Some((root, rel)) => root.mkdir(rel).await,
            None => self.provider.mkdir(&self.path).await,
        }
    }

    async fn write(&self) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        match &self.confined {
            Some((root, rel)) => root.write(rel).await,
            None => self.provider.write(&self.path).await,
        }
    }

    /// Como [`Provider::open_resumable`]. Un destino confinado NO reanuda: su
    /// staging es efímero, así que se abre fresco y el caller recopia entero
    /// —ver [`Dest::resumes`], que es lo que impide que un reintento vaya
    /// dejando parciales que nadie va a continuar—.
    async fn open_resumable(&self) -> Result<(Box<dyn norte_vfs::ByteSink>, u64), Error> {
        match &self.confined {
            Some((root, rel)) => root.open_resumable(rel).await,
            None => self.provider.open_resumable(&self.path).await,
        }
    }

    /// ¿Puede este destino continuar un parcial suyo?
    ///
    /// Lo contesta la RAÍZ cuando la hay (#297). Antes era `false` para todo
    /// destino confinado, porque el staging confinado llevaba un nombre
    /// efímero por sink y conservarlo dejaría un `.norte-partial` por intento
    /// que ningún `open_resumable` posterior iba a encontrar. Desde que la raíz
    /// local sabe abrir el staging ESTABLE, esa razón dejó de aplicar — y
    /// mantenerla convertía `ResumePolicy::On` en un no-op justo para el caso
    /// donde reanudar más importa: un fichero grande y solo.
    pub(crate) fn resumes(&self) -> bool {
        match &self.confined {
            Some((root, _)) => root.resumes(),
            None => true,
        }
    }

    async fn symlink(&self, target: &[u8], kind: SymlinkKind) -> Result<(), Error> {
        match &self.confined {
            Some((root, rel)) => root.symlink(rel, target, kind).await,
            None => self.provider.symlink(&self.path, target, kind).await,
        }
    }

    /// El `lstat` del destino, POR EL DESCRIPTOR cuando lo hay (#218).
    ///
    /// La resolución de colisión miraba el destino por ruta, así que con un
    /// componente intermedio sustituido lstateaba un fichero del árbol del
    /// atacante — y era ese el que `Overwrite` decidía borrar.
    async fn stat(&self) -> Result<Entry, Error> {
        match &self.confined {
            Some((root, rel)) => root.stat(rel).await,
            None => self.provider.stat(&self.path).await,
        }
    }

    /// Borra la HOJA del destino, por el descriptor cuando lo hay (#218).
    ///
    /// Sin caída al camino por ruta: una raíz que dice no saber borrar
    /// confinado devuelve [`Error::Unsupported`] y el llamante RECHAZA la
    /// política. Caerse a la ruta sería reabrir el agujero justo en el caso
    /// que este método existe para cerrar, y encima en silencio.
    async fn remove(&self, cancel: &CancellationToken) -> Result<(), Error> {
        match &self.confined {
            // El MISMO bucle que el camino por ruta, y no un `with_retry`
            // pelado: sin él, un `unlinkat` que sufre un transitorio y luego
            // contesta `ENOENT` sale como `NotFound` duro y mata el
            // `Overwrite`, en vez de contar como hecho — que es justo el
            // agujero que #186 documentó por la otra puerta.
            Some((root, rel)) => bucle_de_borrado(|| root.remove(rel), cancel)
                .await
                .map_err(|(e, _)| e),
            None => remove_retrying(self.provider, &self.path, cancel).await,
        }
    }

    /// El digest del parcial de este destino, por el descriptor cuando lo hay.
    async fn partial_digest(&self, len: u64) -> Result<Option<[u8; 32]>, Error> {
        match &self.confined {
            Some((root, rel)) => root.partial_digest(rel, len).await,
            None => self.provider.partial_digest(&self.path, len).await,
        }
    }

    /// Borra este destino DICIENDO su clase, y contando la ambigüedad (#296).
    ///
    /// Un borrado en post-orden llega a directorios y a hojas, y `unlinkat`
    /// necesita saber cuál es: son dos efectos distintos, y confundirlos es
    /// como se borra un árbol creyendo que se borra un fichero. Por ruta la
    /// distinción no hace falta —`Provider::remove` la hace por dentro— así
    /// que esa rama es la de siempre.
    pub(crate) async fn remove_kind(
        &self,
        es_dir: bool,
        cancel: &CancellationToken,
    ) -> Result<(), (Error, Ambiguity)> {
        match &self.confined {
            Some((root, rel)) => {
                if es_dir {
                    bucle_de_borrado(|| root.rmdir(rel), cancel).await
                } else {
                    bucle_de_borrado(|| root.remove(rel), cancel).await
                }
            }
            None => remove_retrying_amb(self.provider, &self.path, cancel).await,
        }
    }
}

/// El destino de una operación RECURSIVA: su provider, su raíz confinada si la
/// hay, y la ruta base de la que cuelga todo lo que va a escribir.
///
/// Va junto porque los tres se necesitan juntos en cada paso, y porque así la
/// raíz se abre UNA vez por operación en vez de una por hoja (#164).
pub(crate) struct Destination<'a> {
    provider: &'a dyn Provider,
    root: Option<&'a dyn norte_vfs::ConfinedRoot>,
    base: &'a VPath,
}

impl<'a> Destination<'a> {
    /// Con la raíz que [`open_dest_root`] haya podido abrir.
    pub(crate) fn new(
        provider: &'a dyn Provider,
        root: Option<&'a dyn norte_vfs::ConfinedRoot>,
        base: &'a VPath,
    ) -> Self {
        Self {
            provider,
            root,
            base,
        }
    }

    /// Sin raíz: el destino no sabe confinarse, o no hay directorio del que
    /// colgar. Cada ruta va tal cual, que es lo de siempre.
    pub(crate) fn unconfined(provider: &'a dyn Provider, base: &'a VPath) -> Self {
        Self::new(provider, None, base)
    }

    /// El provider, para lo que no se confina (leer, desambiguar, colisiones).
    pub(crate) fn provider(&self) -> &'a dyn Provider {
        self.provider
    }

    /// El destino de UNA ruta bajo esta base, confinado si cae dentro de ella.
    pub(crate) fn at(&self, path: VPath) -> Dest<'a> {
        match self.root.zip(rel_under(self.base, &path)) {
            Some((root, rel)) => Dest::under(self.provider, Some(root), rel, path),
            None => Dest::plain(self.provider, path),
        }
    }
}

/// ¿La raíz que se abrió es el nodo que hay en `path` AHORA MISMO?
///
/// El confinamiento ancla en un descriptor, pero el descriptor se consigue
/// abriendo una ruta, y esa apertura resuelve symlinks como cualquier otra —a
/// propósito: un `~/copias -> /mnt/disco/copias` es un destino legítimo—. Entre
/// crear el directorio y abrirlo cabe un cambiazo (`rmdir` + symlink), y
/// entonces todo lo que venga después va perfectamente confinado AL ÁRBOL
/// EQUIVOCADO, sin que nada chirríe: lo encontró la revisión de seguridad de
/// esta fase.
///
/// Lo que lo cierra es comparar identidades: el `stat` es un `lstat`, así que
/// un `path` sustituido por un symlink sale con la identidad del ENLACE y no
/// con la del directorio abierto, y no casan. Restaurar el directorio de verdad
/// tampoco cuela: sería otro inodo.
///
/// Un backend sin identidad estable (`Ok(None)` en cualquiera de los dos lados)
/// no tiene nada que comparar y pasa: es la misma degradación honesta de
/// siempre, y `node_id` ya la documenta.
async fn same_root_or_fail(
    dst: &dyn Provider,
    root: &dyn norte_vfs::ConfinedRoot,
    path: &VPath,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let abierta = root.root_id().await?;
    let en_ruta = with_retry(cancel, || dst.node_id(path, FollowLinks::No).boxed()).await?;
    let (Some(abierta), Some(en_ruta)) = (abierta, en_ruta) else {
        return Ok(());
    };
    if abierta == en_ruta {
        return Ok(());
    }
    tracing::error!(
        dest = %crate::engine::span_path(path),
        "la raíz de destino cambió entre crearla y abrirla: se para en vez de escribir \
         confinadamente en otro árbol (#164)"
    );
    Err(Error::Conflict {
        conflict: ConflictKind::EscapesRoot,
    })
}

/// Los segmentos de `path` que cuelgan de `root`, o `None` si `path` no está
/// bajo `root` — en cuyo caso no hay relativo que dar y quien pregunta se
/// queda sin confinar, que es lo honesto.
pub(crate) fn rel_under(root: &VPath, path: &VPath) -> Option<Vec<Segment>> {
    if path.scheme() != root.scheme() || path.authority() != root.authority() {
        return None;
    }
    let prefix: Vec<&[u8]> = root.segments().collect();
    let full: Vec<&[u8]> = path.segments().collect();
    if full.len() < prefix.len() || full[..prefix.len()] != prefix[..] {
        return None;
    }
    full[prefix.len()..]
        .iter()
        .map(|s| Segment::new(s.to_vec()).ok())
        .collect()
}

/// La misma comprobación de ancla que hace [`open_leaf_root`], pero por RUTA y
/// sobre el PADRE de `to` (#295), para los caminos que CREAN su destino.
///
/// Un árbol se copia creando `to` y confinando debajo, así que en el momento
/// de comprobar no hay descriptor que preguntar: lo que el humano listó es el
/// directorio donde el árbol va a caer, o sea el padre. La comprobación es por
/// ruta y por tanto tiene su propia ventana —minúscula, entre preguntar y
/// crear—, pero el caso que este ancla existe para cerrar es el enlace **ya
/// plantado** antes de que nadie mirara, y ese lo caza igual.
async fn anchor_parent_or_fail(
    dst: &dyn Provider,
    to: &VPath,
    anchor: Option<&norte_proto::DirAnchor>,
    task_id: u64,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let (Some(anchor), Some(dir)) = (anchor, to.parent()) else {
        return Ok(());
    };
    let observado = with_retry(cancel, || {
        dst.node_id(&dir, norte_vfs::FollowLinks::Yes).boxed()
    })
    .await?;
    match observado {
        Some(id) if crate::anchor::casa(anchor, id) => Ok(()),
        Some(_) => {
            tracing::warn!(
                task_id,
                dest = %crate::engine::span_path(&dir),
                "el directorio destino ya no es el nodo que el cliente listó: se rehúsa \
                 escribir (#295)"
            );
            Err(Error::Conflict {
                conflict: norte_proto::ConflictKind::EscapesRoot,
            })
        }
        None => {
            tracing::debug!(
                task_id,
                dest = %crate::engine::span_path(&dir),
                "vino un ancla y este destino no sabe dar identidad: no se comprueba (#295)"
            );
            Ok(())
        }
    }
}

/// El destino de una hoja, con raíz si `open_leaf_root` pudo darla.
fn destino_de_hoja<'a>(
    dst: &'a dyn Provider,
    dir: Option<&'a VPath>,
    raiz: Option<&'a dyn norte_vfs::ConfinedRoot>,
    to: &'a VPath,
) -> Destination<'a> {
    match dir {
        Some(dir) => Destination::new(dst, raiz, dir),
        None => Destination::unconfined(dst, to),
    }
}

/// La raíz de una transferencia de UNA hoja: su DIRECTORIO destino (#219).
///
/// Devuelve `(directorio, raíz)`; `None` en la raíz = no hay dónde anclarse y
/// el llamante sigue por ruta, que es lo de siempre.
///
/// # Por qué el directorio y no el scope de policy
///
/// La issue proponía el scope. No sirve, y la razón es simple: **un `fs.copy`
/// humano no tiene scope** — los scopes existen solo para sesiones de agente,
/// así que para el llamante más común no habría nada que abrir.
///
/// Lo que el humano SÍ aprobó es el directorio de destino: es lo que el panel
/// enseñaba, de lo que `pedir_transferencia` compone `to`, y lo que el diálogo
/// nombra en su propio campo.
///
/// # Qué cierra, y qué no
///
/// Cierra el CAMBIAZO, que es el modelo de #164: entre que se mira el
/// directorio y que se escribe en él, alguien lo sustituye por un enlace hacia
/// otro árbol. La comprobación de identidad lo caza —el nodo abierto deja de
/// ser el que se miró— y, una vez abierto, el staging y su publicación van los
/// dos por el descriptor: `dest/sub` se resuelve UNA vez en lugar de tres
/// —crear el staging, renombrar, y otra vez por cada uno de los tres
/// reintentos de 100/200/400 ms.
///
/// Un enlace que YA estaba cuando el core miró por primera vez no lo cierra
/// ese confinamiento, y no puede: desde aquí un `~/copias -> /mnt/disco/copias`
/// legítimo y un enlace hostil son indistinguibles, porque los dos resuelven a
/// otro sitio. Lo que los separa es la identidad que se observó al APROBAR —el
/// listado que el humano miró—, y desde #295 esa identidad VIAJA con la
/// petición: es el `anchor` de esta función (ADR 0073). Sin ancla, un
/// directorio destino que es un enlace se copia por ruta, exactamente como
/// antes de #219, y se dice en el log.
///
/// Lo que sigue sin cerrarse es la sustitución de un componente INTERMEDIO del
/// directorio: la raíz se consigue ABRIENDO una ruta, así que esa primera
/// resolución es por ruta por definición, y el ancla nombra el directorio
/// final, no los de encima. Es el mismo residuo que una copia recursiva acepta
/// para su propio destino.
async fn open_leaf_root(
    dst: &dyn Provider,
    to: &VPath,
    anchor: Option<&norte_proto::DirAnchor>,
    task_id: u64,
    cancel: &CancellationToken,
) -> Result<(Option<VPath>, Option<Box<dyn norte_vfs::ConfinedRoot>>), Error> {
    // Una hoja cuyo destino es una RAÍZ no tiene directorio del que colgar.
    let Some(dir) = to.parent() else {
        return Ok((None, None));
    };
    // ¿Es el directorio destino un ENLACE? Se pregunta con las dos
    // resoluciones del mismo path: si `lstat` y `stat` dan el mismo nodo, el
    // último componente es un directorio de verdad; si difieren, es un enlace.
    //
    // Y si lo es, NO se confina. No es una concesión: es que ahí no hay nada
    // que comprobar. `~/copias -> /mnt/disco/copias` es un destino legítimo y
    // corriente —en macOS lo son `/tmp`, `/var` y `/etc`; en un Linux con
    // usrmerge, `/bin` y `/lib`—, y un enlace hostil recién plantado se ve
    // EXACTAMENTE igual desde aquí: los dos resuelven a otro sitio. Rechazar
    // los dos rompería la copia a media distribución para no cerrar nada;
    // aceptar los dos con la comprobación apagada sería lo de siempre, que es
    // lo que se hace, diciéndolo.
    let (por_enlace, id_directo) = (
        with_retry(cancel, || dst.node_id(&dir, FollowLinks::Yes).boxed()).await?,
        with_retry(cancel, || dst.node_id(&dir, FollowLinks::No).boxed()).await?,
    );
    let es_enlace = matches!((por_enlace, id_directo), (Some(a), Some(b)) if a != b);
    let root = match dst.open_root(&dir).await {
        Ok(r) => Some(r),
        // «No sé confinar»: el único caso que degrada, igual que en una copia
        // recursiva y por el mismo motivo (ADR 0054).
        Err(Error::Unsupported) => {
            // `debug!` y no `warn!`, al revés que en una copia recursiva: eso
            // avisa una vez por OPERACIÓN y esto una vez por FICHERO, y el
            // lote de la ventana encola una Task por marca. Cinco mil copias a
            // un SFTP, a un bucket o desde Windows —donde `open_root` es el
            // default `Unsupported`— escribirían cinco mil líneas idénticas, y
            // un aviso que se repite cinco mil veces no es un aviso.
            tracing::debug!(
                task_id,
                dest = %crate::engine::span_path(&dir),
                "el destino no sabe confinar sus escrituras: un symlink intermedio podría \
                 desviar esta transferencia fuera de su directorio (#219)"
            );
            None
        }
        // El directorio destino no está. Es la respuesta, no un fallo del
        // confinamiento: la escritura iba a fallar igual, y decirlo aquí evita
        // el mensaje alarmista del brazo de abajo.
        Err(e @ Error::NotFound) => return Err(e),
        Err(e) => {
            tracing::error!(
                task_id,
                dest = %crate::engine::span_path(&dir),
                error = %e,
                "no se pudo abrir el directorio destino confinado y el destino declaró que \
                 sabía: se para en vez de escribir por ruta sin decirlo (#219)"
            );
            return Err(e);
        }
    };
    // La comprobación de identidad SOLO si el último componente es un
    // directorio de verdad. Sobre un enlace compara el nodo del ENLACE con el
    // del directorio al que apunta, que nunca casan: rechazaría un
    // `~/copias -> /mnt/disco/copias` legítimo, y en macOS un `/tmp`.
    //
    // Se confina igual, sin ella: el descriptor sigue valiendo lo que vale
    // —una resolución en vez de tres más los reintentos— y lo que se pierde es
    // solo la comprobación, que sobre un enlace no podía decir nada.
    if let Some(root) = root.as_deref()
        && !es_enlace
    {
        same_root_or_fail(dst, root, &dir, cancel).await?;
    }
    // El ANCLA (#295, ADR 0073): ¿este directorio sigue siendo el nodo que el
    // humano estaba mirando cuando aprobó?
    //
    // Es lo ÚNICO que separa un `dest/sub -> /etc` plantado antes de que nadie
    // mirase de un `~/copias -> /mnt/disco/copias` legítimo, porque los dos
    // resuelven a otro sitio y desde aquí se ven igual. Y se contesta contra
    // el DESCRIPTOR ya abierto —`root_id`—, no volviendo a resolver la ruta:
    // la raíz que se comprueba es exactamente la raíz por la que se va a
    // escribir, sin ventana en medio. Sobre un enlace vale igual, que es la
    // diferencia con la comprobación de arriba: al ancla no le importa por qué
    // nombre se llegó, solo A QUÉ NODO.
    if let Some(anchor) = anchor {
        let observado = match root.as_deref() {
            Some(root) => root.root_id().await?,
            // Sin raíz confinada (el destino no sabe) queda la ruta, que es
            // peor y se dice: sigue cerrando el enlace YA plantado, que es el
            // caso del issue, y no la sustitución de después.
            None => {
                with_retry(cancel, || {
                    dst.node_id(&dir, norte_vfs::FollowLinks::Yes).boxed()
                })
                .await?
            }
        };
        match observado {
            Some(id) if crate::anchor::casa(anchor, id) => {}
            Some(_) => {
                tracing::warn!(
                    task_id,
                    dest = %crate::engine::span_path(&dir),
                    "el directorio destino ya no es el nodo que el cliente listó: se rehúsa \
                     escribir (#295)"
                );
                return Err(Error::Conflict {
                    conflict: norte_proto::ConflictKind::EscapesRoot,
                });
            }
            // El destino no sabe dar identidad de nodo. No se puede comprobar
            // y NO se inventa un veredicto: se sigue, como sin ancla. Un
            // cliente contra un destino así tampoco recibió ancla que mandar,
            // así que llegar aquí es un cliente que se la inventó o un destino
            // que cambió de opinión.
            None => tracing::debug!(
                task_id,
                dest = %crate::engine::span_path(&dir),
                "vino un ancla y este destino no sabe dar identidad: no se comprueba (#295)"
            ),
        }
    }
    if es_enlace {
        tracing::debug!(
            task_id,
            dest = %crate::engine::span_path(&dir),
            "el directorio destino es un enlace: se confina, pero su identidad no se \
             puede comprobar (#219)"
        );
    }
    Ok((Some(dir), root))
}

/// Abre la raíz confinada de `root`, o dice por qué no la hay.
///
/// **Un destino que no sabe confinarse no se rechaza: se avisa y se sigue.**
/// Lo contrario dejaría sin copiar hacia un SFTP, un bucket o un Windows por
/// una defensa que esos destinos no pueden dar, y el agujero que cierra
/// necesita que alguien plante un symlink en el momento justo. El aviso va UNA
/// vez por operación, no una por paso.
pub(crate) async fn open_dest_root(
    dst: &dyn Provider,
    root: &VPath,
    task_id: u64,
) -> Result<Option<Box<dyn norte_vfs::ConfinedRoot>>, Error> {
    match dst.open_root(root).await {
        Ok(r) => Ok(Some(r)),
        // «No sé confinar»: el único caso en el que se degrada, y es el que el
        // ADR 0054 razona. El backend no puede, y no copiar hacia él por eso
        // sería peor.
        Err(Error::Unsupported) => {
            tracing::warn!(
                task_id,
                dest = %crate::engine::span_path(root),
                "el destino no sabe confinar sus escrituras: un symlink intermedio podría \
                 desviar esta operación fuera de su raíz (#164)"
            );
            Ok(None)
        }
        // **Cualquier otro fallo PARA la operación, y esto es un cambio de la
        // revisión de seguridad de esta fase.** Degradar aquí era el
        // comportamiento de antes, sí, pero antes no había ninguna promesa que
        // romper: hoy `capabilities_at` ya le dijo al humano que este destino
        // confina —el diálogo se lo dijo CALLANDO— y seguir por ruta sin
        // decírselo convierte «haz fallar el `open` una vez» en la llave que
        // reabre #164 para toda la operación. Un `ENOTDIR` de un cambiazo
        // momentáneo, un `EMFILE`, un `EACCES` transitorio: todos valían.
        //
        // El destino que NO puede confinar ya está cubierto por el brazo de
        // arriba, así que aquí solo cae quien dijo que podía y falló.
        Err(e) => {
            tracing::error!(
                task_id,
                dest = %crate::engine::span_path(root),
                error = %e,
                "no se pudo abrir la raíz de destino confinada y el destino declaró que sabía: \
                 se para en vez de escribir por ruta sin decirlo (#164)"
            );
            Err(e)
        }
    }
}

/// ¿Merece reintento? Solo lo explícitamente transitorio; el resto de
/// errores JAMÁS se reintenta (repetir un `Conflict` no lo arregla).
fn is_transient(e: &Error) -> bool {
    matches!(
        e,
        Error::ProviderUnavailable { retryable: true } | Error::Io { retryable: true }
    )
}

/// Espera el backoff del intento `attempt`, cancelable DURANTE la espera
/// (regla 3: la cancelación jamás espera al backoff).
async fn backoff_or_cancel(cancel: &CancellationToken, attempt: u32) -> Result<(), Error> {
    let delay = std::time::Duration::from_millis(BACKOFF_BASE_MS << attempt);
    tokio::select! {
        () = cancel.cancelled() => Err(Error::Cancelled),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

/// Reintenta una operación puntual IDEMPOTENTE (`stat`/`read`/`read_link`/
/// `node_id`) ante errores transitorios: hasta [`MAX_RETRIES`] reintentos
/// con backoff exponencial cancelable.
///
/// Las MUTACIONES no pasan por aquí: tras un fallo transitorio su efecto
/// pudo haberse aplicado (timeout post-commit en remotos) y reintentarlas a
/// ciegas duplicaría efectos o mentiría al journal — usan los wrappers
/// `*_retrying` con desambiguación por operación (issue #17). El COMMIT del
/// write ambiguo también se desambigua (#32.1, en [`copy_file`]: presencia +
/// tamaño del destino). Deuda restante (#32): el `Created` del mkdir ambiguo
/// (exige pre-stat, +1 stat/dir — solo deja un dir vacío de más, jamás
/// pérdida) y el retry de `trash` (op del OS; el `dest` del journal se
/// perdería en el reintento).
pub(crate) async fn with_retry<'a, T: 'a>(
    cancel: &CancellationToken,
    mut op: impl FnMut() -> futures::future::BoxFuture<'a, Result<T, Error>>,
) -> Result<T, Error> {
    let mut attempt = 0u32;
    loop {
        match op().await {
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) => {
                backoff_or_cancel(cancel, attempt).await?;
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// Qué se sabe del EFECTO de una mutación que falló.
///
/// Una mutación puntual que falla tras un transitorio deja el otro extremo en
/// duda: el `remove` pudo llegar y perderse la respuesta (el timeout
/// post-commit de un remoto, issue #17). Los `*_retrying` de aquí llevan esa
/// duda desde siempre; lo que no hacían era CONTARLA, y quien la necesita es el
/// llamante que decide si journalizar.
///
/// El árbol entero resuelve la duda hacia «lo hicimos» —así lo hacen ya los
/// brazos `Err(NotFound) if ambiguous` de aquí abajo y la re-lista de
/// `mkdir_retrying`— porque el error de esa elección es una fila de más, y el
/// de la contraria es un efecto sin fila.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ambiguity {
    /// El fallo llegó antes de que nada cambiara: el efecto NO se aplicó.
    NotApplied,
    /// Hubo un transitorio por medio: el efecto PUDO quedar aplicado.
    MaybeApplied,
}

/// `remove` con reintentos y desambiguación (issue #17): tras un fallo
/// transitorio el efecto pudo aplicarse — `NotFound` en el reintento
/// significa "ya no está", que ES el estado que el remove perseguía (lo
/// borrase nuestra primera aplicación o no, el journal registra un único
/// `Removed` verdadero).
pub(crate) async fn remove_retrying(
    p: &dyn Provider,
    path: &VPath,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    remove_retrying_amb(p, path, cancel)
        .await
        .map_err(|(e, _)| e)
}

/// Como [`remove_retrying`], DICIENDO si el efecto quedó en duda.
///
/// Lo pide el borrado de árboles de `sync::exec` (#186): un `remove` que falla
/// tras un transitorio pudo haber llegado al bucket, y si era el primer nodo
/// del post-orden, contarlo como «no quitado» deja el árbol dentado y sin fila
/// de journal — que es exactamente el agujero que #186 cerró por la otra
/// puerta.
pub(crate) async fn remove_retrying_amb(
    p: &dyn Provider,
    path: &VPath,
    cancel: &CancellationToken,
) -> Result<(), (Error, Ambiguity)> {
    bucle_de_borrado(|| p.remove(path), cancel).await
}

/// El bucle de [`remove_retrying_amb`], sobre CUALQUIER forma de borrar.
///
/// Existe porque desde #218 hay dos: por ruta y por el descriptor de una raíz
/// confinada. Las dos necesitan lo mismo —comprobar la cancelación antes del
/// primer intento, sembrar la duda al ver un transitorio, y tratar un
/// `NotFound` posterior como «ya no está», que ES el estado que el borrado
/// perseguía— y tenerlo dos veces es tenerlo de dos maneras en cuanto una de
/// las dos se toque.
async fn bucle_de_borrado<F, Fut>(
    mut borra: F,
    cancel: &CancellationToken,
) -> Result<(), (Error, Ambiguity)>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    let mut attempt = 0u32;
    let mut ambiguous = false;
    // La duda, una vez sembrada, viaja con TODA salida de error — incluida la
    // cancelación que corta el backoff, que es la que #186 llegó a producir en
    // vivo (el usuario ve el parón y pulsa Ctrl+K).
    let dudoso = |ambiguous: bool| {
        if ambiguous {
            Ambiguity::MaybeApplied
        } else {
            Ambiguity::NotApplied
        }
    };
    loop {
        if cancel.is_cancelled() {
            return Err((Error::Cancelled, dudoso(ambiguous)));
        }
        match borra().await {
            Ok(()) => return Ok(()),
            Err(Error::NotFound) if ambiguous => return Ok(()),
            // La duda se siembra en cuanto se VE un transitorio, no solo cuando
            // se decide reintentar: el efecto pudo quedar aplicado en el otro
            // extremo con independencia de lo que hagamos después, y agotar los
            // reintentos y cancelar son precisamente las dos salidas por las que
            // #186 se escapaba.
            Err(e) if is_transient(&e) => {
                ambiguous = true;
                if attempt >= MAX_RETRIES || cancel.is_cancelled() {
                    return Err((e, Ambiguity::MaybeApplied));
                }
                backoff_or_cancel(cancel, attempt)
                    .await
                    .map_err(|e| (e, Ambiguity::MaybeApplied))?;
                attempt += 1;
            }
            Err(e) => return Err((e, dudoso(ambiguous))),
        }
    }
}

/// `trash` con reintentos y desambiguación (#99). Tras un fallo transitorio el
/// efecto pudo aplicarse. Una papelera que NOMBRA su destino —la lógica, y la
/// freedesktop de `norte-vfs-local` desde la tarea 11b— recupera el payload en
/// el reintento con el MISMO id determinista (`Some`, conserva el
/// `reversal_ref` del undo). Una que no lo nombra (macOS, Windows) no puede: un
/// `NotFound` en el reintento significa "ya no está" (lo trasheó nuestra
/// primera aplicación) → `Ok(None)`, y el undo degrada. Un `NotFound` SIN
/// transitorio previo es la víctima que nunca existió: se propaga.
pub(crate) async fn trash_retrying(
    provider: &dyn Provider,
    path: &VPath,
    id: &norte_vfs::trash::TrashId,
    cancel: &CancellationToken,
) -> Result<Option<VPath>, Error> {
    trash_retrying_amb(provider, path, id, cancel)
        .await
        .map_err(|(e, _)| e)
}

/// Como [`trash_retrying`], DICIENDO si el efecto quedó en duda. Mismo motivo
/// que [`remove_retrying_amb`] (#186).
pub(crate) async fn trash_retrying_amb(
    provider: &dyn Provider,
    path: &VPath,
    id: &norte_vfs::trash::TrashId,
    cancel: &CancellationToken,
) -> Result<Option<VPath>, (Error, Ambiguity)> {
    let mut attempt = 0u32;
    let mut ambiguous = false;
    let dudoso = |ambiguous: bool| {
        if ambiguous {
            Ambiguity::MaybeApplied
        } else {
            Ambiguity::NotApplied
        }
    };
    loop {
        if cancel.is_cancelled() {
            return Err((Error::Cancelled, dudoso(ambiguous)));
        }
        match provider.trash(path, id).await {
            Ok(dest) => return Ok(dest),
            Err(Error::NotFound) if ambiguous => return Ok(None),
            // La duda se siembra en cuanto se VE un transitorio, no solo cuando
            // se decide reintentar: el efecto pudo quedar aplicado en el otro
            // extremo con independencia de lo que hagamos después, y agotar los
            // reintentos y cancelar son precisamente las dos salidas por las que
            // #186 se escapaba.
            Err(e) if is_transient(&e) => {
                ambiguous = true;
                if attempt >= MAX_RETRIES || cancel.is_cancelled() {
                    return Err((e, Ambiguity::MaybeApplied));
                }
                backoff_or_cancel(cancel, attempt)
                    .await
                    .map_err(|e| (e, Ambiguity::MaybeApplied))?;
                attempt += 1;
            }
            Err(e) => return Err((e, dudoso(ambiguous))),
        }
    }
}

/// `mkdir` con reintentos y desambiguación (#32.2). CONTRATO: el caller ya
/// verificó que el destino NO preexistía (pre-stat de [`ensure_dir`]) — con
/// esa garantía, un `Conflict` tras un fallo transitorio es CANDIDATO a
/// nuestra primera aplicación y se VERIFICA listándolo (#104 review
/// MAJOR-1, mismo criterio que `symlink_retrying`): un dir que acabamos de
/// crear no puede tener contenido — vacío = nuestro (`Ok`, con su `Created`
/// para el journal); con contenido = de un tercero, `Conflict` fail-safe.
/// Sin la verificación, un `Created` falsamente reclamado haría que el undo
/// (M3-2 enruta `Created` por PAPELERA, ya no `remove`) mandara a trash el
/// dir AJENO con su contenido. Ventana residual honesta: un tercero que
/// crea el dir y AÚN no metió nada pasa por nuestro (indistinguible sin
/// node-id); su undo trashea un dir VACÍO ajeno — recuperable y acotado.
/// Un `Conflict` SIN transitorio previo sí es colisión real (carrera
/// externa): se propaga sin listar y la política del caller decide.
pub(crate) async fn mkdir_retrying(
    dest: &Dest<'_>,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut attempt = 0u32;
    let mut ambiguous = false;
    let (p, path) = (dest.provider(), dest.path());
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match dest.mkdir().await {
            // Un escape es un VEREDICTO, no un «quizá se aplicó». Tiene que
            // salir antes que la desambiguación: esa lee por RUTA, siguiendo el
            // mismo componente hostil que acaba de producirlo, y un directorio
            // vacío al otro lado se leería como «lo creamos nosotros» — con su
            // `Created` en el journal y un undo que va a la papelera con algo
            // de fuera del árbol.
            Err(
                e @ Error::Conflict {
                    conflict: ConflictKind::EscapesRoot,
                },
            ) => return Err(e),
            Err(e @ Error::Conflict { .. }) if ambiguous => {
                // La desambiguación LEE, y leer no es por lo que este destino
                // se confina: el `list` va por ruta, como siempre.
                let mut stream = with_retry(cancel, || p.list(path).boxed()).await?;
                return match stream.next().await {
                    None => Ok(()),
                    Some(Ok(_)) => Err(e),
                    Some(Err(le)) => Err(le),
                };
            }
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) && !cancel.is_cancelled() => {
                ambiguous = true;
                backoff_or_cancel(cancel, attempt).await?;
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// `symlink` con reintentos y desambiguación (issue #17): un `Conflict`
/// tras fallo transitorio se verifica leyendo el link — si su target son
/// EXACTAMENTE nuestros bytes, es nuestra primera aplicación y cuenta como
/// éxito (un único `Created` para el journal). Target distinto = colisión
/// real.
///
/// Límites documentados: (a) un provider que CANONICALICE el target al
/// releerlo (Windows reconstruye desde el reparse buffer; SFTP exóticos)
/// daría falso negativo → `Conflict` fail-safe con el efecto aplicado y
/// un `Created` perdido para el journal (misma deuda que mkdir, #32);
/// (b) el kind no se verifica — un link preexistente con el MISMO target
/// y otro kind pasaría por nuestro (requiere transitorio + preexistencia
/// exacta; el target manda, jamás hay pérdida).
pub(crate) async fn symlink_retrying(
    dest: &Dest<'_>,
    target: &[u8],
    kind: SymlinkKind,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut attempt = 0u32;
    let mut ambiguous = false;
    let (backend, link) = (dest.provider(), dest.path());
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match dest.symlink(target, kind).await {
            Ok(()) => return Ok(()),
            // Terminal, por lo mismo que en `mkdir_retrying`: el `read_link` de
            // la desambiguación va por ruta y podría contestar que el enlace
            // «ya es nuestro» leyendo uno que está fuera de la raíz.
            Err(
                e @ Error::Conflict {
                    conflict: ConflictKind::EscapesRoot,
                },
            ) => return Err(e),
            Err(e @ Error::Conflict { .. }) if ambiguous => {
                return match with_retry(cancel, || backend.read_link(link).boxed()).await {
                    Ok(bytes) if bytes == target => Ok(()),
                    Err(Error::Cancelled) => Err(Error::Cancelled),
                    _ => Err(e),
                };
            }
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) && !cancel.is_cancelled() => {
                ambiguous = true;
                backoff_or_cancel(cancel, attempt).await?;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// ¿`to` es el MISMO nodo que `from`, o sea el propio fichero con otra
/// ortografía? (#274)
///
/// Identidad **y** ortografía, y hacen falta las dos.
///
/// La identidad sola no basta: `NodeId` es `(dispositivo, inodo)`, así que dos
/// entradas de directorio DISTINTAS que sean hardlinks del mismo fichero dan el
/// mismo id — y `mv a.txt b.txt` con `b.txt` enlazado a `a.txt` no es un cambio
/// de ortografía, es una colisión de verdad a la que le toca su política.
/// `rename_applied` ya documenta ese mismo agujero unas líneas más abajo.
///
/// La ortografía sola tampoco: compara claves plegadas, que es una heurística
/// sobre NOMBRES, y lo que se decide aquí mueve un fichero por encima de otro.
///
/// Así que: mismo directorio, hojas que pliegan a la misma clave bajo el modo
/// que ese directorio usa, y el mismo nodo. La comprobación barata va primero,
/// que es lo que evita un `node_id` de más en cada colisión de un `move`
/// masivo contra un sftp.
///
/// `from_id` viene capturado ANTES del primer intento, como manda #17.
async fn es_la_misma_hoja_con_otra_ortografia(
    src: &dyn Provider,
    from: &VPath,
    to: &VPath,
    from_id: Option<norte_vfs::NodeId>,
    cancel: &CancellationToken,
) -> bool {
    if from == to || from.parent() != to.parent() {
        return false;
    }
    let (Some(a), Some(hoja_from), Some(hoja_to)) = (from_id, from.file_name(), to.file_name())
    else {
        return false;
    };
    let mode = fold_mode_at(to, src).await;
    if mode == norte_encoding::FoldMode::None
        || norte_encoding::name_key(hoja_from.as_bytes(), mode)
            != norte_encoding::name_key(hoja_to.as_bytes(), mode)
    {
        return false;
    }
    matches!(
        with_retry(cancel, || src.node_id(to, FollowLinks::No).boxed()).await,
        Ok(Some(b)) if a == b
    )
}

/// Cambia la ORTOGRAFÍA de un nombre: `Foo.txt → foo.txt` en un volumen que
/// pliega, donde los dos nombres son el mismo nodo (#274).
///
/// En dos pasos y por un nombre intermedio, porque lo único que este provider
/// sabe hacer es renombrar SIN PISAR y el destino «existe» —es el origen—:
/// primero a un nombre que no colisiona con nadie, y de ahí al que se pidió,
/// que para entonces ya está libre. Es lo que hace cualquiera que renombre
/// `README` a `readme` en un Mac.
///
/// El nombre intermedio va por PREFIJO y no empotra la hoja
/// (`.norte-rename-case-<n>`), como los del ejecutor de lotes
/// ([`crate::rename::naming`]): un sufijo sobre una hoja de 250 bytes revienta
/// el límite de 255 del componente —la fixture `name_max_255` del corpus
/// existe por eso— y además le cambiaría la extensión al fichero mientras
/// dura. `n` sube hasta encontrar uno libre, porque un fichero de verdad puede
/// llamarse así.
///
/// # La ventana entre los dos pasos NO es cancelable, a propósito
///
/// Es la misma decisión que tomó `rename::exec`: la cancelación se comprueba
/// ENTRE operaciones, jamás dentro de una. Con el token del task, cancelar
/// entre el paso 1 y el 2 hacía que el paso 2 **y la vuelta atrás** salieran
/// inmediatamente sin intentar nada, dejando el fichero con un nombre que
/// nadie escribió y contestando `Cancelled` — y en este repositorio una task
/// cancelada significa «el árbol está como estaba».
///
/// Si el segundo paso falla se vuelve al nombre de partida. Si ni eso se
/// puede, el error va al log CON la ruta donde quedó el fichero: es lo único
/// que le queda al lector para encontrarlo.
///
/// El journal ve UN `Renamed` de `from` a `to`, que es lo que pasó: el nombre
/// intermedio no existió para nadie más que para estas dos llamadas, y meterlo
/// en el diario haría que deshacer pasara por él. Su deshacer necesita el
/// mismo rodeo, y lo tiene (`undo::rename_por_rodeo`): por identidad, porque
/// en un volumen que pliega el nombre de partida «está ocupado» por el propio
/// fichero.
///
/// El humano aprobó DOS nombres y existieron tres. El tercero cae en el mismo
/// directorio que los otros dos —el gate de política se consultó sobre ese
/// padre— y vive lo que tardan dos renames, pero conviene que esté dicho.
async fn rename_de_ortografia(
    src: &dyn Provider,
    from: &VPath,
    to: &VPath,
    from_id: Option<norte_vfs::NodeId>,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let paso = spelling_detour(src, from, &ctx.cancel).await?;
    rename_retrying(src, from, &paso, from_id, &ctx.cancel).await?;
    // De aquí al final, con un token LIMPIO: ver «la ventana no es cancelable».
    let sin_cancelar = CancellationToken::new();
    if let Err(e) = rename_retrying(src, &paso, to, from_id, &sin_cancelar).await {
        if let Err(vuelta) = rename_retrying(src, &paso, from, from_id, &sin_cancelar).await {
            tracing::error!(
                error = %e,
                vuelta = %vuelta,
                quedo = %crate::engine::span_path(&paso),
                "un cambio de ortografía no pudo terminar ni volver a su nombre"
            );
        }
        return Err(e);
    }
    observer
        .on_mutation(
            &Mutation::Renamed {
                from,
                to,
                batch: None,
            },
            &ctx.actor,
        )
        .await?;
    Ok(())
}

/// El nombre intermedio LIBRE para un cambio de ortografía, en el directorio
/// de `from`.
///
/// Sube `n` hasta que el nombre no exista: un fichero de verdad puede llamarse
/// como un temporal nuestro, y renombrar encima sería borrarlo. El tope es
/// generoso y su agotamiento es un error honesto, no un bucle.
async fn spelling_detour(
    src: &dyn Provider,
    from: &VPath,
    cancel: &CancellationToken,
) -> Result<VPath, Error> {
    for n in 0..1000u32 {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut name = crate::rename::naming::TEMP_PREFIX.to_vec();
        name.extend_from_slice(format!("case-{n}").as_bytes());
        let seg = Segment::new(name).map_err(|_| Error::InvalidPath)?;
        let cand = from.with_file_name(seg).ok_or(Error::InvalidPath)?;
        match with_retry(cancel, || src.stat(&cand).boxed()).await {
            Err(Error::NotFound) => return Ok(cand),
            Ok(_) => {}
            Err(e) => return Err(e),
        }
    }
    Err(Error::Conflict {
        conflict: ConflictKind::Exists,
    })
}

/// `rename` con reintentos y desambiguación (issue #17): tras un fallo
/// transitorio, un `NotFound`/`Conflict` del reintento se verifica por
/// IDENTIDAD (`from_id`, capturada por el caller ANTES del primer intento):
/// destino = nodo original Y origen ausente ⇒ el rename se aplicó. Sin
/// identidad no se adivina: surge el error transitorio original (fail-safe;
/// el usuario reintenta contra el estado real).
async fn rename_retrying(
    p: &dyn Provider,
    from: &VPath,
    to: &VPath,
    from_id: Option<NodeId>,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut attempt = 0u32;
    let mut last_transient: Option<Error> = None;
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let err = match p.rename(from, to).await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        match err {
            Error::NotFound | Error::Conflict { .. } if last_transient.is_some() => {
                return match rename_applied(p, from, to, from_id, cancel).await? {
                    Some(true) => Ok(()),
                    // Verificado: NO se aplicó — el error es genuino (la
                    // política de colisión del caller sigue funcionando).
                    Some(false) => Err(err),
                    // Inverificable: el transitorio original es la verdad.
                    None => Err(last_transient.take().unwrap_or(err)),
                };
            }
            e if attempt < MAX_RETRIES && is_transient(&e) && !cancel.is_cancelled() => {
                last_transient = Some(e);
                backoff_or_cancel(cancel, attempt).await?;
                attempt += 1;
            }
            e => return Err(e),
        }
    }
}

/// ¿Se aplicó el rename de verdad? `Some(true)` = el destino ES el nodo
/// original y el origen ya no existe. `Some(false)` = verificado que NO
/// (destino es OTRO nodo con el origen aún vivo, o el "destino" es un
/// hardlink del origen — id igual pero origen presente: eso no es un
/// rename aplicado). `None` = inverificable (sin identidad).
async fn rename_applied(
    p: &dyn Provider,
    from: &VPath,
    to: &VPath,
    from_id: Option<NodeId>,
    cancel: &CancellationToken,
) -> Result<Option<bool>, Error> {
    let Some(expected) = from_id else {
        return Ok(None);
    };
    let to_id = match with_retry(cancel, || p.node_id(to, FollowLinks::No).boxed()).await {
        Ok(Some(id)) => id,
        // Sin identidad del destino (o destino ausente): inverificable.
        Ok(None) | Err(Error::NotFound) => return Ok(None),
        Err(e) => return Err(e),
    };
    let from_gone = match with_retry(cancel, || p.stat(from).boxed()).await {
        Err(Error::NotFound) => true,
        Ok(_) => false,
        Err(e) => return Err(e),
    };
    match (to_id == expected, from_gone) {
        (true, true) => Ok(Some(true)),
        // Origen vivo: no hubo rename — o el destino es OTRO nodo
        // (conflicto real) o es un HARDLINK del origen (id igual, pero un
        // rename aplicado habría hecho desaparecer el dirent de origen).
        (_, false) => Ok(Some(false)),
        // Origen desaparecido y destino ajeno: estado irreconocible.
        (false, true) => Ok(None),
    }
}

/// Resultado de colocar UNA hoja (archivo o symlink) en el destino.
#[derive(Debug, PartialEq, Eq)]
enum Placed {
    /// Transferida (quizá bajo un nombre alternativo, `RenameAuto`).
    Done,
    /// Saltada por política: el destino no se tocó; en un move, el ORIGEN
    /// debe conservarse.
    Skipped,
}

/// ¿La política tolera que el dir destino ya exista (merge)?
/// `Fail`/`Ask` mantienen el comportamiento estricto de M0.
fn merge_allowed(p: CollisionPolicy) -> bool {
    !matches!(p, CollisionPolicy::Fail | CollisionPolicy::Ask)
}

/// Nombre alternativo nº `n`: sufijo ` (n)` antes de la ÚLTIMA extensión
/// (split en el último `.` que no sea el primer byte — un dotfile no tiene
/// extensión). Byte-safe: jamás decodifica el nombre.
fn rename_auto_candidate(name: &[u8], n: u32) -> Vec<u8> {
    let dot = name.iter().rposition(|&b| b == b'.').filter(|&i| i > 0);
    let (stem, ext) = match dot {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, &[][..]),
    };
    let mut out = stem.to_vec();
    out.extend_from_slice(format!(" ({n})").as_bytes());
    out.extend_from_slice(ext);
    out
}

/// ¿`from` y `to` apuntan al MISMO nodo del provider? Sobrescribir algo
/// consigo mismo lo DESTRUYE (remove + read → NotFound): hay que
/// rechazarlo antes.
///
/// Con identidad real ([`Provider::node_id`], issue #16) el veredicto es
/// DEFINITIVO en ambos sentidos: ids iguales = mismo nodo (aunque el FS
/// pliegue caja/normalización más ancho que cualquier heurística); ids
/// distintos = nodos distintos (aunque los nombres solo difieran en caja —
/// el caso NTFS case-sensitive bajo WSL, que la heurística bloqueaba mal).
/// Sin identidad (`Ok(None)`), degrada a la heurística conservadora de M1.
/// Un error real de `node_id` aborta (fail-safe: ante la duda, nada
/// destructivo).
///
/// `follow_src`: bajo `SymlinkPolicy::Follow` lo que se copia es el
/// TARGET del origen — copiar `ln → f` con `ln` apuntando a `f` es
/// sobrescribir `f` consigo mismo (el remove del Overwrite lo destruiría
/// antes de leerlo a través del link): la identidad del origen se compara
/// RESUELTA (hallazgo del encoding-auditor, fase 1 M2).
async fn same_node(
    from: &VPath,
    to: &VPath,
    dst: &dyn Provider,
    follow_src: FollowLinks,
    cancel: &CancellationToken,
) -> Result<bool, Error> {
    if from == to {
        return Ok(true);
    }
    if from.scheme() != to.scheme() || from.authority() != to.authority() {
        return Ok(false);
    }
    let from_id = match with_retry(cancel, || dst.node_id(from, follow_src).boxed()).await {
        Ok(id) => id,
        // Sin origen (o link roto): no hay autodestrucción posible; su
        // stat/read posterior dará el error honesto.
        Err(Error::NotFound) => return Ok(false),
        Err(e) => return Err(e),
    };
    if let Some(a) = from_id {
        match with_retry(cancel, || dst.node_id(to, FollowLinks::No).boxed()).await {
            Ok(Some(b)) => return Ok(a == b),
            // Destino libre: nada que destruir.
            Err(Error::NotFound) => return Ok(false),
            // Identidad a medias (volumen mixto): cae a la heurística.
            Ok(None) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(same_node_heuristic(from, to, dst).await)
}

/// El modo de identidad del ORIGEN según la política de symlinks: bajo
/// `Follow` se copia el target, así que la identidad relevante es la
/// resuelta.
fn follow_links_for(opts: TransferOptions) -> FollowLinks {
    if opts.symlinks == SymlinkPolicy::Follow {
        FollowLinks::Yes
    } else {
        FollowLinks::No
    }
}

/// Heurística conservadora para providers sin identidad de nodo: byte-igual
/// siempre; y, cuando el DESTINO no distingue caja, también la variante que
/// pliega a lo mismo. La identidad real tiene prioridad; esto es lo que queda
/// cuando el backend no la da.
///
/// Pregunta por la UBICACIÓN y no por el provider (#215): `capabilities()`
/// contesta por el mount del provider, así que bajo un mismo `file://` un
/// pincho exFAT montado en `/mnt` recibía la respuesta de `/home` — y lo que
/// se decide con ella es si un `move` es un rename sobre sí mismo, que es un
/// camino de COPIA. Que la función sea `async` no cuesta nada: su único
/// llamante ya lo era.
///
/// Y pliega con la clave compartida (`norte_encoding::name_key`, ADR 0051) en
/// vez de con un `to_lowercase`: el `to_lowercase` de std no es el pliegue de
/// ningún filesystem —le faltan las 22 deltas de #129— y en un ext4 `+F` no es
/// ni de lejos el pliegue que ese directorio hace. La clave ya sabe cuál toca
/// a partir de las capabilities.
async fn same_node_heuristic(from: &VPath, to: &VPath, dst: &dyn Provider) -> bool {
    let mode = fold_mode_at(to, dst).await;
    if mode == norte_encoding::FoldMode::None {
        return false;
    }
    let a: Vec<&[u8]> = from.segments().collect();
    let b: Vec<&[u8]> = to.segments().collect();
    a.len() == b.len()
        && a.iter().zip(&b).all(|(x, y)| {
            x == y || norte_encoding::name_key(x, mode) == norte_encoding::name_key(y, mode)
        })
}

/// Resuelve la colisión de UNA hoja contra el DESTINO (trampa del dominio:
/// siempre contra el destino). `Ok(Some(path))` = copiar ahí; `Ok(None)` =
/// saltar por política.
///
/// Toma el [`Destination`] y no el provider pelado (#218): el `stat` que
/// decide y el `remove` que ejecuta van por el DESCRIPTOR cuando lo hay. Por
/// ruta, un componente intermedio sustituido hacía que se lstateara —y luego
/// se borrara— un fichero del árbol del atacante, y solo después el write
/// confinado se negaba: un fichero destruido fuera de la raíz, nada escrito
/// en su lugar, y una entrada de journal nombrando otro sitio.
///
/// Ventana TOCTOU residual documentada: entre este `stat` y el remove/write
/// posterior el destino puede cambiar. Sin pérdida silenciosa (el `write()`
/// del provider es create-new), pero el replace atómico llega con `WriteOpts`
/// en M2 (ADR 0005).
async fn resolve_collision(
    into: &Destination<'_>,
    to: &VPath,
    src_entry: &Entry,
    policy: CollisionPolicy,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<Option<VPath>, Error> {
    let dst = into.provider();
    let en_destino = into.at(to.clone());
    let existing = match with_retry(&ctx.cancel, || en_destino.stat().boxed()).await {
        Err(Error::NotFound) => return Ok(Some(to.clone())),
        Ok(e) => e,
        Err(e) => return Err(e),
    };
    // Si el provider ecoa claves REALES (MemProvider), esto caza cualquier
    // plegado (caja Y normalización): el "colisionado" es el propio origen.
    if existing.path == src_entry.path {
        return Err(Error::InvalidPath);
    }
    match policy {
        // `Ask` de verdad llega con los diálogos del TUI (fase 5, ADR 0005).
        CollisionPolicy::Fail | CollisionPolicy::Ask => Err(Error::Conflict {
            conflict: ConflictKind::Exists,
        }),
        CollisionPolicy::Skip => Ok(None),
        CollisionPolicy::Overwrite => {
            overwrite_existing(&en_destino, &existing, observer, ctx).await?;
            Ok(Some(to.clone()))
        }
        CollisionPolicy::Newer => match (src_entry.mtime_ms, existing.mtime_ms) {
            (Some(s), Some(d)) if s > d => {
                overwrite_existing(&en_destino, &existing, observer, ctx).await?;
                Ok(Some(to.clone()))
            }
            (Some(_), Some(_)) => Ok(None),
            // Sin mtime comparable: jamás adivinar (ADR 0005).
            _ => Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            }),
        },
        CollisionPolicy::RenameAuto => {
            let name = to.file_name().ok_or(Error::InvalidPath)?;
            let name = name.as_bytes().to_vec();
            for n in 1..=1000u32 {
                if ctx.cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let seg = Segment::new(rename_auto_candidate(&name, n))
                    .map_err(|_| Error::InvalidPath)?;
                let cand = to.with_file_name(seg).ok_or(Error::InvalidPath)?;
                match with_retry(&ctx.cancel, || dst.stat(&cand).boxed()).await {
                    Err(Error::NotFound) => return Ok(Some(cand)),
                    Ok(_) => {}
                    Err(e) => return Err(e),
                }
            }
            Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            })
        }
    }
}

/// Quita la hoja existente del destino para reemplazarla (Overwrite/Newer).
/// Jamás pisa un DIR con una hoja: eso es `TypeMismatch`, no política.
///
/// El borrado va por el DESCRIPTOR si el destino tiene raíz (#218). Una raíz
/// que no sabe borrar confinado hace que la política se RECHACE: caerse al
/// borrado por ruta sería reabrir el agujero en el único sitio donde esta
/// operación destruye, y hacerlo callando.
async fn overwrite_existing(
    dest: &Dest<'_>,
    existing: &Entry,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if existing.kind == EntryKind::Dir {
        return Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        });
    }
    dest.remove(&ctx.cancel).await?;
    observer
        .on_mutation(&Mutation::Removed(dest.path()), &ctx.actor)
        .await?;
    Ok(())
}

/// Crea el dir destino, o lo ACEPTA si ya existe como dir y la política
/// permite merge (spec: copiar dir sobre dir = fusionar, política por hoja).
///
/// #32.2 — pre-stat del destino ANTES del primer mkdir: es la ÚNICA forma de
/// distinguir, tras un fallo transitorio, nuestro dir fantasma de uno
/// preexistente — con él, el `Created` del dir ambiguo llega al journal
/// (regla 4) y el undo lo conoce. Coste honesto: +1 stat por dir NUEVO; para
/// un dir preexistente bajo merge es neutro o mejor (el stat sustituye al
/// mkdir fallido + stat del camino viejo).
async fn ensure_dir(
    dest: &Dest<'_>,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let to = dest.path();
    // El pre-stat va por el DESCRIPTOR cuando lo hay (#218): por ruta, un
    // componente intermedio sustituido lo hacía mirar el árbol del atacante y
    // contestar «ya existe como dir», con lo que la fusión seguía adelante
    // sobre un sitio que la raíz no cubre. Un `EscapesRoot` sale por el brazo
    // de error de abajo y para la operación, que es la respuesta.
    let pre = match with_retry(&ctx.cancel, || dest.stat().boxed()).await {
        Ok(e) => Some(e),
        Err(Error::NotFound) => None,
        Err(e) => return Err(e),
    };
    if let Some(existing) = pre {
        // Preexistente: JAMÁS Created (no es nuestro; el undo no lo toca).
        return if existing.kind != EntryKind::Dir {
            Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            })
        } else if merge_allowed(opts.on_collision) {
            Ok(())
        } else {
            Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            })
        };
    }
    match mkdir_retrying(dest, &ctx.cancel).await {
        Ok(()) => {
            observer
                .on_mutation(&Mutation::Created(to), &ctx.actor)
                .await?;
            Ok(())
        }
        // Un escape NO es una colisión que fusionar: el `stat` de abajo va por
        // ruta y encontraría el directorio de FUERA, con lo que la fusión
        // seguiría adelante sobre un sitio que la raíz no cubre.
        Err(
            e @ Error::Conflict {
                conflict: ConflictKind::EscapesRoot,
            },
        ) => Err(e),
        // Conflict SIN ambigüedad: un tercero creó el dir entre nuestro
        // pre-stat y el mkdir (carrera externa). Merge lo absorbe SIN
        // Created (no es nuestro); Fail/Ask fallan en seguro.
        Err(Error::Conflict { .. }) if merge_allowed(opts.on_collision) => {
            // Por el descriptor, mismo motivo que el pre-stat.
            let existing = with_retry(&ctx.cancel, || dest.stat().boxed()).await?;
            if existing.kind == EntryKind::Dir {
                Ok(())
            } else {
                Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                })
            }
        }
        Err(e) => Err(e),
    }
}

/// Copia una hoja ARCHIVO aplicando la política de colisión y reintentos
/// a nivel de archivo completo (un fallo transitorio reinicia el archivo;
/// el resume por offset llega en M2).
async fn copy_file_leaf(
    src: &dyn Provider,
    into: &Destination<'_>,
    entry: &Entry,
    to: &VPath,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<Placed, Error> {
    let Some(target) = resolve_collision(into, to, entry, opts.on_collision, observer, ctx).await?
    else {
        return Ok(Placed::Skipped);
    };
    // DESPUÉS de la colisión: la política pudo cambiarle el nombre, y el
    // relativo tiene que ser el de la ruta sobre la que se escribe de verdad.
    let dest = into.at(target);
    copy_file_retrying(src, &dest, &entry.path, entry.size, opts, observer, ctx).await?;
    Ok(Placed::Done)
}

/// Copia una hoja SYMLINK según la política (ADR 0005).
async fn copy_symlink_leaf(
    src: &dyn Provider,
    into: &Destination<'_>,
    entry: &Entry,
    to: &VPath,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<Placed, Error> {
    match opts.symlinks {
        SymlinkPolicy::Skip => Ok(Placed::Skipped),
        SymlinkPolicy::Preserve => {
            let target_bytes =
                with_retry(&ctx.cancel, || src.read_link(&entry.path).boxed()).await?;
            let Some(target) =
                resolve_collision(into, to, entry, opts.on_collision, observer, ctx).await?
            else {
                return Ok(Placed::Skipped);
            };
            // `Unknown` (issue #18): el kind lo resuelve el provider DESTINO
            // best-effort contra su propio árbol; unix lo ignora gratis.
            let dest = into.at(target.clone());
            symlink_retrying(&dest, &target_bytes, SymlinkKind::Unknown, &ctx.cancel).await?;
            observer
                .on_mutation(&Mutation::Created(&target), &ctx.actor)
                .await?;
            Ok(Placed::Done)
        }
        SymlinkPolicy::Follow => {
            // Sondea el target ANTES de cualquier acción destructiva
            // (Overwrite borra el destino): un dir-symlink debe fallar sin
            // haber tocado nada. Soltar el stream libera el fd (testeado).
            match src.read(&entry.path, None).await {
                Ok(probe) => drop(probe),
                // El link apunta a un DIRECTORIO: seguirlo exige detección
                // de ciclos (visited set) — M2 (ADR 0005).
                Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                }) => return Err(Error::Unsupported),
                Err(e) => return Err(e),
            }
            let Some(target) =
                resolve_collision(into, to, entry, opts.on_collision, observer, ctx).await?
            else {
                return Ok(Placed::Skipped);
            };
            // Tamaño desconocido (el stat describe el LINK, no el destino).
            // El target pudo cambiar tras el sondeo: se re-mapea igual.
            let dest = into.at(target);
            match copy_file_retrying(src, &dest, &entry.path, None, opts, observer, ctx).await {
                Ok(()) => Ok(Placed::Done),
                Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                }) => Err(Error::Unsupported),
                Err(e) => Err(e),
            }
        }
    }
}

/// Copia `from` → `to` (recursiva si es dir) con las políticas de `opts`.
///
/// Cancelación/fallo a mitad de ÁRBOL: cada archivo individual queda completo
/// o sin rastro (contrato del sink), pero el subárbol ya copiado PERMANECE en
/// el destino — todo lo commiteado fue observado como `Created` (el undo del
/// journal M3 lo revertirá; hasta entonces es limpieza manual).
#[tracing::instrument(skip_all, fields(from = %from.display_lossy(), to = %to.display_lossy()))]
// El octavo argumento es el ancla del destino (#295). Agruparla con `opts`
// costaría el `Copy` de `TransferOptions`, que se copia en cada paso de un
// árbol; agruparla con los providers mezclaría el QUÉ con el DÓNDE.
#[expect(
    clippy::too_many_arguments,
    reason = "las opciones viajan sueltas: con los providers mezclarían el QUÉ con el DÓNDE"
)]
pub(crate) async fn copy_task(
    src: Arc<dyn Provider>,
    dst: Arc<dyn Provider>,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
    dest_anchor: Option<norte_proto::DirAnchor>,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    // El veredicto del journal, fijado ANTES del primer efecto (#205): esta
    // Task queda entera dentro del journal o entera fuera. Es un `copy_tree`
    // el ejemplo del que salió la issue.
    let observer = crate::observer::pin_for_task(observer).await?;
    // Copiar un dir DENTRO de sí mismo produciría una copia anidada absurda;
    // copiar algo SOBRE SÍ MISMO con Overwrite lo destruiría (hallazgo B1).
    // Bajo Follow, el "sí mismo" es el TARGET resuelto del origen.
    if Arc::ptr_eq(&src, &dst)
        && (is_descendant_folded(&to, &from, &*dst).await
            || same_node(&from, &to, &*dst, follow_links_for(opts), &ctx.cancel).await?)
    {
        return Err(Error::InvalidPath);
    }
    let src_entry = with_retry(&ctx.cancel, || src.stat(&from).boxed()).await?;
    let tarea = ctx.progress.snapshot().task_id.get();
    match src_entry.kind {
        EntryKind::File => {
            ctx.progress.update(|p| {
                p.bytes_total = src_entry.size;
                p.entries_total = Some(1);
                p.current = Some(from.clone());
            });
            // La hoja se confina bajo SU DIRECTORIO destino (#219), que es lo
            // que el humano eligió y lo que el gate resolvió. Antes iba sin
            // confinar, con el argumento de que «una hoja suelta no cuelga de
            // ningún árbol aprobado»; la revisión de seguridad enseñó que sí
            // cuelga de uno, y que sin él `fs.copy` —la operación más común
            // del producto— tenía #164 intacto.
            //
            // Se abre DENTRO de cada brazo de hoja, no antes del `match`: el
            // tercero —un dir-symlink bajo `Follow`— se desvía a `copy_tree`,
            // que abre la suya, y pagar aquí una raíz que descarta le haría
            // heredar además sus errores.
            let (dir, raiz) =
                open_leaf_root(&*dst, &to, dest_anchor.as_ref(), tarea, &ctx.cancel).await?;
            let into = destino_de_hoja(&*dst, dir.as_ref(), raiz.as_deref(), &to);
            copy_file_leaf(&*src, &into, &src_entry, &to, opts, &observer, ctx).await?;
            ctx.progress.update(|p| p.entries_done = 1);
            Ok(())
        }
        EntryKind::Symlink => {
            // Dir-symlink raíz con Follow: se copia el ÁRBOL del target
            // como dir real (issue #19), no como hoja.
            if opts.symlinks == SymlinkPolicy::Follow
                && probe_symlink_target(&*src, &from, &ctx.cancel).await? == TargetKind::Dir
            {
                anchor_parent_or_fail(&*dst, &to, dest_anchor.as_ref(), tarea, &ctx.cancel).await?;
                let mut plan = walk_following(&*src, &from, true, &ctx.cancel).await?;
                hydrate_plan(&*src, &mut plan, ctx).await?;
                return copy_tree(&src, &dst, &from, &to, &plan, opts, &observer, ctx)
                    .await
                    .map(|_skipped| ());
            }
            ctx.progress.update(|p| {
                p.entries_total = Some(1);
                p.current = Some(from.clone());
            });
            let (dir, raiz) =
                open_leaf_root(&*dst, &to, dest_anchor.as_ref(), tarea, &ctx.cancel).await?;
            let into = destino_de_hoja(&*dst, dir.as_ref(), raiz.as_deref(), &to);
            copy_symlink_leaf(&*src, &into, &src_entry, &to, opts, &observer, ctx).await?;
            ctx.progress.update(|p| p.entries_done = 1);
            Ok(())
        }
        EntryKind::Dir => {
            // Un árbol crea su destino, así que aquí el ancla se comprueba
            // sobre el PADRE y por ruta (ver `anchor_parent_or_fail`): la raíz
            // que confina lo demás todavía no existe.
            anchor_parent_or_fail(&*dst, &to, dest_anchor.as_ref(), tarea, &ctx.cancel).await?;
            let mut plan = plan_for(&*src, &from, opts, &ctx.cancel).await?;
            hydrate_plan(&*src, &mut plan, ctx).await?;
            copy_tree(&src, &dst, &from, &to, &plan, opts, &observer, ctx)
                .await
                .map(|_skipped| ())
        }
        EntryKind::Other => Err(Error::Unsupported),
    }
}

/// #52: el listado local es lazy (`size`/`mtime_ms` en `None`). El progreso
/// (`bytes_total`), `CollisionPolicy::Newer` y la conservación del mtime del
/// symlink necesitan los metadatos ANTES de copiar: stat-ea SOLO las hojas a
/// las que les falte algo (un `Symlink` nunca tiene `size` — solo le falta
/// `mtime_ms`, así que no se re-statea por eso). Un stat fallido deja `None`:
/// la barra queda subestimada y, bajo `CollisionPolicy::Newer`, la colisión
/// degrada a `Conflict{Exists}` (sin mtime comparable no se adivina, ADR
/// 0005) — fail-closed, nunca pérdida de datos.
async fn hydrate_plan(
    src: &dyn Provider,
    plan: &mut [PlanEntry],
    ctx: &TaskCtx,
) -> Result<(), Error> {
    for pe in plan.iter_mut() {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let e = &mut pe.entry;
        let needs = match e.kind {
            EntryKind::File => e.size.is_none() || e.mtime_ms.is_none(),
            EntryKind::Symlink => e.mtime_ms.is_none(),
            _ => false,
        };
        if needs {
            // Progreso solo cuando REALMENTE hay stat de por medio: en un
            // provider no-lazy `needs` es casi siempre falso y no queremos
            // 100k updates de la barra que no aportan nada (MINOR-5).
            ctx.progress.update(|p| p.current = Some(e.path.clone()));
            if let Ok(st) = src.stat(&e.path).await {
                e.size = e.size.or(st.size);
                e.mtime_ms = e.mtime_ms.or(st.mtime_ms);
            }
        }
    }
    Ok(())
}

/// Copia el árbol `from` → `to` según un plan YA walkeado (el walk es del
/// caller: el move lo reusa para el delete — issue #9). La copia ignora la
/// provenance (un dir sintético de un link expandido se crea como dir
/// real, issue #19); la provenance manda en el DELETE del move. Devuelve
/// los paths de ORIGEN saltados por política (el move no debe borrarlos).
#[expect(
    clippy::too_many_arguments,
    reason = "función interna del módulo, no API"
)]
async fn copy_tree(
    src: &Arc<dyn Provider>,
    dst: &Arc<dyn Provider>,
    from: &VPath,
    to: &VPath,
    plan: &[PlanEntry],
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<Vec<VPath>, Error> {
    let bytes_total: u64 = plan
        .iter()
        .filter(|pe| pe.entry.kind == EntryKind::File)
        .filter_map(|pe| pe.entry.size)
        .sum();
    let total = plan.len() as u64 + 1; // +1 por la raíz
    ctx.progress.update(|p| {
        p.bytes_total = Some(bytes_total);
        p.entries_total = Some(total);
    });

    // La raíz del árbol se crea por RUTA: su padre no es un sitio del que este
    // destino tenga raíz, y crearla es justo lo que da la raíz que confina todo
    // lo demás.
    ensure_dir(&Dest::plain(&**dst, to.clone()), opts, observer, ctx).await?;
    ctx.progress.update(|p| {
        p.entries_done += 1;
        p.current = Some(to.clone());
    });
    // Y a partir de aquí, TODO cuelga de ella: se abre una vez por operación
    // (#164) y cada paso direcciona su relativo. Un destino que no sabe
    // confinarse lo dice en el log y sigue por el camino de siempre; uno que
    // dijo saber y no pudo PARA la copia (ver `open_dest_root`).
    let root = open_dest_root(&**dst, to, ctx.progress.snapshot().task_id.get()).await?;
    // Y que la raíz abierta sea la que se acaba de crear, no otra.
    if let Some(root) = root.as_deref() {
        same_root_or_fail(&**dst, root, to, &ctx.cancel).await?;
    }
    let into = Destination::new(&**dst, root.as_deref(), to);

    let mut skipped: Vec<VPath> = Vec::new();
    for pe in plan {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let entry = &pe.entry;
        let target = rebase(&entry.path, from, to)?;
        ctx.progress
            .update(|p| p.current = Some(entry.path.clone()));
        match entry.kind {
            EntryKind::Dir => {
                ensure_dir(&into.at(target), opts, observer, ctx).await?;
            }
            EntryKind::File => {
                if copy_file_leaf(&**src, &into, entry, &target, opts, observer, ctx).await?
                    == Placed::Skipped
                {
                    // La barra debe poder llegar a 100%: lo saltado no cuenta.
                    ctx.progress.update(|p| {
                        p.bytes_total = p
                            .bytes_total
                            .map(|t| t.saturating_sub(entry.size.unwrap_or(0)));
                    });
                    skipped.push(entry.path.clone());
                }
            }
            EntryKind::Symlink => {
                if copy_symlink_leaf(&**src, &into, entry, &target, opts, observer, ctx).await?
                    == Placed::Skipped
                {
                    skipped.push(entry.path.clone());
                }
            }
            EntryKind::Other => return Err(Error::Unsupported),
        }
        ctx.progress.update(|p| p.entries_done += 1);
    }
    Ok(skipped)
}

/// Copia UN archivo con reintentos a nivel de archivo. Con `resume=Off` un
/// fallo transitorio reinicia el archivo entero (el sink abortó limpio) y
/// devuelve el progreso al punto de partida. Con `resume=On` el parcial
/// SOBREVIVE (`keep`) y el reintento continúa desde donde iba
/// (`open_resumable`) — el `before` que se restaura es la base del archivo,
/// no cero, y `copy_file` recompone `base + already` en cada intento.
pub(crate) async fn copy_file_retrying(
    src: &dyn Provider,
    dest: &Dest<'_>,
    from: &VPath,
    known_size: Option<u64>,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let base = ctx.progress.snapshot().bytes_done;
    let mut attempt = 0u32;
    loop {
        match copy_file(src, dest, from, known_size, base, opts, observer, ctx).await {
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) && !ctx.cancel.is_cancelled() => {
                // Base del archivo: `copy_file` recompone `base + already`
                // (con resume, `already` crece; sin resume, vuelve a 0).
                ctx.progress.update(|p| p.bytes_done = base);
                let delay = std::time::Duration::from_millis(BACKOFF_BASE_MS << attempt);
                attempt += 1;
                tokio::select! {
                    () = ctx.cancel.cancelled() => return Err(Error::Cancelled),
                    () = tokio::time::sleep(delay) => {}
                }
            }
            other => return other,
        }
    }
}

/// SHA-256 de los primeros `len` bytes del ORIGEN (#35, `VerifyPolicy::Hash`):
/// se compara con el digest del staging del destino para decidir si el
/// parcial sigue siendo válido. Lee `origen[0..len]` por stream (el mismo
/// coste que Length ahorra: Hash re-LEE el prefijo del origen, pero no lo
/// re-ESCRIBE).
///
/// `Ok(Some(d))` = digest del prefijo; `Ok(None)` = el origen es MÁS CORTO
/// que `len` (no hay prefijo que casar → el caller descarta). Un error REAL
/// de lectura (transitorio, permisos) se PROPAGA con `Err` — jamás se
/// confunde con "origen corto", que destruiría el parcial (M1 del reviewer).
/// Chequea cancelación por chunk (regla 3, M2 del reviewer): un parcial de
/// GiB no bloquea la Task.
async fn hash_source_prefix(
    src: &dyn Provider,
    from: &VPath,
    len: u64,
    ctx: &TaskCtx,
) -> Result<Option<[u8; 32]>, Error> {
    use sha2::{Digest, Sha256};
    let range = norte_proto::ByteRange {
        offset: 0,
        len: Some(len),
    };
    let mut stream = src.read(from, Some(range)).await?;
    let mut hasher = Sha256::new();
    let mut seen: u64 = 0;
    while let Some(item) = stream.next().await {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let chunk = item?;
        // El origen podría entregar de más si ignora el `len`: recorta al
        // prefijo exacto para que el digest cubra SOLO `origen[..len]`.
        let take = usize::try_from(len - seen)
            .unwrap_or(chunk.len())
            .min(chunk.len());
        hasher.update(&chunk[..take]);
        seen += take as u64;
        if seen >= len {
            break;
        }
    }
    if seen < len {
        return Ok(None); // origen más corto que el parcial
    }
    Ok(Some(hasher.finalize().into()))
}

/// ¿Descartar el parcial reanudable y empezar de cero? Decide según
/// `VerifyPolicy` (#35): Length compara tamaños; Hash compara el digest del
/// prefijo del origen con el del staging (si el provider lo expone, si no
/// degrada a Length).
async fn should_discard_partial(
    src: &dyn Provider,
    dest: &Dest<'_>,
    from: &VPath,
    already: u64,
    known_size: Option<u64>,
    verify: VerifyPolicy,
    ctx: &TaskCtx,
) -> Result<bool, Error> {
    // Un parcial más largo que el origen nunca cuadra (ambas políticas), y
    // ahorra hashear: el origen cambió/encogió.
    if known_size.is_some_and(|size| already > size) {
        return Ok(true);
    }
    if already == 0 || verify == VerifyPolicy::Length {
        return Ok(false);
    }
    // Hash: sin digest del staging el provider no permite verificar → degrada
    // a Length (el check de tamaño de arriba ya se aplicó).
    // Por el DESCRIPTOR cuando hay raíz: por ruta, esto resuelve el nombre del
    // staging siguiendo cada componente, así que la ÚNICA verificación que hay
    // sobre los bytes que se reanudan se hacía por la puerta que el
    // confinamiento cerró para el `stat` y el `remove` — y un componente
    // intermedio sustituido le da el digest de otro fichero.
    let Some(partial_dig) = dest.partial_digest(already).await? else {
        return Ok(false);
    };
    // `None` = origen más corto que el parcial → descartar. Un error REAL se
    // propaga (`?`): jamás se traga como "descartar" (M1 del reviewer).
    match hash_source_prefix(src, from, already, ctx).await? {
        Some(src_dig) => Ok(src_dig != partial_dig),
        None => Ok(true),
    }
}

/// Copia UN archivo: `copy_native` si el provider (el mismo a ambos lados)
/// declara `SERVER_COPY`; si no, streaming con cancelación por chunk.
///
/// `base` = `bytes_done` ANTES de este archivo (para recomponer el progreso
/// al reanudar). Con resume: abre `open_resumable`, descarta el parcial si
/// no cuadra con el origen (`verify`, #35), lee el origen desde `already`, y
/// en cancelación/fallo CONSERVA el parcial (`keep`) en vez de abortar.
#[expect(
    clippy::too_many_arguments,
    reason = "función interna del módulo, no API"
)]
async fn copy_file(
    src: &dyn Provider,
    dest: &Dest<'_>,
    from: &VPath,
    known_size: Option<u64>,
    base: u64,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let (backend, to) = (dest.provider(), dest.path());
    if std::ptr::eq(
        std::ptr::from_ref(src).cast::<()>(),
        std::ptr::from_ref(backend).cast::<()>(),
    ) && src
        .capabilities()
        .flags
        .contains(norte_proto::CapabilityFlags::SERVER_COPY)
    {
        // Regla 3 (#51): copy_native es UN await potencialmente de minutos
        // (multipart copy S3, opendal lo trocea solo) — se racea contra la
        // cancelación. `biased` con el copy PRIMERO: una completación ya
        // observada se journaliza SIEMPRE aunque el token también esté
        // cancelado (regla 4); la cancelación no pierde latencia (el select
        // pollea ambas ramas en cada wakeup). Dropear el future a medias
        // jamás publica un objeto A MEDIAS (CopyObject es atómico; un
        // multipart incompleto no publica), pero quedan dos ambigüedades
        // documentadas (contrato en el rustdoc de `Provider::copy_native`):
        // - partes huérfanas facturables en S3: opendal solo aborta el
        //   multipart en su camino de error, no en drop — mismo caso que el
        //   Drop del sink de escritura (ADR 0016 E: lifecycle rule del bucket
        //   para AbortIncompleteMultipartUpload);
        // - si el server completa la copia DESPUÉS del drop, el destino queda
        //   con el objeto ÍNTEGRO sin entrada de journal (ambigüedad
        //   post-efecto, misma familia que #32) — nunca un parcial sin marcar.
        let native = tokio::select! {
            biased;
            res = src.copy_native(from, to) => res,
            () = ctx.cancel.cancelled() => return Err(Error::Cancelled),
        };
        if let Some(res) = native {
            res?;
            // El tamaño ya lo dio el stat del origen: cero round-trips extra.
            let size = known_size.unwrap_or(0);
            ctx.progress.update(|p| p.bytes_done = base + size);
            observer
                .on_mutation(&Mutation::Created(to), &ctx.actor)
                .await?;
            return Ok(());
        }
        // `None`: el provider declinó pese al cap — cae al streaming.
    }

    // Resume AGNÓSTICO del provider (ADR 0012 A2): `open_resumable` con su
    // default seguro `(write, 0)` degrada limpio en un provider sin
    // reanudación real; no se gatea por capability (S3 reanuda por
    // multipart, no por APPEND — M1 del rust-reviewer).
    // Un destino confinado NO reanuda (`Dest::resumes`): conservar su staging
    // efímero dejaría un `.norte-partial` por intento que nadie continúa.
    let resume = opts.resume == norte_proto::ResumePolicy::On && dest.resumes();
    // Abre el sink: reanudable (con offset ya durable) o fresco.
    let (mut sink, already) = if resume {
        let (sink, already) = dest.open_resumable().await?;
        // ¿Descartar el parcial y empezar de cero? El origen pudo cambiar
        // bajo los pies entre invocaciones (ADR 0012, #35):
        //   - Length: un parcial más largo que el origen no cuadra.
        //   - Hash: el prefijo `origen[..already]` no casa byte-a-byte con el
        //     del parcial. Si el provider no expone digest del staging,
        //     DEGRADA a Length (documentado en el trait).
        let discard =
            should_discard_partial(src, dest, from, already, known_size, opts.verify, ctx).await?;
        if discard {
            // Propagar el fallo de abort (M2 del rust-reviewer): tragarlo y
            // seguir dejaría bytes obsoletos y el destino saldría corrupto.
            sink.abort().await?;
            let (fresh, fresh_already) = dest.open_resumable().await?;
            if fresh_already != 0 {
                // El staging sigue ahí tras el abort: no se puede reanudar
                // limpio — fallar en vez de publicar algo dudoso.
                return Err(Error::Io { retryable: false });
            }
            (fresh, 0)
        } else {
            (sink, already)
        }
    } else {
        (dest.write().await?, 0)
    };

    // El tramo ya presente cuenta como hecho de inmediato (la barra no
    // retrocede al reanudar).
    ctx.progress.update(|p| p.bytes_done = base + already);

    let range = (already > 0).then_some(norte_proto::ByteRange {
        offset: already,
        len: None,
    });
    let mut stream = match src.read(from, range).await {
        Ok(s) => s,
        Err(e) => {
            release(sink, to, resume).await;
            return Err(e);
        }
    };
    let mut written = base + already;
    while let Some(item) = stream.next().await {
        // Cancelación por chunk: destino limpio, o `.norte-partial`
        // reanudable (resume), jamás un archivo a medias sin marcar.
        if ctx.cancel.is_cancelled() {
            release(sink, to, resume).await;
            return Err(Error::Cancelled);
        }
        let chunk = match item {
            Ok(c) => c,
            Err(e) => {
                release(sink, to, resume).await;
                return Err(e);
            }
        };
        let n = chunk.len() as u64;
        if let Err(e) = sink.write(chunk).await {
            release(sink, to, resume).await;
            return Err(e);
        }
        written += n;
        ctx.progress.update(|p| p.bytes_done = written);
    }
    if ctx.cancel.is_cancelled() {
        release(sink, to, resume).await;
        return Err(Error::Cancelled);
    }
    // Tamaño del fichero commiteado = lo escrito en ESTA copia (`written`)
    // menos la `base` de progreso previa a este archivo. Es el testigo para
    // desambiguar un commit que aplicó pero devolvió transitorio (#32.1).
    let final_size = written - base;
    match sink.commit().await {
        Ok(()) => {}
        // El commit (rename staging→final) pudo APLICARSE antes de devolver
        // un error transitorio (#32.1): sin esto, el retry recopia y su
        // commit no-replace da `Conflict` → task FALLIDA con el archivo bien
        // copiado y SIN `Created` (regla 4). Se desambigua por presencia +
        // tamaño: si `to` existe con el tamaño esperado, fue nuestra
        // escritura → cuenta como éxito. Si no aparece, el commit no aplicó y
        // se propaga el transitorio (el retry recopia limpio).
        //
        // SUPUESTOS (reviewer): `to` está garantizado LIBRE al empezar
        // (`resolve_collision` lo asegura en toda política) y hay un único
        // escritor por task — así, un `to` del tamaño esperado SOLO puede ser
        // nuestra escritura (jamás reclama un fichero ajeno). Limitaciones
        // residuales, fail-safe (propagan el transitorio → como antes del
        // fix): un provider cuyo `stat` no reporte `size` (`None`), o un
        // episodio transitorio que también agote el `stat`, no confirman y
        // recaen en el camino de fallo.
        Err(e) if is_transient(&e) => {
            // Y la desambiguación post-transitorio también (#218): reclamar
            // como nuestra una escritura que en realidad está fuera de la raíz
            // le pondría un `Created` en el journal a un fichero ajeno, y el
            // undo lo mandaría a la papelera.
            match with_retry(&ctx.cancel, || dest.stat().boxed()).await {
                // Aplicó: `to` existe con el tamaño esperado → cae al Created.
                Ok(entry) if entry.size == Some(final_size) => {}
                // Cancelado durante la comprobación: propaga cancelación.
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                // No aplicó (NotFound), existe pero no cuadra, o el stat
                // falló: no lo reclamamos — propaga el transitorio (el retry
                // recopia limpio, o el usuario reintenta contra el estado
                // real).
                _ => return Err(e),
            }
        }
        Err(e) => return Err(e),
    }
    observer
        .on_mutation(&Mutation::Created(to), &ctx.actor)
        .await?;
    Ok(())
}

/// Suelta el sink al interrumpir: `keep` (conserva el `.norte-partial`
/// reanudable) si hay resume, `abort` (destino limpio) si no.
async fn release(sink: Box<dyn norte_vfs::ByteSink>, to: &VPath, resume: bool) {
    let res = if resume {
        sink.keep().await
    } else {
        sink.abort().await
    };
    if let Err(e) = res {
        tracing::warn!(
            path = %to.display_lossy(),
            error = %e,
            "soltar el sink falló; posible staging huérfano"
        );
    }
}

/// Move: rename si origen y destino viven en el MISMO provider (0 bytes);
/// si el provider no puede (`Unsupported`: EXDEV entre montajes, remoto sin
/// rename) o es cross-provider, copy + delete del origen (spec §5).
///
/// El rename se INTENTA primero (no-replace atómico del provider) y la
/// política de colisión se aplica sobre su `Conflict` — así el case-rename
/// en FS insensitive jamás se confunde con una colisión real (el provider
/// lo resuelve por identidad) y `Overwrite` jamás borra el propio origen.
///
/// El copy y el delete se conducen desde UN plan (walk único, issue #9): el
/// delete borra EXACTAMENTE lo copiado, en post-order. Lo saltado por
/// política Y lo aparecido tras el walk sobreviven en el origen — jamás
/// pérdida silenciosa.
#[tracing::instrument(skip_all, fields(from = %from.display_lossy(), to = %to.display_lossy()))]
// Octavo argumento: el ancla del destino (#295), ver `copy_task`.
#[expect(
    clippy::too_many_arguments,
    reason = "Octavo argumento: el ancla del destino (#295), ver `copy_task`"
)]
pub(crate) async fn move_task(
    src: Arc<dyn Provider>,
    dst: Arc<dyn Provider>,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
    dest_anchor: Option<norte_proto::DirAnchor>,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    // #205, y aquí importa el doble: un move por copia emite `created` por cada
    // entrada y `removed` por cada una, así que una Task a medio registrar deja
    // un undo que restaura la mitad del origen sobre la mitad del destino.
    let observer = crate::observer::pin_for_task(observer).await?;
    // El ancla, ANTES de decidir por qué camino va el movimiento (#295).
    //
    // El rename no compone rutas nuevas bajo el destino y por eso no necesita
    // confinamiento, pero SÍ resuelve `to` por ruta una vez: con `d/sub` hecho
    // enlace, `rename` deja el fichero al otro lado igual que lo dejaría una
    // copia. Comprobar aquí cubre los dos caminos; el de copia vuelve a
    // comprobar contra el descriptor, que es exacto.
    anchor_parent_or_fail(
        &*dst,
        &to,
        dest_anchor.as_ref(),
        ctx.progress.snapshot().task_id.get(),
        &ctx.cancel,
    )
    .await?;
    if Arc::ptr_eq(&src, &dst) {
        if is_descendant_folded(&to, &from, &*dst).await {
            return Err(Error::InvalidPath);
        }
        ctx.progress.update(|p| {
            p.entries_total = Some(1);
            p.current = Some(from.clone());
        });
        match rename_with_policy(&*src, &from, &to, opts, &observer, ctx).await {
            Ok(RenameOutcome::Renamed | RenameOutcome::SkippedByPolicy) => {
                ctx.progress.update(|p| p.entries_done = 1);
                return Ok(());
            }
            // El provider no sabe renombrar ESTO (EXDEV entre montajes es el
            // caso típico): degradar a copy+delete, como cross-provider.
            Err(Error::Unsupported) => {}
            Err(e) => return Err(e),
        }
    }
    move_by_copy(src, dst, from, to, opts, dest_anchor, observer, ctx).await
}

/// Overwrite/Newer jamás cruzan tipos (ADR 0005): dir sobre hoja o
/// viceversa es `TypeMismatch`; dir sobre dir degrada a copy+delete
/// (merge) devolviendo `Unsupported` al caller del rename.
fn check_overwrite_kinds(src_e: &Entry, existing: &Entry) -> Result<(), Error> {
    let src_dir = src_e.kind == EntryKind::Dir;
    let dst_dir = existing.kind == EntryKind::Dir;
    if src_dir && dst_dir {
        // Merge de dirs: que lo haga el camino copy+delete.
        return Err(Error::Unsupported);
    }
    if src_dir != dst_dir {
        return Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        });
    }
    Ok(())
}

enum RenameOutcome {
    Renamed,
    SkippedByPolicy,
}

/// Rename same-provider aplicando la política de colisión sobre el
/// `Conflict` del rename no-replace del provider.
// Un brazo por POLÍTICA de colisión, y cada uno con su secuencia completa —
// stat del origen, stat del destino, comprobación de tipos, borrado, rename,
// journal—. Partirla escondería cuál de las cinco hace qué.
#[expect(
    clippy::too_many_lines,
    reason = "cinco fases en orden; partirla escondería cuál hace qué"
)]
async fn rename_with_policy(
    src: &dyn Provider,
    from: &VPath,
    to: &VPath,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<RenameOutcome, Error> {
    // Identidad del origen ANTES del primer intento: es lo único que puede
    // desambiguar un rename cuyo efecto se aplicó tras un timeout (#17).
    // Best-effort puro — la identidad solo VERIFICA: cualquier error aquí
    // degrada a None (el rename sigue funcionando como en M1 y dará su
    // propio error si el problema es real).
    let from_id = with_retry(&ctx.cancel, || src.node_id(from, FollowLinks::No).boxed())
        .await
        .ok()
        .flatten();
    let first = rename_retrying(src, from, to, from_id, &ctx.cancel).await;
    let conflict = match first {
        Ok(()) => {
            observer
                .on_mutation(
                    &Mutation::Renamed {
                        from,
                        to,
                        batch: None,
                    },
                    &ctx.actor,
                )
                .await?;
            return Ok(RenameOutcome::Renamed);
        }
        Err(e @ Error::Conflict { .. }) => e,
        Err(e) => return Err(e),
    };
    // ¿La «colisión» es el PROPIO fichero visto con otra ortografía? (#274)
    //
    // En un volumen que pliega —APFS, NTFS, exFAT, un ext4 `+F`— `Foo.txt` y
    // `foo.txt` son el mismo nodo, y renombrar SIN PISAR (que es como norte
    // renombra: `renameat2(RENAME_NOREPLACE)`, `renamex_np(RENAME_EXCL)`,
    // `MoveFileExW` sin replace) contesta que el destino ya existe. Pero
    // cambiar la ortografía es trabajo de VERDAD: los bytes del nombre
    // cambian, el planificador de lotes ya lo trata así, y el lector no tiene
    // otra manera de hacerlo.
    //
    // Y no era solo que se rehusara. Con `Overwrite`, el brazo de abajo
    // borraba el destino antes de renombrar — o sea el propio fichero — y
    // renombraba después algo que ya no estaba: un cambio de caja que se
    // llevaba el fichero por delante.
    //
    // Se decide por IDENTIDAD y jamás por la heurística de nombres: lo que
    // viene después mueve un fichero por encima de otro, y hacerlo sobre una
    // suposición es exactamente cómo se pierde el que no era. Sin identidad
    // (un provider que no la da) se cae al comportamiento de siempre.
    if es_la_misma_hoja_con_otra_ortografia(src, from, to, from_id, &ctx.cancel).await {
        rename_de_ortografia(src, from, to, from_id, observer, ctx).await?;
        return Ok(RenameOutcome::Renamed);
    }
    match opts.on_collision {
        CollisionPolicy::Fail | CollisionPolicy::Ask => Err(conflict),
        CollisionPolicy::Skip => Ok(RenameOutcome::SkippedByPolicy),
        CollisionPolicy::Overwrite => {
            let src_e = with_retry(&ctx.cancel, || src.stat(from).boxed()).await?;
            let existing = with_retry(&ctx.cancel, || src.stat(to).boxed()).await?;
            check_overwrite_kinds(&src_e, &existing)?;
            // SIN confinar, y a propósito: un rename in-place no abre ninguna
            // raíz —el `renameat` que viene después tampoco podría ir por
            // descriptor sin una— así que aquí no hay handle que usar y
            // fingirlo sería peor. Lo que este camino sí tiene es el
            // `from_id`, que `rename_retrying` comprueba.
            overwrite_existing(&Dest::plain(src, to.clone()), &existing, observer, ctx).await?;
            rename_retrying(src, from, to, from_id, &ctx.cancel).await?;
            observer
                .on_mutation(
                    &Mutation::Renamed {
                        from,
                        to,
                        batch: None,
                    },
                    &ctx.actor,
                )
                .await?;
            Ok(RenameOutcome::Renamed)
        }
        CollisionPolicy::Newer => {
            let src_e = with_retry(&ctx.cancel, || src.stat(from).boxed()).await?;
            let existing = with_retry(&ctx.cancel, || src.stat(to).boxed()).await?;
            match (src_e.mtime_ms, existing.mtime_ms) {
                (Some(s), Some(d)) if s > d => {
                    check_overwrite_kinds(&src_e, &existing)?;
                    overwrite_existing(&Dest::plain(src, to.clone()), &existing, observer, ctx)
                        .await?;
                    rename_retrying(src, from, to, from_id, &ctx.cancel).await?;
                    observer
                        .on_mutation(
                            &Mutation::Renamed {
                                from,
                                to,
                                batch: None,
                            },
                            &ctx.actor,
                        )
                        .await?;
                    Ok(RenameOutcome::Renamed)
                }
                (Some(_), Some(_)) => Ok(RenameOutcome::SkippedByPolicy),
                _ => Err(conflict),
            }
        }
        CollisionPolicy::RenameAuto => {
            let name = to.file_name().ok_or(Error::InvalidPath)?;
            let name = name.as_bytes().to_vec();
            for n in 1..=1000u32 {
                if ctx.cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let seg = Segment::new(rename_auto_candidate(&name, n))
                    .map_err(|_| Error::InvalidPath)?;
                let cand = to.with_file_name(seg).ok_or(Error::InvalidPath)?;
                match rename_retrying(src, from, &cand, from_id, &ctx.cancel).await {
                    Ok(()) => {
                        observer
                            .on_mutation(
                                &Mutation::Renamed {
                                    from,
                                    to: &cand,
                                    batch: None,
                                },
                                &ctx.actor,
                            )
                            .await?;
                        return Ok(RenameOutcome::Renamed);
                    }
                    Err(Error::Conflict { .. }) => {}
                    Err(e) => return Err(e),
                }
            }
            Err(conflict)
        }
    }
}

/// Move por copy + delete con plan único: el walk de la copia ES la lista
/// del delete. Lo saltado por política queda en el origen (junto con sus
/// dirs ancestros).
// Dos formas del mismo verbo —una hoja y un árbol— cada una con su fase de
// copia y su fase de borrado. Separarlas duplicaría la guarda de «dentro de sí
// mismo» y el plan, que es donde estaría el error si se separaran.
#[expect(
    clippy::too_many_lines,
    reason = "copia y borrado comparten la guarda «dentro de sí mismo» y el plan"
)]
// Octavo argumento: el ancla del destino (#295), ver `copy_task`.
#[expect(
    clippy::too_many_arguments,
    reason = "Octavo argumento: el ancla del destino (#295), ver `copy_task`"
)]
async fn move_by_copy(
    src: Arc<dyn Provider>,
    dst: Arc<dyn Provider>,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
    dest_anchor: Option<norte_proto::DirAnchor>,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if Arc::ptr_eq(&src, &dst)
        && (is_descendant_folded(&to, &from, &*dst).await
            || same_node(&from, &to, &*dst, follow_links_for(opts), &ctx.cancel).await?)
    {
        return Err(Error::InvalidPath);
    }
    let src_entry = with_retry(&ctx.cancel, || src.stat(&from).boxed()).await?;
    // ¿Se mueve como ÁRBOL? Un dir siempre; un dir-symlink raíz solo bajo
    // Follow (issue #19): su contenido se expande en el destino y en el
    // origen se borra EL LINK.
    let tree_plan = match src_entry.kind {
        EntryKind::Dir => Some(plan_for(&*src, &from, opts, &ctx.cancel).await?),
        EntryKind::Symlink
            if opts.symlinks == SymlinkPolicy::Follow
                && probe_symlink_target(&*src, &from, &ctx.cancel).await? == TargetKind::Dir =>
        {
            Some(walk_following(&*src, &from, true, &ctx.cancel).await?)
        }
        EntryKind::File | EntryKind::Symlink => None,
        EntryKind::Other => return Err(Error::Unsupported),
    };
    match tree_plan {
        None => {
            ctx.progress.update(|p| {
                p.bytes_total = src_entry.size;
                // 2 pasos: copiar + borrar el origen.
                p.entries_total = Some(2);
                p.current = Some(from.clone());
            });
            // Misma raíz que en `copy_task` (#219): el directorio destino de
            // la hoja. Un movimiento por copia ESCRIBE igual que una copia, y
            // además borra el origen después — dejarlo sin confinar era la
            // mitad del agujero con la otra mitad al lado.
            let (dir_destino, raiz) = open_leaf_root(
                &*dst,
                &to,
                dest_anchor.as_ref(),
                ctx.progress.snapshot().task_id.get(),
                &ctx.cancel,
            )
            .await?;
            let into = destino_de_hoja(&*dst, dir_destino.as_ref(), raiz.as_deref(), &to);
            let placed = if src_entry.kind == EntryKind::File {
                copy_file_leaf(&*src, &into, &src_entry, &to, opts, &observer, ctx).await?
            } else {
                copy_symlink_leaf(&*src, &into, &src_entry, &to, opts, &observer, ctx).await?
            };
            ctx.progress.update(|p| p.entries_done = 1);
            if placed == Placed::Skipped {
                // No copiado ⇒ no se borra: el origen se conserva.
                ctx.progress.update(|p| p.entries_done = 2);
                return Ok(());
            }
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            remove_retrying(&*src, &from, &ctx.cancel).await?;
            observer
                .on_mutation(&Mutation::Removed(&from), &ctx.actor)
                .await?;
            ctx.progress.update(|p| p.entries_done = 2);
            Ok(())
        }
        Some(mut plan) => {
            anchor_parent_or_fail(
                &*dst,
                &to,
                dest_anchor.as_ref(),
                ctx.progress.snapshot().task_id.get(),
                &ctx.cancel,
            )
            .await?;
            hydrate_plan(&*src, &mut plan, ctx).await?;
            let skipped = copy_tree(&src, &dst, &from, &to, &plan, opts, &observer, ctx).await?;
            // Fase delete: el total crece con los pasos de borrado (la barra
            // sigue monótona; copy_tree ya contó los suyos).
            ctx.progress.update(|p| {
                p.entries_total = p.entries_total.map(|t| t + plan.len() as u64 + 1);
            });
            // Borra EXACTAMENTE lo copiado, en post-order. Lo saltado (y sus
            // ancestros) y lo aparecido tras el walk sobreviven: ese remove
            // ni se intenta (skip) o falla con Conflict (aparecido). La
            // provenance manda (issue #19): lo visto A TRAVÉS de un link es
            // del TARGET y jamás se borra; del link expandido se borra EL
            // LINK. Esquina DAG documentada: una hoja alcanzable por dos
            // caminos, saltada en uno y movida por el otro, termina solo en
            // el destino (sin pérdida: el contenido vive allí).
            for pe in plan.iter().rev() {
                if ctx.cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let keep = match pe.provenance {
                    Provenance::ViaLink => true,
                    Provenance::LinkRoot => {
                        skipped.iter().any(|s| is_descendant(s, &pe.entry.path))
                    }
                    Provenance::Real => {
                        skipped.contains(&pe.entry.path)
                            || (pe.entry.kind == EntryKind::Dir
                                && skipped.iter().any(|s| is_descendant(s, &pe.entry.path)))
                    }
                };
                if keep {
                    ctx.progress.update(|p| p.entries_done += 1);
                    continue;
                }
                ctx.progress
                    .update(|p| p.current = Some(pe.entry.path.clone()));
                remove_retrying(&*src, &pe.entry.path, &ctx.cancel).await?;
                observer
                    .on_mutation(&Mutation::Removed(&pe.entry.path), &ctx.actor)
                    .await?;
                ctx.progress.update(|p| p.entries_done += 1);
            }
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if skipped.is_empty() {
                // Raíz: para un dir, el dir ya vacío; para un dir-symlink
                // raíz bajo Follow, EL LINK (remove jamás sigue links).
                remove_retrying(&*src, &from, &ctx.cancel).await?;
                observer
                    .on_mutation(&Mutation::Removed(&from), &ctx.actor)
                    .await?;
            }
            ctx.progress.update(|p| p.entries_done += 1);
            Ok(())
        }
    }
}

/// Delete: `Trash` = UNA operación del provider sobre la raíz (el OS se
/// lleva el árbol entero — cancelable ANTES de disparar, no a mitad);
/// `Permanent` = recursivo post-order (los hijos caen antes que su padre;
/// cancelar a mitad deja el resto del árbol intacto, la raíz cae la
/// última). ADR 0009.
///
/// # La fila que no llega
/// Un observer que falla DESPUÉS de un entierro exitoso deja el fichero movido
/// y sin registrar (#160). Se compensa con `restore_from` cuando la papelera
/// nombra lo que se llevó, y se dice en el log llegue o no. Mismo criterio,
/// mismos límites y mismo TOCTOU de `restore_from` que
/// [`crate::sync::exec::bury`]: el efecto no se queda huérfano de su registro.
#[tracing::instrument(skip_all, fields(path = %path.display_lossy(), ?mode))]
pub(crate) async fn delete_task(
    provider: Arc<dyn Provider>,
    path: VPath,
    mode: DeleteMode,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    // #205: un borrado permanente recorre el árbol emitiendo una mutación por
    // entrada, así que es el caso más fácil de partir por la mitad — y el más
    // caro, porque lo que no lleva fila no se puede ni nombrar después.
    let observer = crate::observer::pin_for_task(observer).await?;
    if mode == DeleteMode::Trash {
        ctx.progress.update(|p| {
            p.entries_total = Some(1);
            p.current = Some(path.clone());
        });
        // Id determinista de la operación (#99): reloj de pared UNA vez +
        // el id numérico de la task como contador — estable en todo reintento.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let trash_id =
            norte_vfs::trash::TrashId::new(now_ms, ctx.progress.snapshot().task_id.get());
        let dest = trash_retrying(&*provider, &path, &trash_id, &ctx.cancel).await?;
        if let Err(e) = observer
            .on_mutation(
                &Mutation::Trashed {
                    path: &path,
                    dest: dest.as_ref(),
                },
                &ctx.actor,
            )
            .await
        {
            // #160: el fichero ya está enterrado y su registro no llegó — regla
            // dura 4 rota por el camino de un F8 corriente. Se devuelve a su
            // ruta y el borrado falla con el árbol como estaba. Igual que en
            // `sync::exec::bury`, solo se puede cuando la papelera NOMBRA lo que
            // se lleva: con `DestTrash::Opaque` (macOS, Windows) queda la línea
            // de log.
            let devuelto = match dest.as_ref() {
                Some(en) => provider.restore_from(en, &path).await,
                None => Err(Error::Unsupported),
            };
            tracing::error!(
                error = %e,
                enterrado = %crate::engine::span_path(&path),
                en = dest.as_ref().map(crate::engine::span_path),
                devuelto = devuelto.is_ok(),
                "fs.delete: se enterró el fichero y su entrada de journal NO llegó",
            );
            return Err(e);
        }
        ctx.progress.update(|p| p.entries_done = 1);
        return Ok(());
    }
    let entry = with_retry(&ctx.cancel, || provider.stat(&path).boxed()).await?;
    if entry.kind == EntryKind::Dir {
        let entries = walk(&*provider, &path, &ctx.cancel).await?;
        ctx.progress
            .update(|p| p.entries_total = Some(entries.len() as u64 + 1));
        // El walk emite cada padre antes que sus hijos: recorrerlo al revés
        // ES el post-order (todo dir llega vacío a su remove).
        for e in entries.iter().rev() {
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            ctx.progress.update(|p| p.current = Some(e.path.clone()));
            remove_retrying(&*provider, &e.path, &ctx.cancel).await?;
            observer
                .on_mutation(&Mutation::Removed(&e.path), &ctx.actor)
                .await?;
            ctx.progress.update(|p| p.entries_done += 1);
        }
    } else {
        ctx.progress.update(|p| p.entries_total = Some(1));
    }
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    remove_retrying(&*provider, &path, &ctx.cancel).await?;
    observer
        .on_mutation(&Mutation::Removed(&path), &ctx.actor)
        .await?;
    ctx.progress.update(|p| p.entries_done += 1);
    Ok(())
}

/// Task de `fs.mkdir` (#104, F7): UN directorio, sin `-p`. Pre-stat (#32)
/// para no reclamar jamás como nuestro un nodo preexistente: CUALQUIER nodo
/// previo — dir incluido — es `Conflict{Exists}` (crear afirma un nombre
/// LIBRE; la idempotencia silenciosa de `ensure_dir` es de los merges de
/// copia, no de un F7). Con el pre-stat en `NotFound`, `mkdir_retrying`
/// desambigua los transitorios (verificando VACÍO el dir ambiguo — ver su
/// rustdoc y la ventana residual que documenta) y el `Created` llega al
/// journal (regla 4) cuando el dir es nuestro.
pub(crate) async fn mkdir_task(
    provider: Arc<dyn Provider>,
    path: VPath,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    // Una sola mutación, así que aquí no hay mitad que partir. Se fija igual:
    // la regla es «toda Task que muta fija su veredicto», y una excepción por
    // ser corta es la que alguien alarga sin acordarse (#205).
    let observer = crate::observer::pin_for_task(observer).await?;
    ctx.progress.update(|p| {
        p.entries_total = Some(1);
        p.current = Some(path.clone());
    });
    match with_retry(&ctx.cancel, || provider.stat(&path).boxed()).await {
        Ok(_) => {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        Err(Error::NotFound) => {}
        Err(e) => return Err(e),
    }
    mkdir_retrying(&Dest::plain(&*provider, path.clone()), &ctx.cancel).await?;
    observer
        .on_mutation(&Mutation::Created(&path), &ctx.actor)
        .await?;
    ctx.progress.update(|p| p.entries_done = 1);
    Ok(())
}

/// Crea un fichero VACÍO (#290). Lo mismo que [`mkdir_task`] con la otra clase
/// de nodo, y con sus mismas reglas.
///
/// **La exclusividad NO la pone este `stat`: la pone el provider.**
/// [`Provider::write`](norte_vfs::Provider::write) contrata create-new, y cada
/// implementación lo cumple con la fuerza que su transporte permite — el local
/// con un `rename_noreplace` atómico en el commit, el de objetos con un
/// `If-None-Match`, `MemProvider` revalidando bajo su lock. La única ventana
/// TOCTOU real es la de SFTP, y es de SFTP: v3 no tiene rename atómico.
///
/// El `stat` de aquí es lo mismo que hace [`mkdir_task`] y por el mismo motivo:
/// dar un `Conflict` limpio y TEMPRANO —antes de crear el staging— y no
/// reclamar como nuestro un nodo que ya estaba. Quitarlo no abriría un agujero;
/// solo movería el error más tarde y más feo.
pub(crate) async fn create_task(
    provider: Arc<dyn Provider>,
    path: VPath,
    dest_anchor: Option<norte_proto::DirAnchor>,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let observer = crate::observer::pin_for_task(observer).await?;
    ctx.progress.update(|p| {
        p.entries_total = Some(1);
        p.current = Some(path.clone());
    });
    // El ancla ANTES del `stat`: si el directorio ya no es el que el humano
    // listó, no hay nada que comprobar dentro de él.
    anchor_parent_or_fail(
        &*provider,
        &path,
        dest_anchor.as_ref(),
        ctx.progress.snapshot().task_id.get(),
        &ctx.cancel,
    )
    .await?;
    match with_retry(&ctx.cancel, || provider.stat(&path).boxed()).await {
        Ok(_) => {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        Err(Error::NotFound) => {}
        Err(e) => return Err(e),
    }
    // Abrir y cerrar: el sink publica su staging vacío, que es exactamente un
    // fichero de cero bytes en el destino. Sin `write` ninguno de por medio.
    let sink = provider.write(&path).await?;
    sink.commit().await?;
    observer
        .on_mutation(&Mutation::Created(&path), &ctx.actor)
        .await?;
    ctx.progress.update(|p| p.entries_done = 1);
    Ok(())
}

/// Qué hacer si el destino de [`write_task`] ya existe (ADR 0101).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExists {
    /// `Conflict{Exists}`: no se toca nada.
    Refuse,
    /// Lo que había va a la papelera lógica ANTES de crear el nuevo: dos
    /// entradas de journal seguidas (`trashed`, `created`), y el contenido
    /// anterior con vuelta atrás. Nunca se sobrescribe en sitio.
    Replace,
}

/// Escribe un fichero con CONTENIDO desde memoria (ADR 0101): lo que un hook
/// pide como sidecar. Es [`create_task`] con bytes y con una política de
/// «ya existe» explícita; no lleva ancla porque no viene de un listado.
#[tracing::instrument(skip_all, fields(path = %path.display_lossy(), ?on_exists, bytes = content.len()))]
pub(crate) async fn write_task(
    provider: Arc<dyn Provider>,
    path: VPath,
    content: Vec<u8>,
    on_exists: OnExists,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let observer = crate::observer::pin_for_task(observer).await?;
    ctx.progress.update(|p| {
        p.entries_total = Some(1);
        p.current = Some(path.clone());
    });
    match with_retry(&ctx.cancel, || provider.stat(&path).boxed()).await {
        Ok(_) if on_exists == OnExists::Refuse => {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        Ok(_) => {
            // Mismo entierro que `delete_task`, misma vuelta atrás si la fila
            // no llega (#160).
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            let trash_id =
                norte_vfs::trash::TrashId::new(now_ms, ctx.progress.snapshot().task_id.get());
            let dest = trash_retrying(&*provider, &path, &trash_id, &ctx.cancel).await?;
            if let Err(e) = observer
                .on_mutation(
                    &Mutation::Trashed {
                        path: &path,
                        dest: dest.as_ref(),
                    },
                    &ctx.actor,
                )
                .await
            {
                let devuelto = match dest.as_ref() {
                    Some(en) => provider.restore_from(en, &path).await,
                    None => Err(Error::Unsupported),
                };
                tracing::error!(
                    error = %e,
                    enterrado = %crate::engine::span_path(&path),
                    devuelto = devuelto.is_ok(),
                    "sidecar: se enterró el anterior y su entrada de journal NO llegó",
                );
                return Err(e);
            }
        }
        Err(Error::NotFound) => {}
        Err(e) => return Err(e),
    }
    let mut sink = provider.write(&path).await?;
    if let Err(e) = sink.write(bytes::Bytes::from(content)).await {
        let _ = sink.abort().await;
        return Err(e);
    }
    sink.commit().await?;
    observer
        .on_mutation(&Mutation::Created(&path), &ctx.actor)
        .await?;
    ctx.progress.update(|p| p.entries_done = 1);
    Ok(())
}

/// Cambia los permisos POSIX de un lote de rutas (#314).
///
/// **Una entrada de journal POR RUTA**, con el modo anterior como reversa, y
/// se registra ANTES de pasar a la siguiente: un lote a medias tiene que dejar
/// deshecho lo que ya hizo, y una entrada por lote no diría cuáles.
///
/// El modo anterior se LEE antes de escribir el nuevo. Si no se puede leer, la
/// mutación se registra igual y como irreversible: cambiar los permisos sin
/// poder decir cuáles eran es lo que pasa de verdad, y callarlo o abortar
/// serían las dos formas de mentir sobre ello.
///
/// **Una ruta que falla no tumba el lote**: se cuenta como ilegible en el
/// progreso y las demás se cambian. El caso típico es una selección con un
/// fichero de otro dueño dentro, y perder las otras cincuenta por ella sería
/// castigar al que marcó bien.
///
/// La cancelación se mira por RUTA (regla 3): un `chmod` no se puede partir.
/// Expande las raíces de un `set_mode` recursivo a la lista de nodos que se
/// van a tocar, y dice cuántos quedaron SIN VISITAR por el tope (#315).
///
/// El orden es de arriba abajo —la raíz antes que su contenido— y eso importa:
/// quitarle a un directorio el bit de ejecución antes de recorrerlo dejaría el
/// resto del árbol inalcanzable a mitad de la operación. Con `dir_mode` a
/// `755` no pasa; con el mismo modo para todo, sí, y es el pie de bala que la
/// documentación nombra. Aun así se recorre entero ANTES de tocar nada, así
/// que ese árbol se cambia completo y lo que queda inalcanzable es lo de
/// después, no lo de este lote.
///
/// Un SYMLINK no se sigue: se añade como nodo y `set_mode` lo salta con su
/// motivo. Seguirlo saldría del árbol que el humano señaló, que es lo que la
/// ADR 0072 lleva entera diciendo.
///
/// Un directorio que no se puede listar no mata la operación: se cuenta como
/// no visitado, como hace `dir_size`. Morirse en el `EACCES` de la hoja 40 000
/// devolvería nada a cambio de todo el trabajo hecho.
async fn expandir_arbol(
    raices: Vec<(Arc<dyn Provider>, VPath)>,
    ctx: &TaskCtx,
) -> Result<(Vec<(Arc<dyn Provider>, VPath)>, u64), Error> {
    let tope = usize::try_from(norte_proto::methods::SET_MODE_RECURSIVE_MAX).unwrap_or(usize::MAX);
    let mut salida: Vec<(Arc<dyn Provider>, VPath)> = Vec::new();
    let mut sin_visitar: u64 = 0;
    let mut pendientes: std::collections::VecDeque<(Arc<dyn Provider>, VPath)> =
        raices.into_iter().collect();
    while let Some((provider, path)) = pendientes.pop_front() {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if salida.len() >= tope {
            // Lo que queda en la cola, DE UNA VEZ, y se corta. Contarlos uno a
            // uno sacándolos daba la profundidad de la cola en el momento del
            // corte y no lo que falta del árbol; y seguir sacando para contar
            // recorre una lista que ya no se va a expandir.
            sin_visitar = sin_visitar.saturating_add(
                u64::try_from(pendientes.len().saturating_add(1)).unwrap_or(u64::MAX),
            );
            break;
        }
        // Un `stat` que falla NO se toca a ciegas (#315): sin él no se sabe si
        // es un enlace, y `chmod(2)` sigue los enlaces — se cambiaría un
        // fichero que puede estar fuera del árbol que el humano señaló. Se
        // cuenta como no visitado y se sigue.
        let Ok(entrada) = with_retry(&ctx.cancel, || provider.stat(&path).boxed()).await else {
            sin_visitar = sin_visitar.saturating_add(1);
            continue;
        };
        let dir = entrada.kind == EntryKind::Dir;
        salida.push((Arc::clone(&provider), path.clone()));
        if !dir {
            continue;
        }
        match provider.list(&path).await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(e) => pendientes.push_back((Arc::clone(&provider), e.path)),
                        // Una entrada ilegible del listado no tumba el
                        // recorrido; se cuenta y se sigue.
                        Err(_) => sin_visitar = sin_visitar.saturating_add(1),
                    }
                }
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(_) => sin_visitar = sin_visitar.saturating_add(1),
        }
    }
    Ok((salida, sin_visitar))
}

pub(crate) struct SetModeOptions {
    /// Los doce bits para lo que no es un directorio.
    pub(crate) mode: u32,
    /// Bajar por los directorios de lo pedido (#315).
    pub(crate) recursive: bool,
    /// El modo de los DIRECTORIOS. `None` = el mismo que el de los ficheros,
    /// que es lo que hace `chmod -R` y lo que deja un árbol sin bit de
    /// ejecución donde hacía falta.
    pub(crate) dir_mode: Option<u32>,
    /// El lote bajo el que se agrupan las entradas del diario cuando esto es
    /// recursivo (#315). `None` = un cambio suelto, o un journal que no puede
    /// dar ids de lote.
    pub(crate) batch: Option<i64>,
}

pub(crate) async fn set_mode(
    paths: Vec<(Arc<dyn Provider>, VPath)>,
    opts: SetModeOptions,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let observer = crate::observer::pin_for_task(observer).await?;
    // Con recursivo, lo pedido es la RAÍZ y no la lista: se expande antes de
    // empezar para que el progreso diga cuántos son de verdad. Expandir sobre
    // la marcha dejaría un total que sube mientras el lector lo mira, y el
    // corte por tope no se podría decir hasta el final.
    let (paths, sin_visitar) = if opts.recursive {
        expandir_arbol(paths, ctx).await?
    } else {
        (paths, 0)
    };
    let total = u64::try_from(paths.len()).unwrap_or(u64::MAX);
    ctx.progress.update(|p| {
        p.entries_total = Some(total);
        p.entries_done = 0;
    });
    if sin_visitar > 0 {
        // El tope se dice ANTES de tocar nada, y por su PROPIO campo: mezclarlo
        // con `unreadable` hacía que el frontend dijera «no se pudieron
        // cambiar (un enlace, o no es tuyo)» sobre nodos que ni se miraron.
        ctx.progress.update(|p| p.unvisited = Some(sin_visitar));
        tracing::warn!(
            sin_visitar,
            tope = norte_proto::methods::SET_MODE_RECURSIVE_MAX,
            "fs.set_mode recursivo: el árbol se pasa del tope de nodos"
        );
    }
    let mut hechos: u64 = 0;
    let mut fallidas: u64 = 0;
    for (provider, path) in paths {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        ctx.progress.update(|p| p.current = Some(path.clone()));
        // Un SYMLINK no se toca, y esto no es remilgo: `chmod(2)` SIGUE el
        // enlace mientras que el `stat` de esta misma función NO lo sigue
        // (lstat, contrato del trait). O sea que el modo que se guardaría como
        // reversa sería el del ENLACE —`0o777` siempre en Linux— y deshacer
        // dejaría el DESTINO abierto a todo el mundo. Y hay algo peor que la
        // reversa: el destino puede estar fuera del scope que alguien aprobó,
        // así que un chmod sobre un enlace es una escritura que se sale de su
        // raíz. Se cuenta como fallida, y el frontend lo dice.
        let entrada = provider.stat(&path).await;
        if matches!(&entrada, Ok(e) if e.kind == EntryKind::Symlink) {
            fallidas = fallidas.saturating_add(1);
            hechos = hechos.saturating_add(1);
            ctx.progress.update(|p| {
                p.entries_done = hechos;
                p.unreadable = Some(fallidas);
            });
            continue;
        }
        // El modo de un DIRECTORIO puede ser otro (#315): `chmod -R 644` sobre
        // un árbol lo deja inutilizable —sin bit de ejecución no se entra— y
        // `dir_mode` es la salida explícita a eso. Sin él, el mismo para todo.
        let es_dir = matches!(&entrada, Ok(e) if e.kind == EntryKind::Dir);
        let mode = if es_dir {
            opts.dir_mode.unwrap_or(opts.mode)
        } else {
            opts.mode
        };
        let anterior = crate::undo::modo_actual(provider.as_ref(), &path).await;
        match provider.set_mode(&path, mode).await {
            Ok(()) => {
                // El modo que QUEDÓ, releído: `chmod(2)` limpia setgid en
                // silencio cuando quien llama no pertenece al grupo del
                // fichero, y un diario que dijera `2755` sobre un `755` real
                // mentiría en la dirección peligrosa. Si no se puede releer,
                // se apunta el que se pidió, que es lo único que se sabe.
                let quedo = crate::undo::modo_actual(provider.as_ref(), &path)
                    .await
                    .unwrap_or(mode);
                observer
                    .on_mutation(
                        &Mutation::ModeChanged {
                            path: &path,
                            from: anterior,
                            to: quedo,
                            batch: opts.batch,
                        },
                        &ctx.actor,
                    )
                    .await?;
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(_) => fallidas = fallidas.saturating_add(1),
        }
        hechos = hechos.saturating_add(1);
        ctx.progress.update(|p| {
            p.entries_done = hechos;
            // #251: un `Completed` sobre un lote donde la mitad no se pudo
            // cambiar se lee como un total confiado si el progreso no lo dice.
            p.unreadable = Some(fallidas);
        });
    }
    Ok(())
}

/// Cuánto ocupan `roots`, contando lo que se pueda leer (#139).
///
/// El total NO se devuelve: viaja en el progreso (`bytes_done`/`entries_done`),
/// que ya existe y que todo frontend sabe pintar. El último snapshot ES el
/// resultado, y por eso esta función no inventa un tipo nuevo.
///
/// **Un directorio ilegible no mata el recuento.** Contar un árbol grande puede
/// tardar minutos, y morirse en un `EACCES` de la hoja 40 000 devolvería nada a
/// cambio de todo el trabajo hecho: lo que sale es el tamaño de lo que se pudo
/// leer. La cancelación sí para: es una orden, no un tropiezo.
///
/// No materializa el árbol —a diferencia de [`walk`], que devuelve un `Vec`—
/// porque aquí no hace falta ninguna entrada después de sumarla, y un
/// directorio de diez millones de ficheros no cabe dos veces en memoria por
/// gusto.
pub(crate) async fn dir_size(
    roots: Vec<(std::sync::Arc<dyn Provider>, VPath)>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let mut bytes: u64 = 0;
    let mut entries: u64 = 0;
    let mut ilegibles: u64 = 0;
    for (provider, root) in roots {
        // La raíz cuenta por sí misma: medir un FICHERO suelto es una pregunta
        // legítima y no recorre nada.
        match provider.stat(&root).await {
            Ok(e) if e.kind != EntryKind::Dir => {
                bytes = bytes.saturating_add(e.size.unwrap_or(0));
                entries = entries.saturating_add(1);
                ctx.progress.update(|p| {
                    p.bytes_done = bytes;
                    p.entries_done = entries;
                    p.unreadable = Some(ilegibles);
                });
                continue;
            }
            Ok(_) => {}
            // Una raíz que no se deja ni mirar cuenta como ilegible y no tumba
            // el recuento de las demás: una selección de veinte carpetas no se
            // pierde por una.
            Err(e) => {
                if matches!(e, Error::Cancelled) {
                    return Err(Error::Cancelled);
                }
                ilegibles = ilegibles.saturating_add(1);
                continue;
            }
        }
        let mut pending = vec![root];
        while let Some(dir) = pending.pop() {
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut stream = match provider.list(&dir).await {
                Ok(s) => s,
                Err(e) => {
                    if matches!(e, Error::Cancelled) {
                        return Err(Error::Cancelled);
                    }
                    ilegibles = ilegibles.saturating_add(1);
                    continue;
                }
            };
            ctx.progress.update(|p| p.current = Some(dir.clone()));
            while let Some(item) = stream.next().await {
                // Inner loop de verdad (regla 3): un dir de 10^6 entradas o un
                // provider lento no pueden retrasar la cancelación al pop.
                if ctx.cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let entry = match item {
                    Ok(e) => e,
                    Err(e) => {
                        if matches!(e, Error::Cancelled) {
                            return Err(Error::Cancelled);
                        }
                        ilegibles = ilegibles.saturating_add(1);
                        continue;
                    }
                };
                entries = entries.saturating_add(1);
                if entry.kind == EntryKind::Dir {
                    pending.push(entry.path);
                } else {
                    // Un listado PEREZOSO no trae tamaños (#52: el provider
                    // local los deja en `None` y quien los necesita los pide),
                    // así que aquí hay que pedirlos: sumar `unwrap_or(0)` daba
                    // «0 B» para un árbol entero, que es la respuesta más
                    // equivocada posible a la única pregunta que se hizo.
                    //
                    // Un `stat` por fichero es lo que hace `du`, y es lo que
                    // hace la hidratación de una copia (`hydrate_plan`). En un
                    // provider que SÍ trae el tamaño en el listado no se pide
                    // nada. En serie, como la hidratación: contra SFTP eso son
                    // N viajes y ya está anotado como #156 para las dos.
                    let size = match entry.size {
                        Some(n) => Some(n),
                        None => match provider.stat(&entry.path).await {
                            Ok(st) => st.size,
                            Err(Error::Cancelled) => return Err(Error::Cancelled),
                            // Un fichero que se deja listar y no statear cuenta
                            // como ilegible: su tamaño no se sabe, y el resto
                            // del recuento no se pierde por él.
                            Err(_) => {
                                ilegibles = ilegibles.saturating_add(1);
                                None
                            }
                        },
                    };
                    bytes = bytes.saturating_add(size.unwrap_or(0));
                }
                ctx.progress.update(|p| {
                    p.bytes_done = bytes;
                    p.entries_done = entries;
                    p.unreadable = Some(ilegibles);
                });
            }
        }
    }
    // El número VIAJA (#251), no se queda en un log. `fs.dir_size` existe para
    if ilegibles > 0 {
        tracing::info!(
            ilegibles,
            "fs.dir_size: partes del árbol no se pudieron leer"
        );
    }
    ctx.progress.update(|p| {
        p.bytes_done = bytes;
        p.entries_done = entries;
        // Al terminar, el total ES lo contado: decirlo cierra la barra en vez
        // de dejarla en un «de cuánto» que nunca llegó.
        p.bytes_total = Some(bytes);
        p.entries_total = Some(entries);
        // Y el número VIAJA (#251), no se queda en el log de arriba: este
        // método existe para contestar «¿cabe esto en el destino?», y un
        // árbol del que la mitad dio `EACCES` reportaba `Completed` con un
        // total confiado y demasiado pequeño. Con esto, quien pinte dice «al
        // menos X». `Some` siempre: `fs.dir_size` SÍ cuenta ilegibles, y
        // `Some(0)` es una respuesta —«los conté y no hubo»— que `None` no
        // sabe dar.
        p.unreadable = Some(ilegibles);
        p.current = None;
    });
    Ok(())
}

/// Recorre el árbol bajo `root` (sin incluirlo). Garantía de orden: todo
/// directorio aparece ANTES que cualquiera de sus descendientes.
pub(crate) async fn walk(
    provider: &dyn Provider,
    root: &VPath,
    cancel: &CancellationToken,
) -> Result<Vec<Entry>, Error> {
    let mut out = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut stream = provider.list(&dir).await?;
        while let Some(item) = stream.next().await {
            // Inner loop de verdad (regla 3): un dir de 10^6 entradas o un
            // provider lento no pueden retrasar la cancelación al pop.
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let entry = item?;
            if entry.kind == EntryKind::Dir {
                pending.push(entry.path.clone());
            }
            out.push(entry);
        }
    }
    Ok(out)
}

/// Entrada del plan de copia: la provenance decide cómo la trata el
/// DELETE de un move (la copia la ignora — issue #19).
#[derive(Debug)]
struct PlanEntry {
    entry: Entry,
    provenance: Provenance,
}

/// Origen de una entrada del plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provenance {
    /// Dirent real del árbol origen: se borra en un move.
    Real,
    /// Dir-symlink REAL expandido por Follow: en un move se borra EL LINK
    /// (un solo remove), jamás su contenido.
    LinkRoot,
    /// Visto A TRAVÉS de un link expandido: pertenece al TARGET del link;
    /// un move jamás lo borra (el `LinkRoot` se lleva el link).
    ViaLink,
}

/// ¿A qué apunta un symlink?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    /// Archivo — o roto: la hoja Follow dará su error honesto al copiar.
    File,
    /// Directorio.
    Dir,
}

/// Sondea el tipo del target de un symlink SIN abrir el nodo: `list()`
/// valida con metadata que sigue el link (Ok = dir; `TypeMismatch` =
/// archivo u otro; `NotFound` = roto — la hoja Follow dará su error
/// honesto al copiar). Jamás `read()`: abrir un symlink→FIFO bloquearía
/// el hilo blocking sin respetar la cancelación (hallazgo M3 del
/// rust-reviewer). Soltar el stream cancela el listado.
async fn probe_symlink_target(
    provider: &dyn Provider,
    p: &VPath,
    cancel: &CancellationToken,
) -> Result<TargetKind, Error> {
    match with_retry(cancel, || provider.list(p).boxed()).await {
        Ok(probe) => {
            drop(probe);
            Ok(TargetKind::Dir)
        }
        // TypeMismatch = archivo/otro; NotFound = roto → en ambos casos la
        // hoja Follow decide (y dará su error honesto si aplica).
        Err(
            Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            }
            | Error::NotFound,
        ) => Ok(TargetKind::File),
        Err(e) => Err(e),
    }
}

/// Plan de un árbol: walk plano (todo `Real`) salvo bajo Follow, donde los
/// dir-symlinks se expanden con detección de ciclos (issue #19).
async fn plan_for(
    provider: &dyn Provider,
    root: &VPath,
    opts: TransferOptions,
    cancel: &CancellationToken,
) -> Result<Vec<PlanEntry>, Error> {
    if opts.symlinks == SymlinkPolicy::Follow {
        walk_following(provider, root, false, cancel).await
    } else {
        Ok(walk(provider, root, cancel)
            .await?
            .into_iter()
            .map(|entry| PlanEntry {
                entry,
                provenance: Provenance::Real,
            })
            .collect())
    }
}

/// Un directorio pendiente del walk con Follow: su path, la cadena de
/// identidades de sus ancestros (ciclos, spec §17.9) y si se llegó a él a
/// través de un link expandido.
struct DirFrame {
    dir: VPath,
    ancestors: Vec<NodeId>,
    via_link: bool,
}

/// Walk con expansión de dir-symlinks (`SymlinkPolicy::Follow`, issue
/// #19): cada symlink se sondea; los que apuntan a dir se convierten en
/// dirs sintéticos y se desciende A TRAVÉS del link. Un link cuyo target
/// resuelto ya está en la cadena de ancestros es un CICLO → [`Error::Loop`].
/// Expandir exige identidad ([`Provider::node_id`]): sin ella,
/// `Unsupported` — exactamente el comportamiento M1 (los árboles sin
/// dir-symlinks no la necesitan y siguen funcionando).
///
/// `root_is_link` = la raíz misma es un dir-symlink a expandir (todo el
/// contenido queda `ViaLink` y el move borra solo el link raíz).
async fn walk_following(
    provider: &dyn Provider,
    root: &VPath,
    root_is_link: bool,
    cancel: &CancellationToken,
) -> Result<Vec<PlanEntry>, Error> {
    // La identidad de la raíz abre la cadena de ancestros. Para una raíz
    // link es OBLIGATORIA (expandir sin visited set sería ruleta rusa);
    // para un dir normal, best-effort (sin ids solo fallará si aparece un
    // dir-symlink que expandir).
    let root_id =
        match with_retry(cancel, || provider.node_id(root, FollowLinks::Yes).boxed()).await? {
            Some(id) => Some(id),
            None if root_is_link => return Err(Error::Unsupported),
            None => None,
        };
    let mut out: Vec<PlanEntry> = Vec::new();
    let mut pending = vec![DirFrame {
        dir: root.clone(),
        ancestors: root_id.into_iter().collect(),
        via_link: root_is_link,
    }];
    while let Some(frame) = pending.pop() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut stream = provider.list(&frame.dir).await?;
        while let Some(item) = stream.next().await {
            // Inner loop de verdad (regla 3), como en walk().
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let entry = item?;
            let provenance = if frame.via_link {
                Provenance::ViaLink
            } else {
                Provenance::Real
            };
            match entry.kind {
                EntryKind::Dir => {
                    let id = with_retry(cancel, || {
                        provider.node_id(&entry.path, FollowLinks::Yes).boxed()
                    })
                    .await?;
                    let mut ancestors = frame.ancestors.clone();
                    ancestors.extend(id);
                    pending.push(DirFrame {
                        dir: entry.path.clone(),
                        ancestors,
                        via_link: frame.via_link,
                    });
                    out.push(PlanEntry { entry, provenance });
                }
                EntryKind::Symlink => {
                    match probe_symlink_target(provider, &entry.path, cancel).await? {
                        TargetKind::File => out.push(PlanEntry { entry, provenance }),
                        TargetKind::Dir => {
                            let Some(id) = with_retry(cancel, || {
                                provider.node_id(&entry.path, FollowLinks::Yes).boxed()
                            })
                            .await?
                            else {
                                return Err(Error::Unsupported);
                            };
                            if frame.ancestors.contains(&id) {
                                // Ciclo: seguirlo copiaría infinito. Categoría
                                // propia desde 0.4.0 (#31, ADR 0011).
                                return Err(Error::Loop);
                            }
                            let mut ancestors = frame.ancestors.clone();
                            ancestors.push(id);
                            pending.push(DirFrame {
                                dir: entry.path.clone(),
                                ancestors,
                                via_link: true,
                            });
                            // Dir SINTÉTICO: la copia crea un dir real en
                            // el destino. El mtime de referencia viene del
                            // `entry` del link tal cual lo dio el listado —
                            // con un listado local lazy (#52) hoy es casi
                            // siempre `None` (`hydrate_plan` no toca Dirs);
                            // ningún consumidor lo lee todavía.
                            out.push(PlanEntry {
                                entry: Entry {
                                    // attrs: dir SINTÉTICO, vacío a
                                    // propósito (decisión del bloque 2 de
                                    // #108): un `PlanEntry` es interno del
                                    // core — los atributos son presentación
                                    // de listados y NO se propagan del link
                                    // consumido (describirían al LINK, no al
                                    // dir sintético que lo sustituye).
                                    attrs: std::collections::BTreeMap::new(),
                                    path: entry.path,
                                    kind: EntryKind::Dir,
                                    size: None,
                                    mtime_ms: entry.mtime_ms,
                                },
                                provenance: if frame.via_link {
                                    Provenance::ViaLink
                                } else {
                                    Provenance::LinkRoot
                                },
                            });
                        }
                    }
                }
                EntryKind::File | EntryKind::Other => out.push(PlanEntry { entry, provenance }),
            }
        }
    }
    Ok(out)
}

/// Reubica `path` (descendiente de `from`) bajo `to`, segmento a segmento.
fn rebase(path: &VPath, from: &VPath, to: &VPath) -> Result<VPath, Error> {
    let prefix_len = from.segments().count();
    let mut target = to.clone();
    for seg in path.segments().skip(prefix_len) {
        // Invariante: los segmentos vienen de un VPath ya validado.
        let seg = Segment::new(seg.to_vec()).map_err(|_| Error::Internal { panic: false })?;
        target = target.join(seg);
    }
    Ok(target)
}

/// `true` si `child` es descendiente PROPIO de `ancestor` (mismo scheme y
/// authority, prefijo estricto de segmentos), BYTE A BYTE.
///
/// Vale para comparar dos paths que salieron de la MISMA foto de listado —
/// donde los bytes ya son los que el provider dio— y NO vale para decidir si
/// una transferencia cae dentro de su propio origen: eso se pregunta contra el
/// volumen destino con [`is_descendant_folded`] (#269).
fn is_descendant(child: &VPath, ancestor: &VPath) -> bool {
    if child.scheme() != ancestor.scheme() || child.authority() != ancestor.authority() {
        return false;
    }
    let a: Vec<&[u8]> = ancestor.segments().collect();
    let c: Vec<&[u8]> = child.segments().collect();
    c.len() > a.len() && c[..a.len()] == a[..]
}

/// La misma pregunta, plegando los segmentos bajo el modo del volumen DESTINO
/// (#269) — la trampa del dominio de siempre: la caja y la normalización las
/// decide el destino, no el origen.
///
/// El escenario que la abrió: panel A en `/casa` sobre `docs`, panel B
/// navegado a `/casa/DOCS`, que en APFS o NTFS **es** `/casa/docs`. Entonces
/// `to = /casa/DOCS/docs` y la comparación de bytes decía que no, mientras
/// `same_node` salía antes porque el número de segmentos difiere. Ninguna de
/// las dos guardas saltaba y el árbol se copiaba dentro de sí mismo: acotado
/// —el plan es una foto— pero es exactamente lo que la guarda existe para
/// impedir, y el `docs/docs` anidado es una trampa para quien luego limpie.
///
/// Bajo `FoldMode::None` es literalmente [`is_descendant`]: sin pliegue, la
/// comparación de bytes ya es la respuesta.
async fn is_descendant_folded(child: &VPath, ancestor: &VPath, dst: &dyn Provider) -> bool {
    if child.scheme() != ancestor.scheme() || child.authority() != ancestor.authority() {
        return false;
    }
    let a: Vec<&[u8]> = ancestor.segments().collect();
    let c: Vec<&[u8]> = child.segments().collect();
    if c.len() <= a.len() {
        return false;
    }
    let mode = fold_mode_at(child, dst).await;
    if mode == norte_encoding::FoldMode::None {
        return c[..a.len()] == a[..];
    }
    a.iter().zip(&c).all(|(x, y)| {
        x == y || norte_encoding::name_key(x, mode) == norte_encoding::name_key(y, mode)
    })
}

/// El digest del CONTENIDO de cada ruta, en el orden en que se pidieron
/// (`fs.checksum`, 0.59.0, #311).
///
/// El informe se rellena SEGÚN se calcula, no al final: quien lo pide mientras
/// corre ve lo que lleva, que es lo que hace útil comprobar cien ficheros sin
/// esperar a los cien. `pending` es lo que falta, y llega a cero con el último.
///
/// **Cancelar deja `pending > 0` en una Task ya terminal**, y eso no es un
/// descuido: es la señal de que el informe está a medias. Quien lo lea tiene
/// que mirar el estado de la Task además del número — un lector que solo
/// sondease `pending == 0` no pararía nunca, y uno que tratase el informe
/// truncado como definitivo diría «falta» de ficheros que nadie llegó a mirar.
///
/// **Un fichero ilegible no mata el lote**, igual que en [`dir_size`]: sale con
/// su motivo y los demás se calculan. Un DIRECTORIO no se recorre — sale
/// marcado, porque hashear un árbol es otra pregunta con su propio formato.
///
/// **La cancelación se mira por TROZO**, no por fichero (regla 3): mirarla por
/// fichero dejaría que cancelar en mitad de uno de 40 GB esperase a terminar de
/// leerlo, que es justo cuando alguien cancela.
///
/// No materializa el contenido: se lee en los trozos que dé el provider y solo
/// vive uno a la vez, así que un fichero de 40 GB cuesta 40 GB de lectura y no
/// de memoria.
pub(crate) async fn checksum(
    paths: Vec<(std::sync::Arc<dyn Provider>, VPath)>,
    informe: std::sync::Arc<std::sync::Mutex<norte_proto::methods::FsChecksumReportResult>>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    use norte_proto::methods::{ChecksumEntry, ChecksumMiss};

    let total = u64::try_from(paths.len()).unwrap_or(u64::MAX);
    {
        let mut r = informe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        r.pending = total;
    }
    ctx.progress.update(|p| {
        p.entries_total = Some(total);
        p.entries_done = 0;
    });
    let mut bytes: u64 = 0;
    let mut hechos: u64 = 0;
    let mut ilegibles: u64 = 0;
    for (provider, path) in paths {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        ctx.progress.update(|p| p.current = Some(path.clone()));
        // Lo que no es un fichero no se lee: un directorio aquí es una
        // selección que incluía una carpeta, no un error de quien pidió.
        let entrada = match provider.stat(&path).await {
            Ok(e) if e.kind == EntryKind::Dir => ChecksumEntry {
                path: path.clone(),
                digest: None,
                miss: Some(ChecksumMiss::NotAFile),
            },
            Ok(_) => match digest_de(provider.as_ref(), &path, &ctx.cancel).await {
                Ok(d) => {
                    bytes = bytes.saturating_add(d.1);
                    ChecksumEntry {
                        path: path.clone(),
                        digest: Some(d.0),
                        miss: None,
                    }
                }
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(_) => ChecksumEntry {
                    path: path.clone(),
                    digest: None,
                    miss: Some(ChecksumMiss::Unreadable),
                },
            },
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(_) => ChecksumEntry {
                path: path.clone(),
                digest: None,
                miss: Some(ChecksumMiss::Unreadable),
            },
        };
        hechos = hechos.saturating_add(1);
        if entrada.digest.is_none() {
            ilegibles = ilegibles.saturating_add(1);
        }
        {
            let mut r = informe
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            r.entries.push(entrada);
            r.pending = total.saturating_sub(hechos);
        }
        ctx.progress.update(|p| {
            p.entries_done = hechos;
            p.bytes_done = bytes;
            // #251: un `Completed` con la mitad del lote sin digest se lee como
            // un total confiado si el progreso no lo dice. El informe lo dice
            // entero, pero quien solo mira el tablero ve esto.
            p.unreadable = Some(ilegibles);
        });
    }
    Ok(())
}

/// El sha256 de `path` en hex minúscula, y cuántos bytes se leyeron.
///
/// El hex en MINÚSCULA siempre, como el resto de digests del protocolo: dos
/// escrituras del mismo hash que comparan distinto son un bug esperando.
async fn digest_de(
    provider: &dyn Provider,
    path: &VPath,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(String, u64), Error> {
    use futures::StreamExt as _;
    use sha2::{Digest as _, Sha256};

    let mut stream = provider.read(path, None).await?;
    let mut hasher = Sha256::new();
    let mut leidos: u64 = 0;
    while let Some(trozo) = stream.next().await {
        // Por TROZO y no por fichero (regla 3).
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let trozo = trozo?;
        leidos = leidos.saturating_add(trozo.len() as u64);
        hasher.update(&trozo);
    }
    Ok((norte_proto::hashing::hex_lower(&hasher.finalize()), leidos))
}

/// Cómo pliega nombres el volumen que contiene `at`, preguntado al provider
/// DESTINO. `capabilities_at` responde por el MOUNT (#215): un pincho FAT bajo
/// un `/home` sensible a la caja no hereda la respuesta de `/home`.
async fn fold_mode_at(at: &VPath, dst: &dyn Provider) -> norte_encoding::FoldMode {
    let caps = dst
        .capabilities_at(at)
        .await
        .unwrap_or_else(|_| dst.capabilities());
    norte_compare::Sides::mode_of(caps)
}

#[cfg(test)]
mod tests {
    use norte_proto::{Entry, Error};

    use super::{is_descendant_folded, rename_auto_candidate, same_node_heuristic};

    /// #269 — la guarda de «dentro de sí mismo» comparaba los prefijos BYTE A
    /// BYTE, y en un volumen que pliega (APFS, NTFS, exFAT, un SMB/SFTP contra
    /// un servidor que pliega) `/casa/DOCS` **es** `/casa/docs`. Con el panel
    /// destino navegado ahí, `from = /casa/docs` y `to = /casa/DOCS/docs`: la
    /// guarda no saltaba, `same_node` salía antes porque el número de
    /// segmentos difiere, y el core copiaba un árbol dentro de sí mismo.
    #[tokio::test]
    async fn dentro_de_si_mismo_pliega_bajo_el_modo_del_destino() {
        use norte_proto::{CapabilityFlags, VPath};
        use norte_testkit::MemProvider;

        let vp = |w: &str| VPath::parse(w).expect("wire");
        let from = vp("mem:///casa/docs");
        let to = vp("mem:///casa/DOCS/docs");

        // Volumen que DISTINGUE caja: son dos árboles distintos, y copiar uno
        // dentro del otro es una operación legítima.
        let sensible = MemProvider::new();
        assert!(!is_descendant_folded(&to, &from, &sensible).await);

        // Volumen que PLIEGA: es el mismo árbol.
        let pliega = MemProvider::with_flags(
            CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING,
        );
        assert!(
            is_descendant_folded(&to, &from, &pliega).await,
            "un árbol se copia dentro de sí mismo"
        );

        // Y sigue siendo un prefijo ESTRICTO: el mismo path plegado no es
        // descendiente de sí mismo (de eso se ocupa `same_node`).
        assert!(!is_descendant_folded(&vp("mem:///casa/DOCS"), &from, &pliega).await);

        // Ni un hermano cuyo nombre solo comparte prefijo de BYTES.
        assert!(!is_descendant_folded(&vp("mem:///casa/docsx/y"), &from, &pliega).await);
    }

    /// #215: la heurística de identidad pregunta por la UBICACIÓN, no por el
    /// provider, y pliega con la clave compartida y no con `to_lowercase`.
    ///
    /// `capabilities()` contesta por el mount del provider, así que bajo un
    /// mismo `file://` un pincho que no distingue caja recibía la respuesta de
    /// `/home`. Lo que se decide con ella es si un `move` es un rename sobre
    /// sí mismo — o sea, un camino de copia que puede destruir el origen.
    #[tokio::test]
    async fn la_heuristica_de_identidad_pregunta_por_la_ubicacion() {
        use norte_proto::{CapabilityFlags, VPath};

        /// Distingue caja en todas partes MENOS bajo `/pincho`.
        struct PorMontajes(norte_testkit::MemProvider);

        #[async_trait::async_trait]
        impl norte_vfs::Provider for PorMontajes {
            fn scheme(&self) -> &str {
                self.0.scheme()
            }
            fn capabilities(&self) -> norte_proto::Capabilities {
                let mut c = self.0.capabilities();
                c.flags.insert(CapabilityFlags::CASE_SENSITIVE);
                c
            }
            async fn capabilities_at(&self, p: &VPath) -> Result<norte_proto::Capabilities, Error> {
                let mut c = self.capabilities();
                if p.segments().any(|s| s == b"pincho") {
                    c.flags.remove(CapabilityFlags::CASE_SENSITIVE);
                }
                Ok(c)
            }
            async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
                self.0.stat(p).await
            }
            async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
                self.0.list(p).await
            }
            async fn read(
                &self,
                p: &VPath,
                r: Option<norte_proto::ByteRange>,
            ) -> Result<norte_vfs::ByteStream, Error> {
                self.0.read(p, r).await
            }
            async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
                self.0.write(p).await
            }
            async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
                self.0.mkdir(p).await
            }
            async fn remove(&self, p: &VPath) -> Result<(), Error> {
                self.0.remove(p).await
            }
            async fn rename(&self, a: &VPath, b: &VPath) -> Result<(), Error> {
                self.0.rename(a, b).await
            }
        }

        let dst = PorMontajes(norte_testkit::MemProvider::new());
        let vp = |w: &str| VPath::parse(w).expect("wire");

        // Bajo la raíz, que distingue caja: dos nombres, no uno.
        assert!(
            !same_node_heuristic(&vp("mem:///casa/A.txt"), &vp("mem:///casa/a.txt"), &dst).await
        );
        // Bajo el montaje que NO la distingue: el mismo fichero.
        assert!(
            same_node_heuristic(&vp("mem:///pincho/A.txt"), &vp("mem:///pincho/a.txt"), &dst).await,
            "el mount decide, no el provider"
        );
        // Y pliega con la clave compartida: el signo micro y la mu griega son
        // el mismo nombre bajo pliegue, y `to_lowercase` no los mueve.
        assert!(
            same_node_heuristic(
                &vp("mem:///pincho/%C2%B5"),
                &vp("mem:///pincho/%CE%BC"),
                &dst
            )
            .await,
            "la clave de plegado, no un to_lowercase"
        );
    }

    /// Cancelación limpia de `trash_retrying` (regla 3): un token ya cancelado
    /// devuelve `Cancelled` SIN tocar el provider (la víctima inexistente ni
    /// siquiera se consulta → no hay `NotFound`), y el bucle de reintento
    /// observa el token en cada vuelta.
    #[tokio::test]
    async fn trash_retrying_honra_el_token_cancelado() {
        use norte_proto::{Error, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::trash::TrashId;
        use tokio_util::sync::CancellationToken;

        let mem = MemProvider::new().with_logical_trash();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let id = TrashId::new(0, 0);
        let r = super::trash_retrying(&mem, &VPath::parse("mem:///x").expect("wire"), &id, &cancel)
            .await;
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    }

    /// #104 review MAJOR-1: el `Conflict` ambiguo de `mkdir_retrying` se
    /// VERIFICA listando — un dir ajeno CON CONTENIDO jamás se reclama como
    /// nuestro (el `Created` falso haría que un undo lo mandara entero a la
    /// papelera). La ventana residual (dir ajeno aún VACÍO) se acepta y se
    /// pinea como decisión: recuperable de trash, indistinguible sin
    /// node-id.
    #[tokio::test]
    async fn mkdir_retrying_no_reclama_un_dir_ajeno_con_contenido() {
        use norte_proto::{Error, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::Provider as _;
        use tokio_util::sync::CancellationToken;

        let mem = MemProvider::new();
        let dir = VPath::parse("mem:///x").expect("wire");
        // Tercero: dir CON contenido, ya presente cuando llega el retry.
        mem.mkdir(&dir).await.expect("mkdir ajeno");
        {
            let mut s = mem
                .write(&VPath::parse("mem:///x/suyo.txt").expect("wire"))
                .await
                .expect("write");
            s.write(bytes::Bytes::from_static(b"ajeno"))
                .await
                .expect("chunk");
            s.commit().await.expect("commit");
        }
        // Primer intento: transitorio SIN aplicar → ambiguous.
        mem.faults().unavailable_for_next(1);
        let cancel = CancellationToken::new();
        let r = super::mkdir_retrying(&super::Dest::plain(&mem, dir.clone()), &cancel).await;
        assert!(
            matches!(r, Err(Error::Conflict { .. })),
            "un dir con contenido es de un tercero, jamás nuestro: {r:?}"
        );

        // Decisión pineada: el dir ajeno VACÍO sí pasa por nuestro (ventana
        // residual documentada en el rustdoc de `mkdir_retrying`).
        let vacio = VPath::parse("mem:///vacio").expect("wire");
        mem.mkdir(&vacio).await.expect("mkdir ajeno vacío");
        mem.faults().unavailable_for_next(1);
        let r = super::mkdir_retrying(&super::Dest::plain(&mem, vacio.clone()), &cancel).await;
        assert!(r.is_ok(), "{r:?}");
    }

    /// Cancelación limpia de `mkdir_task` (regla 3, #104): un token ya
    /// cancelado devuelve `Cancelled` ANTES de tocar el provider — ni
    /// pre-stat, ni mkdir, ni `Created` al journal (el observer registraría
    /// la mutación; un mkdir no aplicado no debe llegar jamás).
    #[tokio::test]
    async fn mkdir_task_honra_el_token_cancelado() {
        use crate::journal::Actor;
        use crate::progress::ProgressReporter;
        use crate::scheduler::TaskCtx;
        use norte_proto::{Error, TaskId, TaskKind, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::Provider as _;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        let mem = Arc::new(MemProvider::new());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (reporter, _rx) = ProgressReporter::new(TaskId::new(1), TaskKind::Mkdir);
        let ctx = TaskCtx {
            cancel,
            progress: Arc::new(reporter),
            actor: Actor::User,
        };
        let observer: Arc<dyn crate::MutationObserver> = Arc::new(crate::observer::NoopObserver);
        let r = super::mkdir_task(
            mem.clone(),
            VPath::parse("mem:///x").expect("wire"),
            observer,
            &ctx,
        )
        .await;
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        assert!(
            (*mem)
                .stat(&VPath::parse("mem:///x").expect("wire"))
                .await
                .is_err(),
            "nada creado bajo cancelación"
        );
    }

    /// Regla dura 3: toda task nueva tiene su test de cancelación limpia.
    ///
    /// `create_task` mira el token UNA vez, al entrar, y eso basta porque no
    /// tiene bucle: lo que hay después es un `stat`, un `write` y un `commit`,
    /// y partir la creación de un fichero vacío por la mitad no significa
    /// nada. Lo que este test clava es que bajo cancelación NO queda un
    /// fichero a medias en el destino.
    #[tokio::test]
    async fn create_task_honra_el_token_cancelado() {
        use crate::journal::Actor;
        use crate::progress::ProgressReporter;
        use crate::scheduler::TaskCtx;
        use norte_proto::{Error, TaskId, TaskKind, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::Provider as _;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        let mem = Arc::new(MemProvider::new());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (reporter, _rx) = ProgressReporter::new(TaskId::new(1), TaskKind::Create);
        let ctx = TaskCtx {
            cancel,
            progress: Arc::new(reporter),
            actor: Actor::User,
        };
        let observer: Arc<dyn crate::MutationObserver> = Arc::new(crate::observer::NoopObserver);
        let r = super::create_task(
            mem.clone(),
            VPath::parse("mem:///nuevo.txt").expect("wire"),
            None,
            observer,
            &ctx,
        )
        .await;
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        assert!(
            (*mem)
                .stat(&VPath::parse("mem:///nuevo.txt").expect("wire"))
                .await
                .is_err(),
            "nada creado bajo cancelación, ni siquiera vacío"
        );
    }

    /// Observer que dice que no a TODO: lo que se prueba en las dos siguientes
    /// es el camino en el que la mutación ya ocurrió y su registro no llega —
    /// exactamente el arma del #160 en `delete_task`.
    #[derive(Debug)]
    struct ObserverFalla;

    #[async_trait::async_trait]
    impl crate::MutationObserver for ObserverFalla {
        async fn on_mutation(
            &self,
            _mutation: &crate::Mutation<'_>,
            _actor: &crate::journal::Actor,
        ) -> Result<(), norte_proto::Error> {
            Err(norte_proto::Error::Io { retryable: false })
        }
    }

    /// #160, la misma forma que en `sync::exec::bury` y por el camino que anda
    /// un F8: la papelera se llevó el fichero y el observer del journal falló
    /// después. Se devuelve, y el borrado falla con el árbol como estaba.
    #[tokio::test]
    async fn un_observer_que_falla_tras_enterrar_devuelve_el_fichero() {
        use crate::journal::Actor;
        use crate::progress::ProgressReporter;
        use crate::scheduler::TaskCtx;
        use norte_proto::{DeleteMode, Error, TaskId, TaskKind, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::Provider as _;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        let mem = Arc::new(MemProvider::new().with_logical_trash());
        let path = VPath::parse("mem:///a.txt").expect("wire");
        {
            let mut sink = mem.write(&path).await.expect("write");
            sink.write(bytes::Bytes::from_static(b"vivo"))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }

        // El mismo `TaskCtx` a mano que arman los tests vecinos de este módulo
        // (no hay helper compartido; no se añade uno para un solo test más).
        let (reporter, _rx) = ProgressReporter::new(TaskId::new(1), TaskKind::Delete);
        let ctx = TaskCtx {
            cancel: CancellationToken::new(),
            progress: Arc::new(reporter),
            actor: Actor::User,
        };

        let err = super::delete_task(
            Arc::clone(&mem) as Arc<dyn norte_vfs::Provider>,
            path.clone(),
            DeleteMode::Trash,
            Arc::new(ObserverFalla),
            &ctx,
        )
        .await
        .expect_err("el observer falló");
        assert!(matches!(err, Error::Io { .. }), "{err:?}");

        assert!(
            mem.stat(&path).await.is_ok(),
            "el fichero volvió de la papelera a su ruta"
        );
    }

    /// #160, el otro brazo: una papelera "vanish" (macOS/Windows, `Opaque`) no
    /// nombra lo que se llevó — `dest` llega `None` y no hay adónde apuntar
    /// `restore_from`. La compensación no es posible; lo único que le queda al
    /// operador es la línea de log, y el fallo del observer se sigue
    /// propagando para que el borrado falle alto (no se pretende éxito).
    #[tokio::test]
    async fn un_observer_que_falla_con_papelera_opaca_no_compensa_pero_sigue_fallando_alto() {
        use crate::journal::Actor;
        use crate::progress::ProgressReporter;
        use crate::scheduler::TaskCtx;
        use norte_proto::{DeleteMode, Error, TaskId, TaskKind, VPath};
        use norte_testkit::MemProvider;
        use norte_vfs::Provider as _;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        // Sin `.with_logical_trash()`: la papelera del testkit es "vanish"
        // (como la nativa de macOS/Windows) y `trash()` devuelve `Ok(None)`.
        let mem = Arc::new(MemProvider::new());
        let path = VPath::parse("mem:///a.txt").expect("wire");
        {
            let mut sink = mem.write(&path).await.expect("write");
            sink.write(bytes::Bytes::from_static(b"vivo"))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }

        let (reporter, _rx) = ProgressReporter::new(TaskId::new(1), TaskKind::Delete);
        let ctx = TaskCtx {
            cancel: CancellationToken::new(),
            progress: Arc::new(reporter),
            actor: Actor::User,
        };

        let err = super::delete_task(
            Arc::clone(&mem) as Arc<dyn norte_vfs::Provider>,
            path.clone(),
            DeleteMode::Trash,
            Arc::new(ObserverFalla),
            &ctx,
        )
        .await
        .expect_err("el observer falló, igual que con papelera nombrada");
        assert!(matches!(err, Error::Io { .. }), "{err:?}");

        assert!(
            mem.stat(&path).await.is_err(),
            "sin `dest` no hay compensación posible: el fichero sigue fuera de \
             su sitio, y eso lo dice el log, no un `stat` que vuelva a \
             encontrarlo"
        );
    }

    /// Rama NEGATIVA de la desambiguación de symlink (encoding-auditor,
    /// fixture 3): tras un transitorio, el Conflict con un link AJENO
    /// (target distinto) sigue siendo Conflict y el link ajeno queda
    /// intacto. Y la rama positiva desambigua igual con bytes no-UTF8.
    #[tokio::test]
    async fn symlink_retrying_no_adopta_links_ajenos_y_desambigua_bytes_crudos() {
        use futures::StreamExt as _;
        use norte_proto::Error;
        use norte_testkit::MemProvider;
        use norte_vfs::{Provider, SymlinkKind};
        use tokio_util::sync::CancellationToken;

        let mem = MemProvider::new();
        let root = MemProvider::root();
        let seg = |b: &[u8]| norte_proto::Segment::new(b.to_vec()).expect("segmento válido");
        let cancel = CancellationToken::new();

        // Negativa: link preexistente con OTRO target + transitorio previo.
        let ajeno = root.join(seg(b"ajeno"));
        mem.symlink(&ajeno, b"otro", SymlinkKind::File)
            .await
            .expect("symlink previo");
        mem.faults().unavailable_for_next(1);
        let res = super::symlink_retrying(
            &super::Dest::plain(&mem, ajeno.clone()),
            b"nuestro",
            SymlinkKind::File,
            &cancel,
        )
        .await;
        assert!(
            matches!(res, Err(Error::Conflict { .. })),
            "un link ajeno jamás se adopta: {res:?}"
        );
        assert_eq!(
            mem.read_link(&ajeno).await.expect("intacto"),
            b"otro",
            "el link ajeno no se toca"
        );

        // Positiva con bytes CRUDOS no-UTF8 (regla 1: comparación por bytes).
        let crudo = root.join(seg(b"crudo"));
        mem.faults().ambiguous_mutations(1);
        super::symlink_retrying(
            &super::Dest::plain(&mem, crudo.clone()),
            b"caf\xE9",
            SymlinkKind::File,
            &cancel,
        )
        .await
        .expect("efecto aplicado + verificado por bytes = ok");
        assert_eq!(mem.read_link(&crudo).await.expect("existe"), b"caf\xE9");
        // El stream de list sigue vivo tras todo esto (sanidad).
        drop(mem.list(&root).await.expect("list ok").next().await);
    }

    #[test]
    fn rename_auto_respeta_la_extension() {
        assert_eq!(rename_auto_candidate(b"a.txt", 1), b"a (1).txt");
        assert_eq!(rename_auto_candidate(b"a.txt", 12), b"a (12).txt");
        assert_eq!(rename_auto_candidate(b"sin-ext", 1), b"sin-ext (1)");
        // Dotfile: el punto inicial NO es extensión.
        assert_eq!(rename_auto_candidate(b".bashrc", 1), b".bashrc (1)");
        // Solo la ÚLTIMA extensión (limitación documentada: tar.gz se parte).
        assert_eq!(
            rename_auto_candidate(b"archivo.tar.gz", 1),
            b"archivo.tar (1).gz"
        );
        // Byte-safe con nombres no-UTF8.
        assert_eq!(
            rename_auto_candidate(&[0xE9, b'.', b'd'], 2),
            &[0xE9, b' ', b'(', b'2', b')', b'.', b'd'][..]
        );
    }
}
