//! `org.norte.markdown`: Markdown as styled lines in norte's viewer.
//!
//! The rendering lives in [`render`], pure and tested on the host. The WIT
//! glue only exists when compiled as a component.

pub mod render;

#[cfg(target_arch = "wasm32")]
mod guest {
    wit_bindgen::generate!({
        world: "norte-plugin",
        path: "wit",
        generate_all,
    });

    use exports::norte::plugin::command::Guest as CommandGuest;
    use exports::norte::plugin::previewer::{Guest as PreviewerGuest, PreviewInput, Span};

    struct Markdown;

    fn lines_of(input: &PreviewInput) -> Vec<Vec<Span>> {
        // The host decoded the file as text before handing it over; a byte
        // that still does not decode is the host's `lossy` flag, not ours.
        let text = String::from_utf8_lossy(&input.content);
        crate::render::render(&text)
            .into_iter()
            .map(|line| {
                line.into_iter()
                    .map(|s| Span {
                        text: s.text,
                        role: s.role.map(str::to_owned),
                        fg: s.fg,
                    })
                    .collect()
            })
            .collect()
    }

    impl PreviewerGuest for Markdown {
        fn render(input: PreviewInput) -> Result<String, String> {
            let lines = lines_of(&input);
            Ok(lines
                .iter()
                .map(|l| l.iter().map(|s| s.text.as_str()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n"))
        }

        fn render_styled(input: PreviewInput) -> Result<Vec<Vec<Span>>, String> {
            Ok(lines_of(&input))
        }
    }

    impl CommandGuest for Markdown {
        fn run(id: String, _arg: String) -> Result<String, String> {
            Err(format!("unknown command `{id}`: this plugin only previews"))
        }
    }

    export!(Markdown);
}
