//! Opciones de listado/stat ([`ListOptions`]) y la petición saneada de
//! atributos ([`AttrRequest`]) que viaja con ellas (#108 bloque 2, ADR 0039).

use norte_proto::ATTRS_MAX_REQUEST;

/// Ids de atributos solicitados, saneados: todos válidos según
/// [`norte_proto::is_valid_attr_id`], sin duplicados (el primero gana) y a lo
/// sumo [`ATTRS_MAX_REQUEST`]. Este tipo FILTRA — rechazar una petición
/// malformada con `-32602` es trabajo del daemon, *antes* de construir uno.
///
/// ```
/// use norte_vfs::AttrRequest;
/// let req = AttrRequest::sanitized(["posix.mode".to_owned(), "BAD".to_owned()]);
/// assert!(req.wants("posix.mode"));
/// assert!(!req.wants("BAD"));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttrRequest(Vec<String>);

impl AttrRequest {
    /// Construye filtrando: id inválido fuera, duplicado fuera (el primero
    /// gana), truncado a [`ATTRS_MAX_REQUEST`].
    #[must_use]
    pub fn sanitized<I: IntoIterator<Item = String>>(ids: I) -> Self {
        let mut out: Vec<String> = Vec::new();
        for id in ids {
            if out.len() == ATTRS_MAX_REQUEST {
                break;
            }
            if norte_proto::is_valid_attr_id(&id) && !out.contains(&id) {
                out.push(id);
            }
        }
        Self(out)
    }

    /// Sin atributos pedidos: el provider debe tomar su camino rápido pelado.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// ¿Está pedido `id`? Los providers condicionan cada materialización aquí.
    #[must_use]
    pub fn wants(&self, id: &str) -> bool {
        self.0.iter().any(|have| have == id)
    }

    /// Ids pedidos, en orden de petición.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

/// Opciones de [`Provider::list_with`](crate::Provider::list_with) y
/// [`Provider::stat_with`](crate::Provider::stat_with). Struct para que
/// futuras opciones de listado no vuelvan a agitar todas las firmas.
///
/// ```
/// use norte_vfs::ListOptions;
/// assert!(ListOptions::default().attrs.is_empty());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListOptions {
    /// Atributos a materializar por entrada. Vacío = entradas peladas.
    pub attrs: AttrRequest,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_filtra_invalidos_dedup_primero_gana_y_trunca() {
        let ids = vec![
            "posix.mode".to_owned(),
            "MAYUS.no".to_owned(),   // inválido: mayúsculas
            "sindot".to_owned(),     // inválido: sin punto
            "posix.mode".to_owned(), // duplicado
            "s3.etag".to_owned(),
        ];
        let req = AttrRequest::sanitized(ids);
        assert_eq!(req.iter().collect::<Vec<_>>(), ["posix.mode", "s3.etag"]);
        assert!(req.wants("posix.mode"));
        assert!(!req.wants("mayus.no"));

        // Truncado al tope del wire: 20 ids válidos → 16.
        let many = (0..20).map(|i| format!("a.b{i}"));
        assert_eq!(
            AttrRequest::sanitized(many).iter().count(),
            norte_proto::ATTRS_MAX_REQUEST
        );
    }

    #[test]
    fn default_es_vacio_y_list_options_lo_envuelve() {
        assert!(AttrRequest::default().is_empty());
        assert!(ListOptions::default().attrs.is_empty());
    }
}
