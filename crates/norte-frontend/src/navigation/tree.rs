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
    child_dirs: BTreeMap<VPath, Vec<VPath>>,
    /// Qué ramas están desplegadas.
    expanded_dirs: BTreeSet<VPath>,
    /// Dónde está el cursor, por posición en las filas visibles.
    cursor: usize,
    /// El directorio que [`Self::follow`] quiere dejar bajo el cursor y que
    /// TODAVÍA no es una fila.
    ///
    /// Revelar una rama profunda necesita listar cada nivel, y eso son varias
    /// vueltas del run loop: sin apuntar el objetivo, el cursor se quedaría
    /// en el último ancestro que sí existía cuando se pidió. Se liquida en
    /// [`Self::insert_children`], que es cuando aparecen filas nuevas.
    revealing: Option<VPath>,
    /// El último directorio al que [`Self::follow`] siguió.
    ///
    /// Sirve para NO volver a mover el cursor cuando el listado no ha cambiado
    /// de sitio: el embudo por el que se sigue pasa también en un refresco y
    /// en un click, y sin esto el cursor que el lector había movido a mano
    /// para mirar otra rama volvía de un salto por pulsar en el listado.
    seguido: Option<VPath>,
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
        self.child_dirs.clear();
        self.expanded_dirs.clear();
        self.cursor = 0;
        self.revealing = None;
        self.seguido = None;
    }

    /// Ancla el árbol CERCA de `dir` y lo revela: en `home` si `dir` cuelga
    /// de él, y si no en la raíz de su provider.
    ///
    /// Anclar en `dir` mismo —lo que se hacía— enseñaba una sola fila cuando
    /// el directorio no tiene subcarpetas, con la ruta entera por nombre: un
    /// árbol que no enseña nada de alrededor no sirve para moverse (captura
    /// del 2026-09-21). Colgarlo de más arriba y revelar la rama es lo que
    /// hace el explorador de VS Code.
    ///
    /// ```
    /// use norte_frontend::tree::Tree;
    /// use norte_proto::VPath;
    /// let vp = |w: &str| VPath::parse(w).unwrap();
    /// let casa = vp("file:///home/ana");
    /// let mut t = Tree::default();
    /// t.anchor_near(&vp("file:///home/ana/fotos/2026"), &casa);
    /// assert_eq!(t.root(), Some(&casa));
    /// assert_eq!(t.revealing(), Some(&vp("file:///home/ana/fotos/2026")));
    /// // Fuera de casa, la raíz del provider.
    /// let mut t = Tree::default();
    /// t.anchor_near(&vp("file:///etc/ssh"), &casa);
    /// assert_eq!(t.root(), Some(&vp("file:///")));
    /// // Y en otro provider, también su raíz.
    /// let mut t = Tree::default();
    /// t.anchor_near(&vp("mem:///r/a"), &casa);
    /// assert_eq!(t.root(), Some(&vp("mem:///")));
    /// ```
    pub fn anchor_near(&mut self, dir: &VPath, home: &VPath) {
        let cadena = Self::hasta_la_raiz(dir);
        let base = if cadena.contains(home) {
            home.clone()
        } else {
            cadena.last().cloned().unwrap_or_else(|| dir.clone())
        };
        self.anchor(base);
        self.follow(dir);
    }

    /// Una rama no se dejó leer.
    ///
    /// Se marca como leída y VACÍA —si no, se volvería a pedir en cada
    /// vuelta, un bucle de peticiones contra un directorio prohibido— salvo
    /// que sea la RAÍZ y el árbol esté siguiendo otro directorio: entonces
    /// el árbol se re-ancla en ese directorio. Pasa con [`Self::anchor_near`]
    /// en un servidor que no deja listar `/`, o en un sistema aislado: un
    /// árbol colgado de una raíz ilegible no enseñaría nada.
    ///
    /// ```
    /// use norte_frontend::tree::Tree;
    /// use norte_proto::VPath;
    /// let vp = |w: &str| VPath::parse(w).unwrap();
    /// let mut t = Tree::default();
    /// t.anchor_near(&vp("mem:///casa"), &vp("file:///home/ana"));
    /// assert_eq!(t.root(), Some(&vp("mem:///")));
    /// t.branch_unreadable(vp("mem:///"));
    /// assert_eq!(t.root(), Some(&vp("mem:///casa")), "vuelve al listado");
    /// // Otra rama ilegible solo se marca vacía.
    /// t.branch_unreadable(vp("mem:///casa/cerrada"));
    /// assert_eq!(t.root(), Some(&vp("mem:///casa")));
    /// ```
    pub fn branch_unreadable(&mut self, dir: VPath) {
        if self.root.as_ref() == Some(&dir)
            && let Some(sigue) = self.seguido.clone()
            && sigue != dir
        {
            self.anchor(sigue.clone());
            self.follow(&sigue);
            return;
        }
        self.insert_children(dir, Vec::new());
    }

    /// Sigue al listado de al lado: deja `dir` bajo el cursor SIN tirar lo que
    /// esté abierto. Dice si movió algo.
    ///
    /// Es la diferencia entre un árbol útil y uno que estorba. [`Self::anchor`]
    /// vacía —tiene que hacerlo, porque las ramas de otra raíz no dicen nada
    /// de ésta—, así que re-anclar en cada `cd` cerraría el árbol entero cada
    /// vez que alguien entra en una carpeta.
    ///
    /// **La regla es una: lo leído sigue valiendo mientras la raíz nueva sea un
    /// ANCESTRO de la vieja.** De ahí salen los tres casos:
    ///
    /// - `dir` cuelga de la raíz: se despliegan sus ancestros y el cursor va
    ///   ahí. La raíz no se mueve y nada se tira.
    /// - `dir` está por ENCIMA o al lado: la raíz sube al ancestro común más
    ///   hondo y **se conserva todo**, porque cada rama leída sigue colgando de
    ///   ahí. Es lo que hace que subir un nivel (`nav.up`) y alternar entre dos
    ///   paneles hermanos con `Tab` no cierren el árbol en cada pulsación —
    ///   antes lo hacían, y era la tecla más usada del programa.
    /// - Otro provider u otra máquina: entonces sí se ancla y se vacía. No hay
    ///   ancestro común, y un árbol que enseña un sitio junto a un listado que
    ///   enseña otro no responde a nada.
    ///
    /// `dir` mismo NO se despliega: quien navega ahí ya está viendo su
    /// contenido en el listado de al lado, y desplegarlo costaría un listado
    /// más por cada paso que dé el lector.
    ///
    /// Seguir DOS VECES al mismo directorio no vuelve a mover el cursor: por
    /// este embudo pasan también los refrescos y los clicks, y sin eso el
    /// cursor que el lector había movido a mano volvía de un salto cada vez
    /// que pulsaba en el listado.
    ///
    /// ```
    /// use norte_frontend::tree::Tree;
    /// use norte_proto::VPath;
    /// let vp = |w: &str| VPath::parse(w).unwrap();
    /// let mut t = Tree::default();
    /// t.anchor(vp("mem:///r/a"));
    /// t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);
    /// t.follow(&vp("mem:///r/a/y"));
    /// assert_eq!(t.selected(), Some(vp("mem:///r/a/y")));
    /// // Subir un nivel mueve la RAÍZ hacia arriba y conserva lo leído: hace
    /// // falta el listado de la raíz nueva —el mismo viaje que `anchor`
    /// // habría pedido— y con él vuelve todo lo que estaba abierto.
    /// t.follow(&vp("mem:///r"));
    /// assert_eq!(t.root(), Some(&vp("mem:///r")));
    /// t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
    /// assert!(t.rows().iter().any(|f| f.path == vp("mem:///r/a/y")));
    /// ```
    pub fn follow(&mut self, dir: &VPath) -> bool {
        let Some(root) = self.root.clone() else {
            self.anchor(dir.clone());
            self.seguido = Some(dir.clone());
            return true;
        };
        let cadena_dir = Self::hasta_la_raiz(dir);
        let cadena_root = Self::hasta_la_raiz(&root);
        // El ancestro común más HONDO: el primero de la cadena de `dir` —que va
        // de abajo arriba— que también está en la de la raíz. Sin ninguno son
        // dos providers distintos, y entonces nada de lo leído sirve.
        let Some(base) = cadena_dir.iter().find(|p| cadena_root.contains(p)).cloned() else {
            self.anchor(dir.clone());
            self.seguido = Some(dir.clone());
            return true;
        };
        let mismo_sitio = self.seguido.as_ref() == Some(dir);
        let antes = (self.root.clone(), self.expanded_dirs.len());
        if base != root {
            // La raíz SUBE, y no se vacía: la nueva es un ancestro de la
            // vieja, así que cada rama leída sigue colgando de ella. Lo que
            // hay que hacer es desplegar la cadena hasta la raíz anterior, o
            // lo que estaba abierto dejaría de verse — deja de estar a
            // profundidad cero, que es la única que se despliega sola.
            self.root = Some(base.clone());
            for p in cadena_root.iter().take_while(|p| **p != base) {
                self.expanded_dirs.insert(p.clone());
            }
        }
        // Los ANCESTROS de `dir` hasta la base; `dir` no.
        for p in cadena_dir.iter().take_while(|p| **p != base).skip(1) {
            self.expanded_dirs.insert(p.clone());
        }
        self.seguido = Some(dir.clone());
        let movio = antes != (self.root.clone(), self.expanded_dirs.len());
        if mismo_sitio && !movio {
            // El listado no se ha movido de sitio: el cursor del árbol es del
            // lector.
            return false;
        }
        self.revealing = Some(dir.clone());
        let antes_cursor = self.cursor;
        self.asentar_revelado();
        movio || self.cursor != antes_cursor
    }

    /// `dir` y todos sus ancestros, del más hondo a la raíz del provider.
    fn hasta_la_raiz(dir: &VPath) -> Vec<VPath> {
        let mut out = vec![dir.clone()];
        let mut actual = dir.clone();
        while let Some(padre) = actual.parent() {
            out.push(padre.clone());
            actual = padre;
        }
        out
    }

    /// El directorio que [`Self::follow`] pidió y que aún no tiene fila, si
    /// hay alguno.
    ///
    /// Lo que falta para que exista es listar sus ancestros, y eso ya lo pide
    /// [`Self::wants`] solo.
    ///
    /// ```
    /// use norte_frontend::tree::Tree;
    /// use norte_proto::VPath;
    /// let vp = |w: &str| VPath::parse(w).unwrap();
    /// let mut t = Tree::default();
    /// t.anchor(vp("mem:///r"));
    /// t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
    /// t.follow(&vp("mem:///r/a/y"));
    /// assert_eq!(t.revealing(), Some(&vp("mem:///r/a/y")));
    /// ```
    #[must_use]
    pub fn revealing(&self) -> Option<&VPath> {
        self.revealing.as_ref()
    }

    /// Pone el cursor en la rama que [`Self::follow`] pidió, si ya es una fila.
    ///
    /// Y suelta el objetivo cuando se sabe que la fila NO va a aparecer: el
    /// padre ya está listado y `dir` no está entre sus hijos. Pasa de verdad —
    /// el listado de una rama se recorta a un tope, así que el hijo que hace
    /// falta puede quedarse fuera— y sin esto el objetivo se quedaba puesto
    /// para siempre, pagando un barrido de filas en cada rama que llegara.
    fn asentar_revelado(&mut self) {
        let Some(objetivo) = self.revealing.clone() else {
            return;
        };
        if let Some(i) = self.rows().iter().position(|r| r.path == objetivo) {
            self.cursor = i;
            self.revealing = None;
            return;
        }
        if let Some(padre) = objetivo.parent()
            && let Some(hijos) = self.child_dirs.get(&padre)
            && !hijos.contains(&objetivo)
        {
            self.revealing = None;
        }
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
    pub fn insert_children(&mut self, dir: VPath, child_dirs: Vec<VPath>) {
        self.child_dirs.insert(dir, child_dirs);
        // Filas nuevas: puede que una de ellas sea la que se estaba revelando.
        self.asentar_revelado();
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
        if !self.child_dirs.contains_key(root) {
            return Some(root.clone());
        }
        self.rows()
            .into_iter()
            .find(|r| r.expanded && !self.child_dirs.contains_key(&r.path))
            .map(|r| r.path)
    }

    /// Las filas visibles, en orden de pintado.
    #[must_use]
    pub fn rows(&self) -> Vec<Row> {
        let Some(root) = self.root.clone() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        self.push_rows(&root, 0, &mut out);
        out
    }

    fn push_rows(&self, dir: &VPath, depth: usize, out: &mut Vec<Row>) {
        let child_dirs = self.child_dirs.get(dir);
        let expanded = self.expanded_dirs.contains(dir) || depth == 0;
        out.push(Row {
            path: dir.clone(),
            depth,
            expanded,
            children: child_dirs.map(|h| !h.is_empty()),
        });
        if !expanded {
            return;
        }
        for h in child_dirs.into_iter().flatten() {
            self.push_rows(h, depth + 1, out);
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
        let rows = self.rows();
        rows.get(self.cursor()).map(|r| r.path.clone())
    }

    /// Pone el cursor en una fila concreta, acotado a las que hay.
    ///
    /// Para el ratón: un click nombra una fila por su índice, y el índice
    /// puede venir de una foto anterior a que llegaran los hijos de una rama.
    /// Se acota en vez de rechazar porque quien rechaza es la GENERACIÓN, que
    /// es la que sabe si el árbol pintado es este.
    pub fn set_cursor(&mut self, row: usize) {
        let n = self.rows().len();
        self.cursor = row.min(n.saturating_sub(1));
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
            self.expanded_dirs.insert(p);
        }
    }

    /// Pliega la rama bajo el cursor.
    ///
    /// Lo LEÍDO se conserva: volver a abrirla no cuesta otro viaje, y el
    /// contenido de un directorio no cambia por plegarlo.
    pub fn collapse(&mut self) {
        if let Some(p) = self.selected() {
            self.expanded_dirs.remove(&p);
        }
    }

    /// Pliega o despliega, según esté.
    pub fn toggle(&mut self) {
        let Some(p) = self.selected() else { return };
        if self.expanded_dirs.contains(&p) {
            self.expanded_dirs.remove(&p);
        } else {
            self.expanded_dirs.insert(p);
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
        let rows = t.rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].depth, 2);
        assert_eq!(rows[2].path, vp("mem:///r/a/x"));
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

    /// Seguir al listado deja la rama bajo el cursor y NO cierra lo que
    /// hubiera abierto en otra parte del árbol: re-anclar en cada `cd` era
    /// justo lo que hacía inútil tener el panel abierto.
    #[test]
    fn seguir_revela_la_rama_y_conserva_lo_abierto() {
        let mut t = con_raiz();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a"), vp("mem:///r/b")]);
        // `b` queda abierta: es la hermana que no debe cerrarse.
        t.set_cursor(2);
        t.expand();
        t.insert_children(vp("mem:///r/b"), vec![vp("mem:///r/b/x")]);
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);

        t.follow(&vp("mem:///r/a/y"));

        let filas: Vec<VPath> = t.rows().into_iter().map(|r| r.path).collect();
        assert_eq!(
            filas,
            vec![
                vp("mem:///r"),
                vp("mem:///r/a"),
                vp("mem:///r/a/y"),
                vp("mem:///r/b"),
                vp("mem:///r/b/x"),
            ],
            "el ancestro se despliega y `b` sigue abierta"
        );
        assert_eq!(t.selected(), Some(vp("mem:///r/a/y")));
        assert_eq!(t.revealing(), None, "ya está revelada");
    }

    /// **Subir un nivel mueve la raíz ARRIBA y conserva lo leído.**
    ///
    /// `nav.up` es de las teclas más pulsadas de un gestor ortodoxo, y antes
    /// vaciaba el árbol entero: la raíz nueva no colgaba de la vieja, así que
    /// se anclaba. Pero al revés SÍ colgaba — todo lo leído sigue estando
    /// debajo de la raíz nueva—, y tirarlo era gratis y encima costaba otro
    /// listado.
    #[test]
    fn seguir_hacia_arriba_sube_la_raiz_sin_vaciar() {
        let mut t = Tree::default();
        t.anchor(vp("mem:///r/a"));
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);
        t.set_cursor(1);
        t.expand();
        t.insert_children(vp("mem:///r/a/y"), vec![vp("mem:///r/a/y/z")]);

        t.follow(&vp("mem:///r"));

        assert_eq!(t.root(), Some(&vp("mem:///r")));
        // La raíz nueva todavía no se ha listado, así que en esta vuelta solo
        // se ve ella: lo que importa es que NADA se tiró. `wants` pide su
        // listado —el mismo que `anchor` habría pedido— y con él vuelve todo.
        assert_eq!(t.wants(), Some(vp("mem:///r")));
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);

        let filas: Vec<VPath> = t.rows().into_iter().map(|r| r.path).collect();
        assert_eq!(
            filas,
            vec![
                vp("mem:///r"),
                vp("mem:///r/a"),
                vp("mem:///r/a/y"),
                vp("mem:///r/a/y/z"),
            ],
            "la raíz vieja queda desplegada y lo abierto debajo sigue abierto"
        );
        assert_eq!(t.selected(), Some(vp("mem:///r")), "y el cursor, arriba");
    }

    /// **Alternar entre dos paneles hermanos no vacía el árbol.**
    ///
    /// Es `Tab` con el árbol abierto, o sea la tecla más usada del programa:
    /// re-anclar en el destino cerraba el árbol en CADA pulsación. La raíz sube
    /// al ancestro común una vez y ahí se queda, así que la segunda vuelta ya
    /// no mueve nada.
    #[test]
    fn seguir_a_un_hermano_sube_al_ancestro_comun_una_vez() {
        let mut t = Tree::default();
        t.anchor(vp("mem:///r/a"));
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);
        t.set_cursor(1);
        t.expand();
        t.insert_children(vp("mem:///r/a/y"), Vec::new());

        t.follow(&vp("mem:///r/b"));
        assert_eq!(t.root(), Some(&vp("mem:///r")), "sube al ancestro común");
        // Un listado de la raíz nueva —el que `anchor` habría pedido igual— y
        // lo que estaba abierto vuelve entero.
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a"), vp("mem:///r/b")]);
        assert!(
            t.rows().iter().any(|f| f.path == vp("mem:///r/a/y")),
            "lo que estaba abierto sigue ahí: {:?}",
            t.rows()
        );
        assert_eq!(t.selected(), Some(vp("mem:///r/b")), "y el cursor, en `b`");

        // La vuelta ya no mueve la raíz ni pide nada: `a` cuelga de `r`.
        t.follow(&vp("mem:///r/a"));
        assert_eq!(t.root(), Some(&vp("mem:///r")));
        assert_eq!(t.wants(), None, "no hace falta otro viaje");
        assert!(t.rows().iter().any(|f| f.path == vp("mem:///r/a/y")));
        assert_eq!(t.selected(), Some(vp("mem:///r/a")));
    }

    /// Otro provider SÍ ancla y vacía: no hay ancestro común, y las ramas de
    /// otra máquina no dicen nada de ésta.
    #[test]
    fn seguir_a_otro_provider_reancla() {
        let mut t = con_raiz();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.follow(&vp("file:///otro/z"));
        assert_eq!(t.root(), Some(&vp("file:///otro/z")));
        assert_eq!(t.rows().len(), 1, "solo la raíz nueva");
        assert_eq!(t.revealing(), None, "anclar no deja nada pendiente");
    }

    /// Seguir DOS VECES al mismo sitio no vuelve a mover el cursor: por este
    /// embudo pasan los refrescos y los clicks, y el cursor que el lector movió
    /// a mano para mirar otra rama es suyo.
    #[test]
    fn seguir_dos_veces_al_mismo_sitio_no_toca_el_cursor() {
        let mut t = con_raiz();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a"), vp("mem:///r/b")]);
        assert!(t.follow(&vp("mem:///r/a")), "la primera vez sí mueve");
        assert_eq!(t.selected(), Some(vp("mem:///r/a")));

        t.set_cursor(2);
        assert!(!t.follow(&vp("mem:///r/a")), "la segunda no mueve nada");
        assert_eq!(
            t.selected(),
            Some(vp("mem:///r/b")),
            "el cursor se queda donde el lector lo dejó"
        );
    }

    /// Y un objetivo que NO va a aparecer se suelta en cuanto se sabe: el
    /// listado de una rama se recorta a un tope, así que el hijo que hacía
    /// falta puede quedarse fuera y el objetivo colgado para siempre.
    #[test]
    fn un_objetivo_que_no_va_a_llegar_se_suelta() {
        let mut t = con_raiz();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.follow(&vp("mem:///r/a/y"));
        assert_eq!(t.revealing(), Some(&vp("mem:///r/a/y")));

        // `a` se lista y `y` no está: recortado, borrado, o nunca existió.
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/otro")]);
        assert_eq!(t.revealing(), None, "ya se sabe que esa fila no viene");
    }

    /// Y una rama profunda que no se ha listado todavía se revela A PLAZOS:
    /// `wants` pide un nivel por vuelta y el cursor aterriza cuando la fila
    /// por fin existe, no en el ancestro más hondo que hubiera.
    #[test]
    fn seguir_una_rama_sin_listar_espera_a_que_llegue() {
        let mut t = con_raiz();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);

        t.follow(&vp("mem:///r/a/y"));
        assert_eq!(t.cursor(), 0, "todavía no hay fila que enseñar");
        assert_eq!(t.revealing(), Some(&vp("mem:///r/a/y")));
        assert_eq!(t.wants(), Some(vp("mem:///r/a")), "hace falta listar `a`");

        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);
        assert_eq!(t.selected(), Some(vp("mem:///r/a/y")));
        assert_eq!(t.revealing(), None);
    }

    /// Seguir a la raíz misma no cambia de sitio nada.
    #[test]
    fn seguir_a_la_propia_raiz_no_tira_nada() {
        let mut t = con_raiz();
        t.insert_children(vp("mem:///r"), vec![vp("mem:///r/a")]);
        t.set_cursor(1);
        t.expand();
        t.insert_children(vp("mem:///r/a"), vec![vp("mem:///r/a/y")]);

        t.follow(&vp("mem:///r"));

        assert_eq!(t.cursor(), 0);
        assert_eq!(t.rows().len(), 3, "`a` sigue desplegada");
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
