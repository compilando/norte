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

/// El nombre de hueco de un panel aportado por un plugin (fase 3).
///
/// `plugin:<id>:<kind>`, y el prefijo es la garantía: [`KindId`] es un
/// `String` sin validar, así que lo único que impide que un plugin declare un
/// panel llamado `browser` y secuestre el listado es que su nombre real nunca
/// empieza por `plugin:`. Los dos frontends lo forman AQUÍ y no cada uno por
/// su cuenta, que es como dos superficies acaban abriendo huecos distintos
/// para el mismo panel.
///
/// ```
/// use norte_frontend::layout::panel_kind_id;
///
/// let id = panel_kind_id("org.norte.git-panel", "git");
/// assert_eq!(id.as_str(), "plugin:org.norte.git-panel:git");
/// ```
#[must_use]
pub fn panel_kind_id(plugin_id: &str, kind: &str) -> KindId {
    KindId::new(format!("plugin:{plugin_id}:{kind}"))
}

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
                // La barra de estado: una fila, nadie la enfoca.
                decl("status", (1, 1), false, false, false, SIN_ROLES),
                // El sidebar de sitios (L3): se enfoca y toma teclas, pero NO
                // opta a ningún rol — un sidebar jamás es el destino de una
                // copia. Y hay uno: dos listas idénticas de discos no son un
                // layout, son un fallo. El mínimo de 14 columnas es lo que
                // ocupa `/boot 402M` con el marco alrededor.
                decl("places", (14, 5), true, true, false, SIN_ROLES),
                decl("viewer", (20, 5), true, true, false, SIN_ROLES),
                decl("compare", (40, 8), true, true, false, SIN_ROLES),
                decl("sync", (40, 8), true, true, false, SIN_ROLES),
                // El panel de procesos: la franja `tasks` sigue existiendo y
                // sigue siendo lo que trae `orthodox`. Este es el panel de
                // verdad —se enfoca, se recorre y cancela la fila del cursor—
                // y hay uno. El mínimo de 30x4 es lo que ocupa una fila con
                // nombre, barra y porcentaje.
                decl("processes", (30, 4), true, true, false, SIN_ROLES),
                // La hoja de atributos: sigue al rol `active` con el mismo
                // vínculo que el visor acoplado. 24 columnas es la etiqueta
                // más larga con su valor al lado.
                //
                // NO toma teclas, y declararlo era la mitad de #243: la hoja
                // sigue al cursor del listado, así que con el teclado dentro
                // dejaría de seguir a nada. Se enfoca —el reparto la cuenta—
                // pero no consume ninguna tecla.
                decl("metadata", (24, 4), true, false, false, SIN_ROLES),
                // El árbol de directorios (#136): se enfoca, toma teclas y hay
                // UNO. No opta a ningún rol —un árbol no es el destino de una
                // copia, igual que el sidebar—, y 16 columnas es lo que ocupa
                // un nombre corto con dos niveles de sangrado y el marco.
                decl("tree", (16, 5), true, true, false, SIN_ROLES),
                // El registro (#323): se enfoca, toma teclas —filtra por nivel
                // y por texto— y hay UNO. No opta a ningún rol: nadie copia a
                // un log. 30 columnas es lo que ocupa `13:36:50 WARN` con un
                // mensaje corto y el marco; por debajo la hora y el nivel se
                // comen la línea entera y no queda sitio para lo que dice.
                decl("log", (30, 4), true, true, false, SIN_ROLES),
                // El mapa de disco (fase 4): se enfoca, toma teclas —se anda
                // por los rectángulos y se entra en uno— y hay UNO. No opta a
                // ningún rol: un mapa se mira y se recorre, y nadie copia
                // dentro de un treemap.
                //
                // 24x6 es el mínimo con el que sigue siendo un MAPA. A lo
                // ancho, 24 columnas es lo que ocupa una etiqueta como
                // `documentos 1,2G` con el marco alrededor; por debajo los
                // rectángulos dejan de caber con su nombre y lo que queda es
                // un mosaico de colores sin leyenda. A lo alto, seis filas son
                // dos tiras con su etiqueta más el marco: con menos solo cabe
                // una tira, y una sola tira no reparte nada — es una barra.
                decl("disk-map", (24, 6), true, true, false, SIN_ROLES),
            ],
        }
    }

    /// Todas las declaraciones, EN ORDEN DE REGISTRO: primero las de serie,
    /// luego lo que se haya añadido.
    ///
    /// El orden es parte del contrato y no un detalle: la barra de paneles
    /// (#324) lo usa para pintar los de siempre en el mismo sitio y lo aportado
    /// detrás, que es lo que permite aprender la posición con el dedo.
    #[must_use]
    pub fn decls(&self) -> &[KindDecl] {
        &self.decls
    }

    /// La declaración de `id`, o `None` si este binario no conoce ese kind.
    #[must_use]
    pub fn get(&self, id: &KindId) -> Option<&KindDecl> {
        self.decls.iter().find(|d| &d.id == id)
    }

    /// Declara los paneles que aportan los plugins CONSENTIDOS (fase 3).
    ///
    /// Un panel se llama `plugin:<id>:<kind>`, y ese prefijo es lo que impide
    /// que choque con uno de casa: `KindId` no valida nada —es un `String`—,
    /// así que la garantía la da el NOMBRE, no el tipo. Un plugin llamado
    /// `browser` no puede secuestrar el listado.
    ///
    /// Solo los aprobados Y activados, con el mismo criterio que las columnas
    /// (`validated_plugin_requests`): un panel de un plugin que el lector no
    /// ha consentido no existe para el reparto, así que su hueco no se coloca
    /// y su botón no sale en la barra.
    ///
    /// REEMPLAZA lo aportado, no lo añade: retirar el consentimiento a un
    /// plugin tiene que retirar su panel en la misma sesión. Añadiendo, un
    /// plugin desactivado en el gestor conservaba su kind declarado hasta el
    /// siguiente arranque —su hueco seguía colocándose y tomando foco—, que es
    /// lo contrario de lo que promete el párrafo de arriba. Las de serie no se
    /// tocan, y lo aportado se reconstruye entero en cada catálogo.
    ///
    /// Dentro de eso el orden se mantiene: `decls()` promete las de serie
    /// primero y lo aportado detrás.
    pub fn insert_panels(&mut self, plugins: &[norte_proto::methods::PluginInfo]) {
        // El alfabeto de un nombre que va a un `KindId`: ASCII alfanumérico y
        // `. _ -`, con tope. Deja fuera el espacio, los dos puntos —que son el
        // separador del propio prefijo—, los controles, los saltos de línea y
        // cualquier cosa de ancho doble o de derecha a izquierda.
        let valido = |s: &str| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        };
        self.decls.retain(|d| !d.id.as_str().starts_with("plugin:"));
        for p in plugins.iter().filter(|p| p.approved && p.enabled) {
            for panel in &p.panels {
                // El id y el kind son texto de un TERCERO y acaban en un
                // `KindId`, que no valida nada: de ahí salen el nombre que se
                // pinta y la clave que se guarda en la sesión. Un kind con un
                // salto de línea, un carácter de ancho doble o una secuencia
                // de escape rompe la barra y el fichero de disposición, así
                // que lo que no encaje en el alfabeto no se declara — el panel
                // desaparece, que es el fallo seguro.
                if !valido(&p.id) || !valido(&panel.kind) {
                    continue;
                }
                self.insert(KindDecl {
                    id: panel_kind_id(&p.id, &panel.kind),
                    // Lo que el manifiesto pida, y si no pide nada, el mínimo
                    // de un panel lateral cualquiera: por debajo de eso no
                    // cabe ni una línea con su marco.
                    min: (panel.min_cols.unwrap_or(20), panel.min_rows.unwrap_or(4)),
                    // Se enfoca y toma teclas: un panel que no pudiera recibir
                    // una tecla no podría ofrecer nada que no fuera un clic, y
                    // el guest recibe COMANDOS precisamente para eso.
                    focusable: true,
                    takes_keys: true,
                    // Uno de cada: dos copias del mismo panel de git no son un
                    // layout, son un fallo. Mismo criterio que los laterales
                    // de casa.
                    multi: false,
                    // Ningún rol: un panel de plugin no es el destino de una
                    // copia, igual que el sidebar o el árbol.
                    roles: SIN_ROLES,
                });
            }
        }
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

    /// Los dos kinds de la fase A. Ninguno opta a un rol: un panel de
    /// procesos y una hoja de atributos jamás son el destino de una copia, y
    /// dejarles `Target` es como una tecla de copiar acaba apuntando a una
    /// caja que no es un directorio.
    ///
    /// Y solo UNO de los dos toma teclas. La hoja sigue al cursor del
    /// listado, así que con el teclado dentro dejaría de seguir a nada;
    /// declararlo al revés era la mitad de #243 —la otra mitad era que nadie
    /// leía el `KeyOwner` que se ponía—, y el resultado en pantalla era un
    /// panel con borde de foco cuyas flechas movían la lista de al lado.
    #[test]
    fn processes_y_metadata_se_enfocan_pero_no_son_destino() {
        let reg = KindRegistry::builtin();
        for id in ["processes", "metadata"] {
            let d = reg.get(&KindId::new(id)).expect("declarado");
            assert!(d.focusable, "{id} se enfoca");
            assert!(!d.multi, "{id} es uno solo");
            assert!(d.roles.is_empty(), "{id} no opta a rol");
            assert!(!reg.holds_role(&KindId::new(id), RoleId::Target));
        }
        assert!(
            reg.get(&KindId::new("processes"))
                .expect("declarado")
                .takes_keys,
            "el panel de procesos SÍ toma teclas: se recorre y cancela"
        );
        assert!(
            !reg.get(&KindId::new("metadata"))
                .expect("declarado")
                .takes_keys,
            "la hoja de atributos NO: sigue al cursor del listado"
        );
        assert_eq!(reg.min_of(&KindId::new("processes")), (30, 4));
        assert_eq!(reg.min_of(&KindId::new("metadata")), (24, 4));
    }

    /// Un kind que el registro no conoce no revienta: devuelve `None` y quien
    /// pinta dibuja la caja con el nombre. Es la regla 3 del modelo.
    #[test]
    fn un_kind_fuera_del_registro_no_es_un_error() {
        let reg = KindRegistry::builtin();
        assert!(reg.get(&KindId::new("terminal")).is_none());
        assert_eq!(reg.min_of(&KindId::new("terminal")), (1, 1));
        assert!(!reg.holds_role(&KindId::new("terminal"), RoleId::Target));
    }

    /// Un panel APORTADO no sale en la barra, aunque se enfoque.
    ///
    /// El comando de un botón es `layout.<kind>`, y para uno aportado sería
    /// `layout.plugin:git:status`, que no existe en ningún catálogo: la TUI lo
    /// tiraba en silencio y la ventana contestaba «cmd-not-here». La misma
    /// decisión con dos respuestas es justo lo que el ADR 0077 prohíbe, así
    /// que hasta que exista el comando que lo abre y lo cierra, no hay botón.
    #[test]
    fn un_panel_de_plugin_no_tiene_boton_en_la_barra() {
        let mut reg = KindRegistry::builtin();
        reg.insert_panels(&[panel_de_plugin("git", "status", None, true)]);
        let d = reg
            .get(&KindId::new("plugin:git:status"))
            .expect("está declarado");
        assert!(d.focusable, "se enfoca");
        assert!(
            !crate::panelbar::es_boton(d),
            "y aun así no sale en la barra"
        );
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

    /// El sidebar no opta a ningún rol y no admite dos. Lo primero es lo que
    /// impide que una copia acabe teniendo por destino una lista de discos.
    #[test]
    fn places_no_toma_roles_y_es_unico() {
        let reg = KindRegistry::builtin();
        let d = reg.get(&KindId::new("places")).expect("places está");
        assert_eq!(d.min, (14, 5));
        assert!(d.focusable && d.takes_keys);
        assert!(!d.multi);
        assert!(d.roles.is_empty());
        assert!(!reg.holds_role(&KindId::new("places"), RoleId::Target));
    }

    fn panel_de_plugin(
        id: &str,
        kind: &str,
        min: Option<(u16, u16)>,
        ok: bool,
    ) -> norte_proto::methods::PluginInfo {
        use norte_proto::methods::PluginInfo;
        PluginInfo {
            id: id.to_owned(),
            name: id.to_owned(),
            publisher: String::new(),
            version: "1.0.0".to_owned(),
            category: "panel".to_owned(),
            capabilities: Vec::new(),
            approved: ok,
            enabled: ok,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            panels: vec![norte_proto::methods::PluginPanelInfo {
                kind: kind.to_owned(),
                title: "Git".to_owned(),
                min_cols: min.map(|(c, _)| c),
                min_rows: min.map(|(_, r)| r),
            }],
            has_help: false,
            manifest_digest: None,
        }
    }

    /// Un panel de un plugin SIN consentir no existe para el reparto.
    ///
    /// Mismo criterio que las columnas: el hueco no se coloca y su botón no
    /// sale en la barra hasta que el lector aprueba y activa el plugin.
    #[test]
    fn un_panel_sin_consentir_no_aporta_kind() {
        let mut reg = KindRegistry::builtin();
        reg.insert_panels(&[panel_de_plugin("org.norte.git", "git", None, false)]);
        assert!(
            reg.get(&panel_kind_id("org.norte.git", "git")).is_none(),
            "sin aprobar ni activar, no hay panel"
        );
    }

    /// Consentido, el kind existe, lleva el prefijo que impide colisiones y
    /// toma teclas.
    #[test]
    fn un_panel_consentido_es_un_kind_con_su_prefijo() {
        let mut reg = KindRegistry::builtin();
        reg.insert_panels(&[panel_de_plugin("org.norte.git", "git", None, true)]);
        let decl = reg
            .get(&panel_kind_id("org.norte.git", "git"))
            .expect("el panel está declarado");
        assert_eq!(decl.id.as_str(), "plugin:org.norte.git:git");
        assert!(decl.focusable && decl.takes_keys);
        assert!(!decl.multi, "uno de cada panel, como los laterales de casa");
    }

    /// El tamaño lo decide el MANIFIESTO cuando lo dice, y hay respaldo
    /// cuando calla: un panel sin mínimos declarados no puede quedarse sin
    /// ninguno, o el reparto lo colocaría en dos columnas.
    #[test]
    fn los_minimos_del_manifiesto_mandan_y_hay_respaldo() {
        let mut reg = KindRegistry::builtin();
        reg.insert_panels(&[
            panel_de_plugin("org.norte.git", "git", Some((40, 9)), true),
            panel_de_plugin("org.norte.otro", "x", None, true),
        ]);
        assert_eq!(
            reg.get(&panel_kind_id("org.norte.git", "git"))
                .expect("está")
                .min,
            (40, 9)
        );
        assert_eq!(
            reg.get(&panel_kind_id("org.norte.otro", "x"))
                .expect("está")
                .min,
            (20, 4)
        );
    }

    /// Lo aportado va DETRÁS de lo de serie.
    ///
    /// No es cosmético: la barra de paneles pinta en el orden del registro, y
    /// que los de siempre estén donde siempre es lo que deja aprender la
    /// posición de un botón con el dedo.
    #[test]
    fn lo_aportado_no_se_cuela_delante_de_lo_de_serie() {
        let antes: Vec<String> = KindRegistry::builtin()
            .decls()
            .iter()
            .map(|d| d.id.as_str().to_owned())
            .collect();
        let mut reg = KindRegistry::builtin();
        reg.insert_panels(&[panel_de_plugin("org.norte.git", "git", None, true)]);
        let despues: Vec<String> = reg
            .decls()
            .iter()
            .map(|d| d.id.as_str().to_owned())
            .collect();
        assert_eq!(
            &despues[..antes.len()],
            &antes[..],
            "los de serie, intactos"
        );
        assert_eq!(
            despues.last().map(String::as_str),
            Some("plugin:org.norte.git:git")
        );
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
