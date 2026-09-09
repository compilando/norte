//! Keymap de la TUI: re-exporta el motor compartido de
//! [`norte_frontend::keymap`] y aporta el adaptador de crossterm, la lista
//! de comandos de la TUI y sus presets de fábrica. El motor en sí (tipos
//! neutros, parseo, fusión de capas, resolución) vive en `norte-frontend`
//! (GUI-c T1/T2) — este módulo es una capa fina TUI-específica.
/// `ModKey`/`set_mod_key` están DELIBERADAMENTE ausentes de esta lista (ADR
/// 0043 decisión 9): la TUI no puede observar ⌘ —crossterm no entrega super
/// sin `PushKeyboardEnhancementFlags`, que norte no activa— así que no puede
/// honrar ninguna política que no sea Ctrl, y por tanto no debe poder
/// nombrarla. Una API que acepta un ajuste que va a ignorar es peor que una
/// que no lo ofrece.
pub use norte_frontend::keymap::{
    Availability, Chord, Count, Effective, KeyCode, KeymapError, KeymapFile, Mods, Rebind,
    RebindError, RebindSources, RebindWrite, Resolution, Resolver, Screen, UnbindOutcome,
    UnbindWrite, count_ignored_message, paint_chord, parse_chord, parse_keymap, rebind_check,
    rebind_dry_run, unavailable_message, unbind_dry_run,
};

use crossterm::event::{KeyCode as CtCode, KeyModifiers as CtMods};

/// Adaptador: evento de crossterm → [`Chord`] neutro. En `Char` el carácter
/// ya codifica shift (lo descarta `Chord::new`); el resto conserva mods.
/// Devuelve `None` para teclas que el keymap no modela (p. ej. `Media`,
/// `BackTab`, `CapsLock`): el caller debe tratarlo como si la tecla no
/// ligara nada (equivalente a [`Resolution::Reset`] — nunca pánico, nunca
/// una tecla "perdida" en silencio distinto de antes).
#[must_use]
pub fn chord_from_crossterm(mods: CtMods, code: CtCode) -> Option<Chord> {
    let neutral = match code {
        CtCode::Char(c) => KeyCode::Char(c),
        // Clamp a f1..=f12, como el adaptador de la GUI (#109): xterm
        // clásico reporta Shift+F1 como F13, y un F(n>12) no casa ningún
        // binding (`parse_chord` lo rechaza) ni re-parsea su `Display`
        // ("f13") — mejor tecla-no-modelada que un chord irrepresentable.
        CtCode::F(n @ 1..=12) => KeyCode::F(n),
        CtCode::Enter => KeyCode::Enter,
        CtCode::Tab => KeyCode::Tab,
        CtCode::Esc => KeyCode::Esc,
        CtCode::Backspace => KeyCode::Backspace,
        CtCode::Up => KeyCode::Up,
        CtCode::Down => KeyCode::Down,
        CtCode::Left => KeyCode::Left,
        CtCode::Right => KeyCode::Right,
        CtCode::Home => KeyCode::Home,
        CtCode::End => KeyCode::End,
        CtCode::PageUp => KeyCode::PageUp,
        CtCode::PageDown => KeyCode::PageDown,
        CtCode::Insert => KeyCode::Insert,
        CtCode::Delete => KeyCode::Delete,
        _ => return None, // teclas que el keymap no modela
    };
    // Solo ctrl/alt/shift: crossterm no reporta super/meta sin
    // PushKeyboardEnhancementFlags (no activado).
    let m = Mods {
        ctrl: mods.contains(CtMods::CONTROL),
        alt: mods.contains(CtMods::ALT),
        shift: mods.contains(CtMods::SHIFT),
        // JAMÁS puede ser otra cosa en la TUI. `CtMods::SUPER` existe en el
        // tipo, pero el terminal solo lo entrega bajo
        // `PushKeyboardEnhancementFlags` (protocolo de teclado de Kitty), que
        // norte no activa: leerlo aquí devolvería `false` siempre y fingiría
        // una capacidad que no hay. Por eso `mod+` es Ctrl en la TUI en TODAS
        // las plataformas, macOS incluido, y lo decimos en vez de prometer
        // una tecla que el terminal nunca va a entregar.
        cmd: false,
    };
    Some(Chord::new(m, neutral))
}

/// Los comandos que el TUI sabe ejecutar — la fuente ÚNICA contra la que
/// se valida todo keymap (los mismos nombres que verán la palette y el
/// wire, ADR 0006).
/// Una sola fuente para el vocabulario de comandos (#112): el macro emite
/// `COMMANDS` (la lista de validación de siempre, misma superficie pública)
/// Y el enum [`Command`] con una variante por nombre. `dispatch` (main.rs)
/// matchea el enum SIN comodín: un comando nuevo sin brazo, o un brazo sin
/// variante, es un ERROR DE COMPILACIÓN — la clase de bug que motivó esto
/// (`mark.pattern-*` en COMMANDS sin brazo: pánico en debug, no-op mudo en
/// release) deja de existir en runtime.
macro_rules! commands {
    ($($name:literal => $variant:ident,)+) => {
        /// Los comandos que el TUI sabe ejecutar — la fuente ÚNICA contra la
        /// que se valida todo keymap (los mismos nombres que verán la palette
        /// y el wire, ADR 0006).
        pub const COMMANDS: &[&str] = &[$($name),+];

        /// El vocabulario de `dispatch`, tipado (#112). Se parsea UNA vez en
        /// la frontera (resolver/palette -> [`Command::parse`]); a partir de
        /// ahí el compilador exige un brazo por variante.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Command {
            $(
                #[doc = concat!("`", $name, "`")]
                $variant,
            )+
        }

        impl Command {
            /// Nombre -> variante. `None` = fuera del vocabulario (el keymap
            /// lo valida al cargar; `lua:`/`plugin:` se enrutan ANTES).
            #[must_use]
            pub fn parse(s: &str) -> Option<Self> {
                match s {
                    $($name => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

commands! {
    "app.quit" => AppQuit,
    "pane.switch" => PaneSwitch,
    "pane.tab-new" => TabNew,
    "pane.tab-close" => TabClose,
    "pane.tab-next" => TabNext,
    "pane.tab-prev" => TabPrev,
    "pane.tab-move-left" => TabMoveLeft,
    "pane.tab-move-right" => TabMoveRight,
    "pane.tab-goto-1" => TabGoto1,
    "pane.tab-goto-2" => TabGoto2,
    "pane.tab-goto-3" => TabGoto3,
    "pane.tab-goto-4" => TabGoto4,
    "pane.tab-goto-5" => TabGoto5,
    "pane.tab-goto-6" => TabGoto6,
    "pane.tab-goto-7" => TabGoto7,
    "pane.tab-goto-8" => TabGoto8,
    "pane.tab-goto-9" => TabGoto9,
    "layout.split-h" => LayoutSplitH,
    "layout.split-v" => LayoutSplitV,
    "layout.focus-next" => LayoutFocusNext,
    "layout.focus-prev" => LayoutFocusPrev,
    "layout.close-slot" => LayoutCloseSlot,
    "layout.grow" => LayoutGrow,
    "layout.shrink" => LayoutShrink,
    "layout.equalize" => LayoutEqualize,
    "layout.set-target" => LayoutSetTarget,
    "layout.places" => LayoutPlaces,
    "layout.preview" => LayoutPreview,
    "layout.processes" => LayoutProcesses,
    "layout.metadata" => LayoutMetadata,
    "layout.log" => LayoutLog,
    "layout.pick" => LayoutPick,
    "profile.pick" => ProfilePick,
    "profile.save-as" => ProfileSaveAs,
    "profile.next" => ProfileNext,
    "profile.prev" => ProfilePrev,
    "pane.mirror" => PaneMirror,
    "pane.mirror-target" => PaneMirrorTarget,
    "pane.pull" => PanePull,
    "pane.swap" => PaneSwap,
    "cursor.up" => CursorUp,
    "cursor.down" => CursorDown,
    "cursor.page-up" => CursorPageUp,
    "cursor.page-down" => CursorPageDown,
    "cursor.top" => CursorTop,
    "cursor.bottom" => CursorBottom,
    "nav.enter" => NavEnter,
    "nav.parent" => NavParent,
    "nav.back" => NavBack,
    "nav.forward" => NavForward,
    "app.help" => AppHelp,
    "app.theme" => AppTheme,
    "app.extensions" => AppExtensions,
    "app.palette" => AppPalette,
    "app.menu" => AppMenu,
    "app.settings" => AppSettings,
    "app.pick-accept" => AppPickAccept,
    "app.terminal" => AppTerminal,
    "app.toggle-panels" => AppTogglePanels,
    "pane.command-line" => PaneCommandLine,
    "pane.ai-rename" => PaneAiRename,
    "pane.semantic-search" => PaneSemanticSearch,
    "pane.copy" => PaneCopy,
    "pane.move" => PaneMove,
    "pane.delete" => PaneDelete,
    "pane.delete-permanent" => PaneDeletePermanent,
    "pane.view" => PaneView,
    "pane.open" => PaneOpen,
    "task.cancel" => TaskCancel,
    "viewer.close" => ViewerClose,
    "viewer.up" => ViewerUp,
    "viewer.down" => ViewerDown,
    "viewer.page-up" => ViewerPageUp,
    "viewer.page-down" => ViewerPageDown,
    "viewer.top" => ViewerTop,
    "viewer.bottom" => ViewerBottom,
    "viewer.left" => ViewerLeft,
    "viewer.right" => ViewerRight,
    "viewer.encoding" => ViewerEncoding,
    "viewer.encoding-auto" => ViewerEncodingAuto,
    "viewer.hex" => ViewerHex,
    "pane.quick-search" => PaneQuickSearch,
    "pane.history" => PaneHistory,
    "pane.hotlist" => PaneHotlist,
    "pane.select-drive" => PaneSelectDrive,
    "pane.select-drive-left" => PaneSelectDriveLeft,
    "pane.select-drive-right" => PaneSelectDriveRight,
    "pane.compare-dirs" => PaneCompareDirs,
    "pane.compare-files" => PaneCompareFiles,
    "pane.sync-dirs" => PaneSyncDirs,
    "pane.search" => PaneSearch,
    "pane.names-encoding" => PaneNamesEncoding,
    "pane.toggle-hidden" => PaneToggleHidden,
    "pane.columns" => PaneColumns,
    "pane.sort-name" => PaneSortName,
    "pane.sort-ext" => PaneSortExt,
    "pane.sort-size" => PaneSortSize,
    "pane.sort-time" => PaneSortTime,
    "pane.sort-menu" => PaneSortMenu,
    "pane.properties" => PaneProperties,
    "pane.chmod" => PaneChmod,
    "pane.edit" => PaneEdit,
    "pane.tree" => PaneTree,
    "pane.connect" => PaneConnect,
    "pane.disconnect" => PaneDisconnect,
    "pane.edit-new" => PaneEditNew,
    "pane.dir-size" => PaneDirSize,
    "pane.checksum" => PaneChecksum,
    "pane.checksum-verify" => PaneChecksumVerify,
    "pane.pack" => PanePack,
    "pane.unpack" => PaneUnpack,
    "pane.test-archive" => PaneTestArchive,
    "pane.split-file" => PaneSplitFile,
    "pane.combine-files" => PaneCombineFiles,
    "pane.mkdir" => PaneMkdir,
    "pane.rename" => PaneRename,
    "pane.rename-batch" => PaneRenameBatch,
    "pane.refresh" => PaneRefresh,
    "pane.copy-path" => PaneCopyPath,
    "mark.toggle" => MarkToggle,
    "mark.all" => MarkAll,
    "mark.invert" => MarkInvert,
    "mark.clear" => MarkClear,
    "mark.pattern-add" => MarkPatternAdd,
    "mark.pattern-remove" => MarkPatternRemove,
    "mark.extension-add" => MarkExtensionAdd,
    "mark.extension-remove" => MarkExtensionRemove,
    "mark.files" => MarkFiles,
    "mark.dirs" => MarkDirs,
    "mark.restore" => MarkRestore,
    "mark.toggle-up" => MarkToggleUp,
    "mark.toggle-page-down" => MarkTogglePageDown,
    "mark.toggle-page-up" => MarkTogglePageUp,
    "mark.to-top" => MarkToTop,
    "mark.to-bottom" => MarkToBottom,
}

/// Los comandos del contexto `dialog` (H1, issue #24) — la lista CERRADA
/// que el TUI pasa a [`Effective::build_for`] para `Screen::Dialog`. Cada
/// overlay (modal, theme picker, extensions, nav popup) declara en código
/// su propio ALLOWLIST de cuáles soporta (`app::dialog_action` y las
/// resoluciones ad hoc en `main.rs`); la semántica de seguridad vive ahí,
/// jamás aquí. Coincide 1:1 con las secciones `[dialog]` de los tres
/// presets compartidos (`orthodox`/`vim`/`cua`) — un comando nuevo en el
/// preset sin su entrada aquí falla a construir con `UnknownCommand`.
pub const DIALOG_COMMANDS: &[&str] = &[
    "dialog.confirm",
    "dialog.cancel",
    "dialog.approve",
    "dialog.deny",
    "dialog.overwrite",
    "dialog.skip",
    "dialog.rename",
    "dialog.newer",
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "dialog.add",
    "dialog.toggle-enabled",
    "dialog.remove",
    "dialog.move-up",
    "dialog.move-down",
    "dialog.sort",
    "dialog.cycle-format",
    // H3b — verbs the help overlay adds. They are `dialog.*` and not
    // `app.*` because they only mean anything inside an overlay: "the other
    // pane of this overlay", "the page I came from", "start filtering this
    // list". Documented in the `help` topic, so the documentation gate is
    // paid in the same change that introduces them.
    "dialog.pane",
    "dialog.back",
    "dialog.filter",
];

/// Id de Fluent con la descripción de un comando (`app.quit` →
/// `help-cmd-app-quit`). La suite OBLIGA a que exista en ambos locales
/// para TODO comando de [`COMMANDS`]: un comando nuevo sin descripción
/// rompe tests — la ayuda no puede quedarse atrás.
#[must_use]
pub fn help_id(command: &str) -> String {
    format!("help-cmd-{}", command.replace('.', "-"))
}

/// Id de Fluent con la etiqueta CORTA de un comando `dialog.*` (`dialog.
/// page-up` → `dialog-cmd-page-up`), usada por los hints generados de pie
/// de página (H1 T3, #24). Mismo mangling que [`help_id`] (puntos→guiones)
/// aplicado al SUFIJO tras `dialog.` — el prefijo no se repite en el id
/// (evita `dialog-cmd-dialog-page-up`). La suite OBLIGA a que exista en
/// ambos locales para TODO comando de [`DIALOG_COMMANDS`].
#[must_use]
pub fn dialog_hint_id(command: &str) -> String {
    let suffix = command.strip_prefix("dialog.").unwrap_or(command);
    format!("dialog-cmd-{}", suffix.replace('.', "-"))
}

/// Los presets de fábrica, parseados (se validan en tests y al construir
/// el efectivo). Default del producto: `orthodox` (decisión 2026-07-10).
///
/// # Panics
/// Nunca con los TOML embebidos (los valida la suite).
#[must_use]
pub fn presets() -> Vec<(&'static str, KeymapFile)> {
    use norte_frontend::keymap::presets as shared;

    shared::NAMES
        .iter()
        .map(|&name| {
            let src = shared::source(name).expect("NAMES resuelve en source()");
            (
                name,
                parse_keymap(src)
                    .unwrap_or_else(|e| panic!("preset {name} embebido inválido: {e}")),
            )
        })
        .collect()
}

/// El segmento «pendiente» de la barra de estado (`[… ]` en `draw_status`):
/// el contador tecleado hasta ahora (K2a) seguido de los chords ya pulsados.
/// Los dos van JUNTOS porque en `12gg` conviven — el `12` sigue vivo mientras
/// la secuencia `g g` se teclea, y pintar solo uno de los dos miente sobre lo
/// que va a pasar al soltar la próxima tecla. Un contador que no se ve es un
/// contador que no se puede cancelar.
///
/// Vacío cuando no hay ni contador ni secuencia: la barra calla.
///
/// K3a: la composición vive en `norte_frontend::whichkey::pending_title`, que
/// es también el TÍTULO del panel which-key. Las dos superficies pintan el
/// MISMO estado a una línea de distancia, así que dos strings distintos serían
/// dos ortografías de una sola cosa (`f5 g` abajo, `F5 g` arriba) — y solo una
/// de ellas iba por `paint_chord`, que es quien ENMASCARA: un `keymap.toml` de
/// proyecto no lleva confianza y puede ligar un RLO o un BEL, y este string se
/// pinta en un terminal.
#[must_use]
pub fn pending_display(resolver: &Resolver) -> String {
    norte_frontend::whichkey::pending_title(resolver.pending(), resolver.count())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// K2a: la barra pinta el contador MIENTRAS se teclea, y lo sigue
    /// pintando con una secuencia a medias encima (`12` + `g`). Sin contador
    /// vivo el segmento es el de siempre; sin nada, calla.
    #[test]
    fn el_segmento_pendiente_pinta_contador_y_secuencia() {
        let preset = parse_keymap(
            r"
counts = true

[pane]
keymap = [ { on = ['g', 'g'], run = 'cursor.top' } ]
",
        )
        .expect("preset de test");
        let eff = Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse)
            .expect("efectivo de test");
        let mut r = Resolver::new(eff);
        assert_eq!(pending_display(&r), "", "sin nada, calla");
        assert!(matches!(
            r.push(parse_chord("1").expect("chord")),
            Resolution::Counting(1)
        ));
        assert!(matches!(
            r.push(parse_chord("2").expect("chord")),
            Resolution::Counting(12)
        ));
        assert_eq!(pending_display(&r), "12", "el contador se ve al teclearlo");
        assert!(matches!(
            r.push(parse_chord("g").expect("chord")),
            Resolution::Pending(1)
        ));
        assert_eq!(
            pending_display(&r),
            "12 g",
            "el contador SOBREVIVE a la secuencia a medias"
        );
    }

    /// Sin contadores en el preset el segmento es exactamente el de antes de
    /// K2a: los chords unidos por espacio, sin prefijo numérico inventado.
    #[test]
    fn sin_contadores_el_segmento_es_solo_la_secuencia() {
        let preset = parse_keymap(
            r"
[pane]
keymap = [ { on = ['g', 'g'], run = 'cursor.top' } ]
",
        )
        .expect("preset de test");
        let eff = Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse)
            .expect("efectivo de test");
        let mut r = Resolver::new(eff);
        assert!(matches!(
            r.push(parse_chord("g").expect("chord")),
            Resolution::Pending(1)
        ));
        assert_eq!(pending_display(&r), "g");
    }

    /// #112: `COMMANDS` y `Command` nacen del MISMO macro — cada nombre
    /// parsea a su variante. Trivial por construcción; pinea contra un
    /// futuro edit a mano de la lista fuera del macro.
    #[test]
    fn cada_nombre_de_commands_parsea_a_una_variante() {
        for name in COMMANDS {
            assert!(Command::parse(name).is_some(), "{name}");
        }
        assert!(Command::parse("no.existe").is_none());
        assert!(Command::parse("plugin:x:y").is_none(), "plugin: va aparte");
    }

    /// The TUI's `COMMANDS`/`DIALOG_COMMANDS` are now a SUBSET declaration, not a
    /// vocabulary. A name the shared catalogue has never heard of means the two
    /// have drifted — which is the whole class of bug this catalogue removes.
    #[test]
    fn todo_comando_del_tui_esta_en_el_catalogo_compartido() {
        use norte_frontend::keymap::catalogue::{Status, lookup};
        for name in COMMANDS.iter().chain(DIALOG_COMMANDS.iter()) {
            let def = lookup(name)
                .unwrap_or_else(|| panic!("{name} lo implementa el TUI y no está en CATALOGUE"));
            assert_eq!(
                def.status,
                Status::Live,
                "{name} lo implementa el TUI pero el catálogo lo declara Planned"
            );
        }
    }

    /// El arnés de tmux no vio NADA al pulsar `Shift+F2` sobre el preset por
    /// defecto (ni al pulsar `Shift+F6`, que lleva ligado a `pane.rename`
    /// desde hace mucho), mientras el MISMO comando en un `f2` pelado
    /// respondía al instante. Esto pinea las dos mitades que sí están de este
    /// lado, para que la próxima vez que alguien lo mire no tenga que
    /// descartarlas otra vez:
    ///
    /// 1. el adaptador CONSERVA el shift en una tecla de función (solo lo
    ///    descarta en `Char`, donde el carácter ya lo codifica), y
    /// 2. el chord que produce es byte a byte el que parsea `"shift+f2"`, que
    ///    es lo que el preset liga.
    ///
    /// Lo que queda fuera —si el terminal manda la secuencia y si crossterm
    /// la decodifica a `SHIFT + F(2))`— no se puede afirmar sin un terminal, y
    /// es donde apunta la evidencia.
    #[test]
    fn una_tecla_de_funcion_con_shift_conserva_su_shift() {
        let del_terminal = chord_from_crossterm(CtMods::SHIFT, CtCode::F(2));
        assert_eq!(
            del_terminal,
            parse_chord("shift+f2").ok(),
            "el adaptador y el parser del preset tienen que coincidir"
        );
        let (mods, code) = del_terminal.expect("chord").parts();
        assert!(
            mods.shift,
            "el shift no puede perderse en una tecla de función"
        );
        assert_eq!(code, KeyCode::F(2));
    }

    #[test]
    fn chord_from_crossterm_traduce_teclas_conocidas() {
        assert_eq!(
            chord_from_crossterm(CtMods::CONTROL, CtCode::Char('k')),
            Some(Chord::new(
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                KeyCode::Char('k')
            ))
        );
        assert_eq!(
            chord_from_crossterm(CtMods::NONE, CtCode::F(5)),
            Some(Chord::new(Mods::default(), KeyCode::F(5)))
        );
    }

    #[test]
    fn chord_from_crossterm_devuelve_none_para_teclas_no_modeladas() {
        assert_eq!(chord_from_crossterm(CtMods::NONE, CtCode::BackTab), None);
        assert_eq!(chord_from_crossterm(CtMods::NONE, CtCode::CapsLock), None);
    }

    /// #109: xterm clásico reporta Shift+F1 como `F13`, así que un `F(13)`
    /// es construible EN RUNTIME desde este adaptador — pero `parse_chord`
    /// solo acepta `f1..=f12`, con lo que el chord no puede casar ningún
    /// binding y su `Display` (`"f13"`) no re-parsea. Mismo clamp que el
    /// adaptador de la GUI: fuera de rango = tecla no modelada, `None`.
    #[test]
    fn chord_from_crossterm_clampa_f13_y_superiores_como_no_modeladas() {
        assert_eq!(chord_from_crossterm(CtMods::NONE, CtCode::F(13)), None);
        assert_eq!(chord_from_crossterm(CtMods::NONE, CtCode::F(0)), None);
        assert_eq!(chord_from_crossterm(CtMods::NONE, CtCode::F(255)), None);
        assert_eq!(
            chord_from_crossterm(CtMods::NONE, CtCode::F(12)),
            Some(Chord::new(Mods::default(), KeyCode::F(12)))
        );
    }

    #[test]
    fn los_tres_presets_de_fabrica_parsean() {
        for (nombre, _preset) in presets() {
            assert!(!nombre.is_empty());
        }
    }

    /// K2b Task 4, check 1 — HALF A of two. Every bundled preset
    /// (`presets()`, driven by the shared `presets::NAMES`, not a hardcoded
    /// three) builds for all three `Screen`s against the TUI's OWN
    /// vocabulary: `COMMANDS` for Browse/Viewer, `COMMANDS` ∪
    /// `DIALOG_COMMANDS` for Dialog — the same union `main.rs` passes at
    /// startup (`Effective::build_for(preset, &cfg.keymap_layers,
    /// &dialog_known, Screen::Dialog)`).
    ///
    /// Split across two crates on purpose: `norte-frontend` (where
    /// `presets::NAMES` and the shared catalogue live) cannot see EITHER
    /// frontend's `COMMANDS` list — it is upstream of both — so only a
    /// frontend that owns a list can check a preset against it. This is
    /// HALF A. Half B belonged to the GPUI frontend, retired on 2026-08-20
    /// (ADR 0065): the graphical frontend that replaces it owes this repo the
    /// same test against ITS command list, and until it exists no preset is
    /// checked against a graphical surface at all.
    #[test]
    fn todos_los_presets_construyen_las_tres_pantallas_del_tui() {
        let dialog_known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        for (nombre, preset) in presets() {
            for screen in [Screen::Browse, Screen::Viewer, Screen::Dialog] {
                let known: &[&str] = if screen == Screen::Dialog {
                    &dialog_known
                } else {
                    COMMANDS
                };
                Effective::build_for(&preset, &[], known, screen)
                    .unwrap_or_else(|e| panic!("preset {nombre} en {screen:?}: {e}"));
            }
        }
    }

    /// Decisión #23 pineada: en el preset `cua`, Ctrl+C SALE (emergencia
    /// universal) — jamás copy. `pane.copy` se queda en F5. Ligar Ctrl+C a
    /// copy divergiría del Ctrl-C hardcodeado que aborta el cd/refresh (loops
    /// transitorios que no consultan el keymap). Este test fija la decisión:
    /// quien intente rebindear Ctrl+C a copy rompe aquí y ve el porqué.
    #[test]
    fn cua_ctrl_c_es_salir_no_copy() {
        let cua = presets()
            .into_iter()
            .find(|(n, _)| *n == "cua")
            .expect("preset cua")
            .1;
        let eff = Effective::build_for(&cua, &[], COMMANDS, Screen::Browse).expect("cua efectivo");
        let mut r = Resolver::new(eff);
        let ctrl_c = Chord::new(
            Mods {
                ctrl: true,
                ..Default::default()
            },
            KeyCode::Char('c'),
        );
        assert_eq!(
            r.push(ctrl_c),
            Resolution::Run {
                command: "app.quit".to_owned(),
                count: Count::None
            }
        );
        // Copy vive en F5, no en un chord de Ctrl.
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::F(5))),
            Resolution::Run {
                command: "pane.copy".to_owned(),
                count: Count::None
            }
        );
    }

    /// H1 T2: los tres presets construyen `Screen::Dialog` con el
    /// `known_commands` UNIÓN (`COMMANDS` ∪ `DIALOG_COMMANDS` —
    /// `build_for_impl` valida TODO el efectivo fusionado, incluido
    /// `[global]`, contra la lista que le pasa el caller; T1 lo confirmó).
    /// `y` resuelve `dialog.approve` en los tres (preset idéntico).
    #[test]
    fn dialog_commands_se_resuelven_en_los_tres_presets() {
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        for (nombre, preset) in presets() {
            let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog)
                .unwrap_or_else(|e| panic!("preset {nombre}: {e}"));
            let mut r = Resolver::new(eff);
            assert_eq!(
                r.push(Chord::new(Mods::default(), KeyCode::Char('y'))),
                Resolution::Run {
                    command: "dialog.approve".to_owned(),
                    count: Count::None
                },
                "preset {nombre}"
            );
        }
    }

    #[test]
    fn help_id_reemplaza_puntos_por_guiones() {
        assert_eq!(help_id("app.quit"), "help-cmd-app-quit");
        assert_eq!(
            help_id("pane.delete-permanent"),
            "help-cmd-pane-delete-permanent"
        );
    }

    #[test]
    fn dialog_hint_id_pela_el_prefijo_dialog_punto() {
        assert_eq!(dialog_hint_id("dialog.confirm"), "dialog-cmd-confirm");
        assert_eq!(dialog_hint_id("dialog.page-up"), "dialog-cmd-page-up");
        assert_eq!(
            dialog_hint_id("dialog.toggle-enabled"),
            "dialog-cmd-toggle-enabled"
        );
    }

    /// #108 7a: las teclas del picker de columnas resuelven en los TRES
    /// presets A TRAVÉS del adaptador de crossterm real — pinea las
    /// decisiones de chord verificadas en el plan:
    /// - `shift+up`/`shift+down` → move-up/move-down: `parse_chord`
    ///   CONSERVA shift en teclas no-Char y `chord_from_crossterm` también,
    ///   así que el chord del preset casa el evento SHIFT+flecha.
    /// - `K`/`J` → move-up/move-down: crossterm entrega `Char('J')`+SHIFT y
    ///   `Chord::new` DESCARTA shift en Char — casa el binding `"J"`.
    ///   (`shift+j` como texto NO parsea: `ShiftWithChar`.)
    /// - `ctrl+s` → dialog.sort: `s` a secas ya es `dialog.skip` (colisión
    ///   del modal de colisiones) — el fallback del plan.
    #[test]
    fn columns_picker_chords_resuelven_via_adaptador_crossterm() {
        let expected = [
            ((CtMods::SHIFT, CtCode::Up), "dialog.move-up"),
            ((CtMods::SHIFT, CtCode::Down), "dialog.move-down"),
            ((CtMods::SHIFT, CtCode::Char('K')), "dialog.move-up"),
            ((CtMods::SHIFT, CtCode::Char('J')), "dialog.move-down"),
            ((CtMods::CONTROL, CtCode::Char('s')), "dialog.sort"),
            // #108 7b: `f` cicla el formato (libre en los tres [dialog]).
            ((CtMods::NONE, CtCode::Char('f')), "dialog.cycle-format"),
        ];
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        // K2b: `presets()` ahora también trae `total-commander`/`krusader`,
        // que NO comparten `alt+c` → `pane.columns` (no está en sus fuentes,
        // y rule 1 prohíbe inventarlo) — este test pinea la convención
        // PROPIA de norte, así que se queda en los tres presets nativos.
        for (nombre, preset) in presets()
            .into_iter()
            .filter(|(n, _)| matches!(*n, "orthodox" | "vim" | "cua"))
        {
            let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog)
                .unwrap_or_else(|e| panic!("preset {nombre}: {e}"));
            for ((mods, code), command) in &expected {
                let mut r = Resolver::new(eff.clone());
                let chord = chord_from_crossterm(*mods, *code)
                    .unwrap_or_else(|| panic!("preset {nombre}: chord no modelado {code:?}"));
                assert_eq!(
                    r.push(chord),
                    Resolution::Run {
                        command: (*command).to_owned(),
                        count: Count::None
                    },
                    "preset {nombre}: {command}"
                );
            }
            // Y `alt+c` abre el picker desde el pane.
            let browse = Effective::build_for(&preset, &[], COMMANDS, Screen::Browse)
                .unwrap_or_else(|e| panic!("preset {nombre}: {e}"));
            let mut r = Resolver::new(browse);
            let alt_c = chord_from_crossterm(CtMods::ALT, CtCode::Char('c')).expect("alt+c");
            assert_eq!(
                r.push(alt_c),
                Resolution::Run {
                    command: "pane.columns".to_owned(),
                    count: Count::None
                },
                "preset {nombre}: pane.columns"
            );
        }
    }

    /// The five pane/navigation gestures resolve in the three factory
    /// presets, THROUGH the real crossterm adapter. A preset that loses one
    /// leaves the gesture unreachable by keyboard while every other test
    /// stays green — the same regression class the mark commands are pinned
    /// against.
    ///
    /// `pane.swap` is deliberately NOT the same chord everywhere: `ctrl+u` is
    /// already `cursor.page-up` in `vim`, and the vim idiom outranks the
    /// borrowed one in its own preset, so there it is `alt+s`. This test
    /// spells out both so a future edit that "unifies" them has to argue with
    /// the reason.
    ///
    /// K2b: scoped to the three NATIVE presets on purpose. `total-commander`
    /// and `krusader` transcribe a foreign program (plan rule 1): TC has no
    /// "mirror" key at all, and Krusader's own "adopt the other panel's
    /// path" is `ctrl+=`, not `alt+u` — inventing `alt+i`/`alt+u` there to
    /// pass this pin would be exactly the approximation rule 1 forbids.
    #[test]
    fn pane_gesture_chords_resolve_in_the_three_presets() {
        let common = [
            ((CtMods::ALT, CtCode::Char('i')), "pane.mirror"),
            ((CtMods::ALT, CtCode::Char('u')), "pane.pull"),
            ((CtMods::ALT, CtCode::Left), "nav.back"),
            ((CtMods::ALT, CtCode::Right), "nav.forward"),
        ];
        for (name, preset) in presets()
            .into_iter()
            .filter(|(n, _)| matches!(*n, "orthodox" | "vim" | "cua"))
        {
            let eff = Effective::build_for(&preset, &[], COMMANDS, Screen::Browse)
                .unwrap_or_else(|e| panic!("preset {name}: {e}"));
            let swap = if name == "vim" {
                (CtMods::ALT, CtCode::Char('s'))
            } else {
                (CtMods::CONTROL, CtCode::Char('u'))
            };
            for ((mods, code), command) in common.iter().chain(&[(swap, "pane.swap")]) {
                let mut r = Resolver::new(eff.clone());
                let chord = chord_from_crossterm(*mods, *code)
                    .unwrap_or_else(|| panic!("preset {name}: chord no modelado {code:?}"));
                assert_eq!(
                    r.push(chord),
                    Resolution::Run {
                        command: (*command).to_owned(),
                        count: Count::None
                    },
                    "preset {name}: {command}"
                );
            }
        }
    }

    /// En `vim`, `ctrl+u` SIGUE siendo `cursor.page-up`: el chord de
    /// `pane.swap` de los otros dos presets no lo pisó. (Mutación de control:
    /// ligar ahí `pane.swap` rompe este test.)
    #[test]
    fn vim_ctrl_u_sigue_siendo_page_up() {
        let vim = presets()
            .into_iter()
            .find(|(n, _)| *n == "vim")
            .expect("preset vim")
            .1;
        let eff = Effective::build_for(&vim, &[], COMMANDS, Screen::Browse).expect("vim efectivo");
        let mut r = Resolver::new(eff);
        let ctrl_u = chord_from_crossterm(CtMods::CONTROL, CtCode::Char('u')).expect("ctrl+u");
        assert_eq!(
            r.push(ctrl_u),
            Resolution::Run {
                command: "cursor.page-up".to_owned(),
                count: Count::None
            }
        );
    }

    /// The six mark commands resolve in the three factory presets (#103). A
    /// preset that loses one leaves the selection unreachable by keyboard,
    /// which is the regression class the GUI already hit once.
    ///
    /// K2b: scoped to the three NATIVE presets. `total-commander` and
    /// `krusader` bind the same SIX commands (rule 1 fidelity check, not
    /// this pin's job), but on different chords — `ctrl+A`/`ctrl+shift+a`
    /// for `mark.clear` is norte's own convention, not Krusader's (which is
    /// `alt+-`) or TC's (`ctrl+-`, TC's own `CTRL+NUM-`).
    #[test]
    fn mark_commands_resolve_in_the_three_presets() {
        let expected = [
            (Chord::new(Mods::default(), KeyCode::Insert), "mark.toggle"),
            (
                Chord::new(
                    Mods {
                        ctrl: true,
                        ..Default::default()
                    },
                    KeyCode::Char('a'),
                ),
                "mark.all",
            ),
            (
                Chord::new(Mods::default(), KeyCode::Char('*')),
                "mark.invert",
            ),
            (
                // `alt+a`, no `ctrl+A`. Este par —`ctrl+a` para marcar todo y
                // su mayúscula para desmarcar— se leía bien y no funcionaba:
                // el terminal manda el MISMO byte para Ctrl+A y Ctrl+Shift+A,
                // así que `mark.clear` estaba anunciado y muerto en los tres
                // presets nativos. Lo caza
                // `ningun_preset_ata_un_acorde_que_el_terminal_no_entrega`.
                Chord::new(
                    Mods {
                        alt: true,
                        ..Default::default()
                    },
                    KeyCode::Char('a'),
                ),
                "mark.clear",
            ),
            (
                Chord::new(Mods::default(), KeyCode::Char('+')),
                "mark.pattern-add",
            ),
            (
                Chord::new(Mods::default(), KeyCode::Char('-')),
                "mark.pattern-remove",
            ),
        ];
        for (name, preset) in presets()
            .into_iter()
            .filter(|(n, _)| matches!(*n, "orthodox" | "vim" | "cua"))
        {
            let eff = Effective::build_for(&preset, &[], COMMANDS, Screen::Browse)
                .unwrap_or_else(|e| panic!("preset {name}: {e}"));
            for (chord, command) in &expected {
                let mut r = Resolver::new(eff.clone());
                assert_eq!(
                    r.push(*chord),
                    Resolution::Run {
                        command: (*command).to_owned(),
                        count: Count::None
                    },
                    "preset {name}: {command}"
                );
            }
        }
    }
}

/// Parsea una `key` de fila de plugin de la palette de vuelta a
/// `(plugin_id, command_id)`.
///
/// Vive en `norte-frontend`, JUNTO a `palette::plugin_rows`, que es quien
/// COMPONE esa clave: el que la escribe y el que la lee no pueden estar en
/// dos crates con dos respuestas sobre dónde empieza el `command_id` —que es
/// justo la mitad sin charset validado—. Se re-exporta con su nombre de
/// siempre para que ningún call site de este crate se mueva.
pub use norte_frontend::palette::parse_plugin_key;

#[cfg(test)]
mod parse_plugin_key_tests {
    use super::parse_plugin_key;

    #[test]
    fn separa_plugin_id_y_command_id() {
        assert_eq!(
            parse_plugin_key("plugin:org.norte.demo:greet"),
            Some(("org.norte.demo", "greet"))
        );
    }

    /// El `command_id` NO tiene charset validado (a diferencia del
    /// `plugin_id`): puede llevar `:` o saltos de línea, y el split se
    /// queda con TODO lo que sigue al primero, sin volver a partir.
    #[test]
    fn command_id_hostil_se_toma_entero_sin_repartir() {
        assert_eq!(
            parse_plugin_key("plugin:org.norte.demo:a:b\nc"),
            Some(("org.norte.demo", "a:b\nc"))
        );
    }

    #[test]
    fn sin_prefijo_plugin_es_none() {
        assert_eq!(parse_plugin_key("app.quit"), None);
        assert_eq!(parse_plugin_key(""), None);
    }

    /// Sin el segundo `:` (formato mínimo `plugin:x` sin `command_id`): `None`
    /// — un despacho parcial jamás corre `plugin_run_command` con un id
    /// vacío o adivinado.
    #[test]
    fn sin_segundo_separador_es_none() {
        assert_eq!(parse_plugin_key("plugin:org.norte.demo"), None);
    }

    /// `plugin_id` vacío (`"plugin::greet"`) es `None` — nunca alcanzable
    /// desde una fila real (`PluginInfo.id` siempre no-vacío, validado por
    /// el core), pero el parser no debe entregar un id vacío a
    /// `plugin_run_command` si alguna vez lo fuera.
    #[test]
    fn plugin_id_vacio_es_none() {
        assert_eq!(parse_plugin_key("plugin::greet"), None);
    }
}
