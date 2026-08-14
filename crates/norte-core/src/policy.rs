//! Policy engine (M3-3, spec §10): cada mutación de un AGENTE se evalúa contra
//! la frontera de su SCOPE (rutas + ops + TTL) y las reglas de `policy.toml`,
//! dando `Allow | Ask | Deny` ANTES de ejecutarse. Los humanos (`User`) no se
//! sandboxean. Enforcement, no prompt-engineering (regla 9).
//!
//! **Requisitos de seguridad para M3-3b/M3-4 (deuda anotada):**
//! - El daemon DEBE instalar `ScopedPolicy` vía [`crate::Engine::with_policy`];
//!   el default [`AllowAll`] es solo para el engine embebido/humano. Con agentes,
//!   olvidar el wiring = sin sandbox (fail-open estructural, security M3).
//! - El actor lo fija el CORE por conexión autenticada, jamás se lee de params
//!   del wire: un cliente agéntico no debe poder declararse `User` (que
//!   cortocircuita a `Allow`).
//! - [`ScopeRegistry`] hoy clava por id string; namespacear por `(actor_kind,
//!   id)` para que un Plugin no herede el scope de un Agent homónimo (m4) y
//!   añadir `revoke(session)` para respuesta a incidentes (m7). Relacionado
//!   (#66): la sesión la RECLAMA el cliente en su `initialize` — un agente
//!   hostil-cooperante puede declarar la sesión de OTRO agente y heredar sus
//!   scopes, ver/cancelar sus tasks y recibir su `task.progress`. Aceptado
//!   bajo el threat model same-uid (§14, guardarraíl no sandbox); si algún
//!   día hace falta aislar agente-de-agente, la sesión necesita prueba de
//!   posesión (token al crearla), no solo nombre.
//! - El resolver de `Ask` (M3-3b) DEBE aplicar TTL/timeout y ser cancelable: el
//!   gate suspende la llamada del engine fuera del framework de Task (m5).
//! - Escape de scope vía symlink dentro→fuera que el provider siga (m6): la
//!   mitigación es la política de symlinks del provider (ADR 0005) + fixture
//!   hostil; el gate razona sobre `VPath`s lógicos.

use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use norte_proto::methods::RelPath;
use norte_proto::{DeleteMode, VPath};

use crate::journal::Actor;

pub use config::{PolicyConfig, PolicyConfigError, Rule, RuleAction};

mod config;

/// Operación evaluable (más detalle que `TaskKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyOp {
    /// Copia.
    Copy,
    /// Movimiento.
    Move,
    /// Borrado (permanente o a papelera).
    Delete {
        /// Permanente vs papelera.
        mode: DeleteMode,
    },
    /// Creación de directorio.
    Mkdir,
}

impl PolicyOp {
    /// Etiqueta estable para reglas/scope (`"copy"|"move"|"delete"|"mkdir"`).
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            PolicyOp::Copy => "copy",
            PolicyOp::Move => "move",
            PolicyOp::Delete { .. } => "delete",
            PolicyOp::Mkdir => "mkdir",
        }
    }
}

/// Conjunto de op-kinds que un scope concede.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpSet {
    kinds: BTreeSet<&'static str>,
}

impl OpSet {
    /// Todas las operaciones.
    #[must_use]
    pub fn all() -> Self {
        Self {
            kinds: ["copy", "move", "delete", "mkdir"].into_iter().collect(),
        }
    }
    /// Con un conjunto explícito de op-kinds.
    #[must_use]
    pub fn of(kinds: &[&'static str]) -> Self {
        Self {
            kinds: kinds.iter().copied().collect(),
        }
    }
    /// Desde nombres en RUNTIME (wire): conserva cada nombre que sea un op-kind
    /// canónico y DESCARTA los desconocidos (fail-closed — un op-kind que no
    /// reconocemos no se concede jamás). La fuente de verdad de los kinds
    /// válidos es [`Self::all`]: añadir una op ahí la hace concedible por wire
    /// sin tocar este método. Usado por el daemon al conceder un scope pedido.
    #[must_use]
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> Self {
        let all = Self::all();
        let kinds = names
            .iter()
            .filter_map(|n| all.kinds.iter().copied().find(|k| *k == n.as_ref()))
            .collect();
        Self { kinds }
    }
    /// `true` si `op` está concedida.
    #[must_use]
    pub fn allows(&self, op: PolicyOp) -> bool {
        self.kinds.contains(op.kind())
    }
}

/// Un scope concedido a una sesión de agente: contención por subtree-prefix.
#[derive(Debug, Clone)]
pub struct Scope {
    /// Raíces permitidas (contención por prefijo de segmentos).
    pub roots: Vec<VPath>,
    /// Op-kinds concedidos.
    pub ops: OpSet,
    /// Expiración (TTL). `None` = sin expiración (tests / grants permanentes).
    pub expires_at: Option<Instant>,
}

impl Scope {
    /// Scope sin expiración (para tests / grants permanentes).
    #[must_use]
    pub fn forever(roots: Vec<VPath>, ops: OpSet) -> Self {
        Self {
            roots,
            ops,
            expires_at: None,
        }
    }
    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|e| now >= e)
    }
}

/// `true` si `path` está bajo `root` (mismo scheme+authority y los segmentos de
/// `root` son PREFIJO de los de `path`). Delegado en [`RelPath::under`]
/// (`norte-proto`), que hace la misma comparación byte-exacta por segmentos
/// (regla dura 1) — es la única implementación desde #172. La raíz misma
/// cuenta como contenida: un scope sobre `/a` cubre `/a`.
#[must_use]
pub fn is_under(root: &VPath, path: &VPath) -> bool {
    RelPath::under(root, path).is_some()
}

/// El `VPath` `file://` de un directorio NATIVO **absoluto**, la inversa de lo
/// que hace `norte-vfs-local` al resolver un `VPath` a un path del OS: los
/// componentes se copian a segmentos POR BYTES (regla dura 1 — un nombre que
/// no es UTF-8 sobrevive), y el prefijo de Windows (`C:`, `\\server\share`)
/// es el primer segmento, tal como espera la raíz del provider del OS.
///
/// `None` si el path es RELATIVO o lleva un componente que no es un nombre
/// (`.`/`..`): una raíz que no se puede nombrar no es una raíz, y devolver una
/// ruta que no resuelve sería peor que no devolver ninguna.
///
/// ```
/// use norte_core::policy::local_root_vpath;
/// use std::path::Path;
///
/// # #[cfg(unix)] {
/// let r = local_root_vpath(Path::new("/home/u/.config/norte")).expect("absoluto");
/// assert_eq!(r.to_wire(), "file:///home/u/.config/norte");
/// assert!(local_root_vpath(Path::new("relativo/norte")).is_none());
/// # }
/// ```
#[must_use]
pub fn local_root_vpath(dir: &Path) -> Option<VPath> {
    use norte_proto::{Scheme, Segment};

    if !dir.is_absolute() {
        return None;
    }
    let mut out = VPath::root(Scheme::new("file").ok()?, None);
    for comp in dir.components() {
        let bytes = match comp {
            // La raíz unix no aporta segmento; el prefijo de Windows SÍ (es el
            // primer segmento del `VPath` de la raíz del OS).
            Component::RootDir => continue,
            Component::Prefix(p) => norte_vfs::wtf8::os_to_bytes(p.as_os_str()),
            Component::Normal(os) => norte_vfs::wtf8::os_to_bytes(os),
            Component::CurDir | Component::ParentDir => return None,
        };
        out = out.join(Segment::new(bytes).ok()?);
    }
    Some(out)
}

/// Los subárboles que el WALK de una lectura recursiva no debe mirar para
/// este actor: vacío para el humano (que no se sandboxea) y la raíz protegida
/// de este proceso para un agente o un plugin.
///
/// Existe porque el gate de lectura mira la RAÍZ de la petición y nada más:
/// una búsqueda sobre `$HOME` es legítima y arrastraría el directorio de
/// estado con ella (#165). El gate dice si puedes empezar; esto dice por dónde
/// no se baja.
#[must_use]
pub fn walk_exclusions(actor: &Actor) -> Vec<VPath> {
    match actor {
        Actor::User => Vec::new(),
        Actor::Agent { .. } | Actor::Plugin { .. } => daemon_state_root().into_iter().collect(),
    }
}

/// La raíz PROTEGIDA de este proceso: el directorio de estado del daemon
/// (`journal.db`, los spools de `sync.plan`, `index.db`, `secrets.age`,
/// `connections.toml`, `policy.toml` — el mismo directorio, ver
/// [`crate::connect::config_dir`]), como `VPath` `file://`.
///
/// `None` si el directorio resuelto no es absoluto — el fallback
/// `./.config/norte` de un proceso sin `HOME` ni entrada de passwd. Ahí no hay
/// raíz que proteger porque tampoco hay una ruta estable que un scope pudiese
/// alcanzar, y quien construye el registro se queda sin la protección: se dice
/// aquí porque un `None` silencioso en un gate de seguridad es exactamente la
/// clase de cosa que nadie mira.
#[must_use]
pub fn daemon_state_root() -> Option<VPath> {
    local_root_vpath(&crate::connect::config_dir())
}

/// Resultado de la comprobación de frontera de scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeVerdict {
    /// Dentro de un scope vivo con la op concedida.
    Within,
    /// Fuera de todo scope.
    OutOfScope,
    /// Solo había scopes expirados aplicables.
    Expired,
}

/// Grants de scope por sesión de agente (en memoria; TTL). Thread-safe.
///
/// Lleva además las **raíces protegidas** (#165): subárboles que NINGÚN grant
/// alcanza, se conceda lo que se conceda. La única que existe hoy es el
/// directorio de estado del daemon, y la pone [`Self::new`] sin que nadie
/// tenga que acordarse — ver [`Self::protected_roots`].
#[derive(Clone)]
pub struct ScopeRegistry {
    inner: Arc<Mutex<HashMap<String, Vec<Scope>>>>,
    /// Raíces que ningún scope alcanza. Se fija al construir y no cambia: una
    /// exclusión que se puede quitar en caliente no es una exclusión.
    protected: Arc<Vec<VPath>>,
}

impl Default for ScopeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeRegistry {
    /// Registro vacío, con el directorio de estado del daemon YA protegido
    /// ([`daemon_state_root`]).
    ///
    /// Protege por DEFECTO a propósito: la exclusión existe justo porque se
    /// concede sin querer —un scope sobre `$HOME` contiene
    /// `$HOME/.config/norte` en el layout por defecto—, así que un registro
    /// que hubiera que recordar proteger habría fallado exactamente en el
    /// caso que motiva el issue. Para un registro con OTRAS raíces (tests,
    /// embebedores) está [`Self::with_protected_roots`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_protected_roots(daemon_state_root().into_iter().collect())
    }
    /// Registro vacío con las raíces protegidas EXPLÍCITAS (no consulta el
    /// entorno). Para tests y para un embebedor que ancla su estado en otro
    /// sitio.
    #[must_use]
    pub fn with_protected_roots(roots: Vec<VPath>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            protected: Arc::new(roots),
        }
    }
    /// Las raíces protegidas de este registro.
    ///
    /// Un path AT-OR-UNDER una de ellas responde
    /// [`ScopeVerdict::OutOfScope`] en [`Self::permits`],
    /// [`Self::covers_read`] y [`Self::covers_content`] —las tres puertas por
    /// las que pasan las mutaciones y las lecturas de un agente— ANTES de
    /// mirar ningún grant. Los ANCESTROS no se tocan: `fs.list` de `$HOME`
    /// sigue funcionando y enseña el nombre del directorio de estado, que es
    /// lo mismo que enseña `ls`; lo que no se puede es entrar.
    ///
    /// El veredicto es `OutOfScope` y no una categoría propia porque la causa
    /// gruesa que viaja por el wire (`PolicyDenied.rule`) es un vocabulario
    /// CERRADO: el path no está dentro de ningún scope alcanzable, que es
    /// literalmente lo que dice `out-of-scope`. Añadir una categoría sería un
    /// cambio de wire para decir lo mismo con más detalle del que un agente
    /// denegado debería recibir.
    ///
    /// LO QUE NO CUBRE, dicho aquí porque un gate a medias se lee como
    /// completo: la protección es sobre el `VPath` LÓGICO. Una ruta distinta
    /// que resuelve al mismo directorio —un symlink desde dentro del scope—
    /// la esquiva, y esa es la familia de #164 (`RESOLVE_BENEATH`), no algo
    /// que este registro pueda decidir.
    #[must_use]
    pub fn protected_roots(&self) -> &[VPath] {
        &self.protected
    }
    /// `true` si `path` cae en una raíz protegida (la raíz misma incluida).
    #[must_use]
    fn is_protected(&self, path: &VPath) -> bool {
        self.protected.iter().any(|r| is_under(r, path))
    }
    /// Concede `scope` a la sesión `session`.
    ///
    /// # Panics
    /// Solo si el lock interno queda envenenado.
    pub fn grant(&self, session: &str, scope: Scope) {
        let now = Instant::now();
        let mut map = self.inner.lock().expect("scope registry lock");
        if scope.roots.iter().any(|r| {
            self.protected
                .iter()
                .any(|p| is_under(p, r) || is_under(r, p))
        }) {
            // El grant se acepta y se recorta al consultarlo: avisarlo aquí es
            // lo único que evita que un operador crea que concedió el
            // directorio de estado y se pregunte por qué el agente falla.
            tracing::warn!(
                session,
                "el scope concedido toca una raíz protegida: ahí no se concede nada"
            );
        }
        let entry = map.entry(session.to_owned()).or_default();
        // Poda perezosa: al conceder, descarta los scopes ya expirados de esta
        // sesión para que el `Vec` no crezca monótono en un daemon longevo. La
        // recolección de claves de sesiones muertas es deuda m7 (revoke).
        entry.retain(|s| !s.is_expired(now));
        entry.push(scope);
    }
    /// Veredicto de frontera para `(session, op, path)` a instante `now`.
    ///
    /// # Panics
    /// Solo si el lock interno queda envenenado.
    #[must_use]
    pub fn permits(&self, session: &str, op: PolicyOp, path: &VPath, now: Instant) -> ScopeVerdict {
        // ANTES de mirar un solo grant (#165): lo protegido no se concede.
        if self.is_protected(path) {
            return ScopeVerdict::OutOfScope;
        }
        let map = self.inner.lock().expect("scope registry lock");
        let Some(scopes) = map.get(session) else {
            return ScopeVerdict::OutOfScope;
        };
        let mut saw_expired = false;
        for s in scopes {
            if s.is_expired(now) {
                saw_expired = true;
                continue;
            }
            if s.ops.allows(op) && s.roots.iter().any(|r| is_under(r, path)) {
                return ScopeVerdict::Within;
            }
        }
        if saw_expired {
            ScopeVerdict::Expired
        } else {
            ScopeVerdict::OutOfScope
        }
    }

    /// Membresía de RAÍZ para una lectura recursiva (`fs.search`), INDEPENDIENTE
    /// de la op: `Within` si ALGÚN scope VIVO de la sesión tiene un root con
    /// `is_under(root, path)`. A diferencia de [`Self::permits`], no consulta el
    /// [`OpSet`]: la búsqueda es lectura y no mapea a un [`PolicyOp`] concreto —
    /// el criterio es la simple contención en la frontera concedida. `Expired`
    /// si SOLO scopes ya vencidos habrían cubierto la raíz (un expirado que ni
    /// la cubre no produce `Expired`); `OutOfScope` si ninguno la cubre.
    ///
    /// Es el gate de lectura de TODOS los reads de agente (fs.search T4 +
    /// fs.list/read/stat/capabilities + plugin.preview, #80): un `Agent` solo
    /// lee bajo un scope concedido de su sesión; un `User` no se sandboxea. La
    /// fuente única que los consulta es `daemon::read_gate`.
    ///
    /// CAVEAT ACEPTADO (op-independiente, decisión oscar en
    /// `2026-07-18-gate-lectura-agentes-design.md` §1): al ignorar el
    /// [`OpSet`], un scope de SOLO `delete`/`mkdir` bajo `/tmp` concede lectura
    /// de `/tmp`. Se aceptó a sabiendas: leer es estrictamente menos que
    /// cualquier mutación y `copy`/`move`/`delete` YA implican leer; el residual
    /// `delete`/`mkdir`-sin-lectura es raro. Least-privilege puro (write-only-
    /// no-read) exigiría un `PolicyOp::Read` — fuera de alcance de #80.
    ///
    /// # Panics
    /// Solo si el lock interno queda envenenado.
    #[must_use]
    pub fn covers_read(&self, session: &str, path: &VPath, now: Instant) -> ScopeVerdict {
        if self.is_protected(path) {
            return ScopeVerdict::OutOfScope;
        }
        let map = self.inner.lock().expect("scope registry lock");
        let Some(scopes) = map.get(session) else {
            return ScopeVerdict::OutOfScope;
        };
        let mut saw_expired = false;
        for s in scopes {
            // Solo cuenta un scope que REALMENTE cubre la raíz; la op se ignora.
            if !s.roots.iter().any(|r| is_under(r, path)) {
                continue;
            }
            if s.is_expired(now) {
                saw_expired = true;
            } else {
                return ScopeVerdict::Within;
            }
        }
        if saw_expired {
            ScopeVerdict::Expired
        } else {
            ScopeVerdict::OutOfScope
        }
    }

    /// Membresía de raíz para una lectura de CONTENIDO amplificada
    /// (`fs.compare` con el rung de hash, C6): como [`Self::covers_read`],
    /// pero exigiendo además que el scope conceda una op que maneje BYTES —
    /// hoy `copy` o `move`, las dos que no pueden ejecutarse sin leer el
    /// contenido del origen.
    ///
    /// Por qué existe una segunda puerta: el rung de hash es un **oráculo de
    /// igualdad sobre contenido**. Contesta «¿son iguales estos dos ficheros?»
    /// sin devolver un byte, y lo que lo hace peligroso no es leer, es poder
    /// PONER el candidato: quien coloca su conjetura en un lado y pregunta,
    /// lee el secreto del otro a fuerza de preguntar. Colocar un fichero
    /// exige `copy` o `move` — exactamente lo que esta puerta pide, así que la
    /// puerta cae justo sobre el abuso. Y el agente con el caso de uso
    /// legítimo —«¿funcionó la copia que acabo de hacer?»— tiene `copy` por
    /// construcción.
    ///
    /// NO es un argumento de amplificación, y conviene no escribirlo como si
    /// lo fuera: `fs.search` con criterio de CONTENIDO lee hoy el mismo árbol
    /// entero bajo [`Self::covers_read`] a secas, y encima devuelve
    /// `MatchInfo::preview`, o sea texto de verdad. Comparado con eso, un bit
    /// por pareja no es la amplificación mayor, sino la menor.
    ///
    /// LA INCONSISTENCIA, dicha en voz alta: `fs.read` y `fs.search` con
    /// contenido pasan solo por [`Self::covers_read`], así que un agente con
    /// scope de solo `mkdir` puede leer bytes por esas dos vías aunque no
    /// pueda pedir esta comparación. La respuesta correcta NO es aflojar esta
    /// puerta para igualarlas: es un `PolicyOp::Read`/de contenido que cierre
    /// las tres a la vez, que es lo que #80 dejó fuera de alcance. Mientras
    /// tanto se prefiere la puerta estrecha en lo NUEVO, porque aflojarla
    /// después es aditivo y apretarla no.
    ///
    /// `Expired` con el mismo criterio que [`Self::covers_read`]: solo si un
    /// scope que HABRÍA concedido contenido está vencido.
    ///
    /// # Panics
    /// Solo si el lock interno queda envenenado.
    #[must_use]
    pub fn covers_content(&self, session: &str, path: &VPath, now: Instant) -> ScopeVerdict {
        if self.is_protected(path) {
            return ScopeVerdict::OutOfScope;
        }
        let map = self.inner.lock().expect("scope registry lock");
        let Some(scopes) = map.get(session) else {
            return ScopeVerdict::OutOfScope;
        };
        let mut saw_expired = false;
        for s in scopes {
            // Cubre la raíz Y concede una op que mueve bytes; lo demás no
            // cuenta ni para `Expired` (un scope de `mkdir` vencido no es un
            // permiso de contenido que caducó: nunca lo fue).
            if !s.roots.iter().any(|r| is_under(r, path)) {
                continue;
            }
            if !(s.ops.allows(PolicyOp::Copy) || s.ops.allows(PolicyOp::Move)) {
                continue;
            }
            if s.is_expired(now) {
                saw_expired = true;
            } else {
                return ScopeVerdict::Within;
            }
        }
        if saw_expired {
            ScopeVerdict::Expired
        } else {
            ScopeVerdict::OutOfScope
        }
    }
}

/// Veredicto del motor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Procede sin preguntar.
    Allow,
    /// Requiere aprobación interactiva.
    Ask,
    /// Denegado, con motivo.
    Deny(DenyReason),
}

/// Por qué se denegó.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// Ruta/op fuera del scope del agente.
    OutOfScope,
    /// El scope aplicable expiró.
    ScopeExpired,
    /// Una regla `deny` de `policy.toml`.
    PolicyRule,
    /// Fail-closed: ninguna regla aplicó dentro del scope.
    NoRule,
    /// `Ask` denegado o TTL vencido (lo pone el gate del engine).
    NotApproved,
}

impl DenyReason {
    /// Identificador estable de la causa, tal como viaja en
    /// [`norte_proto::Error::PolicyDenied`]`.rule` por el wire (M3-3b). El
    /// cliente lo compara por igualdad, jamás parsea texto libre. El conjunto
    /// es CERRADO y contractual: su enumeración autoritativa vive en el rustdoc
    /// de `PolicyDenied.rule`; cambiar un string aquí es cambio de wire.
    #[must_use]
    pub fn rule_id(self) -> &'static str {
        match self {
            DenyReason::OutOfScope => "out-of-scope",
            DenyReason::ScopeExpired => "scope-expired",
            DenyReason::PolicyRule => "policy-rule",
            DenyReason::NoRule => "no-rule",
            DenyReason::NotApproved => "not-approved",
        }
    }
}

/// El gate que consulta el engine antes de cada mutación.
pub trait PolicyGate: Send + Sync {
    /// Decide para (actor, op, rutas). TODAS las rutas deben pasar la frontera.
    fn evaluate(&self, actor: &Actor, op: PolicyOp, paths: &[&VPath]) -> Decision;
}

/// Gate permisivo (default del engine embebido / sin policy configurada).
pub struct AllowAll;

impl PolicyGate for AllowAll {
    fn evaluate(&self, _actor: &Actor, _op: PolicyOp, _paths: &[&VPath]) -> Decision {
        Decision::Allow
    }
}

/// Policy real: frontera de scope (agentes) + reglas `policy.toml`.
pub struct ScopedPolicy {
    scopes: ScopeRegistry,
    config: PolicyConfig,
}

impl ScopedPolicy {
    /// Con un registro de scopes y una config de reglas.
    #[must_use]
    pub fn new(scopes: ScopeRegistry, config: PolicyConfig) -> Self {
        Self { scopes, config }
    }
    /// Acceso al registro (para conceder scopes en 3b/tests).
    #[must_use]
    pub fn scopes(&self) -> &ScopeRegistry {
        &self.scopes
    }
}

impl PolicyGate for ScopedPolicy {
    fn evaluate(&self, actor: &Actor, op: PolicyOp, paths: &[&VPath]) -> Decision {
        let session = match actor {
            Actor::User => return Decision::Allow, // el humano no se sandboxea
            Actor::Agent { session } => session.as_str(),
            Actor::Plugin { id } => id.as_str(),
        };
        let now = Instant::now();
        // Frontera: TODAS las rutas dentro del scope, o deny duro.
        for path in paths {
            match self.scopes.permits(session, op, path, now) {
                ScopeVerdict::Within => {}
                ScopeVerdict::OutOfScope => return Decision::Deny(DenyReason::OutOfScope),
                ScopeVerdict::Expired => return Decision::Deny(DenyReason::ScopeExpired),
            }
        }
        // Dentro del scope: reglas de policy.toml (fail-closed sin regla).
        self.config.decide(actor, op, paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Actor;
    use norte_proto::{DeleteMode, VPath};

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire")
    }

    #[test]
    fn user_is_always_allowed() {
        let pol = ScopedPolicy::new(ScopeRegistry::new(), PolicyConfig::default());
        let d = pol.evaluate(
            &Actor::User,
            PolicyOp::Delete {
                mode: DeleteMode::Permanent,
            },
            &[&vp("file:///x")],
        );
        assert!(matches!(d, Decision::Allow));
    }

    #[test]
    fn una_raiz_protegida_no_la_concede_ni_un_scope_sobre_el_padre() {
        // #165: el directorio de estado del daemon cae bajo un scope sobre
        // `$HOME`, y ahí viven `journal.db` y los spools de sync.
        let estado = vp("file:///home/u/.config/norte");
        let reg = ScopeRegistry::with_protected_roots(vec![estado.clone()]);
        reg.grant(
            "s1",
            Scope::forever(vec![vp("file:///home/u")], OpSet::all()),
        );
        let now = Instant::now();
        for p in [
            "file:///home/u/.config/norte",
            "file:///home/u/.config/norte/journal.db",
            "file:///home/u/.config/norte/sync-spools/c1-ab.jsonl",
        ] {
            let p = vp(p);
            assert_eq!(
                reg.covers_read("s1", &p, now),
                ScopeVerdict::OutOfScope,
                "lectura de {p:?}"
            );
            assert_eq!(
                reg.covers_content("s1", &p, now),
                ScopeVerdict::OutOfScope,
                "contenido de {p:?}"
            );
            assert_eq!(
                reg.permits("s1", PolicyOp::Copy, &p, now),
                ScopeVerdict::OutOfScope,
                "mutación sobre {p:?}"
            );
            assert_eq!(
                reg.permits(
                    "s1",
                    PolicyOp::Delete {
                        mode: DeleteMode::Permanent
                    },
                    &p,
                    now
                ),
                ScopeVerdict::OutOfScope,
                "borrado de {p:?}"
            );
        }
    }

    #[test]
    fn la_proteccion_es_por_segmentos_y_no_toca_ni_al_padre_ni_al_vecino() {
        // Ni se come el ancestro (`fs.list` de `$HOME` sigue vivo) ni un
        // hermano con el mismo prefijo de BYTES (`norte-backup`).
        let reg = ScopeRegistry::with_protected_roots(vec![vp("file:///home/u/.config/norte")]);
        reg.grant(
            "s1",
            Scope::forever(vec![vp("file:///home/u")], OpSet::all()),
        );
        let now = Instant::now();
        for p in [
            "file:///home/u",
            "file:///home/u/.config",
            "file:///home/u/.config/norte-backup/journal.db",
            "file:///home/u/docs/x",
        ] {
            assert_eq!(
                reg.covers_read("s1", &vp(p), now),
                ScopeVerdict::Within,
                "{p} no está protegido"
            );
        }
    }

    #[test]
    fn una_raiz_protegida_de_otro_scheme_no_afecta() {
        // La protección es del `file://` del daemon: no puede recortar en
        // silencio un scope sobre otro provider con los mismos segmentos.
        let reg = ScopeRegistry::with_protected_roots(vec![vp("file:///home/u/.config/norte")]);
        reg.grant(
            "s1",
            Scope::forever(vec![vp("mem:///home/u")], OpSet::all()),
        );
        assert_eq!(
            reg.covers_read(
                "s1",
                &vp("mem:///home/u/.config/norte/journal.db"),
                Instant::now()
            ),
            ScopeVerdict::Within
        );
    }

    #[test]
    fn el_registro_por_defecto_protege_el_estado_del_daemon() {
        // El pin del wiring: `new()` (y `default()`, que es `new()`) llevan la
        // raíz del proceso, sin que nadie tenga que acordarse. En un proceso
        // sin HOME ni passwd el dir resuelto es relativo y no hay raíz: el
        // test compara contra la MISMA función, así que ambos casos valen.
        let esperado: Vec<VPath> = daemon_state_root().into_iter().collect();
        assert_eq!(ScopeRegistry::new().protected_roots(), esperado.as_slice());
        assert_eq!(
            ScopeRegistry::default().protected_roots(),
            esperado.as_slice()
        );
    }

    #[test]
    fn el_scoped_policy_deniega_una_mutacion_sobre_lo_protegido() {
        // La puerta de arriba, la que ve el agente: `out-of-scope`, la
        // categoría gruesa de siempre (nada nuevo en el wire).
        let reg = ScopeRegistry::with_protected_roots(vec![vp("file:///home/u/.config/norte")]);
        reg.grant(
            "s1",
            Scope::forever(vec![vp("file:///home/u")], OpSet::all()),
        );
        let pol = ScopedPolicy::new(reg, PolicyConfig::default());
        let agent = Actor::Agent {
            session: "s1".into(),
        };
        let d = pol.evaluate(
            &agent,
            PolicyOp::Delete {
                mode: DeleteMode::Permanent,
            },
            &[&vp("file:///home/u/.config/norte/journal.db")],
        );
        assert!(matches!(d, Decision::Deny(DenyReason::OutOfScope)), "{d:?}");
    }

    #[test]
    fn las_exclusiones_del_walk_son_del_agente_no_del_humano() {
        assert!(
            walk_exclusions(&Actor::User).is_empty(),
            "el humano busca en sus propios ficheros"
        );
        let esperado: Vec<VPath> = daemon_state_root().into_iter().collect();
        assert_eq!(
            walk_exclusions(&Actor::Agent {
                session: "s1".into()
            }),
            esperado
        );
        assert_eq!(
            walk_exclusions(&Actor::Plugin { id: "p1".into() }),
            esperado
        );
    }

    #[test]
    fn local_root_vpath_conserva_los_bytes_y_exige_absoluto() {
        assert!(local_root_vpath(std::path::Path::new("rel/ativo")).is_none());
        assert!(
            local_root_vpath(std::path::Path::new("/a/./b")).is_some(),
            "`.` lo normaliza `components()`"
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let dir =
                std::path::Path::new(std::ffi::OsStr::from_bytes(b"/home/\xff\xfe/.config/norte"));
            let v = local_root_vpath(dir).expect("absoluto");
            let segs: Vec<Vec<u8>> = v.segments().map(<[u8]>::to_vec).collect();
            assert_eq!(
                segs,
                vec![
                    b"home".to_vec(),
                    b"\xff\xfe".to_vec(),
                    b".config".to_vec(),
                    b"norte".to_vec()
                ],
                "un nombre que no es UTF-8 sobrevive (regla dura 1)"
            );
        }
    }

    #[test]
    fn deny_reason_rule_ids_are_the_closed_wire_vocabulary() {
        // Pin del conjunto CERRADO que viaja en PolicyDenied.rule: renombrar
        // cualquiera es cambio de wire y debe romper aquí (no en silencio).
        assert_eq!(DenyReason::OutOfScope.rule_id(), "out-of-scope");
        assert_eq!(DenyReason::ScopeExpired.rule_id(), "scope-expired");
        assert_eq!(DenyReason::PolicyRule.rule_id(), "policy-rule");
        assert_eq!(DenyReason::NoRule.rule_id(), "no-rule");
        assert_eq!(DenyReason::NotApproved.rule_id(), "not-approved");
    }

    #[test]
    fn agent_out_of_scope_is_denied() {
        let pol = ScopedPolicy::new(ScopeRegistry::new(), PolicyConfig::default());
        let agent = Actor::Agent {
            session: "s1".into(),
        };
        let d = pol.evaluate(&agent, PolicyOp::Copy, &[&vp("file:///a/x")]);
        assert!(matches!(d, Decision::Deny(DenyReason::OutOfScope)));
    }

    #[test]
    fn agent_in_scope_without_rule_is_denied_fail_closed() {
        let reg = ScopeRegistry::new();
        reg.grant("s1", Scope::forever(vec![vp("file:///a")], OpSet::all()));
        let pol = ScopedPolicy::new(reg, PolicyConfig::default());
        let agent = Actor::Agent {
            session: "s1".into(),
        };
        let d = pol.evaluate(&agent, PolicyOp::Copy, &[&vp("file:///a/x")]);
        assert!(
            matches!(d, Decision::Deny(DenyReason::NoRule)),
            "fail-closed sin regla"
        );
    }

    #[test]
    fn scope_containment_is_byte_exact_prefix() {
        assert!(is_under(&vp("file:///a"), &vp("file:///a/x")));
        assert!(is_under(&vp("file:///a"), &vp("file:///a")));
        assert!(!is_under(&vp("file:///a"), &vp("file:///ab")));
        assert!(!is_under(&vp("file:///a"), &vp("file:///b/x")));
        assert!(!is_under(&vp("file:///a"), &vp("mem:///a/x")));
    }

    #[test]
    fn op_not_granted_is_out_of_scope() {
        let reg = ScopeRegistry::new();
        // Solo copy concedido; un delete queda fuera de scope.
        reg.grant(
            "s1",
            Scope::forever(vec![vp("file:///a")], OpSet::of(&["copy"])),
        );
        let pol = ScopedPolicy::new(reg, PolicyConfig::default());
        let agent = Actor::Agent {
            session: "s1".into(),
        };
        let d = pol.evaluate(
            &agent,
            PolicyOp::Delete {
                mode: DeleteMode::Permanent,
            },
            &[&vp("file:///a/x")],
        );
        assert!(matches!(d, Decision::Deny(DenyReason::OutOfScope)));
    }

    #[test]
    fn expired_scope_denies_with_scope_expired() {
        let reg = ScopeRegistry::new();
        reg.grant(
            "s1",
            Scope {
                roots: vec![vp("file:///a")],
                ops: OpSet::all(),
                // Expiró hace rato.
                expires_at: Some(
                    Instant::now()
                        .checked_sub(std::time::Duration::from_secs(1))
                        .expect("instante en el pasado"),
                ),
            },
        );
        let pol = ScopedPolicy::new(reg, PolicyConfig::default());
        let agent = Actor::Agent {
            session: "s1".into(),
        };
        let d = pol.evaluate(&agent, PolicyOp::Copy, &[&vp("file:///a/x")]);
        assert!(matches!(d, Decision::Deny(DenyReason::ScopeExpired)));
    }

    #[test]
    fn covers_read_es_membresia_de_raiz_independiente_de_op() {
        let reg = ScopeRegistry::new();
        // Scope con SOLO `copy` (sin ninguna op de lectura): covers_read NO mira
        // la op — basta que la raíz contenga el path (fs.search es lectura, no
        // mapea a PolicyOp).
        reg.grant(
            "s1",
            Scope::forever(vec![vp("mem:///proj")], OpSet::of(&["copy"])),
        );
        let now = Instant::now();
        assert_eq!(
            reg.covers_read("s1", &vp("mem:///proj/sub/x"), now),
            ScopeVerdict::Within,
        );
        assert_eq!(
            reg.covers_read("s1", &vp("mem:///proj"), now),
            ScopeVerdict::Within,
            "la propia raíz cuenta"
        );
        assert_eq!(
            reg.covers_read("s1", &vp("mem:///otro"), now),
            ScopeVerdict::OutOfScope,
        );
        // Sesión sin ningún scope: fuera de scope.
        assert_eq!(
            reg.covers_read("s2", &vp("mem:///proj"), now),
            ScopeVerdict::OutOfScope,
        );
    }

    #[test]
    fn covers_read_solo_expirados_que_cubren_la_raiz_dan_expired() {
        let reg = ScopeRegistry::new();
        let past = Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .expect("instante en el pasado");
        // Scope YA expirado que cubre la raíz buscada.
        reg.grant(
            "s1",
            Scope {
                roots: vec![vp("mem:///proj")],
                ops: OpSet::all(),
                expires_at: Some(past),
            },
        );
        let now = Instant::now();
        assert_eq!(
            reg.covers_read("s1", &vp("mem:///proj/x"), now),
            ScopeVerdict::Expired,
        );
        // Un scope expirado que NO cubre la raíz → OutOfScope, no Expired: el
        // veredicto Expired solo lo produce un scope que HABRÍA cubierto.
        assert_eq!(
            reg.covers_read("s1", &vp("mem:///otra/x"), now),
            ScopeVerdict::OutOfScope,
        );
    }

    #[test]
    fn covers_read_vivo_gana_a_expirado_bajo_la_misma_raiz() {
        let reg = ScopeRegistry::new();
        let past = Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .expect("pasado");
        reg.grant(
            "s1",
            Scope {
                roots: vec![vp("mem:///proj")],
                ops: OpSet::all(),
                expires_at: Some(past),
            },
        );
        reg.grant("s1", Scope::forever(vec![vp("mem:///proj")], OpSet::all()));
        assert_eq!(
            reg.covers_read("s1", &vp("mem:///proj/x"), Instant::now()),
            ScopeVerdict::Within,
        );
    }

    #[test]
    fn covers_content_exige_una_op_que_maneje_bytes() {
        let reg = ScopeRegistry::new();
        // `mkdir` toca la estructura, no los bytes: cubre lectura pero NO
        // contenido. `copy` mueve bytes: cubre las dos.
        reg.grant(
            "solo-mkdir",
            Scope::forever(vec![vp("mem:///d")], OpSet::of(&["mkdir"])),
        );
        reg.grant(
            "con-copy",
            Scope::forever(vec![vp("mem:///d")], OpSet::of(&["copy"])),
        );
        reg.grant(
            "con-move",
            Scope::forever(vec![vp("mem:///d")], OpSet::of(&["move"])),
        );
        let now = Instant::now();
        assert_eq!(
            reg.covers_read("solo-mkdir", &vp("mem:///d/x"), now),
            ScopeVerdict::Within,
            "la lectura sigue siendo op-independiente"
        );
        assert_eq!(
            reg.covers_content("solo-mkdir", &vp("mem:///d/x"), now),
            ScopeVerdict::OutOfScope,
        );
        assert_eq!(
            reg.covers_content("con-copy", &vp("mem:///d/x"), now),
            ScopeVerdict::Within,
        );
        assert_eq!(
            reg.covers_content("con-move", &vp("mem:///d/x"), now),
            ScopeVerdict::Within,
        );
        // La raíz también manda: `copy` sobre /d no da contenido en /otro.
        assert_eq!(
            reg.covers_content("con-copy", &vp("mem:///otro/x"), now),
            ScopeVerdict::OutOfScope,
        );
    }

    #[test]
    fn covers_content_expirado_que_habria_cubierto_da_expired() {
        let reg = ScopeRegistry::new();
        let past = Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .expect("pasado");
        // ORDEN deliberado: `grant` PODA los scopes ya expirados de la sesión
        // al conceder, así que el vencido tiene que entrar el último para que
        // los dos convivan. El vivo cubre la raíz pero no maneja bytes: ni
        // convierte el veredicto en `Within` ni tapa el `Expired` del que sí
        // la manejaba.
        reg.grant(
            "s1",
            Scope::forever(vec![vp("mem:///d")], OpSet::of(&["mkdir"])),
        );
        reg.grant(
            "s1",
            Scope {
                roots: vec![vp("mem:///d")],
                ops: OpSet::of(&["copy"]),
                expires_at: Some(past),
            },
        );
        assert_eq!(
            reg.covers_content("s1", &vp("mem:///d/x"), Instant::now()),
            ScopeVerdict::Expired,
        );
    }
}
