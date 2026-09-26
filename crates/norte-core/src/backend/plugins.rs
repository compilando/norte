//! [`Backend`](super::Backend)'s plugin area (M4-P3/P4, ADR 0037, ADR 0095):
//! catalog and approval/activation/uninstall, command execution, preview
//! (plain and styled), thumbnails, decorators, columns, panels, plugin
//! renamers/organizers, and `[config]`.

use futures::StreamExt;
use norte_proto::{ByteRange, Error, VPath};

use super::Backend;

impl Backend {
    /// Catalog of discovered plugins + their approved/enabled state (M4-P3).
    /// Embedded: discovers from [`crate::connect::config_dir`] on demand
    /// (the state lives in `plugins-state.toml`, not in memory — no
    /// registry needs to be retained between calls); remote: `plugin.list`
    /// against the daemon.
    ///
    /// # Errors
    /// Protocol taxonomy; with the daemon down,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn plugins_list(&self) -> Result<norte_proto::methods::PluginListResult, Error> {
        match self {
            Self::Embedded(_) => {
                // EPHEMERAL per-call registry (sync I/O → spawn_blocking,
                // rule 2). The truth lives in the state file.
                let dir = crate::connect::config_dir();
                crate::blocking::spawn_blocking(move || {
                    crate::PluginRegistry::discover(&dir).map(|r| r.list())
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
                .map_err(|_| Error::Io { retryable: false })
            }
            Self::Remote(r) => r.plugins_list().await,
        }
    }

    /// Approves (or revokes) a plugin's capabilities (M4-P3). Embedded:
    /// discover + `set_approval` + persist, all in `spawn_blocking`.
    ///
    /// # Security invariant (defense in depth)
    /// The "ONLY a human approves" gate lives in the WIRE layer (the
    /// daemon, which ties the connection to a [`crate::journal::Actor`]).
    /// This embedded `Backend` is the HUMAN frontend's in-process API and
    /// does NOT receive an `Actor`: approving through here is, by
    /// construction, a human's act. If an agent bridge to an embedded
    /// `Backend` is ever wired up, the gate would have to be replicated
    /// HERE (it doesn't exist today and must not be introduced without
    /// that gate).
    ///
    /// # Errors
    /// [`Error::NotFound`] if the id is unknown; protocol taxonomy for the
    /// rest.
    pub async fn plugins_set_approval(
        &self,
        id: &str,
        approved: bool,
        expected_digest: Option<&str>,
    ) -> Result<(), Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let expected = expected_digest.map(ToOwned::to_owned);
                let applied = crate::blocking::spawn_blocking(move || {
                    let mut reg = crate::PluginRegistry::discover(&dir)?;
                    // #282's check matters MORE here than on the daemon:
                    // that one discovers the catalog once at startup, so
                    // its window is closed by accident. This `discover`
                    // runs on EVERY call, i.e. the `plugin.toml` read now
                    // may not be the one the human read a moment ago.
                    // An unknown id is NOT a stale anchor: both give
                    // `None` here, and conflating them would tell whoever
                    // named a nonexistent plugin "the manifest changed".
                    if approved
                        && let Some(actual) = reg.manifest_digest(&id)
                        && let Some(expected) = &expected
                        && &actual != expected
                    {
                        return Ok(None);
                    }
                    reg.set_approval(&id, approved).map(Some)
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
                .map_err(|_| Error::Io { retryable: false })?;
                match applied {
                    Some(true) => Ok(()),
                    Some(false) => Err(Error::NotFound),
                    // The manifest changed underfoot: it's not granted, and
                    // it's said with the variant that means exactly that
                    // —the same one `session.put` uses with a stale
                    // revision—. Not `Exists`, which reads as "the
                    // destination is already there" and means nothing
                    // here.
                    None => Err(Error::Conflict {
                        conflict: norte_proto::ConflictKind::StaleRevision,
                    }),
                }
            }
            Self::Remote(r) => r.plugins_set_approval(id, approved, expected_digest).await,
        }
    }

    /// Enables/disables an already-approved plugin (M4-P3). Semantics
    /// identical to [`Self::plugins_set_approval`].
    ///
    /// # Security invariant (defense in depth)
    /// Same as [`Self::plugins_set_approval`]: the "human only" gate lives
    /// in the wire layer (the daemon with `Actor`); this embedded Backend
    /// is the human frontend's API and receives no `Actor`. A future agent
    /// bridge to an embedded Backend would have to replicate the gate here.
    ///
    /// # Errors
    /// [`Error::NotFound`] if the id is unknown; protocol taxonomy for the
    /// rest.
    pub async fn plugins_set_enabled(&self, id: &str, enabled: bool) -> Result<(), Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let applied = crate::blocking::spawn_blocking(move || {
                    let mut reg = crate::PluginRegistry::discover(&dir)?;
                    reg.set_enabled(&id, enabled)
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
                .map_err(|_| Error::Io { retryable: false })?;
                if applied {
                    Ok(())
                } else {
                    Err(Error::NotFound)
                }
            }
            Self::Remote(r) => r.plugins_set_enabled(id, enabled).await,
        }
    }

    /// Uninstalls a plugin (0.71.0, ADR 0104): deletes its directory and
    /// leaves its state disabled and unapproved. Returns whether it had
    /// consent, which is what just stopped existing.
    ///
    /// Embedded: [`crate::plugins::uninstall`] in `spawn_blocking`, the
    /// same thing the CLI does. Remote: `plugin.uninstall` against the
    /// daemon, which also forgets it in its in-memory registry.
    ///
    /// # Security invariant (defense in depth)
    /// Same as [`Self::plugins_set_approval`]: the "human only" gate lives
    /// in the wire layer.
    ///
    /// # Errors
    /// [`Error::NotFound`] if the id isn't an id or isn't installed;
    /// [`Error::Io`] if the delete or the state fail.
    pub async fn plugins_uninstall(&self, id: &str) -> Result<bool, Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let report =
                    crate::blocking::spawn_blocking(move || crate::plugins::uninstall(&dir, &id))
                        .await
                        .map_err(|_| Error::Internal { panic: true })?
                        .map_err(|e| {
                            use crate::plugins::UninstallError as U;
                            match e {
                                U::InvalidId | U::NotInstalled(_) => Error::NotFound,
                                U::Io(_) => Error::Io { retryable: false },
                            }
                        })?;
                Ok(report.was_approved)
            }
            Self::Remote(r) => r.plugins_uninstall(id).await.map(|r| r.was_approved),
        }
    }

    /// Runs a command from an ALREADY approved and enabled plugin (M4-P4)
    /// and returns its output. Embedded: EPHEMERAL per-call registry + a new
    /// `PluginRuntime`, ALL in `spawn_blocking` (instantiation compiles
    /// WASM: heavy and synchronous, rule 2). Remote: `plugin.run_command`
    /// against the daemon.
    ///
    /// The cost of creating the runtime per-call is accepted just like
    /// [`Self::plugins_list`]'s ephemeral registry: embedded mode is an
    /// occasional human frontend, not a high-frequency plugin server (that's
    /// the daemon, which does reuse a shared runtime).
    ///
    /// # Errors
    /// Protocol taxonomy; with the daemon down,
    /// `ProviderUnavailable{retryable:true}`. A plugin runtime failure is
    /// delivered REDACTED (`Internal`), with no leaked paths or internal
    /// details.
    pub async fn plugin_run_command(
        &self,
        id: &str,
        command: &str,
        arg: &str,
    ) -> Result<String, Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let command = command.to_owned();
                let arg = arg.to_owned();
                crate::blocking::spawn_blocking(move || -> Result<String, Error> {
                    let reg = crate::PluginRegistry::discover(&dir)
                        .map_err(|_| Error::Io { retryable: false })?;
                    let runtime = shared_runtime().map_err(|_| Error::Internal { panic: false })?;
                    reg.run_command(runtime, &id, &command, &arg)
                        .map_err(|e| run_error_to_taxonomy(&e))
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            Self::Remote(r) => r.plugin_run_command(id, command, arg).await,
        }
    }

    /// Previews `path` with the FIRST consented previewer whose mimetype
    /// (guessed by extension) matches, or returns `preview: None` if none
    /// applies (the frontend falls back to the raw view). An unreadable
    /// file is an honest error, not an empty preview.
    ///
    /// Embedded: EPHEMERAL per-call registry + previewer resolution + a new
    /// `PluginRuntime`, interleaved with the capped byte read. Resolution
    /// (discover, IO) and execution (WASM instance, synchronous) go in
    /// `spawn_blocking` (rule 2); reading bytes uses the async engine in
    /// between. The ephemeral registry/runtime cost is accepted just like
    /// in [`Self::plugin_run_command`]: embedded mode is an occasional human
    /// frontend, not a high-frequency plugin server (that's the daemon,
    /// which reuses a shared registry and runtime). Remote:
    /// `plugin.preview` against the daemon.
    ///
    /// # Errors
    /// Protocol taxonomy; with the daemon down,
    /// `ProviderUnavailable{retryable:true}`. A plugin runtime failure is
    /// delivered REDACTED (`Internal`), with no leaked paths or internal
    /// details.
    pub async fn plugin_preview(
        &self,
        path: &VPath,
    ) -> Result<norte_proto::methods::PluginPreviewResult, Error> {
        match self {
            Self::Embedded(engine) => {
                // 1) Resolve the previewer (discover = IO) in spawn_blocking.
                let dir = crate::connect::config_dir();
                let mime = crate::plugins::guess_mimetype(path);
                let resolved = crate::blocking::spawn_blocking(
                    move || -> Result<Option<crate::plugins::ResolvedPreviewer>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        Ok(reg.resolve_previewer(mime))
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                let Some((id, name, wasm, caps, settings)) = resolved else {
                    return Ok(norte_proto::methods::PluginPreviewResult { preview: None });
                };

                // 2) Read the CAPPED bytes via the engine (async). An
                // unreadable file is an honest error that propagates.
                let range = ByteRange {
                    offset: 0,
                    len: Some(crate::plugins::PREVIEW_MAX_BYTES),
                };
                let mut stream = engine.read(path, Some(range)).await?;
                let mut bytes: Vec<u8> = Vec::new();
                while let Some(chunk) = stream.next().await {
                    bytes.extend_from_slice(&chunk?);
                    if bytes.len() as u64 >= crate::plugins::PREVIEW_MAX_BYTES {
                        break;
                    }
                }
                let cap = usize::try_from(crate::plugins::PREVIEW_MAX_BYTES).unwrap_or(usize::MAX);
                bytes.truncate(cap.min(bytes.len()));

                // 2.5) §6.2 (#29): the previewer receives TEXT already
                // decoded by the core's detection — never raw bytes to
                // assume UTF-8 over (logic tested in
                // `plugins::decode_for_preview`). `lossy` (#101) travels
                // to the frontend for the "via …" notice.
                let (content, lossy) = crate::plugins::decode_for_preview(bytes);

                // 3) Instantiate + render (synchronous, WASM) in
                // spawn_blocking.
                let mime_owned = mime.to_owned();
                let output = crate::blocking::spawn_blocking(move || -> Result<String, Error> {
                    let runtime = shared_runtime().map_err(|_| Error::Internal { panic: false })?;
                    let mut inst = runtime
                        .instantiate(&wasm, caps)
                        .map_err(|_| Error::Internal { panic: false })?;
                    // P2 Task 4a: delivers `[config]` ALREADY resolved to
                    // the previewer, same criterion as `plugin_run_command`
                    // (via `PluginRegistry::run_command`, Task 3).
                    inst.set_settings(settings);
                    inst.render_preview(&mime_owned, &content)
                        .map_err(|_| Error::Internal { panic: false })
                })
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                Ok(norte_proto::methods::PluginPreviewResult {
                    preview: Some(norte_proto::methods::PluginPreview {
                        plugin_id: id,
                        plugin_name: name,
                        output,
                        lossy,
                    }),
                })
            }
            Self::Remote(r) => r.plugin_preview(path).await,
        }
    }

    /// Previews `path` WITH STYLE (G3a, ADR 0037): the twin of
    /// [`Self::plugin_preview`] that returns lines of spans instead of a
    /// flat string. Same steps 1 (resolve) and 2 (read bytes) as
    /// `plugin_preview` — their errors PROPAGATE the same way (an
    /// unreadable file is still an honest failure). It differs at step 3
    /// (execution): it invokes `render-styled` instead of `render`, and
    /// —unlike `plugin_preview`— ANY runtime failure AT THAT STEP (a trap,
    /// a guest logic error, or the ADR 0037 table's limits exceeded via
    /// `RuntimeError::StyledPreviewTooLarge`) degrades to `Ok(None)`
    /// instead of propagating: the styled preview is an ENRICHMENT over the
    /// plain one, it must never block the file — the caller falls back to
    /// [`Self::plugin_preview`] (plain), which in turn falls back to the raw
    /// view if none applies either.
    ///
    /// Responsibility boundary over `SpanWire::role` (ADR 0037 decision 3,
    /// amendment): it travels UNVALIDATED from here. `norte-core` is
    /// headless and does NOT depend on `norte-theme` (owner of the closed
    /// `Role` set); adding that structural dependency just to validate a
    /// `String` that's already size-capped anyway (wire limits, applied in
    /// `render_styled_preview`) isn't justified (rule 8) when the
    /// FRONTEND —which does know the theme and is the one that PAINTS— is
    /// the only one that can resolve a `role` to a color, and therefore the
    /// only place where an unknown name has an operable meaning (`None`, no
    /// color) instead of inert data. An unrecognized `role` must never be
    /// used by a frontend as a lookup key without first going through
    /// `norte_theme::Role::from_kebab_requestable` (see
    /// `norte-frontend::viewer::Viewer::with_plugin_preview_styled`, which
    /// is where that happens). `_requestable` and not bare `from_kebab`:
    /// since the 2026-09-11 spec, the vocabulary a plugin can name is that
    /// of MEANING, not chrome or window state.
    ///
    /// # Errors
    /// Same as [`Self::plugin_preview`] for resolution/reading; never from
    /// a guest EXECUTION failure (see above: degrades to `Ok(None)`).
    pub async fn plugin_preview_styled(
        &self,
        path: &VPath,
        columns: Option<u32>,
    ) -> Result<Option<norte_proto::methods::PluginPreviewStyled>, Error> {
        match self {
            Self::Embedded(engine) => {
                // 1) Resolve the previewer (discover = IO) in spawn_blocking.
                let dir = crate::connect::config_dir();
                let mime = crate::plugins::guess_mimetype(path);
                let resolved = crate::blocking::spawn_blocking(
                    move || -> Result<Option<crate::plugins::ResolvedPreviewer>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        Ok(reg.resolve_previewer(mime))
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                let Some((id, name, wasm, caps, settings)) = resolved else {
                    return Ok(None);
                };

                // 2) Read the CAPPED bytes via the engine (async), same as
                // `plugin_preview`. An unreadable file propagates honestly.
                let range = ByteRange {
                    offset: 0,
                    len: Some(crate::plugins::PREVIEW_MAX_BYTES),
                };
                let mut stream = engine.read(path, Some(range)).await?;
                let mut bytes: Vec<u8> = Vec::new();
                while let Some(chunk) = stream.next().await {
                    bytes.extend_from_slice(&chunk?);
                    if bytes.len() as u64 >= crate::plugins::PREVIEW_MAX_BYTES {
                        break;
                    }
                }
                let cap = usize::try_from(crate::plugins::PREVIEW_MAX_BYTES).unwrap_or(usize::MAX);
                bytes.truncate(cap.min(bytes.len()));
                let (content, lossy) = crate::plugins::decode_for_preview(bytes);

                // 3) Instantiate + `render-styled` (synchronous, WASM) in
                // spawn_blocking. Any `RuntimeError` here (trap, guest, or
                // an exceeded limit) degrades to `Ok(None)` — see rustdoc.
                let mime_owned = mime.to_owned();
                let outcome = crate::blocking::spawn_blocking(move || {
                    let runtime = shared_runtime()?;
                    let mut inst = runtime.instantiate(&wasm, caps)?;
                    inst.set_settings(settings);
                    inst.render_styled_preview(
                        &mime_owned,
                        &content,
                        crate::plugins::clamp_preview_columns(columns),
                    )
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?;

                let lines = match outcome {
                    Ok(lines) => lines,
                    Err(e) => {
                        tracing::debug!(
                            plugin = %id,
                            error = %e,
                            "render-styled failed: falling back to plugin_preview (plain)"
                        );
                        return Ok(None);
                    }
                };
                Ok(Some(norte_proto::methods::PluginPreviewStyled {
                    plugin_id: id,
                    plugin_name: name,
                    lines: crate::plugins::to_wire_lines(lines),
                    lossy,
                }))
            }
            Self::Remote(r) => r.plugin_preview_styled(path, columns).await,
        }
    }

    /// `plugin.thumbnail` (ADR 0107): `path`'s thumbnail from the first
    /// consented thumbnail plugin whose mimetype matches, or `None` if
    /// there's none or the one there is couldn't — cosmetic and fail-soft,
    /// like the styled preview. Embedded: resolve and read here, run the
    /// guest in `spawn_blocking`. Remote: the daemon does the same.
    ///
    /// # Errors
    /// Those of reading the file; never from a guest failure.
    pub async fn plugin_thumbnail(
        &self,
        path: &VPath,
        max_edge: u32,
    ) -> Result<Option<norte_proto::methods::PluginThumbnail>, Error> {
        match self {
            Self::Embedded(engine) => {
                let dir = crate::connect::config_dir();
                let mime = crate::plugins::guess_mimetype(path);
                let resolved = crate::blocking::spawn_blocking(
                    move || -> Result<Option<crate::plugins::ResolvedPreviewer>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        Ok(reg.resolve_thumbnailer(mime))
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                let Some((id, name, wasm, caps, settings)) = resolved else {
                    return Ok(None);
                };
                let range = ByteRange {
                    offset: 0,
                    len: Some(crate::plugins::THUMBNAIL_MAX_BYTES),
                };
                let mut stream = engine.read(path, Some(range)).await?;
                let mut bytes: Vec<u8> = Vec::new();
                while let Some(chunk) = stream.next().await {
                    bytes.extend_from_slice(&chunk?);
                    if bytes.len() as u64 >= crate::plugins::THUMBNAIL_MAX_BYTES {
                        break;
                    }
                }
                let cap =
                    usize::try_from(crate::plugins::THUMBNAIL_MAX_BYTES).unwrap_or(usize::MAX);
                bytes.truncate(cap.min(bytes.len()));
                let outcome = crate::blocking::spawn_blocking(move || {
                    let runtime = shared_runtime()?;
                    let mut inst = runtime.instantiate_thumbnail(&wasm, caps)?;
                    inst.set_settings(settings);
                    inst.render_thumbnail(mime, &bytes, max_edge)
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?;
                let thumb = match outcome {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::debug!(plugin = %id, error = %e, "thumbnail failed: no thumbnail");
                        return Ok(None);
                    }
                };
                Ok(Some(norte_proto::methods::PluginThumbnail {
                    plugin_id: id,
                    plugin_name: name,
                    mimetype: thumb.mimetype.to_owned(),
                    bytes: thumb.bytes,
                    width: thumb.width,
                    height: thumb.height,
                }))
            }
            Self::Remote(r) => r.plugin_thumbnail(path, max_edge).await,
        }
    }

    /// Decorates `paths` (G3b, ADR 0037 decision 2): the OVERLAY of ALL
    /// APPROVED and ENABLED `decorator` plugins ([`crate::PluginRegistry::
    /// resolve_decorators`], plural — unlike the previewer, which picks the
    /// first one), batched — ONE call per plugin over the WHOLE page, never
    /// entry by entry. `paths` are the CURRENT page's VISIBLE paths, in
    /// listing order; each `PluginDecorations::decorations` is POSITIONAL
    /// 1:1 with `paths`. The `entries` crossing into the guest are
    /// BASENAMES (`crate::plugins::paths_to_basenames`) — a decorator sees
    /// each entry's name, not where it lives in the tree (privacy, see that
    /// function's rustdoc).
    ///
    /// Fail-closed PER PLUGIN, never for the whole batch: if a plugin fails
    /// to instantiate, its runtime traps, or it returns a length that
    /// doesn't match `paths.len()` (contract violation,
    /// `crate::plugins::decorations_to_wire_checked`), THAT plugin is
    /// OMITTED from the result (with a local-log notice) — the rest of the
    /// plugins and the rest of the page still get painted, same
    /// per-plugin degradation criterion as
    /// `plugin_preview`/`plugin_preview_styled` (an enrichment must never
    /// block the listing). An empty `paths` returns `Ok(vec![])` without
    /// resolving the catalog (nothing to decorate).
    ///
    /// # Errors
    /// Only from `Backend`'s own INFRASTRUCTURE failures (I/O discovering
    /// the catalog, a real `spawn_blocking` panic) — never from an
    /// individual plugin failing (it degrades, see above).
    pub async fn plugin_decorate(
        &self,
        paths: &[VPath],
        kinds: &[norte_proto::EntryKind],
    ) -> Result<Vec<norte_proto::methods::PluginDecorations>, Error> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let entries = crate::plugins::paths_to_entries(paths, kinds);
                let expected_len = paths.len();
                let plugins = crate::blocking::spawn_blocking(
                    move || -> Result<Vec<norte_proto::methods::PluginDecorations>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        let runtime = shared_runtime()
                            .map_err(|_| Error::Internal { panic: false })?;
                        let mut out = Vec::new();
                        for ((id, _name, wasm, caps, settings), slot) in reg.resolve_decorators() {
                            let Ok(mut inst) = runtime.instantiate_decorator(&wasm, caps) else {
                                tracing::warn!(
                                    plugin = %id,
                                    "decorator: failed to instantiate, omitted from the batch"
                                );
                                continue;
                            };
                            inst.set_settings(settings);
                            let Ok(raw) = inst.decorate(&entries) else {
                                tracing::warn!(
                                    plugin = %id,
                                    "decorator: failed to run decorate, omitted from the batch"
                                );
                                continue;
                            };
                            let Some(decorations) =
                                crate::plugins::decorations_to_wire_checked(raw, expected_len)
                            else {
                                tracing::warn!(
                                    plugin = %id,
                                    "decorator: length doesn't match the positional contract, omitted from the batch"
                                );
                                continue;
                            };
                            out.push(norte_proto::methods::PluginDecorations {
                                plugin_id: id,
                                slot: crate::plugins::slot_to_wire(slot),
                                decorations,
                            });
                        }
                        Ok(out)
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                Ok(plugins)
            }
            Self::Remote(r) => r.plugin_decorate(paths, kinds).await,
        }
    }

    /// The rename plan a `renamer` plugin PROPOSES (C3, ADR 0095) for
    /// `names` in `dir`: the same result shape as [`Self::ai_rename_plan`],
    /// which is what lets frontends review and run it through the path they
    /// already have.
    ///
    /// If the guest refuses it isn't an error: empty `entries` and
    /// `refused` with its sentence, already masked and capped (#332).
    ///
    /// # Errors
    /// [`Error::NotFound`] if that plugin/renamer isn't consented to;
    /// [`Error::Io`] if the guest doesn't run. On `Remote`, whatever the
    /// daemon answers.
    pub async fn plugin_rename_plan(
        &self,
        plugin_id: &str,
        renamer_id: &str,
        dir: &VPath,
        names: &[String],
    ) -> Result<norte_proto::methods::AiRenamePlanResult, Error> {
        match self {
            Self::Embedded(_) => {
                let cfg = crate::connect::config_dir();
                let (plugin_id, renamer_id) = (plugin_id.to_owned(), renamer_id.to_owned());
                let (dir, names) = (dir.clone(), names.to_vec());
                crate::blocking::spawn_blocking(move || {
                    let reg = crate::PluginRegistry::discover(&cfg)
                        .map_err(|_| Error::Io { retryable: false })?;
                    let Some(resolved) = reg.resolve_renamer(&plugin_id, &renamer_id) else {
                        return Err(Error::NotFound);
                    };
                    let (runtime, _) = process_columns()?;
                    // Embedded: whoever asks is the person, the same case
                    // as `Actor::User` on the daemon — climbs up to the
                    // marker.
                    match crate::plugins::run_rename_plan(
                        runtime,
                        resolved,
                        &renamer_id,
                        Some(&dir),
                        true,
                        &names,
                    ) {
                        crate::plugins::RenamePlanOutcome::Plan(entries) => {
                            Ok(norte_proto::methods::AiRenamePlanResult {
                                entries,
                                refused: None,
                            })
                        }
                        // Refusing isn't an error (#332): it's an empty
                        // plan with a reason, and the reason is third-party
                        // text that gets masked and capped before being
                        // shown.
                        crate::plugins::RenamePlanOutcome::Refused(reason) => {
                            tracing::info!(plugin = %plugin_id, reason = %reason, "renamer: refused");
                            Ok(norte_proto::methods::AiRenamePlanResult {
                                entries: Vec::new(),
                                refused: Some(crate::plugins::guest_reason(&reason)),
                            })
                        }
                        crate::plugins::RenamePlanOutcome::Failed => {
                            Err(Error::Io { retryable: false })
                        }
                    }
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            Self::Remote(r) => {
                r.plugin_rename_plan(plugin_id, renamer_id, dir, names)
                    .await
            }
        }
    }

    /// The ORGANIZE plan an `organizer` plugin proposes (phase 8) for
    /// `dir`: the same result shape as [`Self::ai_organize_plan`], and for
    /// the same reason as its rename twin — a plugin plan and a model plan
    /// are reviewed and applied through the path frontends already have.
    ///
    /// The plan's token also comes from here, bound to `dir`: it's the only
    /// thing [`Self::organize`] accepts, and computing it in the frontend
    /// would put the digest in two places.
    ///
    /// **`names` is the operand, and empty here means empty** — not "all",
    /// which is what it means in [`Self::ai_organize_plan`]. The asymmetry
    /// isn't an oversight: the model gets the listing because the ENGINE
    /// lists the directory for it, and a plugin cannot list anything
    /// (rule 9), so whatever it isn't given doesn't exist for it. An
    /// organizer called with an empty list correctly answers that it moves
    /// nothing.
    ///
    /// # Errors
    /// [`Error::NotFound`] if that plugin/organizer isn't consented to;
    /// [`Error::InvalidPath`] if the plugin proposes a destination that
    /// escapes the directory —which brings down the WHOLE plan, not just
    /// the move—; [`Error::Io`] if the guest doesn't run. On `Remote`,
    /// whatever the daemon answers.
    pub async fn plugin_organize_plan(
        &self,
        plugin_id: &str,
        organizer_id: &str,
        dir: &VPath,
        names: &[String],
    ) -> Result<norte_proto::methods::AiOrganizePlanResult, Error> {
        match self {
            Self::Embedded(_) => {
                let cfg = crate::connect::config_dir();
                let (plugin_id, organizer_id) = (plugin_id.to_owned(), organizer_id.to_owned());
                let (dir, names) = (dir.clone(), names.to_vec());
                crate::blocking::spawn_blocking(move || {
                    let reg = crate::PluginRegistry::discover(&cfg)
                        .map_err(|_| Error::Io { retryable: false })?;
                    let Some(resolved) = reg.resolve_organizer(&plugin_id, &organizer_id) else {
                        return Err(Error::NotFound);
                    };
                    let (runtime, _) = process_columns()?;
                    match crate::plugins::run_organize_plan(
                        runtime,
                        resolved,
                        &organizer_id,
                        Some(&dir),
                        true,
                        &names,
                    ) {
                        crate::plugins::OrganizePlanOutcome::Plan(moves) => {
                            let plan_hash = if moves.is_empty() {
                                None
                            } else {
                                Some(crate::organize::plan_hash(&dir, &moves)?)
                            };
                            Ok(norte_proto::methods::AiOrganizePlanResult {
                                moves,
                                refused: None,
                                plan_hash,
                            })
                        }
                        crate::plugins::OrganizePlanOutcome::Refused(reason) => {
                            tracing::info!(plugin = %plugin_id, reason = %reason, "organizer: refused");
                            Ok(norte_proto::methods::AiOrganizePlanResult {
                                moves: Vec::new(),
                                refused: Some(crate::plugins::guest_reason(&reason)),
                                plan_hash: None,
                            })
                        }
                        // Proposing a write outside the directory isn't an
                        // I/O failure: it's exactly `InvalidPath`.
                        crate::plugins::OrganizePlanOutcome::Escapes => Err(Error::InvalidPath),
                        crate::plugins::OrganizePlanOutcome::Failed => {
                            Err(Error::Io { retryable: false })
                        }
                    }
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            Self::Remote(r) => {
                r.plugin_organize_plan(plugin_id, organizer_id, dir, names)
                    .await
            }
        }
    }

    /// Values of column `column_id` for `paths` (G3b, ADR 0037 decision 2):
    /// unlike [`Self::plugin_decorate`] (overlay of ALL decorators), at most
    /// ONE `columns` plugin supplies `column_id`
    /// ([`crate::PluginRegistry::resolve_columns`], first-match). Same
    /// entry contract (basenames) as `plugin_decorate`. Fail-closed: if the
    /// ONLY plugin supplying the column fails to instantiate, traps, or
    /// breaks the positional contract, the result is a `None` vector the
    /// size of `paths` (an empty cell for the whole page) instead of an
    /// error — a column with no data is a lost enrichment, not a listing
    /// failure. An empty `paths` returns `Ok(vec![])`.
    ///
    /// # Errors
    /// Only from INFRASTRUCTURE failures (same as
    /// [`Self::plugin_decorate`]).
    pub async fn plugin_column_values(
        &self,
        plugin_id: &str,
        column_id: &str,
        paths: &[VPath],
    ) -> Result<Vec<Option<String>>, Error> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let entries = crate::plugins::paths_to_basenames(paths);
                let expected_len = paths.len();
                // The page's parent directory: the location the guest can
                // read if it was approved (ADR 0057).
                let location = paths.first().and_then(norte_proto::VPath::parent);
                let column_id_owned = column_id.to_owned();
                let plugin_id_owned = plugin_id.to_owned();
                let values = crate::blocking::spawn_blocking(
                    move || -> Result<Vec<Option<String>>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        // THAT plugin or none (#120): two consented plugins
                        // declaring the same bare id cannot stand in for
                        // each other.
                        let Some(resolved) =
                            reg.resolve_columns_of(Some(&plugin_id_owned), &column_id_owned)
                        else {
                            return Ok(vec![None; expected_len]);
                        };
                        // The engine and the pool are PROCESS-wide, like on
                        // the daemon (#224). Building a `PluginRuntime` per
                        // call doesn't just compile the engine again: it
                        // starts and stops an epoch-ticker thread per
                        // painted page.
                        let (runtime, pool) = process_columns()?;
                        // SAME function as the daemon: the location
                        // capability can't mean one thing here and another
                        // there.
                        Ok(pool.column_values(
                            runtime,
                            resolved,
                            &column_id_owned,
                            location.as_ref(),
                            // Embedded: whoever's looking is the person who
                            // opened the panel, the same case as
                            // `Actor::User`.
                            true,
                            &entries,
                            expected_len,
                        ))
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                Ok(values)
            }
            Self::Remote(r) => r.plugin_column_values(plugin_id, column_id, paths).await,
        }
    }

    /// The frame a plugin paints for its panel (0.74.0, phase 3).
    ///
    /// ALSO embedded, not daemon-only: the same `ntc` can't show a git
    /// panel when there's a daemon and an empty box when there isn't. What
    /// runs is the same function as the daemon
    /// (`render_panel_blocking`), with the same mint and the same location
    /// session lifetime.
    ///
    /// `climb` is `true` here because embedded, whoever's looking is the
    /// person who opened the panel — the daemon's `Actor::User` case—, so
    /// climbing to find the project root (`.git`) is correct.
    ///
    /// Fail-soft: with no plugin to paint it, or if the guest fails, there's
    /// no frame.
    ///
    /// Embedded costs one catalog DISCOVERY per call —reading the plugins
    /// directory and parsing each `plugin.toml`—, where the daemon has its
    /// registry in memory. Paid per context change (a different directory,
    /// row, or size), not per frame, because a signature already requested
    /// or that already came back empty isn't repeated.
    ///
    /// # Errors
    /// Protocol taxonomy. With the daemon down,
    /// `ProviderUnavailable{retryable:true}`; embedded, `Io{retryable:false}`
    /// if the catalog can't be read and `Internal` if the wasm engine
    /// doesn't start or the blocking task panics.
    pub async fn plugin_panel_render(
        &self,
        params: norte_proto::methods::PluginPanelRenderParams,
    ) -> Result<Option<norte_proto::methods::PanelFrame>, Error> {
        match self {
            Self::Embedded(_) => {
                let dir_cfg = crate::connect::config_dir();
                crate::blocking::spawn_blocking(
                    move || -> Result<Option<norte_proto::methods::PanelFrame>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir_cfg)
                            .map_err(|_| Error::Io { retryable: false })?;
                        let Some(resolved) = reg.resolve_panel(&params.plugin_id, &params.kind)
                        else {
                            return Ok(None);
                        };
                        // The engine is PROCESS-wide, like on the daemon
                        // (#224): building one per repaint compiles the
                        // engine again and starts an epoch thread per
                        // frame.
                        let (runtime, _) = process_columns()?;
                        let context = norte_plugin_host::panel_iface::PanelContext {
                            cols: params.cols,
                            rows: params.rows,
                            lang: params.lang.clone(),
                            cursor_name: params.cursor_name.clone(),
                        };
                        let event = crate::plugins::panel_event_to_host(&params.event);
                        let state = params.state.clone().unwrap_or_default();
                        match crate::plugins::render_panel_blocking(
                            runtime,
                            resolved,
                            &crate::plugins::PanelCall {
                                dir: &params.dir,
                                climb: true,
                                kind: &params.kind,
                                context: &context,
                                state: &state,
                                event: &event,
                            },
                        ) {
                            Ok((id, frame)) => {
                                Ok(Some(crate::plugins::panel_frame_to_wire(id, frame)))
                            }
                            // A guest that fails leaves the panel with no
                            // frame, not the screen with an error.
                            Err(_) => Ok(None),
                        }
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            Self::Remote(r) => r.plugin_panel_render(params).await,
        }
    }

    /// `[config]` schema + effective values for `id` (0.28.0, G3c, ADR
    /// 0037): closes the P2 debt (`settings` was host-only). An unknown id
    /// returns `keys: []` (same lenient criterion as [`Self::plugins_list`]
    /// with an empty catalog). Embedded: EPHEMERAL per-call registry in
    /// `spawn_blocking` (rule 2), same cost criterion as
    /// [`Self::plugins_list`].
    ///
    /// # Errors
    /// Protocol taxonomy; with the daemon down,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn plugin_get_config(
        &self,
        id: &str,
    ) -> Result<norte_proto::methods::PluginGetConfigResult, Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let keys = crate::blocking::spawn_blocking(
                    move || -> Result<Vec<norte_proto::methods::PluginConfigKeyWire>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        Ok(reg
                            .config_keys(&id)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|(key, spec, value)| {
                                crate::plugins::config_key_to_wire(key, &spec, value)
                            })
                            .collect())
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                Ok(norte_proto::methods::PluginGetConfigResult { keys })
            }
            Self::Remote(r) => r.plugin_get_config(id).await,
        }
    }

    /// A plugin's help page (H3e, 0.34.0), already capped by the host to
    /// [`norte_proto::methods::PLUGIN_HELP_MAX_BYTES`]. Embedded: EPHEMERAL
    /// per-call registry in `spawn_blocking` (rule 2), same cost criterion
    /// as [`Self::plugin_get_config`].
    ///
    /// The returned markdown is NOT masked: it carries verbatim whatever
    /// terminal hazards the plugin wrote (ESC, C0 controls, bidi
    /// overrides). It's parsed with `norte_help::parse_untrusted`, which
    /// masks while building the model; it's never painted or logged raw.
    ///
    /// # Errors
    /// Protocol taxonomy; with the daemon down,
    /// `ProviderUnavailable{retryable:true}`.
    ///
    /// An `id` not in the catalog does NOT give the same error on both
    /// paths, worth knowing: embedded is [`Error::NotFound`], while the
    /// daemon answers `INVALID_PARAMS` with no taxonomy in `data`, which
    /// `to_taxonomy` delivers as `Internal{panic:false}`. A frontend that
    /// wants to tell "unknown plugin" apart from "the daemon had a problem"
    /// cannot do it over the wire; the right move in both cases is to treat
    /// it as "no page" and keep painting.
    pub async fn plugin_help(
        &self,
        id: &str,
    ) -> Result<norte_proto::methods::PluginHelpResult, Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                crate::blocking::spawn_blocking(
                    move || -> Result<norte_proto::methods::PluginHelpResult, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        reg.help_of(&id).ok_or(Error::NotFound)
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            Self::Remote(r) => r.plugin_help(id).await,
        }
    }

    /// Persists ONE `[config]` value for `id`, validated against the
    /// manifest's schema (0.28.0, G3c, ADR 0037). Embedded: EPHEMERAL
    /// per-call registry + `PluginRegistry::set_config` (validates,
    /// persists, re-resolves), ALL in `spawn_blocking`.
    ///
    /// # Security invariant (defense in depth)
    /// Same as [`Self::plugins_set_approval`]: the "human only" gate lives
    /// in the wire layer (the daemon with `Actor`); this embedded `Backend`
    /// is the human frontend's API and receives no `Actor`.
    ///
    /// # Errors
    /// [`Error::NotFound`] if `id` is unknown; protocol taxonomy for the
    /// rest (an invalid value or unknown key arrives as a generic error —
    /// the caller must validate client-side BEFORE calling, which is what
    /// the TUI/GUI do).
    pub async fn plugin_set_config(&self, id: &str, key: &str, value: &str) -> Result<(), Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let key = key.to_owned();
                let value = value.to_owned();
                crate::blocking::spawn_blocking(move || -> Result<(), Error> {
                    let mut reg = crate::PluginRegistry::discover(&dir)
                        .map_err(|_| Error::Io { retryable: false })?;
                    reg.set_config(&id, &key, &value)
                        .map_err(|e| config_set_error_to_taxonomy(&e))
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            Self::Remote(r) => r.plugin_set_config(id, key, value).await,
        }
    }
}

/// Maps a [`crate::PluginRegistry::set_config`] failure to protocol
/// taxonomy (EMBEDDED mode): an unknown id is honestly `NotFound` (same
/// criterion as [`Backend::plugins_set_approval`]); the rest (unknown key /
/// invalid value / I/O) is REDACTED to `Internal` — the caller (TUI/GUI)
/// validates client-side BEFORE calling, so this path is only hit by a
/// value that slipped past that barrier (a backstop, not primary UX).
fn config_set_error_to_taxonomy(e: &crate::plugins::PluginConfigSetError) -> Error {
    use crate::plugins::PluginConfigSetError as E;
    match e {
        E::Unknown(_) => Error::NotFound,
        E::UnknownKey(_) | E::Invalid(_) | E::Io(_) => Error::Internal { panic: false },
    }
}

/// Maps a plugin's execution failure to protocol taxonomy (EMBEDDED mode).
/// A runtime failure is REDACTED to `Internal` (never the raw `Display`,
/// which can carry the `.wasm` path or wasmtime details): the same policy
/// the daemon applies on the wire (security-reviewer M4-P4).
fn run_error_to_taxonomy(e: &crate::plugins::PluginRunError) -> Error {
    use crate::plugins::PluginRunError as E;
    match e {
        // Nonexistent id / no binary / not consented: the requested node
        // isn't available to run. `NotFound` is the honest category and
        // does NOT reveal paths (these variants' messages carry the id,
        // not the path).
        E::Unknown(_) | E::NoBinary(_) | E::NotApproved(_) | E::Disabled(_) => Error::NotFound,
        // A kind with no `command`: that plugin doesn't have the
        // capability being asked of it, which is what `Unsupported` says
        // (the daemon returns it as `INVALID_PARAMS`, with the same
        // meaning).
        E::NotRunnable(_) => Error::Unsupported,
        // Runtime failure: redacted, no detail leaked to the frontend.
        E::Runtime(_) => Error::Internal { panic: false },
    }
}

/// THIS process's plugin runtime, for everything that isn't columns:
/// commands, viewers, thumbnails, decorators (ADR 0141).
///
/// One per call —as it used to be— threw away the already-compiled plugins
/// with it: every F3 recompiled the syntax-highlighting one, seconds. It's
/// the same runtime as [`process_columns`]'s, which already lived for the
/// whole process.
///
/// # Errors
/// If the wasmtime engine could not be configured on this platform.
fn shared_runtime()
-> Result<&'static norte_plugin_host::PluginRuntime, norte_plugin_host::RuntimeError> {
    process_columns().map(|(runtime, _)| runtime).map_err(|_| {
        norte_plugin_host::RuntimeError::Instantiate("wasm engine not available".into())
    })
}

/// The PROCESS's WASM engine and column-instance pool, for the embedded
/// backend (#224).
///
/// The daemon hangs them off its `Shared`; there's nowhere here, because
/// the embedded backend rediscovers the registry on every call and has no
/// state of its own. A `static` is what makes the same binary's two
/// halves —embedded `ntc` and `ntc --daemon`— pay the same for the same
/// page.
///
/// Built once and never released: `PluginRuntime` owns the ticker thread
/// that advances the engine's epoch, i.e. the clock a looping guest traps
/// against (hard rule 3). A runtime per call started and stopped that
/// thread per painted page.
///
/// # Errors
/// If the wasmtime engine can't be configured on this platform. The
/// failure is remembered: retrying it on every page would mean paying for
/// the failure N times to reach the same place.
///
/// THIS process's wasm engine and column pool.
///
/// Used by columns and, since phase 3, by plugin panels, which keep only
/// the engine. Building a `PluginRuntime` per call compiles the engine
/// again and starts an epoch thread per paint. The name says "columns"
/// because that's who got here first.
fn process_columns() -> Result<
    (
        &'static norte_plugin_host::PluginRuntime,
        &'static crate::plugins::ColumnPool,
    ),
    Error,
> {
    struct Columns {
        runtime: norte_plugin_host::PluginRuntime,
        pool: crate::plugins::ColumnPool,
    }
    static COLUMNS: std::sync::OnceLock<Option<Columns>> = std::sync::OnceLock::new();
    let cell = COLUMNS.get_or_init(|| {
        norte_plugin_host::PluginRuntime::new()
            .ok()
            .map(|runtime| Columns {
                runtime,
                pool: crate::plugins::ColumnPool::default(),
            })
    });
    match cell {
        Some(c) => Ok((&c.runtime, &c.pool)),
        None => Err(Error::Internal { panic: false }),
    }
}
