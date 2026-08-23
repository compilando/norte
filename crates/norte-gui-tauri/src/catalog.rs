//! Lo que el renderer necesita UNA vez: los textos y los colores, ya resueltos.
//!
//! Las dos cosas viajan resueltas EN RUST y por el mismo motivo. Los textos,
//! porque traducir es elegir plural, orden y forma, y hacerlo dos veces es
//! tener dos catálogos que divergen. Los colores, porque el tema es un
//! documento del proyecto con sus roles y sus fallbacks, y un renderer que se
//! los inventara pintaría otro norte.

use std::collections::BTreeMap;

use norte_i18n::Lang;
use norte_theme::{Role, Theme};
use norte_ui_host::{BRIDGE_VERSION, InstanceId};
use serde::{Deserialize, Serialize};

/// El paquete de arranque del renderer.
///
/// **Es el quinto mensaje del cable y el único que no vivía en
/// `norte-ui-host`**, así que ni el puente versionado ni su corpus golden lo
/// cubrían (#259). Sigue aquí —lo que lleva son textos traducidos y colores,
/// que son cosa de quien pinta y no del host—, pero ya no viaja sin red:
/// `Deserialize` y un caso en `tests/catalogo_wire.rs` clavan su forma.
///
/// Su `bridge_version` es INFORMATIVO. La compatibilidad la decide el
/// renderer sobre el sobre que está a punto de interpretar
/// (`session.ts`), que ya lleva la suya: confiar para eso en un mensaje
/// lateral sería creerse un número que no acompaña a los datos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCatalog {
    /// La versión del contrato que habla este host, para diagnóstico.
    ///
    /// No es lo que decide si el renderer sigue: eso lo dice el sobre.
    pub bridge_version: u32,
    /// La instancia viva. Un mensaje de otra no se interpreta.
    pub instance_id: String,
    /// El idioma negociado.
    pub locale: String,
    /// Clave Fluent → texto ya traducido.
    pub strings: BTreeMap<String, String>,
    /// Nombre de variable CSS (sin `--`) → color `#rrggbb`.
    pub theme: BTreeMap<String, String>,
    /// El renderer tiene que MEDIRSE en vez de esperar a un humano.
    ///
    /// Lo enciende `NORTE_GUI_MEASURE=1`, y solo sirve para la tarea 3.6: una
    /// pasada guionizada de teclas y scroll que apunta latencias y las manda
    /// por el comando `metrics` (que solo existe con la feature del mismo
    /// nombre, o sea, no en el binario que se publica).
    pub measure: bool,
}

/// Construye el paquete para esta instancia, este idioma y este tema.
#[must_use]
pub fn catalogo(instance: &InstanceId, lang: Lang, theme: &Theme) -> HostCatalog {
    let mut strings = BTreeMap::new();
    for id in norte_i18n::message_ids(lang) {
        let texto = norte_i18n::t_in(lang, &id);
        strings.insert(id, texto);
    }
    HostCatalog {
        bridge_version: BRIDGE_VERSION,
        instance_id: instance.as_str().to_owned(),
        locale: match lang {
            Lang::Es => "es".to_owned(),
            Lang::En => "en".to_owned(),
        },
        strings,
        theme: variables(theme),
        measure: std::env::var_os("NORTE_GUI_MEASURE").is_some_and(|v| v == "1"),
    }
}

/// Los roles del tema, como variables CSS.
///
/// La correspondencia es EXPLÍCITA y no automática: una variable de la hoja de
/// estilos que nadie alimenta se ve (queda el valor por defecto), pero un
/// volcado automático de `Role` a CSS convertiría cada rol nuevo en una
/// variable que nadie usa y cada rename en un color que desaparece sin ruido.
#[must_use]
pub fn variables(theme: &Theme) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut poner = |nombre: &str, role: Role, fondo: bool| {
        let style = theme.style(role);
        let color = if fondo { style.bg } else { style.fg };
        if let Some(c) = color {
            out.insert(nombre.to_owned(), c.to_hex());
        }
    };
    poner("bg", Role::Background, true);
    poner("fg", Role::Regular, false);
    poner("panel-bg", Role::PaneBackground, true);
    poner("panel-focus-bg", Role::PaneFocusBackground, true);
    poner("border", Role::BorderUnfocused, false);
    poner("border-focus", Role::BorderFocus, false);
    poner("selection-bg", Role::Selection, true);
    poner("selection-fg", Role::Selection, false);
    poner("mark-bg", Role::Mark, true);
    poner("hostile-fg", Role::HostileBadge, false);
    poner("status-bg", Role::StatusBar, true);
    poner("title-fg", Role::Title, false);
    poner("error-fg", Role::Error, false);
    // Los dos roles que una DECORACIÓN de plugin puede pedir además de
    // `error`. Sin ellos, una insignia `warning` caía al color del título y
    // era indistinguible de una `info`: el rol es vocabulario cerrado
    // justamente para que signifique algo en pantalla.
    poner("warning-fg", Role::Warning, false);
    poner("info-fg", Role::Info, false);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El catálogo trae texto TRADUCIDO, no la clave: el renderer no tiene
    /// catálogo propio que consultar.
    #[test]
    fn los_textos_vienen_resueltos() {
        let c = catalogo(&InstanceId::new("i"), Lang::Es, &Theme::preset_default());
        assert_eq!(c.bridge_version, BRIDGE_VERSION);
        assert!(!c.strings.is_empty(), "hay catálogo");
        for (clave, texto) in c.strings.iter().take(20) {
            assert!(!texto.is_empty(), "{clave} sin texto");
        }
    }

    /// Dos idiomas, dos catálogos: el que se manda es el negociado.
    #[test]
    fn el_idioma_manda() {
        let es = catalogo(&InstanceId::new("i"), Lang::Es, &Theme::preset_default());
        let en = catalogo(&InstanceId::new("i"), Lang::En, &Theme::preset_default());
        assert_eq!(es.locale, "es");
        assert_eq!(en.locale, "en");
        assert_ne!(es.strings, en.strings, "no es el mismo catálogo");
    }

    /// Los colores salen del tema, en la forma que el CSS entiende.
    #[test]
    fn los_colores_son_del_tema() {
        let t = Theme::preset("catppuccin-mocha")
            .expect("parsea")
            .expect("preset de fábrica");
        let v = variables(&t);
        for (nombre, valor) in &v {
            assert!(
                valor.starts_with('#') && valor.len() == 7,
                "{nombre} = {valor} no es #rrggbb"
            );
        }
        assert!(v.contains_key("fg"), "al menos el texto normal está");
    }
}
