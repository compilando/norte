# Los prompts y el bucle de eventos — lo hecho

> **Estado:** COMPLETO lo que esta ronda se propuso. Las nueve familias de
> prompts comparten UNA implementación; el enrutado de teclas vive fuera del
> bucle. `run`: **2.244 → 1.217 líneas**; `event_loop.rs`: 2.622 → 1.418.
> `just ci-fast` verde: **5.077 tests**. Prueba manual bajo tmux, además del
> gate.
>
> **Fecha:** 2026-08-20. Continúa `2026-08-20-tui-app-impl-split.md`.

## 1. Los nueve prompts de texto libre

El reparto de `app.rs` los dejó juntos en `app/prompts.rs` justamente para
poder mirar si compartían forma. La compartían entera salvo en cuatro cosas,
y esas cuatro son ahora una TABLA en vez de nueve copias:

| prompt | campo | tope | el tope se dice | retroceso |
| --- | --- | --- | --- | --- |
| marcar por patrón | `pattern` | 256 | no | carácter |
| nombre de destino | `name` | 256 | no | carácter (+ `touched`) |
| destino de transferencia | `input` | 8192 | **sí** | **escape wire entero** |
| empaquetar | `name` | 256 | no | carácter |
| partir | `size` | **32** | no | carácter |
| crear directorio | `name` | 256 | no | carácter |
| línea de comandos | `command` | 256 | **sí** | carácter |
| rename IA | `instruction` | 256 | no | carácter |
| búsqueda semántica | `query` | 256 | no | carácter |

`Modal::text_prompt` devuelve el campo abierto con su política; `PromptKind`
dice cuál es. Los 45 métodos con nombre propio siguen existiendo —son lo que
nombran las tablas de despacho y los tests— pero su cuerpo es una línea.

**Lo que el cambio destapó, y no se cambió en silencio:**

- `transfer_name_submitted` NO era el cierre genérico: consume el lote de
  marcas al enviar. Lo cazó su propio test al fallar en rojo.
- El retroceso limpiaba el diagnóstico en ocho familias y en la novena no.
  Ahora es la regla, con su test; `touched` sigue fijándose solo si borró.
- El pegado (`route_paste`) reconocía SIETE prompts a mano: empaquetar y
  partir aceptaban teclas y se comían un pegado sin decir nada. Test nuevo.

En el bucle de eventos eran nueve bloques `if matches!(app.modal, …)` con el
mismo Char/Backspace/Esc; ahora es uno, y solo el Enter —el submit, que es
async— se abre en nueve brazos.

## 2. El bucle de eventos

Tres movimientos, en este orden, cada uno habilitando el siguiente:

1. **`settle_cd`**: el desenlace de un cd —reordenar por esquema el pane que
   aterrizó, pedirle decoraciones, aplicar el resultado— estaba copiado
   PALABRA POR PALABRA en nueve sitios.
2. **`run_command` y `launch_pending_open`**: despachar un comando y asentarlo
   se escribía cinco veces. Quedan fuera a propósito los dos que no son ese
   molde: el resolver de teclas (necesita `nav_stalled` ANTES de que el
   desenlace se consuma) y el despacho del ratón.
3. **`jobs::InFlight`**: diecisiete variables locales de `run` —rellenos,
   sondas y los cinco trabajos de fondo— pasan a una estructura. No es
   cosmética: eran las que hacían que cualquier función extraída del bucle
   naciera con quince parámetros.

Con eso, **`keys.rs`**: la cadena de precedencia entera (mil líneas: quién se
queda una tecla, del menú al resolver) sale de `run`. Los `continue` del bucle
son ahí `return`, que es lo mismo dicho sin bucle.

## 3. Lo que sigue sin hacerse

- **El `select!` de 23 brazos sigue en `run`**, y con él las 1.200 líneas que
  quedan. Sus brazos cortos ya no son el problema; el bloque del prólogo
  («drenar lo pendiente») sí se puede repartir igual que las teclas.
- **El despacho del ratón no pide decoraciones** tras un cd, y los otros ocho
  sitios sí. Se dejó como estaba —esta ronda no cambia comportamiento sin
  test— pero es una diferencia que no parece querida.
- **La palette no se pudo probar bajo tmux**: su atajo en el preset activo es
  `ctrl+shift+p` y ningún modificador sobre esa tecla llega bajo tmux (#159).
