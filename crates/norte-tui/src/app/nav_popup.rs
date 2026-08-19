//! El popup de navegación: sus items, cómo se muestran y de dónde salen los
//! volúmenes.

use super::display_name;
use norte_i18n::t;
use norte_proto::VPath;

/// Qué popup de navegación está abierto (spec 2026-07-18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavPopupKind {
    /// Historial de directorios del pane con foco (sesión, no persistido).
    History,
    /// Favoritos persistidos en el `norte.toml` del USUARIO.
    Hotlist,
    /// Volúmenes del host (`pane.select-drive`/`-left`/`-right`, design
    /// 2026-08-10-volumes-design.md §D): snapshot congelada al abrir vía
    /// `Backend::volumes` — `main.rs` hace el fetch async (app.rs no conoce
    /// `Backend`) y entrega los items ya construidos a
    /// [`crate::app::App::open_volumes_popup`].
    Volumes,
}

/// Un item del popup de navegación, CONGELADO al construirse en
/// [`crate::app::App::open_nav_popup`]: display ya saneado, destino ya parseado y (en
/// hotlist) la clave cruda del favorito. El popup es una snapshot a
/// propósito — todo lo que una tecla necesita viaja dentro del item, nada
/// se re-resuelve contra un estado que pudo cambiar debajo.
#[derive(Debug, Clone)]
pub struct NavItem {
    /// Display YA saneado, listo para pintar.
    pub display: String,
    /// Destino parseado; `None` = entrada de hotlist inválida (se muestra
    /// con su aviso, no navega).
    pub target: Option<VPath>,
    /// `name` CRUDO del favorito — la clave del borrado con `d`
    /// ([`crate::app::App::nav_popup_selected_hotlist_name`]), congelada al abrir: un
    /// hot-reload puede mutar `App::hotlist` bajo el popup y el borrado
    /// debe caer sobre lo MOSTRADO, jamás sobre lo que ahora ocupe ese
    /// índice en la lista nueva (review MAJOR T5). `None` en historial.
    pub hotlist_name: Option<String>,
}

/// Popup de navegación (`Alt+↓` historial / `Ctrl+D` hotlist). Los `items`
/// se construyen YA saneados en [`crate::app::App::open_nav_popup`] (ver [`NavItem`]):
/// el render no re-decide nada y Enter no re-parsea nada.
#[derive(Debug, Clone)]
pub struct NavPopup {
    /// Historial, hotlist o volúmenes (decide título, footer y qué teclas
    /// extra acepta).
    pub kind: NavPopupKind,
    /// Items congelados al abrir.
    pub(crate) items: Vec<NavItem>,
    /// Índice resaltado.
    pub(crate) cursor: usize,
    /// Input de nombre abierto (`a` en hotlist): captura imprimibles antes
    /// que nada (main.rs); `None` = navegación normal del popup.
    pub name_input: Option<String>,
    /// El pane que `Confirm` navega. El foco para historial, hotlist y
    /// `pane.select-drive`; un LADO fijo para `-left`/`-right`
    /// independientemente de dónde esté el foco (design §D — así se
    /// comportan `Alt+F1`/`Alt+F2` de Total Commander). Congelado al abrir,
    /// misma razón que el resto del item: nada aquí se re-resuelve contra un
    /// foco que pudo moverse debajo del popup.
    pub(crate) target_pane: usize,
    /// Solo volúmenes: si la lista ACTUAL incluye pseudo-filesystems (el
    /// toggle "mostrar todo" del design §E). Sin sentido en historial/
    /// hotlist, donde queda `false`.
    pub(crate) include_pseudo: bool,
}

impl NavPopup {
    /// Sube el cursor (tope arriba).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja el cursor (tope en el último item).
    pub fn down(&mut self) {
        if self.cursor + 1 < self.items.len() {
            self.cursor += 1;
        }
    }

    /// Items congelados para el render.
    #[must_use]
    pub fn items(&self) -> &[NavItem] {
        &self.items
    }

    /// Índice resaltado.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// El item resaltado, si lo hay.
    #[must_use]
    pub fn selected(&self) -> Option<&NavItem> {
        self.items.get(self.cursor)
    }

    /// El pane que `Confirm` debe navegar — ver el campo.
    #[must_use]
    pub fn target_pane(&self) -> usize {
        self.target_pane
    }

    /// Si la lista de volúmenes actual incluye pseudo-filesystems — ver el
    /// campo. Sin significado fuera de `NavPopupKind::Volumes`.
    #[must_use]
    pub fn include_pseudo(&self) -> bool {
        self.include_pseudo
    }
}

/// Display de un item del popup de navegación: `[name — ]path` con el badge
/// hostil como PREFIJO si cualquier parte saldría alterada (mismo criterio
/// que los panes: lossy y MARCADO, spec §6).
pub(crate) fn nav_item_display(
    name: Option<&str>,
    path: &VPath,
    enc: Option<norte_encoding::NameEncoding>,
) -> String {
    // #98/F4: los popups son superficie de DECISIÓN (elegir destino de
    // salto) — siguen la reinterpretación del pane con foco, como la barra.
    let (text, path_hostile) = norte_frontend::path_display_with(path, enc);
    let (prefix, name_hostile) = match name {
        Some(n) => {
            let (nt, nh) = display_name(n.as_bytes());
            (format!("{nt} — "), nh)
        }
        None => (String::new(), false),
    };
    if path_hostile || name_hostile {
        format!("{} {prefix}{text}", crate::ui::HOSTILE_BADGE)
    } else {
        format!("{prefix}{text}")
    }
}

/// Rows for the volumes popup (design §D): `main.rs` calls this right after
/// `Backend::volumes` answers and hands the result to
/// [`crate::app::App::open_volumes_popup`] — this function owns none of the I/O, only the
/// presentation, same split as the rest of the popup family.
#[must_use]
pub fn volume_items(
    volumes: &[norte_proto::methods::Volume],
    enc: Option<norte_encoding::NameEncoding>,
) -> Vec<NavItem> {
    volumes
        .iter()
        .map(|v| NavItem {
            display: volume_item_display(v, enc),
            target: Some(v.mount.clone()),
            hotlist_name: None,
        })
        .collect()
}

/// One volume row: `[label — ]mount  fs_type  free / total`. Every text
/// field the platform hands us — label, mount AND `fs_type` — goes through
/// the same masking [`nav_item_display`] uses (`display_name`/
/// `path_display_with`, both backed by `norte_encoding::is_terminal_hazard`)
/// before it reaches the screen. `fs_type` is not the closed, ASCII-only
/// vocabulary it looks like: a FUSE mount's `fuse.<subtype>` component is the
/// `-o subtype=` value an UNPRIVILEGED user picks (`sshfs`, `rclone mount`,
/// `encfs`…), so it is exactly as untrusted as a filename — encoding-auditor
/// review caught it reaching the row unmasked in an earlier draft of this
/// function, the same class of bug `control_escape` in the canonical corpus
/// exists to catch. `free`/`total` print `volumes-size-unknown` instead of a
/// number when the filesystem did not answer in time — design §A is explicit
/// that a bare `0` here would read as "full", the opposite of what an absent
/// size means.
///
/// `label` is `Option<Vec<u8>>` (V3.5, a second encoding-auditor finding on
/// the same review pass that caught `fs_type` above): it reaches
/// [`display_name`] as the raw bytes the wire carried, with NO `String`
/// upstream to have already thrown away or lossily rewritten a non-UTF-8
/// label before the masking ever saw it — otherwise the badge below would
/// be protecting evidence that was already gone.
fn volume_item_display(
    v: &norte_proto::methods::Volume,
    enc: Option<norte_encoding::NameEncoding>,
) -> String {
    // #98/F4 (same reasoning `nav_item_display` carries): a popup is a
    // decision surface, so it follows the focused pane's reinterpretation.
    let (path_text, path_hostile) = norte_frontend::path_display_with(&v.mount, enc);
    let (label_prefix, label_hostile) = match v.label.as_deref() {
        Some(l) => {
            let (nt, nh) = display_name(l);
            (format!("{nt} — "), nh)
        }
        None => (String::new(), false),
    };
    let (fs_type_text, fs_type_hostile) = display_name(v.fs_type.as_bytes());
    let free = v
        .free_bytes
        .map_or_else(|| t("volumes-size-unknown"), norte_frontend::human_bytes);
    let total = v
        .total_bytes
        .map_or_else(|| t("volumes-size-unknown"), norte_frontend::human_bytes);
    let body = format!("{label_prefix}{path_text}  {fs_type_text}  {free} / {total}");
    if path_hostile || label_hostile || fs_type_hostile {
        format!("{} {body}", crate::ui::HOSTILE_BADGE)
    } else {
        body
    }
}
