//! Qué necesita saber el motor de un panel, y el registro que se lo dice.
//!
//! Lo que NO está aquí es cómo se pinta: eso es una tabla por frontend, porque
//! el TUI pinta ratatui y la GUI pinta GPUI. El motor solo necesita tamaños
//! mínimos, si toma foco, si toma teclas, si admite varias instancias y a qué
//! roles puede optar.

use super::{KindId, RoleId};

/// Lo que el motor necesita saber de un kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindDecl {
    /// Qué kind describe.
    pub id: KindId,
    /// Ancho y alto MÍNIMOS en celdas. Por debajo de esto,
    /// [`super::resolve`] colapsa el `Split` que lo contiene.
    pub min: (u16, u16),
    /// ¿Puede tener el foco?
    pub focusable: bool,
    /// ¿Consume teclas de su propio namespace?
    pub takes_keys: bool,
    /// ¿Pueden coexistir varias instancias?
    pub multi: bool,
    /// A qué roles puede optar este kind.
    pub roles: &'static [RoleId],
}

/// Los roles que un `browser` puede tomar: los dos.
const ROLES_BROWSER: &[RoleId] = &[RoleId::Active, RoleId::Target];
/// Ningún rol.
const SIN_ROLES: &[RoleId] = &[];

/// Los kinds que este binario sabe pintar.
///
/// ABIERTO por construcción: [`KindRegistry::get`] devuelve `None` para lo que
/// no conoce y eso **no es un error** — quien pinta dibuja una caja con el
/// nombre y el layout conserva el nodo intacto. Es lo que permite que un
/// frontend abra el layout del otro sin borrarle nada, y más adelante que un
/// plugin aporte un kind.
#[derive(Debug, Clone, Default)]
pub struct KindRegistry {
    decls: Vec<KindDecl>,
}

impl KindRegistry {
    /// Los cinco kinds que existen hoy, re-encuadrados: `browser`, `tasks`,
    /// `viewer`, `compare` y `sync`.
    ///
    /// Los mínimos salen de lo que la pantalla de hoy necesita de verdad: un
    /// `browser` por debajo de 20 columnas no pinta ni un nombre con su
    /// tamaño, y `compare`/`sync` llevan dos lados y una cabecera.
    #[must_use]
    pub fn builtin() -> Self {
        let decl = |id: &str, min, focusable, takes_keys, multi, roles| KindDecl {
            id: KindId::new(id),
            min,
            focusable,
            takes_keys,
            multi,
            roles,
        };
        Self {
            decls: vec![
                decl("browser", (20, 5), true, true, true, ROLES_BROWSER),
                // La franja de tareas: se mira, no se enfoca, y hay una.
                decl("tasks", (20, 3), false, false, false, SIN_ROLES),
                decl("viewer", (20, 5), true, true, false, SIN_ROLES),
                decl("compare", (40, 8), true, true, false, SIN_ROLES),
                decl("sync", (40, 8), true, true, false, SIN_ROLES),
            ],
        }
    }

    /// La declaración de `id`, o `None` si este binario no conoce ese kind.
    #[must_use]
    pub fn get(&self, id: &KindId) -> Option<&KindDecl> {
        self.decls.iter().find(|d| &d.id == id)
    }

    /// Añade o reemplaza una declaración.
    pub fn insert(&mut self, decl: KindDecl) {
        if let Some(slot) = self.decls.iter_mut().find(|d| d.id == decl.id) {
            *slot = decl;
        } else {
            self.decls.push(decl);
        }
    }

    /// El mínimo de un kind, o `(1, 1)` si no se conoce: un kind desconocido
    /// se pinta igual (caja con su nombre), así que no puede exigir sitio que
    /// nadie sabe cuánto es.
    #[must_use]
    pub fn min_of(&self, id: &KindId) -> (u16, u16) {
        self.get(id).map_or((1, 1), |d| d.min)
    }

    /// ¿Puede este kind tomar el rol `role`? Un kind desconocido, jamás.
    #[must_use]
    pub fn holds_role(&self, id: &KindId, role: RoleId) -> bool {
        self.get(id).is_some_and(|d| d.roles.contains(&role))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un kind que el registro no conoce no revienta: devuelve `None` y quien
    /// pinta dibuja la caja con el nombre. Es la regla 3 del modelo.
    #[test]
    fn un_kind_fuera_del_registro_no_es_un_error() {
        let reg = KindRegistry::builtin();
        assert!(reg.get(&KindId::new("terminal")).is_none());
        assert_eq!(reg.min_of(&KindId::new("terminal")), (1, 1));
        assert!(!reg.holds_role(&KindId::new("terminal"), RoleId::Target));
    }

    /// Los mínimos son lo único que el motor consulta para colapsar, así que
    /// declararlos mal se nota en toda la pantalla.
    #[test]
    fn el_browser_declara_su_minimo_y_puede_tomar_los_dos_roles() {
        let reg = KindRegistry::builtin();
        let d = reg.get(&KindId::browser()).expect("browser está");
        assert_eq!(d.min, (20, 5));
        assert!(d.focusable && d.takes_keys && d.multi);
        assert_eq!(d.roles, &[RoleId::Active, RoleId::Target]);
    }

    /// `tasks` es la franja de abajo: no toma foco, no toma teclas, y hay UNA.
    #[test]
    fn tasks_es_unico_y_no_toma_foco() {
        let reg = KindRegistry::builtin();
        let d = reg.get(&KindId::new("tasks")).expect("tasks está");
        assert!(!d.focusable && !d.takes_keys && !d.multi);
        assert!(d.roles.is_empty());
    }

    /// `insert` REEMPLAZA: dos declaraciones del mismo kind harían que `get`
    /// devolviera una y `min_of` la otra según el orden, que es la clase de
    /// bug que solo aparece cuando alguien añade un kind.
    #[test]
    fn insertar_el_mismo_kind_dos_veces_reemplaza() {
        let mut reg = KindRegistry::builtin();
        reg.insert(KindDecl {
            id: KindId::browser(),
            min: (99, 99),
            focusable: false,
            takes_keys: false,
            multi: false,
            roles: SIN_ROLES,
        });
        assert_eq!(reg.min_of(&KindId::browser()), (99, 99));
        assert!(!reg.holds_role(&KindId::browser(), RoleId::Target));
    }
}
