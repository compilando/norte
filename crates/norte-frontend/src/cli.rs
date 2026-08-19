//! Parseo de la línea de comandos COMPARTIDO por los frontends interactivos
//! (TUI y GUI). Ninguno de los dos usa clap: son binarios cuyo arranque se
//! nota, y su superficie de argumentos es un puñado de flags. Compartirlo
//! evita que diverjan en lo que sí es contrato con la persona que teclea:
//! qué es el posicional, qué flags existen, y qué pasa con uno que no.
//!
//! Cada frontend DECLARA su superficie ([`parse`] recibe qué flags acepta),
//! así que un flag que no soporta sale por [`Cli::unknown`] y el binario
//! puede rechazarlo con un mensaje — jamás tragárselo en silencio, que es
//! como `--help` acababa muriendo dentro del inicializador del terminal.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

/// Lo que la línea de comandos pidió.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cli {
    /// Primer argumento posicional: el directorio de arranque. Se conserva
    /// como ruta cruda (regla 1: un nombre de dir no tiene por qué ser
    /// UTF-8). Los posicionales siguientes se ignoran.
    pub dir: Option<PathBuf>,
    /// Flags booleanos presentes, por nombre EXACTO (`--daemon`).
    pub flags: Vec<String>,
    /// Flags con valor, por nombre exacto (`--socket` → su valor crudo).
    pub values: BTreeMap<String, OsString>,
    /// Se pidió ayuda (`-h`/`--help`).
    pub help: bool,
    /// Se pidió la versión (`-V`/`--version`).
    pub version: bool,
    /// PRIMER flag no reconocido, tal cual se tecleó.
    pub unknown: Option<String>,
}

impl Cli {
    /// ¿Estaba presente este flag booleano?
    #[must_use]
    pub fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }

    /// Valor de un flag con valor, como `String` con conversión LOSSY —
    /// solo para valores que son texto por contrato (un nombre de preset).
    /// Para rutas usa [`Cli::path`], que no toca los bytes.
    #[must_use]
    pub fn text(&self, flag: &str) -> Option<String> {
        self.values
            .get(flag)
            .map(|v| v.to_string_lossy().into_owned())
    }

    /// Valor de un flag con valor, con los BYTES intactos.
    ///
    /// Para lo que no es texto por contrato aunque lo parezca: el nombre de
    /// una disposición acaba siendo `layouts/<nombre>.toml`, así que pasarlo
    /// por [`Cli::text`] cambiaba qué fichero se abre —dos bytes inválidos
    /// distintos aterrizaban en el mismo `\u{FFFD}.toml`— sin decir nada
    /// (#246).
    #[must_use]
    pub fn os_text(&self, flag: &str) -> Option<&std::ffi::OsStr> {
        self.values.get(flag).map(OsString::as_os_str)
    }

    /// Valor de un flag con valor, como ruta (bytes intactos).
    #[must_use]
    pub fn path(&self, flag: &str) -> Option<PathBuf> {
        self.values.get(flag).map(PathBuf::from)
    }
}

/// Parsea `argv` SIN `argv[0]` (el caller lo salta).
///
/// `bool_flags` y `value_flags` son la superficie que el frontend soporta;
/// `-h`/`--help` y `-V`/`--version` se reconocen siempre. Un flag fuera de
/// esas listas va a [`Cli::unknown`] en vez de ignorarse. Un flag con valor
/// sin valor detrás (`--socket` al final) se queda sin entrada, como si no
/// se hubiera pasado.
#[must_use]
pub fn parse<I, S>(argv: I, bool_flags: &[&str], value_flags: &[&str]) -> Cli
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut out = Cli::default();
    let mut it = argv.into_iter().map(Into::into);
    while let Some(arg) = it.next() {
        let a = arg.to_string_lossy().into_owned();
        match a.as_str() {
            "-h" | "--help" => out.help = true,
            "-V" | "--version" => out.version = true,
            s if bool_flags.contains(&s) => out.flags.push(s.to_owned()),
            s if value_flags.contains(&s) => {
                if let Some(v) = it.next() {
                    out.values.insert(s.to_owned(), v);
                }
            }
            // `-` a secas es un posicional por convención (stdin), no un flag.
            s if s.starts_with('-') && s != "-" => {
                if out.unknown.is_none() {
                    out.unknown = Some(s.to_owned());
                }
            }
            _ if out.dir.is_none() => out.dir = Some(PathBuf::from(arg)),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Cli, parse};

    const BOOL: &[&str] = &["--daemon"];
    const VALUE: &[&str] = &["--preset", "--socket"];

    /// El posicional es el DIR; los flags con valor se llevan el siguiente
    /// argumento; los booleanos se registran por nombre.
    #[test]
    fn posicional_flags_y_valores() {
        let c = parse(
            [
                "/tmp/x", "--preset", "vim", "--daemon", "--socket", "/run/s",
            ],
            BOOL,
            VALUE,
        );
        assert_eq!(c.dir.as_deref(), Some(std::path::Path::new("/tmp/x")));
        assert_eq!(c.text("--preset").as_deref(), Some("vim"));
        assert!(c.has("--daemon"));
        assert_eq!(
            c.path("--socket").as_deref(),
            Some(std::path::Path::new("/run/s"))
        );
        assert!(c.unknown.is_none());
    }

    /// `--help`/`--version` se reconocen SIEMPRE, los declare quien los
    /// declare: antes caían en «flag desconocido, ignora» y el binario
    /// seguía hasta morir tomando el terminal.
    #[test]
    fn ayuda_y_version_siempre() {
        for a in ["-h", "--help"] {
            assert!(parse([a], &[], &[]).help, "{a}");
        }
        for a in ["-V", "--version"] {
            assert!(parse([a], &[], &[]).version, "{a}");
        }
    }

    /// Un flag fuera de la superficie DECLARADA se nombra (el frontend lo
    /// rechaza), aunque otro frontend sí lo soporte — cada uno responde de
    /// lo suyo.
    #[test]
    fn flag_fuera_de_la_superficie_declarada() {
        let c = parse(["--daemon", "/tmp"], &[], VALUE);
        assert_eq!(c.unknown.as_deref(), Some("--daemon"));
        assert_eq!(c.dir.as_deref(), Some(std::path::Path::new("/tmp")));
        assert_eq!(
            parse(["--nope"], BOOL, VALUE).unknown.as_deref(),
            Some("--nope")
        );
    }

    /// Un flag con valor SIN valor detrás no inventa nada; sin argumentos,
    /// no se pide nada.
    #[test]
    fn valor_ausente_y_vacio() {
        let c = parse(["--socket"], BOOL, VALUE);
        assert!(c.path("--socket").is_none());
        assert_eq!(
            parse(std::iter::empty::<String>(), BOOL, VALUE),
            Cli::default()
        );
    }

    /// Regla 1: un dir con bytes no-UTF8 llega ENTERO (nada de lossy en el
    /// camino del dato).
    #[test]
    #[cfg(unix)]
    fn el_dir_no_utf8_sobrevive() {
        use std::os::unix::ffi::OsStringExt as _;
        let crudo = std::ffi::OsString::from_vec(b"/tmp/due\xffo".to_vec());
        let c = parse([crudo.clone()], BOOL, VALUE);
        assert_eq!(c.dir.as_deref(), Some(std::path::Path::new(&crudo)));
    }
}
