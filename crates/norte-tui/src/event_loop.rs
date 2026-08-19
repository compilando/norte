//! El bucle de eventos del TUI y lo que puede tumbarlo.
//!
//! Por ahora solo el error. El bucle (`run`, ~2.500 líneas) sigue en el root
//! del binario `ntc` y viene en la ronda siguiente; el error va delante porque
//! es lo que hace posible que venga: la regla 6 prohíbe `anyhow` en una
//! biblioteca, y `run` devolvía `anyhow::Result<()>`.
//!
//! El muro resultó ser de CUATRO líneas —dos `terminal.size()`, un
//! `terminal.draw()` y un `.context("evento de terminal")`—, así que dos
//! variantes lo cubren. Las dos llevan un `std::io::Error` dentro; están
//! separadas porque la información que aportaba aquel `.context` era CUÁL de las
//! dos superficies falló, y un solo `Io(..)` la perdería: no se diagnostica igual
//! una terminal que no se deja medir que un flujo de eventos que se corta.
//!
//! Estos textos NO van por Fluent, y es deliberado: no son cadenas de interfaz
//! sino el diagnóstico que `anyhow` imprime al morir el proceso, que es
//! exactamente el uso que el `.context` anterior les daba.

/// Lo que aborta el bucle de eventos.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// La terminal no se dejó medir (`size`) o pintar (`draw`).
    #[error("la terminal no respondió: {0}")]
    Terminal(#[source] std::io::Error),
    /// El flujo de eventos de la terminal se rompió. Un flujo que se AGOTA no
    /// es esto: cerrar la ventana sale por el mismo sitio que un `app.quit`,
    /// para no perder la última foto de la sesión.
    #[error("evento de terminal: {0}")]
    Event(#[source] std::io::Error),
}
