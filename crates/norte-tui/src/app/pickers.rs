//! The popup pickers seen from `App`: theme (with live preview), layout,
//! connections and columns, with each one's key input.

use super::{App, PickerAction, ThemePicker};
use norte_i18n::ta;

impl App {
    /// Opens the theme picker popup (ADR 0020): a list of presets, cursor on
    /// the current theme, with a LIVE preview from the start.
    pub fn open_theme_picker(&mut self) {
        let names = norte_frontend::theme::theme_names(&self.user_themes);
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

    /// Opens the layout picker: the five factory ones plus whatever is in
    /// `<dir>/layouts/*.toml`.
    ///
    /// The caller does the directory listing and hands it over already done:
    /// reading a directory is I/O, and this gets called from an async
    /// context (rule 2).
    pub fn open_layout_picker(&mut self, user: Vec<norte_frontend::layout_picker::UserLayout>) {
        self.layout_picker = Some(norte_frontend::layout_picker::LayoutPicker::open(user));
    }

    /// Opens the PROFILE picker with whatever is in `profiles/`.
    ///
    /// Same as the layout one: the listing — and reading each one's
    /// `norte.toml`, which is what gives the title and the reason for a
    /// broken row — is done by the caller outside the runtime (rule 2,
    /// #244).
    pub fn open_profile_picker(
        &mut self,
        profiles: Vec<norte_frontend::profile_picker::UserProfile>,
    ) {
        self.profile_picker = Some(norte_frontend::profile_picker::ProfilePicker::open(
            profiles,
            self.active_profile.as_deref(),
        ));
    }

    /// Opens the connections picker (#140) with whatever is in
    /// `connections.toml`. Reading it belongs to the frontend: this type
    /// doesn't touch disk.
    pub fn open_connections_picker(&mut self, rows: Vec<norte_frontend::connections_picker::Row>) {
        self.connections_picker = Some(
            norte_frontend::connections_picker::ConnectionsPicker::open(rows),
        );
    }

    /// Connections picker keys. Confirming returns the chosen URL —
    /// navigating belongs to the run loop, which is what has the backend —
    /// and closing the picker is part of confirming: the connection is
    /// requested once.
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

    /// PROFILE picker keys.
    ///
    /// Confirming doesn't change anything here: it leaves the REQUESTED name
    /// and closes. The loop does the switch, since it's the one holding the
    /// layers and the resolvers — and doing it here would mean reloading
    /// config from inside a key handler, rule 2 again.
    ///
    /// Choosing the profile that's ALREADY active requests nothing: a change
    /// that changes nothing would tear down and reload the screen just to
    /// leave it the same.
    pub fn profile_picker_input(&mut self, action: PickerAction) {
        match action {
            PickerAction::Up => {
                if let Some(p) = &mut self.profile_picker {
                    p.up();
                }
            }
            PickerAction::Down => {
                if let Some(p) = &mut self.profile_picker {
                    p.down();
                }
            }
            PickerAction::Confirm => {
                let Some(row) = self
                    .profile_picker
                    .take()
                    .and_then(|p| p.current().cloned())
                else {
                    return;
                };
                if !row.active {
                    self.pending_profile = Some(row.name);
                }
            }
            PickerAction::Cancel => self.profile_picker = None,
        }
    }

    /// Processes a user action on the layout picker.
    ///
    /// Unlike the theme picker there's NO live preview: applying a layout
    /// recreates panels and moves focus, so moving the cursor through the
    /// list and redoing the screen five times would be worse than seeing it
    /// once. Each row's thumbnail does that job.
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
            // Confirming does NOT read disk: the row already carries its
            // tree, read outside the loop when the picker opened. Before,
            // `Enter` on a row called the loader from inside the event loop,
            // and with the config directory on a downed mount, input,
            // repainting, task progress and `Ctrl+C` all hung at once (#244
            // M2, rule 2).
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
                    // A row that doesn't parse was chosen knowingly: the
                    // picker already said so on its right half.
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

    /// Opens the columns picker for the focused pane (#108 7a): built from
    /// its scheme's effective set and its LIVE order (the pane's, not
    /// config's — a prior header sort doesn't get lost on open). With the
    /// scheme's cached catalogue (#117): the picker OFFERS the attrs the
    /// provider advertises and cycles their formats by hint. `plugins` = the
    /// LIVE catalogue (`plugin.list`), to also offer the columns approved
    /// and enabled plugins declare (#120). Empty (the fetch failed, or
    /// there's no daemon) = only builtins and attrs get offered: a plugin
    /// column that can't be confirmed to exist isn't offered, same as an
    /// unadvertised attr.
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

    /// Applies the popup's highlighted theme on the fly (live preview).
    fn preview_theme(&mut self) {
        let Some(name) = self
            .theme_picker
            .as_ref()
            .and_then(|p| p.selected().map(String::from))
        else {
            return;
        };
        // No disk: a preset or a user theme the config already loaded.
        // Resolving here would read a file from inside a key handler
        // (rule 2).
        let depth = crate::theme::detect_depth();
        if let Some(theme) = norte_frontend::theme::theme_by_name(&name, &self.user_themes) {
            self.theme = crate::theme::TuiTheme::new(theme, depth);
        }
    }

    /// Processes a user action on the theme popup. `Confirm` fixes the
    /// previewed theme; `Cancel` reverts to whatever was there on open.
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
