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
mod options;
mod provider;
mod sink;
pub mod trash;

pub use norte_proto as proto;
pub use norte_proto::{ByteRange, Capabilities, CapabilityFlags, Entry, EntryKind, Error, VPath};
pub use options::{AttrRequest, ListOptions};
pub use provider::{ByteStream, EntryStream, FollowLinks, NodeId, Provider, SymlinkKind};
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
