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
        let nombres = norte_frontend::theme::theme_names(&self.config.user_themes);
        let temas: Vec<&str> = nombres.iter().map(String::as_str).collect();
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

    /// Pone la pantalla de arranque (spec 2026-09-15, ADR 0115).
    ///
    /// Con la MISMA puerta que el terminal: `off` no pone nada, el asistente
    /// gana —de dos cosas que taparían el primer frame, la que pregunta algo
    /// va primero— y `brief` se quita sola pasado su plazo.
    pub(super) fn abrir_splash(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_config::load::SplashMode;
        let modo = self.config.common.ui_chrome.splash();
        // Se enseña UNA vez por sesión del host, y solo si no hay nada que ya
        // esté pidiendo algo. El renderer manda `splash_open` en cada
        // arranque suyo —y el host sobrevive a una recarga del webview—, así
        // que sin la marca una recarga a media sesión tapaba lo que estabas
        // mirando. Y de dos cosas que cubren la pantalla, la que PREGUNTA va
        // primero: un diálogo o el visor se quedan delante, y además así
        // ningún dígito acaba navegando el panel de detrás mientras hay una
        // pregunta con plazo esperando respuesta.
        if modo == SplashMode::Off
            || self.splash_visto
            || self.asistente.is_some()
            || !self.dialogos.is_empty()
            || self.visor.is_some()
        {
            return (self.aplicada(), Vec::new());
        }
        self.splash_visto = true;
        let secciones = if modo == SplashMode::Home {
            self.fuentes_de_splash()
        } else {
            // `brief` va SIN secciones: se quita sola, así que una lista de
            // sitios ahí sería una oferta que se retira antes de aceptarla.
            Vec::new()
        };
        self.splash = Some(norte_frontend::splash::SplashView {
            art: norte_frontend::splash::ART,
            version: norte_frontend::version::VERSION.to_owned(),
            revision: norte_frontend::version::VERSION_LINE
                .split_once(' ')
                .map_or_else(String::new, |(_, resto)| resto.to_owned()),
            daemon: norte_frontend::splash::Daemon::Connected,
            sections: secciones,
        });
        // El plazo sale de `[ui] splash_ms`, no de una constante: una portada
        // que no da tiempo a leerse solo estorba, y cuánto es «tiempo» depende
        // de quién mira.
        self.splash_hasta_ms = (modo == SplashMode::Brief)
            .then(|| super::ahora_ms() + i64::from(self.config.common.ui_chrome.splash_ms()));
        (self.aplicada(), vec![self.parche_splash()])
    }

    /// Las secciones de ESTA ventana: a dónde sueles ir, y lo que guardaste.
    fn fuentes_de_splash(&self) -> Vec<norte_frontend::splash::SplashSection> {
        use norte_frontend::splash::{SplashRow, SplashSection};
        let populares: Vec<SplashRow> = self
            .popular
            .ranked()
            .into_iter()
            .take(5)
            .map(|e| {
                let (texto, _) = norte_frontend::display::path_display(&e.path);
                SplashRow {
                    label: clamp_display(texto),
                    detail: e.visits.to_string(),
                    command: "nav.enter".to_owned(),
                    arg: Some(e.path.to_wire()),
                }
            })
            .collect();
        let favoritos: Vec<SplashRow> = self
            .config
            .common
            .hotlist
            .iter()
            .take(5)
            .filter_map(|h| {
                let destino = h.target.as_ref().ok()?;
                let (nombre, _) = norte_frontend::display_name(h.name.as_bytes());
                let (ruta, _) = norte_frontend::display::path_display(destino);
                Some(SplashRow {
                    label: clamp_display(nombre),
                    detail: clamp_display(ruta),
                    command: "nav.enter".to_owned(),
                    arg: Some(destino.to_wire()),
                })
            })
            .collect();
        [
            ("splash-popular", populares),
            ("splash-bookmarks", favoritos),
        ]
        .into_iter()
        .filter(|(_, filas)| !filas.is_empty())
        .map(|(title_key, rows)| SplashSection { title_key, rows })
        .collect()
    }

    /// La pantalla de arranque, para la foto y para el parche.
    pub(super) fn vista_splash(&self) -> Option<crate::dto::SplashView> {
        let s = self.splash.as_ref()?;
        let numeradas = norte_frontend::splash::numbered(&s.sections);
        let mut n = 0u8;
        Some(crate::dto::SplashView {
            art: s.art.iter().map(|l| (*l).to_owned()).collect(),
            version: clamp_display(s.version.clone()),
            revision: clamp_display(s.revision.clone()),
            daemon: clamp_display(norte_i18n::t_in(self.lang, s.daemon.key())),
            hint: clamp_display(norte_i18n::t_in(
                self.lang,
                if numeradas.is_empty() {
                    "splash-hint"
                } else {
                    "splash-hint-home"
                },
            )),
            sections: s
                .sections
                .iter()
                .map(|sec| crate::dto::SplashSectionView {
                    title: clamp_display(norte_i18n::t_in(self.lang, sec.title_key)),
                    rows: sec
                        .rows
                        .iter()
                        .map(|f| {
                            n = n.saturating_add(1);
                            crate::dto::SplashRowView {
                                // Cero = la fila se lee pero no tiene tecla que
                                // la llame: más allá de nueve no se promete.
                                number: u8::from(usize::from(n) <= numeradas.len()) * n,
                                label: clamp_display(f.label.clone()),
                                detail: clamp_display(f.detail.clone()),
                            }
                        })
                        .collect(),
                })
                .collect(),
            // Lo que le QUEDA, no cuándo vence: el renderer no comparte reloj
            // con el host —ni siquiera proceso—, así que una marca de tiempo
            // absoluta sería un número que allí no significa nada.
            close_after_ms: self
                .splash_hasta_ms
                .map(|hasta| u32::try_from((hasta - super::ahora_ms()).max(0)).unwrap_or(u32::MAX)),
        })
    }

    fn parche_splash(&mut self) -> BridgeEnvelope<UiUpdate> {
        let cambio = ViewChange::Splash {
            splash: self.vista_splash(),
        };
        self.parche(vec![cambio])
    }

    /// Abre lo que dice una fila NUMERADA de la pantalla de arranque, y la
    /// quita.
    ///
    /// El número es el PINTADO (1..=9), no un índice, porque es lo que el
    /// lector teclea o pulsa. Un número que ninguna fila lleva no es un
    /// error: quita la pantalla y ya está, igual que cualquier otra tecla —
    /// exigir puntería para salir de una pantalla de bienvenida sería un
    /// castigo raro.
    ///
    /// Navega con [`Trail::Record`] a propósito: entrar desde aquí es una
    /// visita como cualquier otra, y `alt+izquierda` tiene que poder
    /// deshacerla (ADR 0114).
    pub(super) fn activar_fila_de_splash(
        &mut self,
        number: u8,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // El `arg` de la fila elegida, si la pantalla está puesta y ese número
        // nombra una fila. `None` aquí es «ese número no ofrece nada» —un
        // clic que llega tarde, un dígito suelto—, que NO es un error.
        let wire = self.splash.as_ref().and_then(|s| {
            norte_frontend::splash::numbered(&s.sections)
                .into_iter()
                .find(|(i, _)| *i == number)
                .and_then(|(_, fila)| fila.arg.clone())
        });
        let destino = wire
            .as_deref()
            .and_then(|w| norte_proto::VPath::parse(w).ok());
        // Solo es un fallo si la fila SÍ nombraba un sitio y ese sitio no
        // parsea: la escribimos nosotros con `to_wire`, así que llegar aquí
        // es cosa nuestra.
        let rota = wire.is_some() && destino.is_none();
        let mut envios = self.cerrar_splash();
        if let Some(destino) = destino {
            envios.extend(self.navegar(&destino, Trail::Record, backend, buzon));
        } else if rota {
            // Y si la fila nombraba un sitio que no parsea, se DICE. Es un fallo
            // nuestro —la fila la escribimos nosotros con `to_wire`—, pero
            // una pantalla que se quita sin hacer lo que la fila prometía y
            // sin decir por qué se lee como una tecla que no funciona. El
            // terminal dice esto mismo (`norte-tui/src/event_loop.rs`).
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "err-invalid-path",
            )));
            envios.push(self.parche(vec![ViewChange::Status(self.status.clone())]));
        }
        (self.aplicada(), envios)
    }

    /// Quita la pantalla de arranque (una tecla, un clic, o su plazo).
    ///
    /// Devuelve el parche solo si había algo puesto: un cierre que manda
    /// parches vacíos gasta números de secuencia que nadie recibe.
    pub(super) fn cerrar_splash(&mut self) -> Vec<BridgeEnvelope<UiUpdate>> {
        if self.splash.take().is_none() {
            return Vec::new();
        }
        self.splash_hasta_ms = None;
        vec![self.parche_splash()]
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
        // Con `Dismissed` se escribe el tema con el que se ABRIÓ el
        // asistente: el modelo lo trae, y así el terminal y la ventana
        // escriben lo mismo (ADR 0077, revisión B1).
        let (preset, theme, icons) = match outcome {
            Outcome::Done(c) => (
                c.preset,
                c.theme.or_else(|| Some("default".to_owned())),
                c.icons,
            ),
            Outcome::Dismissed { keep_theme } => (None, Some(keep_theme), None),
            Outcome::Continue => (None, None, None),
        };
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
