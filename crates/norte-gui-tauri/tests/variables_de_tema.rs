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
//!
//! **Este test NO lo corre `just ci` ni `just ci-fast`**: `core_pkgs` excluye
//! `norte-gui-tauri` del gate portable porque compilarlo exige `WebKitGTK`. Lo
//! corren `just gui-test` (el bucle) y `just gui-ci` (su gate, que es el que
//! ejecuta `.github/workflows/gui.yml`). Se dice aquí porque el valor de este
//! test es cazar la errata del siguiente, y el siguiente correrá `just ci`.

use std::collections::BTreeSet;

/// Variables de GEOMETRÍA y de fuente: nunca salen del tema, y por eso no
/// tienen que estar en `roles_de_tema`.
const NO_SON_COLOR: &[&str] = &[
    "cell-w",
    "cell-h",
    "menubar-h",
    "panelbar-h",
    "activity-w",
    "activity-size",
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
    // Una OPACIDAD, no un color: cuánto se apaga el contenido del panel que
    // no tiene el teclado (ADR 0115). Pedirle un rol al tema sería pedirle
    // que eligiera un color para algo que no pinta ninguno.
    "inactive-dim",
    // Y un ANCHO: lo que lleva hecho la task de esa fila, en tanto por
    // ciento. Lo pone el renderer fila a fila, no el tema.
    "pct",
    // Dónde empieza una zona pulsable de un panel de plugin y cuánto ocupa, en
    // CELDAS (fase 3). Las pone el renderer zona a zona, de lo que dijo el
    // guest: el marco es texto, y una zona es una región de ese texto. Pedirle
    // un rol al tema sería pedirle un color para una coordenada.
    "hit-col",
    "hit-width",
    // Un FACTOR de escala: el zoom de una imagen, el porcentaje que el host
    // lleva ya dividido entre cien (puente 80). `1` es ajustada. Pedirle un
    // rol al tema sería pedirle un color para un multiplicador.
    "zoom",
    // Una IMAGEN: la regla de marcas ya compuesta (ADR 0135), un degradado
    // con una banda por racha de tramos marcados. Su color sale de
    // `--mark-bg` y `--fg`, que sí son del tema.
    "mark-ruler",
];

/// Huérfanas CONOCIDAS, con dueño y fecha. Vacía desde que los roles `muted`
/// y `badge` alimentan lo que eran `--dim-fg` y `--chip-bg`. Se queda como
/// constante —y no se borra— porque el mecanismo tiene que existir para la
/// siguiente: una lista de excepciones con dueño es un plan, una sin dueño es
/// una fuga, y no tener lista obliga a elegir entre las dos cosas peores
/// (apagar el test, o dejar la variable sin escribir).
const HUERFANAS_CONOCIDAS: &[&str] = &[];

/// Nombres que el acuerdo tiene y la hoja aún no GASTA. **Vacía**: desde la
/// tarea 7 del plan `2026-09-11-vscode-theme.md` no queda ninguno, y las dos
/// direcciones del guardián están vivas sin más excepción que la geometría.
/// Se queda por el mismo motivo que `HUERFANAS_CONOCIDAS`: el mecanismo tiene
/// que existir para el siguiente que llegue con dueño y fecha.
const PENDIENTES_DE_GASTAR: &[&str] = &[];

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

/// El ACUERDO: los nombres de variable que el host conoce, exista o no el
/// color en un tema concreto.
///
/// Es `nombres_de_tema` y no `roles_de_tema(preset_default())` por un motivo
/// que costó un test mal escrito: los diez roles de cromo no están en
/// `Role::CORE`, así que el preset por defecto los CALLA y la hoja los deriva
/// — preguntarle a un tema concreto habría leído ese silencio como «nadie
/// alimenta esa variable» y habría declarado huérfanas las nueve que la spec
/// acaba de añadir.
fn acordadas() -> BTreeSet<String> {
    norte_ui_host::pickers::nombres_de_tema()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[test]
fn cada_variable_de_color_la_alimenta_el_tema() {
    let acordadas = acordadas();
    let huerfanas: Vec<String> = variables_de_la_hoja()
        .into_iter()
        .filter(|v| !acordadas.contains(v))
        .filter(|v| !NO_SON_COLOR.contains(&v.as_str()))
        .filter(|v| !HUERFANAS_CONOCIDAS.contains(&v.as_str()))
        .collect();
    assert!(
        huerfanas.is_empty(),
        "variables de color que nadie alimenta: {huerfanas:?}"
    );
}

#[test]
fn cada_color_acordado_lo_gasta_la_hoja() {
    let usadas = variables_de_la_hoja();
    let sin_gastar: Vec<String> = acordadas()
        .into_iter()
        .filter(|k| !usadas.contains(k))
        .filter(|k| !PENDIENTES_DE_GASTAR.contains(&k.as_str()))
        .collect();
    assert!(
        sin_gastar.is_empty(),
        "colores que el host proyecta y la hoja no pinta: {sin_gastar:?}"
    );
}
