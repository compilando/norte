//! La hoja de atributos: qué filas describen a lo que hay bajo el cursor.
//!
//! Vivía DOS veces —`norte-tui/src/ui/panels.rs` y
//! `norte-ui-host/src/controller/places.rs`— con el mismo orden de campos, el
//! mismo formato de tamaño y el mismo recorrido de atributos, copiados a
//! mano. Dos copias de una regla de presentación divergen, y estas ya lo
//! habían hecho: la ventana marcaba un valor de atributo hostil y el TUI no,
//! porque el arreglo se aplicó en una sola.
//!
//! Aquí está una vez. Los frontends la PINTAN; ninguno decide qué va dentro.
//!
//! No pide nada: todo sale de la [`Entry`] que el listado ya tenía. Un panel
//! que sigue al cursor y además pide datos por cada fila es como bajar por un
//! directorio se convierte en una tormenta de peticiones.

use norte_i18n::{Lang, t_in};
use norte_proto::{AttrCatalog, Entry, EntryKind};

use crate::columns::{ColumnId, ColumnStyle, header_label_in, styled_cell};

/// Una fila de la hoja: etiqueta, valor y si el valor lleva bytes que hubo
/// que enmascarar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// La etiqueta, ya traducida —o la cabecera que el catálogo da al
    /// atributo.
    pub label: String,
    /// El valor, ya formateado y saneado.
    pub value: String,
    /// El valor llevaba bytes que no se podían pintar. Quien lo enseñe lo
    /// MARCA, igual que la columna equivalente.
    pub hostile: bool,
}

/// Las filas que describen `entry`.
///
/// `fila_de_subir` dice si lo que hay bajo el cursor es la fila `..`. Sobre
/// ella la hoja NO se llama como el directorio padre: se llama `..`, como en
/// el listado, y añade a dónde lleva. Describir la fila con el nombre del
/// padre haría creer que el cursor está sobre el padre, que es exactamente lo
/// que la fila no es.
///
/// `catalog` es el de atributos del esquema de la entrada, si se conoce: los
/// atributos que el provider ya trajo van por la MISMA puerta que su columna
/// equivalente, para que la hoja y la columna no puedan discrepar sobre lo
/// que vale un atributo.
///
/// ```
/// use norte_frontend::metadata::sheet;
/// use norte_proto::{Entry, EntryKind, VPath};
///
/// let e = Entry {
///     path: VPath::parse("mem:///casa/leeme.txt").unwrap(),
///     kind: EntryKind::File,
///     size: Some(12),
///     mtime_ms: None,
///     attrs: std::collections::BTreeMap::new(),
/// };
/// let filas = sheet(&e, false, None, norte_i18n::Lang::En);
/// assert_eq!(filas[0].value, "leeme.txt");
/// ```
#[must_use]
pub fn sheet(
    entry: &Entry,
    fila_de_subir: bool,
    catalog: Option<&AttrCatalog>,
    lang: Lang,
) -> Vec<Field> {
    let mut fields = Vec::new();
    let mut campo = |clave: &str, value: String, hostile: bool| {
        fields.push(Field {
            label: t_in(lang, clave),
            value,
            hostile,
        });
    };

    if fila_de_subir {
        // `..` es el nombre que el listado pinta en esa fila, y aquí se
        // repite: la hoja y la fila que describe se leen igual.
        campo("metadata-name", "..".to_owned(), false);
        campo("metadata-kind", t_in(lang, "metadata-kind-dir"), false);
        let (destino, hostil) = crate::display::path_display(&entry.path);
        campo("metadata-target", destino, hostil);
        // Ni tamaño, ni fecha, ni atributos: la `Entry` sintética no los trae
        // —no son de este directorio— y rellenarlos sería contestar por el
        // padre sin haberlo mirado.
        return fields;
    }

    let nombre = entry
        .path
        .file_name()
        .map_or_else(Vec::new, |s| s.as_bytes().to_vec());
    let (pintable, hostil) = crate::display::display_name(&nombre);
    campo("metadata-name", pintable, hostil);
    campo(
        "metadata-kind",
        t_in(
            lang,
            match entry.kind {
                EntryKind::Dir => "metadata-kind-dir",
                EntryKind::File => "metadata-kind-file",
                EntryKind::Symlink => "metadata-kind-symlink",
                EntryKind::Other => "metadata-kind-other",
            },
        ),
        false,
    );
    if let Some(n) = entry.size {
        // El humano y el exacto, los dos: «1,2 MiB» no sirve para comparar y
        // `1258291` no sirve para leer.
        campo(
            "metadata-size",
            format!("{} ({n})", crate::human_bytes_short(n)),
            false,
        );
    }
    if let Some(ms) = entry.mtime_ms {
        campo(
            "metadata-mtime",
            crate::columns::format_mtime(ms, crate::columns::TimeFormat::Iso, ms),
            false,
        );
    }
    let ahora = entry.mtime_ms.unwrap_or(0);
    for attr in entry.attrs.keys() {
        let col = ColumnId::Attr(attr.clone());
        let style = ColumnStyle::default_for_id(&col, catalog);
        let Some(celda) = styled_cell(entry, &col, ahora, &style) else {
            continue;
        };
        // La marca se saca del valor CRUDO, no de la celda ya formateada:
        // `styled_cell` enmascara por dentro y no devuelve la bandera, y
        // volver a preguntársela a lo ya enmascarado no contesta nada
        // —U+FFFD no es un peligro de terminal, así que un valor ya
        // convertido se declara fiel—.
        let hostil = match entry.attrs.get(attr) {
            Some(norte_proto::AttrValue::Text(t)) => crate::display::display_name(t.as_bytes()).1,
            Some(norte_proto::AttrValue::Bytes(b)) => crate::display::display_name(b).1,
            // Los demás son números o marcas de tiempo que formatea norte: no
            // hay texto de tercero que enmascarar. ENUMERADOS y no `_`: el día
            // que `AttrValue` gane una variante con texto dentro, esto tiene
            // que ser un error de compilación y no una declaración silenciosa
            // de que unos bytes ajenos son fieles.
            Some(
                norte_proto::AttrValue::Uint(_)
                | norte_proto::AttrValue::Int(_)
                | norte_proto::AttrValue::TimeMs(_)
                | norte_proto::AttrValue::Bool(_)
                | norte_proto::AttrValue::Unknown,
            )
            | None => false,
        };
        fields.push(Field {
            // `_in` y no la global: quien pasa `lang` lo hace porque el suyo
            // no tiene por qué ser el del proceso, y media hoja traducida es
            // peor que ninguna.
            label: header_label_in(&col, &style, catalog, lang),
            value: celda,
            hostile: hostil,
        });
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{AttrValue, VPath};

    fn entrada(wire: &str, kind: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).expect("vpath de test"),
            kind,
            size: None,
            mtime_ms: None,
        }
    }

    fn etiquetas(filas: &[Field]) -> Vec<&str> {
        filas.iter().map(|f| f.label.as_str()).collect()
    }

    /// Un fichero: nombre, clase, tamaño humano Y exacto, y fecha en ISO.
    #[test]
    fn un_fichero_lleva_nombre_clase_tamano_y_fecha() {
        let mut e = entrada("mem:///casa/leeme.txt", EntryKind::File);
        e.size = Some(1_258_291);
        e.mtime_ms = Some(1_700_000_000_000);
        let filas = sheet(&e, false, None, Lang::Es);
        assert_eq!(
            etiquetas(&filas),
            ["Nombre", "Clase", "Tamaño", "Modificado"]
        );
        assert_eq!(filas[0].value, "leeme.txt");
        assert_eq!(filas[1].value, "fichero");
        assert!(
            filas[2].value.contains("(1258291)"),
            "el número exacto va al lado del humano: {}",
            filas[2].value
        );
    }

    /// Sin tamaño ni fecha no se inventa una fila vacía: la fila no está.
    #[test]
    fn lo_que_no_se_sabe_no_sale() {
        let e = entrada("mem:///casa/dir", EntryKind::Dir);
        let filas = sheet(&e, false, None, Lang::Es);
        assert_eq!(etiquetas(&filas), ["Nombre", "Clase"]);
        assert_eq!(filas[1].value, "carpeta");
    }

    /// La fila `..` se describe como `..` y dice A DÓNDE lleva.
    ///
    /// El bug que cierra: la hoja la nombraba con el basename del padre
    /// —«oscar» estando en `/home/oscar/Downloads`— o, antes de eso, no la
    /// describía en absoluto.
    #[test]
    fn la_fila_de_subir_se_llama_dos_puntos_y_dice_a_donde_lleva() {
        let e = entrada("mem:///casa", EntryKind::Dir);
        let filas = sheet(&e, true, None, Lang::Es);
        assert_eq!(etiquetas(&filas), ["Nombre", "Clase", "Destino"]);
        assert_eq!(filas[0].value, "..", "no el nombre del padre");
        assert_eq!(filas[1].value, "carpeta");
        assert_eq!(
            filas[2].value, "⟨mem⟩/casa",
            "la MISMA forma que la cabecera del listado, no el wire"
        );
    }

    /// Un nombre que no es UTF-8 llega enmascarado Y marcado.
    #[test]
    fn un_nombre_hostil_va_marcado() {
        let e = entrada("mem:///casa/%FF%FE", EntryKind::File);
        let filas = sheet(&e, false, None, Lang::Es);
        assert!(filas[0].hostile, "{:?}", filas[0]);
        assert!(
            !filas[1].hostile,
            "la clase la escribe norte: nunca es hostil"
        );
    }

    /// Y un VALOR de atributo hostil también.
    ///
    /// Este es el que estaba mal en el TUI: la ventana lo marcaba, el TUI no,
    /// y la COLUMNA equivalente sí en los dos. La hoja decía que unos bytes
    /// eran fieles mientras la columna de al lado decía que no.
    #[test]
    fn un_valor_de_atributo_hostil_tambien_va_marcado() {
        let mut e = entrada("mem:///casa/x", EntryKind::File);
        e.attrs
            .insert("dueño".to_owned(), AttrValue::Bytes(b"\xff\xfe".to_vec()));
        let filas = sheet(&e, false, None, Lang::Es);
        let attr = filas.last().expect("el atributo sale detrás de lo fijo");
        assert!(attr.hostile, "{attr:?}");
    }
}
