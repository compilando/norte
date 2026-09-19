//! El área de plugins de [`Backend`](super::Backend) (M4-P3/P4, ADR 0037,
//! ADR 0095): catálogo y aprobación/activación/desinstalación, ejecución de
//! comandos, preview (plano y con estilo), miniaturas, decoradores, columnas,
//! paneles, renamers/organizers de plugin, y `[config]`.

use futures::StreamExt;
use norte_proto::{ByteRange, Error, VPath};

use super::Backend;

impl Backend {
    /// Catálogo de plugins descubiertos + su estado aprobado/activado (M4-P3).
    /// Embebido: descubre de [`crate::connect::config_dir`] bajo demanda (el
    /// estado vive en `plugins-state.toml`, no en memoria — no hay que retener
    /// un registro entre llamadas); remoto: `plugin.list` contra el daemon.
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn plugins_list(&self) -> Result<norte_proto::methods::PluginListResult, Error> {
        match self {
            Self::Embedded(_) => {
                // Registro EFÍMERO por-llamada (I/O sync → spawn_blocking,
                // regla 2). La verdad vive en el fichero de estado.
                let dir = crate::connect::config_dir();
                crate::blocking::spawn_blocking(move || {
                    crate::PluginRegistry::discover(&dir).map(|r| r.list())
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
                .map_err(|_| Error::Io { retryable: false })
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugins_list().await,
        }
    }

    /// Aprueba (o revoca) las capabilities de un plugin (M4-P3). Embebido:
    /// discover + `set_approval` + persiste, todo en `spawn_blocking`.
    ///
    /// # Invariante de seguridad (defensa en profundidad)
    /// El gate "SOLO un humano aprueba" vive en la capa WIRE (el daemon, que ata
    /// la conexión a un [`crate::journal::Actor`]). Este `Backend` embebido es la
    /// API in-proceso del frontend HUMANO y NO recibe `Actor`: aprobar por aquí
    /// es, por construcción, un acto del humano. Si algún día se cablea un bridge
    /// de agente a un `Backend` embebido, habría que replicar el gate AQUÍ (no
    /// existe hoy y no debe introducirse sin ese gate).
    ///
    /// # Errors
    /// [`Error::NotFound`] si el id es desconocido; taxonomía del protocolo en
    /// lo demás.
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
                let esperado = expected_digest.map(ToOwned::to_owned);
                let applied = crate::blocking::spawn_blocking(move || {
                    let mut reg = crate::PluginRegistry::discover(&dir)?;
                    // La comprobación de #282 importa MÁS aquí que en el
                    // daemon: éste descubre el catálogo una vez al arrancar,
                    // así que su ventana está cerrada por accidente. Este
                    // `discover` corre en CADA llamada, o sea que el
                    // `plugin.toml` que se lee ahora puede no ser el que el
                    // humano leyó hace un momento.
                    // Un id desconocido NO es un ancla rancia: los dos dan
                    // `None` aquí, y confundirlos daría «el manifiesto
                    // cambió» a quien nombró un plugin que no existe.
                    if approved
                        && let Some(actual) = reg.manifest_digest(&id)
                        && let Some(esperado) = &esperado
                        && &actual != esperado
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
                    // El manifiesto cambió bajo los pies: no se concede, y se
                    // dice con la variante que significa exactamente eso —la
                    // misma que `session.put` con una revisión rancia—. No
                    // `Exists`, que se lee como «el destino ya está» y aquí no
                    // significa nada.
                    None => Err(Error::Conflict {
                        conflict: norte_proto::ConflictKind::StaleRevision,
                    }),
                }
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugins_set_approval(id, approved, expected_digest).await,
        }
    }

    /// Activa/desactiva un plugin ya aprobado (M4-P3). Semántica idéntica a
    /// [`Self::plugins_set_approval`].
    ///
    /// # Invariante de seguridad (defensa en profundidad)
    /// Igual que [`Self::plugins_set_approval`]: el gate "solo humano" vive en la
    /// capa wire (daemon con `Actor`); este Backend embebido es la API del
    /// frontend humano y no recibe `Actor`. Un futuro bridge de agente a un
    /// Backend embebido tendría que replicar el gate aquí.
    ///
    /// # Errors
    /// [`Error::NotFound`] si el id es desconocido; taxonomía del protocolo en
    /// lo demás.
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
            #[cfg(unix)]
            Self::Remote(r) => r.plugins_set_enabled(id, enabled).await,
        }
    }

    /// Desinstala un plugin (0.71.0, ADR 0104): borra su directorio y deja su
    /// estado apagado y sin aprobar. Devuelve si tenía consentimiento, que
    /// es lo que acaba de dejar de existir.
    ///
    /// Embebido: [`crate::plugins::uninstall`] en `spawn_blocking`, lo mismo
    /// que hace la CLI. Remoto: `plugin.uninstall` contra el daemon, que
    /// además lo olvida en su registro en memoria.
    ///
    /// # Invariante de seguridad (defensa en profundidad)
    /// Igual que [`Self::plugins_set_approval`]: el gate «solo humano» vive
    /// en la capa wire.
    ///
    /// # Errors
    /// [`Error::NotFound`] si el id no es un id o no está instalado;
    /// [`Error::Io`] si el borrado o el estado fallan.
    pub async fn plugins_uninstall(&self, id: &str) -> Result<bool, Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let informe =
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
                Ok(informe.was_approved)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugins_uninstall(id).await.map(|r| r.was_approved),
        }
    }

    /// Ejecuta un comando de un plugin YA aprobado y activado (M4-P4) y devuelve
    /// su salida. Embebido: registro EFÍMERO por-llamada + un `PluginRuntime`
    /// nuevo, TODO en `spawn_blocking` (la instanciación compila WASM: pesada y
    /// síncrona, regla 2). Remoto: `plugin.run_command` contra el daemon.
    ///
    /// El coste de crear el runtime por-llamada se acepta igual que el registro
    /// efímero de [`Self::plugins_list`]: el modo embebido es un frontend humano
    /// puntual, no un servidor de plugins de alta frecuencia (ese es el daemon,
    /// que sí reutiliza un runtime compartido).
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`. Un fallo del runtime del plugin se
    /// entrega REDACTADO (`Internal`), sin filtrar rutas ni detalles internos.
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
                    let runtime = norte_plugin_host::PluginRuntime::new()
                        .map_err(|_| Error::Internal { panic: false })?;
                    reg.run_command(&runtime, &id, &command, &arg)
                        .map_err(|e| run_error_to_taxonomy(&e))
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_run_command(id, command, arg).await,
        }
    }

    /// Previsualiza `path` con el PRIMER previewer consentido cuyo mimetype
    /// (adivinado por extensión) case, o devuelve `preview: None` si ninguno
    /// aplica (el frontend cae a la vista cruda). Un archivo ilegible es un error
    /// honesto, no un preview vacío.
    ///
    /// Embebido: registro EFÍMERO por-llamada + resolución del previewer +
    /// `PluginRuntime` nuevo, con la lectura de bytes acotada intercalada. La
    /// resolución (discover, IO) y la ejecución (instancia WASM, síncrona) van en
    /// `spawn_blocking` (regla 2); la lectura de bytes usa el engine async entre
    /// medias. El coste del registro/runtime efímero se acepta igual que en
    /// [`Self::plugin_run_command`]: el modo embebido es un frontend humano
    /// puntual, no un servidor de plugins de alta frecuencia (ese es el daemon,
    /// que reutiliza un registro y un runtime compartidos). Remoto:
    /// `plugin.preview` contra el daemon.
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`. Un fallo del runtime del plugin se
    /// entrega REDACTADO (`Internal`), sin filtrar rutas ni detalles internos.
    pub async fn plugin_preview(
        &self,
        path: &VPath,
    ) -> Result<norte_proto::methods::PluginPreviewResult, Error> {
        match self {
            Self::Embedded(engine) => {
                // 1) Resolver el previewer (discover = IO) en spawn_blocking.
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

                // 2) Leer los bytes ACOTADOS vía el engine (async). Un archivo
                // ilegible es un error honesto que se propaga.
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

                // 2.5) §6.2 (#29): el previewer recibe TEXTO ya decodificado
                // por la detección del core — jamás bytes crudos sobre los que
                // asumir UTF-8 (lógica testeada en `plugins::decode_for_preview`).
                // `lossy` (#101) viaja al frontend para el aviso «via …».
                let (content, lossy) = crate::plugins::decode_for_preview(bytes);

                // 3) Instanciar + renderizar (síncrono, WASM) en spawn_blocking.
                let mime_owned = mime.to_owned();
                let output = crate::blocking::spawn_blocking(move || -> Result<String, Error> {
                    let runtime = norte_plugin_host::PluginRuntime::new()
                        .map_err(|_| Error::Internal { panic: false })?;
                    let mut inst = runtime
                        .instantiate(&wasm, caps)
                        .map_err(|_| Error::Internal { panic: false })?;
                    // P2 Task 4a: entrega `[config]` YA resuelto al previewer,
                    // mismo criterio que `plugin_run_command` (vía
                    // `PluginRegistry::run_command`, Task 3).
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
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_preview(path).await,
        }
    }

    /// Previsualiza `path` CON ESTILO (G3a, ADR 0037): el gemelo de
    /// [`Self::plugin_preview`] que devuelve líneas de spans en vez de una
    /// cadena plana. Mismos pasos 1 (resolver) y 2 (leer bytes) que
    /// `plugin_preview` — sus errores se PROPAGAN igual (un archivo
    /// ilegible sigue siendo un fallo honesto). Difiere en el paso 3
    /// (ejecución): invoca `render-styled` en vez de `render`, y —a
    /// diferencia de `plugin_preview`— CUALQUIER fallo del runtime EN ESE
    /// PASO (trap, error de lógica del guest, o los topes de la tabla ADR
    /// 0037 excedidos vía `RuntimeError::StyledPreviewTooLarge`) degrada a
    /// `Ok(None)` en vez de propagarse: el preview con estilo es un
    /// ENRIQUECIMIENTO sobre el plano, nunca debe bloquear el archivo — el
    /// caller cae a [`Self::plugin_preview`] (plano), que a su vez cae a la
    /// vista cruda si tampoco aplica ninguno.
    ///
    /// Límite de responsabilidad sobre `SpanWire::role` (ADR 0037 decisión
    /// 3, enmienda): viaja SIN VALIDAR desde aquí. `norte-core` es headless
    /// y NO depende de `norte-theme` (dueño del conjunto cerrado `Role`);
    /// añadir esa dependencia estructural solo para validar un `String` que
    /// de todos modos ya llega acotado en tamaño (topes del wire, aplicados
    /// en `render_styled_preview`) no se justifica (regla 8) cuando el
    /// FRONTEND —que sí conoce el tema y es quien PINTA— es el único que
    /// puede resolver un `role` a un color, y por tanto el único lugar
    /// donde un nombre desconocido tiene un significado operable (`None`,
    /// sin color) en vez de un dato inerte. Un `role` no reconocido nunca
    /// debe usarse por un frontend como clave de lookup sin pasar antes por
    /// `norte_theme::Role::from_kebab_requestable` (ver `norte-frontend::
    /// viewer::Viewer::with_plugin_preview_styled`, que es donde eso ocurre).
    /// `_requestable` y no `from_kebab` a secas: desde la spec 2026-09-11 el
    /// vocabulario que un plugin puede nombrar es el de SIGNIFICADO, no el
    /// cromo ni el estado de la ventana.
    ///
    /// # Errors
    /// Igual que [`Self::plugin_preview`] para resolución/lectura; jamás por
    /// un fallo de EJECUCIÓN del guest (ver arriba: degrada a `Ok(None)`).
    pub async fn plugin_preview_styled(
        &self,
        path: &VPath,
        columns: Option<u32>,
    ) -> Result<Option<norte_proto::methods::PluginPreviewStyled>, Error> {
        match self {
            Self::Embedded(engine) => {
                // 1) Resolver el previewer (discover = IO) en spawn_blocking.
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

                // 2) Leer los bytes ACOTADOS vía el engine (async), igual que
                // `plugin_preview`. Un archivo ilegible se propaga honesto.
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

                // 3) Instanciar + `render-styled` (síncrono, WASM) en
                // spawn_blocking. Cualquier `RuntimeError` aquí (trap, guest,
                // o tope excedido) degrada a `Ok(None)` — ver rustdoc.
                let mime_owned = mime.to_owned();
                let outcome = crate::blocking::spawn_blocking(move || {
                    let runtime = norte_plugin_host::PluginRuntime::new()?;
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
                            "render-styled falló: cae a plugin_preview (plano)"
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
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_preview_styled(path, columns).await,
        }
    }

    /// `plugin.thumbnail` (ADR 0107): la miniatura de `path` por el primer
    /// plugin de miniaturas consentido cuyo mimetype casa, o `None` si no
    /// hay ninguno o el que hay no supo — cosmético y fail-soft, como la
    /// preview con estilo. Embebido: resolver y leer aquí, correr el guest
    /// en `spawn_blocking`. Remoto: el daemon hace lo mismo.
    ///
    /// # Errors
    /// Los de la lectura del fichero; jamás por un fallo del guest.
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
                    let runtime = norte_plugin_host::PluginRuntime::new()?;
                    let mut inst = runtime.instantiate_thumbnail(&wasm, caps)?;
                    inst.set_settings(settings);
                    inst.render_thumbnail(mime, &bytes, max_edge)
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?;
                let thumb = match outcome {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::debug!(plugin = %id, error = %e, "thumbnail falló: sin miniatura");
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
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_thumbnail(path, max_edge).await,
        }
    }

    /// Decora `paths` (G3b, ADR 0037 decisión 2): la SUPERPOSICIÓN de TODOS
    /// los plugins `decorator` APROBADOS y ACTIVADOS ([`crate::PluginRegistry::
    /// resolve_decorators`], plural — a diferencia del previewer que elige
    /// el primero), batched — UNA llamada por plugin sobre la página
    /// ENTERA, nunca entrada por entrada. `paths` son las rutas VISIBLES de
    /// la página actual, en el orden en que se listan; cada
    /// `PluginDecorations::decorations` es POSICIONAL 1:1 con `paths`. Los
    /// `entries` que cruzan al guest son BASENAMES
    /// (`crate::plugins::paths_to_basenames`) — un decorator ve el
    /// nombre de cada entrada, no dónde vive en el árbol (privacidad, ver
    /// el rustdoc de esa función).
    ///
    /// Fail-closed POR PLUGIN, nunca por lote entero: si un plugin no
    /// instancia, su runtime trapea, o devuelve una longitud que no casa
    /// `paths.len()` (violación de contrato,
    /// `crate::plugins::decorations_to_wire_checked`), ESE plugin se
    /// OMITE del resultado (con aviso en el log local) — el resto de
    /// plugins y el resto de la página se pintan igual, mismo criterio de
    /// degradación por-plugin que `plugin_preview`/`plugin_preview_styled`
    /// (un enriquecimiento nunca debe bloquear el listado). `paths` vacío
    /// devuelve `Ok(vec![])` sin resolver el catálogo (nada que decorar).
    ///
    /// # Errors
    /// Solo por fallos de INFRAESTRUCTURA del propio `Backend` (I/O al
    /// descubrir el catálogo, panic real de `spawn_blocking`) — jamás por
    /// un plugin individual que falla (degrada, ver arriba).
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
                        let runtime = norte_plugin_host::PluginRuntime::new()
                            .map_err(|_| Error::Internal { panic: false })?;
                        let mut out = Vec::new();
                        for ((id, _name, wasm, caps, settings), slot) in reg.resolve_decorators() {
                            let Ok(mut inst) = runtime.instantiate_decorator(&wasm, caps) else {
                                tracing::warn!(
                                    plugin = %id,
                                    "decorator: fallo al instanciar, se omite del lote"
                                );
                                continue;
                            };
                            inst.set_settings(settings);
                            let Ok(raw) = inst.decorate(&entries) else {
                                tracing::warn!(
                                    plugin = %id,
                                    "decorator: fallo al ejecutar decorate, se omite del lote"
                                );
                                continue;
                            };
                            let Some(decorations) =
                                crate::plugins::decorations_to_wire_checked(raw, expected_len)
                            else {
                                tracing::warn!(
                                    plugin = %id,
                                    "decorator: longitud no casa el contrato posicional, se omite del lote"
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
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_decorate(paths, kinds).await,
        }
    }

    /// El plan de renombrado que PROPONE un plugin `renamer` (C3, ADR 0095)
    /// para `names` en `dir`: el mismo resultado que [`Self::ai_rename_plan`],
    /// que es lo que hace que los frontends lo revisen y ejecuten por el
    /// camino que ya tienen.
    ///
    /// Si el guest rehúsa no es un error: `entries` vacío y `refused` con
    /// su frase, ya enmascarada y acotada (#332).
    ///
    /// # Errors
    /// [`Error::NotFound`] si ese plugin/renamer no está consentido;
    /// [`Error::Io`] si el guest no corre. En `Remote`, lo que conteste el
    /// daemon.
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
                    let (runtime, _) = columnas_de_proceso()?;
                    // Embebido: quien pide es la persona, el mismo caso que
                    // `Actor::User` en el daemon — sube hasta el marcador.
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
                        // Rehusar no es un error (#332): es un plan vacío con
                        // motivo, y el motivo es texto de un tercero que se
                        // enmascara y acota antes de enseñarse.
                        crate::plugins::RenamePlanOutcome::Refused(frase) => {
                            tracing::info!(plugin = %plugin_id, motivo = %frase, "renamer: rehusó");
                            Ok(norte_proto::methods::AiRenamePlanResult {
                                entries: Vec::new(),
                                refused: Some(crate::plugins::guest_reason(&frase)),
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
            #[cfg(unix)]
            Self::Remote(r) => {
                r.plugin_rename_plan(plugin_id, renamer_id, dir, names)
                    .await
            }
        }
    }

    /// El plan de ORGANIZAR que propone un plugin `organizer` (fase 8) para
    /// `dir`: el mismo resultado que [`Self::ai_organize_plan`], y por la
    /// misma razón que su gemelo de renombrar — un plan de plugin y uno de
    /// modelo se revisan y se aplican por el camino que los frontends ya
    /// tienen.
    ///
    /// El token del plan sale de aquí también, atado a `dir`: es lo único que
    /// [`Self::organize`] acepta, y calcularlo en el frontend pondría el
    /// digest en dos sitios.
    ///
    /// **`names` es el operando, y aquí vacío significa vacío** — no «todo»,
    /// que es lo que significa en [`Self::ai_organize_plan`]. La asimetría no
    /// es un descuido: el modelo recibe el listado porque el ENGINE lista el
    /// directorio por él, y un plugin no puede listar nada (regla 9), así que
    /// lo que no le den no existe para él. Un organizer llamado con la lista
    /// vacía contesta, correctamente, que no mueve nada.
    ///
    /// # Errors
    /// [`Error::NotFound`] si ese plugin/organizer no está consentido;
    /// [`Error::InvalidPath`] si el plugin propone un destino que se sale del
    /// directorio —lo que tumba el plan ENTERO, no el movimiento—;
    /// [`Error::Io`] si el guest no corre. En `Remote`, lo que conteste el
    /// daemon.
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
                    let (runtime, _) = columnas_de_proceso()?;
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
                        crate::plugins::OrganizePlanOutcome::Refused(frase) => {
                            tracing::info!(plugin = %plugin_id, motivo = %frase, "organizer: rehusó");
                            Ok(norte_proto::methods::AiOrganizePlanResult {
                                moves: Vec::new(),
                                refused: Some(crate::plugins::guest_reason(&frase)),
                                plan_hash: None,
                            })
                        }
                        // Proponer una escritura fuera del directorio no es un
                        // fallo de E/S: es exactamente `InvalidPath`.
                        crate::plugins::OrganizePlanOutcome::Escapes => Err(Error::InvalidPath),
                        crate::plugins::OrganizePlanOutcome::Failed => {
                            Err(Error::Io { retryable: false })
                        }
                    }
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            #[cfg(unix)]
            Self::Remote(r) => {
                r.plugin_organize_plan(plugin_id, organizer_id, dir, names)
                    .await
            }
        }
    }

    /// Valores de la columna `column_id` para `paths` (G3b, ADR 0037
    /// decisión 2): a diferencia de [`Self::plugin_decorate`] (superposición
    /// de TODOS los decorators), como mucho UN plugin `columns` aporta
    /// `column_id` ([`crate::PluginRegistry::resolve_columns`],
    /// primero-que-casa). Mismo contrato de entradas (basenames) que
    /// `plugin_decorate`. Fail-closed: si el ÚNICO plugin que aporta la
    /// columna no instancia, trapea, o rompe el contrato posicional, el
    /// resultado es un vector de `None` del tamaño de `paths` (celda vacía
    /// para toda la página) en vez de un error — una columna sin datos es
    /// un enriquecimiento perdido, no un fallo del listado. `paths` vacío
    /// devuelve `Ok(vec![])`.
    ///
    /// # Errors
    /// Solo por fallos de INFRAESTRUCTURA (igual que [`Self::plugin_decorate`]).
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
                // El directorio padre de la página: la ubicación que el guest
                // puede leer si se le aprobó (ADR 0057).
                let location = paths.first().and_then(norte_proto::VPath::parent);
                let column_id_owned = column_id.to_owned();
                let plugin_id_owned = plugin_id.to_owned();
                let values = crate::blocking::spawn_blocking(
                    move || -> Result<Vec<Option<String>>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        // ESE plugin o ninguno (#120): dos plugins consentidos
                        // que declaren el mismo id bare no pueden servirse el
                        // uno por el otro.
                        let Some(resolved) =
                            reg.resolve_columns_of(Some(&plugin_id_owned), &column_id_owned)
                        else {
                            return Ok(vec![None; expected_len]);
                        };
                        // El motor y el pool son de PROCESO, como en el daemon
                        // (#224). Construir un `PluginRuntime` por llamada no
                        // solo compila el motor otra vez: arranca y para un
                        // hilo ticker de época por página pintada.
                        let (runtime, pool) = columnas_de_proceso()?;
                        // MISMA función que el daemon: la capacidad de
                        // ubicación no puede significar una cosa aquí y otra
                        // allí.
                        Ok(pool.column_values(
                            runtime,
                            resolved,
                            &column_id_owned,
                            location.as_ref(),
                            // Embebido: quien mira es la persona que abrió el
                            // panel, el mismo caso que `Actor::User`.
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
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_column_values(plugin_id, column_id, paths).await,
        }
    }

    /// El marco que un plugin pinta para su panel (0.74.0, fase 3).
    ///
    /// Embebido TAMBIÉN, y no solo con daemon: el mismo `ntc` no puede enseñar
    /// un panel de git cuando hay daemon y una caja vacía cuando no lo hay.
    /// Lo que corre es la misma función que el daemon (`render_panel_blocking`),
    /// con el mismo mint y la misma vida de la sesión de ubicación.
    ///
    /// `climb` es `true` aquí porque embebido quien mira es la persona que
    /// abrió el panel — el caso de `Actor::User` en el daemon—, así que subir a
    /// buscar la raíz del proyecto (`.git`) es lo correcto.
    ///
    /// Fail-soft: sin plugin que lo pinte, o si el guest falla, no hay marco.
    ///
    /// Embebido cuesta un DESCUBRIMIENTO del catálogo por llamada —leer el
    /// directorio de plugins y parsear cada `plugin.toml`—, donde el daemon
    /// tiene su registro en memoria. Se paga por cambio de contexto (otro
    /// directorio, otra fila, otro tamaño), no por frame, porque una firma que
    /// ya se pidió o que ya volvió vacía no se repite.
    ///
    /// # Errors
    /// Taxonomía del protocolo. Con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`; embebido, `Io{retryable:false}`
    /// si el catálogo no se puede leer e `Internal` si el motor de wasm no
    /// arranca o la tarea bloqueante se cae.
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
                        let Some(resuelto) = reg.resolve_panel(&params.plugin_id, &params.kind)
                        else {
                            return Ok(None);
                        };
                        // El motor es de PROCESO, como en el daemon (#224):
                        // construir uno por repintado compila el motor otra vez
                        // y arranca un hilo de época por frame.
                        let (runtime, _) = columnas_de_proceso()?;
                        let contexto = norte_plugin_host::panel_iface::PanelContext {
                            cols: params.cols,
                            rows: params.rows,
                            lang: params.lang.clone(),
                            cursor_name: params.cursor_name.clone(),
                        };
                        let evento = crate::plugins::panel_event_to_host(&params.event);
                        let state = params.state.clone().unwrap_or_default();
                        match crate::plugins::render_panel_blocking(
                            runtime,
                            resuelto,
                            &crate::plugins::PanelCall {
                                dir: &params.dir,
                                climb: true,
                                kind: &params.kind,
                                contexto: &contexto,
                                state: &state,
                                evento: &evento,
                            },
                        ) {
                            Ok((id, marco)) => {
                                Ok(Some(crate::plugins::panel_frame_to_wire(id, marco)))
                            }
                            // Un guest que falla deja el panel sin marco, no la
                            // pantalla con un error.
                            Err(_) => Ok(None),
                        }
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_panel_render(params).await,
        }
    }

    /// Esquema `[config]` + valores efectivos de `id` (0.28.0, G3c, ADR
    /// 0037): cierra la deuda P2 (`settings` era host-only). Id desconocido
    /// devuelve `keys: []` (mismo criterio indulgente que
    /// [`Self::plugins_list`] con un catálogo vacío). Embebido: registro
    /// EFÍMERO por-llamada en `spawn_blocking` (regla 2), igual criterio de
    /// coste que [`Self::plugins_list`].
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
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
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_get_config(id).await,
        }
    }

    /// La página de ayuda de un plugin (H3e, 0.34.0), ya acotada por el host a
    /// [`norte_proto::methods::PLUGIN_HELP_MAX_BYTES`]. Embebido: registro
    /// EFÍMERO por llamada en `spawn_blocking` (regla 2), mismo criterio de
    /// coste que [`Self::plugin_get_config`].
    ///
    /// El markdown que devuelve NO está enmascarado: lleva verbatim los peligros
    /// de terminal que el plugin escribiera (ESC, controles C0, anulaciones
    /// bidi). Se parsea con `norte_help::parse_untrusted`, que enmascara al
    /// construir el modelo; nunca se pinta ni se loguea en crudo.
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    ///
    /// Un `id` que no está en el catálogo NO da el mismo error por los dos
    /// caminos, y conviene saberlo: embebido es [`Error::NotFound`], mientras
    /// que el daemon responde `INVALID_PARAMS` sin taxonomía en `data`, que
    /// `to_taxonomy` entrega como `Internal{panic:false}`. Un frontend que
    /// quiera distinguir "plugin desconocido" de "el daemon tuvo un problema"
    /// no puede hacerlo por el wire; lo correcto en ambos casos es tratarlo
    /// como "no hay página" y seguir pintando.
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
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_help(id).await,
        }
    }

    /// Persiste UN valor de `[config]` para `id`, validado contra el
    /// esquema del manifiesto (0.28.0, G3c, ADR 0037). Embebido: registro
    /// EFÍMERO por-llamada + `PluginRegistry::set_config` (valida, persiste,
    /// re-resuelve), TODO en `spawn_blocking`.
    ///
    /// # Invariante de seguridad (defensa en profundidad)
    /// Igual que [`Self::plugins_set_approval`]: el gate "solo humano" vive
    /// en la capa wire (daemon con `Actor`); este `Backend` embebido es la
    /// API del frontend humano y no recibe `Actor`.
    ///
    /// # Errors
    /// [`Error::NotFound`] si `id` es desconocido; taxonomía del protocolo
    /// en lo demás (un valor inválido o clave desconocida llega como un
    /// error genérico — el caller debe validar client-side ANTES de llamar,
    /// que es lo que hacen TUI/GUI).
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
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_set_config(id, key, value).await,
        }
    }
}

/// Mapea un fallo de [`crate::PluginRegistry::set_config`] a la taxonomía
/// del protocolo (modo EMBEBIDO): un id desconocido es honestamente
/// `NotFound` (mismo criterio que [`Backend::plugins_set_approval`]); el
/// resto (clave desconocida / valor inválido / I/O) se REDACTA a `Internal`
/// — el caller (TUI/GUI) valida client-side ANTES de llamar, así que este
/// camino solo se pisa por un valor que se coló esa barrera (backstop, no
/// UX primaria).
fn config_set_error_to_taxonomy(e: &crate::plugins::PluginConfigSetError) -> Error {
    use crate::plugins::PluginConfigSetError as E;
    match e {
        E::Unknown(_) => Error::NotFound,
        E::UnknownKey(_) | E::Invalid(_) | E::Io(_) => Error::Internal { panic: false },
    }
}

/// Mapea el fallo de ejecución de un plugin a la taxonomía del protocolo (modo
/// EMBEBIDO). Un fallo de runtime se REDACTA a `Internal` (jamás el `Display`
/// crudo, que puede llevar la ruta del `.wasm` o detalles de wasmtime): la
/// misma política que el daemon aplica en el wire (security-reviewer M4-P4).
fn run_error_to_taxonomy(e: &crate::plugins::PluginRunError) -> Error {
    use crate::plugins::PluginRunError as E;
    match e {
        // Id inexistente / sin binario / no consentido: el nodo pedido no está
        // disponible para ejecución. `NotFound` es la categoría honesta y NO
        // revela rutas (los mensajes de estos variantes llevan el id, no el path).
        E::Unknown(_) | E::NoBinary(_) | E::NotApproved(_) | E::Disabled(_) => Error::NotFound,
        // Un kind sin `command`: ese plugin no tiene la capacidad que se le
        // pide, que es lo que `Unsupported` dice (el daemon lo devuelve como
        // `INVALID_PARAMS`, con el mismo sentido).
        E::NotRunnable(_) => Error::Unsupported,
        // Fallo del runtime: redactado, sin filtrar el detalle al frontend.
        E::Runtime(_) => Error::Internal { panic: false },
    }
}

/// El motor WASM y el pool de instancias de columnas del PROCESO, para el
/// backend embebido (#224).
///
/// El daemon los cuelga de su `Shared`; aquí no hay dónde, porque el backend
/// embebido redescubre el registro en cada llamada y no tiene estado propio.
/// Un `static` es lo que hace que las dos mitades del mismo binario —`ntc`
/// embebido y `ntc --daemon`— paguen lo mismo por la misma página.
///
/// Se construye una vez y no se suelta: `PluginRuntime` posee el hilo ticker
/// que hace avanzar la época del motor, o sea el reloj con el que un guest en
/// bucle trapa (regla dura 3). Un runtime por llamada arrancaba y paraba ese
/// hilo por página pintada.
///
/// # Errors
/// Si el motor wasmtime no se puede configurar en esta plataforma. El fallo se
/// recuerda: reintentarlo por cada página sería pagar el fallo N veces para
/// llegar al mismo sitio.
/// El motor de wasm y el pool de columnas de ESTE proceso.
///
/// Lo usan las columnas y, desde la fase 3, los paneles de plugin, que se
/// quedan solo con el motor: construir un `PluginRuntime` por llamada compila
/// el motor otra vez y arranca un hilo de época por pintado. El nombre dice
/// «columnas» por quién llegó primero.
fn columnas_de_proceso() -> Result<
    (
        &'static norte_plugin_host::PluginRuntime,
        &'static crate::plugins::ColumnPool,
    ),
    Error,
> {
    struct Columnas {
        runtime: norte_plugin_host::PluginRuntime,
        pool: crate::plugins::ColumnPool,
    }
    static COLUMNAS: std::sync::OnceLock<Option<Columnas>> = std::sync::OnceLock::new();
    let cel = COLUMNAS.get_or_init(|| {
        norte_plugin_host::PluginRuntime::new()
            .ok()
            .map(|runtime| Columnas {
                runtime,
                pool: crate::plugins::ColumnPool::default(),
            })
    });
    match cel {
        Some(c) => Ok((&c.runtime, &c.pool)),
        None => Err(Error::Internal { panic: false }),
    }
}
