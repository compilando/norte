//! Whether a command can run RIGHT NOW, and why not.
//!
//! One table, two frontends. The GUI dims a context-menu entry and the TUI
//! dims a help row for the same reasons, and they must agree: a menu that
//! greys out "copy" while the help page says it is available is worse than
//! either alone. The vocabulary is [`norte_help::Reason`], so the third
//! consumer — the help corpus' own rendering — reads the same answers.
//!
//! The table is a function of FACTS the caller gathers, never of state it
//! reaches for: a verdict computed while a menu is open must not change under
//! the reader's cursor, and the frontends disagree about how to compute some
//! of the facts (a `.zip` is enterable in the TUI and not in the GUI).
//!
//! What the table deliberately does NOT decide:
//!
//! - **Labels and painting.** [`verdict`] is keyed by command id and returns a
//!   verdict; each frontend keeps its own wording and its own dim style.
//! - **Which commands exist.** An id the table has no arm for is available
//!   (see [`verdict`]): the table is a list of known IMPEDIMENTS, not a
//!   registry of commands.
//! - **Impediments of STATE.** This is the boundary of what dimming MEANS
//!   here, and it is a decision rather than a gap. The table models
//!   impediments of BACKEND (the location refuses mutation) and of TARGET (the
//!   entry the command would act on is the wrong kind, or there are too many
//!   of them). It never models "there is nothing to do right now": `task.cancel`
//!   with no task running, `nav.parent` at the root, `nav.back` and
//!   `nav.forward` on an empty trail are all knowable no-ops in common states,
//!   and all four stay lit. Two reasons. A trail that is empty AT THIS INSTANT
//!   is not the same kind of fact as a backend that cannot write — the first
//!   changes with the next keystroke and the second needs the reader to go
//!   somewhere else — and a reader who learns "dimmed means it does not apply
//!   here" from one row must not meet a row where it meant "not yet". The
//!   frontends have all of these facts in hand and could fill them cheaply;
//!   the reason they do not is this paragraph, not the cost. Pinned by
//!   `la_tabla_no_modela_impedimentos_de_estado`.
//!
//! One veto is knowable, in scope, and DEFERRED: `pane.open` (the TUI's F4)
//! refuses anything the openers cannot resolve to a native path, and
//! [`Reason::Unsupported`] already has its Fluent strings. It needs a seventh
//! field in [`Facts`] — "the focused entry has a native path" — which no
//! caller computes today, so it arrives with the change that adds it rather
//! than being quietly missing.

use norte_help::{Availability, Reason};

/// What the frontend knows about the current context and the table needs in
/// order to decide what can run.
///
/// Every field is a boolean the CALLER computes, never raw state the table
/// interprets. That is not indirection for its own sake: the frontends answer
/// some of these questions differently and both answers are right. In the TUI
/// a `.zip` file is enterable (`nav.enter` composes an archive scheme onto
/// it); in the GUI it is not, because the GUI has no archive composition on
/// Enter. A `kind: EntryKind` in here would force the table to pick one of the
/// two and be wrong in the other frontend. [`Facts::rename_single`] is the
/// second instance of exactly that, so the pattern is the rule here and not an
/// exception made once.
///
/// (`clippy::struct_excessive_bools`: allowed on purpose. These are seven
/// INDEPENDENT observations about one moment, not the states of a machine —
/// any combination of them is a real context, so there is no enum to collapse
/// them into. Wrapping each in a two-variant enum would make every call site
/// read `Enterable::No, Viewable::Yes, RenameSingle::Yes` for no gain: the
/// field names already say which question each answer belongs to.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "field names already say which question each answer belongs to"
)]
pub struct Facts {
    /// The focused entry can be entered (a directory, or an archive the
    /// frontend knows how to compose a scheme for).
    pub enterable: bool,
    /// The focused entry has something to show in a viewer.
    pub viewable: bool,
    /// Exactly one entry will be renamed.
    ///
    /// Not "exactly one entry is selected", because the two frontends TARGET
    /// differently: the GUI's rename refuses a multiple selection, while the
    /// TUI's renames the entry under the CURSOR and ignores the marks
    /// entirely. Same shape as [`Facts::enterable`] — the table cannot know, so
    /// the caller says. The field is deliberately about the rename and not
    /// about the selection: a generic `single` read by this one arm invited the
    /// TUI to fill it from its marked set, which dimmed `pane.rename` for a
    /// batch the TUI renames one entry of quite happily.
    pub rename_single: bool,
    /// The pane the command reads FROM refuses mutation.
    pub source_read_only: bool,
    /// The pane the command writes TO refuses mutation.
    pub dest_read_only: bool,
    /// The connection behind the acting pane is degraded.
    ///
    /// Gathered but NOT a veto today, and that is a decision rather than an
    /// omission: `connection.degraded` on the wire means the session is
    /// UNENCRYPTED (`tls-auth-rejected`, `ftp-plaintext` — see
    /// `norte_proto::methods::ConnectionDegraded`), not that it cannot act. A
    /// plaintext FTP session copies, moves and deletes perfectly well, so
    /// dimming those rows would tell every FTP user that the app refuses what
    /// it is about to do — over-dimming, which misleads exactly as much as
    /// not dimming at all. It stays in [`Facts`] because the caller already
    /// has it and because a future wire reason that DOES prevent acting (an
    /// unreachable or reconnecting session) belongs in this slot without
    /// changing the shape of the struct.
    pub degraded: bool,
    /// Mutations through this backend are recorded in a journal, so they can
    /// be undone.
    ///
    /// `false` for the TUI's in-process engine — the one `norte-tui` builds
    /// without `--daemon`. Since #167 that engine DOES carry the state
    /// directory's journal, but it still installs no sync spool, and `sync.plan`
    /// refuses without one; this field gates synchronising, which needs both, so
    /// it stays `false` and the name undersells what it answers. It is an
    /// impediment of BACKEND and not of state, which is why it belongs here
    /// and not in the paragraph this module's docs write against: it does not
    /// change with the next keystroke, the reader has to start norte
    /// differently.
    ///
    /// Only `pane.sync-dirs` reads it today. Copying, moving and deleting
    /// stay lit without a journal because they have always worked that way
    /// and dimming them would be a new claim about a pre-existing gap;
    /// synchronising is the first mutation the core itself refuses without one
    /// (`sync.apply` needs the journal to open a batch), so this is the field
    /// that says so before the reader presses the key.
    pub journalled: bool,
}

/// Fluent id for a reason, WITHOUT a frontend prefix: both frontends read
/// these keys now, so a `gui-` one would have the TUI shipping GUI strings.
///
/// The `match` has a wildcard ON PURPOSE: [`Reason`] is `#[non_exhaustive]`,
/// so a variant added later must not break the build of every frontend. It
/// falls back to `reason-unavailable` ("unavailable right now"), which is
/// safe because it is true of every reason by construction — a row is only
/// asked for its key when it is already unavailable, so the fallback loses
/// detail and never states anything false.
#[must_use]
pub fn reason_key(reason: Reason) -> &'static str {
    match reason {
        Reason::ReadOnlyBackend => "reason-read-only",
        Reason::Unsupported => "reason-unsupported",
        Reason::PluginInactive => "reason-plugin-inactive",
        Reason::PolicyDenied => "reason-policy-denied",
        Reason::ConnectionDegraded => "reason-connection-degraded",
        Reason::WrongTarget => "reason-wrong-target",
        Reason::NeedsDaemon => "reason-needs-daemon",
        Reason::AnsweredByTheOverlay => "reason-answered-by-overlay",
        _ => "reason-unavailable",
    }
}

/// Whether a location on this scheme refuses mutation, decided SYNTACTICALLY.
///
/// Today that means "inside an archive" (`zip+file`, `tar+gz+file`…, ADR
/// 0018/0028): the archive provider announces `READ_ONLY` and no mutation ever
/// leaves it, so the scheme alone is enough to know.
///
/// It exists for the moment BEFORE the capability flags have arrived. A
/// frontend that caches `Capabilities` per connection should prefer the flags
/// and fall back here (see `norte_tui::app::App::pane_read_only`); one that
/// caches nothing has only this.
///
/// It dims only what is read-only BY CONSTRUCTION, and everything else it
/// reports writable: a read-only SFTP export and an S3 bucket the credentials
/// cannot write to both come back `false` here. That is the safe direction and
/// it is the reason the fallback is allowed to be this crude — every wrong
/// answer offers an operation that then refuses with a real error the reader
/// sees, whereas the opposite mistake, dimming a location that would have
/// accepted the write, teaches the reader that the app cannot do something it
/// can and is not corrected by anything.
///
/// ```
/// use norte_frontend::availability::scheme_is_read_only;
///
/// assert!(scheme_is_read_only("zip+file"));
/// assert!(scheme_is_read_only("tar+gz+file"));
/// assert!(!scheme_is_read_only("file"));
/// assert!(!scheme_is_read_only("s3"));
/// ```
#[must_use]
pub fn scheme_is_read_only(scheme: &str) -> bool {
    norte_proto::scheme_archive_format(scheme).is_some()
}

/// Whether a location refuses mutation: the flags if they have arrived, the
/// scheme if they have not.
///
/// The two-step answer both frontends need, in ONE place. Each had written it
/// out — `norte_tui::app::App::pane_read_only` and the GUI host's
/// `solo_lectura` — which is the shape ADR 0077 exists to stop: two spellings
/// of one decision, drifting quietly. `enter_target` moved here for the same
/// reason and in the same change.
///
/// The ORDER is the decision. The provider's own answer wins, because it knows
/// about a read-only export or an `ro` mount that no scheme can express; the
/// scheme answers only while nothing has come back, and it errs towards
/// writable (see [`scheme_is_read_only`] for why that direction is the safe
/// one).
///
/// ```
/// use norte_frontend::availability::read_only;
/// use norte_proto::{Capabilities, CapabilityFlags};
///
/// let ro = Capabilities { flags: CapabilityFlags::READ_ONLY, max_path: None };
/// let rw = Capabilities { flags: CapabilityFlags::CASE_SENSITIVE, max_path: None };
///
/// // El flag manda, en los dos sentidos.
/// assert!(read_only(Some(ro), "sftp"));
/// assert!(!read_only(Some(rw), "sftp"));
/// // Sin respuesta todavía, contesta el esquema.
/// assert!(read_only(None, "zip+file"));
/// assert!(!read_only(None, "sftp"));
/// ```
#[must_use]
pub fn read_only(caps: Option<norte_proto::Capabilities>, scheme: &str) -> bool {
    caps.map_or_else(
        || scheme_is_read_only(scheme),
        |c| c.flags.contains(norte_proto::CapabilityFlags::READ_ONLY),
    )
}

/// Deshabilitado por `reason`.
fn no(reason: Reason) -> Availability {
    Availability::Unavailable { reason }
}

/// `Available` si `ok`, si no deshabilitado por `reason`.
fn gated(ok: bool, reason: Reason) -> Availability {
    if ok {
        Availability::Available
    } else {
        no(reason)
    }
}

/// El PRIMER motivo cuya condición se cumple, o `Available` si ninguna. El
/// orden de la lista es el de importancia: con dos impedimentos a la vez se
/// explica el que el usuario tendría que resolver primero (soltar marcas no
/// hace escribible un zip).
fn first_failure(checks: &[(bool, Reason)]) -> Availability {
    match checks.iter().find(|(hit, _)| *hit) {
        Some((_, reason)) => no(*reason),
        None => Availability::Available,
    }
}

/// Whether `command` can run under `facts`, and why not.
///
/// An id with no arm here is [`Availability::Available`]. That fail-OPEN
/// default is the opposite of the usual one and it is deliberate: the table
/// cannot know every command in the vocabulary (plugins contribute their own,
/// and the help corpus names commands this crate never sees), so dimming by
/// default would turn every new command into one the help declares broken.
/// Offering it and letting it fail honestly beats denying it out of ignorance.
///
/// ```
/// use norte_frontend::availability::{Facts, verdict};
/// use norte_help::Reason;
///
/// let en_un_zip = Facts {
///     enterable: false,
///     viewable: true,
///     rename_single: true,
///     source_read_only: true,
///     dest_read_only: false,
///     degraded: false,
///     journalled: true,
/// };
/// // Se lee DESDE el zip: copiar vale.
/// assert!(verdict("pane.copy", &en_un_zip).is_available());
/// // Se escribe DENTRO del zip: borrar no.
/// assert_eq!(
///     verdict("pane.delete", &en_un_zip).reason(),
///     Some(Reason::ReadOnlyBackend)
/// );
/// // Un comando que la tabla no conoce se ofrece.
/// assert!(verdict("app.quit", &en_un_zip).is_available());
/// ```
#[must_use]
pub fn verdict(command: &str, facts: &Facts) -> Availability {
    match command {
        // Entrar: el llamador dice qué es entrable, y los dos frontends lo
        // preguntan al mismo sitio ([`crate::nav::enter_target`]) — un
        // directorio, un enlace y un contenedor, que es al que compone el
        // scheme. La tabla no vuelve a mirar el recuento: el frontend que
        // exige UNA sola entrada ya lo dobló en el hecho.
        "nav.enter" => gated(facts.enterable, Reason::WrongTarget),
        "pane.view" => gated(facts.viewable, Reason::WrongTarget),
        // Copiar LEE del origen (un zip vale) y ESCRIBE en el destino.
        "pane.copy" => gated(!facts.dest_read_only, Reason::ReadOnlyBackend),
        // Mover escribe en los DOS: borra en el origen.
        "pane.move" => gated(
            !facts.dest_read_only && !facts.source_read_only,
            Reason::ReadOnlyBackend,
        ),
        // Renombrar de verdad (`pane.rename`, shift+F6): UNA entrada. CUÁL y
        // si con varias marcas cuenta como una lo dice el llamador
        // (`rename_single`), porque los dos frontends apuntan distinto — la
        // GUI se niega con selección múltiple, la TUI renombra la del cursor
        // ignorando las marcas.
        //
        // El ORDEN de estos dos checks es la decisión, no un detalle: dentro
        // de un zip con tres marcas, «es de solo lectura» es lo que el
        // usuario tendría que resolver primero (soltar las marcas no hace
        // escribible un zip), así que gana el backend.
        "pane.rename" => first_failure(&[
            (facts.source_read_only, Reason::ReadOnlyBackend),
            (!facts.rename_single, Reason::WrongTarget),
        ]),
        // Los dos escriben en el ORIGEN y sólo el origen los veta. El rename
        // de IA además actúa sobre la CARPETA entera, no sobre el objetivo
        // señalado, así que a diferencia de `pane.rename` el recuento no le
        // afecta (por eso no comparte arm con él).
        //
        // `pane.delete-permanent` (shift+F8) es del vocabulario de la TUI y no
        // del menú de la GUI, pero se veta con el MISMO criterio: borrar
        // saltándose la papelera sigue siendo escribir en el origen. Sin este
        // brazo la página de copiado atenuaba F8 y dejaba shift+F8 encendido
        // dentro de un zip — dos filas contiguas contándose lo contrario.
        //
        // `pane.mkdir` (F7) crea DENTRO del pane con foco, que es el origen:
        // mismo veto y por la misma razón. Sin brazo caía en el fail-OPEN y el
        // lector llegaba a teclear el nombre en el modal antes de que el
        // despacho fallara.
        // Organizar (fase 8) CREA carpetas y mueve ficheros dentro del pane
        // con foco: mismo veto que renombrar, y por la misma razón.
        "pane.ai-rename"
        | "pane.organize"
        | "pane.delete"
        | "pane.delete-permanent"
        | "pane.mkdir" => gated(!facts.source_read_only, Reason::ReadOnlyBackend),
        // Sincronizar (spec 2 del ítem 1): escribe en el DESTINO —como copiar—
        // y además borra y sobrescribe allí, así que el core exige journal
        // (`sync.apply` abre un lote deshacible; regla dura 4) y se niega sin
        // él. El orden es la decisión, igual que en `pane.rename`: sin daemon
        // no hay nada que el lector pueda arreglar quedándose donde está, y
        // «este destino no escribe» es un consejo para una sesión que sí podría
        // sincronizar. Sin este brazo caía en el fail-OPEN y la hoja de
        // referencia ofrecía la tecla que el engine embebido rechaza.
        "pane.sync-dirs" => first_failure(&[
            (!facts.journalled, Reason::NeedsDaemon),
            (facts.dest_read_only, Reason::ReadOnlyBackend),
        ]),
        // Aquí caen dos cosas distintas, y conviene no confundirlas al leer:
        // los comandos que NO tienen impedimento posible (`pane.copy-path` no
        // toca el backend — vale hasta dentro de un zip; `app.quit` tampoco) y
        // los que esta tabla no conoce, que se ofrecen por el fail-OPEN de la
        // rustdoc de arriba. No llevan arm propio porque el veredicto sería
        // idéntico y clippy no admite el arm redundante; el test
        // `una_conexion_degradada_no_veta_por_si_sola` fija el de
        // `pane.copy-path`.
        _ => Availability::Available,
    }
}

/// The plugin id inside a palette dispatch key, `plugin:{id}:{command}`.
///
/// `None` for anything that is not one of those keys. A key with the prefix
/// but no `id:command` after it returns `None` too, and the caller treats that
/// as inactive: a malformed key names no plugin, so there is nothing that
/// could run it.
///
/// # Where the boundary is
///
/// The FIRST `:` after the prefix, and that is not a coin toss. The two halves
/// are validated differently and the split has to follow the asymmetry: the
/// core restricts `plugin_id` to reverse-DNS (`[A-Za-z0-9-]` segments, so
/// never a `:`), while `command_id` comes out of the manifest with NO charset
/// validation and may carry any byte, colons included — see
/// [`crate::palette::Row`], which builds these keys. Splitting on the LAST
/// colon, or splitting more than once, would attribute
/// `plugin:acme.ftp:do:it` to a plugin that does not exist.
///
/// The one input where this disagrees with `norte_help`'s `is_own_command` is
/// a plugin id that itself contains a `:`. That crate refuses such an id
/// outright (it makes the split ambiguous, so it fails closed and the plugin
/// gets no command rows at all); this function cannot see the ambiguity from
/// the key alone and reports the segment before the first colon. The
/// divergence is documented rather than papered over because it is
/// unreachable from a validated id and because the two failures point the
/// same way in practice: a `help.md` whose plugin id carries a colon yields
/// zero command rows, so there is no verdict left for this function to get
/// wrong on that path.
///
/// ```
/// use norte_frontend::availability::plugin_of_command;
///
/// assert_eq!(plugin_of_command("plugin:acme.ftp:sync"), Some("acme.ftp"));
/// // The command id may carry colons; the plugin id may not.
/// assert_eq!(plugin_of_command("plugin:acme.ftp:do:it"), Some("acme.ftp"));
/// // Not a plugin key, or not a whole one.
/// assert_eq!(plugin_of_command("pane.copy"), None);
/// assert_eq!(plugin_of_command("plugin:acme.ftp"), None);
/// assert_eq!(plugin_of_command("plugin:"), None);
/// ```
#[must_use]
pub fn plugin_of_command(command: &str) -> Option<&str> {
    let rest = command.strip_prefix("plugin:")?;
    let (id, cmd) = rest.split_once(':')?;
    (!id.is_empty() && !cmd.is_empty()).then_some(id)
}

/// [`verdict`], plus the arm for plugin-contributed commands (H3e).
///
/// `active` is the set of plugin ids that are approved AND enabled — the
/// frontend's SNAPSHOT, taken when the help opened. A `plugin:` key whose
/// plugin is not in it is [`Reason::PluginInactive`]: the row stays visible,
/// because the reader is looking at that plugin's own page and "it is here but
/// switched off" is the answer they came for, and it dims because
/// `plugin.run_command` would refuse it.
///
/// The palette does NOT get this row — it filters an inactive plugin's
/// commands out entirely (see [`crate::palette::plugin_rows`]), and that
/// decision stands. A list of everything you can run has no business showing
/// what you cannot; a page ABOUT one plugin has every business saying that
/// this is the plugin's own command and it is switched off.
///
/// Malformed `plugin:` keys are inactive rather than available: the fail-OPEN
/// default of [`verdict`] exists for commands this table does not KNOW, and a
/// key that names no plugin is not unknown, it is broken.
///
/// ```
/// use norte_frontend::availability::{Facts, verdict_with_plugins};
/// use norte_help::Reason;
/// use std::collections::BTreeSet;
///
/// let facts = Facts {
///     enterable: false,
///     viewable: true,
///     rename_single: true,
///     source_read_only: false,
///     dest_read_only: false,
///     degraded: false,
///     journalled: true,
/// };
/// let activos: BTreeSet<String> = ["acme.ftp".to_owned()].into_iter().collect();
///
/// assert!(verdict_with_plugins("plugin:acme.ftp:sync", &facts, &activos).is_available());
/// assert_eq!(
///     verdict_with_plugins("plugin:otro:sync", &facts, &activos).reason(),
///     Some(Reason::PluginInactive)
/// );
/// // A built-in command never looks at the set.
/// assert!(verdict_with_plugins("pane.copy", &facts, &BTreeSet::new()).is_available());
/// ```
#[must_use]
pub fn verdict_with_plugins(
    command: &str,
    facts: &Facts,
    active: &std::collections::BTreeSet<String>,
) -> Availability {
    if command.starts_with("plugin:") {
        let ok = plugin_of_command(command).is_some_and(|id| active.contains(id));
        return gated(ok, Reason::PluginInactive);
    }
    verdict(command, facts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_help::Reason;

    fn one_file() -> Facts {
        Facts {
            enterable: false,
            viewable: true,
            rename_single: true,
            source_read_only: false,
            dest_read_only: false,
            degraded: false,
            journalled: true,
        }
    }

    #[test]
    fn copiar_hacia_un_destino_de_solo_lectura_esta_vetado() {
        let f = Facts {
            dest_read_only: true,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.copy", &f).reason(),
            Some(Reason::ReadOnlyBackend)
        );
        // Y al revés: leer DESDE un origen de solo lectura es el caso que la
        // función existe para permitir — copiar de un .zip a un bucket.
        let desde_zip = Facts {
            source_read_only: true,
            ..one_file()
        };
        assert!(verdict("pane.copy", &desde_zip).is_available());
    }

    #[test]
    fn renombrar_reporta_el_primer_fallo_no_el_ultimo() {
        // El orden es la decisión: con un lote de 3 ficheros dentro de un
        // zip, «es de solo lectura» explica más que «hay más de uno».
        let f = Facts {
            rename_single: false,
            source_read_only: true,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.rename", &f).reason(),
            Some(Reason::ReadOnlyBackend)
        );
    }

    /// La MISMA divergencia que `enterable`, sobre renombrar: la GUI se niega
    /// con selección múltiple, la TUI renombra la del cursor e ignora las
    /// marcas. Con un `single` genérico decidiendo este brazo, la ayuda de la
    /// TUI atenuaba shift+F6 en cuanto hubiese dos marcas — una fila apagada
    /// para algo que la app hace sin pestañear, que es justo el fallo que H3d
    /// existe para no cometer.
    #[test]
    fn renombrar_lo_decide_el_llamador_no_el_recuento() {
        let tui_con_marcas = Facts {
            rename_single: true,
            ..one_file()
        };
        assert!(
            verdict("pane.rename", &tui_con_marcas).is_available(),
            "la TUI renombra la del cursor: no se atenúa"
        );
        let gui_con_marcas = Facts {
            rename_single: false,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.rename", &gui_con_marcas).reason(),
            Some(Reason::WrongTarget),
            "la GUI se niega con selección múltiple"
        );
    }

    #[test]
    fn entrar_lo_decide_el_llamador_no_el_tipo_de_entrada() {
        // La divergencia REAL entre frontends: en la TUI un .zip se ENTRA
        // (se compone el scheme), en la GUI no. Por eso el hecho es
        // «entrable», no «es un directorio».
        let zip = Facts {
            enterable: true,
            ..one_file()
        };
        assert!(verdict("nav.enter", &zip).is_available());
        assert_eq!(
            verdict("nav.enter", &one_file()).reason(),
            Some(Reason::WrongTarget)
        );
    }

    #[test]
    fn un_comando_que_la_tabla_no_conoce_esta_disponible() {
        // Fail-OPEN a propósito, y es lo contrario de lo que suele pedirse:
        // la tabla no puede saber de cada comando del vocabulario, y atenuar
        // por defecto convertiría cada comando nuevo en un comando que la
        // ayuda declara roto. Ofrecerlo y que falle honestamente es mejor
        // que negarlo por ignorancia.
        assert!(verdict("app.quit", &one_file()).is_available());
        assert!(verdict("no.such.command", &one_file()).is_available());
    }

    #[test]
    fn cada_razon_tiene_clave_fluent_y_ninguna_se_solapa() {
        use std::collections::BTreeSet;
        let mut vistas = BTreeSet::new();
        for r in [
            Reason::ReadOnlyBackend,
            Reason::Unsupported,
            Reason::PluginInactive,
            Reason::PolicyDenied,
            Reason::ConnectionDegraded,
            Reason::WrongTarget,
            Reason::NeedsDaemon,
        ] {
            let k = reason_key(r);
            assert!(!k.is_empty(), "{r:?} sin clave");
            assert!(vistas.insert(k), "clave repetida: {k}");
        }
    }

    /// Toda clave que [`reason_key`] nombra existe en los DOS locales — el
    /// test de paridad de `norte-i18n` cubre el catálogo entero, esto cubre
    /// que estas claves son claves REALES y no un typo que llegaría a la UI
    /// como su propio id.
    #[test]
    fn las_claves_de_los_motivos_existen_en_ambos_locales() {
        for clave in [
            "reason-read-only",
            "reason-unsupported",
            "reason-plugin-inactive",
            "reason-policy-denied",
            "reason-connection-degraded",
            "reason-wrong-target",
            "reason-needs-daemon",
            "reason-unavailable",
        ] {
            for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
                assert_ne!(
                    norte_i18n::t_in(lang, clave),
                    clave,
                    "falta {clave} en {lang:?}"
                );
            }
        }
    }

    /// Borrar sin papelera se veta como borrar: dentro de un zip las DOS
    /// filas de la página de copiado (F8 y shift+F8) tienen que decir lo
    /// mismo — la que quedase encendida prometería la más destructiva.
    #[test]
    fn borrar_permanente_se_veta_como_borrar() {
        let dentro_de_un_zip = Facts {
            source_read_only: true,
            ..one_file()
        };
        for cmd in ["pane.delete", "pane.delete-permanent"] {
            assert_eq!(
                verdict(cmd, &dentro_de_un_zip).reason(),
                Some(Reason::ReadOnlyBackend),
                "{cmd} ofrecido dentro de un backend de solo lectura"
            );
        }
    }

    /// MAJOR-3(a): crear un directorio ESCRIBE en el pane con foco, así que se
    /// veta con el mismo criterio que borrar. Sin brazo caía en el fail-OPEN y
    /// dentro de un zip la ayuda ofrecía F7: el lector teclea un nombre en el
    /// modal, lo confirma y el despacho falla. Es el MISMO argumento que la
    /// propia fase usó para añadir `pane.delete-permanent` — dos filas
    /// contiguas contándose lo contrario.
    #[test]
    fn crear_directorio_se_veta_como_escribir() {
        let dentro_de_un_zip = Facts {
            source_read_only: true,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.mkdir", &dentro_de_un_zip).reason(),
            Some(Reason::ReadOnlyBackend),
        );
        assert!(
            verdict("pane.mkdir", &one_file()).is_available(),
            "fuera de un backend de solo lectura se ofrece"
        );
    }

    /// MAJOR-3(b): la tabla modela impedimentos de BACKEND y de OBJETIVO, y
    /// jamás de ESTADO. Estos cuatro son no-ops CONOCIDOS en estados comunes
    /// —nada corriendo, en la raíz, sin rastro— y aun así se ofrecen: un
    /// rastro que está vacío AHORA no es la misma clase de hecho que un
    /// backend que no sabe escribir, y el lector que ve una fila apagada
    /// aprende «esto no se puede aquí», no «esto no tiene nada que hacer
    /// todavía». La decisión está escrita en la rustdoc del módulo; este test
    /// es dónde se cambia si algún día se decide lo contrario.
    #[test]
    fn la_tabla_no_modela_impedimentos_de_estado() {
        for cmd in ["task.cancel", "nav.parent", "nav.back", "nav.forward"] {
            assert!(
                verdict(cmd, &one_file()).is_available(),
                "{cmd} atenuado por un impedimento de ESTADO"
            );
        }
    }

    /// Sincronizar sin journal se APAGA, y con un motivo sobre el que se puede
    /// actuar: arranca norte contra el daemon. No es un impedimento de estado
    /// —no cambia con la siguiente tecla— sino de backend, que es la clase que
    /// esta tabla sí modela. El engine embebido de la TUI no tiene journal ni
    /// spool y `sync.apply` se niega en cerrado (regla dura 4), así que sin
    /// este brazo la hoja de referencia ofrecía una tecla muerta — que es
    /// exactamente lo que #159 acaba de costar una vez.
    #[test]
    fn sincronizar_sin_journal_manda_al_daemon() {
        let embebida = Facts {
            journalled: false,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.sync-dirs", &embebida).reason(),
            Some(Reason::NeedsDaemon)
        );
        assert!(
            verdict("pane.sync-dirs", &one_file()).is_available(),
            "con journal se ofrece"
        );
        // Y comparar NO se apaga por lo mismo: leer los dos árboles no muta
        // nada, así que no necesita journal. Dos comandos vecinos que dicen
        // cosas distintas porque son cosas distintas.
        assert!(verdict("pane.compare-dirs", &embebida).is_available());
    }

    /// Con daemon pero contra un destino que no escribe, el motivo es el del
    /// destino. El ORDEN importa: sin daemon no hay nada que el lector arregle
    /// quedándose donde está, así que ese gana aunque los dos se cumplan.
    #[test]
    fn sincronizar_reporta_el_primer_fallo_no_el_ultimo() {
        let hacia_un_zip = Facts {
            dest_read_only: true,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.sync-dirs", &hacia_un_zip).reason(),
            Some(Reason::ReadOnlyBackend)
        );
        let ninguna_de_las_dos = Facts {
            dest_read_only: true,
            journalled: false,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.sync-dirs", &ninguna_de_las_dos).reason(),
            Some(Reason::NeedsDaemon)
        );
    }

    fn activos(ids: &[&str]) -> std::collections::BTreeSet<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn el_comando_de_un_plugin_apagado_se_atenua() {
        let v = verdict_with_plugins(
            "plugin:acme.ftp:sync",
            &one_file(),
            &activos(&["otro.plugin"]),
        );
        assert_eq!(v.reason(), Some(Reason::PluginInactive));
    }

    #[test]
    fn el_comando_de_un_plugin_activo_se_ofrece() {
        let v = verdict_with_plugins("plugin:acme.ftp:sync", &one_file(), &activos(&["acme.ftp"]));
        assert!(v.is_available());
    }

    #[test]
    fn un_comando_del_binario_no_mira_los_plugins() {
        let v = verdict_with_plugins("pane.copy", &one_file(), &activos(&[]));
        assert!(v.is_available(), "el prefijo `plugin:` es lo que decide");
    }

    #[test]
    fn una_clave_de_plugin_malformada_no_se_ofrece() {
        // `plugin:` sin id ni comando no identifica nada: fail-closed, porque
        // el único despacho posible sería contra un plugin que no existe.
        let v = verdict_with_plugins("plugin:", &one_file(), &activos(&["acme.ftp"]));
        assert_eq!(v.reason(), Some(Reason::PluginInactive));
    }

    /// La frontera la marca el PRIMER `:` tras `plugin:`, y eso no es un
    /// detalle: `plugin_id` es DNS inverso validado por el core (nunca lleva
    /// `:`), mientras que `command_id` sale del manifiesto SIN validación de
    /// charset y puede llevar los que quiera. Partir por el último, o partir
    /// más de una vez, atribuiría `plugin:acme.ftp:do:it` a un plugin que no
    /// existe y atenuaría una fila que sí se puede ejecutar.
    #[test]
    fn el_id_del_comando_puede_llevar_dos_puntos() {
        assert_eq!(
            plugin_of_command("plugin:acme.ftp:do:it"),
            Some("acme.ftp"),
            "la frontera es el primer `:`, no el último"
        );
        assert!(
            verdict_with_plugins(
                "plugin:acme.ftp:do:it",
                &one_file(),
                &activos(&["acme.ftp"])
            )
            .is_available()
        );
    }

    /// Toda forma que no nombra un plugin Y un comando es `None`, y el
    /// veredicto de todas ellas es el mismo: apagada. La lista es el contrato
    /// —lo que la tabla considera «roto» frente a «desconocido»— y por eso se
    /// enumera aquí y no se deduce de la implementación.
    #[test]
    fn las_claves_que_no_nombran_plugin_y_comando_son_none() {
        for clave in [
            "plugin:",
            "plugin::",
            "plugin:acme.ftp",
            "plugin:acme.ftp:",
            "plugin::sync",
        ] {
            assert_eq!(plugin_of_command(clave), None, "{clave} identificó algo");
            assert_eq!(
                verdict_with_plugins(clave, &one_file(), &activos(&["acme.ftp", ""])).reason(),
                Some(Reason::PluginInactive),
                "{clave} ofrecida"
            );
        }
        // Y lo que no lleva el prefijo no es asunto suyo.
        assert_eq!(plugin_of_command("pane.copy"), None);
        assert_eq!(plugin_of_command("plugins:acme.ftp:sync"), None);
    }

    /// El criterio SINTÁCTICO: un scheme compuesto de archivo es de solo
    /// lectura por construcción; uno de provider, no.
    #[test]
    fn el_scheme_de_archivo_es_de_solo_lectura() {
        assert!(scheme_is_read_only("zip+file"));
        assert!(scheme_is_read_only("tar+file"));
        assert!(scheme_is_read_only("tar+gz+file"));
        assert!(!scheme_is_read_only("file"));
        assert!(!scheme_is_read_only("sftp"));
        assert!(!scheme_is_read_only("s3"));
        assert!(!scheme_is_read_only("mem"));
    }

    /// Una conexión degradada NO veta nada, y eso está fijado a propósito:
    /// `connection.degraded` significa «sesión sin cifrar», no «sesión
    /// inservible». Atenuar copiar/mover/borrar por ello le diría a todo
    /// usuario de FTP que la app se niega a hacer lo que va a hacer. Si algún
    /// día el motivo del wire pasa a significar «no se puede actuar», este
    /// test es el sitio donde la decisión se cambia a la vista.
    #[test]
    fn una_conexion_degradada_no_veta_por_si_sola() {
        let f = Facts {
            degraded: true,
            ..one_file()
        };
        for cmd in [
            "pane.copy",
            "pane.move",
            "pane.delete",
            "pane.rename",
            "pane.copy-path",
        ] {
            assert!(
                verdict(cmd, &f).is_available(),
                "{cmd} atenuado por una sesión en claro"
            );
        }
    }
}
