//! `org.norte.image-ansi`: pictures as half-block cells in norte's viewer.
//!
//! Fitting and encoding live in [`render`], pure and tested on the host.
//! Decoding is the `image` crate; the WIT glue only exists when compiled as
//! a component.

pub mod render;

/// What the host reads of a file for a preview. A `content` this long may be
/// truncated, and a truncated picture decodes to garbage or not at all, so
/// it is refused with a message instead.
pub const HOST_READ_CAP: usize = 1024 * 1024;

/// Decoder bounds: a 1 MiB PNG can declare a 100 000² canvas; the guest has
/// 64 MiB and ten seconds, and a preview does not need more than this.
const MAX_SIDE: u32 = 8192;
const MAX_ALLOC: u64 = 48 * 1024 * 1024;

/// Decodes `bytes` (first frame of a GIF), blends transparency over black,
/// fits the picture to `columns` and encodes it. `Err` is a message for the
/// viewer, not a crash.
///
/// # Errors
///
/// A picture the decoder rejects, one over the decoder's bounds, or one the
/// host truncated.
pub fn render_bytes(bytes: &[u8], columns: Option<u32>) -> Result<Vec<Vec<render::Span>>, String> {
    if bytes.len() >= HOST_READ_CAP {
        return Err("picture over 1 MiB: the preview would be a truncated file".to_owned());
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("unreadable picture: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let img = reader
        .decode()
        .map_err(|e| format!("undecodable picture: {e}"))?;
    let (w, h) = render::fit(img.width(), img.height(), columns)
        .ok_or_else(|| "empty picture".to_owned())?;
    // Box sampling (`thumbnail`), not a triangle filter: the picture only
    // ever shrinks, a box averages exactly the pixels a cell covers, and it
    // does not bleed a row into its neighbour the way a wider kernel does.
    let small = image::imageops::thumbnail(&img.to_rgba8(), w, h);
    let pixels: Vec<(u8, u8, u8)> = small
        .pixels()
        .map(|p| {
            let [r, g, b, a] = p.0;
            let over = |c: u8| ((u16::from(c) * u16::from(a)) / 255) as u8;
            (over(r), over(g), over(b))
        })
        .collect();
    Ok(render::encode(&pixels, w, h))
}

#[cfg(target_arch = "wasm32")]
mod guest {
    wit_bindgen::generate!({
        world: "norte-plugin",
        path: "wit",
        generate_all,
    });

    use exports::norte::plugin::command::Guest as CommandGuest;
    use exports::norte::plugin::previewer::{Guest as PreviewerGuest, PreviewInput, Span};

    struct ImageAnsi;

    impl PreviewerGuest for ImageAnsi {
        fn render(input: PreviewInput) -> Result<String, String> {
            // The plain twin has no colours to give: say what the picture
            // is, so the viewer shows something rather than bytes.
            let img = image::ImageReader::new(std::io::Cursor::new(&input.content))
                .with_guessed_format()
                .map_err(|e| e.to_string())?;
            let format = img.format().map_or("image", |f| f.extensions_str()[0]);
            let (w, h) = img.into_dimensions().map_err(|e| e.to_string())?;
            Ok(format!(
                "{format} {w}×{h} (styled preview needed for the picture)"
            ))
        }

        fn render_styled(input: PreviewInput) -> Result<Vec<Vec<Span>>, String> {
            let lines = match crate::render_bytes(&input.content, input.columns) {
                Ok(lines) => lines,
                // A refusal is a line the viewer can show, not a fall to
                // the raw view: the reader asked for a picture.
                Err(msg) => {
                    return Ok(vec![vec![Span {
                        text: msg,
                        role: Some("info".to_owned()),
                        fg: None,
                        bg: None,
                    }]]);
                }
            };
            Ok(lines
                .into_iter()
                .map(|line| {
                    line.into_iter()
                        .map(|s| Span {
                            text: s.text,
                            role: None,
                            fg: s.fg,
                            bg: s.bg,
                        })
                        .collect()
                })
                .collect())
        }
    }

    impl CommandGuest for ImageAnsi {
        fn run(id: String, _arg: String) -> Result<String, String> {
            Err(format!("unknown command `{id}`: this plugin only previews"))
        }
    }

    export!(ImageAnsi);
}
