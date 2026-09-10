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
// `PartialEq` sin `Eq`: `font_size` es un `f32` porque la configuración acepta
// una parte fraccionaria a propósito (un `14.5` escrito a mano se puede editar
// desde la pantalla de ajustes), y un float no es `Eq`. Aquí solo se compara
// en tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Lo que esta ventana pinta y no es color: fuentes y movimiento.
    pub appearance: Appearance,
    /// No hay `norte.toml` de usuario todavía (spec 2026-09-10): el renderer
    /// abre el asistente de primer arranque al pintar la primera foto. Lo
    /// decide el arranque, que es quien mira el disco; `NORTE_NO_WIZARD` lo
    /// apaga, como en el terminal. Con `default`: un catálogo anterior no lo
    /// trae, y no traerlo es «no es el primero».
    #[serde(default)]
    pub first_run: bool,
}

/// `[ui] font`, `mono_font`, `font_size` y `reduce_motion`, para el renderer.
///
/// Las cuatro se cargaban, se validaban, se ofrecían en la pantalla de ajustes
/// —con `applies_live: true`— y no las leía NADIE: en el terminal no aplican
/// (una terminal no elige su fuente) y en la ventana no llegaban a cruzar.
/// `reduce_motion` además es un compromiso de accesibilidad de la spec §17.
///
/// Viajan en el CATÁLOGO y no en la foto porque no son estado de pantalla:
/// son de arranque y de recarga, como el tema, y por el mismo camino se
/// aplican en caliente al cambiar de perfil.
///
/// Ningún campo lleva `skip_serializing_if`: ausente y `null` tienen que
/// significar lo mismo aquí —«no lo dice la configuración»— y la única forma
/// de garantizarlo es que el campo viaje siempre.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Appearance {
    /// Familia para el texto de interfaz. `None` = la del sistema.
    #[serde(default)]
    pub font: Option<String>,
    /// Familia monoespaciada, para lo que se alinea en columnas. `None` = la
    /// que trae la hoja de estilos.
    #[serde(default)]
    pub mono_font: Option<String>,
    /// Tamaño base en px, ya validado a `[8, 32]` por la configuración.
    ///
    /// No es solo texto más grande: la rejilla de esta ventana se reparte en
    /// CELDAS, así que el alto de fila y el ancho de columna salen de aquí. Un
    /// tamaño que solo cambiara la letra la dejaría desbordando su fila.
    #[serde(default)]
    pub font_size: Option<f32>,
    /// Quien pide menos movimiento no ve animaciones. `None` = manda lo que
    /// diga el sistema (`prefers-reduced-motion`), que es el default correcto:
    /// la configuración solo puede AÑADIR la petición, nunca contradecir a
    /// quien ya la hizo en su escritorio.
    #[serde(default)]
    pub reduce_motion: Option<bool>,
}

impl Appearance {
    /// Los cuatro escalares de `[ui]`, tal y como los dejó la configuración.
    #[must_use]
    pub fn de(cfg: &norte_config::CommonConfig) -> Self {
        Self {
            font: cfg.ui_font.clone(),
            mono_font: cfg.ui_mono_font.clone(),
            font_size: cfg.ui_font_size,
            reduce_motion: cfg.ui_reduce_motion,
        }
    }
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
        appearance: Appearance::default(),
        first_run: false,
    }
}

impl HostCatalog {
    /// El mismo catálogo con la apariencia que dice la configuración.
    ///
    /// Aparte de [`catalogo`] y no un parámetro más porque los sitios que
    /// construyen un catálogo sin configuración son casi todos —los tests— y
    /// un cuarto argumento que la mitad de los llamantes rellena con un
    /// `Default` es un argumento que se olvida en el que importa.
    #[must_use]
    pub fn con_apariencia(mut self, cfg: &norte_config::CommonConfig) -> Self {
        self.appearance = Appearance::de(cfg);
        self
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
