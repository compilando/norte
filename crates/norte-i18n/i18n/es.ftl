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
modal-collision-keys = [o]sobrescribir  [s]altar  [r]enombrar  [n]más nuevo  [esc]cancelar
modal-confirm-keys = [y/enter] adelante   [n/esc] cancelar

# --- Mensajes de la barra ---
msg-done = hecho
msg-cancelled = cancelado
msg-cancelling = cancelando…
msg-no-tasks = no hay tasks en marcha
msg-error = error: { $error }
msg-refresh-error = refresh: { $error }
msg-view-error = view: { $error }
msg-config-reloaded = config recargada
msg-config-not-applied = config NO aplicada: { $error }
msg-config-polling = config: vigilancia degradada a polling
msg-no-trash-here = sin papelera aquí: F8 de nuevo para permanente

# --- Panel de tasks ---
task-cancelled = cancelado

# --- Viewer ---
viewer-forced = (forzado)
viewer-lossy = con pérdidas (�)
viewer-truncated = [cabecera]
viewer-binary = binario
eol-mixed = EOL mixto
eol-none = sin EOL

# --- CLI ---
cli-runtime-error = norte: no se pudo arrancar el runtime: { $error }
cli-enqueue-copy = no se pudo encolar la copia
cli-enqueue-move = no se pudo encolar el move
cli-enqueue-delete = no se pudo encolar el borrado
cli-list-failed = list falló
cli-entry-unreadable = entrada ilegible
cli-serialize-failed = no se pudo serializar
cli-cancelling = cancelando…
cli-cancelled-clean = cancelado (destino limpio)
cli-final-error = error: { $error }
cli-unexpected-state = estado final inesperado: { $state }
