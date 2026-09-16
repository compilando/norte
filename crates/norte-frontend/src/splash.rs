//! La pantalla de arranque, compartida (spec 2026-09-15, fase 2).
//!
//! Qué dice el splash —qué build corre, contra qué daemon, y a dónde puedes ir
//! de un número— es la misma pregunta en el terminal y en la ventana, así que
//! se contesta una vez. Cada frontend pone los píxeles.
//!
//! Las secciones salen de un REGISTRO y no de una lista escrita a mano: una
//! fuente nueva (los populares, los favoritos, los perfiles… y mañana lo que
//! aporte un plugin) se añade implementando [`SplashSource`], sin tocar a
//! quien pinta. Una fuente sin nada que decir NO ocupa sitio.

/// Contra qué está hablando este frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Daemon {
    /// El core va dentro del proceso.
    Embedded,
    /// Hay un daemon y está conectado.
    Connected,
    /// Se está conectando, o reconectando.
    Connecting,
}

impl Daemon {
    /// La clave Fluent que lo dice.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Embedded => "splash-daemon-embedded",
            Self::Connected => "splash-daemon-connected",
            Self::Connecting => "splash-daemon-connecting",
        }
    }
}

/// Una fila del splash: lo que se lee, y el comando que corre si se elige.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplashRow {
    /// Lo que se lee, ya saneado por quien lo construye.
    pub label: String,
    /// El detalle a la derecha (una ruta, un número de visitas). Puede ir
    /// vacío.
    pub detail: String,
    /// El comando del catálogo que ejecuta la fila.
    pub command: String,
    /// Su argumento, si lo lleva (un directorio, un nombre de perfil).
    pub arg: Option<String>,
}

/// Un grupo de filas con su título.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplashSection {
    /// Clave Fluent del título: el splash no lleva prosa traducida dentro.
    pub title_key: &'static str,
    /// Las filas, en el orden en que se pintan.
    pub rows: Vec<SplashRow>,
}

/// Todo lo que el splash enseña.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplashView {
    /// El arte, una fila por línea ([`ART`]).
    pub art: &'static [&'static str],
    /// La versión del binario.
    pub version: String,
    /// La revisión de git con la que se compiló.
    pub revision: String,
    /// Contra qué core habla.
    pub daemon: Daemon,
    /// Las secciones, ya filtradas: ninguna viene vacía.
    pub sections: Vec<SplashSection>,
}

/// De dónde sale una sección del splash.
///
/// `None` = esta fuente no tiene nada que decir hoy (sin favoritos, sin
/// perfiles), y entonces no ocupa sitio en pantalla.
pub trait SplashSource {
    /// La sección de esta fuente, si tiene filas.
    fn section(&self) -> Option<SplashSection>;
}

/// Las secciones de las fuentes dadas, en su orden, saltándose las vacías.
///
/// ```
/// use norte_frontend::splash::{SplashRow, SplashSection, SplashSource, sections};
///
/// struct Vacia;
/// impl SplashSource for Vacia {
///     fn section(&self) -> Option<SplashSection> { None }
/// }
/// struct Una;
/// impl SplashSource for Una {
///     fn section(&self) -> Option<SplashSection> {
///         Some(SplashSection {
///             title_key: "splash-recent",
///             rows: vec![SplashRow {
///                 label: "casa".to_owned(),
///                 detail: String::new(),
///                 command: "nav.enter".to_owned(),
///                 arg: None,
///             }],
///         })
///     }
/// }
/// let fuentes: [&dyn SplashSource; 3] = [&Vacia, &Una, &Vacia];
/// let s = sections(&fuentes);
/// assert_eq!(s.len(), 1, "una fuente sin filas no ocupa sitio");
/// assert_eq!(s[0].title_key, "splash-recent");
/// ```
#[must_use]
pub fn sections(fuentes: &[&dyn SplashSource]) -> Vec<SplashSection> {
    fuentes
        .iter()
        .filter_map(|f| f.section())
        .filter(|s| !s.rows.is_empty())
        .collect()
}

/// Cuánto tapa el splash `brief` como MUCHO, en milisegundos.
///
/// Compartido porque es parte de lo que la pantalla PROMETE: «se ve, y se
/// quita sola». Dos plazos distintos serían dos arranques distintos, y el que
/// tardara más se leería como que esa superficie va más lenta.
///
/// No es de los temporizadores que prohíbe la ADR 0006 —aquello va de resolver
/// TECLAS—: aquí ninguna tecla espera al reloj, porque cualquiera lo quita
/// antes.
pub const BRIEF_MS: i64 = 1_200;

/// Cuántas filas del splash se pueden elegir por número.
///
/// Nueve, y no diez: `0` no es la décima de nada, y una lista que empieza en
/// `1` y acaba en `0` hay que leerla dos veces.
pub const NUMBERED: usize = 9;

/// Las filas numeradas, en el orden en que se pintan: `(número, fila)`.
///
/// Numera a través de las secciones, no dentro de cada una: lo que el lector
/// ve es una lista con números, y dos filas con el mismo número serían dos
/// teclas que hacen cosas distintas.
///
/// ```
/// use norte_frontend::splash::{SplashRow, SplashSection, numbered};
/// let fila = |l: &str| SplashRow {
///     label: l.to_owned(),
///     detail: String::new(),
///     command: "nav.enter".to_owned(),
///     arg: None,
/// };
/// let secciones = vec![
///     SplashSection { title_key: "a", rows: vec![fila("uno"), fila("dos")] },
///     SplashSection { title_key: "b", rows: vec![fila("tres")] },
/// ];
/// let n = numbered(&secciones);
/// assert_eq!(n[2].0, 3, "la numeración cruza las secciones");
/// assert_eq!(n[2].1.label, "tres");
/// ```
#[must_use]
pub fn numbered(secciones: &[SplashSection]) -> Vec<(u8, &SplashRow)> {
    secciones
        .iter()
        .flat_map(|s| s.rows.iter())
        .take(NUMBERED)
        .enumerate()
        .map(|(i, fila)| (u8::try_from(i + 1).unwrap_or(u8::MAX), fila))
        .collect()
}

/// La brújula: el arte del splash, una fila por línea.
///
/// Todas las filas miden lo MISMO en celdas — lo fija un test—, porque las dos
/// superficies la centran, y una fila más ancha que las demás sale torcida en
/// cuanto el centrado es por línea.
pub const ART: &[&str] = &[
    "╭───────────╮",
    "│     N     │",
    "│     ▲     │",
    "│  W ─┼─ E  │",
    "│     │     │",
    "│     S     │",
    "╰───────────╯",
];

#[cfg(test)]
mod tests {
    use super::*;

    struct Fija(&'static str, usize);

    impl SplashSource for Fija {
        fn section(&self) -> Option<SplashSection> {
            Some(SplashSection {
                title_key: self.0,
                rows: (0..self.1)
                    .map(|i| SplashRow {
                        label: format!("fila {i}"),
                        detail: String::new(),
                        command: "nav.enter".to_owned(),
                        arg: None,
                    })
                    .collect(),
            })
        }
    }

    #[test]
    fn el_registro_conserva_el_orden_y_se_salta_lo_vacio() {
        let (a, vacia, b) = (Fija("a", 2), Fija("vacia", 0), Fija("b", 1));
        let fuentes: [&dyn SplashSource; 3] = [&a, &vacia, &b];
        let s = sections(&fuentes);
        assert_eq!(
            s.iter().map(|x| x.title_key).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    /// El arte se CENTRA, así que una fila de otro ancho sale torcida.
    #[test]
    fn el_arte_mide_lo_mismo_en_todas_sus_filas() {
        let anchos: Vec<usize> = ART.iter().map(|l| crate::display::cells(l)).collect();
        assert!(
            anchos.windows(2).all(|w| w[0] == w[1]),
            "filas de anchos distintos: {anchos:?}"
        );
    }

    /// Nueve como mucho: la décima fila se pinta, pero sin número que la
    /// llame — un `0` detrás del `9` se lee dos veces.
    #[test]
    fn la_numeracion_se_para_en_nueve() {
        let muchas = Fija("muchas", 12);
        let fuentes: [&dyn SplashSource; 1] = [&muchas];
        let s = sections(&fuentes);
        let n = numbered(&s);
        assert_eq!(n.len(), NUMBERED);
        assert_eq!(n.last().expect("hay filas").0, 9);
    }
}
