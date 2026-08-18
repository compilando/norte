//! El selector de disposiciones: qué filas hay y qué se ve de cada una.
//!
//! Vive aquí y no en un frontend por la regla 7 —la lógica no va en la TUI— y
//! porque la GUI necesitará el mismo selector con otro pintor.

use crate::layout::{KindRegistry, Node, Rect, SlotId, presets, resolve};

/// Una fila del selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// El nombre con el que se carga.
    pub name: String,
    /// De fábrica, o de `layouts/<nombre>.toml` en el directorio de config.
    pub factory: bool,
    /// Si el nombre coincide con un preset de KEYMAP.
    ///
    /// El selector lo AVISA, porque elegir esta disposición no cambia ni una
    /// tecla: son dos ajustes distintos que comparten nombre, y sin la línea
    /// la coincidencia es una trampa en vez de una comodidad.
    pub shares_keymap_name: bool,
}

/// El selector de disposiciones.
#[derive(Debug)]
pub struct LayoutPicker {
    rows: Vec<Row>,
    cursor: usize,
}

impl LayoutPicker {
    /// Abre el selector con los cinco de fábrica y los nombres de usuario que
    /// se le pasen.
    ///
    /// Los ficheros los lista quien tiene el disco delante: este crate no lee
    /// directorios.
    #[must_use]
    pub fn open(user: &[String]) -> Self {
        let fila = |name: &str, factory: bool| Row {
            name: name.to_owned(),
            factory,
            shares_keymap_name: crate::keymap::presets::NAMES.contains(&name),
        };
        let mut rows: Vec<Row> = presets::NAMES.iter().map(|n| fila(n, true)).collect();
        // Un fichero del usuario que se llama como uno de fábrica NO se
        // duplica: gana el del usuario, que es lo que hacen todas las demás
        // capas de configuración.
        for n in user {
            if let Some(r) = rows.iter_mut().find(|r| &r.name == n) {
                r.factory = false;
            } else {
                rows.push(fila(n, false));
            }
        }
        Self { rows, cursor: 0 }
    }

    /// Las filas, en orden.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Dónde está el cursor, acotado a las filas que hay.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor.min(self.rows.len().saturating_sub(1))
    }

    /// Sube.
    pub const fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja.
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    /// El nombre de la fila resaltada.
    #[must_use]
    pub fn chosen(&self) -> Option<&str> {
        self.rows.get(self.cursor()).map(|r| r.name.as_str())
    }
}

/// El área NOMINAL sobre la que se reparte una previsualización: un terminal
/// normal. La miniatura se ESCALA desde aquí en vez de repartir directamente
/// en su caja, porque el reparto respeta los mínimos de cada kind y en veinte
/// columnas la mitad de los paneles se colapsarían — la miniatura enseñaría
/// entonces una pantalla que nadie va a ver.
const NOMINAL: (u16, u16) = (80, 24);

/// La previsualización de un árbol: cajas dibujadas a partir de su REPARTO,
/// una cadena por fila de celdas de un área de `w`×`h`.
///
/// Del reparto y no de un dibujo guardado al lado del fichero: un dibujo
/// guardado empieza a mentir en cuanto alguien toca los tamaños, y quien lo
/// mira no tiene forma de saber cuál de los dos es la pantalla de verdad.
///
/// ```
/// use norte_frontend::layout::{KindRegistry, presets};
/// use norte_frontend::layout_picker::preview;
///
/// let reg = KindRegistry::builtin();
/// let filas = preview(&presets::tree("simple").expect("de fábrica"), 20, 8, &reg);
/// assert_eq!(filas.len(), 8);
/// assert!(filas.iter().all(|f| f.chars().count() == 20));
/// ```
#[must_use]
pub fn preview(tree: &Node, w: u16, h: u16, decls: &KindRegistry) -> Vec<String> {
    // La franja de tareas es `Auto` y en reposo mide cero, así que en una
    // previsualización desaparecería. Se le da su mínimo: lo que se enseña es
    // la FORMA de la pantalla, no la carga de trabajo del momento.
    let natural = |id: SlotId| tree.kind_of(id).map_or((0, 1), |k| (0, decls.min_of(k).1));
    let sustituido = tree.substitute_auto(&natural);
    let res = resolve(Rect::new(0, 0, NOMINAL.0, NOMINAL.1), &sustituido, decls);
    let (wu, hu) = (w as usize, h as usize);
    let mut lienzo = vec![vec![' '; wu]; hu];
    // Escala de celdas nominales a celdas de la miniatura, redondeando hacia
    // el borde más cercano y garantizando que ninguna caja desaparece: una
    // caja de una celda sigue diciendo que ese panel está ahí.
    let escala = |v: u16, de: u16, a: u16| (u32::from(v) * u32::from(a) / u32::from(de)) as usize;
    for (id, r) in &res.placements {
        let inicial = sustituido
            .kind_of(*id)
            .and_then(|k| k.as_str().chars().next())
            .unwrap_or('?');
        let x0 = escala(r.x, NOMINAL.0, w).min(wu.saturating_sub(1));
        let y0 = escala(r.y, NOMINAL.1, h).min(hu.saturating_sub(1));
        let x1 = escala(r.x.saturating_add(r.width), NOMINAL.0, w)
            .max(x0 + 1)
            .min(wu);
        let y1 = escala(r.y.saturating_add(r.height), NOMINAL.1, h)
            .max(y0 + 1)
            .min(hu);
        for (y, fila) in lienzo.iter_mut().enumerate().take(y1).skip(y0) {
            for (x, celda) in fila.iter_mut().enumerate().take(x1).skip(x0) {
                // El marco solo se dibuja si queda hueco para algo dentro. En
                // una caja de una o dos celdas, el marco SERÍA la caja entera
                // y la miniatura perdería la letra que dice qué panel es.
                let marco_h = x1 - x0 >= 3 && (x == x0 || x + 1 == x1);
                let marco_v = y1 - y0 >= 3 && (y == y0 || y + 1 == y1);
                *celda = if marco_h || marco_v { '·' } else { inicial };
            }
        }
    }
    lienzo
        .into_iter()
        .map(|f| f.into_iter().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn los_cinco_de_fabrica_salen_en_orden() {
        let p = LayoutPicker::open(&[]);
        let nombres: Vec<&str> = p.rows().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(nombres, presets::NAMES);
        assert!(p.rows().iter().all(|r| r.factory));
    }

    /// `krusader` es TAMBIÉN un preset de keymap, y elegir la disposición no
    /// cambia ni una tecla. La fila lo dice; si esto se rompe, el aviso
    /// desaparece y la coincidencia de nombres pasa a ser una trampa.
    #[test]
    fn la_fila_avisa_cuando_el_nombre_es_tambien_de_keymap() {
        let p = LayoutPicker::open(&[]);
        let f = |n: &str| {
            p.rows()
                .iter()
                .find(|r| r.name == n)
                .expect(n)
                .shares_keymap_name
        };
        assert!(f("krusader"));
        assert!(f("orthodox"));
        assert!(!f("explorer"));
    }

    /// Un fichero del usuario con nombre de fábrica no aparece dos veces.
    #[test]
    fn un_layout_de_usuario_con_nombre_de_fabrica_no_se_duplica() {
        let p = LayoutPicker::open(&["simple".to_owned(), "mio".to_owned()]);
        assert_eq!(p.rows().len(), presets::NAMES.len() + 1);
        assert!(
            !p.rows()
                .iter()
                .find(|r| r.name == "simple")
                .expect("simple")
                .factory
        );
        assert_eq!(p.rows().last().expect("mio").name, "mio");
    }

    #[test]
    fn el_cursor_no_se_sale() {
        let mut p = LayoutPicker::open(&[]);
        for _ in 0..20 {
            p.down();
        }
        assert_eq!(p.chosen(), Some("full"));
        for _ in 0..20 {
            p.up();
        }
        assert_eq!(p.chosen(), Some("orthodox"));
    }

    /// La vista previa sale del reparto: `simple` tiene UN listado y
    /// `orthodox` dos, así que no se pueden pintar igual.
    #[test]
    fn la_vista_previa_distingue_una_disposicion_de_otra() {
        let reg = KindRegistry::builtin();
        let uno = preview(&presets::tree("simple").expect("s"), 20, 8, &reg);
        let dos = preview(&presets::tree("orthodox").expect("o"), 20, 8, &reg);
        assert_ne!(uno, dos);
    }

    /// Ninguna previsualización se sale de su caja, ni con un área absurda.
    #[test]
    fn la_vista_previa_respeta_su_area() {
        let reg = KindRegistry::builtin();
        for name in presets::NAMES {
            let arbol = presets::tree(name).expect(name);
            for (w, h) in [(20_u16, 8_u16), (1, 1), (3, 2), (80, 24)] {
                let filas = preview(&arbol, w, h, &reg);
                assert_eq!(filas.len(), h as usize, "{name} {w}x{h}");
                assert!(
                    filas.iter().all(|f| f.chars().count() == w as usize),
                    "{name} {w}x{h}"
                );
            }
        }
    }

    /// La franja de tareas se ve en la previsualización aunque en reposo mida
    /// cero: lo que se enseña es la forma de la pantalla, no la carga de
    /// trabajo del momento.
    #[test]
    fn la_franja_de_tareas_se_ve_en_la_vista_previa() {
        let reg = KindRegistry::builtin();
        let filas = preview(&presets::tree("orthodox").expect("o"), 20, 10, &reg);
        assert!(
            filas.iter().any(|f| f.contains('t')),
            "la franja `tasks` no aparece: {filas:?}"
        );
    }
}
