//! The status bar's PERSISTENT notices, shared.
//!
//! They live here and not in a frontend because they are pure presentation
//! over wire data, and because rule D14 of the new frontend's plan forbids
//! writing them twice: two implementations of a SECURITY indicator are two
//! places where the masking gets forgotten, and the graphical window would
//! inherit the version without it.

use norte_proto::methods::ConnectionDegraded;

/// How many degradations are retained at most.
///
/// It is a collection fed by the WIRE: a server reconnecting in a loop
/// would send one notice per attempt, and with no ceiling the status bar
/// turns into a channel for unbounded memory growth.
pub const DEGRADED_MAX: usize = 32;

/// How many cells the host is given before the middle ellipsis.
///
/// Long enough for a real FQDN, short enough that the notice does not push
/// everything else out of the bar.
const HOST_MAX: usize = 48;

/// The same for the scheme. Seven letters make a real scheme; the ceiling
/// exists because the wire can send anything.
const SCHEME_MAX: usize = 16;

/// How many cells an UNKNOWN reason's detail is given.
///
/// Short on purpose: it is free-form text from the wire, it helps orient
/// and is not for deciding, and what decides — the scheme and the host —
/// go in their own field.
const DETAIL_MAX: usize = 64;

/// The retained degradations, with their rule inside.
///
/// **The type exists so the rule is not optional.** Before, this was a
/// bare `VecDeque` and a free function that sorted it: the ceiling and the
/// dedupe key only applied if the caller went through it, and nothing
/// stopped a direct `push_back` that skipped both. With the data inside,
/// there is no way to write into the collection without going through
/// [`DegradedSet::note`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DegradedSet(std::collections::VecDeque<ConnectionDegraded>);

impl DegradedSet {
    /// Notes a degradation, ONE per SESSION.
    ///
    /// And a session is `(scheme, host)`, not `scheme`. Deduplicating by
    /// scheme alone was the TUI's old rule, justified by "the old one was
    /// talking about the same session": with two FTP connections to
    /// different hosts that is false, and the second notice ERASED the
    /// first, leaving the "and N others" count at zero. What disappeared
    /// was exactly the host the reader was not looking at, and "which
    /// one?" is the only question this indicator answers.
    ///
    /// Identity is decided by folding to ASCII lowercase: `FTP` and `ftp`
    /// from the wire are the same session. The BYTES that are painted are
    /// the latest report's — folding is for deciding, not for rewriting
    /// what is shown.
    ///
    /// Past [`DEGRADED_MAX`] the oldest one is dropped.
    pub fn note(&mut self, d: ConnectionDegraded) {
        let key =
            |x: &ConnectionDegraded| (x.scheme.to_ascii_lowercase(), x.host.to_ascii_lowercase());
        let new_key = key(&d);
        self.0.retain(|old| key(old) != new_key);
        self.0.push_back(d);
        while self.0.len() > DEGRADED_MAX {
            self.0.pop_front();
        }
    }

    /// The degradation reported for `scheme`, if there is one: the most
    /// recent.
    ///
    /// It does not veto anything by itself — the wire's vocabulary says
    /// "unencrypted", not "unusable".
    #[must_use]
    pub fn for_scheme(&self, scheme: &str) -> Option<&ConnectionDegraded> {
        self.0.iter().rev().find(|d| d.scheme == scheme)
    }

    /// How many degraded sessions are retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// None at all?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The persistent notice, or `None` if there is no degradation.
    #[must_use]
    pub fn banner(&self, lang: norte_i18n::Lang) -> Option<DegradedBanner> {
        connection_banner(lang, &self.0)
    }
}

/// The persistent notice of plaintext connections, or `None` if there is
/// none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DegradedBanner {
    /// The sentence, already translated, with NOTHING from the wire inside.
    pub text: String,
    /// The scheme, masked.
    pub scheme: String,
    /// The host, masked and shortened.
    pub host: String,
    /// WHY it is degraded, already translated (#279).
    ///
    /// The wire's vocabulary is closed and comparable by equality, and can
    /// GROW additively. A reason this binary does not know falls into the
    /// generic sentence — "degraded session" — instead of inheriting
    /// `ftp-plaintext`'s, which is what used to happen: a newer daemon
    /// reporting a NEW degradation read as "plaintext FTP", i.e. a
    /// security notice asserting something nobody had said.
    pub reason: String,
    /// The wire's human detail, masked and clipped, and ONLY when the
    /// reason is unknown.
    ///
    /// It is what the proto's contract asks for: faced with an unknown
    /// `reason`, degrade "leaning on `detail`". With a known reason it
    /// adds nothing and would be free-form text from the other end inside
    /// a security indicator, so it does not travel.
    pub detail: Option<String>,
    /// What is painted differs from what there is, in the scheme, the
    /// host or the detail.
    pub hostile: bool,
}

/// The reason's i18n key, or `None` if this binary does not know it.
///
/// A `match` over the wire's CLOSED vocabulary and not a
/// `format!("degraded-reason-{reason}")`: composing the key with the other
/// end's string lets a daemon choose which catalogue message is painted,
/// and an invented reason would come out as its own raw identifier in the
/// bar.
fn key_of_reason(reason: &str) -> Option<&'static str> {
    match reason {
        "ftp-plaintext" => Some("degraded-reason-ftp-plaintext"),
        "tls-auth-rejected" => Some("degraded-reason-tls-auth-rejected"),
        _ => None,
    }
}

/// Composes the notice: the sentence on one side and the connection on the
/// other.
///
/// It always NAMES a connection — the most recent — and says how many
/// more there are. A bare count ("2 plaintext connections") loses every
/// one's identity, and "which one?" is the only question this indicator
/// exists to answer.
///
/// **The connection is NOT interpolated into the sentence**, and that is
/// not style. Building `{scheme}://{host}` into the text turns a host like
/// `bank.example@evil.example` — which `Authority::new` accepts, and which
/// carries not a single character that gets masked — into something that
/// reads as the userinfo of a legitimate host, in the indicator where
/// lying is worth the most. Each part in its own field, and whoever
/// paints it keeps them separate.
///
/// The language goes as a PARAMETER and is not read from the global one:
/// the graphical window has one per instance, and a security notice in
/// another window's language is a notice that does not get read.
///
/// The scheme and host are masked and the host is shortened; the flag
/// says it was done, because what is masked is disclosed.
#[must_use]
pub fn connection_banner(
    lang: norte_i18n::Lang,
    degraded: &std::collections::VecDeque<ConnectionDegraded>,
) -> Option<DegradedBanner> {
    let last = degraded.back()?;
    let (scheme, scheme_hostile) = crate::display_name(last.scheme.as_bytes());
    let (host_paintable, host_hostile) = crate::display_name(last.host.as_bytes());
    // The scheme is also clipped: the ceiling was only on the host, and a
    // sixty-kilobyte scheme pushes everything else out of the bar.
    let scheme = crate::middle_ellipsis(&scheme, SCHEME_MAX);
    let host = crate::middle_ellipsis(&host_paintable, HOST_MAX);
    let others = degraded.len() - 1;
    let text = if others == 0 {
        norte_i18n::t_in(lang, "status-connection-degraded")
    } else {
        norte_i18n::ta_in(
            lang,
            "status-connections-degraded",
            &[("n", &others.to_string())],
        )
    };
    // The REASON of the one named (#279). An unknown reason does not
    // inherit `ftp-plaintext`'s sentence: it falls into the generic one
    // and leans on `detail`, which is what the proto's contract asks for.
    let known = key_of_reason(&last.reason);
    let reason = norte_i18n::t_in(lang, known.unwrap_or("degraded-reason-unknown"));
    let (detail, detail_hostile) = match (known, last.detail.as_deref()) {
        (None, Some(d)) if !d.is_empty() => {
            let (paintable, hostile) = crate::display_name(d.as_bytes());
            (
                Some(crate::middle_ellipsis(&paintable, DETAIL_MAX)),
                hostile,
            )
        }
        _ => (None, false),
    };
    Some(DegradedBanner {
        text,
        scheme,
        host,
        reason,
        detail,
        hostile: scheme_hostile || host_hostile || detail_hostile,
    })
}

/// Why a connection COULD NOT be made, already composed for painting
/// (#322).
///
/// Same shape as [`DegradedBanner`] and for the same reason: it is a
/// notice about a CONNECTION, and the authority is not interpolated into
/// the sentence. What changes is the moment — the degradation describes a
/// session that exists, this describes one that never came to exist — and
/// that is why it is not retained: there is nothing open left to keep
/// warning about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureNotice {
    /// The sentence, already translated, with NOTHING from the wire inside.
    pub text: String,
    /// The `connections.toml` name, masked and shortened, if there was one.
    ///
    /// It is the only identifier the human WROTE, so it is the one they
    /// recognize; norte deduced the scheme and the host. It goes in its
    /// own field like everything else: it comes from the configuration
    /// file, which is as untrustworthy a source as the wire for what gets
    /// painted.
    pub conn: Option<String>,
    /// The scheme, masked.
    pub scheme: String,
    /// The host, masked and shortened.
    pub host: String,
    /// WHY it failed, already translated. Closed vocabulary; an unknown
    /// one falls into the generic sentence and leans on `detail`.
    pub reason: String,
    /// The wire's human detail, masked and clipped, and ONLY when the
    /// reason is unknown — same as in [`DegradedBanner::detail`]. With a
    /// known reason the translated sentence already says it, and the
    /// detail would be text from the other end repeating it in the
    /// daemon's language.
    pub detail: Option<String>,
    /// What is painted differs from what there is, in some field.
    pub hostile: bool,
}

/// The i18n key of a failure's reason, or `None` if this binary does not
/// know it.
///
/// A `match` over the closed vocabulary and not a `format!`, for the same
/// reason as [`key_of_reason`]: composing the key with the other end's
/// string lets the daemon choose which catalogue message is painted.
fn key_of_failure(reason: &str) -> Option<&'static str> {
    match reason {
        "secret-missing" => Some("failed-reason-secret-missing"),
        "secret-empty" => Some("failed-reason-secret-empty"),
        "secret-not-utf8" => Some("failed-reason-secret-not-utf8"),
        "secret-store" => Some("failed-reason-secret-store"),
        "auth-rejected" => Some("failed-reason-auth-rejected"),
        "no-user" => Some("failed-reason-no-user"),
        "agent" => Some("failed-reason-agent"),
        "rsa-too-small" => Some("failed-reason-rsa-too-small"),
        _ => None,
    }
}

/// Composes the notice for a connection failure (#322).
///
/// The same rules as [`connection_banner`], and not for symmetry: they are
/// what stops a host like `bank.example@evil.example` from reading as the
/// userinfo of a legitimate host. A connection failure is exactly where
/// someone would want it read that way — "I couldn't get into your bank,
/// enter your password again".
#[must_use]
pub fn failure_notice(
    lang: norte_i18n::Lang,
    f: &norte_proto::methods::ConnectionFailed,
) -> FailureNotice {
    let (scheme, scheme_hostile) = crate::display_name(f.scheme.as_bytes());
    let (host_paintable, host_hostile) = crate::display_name(f.host.as_bytes());
    let scheme = crate::middle_ellipsis(&scheme, SCHEME_MAX);
    let host = crate::middle_ellipsis(&host_paintable, HOST_MAX);
    let (conn, conn_hostile) = match f.conn.as_deref() {
        Some(c) if !c.is_empty() => {
            let (paintable, hostile) = crate::display_name(c.as_bytes());
            (Some(crate::middle_ellipsis(&paintable, HOST_MAX)), hostile)
        }
        _ => (None, false),
    };
    let known = key_of_failure(&f.reason);
    let reason = norte_i18n::t_in(lang, known.unwrap_or("failed-reason-unknown"));
    let (detail, detail_hostile) = match (known, f.detail.as_deref()) {
        (None, Some(d)) if !d.is_empty() => {
            let (paintable, hostile) = crate::display_name(d.as_bytes());
            (
                Some(crate::middle_ellipsis(&paintable, DETAIL_MAX)),
                hostile,
            )
        }
        _ => (None, false),
    };
    FailureNotice {
        text: norte_i18n::t_in(lang, "status-connection-failed"),
        conn,
        scheme,
        host,
        reason,
        detail,
        hostile: scheme_hostile || host_hostile || detail_hostile || conn_hostile,
    }
}

/// The line for ONE connection failure, ready to paint (#322).
///
/// Lives here and not in each frontend by rule D14, and this time with a
/// reason that has already cost a mistake: the TUI and the window have to
/// say the SAME thing about why a machine could not be reached, and two
/// compositions silently diverge (ADR 0077). The window sends it as a
/// `Notice`'s `detail`, the TUI puts it in its bar: same text, two places.
///
/// The authority is LABELED — "scheme X, host Y" — and never as
/// `scheme://host`: see [`connection_banner`]'s rustdoc for what is gained
/// by lying with the second form.
#[must_use]
pub fn failure_line(lang: norte_i18n::Lang, f: &norte_proto::methods::ConnectionFailed) -> String {
    let n = failure_notice(lang, f);
    let line = norte_i18n::ta_in(
        lang,
        "status-failed-subject",
        &[
            ("banner", &n.text),
            ("scheme", &n.scheme),
            ("host", &n.host),
            ("reason", &n.reason),
        ],
    );
    // The `connections.toml` name AFTER, never before. It is the only
    // identifier the human wrote and that is why it travels — but it
    // comes from a file, and `display_name` does not mask what is
    // printable: a name like `bank.example» — ✗ could not connect` put up
    // front reads as a COMPLETE notice about another machine, with the
    // real one pushed behind. Behind the reason it cannot impersonate
    // anything: what comes before it was already written by norte.
    let line = match &n.conn {
        Some(c) => format!("{line} — \u{ab}{c}\u{bb}"),
        None => line,
    };
    match &n.detail {
        Some(d) => format!("{line}: {d}"),
        None => line,
    }
}

/// How many cells a plugin's id is given in a notice. A real reverse-DNS
/// id fits; the ceiling exists because the wire can send anything.
const PLUGIN_ID_MAX: usize = 48;

/// How many cells a hook's sentence is given. The daemon already clipped
/// it to 200 characters; this is the width of a status bar.
const PLUGIN_TEXT_MAX: usize = 160;

/// The sentence for a `plugin.notice` (0.69.0, ADR 0100), or `None` if
/// there is nothing to show: an unknown class with no text.
///
/// Shared for the same reason as the ones above: it is a third party's
/// text (the hook) attributed to an id that also comes from the wire, and
/// two frontends are two places where the masking gets forgotten. The id
/// goes IN FRONT and labeled by norte — "⚑ org.x.y: …" — so the plugin's
/// sentence cannot pass itself off as norte's.
#[must_use]
pub fn plugin_notice_line(
    lang: norte_i18n::Lang,
    n: &norte_proto::methods::PluginNotice,
) -> Option<String> {
    let (id, _) = crate::display_name(n.plugin_id.as_bytes());
    let id = crate::middle_ellipsis(&id, PLUGIN_ID_MAX);
    let text = n.text.as_deref().map(|t| {
        let (paintable, _) = crate::display_name(t.as_bytes());
        crate::middle_ellipsis(&paintable, PLUGIN_TEXT_MAX)
    });
    match (n.kind.as_str(), text) {
        ("hooks-disabled", _) => Some(norte_i18n::ta_in(
            lang,
            "msg-plugin-hooks-disabled",
            &[("plugin", &id)],
        )),
        ("effect-denied", _) => Some(norte_i18n::ta_in(
            lang,
            "msg-plugin-effect-denied",
            &[("plugin", &id)],
        )),
        // `notify`, and any class this binary does not know but that
        // carries text: the proto's contract says to lean on it.
        (_, Some(text)) => Some(norte_i18n::ta_in(
            lang,
            "msg-plugin-notice",
            &[("plugin", &id), ("text", &text)],
        )),
        (_, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No hostile AUTHORITY from the corpus reaches the SENTENCE whole
    /// (#277).
    ///
    /// What this notice exists to answer is "which one?", and every way of
    /// lying about that lives in the authority: userinfo before an `@`, an
    /// in-band sentence, a bidi override, Cyrillic homoglyphs, two FQDNs
    /// that collide when cut, and a scheme with no ceiling. The claim is
    /// the same for all six and is structural, not case-by-case: the
    /// sentence does NOT contain the authority, each part goes in its own
    /// field, what is masked is disclosed and what is cut is clipped.
    #[test]
    fn no_hostile_authority_from_the_corpus_gets_into_the_sentence() {
        let hosts = norte_testkit::corpus::hostile_hosts();
        assert!(hosts.len() >= 6, "the canonical corpus does not shrink");
        for h in &hosts {
            let mut queue = std::collections::VecDeque::new();
            queue.push_back(degradation(h.scheme, h.host));
            let notice =
                connection_banner(norte_i18n::Lang::Es, &queue).expect("there is a notice");
            assert!(
                !notice.text.contains(h.host),
                "[{}] the authority got interpolated into the sentence: {:?}",
                h.id,
                notice.text
            );
            // Both CLIPPED: the ceiling was only on the host, and a long
            // scheme pushes the part that identifies the machine out of
            // the bar.
            assert!(notice.scheme.chars().count() <= SCHEME_MAX, "[{}]", h.id);
            assert!(notice.host.chars().count() <= HOST_MAX, "[{}]", h.id);
            // And what is masked is disclosed.
            let altered = notice.scheme != h.scheme && !notice.scheme.contains('\u{2026}')
                || notice.host != h.host && !notice.host.contains('\u{2026}');
            assert!(
                !altered || notice.hostile,
                "[{}] it was altered and does not say so: {:?} / {:?}",
                h.id,
                notice.scheme,
                notice.host
            );
            // The pair that collides when cut still gets distinguished at
            // 48 cells, and the cut is MARKED when there is one.
            if let Some(twin) = h.twin {
                let mut other = std::collections::VecDeque::new();
                other.push_back(degradation(h.scheme, twin));
                let b = connection_banner(norte_i18n::Lang::Es, &other).expect("there is a notice");
                if notice.host == b.host {
                    assert!(
                        notice.host.contains('\u{2026}'),
                        "[{}] two different authorities paint the same with NO cut mark",
                        h.id
                    );
                }
            }
        }
    }

    fn degradation(scheme: &str, host: &str) -> ConnectionDegraded {
        ConnectionDegraded {
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            reason: "ftp-plaintext".to_owned(),
            detail: None,
        }
    }

    fn failure(scheme: &str, host: &str, reason: &str) -> norte_proto::methods::ConnectionFailed {
        norte_proto::methods::ConnectionFailed {
            conn: None,
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            reason: reason.to_owned(),
            detail: None,
        }
    }

    /// #322: the failure notice inherits the degradation's rules, and that
    /// is checked with the SAME corpus. A connection failure is the place
    /// with the biggest prize for lying about which machine was tried.
    #[test]
    fn no_hostile_authority_from_the_corpus_gets_into_a_failure_sentence() {
        let hosts = norte_testkit::corpus::hostile_hosts();
        assert!(hosts.len() >= 6, "the canonical corpus does not shrink");
        for h in &hosts {
            let notice = failure_notice(
                norte_i18n::Lang::Es,
                &failure(h.scheme, h.host, "auth-rejected"),
            );
            assert!(
                !notice.text.contains(h.host),
                "[{}] the authority got interpolated into the sentence: {:?}",
                h.id,
                notice.text
            );
            assert!(notice.scheme.chars().count() <= SCHEME_MAX, "[{}]", h.id);
            assert!(notice.host.chars().count() <= HOST_MAX, "[{}]", h.id);
            let altered = notice.scheme != h.scheme && !notice.scheme.contains('\u{2026}')
                || notice.host != h.host && !notice.host.contains('\u{2026}');
            assert!(
                !altered || notice.hostile,
                "[{}] it was altered and does not say so: {:?} / {:?}",
                h.id,
                notice.scheme,
                notice.host
            );
        }
    }

    /// The reason is translated by CLOSED vocabulary: one this binary does
    /// not know falls into the generic sentence and does NOT inherit the
    /// neighboring one's.
    ///
    /// It is the same rule #279 put on the degradation, and for the same
    /// reason: a newer daemon reporting a new reason would read as the old
    /// reason, i.e. a notice asserting something nobody said.
    #[test]
    fn an_unknown_reason_falls_into_the_generic_one_and_leans_on_the_detail() {
        let known = failure_notice(
            norte_i18n::Lang::Es,
            &failure("sftp", "example.test", "secret-empty"),
        );
        assert_eq!(
            known.reason,
            norte_i18n::t_in(norte_i18n::Lang::Es, "failed-reason-secret-empty")
        );

        let mut f = failure("sftp", "example.test", "reason-from-the-future");
        f.detail = Some("something this binary cannot name".to_owned());
        let odd = failure_notice(norte_i18n::Lang::Es, &f);
        assert_eq!(
            odd.reason,
            norte_i18n::t_in(norte_i18n::Lang::Es, "failed-reason-unknown"),
            "an unknown reason cannot inherit another one's sentence"
        );
        assert_eq!(
            odd.detail.as_deref(),
            Some("something this binary cannot name"),
            "with no known reason, the detail is the only thing that orients"
        );
        assert!(
            known.detail.is_none(),
            "with a known reason the detail is redundant: it would be the same sentence in the daemon's language"
        );
    }

    /// This binary knows how to translate ALL the vocabulary the proto
    /// declares.
    ///
    /// The other half of the closure (the first is in `norte-core`: what
    /// the core emits is what the proto declares). Without this, forgetting
    /// an i18n key when adding a reason was invisible: `key_of_failure`
    /// returning `None` is indistinguishable from "a newer daemon", so the
    /// new reason painted "unknown" forever and nothing turned red.
    #[test]
    fn all_failure_vocabulary_is_translated() {
        for reason in norte_proto::methods::CONNECTION_FAILURE_REASONS {
            let key = key_of_failure(reason).unwrap_or_else(|| {
                panic!("the proto declares {reason:?} and it is not translated here")
            });
            for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
                let sentence = norte_i18n::t_in(lang, key);
                assert_ne!(
                    sentence, key,
                    "[{lang:?}] {key} is not in the catalogue: its own identifier would come out"
                );
            }
        }
    }

    /// The `connections.toml` name goes in its own FIELD, clipped and
    /// masked.
    ///
    /// The human wrote it, but into a file: a name with a line break or a
    /// bidi override inside breaks the bar just like one from the wire.
    #[test]
    fn the_connections_name_is_clipped_and_masked() {
        let mut f = failure("s3", "example.test", "auth-rejected");
        f.conn = Some(format!("my\u{202e}conn{}", "x".repeat(200)));
        let notice = failure_notice(norte_i18n::Lang::Es, &f);
        let conn = notice.conn.expect("the name travels");
        assert!(
            conn.chars().count() <= HOST_MAX,
            "no ceiling: {}",
            conn.len()
        );
        assert!(!conn.contains('\u{202e}'), "the bidi override is masked");
        assert!(notice.hostile, "it was altered and did not say so");
    }

    /// A repeated scheme does not take up two slots, and the one named is
    /// the last report.
    #[test]
    fn the_same_session_repeated_replaces_its_entry() {
        let mut d = DegradedSet::default();
        d.note(degradation("ftp", "one.example"));
        d.note(degradation("FTP", "ONE.example"));
        assert_eq!(d.len(), 1, "the wire's casing does not make two sessions");
        let notice = d.banner(norte_i18n::active()).expect("there is a notice");
        assert_eq!(notice.host, "ONE.example", "paints the latest one's bytes");
    }

    /// Two HOSTS of the same scheme are two sessions: the second cannot
    /// erase the first, because what disappears is exactly the one that
    /// is not being looked at.
    #[test]
    fn two_hosts_of_the_same_scheme_do_not_step_on_each_other() {
        let mut d = DegradedSet::default();
        d.note(degradation("ftp", "bank.example"));
        d.note(degradation("ftp", "other.example"));
        assert_eq!(d.len(), 2);
        let notice = d.banner(norte_i18n::active()).expect("there is a notice");
        assert_eq!(notice.host, "other.example");
        assert!(
            notice.text.contains('1'),
            "and it counts the other: {}",
            notice.text
        );
    }

    /// With several, one is named AND how many more there are is said.
    #[test]
    fn with_several_one_is_named_and_the_others_are_counted() {
        let mut d = DegradedSet::default();
        d.note(degradation("ftp", "one.example"));
        d.note(degradation("sftp", "two.example"));
        let notice = d.banner(norte_i18n::active()).expect("there is a notice");
        assert_eq!(notice.host, "two.example");
        assert!(notice.text.contains('1'), "{}", notice.text);
    }

    /// A host with control characters does NOT reach the bar raw: it is a
    /// string from the wire, and this is a security indicator.
    #[test]
    fn a_hostile_host_is_masked() {
        let mut d = DegradedSet::default();
        d.note(degradation("ftp", "ba\u{7}d\u{202e}.example"));
        let notice = d.banner(norte_i18n::active()).expect("there is a notice");
        assert!(
            !notice.host.contains('\u{7}') && !notice.host.contains('\u{202e}'),
            "{notice:?}"
        );
        assert!(
            notice.hostile,
            "and it IS SAID that it was masked: {notice:?}"
        );
    }

    /// With no degradations there is no notice.
    #[test]
    fn no_degradations_means_no_notice() {
        assert!(
            DegradedSet::default()
                .banner(norte_i18n::active())
                .is_none()
        );
    }

    /// **An UNKNOWN reason does not inherit `ftp-plaintext`'s sentence**
    /// (#279).
    ///
    /// The wire's vocabulary can grow, and before this a newer daemon
    /// reporting a new degradation read as "plaintext FTP": a security
    /// indicator asserting something nobody had said.
    #[test]
    fn an_unknown_reason_falls_into_the_generic_sentence() {
        let mut d = DegradedSet::default();
        let mut new_one = degradation("sftp", "one.example");
        new_one.reason = "quantum-downgrade".to_owned();
        new_one.detail = Some("the server negotiated an old profile".to_owned());
        d.note(new_one);
        let notice = d.banner(norte_i18n::Lang::Es).expect("there is a notice");

        let known = {
            let mut d = DegradedSet::default();
            d.note(degradation("ftp", "one.example"));
            d.banner(norte_i18n::Lang::Es)
                .expect("there is a notice")
                .reason
        };
        assert_ne!(notice.reason, known, "cannot be read as plaintext FTP");
        assert_eq!(
            notice.detail.as_deref(),
            Some("the server negotiated an old profile"),
            "and it leans on `detail`, which is what the proto asks for"
        );
    }

    /// With a KNOWN reason the detail does not travel: it adds nothing and
    /// would be free-form text from the other end inside a security
    /// indicator.
    #[test]
    fn a_known_reason_does_not_drag_the_detail_along() {
        let mut d = DegradedSet::default();
        let mut known = degradation("ftp", "one.example");
        known.detail = Some("anything at all".to_owned());
        d.note(known);
        let notice = d.banner(norte_i18n::Lang::Es).expect("there is a notice");
        assert_eq!(notice.detail, None);
    }

    /// The detail is a string from the WIRE, so it goes through the same
    /// masking as the host — and when it is altered, it is disclosed.
    #[test]
    fn an_unknown_reasons_detail_is_masked() {
        let mut d = DegradedSet::default();
        let mut new_one = degradation("sftp", "one.example");
        new_one.reason = "new".to_owned();
        new_one.detail = Some("ba\u{7}d\u{202e}".to_owned());
        d.note(new_one);
        let notice = d.banner(norte_i18n::Lang::Es).expect("there is a notice");
        let detail = notice.detail.clone().expect("there is a detail");
        assert!(
            !detail.contains('\u{7}') && !detail.contains('\u{202e}'),
            "{notice:?}"
        );
        assert!(notice.hostile, "and it is disclosed: {notice:?}");
    }

    /// The ceiling ALWAYS applies, because there is no longer a way to
    /// write into the collection without going through `note`: it was the
    /// hole in #279's point 4.
    #[test]
    fn the_ceiling_cannot_be_dodged() {
        let mut d = DegradedSet::default();
        for i in 0..(DEGRADED_MAX + 10) {
            d.note(degradation("ftp", &format!("h{i}.example")));
        }
        assert_eq!(d.len(), DEGRADED_MAX);
        let notice = d.banner(norte_i18n::active()).expect("there is a notice");
        let last = DEGRADED_MAX + 9;
        assert_eq!(
            notice.host,
            format!("h{last}.example"),
            "and the last one is still there"
        );
    }

    /// ADR 0100: a hook's sentence carries the id IN FRONT, masked; an
    /// unknown class leans on the text and with no text nothing is shown.
    #[test]
    fn a_plugins_notice_is_attributed_and_masked() {
        let n = norte_proto::methods::PluginNotice {
            plugin_id: "org.norte.rename-log".to_owned(),
            kind: "notify".to_owned(),
            text: Some("renamed 3 files\u{1b}[31m\u{202e}".to_owned()),
        };
        let l = plugin_notice_line(norte_i18n::Lang::En, &n).expect("there is a sentence");
        assert!(l.starts_with("⚑ org.norte.rename-log"), "{l}");
        assert!(l.contains("renamed 3 files"), "{l}");
        assert!(
            !l.contains('\u{1b}') && !l.contains('\u{202e}'),
            "masked: {l}"
        );

        let off = norte_proto::methods::PluginNotice {
            plugin_id: "org.norte.rename-log".to_owned(),
            kind: "hooks-disabled".to_owned(),
            text: None,
        };
        let l = plugin_notice_line(norte_i18n::Lang::Es, &off).expect("there is a sentence");
        assert_eq!(
            l,
            norte_i18n::ta_in(
                norte_i18n::Lang::Es,
                "msg-plugin-hooks-disabled",
                &[("plugin", "org.norte.rename-log")]
            )
        );

        let odd = norte_proto::methods::PluginNotice {
            plugin_id: "org.x".to_owned(),
            kind: "sing".to_owned(),
            text: None,
        };
        assert!(plugin_notice_line(norte_i18n::Lang::En, &odd).is_none());
    }

    /// Every class the proto declares has a sentence in both languages: a
    /// new value in `PLUGIN_NOTICE_KINDS` with no translation turns red
    /// here, not in someone's bar.
    #[test]
    fn every_declared_class_has_a_sentence() {
        // With NO text: a class with no translation falls into the
        // "unknown with no text" arm and returns `None`, which is what
        // turns this red. With text, any class paints something, and it
        // would prove nothing.
        for kind in norte_proto::methods::PLUGIN_NOTICE_KINDS {
            let n = norte_proto::methods::PluginNotice {
                plugin_id: "org.x.y".to_owned(),
                kind: (*kind).to_owned(),
                text: if *kind == "notify" {
                    Some("t".to_owned())
                } else {
                    None
                },
            };
            for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                let l = plugin_notice_line(lang, &n).unwrap_or_else(|| panic!("{kind}"));
                assert!(l.contains("org.x.y"), "{kind}: {l}");
            }
        }
    }
}
