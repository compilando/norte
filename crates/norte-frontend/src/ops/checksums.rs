//! El formato `sha256sum` visto desde un frontend (#311): leerlo, escribirlo y
//! comparar contra él.
//!
//! Es texto de OTROS —lo publica quien distribuye el fichero— así que se parsea
//! defensivamente: una línea que no encaja se salta en vez de tumbar el
//! fichero entero, porque un `SHA256SUMS` con una firma PGP pegada delante o un
//! comentario al final sigue siendo perfectamente comprobable.
//!
//! **Saltarse una línea se CUENTA.** Lo que no se entendió no puede
//! desaparecer: el resumen «N comprobados, todos correctos» sobre las
//! supervivientes es un verde falso, y la línea que se cayó es justamente la
//! del nombre raro, que es la que un atacante controlaría.
//!
//! # Los nombres son BYTES (regla 1)
//!
//! Un fichero de sumas es una lista de nombres, y un nombre no tiene por qué
//! ser texto. Aquí se parsea sobre bytes y el nombre sale como `Vec<u8>`: pasar
//! por `String` convertiría `caf\xE9.txt` en un nombre con `U+FFFD` que no
//! existe en el disco, y la comprobación diría «falta» sobre un fichero que
//! está ahí. Escribir tiene el mismo problema al revés, y por eso
//! [`to_sums_bytes`] produce BYTES y no una `String`.

/// Longitud de un sha256 en hex.
const SHA256_HEX: usize = 64;

/// Una línea del fichero de sumas: el digest y el nombre al que se refiere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SumLine {
    /// El digest tal como venía, ya en minúscula.
    pub digest: String,
    /// El nombre, en BYTES y relativo al directorio del fichero de sumas.
    pub name: Vec<u8>,
}

/// Lo que salió de leer un fichero de sumas.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedSums {
    /// Las líneas que se entendieron, en el orden del fichero.
    pub lines: Vec<SumLine>,
    /// Cuántas líneas PARECÍAN de sumas y no se pudieron leer.
    ///
    /// Un comentario o una cabecera PGP no cuentan aquí: no pretendían ser una
    /// suma. Esto cuenta lo que llevaba un digest delante y aun así no encajó,
    /// que es lo que no se puede callar — comprobar 37 de 40 líneas y decir
    /// «40 correctos» es la respuesta equivocada de la única herramienta cuyo
    /// trabajo es comprobar.
    pub refused: usize,
}

/// Si estos bytes son un fichero de texto en UTF-16 (BOM al principio).
///
/// Existe para poder DECIR por qué no salió ninguna línea: es lo que escribe
/// `Get-FileHash | Out-File` en PowerShell por defecto, y «esto no parece un
/// fichero de sumas» sobre un fichero de sumas perfectamente válido en otra
/// codificación manda a quien lo lee a buscar el problema donde no está.
#[must_use]
pub fn looks_utf16(bytes: &[u8]) -> bool {
    matches!(bytes.first_chunk::<2>(), Some([0xFF, 0xFE] | [0xFE, 0xFF]))
}

/// Lee un fichero de sumas al estilo `sha256sum`.
///
/// Acepta las formas que producen las herramientas de verdad:
///
/// - `digest␠␠nombre` (texto) y `digest␠*nombre` (binario);
/// - la forma ESCAPADA de coreutils, `\digest␠␠nom\\bre`, que es lo que
///   `sha256sum` escribe cuando el nombre lleva `\`, LF o CR — sin esto, el
///   fichero de nombre hostil no se comprobaba Y NI SIQUIERA SE LISTABA;
/// - la forma BSD/`--tag`, `SHA256 (nombre) = digest`, que `sha256sum -c`
///   también lee;
/// - CRLF y un BOM UTF-8 al principio (un fichero generado en Windows perdía
///   su primera línea en silencio).
///
/// El digest se normaliza a minúscula: hay quien publica en mayúscula, y dos
/// escrituras del mismo hash que comparan distinto son un bug esperando.
///
/// ```
/// use norte_frontend::checksums::parse_sums;
/// let texto = b"# generado a mano\n\
///               e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  vacio\n";
/// let leido = parse_sums(texto);
/// assert_eq!(leido.lines.len(), 1);
/// assert_eq!(leido.lines[0].name, b"vacio");
/// assert_eq!(leido.refused, 0, "un comentario no es una línea rechazada");
/// ```
#[must_use]
pub fn parse_sums(bytes: &[u8]) -> ParsedSums {
    // BOM UTF-8: se lo come el prefijo de la primera línea y con él su hex.
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut out = ParsedSums::default();
    for linea in bytes.split(|b| *b == b'\n') {
        // CR final de un fichero escrito en Windows: fuera antes de nada, o el
        // último byte del NOMBRE sería un retorno de carro.
        let linea = linea.strip_suffix(b"\r").unwrap_or(linea);
        if linea.is_empty() || linea.first() == Some(&b'#') {
            continue;
        }
        // La forma escapada lleva `\` DELANTE del digest, y el nombre viene con
        // `\\`, `\n` y `\r` dentro.
        let (linea, escapada) = match linea.strip_prefix(b"\\") {
            Some(resto) => (resto, true),
            None => (linea, false),
        };
        if let Some(l) = linea_normal(linea, escapada) {
            out.lines.push(l);
        } else if let Some(l) = linea_bsd(linea) {
            out.lines.push(l);
        } else if parecia_una_suma(linea) {
            // Llevaba un digest delante y aun así no encajó: eso no se calla.
            out.refused += 1;
        }
    }
    out
}

/// `digest␠␠nombre` / `digest␠*nombre`, con el nombre desescapado si la línea
/// venía marcada.
fn linea_normal(linea: &[u8], escapada: bool) -> Option<SumLine> {
    // 64 de hex + 2 de separador + 1 de nombre, como mínimo.
    if linea.len() < SHA256_HEX + 3 {
        return None;
    }
    let (hex, resto) = linea.split_at(SHA256_HEX);
    if !hex.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    // El separador de `sha256sum` son DOS caracteres: espacio + (espacio o
    // `*`). Otra cosa no es una línea suya, y adivinarlo sería inventarse un
    // nombre que empieza por un espacio.
    let [b' ', b' ' | b'*', nombre @ ..] = resto else {
        return None;
    };
    let nombre = if escapada {
        desescapar(nombre)
    } else {
        nombre.to_vec()
    };
    (!nombre.is_empty()).then(|| SumLine {
        digest: hex_minuscula(hex),
        name: nombre,
    })
}

/// `SHA256 (nombre) = digest`, la forma que escribe `sha256sum --tag`.
///
/// El nombre va entre el primer `(` y el ÚLTIMO `)`: uno que lleve paréntesis
/// dentro es legal, y cortar por el primero lo partiría.
fn linea_bsd(linea: &[u8]) -> Option<SumLine> {
    let resto = linea.strip_prefix(b"SHA256 (")?;
    let cierre = resto.iter().rposition(|b| *b == b')')?;
    let (nombre, cola) = resto.split_at(cierre);
    let hex = cola.strip_prefix(b") = ")?;
    if nombre.is_empty() || hex.len() != SHA256_HEX || !hex.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    Some(SumLine {
        digest: hex_minuscula(hex),
        name: nombre.to_vec(),
    })
}

/// Si esta línea pretendía ser una suma: empieza por 64 de hex, o por la
/// etiqueta de la forma BSD. Un comentario o una cabecera PGP no lo pretenden.
fn parecia_una_suma(linea: &[u8]) -> bool {
    linea.starts_with(b"SHA256 (")
        || linea
            .first_chunk::<SHA256_HEX>()
            .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
}

/// El hex a `String` sin pasar por una conversión lossy: el llamante ya
/// comprobó que son todo dígitos hexadecimales, o sea ASCII.
fn hex_minuscula(hex: &[u8]) -> String {
    hex.iter().map(|b| b.to_ascii_lowercase() as char).collect()
}

/// Deshace el escapado de coreutils: `\\` es una contrabarra, `\n` un salto de
/// línea y `\r` un retorno de carro. Cualquier otra pareja se deja tal cual —
/// inventarse un significado sería cambiar el nombre.
fn desescapar(nombre: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nombre.len());
    let mut i = 0;
    while i < nombre.len() {
        match (nombre[i], nombre.get(i + 1)) {
            (b'\\', Some(b'\\')) => {
                out.push(b'\\');
                i += 2;
            }
            (b'\\', Some(b'n')) => {
                out.push(b'\n');
                i += 2;
            }
            (b'\\', Some(b'r')) => {
                out.push(b'\r');
                i += 2;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// El veredicto de una línea comprobada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// El digest calculado es el publicado.
    Ok,
    /// El fichero está, y NO es el que dice la lista.
    Mismatch,
    /// No se pudo leer: no está, o no se dejó.
    Missing,
    /// Es un directorio (o algo que el backend no lee como fichero).
    NotAFile,
    /// El nombre que trae la lista no se puede nombrar en este sistema.
    ///
    /// Un nombre con `/` dentro, uno vacío, o —en Windows— uno con `\` o `:`.
    /// Es un veredicto propio y no un «falta» porque se arregla de otra forma:
    /// el fichero puede estar perfectamente ahí.
    Unnameable,
}

impl Verdict {
    /// La clave Fluent con la que se pinta.
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Ok => "checksum-ok",
            Self::Mismatch => "checksum-mismatch",
            Self::Missing => "checksum-missing",
            Self::NotAFile => "checksum-not-a-file",
            Self::Unnameable => "checksum-unnameable",
        }
    }

    /// Si este veredicto es «se comprobó y salió bien». Todo lo demás —incluido
    /// lo que no se pudo mirar— cuenta como no comprobado.
    #[must_use]
    pub const fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Compara lo publicado con lo calculado, línea a línea y en el orden del
/// FICHERO DE SUMAS.
///
/// `calculado` empareja por NOMBRE en bytes. Un nombre que la lista menciona y
/// del que no hay digest es [`Verdict::Missing`] — que es lo que hay que decir:
/// no se pudo comprobar, y eso no es «bien».
///
/// La comparación del digest es en minúscula por los dos lados
/// ([`parse_sums`] ya normaliza el suyo, y el core emite minúscula), así que
/// aquí no se vuelve a plegar nada: hacerlo dos veces esconde el día en que uno
/// de los dos deje de hacerlo.
///
/// El emparejamiento va por índice sobre un mapa construido una vez: con el
/// tope de 4096 rutas, buscar linealmente por línea son millones de
/// comparaciones de vectores en el hilo que pinta.
#[must_use]
pub fn verify(publicado: &[SumLine], calculado: &[(Vec<u8>, Option<String>)]) -> Vec<Verdict> {
    let mut mapa: std::collections::HashMap<&[u8], Option<&str>> =
        std::collections::HashMap::with_capacity(calculado.len());
    for (nombre, digest) in calculado {
        // El PRIMERO gana: un nombre repetido en la petición se calculó dos
        // veces con el mismo resultado, y quedarse con el último no diría nada
        // distinto.
        mapa.entry(nombre.as_slice())
            .or_insert_with(|| digest.as_deref());
    }
    publicado
        .iter()
        .map(
            |linea| match mapa.get(linea.name.as_slice()).copied().flatten() {
                Some(d) if d == linea.digest => Verdict::Ok,
                Some(_) => Verdict::Mismatch,
                None => Verdict::Missing,
            },
        )
        .collect()
}

/// Una ruta del informe, reducida a lo que el veredicto necesita: su nombre
/// Convierte los nombres de un fichero de sumas en RUTAS del directorio que lo
/// contiene, y dice en qué posición de la petición quedó cada uno.
///
/// Devuelve `(rutas a pedir, por línea su posición en esa lista)`. Un `None`
/// es un nombre que este sistema no puede escribir —con un componente vacío,
/// o con `\`/`:` en Windows—: no se pide, y su veredicto será
/// [`Verdict::Unnameable`], que no es lo mismo que «falta».
///
/// **Los nombres con directorio se parten por segmentos** en vez de tirarse:
/// `sub/dentro.txt` es lo que escriben `sha256sum -r` y un `find -exec`, y son
/// ficheros de sumas de todos los días. El confinamiento sale gratis y hay que
/// decirlo — `Segment::new` rechaza `.`, `..`, el vacío y el NUL, así que un
/// fichero de sumas hostil no puede salir de su directorio.
///
/// Vive aquí y no en un frontend porque es la misma decisión en la terminal y
/// en la ventana (ADR 0077).
///
/// ```
/// use norte_frontend::checksums::{parse_sums, resolve_targets};
/// # let vacio = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// let base = norte_proto::VPath::parse("file:///d").unwrap();
/// let lineas = parse_sums(format!("{vacio}  sub/x\n{vacio}  ..\n").as_bytes()).lines;
/// let (rutas, asked) = resolve_targets(&base, &lineas);
/// assert_eq!(rutas.len(), 1, "`..` no se pide");
/// assert_eq!(asked, vec![Some(0), None]);
/// ```
#[must_use]
pub fn resolve_targets(
    base: &norte_proto::VPath,
    lines: &[SumLine],
) -> (Vec<norte_proto::VPath>, Vec<Option<usize>>) {
    let mut paths: Vec<norte_proto::VPath> = Vec::with_capacity(lines.len());
    let mut asked = Vec::with_capacity(lines.len());
    for linea in lines {
        let mut ruta = base.clone();
        let mut vale = !linea.name.is_empty();
        for parte in linea.name.split(|b| *b == b'/') {
            // `a//b` y una barra final: un separador repetido no nombra nada.
            if parte.is_empty() {
                continue;
            }
            let Ok(seg) = norte_proto::Segment::new(parte.to_vec()) else {
                vale = false;
                break;
            };
            ruta = ruta.join(seg);
        }
        // Un nombre que se queda en el propio directorio (`.`, o todo
        // separadores) tampoco nombra un fichero de dentro.
        if vale && ruta == *base {
            vale = false;
        }
        if vale {
            asked.push(Some(paths.len()));
            paths.push(ruta);
        } else {
            asked.push(None);
        }
    }
    (paths, asked)
}

/// Una entrada del informe: su digest y —si no lo hay— el motivo que dio el
/// core, **en el orden en que se pidieron las rutas**.
pub type Computed = (Option<String>, Option<norte_proto::methods::ChecksumMiss>);

/// El veredicto de cada línea del fichero de sumas, con los motivos ya
/// distinguidos (#311).
///
/// `asked` dice, para cada línea publicada, en qué posición de la PETICIÓN
/// quedó su ruta —`None` si su nombre no se puede escribir en este sistema—, y
/// el informe conserva ese orden. Se empareja por ÍNDICE y no por nombre a
/// propósito: por nombre base, un `SHA256SUMS` que dice `sub/dentro.txt`
/// —lo que escribe `sha256sum -r`— no casaba con nada y salía «falta» sobre un
/// fichero que estaba ahí; y dos líneas con el mismo nombre base en carpetas
/// distintas se habrían juzgado con el digest de la otra.
///
/// ```
/// use norte_frontend::checksums::{judge, parse_sums, Verdict};
/// let vacio = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// let pub_ = parse_sums(format!("{vacio}  a\n").as_bytes()).lines;
/// let calc = vec![(Some(vacio.to_owned()), None)];
/// assert_eq!(judge(&pub_, &[Some(0)], &calc), vec![Verdict::Ok]);
/// ```
#[must_use]
pub fn judge(
    published: &[SumLine],
    asked: &[Option<usize>],
    computed: &[Computed],
) -> Vec<Verdict> {
    use norte_proto::methods::ChecksumMiss;

    published
        .iter()
        .enumerate()
        .map(|(i, linea)| {
            // Un nombre que este sistema no puede escribir no es un «falta»:
            // el fichero puede estar ahí, y mandar a buscarlo es mandar mal.
            let Some(Some(k)) = asked.get(i).copied() else {
                return Verdict::Unnameable;
            };
            // Una posición que el informe no trae es una lista más corta de lo
            // que se pidió, y eso ya lo filtró quien exige `pending == 0`:
            // aquí es «no se pudo mirar», nunca «bien».
            let Some((digest, miss)) = computed.get(k) else {
                return Verdict::Missing;
            };
            match (digest, miss) {
                (Some(d), _) if *d == linea.digest => Verdict::Ok,
                (Some(_), _) => Verdict::Mismatch,
                (None, Some(ChecksumMiss::NotAFile)) => Verdict::NotAFile,
                (None, _) => Verdict::Missing,
            }
        })
        .collect()
}

/// Lo que la barra tiene que decir de una comprobación.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Summary {
    /// Se comprobaron `n` y todas cuadran. **Solo** cuando no se cayó ni una
    /// línea: decirlo sobre 37 de 40 es el falso verde que esto viene a
    /// impedir.
    AllOk {
        /// Cuántas líneas se comprobaron.
        n: usize,
    },
    /// `n` no cuadran, faltan, o no se pudieron mirar.
    Bad {
        /// Cuántas no salieron bien.
        n: usize,
    },
    /// `refused` líneas del fichero ni siquiera se entendieron.
    Unreadable {
        /// Cuántas líneas se llegaron a comprobar.
        n: usize,
        /// Cuántas parecían sumas y no se entendieron.
        refused: usize,
    },
}

/// Qué decir de estos veredictos, con las líneas que el parser no entendió
/// pesando por delante de todo lo demás.
#[must_use]
pub fn summarize(verdicts: &[Verdict], refused: usize) -> Summary {
    if refused > 0 {
        return Summary::Unreadable {
            n: verdicts.len(),
            refused,
        };
    }
    let malas = verdicts.iter().filter(|v| !v.is_ok()).count();
    if malas == 0 {
        Summary::AllOk { n: verdicts.len() }
    } else {
        Summary::Bad { n: malas }
    }
}

/// Lo que se copia al portapapeles: una línea por fichero en el formato que
/// `sha256sum -c` sabe leer, **en BYTES**.
///
/// En bytes y no en `String` porque un nombre no tiene por qué ser texto
/// (regla 1). Pasarlo por `String::from_utf8_lossy` metía `U+FFFD` en la línea
/// y `sha256sum -c` respondía «no such file» sobre un fichero que estaba ahí;
/// peor, dos nombres distintos que colapsan al mismo carácter de reemplazo
/// salían como la misma línea dos veces.
///
/// Un nombre con `\`, LF o CR sale ESCAPADO como lo escribe coreutils: la línea
/// lleva `\` delante y dentro `\\`, `\n`, `\r`. Sin eso, un nombre con un salto
/// de línea partía la lista en dos y podía inyectar una entrada falsa en el
/// `SHA256SUMS` que alguien pegue en otro sitio.
///
/// Lo que no tiene digest no produce línea: no hay nada que comprobar de ello.
#[must_use]
pub fn to_sums_bytes(entries: &[(Vec<u8>, Option<String>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (nombre, digest) in entries {
        let Some(d) = digest else { continue };
        let escapar = nombre.iter().any(|b| matches!(b, b'\\' | b'\n' | b'\r'));
        if escapar {
            out.push(b'\\');
        }
        out.extend_from_slice(d.as_bytes());
        out.extend_from_slice(b"  ");
        if escapar {
            for b in nombre {
                match b {
                    b'\\' => out.extend_from_slice(b"\\\\"),
                    b'\n' => out.extend_from_slice(b"\\n"),
                    b'\r' => out.extend_from_slice(b"\\r"),
                    otro => out.push(*otro),
                }
            }
        } else {
            out.extend_from_slice(nombre);
        }
        out.push(b'\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const VACIO: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn lee_las_dos_formas_de_sha256sum_y_tolera_crlf() {
        let texto = format!("{VACIO}  texto.txt\r\n{ABC} *binario.bin\n");
        let l = parse_sums(texto.as_bytes()).lines;
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].name, b"texto.txt");
        assert_eq!(l[1].name, b"binario.bin", "la forma binaria (`*`) también");
        assert_eq!(l[1].digest, ABC);
    }

    /// Un fichero de sumas viene de FUERA: comentarios, líneas en blanco y
    /// basura no pueden tumbar la comprobación de las que sí valen. Pero lo que
    /// PARECÍA una suma y no se entendió se cuenta, o el resumen mentiría.
    #[test]
    fn se_salta_lo_que_no_es_una_linea_de_sumas_y_cuenta_lo_que_lo_parecia() {
        let texto = format!(
            "# comentario\n\n-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\
             {VACIO}  bueno\nzz{}  hex malo\n{VACIO} un-solo-espacio\n",
            &VACIO[2..]
        );
        let leido = parse_sums(texto.as_bytes());
        assert_eq!(leido.lines.len(), 1, "solo la buena: {leido:?}");
        assert_eq!(leido.lines[0].name, b"bueno");
        assert_eq!(
            leido.refused, 1,
            "la del separador de un espacio llevaba digest y no encajó; \
             el hex malo y las cabeceras PGP no pretendían ser sumas"
        );
    }

    /// Lo que coreutils escribe cuando el nombre lleva LF o contrabarra. Sin
    /// esto la línea desaparecía entera: ni se comprobaba ni se listaba, y el
    /// resumen decía «todos correctos».
    #[test]
    fn lee_la_forma_escapada_de_coreutils() {
        let texto = format!("\\{VACIO}  a\\nb\n\\{ABC}  c\\\\d\n");
        let l = parse_sums(texto.as_bytes()).lines;
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].name, b"a\nb", "`\\n` es un salto de línea de verdad");
        assert_eq!(l[1].name, b"c\\d", "`\\\\` es UNA contrabarra");
    }

    /// `sha256sum --tag`, que `sha256sum -c` también lee.
    #[test]
    fn lee_la_forma_bsd() {
        let texto = format!("SHA256 (mi (fichero).txt) = {VACIO}\n");
        let l = parse_sums(texto.as_bytes()).lines;
        assert_eq!(l.len(), 1);
        assert_eq!(
            l[0].name, b"mi (fichero).txt",
            "el nombre va hasta el ÚLTIMO paréntesis"
        );
        assert_eq!(l[0].digest, VACIO);
    }

    /// Un fichero generado en Windows empieza por un BOM, y ese BOM se comía la
    /// primera línea sin decir nada.
    #[test]
    fn un_bom_utf8_no_se_lleva_la_primera_linea() {
        let mut bytes = b"\xEF\xBB\xBF".to_vec();
        bytes.extend_from_slice(format!("{VACIO}  primero\n").as_bytes());
        assert_eq!(parse_sums(&bytes).lines.len(), 1);
    }

    /// Un fichero en UTF-16 (PowerShell) no produce líneas, y hay que poder
    /// decir por qué en vez de «esto no parece un fichero de sumas».
    #[test]
    fn un_fichero_utf16_se_reconoce_como_tal() {
        let mut bytes = vec![0xFF, 0xFE];
        for c in format!("{VACIO}  x\n").encode_utf16() {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        assert!(parse_sums(&bytes).lines.is_empty());
        assert!(looks_utf16(&bytes));
    }

    /// Mayúscula publicada, minúscula calculada: es el mismo hash, y decir que
    /// no lo es sería el peor falso negativo posible.
    #[test]
    fn el_digest_se_normaliza_a_minuscula() {
        let texto = format!("{}  x\n", VACIO.to_uppercase());
        assert_eq!(parse_sums(texto.as_bytes()).lines[0].digest, VACIO);
    }

    /// Un nombre que NO es UTF-8 sobrevive al parseo byte a byte: pasar por
    /// `String` lo convertiría en otro nombre y la comprobación diría «falta»
    /// sobre un fichero que está.
    #[test]
    fn un_nombre_que_no_es_utf8_sobrevive() {
        let mut linea = format!("{VACIO}  caf").into_bytes();
        linea.extend_from_slice(&[0xE9, b'.', b't', b'x', b't', b'\n']);
        let l = parse_sums(&linea).lines;
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].name, b"caf\xE9.txt");
    }

    #[test]
    fn los_tres_veredictos() {
        let publicado = parse_sums(format!("{VACIO}  a\n{ABC}  b\n{VACIO}  c\n").as_bytes()).lines;
        let calculado = vec![
            (b"a".to_vec(), Some(VACIO.to_owned())),
            (b"b".to_vec(), Some(VACIO.to_owned())),
            (b"c".to_vec(), None),
        ];
        assert_eq!(
            verify(&publicado, &calculado),
            vec![Verdict::Ok, Verdict::Mismatch, Verdict::Missing]
        );
    }

    /// El texto del portapapeles es el que `sha256sum -c` sabe leer, y lo que
    /// no tiene digest no se inventa una línea.
    #[test]
    fn el_texto_de_sumas_omite_lo_que_no_tiene_digest() {
        let entries = vec![
            (b"a.txt".to_vec(), Some(VACIO.to_owned())),
            (b"carpeta".to_vec(), None),
        ];
        assert_eq!(
            to_sums_bytes(&entries),
            format!("{VACIO}  a.txt\n").into_bytes()
        );
    }

    /// La propiedad que mata a la vez el lossy, el colapso de dos nombres en
    /// uno y la inyección de una línea forjada: **para todo nombre del corpus
    /// hostil, lo que se copia vuelve a leerse como el mismo nombre, byte a
    /// byte.**
    #[test]
    fn lo_copiado_vuelve_a_leerse_igual_para_todo_el_corpus_hostil() {
        for n in norte_testkit::corpus::hostile_names() {
            let entries = vec![(n.bytes.clone(), Some(VACIO.to_owned()))];
            let bytes = to_sums_bytes(&entries);
            let leido = parse_sums(&bytes);
            assert_eq!(leido.refused, 0, "[{}] {}", n.id, n.why);
            assert_eq!(
                leido.lines.len(),
                1,
                "[{}] una línea copiada es una línea leída: {}",
                n.id,
                n.why
            );
            assert_eq!(
                leido.lines[0].name, n.bytes,
                "[{}] el nombre tiene que volver EXACTO: {}",
                n.id, n.why
            );
        }
    }

    /// El caso concreto que la propiedad de arriba cubre y que más duele:
    /// dos nombres que se ven iguales al pasarlos por `String` siguen siendo
    /// dos líneas distintas.
    #[test]
    fn dos_nombres_que_colapsan_en_texto_no_colapsan_aqui() {
        let entries = vec![
            (b"\xFF.rs".to_vec(), Some(VACIO.to_owned())),
            (b"\xFE.rs".to_vec(), Some(ABC.to_owned())),
        ];
        let leido = parse_sums(&to_sums_bytes(&entries));
        assert_eq!(leido.lines.len(), 2);
        assert_ne!(
            leido.lines[0].name, leido.lines[1].name,
            "colapsarlos comprobaría el mismo fichero dos veces"
        );
    }

    /// Los cuatro motivos por los que una línea no sale «correcto» son cuatro
    /// veredictos distintos, porque se arreglan de formas distintas: el
    /// fichero no está, es un directorio, o el nombre no se puede escribir en
    /// este sistema. Colapsarlos en «falta» manda al lector a buscar donde no
    /// es.
    #[test]
    fn cada_motivo_tiene_su_veredicto() {
        use norte_proto::methods::ChecksumMiss;

        let publicado = parse_sums(
            format!("{VACIO}  ok\n{ABC}  cambiado\n{VACIO}  ausente\n{VACIO}  carpeta\n{VACIO}  \0malo\n")
                .as_bytes(),
        )
        .lines;
        let asked = [Some(0), Some(1), Some(2), Some(3), None];
        let calculado: Vec<Computed> = vec![
            (Some(VACIO.to_owned()), None),
            (Some(VACIO.to_owned()), None),
            (None, Some(ChecksumMiss::Unreadable)),
            (None, Some(ChecksumMiss::NotAFile)),
        ];
        assert_eq!(
            judge(&publicado, &asked, &calculado),
            vec![
                Verdict::Ok,
                Verdict::Mismatch,
                Verdict::Missing,
                Verdict::NotAFile,
                Verdict::Unnameable,
            ]
        );
    }

    /// Un `SHA256SUMS` que nombra `sub/dentro.txt` —lo que escribe
    /// `sha256sum -r`— habla de un fichero que está, y emparejar por nombre
    /// BASE lo daba por «falta». Se empareja por la posición en la petición.
    #[test]
    fn un_nombre_con_directorio_se_juzga_contra_su_propia_ruta() {
        let publicado =
            parse_sums(format!("{VACIO}  sub/dentro.txt\n{ABC}  otro/dentro.txt\n").as_bytes())
                .lines;
        let calculado: Vec<Computed> =
            vec![(Some(VACIO.to_owned()), None), (Some(ABC.to_owned()), None)];
        assert_eq!(
            judge(&publicado, &[Some(0), Some(1)], &calculado),
            vec![Verdict::Ok, Verdict::Ok],
            "dos ficheros con el mismo nombre base en carpetas distintas se \
             juzgan cada uno con SU digest"
        );
    }

    /// «Todos correctos» no se puede decir sobre las líneas que sobrevivieron
    /// al parser: las que se cayeron son justo las de los nombres raros, y son
    /// las que un atacante controlaría.
    #[test]
    fn una_linea_que_no_se_entendio_impide_cantar_verde() {
        let todo_bien = [Verdict::Ok, Verdict::Ok];
        assert_eq!(summarize(&todo_bien, 0), Summary::AllOk { n: 2 });
        assert_eq!(
            summarize(&todo_bien, 3),
            Summary::Unreadable { n: 2, refused: 3 },
            "con líneas ilegibles, el resumen tiene que decirlo antes que nada"
        );
        assert_eq!(
            summarize(&[Verdict::Ok, Verdict::Unnameable], 0),
            Summary::Bad { n: 1 },
            "lo que no se pudo mirar cuenta como no comprobado, jamás como bien"
        );
    }

    /// Un nombre con un salto de línea no puede partir la lista ni colar una
    /// entrada que nadie calculó.
    #[test]
    fn un_nombre_con_salto_de_linea_no_inyecta_una_entrada() {
        let hostil = format!("x\n{ABC}  forjado").into_bytes();
        let bytes = to_sums_bytes(&[(hostil.clone(), Some(VACIO.to_owned()))]);
        let leido = parse_sums(&bytes);
        assert_eq!(leido.lines.len(), 1, "una entrada dentro, una fuera");
        assert_eq!(leido.lines[0].name, hostil);
    }
}
