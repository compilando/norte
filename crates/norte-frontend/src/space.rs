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
    if path.scheme() != "file" || path.authority().is_some() {
        return None;
    }
    volumes
        .iter()
        .filter(|v| norte_proto::methods::RelPath::under(&v.mount, path).is_some())
        .max_by_key(|v| v.mount.segments().count())?
        .free_bytes
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
