//! `org.norte.date-prefix`: a renamer that proposes `YYYY-MM-DD_name` from
//! each file's modification time (demo of the `renamer` category, ADR 0095).
//!
//! The decisions live in pure functions with their own tests; the WIT glue
//! only exists when compiled as a component.

/// The civil date of a Unix timestamp (UTC), as `YYYY-MM-DD`.
///
/// Howard Hinnant's days-to-civil algorithm; no calendar crate for one
/// function. Negative timestamps (before 1970) are handled by the same
/// arithmetic.
#[must_use]
pub fn civil_date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Whether `name` already starts with a `YYYY-MM-DD_` prefix: those are left
/// alone, so running the renamer twice proposes nothing the second time.
#[must_use]
pub fn already_dated(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() > 11
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b'_'
}

/// The proposed name for `name` modified at `mtime_secs`, or `None` when
/// there is nothing to propose.
#[must_use]
pub fn proposal(name: &str, mtime_secs: i64) -> Option<String> {
    if name.is_empty() || already_dated(name) {
        return None;
    }
    Some(format!("{}_{name}", civil_date(mtime_secs)))
}

#[cfg(target_arch = "wasm32")]
mod guest {
    wit_bindgen::generate!({
        world: "norte:renamer/norte-renamer",
        path: "wit",
        generate_all,
    });

    use exports::norte::renamer::renamer::{Guest, LocationRef, Proposal};
    use norte::location::location;

    struct DatePrefix;

    impl Guest for DatePrefix {
        fn plan(
            id: String,
            location: Option<LocationRef>,
            names: Vec<String>,
        ) -> Result<Vec<Proposal>, String> {
            if id != "by-date" {
                return Err(format!("unknown renamer `{id}`"));
            }
            // Without a location there is no modification time to read, and
            // that is said: a plan of guesses is worse than no plan.
            let Some(loc) = location else {
                return Err("this renamer needs to read the files' dates: approve its `location` capability".to_owned());
            };
            let mut out = Vec::new();
            for name in &names {
                let mut rel = loc.prefix.clone();
                if !rel.is_empty() && rel.last() != Some(&b'/') {
                    rel.push(b'/');
                }
                rel.extend_from_slice(name.as_bytes());
                let Ok(meta) = location::stat(&loc.token, &rel) else {
                    continue;
                };
                if let Some(proposed) = crate::proposal(name, meta.mtime_sec) {
                    out.push(Proposal {
                        current: name.clone(),
                        proposed,
                    });
                }
            }
            Ok(out)
        }
    }

    export!(DatePrefix);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_match_known_timestamps() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(951_782_400), "2000-02-29");
        assert_eq!(civil_date(1_756_857_600), "2025-09-03");
        assert_eq!(civil_date(-86_400), "1969-12-31");
    }

    #[test]
    fn proposes_a_prefix_once() {
        assert_eq!(
            proposal("foto.jpg", 1_756_857_600).as_deref(),
            Some("2025-09-03_foto.jpg")
        );
        assert_eq!(proposal("2025-09-03_foto.jpg", 0), None, "already dated");
        assert_eq!(proposal("", 0), None);
        assert!(!already_dated("2025-09-03foto"), "the underscore is part of the mark");
    }
}
