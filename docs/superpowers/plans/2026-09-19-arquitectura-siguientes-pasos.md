# Arquitectura: siguientes pasos — plan

Escrito el 2026-09-19, al cerrar la revisión de arquitectura (ADR 0126, ADR
0127, `norte-frontend` en carpetas; plan hermano
`2026-09-19-revision-de-arquitectura.md`). Es para OTRA sesión: cada ola es
independiente, lleva su rama y se puede tomar sola.

Las cifras son de `main` `05f787b6` y hay que remedirlas antes de empezar.

## Orden y por qué

| ola | qué | valor | coste | riesgo |
| --- | --- | --- | --- | --- |
| 1 | spans dentro de `spawn_blocking` | cierra ADR 0127 | S | bajo |
| 2 | margen de cobertura | el gate está a 0,78 puntos de romperse | M | bajo |
| 3 | `norte-config/src/load.rs`: una clave, un sitio | medido esta semana | M | medio |
| 4 | `norte-cli/src/main.rs` repartido | 4,7k líneas en un binario | M | bajo |
| 5 | `norte-core::Backend`: fachada por áreas | H5 de la revisión 08-20 | L | medio |
| 6 | `pane/mod.rs` y `controller/mod.rs` | los dos mayores del frontend | L | medio |

Las olas 1 y 2 van primero porque son pequeñas y porque una deja a medias un
ADR y la otra deja el gate a un PR mediano de ponerse rojo. La 3 va antes que
las de tamaño porque su dolor es concreto: añadir `[log] format` costó tocar
siete sitios.

## Ola 1 — `fix/spans-en-spawn-blocking`

**Problema.** ADR 0127 cuelga cada tarea de su `rpc`, pero `spawn_blocking` no
hereda el span. Todo lo que se registra dentro de esos cierres sale huérfano,
y es justo donde está la E/S: `norte-vfs-local/src/provider.rs` (11),
`norte-vfs-archive/src/provider.rs` (12), `norte-core/src/sync/spool.rs` (16),
`pack.rs`, y `hooks.rs:310` (un `tokio::spawn` con bloqueantes dentro). El ADR
lo dice en «Bad».

**Propuesta.** Un solo ayudante por crate que capture el span actual y lo
entre dentro del cierre:

```rust
pub(crate) fn blocking<F, R>(f: F) -> tokio::task::JoinHandle<R>
where F: FnOnce() -> R + Send + 'static, R: Send + 'static {
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || span.in_scope(f))
}
```

Luego sustituir cada llamada. Es mecánico, así que puede ir un agente
`model: sonnet`, uno por crate, porque los crates son disjuntos.

**Test primero.** En `norte-vfs-local`: un evento emitido dentro de una
operación del provider, bajo un span `task`, lleva ese span. El patrón está
en `norte-core/tests/daemon/tasks.rs::la_tarea_de_una_peticion_cuelga_de_su_rpc`.

**Regla para que no vuelva.** Un test de fuente (como los de frontera): nadie
fuera del ayudante llama a `spawn_blocking` directamente. O un lint de
`clippy.toml` `disallowed-methods` sobre `tokio::task::spawn_blocking`, con el
ayudante como excepción, que es más barato.

**Cierre.** Actualizar el «Bad» de ADR 0127 (no hace falta ADR nuevo).

## Ola 2 — `test/margen-de-cobertura`

**Problema.** `just cov` exige 85 % de líneas y da **85,78 %**. El 2026-08-20
daba 88,10 %. Un PR mediano con código poco probado lo pone rojo, y se
descubre en el gate de cierre, que es el más caro.

**Paso 1, medir.** Sacar el informe por fichero de `cargo llvm-cov` (la receta
`cov` ya lo imprime) y ordenar por líneas NO cubiertas, no por porcentaje:
un 60 % en 40 líneas importa menos que un 84 % en 3.000.

**Paso 2, subir.** Tests en los cinco primeros de esa lista. El candidato
conocido es `norte-vfs-local/src/confined.rs`: estaba al 79,8 % y es el
`openat2`/`RESOLVE_BENEATH` que contiene a los agentes. Añadir fixtures
hostiles donde toque (`/fixture`).

**Meta.** ≥ 87 %. **No** subir el suelo en la misma PR: primero el margen, y
decidir el suelo aparte.

## Ola 3 — `refactor/config-una-clave-un-sitio`

**Problema.** `norte-config/src/load.rs` tiene 5,5k líneas. Añadir una clave
escalar a una sección obligó a tocar:
1. `schema.rs`
2. el campo en `Config`
3. el acumulador en `load`
4. `merge_*_layer`
5. el literal que construye `Config`
6. y 7. dos desestructuraciones exhaustivas (`norte-tui/src/app/profile.rs`,
   `norte-ui-host/tests/config_cobertura.rs`)

Las desestructuraciones son buenas: obligan a decidir. Los pasos 2 a 5 son
pura fontanería.

**Propuesta.** Que las secciones que solo se fusionan campo a campo, «gana la
última capa» (`[log]`, `[daemon]`, `[archive]`), vivan en `Config` como su
propia sección ya fusionada (`cfg.log.format`), con un `merge(&mut self,
other)` derivable o escrito una vez por sección. Añadir una clave pasa a ser
el campo y su `merge`.

**Cuidado.** El filtro «nunca desde la capa de proyecto» es de seguridad
(`manda_fuera_de_presentacion`). Tiene que seguir aplicándose por SECCIÓN,
con su test. Revisor: `security-reviewer`.

**Coste.** Hay que migrar los consumidores de `cfg.log_dir` / `log_retain` /
`daemon_*` / `archive_*`. Medir con `rg -c` antes; si pasa de ~400 líneas,
partir en una PR por sección.

## Ola 4 — `refactor/cli-reparto`

**Problema.** `norte-cli/src/main.rs` tiene 4,7k líneas y ~96 funciones en el
binario. Ya tiene cuatro módulos al lado (`doctor`, `help`, `paths`, `theme`)
y cuatro módulos de test dentro del propio `main.rs`.

**Propuesta.** El mismo reparto que se le hizo a la TUI (`main.rs` 11,6k →
592): un módulo por subcomando (`cmd/cp.rs`, `cmd/sync.rs`…), y el `main`
se queda con clap, el logging y el despacho. Primero comprobar que no hay
lógica de negocio escondida ahí (regla dura 7): la que haya sube al core.

**Test.** El golden de la ayuda (`NORTE_UPDATE_GOLDEN`) no debe moverse. Es
la prueba de que el reparto no cambió nada visible.

## Ola 5 — `refactor/backend-por-areas`

**Problema.** `norte-core/src/backend.rs` tiene 3,7k líneas y **98 funciones
públicas**. Es la fachada que usa cada frontend (H5 de la revisión de
08-20). Cualquier cambio en el core pasa por aquí, y el fichero es donde
chocan las ramas.

**Propuesta.** El tipo `Backend` se queda, pero sus métodos se reparten en
`backend/{fs,tasks,sync,search,plugins,session}.rs` con `impl Backend`
por fichero. Es la forma que ya tiene `ui-host/src/controller/`. Sin traits
nuevos: una fachada con un solo implementador no gana nada con una
interfaz.

**Antes de empezar.** Leer por qué `methods.rs` y `daemon/server.rs` se
dejaron enteros (sus cabeceras). Si el argumento de «tabla plana
exhaustiva» aplica también aquí, la ola se cae, y está bien que se caiga.

## Ola 6 — `refactor/pane-y-controller`

**Problema.** Son los dos ficheros más grandes del lado frontend:
`norte-frontend/src/pane/mod.rs` (4,6k) y
`norte-ui-host/src/controller/mod.rs` (4,3k), aunque este último ya se
repartió una vez (18,7k → 33 ficheros).

**Propuesta.** Mirar qué ha vuelto a crecer en `controller/mod.rs` desde el
reparto: probablemente sea lo que no tenía sitio. Hay que darle sitio, no
volver a repartir por repartir. En `pane/mod.rs`: separar el estado (cursor,
marcas, filtro) de las transiciones.

## Lo que NO entra, y por qué

- **Command pattern con un tipo por comando.** Rechazado en ADR 0126: pierde
  la exhaustividad del `match`.
- **Mover el host de Lua fuera de `norte-tui`** (H2 de 08-20). Lua está
  congelado (ADR 0110), así que moverlo es trabajo sobre algo que no crece.
  Si se descongela, vuelve a entrar.
- **Partir `norte-proto/src/methods.rs`** (9,9k). Es el catálogo del wire, y
  cualquier cambio ahí es un cambio de protocolo con su versión y sus goldens
  (`/proto-change`). El tamaño viene del vocabulario, no de un mal diseño.
- **Llevar `availability::verdict` al catálogo.** ADR 0126 lo dejó fuera a
  propósito: *dónde* escribe un comando depende del estado en tiempo de
  ejecución.

## Cómo ejecutar

Cada ola en su rama `<tipo>/<kebab>`, con TDD donde haya comportamiento, el
revisor de la tabla de CLAUDE.md antes de cada commit, y `just ci-fast` cada
dos o tres olas; `just ci` una vez al final. Hacerlo el controlador, salvo la
ola 1, que es mecánica sobre crates disjuntos.
