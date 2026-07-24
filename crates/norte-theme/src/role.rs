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
    /// Borde del panel con foco.
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
    /// Marked-entry background (selection marks, distinct from the cursor's
    /// `Selection`).
    Mark,
}

impl Role {
    /// Todos los roles, para iterar (p. ej. validar que un preset los cubre).
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
    ];

    /// Estilo por defecto MONOCROMO del rol: reproduce el aspecto de M1
    /// (`BOLD`/`REVERSED`/`DIM` donde hoy los hay) sin color. Es lo que se usa
    /// cuando el tema no define el rol, de modo que un usuario sin tema ve
    /// exactamente la UI de siempre.
    #[must_use]
    pub const fn fallback(self) -> Style {
        match self {
            Role::Selection | Role::StatusBar => Style::new().reverse(),
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
            Role::Background
            | Role::Regular
            | Role::Error
            | Role::Warning
            | Role::Info
            | Role::Match
            | Role::PaneBackground
            | Role::PaneFocusBackground => Style::new(),
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
}

#[cfg(test)]
mod tests {
    use super::Role;

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
}
