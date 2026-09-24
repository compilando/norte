//! `norte audit` (M3-5, ADR 0025): chain + anchors + export.

use std::process::ExitCode;

use anyhow::Context;

use crate::cmd::entorno::append_line_0600;
use crate::{AuditCmd, AuditFormat};

/// `norte audit <verify|export|anchor>` (M3-5, ADR 0025): operates on the
/// journal's DB in READ-ONLY. With the daemon running, `SQLite` returns
/// `database is locked` (its lock is exclusive): the message says so plainly.
///
/// Since #167 the daemon is not the only one that can hold it: an embedded
/// frontend — an `ntc` without `--daemon` — takes the same lock. But only
/// FROM THE MOMENT it MUTATES something (#177): an `ntc` just browsing does
/// not get in this command's way, and that is why the help text does not
/// tell you to close the frontends, only says who can hold it.
pub(crate) async fn audit_cmd(cmd: AuditCmd) -> anyhow::Result<ExitCode> {
    use norte_core::{Journal, audit};
    let dir = norte_core::connect::config_dir();
    let journal_path = dir.join("journal.db");
    let anchors_path = dir.join("journal-anchors.jsonl");
    // The MARKER's anchors live in their OWN file (#146), not as one more
    // line among the head's. The reason is compatibility and of the kind
    // that is paid for dearly: `verify_anchors` looks up each anchored `seq`
    // in the map the caller passes it, and a binary OLDER than this change
    // does not seed `seq` 0 — so it would read the marker's line as
    // `MissingSeq`, i.e. "the anchored seq no longer exists: truncation or
    // rollback". A FALSE accusation of tampering against a file nobody
    // touched, raised by the very ADR fix whose purpose is to not raise
    // exactly that. And it is not fixed with another line shape either: that
    // verifier fails closed on anything it does not understand, so a
    // different JSON would still come out as `BadLine`.
    //
    // With two files, an old binary simply does not open it: it gains
    // nothing from the new coverage — which it did not have either — and
    // loses nothing.
    let marker_anchors_path = dir.join("journal-marker-anchors.jsonl");
    let journal = Journal::open_read_only(&journal_path)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-audit-open-failed"))?;
    match cmd {
        AuditCmd::Export { format } => {
            let entries = journal
                .entries()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context(norte_i18n::t("cli-audit-open-failed"))?;
            let out = match format {
                AuditFormat::Jsonl => audit::export_jsonl(&entries),
                AuditFormat::Csv => audit::export_csv(&entries),
            };
            print!("{out}");
            Ok(ExitCode::SUCCESS)
        }
        AuditCmd::Anchor => {
            // A chain this binary could not verify is NEVER anchored: the
            // anchor would fix as "good" either a broken history, or one it
            // does not know how to read (ADR 0046).
            let status = journal
                .verify_chain()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if !status.is_intact() {
                let declared = journal.format().await.map_err(|e| anyhow::anyhow!("{e}"))?;
                report_chain_not_certified(&status, declared);
                return Ok(ExitCode::FAILURE);
            }
            let head = journal.head().await.map_err(|e| anyhow::anyhow!("{e}"))?;
            // The FORMAT MARKER (`seq 0`) is anchored too, and FIRST (#146).
            // ADR 0046 granted that re-declaring it costs three column writes
            // and no key, and that the head's anchors do not catch it because
            // that edit does not move any `entry_hash` of `seq >= 1`. Signing
            // it separately turns that re-declaration into a `HashMismatch`
            // at `seq` 0: localized, and with a key behind it.
            //
            // It goes before the head's so that a journal that only has a
            // marker — just created, without a single mutation — is covered
            // too; there `head()` is `None` and the function returns below.
            let marker = journal
                .marker_hash()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if marker.is_none() && head.is_none() {
                println!("{}", norte_i18n::t("cli-audit-empty"));
                return Ok(ExitCode::SUCCESS);
            }
            // The keyring key can block (D-Bus/prompt): outside the reactor
            // (rule 2).
            let key = tokio::task::spawn_blocking(norte_core::connect::journal_anchor_key)
                .await
                .map_err(|_| anyhow::anyhow!("keyring task panicked"))?
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            // The lines go out over stdout ON PURPOSE: the EXTERNAL copy of
            // the anchors (remote log, another host) is what makes trimming
            // the local file detectable (ADR 0025).
            if let Some(head) = marker {
                let line = audit::anchor_line(&key, &audit::Anchor { seq: 0, head });
                // The marker's anchor is DETERMINISTIC: the marker never
                // changes, so anchoring ten times would write ten identical
                // lines and the report would count ten verified anchors
                // where there is one. It is written only if not already
                // there.
                if !already_anchored(&marker_anchors_path, &line).await? {
                    append_line_0600(&marker_anchors_path, &line).await?;
                }
                println!("{line}");
                println!("{}", norte_i18n::t("cli-audit-anchored-marker"));
            }
            let Some((seq, head)) = head else {
                // And not "empty journal: nothing to anchor", which would
                // contradict the line above on the very same screen.
                println!("{}", norte_i18n::t("cli-audit-only-marker"));
                return Ok(ExitCode::SUCCESS);
            };
            let line = audit::anchor_line(&key, &audit::Anchor { seq, head });
            append_line_0600(&anchors_path, &line).await?;
            println!("{line}");
            println!(
                "{}",
                norte_i18n::ta("cli-audit-anchored", &[("seq", &seq.to_string())])
            );
            Ok(ExitCode::SUCCESS)
        }
        AuditCmd::Verify { allow_no_anchors } => {
            audit_verify(
                &journal,
                &anchors_path,
                &marker_anchors_path,
                allow_no_anchors,
            )
            .await
        }
    }
}

/// Why the chain was NOT certified, in the voice that fits: a break is an
/// accusation and cites where; an unknown format (ADR 0046) is NOT one — this
/// binary does not know how to recompute what a newer one wrote — and is
/// stated without accusing anyone, but also without clearing anyone: in both
/// cases the audit exits with FAILURE.
///
/// Goes over STDOUT, same as `cli-audit-chain-ok`: the verdict IS the audit's
/// OUTPUT, not a stray diagnostic, and a `norte audit verify > report.txt`
/// that keeps the coverage and the anchors but not the verdict is exactly the
/// file that must not be produced. The failure is carried by the exit code.
///
/// `declared` comes from the journal because the `Broken` verdict does not
/// carry it: a broken chain IN a journal that is also written in an
/// unreadable format is a break that has to be read in that light.
fn report_chain_not_certified(
    status: &norte_core::ChainStatus,
    declared: norte_core::JournalFormat,
) {
    use norte_core::{ChainStatus, JournalFormat};
    // `Unmarked` does not arrive via the unknown-format branch (a journal
    // without a marker is verified with today's rules), and any future
    // variant is, by definition, something this binary does not know how to
    // read.
    let name = |f: JournalFormat| match f {
        JournalFormat::Version(v) => v.to_string(),
        _ => norte_i18n::t("cli-audit-format-unreadable"),
    };
    let unknown_format = |declared: JournalFormat, known: u32| {
        println!(
            "{}",
            norte_i18n::ta(
                "cli-audit-chain-unknown-format",
                &[("declared", &name(declared)), ("known", &known.to_string())],
            )
        );
    };
    match status {
        ChainStatus::Broken { first_bad_seq } => {
            if declared.is_unknown() {
                unknown_format(declared, norte_core::JOURNAL_FORMAT);
            }
            println!(
                "{}",
                norte_i18n::ta(
                    "cli-audit-chain-broken",
                    &[("seq", &first_bad_seq.to_string())],
                )
            );
        }
        ChainStatus::UnknownFormat {
            declared,
            known,
            first_unverifiable_seq,
        } => {
            unknown_format(*declared, *known);
            if let Some(seq) = first_unverifiable_seq {
                println!(
                    "{}",
                    norte_i18n::ta(
                        "cli-audit-chain-unverifiable-from",
                        &[("seq", &seq.to_string())],
                    )
                );
            }
        }
        // A verdict this binary does not know is treated as NOT certified.
        // `ChainStatus` is `#[non_exhaustive]` precisely so a new verdict
        // lands here instead of slipping through the "intact" branch.
        _ => println!("{}", norte_i18n::t("cli-audit-chain-not-certified")),
    }
}

/// `norte audit verify`: chain (cites the first break, B2) + anchors +
/// COVERAGE (up to which seq the anchors reach Ok — trimming the anchors
/// file shows up as coverage moving backwards). No anchors = FAILURE unless
/// `--allow-no-anchors`: absence is indistinguishable from a hostile deletion
/// (H1 from the security-reviewer).
async fn audit_verify(
    journal: &norte_core::Journal,
    anchors_path: &std::path::Path,
    marker_anchors_path: &std::path::Path,
    allow_no_anchors: bool,
) -> anyhow::Result<ExitCode> {
    use norte_core::{ChainStatus, audit};
    let status = journal
        .verify_chain()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // A BROKEN chain already carries its culprit and its spot: there is no
    // second opinion to seek. One this binary DOES NOT KNOW HOW TO READ (ADR
    // 0046) is the opposite — the anchors are the only evidence that tells
    // "newer journal" apart from "re-declared marker", they do not need to
    // recompute the chain (they compare STORED hashes) and the operator
    // already has them on disk. So it keeps going to the anchors report and
    // still exits with FAILURE.
    let certified = match status {
        ChainStatus::Intact { entries } => {
            println!(
                "{}",
                norte_i18n::ta("cli-audit-chain-ok", &[("entries", &entries.to_string())])
            );
            true
        }
        ChainStatus::UnknownFormat { .. } => {
            report_chain_not_certified(&status, declared_format(journal).await?);
            false
        }
        _ => {
            report_chain_not_certified(&status, declared_format(journal).await?);
            return Ok(ExitCode::FAILURE);
        }
    };
    let head_seq = journal
        .head()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .map(|(seq, _)| seq);
    // The key BEFORE deciding anything about the head's anchors: the marker
    // is checked no matter what happens with them, and without the key
    // neither family can be checked.
    let key = tokio::task::spawn_blocking(norte_core::connect::journal_anchor_key)
        .await
        .map_err(|_| anyhow::anyhow!("keyring task panicked"))?
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let lines = match tokio::fs::read_to_string(&anchors_path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // The MARKER is checked just the same, and this is why its check
            // lives before this early return: a journal with a marker and no
            // mutations has a marker anchor and NONE for the head, and
            // without this, `anchor` and `verify` would contradict each
            // other within the same commit — one would write the anchor and
            // the other would never look at it. It is also the state left by
            // an attacker who deletes the head's anchors file: the marker's
            // signal is all that remains.
            let marker_ok = verify_marker_anchors(journal, marker_anchors_path, &key).await?;
            let msg = norte_i18n::t("cli-audit-no-anchors");
            if allow_no_anchors {
                println!("{msg}");
                // `--allow-no-anchors` forgives the ABSENCE of head anchors,
                // not an uncertified chain nor an unanchored marker.
                return Ok(if marker_ok {
                    exit_for(certified)
                } else {
                    ExitCode::FAILURE
                });
            }
            // Absence = failure by default: an attacker without the key can
            // DELETE the file; only the human decides that "there is none"
            // is fine.
            eprintln!("{msg}");
            return Ok(ExitCode::FAILURE);
        }
        Err(e) => return Err(e).context("journal-anchors.jsonl"),
    };
    // ONE snapshot of the chain for the whole verdict (no TOCTOU between the
    // verify above and the anchor comparisons).
    let hash_by_seq: std::collections::HashMap<i64, [u8; 32]> = journal
        .entries()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .into_iter()
        .filter_map(|e| e.entry_hash.try_into().ok().map(|h: [u8; 32]| (e.seq, h)))
        .collect();

    let report = audit::verify_anchors(&key, &lines, &hash_by_seq);
    report_anchors(&report, head_seq, certified);
    let marker_ok = verify_marker_anchors(journal, marker_anchors_path, &key).await?;
    if !report.bad.is_empty() || !marker_ok {
        return Ok(ExitCode::FAILURE);
    }
    if certified {
        println!(
            "{}",
            norte_i18n::ta(
                "cli-audit-anchors-ok",
                &[("count", &report.checked.to_string())],
            )
        );
    }
    Ok(exit_for(certified))
}

/// Checks the MARKER's anchors (#146) and says whether the marker was left
/// UNANCHORED. `false` = there is something to report and the command exits
/// with failure.
///
/// # Why "unanchored" is its own line and not silence
/// ADR 0025's defense against trimming the anchors file is that COVERAGE
/// moves backwards, and that only works for the TAIL. The marker's anchor is
/// the one with the lowest `seq` that exists, so deleting it — or deleting
/// its whole file — does not move `max_ok_seq` a single digit: the report
/// would say nothing. The attacker's recipe would go from three writes to
/// four over files it can already write.
///
/// The same line covers the other gap, and it cannot be closed any other
/// way: a journal WITHOUT a marker (all the ones that existed before ADR
/// 0046, which by design never gain one) allows one to be INJECTED into it —
/// inserting the `seq` 0 row and re-chaining `seq` 1 — and that turns a
/// localized `Broken` into `UnknownFormat` just like the re-declaration.
/// There is no prior anchor to contradict there, because there was no marker
/// when it was anchored. What CAN be said is that there is a marker NOW and
/// nobody has anchored it, which is exactly what an injected marker
/// produces.
///
/// # Errors
/// Reading the marker's anchors file, or the journal.
async fn verify_marker_anchors(
    journal: &norte_core::Journal,
    path: &std::path::Path,
    key: &[u8],
) -> anyhow::Result<bool> {
    use norte_core::audit;
    let Some(marker) = journal
        .marker_hash()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
    else {
        // Without a marker there is nothing to anchor and nothing to
        // report. A marker anchors file OVER a journal without a marker
        // would indeed be odd, but that is the case below (`MissingSeq`) and
        // counts as bad.
        return Ok(true);
    };
    let lines = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).context("journal-marker-anchors.jsonl"),
    };
    let snapshot: std::collections::HashMap<i64, [u8; 32]> = std::iter::once((0, marker)).collect();
    let report = audit::verify_anchors(key, &lines, &snapshot);
    for (line_no, verdict) in &report.bad {
        let Some(detail) = verdict_detail(verdict) else {
            continue;
        };
        eprintln!(
            "{}",
            norte_i18n::ta(
                "cli-audit-anchor-bad",
                &[("line", &line_no.to_string()), ("detail", &detail)],
            )
        );
    }
    if report.max_ok_seq.is_none() {
        eprintln!("{}", norte_i18n::t("cli-audit-marker-unanchored"));
        return Ok(false);
    }
    println!("{}", norte_i18n::t("cli-audit-marker-ok"));
    Ok(report.bad.is_empty())
}

/// Is that EXACT line already in the file? Avoids duplicating an anchor that
/// is deterministic (the marker's, which never changes).
///
/// # Errors
/// Reading the file, except for its absence.
async fn already_anchored(path: &std::path::Path, line: &str) -> anyhow::Result<bool> {
    match tokio::fs::read_to_string(path).await {
        Ok(s) => Ok(s.lines().any(|l| l == line)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).context("journal-marker-anchors.jsonl"),
    }
}

/// The anchors report: the bad ones one by one, the caveat when the chain
/// was NOT certified, and the coverage.
///
/// Order matters. "Anchored against the chain" presupposes a verified chain;
/// if this binary could not verify it, what the anchors say is a DIFFERENT
/// sentence — the stored hashes have not moved since it was anchored — and
/// that caveat goes BEFORE the coverage, because "up to seq 100 of 100" read
/// without it is the line the operator will cite as a clean bill of health.
fn report_anchors(
    report: &norte_core::audit::AnchorsReport,
    head_seq: Option<i64>,
    certified: bool,
) {
    for (line_no, verdict) in &report.bad {
        let Some(detail) = verdict_detail(verdict) else {
            continue;
        };
        eprintln!(
            "{}",
            norte_i18n::ta(
                "cli-audit-anchor-bad",
                &[("line", &line_no.to_string()), ("detail", &detail)],
            )
        );
    }
    if !certified {
        println!(
            "{}",
            norte_i18n::ta(
                "cli-audit-anchors-ok-unverified-chain",
                &[("count", &report.checked.to_string())],
            )
        );
    }
    // Coverage ALWAYS visible: anchors up to X, chain up to Y. Trimming the
    // anchors file moves X backwards without touching the chain.
    println!(
        "{}",
        norte_i18n::ta(
            "cli-audit-coverage",
            &[
                (
                    "anchored",
                    &report
                        .max_ok_seq
                        .map_or_else(|| "-".into(), |s| s.to_string()),
                ),
                (
                    "head",
                    &head_seq.map_or_else(|| "-".into(), |s| s.to_string()),
                ),
            ],
        )
    );
}

/// The format the journal DECLARES, to put the verdict in context.
async fn declared_format(
    journal: &norte_core::Journal,
) -> anyhow::Result<norte_core::JournalFormat> {
    journal.format().await.map_err(|e| anyhow::anyhow!("{e}"))
}

/// Success only if the chain was certified.
fn exit_for(certified: bool) -> ExitCode {
    if certified {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Translates a NON-Ok anchor verdict to its Fluent message.
fn verdict_detail(verdict: &norte_core::audit::AnchorVerdict) -> Option<String> {
    use norte_core::audit::AnchorVerdict;
    Some(match verdict {
        AnchorVerdict::BadLine => norte_i18n::t("cli-audit-verdict-bad-line"),
        AnchorVerdict::BadMac => norte_i18n::t("cli-audit-verdict-bad-mac"),
        AnchorVerdict::MissingSeq(a) => {
            norte_i18n::ta("cli-audit-verdict-missing", &[("seq", &a.seq.to_string())])
        }
        AnchorVerdict::HashMismatch(a) => {
            norte_i18n::ta("cli-audit-verdict-mismatch", &[("seq", &a.seq.to_string())])
        }
        AnchorVerdict::Ok(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR 0046: every verdict that is not `Intact` has something to say in
    /// both locales, and the "unknown format" one says the version WITHOUT
    /// accusing anyone. A missing Fluent key falls back to the key itself,
    /// which in this path would be the whole message the operator gets.
    #[test]
    fn every_uncertified_verdict_has_its_message_in_both_languages() {
        use norte_core::{ChainStatus, JournalFormat};
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            let unknown = norte_i18n::ta_in(
                lang,
                "cli-audit-chain-unknown-format",
                &[("declared", "999"), ("known", "1")],
            );
            assert!(unknown.contains("999"), "{lang:?}: {unknown}");
            assert!(!unknown.starts_with("cli-audit"), "{lang:?}: untranslated");
            for key in [
                "cli-audit-chain-unverifiable-from",
                "cli-audit-chain-not-certified",
                "cli-audit-format-unreadable",
            ] {
                let msg = norte_i18n::ta_in(lang, key, &[("seq", "7")]);
                assert!(
                    !msg.starts_with("cli-audit"),
                    "{lang:?}/{key}: untranslated"
                );
            }
        }
        // And it does not panic on any shape of the verdict (including the
        // fail-closed branch, which is what a future verdict will hit).
        for status in [
            ChainStatus::Broken { first_bad_seq: 3 },
            ChainStatus::UnknownFormat {
                declared: JournalFormat::Version(999),
                known: norte_core::JOURNAL_FORMAT,
                first_unverifiable_seq: Some(1),
            },
            ChainStatus::UnknownFormat {
                declared: JournalFormat::Unreadable,
                known: norte_core::JOURNAL_FORMAT,
                first_unverifiable_seq: None,
            },
        ] {
            report_chain_not_certified(&status, JournalFormat::Version(999));
        }
        // And a break in a journal whose format also cannot be read says
        // BOTH things: the break is real, and without the caveat it cannot
        // be interpreted.
        report_chain_not_certified(
            &ChainStatus::Broken { first_bad_seq: 3 },
            JournalFormat::Unreadable,
        );
    }
}
