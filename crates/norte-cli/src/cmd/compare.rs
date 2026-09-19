//! `norte compare`: `fs.compare` y su veredicto en el código de salida, más
//! los helpers de presentación (`marcado`, códigos de salida) que
//! `sync`/`ai` comparten.

use std::process::ExitCode;

use anyhow::Context;
use norte_core::backend::Backend;
use norte_proto::TaskState;

use crate::cmd::connect::vpath;

/// El texto ya enmascarado, MARCADO con `!` si hubo que enmascararlo.
///
/// Una función y no las tres copias que había (`ai_cmd`, `compare_cmd`,
/// `sync_cmd`). El `!` es un marcador de SEGURIDAD: dice que lo que se lee no
/// es literalmente lo que hay en el disco, que es exactamente lo que un nombre
/// con una RLO dentro usaría para spoofear una confirmación. Tres copias de un
/// marcador de seguridad en un binario es como una de ellas deja de aplicarse
/// sin que nadie se entere.
///
/// Aquí y no en `norte-frontend` a propósito: el `lib.rs` de esa crate dice
/// que el enmascarado es suyo y el BADGE de la capa de pintado de cada
/// frontend — la TUI lo pinta con color y una tubería no tiene color que dar.
pub(crate) fn marcado(texto: &str, hostil: bool) -> String {
    format!("{}{texto}", if hostil { "!" } else { "" })
}

/// El `rel` de un paso (o de un fallo) de sincronización, listo para una
/// terminal. `render_step`/`rel_display` ya enmascararon (regla 1); esto solo
/// pone el [`marcado`] sobre el `hostile` que esa llamada ya calculó.
pub(crate) fn rel_marcado(d: &norte_frontend::sync::RelDisplay) -> String {
    marcado(&d.text, d.hostile)
}

/// stdout se cerró o falló mientras se imprimía: código 2, jamás un panic.
///
/// `println!` hace **panic** con `EPIPE`, y `norte compare a b | head -20` —la
/// forma obvia de asomarse a un diff que streamea— es exactamente eso: el
/// lector se va en cuanto tiene sus veinte líneas. Un 101 de pánico no está en
/// la tabla que estos dos comandos documentan, y además ensucia stderr en el
/// uso NORMAL de una tubería. El 2 sí está, y encima es verdad: lo que no se
/// pudo terminar de escribir tampoco se pudo contestar entero. `ls --json` ya
/// esquiva lo mismo con `serde_json::to_writer` + `?`.
pub(crate) fn codigo_por_escritura(e: &std::io::Error) -> ExitCode {
    // `EPIPE` es el lector que se fue: callar es lo correcto, no hay nada roto.
    // Cualquier otro fallo de escritura (un `> fichero` que llenó el disco) SÍ
    // se dice, o el 2 no tendría explicación en ninguna parte.
    if e.kind() != std::io::ErrorKind::BrokenPipe {
        eprintln!("norte: {e}");
    }
    ExitCode::from(2)
}

/// Un `Err` de `norte compare`/`norte sync` es un **2**, nunca el 1 de
/// `ExitCode::FAILURE`.
///
/// Estos dos comandos contestan en el código de salida, así que el 1 ya
/// significa algo: «difieren» en uno y «se aplicó» en el otro. El `match` de
/// `main` convierte cualquier `anyhow::Error` en `FAILURE`, o sea en ese mismo
/// 1 — de modo que un `--criteria` mal escrito, una ruta ilegible o un
/// `sync.apply` que se negó saldrían por la misma puerta que un éxito. Se
/// traducen aquí, en el despacho, para que **ningún** camino de error pueda
/// llegar al `match` de `main`: sólo un recorrido que TERMINÓ puede contestar
/// 0 o 1.
pub(crate) fn codigo_de_no_se_pudo(e: &anyhow::Error) -> ExitCode {
    eprintln!("norte: {e:#}");
    ExitCode::from(2)
}

/// Traduce `--criteria` a un [`norte_proto::methods::CompareCriteria`].
///
/// Vacío = el default del wire (tamaño y fecha, sin hash — ver el doctest de
/// `FsCompareParams`). No vacío = EXACTAMENTE la lista pedida: `--criteria
/// hash` a secas enciende solo `hash` y apaga `size`/`mtime`, para que "quiero
/// nada más que el hash" tenga el efecto obvio en la petición aunque el core
/// (ADR 0048) solo lo corra sobre las parejas que los rungs baratos ya dieron
/// por iguales.
pub(crate) fn parse_compare_criteria(
    names: &[String],
) -> anyhow::Result<norte_proto::methods::CompareCriteria> {
    if names.is_empty() {
        return Ok(norte_proto::methods::CompareCriteria::default());
    }
    let mut criteria = norte_proto::methods::CompareCriteria {
        size: false,
        mtime: false,
        hash: false,
    };
    for name in names {
        match name.as_str() {
            "size" => criteria.size = true,
            "mtime" => criteria.mtime = true,
            "hash" => criteria.hash = true,
            other => anyhow::bail!(
                "--criteria: criterio desconocido \"{}\"",
                other.escape_debug()
            ),
        }
    }
    Ok(criteria)
}

/// Lo que una comparación puede contestar, **en orden de precedencia**: el de
/// más abajo gana al de más arriba.
///
/// Tres y no dos, y con `Ord` derivado en vez de un `bool` acumulado, porque
/// la respuesta importante es la del medio: «no se pudo saber» tiene que ganar
/// a las otras dos, y un `bool` no tiene sitio donde guardarla. Es la misma
/// razón por la que el comando tiene tres códigos de salida.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Veredicto {
    /// Todas las filas dijeron «iguales», y todas con confianza.
    Coinciden,
    /// Alguna fila difiere, y ninguna se quedó sin contestar.
    Difieren,
    /// Alguna fila no se pudo contestar, o se contestó sin poder respaldarlo.
    NoSeSabe,
}

impl Veredicto {
    /// Lo que UNA fila aporta al veredicto de la comparación entera.
    ///
    /// La confianza va primero y no de adorno. `CompareVerdict::Same` con
    /// `CompareConfidence::Unknown` es lo que `cascade.rs` contesta cuando no
    /// pudo comparar nada —dos symlinks cuyos destinos no se leyeron, un lado
    /// sin tamaño, un socket— y es una respuesta honesta SOLO mientras quien
    /// la lee vea el glifo de confianza, como en la TUI. Colapsada a un código
    /// de salida sin ese matiz se convertiría en «los árboles coinciden», que
    /// es justamente lo que nadie comprobó.
    fn de_fila(row: &norte_proto::methods::CompareRow) -> Self {
        use norte_proto::methods::{CompareConfidence as Conf, CompareVerdict as V};
        match (row.verdict, row.confidence) {
            // Antes que el veredicto: una conclusión que el criterio no
            // respalda no se puede resumir, diga lo que diga.
            // `Unrecognised` es la confianza de un core N+1, y tampoco.
            (_, Conf::Unknown | Conf::Unrecognised) => Self::NoSeSabe,
            (V::Same, _) => Self::Coinciden,
            (V::Different | V::OnlyLeft | V::OnlyRight | V::TypeMismatch, _) => Self::Difieren,
            // `Error` (listado ilegible, directorio por encima del tope),
            // `Ambiguous` (una colisión de caja o de NFC — justo lo que una
            // sincronización posterior tiene que ver ANTES de escribir), y el
            // veredicto de un core N+1 que este binario no sabe leer. Ninguno
            // de los tres es «difieren»: es que no se sabe.
            _ => Self::NoSeSabe,
        }
    }

    /// El código de salida, que es toda la respuesta que un script lee.
    fn codigo(self) -> ExitCode {
        ExitCode::from(match self {
            Self::Coinciden => 0,
            Self::Difieren => 1,
            Self::NoSeSabe => 2,
        })
    }
}

/// `norte compare`: `fs.compare` y su veredicto en el código de salida.
///
/// # Por qué el veredicto va en el código
/// Es la pregunta «¿funcionó la copia?», y quien la hace suele ser un script.
/// `diff` contesta así desde siempre y no hay nada que mejorar en esa
/// convención: 0 iguales, 1 difieren, y un tercer código para «no se pudo
/// saber» que es el que de verdad importa aquí — una comparación INCOMPLETA
/// que contestara 0 sería exactamente el fallo que este comando existe para
/// no cometer. La precedencia entre los tres está en [`Veredicto`].
pub(crate) async fn compare_cmd(
    backend: &Backend,
    a: &std::path::Path,
    b: &std::path::Path,
    json: bool,
    criteria: &[String],
    max_depth: Option<u32>,
    mtime_tolerance_ms: Option<u32>,
) -> anyhow::Result<ExitCode> {
    use std::io::Write as _;

    let left = vpath(a)?;
    let right = vpath(b)?;
    let params = norte_proto::methods::FsCompareParams {
        left,
        right,
        criteria: parse_compare_criteria(criteria)?,
        max_depth,
        // 2000 ms es el default declarado por `FsCompareParams` (la regla
        // FAT, ADR 0048; ver su doctest: `mtime_tolerance_ms == 2000`). Se
        // repite el número aquí porque el tipo no deriva `Default` y la
        // constante que lo fija en el proto es privada — no hay un
        // `FsCompareParams::default()` que reutilizar.
        mtime_tolerance_ms: mtime_tolerance_ms.unwrap_or(2000),
        // `Backend::compare` rechaza `follow_symlinks: true`, y este comando
        // no tiene motivo para diferir de `sync_plan`, que rechaza los dos.
        follow_symlinks: false,
        descend_orphans: None,
    };

    let (task, mut rx) = backend
        .compare(params)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-compare-failed"))?;

    // Nombres del árbol del OTRO lado, que este proceso no controla: MARCAR
    // el enmascarado, igual que `ai_cmd` — un nombre remoto puede traer RLO y
    // spoofear la salida. `cells_for` ya enmascaró `RowFace::name` con
    // `display_name_with` (rule 1); esto solo añade el `!` de [`marcado`] sobre
    // el `hostile` que esa llamada ya calculó — no un segundo enmascarado por
    // separado, que divergiría el día que este comando gane una
    // reinterpretación (#57) y alguien olvide threadearla también aquí.
    let cara = |face: &norte_frontend::compare::RowFace| marcado(&face.name, face.hostile);

    // Una tubería que se cierra no puede hacer `panic!`: ver
    // [`codigo_por_escritura`]. Bufferizado además porque una fila por
    // `write` syscall sobre un árbol grande es un peaje que no hace falta.
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());

    // Se decide fila a fila mientras se drena — jamás se coleccionan (la
    // rustdoc de `ComparePane` explica lo que cuesta retener un millón de
    // filas, y este comando no tiene motivo para retener ninguna).
    let mut veredicto = Veredicto::Coinciden;
    while let Some(batch) = rx.recv().await {
        for row in &batch.rows {
            veredicto = veredicto.max(Veredicto::de_fila(row));
            let escrito = if json {
                // Forma wire (lossless); --json no traduce ni enmascara —
                // un consumidor de script decodifica con el mismo códec que
                // `norte ls --json`.
                writeln!(out, "{}", serde_json::to_string(row)?)
            } else {
                let cells = norte_frontend::compare::cells_for(row, None, None);
                let left_name = cells.left.as_ref().map_or_else(String::new, cara);
                let right_name = cells.right.as_ref().map_or_else(String::new, cara);
                // LOS DOS glifos, como la TUI. `Same` no es una respuesta por
                // sí solo (ver la rustdoc de `compare::Glyphs`): `Same`/`!`
                // salió de un hash o de un tamaño distinto y `Same`/`?` de un
                // provider que no pudo contestar, y enseñar uno sin el otro es
                // la deriva que este comando existe para no tener.
                writeln!(
                    out,
                    "{}{} {left_name}\t{right_name}",
                    cells.glyphs.verdict, cells.glyphs.confidence
                )
            };
            if let Err(e) = escrito {
                return Ok(codigo_por_escritura(&e));
            }
        }
    }
    if let Err(e) = out.flush() {
        return Ok(codigo_por_escritura(&e));
    }
    // El lock de stdout se suelta AQUÍ: lo que quede por decir va a stderr.
    drop(out);

    match task.join().await {
        TaskState::Completed => Ok(veredicto.codigo()),
        other => {
            eprintln!(
                "norte: {}",
                norte_i18n::ta(
                    "cli-compare-incomplete",
                    &[("state", &format!("{other:?}"))],
                )
            );
            Ok(ExitCode::from(2))
        }
    }
}
