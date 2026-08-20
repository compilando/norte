//! Genera el contexto de Tauri (config, capacidades, iconos) en tiempo de
//! compilación. Es lo único que este crate ejecuta en `build.rs`.

fn main() {
    tauri_build::build();
}
