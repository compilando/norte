//! Viewer state (phase 7, spec §6): decoded text with detection (via
//! `norte-encoding`), "reload as…", and a hexview for binaries. Pure and
//! testable: reading is done by `main` through the core (rule 7).
//!
//! Core WITHOUT i18n (GUI-d T1): the localised status text lives render-side
//! in each frontend (`norte-tui::viewer::status`, TUI T2), composed over this
//! module's getters (`encoding_name`/`eol`/`had_errors`/`is_forced`).

use norte_encoding::{Decoded, Detection, Eol};
use norte_proto::{Entry, EntryKind, VPath};

/// How many lines a page jumps (fixed, same as in the panes).
pub const PAGE: usize = 10;

/// Bytes per hexview row.
const HEX_COLS: usize = 16;

/// A hexview row's width, in cells: `offset  hex×16  ascii`.
///
/// All ASCII, so cells and bytes match. The count is [`hex_rows`]'s: eight
/// for the offset, two of separation, three per byte, one gap between the
/// two halves, two more of separation, and the ASCII column.
const HEX_ROW_CELLS: usize = 8 + 2 + HEX_COLS * 3 + 1 + 2 + HEX_COLS;

/// An image format RECOGNISED by magic bytes (not by extension: the viewer
/// reads CONTENT, spec §6). The core does NOT decode (no `image` dep): it
/// only recognises and hands the raw bytes to the frontend, which decides
/// whether it knows how to paint them (the GPUI GUI decodes and shows the
/// image; the TUI falls back to hexview through `rows`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFmt {
    /// PNG (`\x89PNG\r\n\x1a\n`).
    Png,
    /// JPEG (`\xFF\xD8\xFF`).
    Jpeg,
    /// GIF (`GIF87a` / `GIF89a`).
    Gif,
    /// BMP (`BM`).
    Bmp,
    /// WebP (contenedor RIFF con marca `WEBP`).
    Webp,
}

impl ImageFmt {
    /// Technical label for the status bar (literal, not i18n).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ImageFmt::Png => "PNG",
            ImageFmt::Jpeg => "JPEG",
            ImageFmt::Gif => "GIF",
            ImageFmt::Bmp => "BMP",
            ImageFmt::Webp => "WebP",
        }
    }
}

/// Recognises an image format by its MAGIC bytes (spec §6: the viewer reads
/// CONTENT, never trusts the extension). Pure and cheap: it only looks at
/// the header, it does not decode or validate the rest. `None` if it is none
/// of the supported formats. The real decoding (and its validation) is done
/// by the frontend; a false positive here falls back to the frontend's
/// fallback, it does not break.
#[must_use]
pub fn image_format(bytes: &[u8]) -> Option<ImageFmt> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFmt::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFmt::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ImageFmt::Gif)
    } else if bytes.starts_with(b"BM") {
        Some(ImageFmt::Bmp)
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(ImageFmt::Webp)
    } else {
        None
    }
}

/// [`image_format`]'s twin by EXTENSION, and the ONLY place in this module
/// that looks at a name instead of bytes.
///
/// Exists for a specific, bounded reason: knowing which is the next sibling
/// requires classifying candidates that have not been read yet, and reading
/// all of them to find out would cost one read —and on a remote provider, one
/// round trip— per file that gets discarded. So [`sibling`]'s ladder is
/// walked by extension and the viewer's MODE keeps being decided by content,
/// as always: a `.jpg` that is not one still opens, and opens as whatever it
/// really is.
///
/// The extensions are exactly those of the five formats [`image_format`]
/// recognises. One that is not here does not mean it is not an image: it
/// means this viewer would not know how to paint it.
///
/// Operates on raw bytes (rule 1): it splits at the LAST `.` at the byte
/// level and only validates the EXTENSION as UTF-8, so a name with a
/// non-UTF8 stem (`caf\xe9\xff.png`) is classified by its extension like any
/// other.
///
/// ```
/// use norte_frontend::viewer::{ImageFmt, image_format_by_name};
/// assert_eq!(image_format_by_name(b"snapshot.JPG"), Some(ImageFmt::Jpeg));
/// assert_eq!(image_format_by_name(b"notes.md"), None);
/// assert_eq!(image_format_by_name(b"sin_extension"), None);
/// ```
#[must_use]
pub fn image_format_by_name(name: &[u8]) -> Option<ImageFmt> {
    let ext = name
        .iter()
        .rposition(|&b| b == b'.')
        .and_then(|dot| std::str::from_utf8(&name[dot + 1..]).ok())
        .map(str::to_ascii_lowercase);
    Some(match ext.as_deref()? {
        "png" => ImageFmt::Png,
        "jpg" | "jpeg" => ImageFmt::Jpeg,
        "gif" => ImageFmt::Gif,
        "bmp" => ImageFmt::Bmp,
        "webp" => ImageFmt::Webp,
        _ => return None,
    })
}

/// Which class a sibling belongs to, which is what decides whether "next"
/// stops at it or skips it.
///
/// Two classes and no more: browsing photos, photos are wanted, and reading a
/// file, the next file is wanted. A finer taxonomy (by mimetype, for
/// instance) would sound better and would be noticed for the worse — the
/// reader would be left guessing why their `.md` does not lead to their
/// `.txt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// An image of the kind this viewer knows how to paint.
    Imagen,
    /// Everything else that can be opened: text, binary, whatever.
    Other,
}

/// A name's class, by extension. See [`image_format_by_name`] for why the
/// name rules here and not the content.
///
/// ```
/// use norte_frontend::viewer::{Class, class_by_name};
/// assert_eq!(class_by_name(b"snapshot.png"), Class::Imagen);
/// assert_eq!(class_by_name(b"LEEME"), Class::Other);
/// ```
#[must_use]
pub fn class_by_name(name: &[u8]) -> Class {
    if image_format_by_name(name).is_some() {
        Class::Imagen
    } else {
        Class::Other
    }
}

/// The index of the NEXT (or previous) sibling of the requested class,
/// within the listing the user is looking at.
///
/// Three decisions, and each one covers something that gets noticed:
///
/// - **The class is REQUESTED, not deduced from the starting candidate.**
///   The caller passes the open viewer's, which knows it from its bytes
///   ([`Viewer::is_image`]), so a photo saved as `.dat` still leads to the
///   next photo.
/// - **Only REGULAR files are siblings.** It is not just that a directory is
///   not one (which it also is not, `..` row included: entering a folder
///   already has its own key): a link or something the provider does not
///   classify is not one either. The viewer refuses to read "whatever it
///   is" —that is how an automatic preview ends up opening a block device—
///   so the ladder cannot lead to a fifo called `dump.png` that `pane.view`
///   itself would not open.
/// - **It does not wrap.** On reaching the end it answers `None` and the
///   caller says so; wrapping around silently leaves the reader not knowing
///   they already saw them all, and going back to the first one looks like
///   nothing happened.
///
/// - **It only walks what the reader SEES.** `visible` is the indices the
///   quick search filter leaves on screen
///   ([`crate::PaneState::quick_visible`]); `None` = no filter, and then the
///   ladder is the whole listing. Without this, with a live filter
///   `viewer.next` opened a file that was not in the list the reader had
///   just narrowed down — and the help promised exactly the opposite.
///
/// The order is the listing's AS SEEN —already sorted by the caller—, which
/// is the only ladder the reader can predict. `from` is always an index
/// into `entries`, filter or not; if under a filter that row is not visible,
/// there is no ladder to walk and the answer is `None`.
///
/// ```
/// use norte_frontend::viewer::{Class, sibling};
/// use norte_proto::{Entry, EntryKind, VPath};
///
/// let row = |wire: &str, kind| Entry {
///     attrs: std::collections::BTreeMap::new(),
///     path: VPath::parse(wire).unwrap(),
///     kind,
///     size: None,
///     mtime_ms: None,
/// };
/// let listing = [
///     row("mem:///a.jpg", EntryKind::File),
///     row("mem:///notes.md", EntryKind::File),
///     row("mem:///b.png", EntryKind::File),
/// ];
/// // From the photo, "next image" skips the text in between.
/// assert_eq!(sibling(&listing, None, 0, true, Class::Imagen), Some(2));
/// // And from the last one there is nothing more: it does not go back to the first.
/// assert_eq!(sibling(&listing, None, 2, true, Class::Imagen), None);
/// // With a filter that leaves only the first one, there is no next.
/// assert_eq!(sibling(&listing, Some(&[0]), 0, true, Class::Imagen), None);
/// ```
#[must_use]
pub fn sibling(
    entries: &[Entry],
    visible: Option<&[usize]>,
    from: usize,
    forward: bool,
    wanted: Class,
) -> Option<usize> {
    let step = |i: usize| {
        if forward {
            i.checked_add(1)
        } else {
            i.checked_sub(1)
        }
    };
    // The same question for both walks: only a regular file, and of the
    // requested class. An index outside the listing answers no.
    let valid = |i: usize| {
        entries.get(i).is_some_and(|e| {
            e.kind == EntryKind::File
                && class_by_name(
                    e.path
                        .file_name()
                        .map_or(&[][..], norte_proto::Segment::as_bytes),
                ) == wanted
        })
    };
    match visible {
        // No filter: the ladder is the listing's indices.
        None => {
            let mut i = from;
            loop {
                i = step(i)?;
                // The bound: without this, advancing past the end would
                // never stop.
                entries.get(i)?;
                if valid(i) {
                    return Some(i);
                }
            }
        }
        // With a filter: it walks by POSITION within what is visible, and
        // what gets returned is still the listing's real index.
        Some(vis) => {
            let mut p = vis.iter().position(|&real| real == from)?;
            loop {
                p = step(p)?;
                let &i = vis.get(p)?;
                if valid(i) {
                    return Some(i);
                }
            }
        }
    }
}

/// A preview's pixel budget: 40 megapixels.
///
/// It is not an aesthetic limit but a MEMORY one. A 64 KB PNG can declare
/// 60000×60000 and cost the decoder gigabytes: this is the decompression
/// bomb, and the only cheap defense is to read its header and refuse BEFORE
/// handing the bytes to anything that decodes. Forty megapixels comfortably
/// cover any real photo (a 50 Mpx one is a high-end sensor) and are ~160 MB
/// in RGBA, which is expensive but not lethal.
pub const PIXEL_BUDGET: u64 = 40_000_000;

/// The dimensions the header DECLARES, without decoding anything.
///
/// `None` when the format is not recognised, when the header is incomplete,
/// or when it says something that makes no sense: the caller treats that
/// `None` as "nothing about this image can be promised" and refuses, which
/// is the opposite of treating it as "go ahead".
///
/// Reads FIXED OFFSETS and allocates nothing based on what it reads. This is
/// code that looks at a third party's bytes to decide a number, so it is
/// deliberately the boring kind: no loops over file lengths except JPEG's
/// bounded segment walk, and no arithmetic that could overflow (everything
/// in `u64`).
///
/// ```
/// use norte_frontend::viewer::image_dimensions;
/// // A minimal PNG: signature, IHDR length, type, and width/height.
/// let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
/// png.extend_from_slice(&[0, 0, 0, 13]);
/// png.extend_from_slice(b"IHDR");
/// png.extend_from_slice(&800u32.to_be_bytes());
/// png.extend_from_slice(&600u32.to_be_bytes());
/// assert_eq!(image_dimensions(&png), Some((800, 600)));
/// assert_eq!(image_dimensions(b"not an image"), None);
/// ```
#[must_use]
pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    match image_format(bytes)? {
        // IHDR is ALWAYS the first chunk and sits at a fixed offset.
        ImageFmt::Png => (bytes.len() >= 24 && &bytes[12..16] == b"IHDR")
            .then(|| (be32(bytes, 16), be32(bytes, 20))),
        // Logical Screen Descriptor, little-endian, right after the signature.
        ImageFmt::Gif => {
            (bytes.len() >= 10).then(|| (u32::from(le16(bytes, 6)), u32::from(le16(bytes, 8))))
        }
        // DIB header: signed width and height; a NEGATIVE height means
        // top-to-bottom rows, not a negatively sized image.
        ImageFmt::Bmp => (bytes.len() >= 26).then(|| {
            (
                le32(bytes, 18).unsigned_abs(),
                le32(bytes, 22).unsigned_abs(),
            )
        }),
        ImageFmt::Webp => webp_dimensions(bytes),
        ImageFmt::Jpeg => jpeg_dimensions(bytes),
    }
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// WebP has THREE flavors and each one keeps the size somewhere else. One
/// that is not recognised is `None`: refusing is the correct answer to "I do
/// not know".
fn webp_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 30 {
        return None;
    }
    match &b[12..16] {
        // Simple lossy: VP8's keyframe carries 14 bits per axis.
        b"VP8 " => Some((
            u32::from(le16(b, 26) & 0x3FFF),
            u32::from(le16(b, 28) & 0x3FFF),
        )),
        // Lossless: 14 bits per axis, packed and minus one.
        b"VP8L" => {
            let v = u32::from_le_bytes([b[21], b[22], b[23], b[24]]);
            Some(((v & 0x3FFF) + 1, ((v >> 14) & 0x3FFF) + 1))
        }
        // Extended: 24 bits per axis, minus one.
        // The `+ 1` belongs to the integer VALUE, not the last shift:
        // without the parentheses it binds to the `<< 16` and the width
        // comes out wrong by 65536. clippy caught it, and it is exactly the
        // class of error this function is forbidden from having.
        b"VP8X" => Some((
            (u32::from(b[24]) | (u32::from(b[25]) << 8) | (u32::from(b[26]) << 16)) + 1,
            (u32::from(b[27]) | (u32::from(b[28]) << 8) | (u32::from(b[29]) << 16)) + 1,
        )),
        _ => None,
    }
}

/// JPEG has no fixed offset: segments have to be walked up to the SOF. The
/// walk is BOUNDED by the length of what was read and always advances, so it
/// cannot get stuck spinning over a tampered file.
fn jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize;
    while i + 9 < b.len() {
        if b[i] != 0xFF {
            // Out of sync: it is not guessed at, it is abandoned.
            return None;
        }
        let marker = b[i + 1];
        // SOF0..SOF15, skipping the ones that carry no size (DHT, JPG, DAC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return Some((
                u32::from(u16::from_be_bytes([b[i + 7], b[i + 8]])),
                u32::from(u16::from_be_bytes([b[i + 5], b[i + 6]])),
            ));
        }
        let length = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
        if length < 2 {
            return None;
        }
        i += 2 + length;
    }
    None
}

/// A plugin-produced preview (M4-P5): replaces the raw view while it is
/// present. Styled lines (#29): the plugin's output is parsed with the
/// ANSI-SGR sanitiser ([`crate::ansi::parse_sgr`]) — which DISCARDS any
/// dangerous escape and keeps only foreground color — and each span is
/// masked ([`crate::display_name`]): THIRD-PARTY text, never trusted.
pub struct PluginPreviewView {
    /// The plugin previewer's readable name (already masked), for the
    /// indicator.
    pub plugin_name: String,
    /// The host-side decoding of the file was LOSSY (#101, the wire's
    /// `PluginPreview::lossy`): the `�`s in the output come from a failed
    /// decoding, not from the file. The frontend reads it through
    /// [`Viewer::preview_lossy`] and flags it next to the "via …" indicator.
    /// Private: only the `Viewer`'s two constructors set it.
    lossy: bool,
    /// Colored lines, sanitised and masked.
    styled: Vec<crate::ansi::StyledLine>,
}

/// The viewer open over a file.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about the open file: whether it was \
              truncated, whether it shows in hexadecimal, whether decoding \
              gave errors, and whether the BYTES were an image's. They are \
              not states of a machine —they can occur in any combination— \
              and grouping them into two-variant enums would only rename \
              the same boolean"
)]
pub struct Viewer {
    /// The displayed file.
    pub path: VPath,
    /// What was read (possibly truncated to the viewer's budget).
    bytes: Vec<u8>,
    /// `true` if the file kept going (only the header was read).
    pub truncated: bool,
    /// Hexview active (automatic on NON-image binaries; manual toggle).
    pub hex: bool,
    /// Image format recognised by magic bytes (`recompute`), or `None`. When
    /// it is `Some` and there is no forced encoding, the viewer is in image
    /// mode: the GUI paints it ([`Viewer::is_image`]); frontends with no
    /// image render (TUI) fall back to hexview (the bytes stay available
    /// through `rows`).
    image: Option<ImageFmt>,
    /// Whether the BYTES read were an image's, REGARDLESS of what this
    /// viewer ends up painting.
    ///
    /// Kept apart from `image` because `image` answers "what am I
    /// painting" and turns off with a forced encoding or when a plugin
    /// previewer replaces the whole view; this answers "what the file IS",
    /// which does not change for either of those two things. Set by
    /// [`Viewer::new`] and carried over by [`Viewer::set_image_by_bytes`] in
    /// the preview constructors, which receive no bytes.
    image_bytes: bool,
    /// Encoding forced by "reload as…" (None = detection).
    forced: Option<&'static norte_encoding::Encoding>,
    /// First visible line.
    pub scroll: usize,
    /// First visible COLUMN, in terminal cells.
    ///
    /// Without this, a line wider than the window —a minified HTML, a CSV, a
    /// log— painted truncated and the rest was unreachable: the viewer does
    /// not wrap, so what does not fit is nowhere.
    ///
    /// Each mode has its own width, so switching modes resets it to zero: a
    /// column from the previous view does not name the same column here.
    hscroll: usize,
    /// The widest TEXT line, in cells.
    ///
    /// The real cap comes from [`Self::max_cols`], which also knows about
    /// the hexadecimal — this field is only the text half of that answer.
    ///
    /// Measured once on decoding and not per window. Measuring only what is
    /// visible would make the cap change when scrolling down, and the text
    /// would jump sideways without anyone pressing anything.
    max_cols_text: usize,
    /// The IMAGE's zoom, as a percentage of what it would take up fitted.
    /// `None` = fit, and that is how it always opens: the first thing wanted
    /// from an image is to see it whole.
    ///
    /// [`Viewer::ZOOM_ACTUAL`] is the "real size" sentinel. Lives here and
    /// not in the frontend because both need it the same way and because it
    /// is viewer state, like the hexadecimal or the forced encoding.
    zoom: Option<u16>,
    /// A plugin's preview (M4-P5): if present, REPLACES the raw view and the
    /// decoding (bytes/encoding are ignored; `lines` already masked).
    plugin_preview: Option<PluginPreviewView>,
    // ---- decoding cache (recomputed when the encoding changes) ----
    text: String,
    encoding_name: &'static str,
    eol: Eol,
    had_errors: bool,
    lines: usize,
}

impl Viewer {
    /// Base struct with every field at its zero (not decoded yet).
    fn base(path: VPath, bytes: Vec<u8>, truncated: bool) -> Self {
        Self {
            path,
            bytes,
            truncated,
            hex: false,
            image: None,
            image_bytes: false,
            forced: None,
            scroll: 0,
            hscroll: 0,
            max_cols_text: 0,
            // An image opens FITTED: the first thing wanted from it is to
            // see it whole.
            zoom: None,
            plugin_preview: None,
            text: String::new(),
            encoding_name: "",
            eol: Eol::None,
            had_errors: false,
            lines: 0,
        }
    }

    /// A viewer over `bytes` (already read): detects encoding and binary.
    #[must_use]
    pub fn new(path: VPath, bytes: Vec<u8>, truncated: bool) -> Self {
        let mut v = Self::base(path, bytes, truncated);
        v.recompute();
        // Set ONCE, here: `recompute` runs again when forcing an encoding
        // and then turns `image` off, but the file's bytes are the same and
        // so is its class.
        v.image_bytes = v.image.is_some();
        v
    }

    /// A viewer in plugin preview mode (M4-P5): paints the plugin's output
    /// instead of the raw view. `output` is THIRD-PARTY text: it goes
    /// through the ANSI-SGR sanitiser (#29 — [`crate::ansi::parse_sgr`]:
    /// discards dangerous escapes, keeps only foreground color) and EVERY
    /// span is masked with [`crate::display_name`] (controls/bidi/invisibles
    /// → `�`); `plugin_name` the same way.
    #[must_use]
    pub fn with_plugin_preview(
        path: VPath,
        plugin_name: String,
        output: &str,
        lossy: bool,
    ) -> Self {
        let styled: Vec<crate::ansi::StyledLine> = crate::ansi::parse_sgr(output)
            .into_iter()
            .map(|line| {
                line.into_iter()
                    .map(|span| crate::ansi::StyledSpan {
                        text: crate::display_name(span.text.as_bytes()).0,
                        role: None, // ANSI-SGR has no concept of role (G3a, see ansi.rs)
                        fg: span.fg,
                        bg: None,
                    })
                    .collect()
            })
            .collect();
        let plugin_name = crate::display_name(&plugin_name.into_bytes()).0;
        let mut v = Self::base(path, Vec::new(), false);
        v.max_cols_text = styled_width(&styled);
        v.plugin_preview = Some(PluginPreviewView {
            plugin_name,
            lossy,
            styled,
        });
        v
    }

    /// A viewer in STYLED plugin preview mode (G3a, ADR 0037): twin of
    /// [`Self::with_plugin_preview`] that consumes ALREADY STRUCTURED
    /// `lines` (`SpanWire`, from the `plugin.preview_styled` wire) instead
    /// of an ANSI-SGR output to sanitise. Every span's `text` is
    /// THIRD-PARTY text — masked the SAME WAY as the ANSI path
    /// (`crate::display_name`, same sanitisation, not a parallel copy);
    /// every `role` is a `norte_theme::Role` name that arrives UNVALIDATED
    /// over the wire (`norte-core` does not depend on `norte-theme` — see
    /// `Backend::plugin_preview_styled`'s rustdoc) and is validated HERE, the
    /// boundary where the frontend finally knows the theme
    /// (`norte_theme::Role::from_kebab_requestable`): an unknown name —or one
    /// from the window's CHROME or STATE, which since the 2026-09-11 spec
    /// (F2) a plugin cannot request— collapses to `None` — never a panic
    /// nor a free string another layer would have to re-interpret (ADR
    /// 0037, same criterion as a theme with partial data, ADR 0020). `fg` is
    /// the raw RGB fallback, already bounded by the wire (`[u8; 3]` always
    /// representable) — copied as-is; `bg` (0.66.0) the same, and no role
    /// beats it: a background is a background.
    #[must_use]
    pub fn with_plugin_preview_styled(
        path: VPath,
        plugin_name: String,
        lines: &[Vec<norte_proto::methods::SpanWire>],
        lossy: bool,
    ) -> Self {
        let styled: Vec<crate::ansi::StyledLine> = lines
            .iter()
            .map(|line| line.iter().map(crate::ansi::span_de_wire).collect())
            .collect();
        let plugin_name = crate::display_name(&plugin_name.into_bytes()).0;
        let mut v = Self::base(path, Vec::new(), false);
        v.max_cols_text = styled_width(&styled);
        v.plugin_preview = Some(PluginPreviewView {
            plugin_name,
            lossy,
            styled,
        });
        v
    }

    /// The visible rows of the STYLED plugin preview (#29), from `scroll`
    /// and from [`Self::hscroll`]; `None` if the viewer is not in plugin
    /// preview mode. The frontend translates [`crate::ansi::Rgb`] to its own
    /// color type and paints them.
    ///
    /// Returns OWNED lines and not references because the horizontal scroll
    /// splits spans: a plugin's output scrolls the same way as raw text — a
    /// CSV or JSON previewer produces long lines for the same reason the
    /// file does.
    #[must_use]
    pub fn plugin_styled_rows(&self, height: usize) -> Option<Vec<crate::ansi::StyledLine>> {
        self.plugin_preview.as_ref().map(|p| {
            p.styled
                .iter()
                .skip(self.scroll)
                .take(height)
                .map(|l| scroll_styled(l, self.hscroll))
                .collect()
        })
    }

    /// The plugin's name if the viewer is in preview mode (for the header's
    /// "via …" indicator), or `None` if it is the raw view.
    #[must_use]
    pub fn preview_plugin(&self) -> Option<&str> {
        self.plugin_preview.as_ref().map(|p| p.plugin_name.as_str())
    }

    /// Whether the encoding and line-ending marks describe the FILE (#380).
    /// `false` in plugin preview mode: the viewer holds the plugin's spans,
    /// not the file's bytes, and a frontend that paints the marks anyway
    /// calls a UTF-8 README «binary, no EOL».
    #[must_use]
    pub fn describes_bytes(&self) -> bool {
        self.plugin_preview.is_none()
    }

    /// `true` if the viewer is in plugin preview mode AND the file's
    /// host-side decoding was LOSSY (#101): the frontend paints a warning
    /// next to the "via …" indicator. `false` for the raw view (which flags
    /// its own [`Self::had_errors`]) or for a non-lossy preview.
    #[must_use]
    pub fn preview_lossy(&self) -> bool {
        self.plugin_preview.as_ref().is_some_and(|p| p.lossy)
    }

    /// Re-decodes the bytes and updates everything derived.
    ///
    /// With a plugin preview in front it DOES NOTHING, and that is a fix: in
    /// that mode `bytes` is empty, so detection said "empty text" and swept
    /// away the width measured from the plugin's output —meaning that
    /// pressing "reload as…" over a wide CSV's preview turned off its
    /// horizontal bar and capped the scroll at zero, silently. `total_rows`
    /// and `rows` already asked about the preview first; this was what was
    /// missing from the same rule.
    fn recompute(&mut self) {
        if self.plugin_preview.is_some() {
            return;
        }
        let encoding = self.forced.or(match norte_encoding::detect(&self.bytes) {
            Detection::Text { encoding, .. } => Some(encoding),
            Detection::Binary => None,
        });
        if let Some(enc) = encoding {
            // Forced = no BOM-sniffing (the user RULES, spec §6.2);
            // truncated = the split tail is left pending, it is not a loss.
            let Decoded {
                text,
                encoding,
                had_errors,
            } = if self.forced.is_some() {
                norte_encoding::decode_forced(&self.bytes, enc, !self.truncated)
            } else {
                norte_encoding::decode(&self.bytes, enc, !self.truncated)
            };
            self.eol = norte_encoding::detect_eol(&text);
            // For PAINTING: every EOL (incl. classic Mac CR) splits a line.
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.lines = text.lines().count();
            // The widest ALREADY rendered: tabs are expanded before
            // painting, so measuring the raw text would give a short cap and
            // leave the tail of a line with tabs unreachable.
            self.max_cols_text = text
                .lines()
                .map(|l| crate::display::cells(&render_line(l)))
                .max()
                .unwrap_or(0);
            self.text = text;
            self.encoding_name = encoding.name();
            self.had_errors = had_errors;
            self.hex = false;
            // Text (or forced to text): not an image to show as one.
            self.image = None;
        } else {
            // Binary: never decode blindly (spec §6) — hexview. It also
            // recognises whether it is an image (magic bytes) so the GUI can
            // PAINT it ([`Viewer::is_image`]); the hexview stays active as a
            // fallback for frontends with no image render (TUI) — `rows`
            // does not change.
            self.image = image_format(&self.bytes);
            self.hex = true;
            self.encoding_name = "";
            self.eol = Eol::None;
            self.had_errors = false;
            self.text = String::new();
            self.lines = 0;
            // With no text there is no text line to measure; the
            // hexadecimal view's cap is set by `max_cols`, which knows its
            // fixed width.
            self.max_cols_text = 0;
        }
        self.scroll = 0;
        self.hscroll = 0;
    }

    /// "Reload as…": next encoding in the cycle (spec §6). On a binary it
    /// forces the cycle's FIRST text decoding.
    pub fn cycle_encoding(&mut self) {
        let cycle = norte_encoding::reload_cycle();
        let next = match self.forced {
            None => 0,
            Some(cur) => cycle
                .iter()
                .position(|e| std::ptr::eq(*e, cur))
                .map_or(0, |i| (i + 1) % cycle.len()),
        };
        self.forced = Some(cycle[next]);
        self.recompute();
    }

    /// Goes back to automatic detection (and to hexview if it was binary).
    pub fn reset_encoding(&mut self) {
        self.forced = None;
        self.recompute();
    }

    /// Toggles the hexview manually (the decoded text is kept). The scroll
    /// gets re-clamped: row totals differ between modes.
    pub fn toggle_hex(&mut self) {
        self.hex = !self.hex;
        self.scroll = self.scroll.min(self.total_rows().saturating_sub(1));
        // And the horizontal one to zero: the two views have different
        // widths, so text column 40 is not dump column 40, and keeping it
        // would show one the reader did not choose.
        self.hscroll = 0;
    }

    /// Total visible rows in the current mode.
    #[must_use]
    pub fn total_rows(&self) -> usize {
        if let Some(p) = &self.plugin_preview {
            p.styled.len()
        } else if self.hex {
            self.bytes.len().div_ceil(HEX_COLS)
        } else {
            self.lines
        }
    }

    /// Scrolls down `n` rows (clamped).
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.total_rows().saturating_sub(1));
    }

    /// Scrolls up `n` rows.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// To the top.
    pub fn scroll_top(&mut self) {
        self.scroll = 0;
    }

    /// To the bottom.
    pub fn scroll_bottom(&mut self) {
        self.scroll = self.total_rows().saturating_sub(1);
    }

    /// The first visible column, in cells.
    #[must_use]
    pub const fn hscroll(&self) -> usize {
        self.hscroll
    }

    /// The widest line, in cells: how much there is sideways.
    ///
    /// The frontend compares it against its window's width to decide whether
    /// to draw a horizontal bar.
    ///
    /// **Depends on the MODE, same as [`Self::total_rows`].** The
    /// hexadecimal view has its own width —77 cells: offset, sixteen bytes,
    /// and its ASCII column— which does not fit in a split slot or a narrow
    /// terminal, and denying it the horizontal axis left that ASCII column
    /// unreachable. Reading the TEXT's width in hexadecimal mode was also a
    /// bar that moved over content that did not move.
    #[must_use]
    pub const fn max_cols(&self) -> usize {
        if self.hex {
            HEX_ROW_CELLS
        } else {
            self.max_cols_text
        }
    }

    /// Left `n` columns.
    pub const fn scroll_left(&mut self, n: usize) {
        self.hscroll = self.hscroll.saturating_sub(n);
    }

    /// Right `n` columns, without overshooting the longest line's end.
    ///
    /// The cap ALWAYS leaves one column in view: a viewer scrolled past all
    /// of its content is a blank screen from which the only way out is
    /// blind.
    pub fn scroll_right(&mut self, n: usize) {
        self.hscroll = (self.hscroll + n).min(self.max_cols().saturating_sub(1));
    }

    /// The visible rows from `scroll`, already formatted for the terminal:
    /// tabs EXPANDED (ratatui would delete them: columns collapsed silently)
    /// and the rest of the controls masked to `�` (a `.ans` with an ESC
    /// shows altered, never unmarked) — same policy as names (spec §6).
    ///
    /// And truncated on the LEFT to [`Self::hscroll`], in all THREE modes —
    /// text, hexadecimal, and plugin preview. The truncation happens here,
    /// over the already-rendered line, and not in each frontend: two
    /// truncations are two ways of counting columns that disagree some day.
    #[must_use]
    pub fn rows(&self, height: usize) -> Vec<String> {
        if let Some(p) = &self.plugin_preview {
            // Plain text PROJECTED from the styled truncation, not a second
            // truncation: the same line cannot paint differently with and
            // without color.
            p.styled
                .iter()
                .skip(self.scroll)
                .take(height)
                .map(|line| {
                    scroll_styled(line, self.hscroll)
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect()
                })
                .collect()
        } else if self.hex {
            hex_rows(&self.bytes, self.scroll, height)
                .into_iter()
                .map(|f| self.truncated_row(f))
                .collect()
        } else {
            self.text
                .lines()
                .skip(self.scroll)
                .take(height)
                .map(|l| self.truncated_row(render_line(l)))
                .collect()
        }
    }

    /// An already-rendered row, scrolled to [`Self::hscroll`].
    fn truncated_row(&self, row: String) -> String {
        if self.hscroll == 0 {
            return row;
        }
        crate::display::skip_cells(&row, self.hscroll)
    }

    /// The decoded encoding's name (`"UTF-8"`…), or `""` if binary.
    #[must_use]
    pub fn encoding_name(&self) -> &str {
        self.encoding_name
    }

    /// The detected line ending (only meaningful in text mode).
    #[must_use]
    pub fn eol(&self) -> norte_encoding::Eol {
        self.eol
    }

    /// There were losses while decoding (invalid bytes → `�`).
    #[must_use]
    pub fn had_errors(&self) -> bool {
        self.had_errors
    }

    /// The encoding is FORCED by "reload as…" (not the detection).
    #[must_use]
    pub fn is_forced(&self) -> bool {
        self.forced.is_some()
    }

    /// `true` if the content is a recognised image (magic bytes) and no text
    /// decoding has been forced ("reload as…"). The frontend with image
    /// render (GUI) PAINTS the image through this. The core does NOT decode:
    /// it only recognises. Note: it does NOT depend on `hex` — a graphical
    /// frontend always shows the image; the raw hexview stays available
    /// through `rows` for whoever cannot paint it.
    ///
    /// The TUI does NOT use this getter to decide whether to request pixels
    /// through the kitty protocol (phase 5 WOW): it becomes `false` the
    /// moment a plugin previewer replaces the raw view with its own
    /// `PluginPreview`, and that decision needs to know whether the FILE is
    /// an image regardless of which previewer won — it looks at the bytes on
    /// its own with [`image_format`] before the preview chain gets a chance
    /// to hide the format (`norte-tui::viewer_open`). When it does not paint
    /// pixels, it falls back to `rows` (hexview) as always.
    #[must_use]
    pub fn is_image(&self) -> bool {
        self.plugin_preview.is_none() && self.image.is_some()
    }

    /// Whether the BYTES that were read are an image's, whoever wins the
    /// preview chain.
    ///
    /// It is the different question [`Viewer::is_image`] does not answer:
    /// that one says "this viewer is PAINTING an image" and turns `false`
    /// the moment a plugin previewer replaces the raw view. To decide a
    /// sibling's class ([`sibling`]), it is necessary to know what the FILE
    /// is, which does not change because a plugin won — it is the same
    /// distinction the TUI already made by hand, calling [`image_format`]
    /// over the bytes before letting the previewers act.
    #[must_use]
    pub fn is_image_by_bytes(&self) -> bool {
        self.image_bytes
    }

    /// Carries the byte-based verdict over to a viewer built WITHOUT them.
    ///
    /// The plugin preview constructors receive the plugin's output and never
    /// the file, so they cannot find it out on their own: whoever calls them
    /// does have the bytes (or the previous viewer) at hand and says so
    /// here. Without this, opening a photo with an approved image previewer
    /// left it classified as "not an image", and [`sibling`]'s reel skipped
    /// right past EVERY photo.
    pub fn set_image_by_bytes(&mut self, si: bool) {
        self.image_bytes = si;
    }

    /// The recognised image format (for the status bar), or `None` if not in
    /// image mode. Only `Some` when [`Viewer::is_image`].
    #[must_use]
    pub fn image_kind(&self) -> Option<ImageFmt> {
        self.is_image().then_some(self.image).flatten()
    }

    /// The image's raw bytes for the frontend to decode (rule 7: the core
    /// does not decode), or `None` if not in image mode. They can be
    /// TRUNCATED (`self.truncated`): the frontend's decoder must tolerate a
    /// failed decode and fall back to an error state, not break.
    #[must_use]
    pub fn image_bytes(&self) -> Option<&[u8]> {
        self.is_image().then_some(self.bytes.as_slice())
    }

    /// The image's zoom: `None` = FIT, which is how it opens.
    ///
    /// A percentage and not a factor because that is what shows on the
    /// status bar, and because the step cycle is written in percentages.
    #[must_use]
    pub fn zoom(&self) -> Option<u16> {
        self.zoom
    }

    /// The zoom as a percentage against the FITTED size, already resolved:
    /// what multiplies the space the image would occupy alone.
    ///
    /// Fitting is 100%, so whoever paints always multiplies and does not
    /// have to know that `None` means anything.
    #[must_use]
    pub fn zoom_pct(&self) -> u16 {
        self.zoom.unwrap_or(100)
    }

    /// Zooms in one step.
    pub fn zoom_in(&mut self) {
        self.zoom = Some(Self::step_up(self.zoom_pct()));
    }

    /// Zooms out one step.
    pub fn zoom_out(&mut self) {
        self.zoom = Some(Self::step_down(self.zoom_pct()));
    }

    /// Goes back to FIT: the whole image within what there is.
    pub fn zoom_fit(&mut self) {
        self.zoom = None;
        self.scroll = 0;
        self.hscroll = 0;
    }

    /// Zoom-in cap, as a percentage of the fitted size.
    pub const ZOOM_MAX: u16 = 800;
    /// Zoom-out cap. Below it, the image stops saying anything.
    pub const ZOOM_MIN: u16 = 25;

    /// The steps. Moved up and down through, not by multiplying, so zooming
    /// in and out the same number of times returns to the SAME spot: with a
    /// factor, `100 × 1.25 ÷ 1.25` is 99 or 101 depending on rounding, and
    /// the reader ends up at a zoom they did not ask for and cannot leave.
    const STEPS: [u16; 9] = [25, 50, 75, 100, 150, 200, 300, 400, 800];

    fn step_up(pct: u16) -> u16 {
        Self::STEPS
            .into_iter()
            .find(|&e| e > pct)
            .unwrap_or(Self::ZOOM_MAX)
    }

    fn step_down(pct: u16) -> u16 {
        Self::STEPS
            .into_iter()
            .rev()
            .find(|&e| e < pct)
            .unwrap_or(Self::ZOOM_MIN)
    }
}

/// The viewer's tab width (fixed in M1).
const TAB_WIDTH: usize = 8;

/// Prepares a line for the terminal: tabs to spaces ([`TAB_WIDTH`] tab
/// stops) and the rest of the HAZARDS masked to `�` — SAME policy as names
/// (`is_terminal_hazard`: C0/C1 controls, bidi overrides/isolates,
/// invisibles, Zl/Zp separators, TAG chars). A plain `is_control()` let raw
/// bidi (U+202E) and invisibles (ZWSP) through: a `.txt` with an RLO forged
/// the visual order (Trojan Source, CVE-2021-42574) in the GPUI GUI —which
/// reorders bidi during shaping— and in terminals that honour bidi
/// (encoding-auditor GUI-d).
///
/// The tab stop is counted in CELLS, not characters: with `col += 1` per
/// character, an ideogram before a tab moved the stop by one column and the
/// rest of the line ended up misaligned relative to its neighbours.
fn render_line(line: &str) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    for c in line.chars() {
        if c == '\t' {
            let next = (col / TAB_WIDTH + 1) * TAB_WIDTH;
            for _ in col..next {
                out.push(' ');
            }
            col = next;
        } else if norte_encoding::is_terminal_hazard(c) {
            out.push('\u{FFFD}');
            col += 1;
        } else {
            out.push(c);
            // `unwrap_or(1)`: `None` is a CONTROL, and controls already went
            // through the branch above. One is painted taking up something.
            col += UnicodeWidthChar::width(c).unwrap_or(1);
        }
    }
    out
}

/// The widest styled line, in cells.
fn styled_width(styled: &[crate::ansi::StyledLine]) -> usize {
    styled
        .iter()
        .map(|l| l.iter().map(|s| crate::display::cells(&s.text)).sum())
        .max()
        .unwrap_or(0)
}

/// A styled line scrolled `n` cells to the left.
///
/// **This is the ONLY implementation of a row's left-side cut.** The plain
/// text version ([`Viewer::rows`] in preview mode) is its projection, not a
/// second count: when there were two, a cut landing right on a span's edge
/// cleaned the orphaned marks through one path and not the other, meaning
/// the same line painted differently with and without color.
///
/// Spans that stay whole to the left of the cut are dropped; the one that
/// crosses it is truncated while KEEPING its color, which is what tells
/// this function apart from truncating the whole string and losing the
/// spans. Spans left empty are not emitted: a `<span>` with no text paints
/// nothing and does dirty the DOM.
fn scroll_styled(line: &crate::ansi::StyledLine, n: usize) -> crate::ansi::StyledLine {
    if n == 0 {
        return line.clone();
    }
    let mut remaining = n;
    let mut cut = false;
    let mut out: crate::ansi::StyledLine = Vec::new();
    for span in line {
        let text = if cut {
            span.text.clone()
        } else {
            let width = crate::display::cells(&span.text);
            if remaining > 0 && width <= remaining {
                // This whole span stays to the left of the cut.
                remaining -= width;
                continue;
            }
            cut = true;
            crate::display::skip_cells(&span.text, remaining)
        };
        if text.is_empty() {
            continue;
        }
        out.push(crate::ansi::StyledSpan {
            text,
            ..span.clone()
        });
    }
    // The cut could have landed EXACTLY on a span's edge, and then
    // `skip_cells` had nothing to skip and cleaned nothing. The orphaned
    // mark is dropped all the same: who paints it cannot depend on where
    // the plugin put its color boundaries.
    if let Some(first) = out.first_mut() {
        let clean = crate::display::strip_leading_marks(&first.text);
        if clean.len() != first.text.len() {
            first.text = clean.to_owned();
        }
    }
    out.retain(|s| !s.text.is_empty());
    out
}

/// Hex view rows: `offset  hex×16  ascii`.
fn hex_rows(bytes: &[u8], scroll: usize, height: usize) -> Vec<String> {
    use std::fmt::Write;
    let mut out = Vec::new();
    for row in scroll..(scroll + height) {
        let start = row * HEX_COLS;
        if start >= bytes.len() {
            break;
        }
        let chunk = &bytes[start..(start + HEX_COLS).min(bytes.len())];
        let mut line = format!("{start:08x}  ");
        for (i, b) in chunk.iter().enumerate() {
            let _ = write!(line, "{b:02x} ");
            if i == 7 {
                line.push(' ');
            }
        }
        let hexw = 8 + 2 + HEX_COLS * 3 + 1;
        while line.len() < hexw + 2 {
            line.push(' ');
        }
        for b in chunk {
            line.push(if (0x20..0x7F).contains(b) {
                *b as char
            } else {
                '.'
            });
        }
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        Class, PIXEL_BUDGET, Viewer, class_by_name, image_dimensions, image_format_by_name, sibling,
    };
    use norte_proto::{Entry, EntryKind, VPath};

    /// A listing row with the name and kind asked for.
    fn row_entry(wire: &str, kind: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).unwrap(),
            kind,
            size: None,
            mtime_ms: None,
        }
    }

    /// A file listing, in the order they are written.
    fn listing(names: &[&str]) -> Vec<Entry> {
        names
            .iter()
            .map(|n| row_entry(&format!("mem:///{n}"), EntryKind::File))
            .collect()
    }

    fn vp() -> VPath {
        VPath::parse("file:///f").unwrap()
    }

    /// **The viewer scrolls WIDTH-wise.**
    ///
    /// The viewer does not wrap, so without this the tail of a long line —a
    /// minified HTML, a CSV, a log— was nowhere: it was painted truncated and
    /// there was no way to reach it.
    #[test]
    fn the_viewer_scrolls_horizontally_with_a_cap() {
        let text = b"0123456789\nshort\n".to_vec();
        let mut v = Viewer::new(vp(), text, false);
        assert_eq!(v.max_cols(), 10, "the widest line rules");
        assert_eq!(v.hscroll(), 0);
        assert_eq!(v.rows(2), vec!["0123456789", "short"]);

        v.scroll_right(4);
        assert_eq!(v.hscroll(), 4);
        assert_eq!(
            v.rows(2),
            vec!["456789", "t"],
            "every row is truncated at the same column"
        );

        // The cap ALWAYS leaves one column: scrolling past all the content
        // would be a blank screen from which the only way out is blind.
        v.scroll_right(1000);
        assert_eq!(v.hscroll(), 9);
        assert_eq!(v.rows(1), vec!["9"]);

        v.scroll_left(1000);
        assert_eq!(v.hscroll(), 0);
        assert_eq!(v.rows(1), vec!["0123456789"]);
    }

    /// The cap is measured on the ALREADY rendered line: tabs are expanded
    /// before painting, so measuring the raw text would leave its tail out
    /// of reach.
    #[test]
    fn the_horizontal_cap_counts_expanded_tabs() {
        let v = Viewer::new(vp(), b"\tab\n".to_vec(), false);
        assert_eq!(v.max_cols(), 10, "a tab is 8 columns, then `ab`");
    }

    /// **Hex has its OWN width, and it scrolls.**
    ///
    /// Its rows are 77 cells wide: in a split pane or a narrow terminal the
    /// ASCII column on the right does not fit, and denying it the
    /// horizontal axis left it out of reach — exactly the fault this work
    /// fixes for text.
    ///
    /// And the cap depends on the MODE, not the text. Reading the text's
    /// width while in hex was also a scrollbar moving over content that did
    /// not move: a text file with 200-column lines, switched to hex, drew a
    /// scrollbar and moved the thumb without the rows changing.
    #[test]
    fn hex_has_its_own_width_and_scrolls() {
        let mut v = Viewer::new(vp(), b"\x00\x01payload".to_vec(), false);
        assert!(v.hex);
        assert_eq!(v.max_cols(), 77, "offset + 16 bytes + their ASCII column");
        let whole = v.rows(1)[0].clone();
        assert!(whole.starts_with("00000000"));
        v.scroll_right(10);
        assert_eq!(v.hscroll(), 10);
        assert_eq!(
            v.rows(1)[0],
            whole[10..],
            "the dump is also truncated from the left"
        );

        // A wide TEXT file switched to hex declares the DUMP's width, not
        // the text's: otherwise the scrollbar promised 200 columns over
        // rows that are 77.
        let wide = format!("{}\n", "x".repeat(200)).into_bytes();
        let mut v = Viewer::new(vp(), wide, false);
        assert_eq!(v.max_cols(), 200);
        v.scroll_right(40);
        v.toggle_hex();
        assert_eq!(v.max_cols(), 77);
        assert_eq!(
            v.hscroll(),
            0,
            "and column 40 of the text is not column 40 of the dump"
        );
    }

    /// **A wide character split by the cut leaves its gap blank.**
    ///
    /// Half a cell cannot be painted, so the character goes away whole — but
    /// simply dropping it shifts that row one column relative to its
    /// neighbours, and the grid is exactly what an aligned CSV or log needs
    /// from horizontal scrolling.
    #[test]
    fn a_wide_character_split_by_the_cut_leaves_its_gap() {
        let mut v = Viewer::new(vp(), "漢字x\nabcde\n".as_bytes().to_vec(), false);
        assert_eq!(v.max_cols(), 5, "two two-cell ideographs and an `x`");
        v.scroll_right(1);
        assert_eq!(
            v.rows(2),
            vec![" 字x", "bcde"],
            "the lost ideograph's gap keeps the columns lined up"
        );
        v.scroll_right(1);
        assert_eq!(v.rows(2), vec!["字x", "cde"]);
    }

    /// And with a tab ahead of it, the stop is counted in CELLS: with
    /// `col += 1` per character, an ideograph before a tab moved the stop by
    /// one column and misaligned the rest of the line.
    #[test]
    fn the_tab_stop_is_counted_in_cells_not_characters() {
        let v = Viewer::new(vp(), "漢\tx\n".as_bytes().to_vec(), false);
        assert_eq!(
            v.rows(1),
            vec!["漢      x"],
            "the ideograph takes up TWO, so six spaces are missing to reach 8"
        );
        assert_eq!(v.max_cols(), 9);
    }

    #[test]
    fn binary_non_image_falls_to_hexview_and_the_toggle_comes_back() {
        // Binary that is NOT a recognised image → automatic hexview.
        let bin = b"\x00\x01\x02\x03NUL\x00\x00payload".to_vec();
        let mut v = Viewer::new(vp(), bin, false);
        assert!(v.hex, "NUL with no BOM = automatic hexview (spec §6)");
        assert!(!v.is_image(), "not a recognised image");
        let rows = v.rows(4);
        assert!(rows[0].starts_with("00000000"), "offset: {}", rows[0]);
        assert!(rows[0].contains("00 01 02 03"), "hex: {}", rows[0]);
        // Manual toggle: leaves hex (empty text on binary, but it is ITS
        // decision); x again brings it back.
        v.toggle_hex();
        assert!(!v.hex);
        v.toggle_hex();
        assert!(v.hex);
    }

    /// The header declares the size, and one that is not understood is
    /// REFUSED.
    ///
    /// Refusing what is not understood is half the value: the caller uses
    /// this to decide whether to hand the bytes to a decoder, and a `None`
    /// treated as "go ahead" would be the decompression bomb walking in
    /// through the door that exists to stop it.
    #[test]
    fn image_dimensions_reads_the_header_and_refuses_what_it_does_not_understand() {
        // PNG: IHDR at a fixed offset.
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1920u32.to_be_bytes());
        png.extend_from_slice(&1080u32.to_be_bytes());
        assert_eq!(image_dimensions(&png), Some((1920, 1080)));

        // GIF: little-endian after the signature.
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&640u16.to_le_bytes());
        gif.extend_from_slice(&480u16.to_le_bytes());
        gif.extend_from_slice(&[0, 0]);
        assert_eq!(image_dimensions(&gif), Some((640, 480)));

        // BMP: NEGATIVE height = top-down rows, not a negative size.
        let mut bmp = b"BM".to_vec();
        bmp.resize(18, 0);
        bmp.extend_from_slice(&300i32.to_le_bytes());
        bmp.extend_from_slice(&(-200i32).to_le_bytes());
        assert_eq!(image_dimensions(&bmp), Some((300, 200)));

        // JPEG: has to walk forward to the SOF0.
        let mut jpg = vec![0xFF, 0xD8];
        // An APP0 that has to be skipped.
        jpg.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        jpg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        jpg.extend_from_slice(&768u16.to_be_bytes());
        jpg.extend_from_slice(&1024u16.to_be_bytes());
        jpg.extend_from_slice(&[0; 8]);
        assert_eq!(image_dimensions(&jpg), Some((1024, 768)));

        // What is not understood is REFUSED, instead of guessed.
        assert_eq!(image_dimensions(b"not an image"), None);
        assert_eq!(
            image_dimensions(b"\x89PNG\r\n\x1a\n"),
            None,
            "an incomplete PNG header promises nothing"
        );
        let mut no_ihdr = b"\x89PNG\r\n\x1a\n".to_vec();
        no_ihdr.extend_from_slice(&[0, 0, 0, 13]);
        no_ihdr.extend_from_slice(b"iTXt");
        no_ihdr.resize(32, 0);
        assert_eq!(
            image_dimensions(&no_ihdr),
            None,
            "a PNG whose first chunk is not IHDR is not one we know how to read"
        );
        // A JPEG whose segments do not fit: it gives up, it does not loop.
        assert_eq!(
            image_dimensions(&[0xFF, 0xD8, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0]),
            None
        );
    }

    /// A header can DECLARE an impossible image, and that is the attack.
    #[test]
    fn a_header_can_declare_a_bomb() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&60000u32.to_be_bytes());
        png.extend_from_slice(&60000u32.to_be_bytes());
        let (w, h) = image_dimensions(&png).expect("the header is read");
        assert!(
            u64::from(w) * u64::from(h) > PIXEL_BUDGET,
            "36 gigapixels in 24 bytes of header: it is the decompression \
             bomb, and the budget exists to see it before anyone decodes"
        );
    }

    /// `image_format` recognises each supported format by MAGIC bytes and
    /// returns `None` for non-images and truncated headers.
    #[test]
    fn image_format_recognises_by_magic_bytes() {
        use super::{ImageFmt, image_format};
        assert_eq!(
            image_format(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR"),
            Some(ImageFmt::Png)
        );
        assert_eq!(
            image_format(b"\xFF\xD8\xFF\xE0\x00\x10JFIF"),
            Some(ImageFmt::Jpeg)
        );
        assert_eq!(image_format(b"GIF87a\x01\x00"), Some(ImageFmt::Gif));
        assert_eq!(image_format(b"GIF89a\x01\x00"), Some(ImageFmt::Gif));
        assert_eq!(image_format(b"BM\x8a\x00\x00\x00"), Some(ImageFmt::Bmp));
        assert_eq!(
            image_format(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            Some(ImageFmt::Webp)
        );
        // Non-image (plain text) → None.
        assert_eq!(image_format(b"hello world\n"), None);
        // RIFF without a WEBP mark (e.g. WAV) → None.
        assert_eq!(image_format(b"RIFF\x24\x00\x00\x00WAVEfmt "), None);
        // Truncated PNG header (only 4 bytes) → None: the prefix is incomplete.
        assert_eq!(image_format(b"\x89PNG"), None);
        // Truncated RIFF (<12 bytes) → None with no panic from slicing.
        assert_eq!(image_format(b"RIFF\x24\x00\x00\x00"), None);
    }

    /// A recognised image is flagged as an image (`is_image`/`image_kind`/
    /// `image_bytes` expose it for the GUI to paint) WITHOUT losing the raw
    /// hexview available in `rows` (the TUI's fallback, which does not paint
    /// images). "reload as…" forces text and leaves image mode.
    #[test]
    fn a_recognised_image_is_flagged_and_keeps_the_fallback_hex() {
        use super::ImageFmt;
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
        let mut v = Viewer::new(vp(), png.clone(), false);
        assert!(v.is_image(), "PNG → image");
        assert_eq!(v.image_kind(), Some(ImageFmt::Png));
        assert_eq!(v.image_bytes(), Some(png.as_slice()));
        // The raw hexview stays available for frontends with no render (TUI).
        assert!(v.hex, "fallback hexview active");
        assert!(
            v.rows(1)[0].contains("89 50 4e 47"),
            "fallback hex available"
        );
        // "reload as…" forces text: leaves image mode.
        v.cycle_encoding();
        assert!(!v.is_image(), "forced to text → not an image");
        assert_eq!(v.image_kind(), None);
        assert_eq!(v.image_bytes(), None);
        v.reset_encoding();
        assert!(v.is_image(), "reset goes back to detection → image");
    }

    /// A TRUNCATED image is still recognised by its header (the magic bytes
    /// go at the start): `is_image` and `truncated` at the same time — the
    /// GUI decides whether the partial decode works or falls back to
    /// "unreadable image".
    #[test]
    fn a_truncated_image_is_recognised_by_the_header() {
        let png_head = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00".to_vec();
        let v = Viewer::new(vp(), png_head, true);
        assert!(v.is_image());
        assert!(v.truncated);
        assert!(v.image_bytes().is_some());
    }

    /// Audit H4/H5: EXPANDED tabs (ratatui would erase them) and masked
    /// ESC — never an unmarked alteration.
    #[test]
    fn expanded_tabs_and_masked_controls() {
        let v = Viewer::new(vp(), b"all:\n\tcc -o x x.c\n".to_vec(), false);
        let rows = v.rows(3);
        assert_eq!(rows[0], "all:");
        assert_eq!(rows[1], "        cc -o x x.c", "tab → 8 spaces");
        let v = Viewer::new(vp(), b"red:\x1b[31mX\n".to_vec(), false);
        assert_eq!(v.rows(2)[0], "red:\u{FFFD}[31mX", "ESC shown as \u{FFFD}");
    }

    /// Trojan Source (CVE-2021-42574): a valid UTF-8 TEXT file with RLO
    /// (U+202E), an isolate (U+2066), ZWSP (U+200B) and a Zl separator
    /// (U+2028) — the text viewer must mask them to `�`, not let them
    /// through (`is_control` only covered C0/C1; now `is_terminal_hazard`).
    /// GPUI reorders bidi.
    #[test]
    fn the_text_viewer_does_not_paint_raw_bidi_or_invisible() {
        let hostile = "needle \u{202E}elttahs\u{2066} z\u{200B}w\u{2028}end"
            .as_bytes()
            .to_vec();
        let v = Viewer::new(vp(), hostile, false);
        assert!(!v.hex, "it is UTF-8 text, not binary");
        assert!(
            !v.rows(8)
                .iter()
                .flat_map(|r| r.chars())
                .any(norte_encoding::is_terminal_hazard),
            "the text viewer must not paint raw bidi/invisibles"
        );
    }

    /// H7: toggling to hex with a high scroll re-clamps it (never a blank
    /// screen).
    #[test]
    fn toggle_hex_reclamps_the_scroll() {
        use std::fmt::Write;
        let mut text = String::new();
        for i in 0..100 {
            let _ = writeln!(text, "{i}");
        }
        let mut v = Viewer::new(vp(), text.into_bytes(), false);
        v.scroll_bottom();
        assert_eq!(v.scroll, 99);
        v.toggle_hex();
        assert!(v.scroll < v.total_rows(), "re-clamped: {}", v.scroll);
        assert!(!v.rows(5).is_empty(), "the hexview paints something");
    }

    /// M4-P5: in plugin preview mode, `rows()` paints the plugin's output
    /// (not the raw view), `preview_plugin()` gives the name, and the
    /// output —a THIRD PARTY's text— comes out MASKED (controls → `�`,
    /// never a raw byte).
    #[test]
    fn plugin_preview_replaces_the_view_and_masks() {
        let v = Viewer::with_plugin_preview(
            vp(),
            "Markdown".to_owned(),
            "line one\nline\u{7}two\nline three",
            false,
        );
        assert_eq!(v.preview_plugin(), Some("Markdown"));
        assert!(!v.preview_lossy(), "not lossy");
        assert_eq!(v.total_rows(), 3, "3 lines split on \\n");
        let rows = v.rows(10);
        assert_eq!(rows[0], "line one");
        assert_eq!(rows[2], "line three");
        assert_eq!(
            rows[1], "line\u{FFFD}two",
            "the plugin's \\u{{7}} control comes out masked, not raw"
        );
        assert!(
            !rows[1].contains('\u{7}'),
            "never the raw control byte: {:?}",
            rows[1]
        );
    }

    /// #101: the wire's `lossy` flag reaches `preview_lossy()` in both
    /// constructors (plain and styled), so the frontend paints the warning.
    #[test]
    fn preview_lossy_propagates_from_the_wire() {
        let plain = Viewer::with_plugin_preview(vp(), "P".to_owned(), "a\u{FFFD}b", true);
        assert!(plain.preview_lossy(), "plain lossy");
        let styled = Viewer::with_plugin_preview_styled(vp(), "P".to_owned(), &[], true);
        assert!(styled.preview_lossy(), "styled lossy");
        // The raw view (no preview) never reports lossy through this path.
        let raw = Viewer::new(vp(), b"hello".to_vec(), false);
        assert!(!raw.preview_lossy(), "raw view: preview_lossy = false");
    }

    /// G3a (ADR 0037): `with_plugin_preview_styled` masks the `text` of
    /// EVERY span the same way the ANSI path does (same
    /// `crate::display_name`, no parallel copy), including hostile bidi
    /// (RLO) — the raw byte never reaches `plugin_styled_rows`.
    #[test]
    fn preview_styled_masks_every_span_bidi_included() {
        use norte_proto::methods::SpanWire;
        let lines = vec![
            vec![SpanWire {
                text: "goo\u{7}d".to_owned(), // raw BEL
                role: None,
                fg: None,
                bg: None,
            }],
            vec![SpanWire {
                text: "a\u{202E}b".to_owned(), // RLO (hostile bidi)
                role: None,
                fg: None,
                bg: None,
            }],
        ];
        let v = Viewer::with_plugin_preview_styled(vp(), "Demo".to_owned(), &lines, false);
        assert_eq!(v.preview_plugin(), Some("Demo"));
        let rows = v.plugin_styled_rows(10).expect("styled preview mode");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].text, "goo\u{FFFD}d", "BEL masked");
        assert!(!rows[0][0].text.contains('\u{7}'), "never the raw BEL");
        assert!(
            !rows[1][0].text.contains('\u{202E}'),
            "never the raw RLO: {:?}",
            rows[1][0].text
        );
    }

    /// G3a: `role` is ALREADY VALIDATED against `norte_theme::Role` in the
    /// conversion. A recognised name (kebab-case) resolves to the `Role`;
    /// an UNKNOWN one (e.g. the `"number"`/`"keyword"` from
    /// `previewer-demo`'s mini-highlighter, which are deliberately NOT valid
    /// `Role`s — see its rustdoc) collapses to `None`, never panics nor lets
    /// the raw string through. `fg` ALWAYS travels as-is (it is the raw
    /// fallback, not something to validate against a closed set).
    #[test]
    fn preview_styled_validates_an_unknown_role_to_none() {
        use norte_proto::methods::SpanWire;
        let lines = vec![vec![
            SpanWire {
                text: "42".to_owned(),
                role: Some("number".to_owned()), // not a valid Role
                fg: None,
                bg: None,
            },
            SpanWire {
                text: "TODO".to_owned(),
                role: Some("keyword".to_owned()), // neither is this
                fg: Some([255, 200, 0]),
                bg: Some([0, 0, 64]),
            },
            SpanWire {
                text: "err".to_owned(),
                role: Some("hostile-badge".to_owned()), // this IS a valid Role
                fg: None,
                bg: None,
            },
        ]];
        let v = Viewer::with_plugin_preview_styled(vp(), "Demo".to_owned(), &lines, false);
        let rows = v.plugin_styled_rows(10).expect("styled preview mode");
        assert_eq!(rows[0][0].role, None, "unknown role → None, never a panic");
        assert_eq!(rows[0][1].role, None, "unknown role → None");
        assert_eq!(
            rows[0][1].fg,
            Some((255, 200, 0)),
            "fg travels as-is, unvalidated (it is not a Role)"
        );
        assert_eq!(
            rows[0][2].role,
            Some(norte_theme::Role::HostileBadge),
            "a recognised role resolves to the Role"
        );
    }

    /// M4-P5: scrolling operates on the preview's lines (caps included).
    #[test]
    fn plugin_preview_scrolls_over_its_lines() {
        use std::fmt::Write;
        let mut out = String::new();
        for i in 0..20 {
            let _ = writeln!(out, "l{i}");
        }
        let mut v = Viewer::with_plugin_preview(vp(), "P".to_owned(), out.trim_end(), false);
        assert_eq!(v.total_rows(), 20);
        v.scroll_bottom();
        assert_eq!(v.scroll, 19);
        v.scroll_down(5);
        assert_eq!(v.scroll, 19, "lower cap in the preview");
        v.scroll_top();
        assert_eq!(v.rows(2), vec!["l0", "l1"]);
    }

    #[test]
    fn cycle_and_reset_do_a_forced_round_trip() {
        let mut v = Viewer::new(
            VPath::parse("mem:///a.txt").unwrap(),
            b"hello\n".to_vec(),
            false,
        );
        assert!(!v.is_forced());
        v.cycle_encoding(); // "reload as…" → forced encoding
        assert!(v.is_forced());
        v.reset_encoding(); // back to automatic detection
        assert!(!v.is_forced());
    }

    #[test]
    fn classic_mac_cr_splits_lines() {
        // Bare CR (classic Mac) splits a line just like LF: 3 lines → 3 rows.
        let v = Viewer::new(
            VPath::parse("mem:///a.txt").unwrap(),
            b"a\rb\rc".to_vec(),
            false,
        );
        assert!(!v.hex);
        assert_eq!(v.total_rows(), 3);
    }

    #[test]
    fn getters_expose_the_state_for_the_frontends_status_bar() {
        let v = Viewer::new(
            VPath::parse("mem:///a.txt").unwrap(),
            b"hello\n".to_vec(),
            false,
        );
        assert_eq!(v.encoding_name(), "UTF-8");
        assert!(!v.is_forced());
        assert!(!v.had_errors());
        assert!(!v.hex);
    }

    /// **Flipping through photos flips through photos.**
    ///
    /// What was missing was not "open the next file", it was not having to
    /// leave the viewer between one photo and the next. A README in the
    /// middle of a roll must not interrupt that.
    #[test]
    fn the_next_sibling_skips_what_is_not_its_class() {
        let l = listing(&["a.jpg", "notes.md", "b.png", "c.webp"]);
        assert_eq!(sibling(&l, None, 0, true, Class::Imagen), Some(2));
        assert_eq!(sibling(&l, None, 2, true, Class::Imagen), Some(3));
        // And the other way round, by the same rule.
        assert_eq!(sibling(&l, None, 3, false, Class::Imagen), Some(2));
        assert_eq!(sibling(&l, None, 2, false, Class::Imagen), Some(0));
        // Reading text looks for text, and then the photos are what's left over.
        assert_eq!(sibling(&l, None, 1, true, Class::Other), None);
        assert_eq!(sibling(&l, None, 3, false, Class::Other), Some(1));
    }

    /// **It does not wrap**: at the end it says there is no more, instead of
    /// going back to the first and looking like the key did nothing.
    #[test]
    fn it_does_not_wrap_at_either_end() {
        let l = listing(&["a.png", "b.png"]);
        assert_eq!(
            sibling(&l, None, 1, true, Class::Imagen),
            None,
            "does not wrap around"
        );
        assert_eq!(
            sibling(&l, None, 0, false, Class::Imagen),
            None,
            "nor backward"
        );
        // And an index outside the listing is not a panic, it is "there is none".
        assert_eq!(sibling(&l, None, 99, true, Class::Imagen), None);
        assert_eq!(sibling(&l, None, 99, false, Class::Imagen), None);
        assert_eq!(sibling(&[], None, 0, true, Class::Imagen), None);
    }

    /// **A directory is not a sibling**, and that includes the `..` row the
    /// listing carries in front: entering a folder has its own key, and
    /// this is not it.
    #[test]
    fn directories_are_not_siblings_and_that_includes_the_parent_row() {
        let l = vec![
            row_entry("mem:///casa", EntryKind::Dir), // the `..` row
            row_entry("mem:///casa/a.png", EntryKind::File),
            row_entry("mem:///casa/fotos", EntryKind::Dir),
            // A symlink or a fifo with a photo's name doesn't count EITHER:
            // the viewer refuses to read "whatever it is", and a ladder that
            // steps on its own cannot lead into a block device named
            // `dump.png`.
            row_entry("mem:///casa/link.png", EntryKind::Symlink),
            row_entry("mem:///casa/pipe.png", EntryKind::Other),
            row_entry("mem:///casa/b.png", EntryKind::File),
        ];
        assert_eq!(
            sibling(&l, None, 1, true, Class::Imagen),
            Some(5),
            "skips the folder, the symlink and the fifo"
        );
        assert_eq!(
            sibling(&l, None, 1, false, Class::Imagen),
            None,
            "and backward there is only the `..` row, which is not a sibling"
        );
    }

    /// **The class is what the caller asks for**, which is what makes a
    /// photo saved with the wrong extension still lead to the next photo:
    /// the viewer knows from its BYTES that what it has open is an image,
    /// even if the name does not say so.
    #[test]
    fn the_class_is_what_the_caller_asks_for_not_the_starting_extension() {
        let l = listing(&["roll.dat", "b.png"]);
        assert_eq!(
            sibling(&l, None, 0, true, Class::Imagen),
            Some(1),
            "opened as an image by its bytes, looks for images"
        );
        assert_eq!(
            sibling(&l, None, 0, true, Class::Other),
            None,
            "and the same row, read as text, has no text siblings"
        );
    }

    /// The extension is read case-insensitively and over BYTES (rule 1): a
    /// non-UTF-8 stem with an ASCII extension is classified just like any
    /// other.
    #[test]
    fn the_extension_rules_in_any_case_and_over_raw_bytes() {
        assert_eq!(class_by_name(b"FOTO.JPG"), Class::Imagen);
        assert_eq!(class_by_name(b"foto.JpEg"), Class::Imagen);
        assert_eq!(class_by_name(b"caf\xe9\xff.png"), Class::Imagen);
        assert_eq!(class_by_name(b"sin_extension"), Class::Other);
        assert_eq!(class_by_name(b"archivo.tar.gz"), Class::Other);
        assert_eq!(
            class_by_name(b".png"),
            Class::Imagen,
            "hidden, but an image"
        );
        // An extension that is not UTF-8 matches nothing.
        assert_eq!(image_format_by_name(b"x.p\xffg"), None);
    }

    /// The five formats the viewer knows how to paint each have their
    /// extension, and the two recognisers —bytes and name— name exactly the
    /// same set.
    #[test]
    fn the_by_name_twin_covers_the_five_formats() {
        use super::ImageFmt;
        for (name, fmt) in [
            (&b"a.png"[..], ImageFmt::Png),
            (b"a.jpg", ImageFmt::Jpeg),
            (b"a.jpeg", ImageFmt::Jpeg),
            (b"a.gif", ImageFmt::Gif),
            (b"a.bmp", ImageFmt::Bmp),
            (b"a.webp", ImageFmt::Webp),
        ] {
            assert_eq!(image_format_by_name(name), Some(fmt), "{name:?}");
        }
    }
}
