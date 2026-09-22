# Batida de funcionalidades, 2026-09-20

Qué tiene norte, qué tiene Krusader, qué tienen los gestores modernos, y qué
falta. Hecha leyendo el código, no de memoria: el catálogo de comandos, el
catálogo RPC y los registros de kinds y de modales son la fuente.

> **Estado**: tres de los huecos que nombra ya están cerrados en el mismo día
> que se escribió — el pijama y la columna de permisos (ADR 0128), el zoom
> del visor (ADR 0128) y la búsqueda filtrable (protocolo 0.81.0). Se marcan
> en su sitio en vez de borrarlos: lo que faltaba es parte de por qué se hizo.

## De dónde salen los números

| inventario | dónde vive | cuánto |
| --- | --- | --- |
| comandos | `crates/norte-frontend/src/keymap/catalogue.rs` | **149**, todos `Live` |
| métodos RPC | `crates/norte-proto/src/catalog.rs` (golden `catalogo.tsv`) | **68** (60 petición, 8 notificación) |
| tipos de panel | `crates/norte-frontend/src/layout/kinds.rs` | **13** + los de plugin |
| pantallas del keymap | `crates/norte-frontend/src/keymap/layer.rs` | 3 (`browse`, `viewer`, `dialog`) |
| modales de la TUI | `crates/norte-tui/src/app/modal.rs` | 29 + 4 emergentes + 7 superpuestos |
| presets de teclas | `crates/norte-frontend/presets/keymap/` | 7 |
| temas | `crates/norte-theme/presets/` | 10 |
| proveedores VFS | `crates/norte-vfs-*` | local, sftp, objetos, archivos, rar, ftp (plugin) |

## 1. Lo que norte ya hace y Krusader no

Conviene empezar por aquí, porque cambia qué huecos merecen la pena.

- **Núcleo sin cabeza con protocolo**. 68 métodos JSON-RPC, un daemon, y tres
  clientes (TUI, ventana, CLI) que no tienen lógica propia. Krusader es una
  aplicación KDE; su «motor» es su interfaz.
- **Diario con deshacer, y línea de tiempo** (ADR 0121). Toda mutación deja
  entrada y tiene vuelta atrás, o se marca irreversible con motivo. Krusader
  no tiene deshacer.
- **Agentes gobernados** (M3, ADR 0089): MCP con nueve métodos en lista
  blanca, política con aprobación por operación, deshacer de sesión.
- **Renombrado por lotes transaccional**: previsualización de colisiones del
  plan entero, orden a prueba de ciclos, una sola unidad deshacible.
- **Sincronización con plan retenido** (ADR 0049): el plan aprobado vive en
  disco, atado a la conexión que lo produjo, de un solo uso.
- **Mapa de disco** (ADR 0117) y **organizar un directorio** (ADR 0122).
- **Extensiones WASM con capacidades** (ADR 0057): seis categorías, sin
  acceso directo al sistema de ficheros.
- **Relevo entre frontends** (ADR 0123): `app.handoff` pasa la pantalla de la
  terminal a la ventana y al revés.
- **Nombres como bytes** de punta a punta, con corpus hostil.
- **Ir a cualquier sitio** (ADR 0120): seis fuentes en una pantalla.

## 2. Krusader: lo que tiene y nosotros no

Ordenado por lo que se echa de menos, no por dificultad.

### 2.1 Terminal empotrado (panel)

**No existe.** Lo que hay es distinto y, en una cosa, mejor:

| Krusader | norte |
| --- | --- |
| panel de terminal acoplado, visible a la vez que los listados | — |
| línea de órdenes fija abajo | `pane.command-line`, un modal de un disparo |
| — | `app.toggle-panels`: shell VIVO persistente que sigue al panel (ADR 0084) |
| — | `app.terminal`: shell en el directorio activo |

Falta el **panel**: un emulador VT dentro de la disposición. El registro de
kinds es una tabla abierta de cadenas, así que añadir `terminal` no toca el
wire; lo que no existe es el emulador (no hay `vt100`, `vte`,
`alacritty_terminal`, `tui-term` ni `xterm.js` en el árbol). `portable-pty`
ya está, por ADR 0084.

Un plugin **no** puede serlo: la WIT de paneles pinta spans y recibe comandos
del catálogo, nunca bytes, y no importa ni `exec` ni pty.

### 2.2 Búsqueda — **hecho el 2026-09-20 (protocolo 0.81.0)**

La tabla de abajo era el estado ANTES. Lo que se añadió: filtro por clase
(ficheros/carpetas), rango de tamaño, «cambiado hace N días», excluir
subárboles por ruta, excluir carpetas por NOMBRE en cualquier nivel, palabra
entera, interruptor de subcarpetas y codificación forzada. Siguen faltando
—y están en la lista de abajo— las varias raíces, seguir enlaces, buscar
dentro de comprimidos, la pestaña de resultados persistente, copiar la
consulta, y **la ventana, que sigue mandando solo un glob**: su diálogo tiene
un único campo de texto, así que ponerle los filtros pide una forma de
diálogo con varios campos que el puente no tiene todavía.

### 2.2 bis. El estado anterior, para referencia

`FsSearchParams` tiene hoy: una raíz, `name_glob` | `name_regex`,
`content` | `content_regex`, `case_sensitive` (compartido) y `max_hits`.

| control de Krusader | norte |
| --- | --- |
| patrón de nombre (glob) | **sí** |
| patrón de nombre (regex) | sí en la TUI; **la ventana no lo expone** |
| filtro por tipo de fichero | **falta** |
| distinguir mayúsculas | sí en la TUI; la ventana lo manda fijo a `false` |
| **varias** carpetas donde buscar | **falta** (la raíz es una) |
| **varias** carpetas a excluir | **falta** (el `excluded` que hay es de política) |
| excluir por NOMBRE de carpeta (`node_modules`) | **falta** |
| buscar texto dentro | **sí**, literal y regex |
| elegir la codificación del texto | **falta** (es automática: se transcodifica la AGUJA) |
| solo palabra completa | **falta** |
| buscar en subcarpetas (interruptor) | **falta**: siempre recursivo |
| buscar dentro de comprimidos | **falta** salvo que ya estés dentro de uno |
| seguir enlaces | **falta**: nunca se siguen, por ciclos |
| pestaña de resultados | parcial: el panel se vuelve virtual y se pierde al `cd` |
| copiar la consulta al portapapeles | **falta** |
| filtro por tamaño / por fecha | **falta** (ni en Krusader ni aquí están en esa pestaña, pero se esperan) |

Además: `norte-index` indexa nombres y rutas, **no contenido**, y `fs.search`
no lo consulta. Y `norte-cli` no expone búsqueda ninguna.

### 2.3 Columna de permisos

**Resuelto hoy** (ADR 0128): sale por defecto en `file` y `sftp`.
**Ordenar** por ella y por cualquier atributo: resuelto (ADR 0144).
Sigue faltando **dueño/grupo** por nombre: `posix.uid` y
`posix.gid` existen como atributos crudos, sin resolver a nombre, y no hay
columna. Para SFTP hace falta el `longname` crudo (#114, ya abierta).

### 2.4 Lo demás de Krusader

| función | norte |
| --- | --- |
| MountMan (montar/desmontar/expulsar) | **falta**: hay `host.volumes` y `pane.select-drive`, pero ningún comando monta, desmonta ni expulsa |
| navegación sincronizada de los dos paneles | **falta** |
| cola de operaciones (JobMan): pausar, reordenar, reintentar | **parcial**: hay tablero de tareas y `task.cancel`; no hay pausa, cola ni reintento |
| acciones de usuario (UserActions) | **parcial**: hay extensiones WASM y Lua congelado (ADR 0110); no hay «orden mía con una tecla» sin escribir un plugin |
| modo root | **deliberadamente no** (el daemon no corre como root) |
| enviar por correo | falta (menor) |
| imprimir el listado | falta (menor) |
| comparar por contenido | **sí** (`norte-compare`, cascada de criterios) |
| dividir/unir ficheros | **sí** |
| sumas de comprobación | **sí** (ADR 0080) |
| gestión de marcadores | **sí** (`pane.hotlist`, `pane.popular`) |
| visor y editor internos | visor **sí**; editor no (se delega, ADR 0082) |

## 3. Gestores modernos: lo que se espera hoy

Mirando lo que traen los de esta década (Total Commander moderno, Double
Commander, Far 3, Yazi, nnn, Files/Nautilus, Directory Opus):

| función | norte |
| --- | --- |
| pestañas por panel | **sí**, con `pane.tab-*` |
| vista previa de imágenes en la terminal | **sí** (ADR 0118, kitty y medios bloques) |
| miniaturas | **sí**, por plugin `thumbnail` |
| temas, incluido importar de VSCode | **sí** (ADR 0109) |
| paleta de comandos | **sí** |
| salto difuso a cualquier sitio | **sí** (ADR 0120) |
| iconos por tipo | **sí** (ADR 0105) |
| estado de git en el listado | **sí**, como plugin (ADR 0057) |
| mapa de uso de disco | **sí** (ADR 0117) |
| arrastrar y soltar | **sí** (ADR 0074) |
| **zoom en el visor de imágenes** | **falta**: `viewer.*` tiene 12 comandos y ninguno de zoom, ajuste, 1:1 o rotar |
| **siguiente/anterior imagen sin salir del visor** | **falta**: el visor abre UNA ruta y no sabe de hermanas |
| **filas a rayas** | resuelto hoy (ADR 0128) |
| nubes (Drive, Dropbox…) | **falta**: hay S3 y SFTP; no hay proveedores de nube de consumo |
| SMB / redes Windows | **falta** |
| MTP / móviles | **falta** |
| papelera navegable | **falta**: se borra a la papelera, pero no hay `trash:` que listar |
| vista de árbol | **sí** (kind `tree`) |
| vista en cuadrícula / miniaturas grandes | **falta** |
| edición de nombre en línea | **sí** |
| búsqueda semántica | **sí** (M4-IA) |

## 4. Propuesta, por orden de lo que se nota

1. ~~**Zoom en el visor de imágenes**~~ — hecho (ADR 0128).
2. ~~**Búsqueda a la altura**~~ — hecho en la terminal (protocolo 0.81.0).
   Queda la mitad de la ventana, que es lo siguiente de esta lista.
3. **Los filtros de búsqueda en la ventana.** Su diálogo tiene UN campo de
   texto; los siete campos y los cuatro interruptores piden un
   `DialogView` con varios campos. Es una pieza reutilizable —cualquier
   diálogo futuro con formulario la quiere— y por eso vale la pena hacerla
   bien y no a medida de la búsqueda.
4. **Panel de terminal** (encargo 7). El más caro: emulador VT nuevo,
   kind nuevo, pty en dos frontends, revisión de seguridad. Lo que ya
   está: `portable-pty` en `norte-tui`, el registro de kinds abierto, y el
   reparto E/S-pura de `subshell.rs` como modelo. **Plan escrito** en
   `docs/superpowers/plans/2026-09-20-panel-de-terminal.md`, con las cinco
   fases y la decisión de diseño que hay que tomar (quién manda en el
   teclado cuando el panel lo tiene).
4. **Siguiente/anterior imagen en el visor.** Pequeño y muy echado en falta
   una vez hay zoom.
5. **Ordenar por permisos, y columnas de dueño y grupo.** `SortColumn` es
   hoy un conjunto cerrado (`Name`/`Size`/`Mtime`); abrirlo a atributos es
   una decisión de diseño con ADR.
6. **Cola de operaciones**: pausar y reanudar una copia, reordenar la cola.
7. **Montar y desmontar volúmenes** (el MountMan de Krusader).
8. **Navegación sincronizada** de los dos paneles. Barato y clásico.
9. **Papelera navegable** como proveedor.
10. **Una orden del usuario atada a una tecla** sin escribir un plugin.

Lo que queda fuera a propósito: modo root (el daemon no corre como root),
nubes de consumo y SMB/MTP (proveedores enteros, cada uno su hito).
