# Huecos de funcionalidad frente a Krusader, Total Commander y Norton Commander

2026-08-28. Segunda pasada; la primera fue la de los presets de teclado
(#228/#250), que miró **teclas**. Ésta mira **funciones**: qué sabe hacer cada
uno de los tres gestores de referencia que norte todavía no.

## De dónde sale cada afirmación

Tres fuentes, y ninguna es la memoria de nadie:

1. **Lo que norte hace**: el catálogo de comandos
   (`crates/norte-frontend/src/keymap/catalogue.rs`, 146 entradas) y los
   métodos del protocolo (`FS_*` en `norte-proto`), que es lo que el core
   sabe ejecutar. Un comando que no está ahí no existe.
2. **Lo que hacen ellos, por teclas**: las cabeceras de los presets
   importados. Cada una lleva su lista de «omitted families» escrita al
   transcribir de una fuente de primera mano — `tc-11.58-KEYBOARD.TXT` para
   Total Commander y la página de key-bindings de docs.kde.org para Krusader.
   Norton Commander no tiene fuente de primera mano y su preset lo dice; lo
   suyo va marcado como tal.
3. **Lo que hacen ellos, por menús**: los menús publicados de Krusader
   (docs.kde.org, «Menu Commands») y la lista de features de Total Commander
   (ghisler.com), consultados hoy. Lo que no pude verificar va marcado.

El nivel de evidencia va en cada fila, porque no todas valen lo mismo.

## Lo que ya está cubierto

El núcleo ortodoxo está completo y no hay que volver sobre él: dos paneles con
foco y destino, F5/F6/F7/F8 con confirmación, marcado (uno, todos, patrón,
invertir), quick search con filtro y salto, historial y favoritos, pestañas,
árbol de directorios, visor con hex y encodings, editor externo, comparar
directorios, sincronizar, buscar por nombre/contenido con regex, empaquetar,
desempaquetar, comprobar archivo, partir y unir ficheros, tamaño de directorio,
propiedades, unidades, conexiones remotas (SFTP/FTP/S3) y archivos como
directorios, columnas configurables, temas, ajustes, extensiones y paleta.

Y tres cosas que ninguno de los tres tiene: journal con deshacer, política de
permisos, y agentes/MCP.

## Huecos

### A. Baratos, porque la parte difícil ya está construida

| # | Hueco | Quién lo tiene | Evidencia | Qué falta de verdad |
| --- | --- | --- | --- | --- |
| A1 | **Renombrado en lote SIN IA** | TC («Multi-rename tool»), Krusader («Multi Rename») | verificado hoy en las dos fuentes | `fs.rename_batch_plan`/`fs.rename_batch` EXISTEN, con plan revisable, hash, colisiones, journal y undo (ADR 0042). Lo único que falta es el **generador de plan determinista**: patrón con `[N]`, `[C]` contador, extensión, buscar/reemplazar. Hoy el único que produce planes es `pane.ai-rename`, o sea que un renombrado en lote exige un LLM. |
| A2 | **Checksums: crear y verificar** | Krusader («Create Checksum», «Verify Checksum») | verificado hoy | Todo. No hay método en el protocolo ni comando. Es lectura pura y encaja como Task con progreso. |
| A3 | **Matices de selección** | TC (`Alt+Num +/-` por extensión, `Num /` restaurar selección, ficheros vs carpetas), NC (Gray `+/-/*`) | cabecera de `total-commander.toml` (primera mano) | `mark.*` tiene todos, patrón e invertir. Faltan «los de esta extensión», «solo ficheros / solo carpetas» y «restaurar la selección anterior». Son tres comandos sobre el modelo de marcas que ya existe. |

### B. Piden protocolo nuevo

| # | Hueco | Quién lo tiene | Evidencia | Nota |
| --- | --- | --- | --- | --- |
| B1 | **Cambiar atributos**: permisos, fechas, propietario | los tres | NO verificado en esta pasada | No hay `fs.chmod`/`fs.set_attrs` en el protocolo: `pane.properties` los ENSEÑA y no los toca. Es mutación, así que necesita journal, undo y política — el trabajo real está ahí, no en el diálogo. |
| B2 | **Crear enlace simbólico** | Krusader («New Symlink») | verificado hoy | No hay método. Misma disciplina que B1. |
| B3 | **Borrado seguro (wipe)** | Far (`Alt+Del`) | cabecera de `norton.toml` | `pane.delete-permanent` borra sin sobrescribir. Sobre SSD la promesa es dudosa y eso hay que decirlo en la interfaz, no esconderlo. |
| B4 | **Verificar tras copiar** | TC | NO verificado | Releer y comparar tras una transferencia. Encaja con las Tasks que ya hay. |

### C. Superficie de paneles

| # | Hueco | Quién lo tiene | Evidencia | Nota |
| --- | --- | --- | --- | --- |
| C1 | **Comparar dos FICHEROS por contenido** | TC («Compare files by content»), Krusader («Compare by Content») | verificado hoy | `pane.compare-dirs` compara ÁRBOLES. Falta el de dos ficheros; los dos originales abren un diff. Lo mínimo honesto: mandarlo a un programa externo por `openers.toml`. |
| C2 | **Vista de rama / aplanar subdirectorios** | TC (`Ctrl+B`) | cabecera de `total-commander.toml` | Un listado con todo lo que cuelga, plano. Se apoya en `fs.search` sin filtro, pero el listado tiene que aceptar rutas de varios niveles. |
| C3 | **Portapapeles de FICHEROS** | TC (`Ctrl+C`/`Ctrl+X`/`Ctrl+V`) | cabecera de `total-commander.toml` | `pane.copy-path` copia rutas como TEXTO. Copiar y pegar ficheros con el portapapeles del escritorio es otra cosa, y es la vía de interoperar con el resto del sistema. |
| C4 | **Cola de transferencias** | TC (`Shift+F5`/`F6` en cola), Krusader (Queue Manager) | cabeceras de los dos presets | El tablero de tasks cancela; no encola, no pausa y no reordena. |
| C5 | **Menú de usuario** | NC (`F2`), Krusader (useractions) | cabeceras de `norton.toml` y `krusader.toml` | Hay Lua y paleta, o sea el motor. Falta la superficie: una lista de órdenes propias con su tecla. |

### D. Fuera a sabiendas, o ya cubierto de otra forma

Barras de botones y de rutas, modos de vista (breve/detallada) y miniaturas
—salvo en la ventana, donde sí tienen sentido—, imprimir, comentarios de
fichero, modo root, la lista de pestañas abiertas, «URLs populares», el
transfer serie de NC, y los diálogos de modo FTP. El gestor de montajes
(MountMan) está a medias: hay selector de volúmenes, y expulsar es **#148**.

## Cruce con lo que ya está abierto

Ninguno de los huecos de arriba tiene issue. Las 32 abiertas son plataforma
(Windows, macOS), aguas arriba, o deuda técnica; las más cercanas son **#148**
(expulsión segura, roza D) y **#142** (`Ctrl+O` enseña el scrollback y no una
subshell viva, roza C5).

## Por dónde empezaría

1. **A1, renombrado en lote sin IA.** Es el hueco con mejor relación
   valor/coste de toda la lista: la máquina entera —plan, revisión, colisiones,
   journal, undo— ya está construida y probada, y hoy solo la puede disparar un
   LLM. Es además la función que más se echa de menos al venir de TC.
2. **A2, checksums.** Función completa, sin mutación, sin política: una Task
   que lee y compara.
3. **C1, comparar dos ficheros.** Barato si se delega en `openers.toml`, y
   cierra la pareja con `compare-dirs`, que ya existe.
4. **A3, matices de selección.** Tres comandos sobre un modelo que ya está.
5. **B1, atributos.** El primero de los caros, y el que más se nota: es la
   única categoría donde norte solo mira y los tres originales tocan.
