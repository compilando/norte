//! Generates the Tauri context (config, capabilities, icons) at build time.
//! It is the only thing this crate runs in `build.rs`.

fn main() {
    tauri_build::build();
}
