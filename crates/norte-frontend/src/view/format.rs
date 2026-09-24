//! Presentation formatting shared by the frontends. Lives here, not in a
//! frontend, so the TUI, the GUI, and the columns work (#108) format a size
//! the same way instead of growing three formatters.

/// Human-readable byte size: exact below 1 KiB, one decimal and a binary
/// unit above. Never panics and never overflows — `u64::MAX` is `16.0 EiB`
/// (there is no `ZiB` arm: it is unreachable, `u64::MAX` tops out at 16 EiB).
///
/// The unit is NOT localised: `KiB`/`MiB` are the same token in every locale
/// norte ships, and a translated unit would make sizes incomparable between
/// screenshots and bug reports. Neither is the decimal separator — this
/// always prints `1.5 KiB`, never the Spanish `1,5 KiB` — so the value stays
/// a single unambiguous token a bug report or a `grep` can match verbatim;
/// only the surrounding sentence is localised. The next reviewer wondering
/// whether to "fix" the comma: don't, that's this rustdoc's answer.
///
/// ```
/// use norte_frontend::human_bytes;
/// assert_eq!(human_bytes(1536), "1.5 KiB");
/// ```
#[must_use]
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    // `n` is a byte count; the largest value here is `u64::MAX` ≈ 1.8e19,
    // which already exceeds f64's 2^53 exact-integer range, but the loop
    // below only ever needs ~3 significant digits of `value` to pick a unit
    // and round to one decimal — the precision loss is invisible at that
    // scale (same idiom as `settings.rs`'s `min as f64`/`max as f64`).
    #[expect(clippy::cast_precision_loss, reason = "magnitudes far from 2^53")]
    let mut value = n as f64 / 1024.0;
    let mut unit = 0usize;
    // Promote on the ROUNDED value, not the raw one (review MAJOR M2): the
    // output prints one decimal, so anything that rounds up to `10240.0`
    // (i.e. `1024.0` at the printed precision) must already have promoted —
    // otherwise `value` in `[1023.95, 1024.0)` prints as `"1024.0 KiB"`, a
    // string that must never exist. `(value * 10.0).round()` mirrors the
    // `{value:.1}` formatting below at integer precision, so the promotion
    // decision and the printed digit agree by construction.
    while (value * 10.0).round() >= 10240.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// The same size in FOUR or five cells: no decimals, and the unit's initial.
/// `38G`, `402M`, `900B`.
///
/// Exists for the places sidebar (L3), which has fourteen cells for the
/// mount's name AND its free space: neither fits with `38.2 GiB`, and
/// truncating a size does not give a broken label but a FALSE NUMBER
/// (`38.2 GiB` truncated from the head paints `8.2 GiB`).
///
/// Rounds DOWN on purpose: the free space announced must never be more than
/// there actually is.
///
/// The unit is not localised, for the same reason as in [`human_bytes`].
///
/// ```
/// use norte_frontend::human_bytes_short;
/// assert_eq!(human_bytes_short(900), "900B");
/// assert_eq!(human_bytes_short(402 * 1000 * 1000), "383M");
/// assert_eq!(human_bytes_short(41_000_000_000), "38G");
/// ```
#[must_use]
pub fn human_bytes_short(n: u64) -> String {
    const UNITS: [char; 6] = ['K', 'M', 'G', 'T', 'P', 'E'];
    if n < 1024 {
        return format!("{n}B");
    }
    let mut value = n / 1024;
    let mut unit = 0usize;
    while value >= 1024 && unit + 1 < UNITS.len() {
        value /= 1024;
        unit += 1;
    }
    format!("{value}{}", UNITS[unit])
}

/// The `HH:MM:SS` time of a millisecond timestamp, in UTC.
///
/// UTC and not local, same as the date column in ISO format: this tree
/// carries no timezone database, and a local time invented from a fixed
/// offset would lie twice a year. What gets compared here is lines against
/// each other, and for that the timezone does not matter as long as it is the
/// same one.
///
/// Lives here since #326, when the window needed the same thing: two ideas of
/// what time it is in each frontend's log panel is the kind of difference
/// nobody notices until they compare two screenshots.
///
/// ```
/// use norte_frontend::format::hora_utc;
/// assert_eq!(hora_utc(0), "00:00:00");
/// // And a timestamp BEFORE the epoch does not give a negative time.
/// assert_eq!(hora_utc(-1), "23:59:59");
/// ```
#[must_use]
pub fn hora_utc(epoch_ms: i64) -> String {
    let sod = epoch_ms.div_euclid(1000).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", sod / 3600, (sod % 3600) / 60, sod % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The short form rounds DOWN: announcing more free space than there is
    /// is the lie that matters here.
    #[test]
    fn the_short_form_does_not_round_up() {
        assert_eq!(human_bytes_short(1023), "1023B");
        assert_eq!(human_bytes_short(2047), "1K");
        assert_eq!(human_bytes_short(1024 * 1024 - 1), "1023K");
    }

    /// And it never exceeds five cells, which is what the sidebar can afford.
    #[test]
    fn the_short_form_fits_in_five_cells() {
        for n in [0, 1, 1023, 1024, u64::MAX / 2, u64::MAX] {
            assert!(human_bytes_short(n).chars().count() <= 5, "{n}");
        }
    }

    #[test]
    fn bytes_under_a_kilobyte_are_exact() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
    }

    #[test]
    fn larger_sizes_get_one_decimal_and_a_binary_unit() {
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
    }

    /// Review MAJOR M2: the loop used to promote on the UNROUNDED value
    /// while the output rounds to one decimal, so anything in
    /// `[1023.95, 1024.0)` of a unit printed as `"1024.0 <unit>"` — a string
    /// that must never appear. `1_048_575` B is `1024.0 KiB` unrounded
    /// (`1_048_575 / 1024.0 = 1023.999...`); `1_073_741_823` B is
    /// `1024.0 MiB` unrounded the same way one level up. The three values
    /// the sibling test above checks (1024, 1536, 1 MiB) are exactly the
    /// ones that cannot expose this — none of them sits near a boundary.
    #[test]
    fn values_just_under_a_unit_boundary_promote_instead_of_rounding_to_the_next_unit() {
        assert_eq!(human_bytes(1_048_575), "1.0 MiB");
        assert_eq!(human_bytes(1_073_741_823), "1.0 GiB");
    }

    #[test]
    fn the_largest_u64_does_not_panic_or_overflow() {
        assert_eq!(human_bytes(u64::MAX), "16.0 EiB");
    }
}
