# Translation glossary (Spanish → English)

One term, one translation, everywhere: code, comments, rustdoc and the `en`
Fluent catalogue (`crates/norte-i18n/i18n/en.ftl`). When the catalogue already
uses a word for a concept, the catalogue wins and this table follows it.

**Spelling: follow the text around you, and one concept has one label.**
Identifiers are American (`Color`, `favorite`), except wire values that were
already British (`cancelled`, `catalogue` in several names) and stay. The
English help topics and much of the catalogue's prose are British ("colour");
they are left as they are — rewriting user documentation was not this job.
What PC4 did fix is the same concept labelled two ways in the UI:
`Favourites`/`favourite` are now `Favorites`/`favorite` everywhere in en.ftl.

Grown batch by batch. A term missing here: pick the orthodox-file-manager word,
then add the row in the same commit.

## File manager

| español | English | note |
| --- | --- | --- |
| panel | pane | `panel` also exists in English code for *side panel* (`PanelBar`). A file list is a **pane**. |
| panel lateral | side panel | |
| panel con foco / sin foco | focused / unfocused pane | |
| el otro panel | the other pane | |
| entrada (de un listado) | entry | |
| fichero, archivo | file | |
| carpeta, directorio | directory | `dir` in identifiers |
| enlace (simbólico) | symlink | |
| listado | listing | |
| fila | row | |
| fila `..` | parent row (`..`) | |
| cursor | cursor | |
| marca, marcar | mark, to mark | a **mark** is a multi-selection tag; the **cursor** is the single row |
| selección | selection | |
| visor | viewer | |
| editor | editor | |
| línea de órdenes | command line | |
| barra de estado | status bar | |
| barra de teclas | key bar | |
| barra de menús | menu bar | |
| menú | menu | |
| paleta de comandos | command palette | |
| diálogo, modal | dialog, modal | |
| botón | button | |
| atajo, acorde | shortcut, chord | a **chord** is the key combination (`ctrl+x`) |
| tecla | key | |
| hueco (de layout) | slot | ADR 0058 |
| pestaña | tab | |
| disposición | layout | |
| árbol | tree | |
| favorito | favorite | UI text; `hotlist` in identifiers and config (`[[hotlist]]`) |
| papelera | trash | |
| borrar | delete | permanent: **permanent delete** |
| copiar / mover / renombrar | copy / move / rename | |
| destino / origen | destination / source | |
| colisión | collision | |
| ajustes | settings | |
| tema | theme | |
| rol (de tema) | role | |
| cromo (de la ventana) | chrome | |
| pijama (filas alternas) | striped rows / stripe | UI says "Striped rows"; `Role::Stripe`, `[ui] row_stripes` |
| insignia | badge | |
| fondo / primer plano | background / foreground | |
| atenuado | dimmed / muted | `dim` = attribute, `Muted` = role |
| ventana | window | the Tauri GUI frontend |
| terminal | terminal | the TUI frontend |
| ayuda | help | |

## Core, protocol, safety

| español | English | note |
| --- | --- | --- |
| núcleo | core | |
| daemon | daemon | |
| tarea | task | |
| cancelar | cancel | |
| diario | journal | |
| deshacer | undo | |
| política | policy | |
| permiso, concesión | permission, grant | |
| ancla | anchor | ADR 0073 |
| sesión | session | |
| conexión | connection | |
| secreto | secret | |
| proveedor | provider | |
| hostil | hostile | *hostile name*: bytes a display can be fooled by |
| nombre enmascarado | masked name | |
| relevo | handoff | ADR 0123 |
| índice | index | |
| motivo (de un fallo) | reason | |
| aviso | warning / notice | *warning* for severity, *notice* for a toast |
| registro (de log) | log | `registro` as a *registry* → registry |
| cable (el wire) | wire | |
| golden | golden | |
| gate | gate | the local CI recipe |
| degradar | degrade | a newer value an older norte cannot use becomes `None`, not an error |
| respaldo | fallback | |
| embebido | embedded | |
