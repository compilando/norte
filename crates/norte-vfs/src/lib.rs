//! Contrato central del VFS de norte: el trait `Provider` y sus tipos.
//!
//! Todo backend de almacenamiento (local, sftp, s3, archive, memoria) implementa
//! este trait y pasa la misma suite contractual (`provider_contract!`, o
//! `readonly_provider_contract!` si declara `READ_ONLY` — ADR 0018).
//! Los providers no se conocen entre sí; las operaciones compuestas viven en
//! `norte-core` (spec §5).
#![forbid(unsafe_code)]

mod contract;
mod contract_ro;
pub mod deadline;
/// Conversión entre `VPath` y rutas NATIVAS del sistema.
///
/// Reglas de forma, no acceso a disco: por eso viven aquí y no en el provider
/// local, que es el único crate con `unsafe` y al que un frontend
/// daemon-only no debe arrastrar (ADR 0066, #254).
pub mod native;
mod options;
mod provider;
mod sink;
pub mod trash;
pub mod wtf8;

pub use norte_proto as proto;
pub use norte_proto::{ByteRange, Capabilities, CapabilityFlags, Entry, EntryKind, Error, VPath};
pub use options::{AttrRequest, ListOptions};
pub use provider::{
    ByteStream, ConfinedRoot, EntryStream, FollowLinks, NodeId, Provider, SymlinkKind,
};
pub use sink::ByteSink;

/// Re-exports internos para la expansión de [`provider_contract!`].
/// NO es API: puede cambiar sin aviso.
#[doc(hidden)]
pub mod __private {
    pub use bytes;
    pub use futures;
    pub use norte_proto;

    /// Aserciones del contrato de attrs (#108 bloque 2), COMPARTIDAS por las
    /// dos macros de contrato — duplicarlas dejaría que una divergencia
    /// (p. ej. una variante nueva de `AttrType` en un solo matcher) debilite
    /// una suite en silencio.
    pub mod contract_attrs {
        use norte_proto::{
            ATTR_BYTES_MAX, ATTR_TEXT_MAX, ATTRS_MAX_ADVERTISED, AttrInfo, AttrType, AttrValue,
            Entry, is_valid_attr_id,
        };

        /// ¿La variante del valor casa con el tipo declarado?
        #[must_use]
        pub fn attr_type_matches(ty: AttrType, v: &AttrValue) -> bool {
            matches!(
                (ty, v),
                (AttrType::Uint, AttrValue::Uint(_))
                    | (AttrType::Int, AttrValue::Int(_))
                    | (AttrType::Text, AttrValue::Text(_))
                    | (AttrType::Bytes, AttrValue::Bytes(_))
                    | (AttrType::TimeMs, AttrValue::TimeMs(_))
                    | (AttrType::Bool, AttrValue::Bool(_))
            )
        }

        /// Catálogo sano: acotado, ids válidos, sin duplicados.
        ///
        /// # Panics
        /// Si el catálogo viola cualquiera de las tres condiciones.
        pub fn assert_catalog_sane(catalog: &[AttrInfo]) {
            assert!(catalog.len() <= ATTRS_MAX_ADVERTISED, "catálogo sobre tope");
            let mut seen = std::collections::BTreeSet::new();
            for info in catalog {
                assert!(
                    is_valid_attr_id(&info.id),
                    "id inválido en catálogo: {:?}",
                    info.id
                );
                assert!(seen.insert(info.id.clone()), "id duplicado: {:?}", info.id);
            }
        }

        /// Contrato por entrada: solo ids pedidos, todos anunciados, tipo
        /// declarado ⟺ variante producida, Text/Bytes dentro de tope (bytes).
        ///
        /// # Panics
        /// Si alguna celda de `entry.attrs` viola el contrato.
        pub fn assert_attrs_contract(
            catalog: &[AttrInfo],
            requested: &crate::AttrRequest,
            entry: &Entry,
        ) {
            for (id, v) in &entry.attrs {
                assert!(
                    requested.wants(id),
                    "attr NO pedido en {:?}: {id:?}",
                    entry.path.display_lossy()
                );
                let info = catalog
                    .iter()
                    .find(|a| &a.id == id)
                    .unwrap_or_else(|| panic!("attr no anunciado: {id:?}"));
                assert!(
                    attr_type_matches(info.ty, v),
                    "tipo declarado {:?} no casa con {v:?} para {id:?}",
                    info.ty
                );
                match v {
                    AttrValue::Text(s) => {
                        assert!(s.len() <= ATTR_TEXT_MAX, "Text sobre tope: {id:?}");
                    }
                    AttrValue::Bytes(b) => {
                        assert!(b.len() <= ATTR_BYTES_MAX, "Bytes sobre tope: {id:?}");
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Cómo pliega nombres una UBICACIÓN, según lo que sus capacidades declaran.
///
/// Vive aquí —y no en `norte-compare`, de donde vino— porque es la regla que
/// dice qué SIGNIFICAN las banderas del [`Provider`] cuyo contrato define este
/// crate, y porque la preguntan tres capas que no se ven entre sí: el motor de
/// comparación, el core cuando decide si dos rutas son el mismo nodo, y la
/// ventana cuando comprueba si dos marcas de un lote colisionarían en el
/// destino (#268). Tres copias de tres líneas es como se separan.
///
/// `CASE_SENSITIVE` gana sobre nada, y `FULL_FOLD` gana sobre `CASE_SENSITIVE`:
/// un ext4 con `+F` declara los dos y pliega, que es lo que el orden dice.
///
/// Se pregunta por UBICACIÓN, jamás por provider (#215): un pincho FAT montado
/// bajo el mismo `file://` que un `/home` sensible a la caja da otra respuesta,
/// y contestar por el provider es contestar por el sitio equivocado.
///
/// ```
/// use norte_encoding::FoldMode;
/// use norte_proto::{Capabilities, CapabilityFlags};
/// use norte_vfs::fold_mode_of;
///
/// let ext4 = Capabilities { flags: CapabilityFlags::CASE_SENSITIVE, max_path: None };
/// assert_eq!(fold_mode_of(ext4), FoldMode::None);
///
/// let apfs = Capabilities { flags: CapabilityFlags::empty(), max_path: None };
/// assert_eq!(fold_mode_of(apfs), FoldMode::Simple);
///
/// let ext4_f = Capabilities {
///     flags: CapabilityFlags::CASE_SENSITIVE | CapabilityFlags::FULL_FOLD,
///     max_path: None,
/// };
/// assert_eq!(fold_mode_of(ext4_f), FoldMode::Full);
/// ```
#[must_use]
pub fn fold_mode_of(caps: norte_proto::Capabilities) -> norte_encoding::FoldMode {
    use norte_proto::CapabilityFlags;
    if caps.flags.contains(CapabilityFlags::FULL_FOLD) {
        norte_encoding::FoldMode::Full
    } else if caps.flags.contains(CapabilityFlags::CASE_SENSITIVE) {
        norte_encoding::FoldMode::None
    } else {
        norte_encoding::FoldMode::Simple
    }
}
