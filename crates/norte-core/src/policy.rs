//! Policy engine (M3-3, spec §10): every mutation from an AGENT is evaluated
//! against the boundary of its SCOPE (paths + ops + TTL) and the rules in
//! `policy.toml`, giving `Allow | Ask | Deny` BEFORE it executes. Humans
//! (`User`) are not sandboxed. Enforcement, not prompt-engineering (rule 9).
//!
//! **Security requirements for M3-3b/M3-4 (annotated debt):**
//! - The daemon MUST install `ScopedPolicy` via [`crate::Engine::with_policy`];
//!   the default [`AllowAll`] is only for the embedded/human engine. With
//!   agents, forgetting the wiring = no sandbox (structural fail-open,
//!   security M3).
//! - The actor is fixed by the CORE per authenticated connection, never read
//!   from wire params: an agentic client must not be able to declare itself
//!   `User` (which short-circuits to `Allow`).
//! - [`ScopeRegistry`] today keys by a string id; namespace it by
//!   `(actor_kind, id)` so a Plugin does not inherit the scope of an Agent
//!   with the same name (m4), and add `revoke(session)` for incident response
//!   (m7). Related (#66): the session is CLAIMED by the client in its
//!   `initialize` — a hostile-cooperative agent can declare another agent's
//!   session and inherit its scopes, see/cancel its tasks and receive its
//!   `task.progress`. Accepted under the same-uid threat model (§14, a
//!   guardrail not a sandbox); if agent-to-agent isolation is ever needed,
//!   the session needs proof of possession (a token at creation), not just a
//!   name.
//! - The `Ask` resolver (M3-3b) MUST apply a TTL/timeout and be cancelable:
//!   the gate suspends the engine's call outside the Task framework (m5).
//! - Scope escape via a symlink inside→outside that the provider follows
//!   (m6): the mitigation is the provider's symlink policy (ADR 0005) + a
//!   hostile fixture; the gate reasons about logical `VPath`s.

use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use norte_proto::methods::RelPath;
use norte_proto::{DeleteMode, VPath};

use crate::journal::Actor;

pub use config::{PolicyConfig, PolicyConfigError, Rule, RuleAction};

mod config;

/// Evaluable operation (more detail than `TaskKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyOp {
    /// Copy.
    Copy,
    /// Move.
    Move,
    /// Delete (permanent or to trash).
    Delete {
        /// Permanent vs trash.
        mode: DeleteMode,
    },
    /// Directory creation.
    Mkdir,
    /// Creation of an EMPTY file (#290).
    ///
    /// Separate from [`PolicyOp::Mkdir`] because they are two distinct
    /// permissions: letting something create directories is not the same as
    /// letting it create files, and a rule that said "mkdir" and granted both
    /// would be one nobody wrote.
    Create,
    /// Change of POSIX PERMISSIONS (#314).
    ///
    /// Separate from [`PolicyOp::Create`] for the same reason it is separate
    /// from [`PolicyOp::Mkdir`]: letting something create files is not the
    /// same as letting it change who can read them, and a rule that granted
    /// both at once would be one nobody wrote.
    SetMode {
        /// The mode that is going to be set.
        ///
        /// It goes INSIDE the op and not alongside it because that is what
        /// makes it different from itself: two `set-mode`s on the same paths
        /// with `0600` and with `4777` are the same op and opposite
        /// decisions, and whoever asks the human needs to be able to say so
        /// (0.61.0).
        mode: u32,
        /// Goes down the TREE (0.62.0, #315).
        ///
        /// For the same reason as the mode: `set-mode` on a root and
        /// `set-mode` on that root plus its hundred thousand descendants are
        /// the same op and very different decisions. Without this, the
        /// question said "1 path".
        recursive: bool,
        /// The mode for DIRECTORIES, if it differs (0.62.0, #315).
        dir_mode: Option<u32>,
    },
}

impl PolicyOp {
    /// Stable label for rules/scope
    /// (`"copy"|"move"|"delete"|"mkdir"|"create"`).
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            PolicyOp::Copy => "copy",
            PolicyOp::Move => "move",
            PolicyOp::Delete { .. } => "delete",
            PolicyOp::Mkdir => "mkdir",
            PolicyOp::Create => "create",
            PolicyOp::SetMode { .. } => "set-mode",
        }
    }
}

/// Set of op-kinds that a scope grants.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpSet {
    kinds: BTreeSet<&'static str>,
}

impl OpSet {
    /// Every operation.
    #[must_use]
    pub fn all() -> Self {
        Self {
            kinds: ["copy", "move", "delete", "mkdir", "create", "set-mode"]
                .into_iter()
                .collect(),
        }
    }
    /// With an explicit set of op-kinds.
    #[must_use]
    pub fn of(kinds: &[&'static str]) -> Self {
        Self {
            kinds: kinds.iter().copied().collect(),
        }
    }
    /// From RUNTIME names (wire): keeps every name that is a canonical
    /// op-kind and DROPS the unknown ones (fail-closed — an op-kind we do not
    /// recognize is never granted). The source of truth for the valid kinds
    /// is [`Self::all`]: adding an op there makes it grantable over the wire
    /// without touching this method. Used by the daemon when granting a
    /// requested scope.
    #[must_use]
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> Self {
        let all = Self::all();
        let kinds = names
            .iter()
            .filter_map(|n| all.kinds.iter().copied().find(|k| *k == n.as_ref()))
            .collect();
        Self { kinds }
    }
    /// `true` if `op` is granted.
    #[must_use]
    pub fn allows(&self, op: PolicyOp) -> bool {
        self.kinds.contains(op.kind())
    }
}

/// A scope granted to an agent session: containment by subtree-prefix.
#[derive(Debug, Clone)]
pub struct Scope {
    /// Allowed roots (containment by segment prefix).
    pub roots: Vec<VPath>,
    /// Granted op-kinds.
    pub ops: OpSet,
    /// Expiration (TTL). `None` = no expiration (tests / permanent grants).
    pub expires_at: Option<Instant>,
}

impl Scope {
    /// A scope with no expiration (for tests / permanent grants).
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

/// `true` if `path` is under `root` (same scheme+authority and `root`'s
/// segments are a PREFIX of `path`'s). Delegated to [`RelPath::under`]
/// (`norte-proto`), which does the same byte-exact per-segment comparison
/// (hard rule 1) — it has been the only implementation since #172. The root
/// itself counts as contained: a scope over `/a` covers `/a`.
#[must_use]
pub fn is_under(root: &VPath, path: &VPath) -> bool {
    RelPath::under(root, path).is_some()
}

/// The `file://` `VPath` of an **absolute** NATIVE directory, the inverse of
/// what `norte-vfs-local` does when resolving a `VPath` to an OS path: the
/// components are copied to segments BYTE BY BYTE (hard rule 1 — a name that
/// is not UTF-8 survives), and the Windows prefix (`C:`, `\\server\share`) is
/// the first segment, exactly as the OS provider's root expects it.
///
/// `None` if the path is RELATIVE or carries a component that is not a name
/// (`.`/`..`): a root that cannot be named is not a root, and returning a
/// path that does not resolve would be worse than returning none at all.
///
/// ```
/// use norte_core::policy::local_root_vpath;
/// use std::path::Path;
///
/// # #[cfg(unix)] {
/// let r = local_root_vpath(Path::new("/home/u/.config/norte")).expect("absolute");
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
            // The unix root contributes no segment; the Windows prefix DOES
            // (it is the first segment of the OS root's `VPath`).
            Component::RootDir => continue,
            Component::Prefix(p) => norte_vfs::wtf8::os_to_bytes(p.as_os_str()),
            Component::Normal(os) => norte_vfs::wtf8::os_to_bytes(os),
            Component::CurDir | Component::ParentDir => return None,
        };
        out = out.join(Segment::new(bytes).ok()?);
    }
    Some(out)
}

/// The subtrees that the WALK of a recursive read must not look into for
/// this actor: empty for the human (who is not sandboxed) and this process's
/// protected root for an agent or a plugin.
///
/// Exists because the read gate looks only at the ROOT of the request and
/// nothing else: a search over `$HOME` is legitimate and would sweep the
/// state directory along with it (#165). The gate says whether you can start;
/// this says where you must not descend.
#[must_use]
pub fn walk_exclusions(actor: &Actor) -> Vec<VPath> {
    match actor {
        Actor::User => Vec::new(),
        Actor::Agent { .. } | Actor::Plugin { .. } => protected_roots(),
    }
}

/// This process's PROTECTED root: the daemon's state directory
/// (`journal.db`, the `sync.plan` spools, `index.db`, `secrets.age`,
/// `connections.toml`, `policy.toml` — the same directory, see
/// [`crate::connect::config_dir`]), as a `file://` `VPath`.
///
/// `None` if the resolved directory is not absolute — the `./.config/norte`
/// fallback of a process with neither `HOME` nor a passwd entry. There, there
/// is no root to protect because there is also no stable path a scope could
/// reach, and whoever builds the registry is left without the protection:
/// this is said here because a silent `None` in a security gate is exactly
/// the kind of thing nobody looks at.
#[must_use]
pub fn daemon_state_root() -> Option<VPath> {
    local_root_vpath(&crate::connect::config_dir())
}

/// The OTHER protected root: the user's STATE directory
/// (`$XDG_STATE_HOME/norte`), where `session.json` —with its lock— and
/// `logs/` live.
///
/// They are two different directories, and that is why two functions are
/// needed: the config one carries `journal.db`, the spools and the secrets;
/// this one carries the screen and the logs. Protecting it matters for the
/// same reason as the other and for something more concrete: `session.json`
/// IS the list of paths the reader has been moving through, so a scope over
/// `$HOME` —"sort my downloads"— that reached it would hand an agent the
/// reader's entire history through the front door — exactly what mode 0600
/// and the content-free diagnostics exist to prevent. And its
/// `session.json.lock` is a mutex BETWEEN PROCESSES: deleting it does not
/// destroy data, it makes two `norte`s believe they are the only writer at
/// the same time.
///
/// `None` for the same reason as [`daemon_state_root`]: without an absolute
/// path there is no root to protect.
#[must_use]
pub fn ui_state_root() -> Option<VPath> {
    norte_config::dirs::state_dir().and_then(|d| local_root_vpath(&d))
}

/// Everything this process protects by default, without anyone having to
/// remember either one.
#[must_use]
pub fn protected_roots() -> Vec<VPath> {
    daemon_state_root()
        .into_iter()
        .chain(ui_state_root())
        .collect()
}

/// Result of the scope-boundary check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeVerdict {
    /// Inside a live scope with the op granted.
    Within,
    /// Outside every scope.
    OutOfScope,
    /// Only expired scopes were applicable.
    Expired,
}

/// Per-agent-session scope grants (in memory; TTL). Thread-safe.
///
/// Also carries the **protected roots** (#165): subtrees that NO grant
/// reaches, whatever is granted. The only one that exists today is the
/// daemon's state directory, and [`Self::new`] sets it without anyone having
/// to remember — see [`Self::protected_roots`].
#[derive(Clone)]
pub struct ScopeRegistry {
    inner: Arc<Mutex<HashMap<String, Vec<Scope>>>>,
    /// Roots no scope reaches. Fixed at construction and never changes: an
    /// exclusion that can be removed on the fly is not an exclusion.
    protected: Arc<Vec<VPath>>,
}

impl Default for ScopeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeRegistry {
    /// Empty registry, with the two state directories ALREADY protected
    /// ([`protected_roots`]): the config one —`journal.db`, spools,
    /// secrets— and the user's state one, where the UI session lives.
    ///
    /// Protects by DEFAULT on purpose: the exclusion exists precisely because
    /// it gets granted unintentionally —a scope over `$HOME` contains
    /// `$HOME/.config/norte` under the default layout—, so a registry that
    /// had to be remembered to be protected would have failed exactly in the
    /// case that motivated the issue. For a registry with OTHER roots
    /// (tests, embedders), see [`Self::with_protected_roots`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_protected_roots(protected_roots())
    }
    /// Empty registry with EXPLICIT protected roots (does not consult the
    /// environment). For tests and for an embedder that anchors its state
    /// elsewhere.
    #[must_use]
    pub fn with_protected_roots(roots: Vec<VPath>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            protected: Arc::new(roots),
        }
    }
    /// This registry's protected roots.
    ///
    /// A path AT-OR-UNDER one of them gets [`ScopeVerdict::OutOfScope`] from
    /// [`Self::permits`], [`Self::covers_read`] and [`Self::covers_content`]
    /// —the three gates every mutation and read from an agent passes
    /// through— BEFORE looking at any grant. ANCESTORS are left untouched:
    /// `fs.list` of `$HOME` keeps working and shows the name of the state
    /// directory, which is the same thing `ls` shows; what you cannot do is
    /// go in.
    ///
    /// The verdict is `OutOfScope` and not its own category because the
    /// coarse cause that travels over the wire (`PolicyDenied.rule`) is a
    /// CLOSED vocabulary: the path is not within any reachable scope, which
    /// is literally what `out-of-scope` says. Adding a category would be a
    /// wire change to say the same thing with more detail than a denied
    /// agent should receive.
    ///
    /// WHAT THIS DOES NOT COVER, said here because a half gate reads as a
    /// complete one: the protection is on the LOGICAL `VPath`. A different
    /// path that resolves to the same directory —a symlink from inside the
    /// scope— dodges it, and that is #164's family (`RESOLVE_BENEATH`), not
    /// something this registry can decide.
    #[must_use]
    pub fn protected_roots(&self) -> &[VPath] {
        &self.protected
    }
    /// `true` if `path` falls under a protected root (the root itself
    /// included).
    #[must_use]
    fn is_protected(&self, path: &VPath) -> bool {
        self.protected.iter().any(|r| is_under(r, path))
    }
    /// Grants `scope` to session `session`.
    ///
    /// # Panics
    /// Only if the internal lock is poisoned.
    pub fn grant(&self, session: &str, scope: Scope) {
        let now = Instant::now();
        let mut map = self.inner.lock().expect("scope registry lock");
        if scope.roots.iter().any(|r| {
            self.protected
                .iter()
                .any(|p| is_under(p, r) || is_under(r, p))
        }) {
            // The grant is accepted and trimmed when consulted: warning about
            // it here is the only thing that stops an operator from
            // believing they granted the state directory and wondering why
            // the agent fails.
            tracing::warn!(
                session,
                "the granted scope touches a protected root: nothing is granted there"
            );
        }
        let entry = map.entry(session.to_owned()).or_default();
        // Lazy pruning: on grant, discard this session's already-expired
        // scopes so the `Vec` does not grow monotonically in a long-lived
        // daemon. Collecting dead sessions' keys is m7 debt (revoke).
        entry.retain(|s| !s.is_expired(now));
        entry.push(scope);
    }

    /// Revokes ALL of `session`'s scopes. This is what closes the transient
    /// gap opened for a plugin for a write (ADR 0101): the TTL is the safety
    /// net, this is the door.
    ///
    /// # Panics
    /// If the registry's lock is poisoned by a previous panic.
    pub fn revoke_all(&self, session: &str) {
        self.inner
            .lock()
            .expect("scope registry lock")
            .remove(session);
    }
    /// Boundary verdict for `(session, op, path)` at instant `now`.
    ///
    /// # Panics
    /// Only if the internal lock is poisoned.
    #[must_use]
    pub fn permits(&self, session: &str, op: PolicyOp, path: &VPath, now: Instant) -> ScopeVerdict {
        // BEFORE looking at a single grant (#165): what is protected is not
        // granted.
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

    /// ROOT membership for a recursive read (`fs.search`), INDEPENDENT of the
    /// op: `Within` if ANY LIVE scope of the session has a root with
    /// `is_under(root, path)`. Unlike [`Self::permits`], it does not consult
    /// the [`OpSet`]: search is a read and does not map to a concrete
    /// [`PolicyOp`] — the criterion is simple containment within the granted
    /// boundary. `Expired` if ONLY already-expired scopes would have covered
    /// the root (an expired scope that does not even cover it does not
    /// produce `Expired`); `OutOfScope` if none covers it.
    ///
    /// This is the read gate for ALL of an agent's reads (fs.search T4 +
    /// fs.list/read/stat/capabilities + plugin.preview, #80): an `Agent`
    /// only reads under a scope granted to its session; a `User` is not
    /// sandboxed. The single source that consults it is `daemon::read_gate`.
    ///
    /// ACCEPTED CAVEAT (op-independent, oscar's decision in
    /// `2026-07-18-gate-lectura-agentes-design.md` §1): by ignoring the
    /// [`OpSet`], a scope of ONLY `delete`/`mkdir` under `/tmp` grants read
    /// access to `/tmp`. Accepted knowingly: reading is strictly less than
    /// any mutation and `copy`/`move`/`delete` ALREADY imply reading; the
    /// residual `delete`/`mkdir`-without-read is rare. Pure least-privilege
    /// (write-only-no-read) would require a `PolicyOp::Read` — out of scope
    /// for #80.
    ///
    /// # Panics
    /// Only if the internal lock is poisoned.
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
            // Only a scope that REALLY covers the root counts; the op is
            // ignored.
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

    /// Root membership for an amplified CONTENT read (`fs.compare` with the
    /// hash rung, C6): like [`Self::covers_read`], but additionally
    /// requiring that the scope grant an op that handles BYTES — today
    /// `copy` or `move`, the two that cannot execute without reading the
    /// source's content.
    ///
    /// Why a second gate exists: the hash rung is a **content equality
    /// oracle**. It answers "are these two files equal?" without returning a
    /// single byte, and what makes it dangerous is not reading, it is being
    /// able to PLACE the candidate: whoever puts their guess on one side and
    /// asks, reads the other side's secret by dint of asking. Placing a file
    /// requires `copy` or `move` — exactly what this gate asks for, so the
    /// gate falls right on top of the abuse. And the agent with the
    /// legitimate use case —"did the copy I just made work?"— has `copy` by
    /// construction.
    ///
    /// This is NOT an amplification argument, and it is worth not writing it
    /// as if it were: `fs.search` with a CONTENT criterion today reads the
    /// same entire tree under [`Self::covers_read`] alone, and on top of
    /// that returns `MatchInfo::preview`, i.e. actual text. Compared to that,
    /// a single bit per pair is not the biggest amplification, but the
    /// smallest.
    ///
    /// THE INCONSISTENCY, said out loud: `fs.read` and `fs.search` with
    /// content go only through [`Self::covers_read`], so an agent with a
    /// scope of only `mkdir` can read bytes through those two paths even
    /// though it cannot request this comparison. The correct answer is NOT
    /// to loosen this gate to match them: it is a content `PolicyOp::Read`
    /// that closes all three at once, which is what #80 left out of scope.
    /// In the meantime the narrow gate is preferred on what is NEW, because
    /// loosening it later is additive and tightening it is not.
    ///
    /// `Expired` with the same criterion as [`Self::covers_read`]: only if a
    /// scope that WOULD have granted content is expired.
    ///
    /// # Panics
    /// Only if the internal lock is poisoned.
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
            // Covers the root AND grants an op that moves bytes; nothing
            // else counts, not even for `Expired` (an expired `mkdir` scope
            // is not a content permission that expired: it never was one).
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

/// The engine's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Proceeds without asking.
    Allow,
    /// Requires interactive approval.
    Ask,
    /// Denied, with a reason.
    Deny(DenyReason),
}

/// Why it was denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// Path/op outside the agent's scope.
    OutOfScope,
    /// The applicable scope expired.
    ScopeExpired,
    /// A `deny` rule from `policy.toml`.
    PolicyRule,
    /// Fail-closed: no rule applied within the scope.
    NoRule,
    /// `Ask` denied or TTL expired (set by the engine's gate).
    NotApproved,
}

impl DenyReason {
    /// Stable identifier of the cause, exactly as it travels in
    /// [`norte_proto::Error::PolicyDenied`]`.rule` over the wire (M3-3b). The
    /// client compares it by equality, never parses free text. The set is
    /// CLOSED and contractual: its authoritative enumeration lives in the
    /// rustdoc of `PolicyDenied.rule`; changing a string here is a wire
    /// change.
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

/// The gate the engine consults before every mutation.
pub trait PolicyGate: Send + Sync {
    /// Decides for (actor, op, paths). ALL paths must pass the boundary.
    fn evaluate(&self, actor: &Actor, op: PolicyOp, paths: &[&VPath]) -> Decision;
}

/// Permissive gate (default for the embedded engine / no policy configured).
pub struct AllowAll;

impl PolicyGate for AllowAll {
    fn evaluate(&self, _actor: &Actor, _op: PolicyOp, _paths: &[&VPath]) -> Decision {
        Decision::Allow
    }
}

/// Real policy: scope boundary (agents) + `policy.toml` rules.
pub struct ScopedPolicy {
    scopes: ScopeRegistry,
    config: PolicyConfig,
}

impl ScopedPolicy {
    /// With a scope registry and a rule config.
    #[must_use]
    pub fn new(scopes: ScopeRegistry, config: PolicyConfig) -> Self {
        Self { scopes, config }
    }
    /// Access to the registry (to grant scopes in 3b/tests).
    #[must_use]
    pub fn scopes(&self) -> &ScopeRegistry {
        &self.scopes
    }
}

/// The key with which a non-human actor enters the [`ScopeRegistry`]: an
/// agent's session as-is, and a plugin under `plugin:` — two namespaces,
/// because a plugin id (`org.x.y`) is also a valid agent session, and
/// without the prefix an agent that connected with that name would inherit
/// whatever is granted to the plugin (ADR 0101).
#[must_use]
pub fn scope_key(actor: &Actor) -> Option<std::borrow::Cow<'_, str>> {
    match actor {
        Actor::User => None,
        Actor::Agent { session } => Some(std::borrow::Cow::Borrowed(session.as_str())),
        Actor::Plugin { id } => Some(std::borrow::Cow::Owned(format!("plugin:{id}"))),
    }
}

impl PolicyGate for ScopedPolicy {
    fn evaluate(&self, actor: &Actor, op: PolicyOp, paths: &[&VPath]) -> Decision {
        // The human is not sandboxed.
        let Some(session) = scope_key(actor) else {
            return Decision::Allow;
        };
        let now = Instant::now();
        // Boundary: ALL paths inside the scope, or a hard deny.
        for path in paths {
            match self.scopes.permits(&session, op, path, now) {
                ScopeVerdict::Within => {}
                ScopeVerdict::OutOfScope => return Decision::Deny(DenyReason::OutOfScope),
                ScopeVerdict::Expired => return Decision::Deny(DenyReason::ScopeExpired),
            }
        }
        // Inside the scope: `policy.toml` rules. With no matching rule, an
        // agent is denied (fail-closed: nobody gave it permission); a plugin
        // is allowed, because ITS rule is the manifest the human approved
        // with the `fs-write:<name>` badge in front (ADR 0101). A rule that
        // denies or asks still wins.
        match self.config.decide(actor, op, paths) {
            Decision::Deny(DenyReason::NoRule) if matches!(actor, Actor::Plugin { .. }) => {
                Decision::Allow
            }
            d => d,
        }
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

    /// **Every `PolicyOp` must be GRANTABLE over the wire** (#290).
    ///
    /// `create` came in as a new op and `OpSet::all()` was left with four
    /// kinds, so `from_names(["create"])` returned an EMPTY set with no
    /// error: an agent requested the scope, the daemon traced that it was
    /// granted, and every `fs.create` was denied with `OutOfScope` — a reason
    /// that also lied, because the path really was inside. Fails closed, but
    /// turns a permission into something that only exists to deny.
    ///
    /// This test walks the WHOLE enum so the next one is not forgotten.
    #[test]
    fn every_op_kind_can_be_granted_over_the_wire() {
        let all_ops = [
            PolicyOp::Copy,
            PolicyOp::Move,
            PolicyOp::Delete {
                mode: DeleteMode::Trash,
            },
            PolicyOp::Mkdir,
            PolicyOp::Create,
            PolicyOp::SetMode {
                mode: 0o755,
                recursive: false,
                dir_mode: None,
            },
        ];
        for op in all_ops {
            let name = op.kind();
            let set = OpSet::from_names(&[name]);
            assert!(
                set.allows(op),
                "`{name}` is a core op-kind the wire cannot grant: \
                 missing from `OpSet::all()`"
            );
            assert!(
                OpSet::all().allows(op),
                "`{name}` is not even granted by a FULL scope"
            );
        }
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
    fn a_protected_root_is_not_granted_even_by_a_scope_over_its_parent() {
        // #165: the daemon's state directory falls under a scope over
        // `$HOME`, and that is where `journal.db` and the sync spools live.
        let state = vp("file:///home/u/.config/norte");
        let reg = ScopeRegistry::with_protected_roots(vec![state.clone()]);
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
                "read of {p:?}"
            );
            assert_eq!(
                reg.covers_content("s1", &p, now),
                ScopeVerdict::OutOfScope,
                "content of {p:?}"
            );
            assert_eq!(
                reg.permits("s1", PolicyOp::Copy, &p, now),
                ScopeVerdict::OutOfScope,
                "mutation on {p:?}"
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
                "delete of {p:?}"
            );
        }
    }

    #[test]
    fn protection_is_by_segments_and_touches_neither_the_parent_nor_the_sibling() {
        // Neither eats the ancestor (`fs.list` of `$HOME` stays alive) nor a
        // sibling with the same BYTE prefix (`norte-backup`).
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
                "{p} is not protected"
            );
        }
    }

    #[test]
    fn a_protected_root_of_another_scheme_does_not_affect_it() {
        // The protection is of the daemon's `file://`: it must not silently
        // trim a scope over another provider with the same segments.
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

    /// The UI session does not live where `journal.db` does, and that is the
    /// whole trap: issue #165 protected `$XDG_CONFIG_HOME/norte` and the
    /// screen is saved under `$XDG_STATE_HOME/norte`, a directory that was
    /// left outside. A scope over `$HOME` —"sort my downloads"— reached it,
    /// and inside it `session.json` is the list of paths the reader has been
    /// walking through.
    #[test]
    fn the_users_state_dir_is_also_not_granted_by_a_scope_over_its_parent() {
        let state = vp("file:///home/u/.local/state/norte");
        let reg = ScopeRegistry::with_protected_roots(vec![state.clone()]);
        reg.grant(
            "s1",
            Scope::forever(vec![vp("file:///home/u")], OpSet::all()),
        );
        let now = Instant::now();
        for p in [
            "file:///home/u/.local/state/norte",
            "file:///home/u/.local/state/norte/session.json",
            // The lock is a mutex BETWEEN PROCESSES: deleting it does not
            // delete data, it makes two `norte`s believe they are the only
            // writer at the same time.
            "file:///home/u/.local/state/norte/session.json.lock",
            "file:///home/u/.local/state/norte/logs/norte.log",
        ] {
            assert_eq!(
                reg.permits(
                    "s1",
                    PolicyOp::Delete {
                        mode: DeleteMode::Permanent,
                    },
                    &vp(p),
                    now
                ),
                ScopeVerdict::OutOfScope,
                "{p} had to be out of scope"
            );
        }
    }

    /// The TWO roots are distinct, and protecting them is the job of a
    /// function nobody has to remember to call.
    #[test]
    fn the_two_protected_roots_are_two() {
        let (Some(config), Some(state)) = (daemon_state_root(), ui_state_root()) else {
            // With neither `HOME` nor passwd there are no absolute paths to
            // protect; the rest of the test does not apply.
            return;
        };
        assert_ne!(config, state, "config and state are two directories");
        let all_roots = protected_roots();
        assert!(all_roots.contains(&config) && all_roots.contains(&state));
    }

    #[test]
    fn the_default_registry_protects_the_daemons_state() {
        // The wiring pin: `new()` (and `default()`, which is `new()`) carry
        // the process's root, without anyone having to remember. In a
        // process with neither HOME nor passwd the resolved dir is relative
        // and there is no root: the test compares against the SAME function,
        // so both cases hold.
        let expected: Vec<VPath> = protected_roots();
        assert_eq!(ScopeRegistry::new().protected_roots(), expected.as_slice());
        assert_eq!(
            ScopeRegistry::default().protected_roots(),
            expected.as_slice()
        );
    }

    #[test]
    fn scoped_policy_denies_a_mutation_on_whats_protected() {
        // The upper gate, the one the agent sees: `out-of-scope`, the usual
        // coarse category (nothing new on the wire).
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
    fn walk_exclusions_are_the_agents_not_the_humans() {
        assert!(
            walk_exclusions(&Actor::User).is_empty(),
            "the human searches their own files"
        );
        let expected: Vec<VPath> = protected_roots();
        assert_eq!(
            walk_exclusions(&Actor::Agent {
                session: "s1".into()
            }),
            expected
        );
        assert_eq!(
            walk_exclusions(&Actor::Plugin { id: "p1".into() }),
            expected
        );
    }

    #[test]
    fn local_root_vpath_keeps_the_bytes_and_requires_absolute() {
        assert!(local_root_vpath(std::path::Path::new("rel/ativo")).is_none());
        assert!(
            local_root_vpath(std::path::Path::new("/a/./b")).is_some(),
            "`components()` normalizes `.`"
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let dir =
                std::path::Path::new(std::ffi::OsStr::from_bytes(b"/home/\xff\xfe/.config/norte"));
            let v = local_root_vpath(dir).expect("absolute");
            let segments: Vec<Vec<u8>> = v.segments().map(<[u8]>::to_vec).collect();
            assert_eq!(
                segments,
                vec![
                    b"home".to_vec(),
                    b"\xff\xfe".to_vec(),
                    b".config".to_vec(),
                    b"norte".to_vec()
                ],
                "a name that is not UTF-8 survives (hard rule 1)"
            );
        }
    }

    #[test]
    fn deny_reason_rule_ids_are_the_closed_wire_vocabulary() {
        // Pin of the CLOSED set that travels in PolicyDenied.rule: renaming
        // any of them is a wire change and must break here (not silently).
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
            "fail-closed with no rule"
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
        // Only copy is granted; a delete is out of scope.
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
                // Expired a while ago.
                expires_at: Some(
                    Instant::now()
                        .checked_sub(std::time::Duration::from_secs(1))
                        .expect("instant in the past"),
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
    fn covers_read_is_op_independent_root_membership() {
        let reg = ScopeRegistry::new();
        // Scope with ONLY `copy` (no read op at all): covers_read does NOT
        // look at the op — it is enough for the root to contain the path
        // (fs.search is a read, it does not map to a PolicyOp).
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
            "the root itself counts"
        );
        assert_eq!(
            reg.covers_read("s1", &vp("mem:///otro"), now),
            ScopeVerdict::OutOfScope,
        );
        // Session with no scope at all: out of scope.
        assert_eq!(
            reg.covers_read("s2", &vp("mem:///proj"), now),
            ScopeVerdict::OutOfScope,
        );
    }

    #[test]
    fn covers_read_only_expired_scopes_covering_the_root_give_expired() {
        let reg = ScopeRegistry::new();
        let past = Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .expect("instant in the past");
        // Scope that is ALREADY expired and covers the searched root.
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
        // An expired scope that does NOT cover the root → OutOfScope, not
        // Expired: the Expired verdict is only produced by a scope that
        // WOULD have covered it.
        assert_eq!(
            reg.covers_read("s1", &vp("mem:///otra/x"), now),
            ScopeVerdict::OutOfScope,
        );
    }

    #[test]
    fn covers_read_live_wins_over_expired_under_the_same_root() {
        let reg = ScopeRegistry::new();
        let past = Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .expect("past");
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
    fn covers_content_requires_an_op_that_handles_bytes() {
        let reg = ScopeRegistry::new();
        // `mkdir` touches structure, not bytes: it covers reads but NOT
        // content. `copy` moves bytes: it covers both.
        reg.grant(
            "only-mkdir",
            Scope::forever(vec![vp("mem:///d")], OpSet::of(&["mkdir"])),
        );
        reg.grant(
            "with-copy",
            Scope::forever(vec![vp("mem:///d")], OpSet::of(&["copy"])),
        );
        reg.grant(
            "with-move",
            Scope::forever(vec![vp("mem:///d")], OpSet::of(&["move"])),
        );
        let now = Instant::now();
        assert_eq!(
            reg.covers_read("only-mkdir", &vp("mem:///d/x"), now),
            ScopeVerdict::Within,
            "the read remains op-independent"
        );
        assert_eq!(
            reg.covers_content("only-mkdir", &vp("mem:///d/x"), now),
            ScopeVerdict::OutOfScope,
        );
        assert_eq!(
            reg.covers_content("with-copy", &vp("mem:///d/x"), now),
            ScopeVerdict::Within,
        );
        assert_eq!(
            reg.covers_content("with-move", &vp("mem:///d/x"), now),
            ScopeVerdict::Within,
        );
        // The root also matters: `copy` over /d does not give content over
        // /otro.
        assert_eq!(
            reg.covers_content("with-copy", &vp("mem:///otro/x"), now),
            ScopeVerdict::OutOfScope,
        );
    }

    #[test]
    fn covers_content_expired_that_would_have_covered_it_gives_expired() {
        let reg = ScopeRegistry::new();
        let past = Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .expect("past");
        // Deliberate ORDER: `grant` PRUNES the session's already-expired
        // scopes on grant, so the expired one has to go in last for the two
        // to coexist. The live one covers the root but does not handle
        // bytes: it neither turns the verdict into `Within` nor hides the
        // `Expired` of the one that did handle them.
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
