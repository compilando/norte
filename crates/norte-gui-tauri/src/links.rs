//! Qué enlace externo se puede abrir, y quién decide que se puede.
//!
//! El spike NO abre ninguno: no hay superficie que los produzca todavía. Lo
//! que hay es la puerta, cerrada y con su prueba, porque la pregunta que la
//! tarea 3.3 hace —«¿rechaza un esquema que no está en la lista?»— se
//! contesta una vez y vale para la fase 4, y porque la respuesta por defecto
//! de un renderer sin esta comprobación es «abro lo que me den», que con un
//! `file://` es leer el disco y con un esquema del sistema es ejecutar algo.

/// Los ÚNICOS esquemas que un enlace de la interfaz puede llevar.
///
/// Ni `file:`, ni `data:`, ni `javascript:`, ni nada del sistema: un enlace
/// que aparece en una ayuda, en la salida de un plugin o en un nombre de
/// fichero es un DATO, y un dato no elige qué programa se lanza.
pub const ESQUEMAS: &[&str] = &["https", "http", "mailto"];

/// Por qué no se abre.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// El esquema no está en la lista.
    #[error("esquema no permitido")]
    Scheme,
    /// Ni siquiera tiene forma de URL absoluta.
    #[error("no es una URL absoluta")]
    Shape,
    /// Trae controles, espacios o marcas de dirección: un enlace que se
    /// pinta de una forma y apunta a otra.
    #[error("la URL lleva caracteres de control")]
    Control,
}

/// Valida un enlace antes de que nadie piense en abrirlo.
///
/// # Errors
/// [`LinkError`] cuando el esquema no está permitido, la forma no es la de
/// una URL absoluta, o el texto lleva controles.
///
/// ```
/// use norte_gui_tauri::links::{validar, LinkError};
///
/// assert!(validar("https://norte.example/docs").is_ok());
/// assert_eq!(validar("file:///etc/passwd"), Err(LinkError::Scheme));
/// assert_eq!(validar("javascript:alert(1)"), Err(LinkError::Scheme));
/// ```
pub fn validar(url: &str) -> Result<(), LinkError> {
    // Controles, blancos y —sobre todo— las marcas de dirección: un
    // `U+202E` convierte `…/gpj.exe` en algo que se lee `…/exe.jpg`. Un
    // enlace que no se lee como lo que abre no se abre.
    let sospechoso = |c: char| {
        c.is_control()
            || c.is_whitespace()
            || matches!(c, '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    };
    if url.chars().any(sospechoso) {
        return Err(LinkError::Control);
    }
    let Some((esquema, resto)) = url.split_once(':') else {
        return Err(LinkError::Shape);
    };
    if esquema.is_empty() || resto.is_empty() {
        return Err(LinkError::Shape);
    }
    // Comparación en minúsculas ASCII: `JavaScript:` es el mismo esquema que
    // `javascript:` para el navegador, y una lista que solo mira minúsculas
    // es una lista que se salta escribiendo en mayúsculas.
    let esquema = esquema.to_ascii_lowercase();
    if !ESQUEMAS.contains(&esquema.as_str()) {
        return Err(LinkError::Scheme);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lo_permitido_pasa() {
        assert!(validar("https://ejemplo.test/a").is_ok());
        assert!(validar("mailto:alguien@ejemplo.test").is_ok());
    }

    #[test]
    fn lo_demas_no() {
        for u in [
            "file:///etc/passwd",
            "JavaScript:alert(1)",
            "data:text/html,<script>",
            "ssh://host",
            "vscode://file/etc/passwd",
        ] {
            assert_eq!(validar(u), Err(LinkError::Scheme), "{u} no debería pasar");
        }
    }

    #[test]
    fn una_url_partida_no_pasa() {
        assert_eq!(validar("sin-esquema"), Err(LinkError::Shape));
        assert_eq!(validar("https:"), Err(LinkError::Shape));
    }

    /// Un enlace con un control dentro se pinta de una forma y apunta a otra.
    #[test]
    fn los_controles_no_pasan() {
        assert_eq!(validar("https://a.test/\u{202e}x"), Err(LinkError::Control));
        assert_eq!(validar("https://a.test/ x"), Err(LinkError::Control));
    }
}
