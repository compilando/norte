//! La mitad DERECHA de la barra de estado: elementos informativos que el
//! lector elige (`[ui] status_items`) y que se pulsan (ADR 0132).
//!
//! La mitad izquierda no pasa por aquí y no se configura: es donde van los
//! mensajes, las esperas y los AVISOS (listado incompleto, nombres
//! reinterpretados, marcas podadas), y un aviso que una configuración pudiera
//! quitar dejaría de ser un aviso.
//!
//! Qué dice cada elemento, con qué prioridad cede y qué comando corre se
//! decide AQUÍ, una vez, para la TUI y para la ventana (ADR 0077). Los
//! frontends reúnen los hechos ([`StatusInput`]) y pintan.

use norte_config::{StatusItem, StatusItems};
use norte_i18n::{Lang, t_in, ta_in};

use crate::sort::{SortColumn, SortDir, SortSpec};

/// Los hechos del momento, del pane con el teclado y del programa.
///
/// No es `Copy` desde que el orden puede nombrar un atributo (ADR 0144).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusInput {
    /// `(cursor + 1, total)`, o `None` con un filtro activo: con el filtro
    /// la posición real no es la que se ve, y un `3/120` engañaría.
    pub position: Option<(usize, usize)>,
    /// Entradas marcadas.
    pub marked: usize,
    /// Lo que pesan las marcadas que declaran tamaño.
    pub marked_bytes: u64,
    /// Cuántas de las marcadas son directorios.
    pub marked_dirs: usize,
    /// El orden del listado.
    pub sort: SortSpec,
    /// La reinterpretación de nombres del pane; `None` = los bytes tal cual,
    /// leídos como UTF-8.
    pub encoding: Option<norte_encoding::NameEncoding>,
    /// Tareas en marcha.
    pub tasks: usize,
    /// Avisos que caducaron sin leerse.
    pub notices: u32,
}

impl StatusInput {
    /// Los hechos de `pane` más los del programa. UNA regla para los dos
    /// frontends: con el FILTRO activo no hay posición (review MINOR-2 T4:
    /// la selección no es el cursor real, y el pie ya da el `n/m` honesto).
    #[must_use]
    pub fn from_pane(pane: &crate::PaneState, tasks: usize, notices: u32) -> Self {
        let total = pane.entries().len();
        let pos = if total == 0 { 0 } else { pane.cursor() + 1 };
        Self {
            position: pane.quick_visible().is_none().then_some((pos, total)),
            marked: pane.marks_len(),
            marked_bytes: pane.marked_bytes(),
            marked_dirs: pane.marked_dirs(),
            sort: pane.sort(),
            encoding: pane.name_encoding(),
            tasks,
            notices,
        }
    }
}

/// Un elemento, ya redactado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusItemView {
    /// El id estable (`position`, `marks`…, o `plugin:<plugin>/<columna>`
    /// para el de un plugin): el de `norte.toml`, y el que vuelve con un
    /// clic.
    pub id: String,
    /// El texto, en el idioma pedido.
    pub text: String,
    /// Qué es y, si se pulsa, qué hace.
    pub tooltip: String,
    /// El comando que corre un clic, del catálogo; `None` = no se pulsa.
    pub command: Option<&'static str>,
    /// Mayor = cede más tarde cuando no caben todos.
    pub priority: u8,
}

/// Los elementos de `list`, en su orden, sin los que ahora no tienen nada
/// que decir (sin marcas no hay «0 marcadas»; un elemento vacío no ocupa
/// sitio ni separador).
#[must_use]
pub fn items(input: &StatusInput, list: StatusItems, lang: Lang) -> Vec<StatusItemView> {
    list.iter().filter_map(|i| item(input, i, lang)).collect()
}

fn item(input: &StatusInput, which: StatusItem, lang: Lang) -> Option<StatusItemView> {
    let (text, command, priority) = match which {
        StatusItem::Position => {
            let (pos, total) = input.position?;
            (format!("{pos}/{total}"), None, 60)
        }
        StatusItem::Marks => {
            let s = crate::notes::marked(input.marked, input.marked_bytes, input.marked_dirs, lang);
            if s.is_empty() {
                return None;
            }
            (s, None, 70)
        }
        StatusItem::Sort => (sort_text(&input.sort, lang), Some("pane.sort-menu"), 30),
        StatusItem::Encoding => (
            input
                .encoding
                .map_or_else(|| "UTF-8".to_owned(), |e| e.label().to_uppercase()),
            Some("pane.names-encoding"),
            40,
        ),
        StatusItem::Tasks => {
            if input.tasks == 0 {
                return None;
            }
            (format!("⟳ {}", input.tasks), Some("layout.processes"), 80)
        }
        StatusItem::Notices => {
            if input.notices == 0 {
                return None;
            }
            (format!("!{}", input.notices), Some("layout.log"), 90)
        }
    };
    let id = which.as_str();
    let tooltip = ta_in(
        lang,
        &format!("status-item-{id}-tip"),
        &[("n", &tip_count(input, which))],
    );
    Some(StatusItemView {
        id: id.to_owned(),
        text,
        tooltip,
        command,
        priority,
    })
}

/// Lo más que ocupa el texto de un elemento de plugin, en celdas: el valor
/// es de un tercero, y uno largo no se puede comer la barra.
pub const PLUGIN_ITEM_MAX_CELLS: usize = 32;

/// Los elementos que aportan los PLUGINS (ADR 0137): el valor de cada
/// columna `(plugin, columna)` para la entrada bajo el cursor, en su orden.
///
/// Van a la IZQUIERDA de la mitad derecha —donde VS Code pone la rama— y son
/// los primeros en ceder: lo del programa manda sobre lo de un tercero. No se
/// pulsan: un plugin de columnas pinta, no conduce el gestor.
///
/// El texto sale de [`crate::PaneState::plugin_cell`], que lo sirve
/// re-enmascarado, y además se acota a [`PLUGIN_ITEM_MAX_CELLS`]. Sin
/// valor —plugin sin consentir, columna que no declara, entrada sin dato— el
/// elemento no sale.
#[must_use]
pub fn plugin_items(
    pane: &crate::PaneState,
    pairs: &[(String, String)],
    lang: Lang,
) -> Vec<StatusItemView> {
    let Some(entrada) = pane.selected() else {
        return Vec::new();
    };
    pairs
        .iter()
        .filter_map(|(plugin, column)| {
            let id = crate::columns::plugin_display_id(plugin, column);
            let valor = pane.plugin_cell(&id, &entrada.path)?;
            let text = acotar(&valor, PLUGIN_ITEM_MAX_CELLS);
            let (plugin_visible, _) = crate::display_name(plugin.as_bytes());
            let (column_visible, _) = crate::display_name(column.as_bytes());
            let tooltip = ta_in(
                lang,
                "status-item-plugin-tip",
                &[("plugin", &plugin_visible), ("column", &column_visible)],
            );
            Some(StatusItemView {
                id,
                text,
                tooltip,
                command: None,
                priority: 10,
            })
        })
        .collect()
}

/// `s` en `max` celdas como mucho, con `…` al final si no cabía. Por CELDAS
/// y no por caracteres: un ideograma ocupa dos.
fn acotar(s: &str, max: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if s.width() <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut ancho = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        // Una celda para la elipsis.
        if ancho + w + 1 > max {
            break;
        }
        ancho += w;
        out.push(c);
    }
    // Sin anchura cero al final: un ZWJ o una marca combinante cortados se
    // pegarían a la elipsis (la misma trampa que `ellipsis_at_bytes`).
    while out
        .chars()
        .next_back()
        .is_some_and(|c| c.width() == Some(0))
    {
        out.pop();
    }
    out.push('…');
    out
}

/// La cifra que el tooltip de un elemento necesita, si alguna.
fn tip_count(input: &StatusInput, which: StatusItem) -> String {
    match which {
        StatusItem::Tasks => input.tasks.to_string(),
        StatusItem::Notices => input.notices.to_string(),
        _ => String::new(),
    }
}

/// `Nombre ↑`: la columna y la dirección.
fn sort_text(s: &SortSpec, lang: Lang) -> String {
    let columna = match &s.column {
        SortColumn::Name => t_in(lang, "status-item-sort-name"),
        SortColumn::Size => t_in(lang, "status-item-sort-size"),
        SortColumn::Mtime => t_in(lang, "status-item-sort-mtime"),
        SortColumn::Extension => t_in(lang, "status-item-sort-ext"),
        // Un atributo se nombra por su id (`posix.mode`): su etiqueta legible
        // vive en el catálogo del provider, que esta barra no tiene. Y el id
        // lo emite un TERCERO, así que se enmascara antes de pintarlo, como
        // cualquier otra cosa que un provider dice de sí mismo.
        SortColumn::Attr(id) => crate::display_name(id.as_bytes()).0,
    };
    let flecha = match s.dir {
        SortDir::Asc => '↑',
        SortDir::Desc => '↓',
    };
    format!("{columna} {flecha}")
}

/// Qué elementos caben en `width` celdas con `sep` celdas entre dos
/// seguidos: se descartan los de MENOR prioridad hasta que quepan, y los que
/// quedan conservan el orden configurado. Devuelve sus índices en `items`.
///
/// Media palabra no es un elemento: el que no cabe entero no se pinta.
///
/// ```
/// use norte_frontend::statusbar::{StatusItemView, fit};
/// let v = |id: &str, text: &str, priority| StatusItemView {
///     id: id.into(), text: text.into(), tooltip: String::new(), command: None, priority,
/// };
/// let items = [v("a", "aaaa", 10), v("b", "bb", 90), v("c", "cc", 50)];
/// assert_eq!(fit(&items, 100, 2), [0, 1, 2]);
/// // 2 + 2 + 2 = 6 caben; con «aaaa» serían 12.
/// assert_eq!(fit(&items, 6, 2), [1, 2]);
/// assert_eq!(fit(&items, 1, 2), Vec::<usize>::new());
/// ```
#[must_use]
pub fn fit(items: &[StatusItemView], width: usize, sep: usize) -> Vec<usize> {
    let mut quedan: Vec<usize> = (0..items.len()).collect();
    loop {
        let ancho: usize = quedan
            .iter()
            .map(|&i| crate::display::cells(&items[i].text))
            .sum::<usize>()
            + sep * quedan.len().saturating_sub(1);
        if ancho <= width {
            return quedan;
        }
        // El de menor prioridad; a igual prioridad, el más a la derecha.
        let Some(pos) = quedan
            .iter()
            .enumerate()
            .min_by_key(|&(p, &i)| (items[i].priority, std::cmp::Reverse(p)))
            .map(|(p, _)| p)
        else {
            return quedan;
        };
        quedan.remove(pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> StatusInput {
        StatusInput {
            position: Some((3, 120)),
            marked: 0,
            marked_bytes: 0,
            marked_dirs: 0,
            sort: SortSpec::default(),
            encoding: None,
            tasks: 0,
            notices: 0,
        }
    }

    /// El orden es el de la lista; lo que no tiene nada que decir no sale.
    #[test]
    fn sigue_el_orden_configurado_y_calla_lo_vacio() {
        let lista = StatusItems::parse(&["encoding", "tasks", "position", "marks"]).unwrap();
        let v = items(&input(), lista, Lang::Es);
        let ids: Vec<_> = v.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["encoding", "position"], "sin tareas ni marcas");
        assert_eq!(v[0].text, "UTF-8");
        assert_eq!(v[1].text, "3/120");
    }

    /// Con un filtro, la posición no se enseña: no es la real.
    #[test]
    fn con_filtro_no_hay_posicion() {
        let mut i = input();
        i.position = None;
        let v = items(&i, StatusItems::DEFAULT, Lang::Es);
        assert!(v.iter().all(|x| x.id != "position"));
    }

    /// Cada elemento que se pulsa corre un comando que EXISTE en el
    /// catálogo: un clic que la TUI tira y la ventana rechaza es la misma
    /// decisión con dos respuestas.
    #[test]
    fn los_comandos_existen() {
        let mut i = input();
        i.tasks = 2;
        i.notices = 1;
        i.marked = 1;
        for v in items(&i, StatusItems::DEFAULT, Lang::Es) {
            if let Some(c) = v.command {
                assert!(
                    crate::keymap::catalogue::lookup(c).is_some(),
                    "{c} no está en el catálogo"
                );
            }
            assert!(
                !v.tooltip.starts_with("status-item-"),
                "sin traducir: {}",
                v.tooltip
            );
        }
    }

    #[test]
    fn el_orden_y_la_codificacion_se_leen() {
        let mut i = input();
        i.sort.column = SortColumn::Size;
        i.sort.dir = SortDir::Desc;
        i.encoding = Some(norte_encoding::NameEncoding::Cp437);
        let v = items(&i, StatusItems::DEFAULT, Lang::Es);
        let de = |id| v.iter().find(|x| x.id == id).map(|x| x.text.clone());
        assert_eq!(de("sort").as_deref(), Some("Tamaño ↓"));
        assert_eq!(de("encoding").as_deref(), Some("CP437"));
    }
}
