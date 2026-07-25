//! Política de LISTA de un modal de confirmación: cuántos ítems se pintan
//! antes de resumir el resto, y cómo se sanea cada uno.
//!
//! Vivía duplicada en la GUI (#103 T10 la sube aquí): un lote de copia,
//! movimiento o borrado se confirma sobre una LISTA de nombres, y esa lista
//! es superficie de ataque — un nombre hostil que cuele un `\n`, un override
//! bidi o un separador podría FABRICAR una entrada falsa y hacer que el
//! humano apruebe algo que no leyó. La regla es una sola para ambos
//! frontends: una ruta POR LÍNEA, siempre por [`display_name_with`]
//! (`crate::display_name_with`), y el badge del flag hostil.

use norte_proto::{Segment, VPath};

/// Cuántos ítems lista un modal antes de resumir el resto en «… y N más».
///
/// Es tope de LEGIBILIDAD, no de seguridad: el resumen final jamás calla
/// cuántos quedan fuera (un lote de 500 no puede parecer uno de 10).
pub const MODAL_ITEM_LIMIT: usize = 10;

/// Badge por defecto de [`item_lines`]: el aviso que ya usaba el modal de la
/// GUI. Los frontends con un badge propio (el TUI usa `!`, ASCII, por los
/// terminales que no pintan `⚠`) pasan el suyo a [`item_lines_with`] — el
/// crate no elige badge, solo garantiza que el flag hostil se MARCA.
const DEFAULT_BADGE: &str = "⚠";

/// Hasta [`MODAL_ITEM_LIMIT`] nombres saneados (una línea por ítem, jamás
/// dos rutas en la misma); si sobran, una línea final localizada con cuántos
/// quedan fuera.
///
/// Pinta el NOMBRE de cada ítem, no la ruta entera: en un lote todos
/// comparten directorio (el del pane) y el destino va en su propia línea.
///
/// ```
/// use norte_proto::VPath;
/// let items: Vec<VPath> = (0..12)
///     .map(|i| VPath::parse(&format!("mem:///f{i}")).unwrap())
///     .collect();
/// let lines = norte_frontend::item_lines(&items);
/// assert_eq!(lines.len(), norte_frontend::MODAL_ITEM_LIMIT + 1);
/// assert_eq!(lines[0], "f0");
/// // La última RESUME los que no caben: 12 - 10 = 2.
/// assert!(lines.last().unwrap().contains('2'));
/// ```
#[must_use]
pub fn item_lines(items: &[VPath]) -> Vec<String> {
    item_lines_with(items, DEFAULT_BADGE, None)
}

/// [`item_lines`] con el badge del frontend y la REINTERPRETACIÓN de nombres
/// del pane origen (#57): el diálogo debe pintar el MISMO texto por el que el
/// usuario navegó — con un pane en cp866, confirmar un borrado mostrando el
/// lossy `�����` en vez de `Папка` sería preguntar por otra cosa.
///
/// ```
/// use norte_encoding::NameEncoding;
/// use norte_proto::VPath;
/// let items = vec![VPath::parse("mem:///CAF%90.TXT").unwrap()];
/// let lines = norte_frontend::item_lines_with(&items, "!", Some(NameEncoding::Cp437));
/// // Reinterpretado Y marcado: el texto pintado no son los bytes.
/// assert_eq!(lines, vec!["! CAFÉ.TXT".to_string()]);
/// ```
#[must_use]
pub fn item_lines_with(
    items: &[VPath],
    badge: &str,
    reinterpret: Option<norte_encoding::NameEncoding>,
) -> Vec<String> {
    let mut lines: Vec<String> = items
        .iter()
        .take(MODAL_ITEM_LIMIT)
        .map(|p| {
            let bytes = p.file_name().map_or(&b""[..], Segment::as_bytes);
            let (name, hostile) = crate::display_name_with(bytes, reinterpret);
            if hostile {
                format!("{badge} {name}")
            } else {
                name
            }
        })
        .collect();
    if items.len() > MODAL_ITEM_LIMIT {
        let n = (items.len() - MODAL_ITEM_LIMIT).to_string();
        // Clave heredada de la GUI (GUI-e T1): la comparte ahora el TUI —
        // renombrarla no cambiaría el texto y rompería las traducciones.
        lines.push(norte_i18n::ta("gui-modal-more", &[("n", n.as_str())]));
    }
    lines
}

#[cfg(test)]
mod tests {
    use norte_proto::VPath;

    /// La lista de un modal se corta en [`super::MODAL_ITEM_LIMIT`] y RESUME
    /// el resto en una última línea localizada — jamás pinta 500 rutas ni,
    /// peor, calla las que no caben.
    #[test]
    fn the_modal_lists_the_first_items_and_summarises_the_rest() {
        let many: Vec<VPath> = (0..20)
            .map(|i| VPath::parse(&format!("mem:///f{i}")).unwrap())
            .collect();
        let lines = crate::item_lines(&many);
        assert_eq!(lines.len(), crate::MODAL_ITEM_LIMIT + 1);
        assert!(
            lines
                .last()
                .unwrap()
                .contains(&(20 - crate::MODAL_ITEM_LIMIT).to_string())
        );
    }

    /// Un lote que cabe entero NO lleva línea de resumen (ni un «y 0 más»).
    #[test]
    fn a_batch_that_fits_has_no_summary_line() {
        let few: Vec<VPath> = (0..crate::MODAL_ITEM_LIMIT)
            .map(|i| VPath::parse(&format!("mem:///f{i}")).unwrap())
            .collect();
        assert_eq!(crate::item_lines(&few).len(), crate::MODAL_ITEM_LIMIT);
    }

    /// El corpus hostil entero: ni una línea deja un hazard crudo, ni una
    /// línea contiene un salto (una ruta por línea, siempre) — la lista no
    /// se puede FABRICAR desde un nombre.
    #[test]
    fn item_lines_never_leak_raw_hazards_nor_forge_a_line() {
        for fixture in norte_testkit::corpus::hostile_names() {
            let Ok(seg) = norte_proto::Segment::new(fixture.bytes.clone()) else {
                continue; // un nombre no representable como segmento no llega aquí
            };
            let p = VPath::root(norte_proto::Scheme::new("mem").unwrap(), None).join(seg);
            let lines = crate::item_lines(std::slice::from_ref(&p));
            assert_eq!(lines.len(), 1, "{}: una línea por ítem", fixture.id);
            let line = &lines[0];
            assert!(
                !line.chars().any(norte_encoding::is_terminal_hazard),
                "{}: hazard crudo en {line:?}",
                fixture.id,
            );
            assert!(
                !line.contains('\n'),
                "{}: un nombre no puede fabricar una línea: {line:?}",
                fixture.id,
            );
        }
    }
}
