//! `org.norte.git-panel`: el panel oficial de estado del repositorio.
//!
//! El host abre la raíz del repositorio —el ancestro que contiene `.git`, que
//! es lo que declara el manifiesto como `location-root-marker`— y le pasa a
//! este guest un token opaco. Desde ahí todo lo que hace es LEER dos ficheros:
//! `.git/HEAD` y `.git/logs/HEAD`.
//!
//! Lo que NO hace: escribir, ejecutar `git`, ni saber dónde está nada. No hay
//! rutas en este código; hay un token y caminos relativos.
//!
//! # Por qué el reflog y no el log
//!
//! El log de una rama vive en la base de objetos: los objetos sueltos son
//! flujos zlib y los empaquetados piden el índice del pack, o sea un lector de
//! objetos dentro de un guest `no_std`. El reflog (`.git/logs/HEAD`) es texto
//! plano, una línea por movimiento, y contesta la pregunta que de verdad cuesta
//! recordar: de dónde vengo. Con lo barato se responde lo útil.
//!
//! # Lo que este panel NO puede decir
//!
//! - **Si hay cambios sin guardar.** Eso lo dice `git-status`, que compara el
//!   árbol contra el índice y ya existe como columna. Repetirlo aquí sería la
//!   misma cuenta hecha dos veces y dos respuestas que pueden discrepar.
//! - **Nada, fuera de un repositorio o sobre una ubicación que no sea
//!   `file://`.** La capacidad de ubicación no acuña token para sftp, s3, mem
//!   ni el interior de un archivo: el panel lo DICE en una línea, porque un
//!   hueco vacío no se distingue de uno roto.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

// La capa WASM solo existe cuando se compila COMO componente: los tests del
// host compilan el mismo crate sin ella, que es lo que permite probar las
// decisiones sin un runtime wasm por medio.
#[cfg(target_arch = "wasm32")]
mod guest;

/// El `kind` del panel que este plugin aporta; el mismo del manifiesto.
pub const PANEL_KIND: &str = "status";

/// Cuántos movimientos se enseñan si la configuración no dice otra cosa.
pub const MOVES_DEFAULT: usize = 5;

/// Lo que se puede leer del repositorio sin abrir la base de objetos.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Estado {
    /// La rama actual, o `None` con `HEAD` desprendido.
    pub branch: Option<String>,
    /// El commit al que apunta `HEAD`, abreviado a doce caracteres.
    pub commit: Option<String>,
    /// Los movimientos recientes, del más nuevo al más viejo.
    pub moves: Vec<Movimiento>,
}

/// Un movimiento del reflog: a dónde se fue y por qué.
#[derive(Debug, PartialEq, Eq)]
pub struct Movimiento {
    /// El commit de destino, abreviado.
    pub to: String,
    /// Lo que git escribió como motivo (`checkout: moving from a to b`).
    pub reason: String,
}

/// La rama que nombra `.git/HEAD`, si apunta a una.
///
/// `ref: refs/heads/<rama>` es el caso normal; un SHA a secas es `HEAD`
/// desprendido y no hay rama que nombrar. Se acepta cualquier `refs/…` y se
/// enseña el último tramo: una rama puede llamarse `feature/x/y`, y quedarse
/// con todo lo que sigue a `refs/heads/` conserva las barras que el lector
/// escribió.
///
/// ```
/// use git_panel::rama_de_head;
///
/// assert_eq!(rama_de_head(b"ref: refs/heads/main\n").as_deref(), Some("main"));
/// assert_eq!(rama_de_head(b"ref: refs/heads/feat/x\n").as_deref(), Some("feat/x"));
/// assert!(rama_de_head(b"9f1c2a0e\n").is_none());
/// ```
#[must_use]
pub fn rama_de_head(raw: &[u8]) -> Option<String> {
    let texto = core::str::from_utf8(raw).ok()?;
    let linea = texto.lines().next()?.trim();
    let referencia = linea.strip_prefix("ref:")?.trim();
    let rama = referencia.strip_prefix("refs/heads/")?;
    if rama.is_empty() {
        return None;
    }
    Some(rama.to_string())
}

/// Un hash abreviado a doce caracteres, que es lo que git enseña por defecto
/// en un repositorio grande y lo que cabe en un panel estrecho.
fn abreviar(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// El estado que describe `.git/logs/HEAD`: el commit de ahora y los últimos
/// movimientos.
///
/// El formato de una línea es `<antes> <después> <autor> <tiempo> <zona>\t<motivo>`.
/// Se lee del final hacia atrás porque lo último es lo de ahora, y se toman
/// como mucho `tope` movimientos: un panel no es un histórico.
///
/// Un reflog vacío —un repositorio recién creado, sin commits— no es un error:
/// devuelve un estado sin commit, y el panel lo dice.
#[must_use]
pub fn del_reflog(raw: &[u8], tope: usize) -> (Option<String>, Vec<Movimiento>) {
    let Ok(texto) = core::str::from_utf8(raw) else {
        return (None, Vec::new());
    };
    let lineas: Vec<&str> = texto.lines().filter(|l| !l.trim().is_empty()).collect();
    let commit = lineas.last().and_then(|l| {
        let mut campos = l.split(' ');
        let _antes = campos.next()?;
        let despues = campos.next()?;
        Some(abreviar(despues))
    });
    let mut moves = Vec::new();
    for linea in lineas.iter().rev().take(tope) {
        let Some((cabeza, motivo)) = linea.split_once('\t') else {
            continue;
        };
        let mut campos = cabeza.split(' ');
        let (Some(_antes), Some(despues)) = (campos.next(), campos.next()) else {
            continue;
        };
        moves.push(Movimiento {
            to: abreviar(despues),
            reason: motivo.trim().to_string(),
        });
    }
    (commit, moves)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFLOG: &[u8] = b"0000000000000000000000000000000000000000 1111111111111111111111111111111111111111 Oscar <o@x> 1700000000 +0200\tcommit (initial): primero\n\
1111111111111111111111111111111111111111 2222222222222222222222222222222222222222 Oscar <o@x> 1700000100 +0200\tcheckout: moving from main to feat/x\n";

    /// `HEAD` desprendido no inventa una rama.
    #[test]
    fn head_desprendido_no_tiene_rama() {
        assert!(rama_de_head(b"9f1c2a0e9f1c2a0e\n").is_none());
        assert!(rama_de_head(b"").is_none());
        assert!(rama_de_head(b"ref: refs/tags/v1\n").is_none());
    }

    /// El commit es el DESTINO de la última línea, no el origen.
    ///
    /// Es el error fácil de este formato: cada línea lleva los dos, y quedarse
    /// con el primero enseña el commit anterior como si fuera el actual.
    #[test]
    fn el_commit_es_el_destino_de_la_ultima_linea() {
        let (commit, _) = del_reflog(REFLOG, 5);
        assert_eq!(commit.as_deref(), Some("222222222222"));
    }

    /// Los movimientos van del más NUEVO al más viejo, y se acotan.
    #[test]
    fn los_movimientos_van_del_mas_nuevo_al_mas_viejo() {
        let (_, moves) = del_reflog(REFLOG, 5);
        assert_eq!(moves.len(), 2);
        assert_eq!(moves[0].reason, "checkout: moving from main to feat/x");
        assert_eq!(moves[1].reason, "commit (initial): primero");

        let (_, uno) = del_reflog(REFLOG, 1);
        assert_eq!(uno.len(), 1, "el tope manda");
        assert_eq!(uno[0].reason, "checkout: moving from main to feat/x");
    }

    /// Un reflog vacío no es un error: es un repositorio sin commits.
    #[test]
    fn un_reflog_vacio_no_es_un_error() {
        let (commit, moves) = del_reflog(b"", 5);
        assert!(commit.is_none());
        assert!(moves.is_empty());
    }

    /// Una línea sin tabulador no es un movimiento, y no tumba el resto.
    #[test]
    fn una_linea_rota_se_salta_sin_tumbar_las_demas() {
        let mut raw = Vec::from(&b"basura sin tabulador\n"[..]);
        raw.extend_from_slice(REFLOG);
        let (commit, moves) = del_reflog(&raw, 5);
        assert!(commit.is_some());
        assert_eq!(moves.len(), 2);
    }
}
