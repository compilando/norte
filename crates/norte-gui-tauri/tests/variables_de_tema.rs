//! Cada `var(--x)` de color de la hoja de estilos la alimenta alguien, y cada
//! color que el host proyecta lo gasta alguien.
//!
//! Una variable que nadie escribe no se ve rota: cae a su valor de respaldo y
//! se queda ahí para siempre. `--warn-fg` era exactamente eso —una errata de
//! `warning-fg`, que es lo que `roles_de_tema` proyecta de verdad— y los
//! avisos del panel de registro llevaban desde que se escribió ignorando el
//! tema y pintándose de un `#fc6` cosido al CSS.
//!
//! Vive en el RENDERER y no en quien hospeda a propósito: `style.css` es de la
//! webview, y ADR 0066 prohíbe que `norte-ui-host` conozca un toolkit de
//! pintado. Que el renderer contraste su propia hoja contra lo que el host le
//! proyecta es la dirección correcta del conocimiento.

use std::collections::BTreeSet;

/// Variables de GEOMETRÍA y de fuente: nunca salen del tema, y por eso no
/// tienen que estar en `roles_de_tema`.
const NO_SON_COLOR: &[&str] = &[
    "cell-w",
    "cell-h",
    "menubar-h",
    "panelbar-h",
    "keybar-h",
    "depth",
    "busy-delay",
    "menu-left",
    "menu-open",
    "mono",
    "ui-font",
    "ui-font-size",
    "font-mono",
    "font-ui",
    "dialog-backdrop",
];

/// Huérfanas CONOCIDAS, con dueño y fecha: las alimenta la tarea 4 del plan
/// `2026-09-11-vscode-theme.md` (los roles `muted` y `badge`). Esta lista se
/// VACÍA allí, y vaciarla es lo que impide que se olviden. Una lista de
/// excepciones sin dueño no es un plan, es una fuga.
const HUERFANAS_CONOCIDAS: &[&str] = &["dim-fg", "chip-bg"];

/// Los nombres de `var(--…)` que aparecen en la hoja.
///
/// Se busca sobre el texto ENTERO y no línea a línea porque la hoja parte las
/// pilas de fuentes: `var(` queda en una línea y `--ui-font` en la siguiente.
fn variables_de_la_hoja() -> BTreeSet<String> {
    let css = include_str!("../ui/src/style.css");
    let mut out = BTreeSet::new();
    let mut resto = css;
    while let Some(i) = resto.find("var(") {
        resto = &resto[i + "var(".len()..];
        let t = resto.trim_start();
        if let Some(nombre) = t.strip_prefix("--") {
            let fin = nombre
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .unwrap_or(nombre.len());
            if fin > 0 {
                out.insert(nombre[..fin].to_owned());
            }
        }
    }
    out
}

/// El conjunto de claves POSIBLE: las que `roles_de_tema` produce para un
/// tema que da color a todos sus roles.
fn proyectadas() -> BTreeSet<String> {
    let tema = norte_theme::Theme::preset_default();
    norte_ui_host::pickers::roles_de_tema(&tema)
        .into_iter()
        .map(|(k, _)| k)
        .collect()
}

#[test]
fn cada_variable_de_color_la_alimenta_el_tema() {
    let proyectadas = proyectadas();
    let huerfanas: Vec<String> = variables_de_la_hoja()
        .into_iter()
        .filter(|v| !proyectadas.contains(v))
        .filter(|v| !NO_SON_COLOR.contains(&v.as_str()))
        .filter(|v| !HUERFANAS_CONOCIDAS.contains(&v.as_str()))
        .collect();
    assert!(
        huerfanas.is_empty(),
        "variables de color que nadie alimenta: {huerfanas:?}"
    );
}

#[test]
fn cada_color_proyectado_lo_gasta_la_hoja() {
    let usadas = variables_de_la_hoja();
    let sin_gastar: Vec<String> = proyectadas()
        .into_iter()
        .filter(|k| !usadas.contains(k))
        .collect();
    assert!(
        sin_gastar.is_empty(),
        "colores que el host proyecta y la hoja no pinta: {sin_gastar:?}"
    );
}
