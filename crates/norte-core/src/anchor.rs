//! A directory's anchor: which node the human was looking at (#295).
//!
//! A [`DirAnchor`] is what a listing returns and what a copy or move returns
//! to say "the destination directory was THAT ONE". It serves the one thing
//! ADR 0072 leaves open: a link **already planted** by the time the core
//! first looks is, from inside the core, indistinguishable from a legitimate
//! `~/copies -> /mnt/disk/copies`. From outside there is something that does
//! distinguish them — the human wasn't looking at that other node.
//!
//! # Why it is opaque
//!
//! What identifies a node is a (volume, index) pair, i.e. the device and the
//! inode. Sending them raw over the wire would tell any client —a scoped
//! agent, a plugin— which two paths are the same file and which inode
//! numbers exist, which is none of its business. So what travels is
//! `sha256(secret || volume || index)` truncated to 128 bits: equality is
//! preserved, which is the only thing needed, and the node cannot be
//! deduced nor the anchor forged.
//!
//! The secret is drawn ONCE per process. A daemon that restarts renews the
//! secret and with it every anchor, but a client that reconnects has lost
//! its listing anyway and asks for it again: the window that matters
//! —looking, approving, writing— falls entirely within one session.

use norte_proto::DirAnchor;
use norte_vfs::NodeId;

/// The process secret. Drawn on first use.
static SECRET: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();

fn secret() -> &'static [u8; 32] {
    SECRET.get_or_init(|| {
        let mut bytes = [0u8; 32];
        // A failure of the system CSPRNG must not degrade to a predictable
        // secret: without a real secret, a client could forge the anchor of
        // a node it has never seen and the check would stop checking.
        // `getrandom` only fails if the system has no entropy, which here
        // is as fatal as having no filesystem.
        getrandom::fill(&mut bytes).expect("the system provides no entropy for the anchor secret");
        bytes
    })
}

/// The anchor for `id`: the same for the same node while the process lives,
/// different for different nodes, and with nothing inside that gives it
/// away.
#[must_use]
pub fn de_nodo(id: NodeId) -> DirAnchor {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    h.update(secret());
    h.update(id.volume.to_le_bytes());
    h.update(id.index.to_le_bytes());
    let d = h.finalize();
    let mut hex = String::with_capacity(norte_proto::DIR_ANCHOR_LEN);
    for b in &d[..norte_proto::DIR_ANCHOR_LEN / 2] {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    DirAnchor::new(hex)
}

/// Does the anchor the request brings name node `id`?
///
/// A malformed anchor matches nothing: no separate case is needed for it,
/// because [`de_nodo`] never produces one, and failing closed is correct
/// here —the anchor exists to authorize, not to dispense—.
#[must_use]
pub fn casa(esperada: &DirAnchor, id: NodeId) -> bool {
    de_nodo(id) == *esperada
}

/// The anchors of the directories a client has LISTED, with a cap and
/// arrival order (#301).
///
/// The twin of the one `norte-client` keeps in its `Inner` for the remote
/// path, and exists for the same reason: whoever lists is the panel and
/// whoever writes afterward can be another clone of the same backend, so
/// the memory lives alongside the shared state rather than in the frontend.
/// On the EMBEDDED path that shared state is the [`Engine`](crate::Engine),
/// which is the only thing a cloned `Backend::Embedded` shares.
///
/// Capped and best-effort: these are directories a human has open, i.e. a
/// handful. Losing one costs that write's check, never the write itself.
///
/// **Duplicated on purpose and not shared with the SDK**: exporting it from
/// `norte-client` would tie the embedded path —which exists to work WITHOUT
/// a daemon, and compiles on platforms where the SDK's transport does not—
/// to a crate it needs for nothing. It's forty lines and a cap.
#[derive(Debug, Default)]
pub(crate) struct AnchorCache {
    by_dir: std::collections::HashMap<norte_proto::VPath, DirAnchor>,
    order: std::collections::VecDeque<norte_proto::VPath>,
}

/// How many directories are remembered at once. Same number as the SDK.
const ANCHORS_MAX: usize = 64;

impl AnchorCache {
    /// Remembers (or refreshes) `dir`'s anchor.
    ///
    /// `None` DELETES whatever there was, and that is deliberate: a listing
    /// that no longer brings an anchor —because the provider stopped being
    /// able to give one— cannot leave the old one alive. A write that sent
    /// a stale anchor would reject itself for no reason.
    ///
    /// Eviction is **LRU, not FIFO**: refreshing moves the directory to the
    /// back of the queue. With FIFO —which is what this did, and what the
    /// SDK still does— the active panel's directory would get evicted as
    /// soon as 64 DIFFERENT directories passed through the same backend, no
    /// matter that it was being relisted every second; and then the next
    /// write's check would disappear without anyone saying so, which is
    /// failing open in silence.
    pub(crate) fn remember(&mut self, dir: &norte_proto::VPath, anchor: Option<DirAnchor>) {
        let Some(anchor) = anchor else {
            self.by_dir.remove(dir);
            self.order.retain(|d| d != dir);
            return;
        };
        if self.by_dir.insert(dir.clone(), anchor).is_some() {
            self.order.retain(|d| d != dir);
        }
        self.order.push_back(dir.clone());
        while self.order.len() > ANCHORS_MAX {
            if let Some(old) = self.order.pop_front() {
                self.by_dir.remove(&old);
            }
        }
    }

    /// `dir`'s retained anchor, if it was listed.
    pub(crate) fn get(&self, dir: &norte_proto::VPath) -> Option<DirAnchor> {
        self.by_dir.get(dir).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vpd(wire: &str) -> norte_proto::VPath {
        norte_proto::VPath::parse(wire).expect("test wire")
    }

    #[test]
    fn only_what_was_listed_is_remembered() {
        let mut c = AnchorCache::default();
        let dir = vpd("file:///home");
        let a = DirAnchor::new("a".repeat(32));
        c.remember(&dir, Some(a.clone()));
        assert_eq!(c.get(&dir), Some(a));
        assert_eq!(c.get(&vpd("file:///other")), None);
    }

    /// A listing WITHOUT an anchor deletes the old one: sending the stale one
    /// would make the write reject itself.
    #[test]
    fn a_listing_with_no_anchor_deletes_the_old_one() {
        let mut c = AnchorCache::default();
        let dir = vpd("file:///home");
        c.remember(&dir, Some(DirAnchor::new("b".repeat(32))));
        c.remember(&dir, None);
        assert_eq!(c.get(&dir), None);
    }

    /// The cap evicts whichever one went longest untouched.
    #[test]
    fn the_cap_evicts_the_oldest() {
        let mut c = AnchorCache::default();
        for i in 0..=ANCHORS_MAX {
            c.remember(
                &vpd(&format!("file:///d{i}")),
                Some(DirAnchor::new(format!("{i:032x}"))),
            );
        }
        assert_eq!(c.get(&vpd("file:///d0")), None, "the first one is gone");
        assert!(c.get(&vpd(&format!("file:///d{ANCHORS_MAX}"))).is_some());
    }

    /// And refreshing SAVES it: it's LRU, not FIFO. With FIFO, the active
    /// panel's directory would get evicted at 64 distinct directories even
    /// while it was being relisted the whole time, and the next write's
    /// check would disappear without saying anything.
    #[test]
    fn refreshing_saves_from_eviction() {
        let mut c = AnchorCache::default();
        let panel = vpd("file:///panel");
        c.remember(&panel, Some(DirAnchor::new("a".repeat(32))));
        for i in 0..ANCHORS_MAX {
            // Each round relists the panel, like a real refresh does.
            c.remember(&panel, Some(DirAnchor::new("a".repeat(32))));
            c.remember(
                &vpd(&format!("file:///d{i}")),
                Some(DirAnchor::new(format!("{i:032x}"))),
            );
        }
        assert!(
            c.get(&panel).is_some(),
            "what keeps being looked at is not evicted"
        );
    }

    #[test]
    fn the_same_node_gives_the_same_anchor_and_another_node_does_not() {
        let a = NodeId {
            volume: 7,
            index: 42,
        };
        let b = NodeId {
            volume: 7,
            index: 43,
        };
        assert_eq!(de_nodo(a), de_nodo(a));
        assert_ne!(de_nodo(a), de_nodo(b));
        assert!(casa(&de_nodo(a), a));
        assert!(!casa(&de_nodo(a), b));
    }

    #[test]
    fn the_anchor_does_not_carry_the_inode_or_the_volume_inside() {
        // The case that makes the test interesting: two nodes that only
        // differ in the volume. If the anchor carried the numbers, one
        // would be a prefix of or neighbor to the other.
        let a = NodeId {
            volume: 1,
            index: 999_999,
        };
        let b = NodeId {
            volume: 2,
            index: 999_999,
        };
        let (x, y) = (de_nodo(a), de_nodo(b));
        assert_ne!(x, y);
        assert!(
            !x.as_str().contains("999999"),
            "does not carry the index inside"
        );
        assert!(x.is_well_formed() && y.is_well_formed());
    }

    #[test]
    fn a_malformed_anchor_matches_nothing() {
        let id = NodeId {
            volume: 3,
            index: 3,
        };
        assert!(!casa(&DirAnchor::new(String::new()), id));
        assert!(!casa(&DirAnchor::new("../etc".to_owned()), id));
        // Nor does the one someone would forge without the secret: the
        // bare hash of the pair, which is what someone who knows the
        // format would come up with.
        let no_secret = {
            use sha2::{Digest as _, Sha256};
            let mut h = Sha256::new();
            h.update(id.volume.to_le_bytes());
            h.update(id.index.to_le_bytes());
            let d = h.finalize();
            let mut hex = String::new();
            for b in &d[..16] {
                use std::fmt::Write as _;
                let _ = write!(hex, "{b:02x}");
            }
            DirAnchor::new(hex)
        };
        assert!(
            !casa(&no_secret, id),
            "without the secret, nothing is forged"
        );
    }
}
