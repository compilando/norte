//! Menú contextual del botón derecho (plan de ratón, tarea 4): estado PURO,
//! sin GPUI — igual que `columns_view`/`palette_view`, `main.rs` es quien
//! pinta y quien despacha.
//!
//! Tres decisiones viven aquí, y las tres son de las que un file manager no
//! puede equivocarse:
//!
//! 1. **Sobre qué actúa.** Si la fila pulsada está MARCADA, el menú actúa
//!    sobre las marcas; si no, sobre esa fila y nada más. Es la regla que usa
//!    cualquier file manager, y el menú la DICE ([`Target`] va en la
//!    cabecera): actuar sobre once ficheros cuando el usuario señalaba uno es
//!    justo el error que la regla existe para evitar.
//! 2. **Qué se puede ejecutar AHORA.** Una entrada que no puede correr se
//!    pinta DESHABILITADA con su motivo, nunca escondida — un menú que cambia
//!    de forma no se aprende. El veredicto NO se decide aquí: lo da la tabla
//!    compartida [`norte_frontend::availability`], la misma que atenúa las
//!    filas de la ayuda en la TUI (H3d). Una tabla, dos frontends: un menú
//!    que apaga «copiar» mientras la ayuda lo anuncia disponible es peor que
//!    cualquiera de los dos por separado. El vocabulario de motivos es el de
//!    [`norte_help`] ([`Availability`]/[`Reason`]).
//! 3. **Qué comando dispara cada entrada.** SIEMPRE uno de
//!    [`crate::keymap::COMMANDS`], el mismo nombre que resuelve el teclado y
//!    que ejecuta `NorteGui::run_command`. Este módulo no ejecuta nada: sólo
//!    dice qué comando corresponde, y sólo si la entrada está disponible.
//!
//! Los textos salen de Fluent en el momento de PINTAR (los `Item` guardan la
//! clave, no la cadena), así que el menú habla el idioma vigente y no el que
//! hubiera al abrirlo.

use norte_frontend::availability::{Facts, reason_key, verdict};
use norte_help::Availability;
use norte_proto::EntryKind;

/// Tope de caracteres del nombre que la cabecera del menú cita (encoding: el
/// nombre viene de un fichero de terceros y ya llega ENMASCARADO, pero
/// `mask_terminal_hazards` no capa longitud — un nombre kilométrico
/// desbordaría el panel y empujaría fuera de la vista la parte que importa).
/// Mismo criterio de elipsis final que el resto del chrome de la GUI.
const TARGET_MAX_CHARS: usize = 40;

/// Sobre qué actúa el menú. Se decide UNA vez, al abrirlo, y se pinta: nadie
/// debe deducirlo del estado del pane mientras el menú está delante.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// La fila pulsada NO estaba marcada: actúa sobre ella sola. Lleva su
    /// etiqueta ya saneada (la misma que pinta la fila).
    Entry(String),
    /// La fila pulsada estaba marcada: actúa sobre las `n` marcas del pane.
    Marks(usize),
}

impl Target {
    /// Texto localizado de la cabecera («actúa sobre …»).
    #[must_use]
    pub fn text(&self) -> String {
        let what = match self {
            Self::Entry(name) => elided(name),
            Self::Marks(n) => norte_i18n::ta("gui-menu-target-marks", &[("n", &n.to_string())]),
        };
        norte_i18n::ta("gui-menu-acts-on", &[("target", &what)])
    }

    /// Cuántas entradas abarca (1 para una fila suelta).
    #[must_use]
    pub fn count(&self) -> usize {
        match self {
            Self::Entry(_) => 1,
            Self::Marks(n) => *n,
        }
    }
}

/// Recorta a [`TARGET_MAX_CHARS`] con elipsis FINAL. Por caracteres, nunca
/// por bytes (partir un UTF-8 a medias no es una opción, regla 1).
fn elided(name: &str) -> String {
    if name.chars().count() <= TARGET_MAX_CHARS {
        return name.to_owned();
    }
    let mut out: String = name.chars().take(TARGET_MAX_CHARS).collect();
    out.push('…');
    out
}

/// Una entrada del menú: el comando que despacha, la clave Fluent de su
/// etiqueta y si puede correr ahora.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Comando de [`crate::keymap::COMMANDS`] — el MISMO que el teclado.
    pub command: &'static str,
    /// Clave Fluent de la etiqueta (se resuelve al pintar, no al abrir).
    pub label_key: &'static str,
    /// Si no puede correr, por qué.
    pub avail: Availability,
}

impl Item {
    /// Texto localizado de la entrada: la etiqueta a secas si está
    /// disponible, o «etiqueta — motivo» si no. El motivo se DICE: una
    /// entrada apagada y muda se lee como un fallo del programa.
    #[must_use]
    pub fn text(&self) -> String {
        let label = norte_i18n::t(self.label_key);
        match self.avail.reason() {
            None => label,
            Some(reason) => norte_i18n::ta(
                "gui-menu-entry-disabled",
                &[
                    ("label", &label),
                    ("reason", &norte_i18n::t(reason_key(reason))),
                ],
            ),
        }
    }
}

/// Qué LADOS se niegan a mutar, para [`facts_for`].
///
/// Un par de booleanos con nombre y no dos parámetros sueltos: el literal de
/// struct que esto sustituyó nombraba los dos campos en el sitio de la
/// llamada, y una firma `(…, bool, bool)` los deja intercambiables en
/// silencio. Cambiados de orden compila, y `pane.copy`/`pane.move` quedan
/// vetados al revés — el menú prohíbe copiar hacia un destino escribible y
/// ofrece copiar hacia uno que no lo es, que es casi verosímil. Clippy no
/// avisa: `fn_params_excessive_bools` no salta hasta tres, y los tests que
/// pasan `true, true` no distinguen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOnly {
    /// El pane sobre el que el comando ACTÚA (el del menú).
    pub source: bool,
    /// El pane al que escribiría (el otro).
    pub dest: bool,
}

/// Los [`Facts`] de la GUI a partir de lo que sabe de la fila pulsada. Es la
/// mitad que el frontend NO puede delegar, y por eso vive aquí: la tabla
/// compartida pregunta «se puede entrar» / «se puede ver», no «qué tipo de
/// entrada es», porque los dos frontends contestan distinto. En la GUI, Enter
/// entra en un DIRECTORIO y nada más (no hay composición de scheme de archivo
/// como en la TUI, donde un `.zip` se entra), y el visor abre FICHEROS; las
/// dos cosas exigen además que el objetivo sea UNO solo: «abrir» once
/// ficheros a la vez no significa nada.
///
/// Nada de esto se recalcula mientras el menú está abierto: un menú que cambia
/// bajo el puntero es peor que uno desfasado, que además caduca solo (ver
/// [`ContextMenu::is_stale`]).
#[must_use]
pub fn facts_for(kind: EntryKind, count: usize, read_only: ReadOnly) -> Facts {
    let single = count == 1;
    let ReadOnly {
        source: source_read_only,
        dest: dest_read_only,
    } = read_only;
    Facts {
        enterable: single && kind == EntryKind::Dir,
        viewable: single && kind == EntryKind::File,
        // La GUI se NIEGA a renombrar una selección múltiple: renombraría la
        // del cursor a espaldas del objetivo que el menú anuncia, y renombrar
        // en bloque sería un batch-rename, otra feature. La TUI llena este
        // mismo hecho con `true` porque su shift+F6 apunta al cursor y punto
        // — de ahí que sea un hecho del llamador y no del recuento.
        rename_single: single,
        source_read_only,
        dest_read_only,
        // La GUI no sigue la pista de las degradaciones de conexión (la TUI sí,
        // por scheme). Además hoy no veta nada en la tabla: «degradada»
        // significa sesión sin cifrar, no sesión inservible.
        degraded: false,
    }
}

/// Las entradas del menú, en orden de pintado. TODAS aparecen SIEMPRE: lo que
/// cambia entre contextos es su [`Availability`], no la lista. Cada
/// [`Availability`] la da [`verdict`]: este módulo aporta la LISTA y las
/// etiquetas, jamás un criterio propio de disponibilidad.
#[must_use]
pub fn items(facts: &Facts) -> Vec<Item> {
    // «Abrir» es lo que hace Enter: entrar en el directorio. La GUI no tiene
    // abridor externo (`pane.open` es de la TUI), así que sobre un fichero no
    // hay comando que despachar y la entrada lo dice (vía `enterable`).
    //
    // El rename de IA sigue estando, y sigue siendo OTRA cosa que
    // `pane.rename`: actúa sobre la carpeta entera, no sobre el objetivo del
    // menú, y la etiqueta lo dice en voz alta en vez de fingir un alcance que
    // no tiene.
    [
        ("nav.enter", "gui-menu-open"),
        ("pane.view", "gui-menu-view"),
        ("pane.copy", "gui-menu-copy"),
        ("pane.move", "gui-menu-move"),
        ("pane.rename", "gui-menu-rename"),
        ("pane.ai-rename", "gui-menu-rename-ai"),
        ("pane.delete", "gui-menu-delete"),
        ("pane.copy-path", "gui-menu-copy-path"),
    ]
    .into_iter()
    .map(|(command, label_key)| Item {
        command,
        label_key,
        avail: verdict(command, facts),
    })
    .collect()
}

/// Lo que una tecla le hace al menú.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuOutcome {
    /// Consumida (o ignorada): el menú sigue abierto. Un overlay jamás deja
    /// caer teclas a los panes de abajo.
    None,
    /// Cerrar sin ejecutar nada.
    Close,
    /// Ejecutar este comando (y cerrar).
    Run(&'static str),
}

/// El menú abierto: dónde, sobre qué, con qué entradas.
///
/// Sin `Eq`: el ancla son píxeles (`f32`). Comparar menús por igualdad exacta
/// no le hace falta a nadie — lo que se compara es el objetivo y las
/// entradas, que sí son `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextMenu {
    /// Pane de la fila pulsada (el que tiene el foco mientras el menú vive).
    pub pane: usize,
    /// Ancla en píxeles de ventana (donde estaba el puntero).
    pub anchor: (f32, f32),
    /// Sobre qué actúa.
    pub target: Target,
    /// Entradas, en orden de pintado.
    pub items: Vec<Item>,
    /// Fila resaltada (navegación con teclado).
    pub cursor: usize,
    /// `PaneState::listing_epoch` del pane al abrir: si cambia, el menú
    /// nombra otros ficheros y caduca (ver [`Self::is_stale`]).
    epoch: u64,
}

impl ContextMenu {
    /// Abre el menú sobre `target`, con las entradas que `facts` permita.
    #[must_use]
    pub fn open(
        pane: usize,
        epoch: u64,
        anchor: (f32, f32),
        target: Target,
        facts: &Facts,
    ) -> Self {
        Self {
            pane,
            anchor,
            target,
            items: items(facts),
            cursor: 0,
            epoch,
        }
    }

    /// El comando de la entrada `i`, o `None` si no existe o está
    /// DESHABILITADA. Es el único portillo por el que el menú despacha: una
    /// entrada apagada no ejecuta nada, la active el ratón o el teclado.
    #[must_use]
    pub fn activate(&self, i: usize) -> Option<&'static str> {
        let item = self.items.get(i)?;
        item.avail.is_available().then_some(item.command)
    }

    /// Enruta UNA tecla: ↑/↓ mueven el resaltado, Enter ejecuta la entrada
    /// resaltada (si puede), Esc cierra. Cualquier otra tecla se CONSUME sin
    /// efecto (captura fija de overlay).
    pub fn on_key(&mut self, key: &str) -> MenuOutcome {
        match key {
            "escape" => MenuOutcome::Close,
            "up" => {
                self.cursor = self.cursor.saturating_sub(1);
                MenuOutcome::None
            }
            "down" => {
                let last = self.items.len().saturating_sub(1);
                self.cursor = (self.cursor + 1).min(last);
                MenuOutcome::None
            }
            "enter" => match self.activate(self.cursor) {
                Some(cmd) => MenuOutcome::Run(cmd),
                // Enter sobre una deshabilitada NO cierra: cerrar sería
                // indistinguible de haber ejecutado algo.
                None => MenuOutcome::None,
            },
            _ => MenuOutcome::None,
        }
    }

    /// ¿El listado se movió bajo el menú? Los índices y el objetivo se
    /// fijaron contra el listado que había al abrirlo; un `cd` o un refresh
    /// asíncrono tras una mutación los deja nombrando otros ficheros, así que
    /// el menú se cierra en vez de operar sobre lo que ya no señala.
    #[must_use]
    pub fn is_stale(&self, epoch: u64) -> bool {
        epoch != self.epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_help::Reason;
    use norte_i18n::{Lang, t_in};

    fn facts() -> Facts {
        facts_for(
            EntryKind::File,
            1,
            ReadOnly {
                source: false,
                dest: false,
            },
        )
    }

    /// Deshabilitado por `reason` (los tests comparan veredictos completos;
    /// la tabla compartida no exporta su propio atajo).
    fn no(reason: Reason) -> Availability {
        Availability::Unavailable { reason }
    }

    fn menu(f: &Facts, target: Target) -> ContextMenu {
        ContextMenu::open(0, 7, (10.0, 20.0), target, f)
    }

    /// Cada entrada despacha un comando que el TECLADO también despacha: el
    /// menú no puede tener un camino propio (ni un comando inventado, ni una
    /// segunda implementación de una operación).
    #[test]
    fn cada_entrada_despacha_un_comando_del_teclado() {
        for item in items(&facts()) {
            assert!(
                crate::keymap::COMMANDS.contains(&item.command),
                "{:?} no es un comando del keymap de la GUI",
                item.command
            );
        }
    }

    /// Las siete operaciones del plan están presentes SIEMPRE, disponibles o
    /// no: un menú que cambia de forma según el contexto no se aprende.
    #[test]
    fn el_menu_tiene_las_mismas_entradas_en_todo_contexto() {
        let esperados: Vec<&str> = items(&facts()).iter().map(|i| i.command).collect();
        assert_eq!(esperados.len(), 8);
        for (kind, ro_src, ro_dst, count) in [
            (EntryKind::Dir, false, false, 1),
            (EntryKind::File, true, true, 9),
            (EntryKind::Symlink, false, true, 3),
        ] {
            let f = facts_for(
                kind,
                count,
                ReadOnly {
                    source: ro_src,
                    dest: ro_dst,
                },
            );
            let got: Vec<&str> = items(&f).iter().map(|i| i.command).collect();
            assert_eq!(got, esperados, "la lista cambió con {kind:?}");
        }
    }

    /// Una entrada deshabilitada NO despacha, ni con el ratón (`activate`) ni
    /// con Enter — y Enter tampoco cierra el menú (cerrar se leería como que
    /// algo se ejecutó).
    #[test]
    fn una_entrada_deshabilitada_no_despacha() {
        let f = facts_for(
            EntryKind::File,
            1,
            ReadOnly {
                source: true,
                dest: true,
            },
        );
        let mut m = menu(&f, Target::Entry("x".into()));
        let borrar = m
            .items
            .iter()
            .position(|i| i.command == "pane.delete")
            .expect("la entrada de borrar existe siempre");
        assert_eq!(
            m.items[borrar].avail,
            no(Reason::ReadOnlyBackend),
            "backend de solo lectura"
        );
        assert_eq!(m.activate(borrar), None, "el ratón no la ejecuta");
        m.cursor = borrar;
        assert_eq!(m.on_key("enter"), MenuOutcome::None, "el teclado tampoco");

        // Y la de al lado, que sí puede, sigue despachando.
        let ruta = m
            .items
            .iter()
            .position(|i| i.command == "pane.copy-path")
            .expect("copiar la ruta existe siempre");
        assert_eq!(m.activate(ruta), Some("pane.copy-path"));
    }

    /// El motivo se PINTA junto a la entrada apagada, no sólo se guarda.
    #[test]
    fn la_entrada_deshabilitada_dice_su_motivo() {
        let f = facts_for(
            EntryKind::Dir,
            1,
            ReadOnly {
                source: false,
                dest: true,
            },
        );
        let m = menu(&f, Target::Entry("d".into()));
        let mover = m.items.iter().find(|i| i.command == "pane.move").unwrap();
        let texto = mover.text();
        assert!(
            texto.len() > norte_i18n::t("gui-menu-move").len(),
            "el texto de una entrada apagada añade el motivo: {texto:?}"
        );
        let ver = m.items.iter().find(|i| i.command == "pane.view").unwrap();
        assert_eq!(ver.text(), ver.text(), "estable entre llamadas");
        assert_eq!(
            ver.avail,
            no(Reason::WrongTarget),
            "un directorio no se abre en el visor"
        );
    }

    /// Abrir/ver dependen del tipo Y de que el objetivo sea UNO: «abrir» once
    /// ficheros a la vez no significa nada.
    #[test]
    fn abrir_y_ver_dependen_del_tipo_y_de_ser_uno_solo() {
        let dir = facts_for(
            EntryKind::Dir,
            1,
            ReadOnly {
                source: false,
                dest: false,
            },
        );
        let abrir = |f: &Facts| {
            items(f)
                .into_iter()
                .find(|i| i.command == "nav.enter")
                .unwrap()
                .avail
        };
        let ver = |f: &Facts| {
            items(f)
                .into_iter()
                .find(|i| i.command == "pane.view")
                .unwrap()
                .avail
        };
        assert_eq!(abrir(&dir), Availability::Available);
        assert_eq!(ver(&dir), no(Reason::WrongTarget));
        assert_eq!(abrir(&facts()), no(Reason::WrongTarget));
        assert_eq!(ver(&facts()), Availability::Available);

        let varios = facts_for(
            EntryKind::Dir,
            4,
            ReadOnly {
                source: false,
                dest: false,
            },
        );
        assert_eq!(
            abrir(&varios),
            no(Reason::WrongTarget),
            "cuatro marcas no se «abren»"
        );
    }

    /// Renombrar es UNA entrada: con varias marcas se apaga (renombraría la
    /// del cursor a espaldas del objetivo que el menú anuncia), y dentro de
    /// un archivo se apaga por el backend — con los dos impedimentos a la
    /// vez gana el del backend, que es el que habría que resolver primero.
    /// La entrada de IA sigue siendo OTRA, y no depende del recuento porque
    /// actúa sobre la carpeta.
    #[test]
    fn renombrar_es_una_sola_entrada_y_la_de_ia_es_otra() {
        let avail = |f: &Facts, cmd: &str| {
            items(f)
                .into_iter()
                .find(|i| i.command == cmd)
                .unwrap_or_else(|| panic!("falta {cmd}"))
                .avail
        };
        assert_eq!(avail(&facts(), "pane.rename"), Availability::Available);
        assert_eq!(avail(&facts(), "pane.ai-rename"), Availability::Available);

        let marcas = facts_for(
            EntryKind::File,
            7,
            ReadOnly {
                source: false,
                dest: false,
            },
        );
        assert_eq!(avail(&marcas, "pane.rename"), no(Reason::WrongTarget));
        assert_eq!(
            avail(&marcas, "pane.ai-rename"),
            Availability::Available,
            "el rename de IA es de la CARPETA: el recuento no le afecta"
        );

        let zip = facts_for(
            EntryKind::File,
            7,
            ReadOnly {
                source: true,
                dest: false,
            },
        );
        assert_eq!(
            avail(&zip, "pane.rename"),
            no(Reason::ReadOnlyBackend),
            "con dos impedimentos gana el del backend"
        );
        assert_eq!(avail(&zip, "pane.ai-rename"), no(Reason::ReadOnlyBackend));
    }

    /// Esc cierra; ↑/↓ mueven sin salirse; una tecla ajena se consume.
    #[test]
    fn esc_cierra_y_las_flechas_mueven_sin_desbordar() {
        let mut m = menu(&facts(), Target::Marks(3));
        assert_eq!(m.on_key("escape"), MenuOutcome::Close);
        assert_eq!(m.on_key("up"), MenuOutcome::None);
        assert_eq!(m.cursor, 0, "arriba en la primera no desborda");
        for _ in 0..20 {
            m.on_key("down");
        }
        assert_eq!(m.cursor, m.items.len() - 1, "abajo se para en la última");
        assert_eq!(m.on_key("x"), MenuOutcome::None, "tecla ajena: se consume");
        assert_eq!(m.cursor, m.items.len() - 1);
    }

    /// El menú caduca cuando el listado se mueve bajo él.
    #[test]
    fn el_menu_caduca_con_un_listado_nuevo() {
        let m = menu(&facts(), Target::Entry("a".into()));
        assert!(!m.is_stale(7), "el mismo listado no caduca nada");
        assert!(m.is_stale(8), "otro listado sí");
    }

    /// La cabecera DICE sobre qué actúa, y un nombre kilométrico se recorta
    /// (nunca empuja el resto del panel fuera de la vista).
    #[test]
    fn la_cabecera_dice_el_objetivo_y_recorta_el_nombre() {
        let marcas = Target::Marks(11).text();
        assert!(marcas.contains("11"), "la cabecera cita el recuento");
        assert_eq!(Target::Marks(11).count(), 11);

        let largo = "n".repeat(TARGET_MAX_CHARS * 3);
        let texto = Target::Entry(largo).text();
        assert!(texto.contains('…'), "el nombre largo se recorta: {texto:?}");
        assert!(
            texto.chars().count() < TARGET_MAX_CHARS * 3,
            "y de verdad, no sólo con la elipsis"
        );
    }

    /// Toda clave Fluent del menú existe en los DOS locales (el test de
    /// paridad de `norte-i18n` cubre el catálogo entero; esto cubre que las
    /// claves que este módulo NOMBRA son claves reales).
    #[test]
    fn las_claves_del_menu_existen_en_ambos_locales() {
        let mut claves: Vec<&str> = items(&facts()).iter().map(|i| i.label_key).collect();
        claves.extend([
            "gui-menu-acts-on",
            "gui-menu-target-marks",
            "gui-menu-entry-disabled",
            "gui-menu-hint",
            "gui-menu-copied",
            // Los motivos ya no llevan prefijo `gui-`: son COMPARTIDOS con la
            // ayuda de la TUI (H3d), y los nombra
            // `norte_frontend::availability::reason_key`.
            reason_key(Reason::ReadOnlyBackend),
            reason_key(Reason::WrongTarget),
        ]);
        for clave in claves {
            for lang in [Lang::Es, Lang::En] {
                assert_ne!(t_in(lang, clave), clave, "falta {clave} en {lang:?}");
            }
        }
    }
}
