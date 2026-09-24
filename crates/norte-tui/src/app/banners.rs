//! The status bar's persistent notices: a connection's degradation (#44),
//! the journal's state (busy, recovered, absent) and the session's, plus the
//! phrase that summarizes the three.

use super::{App, JournalIndicator};
use norte_i18n::t;

impl App {
    /// Records a `connection.degraded` notification (#44).
    ///
    /// The rule — one entry per scheme, a repeat becomes the newest, capped
    /// at `DEGRADED_MAX` — lives in `norte_frontend::banners`: the graphical
    /// window has the same indicator, and two copies of a SECURITY notice
    /// are two places where the masking gets forgotten.
    pub fn note_degraded(&mut self, d: norte_proto::methods::ConnectionDegraded) {
        self.degraded.note(d);
    }

    /// The degradation reported for `scheme`, if any.
    ///
    /// This is the fact the help's [`norte_frontend::availability::Facts`]
    /// carries. It vetoes nothing on its own — see that field's rustdoc: the
    /// wire vocabulary means "unencrypted", not "unusable".
    #[must_use]
    pub fn degraded_for(&self, scheme: &str) -> Option<&norte_proto::methods::ConnectionDegraded> {
        self.degraded.for_scheme(scheme)
    }

    /// The persistent notice for plaintext connections, or `None` if there
    /// are none. The SHARED module composes the phrase.
    ///
    /// Never turns off once lit — see the `degraded` field for why that's a
    /// decision and not an oversight.
    #[must_use]
    pub fn connection_banner(&self) -> Option<String> {
        let b = self.degraded.banner(norte_i18n::active())?;
        // The TUI's status bar is ONE line of text, so here the phrase and
        // the connection do have to be joined — but not like a URL:
        // `scheme://host` turns `bank.example@evil.example` into something
        // that reads as the userinfo of a legitimate host. Labeled and
        // separated, which is what the rest of this frontend's modals
        // already do.
        //
        // The reason travels too (#279): without it, a reason this binary
        // doesn't know read exactly like "plaintext FTP".
        let line = norte_i18n::ta(
            "status-degraded-subject",
            &[
                ("banner", &b.text),
                ("scheme", &b.scheme),
                ("host", &b.host),
                ("reason", &b.reason),
            ],
        );
        // And the detail behind it, when there is one — which only happens
        // with an unknown reason, which is when the proto contract says to
        // lean on it. Masked and capped from the shared module.
        Some(match &b.detail {
            Some(d) => format!("{line}: {d}"),
            None => line,
        })
    }

    /// A connection could NOT be opened, and why (#322).
    ///
    /// Goes to `message` and not to the persistent indicator on purpose: the
    /// degradation describes a session that exists and keeps existing while
    /// it's being looked at, this describes an attempt that already ended.
    /// A permanent indicator over something that isn't open would never
    /// turn off.
    ///
    /// Overwrites the previous message because the previous one is, almost
    /// always, the CATEGORY of the same failure — `PermissionDenied` —
    /// which is exactly what #322 exists to improve. The SHARED module
    /// composes the phrase, for the usual rule: two copies of a notice
    /// about a connection are two places where the masking gets forgotten.
    pub fn note_connection_failed(&mut self, f: &norte_proto::methods::ConnectionFailed) {
        self.message = Some(norte_frontend::banners::failure_line(
            norte_i18n::active(),
            f,
        ));
    }

    /// Records a `plugin.notice` (0.69.0, ADR 0100): what a hook plugin said,
    /// attributed to it, as the transient status message. The SHARED module,
    /// which does the masking, composes the phrase.
    pub fn note_plugin_notice(&mut self, n: &norte_proto::methods::PluginNotice) {
        if let Some(l) = norte_frontend::banners::plugin_notice_line(norte_i18n::active(), n) {
            self.message = Some(l);
        }
    }

    /// Notes that this session isn't recording its mutations (#177).
    ///
    /// Idempotent: the core warns once per EPISODE, and if it ever warned
    /// twice, the second one just rewrites the same fact.
    pub fn note_no_journal(&mut self, why: norte_core::embedded::NoJournal) {
        self.no_journal = Some(JournalIndicator::NotRecorded(why));
    }

    /// The journal has been busy for minutes and there's NO daemon listening
    /// (#203).
    ///
    /// It's the SAME fact as a `Busy` — the session mutates without
    /// recording — with a different explanation, so it lights up the usual
    /// indicator and also marks that there's no longer an innocent reason at
    /// hand. The status bar says it with a different phrase: the soft one
    /// also comes out when nothing's wrong, and it's the one the reader has
    /// already learned not to look at.
    pub fn note_journal_squatted(&mut self) {
        self.no_journal = Some(JournalIndicator::Squatted);
    }

    /// And that it started recording again (#179): the ownership window
    /// reopened.
    ///
    /// Turning off the indicator is the half that matters. A "NOT being
    /// recorded" that doesn't know how to become "now it is" lies as soon as
    /// the passing occupant releases the file, and it lies about the only
    /// thing the status bar says about the WHOLE session.
    ///
    /// **What the indicator can't say** is that an operation already
    /// underway keeps the verdict it started with (#205): if the journal
    /// recovers while a long delete keeps running without recording, the
    /// status bar turns off and that delete still leaves no rows. The
    /// recovery notice spells it out — "starting with your NEXT operation" —
    /// but the next key erases it. Distinguishing this on the status bar
    /// would require the core to expose how many Tasks are pinned to
    /// not-recording, and it doesn't.
    pub fn note_journal_recovered(&mut self) {
        self.no_journal = None;
    }

    /// The PERSISTENT notice for a session with no journal, or `None` if it
    /// is being recorded.
    ///
    /// Fixed phrase and no reason: the reason went out via `message` when it
    /// happened (with the core's error sanitized), and the status bar has to
    /// fit.
    ///
    /// **TWO phrases, because they're two different facts (#178).** `Busy`
    /// is "this happened and wasn't recorded" — the session mutates, without
    /// recording. `Failed` is "this is NOT going to happen": the session
    /// refuses to mutate until the file gets fixed. Showing "can't be
    /// undone" for the second one would say the opposite of what's
    /// happening, and that kind of indicator is exactly what #178 came to
    /// remove.
    #[must_use]
    pub fn journal_banner(&self) -> Option<String> {
        use norte_core::embedded::NoJournal as N;
        self.no_journal.as_ref().map(|state| match state {
            // #203: the same fact as a `Busy` with a different explanation.
            // The soft phrase also comes out when there's a live daemon —
            // the usual case — so over an unexplained occupant it says too
            // little.
            JournalIndicator::Squatted => t("status-journal-squatted"),
            JournalIndicator::NotRecorded(N::Failed(_)) => t("status-journal-refused"),
            // `Busy` and any future reason: the conservative message is the
            // one that doesn't promise the mutation has stopped.
            JournalIndicator::NotRecorded(_) => t("status-no-journal"),
        })
    }

    /// The status bar's two persistent indicators, TOGETHER.
    ///
    /// Together and not in different branches of the status bar's `if`:
    /// they're two simultaneous facts of the same class — security, until
    /// the end of the session — so choosing one would hide the other
    /// forever. The journal's goes first: "none of this can be undone"
    /// outweighs "this connection is plaintext", and it's the only one that
    /// talks about the WHOLE session.
    #[must_use]
    pub fn persistent_banner(&self) -> Option<String> {
        let parts: Vec<String> = [
            self.journal_banner(),
            self.connection_banner(),
            self.session_banner(),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join("  "))
    }

    /// The PERSISTENT notice for a DETACHED window, or `None` if this one
    /// owns the session (#232).
    ///
    /// A detached window never writes: it's a second window, a core with no
    /// lock, or one that found a body from a newer version. It used to be
    /// said with a `message` at startup, and the first message that arrived
    /// afterward erased it — from then on the window stopped saving the
    /// screen with nothing to say so. Same discipline as the rest of this
    /// line: a state that lasts the whole session gets painted every frame,
    /// not once.
    #[must_use]
    pub fn session_banner(&self) -> Option<String> {
        self.session.detached.then(|| t("status-session-detached"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::pane::Pane;
    use crate::app::testutil::*;

    /// #44 used to save the degradation as already-formatted PROSE: the
    /// scheme and host got folded into the message and thrown away, so
    /// "which connection degraded?" had no answer. H3d needs it per pane.
    #[test]
    fn la_degradacion_se_guarda_por_scheme() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "example.org"));
        let d = app.degraded_for("sftp").expect("the degradation was kept");
        assert_eq!(
            d.host, "example.org",
            "the host survives, not just the phrase"
        );
        assert_eq!(d.reason, "ftp-plaintext");
        assert!(app.degraded_for("file").is_none());
    }

    /// #322: a connection failure reaches the status bar with its REASON,
    /// not with the error's category.
    ///
    /// And to `message`, not to the persistent indicator: there's no open
    /// session to keep warning about, so a permanent indicator would never
    /// turn off.
    #[test]
    fn un_fallo_de_conexion_dice_el_motivo_en_la_barra() {
        let mut app = app_dos_panes();
        app.note_connection_failed(&norte_proto::methods::ConnectionFailed {
            conn: Some("rosetta".to_owned()),
            scheme: "s3".to_owned(),
            host: "bucket.example".to_owned(),
            reason: "secret-empty".to_owned(),
            detail: Some("the secret is empty".to_owned()),
        });
        let msg = app.message.clone().expect("the status bar says so");
        assert!(msg.contains("bucket.example"), "{msg}");
        assert!(msg.contains("rosetta"), "{msg}");
        assert!(msg.contains(&t("failed-reason-secret-empty")), "{msg}");
        assert!(
            app.connection_banner().is_none(),
            "a failure doesn't light the PERSISTENT degradation indicator"
        );
    }

    /// ADR 0100: a hook's phrase reaches the status bar attributed to the
    /// plugin, and lights no persistent indicator.
    #[test]
    fn el_aviso_de_un_hook_llega_a_la_barra_con_su_plugin_delante() {
        let mut app = app_dos_panes();
        app.note_plugin_notice(&norte_proto::methods::PluginNotice {
            plugin_id: "org.norte.rename-log".to_owned(),
            kind: "notify".to_owned(),
            text: Some("renamed 3 files".to_owned()),
        });
        let msg = app.message.clone().expect("the status bar says so");
        assert!(msg.contains("org.norte.rename-log"), "{msg}");
        assert!(msg.contains("renamed 3 files"), "{msg}");
        assert!(app.connection_banner().is_none());
    }

    /// And two degraded connections don't step on each other: the last one
    /// used to win and the first vanished from the status bar without
    /// anything having resolved it.
    #[test]
    fn dos_degradaciones_conviven() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "a.org"));
        app.note_degraded(degradacion_de_test("ftp", "b.org"));
        assert!(app.degraded_for("sftp").is_some());
        assert!(app.degraded_for("ftp").is_some());
        // And the status bar stops lying about how many there are. It names
        // the LATEST one and says how many more: a bare count ("2 plaintext
        // connections"), with a notice that never clears, left the reader
        // NEVER able to find out which ones they were — and that's the only
        // question this indicator exists to answer.
        let banner = app.connection_banner().expect("there is a notice");
        assert!(
            banner.contains("b.org"),
            "the most recent one is named: {banner}"
        );
        assert!(
            banner.contains('1'),
            "and how many more there are: {banner}"
        );
    }

    /// #177: "this session isn't being recorded" has to survive the next
    /// key. It arrives ONCE, in the middle of an operation the user just
    /// launched, and `app.message` gets erased by the very next keystroke —
    /// which amounts to saying it was never announced.
    /// #203: an occupant with NO daemon to explain it is said with a
    /// different phrase.
    ///
    /// It's the same fact as a `Busy` — the session mutates without being
    /// recorded — and that's why the indicator stays lit; what changes is
    /// that the soft phrase also comes out when there's a live daemon, i.e.
    /// almost always, and it's the one the reader has already learned not to
    /// look at.
    #[test]
    fn el_ocupante_sin_daemon_tiene_su_propia_frase() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        let soft = app.journal_banner().expect("indicator lit");

        app.note_journal_squatted();
        let strong = app.journal_banner().expect("still lit");
        assert_ne!(soft, strong, "two different facts, two phrases");

        // And it turns off the same way: a recovery clears both.
        app.note_journal_recovered();
        assert!(app.journal_banner().is_none());

        // A later `Busy` goes back to the soft phrase and doesn't get stuck
        // with the strong one.
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        assert_eq!(app.journal_banner().as_deref(), Some(soft.as_str()));
    }

    #[test]
    fn la_sesion_sin_journal_tiene_indicador_persistente() {
        let mut app = app_dos_panes();
        assert!(app.journal_banner().is_none(), "recorded by default");

        app.message = Some("something".to_owned());
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        // What clears `message` in the run loop, key by key.
        app.message = None;
        assert!(
            app.journal_banner().is_some(),
            "the indicator doesn't leave with the message"
        );
    }

    /// And it doesn't compete with #44's: both are persistent, of the same
    /// class and simultaneous, so choosing one would hide the other for the
    /// rest of the session.
    #[test]
    fn los_dos_indicadores_persistentes_caben_juntos() {
        let mut app = app_dos_panes();
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        app.note_degraded(degradacion_de_test("sftp", "a.org"));
        let banner = app.persistent_banner().expect("there is a notice");
        assert!(
            banner.contains("a.org"),
            "the connection is still named: {banner}"
        );
        assert!(
            banner.starts_with(&app.journal_banner().expect("there is a journal_banner")),
            "and the journal's goes first: {banner}"
        );
    }

    /// #232: a DETACHED window says it once and then forgets.
    ///
    /// The startup message gets erased by the next key, and from then on
    /// the window doesn't save the screen with nothing on screen to say so.
    #[test]
    fn la_ventana_suelta_tiene_indicador_persistente() {
        let mut app = app_dos_panes();
        assert!(app.session_banner().is_none(), "the owner warns of nothing");

        app.session.detached = true;
        app.message = Some("something".to_owned());
        // What clears `message` in the run loop, key by key.
        app.message = None;
        let banner = app.persistent_banner().expect("there is a notice");
        assert_eq!(
            banner,
            app.session_banner().expect("there is a session_banner"),
            "with nothing else lit, the status bar is exactly that notice: {banner}"
        );
    }

    /// And it coexists with the other two: three simultaneous facts of the
    /// same class, and the session's is the one that weighs least, so it
    /// goes last.
    #[test]
    fn los_tres_indicadores_persistentes_caben_juntos() {
        let mut app = app_dos_panes();
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        app.note_degraded(degradacion_de_test("sftp", "a.org"));
        app.session.detached = true;
        let banner = app.persistent_banner().expect("there is a notice");
        assert!(
            banner.starts_with(&app.journal_banner().expect("there is a journal_banner")),
            "the journal's still goes first: {banner}"
        );
        assert!(
            banner.contains("a.org"),
            "the connection is still named: {banner}"
        );
        assert!(
            banner.ends_with(&app.session_banner().expect("there is a session_banner")),
            "and the session's closes it: {banner}"
        );
    }

    /// MINOR-5: #44's `Option<String>` was bounded by construction; a
    /// collection keyed off the WIRE isn't. The cap is generous — there are
    /// seven schemes — so only something anomalous reaches it, and when it
    /// does the oldest gets dropped and the one that just arrived is kept.
    #[test]
    fn las_degradaciones_tienen_tope() {
        let mut app = app_dos_panes();
        for i in 0..(norte_frontend::banners::DEGRADED_MAX + 10) {
            app.note_degraded(degradacion_de_test(&format!("s{i}"), "host"));
        }
        assert_eq!(app.degraded.len(), norte_frontend::banners::DEGRADED_MAX);
        assert!(
            app.degraded_for("s0").is_none(),
            "the oldest one is the one that drops"
        );
        assert!(
            app.degraded_for(&format!("s{}", norte_frontend::banners::DEGRADED_MAX + 9))
                .is_some(),
            "the last one to arrive stays"
        );
    }

    /// The OTHER end picks the host, and the status bar is the place it used
    /// to arrive raw while the rest of the TUI masks. A host with controls
    /// or bidi is exactly what gets sent to a security indicator to make it
    /// lie.
    #[test]
    fn el_aviso_enmascara_un_host_hostil() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "ma\u{202e}gro.org\n"));
        let banner = app.connection_banner().expect("there is a notice");
        assert!(
            !banner.contains('\u{202e}') && !banner.contains('\n'),
            "the host reached the status bar raw: {banner:?}"
        );
        assert!(
            banner.contains('\u{FFFD}'),
            "and the masking is VISIBLE (never a silent loss): {banner:?}"
        );
    }

    /// With no degradation there's no notice, and with ONE the notice is
    /// the usual one (#44): scheme and host, formatted from the structured
    /// value.
    #[test]
    fn el_aviso_de_una_sola_degradacion_nombra_la_conexion() {
        let mut app = app_dos_panes();
        assert!(app.connection_banner().is_none());
        app.note_degraded(degradacion_de_test("sftp", "remote.example"));
        let banner = app.connection_banner().expect("there is a notice");
        assert!(banner.contains("sftp"), "{banner}");
        assert!(banner.contains("remote.example"), "{banner}");
    }
}
