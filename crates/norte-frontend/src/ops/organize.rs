//! The review of an ORGANIZE plan (phase 8 of the WOW program): the tree
//! that will result, for a human to read before saying yes.
//!
//! A rename plan is reviewed as a list of pairs because that is what it
//! is. An organize plan is not: what changes is the directory's SHAPE, and
//! a list of `a.pdf → invoices/2026/a.pdf` repeated forty times does not
//! let that shape be seen — not how many new folders appear, nor which
//! ones, nor what ends up inside each one. Hence this tree.
//!
//! The model belongs to both frontends. Each paints the lines its own way;
//! what CANNOT be decided twice is which folders are new and what hangs
//! from each one, because what the human believes will happen depends on
//! it.

use std::collections::BTreeMap;

use norte_proto::methods::OrganizeMove;

/// How many tree lines are shown at once.
///
/// It is the twin of [`crate::AI_RENAME_PAIR_LIMIT`] and is here for the
/// same reason: the window decides when the reader "has reached the end",
/// and that gates approving ([`crate::approval_ready`]). Two surfaces with
/// different windows would approve with a different amount read.
pub const ORGANIZE_LINE_LIMIT: usize = 10;

/// A review tree line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeLine {
    /// How much it is indented: 0 is a direct child of the plan's
    /// directory.
    pub depth: usize,
    /// What gets painted on that line. Already paintable; masked by
    /// whoever builds the tree, which is the one that knows where those
    /// bytes come from.
    pub text: String,
    /// What this line is.
    pub kind: TreeKind,
}

/// What a tree line represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeKind {
    /// A folder the plan is going to CREATE. It is the line that matters
    /// most: these are the ones that did not exist, and the ones undo will
    /// take away.
    NewDir,
    /// A folder that ALREADY exists and that the plan puts something into.
    ExistingDir,
    /// A file that gets moved there.
    Moved,
}

/// A plan's tree, in lines ready to paint.
///
/// `existing` is the names of the entries ALREADY in the directory, to
/// tell a new folder apart from one that was there. Without that list
/// everything would be painted as new, which is the comfortable lie: it
/// shows off a plan more spectacular than it is and hides that something
/// is going to land inside a folder the human already had.
#[must_use]
pub fn tree_lines(moves: &[OrganizeMove], existing: &[String]) -> Vec<TreeLine> {
    /// A tree node while it is being built.
    #[derive(Default)]
    struct Node {
        children: BTreeMap<String, Node>,
        files: Vec<String>,
    }

    /// Walks the already-built tree leaving one line per node, parents
    /// before children and folders before files.
    fn walk(
        node: &Node,
        depth: usize,
        existing_prefix: bool,
        existing: &[String],
        out: &mut Vec<TreeLine>,
    ) {
        for (name, child) in &node.children {
            // A folder is EXISTING only if it is at the root and was
            // already there. One hanging from a new folder cannot exist,
            // and saying it does would be promising that something is
            // kept when it is really being created.
            let exists = existing_prefix && depth == 0 && existing.iter().any(|e| e == name);
            out.push(TreeLine {
                depth,
                text: name.clone(),
                kind: if exists {
                    TreeKind::ExistingDir
                } else {
                    TreeKind::NewDir
                },
            });
            walk(child, depth + 1, exists, existing, out);
        }
        for f in &node.files {
            out.push(TreeLine {
                depth,
                text: f.clone(),
                kind: TreeKind::Moved,
            });
        }
    }

    let mut root = Node::default();
    for m in moves {
        let mut parts: Vec<&str> = m.proposed_rel.split('/').collect();
        // The last one is the file's name; the ones before it, folders.
        let Some(file) = parts.pop() else {
            continue;
        };
        let mut node = &mut root;
        for t in parts {
            node = node.children.entry(t.to_owned()).or_default();
        }
        node.files.push(file.to_owned());
    }

    let mut out = Vec::new();
    walk(&root, 0, true, existing, &mut out);
    out
}

/// How many NEW folders the plan creates, and how many files it moves.
///
/// It is the summary that goes ahead of the question: "this creates 3
/// folders and moves 12 files" is what a human needs to decide without
/// counting lines.
#[must_use]
pub fn resumen(lineas: &[TreeLine]) -> (usize, usize) {
    let folders = lineas.iter().filter(|l| l.kind == TreeKind::NewDir).count();
    let files = lineas.iter().filter(|l| l.kind == TreeKind::Moved).count();
    (folders, files)
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

    /// The tree groups by folder instead of repeating the whole path on
    /// every row: what changes is the directory's SHAPE, and that is what
    /// has to be readable.
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
        let painted: Vec<(usize, &str)> =
            lineas.iter().map(|l| (l.depth, l.text.as_str())).collect();
        assert_eq!(
            painted,
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

    /// A folder that ALREADY exists is marked as such. Painting everything
    /// as new is the comfortable lie: it shows off a plan more spectacular
    /// than it is and hides that something lands inside something that was
    /// already there.
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

    /// And a folder that hangs from a NEW one can never be existing, even
    /// if there is one with that name at the root: `nueva/facturas` is not
    /// `facturas`.
    #[test]
    fn una_carpeta_bajo_una_nueva_nunca_es_existente() {
        let lineas = tree_lines(
            &[mov("a.pdf", "nueva/facturas/a.pdf")],
            &["facturas".to_owned()],
        );
        let facturas = lineas
            .iter()
            .find(|l| l.text == "facturas")
            .expect("is there");
        assert_eq!(facturas.kind, TreeKind::NewDir);
    }

    /// The summary counts new folders and moved files, which is what goes
    /// ahead of the question.
    #[test]
    fn el_resumen_cuenta_lo_que_se_va_a_crear_y_lo_que_se_mueve() {
        let lineas = tree_lines(
            &[
                mov("a.pdf", "facturas/2026/a.pdf"),
                mov("b.txt", "notas/b.txt"),
            ],
            &[],
        );
        assert_eq!(resumen(&lineas), (3, 2), "facturas, 2026 and notas are new");
    }

    /// A destination WITHOUT a folder — a plain rename — is painted at the
    /// root.
    #[test]
    fn un_destino_sin_carpeta_va_en_la_raiz() {
        let lineas = tree_lines(&[mov("a.txt", "b.txt")], &[]);
        assert_eq!(lineas.len(), 1);
        assert_eq!((lineas[0].depth, lineas[0].kind), (0, TreeKind::Moved));
    }
}
