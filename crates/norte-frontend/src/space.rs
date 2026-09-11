//! ¿Cabe en el destino? (#149)
//!
//! Una copia jamás preguntaba si el destino tenía sitio, y el espacio libre ya
//! se enumera desde el ítem 3 del roadmap. Lo que faltaba era la pregunta, y
//! sobre todo **qué hacer con la respuesta**: aquí se AVISA y se deja seguir,
//! nunca se rehúsa.
//!
//! El motivo de no rehusar no es timidez. «No cabe» se equivoca a menudo:
//! ficheros dispersos, compresión del propio filesystem, cuotas por usuario, y
//! un destino que informa del espacio de OTRO filesystem que el que va a
//! recibir los bytes. Negar una copia que sí cabía es peor que dejar que el
//! humano decida con el número delante.
//!
//! # Cuándo se calla, que es la mitad del contrato
//!
//! - **El destino no sabe contestar.** SFTP, S3 y un archivo no tienen
//!   concepto de espacio libre, o lo tienen y no es fiable.
//!   [`norte_proto::methods::Volume::free_bytes`] es `Option<u64>` y ausente
//!   significa «no contestó a tiempo», JAMÁS cero — confundirlos convertiría
//!   cada montaje lento en una falsa alarma.
//! - **No se sabe cuánto se va a mover.** Un directorio no trae tamaño en el
//!   listado, y sumar solo lo que sí lo trae daría un total menor que el real:
//!   avisar con él sería avisar de menos, y no avisar con él es lo honesto.

use norte_i18n::{Lang, ta_in};

/// El aviso de espacio, o `None` cuando no hay nada honesto que decir.
///
/// `total` es lo que se va a escribir y `free` lo que el destino dice tener;
/// cualquiera de los dos ausente calla. Que quepa también calla: un «sí cabe»
/// en cada copia es ruido que enseña a ignorar la línea.
///
/// ```
/// use norte_frontend::space::warning;
/// use norte_i18n::Lang;
///
/// // No cabe: se dice, con los dos números.
/// assert!(warning(Some(4_200_000_000), Some(1_100_000_000), Lang::En).is_some());
/// // Cabe: nada que decir.
/// assert!(warning(Some(10), Some(1_000), Lang::En).is_none());
/// // El destino no contesta, o no se sabe cuánto se mueve: silencio, jamás
/// // una falsa alarma.
/// assert!(warning(Some(10), None, Lang::En).is_none());
/// assert!(warning(None, Some(10), Lang::En).is_none());
/// ```
#[must_use]
pub fn warning(total: Option<u64>, free: Option<u64>, lang: Lang) -> Option<String> {
    let (total, free) = (total?, free?);
    if total <= free {
        return None;
    }
    Some(ta_in(
        lang,
        "space-warning",
        &[
            ("size", &crate::human_bytes(total)),
            ("free", &crate::human_bytes(free)),
        ],
    ))
}

/// Los bytes que una transferencia va a escribir, o `None` si alguno de los
/// ítems no lo dice.
///
/// La otra mitad del contrato de [`warning`], y la que decide si hay pregunta
/// que hacer. Es TODO o nada: un directorio no trae tamaño en el listado, así
/// que sumar solo lo que sí lo trae daría un total menor que el real y
/// avisaría de menos — que es peor que callar, porque la línea que sí sale se
/// lee como completa.
///
/// Vive aquí y no en un frontend porque los dos hacen la misma pregunta al
/// abrir el mismo diálogo, y un total calculado con otra regla es una alarma
/// que aparece en un frontend y no en el otro (ADR 0077).
///
/// Un ítem que no está en `entries` tampoco suma: no es que ocupe cero, es
/// que no se sabe.
///
/// ```
/// use norte_frontend::space::total_to_write;
/// use norte_proto::{Entry, EntryKind, VPath};
///
/// let en = |wire: &str, kind, size| Entry {
///     attrs: std::collections::BTreeMap::new(),
///     path: VPath::parse(wire).unwrap(),
///     kind,
///     size,
///     mtime_ms: None,
/// };
/// let uno = VPath::parse("file:///a").unwrap();
/// let dos = VPath::parse("file:///b").unwrap();
/// let dir = VPath::parse("file:///d").unwrap();
/// let listado = [
///     en("file:///a", EntryKind::File, Some(10)),
///     en("file:///b", EntryKind::File, Some(32)),
///     en("file:///d", EntryKind::Dir, None),
/// ];
///
/// assert_eq!(total_to_write(&listado, &[uno.clone(), dos]), Some(42));
/// // Un directorio no dice cuánto ocupa: NO hay total, ni siquiera parcial.
/// assert_eq!(total_to_write(&listado, &[uno, dir]), None);
/// ```
#[must_use]
pub fn total_to_write(entries: &[norte_proto::Entry], items: &[norte_proto::VPath]) -> Option<u64> {
    // Por índice cuando el producto se va de las manos, y lineal cuando no.
    // El caso corriente son tres marcas sobre un listado normal, donde
    // construir un mapa cuesta más que buscar; el caso que importa son 512
    // marcas —el tope de un lote— sobre un directorio de cien mil entradas,
    // que son cincuenta millones de comparaciones de `VPath` con la interfaz
    // parada, porque esto corre en el hilo del actor antes de emitir el
    // parche.
    if items.len().saturating_mul(entries.len()) > 100_000 {
        let indice: std::collections::HashMap<&norte_proto::VPath, &norte_proto::Entry> =
            entries.iter().map(|e| (&e.path, e)).collect();
        return items
            .iter()
            .try_fold(0_u64, |total, p| bytes_de(indice.get(p).copied()?, total));
    }
    items.iter().try_fold(0_u64, |total, path| {
        bytes_de(entries.iter().find(|e| &e.path == path)?, total)
    })
}

/// Suma una entrada al total, o `None` si esa entrada no dice cuánto ocupa.
fn bytes_de(entry: &norte_proto::Entry, total: u64) -> Option<u64> {
    if entry.kind != norte_proto::EntryKind::File {
        return None;
    }
    total.checked_add(entry.size?)
}

/// El espacio libre del volumen que sirve `path`, o `None` si ninguno lo
/// sirve o el que lo sirve no contestó.
///
/// El volumen es el de punto de montaje MÁS LARGO que sea prefijo del path:
/// con `/` y `/home` montados aparte, un fichero de `/home/u` lo sirve
/// `/home`, y preguntarle a `/` daría el número de otro disco.
///
/// Solo `file://`: un `sftp://` o un `s3://` no cuelgan de ningún montaje de
/// esta máquina, y responder con el espacio del disco local sería contestar
/// otra pregunta.
#[must_use]
pub fn free_for(
    path: &norte_proto::VPath,
    volumes: &[norte_proto::methods::Volume],
) -> Option<u64> {
    volume_for(path, volumes)?.free_bytes
}

/// Cuánto del volumen de `path` está OCUPADO, en `0.0..=1.0` (spec
/// 2026-09-11 V5: el indicador de espacio del pie de la ventana). `None`
/// cuando no se sabe el total o lo libre, o el esquema no es local — el
/// mismo criterio que [`free_for`].
#[must_use]
pub fn used_ratio_for(
    path: &norte_proto::VPath,
    volumes: &[norte_proto::methods::Volume],
) -> Option<f32> {
    let v = volume_for(path, volumes)?;
    let total = v.total_bytes.filter(|t| *t > 0)?;
    let free = v.free_bytes?.min(total);
    // Precisión de f32 de sobra para una barra: el cociente cabe en 24 bits
    // mucho antes de que un píxel lo note.
    #[allow(clippy::cast_precision_loss)]
    let ratio = 1.0 - (free as f64 / total as f64);
    Some(ratio.clamp(0.0, 1.0) as f32)
}

/// El volumen MÁS PROFUNDO que contiene a `path`, solo para rutas locales.
fn volume_for<'a>(
    path: &norte_proto::VPath,
    volumes: &'a [norte_proto::methods::Volume],
) -> Option<&'a norte_proto::methods::Volume> {
    if path.scheme() != "file" || path.authority().is_some() {
        return None;
    }
    volumes
        .iter()
        .filter(|v| norte_proto::methods::RelPath::under(&v.mount, path).is_some())
        .max_by_key(|v| v.mount.segments().count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::VPath;
    use norte_proto::methods::{Volume, VolumeKind};

    fn vol(mount: &str, free: Option<u64>) -> Volume {
        Volume {
            mount: VPath::parse(mount).expect("wire"),
            label: None,
            fs_type: "ext4".into(),
            kind: VolumeKind::Fixed,
            total_bytes: Some(1_000),
            free_bytes: free,
            read_only: false,
        }
    }

    fn entrada(wire: &str, kind: norte_proto::EntryKind, size: Option<u64>) -> norte_proto::Entry {
        norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).expect("wire"),
            kind,
            size,
            mtime_ms: None,
        }
    }

    /// Las tres formas de «no se sabe», que son las que el doctest no toca y
    /// las que importan: las tres tienen que dar `None` entero, jamás una
    /// suma parcial. Un total menor que el real avisa de menos, y la línea
    /// que sí sale se lee como completa.
    #[test]
    fn lo_que_no_se_sabe_no_suma_a_medias() {
        use norte_proto::EntryKind;
        let listado = [
            entrada("file:///a", EntryKind::File, Some(10)),
            entrada("file:///sin", EntryKind::File, None),
            entrada("file:///enlace", EntryKind::Symlink, Some(4)),
        ];
        let p = |w: &str| VPath::parse(w).expect("wire");

        assert_eq!(
            total_to_write(&listado, &[p("file:///a"), p("file:///sin")]),
            None,
            "un fichero que no dice cuánto ocupa se lleva el total entero"
        );
        assert_eq!(
            total_to_write(&listado, &[p("file:///a"), p("file:///enlace")]),
            None,
            "un enlace tampoco: lo que se copia es a lo que apunta, y eso no \
             está en este listado"
        );
        assert_eq!(
            total_to_write(&listado, &[p("file:///a"), p("file:///fantasma")]),
            None,
            "lo que no está en el listado no ocupa cero: no se sabe"
        );
        assert_eq!(
            total_to_write(&listado, &[]),
            Some(0),
            "no copiar nada sí se sabe cuánto ocupa"
        );
    }

    /// Y una suma que desborda tampoco inventa: `checked_add` calla.
    #[test]
    fn una_suma_que_desborda_calla() {
        use norte_proto::EntryKind;
        let listado = [
            entrada("file:///a", EntryKind::File, Some(u64::MAX)),
            entrada("file:///b", EntryKind::File, Some(1)),
        ];
        let items = [
            VPath::parse("file:///a").expect("wire"),
            VPath::parse("file:///b").expect("wire"),
        ];
        assert_eq!(total_to_write(&listado, &items), None);
    }

    /// El montaje más ESPECÍFICO manda: con `/` y `/home` montados aparte,
    /// preguntar por `/home/u` y que conteste `/` sería el número de otro
    /// disco.
    #[test]
    fn gana_el_punto_de_montaje_mas_largo() {
        let vols = [vol("file:///", Some(10)), vol("file:///home", Some(99))];
        let libre = free_for(&VPath::parse("file:///home/u/x.txt").expect("wire"), &vols);
        assert_eq!(libre, Some(99));
    }

    /// Un provider que no cuelga de esta máquina no tiene volumen que
    /// preguntar, y contestar con el del disco local sería contestar otra
    /// pregunta.
    #[test]
    fn un_destino_remoto_no_tiene_espacio_que_mirar() {
        let vols = [vol("file:///", Some(10))];
        assert_eq!(
            free_for(&VPath::parse("sftp://h/casa/x").expect("wire"), &vols),
            None
        );
        assert_eq!(
            free_for(&VPath::parse("file://servidor/x").expect("wire"), &vols),
            None,
            "un `file://` CON authority tampoco es este disco"
        );
    }

    /// Y un volumen que no contestó a tiempo se distingue de uno lleno:
    /// `None` no es cero, y tratarlo como cero sería una falsa alarma en cada
    /// montaje lento.
    #[test]
    fn un_volumen_que_no_contesta_calla() {
        let vols = [vol("file:///", None)];
        let libre = free_for(&VPath::parse("file:///x").expect("wire"), &vols);
        assert_eq!(libre, None);
        assert!(warning(Some(1_000_000), libre, Lang::En).is_none());
    }
}
