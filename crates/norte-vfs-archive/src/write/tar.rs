//! INCREMENTAL tar writer, with the same contract as zip's.
//!
//! ustar format with the GNU extension for long names (typeflag `L`): a
//! name over 100 bytes isn't trimmed — trimming a name is losing it —, it
//! travels in its own entry ahead of the real one.
//!
//! Written by hand and not with `tar::Builder` for the same reason the
//! zip index is: the `Builder` wants a `Read` per entry, and here the
//! bytes arrive in pieces from an async provider.

use super::{PackEntry, PackError};

/// A tar block. Everything, headers included, is a multiple of this.
const BLOCK: usize = 512;
/// What fits in a ustar header's `name` field.
const NAME_MAX: usize = 100;
/// Typeflag of the GNU entry that carries a long name.
const TYPE_LONGNAME: u8 = b'L';
/// Typeflag of a regular file.
const TYPE_FILE: u8 = b'0';
/// Typeflag of a directory.
const TYPE_DIR: u8 = b'5';

/// Incremental tar writer.
pub struct TarWriter {
    out: Vec<u8>,
    current_size: Option<u64>,
    /// Bytes written of the entry in progress, for the padding tail.
    written: u64,
    closed: bool,
}

impl TarWriter {
    /// An empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            out: Vec::new(),
            current_size: None,
            written: 0,
            closed: false,
        }
    }

    /// Bytes produced so far; empties the buffer.
    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// Opens an entry. `entry.size` has to be the EXACT size about to be
    /// written: tar carries it in the header, ahead of the data.
    ///
    /// # Errors
    ///
    /// [`PackError::State`] if an entry is already open or the archive is closed.
    pub fn begin(&mut self, entry: &PackEntry) -> Result<(), PackError> {
        if self.current_size.is_some() || self.closed {
            return Err(PackError::State);
        }
        // The size has to FIT in ustar's eleven octal digits: past that,
        // the header would lie and the archive would be unreadable from
        // that entry onward. It's reported, not written (see
        // [`TAR_SIZE_MAX`]).
        if !entry.dir && entry.size > TAR_SIZE_MAX {
            return Err(PackError::Size);
        }
        let mut name = entry.name.clone();
        if entry.dir && !name.ends_with(b"/") {
            name.push(b'/');
        }
        if name.len() > NAME_MAX {
            // GNU longname: an `L` entry whose CONTENT is the name, and
            // behind it the real one with the trimmed name (which readers
            // that understand `L` ignore, and those that don't at least
            // see something).
            let mut header = [0_u8; BLOCK];
            write_header(
                &mut header,
                b"././@LongLink",
                name.len() as u64,
                TYPE_LONGNAME,
                0o644,
                None,
            );
            self.out.extend_from_slice(&header);
            self.out.extend_from_slice(&name);
            self.pad(name.len() as u64);
        }
        let short: Vec<u8> = name.iter().copied().take(NAME_MAX).collect();
        let size = if entry.dir { 0 } else { entry.size };
        let mut header = [0_u8; BLOCK];
        write_header(
            &mut header,
            &short,
            size,
            if entry.dir { TYPE_DIR } else { TYPE_FILE },
            entry.mode.unwrap_or(if entry.dir { 0o755 } else { 0o644 }),
            entry.mtime_ms,
        );
        self.out.extend_from_slice(&header);
        self.current_size = Some(size);
        self.written = 0;
        Ok(())
    }

    /// Adds data to the open entry.
    ///
    /// # Errors
    ///
    /// [`PackError::State`] with no entry open, or if the chunk exceeds
    /// the size announced in the header — a tar whose header lies is a
    /// tar nobody can read past that entry.
    pub fn data(&mut self, chunk: &[u8]) -> Result<(), PackError> {
        let size = self.current_size.ok_or(PackError::State)?;
        let new_total = self.written.saturating_add(chunk.len() as u64);
        if new_total > size {
            return Err(PackError::Size);
        }
        self.written = new_total;
        self.out.extend_from_slice(chunk);
        Ok(())
    }

    /// Closes the open entry and pads it up to the block.
    ///
    /// # Errors
    ///
    /// [`PackError::State`] with no entry open, [`PackError::Size`] if
    /// fewer bytes than announced were written.
    pub fn end(&mut self) -> Result<(), PackError> {
        let size = self.current_size.take().ok_or(PackError::State)?;
        if self.written != size {
            return Err(PackError::Size);
        }
        self.pad(size);
        Ok(())
    }

    /// Writes the end marker: two zeroed blocks.
    ///
    /// # Errors
    ///
    /// [`PackError::State`] if an entry is still open.
    pub fn finish(&mut self) -> Result<(), PackError> {
        if self.current_size.is_some() {
            return Err(PackError::State);
        }
        self.out.extend_from_slice(&[0_u8; BLOCK * 2]);
        self.closed = true;
        Ok(())
    }

    /// Zeros up to closing the 512 block.
    fn pad(&mut self, written: u64) {
        // The remainder of dividing by 512 fits in `usize` on any
        // architecture: it's less than 512.
        let remainder = usize::try_from(written % BLOCK as u64).unwrap_or(0);
        if remainder != 0 {
            self.out
                .extend(std::iter::repeat_n(0_u8, BLOCK - remainder));
        }
    }
}

impl Default for TarWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Fills in a ustar header and computes its checksum.
fn write_header(
    h: &mut [u8; BLOCK],
    name: &[u8],
    size: u64,
    typeflag: u8,
    mode: u32,
    mtime_ms: Option<i64>,
) {
    let n = name.len().min(NAME_MAX);
    h[..n].copy_from_slice(&name[..n]);
    octal(&mut h[100..108], u64::from(mode & 0o7777), 7);
    octal(&mut h[108..116], 0, 7); // uid
    octal(&mut h[116..124], 0, 7); // gid
    octal(&mut h[124..136], size, 11);
    let mtime = mtime_ms.map_or(0, |ms| ms.div_euclid(1000).max(0));
    octal(&mut h[136..148], u64::try_from(mtime).unwrap_or(0), 11);
    // The checksum is computed with its own field filled with spaces.
    h[148..156].fill(b' ');
    h[156] = typeflag;
    h[257..262].copy_from_slice(b"ustar");
    h[263..265].copy_from_slice(b"00");
    let checksum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    octal(&mut h[148..155], u64::from(checksum), 6);
    h[155] = b' ';
}

/// The largest value that fits in ustar's size field: eleven octal
/// digits, i.e. 8 GiB minus one.
///
/// It isn't a format curiosity: `octal` used to keep the LOW digits, so a
/// file of exactly 8 GiB would be announced with size ZERO and its eight
/// gigs would follow behind. The padding was computed from the real size,
/// so the stream stayed aligned, and the corruption wasn't visible until a
/// reader tried to parse those bytes as headers. A task like that
/// finished with not a single error, and the journal logged a `Created`.
pub(super) const TAR_SIZE_MAX: u64 = 0o777_7777_7777;

/// A number in ASCII octal, right-aligned with zeros and NUL-terminated,
/// which is how tar carries them.
///
/// The value has to FIT: the caller checks beforehand ([`TAR_SIZE_MAX`]).
/// Here, if it didn't fit, it gets filled with octal nines — a visibly
/// absurd value — instead of keeping the low digits, which is what turned
/// an overflow into a plausible, false size.
fn octal(field: &mut [u8], v: u64, digits: usize) {
    let s = format!("{v:0>digits$o}");
    let bytes = s.as_bytes();
    if bytes.len() > digits {
        field[..digits].fill(b'7');
    } else {
        let n = bytes.len().min(digits);
        field[digits - n..digits].copy_from_slice(bytes);
    }
    if digits < field.len() {
        field[digits] = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A size that doesn't fit in eleven octal digits gets REPORTED.**
    ///
    /// `octal` used to keep the low digits, so an exact 8 GiB was written
    /// as size ZERO with eight gigs following behind: the padding came
    /// from the real size, the stream stayed aligned, and the corruption
    /// didn't show up until a reader tried to parse those bytes as
    /// headers. The task finished green and the journal logged a `Created`.
    #[test]
    fn a_size_that_does_not_fit_in_ustar_is_refused() {
        let mut w = TarWriter::new();
        assert_eq!(
            w.begin(&PackEntry::file(b"vm.img".to_vec(), TAR_SIZE_MAX + 1)),
            Err(PackError::Size)
        );
        // And right below the ceiling it does fit.
        assert!(
            w.begin(&PackEntry::file(b"vm.img".to_vec(), TAR_SIZE_MAX))
                .is_ok()
        );
    }

    /// The octal field is HIGH-digit: if something didn't fit, it looks
    /// like it didn't fit instead of looking like a plausible number.
    #[test]
    fn octal_does_not_keep_the_low_digits() {
        let mut field = [0_u8; 12];
        octal(&mut field, 8, 11);
        assert_eq!(&field[..11], b"00000000010", "eight is 10 in octal");
        // A value that overflows: nines, not zeros.
        octal(&mut field, TAR_SIZE_MAX + 1, 11);
        assert_eq!(&field[..11], b"77777777777");
    }

    /// A long name's header does NOT trim the name: it travels whole in
    /// its `L` entry, and the one kept to 100 bytes is a copy.
    #[test]
    fn a_long_name_travels_whole_in_its_own_entry() {
        let long_name = vec![b'a'; 150];
        let mut w = TarWriter::new();
        w.begin(&PackEntry::file(long_name.clone(), 0))
            .expect("opens");
        let bytes = w.take();
        assert_eq!(&bytes[..13], b"././@LongLink", "the GNU entry goes first");
        assert_eq!(bytes[156], TYPE_LONGNAME);
        assert_eq!(
            &bytes[BLOCK..BLOCK + long_name.len()],
            &long_name[..],
            "whole"
        );
        // And behind it, the real header with the first 100 bytes.
        let actual = BLOCK * 2;
        assert_eq!(&bytes[actual..actual + 100], &long_name[..100]);
    }
}
