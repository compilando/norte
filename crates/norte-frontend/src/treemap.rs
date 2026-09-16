//! El mapa de disco, repartido en rectángulos (fase 4, T3).
//!
//! Entra una lista de hijos ya medidos (`fs.dir_usage`) y sale un
//! [`StyledFrame`]: líneas con estilo y zonas pulsables, que es lo que los dos
//! frontends ya saben pintar desde la fase 3. El reparto vive aquí y no en cada
//! uno porque un treemap calculado dos veces son dos treemaps distintos en
//! cuanto alguien toque un redondeo — la lección de ADR 0077.
//!
//! # El marco es NUESTRO
//! En un panel de plugin la etiqueta Y el comando los elige un tercero, y por
//! eso existe `zona_puede` (ADR 0116). Aquí los elige esta función: cada
//! rectángulo nombra `nav.enter` sobre un hijo del directorio que se está
//! enseñando, así que sus zonas no pasan por ese filtro y un plugin no puede
//! fabricar un marco de `disk-map`.
//!
//! # El `arg` es el nombre en forma WIRE, nunca lo que se pinta
//! Lo pintado pasa por [`crate::display_name`], que enmascara: un nombre con
//! bytes de control se ve como `�` y ESA forma no identifica ningún fichero. El
//! `arg` lleva [`Segment::to_wire`], que es reversible, y quien lo recibe
//! resuelve `padre.join(Segment::parse_wire(arg))`. Un nombre no-UTF8, uno en
//! NFD o uno llamado `!` llegan enteros o no llegan.

use norte_proto::EntryKind;
use norte_proto::methods::DirUsageChild;
use norte_theme::Role;

use crate::ansi::StyledSpan;
use crate::frame::{Hit, MAX_HITS, StyledFrame};

/// El comando que corre un rectángulo: entrar en ese hijo.
const COMANDO: &str = "nav.enter";

/// Marca de un hijo cuyo tamaño es una COTA INFERIOR (`partial`).
///
/// Va en la etiqueta y no en el color: el color dice de qué CLASE es el fichero,
/// y un rectángulo incompleto puede ser de cualquier clase. Quien pinta no tiene
/// que elegir entre las dos cosas.
const CASI: char = '≈';

/// De qué clase es un hijo, para que el mapa lo pinte como lo que es.
///
/// No existía ninguna taxonomía que reutilizar: el decorador de un plugin
/// recibe un ROL del tema, no una clase (ADR 0105), así que esta es la primera
/// y vive aquí, donde la usan los dos frontends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clase {
    /// Un directorio.
    Directorio,
    /// Fuente, cabeceras, guiones.
    Codigo,
    /// Un contenedor comprimido.
    Comprimido,
    /// Imagen fija.
    Imagen,
    /// Audio o vídeo — lo que suele ocupar el rectángulo grande.
    Medios,
    /// Texto legible: documentos, notas, datos.
    Documento,
    /// Todo lo demás, incluido lo que no sabemos leer.
    Otro,
}

impl Clase {
    /// El papel del tema con el que se pinta.
    ///
    /// Roles y no colores crudos (ADR 0037): el tema manda, y un mapa cosido a
    /// `#ff8800` se ve igual de mal en los dos temas que el lector eligió.
    /// Ninguno de estos roles significa «fichero de tal clase» —no existe esa
    /// familia— así que se toman prestados por CONTRASTE, que es lo que un
    /// treemap necesita: rectángulos vecinos que se distinguen.
    #[must_use]
    pub fn role(self) -> Role {
        match self {
            Self::Directorio => Role::Info,
            Self::Codigo => Role::Match,
            Self::Comprimido => Role::Warning,
            Self::Imagen => Role::Badge,
            Self::Medios => Role::Selection,
            Self::Documento => Role::Regular,
            Self::Otro => Role::Muted,
        }
    }
}

/// La clase de un hijo, por su tipo y por su extensión.
///
/// La extensión se lee de los BYTES del nombre y se compara en ASCII
/// minúscula: no se decodifica el nombre para clasificarlo, porque un nombre
/// que no es UTF-8 tiene extensión igual (regla 1).
#[must_use]
pub fn clase_de(child: &DirUsageChild) -> Clase {
    if child.kind == EntryKind::Dir {
        return Clase::Directorio;
    }
    let bytes = child.name.as_bytes();
    let Some(punto) = bytes.iter().rposition(|b| *b == b'.') else {
        return Clase::Otro;
    };
    let ext: Vec<u8> = bytes[punto + 1..].to_ascii_lowercase();
    match ext.as_slice() {
        b"rs" | b"c" | b"h" | b"cpp" | b"hpp" | b"py" | b"js" | b"ts" | b"go" | b"java" | b"rb"
        | b"sh" | b"toml" | b"json" | b"yaml" | b"yml" => Clase::Codigo,
        b"zip" | b"gz" | b"bz2" | b"xz" | b"zst" | b"tar" | b"rar" | b"7z" => Clase::Comprimido,
        b"png" | b"jpg" | b"jpeg" | b"gif" | b"webp" | b"bmp" | b"svg" | b"ico" => Clase::Imagen,
        b"mp3" | b"flac" | b"ogg" | b"wav" | b"mp4" | b"mkv" | b"avi" | b"mov" | b"webm" => {
            Clase::Medios
        }
        b"txt" | b"md" | b"pdf" | b"doc" | b"docx" | b"odt" | b"csv" | b"html" => Clase::Documento,
        _ => Clase::Otro,
    }
}

/// Un rectángulo del reparto, en CELDAS del marco.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Columna de la esquina superior izquierda.
    pub x: u16,
    /// Fila de la esquina superior izquierda.
    pub y: u16,
    /// Anchura en celdas. Cero = no se pinta.
    pub w: u16,
    /// Altura en celdas. Cero = no se pinta.
    pub h: u16,
}

impl Rect {
    /// Cuántas celdas ocupa.
    #[must_use]
    pub fn celdas(self) -> u32 {
        u32::from(self.w) * u32::from(self.h)
    }
}

/// El reparto: un rectángulo por hijo, en el mismo orden que `pesos`.
///
/// Un hijo al que no le llega para una celda sale con `w` o `h` en cero — **no
/// se pinta, pero no desaparece**: su tamaño ya está contado en el total que
/// alguien enseñe al lado, y borrarlo de la lista haría que los rectángulos
/// mintieran sobre de qué se compone el directorio.
///
/// # Reparto EXACTO, por construcción
/// Las anchuras se reparten con un acumulador de resto en vez de redondeando
/// cada una por su cuenta: la última de cada tira toma lo que queda. Así los
/// rectángulos no se solapan ni dejan huecos, y no hay que comprobarlo después
/// — lo que se comprueba en los tests es que esta propiedad se cumple.
///
/// ```
/// use norte_frontend::treemap::{Rect, repartir};
/// let r = repartir(&[3, 1], Rect { x: 0, y: 0, w: 4, h: 1 });
/// assert_eq!(r.len(), 2);
/// // Cubren la franja entera, sin solaparse.
/// assert_eq!(r[0].w + r[1].w, 4);
/// ```
#[must_use]
pub fn repartir(pesos: &[u64], area: Rect) -> Vec<Rect> {
    let mut out = vec![
        Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0
        };
        pesos.len()
    ];
    if pesos.is_empty() || area.w == 0 || area.h == 0 {
        return out;
    }
    // Orden por tamaño descendente: es lo que hace que un treemap salga
    // legible, y el desempate por POSICIÓN mantiene el resultado estable entre
    // dos vistas de lo mismo.
    let mut orden: Vec<usize> = (0..pesos.len()).collect();
    orden.sort_by(|a, b| pesos[*b].cmp(&pesos[*a]).then(a.cmp(b)));

    let mut restante: u64 = pesos.iter().copied().fold(0, u64::saturating_add);
    let mut libre = area;
    let mut i = 0;
    while i < orden.len() && libre.w > 0 && libre.h > 0 && restante > 0 {
        // La tira se apoya en el lado CORTO, que es lo que mantiene los
        // rectángulos cuadrados en vez de convertirlos en tiras finas.
        let horizontal = libre.w <= libre.h;
        let largo = if horizontal { libre.w } else { libre.h };
        let grueso_total = if horizontal { libre.h } else { libre.w };

        // Cuántos hijos entran en esta tira: se crece mientras el peor aspecto
        // mejore (algoritmo squarified clásico).
        let mut fin = i;
        let mut suma: u64 = 0;
        let mut mejor = f64::INFINITY;
        while fin < orden.len() {
            let nueva = suma.saturating_add(pesos[orden[fin]]);
            if nueva == 0 {
                fin += 1;
                continue;
            }
            let peor = peor_aspecto(&orden[i..=fin], pesos, nueva, restante, largo, grueso_total);
            if peor > mejor {
                break;
            }
            mejor = peor;
            suma = nueva;
            fin += 1;
        }
        if fin == i {
            // Nada mensurable queda: el resto son ceros y se van sin pintar.
            break;
        }

        // El grosor de la tira, al menos una celda si lleva algo.
        let grueso = celdas_de(proporcion(suma, restante), grueso_total).clamp(1, grueso_total);

        // Y dentro, el largo se reparte con acumulador de resto.
        let mut usado: u16 = 0;
        for (n, idx) in orden[i..fin].iter().enumerate() {
            let ultimo = n == fin - i - 1;
            let trozo = if ultimo {
                largo - usado
            } else {
                celdas_de(proporcion(pesos[*idx], suma), largo).min(largo - usado)
            };
            out[*idx] = if horizontal {
                Rect {
                    x: libre.x + usado,
                    y: libre.y,
                    w: trozo,
                    h: grueso,
                }
            } else {
                Rect {
                    x: libre.x,
                    y: libre.y + usado,
                    w: grueso,
                    h: trozo,
                }
            };
            usado += trozo;
        }

        // Lo que queda libre, para la siguiente tira.
        if horizontal {
            libre.y += grueso;
            libre.h -= grueso;
        } else {
            libre.x += grueso;
            libre.w -= grueso;
        }
        restante = restante.saturating_sub(suma);
        i = fin;
    }
    out
}

/// Qué fracción del total es `parte`, para repartir área.
///
/// Un treemap reparte PROPORCIONES, y eso pide coma flotante. La pérdida de
/// precisión de `u64` a `f64` empieza por encima de 2^53 bytes: un directorio
/// de nueve petabytes perdería un byte de precisión al calcular cuántas celdas
/// le tocan. Es un DIBUJO — los tamaños que se enseñan salen del informe, que
/// sigue siendo entero.
#[expect(
    clippy::cast_precision_loss,
    reason = "reparto de área: el error empieza en 2^53 bytes y solo afecta a cuántas celdas se pintan, no al tamaño que se dice"
)]
fn proporcion(parte: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    parte as f64 / total as f64
}

/// Cuántas celdas de `total` le tocan a una proporción.
///
/// El resultado está acotado por construcción: `prop` sale de [`proporcion`] y
/// vive en `[0, 1]`, así que el producto cae en `[0, total]` y el `clamp` lo
/// deja ahí aunque un redondeo se pase por uno. Ni trunca lo que importa ni
/// puede salir negativo.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "producto de una proporción en [0,1] por un número de celdas, y además acotado: el resultado cabe en u16 y no es negativo"
)]
fn celdas_de(prop: f64, total: u16) -> u16 {
    let n = (prop * f64::from(total)).round();
    n.clamp(0.0, f64::from(total)) as u16
}

/// El peor aspecto (lado largo / lado corto) de una tira candidata.
fn peor_aspecto(
    tira: &[usize],
    pesos: &[u64],
    suma: u64,
    restante: u64,
    largo: u16,
    grueso_total: u16,
) -> f64 {
    let grueso = (proporcion(suma, restante) * f64::from(grueso_total)).max(1.0);
    let mut peor: f64 = 1.0;
    for idx in tira {
        let p = pesos[*idx];
        if p == 0 {
            continue;
        }
        let trozo = proporcion(p, suma) * f64::from(largo);
        if trozo <= 0.0 {
            continue;
        }
        let a = (grueso / trozo).max(trozo / grueso);
        peor = peor.max(a);
    }
    peor
}

/// El mapa entero: rectángulos pintados y pulsables.
///
/// `cols`/`rows` son las celdas del hueco. Un hueco de cero no pinta nada, que
/// es distinto de pintar un marco vacío.
///
/// # Las zonas se PRESUPUESTAN
/// Un rectángulo de varias filas se describe con un [`Hit`] por fila (es el
/// contrato del marco), así que un mapa alto se come el tope de zonas enseguida
/// — y [`StyledFrame::clamped`] las tira EN SILENCIO. Se reparten de mayor a
/// menor: si no caben todas, las que se quedan sin zona son las de los
/// rectángulos pequeños, no las del grande que alguien está intentando pulsar.
#[must_use]
pub fn squarify(children: &[DirUsageChild], cols: u16, rows: u16) -> StyledFrame {
    if cols == 0 || rows == 0 || children.is_empty() {
        return StyledFrame::default();
    }
    let pesos: Vec<u64> = children.iter().map(|c| c.bytes).collect();
    let rects = repartir(
        &pesos,
        Rect {
            x: 0,
            y: 0,
            w: cols,
            h: rows,
        },
    );

    // Rejilla de dueños: quién ocupa cada celda. Es lo que convierte
    // rectángulos en líneas sin que dos se pisen.
    let ancho = usize::from(cols);
    let alto = usize::from(rows);
    let mut duenyo: Vec<Option<usize>> = vec![None; ancho * alto];
    for (i, r) in rects.iter().enumerate() {
        for y in r.y..r.y.saturating_add(r.h) {
            for x in r.x..r.x.saturating_add(r.w) {
                let (xi, yi) = (usize::from(x), usize::from(y));
                if xi < ancho && yi < alto {
                    duenyo[yi * ancho + xi] = Some(i);
                }
            }
        }
    }

    let etiquetas: Vec<String> = children.iter().map(etiqueta_de).collect();
    let lines = pintar(&duenyo, children, &etiquetas, &rects, ancho, alto);
    let hits = zonas(children, &rects);
    StyledFrame::clamped(lines, hits)
}

/// La etiqueta de un hijo: su nombre enmascarado y lo que ocupa.
fn etiqueta_de(child: &DirUsageChild) -> String {
    let (nombre, _masked) = crate::display_name(child.name.as_bytes());
    let tam = crate::human_bytes_short(child.bytes);
    if child.partial {
        format!("{nombre} {CASI}{tam}")
    } else {
        format!("{nombre} {tam}")
    }
}

/// Las líneas del marco, una por fila de celdas.
fn pintar(
    duenyo: &[Option<usize>],
    children: &[DirUsageChild],
    etiquetas: &[String],
    rects: &[Rect],
    ancho: usize,
    alto: usize,
) -> Vec<Vec<StyledSpan>> {
    let mut lines = Vec::with_capacity(alto);
    for y in 0..alto {
        let mut fila: Vec<StyledSpan> = Vec::new();
        let mut x = 0;
        while x < ancho {
            let actual = duenyo[y * ancho + x];
            let mut fin = x;
            while fin < ancho && duenyo[y * ancho + fin] == actual {
                fin += 1;
            }
            let celdas = fin - x;
            let texto = match actual {
                // La etiqueta se pinta en la PRIMERA fila del rectángulo y solo
                // si cabe entera: media etiqueta nombra un fichero que no es.
                Some(i)
                    if usize::from(rects[i].y) == y && etiquetas[i].chars().count() <= celdas =>
                {
                    let mut t = etiquetas[i].clone();
                    t.push_str(&" ".repeat(celdas - etiquetas[i].chars().count()));
                    t
                }
                // Sin etiqueta que quepa, o sin dueño: celdas en blanco. Es el
                // MISMO resultado a propósito — el color ya dice de quién es el
                // rectángulo, y una relleno distinto sería ruido.
                _ => " ".repeat(celdas),
            };
            fila.push(StyledSpan {
                text: texto,
                role: actual.map(|i| clase_de(&children[i]).role()),
                fg: None,
                bg: None,
            });
            x = fin;
        }
        lines.push(fila);
    }
    lines
}

/// Las zonas pulsables, de mayor a menor y hasta el tope.
fn zonas(children: &[DirUsageChild], rects: &[Rect]) -> Vec<Hit> {
    let mut orden: Vec<usize> = (0..children.len()).collect();
    orden.sort_by(|a, b| {
        children[*b].bytes.cmp(&children[*a].bytes).then_with(|| {
            children[*a]
                .name
                .as_bytes()
                .cmp(children[*b].name.as_bytes())
        })
    });
    let mut hits = Vec::new();
    for i in orden {
        let r = rects[i];
        if r.w == 0 || r.h == 0 {
            continue;
        }
        for y in r.y..r.y.saturating_add(r.h) {
            if hits.len() >= MAX_HITS {
                return hits;
            }
            hits.push(Hit {
                row: y,
                col: r.x,
                width: r.w,
                command: COMANDO.to_owned(),
                // Forma WIRE: es la que se puede volver a convertir en el
                // nombre exacto. Lo que se PINTA está enmascarado y no sirve.
                arg: Some(children[i].name.to_wire()),
            });
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::Segment;

    fn hijo(name: &str, bytes: u64, kind: EntryKind) -> DirUsageChild {
        DirUsageChild {
            name: Segment::new(name.as_bytes().to_vec()).expect("segmento"),
            kind,
            bytes,
            entries: 1,
            partial: false,
        }
    }

    /// El reparto CUBRE el área y no se solapa: cada celda tiene un dueño y
    /// solo uno. Es la propiedad de la que cuelga todo lo demás — un mapa con
    /// huecos miente sobre el espacio libre, y uno con solapes hace que un clic
    /// abra el fichero de al lado.
    #[test]
    fn los_rectangulos_cubren_el_area_y_no_se_solapan() {
        let pesos = [40_u64, 30, 20, 10];
        let area = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 10,
        };
        let rects = repartir(&pesos, area);
        let mut celdas = [0_u8; 20 * 10];
        for r in &rects {
            for y in r.y..r.y + r.h {
                for x in r.x..r.x + r.w {
                    celdas[usize::from(y) * 20 + usize::from(x)] += 1;
                }
            }
        }
        assert!(
            celdas.iter().all(|c| *c == 1),
            "cada celda, exactamente un dueño"
        );
    }

    /// Un hijo que no llega a una celda NO se pinta, y tampoco desaparece de la
    /// lista: sigue teniendo su sitio en el reparto, con área cero.
    #[test]
    fn un_hijo_diminuto_no_se_pinta_pero_sigue_contando() {
        let pesos = [1_000_000_u64, 1];
        let rects = repartir(
            &pesos,
            Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 2,
            },
        );
        assert_eq!(rects.len(), 2, "el reparto no pierde hijos");
        assert!(rects[0].celdas() > 0, "el grande se pinta");
    }

    /// El `arg` de una zona es la forma WIRE del nombre, no lo que se pinta.
    ///
    /// Lo pintado pasa por el enmascarado y un nombre con bytes de control se
    /// ve como `�`: navegar con eso abriría otro fichero, o ninguno.
    #[test]
    fn la_zona_lleva_el_nombre_en_forma_wire() {
        let name = Segment::new(vec![0xFF, b'.', b'r', b's']).expect("segmento");
        let child = DirUsageChild {
            name: name.clone(),
            kind: EntryKind::File,
            bytes: 100,
            entries: 1,
            partial: false,
        };
        let frame = squarify(&[child], 20, 3);
        let hit = frame.hits.first().expect("una zona");
        assert_eq!(hit.command, "nav.enter");
        assert_eq!(hit.arg.as_deref(), Some(name.to_wire().as_str()));
        assert_eq!(
            Segment::parse_wire(hit.arg.as_deref().expect("arg")).expect("round trip"),
            name,
            "el arg vuelve a ser los bytes exactos"
        );
    }

    /// Lo pintado va ENMASCARADO, aunque el `arg` conserve los bytes.
    #[test]
    fn lo_pintado_esta_enmascarado() {
        let child = DirUsageChild {
            name: Segment::new(vec![0xFF, b'.', b'r', b's']).expect("segmento"),
            kind: EntryKind::File,
            bytes: 100,
            entries: 1,
            partial: false,
        };
        let frame = squarify(&[child], 20, 3);
        let pintado: String = frame
            .lines
            .iter()
            .flat_map(|l| l.iter().map(|s| s.text.clone()))
            .collect();
        assert!(pintado.contains('\u{FFFD}'), "el byte crudo no se pinta");
        assert!(!pintado.as_bytes().contains(&0xFF));
    }

    /// Un rectángulo incompleto se marca, y la marca va en la ETIQUETA: el
    /// color dice de qué clase es el fichero, y las dos cosas tienen que caber.
    #[test]
    fn un_hijo_parcial_se_marca_sin_perder_su_clase() {
        let mut child = hijo("fotos", 1000, EntryKind::Dir);
        child.partial = true;
        let frame = squarify(&[child], 30, 3);
        let pintado: String = frame
            .lines
            .iter()
            .flat_map(|l| l.iter().map(|s| s.text.clone()))
            .collect();
        assert!(pintado.contains(CASI), "dice que es una cota inferior");
        assert_eq!(
            frame.lines[0][0].role,
            Some(Role::Info),
            "y sigue pintándose como el directorio que es"
        );
    }

    /// Las zonas no pasan del tope, y las que sobreviven son las de los
    /// rectángulos GRANDES: `clamped` tira el exceso en silencio, así que
    /// quedarse sin zona tiene que tocarle al que nadie va a pulsar.
    #[test]
    fn las_zonas_se_presupuestan_de_mayor_a_menor() {
        let hijos: Vec<DirUsageChild> = (0..60_u64)
            .map(|i| hijo(&format!("f{i}"), (60 - i) * 1000, EntryKind::File))
            .collect();
        let frame = squarify(&hijos, 40, 30);
        assert!(frame.hits.len() <= MAX_HITS, "no se pasa del tope");
        let mayor = frame.hits.iter().any(|h| h.arg.as_deref() == Some("f0"));
        assert!(mayor, "el mayor conserva su zona");
    }

    /// La clase sale del tipo y de la extensión, leída en BYTES.
    #[test]
    fn la_clase_se_lee_de_los_bytes_del_nombre() {
        assert_eq!(clase_de(&hijo("x", 1, EntryKind::Dir)), Clase::Directorio);
        assert_eq!(
            clase_de(&hijo("main.RS", 1, EntryKind::File)),
            Clase::Codigo
        );
        assert_eq!(
            clase_de(&hijo("a.tar", 1, EntryKind::File)),
            Clase::Comprimido
        );
        assert_eq!(
            clase_de(&hijo("sin_extension", 1, EntryKind::File)),
            Clase::Otro
        );
    }

    /// Un hueco de cero no pinta nada — que no es lo mismo que pintar un marco
    /// vacío.
    #[test]
    fn un_hueco_de_cero_no_pinta_nada() {
        let frame = squarify(&[hijo("a", 10, EntryKind::File)], 0, 5);
        assert!(frame.lines.is_empty());
        assert!(frame.hits.is_empty());
    }
}
