//! La pregunta al índice semántico que alimenta la sección del mismo
//! nombre en «ir a cualquier sitio» (fase 6).
//!
//! Va spawneada y no en línea por lo de siempre: detrás del índice hay un
//! modelo de embeddings, y a veces un proveedor remoto. Pedirla dentro de
//! la tecla dejaría la pantalla clavada entre letra y letra.
//!
//! Es la HERMANA de [`crate::jobs::ai::harvest_semantic`] y no la misma: la
//! búsqueda de `ai.search` abre un MODAL con sus resultados, y ésta rellena
//! una sección de una pantalla que sigue abierta. Comparten el cinturón de
//! ingestión (`validate_semantic_hits`), que es lo que de verdad importaba
//! compartir.

use norte_core::backend::Backend;
use norte_frontend::goto::SECCION_INDICE;
use norte_proto::Error;

use crate::app::App;
use crate::goto::MINIMO_PARA_EL_INDICE;
use crate::jobs::{GotoIndexRun, InFlight};

/// Cuántos resultados se piden: el mismo número en los dos frontends.
const TOPE: u32 = norte_frontend::goto::TOPE_DEL_INDICE;

/// Pregunta al índice por lo que hay escrito ahora mismo, si vale la pena.
///
/// Relanzar ABORTA la petición anterior, que es el mismo contrato de
/// cancelación del resto de peticiones largas: escribir deprisa no deja
/// tres preguntas vivas, y la respuesta a una consulta que ya no está
/// escrita no le sirve a nadie.
///
/// Por debajo de [`MINIMO_PARA_EL_INDICE`] no se pregunta Y se VACÍA la
/// sección: dejar ahí lo que contestó a una consulta más larga es enseñar
/// una respuesta a una pregunta que ya no se hizo.
pub fn pedir_al_indice(app: &mut App, backend: &Backend, work: &mut InFlight) {
    let Some(goto) = &mut app.goto else {
        olvidar(work);
        return;
    };
    let q = goto.query().to_owned();
    // Una RUTA tecleada tampoco: no es una consulta semántica, y mandarla a un
    // proveedor de embeddings —quizá remoto— es mandarle el nombre de un
    // directorio del lector.
    if q.chars().count() < MINIMO_PARA_EL_INDICE || norte_frontend::goto::parece_ruta(&q).is_some()
    {
        olvidar(work);
        goto.reemplazar_seccion(SECCION_INDICE, Vec::new(), true);
        return;
    }
    let b = backend.clone();
    let consulta = q.clone();
    // Sin raíz: contra TODO lo indexado, como la búsqueda semántica de
    // `ai.search`. «Ir a cualquier sitio» es literalmente eso, y acotarlo al
    // panel con el foco haría que la misma consulta diera cosas distintas
    // según dónde estuvieras.
    let handle = tokio::spawn(async move { b.index_search_semantic(None, &consulta, TOPE).await });
    if let Some(vieja) = work.goto_index.replace(GotoIndexRun { handle, query: q }) {
        vieja.handle.abort();
    }
}

/// Abandona la petición en vuelo, si la hay.
///
/// Se llama al cerrar la pantalla y al confirmar una fila: lo que venga
/// detrás manda, y una respuesta tardía ya no tiene dónde caer.
pub fn olvidar(work: &mut InFlight) {
    if let Some(vieja) = work.goto_index.take() {
        vieja.handle.abort();
    }
}

/// Mete la respuesta del índice en su sección.
///
/// Tres cosas que NO hace, y cada una es un fallo que se ha visto en este
/// repositorio:
///
/// - No abre nada ni escribe en la barra. Es una sección de una pantalla
///   que el lector está mirando; un mensaje por cada respuesta taparía lo
///   que está leyendo, y un índice apagado no es un error que anunciar
///   —`Unsupported` es la respuesta normal de quien no lo tiene—.
/// - No se fía del tamaño de la respuesta: pasa por el MISMO
///   `validate_semantic_hits` que la búsqueda de `ai.search`, porque un
///   daemon hostil puede contestar lo que quiera y el tope contractual es
///   del cliente.
/// - No toca nada si la pantalla ya se cerró, ni si lo escrito cambió
///   mientras el índice pensaba: la respuesta es a OTRA consulta, y ponerla
///   sería enseñar resultados de algo que el lector ya no tiene escrito.
pub fn harvest_goto_index(
    app: &mut App,
    work: &mut InFlight,
    res: Result<Result<Vec<norte_proto::methods::SemanticHit>, Error>, tokio::task::JoinError>,
) {
    let pedida = work.goto_index.take().map(|r| r.query);
    let Ok(Ok(hits)) = res else {
        return;
    };
    let Some(pedida) = pedida else { return };
    let Some(goto) = &app.goto else { return };
    if goto.query() != pedida {
        return;
    }
    let Some(hits) = norte_frontend::validate_semantic_hits(hits) else {
        return;
    };
    crate::goto::poner_indice(app, &hits);
}
