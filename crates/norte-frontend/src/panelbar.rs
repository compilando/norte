//! La barra de paneles: qué paneles hay, en qué orden y cómo están.
//!
//! Los paneles laterales —sitios, árbol, procesos, el registro— se abren por
//! atajo, por el menú o por la paleta, y los tres caminos exigen SABER que el
//! panel existe. No había ninguna superficie que los enseñara, así que un panel
//! nuevo era invisible para quien no leyera el changelog.
//!
//! Y hay un motivo que va más allá de la comodidad: `layout::kinds` es un
//! registro ABIERTO —un plugin puede aportar un tipo de panel—, y un panel
//! aportado que no aparece en ninguna parte no lo descubre nadie. Por eso esto
//! se DERIVA del registro y no de una lista escrita a mano: el día que un
//! plugin aporte un kind, sale solo.
//!
//! La decisión vive aquí y no en cada frontend (ADR 0077): qué entra en la
//! barra y en qué orden se decide una vez, y la TUI y la ventana solo pintan.

use crate::layout::KindRegistry;

/// Kinds que NO son paneles que se abren y se cierran.
///
/// `browser` es el listado (siempre hay uno), `tasks` y `status` son franjas
/// que se miran y no se enfocan, y `compare`/`sync` los abre una operación, no
/// un botón. Un botón que no puede abrir ni cerrar nada no es un botón.
const ESTRUCTURALES: &[&str] = &["browser", "tasks", "status", "compare", "sync"];

/// El comando que abre y cierra cada panel de serie.
///
/// Tabla y no convención para estos porque sus nombres son históricos:
/// `viewer` lo abre `layout.preview` y `tree` lo abre `pane.tree`. Para lo que
/// no está aquí se usa la convención `layout.<kind>`, que es la que tendría que
/// seguir un plugin que aporte un panel.
const TOGGLES: &[(&str, &str)] = &[
    ("places", "layout.places"),
    ("tree", "pane.tree"),
    ("viewer", "layout.preview"),
    ("processes", "layout.processes"),
    ("metadata", "layout.metadata"),
    ("log", "layout.log"),
];

/// Cómo está un panel ahora mismo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelState {
    /// Ni siquiera está en la disposición.
    Closed,
    /// Está abierto, pero el teclado lo tienen los listados u otro panel.
    Open,
    /// Está abierto Y tiene el teclado.
    Focused,
}

/// Un botón de la barra.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelButton {
    /// El kind que abre.
    pub kind: String,
    /// El comando que lo abre y lo cierra.
    pub command: String,
    /// La letra que se pinta.
    pub letter: char,
    /// Cómo está.
    pub state: PanelState,
    /// Tiene algo que contar (errores nuevos, tareas vivas).
    pub attention: bool,
}

/// Lo que la barra necesita saber del momento.
#[derive(Debug, Default, Clone, Copy)]
pub struct PanelBarInput<'a> {
    /// Kinds que están colocados en la disposición.
    pub open: &'a [&'a str],
    /// El kind que tiene el teclado, si es un panel.
    pub focused: Option<&'a str>,
    /// Kinds con novedad.
    pub attention: &'a [&'a str],
}

/// Los botones de la barra, en el orden del registro.
///
/// De serie primero —el registro los declara antes— y lo que aporte un plugin
/// detrás, según se instale. La posición de los de siempre no baila nunca, que
/// es lo que permite aprenderla con el dedo.
#[must_use]
pub fn buttons(reg: &KindRegistry, input: PanelBarInput<'_>) -> Vec<PanelButton> {
    buttons_con(reg, input, norte_i18n::t)
}

/// [`buttons`] en un idioma DICHO: la letra sale del nombre corto, así que
/// una ventana que traduce con el idioma de su sesión tiene que derivarla
/// del MISMO nombre que enseña, o «Sitios» llevaría la `P` de «Places».
#[must_use]
pub fn buttons_in(
    reg: &KindRegistry,
    input: PanelBarInput<'_>,
    lang: norte_i18n::Lang,
) -> Vec<PanelButton> {
    buttons_con(reg, input, |clave| norte_i18n::t_in(lang, clave))
}

fn buttons_con(
    reg: &KindRegistry,
    input: PanelBarInput<'_>,
    t: impl Fn(&str) -> String,
) -> Vec<PanelButton> {
    let mut out: Vec<PanelButton> = Vec::new();
    for decl in reg.decls() {
        let id = decl.id.as_str();
        if !es_boton(decl) {
            continue;
        }
        let command = TOGGLES
            .iter()
            .find(|(k, _)| *k == id)
            .map_or_else(|| format!("layout.{id}"), |(_, c)| (*c).to_string());
        let letter = letra(&nombre_con(id, &command, &t), id, &out);
        let abierto = input.open.contains(&id);
        let state = if !abierto {
            PanelState::Closed
        } else if input.focused == Some(id) {
            PanelState::Focused
        } else {
            PanelState::Open
        };
        out.push(PanelButton {
            kind: id.to_string(),
            command,
            letter,
            state,
            attention: input.attention.contains(&id),
        });
    }
    out
}

/// ¿Este kind merece un botón?
///
/// Una sola copia del criterio, y eso importa: los tests que comprueban que
/// cada panel tiene nombre corto en los dos idiomas lo usan también, así que
/// añadir un kind al registro y olvidar su traducción pone un test rojo en vez
/// de pintarle a un lector en español la inicial del id en inglés.
#[must_use]
pub fn es_boton(decl: &crate::layout::KindDecl) -> bool {
    // Un panel de la barra es uno que se enfoca: los que solo se miran no
    // tienen nada que hacer aquí.
    !ESTRUCTURALES.contains(&decl.id.as_str()) && decl.focusable
}

/// El nombre CORTO del panel, en un idioma DICHO.
///
/// Clave propia (`panelbar-<kind>`) y no la etiqueta del menú, que es una
/// frase: «Panel de sitios», «Panel de detalles» y «Panel de procesos» empiezan
/// las tres por `P`, así que sus iniciales no distinguen nada. Un nombre corto
/// es un dato distinto de una entrada de menú, y esto lo trata como tal.
///
/// Sin clave —un panel aportado por un plugin— cae a la etiqueta del menú, y
/// sin ella al id del kind: nunca a nada, porque un botón sin letra no es un
/// botón.
///
/// En un idioma dicho y no en el global porque la ventana traduce con el de
/// su sesión (`t_in`), y una etiqueta que saliera del global diría otro
/// idioma que el resto de su cromo. La TUI pasa por [`buttons`], que usa el
/// global.
#[must_use]
pub fn label_in(lang: norte_i18n::Lang, kind: &str, command: &str) -> String {
    nombre_con(kind, command, |clave| norte_i18n::t_in(lang, clave))
}

fn nombre_con(kind: &str, command: &str, t: impl Fn(&str) -> String) -> String {
    // Comparación con la CLAVE, no `starts_with`: el contrato de `t` es que
    // devuelve el id cuando el mensaje falta, y `starts_with("panelbar-")`
    // también dispararía con una traducción presente cuyo texto empezara por
    // ese literal.
    let clave = format!("panelbar-{kind}");
    let propia = t(&clave);
    if propia != clave {
        return propia;
    }
    // La etiqueta del menú, para un kind que la tenga y no lo otro. Hoy no
    // llega aquí nadie: los mensajes de un plugin no se funden en el paquete
    // de traducciones, así que un kind aportado cae siempre al id. Se queda
    // como escalón RESERVADO para cuando un plugin pueda registrar mensajes.
    let clave_menu = format!("menu-item-{}", command.replace('.', "-"));
    let del_menu = t(&clave_menu);
    if del_menu == clave_menu {
        kind.to_string()
    } else {
        del_menu
    }
}

/// La letra de un botón: la inicial de su NOMBRE, en el idioma del lector.
///
/// Y no la del atajo, que fue la primera versión y se cayó al pintarla: las
/// letras salían de verdad del keymap —`B` de `alt+b`, `Q` de `alt+q`— así que
/// eran inequívocas sobre qué pulsar y mudas sobre qué abría cada una. Una
/// barra que existe para que descubras que los paneles están ahí solo la
/// entendía quien ya se los sabía. El atajo lo enseña el menú, que lista cada
/// panel con su acorde al lado; esta fila enseña que EXISTEN.
///
/// Se desduplica: dos botones con la misma letra no se distinguen, así que el
/// segundo pasa a la siguiente letra libre de su propio nombre y, si se agotan,
/// del alfabeto.
fn letra(nombre: &str, kind: &str, ya: &[PanelButton]) -> char {
    let candidatas = nombre
        .chars()
        .chain(kind.chars())
        .chain('a'..='z')
        // Y los dígitos ANTES del interrogante: un `7` no dice qué panel es,
        // pero al menos distingue dos botones, y `?` no distingue nada.
        .chain('0'..='9')
        // Alfanuméricas, que incluye acentos y eñes: la inicial de «Árbol» es
        // una `Á` y pintarla es correcto.
        .filter(|c| c.is_alphanumeric())
        // `to_uppercase` puede dar varias (la `ß`); se coge la primera.
        .filter_map(|c| c.to_uppercase().next())
        // De UNA celda: el botón mide tres y las zonas pulsables se calculan
        // con ese número. Un carácter ancho —el id de un kind aportado podría
        // llevarlo, y `KindId::new` no valida— pintaría cuatro y desplazaría
        // una columna todos los botones de su derecha respecto a sus zonas.
        .filter(|c| unicode_width::UnicodeWidthChar::width(*c) == Some(1));
    for c in candidatas {
        if !ya.iter().any(|b| b.letter == c) {
            return c;
        }
    }
    // Último recurso, y desduplicado también: dos botones `?` no se
    // distinguen entre sí, que es peor que uno solo que no dice nada.
    if ya.iter().any(|b| b.letter == '?') {
        '·'
    } else {
        '?'
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{KindDecl, KindId};

    fn registro() -> KindRegistry {
        KindRegistry::builtin()
    }

    /// La barra enseña TODO panel que se abre y se cierra, y ninguno de los
    /// que no. Es la razón de existir: un panel que no sale aquí solo lo
    /// encuentra quien ya sabía que estaba.
    #[test]
    fn estan_los_paneles_y_no_lo_estructural() {
        let b = buttons(&registro(), PanelBarInput::default());
        let kinds: Vec<&str> = b.iter().map(|x| x.kind.as_str()).collect();
        // El ORDEN exacto, y no solo la pertenencia: la posición es lo que el
        // lector aprende con el dedo, así que reordenar `builtin()` por un
        // motivo ajeno tiene que caer AQUÍ —con este mensaje— y no en cuarenta
        // snapshots de render que no se explican solos.
        assert_eq!(
            kinds,
            ["places", "viewer", "processes", "metadata", "tree", "log"],
            "cambió el orden de los botones de serie"
        );
        for fuera in ["browser", "tasks", "status", "compare", "sync"] {
            assert!(
                !kinds.contains(&fuera),
                "«{fuera}» no es un panel que se abra: {kinds:?}"
            );
        }
    }

    /// Un kind aportado DESPUÉS —lo que haría un plugin— sale solo, y al
    /// final: la posición de los de serie no puede bailar porque alguien
    /// instale algo.
    #[test]
    fn un_kind_aportado_aparece_el_ultimo() {
        let mut reg = registro();
        let antes = buttons(&reg, PanelBarInput::default());
        reg.insert(KindDecl {
            id: KindId::new("gitlog"),
            min: (20, 4),
            focusable: true,
            takes_keys: true,
            multi: false,
            roles: &[],
        });
        let despues = buttons(&reg, PanelBarInput::default());
        assert_eq!(
            despues.len(),
            antes.len() + 1,
            "el kind aportado no salió: {despues:?}"
        );
        let ultimo = despues.last().expect("hay botones");
        assert_eq!(ultimo.kind, "gitlog");
        // Y por convención lo abre `layout.<kind>`, que es lo que tendría que
        // declarar el plugin.
        assert_eq!(ultimo.command, "layout.gitlog");
        // Los de serie siguen donde estaban.
        assert_eq!(
            despues[..antes.len()]
                .iter()
                .map(|b| &b.kind)
                .collect::<Vec<_>>(),
            antes.iter().map(|b| &b.kind).collect::<Vec<_>>()
        );
    }

    /// La letra es la inicial del NOMBRE del panel, en el idioma del lector, y
    /// sale del mismo sitio que la etiqueta del menú.
    ///
    /// La primera versión la sacaba del atajo, y se cayó al pintarla: `B Q J M
    /// T L` era inequívoco sobre qué pulsar y mudo sobre qué abría cada tecla.
    /// Una barra que existe para descubrir los paneles no puede exigir
    /// conocerlos.
    /// La letra es la inicial del nombre, y cambia con el idioma porque el
    /// nombre cambia: «Registro» da `R` y «Log» da `L`.
    ///
    /// Sobre la función PURA y no sobre `buttons`, que lee el idioma global:
    /// ese global se fija una vez por proceso, así que un test que lo forzara
    /// dependería de quién lo forzó antes — y bajo `cargo test`, que comparte
    /// proceso, eso es una carrera.
    #[test]
    fn la_letra_es_la_inicial_del_nombre() {
        assert_eq!(letra("Sitios", "places", &[]), 'S');
        assert_eq!(letra("Procesos", "processes", &[]), 'P');
        assert_eq!(letra("Registro", "log", &[]), 'R');
        assert_eq!(letra("Log", "log", &[]), 'L');
        // Sin nombre traducible, el id del kind; y si tampoco, el alfabeto:
        // un botón sin letra no es un botón.
        assert_eq!(letra("", "gitlog", &[]), 'G');
    }

    /// Cada panel tiene nombre corto en LOS DOS idiomas.
    ///
    /// Sin él, la letra cae a la etiqueta del menú, que es una frase: «Panel de
    /// sitios», «Panel de detalles» y «Panel de procesos» empiezan las tres por
    /// `P` y la barra dejaría de distinguir nada.
    #[test]
    fn cada_panel_tiene_nombre_corto_en_los_dos_idiomas() {
        // Del REGISTRO y no de una lista escrita aquí: con la lista, añadir un
        // kind y olvidar su traducción dejaba este test verde y le pintaba a un
        // lector en español la inicial del id en inglés.
        let reg = registro();
        let paneles: Vec<&str> = reg
            .decls()
            .iter()
            .filter(|d| es_boton(d))
            .map(|d| d.id.as_str())
            .collect();
        assert!(
            paneles.len() >= 6,
            "el registro perdió paneles: {paneles:?}"
        );
        for kind in paneles {
            for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
                let clave = format!("panelbar-{kind}");
                let nombre = norte_i18n::t_in(lang, &clave);
                assert_ne!(nombre, clave, "{lang:?}: falta «{clave}»");
            }
        }
    }

    /// Dos botones no pueden compartir letra: serían indistinguibles.
    #[test]
    fn las_letras_no_se_repiten() {
        let b = buttons(&registro(), PanelBarInput::default());
        let mut vistas = Vec::new();
        for x in &b {
            assert!(
                !vistas.contains(&x.letter),
                "«{}» repite la letra {}: {b:?}",
                x.kind,
                x.letter
            );
            vistas.push(x.letter);
        }
    }

    /// Tres estados distintos, y el foco gana a estar abierto: un botón que
    /// solo dijera «abierto» no diría dónde está el teclado, que es la mitad
    /// de lo que se pregunta al mirar la barra.
    #[test]
    fn cerrado_abierto_y_con_el_teclado_se_distinguen() {
        let abiertos = ["places", "log"];
        let b = buttons(
            &registro(),
            PanelBarInput {
                open: &abiertos,
                focused: Some("log"),
                attention: &["processes"],
            },
        );
        let de = |k: &str| b.iter().find(|x| x.kind == k).expect("está").clone();
        assert_eq!(de("places").state, PanelState::Open);
        assert_eq!(de("log").state, PanelState::Focused);
        assert_eq!(de("tree").state, PanelState::Closed);
        // Y la novedad es independiente de estar abierto: un panel cerrado con
        // algo que contar es justo el caso que hace mirar la barra.
        assert!(de("processes").attention);
        assert_eq!(de("processes").state, PanelState::Closed);
        assert!(!de("log").attention);
    }
}
