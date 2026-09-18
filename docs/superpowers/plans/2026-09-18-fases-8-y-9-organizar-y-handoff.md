# Fases 8 y 9 del programa WOW — plan

Escrito el 2026-09-18, al cerrar la fase 7. Las fases 1 a 7 están en `main`;
éstas dos no se han empezado, y este fichero existe para que empezarlas no
cueste redescubrir lo que ya se sabe.

**Por qué se pararon aquí y no se hicieron a medias.** Las dos necesitan algo
que esta máquina no puede comprobar de punta a punta, y este repositorio ya ha
pagado por dar por bueno lo que no se pilotó:

- La fase 8 gira sobre un **proveedor de IA** (`ai.organize_plan`) y sobre un
  **paquete WIT nuevo**. Sin proveedor configurado, lo único que se puede
  probar es el camino de plugin; y un kind a medias es un botón que abre algo
  que no existe — exactamente lo que la fase 7 estuvo a punto de dejar y sólo
  cazó el piloto.
- La fase 9 termina en **abrir la ventana desde el terminal**. La ventana no
  arranca en esta máquina sin WebKitGTK/Xvfb, así que su mitad visible no se
  puede ver funcionar.

## Fase 8 — «Pregúntale a norte»: plan de organizar

### Lo que pide la spec

Kind `organizer` (`norte:organizer@0.1.0`), que generaliza el renamer:
propone `{current, proposed-rel}` donde `proposed-rel` PUEDE llevar
subdirectorios. El proveedor de IA sirve el mismo contrato por
`ai.organize_plan {dir, instruction}`. Aplicar = `fs.create` de los directorios
+ `fs.move` por el pipeline del lote (journal, policy, un `batch_id`, deshacer
entero). Revisión como diff de árbol en `StyledFrame`, en las dos superficies.

### Lo que ya está hecho y hay que copiar, no reinventar

- **El renamer es el molde exacto.** `crates/norte-plugin-host/wit/deps/renamer/`
  y ADR 0095: el plugin PROPONE y el core ejecuta. Su cabecera explica por qué
  es paquete propio y no una interfaz de `norte:plugin` (la versión viaja en
  el nombre de la interfaz; un cambio aquí no puede invalidar a los previewers
  de terceros).
- **El plan de la IA y el de la plantilla ya comparten revisión**: un plan de
  renamer aterriza en el MISMO run que el de `ai.rename_plan`
  (`jobs::spawn_renamer_plan`), y por eso no hay dos caminos de revisión. El
  organizer tiene que entrar por ahí también.
- **`plan_hash`** ya existe y es lo que impide aplicar un plan distinto del
  que se revisó (memoria `spec3-superficies`: el `plan_hash` no lo canjea
  nadie más).

### Las trampas escritas

1. **Un kind nuevo es un PAQUETE WIT, no un bump** (memoria
   `ventana-pulido-visual-0107`), y hay un checklist de ONCE sitios para un
   bump de paquete en `columna-de-iconos-0105`. Cuéntalos antes de empezar.
2. **`proposed-rel` con subdirectorios es una superficie de path.** Todo lo de
   la regla 1 aplica: nada de `to_str().unwrap()`, y un `..` o una ruta
   absoluta en lo que propone un tercero tiene que rechazarse EN EL HOST,
   antes de crear nada. El renamer ya aparta los nombres que no son UTF-8
   antes de llamar; aquí hay que apartar además los que se salen del
   directorio.
3. **Crear directorios y mover son mutaciones**: un solo `batch_id`, y el
   deshacer del lote tiene que devolver las dos cosas. Ojo con lo que la fase
   7 acaba de descubrir: **un lote se deshace entero o no se toca**, y ahora
   `undo_after` excluye un lote si el corte cae dentro. Un organizer que
   escriba sus `fs.create` fuera del `batch_id` de sus `fs.move` deja un lote
   que al deshacerse restaura los ficheros y se olvida los directorios.
4. **La revisión va en las dos superficies** y el modelo tiene que ser
   compartido (`norte-frontend`), no dos árboles pintados por separado: la
   memoria `funcion-compartida-no-basta` es exactamente sobre esto.

### Orden sugerido

1. WIT `norte:organizer@0.1.0` + el kind en `norte-plugin-host` (sin IA
   todavía): se puede probar con un plugin de prueba del repo.
2. El modelo del diff de árbol en `norte-frontend`, con tests: qué es «nuevo»,
   qué es «movido», y cómo se pinta un directorio que se crea para meter algo.
3. El pipeline de aplicar (un `batch_id`, `fs.create` + `fs.move`), con un test
   de deshacer ENTERO.
4. `ai.organize_plan` (bump de protocolo) sobre el contrato ya probado.
5. La revisión en la TUI, y después en la ventana.

## Fase 9 — Handoff entre TUI y ventana

### Lo que pide la spec

Sólo en modo daemon. `app.handoff`: el dueño vuelca la sesión (`session.put`),
la suelta (`session.release`, nuevo) y lanza el otro frontend con `--attach`;
ése la reclama en su `session.get`. Pestañas, directorios, cursor e historia ya
viven en la sesión; las MARCAS no — se añaden a `SlotState.marks` (tope 4096,
ids de fila, no índices). Desde la TUI remota (SSH) no hay ventana que abrir:
el comando se anuncia no disponible con motivo.

### Lo que ya está y lo que falta

- **`session.get`/`session.put` existen** (0.48.0) y el daemon ya tiene un
  `ui_session.release(conn_id)` interno, que hoy corre al desconectar
  (`daemon/server.rs`). `session.release` por el wire es poco más que exponer
  eso — y decidir qué pasa si lo pide quien no es el dueño.
- **El cuerpo de la sesión NO lo versiona el protocolo**: lo versiona
  `norte_frontend::session::SCHEMA_VERSION` (hoy 2). Añadir `marks` a
  `SlotState` es aditivo y sube ESE número, no el del wire. Sólo
  `session.release` obliga a bump de protocolo.
- **`--attach` no existe** en ninguno de los dos binarios. Hay que añadirlo, y
  decidir qué significa exactamente: reclamar la sesión y NO volcar la suya al
  salir, presumiblemente.

### Las trampas escritas

1. **Las marcas son ids de fila, no índices** (lo dice la spec, y la memoria
   `posicion-no-nombra-una-fila` explica por qué: una lista que se mueve sola
   pide identidad). El tope de 4096 va en el modelo, no en cada llamante.
2. **«Sólo en modo daemon» hay que DECIRLO, no callarlo.** Un comando que no
   hace nada en embebido es un comando roto; el catálogo ya sabe declarar
   disponibilidad con motivo, y la memoria `motivo-de-fallo-322` es sobre esto.
3. **Por SSH no hay ventana.** Detectarlo y anunciarlo con su motivo, igual.
4. **Soltar la sesión y que el otro la reclame es una carrera.** Si el lanzado
   no arranca, la sesión se ha soltado y nadie la tiene: hay que decidir si el
   que suelta se queda esperando confirmación, o si reclamarla es idempotente.
   La memoria `piloto-tmux-olvidado-retiene-la-sesion` cuenta lo que cuesta una
   sesión retenida por un proceso que ya nadie mira — el fallo contrario, y
   también caro.
5. **`just link-gui` es obligatorio para pilotarlo**, y la ventana empotra su
   webview al compilar: sin reconstruir el bundle se prueba una ventana vieja
   sin que nada lo diga (memoria `donde-esta-el-proyecto`).

### Cómo se prueba

La mitad del terminal se pilota en tmux como todo lo demás. La mitad de la
ventana necesita una máquina con WebKitGTK; `just gui-smoke` (Docker) es lo que
comprueba que el paquete arranca de verdad.
