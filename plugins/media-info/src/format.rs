//! How a cell reads.

/// `1920×1080`.
pub fn dims_cell(w: u32, h: u32) -> String {
    format!("{w}×{h}")
}

/// `3:41`, or `1:02:03` past an hour.
pub fn duration_cell(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_read_the_way_people_write_them() {
        assert_eq!(dims_cell(1920, 1080), "1920×1080");
        assert_eq!(duration_cell(0), "0:00");
        assert_eq!(duration_cell(221), "3:41");
        assert_eq!(duration_cell(3661), "1:01:01");
    }
}
