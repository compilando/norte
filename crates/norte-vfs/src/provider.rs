//! El trait [`Provider`]: contrato único de todo backend de almacenamiento.
//!
//! Reglas del contrato (las verifica `provider_contract!` en `norte-testkit`):
//! - Nombres = bytes ([`VPath`]); un provider jamás renormaliza ni "repara"
//!   nombres en silencio.
//! - Operaciones simples: sin recursión (la hace el core), sin políticas de
//!   colisión (el core decide), sin seguir symlinks.
//! - Errores mapeados a la taxonomía [`Error`] en el borde del provider.

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use norte_proto::{AttrInfo, ByteRange, Capabilities, Entry, Error, Segment, VPath};

use crate::options::ListOptions;
use crate::sink::ByteSink;

/// Stream de entradas de un listado (`fs.list`), perezoso y cancelable
/// soltándolo. Un error a mitad de stream termina el listado.
pub type EntryStream = BoxStream<'static, Result<Entry, Error>>;

/// Stream de contenido de un archivo, en chunks [`Bytes`] del tamaño que el
/// provider prefiera (el copy engine re-trocea si le hace falta).
pub type ByteStream = BoxStream<'static, Result<Bytes, Error>>;

/// Un backend de almacenamiento (local, sftp, s3, archive, memoria).
///
/// Objeto-seguro: el core trabaja con `Box<dyn Provider>` registrados por
/// scheme. Las operaciones compuestas (copy recursivo, move cross-provider,
/// delete de árboles) NO viven aquí: son del copy engine de `norte-core`.
///
/// ```
/// use async_trait::async_trait;
/// use norte_proto::{Capabilities, CapabilityFlags, Entry, Error, VPath};
/// use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};
///
/// struct NullProvider;
///
/// #[async_trait]
/// impl Provider for NullProvider {
///     fn scheme(&self) -> &str {
///         "null"
///     }
///     fn capabilities(&self) -> Capabilities {
///         Capabilities { flags: CapabilityFlags::empty(), max_path: None }
///     }
///     async fn stat(&self, _p: &VPath) -> Result<Entry, Error> {
///         Err(Error::NotFound)
///     }
///     async fn list(&self, _p: &VPath) -> Result<EntryStream, Error> {
///         Err(Error::NotFound)
///     }
///     async fn read(
///         &self,
///         _p: &VPath,
///         _range: Option<norte_proto::ByteRange>,
///     ) -> Result<ByteStream, Error> {
///         Err(Error::NotFound)
///     }
///     async fn write(&self, _p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
///         Err(Error::Unsupported)
///     }
///     async fn mkdir(&self, _p: &VPath) -> Result<(), Error> {
///         Err(Error::Unsupported)
///     }
///     async fn remove(&self, _p: &VPath) -> Result<(), Error> {
///         Err(Error::NotFound)
///     }
///     async fn rename(&self, _from: &VPath, _to: &VPath) -> Result<(), Error> {
///         Err(Error::Unsupported)
///     }
/// }
///
/// // Objeto-seguro: el core registra providers así.
/// let _boxed: Box<dyn Provider> = Box::new(NullProvider);
///
/// // Default de `list_skipped` (#93): un backend que lista todo lo que
/// // existe responde `Ok(None)` — nada que señalizar. `Some(0)` = contenedor
/// // indexado sin omisiones; `Some(n)` = n entradas invisibles del listado.
/// let p = VPath::parse("null:///").unwrap();
/// let skipped = futures::executor::block_on(NullProvider.list_skipped(&p)).unwrap();
/// assert_eq!(skipped, None);
///
/// // Defaults del bloque 2 (#108): catálogo vacío, list_with ≡ list.
/// assert!(NullProvider.attrs().is_empty());
/// ```
// OJO mantenimiento: todo método NUEVO de este trait (aunque tenga default)
// debe delegarse también en `SessionProvider` (norte-core/src/sessions.rs) —
// si no, las sesiones remotas cacheadas servirían el DEFAULT en vez del
// provider vivo, sin error de compilación. Hay un test de completitud allí.
#[async_trait]
pub trait Provider: Send + Sync {
    /// El scheme que sirve este provider (`file`, `sftp`, `mem`…).
    fn scheme(&self) -> &str;

    /// Capacidades declaradas; el core elige estrategia consultándolas.
    fn capabilities(&self) -> Capabilities;

    /// Capacidades REFINADAS para `p`: la misma declaración, corregida con lo
    /// que el backend pueda averiguar de ESA ubicación — cómo pliega la caja
    /// ese mount, si el directorio es un ext4/f2fs `+F`
    /// ([`norte_proto::CapabilityFlags::FULL_FOLD`]), si una escritura bajo él puede
    /// confinarse ([`norte_proto::CapabilityFlags::CONFINED_WRITES`]).
    ///
    /// Es `async` porque la respuesta cuesta I/O: una sonda va en
    /// `spawn_blocking` (regla dura 2), no en el runtime. El default responde
    /// [`Self::capabilities`], que es lo correcto para cualquier backend cuyas
    /// ubicaciones son todas iguales; sobreescribirlo es para el que sirve más
    /// de un filesystem tras un mismo scheme (ADR 0054).
    ///
    /// Una sonda que no sabe responder NO es error: se devuelve la
    /// declaración. [`Capabilities`] no sabe decir «no lo sé» —un flag ausente
    /// significa ausente— y eso es una decisión, no un olvido: la degradación
    /// es exactamente el comportamiento declarado de siempre.
    ///
    /// Errores: los que produzca `p` ([`Error::NotFound`] si no existe).
    async fn capabilities_at(&self, p: &VPath) -> Result<Capabilities, Error> {
        let _ = p;
        Ok(self.capabilities())
    }

    /// Abre `root` como RAÍZ CONFINADA: todo lo que se haga con el handle
    /// direcciona segmentos RELATIVOS a ella y no puede salirse, sea cual sea
    /// la forma del árbol por debajo — un componente INTERMEDIO que sea un
    /// symlink hacia fuera falla en vez de redirigir la escritura (#164).
    ///
    /// No es una comprobación antes de abrir: no hay ninguna ruta que
    /// recomponer, que es lo que lo hace libre de la ventana TOCTOU que una
    /// comprobación del lado del caller tiene por construcción.
    ///
    /// Lo que se prohíbe es SALIRSE, no «que haya symlinks»: uno que apunte a
    /// otro sitio dentro de la raíz se sigue, porque prohibirlo rompería
    /// árboles legítimos sin ganar seguridad ninguna.
    ///
    /// [`Error::Unsupported`] (default) = este backend no sabe confinar. El
    /// caller DEGRADA —no rechaza— y lo dice; ver
    /// [`norte_proto::CapabilityFlags::CONFINED_WRITES`], que lo anuncia por
    /// ubicación (ADR 0054).
    ///
    /// Errores: los de abrir `root` ([`Error::NotFound`] si no está).
    async fn open_root(&self, root: &VPath) -> Result<Box<dyn ConfinedRoot>, Error> {
        let _ = root;
        Err(Error::Unsupported)
    }

    /// Metadatos de un nodo. Symlinks: describe el LINK (kind `Symlink`),
    /// jamás el destino.
    async fn stat(&self, p: &VPath) -> Result<Entry, Error>;

    /// Listado no recursivo de un directorio, como stream perezoso.
    /// El orden es el del backend, sin garantía.
    async fn list(&self, p: &VPath) -> Result<EntryStream, Error>;

    /// Total de entradas del CONTENEDOR bajo `p` omitidas de su índice
    /// (#93): nombres que no mapean a segmentos `VPath` válidos o entradas
    /// recortadas por límites anti-bomba (providers archive, ADR 0018 C2).
    /// Es un total POR CONTENEDOR — las omitidas no tienen ruta
    /// representable donde atribuirse, así que el mismo valor aplica a
    /// cualquier dir de ese contenedor.
    ///
    /// `Ok(None)` (default) = no aplica: este backend lista todo lo que
    /// existe (filesystems, remotos). `Ok(Some(0))` = contenedor indexado
    /// sin omisiones. Los frontends solo señalizan `Some(n)` con `n > 0`.
    async fn list_skipped(&self, p: &VPath) -> Result<Option<u64>, Error> {
        let _ = p;
        Ok(None)
    }

    /// Catálogo de atributos por entrada que este provider sabe materializar
    /// (#108 bloque 2, ADR 0039). Default: ninguno. Un provider con catálogo
    /// NO vacío DEBE sobreescribir [`Self::list_with`] y [`Self::stat_with`]
    /// — la suite contractual fija el acuerdo de tipos declarado
    /// ([`norte_proto::AttrType`]) contra los valores producidos.
    ///
    /// Los ids/labels de aquí son del lado provider; el daemon los envuelve
    /// en `AttrCatalog::new` (que sanea) antes de tocar el wire, y el backend
    /// embebido debe hacer lo mismo (ADR 0039 §4).
    fn attrs(&self) -> &[AttrInfo] {
        &[]
    }

    /// [`Self::list`] con opciones. El default ignora las opciones y produce
    /// entradas peladas — correcto para cualquier provider con catálogo
    /// vacío. Ausencia significa ausencia: un id pedido desconocido o no
    /// producible se OMITE de `Entry::attrs`, jamás se fabrica.
    async fn list_with(&self, p: &VPath, opt: &ListOptions) -> Result<EntryStream, Error> {
        let _ = opt;
        self.list(p).await
    }

    /// [`Self::stat`] con opciones. Mismo contrato que [`Self::list_with`].
    async fn stat_with(&self, p: &VPath, opt: &ListOptions) -> Result<Entry, Error> {
        let _ = opt;
        self.stat(p).await
    }

    /// Contenido de un archivo como stream de chunks. `range: None` = el
    /// archivo completo; con rango, desde `offset` hasta `len` bytes (o EOF,
    /// lo que llegue antes). `offset` más allá de EOF: stream vacío, no
    /// error (semántica de `pread`). Lo exige el resume de M2 y el viewer
    /// (ADR 0005).
    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error>;

    /// Identidad REAL del nodo, si el backend la conoce: `(dev, ino)` en
    /// unix, `(volumen, FileId)` en Windows, clave interna en providers
    /// sintéticos. Es la base de los guards anti-autodestrucción del copy
    /// engine y del visited set contra ciclos de symlinks (spec §17.9).
    ///
    /// `follow` elige entre la identidad del PROPIO nodo (semántica lstat,
    /// coherente con [`Self::stat`]) o la de su destino resuelto; sobre un
    /// nodo que no es symlink ambas coinciden.
    ///
    /// `Ok(None)` (default) = este backend no tiene identidad estable
    /// (object storage, ftp): el caller degrada a heurísticas conservadoras
    /// y las features que EXIGEN identidad (seguir dir-symlinks) responden
    /// `Unsupported`.
    ///
    /// Errores: [`Error::NotFound`] si `p` no existe — o si es un symlink
    /// roto con [`FollowLinks::Yes`].
    async fn node_id(&self, p: &VPath, follow: FollowLinks) -> Result<Option<NodeId>, Error> {
        let _ = (p, follow);
        Ok(None)
    }

    /// Bytes CRUDOS del destino de un symlink (relativo o absoluto, quizá
    /// roto, quizá no-UTF8 — jamás se valida como `VPath` ni se resuelve).
    ///
    /// Errores: [`Error::NotFound`] si `p` no existe;
    /// [`Error::Conflict`] (`TypeMismatch`) si existe pero no es symlink;
    /// [`Error::Unsupported`] si el provider no sabe de symlinks (default).
    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        let _ = p;
        Err(Error::Unsupported)
    }

    /// Mueve `p` (árbol entero si es dir) a la PAPELERA del provider —
    /// recuperable (ADR 0009). Solo con la capability `TRASH`; sin ella:
    /// [`Error::Unsupported`] (default) — el engine JAMÁS degrada a
    /// borrado permanente por su cuenta.
    ///
    /// Devuelve `Some(dest)` con el destino recuperable siempre que el provider
    /// ELIJA ese destino y lo sepa nombrar — el core lo persiste como
    /// `reversal_ref` para el undo. Lo hacen la papelera LÓGICA
    /// (`.norte-trash/<id>/payload`) y la papelera freedesktop de
    /// `norte-vfs-local` (`<Trash>/files/<nombre>`). `None` si la papelera es
    /// del OS y no expone ruta estable (macOS, Windows) o es una papelera
    /// "vanish" de test.
    ///
    /// **`None` no es un detalle cosmético**: sin `reversal_ref` el undo de una
    /// sobrescritura tiene que casar por ruta ORIGINAL, y para cuando llega
    /// ahí el candidato más reciente es el fichero que él mismo acaba de
    /// enterrar. Por eso [`Provider::trash_restorable`] existe: el plan de una
    /// sincronización marca IRREVERSIBLE todo lo que pase por una papelera que
    /// no nombra su destino, antes de que nadie apruebe nada (regla dura 4).
    ///
    /// `id` lo genera el engine UNA vez por operación (#99): la papelera lógica
    /// construye su entrada determinista `.norte-trash/<id>/` con él, de modo
    /// que la operación es IDEMPOTENTE — un reintento tras un fallo transitorio
    /// converge en la misma entrada (víctima ya movida + destino presente →
    /// `Some(payload)`) en vez de crear una segunda o perder el `reversal_ref`.
    /// Las papeleras nativas/vanish lo ignoran (sin destino recuperable).
    ///
    /// Excepciones de plataforma conocidas (ADR 0009, issues #25/#26):
    /// Windows puede DESTRUIR ítems no reciclables (auto-respuesta del
    /// nuke warning); freedesktop cross-device degrada a copy+delete
    /// interno (potencialmente largo e incancelable a mitad).
    async fn trash(&self, p: &VPath, id: &crate::trash::TrashId) -> Result<Option<VPath>, Error> {
        let _ = (p, id);
        Err(Error::Unsupported)
    }

    /// ¿La papelera de este provider NOMBRA el destino de lo que entierra?
    ///
    /// Es una propiedad de la IMPLEMENTACIÓN, no de una víctima concreta, y por
    /// eso no hace I/O: `true` significa "cuando `trash` va bien, contesta
    /// `Some`". Quien planifica una mutación la usa para clasificar la reversa
    /// ANTES de pedir aprobación (regla dura 4): sobre una papelera que
    /// contesta `None` no hay undo posible, ni siquiera el de una copia, porque
    /// deshacer una creación también pasa por la papelera (#65).
    ///
    /// Un provider que conteste `true` puede aun así devolver `None` en un caso
    /// concreto —el destino existe pero cae fuera de lo que ese provider sabe
    /// nombrar—; el journal se queda entonces sin `reversal_ref` y el undo lo
    /// BLOQUEA nombrando la ruta, que es lo honesto. Lo que no es legal es lo
    /// contrario: prometer `false` y devolver `Some`, ni prometer `true` sin
    /// tener nunca destino. La suite contractual lo comprueba.
    ///
    /// Default `false`: quien no lo implemente no promete nada.
    fn trash_restorable(&self) -> bool {
        false
    }

    /// Devuelve a `original` lo que [`Provider::trash`] enterró en `dest`, con
    /// los metadatos que la papelera hubiera dejado al lado.
    ///
    /// El default es el movimiento a secas, que es lo que hace la papelera
    /// LÓGICA. Lo sobrescribe quien deje metadatos fuera del payload — la
    /// papelera freedesktop de `norte-vfs-local` tiene que llevarse también el
    /// `info/<nombre>.trashinfo`, o la papelera del usuario queda con una
    /// entrada que apunta a un fichero que ya no está.
    ///
    /// El destino tiene que estar LIBRE: hereda el contrato no-replace de
    /// [`Provider::rename`], porque restaurar pisando es perder lo que hubiera
    /// llegado a esa ruta después.
    ///
    /// # Errors
    /// Los de [`Provider::rename`]: [`Error::NotFound`] si `dest` ya no está,
    /// [`Error::Conflict`] si `original` está ocupado.
    async fn restore_from(&self, dest: &VPath, original: &VPath) -> Result<(), Error> {
        self.rename(dest, original).await
    }

    /// GC de staging `.norte-partial` huérfano (ADR 0012, #11) en el directorio
    /// `dir`: borra los parciales cuya antigüedad supera `older_than`. Los
    /// reconoce por su FORMA exacta, no por prefijo suelto — un archivo real
    /// `.norte-partial.backup` JAMÁS se toca. Devuelve cuántos borró.
    ///
    /// Default no-op (`Ok(0)`): solo los providers con staging LOCAL lo
    /// implementan. NO es una mutación de usuario → no pasa por el journal.
    ///
    /// # Errors
    /// [`Error`] si `dir` no se puede listar; los fallos de borrado
    /// individuales se cuentan como no-borrados, sin abortar el barrido.
    async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        let _ = (dir, older_than);
        Ok(0)
    }

    /// Restaura desde la papelera NATIVA del OS el ítem cuya ruta original es
    /// `original` (undo M3-2 de un `Trashed` sin `reversal_ref`, ADR 0009).
    /// Default `Unsupported`. Solo el provider local lo implementa: casa por
    /// ruta original el ítem MÁS RECIENTE y lo restaura. Falla limpio si la
    /// plataforma no lista la papelera, no hay match, o el destino está ocupado.
    ///
    /// **Es el camino de ADIVINAR, y por eso ya casi no se usa**: elegir "el más
    /// reciente con esta ruta original" es exactamente lo que restauraba el
    /// fichero equivocado al deshacer una sobrescritura. Desde que la papelera
    /// freedesktop nombra su destino, un `trashed` de `file://` en Linux lleva
    /// `reversal_ref` y el undo usa [`Provider::restore_from`]. Aquí quedan las
    /// entradas viejas del journal y las plataformas cuya papelera no nombra
    /// nada.
    ///
    /// # Errors
    /// [`Error::Unsupported`] (default y plataformas sin listado de papelera);
    /// [`Error::NotFound`] sin match; [`Error::Conflict`] destino ocupado.
    async fn restore_trashed(&self, original: &VPath) -> Result<(), Error> {
        let _ = original;
        Err(Error::Unsupported)
    }

    /// Crea un symlink en `link` apuntando a `target` (bytes crudos, tal
    /// cual — el provider no los interpreta). `kind` distingue archivo/dir
    /// donde el OS lo exige (Windows); unix lo ignora.
    ///
    /// Si `link` ya existe: [`Error::Conflict`]. Providers sin symlinks:
    /// [`Error::Unsupported`] (default) y SIN la capability `SYMLINKS`.
    async fn symlink(&self, link: &VPath, target: &[u8], kind: SymlinkKind) -> Result<(), Error> {
        let _ = (link, target, kind);
        Err(Error::Unsupported)
    }

    /// Abre un sink de escritura para un archivo NUEVO. Si el destino ya
    /// existe: [`Error::Conflict`] — la política de sobrescritura es del core,
    /// no del provider. Los bytes no son visibles en el path final hasta
    /// [`ByteSink::commit`].
    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error>;

    /// Abre un sink que REANUDA una escritura previa a `p` (ADR 0012):
    /// devuelve el sink y cuántos bytes YA hay durables en el staging
    /// (`0` = empieza de cero). El sink AÑADE después de esos bytes; el
    /// engine lee el origen desde ese offset.
    ///
    /// La reanudación cross-invocación exige un staging con nombre ESTABLE
    /// por destino (un provider que lo soporte lo reencuentra). Default:
    /// `(write(p), 0)` — sin reanudación, empieza de cero (correcto y
    /// seguro; el engine recopia entero).
    ///
    /// El destino final debe seguir sin existir: si ya existe, mismo
    /// [`Error::Conflict`] que [`Self::write`].
    async fn open_resumable(&self, p: &VPath) -> Result<(Box<dyn ByteSink>, u64), Error> {
        Ok((self.write(p).await?, 0))
    }

    /// SHA-256 de los PRIMEROS `len` bytes del staging reanudable de `p`
    /// (#35, `VerifyPolicy::Hash`): el engine lo compara con el hash del
    /// mismo prefijo del ORIGEN antes de reanudar — si no casan, el origen
    /// cambió bajo los pies y el parcial se descarta.
    ///
    /// `Ok(None)` = no hay digest disponible → el engine degrada `Hash` a
    /// `Length` (descarta solo si el parcial es más largo que el origen). Dos
    /// causas: el provider no expone digest (default), o NO hay staging para
    /// `p` ahora mismo. Un provider con staging local (local/sftp) o multipart
    /// (S3, `ETag` por parte) devuelve `Some`. `len` jamás excede lo que
    /// `open_resumable` reportó como durable; si aun así lo excediera, se
    /// hashea lo disponible y (si es menos) se devuelve `None`.
    async fn partial_digest(&self, p: &VPath, len: u64) -> Result<Option<[u8; 32]>, Error> {
        let _ = (p, len);
        Ok(None)
    }

    /// Crea UN directorio (el padre debe existir; `mkdir -p` lo compone el core).
    /// Si ya existe: [`Error::Conflict`].
    async fn mkdir(&self, p: &VPath) -> Result<(), Error>;

    /// Borra UN nodo: archivo, symlink o directorio VACÍO (el walk post-order
    /// es del core). Directorio no vacío: [`Error::Conflict`].
    ///
    /// GARANTÍA (contractual): sobre un symlink borra EL LINK, jamás su
    /// target (semántica lstat/unlink). El copy engine confía en esto para
    /// que mover un árbol con links expandidos no destruya los targets
    /// (issue #19).
    async fn remove(&self, p: &VPath) -> Result<(), Error>;

    /// Renombra dentro de ESTE provider (cross-provider = copy+delete en el
    /// core). Atómico si la capability `RENAME_ATOMIC` está declarada.
    /// Si el destino existe: [`Error::Conflict`].
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error>;

    /// Copia server-side de UN archivo si el backend la ofrece (S3 `CopyObject`,
    /// reflink/clonefile…). `None` = "no sé hacerlo, hazlo por streaming";
    /// solo se consulta si la capability `SERVER_COPY` está declarada.
    ///
    /// Si el destino ya existe: [`Error::Conflict`] — MISMA política que
    /// [`Self::write`]. Un backend cuyo copy nativo sobrescribe por defecto
    /// (S3 `CopyObject`) DEBE chequear antes; jamás sobrescritura silenciosa.
    ///
    /// **Contrato de cancelación (#51, regla 3):** el caller puede DROPEAR
    /// este future a medias (el engine lo racea contra su token). El
    /// implementador garantiza que un drop jamás deja en el destino un
    /// parcial VISIBLE sin marcar (S3 cumple: `CopyObject` es atómico y un
    /// multipart incompleto no publica objeto). Un efecto que complete
    /// server-side DESPUÉS del drop es ambigüedad aceptada (el engine la
    /// documenta, familia #32). OJO con implementaciones sobre
    /// `spawn_blocking` (reflink local futuro): el drop del future NO
    /// detiene el hilo — la copia correría hasta el final SIEMPRE y el
    /// "después del drop" pasaría de raza rara a caso determinista; esa
    /// implementación necesita su propio punto de cancelación.
    async fn copy_native(&self, from: &VPath, to: &VPath) -> Option<Result<(), Error>> {
        let _ = (from, to);
        None
    }
}

/// Tipo del symlink a crear: Windows distingue archivo/directorio en la
/// creación (`CreateSymbolicLinkW`); unix lo ignora.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymlinkKind {
    /// El destino es (o será) un archivo.
    File,
    /// El destino es (o será) un directorio.
    Dir,
    /// El caller NO lo sabe (p. ej. el copy engine preservando un link de
    /// otro provider, issue #18): el provider lo determina best-effort
    /// resolviendo el target EN SU PROPIO árbol — target roto o
    /// indeterminable degrada a `File` (documentado). En OS donde el kind
    /// no importa (unix) equivale a `File` sin coste alguno.
    Unknown,
}

/// ¿Resolver symlinks al calcular la identidad de un nodo?
/// (Parámetro de [`Provider::node_id`].)
///
/// ```
/// use norte_vfs::FollowLinks;
/// // `No` = identidad del propio link; `Yes` = la de su destino.
/// assert_ne!(FollowLinks::No, FollowLinks::Yes);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowLinks {
    /// Identidad del propio nodo (semántica lstat, como `stat`).
    No,
    /// Identidad del destino resuelto; symlink roto = `NotFound`.
    Yes,
}

/// Identidad real de un nodo DENTRO de un provider: comparable y hashable,
/// jamás interpretable ni serializable al wire (es un detalle del backend;
/// comparar `NodeId` de providers distintos no significa nada).
///
/// ```
/// use norte_vfs::NodeId;
/// let a = NodeId { volume: 1, index: 42 };
/// let b = NodeId { volume: 1, index: 42 };
/// assert_eq!(a, b);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    /// Dominio de unicidad del índice (device unix, serial de volumen
    /// Windows; 0 si el backend no distingue volúmenes).
    pub volume: u64,
    /// Índice del nodo dentro del volumen (`ino`; 128 bits cubren el
    /// `FileId` de `ReFS`).
    pub index: u128,
}

/// Operaciones bajo una raíz de las que no se puede salir (#164, ADR 0054).
///
/// Se obtiene de [`Provider::open_root`]. `rel` es SIEMPRE relativo a esa raíz
/// y jamás se compone con ella: quien resuelve es el backend, sosteniendo la
/// raíz abierta, y por eso entre dos operaciones nadie puede sustituir un
/// componente por un symlink que mande la siguiente a otro sitio.
///
/// Un `rel` que se saldría responde [`Error::Conflict`] con
/// [`norte_proto::ConflictKind::EscapesRoot`] — jamás [`Error::NotFound`], que
/// un caller contesta creando el padre, o sea haciendo exactamente lo que este
/// trait existe para impedir.
///
/// La superficie es corta a propósito: `Copy` y `CreateDir` son las dos
/// operaciones por las que el agujero era alcanzable. Las destructivas lo
/// esquivan por razones que están escritas (`DeleteTree` no desciende
/// symlinks; la revalidación es un `lstat`), y darles handle sería alcance que
/// nadie pidió.
#[async_trait]
pub trait ConfinedRoot: Send + Sync {
    /// Crea un directorio en `rel`. Mismo contrato que [`Provider::mkdir`].
    async fn mkdir(&self, rel: &[Segment]) -> Result<(), Error>;

    /// Abre un sink para `rel`. Mismo contrato que [`Provider::write`],
    /// publicación incluida: el paso de staging a definitivo va confinado
    /// también, que es donde la garantía se escaparía si no.
    async fn write(&self, rel: &[Segment]) -> Result<Box<dyn ByteSink>, Error>;

    /// Mismo contrato que [`Provider::open_resumable`]. Default: sin
    /// reanudación, que es correcto y seguro (el engine recopia entero).
    async fn open_resumable(&self, rel: &[Segment]) -> Result<(Box<dyn ByteSink>, u64), Error> {
        Ok((self.write(rel).await?, 0))
    }

    /// Mismo contrato que [`Provider::stat`]: describe el LINK, jamás su
    /// destino.
    async fn stat(&self, rel: &[Segment]) -> Result<Entry, Error>;
}
