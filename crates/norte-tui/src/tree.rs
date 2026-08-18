//! El panel de árbol de directorios (#136): qué ramas hay abiertas y cuál es
//! la fila bajo el cursor.
//!
//! **Solo directorios.** Un árbol que enseñara ficheros sería un segundo
//! listado peor que el que ya hay al lado: lo que este panel contesta es
//! «cómo está organizado esto», y para eso los ficheros son ruido.
//!
//! **Perezoso, y por la misma razón que el listado local no trae tamaños
//! (#52):** desplegar una rama lista ESE directorio y nada más. Un árbol que
//! se leyera entero al abrirse tardaría minutos en un `$HOME` grande y
//! horas en un remoto.
//!
//! Aquí vive el ESTADO y la decisión de qué hace falta pedir; pedirlo es del
//! run loop, que es quien tiene el backend — el mismo reparto que el visor
//! acoplado y la hoja de atributos.

use std::collections::{BTreeMap, BTreeSet};

use norte_proto::VPath;

/// El kind que ocupa un hueco de árbol.
pub const KIND: &str = "tree";

/// Una fila pintable del árbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// El directorio de esta fila.
    pub path: VPath,
    /// Cuántos niveles por debajo de la raíz (la raíz es 0).
    pub depth: usize,
    /// Si está desplegada.
    pub expanded: bool,
    /// Si tiene hijos que enseñar. `None` = todavía no se ha mirado.
    ///
    /// Los tres estados son distintos para el lector: una rama que se puede
    /// abrir, una hoja que no, y una que aún no se sabe. Pintar «hoja» a algo
    /// que no se ha leído sería una respuesta inventada.
    pub children: Option<bool>,
}

/// El árbol de directorios de un panel.
#[derive(Debug, Default)]
pub struct Tree {
    /// Desde dónde cuelga.
    root: Option<VPath>,
    /// Los hijos DIRECTORIO de cada directorio ya listado.
    hijos: BTreeMap<VPath, Vec<VPath>>,
    /// Qué ramas están desplegadas.
    abiertas: BTreeSet<VPath>,
    /// Dónde está el cursor, por posición en las filas visibles.
    cursor: usize,
}

impl Tree {
    /// Ancla el árbol en `root` (y lo vacía si cambia de sitio).
    ///
    /// Cambiar de raíz TIRA lo leído: las ramas abiertas de otro árbol no
    /// dicen nada de éste, y conservarlas haría que el panel enseñara una
    /// mezcla de dos sitios.
    pub fn anchor(&mut self, root: VPath) {
        if self.root.as_ref() == Some(&root) {
            return;
        }
        self.root = Some(root);
        self.hijos.clear();
        self.abiertas.clear();
        self.cursor = 0;
    }

    /// Dónde está anclado.
    #[must_use]
    pub fn root(&self) -> Option<&VPath> {
        self.root.as_ref()
    }

    /// Mete los hijos DIRECTORIO de `dir` recién listados.
    ///
    /// El ORDEN que llega es el que se pinta: lo decide quien listó, con el
    /// mismo comparador que el listado de al lado. Reordenar aquí sería un
    /// segundo criterio que se separa del primero en cuanto alguien cambie uno.
    pub fn insert_children(&mut self, dir: VPath, hijos: Vec<VPath>) {
        self.hijos.insert(dir, hijos);
    }

    /// Qué directorio hace falta listar para pintar lo que está abierto, si
    /// alguno.
    ///
    /// Uno por vuelta, y el de más arriba primero: el run loop lo pide, lo
    /// mete con [`Self::insert_children`] y en la siguiente vuelta esta
    /// función dice el siguiente. Así una rama con mil hijos no bloquea el
    /// bucle ni se pide dos veces.
    #[must_use]
    pub fn wants(&self) -> Option<VPath> {
        let root = self.root.as_ref()?;
        if !self.hijos.contains_key(root) {
            return Some(root.clone());
        }
        self.rows()
            .into_iter()
            .find(|r| r.expanded && !self.hijos.contains_key(&r.path))
            .map(|r| r.path)
    }

    /// Las filas visibles, en orden de pintado.
    #[must_use]
    pub fn rows(&self) -> Vec<Row> {
        let Some(root) = self.root.clone() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        self.empuja(&root, 0, &mut out);
        out
    }

    fn empuja(&self, dir: &VPath, depth: usize, out: &mut Vec<Row>) {
        let hijos = self.hijos.get(dir);
        let expanded = self.abiertas.contains(dir) || depth == 0;
        out.push(Row {
            path: dir.clone(),
            depth,
            expanded,
            children: hijos.map(|h| !h.is_empty()),
        });
        if !expanded {
            return;
        }
        for h in hijos.into_iter().flatten() {
            self.empuja(h, depth + 1, out);
        }
    }

    /// La fila bajo el cursor, acotada a las que hay.
    #[must_use]
    pub fn cursor(&self) -> usize {
        let n = self.rows().len();
        self.cursor.min(n.saturating_sub(1))
    }

    /// El directorio bajo el cursor.
    #[must_use]
    pub fn selected(&self) -> Option<VPath> {
        let filas = self.rows();
        filas.get(self.cursor()).map(|r| r.path.clone())
    }

    /// Sube.
    pub const fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja.
    pub fn down(&mut self) {
        let n = self.rows().len();
        self.cursor = (self.cursor + 1).min(n.saturating_sub(1));
    }

    /// Despliega la rama bajo el cursor. La raíz siempre está desplegada.
    pub fn expand(&mut self) {
        if let Some(p) = self.selected() {
            self.abiertas.insert(p);
        }
    }

    /// Pliega la rama bajo el cursor.
    ///
    /// Lo LEÍDO se conserva: volver a abrirla no cuesta otro viaje, y el
    /// contenido de un directorio no cambia por plegarlo.
    pub fn collapse(&mut self) {
        if let Some(p) = self.selected() {
            self.abiertas.remove(&p);
        }
    }

    /// Pliega o despliega, según esté.
    pub fn toggle(&mut self) {
        let Some(p) = self.selected() else { return };
        if self.abiertas.contains(&p) {
            self.abiertas.remove(&p);
        } else {
            self.abiertas.insert(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire")
    }

    fn con_raiz() -> Tree {
        let mut t = Tree::default();
        t.anchor(vp("mem:///r"));
        t
    }

    /// Lo primero que hace falta es la raíz, y en cuanto está, lo siguiente es
    /// lo que el lector ha abierto: uno por vuelta, de arriba abajo.
    #[test]
    fn pide_la_raiz_y_luego_lo_que_se_abre() {
        let mut t = con_raiz();
        assert_eq!(t.wants(), Some(vp("mem:///r")));
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a"), vp("mem:///r/b")]);
        assert_eq!(t.wants(), None, "nada abierto, nada que pedir");
        t.down();
        t.expand();
        assert_eq!(t.wants(), Some(vp("mem:///r/a")));
    }

    /// Un directorio ya listado no se vuelve a pedir aunque se pliegue y se
    /// abra otra vez: su contenido no cambia por plegarlo.
    #[test]
    fn lo_leido_no_se_vuelve_a_pedir() {
        let mut t = con_raiz();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.down();
        t.expand();
        t.insert_children(vp("mem:///r/a"), Vec::new());
        assert_eq!(t.wants(), None);
        t.collapse();
        t.expand();
        assert_eq!(t.wants(), None, "ya se sabe qué hay dentro");
    }

    /// Las filas salen en orden de pintado, con su profundidad, y una rama
    /// plegada esconde a los suyos.
    #[test]
    fn las_filas_llevan_su_profundidad_y_lo_plegado_no_sale() {
        let mut t = con_raiz();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/x")]);
        assert_eq!(
            t.rows().len(),
            2,
            "la raíz y su hijo; el nieto está plegado"
        );
        t.down();
        t.expand();
        let filas = t.rows();
        assert_eq!(filas.len(), 3);
        assert_eq!(filas[2].depth, 2);
        assert_eq!(filas[2].path, vp("mem:///r/a/x"));
    }

    /// «Sin hijos» y «todavía no se ha mirado» son distintos, y el panel los
    /// pinta distinto: decir «hoja» a algo que no se ha leído es inventarse la
    /// respuesta.
    #[test]
    fn no_leido_y_sin_hijos_no_son_lo_mismo() {
        let mut t = con_raiz();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        assert_eq!(t.rows()[1].children, None, "de `a` no se sabe nada aún");
        t.insert_children(vp("mem:///r/a"), Vec::new());
        assert_eq!(
            t.rows()[1].children,
            Some(false),
            "y ahora se sabe: ninguno"
        );
    }

    /// Cambiar de raíz tira lo leído: las ramas abiertas de otro árbol no
    /// dicen nada de éste.
    #[test]
    fn cambiar_de_raiz_vacia_lo_leido() {
        let mut t = con_raiz();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.down();
        t.expand();
        t.anchor(vp("mem:///otro"));
        assert_eq!(t.rows().len(), 1, "solo la raíz nueva");
        assert_eq!(t.wants(), Some(vp("mem:///otro")));
        assert_eq!(t.cursor(), 0);
    }

    /// El cursor no se sale por ningún extremo, y sobre un árbol vacío no
    /// elige nada.
    #[test]
    fn el_cursor_se_queda_dentro() {
        let mut t = Tree::default();
        t.down();
        assert_eq!(t.cursor(), 0);
        assert_eq!(t.selected(), None);
        let mut t = con_raiz();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.down();
        t.down();
        t.down();
        assert_eq!(t.cursor(), 1);
        assert_eq!(t.selected(), Some(vp("mem:///r/a")));
        t.up();
        t.up();
        assert_eq!(t.cursor(), 0);
    }
}
