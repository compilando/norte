# Catálogo español de norte. Paridad total con en.ftl (test).

# --- Modales del TUI ---
modal-trash-title = A la papelera
modal-trash-note = recuperable desde la papelera del sistema
modal-delete-permanent-title = Borrar PERMANENTE
modal-delete-permanent-warning = ⚠ SIN papelera: esto no se puede deshacer
modal-copy-title = Copiar
modal-move-title = Mover
modal-collision-title = Colisión
modal-collision-body = el destino ya existe:
modal-approval-title = Aprobación de agente
modal-approval-body = el agente "{ $session }" pide { $op }:
modal-approval-path = { $badge }ruta { $n }: { $path }
modal-trust-host-title = Host key desconocida
modal-trust-host-host = { $badge }host: { $host }
modal-trust-host-algo = { $badge }algoritmo: { $algo }
modal-trust-host-fp = { $badge }huella: { $fingerprint }
modal-trust-host-note = compárala fuera de banda antes de confiar.
modal-lua-trust-title = ¿Ejecutar el init.lua del proyecto?
modal-lua-trust-body = { $path } (sha256 { $hash }) se ejecutará CON TUS PERMISOS. El script de un repo clonado puede hacer todo lo que tú puedas. y = confiar y ejecutar · n/Esc = denegar (se recuerda hasta que el fichero cambie)
modal-confirm-quit-title = ¿Salir de norte?
modal-confirm-quit-body = Cierra la aplicación.
modal-mark-pattern-add = Marcar por patrón
modal-mark-pattern-remove = Desmarcar por patrón
modal-mark-pattern-hint = glob, por ejemplo *.rs
modal-mark-pattern-keys = [enter] confirmar · [esc] cancelar
modal-mkdir = Crear directorio
modal-mkdir-hint = nombre del directorio nuevo
modal-transfer-name-copy = Copiar a
modal-transfer-name-move = Mover a
modal-transfer-name-hint = nombre en el destino (edítalo para renombrar)
modal-ai-rename = Renombrado IA — instrucción
modal-ai-rename-hint = Enter: pedir plan · Esc: cancelar
modal-ai-rename-empty-instruction = escribe una instrucción primero
modal-ai-rename-plan = Renombrado IA — plan propuesto
modal-ai-rename-dir = en: { $dir }
modal-ai-rename-pair-from = { $n }. { $from }
modal-ai-rename-pair-to = → { $to }
modal-ai-rename-more = … { $shown }/{ $total } (desplazar: ↓/↑)
modal-ai-rename-plan-hint = y/Enter: aplicar · n/Esc: descartar
modal-semantic = Búsqueda semántica
modal-semantic-hint = Enter busca · Esc cancela
modal-semantic-empty-query = escribe una consulta primero
modal-semantic-hits = Resultados semánticos
modal-semantic-hit = { $n }. { $path } · { $score }
modal-semantic-more = … { $shown }/{ $total } (desplazar: ↓/↑)
modal-semantic-hits-hint = y/Enter: abrir ubicación · n/Esc: cerrar
msg-transfer-name-fffd = el nombre aún contiene el carácter de sustitución — reescríbelo limpio
msg-transfer-name-same = mismo nombre y sitio: nada que hacer
msg-transfer-name-failed = no se pudo encolar — el nombre se conserva

# --- Mensajes de la barra ---
msg-done = hecho
msg-cancelled = cancelado
msg-cancelling = cancelando…
msg-no-tasks = no hay tasks en marcha
msg-error = error: { $error }
# Errores por CATEGORÍA (spec §17.7): localizados, jamás el string del OS.
err-not-found = no encontrado
err-permission-denied = permiso denegado
err-conflict-exists = el destino ya existe
err-conflict-case = el nombre colisiona por caja con una entrada existente
err-conflict-normalization = el nombre colisiona tras normalización Unicode
err-conflict-type = el destino es de otro tipo
err-conflict = conflicto en el destino
err-provider-unavailable = la ubicación no está disponible (reintentable)
err-no-space = sin espacio en el destino
err-io = error de E/S
err-cancelled = cancelado
err-policy-denied = denegado por la política
err-encoding-loss = la operación perdería datos al transcodificar
err-unsupported = no soportado aquí
err-invalid-path = ruta inválida
err-internal = error interno
err-loop = ciclo de symlinks
err-corrupt = no es un archivo/contenedor válido
err-limit-exceeded = el contenedor excede los límites locales de seguridad (no se abre)
err-host-key-unknown = host key desconocida (primer contacto)
err-host-key-mismatch = host key NO COINCIDE — posible MITM
err-cursor-expired = el listado expiró; refresca
err-unknown = error desconocido
err-config-io = no se pudo leer { $path }: { $error }
err-config-parse = TOML inválido en { $path }: { $detail }
err-keymap-preset-unknown = preset desconocido { $name }; disponibles: { $available }
err-keymap-invalid = keymap inválido: { $detail }
# --- Scripting Lua (M4, ADR 0026) — detalles SIEMPRE por detail_for_bar ---
err-lua-load = init.lua ({ $layer }): { $detail }
err-lua-unknown = comando Lua desconocido: { $name }
err-lua-command = el comando Lua falló: { $detail }
err-lua-cancelled = comando Lua cancelado
err-lua-timeout = el comando Lua agotó el tiempo
err-lua-statusbar = statusbar Lua deshabilitada: { $detail }
err-lua-no-state-dir = init.lua del proyecto no cargado: sin directorio de estado (XDG_STATE_HOME/HOME)
# Ya en la cima: no hay directorio padre (raíz `/` o raíz de unidad Windows).
msg-nav-at-top = ya estás en la cima
msg-refresh-error = refresh: { $error }
msg-view-error = view: { $error }
msg-config-reloaded = config recargada
msg-daemon-lost = conexión con el daemon perdida; reconectando…
msg-daemon-restored = reconectado al daemon
msg-config-not-applied = config NO aplicada: { $error }
msg-config-polling = config: vigilancia degradada a polling
msg-no-trash-here = sin papelera aquí: F8 de nuevo para permanente
msg-list-incomplete = listado incompleto: se cortó al rellenar
msg-lua-busy = ya hay un comando Lua en marcha (encolado)
msg-lua-queue-full = comando Lua descartado: cola llena
msg-lua-denied-changed = el init.lua del proyecto se denegó; ha cambiado (no se carga)
msg-lua-symlink = el init.lua del proyecto es un symlink; no se carga
msg-lua-keymap-project = keymap.toml del proyecto: { $n } binding(s) lua: ignorados (sin trust)
pane-loading = cargando… ({ $n })
quicksearch-partial = (parcial)

# --- Panel de tasks ---
task-cancelled = cancelado

# --- Viewer ---
viewer-forced = (forzado)
viewer-lossy = con pérdidas (�)
viewer-truncated = [cabecera]
viewer-binary = binario
viewer-plugin-preview = via { $plugin }
viewer-plugin-preview-lossy = [decodificación con pérdida]
eol-mixed = EOL mixto
eol-none = sin EOL

# --- CLI ---
cli-runtime-error = norte: no se pudo arrancar el runtime: { $error }
cli-enqueue-copy = no se pudo encolar la copia
cli-enqueue-move = no se pudo encolar el move
cli-enqueue-delete = no se pudo encolar el borrado
cli-enqueue-mkdir = no se pudo encolar el mkdir
cli-list-failed = list falló
cli-entry-unreadable = entrada ilegible
cli-serialize-failed = no se pudo serializar
cli-cancelling = cancelando…
cli-cancelled-clean = cancelado (destino limpio)
cli-final-error = error: { $error }
cli-unexpected-state = estado final inesperado: { $state }
cli-daemon-listening = daemon escuchando en { $socket }
cli-mcp-serving = MCP por stdio (sesión { $session }); Ctrl-D para terminar
cli-scope-granted = scope concedido
cli-undo-done = sesión { $session } deshecha
cli-undo-failed = undo incompleto: { $error }
cli-undo-report = { $undone } revertidas, { $skipped_irreversible } irreversibles saltadas
cli-undo-left-in-place = { $count } creación(es) se quedan donde están (el destino no tiene papelera); bórralas explícitamente si quieres que desaparezcan
cli-undo-blocked = el undo paró en la entrada { $seq } del journal ({ $error }): las anteriores NO se deshicieron
cli-undo-report-unavailable = el undo terminó pero su informe no se pudo leer ({ $error }): desenlace sin verificar
cli-audit-open-failed = no se pudo abrir el journal (¿daemon corriendo? el audit lo necesita parado)
cli-audit-empty = journal vacío: nada que anclar
cli-audit-anchored = head del journal anclado en seq { $seq }
cli-audit-chain-ok = hash-chain íntegra ({ $entries } entradas)
cli-audit-chain-broken = hash-chain ROTA en la entrada { $seq } del journal
cli-audit-no-anchors = sin fichero de anclas: `norte audit anchor` fija el head actual (la ausencia es FALLO salvo --allow-no-anchors: un atacante puede simplemente borrar el fichero)
cli-audit-anchor-bad = ancla de la línea { $line } FALLIDA: { $detail }
cli-audit-coverage = cobertura de anclas: hasta seq { $anchored } de head { $head } de la cadena
cli-audit-verdict-bad-line = línea de ancla ilegible
cli-audit-verdict-bad-mac = MAC inválido (¿ancla fabricada o clave rotada sin re-anclar?)
cli-audit-verdict-missing = el seq { $seq } anclado YA NO EXISTE (truncación de cola/rollback)
cli-audit-verdict-mismatch = el seq { $seq } existe con OTRO hash (historia reescrita)
cli-audit-anchors-ok = { $count } ancla(s) verificadas contra la cadena
cli-plugin-run-failed = no se pudo ejecutar el plugin: { $error }
cli-daemon-stopped = apagado pedido al daemon
cli-daemon-hard-shutdown = segunda señal: cancelando tasks…
cli-hostkey-unknown = primera conexión a { $host }:{ $port } — host key sin registrar
cli-hostkey-fingerprint = huella { $algo }: { $fingerprint }
cli-hostkey-prompt = ¿Confiar en esta clave y registrarla en known_hosts? [s/N]
cli-hostkey-refused = conexión abortada: host key sin confirmar
cli-hostkey-noninteractive = entrada no interactiva: confirma la host key con `norte connect <url>` en un terminal, o pre-puebla el known_hosts (env NORTE_KNOWN_HOSTS)
cli-hostkey-trusted = host key registrada
cli-connect-ok = conexión establecida: { $target }
cli-connect-failed = no se pudo conectar
cli-connect-daemon-unsupported = `norte connect` todavía no funciona con --daemon (usa el modo embebido)
cli-invalid-url = URL remota inválida: { $url }
cli-confirm-read = lectura de confirmación
cli-inline-password = la URL no debe llevar el password inline (user:pass@…); el secreto va por el keyring/env/secrets.age
cli-connection-degraded = ⚠ { $scheme }://{ $host }: sesión SIN cifrar (el servidor rechazó AUTH TLS, tls="allow"). Datos y credenciales viajan en claro.
status-connection-degraded = ⚠ { $scheme }://{ $host } — texto plano
status-archive-skipped = ⚠ { $n } entradas omitidas (nombres hostiles/límites)
status-names-encoding = nombres: { $enc }
status-hidden = { $n } ocultas
# --- Celdas de columnas (#108) ---
col-time-now = ahora
col-header-name = Nombre
col-header-size = Tamaño
col-header-mtime = Fecha
col-header-kind = Tipo
col-kind-dir = dir
col-kind-file = fichero
col-kind-symlink = enlace
col-kind-other = otro
col-time-min = hace { $n }m
col-time-hour = hace { $n }h
col-time-day = hace { $n }d
col-time-year = hace { $n }a
col-cell-yes = sí
col-cell-no = no
col-attr-posix-mode = Modo
col-attr-posix-uid = UID
col-attr-posix-gid = GID
col-attr-posix-nlink = Enlaces
col-attr-posix-ctime-ms = Cambiado
col-attr-win-attributes = Atributos
col-attr-s3-etag = ETag
col-attr-s3-content-type = Tipo de contenido
col-attr-archive-method = Método
col-attr-archive-packed-size = Comprimido
col-attr-archive-crc32 = CRC-32
status-marked = { $n } marcadas, { $size }
status-marked-with-dirs = { $n } marcadas, { $size } + { $dirs } dirs
status-marks-pruned = { $n } marcas caídas, sus entradas ya no están
status-watch-degraded = vigilancia de directorios degradada a sondeo (¿límite de inotify?) — crear/borrar/renombrar se ve en segundos; editar un fichero existente no se detecta
msg-names-encoding = nombres mostrados como { $enc } (solo display; los bytes no cambian)
msg-names-encoding-off = nombres tal cual (reinterpretación apagada)
msg-hidden-shown = entradas ocultas visibles
msg-mkdir-in-search = los resultados de búsqueda no tienen directorio destino — sal antes de la búsqueda
msg-ai-rename-running = Renombrado IA: pensando… (Esc cancela)
# Variante GUI: la GUI no tiene camino para abortar la petición en vuelo, así
# que no debe prometer "Esc cancela" (jamás una affordance falsa).
gui-msg-ai-rename-running = Renombrado IA: pensando…
# Un plan retenido (esperando tras un modal abierto) fue reemplazado por una
# petición más nueva antes de poder revisarse — la pérdida se dice, jamás muda.
gui-msg-ai-rename-superseded = Renombrado IA: plan anterior descartado (nueva petición)
msg-ai-rename-empty = Renombrado IA: el modelo no propuso cambios
msg-ai-rename-failed = el renombrado IA falló: { $error }
msg-ai-rename-applied = Renombrado IA: { $n } movimientos enviados
msg-ai-rename-invalid-plan = renombrado IA: plan inválido del daemon — no se aplicó nada
msg-ai-rename-in-search = el renombrado IA no está disponible en un pane de búsqueda
msg-semantic-running = Búsqueda semántica: pensando… (Esc cancela)
# Variante GUI: la GUI no tiene camino para abortar la petición en vuelo, así
# que no debe prometer "Esc cancela" (jamás una affordance falsa).
gui-msg-semantic-running = Búsqueda semántica: pensando…
# Unos hits retenidos (esperando tras un modal abierto) fueron reemplazados por
# un resultado más nuevo antes de poder revisarse — la pérdida se dice, jamás
# muda.
gui-msg-semantic-superseded = Búsqueda semántica: hits anteriores descartados (nuevo resultado)
msg-semantic-empty = Búsqueda semántica: sin resultados
msg-semantic-failed = la búsqueda semántica falló: { $error }
msg-semantic-invalid = búsqueda semántica: respuesta inválida del daemon — no se muestra nada
msg-semantic-in-search = la búsqueda semántica no está disponible en un pane de búsqueda
msg-hidden-hidden = entradas ocultas escondidas
msg-marked-by-pattern = { $n } marcas cambiadas
msg-mouse-capture-failed = el terminal no aceptó la captura de ratón: norte se queda solo con teclado
# Arrastre EN VUELO, en LOS DOS frontends: qué haría soltar ahora mismo.
# Jamás solo de la GUI — la barra de estado de la TUI dice lo mismo desde
# la misma fuente (`Drag::pending`), así que no pueden prometer drops
# distintos.
drag-copy = Soltar para COPIAR { $n } elemento(s) → { $to }   (con shift, mover)
drag-move = Soltar para MOVER { $n } elemento(s) → { $to }   (suelta shift para copiar)
msg-open-missing-program = el opener necesita `{ $program }` — no instalado
msg-open-remote = los openers solo funcionan con archivos locales
msg-open-launched = abierto con { $program }
msg-open-failed = no se pudo lanzar { $program }: { $error }
cli-ls-skipped = aviso: { $n } entradas del contenedor omitidas del índice (nombres hostiles/límites)

# --- Ayuda (F1) — construida del keymap efectivo ---
help-title = Ayuda — teclas activas
help-hint = [esc/q/f1] cerrar   [↑/↓/pgup/pgdn] desplazar
help-section-browse = Navegación (panes)
help-section-viewer = Viewer
help-section-dialog = Diálogos y overlays
help-dialog-note = cada diálogo soporta su propio subconjunto de estas teclas
help-cmd-app-quit = salir de norte
help-cmd-app-help = esta ayuda
help-cmd-app-theme = elegir tema
help-cmd-app-extensions = gestor de extensiones
help-cmd-app-palette = paleta de comandos
help-cmd-app-settings = ajustes
# --- Paleta de comandos (H1 T4) — editor de filtro libre como el diálogo
# de búsqueda (decisión 8): sus teclas son fijas, NO resuelven por el
# contexto `dialog` — este hint es una cadena estática, como `search-hint`.
palette-title = Paleta de comandos
palette-hint = [↑/↓/pgup/pgdn] navegar · [enter] ejecutar · [esc] cerrar
# --- Overlay de ajustes (S3) — mismo criterio de filtro libre que la
# paleta de arriba (decisión 8): la búsqueda está SIEMPRE activa, Enter
# togglea/cicla/edita.
settings-title = Ajustes
settings-hint = [↑/↓/pgup/pgdn] navegar · [enter] editar · [esc] cerrar
settings-edit-hint = [enter] guardar · [esc] cancelar
settings-section-general = General
settings-section-plugins = Plugins
settings-plugins-name = Ajustes de plugins
settings-plugins-note = Ningún plugin instalado declara ajustes configurables.
settings-plugins-open-hint = [enter] abrir los ajustes de este plugin
settings-plugins-key-count = {$count} ajustes
# --- Vista de ajustes de la GUI (S4) — swap a pantalla completa por ratón
# sobre el mismo catálogo/máquina de estado que el overlay de arriba.
settings-restart-badge = requiere reinicio
settings-hint-gui = [↑/↓/pgup/pgdn/click] navegar · [enter/click] editar · [esc] cerrar
# P1: prefijo de una fila aportada por un plugin (`palette::plugin_rows`) —
# ninguna fila built-in lo lleva, así que un plugin no puede disfrazarse de
# comando built-in copiando su texto exacto.
palette-plugin-prefix = extensión
theme-picker-title = Tema
columns-picker-title = Columnas — { $target }
columns-picker-target-default = todos los schemes
columns-picker-hint-gui = Espacio activa · Shift+↑/↓ mueve · S ordena · F formato · Enter aplica · Esc cierra
msg-columns-saved = columnas guardadas
ext-title = Extensiones
ext-empty = no hay extensiones instaladas
ext-unapproved = sin aprobar
history-title = Historial
history-empty = todavía no hay historial
hotlist-title = Favoritos
hotlist-empty = vacío — añade el directorio actual desde el popup
hotlist-name-prompt = nombre:
hotlist-invalid = ruta inválida
msg-theme-applied = tema aplicado: { $name }
msg-theme-reverted = tema sin cambios
msg-theme-saved = tema guardado: { $name } → { $path }
msg-theme-save-failed = tema aplicado (no guardado): { $error }
msg-hotlist-saved = favorito guardado: { $name }
msg-hotlist-removed = favorito eliminado: { $name }
# P1: resultado de Enter sobre una fila de plugin de la palette — { $output }
# es salida NO confiable del plugin, ya enmascarada+acotada por
# `detail_for_bar` antes de llegar aquí (patrón #73). El prefijo
# "extensión:" la marca como texto de terceros, mismo vocabulario que
# `palette-plugin-prefix`.
msg-plugin-run-ok = extensión: { $output }
# G3c: `dialog.confirm` sobre un plugin con esquema `[config]` vacío.
msg-plugin-config-empty = este plugin no declara ajustes configurables
# G3c: `plugin.set_config` tuvo éxito — { $key } es la clave declarada por el
# manifiesto (charset seguro, nunca texto libre del plugin); { $value } es el
# valor nuevo, ya validado client-side.
msg-plugin-config-saved = { $key } guardado: { $value }
msg-settings-saved = { $name } guardado: { $value }
msg-settings-save-failed = no se pudo guardar: { $error }
# Revisión S I1: la propia tarea de fondo de la escritura panicó o se
# canceló (nunca observado en la práctica — la única causa de panic
# conocida, una forma inesperada de `[section]`, ya es un `Err` limpio de
# `persist_set` — este es el brazo de defensa en profundidad para cualquier
# otra cosa que pudiera tumbar esa tarea). Sin placeholder `{ $error }` a
# propósito: un fallo de join no trae una categoría limpia y localizable
# como sí trae un `ErrorKind` de `io::Error`.
msg-settings-save-crashed = error interno al guardar — el valor no se escribió
msg-settings-invalid-int = no es un número
msg-settings-invalid-range = el valor debe estar entre { $min } y { $max }
msg-settings-no-config-dir = sin directorio de config de usuario (entorno sin definir)
msg-hotlist-persist-failed = favoritos no guardados: { $error }
help-cmd-pane-switch = cambiar de pane
help-cmd-cursor-up = subir el cursor
help-cmd-cursor-down = bajar el cursor
help-cmd-cursor-page-up = subir una página
help-cmd-cursor-page-down = bajar una página
help-cmd-cursor-top = ir al principio
help-cmd-cursor-bottom = ir al final
help-cmd-nav-enter = entrar en el directorio seleccionado
help-cmd-nav-parent = subir al directorio padre
help-cmd-pane-copy = copiar la selección al otro pane
help-cmd-pane-move = mover la selección al otro pane
help-cmd-pane-delete = borrar (papelera si la hay)
help-cmd-pane-delete-permanent = borrar PERMANENTE
help-cmd-pane-view = ver el archivo seleccionado
help-cmd-pane-open = abrir el archivo seleccionado con un programa externo (openers.toml)
help-cmd-pane-quick-search = quick search en el pane (filtro/salto)
help-cmd-pane-history = historial de directorios
help-cmd-pane-hotlist = directorios favoritos
help-cmd-task-cancel = cancelar la task más reciente
# G3c: comandos SOLO de la GUI (sin equivalente en la TUI, que usa otros
# bindings para multi-selección/franja de tasks) — hacen falta ahora que la
# paleta de comandos de la GUI lista `app.palette`/`app.extensions` y
# necesita texto de ayuda para todo comando de la GUI.
# `mark.toggle` pasó a los presets compartidos (#103) — se queda aquí porque
# el id es anterior a ese cambio y ambos frontends lo siguen usando.
help-cmd-mark-toggle = marcar/desmarcar la entrada bajo el cursor
help-cmd-mark-all = marcar todas las entradas visibles
help-cmd-mark-invert = invertir las marcas
help-cmd-mark-clear = quitar todas las marcas
help-cmd-mark-pattern-add = marcar por patrón
help-cmd-mark-pattern-remove = desmarcar por patrón
help-cmd-task-next = resaltar la siguiente task
help-cmd-task-prev = resaltar la task anterior
help-cmd-task-dismiss = descartar las tasks terminadas de la franja
help-cmd-viewer-close = cerrar el viewer
help-cmd-viewer-up = subir una línea
help-cmd-viewer-down = bajar una línea
help-cmd-viewer-page-up = subir una página
help-cmd-viewer-page-down = bajar una página
help-cmd-viewer-top = ir al principio
help-cmd-viewer-bottom = ir al final
help-cmd-viewer-encoding = recargar como… (siguiente encoding)
help-cmd-pane-names-encoding = ver nombres como… (cp437/cp866/Shift-JIS/GBK/…; solo display)
help-cmd-pane-toggle-hidden = mostrar u ocultar entradas ocultas
help-cmd-pane-columns = selector de columnas
help-cmd-pane-mkdir = crear un directorio (F7)
help-cmd-pane-ai-rename = renombrado IA del directorio actual (plan revisable)
help-cmd-pane-semantic-search = búsqueda semántica sobre el índice (IA)
help-cmd-pane-rename = renombrar in situ (Shift+F6)
help-cmd-pane-refresh = recargar ambos panes (Ctrl+R)
help-cmd-viewer-encoding-auto = volver a la detección automática
help-cmd-viewer-hex = alternar vista hexadecimal
help-cmd-pane-search = buscar por nombre/contenido (Alt+F7)
help-cmd-pane-copy-path = copia la ruta de la selección al portapapeles
search-title = Búsqueda
search-name = nombre (glob):
search-content = contenido:
search-regex = [F2] regex: { $on }
search-case = [F3] mayús: { $on }
search-hint = [tab] campo · [enter] buscar · [esc] cancelar
search-empty = introduce un criterio de nombre o contenido
search-status-running = búsqueda: { $n } hits (buscando…)
search-status-done = búsqueda: { $n } hits
search-status-truncated = búsqueda: { $n } hits (truncada)
search-status-cancelled = búsqueda: { $n } hits (cancelada)
search-status-failed = búsqueda fallida: { $error }
on-yes = sí
on-no = no

# --- GUI (GUI-e T1) — banners, estados, modales, franja de tasks ---
gui-banner-keymap-error = keymap: { $error }
gui-banner-config-invalid = configuración inválida: { $error }
gui-banner-op-rejected = operación rechazada: { $error }
gui-banner-viewer-error = visor { $name }: { $error }
gui-banner-error = error: { $error }
gui-loading = cargando…
gui-dir-empty = (directorio vacío)
gui-tasks-empty = (sin tasks)
gui-viewer-opening = abriendo visor…
gui-viewer-image-unreadable = imagen ilegible
gui-a11y-pane-left = panel izquierdo
gui-a11y-pane-right = panel derecho
gui-a11y-tasks = tareas en curso
gui-menu-acts-on = actúa sobre { $target }
gui-menu-target-marks = { $n } elementos marcados
gui-menu-entry-disabled = { $label } — { $reason }
gui-menu-hint = ↑/↓ mover   Enter ejecutar   Esc cerrar
gui-menu-open = Abrir
gui-menu-view = Ver
gui-menu-copy = Copiar al otro panel
gui-menu-move = Mover al otro panel
gui-menu-rename-ai = Renombrar con IA (toda la carpeta)…
gui-menu-delete = Borrar
gui-menu-copy-path = Copiar la ruta
gui-menu-reason-read-only = backend de solo lectura
gui-menu-reason-wrong-target = no aplica a esta selección
gui-menu-reason-unavailable = no disponible ahora
gui-menu-copied = { $n } ruta(s) copiada(s) al portapapeles
gui-modal-rename-title = Renombrar
gui-modal-rename-from = actual: { $name }
gui-modal-rename-to = nuevo: { $name }
gui-modal-rename-footer = Enter renombra   Esc cancela
gui-menu-rename = Renombrar…
gui-modal-copy-title = Copiar { $n } elemento(s) → { $to }
gui-modal-move-title = Mover { $n } elemento(s) → { $to }
gui-modal-delete-title = Borrar { $n } elemento(s)
gui-modal-mode-trash = PAPELERA
gui-modal-mode-permanent = PERMANENTE
gui-modal-conflict-title = Conflicto: { $conflict }
gui-modal-more = … y { $n } más
gui-modal-footer-transfer = y confirmar   n/Esc cancelar
gui-modal-footer-delete = y confirmar   p alternar permanente   n/Esc cancelar
gui-modal-footer-conflict = o sobrescribir   s saltar   c/Esc cancelar
gui-task-kind-copy = copy
gui-task-kind-move = move
gui-task-kind-mkdir = mkdir
gui-task-kind-delete = delete
gui-task-kind-undo = undo
gui-task-kind-search = search
gui-task-kind-index = index
gui-task-kind-embed = embed
gui-task-kind-unknown = task
gui-task-state-pending = pending
gui-task-state-running = running
gui-task-state-paused = paused
gui-task-state-done = done
gui-task-state-cancelled = cancelled
gui-task-state-failed = failed
gui-task-state-unknown = ?
cli-gc-result = barridos { $n } parciales huérfanos bajo { $dir }
cli-gc-remote-unsupported = `norte gc` aún no funciona con --daemon (usa el modo embebido)
cli-ai-rename-plan = Renombrados propuestos (revísalos antes de aplicar):
cli-ai-rename-empty = el modelo no propuso ningún renombrado
cli-ai-rename-confirm = ¿Aplicar estos renombrados? [s/N]
cli-ai-rename-abort = cancelado; no se renombró nada
cli-ai-rename-done = renombrados { $n } archivo(s)
gui-modal-quit-title = ¿Salir con { $tasks } tarea(s) en curso y { $marks } marca(s)?
gui-modal-quit-title-empty = ¿Salir de norte?
gui-modal-footer-quit = y confirmar   n/Esc cancelar
gui-banner-theme-io = tema { $spec }: { $error }
gui-banner-theme-parse = tema { $spec }: { $detail }
gui-banner-config-io = configuración { $path }: { $error }
gui-banner-config-parse = configuración { $path }: { $detail }
gui-banner-effects-key-skipped = efectos del tema: se omitió { $key }
gui-banner-font-unknown = fuente { $family } no encontrada; se usa la de por defecto

# --- Hints de pie de diálogo (H1 T3, cierra #24) — GENERADOS: comandos
# dialog.* soportados × el keymap dialog EFECTIVO × estas etiquetas. Jamás
# un string de pie de página escrito a mano otra vez: un rebind no puede
# desincronizarlo.
dialog-cmd-confirm = confirmar
dialog-cmd-cancel = cancelar
dialog-cmd-approve = aprobar
dialog-cmd-deny = denegar
dialog-cmd-overwrite = sobrescribir
dialog-cmd-skip = saltar
dialog-cmd-rename = renombrar
dialog-cmd-newer = más nuevo
dialog-cmd-up = arriba
dialog-cmd-down = abajo
dialog-cmd-page-up = re pág
dialog-cmd-page-down = av pág
dialog-cmd-add = añadir
dialog-cmd-toggle-enabled = activar
dialog-cmd-remove = borrar
dialog-cmd-move-up = subir
dialog-cmd-move-down = bajar
# «ordenar» a secas: con «ordenar por» el hint del picker de columnas
# (que suma [f] formato en 7b) mide 77 celdas y no cabe entero en un
# frame de 80 (interior 78) — el guard ruidoso del snapshot lo pinea.
dialog-cmd-sort = ordenar
dialog-cmd-cycle-format = formato

# --- norte doctor (H2): diagnóstico de solo lectura sobre capas de config,
# keymaps, plugins y conexiones.
cli-doctor-title = norte doctor — diagnóstico de solo lectura
cli-doctor-section-config = -- config --
cli-doctor-section-keymap = -- keymap --
cli-doctor-section-plugins = -- plugins --
cli-doctor-section-connections = -- conexiones --
cli-doctor-ok = OK
cli-doctor-warn = AVISO
cli-doctor-error = ERROR
cli-doctor-footer-keymap-approx = nota: un aviso de «comando desconocido» es una aproximación contra los bindings propios de los tres presets empaquetados para esa pantalla — un comando específico de un frontend sin binding por defecto en ninguno es invisible para este chequeo.
cli-doctor-footer-connections-not-probed = nota: el keyring/`age` no se prueban (diagnóstico sin efectos secundarios) — solo se comprueba la presencia de la variable de entorno de respaldo; usa `norte connect` para probar una conexión de verdad.
cli-doctor-detail-connections-parse = connections.toml no parsea (corrígelo o bórralo)
cli-doctor-detail-connections-none = no hay connections.toml, o no hay conexiones configuradas
cli-doctor-detail-plugin-digest-stale = { $id }: las capabilities del manifiesto cambiaron desde la aprobación; requiere re-aprobación

# --- Registro de settings (S2): ajustes generales curados que muestra el
# overlay del TUI (S3) y la vista de la GUI (S4). Un par nombre/desc por
# entrada de `norte_frontend::settings::catalog()`.
setting-ui-theme-name = Tema
setting-ui-theme-desc = Preset de color de la interfaz (o una ruta a un fichero de tema propio, ADR 0020).
setting-ui-lang-name = Idioma
setting-ui-lang-desc = Idioma de la interfaz. Déjalo sin fijar para negociarlo desde el entorno.
setting-ui-font-name = Tipografía de la interfaz
setting-ui-font-desc = Familia tipográfica del chrome de la GUI (texto de ventana, no el listado). El TUI la ignora.
setting-ui-mono-font-name = Tipografía monoespaciada
setting-ui-mono-font-desc = Familia tipográfica de los listados y el visor de ficheros. El TUI la ignora.
setting-ui-font-size-name = Tamaño de letra
setting-ui-font-size-desc = Tamaño base de la tipografía en píxeles (8-32). El TUI lo ignora.
setting-ui-quick-search-name = Modo de búsqueda rápida
setting-ui-quick-search-desc = Qué hace `/`: filtrar el listado (filter) o mover el cursor sin cambiarlo (jump).
setting-ui-reduce-motion-name = Reducir movimiento
setting-ui-reduce-motion-desc = Desactiva los efectos animados (flicker CRT, parpadeo del cursor) por accesibilidad. Solo GUI.
setting-ui-mouse-name = Ratón
setting-ui-mouse-desc = Si la TUI captura el ratón (click, rueda, arrastre para marcar). Mientras está capturado el terminal no puede seleccionar texto con el ratón; mantén Mayús para seleccionar igualmente, o desactiva esto. La GUI lo ignora.
setting-ui-confirm-quit-name = Confirmar antes de salir
setting-ui-confirm-quit-desc = Al salir pide confirmación: solo con trabajo pendiente (auto), siempre, o nunca. Un atajo de salida de emergencia, donde esté ligado (p. ej. Ctrl+C en la TUI), siempre lo evita.
setting-keymap-preset-name = Preset de keymap
setting-keymap-preset-desc = Preset base de atajos de teclado (orthodox, vim o cua). Las capas de usuario/proyecto pueden seguir rebindeando encima.
