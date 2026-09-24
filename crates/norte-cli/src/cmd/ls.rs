//! `norte ls`: lists a directory, in text or `--json`.

use std::process::ExitCode;

use anyhow::Context;
use norte_core::backend::Backend;
use norte_proto::{Entry, EntryKind};

use crate::cmd::connect::{tofu_confirm, vpath};

pub(crate) async fn ls(
    backend: &Backend,
    path: &std::path::Path,
    json: bool,
    attrs: &[String],
) -> anyhow::Result<ExitCode> {
    let target = vpath(path)?;
    // Validation BEFORE the backend (review #108-b2 MAJOR): the daemon
    // rejects malformed/over-cap ids with -32602 but the embedded backend
    // FILTERS — without this gate the same command would behave
    // differently depending on the transport. `escape_debug`: the id is
    // user input but it can come from a hostile script.
    if attrs.len() > norte_proto::ATTRS_MAX_REQUEST {
        anyhow::bail!(
            "--attrs: at most {} ids per call",
            norte_proto::ATTRS_MAX_REQUEST
        );
    }
    if let Some(bad) = attrs.iter().find(|id| !norte_proto::is_valid_attr_id(id)) {
        anyhow::bail!("--attrs: malformed id \"{}\"", bad.escape_debug());
    }
    let (mut entries, skipped): (Vec<Entry>, Option<u64>) =
        match backend.list_with_skipped_attrs(&target, attrs).await {
            // First TOFU contact: confirm and retry ONCE.
            Err(e) if tofu_confirm(backend, &e).await? => {
                backend.list_with_skipped_attrs(&target, attrs).await
            }
            other => other,
        }
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-list-failed"))?;
    // #93: the container omitted entries from its index — the listing is
    // NOT everything the archive contains. To stderr (does not
    // contaminate stdout nor --json).
    if let Some(n) = skipped.filter(|&n| n > 0) {
        eprintln!(
            "{}",
            norte_i18n::ta("cli-ls-skipped", &[("n", &n.to_string())])
        );
    }
    // #52: the local listing is lazy (size/mtime_ms are None). `ls` is a
    // SINGLE-pass command (there is no focus that hydrates later, as in
    // the TUI): it is hydrated here, serially, BEFORE printing — restores
    // the pre-#52 output (text and --json) at the pre-#52 cost (one stat
    // per File) — only for Files: Dir/Symlink emit null in --json (their
    // mtime is not `ls`'s contract). A failed stat leaves `None` (empty
    // column/field): it never aborts the listing.
    for e in &mut entries {
        if e.kind == EntryKind::File
            && (e.size.is_none() || e.mtime_ms.is_none())
            && let Ok(st) = backend.stat(&e.path).await
        {
            e.size = e.size.or(st.size);
            e.mtime_ms = e.mtime_ms.or(st.mtime_ms);
        }
    }
    if json {
        // Wire form (lossless); the consumer decodes with the codec.
        serde_json::to_writer_pretty(std::io::stdout().lock(), &entries)
            .context(norte_i18n::t("cli-serialize-failed"))?;
        println!();
    } else {
        use std::fmt::Write as _;
        for e in &entries {
            let marker = match e.kind {
                EntryKind::Dir => "d",
                EntryKind::File => "-",
                EntryKind::Symlink => "l",
                EntryKind::Other => "?",
            };
            let size = e.size.map_or_else(String::new, |s| s.to_string());
            let mut line = format!("{marker}\t{size}\t{}", e.path.display_lossy());
            // Requested attrs (#108 block 2), in request order; absent =
            // column that is not painted (never a made-up 0).
            for id in attrs {
                if let Some(v) = e.attrs.get(id) {
                    let _ = write!(line, "\t{id}={}", render_attr_value(v));
                }
            }
            println!("{line}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Attr value for human `ls`. Text and bytes are THIRD-PARTY:
/// `escape_debug` neutralizes controls, RTL and invisibles; bytes go
/// through lossy conversion FIRST (rule 1: the loss is explicit and only
/// presentational — `--json` keeps the exact wire form).
fn render_attr_value(v: &norte_proto::AttrValue) -> String {
    use norte_proto::AttrValue as V;
    match v {
        V::Uint(n) => n.to_string(),
        V::Int(n) | V::TimeMs(n) => n.to_string(),
        V::Bool(b) => b.to_string(),
        V::Text(s) => s.escape_debug().to_string(),
        V::Bytes(b) => String::from_utf8_lossy(b).escape_debug().to_string(),
        V::Unknown => "?".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H1 (encoding review #108-b2): the human render of attrs NEUTRALIZES
    /// third-party text — RTL override/ZWJ escaped, non-UTF-8 bytes via
    /// lossy+escape, never raw in the terminal.
    #[test]
    fn render_attr_value_neutralizes_hostiles() {
        use norte_proto::AttrValue;
        // The synthetic MemProvider's canonical hostile values.
        let text = render_attr_value(&AttrValue::Text("\u{202e}atón\u{202c} a\u{200d}b".into()));
        assert!(!text.contains('\u{202e}'), "RTL escaped: {text}");
        assert!(!text.contains('\u{200d}'), "ZWJ escaped: {text}");
        assert!(text.contains("\\u{202e}"), "visible as escape: {text}");
        let bytes = render_attr_value(&AttrValue::Bytes(b"due\xf1o-\xff\xfe".to_vec()));
        // Explicit lossy conversion: invalid bytes are visible U+FFFD, the
        // rest readable, and never raw controls.
        assert!(bytes.contains("due"), "readable part kept: {bytes}");
        assert!(bytes.contains('\u{fffd}'), "VISIBLE loss: {bytes}");
        assert!(!bytes.bytes().any(|b| b < 0x20), "no raw controls");
        // A tab inside the value does not inject a column: it goes escaped.
        let tab = render_attr_value(&AttrValue::Text("a\tb".into()));
        assert_eq!(tab, "a\\tb");
        assert_eq!(render_attr_value(&AttrValue::Uint(7)), "7");
        assert_eq!(render_attr_value(&AttrValue::Unknown), "?");
    }
}
