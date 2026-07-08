# 0002 — Runtime async y modelo de I/O bloqueante

- Estado: accepted
- Fecha: 2026-07-08
- Decisores: Oscar González, Claude (sesión M0)

## Contexto y problema

El principio 2 de la spec exige que toda I/O sea asíncrona y cancelable y que la
UI nunca se bloquee. Pero las APIs de filesystem de los tres OS son síncronas
(POSIX, Win32): hay que decidir runtime async y cómo puentear el FS bloqueante
sin envenenar el executor.

## Opciones consideradas

1. **tokio multi-thread + `spawn_blocking` para FS local**
   - ✓ Ecosistema dominante (tokio-util `CancellationToken`, streams, testcontainers).
   - ✓ `spawn_blocking` con pool separado: el FS síncrono no bloquea workers async.
   - ✓ Mismo modelo en los 3 OS; sin cfg por plataforma en el core.
   - ✗ Cada op de FS paga un salto de thread (~µs); irrelevante frente a I/O real.
2. **tokio + `tokio-uring` en Linux (io_uring nativo)**
   - ✓ Menos syscalls y copias en Linux moderno.
   - ✗ Solo Linux → dos rutas de código desde el día 1, contra "compat es una feature".
   - ✗ Madurez/API en flujo; complica la cancelación.
3. **smol / async-std**
   - ✗ Ecosistema menor; async-std abandonado de facto. Sin ventaja que lo compense.

## Decisión

**Opción 1.** tokio multi-thread en todo el workspace; todo acceso al FS local va
por `spawn_blocking` y vive en `norte-vfs-local` (regla 2 de CLAUDE.md: nada de
`std::fs` fuera de ese crate). Los providers puentean hacia async con canales
acotados (backpressure); la cancelación es cooperativa vía
`tokio_util::sync::CancellationToken` chequeado en el inner loop de cada Task.

`tokio-uring` queda como upgrade path detrás de feature flag, condicionado a
benchmark (spec §4), registrado como deuda.

## Consecuencias

- ＋ Un solo modelo mental de concurrencia; tests con `tokio::time::pause`.
- ＋ El pool de blocking está acotado y dimensionable; los streams perezosos deben
  liberar el hilo entre chunks (diseño del provider local, fase 8).
- － Rendimiento máximo en Linux no explotado hasta el benchmark de uring.
- － Dependencia estructural de tokio asumida en todo el árbol async.
