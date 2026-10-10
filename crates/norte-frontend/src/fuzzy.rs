//! A fuzzy scorer: ranks a candidate against a query and says which chars it used.
//! Positions are CHAR indices of the candidate as given, so a frontend can highlight them.
//! Not `nucleo-matcher`: it is MPL-2.0, which `deny.toml` does not allow.

use unicode_normalization::char::{decompose_canonical, is_combining_mark};

/// A match: how good it is and which characters of the candidate it used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Higher is better; only comparable between candidates of ONE query.
    pub score: i32,
    /// The matched characters, as CHAR indices of the candidate, ascending.
    pub positions: Vec<u32>,
}

const SCORE_MATCH: i32 = 16;
const BONUS_BOUNDARY: i32 = 8;
const BONUS_FIRST_CHAR: i32 = 8;
const BONUS_CAMEL: i32 = 7;
const BONUS_CONSECUTIVE: i32 = 8;
const BONUS_EXACT_CASE: i32 = 1;
const PENALTY_GAP_START: i32 = 3;
const PENALTY_GAP_EXTENSION: i32 = 1;

/// One char as the matcher compares it: lower case, without its accent.
/// One char in, one char out, so a position is still an index of the
/// original. Only a letter-plus-marks decomposition is stripped: Hangul
/// decomposes into jamo, and folding a syllable to its first jamo would
/// match half the language.
fn fold_char(c: char) -> char {
    let mut base: Option<char> = None;
    let mut only_marks = true;
    decompose_canonical(c, |d| match base {
        None => base = Some(d),
        Some(_) => only_marks &= is_combining_mark(d),
    });
    let b = if only_marks { base.unwrap_or(c) } else { c };
    b.to_lowercase().next().unwrap_or(b)
}

fn is_separator(c: char) -> bool {
    matches!(c, ' ' | '.' | '-' | '_' | '/' | ':')
}

/// The bonus for matching at `i`: the candidate's start, a word's start
/// (after a separator) or a lower→upper change.
fn bonus_at(chars: &[char], i: usize) -> i32 {
    let Some(prev) = i.checked_sub(1).and_then(|p| chars.get(p)) else {
        return BONUS_BOUNDARY + BONUS_FIRST_CHAR;
    };
    let cur = chars.get(i).copied().unwrap_or(' ');
    if is_separator(*prev) {
        BONUS_BOUNDARY
    } else if prev.is_lowercase() && cur.is_uppercase() {
        BONUS_CAMEL
    } else {
        0
    }
}

/// Scores `candidate` against `query`, or `None` if `query` is not a
/// subsequence of it (after folding case and accents).
///
/// fzf's v1: a greedy forward pass finds where the query ends, a backward
/// pass from there finds the latest start that still holds it (the tightest
/// window), and a forward pass inside the window scores.
///
/// ```
/// use norte_frontend::fuzzy::score;
/// let m = score("pa", "copy path").expect("matches");
/// assert_eq!(m.positions, vec![5, 6]);
/// assert!(score("zz", "copy path").is_none());
/// ```
#[must_use]
pub fn score(query: &str, candidate: &str) -> Option<Match> {
    let q: Vec<(char, char)> = query
        .chars()
        .filter(|c| !is_combining_mark(*c))
        .map(|c| (c, fold_char(c)))
        .collect();
    if q.is_empty() {
        return Some(Match {
            score: 0,
            positions: Vec::new(),
        });
    }
    let cand: Vec<char> = candidate.chars().collect();
    let folded: Vec<char> = cand.iter().map(|&c| fold_char(c)).collect();

    let mut qi = 0;
    let mut end = None;
    for (i, &f) in folded.iter().enumerate() {
        if f == q[qi].1 {
            qi += 1;
            if qi == q.len() {
                end = Some(i);
                break;
            }
        }
    }
    let end = end?;

    let mut qi = q.len();
    let mut start = end;
    for i in (0..=end).rev() {
        if folded[i] == q[qi - 1].1 {
            qi -= 1;
            if qi == 0 {
                start = i;
                break;
            }
        }
    }

    let mut positions = Vec::with_capacity(q.len());
    let mut total = 0i32;
    let mut qi = 0;
    let mut prev: Option<usize> = None;
    for i in start..=end {
        if qi < q.len() && folded[i] == q[qi].1 {
            let mut s = SCORE_MATCH + bonus_at(&cand, i);
            match prev {
                Some(p) if p + 1 == i => s += BONUS_CONSECUTIVE,
                Some(p) => {
                    let gap = i32::try_from(i - p - 1).unwrap_or(i32::MAX / 2);
                    s -= PENALTY_GAP_START + PENALTY_GAP_EXTENSION * (gap - 1);
                }
                None => {}
            }
            if cand[i] == q[qi].0 {
                s += BONUS_EXACT_CASE;
            }
            total = total.saturating_add(s);
            positions.push(u32::try_from(i).unwrap_or(u32::MAX));
            prev = Some(i);
            qi += 1;
        }
    }
    Some(Match {
        score: total,
        positions,
    })
}

/// Is `needle` a subsequence of `hay`? By CHARS, not bytes: a query must not
/// match a continuation byte in the middle of a character (review m12).
/// Folded input only.
pub(crate) fn is_subsequence(needle: &str, hay: &str) -> bool {
    let mut it = hay.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}

#[cfg(test)]
mod tests {
    use super::{Match, is_subsequence, score};

    fn s(q: &str, c: &str) -> i32 {
        score(q, c)
            .unwrap_or_else(|| panic!("{q:?} should match {c:?}"))
            .score
    }

    /// A word start beats the middle of a word, and the backward pass picks
    /// the tightest window: `pa` in "copy path" is the `pa` of "path", not
    /// the `p` of "copy".
    #[test]
    fn a_word_start_beats_the_middle_of_a_word() {
        assert!(s("pa", "copy path") > s("pa", "capable"));
        assert_eq!(
            score("pa", "copy path").map(|m| m.positions),
            Some(vec![5, 6])
        );
    }

    #[test]
    fn consecutive_beats_scattered() {
        assert!(s("cop", "copy") > s("cop", "cxoxpx"));
    }

    #[test]
    fn the_exact_case_counts_a_little() {
        assert!(s("Cp", "Copy") > s("Cp", "copy"));
    }

    /// Positions are CHARS of the candidate as given — an emoji is one, an
    /// accented letter is one — and `e` finds `é` (accents and case fold).
    #[test]
    fn positions_are_chars_and_accents_fold() {
        assert_eq!(
            score("cafe", "🦀 Café").map(|m| m.positions),
            Some(vec![2, 3, 4, 5])
        );
        // A decomposed query (`e` + combining acute) is the same query.
        assert_eq!(
            score("e\u{301}", "café").map(|m| m.positions),
            Some(vec![3])
        );
        // A precomposed one finds a plain letter too.
        assert!(score("é", "cafe").is_some());
        // Hangul is not "a letter plus marks": it does not loosen to its jamo.
        assert!(score("한", "할").is_none());
    }

    #[test]
    fn no_match_is_none_and_an_empty_query_matches_everything() {
        assert_eq!(score("xyz", "copy"), None);
        assert_eq!(
            score("", "copy"),
            Some(Match {
                score: 0,
                positions: Vec::new()
            })
        );
        assert_eq!(
            score("\u{301}", "copy"),
            Some(Match {
                score: 0,
                positions: Vec::new()
            })
        );
    }

    /// By chars, not bytes.
    #[test]
    fn is_subsequence_is_by_chars() {
        assert!(is_subsequence("", "x") && !is_subsequence("ba", "ab"));
        assert!(!is_subsequence("\u{a9}", "é"));
    }
}
