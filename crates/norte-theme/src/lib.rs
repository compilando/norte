//! Modelo de theming compartido de norte: colores, roles semánticos y temas,
//! independientes del framework de render (ADR 0020).
//!
//! El TUI (`norte-tui`) lo consume ya; la GUI de M5 REUSA el mismo modelo. Por
//! eso este crate NO depende de `ratatui` ni de ningún backend: expone un
//! [`Color`] propio (RGB de 24 bits con degradación a 256/16), [`Style`]s por
//! [`Role`] semántico, y una capa de efectos OPACA reservada a la GPU de la
//! GUI que un frontend de terminal ignora sin coste.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

// El modelo (Color/Role/Palette/Theme + parse) llega en la fase T2; los
// presets embebidos en T3 y los colores por tipo de archivo en T4. Este
// scaffold fija crate, licencia y lints (ADR 0020).

#[cfg(test)]
mod tests {
    #[test]
    fn crate_compila() {
        // Placeholder hasta T2: garantiza que el scaffold enlaza.
    }
}
