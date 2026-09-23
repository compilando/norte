# Catálogo español de norte. Paridad total con en.ftl (test).

# --- Modales del TUI ---
modal-trash-title = A la papelera
modal-trash-note = recuperable desde la papelera del sistema
modal-delete-permanent-title = Borrar PERMANENTE
modal-delete-permanent-warning = ⚠ SIN papelera: esto no se puede deshacer
modal-copy-title = Copiar
modal-move-title = Mover
modal-drop-title = Copiar lo que soltaste
modal-collision-title = Colisión
modal-collision-body = el destino ya existe:
modal-approval-title = Aprobación de agente
modal-semantic-title = Buscar por significado
modal-semantic-scope = en todo el índice (se construye con `norte index build`)
msg-semantic-no-index = no hay nada indexado: corre `norte index build` y luego `norte index embed`
msg-semantic-unsupported = este daemon no sabe buscar por significado
msg-semantic-bad-hits = la respuesta no parecía un conjunto de resultados y se descartó
search-title-semantic = Por significado
modal-approval-ttl = caduca en { $s } s
modal-approval-ttl-unknown = plazo desconocido (reconstruida tras una reconexión): puede haber caducado ya
dialog-subject = pide:
dialog-asker = agente:
msg-approval-not-delivered = la aprobación { $id } no llegó al daemon: la operación sigue denegada
msg-approval-expired = la aprobación { $id } venció antes de tu respuesta: la operación quedó denegada
msg-approval-already-decided = la aprobación { $id } ya la había decidido alguien
msg-approval-unknown = la aprobación { $id } no es de este daemon (¿se reinició?)
msg-dialog-dropped = demasiados diálogos abiertos: se cerró el más viejo
modal-approval-body = el agente "{ $session }" pide { $op }:
modal-approval-mode = permisos: { $mode }
# #315: el ALCANCE. Sin estas dos líneas, un recursivo sobre una raíz se
# preguntaba como «1 ruta» y lo que se aprobaba era el árbol entero.
modal-approval-recursive = ¡y TODO lo que hay dentro!
modal-approval-recursive-dirs = ¡y TODO lo que hay dentro! (carpetas: { $mode })
# La ventana lo junta con la op, porque su diálogo tiene UN campo de sujeto.
modal-approval-op-mode = { $op } { $mode }
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
modal-ask-secret-title = Contraseña de la conexión
modal-ask-secret-conn = { $badge }conexión: { $conn }
modal-ask-secret-endpoint = { $badge }se conecta a: { $endpoint }
modal-ask-secret-field = contraseña: { $dots }
modal-ask-secret-note = solo para esta sesión: no se guarda en ninguna parte.
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
# #306: guardar el espacio de trabajo como perfil. El pie dice QUÉ se guarda,
# que es la pregunta de quien abre este diálogo.
modal-profile-save-as = Guardar como perfil
modal-profile-save-as-hint = nombre del perfil; se guardan la disposición y dónde está cada panel
modal-new-file = Crear fichero
modal-new-file-hint = nombre del fichero nuevo
modal-transfer-name-copy = Copiar a
modal-transfer-name-move = Mover a
modal-transfer-name-hint = nombre en el destino (edítalo para renombrar)
# Las dos etiquetas van en su COLUMNA, jamás dentro del texto: un directorio
# puede llamarse `docs → /casa/BORRAR`, y con la flecha in-band esa línea se
# lee como dos rutas.
modal-transfer-from = De
modal-transfer-to = A
# El destino tecleado: F5/F6 cuando no hay otro panel al que copiar (la
# disposición `simple`). La dirección va en forma wire, la misma que acepta
# `[[hotlist]]`, prellenada con la del propio panel.
modal-transfer-dest-copy = Copiar a la dirección
modal-transfer-dest-move = Mover a la dirección
modal-transfer-dest-hint = dirección, por ejemplo file:///home/tu/trabajo
# `pane.command-line` (#135): el prompt de texto libre cuyo Enter corre
# `$SHELL -c CMD` en el directorio del pane, con la TUI suspendida. Mismo
# molde que los prompts de mkdir/renombrado IA.
modal-command-line = Ejecutar un comando
modal-command-line-hint = se ejecuta en el directorio del pane activo
modal-command-line-empty = escribe un comando primero
modal-command-line-too-long = la línea de comandos está llena ({ $max } caracteres); el resto no se escribió
# El renombrado en lote SIN IA (#310): una plantilla y el mismo plan revisable.
# Las sumas de comprobación (#311).
modal-checksums-create = Sumas sha256
modal-checksums-verify = Comprobación de sumas
modal-checksums-hint = Enter: copiar la lista · Esc: cerrar
# Una comprobación no tiene digests que copiar: su pie no ofrece la tecla.
modal-checksums-hint-verify = Esc: cerrar
checksum-not-a-file = no es un fichero
checksum-unnameable = nombre imposible aquí
modal-checksums-more = … y { $n } más
checksum-ok = correcto
checksum-mismatch = NO CUADRA
checksum-missing = falta
checksum-unreadable = ilegible
msg-checksum-started = calculando sumas…
msg-disk-map-started = midiendo el directorio…
msg-disk-map-partial = el mapa se quedó a medias: lo medido es una cota inferior
msg-checksum-all-ok = { $n } comprobados, todos correctos
msg-checksum-bad = { $n } no cuadran o faltan
msg-checksum-copied = sumas copiadas al portapapeles
msg-checksum-nothing-to-copy = no hay ninguna suma que copiar
msg-checksum-not-a-sums-file = esto no parece un fichero de sumas
msg-checksum-done-hidden = sumas calculadas (cierra el diálogo para verlas)
msg-checksum-copied-osc52 = sumas copiadas (por el terminal; puede no honrarlo)
# Lo que no se pudo entender NO se calla: comprobar 37 de 40 líneas y decir
# «40 correctos» es la respuesta equivocada.
msg-checksum-unreadable-lines = { $n } comprobados; { $refused } líneas no se pudieron leer
msg-checksum-sums-too-big = el fichero de sumas es demasiado grande para comprobarlo entero
msg-checksum-sums-utf16 = el fichero de sumas está en UTF-16; guárdalo en UTF-8
msg-checksum-partial = el lote no terminó: lo calculado está a medias y no se compara
msg-checksum-failed = el cálculo de sumas falló
modal-rename-batch = Renombrar en lote — plantilla
modal-rename-batch-hint = `[N]` nombre · `[E]` extensión · `[C]` contador (`[C3]` con ceros) · Enter: pedir plan
msg-rename-pattern-empty = la plantilla está vacía
msg-rename-pattern-bad-result = esa plantilla dejaría un nombre vacío o con `/` dentro
msg-rename-batch-nothing = no hay nada que renombrar
msg-rename-batch-no-changes = la plantilla deja todos los nombres igual
# Organizar un directorio (fase 8). El resumen va ANTES del árbol: es lo que
# se necesita para decidir sin contar líneas.
modal-organize-plan = Organizar — revisa el árbol
modal-organize-summary = crea { $dirs } carpetas y mueve { $files } ficheros
msg-organize-running = pidiendo un plan para organizar…
msg-organize-empty = el plan no mueve nada
msg-organize-failed = no se pudo pedir el plan: { $error }
# Un plan que no se puede aprobar no se enseña: sin token no hay nada que
# canjear, y abrir el árbol prometería un botón que no puede hacer nada.
msg-organize-invalid-plan = el plan no es aplicable y se ha descartado entero
msg-organize-hidden = el árbol está listo: cierra este diálogo y vuelve a pedirlo
# Una extensión no lista directorios: los nombres se los damos nosotros, y hay
# un tope para esa lista. La IA no tiene este límite porque el core lista por
# ella.
msg-organize-too-many = este directorio tiene demasiadas entradas para que las organice una extensión
# El relevo entre frontends (fase 9). Cada negativa dice CUÁL de las dos cosas
# falta, porque se arreglan de formas distintas.
msg-handoff-needs-daemon = el relevo necesita el daemon: sin él no hay sesión que compartir (arranca con --daemon)
msg-handoff-needs-desktop = el relevo necesita un escritorio: aquí no hay dónde abrir una ventana
msg-handoff-failed = no se pudo entregar la pantalla; sigues donde estabas
# La ventana del relevo no llegó a vivir (salió enseguida, o no se pudo
# lanzar). La terminal se queda y recupera la pantalla: el motivo es el código
# de salida o el error del sistema, que es lo que permite averiguar por qué.
msg-handoff-window-died = la ventana no arrancó ({ $reason }); sigues aquí, con tu pantalla
msg-handoff-running = entregando la pantalla…
msg-handoff-no-terminal = no hay ningún emulador de terminal donde abrir `ntc`; sigues aquí, con tu pantalla
msg-handoff-terminal-failed = el emulador de terminal no arrancó; sigues aquí, con tu pantalla
host-handoff-not-owner = esta ventana no tiene la pantalla: la lleva otra, y sólo su dueña la puede entregar
modal-ai-rename = Renombrado IA — instrucción
modal-ai-rename-hint = Enter: pedir plan · Esc: cancelar
modal-ai-rename-empty-instruction = escribe una instrucción primero
modal-ai-rename-plan = Plan de renombrado — revísalo
modal-ai-rename-dir = en: { $dir }
modal-ai-rename-pair-from = { $n }. { $from }
modal-ai-rename-pair-to = → { $to }
modal-ai-rename-more = … { $shown }/{ $total } (desplazar: ↓/↑)
modal-ai-rename-real-steps = se renombrarán { $n } de verdad
modal-ai-rename-hidden-hostile = ⚠ un nombre que no se ve se pinta distinto de lo que es
modal-ai-rename-apply = Aplicar
modal-ai-rename-discard = Descartar
gui-modal-ai-rename-plan-hint = y o Aplicar: aplicar · n/Esc o Descartar: descartar
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
modal-rename-batch-collision-prefix = ✗ { $n }. { $kind }:
modal-rename-batch-collision-prefix-unindexed = ✗ { $kind }:
modal-rename-batch-temp-part = { $n } paso(s) temporal(es) del planificador
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
modal-volumes-more = … { $shown }/{ $total } (desplazar: ↓/↑)
msg-transfer-name-fffd = el nombre aún contiene el carácter de sustitución — reescríbelo limpio
msg-transfer-name-same = mismo nombre y sitio: nada que hacer
msg-transfer-name-failed = no se pudo encolar — el nombre se conserva

# --- Mensajes de la barra ---
msg-done = hecho
msg-pack-warnings = empaquetado, pero { $risky } nombre(s) significan otra cosa en otro sistema
msg-pack-warnings-partial = empaquetado, pero al menos { $risky } nombre(s) significan otra cosa en otro sistema
msg-cancelled = cancelado
msg-cancelling = cancelando…
msg-pausing = pausando… (se para al acabar el trozo o el fichero en curso)
msg-resuming = reanudando…
msg-pause-unsupported = este daemon no sabe pausar tareas: actualízalo para poder hacerlo
msg-pause-not-this = esta tarea no se puede pausar: solo copiar, mover y borrar
msg-retrying = repitiendo la transferencia…
msg-no-retry = no hay ninguna transferencia fallida que repetir
msg-queue-on = las transferencias que lances ahora van a la cola: de una en una
msg-queue-off = las transferencias que lances ahora van en paralelo
msg-queued-moved = movida en la cola
msg-queued-not-moved = ahí no se mueve: ya está corriendo, o ya es la primera
msg-no-tasks = no hay tasks en marcha
msg-task-finished = esa task ya había terminado
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
err-secret-needed = falta el secreto de la conexión (ponlo en NORTE_SECRET_<CONEXIÓN>)
err-host-key-mismatch = host key NO COINCIDE — posible MITM
err-cursor-expired = el listado expiró; refresca
err-plan-stale = la carpeta cambió; revisa otra vez el plan de renombrado
err-plan-not-executable = el plan de renombrado tiene colisiones de nombre
err-journal-unavailable = el journal de esta sesión no se puede abrir, así que no se ha tocado nada: nada quedaría registrado ni se podría deshacer. El motivo que sale junto a este error dice qué arreglar: el directorio de estado, o el journal.db de dentro
err-unknown = error desconocido
err-config-io = no se pudo leer { $path }: { $error }
err-config-parse = TOML inválido en { $path }: { $detail }
err-keymap-preset-unknown = preset desconocido { $name }; disponibles: { $available }
err-keymap-invalid = keymap inválido: { $detail }
# --- Scripting Lua (M4, ADR 0026) — detalles SIEMPRE por detail_for_bar ---
err-lua-load = init.lua ({ $layer }): { $detail }
err-lua-profile-ignored = el init.lua del perfil se ha ignorado: un perfil declara, no ejecuta código
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
msg-nav-no-jump-point = este panel no tiene punto de salto
msg-nav-jump-point-set = punto de salto fijado aquí
msg-history-cleared = historial vaciado
msg-popular-cleared = populares vaciados
msg-nav-no-forward = no hay nada hacia delante
# Un pane virtual de búsqueda no es una ubicación: una lista de hits no se
# puede mandar al otro pane ni traer de él.
msg-pane-not-a-location = los resultados de búsqueda no son una ubicación: no hay nada que mandar
# Un pegado con salto de línea jamás debe enviar un campo (#143): solo se
# inserta la primera línea, y esto dice cuántas más se descartaron.
msg-paste-truncated = pegada la primera línea; { $lines } descartadas
msg-refresh-error = refresh: { $error }
msg-view-error = view: { $error }
msg-viewer-no-sibling = no hay más ficheros de esta clase en este listado
msg-config-reloaded = config recargada
msg-profile-switched = perfil: { $profile }
msg-profile-switched-partial = perfil: { $profile } — hace falta reiniciar para aplicar: { $keys }
msg-profile-config-ignored = perfil: { $profile } — { $n } línea(s) del perfil no se entienden y se ignoran; mira el registro
msg-profile-start-orphans = [profile.start] nombra { $n } hueco(s) que esta disposición no coloca: { $ids }
msg-profile-not-applied = no se ha podido aplicar el perfil { $profile }
# #306: guardar el espacio de trabajo como perfil.
msg-profile-saved = perfil guardado: { $name }
msg-profile-save-failed = no se pudo guardar el perfil: { $error }
msg-profile-name-invalid = ese nombre no vale para un perfil: acaba siendo un directorio
msg-no-config-dir = no hay directorio de configuración donde guardarlo
msg-daemon-lost = conexión con el daemon perdida; reconectando…
msg-daemon-restored = reconectado al daemon
msg-daemon-handover = el daemon se releva; vuelve enseguida
msg-daemon-stopping = el daemon se está parando
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
# La cabecera declara un tamaño que esta ventana no va a decodificar. Un PNG
# de 64 KB puede declarar 60000x60000 y costarle gigabytes al decodificador;
# leerle la cabecera y negarse es la única defensa barata.
viewer-image-too-large = imagen demasiado grande para previsualizarla
# La cabecera no dice nada que entendamos. Tratar eso como «adelante» es la
# puerta que el presupuesto existe para cerrar.
viewer-image-unreadable = no se entiende la cabecera de la imagen
viewer-image-loading = cargando la imagen…
viewer-plugin-preview-lossy = [decodificación con pérdida]
# Ronda de arreglo 2 (Task 5, fase 5 WOW): el texto largo original no cabía
# junto a la posición de scroll (`pos`) en 80 columnas — ni siquiera solo.
# Acortado a lo que quepa en los dos idiomas con `pos` al lado.
viewer-image-needs-previewer = falta aprobar una extensión de vista previa de imagen — F12
# Task 5b (fase 5 WOW, hallazgo de revisión de T6): la contraparte de la
# clave de arriba para `Modo::Kitty`, que pide aprobar un plugin `thumbnail`
# (no un `previewer`) — la ayuda distingue las dos extensiones, y mandar al
# lector a aprobar la equivocada es peor que no avisar. Mismo presupuesto de
# 80 columnas con `pos` al lado.
viewer-image-needs-thumbnail = falta aprobar una extensión de miniaturas de imagen — F12
# El OTRO motivo por el que en Kitty no hay píxeles, y no se arregla desde
# F12: la extensión de miniaturas está aprobada, encendida y contestó, pero
# en JPEG o WebP, y el protocolo de kitty sólo coloca PNG (`f=100`). Con el
# aviso de arriba el lector iba a F12, lo encontraba todo en orden y se
# quedaba sin pista. No se interpola el mimetype: es texto de un TERCERO y
# la barra no es sitio para enmascararlo. Mismo presupuesto de 80 columnas.
viewer-image-thumbnail-format = la extensión de miniaturas no contestó en PNG
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
# `norte theme import` (spec 2026-09-11, F5).
cli-theme-imported = tema { $name } importado → { $path } (sobre { $base })
cli-theme-ignored = aviso: { $count } entradas no son colores y se han saltado: { $ids }
cli-theme-used = [ui] theme = "{ $name }" escrito en { $path }
cli-theme-err-no-config-dir = no se encuentra el directorio de configuración del usuario
cli-theme-err-name-invalid = nombre de tema no válido: usa letras, dígitos, -, _ y ., sin empezar por punto (--name)
cli-theme-err-name-preset = «{ $name }» es un preset embebido, que siempre gana a un fichero con ese nombre: elige otro --name
cli-theme-err-exists = { $path } ya existe: pasa --force para sustituirlo
cli-theme-err-depth = la cadena include tiene más de { $max } include
cli-theme-err-cycle = la cadena include vuelve a { $path }
cli-theme-err-read = no se puede leer el fichero de tema { $path }
cli-theme-err-too-big = { $path } pasa de { $max } MiB: no es un tema
cli-theme-err-parse = { $path } no es un tema de VSCode
cli-theme-err-binary = { $path } no es texto en un encoding que norte sepa reconocer: si es UTF-16 sin marca de orden de bytes, guárdalo como UTF-8
cli-theme-err-use = el tema se importó, pero no se pudo escribir [ui] theme
cli-theme-header-imported = Importado de un tema de VS Code con `norte theme import`.
cli-theme-header-source = Origen: { $file }
cli-theme-header-base = Base: { $base } — lo que el tema no define lo pone ese preset.
cli-theme-header-ignored = Ignorados, no son colores: { $count }
cli-list-failed = list falló
cli-entry-unreadable = entrada ilegible
cli-serialize-failed = no se pudo serializar
cli-compare-failed = compare falló
cli-compare-incomplete = compare no terminó limpiamente: { $state }
cli-sync-failed = sync falló
cli-sync-incomplete = el plan de sync no terminó limpiamente: { $state }
cli-sync-empty = nada que sincronizar
cli-sync-plan = Plan de sincronización:
cli-sync-unjournalled = sin journal esta sincronización no se puede aplicar — norte rehúsa escribir un árbol que no podría deshacer. Lo tiene otro proceso (un `ntc` o un daemon vivos abren journal.db en exclusiva): usa --daemon para ir por él
cli-sync-journal-unreadable = sin journal esta sincronización no se puede aplicar — norte rehúsa escribir un árbol que no podría deshacer. El journal de esta sesión NO SE PUDO ABRIR (no es que lo tenga otro proceso), así que `--daemon` no es remedio: el daemon se niega a arrancar con ese mismo fichero. El motivo de arriba dice qué arreglar
cli-ai-rename-refused = estos renombrados NO se han hecho: el journal de esta sesión no se pudo abrir, así que nada quedaría registrado ni se podría deshacer
cli-sync-noninteractive = no hay terminal a la que preguntar, y no se aplicó nada — usa --yes para aplicar sin pregunta
cli-sync-blocked = el plan no se puede ejecutar, así que no se aplicó nada
# Los últimos doce mensajes que el CLI seguía imprimiendo en español desde
# dentro del código (#319). Son avisos de arranque y notas de «no hay nada»:
# nadie los tradujo porque cada uno llegó solo, en un cambio que iba de otra
# cosa. `$error` es el mensaje de la librería y se queda FUERA de la
# traducción, como en todas partes.
cli-warn-no-index = aviso: índice no disponible ({ $error }); index.* dará Unsupported
cli-warn-ai-invalid = aviso: [ai] inválido ({ $error }); ai.* dará Unsupported
cli-warn-ai-load-failed = aviso: carga de [ai] falló ({ $error }); ai.* dará Unsupported
cli-no-results = (sin resultados)
cli-spool-swept = barridos { $count } planes de sync huérfanos
cli-spool-sweep-failed = aviso: no se pudo barrer { $path }: { $error }
cli-spool-partial = aviso: barridos { $removed } planes de sync huérfanos y { $failed } no se dejaron borrar en { $path }
cli-warn-ai-unavailable = aviso: proveedor de IA no disponible ({ $error }); ai.* dará Unsupported
cli-warn-archive-invalid = aviso: norte.toml no se pudo leer ({ $error }); los límites de [archive] se quedan en los de por defecto
# Se imprime bajo un plugin instalado y NO aprobado: instalar no activa, y
# decir solo «instalado» dejaría al lector esperando algo que no va a pasar.
cli-plugin-unapproved = queda SIN aprobar: apruébalo y actívalo en el gestor de extensiones
cli-plugin-installed = instalado: { $id } ({ $name })
cli-plugin-replaced = reemplazado: { $id } ({ $name })
# Reemplazar retira la aprobación: el .wasm es otro y el digest del manifiesto
# solo no lo habría notado.
cli-plugin-replaced-consent = consentimiento RETIRADO: el .wasm es otro binario y el digest del manifiesto no lo habría notado
cli-plugin-uninstalled = desinstalado: { $id }
# Desinstalar retira también la aprobación, a propósito: un plugin instalado
# después con el mismo id no debe heredar el consentimiento dado a otro binario.
cli-plugin-uninstalled-consent = su consentimiento se fue con él: uno con el mismo id se instala sin aprobar
cli-plugin-uninstall-invalid-id = no es un id de plugin: se espera reverse-DNS, p. ej. `org.foo.bar`
cli-plugin-uninstall-not-installed = { $id } no está instalado
cli-plugin-uninstall-io = desinstalando: { $error }
cli-plugin-uninstall-daemon-stale = hay un daemon en marcha y lo seguirá listando hasta reiniciarse; `norte --daemon plugin uninstall` lo quita a través del daemon
cli-plugin-list-empty = no hay plugins instalados
cli-plugin-list-broken = { $count } plugin(s) rotos sin listar — `norte doctor` dice por qué
cli-plugin-state-approved = aprobado
cli-plugin-state-unapproved = SIN aprobar
cli-plugin-state-enabled = activado
cli-plugin-state-disabled = apagado
# Un bloqueo son DOS campos en DOS líneas, no uno unido por `: ` — el mismo
# motivo que las filas de fallo de abajo (corpus `cause_join_spoof`).
cli-sync-blocker = { $rel }
cli-sync-blocker-why = { $why }
cli-sync-blockers-more = … y { $n } más
cli-sync-integrity = el plan de arriba no es todo el plan que se aplicaría, así que no se aplica: { $detail }
cli-sync-nothing-to-apply = todos los pasos son omisiones: no hay nada que aplicar
cli-sync-confirm = ¿Aplicar este plan? [s/N]
cli-sync-abort = cancelado; no se aplicó nada
cli-sync-done = aplicado: { $done } hechos, { $failed } fallidos, { $skipped } omitidos
cli-sync-cancelled = cancelado — lo aplicado antes del corte se queda, journalizado; el resto no se aplicó
# Un fallo son TRES campos en TRES líneas, jamás uno unido por `: ` y ` → `:
# los dos joiners son imprimibles corrientes que el enmascarado de nombres deja
# pasar, así que un nombre puede fabricar una fila entera en banda (corpus
# `cause_join_spoof`). Un salto de línea es Cc y sí se enmascara — un nombre no
# lo puede falsificar, y por eso es el separador estructural de una tubería.
cli-sync-step-dest = al destino: { $dest }
cli-sync-step-reason = porque: { $reason }
cli-sync-failure = { $rel }
cli-sync-failure-dest = en el destino: { $dest }
cli-sync-failure-cause = falló: { $cause }
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
cli-audit-anchored-marker = anclado también el marcador de formato (seq 0): una re-declaración del formato ya no pasa por «no sé leer esto»
cli-audit-marker-ok = el marcador de formato (seq 0) está anclado y casa
cli-audit-marker-unanchored = el journal declara un marcador de formato y NADA lo ancla: un marcador re-declarado o inyectado se vería igual desde aquí. Ejecuta `norte audit anchor`, y si ya lo habías hecho, su fichero de anclas ha desaparecido
cli-audit-only-marker = todavía no hay mutaciones: el marcador de formato queda anclado y no hay nada más que anclar
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
# Un relevo pedido a un daemon demasiado viejo para saber qué es. Hizo un
# apagado corriente, así que a los frontends no se les dijo nada y no van a
# volver solos — que conviene decirlo, porque si no la CLI daría por bueno algo
# que no ocurrió. Es la PRIMERA actualización a 0.46 por definición, o sea el
# caso normal y no un rincón.
cli-daemon-handover-unsupported = este daemon habla el protocolo { $version } y no sabe de relevos: se le ha parado sin más, así que las ventanas abiertas no van a reconectar solas
cli-daemon-handover-requested = relevo pedido: las ventanas volverán cuando el reemplazo esté en pie
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
status-connection-degraded = ⚠ sesión SIN cifrar
status-connections-degraded = ⚠ sesión SIN cifrar (y otras { $n })
status-degraded-subject = { $banner } — esquema { $scheme }, host { $host } ({ $reason })
degraded-reason-ftp-plaintext = FTP sin cifrado
degraded-reason-tls-auth-rejected = el servidor rechazó TLS
degraded-reason-unknown = motivo desconocido
# Un `plugin.notice` (0.69.0, ADR 0100): lo que un plugin hook dijo sobre una
# mutación que el journal registró, atribuido al plugin — y el daemon diciendo
# que apagó los hooks de un plugin. `$plugin` y `$text` son texto del wire, ya
# enmascarado.
msg-plugin-notice = ⚑ { $plugin }: { $text }
msg-plugin-hooks-disabled = ⚑ { $plugin }: sus hooks se apagaron tras tres fallos seguidos — desactívalo y vuelve a activarlo en el gestor de extensiones para reintentar
msg-plugin-effect-denied = ⚑ { $plugin }: tu policy denegó un fichero que este plugin pidió escribir (regla para actor "plugin"); se dice una vez mientras norte corre
status-connection-failed = ✗ no se pudo conectar
status-failed-subject = { $banner } — esquema { $scheme }, host { $host } ({ $reason })
failed-reason-secret-missing = falta el secreto
failed-reason-secret-empty = el secreto configurado está VACÍO
failed-reason-secret-not-utf8 = el secreto no es texto válido
failed-reason-secret-store = no se pudo leer el almacén de secretos
failed-reason-auth-rejected = el servidor rechazó las credenciales
failed-reason-no-user = falta el usuario
failed-reason-agent = el agente SSH no pudo autenticar
failed-reason-unknown = motivo desconocido
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
col-attr-posix-owner = Dueño
col-attr-posix-group = Grupo
col-attr-posix-nlink = Enlaces
col-attr-posix-ctime-ms = Cambiado
col-attr-win-attributes = Atributos
col-attr-s3-etag = ETag
col-attr-s3-content-type = Tipo de contenido
col-attr-archive-method = Método
col-attr-archive-packed-size = Comprimido
col-attr-archive-crc32 = CRC-32
pane-footer-counts = { $dirs } dirs · { $files } ficheros · { $size }
pane-footer-free = { $free } libres
status-marked = { $n } marcadas, { $size }
status-marked-with-dirs = { $n } marcadas, { $size } + { $dirs } dirs
# Los elementos de la mitad derecha de la barra de estado (ADR 0132): el
# rótulo de la columna de orden y el tooltip de cada uno.
status-item-sort-name = Nombre
status-item-sort-size = Tamaño
status-item-sort-mtime = Fecha
status-item-sort-ext = Extensión
status-item-position-tip = Posición del cursor en el listado
status-item-marks-tip = Lo marcado en este panel
status-item-sort-tip = Orden del listado. Pulsa para cambiarlo
status-item-encoding-tip = Cómo se leen los nombres. Pulsa para reinterpretarlos
status-item-tasks-tip = { $n } tareas en marcha. Pulsa para ver los procesos

## La barra de progreso ligera (ADR 0146)
strip-running-copy = copiando
strip-running-move = moviendo
strip-running-delete = borrando
strip-running-other = trabajando en
strip-paused = pausada
strip-done-copy = copiado
strip-done-move = movido
strip-done-delete = borrado
strip-done-other = hecho
strip-done-many = { $n ->
    [one] 1 hecha
   *[other] { $n } hechas
}
strip-failed = { $n ->
    [one] 1 falló
   *[other] { $n } fallaron
}
status-item-notices-tip = { $n } avisos sin leer. Pulsa para abrir el registro
# Un elemento aportado por un plugin (ADR 0137): de quién es y qué columna.
status-item-plugin-tip = Del plugin { $plugin }, columna { $column }, para la entrada bajo el cursor
status-marks-pruned = { $n } marcas caídas, sus entradas ya no están
status-watch-degraded = vigilancia de directorios degradada a sondeo (¿límite de inotify?) — crear/borrar/renombrar se ve en segundos; editar un fichero existente no se detecta
msg-names-encoding = nombres mostrados como { $enc } (solo display; los bytes no cambian)
msg-names-encoding-off = nombres tal cual (reinterpretación apagada)
msg-hidden-shown = entradas ocultas visibles
msg-project-config-skipped = { $n } configuración de proyecto no cargó y se ignoró
msg-mkdir-in-search = los resultados de búsqueda no tienen directorio destino — sal antes de la búsqueda
msg-ai-rename-running = Renombrado IA: pensando… (Esc cancela)
# Variante GUI: la GUI no tiene camino para abortar la petición en vuelo, así
# que no debe prometer "Esc cancela" (jamás una affordance falsa).
gui-msg-ai-rename-running = Renombrado IA: pensando…
# Un plan retenido (esperando tras un modal abierto) fue reemplazado por una
# petición más nueva antes de poder revisarse — la pérdida se dice, jamás muda.
gui-msg-ai-rename-superseded = Renombrado IA: plan anterior descartado (nueva petición)
msg-ai-rename-empty = Renombrado IA: el modelo no propuso cambios
msg-rename-plan-refused = la extensión no propone nada: { $why }
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
# Solo GUI (2026-08-10-volumes.md tarea V4): misma doctrina que el trío de
# búsqueda semántica de arriba — "cargando" no puede prometer "Esc cancela"
# (sin camino de aborto), una lista reemplazada lo dice en vez de perderla en
# silencio, y "failed" envuelve el texto de error ya aplanado del daemon.
gui-msg-volumes-running = Unidades: cargando…
gui-msg-action-rejected = la ventana envió una orden que el núcleo no entiende
gui-msg-volumes-superseded = Unidades: lista anterior descartada (nuevo resultado)
gui-msg-volumes-failed = no se pudieron listar las unidades: { $error }
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
# (después de una línea de comandos): los paneles ya no están y esto es lo
# único que le dice al lector que norte sigue ahí.
msg-shell-press-key = [norte] pulsa una tecla para volver
# `app.toggle-panels` le cede la terminal a un shell VIVO (#142), y la tecla
# que recupera los paneles es la misma que se los llevó. Un preset que ate el
# comando a una SECUENCIA no deja tal tecla —su primer acorde es del shell,
# que es donde el lector está tecleando—, así que no se cede la terminal en vez
# de cederla sin salida.
msg-subshell-no-key = ata app.toggle-panels a una tecla suelta para usar el shell: una secuencia no puede devolver los paneles
# El subshell es POSIX: un pty, un `cd` y los ganchos de prompt de bash/zsh/
# fish. En Windows se declina en vez de dejarlo a medias.
msg-subshell-not-here = el shell vivo detrás de los paneles no está disponible en este sistema
# El shell dijo dónde está y norte no ha podido ir. La ruta NO se interpola: es
# lo que el shell imprimió, y un seguimiento que a veces no pasa sin decir nada
# es indistinguible de uno roto.
msg-subshell-bad-cwd = el shell dice estar en un directorio que norte no puede abrir; el panel se queda donde estaba
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
help-cmd-app-handoff = entregar la pantalla al otro frontend y seguir allí (solo con daemon)
help-cmd-app-toggle-panels = ocultar los paneles y enseñar la terminal
help-cmd-pane-command-line = ejecutar un comando en el directorio del pane activo
cli-ls-skipped = aviso: { $n } entradas del contenedor omitidas del índice (nombres hostiles/límites)

# --- Ayuda (F1) — construida del keymap efectivo ---
help-title = Ayuda
help-toc = Secciones de esta página
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
help-hint-gui = ⇥ índice ↔ texto · ⏎ ejecutar · / filtrar · ⌫ atrás · Ctrl+P paleta · Esc cerrar
help-cmd-app-quit = salir de norte
help-cmd-app-help = esta ayuda
help-cmd-app-theme = elegir tema
help-cmd-app-extensions = gestor de extensiones
help-cmd-app-agents = sesiones de agente
agents-note = solo las sesiones de agente que ESTA ventana ha visto pedir permiso; no es el censo de agentes del sistema
agents-empty = ningún agente ha pedido permiso desde que se abrió esta ventana
agents-not-listening = esta ventana está montada sin efectos: no recibe peticiones de permiso, así que esta lista está vacía por eso y no porque nadie haya pedido nada
agents-forgotten = se han olvidado { $n } sesiones por el tope: la lista no está completa
host-undo-already-running = esa sesión ya tiene un deshacer en marcha
agents-title = Sesiones de agente
agents-counts = pidió { $seen }, aprobadas desde aquí { $approved }
host-no-session = no hay ninguna sesión señalada
modal-undo-session-title = ¿deshacer todo lo que hizo esta sesión?
modal-undo-session-scope = revierte TODAS sus operaciones, en orden inverso; lo que no se pueda revertir se dirá en el informe
help-cmd-app-palette = paleta de comandos
help-cmd-app-goto = ir a cualquier sitio
# Los títulos de las secciones de «ir a» (fase 6). El orden en que salen lo
# fija `norte_frontend::goto::ORDEN`, no este fichero.
goto-title = Ir a
goto-section-path = Ruta
goto-section-history = Historia
goto-section-popular = Frecuentes
goto-section-favorites = Favoritos
goto-section-connections = Conexiones
goto-section-commands = Comandos
goto-section-index = Encontrado por el índice
goto-empty = nada casa con eso
goto-index-off = el índice semántico no está encendido
goto-path-desc = ir a esta ruta
msg-goto-bad-path = esa ruta no se puede leer como destino
msg-goto-no-home = no hay HOME en el entorno, así que «~» no nombra nada
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
# El nombre de la barra de menús para un lector de pantalla. Solo la pinta la
# VENTANA: en el terminal la barra es una fila de texto y no hay landmark que
# etiquetar.
menu-bar-label = Barra de menús
# Lo mismo para la barra de paneles (#324), que en la ventana es una fila de
# botones con landmark propio.
panelbar-label = Barra de paneles
# Los botones de disposición de la derecha de la barra de menús (ADR 0133).
layout-buttons-label = Disposición
# Los botones de la ventana con la barra de título propia (ADR 0136).
window-controls-label = Ventana
window-minimize = Minimizar
window-maximize = Maximizar o restaurar
window-close = Cerrar
# La marca de novedad de un botón de la barra, para un lector de pantalla.
panelbar-attention = Novedades
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
settings-hint = [tab] lado · [[/]] sección · [enter] editar · [ctrl+r] restablecer · [esc] cerrar

settings-edit-hint = [enter] guardar · [esc] cancelar
# obsoleta: las 33 entradas vivían aquí. Se borra en el ciclo siguiente.
settings-section-general = General
settings-section-appearance = Apariencia
settings-section-panes = Paneles y listado
settings-section-open-with = Abrir con
settings-section-input = Teclado y ratón
settings-section-behavior = Comportamiento
settings-section-plugins = Plugins
settings-count = {$shown} de {$total}
settings-modified = no es el valor de fábrica
settings-default-placeholder = lo que norte trae
settings-reset = Restablecer
settings-reset-done = «{$name}» vuelve al valor de fábrica
# Quitar la clave de TU capa no siempre devuelve el valor de fábrica: si el
# perfil o el proyecto fijan la misma, el valor cambia y sigue sin ser el
# defecto. Se dice, en vez de dejar al lector creyendo que no funcionó.
settings-still-set-elsewhere = «{$name}» sigue fijado por otra capa (perfil o proyecto): no vuelve al valor de fábrica
settings-plugins-name = Ajustes de plugins
settings-plugins-note = Ningún plugin instalado declara ajustes configurables.
settings-plugins-open-hint = [enter] abrir los ajustes de este plugin
settings-plugins-key-count = {$count} ajustes
# --- Vista de ajustes de la GUI (S4) — swap a pantalla completa por ratón
# sobre el mismo catálogo/máquina de estado que el overlay de arriba.
# Tarea 4.5: la vista de ajustes en solo lectura de la ventana gráfica. La
# sección de rutas es DIAGNÓSTICO, no configuración: contesta «de dónde sale
# lo que estoy viendo». Solo ubicaciones — nunca un valor, o sea nada secreto.
settings-section-paths = Dónde vive cada cosa
settings-path-config-system = Configuración del sistema
settings-path-config-user = Tu configuración
settings-path-config-profile = Perfil activo
settings-path-config-project = Configuración del proyecto
settings-path-state = Estado (sesión, historial)
settings-path-logs = Logs
settings-path-socket = Socket del daemon
settings-path-missing = no está
settings-restart-badge = requiere reinicio
modal-setting-edit = Cambiar ajuste
settings-hint-gui = [↑/↓/pgup/pgdn/click] navegar · [enter/click] editar · [ctrl+k] atajos · [esc] cerrar
# P1: prefijo de una fila aportada por un plugin (`palette::plugin_rows`) —
# ninguna fila built-in lo lleva, así que un plugin no puede disfrazarse de
# comando built-in copiando su texto exacto.
palette-plugin-prefix = extensión
# Una fila de RENAMER (C3, ADR 0095): propone un plan de renombrado que se
# revisa como el de la IA. Otro rótulo porque hace otra cosa.
palette-renamer-prefix = renombrar
palette-organizer-prefix = organizar
theme-picker-title = Tema
columns-picker-title = Columnas — { $target }
columns-picker-target-default = todos los schemes
columns-picker-hint-toggle = activa
columns-picker-hint-move = mueve
columns-picker-hint-sort = ordena
columns-picker-hint-format = formato
columns-picker-hint-apply = aplica
columns-picker-hint-close = cierra
msg-columns-saved = columnas guardadas
ext-title = Extensiones
ext-empty = no hay extensiones instaladas
ext-unapproved = sin aprobar
# Tarea 4.5: el gestor de extensiones en solo lectura de la ventana
# gráfica reutiliza las tres de arriba y añade estas. El estado son DOS
# hechos independientes —aprobada, y encendida— porque una aprobada y
# luego apagada no es lo mismo que una que nadie ha mirado, y
# «cargando» tampoco es «ninguna». Las `ext-config-*` dicen qué acota
# una clave `[config]`; un `enum` enumera sus valores.
ext-loading = preguntando al daemon qué hay instalado…
ext-state-on = aprobada · encendida
ext-state-off = aprobada · apagada
ext-config-title = Sus ajustes
ext-commands-title = Comandos
# Puente 61: el gestor de la ventana gana una ficha con botones. Los cuatro
# verbos son los del teclado (`dialog.add`, `dialog.toggle-enabled`,
# `dialog.remove`, `app.help`) con su estado resuelto en la etiqueta, para
# que un botón nunca diga «alternar». Los contadores los compone el renderer
# como `N instaladas · M encendidas`: dos sustantivos, sin marcadores, porque
# el catálogo de la ventana no lleva argumentos que rellenar.
ext-approve = Aprobar
ext-revoke = Revocar
ext-enable = Encender
ext-disable = Apagar
ext-uninstall = Desinstalar
ext-help = Ayuda
ext-settings = Ajustes
ext-close = Cerrar
ext-installed = instaladas
ext-enabled = encendidas
ext-actions = acciones sobre esta extensión
ext-detail-hint = pulsa Intro, o la fila, para ver sus ajustes
ext-errors-title = No cargaron
ext-broken-only-uninstall = esta extensión no cargó: lo único que se puede hacer con ella aquí es desinstalarla
ext-broken-not-id = este directorio no se puede desinstalar desde aquí: su nombre no es un id de extensión, o una extensión cargada ya usa ese id — bórralo a mano
ext-counts-commands = comandos
ext-counts-columns = columnas
plugin-output-title = Salida de la extensión
plugin-output-empty = (no imprimió nada)
plugin-output-truncated = la salida se cortó: era más larga de lo que cabe
# La salida de un programa que la ventana corrió esperándolo (#312).
program-output-title = Salida del programa
program-output-compare = Comparar dos ficheros
# Lo que imprime un programa lanzado por `openers.toml` o por `[ui] editor`,
# cuando la regla dice que se le espera en vez de soltarlo.
program-output-open = Abrir
program-output-edit = Editar
program-output-failed = el programa no llegó a correr, o se pasó del plazo
# El programa de `[ui] diff` (o `diff`) no está en el PATH.
host-program-missing = no hay programa con el que hacerlo: no está en el PATH
ext-config-none = Esta extensión no declara ajustes.
ext-config-range = entre {$min} y {$max}
ext-config-min = al menos {$min}
ext-config-max = como mucho {$max}
# Tarea 4.5: los dos selectores de lista y la vista del tema de la
# ventana gráfica. El selector de conexiones NO está: leer
# connections.toml obliga a meter el crate de conexiones —russh, opendal,
# suppaftp, age, el llavero— en esta ventana para una lista que todavía no
# puede abrir ninguna. Los
# `effects` de un tema los interpreta cada renderer: los que este no sabe
# pintar se NOMBRAN, porque un tema retro idéntico se lee como roto.
picker-volumes-title = Volúmenes
picker-volumes-title-left = Volúmenes (panel izquierdo)
picker-volumes-title-right = Volúmenes (panel derecho)
picker-volumes-loading = preguntando al host por su tabla de montaje…
picker-connections-title = Conexiones
picker-connections-loading = preguntando al daemon qué conexiones hay…
picker-connections-empty = no hay conexiones configuradas (connections.toml)
picker-volumes-empty = el host no reportó ningún volumen
picker-history-title = Historial
picker-history-empty = este panel no ha estado en ningún otro sitio
picker-history-title-left = Historial — panel izquierdo
picker-history-title-right = Historial — panel derecho
picker-popular-title = Directorios populares
picker-popular-empty = todavía no hay directorios populares
picker-hotlist-title = Favoritos
picker-hotlist-empty = no hay favoritos configurados
picker-volume-space = {$free} libres de {$total}
# Se sabe cuánto queda pero no de cuánto. Pasa —un montaje que contesta a
# medias— y decir «desconocido» ahí tira el único número que hay: cuánto
# queda es la mitad que se mira antes de copiar.
picker-volume-free = {$free} libres
picker-volume-read-only = solo lectura
theme-title = Tema
theme-roles = Con qué se pinta cada papel
theme-effects-unsupported = Este tema declara efectos que esta ventana no pinta:
history-title = Historial
history-empty = todavía no hay historial
history-title-left = Historial — panel izquierdo
history-title-right = Historial — panel derecho
popular-title = Directorios populares
popular-empty = todavía no hay directorios populares
history-mark-current = aquí
history-mark-forward = adelante
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
# L3: el sidebar de sitios (`layout.places`) — un panel, no un popup, así que
# sus etiquetas son ETIQUETAS: hay 14 celdas y ahí no cabe una frase.
places-title = Sitios
places-section-drives = Unidades
places-section-favorites = Favoritos
places-empty = todavía nada
# L3: lo que el visor acoplado dice EN VEZ de un fichero. Un directorio no
# se lee nunca: el preview sigue al cursor, y leer lo que sea es como uno
# se convierte en abrir un dispositivo de bloque sin querer.
preview-title = Vista
preview-directory = directorio
preview-empty = nada seleccionado
preview-not-a-file = no es un fichero normal
# La ventana (#291): la lectura falló y el hueco lo dice, en vez de dejar
# puesto el fichero anterior como si fuera este.
preview-unreadable = no se pudo leer
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
msg-plugin-not-approved = esta extensión no está aprobada: apruébala antes de encenderla
modal-plugin-approval-title = ¿Conceder estas capacidades?
modal-plugin-approval-note = una extensión aprobada actúa en tu nombre con lo que aquí se lista
msg-extensions-no-help = esta extensión no trae página de ayuda
# G3c: `plugin.set_config` tuvo éxito — { $key } es la clave declarada por el
# manifiesto (charset seguro, nunca texto libre del plugin); { $value } es el
# valor nuevo, ya validado client-side.
msg-plugin-config-saved = { $key } guardado: { $value }
msg-settings-saved = { $name } guardado: { $value }
msg-settings-saved-restart = { $name } guardado: { $value } — hace efecto al reiniciar la ventana
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
msg-settings-invalid-status-items = cada elemento una vez, de: position marks sort encoding tasks notices
msg-settings-no-config-dir = sin directorio de config de usuario (entorno sin definir)
msg-hotlist-persist-failed = favoritos no guardados: { $error }
help-cmd-app-menu = barra de menús
menu-item-pane-view = Ver
menu-item-pane-properties = Propiedades
menu-item-pane-dir-size = Cuánto ocupa
menu-item-pane-open = Abrir con…
menu-item-pane-copy = Copiar
menu-item-pane-move = Mover
menu-item-pane-rename = Renombrar
menu-item-pane-mkdir = Crear directorio
menu-item-pane-delete = Borrar
menu-item-pane-delete-permanent = Borrar permanente
menu-item-app-quit = Salir
menu-item-mark-toggle = Marcar o desmarcar
menu-item-mark-all = Marcar todo
menu-item-mark-invert = Invertir marcas
menu-item-mark-clear = Quitar marcas
menu-item-mark-pattern-add = Marcar por patrón
menu-item-mark-pattern-remove = Desmarcar por patrón
menu-item-pane-switch = Cambiar de panel
menu-item-pane-mirror = Llevar al otro
menu-item-pane-pull = Traer del otro
menu-item-pane-swap = Intercambiar paneles
menu-item-layout-split-h = Partir lado a lado
menu-item-layout-split-v = Partir arriba y abajo
menu-item-layout-close-slot = Cerrar panel
menu-item-layout-grow = Agrandar
menu-item-layout-shrink = Encoger
menu-item-layout-equalize = Igualar
menu-item-layout-flip = Girar (lado a lado ↔ arriba y abajo)
menu-item-layout-set-target = Fijar destino
# Nombres CORTOS para la barra de paneles (#324). Aparte de las etiquetas del
# menú a propósito: aquellas son frases («Panel de sitios») y sus iniciales no
# distinguirían nada — tres empiezan por P.
panelbar-places = Sitios
panelbar-tree = Árbol
panelbar-viewer = Visor
panelbar-processes = Procesos
panelbar-metadata = Detalles
panelbar-log = Registro
panelbar-disk-map = Mapa
panelbar-timeline = Historial
# La línea de tiempo del journal (fase 7).
timeline-title = Línea de tiempo
timeline-empty = todavía no se ha hecho nada
timeline-loading = leyendo el historial…
timeline-batch = { $n } de golpe
timeline-irreversible = sin vuelta
timeline-unavailable = este daemon no guarda historial
timeline-undo-title = ¿Deshacer hasta aquí?
timeline-undo-body = Se va a deshacer lo que hiciste DESPUÉS de esta fila, de lo más nuevo a lo más viejo. La fila señalada se queda.
timeline-undo-count = { $n } entradas se deshacen
timeline-undo-skipped = { $n } no tienen vuelta y se saltan
timeline-undo-foreign = { $n } no son tuyas y no se tocan
timeline-undo-nothing = no hay nada tuyo que deshacer por encima de esa fila
msg-timeline-undo-running = deshaciendo hasta el punto elegido…

# El asistente de primer arranque (spec 2026-09-10): tres preguntas cuando
# aún no hay norte.toml de usuario. Esc en cualquier paso = «no volver a
# preguntar».
wizard-title = Bienvenido a norte
wizard-step-preset = teclas
wizard-step-theme = tema
wizard-step-icons = iconos
wizard-ask-preset = ¿Qué gestor de ficheros tienes en los dedos? Las teclas lo siguen.
wizard-ask-theme = Elige un tema. Lo que ves mientras te mueves es lo que eliges.
wizard-ask-icons = ¿Ves aquí tres iconos: 📁 🖼 ⚙ ? Si salen como cuadros, la fuente del terminal no tiene emoji.
wizard-icons-yes = Sí, los veo — usar iconos
wizard-icons-no = No — insignias ASCII
# Cabe ENTERO en la caja del asistente (66 celdas): recortado por la mitad se
# perdía justo `[Esc]`, la única tecla que dice cómo no contestar.
wizard-hint = [Intro] siguiente · [Retroceso] atrás · [Esc] no preguntar más
wizard-preset-orthodox = estilo mc: teclas F, Tab entre paneles. El de serie.
wizard-preset-vim = hjkl, contadores, : para la paleta.
wizard-preset-cua = Ctrl+C/X/V y teclas al estilo Windows.
wizard-preset-krusader = las teclas de Krusader, transcritas.
wizard-preset-far = las teclas de FAR Manager, transcritas.
wizard-preset-norton = las teclas de Norton Commander.
wizard-preset-total-commander = las teclas de Total Commander, transcritas.
wizard-done = Guardado. Cámbialo cuando quieras desde la pantalla de ajustes.
menu-item-layout-places = Panel de sitios
menu-item-layout-preview = Visor acoplado
menu-item-layout-processes = Panel de procesos
menu-item-layout-log = Registro
menu-item-layout-disk-map = Mapa de disco
menu-item-layout-timeline = Línea de tiempo
menu-item-layout-metadata = Panel de detalles
menu-item-layout-pick = Disposición...
menu-item-profile-pick = Perfil...
menu-item-pane-tab-new = Abrir pestaña
menu-item-pane-tab-close = Cerrar pestaña
menu-item-pane-tab-next = Pestaña siguiente
menu-item-pane-tab-prev = Pestaña anterior
menu-item-pane-tab-move-left = Mover a la izquierda
menu-item-pane-tab-move-right = Mover a la derecha
menu-item-pane-quick-search = Filtro rápido
menu-item-pane-search = Buscar en el árbol
menu-item-pane-semantic-search = Buscar por significado
menu-item-pane-compare-dirs = Comparar directorios
menu-item-pane-sync-dirs = Sincronizar
menu-item-pane-toggle-hidden = Entradas ocultas
menu-item-pane-columns = Columnas
menu-item-pane-sort-menu = Ordenar por…
menu-item-pane-names-encoding = Codificación de nombres
menu-item-app-theme = Tema
menu-item-app-settings = Ajustes
menu-item-app-extensions = Extensiones
menu-item-app-help = Ayuda
menu-item-app-palette = Paleta de comandos
menu-file = Archivo
menu-mark = Marcar
menu-panels = Paneles
menu-tabs = Pestañas
menu-find = Buscar
menu-view = Ver
menu-help = Ayuda
# Rótulos de sección dentro de un menú (ADR 0125). Solo los grupos que no se
# explican solos lo llevan; los demás son una raya sin nombre.
menu-section-archives = Archivos comprimidos
menu-section-pieces = Trozos
menu-section-integrity = Integridad
menu-section-by-pattern = Por patrón
menu-section-by-kind = Por clase
menu-section-history = Historial
menu-section-places = Sitios
menu-section-shell = Terminal
menu-section-contents = Contenido
menu-section-split = Dividir
menu-section-compare = Comparar
menu-section-side-panels = Paneles laterales
menu-section-profiles = Perfiles
# Diez grupos (2026-09-10): tres títulos nuevos y las etiquetas de lo que
# hasta ahora solo se alcanzaba por tecla o por la paleta.
menu-operate = Operar
menu-go = Ir
menu-tools = Herramientas
menu-item-pane-copy-path = Copiar la ruta
menu-item-pane-rename-batch = Renombrar en lote
menu-item-pane-ai-rename = Renombrar con IA
menu-item-pane-organize = Organizar…
menu-item-pane-chmod = Permisos
menu-item-pane-pack = Empaquetar
menu-item-pane-unpack = Desempaquetar
menu-item-pane-test-archive = Probar el archivo
menu-item-pane-split-file = Partir el fichero
menu-item-pane-combine-files = Unir los trozos
menu-item-pane-checksum = Sumas de comprobación
menu-item-pane-checksum-verify = Verificar sumas
menu-item-mark-restore = Restaurar marcas
menu-item-mark-extension-add = Marcar por extensión
menu-item-mark-extension-remove = Desmarcar por extensión
menu-item-mark-files = Marcar los ficheros
menu-item-mark-dirs = Marcar los directorios
menu-item-app-goto = Ir a…
menu-item-nav-parent = Subir
menu-item-nav-back = Atrás
menu-item-nav-jump-back = Volver al punto de salto
menu-item-nav-set-jump-point = Fijar punto de salto
menu-item-pane-history-left = Historial del panel izquierdo
menu-item-pane-history-right = Historial del panel derecho
menu-item-pane-popular = Directorios populares
menu-item-nav-forward = Adelante
menu-item-pane-history = Historial
menu-item-pane-hotlist = Favoritos
menu-item-pane-select-drive = Volúmenes
menu-item-pane-refresh = Refrescar
menu-item-pane-command-line = Línea de comandos
menu-item-app-terminal = Terminal aquí
menu-item-app-handoff = Seguir en la otra ventana
menu-item-layout-focus-next = Panel siguiente
menu-item-layout-focus-prev = Panel anterior
menu-item-pane-mirror-target = Llevar lo del cursor al otro
menu-item-app-toggle-panels = Ocultar o enseñar los paneles
menu-item-pane-compare-files = Comparar dos ficheros
menu-item-app-agents = Agentes
menu-item-profile-save-as = Guardar perfil como…
help-cmd-pane-switch = cambiar de pane
help-cmd-pane-tab-new = abrir pestaña
help-cmd-pane-tab-close = cerrar pestaña
help-cmd-pane-tab-next = pestaña siguiente
help-cmd-pane-tab-prev = pestaña anterior
help-cmd-pane-tab-move-left = mover pestaña a la izquierda
help-cmd-pane-tab-move-right = mover pestaña a la derecha
help-cmd-pane-tab-goto-1 = ir a la pestaña 1
help-cmd-pane-tab-goto-2 = ir a la pestaña 2
help-cmd-pane-tab-goto-3 = ir a la pestaña 3
help-cmd-pane-tab-goto-4 = ir a la pestaña 4
help-cmd-pane-tab-goto-5 = ir a la pestaña 5
help-cmd-pane-tab-goto-6 = ir a la pestaña 6
help-cmd-pane-tab-goto-7 = ir a la pestaña 7
help-cmd-pane-tab-goto-8 = ir a la pestaña 8
help-cmd-pane-tab-goto-9 = ir a la pestaña 9
help-cmd-layout-split-h = partir lado a lado
help-cmd-layout-split-v = partir arriba y abajo
help-cmd-layout-focus-next = panel siguiente
help-cmd-layout-focus-prev = panel anterior
help-cmd-layout-close-slot = cerrar panel
help-cmd-layout-grow = agrandar panel
help-cmd-layout-shrink = encoger panel
help-cmd-layout-equalize = igualar paneles
help-cmd-layout-flip = girar el reparto del panel: lado a lado o uno encima del otro
help-cmd-layout-set-target = fijar destino
help-cmd-layout-places = enseñar u ocultar el panel de sitios
help-cmd-layout-preview = enseñar u ocultar el visor acoplado
help-cmd-layout-processes = muestra u oculta el panel de procesos
help-cmd-layout-log = muestra u oculta el registro de esta sesión
help-cmd-layout-disk-map = muestra u oculta el mapa de disco
help-cmd-layout-timeline = muestra u oculta la línea de tiempo
help-cmd-layout-metadata = muestra u oculta el panel de detalles
help-cmd-layout-pick = elige una disposición
help-cmd-profile-pick = elige un perfil
help-cmd-profile-next = perfil siguiente
help-cmd-profile-prev = perfil anterior
help-cmd-profile-save-as = guarda este espacio de trabajo como perfil
cmd-planned-profile-save-as = escribir un perfil desde lo que hay en pantalla llega con el resto de la edición de config
msg-layout-last-panel = no se puede cerrar el último panel
msg-layout-slot-closed = panel cerrado · { $chord } vuelve a partir
msg-layout-slot-closed-nokey = panel cerrado · «partir lado a lado», en el menú de paneles
# Un gesto de panel con tres o más y ninguno designado: adivinar cuál es «el
# otro» sería mandar un panel a un sitio que nadie eligió (ADR 0058 D7).
msg-no-target-designated = hay más de un panel: designa antes un destino
# Partir un hueco que ya no da para dos: la tecla se niega en vez de crear un
# panel que el reparto esconde en el mismo frame.
msg-layout-split-no-room = no cabe otro panel aquí
msg-layout-move-no-room = así no cabe: algún panel dejaría de verse
msg-transfer-dest-invalid = eso no es una dirección: {$err}
msg-transfer-dest-same = ahí es donde ya están: teclea otro destino
msg-layout-load-failed = no se pudo cargar el layout «{$name}»: {$err}
msg-session-secret-needed = «{ $conn }» necesita su contraseña: pulsa refrescar en ese panel
msg-session-slots-timeout = { $n } paneles no listaron a tiempo al arrancar: entra en ellos para rellenarlos
pane-unlisted = sin listar
modal-pack = Empaquetar en
modal-pack-title = Empaquetar
modal-pack-hint = Enter empaqueta · Esc cancela
modal-pack-hint-zip = zip · Enter empaqueta · Esc cancela
modal-pack-hint-tar = tar · Enter empaqueta · Esc cancela
modal-pack-hint-targz = tar.gz · Enter empaqueta · Esc cancela
modal-pack-hint-unknown = extensión desconocida — usa .zip, .tar, .tar.gz o .tgz
modal-split = Partir en trozos de
modal-split-title = Partir
modal-split-hint = 4096, 10M, 700M · los trozos van al otro panel · Enter parte · Esc cancela
# Permisos POSIX (#314). El título dice sobre CUÁNTAS entradas va.
#
# DOS ids y no un selector de plural: los args de este crate viajan como
# CADENA, así que Fluent no puede aplicar reglas de plural —ni con `[one]`, que
# nunca casa, ni con `[1]`, que compara un número contra un texto—, y el
# singular se leía «1 entradas». Quien elige es el código, que sí sabe cuántas
# son.
modal-chmod-one = Permisos de 1 entrada
modal-chmod = Permisos de { $n } entradas
modal-chmod-hint = en octal (755) · «-R 755» baja por el árbol · «-R 644,755» pone otro a las carpetas · Enter aplica
msg-chmod-not-octal = eso no es un modo en octal: tres o cuatro dígitos del 0 al 7
msg-chmod-too-big = ese número se sale de los permisos: como mucho 7777
# #315: dos modos sin `-R` no significan nada — sin bajar por el árbol no hay
# carpetas a las que aplicárselo.
msg-chmod-dir-mode-needs-recursive = el modo de las carpetas solo vale con «-R»
# #121: si todo lo marcado son nombres que no son texto, la lista de nombres
# viajaría vacía — y vacía significa «el directorio entero», justo lo contrario
# de lo que se pidió.
msg-ai-rename-marks-not-text = lo que has marcado no son nombres de texto: el plan no se pide sobre eso
msg-chmod-started = cambiando los permisos de { $n }…
msg-chmod-partial = { $n } no se pudieron cambiar (un enlace, o no es tuyo)
msg-pack-read-only = ese panel es de solo lectura: ahí no se puede escribir
msg-pack-nothing = no hay nada marcado ni bajo el cursor
msg-pack-unknown-format = norte escribe .zip, .tar y .tar.gz; el .rar lo lee pero no lo escribe
msg-pack-bad-name = ese nombre no vale como nombre de fichero
msg-split-needs-file = partir es de un fichero, no de una carpeta
msg-split-bad-size = un tamaño como 4096, 10M o 700M
msg-unpack-not-archive = eso no es un archivo que norte sepa abrir
msg-unpack-started = desempaquetando en el otro panel
msg-test-archive-started = comprobando el archivo
msg-combine-needs-first = empieza por el primer trozo (.001)
msg-combine-started = juntando los trozos
msg-session-too-large = la sesión no cabe; se ha tirado el historial
msg-session-unreadable = la sesión guardada no se pudo leer; se arranca con la disposición de la configuración
msg-layout-applied = disposición aplicada: { $name }
layout-picker-title = Disposición
# Esta ventana todavía no escribe configuración: lo elegido vale para ESTA
# ventana y se pierde al cerrarla. Callarlo dejaría al usuario creyendo que
# acaba de configurar norte.
columns-picker-session-only = se aplica a esta ventana; no se guarda
# Tarea 6.1 en la ventana gráfica: buscar por un subárbol. «Corriendo» y
# «no casó nada» se dicen distinto, porque una lista corta que ya no crece
# y una que todavía crece se leen igual.
modal-search-title = Buscar en este árbol
err-empty-pattern = teclea un patrón: uno vacío casa el árbol entero
modal-mark-pattern-title = marcar por patrón (glob)
modal-unmark-pattern-title = desmarcar por patrón (glob)
err-bad-pattern = ese patrón no es un glob válido
# Tarea 4.1 en la ventana gráfica: qué dice cada fila del selector de
# disposiciones sobre sí misma. El aviso del nombre de teclado no es adorno:
# los dos ajustes comparten espacio de nombres, y sin la línea la
# coincidencia es una trampa.
layout-picker-factory = de fábrica
layout-picker-shares-keymap = también es un preset de teclado — no cambia ninguna tecla
layout-picker-mine = tuya
layout-picker-keymap-note = la disposición no cambia tus teclas (eso es [keymap] preset)
profile-picker-title = Perfil
profile-picker-empty = todavía no tienes perfiles — un perfil es un directorio en profiles/
profile-picker-broken = no carga
profile-picker-no-state = este nombre no se puede guardar: no recordará tus paneles
profile-picker-clash-layout = también hay una disposición así; elegir el perfil no es elegirla
profile-picker-clash-keymap = también hay un preset de teclas así; elegir el perfil no ata ninguna
profile-picker-clash-both = también hay una disposición y un preset de teclas así; el perfil no es ninguno
help-cmd-cursor-up = subir el cursor
help-cmd-cursor-down = bajar el cursor
help-cmd-cursor-page-up = subir una página
help-cmd-cursor-page-down = bajar una página
help-cmd-cursor-top = ir al principio
help-cmd-cursor-bottom = ir al final
help-cmd-nav-enter = entrar en el directorio seleccionado
help-cmd-nav-parent = subir al directorio padre
help-cmd-nav-back = volver al directorio anterior
help-cmd-nav-jump-back = volver al punto de salto del panel
help-cmd-nav-set-jump-point = marcar este directorio como punto de salto del panel
help-cmd-pane-history-left = historial de directorios del panel izquierdo
help-cmd-pane-history-right = historial de directorios del panel derecho
help-cmd-pane-popular = directorios más visitados
help-cmd-dialog-confirm-other = abrir en el otro panel
help-cmd-dialog-clear = vaciar la lista
help-cmd-dialog-top = ir al principio de la lista o del texto
help-cmd-dialog-bottom = ir al final de la lista o del texto
help-cmd-dialog-section-prev = ir a la sección anterior de la página
help-cmd-dialog-section-next = ir a la sección siguiente de la página
help-cmd-nav-forward = avanzar otra vez
help-cmd-pane-mirror = mandar esta ubicación al otro pane
help-cmd-pane-sync-nav = navegar los dos paneles a la vez
msg-sync-nav-on = navegación sincronizada: los dos paneles van juntos
msg-sync-nav-off = navegación sincronizada apagada
msg-columns-sort-session-only = columnas guardadas; el orden por atributo vale para esta sesión (norte.toml solo guarda los órdenes con tecla)
help-cmd-pane-mirror-target = mandar al otro pane la carpeta bajo el cursor
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
help-cmd-task-cancel = cancelar la task señalada, o la más reciente
help-cmd-task-pause = pausar la task señalada, o la más reciente; si ya está pausada, reanudarla
help-cmd-task-retry = repetir la transferencia que falló, con las mismas opciones
help-cmd-task-queue = mandar a la cola las transferencias que lances a partir de ahora, o dejar de hacerlo
help-cmd-task-up = subir en la cola la tarea señalada
help-cmd-task-down = bajarla
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
help-cmd-mark-extension-add = marcar las de la misma extensión que el cursor
help-cmd-mark-extension-remove = desmarcar las de la misma extensión que el cursor
help-cmd-mark-files = marcar los ficheros (no las carpetas)
help-cmd-mark-dirs = marcar las carpetas (no los ficheros)
help-cmd-mark-restore = devolver la selección anterior
# Marcar MOVIÉNDOSE. `mark.toggle` es el que baja, así que estos dicen la
# dirección: sin ella, dos filas de la hoja de referencia se leen igual.
help-cmd-mark-toggle-up = marcar o desmarcar y subir
help-cmd-mark-toggle-page-down = marcar (o desmarcar) una página hacia abajo
help-cmd-mark-toggle-page-up = marcar (o desmarcar) una página hacia arriba
help-cmd-mark-to-top = marcar hasta arriba y desmarcar el resto
help-cmd-mark-to-bottom = marcar hasta abajo y desmarcar el resto
# Marcar por extensión y por clase (#313).
msg-mark-no-extension = lo que hay bajo el cursor no tiene extensión que marcar
msg-marks-restored = selección anterior devuelta: { $n } marcadas
msg-marks-nothing-to-restore = no hay ninguna selección anterior que devolver
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
help-cmd-pane-sort-name = ordenar por nombre
help-cmd-pane-sort-ext = ordenar por extensión
help-cmd-pane-sort-size = ordenar por tamaño
help-cmd-pane-sort-time = ordenar por fecha
help-cmd-pane-sort-menu = elegir el orden (abre el diálogo de columnas)
help-cmd-pane-properties = propiedades de la entrada
help-cmd-pane-chmod = cambiar los permisos POSIX
help-cmd-pane-checksum = calcular la suma sha256 de lo marcado
help-cmd-pane-checksum-verify = comprobar un fichero de sumas contra lo que lista
help-cmd-pane-dir-size = contar cuánto ocupa
help-cmd-pane-pack = empaquetar en un archivo
help-cmd-pane-unpack = desempaquetar en el otro panel
help-cmd-pane-test-archive = comprobar un archivo
help-cmd-pane-split-file = partir un fichero en trozos
help-cmd-pane-combine-files = juntar los trozos
help-cmd-pane-edit = editar con tu editor
help-cmd-pane-edit-new = editar un fichero nuevo
help-cmd-pane-connect = abrir una conexión guardada
help-cmd-pane-tree = árbol de directorios
menu-item-pane-tree = Árbol
tree-title = Árbol
tree-loading = leyendo…
help-cmd-pane-disconnect = cerrar la conexión de este panel
menu-item-pane-connect = Conectar…
menu-item-pane-disconnect = Desconectar
connections-picker-title = Conexiones
connections-picker-empty = no hay conexiones en connections.toml
msg-connect-bad-url = esa conexión tiene una dirección que no se entiende: { $url }
msg-disconnect-local = este panel es local: no hay conexión que cerrar
msg-disconnect-done = conexión cerrada
msg-disconnect-none = no había conexión abierta
menu-item-pane-edit = Editar
menu-item-pane-edit-new = Editar uno nuevo
msg-edit-nothing = no hay nada bajo el cursor que editar
msg-edit-not-a-file = eso es una carpeta: se entra con ⏎, no se edita
# #303: el fichero se creó, y entre crearlo y abrirlo dejó de ser un fichero
# regular. Un solo mensaje para las tres causas (enlace, carpeta, ya no está):
# decir cuál sería confirmárselo a quien puso el enlace.
msg-edit-created-changed = el fichero se creó, pero ya no es el que se creó: el editor no se ha abierto
msg-edit-created-unnamable = el fichero se creó, pero su ruta no se puede dar a un editor de esta máquina
# Y este es otro: no se pudo PREGUNTAR. Llamar manipulación a un daemon
# relevado es una acusación falsa, y enseñar a ignorar el mensaje de arriba es
# lo que lo inutiliza el día que sea verdad.
msg-edit-created-unchecked = el fichero se creó, pero no se pudo comprobar qué hay ahí: el editor no se ha abierto
# Comparar dos ficheros (#312): dos, y no se adivina cuáles.
msg-compare-files-need-two = marca DOS ficheros, o pon uno bajo el cursor en cada panel
msg-compare-files-not-files = eso es una carpeta: dos carpetas se comparan con «comparar directorios»
props-kind = clase
props-kind-dir = carpeta
props-kind-file = fichero
props-kind-symlink = enlace
props-kind-other = otra
props-size = tamaño
props-size-unknown = lo desconoce el backend
props-mtime-unknown = la desconoce el backend
props-modified = modificado
props-path = ruta
props-entries = { $count } entradas
props-counting = contando…
props-count-hint = sin contar (ciérralo y usa «contar cuánto ocupa»)
props-hint = [Esc] cerrar
msg-dir-size-counting = contando lo que ocupa…
msg-dir-size = { $size } en { $count } entradas
msg-dir-size-partial = al menos { $size } en { $count } entradas ({ $skipped } sin poder leer)
help-cmd-pane-mkdir = crear un directorio (F7)
help-cmd-pane-rename-batch = renombrar en lote con una plantilla (plan revisable)
help-cmd-pane-ai-rename = renombrado IA del directorio actual (plan revisable)
help-cmd-pane-organize = organizar el directorio en carpetas (plan revisable)
help-cmd-pane-semantic-search = búsqueda semántica sobre el índice (IA)
help-cmd-pane-rename = renombrar in situ (Shift+F6)
help-cmd-pane-refresh = recargar ambos panes (Ctrl+R)
help-cmd-viewer-encoding-auto = volver a la detección automática
help-cmd-viewer-hex = alternar vista hexadecimal
help-cmd-viewer-zoom-in = acercar la imagen
help-cmd-viewer-zoom-out = alejar la imagen
help-cmd-viewer-zoom-fit = ajustar la imagen al visor
help-cmd-viewer-next = pasar al siguiente fichero de la misma clase
help-cmd-viewer-prev = volver al anterior de la misma clase
help-cmd-viewer-left = una columna a la izquierda
help-cmd-viewer-right = una columna a la derecha
help-cmd-pane-search = buscar por nombre/contenido (Alt+F7)
help-cmd-pane-compare-dirs = compara los dos paneles y abre el panel de diferencias
help-cmd-pane-compare-files = compara DOS ficheros con el programa de `[ui] diff`
help-cmd-pane-sync-dirs = planifica una sincronización de un sentido de este panel al otro
help-cmd-pane-copy-path = copia la ruta de la selección al portapapeles
search-title = Búsqueda
search-name = nombre (glob):
search-content = contenido:
search-exclude = saltar carpetas:
search-min-size = al menos:
search-max-size = como mucho:
search-days = cambiado hace (días):
search-encoding = leer el texto como:
search-regex = [F2] regex: { $on }
search-case = [F3] mayús: { $on }
search-whole-word = [F4] palabra entera: { $on }
search-recursive = [F5] subcarpetas: { $on }
search-kinds = [F6] busco: { $what }
search-kinds-any = cualquier cosa
search-kinds-files = ficheros
search-kinds-dirs = carpetas
search-hint = [tab] campo · [enter] buscar · [esc] cancelar
# Las etiquetas de los interruptores en la VENTANA: sin el nombre de la tecla
# y sin el estado dentro del texto — ahí el estado es la casilla.
search-toggle-regex = regex
search-toggle-case = distinguir mayúsculas
search-toggle-whole-word = palabra entera
search-toggle-recursive = buscar en subcarpetas
search-toggle-kinds = busco
# La consulta que enseña la cabecera cuando la búsqueda es SOLO de filtros:
# «todo lo que pese más de un giga» no tiene patrón que enseñar.
search-query-filters-only = (solo filtros)
search-empty = introduce un nombre, un contenido o un filtro
search-bad-field = ese campo no se entiende — arréglalo o déjalo vacío
search-status-running = búsqueda: { $n } hits (buscando…)
search-status-done = búsqueda: { $n } hits
search-status-truncated = búsqueda: { $n } hits (truncada)
search-status-cancelled = búsqueda: { $n } hits (cancelada)
search-status-failed = búsqueda fallida: { $error }

# --- Comparación de directorios (2026-08-11-directory-comparison.md, `Shift+F2`) ---
# El panel de diferencias. Cada fila dice QUÉ se decidió, QUÉ criterio lo
# decidió y CUÁNTO vale ese criterio, y los tres son una palabra y no un color:
# la spec §17 pide una pista textual, y `igual/probable` frente a
# `igual/seguro` es justo el par que un lector daltónico no puede perder.
compare-title = Comparar
sync-title = sincronizar
sync-mode-update = actualizar
sync-mode-mirror = espejo
gui-sync-mode-update = ACTUALIZAR: no se borra nada en el destino
gui-sync-mode-mirror = ESPEJO: lo que no esté en el origen se BORRA en el destino
host-sync-read-only = esta ventana todavía solo LEE el plan · ↑↓ mover · Esc cerrar
host-sync-already = ya hay un plan abierto o en camino
sync-hint-applying = Esc pide cancelar · otro Esc cierra y renuncia al informe
msg-sync-cancelled-late = la sincronización ya había arrancado: se le pidió parar
msg-sync-closed-midway = cerrado mientras escribía: el destino puede quedar a medias y su informe se pierde
msg-sync-apply-unknown = no se sabe si llegó a arrancar: la conexión falló DESPUÉS de pedirlo, así que el destino puede estar escribiéndose. Mira el tablero de tareas antes de volver a aplicar
compare-header-left = izquierda
compare-header-right = derecha
compare-empty = todavía no hay filas
# Sí llegaron filas, y están todas las categorías apagadas. Decir «todavía no
# hay filas» ahí es una mentira que las propias cuentas de la línea de filtros
# desmienten — y lo que toca hacer (esperar, o pulsar 1-5) depende de cuál de
# las dos cosas sea.
compare-all-filtered = están todas las categorías ocultas — 1-5 las devuelve
compare-hint = tab lado · 1-5 filtro · ins marcar · s sinc · m espejo · enter ir · esc cerrar
# La línea de teclas PROPIA de la GUI, y la única clave de comparación que no
# se comparte. Es más corta porque su panel hace menos: marcar filas existe
# para sembrar un plan de sincronización, y esa superficie (#161) todavía no
# está construida aquí. Prometer `s sinc` en un frontend que no sincroniza
# sería un atajo documentado y muerto — este repo ya envió uno.
gui-compare-hint = tab lado · 1-5 filtro · ins marcar · s sincr · m espejo · enter ir · esc cerrar
gui-compare-marked = marcada
compare-active-side = actuando sobre: { $side }
compare-status-running = comparación: { $n } filas (comparando…)
compare-status-done = comparación: { $n } filas
compare-status-unknown = comparación: llegaron { $n } filas, y nada dijo si eran todas
# El flujo de filas terminó con MENOS filas de las que contó la task: se perdió
# un lote de notificación entre el daemon y aquí. Decir «hecho» sería mentir
# sobre lo completo que está, y en una comparación eso es toda la respuesta.
compare-status-incomplete = comparación: { $n } de { $total } filas (se perdieron algunas por el camino)
compare-status-cancelled = comparación: { $n } filas (cancelada)
compare-status-failed = comparación fallida: { $error }
# Se rechaza aquí, antes de ir y volver: el daemon también responde `-32602`,
# pero el lector merece la frase sin la espera.
compare-same-path = los dos paneles están en el mismo directorio — no hay nada que comparar
compare-no-target = esa fila no tiene nada en el lado { $side }
compare-side-left = izquierdo
compare-side-right = derecho
compare-side-unknown = un lado desconocido
compare-verdict-same = igual
compare-paired-under = los dos nombres se escriben distinto y aun así emparejan
compare-paired-under-singleton = AVISO: estos dos nombres pueden ser ficheros DISTINTOS que Unicode declara iguales
compare-paired-under-full-fold = AVISO: estos dos nombres pueden ser ficheros DISTINTOS que este filesystem pliega en uno
compare-verdict-different = distinto
compare-verdict-only-left = solo a la izquierda
compare-verdict-only-right = solo a la derecha
compare-verdict-type-mismatch = de otra clase
compare-verdict-ambiguous = ambiguo
compare-verdict-error = error
compare-verdict-unknown = veredicto que esta versión no conoce
compare-confidence-certain = seguro
compare-confidence-probable = probable
# NO es un fallo: un archivo comprimido sin fecha fiable, un object store cuyo
# ETag solo a veces es un hash. Un provider honesto, y la razón de que exista
# el vocabulario de confianza.
compare-confidence-unknown = no puede saberse
compare-confidence-unrecognised = confianza que esta versión no conoce
compare-criterion-presence = presencia
compare-criterion-kind = clase
compare-criterion-link-target = destino del enlace
compare-criterion-size = tamaño
compare-criterion-mtime = fecha
compare-criterion-hash = hash del contenido
compare-criterion-unknown = criterio que esta versión no conoce
compare-reason-case-fold = dos nombres de un lado difieren solo en mayúsculas
compare-reason-normalization = dos nombres de un lado difieren solo en la normalización Unicode
compare-reason-unreadable = el directorio no se pudo listar, o una entrada no se pudo consultar
compare-reason-dir-too-large = el directorio supera el límite de entradas
compare-reason-read-failed = falló una lectura durante el hash
compare-reason-unknown = motivo que esta versión no conoce
compare-filter-same = iguales
compare-filter-different = distintas
compare-filter-only-left = solo izq.
compare-filter-only-right = solo der.
compare-filter-problems = problemas

# --- Sincronización de directorios (2026-08-11-directory-sync.md) ---
# El vocabulario del diálogo de aprobación. La regla que cumple cada cadena de
# aquí: la columna `reversal` de un paso dice cómo VOLVERÍA, y solo la papelera
# del destino dice si va a volver. Una copia sobre un destino sin papelera
# viaja como `delete` y el undo se la salta — así que aquí nada promete que un
# paso vuelve sin decir sobre qué destino eso es verdad.
sync-step-create-dir = crear directorio
sync-step-copy = copiar
sync-step-overwrite = sobrescribir
sync-step-delete-tree = borrar
sync-step-skip = dejar como está
sync-step-unknown = paso que esta versión no conoce
# Neutral A PROPÓSITO: en una sobrescritura el undo trae de vuelta el fichero
# viejo, y en una copia BORRA el nuevo. «Lo devuelve» se lee como lo primero en
# los dos casos.
sync-undo-reverts = el undo lo revierte
sync-undo-left-behind = el undo lo deja donde está
sync-undo-irreversible = no se puede deshacer
sync-undo-nothing = no hay nada que deshacer
sync-undo-unclear = esta versión no lo puede decir
sync-reason-ambiguous-source = dos nombres del origen colapsan en uno — no se copia ninguno
sync-reason-unknown-confidence = el provider no pudo distinguir los dos lados
sync-reason-unreadable = no se pudo leer, así que el plan lo deja como está
# Cubre LOS DOS casos —no hay papelera, o la hay y no dice dónde deja las cosas
# (macOS, Windows)—: el token del wire no los separa, así que esta frase no
# puede afirmar ninguno de los dos.
sync-reason-no-trash-on-target = el destino no puede devolver esto: no tiene papelera, o tiene una que no dice dónde deja las cosas
sync-reason-non-injective-pairing = los dos nombres pueden ser ficheros DISTINTOS que Unicode declara iguales — el plan no toca esta pareja
sync-reason-unknown = motivo que esta versión no conoce
sync-blocker-ambiguous-dest = dos nombres del destino colapsan en uno — escribir ahí podría dar al fichero equivocado
sync-blocker-overlap-detected = las dos raíces son el mismo árbol
sync-blocker-dest-read-only = el destino no admite escrituras
sync-blocker-dir-too-large = un directorio del destino pasa del límite de entradas
sync-blocker-type-mismatch-dir = un directorio de un lado es un fichero en el otro
sync-blocker-unknown = bloqueo que esta versión no conoce
# El titular: qué devolvería el undo si este plan se aplica. «Se puede
# deshacer» habla del PLAN, jamás es una garantía por entrada — una entrada que
# la papelera no sepa nombrar, o una ruta que haya cambiado entretanto, se
# NOMBRA en el informe del undo en vez de tocarla.
space-warning = { $size } a escribir y { $free } libres en el destino
# #164: este destino no sabe abrir una raíz confinada, así que una escritura
# llega a su sitio resolviendo una ruta. Quien pueda plantar un symlink dentro
# del destino entre este diálogo y la escritura puede desviarla. Se dice, jamás
# se rehúsa: rehusar dejaría sin copiar a los destinos que no pueden dar esa
# defensa, que cuesta mucho más que la carrera que evita.
confine-warning = este destino no puede confinar las escrituras: un symlink puesto dentro podría desviar esto a otro sitio
sync-outlook-full = todo esto se puede deshacer después, salvo lo que cambie entretanto
sync-outlook-partial = parte de esto se puede deshacer después y parte no
sync-outlook-nothing = nada de esto lo puede deshacer norte
# De qué destino se trata. El mismo pronóstico y dos noticias muy distintas: en
# la primera el fichero está en la papelera del sistema, en la segunda no está.
sync-trash-restorable = la papelera del destino nombra lo que entierra, así que el undo lo sabe encontrar
sync-trash-opaque = la papelera del destino no dice dónde deja las cosas: lo que esto sustituya se puede rescatar A MANO desde la papelera del sistema, pero norte no
sync-trash-absent = el destino no tiene papelera: lo que esto sustituya o borre no se guarda en ningún sitio
sync-trash-unknown = esta versión no sabe qué clase de papelera tiene el destino
sync-outlook-unclear = esta versión no puede decir si algo de esto se podrá deshacer
sync-summary-irreversible = { $n } pasos son irreversibles: no los devuelve nada
sync-summary-actions = { $copy } a copiar · { $overwrite } a sobrescribir · { $createdir } directorios a crear · { $deletetree } a borrar · { $skip } sin tocar
sync-summary-bytes = { $bytes } a escribir
# `bytes` es una COTA INFERIOR, jamás un total: un listado sobre file:// no da
# tamaños, así que un número con aplomo es la forma normal de mentir aquí.
sync-summary-bytes-partial = { $bytes } a escribir, más { $n } ficheros cuyo tamaño el provider no dio
# Un directorio que no se dejó listar es UNA entrada aquí, y todo lo que hay
# dentro se queda sin ver — de ahí «ni lo que hay dentro».
sync-summary-unreadable = { $n } entradas no se pudieron leer: este plan no las cubre ni a ellas ni a lo que haya dentro
# Los pasos que llegaron no cuadran con lo que el plan cerró.
sync-summary-mismatch = llegaron { $received } pasos y el plan dice { $n }: este plan no se puede aprobar
sync-summary-unnameable = { $n } pasos son de una clase que esta versión no sabe enseñar: este plan no se puede aprobar
sync-summary-malformed = { $n } pasos se contradicen a sí mismos: este plan no se puede aprobar
sync-summary-duplicate-ids = { $n } pasos traen un id que no avanza sobre uno anterior: este plan no se puede aprobar
sync-summary-list-truncated = la lista enseña los primeros { $shown } pasos; hay { $hidden } más contados y sin listar
# Las clases cuadran y un número de los que el diálogo enseña primero, no.
sync-summary-contradictory = las cuentas del propio plan no cuadran con los pasos que mandó: este plan no se puede aprobar
# La selección no recorta los bloqueos, así que esto habla de la comparación
# entera y no de lo que hay en pantalla.
sync-summary-blocked = { $n } bloqueos impiden este plan — en cualquier punto de los dos árboles, no solo en lo que seleccionaste
# Una pregunta por pronóstico, diciendo lo MISMO que dijo el resumen: un
# titular que dice «parte se puede deshacer» encima de una confirmación que
# dice «no se puede deshacer nada» enseña a saltarse las dos.
sync-confirm-delete = se van a borrar { $n } árboles del destino. Van a la papelera y se pueden restaurar. Un árbol se vuelve a comprobar en el directorio, no por dentro: algo añadido más abajo desde que aprobaste no detendrá el borrado. ¿Seguir?
sync-confirm-delete-final = se van a borrar { $n } árboles del destino y NO se van a poder restaurar. Un árbol se vuelve a comprobar en el directorio, no por dentro: algo añadido más abajo desde que aprobaste no detendrá el borrado. ¿Seguir?
sync-confirm-delete-partial = se van a borrar { $n } árboles del destino, y { $steps } pasos de este plan no se van a poder deshacer. Un árbol se vuelve a comprobar en el directorio, no por dentro: algo añadido más abajo desde que aprobaste no detendrá el borrado. ¿Seguir?
sync-confirm-delete-unclear = se van a borrar { $n } árboles del destino, y esta versión no puede decir si se podrían restaurar. Un árbol se vuelve a comprobar en el directorio, no por dentro: algo añadido más abajo desde que aprobaste no detendrá el borrado. ¿Seguir?
sync-confirm-no-way-back = { $n } pasos van a cambiar el destino y ninguno se va a poder deshacer. ¿Seguir?
sync-confirm-partial = { $n } pasos de este plan no se van a poder deshacer. ¿Seguir?
sync-confirm-unclear = esta versión no puede decir si estos { $n } cambios se van a poder deshacer. ¿Seguir?
# Título de la lista de fallos, y la MISMA cadena la nombra ante un lector de
# pantalla: una frase, las dos superficies. Nombre propio y jamás el de la
# lista de pasos — van una encima de la otra, y quien aterrice en la
# equivocada lee «aplicado» donde pone «falló».
sync-failures-title = pasos que fallaron
# `sync.report` lista como mucho 256 fallos y los cuenta todos, así que una
# ejecución con más lo dice en vez de dejar que la lista pase por el total.
sync-failures-more = … y { $n } más
# Por qué UN paso de un plan aplicado no ocurrió (`sync.report`). Compartidas
# por todos los frontends: el CLI las imprime tras la ejecución, la GUI las
# lista bajo el plan.
sync-cause-conflict = el destino cambió desde que se hizo el plan
sync-cause-denied = permiso denegado
sync-cause-illegal-name = el nombre no es legal en el destino
sync-cause-io = falló la lectura o la escritura
sync-cause-unknown = fallo no reconocido
# El PANEL de sincronización (Ctrl+Y, o `s`/`m` dentro del de diferencias).
# Sus teclas son fijas, como las del de diferencias, así que la línea de
# teclas es el único sitio donde están escritas.
# Un modo que esta build no sabe nombrar. NO puede caer en «actualizar»: decir
# «esto no borra» de un modo desconocido afirma la mitad SEGURA de lo que se
# está aprobando.
sync-mode-unknown = modo que esta versión no conoce
sync-header-step = paso
sync-header-path = ruta
sync-header-size = tamaño
sync-planning = planificando… { $n } pasos hasta ahora
sync-empty = este plan no tiene pasos: los dos árboles ya coinciden
sync-status-cancelled = cancelado — habían llegado { $n } pasos, y no hay plan que aprobar
sync-status-failed = el plan falló: { $error }
sync-status-ready = { $n } pasos · pulsa a para aprobar
sync-status-not-approvable = { $n } pasos · este plan no se puede aprobar
sync-status-applying = aplicando…
sync-status-applied = { $done } pasos aplicados, { $failed } fallaron
sync-status-applied-undoable = { $done } pasos aplicados, { $failed } fallaron · deshazlo con el comando de deshacer
sync-status-applied-not-undoable = { $done } pasos aplicados, { $failed } fallaron · no quedó nada en el journal, así que no hay nada que deshacer
# El mismo informe, cuando la ejecución NO terminó sola. Las cuentas a secas se
# leen como una sincronización completa, y la palabra que dice lo contrario no
# puede quedarse en el color.
sync-orphan-report = la sincronización terminó sin panel abierto: { $done } pasos aplicados, { $failed } fallidos · desházla con el comando de deshacer
sync-status-applied-cut-undoable = cancelado tras aplicar { $done } pasos ({ $failed } fallaron); el resto no se aplicó · deshazlo con el comando de deshacer
sync-status-applied-cut-not-undoable = cancelado tras aplicar { $done } pasos ({ $failed } fallaron); el resto no se aplicó · no quedó nada en el journal, así que no hay nada que deshacer
# Y cuando murió: el error Y las cuentas, porque un mirror que borró cuarenta
# árboles y luego falló es el último sitio donde esconder cuánto escribió.
sync-status-applied-failed-undoable = falló tras aplicar { $done } pasos ({ $failed } fallaron): { $error } · deshazlo con el comando de deshacer
sync-status-applied-failed-not-undoable = falló tras aplicar { $done } pasos ({ $failed } fallaron): { $error } · no quedó nada en el journal, así que no hay nada que deshacer
sync-hint = ↑↓ mover · a aprobar · Esc cerrar
sync-hint-done = ↑↓ mover · Esc cerrar
sync-hint-confirm = y confirmar · cualquier otra tecla cancela
# El destino escribe esta entrada distinto que el origen (#152): la escritura
# cae sobre la ortografía del DESTINO, y se enseñan las dos para que nadie lea
# una diferencia de normalización como un segundo fichero.
sync-dest-spelling = destino: { $path }
# La ruta del paso cuelga de la raíz del destino, no de la del origen.
sync-anchor-dest = (destino)
sync-anchor-either = (no consta de qué lado)
# Un bloqueo de todo el árbol (un destino de solo lectura) nombra la RAÍZ, que
# `rel_display` sola pinta como una cadena vacía — esto es lo que un panel
# dice ahí en su lugar, para que no se lea como una fila en blanco (#193).
sync-rel-root = todo el árbol
# Un par NFC/NFD (o cualquier otra ortografía byte-distinta pero
# glifo-idéntica) es UTF-8 válido en las dos mitades, así que ninguna se
# enmascara como hostil — esto es lo que explica la flecha en su lugar (#192).
sync-dest-twin = (igual en pantalla, bytes distintos)
compare-marked = { $n } marcadas
msg-sync-needs-daemon = sincronizar necesita el daemon (--daemon): tiene que quedar en el journal
msg-journal-squatted = otro proceso lleva minutos reteniendo tu journal y no hay ningún daemon escuchando: no se está registrando nada
status-journal-squatted = ⛔ alguien retiene tu journal — sin registrar
status-no-journal = SIN journal: esta sesión no se puede deshacer
status-journal-refused = journal ILEGIBLE: esta sesión rehúsa tocar tus ficheros
status-session-detached = sesión sin guardar
msg-journal-busy = esta sesión NO queda registrada en el journal: lo tiene otro proceso norte (un daemon, u otra ventana)
msg-journal-refused = esta sesión REHÚSA tocar nada: su journal no se pudo abrir ({ $motivo }). No quedaría registrado ni se podría deshacer. Arregla lo que nombra el motivo —el directorio o el fichero— y vuelve a intentarlo
msg-journal-recovered = vuelve a registrarse desde tu PRÓXIMA operación; una ya en marcha se queda con el veredicto con el que empezó
msg-sync-too-many-marks = { $n } marcas pasan de las { $max } que admite esta petición: marca menos, o marca un directorio que las contenga
msg-sync-cannot-approve = este plan no se puede aprobar tal como está
msg-sync-mark-outside-roots = una fila marcada no está en ninguno de los dos directorios: repite la comparación
msg-sync-mark-is-the-root = una fila marcada es uno de los dos directorios, lo que significaría el árbol entero: marca lo que hay dentro
err-overlapping-roots = los dos directorios se solapan
err-overlapping-roots-same = el origen y el destino son el mismo directorio
err-overlapping-roots-source-inside = el origen está dentro del destino
err-overlapping-roots-dest-inside = el destino está dentro del origen
reason-needs-daemon = necesita el daemon (--daemon)
reason-needs-desktop = necesita un escritorio: aquí no hay dónde abrir una ventana

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
# Nombre de la lista de filas del panel de diferencias (#158). Va en un
# ENVOLTORIO de la lista virtualizada, jamás en la lista misma: su id alimenta
# el scroll y la medida.
gui-a11y-compare-rows = filas de la comparación
# Nombre de la lista de pasos del panel de sincronización (#161). Misma regla
# del envoltorio que la del panel de diferencias, y nombre PROPIO: el marco ya
# se anuncia como «sincronizar», y darle a la lista las mismas palabras no
# dice en cuál de los dos está el lector.
gui-a11y-sync-steps = pasos del plan
# Nombre de la segunda pregunta del panel, la que se contesta antes de que se
# escriba nada. Nombra la pregunta para que quien aterrice en ella sepa a qué
# contesta la `y`.
gui-a11y-sync-confirm = confirmar el plan
# Prefijo hablado de un nombre que el saneado tuvo que alterar (spec §6). Una
# PALABRA y no el badge `⚠`: con la verbosidad de símbolos por defecto de NVDA
# y de Orca un U+26A0 suelto no se pronuncia, y bajo una reinterpretación de
# nombres activa (#57) la forma enmascarada tampoco lleva ningún U+FFFD — es
# texto limpio y legible que difiere de los bytes del disco, así que el badge
# es su única marca.
gui-a11y-hostile-name = nombre alterado
# La insignia VISIBLE junto a un nombre que no se pinta como es de verdad
# (overrides bidi, controles, bytes que no decodifican). Es TEXTO y no un
# símbolo a secas a propósito: el símbolo solo es un misterio la primera
# vez, y esta marca es el único aviso que tiene quien va a actuar sobre ese
# nombre.
hostile-name = ⚠ nombre alterado
# Lo que pinta la ventana gráfica donde una lista no tiene nada. Son dos
# mensajes porque contestan preguntas distintas: un directorio sin
# entradas, y un filtro que no casó ninguna.
listing-empty = vacío
# Lo que el provider se saltó lo dice `status-archive-skipped`, que es de los
# dos frontends (`norte_frontend::notes::skipped`). Aquí hubo una segunda
# redacción, sin el ⚠ que la hace leerse como aviso, y era la que pintaba la
# ventana: dos claves para un hecho es cómo dos superficies acaban diciendo
# cosas distintas de lo mismo.
palette-empty = nada casa con lo que has tecleado
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
reason-answered-by-overlay = lo contesta el propio overlay abierto
# H3f/4.4: una fila de la pantalla del VISOR, leída desde una página de ayuda
# sin visor abierto. No es «este frontend no lo hace» —sí lo hace— ni es de la
# selección: la tecla es de otra pantalla.
reason-viewer-only = solo con el visor abierto
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
gui-task-kind-create = crear fichero
gui-task-kind-delete = delete
gui-task-kind-undo = undo
gui-task-kind-search = search
gui-task-kind-index = index
gui-task-kind-embed = embed
gui-task-kind-rename-batch = rename
gui-task-kind-unknown = task
gui-task-kind-compare = comparar
gui-task-kind-dir-size = tamaño
gui-task-kind-checksum = sumas
gui-task-kind-set-mode = permisos
gui-task-kind-pack = empaquetar
gui-task-kind-test-archive = probar
gui-task-kind-split = partir
gui-task-kind-combine = juntar
gui-task-kind-sync-plan = plan
gui-task-kind-sync = sincronizar
# La lanzó OTRO cliente de la misma sesión. Se pinta igual y se cancela igual
# —es la misma sesión—, pero el tablero lo dice: una operación que uno no ha
# pedido y no se distingue de las suyas es una sorpresa.
gui-task-foreign = ajena
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
cli-ai-rename-unjournalled = estos renombrados NO quedarán registrados en el journal, así que no se podrán deshacer
cli-ai-rename-confirm = ¿Aplicar estos renombrados? [s/N]
cli-ai-rename-abort = cancelado; no se renombró nada
cli-ai-rename-done = renombrados { $n } archivo(s)
cli-ai-rename-invalid = el plan del modelo no es válido (un nombre no es un nombre de fichero legal); no se renombró nada
cli-ai-rename-collisions = el plan no se puede aplicar tal como está; no se renombró nada:
cli-ai-rename-stale = el directorio cambió mientras leías el plan; no se renombró nada, vuelve a lanzarlo
gui-modal-quit-title = ¿Salir con { $tasks } tarea(s) en curso y { $marks } marca(s)?
gui-modal-quit-title-empty = ¿Salir de norte?
gui-modal-footer-quit = y confirmar   n/Esc cancelar
gui-modal-footer-volumes = y/Enter abrir   n/Esc cancelar   ↓↑ mover   Tab alternar todo
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
help-cmd-pane = índice ↔ texto
help-cmd-page-up = subir
help-cmd-page-down = bajar
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
dialog-cmd-top = inicio
dialog-cmd-bottom = fin
dialog-cmd-section-prev = sección anterior
dialog-cmd-section-next = sección siguiente
dialog-cmd-add = añadir
dialog-cmd-toggle-enabled = activar
dialog-cmd-remove = quitar
dialog-cmd-confirm-other = otro panel
dialog-cmd-clear = vaciar
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
# Indicador de espera (#323). Verbos, no sustantivos: la línea contesta «qué
# está haciendo», y solo se enseña al lado de un spinner que se mueve de verdad.
busy-connecting = conectando…
busy-listing = listando…
busy-opening = abriendo…
busy-cancel = Esc cancela
cli-paths-missing = (no está)
cli-paths-footer = nota: preguntar no crea nada — una ruta marcada «(no está)» sencillamente no se usa todavía. Las capas van en precedencia ASCENDENTE: manda la última. Si lo que SÍ está es válido lo dice `norte doctor`.
cli-doctor-detail-connections-parse = connections.toml no parsea (corrígelo o bórralo)
cli-doctor-detail-connections-none = no hay connections.toml, o no hay conexiones configuradas
cli-doctor-detail-conn-secret-env-empty = { $detail } está definida pero VACÍA: dale un valor real o quítala (un secreto vacío no autentica, y antes se caía a las credenciales que ofreciera el entorno)
cli-doctor-detail-conn-secret-env-not-utf8 = { $detail } tiene bytes que no son UTF-8 válido, así que el secreto no se puede leer: reescríbelo, o guárdalo en el keyring o en secrets.age
cli-doctor-detail-conn-secret-env-absent = { $detail } no está definida; el keyring o secrets.age todavía pueden dar el secreto — es solo un aviso porque desde aquí no se comprueban
cli-doctor-detail-conn-secret-prompt = { $detail } no está definida, y la entrada dice secret = "prompt": norte lo pedirá al abrir la conexión
cli-doctor-detail-conn-secret-prompt-inert = { $detail } dice secret = "prompt", que con este método de auth no hace nada — solo se aplica a password y access-key
cli-doctor-detail-conn-rsa-allowed = { $detail } acepta una clave RSA (allow_rsa): firma por el camino de RUSTSEC-2023-0071, un riesgo aceptado (ADR 0150) — pásate a ed25519 en cuanto el servidor lo permita
cli-doctor-detail-conn-rsa-allowed-inert = { $detail } dice allow_rsa = true, que aquí no hace nada — solo se aplica a sftp con auth = "key"
cli-doctor-detail-connection-scheme-unserved = { $detail }: nada sirve ese scheme — ni el core, ni ningún provider plugin instalado lo declara (un error de tecleo, o un plugin por instalar)
cli-doctor-detail-plugin-wit-mismatch = { $detail }: compilado contra un paquete WIT que este norte no sirve — recompila el plugin contra el WIT actual y vuelve a aprobarlo
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
setting-ui-panel-bar-name = Barra de paneles
setting-ui-panel-bar-desc = Si la fila de botones de panel se queda fijada bajo la de menú. Encendida por defecto: los paneles laterales se abrían por atajo, por menú o por la paleta, y los tres exigen saber que el panel existe. Apagarla devuelve la fila — cada panel sigue abriéndose con su tecla y desde el menú. La GUI lo ignora.
setting-ui-menu-bar-name = Barra de menú
setting-ui-menu-bar-desc = Si la barra de menú se queda fijada en la fila de arriba. Encendida por defecto: hay comandos a los que solo se llega por el menú, y nada en pantalla decía que estaba ahí. Apagarla devuelve la fila — el menú sigue abriéndose con su tecla. La GUI lo ignora.
setting-ui-mouse-name = Ratón
setting-ui-mouse-desc = Si la TUI captura el ratón (click, rueda, arrastre para marcar). Mientras está capturado el terminal no puede seleccionar texto con el ratón; mantén Mayús para seleccionar igualmente, o desactiva esto. La GUI lo ignora.
setting-ui-alt-menu-name = Alt abre el menú
setting-ui-alt-menu-desc = Si pulsar y soltar Alt solo abre la barra de menús en la TUI. Necesita un terminal con el protocolo de teclado de kitty (kitty, foot, WezTerm, Ghostty); tmux, xterm y GNOME Terminal no pueden avisar de un Alt suelto. Apagado por defecto: en ese modo el terminal manda teclas y no texto, así que una letra con tecla muerta (é) o un símbolo con AltGr (@, #) llega como su tecla base. La GUI lo tiene siempre.
setting-ui-parent-entry-name = Fila para subir
setting-ui-parent-entry-desc = Si cada listado lleva arriba la fila `..`, que sube al directorio padre. Encendida por defecto: es lo que espera quien viene de cualquier gestor de la familia. Nunca es un operando — con el cursor encima no hay nada señalado, así que una copia o un borrado no tienen sobre qué actuar.
setting-ui-show-hidden-name = Ver ocultos al arrancar
setting-ui-show-hidden-desc = Con qué valor abre norte: si los nombres que empiezan por punto se ven desde el primer listado. La tecla que los alterna cambia la sesión y no escribe nada, así que este es el sitio donde se decide el default.
setting-ui-editor-name = Editor
setting-ui-editor-desc = El programa que abre la tecla de editar, como línea de órdenes con los códigos de campo de `openers.toml`: `%f` el fichero, `%d` el directorio del panel. Ejemplo: `zed %f`. Vacío = el de `$VISUAL`, el de `$EDITOR`, o el que exija POSIX. No se lee de la capa de proyecto: nombra un programa que se ejecuta.
setting-ui-editor-detached-name = El editor abre ventana
setting-ui-editor-detached-desc = Si ese editor abre una ventana propia en vez de ocupar el terminal. Uno de terminal necesita que norte se aparte y lo espere; uno de ventana devuelve el control al instante, y esperarlo dejaría el terminal en blanco hasta que cierres algo que está en otra pantalla.
setting-ui-diff-name = Comparar ficheros
setting-ui-diff-desc = El programa que compara DOS ficheros, como línea de órdenes con los códigos de campo de `openers.toml`: `%F` son los dos ficheros, `%d` el directorio del panel. Ejemplo: `meld %F`. Vacío = `diff -u`, cuya salida se queda en pantalla hasta que pulsas una tecla. No se lee de la capa de proyecto: nombra un programa que se ejecuta.
setting-ui-diff-detached-name = El comparador abre ventana
setting-ui-diff-detached-desc = Si ese comparador abre una ventana propia (Meld, Kompare) en vez de ocupar el terminal. Mismo trato que el editor, y por lo mismo.
setting-ui-confirm-quit-name = Confirmar antes de salir
setting-ui-confirm-quit-desc = Al salir pide confirmación: solo con trabajo pendiente (auto), siempre, o nunca. Un atajo de salida de emergencia, donde esté ligado (p. ej. Ctrl+C en la TUI), siempre lo evita.
setting-ui-key-bar-name = Barra de teclas
setting-ui-key-bar-desc = Solo en el terminal. Si la fila de teclas de función (F1–F10 y qué hace cada una en la pantalla actual) se queda fijada abajo. Se lee del keymap, así que reatar una tecla cambia su etiqueta, y pulsar una celda con el ratón ejecuta el comando. La ventana no la tiene: sus comandos están en el menú y en la paleta.
setting-ui-panel-bar-style-name = Etiquetas de la barra de paneles
setting-ui-panel-bar-style-desc = Cómo nombra sus botones la barra de paneles: el nombre del panel con la letra de acceso subrayada (names) o solo la letra (letters). Con menos de sesenta celdas los nombres pasan solos a letras. En la columna del terminal (posición left) se pintan iconos: símbolos Unicode con names o icons, los de Nerd Fonts con nerd, y la letra con letters.
setting-ui-panel-bar-position-name = Posición de la barra de paneles
setting-ui-panel-bar-position-desc = Dónde va la barra de paneles: una fila bajo el menú (top) o una columna en el borde izquierdo (left). Con auto, cada interfaz elige la suya: arriba en el terminal, donde falta ancho, y a la izquierda en la ventana, donde falta alto.
setting-ui-titlebar-name = Barra de título
setting-ui-titlebar-desc = Quién pinta la barra de título de la ventana: el escritorio (native) o norte (custom), que usa la barra de menús como barra de título, con minimizar, maximizar y cerrar, como VS Code. Se aplica al volver a abrir la ventana. El terminal no tiene barra de título.
setting-ui-status-items-name = Elementos de la barra de estado
setting-ui-status-items-desc = Qué enseña la mitad derecha de la barra de estado, en orden: position (posición del cursor), marks (lo marcado), sort (el orden), encoding (cómo se leen los nombres), tasks (tareas en marcha) y notices (avisos sin leer). Los que no caben ceden por importancia; los mensajes y los avisos de la izquierda no se configuran.
setting-ui-pane-footer-name = Pie del panel
setting-ui-pane-footer-desc = Si cada listado lleva un pie con sus cuentas (directorios, ficheros, bytes), lo marcado, y el espacio libre del volumen donde vive el directorio.
setting-ui-row-stripes-name = Filas a rayas
setting-ui-row-stripes-desc = Pinta las filas impares del listado sobre una banda propia, para seguir una fila ancha desde el nombre hasta la fecha. El color lo pone el tema; el cursor, una fila marcada y el puntero se pintan encima de la banda.
setting-ui-date-format-name = Formato de fecha
setting-ui-date-format-desc = Cómo pinta la columna de modificación una fecha cuando el ajuste de columnas no fija uno: la hora si es de hoy, día y hora si es de este año, la fecha si es anterior (smart); hace cuánto (relative); o fecha y hora completas (iso). Los tres en hora local.
setting-ui-theme-light-name = Tema con el escritorio claro
setting-ui-theme-light-desc = El tema que la ventana pinta cuando el escritorio prefiere un esquema claro: un nombre de preset o la ruta de un fichero de tema, como `theme`. Vacío pinta `theme` en los dos esquemas. El terminal lo ignora.
setting-ui-theme-dark-name = Tema con el escritorio oscuro
setting-ui-theme-dark-desc = El tema que la ventana pinta cuando el escritorio prefiere un esquema oscuro. Vacío pinta `theme` en los dos esquemas. El terminal lo ignora.
splash-title = norte
splash-daemon-embedded = el core va en este proceso
splash-daemon-connected = conectado al daemon
splash-daemon-connecting = conectando…
splash-recent = Dónde estabas
splash-popular = A dónde más vas
splash-bookmarks = Favoritos
splash-profiles = Perfiles
splash-hint = cualquier tecla continúa
splash-hint-home = un número abre su fila · cualquier otra tecla continúa
setting-ui-splash-name = Pantalla de arranque
setting-ui-splash-desc = Qué hace la pantalla de arranque: «brief» es una cubierta que quita cualquier tecla, «off» no enseña ninguna y «home» se queda hasta que pulses, con los directorios recientes y populares, los favoritos y los perfiles. El asistente de primer arranque siempre le gana.
setting-ui-processes-panel-name = Panel de procesos
setting-ui-processes-panel-desc = «auto» abre el panel cuando empieza una tarea y lo cierra cuando se va la última; «manual» lo deja al comando y a la barra de paneles. Abrirlo o cerrarlo a mano mientras hay una tarea manda hasta que esa tarea acabe.
setting-ui-images-name = Imágenes en el visor
setting-ui-images-desc = «auto» pinta la imagen con los gráficos del propio terminal cuando los tiene, y si no cae a medios bloques de un previewer de imagen aprobado; «kitty» y «blocks» fuerzan uno de los dos; «off» deja el visor enseñando los bytes. La ventana ignora esta clave.
setting-ui-dir-indicator-name = Barra en los directorios
setting-ui-dir-indicator-desc = La «/» delante de un directorio: «auto» la pinta solo cuando la columna de iconos está cerrada, porque el icono ya dice qué es la fila; «slash» siempre y «none» nunca.
setting-ui-history-size-name = Tamaño de la historia
setting-ui-history-size-desc = Cuántos directorios recuerda cada panel: los que lista la historia y los pasos que recorre «atrás». Entre 5 y 64; 64 es lo que guarda la sesión.
setting-ui-notice-seconds-name = Segundos de aviso
setting-ui-notice-seconds-desc = Cuánto se queda un aviso en la línea de estado antes de pasar al anillo de avisos y dejar solo una insignia. 0 lo mantiene hasta la siguiente tecla. Los banners persistentes (una conexión degradada, un journal que no abre) no caducan: son estado, no aviso.
setting-ui-dialog-buttons-name = Botones en los diálogos
setting-ui-dialog-buttons-desc = Si la línea de teclas de un diálogo se pinta como botones que se pueden pulsar, cada uno con su tecla, en vez de como una línea de teclas.
setting-keymap-preset-name = Preset de keymap
setting-keymap-preset-desc = Preset base de atajos de teclado (orthodox, vim o cua). Las capas de usuario/proyecto pueden seguir rebindeando encima.
keymap-unavailable-not-built = { $command }: aún no está construido ({ $reason }, issue #{ $issue })
keymap-unavailable-not-here = { $command }: no está disponible aquí
keymap-count-ignored = { $command } no acepta un contador (se ignoró { $count })
keymap-reason-archive-write = escritura de archivos comprimidos
keymap-reason-editor = editor integrado
keymap-reason-tree = panel de árbol de directorios
keymap-reason-tabs = pestañas de panel
keymap-reason-sort = comandos de ordenación
keymap-reason-task-walk = recorrer el tablero de tareas
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

# Fase A: el panel de procesos y la hoja de atributos.
processes-title = Procesos
log-level = nivel
log-level-error = error
log-level-warn = aviso
log-level-info = info
log-level-debug = depuración
log-level-trace = traza
log-filter = filtrar módulo o mensaje
log-follow = ir al final
log-detached = despegado del final
processes-empty = nada en marcha
processes-has-keyboard = este panel tiene el teclado · Esc lo devuelve

# El panel de registro (#323). El título lleva el nivel al lado porque un panel
# que se ve vacío tiene que distinguir «no ha pasado nada» de «lo estás
# filtrando fuera».
log-title = Registro
disk-map-title = Mapa de disco
disk-map-measuring = midiendo
slot-retry = reintentar
# Esperando un listado (#323). Tres piezas y no una frase: el renderer las
# compone con la RUTA en su propio nodo, porque una ruta pegada dentro de la
# frase se lee como parte de ella — y aquí además se ofrece cancelar.
slot-busy = cargando…
slot-busy-to = yendo a
log-keys = e/w/i/d/t nivel · / filtrar · Fin al final · Esc devuelve el teclado
# Solo se ofrece cuando el daemon sirve su registro (#328): anunciar un mando
# que recorrería tres vistas del MISMO anillo es prometer algo que no existe.
log-keys-source = s fuente
log-empty = nada que enseñar con este filtro
log-no-ring = sin registro instalado en este proceso
log-dropped = { $n } líneas viejas descartadas
# Con DOS anillos en pantalla (#328) cada cuenta dice de cuál habla: la de este
# proceso cuenta lo evacuado desde que arrancó, la del daemon lo que esta
# apertura del panel se perdió. Son números distintos y no se suman.
#
# «Este proceso» y no «la ventana»: las mismas frases las usan la ventana y
# `ntc --socket`, que no es una ventana, y la clave se llama `-window` por la
# variante de `LogSource`, no por el frontend.
log-dropped-window = este proceso descartó { $n } líneas viejas
log-missed-daemon = te perdiste { $n } líneas del daemon
# Cuando el anillo captura MÁS de lo que se enseña: subirlo no baja solo, así
# que el proceso sigue pagando ese nivel hasta que se cierre el panel.
log-capturing = capturando { $level }
# Y con los dos a la vista hay que decir de quién es cada nivel: el del daemon
# es global a todos sus clientes, no baja nunca y cerrar este panel no lo toca.
log-capturing-window = este proceso captura { $level }
log-capturing-daemon = el daemon captura { $level }
task-failed = falló
metadata-title = Detalles
metadata-empty = nada bajo el cursor
metadata-name = Nombre
metadata-kind = Clase
metadata-size = Tamaño
metadata-mtime = Modificado
# A dónde lleva la fila `..`. Solo la lleva esa fila: sobre `..` la hoja dice
# `..` y a dónde va, en vez de llamarse como el directorio padre.
metadata-target = Destino
metadata-kind-dir = carpeta
metadata-kind-file = fichero
metadata-kind-symlink = enlace
metadata-kind-other = otro

# ---------------------------------------------------------------------------
# El host gráfico: qué contestan sus diálogos, y por qué una acción no se pudo
# hacer. Estas claves las elige RUST y las pinta el renderer con `t(key)`, así
# que no aparecen en ningún literal `t("…")` de TypeScript y el test de
# catálogo del renderer no las ve. Faltaban veintiuna y se pintaban como su
# propio identificador —los dos botones del diálogo donde un humano aprueba la
# mutación de un agente incluidos.
dialog-body-truncated = … se enseñan { $shown } de { $total }
dialog-destination = Destino:
# Mientras se pregunta si el destino tiene sitio (#149) y si sabe sujetar sus
# escrituras (#164). Se DICE, en vez de dejar el hueco vacío: sin esta línea,
# «todavía no lo sé» se lee igual que «lo pregunté y no hay nada que decir», y
# lo segundo es una afirmación sobre la seguridad del destino.
dialog-checking-destination = comprobando el destino…
dialog-confirm = Confirmar
# Sumas en la ventana (#311). El diálogo es de LECTURA: copiar es lo único que
# se puede hacer con una lista de digests, y una comprobación no los trae.
dialog-copy = Copiar
dialog-close = Cerrar
modal-checksums-title = Sumas sha256
err-checksum-not-a-sums-file = esto no parece un fichero de sumas
err-checksum-sums-too-big = el fichero de sumas es demasiado grande para comprobarlo entero
err-checksum-sums-utf16 = el fichero de sumas está en UTF-16; guárdalo en UTF-8
err-checksum-partial = el lote no terminó: lo calculado está a medias y no se compara
err-checksum-failed = el cálculo de sumas falló
dialog-cancel = Cancelar
# Cerrar la ventana con `[ui] confirm_quit` pidiendo confirmación.
dialog-quit = Cerrar
modal-quit-title = ¿Cerrar norte?
modal-quit-pending = { $n ->
    [one] queda una operación en marcha; cerrar la deja a medias
   *[other] quedan { $n } operaciones en marcha; cerrar las deja a medias
}
dialog-approve = Aprobar
dialog-deny = Denegar
dialog-overwrite = Sobrescribir
dialog-newer = Solo si es más nuevo
dialog-rename = Renombrar automáticamente
dialog-skip = Saltar
modal-mkdir-title = Nuevo directorio
# Permisos en la ventana (#314). El cuerpo dice sobre cuántas va: en la
# terminal eso cabe en el título y aquí no, porque el título es una clave.
modal-chmod-title = Cambiar permisos
modal-chmod-count = { $n } entradas
modal-new-file-title = Fichero nuevo
# #309: la ventana ya puede AÑADIR un favorito, no solo abrir la lista. El
# nombre viene sugerido y guardarlo REEMPLAZA el que se llame igual.
modal-hotlist-name-title = Guardar como favorito
hotlist-name-empty = un favorito sin nombre no se guarda
modal-rename-title = Renombrar
modal-delete-title = ¿Mover a la papelera?
# Los tres veredictos del resolver compartido, para una tecla que SÍ está ligada.
cmd-here = es lo que ya hace aquí
cmd-not-built = todavía no está hecho
cmd-not-here = aquí no
host-key-unmapped = esa tecla no está ligada
host-cannot-view-dir = un directorio no se ve
host-nothing-to-view = nada que ver
host-no-sibling = no hay más ficheros de esta clase en este listado
host-unknown-field = ese diálogo no tiene ese campo
host-column-not-sortable = por esa columna no se ordena
host-help-over-input = la ayuda no se abre sobre un campo de texto
host-layout-broken = ese fichero de disposición no parsea
host-name-too-long = ese nombre es demasiado largo
host-secret-empty = escribe la contraseña: entregarla vacía cambiaría de identidad, no de sesión
host-secret-too-long = la contraseña no cabe: el tope son 256 caracteres
host-log-level-unknown = ese nivel de registro no existe
log-source-window = de este proceso (el daemon registra aparte)
log-source-daemon = del daemon
log-source-both = de este proceso y del daemon
log-source-unsupported = este daemon no sirve su registro: se enseña el de este proceso
log-source-daemon-level = el nivel es el del daemon: global a sus clientes y solo sube
host-name-not-editable = ese nombre no cabe en pantalla: aquí no se puede editar sin recortarlo
host-cannot-transfer-root = una raíz no se copia ni se mueve
host-no-other-slot = no hay otro panel
# No hay capa de configuración donde escribir: ni perfil ni directorio del
# usuario. Se dice en vez de tragarse el guardado, que es lo que separa
# «elegido» de «guardado».
host-no-config-dir = no hay dónde guardar la configuración
host-theme-unreadable = el fichero de tema no se pudo leer; se queda el tema anterior
host-theme-invalid = el fichero de tema no vale; se queda el tema anterior
# No hay directorio de usuario donde buscar `profiles/`.
host-no-profiles = no hay dónde buscar perfiles
# `profile.next`/`prev` sin a dónde ir: ni perfiles, o solo el que ya está.
host-no-other-profile = no hay otro perfil
# El perfil elegido no carga. No se cambia nada: se sigue en el que estabas.
host-profile-broken = ese perfil no se puede cargar
notify-approval-title = norte: un agente pide permiso
notify-approval-body = { $op } — { $who }
notify-task-done = norte: terminado
notify-task-failed = norte: algo falló
notify-task-body = { $kind }: { $what }
host-pick-destination = elige la carpeta de destino
host-bad-destination = esa carpeta no se puede nombrar aquí
host-drop-empty = no llegó ningún fichero
host-drop-unusable = lo que llegó no son rutas de esta máquina
err-bad-name = eso no es un nombre legal aquí
host-no-target-designated = hay más de un panel: designa antes un destino
host-plan-not-applicable = el core no aceptó este plan: no se renombraría nada
host-plan-abandoned = el plan de renombrado se abandonó
host-plan-asking = pidiéndole al modelo un plan de renombrado…
host-plan-acknowledge = pulsa otra vez para contestar: esta ventana se abrió sola
host-dialog-acknowledge = pulsa otra vez para contestar: este diálogo se abrió solo
dialog-ok = Entendido
modal-batch-report-title = informe del lote de renombrado
modal-undo-report-title = informe del deshacer
modal-undo-summary = { $undone } revertidas, { $skipped } irreversibles saltadas
modal-undo-left-in-place = { $n } creación(es) se quedan donde están: el destino no tiene papelera, y un deshacer nunca destruye sin retorno
modal-undo-blocked = el deshacer paró en la entrada { $seq } del journal ({ $error }): lo anterior a ella NO se deshizo
modal-undo-batch-stuck = un lote de renombrado no volvió; ahora se llama:
modal-undo-denied = { $n } unidad(es) que la policy denegó: se quedaron sin tocar
modal-undo-unsupported = este daemon no sabe informar de un deshacer, así que lo que volvió queda sin comprobar
modal-undo-report-failed = el informe del deshacer no se pudo pedir, así que lo que volvió queda sin comprobar
task-undo-done = { $n } revertidas
task-undo-unverified = terminado, sin informe
modal-batch-summary = { $applied } aplicados, { $back } devueltos
modal-batch-stuck = no se pudo devolver; ahora se llama:
modal-batch-stuck-journalled = el journal lo describe: un deshacer puede rematarlo
modal-batch-stuck-unjournalled = el journal no lo registró: esto solo lo deshace una persona
modal-batch-uncertain = no se sabe si este paso surtió efecto; mira aquí:
modal-batch-compensations-lost = { $n } compensaciones perdidas: un deshacer de sesión se parará ahí
modal-batch-unsupported = este daemon no sabe informar de un lote, así que el desenlace queda sin comprobar
modal-batch-report-failed = el informe no se pudo pedir, así que el desenlace queda sin comprobar
task-batch-applied = { $n } renombrados
task-batch-half = lote a medias: { $applied } aplicados, { $back } devueltos
task-batch-unverified = terminado, sin informe
host-plan-unseen = recorre el plan entero antes de aplicarlo
host-same-directory = el origen y el destino son el mismo directorio
host-batch-too-large = demasiadas entradas para una sola transferencia
host-batch-folds-to-one = dos de las marcas son el mismo nombre en el destino
host-task-board-full = el tablero de tareas está lleno
host-read-only = esta ventana está montada sin efectos: no escribe
host-no-tabs = este panel no está en un grupo de pestañas
host-no-such-tab = no hay tantas pestañas
host-task-running = esa tarea sigue en marcha: pararla es otra tecla
host-nothing-selected = no hay nada señalado
host-not-local = eso no está en este disco: no hay ruta nativa que darle al escritorio
host-no-desktop = esta ventana no tiene escritorio detrás: no puede copiar al portapapeles ni lanzar nada
# #303: el mismo texto que el msg-edit-created-changed de la TUI, y por lo
# mismo: un solo mensaje para las tres causas (enlace, carpeta, ya no está).
host-created-changed = el fichero se creó, pero ya no es el que se creó: no se ha abierto
host-created-unchecked = el fichero se creó, pero no se pudo comprobar qué hay ahí: no se ha abierto
msg-paths-copied = { $n } ruta(s) al portapapeles
msg-paths-copied-osc52 = { $n } ruta(s) al portapapeles del terminal (OSC 52); comprueba pegando
msg-clipboard-failed = el portapapeles no aceptó las rutas
msg-opening-external = abriendo con la aplicación del escritorio…
msg-opening-terminal = abriendo un terminal aquí…
host-plugin-running = ejecutando el comando de la extensión…
host-not-an-int = eso no es un número entero
host-out-of-range = el esquema lo acota entre { $min } y { $max }
host-value-rejected = el valor no vale para el esquema de esa clave
host-no-extension = no hay ninguna extensión señalada
host-extension-not-approved = primero hay que aprobar sus capabilities: sin ellas el core no la carga
host-extension-changed = las capabilities que declara han cambiado desde que se preguntó: se vuelve a preguntar
host-extension-too-many-caps = declara demasiadas capabilities para poder leerlas en una pantalla; no se concede desde aquí
host-extension-updated = hecho; el catálogo se vuelve a pedir para confirmarlo
modal-extension-approve-title = ¿conceder estas capabilities?
# Desinstalar desde el gestor (ADR 0104). La nota dice lo que se pierde —los
# ficheros Y la aprobación— porque «¿desinstalar?» a secas se lee como
# «¿apagar del todo?», y no es eso: una instalada después con el mismo id
# nace sin aprobar.
modal-extension-uninstall-title = ¿desinstalar esta extensión?
modal-extension-uninstall-note = se borran sus ficheros y se retira su aprobación; no tiene vuelta
dialog-uninstall = Desinstalar
host-settings-closed = los ajustes se cerraron; el valor no se escribió
msg-nav-at-root = ya estás en la raíz
msg-nothing-selected = nada seleccionado
msg-batch-summary = { $total } transferencias: { $ok } bien, { $fail } mal
