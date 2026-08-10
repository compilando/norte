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
# H3c: el pie de un modal al que una página de ayuda está tapando. Mientras
# esa ayuda esté abierta se queda las teclas, así que los verbos del modal no
# hacen nada — un pie que siguiera ofreciéndolos mentiría. La caja y la
# pregunta siguen a la vista (el modal se pinta el último); lo que se sustituye
# son los verbos por lo que es verdad.
modal-hint-help-open = cierra la ayuda para responder a esto
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
# `pane.command-line` (#135): el prompt de texto libre cuyo Enter corre
# `$SHELL -c CMD` en el directorio del pane, con la TUI suspendida. Mismo
# molde que los prompts de mkdir/renombrado IA.
modal-command-line = Ejecutar un comando
modal-command-line-hint = se ejecuta en el directorio del pane activo
modal-command-line-empty = escribe un comando primero
modal-command-line-too-long = la línea de comandos está llena ({ $max } caracteres); el resto no se escribió
modal-ai-rename = Renombrado IA — instrucción
modal-ai-rename-hint = Enter: pedir plan · Esc: cancelar
modal-ai-rename-empty-instruction = escribe una instrucción primero
modal-ai-rename-plan = Renombrado IA — plan propuesto
modal-ai-rename-dir = en: { $dir }
modal-ai-rename-pair-from = { $n }. { $from }
modal-ai-rename-pair-to = → { $to }
modal-ai-rename-more = … { $shown }/{ $total } (desplazar: ↓/↑)
modal-ai-rename-plan-hint = y/Enter: aplicar · n/Esc: descartar
# El plan no se puede aplicar (colisiones, o aún comprobándose): el pie no
# puede ofrecer una tecla que no hace nada.
modal-rename-batch-plan-hint-blocked = n/Esc: descartar
# Estado del plan transaccional del lote (fs.rename_batch_plan), bajo las
# parejas. "pending" = el core todavía no ha contestado.
modal-rename-batch-pending = lote: comprobando…
modal-rename-batch-applicable = lote: aplicable — una task, un deshacer
modal-rename-batch-not-applicable = lote: NO aplicable — no se renombrará nada
# La comprobación no llegó a hacerse (o falló). Distinto de «comprobando…»:
# aquel se resuelve solo, este no, y un spinner que no avanza nunca es una
# mentira. El motivo se fue a la barra.
modal-rename-batch-unchecked = lote: SIN comprobar — no se renombrará nada
# Maquinaria del planificador: renames por un nombre temporal para romper un
# ciclo. Solo se enseña el NÚMERO; los nombres temporales jamás se presentan
# como propuestas.
modal-rename-batch-temp = con { $n } paso(s) interno(s) (un ciclo necesita un rodeo)
# Una colisión POR LÍNEA. El nombre ofensor va el ÚLTIMO para que un recorte
# jamás pueda comerse el veredicto.
modal-rename-batch-collision = ✗ { $n }. { $kind }: { $name }
# La misma línea para un veredicto cuyo pair_index no señala ninguna fila de
# la petición: se cae el índice antes que señalar una fila que no está.
modal-rename-batch-collision-unindexed = ✗ { $kind }: { $name }
modal-rename-batch-collision-internal = otra pareja se lo llevó
modal-rename-batch-collision-external = ya existe
modal-rename-batch-collision-absent-source = el origen no está
modal-rename-batch-collision-ambiguous-source = origen ambiguo
modal-rename-batch-collision-unknown = veredicto desconocido
modal-rename-batch-collision-more = … { $shown }/{ $total } colisiones
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
err-plan-stale = la carpeta cambió; revisa otra vez el plan de renombrado
err-plan-not-executable = el plan de renombrado tiene colisiones de nombre
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
# El rastro de atrás/adelante se acabó: la tecla lo DICE, porque una tecla
# que calla es indistinguible de una rota.
msg-nav-no-back = no hay más atrás
msg-nav-no-forward = no hay nada hacia delante
# Un pane virtual de búsqueda no es una ubicación: una lista de hits no se
# puede mandar al otro pane ni traer de él.
msg-pane-not-a-location = los resultados de búsqueda no son una ubicación: no hay nada que mandar
# Un pegado con salto de línea jamás debe enviar un campo (#143): solo se
# inserta la primera línea, y esto dice cuántas más se descartaron.
msg-paste-truncated = pegada la primera línea; { $lines } descartadas
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
# `--cd-file` (S3, shell.rs `cd_bytes`): el pane activo no era `file://`, así
# que no se escribió nada en el cd-file y el wrapper deja el shell donde
# estaba. `$path` ya pasó por el enmascarado de `path_display`, con un `!` al
# principio en vez del color de la insignia que una línea de stderr no puede
# llevar.
msg-cd-not-local = el pane activo era { $path }; el shell se queda donde está
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
# S3 (shell-integration): el vocabulario de `Shell::parse` son exactamente
# estos tres — nombrados aquí en vez de repetir el nombre tecleado como hace
# `cli-help-unknown-topic`, porque este es un conjunto cerrado, no un corpus
# donde el usuario pudiera haberse equivocado de página nueva.
cli-shell-init-unknown = shell desconocido «{ $shell }» — soportados: bash, zsh, fish
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
cli-audit-chain-unknown-format = el journal declara el formato { $declared } y este binario conoce el { $known }: NO puede verificarlo — actualiza norte y vuelve a verificar. Ni es un visto bueno ni es una acusación: un marcador re-declarado se ve igual desde aquí, así que lee el informe de anclas de abajo antes de creerte ninguna de las dos lecturas.
cli-audit-chain-unverifiable-from = primera entrada que este binario no supo recomputar: { $seq }
cli-audit-chain-not-certified = la cadena NO quedó certificada por este binario (veredicto que esta versión no conoce): trata el journal como sin verificar
cli-audit-format-unreadable = ilegible
cli-audit-no-anchors = sin fichero de anclas: `norte audit anchor` fija el head actual (la ausencia es FALLO salvo --allow-no-anchors: un atacante puede simplemente borrar el fichero)
cli-audit-anchor-bad = ancla de la línea { $line } FALLIDA: { $detail }
cli-audit-coverage = cobertura de anclas: hasta seq { $anchored } de head { $head } de la cadena
cli-audit-verdict-bad-line = línea de ancla ilegible
cli-audit-verdict-bad-mac = MAC inválido (¿ancla fabricada o clave rotada sin re-anclar?)
cli-audit-verdict-missing = el seq { $seq } anclado YA NO EXISTE (truncación de cola/rollback)
cli-audit-verdict-mismatch = el seq { $seq } existe con OTRO hash (historia reescrita)
cli-audit-anchors-ok = { $count } ancla(s) verificadas contra la cadena
cli-audit-anchors-ok-unverified-chain = { $count } ancla(s) casan con los hashes ALMACENADOS — que no es una cadena verificada: este binario no pudo recomputar las entradas que cubren
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
status-connections-degraded = ⚠ { $scheme }://{ $host } — texto plano (+{ $n } más)
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
msg-ai-rename-invalid-plan = renombrado IA: plan inválido del daemon — no se aplicó nada
msg-ai-rename-in-search = el renombrado IA no está disponible en un pane de búsqueda
# El plan IA se aplica por el ejecutor transaccional de lotes (spec §17): UNA
# task, UNA unidad deshacible del journal, rollback si falla.
msg-rename-batch-plan-failed = lote de renombrado: no se pudo comprobar el plan: { $error }
msg-rename-batch-no-plan = lote de renombrado: sin plan comprobado — no se aplicó nada
msg-rename-batch-collisions = lote de renombrado: el plan colisiona — no se aplicó nada
msg-rename-batch-applied = lote de renombrado: { $n } renombrado(s) enviados como UN lote
msg-rename-batch-failed = el lote de renombrado falló: { $error }
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
# H3b: Enter sobre una fila de la ayuda que documenta un verbo de OVERLAY
# (`dialog.*`). No son despachables desde un pane, así que no corre nada — y
# se dice, porque un Enter comido se lee como un comando que sí corrió.
msg-help-not-runnable = esa fila documenta una tecla de overlay, no un comando que un pane pueda correr
msg-help-modal-waiting = contesta primero al diálogo: un comando lanzado desde aquí lo echaría de la pantalla
# H3c (review MAJOR-2): F1 sobre un diálogo que ninguna página documenta
# todavía. NO se abre el índice encima — una página que tapa una pregunta viva
# le congela las teclas y habla de otra cosa —, así que se dice y el diálogo
# sigue contestable.
msg-help-no-dialog-page = ninguna página explica este diálogo todavía: contéstalo y pulsa F1 para el índice
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
# #135 (S4) — suspensión. Un pane que no es `file://` no tiene directorio
# donde pueda sentarse un shell local, así que se declina en vez de abrirlo en
# otro sitio. `$path` llega ya saneado (`path_display`): la línea aterriza en
# la terminal del usuario, después de que norte la haya soltado.
msg-shell-remote = el pane activo es { $path }; un shell ahí no estaría donde estás mirando
msg-shell-failed = no se pudo ejecutar { $program }: { $error }
# El pane SÍ es local, pero su forma nativa solo la expresa el espacio de
# nombres verbatim `\\?\` (un nombre de dispositivo reservado, un punto o un
# espacio final, o más de 260 caracteres) — y `CreateProcessW` no lo acepta.
# Quitarlo abriría el hijo en otro sitio sin decirlo.
msg-shell-cwd-unsupported = { $path } no puede ser el directorio de trabajo de un programa en este sistema
# Se imprime en la terminal ANFITRIONA tras una suspensión que espera
# (`Ctrl+O`, y después de una línea de comandos): los paneles ya no están y
# esto es lo único que le dice al lector que norte sigue ahí.
msg-shell-press-key = [norte] pulsa una tecla para volver
# El `app.terminal` de la GUI (§E): nada respondió en este escritorio, así que
# se dice qué se intentó en vez de no hacer nada.
# `$configured` es el $TERMINAL del usuario y `$tried` la lista cerrada de
# norte. Van SEPARADOS a propósito: juntar un valor ajeno en un informe
# separado por `", "` deja que un ajuste se lea como dos entradas
# (`TERMINAL='kitty, konsole'`), que es el spoof de la flecha con una coma.
# Nada ajeno comparte jamás separador con nada.
msg-terminal-none = $TERMINAL es { $configured } y no se encontró ningún emulador de terminal; norte probó además su propia lista ({ $tried })
msg-terminal-none-unset = $TERMINAL no está definido y no se encontró ningún emulador de terminal; norte probó { $tried }
help-cmd-app-terminal = abrir un shell en el directorio del pane activo
help-cmd-app-toggle-panels = ocultar los paneles y enseñar la terminal
help-cmd-pane-command-line = ejecutar un comando en el directorio del pane activo
cli-ls-skipped = aviso: { $n } entradas del contenedor omitidas del índice (nombres hostiles/límites)

# --- Ayuda (F1) — construida del keymap efectivo ---
help-title = Ayuda
help-section-browse = Navegación (panes)
help-section-viewer = Viewer
help-section-dialog = Diálogos y overlays
help-dialog-note = cada diálogo soporta su propio subconjunto de estas teclas
# La advertencia de la hoja de TECLADO (K3b), impresa una vez bajo su título.
# Dos afirmaciones, y la segunda es la que merece la línea: una fila sin la
# marca es un comando que norte SÍ ha construido, pero este comando no tiene
# frontend y no puede decir cuál de los dos lo implementa — preguntarlo
# exigiría una app en marcha.
keys-page-note = Las teclas que norte aún no ha construido también se listan, marcadas con el issue que las sigue. Una tecla sin esa marca está construida; qué frontend la implementa no se puede saber desde aquí.
# --- Overlay de ayuda (H3b) — cabeceras de grupo de la lateral y página
# sintética de teclado. `help-group-{tag}` se busca por el PRIMER tag del
# propio corpus (ver el front matter de `norte-help/topics/*/*.md`): un tag
# nuevo allí necesita su entrada aquí, en ambos locales, o la lateral pinta
# la clave de búsqueda.
help-group-basics = Fundamentos
help-group-doing = Operaciones
help-group-remote = Remoto y archivos
# H3e: el grupo bajo el que van las páginas de plugin, detrás de todo lo que
# escribió el host. No es un tag del corpus — lo emite `norte_frontend::help`
# para las filas de plugin.
help-group-agents = Agentes y política
help-group-extensions = Extensiones
# H3e: la línea de procedencia bajo el título de una página de plugin.
# `help-plugin-origin` es INCONDICIONAL en toda página de plugin y el resto se
# añade si procede — un plugin que no declara publicador y manda un fichero
# limpio no debe poder hacer desaparecer la línea y que su página se lea como
# una del manual. `truncated`/`lossy` los dice el HOST, no el re-parseo del
# texto (que llega ya corto y ya decodificado): es el único sitio donde el
# lector se entera de que la página venía recortada.
help-plugin-origin = de una extensión
help-plugin-by = publicada por { $who }
help-plugin-truncated = recortada
help-plugin-lossy = hay bytes que no decodifican
# H3g: `norte help` escribe a un flujo que puede no ser un terminal, así que sus
# avisos son PALABRAS ASCII donde el TUI pinta `ℹ`/`⚠`/`💡`, y la fila de
# «ver también» se escribe en vez de insinuarse con un estilo.
help-see-also = Ver también
help-callout-note = nota
help-callout-warn = aviso
help-callout-tip = truco
cli-help-unknown-topic = ninguna página de ayuda se llama { $id } — `norte help --list` las nombra todas
cli-help-no-matches = nada coincide con { $query }
# La entrada sintética `keys` NO lleva cabecera: es un grupo de uno por
# construcción y su cabecera se llamaría igual que su única fila, así que la
# lateral no la pinta (`ui::draw_help`). Por eso no hay `help-group-keys`.
help-topic-keys = Teclado
# H3f: pie del overlay de ayuda de la GUI. Cadena FIJA, a diferencia del pie
# de la TUI, que se genera del keymap `dialog` — la GUI no tiene ese contexto
# y rutea estas teclas por nombre GPUI hardcodeado (`help_view::on_key`), así
# que un hint generado no tendría de qué generarse. Redefinir teclas no cambia
# estas, y esta cadena debe cambiar si cambia `on_key`.
help-hint-gui = ⇥ panel · ⏎ ejecutar · / filtrar · ⌫ atrás · Ctrl+P paleta · Esc cerrar
help-cmd-app-quit = salir de norte
help-cmd-app-help = esta ayuda
help-cmd-app-theme = elegir tema
help-cmd-app-extensions = gestor de extensiones
help-cmd-app-palette = paleta de comandos
help-cmd-app-settings = ajustes
help-cmd-app-pick-accept = aceptar la selección y salir (modo picker)
# --- Paleta de comandos (H1 T4) — editor de filtro libre como el diálogo
# de búsqueda (decisión 8): sus teclas son fijas, NO resuelven por el
# contexto `dialog` — este hint es una cadena estática, como `search-hint`.
#
# Las flechas y el paginado NO se listan, por lo mismo que dice
# `without_navigation` (H1 MAJOR-1): son autoevidentes y la caja mide 60
# celdas, así que escribirlas recortaba el resto del pie a media palabra.
palette-title = Paleta de comandos
palette-hint = [enter] ejecutar · [esc] cerrar
# H3c, y clave SEPARADA a propósito: `palette-hint` lo pintan los DOS
# frontends, y solo la TUI tiene overlay de ayuda que F1 pueda abrir (el de la
# GUI es la fase H3f). Metido en la cadena de arriba, el pie de la GUI
# anunciaría una tecla que allí no hace nada. Cuando llegue H3f, la GUI une
# también este grupo.
palette-hint-help = [f1] ayuda
# H3c: F1 sobre una fila abre la página que documenta ese comando. Si ninguna
# lo documenta, la palette se queda abierta y lo dice — abrir el índice
# dejaría al lector averiguando qué tenía que ver con lo que pidió.
msg-palette-no-help = ninguna página de la ayuda documenta este comando todavía
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
settings-hint-gui = [↑/↓/pgup/pgdn/click] navegar · [enter/click] editar · [ctrl+k] atajos · [esc] cerrar
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
# 2026-08-10-volumes.md §D: el picker de unidades (`pane.select-drive*`), un
# tercer `NavPopupKind` junto a historial y hotlist.
volumes-title = Unidades
volumes-empty = no se encontraron volúmenes
# Los dos modos del toggle "mostrar todo" dentro del popup (diseño §E) — el
# footer dice en cuál está, para que el toggle nunca calle qué hizo.
volumes-mode-filtered = sistemas de archivos de sistema ocultos
volumes-mode-all = mostrando todo
# Un tamaño que el filesystem no respondió a tiempo (diseño §A): nunca un `0`
# pelado, que se leería como «lleno» — justo lo contrario de «desconocido».
volumes-size-unknown = desconocido
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
msg-extensions-no-help = esta extensión no trae página de ayuda
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
help-cmd-nav-back = volver al directorio anterior
help-cmd-nav-forward = avanzar otra vez
help-cmd-pane-mirror = mandar esta ubicación al otro pane
help-cmd-pane-pull = ir a donde está el otro pane
help-cmd-pane-swap = intercambiar los dos panes
help-cmd-pane-copy = copiar la selección al otro pane
help-cmd-pane-move = mover la selección al otro pane
help-cmd-pane-delete = borrar (papelera si la hay)
help-cmd-pane-delete-permanent = borrar PERMANENTE
help-cmd-pane-view = ver el archivo seleccionado
help-cmd-pane-open = abrir el archivo seleccionado con un programa externo (openers.toml)
help-cmd-pane-quick-search = quick search en el pane (filtro/salto)
help-cmd-pane-history = historial de directorios
help-cmd-pane-hotlist = directorios favoritos
# 2026-08-10-volumes.md (cierra #131): el panel con foco, y los dos LADOS que
# nombran Alt+F1/Alt+F2 de Total Commander — no el foco, ver el diseño §D.
help-cmd-pane-select-drive = elegir unidad para el panel con foco
help-cmd-pane-select-drive-left = elegir unidad para el panel IZQUIERDO
help-cmd-pane-select-drive-right = elegir unidad para el panel DERECHO
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
gui-menu-copied = { $n } ruta(s) copiada(s) al portapapeles

# --- Por qué un comando no puede correr (H3d) — COMPARTIDAS por los dos
# frontends: la GUI apaga una entrada del menú contextual y la TUI atenúa una
# fila de la ayuda con el mismo texto, vía
# `norte_frontend::availability::reason_key`. Sin prefijo `gui-` a propósito.
# `reason-unavailable` es el fallback de una variante de `Reason` añadida
# después de este catálogo (el enum es `#[non_exhaustive]`).
reason-read-only = backend de solo lectura
reason-wrong-target = no aplica a esta selección
reason-unsupported = el backend no lo soporta
reason-plugin-inactive = la extensión está desactivada o sin aprobar
reason-policy-denied = la policy lo deniega
reason-connection-degraded = conexión degradada
reason-unavailable = no disponible ahora
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
gui-task-kind-rename-batch = rename
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
dialog-cmd-pane = otro panel
dialog-cmd-back = atrás
dialog-cmd-filter = filtrar

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
# S3 (shell-integration): si el wrapper se evalúa en el rc del shell no se
# puede ver desde un proceso hijo — misma limitación de honestidad que las
# dos notas de arriba —, así que esto nombra la instrucción en vez de un
# veredicto ok/mal.
cli-doctor-footer-shell-init = cd-on-quit: añade `eval "$(norte shell-init bash)"` (o zsh/fish) al rc de tu shell
cli-doctor-detail-connections-parse = connections.toml no parsea (corrígelo o bórralo)
cli-doctor-detail-connections-none = no hay connections.toml, o no hay conexiones configuradas
cli-doctor-detail-plugin-digest-stale = { $id }: las capabilities del manifiesto cambiaron desde la aprobación; requiere re-aprobación
cli-doctor-detail-plugin-help-truncated = { $id }: su help.md pasa del tope y se sirve cortado
cli-doctor-detail-plugin-help-lossy = { $id }: su help.md tiene bytes que no decodifican; se pintan como caracteres de reemplazo
cli-doctor-detail-plugin-help-empty = { $id }: anuncia un help.md que no sirve nada: vacío, ilegible, o un enlace que apunta fuera del directorio del propio plugin
cli-doctor-detail-plugin-help-bad-header = { $id }: su help.md abre una cabecera +++ que no parsea; la cabecera entera se ignora y su texto se lee como prosa
cli-doctor-detail-plugin-help-foreign-command = { $detail } — su help.md declara un comando que no es suyo; esa fila se descarta
cli-doctor-detail-plugin-help-shadows-topic = { $id }: su id es también una página de ayuda del binario; la del plugin no se muestra

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
keymap-unavailable-not-built = { $command }: aún no está construido ({ $reason }, issue #{ $issue })
keymap-unavailable-not-here = { $command }: no está disponible aquí
keymap-count-ignored = { $command } no acepta un contador (se ignoró { $count })
keymap-reason-archive-write = escritura de archivos comprimidos
keymap-reason-editor = editor integrado
keymap-reason-compare-sync = comparación y sincronización de directorios
keymap-reason-tree = panel de árbol de directorios
keymap-reason-tabs = pestañas de panel
keymap-reason-sort = comandos de ordenación
keymap-reason-properties = propiedades y tamaño de directorio
keymap-reason-connections = gestión de conexiones
# Forma CORTA, para una superficie cuya FILA ya nombra el comando: el panel
# which-key (K3a) y la hoja de referencia (K3b, `norte help keys` incluido).
# A diferencia de `keymap-unavailable-*` — la frase que pinta la barra de
# estado cuando la tecla SÍ se ha pulsado — quita el comando, pero mantiene
# «aún no construido»: la hoja de la CLI escribe a una tubería y no puede
# atenuar una fila, y el motivo a secas se leería como una descripción de lo
# que la tecla hace.
# `#132` y no `issue #132`, a diferencia de la forma larga: el panel
# which-key es la superficie sin sitio, y esos seis caracteres de más echaban
# el NÚMERO —lo único accionable— fuera del borde derecho de un panel de 60
# columnas. La frase ya dice «aún no construido», así que `#` no necesita
# presentación.
keymap-short-not-built = aún no construido ({ $reason }, #{ $issue })
keymap-short-not-here = no está disponible aquí

# Overlay which-key (K3a): el panel que lista lo que puede seguir a un
# prefijo pendiente.
whichkey-more-keys = más teclas
# Una fila que no cupo en el panel se CUENTA, jamás se cae en
# silencio: una caja que simplemente termina da a entender que la
# lista terminó con ella.
whichkey-truncated = … { $shown }/{ $total }

# Editor de atajos (K3c): la pantalla de Atajos, abierta desde Ajustes. Lista
# cada tecla ligada Y cada comando ejecutable que no pulsa nada — la hoja de
# referencia de arriba responde «qué hace esta tecla», y un editor tiene que
# responder además «cómo pulso X».
shortcuts-title = Atajos
shortcuts-hint = [↑/↓/repág/avpág] navegar · [enter] reasignar · [ctrl+u] desligar · [esc] cerrar
# Modo captura. Esc es lo que lo cancela, así que Esc es el único chord que el
# editor NO puede capturar, y se dice en vez de dejar al lector pulsándolo.
# `mod+` es la otra honestidad: crossterm no entrega ⌘ sin el protocolo de
# teclado de Kitty, que norte no activa, así que un chord capturado aquí es
# uno de Ctrl en cualquier plataforma (ADR 0043 decisión 9).
shortcuts-capture-hint = pulsa la tecla nueva · [esc] cancelar
shortcuts-capture-note = esc cancela (así que no se puede ligar aquí) · mod+ es ctrl en la terminal
shortcuts-confirm-hint = [enter] guardar · [retroceso] otra tecla · [esc] cancelar
# Una fila para un comando que ninguna tecla pulsa.
shortcuts-no-key = (sin tecla)
# El veredicto, mostrado ANTES de confirmar. Solo los dos primeros se pueden
# confirmar; el resto es lo que el cargador rechazaría, y negarse aquí es lo
# que evita un keymap.toml que revierte el mapa entero al recargar.
shortcuts-verdict-free = libre — no la usa nada más
shortcuts-verdict-replaces = sustituye a { $command }
shortcuts-verdict-replaces-unavailable = sustituye a { $command } ({ $reason })
shortcuts-verdict-prefix-clash = rechazada: { $chord } ya la usa ({ $command })
shortcuts-verdict-sacred = rechazada: reservada para { $command }
shortcuts-verdict-digit = rechazada: este preset lee los dígitos como repeticiones
shortcuts-verdict-empty = rechazada: no se capturó ninguna tecla
shortcuts-verdict-esc = rechazada: esc siempre cancela una secuencia pendiente
shortcuts-verdict-unwritable = rechazada: { $chord } no se puede escribir en keymap.toml
# La puerta (`rebind_dry_run`) se negó tras confirmar. El diagnóstico de carga
# NO se cita: puede llevar texto de una capa de proyecto que llegó con un
# repositorio clonado, y esto va a la barra de estado (#73).
shortcuts-refused-load = sin guardar: ese binding dejaría un keymap que no carga
shortcuts-refused-shadowed = sin guardar: { $command } se queda esa tecla (un keymap de proyecto manda sobre el tuyo)
shortcuts-refused-shadowed-unavailable = sin guardar: { $command } se queda esa tecla ({ $reason }, un keymap de proyecto manda sobre el tuyo)
# Inalcanzable mientras el editor está abierto (la misma búsqueda de preset
# construyó los mapas que enseña), y redactado en vez de dado por imposible:
# la regla 6 no hace excepciones con lo inalcanzable.
shortcuts-refused-preset = sin guardar: el preset de keymap activo es desconocido
msg-shortcut-bound = { $chord } ahora ejecuta { $command }
# El desligado de la GUI sigue casando byte a byte y hablando del FICHERO,
# jamás de lo que la tecla hace ahora (#141: un binding de `[global]`, un
# gemelo o otra capa que siga ligando la tecla convertirían «quitado» en
# verdad e inútil a la vez). El desligado de la TUI se redacta desde el mapa
# reconstruido — `msg-shortcut-unbound-cleared` abajo, o `msg-shortcut-bound`
# reutilizado si la tecla pasa a ejecutar otra cosa — y esta clave le queda
# solo para su propio no-op.
msg-shortcut-unbound = quitado de tu keymap.toml: { $chord } → { $command }
msg-shortcut-unbound-cleared = { $chord } ya no hace nada
msg-shortcut-nothing-to-unbind = no se quitó nada: nada de esa sección de tu keymap.toml casó con esa tecla
msg-shortcut-not-bindable = esa tecla no se puede capturar aquí
# K3c #141: un binding leído de `[global]` se fusiona en las tres pantallas,
# así que `Screen::section` nunca lo nombra y este editor no debe escribir ahí
# desde una fila que nombra una sola pantalla — cambiaría las tres. La fila
# dice dónde vive de verdad en vez de no hacer nada en silencio.
shortcuts-row-global = atado en [global]; edítalo en keymap.toml

# La redacción propia de la GUI para la misma pantalla (K3c c4). Dos cosas
# cambian respecto a la terminal y ninguna es cosmética. Tiene ratón, así que
# la pista nombra el click. Y SÍ ve Cmd/Super — gpui reporta `platform`, y
# crossterm no lo entrega jamás sin el protocolo de teclado Kitty que norte no
# activa —, así que un chord capturado aquí puede ser uno que la terminal no
# puede pulsar nunca. No es un rechazo (en esta ventana funciona), así que se
# dice en la fila al capturar y otra vez al confirmar, en vez de descubrirse
# meses después en la TUI.
gui-shortcuts-hint = [↑/↓/repág/avpág/click] navegar · [enter] reasignar · [ctrl+u] desligar · [esc] cerrar
gui-shortcuts-capture-note = esc cancela (así que no se puede ligar aquí) · ⌘ solo funciona en esta ventana
gui-shortcuts-cmd-note = ⌘ no es alcanzable en la terminal
# La escritura entró pero el keymap no: este frontend no vigila ficheros, así
# que un rebind llega al teclado solo por la reconstrucción que sigue a la
# escritura, y esa reconstrucción es todo-o-nada. Decir solo «guardado»
# describiría una tecla que no cambió.
gui-msg-shortcut-saved-not-applied = guardado, pero esta ventana conservó el keymap anterior
