//! La vista de ajustes, en solo lectura.
//!
//! Parte de `controller`: son métodos de `Estado`, movidos aquí sin
//! tocarlos (ADR 0086). El único escritor sigue siendo el actor.

// Estos módulos son el mismo `impl Estado` partido en trozos, así que usan
// los mismos imports que el padre. Enumerarlos aquí sería una lista de
// cuarenta líneas por fichero, en 32 ficheros, que se desincroniza en cuanto
// el padre importa algo — `super::*` la sigue sola.
#[allow(clippy::wildcard_imports)]
use super::*;

impl Estado {
    /// Abre los ajustes, en solo lectura.
    ///
    /// Las filas se construyen AQUÍ y se congelan, como las de la paleta y
    /// por el mismo motivo: `build_rows` resuelve el valor efectivo de cada
    /// entrada y formatea dos cadenas Fluent por fila. La configuración es la
    /// que el host recibió al arrancar, que es la que está usando.
    pub(super) fn abrir_ajustes(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.ajustes = Some(crate::settings::Ajustes::abrir(
            &self.config,
            &self.paths,
            self.lang,
        ));
        let cambio = ViewChange::Settings {
            settings: self.vista_ajustes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Un click en una fila de los ajustes: solo mueve el cursor.
    pub(super) fn elegir_ajuste(&mut self, row: u32) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(a) = self.ajustes.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        a.senalar(row as usize);
        let cambio = ViewChange::Settings {
            settings: self.vista_ajustes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// La proyección de los ajustes.
    pub(super) fn vista_ajustes(&self) -> Option<crate::dto::SettingsView> {
        Some(self.ajustes.as_ref()?.vista(self.lang))
    }

    /// Las teclas mientras los ajustes están abiertos.
    ///
    /// FIJAS, como las de la paleta y la ayuda: el catálogo no tiene
    /// comandos para «bajar por esta lista». `enter` no edita —esta ventana
    /// no escribe ajustes todavía— y lo DICE, en vez de no hacer nada.
    pub(super) fn tecla_en_ajustes(
        &mut self,
        k: &crate::keys::KeyInput,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// Cuántas filas mueve una página.
        const PAGINA: i64 = 10;
        if self.ajustes.is_none() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        let verbo = match k.key.as_str() {
            "Home" | "home" | "End" | "end" => None,
            _ => self.verbo_de_dialogo(k),
        };
        let Some(a) = self.ajustes.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match (verbo.as_deref(), k.key.as_str()) {
            (Some("dialog.cancel"), _) => self.ajustes = None,
            (Some("dialog.down"), _) => a.mover(1),
            (Some("dialog.up"), _) => a.mover(-1),
            (Some("dialog.page-down"), _) => a.mover(PAGINA),
            (Some("dialog.page-up"), _) => a.mover(-PAGINA),
            (_, "Home" | "home") => a.mover(i64::MIN / 2),
            (_, "End" | "end") => a.mover(i64::MAX / 2),
            (Some("dialog.confirm"), _) => {
                // No es un descarte silencioso: quien pulsa `enter` sobre un
                // ajuste espera editarlo, y esta ventana todavía no escribe.
                return (
                    ActionAck::Unavailable {
                        reason_key: "host-settings-read-only".to_owned(),
                    },
                    Vec::new(),
                );
            }
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::Settings {
            settings: self.vista_ajustes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }
}
