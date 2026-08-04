//! Cabecera de un tema: TOML entre vallas `+++` (ADR 0040 decisión 2). TOML
//! y no YAML porque el workspace ya parsea TOML en todas partes y meter un
//! crate de YAML por seis campos no pasaría la regla 8.

// El módulo es privado y todavía no lo consume nadie fuera de sus tests: lo
// hará `parse` en la tarea 5 de esta fase. Es `expect` y no `allow` a
// propósito, para que el día que `parse` lo use la expectativa quede sin
// cumplir y el compilador obligue a borrar esta línea; y va bajo
// `not(test)` porque en la compilación de tests los tests SÍ lo usan.
#![cfg_attr(not(test), expect(dead_code))]

use serde::Deserialize;

/// Valla que abre y cierra la cabecera.
const FENCE: &str = "+++";

/// Cabecera declarada por un tema.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrontMatter {
    /// Id único del tema.
    pub id: String,
    /// Título mostrado.
    pub title: String,
    /// Etiquetas de agrupación.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Temas relacionados.
    #[serde(default)]
    pub see_also: Vec<String>,
    /// Comandos documentados por el tema.
    #[serde(default)]
    pub commands: Vec<String>,
    /// Contextos de UI que abren este tema con F1.
    #[serde(default)]
    pub context: Vec<String>,
}

/// Fallos al leer la cabecera.
#[derive(Debug, thiserror::Error)]
pub enum FrontMatterError {
    /// El fichero no empieza por `+++`.
    #[error("el tema no empieza con la valla `+++`")]
    Missing,
    /// Falta la valla de cierre.
    #[error("la cabecera `+++` no se cierra")]
    Unterminated,
    /// TOML inválido o campo desconocido.
    #[error("cabecera TOML inválida: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Separa `(cabecera, cuerpo)`. El cuerpo se devuelve tal cual, sin
/// interpretar: quien lo parsea es el módulo `parse` (tarea 5).
///
/// # Errors
/// [`FrontMatterError`] si faltan vallas o el TOML no valida.
pub fn split(source: &str) -> Result<(FrontMatter, &str), FrontMatterError> {
    let rest = source
        .strip_prefix(FENCE)
        .and_then(|r| r.strip_prefix('\n'))
        .ok_or(FrontMatterError::Missing)?;
    let end = rest.find("\n+++").ok_or(FrontMatterError::Unterminated)?;
    let header = &rest[..end];
    let body = rest[end + 1 + FENCE.len()..]
        .strip_prefix('\n')
        .unwrap_or("");
    let fm: FrontMatter = toml::from_str(header)?;
    Ok((fm, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "+++\n\
id = \"copying\"\n\
title = \"Copying across backends\"\n\
tags = [\"doing\"]\n\
see_also = [\"selection\"]\n\
commands = [\"fs.copy\"]\n\
+++\n\
Body starts here.\n";

    #[test]
    fn separa_cabecera_y_cuerpo() {
        let (fm, body) = split(SRC).expect("front matter válido");
        assert_eq!(fm.id, "copying");
        assert_eq!(fm.title, "Copying across backends");
        assert_eq!(fm.tags, vec!["doing".to_owned()]);
        assert_eq!(fm.see_also, vec!["selection".to_owned()]);
        assert_eq!(fm.commands, vec!["fs.copy".to_owned()]);
        assert!(fm.context.is_empty(), "campo opcional, por defecto vacío");
        assert_eq!(body, "Body starts here.\n");
    }

    #[test]
    fn sin_valla_de_apertura_es_error() {
        let err = split("id = \"x\"\nbody").unwrap_err();
        assert!(matches!(err, FrontMatterError::Missing));
    }

    #[test]
    fn valla_sin_cerrar_es_error() {
        let err = split("+++\nid = \"x\"\nbody\n").unwrap_err();
        assert!(matches!(err, FrontMatterError::Unterminated));
    }

    #[test]
    fn campo_desconocido_es_error() {
        let src = "+++\nid = \"x\"\ntitle = \"X\"\nbogus = 1\n+++\nbody\n";
        assert!(matches!(split(src).unwrap_err(), FrontMatterError::Toml(_)));
    }

    // --- Endurecimiento: este módulo lo pisa la ayuda de plugins de terceros
    // (tarea 6), así que una entrada hostil DEGRADA a error, jamás entra en
    // pánico ni corta a mitad de un carácter multibyte.

    #[test]
    fn valla_trunca_sin_salto_es_error() {
        assert!(matches!(
            split(FENCE).unwrap_err(),
            FrontMatterError::Missing
        ));
    }

    #[test]
    fn solo_valla_de_apertura_es_error() {
        assert!(matches!(
            split("+++\n").unwrap_err(),
            FrontMatterError::Unterminated
        ));
    }

    #[test]
    fn valla_de_cierre_al_final_del_fichero_deja_cuerpo_vacio() {
        let src = "+++\nid = \"x\"\ntitle = \"X\"\n+++";
        let (fm, body) = split(src).expect("cabecera cerrada, aunque sin salto final");
        assert_eq!(fm.id, "x");
        assert_eq!(body, "", "sin cuerpo tras la valla de cierre");
    }

    #[test]
    fn gana_la_primera_valla_de_cierre() {
        let src = "+++\nid = \"x\"\ntitle = \"X\"\n+++\nantes\n+++\ndespues\n";
        let (fm, body) = split(src).expect("front matter válido");
        assert_eq!(fm.title, "X");
        assert_eq!(
            body, "antes\n+++\ndespues\n",
            "la segunda valla es cuerpo, no cabecera"
        );
    }

    #[test]
    fn titulo_multibyte_sobrevive_byte_a_byte() {
        let src = "+++\nid = \"copiar\"\ntitle = \"Copiar entre backends — ñ\"\n\
tags = [\"日本語\"]\n+++\ncuerpo — ñ\n";
        let (fm, body) = split(src).expect("front matter válido");
        assert_eq!(fm.title.as_bytes(), "Copiar entre backends — ñ".as_bytes());
        assert_eq!(fm.tags, vec!["日本語".to_owned()]);
        assert_eq!(body.as_bytes(), "cuerpo — ñ\n".as_bytes());
    }

    #[test]
    fn ningun_truncado_del_fuente_entra_en_panico() {
        let src = "+++\nid = \"x\"\ntitle = \"ñ — 日本語\"\n+++\ncuerpo — ñ\n";
        for (i, _) in src.char_indices().chain(std::iter::once((src.len(), ' '))) {
            // Cada prefijo es un fichero truncado plausible: o parsea, o da
            // error tipado; lo que no puede es cortar a mitad de carácter.
            let _ = split(&src[..i]);
        }
    }

    #[test]
    fn cabecera_multibyte_pegada_a_la_valla_no_corta_caracter() {
        // El carácter multibyte acaba justo contra el `\n+++`: si el corte
        // usara bytes mal calculados, este caso entraría en pánico.
        let src = "+++\nid = \"x\"\ntitle = \"ñ\"\n+++\n—\n";
        let (fm, body) = split(src).expect("front matter válido");
        assert_eq!(fm.title, "ñ");
        assert_eq!(body, "—\n");
    }
}
