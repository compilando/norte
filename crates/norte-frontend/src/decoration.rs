//! Decoraciones de plugin por entrada (row badges, G3b, ADR 0037 decisión
//! 2): saneado + validación de rol compartidos por TUI y GUI — mismo
//! criterio que el saneado de spans con estilo de un preview
//! (`crate::viewer::Viewer::with_plugin_preview_styled`): texto de un
//! TERCERO, jamás confiado sin pasar por [`crate::display_name`], y un
//! `role` que llega SIN VALIDAR por el wire (`norte-core` es headless, no
//! conoce `norte_theme::Role` — ver el rustdoc de
//! `norte_core::Backend::plugin_decorate`) se valida AQUÍ, donde el
//! frontend por fin conoce el tema.

use std::collections::HashMap;

use norte_proto::VPath;
use norte_proto::methods::{DecorationWire, PluginDecorations};

/// Tope de un badge TRAS enmascarar (ADR 0037 tabla de decisión 1), en
/// CARACTERES (no bytes: coherente con el resto de topes de display —
/// `description`, `title`…). El server YA aplica un tope equivalente antes
/// de enviar; este es defensa en profundidad — un frontend remoto no confía
/// ciegamente en un daemon ajeno.
pub const BADGE_MAX_CHARS: usize = 8;

/// Una decoración SANEADA lista para pintar: `badge` ya enmascarado y
/// truncado a [`BADGE_MAX_CHARS`] caracteres (o `None` — sin badge para esa
/// entrada, ausente o vacío tras enmascarar/truncar, ambos casos idénticos
/// para el render); `role` ya validado contra `norte_theme::Role` (`None` =
/// sin rol, o un nombre que el frontend no reconoce).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Decoration {
    /// Badge corto ya enmascarado y acotado, o `None` = sin badge.
    pub badge: Option<String>,
    /// El badge se pinta DISTINTO de lo que es.
    ///
    /// Lo escribe un plugin y se pinta pegado a un nombre de fichero, que es
    /// el sitio donde una diferencia entre lo que se ve y lo que hay importa
    /// más. La marca se calculaba y se tiraba, como en otras seis
    /// superficies de la ventana gráfica.
    pub badge_hostile: bool,
    /// Rol semántico ya validado, o `None` = sin rol reconocido.
    pub role: Option<norte_theme::Role>,
}

/// Sanea UNA [`DecorationWire`] cruda del wire: enmascara `badge`
/// ([`crate::display_name`], mismo saneado que cualquier texto de plugin) y
/// lo trunca a [`BADGE_MAX_CHARS`] caracteres TRAS enmascarar; valida `role`
/// contra `norte_theme::Role::from_kebab` (un nombre desconocido colapsa a
/// `None`, nunca un panic ni una cadena libre que otra capa deba
/// re-interpretar — mismo criterio que ADR 0037 aplica a `SpanWire::role`).
#[must_use]
pub fn sanitize_decoration(w: &DecorationWire) -> Decoration {
    let mut badge_hostile = false;
    let badge = w.badge.as_deref().and_then(|b| {
        let (masked, hostil) = crate::display_name(b.as_bytes());
        let truncated: String = masked.chars().take(BADGE_MAX_CHARS).collect();
        // La marca se queda solo si queda badge: un badge que se enmascara
        // ENTERO a vacío no se pinta, y decir que lo pintado difiere de lo
        // real cuando no se pinta nada es ruido.
        let hay = !truncated.is_empty();
        badge_hostile = hostil && hay;
        hay.then_some(truncated)
    });
    let role = w.role.as_deref().and_then(norte_theme::Role::from_kebab);
    Decoration {
        badge,
        badge_hostile,
        role,
    }
}

/// Aplana la SUPERPOSICIÓN de decoradores del wire
/// (`PluginDecorateResult::plugins`, un elemento por plugin `decorator`
/// consentido) a UNA [`Decoration`] por ruta, indexada por [`VPath`]: el
/// PRIMER plugin (en el orden en que `plugins` llega — el orden del
/// catálogo, ver `PluginRegistry::resolve_decorators`) cuyo `badge` no es
/// `None` para esa posición GANA. Decisión de alcance MVP (G3b, judgment
/// call documentado): pintar la superposición COMPLETA (una fila de N
/// badges por entrada cuando N decoradores consienten sobre la misma
/// entrada) queda fuera de esta primera pasada — un solo badge por entrada
/// es la superficie de render que TUI/GUI implementan hoy.
///
/// `paths` y `pd.decorations` se recorren POSICIONALMENTE (`zip`, se
/// detiene en el más corto): defensa en profundidad si un daemon remoto no
/// respetara el contrato 1:1 del wire (ya validado server-side por
/// `decorations_to_wire_checked`, pero un cliente no confía ciegamente).
#[must_use]
pub fn merge_decorations(
    paths: &[VPath],
    plugins: &[PluginDecorations],
) -> HashMap<VPath, Decoration> {
    let mut out: HashMap<VPath, Decoration> = HashMap::new();
    for pd in plugins {
        for (path, wire) in paths.iter().zip(pd.decorations.iter()) {
            if wire.badge.is_none() || out.contains_key(path) {
                continue;
            }
            out.insert(path.clone(), sanitize_decoration(wire));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_decoration_trunca_tras_enmascarar_no_antes() {
        // 10 caracteres de control (se enmascaran a 10 '�'), luego truncar a 8.
        let w = DecorationWire {
            badge: Some("\n".repeat(10)),
            role: None,
        };
        let d = sanitize_decoration(&w);
        assert_eq!(
            d.badge.as_deref().map(str::chars).map(Iterator::count),
            Some(8)
        );
    }

    #[test]
    fn sanitize_decoration_vacio_tras_mask_es_none() {
        let w = DecorationWire {
            badge: Some(String::new()),
            role: None,
        };
        assert_eq!(sanitize_decoration(&w).badge, None);
    }

    #[test]
    fn sanitize_decoration_role_desconocido_colapsa_a_none() {
        let w = DecorationWire {
            badge: None,
            role: Some("no-es-un-role-real".to_string()),
        };
        assert_eq!(sanitize_decoration(&w).role, None);
    }

    #[test]
    fn sanitize_decoration_role_valido_se_reconoce() {
        let w = DecorationWire {
            badge: None,
            role: Some("warning".to_string()),
        };
        assert_eq!(
            sanitize_decoration(&w).role,
            norte_theme::Role::from_kebab("warning")
        );
    }

    fn vp(s: &str) -> VPath {
        VPath::parse(s).unwrap()
    }

    #[test]
    fn merge_decorations_primer_plugin_con_badge_gana() {
        let paths = vec![vp("mem:///a.rs"), vp("mem:///b.rs")];
        let plugins = vec![
            PluginDecorations {
                plugin_id: "p1".into(),
                decorations: vec![
                    DecorationWire {
                        badge: None,
                        role: None,
                    },
                    DecorationWire {
                        badge: Some("M".into()),
                        role: Some("warning".into()),
                    },
                ],
            },
            PluginDecorations {
                plugin_id: "p2".into(),
                decorations: vec![
                    DecorationWire {
                        badge: Some("X".into()),
                        role: None,
                    },
                    DecorationWire {
                        badge: Some("Y".into()),
                        role: None,
                    },
                ],
            },
        ];
        let merged = merge_decorations(&paths, &plugins);
        // a.rs: p1 sin badge, p2 con "X" -> gana p2.
        assert_eq!(
            merged.get(&vp("mem:///a.rs")).unwrap().badge.as_deref(),
            Some("X")
        );
        // b.rs: p1 con "M" ya presente primero -> gana p1, p2 no lo pisa.
        assert_eq!(
            merged.get(&vp("mem:///b.rs")).unwrap().badge.as_deref(),
            Some("M")
        );
    }

    #[test]
    fn merge_decorations_sin_badge_en_ningun_plugin_no_entra_en_el_mapa() {
        let paths = vec![vp("mem:///a.rs")];
        let plugins = vec![PluginDecorations {
            plugin_id: "p1".into(),
            decorations: vec![DecorationWire {
                badge: None,
                role: None,
            }],
        }];
        let merged = merge_decorations(&paths, &plugins);
        assert!(merged.is_empty());
    }
}
