//! Capabilities of a plugin (ADR 0022 D4): what the manifest DECLARES and the
//! host enforces. `exec` is ALWAYS `none` (spec §7.1) — it is validated while
//! parsing the manifest, it is not represented here.

use serde::Deserialize;

/// Feeds a hasher with a length-prefixed string (u64 LE length + bytes). No
/// ambiguity from concatenation. `pub(crate)` to compose canonical digests
/// from other modules (e.g. [`crate::Manifest::approval_digest`]).
pub(crate) fn update_str(h: &mut sha2::Sha256, s: &str) {
    use sha2::Digest;
    h.update((s.len() as u64).to_le_bytes());
    h.update(s.as_bytes());
}

/// Feeds a hasher with an OPTIONAL string: presence (`0`/`1`) + the
/// length-prefixed string. Distinguishes `None` from `Some("")`.
pub(crate) fn update_opt_str(h: &mut sha2::Sha256, value: Option<&str>) {
    use sha2::Digest;
    match value {
        None => h.update([0u8]),
        Some(s) => {
            h.update([1u8]);
            update_str(h, s);
        }
    }
}

/// Feeds a hasher with an OPTIONAL `i64`: presence (`0`/`1`) + 8 LE bytes.
/// Used by `[config.<key>]` (P2) for `min`/`max` of `int` keys.
pub(crate) fn update_opt_i64(h: &mut sha2::Sha256, value: Option<i64>) {
    use sha2::Digest;
    match value {
        None => h.update([0u8]),
        Some(v) => {
            h.update([1u8]);
            h.update(v.to_le_bytes());
        }
    }
}

/// Encodes a binary digest to lowercase hex (64 chars for sha256).
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // A write into a String never fails; the `_` doesn't hide a real error.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Scope of an FS permission: nothing, or only what the host opens and
/// passes (NEVER the raw FS — hard rule 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// No access.
    #[default]
    None,
    /// Only the resources the host hands over explicitly.
    Scoped,
}

impl Scope {
    /// `true` if it grants some access (to paint the badge).
    #[must_use]
    pub fn granted(self) -> bool {
        matches!(self, Scope::Scoped)
    }

    /// Canonical, stable byte for the capabilities digest (issue #69). Does
    /// NOT use the enum's discriminant (it could be reordered) but a fixed
    /// value.
    fn digest_tag(self) -> u8 {
        match self {
            Scope::None => 0,
            Scope::Scoped => 1,
        }
    }
}

/// FS write (ADR 0101): nothing, or a CLOSED list of file names —
/// sidecars — that a `hook` may ask the host to write next to what it
/// changed. The host writes them as the `plugin` actor, through the policy
/// engine and the journal; the guest never sees paths nor opens anything.
///
/// `fs-write = "scoped"` was a reserved value nobody honored, and since
/// ADR 0088 it is rejected while parsing: the [`Self::Reserved`] variant
/// exists so the rejection can say what was written, not to grant it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(untagged)]
pub enum FsWriteCap {
    /// No write.
    #[default]
    #[serde(skip)]
    None,
    /// A string (`"scoped"` or another): rejected while validating the
    /// manifest.
    Reserved(String),
    /// `fs-write = { sidecar = ["a", "b"] }`: the names, as given.
    Sidecar(SidecarList),
}

/// The `fs-write` table: a single key, and no other — it goes into the
/// approval digest, so an unknown key is an invalid manifest, not a field
/// that gets ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SidecarList {
    /// File names, one segment each. Validated in the manifest, not here.
    pub sidecar: Vec<String>,
}

impl FsWriteCap {
    /// The granted sidecar names; empty if there is no write.
    #[must_use]
    pub fn sidecar_names(&self) -> &[String] {
        match self {
            FsWriteCap::Sidecar(l) => &l.sidecar,
            _ => &[],
        }
    }

    /// Canonical byte + content for the digest. `None` digests EXACTLY what
    /// an absent `fs-write` digested before ADR 0101 (byte 0), so no
    /// existing approval moves. `Sidecar` carries a new byte and the names,
    /// in order: changing what a plugin can write is changing what was
    /// approved.
    fn update_digest(&self, h: &mut sha2::Sha256) {
        use sha2::Digest;
        match self {
            FsWriteCap::None => h.update([0u8]),
            // Never reaches the digest: it is rejected earlier. The byte
            // exists so that, if it ever did, it would not collide with
            // `None`.
            FsWriteCap::Reserved(_) => h.update([1u8]),
            FsWriteCap::Sidecar(l) => {
                h.update([2u8]);
                // A SET, like the `net` hosts: reordering two names in the
                // TOML is not changing what was approved.
                let mut names: Vec<&str> = l.sidecar.iter().map(String::as_str).collect();
                names.sort_unstable();
                names.dedup();
                h.update((names.len() as u64).to_le_bytes());
                for n in names {
                    update_str(h, n);
                }
            }
        }
    }
}

/// LOCATION access (ADR 0057): nothing, or read under the opaque token the
/// host hands over while painting a column.
///
/// CLOSED vocabulary, like `exec`: a value not listed here is an invalid
/// manifest, not a capability silently ignored. What is granted is reading
/// UNDER a directory the host opened and confined — the guest never
/// receives the path, so this does not reopen rule 9: it keeps it with its
/// own permission, visible when approving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocationCap {
    /// No location access.
    #[default]
    None,
    /// Read (`read`/`stat`/`list`) under the token.
    Read,
}

impl LocationCap {
    /// `true` if it grants some access (to paint the badge).
    #[must_use]
    pub fn granted(self) -> bool {
        matches!(self, Self::Read)
    }

    /// Canonical, stable byte for the digest, same as [`Scope::digest_tag`].
    fn digest_tag(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Read => 1,
        }
    }
}

/// Network permission: an allow-list of hosts.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NetCap {
    /// Hosts the plugin may connect to over OUTBOUND TCP (exact, no
    /// wildcards). An `ip:port` entry authorizes ONLY that port; an entry
    /// with just `ip` authorizes ANY port on that host (needed for passive
    /// FTP, which negotiates dynamic data ports) — the human sees this when
    /// approving. No DNS: it connects by IP (hostname resolution = stage
    /// 3b, #30).
    pub hosts: Vec<String>,
}

/// The manifest's `[capabilities]` block, already validated. An absent
/// permission = `None`/empty: no syscall.
/// `PartialEq` is not cosmetic: it is what lets an instance pool check that
/// the instance it is about to reuse has EXACTLY the permissions the
/// catalog just resolved (#224). Without that comparison, a withdrawn
/// consent would take as long to take effect as the pool's TTL, which is
/// unacceptable latency for a permission.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// FS read.
    #[serde(default, rename = "fs-read")]
    pub fs_read: Scope,
    /// FS write: the sidecars a `hook` may request (ADR 0101).
    #[serde(default, rename = "fs-write")]
    pub fs_write: FsWriteCap,
    /// Network (host allow-list); absent = no network.
    #[serde(default)]
    pub net: Option<NetCap>,
    /// AI access (`ai = "chat"`); absent = no AI. The raw string is stored
    /// (the concrete modes are typed in M4-ai).
    #[serde(default)]
    pub ai: Option<String>,
    /// Location (`location = "read"`); absent = no location (ADR 0057).
    #[serde(default)]
    pub location: LocationCap,
    /// PROJECT ROOT marker (`location-root-marker = ".git"`).
    ///
    /// With it, the host does not open the directory being listed but
    /// rather the nearest ANCESTOR containing an entry with that name — and
    /// tells the guest which prefix the user is looking at. Without it, the
    /// root is the visible directory.
    ///
    /// It exists because confinement is real: a token cannot go up (the
    /// kernel rejects `..`), so a plugin that needs a project's control
    /// file — `.git/index`, `Cargo.toml`, `.hg` — could otherwise only work
    /// when the user is right at the root. What is granted stays VISIBLE
    /// when approving: the marker's name is shown with the badge, and going
    /// up too far is cut at protected roots and at a level cap.
    #[serde(default, rename = "location-root-marker")]
    pub location_root_marker: Option<String>,
    /// `exec`: MUST be `none` or absent. Validated and discarded while
    /// parsing the manifest ([`crate::Manifest::from_toml`]); never exposed
    /// here.
    #[serde(default)]
    pub(crate) exec: Option<String>,
}

impl Capabilities {
    /// Capabilities with `fs-read=scoped` (for enforcement tests).
    #[doc(hidden)]
    #[must_use]
    pub fn scoped_read_for_test() -> Self {
        Self {
            fs_read: Scope::Scoped,
            ..Self::default()
        }
    }

    /// Capabilities with ONLY network: an allow-list of `hosts` the guest
    /// may connect to (#30 stage 3). The rest of the permissions stay at
    /// zero (no fs, no ai, no exec). Used by a network provider's wiring
    /// and its tests; the private `exec` prevents building the struct from
    /// outside.
    #[must_use]
    pub fn with_net(hosts: Vec<String>) -> Self {
        Self {
            net: Some(NetCap { hosts }),
            ..Self::default()
        }
    }

    /// Hex (sha256) digest of the CANONICAL form of these capabilities
    /// (issue #69). The host stores it ALONGSIDE the human's approval; if
    /// `plugin.toml` changes on disk after approving and a later
    /// `discover` brings different capabilities, this digest stops
    /// matching and the approval is treated as nonexistent
    /// (re-consent) — a defense against the approve↔run confused-deputy
    /// TOCTOU.
    ///
    /// The form is deterministic and NOT ambiguous: each field goes with
    /// its presence (`0`/`1`) and strings are length-prefixed (u64 LE), so
    /// that two different capability sets cannot collide by concatenation
    /// (e.g. one host `"a,b"` versus two hosts `"a"`,`"b"`). The network
    /// hosts are sorted: it is a SET, its order in the file is not
    /// semantic.
    #[must_use]
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        // Domain prefix + schema version: if the canonical form ever
        // changes, old digests won't collide with new ones.
        h.update(b"norte-plugin-caps:v1\n");
        self.update_digest(&mut h);
        hex_lower(&h.finalize())
    }

    /// Feeds a hasher with the CANONICAL form of these capabilities,
    /// WITHOUT finalizing (to compose a broader-scoped digest — e.g. the
    /// manifest's, [`crate::Manifest::approval_digest`]). It does not emit
    /// its own domain prefix: whoever finalizes adds it.
    pub(crate) fn update_digest(&self, h: &mut sha2::Sha256) {
        use sha2::Digest;
        h.update([self.fs_read.digest_tag()]);
        self.fs_write.update_digest(h);
        // net: presence + host count + each host length-prefixed. The
        // hosts are SORTED and DEDUPLICATED: it is a SET, neither the
        // order nor the repetitions in the file are semantic.
        match &self.net {
            None => h.update([0u8]),
            Some(net) => {
                h.update([1u8]);
                let mut hosts: Vec<&str> = net.hosts.iter().map(String::as_str).collect();
                hosts.sort_unstable();
                hosts.dedup();
                h.update((hosts.len() as u64).to_le_bytes());
                for host in hosts {
                    h.update((host.len() as u64).to_le_bytes());
                    h.update(host.as_bytes());
                }
            }
        }
        // ai: presence + length-prefixed string.
        update_opt_str(h, self.ai.as_deref());
        // location (ADR 0057): goes into the digest ONLY when granted.
        //
        // The order matters. Always emitting it would move the digest of
        // every manifest that does NOT ask for it, and that would reset
        // every human approval already given — the same snag P2 left
        // pinned in `manifest_without_config_digests_identical_to_pre_p2`.
        // Emitting it only when requested keeps those approvals AND still
        // requires a new one from whoever requests the capability: that is
        // the property needed, and the absence stays unambiguous because
        // the previous field (the optional `exec`) already self-delimits.
        if self.location.granted() {
            h.update([b'L', self.location.digest_tag()]);
            // The marker goes in WITH the capability: changing `.git` for
            // something else changes which directory gets opened, so it
            // requires approving again.
            update_opt_str(h, self.location_root_marker.as_deref());
        }
        // exec: ALWAYS `none`/absent (validated while parsing), but it
        // goes into the digest for completeness — if some future change
        // relaxed the invariant, the change would show up in the approval.
        update_opt_str(h, self.exec.as_deref());
    }

    /// Short labels of the granted permissions, for the manager's badge
    /// (ADR 0022 D5): e.g. `["fs-read", "net"]`.
    ///
    /// The location one STATES THE MARKER when there is one
    /// (`location-root:.git`, #241): with plain `location`, whoever
    /// approves reads "can read where I'm looking", and what it grants is
    /// "can read the nearest ancestor containing this" — which in a
    /// repository is every file in the project, not the folder that is
    /// open. The wider permission is the one that must be named.
    #[must_use]
    pub fn badges(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.fs_read.granted() {
            out.push("fs-read".to_owned());
        }
        // One badge PER NAME: what the human approves is which files the
        // plugin can write, and plain "fs-write" doesn't say that.
        for n in self.fs_write.sidecar_names() {
            out.push(format!("fs-write:{n}"));
        }
        if self.net.is_some() {
            out.push("net".to_owned());
        }
        if self.ai.is_some() {
            out.push("ai".to_owned());
        }
        if self.location.granted() {
            match &self.location_root_marker {
                Some(marker) => out.push(format!("location-root:{marker}")),
                None => out.push("location".to_owned()),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_stable_and_64_hex() {
        let caps = Capabilities::scoped_read_for_test();
        let d = caps.digest();
        assert_eq!(d.len(), 64, "sha256 hex = 64 chars: {d}");
        assert!(d.bytes().all(|b| b.is_ascii_hexdigit()));
        // Deterministic: two computations of the same value agree.
        assert_eq!(d, caps.digest());
    }

    #[test]
    fn digest_changes_when_capabilities_change() {
        let base = Capabilities::default();
        let read = Capabilities::scoped_read_for_test();
        assert_ne!(
            base.digest(),
            read.digest(),
            "adding fs-read must move the digest (re-consent)"
        );

        let with_net = Capabilities {
            net: Some(NetCap {
                hosts: vec!["example.com".into()],
            }),
            ..Capabilities::default()
        };
        assert_ne!(
            base.digest(),
            with_net.digest(),
            "adding net must move the digest"
        );
    }

    #[test]
    fn net_digest_is_by_set_not_by_order() {
        let a = Capabilities {
            net: Some(NetCap {
                hosts: vec!["a.example".into(), "b.example".into()],
            }),
            ..Capabilities::default()
        };
        let b = Capabilities {
            net: Some(NetCap {
                hosts: vec!["b.example".into(), "a.example".into()],
            }),
            ..Capabilities::default()
        };
        assert_eq!(
            a.digest(),
            b.digest(),
            "host order is not semantic: same set = same digest"
        );
    }

    #[test]
    fn net_digest_deduplicates_repeated_hosts() {
        // MINOR 2: a repeated host doesn't change the permission set, so it
        // must not change the digest compared to declaring it once.
        let once = Capabilities {
            net: Some(NetCap {
                hosts: vec!["a.example".into()],
            }),
            ..Capabilities::default()
        };
        let twice = Capabilities {
            net: Some(NetCap {
                hosts: vec!["a.example".into(), "a.example".into()],
            }),
            ..Capabilities::default()
        };
        assert_eq!(once.digest(), twice.digest());
    }

    #[test]
    fn digest_is_not_confused_by_host_concatenation() {
        // A host "a.example,b.example" must NOT collide with two hosts
        // "a.example" and "b.example" (length-prefixing avoids the
        // ambiguity).
        let joined = Capabilities {
            net: Some(NetCap {
                hosts: vec!["a.example,b.example".into()],
            }),
            ..Capabilities::default()
        };
        let split = Capabilities {
            net: Some(NetCap {
                hosts: vec!["a.example".into(), "b.example".into()],
            }),
            ..Capabilities::default()
        };
        assert_ne!(joined.digest(), split.digest());
    }
}
