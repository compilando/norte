//! Los permisos POSIX vistos desde un frontend (#314): leerlos de un listado,
//! escribirlos en octal, y volverlos a leer.
//!
//! La regla vive aquí y no en la terminal porque es la misma en la ventana, y
//! una decisión duplicada entre frontends diverge en silencio (ADR 0077).

/// Los doce bits que se pueden cambiar: `rwx` por dueño, grupo y otros, más
/// setuid, setgid y sticky. Los de arriba dicen de qué CLASE es el nodo, y eso
/// no se cambia.
///
/// Es la constante del PROTOCOLO, reexportada: dos definiciones del mismo
/// número en dos crates es exactamente como divergen.
pub use norte_proto::methods::MODE_PERMISSION_BITS as PERMISSION_BITS;

/// Por qué un modo tecleado no vale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeError {
    /// Vacío, o con algo que no es un dígito octal.
    NotOctal,
    /// Es octal y se sale de los doce bits.
    TooBig,
}

impl ModeError {
    /// La clave Fluent con la que se dice.
    #[must_use]
    pub const fn message_key(self) -> &'static str {
        match self {
            Self::NotOctal => "msg-chmod-not-octal",
            Self::TooBig => "msg-chmod-too-big",
        }
    }
}

/// Lee un modo tecleado en OCTAL (`755`, `0644`, `4755`).
///
/// En octal porque es la forma que un listado enseña y la que teclea quien
/// sabe lo que quiere. Decimal sería una trampa silenciosa: `755` en decimal
/// es `0o1363`, un modo perfectamente válido y completamente distinto del que
/// el humano tenía en la cabeza.
///
/// ```
/// use norte_frontend::chmod::{parse_mode, ModeError};
/// assert_eq!(parse_mode("755"), Ok(0o755));
/// assert_eq!(parse_mode("0644"), Ok(0o644));
/// assert_eq!(parse_mode("4755"), Ok(0o4755), "setuid también");
/// assert_eq!(parse_mode("8"), Err(ModeError::NotOctal));
/// assert_eq!(parse_mode("77777"), Err(ModeError::TooBig));
/// ```
///
/// # Errors
///
/// [`ModeError::NotOctal`] si está vacío o tiene algo que no es un dígito
/// octal; [`ModeError::TooBig`] si se sale de los doce bits.
pub fn parse_mode(texto: &str) -> Result<u32, ModeError> {
    let t = texto.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit() && b < b'8') {
        return Err(ModeError::NotOctal);
    }
    let n = u32::from_str_radix(t, 8).map_err(|_| ModeError::TooBig)?;
    if n & !PERMISSION_BITS != 0 {
        return Err(ModeError::TooBig);
    }
    Ok(n)
}

/// El modo en octal de cuatro dígitos, que es como se prellena el campo.
///
/// ```
/// use norte_frontend::chmod::format_mode;
/// assert_eq!(format_mode(0o755), "0755");
/// assert_eq!(format_mode(0o4755), "4755");
/// ```
#[must_use]
pub fn format_mode(mode: u32) -> String {
    format!("{:04o}", mode & PERMISSION_BITS)
}

/// El modo POSIX de una entrada, si su listado lo trae (#314).
///
/// Sale del atributo `posix.mode` que publican los providers que tienen
/// permisos. `None` = este listado no lo pidió, o esta ubicación no los tiene:
/// las dos cosas significan lo mismo para quien pinta, que es «no lo sé», y
/// ninguna autoriza a inventarse un `0644` por defecto.
#[must_use]
pub fn mode_of(entry: &norte_proto::Entry) -> Option<u32> {
    match entry.attrs.get("posix.mode")? {
        norte_proto::AttrValue::Uint(m) => u32::try_from(*m).ok().map(|m| m & PERMISSION_BITS),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La trampa que justifica el octal: `755` leído en decimal es un modo
    /// legal y distinto, así que equivocarse aquí no daría un error — daría
    /// unos permisos que nadie pidió.
    #[test]
    fn se_lee_en_octal_y_no_en_decimal() {
        assert_eq!(parse_mode("755"), Ok(0o755));
        assert_ne!(parse_mode("755"), Ok(755));
    }

    #[test]
    fn el_espacio_alrededor_no_estorba() {
        assert_eq!(parse_mode("  644 "), Ok(0o644));
    }

    #[test]
    fn los_digitos_que_no_son_octales_se_rechazan() {
        for malo in ["8", "9", "75a", "-1", "", "   ", "0x1ff"] {
            assert_eq!(parse_mode(malo), Err(ModeError::NotOctal), "{malo:?}");
        }
    }

    /// Los bits de clase de nodo no son un permiso: `100644` es «fichero
    /// regular con 644», y fijarlo entero pediría cambiar de qué clase es.
    #[test]
    fn los_bits_de_clase_no_caben() {
        assert_eq!(parse_mode("100644"), Err(ModeError::TooBig));
        assert_eq!(parse_mode("10000"), Err(ModeError::TooBig));
    }

    #[test]
    fn ida_y_vuelta() {
        for m in [0o644, 0o755, 0o600, 0o4755, 0o1777, 0] {
            assert_eq!(parse_mode(&format_mode(m)), Ok(m));
        }
    }

    #[test]
    fn el_modo_sale_del_atributo_del_listado() {
        let mut e = norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: norte_proto::VPath::parse("file:///a").expect("wire"),
            kind: norte_proto::EntryKind::File,
            size: None,
            mtime_ms: None,
        };
        assert_eq!(mode_of(&e), None, "sin el atributo, no se sabe");
        // `st_mode` entero: los bits de clase se recortan al leer, porque lo
        // que se puede ESCRIBIR son los doce de abajo.
        e.attrs.insert(
            "posix.mode".to_owned(),
            norte_proto::AttrValue::Uint(0o100_644),
        );
        assert_eq!(mode_of(&e), Some(0o644));
    }
}
