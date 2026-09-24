//! Configuration profiles: choosing them, applying them, and saving them.
//!
//! Part of `controller`: these are methods of `Estado`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl Estado` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

/// The open theme selector.
///
/// Same model as the terminal's: the list, the cursor, and what was set on
/// opening — without the last one, `Escape` would leave whatever the cursor
/// brushed past in place, which is changing the theme by accident.
pub(super) struct SeleccionDeTema {
    /// The presets, in the order they are declared.
    pub(super) nombres: Vec<String>,
    /// Which one is pointed at.
    pub(super) cursor: usize,
    /// The one that was set on opening, WHOLE and not just its name.
    ///
    /// Whole because the one that was set might not be a preset — a user
    /// theme file is one just as much — and resolving it again by name would
    /// lose it. The list only offers presets; what gets restored is what was
    /// there.
    pub(super) previo: Box<crate::pickers::HostTheme>,
}

/// What about a profile CANNOT be applied without restarting THIS WINDOW.
///
/// Measured, not assumed, and different from the terminal's list — which is
/// why it is not shared. Here the theme IS applied: the catalogue crosses
/// over again when it changes, and the renderer replugs its CSS variables.
///
/// FONTS and `reduce_motion` travel through that same catalogue and are
/// applied on STARTUP, but not on a profile change: the only thing that
/// triggers a new catalogue is the theme, and that path keeps the appearance
/// that was there instead of rereading it. Making them hot is the question of
/// whether this window reloads its configuration live, which has its own ADR
/// pending — so until then it is SAID, which is what this list exists to do.
///
/// `[ui] lang` neither: `norte_i18n::force` runs once per process.
///
/// A change that stayed quiet about this would be a change that lies (ADR
/// 0079, D8).
fn fuera_de_alcance_en_caliente(
    before: &norte_config::CommonConfig,
    after: &norte_config::CommonConfig,
) -> Vec<&'static str> {
    let mut out = Vec::new();
    if before.ui_lang != after.ui_lang {
        out.push("ui.lang");
    }
    if before.ui_font != after.ui_font
        || before.ui_mono_font != after.ui_mono_font
        || before.ui_font_size != after.ui_font_size
    {
        out.push("ui.font");
    }
    if before.ui_reduce_motion != after.ui_reduce_motion {
        out.push("ui.reduce_motion");
    }
    out
}

impl Estado {
    /// The profile loaded (or did not): everything that can be applied
    /// without restarting is applied, and what cannot is SAID.
    ///
    /// Order matters. First the theme, which is what is seen; then the whole
    /// keymap, the columns and the favorites; and last the layout the profile
    /// names, because it changes the slots and everything before it has to
    /// be in place when they are relisted.
    ///
    /// A profile that does NOT load changes nothing: you stay where you were
    /// and it says why (ADR 0079, D7).
    pub(super) fn aplicar_perfil(
        &mut self,
        name: &std::ffi::OsStr,
        loaded: Result<norte_frontend::config::FrontendConfig, &'static str>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let cfg = match loaded {
            Ok(c) => c,
            Err(key) => return self.decir(key),
        };
        self.perfil_activo = Some(name.to_os_string());
        self.selector_perfil = None;
        let out = self.aplicar_config(cfg, backend, mailbox);
        // And `[profile.start]`: where each slot the session knows nothing
        // about opens. That is what makes a freshly created profile, or one
        // arriving from another machine, useful — without this, switching
        // into "work" left both panels where they were and the profile only
        // changed the colors.
        //
        // It goes AFTER the layout because the slot has to exist to be able
        // to seed it, and through `navegar_hueco` because there may already
        // be a request in flight for the previous listing: a new token
        // supersedes it, and setting the dir by hand would let the old one
        // land on top.
        //
        // `Seed` and not `Record`: seeding is not a step the reader walked,
        // and putting the previous profile's directory into this one's
        // "back" is offering a way back to a place you never came from. It is
        // also what the terminal does, seeding by building the pane from
        // scratch.
        //
        // The patches `navegar_hueco` returns are DISCARDED, same as the
        // layout's and for the same reason: this ends in a whole snapshot.
        // Sequence numbers the renderer never gets to see are spent, and it
        // is not a hole — a snapshot closes any gap in the sequence, which is
        // exactly what it exists for.
        for (id, target) in self.siembra_de_perfil() {
            let _ = self.navegar_hueco(id, &target, Trail::Seed, backend, mailbox);
        }
        let mut changes = Vec::new();
        // And whatever the profile's FILE brings that is not understood,
        // which outranks the other two messages: "could not apply live"
        // describes a limit of this process, and this describes lines that
        // are never going to do anything. Staying quiet about them is what
        // turned `[profile.start]` into a trap — a path with no scheme was
        // dropped and the slot opened wherever it felt like, with nothing
        // saying so.
        let warnings = self.config.common.profile_warnings.len();
        for warning in &self.config.common.profile_warnings {
            tracing::warn!(motivo = %warning, "profile line ignored");
        }
        if warnings > 0 {
            self.status.message = Some(clamp_display(norte_i18n::ta_in(
                self.lang,
                "msg-profile-config-ignored",
                &[
                    ("profile", &name.to_string_lossy()),
                    ("n", &warnings.to_string()),
                ],
            )));
        } else if out.is_empty() {
            self.status.message = Some(clamp_display(norte_i18n::ta_in(
                self.lang,
                "msg-profile-switched",
                &[("profile", &name.to_string_lossy())],
            )));
        } else {
            self.status.message = Some(clamp_display(norte_i18n::ta_in(
                self.lang,
                "msg-profile-switched-partial",
                &[
                    ("profile", &name.to_string_lossy()),
                    ("keys", &out.join(", ")),
                ],
            )));
        }
        let snap = self.snapshot();
        changes.push(self.sobre(UiUpdate::Snapshot(Box::new(snap))));
        changes
    }

    /// Sets `cfg` as the current configuration and applies everything this
    /// window knows how to apply without restarting: theme, whole keymap,
    /// columns, favorites, and the layout if its name changes.
    ///
    /// This is the profile-change path, and also the one for a setting
    /// written from F11: a reload is a reload, wherever it comes from (ADR
    /// 0077 again — two ways of applying the same configuration diverge
    /// silently). It returns the ids of what could NOT be applied live, so
    /// the caller can say so by name (D8).
    ///
    /// A keymap that cannot be built leaves the one that was there: a broken
    /// `keymap.toml` cannot leave the window without keys. What the layout
    /// and the listings return is DISCARDED: the caller ends up with a
    /// complete snapshot, and sending patches before that is sending them for
    /// nothing.
    pub(super) fn aplicar_config(
        &mut self,
        cfg: norte_frontend::config::FrontendConfig,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<&'static str> {
        let before = self.config.common.clone();
        // The THEME. It is set by name, and the notice to whoever hosts it
        // comes from here: it is what is seen.
        if let Some(theme) = cfg.common.ui_theme.clone() {
            self.aplicar_tema(&theme, mailbox);
        }
        // The WHOLE keymap, with the layers inside.
        if let Ok(browse) = crate::keys::keymap_de_preset_con_capas(
            &cfg.common.preset,
            &cfg.keymap_layers,
            self.efectos,
        ) {
            self.efectivo = browse.clone();
            self.resolver = Resolver::new(browse);
        }
        if let Ok(visor) =
            crate::keys::keymap_visor_de_preset_con_capas(&cfg.common.preset, &cfg.keymap_layers)
        {
            self.efectivo_visor = visor.clone();
            self.resolver_visor = Resolver::new(visor);
        }
        if let Ok(dialogo) =
            crate::keys::keymap_dialogo_de_preset_con_capas(&cfg.common.preset, &cfg.keymap_layers)
        {
            self.resolver_dialogo = Resolver::new(dialogo);
        }
        // Columns and favorites.
        self.columnas = norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns)
            .with_date_format(cfg.common.ui_chrome.date_format());
        self.config = cfg;
        self.sembrar_sitios();
        // And the LAYOUT the configuration names, if it names a different
        // one: that is what makes a profile "a different screen" and not
        // just different colors.
        if before.ui_layout != self.config.common.ui_layout
            && let Some(name) = self.config.common.ui_layout.clone()
            && let Ok(tree) = norte_frontend::layout::presets::tree(&name)
        {
            let _ = self.aplicar_disposicion(tree, backend, mailbox);
        }
        // Whatever cannot be applied without restarting is said by name: a
        // change that stayed quiet about this would be a change that lies
        // (D8).
        fuera_de_alcance_en_caliente(&before, &self.config.common)
    }

    /// The configuration layers exactly as they are set NOW: the ones
    /// startup resolved, with the active profile on top if there is one.
    ///
    /// This is where it is reread from after writing a setting: the window
    /// rereads from where it actually read (ADR 0066 D14), and with whatever
    /// profile it currently has set, which may not be the startup one.
    pub(super) fn capas_actuales(&self) -> norte_config::Layers {
        use crate::settings::ConfigLayer;
        if let Some(profile) = &self.perfil_activo
            && let Some(layers) = self.capas_con_perfil(profile)
        {
            return layers;
        }
        let dirs = self
            .paths
            .config_layers
            .iter()
            .map(|(layer, path)| {
                let kind = match layer {
                    ConfigLayer::System => norte_config::Layer::System,
                    ConfigLayer::User => norte_config::Layer::User,
                    ConfigLayer::Profile => norte_config::Layer::Profile,
                    ConfigLayer::Project => norte_config::Layer::Project,
                };
                (path.path.clone(), kind)
            })
            .collect();
        norte_config::Layers { dirs }
    }

    /// Requests the profile list, OUTSIDE the actor.
    ///
    /// `neighbor` says what to do with it once it arrives: `None` opens the
    /// selector, `Some(forward)` jumps to the next one without opening
    /// anything — which is what whoever has two profiles and alternates
    /// wants.
    ///
    /// Reading `profiles/` is a directory and a `norte.toml` per profile: in
    /// the actor it would freeze the window, and with a directory on a
    /// downed NFS it would freeze it until the mount expires (rule 2, #244).
    pub(super) fn pedir_perfiles(
        &mut self,
        neighbor: Option<bool>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(dir) = self.dir_de_perfiles() else {
            return self.no_hay_perfiles();
        };
        let mailbox = mailbox.clone();
        tokio::task::spawn_blocking(move || {
            let profiles = norte_frontend::config::read_profiles(&dir);
            let _ = mailbox.blocking_send(Mensaje::Fondo(Box::new(Fondo::Perfiles(
                profiles, neighbor,
            ))));
        });
        (self.aplicada(), Vec::new())
    }

    /// Where `profiles/` lives: the USER layer.
    ///
    /// The user's and only the user's, even with an active profile: profiles
    /// do not nest (D1), and looking for them inside the current profile
    /// would be inventing a hierarchy the ADR does not have.
    pub(super) fn dir_de_perfiles(&self) -> Option<std::path::PathBuf> {
        use crate::settings::ConfigLayer;
        self.paths
            .config_layers
            .iter()
            .find(|(layer, _)| matches!(layer, ConfigLayer::User))
            .map(|(_, p)| p.path.clone())
    }

    /// There is nowhere to look for profiles, and it says so.
    pub(super) fn no_hay_perfiles(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let outgoing = self.decir("host-no-profiles");
        (
            ActionAck::Unavailable {
                reason_key: "host-no-profiles".to_owned(),
            },
            outgoing,
        )
    }

    /// The profile list arrived: either the selector opens, or it jumps to
    /// the neighbor.
    pub(super) fn con_los_perfiles(
        &mut self,
        profiles: Vec<norte_frontend::profile_picker::UserProfile>,
        neighbor: Option<bool>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.gen_perfiles += 1;
        let Some(forward) = neighbor else {
            self.selector_perfil = Some(norte_frontend::profile_picker::ProfilePicker::open(
                profiles,
                self.perfil_activo.as_deref(),
            ));
            return vec![self.parche(vec![ViewChange::Profiles {
                profiles: self.vista_perfiles(),
            }])];
        };
        // Spinning around a single one is not a change: tearing down and
        // reloading the screen to leave it the same would be worse than doing
        // nothing, and saying so is more honest than pretending something
        // happened.
        let Some(next) = norte_frontend::profile_picker::next_profile(
            &profiles,
            self.perfil_activo.as_deref(),
            forward,
        ) else {
            return self.decir("host-no-other-profile");
        };
        self.cambiar_de_perfil(&next, mailbox)
    }

    /// Starts a profile change: loads its configuration OUTSIDE the actor.
    ///
    /// The change is not applied here. Loading the configuration reads
    /// between one and four files per layer, and until it is known whether
    /// it loads, nothing is touched: a broken profile leaves the reader where
    /// they were (ADR 0079, D7).
    pub(super) fn cambiar_de_perfil(
        &mut self,
        name: &std::ffi::OsStr,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(layers) = self.capas_con_perfil(name) else {
            return self.decir("host-no-profiles");
        };
        let name = name.to_os_string();
        let mailbox = mailbox.clone();
        tokio::task::spawn_blocking(move || {
            let res = norte_frontend::config::load(&layers).map_err(|_| "host-profile-broken");
            let _ = mailbox.blocking_send(Mensaje::Fondo(Box::new(Fondo::PerfilCargado(
                name,
                Box::new(res),
            ))));
        });
        Vec::new()
    }

    /// The configuration layers WITH the profile set.
    ///
    /// They are built on top of the ones whoever started the host resolved,
    /// without looking at the environment again: the window reads from where
    /// it actually read (ADR 0066 D14). The profile goes in above the user's
    /// and below the project's, which is its place (ADR 0079, D1) — and since
    /// here the layers come in ascending precedence, it is enough to insert
    /// it right after the user's.
    pub(super) fn capas_con_perfil(&self, name: &std::ffi::OsStr) -> Option<norte_config::Layers> {
        use crate::settings::ConfigLayer;
        let mut dirs = Vec::new();
        let mut set = false;
        for (layer, path) in &self.paths.config_layers {
            match layer {
                ConfigLayer::System => dirs.push((path.path.clone(), norte_config::Layer::System)),
                ConfigLayer::User => {
                    dirs.push((path.path.clone(), norte_config::Layer::User));
                    dirs.push((
                        path.path.join("profiles").join(name),
                        norte_config::Layer::Profile,
                    ));
                    set = true;
                }
                // Whichever one there was is REPLACED: switching profiles
                // does not stack profiles.
                ConfigLayer::Profile => {}
                ConfigLayer::Project => {
                    dirs.push((path.path.clone(), norte_config::Layer::Project));
                }
            }
        }
        set.then_some(norte_config::Layers { dirs })
    }

    /// Opens the theme selector, with the cursor on the current one.
    ///
    /// It used to only SHOW the active theme from the inside, and not for
    /// lack of trying: whatever hosts this window resolves the theme once on
    /// startup, so there was no way for a chosen theme to be seen. With
    /// [`NativeEffect::ThemeChanged`] there is, and this selector is the same
    /// as the terminal's — presets, cursor on the one that is set, and LIVE
    /// preview while moving.
    pub(super) fn abrir_tema(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let names = norte_frontend::theme::theme_names(&self.config.user_themes);
        let cursor = names.iter().position(|n| *n == self.tema.name).unwrap_or(0);
        self.tema_elegido = Some(SeleccionDeTema {
            nombres: names,
            cursor,
            previo: Box::new(self.tema.clone()),
        });
        let change = ViewChange::Theme {
            theme: self.vista_tema(),
        };
        (self.aplicada(), vec![self.parche(vec![change])])
    }
}
