# norte-gui — SPIKE M5 (hito 1)

Binario GPUI que pinta un directorio real del daemon `norte`, con los
colores de `norte-theme`. Es un **spike de viabilidad**, no producción:
mide si GPUI (el toolkit de Zed) es una base razonable para la GUI real
de M5 hito 2. La decisión go/no-go vive en `docs/adr/0027-*.md`.

Frontend sin lógica de negocio (regla dura 7): habla SOLO `norte-proto`,
vía el `RemoteBackend` de `norte-core`. Read-only (`fs.list`); no muta
nada, no abre socket propio, no tiene keymap.

## Este crate está EXCLUIDO del workspace

`Cargo.toml` raíz lo lista en `[workspace] exclude` (junto a
`examples-wasm`), no en `members`. Consecuencias:

- **NO** participa en `cargo build --workspace`, `cargo clippy --workspace`,
  `cargo nextest run --workspace` ni en `just ci`. El gate del core nunca
  se entera de si GPUI compila o no.
- Tiene su **propio `Cargo.lock`** (no comparte el del workspace).
- Se compila y se corre **desde dentro de su directorio**, o con `-p`
  desde la raíz (ambos funcionan; ver comprobación de T1):
  ```bash
  # desde la raíz del repo
  cargo build -p norte-gui
  cargo run -p norte-gui

  # o, equivalente, desde este directorio
  cd crates/norte-gui
  cargo build
  cargo run
  ```

## Toolchain

- **Rust stable 1.96.1** (la misma que pinea el `rust-toolchain.toml` de
  la raíz) — GPUI en el rev pineado compiló sin pedir nightly.
- **GPUI de Zed** por dependencia git pineada a un rev exacto (NO existe
  como `gpui` estable en crates.io):
  ```toml
  gpui          = { git = "https://github.com/zed-industries/zed", rev = "f14fea9bf3c93797d5161f7440ed418655bc6c57" }
  gpui_platform = { git = "https://github.com/zed-industries/zed", rev = "f14fea9bf3c93797d5161f7440ed418655bc6c57", features = ["wayland", "x11"] }
  ```
  rev `f14fea9bf3c93797d5161f7440ed418655bc6c57` (zed-industries/zed,
  rama `main`, 2026-07-19). Licencia GPUI: Apache-2.0. Las features
  `wayland`/`x11` habilitan el backend nativo de Linux (`gpui_linux`);
  sin ellas no hay ningún backend que compilar/abrir. Detalle completo
  (por qué `gpui_platform` es un crate separado en este rev, cómo se
  descubrió la API) en los comentarios de cabecera de `src/main.rs` y en
  `Cargo.toml`.
- Requiere un **display** (X11 o Wayland) para abrir ventana — no corre
  en Linux headless sin compositor. Verificado en este entorno con
  `DISPLAY=:1` + `WAYLAND_DISPLAY=wayland-0` (KDE Plasma/kwin_wayland).

## Variables de entorno

`backend_task::LoadConfig::from_env` (`src/backend_task.rs`) resuelve la
config del daemon a listar:

| Variable | Significado | Default si falta |
|---|---|---|
| `NORTE_SOCKET` | Path del socket UDS del daemon. | `$XDG_RUNTIME_DIR/norte/daemon.sock` (el mismo default que `norte daemon run` y la TUI — `norte_core::daemon::default_socket_path`). |
| `NORTE_DIR` | Directorio a listar, en forma **wire** (`file:///ruta/absoluta`, con los `%XX` que hagan falta para bytes no-ASCII). | El `cwd` del proceso, convertido a `VPath` `file://` igual que la TUI. |
| `NORTE_GUI_DEBUG` | Si está presente (cualquier valor), imprime por `stderr` el resultado de la carga (`N entradas de <dir>`) y, por cada fila, `nombre kind=... filekind=... fg=...` — útil para confirmar sin verificación visual que el listado y el color por tipo llegaron bien. | Silencioso. |

El spike **no autoarranca el daemon** (`RemoteBackend::connect` se llama
con `spawn_cmd = None`): si no hay un `norte daemon run` ya escuchando en
`NORTE_SOCKET`, la ventana abre igual y muestra el error en texto (nunca
panic — contrato de T4).

## Cómo arrancar el trío (daemon + segundo cliente + GUI)

Para reproducir la verificación de simultaneidad (criterio 2 — ver
abajo):

```bash
# 0. build (una vez)
cargo build -p norte-cli -p norte-tui        # desde la raíz
(cd crates/norte-gui && cargo build)         # el spike, con su propio lockfile

# 1. daemon, en su propia terminal — anota el socket
./target/debug/norte daemon run --socket /tmp/norte-demo/daemon.sock

# 2a. segundo cliente: TUI en modo daemon (necesita TTY real —
#     raw mode falla en background sin pty; usa una terminal real o
#     `tmux new-session` si no tienes una a mano)
cd /tmp/norte-demo   # el dir que la TUI lista es su cwd
./target/debug/norte-tui --daemon --socket /tmp/norte-demo/daemon.sock

#     alternativa sin TTY: un segundo cliente por CLI (mismo protocolo,
#     mismo socket, sin necesitar raw mode)
./target/debug/norte --daemon --socket /tmp/norte-demo/daemon.sock ls /tmp/norte-demo

# 2b. la GUI — tercera terminal, mismo socket, mismo dir
NORTE_SOCKET=/tmp/norte-demo/daemon.sock \
NORTE_DIR=file:///tmp/norte-demo \
NORTE_GUI_DEBUG=1 \
cargo run -p norte-gui
```

Con el daemon vivo, la TUI (o la CLI) y la GUI pueden estar conectadas
**a la vez** al mismo socket: el daemon ya es multi-conexión (probado en
M3 con TUI+agente MCP); este spike lo confirma con un frontend gráfico.

## Verificación de simultaneidad (criterio 2) — hecha en T6

Corrido en este entorno (`DISPLAY=:1`, KDE Plasma/kwin_wayland, socket
custom `/tmp/norte-t6-demo/daemon.sock`, dir con 7 entradas: archivo
regular, symlink, subdirectorio, `.rs`):

1. `norte daemon run --socket ...` en background (`nohup` + log)
   → `daemon enlazado ... uid=1000`, socket `srw-------` (0600).
2. **Segundo cliente = la TUI real** (`norte-tui --daemon --socket ...`),
   NO la alternativa CLI: en background puro con stdin cerrado murió con
   `failed to initialize terminal: Os { code: 6, ... }` (raw mode exige
   un TTY real — fricción de entorno esperada y documentada en el plan).
   Se resolvió dándole un **pty real vía `tmux new-session -d`**: con
   eso la TUI SÍ inicializó, conectó al daemon (dos `fs.list` — panel
   izquierdo y derecho) y pintó las 7 entradas correctamente coloreadas
   por tipo en ambos paneles (`tmux capture-pane` lo confirma).
3. Con la TUI ya conectada (conexión persistente — `lsof` sobre el
   socket la muestra `CONNECTED`), se arrancó `norte-gui` apuntando al
   MISMO socket y MISMO dir, con `NORTE_GUI_DEBUG=1`. Log de la GUI:
   ```
   [norte-gui] listado recibido del daemon: 8 entradas de file:///tmp/norte-t6-demo
   [norte-gui] 'main.rs' kind=File filekind=Regular fg=Some(Color { r: 215, g: 135, b: 95 })
   [norte-gui] 'enlace.lnk' kind=Symlink filekind=Symlink fg=Some(Color { r: 95, g: 175, b: 175 })
   [norte-gui] 'subdir' kind=Dir filekind=Dir fg=Some(Color { r: 95, g: 175, b: 215 })
   ...
   ```
   (8 en vez de 7: el propio `gui.log` del redirect ya existía para
   cuando la GUI listó — coherente, es el mismo dir visto un instante
   más tarde, no una discrepancia.)
4. Captura de pantalla real (KDE/spectacle) confirma la ventana GPUI
   abierta con la lista coloreada (`main.rs` naranja, `enlace.lnk` cian,
   `subdir` azul — los mismos RGB que el log).
5. Ninguno de los dos clientes murió ni afectó al otro: tras la carga de
   la GUI, la TUI (proceso `norte-tui`, PID vivo) y el daemon siguieron
   corriendo con la conexión de la TUI intacta (`lsof` la sigue viendo
   `CONNECTED`). Se pararon los tres a mano (`kill`/`tmux kill-session`)
   al terminar la verificación — nada quedó zombi.

Conclusión: **criterio 2 demostrado** — TUI y GUI, dos frontends
distintos, contra el mismo daemon y el mismo socket, a la vez, viendo el
mismo listado, sin interferirse.

## Alcance (spike, hito 1 de M5)

Cumple los criterios 1 (listado real, T4), 2 (simultaneidad, T6) y 3
(theming por tipo, T5) de la spec
(`docs/superpowers/specs/2026-07-19-m5-spike-gpui-design.md`). El
criterio 4 (medición de viabilidad + ADR go/no-go) es T7, fuera de este
README. Fuera de alcance de M5 hito 1 (quedan para el hito 2 si el ADR
sale `go`): dual-pane, mutación (copy/move/delete), keymap, hot-reload
de tema, Windows/macOS.
