//! El recorrido: dos raíces entran, un flujo de [`CompareRow`] sale.
//!
//! Es **profundidad primero con pila explícita**, no recursión async: sin un
//! future boxeado por nivel y sin pila reventada en un árbol hondo.
//!
//! # El techo de memoria, dicho con precisión
//!
//! Es O(un directorio), no O(un árbol) — que es lo que importa y lo que
//! [`COMPARE_MAX_DIR_ENTRIES`] acota— pero la constante NO es 1: dentro de
//! `visit` conviven los dos listados, sus dos índices (un `BTreeMap` con un
//! `Vec` por clave), los subdirectorios comunes y las filas ya producidas, que
//! además llevan `Entry` CLONADOS y sobreviven al `visit` hasta que el
//! consumidor las drena. Cuatro o cinco veces un listado, no una. La pila, en
//! cambio, sí es despreciable: crece con los hermanos de cada nivel, y todos
//! ellos existen de verdad en el árbol.
//!
//! # Por qué directorio contra directorio
//!
//! `fs.list` documenta su orden como «el del provider, sin garantía», así que
//! no hay dos flujos ordenados que fusionar. Se drena un directorio de cada
//! lado, se indexan por su clave de emparejamiento ([`crate::key`]) —que sí
//! ordena—, se fusionan las dos listas de claves, se emiten las filas y se
//! apilan los subdirectorios comunes. De ahí sale el techo de memoria, y de ahí
//! sale [`COMPARE_MAX_DIR_ENTRIES`]: un directorio por encima cuesta SU fila,
//! jamás un OOM que se lleve las otras tres horas de trabajo.
//!
//! # Lo que el listado no trajo se pregunta, y solo cuando hace falta
//!
//! `Entry::size` y `Entry::mtime_ms` son `Option` porque un listado puede no
//! traerlos, y el provider que la gente USA no los trae:
//! `norte-vfs-local::list` los deja a `None` a propósito (#52 — el `readdir` de
//! un directorio de 40 000 entradas no gasta 40 000 `stat` para pintar una
//! lista). Alimentar la cascada con eso hace que el rung de tamaño conteste
//! `Same`/`Unknown` a dos ficheros de 5 y 12 bytes, o sea que la comparación
//! local entera —la única que casi todo el mundo hace— no distinga nada.
//!
//! Así que el walk **hidrata bajo demanda**: `hydrate` gasta un `stat` en el
//! lado al que le falta el campo, y solo cuando la pareja va a LLEGAR al rung
//! que lo usa. Su rustdoc —el de la función, privada, en este mismo fichero—
//! lleva el coste, cuándo se paga y qué pasa cuando el `stat` falla. No se
//! enlaza desde aquí a propósito: este doc de módulo es público y `hydrate` no,
//! y `-D warnings` convierte ese enlace en un error del gate de docs.
//!
//! # Los errores son filas
//!
//! Un listado ilegible, un directorio desmesurado, una colisión de
//! emparejamiento o una lectura que se rompe a mitad de hash producen su fila y
//! el walk SIGUE. Lo único que termina el
//! flujo antes de tiempo es la cancelación (regla dura 3), y lo dice con un
//! [`CompareError::Cancelled`] final para que quien lo consuma no tenga que
//! adivinar si el árbol se acabó o se cortó.
//!
//! Una fila de error o de ambigüedad SOBRE UN DIRECTORIO se lleva por delante
//! todo su subárbol, que queda sin examinar y sin filas. Es la decisión
//! correcta —no se puede emparejar lo que no se ha podido listar— pero la fila
//! no lo dice, así que quien la pinte tiene que decirlo por ella.
//!
//! # Orden de las filas
//!
//! Determinista: por directorio, primero las filas ambiguas de la izquierda,
//! luego las de la derecha, y después el merge-join en orden de CLAVE. Los
//! subdirectorios comunes se apilan al revés para que la pila los saque
//! también en orden de clave. Sin ese determinismo no se puede afirmar que
//! comparar al revés da el espejo exacto, que es como se comprueba que la
//! comparación no tiene un lado favorito.
//!
//! La única asimetría conocida es el ORDEN (no los veredictos) cuando LOS DOS
//! lados fallan a la vez —una clave que colisiona en ambos, dos directorios
//! ilegibles emparejados—: las filas de la izquierda salen primero por
//! convenio, y no hay convenio simétrico posible.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeMap, VecDeque};

use futures::StreamExt;
use futures::stream::{self, BoxStream};
use norte_proto::{Entry, EntryKind, VPath};
use norte_vfs::Provider;
use tokio_util::sync::CancellationToken;

use crate::cascade::{Decision, HashOutcome, Prefetched, decide};
use crate::hash::{HashFailure, sha256_of};
use crate::key::{PairName, SideIndex, Sides, index_side, key_for};
use crate::{
    COMPARE_MAX_DIR_ENTRIES, CompareConfidence, CompareCriterion, CompareError, CompareOptions,
    CompareReason, CompareRow, CompareVerdict, Side,
};

/// El flujo que produce [`compare`].
///
/// Es un `BoxStream` y no un `impl Stream` por una razón aburrida y buena: el
/// tipo es CONCRETO, así que `norte-core` puede guardarlo en un struct de Task
/// sin arrastrar parámetros de tipo, y las dos raíces se pueden pasar por
/// referencia sin que su préstamo quede capturado en el tipo de retorno. Una
/// asignación por comparación entera.
pub type CompareStream<'a> = BoxStream<'a, Result<CompareRow, CompareError>>;

/// Compara dos árboles y emite una fila por pareja.
///
/// `left`/`right` son los dos providers y `left_root`/`right_root` las dos
/// raíces; no tienen por qué ser del mismo provider ni del mismo scheme.
/// `cancel` es el token de la Task (regla dura 3): en cuanto se dispara, el
/// flujo suelta lo que tuviera pendiente, emite un
/// [`CompareError::Cancelled`] y termina.
///
/// No muta nada y no lee contenido salvo que `opts.criteria.hash` lo pida.
///
/// Es SIMÉTRICA: comparar al revés da las mismas filas con los lados y los
/// veredictos cambiados de sitio, y nada más.
#[must_use]
pub fn compare<'a>(
    left: &'a dyn Provider,
    left_root: &VPath,
    right: &'a dyn Provider,
    right_root: &VPath,
    opts: CompareOptions,
    cancel: CancellationToken,
) -> CompareStream<'a> {
    let walk = Walk {
        left,
        right,
        opts,
        // Se rellena tras el PRIMER listado, no aquí: ver `Walk::sides`.
        sides: None,
        cancel,
        stack: vec![Frame {
            left: synthetic_dir(left_root),
            right: synthetic_dir(right_root),
            depth: 0,
        }],
        pending: VecDeque::new(),
        next_id: 0,
        finished: false,
    };
    stream::unfold(walk, |mut walk| async move {
        let item = walk.step().await?;
        Some((item, walk))
    })
    .boxed()
}

/// La `Entry` de una raíz, que nadie listó: el walk la necesita para poder
/// nombrar el directorio en una fila de error, y una raíz no tiene padre que
/// la haya descrito.
fn synthetic_dir(path: &VPath) -> Entry {
    Entry {
        path: path.clone(),
        kind: EntryKind::Dir,
        size: None,
        mtime_ms: None,
        attrs: BTreeMap::new(),
    }
}

/// Qué salió de mirar UNA pareja emparejada.
///
/// Son tres cosas distintas y no dos: una decisión que se publica, una lectura
/// rota que es SU fila y no termina nada, y una cancelación que no publica fila
/// ninguna y termina el flujo.
enum PairOutcome {
    /// La cascada decidió, con o sin el rung caro.
    Decided(Decision),
    /// El rung caro no pudo leer un lado. Trae el lado que falló, que es la
    /// mitad útil de la fila de error.
    ReadFailed(Side),
    /// El token se disparó mientras se hasheaba.
    Cancelled,
}

/// Lo mismo que [`HashFailure`], ya sabiendo de qué lado vino.
enum PairFailure {
    Read(Side),
    Cancelled,
}

impl PairFailure {
    fn of(failure: HashFailure, side: Side) -> Self {
        match failure {
            HashFailure::Read => Self::Read(side),
            // La cancelación no tiene lado: no es de un fichero, es de la Task.
            HashFailure::Cancelled => Self::Cancelled,
        }
    }
}

/// Por qué no se pudo completar la [`hydrate`] de una pareja.
#[derive(Debug)]
enum HydrationFailure {
    /// El `stat` falló. Trae el lado que falló y el rung que lo pidió, que son
    /// las dos mitades útiles de la fila de error.
    Stat {
        side: Side,
        /// `Size` o `Mtime`: qué rung se quedó sin su dato.
        rung: CompareCriterion,
    },
    /// El token se disparó antes de un `stat`.
    Cancelled,
}

/// Cómo salió de hidratar UNA pareja.
///
/// El caso de fallo lleva las DOS entradas igual que el bueno: si la izquierda
/// contestó y la derecha no, lo que la izquierda dijo es cierto y la fila de
/// error debe llevarlo — el panel pinta esa celda, y vaciarla sería tirar una
/// respuesta que sí se tuvo.
enum Hydrated<'e> {
    /// Los campos que la cascada va a mirar, ya rellenos.
    Ready(Cow<'e, Entry>, Cow<'e, Entry>),
    /// Un `stat` falló: las dos entradas tal y como quedaron, y de quién y de
    /// qué rung fue el fallo.
    Failed {
        left: Cow<'e, Entry>,
        right: Cow<'e, Entry>,
        side: Side,
        rung: CompareCriterion,
    },
    /// El token se disparó antes de un `stat`.
    Cancelled,
}

/// Un lado de la pareja mientras se le pregunta lo que el listado no trajo.
///
/// El `Cow` es lo que hace que la hidratación no cueste nada cuando no hace
/// falta: un provider que ya rellenó los campos —SFTP, object, archive,
/// `MemProvider`— sale por [`Cow::Borrowed`] sin haber clonado ni preguntado.
struct Fresh<'e> {
    entry: Cow<'e, Entry>,
    /// Ya se le gastó SU `stat`. Lo que siga faltando después de eso falta de
    /// verdad, y preguntarlo otra vez es un segundo viaje para oír lo mismo.
    asked: bool,
}

impl<'e> Fresh<'e> {
    const fn of(entry: &'e Entry) -> Self {
        Self {
            entry: Cow::Borrowed(entry),
            asked: false,
        }
    }
}

/// Gasta un `stat` en `fresh` para que el rung `rung` tenga su dato.
///
/// # Cuándo se paga
///
/// Solo cuando las TRES cosas se dan a la vez: la pareja es de ficheros (una
/// ausencia la decide la presencia, un tipo distinto el kind, dos directorios
/// el kind también, y un enlace su destino — ninguno mira tamaño ni fecha), el
/// rung que necesita el campo va a correr de verdad, y ese lado no lo trae ya.
/// Un provider que rellena su listado no recibe ni una llamada de más; el
/// segundo rung reutiliza el `stat` del primero (`asked`), así que el techo es
/// **un `stat` por lado y por pareja de ficheros**.
///
/// El precio de esa disciplina se ve en el panel: sobre un provider perezoso,
/// una pareja de ficheros enseña su tamaño y un HUÉRFANO no, porque a él no lo
/// mira ningún rung (<https://github.com/compilando/norte/issues/157>).
///
/// # Lo que cuesta, dicho sin adornos
///
/// Un `stat` es un viaje de ida y vuelta al provider, y **van en SERIE**: uno
/// detrás de otro, un lado detrás del otro, una pareja detrás de la anterior,
/// con profundidad de cola uno. Un árbol de N ficheros emparejados cuesta hasta
/// 2N viajes encadenados. Es la misma forma que el motor de copia
/// (`norte-core::ops::hydrate_plan`, que statea las hojas de su plan por el
/// mismo #52), y para un disco local es scheduling, no latencia.
///
/// Dónde SÍ duele, que no es donde parece:
///
/// - `file://` **no significa disco local**. `LocalProvider` sirve lo que el OS
///   tenga montado, y sobre SMB, NFS o sshfs cada `lstat` es un viaje por red.
///   Comparar dos shares montados es lo más normal del mundo en un gestor de
///   ficheros, y ahí 2N viajes en serie se notan.
/// - Los providers remotos rellenan su listado **casi siempre, no siempre**:
///   `norte-vfs-sftp` saca `size` de los atributos del `readdir` (siempre
///   presente), pero `mtime_ms` solo si el servidor manda `ACMODTIME` —OpenSSH
///   lo manda; un servidor mínimo o un aparato pueden no hacerlo—, y
///   `norte-vfs-object` saca `mtime_ms` de `last_modified`, que también es
///   opcional. Contra uno de esos, un árbol espejado —tamaños iguales, o sea
///   todas las parejas llegando al rung de fecha— paga los 2N viajes.
///
/// La salida no es que el motor adivine: es hidratar las parejas de UN
/// directorio con concurrencia acotada (los dos listados ya están enteros en
/// memoria cuando se emparejan) o que el provider rellene su listado — la
/// medida está en <https://github.com/compilando/norte/issues/156>, y hasta que
/// se tome, esto es lo que cuesta.
///
/// # Un `stat` que falla NO es un `Unknown`
///
/// Es una fila [`CompareVerdict::Error`] con [`CompareReason::Unreadable`] y el
/// lado que falló, igual que un listado ilegible o una lectura rota a mitad de
/// hash. `Unknown` significa «el provider no puede contestar esta pregunta» —el
/// enlace de un tar sin destino, un kind que no es fichero ni directorio—, y
/// esa respuesta viaja junto a un veredicto `Same`. Degradar aquí a `Unknown`
/// diría «iguales, no sé» de una pareja que nadie llegó a mirar, y taparía un
/// `EACCES` que el usuario puede arreglar. La comparación no inventa
/// respuestas, y «no pude preguntar» no es «pregunté y no se sabe».
///
/// **`NotFound` va por el mismo camino, y es una decisión**: un fichero que
/// desaparece entre el `list` y el `stat` es una carrera real (`/tmp`, un
/// directorio de build). El listado de un provider la resuelve al revés —
/// `norte-vfs-local::list_with` omite la entrada que se esfumó, para no matar
/// el listado de un directorio vivo—, y aquí no se puede: la pareja ya está
/// emparejada, y callarla sería quitar del panel una fila que el otro lado sí
/// tiene. La fila de error dice «esto no se pudo comparar», que es lo que pasó.
/// El motivo del wire no distingue las causas (igual que `list_all` manda todo
/// fallo de listado a `Unreadable`): el vocabulario tiene UNA palabra para «no
/// se pudo leer», y afinarla es cambiar el wire.
///
/// # Lo que se copia del `stat`, y lo que no
///
/// SOLO `size` y `mtime_ms`. El `path` y el `kind` se quedan los del listado:
/// el path ya pasó la frontera de [`is_direct_child`] y el kind ya decidió su
/// rung, así que una entrada sustituida entre el `list` y el `stat` no puede
/// colar aquí ni otro tipo ni otra ruta.
///
/// El residuo, que hay que saber: una `Entry` hidratada es un COMPUESTO de dos
/// observaciones en dos instantes —`path`, `kind` y `attrs` del listado;
/// `size` y `mtime_ms` del `stat`—. Si alguien sustituyó el fichero en medio,
/// la fila describe dos objetos a la vez. Es inevitable en cualquier diseño
/// perezoso y no lo arregla mirar más veces, pero la spec 2 va a LEER estas
/// filas para decidir qué copiar, así que queda dicho aquí.
async fn hydrate(
    provider: &dyn Provider,
    fresh: &mut Fresh<'_>,
    side: Side,
    rung: CompareCriterion,
    cancel: &CancellationToken,
) -> Result<(), HydrationFailure> {
    if fresh.asked {
        return Ok(());
    }
    // Regla dura 3: esto es I/O, y un directorio de 40 000 ficheros son 40 000
    // viajes que la cancelación no tiene por qué esperar.
    if cancel.is_cancelled() {
        return Err(HydrationFailure::Cancelled);
    }
    fresh.asked = true;
    let statted = provider
        .stat(&fresh.entry.path)
        .await
        .map_err(|_| HydrationFailure::Stat { side, rung })?;
    let entry = fresh.entry.to_mut();
    entry.size = statted.size;
    entry.mtime_ms = statted.mtime_ms;
    Ok(())
}

/// Un par de directorios pendiente de emparejar, con su profundidad.
struct Frame {
    left: Entry,
    right: Entry,
    depth: u32,
}

/// El estado del recorrido entre dos llamadas al flujo.
struct Walk<'a> {
    left: &'a dyn Provider,
    right: &'a dyn Provider,
    opts: CompareOptions,
    /// Cómo empareja la PAREJA de lados. `None` hasta el primer listado: ver
    /// [`Walk::sides`].
    sides: Option<Sides>,
    cancel: CancellationToken,
    /// Pila explícita: profundidad primero sin recursión async.
    stack: Vec<Frame>,
    /// Las filas del último directorio emparejado, aún sin entregar.
    pending: VecDeque<CompareRow>,
    /// Contador monótono de [`CompareRow::id`].
    next_id: u64,
    finished: bool,
}

impl Walk<'_> {
    /// Un paso del flujo: entrega la siguiente fila, emparejando directorios
    /// mientras no tenga ninguna a mano.
    async fn step(&mut self) -> Option<Result<CompareRow, CompareError>> {
        loop {
            // Cancelación ANTES de entregar nada (regla dura 3): ninguna fila
            // sale después del corte, ni siquiera una ya calculada.
            if self.cancel.is_cancelled() {
                if self.finished {
                    return None;
                }
                self.finished = true;
                self.pending.clear();
                self.stack.clear();
                return Some(Err(CompareError::Cancelled));
            }
            if let Some(row) = self.pending.pop_front() {
                return Some(Ok(row));
            }
            if self.finished {
                return None;
            }
            let Some(frame) = self.stack.pop() else {
                self.finished = true;
                return None;
            };
            self.visit(frame).await;
        }
    }

    /// Empareja UN par de directorios: llena [`Walk::pending`] con sus filas y
    /// apila los subdirectorios comunes.
    async fn visit(&mut self, frame: Frame) {
        // Los DOS lados se listan siempre, aunque el primero ya haya fallado:
        // dos directorios rotos son dos hechos, y volverse en el primero
        // dejaría el segundo sin descubrir para siempre.
        let listed = (
            list_all(self.left, &frame.left.path, &self.cancel).await,
            list_all(self.right, &frame.right.path, &self.cancel).await,
        );
        if matches!(listed.0, Err(ListFailure::Cancelled))
            || matches!(listed.1, Err(ListFailure::Cancelled))
        {
            return;
        }
        if let Err(ListFailure::Reason(reason)) = listed.0 {
            self.push_error(Some(frame.left.clone()), None, reason, Side::Left);
        }
        if let Err(ListFailure::Reason(reason)) = listed.1 {
            self.push_error(None, Some(frame.right.clone()), reason, Side::Right);
        }
        // Un listado que falló no se sabe qué contenía, así que la otra parte
        // tampoco se puede emparejar: decir `OnlyRight` de sus entradas sería
        // afirmar una ausencia que nadie ha comprobado.
        let (Ok(lefts), Ok(rights)) = listed else {
            return;
        };

        let sides = self.sides();
        let left_index = index_side(&lefts, sides);
        let right_index = index_side(&rights, sides);
        let left_collided = collided_keys(&left_index, sides);
        let right_collided = collided_keys(&right_index, sides);

        // Las colisiones: UNA fila por entrada implicada, jamás una fusión y
        // jamás una deduplicada (contrato normativo de `CompareVerdict::Ambiguous`).
        for (entry, reason) in left_index.collisions() {
            self.push_ambiguous(Some(entry.clone()), None, reason, Side::Left);
        }
        for (entry, reason) in right_index.collisions() {
            self.push_ambiguous(None, Some(entry.clone()), reason, Side::Right);
        }

        let mut descend: Vec<(Entry, Entry)> = Vec::new();
        let mut lefts_iter = left_index.unique().peekable();
        let mut rights_iter = right_index.unique().peekable();
        loop {
            // Por PAREJA, y no solo por directorio: dos listados al tope caben
            // 400 000 parejas, y con el rung caro apagado no hay un solo
            // `await` en todo el merge-join, así que sin esto la cancelación
            // esperaría a que terminase el directorio entero (regla dura 3).
            //
            // Ningún test puede verlo desde fuera —`step` ya tira `pending`
            // entero al cancelar, así que la SALIDA es la misma con o sin este
            // chequeo—: lo que cambia es cuánto tarda en llegar, y 400 000
            // parejas de trabajo tirado. No es un invariante sin test, es un
            // invariante de latencia.
            if self.cancel.is_cancelled() {
                return;
            }
            let order = match (lefts_iter.peek(), rights_iter.peek()) {
                (None, None) => break,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some((lk, _)), Some((rk, _))) => lk.cmp(rk),
            };
            match order {
                Ordering::Less => {
                    // `expect`: `peek` acaba de devolver `Some` sobre este
                    // mismo iterador y nadie lo ha tocado en medio, así que
                    // `next` no puede ser `None` (regla dura 6).
                    let (key, entry) = lefts_iter.next().expect("peek dijo que había");
                    self.only_on_one_side(
                        Some(entry.clone()),
                        None,
                        right_collided.get(key.as_bytes()).copied(),
                        Side::Right,
                    );
                }
                Ordering::Greater => {
                    let (key, entry) = rights_iter.next().expect("peek dijo que había");
                    self.only_on_one_side(
                        None,
                        Some(entry.clone()),
                        left_collided.get(key.as_bytes()).copied(),
                        Side::Left,
                    );
                }
                Ordering::Equal => {
                    let (_, left_entry) = lefts_iter.next().expect("peek dijo que había");
                    let (_, right_entry) = rights_iter.next().expect("peek dijo que había");
                    // Cancelado a mitad del rung caro: no hay fila que publicar
                    // —sería un veredicto provisional— y el paso del flujo se
                    // encarga de terminar. Lo que quedaba apilado se tira: nada
                    // se ha escrito.
                    let Some(is_dir_pair) = self.pair_row(left_entry, right_entry).await else {
                        return;
                    };
                    if is_dir_pair && self.descends_below(frame.depth) {
                        descend.push((left_entry.clone(), right_entry.clone()));
                    }
                }
            }
        }
        drop(lefts_iter);
        drop(rights_iter);

        // Al revés: la pila es LIFO, así que apilar en orden inverso de clave
        // es lo que hace que se saquen en orden de clave.
        let depth = frame.depth.saturating_add(1);
        for (left, right) in descend.into_iter().rev() {
            self.stack.push(Frame { left, right, depth });
        }
    }

    /// Una entrada que solo aparece en un lado. Si la clave que le tocaba en el
    /// OTRO lado está colisionada, la fila NO es `OnlyLeft`/`OnlyRight`: es
    /// `Ambiguous`.
    ///
    /// El motivo es lo que un plan de sincronización haría con cada una.
    /// `OnlyRight` le dice «cópialo al otro lado», y copiar dentro de un
    /// directorio que ya no sabe distinguir esos dos nombres crea un TERCER
    /// fichero que colisiona. `Ambiguous` hace que ese plan se niegue a actuar,
    /// que es la única respuesta segura mientras nadie deshaga la colisión.
    fn only_on_one_side(
        &mut self,
        left: Option<Entry>,
        right: Option<Entry>,
        collided_with: Option<CompareReason>,
        collision_side: Side,
    ) {
        if let Some(reason) = collided_with {
            self.push_ambiguous(left, right, reason, collision_side);
            return;
        }
        let decision = if left.is_some() {
            Decision::only_left()
        } else {
            Decision::only_right()
        };
        let id = self.next_id();
        self.pending.push_back(decision.into_row(id, left, right));
    }

    /// Apunta la fila de UNA pareja emparejada y dice si hay que bajar por
    /// ella.
    ///
    /// `None` significa cancelado: ni fila ni descenso, y quien llama suelta
    /// el directorio entero.
    async fn pair_row(&mut self, left: &Entry, right: &Entry) -> Option<bool> {
        // Lo que el listado no trajo y la cascada va a necesitar, preguntado
        // ANTES de decidir. Las filas llevan las entradas hidratadas: una fila
        // que dice «distinto por tamaño» sobre dos tamaños vacíos no se puede
        // leer.
        let (left, right) = match self.hydrated_pair(left, right).await {
            Hydrated::Ready(left, right) => (left, right),
            Hydrated::Cancelled => return None,
            Hydrated::Failed {
                left,
                right,
                side,
                rung,
            } => {
                let id = self.next_id();
                self.pending.push_back(flagged(
                    id,
                    // Lo que se llegó a saber viaja: si la izquierda contestó y
                    // la derecha no, su tamaño es cierto y la celda del panel lo
                    // enseña.
                    Some(left.into_owned()),
                    Some(right.into_owned()),
                    CompareVerdict::Error,
                    // El rung que se quedó sin su dato, igual que una lectura
                    // rota dice `Hash`: ese rung corrió y se murió.
                    rung,
                    CompareReason::Unreadable,
                    side,
                ));
                // Solo las parejas de ficheros se hidratan, así que no hay
                // descenso que plantearse.
                return Some(false);
            }
        };
        let (left, right) = (left.as_ref(), right.as_ref());
        match self.verdict_for_pair(left, right).await {
            PairOutcome::Cancelled => None,
            PairOutcome::Decided(decision) => {
                let id = self.next_id();
                self.pending.push_back(decision.into_row(
                    id,
                    Some(left.clone()),
                    Some(right.clone()),
                ));
                Some(left.kind == EntryKind::Dir && right.kind == EntryKind::Dir)
            }
            // Una lectura rota cuesta SU fila y el walk sigue, igual que un
            // listado ilegible. La fila lleva los dos lados: la pareja sí se
            // emparejó, lo que falló fue verificarla. Y no hay descenso que
            // plantearse: solo los ficheros llegan al rung de hash.
            PairOutcome::ReadFailed(side) => {
                let id = self.next_id();
                self.pending.push_back(flagged(
                    id,
                    Some(left.clone()),
                    Some(right.clone()),
                    CompareVerdict::Error,
                    // `Hash` y no `Presence`: el rung CORRIÓ y se murió. La
                    // convención de `Presence` es para las filas donde no
                    // corrió ninguno.
                    CompareCriterion::Hash,
                    CompareReason::ReadFailed,
                    side,
                ));
                Some(false)
            }
        }
    }

    /// La pareja con los campos que la cascada va a mirar ya rellenos.
    ///
    /// Sigue el MISMO orden que `cascade::size_and_mtime`, y por la misma
    /// razón por la que el rung de hash solo alcanza a lo que los baratos
    /// dieron por igual: lo que ya está decidido no se paga. Dos ficheros con
    /// tamaños distintos no gastan el `stat` del rung de fecha, y un rung
    /// apagado no gasta nada.
    ///
    /// Que devuelva [`Cow`] es el camino barato: sin un campo que falte no se
    /// clona ni una `Entry`.
    async fn hydrated_pair<'e>(&self, left: &'e Entry, right: &'e Entry) -> Hydrated<'e> {
        let mut l = Fresh::of(left);
        let mut r = Fresh::of(right);
        match self.hydrate_rungs(&mut l, &mut r).await {
            Ok(()) => Hydrated::Ready(l.entry, r.entry),
            Err(HydrationFailure::Cancelled) => Hydrated::Cancelled,
            // Las dos entradas viajan igualmente: la que sí contestó lleva su
            // dato, y la fila de error lo enseña.
            Err(HydrationFailure::Stat { side, rung }) => Hydrated::Failed {
                left: l.entry,
                right: r.entry,
                side,
                rung,
            },
        }
    }

    /// Los dos rungs, en orden, sobre los dos lados. Separada de
    /// [`Walk::hydrated_pair`] solo para poder usar `?` sin perder lo ya
    /// hidratado cuando algo falla.
    async fn hydrate_rungs(
        &self,
        l: &mut Fresh<'_>,
        r: &mut Fresh<'_>,
    ) -> Result<(), HydrationFailure> {
        // Solo parejas de FICHEROS. Un huérfano lo decide la presencia y aquí
        // ni llega; un kind distinto lo decide el kind; dos directorios también
        // (C3: la fecha de un directorio se mueve con cualquier hijo, así que
        // no se comparan ni por fecha ni por tamaño); y un enlace, su destino.
        // Statear cualquiera de ellos es un viaje al provider a cambio de nada.
        if l.entry.kind == EntryKind::File && r.entry.kind == EntryKind::File {
            if self.opts.criteria.size {
                if l.entry.size.is_none() {
                    hydrate(
                        self.left,
                        l,
                        Side::Left,
                        CompareCriterion::Size,
                        &self.cancel,
                    )
                    .await?;
                }
                if r.entry.size.is_none() {
                    hydrate(
                        self.right,
                        r,
                        Side::Right,
                        CompareCriterion::Size,
                        &self.cancel,
                    )
                    .await?;
                }
                // El rung de tamaño decide —distintos, o alguno todavía
                // desconocido tras preguntar— y la cascada no baja al de fecha:
                // hidratarla sería pagar por un rung que no va a correr.
                match (l.entry.size, r.entry.size) {
                    (Some(a), Some(b)) if a == b => {}
                    _ => return Ok(()),
                }
            }
            if self.opts.criteria.mtime {
                if l.entry.mtime_ms.is_none() {
                    hydrate(
                        self.left,
                        l,
                        Side::Left,
                        CompareCriterion::Mtime,
                        &self.cancel,
                    )
                    .await?;
                }
                if r.entry.mtime_ms.is_none() {
                    hydrate(
                        self.right,
                        r,
                        Side::Right,
                        CompareCriterion::Mtime,
                        &self.cancel,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// La decisión de UNA pareja emparejada, con lo que exige I/O ya averiguado.
    async fn verdict_for_pair(&self, left: &Entry, right: &Entry) -> PairOutcome {
        // `read_link` SOLO cuando los dos lados son enlaces: si uno no lo es,
        // el rung de kind ya decidió y leer el destino del otro es una llamada
        // al provider a cambio de nada.
        let (left_target, right_target) =
            if left.kind == EntryKind::Symlink && right.kind == EntryKind::Symlink {
                (
                    self.left.read_link(&left.path).await.ok(),
                    self.right.read_link(&right.path).await.ok(),
                )
            } else {
                (None, None)
            };
        let facts = Prefetched::links(left_target.as_deref(), right_target.as_deref());
        let decision = decide(left, right, &self.opts, &facts);

        // `needs_hash == true` significa exactamente esto: los rungs baratos
        // dieron la pareja por IGUAL y el llamante pidió hash, así que la
        // decisión NO es final. Publicarla aquí sería un veredicto
        // provisional, y en esta spec ninguna fila se corrige después.
        if !decision.needs_hash {
            return PairOutcome::Decided(decision);
        }
        let outcome = match self.hash_pair(left, right).await {
            Ok(outcome) => outcome,
            Err(PairFailure::Cancelled) => return PairOutcome::Cancelled,
            Err(PairFailure::Read(side)) => return PairOutcome::ReadFailed(side),
        };
        let decided = decide(left, right, &self.opts, &facts.with_hash(outcome));
        debug_assert!(
            !decided.needs_hash,
            "el rung de hash contestó y la cascada lo volvió a pedir"
        );
        PairOutcome::Decided(decided)
    }

    /// El rung caro sobre UNA pareja: los dos sha256, y qué dicen.
    ///
    /// Los lados van en orden y no en paralelo. Leer los dos a la vez dobla el
    /// ancho de banda y la memoria viva para adelantar como mucho la mitad del
    /// tiempo, y sobre todo hace que un fallo del primero llegue con el segundo
    /// fichero ya medio leído. Si la izquierda no se puede leer, la derecha no
    /// se abre: la fila ya es de error y leerla entera no cambiaría ni una
    /// letra de ella.
    async fn hash_pair(&self, left: &Entry, right: &Entry) -> Result<HashOutcome, PairFailure> {
        let left_digest = sha256_of(self.left, &left.path, &self.cancel)
            .await
            .map_err(|failure| PairFailure::of(failure, Side::Left))?;
        let right_digest = sha256_of(self.right, &right.path, &self.cancel)
            .await
            .map_err(|failure| PairFailure::of(failure, Side::Right))?;
        Ok(if left_digest == right_digest {
            HashOutcome::Equal
        } else {
            HashOutcome::Differ
        })
    }

    /// Cómo empareja la pareja de lados, preguntado DESPUÉS del primer listado
    /// y recordado.
    ///
    /// No se puede preguntar en [`compare`], que es síncrona: las
    /// `Capabilities` de un provider local son EXACTAS solo tras su primera
    /// operación async —el sondeo corre ahí—, y antes son el default del OS,
    /// o sea un `cfg!(target_os)`. Emparejar con eso significa no plegar caja
    /// contra un exFAT montado en Linux (y perder sus colisiones) o plegarla
    /// contra un APFS sensible a caja (y declarar ambiguo lo que no lo es).
    /// Aquí ya ha corrido un `list` en los dos lados, así que lo que se lee es
    /// lo sondeado.
    ///
    /// Sigue siendo UNA respuesta para toda la comparación, porque
    /// `Provider::capabilities` no toma path: dos mounts distintos servidos por
    /// un mismo provider comparten veredicto
    /// (<https://github.com/compilando/norte/issues/153>).
    fn sides(&mut self) -> Sides {
        *self.sides.get_or_insert_with(|| {
            Sides::from_capabilities(self.left.capabilities(), self.right.capabilities())
        })
    }

    /// ¿Se puede bajar un nivel más desde `depth`?
    fn descends_below(&self, depth: u32) -> bool {
        self.opts
            .max_depth
            .is_none_or(|max| depth.saturating_add(1) <= max)
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn push_ambiguous(
        &mut self,
        left: Option<Entry>,
        right: Option<Entry>,
        reason: CompareReason,
        side: Side,
    ) {
        let id = self.next_id();
        self.pending.push_back(flagged(
            id,
            left,
            right,
            CompareVerdict::Ambiguous,
            CompareCriterion::Presence,
            reason,
            side,
        ));
    }

    fn push_error(
        &mut self,
        left: Option<Entry>,
        right: Option<Entry>,
        reason: CompareReason,
        side: Side,
    ) {
        let id = self.next_id();
        self.pending.push_back(flagged(
            id,
            left,
            right,
            CompareVerdict::Error,
            CompareCriterion::Presence,
            reason,
            side,
        ));
    }
}

/// Las dos filas que la cascada no produce: `Ambiguous` y `Error`. Las dos
/// llevan motivo y lado obligatorios.
///
/// El `criterion` lo pone el llamante porque hay dos casos y no uno: la fila de
/// una lectura rota a mitad de hash dice `Hash` —ese rung CORRIÓ y se murió—, y
/// las demás dicen `Presence`, que es la convención del wire para «aquí no
/// informa ningún criterio». La confianza es `Unknown` en las dos: nada quedó
/// comparado.
fn flagged(
    id: u64,
    left: Option<Entry>,
    right: Option<Entry>,
    verdict: CompareVerdict,
    criterion: CompareCriterion,
    reason: CompareReason,
    side: Side,
) -> CompareRow {
    let row = CompareRow {
        id,
        left,
        right,
        verdict,
        criterion,
        confidence: CompareConfidence::Unknown,
        newer: None,
        reason: Some(reason),
        side: Some(side),
    };
    debug_assert!(row.reason_is_consistent(), "fila {verdict:?} sin motivo");
    debug_assert!(
        row.sides_are_consistent(),
        "fila {verdict:?} con lados rotos"
    );
    row
}

/// Por qué no se pudo emparejar un directorio.
enum ListFailure {
    /// El motivo que viaja en la fila.
    Reason(CompareReason),
    /// Cancelado a mitad del drenaje: no hay fila, hay final de flujo.
    Cancelled,
}

/// Drena el listado de un directorio ENTERO, con techo.
///
/// El techo se comprueba ANTES de meter la entrada, así que un directorio de
/// exactamente [`COMPARE_MAX_DIR_ENTRIES`] entradas se empareja y uno de una
/// más se rechaza sin haber materializado la de más: el límite es también el
/// techo de memoria, no solo el de la respuesta.
///
/// El token se mira dentro del bucle: un directorio de cientos de miles de
/// entradas no puede hacer esperar a la cancelación hasta que termine de
/// drenarse.
async fn list_all(
    provider: &dyn Provider,
    dir: &VPath,
    cancel: &CancellationToken,
) -> Result<Vec<Entry>, ListFailure> {
    let mut stream = provider
        .list(dir)
        .await
        .map_err(|_| ListFailure::Reason(CompareReason::Unreadable))?;
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        if cancel.is_cancelled() {
            return Err(ListFailure::Cancelled);
        }
        // Un error a mitad de listado deja el directorio a MEDIAS, y medio
        // listado emparejado produciría `OnlyLeft` de entradas que sí estaban:
        // vale como ilegible entero.
        let entry = item.map_err(|_| ListFailure::Reason(CompareReason::Unreadable))?;
        // FRONTERA DURA (defensa en profundidad, security T4 — el mismo
        // criterio que `norte-core::search::run_walk`): NO se confía en que
        // `list` devuelva solo hijos DIRECTOS de `dir`. Un provider con un bug
        // —o de un plugin de terceros— que liste un path de fuera haría que la
        // comparación lo emparejara, lo nombrara en una fila y, con el rung de
        // hash, LEYERA su contenido; y el gate del daemon solo comprueba las
        // dos RAÍCES, así que un path colado se saltaría el scope entero.
        //
        // El listado entero vale como ilegible, no se salta la entrada: la
        // misma razón que el error a mitad de listado de arriba — un listado
        // al que le falta una entrada produce `OnlyLeft` del lado contrario,
        // o sea una respuesta EQUIVOCADA en vez de una que se declara.
        //
        // Sin traza: este crate no depende de `tracing` (es una función pura
        // de dos providers) y no va a hacerlo por un aviso. La señal es la
        // fila de error, que sí llega al usuario.
        if !is_direct_child(dir, &entry.path) {
            return Err(ListFailure::Reason(CompareReason::Unreadable));
        }
        if out.len() >= COMPARE_MAX_DIR_ENTRIES {
            return Err(ListFailure::Reason(CompareReason::DirTooLarge));
        }
        out.push(entry);
    }
    Ok(out)
}

/// `true` si `path` es hijo DIRECTO de `dir`: mismo scheme y misma authority,
/// y sus segmentos son los de `dir` más exactamente uno.
///
/// Byte-exacto (regla dura 1): compara segmentos crudos, jamás la forma wire
/// —que confundiría `a` con `ab`— ni un string.
///
/// Es más estricto que «cae bajo `dir`» a propósito: lo que un `list` puede
/// devolver legítimamente son sus hijos, y un nieto en la lista ya es un
/// provider que no está contestando a la pregunta que se le hizo.
fn is_direct_child(dir: &VPath, path: &VPath) -> bool {
    if dir.scheme() != path.scheme() || dir.authority() != path.authority() {
        return false;
    }
    let mut d = dir.segments();
    let mut p = path.segments();
    loop {
        match (d.next(), p.next()) {
            // `dir` se agotó: queda exactamente un segmento por consumir.
            (None, Some(_)) => return p.next().is_none(),
            (Some(ds), Some(ps)) if ds == ps => {}
            _ => return false,
        }
    }
}

/// Las claves COLISIONADAS de un lado, con el motivo de su colisión.
///
/// Solo recorre las entradas que ya colisionan (casi siempre ninguna), no el
/// listado entero. El motivo que se guarda es el de la primera entrada del
/// grupo en orden de listado: un grupo puede tener causas distintas por
/// pareja, y la fila del lado contrario necesita UNA.
fn collided_keys(index: &SideIndex<'_, Entry>, sides: Sides) -> BTreeMap<Vec<u8>, CompareReason> {
    let mut out = BTreeMap::new();
    for (entry, reason) in index.collisions() {
        out.entry(key_for(entry.pair_name(), sides).as_bytes().to_vec())
            .or_insert(reason);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;
    use norte_proto::Segment;
    use norte_testkit::{MemProvider, TarSmith};
    use norte_vfs_archive::{ArchiveProvider, Format};
    use norte_vfs_local::LocalProvider;

    use super::*;

    // ---------- utillería de árboles ----------

    /// Trocea `"sub/deep/c.txt"` en segmentos crudos. Los tests hablan `&str`
    /// por comodidad; lo que viaja al provider son BYTES (regla dura 1).
    fn segments(path: &str) -> Vec<Segment> {
        path.split('/')
            .map(|s| Segment::new(s.as_bytes().to_vec()).expect("segmento válido"))
            .collect()
    }

    /// Crea `path` con `content`, materializando sus directorios intermedios.
    async fn seed(mem: &MemProvider, path: &str, content: &[u8]) {
        let segs = segments(path);
        let (name, dirs) = segs.split_last().expect("path no vacío");
        let mut at = MemProvider::root();
        for dir in dirs {
            at = at.join(dir.clone());
            // Ya existe: el árbol lo comparten varios paths sembrados.
            let _ = mem.mkdir(&at).await;
        }
        let file = at.join(name.clone());
        let mut sink = mem.write(&file).await.expect("write");
        sink.write(Bytes::copy_from_slice(content))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    /// Un árbol con esos ficheros; el contenido de cada uno es su propio path,
    /// así que dos árboles con la misma lista salen idénticos byte a byte.
    async fn tree(paths: &[&str]) -> MemProvider {
        let mem = MemProvider::new();
        for path in paths {
            seed(&mem, path, path.as_bytes()).await;
        }
        mem
    }

    /// Dos árboles idénticos. Los mtimes de `MemProvider` son un reloj LÓGICO
    /// (una unidad por mutación), así que sembrar la misma lista en el mismo
    /// orden da las mismas fechas: nada de este test depende del reloj de
    /// pared.
    async fn twin_trees(paths: &[&str]) -> (MemProvider, MemProvider) {
        (tree(paths).await, tree(paths).await)
    }

    /// Un `wide/` con `n` entradas a la IZQUIERDA y vacío a la derecha. Ancho
    /// de un solo lado a propósito: el techo se comprueba por lado, y sembrar
    /// el doble solo dobla lo que tarda el test.
    async fn twin_trees_with_wide_dir(n: usize) -> (MemProvider, MemProvider) {
        let left = MemProvider::new();
        let right = MemProvider::new();
        let wide = MemProvider::root().join(Segment::new(b"wide".to_vec()).expect("seg"));
        left.mkdir(&wide).await.expect("mkdir");
        right.mkdir(&wide).await.expect("mkdir");
        for i in 0..n {
            let name = Segment::new(format!("e{i:07}").into_bytes()).expect("seg");
            left.mkdir(&wide.join(name)).await.expect("mkdir");
        }
        (left, right)
    }

    /// Árboles con una fila de cada categoría barata.
    async fn trees_that_differ() -> (MemProvider, MemProvider) {
        let left = tree(&["igual.txt", "solo-izq.txt", "sub/dentro.txt"]).await;
        let right = tree(&["igual.txt", "solo-der.txt", "sub/dentro.txt"]).await;
        seed(&left, "tamano.txt", b"aaaa").await;
        seed(&right, "tamano.txt", b"aaaaaaaaaaaa").await;
        (left, right)
    }

    /// El `VPath` de `path` dentro de un [`MemProvider`].
    fn at(path: &str) -> VPath {
        let mut out = MemProvider::root();
        for seg in segments(path) {
            out = out.join(seg);
        }
        out
    }

    /// El `list` de `dir` falla con E/S; el resto del árbol se lista normal.
    fn deny_list(mem: &MemProvider, dir: &str) {
        mem.faults().fail_list_at(&at(dir));
    }

    /// Dos árboles de UN fichero con el mismo nombre y contenidos distintos.
    ///
    /// Sembrar los dos con la misma secuencia de mutaciones les da la MISMA
    /// fecha (el mtime de `MemProvider` es un reloj lógico), así que con
    /// contenidos del mismo tamaño los rungs baratos no pueden distinguirlos:
    /// es exactamente la pareja que el rung de hash existe para cazar.
    async fn pair_with_content(
        name: &str,
        left: &[u8],
        right: &[u8],
    ) -> (MemProvider, MemProvider) {
        let l = MemProvider::new();
        let r = MemProvider::new();
        seed(&l, name, left).await;
        seed(&r, name, right).await;
        (l, r)
    }

    // ---------- utillería de filas ----------

    fn compare_with<'a>(
        left: &'a MemProvider,
        right: &'a MemProvider,
        opts: CompareOptions,
    ) -> CompareStream<'a> {
        compare(
            left,
            &MemProvider::root(),
            right,
            &MemProvider::root(),
            opts,
            CancellationToken::new(),
        )
    }

    fn compare_default<'a>(left: &'a MemProvider, right: &'a MemProvider) -> CompareStream<'a> {
        compare_with(left, right, CompareOptions::cheap())
    }

    async fn collect(stream: CompareStream<'_>) -> Vec<CompareRow> {
        stream
            .map(|item| item.expect("ninguna de estas comparaciones se cancela"))
            .collect()
            .await
    }

    /// ¿Alguno de los dos lados de la fila se llama así? Por BYTES: `VPath` no
    /// tiene `as_bytes` porque un nombre no es texto (regla dura 1).
    fn named(row: &CompareRow, name: &[u8]) -> bool {
        [row.left.as_ref(), row.right.as_ref()]
            .into_iter()
            .flatten()
            .any(|entry| entry.path.file_name().map(Segment::as_bytes) == Some(name))
    }

    fn flip(side: Side) -> Side {
        match side {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            // `Side` NO es `#[non_exhaustive]` (a diferencia de los cuatro
            // vocabularios de la comparación): un lado que este binario no
            // conoce no tiene espejo, y decir que sí lo tiene sería inventárselo.
            Side::Unknown => Side::Unknown,
        }
    }

    /// Las mismas filas vistas desde el otro lado: entradas, veredicto, lado
    /// más nuevo y lado del motivo, todo cambiado de sitio. Nada más.
    fn mirror(rows: &[CompareRow]) -> Vec<CompareRow> {
        rows.iter()
            .map(|row| CompareRow {
                left: row.right.clone(),
                right: row.left.clone(),
                verdict: match row.verdict {
                    CompareVerdict::OnlyLeft => CompareVerdict::OnlyRight,
                    CompareVerdict::OnlyRight => CompareVerdict::OnlyLeft,
                    other => other,
                },
                newer: row.newer.map(flip),
                side: row.side.map(flip),
                ..row.clone()
            })
            .collect()
    }

    // ---------- los tests del plan ----------

    /// The base case, and the one a user runs after every copy: two identical
    /// trees produce nothing but `Same`, at every depth.
    #[tokio::test]
    async fn identical_trees_are_all_same() {
        let (l, r) = twin_trees(&["a.txt", "sub/b.txt", "sub/deep/c.txt"]).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert!(
            rows.iter().all(|row| row.verdict == CompareVerdict::Same),
            "{rows:#?}"
        );
        assert!(rows.iter().any(|row| named(row, b"c.txt")), "{rows:#?}");
    }

    /// A directory that exists on one side only is ONE row, not its whole
    /// subtree: the plan will copy it with a recursive `fs.copy`, so
    /// enumerating it buys nothing and costs the walk everything.
    #[tokio::test]
    async fn an_orphan_directory_is_one_row_and_is_not_enumerated() {
        let l = tree(&["only/1.txt", "only/2.txt", "only/deep/3.txt"]).await;
        let r = tree(&[]).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::OnlyLeft);
        assert_eq!(
            rows[0].left.as_ref().expect("el lado que sí está").kind,
            EntryKind::Dir
        );
    }

    /// Swapping the sides mirrors the verdicts and nothing else. A comparison
    /// that is not symmetric is a comparison that has a favourite.
    #[tokio::test]
    async fn comparing_the_other_way_round_mirrors_the_verdicts() {
        let (l, r) = trees_that_differ().await;
        let forward = collect(compare_default(&l, &r)).await;
        let backward = collect(compare_default(&r, &l)).await;
        assert!(!forward.is_empty());
        assert_eq!(mirror(&forward), backward);
    }

    /// One unreadable subdirectory must cost ITSELF, not the other 40 000
    /// leaves. This is the difference between a three-hour comparison that
    /// answers and one that dies at the first EACCES.
    #[tokio::test]
    async fn an_unreadable_directory_is_a_row_and_the_walk_continues() {
        let (l, r) = twin_trees(&["ok.txt", "denied/x.txt", "after/y.txt"]).await;
        deny_list(&l, "denied");
        let rows = collect(compare_default(&l, &r)).await;
        let bad = rows
            .iter()
            .find(|row| row.verdict == CompareVerdict::Error)
            .expect("error row");
        assert_eq!(bad.reason, Some(CompareReason::Unreadable));
        assert_eq!(bad.side, Some(Side::Left));
        assert!(
            rows.iter().any(|row| named(row, b"y.txt")),
            "the walk stopped at the error"
        );
    }

    /// Un provider que lista un path de FUERA del directorio no consigue que
    /// la comparación lo empareje, lo nombre en una fila ni —con el rung de
    /// hash— lo lea: el listado entero vale como ilegible.
    ///
    /// Ningún provider del árbol puede hacer esto hoy (todos construyen el
    /// hijo con `dir.join(Segment)`, y `Segment` rechaza `/`, `.` y `..`), y
    /// justo por eso hace falta el test: la frontera del gate de `fs.compare`
    /// son las dos RAÍCES, así que un path colado por un provider de plugin se
    /// saltaría el scope entero. Es defensa en profundidad, y sin test es una
    /// intención.
    #[tokio::test]
    async fn una_entrada_fuera_del_directorio_invalida_el_listado() {
        let honest = tree(&["dentro.txt"]).await;
        let liar = LiarProvider {
            inner: tree(&["dentro.txt"]).await,
            at: MemProvider::root(),
            escape: Entry {
                // NIETO de la raíz, no hijo: `mem:///secreto` sería una
                // entrada legítima de `mem:///` y no probaría nada.
                path: at("fuera/secreto"),
                kind: EntryKind::File,
                size: Some(1),
                mtime_ms: Some(0),
                attrs: BTreeMap::new(),
            },
        };
        let rows = collect(compare(
            &liar,
            &MemProvider::root(),
            &honest,
            &MemProvider::root(),
            CompareOptions::cheap(),
            CancellationToken::new(),
        ))
        .await;
        assert!(
            rows.iter().all(|row| row.verdict == CompareVerdict::Error
                && row.reason == Some(CompareReason::Unreadable)),
            "un listado que se sale de su directorio no se empareja: {rows:#?}"
        );
        assert!(
            !rows.iter().any(|row| named(row, b"secreto")),
            "el path colado no puede llegar a una fila: {rows:#?}"
        );
    }

    /// La regla, aislada: hijo directo sí, nieto no, el propio directorio no,
    /// otro scheme o authority no.
    #[test]
    fn is_direct_child_es_exacto() {
        let vp = |wire: &str| VPath::parse(wire).expect("wire válido");
        let dir = vp("mem:///a/b");
        assert!(is_direct_child(&dir, &vp("mem:///a/b/c")));
        assert!(!is_direct_child(&dir, &vp("mem:///a/b/c/d")), "nieto");
        assert!(!is_direct_child(&dir, &vp("mem:///a/b")), "él mismo");
        assert!(!is_direct_child(&dir, &vp("mem:///a")), "su padre");
        assert!(!is_direct_child(&dir, &vp("mem:///a/bb/c")), "hermano");
        assert!(!is_direct_child(&dir, &vp("file:///a/b/c")), "otro scheme");
        assert!(
            !is_direct_child(&vp("sftp://uno/x"), &vp("sftp://otro/x/y")),
            "otra authority"
        );
    }

    /// Un `MemProvider` con UN listado envenenado: para `at` devuelve `escape`
    /// (un path que no es hijo suyo) y para todo lo demás delega.
    struct LiarProvider {
        inner: MemProvider,
        at: VPath,
        escape: Entry,
    }

    #[async_trait::async_trait]
    impl Provider for LiarProvider {
        fn scheme(&self) -> &str {
            self.inner.scheme()
        }
        fn capabilities(&self) -> norte_proto::Capabilities {
            self.inner.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, norte_proto::Error> {
            self.inner.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
            if p == &self.at {
                let escape = self.escape.clone();
                return Ok(stream::once(async move { Ok(escape) }).boxed());
            }
            self.inner.list(p).await
        }
        async fn read(
            &self,
            p: &VPath,
            range: Option<norte_proto::ByteRange>,
        ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
            self.inner.read(p, range).await
        }
        async fn write(
            &self,
            p: &VPath,
        ) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
            self.inner.write(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.mkdir(p).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.remove(p).await
        }
        async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.rename(from, to).await
        }
    }

    /// A directory over the declared cap costs that directory, not an OOM.
    #[tokio::test]
    async fn a_directory_over_the_cap_is_a_row_not_an_oom() {
        let (l, r) = twin_trees_with_wide_dir(COMPARE_MAX_DIR_ENTRIES + 1).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert!(
            rows.iter()
                .any(|row| row.reason == Some(CompareReason::DirTooLarge)),
            "{rows:#?}"
        );
    }

    /// Hard rule 3. Cancelling stops the stream — no row after the cut, no work
    /// after the cut, and nothing to clean up because nothing is written.
    #[tokio::test]
    async fn cancelling_stops_the_stream_cleanly() {
        let (l, r) = twin_trees_with_wide_dir(5_000).await;
        let cancel = CancellationToken::new();
        let mut stream = compare(
            &l,
            &MemProvider::root(),
            &r,
            &MemProvider::root(),
            CompareOptions::cheap(),
            cancel.clone(),
        );
        let first = stream.next().await.expect("at least one row");
        assert!(first.is_ok());
        cancel.cancel();
        let rest = stream.count().await;
        assert!(
            rest < 5_000,
            "the walk kept going after cancellation: {rest} more rows"
        );
    }

    /// Un token que ya venía disparado no empareja NADA: ni un listado, ni una
    /// fila. La cancelación se mira antes de trabajar, no después.
    #[tokio::test]
    async fn un_token_ya_cancelado_no_empareja_nada() {
        let (l, r) = twin_trees(&["a.txt", "sub/b.txt"]).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let items: Vec<Result<CompareRow, CompareError>> = compare(
            &l,
            &MemProvider::root(),
            &r,
            &MemProvider::root(),
            CompareOptions::cheap(),
            cancel,
        )
        .collect()
        .await;
        assert_eq!(items, vec![Err(CompareError::Cancelled)]);
    }

    /// El drenaje de UN directorio mira el token entrada a entrada.
    ///
    /// Es la mitad de la regla dura 3 que los tests de flujo no pueden ver: un
    /// directorio de cientos de miles de entradas se drena DENTRO de un solo
    /// paso del flujo, así que sin este chequeo la cancelación esperaría a que
    /// terminase. Se prueba sobre `list_all` directamente porque hacerlo por el
    /// flujo exigiría cancelar a mitad de un `await`, que es una carrera.
    #[tokio::test]
    async fn el_drenaje_de_un_directorio_mira_el_token() {
        let mem = tree(&["a.txt", "b.txt"]).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let outcome = list_all(&mem, &MemProvider::root(), &cancel).await;
        assert!(
            matches!(outcome, Err(ListFailure::Cancelled)),
            "el drenaje siguió con el token disparado"
        );
        // Y sin cancelar, el mismo listado sí se drena entero.
        let ok = list_all(&mem, &MemProvider::root(), &CancellationToken::new()).await;
        assert!(matches!(ok, Ok(entries) if entries.len() == 2));
    }

    /// `max_depth` bounds the descent and says so by not emitting deeper rows.
    #[tokio::test]
    async fn max_depth_bounds_the_descent() {
        let (l, r) = twin_trees(&["a.txt", "one/b.txt", "one/two/c.txt"]).await;
        let rows = collect(compare_with(&l, &r, CompareOptions::cheap().max_depth(1))).await;
        assert!(rows.iter().any(|row| named(row, b"b.txt")), "{rows:#?}");
        assert!(!rows.iter().any(|row| named(row, b"c.txt")), "{rows:#?}");
    }

    /// `max_depth(0)` empareja SOLO la raíz: sus hijos directos salen como
    /// filas y ningún directorio se abre.
    ///
    /// Es el extremo del contrato de `max_depth`, que la rustdoc podía leerse
    /// de dos maneras y ahora dice de una.
    #[tokio::test]
    async fn max_depth_cero_empareja_solo_la_raiz() {
        let (l, r) = twin_trees(&["a.txt", "one/b.txt"]).await;
        let rows = collect(compare_with(&l, &r, CompareOptions::cheap().max_depth(0))).await;
        assert!(rows.iter().any(|row| named(row, b"a.txt")), "{rows:#?}");
        assert!(rows.iter().any(|row| named(row, b"one")), "{rows:#?}");
        assert!(!rows.iter().any(|row| named(row, b"b.txt")), "{rows:#?}");
    }

    /// `follow_symlinks` se acepta y NO hace nada.
    ///
    /// Una opción que se acepta y se ignora es peor que una que no existe: el
    /// llamante cree haber pedido algo. Mientras siga en la struct —y en el
    /// wire—, esto fija que no cambia ni una fila, para que quien la implemente
    /// algún día vea este test caerse.
    #[tokio::test]
    async fn follow_symlinks_se_acepta_y_no_hace_nada() {
        let (l, r) = twin_trees(&["a.txt", "sub/b.txt"]).await;
        l.symlink(&at("enlace"), b"../fuera", norte_vfs::SymlinkKind::File)
            .await
            .expect("symlink");

        let sin = collect(compare_default(&l, &r)).await;
        let con = collect(compare_with(
            &l,
            &r,
            CompareOptions {
                follow_symlinks: true,
                ..CompareOptions::cheap()
            },
        ))
        .await;
        assert_eq!(sin, con);
    }

    // ---------- lo que el plan no fija ----------

    /// El walk LEE los destinos de los enlaces y se los pasa a la cascada.
    ///
    /// La cascada ya prueba que dos destinos distintos son `Different`; lo que
    /// falta probar aquí es que alguien llama a `read_link`. Sin este test, un
    /// walk que no leyera un solo destino seguiría pasando el test del archivo
    /// —que espera `Unknown` precisamente porque los destinos NO se pueden
    /// leer—: la ausencia de la llamada y la ausencia de la respuesta se ven
    /// igual desde fuera.
    #[tokio::test]
    async fn el_walk_lee_los_destinos_de_los_enlaces() {
        let link = || MemProvider::root().join(Segment::new(b"l".to_vec()).expect("seg"));
        let sembrar = async |target: &'static [u8]| {
            let mem = MemProvider::new();
            mem.symlink(&link(), target, norte_vfs::SymlinkKind::File)
                .await
                .expect("symlink");
            mem
        };

        let l = sembrar(b"../a").await;
        let distinto = sembrar(b"../b").await;
        let rows = collect(compare_default(&l, &distinto)).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(
            (rows[0].verdict, rows[0].criterion, rows[0].confidence),
            (
                CompareVerdict::Different,
                CompareCriterion::LinkTarget,
                CompareConfidence::Certain
            ),
            "{rows:#?}"
        );

        let igual = sembrar(b"../a").await;
        let rows = collect(compare_default(&l, &igual)).await;
        assert_eq!(
            (rows[0].verdict, rows[0].criterion, rows[0].confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::LinkTarget,
                CompareConfidence::Certain
            ),
            "{rows:#?}"
        );
    }

    /// Un enlace contra un fichero no gasta un `read_link`: el rung de kind ya
    /// decidió, y preguntar por el destino del otro es una llamada al provider
    /// a cambio de nada.
    #[tokio::test]
    async fn un_enlace_contra_un_fichero_es_type_mismatch() {
        let l = MemProvider::new();
        l.symlink(
            &MemProvider::root().join(Segment::new(b"x".to_vec()).expect("seg")),
            b"../a",
            norte_vfs::SymlinkKind::File,
        )
        .await
        .expect("symlink");
        let r = tree(&["x"]).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::TypeMismatch);
        assert_eq!(rows[0].criterion, CompareCriterion::Kind);
    }

    /// El walk sigue DESPUÉS del error, no solo alrededor: `zz` ordena por
    /// detrás de `denied`, así que su fila solo puede existir si el recorrido
    /// continuó tras la fila de error.
    #[tokio::test]
    async fn el_walk_sigue_despues_del_directorio_ilegible() {
        let (l, r) = twin_trees(&["denied/x.txt", "zz/z.txt"]).await;
        deny_list(&l, "denied");
        let rows = collect(compare_default(&l, &r)).await;
        let error_at = rows
            .iter()
            .position(|row| row.verdict == CompareVerdict::Error)
            .expect("la fila de error");
        let z_at = rows
            .iter()
            .position(|row| named(row, b"z.txt"))
            .expect("la hoja de después");
        assert!(error_at < z_at, "{rows:#?}");
    }

    /// El id es monótono y no se repite: la selección del panel se ancla a él.
    #[tokio::test]
    async fn los_ids_son_monotonos_y_unicos() {
        let (l, r) = twin_trees(&["a.txt", "sub/b.txt", "sub/deep/c.txt"]).await;
        let rows = collect(compare_default(&l, &r)).await;
        let ids: Vec<u64> = rows.iter().map(|row| row.id).collect();
        let mut ordenados = ids.clone();
        ordenados.sort_unstable();
        ordenados.dedup();
        assert_eq!(ids, ordenados, "{ids:?}");
    }

    /// Dos entradas de un mismo lado que colapsan salen como DOS filas
    /// `Ambiguous`, cada una en el campo de SU lado y con `side` nombrando
    /// dónde está la colisión. Jamás se funden y jamás se deduplican.
    #[tokio::test]
    async fn una_colision_de_un_lado_sale_una_fila_por_entrada() {
        let left = tree(&["README", "readme"]).await;
        // Un lado que no distingue caja: emparejar contra él es plegar.
        let right = MemProvider::with_flags(norte_vfs::CapabilityFlags::CASE_PRESERVING);
        seed(&right, "README", b"README").await;

        let rows = collect(compare_default(&left, &right)).await;
        let ambiguas: Vec<&CompareRow> = rows
            .iter()
            .filter(|row| row.verdict == CompareVerdict::Ambiguous)
            .collect();
        assert_eq!(
            ambiguas.len(),
            3,
            "dos colisionadas + su contraparte: {rows:#?}"
        );
        assert!(
            ambiguas
                .iter()
                .all(|row| row.reason == Some(CompareReason::CaseFold)
                    && row.side == Some(Side::Left)),
            "{ambiguas:#?}"
        );
        // Las dos de la izquierda llevan SU entrada; la contraparte, la suya.
        let izquierdas = ambiguas.iter().filter(|row| row.left.is_some()).count();
        let derechas = ambiguas.iter().filter(|row| row.right.is_some()).count();
        assert_eq!((izquierdas, derechas), (2, 1), "{ambiguas:#?}");
        assert!(
            ambiguas
                .iter()
                .all(|row| row.left.is_none() || row.right.is_none()),
            "una colisión es de UN lado: {ambiguas:#?}"
        );
    }

    /// La contraparte solitaria de una colisión NO es `OnlyRight`.
    ///
    /// Decirle `OnlyRight` a un plan de sincronización es decirle «cópialo al
    /// otro lado», y copiar dentro de un directorio que ya no sabe distinguir
    /// esos dos nombres crea un TERCER fichero colisionado. `Ambiguous` hace
    /// que ese plan se niegue a actuar, que es la única respuesta segura.
    #[tokio::test]
    async fn la_contraparte_de_una_colision_no_se_ofrece_para_copiar() {
        let left = tree(&["README", "readme"]).await;
        let right = MemProvider::with_flags(norte_vfs::CapabilityFlags::CASE_PRESERVING);
        seed(&right, "README", b"README").await;

        let rows = collect(compare_default(&left, &right)).await;
        let contraparte = rows
            .iter()
            .find(|row| row.right.is_some() && row.left.is_none())
            .expect("la fila de la contraparte");
        assert_eq!(contraparte.verdict, CompareVerdict::Ambiguous);
        assert_eq!(contraparte.reason, Some(CompareReason::CaseFold));
        assert_eq!(
            contraparte.side,
            Some(Side::Left),
            "el lado que colisiona es el izquierdo, no el de la fila"
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.verdict == CompareVerdict::OnlyRight),
            "{rows:#?}"
        );
    }

    /// Dos directorios ilegibles emparejados son DOS filas, una por lado.
    ///
    /// Volverse en el primer fallo es lo cómodo y deja el segundo sin
    /// descubrir: el usuario arreglaría los permisos de la izquierda y la
    /// comparación siguiente le enseñaría el mismo directorio roto otra vez,
    /// ahora por el otro lado.
    #[tokio::test]
    async fn dos_lados_ilegibles_son_dos_filas() {
        let (l, r) = twin_trees(&["dir/a.txt"]).await;
        deny_list(&l, "dir");
        deny_list(&r, "dir");
        let rows = collect(compare_default(&l, &r)).await;
        let errores: Vec<&CompareRow> = rows
            .iter()
            .filter(|row| row.verdict == CompareVerdict::Error)
            .collect();
        assert_eq!(errores.len(), 2, "{rows:#?}");
        assert_eq!(errores[0].side, Some(Side::Left));
        assert_eq!(errores[1].side, Some(Side::Right));
        // Cada fila lleva el directorio del lado que nombra, y solo ese.
        assert!(errores[0].left.is_some() && errores[0].right.is_none());
        assert!(errores[1].right.is_some() && errores[1].left.is_none());
    }

    /// Un directorio ilegible NO convierte en `OnlyRight` lo que hay enfrente:
    /// nadie ha comprobado esa ausencia.
    #[tokio::test]
    async fn un_listado_roto_no_inventa_ausencias_en_el_otro_lado() {
        let (l, r) = twin_trees(&["dir/a.txt", "dir/b.txt"]).await;
        deny_list(&l, "dir");
        let rows = collect(compare_default(&l, &r)).await;
        assert!(
            !rows
                .iter()
                .any(|row| row.verdict == CompareVerdict::OnlyRight),
            "{rows:#?}"
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.verdict == CompareVerdict::Error)
                .count(),
            1,
            "{rows:#?}"
        );
    }

    // ---------- el rung de hash ----------

    /// The point of the rung: same size, same mtime, different bytes. Every
    /// cheap criterion says `Same`; only the hash tells the truth. This is the
    /// case a user turns hashing on FOR.
    #[tokio::test]
    async fn same_size_same_mtime_different_bytes_is_caught_only_by_hash() {
        let (l, r) = pair_with_content("x.bin", b"aaaa", b"bbbb").await;

        let cheap = collect(compare_default(&l, &r)).await;
        assert_eq!(cheap.len(), 1, "{cheap:#?}");
        assert_eq!(cheap[0].verdict, CompareVerdict::Same);
        assert_eq!(cheap[0].confidence, CompareConfidence::Probable);

        let hashed = collect(compare_with(&l, &r, CompareOptions::cheap().with_hash())).await;
        assert_eq!(hashed.len(), 1, "{hashed:#?}");
        assert_eq!(hashed[0].verdict, CompareVerdict::Different);
        assert_eq!(hashed[0].criterion, CompareCriterion::Hash);
        assert_eq!(hashed[0].confidence, CompareConfidence::Certain);
    }

    /// With hash off, comparison reads no content at all. A user who did not
    /// ask to hash a terabyte over SFTP must not be made to.
    #[tokio::test]
    async fn without_the_hash_rung_no_content_is_read() {
        let (l, r) = twin_trees(&["a.txt", "b.txt"]).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(l.faults().read_calls(), 0);
        assert_eq!(r.faults().read_calls(), 0);
    }

    /// The hash only reaches the pairs the cheap rungs called equal. Hashing a
    /// pair already known to differ is pure waste.
    #[tokio::test]
    async fn the_hash_only_runs_on_pairs_the_cheap_rungs_called_equal() {
        let l = MemProvider::new();
        let r = MemProvider::new();
        // Misma secuencia de mutaciones en los dos lados: mismas fechas.
        seed(&l, "igual.bin", b"aaaa").await;
        seed(&r, "igual.bin", b"aaaa").await;
        seed(&l, "tamano.bin", b"aa").await;
        seed(&r, "tamano.bin", b"aaaaaaaaaa").await;

        let rows = collect(compare_with(&l, &r, CompareOptions::cheap().with_hash())).await;
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(
            l.faults().read_calls(),
            1,
            "only the equal-looking pair should be read: {rows:#?}"
        );
        assert_eq!(r.faults().read_calls(), 1, "{rows:#?}");
    }

    /// A read that fails mid-hash costs its row, not the walk — and says which
    /// side failed.
    ///
    /// El criterio es `Hash` y no `Presence`: el rung CORRIÓ y se murió. La
    /// convención de `Presence` es para las filas donde no corrió ninguno.
    #[tokio::test]
    async fn a_read_that_fails_mid_hash_is_an_error_row() {
        let (l, r) = pair_with_content("x.bin", b"aaaa", b"aaaa").await;
        l.faults().fail_read_at(&at("x.bin"), 2);
        let rows = collect(compare_with(&l, &r, CompareOptions::cheap().with_hash())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::Error);
        assert_eq!(rows[0].reason, Some(CompareReason::ReadFailed));
        assert_eq!(rows[0].side, Some(Side::Left));
        assert_eq!(rows[0].criterion, CompareCriterion::Hash);
        assert!(
            rows[0].left.is_some() && rows[0].right.is_some(),
            "la pareja se emparejó: la fila lleva los dos lados"
        );
        assert!(rows[0].reason_is_consistent());
    }

    /// Un lado que no se puede leer no hace leer el otro: la fila ya es de
    /// error, y leer el segundo fichero entero no cambiaría ni una letra de
    /// ella. Sobre 40 GB eso es media hora regalada.
    #[tokio::test]
    async fn una_lectura_rota_no_arrastra_al_otro_lado() {
        let (l, r) = pair_with_content("x.bin", b"aaaa", b"aaaa").await;
        l.faults().fail_read_at(&at("x.bin"), 2);
        collect(compare_with(&l, &r, CompareOptions::cheap().with_hash())).await;
        assert_eq!(l.faults().read_calls(), 1);
        assert_eq!(r.faults().read_calls(), 0);
    }

    /// El walk SIGUE tras una lectura rota, igual que sigue tras un listado
    /// ilegible: una comparación de tres horas no se muere en la hoja 40 000.
    #[tokio::test]
    async fn el_walk_sigue_despues_de_una_lectura_rota() {
        let l = MemProvider::new();
        let r = MemProvider::new();
        seed(&l, "a.bin", b"aaaa").await;
        seed(&r, "a.bin", b"aaaa").await;
        seed(&l, "zz.bin", b"zzzz").await;
        seed(&r, "zz.bin", b"zzzz").await;
        l.faults().fail_read_at(&at("a.bin"), 2);

        let rows = collect(compare_with(&l, &r, CompareOptions::cheap().with_hash())).await;
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::Error, "{rows:#?}");
        let zz = rows
            .iter()
            .find(|row| named(row, b"zz.bin"))
            .expect("la hoja de después de la lectura rota");
        assert_eq!(zz.verdict, CompareVerdict::Same);
        assert_eq!(zz.criterion, CompareCriterion::Hash);
        assert_eq!(zz.confidence, CompareConfidence::Certain);
    }

    /// Con el rung caro encendido la comparación sigue siendo SIMÉTRICA: el
    /// lado que falla al leer cambia de sitio, y nada más.
    #[tokio::test]
    async fn el_rung_de_hash_tambien_es_simetrico() {
        let (l, r) = pair_with_content("x.bin", b"aaaa", b"bbbb").await;
        let opts = CompareOptions::cheap().with_hash();
        let ida = collect(compare_with(&l, &r, opts)).await;
        let vuelta = collect(compare_with(&r, &l, opts)).await;
        assert_eq!(mirror(&ida), vuelta);

        l.faults().fail_read_at(&at("x.bin"), 2);
        let ida = collect(compare_with(&l, &r, opts)).await;
        let vuelta = collect(compare_with(&r, &l, opts)).await;
        assert_eq!(ida[0].side, Some(Side::Left));
        assert_eq!(vuelta[0].side, Some(Side::Right));
        assert_eq!(mirror(&ida), vuelta);
    }

    /// Cancelar mientras el rung caro lee no publica una fila provisional: el
    /// flujo termina en [`CompareError::Cancelled`] y ya está.
    #[tokio::test]
    async fn cancelar_durante_el_hash_no_publica_la_pareja() {
        let (l, r) = pair_with_content("x.bin", b"aaaa", b"aaaa").await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let items: Vec<Result<CompareRow, CompareError>> = compare(
            &l,
            &MemProvider::root(),
            &r,
            &MemProvider::root(),
            CompareOptions::cheap().with_hash(),
            cancel,
        )
        .collect()
        .await;
        assert_eq!(items, vec![Err(CompareError::Cancelled)]);
        assert_eq!(l.faults().read_calls(), 0, "ni se abrió el fichero");
    }

    // ---------- el provider que la gente USA ----------
    //
    // `norte-vfs-local::list` no rellena `size` ni `mtime_ms` (#52: stat lazy,
    // `None` = «no lo sé», contrato de `Entry`). Toda la suite de arriba corre
    // sobre `MemProvider`, que SÍ los rellena, así que ninguno de sus tests
    // puede ver lo único que le pasa a un usuario: comparar dos directorios
    // locales. Estos tres van contra directorios temporales de verdad.

    /// Siembra un directorio temporal y lo sirve por `norte-vfs-local`.
    ///
    /// `sembrar` recibe la ruta NATIVA: los tests que fijan fechas escriben
    /// ahí. El `TempDir` viaja dentro del provider (`with_guard`), así que vive
    /// exactamente lo que él.
    fn local_tree(sembrar: impl FnOnce(&std::path::Path)) -> LocalProvider {
        let dir = tempfile::tempdir().expect("tempdir");
        sembrar(dir.path());
        let base = dir.path().to_path_buf();
        LocalProvider::rooted(base).with_guard(Box::new(dir))
    }

    /// Fija la fecha de modificación de un fichero, en segundos desde epoch.
    ///
    /// El reloj de pared no sirve: dos ficheros escritos seguidos pueden caer
    /// dentro de la tolerancia de 2 s, o no, según lo cargada que esté la
    /// máquina. Un test que a veces pasa no prueba nada.
    fn set_mtime(path: &std::path::Path, secs: u64) {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("abrir para fijar la fecha");
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .expect("fijar la fecha");
    }

    fn compare_local<'a>(
        left: &'a LocalProvider,
        right: &'a LocalProvider,
        opts: CompareOptions,
    ) -> CompareStream<'a> {
        compare(
            left,
            &LocalProvider::root(),
            right,
            &LocalProvider::root(),
            opts,
            CancellationToken::new(),
        )
    }

    /// Dos ficheros locales de 5 y 12 bytes son DIFERENTES, y con certeza.
    ///
    /// Es el test que faltaba: sin hidratar, `list` no trae el tamaño, el rung
    /// de tamaño se corta en `Same`/`Unknown` y la comparación local —la única
    /// que casi todo el mundo hace— no distingue dos ficheros que no se parecen
    /// en nada.
    #[tokio::test]
    async fn dos_ficheros_locales_de_distinto_tamano_son_different() {
        let l = local_tree(|d| {
            std::fs::write(d.join("a.txt"), b"hola!").expect("sembrar");
        });
        let r = local_tree(|d| {
            std::fs::write(d.join("a.txt"), b"hola mundo!!").expect("sembrar");
        });

        let rows = collect(compare_local(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(
            (rows[0].verdict, rows[0].criterion, rows[0].confidence),
            (
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain
            ),
            "{rows:#?}"
        );
        // Y la fila LLEVA los tamaños que la decidieron: un panel que pinta
        // «distinto por tamaño» sobre dos tamaños vacíos no se puede leer.
        assert_eq!(rows[0].left.as_ref().expect("lado izquierdo").size, Some(5));
        assert_eq!(rows[0].right.as_ref().expect("lado derecho").size, Some(12));
    }

    /// Lo mismo un rung más abajo: mismo tamaño, fechas separadas por un
    /// minuto. Sin hidratar, el rung de fecha ni siquiera llega a correr.
    #[tokio::test]
    async fn dos_ficheros_locales_del_mismo_tamano_los_decide_la_fecha() {
        let l = local_tree(|d| {
            let p = d.join("a.txt");
            std::fs::write(&p, b"aaaa").expect("sembrar");
            set_mtime(&p, 1_700_000_000);
        });
        let r = local_tree(|d| {
            let p = d.join("a.txt");
            std::fs::write(&p, b"bbbb").expect("sembrar");
            set_mtime(&p, 1_700_000_060);
        });

        let rows = collect(compare_local(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(
            (rows[0].verdict, rows[0].criterion, rows[0].confidence),
            (
                CompareVerdict::Different,
                CompareCriterion::Mtime,
                CompareConfidence::Probable
            ),
            "{rows:#?}"
        );
        assert_eq!(rows[0].newer, Some(Side::Right));
        assert_eq!(
            rows[0].left.as_ref().expect("lado izquierdo").mtime_ms,
            Some(1_700_000_000_000)
        );
    }

    // ---------- a quién se le gasta un `stat`, y a quién no ----------

    /// Un `MemProvider` que LISTA como el provider local de verdad: sin `size`
    /// y sin `mtime_ms` (#52). Cuenta los `stat` y sabe fallarlos.
    ///
    /// El local no vale para esto: no se le pueden contar las llamadas ni
    /// romperle un `stat` concreto. Lo que sí prueba el local —que la
    /// comparación que hace todo el mundo funciona— son los dos tests de
    /// arriba; esto prueba a QUIÉN se le pregunta.
    struct LazyProvider {
        inner: MemProvider,
        stats: std::sync::atomic::AtomicUsize,
        /// Qué campos borra del listado. `false` = ese campo viaja como lo puso
        /// `MemProvider`.
        blank_size: bool,
        stat_fails: Option<norte_proto::Error>,
    }

    impl LazyProvider {
        /// Perezoso como el local: sin `size` y sin `mtime_ms`.
        fn new(inner: MemProvider) -> Self {
            Self {
                inner,
                stats: std::sync::atomic::AtomicUsize::new(0),
                blank_size: true,
                stat_fails: None,
            }
        }

        /// Perezoso SOLO en la fecha: la forma de un SFTP cuyo servidor no
        /// manda `ACMODTIME` en los atributos del `readdir` (el `size` de
        /// `russh_sftp` siempre viene; el `mtime` es opcional).
        fn only_mtime_missing(inner: MemProvider) -> Self {
            Self {
                blank_size: false,
                ..Self::new(inner)
            }
        }

        fn failing(inner: MemProvider, error: norte_proto::Error) -> Self {
            Self {
                stat_fails: Some(error),
                ..Self::new(inner)
            }
        }

        fn stats(&self) -> usize {
            self.stats.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    #[async_trait::async_trait]
    impl Provider for LazyProvider {
        fn scheme(&self) -> &str {
            self.inner.scheme()
        }
        fn capabilities(&self) -> norte_proto::Capabilities {
            self.inner.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, norte_proto::Error> {
            self.stats
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Some(error) = self.stat_fails.clone() {
                return Err(error);
            }
            self.inner.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
            let blank_size = self.blank_size;
            Ok(self
                .inner
                .list(p)
                .await?
                .map(move |item| {
                    item.map(|entry| Entry {
                        size: if blank_size { None } else { entry.size },
                        mtime_ms: None,
                        ..entry
                    })
                })
                .boxed())
        }
        async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, norte_proto::Error> {
            self.inner.read_link(p).await
        }
        async fn symlink(
            &self,
            link: &VPath,
            target: &[u8],
            kind: norte_vfs::SymlinkKind,
        ) -> Result<(), norte_proto::Error> {
            self.inner.symlink(link, target, kind).await
        }
        async fn read(
            &self,
            p: &VPath,
            range: Option<norte_proto::ByteRange>,
        ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
            self.inner.read(p, range).await
        }
        async fn write(
            &self,
            p: &VPath,
        ) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
            self.inner.write(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.mkdir(p).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.remove(p).await
        }
        async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.rename(from, to).await
        }
    }

    fn compare_lazy<'a>(
        left: &'a LazyProvider,
        right: &'a LazyProvider,
        opts: CompareOptions,
    ) -> CompareStream<'a> {
        compare(
            left,
            &MemProvider::root(),
            right,
            &MemProvider::root(),
            opts,
            CancellationToken::new(),
        )
    }

    /// La hidratación es SOLO para las parejas que van a usar el dato.
    ///
    /// Un huérfano lo decide la presencia, un tipo distinto lo decide el kind y
    /// dos directorios también (C3): a ninguno se le gasta un viaje al
    /// provider. Sobre SFTP, statear lo que ya está decidido es la diferencia
    /// entre una comparación y una espera.
    #[tokio::test]
    async fn lo_que_deciden_la_presencia_o_el_kind_no_gasta_un_stat() {
        // Un huérfano: la presencia decide y nadie pregunta nada.
        let l = LazyProvider::new(tree(&["solo.txt"]).await);
        let r = LazyProvider::new(tree(&[]).await);
        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::OnlyLeft);
        assert_eq!((l.stats(), r.stats()), (0, 0), "un huérfano no se statea");

        // Tipos distintos: los decide el kind.
        let l = LazyProvider::new(tree(&["x/dentro.txt"]).await);
        let r = LazyProvider::new(tree(&["x"]).await);
        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::TypeMismatch);
        assert_eq!(
            (l.stats(), r.stats()),
            (0, 0),
            "un tipo distinto no se statea"
        );

        // Dos directorios: los decide el kind; sus HIJOS sí se hidratan, y con
        // un solo `stat` por lado y por pareja.
        let l = LazyProvider::new(tree(&["d/a.txt"]).await);
        let r = LazyProvider::new(tree(&["d/a.txt"]).await);
        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(
            (l.stats(), r.stats()),
            (1, 1),
            "solo el fichero: el directorio lo decidió el kind"
        );
    }

    /// Un provider que YA rellena su listado no recibe una llamada de más.
    ///
    /// `MemProvider` trae `size` y `mtime_ms` en cada entrada, así que la
    /// comparación entera no gasta un solo `stat`. Sin esto, la hidratación
    /// podría dispararse siempre y nadie se enteraría: el veredicto sería el
    /// mismo y la factura, el doble.
    #[tokio::test]
    async fn un_listado_que_ya_trae_los_campos_no_se_vuelve_a_preguntar() {
        struct Contado(MemProvider, std::sync::atomic::AtomicUsize);
        #[async_trait::async_trait]
        impl Provider for Contado {
            fn scheme(&self) -> &str {
                self.0.scheme()
            }
            fn capabilities(&self) -> norte_proto::Capabilities {
                self.0.capabilities()
            }
            async fn stat(&self, p: &VPath) -> Result<Entry, norte_proto::Error> {
                self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.0.stat(p).await
            }
            async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
                self.0.list(p).await
            }
            async fn read(
                &self,
                p: &VPath,
                range: Option<norte_proto::ByteRange>,
            ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
                self.0.read(p, range).await
            }
            async fn write(
                &self,
                p: &VPath,
            ) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
                self.0.write(p).await
            }
            async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
                self.0.mkdir(p).await
            }
            async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
                self.0.remove(p).await
            }
            async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
                self.0.rename(from, to).await
            }
        }

        let count = || std::sync::atomic::AtomicUsize::new(0);
        let l = Contado(tree(&["a.txt", "sub/b.txt"]).await, count());
        let r = Contado(tree(&["a.txt", "sub/b.txt"]).await, count());
        let rows = collect(compare(
            &l,
            &MemProvider::root(),
            &r,
            &MemProvider::root(),
            CompareOptions::cheap(),
            CancellationToken::new(),
        ))
        .await;
        assert!(!rows.is_empty());
        assert_eq!(
            (
                l.1.load(std::sync::atomic::Ordering::Relaxed),
                r.1.load(std::sync::atomic::Ordering::Relaxed)
            ),
            (0, 0),
            "el listado ya traía los campos: {rows:#?}"
        );
    }

    /// Un `stat` que falla es una fila de ERROR, no un `Unknown`.
    ///
    /// `Unknown` es «el provider no puede contestar esta pregunta», y viaja con
    /// un veredicto `Same`. Un `stat` roto no es eso: es un `EACCES` que el
    /// usuario puede arreglar, o un fichero que desapareció entre el `list` y
    /// el `stat`. Contestar «iguales, no sé» a una pareja que nadie llegó a
    /// mirar es justo lo que el vocabulario de confianza existe para no hacer.
    #[tokio::test]
    async fn un_stat_que_falla_es_una_fila_de_error() {
        let l = LazyProvider::failing(
            tree(&["a.txt", "zz.txt"]).await,
            norte_proto::Error::Io { retryable: false },
        );
        let r = LazyProvider::new(tree(&["a.txt", "zz.txt"]).await);

        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 2, "{rows:#?}");
        for row in &rows {
            assert_eq!(row.verdict, CompareVerdict::Error, "{rows:#?}");
            assert_eq!(row.reason, Some(CompareReason::Unreadable));
            assert_eq!(row.side, Some(Side::Left));
            // El rung que se quedó sin su dato, igual que una lectura rota dice
            // `Hash`: ese rung corrió y se murió.
            assert_eq!(row.criterion, CompareCriterion::Size);
            assert!(
                row.left.is_some() && row.right.is_some(),
                "la pareja se emparejó: lo que falló fue describirla"
            );
            assert!(row.reason_is_consistent() && row.sides_are_consistent());
        }
        // El walk SIGUE tras el error —las dos filas están ahí— y el lado que
        // no falló no paga el viaje: la fila ya es de error y statear la
        // derecha no cambiaría ni una letra de ella.
        assert_eq!(l.stats(), 2, "un `stat` por pareja, no más");
        assert_eq!(r.stats(), 0, "un lado roto no arrastra al otro");
    }

    /// Un fichero que DESAPARECE entre el `list` y el `stat` sale como fila de
    /// error, y es una decisión.
    ///
    /// Es una carrera real —`/tmp`, un directorio de build— y el listado de un
    /// provider la resuelve al revés: `norte-vfs-local::list_with` omite la
    /// entrada que se esfumó para no matar el listado de un directorio vivo.
    /// Aquí no se puede omitir: la pareja ya está emparejada, y callarla
    /// quitaría del panel una fila que el otro lado sí tiene. Este test fija
    /// esa decisión para que cambiarla cueste discutirlo.
    #[tokio::test]
    async fn un_fichero_que_desaparece_entre_el_list_y_el_stat_sale_como_error() {
        let l = LazyProvider::failing(tree(&["a.txt"]).await, norte_proto::Error::NotFound);
        let r = LazyProvider::new(tree(&["a.txt"]).await);

        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::Error, "{rows:#?}");
        assert_eq!(rows[0].reason, Some(CompareReason::Unreadable));
        assert_eq!(rows[0].side, Some(Side::Left));
    }

    /// La fila de error lleva lo que el lado que SÍ contestó dijo.
    ///
    /// La derecha falla, la izquierda no: su tamaño se averiguó y es cierto, y
    /// el panel pinta esa celda. Vaciarla sería tirar una respuesta que se
    /// tuvo.
    #[tokio::test]
    async fn la_fila_de_un_stat_roto_conserva_el_lado_que_contesto() {
        let l = LazyProvider::new(tree(&["a.txt"]).await);
        let r = LazyProvider::failing(
            tree(&["a.txt"]).await,
            norte_proto::Error::Io { retryable: false },
        );

        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].side, Some(Side::Right));
        assert_eq!(
            rows[0].left.as_ref().expect("el lado que contestó").size,
            Some(b"a.txt".len() as u64),
            "lo que se llegó a saber viaja en la fila"
        );
        assert!(
            rows[0].right.as_ref().expect("el lado roto").size.is_none(),
            "y del que no contestó no se inventa nada"
        );
    }

    /// El corte entre rungs: con el tamaño ya sabido y distinto, la fecha no se
    /// pregunta.
    ///
    /// Solo se puede ver con un provider que rellene `size` y no `mtime_ms` —la
    /// forma de un SFTP cuyo servidor no manda `ACMODTIME`—: con uno perezoso
    /// del todo, el `stat` del rung de tamaño ya trae la fecha y el corte no
    /// ahorra nada medible.
    #[tokio::test]
    async fn un_tamano_que_ya_decide_no_paga_el_stat_de_la_fecha() {
        let l = MemProvider::new();
        let r = MemProvider::new();
        seed(&l, "a.txt", b"aa").await;
        seed(&r, "a.txt", b"aaaaaaaaaa").await;
        let l = LazyProvider::only_mtime_missing(l);
        let r = LazyProvider::only_mtime_missing(r);

        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(
            (rows[0].verdict, rows[0].criterion),
            (CompareVerdict::Different, CompareCriterion::Size),
            "{rows:#?}"
        );
        assert_eq!(
            (l.stats(), r.stats()),
            (0, 0),
            "el tamaño venía en el listado y ya decidió: la fecha no se pregunta"
        );

        // Y cuando el tamaño NO decide, la fecha sí se pregunta: un `stat` por
        // lado, no dos.
        let l = MemProvider::new();
        let r = MemProvider::new();
        seed(&l, "a.txt", b"aa").await;
        seed(&r, "a.txt", b"bb").await;
        let l = LazyProvider::only_mtime_missing(l);
        let r = LazyProvider::only_mtime_missing(r);
        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows[0].criterion, CompareCriterion::Mtime, "{rows:#?}");
        assert_eq!((l.stats(), r.stats()), (1, 1));
    }

    /// La hidratación mira el token ANTES de cada `stat` (regla dura 3).
    ///
    /// Se prueba sobre [`hydrate`] directamente, igual que el drenaje de un
    /// directorio se prueba sobre `list_all`: por el flujo haría falta cancelar
    /// a mitad de un `await`, que es una carrera.
    #[tokio::test]
    async fn la_hidratacion_mira_el_token_antes_de_preguntar() {
        let mem = LazyProvider::new(tree(&["a.txt"]).await);
        let entry = Entry {
            path: at("a.txt"),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
            attrs: BTreeMap::new(),
        };

        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut fresh = Fresh::of(&entry);
        let outcome = hydrate(
            &mem,
            &mut fresh,
            Side::Left,
            CompareCriterion::Size,
            &cancel,
        )
        .await;
        assert!(matches!(outcome, Err(HydrationFailure::Cancelled)));
        assert_eq!(mem.stats(), 0, "ni se preguntó");

        // Y sin cancelar, el mismo lado se hidrata una sola vez: el segundo
        // rung reutiliza el `stat` del primero.
        let mut fresh = Fresh::of(&entry);
        let vivo = CancellationToken::new();
        for rung in [CompareCriterion::Size, CompareCriterion::Mtime] {
            hydrate(&mem, &mut fresh, Side::Left, rung, &vivo)
                .await
                .expect("hidrata");
        }
        assert_eq!(mem.stats(), 1, "un `stat` por lado y por pareja");
        assert_eq!(fresh.entry.size, Some(b"a.txt".len() as u64));
        assert!(fresh.entry.mtime_ms.is_some());
    }

    // ---------- el provider que de verdad no puede contestar ----------

    /// Un tar REAL, indexado por el provider archive.
    ///
    /// El contenedor vive en un `MemProvider` porque lo que se está probando es
    /// el archivo, no el filesystem que lo guarda.
    async fn tar_provider(bytes: &[u8]) -> (ArchiveProvider, VPath) {
        let mem = Arc::new(MemProvider::new());
        let container = MemProvider::root().join(Segment::new(b"f.tar".to_vec()).expect("seg"));
        let mut sink = mem.write(&container).await.expect("write");
        sink.write(Bytes::copy_from_slice(bytes))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
        let root = VPath::archive_compose("tar", &container, &[]).expect("compose");
        let provider = ArchiveProvider::new(mem as Arc<dyn Provider>, Format::Tar, "tar+mem");
        (provider, root)
    }

    /// `Unknown` exigido a un provider que de VERDAD no puede contestar, no a
    /// un mock al que se le dice qué decir.
    ///
    /// El tar lleva un enlace cuyo header no trae destino —los escribe
    /// cualquier productor descuidado, y el provider archive ya tiene su camino
    /// para eso: `read_link` contesta `Corrupt`—. Sin los dos destinos no hay
    /// comparación posible, y la respuesta honesta es `Same`/`LinkTarget`/
    /// `Unknown`: inventarse una diferencia sería tan falso como inventarse una
    /// igualdad, y llamarlo error sería decir que la comparación falló cuando
    /// lo que pasa es que no se sabe.
    ///
    /// El OTRO lado lista PEREZOSO (como `norte-vfs-local`, #52) a propósito.
    /// Con un lado que rellena los campos, este test pasaba también con el bug
    /// de C7b encima: un `Unknown` producido porque nadie hidrató el tamaño se
    /// ve igual que uno producido por un enlace sin destino. Con un lado
    /// perezoso ya no: la pareja de ficheros tiene que salir `Certain`, así que
    /// el `Unknown` del enlace solo puede venir de lo que de verdad no se sabe.
    #[tokio::test]
    async fn a_real_archive_produces_unknown_rather_than_a_guess() {
        let bytes = TarSmith::new()
            .file(b"a.txt", b"hola")
            .symlink(b"link", b"")
            .build();
        let (zip, zip_root) = tar_provider(&bytes).await;

        let mem = MemProvider::new();
        seed(&mem, "a.txt", b"hola mundo").await;
        mem.symlink(
            &MemProvider::root().join(Segment::new(b"link".to_vec()).expect("seg")),
            b"../x",
            norte_vfs::SymlinkKind::File,
        )
        .await
        .expect("symlink");
        let local = LazyProvider::new(mem);

        let stream = compare(
            &local,
            &MemProvider::root(),
            &zip,
            &zip_root,
            CompareOptions::cheap(),
            CancellationToken::new(),
        );
        let rows = collect(stream).await;

        let row = rows
            .iter()
            .find(|row| named(row, b"link"))
            .expect("the paired row");
        assert_eq!(row.confidence, CompareConfidence::Unknown, "{rows:#?}");
        assert_eq!(row.criterion, CompareCriterion::LinkTarget);
        assert_ne!(
            row.verdict,
            CompareVerdict::Error,
            "unknown is an answer, not a failure"
        );
        assert!(
            row.left
                .as_ref()
                .expect("el enlace de este lado")
                .size
                .is_none(),
            "al enlace no se le gastó un `stat`: lo decide su destino"
        );

        // Y lo que el archivo SÍ sabe contestar se contesta CON CERTEZA: 10
        // bytes contra 4, con el tamaño de la izquierda hidratado a mano.
        let fichero = rows
            .iter()
            .find(|row| named(row, b"a.txt"))
            .expect("la pareja normal");
        assert_eq!(
            (fichero.verdict, fichero.criterion, fichero.confidence),
            (
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain
            ),
            "{rows:#?}"
        );
        assert_eq!(
            fichero.left.as_ref().expect("el fichero de este lado").size,
            Some(10)
        );
        assert_eq!(local.stats(), 1, "solo la pareja de ficheros se statea");
    }
}
