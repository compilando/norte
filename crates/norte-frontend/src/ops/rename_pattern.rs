//! El generador DETERMINISTA de un plan de renombrado en lote (#310).
//!
//! La maquinaria del lote ya existía entera —plan revisable, `plan_hash`,
//! colisiones, journal y undo (ADR 0042)— y el único que sabía producir un
//! plan era el modelo de lenguaje (`ai.rename_plan`). O sea que renombrar
//! veinte ficheros exigía un LLM. Esto es la otra mitad: una PLANTILLA que el
//! humano escribe y una expansión que no consulta a nadie.
//!
//! Lo que sale de aquí entra por el MISMO sitio que el plan de la IA
//! —`fs.rename_batch_plan`, la misma revisión, el mismo hash— porque lo que
//! hace segura la operación no es de dónde salieron los nombres.
//!
//! # Por qué texto y no bytes
//!
//! Un nombre son bytes (regla 1) y este módulo trabaja sobre `str`. No es un
//! descuido: el par que viaja en el plan (`AiRenameEntry`) es UTF-8 por
//! protocolo, así que un nombre que no lo sea no puede formar parte de un
//! lote — hoy tampoco por el camino de la IA. El llamante los aparta ANTES y
//! lo dice; aquí no se inventa una conversión con pérdida que renombraría un
//! fichero a un nombre que no es el suyo.

/// Los códigos que entiende una plantilla, tal como se escriben.
///
/// `[N]` el nombre sin extensión, `[E]` la extensión sin el punto, `[C]` un
/// contador que empieza en 1 — y `[C3]` el mismo contador acolchado con ceros
/// a tres dígitos. Todo lo demás es literal, incluido un corchete suelto.
///
/// Es el subconjunto de Total Commander que se usa a diario; su herramienta
/// tiene además rangos de subcadena y fechas, y esos se pueden añadir aquí
/// sin mover nada de lo que hay alrededor.
pub const CODES: &[&str] = &["[N]", "[E]", "[C]"];

/// Parte un nombre en `(base, extensión)`, sin el punto.
///
/// El punto que separa es el ÚLTIMO, y un nombre que empieza por punto y no
/// tiene otro —`.bashrc`— es todo base y sin extensión: renombrar un fichero
/// oculto con `[N].[E]` y que se convirtiera en `.bashrc.` sería la clase de
/// sorpresa que un renombrado en lote no se puede permitir.
#[must_use]
pub fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(i) => (&name[..i], &name[i + 1..]),
    }
}

/// Expande `pattern` para `name`, con `n` como valor del contador.
///
/// ```
/// use norte_frontend::rename_pattern::expand;
/// assert_eq!(expand("[N].[E]", "foto.JPG", 1), "foto.JPG");
/// assert_eq!(expand("vacaciones-[C3].[E]", "foto.jpg", 7), "vacaciones-007.jpg");
/// assert_eq!(expand("[N]", "notas.txt", 1), "notas");
/// ```
#[must_use]
pub fn expand(pattern: &str, name: &str, n: usize) -> String {
    let (base, ext) = split_name(name);
    let mut out = String::with_capacity(pattern.len() + name.len());
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'['
            && let Some(fin) = pattern[i..].find(']')
            && let Some(sustituto) = expand_code(&pattern[i + 1..i + fin], base, ext, n)
        {
            out.push_str(&sustituto);
            i += fin + 1;
            continue;
        }
        // Un corchete que no abre un código conocido es un carácter más: un
        // nombre puede llevarlos, y tragárselos convertiría `[borrador]` en
        // nada sin decir por qué.
        let c = pattern[i..].chars().next().unwrap_or('[');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Un código entre corchetes, ya sin ellos. `None` = no es uno de los
/// nuestros, y entonces el texto se queda tal cual.
fn expand_code(code: &str, base: &str, ext: &str, n: usize) -> Option<String> {
    match code {
        "N" => Some(base.to_owned()),
        "E" => Some(ext.to_owned()),
        "C" => Some(n.to_string()),
        _ => {
            let ancho: usize = code.strip_prefix('C')?.parse().ok()?;
            // Un ancho absurdo no rellena la memoria: lo que pide un lote de
            // ficheros cabe de sobra en dos dígitos y el tope deja margen.
            let ancho = ancho.min(12);
            Some(format!("{n:0ancho$}"))
        }
    }
}

/// El plan que produce `pattern` sobre `names`, en orden.
///
/// Devuelve pares `(from, to)` y **omite los que no cambian**: un plan que
/// promete renombrar algo a su propio nombre hace que el resumen mienta sobre
/// cuántas cosas van a pasar. El contador cuenta TODOS los nombres de la
/// entrada, cambien o no, porque lo contrario haría que el número dependiera
/// de la plantilla y saltaría huecos sin explicación.
///
/// ```
/// use norte_frontend::rename_pattern::plan;
/// let names = ["a.txt".to_owned(), "b.txt".to_owned()];
/// let pares = plan("nota-[C].[E]", &names, 1);
/// assert_eq!(pares, vec![
///     ("a.txt".to_owned(), "nota-1.txt".to_owned()),
///     ("b.txt".to_owned(), "nota-2.txt".to_owned()),
/// ]);
/// ```
#[must_use]
pub fn plan(pattern: &str, names: &[String], start: usize) -> Vec<(String, String)> {
    names
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let nuevo = expand(pattern, name, start.saturating_add(i));
            (nuevo != *name).then(|| (name.clone(), nuevo))
        })
        .collect()
}

/// Por qué una plantilla no sirve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternError {
    /// Vacía: no hay nombre que construir.
    Empty,
    /// Expandiría a un nombre vacío, o a uno que el sistema de ficheros no
    /// puede llevar (`/` o NUL dentro).
    BadResult,
}

/// La clave Fluent del diagnóstico.
#[must_use]
pub const fn error_key(e: PatternError) -> &'static str {
    match e {
        PatternError::Empty => "msg-rename-pattern-empty",
        PatternError::BadResult => "msg-rename-pattern-bad-result",
    }
}

/// Comprueba la plantilla contra los nombres que va a tocar.
///
/// Se valida ANTES de pedirle un plan al core: un `/` en la plantilla no es
/// un rename, es un movimiento a otro directorio disfrazado, y el sitio donde
/// eso se explica es el diálogo que el humano tiene delante — no un error del
/// daemon tres pasos después.
///
/// # Errors
/// [`PatternError::Empty`] con una plantilla en blanco;
/// [`PatternError::BadResult`] si algún nombre saldría vacío o con `/`/NUL.
pub fn check(pattern: &str, names: &[String]) -> Result<(), PatternError> {
    if pattern.trim().is_empty() {
        return Err(PatternError::Empty);
    }
    for (i, name) in names.iter().enumerate() {
        let nuevo = expand(pattern, name, i + 1);
        if nuevo.is_empty() || nuevo.contains('/') || nuevo.contains('\0') {
            return Err(PatternError::BadResult);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn los_tres_codigos_y_el_contador_acolchado() {
        assert_eq!(expand("[N].[E]", "foto.jpg", 1), "foto.jpg");
        assert_eq!(expand("[C]-[N].[E]", "foto.jpg", 4), "4-foto.jpg");
        assert_eq!(expand("[C2]", "foto.jpg", 4), "04");
        assert_eq!(expand("[C12]", "x", 1), "000000000001");
        // Ancho absurdo: se acota, no se cree.
        assert_eq!(expand("[C99]", "x", 1).len(), 12);
    }

    #[test]
    fn la_extension_es_el_ultimo_punto_y_un_oculto_no_tiene() {
        assert_eq!(split_name("a.tar.gz"), ("a.tar", "gz"));
        assert_eq!(split_name("sin-extension"), ("sin-extension", ""));
        assert_eq!(split_name(".bashrc"), (".bashrc", ""));
        // Y por eso `[N].[E]` sobre un oculto no le cuelga un punto al final.
        assert_eq!(expand("[N]", ".bashrc", 1), ".bashrc");
    }

    /// Un corchete que no es un código nuestro se queda tal cual: hay
    /// nombres con corchetes, y tragárselos sería perder texto sin decirlo.
    #[test]
    fn un_corchete_que_no_es_codigo_es_literal() {
        assert_eq!(expand("[borrador] [N]", "a.txt", 1), "[borrador] a");
        assert_eq!(expand("[X]-[N]", "a.txt", 1), "[X]-a");
        assert_eq!(expand("sin cerrar [N", "a.txt", 1), "sin cerrar [N");
    }

    /// El texto de la plantilla puede no ser ASCII, y no se parte por bytes.
    #[test]
    fn la_plantilla_admite_texto_no_ascii() {
        assert_eq!(expand("añó-[C]-[N].[E]", "a.txt", 2), "añó-2-a.txt");
    }

    /// Los que no cambian NO entran en el plan, y el contador no se salta
    /// nada por ello.
    #[test]
    fn el_plan_omite_lo_que_no_cambia_y_el_contador_no_salta() {
        let names = vec!["a.txt".to_owned(), "b.txt".to_owned(), "c.txt".to_owned()];
        // `b` ya se llama como saldría, así que no hay nada que hacer con él.
        let pares = plan("[N].[E]", &names, 1);
        assert!(pares.is_empty(), "nada cambia: plan vacío");

        let pares = plan("f[C].[E]", &names, 1);
        assert_eq!(
            pares,
            vec![
                ("a.txt".to_owned(), "f1.txt".to_owned()),
                ("b.txt".to_owned(), "f2.txt".to_owned()),
                ("c.txt".to_owned(), "f3.txt".to_owned()),
            ]
        );
    }

    #[test]
    fn el_contador_puede_arrancar_donde_se_diga() {
        let names = vec!["a".to_owned()];
        assert_eq!(
            plan("[C]", &names, 10),
            vec![("a".to_owned(), "10".to_owned())]
        );
    }

    /// Una plantilla vacía, o una que fabricaría un nombre imposible, se
    /// rechaza AQUÍ: con el humano delante y antes de pedir plan ninguno.
    #[test]
    fn una_plantilla_imposible_se_rechaza_antes_de_pedir_plan() {
        let names = vec!["a.txt".to_owned()];
        assert_eq!(check("", &names), Err(PatternError::Empty));
        assert_eq!(check("   ", &names), Err(PatternError::Empty));
        assert_eq!(check("sub/[N]", &names), Err(PatternError::BadResult));
        assert_eq!(
            check("[E]", &["sin-extension".to_owned()]),
            Err(PatternError::BadResult)
        );
        assert!(check("[N]-copia.[E]", &names).is_ok());
    }
}
