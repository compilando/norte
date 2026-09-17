//! «Ir a cualquier sitio» en la TUI (fase 6 del programa WOW): de dónde
//! salen sus filas y qué pasa al confirmar una.
//!
//! El modelo —secciones, orden, filtrado, cursor— es de
//! [`norte_frontend::goto`], compartido con la ventana. Aquí vive lo que
//! sólo esta terminal sabe: qué listas tiene en memoria, con qué
//! codificación se pintan sus rutas, y qué significa confirmar.
//!
//! Las filas se toman como una FOTO al abrir, igual que la paleta con las
//! suyas, salvo la ruta tecleada (que es la consulta) y el índice semántico
//! (que llega cuando el core contesta). Una lista que cambia bajo el cursor
//! mientras el lector la lee es cómo un Enter acaba en otro sitio.

use norte_frontend::goto::{
    FixedSource, Goto, GotoRow, GotoSource, RutaSource, SECCION_COMANDOS, SECCION_CONEXIONES,
    SECCION_FAVORITOS, SECCION_HISTORIA, SECCION_INDICE, SECCION_POPULARES,
};
use norte_i18n::t;
use norte_proto::VPath;

use crate::app::App;

/// Cuántas filas se traen de cada lista larga ANTES de filtrar.
///
/// No es el tope de lo que se ve —ése es
/// [`norte_frontend::goto::TOPE_POR_SECCION`], y lo aplica el modelo a
/// todas las secciones por igual— sino cuántas entradas de una lista de
/// cientos se le ofrecen al filtro. Más holgado que el de pintado a
/// propósito: filtrar sobre cuarenta encuentra cosas que filtrar sobre doce
/// no, y las que sobren las recorta el modelo después.
const TRAIDAS_POR_LISTA: usize = 40;

/// A partir de cuántos caracteres se le pregunta al índice.
///
/// Con menos, la respuesta no puede ser buena —una o dos letras no son una
/// consulta semántica— y cada pregunta es una llamada a un proveedor que
/// cuesta tiempo y puede costar dinero.
pub const MINIMO_PARA_EL_INDICE: usize = 3;

/// Prefijo de despacho de una fila que lleva a un `VPath` ya parseado.
const K_IR: &str = "go:";
/// Prefijo de una fila que lleva a un comando del catálogo.
const K_CMD: &str = "cmd:";
/// Prefijo de la fila de la RUTA TECLEADA, que aún hay que resolver.
/// Lo pone [`norte_frontend::goto::RutaSource`].
const K_RUTA: &str = "path:";

/// Lo que significa confirmar una fila.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accion {
    /// Navegar el panel con el foco a este directorio.
    Ir(VPath),
    /// Correr este comando del catálogo, como si se hubiera pulsado su
    /// tecla.
    Comando(String),
    /// La fila no lleva a ningún sitio que se pueda resolver — una ruta
    /// tecleada que no parsea, sobre todo. Trae la clave del mensaje.
    Nada(&'static str),
}

/// Una fila hacia un directorio conocido.
///
/// `enc` es la reinterpretación de nombres del panel con el foco, y sólo se
/// le pasa a las rutas que son DE ese panel —su historia—. A las demás
/// (populares, favoritos, conexiones, índice) se les pasa `None`: son de
/// toda la sesión, y aplicarles el encoding de un panel a rutas de otro
/// inventa mojibake. Es la misma regla que ya escribió `popular_rows` en su
/// sitio, y el motivo de que esto sea un parámetro y no `app`.
fn fila_ruta(
    section: &'static str,
    nombre: Option<&str>,
    path: &VPath,
    enc: Option<norte_encoding::NameEncoding>,
) -> GotoRow {
    let (texto, hostil) = norte_frontend::path_display_with(path, enc);
    // El nombre que puso el lector (un favorito, una conexión) va DELANTE y
    // la ruta detrás: se busca por el nombre que uno mismo eligió, y la
    // ruta es lo que confirma que es la que se cree.
    let (text, desc, hostile) = match nombre {
        Some(n) => {
            let (nt, nh) = crate::app::display_name(n.as_bytes());
            (nt, texto, nh || hostil)
        }
        None => (texto, String::new(), hostil),
    };
    GotoRow {
        section,
        key: format!("{K_IR}{}", path.to_wire()),
        text,
        desc,
        hostile,
    }
}

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

    // Una conexión llega como la URL CRUDA de `connections.toml`, que es lo
    // que el selector de #140 maneja, y puede no parsear. Se ofrece igual y
    // se dice al confirmar —lo mismo que hace el selector, con el mismo
    // mensaje— en vez de desaparecer: «mi conexión no sale en ir a» es peor
    // que un error al pulsar Enter, porque no tiene ni dónde mirar.
    let conexiones: Vec<GotoRow> = conexiones
        .iter()
        .map(|(name, url)| {
            let (texto, hostil) = VPath::parse(url).map_or_else(
                |_| (norte_encoding::mask_terminal_hazards(url), true),
                |p| norte_frontend::path_display_with(&p, None),
            );
            let (nt, nh) = crate::app::display_name(name.as_bytes());
            GotoRow {
                section: SECCION_CONEXIONES.id,
                key: format!("{K_IR}{url}"),
                text: nt,
                desc: texto,
                hostile: nh || hostil,
            }
        })
        .collect();
    out.push(Box::new(FixedSource::new(SECCION_CONEXIONES, conexiones)));

    // Los comandos, los MISMOS que la paleta ofrece en este contexto: dos
    // listas de comandos que se calculan por separado divergen, y la
    // paleta ya resuelve qué se puede correr con el visor abierto.
    let comandos: Vec<GotoRow> =
        crate::palette::rows_for_context(&app.palette_rows, app.viewer.is_some())
            .into_iter()
            .map(|r| GotoRow {
                section: SECCION_COMANDOS.id,
                key: format!("{K_CMD}{}", r.key),
                text: r.text,
                desc: r.desc,
                hostile: r.hostile,
            })
            .collect();
    out.push(Box::new(
        FixedSource::new(SECCION_COMANDOS, comandos).solo_con_consulta(),
    ));

    out
}

/// Las filas de una tanda de resultados del índice semántico.
///
/// Son RUTAS de ficheros que el core encontró, así que su nombre son bytes
/// de disco y se pintan por el mismo camino enmascarado que las demás.
#[must_use]
pub fn filas_del_indice(hits: &[norte_proto::methods::SemanticHit]) -> Vec<GotoRow> {
    hits.iter()
        // Sin reinterpretación, como los populares: lo que devuelve el
        // índice puede estar en cualquier sitio, no en el panel con el foco.
        .map(|h| fila_ruta(SECCION_INDICE.id, None, &h.path, None))
        .collect()
}

/// Mete en `goto` lo que contestó el índice.
pub fn poner_indice(app: &mut App, hits: &[norte_proto::methods::SemanticHit]) {
    let filas = filas_del_indice(hits);
    if let Some(goto) = &mut app.goto {
        // `ya_filtrada`: el índice casó por SIGNIFICADO, y volver a pasarle
        // la subsecuencia de la consulta tiraría justo lo que lo hace útil.
        goto.reemplazar_seccion(SECCION_INDICE, filas, true);
    }
}

/// Qué hacer con la fila que el lector acaba de confirmar.
///
/// La `key` NUNCA se pinta y aquí es donde se lee: el prefijo dice de qué
/// clase es la fila, y cada clase se resuelve por su camino. Una ruta
/// TECLEADA es la única que puede no resolver, porque es lo único que no
/// salió de una lista que ya existía.
#[must_use]
pub fn accion(app: &App, key: &str) -> Accion {
    if let Some(cmd) = key.strip_prefix(K_CMD) {
        return Accion::Comando(cmd.to_owned());
    }
    if let Some(wire) = key.strip_prefix(K_IR) {
        return VPath::parse(wire).map_or(Accion::Nada("msg-goto-bad-path"), Accion::Ir);
    }
    if let Some(texto) = key.strip_prefix(K_RUTA) {
        return resolver_tecleada(app, texto);
    }
    Accion::Nada("msg-goto-bad-path")
}

/// Resuelve la ruta que el lector tecleó.
///
/// `~` se expande contra el HOME de este proceso, no contra el directorio
/// del panel: «la casa» es una sola, y hacerla depender de dónde estabas
/// sería que la misma tecla lleve a dos sitios. Una ruta absoluta se toma
/// como local, y una con esquema se parsea tal cual — si el backend no
/// existe, el `VPath` no parsea y se dice, que es mejor que navegar a algo
/// que no es lo que se escribió.
fn resolver_tecleada(app: &App, texto: &str) -> Accion {
    let _ = app;
    let expandido = if texto == "~" || texto.starts_with("~/") {
        let Some(home) = std::env::var_os("HOME") else {
            return Accion::Nada("msg-goto-no-home");
        };
        let mut p = std::path::PathBuf::from(home);
        if let Some(resto) = texto.strip_prefix("~/") {
            p.push(resto);
        }
        p
    } else if texto.starts_with('/') {
        std::path::PathBuf::from(texto)
    } else {
        // Con esquema: el wire ya es un wire.
        return VPath::parse(texto).map_or(Accion::Nada("msg-goto-bad-path"), Accion::Ir);
    };
    norte_vfs_local::vpath_from_native(&expandido)
        .map_or(Accion::Nada("msg-goto-bad-path"), Accion::Ir)
}

/// Abre la pantalla con las fuentes dadas.
pub fn abrir(app: &mut App, conexiones: &[(String, String)]) {
    let fuentes = fuentes(app, conexiones);
    app.goto = Some(Goto::new(fuentes));
}

#[cfg(test)]
mod tests {
    use super::{Accion, K_CMD, K_IR, K_RUTA, accion};
    use crate::app::{App, Pane};
    use norte_vfs::VPath;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("wire de test");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// Cada prefijo de `key` va por su camino, y ninguno se confunde con
    /// otro: la `key` no se pinta nunca, así que esto es lo único que
    /// decide a dónde lleva un Enter.
    #[test]
    fn cada_prefijo_resuelve_a_lo_suyo() {
        let a = app();
        assert_eq!(
            accion(&a, &format!("{K_CMD}app.quit")),
            Accion::Comando("app.quit".to_owned())
        );
        assert_eq!(
            accion(&a, &format!("{K_IR}file:///tmp")),
            Accion::Ir(VPath::parse("file:///tmp").expect("wire"))
        );
        assert_eq!(
            accion(&a, &format!("{K_RUTA}/tmp")),
            Accion::Ir(VPath::parse("file:///tmp").expect("wire"))
        );
    }

    /// Una ruta con esquema tecleada se parsea tal cual; una que NO parsea
    /// se dice, en vez de navegar a cualquier otra cosa.
    ///
    /// Un esquema desconocido SÍ parsea —`VPath` no tiene la lista de
    /// backends, y no debe tenerla— y se navega: quien dice que no hay
    /// quien sirva `noexiste://` es el core, con su error, igual que con
    /// cualquier otra URL. Lo que no pasa de aquí es un wire roto: un
    /// segmento vacío es el caso que `VPath::parse` rechaza.
    #[test]
    fn una_ruta_tecleada_que_no_parsea_se_dice() {
        let a = app();
        assert!(
            matches!(accion(&a, &format!("{K_RUTA}sftp://h//x")), Accion::Nada(_)),
            "un segmento vacío no es una ruta"
        );
        assert!(
            matches!(accion(&a, "otra cosa"), Accion::Nada(_)),
            "y una key sin prefijo conocido no lleva a ningún sitio"
        );
        assert!(
            matches!(
                accion(&a, &format!("{K_RUTA}noexiste://h/x")),
                Accion::Ir(_)
            ),
            "un esquema que este build no sirve lo rechaza el core, no esto"
        );
    }

    /// `~` se expande contra el HOME del proceso, no contra el panel: la
    /// casa es una sola.
    #[test]
    fn la_casa_no_depende_del_panel() {
        let a = app();
        let Accion::Ir(p) = accion(&a, &format!("{K_RUTA}~")) else {
            panic!("`~` tiene que resolver mientras haya HOME");
        };
        let home = std::env::var("HOME").expect("HOME en el entorno de test");
        assert_eq!(
            p,
            norte_vfs_local::vpath_from_native(std::path::Path::new(&home))
                .expect("el HOME es una ruta")
        );
    }
}
