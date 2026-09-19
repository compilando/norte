//! «Ir a cualquier sitio» en la TUI (fase 6 del programa WOW): de dónde
//! salen sus filas.
//!
//! El modelo —secciones, orden, filtrado, cursor—, cómo se construye cada
//! clase de fila y qué significa confirmarla son de [`norte_frontend::goto`],
//! compartidos con la ventana (#357). Aquí vive lo que sólo esta terminal
//! sabe: qué listas tiene en memoria y con qué codificación se pintan sus
//! rutas.
//!
//! Las filas se toman como una FOTO al abrir, igual que la paleta con las
//! suyas, salvo la ruta tecleada (que es la consulta) y el índice semántico
//! (que llega cuando el core contesta). Una lista que cambia bajo el cursor
//! mientras el lector la lee es cómo un Enter acaba en otro sitio.

use norte_frontend::goto::{
    FixedSource, Goto, GotoRow, GotoSource, RutaSource, SECCION_COMANDOS, SECCION_CONEXIONES,
    SECCION_FAVORITOS, SECCION_HISTORIA, SECCION_INDICE, SECCION_POPULARES, TRAIDAS_POR_LISTA,
    fila_conexion, fila_ruta, filas_de_comandos,
};
use norte_i18n::t;

use crate::app::App;

pub use norte_frontend::goto::{Accion, MINIMO_PARA_EL_INDICE};

/// Las fuentes SÍNCRONAS: las listas que la TUI ya tiene en memoria.
///
/// `conexiones` llega aparte porque leerlas es tocar disco y eso lo hace el
/// llamante, que es asíncrono; pasarlas vacías es lo correcto cuando no se
/// pudieron leer — una sección menos, no una pantalla que no abre.
#[must_use]
pub fn fuentes(app: &App, conexiones: &[(String, String)]) -> Vec<Box<dyn GotoSource + Send>> {
    let foco = app.focus();
    let enc = app.focused().name_encoding();
    let actual = app.focused().dir().clone();
    let mut out: Vec<Box<dyn GotoSource + Send>> = Vec::new();

    // La ruta tecleada. Su detalle dice qué va a pasar, porque la fila es
    // la consulta y sin eso parecería que no ha entendido lo que escribes.
    out.push(Box::new(RutaSource::new(t("goto-path-desc"))));

    // Historia del panel con el foco: sin filtro (filtra el modelo) y sin
    // el directorio actual, al que no se quiere ir. Es la ÚNICA sección que
    // lleva la reinterpretación del panel, porque es la única cuyas rutas
    // son de ese panel.
    let historia: Vec<GotoRow> =
        norte_frontend::history::history_rows(&app.history[foco], &actual, "", enc)
            .into_iter()
            .filter(|r| r.mark != norte_frontend::history::HistoryMark::Current)
            .take(TRAIDAS_POR_LISTA)
            .map(|r| fila_ruta(SECCION_HISTORIA.id, None, &r.path, enc))
            .collect();
    out.push(Box::new(FixedSource::new(SECCION_HISTORIA, historia)));

    let populares: Vec<GotoRow> = norte_frontend::history::popular_rows(&app.popular, &actual, "")
        .into_iter()
        .take(TRAIDAS_POR_LISTA)
        .map(|r| fila_ruta(SECCION_POPULARES.id, None, &r.path, None))
        .collect();
    out.push(Box::new(FixedSource::new(SECCION_POPULARES, populares)));

    // Un favorito cuyo destino no parsea NO se ofrece: la lista de sitios
    // ya lo dice con su error, y aquí una fila que no puede llevar a
    // ninguna parte es sólo una forma de gastar un Enter.
    let favoritos: Vec<GotoRow> = app
        .hotlist
        .iter()
        .filter_map(|it| {
            it.target
                .as_ref()
                .ok()
                .map(|p| fila_ruta(SECCION_FAVORITOS.id, Some(&it.name), p, None))
        })
        .collect();
    out.push(Box::new(FixedSource::new(SECCION_FAVORITOS, favoritos)));

    let conexiones: Vec<GotoRow> = conexiones
        .iter()
        .map(|(name, url)| fila_conexion(name, url))
        .collect();
    out.push(Box::new(FixedSource::new(SECCION_CONEXIONES, conexiones)));

    // Los comandos, los MISMOS que la paleta ofrece en este contexto: la
    // paleta ya resuelve qué se puede correr con el visor abierto.
    let comandos = filas_de_comandos(crate::palette::rows_for_context(
        &app.palette_rows,
        app.viewer.is_some(),
    ));
    out.push(Box::new(
        FixedSource::new(SECCION_COMANDOS, comandos).solo_con_consulta(),
    ));

    out
}

/// Mete en `goto` lo que contestó el índice.
pub fn poner_indice(app: &mut App, hits: &[norte_proto::methods::SemanticHit]) {
    let filas = norte_frontend::goto::filas_del_indice(hits);
    if let Some(goto) = &mut app.goto {
        // `ya_filtrada`: el índice casó por SIGNIFICADO, y volver a pasarle
        // la subsecuencia de la consulta tiraría justo lo que lo hace útil.
        goto.reemplazar_seccion(SECCION_INDICE, filas, true);
    }
}

/// Qué hacer con la fila que el lector acaba de confirmar — la decisión es
/// del modelo compartido ([`norte_frontend::goto::accion`]).
#[must_use]
pub fn accion(_app: &App, key: &str) -> Accion {
    norte_frontend::goto::accion(key)
}

/// Abre la pantalla con las fuentes dadas.
pub fn abrir(app: &mut App, conexiones: &[(String, String)]) {
    let fuentes = fuentes(app, conexiones);
    app.goto = Some(Goto::new(fuentes));
}
