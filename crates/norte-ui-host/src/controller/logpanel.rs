//! The window's log panel (#326): its projection and its controls.
//!
//! Part of `controller`: these are `Estado` methods, moved here without
//! touching them (ADR 0086). The only writer is still the actor.
//!
//! The STATE — level, filter, following the tail, the two-levels rule —
//! lives in `norte_frontend::logpanel`, the same code the TUI uses; the
//! lines come from `norte_config::logring`'s ring. What is this window's own
//! stays here: how it projects to the bridge and what each control does.

// These modules are the same `impl Estado` split into pieces, so they use
// the same imports as the parent. Enumerating them here would be a
// forty-line list per file, in 32 files, that goes stale the moment the
// parent imports something — `super::*` tracks it on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

use norte_config::logline::{LogLevel, LogLine};
use norte_frontend::logpanel::LogSource;

/// The kind that occupies a log slot. The same one the TUI uses.
pub(super) const KIND: &str = "log";

/// How many lines are requested from the daemon per round.
///
/// The daemon trims to 1000, so this is a request and not a contract. Five
/// hundred because a round that does not fit loses NOTHING — the leftover
/// stays past the cursor and the next round picks it up, half a second
/// later — and because the panel shows at most one screen: requesting the
/// whole ring every time would mean paying for two thousand lines for every
/// one that gets painted.
const MAX_REMOTO: u32 = 500;

/// Cap on daemon lines kept in memory.
///
/// The local ring already has its own; this is the same care for the remote
/// one, because here lines ACCUMULATE round after round and with no cap a
/// panel left open all afternoon would grow without end. Same order of
/// magnitude as the default ring: what can be scrolled back through.
const MAX_LINEAS_REMOTAS: usize = 2000;

/// How many lines a page jumps when the renderer does not say its height.
const PAGINA: isize = 10;

/// How often it is checked whether the log has changed.
///
/// The panel promises it FOLLOWS what arrives, and that promise has to be
/// kept: the TUI keeps it because it repaints every frame, and this window
/// only repaints when someone does something — so without polling the panel
/// would sit frozen between keystrokes while saying "stuck to the end".
///
/// Half a second: a log is read, not timed to the millisecond. And polling
/// is CHEAP — an `AtomicU64`, without touching the ring's lock — so what
/// actually costs something is the snapshot, and that is only sent when
/// there is something new.
const SONDEO: std::time::Duration = std::time::Duration::from_millis(500);

/// Cap on rows a renderer can declare visible.
///
/// Generous for a real screen — a 4K monitor with small type does not reach
/// it — and capped because this number comes from the webview: with no cap,
/// a huge `rows` would turn every snapshot into the whole ring.
const MAX_FILAS_REGISTRO: usize = 512;

/// What is known about the DAEMON's log.
///
/// Three values and not a `bool`, because "has not answered yet" and "said
/// it has no log" are shown differently: the first says nothing — the
/// selector is simply absent — and the second is a phrase the panel has to
/// put on screen. Collapsing them would make a freshly opened panel assert a
/// lack nobody has checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Servicio {
    /// Has never answered: unknown.
    #[default]
    SinRespuesta,
    /// Serves its log: there is a second source of truth.
    Sirve,
    /// Said it has no log to serve.
    SinAnillo,
}

/// The log panel's remote half (#328).
#[derive(Debug, Default)]
pub(super) struct RegistroRemoto {
    /// What the daemon has delivered so far, already in presentation form
    /// and from oldest to newest.
    ///
    /// It accumulates and is not re-fetched whole every round: polling pulls
    /// from the remote ring with a cursor, so each answer brings only what
    /// is new.
    pub(super) lineas: Vec<LogLine>,
    /// Where it was up to. `None` = has not been asked yet, which is "give
    /// me whatever there is" and is NOT the same as zero: against a ring
    /// that has already wrapped around, a zero would report a false `lost`
    /// on the first poll.
    pub(super) cursor: Option<u64>,
    /// A request is in flight.
    ///
    /// Without this, a daemon slower than half a second to answer would
    /// pile up one request per tick forever.
    pub(super) en_vuelo: bool,
    /// What is known about whether it serves its log.
    pub(super) servicio: Servicio,
    /// The level it answered it has set, in wire form.
    ///
    /// Its own, not ours: it is global to all its clients and only goes up,
    /// so what was requested and what is set need not match.
    pub(super) nivel: Option<String>,
    /// How many lines were dropped past this cursor. They accumulate: a
    /// silent gap lies about what happened.
    pub(super) perdidas: u64,
}

impl RegistroRemoto {
    /// Starts from zero, keeping what is known about the daemon.
    ///
    /// The lines and the cursor belong to THIS opening; whether the daemon
    /// serves its log or not is a fact about the daemon, and forgetting it
    /// would hide the selector for half a second every time the panel
    /// reopens.
    pub(super) fn reiniciar(&mut self) {
        self.lineas.clear();
        self.cursor = None;
        self.en_vuelo = false;
        self.perdidas = 0;
    }

    /// Does it make sense to ask this daemon anything again?
    ///
    /// Not one that already said it has no log: the negative cannot change
    /// while that daemon lives — it comes from a compile-time feature or
    /// from a mount that failed at startup — so continuing to ask would be
    /// RPCs forever for an answer that cannot be any other. It is
    /// asymmetric on purpose: the POSITIVE case does need to keep being
    /// asked, because the log grows.
    ///
    /// Shared by `log.tail`'s polling and `log.level`'s request, and that is
    /// the fix for an asymmetry: the level used to be requested only by the
    /// source, so against a daemon that had already answered `Unsupported`
    /// the window sent a dead RPC on every level press. The TUI already got
    /// this right (`RegistroRemoto::debe_pedir`); now it is the same rule in
    /// both.
    ///
    /// There is no version comparison here or anywhere: an older daemon does
    /// not even complete `initialize`.
    pub(super) const fn debe_pedir(&self) -> bool {
        !matches!(self.servicio, Servicio::SinAnillo)
    }
}

/// One wire line into the shape the panel paints.
///
/// The two types share a name and coexist in this file on purpose:
/// `methods::LogLine` is the WIRE one and `logline::LogLine` the
/// presentation one, and mixing them up is how you end up sending a level
/// `String` to a filter that compares verbosities.
///
/// An unrecognized level falls back to `Info` instead of dropping the line:
/// the protocol says an unknown value has to be able to ARRIVE, and losing
/// the whole message for not understanding its label is worse than showing
/// it with the ordinary label. With today's closed vocabulary it does not
/// happen.
fn linea_de_wire(l: norte_proto::methods::LogLine) -> LogLine {
    LogLine {
        epoch_ms: l.epoch_ms,
        level: LogLevel::from_wire(&l.level).unwrap_or(LogLevel::Info),
        target: l.target,
        message: l.message,
    }
}

impl Estado {
    /// The panel's projection: only the visible WINDOW.
    ///
    /// Like the listing, and for the same reason: a ring of two thousand
    /// lines sent whole on every patch is the waste decision D7 exists to
    /// prevent, and a log moves more than a directory does.
    pub(super) fn panel_de_registro(&self, slot: u32) -> crate::dto::LogSlotView {
        let lineas = self
            .log_ring
            .as_ref()
            .map(norte_config::logring::LogRing::snapshot)
            .unwrap_or_default();
        let fuente = self.fuente_efectiva();
        // Borrowed, not cloned: `merge` returns references on purpose — the
        // ring already cloned once in its `snapshot` — and the panel paints
        // at most one screen.
        let mezcla = norte_frontend::logpanel::merge(&lineas, &self.log_remoto.lineas, fuente);
        let visibles: Vec<_> = mezcla
            .into_iter()
            .filter(|(l, _)| self.log_panel.matches(l))
            .collect();
        let desde = self.log_panel.window_start(visibles.len(), self.log_filas);
        let ventana = visibles
            .iter()
            .skip(desde)
            .take(self.log_filas)
            .map(|(l, s)| Self::linea_de_registro(l, *s));
        crate::dto::LogSlotView {
            slot_id: slot,
            lines: ventana.collect(),
            // The one that is SHOWN, always, across all sources — and
            // therefore the one the buttons control.
            //
            // Showing here the one the daemon answered was a two-headed bug:
            // the `visibles` filter is still the panel's, so with the daemon
            // at `trace` and the panel at `info` the header marked `trace`
            // while every `debug` line from the daemon arrived over the wire
            // and was silently dropped — exactly the "no DEBUG lines is
            // indistinguishable from not capturing them" that
            // `LogTailResult::level`'s rustdoc exists to prevent — and
            // pressing `info` did not move the mark, because the daemon
            // never goes down, so the control read as dead. The daemon's
            // level is stated in `capturing`, which is already the spot that
            // means "more is being collected than is shown".
            level: self.log_panel.level().wire().to_owned(),
            // The title chip used to paint the wire id; the terminal paints
            // the label (`Log · TRACE`). Same pairing as in every line: the
            // id is compared — the renderer marks which button is set — and
            // the label is read.
            level_label: self.log_panel.level().label().trim().to_owned(),
            // The filter is TYPED by the reader, so it is painted like any
            // other outside text: masked and clamped.
            filter: clamp_display(
                norte_frontend::display_name(self.log_panel.filter().as_bytes()).0,
            ),
            following: self.log_panel.following(),
            total: visibles.len() as u64,
            first_visible: desde as u64,
            dropped_note: self.nota_de_descartes(fuente),
            capturing: self.nota_de_captura(fuente),
            source: clamp_display(norte_i18n::t_in(
                self.lang,
                match fuente {
                    // From THIS process, and saying so is the point: the
                    // window starts its own daemon (#300), so what is NOT
                    // here is the daemon's — the providers, the journal, the
                    // policy — which is the interesting half. Staying quiet
                    // about it would make the panel look broken: someone
                    // opens it while a connection fails, does not see the
                    // line that explains it, and concludes the panel is not
                    // working instead of that it is looking somewhere else.
                    LogSource::Window if self.log_ring.is_some() => "log-source-window",
                    // Not "nothing is being logged": the process keeps
                    // writing to its file. What is missing is the in-memory
                    // ring, which is what this panel reads — and saying the
                    // first thing would be a more reassuring answer than the
                    // truth. The TUI already had the exact phrase.
                    LogSource::Window => "log-no-ring",
                    LogSource::Daemon => "log-source-daemon",
                    LogSource::Both => "log-source-both",
                },
            )),
            source_mode: match fuente {
                LogSource::Window => "window",
                LogSource::Daemon => "daemon",
                LogSource::Both => "both",
            }
            .to_owned(),
            sources_available: self.log_remoto.servicio == Servicio::Sirve,
            source_note: self.nota_de_fuente(fuente),
        }
    }

    /// The source that is really being shown.
    ///
    /// The preference is stored as is (`LogPanel::source`), but a source
    /// that does not exist cannot be shown, and the panel reports what there
    /// is, not what was requested. It collapses in BOTH directions, which
    /// are the same rule seen from each shore:
    ///
    /// - with no ring on the other side (a daemon without the `logging`
    ///   feature, or the embedded case) everything falls to `Window`;
    /// - with no ring in THIS process — nobody mounted the layer — there is
    ///   nothing local to merge, so everything falls to `Daemon`. Without
    ///   this, a `Both` over a ringless process announced itself as "from
    ///   the window and the daemon" while being the daemon's whole list.
    ///
    /// With both rings absent it stays `Window`, which is where #326's
    /// phrase lives: there is no log IN MEMORY to read, and that is not the
    /// same as "nothing is being logged".
    fn fuente_efectiva(&self) -> LogSource {
        match (
            self.log_remoto.servicio == Servicio::Sirve,
            self.log_ring.is_some(),
        ) {
            (true, true) => self.log_panel.source(),
            (true, false) => LogSource::Daemon,
            (false, _) => LogSource::Window,
        }
    }

    /// What has to be said about the source. Empty = nothing to say.
    ///
    /// Two mutually exclusive phrases, and both exist so the panel does not
    /// lie by omission. That the daemon has no log to serve, or the reader
    /// would think the interesting half simply is not happening. And **whose
    /// level it is**, whenever the daemon is one of the sources being read —
    /// not only when it is the only one: in `Both`, which is what the panel
    /// opens with, pressing "trace" raises a GLOBAL ring on the daemon,
    /// shared with all its clients, that never comes back down and that
    /// closing this panel does not lower. Staying quiet about it on the
    /// common path left that decision unannounced.
    fn nota_de_fuente(&self, fuente: LogSource) -> String {
        let clave = if self.log_remoto.servicio == Servicio::SinAnillo {
            "log-source-unsupported"
        } else if fuente == LogSource::Window {
            return String::new();
        } else {
            "log-source-daemon-level"
        };
        clamp_display(norte_i18n::t_in(self.lang, clave))
    }

    /// Which ring is holding MORE than is shown, and which.
    ///
    /// Only when more is being captured than is shown: saying "capturing
    /// info" over a panel showing info would be noise, and noise is what
    /// makes the line that does matter stop being read.
    ///
    /// With a single source the phrase does not name the ring — there is no
    /// other to confuse it with — with both, each part says whose it is.
    /// That the daemon's level shows up here is what makes the whole rule
    /// legible: its own is global to its clients and only goes up, so it can
    /// sit well above what this panel shows, and that gap is exactly what
    /// this phrase exists to not keep quiet about.
    fn nota_de_captura(&self, fuente: LogSource) -> String {
        let ensena = self.log_panel.level();
        let local = self
            .log_ring
            .as_ref()
            .map(norte_config::logring::LogRing::level)
            .filter(|cap| *cap > ensena);
        let remoto = self
            .log_remoto
            .nivel
            .as_deref()
            .and_then(LogLevel::from_wire)
            .filter(|cap| *cap > ensena);
        let frase =
            |clave, cap: LogLevel| norte_i18n::ta_in(self.lang, clave, &[("level", cap.wire())]);
        let partes: Vec<String> = match fuente {
            LogSource::Window => local
                .map(|c| frase("log-capturing", c))
                .into_iter()
                .collect(),
            LogSource::Daemon => remoto
                .map(|c| frase("log-capturing-daemon", c))
                .into_iter()
                .collect(),
            LogSource::Both => local
                .map(|c| frase("log-capturing-window", c))
                .into_iter()
                .chain(remoto.map(|c| frase("log-capturing-daemon", c)))
                .collect(),
        };
        clamp_display(partes.join(" · "))
    }

    /// The lines that were lost, per ring and SAYING which one.
    ///
    /// Two numbers and not one, because they do not mean the same thing and
    /// do not live the same lifetime: the local ring's counts what has been
    /// evicted since the process started and never resets; the daemon's
    /// counts what THIS opening of the panel lost, and goes back to zero on
    /// reopening. Adding them together gave a number that was neither of the
    /// two things.
    ///
    /// Each ring is only mentioned if it is being read: warning about a gap
    /// in a log that is not on screen is an alarm about nothing.
    fn nota_de_descartes(&self, fuente: LogSource) -> String {
        let mut partes: Vec<String> = Vec::new();
        let locales = self
            .log_ring
            .as_ref()
            .map_or(0, norte_config::logring::LogRing::dropped);
        if locales > 0 && fuente != LogSource::Daemon {
            partes.push(norte_i18n::ta_in(
                self.lang,
                // Without naming the ring when it is the only one being
                // read: it is the same phrase the TUI uses, which never has
                // two.
                if fuente == LogSource::Window {
                    "log-dropped"
                } else {
                    "log-dropped-window"
                },
                &[("n", &locales.to_string())],
            ));
        }
        if self.log_remoto.perdidas > 0 && fuente != LogSource::Window {
            partes.push(norte_i18n::ta_in(
                self.lang,
                "log-missed-daemon",
                &[("n", &self.log_remoto.perdidas.to_string())],
            ));
        }
        clamp_display(partes.join(" · "))
    }

    /// One line, sanitized.
    ///
    /// The message goes through `display_name` like any painted text, and
    /// here for a reason of its own: a log message can carry inside it a
    /// file name someone chose, and a `U+202E` there reorders the panel's
    /// whole line.
    fn linea_de_registro(l: &LogLine, origen: LogSource) -> crate::dto::LogLineView {
        let (target, t_hostil) = norte_frontend::display_name(l.target.as_bytes());
        let (mensaje, m_hostil) = norte_frontend::display_name(l.message.as_bytes());
        crate::dto::LogLineView {
            time: norte_frontend::format::hora_utc(l.epoch_ms),
            level: l.level.wire().to_owned(),
            // What is PAINTED, which is not the wire id. The renderer used
            // to paint `trace` while the terminal paints `TRACE` and its own
            // buttons said "trace" in yet another spelling: three
            // vocabularies for the same level, all three on screen at once.
            // The label is the one from `LogLevel::label`, which is where
            // the terminal takes it from — without the column padding,
            // which is a fixed-width thing the window does not have.
            level_label: l.level.label().trim().to_owned(),
            target: clamp_display(target),
            message: clamp_display(mensaje),
            hostile: t_hostil || m_hostil,
            // `Both` is never passed to a line: `merge` marks each one with
            // the process it came from, which is the only thing that means
            // anything here.
            source: if origen == LogSource::Daemon {
                "daemon"
            } else {
                "window"
            }
            .to_owned(),
        }
    }

    /// Show up to this level.
    ///
    /// **Raises the RING's level if needed, and never lowers it.** It is the
    /// rule `LogPanel::show_level` returns and that has to be honored:
    /// filtering on screen what was never logged is impossible, so
    /// requesting DEBUG has to make the ring start capturing it. And
    /// dropping to ERROR does not stop capturing, because then going back up
    /// would show a hole the size of the time spent at ERROR.
    /// And, when the source includes the daemon, **it is asked of it TOO**
    /// (#328). Its ring is its own: this client does not apply levels,
    /// because the boundary that prevents a password from showing up inside
    /// lives in the process holding the ring. Whatever ends up set is
    /// answered by it, and it may not be what was requested — it is global
    /// to all its clients.
    ///
    /// It is requested by the PREFERENCE and not by the effective source:
    /// whoever chose to read the daemon is asking for the daemon's level
    /// even if right now it has not answered yet, and the answer to this
    /// call is precisely one of the two ways to find out whether it knows
    /// about logging.
    pub(super) fn nivel_de_registro(
        &mut self,
        nivel: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(nivel) = LogLevel::from_wire(nivel) else {
            // CLOSED vocabulary: an unrecognized one does not fall back to
            // `Info`, which would leave the panel showing something other
            // than what was requested.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-log-level-unknown".to_owned(),
                },
                Vec::new(),
            );
        };
        self.log_panel.show_level(nivel);
        if let Some(anillo) = &self.log_ring {
            anillo.raise_to(nivel);
        }
        // And only if there is someone to ask: for a daemon that already
        // said it has no ring, raising its level is an RPC per keystroke
        // whose answer is already known. It is the same condition that cuts
        // off polling (see `RegistroRemoto::debe_pedir`), and the TUI already
        // applied it here.
        if self.log_panel.source() != LogSource::Window && self.log_remoto.debe_pedir() {
            let backend = Arc::clone(backend);
            let buzon = buzon.clone();
            let epoca = self.log_epoca;
            let pedido = nivel.wire().to_owned();
            tokio::spawn(async move {
                let r = backend.log_level(pedido).await;
                let _ = buzon.send(Mensaje::RegistroNivel(epoca, Box::new(r))).await;
            });
        }
        self.repintar_registro()
    }

    /// Cycles the log's source (#328).
    ///
    /// With no second source it does nothing and does not repaint: switching
    /// between three views of the same ring would be a control promising
    /// something that does not exist. The action is still accepted rather
    /// than rejected — the renderer only sends it when the selector is on
    /// screen, and an `Unavailable` here would be a notice about a press
    /// nobody could have made.
    ///
    /// The guard is HERE and not the renderer's, and that was fixed: leaving
    /// it in `sources_available` was enough for nothing odd to show — the
    /// effective source collapses to `Window` anyway — but the PREFERENCE
    /// moved underneath a reader who cannot see it move, and it reappeared
    /// set to something else the day there really was a daemon. It is the
    /// same thing the TUI does, which also does not cycle without a serving
    /// daemon.
    pub(super) fn fuente_de_registro(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.log_remoto.servicio == Servicio::Sirve {
            self.log_panel.cycle_source();
        }
        self.repintar_registro()
    }

    /// The text filter over module and message.
    pub(super) fn filtro_de_registro(
        &mut self,
        texto: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if texto.len() > MAX_NOMBRE {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-name-too-long".to_owned(),
                },
                Vec::new(),
            );
        }
        self.log_panel.set_filter(texto);
        self.repintar_registro()
    }

    /// Scrolls up or down through the log, detaching from the tail.
    ///
    /// Detaching is half the panel: one that always jumps to the end cannot
    /// be read while something is writing, which is exactly when it is
    /// needed.
    pub(super) fn desplazar_registro(
        &mut self,
        delta: i64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let lineas = self
            .log_ring
            .as_ref()
            .map(norte_config::logring::LogRing::snapshot)
            .unwrap_or_default();
        // Over the MERGED list, which is what is seen: counting only the
        // local ones would leave the cap short and a page would not reach
        // the end.
        let visibles = norte_frontend::logpanel::merge(
            &lineas,
            &self.log_remoto.lineas,
            self.fuente_efectiva(),
        )
        .into_iter()
        .filter(|(l, _)| self.log_panel.matches(l))
        .count();
        let delta = isize::try_from(delta).unwrap_or(PAGINA);
        if delta < 0 {
            self.log_panel.scroll_up(delta.unsigned_abs(), visibles);
        } else {
            self.log_panel.scroll_down(delta.unsigned_abs(), visibles);
        }
        self.repintar_registro()
    }

    /// Sticks back to the end.
    pub(super) fn seguir_registro(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.log_panel.follow();
        self.repintar_registro()
    }

    /// How many rows fit, from the frame the renderer just painted.
    ///
    /// It sets this, it is not guessed here: in the TUI, guessing the height
    /// made every page skip two lines and the first one four, and what
    /// neither window showed could not be read at all.
    pub(super) fn filas_de_registro(
        &mut self,
        filas: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // With a floor and a CEILING. The floor, so a page moves something;
        // the ceiling, because this number is sent by the webview and
        // without it a `rows` of four billion would make every snapshot
        // carry the whole ring — two thousand lines per action, exactly what
        // decision D7 and `LogSlotView`'s rustdoc exist to prevent. The
        // listing's path is already capped the same way.
        let filas = (filas as usize).clamp(1, MAX_FILAS_REGISTRO);
        if filas == self.log_filas {
            // With no change there is no patch: the renderer sends this
            // every frame, and answering all of them would spend a sequence
            // number per frame.
            return (self.aplicada(), Vec::new());
        }
        self.log_filas = filas;
        self.log_panel.set_viewport_rows(filas);
        self.repintar_registro()
    }

    /// Repaints the log, if some slot is showing it.
    ///
    /// It goes as a SNAPSHOT and not a patch, for the same reason as the
    /// processes panel's cursor: there is no `ViewChange` for a slot that is
    /// not a listing, and adding one for this would be a new contract for
    /// what are one-off keys, not a continuous scroll.
    ///
    /// With no log slot at all, nothing is sent — the panel closes and an
    /// action in flight lands afterward — an extra snapshot spends a
    /// sequence number to paint the same thing.
    fn repintar_registro(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.huecos_de_registro().is_empty() {
            return (self.aplicada(), Vec::new());
        }
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Schedules the log's next poll, if there is a panel open.
    ///
    /// It only rearms while the panel stays on screen and turns off when it
    /// closes: a timer that outlived the panel would be waking up the actor
    /// every half second to paint nothing.
    ///
    /// `epoca` tells one opening from the next: opening, closing and
    /// reopening would leave two timers alive over the same panel, and the
    /// old one would keep rearming forever.
    pub(super) fn sondear_registro(&self, buzon: &mpsc::Sender<Mensaje>) {
        if self.huecos_de_registro().is_empty() {
            return;
        }
        let buzon = buzon.clone();
        let epoca = self.log_epoca;
        tokio::spawn(async move {
            tokio::time::sleep(SONDEO).await;
            let _ = buzon.send(Mensaje::RegistroTic(epoca)).await;
        });
    }

    /// The poll arrived: it only repaints if the ring has something new.
    ///
    /// The entry counter is an `AtomicU64` that only goes up, so the check
    /// touches neither the lock nor clones anything. Without it, this would
    /// be a full screen snapshot twice a second to paint the same thing.
    pub(super) fn tic_de_registro(
        &mut self,
        epoca: u64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if epoca != self.log_epoca {
            // From a previous opening: let it die without rearming.
            return Vec::new();
        }
        self.sondear_registro(buzon);
        // And along the way the daemon's log is pulled (#328), hung off
        // THIS timer and not one of its own: two clocks over the same panel
        // are two things to turn off on close, and the second one is the
        // one that gets forgotten. The answer comes back through the
        // mailbox, so the actor is still the only one writing.
        self.pedir_registro_remoto(backend, buzon);
        let ahora = self
            .log_ring
            .as_ref()
            .map_or(0, norte_config::logring::LogRing::pushed);
        if ahora == self.log_visto {
            return Vec::new();
        }
        self.log_visto = ahora;
        // Only whatever FOLLOWS the tail refreshes on its own. Whoever has
        // detached is reading something specific, and moving the list
        // underneath them is worse than not showing the new stuff — which
        // will still be there when they come back, anyway.
        if !self.log_panel.following() {
            return Vec::new();
        }
        let (_, salidas) = self.repintar_registro();
        salidas
    }

    /// Pulls the DAEMON's log from where it left off (#328).
    ///
    /// It is asked WHENEVER the panel is open, even with the source set to
    /// "this window": it is the only way to know whether there is a second
    /// source to offer, and therefore to decide whether the selector paints.
    /// It is one call every half second while someone is looking at the
    /// log; a closed panel costs nothing, which is where most of the time
    /// is spent.
    ///
    /// The epoch travels with the request: a close and an open fit between
    /// asking and answering, and the previous session's answer has to die
    /// instead of landing on the new panel.
    pub(super) fn pedir_registro_remoto(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        if self.huecos_de_registro().is_empty()
            // With one in flight another is not enqueued: a daemon slower
            // than half a second to answer would pile up one request per
            // tick forever.
            || self.log_remoto.en_vuelo
            // And a daemon that has already said it has no log is not asked
            // again. The negative CANNOT change while that daemon lives: it
            // comes from a compile-time feature or from a mount that failed
            // at startup. Continuing to poll was two RPCs per second,
            // forever, for an answer that cannot be any other.
            //
            // It is asymmetric on purpose. The POSITIVE case does need to
            // keep being asked — the log grows — and that is why `Sirve`
            // cuts off nothing.
            || !self.log_remoto.debe_pedir()
        {
            return;
        }
        self.log_remoto.en_vuelo = true;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let epoca = self.log_epoca;
        let cursor = self.log_remoto.cursor;
        tokio::spawn(async move {
            let r = backend.log_tail(cursor, MAX_REMOTO).await;
            let _ = buzon
                .send(Mensaje::RegistroRemoto(epoca, Box::new(r)))
                .await;
        });
    }

    /// Lands what the daemon answered to `log.tail`.
    pub(super) fn aterrizar_registro_remoto(
        &mut self,
        epoca: u64,
        res: Result<norte_proto::methods::LogTailResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if epoca != self.log_epoca {
            // From a previous opening. Neither its lines nor its cursor are
            // valid anymore, and `en_vuelo` belongs to the CURRENT opening:
            // touching it from here would disarm the guard of a request that
            // is still alive.
            return Vec::new();
        }
        self.log_remoto.en_vuelo = false;
        let antes = (self.log_remoto.servicio, self.log_remoto.nivel.clone());
        let nuevas = match res {
            Ok(r) => {
                self.log_remoto.servicio = Servicio::Sirve;
                self.log_remoto.nivel = Some(r.level);
                self.log_remoto.cursor = Some(r.next);
                self.log_remoto.perdidas = self.log_remoto.perdidas.saturating_add(r.lost);
                let n = r.lines.len();
                self.log_remoto
                    .lineas
                    .extend(r.lines.into_iter().map(linea_de_wire));
                // The cap applies from the front: the old stuff is what gets
                // dropped, same as in the ring, and it counts as lost — that
                // is what keeps the trim from leaving a silent gap.
                let sobra = self
                    .log_remoto
                    .lineas
                    .len()
                    .saturating_sub(MAX_LINEAS_REMOTAS);
                if sobra > 0 {
                    self.log_remoto.lineas.drain(..sobra);
                    self.log_remoto.perdidas = self
                        .log_remoto
                        .perdidas
                        .saturating_add(sobra.try_into().unwrap_or(u64::MAX));
                }
                n
            }
            // The ONLY degradation that can be reached: a daemon of the same
            // version without the `logging` feature. There is no version
            // comparison anywhere — an older one does not even complete
            // `initialize`, so it never gets this far.
            Err(Error::Unsupported) => {
                self.log_remoto.servicio = Servicio::SinAnillo;
                0
            }
            // Any failure — the connection dropped, the daemon is busy — is
            // NOT "this daemon has no log": saying so would be accusing of a
            // permanent lack something that fixes itself on the next round.
            // It is kept quiet and retried in half a second.
            Err(_) => 0,
        };
        let cambia_el_estado = antes != (self.log_remoto.servicio, self.log_remoto.nivel.clone());
        self.repintar_si_hace_falta(nuevas > 0, cambia_el_estado)
    }

    /// Lands the level the daemon really left set.
    pub(super) fn aterrizar_nivel_remoto(
        &mut self,
        epoca: u64,
        res: Result<String, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if epoca != self.log_epoca {
            return Vec::new();
        }
        let antes = (self.log_remoto.servicio, self.log_remoto.nivel.clone());
        match res {
            Ok(nivel) => {
                self.log_remoto.servicio = Servicio::Sirve;
                self.log_remoto.nivel = Some(nivel);
            }
            Err(Error::Unsupported) => self.log_remoto.servicio = Servicio::SinAnillo,
            Err(_) => {}
        }
        let cambia = antes != (self.log_remoto.servicio, self.log_remoto.nivel.clone());
        self.repintar_si_hace_falta(false, cambia)
    }

    /// The repaint rule shared by both of the daemon's answers.
    ///
    /// New lines only refresh whoever FOLLOWS the tail — moving the list
    /// underneath someone who has detached is worse than not showing them
    /// the new stuff — but a STATE change (a second source appeared, the
    /// daemon said it has no log, its level changed) always paints: it does
    /// not move the list and it is exactly what needs to be said.
    fn repintar_si_hace_falta(
        &mut self,
        hay_lineas: bool,
        cambia_el_estado: bool,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if cambia_el_estado || (hay_lineas && self.log_panel.following()) {
            let (_, salidas) = self.repintar_registro();
            return salidas;
        }
        Vec::new()
    }

    /// The slots that are painting the log right now.
    pub(super) fn huecos_de_registro(&self) -> Vec<u32> {
        self.reparto
            .placements
            .iter()
            .filter(|(s, _)| !self.huecos.contains_key(&s.0))
            .filter(|(s, _)| kind_de(&self.arbol, *s).is_some_and(|k| k.as_str() == KIND))
            .map(|(s, _)| s.0)
            .collect()
    }
}
