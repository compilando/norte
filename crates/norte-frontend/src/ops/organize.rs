//! La revisión de un plan de ORGANIZAR (fase 8 del programa WOW): el árbol
//! que va a quedar, para que un humano lo lea antes de decir que sí.
//!
//! Un plan de renombrar se revisa como una lista de parejas porque eso es lo
//! que es. Uno de organizar no: lo que cambia es la FORMA del directorio, y
//! una lista de `a.pdf → facturas/2026/a.pdf` repetida cuarenta veces no
//! deja ver esa forma — ni cuántas carpetas nuevas aparecen, ni cuáles, ni
//! qué acaba dentro de cada una. De ahí este árbol.
//!
//! El modelo es de los dos frontends. Cada uno pinta las líneas a su manera;
//! lo que NO puede decidirse dos veces es qué carpetas son nuevas y qué
//! cuelga de cada una, porque de eso depende lo que el humano cree que va a
//! pasar.

use std::collections::BTreeMap;

use norte_proto::methods::OrganizeMove;

/// Cuántas líneas del árbol se enseñan de una vez.
///
/// Es el gemelo de [`crate::AI_RENAME_PAIR_LIMIT`] y está aquí por la misma
/// razón: la ventana decide cuándo el lector «ha llegado al final», y eso
/// gatea el aprobar ([`crate::approval_ready`]). Dos superficies con ventanas
/// distintas aprobarían con distinta cantidad leída.
pub const ORGANIZE_LINE_LIMIT: usize = 10;

/// Una línea del árbol de revisión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeLine {
    /// Cuánto se sangra: 0 es hijo directo del directorio del plan.
    pub depth: usize,
    /// Lo que se pinta en esa línea. Ya pintable; lo enmascara quien
    /// construye el árbol, que es quien sabe de dónde vienen esos bytes.
    pub text: String,
    /// Qué es esta línea.
    pub kind: TreeKind,
}

/// Qué representa una línea del árbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeKind {
    /// Una carpeta que el plan va a CREAR. Es la línea que más importa: son
    /// las que no existían, y las que el undo se llevará.
    NewDir,
    /// Una carpeta que YA existe y a la que el plan mete algo.
    ExistingDir,
    /// Un fichero que se mueve hasta ahí.
    Moved,
}

/// El árbol de un plan, en líneas listas para pintar.
///
/// `existentes` son los nombres de las entradas que YA hay en el directorio,
/// para distinguir una carpeta nueva de una que estaba. Sin esa lista todo
/// se pintaría como nuevo, que es la mentira cómoda: enseña un plan más
/// espectacular de lo que es y esconde que algo va a caer dentro de una
/// carpeta que el humano ya tenía.
#[must_use]
pub fn tree_lines(moves: &[OrganizeMove], existentes: &[String]) -> Vec<TreeLine> {
    /// Un nodo del árbol mientras se construye.
    #[derive(Default)]
    struct Nodo {
        hijos: BTreeMap<String, Nodo>,
        ficheros: Vec<String>,
    }

    /// Recorre el árbol ya construido dejando una línea por nodo, padres
    /// antes que hijos y carpetas antes que ficheros.
    fn recorrer(
        nodo: &Nodo,
        depth: usize,
        prefijo_existente: bool,
        existentes: &[String],
        out: &mut Vec<TreeLine>,
    ) {
        for (nombre, hijo) in &nodo.hijos {
            // Una carpeta es EXISTENTE sólo si está en la raíz y ya estaba.
            // Una que cuelga de una carpeta nueva no puede existir, y decir
            // que sí sería prometer que algo se conserva cuando en realidad
            // se crea.
            let existe = prefijo_existente && depth == 0 && existentes.iter().any(|e| e == nombre);
            out.push(TreeLine {
                depth,
                text: nombre.clone(),
                kind: if existe {
                    TreeKind::ExistingDir
                } else {
                    TreeKind::NewDir
                },
            });
            recorrer(hijo, depth + 1, existe, existentes, out);
        }
        for f in &nodo.ficheros {
            out.push(TreeLine {
                depth,
                text: f.clone(),
                kind: TreeKind::Moved,
            });
        }
    }

    let mut raiz = Nodo::default();
    for m in moves {
        let mut trozos: Vec<&str> = m.proposed_rel.split('/').collect();
        // El último es el nombre del fichero; lo de delante, carpetas.
        let Some(fichero) = trozos.pop() else {
            continue;
        };
        let mut nodo = &mut raiz;
        for t in trozos {
            nodo = nodo.hijos.entry(t.to_owned()).or_default();
        }
        nodo.ficheros.push(fichero.to_owned());
    }

    let mut out = Vec::new();
    recorrer(&raiz, 0, true, existentes, &mut out);
    out
}

/// Cuántas carpetas NUEVAS crea el plan, y cuántos ficheros mueve.
///
/// Es el resumen que va delante de la pregunta: «esto crea 3 carpetas y
/// mueve 12 ficheros» es lo que un humano necesita para decidir sin contar
/// líneas.
#[must_use]
pub fn resumen(lineas: &[TreeLine]) -> (usize, usize) {
    let carpetas = lineas.iter().filter(|l| l.kind == TreeKind::NewDir).count();
    let ficheros = lineas.iter().filter(|l| l.kind == TreeKind::Moved).count();
    (carpetas, ficheros)
}

#[cfg(test)]
mod tests {
    use super::{TreeKind, resumen, tree_lines};
    use norte_proto::methods::OrganizeMove;

    fn mov(current: &str, rel: &str) -> OrganizeMove {
        OrganizeMove {
            current: current.to_owned(),
            proposed_rel: rel.to_owned(),
        }
    }

    /// El árbol agrupa por carpeta en vez de repetir la ruta entera en cada
    /// fila: lo que cambia es la FORMA del directorio, y eso es lo que hay
    /// que poder leer.
    #[test]
    fn el_arbol_agrupa_por_carpeta() {
        let lineas = tree_lines(
            &[
                mov("a.pdf", "facturas/2026/a.pdf"),
                mov("b.pdf", "facturas/2026/b.pdf"),
                mov("c.txt", "notas/c.txt"),
            ],
            &[],
        );
        let pintado: Vec<(usize, &str)> =
            lineas.iter().map(|l| (l.depth, l.text.as_str())).collect();
        assert_eq!(
            pintado,
            vec![
                (0, "facturas"),
                (1, "2026"),
                (2, "a.pdf"),
                (2, "b.pdf"),
                (0, "notas"),
                (1, "c.txt"),
            ]
        );
    }

    /// Una carpeta que YA existe se marca como tal. Pintarlo todo como nuevo
    /// es la mentira cómoda: enseña un plan más espectacular de lo que es y
    /// esconde que algo cae dentro de algo que ya estaba.
    #[test]
    fn una_carpeta_que_ya_existe_no_se_pinta_como_nueva() {
        let lineas = tree_lines(
            &[mov("a.pdf", "facturas/a.pdf"), mov("b.txt", "nueva/b.txt")],
            &["facturas".to_owned()],
        );
        assert_eq!(lineas[0].text, "facturas");
        assert_eq!(lineas[0].kind, TreeKind::ExistingDir);
        assert_eq!(lineas[2].text, "nueva");
        assert_eq!(lineas[2].kind, TreeKind::NewDir);
    }

    /// Y una carpeta que cuelga de una NUEVA no puede existir, aunque haya
    /// una con ese nombre en la raíz: `nueva/facturas` no es `facturas`.
    #[test]
    fn una_carpeta_bajo_una_nueva_nunca_es_existente() {
        let lineas = tree_lines(
            &[mov("a.pdf", "nueva/facturas/a.pdf")],
            &["facturas".to_owned()],
        );
        let facturas = lineas.iter().find(|l| l.text == "facturas").expect("está");
        assert_eq!(facturas.kind, TreeKind::NewDir);
    }

    /// El resumen cuenta carpetas nuevas y ficheros movidos, que es lo que va
    /// delante de la pregunta.
    #[test]
    fn el_resumen_cuenta_lo_que_se_va_a_crear_y_lo_que_se_mueve() {
        let lineas = tree_lines(
            &[
                mov("a.pdf", "facturas/2026/a.pdf"),
                mov("b.txt", "notas/b.txt"),
            ],
            &[],
        );
        assert_eq!(
            resumen(&lineas),
            (3, 2),
            "facturas, 2026 y notas son nuevas"
        );
    }

    /// Un destino SIN carpeta —un renombrado de paso— se pinta en la raíz.
    #[test]
    fn un_destino_sin_carpeta_va_en_la_raiz() {
        let lineas = tree_lines(&[mov("a.txt", "b.txt")], &[]);
        assert_eq!(lineas.len(), 1);
        assert_eq!((lineas[0].depth, lineas[0].kind), (0, TreeKind::Moved));
    }
}
