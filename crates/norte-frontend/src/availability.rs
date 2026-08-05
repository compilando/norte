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
/// (`clippy::struct_excessive_bools`: allowed on purpose. These are six
/// INDEPENDENT observations about one moment, not the states of a machine —
/// any combination of them is a real context, so there is no enum to collapse
/// them into. Wrapping each in a two-variant enum would make every call site
/// read `Enterable::No, Viewable::Yes, RenameSingle::Yes` for no gain: the
/// field names already say which question each answer belongs to.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
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
/// frontend that caches `Capabilities` per scheme should prefer the flags and
/// fall back here (see `norte_tui::app::App::pane_read_only`); one that caches
/// nothing has only this. Either way it answers too MUCH read-only, never too
/// little: a backend that refuses writes for some other reason says so when
/// the task is submitted, which is a real error the user sees, whereas
/// claiming a read-only location is writable would offer an operation that
/// cannot exist.
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
        // Entrar: el llamador decide qué es entrable (un directorio en la
        // GUI; también un archivo comprimido en la TUI, que le compone el
        // scheme). La tabla no vuelve a mirar el recuento — el frontend que
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
        "pane.ai-rename" | "pane.delete" | "pane.delete-permanent" => {
            gated(!facts.source_read_only, Reason::ReadOnlyBackend)
        }
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
