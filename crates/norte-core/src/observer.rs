//! Costura del journal (M3): TODA mutación que ejecuta el core pasa por un
//! [`MutationObserver`] ANTES de considerarse completa (regla dura 4). En M0
//! el observador es no-op; el journal se enchufará aquí sin tocar el engine.

use std::sync::Arc;

use async_trait::async_trait;
use norte_proto::{Error, VPath};

use crate::journal::Actor;

/// Una mutación observable del VFS.
#[derive(Debug)]
pub enum Mutation<'a> {
    /// Nodo creado (archivo commiteado o dir).
    Created {
        /// Dónde se creó.
        path: &'a VPath,
        /// QUÉ se creó: la identidad del nodo, si el backend la sabe dar
        /// (#369, ADR 0152).
        ///
        /// La reversa de un `created` es un borrado, y sin esto borra lo que
        /// haya en esa ruta AHORA — que no tiene por qué ser lo que se creó.
        /// Con ella, el undo compara y se niega cuando no cuadra.
        ///
        /// `None` = no se pudo saber (un provider sin identidad estable, o un
        /// `stat` que falló). Entonces el undo hace lo de siempre: esto solo
        /// puede hacerle negarse MÁS, nunca menos.
        node: Option<norte_vfs::NodeId>,
    },
    /// Nodo eliminado PERMANENTEMENTE (irreversible).
    Removed(&'a VPath),
    /// Nodo movido a la papelera (RECUPERABLE — el undo de M3 lo restaura;
    /// régimen distinto a `Removed`, ADR 0009).
    Trashed {
        /// Path original (víctima).
        path: &'a VPath,
        /// Destino recuperable en una papelera LÓGICA (`.norte-trash/<id>`,
        /// fase 9) → `reversal_ref`. `None` si es papelera NATIVA del OS o
        /// "vanish" (sin ruta estable; el handle se resuelve en el undo M3-2).
        dest: Option<&'a VPath>,
    },
    /// Nodo renombrado dentro de un provider.
    Renamed {
        /// Path original.
        from: &'a VPath,
        /// Path nuevo.
        to: &'a VPath,
        /// Lote al que pertenece el rename (`fs.rename_batch`): la etiqueta que
        /// agrupa n entradas del journal para deshacerlas juntas. `None` para
        /// un rename suelto — que es todo lo que hay fuera del ejecutor de
        /// lotes.
        ///
        /// El lote vive en ESTA variante y no en el contexto de la task porque
        /// el ejecutor de lotes solo emite renames.
        ///
        /// OBLIGACIÓN DEL EJECUTOR: cada paso llama a `Provider::rename`
        /// DIRECTAMENTE. Si en su lugar pasara por el camino de move con
        /// política de colisión, una sobrescritura emitiría un
        /// [`Mutation::Removed`] —clasificado `Irreversible`— que se quedaría
        /// FUERA del grupo: un borrado permanente dentro de una operación que
        /// el wire anuncia como una unidad deshacible, y que un undo por lote
        /// ni siquiera vería para bloquearse. El planificador ya rechaza el
        /// plan entero ante cualquier colisión, así que el ejecutor nunca tiene
        /// motivo para sobrescribir nada.
        batch: Option<i64>,
    },
    /// Permisos POSIX cambiados (#314).
    ModeChanged {
        /// El nodo cuyo modo cambió.
        path: &'a VPath,
        /// El modo que TENÍA, leído antes de escribir el nuevo. Es la reversa
        /// entera: sin él no hay undo que ofrecer.
        ///
        /// `None` cuando no se pudo leer —un provider que no publica
        /// `posix.mode`, o un `stat` que falló—, y entonces la entrada se
        /// clasifica `Irreversible` con ese motivo (regla 4). Prometer un undo
        /// que devolvería un modo inventado es peor que no ofrecer ninguno.
        from: Option<u32>,
        /// El modo que se puso.
        to: u32,
        /// Lote al que pertenece este cambio (#315): la etiqueta que agrupa
        /// los n nodos de UN `fs.set_mode` recursivo. `None` para un cambio
        /// suelto — que es todo lo que hay sin recursión.
        ///
        /// Existe por lo mismo que en [`Self::Renamed`]: sin ella, un chmod
        /// sobre un árbol de cien mil ficheros deja cien mil entradas que
        /// nadie puede volver a juntar, y una auditoría que las lea vería cien
        /// mil acciones donde el humano hizo una. El undo funciona igual —es
        /// LIFO y cada entrada lleva su reversa—; lo que el lote compra es
        /// poder DECIR que fueron una.
        batch: Option<i64>,
    },
}

impl<'a> Mutation<'a> {
    /// Un `created` del que no se sabe la identidad del nodo (ADR 0152).
    ///
    /// Es lo correcto para quien crea algo y no tiene barato preguntar QUÉ
    /// creó —y lo honesto: el undo hará lo de siempre—. Quien sí puede
    /// preguntarlo construye la variante con su `node`, que es lo que hace la
    /// copia.
    #[must_use]
    pub fn creado(path: &'a VPath) -> Self {
        Self::Created { path, node: None }
    }
}

/// Receptor de mutaciones. M3 lo implementa el journal (con undo); hasta
/// entonces, un observador no-op interno.
#[async_trait]
pub trait MutationObserver: Send + Sync {
    /// Notifica una mutación ya aplicada con éxito. Async y falible: el journal
    /// await-ea el insert antes de que la op se considere completa, y su fallo
    /// PROPAGA (regla 4 — la op falla si su entrada no quedó durable).
    ///
    /// # Errors
    /// El error del sink (p. ej. fallo de escritura del journal).
    async fn on_mutation(&self, mutation: &Mutation<'_>, actor: &Actor) -> Result<(), Error>;

    /// ¿Merece la pena averiguar la identidad de lo que se crea (ADR 0152)?
    ///
    /// Saberla cuesta un `stat` por nodo creado, y contra un destino remoto
    /// eso es un viaje de red. Quien no va a guardar la mutación tampoco va a
    /// usar la identidad, así que puede decir que no y ahorrárselo entero.
    ///
    /// Es una PISTA de coste, no una garantía: contestar `true` no obliga al
    /// llamante a traerla —puede fallar y llegar `None` igualmente—, y
    /// contestar `false` solo promete que no la va a echar de menos. Por eso
    /// el default es `true`: un observer que guarde y no la conteste pierde la
    /// protección en silencio, que es la dirección cara del error.
    fn quiere_identidad(&self) -> bool {
        true
    }

    /// El observer que UNA Task va a usar para TODAS sus mutaciones, decidido
    /// **una sola vez, antes del primer efecto**.
    ///
    /// `None` —el caso por omisión— significa «yo mismo sirvo»: un observer sin
    /// ventana que perder (el journal del daemon, ya abierto; un no-op) no tiene
    /// nada que fijar.
    ///
    /// # Por qué existe: media operación registrada es peor que ninguna (#205)
    ///
    /// El journal EMBEBIDO puede perderse y recuperarse a mitad de sesión
    /// (#179). Preguntándole por MUTACIÓN, una operación larga —un `copy_tree`
    /// que empieza con el fichero ocupado y dura más que el freno de
    /// reintento— empieza a registrar por el medio: las primeras k entradas sin
    /// fila, las n-k siguientes con ella, dentro de UNA Task y UN actor. Y
    /// entonces `undo_session` desanda la cola registrada y deja la cabeza que
    /// no lo está: media copia deshecha, sin nada que le diga al usuario cuál
    /// mitad — porque no hay filas que nombrar.
    ///
    /// «No quedó registrado» se arregla a mano; «quedó registrado a medias» es
    /// una trampa. Fijando el veredicto al principio de la Task, una operación
    /// queda entera dentro del journal o entera fuera, que es lo que era antes
    /// de que la ventana supiera reabrirse.
    ///
    /// **El handle fijado se sostiene durante toda la Task**, y eso es la otra
    /// mitad del contrato: mientras viva,
    /// [`crate::embedded::LazyJournal::release`] no puede soltar el fichero.
    /// Lo que eso protege es la ventana **fijado → última fila**, y conviene
    /// no confundirla con la que va del gate al fijado: el gate resuelve y
    /// SUELTA su `Arc`, y la Task puede esperar en la cola del scheduler un
    /// rato indefinido antes de fijar. Un temporizador de ociosidad que
    /// dispare ahí cierra la ventana sin romper nada —no hay filas que perder
    /// y el fijado la reabre— pero puede dejar la operación entera sin
    /// registrar si un tercero gana la reapertura. Quien escriba ese
    /// temporizador tiene que mirar también las Tasks despachadas y no
    /// empezadas.
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] si el journal existe y NO SE PUEDE ABRIR
    /// (#178). Es el mismo fail-closed que el gate del engine, repetido aquí
    /// porque el gate mira ANTES de encolar y esto mira al empezar de verdad:
    /// entre los dos caben treinta segundos de cola, y en ese hueco un fichero
    /// puede pasar de sano a corrupto. No propagarlo dejaría la Task mutando
    /// un árbol entero en silencio, que es exactamente lo que #178 rehúsa —
    /// y aquí todavía no ha ocurrido ningún efecto, así que rehusar es gratis.
    async fn pin_for_task(&self) -> Result<Option<Arc<dyn MutationObserver>>, Error> {
        Ok(None)
    }
}

/// El observer de ESTA Task, fijado de una vez (ver
/// [`MutationObserver::pin_for_task`]).
///
/// Se llama al PRINCIPIO del cuerpo de cada Task que muta, antes de cualquier
/// efecto. Que la decisión se tome aquí y no en cada `on_mutation` es lo que
/// hace que la Task quede entera dentro o entera fuera del journal.
/// # Errors
/// Las de [`MutationObserver::pin_for_task`]: el journal existe y no se puede
/// abrir. Va ANTES del primer efecto, así que la Task muere sin haber tocado
/// nada.
pub(crate) async fn pin_for_task(
    observer: Arc<dyn MutationObserver>,
) -> Result<Arc<dyn MutationObserver>, Error> {
    Ok(match observer.pin_for_task().await? {
        Some(fijado) => fijado,
        None => observer,
    })
}

/// Observador que no hace nada (M0 / tests sin journal).
pub(crate) struct NoopObserver;

#[async_trait]
impl MutationObserver for NoopObserver {
    async fn on_mutation(&self, _mutation: &Mutation<'_>, _actor: &Actor) -> Result<(), Error> {
        Ok(())
    }

    fn quiere_identidad(&self) -> bool {
        false
    }
}
