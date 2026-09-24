//! Keyboard input, and why the renderer does not interpret it.
//!
//! A renderer sends KEYS —normalized, and little else—; who resolves a
//! count, a half-typed prefix or what command `ctrl+shift+f5` is, is Rust,
//! with the same resolver, the same presets and the same catalogue the TUI
//! uses (ADR 0066, decision D14). If the renderer resolved, there would be
//! two keymaps and the day they diverged nobody would notice until a user
//! reported it.
//!
//! # The adapter is thin on purpose
//!
//! The only thing here is the translation of the renderer's vocabulary
//! (`"ArrowDown"`, `"Escape"`, `meta`) to the project's (`down`, `esc`,
//! `mod`). The `mod` mapping on macOS is adapter input, NOT a forked
//! keymap: the chord that comes out of here is the same type the TUI pushes
//! to its resolver.

use norte_frontend::keymap::{Chord, KeymapError, parse_chord};
use serde::{Deserialize, Serialize};

/// A key exactly as the renderer sends it.
///
/// Four flags and not a modifier set: it is the shape in which a browser
/// —and any toolkit— delivers the event, and translating at the boundary is
/// cheaper than forcing every adapter to build one of our own types. The
/// chord that comes out of here is already the keymap's.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the shape of the input event, not state"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyInput {
    /// LOGICAL name of the key. Both the browser's (`"ArrowDown"`,
    /// `"Escape"`, `"F5"`, `"a"`) and the project's (`"down"`, `"esc"`) are
    /// accepted.
    pub key: String,
    /// Control.
    #[serde(default)]
    pub ctrl: bool,
    /// Alt / Option.
    #[serde(default)]
    pub alt: bool,
    /// Shift.
    #[serde(default)]
    pub shift: bool,
    /// Command (macOS) or Super. Travels as `mod`, which is what the keymap
    /// understands and what makes one preset work on both platforms.
    #[serde(default)]
    pub meta: bool,
}

impl KeyInput {
    /// Translates to a shared keymap [`Chord`].
    ///
    /// ```
    /// use norte_ui_host::KeyInput;
    ///
    /// let k = KeyInput {
    ///     key: "ArrowDown".to_owned(),
    ///     ctrl: false,
    ///     alt: false,
    ///     shift: false,
    ///     meta: false,
    /// };
    /// assert!(k.to_chord().is_ok());
    ///
    /// // A key that is not understood is discarded; it is not guessed.
    /// let odd = KeyInput { key: "Compose".to_owned(), ..k };
    /// assert!(odd.to_chord().is_err());
    /// ```
    ///
    /// # Errors
    /// [`KeymapError::BadChord`] if the key name is not recognized: a key
    /// that is not understood is DISCARDED, never guessed.
    pub fn to_chord(&self) -> Result<Chord, KeymapError> {
        let name = canonical_name(&self.key).ok_or_else(|| KeymapError::BadChord {
            chord: self.key.clone(),
        })?;
        let mut text = String::new();
        // The ORDER of the modifiers is the one the parser expects; the
        // renderer has no reason to know it.
        if self.ctrl {
            text.push_str("ctrl+");
        }
        if self.alt {
            text.push_str("alt+");
        }
        // `shift` is DISCARDED on a lone character, and it is the same rule
        // the chord grammar has written down: in a `Char` the shift is
        // already INSIDE the character —the browser sends `A`, not
        // `shift+a`— so naming it again is a chord `parse_chord` rejects
        // (`ShiftWithChar`).
        //
        // Without this, no uppercase letter and no `| > ~ ? : " _` ever got
        // anywhere: the chord did not build and the key died as
        // `host-key-unmapped`. In the terminal panel (#362) that meant
        // `ls | grep Foo` could not be typed; in the rest of the window,
        // that a preset binding `V` or `P` —as the repository's rule says
        // to write them— was dead.
        if self.shift && name.chars().count() != 1 {
            text.push_str("shift+");
        }
        if self.meta {
            text.push_str("mod+");
        }
        text.push_str(&name);
        parse_chord(&text)
    }
}

/// The project's key name for whatever the renderer sends.
///
/// It accepts both spellings —the browser's and ours— because each
/// renderer's adapter has no reason to normalize twice, and because a
/// renderer that already sends `"down"` should not be the odd case.
fn canonical_name(key: &str) -> Option<String> {
    let lower = key.to_ascii_lowercase();
    let canonical = match lower.as_str() {
        "arrowdown" | "down" => "down",
        "arrowup" | "up" => "up",
        "arrowleft" | "left" => "left",
        "arrowright" | "right" => "right",
        "escape" | "esc" => "esc",
        "enter" | "return" => "enter",
        "tab" => "tab",
        "backspace" => "backspace",
        "delete" | "del" => "delete",
        "insert" | "ins" => "insert",
        "home" => "home",
        "end" => "end",
        // `pgup`/`pgdn` and not `pageup`/`pagedown`: those are the names
        // `parse_chord` understands. With the long ones the key did not
        // resolve, and in this window `PageUp`/`PageDown` never reached the
        // keymap at all — it went unnoticed because the renderer scrolled
        // the body on its own, and in the help sidebar they simply did
        // nothing.
        "pageup" | "pgup" => "pgup",
        "pagedown" | "pgdn" => "pgdn",
        " " | "space" | "spacebar" => "space",
        other => {
            // Function keys and lone characters. A long name that is not in
            // the table is NOT interpreted as text: that would be the door
            // through which `"F13"` ends up as three characters.
            if let Some(n) = other.strip_prefix('f')
                && !n.is_empty()
                && n.chars().all(|c| c.is_ascii_digit())
            {
                return Some(other.to_owned());
            }
            if other.chars().count() == 1 {
                // With Shift, the renderer already sends the letter
                // uppercase (it is what the user sees); the keymap wants it
                // as is.
                return Some(key.to_owned());
            }
            return None;
        }
    };
    Some(canonical.to_owned())
}

/// The effective keymap of a factory preset, with the list of commands this
/// host implements.
///
/// The minimum to start without configuration —a test, a first launch—. A
/// real host also merges the user's layers and passes it the result in
/// [`crate::UiHostOptions`]: reading configuration is not this crate's
/// business.
///
/// # Errors
/// [`KeymapError`] if the preset does not exist or does not validate.
pub fn keymap_de_preset(name: &str) -> Result<norte_frontend::keymap::Effective, KeymapError> {
    keymap_de_preset_con(name, crate::commands::Effects::Full)
}

/// The listing's keymap for a frontend with the STATED effects.
///
/// In read-only mode, commands that write do not enter the list of known
/// ones, so a key bound to `pane.delete` resolves to
/// [`norte_frontend::keymap::Availability::NotHere`] and says so — which is
/// what a user needs to read, instead of a dead key.
///
/// # Errors
/// [`KeymapError`] if the preset does not exist or does not validate.
pub fn keymap_de_preset_con(
    name: &str,
    effects: crate::commands::Effects,
) -> Result<norte_frontend::keymap::Effective, KeymapError> {
    preset_keymap_with_layers(name, &[], effects)
}

/// The preset's listing keymap PLUS the user's layers.
///
/// This is the one a real frontend uses. The ones above build on the
/// factory preset alone —the minimum for a test or a first launch— and
/// using them in a binary leaves the user with the silent factory shortcuts
/// while the other frontend does honor its `keymap.toml` (#253).
///
/// # Errors
/// [`KeymapError`] if the preset does not exist, or if a layer does not
/// validate.
pub fn preset_keymap_with_layers(
    name: &str,
    layers: &[norte_frontend::keymap::KeymapFile],
    effects: crate::commands::Effects,
) -> Result<norte_frontend::keymap::Effective, KeymapError> {
    effective_with(
        name,
        norte_frontend::keymap::Screen::Browse,
        layers,
        effects,
    )
}

/// The effective keymap of the VIEWER screen, for the same preset.
///
/// It is ANOTHER screen, not another layer: with the viewer open the keys
/// are its own —`esc` closes, `e` changes the encoding— and mixing them
/// with the listing's would be an input context that exists in no preset.
///
/// # Errors
/// [`KeymapError`] if the preset does not exist or does not validate.
pub fn keymap_visor_de_preset(
    name: &str,
) -> Result<norte_frontend::keymap::Effective, KeymapError> {
    preset_viewer_keymap_with_layers(name, &[])
}

/// The VIEWER keymap of the preset plus the user's layers (#253).
///
/// # Errors
/// [`KeymapError`] if the preset does not exist, or if a layer does not
/// validate.
pub fn preset_viewer_keymap_with_layers(
    name: &str,
    layers: &[norte_frontend::keymap::KeymapFile],
) -> Result<norte_frontend::keymap::Effective, KeymapError> {
    effective_with(
        name,
        norte_frontend::keymap::Screen::Viewer,
        layers,
        crate::commands::Effects::Full,
    )
}

/// The effective keymap of a DIALOG, for the same preset.
///
/// Another screen, like the viewer: with a question in front the keys are
/// its own. It exists so that a preset rebinding `dialog.confirm` changes
/// both surfaces and not just the TUI — which is the drift the shared
/// catalogue exists to avoid (#287).
///
/// # Errors
/// [`KeymapError`] if the preset does not exist or does not validate.
pub fn preset_dialog_keymap(name: &str) -> Result<norte_frontend::keymap::Effective, KeymapError> {
    keymap_dialog_preset_with_layers(name, &[])
}

/// The keymap of a DIALOG for the preset plus the user's layers (#253).
///
/// # Errors
/// [`KeymapError`] if the preset does not exist, or if a layer does not
/// validate.
pub fn keymap_dialog_preset_with_layers(
    name: &str,
    layers: &[norte_frontend::keymap::KeymapFile],
) -> Result<norte_frontend::keymap::Effective, KeymapError> {
    let preset = preset_from(name)?;
    norte_frontend::keymap::Effective::build_for(
        &preset,
        layers,
        crate::commands::IMPLEMENTADOS_DIALOG,
        norte_frontend::keymap::Screen::Dialog,
    )
}

fn preset_from(name: &str) -> Result<norte_frontend::keymap::KeymapFile, KeymapError> {
    let source = norte_frontend::keymap::presets::source(name).ok_or(KeymapError::BadChord {
        chord: name.to_owned(),
    })?;
    norte_frontend::keymap::parse_keymap(source)
}

fn effective_with(
    name: &str,
    screen: norte_frontend::keymap::Screen,
    layers: &[norte_frontend::keymap::KeymapFile],
    effects: crate::commands::Effects,
) -> Result<norte_frontend::keymap::Effective, KeymapError> {
    let preset = preset_from(name)?;
    norte_frontend::keymap::Effective::build_for(
        &preset,
        layers,
        &crate::commands::all_with(effects),
        screen,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_frontend::keymap::{KeyCode, Mods};

    /// A user layer changes this window's EFFECTIVE keymap (#253).
    ///
    /// It was always built from the factory preset with an empty `&[]` of
    /// layers, so a `keymap.toml` with rebinds was silently ignored here
    /// while the terminal did honor it. The test compares the two builds:
    /// the factory one does NOT have the binding and the layered one DOES,
    /// which is the only thing that tells apart "the layer was read" from
    /// "the preset already had it".
    #[test]
    fn a_user_layer_changes_the_window_keymap() {
        let layer = norte_frontend::keymap::parse_keymap(
            "[pane]\nprepend_keymap = [{ on = [\"ctrl+alt+j\"], run = \"pane.refresh\" }]\n",
        )
        .expect("the layer parses");
        let bound = |e: &norte_frontend::keymap::Effective| {
            e.bindings()
                .into_iter()
                .any(|(seq, cmd)| seq == "ctrl+alt+j" && cmd == "pane.refresh")
        };

        let factory = keymap_de_preset("orthodox").expect("preset");
        assert!(
            !bound(&factory),
            "the factory preset does not bind `ctrl+alt+j`, or the test proves nothing"
        );

        let with_layer = preset_keymap_with_layers(
            "orthodox",
            std::slice::from_ref(&layer),
            crate::commands::Effects::Full,
        )
        .expect("preset + layer");
        assert!(
            bound(&with_layer),
            "the user's layer must reach the effective keymap: {:?}",
            with_layer.bindings()
        );
    }

    #[test]
    fn the_browser_vocabulary_is_translated() {
        let k = KeyInput {
            key: "ArrowDown".to_owned(),
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
        };
        assert_eq!(
            k.to_chord().expect("chord"),
            Chord::new(Mods::default(), KeyCode::Down)
        );
    }

    /// The browser's `PageUp`/`PageDown` are keymap keys. They were
    /// translated to `pageup`/`pagedown`, which `parse_chord` does not
    /// understand, so in the window they never resolved.
    #[test]
    fn page_keys_resolve() {
        for (dom, expected) in [("PageUp", KeyCode::PageUp), ("PageDown", KeyCode::PageDown)] {
            let k = KeyInput {
                key: dom.to_owned(),
                ctrl: false,
                alt: false,
                shift: false,
                meta: false,
            };
            assert_eq!(
                k.to_chord().expect(dom),
                Chord::new(Mods::default(), expected),
                "{dom}"
            );
        }
    }

    #[test]
    fn modifiers_go_in_the_parsers_order() {
        let k = KeyInput {
            key: "F5".to_owned(),
            ctrl: true,
            alt: false,
            shift: true,
            meta: false,
        };
        let c = k.to_chord().expect("chord");
        assert_eq!(c, parse_chord("ctrl+shift+f5").expect("parse"));
    }

    /// `meta` travels as `mod`: it is the adapter that knows about macOS,
    /// not the keymap.
    #[test]
    fn meta_travels_as_mod() {
        let k = KeyInput {
            key: "p".to_owned(),
            ctrl: false,
            alt: false,
            shift: false,
            meta: true,
        };
        assert_eq!(
            k.to_chord().expect("chord"),
            parse_chord("mod+p").expect("parse")
        );
    }

    /// A key that is not recognized is discarded; it is not guessed.
    #[test]
    fn an_unknown_key_is_not_invented() {
        let k = KeyInput {
            key: "Compose".to_owned(),
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
        };
        assert!(k.to_chord().is_err());
    }
}
