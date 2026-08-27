//! Los selectores en popup vistos desde `App`: tema (con preview en vivo),
//! disposición, conexiones y columnas, con la entrada de teclado de cada uno.

use super::{App, PickerAction, ThemePicker};
use norte_i18n::ta;

impl App {
    /// Abre el popup selector de tema (ADR 0020): lista de presets, cursor en el
    /// tema vigente, con preview EN VIVO desde ya.
    pub fn open_theme_picker(&mut self) {
        let names: Vec<String> = norte_theme::preset_names()
            .into_iter()
            .map(String::from)
            .collect();
        let current = self.theme.name().map(String::from);
        let cursor = current
            .as_ref()
            .and_then(|c| names.iter().position(|n| n == c))
            .unwrap_or(0);
        self.theme_picker = Some(ThemePicker {
            names,
            cursor,
            original: self.theme.clone(),
        });
        self.preview_theme();
    }

    /// Abre el selector de disposición: las cinco de fábrica más lo que haya
    /// en `<dir>/layouts/*.toml`.
    ///
    /// El listado del directorio lo hace el llamante y llega ya hecho: leer
    /// un directorio es I/O, y esto se llama desde un contexto async
    /// (regla 2).
    pub fn open_layout_picker(&mut self, user: Vec<norte_frontend::layout_picker::UserLayout>) {
        self.layout_picker = Some(norte_frontend::layout_picker::LayoutPicker::open(user));
    }

    /// Abre el selector de PERFILES con lo que haya en `profiles/`.
    ///
    /// Igual que el de disposición: el listado —y la lectura del `norte.toml`
    /// de cada uno, que es lo que da el título y el motivo de una fila rota—
    /// lo hace el llamante fuera del runtime (regla 2, #244).
    pub fn open_profile_picker(
        &mut self,
        perfiles: Vec<norte_frontend::profile_picker::UserProfile>,
    ) {
        self.profile_picker = Some(norte_frontend::profile_picker::ProfilePicker::open(
            perfiles,
            self.active_profile.as_deref(),
        ));
    }

    /// Abre el selector de conexiones (#140) con lo que haya en
    /// `connections.toml`. Leerlo es del frontend: este tipo no toca disco.
    pub fn open_connections_picker(&mut self, filas: Vec<norte_frontend::connections_picker::Row>) {
        self.connections_picker = Some(
            norte_frontend::connections_picker::ConnectionsPicker::open(filas),
        );
    }

    /// Teclas del selector de conexiones. Confirmar devuelve la URL elegida
    /// —navegar es del run loop, que es quien tiene el backend— y cerrar el
    /// selector es parte de confirmar: la conexión se pide una vez.
    pub fn connections_picker_input(&mut self, action: PickerAction) -> Option<String> {
        match action {
            PickerAction::Up => {
                if let Some(p) = &mut self.connections_picker {
                    p.up();
                }
                None
            }
            PickerAction::Down => {
                if let Some(p) = &mut self.connections_picker {
                    p.down();
                }
                None
            }
            PickerAction::Confirm => self
                .connections_picker
                .take()
                .and_then(|p| p.chosen().map(String::from)),
            PickerAction::Cancel => {
                self.connections_picker = None;
                None
            }
        }
    }

    /// Procesa una acción del usuario sobre el selector de disposición.
    ///
    /// A diferencia del selector de tema NO hay preview en vivo: aplicar un
    /// layout recrea paneles y mueve el foco, así que pasar el cursor por la
    /// lista rehaciendo la pantalla cinco veces sería peor que verla una vez.
    /// La miniatura de cada fila hace ese trabajo.
    pub fn layout_picker_input(&mut self, action: PickerAction) {
        match action {
            PickerAction::Up => {
                if let Some(p) = &mut self.layout_picker {
                    p.up();
                }
            }
            PickerAction::Down => {
                if let Some(p) = &mut self.layout_picker {
                    p.down();
                }
            }
            // Confirmar NO lee disco: la fila ya trae su árbol, leído fuera
            // del bucle al abrir el selector. Antes, `Enter` sobre una fila
            // llamaba al cargador desde dentro del bucle de eventos, y con el
            // directorio de config en un montaje caído se colgaban entrada,
            // repintado, progreso de tareas y `Ctrl+C` a la vez (#244 M2,
            // regla 2).
            PickerAction::Confirm => {
                let Some(row) = self.layout_picker.take().and_then(|p| p.current().cloned()) else {
                    return;
                };
                let (showable, _) = norte_frontend::display_os_name(&row.name);
                let showable = norte_encoding::mask_terminal_hazards(&showable);
                if let Some(tree) = row.tree {
                    self.set_layout(tree);
                    self.message = Some(ta("msg-layout-applied", &[("name", &showable)]));
                } else {
                    // Una fila que no parsea se eligió a sabiendas: el
                    // selector ya lo decía en su mitad derecha.
                    let err = row.problem.unwrap_or_default();
                    self.message = Some(ta(
                        "msg-layout-load-failed",
                        &[("name", &showable), ("err", &err)],
                    ));
                }
            }
            PickerAction::Cancel => self.layout_picker = None,
        }
    }

    /// Abre el picker de columnas para el pane con foco (#108 7a): parte del
    /// set efectivo de su scheme y de su orden VIVO (el del pane, no el de
    /// config — un sort de cabecera previo no se pierde al abrir). Con el
    /// catálogo cacheado del scheme (#117): el picker OFRECE los attrs
    /// anunciados por el provider y cicla sus formatos por hint.
    /// `plugins` = el catálogo VIVO (`plugin.list`), para ofrecer también las
    /// columnas que declaran los plugins aprobados y activados (#120). Vacío
    /// (el fetch falló, o no hay daemon) = se ofrecen solo builtins y attrs:
    /// una columna de plugin que no se puede confirmar que exista no se
    /// ofrece, igual que un attr no anunciado.
    pub fn open_columns_picker(&mut self, plugins: &[norte_proto::methods::PluginInfo]) {
        let scheme = self.focused().dir().scheme().to_owned();
        let sort = self.focused().sort();
        self.columns_picker = Some(
            norte_frontend::columns_picker::ColumnsPicker::open_with_catalog(
                &self.columns,
                &scheme,
                sort,
                self.attr_catalog(&scheme),
                plugins,
            ),
        );
    }

    /// Aplica al vuelo el tema resaltado en el popup (preview en vivo).
    fn preview_theme(&mut self) {
        let Some(name) = self
            .theme_picker
            .as_ref()
            .and_then(|p| p.selected().map(String::from))
        else {
            return;
        };
        let depth = crate::theme::detect_depth();
        if let Ok(theme) = crate::theme::resolve(Some(&name), depth) {
            self.theme = theme;
        }
    }

    /// Procesa una acción del usuario sobre el popup de tema. `Confirm` fija el
    /// tema previsualizado; `Cancel` revierte al que había al abrir.
    pub fn theme_picker_input(&mut self, action: PickerAction) {
        match action {
            PickerAction::Up => {
                if let Some(p) = &mut self.theme_picker {
                    p.up();
                }
                self.preview_theme();
            }
            PickerAction::Down => {
                if let Some(p) = &mut self.theme_picker {
                    p.down();
                }
                self.preview_theme();
            }
            PickerAction::Confirm => {
                let name = self
                    .theme_picker
                    .as_ref()
                    .and_then(|p| p.selected().map(String::from));
                self.theme_picker = None;
                if let Some(n) = name {
                    self.message = Some(norte_i18n::ta("msg-theme-applied", &[("name", &n)]));
                }
            }
            PickerAction::Cancel => {
                if let Some(p) = self.theme_picker.take() {
                    self.theme = p.original;
                }
                self.message = Some(norte_i18n::t("msg-theme-reverted"));
            }
        }
    }
}
