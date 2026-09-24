//! `norte compare`: `fs.compare` and its verdict in the exit code, plus the
//! presentation helpers (`masked`, exit codes) that `sync`/`ai` share.

use std::process::ExitCode;

use anyhow::Context;
use norte_core::backend::Backend;
use norte_proto::TaskState;

use crate::cmd::connect::vpath;

/// The already-masked text, MARKED with `!` if it had to be masked.
///
/// One function and not the three copies there used to be (`ai_cmd`,
/// `compare_cmd`, `sync_cmd`). The `!` is a SECURITY marker: it says what
/// is being read is not literally what is on disk, which is exactly what
/// a name with an RLO inside would use to spoof a confirmation. Three
/// copies of a security marker in one binary is how one of them stops
/// applying without anyone noticing.
///
/// Here and not in `norte-frontend` on purpose: that crate's `lib.rs` says
/// masking is its job and the BADGE is each frontend's paint layer's — the
/// TUI paints it with color and a pipe has no color to give.
pub(crate) fn masked(text: &str, hostile: bool) -> String {
    format!("{}{text}", if hostile { "!" } else { "" })
}

/// A sync step's (or failure's) `rel`, ready for a terminal.
/// `render_step`/`rel_display` already masked (rule 1); this only puts
/// [`masked`]'s marker over the `hostile` flag that call already computed.
pub(crate) fn rel_marked(d: &norte_frontend::sync::RelDisplay) -> String {
    masked(&d.text, d.hostile)
}

/// stdout closed or failed while printing: code 2, never a panic.
///
/// `println!` **panics** on `EPIPE`, and `norte compare a b | head -20` —
/// the obvious way to peek at a streaming diff — is exactly that: the
/// reader leaves as soon as it has its twenty lines. A panic's 101 is not
/// in the table these two commands document, and it also dirties stderr
/// in NORMAL pipe use. The 2 is in the table, and it is also true: what
/// could not finish being written could not be fully answered either.
/// `ls --json` already dodges the same thing with `serde_json::to_writer`
/// + `?`.
pub(crate) fn write_error_code(e: &std::io::Error) -> ExitCode {
    // `EPIPE` is the reader leaving: staying silent is correct, nothing is
    // broken. Any other write failure (a `> file` that filled the disk)
    // IS reported, or the 2 would have no explanation anywhere.
    if e.kind() != std::io::ErrorKind::BrokenPipe {
        eprintln!("norte: {e}");
    }
    ExitCode::from(2)
}

/// An `Err` from `norte compare`/`norte sync` is a **2**, never
/// `ExitCode::FAILURE`'s 1.
///
/// These two commands answer through the exit code, so 1 already means
/// something: "differ" in one and "applied" in the other. `main`'s `match`
/// turns any `anyhow::Error` into `FAILURE`, i.e. that same 1 — so a
/// misspelled `--criteria`, an unreadable path or a refused `sync.apply`
/// would exit through the same door as a success. They are translated
/// here, at dispatch, so that **no** error path can reach `main`'s
/// `match`: only a run that FINISHED can answer 0 or 1.
pub(crate) fn code_for_could_not(e: &anyhow::Error) -> ExitCode {
    eprintln!("norte: {e:#}");
    ExitCode::from(2)
}

/// Translates `--criteria` to a [`norte_proto::methods::CompareCriteria`].
///
/// Empty = the wire default (size and date, no hash — see
/// `FsCompareParams`'s doctest). Not empty = EXACTLY the requested list:
/// `--criteria hash` alone turns on only `hash` and turns off
/// `size`/`mtime`, so that "I want nothing but the hash" has the obvious
/// effect on the request even though the core (ADR 0048) only runs it over
/// the pairs the cheap rungs already called equal.
pub(crate) fn parse_compare_criteria(
    names: &[String],
) -> anyhow::Result<norte_proto::methods::CompareCriteria> {
    if names.is_empty() {
        return Ok(norte_proto::methods::CompareCriteria::default());
    }
    let mut criteria = norte_proto::methods::CompareCriteria {
        size: false,
        mtime: false,
        hash: false,
    };
    for name in names {
        match name.as_str() {
            "size" => criteria.size = true,
            "mtime" => criteria.mtime = true,
            "hash" => criteria.hash = true,
            other => anyhow::bail!(
                "--criteria: criterio desconocido \"{}\"",
                other.escape_debug()
            ),
        }
    }
    Ok(criteria)
}

/// What a comparison can answer, **in precedence order**: the one further
/// down wins over the one above it.
///
/// Three and not two, and with a derived `Ord` instead of an accumulated
/// `bool`, because the important answer is the middle one: "could not
/// tell" has to win over the other two, and a `bool` has nowhere to keep
/// it. It is the same reason the command has three exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Verdict {
    /// Every row said "same", and all of them with confidence.
    Match,
    /// Some row differs, and none was left unanswered.
    Differ,
    /// Some row could not be answered, or was answered without being able
    /// to back it up.
    Unknown,
}

impl Verdict {
    /// What ONE row contributes to the whole comparison's verdict.
    ///
    /// Confidence goes first, and not as decoration.
    /// `CompareVerdict::Same` with `CompareConfidence::Unknown` is what
    /// `cascade.rs` answers when it could not compare anything — two
    /// symlinks whose targets were not read, a side with no size, a
    /// socket — and it is an honest answer ONLY as long as whoever reads
    /// it sees the confidence glyph, as in the TUI. Collapsed to an exit
    /// code without that nuance it would become "the trees match", which
    /// is exactly what nobody checked.
    fn from_row(row: &norte_proto::methods::CompareRow) -> Self {
        use norte_proto::methods::{CompareConfidence as Conf, CompareVerdict as V};
        match (row.verdict, row.confidence) {
            // Before the verdict: a conclusion the criterion does not back
            // up cannot be summarized, whatever it says. `Unrecognised` is
            // an N+1 core's confidence, and neither can that.
            (_, Conf::Unknown | Conf::Unrecognised) => Self::Unknown,
            (V::Same, _) => Self::Match,
            (V::Different | V::OnlyLeft | V::OnlyRight | V::TypeMismatch, _) => Self::Differ,
            // `Error` (unreadable listing, directory above the cap),
            // `Ambiguous` (a case or NFC collision — exactly what a later
            // sync has to see BEFORE writing), and an N+1 core's verdict
            // this binary cannot read. None of the three is "differ": it
            // is that it is not known.
            _ => Self::Unknown,
        }
    }

    /// The exit code, which is the whole answer a script reads.
    fn code(self) -> ExitCode {
        ExitCode::from(match self {
            Self::Match => 0,
            Self::Differ => 1,
            Self::Unknown => 2,
        })
    }
}

/// `norte compare`: `fs.compare` and its verdict in the exit code.
///
/// # Why the verdict goes in the code
/// It is the question "did the copy work?", and whoever asks it is
/// usually a script. `diff` has answered this way forever and there is
/// nothing to improve in that convention: 0 same, 1 differ, and a third
/// code for "could not tell" which is the one that really matters here —
/// an INCOMPLETE comparison answering 0 would be exactly the mistake this
/// command exists not to make. The precedence among the three is in
/// [`Verdict`].
pub(crate) async fn compare_cmd(
    backend: &Backend,
    a: &std::path::Path,
    b: &std::path::Path,
    json: bool,
    criteria: &[String],
    max_depth: Option<u32>,
    mtime_tolerance_ms: Option<u32>,
) -> anyhow::Result<ExitCode> {
    use std::io::Write as _;

    let left = vpath(a)?;
    let right = vpath(b)?;
    let params = norte_proto::methods::FsCompareParams {
        left,
        right,
        criteria: parse_compare_criteria(criteria)?,
        max_depth,
        // 2000 ms is the default `FsCompareParams` declares (the FAT
        // rule, ADR 0048; see its doctest: `mtime_tolerance_ms == 2000`).
        // The number is repeated here because the type does not derive
        // `Default` and the constant that fixes it in the proto is
        // private — there is no `FsCompareParams::default()` to reuse.
        mtime_tolerance_ms: mtime_tolerance_ms.unwrap_or(2000),
        // `Backend::compare` rejects `follow_symlinks: true`, and this
        // command has no reason to diverge from `sync_plan`, which
        // rejects both.
        follow_symlinks: false,
        descend_orphans: None,
    };

    let (task, mut rx) = backend
        .compare(params)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-compare-failed"))?;

    // Names from the OTHER side's tree, which this process does not
    // control: MARK the masked one, like `ai_cmd` — a remote name can
    // carry an RLO and spoof the output. `cells_for` already masked
    // `RowFace::name` with `display_name_with` (rule 1); this only adds
    // [`masked`]'s `!` over the `hostile` flag that call already
    // computed — not a second, separate masking, which would diverge the
    // day this command gains a reinterpretation (#57) and someone forgets
    // to thread it here too.
    let face_name = |face: &norte_frontend::compare::RowFace| masked(&face.name, face.hostile);

    // A pipe that closes must not `panic!`: see [`write_error_code`].
    // Buffered also because one `write` syscall per row over a large tree
    // is a toll that is not needed.
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());

    // Decided row by row while draining — they are NEVER collected (the
    // `ComparePane` rustdoc explains what retaining a million rows costs,
    // and this command has no reason to retain any).
    let mut verdict = Verdict::Match;
    while let Some(batch) = rx.recv().await {
        for row in &batch.rows {
            verdict = verdict.max(Verdict::from_row(row));
            let written = if json {
                // Wire form (lossless); --json neither translates nor
                // masks — a script consumer decodes with the same codec
                // as `norte ls --json`.
                writeln!(out, "{}", serde_json::to_string(row)?)
            } else {
                let cells = norte_frontend::compare::cells_for(row, None, None);
                let left_name = cells.left.as_ref().map_or_else(String::new, face_name);
                let right_name = cells.right.as_ref().map_or_else(String::new, face_name);
                // BOTH glyphs, like the TUI. `Same` is not an answer by
                // itself (see `compare::Glyphs`'s rustdoc): `Same`/`!`
                // came from a hash or a different size and `Same`/`?`
                // from a provider that could not answer, and showing one
                // without the other is the drift this command exists not
                // to have.
                writeln!(
                    out,
                    "{}{} {left_name}\t{right_name}",
                    cells.glyphs.verdict, cells.glyphs.confidence
                )
            };
            if let Err(e) = written {
                return Ok(write_error_code(&e));
            }
        }
    }
    if let Err(e) = out.flush() {
        return Ok(write_error_code(&e));
    }
    // stdout's lock is released HERE: whatever is left to say goes to
    // stderr.
    drop(out);

    match task.join().await {
        TaskState::Completed => Ok(verdict.code()),
        other => {
            eprintln!(
                "norte: {}",
                norte_i18n::ta(
                    "cli-compare-incomplete",
                    &[("state", &format!("{other:?}"))],
                )
            );
            Ok(ExitCode::from(2))
        }
    }
}
