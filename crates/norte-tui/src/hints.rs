//! Hints de pie de página de los overlays de diálogo (H1 T3, issue #24 —
//! CIERRA): mismo patrón que la ayuda F1 (`help.rs`), pero por comando del
//! contexto `dialog`. Un hint es el JOIN de los comandos SOPORTADOS por un
//! overlay concreto × el keymap `dialog` EFECTIVO × las etiquetas Fluent
//! `dialog-cmd-*` — jamás una cadena estática mantenida a mano: un rebind
//! ya no puede desincronizar el pie de página de lo que la tecla hace de
//! verdad.

use std::collections::HashSet;

use norte_i18n::t;

use crate::keymap::{Effective, dialog_hint_id};

/// Commands considered self-evident navigation (MAJOR-1, H1 close): arrows
/// and paging are universal — every terminal user already knows what they
/// do — so they cost footer width without paying for it in clarity. Excluded
/// ONLY from the generated HINT text via [`without_navigation`]; the
/// dispatch allowlists in `app.rs` (`ALLOW_PICKER`/`ALLOW_EXTENSIONS`/
/// `ALLOW_NAV_HOTLIST`) are untouched — the keys still work, they are just
/// not spelled out in the footer.
const NAVIGATION_HINT_EXCLUDED: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
];

/// Filters a SUPPORTED allowlist down to the commands worth spelling out in
/// a footer hint (MAJOR-1): drops [`NAVIGATION_HINT_EXCLUDED`] entries. Used
/// only by [`DialogHints::build`] for the NON-MODAL overlays (theme picker,
/// extensions, hotlist popup) — 80-column footers were being cut mid-word
/// (`snapshots_ui__snapshot_popup_hotlist.snap` showed `[d] borr┘`) because
/// universally-known arrow keys were eating the scarce width. Modal dialogs
/// (confirm/collision/approval/trust-host) never include navigation commands
/// in their allowlists to begin with, so this is a no-op for them.
#[must_use]
pub(crate) fn without_navigation<'a>(supported: &'a [&'a str]) -> Vec<&'a str> {
    supported
        .iter()
        .copied()
        .filter(|c| !NAVIGATION_HINT_EXCLUDED.contains(c))
        .collect()
}

/// The HELP overlay's printable verbs, in the PRIORITY order its footer
/// offers them.
///
/// The help overlay is full-screen and cannot grow, so it is the one footer
/// whose hint has to be CUT — `ui::fit_hint_groups` drops whole
/// `[chord] label` groups from the TAIL and marks the loss with a `…`. That
/// mechanism is what decides here: this list is offered WHOLE and the width
/// takes what it takes, so a 113-column terminal shows all five groups and an
/// 80-column one keeps the three at the head.
///
/// The order is therefore a ranking of what a reader cannot guess:
///
/// 1. `dialog.filter` — nothing else on screen suggests the page is
///    searchable;
/// 2. `dialog.back` — the only way out of a link, and the overlay's history is
///    invisible;
/// 3. `dialog.pane` — the only way INTO the body, where `Enter` runs commands
///    that touch the filesystem;
/// 4. `dialog.confirm`, 5. `dialog.cancel` — the UNIVERSAL overlay keys, which
///    the `help` topic also spells out in prose. Last because they are the
///    ones a reader already knows, not because they are unimportant.
///
/// It was a fixed EXCLUSION of the last two before, which honoured the
/// 80-column frame by hiding `[enter]`/`[esc]` on every frame, wide ones
/// included. `ALLOW_HELP` and the dispatch in `app.rs` are untouched either
/// way — this is the printed hint only.
///
/// **La paginación SÍ está**, al contrario que en los demás overlays. En una
/// lista de opciones las flechas se dan por sabidas y el pie es estrecho; aquí
/// el cuerpo es PROSA de doscientas líneas en una ventana de veinte, y no
/// había nada en pantalla que dijera cómo bajar por ella. `dialog.up`/`down`
/// siguen fuera: en esta pantalla mueven el cursor entre filas ejecutables, y
/// eso se descubre solo — bajar por el texto no.
///
/// `help_priority_covers_every_printable_verb` fija que esta lista siga siendo
/// una proyección completa de `ALLOW_HELP`: un verbo añadido allí hay que
/// rankearlo aquí, no dejarlo mudo para siempre.
const HELP_HINT_PRIORITY: &[&str] = &[
    "dialog.pane",
    "dialog.page-down",
    "dialog.page-up",
    "dialog.filter",
    "dialog.back",
    "dialog.confirm",
    "dialog.cancel",
];

/// La etiqueta de un verbo EN LA AYUDA, que no siempre es la del mismo verbo
/// en un diálogo.
///
/// `dialog.pane` es el caso que lo motiva: en un modal significa «el otro
/// panel», y aquí significa «índice ↔ contenido» — pintar «otro panel» sobre
/// un overlay que no tiene panes le dice al lector algo que no puede hacer, y
/// le esconde lo único que necesita para llegar al texto. La paginación
/// también se dice distinta: aquí no pagina una lista, desplaza la página.
fn help_hint_id(cmd: &str) -> String {
    match cmd {
        "dialog.pane" => "help-cmd-pane".to_owned(),
        "dialog.page-up" => "help-cmd-page-up".to_owned(),
        "dialog.page-down" => "help-cmd-page-down".to_owned(),
        otro => dialog_hint_id(otro),
    }
}

/// Footer hint for an overlay: the join of its SUPPORTED dialog commands ×
/// the effective dialog keymap × Fluent labels — same invariant as F1 help
/// (#24: a rebind can never desync the hint again).
///
/// El ORDEN sale del keymap EFECTIVO (`eff.bindings()`, en precedencia
/// real), no del array `supported`: un comando SIN binding en el efectivo
/// (rebindeado a nada, o simplemente jamás ligado en una capa exótica)
/// queda fuera — honesto, sin tecla fantasma. La PRIMERA chord de cada
/// comando en ese orden es la que se muestra (p. ej. en `vim`, `up`/`down`
/// ganan a los `k`/`j` añadidos después en el preset).
#[must_use]
pub fn dialog_hints(supported: &[&str], eff: &Effective) -> String {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for (chord, cmd) in eff.bindings() {
        if supported.contains(&cmd) && seen.insert(cmd) {
            // RENDER-side duty (encoding audit H1): `chord` viene de un
            // keymap potencialmente hostil (`./.norte/keymap.toml`, capa de
            // PROYECTO sin trust — `parse_chord` acepta CUALQUIER
            // codepoint suelto como `KeyCode::Char`). `Chord`'s `Display`
            // lo escribe crudo y en minúscula A PROPÓSITO (logs/debug
            // quieren el chord real); este hint SÍ se pinta en el pie de
            // modales de seguridad, así que `paint_chord` — el ÚNICO hogar
            // de presentación de un chord, compartido con la palette y la
            // ayuda F1 — enmascara PRIMERO y solo después escribe la tecla
            // como la escribe la documentación (`F5`, no `f5`).
            let chord = crate::keymap::paint_chord(&chord);
            out.push(format!("[{chord}] {}", t(&dialog_hint_id(cmd))));
        }
    }
    out.join(" ")
}

/// Same hint, but in the order the CALLER gives instead of the effective
/// keymap's.
///
/// [`dialog_hints`] follows the effective on purpose (a footer that lists keys
/// in the order they resolve), and every overlay that can GROW to fit its hint
/// wants exactly that. The help overlay cannot grow: its footer is cut by
/// `ui::fit_hint_groups`, which drops groups from the TAIL — so the order is
/// what decides which verbs survive a narrow frame, and that is a
/// presentation ranking (`HELP_HINT_PRIORITY`, private to this module), not a
/// keymap fact.
///
/// A command with no binding in the effective is skipped, same as
/// [`dialog_hints`] — no phantom key. `order` is expected to be duplicate-free
/// (a constant ranking); a repeat would simply print its group twice.
#[must_use]
pub fn dialog_hints_in_order(order: &[&str], eff: &Effective) -> String {
    hints_in_order_with(order, eff, dialog_hint_id)
}

/// Como [`dialog_hints_in_order`], con la etiqueta que decida `label`.
fn hints_in_order_with(order: &[&str], eff: &Effective, label: impl Fn(&str) -> String) -> String {
    order
        .iter()
        .filter_map(|cmd| {
            // `first_chord` is the same join `dialog_hints` performs (first
            // chord of the command in the effective's precedence order),
            // through the same `paint_chord` presentation home.
            norte_frontend::palette::first_chord(cmd, eff)
                .map(|chord| format!("[{chord}] {}", t(&label(cmd))))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Hints precomputados de TODOS los overlays de diálogo, uno por campo.
/// Se reconstruyen en el arranque y en cada hot-reload OK (`main.rs`),
/// igual que `help_lines` (`help::build`), a partir del MISMO efectivo
/// `dialog` que consume el `Resolver` compartido — ANTES de que ese
/// efectivo se mueva al `Resolver` (`Effective` es `Clone`, pero
/// `DialogHints::build` solo toma prestado: no hace falta clonar).
/// `ui::draw_*` los lee en vez de una clave Fluent estática. El único punto
/// de acoplamiento con la semántica de SEGURIDAD (qué comandos acepta cada
/// overlay) son los ALLOWLIST de `app.rs` — la MISMA lista que filtra el
/// despacho, jamás una copia.
#[derive(Debug, Clone, Default)]
pub struct DialogHints {
    /// `Modal::ConfirmDelete`/`Modal::ConfirmTransfer`/`Modal::ConfirmQuit`
    /// (S2, `[ui] confirm_quit`).
    pub confirm: String,
    /// `Modal::Collision`.
    pub collision: String,
    /// `Modal::ApproveAgentOp`.
    pub approval: String,
    /// `Modal::TrustHostKey`.
    pub trust_host: String,
    /// `Modal::AskSecret` (#325).
    pub ask_secret: String,
    /// Selector de tema (`App::theme_picker`).
    pub picker: String,
    /// Picker de columnas (`App::columns_picker`, #108 7a).
    pub columns: String,
    /// Gestor de extensiones (`App::extensions`).
    pub extensions: String,
    /// Panel de `[config]` de un plugin dentro del gestor de extensiones
    /// (`App::extensions`'s `config`, G3c).
    pub plugin_config: String,
    /// Popup de navegación en modo hotlist (`App::nav_popup`,
    /// `NavPopupKind::Hotlist`) — el historial no pinta footer, igual que
    /// antes de H1.
    pub nav_list: String,
    /// Popup de navegación en modo volúmenes (`App::nav_popup`,
    /// `NavPopupKind::Volumes`, design §D) — su propio hint porque
    /// `add`/`remove` de `nav_list` no significan nada aquí y el toggle
    /// "mostrar todo" sí.
    pub nav_volumes: String,
    /// Help overlay (`App::help`, H3b).
    pub help: String,
    /// `true` when a help page is covering a modal, so the modal's own keys
    /// are inert until it closes (H3c).
    ///
    /// The generated footers already say so — [`Self::with_modals_inert`]
    /// replaces them. This flag is for the modals whose key hint is not
    /// generated but baked into Fluent PROSE, so their text can say the same
    /// true thing instead of advertising `y`/`n` at a reader for whom both do
    /// nothing.
    ///
    /// Today that is the AI rename plan and the semantic hits — the two whose
    /// hint is PROSE, which is not the same set as "the modals a help can
    /// cover". That set is every modal
    /// `norte_tui::help_context::help_over_modal_allowed` admits, the agent
    /// approval and the host-key TOFU included; those simply have a generated
    /// footer, which the function above replaces. A new prose-hinted modal needs
    /// an arm here too.
    pub modals_inert: bool,
}

impl DialogHints {
    /// Reconstruye todos los hints del efectivo `dialog` vigente.
    #[must_use]
    pub fn build(eff: &Effective) -> Self {
        use crate::app::{
            ALLOW_APPROVAL, ALLOW_ASK_SECRET, ALLOW_COLLISION, ALLOW_COLUMNS, ALLOW_CONFIRM,
            ALLOW_EXTENSIONS, ALLOW_NAV_HOTLIST, ALLOW_NAV_VOLUMES, ALLOW_PICKER,
            ALLOW_PLUGIN_CONFIG, ALLOW_TRUST_HOST,
        };
        Self {
            confirm: dialog_hints(ALLOW_CONFIRM, eff),
            collision: dialog_hints(ALLOW_COLLISION, eff),
            approval: dialog_hints(ALLOW_APPROVAL, eff),
            trust_host: dialog_hints(ALLOW_TRUST_HOST, eff),
            ask_secret: dialog_hints(ALLOW_ASK_SECRET, eff),
            // Non-modal overlays (MAJOR-1): arrows are self-evident, so they
            // are dropped from the PRINTED hint (never from dispatch — see
            // `without_navigation`).
            picker: dialog_hints(&without_navigation(ALLOW_PICKER), eff),
            // #108 7a: además de la navegación, el pie del picker de
            // columnas omite los verbos de REORDENACIÓN — shift+↑/↓ son las
            // flechas con shift, autoevidentes junto a up/down, y con ellos
            // el hint (101 celdas en es) no cabe en un frame de 80 (mismo
            // MAJOR-1 que motivó `without_navigation`). Solo el hint
            // IMPRESO: `ALLOW_COLUMNS` y el dispatch no cambian.
            columns: dialog_hints(
                &without_navigation(ALLOW_COLUMNS)
                    .into_iter()
                    .filter(|c| !matches!(*c, "dialog.move-up" | "dialog.move-down"))
                    .collect::<Vec<_>>(),
                eff,
            ),
            extensions: dialog_hints(&without_navigation(ALLOW_EXTENSIONS), eff),
            plugin_config: dialog_hints(&without_navigation(ALLOW_PLUGIN_CONFIG), eff),
            nav_list: dialog_hints(&without_navigation(ALLOW_NAV_HOTLIST), eff),
            nav_volumes: dialog_hints(&without_navigation(ALLOW_NAV_VOLUMES), eff),
            // H3b: offered WHOLE, in priority order — the width decides how
            // much of it is printed (`ui::fit_hint_groups`), not a fixed
            // exclusion. See [`HELP_HINT_PRIORITY`].
            help: hints_in_order_with(HELP_HINT_PRIORITY, eff, help_hint_id),
            // Los hints RECIÉN construidos describen teclas que sí responden;
            // solo `with_modals_inert` levanta el flag, y solo mientras una
            // ayuda tape el modal.
            modals_inert: false,
        }
    }

    /// The same hints, with every MODAL footer replaced by "close the help to
    /// answer" (H3c).
    ///
    /// For the state where a help page is open OVER a modal: the help owns the
    /// keys then (`HelpView::over_modal`), so the modal's verbs are INERT — an
    /// approval prompt advertising `[y] approve [n] deny` while both keys do
    /// nothing is the same defect this module exists to prevent, only reached
    /// through key OWNERSHIP instead of through a rebind. A footer here can
    /// never desync from what the key does; that has to include the case where
    /// the key does nothing.
    ///
    /// The four modal fields and no others. The rest belong to overlays that
    /// are not modals, and a modal being on screen at all already took their
    /// keys away long before this (`modal_wins`) — a state H1 decided
    /// deliberately, not one this function is about.
    ///
    /// Only the footer changes: the box, the title and the question keep being
    /// painted, on top of the page ([`crate::ui::draw`] paints the modal last).
    /// Hiding the question is the defect that ordering fixed, and replacing a
    /// footer must not undo it.
    #[must_use]
    pub fn with_modals_inert(&self) -> Self {
        let notice = t("modal-hint-help-open");
        Self {
            confirm: notice.clone(),
            collision: notice.clone(),
            approval: notice.clone(),
            trust_host: notice,
            modals_inert: true,
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Screen, parse_keymap};

    /// El efectivo `dialog` del preset de fábrica, que es el que pintan los
    /// pies reales. Vocabulario = `COMMANDS` ∪ `DIALOG_COMMANDS`: el efectivo
    /// `dialog` fusiona TAMBIÉN la sección `[global]` del preset, así que
    /// `DIALOG_COMMANDS` a secas no basta (`build_for` fallaría con
    /// `UnknownCommand { run: "app.quit" }`).
    fn orthodox_dialog() -> Effective {
        let (_, preset) = crate::keymap::presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let known: Vec<&str> = crate::keymap::COMMANDS
            .iter()
            .copied()
            .chain(crate::keymap::DIALOG_COMMANDS.iter().copied())
            .collect();
        Effective::build_for(&preset, &[], &known, Screen::Dialog).expect("efectivo dialog")
    }

    /// Encoding audit H1: un `./.norte/keymap.toml` de PROYECTO (sin trust)
    /// puede ligar un chord hostil (RLO/ZWSP/LRM/BEL, corpus
    /// `norte_testkit::corpus::hostile_chords`) a un comando `dialog.*`
    /// soportado vía `prepend_keymap` — capa de usuario, gana al preset. El
    /// hint generado (`dialog_hints`) es lo que se pinta en el pie de
    /// modales de SEGURIDAD (`ApproveAgentOp`/`TrustHostKey`/
    /// `ConfirmDelete`-permanente): ningún hazard puede sobrevivir crudo.
    #[test]
    fn dialog_hints_enmascara_chords_hostiles_de_una_capa() {
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [{ on = ["y"], run = "dialog.approve" }]
        "#,
        )
        .unwrap();
        let known = ["dialog.approve", "dialog.deny"];
        for hazard in norte_testkit::corpus::hostile_chords() {
            // Escape `\uXXXX` de TOML (spec v1.0.0): un control C0 crudo
            // como BEL (U+0007) es sintaxis inválida dentro de una basic
            // string TOML, así que el token va SIEMPRE escapado, no crudo.
            let token_esc = format!("\\u{:04X}", hazard.token as u32);
            let layer_src = format!(
                r#"
                [dialog]
                prepend_keymap = [{{ on = ["{token_esc}"], run = "dialog.approve" }}]
                "#,
            );
            let layer = parse_keymap(&layer_src).unwrap();
            let eff = Effective::build_for(&preset, &[layer], &known, Screen::Dialog)
                .unwrap_or_else(|e| panic!("[{}] keymap efectivo: {e}", hazard.id));
            let hint = dialog_hints(&["dialog.approve", "dialog.deny"], &eff);
            assert!(
                !hint.chars().any(norte_encoding::is_terminal_hazard),
                "[{}] hazard crudo en el hint: {hint:?}",
                hazard.id
            );
            assert!(
                hint.contains('\u{FFFD}'),
                "[{}] el hazard debe enmascararse a U+FFFD: {hint:?}",
                hazard.id
            );
        }
    }

    #[test]
    fn dialog_hints_omite_comandos_sin_binding() {
        // Este test afirma los strings del corpus INGLÉS. Sin fijar el idioma
        // resolvía por entorno (`LANG`), así que era verde en CI y rojo en
        // cualquier máquina con `LANG=es_*` — la misma línea que el resto de
        // los tests de render de este crate ya llevaba.
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [{ on = ["y"], run = "dialog.approve" }]
        "#,
        )
        .unwrap();
        let known = ["dialog.approve", "dialog.deny"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        let hint = dialog_hints(&["dialog.approve", "dialog.deny"], &eff);
        assert_eq!(hint, "[y] approve");
    }

    #[test]
    fn dialog_hints_respeta_el_orden_del_efectivo_no_del_allowlist() {
        // Este test afirma los strings del corpus INGLÉS. Sin fijar el idioma
        // resolvía por entorno (`LANG`), así que era verde en CI y rojo en
        // cualquier máquina con `LANG=es_*` — la misma línea que el resto de
        // los tests de render de este crate ya llevaba.
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [
                { on = ["esc"], run = "dialog.cancel" },
                { on = ["enter"], run = "dialog.confirm" },
            ]
        "#,
        )
        .unwrap();
        let known = ["dialog.confirm", "dialog.cancel"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        // El allowlist pide confirm-antes-que-cancel; el efectivo declara
        // cancel primero — el hint sigue al efectivo.
        // Chords PINTADOS (`paint_chord`): `Esc`/`Enter`, no `esc`/`enter` —
        // `Chord`'s `Display` es crudo y en minúscula solo para logs.
        let hint = dialog_hints(&["dialog.confirm", "dialog.cancel"], &eff);
        assert_eq!(hint, "[Esc] cancel [Enter] confirm");
    }

    #[test]
    fn dialog_hints_usa_la_primera_chord_ante_un_duplicado() {
        // Este test afirma los strings del corpus INGLÉS. Sin fijar el idioma
        // resolvía por entorno (`LANG`), así que era verde en CI y rojo en
        // cualquier máquina con `LANG=es_*` — la misma línea que el resto de
        // los tests de render de este crate ya llevaba.
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [
                { on = ["up"], run = "dialog.up" },
                { on = ["k"], run = "dialog.up" },
            ]
        "#,
        )
        .unwrap();
        let known = ["dialog.up"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        let hint = dialog_hints(&["dialog.up"], &eff);
        assert_eq!(hint, "[Up] up");
    }

    #[test]
    fn dialog_hints_string_vacia_sin_soportados_ligados() {
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [{ on = ["y"], run = "dialog.approve" }]
        "#,
        )
        .unwrap();
        let known = ["dialog.approve"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        let hint = dialog_hints(&["dialog.rename"], &eff);
        assert_eq!(hint, "");
    }

    /// MAJOR-1 (H1 close): las teclas de navegación NO salen en los tres
    /// hints de overlay NO-modal (picker/extensions/hotlist) — se pintarían
    /// self-evidentes y truncaban el pie a 80 col
    /// (`snapshots_ui__snapshot_popup_hotlist.snap` antes de este fix). Los
    /// modales sí llevan sus comandos completos (ninguno soporta
    /// navegación) — nada que filtrar, así que su comportamiento no cambia.
    #[test]
    fn overlays_no_modales_omiten_navegacion_del_hint() {
        let hints = DialogHints::build(&orthodox_dialog());
        for hint in [
            &hints.picker,
            &hints.extensions,
            &hints.plugin_config,
            &hints.nav_list,
        ] {
            assert!(
                !hint.contains("[Up]") && !hint.contains("[Down]"),
                "las flechas no deberían salir en un hint no-modal: {hint:?}"
            );
        }
        // El picker SÍ conserva confirm/cancel (no son navegación).
        assert!(hints.picker.contains("[Enter]"));
        assert!(hints.picker.contains("[Esc]"));
    }

    /// H3b: the help overlay's footer is GENERATED like every other
    /// overlay's — the three verbs it adds must reach it with their chords.
    ///
    /// Con la etiqueta de la AYUDA, que no es la del mismo verbo en un modal:
    /// `dialog.pane` aquí es «índice ↔ texto» y no «otro panel», que sobre un
    /// overlay sin panes nombra algo que el lector no puede hacer.
    #[test]
    fn el_hint_de_la_ayuda_lista_sus_verbos_propios() {
        let hints = DialogHints::build(&orthodox_dialog());
        for cmd in ["dialog.filter", "dialog.back", "dialog.pane"] {
            assert!(
                hints.help.contains(&t(&help_hint_id(cmd))),
                "{cmd} debe aparecer en el pie de la ayuda: {}",
                hints.help
            );
        }
        assert!(
            !hints.help.contains(&t("dialog-cmd-pane")),
            "y jamás con la etiqueta del modal: {}",
            hints.help
        );
    }

    /// H3b, adaptive footer: the help hint is OFFERED whole — all five
    /// printable verbs, `Enter` and `Esc` included — and in the priority order
    /// a narrow frame will cut from the tail. Nothing is excluded up front any
    /// more: a 113-column terminal has room for the lot and used to paint half
    /// an empty footer while hiding them.
    #[test]
    fn el_pie_de_la_ayuda_ofrece_todos_sus_verbos_en_orden_de_prioridad() {
        let hints = DialogHints::build(&orthodox_dialog());
        let position = |cmd: &str| {
            hints
                .help
                .find(&t(&help_hint_id(cmd)))
                .unwrap_or_else(|| panic!("{cmd} debe estar en el pie de la ayuda: {}", hints.help))
        };
        let order: Vec<usize> = HELP_HINT_PRIORITY.iter().map(|c| position(c)).collect();
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "los verbos salen en el orden de prioridad, que es el que decide \
             qué sobrevive a un frame estrecho: {}",
            hints.help
        );
        // Las FLECHAS siguen fuera —mover el cursor entre filas ejecutables se
        // descubre solo— pero la paginación SÍ está: el cuerpo es prosa larga
        // en una ventana corta, y nada más en pantalla dice cómo bajar por
        // ella. Es la diferencia entre esta pantalla y una lista de opciones.
        for cmd in ["dialog.up", "dialog.down"] {
            assert!(
                !hints
                    .help
                    .contains(&format!("] {}", t(&dialog_hint_id(cmd)))),
                "{cmd} es autoevidente y no gasta ancho: {}",
                hints.help
            );
        }
        for cmd in ["dialog.page-up", "dialog.page-down"] {
            assert!(
                hints.help.contains(&t(&help_hint_id(cmd))),
                "{cmd} es lo que nadie adivina en una página de prosa: {}",
                hints.help
            );
        }
    }

    /// [`HELP_HINT_PRIORITY`] es una proyección COMPLETA de `ALLOW_HELP`: un
    /// verbo nuevo en el allowlist tiene que rankearse aquí, no quedarse
    /// invisible en el pie para siempre (que es lo que hacía la exclusión
    /// fija). Y al revés: nada se anuncia que el despacho no acepte.
    #[test]
    fn help_priority_covers_every_printable_verb() {
        use crate::app::{ALLOW_HELP, help_action};
        // La paginación SÍ se imprime en esta pantalla (ver
        // `HELP_HINT_PRIORITY`): lo único que no gasta ancho aquí son las
        // flechas, que mueven el cursor entre filas ejecutables y se
        // descubren solas.
        let printable: Vec<&str> = ALLOW_HELP
            .iter()
            .copied()
            .filter(|c| !matches!(*c, "dialog.up" | "dialog.down"))
            .collect();
        for cmd in &printable {
            assert!(
                HELP_HINT_PRIORITY.contains(cmd),
                "{cmd} es imprimible pero no está rankeado en HELP_HINT_PRIORITY"
            );
        }
        for cmd in HELP_HINT_PRIORITY {
            assert!(
                printable.contains(cmd),
                "{cmd} se anunciaría sin que el despacho lo acepte"
            );
            assert!(
                help_action(cmd).is_some(),
                "…y la tecla tiene que estar viva: {cmd}"
            );
        }
        assert_eq!(HELP_HINT_PRIORITY.len(), printable.len());
    }

    /// `dialog_hints_in_order` sigue el ORDEN del caller (al revés que
    /// [`dialog_hints`], que sigue el efectivo) y omite lo no ligado.
    #[test]
    fn dialog_hints_in_order_sigue_al_caller_no_al_efectivo() {
        // Este test afirma los strings del corpus INGLÉS. Sin fijar el idioma
        // resolvía por entorno (`LANG`), así que era verde en CI y rojo en
        // cualquier máquina con `LANG=es_*` — la misma línea que el resto de
        // los tests de render de este crate ya llevaba.
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let preset = parse_keymap(
            r#"
            [dialog]
            keymap = [
                { on = ["esc"], run = "dialog.cancel" },
                { on = ["enter"], run = "dialog.confirm" },
            ]
        "#,
        )
        .unwrap();
        let known = ["dialog.confirm", "dialog.cancel", "dialog.pane"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).unwrap();
        assert_eq!(
            dialog_hints_in_order(&["dialog.confirm", "dialog.pane", "dialog.cancel"], &eff),
            "[Enter] confirm [Esc] cancel",
            "el orden es el pedido, y `dialog.pane` (sin binding) no inventa tecla"
        );
    }

    /// H3c: con una página de ayuda ENCIMA, los CUATRO pies de modal dicen que
    /// hay que cerrarla y no ofrecen ni un verbo — ni `confirmar`/`cancelar`,
    /// que es lo que el test de render no puede aislar (el pie de la propia
    /// ayuda los lista, y ahí sí responden).
    ///
    /// Los pies que NO son de modal se quedan intactos: un modal en pantalla ya
    /// les había quitado la tecla mucho antes (`modal_wins`, H1), y eso es una
    /// decisión de entonces, no lo que esta función arregla.
    #[test]
    fn los_pies_de_modal_dejan_de_ofrecer_verbos_bajo_la_ayuda() {
        let alive = DialogHints::build(&orthodox_dialog());
        let inert = alive.with_modals_inert();
        let notice = t("modal-hint-help-open");
        for pie in [
            &inert.confirm,
            &inert.collision,
            &inert.approval,
            &inert.trust_host,
        ] {
            assert_eq!(pie, &notice);
        }
        // Ningún verbo del vocabulario `dialog.*` sobrevive en ellos.
        for cmd in crate::keymap::DIALOG_COMMANDS {
            let label = t(&dialog_hint_id(cmd));
            for pie in [
                &inert.confirm,
                &inert.collision,
                &inert.approval,
                &inert.trust_host,
            ] {
                assert!(
                    !pie.contains(&label),
                    "{cmd} sigue anunciado en un pie inerte: {pie:?}"
                );
            }
        }
        // Y lo que no es un modal no se toca.
        assert_eq!(inert.picker, alive.picker);
        assert_eq!(inert.columns, alive.columns);
        assert_eq!(inert.extensions, alive.extensions);
        assert_eq!(inert.plugin_config, alive.plugin_config);
        assert_eq!(inert.nav_list, alive.nav_list);
        assert_eq!(inert.help, alive.help, "la ayuda SÍ tiene las teclas");
    }

    /// [`without_navigation`] filtra SOLO las cuatro entradas de navegación,
    /// preservando el resto intacto y su orden relativo.
    #[test]
    fn without_navigation_filtra_solo_navegacion() {
        let supported = [
            "dialog.up",
            "dialog.approve",
            "dialog.down",
            "dialog.cancel",
        ];
        assert_eq!(
            without_navigation(&supported),
            vec!["dialog.approve", "dialog.cancel"]
        );
    }
}
