//! El asistente de primer arranque en la ventana (spec 2026-09-10): abrir,
//! mover, confirmar, y escribir lo elegido por el MISMO camino que la
//! pantalla de ajustes (`escribir_ajuste`). El modelo es el compartido con
//! el terminal; aquí va la vista previa del tema en vivo y la escritura.

// El mismo `impl Estado` partido en trozos: los imports del padre, como en
// los otros módulos de `controller` (ADR 0086).
#[allow(clippy::wildcard_imports)]
use super::*;
use norte_frontend::wizard::{Outcome, Wizard};

impl Estado {
    /// Abre el asistente con los presets y los temas que hay, arrancando en
    /// lo vigente. Lo manda el renderer cuando el catálogo dice `first_run`.
    pub(super) fn abrir_asistente(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let temas = norte_theme::preset_names();
        let tema = self
            .config
            .common
            .ui_theme
            .clone()
            .unwrap_or_else(|| "default".to_owned());
        self.asistente = Some(Wizard::new(
            norte_frontend::keymap::presets::NAMES,
            &temas,
            &self.config.common.preset,
            &tema,
        ));
        (self.aplicada(), vec![self.parche_asistente()])
    }

    /// El asistente, para la foto y para el parche.
    pub(super) fn vista_asistente(&self) -> Option<crate::dto::WizardView> {
        let w = self.asistente.as_ref()?;
        Some(crate::dto::WizardView {
            title: clamp_display(w.title(self.lang)),
            question: clamp_display(w.question(self.lang)),
            rows: w.rows(self.lang).into_iter().map(clamp_display).collect(),
            cursor: w.cursor() as u64,
            hint: clamp_display(norte_i18n::t_in(self.lang, "wizard-hint")),
        })
    }

    fn parche_asistente(&mut self) -> BridgeEnvelope<UiUpdate> {
        let cambio = ViewChange::Wizard {
            wizard: self.vista_asistente(),
        };
        self.parche(vec![cambio])
    }

    /// Un click en una fila: la elige Y la confirma, que es lo que un click
    /// significa en una lista de tres.
    pub(super) fn activar_fila_de_asistente(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(w) = self.asistente.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        w.select(row as usize);
        let outcome = w.confirm();
        self.tras_el_paso(outcome, backend, buzon)
    }

    /// Una tecla con el asistente abierto: sube, baja, confirma, vuelve o
    /// sale. Teclas FIJAS, como la paleta: no hay preset todavía, es justo
    /// lo que se pregunta.
    pub(super) fn tecla_en_asistente(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(w) = self.asistente.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let outcome = match k.key.as_str() {
            "ArrowUp" | "up" | "Up" => {
                w.up();
                Outcome::Continue
            }
            "ArrowDown" | "down" | "Down" => {
                w.down();
                Outcome::Continue
            }
            "Backspace" | "backspace" => {
                w.back();
                Outcome::Continue
            }
            "Enter" | "enter" => w.confirm(),
            "Escape" | "esc" => w.dismiss(),
            _ => return (self.aplicada(), Vec::new()),
        };
        self.tras_el_paso(outcome, backend, buzon)
    }

    /// Lo que sigue a mover o confirmar: la vista previa del tema si toca, y
    /// al terminar, escribir y cerrar.
    fn tras_el_paso(
        &mut self,
        outcome: Outcome,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match outcome {
            Outcome::Continue => {
                if let Some(nombre) = self
                    .asistente
                    .as_ref()
                    .and_then(Wizard::preview_theme)
                    .map(str::to_owned)
                {
                    self.aplicar_tema(&nombre, buzon);
                }
                (self.aplicada(), vec![self.parche_asistente()])
            }
            done => {
                self.asistente = None;
                let mut fuera = vec![self.parche_asistente()];
                fuera.extend(self.terminar_asistente(done, backend, buzon));
                (self.aplicada(), fuera)
            }
        }
    }

    /// Escribe lo elegido por el camino de la pantalla de ajustes. Con
    /// `Dismissed` escribe SOLO el tema vigente, para que exista el fichero y
    /// no se vuelva a preguntar. Los iconos van al plugin `file-icons` por
    /// el daemon; sin plugin, el rehúse se ignora.
    fn terminar_asistente(
        &mut self,
        outcome: Outcome,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let (preset, theme, icons) = match outcome {
            Outcome::Done(c) => (c.preset, c.theme, c.icons),
            Outcome::Dismissed | Outcome::Continue => (None, None, None),
        };
        let theme = theme.or_else(|| {
            Some(
                self.config
                    .common
                    .ui_theme
                    .clone()
                    .unwrap_or_else(|| "default".to_owned()),
            )
        });
        let mut fuera = Vec::new();
        for (section, key, value) in [("keymap", "preset", preset), ("ui", "theme", theme)] {
            let Some(v) = value else { continue };
            let write = norte_frontend::settings::PendingWrite::text(
                section,
                key,
                &v,
                norte_i18n::t_in(self.lang, "wizard-title"),
            );
            let (_, envelopes) = self.escribir_ajuste(write, buzon);
            fuera.extend(envelopes);
        }
        if let Some(icons) = icons {
            let style = if icons { "emoji" } else { "ascii" };
            let backend = Arc::clone(backend);
            tokio::spawn(async move {
                if let Err(e) = backend
                    .plugin_set_config(
                        "file-icons".to_owned(),
                        "style".to_owned(),
                        style.to_owned(),
                    )
                    .await
                {
                    tracing::info!(error = %e, "sin plugin file-icons: los iconos del asistente no aplican");
                }
            });
        }
        fuera
    }
}
