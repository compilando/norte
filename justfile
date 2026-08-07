# Interfaz única de comandos: humanos, Claude y CI corren exactamente esto.

default: ci

# El ÚNICO conjunto de features del gate. Una sola fuente porque cargo no
# comparte artefactos entre conjuntos distintos: `cargo nextest -p norte-tui`
# a secas y `just test` compilan DOS universos completos de norte-tui y de
# todo lo que depende de él, y ninguno de los dos se borra jamás. Cada
# universo del workspace pesa ~30 G. Toda receta que compile el workspace
# usa esta variable; ver `just prune` y `just disk`.
#
# Features EXPLÍCITAS y no `--all-features`: `it-openssh` (norte-vfs-sftp) es
# un test nightly contra Docker (ADR 0013) y no debe ni compilar aquí.
features := "--features norte-tui/schema --features norte-config/watch --features norte-proto/schema"

# Los paquetes del gate: TODO el workspace MENOS la GUI. Esto NO es una
# optimización de tiempo, es de corrección, y `default-members` no basta porque
# `--workspace` lo pisa.
#
# GPUI activa `serde_json/preserve_order`. Las features de cargo se UNIFICAN
# por invocación: si `norte-gui` entra en el mismo `cargo` que el core, el
# `serde_json` del core pasa de mapas ordenados (BTreeMap) a orden de inserción
# (IndexMap) — y con él cambia el JSON Schema que PUBLICAMOS, el `--json` del
# CLI y los goldens. Se descubrió al meter la GUI en `members`: cinco tests en
# rojo, ninguno por un cambio de código.
#
# Así que el core se compila y se prueba EXACTAMENTE como se distribuye: sin la
# GUI en la invocación. La GUI tiene su gate propio (`just gui-ci`), donde la
# unificación es asunto suyo.
core_pkgs := "--workspace --exclude norte-gui"

# Suelo de disco libre (GiB) por debajo del cual `just ci` se niega a
# arrancar. Un `cargo build` del workspace más el target instrumentado de
# cobertura necesitan del orden de 40 G; quedarse sin disco a mitad no da un
# error limpio: corrompe artefactos y deja errores de linker (`os error 28`)
# que parecen bugs del código.
disk_floor := "40"

fmt:
    cargo fmt --all

# Recompila el guest ftp-provider a wasm32-wasip2 y actualiza el artefacto
# EMBEBIDO en norte-core (ADR 0033). Correr tras tocar el guest ftp-provider o
# la interfaz WIT `provider`.
build-ftp-wasm:
    cargo build --release --target wasm32-wasip2 \
        --manifest-path crates/norte-plugin-host/examples-wasm/ftp-provider/Cargo.toml
    cp crates/norte-plugin-host/examples-wasm/ftp-provider/target/wasm32-wasip2/release/ftp_provider.wasm \
        crates/norte-core/resources/ftp-provider.wasm

fmt-check:
    cargo fmt --all -- --check

lint: fmt-check
    CARGO_INCREMENTAL=0 cargo clippy {{core_pkgs}} --all-targets {{features}} -- -D warnings
    cargo deny check

# --no-tests=pass: el esqueleto de fase 1 no tiene tests aún; con código real
# el gate de cobertura (85%) hace imposible un workspace sin tests que pase CI.
# nextest no corre doctests: van aparte (los exige la convención de rustdoc).
# Las features salen de `{{features}}`: una sola fuente para todo el gate.
# CARGO_INCREMENTAL=0 porque la compilación incremental no aporta nada a una
# corrida completa (se recompila todo igual) y su caché pesa ~8 G por universo.
test:
    CARGO_INCREMENTAL=0 cargo nextest run {{core_pkgs}} {{features}} --no-tests=pass --no-fail-fast
    CARGO_INCREMENTAL=0 cargo test {{core_pkgs}} {{features}} --doc

# Gate de cobertura (mismo umbral que CI): solo crates de lógica (spec §12).
#
# `clean` ANTES por dos razones que van juntas: los `.profraw` de una corrida
# anterior dan porcentajes FALSOS (se han visto 58 % y 76 % espurios donde el
# real era 88 %), y el target instrumentado es un universo entero aparte
# —proto/vfs/core y su árbol, otra vez— que si no se tira crece sin fin.
cov:
    cargo llvm-cov clean --workspace
    CARGO_INCREMENTAL=0 cargo llvm-cov nextest -p norte-proto -p norte-vfs -p norte-core --fail-under-lines 85

docs:
    RUSTDOCFLAGS="-D warnings" CARGO_INCREMENTAL=0 cargo doc {{core_pkgs}} --no-deps

# Chequeo barato de norte-gui (GP review): el gate del core la deja fuera a
# propósito (ver `core_pkgs`), así que un bump de proto/frontend/core puede
# romper la GUI sin que nada lo note hasta correr `gui-ci` a mano. Solo
# `cargo check` (no el gate completo `gui-ci`: nextest+clippy+fmt son caros
# para correr en cada `just ci`) — suficiente para atrapar una API rota.
# `--locked`: el lockfile es ahora el del workspace (la GUI es miembro y ya no
# tiene el suyo); sin esta bandera el chequeo podría reescribirlo en silencio.
check-gui:
    cd crates/norte-gui && cargo check --locked

# Detección de rupturas de API en los crates publicables (ADR 0038, #13).
# Herramienta externa: `cargo install cargo-semver-checks`. Necesita una
# BASELINE: el primer release publicado o un tag git (`--baseline-rev vX.Y.Z`);
# hasta que exista ese tag no hay contra qué comparar y la receta no corre.
# `--exclude` deja fuera lo NO publicable (frontends/binarios y el árbol de la
# GUI, excluido del workspace). AÚN NO está en `ci`: cablearla antes de tener
# binario+baseline rompería cada `just ci` (decisión de gate del ADR 0038); se
# añade a `ci` cuando ambos estén disponibles.
baseline := "HEAD"
semver:
    cargo semver-checks --workspace --baseline-rev {{baseline}}

# Lo que corre CI. `_disk` primero: quedarse sin disco a mitad de un build no
# falla limpio, corrompe artefactos.
ci: _disk lint test cov docs check-gui

# Iteración rápida: todo el gate MENOS cobertura (cov recompila proto/vfs/core
# instrumentados en su propio target y re-corre sus tests: ~34 s fijos incluso
# sin cambios). El gate real pre-commit sigue siendo `just ci`.
ci-fast: _disk lint test docs

# ---------- disco: por qué se llena y cómo recuperarlo ----------

# Guarda de espacio libre. Falla ANTES de compilar en vez de a mitad.
_disk:
    #!/usr/bin/env bash
    set -euo pipefail
    libre=$(df -BG --output=avail . | tail -1 | tr -dc '0-9')
    if [ "$libre" -lt {{disk_floor}} ]; then
        echo "disco: ${libre} GiB libres, por debajo del suelo de {{disk_floor}} GiB." >&2
        echo "corre 'just prune' (o 'just disk' para ver dónde está el espacio)." >&2
        exit 1
    fi

# Dónde está el espacio: el target de este árbol, sus piezas, los targets de
# otros worktrees y el registro de cargo.
disk:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "== libre =="; df -h . | tail -1
    echo "== target de este árbol =="
    # `|| true` en cada du: un build en marcha borra ficheros bajo los pies
    # de du y lo hace salir con error aunque el total sea correcto.
    [ -d target ] && du -sh target 2>/dev/null || true
    for d in target/debug/deps target/debug/incremental target/debug/build target/llvm-cov-target target/tmp; do
        [ -d "$d" ] && du -sh "$d"
    done
    echo "== targets de OTROS worktrees (no los toca 'just prune') =="
    git worktree list --porcelain | awk '/^worktree /{print $2}' | while read -r w; do
        [ "$w" = "$PWD" ] && continue
        [ -d "$w/target" ] && du -sh "$w/target"
    done
    echo "== registro de cargo (compartido; no es del proyecto) =="
    du -sh "${CARGO_HOME:-$HOME/.cargo}" 2>/dev/null || true

# Recupera espacio SIN tirar el build entero: la caché incremental (inútil
# entre corridas completas) y el target instrumentado de cobertura (se
# regenera en cada `just cov`). Deja intactos los artefactos que hacen que la
# siguiente compilación sea rápida.
#
# Por qué hace falta una receta: cargo NUNCA recolecta basura. Cada conjunto
# de features, cada versión de dependencia y cada toolchain deja su universo
# de artefactos ahí para siempre, y cada universo del workspace pesa ~30 G.
# Si esto no basta, `just prune-all` tira el target completo (la siguiente
# compilación es desde cero, varios minutos).
prune:
    #!/usr/bin/env bash
    set -euo pipefail
    antes=$(du -sk target 2>/dev/null | cut -f1 || echo 0)
    cargo llvm-cov clean --workspace 2>/dev/null || true
    rm -rf target/debug/incremental target/release/incremental target/tmp
    despues=$(du -sk target 2>/dev/null | cut -f1 || echo 0)
    echo "target: $((antes / 1024 / 1024)) GiB → $((despues / 1024 / 1024)) GiB"
    df -h . | tail -1

# El martillo: tira TODO el target de este árbol.
prune-all:
    cargo clean
    @df -h . | tail -1

# ---------- desarrollo: ejecutar y probar a mano ----------

# El TUI (release: arranque frío <50 ms es presupuesto de la spec §12).
run:
    cargo run --release -p norte-tui

# El TUI en debug (compila más rápido; para iterar).
dev:
    cargo run -p norte-tui

# La GUI (GPUI). EXCLUIDA del workspace (spike M5, regla 7) → --manifest-path,
# NUNCA entra en `just ci`. Es siempre por daemon: arranca antes `norte daemon
# run`. Dir inicial vía `NORTE_DIR=file:///ruta just gui`; socket vía
# `NORTE_SOCKET`. Args libres: `just gui --release`.
gui *args:
    cargo run --manifest-path crates/norte-gui/Cargo.toml {{args}}

# La GUI en UN solo comando: arranca un daemon EFÍMERO (socket propio, sin
# idle-shutdown), lanza la GUI contra él, y para el daemon al cerrar la ventana.
# Para probar sin gestionar el daemon a mano. Dir inicial vía
# `NORTE_DIR=file:///ruta just gui-demo`. Args extra van a la GUI (`--release`).
gui-demo *args:
    #!/usr/bin/env bash
    set -euo pipefail
    sockdir="${XDG_RUNTIME_DIR:-/tmp}/norte-gui-demo"
    sock="$sockdir/daemon.sock"
    mkdir -p "$sockdir"; chmod 700 "$sockdir"; rm -f "$sock"
    echo "[gui-demo] compilando daemon + GUI…"
    cargo build -q -p norte-cli
    cargo build -q --manifest-path crates/norte-gui/Cargo.toml
    echo "[gui-demo] arrancando daemon efímero en $sock"
    cargo run -q -p norte-cli -- daemon run --socket "$sock" --idle-timeout 0 &
    dpid=$!
    trap 'kill "$dpid" 2>/dev/null || true' EXIT INT TERM
    for _ in $(seq 1 100); do [ -S "$sock" ] && break; sleep 0.1; done
    [ -S "$sock" ] || { echo "[gui-demo] el daemon no abrió el socket a tiempo"; exit 1; }
    echo "[gui-demo] lanzando GUI (cierra la ventana para parar el daemon)"
    NORTE_SOCKET="$sock" cargo run --manifest-path crates/norte-gui/Cargo.toml {{args}}

# El CLI de humo (paths NATIVOS): `just cli ls /tmp`, `just cli cp a b`…
cli *args:
    cargo run -p norte-cli -- {{args}}

# Tests de un crate concreto: `just t norte-vfs`, `just t norte-tui`.
#
# Con las MISMAS features del gate a propósito. Un `cargo nextest run -p
# norte-tui` a secas no es más barato: compila un universo de artefactos
# DISTINTO (otro conjunto de features = otro fingerprint) para ese crate y
# todo su árbol, que además se queda en disco para siempre. Iterar con esta
# receta reaprovecha lo que `just ci` ya compiló, y al revés.
t crate:
    cargo nextest run -p {{crate}} {{features}}

# Clippy de un crate concreto, con las features del gate por lo mismo:
# `just c norte-tui`.
c crate:
    cargo clippy -p {{crate}} --all-targets {{features}} -- -D warnings

# Loop de desarrollo: tests del workspace en cada guardado (exige cargo-watch).
watch:
    cargo watch -x "nextest run {{core_pkgs}}"

# Tests de integración NIGHTLY contra servidores REALES por Docker (ADR 0013/
# 0016): sftp contra OpenSSH real (atmoz/sftp) y S3 real. EXIGEN Docker; fuera
# del gate de PR (lo corre el workflow nightly, no `just ci`). (FTP real: el
# provider es ahora un plugin WASM — ADR 0033 — cuyo contrato corre in-process
# contra libunftp en `just ci`; no hay job nightly Docker propio.)
it-remote:
    cargo nextest run -p norte-vfs-sftp --features it-openssh
    cargo nextest run -p norte-vfs-object --features it-s3 -E 'binary(reals3)'

# Benchmarks de los presupuestos de la spec §12 (manual: tardan).
bench:
    cargo bench -p norte-tui --bench presupuestos
    cargo bench -p norte-core --bench copy_remoto

# ---------- instalación ----------

# Instala en $CARGO_HOME/bin —~/.cargo/bin por defecto— (release):
# `ntc` (el gestor) y `norte` (el CLI).
# --locked: exactamente las versiones del Cargo.lock que pasó CI.
install:
    cargo install --path crates/norte-tui --locked
    cargo install --path crates/norte-cli --locked
    @echo "instalados: $(command -v ntc) y $(command -v norte)"

# `cargo uninstall` toma el nombre del CRATE, no el del binario: el paquete
# sigue llamándose `norte-tui` aunque instale un `ntc`. No es un despiste de la
# línea de arriba.
uninstall:
    cargo uninstall norte-tui
    cargo uninstall norte-cli

# Gate PROPIO de norte-gui (M5): el crate es miembro del workspace pero está
# FUERA de `default-members` a propósito (GPUI = deps GPU pesadas; regla 7:
# solo habla norte-proto), así que `just ci` no lo cubre — este es su gate a un
# comando. Correrlo al tocar norte-gui o cualquier crate que la GUI consume
# (frontend/proto/core).
gui-ci:
    cd crates/norte-gui && cargo nextest run && cargo clippy --all-targets -- -D warnings && cargo fmt --check
    # La GUI es miembro del workspace pero está FUERA del grafo de `cargo deny`
    # del core (`[graph] exclude`): el árbol de GPUI trae git-sources y
    # licencias que no deben relajar la auditoría de las librerías publicables.
    # Se audita aquí, contra su propia política, con su manifiesto como ÚNICA
    # raíz del grafo.
    cargo deny --manifest-path crates/norte-gui/Cargo.toml check --config crates/norte-gui/deny.toml
