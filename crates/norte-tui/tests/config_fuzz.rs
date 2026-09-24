//! Fuzzing of the TOML config (phase 10, spec §12) as property tests: runs
//! on EVERY run of the suite, no nightly, no libFuzzer. Property: NO
//! input — arbitrary bytes, valid TOML with an unexpected shape, mutations
//! of real configs — can panic the parsing nor the keymap construction;
//! only Ok or a typed error.

use norte_tui::config::{Layer, Layers};
use norte_tui::keymap::{COMMANDS, Effective, parse_keymap, presets};
use proptest::prelude::*;

proptest! {
    /// Arbitrary garbage: never panics.
    #[test]
    fn parse_keymap_never_panics(input in ".{0,512}") {
        let _ = parse_keymap(&input);
    }

    /// Structurally valid TOML with arbitrary keys/types.
    #[test]
    fn valid_toml_with_odd_shapes(
        section in "[a-z]{1,10}",
        key in "[a-z_]{1,12}",
        value in ".{0,40}",
    ) {
        let doc = format!("[{section}]\n{key} = {value:?}\n");
        let _ = parse_keymap(&doc);
        let _ = toml::from_str::<norte_tui::config::NorteToml>(&doc);
    }

    /// Mutations of a REAL keymap: cutting, duplicating and injecting bytes
    /// into the orthodox preset cannot panic either while parsing or while
    /// building.
    #[test]
    fn mutations_of_a_real_preset(
        cut in 0usize..2048,
        inject in ".{0,16}",
        pos in 0usize..2048,
    ) {
        let base = norte_frontend::keymap::presets::ORTHODOX;
        let mut s = base.to_owned();
        let mut cut2 = cut.min(s.len());
        while !s.is_char_boundary(cut2) {
            cut2 -= 1;
        }
        s.truncate(cut2);
        let mut pos2 = pos.min(s.len());
        while !s.is_char_boundary(pos2) { pos2 -= 1; }
        s.insert_str(pos2, &inject);
        if let Ok(file) = parse_keymap(&s) {
            let _ = Effective::build_layered(&presets()[0].1, &[file], COMMANDS);
        }
    }

    /// load() over layer trees with arbitrary contents: never panics,
    /// always Ok or a ConfigError with the file.
    #[test]
    fn load_with_arbitrary_layers(norte in ".{0,256}", keymap in ".{0,256}") {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("norte.toml"), &norte).unwrap();
        std::fs::write(d.path().join("keymap.toml"), &keymap).unwrap();
        let _ = norte_tui::config::load(&Layers {
            dirs: vec![(d.path().to_path_buf(), Layer::User)],
        });
    }
}
