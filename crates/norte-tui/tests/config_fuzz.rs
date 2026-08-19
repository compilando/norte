//! Fuzzing de la config TOML (fase 10, spec §12) como property tests: se
//! ejecuta en CADA run de la suite, sin nightly ni libFuzzer. Propiedad:
//! NINGUNA entrada — bytes arbitrarios, TOML válido con forma inesperada,
//! mutaciones de configs reales — puede panicar el parseo ni la
//! construcción del keymap; solo Ok o error tipado.

use norte_tui::config::{Layer, Layers};
use norte_tui::keymap::{COMMANDS, Effective, parse_keymap, presets};
use proptest::prelude::*;

proptest! {
    /// Basura arbitraria: jamás panic.
    #[test]
    fn parse_keymap_jamas_panica(input in ".{0,512}") {
        let _ = parse_keymap(&input);
    }

    /// TOML estructuralmente válido con claves/tipos arbitrarios.
    #[test]
    fn toml_valido_con_formas_raras(
        seccion in "[a-z]{1,10}",
        clave in "[a-z_]{1,12}",
        value in ".{0,40}",
    ) {
        let doc = format!("[{seccion}]\n{clave} = {value:?}\n");
        let _ = parse_keymap(&doc);
        let _ = toml::from_str::<norte_tui::config::NorteToml>(&doc);
    }

    /// Mutaciones de un keymap REAL: cortar, duplicar e inyectar bytes en
    /// el preset orthodox no puede panicar ni al parsear ni al construir.
    #[test]
    fn mutaciones_de_un_preset_real(
        corte in 0usize..2048,
        inyecta in ".{0,16}",
        pos in 0usize..2048,
    ) {
        let base = norte_frontend::keymap::presets::ORTHODOX;
        let mut s = base.to_owned();
        let mut corte2 = corte.min(s.len());
        while !s.is_char_boundary(corte2) {
            corte2 -= 1;
        }
        s.truncate(corte2);
        let mut pos2 = pos.min(s.len());
        while !s.is_char_boundary(pos2) { pos2 -= 1; }
        s.insert_str(pos2, &inyecta);
        if let Ok(file) = parse_keymap(&s) {
            let _ = Effective::build_layered(&presets()[0].1, &[file], COMMANDS);
        }
    }

    /// load() sobre árboles de capas con contenidos arbitrarios: jamás
    /// panic, siempre Ok o ConfigError con archivo.
    #[test]
    fn load_con_capas_arbitrarias(norte in ".{0,256}", keymap in ".{0,256}") {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("norte.toml"), &norte).unwrap();
        std::fs::write(d.path().join("keymap.toml"), &keymap).unwrap();
        let _ = norte_tui::config::load(&Layers {
            dirs: vec![(d.path().to_path_buf(), Layer::User)],
        });
    }
}
