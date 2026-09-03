//! The smallest norte plugin: a previewer and a command.
//!
//! The host calls into this component through the `norte-plugin` world. It
//! never calls the host on its own except through the two doors the world
//! imports: `host-log` (a line in norte's log) and `host-config` (the
//! settings declared in `plugin.toml`, already validated by the host).

// Bindings for the world, generated from the WIT the host serves. `path` is
// the `wit` directory next to `Cargo.toml`; `generate_all` also generates the
// interfaces imported from other packages (`norte:host`).
wit_bindgen::generate!({
    world: "norte-plugin",
    path: "wit",
    generate_all,
});

use exports::norte::plugin::command::Guest as CommandGuest;
use exports::norte::plugin::previewer::{Guest as PreviewerGuest, PreviewInput, Span};
use norte::host::{host_config, host_log};

struct Template;

impl PreviewerGuest for Template {
    /// A plain-text preview: what the host said the file is, how many bytes
    /// it handed over, and the first line.
    fn render(input: PreviewInput) -> Result<String, String> {
        host_log::log(&format!("template: {} bytes", input.content.len()));
        let text = String::from_utf8_lossy(&input.content);
        let first = text.lines().next().unwrap_or_default();
        Ok(format!(
            "{}, {} bytes\n{first}",
            input.mimetype,
            input.content.len()
        ))
    }

    /// The styled twin: the same lines, one plain span each. A span may carry
    /// a theme `role` (`title`, `match`...) or an `fg` colour; here neither.
    fn render_styled(input: PreviewInput) -> Result<Vec<Vec<Span>>, String> {
        let plain = Self::render(input)?;
        Ok(plain
            .lines()
            .map(|line| {
                vec![Span {
                    text: line.to_string(),
                    role: None,
                    fg: None,
                }]
            })
            .collect())
    }
}

impl CommandGuest for Template {
    /// `hello <arg>` greets with the `greeting` setting; any other id is an
    /// error the host shows in the status bar.
    fn run(id: String, arg: String) -> Result<String, String> {
        match id.as_str() {
            "hello" => {
                let greeting = host_config::get("greeting").unwrap_or_else(|| "hello".to_string());
                Ok(format!("{greeting}, {arg}"))
            }
            other => Err(format!("unknown command `{other}`")),
        }
    }
}

export!(Template);
