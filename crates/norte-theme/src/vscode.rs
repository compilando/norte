//! Un tema de Visual Studio Code, leído y proyectado sobre un [`Theme`]
//! (spec 2026-09-11, F5).
//!
//! Dos hechos dan forma a este módulo:
//!
//! 1. **Un tema de VS Code NO es una paleta completa.** `dark_modern.json`
//!    incluye `dark_plus.json`, que incluye `dark_vs.json`, y ninguno de los
//!    tres define `list.*` ni `scrollbarSlider.*`: esos viven en el registro
//!    de colores del editor. Por eso [`to_theme`] pinta SOBRE una base
//!    (`vscode-dark` o `vscode-light`) y lo que el tema calla lo pone ella, en
//!    vez de caer al monocromo.
//! 2. **Este módulo no toca el disco.** `include` se devuelve crudo y quien
//!    tiene el fichero —el binario— recorre la cadena y llama a
//!    [`VsCodeTheme::merge_under`]. `norte-theme` es un modelo puro.
//!
//! `tokenColors` y `semanticTokenColors` se IGNORAN: norte no colorea
//! sintaxis. Solo cuenta el bloque `colors`.
//!
//! ```
//! use norte_theme::{Role, Theme, vscode};
//!
//! let src = r##"{
//!     // Un tema del marketplace trae comentarios y comas colgantes.
//!     "type": "dark",
//!     "colors": { "editor.background": "#101010", },
//! }"##;
//! let tema = vscode::parse(src).unwrap();
//! let base = Theme::preset(tema.base_or_default().preset()).unwrap().unwrap();
//! let t = vscode::to_theme(&tema.colors, &base);
//! assert_eq!(t.style(Role::Background).bg.unwrap().to_hex(), "#101010");
//! ```

use std::collections::HashMap;

use serde::Deserialize;

use crate::color::Color;
use crate::role::Role;
use crate::style::Style;
use crate::theme::Theme;

/// Si el tema es claro u oscuro: decide sobre qué preset se pinta.
///
/// ```
/// use norte_theme::vscode::VsBase;
/// assert_eq!(VsBase::Light.preset(), "vscode-light");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsBase {
    /// `"dark"`, `"vs-dark"`, `"hc"`, `"hc-black"`.
    Dark,
    /// `"light"`, `"vs"`, `"hc-light"`.
    Light,
}

impl VsBase {
    /// El preset embebido que hace de base para este tipo.
    ///
    /// ```
    /// use norte_theme::{Theme, vscode::VsBase};
    /// assert!(Theme::preset(VsBase::Dark.preset()).unwrap().is_some());
    /// ```
    #[must_use]
    pub const fn preset(self) -> &'static str {
        match self {
            Self::Dark => "vscode-dark",
            Self::Light => "vscode-light",
        }
    }

    /// Lee el campo `"type"`. Un valor que no se reconoce es `None`, igual
    /// que su ausencia: ninguno de los dos dice nada.
    fn from_type(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "dark" | "vs-dark" | "hc" | "hc-black" | "hcdark" | "hc-dark" => Some(Self::Dark),
            "light" | "vs" | "hc-light" | "hclight" => Some(Self::Light),
            _ => None,
        }
    }
}

/// Un color de VS Code: RGB más un canal alfa que norte no tiene.
///
/// VS Code acepta `#rgb`, `#rgba`, `#rrggbb` y `#rrggbbaa`. El modelo de tema
/// de norte es RGB de 24 bits (ADR 0020), así que el alfa se guarda aquí y
/// [`to_theme`] lo COMPONE sobre el fondo del tema — lo mismo que se hizo a
/// mano con `scrollbar-slider` en los dos presets. Descartarlo sin más
/// convertiría un deslizador al 40 % en uno opaco y chillón.
///
/// ```
/// use norte_theme::vscode::VsColor;
/// let c = VsColor::parse("#ffffff80").unwrap();
/// assert_eq!(c.alpha, 0x80);
/// assert_eq!(VsColor::parse("#abc").unwrap().rgb.to_hex(), "#aabbcc");
/// assert!(VsColor::parse("rojo").is_none());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsColor {
    /// Los tres canales de color.
    pub rgb: Color,
    /// Opacidad, 255 = opaco.
    pub alpha: u8,
}

impl VsColor {
    /// Parsea las cuatro formas que admite VS Code. `None` si no es ninguna.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let hex = s.strip_prefix('#')?;
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        // Las formas cortas duplican cada dígito: `#abc8` = `#aabbcc88`.
        let largo: String = match hex.len() {
            3 | 4 => hex.chars().flat_map(|c| [c, c]).collect(),
            6 | 8 => hex.to_owned(),
            _ => return None,
        };
        let rgb = Color::parse(&format!("#{}", &largo[..6])).ok()?;
        let alpha = match largo.get(6..8) {
            Some(a) => u8::from_str_radix(a, 16).ok()?,
            None => 255,
        };
        Some(Self { rgb, alpha })
    }

    /// El color resultante de pintar este sobre `fondo` opaco.
    ///
    /// ```
    /// use norte_theme::{Color, vscode::VsColor};
    /// // #797979 al 40 % sobre #1f1f1f: el `scrollbar-slider` de vscode-dark.
    /// let c = VsColor { rgb: Color::rgb(0x79, 0x79, 0x79), alpha: 102 };
    /// assert_eq!(c.over(Color::rgb(0x1f, 0x1f, 0x1f)).to_hex(), "#434343");
    /// ```
    #[must_use]
    pub fn over(self, fondo: Color) -> Color {
        let a = u16::from(self.alpha);
        let mezcla = |c: u8, f: u8| -> u8 {
            let v = (u16::from(c) * a + u16::from(f) * (255 - a) + 127) / 255;
            // c,f ≤ 255 y a ≤ 255 ⇒ v ≤ 255: el try_from nunca falla.
            u8::try_from(v).unwrap_or(u8::MAX)
        };
        Color::rgb(
            mezcla(self.rgb.r, fondo.r),
            mezcla(self.rgb.g, fondo.g),
            mezcla(self.rgb.b, fondo.b),
        )
    }
}

/// Un tema de VS Code ya leído, con `include` todavía sin resolver.
#[derive(Debug, Clone, Default)]
pub struct VsCodeTheme {
    /// El `"name"` del JSON, si lo trae.
    pub name: Option<String>,
    /// El `"type"`. `None` si falta o no se reconoce; ver
    /// [`Self::base_or_default`].
    pub base: Option<VsBase>,
    /// El `"include"` tal cual, relativo al fichero que lo nombra.
    pub include: Option<String>,
    /// Los colores que parsean, por id de VS Code.
    pub colors: HashMap<String, VsColor>,
    /// Los ids cuyo valor no es un color, ordenados. VS Code los ignora y aquí
    /// también, pero se dicen: un tema que pierde colores en silencio se
    /// parece demasiado a un importador roto.
    pub ignored: Vec<String>,
}

impl VsCodeTheme {
    /// El tipo efectivo: el declarado, u oscuro, que es lo que asume VS Code.
    ///
    /// ```
    /// use norte_theme::vscode::{VsBase, VsCodeTheme};
    /// assert_eq!(VsCodeTheme::default().base_or_default(), VsBase::Dark);
    /// ```
    #[must_use]
    pub fn base_or_default(&self) -> VsBase {
        self.base.unwrap_or(VsBase::Dark)
    }

    /// Mete `padre` DEBAJO de este tema: el hijo gana cada color que ya
    /// tiene, el padre rellena el resto, y el `include` pasa a ser el del
    /// padre para seguir la cadena. El nombre es siempre el del hijo.
    ///
    /// ```
    /// use norte_theme::vscode::parse;
    /// let mut hijo = parse(r##"{"include":"p.json","colors":{"foreground":"#111111"}}"##).unwrap();
    /// let padre = parse(r##"{"type":"light","colors":{"foreground":"#999999","focusBorder":"#0000ff"}}"##).unwrap();
    /// hijo.merge_under(padre);
    /// assert_eq!(hijo.colors["foreground"].rgb.to_hex(), "#111111");
    /// assert!(hijo.colors.contains_key("focusBorder"));
    /// assert!(hijo.include.is_none());
    /// ```
    pub fn merge_under(&mut self, padre: VsCodeTheme) {
        for (id, color) in padre.colors {
            self.colors.entry(id).or_insert(color);
        }
        self.base = self.base.or(padre.base);
        self.include = padre.include;
        // Un inválido del padre que el hijo sí define no se ha perdido.
        let colors = &self.colors;
        self.ignored.extend(
            padre
                .ignored
                .into_iter()
                .filter(|id| !colors.contains_key(id)),
        );
        self.ignored.sort();
        self.ignored.dedup();
    }
}

/// Error al leer un tema de VS Code.
#[derive(Debug, thiserror::Error)]
pub enum VsCodeError {
    /// No es JSON (ni JSONC) válido, o su forma no es la de un tema.
    #[error("tema de VSCode inválido: {0}")]
    Json(#[from] serde_json::Error),
}

/// La forma del JSON, tolerante: un color que no es cadena no tumba el tema.
#[derive(Deserialize)]
struct Crudo {
    #[serde(default)]
    name: Option<serde_json::Value>,
    #[serde(default, rename = "type")]
    tipo: Option<serde_json::Value>,
    #[serde(default)]
    include: Option<serde_json::Value>,
    #[serde(default)]
    colors: Option<HashMap<String, serde_json::Value>>,
}

/// Una cadena del JSON, o nada: un `"name": 3` se trata como ausente.
fn cadena(v: Option<serde_json::Value>) -> Option<String> {
    match v {
        Some(serde_json::Value::String(s)) => Some(s),
        _ => None,
    }
}

/// Parsea un tema de VS Code (JSON con comentarios y comas colgantes).
///
/// Un `null` en `colors` cuenta como ausente; cualquier otro valor que no sea
/// un color va a [`VsCodeTheme::ignored`].
///
/// # Errors
/// [`VsCodeError::Json`] si el texto no es JSONC o no tiene forma de tema.
///
/// ```
/// let t = norte_theme::vscode::parse(r##"{"name":"X","colors":{"foreground":7}}"##).unwrap();
/// assert_eq!(t.ignored, ["foreground"]);
/// ```
pub fn parse(src: &str) -> Result<VsCodeTheme, VsCodeError> {
    let crudo: Crudo = serde_json::from_str(&strip_jsonc(src))?;
    let mut colors = HashMap::new();
    let mut ignored = Vec::new();
    for (id, valor) in crudo.colors.unwrap_or_default() {
        match &valor {
            serde_json::Value::Null => {}
            serde_json::Value::String(s) => match VsColor::parse(s) {
                Some(c) => {
                    colors.insert(id, c);
                }
                None => ignored.push(id),
            },
            _ => ignored.push(id),
        }
    }
    ignored.sort();
    Ok(VsCodeTheme {
        name: cadena(crudo.name),
        base: cadena(crudo.tipo).as_deref().and_then(VsBase::from_type),
        include: cadena(crudo.include),
        colors,
        ignored,
    })
}

/// JSONC → JSON: quita `//` y `/* */` y las comas colgantes.
///
/// A mano y no con una dependencia: son treinta líneas (regla 8). Un `//`
/// DENTRO de una cadena no es un comentario —`"https://…"` aparece en temas
/// reales—, así que la máquina sigue si está dentro de una. Los comentarios
/// se cambian por espacios y se conservan los saltos de línea, para que un
/// error de `serde_json` siga apuntando a la línea correcta.
fn strip_jsonc(src: &str) -> String {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut en_cadena = false;
    while let Some(c) = chars.next() {
        if en_cadena {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(escapado) = chars.next() {
                        out.push(escapado);
                    }
                }
                '"' => en_cadena = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                en_cadena = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut previo = '\0';
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                    }
                    if previo == '*' && c == '/' {
                        break;
                    }
                    previo = c;
                }
                out.push(' ');
            }
            ('}' | ']', _) => {
                // Coma colgante: la última cosa no blanca antes del cierre.
                let fin = out.trim_end().len();
                if out[..fin].ends_with(',') {
                    out.remove(fin - 1);
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Id de VS Code → rol de norte. El `bool` es `true` si el id llena el `bg`
/// del rol y `false` si llena el `fg`.
///
/// **El orden importa: si dos ids alimentan el mismo lado de un rol, gana el
/// que va DESPUÉS.** Así se escribe un respaldo: `editor.foreground` antes que
/// `foreground`, porque One Dark Pro —uno de los temas más instalados— no
/// define `foreground` y sin el respaldo su texto saldría del gris de la base.
///
/// Es la misma tabla por la que se transcribieron `vscode-dark` y
/// `vscode-light` (cada cabecera la repite), menos `widget.shadow`: una
/// sombra es alfa, y sin alfa la hoja de estilos de la ventana hace mejor
/// trabajo con su respaldo translúcido.
///
/// ```
/// use norte_theme::{Role, vscode::MAPPING};
/// assert!(MAPPING.contains(&("list.hoverBackground", Role::Hover, true)));
/// ```
pub const MAPPING: &[(&str, Role, bool)] = &[
    ("editor.background", Role::Background, true),
    ("editor.foreground", Role::Regular, false),
    ("foreground", Role::Regular, false),
    ("sideBar.background", Role::PaneBackground, true),
    ("editor.background", Role::PaneFocusBackground, true),
    ("list.activeSelectionBackground", Role::Selection, true),
    ("list.activeSelectionForeground", Role::Selection, false),
    (
        "list.inactiveSelectionBackground",
        Role::SelectionUnfocused,
        true,
    ),
    (
        "list.inactiveSelectionForeground",
        Role::SelectionUnfocused,
        false,
    ),
    ("list.hoverBackground", Role::Hover, true),
    ("focusBorder", Role::BorderFocus, false),
    ("focusBorder", Role::FocusBorder, false),
    ("panel.border", Role::BorderUnfocused, false),
    ("panel.border", Role::Separator, false),
    ("widget.border", Role::ModalBorder, false),
    ("statusBar.background", Role::StatusBar, true),
    ("statusBar.foreground", Role::StatusBar, false),
    ("sideBarSectionHeader.foreground", Role::Title, false),
    ("sideBarTitle.foreground", Role::Title, false),
    ("button.background", Role::Button, true),
    ("button.foreground", Role::Button, false),
    ("editor.findMatchBackground", Role::Match, true),
    ("editorError.foreground", Role::Error, false),
    ("errorForeground", Role::Error, false),
    ("editorWarning.foreground", Role::Warning, false),
    ("editorInfo.foreground", Role::Info, false),
    ("descriptionForeground", Role::Muted, false),
    ("badge.background", Role::Badge, true),
    ("badge.foreground", Role::Badge, false),
    ("input.background", Role::InputBackground, true),
    ("input.border", Role::InputBorder, false),
    ("editorWidget.background", Role::WidgetBackground, true),
    ("scrollbarSlider.background", Role::ScrollbarSlider, true),
];

/// Pinta `colors` sobre `base` y devuelve un tema COMPLETO.
///
/// Parte de un clon de `base`; por cada fila de [`MAPPING`] cuyo id está en
/// `colors`, sustituye ESE lado del rol y deja el otro y los atributos como
/// los tenía la base. Lo que el tema no dice —los roles sin id, `[files]`,
/// `hostile-badge`, `mark`— sigue siendo de la base. El `name` queda vacío:
/// el de la base mentiría, y lo pone quien importa.
///
/// Un color con alfa se compone sobre el fondo: el `editor.background` del
/// tema (compuesto a su vez sobre el de la base), o el de la base si el tema
/// no lo trae. Es una aproximación —un color de la barra lateral se ve
/// realmente sobre la barra lateral— y es la misma que se usó al transcribir
/// los presets.
///
/// ```
/// use std::collections::HashMap;
/// use norte_theme::{Role, Theme, vscode::{to_theme, VsColor}};
///
/// let base = Theme::preset("vscode-dark").unwrap().unwrap();
/// let mut colors = HashMap::new();
/// colors.insert("focusBorder".to_owned(), VsColor::parse("#ff0000").unwrap());
/// let t = to_theme(&colors, &base);
/// assert_eq!(t.style(Role::FocusBorder).fg.unwrap().to_hex(), "#ff0000");
/// assert_eq!(t.style(Role::Hover), base.style(Role::Hover));
/// ```
#[must_use]
pub fn to_theme<S: std::hash::BuildHasher>(
    colors: &HashMap<String, VsColor, S>,
    base: &Theme,
) -> Theme {
    let fondo_base = base
        .style(Role::Background)
        .bg
        .unwrap_or(Color::rgb(0, 0, 0));
    let fondo = colors
        .get("editor.background")
        .map_or(fondo_base, |c| c.over(fondo_base));

    let mut tema = base.clone();
    tema.name = None;
    for &(id, role, es_fondo) in MAPPING {
        let Some(color) = colors.get(id) else {
            continue;
        };
        // `roles.get`, no `style()`: el fallback monocromo de un rol que la
        // base no define (un `reverse`) no debe colarse bajo un color nuevo.
        let previo = tema.roles.get(&role).copied().unwrap_or_default();
        let lado = if id == "editor.background" {
            // Ya compuesto arriba: componerlo otra vez sobre sí mismo lo
            // aclararía una segunda vez.
            Style::new().bg(fondo)
        } else if es_fondo {
            Style::new().bg(color.over(fondo))
        } else {
            // Un primer plano translúcido se ve sobre el fondo de SU rol
            // (`statusBar.foreground` sobre `statusBar.background`), y la
            // tabla pone siempre el fondo de un rol antes que su frente.
            Style::new().fg(color.over(previo.bg.unwrap_or(fondo)))
        };
        tema.roles.insert(role, previo.overlay(lado));
    }
    tema
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dark() -> Theme {
        Theme::preset("vscode-dark")
            .expect("parsea")
            .expect("preset")
    }

    /// JSONC: un tema del marketplace trae comentarios y comas colgantes.
    /// `serde_json` no los acepta, así que hay que limpiarlos antes.
    #[test]
    fn parsea_jsonc_con_comentarios_y_coma_colgante() {
        let src = r##"{
            // el nombre
            "name": "Mío",
            "type": "dark",
            "colors": {
                "editor.background": "#1F1F1F", /* bloque */
                "foreground": "#CCCCCC",
            },
        }"##;
        let t = parse(src).expect("parsea");
        assert_eq!(t.name.as_deref(), Some("Mío"));
        assert_eq!(t.base, Some(VsBase::Dark));
        assert_eq!(t.colors.len(), 2);
    }

    /// Un `//` dentro de una cadena NO es un comentario: las URL aparecen en
    /// temas reales, y comérselas dejaría la cadena sin cerrar.
    #[test]
    fn una_barra_doble_dentro_de_una_cadena_no_es_comentario() {
        let src = r##"{"name": "https://x.y/*z*/", "colors": {"a": "#fff"}}"##;
        let t = parse(src).expect("parsea");
        assert_eq!(t.name.as_deref(), Some("https://x.y/*z*/"));
        assert_eq!(t.colors.len(), 1);
    }

    /// Una comilla escapada no cierra la cadena, y un comentario en un array
    /// —como los de `dark_plus.json`— tampoco rompe la coma colgante.
    #[test]
    fn escapes_y_comentarios_en_arrays() {
        let src = "{\"name\": \"a\\\"//b\", \"x\": [1, // c\n 2, ],\n}";
        let t = parse(src).expect("parsea");
        assert_eq!(t.name.as_deref(), Some("a\"//b"));
        // Un comentario ENTRE la coma colgante y el cierre.
        assert!(parse("{\"x\": [1, /* c */ ], \"k\": \",\" }").is_ok());
    }

    /// Un campo que no es cadena, o `colors: null`, no tumba el tema.
    #[test]
    fn campos_con_forma_rara_se_toleran() {
        let t = parse(r#"{"name": 3, "type": ["dark"], "include": {}, "colors": null}"#)
            .expect("parsea");
        assert!(t.name.is_none() && t.base.is_none() && t.include.is_none());
        assert!(t.colors.is_empty());
    }

    /// Un `editor.background` translúcido se compone UNA vez sobre la base.
    #[test]
    fn un_fondo_translucido_se_compone_una_sola_vez() {
        let base = dark();
        let src = r##"{"colors":{"editor.background":"#ffffff80"}}"##;
        let t = to_theme(&parse(src).unwrap().colors, &base);
        let esperado = VsColor::parse("#ffffff80")
            .unwrap()
            .over(base.style(Role::Background).bg.unwrap());
        assert_eq!(t.style(Role::Background).bg, Some(esperado));
        assert_eq!(t.style(Role::PaneFocusBackground).bg, Some(esperado));
    }

    /// Un frente translúcido se compone sobre el fondo de su propio rol.
    #[test]
    fn un_frente_translucido_se_compone_sobre_el_fondo_de_su_rol() {
        let src = r##"{"colors":{
            "editor.background":"#000000",
            "statusBar.background":"#ffffff",
            "statusBar.foreground":"#00000080"
        }}"##;
        let t = to_theme(&parse(src).unwrap().colors, &dark());
        let esperado = VsColor::parse("#00000080")
            .unwrap()
            .over(Color::rgb(255, 255, 255));
        assert_eq!(t.style(Role::StatusBar).fg, Some(esperado));
    }

    /// Un `#RRGGBBAA` de ocho dígitos es legal en VS Code, y `#rgba` también.
    #[test]
    fn las_cuatro_formas_de_color_parsean() {
        let src = r##"{"colors":{"a":"#000000","b":"#00000066","c":"#abc","d":"#abc8"}}"##;
        let t = parse(src).expect("parsea");
        assert_eq!(t.colors["b"].alpha, 0x66);
        assert_eq!(t.colors["d"].rgb.to_hex(), "#aabbcc");
        assert_eq!(t.colors["d"].alpha, 0x88);
        assert!(t.ignored.is_empty());
    }

    /// El alfa se COMPONE sobre el fondo del propio tema: el `#4e566660` del
    /// deslizador de One Dark Pro sobre su `#282c34`, no un `#4e5666` opaco.
    #[test]
    fn un_color_con_alfa_se_compone_sobre_el_fondo_del_tema() {
        let src = r##"{"colors":{
            "editor.background":"#282c34",
            "scrollbarSlider.background":"#4e566660"
        }}"##;
        let t = to_theme(&parse(src).unwrap().colors, &dark());
        let slider = t.style(Role::ScrollbarSlider).bg.unwrap();
        assert_eq!(
            slider,
            VsColor::parse("#4e566660")
                .unwrap()
                .over(Color::rgb(0x28, 0x2c, 0x34))
        );
        assert_ne!(slider.to_hex(), "#4e5666", "no se descarta el alfa sin más");
    }

    /// Un valor que no es color no tumba el tema: se ignora y se dice.
    #[test]
    fn un_color_invalido_se_ignora_y_se_lista() {
        let src = r##"{"colors":{"a":"transparent","b":"#12","c":null,"d":"#123456"}}"##;
        let t = parse(src).expect("parsea");
        assert_eq!(t.ignored, ["a", "b"], "null es ausente, no inválido");
        assert_eq!(t.colors.len(), 1);
    }

    /// `include` se DEVUELVE sin resolver: este módulo no toca el disco.
    #[test]
    fn el_include_se_devuelve_crudo() {
        let src = r#"{"include":"./dark_plus.json","type":"dark","colors":{}}"#;
        assert_eq!(
            parse(src).unwrap().include.as_deref(),
            Some("./dark_plus.json")
        );
    }

    /// El tipo de alto contraste y el claro se reconocen; uno desconocido es
    /// silencio, y el silencio es oscuro, como en VS Code.
    #[test]
    fn los_tipos_de_vscode() {
        let tipo = |s: &str| parse(&format!(r#"{{"type":"{s}"}}"#)).unwrap().base;
        assert_eq!(tipo("hc-black"), Some(VsBase::Dark));
        assert_eq!(tipo("hc-light"), Some(VsBase::Light));
        assert_eq!(tipo("vs"), Some(VsBase::Light));
        assert_eq!(tipo("sepia"), None);
        assert_eq!(parse("{}").unwrap().base_or_default(), VsBase::Dark);
    }

    /// El padre va DEBAJO: el hijo gana, el tipo se hereda si el hijo calla,
    /// y el include avanza al del padre.
    #[test]
    fn merge_under_el_hijo_gana_y_la_cadena_avanza() {
        let mut hijo = parse(r##"{"name":"h","include":"p","colors":{"a":"#111111"}}"##).unwrap();
        let padre = parse(
            r##"{"name":"p","type":"light","include":"abuelo","colors":{"a":"#999999","b":"#222222"}}"##,
        )
        .unwrap();
        hijo.merge_under(padre);
        assert_eq!(hijo.name.as_deref(), Some("h"));
        assert_eq!(hijo.base, Some(VsBase::Light));
        assert_eq!(hijo.include.as_deref(), Some("abuelo"));
        assert_eq!(hijo.colors["a"].rgb.to_hex(), "#111111");
        assert_eq!(hijo.colors["b"].rgb.to_hex(), "#222222");
    }

    /// Un tema que fija VEINTE claves produce un tema COMPLETO: lo que no
    /// dice lo pone la base (spec 2026-09-11, F5). Sin esto, importar del
    /// marketplace daría veinte colores y el resto en monocromo, que se lee
    /// como un importador roto.
    #[test]
    fn lo_que_el_tema_no_dice_lo_pone_la_base() {
        let base = dark();
        let mut colors = HashMap::new();
        colors.insert(
            "editor.background".to_owned(),
            VsColor::parse("#101010").unwrap(),
        );
        let t = to_theme(&colors, &base);
        assert_eq!(t.style(Role::Background).bg.unwrap().to_hex(), "#101010");
        assert_eq!(
            t.style(Role::Hover).bg,
            base.style(Role::Hover).bg,
            "un rol que el tema calla lo hereda de la base"
        );
        assert!(t.style(Role::Regular).fg.is_some());
        assert!(t.name.is_none(), "el nombre de la base mentiría");
        for &role in Role::CORE {
            let s = t.style(role);
            assert!(s.fg.is_some() || s.bg.is_some(), "{role:?} sin color");
        }
    }

    /// Un id que llena un lado no borra el otro ni los atributos de la base:
    /// `title` es negrita en `vscode-dark` y lo sigue siendo.
    #[test]
    fn un_lado_no_borra_el_otro() {
        let base = dark();
        let mut colors = HashMap::new();
        colors.insert(
            "button.background".to_owned(),
            VsColor::parse("#00ff00").unwrap(),
        );
        colors.insert(
            "sideBarTitle.foreground".to_owned(),
            VsColor::parse("#ff00ff").unwrap(),
        );
        let t = to_theme(&colors, &base);
        assert_eq!(t.style(Role::Button).fg, base.style(Role::Button).fg);
        assert!(t.style(Role::Title).bold);
    }

    /// El orden de [`MAPPING`] es el respaldo: `foreground` gana a
    /// `editor.foreground` si están los dos, y este suple si falta aquel.
    #[test]
    fn el_ultimo_id_de_la_tabla_gana() {
        let solo_editor = parse(r##"{"colors":{"editor.foreground":"#abb2bf"}}"##).unwrap();
        let t = to_theme(&solo_editor.colors, &dark());
        assert_eq!(t.style(Role::Regular).fg.unwrap().to_hex(), "#abb2bf");

        let ambos = parse(r##"{"colors":{"editor.foreground":"#abb2bf","foreground":"#cccccc"}}"##)
            .unwrap();
        let t = to_theme(&ambos.colors, &dark());
        assert_eq!(t.style(Role::Regular).fg.unwrap().to_hex(), "#cccccc");
    }

    /// `widget.shadow` no entra, a propósito: ver el rustdoc de [`MAPPING`].
    #[test]
    fn la_sombra_no_se_importa() {
        assert!(!MAPPING.iter().any(|(id, ..)| *id == "widget.shadow"));
    }

    /// `tokenColors` se IGNORA: norte no colorea sintaxis. Un importador que
    /// se tragase la mitad de su entrada en silencio sería un test verde que
    /// no prueba nada — por eso el rustdoc del módulo lo dice.
    #[test]
    fn token_colors_se_ignora() {
        let src = r#"{"type":"dark","colors":{},"tokenColors":[{"scope":"comment"}]}"#;
        assert!(parse(src).is_ok());
    }

    /// Lo que no es JSONC es un error, no un tema vacío.
    #[test]
    fn basura_es_error() {
        assert!(parse("esto no es json").is_err());
        assert!(parse(r##"{"colors": ["#fff"]}"##).is_err());
    }
}
