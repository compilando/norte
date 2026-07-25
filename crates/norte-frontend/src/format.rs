//! Presentation formatting shared by the frontends. Lives here, not in a
//! frontend, so the TUI, the GUI, and the columns work (#108) format a size
//! the same way instead of growing three formatters.

/// Human-readable byte size: exact below 1 KiB, one decimal and a binary
/// unit above. Never panics and never overflows — `u64::MAX` is `16.0 EiB`.
///
/// The unit is NOT localised: `KiB`/`MiB` are the same token in every locale
/// norte ships, and a translated unit would make sizes incomparable between
/// screenshots and bug reports. The surrounding sentence IS localised.
///
/// ```
/// use norte_frontend::human_bytes;
/// assert_eq!(human_bytes(1536), "1.5 KiB");
/// ```
#[must_use]
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 7] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB", "ZiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    // `n` is a byte count; the largest value here is `u64::MAX` ≈ 1.8e19,
    // which already exceeds f64's 2^53 exact-integer range, but the loop
    // below only ever needs ~3 significant digits of `value` to pick a unit
    // and round to one decimal — the precision loss is invisible at that
    // scale (same idiom as `settings.rs`'s `min as f64`/`max as f64`).
    #[allow(clippy::cast_precision_loss)]
    let mut value = n as f64 / 1024.0;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn the_largest_u64_does_not_panic_or_overflow() {
        assert_eq!(human_bytes(u64::MAX), "16.0 EiB");
    }
}
