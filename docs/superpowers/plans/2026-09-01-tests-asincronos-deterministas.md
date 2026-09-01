# Iniciativa 1 — pruebas asíncronas deterministas en `norte-ui-host`

Fecha: 2026-09-01. Alcance: `crates/norte-ui-host/tests/controller.rs` y su
doble `tests/backend_falso/mod.rs`.

## Lo que se midió (2026-09-01, `main e7edaf4e`)

| medida | valor |
| --- | --- |
| `sleep(` en `crates/` | 227 |
| `sleep(` en `crates/*/tests/` | 185 |
| `sleep(` en `tests/controller.rs` | **111** |
| tests en el crate | 423 (≈376 en `controller.rs`) |
| `controller.rs` | 17 444 líneas |
| suite entera (`just t norte-ui-host`, árbol ya compilado) | 15,1 s de resumen, 17 s de reloj |
| CPU sumada de TODOS los tests de `controller` | **3,89 s** |

Corrección al enunciado de la iniciativa: **la suite no es lenta por los
`sleep`**. Los 15 s los ponen tres tests de `payload` (13,6 + 8,6 + 7,9 s);
`controller` entero suma 3,9 s porque nextest los corre en paralelo. Lo que se
compra aquí es **determinismo**, no velocidad. El criterio de aceptación «el
tiempo total no aumenta» se mantiene; el de «la suite es más rápida» no era
real y no se promete.

## Por qué son un bug igualmente

Dos formas, y la peligrosa es la primera:

1. **`sleep(N)` seguido de una aserción, sin reintento** (≈60 sitios). Bajo
   carga —nextest corre 423 tests en paralelo— 30 ms no garantizan que la task
   que el actor lanzó con `tokio::spawn` haya llegado a anotarse en el doble.
   Es una carrera pura: cuando pierde, el test miente sobre QUÉ falló.
2. **Bucle acotado `for _ in 0..40 { …; sleep(25) }`** (≈50 sitios). Tiene
   reintento, así que casi nunca se pone rojo, pero su presupuesto es de reloj
   de pared (1 s) y bajo carga se agota igual.

## La arquitectura que lo permite arreglar

El actor es un solo escritor con buzón. Cada mutación se despacha así
(`controller.rs:7574`, `crear_directorio`): el actor valida, hace
`tokio::spawn` de la llamada al backend, y **devuelve el ack**. El doble anota
lo que le piden de forma síncrona, al entrar en el método. De ahí las tres
propiedades que usamos:

- Si el ack volvió, el actor YA decidió: si iba a encolar, ya hizo el `spawn`.
- Las tasks lanzadas se anotan en el doble en el orden en que se lanzaron
  (ejecutor `current_thread`, cola FIFO).
- El doble puede AVISAR cuando anota. Eso convierte «espera 30 ms» en «espera
  a que ocurra».

## Los cuatro reemplazos

### 1. `Falso` late

Un `Notify` compartido en el doble, y un `latido()` detrás de cada anotación
(≈35 sitios: `creados`, `borrados`, `transferencias`, `lotes`, `sondeos`,
`listados`, `escrituras`, `gobierno`, `aplicados`, `permisos`, …).

```rust
/// Un aviso por cada cosa que el doble ANOTA.
///
/// `notify_waiters` solo despierta a quien ya espera: el que espera arma el
/// futuro ANTES de volver a mirar, igual que `Puerta::esperar`.
pub pulso: tokio::sync::Notify,

fn latido(&self) { self.pulso.notify_waiters(); }

/// Espera a que el doble haya anotado lo que se le pregunta. Sin reloj.
///
/// El plazo de socorro NO es una espera: es el presupuesto de FALLO. En el
/// camino verde no se consume, y cuando se agota el test dice qué esperaba
/// en vez de reventar en la aserción de después.
pub async fn hasta<T>(&self, que: impl Fn(&Self) -> Option<T>, que_esperaba: &str) -> T
```

### 2. `asentar()` — para las aserciones NEGATIVAS

Cuando el test afirma que NO se encoló nada, no hay evento que esperar. Lo que
hay que dejar correr es lo que ya está en la cola del ejecutor:

```rust
/// Deja correr lo que YA está encolado, cediendo el turno.
///
/// No es un reloj: no se vuelve más frágil bajo carga, que es exactamente lo
/// que le pasa a un `sleep`. El fichero ya usaba este patrón
/// (`siguiente_recuento`, 200 `yield_now`); esto lo nombra.
async fn asentar() { for _ in 0..32 { tokio::task::yield_now().await; } }
```

Y para «no llega otra foto», tras `asentar()`, un sondeo NO bloqueante:
`tokio::time::timeout(Duration::ZERO, sub.recv())` — poll único, cero espera.
(`UiSubscription` no gana `try_recv`: el API público no se toca.)

### 3. Relojes de verdad → `start_paused` + `advance`

TTL del tablero de tasks, `timeout` de extensión, backoff. Ya hay precedente en
el fichero (`controller.rs:1443`). Son ≈6 sitios.

### 4. Latencia simulada → la anota el doble

`retraso_ms` (5 tests) es un `sleep` DENTRO del doble, y eso está bien: es la
latencia que se simula, no una adivinanza del test. Lo que cambia es cómo se
espera: el doble anota «respondí tarde» después del retraso y el test usa
`hasta(...)`.

## Tareas

| # | qué | sitios | verificación |
| --- | --- | --- | --- |
| T1 | `pulso` + `latido()` + `Falso::hasta` en el doble; `asentar()` y `hasta()` en el fichero de test; documentar el patrón en la cabecera de ambos | — | compila |
| T2 | Región 466–3400: navegación, listados, diálogos | 17 | `just t norte-ui-host` |
| T3 | Región 7700–8500: archivo, partir/juntar, transferencias | 17 | idem |
| T4 | Región 8600–9600: copiar/mover, dos paneles | 17 | idem |
| T5 | Región 9700–11800: renombrado por IA, informes | 17 | idem |
| T6 | Región 12100–13400: sync, extensiones, gobierno | 25 | idem |
| T7 | Región 13400–17440: paleta, tablero, permisos | 18 | idem |
| T8 | Barrido: `grep sleep` a cero, 3 pases seguidos de la suite, doc | — | `just ci-fast` |

## Límites

- No se toca el API público de `norte-ui-host` ni `src/`.
- No se cambian tiempos de producción.
- No se añade sondeo con otro `sleep`.
- Los `sleep` que quedan vivos están DENTRO del doble simulando latencia, y
  cada uno con su comentario diciendo por qué.

## Criterios de aceptación

- Cero `tokio::time::sleep` en `tests/controller.rs`.
- Cada espera nombra el evento que espera.
- `just t norte-ui-host` verde tres veces seguidas.
- El resumen de la suite no sube.
- El patrón queda escrito donde lo lea quien añada el test siguiente.
