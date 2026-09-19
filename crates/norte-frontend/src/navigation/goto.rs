//! «Ir a cualquier sitio» (fase 6 del programa WOW): una sola pantalla que
//! junta lo que hasta ahora estaba en cinco — la paleta de comandos, la
//! historia, los populares, los favoritos y las conexiones — y le añade lo
//! que no tenía sitio: una ruta TECLEADA y, cuando llega, lo que el índice
//! semántico encontró.
//!
//! Lo que este módulo aporta y lo que deja fuera, a propósito:
//!
//! - Aporta el MODELO: qué es una fila, en qué sección va, en qué orden van
//!   las secciones, cómo se filtra y por dónde anda el cursor. Todo puro y
//!   con tests.
//! - No aporta los DATOS. Cada fuente los trae, porque cada una sabe cosas
//!   que este módulo no puede saber: de dónde vienen los bytes de un nombre
//!   y con qué codificación se pintan, si el texto es del proyecto o de un
//!   tercero, y si hace falta enmascararlo. Una fila llega con su texto ya
//!   pintable y su bandera [`GotoRow::hostile`] ya puesta — el mismo
//!   criterio que [`crate::palette::Row`], y por la misma razón: enmascarar
//!   al pintar es enmascarar en cada frame y olvidarlo en uno.
//!
//! Una fuente nueva es implementar [`GotoSource`] y meterla en la lista. No
//! hay ningún sitio más que tocar: el filtrado, las cabeceras, el orden y el
//! cursor son de aquí.

use crate::palette_state::is_subsequence;

/// Una sección del «ir a»: un id estable y la clave Fluent de su título.
///
/// El id NO se pinta: es lo que empareja una fila con su cabecera y lo que
/// fija el orden. El título sí, y sale de Fluent como todo lo demás.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GotoSection {
    /// Id estable de la sección.
    pub id: &'static str,
    /// Clave Fluent del título que se pinta encima de sus filas.
    pub title_key: &'static str,
}

/// La ruta que el lector acaba de teclear. Va la PRIMERA porque es lo que
/// acaba de escribir: ninguna lista tiene prioridad sobre eso.
pub const SECCION_RUTA: GotoSection = GotoSection {
    id: "path",
    title_key: "goto-section-path",
};

/// Dónde ha estado este panel.
pub const SECCION_HISTORIA: GotoSection = GotoSection {
    id: "history",
    title_key: "goto-section-history",
};

/// Dónde vuelve más veces.
pub const SECCION_POPULARES: GotoSection = GotoSection {
    id: "popular",
    title_key: "goto-section-popular",
};

/// Los sitios que guardó a mano.
pub const SECCION_FAVORITOS: GotoSection = GotoSection {
    id: "favorites",
    title_key: "goto-section-favorites",
};

/// Las conexiones remotas configuradas.
pub const SECCION_CONEXIONES: GotoSection = GotoSection {
    id: "connections",
    title_key: "goto-section-connections",
};

/// Los comandos del catálogo — la paleta de siempre, aquí como una sección
/// más.
pub const SECCION_COMANDOS: GotoSection = GotoSection {
    id: "commands",
    title_key: "goto-section-commands",
};

/// Lo que encontró el índice semántico. Llega TARDE (es una pregunta al
/// core, no una lista en memoria) y por eso va la última: una sección que
/// aparece a media escritura no debe empujar hacia abajo lo que el lector
/// ya estaba mirando.
pub const SECCION_INDICE: GotoSection = GotoSection {
    id: "index",
    title_key: "goto-section-index",
};

/// El ORDEN en que se pintan las secciones, y el único sitio donde vive.
///
/// Fijo y no configurable: es el orden en que un lector busca —lo que acaba
/// de teclear, por dónde ha pasado, lo que guardó, lo que puede hacer— y
/// una lista que se reordena sola es una lista donde no se puede aprender
/// dónde está nada.
pub const ORDEN: &[GotoSection] = &[
    SECCION_RUTA,
    SECCION_HISTORIA,
    SECCION_POPULARES,
    SECCION_FAVORITOS,
    SECCION_CONEXIONES,
    SECCION_COMANDOS,
    SECCION_INDICE,
];

/// Una fila del «ir a».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GotoRow {
    /// Id de la sección a la que pertenece (uno de los de [`ORDEN`]).
    pub section: &'static str,
    /// Clave de DESPACHO, nunca pintada — el llamante la consume al
    /// confirmar. Mismo contrato que [`crate::palette::Row::key`]: para una
    /// ruta lleva el wire del `VPath`, para un comando su nombre.
    pub key: String,
    /// Lo que se pinta, YA pintable (enmascarado si hacía falta).
    pub text: String,
    /// La segunda línea, o vacío.
    pub desc: String,
    /// `text` se pinta distinto de lo que dicen los bytes de origen.
    ///
    /// Viaja con la fila y no se recalcula al pintar: una cadena
    /// enmascarada que viaja sin su bandera se lee como fiel, y esta es una
    /// pantalla donde se elige a dónde ir.
    pub hostile: bool,
}

/// Una fuente de filas del «ir a».
///
/// El contrato es corto a propósito: dar las filas que aporta para una
/// consulta. Lo demás —filtrar por subsecuencia, poner la cabecera, ordenar
/// las secciones, mover el cursor— es del modelo, para que una fuente nueva
/// no tenga que acertar con ninguna de esas cuatro cosas.
///
/// `query` llega por si la fuente sabe filtrar MEJOR que la subsecuencia
/// genérica (el índice semántico pregunta al core con ella, y la ruta
/// tecleada ES la consulta). Una fuente que no tenga nada especial que
/// hacer puede devolverlo todo: [`Goto::refrescar`] filtra después, y así
/// el filtrado es uno solo para todas.
pub trait GotoSource {
    /// La sección en la que caen sus filas.
    fn section(&self) -> GotoSection;
    /// Las filas que aporta para `query`.
    fn rows(&self, query: &str) -> Vec<GotoRow>;
    /// Si sus filas YA vienen filtradas y el modelo no debe volver a
    /// pasarles la subsecuencia.
    ///
    /// `false` por defecto, que es lo que quiere una lista en memoria. Lo
    /// pone a `true` el índice semántico: preguntó al core con la consulta
    /// entera y sus resultados casan por SIGNIFICADO, no por letras — un
    /// filtro de subsecuencia encima tiraría justo lo que lo hace útil.
    fn ya_filtrada(&self) -> bool {
        false
    }

    /// Si esta fuente sólo aporta filas cuando hay algo escrito.
    ///
    /// `false` por defecto. Lo pone a `true` la sección de COMANDOS: con la
    /// consulta vacía son cientos de filas que sepultan las cuatro listas de
    /// destinos, y quien abre «ir a» sin escribir nada está preguntando a
    /// dónde puede ir, no qué verbos existen. En cuanto teclea algo vuelven,
    /// y el catálogo entero sigue estando en la paleta, que es su pantalla.
    fn solo_con_consulta(&self) -> bool {
        false
    }
}

/// Tope de filas POR SECCIÓN.
///
/// Una sección más larga que esto no se lee: se hojea, y para hojear están
/// las pantallas propias de cada lista, que además dejan borrar entradas.
/// El tope vive aquí, en el modelo, y no en cada fuente, porque si viviera
/// en cada fuente la siguiente se olvidaría de ponérselo.
pub const TOPE_POR_SECCION: usize = 12;

/// Una fuente con las filas ya hechas: una foto de una lista que el
/// frontend ya tenía en memoria (historia, favoritos, comandos…).
///
/// La foto se toma al abrir, como hace la paleta con sus filas, y por lo
/// mismo: lo que se ve mientras la pantalla está abierta no debe cambiar
/// bajo el cursor.
pub struct FixedSource {
    section: GotoSection,
    rows: Vec<GotoRow>,
    ya_filtrada: bool,
    solo_con_consulta: bool,
}

impl FixedSource {
    /// Una fuente de filas fijas en `section`.
    #[must_use]
    pub fn new(section: GotoSection, rows: Vec<GotoRow>) -> Self {
        Self {
            section,
            rows,
            ya_filtrada: false,
            solo_con_consulta: false,
        }
    }

    /// Como [`Self::new`], pero declarando que las filas YA vienen
    /// filtradas por quien las trajo (ver [`GotoSource::ya_filtrada`]).
    #[must_use]
    pub fn ya_filtrada(section: GotoSection, rows: Vec<GotoRow>) -> Self {
        Self {
            section,
            rows,
            ya_filtrada: false,
            solo_con_consulta: false,
        }
        .con_ya_filtrada()
    }

    /// Declara que esta fuente sólo aporta con algo escrito (ver
    /// [`GotoSource::solo_con_consulta`]).
    #[must_use]
    pub fn solo_con_consulta(mut self) -> Self {
        self.solo_con_consulta = true;
        self
    }

    /// Marca sus filas como ya filtradas.
    #[must_use]
    fn con_ya_filtrada(mut self) -> Self {
        self.ya_filtrada = true;
        self
    }
}

impl GotoSource for FixedSource {
    fn section(&self) -> GotoSection {
        self.section
    }
    fn rows(&self, _query: &str) -> Vec<GotoRow> {
        self.rows.clone()
    }
    fn ya_filtrada(&self) -> bool {
        self.ya_filtrada
    }
    fn solo_con_consulta(&self) -> bool {
        self.solo_con_consulta
    }
}

/// La fuente de la RUTA TECLEADA: mira la consulta y, si parece una ruta,
/// ofrece ir ahí.
///
/// Es la única fuente que no tiene lista detrás — su fila ES lo que el
/// lector acaba de escribir— y por eso vive aquí y no en un frontend: la
/// decisión de qué cuenta como ruta ([`parece_ruta`]) es una sola para los
/// dos.
pub struct RutaSource {
    desc: String,
}

impl RutaSource {
    /// La fuente, con la línea de detalle que acompaña a la fila (ya
    /// traducida por el llamante: este módulo no elige idioma).
    #[must_use]
    pub fn new(desc: impl Into<String>) -> Self {
        Self { desc: desc.into() }
    }
}

impl GotoSource for RutaSource {
    fn section(&self) -> GotoSection {
        SECCION_RUTA
    }
    fn rows(&self, query: &str) -> Vec<GotoRow> {
        parece_ruta(query).map_or_else(Vec::new, |ruta| {
            vec![GotoRow {
                section: SECCION_RUTA.id,
                key: format!("{K_RUTA}{ruta}"),
                // Lo tecleado se pinta TAL CUAL. Es del propio lector, así
                // que no hay nada que enmascarar; y cambiárselo mientras lo
                // escribe es la peor forma de decirle que se equivocó.
                text: ruta.to_owned(),
                desc: self.desc.clone(),
                hostile: false,
            }]
        })
    }
    /// La fila de la ruta no pasa por el filtro: ES la consulta, y
    /// preguntarle a lo tecleado si se parece a sí mismo no puede decir
    /// nada útil. Se recorta al construirla (`parece_ruta` hace `trim`), y
    /// ese recorte bastaría para que el filtro genérico la tirara.
    fn ya_filtrada(&self) -> bool {
        true
    }
}

/// Una línea de lo que se pinta: una cabecera de sección, o una fila.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GotoLine {
    /// Cabecera de sección — nunca recibe el cursor.
    Header(GotoSection),
    /// Una fila, con su índice dentro de [`Goto::rows`].
    Row(usize),
}

/// El estado del «ir a»: las fuentes, la consulta, lo que se ve y dónde
/// está el cursor.
pub struct Goto {
    sources: Vec<Box<dyn GotoSource + Send>>,
    query: String,
    rows: Vec<GotoRow>,
    lines: Vec<GotoLine>,
    cursor: usize,
}

/// A mano porque [`GotoSource`] es un trait objeto y no puede derivar
/// `Debug`. De las fuentes se imprime cuántas hay, que es lo único que un
/// `Debug` podría decir de ellas sin obligar a toda fuente futura a
/// implementar `Debug` para nada.
impl std::fmt::Debug for Goto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Goto")
            .field("fuentes", &self.sources.len())
            .field("query", &self.query)
            .field("rows", &self.rows)
            .field("lines", &self.lines)
            .field("cursor", &self.cursor)
            .finish()
    }
}

impl Goto {
    /// Un «ir a» sobre estas fuentes, con la consulta vacía.
    #[must_use]
    pub fn new(sources: Vec<Box<dyn GotoSource + Send>>) -> Self {
        let mut goto = Self {
            sources,
            query: String::new(),
            rows: Vec::new(),
            lines: Vec::new(),
            cursor: 0,
        };
        goto.refrescar();
        goto
    }

    /// Vuelve a preguntar a todas las fuentes y rehace lo que se ve.
    ///
    /// El cursor se queda en la PRIMERA fila: la consulta cambió, así que
    /// lo que había bajo el cursor probablemente ya no está, y dejarlo
    /// donde estaba es cómo un Enter acaba yendo a un sitio que el lector
    /// no llegó a leer.
    pub fn refrescar(&mut self) {
        let q = self.query.to_lowercase();
        self.rows.clear();
        self.lines.clear();
        for seccion in ORDEN {
            let mut de_esta: Vec<GotoRow> = Vec::new();
            for fuente in &self.sources {
                if fuente.section().id != seccion.id {
                    continue;
                }
                if fuente.solo_con_consulta() && q.is_empty() {
                    continue;
                }
                let crudas = fuente.rows(&self.query);
                if fuente.ya_filtrada() || q.is_empty() {
                    de_esta.extend(crudas);
                } else {
                    de_esta.extend(
                        crudas
                            .into_iter()
                            .filter(|r| is_subsequence(&q, &r.text.to_lowercase())),
                    );
                }
            }
            if de_esta.is_empty() {
                continue;
            }
            de_esta.truncate(TOPE_POR_SECCION);
            self.lines.push(GotoLine::Header(*seccion));
            for fila in de_esta {
                self.lines.push(GotoLine::Row(self.rows.len()));
                self.rows.push(fila);
            }
        }
        self.cursor = self.primera_fila().unwrap_or(0);
    }

    /// Sustituye las filas de una sección por otras, y repinta.
    ///
    /// Es la puerta de las fuentes ASÍNCRONAS: el índice semántico
    /// pregunta al core, tarda, y cuando contesta su sección aparece sin
    /// tocar las demás. Reemplaza en vez de añadir porque una respuesta
    /// vieja no debe convivir con la nueva — son respuestas a consultas
    /// distintas, y juntas no describen ninguna de las dos.
    ///
    /// El cursor se queda DONDE ESTÁ si la línea bajo él sigue siendo una
    /// fila. Que una respuesta tardía mueva el cursor es cómo un Enter
    /// acaba en un sitio que el lector no eligió: escribió, leyó, fue a
    /// confirmar, y entre medias llegó el índice.
    pub fn reemplazar_seccion(
        &mut self,
        section: GotoSection,
        rows: Vec<GotoRow>,
        ya_filtrada: bool,
    ) {
        self.sources.retain(|s| s.section().id != section.id);
        self.sources.push(if ya_filtrada {
            Box::new(FixedSource::ya_filtrada(section, rows))
        } else {
            Box::new(FixedSource::new(section, rows))
        });
        let antes = self.selected().cloned();
        self.refrescar();
        if let Some(antes) = antes
            && let Some(i) = self.lines.iter().position(|l| match l {
                GotoLine::Row(i) => self.rows.get(*i) == Some(&antes),
                GotoLine::Header(_) => false,
            })
        {
            self.cursor = i;
        }
    }

    /// El índice de la primera línea que es una fila, si hay alguna.
    fn primera_fila(&self) -> Option<usize> {
        self.lines
            .iter()
            .position(|l| matches!(l, GotoLine::Row(_)))
    }

    /// Teclea un carácter en la consulta.
    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.refrescar();
    }

    /// Borra el último carácter de la consulta.
    pub fn backspace(&mut self) {
        self.query.pop();
        self.refrescar();
    }

    /// La consulta tal cual.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Sube a la fila anterior, saltándose las cabeceras. Se para arriba.
    pub fn up(&mut self) {
        let mut i = self.cursor;
        while i > 0 {
            i -= 1;
            if matches!(self.lines.get(i), Some(GotoLine::Row(_))) {
                self.cursor = i;
                return;
            }
        }
    }

    /// Baja a la fila siguiente, saltándose las cabeceras. Se para abajo.
    pub fn down(&mut self) {
        let mut i = self.cursor;
        while i + 1 < self.lines.len() {
            i += 1;
            if matches!(self.lines.get(i), Some(GotoLine::Row(_))) {
                self.cursor = i;
                return;
            }
        }
    }

    /// Lo que se pinta, en orden.
    #[must_use]
    pub fn lines(&self) -> &[GotoLine] {
        &self.lines
    }

    /// Las filas, indexadas por [`GotoLine::Row`].
    #[must_use]
    pub fn rows(&self) -> &[GotoRow] {
        &self.rows
    }

    /// En qué línea está el cursor.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// La fila bajo el cursor, si el cursor está sobre una.
    #[must_use]
    pub fn selected(&self) -> Option<&GotoRow> {
        match self.lines.get(self.cursor) {
            Some(GotoLine::Row(i)) => self.rows.get(*i),
            _ => None,
        }
    }

    /// Cuántas filas se ven ahora mismo (sin contar cabeceras).
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Si no se ve ninguna fila.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Si lo TECLEADO parece una ruta a la que ir, y no un texto que buscar.
///
/// Tres formas, y ninguna de ellas puede confundirse con una consulta: una
/// ruta absoluta (`/etc`), la de casa (`~` o `~/…`) y una URL con esquema
/// (`sftp://host/…`). Lo demás —`etc`, `documentos`— es una consulta, y una
/// ruta relativa no entra a propósito: «a dónde» no puede depender de en qué
/// panel estabas, o la misma tecla lleva a dos sitios distintos.
///
/// Devuelve el texto TAL CUAL se tecleó. Resolver `~` y validar el esquema
/// es del llamante, que es quien tiene el `VPath` y sabe si ese backend
/// existe: aquí sólo se decide si ofrecer la fila.
#[must_use]
pub fn parece_ruta(query: &str) -> Option<&str> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    if q.starts_with('/') || q == "~" || q.starts_with("~/") {
        return Some(q);
    }
    // Un esquema con `://` y algo detrás. `q.find` y no `split_once` para no
    // aceptar `://x`, que no nombra ningún backend.
    let idx = q.find("://")?;
    (idx > 0 && q.len() > idx + 3).then_some(q)
}

// ---------------------------------------------------------------------------
// Lo que antes decidía cada frontend por su cuenta (#357): cómo se construye
// cada clase de fila, qué clave de despacho lleva y qué significa confirmarla.
// La TUI lo tenía en `norte-tui/src/goto.rs`, y la ventana lo necesitaba igual;
// dos copias de «a dónde lleva este Enter» son dos respuestas.
// ---------------------------------------------------------------------------

/// Prefijo de despacho de una fila que lleva a un `VPath` ya conocido.
pub const K_IR: &str = "go:";
/// Prefijo de una fila que lleva a un comando del catálogo.
pub const K_CMD: &str = "cmd:";
/// Prefijo de la fila de la RUTA TECLEADA, que aún hay que resolver. Lo pone
/// [`RutaSource`].
pub const K_RUTA: &str = "path:";

/// Cuántas filas se traen de cada lista larga ANTES de filtrar.
///
/// No es el tope de lo que se ve —ése es [`TOPE_POR_SECCION`], y lo aplica el
/// modelo a todas las secciones por igual— sino cuántas entradas de una lista
/// de cientos se le ofrecen al filtro. Más holgado que el de pintado a
/// propósito: filtrar sobre cuarenta encuentra cosas que filtrar sobre doce
/// no, y las que sobren las recorta el modelo después.
pub const TRAIDAS_POR_LISTA: usize = 40;

/// A partir de cuántos caracteres se le pregunta al índice.
///
/// Con menos, la respuesta no puede ser buena —una o dos letras no son una
/// consulta semántica— y cada pregunta es una llamada a un proveedor que
/// cuesta tiempo y puede costar dinero.
pub const MINIMO_PARA_EL_INDICE: usize = 3;

/// Cuántos resultados se le piden al índice.
pub const TOPE_DEL_INDICE: u32 = 8;

/// Una fila hacia un directorio conocido.
///
/// `enc` es la reinterpretación de nombres del panel con el foco, y sólo se le
/// pasa a las rutas que son DE ese panel —su historia—. A las demás
/// (populares, favoritos, conexiones, índice) se les pasa `None`: son de toda
/// la sesión, y aplicarles el encoding de un panel a rutas de otro inventa
/// mojibake. Es la misma regla que ya escribió `popular_rows` en su sitio.
#[must_use]
pub fn fila_ruta(
    section: &'static str,
    nombre: Option<&str>,
    path: &norte_proto::VPath,
    enc: Option<norte_encoding::NameEncoding>,
) -> GotoRow {
    let (texto, hostil) = crate::path_display_with(path, enc);
    // El nombre que puso el lector (un favorito, una conexión) va DELANTE y la
    // ruta detrás: se busca por el nombre que uno mismo eligió, y la ruta es
    // lo que confirma que es la que se cree.
    let (text, desc, hostile) = match nombre {
        Some(n) => {
            let (nt, nh) = crate::display_name(n.as_bytes());
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

/// La fila de una conexión configurada: su nombre y su URL CRUDA de
/// `connections.toml`, que puede no parsear.
///
/// Se ofrece igual y se dice al confirmar —lo mismo que hace el selector de
/// `pane.connect`, con el mismo mensaje— en vez de desaparecer: «mi conexión
/// no sale en ir a» es peor que un error al pulsar Enter, porque no tiene ni
/// dónde mirar. Un FAVORITO que no parsea, en cambio, no se ofrece: la lista
/// de sitios ya lo enseña con su error.
#[must_use]
pub fn fila_conexion(nombre: &str, url: &str) -> GotoRow {
    let (texto, hostil) = norte_proto::VPath::parse(url).map_or_else(
        |_| (norte_encoding::mask_terminal_hazards(url), true),
        |p| crate::path_display_with(&p, None),
    );
    let (nt, nh) = crate::display_name(nombre.as_bytes());
    GotoRow {
        section: SECCION_CONEXIONES.id,
        key: format!("{K_IR}{url}"),
        text: nt,
        desc: texto,
        hostile: nh || hostil,
    }
}

/// Las filas de COMANDOS, a partir de las de la paleta: los MISMOS que la
/// paleta ofrece en ese contexto, porque dos listas de comandos calculadas por
/// separado divergen.
#[must_use]
pub fn filas_de_comandos(rows: Vec<crate::palette::Row>) -> Vec<GotoRow> {
    rows.into_iter()
        .map(|r| GotoRow {
            section: SECCION_COMANDOS.id,
            key: format!("{K_CMD}{}", r.key),
            text: r.text,
            desc: r.desc,
            hostile: r.hostile,
        })
        .collect()
}

/// Las filas de una tanda de resultados del índice semántico.
///
/// Son RUTAS de ficheros que el core encontró, así que su nombre son bytes de
/// disco y se pintan por el mismo camino enmascarado que las demás. Sin
/// reinterpretación, como los populares: lo que devuelve el índice puede estar
/// en cualquier sitio, no en el panel con el foco.
#[must_use]
pub fn filas_del_indice(hits: &[norte_proto::methods::SemanticHit]) -> Vec<GotoRow> {
    hits.iter()
        .map(|h| fila_ruta(SECCION_INDICE.id, None, &h.path, None))
        .collect()
}

/// Lo que significa confirmar una fila.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accion {
    /// Navegar el panel con el foco a este directorio.
    Ir(norte_proto::VPath),
    /// Correr este comando del catálogo, como si se hubiera pulsado su tecla.
    Comando(String),
    /// La fila no lleva a ningún sitio que se pueda resolver — una ruta
    /// tecleada que no parsea, sobre todo. Trae la clave del mensaje.
    Nada(&'static str),
}

/// Qué hacer con la fila que el lector acaba de confirmar.
///
/// La `key` NUNCA se pinta y aquí es donde se lee: el prefijo dice de qué
/// clase es la fila, y cada clase se resuelve por su camino. Una ruta TECLEADA
/// es la única que puede no resolver, porque es lo único que no salió de una
/// lista que ya existía.
///
/// ```
/// use norte_frontend::goto::{Accion, accion};
/// assert_eq!(accion("cmd:app.quit"), Accion::Comando("app.quit".to_owned()));
/// assert!(matches!(accion("go:file:///tmp"), Accion::Ir(_)));
/// assert!(matches!(accion("otra cosa"), Accion::Nada(_)));
/// ```
#[must_use]
pub fn accion(key: &str) -> Accion {
    if let Some(cmd) = key.strip_prefix(K_CMD) {
        return Accion::Comando(cmd.to_owned());
    }
    if let Some(wire) = key.strip_prefix(K_IR) {
        return norte_proto::VPath::parse(wire)
            .map_or(Accion::Nada("msg-goto-bad-path"), Accion::Ir);
    }
    if let Some(texto) = key.strip_prefix(K_RUTA) {
        return resolver_tecleada(texto);
    }
    Accion::Nada("msg-goto-bad-path")
}

/// Resuelve la ruta que el lector tecleó.
///
/// `~` se expande contra el HOME de este proceso, no contra el directorio del
/// panel: «la casa» es una sola, y hacerla depender de dónde estabas sería que
/// la misma tecla lleve a dos sitios. Una ruta absoluta se toma como local, y
/// una con esquema se parsea tal cual — si el backend no existe lo dice el
/// core, que es mejor que navegar a algo que no es lo que se escribió.
fn resolver_tecleada(texto: &str) -> Accion {
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
        return norte_proto::VPath::parse(texto)
            .map_or(Accion::Nada("msg-goto-bad-path"), Accion::Ir);
    };
    norte_vfs::native::vpath_from_native(&expandido)
        .map_or(Accion::Nada("msg-goto-bad-path"), Accion::Ir)
}

#[cfg(test)]
mod despacho_tests {
    use super::{Accion, K_CMD, K_IR, K_RUTA, accion};
    use norte_proto::VPath;

    /// Cada prefijo de `key` va por su camino, y ninguno se confunde con
    /// otro: la `key` no se pinta nunca, así que esto es lo único que decide
    /// a dónde lleva un Enter.
    #[test]
    fn cada_prefijo_resuelve_a_lo_suyo() {
        assert_eq!(
            accion(&format!("{K_CMD}app.quit")),
            Accion::Comando("app.quit".to_owned())
        );
        assert_eq!(
            accion(&format!("{K_IR}file:///tmp")),
            Accion::Ir(VPath::parse("file:///tmp").expect("wire"))
        );
        assert_eq!(
            accion(&format!("{K_RUTA}/tmp")),
            Accion::Ir(VPath::parse("file:///tmp").expect("wire"))
        );
    }

    /// Una ruta con esquema tecleada se parsea tal cual; una que NO parsea se
    /// dice, en vez de navegar a cualquier otra cosa. Un esquema desconocido
    /// SÍ parsea: quien dice que no hay quien lo sirva es el core.
    #[test]
    fn una_ruta_tecleada_que_no_parsea_se_dice() {
        assert!(
            matches!(accion(&format!("{K_RUTA}sftp://h//x")), Accion::Nada(_)),
            "un segmento vacío no es una ruta"
        );
        assert!(matches!(accion("otra cosa"), Accion::Nada(_)));
        assert!(matches!(
            accion(&format!("{K_RUTA}noexiste://h/x")),
            Accion::Ir(_)
        ));
    }

    /// `~` se expande contra el HOME del proceso, no contra el panel: la casa
    /// es una sola.
    #[test]
    fn la_casa_no_depende_del_panel() {
        let Accion::Ir(p) = accion(&format!("{K_RUTA}~")) else {
            panic!("`~` tiene que resolver mientras haya HOME");
        };
        let home = std::env::var("HOME").expect("HOME en el entorno de test");
        assert_eq!(
            p,
            norte_vfs::native::vpath_from_native(std::path::Path::new(&home))
                .expect("el HOME es una ruta")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Goto, GotoLine, GotoRow, GotoSection, GotoSource, SECCION_COMANDOS, SECCION_HISTORIA,
        SECCION_INDICE, TOPE_POR_SECCION, parece_ruta,
    };

    /// Una fuente de mentira con filas fijas.
    struct Fija {
        seccion: GotoSection,
        textos: Vec<&'static str>,
        ya_filtrada: bool,
        solo_con_consulta: bool,
    }

    impl GotoSource for Fija {
        fn section(&self) -> GotoSection {
            self.seccion
        }
        fn rows(&self, _query: &str) -> Vec<GotoRow> {
            self.textos
                .iter()
                .map(|t| GotoRow {
                    section: self.seccion.id,
                    key: (*t).to_owned(),
                    text: (*t).to_owned(),
                    desc: String::new(),
                    hostile: false,
                })
                .collect()
        }
        fn ya_filtrada(&self) -> bool {
            self.ya_filtrada
        }
        fn solo_con_consulta(&self) -> bool {
            self.solo_con_consulta
        }
    }

    fn fuente(seccion: GotoSection, textos: &[&'static str]) -> Box<dyn GotoSource + Send> {
        Box::new(Fija {
            seccion,
            textos: textos.to_vec(),
            ya_filtrada: false,
            solo_con_consulta: false,
        })
    }

    /// Las secciones salen en el orden de `ORDEN`, no en el orden en que se
    /// registraron las fuentes: lo que se aprende es dónde está cada cosa.
    #[test]
    fn las_secciones_salen_en_el_orden_fijo() {
        let goto = Goto::new(vec![
            fuente(SECCION_COMANDOS, &["app.quit"]),
            fuente(SECCION_HISTORIA, &["/etc"]),
        ]);
        let cabeceras: Vec<&str> = goto
            .lines()
            .iter()
            .filter_map(|l| match l {
                GotoLine::Header(s) => Some(s.id),
                GotoLine::Row(_) => None,
            })
            .collect();
        assert_eq!(cabeceras, vec!["history", "commands"]);
    }

    /// Una sección sin filas no pinta cabecera: una cabecera vacía dice que
    /// hay algo donde no hay nada.
    #[test]
    fn una_seccion_vacia_no_pinta_cabecera() {
        let mut goto = Goto::new(vec![
            fuente(SECCION_COMANDOS, &["app.quit"]),
            fuente(SECCION_HISTORIA, &["/etc"]),
        ]);
        for c in "quit".chars() {
            goto.push_char(c);
        }
        let cabeceras: Vec<&str> = goto
            .lines()
            .iter()
            .filter_map(|l| match l {
                GotoLine::Header(s) => Some(s.id),
                GotoLine::Row(_) => None,
            })
            .collect();
        assert_eq!(cabeceras, vec!["commands"], "/etc no casa con «quit»");
    }

    /// El cursor nunca se posa en una cabecera, ni bajando ni subiendo.
    #[test]
    fn el_cursor_salta_las_cabeceras() {
        let mut goto = Goto::new(vec![
            fuente(SECCION_HISTORIA, &["/etc", "/var"]),
            fuente(SECCION_COMANDOS, &["app.quit"]),
        ]);
        let mut vistos = Vec::new();
        for _ in 0..5 {
            vistos.push(goto.selected().map(|r| r.text.clone()));
            goto.down();
        }
        assert_eq!(
            vistos,
            vec![
                Some("/etc".to_owned()),
                Some("/var".to_owned()),
                Some("app.quit".to_owned()),
                Some("app.quit".to_owned()),
                Some("app.quit".to_owned()),
            ],
            "baja fila a fila y se para en la última, sin caer en la cabecera"
        );
        for _ in 0..5 {
            goto.up();
        }
        assert_eq!(goto.selected().map(|r| r.text.as_str()), Some("/etc"));
    }

    /// Una fuente que ya filtró —el índice semántico— no vuelve a pasar por
    /// la subsecuencia: sus resultados casan por significado, y las letras
    /// de la consulta pueden no estar en el nombre.
    #[test]
    fn una_fuente_ya_filtrada_no_se_vuelve_a_filtrar() {
        let indice = Box::new(Fija {
            seccion: SECCION_INDICE,
            textos: vec!["la factura del gas"],
            ya_filtrada: true,
            solo_con_consulta: false,
        });
        let mut goto = Goto::new(vec![indice, fuente(SECCION_HISTORIA, &["/etc"])]);
        for c in "recibo".chars() {
            goto.push_char(c);
        }
        assert_eq!(goto.len(), 1, "sobrevive la del índice, no la de historia");
        assert_eq!(
            goto.rows().first().map(|r| r.text.as_str()),
            Some("la factura del gas")
        );
    }

    /// Escribir mueve el cursor a la primera fila de lo que AHORA se ve.
    /// Dejarlo donde estaba es cómo un Enter va a un sitio que nadie leyó.
    #[test]
    fn escribir_devuelve_el_cursor_arriba() {
        let mut goto = Goto::new(vec![fuente(SECCION_HISTORIA, &["/etc", "/var"])]);
        goto.down();
        assert_eq!(goto.selected().map(|r| r.text.as_str()), Some("/var"));
        goto.push_char('e');
        assert_eq!(goto.selected().map(|r| r.text.as_str()), Some("/etc"));
    }

    /// Una respuesta tardía del índice no mueve el cursor de debajo del
    /// dedo: el lector escribió, leyó y fue a confirmar, y entre medias
    /// llegó una sección nueva.
    #[test]
    fn una_seccion_que_llega_tarde_no_mueve_el_cursor() {
        let mut goto = Goto::new(vec![fuente(SECCION_HISTORIA, &["/etc", "/var"])]);
        goto.down();
        assert_eq!(goto.selected().map(|r| r.text.as_str()), Some("/var"));

        goto.reemplazar_seccion(
            SECCION_INDICE,
            vec![GotoRow {
                section: SECCION_INDICE.id,
                key: "x".to_owned(),
                text: "lo que encontró el índice".to_owned(),
                desc: String::new(),
                hostile: false,
            }],
            true,
        );

        assert_eq!(
            goto.selected().map(|r| r.text.as_str()),
            Some("/var"),
            "el cursor sigue en lo que el lector estaba mirando"
        );
        assert_eq!(goto.len(), 3, "y la sección nueva está");
    }

    /// Y una respuesta nueva SUSTITUYE a la anterior: dos respuestas a
    /// consultas distintas juntas no describen ninguna de las dos.
    #[test]
    fn una_seccion_asincrona_se_sustituye_no_se_acumula() {
        let mut goto = Goto::new(vec![fuente(SECCION_HISTORIA, &["/etc"])]);
        for texto in ["primera", "segunda"] {
            goto.reemplazar_seccion(
                SECCION_INDICE,
                vec![GotoRow {
                    section: SECCION_INDICE.id,
                    key: texto.to_owned(),
                    text: texto.to_owned(),
                    desc: String::new(),
                    hostile: false,
                }],
                true,
            );
        }
        let del_indice: Vec<&str> = goto
            .rows()
            .iter()
            .filter(|r| r.section == SECCION_INDICE.id)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(del_indice, vec!["segunda"]);
    }

    /// Sin nada escrito, la sección de comandos no sale: quien abre «ir a»
    /// y no teclea está preguntando A DÓNDE puede ir, y cientos de verbos
    /// sepultan las listas de destinos que tiene encima. Con una letra,
    /// vuelven.
    #[test]
    fn los_comandos_no_salen_hasta_que_se_escribe() {
        let comandos = Box::new(Fija {
            seccion: SECCION_COMANDOS,
            textos: vec!["app.quit"],
            ya_filtrada: false,
            solo_con_consulta: true,
        });
        let mut goto = Goto::new(vec![comandos, fuente(SECCION_HISTORIA, &["/etc"])]);
        assert_eq!(goto.len(), 1, "sólo la historia");

        goto.push_char('q');

        assert_eq!(
            goto.rows().first().map(|r| r.text.as_str()),
            Some("app.quit"),
            "con algo escrito, el comando vuelve"
        );
    }

    /// Ninguna sección pasa de [`TOPE_POR_SECCION`] filas: una lista más
    /// larga no se lee, y para hojearlas enteras están sus pantallas.
    #[test]
    fn ninguna_seccion_pasa_del_tope() {
        let muchas: Vec<&'static str> = vec!["/x"; TOPE_POR_SECCION * 3];
        let goto = Goto::new(vec![fuente(SECCION_HISTORIA, &muchas)]);
        assert_eq!(goto.len(), TOPE_POR_SECCION);
    }

    /// Las tres formas que SON una ruta, y las que no.
    #[test]
    fn que_cuenta_como_ruta_tecleada() {
        assert_eq!(parece_ruta("/etc"), Some("/etc"));
        assert_eq!(parece_ruta("~"), Some("~"));
        assert_eq!(parece_ruta("~/notas"), Some("~/notas"));
        assert_eq!(parece_ruta("sftp://host/tmp"), Some("sftp://host/tmp"));
        assert_eq!(parece_ruta("  /etc  "), Some("/etc"), "se recorta");

        assert_eq!(parece_ruta(""), None);
        assert_eq!(parece_ruta("etc"), None, "relativa: no se sabe desde dónde");
        assert_eq!(parece_ruta("~notas"), None, "no es la casa de nadie");
        assert_eq!(parece_ruta("://x"), None, "sin esquema no hay backend");
        assert_eq!(parece_ruta("sftp://"), None, "sin destino no hay a dónde");
    }
}
