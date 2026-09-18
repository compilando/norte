//! El selector de PERFILES: qué filas hay y qué se dice de cada una.
//!
//! Hermano de [`crate::layout_picker`] a propósito, y con su misma disciplina:
//! vive aquí y no en un frontend por la regla 7, las filas llegan YA LEÍDAS
//! porque leer al pasar el cursor sería I/O en el bucle de eventos (#244), y
//! una fila que no se puede usar se ENSEÑA con su motivo en vez de
//! desaparecer — esconder un directorio que el lector creó es peor que
//! enseñarlo roto.
//!
//! Lo que este selector añade sobre aquél son dos avisos que la spec pide por
//! su nombre (`docs/superpowers/specs/2026-08-26-config-profiles-design.md`):
//! un perfil cuyo nombre no es UTF-8 no puede llevar estado (D4), y un nombre
//! que coincide con el de una disposición o un preset de teclado es una
//! trampa si no se dice.

use std::ffi::{OsStr, OsString};

/// Un perfil del disco, ya leído.
///
/// El `title` y el `problem` los resuelve quien tiene el disco delante: este
/// crate no abre directorios.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserProfile {
    /// El nombre del directorio, con sus bytes (rule 1, D4).
    pub name: OsString,
    /// Su `[profile] title`, si lo declara.
    pub title: Option<String>,
    /// Por qué no se pudo leer su `norte.toml`, cuando no se pudo.
    pub problem: Option<String>,
}

/// Qué otra cosa de norte se llama igual que un perfil.
///
/// Se AVISA por lo mismo que lo avisa el selector de disposiciones: son
/// ajustes distintos que comparten nombre, y sin la línea la coincidencia es
/// una trampa en vez de una comodidad. Elegir el perfil `far` no ata ni una
/// tecla del preset `far`.
///
/// Un enum y no dos `bool` porque es UNA pregunta —«¿este nombre significa
/// otra cosa en algún sitio?»— y quien pinta tiene que decir cuál, no dos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NameClash {
    /// Solo es un perfil.
    #[default]
    None,
    /// También hay una disposición de fábrica así.
    Layout,
    /// También hay un preset de teclado así.
    Keymap,
    /// Las dos cosas.
    Both,
}

/// Una fila del selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// El nombre con el que se activa. La IDENTIDAD del perfil.
    ///
    /// [`OsString`] y no `String`: es un nombre de DIRECTORIO y acaba en
    /// `profiles/<nombre>/`, así que pasarlo por texto cambia cuál se abre
    /// (#245, #246).
    pub name: OsString,
    /// Su `[profile] title`, para enseñar al lado del nombre. Nunca EN VEZ
    /// del nombre: dos perfiles pueden compartir título y seguir siendo dos.
    pub title: Option<String>,
    /// Si es el perfil activo ahora mismo.
    ///
    /// Sin esto el selector es una lista de nombres en la que no se sabe
    /// dónde estás.
    pub active: bool,
    /// Qué OTRA cosa se llama igual que este perfil.
    pub clash: NameClash,
    /// Si este perfil puede guardar dónde dejaste cada panel.
    ///
    /// `false` cuando su nombre no es UTF-8: la clave de las disposiciones
    /// guardadas es un objeto JSON, así que un nombre así vale para
    /// configuración y no puede llevar estado ni ser pegajoso (D4). Se dice
    /// ANTES de elegirlo, no después de perderlo.
    pub carries_state: bool,
    /// Por qué esta fila no se puede cargar, cuando no se puede.
    pub problem: Option<String>,
}

/// El selector de perfiles.
#[derive(Debug)]
pub struct ProfilePicker {
    rows: Vec<Row>,
    cursor: usize,
}

/// El perfil que sigue (o precede) al activo, girando por el final.
///
/// `None` cuando no hay a dónde ir: ni perfiles, o solo el que ya está activo
/// — girar sobre uno solo es un cambio que no cambia nada, y hacerlo pasar por
/// la secuencia entera tiraría y recargaría la pantalla para dejarla igual.
///
/// Sin perfil activo, `next` es el primero y `prev` el último: entrar por
/// cualquiera de los dos extremos es lo que espera quien todavía no ha elegido
/// ninguno.
///
/// Compartido a propósito: `profile.next` tiene que significar lo mismo en la
/// ventana y en el terminal, y dos copias de un giro con módulo son dos
/// órdenes distintas esperando a divergir (ADR 0077).
#[must_use]
pub fn next_profile(
    perfiles: &[UserProfile],
    activo: Option<&OsStr>,
    hacia_delante: bool,
) -> Option<OsString> {
    if perfiles.is_empty() {
        return None;
    }
    let Some(activo) = activo else {
        let i = if hacia_delante { 0 } else { perfiles.len() - 1 };
        return Some(perfiles[i].name.clone());
    };
    let actual = perfiles.iter().position(|p| p.name == activo)?;
    if perfiles.len() == 1 {
        return None;
    }
    let n = perfiles.len();
    let i = if hacia_delante {
        (actual + 1) % n
    } else {
        (actual + n - 1) % n
    };
    Some(perfiles[i].name.clone())
}

impl ProfilePicker {
    /// Abre el selector con los perfiles que se le pasen, ya leídos, y el
    /// nombre del que está activo.
    ///
    /// No hay perfiles «de fábrica»: a diferencia de las disposiciones, un
    /// perfil es siempre un directorio que el lector creó. Una lista vacía es
    /// una lista vacía, y quien pinta lo dice.
    #[must_use]
    pub fn open(profiles: Vec<UserProfile>, active: Option<&OsStr>) -> Self {
        let rows = profiles
            .into_iter()
            .map(|p| {
                let texto = p.name.to_str();
                let como_layout = texto.is_some_and(|n| crate::layout::presets::NAMES.contains(&n));
                let como_keymap = texto.is_some_and(|n| crate::keymap::presets::NAMES.contains(&n));
                Row {
                    active: active == Some(p.name.as_os_str()),
                    clash: match (como_layout, como_keymap) {
                        (true, true) => NameClash::Both,
                        (true, false) => NameClash::Layout,
                        (false, true) => NameClash::Keymap,
                        (false, false) => NameClash::None,
                    },
                    carries_state: texto.is_some(),
                    name: p.name,
                    title: p.title,
                    problem: p.problem,
                }
            })
            .collect();
        Self { rows, cursor: 0 }
    }

    /// Las filas, en orden.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Dónde está el cursor, acotado a las filas que hay.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor.min(self.rows.len().saturating_sub(1))
    }

    /// Sube.
    pub const fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja.
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    /// El nombre de la fila resaltada.
    #[must_use]
    pub fn chosen(&self) -> Option<&OsStr> {
        self.rows.get(self.cursor()).map(|r| r.name.as_os_str())
    }

    /// La fila resaltada entera.
    #[must_use]
    pub fn current(&self) -> Option<&Row> {
        self.rows.get(self.cursor())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perfil(name: &str) -> UserProfile {
        UserProfile {
            name: OsString::from(name),
            title: None,
            problem: None,
        }
    }

    fn perfiles(nombres: &[&str]) -> Vec<UserProfile> {
        nombres.iter().map(|n| perfil(n)).collect()
    }

    #[test]
    fn gira_por_el_final_en_los_dos_sentidos() {
        let p = perfiles(&["a", "b", "c"]);
        assert_eq!(
            next_profile(&p, Some(OsStr::new("c")), true).as_deref(),
            Some(OsStr::new("a")),
            "del último al primero"
        );
        assert_eq!(
            next_profile(&p, Some(OsStr::new("a")), false).as_deref(),
            Some(OsStr::new("c")),
            "y del primero al último"
        );
    }

    /// Girar sobre UN solo perfil no es un cambio: hacerlo pasar por la
    /// secuencia entera tiraría y recargaría la pantalla para dejarla igual.
    #[test]
    fn con_un_solo_perfil_activo_no_hay_a_donde_ir() {
        let p = perfiles(&["a"]);
        assert_eq!(next_profile(&p, Some(OsStr::new("a")), true), None);
        assert_eq!(next_profile(&p, Some(OsStr::new("a")), false), None);
    }

    /// Sin perfil activo se entra por el extremo que corresponda al sentido.
    #[test]
    fn sin_activo_se_entra_por_un_extremo() {
        let p = perfiles(&["a", "b", "c"]);
        assert_eq!(
            next_profile(&p, None, true).as_deref(),
            Some(OsStr::new("a"))
        );
        assert_eq!(
            next_profile(&p, None, false).as_deref(),
            Some(OsStr::new("c"))
        );
    }

    /// Un activo que ya no está en la lista —lo borraron con el programa
    /// abierto— no elige ninguno a ciegas: girar desde un sitio que no existe
    /// no tiene respuesta buena, y saltar al primero movería al lector a un
    /// perfil que no pidió.
    #[test]
    fn un_activo_que_ya_no_existe_no_elige_a_ciegas() {
        let p = perfiles(&["a", "b"]);
        assert_eq!(next_profile(&p, Some(OsStr::new("fantasma")), true), None);
    }

    #[test]
    fn sin_perfiles_no_hay_nada() {
        assert_eq!(next_profile(&[], None, true), None);
    }

    /// La fila del perfil ACTIVO se marca. Sin eso, el selector es una lista
    /// de nombres en la que no se sabe dónde estás.
    #[test]
    fn el_activo_se_marca() {
        let p = ProfilePicker::open(
            vec![perfil("work"), perfil("photos")],
            Some(OsStr::new("photos")),
        );
        assert!(!p.rows()[0].active);
        assert!(p.rows()[1].active, "photos es el activo");
    }

    /// Sin perfil activo no se marca ninguna: «ninguno» es un estado legítimo
    /// y no se disfraza de la primera fila.
    #[test]
    fn sin_activo_no_se_marca_ninguna() {
        let p = ProfilePicker::open(vec![perfil("work")], None);
        assert!(p.rows().iter().all(|r| !r.active));
    }

    /// D4: un nombre que no es UTF-8 vale para configuración y NO puede llevar
    /// estado, ni siquiera pegajoso. La fila lo dice ANTES de elegirlo, no
    /// después de perderlo. Y los bytes viajan intactos.
    #[test]
    #[cfg(unix)]
    fn un_nombre_no_utf8_se_lista_y_avisa_de_que_no_guarda_estado() {
        use std::os::unix::ffi::OsStringExt;

        let hostil = OsString::from_vec(vec![b'w', 0xFF, b'k']);
        let p = ProfilePicker::open(
            vec![UserProfile {
                name: hostil.clone(),
                title: None,
                problem: None,
            }],
            None,
        );
        let fila = &p.rows()[0];
        assert_eq!(fila.name, hostil, "los bytes intactos");
        assert!(!fila.carries_state);
        assert_eq!(p.chosen(), Some(hostil.as_os_str()));
    }

    /// Un perfil que no parsea SE LISTA, con su motivo: esconder un directorio
    /// que el lector creó es peor que enseñarlo roto, y es lo que hace el
    /// selector de disposiciones con un layout ilegible.
    #[test]
    fn un_perfil_roto_se_lista_con_su_motivo() {
        let p = ProfilePicker::open(
            vec![UserProfile {
                name: OsString::from("work"),
                title: None,
                problem: Some("línea 3: unknown field `them`".to_owned()),
            }],
            None,
        );
        assert_eq!(p.rows().len(), 1, "la fila no desaparece");
        assert!(p.rows()[0].problem.is_some());
    }

    /// Compartir nombre con una disposición o con un preset de teclado se
    /// AVISA: son tres ajustes distintos, y la coincidencia es una trampa si
    /// no se dice.
    #[test]
    fn una_coincidencia_de_nombre_se_avisa() {
        let p = ProfilePicker::open(vec![perfil("orthodox"), perfil("far"), perfil("mío")], None);
        assert_eq!(
            p.rows()[0].clash,
            NameClash::Both,
            "orthodox es las dos cosas"
        );
        assert_eq!(
            p.rows()[1].clash,
            NameClash::Keymap,
            "far es un preset de teclado y no una disposición"
        );
        assert_eq!(p.rows()[2].clash, NameClash::None);
    }

    /// El cursor se acota a las filas que hay, y una lista vacía no panica.
    #[test]
    fn el_cursor_se_acota() {
        let mut p = ProfilePicker::open(vec![perfil("a"), perfil("b")], None);
        p.down();
        p.down();
        p.down();
        assert_eq!(p.cursor(), 1, "no se sale por abajo");
        p.up();
        p.up();
        assert_eq!(p.cursor(), 0, "ni por arriba");

        let vacio = ProfilePicker::open(Vec::new(), None);
        assert_eq!(vacio.cursor(), 0);
        assert_eq!(vacio.chosen(), None);
        assert!(vacio.current().is_none());
    }
}
