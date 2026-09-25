//! The DETERMINISTIC generator of a batch rename plan (#310).
//!
//! The batch machinery already existed whole — a reviewable plan,
//! `plan_hash`, collisions, journal and undo (ADR 0042) — and the only
//! thing that knew how to produce a plan was the language model
//! (`ai.rename_plan`). Meaning renaming twenty files required an LLM. This
//! is the other half: a TEMPLATE the human writes and an expansion that
//! asks nobody.
//!
//! What comes out of here enters through the SAME door as the AI's plan —
//! `fs.rename_batch_plan`, the same review, the same hash — because what
//! makes the operation safe is not where the names came from.
//!
//! # Why text and not bytes
//!
//! A name is bytes (rule 1) and this module works on `str`. It is not an
//! oversight: the pair that travels in the plan (`AiRenameEntry`) is UTF-8
//! by protocol, so a name that is not cannot be part of a batch — not
//! today, not through the AI path either. The caller sets those aside
//! BEFORE and says so; here no lossy conversion is invented that would
//! rename a file to a name that is not its own.

/// The codes a template understands, as they are written.
///
/// `[N]` the name without extension, `[E]` the extension without the dot,
/// `[C]` a counter starting at 1 — and `[C3]` the same counter padded with
/// zeros to three digits. Everything else is literal, including a stray
/// bracket.
///
/// It is the subset of Total Commander used daily; its tool also has
/// substring ranges and dates, and those can be added here without moving
/// anything around them.
pub const CODES: &[&str] = &["[N]", "[E]", "[C]"];

/// Splits a name into `(base, extension)`, without the dot.
///
/// The separating dot is the LAST one, and a name that starts with a dot
/// and has no other — `.bashrc` — is all base and no extension: renaming a
/// hidden file with `[N].[E]` and having it turn into `.bashrc.` would be
/// the kind of surprise a batch rename cannot afford.
#[must_use]
pub fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(i) => (&name[..i], &name[i + 1..]),
    }
}

/// Expands `pattern` for `name`, with `n` as the counter's value.
///
/// ```
/// use norte_frontend::rename_pattern::expand;
/// assert_eq!(expand("[N].[E]", "snapshot.JPG", 1), "snapshot.JPG");
/// assert_eq!(expand("vacaciones-[C3].[E]", "snapshot.jpg", 7), "vacaciones-007.jpg");
/// assert_eq!(expand("[N]", "notes.txt", 1), "notes");
/// ```
#[must_use]
pub fn expand(pattern: &str, name: &str, n: usize) -> String {
    let (base, ext) = split_name(name);
    let mut out = String::with_capacity(pattern.len() + name.len());
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'['
            && let Some(end) = pattern[i..].find(']')
            && let Some(replacement) = expand_code(&pattern[i + 1..i + end], base, ext, n)
        {
            out.push_str(&replacement);
            i += end + 1;
            continue;
        }
        // A bracket that does not open a known code is one more character:
        // a name can carry them, and swallowing it would turn `[draft]`
        // into nothing without saying why.
        let c = pattern[i..].chars().next().unwrap_or('[');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// A code between brackets, already without them. `None` = not one of
/// ours, and then the text stays as is.
fn expand_code(code: &str, base: &str, ext: &str, n: usize) -> Option<String> {
    match code {
        "N" => Some(base.to_owned()),
        "E" => Some(ext.to_owned()),
        "C" => Some(n.to_string()),
        _ => {
            let width: usize = code.strip_prefix('C')?.parse().ok()?;
            // An absurd width does not fill up memory: what a batch of
            // files asks for fits comfortably in two digits and the cap
            // leaves margin.
            let width = width.min(12);
            Some(format!("{n:0width$}"))
        }
    }
}

/// The plan `pattern` produces over `names`, in order.
///
/// Returns `(from, to)` pairs and **omits the ones that do not change**: a
/// plan that promises to rename something to its own name makes the
/// summary lie about how many things are going to happen. The counter
/// counts ALL the input names, whether they change or not, because the
/// opposite would make the number depend on the template and skip gaps
/// with no explanation.
///
/// ```
/// use norte_frontend::rename_pattern::plan;
/// let names = ["a.txt".to_owned(), "b.txt".to_owned()];
/// let pairs = plan("note-[C].[E]", &names, 1);
/// assert_eq!(pairs, vec![
///     ("a.txt".to_owned(), "note-1.txt".to_owned()),
///     ("b.txt".to_owned(), "note-2.txt".to_owned()),
/// ]);
/// ```
#[must_use]
pub fn plan(pattern: &str, names: &[String], start: usize) -> Vec<(String, String)> {
    names
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let new_name = expand(pattern, name, start.saturating_add(i));
            (new_name != *name).then(|| (name.clone(), new_name))
        })
        .collect()
}

/// Why a template is no good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternError {
    /// Empty: there is no name to build.
    Empty,
    /// Would expand to an empty name, or one the filesystem cannot carry
    /// (a `/` or NUL inside).
    BadResult,
}

/// The diagnostic's Fluent key.
#[must_use]
pub const fn error_key(e: PatternError) -> &'static str {
    match e {
        PatternError::Empty => "msg-rename-pattern-empty",
        PatternError::BadResult => "msg-rename-pattern-bad-result",
    }
}

/// Checks the template against the names it is going to touch.
///
/// Validated BEFORE asking the core for a plan: a `/` in the template is
/// not a rename, it is a move to another directory in disguise, and the
/// place that gets explained is the dialog the human has in front of
/// them — not a daemon error three steps later.
///
/// # Errors
/// [`PatternError::Empty`] with a blank template;
/// [`PatternError::BadResult`] if any name would come out empty or with
/// `/`/NUL.
pub fn check(pattern: &str, names: &[String]) -> Result<(), PatternError> {
    if pattern.trim().is_empty() {
        return Err(PatternError::Empty);
    }
    for (i, name) in names.iter().enumerate() {
        let new_name = expand(pattern, name, i + 1);
        if new_name.is_empty() || new_name.contains('/') || new_name.contains('\0') {
            return Err(PatternError::BadResult);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_codes_and_the_padded_counter() {
        assert_eq!(expand("[N].[E]", "foto.jpg", 1), "foto.jpg");
        assert_eq!(expand("[C]-[N].[E]", "foto.jpg", 4), "4-foto.jpg");
        assert_eq!(expand("[C2]", "foto.jpg", 4), "04");
        assert_eq!(expand("[C12]", "x", 1), "000000000001");
        // Absurd width: it is capped, not trusted.
        assert_eq!(expand("[C99]", "x", 1).len(), 12);
    }

    #[test]
    fn the_extension_is_the_last_dot_and_a_hidden_file_has_none() {
        assert_eq!(split_name("a.tar.gz"), ("a.tar", "gz"));
        assert_eq!(split_name("sin-extension"), ("sin-extension", ""));
        assert_eq!(split_name(".bashrc"), (".bashrc", ""));
        // And that is why `[N].[E]` on a hidden file does not hang a dot
        // off the end.
        assert_eq!(expand("[N]", ".bashrc", 1), ".bashrc");
    }

    /// A bracket that is not one of our codes stays as is: there are names
    /// with brackets, and swallowing it would be losing text without
    /// saying so.
    #[test]
    fn a_bracket_that_is_not_code_is_literal() {
        assert_eq!(expand("[borrador] [N]", "a.txt", 1), "[borrador] a");
        assert_eq!(expand("[X]-[N]", "a.txt", 1), "[X]-a");
        assert_eq!(expand("sin cerrar [N", "a.txt", 1), "sin cerrar [N");
    }

    /// The template's text may not be ASCII, and it is not split by bytes.
    #[test]
    fn the_template_accepts_non_ascii_text() {
        assert_eq!(expand("añó-[C]-[N].[E]", "a.txt", 2), "añó-2-a.txt");
    }

    /// The ones that do not change do NOT enter the plan, and the counter
    /// does not skip anything because of it.
    #[test]
    fn the_plan_omits_what_does_not_change_and_the_counter_does_not_skip() {
        let names = vec!["a.txt".to_owned(), "b.txt".to_owned(), "c.txt".to_owned()];
        // `b` is already named what it would come out as, so there is
        // nothing to do with it.
        let pairs = plan("[N].[E]", &names, 1);
        assert!(pairs.is_empty(), "nothing changes: empty plan");

        let pairs = plan("f[C].[E]", &names, 1);
        assert_eq!(
            pairs,
            vec![
                ("a.txt".to_owned(), "f1.txt".to_owned()),
                ("b.txt".to_owned(), "f2.txt".to_owned()),
                ("c.txt".to_owned(), "f3.txt".to_owned()),
            ]
        );
    }

    #[test]
    fn the_counter_can_start_wherever_told() {
        let names = vec!["a".to_owned()];
        assert_eq!(
            plan("[C]", &names, 10),
            vec![("a".to_owned(), "10".to_owned())]
        );
    }

    /// An empty template, or one that would manufacture an impossible
    /// name, is rejected HERE: with the human in front and before asking
    /// for any plan at all.
    #[test]
    fn an_impossible_template_is_rejected_before_requesting_a_plan() {
        let names = vec!["a.txt".to_owned()];
        assert_eq!(check("", &names), Err(PatternError::Empty));
        assert_eq!(check("   ", &names), Err(PatternError::Empty));
        assert_eq!(check("sub/[N]", &names), Err(PatternError::BadResult));
        assert_eq!(
            check("[E]", &["sin-extension".to_owned()]),
            Err(PatternError::BadResult)
        );
        assert!(check("[N]-copia.[E]", &names).is_ok());
    }
}
