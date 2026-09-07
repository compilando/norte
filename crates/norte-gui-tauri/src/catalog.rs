//! Lo que el renderer necesita UNA vez: los textos y los colores, ya resueltos.
//!
//! Las dos cosas viajan resueltas EN RUST y por el mismo motivo. Los textos,
//! porque traducir es elegir plural, orden y forma, y hacerlo dos veces es
//! tener dos catálogos que divergen. Los colores, porque el tema es un
//! documento del proyecto con sus roles y sus fallbacks, y un renderer que se
//! los inventara pintaría otro norte.

use std::collections::BTreeMap;

use norte_i18n::Lang;
use norte_theme::Theme;
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
    /// Cuánto se espera antes de ENSEÑAR que se está esperando, en ms.
    ///
    /// Viaja en vez de estar escrito en el CSS porque es una decisión
    /// compartida con el terminal: `norte_frontend::busy::THRESHOLD`, con su
    /// razonamiento —por debajo la operación acaba antes de que el ojo la
    /// registre y lo único que queda es un parpadeo—. Un número repetido en
    /// una hoja de estilos es el tercer sitio donde cambiarlo y el primero
    /// donde olvidarse.
    pub busy_threshold_ms: u64,
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
        busy_threshold_ms: u64::try_from(norte_frontend::busy::THRESHOLD.as_millis())
            .unwrap_or(250),
    }
}

/// Los roles del tema, como variables CSS.
///
/// La correspondencia vive en el HOST (`pickers::roles_de_tema`) desde que su
/// selector de tema elige: entonces el host tiene que resolver por nombre un
/// tema que nadie le pasó, y dos listas —una para pintar y otra para
/// enseñar— acabarían diciendo cosas distintas del mismo tema. Aquí solo se
/// le da la forma que la webview espera.
#[must_use]
pub fn variables(theme: &Theme) -> BTreeMap<String, String> {
    norte_ui_host::pickers::roles_de_tema(theme)
        .into_iter()
        .collect()
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
