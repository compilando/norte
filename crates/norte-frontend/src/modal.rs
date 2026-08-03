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

/// Parejas del plan de rename IA (M4-IA) visibles a la vez en el modal del
/// plan (ventana de scroll, audit MAJOR-3: el plan ENTERO es revisable por
/// scroll — sin ventana, la cola de un plan largo se aplicaría sin poder
/// verse). Única fuente para el render, el alto del modal (TUI) y el clamp
/// del scroll en ambos frontends.
pub const AI_RENAME_PAIR_LIMIT: usize = 5;

/// Hits de la búsqueda semántica (M4-IA-2) visibles a la vez en el modal de
/// hits (ventana de scroll con cursor, molde [`AI_RENAME_PAIR_LIMIT`]).
/// Única fuente para el render, el alto del modal (TUI) y el clamp del
/// cursor en ambos frontends.
pub const SEMANTIC_HIT_LIMIT: usize = 10;

/// Tope de parejas que un frontend ACEPTA de `ai.rename_plan` (M4-IA,
/// cinturón de ingestión): el engine acota los planes legítimos MUY por
/// debajo (los basenames de UN directorio), así que un plan que lo supere
/// delata un daemon hostil/N+1 inflando la respuesta — se rechaza EN BLOQUE
/// (mismo mensaje que un plan adulterado), jamás se trocea ni se revisa "lo
/// que quepa".
pub const MAX_AI_PLAN_ENTRIES: usize = 256;

/// Valida TODAS las parejas del plan como [`Segment`] (cinturón fail-loud,
/// audit MAJOR-2, compartido por TUI y GUI): un plan bien formado del engine
/// JAMÁS trae un segmento inválido (el daemon los validó al armarlo), así
/// que UN rechazo aquí delata un daemon hostil/roto — `None` aborta el lote
/// ENTERO, jamás un skip silencioso que aplique «lo demás» de un plan
/// adulterado.
///
/// PURA a propósito: testeable sin backend (audit MINOR-6e).
///
/// ```
/// use norte_proto::methods::AiRenameEntry;
/// let ok = AiRenameEntry { from: "a.txt".into(), to: "b.txt".into() };
/// assert!(norte_frontend::validate_ai_plan(std::slice::from_ref(&ok)).is_some());
/// let evil = AiRenameEntry { from: "c.txt".into(), to: "../evil".into() };
/// // UNA pareja inválida tumba el plan ENTERO, aunque el resto sea legítimo.
/// assert!(norte_frontend::validate_ai_plan(&[ok, evil]).is_none());
/// ```
#[must_use]
pub fn validate_ai_plan(
    entries: &[norte_proto::methods::AiRenameEntry],
) -> Option<Vec<(Segment, Segment)>> {
    entries
        .iter()
        .map(|e| {
            Some((
                Segment::new(e.from.as_bytes().to_vec()).ok()?,
                Segment::new(e.to.as_bytes().to_vec()).ok()?,
            ))
        })
        .collect()
}

/// Cinturón de INGESTIÓN de los hits semánticos (M4-IA-2, paridad con el
/// belt del plan IA, compartido por TUI y GUI): un daemon CONFORME jamás
/// supera [`norte_proto::methods::INDEX_SEMANTIC_MAX_K`] (el server recorta
/// `k` a ese techo contractual) ni emite scores no finitos (el engine los
/// filtra) — superar el techo o colar un NaN/∞ delata un daemon hostil/N+1
/// inflando o envenenando la respuesta. `None` = rechazo EN BLOQUE (cero
/// hits pintados, jamás un recorte silencioso); `Some` devuelve los hits
/// intactos.
///
/// PURA a propósito: testeable sin backend, como [`validate_ai_plan`].
///
/// ```
/// use norte_proto::VPath;
/// use norte_proto::methods::SemanticHit;
/// let ok = SemanticHit { path: VPath::parse("mem:///a").unwrap(), score: 0.9 };
/// assert!(norte_frontend::validate_semantic_hits(vec![ok.clone()]).is_some());
/// // UN score no finito tumba la respuesta ENTERA, aunque el resto sea legítimo.
/// let evil = SemanticHit { path: VPath::parse("mem:///b").unwrap(), score: f64::NAN };
/// assert!(norte_frontend::validate_semantic_hits(vec![ok, evil]).is_none());
/// ```
#[must_use]
pub fn validate_semantic_hits(
    hits: Vec<norte_proto::methods::SemanticHit>,
) -> Option<Vec<norte_proto::methods::SemanticHit>> {
    (hits.len() <= norte_proto::methods::INDEX_SEMANTIC_MAX_K as usize
        && hits.iter().all(|h| h.score.is_finite()))
    .then_some(hits)
}

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
mod ai_plan_tests {
    use super::validate_ai_plan;
    use norte_proto::methods::AiRenameEntry;

    fn e(from: &str, to: &str) -> AiRenameEntry {
        AiRenameEntry {
            from: from.into(),
            to: to.into(),
        }
    }

    /// Audit MAJOR-2 (fail-loud): UNA pareja inválida — traversal `..`,
    /// separador embebido o nombre vacío — tumba el plan ENTERO (`None`),
    /// jamás un skip silencioso que aplique "lo demás" de un plan
    /// adulterado por un daemon hostil/roto.
    #[test]
    fn una_pareja_invalida_tumba_el_plan_entero() {
        assert!(validate_ai_plan(&[e("a", "b"), e("c", "..")]).is_none());
        assert!(validate_ai_plan(&[e("a/b", "c"), e("d", "e")]).is_none());
        assert!(validate_ai_plan(&[e("", "x")]).is_none());
        assert!(validate_ai_plan(&[e("ok", "tambien-ok"), e("x", "a/b")]).is_none());
    }

    /// Un plan bien formado conserva orden y longitud, bytes exactos.
    #[test]
    fn un_plan_valido_conserva_orden_y_longitud() {
        let pairs = validate_ai_plan(&[e("a", "b"), e("c", "d")]).expect("plan válido");
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0.as_bytes(), b"a");
        assert_eq!(pairs[0].1.as_bytes(), b"b");
        assert_eq!(pairs[1].0.as_bytes(), b"c");
        assert_eq!(pairs[1].1.as_bytes(), b"d");
    }

    /// El plan vacío es válido (los frontends no encolan nada con él).
    #[test]
    fn un_plan_vacio_es_valido() {
        assert_eq!(validate_ai_plan(&[]).expect("vacío válido").len(), 0);
    }
}

#[cfg(test)]
mod semantic_hits_tests {
    use super::validate_semantic_hits;
    use norte_proto::VPath;
    use norte_proto::methods::{INDEX_SEMANTIC_MAX_K, SemanticHit};

    fn hits(n: usize) -> Vec<SemanticHit> {
        (1..=n)
            .map(|i| SemanticHit {
                path: VPath::parse(&format!("mem:///d/f{i}")).expect("wire válido"),
                score: 0.5,
            })
            .collect()
    }

    /// M4-IA-2 (paridad IA-1 con el belt del plan): el cinturón acepta hasta
    /// el techo contractual del server (`INDEX_SEMANTIC_MAX_K` — un daemon
    /// conforme jamás lo supera) con los hits INTACTOS, y rechaza EN BLOQUE
    /// una respuesta inflada (daemon hostil/N+1) — jamás un recorte
    /// silencioso.
    #[test]
    fn el_techo_exacto_pasa_y_uno_mas_se_rechaza_en_bloque() {
        let max = usize::try_from(INDEX_SEMANTIC_MAX_K).expect("techo pequeño");
        let ok = validate_semantic_hits(hits(max));
        assert_eq!(
            ok.as_ref().map(Vec::len),
            Some(max),
            "el techo exacto pasa intacto"
        );
        assert!(
            validate_semantic_hits(hits(max + 1)).is_none(),
            "uno más = rechazo en bloque"
        );
    }

    /// UN score no finito (NaN/∞ — el engine los filtra, así que solo un
    /// daemon hostil/roto los emite) tumba la respuesta ENTERA, aunque el
    /// resto sea legítimo.
    #[test]
    fn un_score_no_finito_tumba_la_respuesta_entera() {
        for evil in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut lote = hits(3);
            lote[1].score = evil;
            assert!(
                validate_semantic_hits(lote).is_none(),
                "score {evil} debe rechazar en bloque"
            );
        }
    }
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
