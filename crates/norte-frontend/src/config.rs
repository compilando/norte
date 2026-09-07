//! Frontend configuration: the shared scalar merge (`norte-config`) plus the
//! frontend-only passes — `keymap.toml` layers and `openers.toml` (#28).

use std::path::{Path, PathBuf};

use norte_config::schema::read_optional;
use norte_config::{CommonConfig, ConfigError, Layer, Layers, QuickSearch};

use crate::keymap::{KeymapFile, parse_keymap_layer};
use crate::nav;
use crate::openers::OpenersConfig;

/// Everything a frontend needs, flat (same shape the TUI historically used).
#[derive(Debug, Clone)]
pub struct FrontendConfig {
    /// The merged scalars (preset, ui, daemon, hotlist, archive, ai, sources).
    /// `common.sources` is NOT grouped by layer: `norte-config::load` fills
    /// it with every layer's `norte.toml` first, and this module's `load`
    /// then appends each layer's `keymap.toml`/`openers.toml` afterwards —
    /// treat it as a set of files that participated, not an ordered log.
    pub common: CommonConfig,
    /// `keymap.toml` layers present, ascending precedence.
    ///
    /// One entry per layer dir that HAS the file — a dir without one
    /// contributes nothing, so the position of a layer here does NOT say
    /// which layer it is. That is what [`Self::keymap_layer_kinds`] is for;
    /// see it before cutting this list at any index.
    pub keymap_layers: Vec<KeymapFile>,
    /// The [`Layer`] each entry of [`Self::keymap_layers`] came from — same
    /// length, same order, index by index.
    ///
    /// Carried rather than inferred because the two are not recoverable from
    /// each other and a wrong guess is silent: with only the system dir
    /// holding a `keymap.toml`, `keymap_layers` is a one-element list whose
    /// entry is the SYSTEM layer, and with a project layer present the last
    /// entry is `./.norte`. A shortcut editor that cut this list positionally
    /// would model its write into the wrong layer and approve a binding that
    /// never fires — see
    /// [`RebindSources::split_at`](crate::keymap::RebindSources::split_at),
    /// which is the only supported way to make that cut.
    pub keymap_layer_kinds: Vec<Layer>,
    /// El DIRECTORIO del que salió cada entrada de [`Self::keymap_layers`] —
    /// misma longitud, mismo orden, índice a índice que
    /// [`Self::keymap_layer_kinds`].
    ///
    /// Los tres vectores son UNA tabla. Este existe porque el destino de una
    /// escritura de atajo y el directorio donde cae tienen que salir del mismo
    /// sitio: `split_at` dice a qué capa apunta y esto dice dónde vive, así que
    /// el escritor ya no resuelve un directorio por su cuenta. Con un perfil
    /// activo eso mandaba la escritura al fichero del usuario, donde el perfil
    /// la tapaba (#305).
    pub keymap_layer_dirs: Vec<std::path::PathBuf>,
    /// Quick-search mode mapped onto the navigation enum.
    pub quick_search_mode: nav::Mode,
    /// Merged declarative openers (#28): System/User only, fail-closed.
    pub openers: OpenersConfig,
}

/// Loads the `keymap.toml` layer from `dir` (ADR 0006/0007); `None` if it
/// doesn't exist. The PROJECT layer is marked (`mark_project`) so
/// `Effective` discards its `lua:` bindings (security #75). A user layer
/// does not accept the full `keymap` list (that belongs to presets): using
/// it is an error naming the culprit file.
///
/// Kept `pub` for out-of-workspace frontends (e.g. the GUI): they can load
/// keymap layers without going through this module's combined `load`.
///
/// # Errors
/// [`ConfigError::Toml`] if it doesn't parse, or a layer uses `keymap` or
/// `dialog_from` (both are preset-only keys).
pub fn load_keymap_layer(
    dir: &Path,
    kind: Layer,
    sources: &mut Vec<PathBuf>,
) -> Result<Option<KeymapFile>, ConfigError> {
    let keymap = dir.join("keymap.toml");
    let Some(raw) = read_optional(&keymap)? else {
        return Ok(None);
    };
    // `parse_keymap_layer`, not `parse_keymap`: a layer may not inherit a
    // `[dialog]` (ADR 0045), and refusing the key BEFORE resolving it is what
    // makes the error name `dialog_from` instead of the `keymap` list the
    // resolution would have copied in — `has_full_keymap` below reads
    // `dialog.keymap` and would otherwise fire first, on a key the user never
    // wrote.
    let mut parsed = parse_keymap_layer(&raw).map_err(|e| ConfigError::Toml {
        path: keymap.clone(),
        message: e.to_string(),
    })?;
    if kind == Layer::Project {
        parsed.mark_project();
    }
    if parsed.has_full_keymap() {
        return Err(ConfigError::Toml {
            path: keymap,
            message:
                "una capa de config no admite `keymap`: usa prepend_keymap/append_keymap (ADR 0006)"
                    .to_owned(),
        });
    }
    sources.push(keymap);
    Ok(Some(parsed))
}

/// Loads and parses `openers.toml` for a layer (#28); `None` if the file
/// doesn't exist or the layer is PROJECT (fail-closed — a hostile repo's
/// `./.norte/openers.toml` must not be able to launch external binaries).
///
/// Kept `pub` for out-of-workspace frontends (e.g. the GUI): they can load
/// openers without going through this module's combined `load`.
///
/// # Errors
/// [`ConfigError::Toml`] naming the culprit file if it doesn't parse.
pub fn load_openers(
    dir: &Path,
    kind: Layer,
    sources: &mut Vec<PathBuf>,
) -> Result<Option<OpenersConfig>, ConfigError> {
    if kind == Layer::Project {
        return Ok(None);
    }
    let openers_path = dir.join("openers.toml");
    let Some(raw) = read_optional(&openers_path)? else {
        return Ok(None);
    };
    let parsed = OpenersConfig::parse(&raw).map_err(|e| ConfigError::Toml {
        path: openers_path.clone(),
        message: e.to_string(),
    })?;
    sources.push(openers_path);
    Ok(Some(parsed))
}

/// Load and merge every layer (ADR 0007): common scalars + keymap + openers.
///
/// # Errors
/// [`ConfigError`] with the culprit file; an absent layer is not an error.
pub fn load(layers: &Layers) -> Result<FrontendConfig, ConfigError> {
    let mut common = norte_config::load(layers)?;
    let mut keymap_layers = Vec::new();
    let mut keymap_layer_kinds = Vec::new();
    let mut keymap_layer_dirs = Vec::new();
    let mut openers = OpenersConfig::empty();
    for (dir, kind) in &layers.dirs {
        if let Some(parsed) = load_keymap_layer(dir, *kind, &mut common.sources)? {
            keymap_layers.push(parsed);
            // In lockstep with the push above and never apart from it: the
            // two vectors are one table, and a layer whose kind was dropped
            // cannot be recovered by position (a dir with no `keymap.toml`
            // leaves no gap here). Tres desde #305: el directorio también, por
            // lo mismo.
            keymap_layer_kinds.push(*kind);
            keymap_layer_dirs.push(dir.clone());
        }
        if let Some(parsed) = load_openers(dir, *kind, &mut common.sources)? {
            openers.extend_front(parsed);
        }
    }
    let quick_search_mode = match common.quick_search {
        QuickSearch::Filter => nav::Mode::Filter,
        QuickSearch::Jump => nav::Mode::Jump,
    };
    Ok(FrontendConfig {
        common,
        keymap_layers,
        keymap_layer_kinds,
        keymap_layer_dirs,
        quick_search_mode,
        openers,
    })
}

/// [`load`] con un perfil de por medio, con la regla de tres respuestas de D7
/// aplicada a la CAPA ENTERA y no solo a su `norte.toml`.
///
/// **Ésta es la que llama un frontend**, y la de `norte-config` la que llama
/// el core. La diferencia no es de comodidad: aquella decide sobre
/// `norte.toml`, y un perfil trae además `keymap.toml` y `openers.toml`, que
/// son fatales para cualquier capa que no sea de proyecto. Pasando por la de
/// abajo, un perfil con una errata en un atajo se declaraba sano y reventaba
/// después — con `ProfileSource::Sticky` eso es exactamente el desenlace que
/// D7 existe para impedir, porque el lector se queda fuera del programa y sin
/// manera de elegir otro perfil (#305).
///
/// La regla no se duplica: [`norte_config::load_with`] la tiene, y esto le
/// pasa el cargador de este crate.
///
/// # Errors
///
/// [`norte_config::ProfileError`] según la procedencia del nombre, igual que
/// [`norte_config::load_with_profile`].
pub fn load_with_profile(
    layers_for: &impl Fn(Option<&std::ffi::OsStr>) -> Layers,
    name: Option<&std::ffi::OsStr>,
    source: norte_config::ProfileSource,
) -> Result<norte_config::Loaded<FrontendConfig>, norte_config::ProfileError> {
    norte_config::load_with(layers_for, name, source, &load)
}

/// Lo que «guardar como perfil» guarda, montado a partir de lo que se VE
/// (#306 en el terminal, #318 en la ventana).
///
/// Vive aquí, y no una copia en cada frontend, por la lección de la ADR 0077:
/// **una decisión duplicada entre frontends diverge en silencio**. Y este es
/// el peor sitio donde podría divergir — dos «guardar como» que producen
/// perfiles distintos convierten el perfil en algo que depende de por dónde lo
/// guardaste. Con una sola función, la paridad no es un test que haya que
/// acordarse de escribir: es que no hay dos cosas que comparar.
///
/// `dir_de_hueco` contesta dónde está cada listado; un hueco que no lo sea
/// (visor, procesos, sitios) contesta `None` y no entra en `[profile.start]`,
/// que es lo correcto: no tiene directorio que recordar.
///
/// Lo que NO se guarda, y por qué:
///
/// - los escalares de `[ui]`: lo que el lector cambia en marcha —tema,
///   preset— ya se persiste por su propio camino, y copiarlo aquí escribiría
///   dos veces lo mismo con dos verdades posibles;
/// - los favoritos, las conexiones y el resto de secciones. Un perfil es un
///   espacio de TRABAJO, no una copia de la configuración entera: duplicar la
///   hotlist en cada perfil la congelaría, y la del usuario sigue viéndose por
///   debajo.
///
/// El `keymap.toml` sí se copia, BYTE a BYTE y sin reescribirlo: es un fichero
/// del lector, con sus comentarios, y «guardar como» tiene que producir un
/// perfil que se comporte igual que el que tenías.
#[must_use]
pub fn profile_snapshot(
    arbol: &crate::layout::Node,
    dir_de_hueco: &dyn Fn(crate::layout::SlotId) -> Option<norte_proto::VPath>,
    keymap: Option<Vec<u8>>,
) -> norte_config::ProfileSnapshot {
    let start = arbol
        .slot_ids()
        .into_iter()
        .filter_map(|id| {
            let crate::layout::SlotId(n) = id;
            Some((n.to_string(), dir_de_hueco(id)?.to_wire()))
        })
        .collect();
    norte_config::ProfileSnapshot {
        title: None,
        layout_toml: crate::layout::config::to_toml(arbol).ok(),
        ui: Vec::new(),
        start,
        keymap,
    }
}

/// Qué huecos siembra `[profile.start]` al entrar en un perfil.
///
/// **La SESIÓN gana.** `[profile.start]` dice dónde abre un hueco «la primera
/// vez»: en cuanto ese hueco tiene estado guardado, lo que manda es dónde lo
/// dejaste, porque un perfil es un espacio de trabajo y no un marcador que te
/// devuelve al principio cada vez que entras.
///
/// Dos vetos, y hacen falta los dos:
///
/// - `conocidos` son los huecos de los que la sesión GUARDADA sabe algo. Tiene
///   que ser lo leído del disco, no la pantalla de ahora: ésta nombra todos los
///   huecos vivos, así que preguntándole el perfil no sembraría nunca.
/// - `sembrados` son los que este proceso ya sembró. Sin ellos, un lector sin
///   sesión guardada —una instalación nueva— volvería al directorio de arranque
///   del perfil cada vez que entra y sale de él, porque para él la sesión no
///   sabe nunca nada de nada.
///
/// Se devuelven en el orden del mapa —por id de hueco— para que sembrar sea
/// determinista: dos huecos que se siembran en distinto orden acaban con el
/// mismo contenido pero con el foco en sitios distintos.
///
/// Vive en este crate porque los dos frontends contestan la misma pregunta, y
/// esa es exactamente la clase de decisión que escrita dos veces diverge
/// (ADR 0077). No hace I/O: decide, y quien llame lista.
///
/// ```
/// use std::collections::{BTreeMap, BTreeSet};
/// use norte_proto::VPath;
/// use norte_frontend::config::profile_start_seeds;
///
/// let mut start = BTreeMap::new();
/// start.insert(1, VPath::parse("file:///src").unwrap());
/// start.insert(2, VPath::parse("file:///tmp").unwrap());
/// let nada = BTreeSet::new();
///
/// // Sin sesión y sin haber sembrado, van los dos.
/// assert_eq!(profile_start_seeds(&start, &nada, &nada).len(), 2);
///
/// // El hueco 1 lo conoce la sesión: ese lo manda ella.
/// let conocidos = BTreeSet::from([1]);
/// let siembra = profile_start_seeds(&start, &conocidos, &nada);
/// assert_eq!(siembra.len(), 1);
/// assert_eq!(siembra[0].0, 2);
///
/// // Y lo ya sembrado no se vuelve a sembrar: entrar y salir del perfil no
/// // te saca de donde estabas.
/// let sembrados = BTreeSet::from([2]);
/// assert!(profile_start_seeds(&start, &conocidos, &sembrados).is_empty());
/// ```
#[must_use]
pub fn profile_start_seeds(
    start: &std::collections::BTreeMap<u32, norte_proto::VPath>,
    conocidos: &std::collections::BTreeSet<u32>,
    sembrados: &std::collections::BTreeSet<u32>,
) -> Vec<(u32, norte_proto::VPath)> {
    start
        .iter()
        .filter(|(id, _)| !conocidos.contains(id) && !sembrados.contains(id))
        .map(|(id, v)| (*id, v.clone()))
        .collect()
}

/// Los huecos que `[profile.start]` nombra y esta DISPOSICIÓN no coloca.
///
/// No tienen dónde abrir, así que se caen — y eso hay que decirlo. Es la misma
/// clase de silencio que la clave entera tenía antes de ADR 0098: se escribe
/// algo en el fichero del perfil y no pasa nada, sin que nada explique por
/// qué. Ocurre editando a mano o cambiando la disposición del perfil sin
/// reguardarlo; `save_profile` siempre escribe ids que su propia disposición
/// coloca.
///
/// Devuelve los ids EN ORDEN, para que el mensaje sea el mismo en las dos
/// superficies.
///
/// ```
/// use std::collections::{BTreeMap, BTreeSet};
/// use norte_proto::VPath;
/// use norte_frontend::config::profile_start_huerfanos;
///
/// let mut start = BTreeMap::new();
/// start.insert(1, VPath::parse("file:///src").unwrap());
/// start.insert(9, VPath::parse("file:///tmp").unwrap());
/// let colocados = BTreeSet::from([1, 2]);
/// assert_eq!(profile_start_huerfanos(&start, &colocados), vec![9]);
/// ```
#[must_use]
pub fn profile_start_huerfanos(
    start: &std::collections::BTreeMap<u32, norte_proto::VPath>,
    colocados: &std::collections::BTreeSet<u32>,
) -> Vec<u32> {
    start
        .keys()
        .filter(|id| !colocados.contains(id))
        .copied()
        .collect()
}

/// Lee todos los perfiles de `<dir>/profiles/`, con su título y su motivo si
/// no cargan.
///
/// **Bloquea**: lista un directorio y abre un fichero por perfil. Quien la
/// llame desde un bucle de eventos pasa por `spawn_blocking` (regla 2), y
/// #244 es por qué.
///
/// Un perfil que no parsea NO desaparece: vuelve con su `problem` puesto, para
/// que el selector lo enseñe roto en vez de esconder un directorio que el
/// lector creó.
///
/// Vive en este módulo y no en [`crate::profile_picker`] porque abre
/// ficheros, y aquel es puro por contrato. Y vive en este CRATE y no en un
/// frontend porque los dos lo necesitan igual: la ventana y el terminal
/// enseñan la misma lista, y dos lectores del mismo directorio acaban
/// discrepando en qué es un perfil roto.
#[must_use]
pub fn read_profiles(dir: &Path) -> Vec<crate::profile_picker::UserProfile> {
    let raiz = dir.join("profiles");
    norte_config::list_profiles(&raiz)
        .unwrap_or_default()
        .into_iter()
        .map(|name| {
            let toml = raiz.join(&name).join("norte.toml");
            let (title, problem) = match std::fs::read_to_string(&toml) {
                // Un perfil sin `norte.toml` es legítimo: puede traer solo su
                // `layouts/` o su `keymap.toml`.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
                Err(e) => (None, Some(e.kind().to_string())),
                Ok(raw) => match toml::from_str::<norte_config::NorteToml>(&raw) {
                    Ok(p) => (p.profile.title, None),
                    // El diagnóstico NO cita el contenido del fichero: la
                    // barra de mensajes tiene un tope y una config puede
                    // llevar rutas (#73).
                    Err(e) => (None, Some(e.message().to_owned())),
                },
            };
            crate::profile_picker::UserProfile {
                name,
                title,
                problem,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use norte_config::{Layer, Layers};

    use super::*;

    /// **Solo entra en `[profile.start]` lo que ES un listado.**
    ///
    /// Un hueco de visor, de procesos o de sitios no tiene directorio que
    /// recordar, y meterlo con el del panel de al lado escribiría un perfil
    /// que al abrirse manda un visor a un directorio.
    #[test]
    fn el_start_solo_lleva_los_huecos_que_son_listado() {
        use crate::layout::SlotId;
        use crate::layout::{Dir, KindId, Node};
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        // El hueco 2 no contesta: no es un listado.
        let snap = profile_snapshot(
            &arbol,
            &|SlotId(n)| (n == 1).then(|| norte_proto::VPath::parse("mem:///uno").unwrap()),
            None,
        );
        assert_eq!(snap.start, [("1".to_owned(), "mem:///uno".to_owned())]);
        assert!(snap.layout_toml.is_some(), "la disposición sí va entera");
        assert!(snap.ui.is_empty(), "los escalares de [ui] no se copian");
        assert!(snap.keymap.is_none());
    }

    /// El `keymap.toml` viaja BYTE a BYTE: es un fichero del lector, con sus
    /// comentarios, y reescribirlo le cambiaría el suyo.
    #[test]
    fn el_keymap_se_copia_tal_cual() {
        use crate::layout::SlotId;
        use crate::layout::{KindId, Node};
        let arbol = Node::slot(SlotId(1), KindId::browser());
        let crudo = b"# mio\n[pane]\nkeymap = []\n\xff".to_vec();
        let snap = profile_snapshot(&arbol, &|SlotId(_)| None, Some(crudo.clone()));
        assert_eq!(snap.keymap.as_deref(), Some(crudo.as_slice()));
    }

    /// Un árbol con capa de usuario y un perfil `work` cuyo contenido se da.
    fn arbol_con_perfil(
        ficheros: &[(&str, &str)],
    ) -> (
        impl Fn(Option<&std::ffi::OsStr>) -> Layers + use<>,
        tempfile::TempDir,
    ) {
        let usuario = tempfile::tempdir().expect("tempdir");
        let dir = usuario.path().join("profiles").join("work");
        std::fs::create_dir_all(&dir).expect("mkdir");
        for (nombre, contenido) in ficheros {
            std::fs::write(dir.join(nombre), contenido).expect("write");
        }
        let raiz = usuario.path().to_path_buf();
        let f = move |n: Option<&std::ffi::OsStr>| Layers {
            dirs: match n {
                None => vec![(raiz.clone(), Layer::User)],
                Some(n) => vec![
                    (raiz.clone(), Layer::User),
                    (raiz.join("profiles").join(n), Layer::Profile),
                ],
            },
        };
        (f, usuario)
    }

    /// D7 es una regla sobre la CAPA, no sobre `norte.toml`.
    ///
    /// Un `keymap.toml` roto en el perfil PEGAJOSO tiene que degradar igual:
    /// `load_keymap_layer` es fatal para toda capa que no sea de proyecto, así
    /// que pasando solo por el cargador de `norte.toml` un perfil con una
    /// errata en un atajo se declaraba sano y reventaba después — dejando al
    /// lector fuera del programa y sin manera de elegir otro, que es justo lo
    /// que D7 existe para impedir (#305).
    #[test]
    fn un_keymap_roto_en_el_perfil_pegajoso_degrada() {
        // `keymap` (la lista ENTERA) es clave solo de preset: en una capa es
        // un error que nombra el fichero culpable.
        let (layers_for, _g) = arbol_con_perfil(&[
            ("norte.toml", "[ui]\ntheme = \"nord\"\n"),
            (
                "keymap.toml",
                "[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n",
            ),
        ]);
        let work = std::ffi::OsStr::new("work");

        let r = load_with_profile(&layers_for, Some(work), norte_config::ProfileSource::Sticky)
            .expect("arranca igual");
        assert_eq!(r.active, None, "sin capa de perfil");
        assert!(r.degraded.is_some(), "y no en silencio");

        assert!(
            load_with_profile(
                &layers_for,
                Some(work),
                norte_config::ProfileSource::Explicit
            )
            .is_err(),
            "con --profile es fatal: el lector nombró ese perfil"
        );
    }

    /// Lo mismo con `openers.toml`, que es la otra mitad de la capa y también
    /// es fatal fuera de proyecto.
    #[test]
    fn un_openers_roto_en_el_perfil_pegajoso_degrada() {
        let (layers_for, _g) = arbol_con_perfil(&[("openers.toml", "[[opener]]\nmime = 3\n")]);
        let r = load_with_profile(
            &layers_for,
            Some(std::ffi::OsStr::new("work")),
            norte_config::ProfileSource::Sticky,
        )
        .expect("arranca igual");
        assert_eq!(r.active, None);
        assert!(r.degraded.is_some());
    }

    /// El camino feliz trae el keymap DEL PERFIL, y su kind viaja para que
    /// `split_at` pueda cortar por él (D10).
    #[test]
    fn un_perfil_sano_aporta_su_capa_de_keymap() {
        let (layers_for, _g) = arbol_con_perfil(&[(
            "keymap.toml",
            "[pane]\nprepend_keymap = [{ on = [\"f5\"], run = \"pane.move\" }]\n",
        )]);
        let r = load_with_profile(
            &layers_for,
            Some(std::ffi::OsStr::new("work")),
            norte_config::ProfileSource::Explicit,
        )
        .expect("carga");
        assert_eq!(r.active.as_deref(), Some(std::ffi::OsStr::new("work")));
        assert_eq!(r.config.keymap_layer_kinds, vec![Layer::Profile]);
    }

    /// #28 seguridad: un `openers.toml` en la capa de PROYECTO (`./.norte`) se
    /// IGNORA fail-closed — un repo hostil no puede inyectar un binario que se
    /// ejecute al pulsar F4. La capa de USUARIO sí se honra.
    #[test]
    fn openers_de_proyecto_se_ignoran_usuario_se_honra() {
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
        )
        .unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(
            proyecto.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"curl-malicioso\", \"%f\"]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        // Resuelve al opener del USUARIO, jamás al del proyecto.
        assert_eq!(
            cfg.openers
                .resolve_for("text/plain", "linux")
                .unwrap()
                .program(),
            "bat",
            "el opener de proyecto se ignora fail-closed"
        );
    }

    /// K3c c1, from the other side: what `norte_config::persist_keymap_bind`
    /// writes LOADS — through the real loader — and the binding it wrote
    /// RESOLVES. This is the pin for the whole point of that writer: it lives
    /// in `norte-config`, which is below the keymap grammar and cannot call
    /// `parse_keymap_layer`/`check_layer_keys`, so a section name or a list
    /// key that drifted there would only show up as a user's entire keymap
    /// silently reverting on the next reload.
    #[test]
    fn un_binding_persistido_carga_y_resuelve() {
        use crate::keymap::{Effective, Screen, parse_chord, parse_keymap};

        let dir = tempfile::tempdir().unwrap();
        for section in ["global", "pane", "viewer", "dialog"] {
            norte_config::persist_keymap_bind(
                dir.path(),
                section,
                norte_config::KeymapList::Prepend,
                &["ctrl+g".to_owned()],
                "cursor.top",
            )
            .expect("persist");
        }
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("lo escrito por el persistidor CARGA");
        assert_eq!(cfg.keymap_layers.len(), 1);
        // Un preset mínimo: el pin es sobre la CAPA, no sobre un preset
        // concreto, y así `ctrl+g` no puede chocar con lo que el preset del
        // día bindee.
        let preset =
            parse_keymap("[pane]\nkeymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n").unwrap();
        // `build_for` es quien corre `check_layer_keys`: si el escritor
        // hubiese producido `keymap` (o `counts`, o `dialog_from`) esto sería
        // `Err` y la config del usuario se habría revertido entera.
        let eff = Effective::build_for(
            &preset,
            &cfg.keymap_layers,
            // El set del frontend: sin él un comando del catálogo resuelve
            // `NotHere` (no lo sirve ESTA pantalla) y `single_chord_runs`
            // diría `false` por una razón que no es la que se prueba.
            &["cursor.top", "cursor.down"],
            Screen::Browse,
        )
        .expect("la capa escrita es una capa legal");
        assert!(
            eff.single_chord_runs(parse_chord("ctrl+g").unwrap(), "cursor.top"),
            "el binding persistido resuelve"
        );
    }

    /// K3c c1, la razón de que `persist_keymap_bind` lleve `KeymapList`: sobre
    /// una tecla que el PRESET ya bindea EN EL MISMO contexto, solo un
    /// `prepend_keymap` gana. Un `append_keymap` parsea, carga, valida — y no
    /// dispara nunca, porque el orden de fusión es prepends → preset →
    /// appends y gana el PRIMERO. Escrito como test y no como comentario
    /// porque es exactamente el fallo que un editor de atajos comete callando:
    /// "guardado", y la tecla sigue haciendo lo de antes.
    #[test]
    fn solo_un_prepend_pisa_al_preset_en_su_propio_contexto() {
        use crate::keymap::{Effective, Screen, parse_chord, parse_keymap};

        let preset =
            parse_keymap("[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n").unwrap();
        let efectivo = |list| {
            let dir = tempfile::tempdir().unwrap();
            norte_config::persist_keymap_bind(
                dir.path(),
                "pane",
                list,
                &["f5".to_owned()],
                "pane.move",
            )
            .expect("persist");
            let layers = Layers {
                dirs: vec![(dir.path().to_path_buf(), Layer::User)],
            };
            let cfg = load(&layers).expect("carga");
            Effective::build_for(
                &preset,
                &cfg.keymap_layers,
                &["pane.copy", "pane.move"],
                Screen::Browse,
            )
            .expect("capa legal")
        };
        let f5 = parse_chord("f5").unwrap();
        assert!(
            efectivo(norte_config::KeymapList::Prepend).single_chord_runs(f5, "pane.move"),
            "un prepend pisa al preset: es lo que un rebind necesita"
        );
        assert!(
            efectivo(norte_config::KeymapList::Append).single_chord_runs(f5, "pane.copy"),
            "un append NO pisa al preset — el binding se escribe y no hace nada"
        );
    }

    /// `dialog_from` es clave de PRESET (ADR 0045). Una capa que la use tiene
    /// que enterarse por su nombre: si la capa se parsease con `parse_keymap`,
    /// la herencia se resolvería ANTES del chequeo de `has_full_keymap`, que
    /// mira `dialog.keymap` — y el usuario recibiría un error sobre `keymap`,
    /// una clave que no escribió. Este test es la única red que hay a la
    /// altura del cargador; `check_layer_keys` se prueba aparte y no ve esto.
    #[test]
    fn una_capa_con_dialog_from_falla_nombrando_dialog_from() {
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("keymap.toml"),
            "dialog_from = \"orthodox\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(usuario.path().to_path_buf(), Layer::User)],
        };
        let e = load(&layers).expect_err("una capa no puede heredar [dialog]");
        let msg = e.to_string();
        assert!(msg.contains("dialog_from"), "{msg}");
        assert!(
            !msg.contains("prepend_keymap"),
            "el diagnóstico habla de la clave equivocada: {msg}"
        );
    }

    /// K3c c2: `keymap_layers` carries one entry per dir that HAS the file, so
    /// its INDICES say nothing about which layer is which — here the user dir
    /// has no `keymap.toml` and the list is `[system, project]`, with the
    /// system layer sitting at index 0 where a positional guess would look for
    /// the user's. `keymap_layer_kinds` is the answer, parallel index by
    /// index; without it a shortcut editor cutting this list would model its
    /// write into the system layer (see `RebindSources::split_at`).
    #[test]
    fn keymap_layer_kinds_va_en_paralelo_a_las_capas_presentes() {
        let sistema = tempfile::tempdir().unwrap();
        std::fs::write(
            sistema.path().join("keymap.toml"),
            "[pane]\nprepend_keymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
        )
        .unwrap();
        // El usuario todavía no tiene fichero: el primer rebind de una
        // instalación nueva.
        let usuario = tempfile::tempdir().unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(
            proyecto.path().join("keymap.toml"),
            "[pane]\nprepend_keymap = [{ on = [\"k\"], run = \"cursor.up\" }]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (sistema.path().to_path_buf(), Layer::System),
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.keymap_layers.len(), 2, "el usuario no aporta fichero");
        assert_eq!(
            cfg.keymap_layer_kinds,
            vec![Layer::System, Layer::Project],
            "el kind viaja con la capa, no con el índice"
        );
        assert!(
            cfg.keymap_layers[1].is_project(),
            "y la de proyecto sigue marcada"
        );
    }

    /// #28: entre capas, la superior (usuario) gana el empate de mimetype.
    #[test]
    fn openers_usuario_gana_sobre_sistema() {
        let sistema = tempfile::tempdir().unwrap();
        std::fs::write(
            sistema.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"less\", \"%f\"]\n",
        )
        .unwrap();
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (sistema.path().to_path_buf(), Layer::System),
                (usuario.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(
            cfg.openers
                .resolve_for("text/plain", "linux")
                .unwrap()
                .program(),
            "bat",
            "la capa de usuario (superior) gana"
        );
    }

    /// The combined loader wires all three passes: scalars, keymap layers,
    /// openers — and maps `quick_search` onto `nav::Mode`.
    #[test]
    fn load_combina_escalares_keymap_y_openers() {
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[ui]\nquick_search = \"jump\"\n[keymap]\npreset = \"vim\"\n",
        )
        .unwrap();
        std::fs::write(
            usuario.path().join("keymap.toml"),
            "[pane]\nprepend_keymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
        )
        .unwrap();
        std::fs::write(
            usuario.path().join("openers.toml"),
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(usuario.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.common.preset, "vim");
        assert_eq!(cfg.quick_search_mode, nav::Mode::Jump);
        assert_eq!(cfg.keymap_layers.len(), 1);
        assert!(cfg.openers.resolve_for("text/plain", "linux").is_some());
        assert_eq!(
            cfg.common.sources.len(),
            3,
            "norte.toml + keymap.toml + openers.toml"
        );
    }
}
