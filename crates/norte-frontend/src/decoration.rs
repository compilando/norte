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
use norte_proto::methods::{DecorationSlot, DecorationWire, PluginDecorations};

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
    /// El ICONO de la fila (ADR 0105): lo que devolvió el primer decorador
    /// de hueco `icon`, ya enmascarado y acotado. Se pinta a la IZQUIERDA
    /// del nombre, en una columna de ancho fijo; la insignia de arriba, a la
    /// derecha. Los dos coexisten: vienen de plugins distintos.
    pub icon: Option<String>,
    /// El icono se pinta distinto de lo que es. Misma razón que
    /// `badge_hostile`.
    pub icon_hostile: bool,
}

impl Decoration {
    /// Sin nada que pintar: ni icono ni insignia.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.badge.is_none() && self.icon.is_none()
    }
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
        icon: None,
        icon_hostile: false,
    }
}

/// Lo mismo que [`sanitize_decoration`] para un decorador de ICONOS (ADR
/// 0105): el texto va al hueco del icono, con el mismo enmascarado y el
/// mismo tope. El rol no aplica: un icono se pinta con el color de la
/// entrada, no con el del tema del estado.
#[must_use]
pub fn sanitize_icon(w: &DecorationWire) -> Decoration {
    let s = sanitize_decoration(w);
    Decoration {
        badge: None,
        badge_hostile: false,
        role: None,
        icon: s.badge,
        icon_hostile: s.badge_hostile,
    }
}

/// Aplana la SUPERPOSICIÓN de decoradores del wire
/// (`PluginDecorateResult::plugins`, un elemento por plugin `decorator`
/// consentido) a UNA [`Decoration`] por ruta, indexada por [`VPath`], con
/// DOS huecos (ADR 0105): el icono, a la izquierda del nombre, y la
/// insignia, a la derecha. Cada hueco lo llena el PRIMER plugin de ese
/// hueco (en el orden en que `plugins` llega — el del catálogo, ver
/// `PluginRegistry::resolve_decorators`) cuyo texto no es `None` NI se
/// enmascara a nada: una insignia que se queda vacía tras enmascarar no es
/// una insignia, y no bloquea a la siguiente. Un icono no tapa una insignia
/// ni al revés; dos iconos sí se tapan, y manda el orden. Una ruta que se
/// queda sin nada en ninguno de los dos huecos no entra en el mapa.
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
            if wire.badge.is_none() {
                continue;
            }
            // Un hueco por plugin y el PRIMERO de cada hueco gana (ADR
            // 0105): un icono no tapa una insignia ni al revés, porque son
            // dos sitios de la fila; dos iconos sí se tapan, y manda el
            // orden del catálogo.
            let d = out.entry(path.clone()).or_default();
            match pd.slot {
                DecorationSlot::Icon if d.icon.is_none() => {
                    let s = sanitize_icon(wire);
                    d.icon = s.icon;
                    d.icon_hostile = s.icon_hostile;
                }
                DecorationSlot::Badge if d.badge.is_none() => {
                    let s = sanitize_decoration(wire);
                    d.badge = s.badge;
                    d.badge_hostile = s.badge_hostile;
                    d.role = s.role;
                }
                DecorationSlot::Icon | DecorationSlot::Badge => {}
            }
        }
    }
    // Una entrada que se enmascaró entera a nada no es una decoración.
    out.retain(|_, d| !d.is_empty());
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
                slot: DecorationSlot::Badge,
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
                slot: DecorationSlot::Badge,
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
            slot: DecorationSlot::Badge,
            decorations: vec![DecorationWire {
                badge: None,
                role: None,
            }],
        }];
        let merged = merge_decorations(&paths, &plugins);
        assert!(merged.is_empty());
    }

    /// ADR 0105: un icono y una insignia son dos HUECOS de la fila y no se
    /// tapan; dos iconos sí, y gana el primero. Un icono con rol lo pierde:
    /// se pinta con el color de la entrada.
    #[test]
    fn merge_decorations_icono_e_insignia_coexisten_y_dos_iconos_no() {
        let paths = vec![vp("mem:///a.rs")];
        let deco = |b: &str, r: Option<&str>| DecorationWire {
            badge: Some(b.into()),
            role: r.map(str::to_owned),
        };
        let plugins = vec![
            PluginDecorations {
                plugin_id: "icons".into(),
                slot: DecorationSlot::Icon,
                decorations: vec![deco("🦀", Some("warning"))],
            },
            PluginDecorations {
                plugin_id: "git".into(),
                slot: DecorationSlot::Badge,
                decorations: vec![deco("M", Some("warning"))],
            },
            PluginDecorations {
                plugin_id: "otros-iconos".into(),
                slot: DecorationSlot::Icon,
                decorations: vec![deco("X", None)],
            },
        ];
        let merged = merge_decorations(&paths, &plugins);
        let d = &merged[&paths[0]];
        assert_eq!(d.icon.as_deref(), Some("🦀"), "el primer icono");
        assert_eq!(d.badge.as_deref(), Some("M"), "y la insignia, aparte");
        assert_eq!(d.role, norte_theme::Role::from_kebab("warning"));
        assert!(!d.icon_hostile);
    }

    /// Una insignia que se queda en NADA —`Some("")`, que el saneado deja en
    /// `None`— no es una insignia: no bloquea a la del siguiente plugin.
    /// Antes el primer plugin con `Some(..)` se quedaba la ruta aunque su
    /// texto quedara vacío, y el mapa guardaba una decoración sin nada que
    /// pintar.
    #[test]
    fn merge_decorations_una_insignia_enmascarada_a_nada_no_bloquea_la_siguiente() {
        let paths = vec![vp("mem:///a.rs")];
        let deco = |b: &str| DecorationWire {
            badge: Some(b.into()),
            role: None,
        };
        let plugins = vec![
            PluginDecorations {
                plugin_id: "p1".into(),
                slot: DecorationSlot::Badge,
                decorations: vec![deco("")],
            },
            PluginDecorations {
                plugin_id: "p2".into(),
                slot: DecorationSlot::Badge,
                decorations: vec![deco("M")],
            },
        ];
        let merged = merge_decorations(&paths, &plugins);
        assert_eq!(merged[&paths[0]].badge.as_deref(), Some("M"));
        // Y con solo la vacía, la ruta no entra en el mapa.
        let sola = vec![PluginDecorations {
            plugin_id: "p1".into(),
            slot: DecorationSlot::Badge,
            decorations: vec![deco("")],
        }];
        assert!(merge_decorations(&paths, &sola).is_empty());
    }
}
