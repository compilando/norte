//! `org.norte.media-info`: image dimensions and audio duration as columns,
//! from file HEADERS read under the location token.
//!
//! The decisions live in [`image`], [`audio`] and [`format`], pure functions
//! over bytes with their own tests. The WIT glue only exists when compiled
//! as a component.

pub mod audio;
pub mod format;
pub mod image;

/// At most this many bytes of any file, through `read-prefix`. Every header
/// this plugin understands fits; a JPEG whose frame marker sits later than
/// this gets an empty cell, which is "cannot tell" and correct.
pub const PREFIX_MAX: u64 = 64 * 1024;

/// What a name's extension claims, and therefore which column may open it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    Image,
    Audio,
}

/// From the extension alone — last dot, bytes, ASCII case-insensitive. A
/// file this says nothing about is never opened.
pub fn media_of(name: &[u8]) -> Option<Media> {
    let i = name.iter().rposition(|b| *b == b'.')?;
    if i == 0 {
        return None;
    }
    let ext: Vec<u8> = name[i + 1..].iter().map(u8::to_ascii_lowercase).collect();
    match ext.as_slice() {
        b"png" | b"jpg" | b"jpeg" | b"gif" | b"webp" => Some(Media::Image),
        b"wav" | b"mp3" | b"flac" => Some(Media::Audio),
        _ => None,
    }
}

#[cfg(target_arch = "wasm32")]
mod guest {
    wit_bindgen::generate!({
        world: "norte-columns",
        path: "wit",
        generate_all,
    });

    use exports::norte::plugin::columns::{Guest as ColumnsGuest, LocationRef};
    use norte::location::location;

    use crate::{audio, format, image, media_of, Media, PREFIX_MAX};

    struct MediaInfo;

    impl ColumnsGuest for MediaInfo {
        fn column_values(
            id: String,
            location: Option<LocationRef>,
            entries: Vec<Vec<u8>>,
        ) -> Vec<Option<String>> {
            let wanted = match id.as_str() {
                "dims" => Media::Image,
                "duration" => Media::Audio,
                _ => return entries.iter().map(|_| None).collect(),
            };
            // Without a location there is nothing to read, and empty cells
            // are the right answer: the panel keeps painting.
            let Some(loc) = location else {
                return entries.iter().map(|_| None).collect();
            };
            entries
                .iter()
                .map(|name| {
                    if media_of(name) != Some(wanted) {
                        return None;
                    }
                    let mut rel = loc.prefix.clone();
                    if !rel.is_empty() && rel.last() != Some(&b'/') {
                        rel.push(b'/');
                    }
                    rel.extend_from_slice(name);
                    let head = location::read_prefix(&loc.token, &rel, PREFIX_MAX).ok()?;
                    match wanted {
                        Media::Image => {
                            let (w, h) = image::dims(&head)?;
                            Some(format::dims_cell(w, h))
                        }
                        Media::Audio => {
                            let len = location::stat(&loc.token, &rel).ok()?.size;
                            let secs = audio::duration_secs(&head, len)?;
                            Some(format::duration_cell(secs))
                        }
                    }
                })
                .collect()
        }
    }

    export!(MediaInfo);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_media_extensions_are_ever_opened() {
        assert_eq!(media_of(b"photo.PNG"), Some(Media::Image));
        assert_eq!(media_of(b"song.mp3"), Some(Media::Audio));
        assert_eq!(media_of(b"notes.txt"), None);
        assert_eq!(media_of(b".png"), None, "a dotfile has no extension");
        assert_eq!(media_of(b"x"), None);
    }
}
