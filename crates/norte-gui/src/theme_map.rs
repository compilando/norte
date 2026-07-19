//! Puente `norte-theme` → color de GPUI. Determinista, testeable sin GPU;
//! sobrevive al MVP (M5 hito 2). Truecolor SIEMPRE (la GUI es la capa GPU
//! que MT reservó): jamás degrada a 256/16.
//!
//! Tipo GPUI elegido: `gpui::Rgba { r, g, b, a: f32 }` (rango `[0.0, 1.0]`) —
//! es el tipo que consumen directamente `.bg(...)` y `.text_color(...)` en el
//! render de T2/main.rs (vía el helper `gpui::rgb(u32)`, que también devuelve
//! `Rgba`), así que es el tipo que T4/T5 van a necesitar para colorear texto.
//! GPUI convierte `Rgba` a `Hsla` internamente cuando lo necesita (`impl
//! From<Hsla> for Rgba` existe en `gpui::color`, y viceversa) — no hace falta
//! pasar por `Hsla` aquí.

use norte_theme::{Color, ColorDepth, ResolvedColor};

/// `norte_theme::Color` → `gpui::Rgba` opaco (alpha 1.0).
///
/// Truecolor siempre resuelve a [`ResolvedColor::Rgb`]; la rama `Indexed` es
/// inalcanzable con [`ColorDepth::Truecolor`] pero se cubre con un negro
/// opaco por exhaustividad (nunca `unwrap`/`panic`, regla 6).
#[must_use]
pub fn to_gpui_rgba(c: Color) -> gpui::Rgba {
    match c.resolve(ColorDepth::Truecolor) {
        ResolvedColor::Rgb(r, g, b) => gpui::Rgba {
            r: f32::from(r) / 255.0,
            g: f32::from(g) / 255.0,
            b: f32::from(b) / 255.0,
            a: 1.0,
        },
        // Inalcanzable: Truecolor jamás degrada a índice de paleta.
        ResolvedColor::Indexed(_) => gpui::Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        },
    }
}

#[cfg(test)]
mod tests {
    use norte_theme::Color;

    #[test]
    fn color_a_rgba_gpui_conserva_los_bytes() {
        // Color::parse de un hex conocido → resolve Truecolor → rgba gpui.
        let c = Color::parse("#3b82f6").expect("hex válido");
        let g = super::to_gpui_rgba(c);
        // gpui::Rgba tiene r/g/b/a en f32 [0,1]; 0x3b=59, 0x82=130, 0xf6=246.
        assert!((g.r - 59.0 / 255.0).abs() < 1e-4, "r");
        assert!((g.g - 130.0 / 255.0).abs() < 1e-4, "g");
        assert!((g.b - 246.0 / 255.0).abs() < 1e-4, "b");
        assert!((g.a - 1.0).abs() < 1e-4, "alpha opaco");
    }
}
