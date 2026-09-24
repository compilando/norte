//! The first-run wizard in the window (spec 2026-09-10): opening, moving,
//! confirming, and writing what was chosen through the SAME path as the
//! settings screen (`escribir_ajuste`). The model is shared with the
//! terminal; what lives here is the live theme preview and the writing.

// The same `impl Estado` split into pieces: the parent's imports, like the
// other `controller` modules (ADR 0086).
#[allow(clippy::wildcard_imports)]
use super::*;
use norte_frontend::wizard::{Outcome, Wizard};

impl Estado {
    /// Opens the wizard with the presets and themes there are, starting on
    /// whatever is current. The renderer sends this when the catalogue says
    /// `first_run`.
    pub(super) fn abrir_asistente(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let names = norte_frontend::theme::theme_names(&self.config.user_themes);
        let themes: Vec<&str> = names.iter().map(String::as_str).collect();
        let theme = self
            .config
            .common
            .ui_theme
            .clone()
            .unwrap_or_else(|| "default".to_owned());
        self.asistente = Some(Wizard::new(
            norte_frontend::keymap::presets::NAMES,
            &themes,
            &self.config.common.preset,
            &theme,
        ));
        (self.aplicada(), vec![self.parche_asistente()])
    }

    /// Sets up the splash screen (spec 2026-09-15, ADR 0115).
    ///
    /// Through the SAME gate as the terminal: `off` sets nothing, the wizard
    /// wins — of two things that would cover the first frame, the one that
    /// asks something goes first — and `brief` removes itself once its
    /// deadline passes.
    pub(super) fn abrir_splash(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_config::load::SplashMode;
        let mode = self.config.common.ui_chrome.splash();
        // It is shown ONCE per host session, and only if nothing is already
        // asking for something. The renderer sends `splash_open` on every one
        // of its own startups — and the host survives a webview reload — so
        // without the flag, a mid-session reload would cover up what you were
        // looking at. And of two things that cover the screen, the one that
        // ASKS goes first: a dialog or the viewer stay in front, and this way
        // no digit ends up navigating the panel behind it while a question
        // with a deadline is waiting for an answer.
        if mode == SplashMode::Off
            || self.splash_visto
            || self.asistente.is_some()
            || !self.dialogos.is_empty()
            || self.visor.is_some()
        {
            return (self.aplicada(), Vec::new());
        }
        self.splash_visto = true;
        let sections = if mode == SplashMode::Home {
            self.fuentes_de_splash()
        } else {
            // `brief` goes WITHOUT sections: it removes itself, so a list of
            // places there would be an offer withdrawn before it can be
            // accepted.
            Vec::new()
        };
        self.splash = Some(norte_frontend::splash::SplashView {
            art: norte_frontend::splash::ART,
            version: norte_frontend::version::VERSION.to_owned(),
            revision: norte_frontend::version::VERSION_LINE
                .split_once(' ')
                .map_or_else(String::new, |(_, rest)| rest.to_owned()),
            daemon: norte_frontend::splash::Daemon::Connected,
            sections,
        });
        // The deadline comes from `[ui] splash_ms`, not a constant: a splash
        // screen that does not give time to read it is just in the way, and
        // how much "time" is depends on who is looking.
        self.splash_hasta_ms = (mode == SplashMode::Brief)
            .then(|| super::ahora_ms() + i64::from(self.config.common.ui_chrome.splash_ms()));
        (self.aplicada(), vec![self.parche_splash()])
    }

    /// THIS window's sections: where you usually go, and what you saved.
    fn fuentes_de_splash(&self) -> Vec<norte_frontend::splash::SplashSection> {
        use norte_frontend::splash::{SplashRow, SplashSection};
        let popular: Vec<SplashRow> = self
            .popular
            .ranked()
            .into_iter()
            .take(5)
            .map(|e| {
                let (text, _) = norte_frontend::display::path_display(&e.path);
                SplashRow {
                    label: clamp_display(text),
                    detail: e.visits.to_string(),
                    command: "nav.enter".to_owned(),
                    arg: Some(e.path.to_wire()),
                }
            })
            .collect();
        let favorites: Vec<SplashRow> = self
            .config
            .common
            .hotlist
            .iter()
            .take(5)
            .filter_map(|h| {
                let target = h.target.as_ref().ok()?;
                let (name, _) = norte_frontend::display_name(h.name.as_bytes());
                let (path, _) = norte_frontend::display::path_display(target);
                Some(SplashRow {
                    label: clamp_display(name),
                    detail: clamp_display(path),
                    command: "nav.enter".to_owned(),
                    arg: Some(target.to_wire()),
                })
            })
            .collect();
        [("splash-popular", popular), ("splash-bookmarks", favorites)]
            .into_iter()
            .filter(|(_, rows)| !rows.is_empty())
            .map(|(title_key, rows)| SplashSection { title_key, rows })
            .collect()
    }

    /// The splash screen, for the snapshot and for the patch.
    pub(super) fn vista_splash(&self) -> Option<crate::dto::SplashView> {
        let s = self.splash.as_ref()?;
        let numbered = norte_frontend::splash::numbered(&s.sections);
        let mut n = 0u8;
        Some(crate::dto::SplashView {
            art: s.art.iter().map(|l| (*l).to_owned()).collect(),
            version: clamp_display(s.version.clone()),
            revision: clamp_display(s.revision.clone()),
            daemon: clamp_display(norte_i18n::t_in(self.lang, s.daemon.key())),
            hint: clamp_display(norte_i18n::t_in(
                self.lang,
                if numbered.is_empty() {
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
                                // Zero = the row is read but has no key that
                                // calls it: beyond nine, nothing is promised.
                                number: u8::from(usize::from(n) <= numbered.len()) * n,
                                label: clamp_display(f.label.clone()),
                                detail: clamp_display(f.detail.clone()),
                            }
                        })
                        .collect(),
                })
                .collect(),
            // What is LEFT, not when it expires: the renderer does not share
            // a clock with the host — not even a process — so an absolute
            // timestamp would be a number that means nothing there.
            close_after_ms: self
                .splash_hasta_ms
                .map(|until| u32::try_from((until - super::ahora_ms()).max(0)).unwrap_or(u32::MAX)),
        })
    }

    fn parche_splash(&mut self) -> BridgeEnvelope<UiUpdate> {
        let change = ViewChange::Splash {
            splash: self.vista_splash(),
        };
        self.parche(vec![change])
    }

    /// Opens whatever a NUMBERED row of the splash screen says, and removes
    /// it.
    ///
    /// The number is the PAINTED one (1..=9), not an index, because that is
    /// what the reader types or presses. A number no row carries is not an
    /// error: it just removes the screen, same as any other key — demanding
    /// aim to leave a welcome screen would be an odd punishment.
    ///
    /// It navigates with [`Trail::Record`] on purpose: entering from here is
    /// a visit like any other, and `alt+left` has to be able to undo it (ADR
    /// 0114).
    pub(super) fn activar_fila_de_splash(
        &mut self,
        number: u8,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // The chosen row's `arg`, if the screen is up and that number names a
        // row. `None` here is "that number offers nothing" — a click that
        // arrives late, a stray digit — which is NOT an error.
        let wire = self.splash.as_ref().and_then(|s| {
            norte_frontend::splash::numbered(&s.sections)
                .into_iter()
                .find(|(i, _)| *i == number)
                .and_then(|(_, row)| row.arg.clone())
        });
        let target = wire
            .as_deref()
            .and_then(|w| norte_proto::VPath::parse(w).ok());
        // It is only a failure if the row DID name a place and that place
        // does not parse: we wrote it ourselves with `to_wire`, so getting
        // here is on us.
        let broken = wire.is_some() && target.is_none();
        let mut outgoing = self.cerrar_splash();
        if let Some(target) = target {
            outgoing.extend(self.navegar(&target, Trail::Record, backend, mailbox));
        } else if broken {
            // And if the row named a place that does not parse, it is SAID.
            // It is a failure of ours — we wrote the row ourselves with
            // `to_wire` — but a screen that goes away without doing what the
            // row promised and without saying why reads as a key that does
            // not work. The terminal says this same thing
            // (`norte-tui/src/event_loop.rs`).
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "err-invalid-path",
            )));
            outgoing.push(self.parche(vec![ViewChange::Status(self.status.clone())]));
        }
        (self.aplicada(), outgoing)
    }

    /// Removes the splash screen (a key, a click, or its deadline).
    ///
    /// Returns the patch only if something was showing: a close that sends
    /// empty patches spends sequence numbers nobody receives.
    pub(super) fn cerrar_splash(&mut self) -> Vec<BridgeEnvelope<UiUpdate>> {
        if self.splash.take().is_none() {
            return Vec::new();
        }
        self.splash_hasta_ms = None;
        vec![self.parche_splash()]
    }

    /// The wizard, for the snapshot and for the patch.
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
        let change = ViewChange::Wizard {
            wizard: self.vista_asistente(),
        };
        self.parche(vec![change])
    }

    /// A click on a row: it selects it AND confirms it, which is what a click
    /// means in a list of three.
    pub(super) fn activar_fila_de_asistente(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(w) = self.asistente.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        w.select(row as usize);
        let outcome = w.confirm();
        self.tras_el_paso(outcome, backend, mailbox)
    }

    /// A key with the wizard open: up, down, confirm, back, or leave. FIXED
    /// keys, like the palette: there is no preset yet, that is exactly what
    /// is being asked.
    pub(super) fn tecla_en_asistente(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
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
        self.tras_el_paso(outcome, backend, mailbox)
    }

    /// What follows moving or confirming: the theme preview if it applies,
    /// and on finishing, writing and closing.
    fn tras_el_paso(
        &mut self,
        outcome: Outcome,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match outcome {
            Outcome::Continue => {
                if let Some(name) = self
                    .asistente
                    .as_ref()
                    .and_then(Wizard::preview_theme)
                    .map(str::to_owned)
                {
                    self.aplicar_tema(&name, mailbox);
                }
                (self.aplicada(), vec![self.parche_asistente()])
            }
            done => {
                self.asistente = None;
                let mut outgoing = vec![self.parche_asistente()];
                outgoing.extend(self.terminar_asistente(done, backend, mailbox));
                (self.aplicada(), outgoing)
            }
        }
    }

    /// Writes what was chosen through the settings screen's path. With
    /// `Dismissed` it writes ONLY the current theme, so the file exists and
    /// it is not asked again. Icons go to the `file-icons` plugin through the
    /// daemon; without the plugin, the refusal is ignored.
    fn terminar_asistente(
        &mut self,
        outcome: Outcome,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // With `Dismissed`, the theme the wizard OPENED with is written: the
        // model carries it, so the terminal and the window write the same
        // thing (ADR 0077, revision B1).
        let (preset, theme, icons) = match outcome {
            Outcome::Done(c) => (
                c.preset,
                c.theme.or_else(|| Some("default".to_owned())),
                c.icons,
            ),
            Outcome::Dismissed { keep_theme } => (None, Some(keep_theme), None),
            Outcome::Continue => (None, None, None),
        };
        let mut outgoing = Vec::new();
        for (section, key, value) in [("keymap", "preset", preset), ("ui", "theme", theme)] {
            let Some(v) = value else { continue };
            let write = norte_frontend::settings::PendingWrite::text(
                section,
                key,
                &v,
                norte_i18n::t_in(self.lang, "wizard-title"),
            );
            let (_, envelopes) = self.escribir_ajuste(write, mailbox);
            outgoing.extend(envelopes);
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
                    tracing::info!(error = %e, "no file-icons plugin: the wizard's icons do not apply");
                }
            });
        }
        outgoing
    }
}
