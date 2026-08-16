//! El matcher de `.gitignore`, lo justo para que la columna no sea ruido.
//!
//! Sin él, un árbol de Rust enseña `target/` y sus diez mil ficheros como «no
//! rastreado» y la columna deja de servir para nada. Es parsear texto y casar
//! globs — no toca objetos de git ni pretende ser `git check-ignore`.
//!
//! Qué cubre: comentarios y líneas vacías, negación (`!`), anclaje a la raíz
//! del fichero (`/` inicial o una barra en medio), solo-directorios (`/`
//! final), `*`, `?`, clases `[...]` y `**`. La última regla que casa gana, que
//! es la regla de git.

extern crate alloc;

use alloc::vec::Vec;

/// Una regla de un fichero de ignores.
#[derive(Debug, Clone)]
struct Rule {
    /// El patrón sin `!` ni barra final.
    pattern: Vec<u8>,
    /// `true` si empieza por `!`: lo que case DEJA de estar ignorado.
    negated: bool,
    /// `true` si acaba en `/`: solo casa directorios.
    dir_only: bool,
    /// `true` si el patrón lleva barra (o empieza por ella): casa contra la
    /// ruta ENTERA relativa al fichero, no contra el nombre suelto.
    anchored: bool,
    /// Dónde vivía el fichero de ignores, relativo a la raíz del repositorio
    /// y sin barra final. Vacío = la raíz.
    base: Vec<u8>,
}

/// Las reglas de uno o varios ficheros de ignores, en orden de precedencia
/// creciente (las de un `.gitignore` más profundo ganan a las de arriba).
#[derive(Debug, Default)]
pub struct Ignores {
    rules: Vec<Rule>,
}

impl Ignores {
    /// Añade las reglas de un fichero de ignores que vivía en `base`
    /// (relativo a la raíz del repositorio; vacío = la raíz).
    pub fn add_file(&mut self, base: &[u8], content: &[u8]) {
        for linea in content.split(|b| *b == b'\n') {
            let linea = trim(linea);
            if linea.is_empty() || linea[0] == b'#' {
                continue;
            }
            let (negated, resto) = match linea.first() {
                Some(b'!') => (true, &linea[1..]),
                _ => (false, linea),
            };
            let dir_only = resto.last() == Some(&b'/');
            let resto = if dir_only {
                &resto[..resto.len() - 1]
            } else {
                resto
            };
            let anchored = resto.first() == Some(&b'/')
                || resto[..resto.len().saturating_sub(1)].contains(&b'/');
            let pattern = resto.strip_prefix(b"/").unwrap_or(resto).to_vec();
            if pattern.is_empty() {
                continue;
            }
            self.rules.push(Rule {
                pattern,
                negated,
                dir_only,
                anchored,
                base: base.to_vec(),
            });
        }
    }

    /// ¿Está ignorada `path` (relativa a la raíz del repositorio)?
    ///
    /// `is_dir` importa: una regla `target/` ignora el directorio y no un
    /// fichero llamado igual.
    #[must_use]
    pub fn is_ignored(&self, path: &[u8], is_dir: bool) -> bool {
        let mut verdict = false;
        for rule in &self.rules {
            if rule.dir_only && !is_dir {
                continue;
            }
            let Some(rel) = strip_base(&rule.base, path) else {
                continue;
            };
            let casa = if rule.anchored {
                glob(&rule.pattern, rel)
            } else {
                // Sin anclar, la regla casa contra el nombre de CUALQUIER
                // componente: `*.tmp` tapa `a/b/c.tmp`, como en git.
                rel.split(|b| *b == b'/')
                    .any(|comp| glob(&rule.pattern, comp))
                    || glob(&rule.pattern, rel)
            };
            if casa {
                verdict = !rule.negated;
            }
        }
        verdict
    }

    /// `true` si algún fichero de ignores aportó reglas.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// `path` visto desde `base`, o `None` si no cuelga de él.
fn strip_base<'a>(base: &[u8], path: &'a [u8]) -> Option<&'a [u8]> {
    if base.is_empty() {
        return Some(path);
    }
    let rest = path.strip_prefix(base)?;
    rest.strip_prefix(b"/")
}

fn trim(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if first.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    // El espacio final SÍ se recorta (git lo hace salvo que vaya escapado);
    // el `\r` de un fichero con finales de línea de Windows también.
    while let [rest @ .., last] = s {
        if last.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    s
}

/// Glob de gitignore: `*` no cruza `/`, `**` sí, `?` es un byte, `[...]` es
/// una clase.
fn glob(pattern: &[u8], candidate: &[u8]) -> bool {
    match pattern.first() {
        None => candidate.is_empty(),
        Some(b'*') if pattern.get(1) == Some(&b'*') => {
            let resto = pattern[2..].strip_prefix(b"/").unwrap_or(&pattern[2..]);
            (0..=candidate.len()).any(|skip| glob(resto, &candidate[skip..]))
        }
        Some(b'*') => (0..=candidate.len())
            .take_while(|skip| !candidate[..*skip].contains(&b'/'))
            .any(|skip| glob(&pattern[1..], &candidate[skip..])),
        Some(b'?') => {
            !candidate.is_empty() && candidate[0] != b'/' && glob(&pattern[1..], &candidate[1..])
        }
        Some(b'[') => match class_end(pattern) {
            Some(end) => {
                !candidate.is_empty()
                    && class_matches(&pattern[1..end], candidate[0])
                    && glob(&pattern[end + 1..], &candidate[1..])
            }
            None => literal(pattern, candidate),
        },
        Some(_) => literal(pattern, candidate),
    }
}

fn literal(pattern: &[u8], candidate: &[u8]) -> bool {
    match (pattern.first(), candidate.first()) {
        (Some(p), Some(c)) if p == c => glob(&pattern[1..], &candidate[1..]),
        _ => false,
    }
}

fn class_end(pattern: &[u8]) -> Option<usize> {
    let start = if pattern.get(1) == Some(&b'!') { 2 } else { 1 };
    let start = if pattern.get(start) == Some(&b']') {
        start + 1
    } else {
        start
    };
    pattern[start..]
        .iter()
        .position(|b| *b == b']')
        .map(|at| at + start)
}

fn class_matches(class: &[u8], byte: u8) -> bool {
    let (negate, body) = match class.first() {
        Some(b'!') => (true, &class[1..]),
        _ => (false, class),
    };
    let mut hit = false;
    let mut i = 0;
    while i < body.len() {
        if i + 2 < body.len() && body[i + 1] == b'-' {
            if (body[i]..=body[i + 2]).contains(&byte) {
                hit = true;
            }
            i += 3;
        } else {
            if body[i] == byte {
                hit = true;
            }
            i += 1;
        }
    }
    hit != negate
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignores(content: &[u8]) -> Ignores {
        let mut i = Ignores::default();
        i.add_file(b"", content);
        i
    }

    #[test]
    fn un_directorio_con_barra_final_solo_tapa_directorios() {
        let i = ignores(b"target/\n");
        assert!(i.is_ignored(b"target", true));
        assert!(
            !i.is_ignored(b"target", false),
            "un FICHERO llamado igual, no"
        );
        assert!(
            i.is_ignored(b"a/target", true),
            "sin anclar, a cualquier nivel"
        );
    }

    #[test]
    fn una_extension_tapa_a_cualquier_profundidad() {
        let i = ignores(b"*.tmp\n");
        assert!(i.is_ignored(b"basura.tmp", false));
        assert!(i.is_ignored(b"a/b/basura.tmp", false));
        assert!(!i.is_ignored(b"basura.txt", false));
    }

    #[test]
    fn una_barra_inicial_ancla_a_la_raiz() {
        let i = ignores(b"/build\n");
        assert!(i.is_ignored(b"build", true));
        assert!(
            !i.is_ignored(b"sub/build", true),
            "anclado: solo en la raíz"
        );
    }

    #[test]
    fn la_negacion_gana_si_va_despues() {
        let i = ignores(b"*.log\n!importante.log\n");
        assert!(i.is_ignored(b"ruido.log", false));
        assert!(
            !i.is_ignored(b"importante.log", false),
            "la última que casa manda"
        );
    }

    #[test]
    fn los_comentarios_y_las_lineas_vacias_no_son_reglas() {
        let i = ignores(b"# esto es un comentario\n\n   \n*.o\n");
        assert!(i.is_ignored(b"a.o", false));
        assert!(!i.is_ignored(b"# esto es un comentario", false));
    }

    #[test]
    fn un_gitignore_mas_profundo_gana_al_de_arriba() {
        let mut i = Ignores::default();
        i.add_file(b"", b"*.log\n");
        i.add_file(b"sub", b"!guardado.log\n");
        assert!(i.is_ignored(b"raiz.log", false));
        assert!(!i.is_ignored(b"sub/guardado.log", false));
        assert!(i.is_ignored(b"otro/guardado.log", false), "solo bajo `sub`");
    }

    #[test]
    fn doble_asterisco_cruza_directorios_y_uno_solo_no() {
        let i = ignores(b"docs/**/borrador.md\n");
        assert!(i.is_ignored(b"docs/a/b/borrador.md", false));
        assert!(i.is_ignored(b"docs/borrador.md", false));
        let j = ignores(b"docs/*/borrador.md\n");
        assert!(j.is_ignored(b"docs/a/borrador.md", false));
        assert!(!j.is_ignored(b"docs/a/b/borrador.md", false));
    }

    #[test]
    fn un_nombre_que_no_es_utf8_se_casa_por_bytes() {
        let i = ignores(b"cp437-\xa4\xa5.txt\n");
        assert!(i.is_ignored(b"cp437-\xa4\xa5.txt", false));
        assert!(!i.is_ignored(b"cp437-.txt", false));
    }
}
