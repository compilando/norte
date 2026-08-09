//! Resolution state for ONE in-flight sequence. Owns its effective keymap:
//! hot-reload (ADR 0007) builds a new one and swaps the resolver whole.

use super::chord::Chord;
use super::effective::{Effective, Lookup};

/// Resultado de empujar una tecla al [`Resolver`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Secuencia completa: ejecutar este comando.
    Run(String),
    /// Prefijo válido de alguna secuencia: esperando (profundidad actual).
    Pending(usize),
    /// Sin binding (o cancelación): estado limpio, tecla descartada.
    Reset,
}

/// Estado de resolución de UNA secuencia en curso. POSEE su keymap
/// efectivo: el hot-reload (ADR 0007) construye uno nuevo y reemplaza el
/// resolver entero.
#[derive(Debug, Clone)]
pub struct Resolver {
    eff: Effective,
    pending: Vec<Chord>,
}

impl Resolver {
    /// Resolver limpio sobre un keymap efectivo.
    #[must_use]
    pub fn new(eff: Effective) -> Self {
        Self {
            eff,
            pending: Vec::new(),
        }
    }

    /// La secuencia pendiente (para pintarla en la status bar).
    #[must_use]
    pub fn pending(&self) -> &[Chord] {
        &self.pending
    }

    /// El keymap efectivo que este resolver posee (G3c): la GUI lo necesita
    /// para construir las filas de la paleta de comandos
    /// (`palette::first_chord`) sin duplicar el `Effective` en un campo
    /// aparte de `NorteGui` — el resolver ya es la única fuente de verdad
    /// del keymap vigente (hot-reload lo reemplaza entero, ver el doc del
    /// tipo).
    #[must_use]
    pub fn effective(&self) -> &Effective {
        &self.eff
    }

    /// Rompe cualquier secuencia pendiente (una tecla no modelada por el
    /// frontend equivale a un miss: cancela el multi-tecla en curso).
    pub fn reset(&mut self) {
        self.pending.clear();
    }

    /// Empuja una tecla. Con secuencia pendiente, `Esc` SIEMPRE cancela
    /// (jamás ejecuta un binding); sin pendiente, `Esc` es una tecla más.
    pub fn push(&mut self, chord: Chord) -> Resolution {
        if !self.pending.is_empty() && chord.is_bare_esc() {
            self.pending.clear();
            return Resolution::Reset;
        }
        self.pending.push(chord);
        match self.eff.lookup(&self.pending) {
            Lookup::Exact(run) => {
                self.pending.clear();
                Resolution::Run(run.to_owned())
            }
            Lookup::Prefix => Resolution::Pending(self.pending.len()),
            Lookup::Miss => {
                self.pending.clear();
                Resolution::Reset
            }
        }
    }
}
