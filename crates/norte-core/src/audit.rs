//! Journal audit (M3-5, ADR 0025): deterministic export (JSONL/CSV) and HMAC
//! anchors for the chain's head.
//!
//! **Honest guarantee for the anchors.** The journal's keyless hash chain
//! does not withstand an attacker with write access to the DB (#63): full
//! rewrite, tail truncation and rollback all pass `verify_chain`. An anchor
//! `HMAC-SHA256(key, "norte-anchor-v1" ‖ seq ‖ head)` with the key in the
//! OS keyring bounds that window: REWRITING history covered by an anchor
//! ALSO requires the key. What the key does NOT protect: the anchors file
//! itself lives in the same dir — an attacker with file write access can
//! DELETE it, trim lines off it (the earlier ones are still valid MACs), or
//! restore a coherent snapshot of the PAIR (DB + anchors), all WITHOUT the
//! key. That's why `verify` reports the COVERAGE (up to which seq the
//! anchors reach) and treats absent anchors as a failure unless explicitly
//! opted out; the EXTERNAL copy of the anchors file (another host, a remote
//! log) is what makes the trim detectable. Also not covered: an attacker
//! with keyring access, or mutations after the last anchor. The core does
//! NOT know the keyring: the key comes in as bytes (resolved by the CLI via
//! `norte-connect`, rule 10).

use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;

use crate::hashing::hex_lower as hex;
use crate::journal::JournalEntry;

type HmacSha256 = Hmac<Sha256>;

/// An anchor: the chain's `(seq, entry_hash)` head at the moment it was
/// anchored. Persisted as ONE JSON line in `journal-anchors.jsonl`
/// (append-only) alongside its MAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// `seq` of the anchored head.
    pub seq: i64,
    /// `entry_hash` of the anchored head.
    pub head: [u8; 32],
}

/// Verdict for ONE anchor line against the current chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorVerdict {
    /// Valid MAC and the chain's hash at that `seq` matches.
    Ok(Anchor),
    /// The line doesn't parse (corrupted file or a future format).
    BadLine,
    /// Invalid MAC: this key did not produce the anchor (forged, or the key
    /// rotated without re-anchoring).
    BadMac,
    /// The chain NO LONGER HAS that `seq`: truncation/rollback behind the
    /// anchor.
    MissingSeq(Anchor),
    /// The `seq` exists but its hash differs: history was rewritten.
    HashMismatch(Anchor),
}

/// The MAC's context string: domain separation + format version. If the key
/// were ever reused for another MAC, or the format changes, there's no
/// cross-protocol confusion or ambiguous migration.
const ANCHOR_CONTEXT: &[u8] = b"norte-anchor-v1";

/// `HMAC-SHA256(key, "norte-anchor-v1" ‖ seq_le ‖ head)`. FIXED-length
/// fields (15+8+32): no concatenation ambiguity. Noted debt: with no
/// journal identity in the MAC, an anchor from ANOTHER profile
/// (`$NORTE_CONFIG_DIR`) of the same user is MAC-valid against this one —
/// it produces `MissingSeq`/`HashMismatch` (a false alarm), not a bypass.
fn anchor_mac(key: &[u8], seq: i64, head: &[u8; 32]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(ANCHOR_CONTEXT);
    mac.update(&seq.to_le_bytes());
    mac.update(head);
    mac.finalize().into_bytes().into()
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
        let hi = char::from(chunk[0]).to_digit(16)?;
        let lo = char::from(chunk[1]).to_digit(16)?;
        out[i] = u8::try_from(hi * 16 + lo).ok()?;
    }
    Some(out)
}

/// Serializes an anchor as a JSONL line with its MAC (no trailing `\n`).
#[must_use]
pub fn anchor_line(key: &[u8], anchor: &Anchor) -> String {
    let mac = anchor_mac(key, anchor.seq, &anchor.head);
    format!(
        r#"{{"seq":{},"head":"{}","mac":"{}"}}"#,
        anchor.seq,
        hex(&anchor.head),
        hex(&mac)
    )
}

/// Verifies ONE anchor line: parse → MAC → compare against the hash the
/// chain has TODAY at that `seq` (`None` = the seq no longer exists).
///
/// The MAC is checked BEFORE looking at the chain: a line forged without
/// the key never gets to accuse the chain of anything.
///
/// # Panics
/// Never: HMAC accepts keys of any length (the `expect` documents that
/// invariant of `RustCrypto`'s constructor API).
#[must_use]
pub fn verify_anchor_line(key: &[u8], line: &str, hash_at_seq: Option<[u8; 32]>) -> AnchorVerdict {
    #[derive(serde::Deserialize)]
    struct Line {
        seq: i64,
        head: String,
        mac: String,
    }
    let Ok(parsed) = serde_json::from_str::<Line>(line) else {
        return AnchorVerdict::BadLine;
    };
    let (Some(head), Some(mac)) = (unhex::<32>(&parsed.head), unhex::<32>(&parsed.mac)) else {
        return AnchorVerdict::BadLine;
    };
    // Constant-time comparison (hmac::Mac::verify_slice).
    let mut check = HmacSha256::new_from_slice(key).expect("HMAC accepts any length");
    check.update(ANCHOR_CONTEXT);
    check.update(&parsed.seq.to_le_bytes());
    check.update(&head);
    if check.verify_slice(&mac).is_err() {
        return AnchorVerdict::BadMac;
    }
    let anchor = Anchor {
        seq: parsed.seq,
        head,
    };
    match hash_at_seq {
        None => AnchorVerdict::MissingSeq(anchor),
        Some(h) if h == head => AnchorVerdict::Ok(anchor),
        Some(_) => AnchorVerdict::HashMismatch(anchor),
    }
}

/// Result of verifying the WHOLE anchors file against a snapshot of the
/// chain (rule 7: orchestration lives in the core, the CLI only translates).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnchorsReport {
    /// Anchors reviewed (non-empty lines).
    pub checked: u64,
    /// NON-Ok verdicts, with their line number (1-indexed).
    pub bad: Vec<(usize, AnchorVerdict)>,
    /// Highest `seq` among the Ok anchors: the anchors' coverage reaches
    /// THAT FAR. Everything later in the chain is UNANCHORED — and a trim of
    /// the anchors file shows up as coverage moving backward (that's why
    /// `verify` ALWAYS prints it).
    pub max_ok_seq: Option<i64>,
}

/// Verifies each line of `anchors_text` against a SNAPSHOT of the chain (a
/// `seq → entry_hash` map, taken from [`crate::Journal::entries`] after a
/// [`crate::Journal::verify_chain`] `Intact` — one read, one coherent
/// verdict). The MAC decides BEFORE consulting the snapshot: a line forged
/// without the key never gets to accuse the chain of anything.
#[must_use]
pub fn verify_anchors<S: std::hash::BuildHasher>(
    key: &[u8],
    anchors_text: &str,
    hash_by_seq: &std::collections::HashMap<i64, [u8; 32], S>,
) -> AnchorsReport {
    let mut report = AnchorsReport::default();
    for (idx, line) in anchors_text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        report.checked += 1;
        // The lookup happens after the MAC is validated inside
        // verify_anchor_line (the closure below only runs for lines with a
        // parsed seq; the map access is harmless, the VERDICT requires a
        // valid MAC first).
        let seq = serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|v| v["seq"].as_i64());
        let at = seq.and_then(|s| hash_by_seq.get(&s).copied());
        match verify_anchor_line(key, line, at) {
            AnchorVerdict::Ok(a) => {
                report.max_ok_seq = Some(report.max_ok_seq.map_or(a.seq, |m| m.max(a.seq)));
            }
            verdict => report.bad.push((idx + 1, verdict)),
        }
    }
    report
}

/// An export row: a [`JournalEntry`]'s fields in a stable shape. The
/// journal's paths are `to_wire` bytes (rule 1) — normally UTF-8 (the wire
/// form is percent-encoded); if a corrupted blob isn't, the field goes as
/// `<field>_hex` and the normal one stays `null`, never silently lossy.
#[derive(Serialize)]
struct AuditRow<'a> {
    seq: i64,
    ts_ms: i64,
    actor_kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor_id: Option<&'a str>,
    op: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_to: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_to_hex: Option<String>,
    reversal: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    undoes_seq: Option<i64>,
    entry_hash: String,
    /// LAST, and omitted when there's no batch: an old export and a new one
    /// of the same standalone entries stay identical. Without it, forty
    /// renames from ONE agent action read as forty independent actions,
    /// which is exactly the question an audit exists to answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    batch_id: Option<i64>,
}

/// Wire-bytes field: `(utf8, hex_fallback)`.
fn wire_field(bytes: &[u8]) -> (Option<&str>, Option<String>) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (Some(s), None),
        Err(_) => (None, Some(hex(bytes))),
    }
}

fn row(e: &JournalEntry) -> AuditRow<'_> {
    let (path, path_hex) = wire_field(&e.path);
    let (path_to, path_to_hex) = match &e.path_to {
        None => (None, None),
        Some(b) => wire_field(b),
    };
    AuditRow {
        seq: e.seq,
        ts_ms: e.ts_ms,
        actor_kind: &e.actor_kind,
        actor_id: e.actor_id.as_deref(),
        op: &e.op,
        path,
        path_hex,
        path_to,
        path_to_hex,
        reversal: &e.reversal,
        undoes_seq: e.undoes_seq,
        entry_hash: hex(&e.entry_hash),
        batch_id: e.batch_id,
    }
}

/// JSONL export: one JSON line per entry, in `seq` order, with keys in
/// declaration order (stable across runs — fit for diff/signing).
///
/// # Panics
/// Never: a flat struct of strings/integers always serializes (the `expect`
/// documents that invariant).
#[must_use]
pub fn export_jsonl(entries: &[JournalEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        out.push_str(&serde_json::to_string(&row(e)).expect("flat struct serializes"));
        out.push('\n');
    }
    out
}

/// Escapes a CSV field (RFC 4180: doubled quotes; quoted whenever needed)
/// and NEUTRALIZES formulas: the CSV is "for humans" (ADR 0025) — it will
/// be opened in Excel/LibreOffice — and filenames are hostile by design
/// (`=HYPERLINK(...)`, `=cmd|...`); a field starting with `=`/`+`/`-`/`@`/TAB
/// is prefixed with `'` (the standard anti formula-injection convention for
/// audit material).
fn csv_field(s: &str) -> String {
    let neutralized = if s.starts_with(['=', '+', '-', '@', '\t']) {
        format!("'{s}")
    } else {
        s.to_owned()
    };
    if neutralized.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", neutralized.replace('"', "\"\""))
    } else {
        neutralized
    }
}

/// CSV export with a fixed header. Non-UTF-8 paths go in hex with a `hex:`
/// prefix — knowingly AMBIGUOUS: a UTF-8 path that literally starts with
/// `hex:` is indistinguishable (unlikely: the wire form starts with a
/// whitelisted scheme). For machine consumption use the JSONL, which keeps
/// `path`/`path_hex` in separate fields.
#[must_use]
pub fn export_csv(entries: &[JournalEntry]) -> String {
    // `batch_id` is APPENDED at the end: existing columns don't move, so a
    // by-position consumer keeps reading the same thing.
    let mut out = String::from(
        "seq,ts_ms,actor_kind,actor_id,op,path,path_to,reversal,undoes_seq,entry_hash,batch_id\n",
    );
    let wire = |b: &[u8]| match std::str::from_utf8(b) {
        Ok(s) => s.to_owned(),
        Err(_) => format!("hex:{}", hex(b)),
    };
    for e in entries {
        let cols = [
            e.seq.to_string(),
            e.ts_ms.to_string(),
            e.actor_kind.clone(),
            e.actor_id.clone().unwrap_or_default(),
            e.op.clone(),
            wire(&e.path),
            e.path_to.as_deref().map(wire).unwrap_or_default(),
            e.reversal.clone(),
            e.undoes_seq.map(|s| s.to_string()).unwrap_or_default(),
            hex(&e.entry_hash),
            e.batch_id.map(|b| b.to_string()).unwrap_or_default(),
        ];
        let line: Vec<String> = cols.iter().map(|c| csv_field(c)).collect();
        out.push_str(&line.join(","));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: i64, path: &[u8]) -> JournalEntry {
        JournalEntry {
            seq,
            ts_ms: 1_700_000_000_000 + seq,
            entry_hash: vec![u8::try_from(seq).unwrap_or(0); 32],
            actor_kind: "agent".into(),
            actor_id: Some("claude".into()),
            op: "created".into(),
            path: path.to_vec(),
            path_to: None,
            reversal: "delete".into(),
            reversal_ref: None,
            undoes_seq: None,
            batch_id: None,
        }
    }

    #[test]
    fn jsonl_is_stable_and_one_line_per_entry() {
        let e = [entry(1, b"mem:///a.txt"), entry(2, b"mem:///b.txt")];
        let out = export_jsonl(&e);
        assert_eq!(out.lines().count(), 2);
        let first: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert_eq!(first["seq"], 1);
        assert_eq!(first["path"], "mem:///a.txt");
        assert_eq!(first["actor_id"], "claude");
        assert_eq!(out, export_jsonl(&e), "deterministic");
    }

    #[test]
    fn jsonl_non_utf8_bytes_go_in_hex_never_lossy() {
        let e = [entry(1, b"\xff\xfe")];
        let v: serde_json::Value =
            serde_json::from_str(export_jsonl(&e).lines().next().unwrap()).unwrap();
        assert!(v.get("path").is_none(), "no lossy path");
        assert_eq!(v["path_hex"], "fffe");
    }

    /// The batch SHOWS UP in both exports: without it, n renames from ONE
    /// agent action read as n independent actions. Absent ⇒ absent (JSONL)
    /// and empty (CSV), so an export of standalone entries doesn't change.
    #[test]
    fn the_batch_shows_up_in_both_exports_and_absent_changes_nothing() {
        let standalone = entry(1, b"mem:///a.txt");
        let mut grouped = entry(2, b"mem:///b.txt");
        grouped.batch_id = Some(7);
        let e = [standalone, grouped];

        let jsonl = export_jsonl(&e);
        let mut lines = jsonl.lines();
        let v0: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        let v1: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert!(v0.get("batch_id").is_none(), "no batch, no key");
        assert_eq!(v1["batch_id"], 7);

        let csv = export_csv(&e);
        let header = csv.lines().next().unwrap();
        assert!(header.ends_with(",batch_id"), "column at the end: {header}");
        assert!(
            csv.lines().nth(1).unwrap().ends_with(','),
            "no batch, empty"
        );
        assert!(csv.lines().nth(2).unwrap().ends_with(",7"));
    }

    #[test]
    fn csv_escapes_commas_and_quotes() {
        let e = [entry(1, br#"mem:///a,"b".txt"#)];
        let out = export_csv(&e);
        let data = out.lines().nth(1).unwrap();
        assert!(data.contains(r#""mem:///a,""b"".txt""#), "{data}");
    }

    #[test]
    fn anchor_round_trip_and_forged_mac_is_rejected() {
        let key = [7u8; 32];
        let a = Anchor {
            seq: 5,
            head: [9u8; 32],
        };
        let line = anchor_line(&key, &a);
        assert_eq!(
            verify_anchor_line(&key, &line, Some(a.head)),
            AnchorVerdict::Ok(a)
        );
        // Another key does NOT produce the same valid line.
        assert_eq!(
            verify_anchor_line(&[8u8; 32], &line, Some(a.head)),
            AnchorVerdict::BadMac
        );
        // Line forged without the key: invalid MAC before looking at anything.
        let forged = line.replace("\"seq\":5", "\"seq\":6");
        assert_eq!(
            verify_anchor_line(&key, &forged, Some(a.head)),
            AnchorVerdict::BadMac
        );
    }

    #[test]
    fn anchor_detects_truncation_and_rewrite() {
        let key = [7u8; 32];
        let a = Anchor {
            seq: 5,
            head: [9u8; 32],
        };
        let line = anchor_line(&key, &a);
        // Truncation/rollback: the chain no longer reaches the anchored seq.
        assert_eq!(
            verify_anchor_line(&key, &line, None),
            AnchorVerdict::MissingSeq(a)
        );
        // Rewrite: the seq exists with ANOTHER hash.
        assert_eq!(
            verify_anchor_line(&key, &line, Some([1u8; 32])),
            AnchorVerdict::HashMismatch(a)
        );
    }

    #[test]
    fn unreadable_line_is_bad_line() {
        assert_eq!(
            verify_anchor_line(&[7u8; 32], "no-json", None),
            AnchorVerdict::BadLine
        );
        assert_eq!(
            verify_anchor_line(&[7u8; 32], r#"{"seq":1,"head":"short","mac":"00"}"#, None),
            AnchorVerdict::BadLine
        );
    }

    /// GOLDEN for the persisted line format: fixed key/seq/head → EXACT
    /// line. If this changes, the existing anchors file stops verifying:
    /// requires bumping the context string ("norte-anchor-v2").
    #[test]
    fn golden_anchor_line_format() {
        let line = anchor_line(
            &[7u8; 32],
            &Anchor {
                seq: 5,
                head: [9u8; 32],
            },
        );
        assert_eq!(
            line,
            "{\"seq\":5,\"head\":\"0909090909090909090909090909090909090909090909090909090909090909\",\"mac\":\"b63195ea067b1bf43048bec2b4e3f2707984f8a9680f131cad30b0a44f7a6e1e\"}",
        );
    }

    #[test]
    fn csv_neutralizes_formulas() {
        let mut e = entry(1, b"=HYPERLINK(\"http://evil\")");
        e.actor_id = Some("-2-2".into());
        let out = export_csv(&[e]);
        let data = out.lines().nth(1).unwrap();
        assert!(
            data.contains("'=HYPERLINK"),
            "path formula neutralized: {data}"
        );
        assert!(
            data.contains(",'-2-2,"),
            "actor_id chosen by the agent too: {data}"
        );
    }

    #[test]
    fn verify_anchors_reports_coverage_and_bad_ones() {
        let key = [7u8; 32];
        let a1 = Anchor {
            seq: 1,
            head: [1u8; 32],
        };
        let a2 = Anchor {
            seq: 3,
            head: [3u8; 32],
        };
        let text = format!(
            "{}\n\n{}\ngarbage\n",
            anchor_line(&key, &a1),
            anchor_line(&key, &a2)
        );
        let mut chain = std::collections::HashMap::new();
        chain.insert(1, [1u8; 32]);
        chain.insert(3, [3u8; 32]);
        let r = verify_anchors(&key, &text, &chain);
        assert_eq!(r.checked, 3, "empty ones don't count");
        assert_eq!(r.max_ok_seq, Some(3), "coverage = highest Ok seq");
        assert_eq!(r.bad, vec![(4, AnchorVerdict::BadLine)]);
        // Trimming the pair: the chain fell back to seq<3 -> MissingSeq.
        chain.remove(&3);
        let r = verify_anchors(&key, &text, &chain);
        assert_eq!(r.max_ok_seq, Some(1), "coverage MOVES BACKWARD — visible");
        assert!(matches!(
            r.bad.as_slice(),
            [
                (3, AnchorVerdict::MissingSeq(_)),
                (4, AnchorVerdict::BadLine)
            ]
        ));
    }
}
