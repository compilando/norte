//! [`Role`]: los papeles SEMÁNTICOS que un tema estiliza (ADR 0020 D2). El
//! frontend pide un rol, nunca un color suelto. Cada rol trae un
//! [`fallback`](Role::fallback) monocromo que reproduce el aspecto de M1, de
//! modo que SIN tema (o con uno parcial) la UI sigue siendo coherente.

use serde::{Deserialize, Serialize};

use crate::style::Style;

/// Papel semántico de la UI. Añadir una variante es no-breaking: un tema que no
/// la cubre hereda su [`fallback`](Role::fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Role {
    /// Fondo BASE de toda la pantalla. Un tema claro fija aquí su `bg` claro;
    /// el frontend lo pinta primero y el resto de estilos (solo `fg`) lo
    /// conservan. Sin definir = fondo del terminal (comportamiento de M1).
    Background,
    /// Texto normal / entrada de fichero por defecto.
    Regular,
    /// Fila seleccionada en un panel.
    Selection,
    /// Borde del panel con foco: dice a cuál de los paneles van las teclas
    /// de navegación. No es [`Role::FocusBorder`], que es el anillo de un
    /// CONTROL dentro de un diálogo; los nombres se parecen y significan
    /// cosas distintas.
    BorderFocus,
    /// Borde del panel sin foco.
    BorderUnfocused,
    /// Borde de un modal/diálogo.
    ModalBorder,
    /// Barra de estado.
    StatusBar,
    /// Título de panel/modal.
    Title,
    /// Marca de nombre hostil (bytes no imprimibles, control…).
    HostileBadge,
    /// Mensaje de error.
    Error,
    /// Mensaje de aviso (p. ej. borrado permanente).
    Warning,
    /// Mensaje informativo.
    Info,
    /// Coincidencia de búsqueda resaltada.
    Match,
    /// Pane interior background (GUI chrome; the TUI may adopt it later).
    PaneBackground,
    /// Focused pane interior background.
    PaneFocusBackground,
    /// Marked entry (selection marks, distinct from the cursor's
    /// `Selection`) — two different consumers, so a preset's `mark = { bg =
    /// ... }` reads differently in each: the GUI applies it as the marked
    /// row's BACKGROUND across the whole row; the TUI applies it only as the
    /// style of a one-cell gutter glyph (`*`) at the start of the row, so the
    /// same `bg` shows up as a small tinted cell rather than a full-row
    /// background. Style it with `bg` only (no `fg`) so both readings stay
    /// legible.
    Mark,
    /// Fila del cursor en un panel SIN foco (spec 2026-09-10). Existe
    /// porque `Selection` pasó a pintarse con el color de acento, y dos
    /// cursores igual de vivos no dicen cuál recibe las teclas: el del panel
    /// sin foco se queda en el gris de antes, presente pero apagado.
    SelectionUnfocused,
    /// Un botón de diálogo (`[ Enter  Confirm ]`): cada modal pinta su línea
    /// de teclas con este rol cuando `[ui] dialog_buttons` está encendido.
    Button,

    // --- Cromo de la ventana (spec 2026-09-11, F2) ---------------------
    //
    // Los diez que siguen nombran SUPERFICIES, no significados: son lo que
    // hace que una ventana se parezca a un editor concreto en vez de a un
    // formulario. Comparten tres rasgos que los separan de los de arriba:
    //
    // 1. NO están en [`Role::CORE`], así que un preset no tiene que
    //    definirlos (ver el rustdoc de esa constante).
    // 2. Su [`Role::fallback`] no lleva color. El valor sensato NO se puede
    //    escribir aquí: depende de la paleta del tema, y la hoja de estilos
    //    de la ventana lo deriva con `var(--hover, var(--panel-focus-bg))`.
    // 3. No son [`Role::REQUESTABLE`]: un plugin no puede pedirlos para una
    //    insignia, porque el color del deslizador de la barra de
    //    desplazamiento no significa nada pegado a un nombre de fichero.
    //
    /// Fila bajo el PUNTERO, en un panel o en una lista. Distinta del cursor
    /// (`Selection`): el ratón está encima, las teclas no van ahí. Pierde
    /// contra el cursor y contra una fila marcada.
    Hover,
    /// Fondo de un campo de texto (diálogos, paleta, ajustes).
    InputBackground,
    /// Borde de un campo de texto. Es el filete del control, no el del panel
    /// (`BorderUnfocused`) ni el anillo de foco (`FocusBorder`).
    InputBorder,
    /// Fondo de un WIDGET flotante: la paleta de comandos, un desplegable,
    /// el menú, el `which-key`. Se apoya sobre el fondo base y por eso
    /// normalmente es un poco más claro que `PaneBackground`.
    WidgetBackground,
    /// El color de la SOMBRA de esos widgets. Existe porque un negro cosido
    /// al CSS es una sombra que en un tema claro se ve como suciedad.
    WidgetShadow,
    /// Una INSIGNIA con fondo: un contador, una etiqueta.
    ///
    /// Sirve a DOS consumidores, igual que [`Role::Mark`], y la pareja
    /// `fg`/`bg` se lee distinta en cada uno: la ventana lo usa como el
    /// contador de un panel lateral, y el panel de registro como el chip que
    /// marca una línea del daemon. Defínelo con `bg` Y `fg`: un chip sin
    /// primer plano hereda el color de la línea que marca, que es
    /// justamente lo que el chip tiene que distinguir.
    Badge,
    /// El DESLIZADOR de la barra de desplazamiento (el canal va
    /// transparente). Solo la ventana: un terminal no pinta barra.
    ScrollbarSlider,
    /// El filete que separa dos SUPERFICIES del cromo — la barra de teclas
    /// del listado, la de paneles de la de menús, el panel lateral del
    /// central.
    ///
    /// Es lo que permite el aspecto «por elevación» de los editores
    /// modernos: un tema que lo pone casi igual a su fondo deja de tener
    /// filetes sin que la hoja de estilos sepa nada de ese tema. No es el
    /// borde de un panel con o sin foco — esos son [`Role::BorderFocus`] y
    /// [`Role::BorderUnfocused`], y significan dónde van las teclas.
    Separator,
    /// El anillo de foco de un CONTROL: un campo, un botón, una casilla.
    ///
    /// No confundir con [`Role::BorderFocus`], que es el borde del PANEL que
    /// tiene el foco. Los nombres se parecen peligrosamente y dicen cosas
    /// distintas: este marca qué control recibe lo que teclees dentro de un
    /// diálogo; aquel, cuál de los dos paneles recibe las teclas de
    /// navegación.
    FocusBorder,
    /// Texto ATENUADO pero legible: migas de pan, un tamaño, una columna
    /// secundaria, la descripción de un ajuste. Es un color propio y no un
    /// `dim` sobre `Regular` porque `dim` en un terminal es un atributo que
    /// muchos emuladores ignoran.
    Muted,
}

impl Role {
    /// Los roles que un preset está OBLIGADO a colorear: los dieciocho que
    /// existían antes del cromo de la ventana (spec 2026-09-11, F2).
    ///
    /// Es lo que itera la completitud de los presets, y no [`Self::ALL`], por
    /// una razón concreta: los diez roles de CROMO se derivan en la hoja de
    /// estilos de la ventana de colores que el tema ya tiene, así que
    /// exigírselos a cada preset serían ochenta valores inventados — y el
    /// monocromo de [`Self::fallback`] es un mal defecto para ellos (un
    /// `hover` sin color no es un hover prudente, es uno invisible).
    pub const CORE: &'static [Role] = &[
        Role::Background,
        Role::Regular,
        Role::Selection,
        Role::BorderFocus,
        Role::BorderUnfocused,
        Role::ModalBorder,
        Role::StatusBar,
        Role::Title,
        Role::HostileBadge,
        Role::Error,
        Role::Warning,
        Role::Info,
        Role::Match,
        Role::PaneBackground,
        Role::PaneFocusBackground,
        Role::Mark,
        Role::SelectionUnfocused,
        Role::Button,
    ];

    /// Los roles que un PLUGIN puede nombrar en un span o en una decoración
    /// (ADR 0037, y la enmienda de la spec 2026-09-11).
    ///
    /// El criterio es uno solo: **un plugin describe CONTENIDO**, así que
    /// puede nombrar lo que un trozo de contenido SIGNIFICA —que es un
    /// error, un aviso, un título, una coincidencia— y no puede nombrar
    /// nada de lo que la ventana usa para decir en qué ESTADO está. Quedan
    /// fuera, por tanto, dos familias:
    ///
    /// - El **cromo** (`hover`, `scrollbar-slider`, `widget-*`, `input-*`,
    ///   `separator`, `focus-border`, `widget-shadow`): una insignia pintada
    ///   con el color del deslizador de la barra de desplazamiento no
    ///   significa nada.
    /// - El **estado** (`selection`, `selection-unfocused`, `status-bar`,
    ///   `mark`, `background`, `pane-*`, `border-*`, `modal-border`,
    ///   `button`): dónde está el cursor, qué hay marcado y cuál es el panel
    ///   con foco son cosas que el plugin no sabe y que, pintadas por él,
    ///   mentirían.
    ///
    /// Esto ESTRECHA el vocabulario que ADR 0037 dejaba abierto a todo
    /// [`Self::ALL`]. Un nombre no pedible degrada a `None` por
    /// [`Self::from_kebab_requestable`] — la misma degradación que ya tenía
    /// un nombre desconocido, y por el mismo motivo: un guest más nuevo no
    /// puede romper el render de un norte más viejo.
    pub const REQUESTABLE: &'static [Role] = &[
        Role::Regular,
        Role::Title,
        Role::HostileBadge,
        Role::Error,
        Role::Warning,
        Role::Info,
        Role::Match,
        Role::Badge,
        Role::Muted,
    ];

    /// Todos los roles, para iterar (p. ej. comprobar que los nombres kebab
    /// hacen ida y vuelta). Es [`Self::CORE`] más los diez de cromo.
    pub const ALL: &'static [Role] = &[
        Role::Background,
        Role::Regular,
        Role::Selection,
        Role::BorderFocus,
        Role::BorderUnfocused,
        Role::ModalBorder,
        Role::StatusBar,
        Role::Title,
        Role::HostileBadge,
        Role::Error,
        Role::Warning,
        Role::Info,
        Role::Match,
        Role::PaneBackground,
        Role::PaneFocusBackground,
        Role::Mark,
        Role::SelectionUnfocused,
        Role::Button,
        // Cromo de la ventana (spec 2026-09-11, F2).
        Role::Hover,
        Role::InputBackground,
        Role::InputBorder,
        Role::WidgetBackground,
        Role::WidgetShadow,
        Role::Badge,
        Role::ScrollbarSlider,
        Role::Separator,
        Role::FocusBorder,
        Role::Muted,
    ];

    /// Estilo por defecto MONOCROMO del rol: reproduce el aspecto de M1
    /// (`BOLD`/`REVERSED`/`DIM` donde hoy los hay) sin color. Es lo que se usa
    /// cuando el tema no define el rol, de modo que un usuario sin tema ve
    /// exactamente la UI de siempre.
    #[must_use]
    pub const fn fallback(self) -> Style {
        match self {
            Role::Selection | Role::StatusBar | Role::Button => Style::new().reverse(),
            // El cursor sin foco: visible sin color, pero no el mismo que el
            // que recibe las teclas.
            Role::SelectionUnfocused => Style::new().reverse().dim(),
            Role::BorderFocus | Role::ModalBorder | Role::HostileBadge | Role::Title => {
                Style::new().bold()
            }
            // BorderUnfocused/Mark: Mark, distinto de Selection (reverse) pero
            // visible sin color, comparte el atenuado del borde sin foco —
            // "presente pero no activo".
            Role::BorderUnfocused | Role::Mark => Style::new().dim(),
            // Background/Regular/Error/Warning/Info/Match: sin color por defecto
            // (la UI de M1 no los distinguía; Background sin fijar = fondo del
            // terminal). Un tema con color los diferencia.
            // PaneBackground/PaneFocusBackground: chrome nuevo de la GUI, sin
            // equivalente en la TUI de M1; mismo tratamiento que Background
            // (sin color = fondo heredado del backend).
            //
            // Los diez de CROMO tampoco llevan color, y por un motivo
            // distinto que merece decirse: su defecto sensato NO SE PUEDE
            // ESCRIBIR AQUÍ. Un `hover` correcto es «el fondo del panel con
            // foco de ESTE tema», y un literal no puede seguir a ocho
            // paletas. La derivación vive en la hoja de estilos de la
            // ventana (`var(--hover, var(--panel-focus-bg))`), que es el
            // único sitio donde los dos colores están a la vez.
            Role::Background
            | Role::Regular
            | Role::Error
            | Role::Warning
            | Role::Info
            | Role::Match
            | Role::PaneBackground
            | Role::PaneFocusBackground
            | Role::Hover
            | Role::InputBackground
            | Role::InputBorder
            | Role::WidgetBackground
            | Role::WidgetShadow
            | Role::Badge
            | Role::ScrollbarSlider
            | Role::Separator
            | Role::FocusBorder
            | Role::Muted => Style::new(),
        }
    }

    /// Parsea un nombre en kebab-case (el MISMO que produce/consume la
    /// serialización serde de este tipo, `#[serde(rename_all =
    /// "kebab-case")]`) al [`Role`] correspondiente. `None` si `s` no es un
    /// nombre reconocido del conjunto CERRADO (ADR 0037, decisión 3 y su
    /// enmienda de límite de responsabilidad): la validación de un `role`
    /// que llega en datos de un plugin (`SpanWire::role`/`DecorationWire::
    /// role`, `norte-proto`) vive en el FRONTEND que posee el tema
    /// (`norte-core` no depende de `norte-theme`), y este es el punto de
    /// entrada único — reutiliza el derive serde existente como fuente de
    /// verdad del nombre en vez de duplicar una tabla de match que podría
    /// desincronizarse de `#[serde(rename_all = "kebab-case")]`. Un nombre
    /// desconocido (de un plugin más nuevo, o de un fork con roles propios)
    /// degrada a `None` — nunca un error — para que un guest de un futuro
    /// norte (o de otro fork) no rompa el render de uno más viejo.
    ///
    /// ```
    /// use norte_theme::Role;
    /// assert_eq!(Role::from_kebab("hostile-badge"), Some(Role::HostileBadge));
    /// assert_eq!(Role::from_kebab("pane-background"), Some(Role::PaneBackground));
    /// assert_eq!(Role::from_kebab("not-a-role"), None);
    /// assert_eq!(Role::from_kebab(""), None);
    /// ```
    #[must_use]
    pub fn from_kebab(s: &str) -> Option<Role> {
        serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
    }

    /// [`Self::from_kebab`] acotado a [`Self::REQUESTABLE`]: el punto de
    /// entrada ÚNICO de un nombre de rol que viene de un plugin.
    ///
    /// Existe para que la restricción viva donde vive la lista, y no
    /// repetida en cada frontend que valida un dato de un guest.
    ///
    /// ```
    /// use norte_theme::Role;
    /// // Un significado: pasa.
    /// assert_eq!(Role::from_kebab_requestable("error"), Some(Role::Error));
    /// // Cromo de la ventana: degrada, no es un error.
    /// assert_eq!(Role::from_kebab_requestable("scrollbar-slider"), None);
    /// // Y sigue existiendo para quien pregunte sin filtro.
    /// assert!(Role::from_kebab("scrollbar-slider").is_some());
    /// ```
    #[must_use]
    pub fn from_kebab_requestable(s: &str) -> Option<Role> {
        Self::from_kebab(s).filter(|r| Self::REQUESTABLE.contains(r))
    }

    /// El nombre kebab de un rol: el inverso exacto de [`Self::from_kebab`].
    ///
    /// Hace falta para los frontends que no comparten memoria con el host —el
    /// renderer gráfico recibe una CADENA, no un enum— y para que un rol que
    /// cruza y vuelve sea el mismo rol.
    ///
    /// ```
    /// use norte_theme::Role;
    /// assert_eq!(Role::HostileBadge.as_kebab(), "hostile-badge");
    /// assert_eq!(Role::from_kebab(Role::PaneBackground.as_kebab()), Some(Role::PaneBackground));
    /// ```
    ///
    /// El `match` es exhaustivo y sin comodín, así que un rol nuevo deja de
    /// compilar aquí; y `as_kebab_es_el_nombre_de_serde` comprueba, rol a rol
    /// sobre [`Self::ALL`], que dice lo mismo que la serialización — que es
    /// lo que impide que las dos tablas se separen.
    #[must_use]
    pub const fn as_kebab(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Regular => "regular",
            Self::Selection => "selection",
            Self::BorderFocus => "border-focus",
            Self::BorderUnfocused => "border-unfocused",
            Self::ModalBorder => "modal-border",
            Self::StatusBar => "status-bar",
            Self::Title => "title",
            Self::HostileBadge => "hostile-badge",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Match => "match",
            Self::Mark => "mark",
            Self::PaneBackground => "pane-background",
            Self::PaneFocusBackground => "pane-focus-background",
            Self::SelectionUnfocused => "selection-unfocused",
            Self::Button => "button",
            Self::Hover => "hover",
            Self::InputBackground => "input-background",
            Self::InputBorder => "input-border",
            Self::WidgetBackground => "widget-background",
            Self::WidgetShadow => "widget-shadow",
            Self::Badge => "badge",
            Self::ScrollbarSlider => "scrollbar-slider",
            Self::Separator => "separator",
            Self::FocusBorder => "focus-border",
            Self::Muted => "muted",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Role;

    /// `as_kebab` dice lo MISMO que serde, rol a rol.
    ///
    /// Sin esto son dos tablas que se separan en el primer rol nuevo: el
    /// `match` deja de compilar, sí, pero nada obliga a que el nombre que se
    /// escriba allí sea el que sale por el cable.
    #[test]
    fn as_kebab_es_el_nombre_de_serde() {
        for &role in Role::ALL {
            let por_serde = serde_json::to_value(role).expect("serializa");
            assert_eq!(
                por_serde.as_str(),
                Some(role.as_kebab()),
                "{role:?} se llama distinto según quién pregunte"
            );
            assert_eq!(Role::from_kebab(role.as_kebab()), Some(role));
        }
    }

    #[test]
    fn from_kebab_todos_los_roles_hacen_roundtrip() {
        // Fuente única: si `Role::ALL` gana una variante y su nombre kebab
        // cambia de forma, este test la ejercita SIN necesidad de listar los
        // nombres a mano (evita la duplicación que el rustdoc de
        // `from_kebab` explícitamente quiere evitar).
        for &role in Role::ALL {
            let kebab = serde_json::to_value(role)
                .expect("Role serializa")
                .as_str()
                .expect("Role serializa a string")
                .to_owned();
            assert_eq!(
                Role::from_kebab(&kebab),
                Some(role),
                "roundtrip kebab de {role:?}"
            );
        }
    }

    #[test]
    fn from_kebab_desconocido_es_none() {
        // Nombres que un plugin ajeno al tema podría mandar (ADR 0037): no
        // deben panicar ni colar como Role válido.
        assert_eq!(Role::from_kebab("number"), None);
        assert_eq!(Role::from_kebab("keyword"), None);
        assert_eq!(Role::from_kebab("HostileBadge"), None); // no es kebab-case
    }

    /// `CORE` es un SUBCONJUNTO de `ALL`, y `ALL` no pierde a nadie.
    ///
    /// Los dos conjuntos existen porque miden cosas distintas: `ALL` es el
    /// vocabulario entero, `CORE` es lo que un preset está OBLIGADO a
    /// colorear. Sin esta comprobación, un rol nuevo puede caer fuera de los
    /// dos y no existir para nadie.
    #[test]
    fn core_es_subconjunto_de_all_y_all_los_tiene_a_todos() {
        for &r in Role::CORE {
            assert!(Role::ALL.contains(&r), "{r:?} está en CORE y no en ALL");
        }
        assert_eq!(Role::CORE.len(), 18, "CORE son los dieciocho de siempre");
        assert_eq!(Role::ALL.len(), 28, "ALL son esos más los diez de cromo");
    }

    /// Lo PEDIBLE por un plugin es un subconjunto de lo que existe, y deja
    /// fuera tanto el cromo como las superficies de ESTADO de la ventana
    /// (ADR 0037 + spec 2026-09-11, F2).
    #[test]
    fn lo_pedible_deja_fuera_el_cromo_y_el_estado() {
        for &r in Role::REQUESTABLE {
            assert!(Role::ALL.contains(&r), "{r:?} es pedible y no existe");
        }
        // Cromo: el color del deslizador no significa nada en una insignia.
        for r in [
            Role::ScrollbarSlider,
            Role::WidgetShadow,
            Role::InputBorder,
            Role::Separator,
            Role::Hover,
        ] {
            assert!(!Role::REQUESTABLE.contains(&r), "{r:?} es cromo");
        }
        // Estado de la ventana: dónde está el cursor, qué hay marcado, cuál
        // es el panel con foco. Un plugin describe CONTENIDO, y no sabe nada
        // de eso.
        for r in [
            Role::Selection,
            Role::SelectionUnfocused,
            Role::StatusBar,
            Role::Mark,
            Role::Background,
            Role::Button,
        ] {
            assert!(
                !Role::REQUESTABLE.contains(&r),
                "{r:?} es estado, no significado"
            );
        }
        // Y las señales que un plugin SÍ necesita para decir algo.
        for r in [
            Role::Error,
            Role::Warning,
            Role::Info,
            Role::Title,
            Role::Match,
            Role::Regular,
            Role::HostileBadge,
            Role::Badge,
            Role::Muted,
        ] {
            assert!(
                Role::REQUESTABLE.contains(&r),
                "{r:?} tiene que ser pedible"
            );
        }
    }

    /// El punto de entrada de un nombre que viene de un plugin: un rol no
    /// pedible degrada a `None`, igual que un nombre desconocido. No es un
    /// error — un guest de un norte más nuevo no puede romper el render de
    /// uno más viejo (ADR 0037).
    #[test]
    fn from_kebab_requestable_degrada_lo_no_pedible_a_none() {
        assert_eq!(Role::from_kebab_requestable("warning"), Some(Role::Warning));
        assert_eq!(Role::from_kebab_requestable("muted"), Some(Role::Muted));
        assert_eq!(Role::from_kebab_requestable("scrollbar-slider"), None);
        assert_eq!(Role::from_kebab_requestable("selection"), None);
        assert_eq!(Role::from_kebab_requestable("no-existe"), None);
        // Y siguen siendo roles de verdad para quien pregunte sin filtro.
        assert_eq!(
            Role::from_kebab("scrollbar-slider"),
            Some(Role::ScrollbarSlider)
        );
        assert_eq!(Role::from_kebab("selection"), Some(Role::Selection));
    }

    /// Los diez roles de cromo NO están en CORE: se DERIVAN en la hoja de
    /// estilos de la ventana a partir de colores que el tema ya tiene (spec
    /// 2026-09-11, F2), y por eso un preset no tiene que definirlos. Exigirlos
    /// serían ochenta valores inventados repartidos por los ocho presets que
    /// ya existen.
    #[test]
    fn los_roles_de_cromo_quedan_fuera_de_core() {
        for r in [
            Role::Hover,
            Role::InputBackground,
            Role::InputBorder,
            Role::WidgetBackground,
            Role::WidgetShadow,
            Role::Badge,
            Role::ScrollbarSlider,
            Role::Separator,
            Role::FocusBorder,
            Role::Muted,
        ] {
            assert!(
                !Role::CORE.contains(&r),
                "{r:?} no debería exigírsele a cada preset"
            );
            assert!(Role::ALL.contains(&r), "{r:?} tiene que existir");
        }
    }
}
