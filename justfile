# Interfaz única de comandos: humanos, Claude y CI corren exactamente esto.

default: ci

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
    cargo clippy --workspace --all-targets --features norte-config/watch -- -D warnings
    cargo deny check

# --no-tests=pass: el esqueleto de fase 1 no tiene tests aún; con código real
# el gate de cobertura (85%) hace imposible un workspace sin tests que pase CI.
# nextest no corre doctests: van aparte (los exige la convención de rustdoc).
# Features EXPLÍCITAS en vez de --all-features: `it-openssh` (norte-vfs-sftp)
# es un test nightly contra Docker (ADR 0013) y NO debe entrar al gate de PR
# —ni correrse ni compilar su árbol (testcontainers/bollard)—. Toda feature
# nueva apta para el gate se añade aquí; las de integración/nightly, no.
test:
    cargo nextest run --workspace --features norte-tui/schema --features norte-config/watch --no-tests=pass --no-fail-fast
    cargo test --workspace --features norte-tui/schema --doc

# Gate de cobertura (mismo umbral que CI): solo crates de lógica (spec §12).
cov:
    cargo llvm-cov nextest -p norte-proto -p norte-vfs -p norte-core --fail-under-lines 85

docs:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Chequeo barato de norte-gui (GP review): el crate está EXCLUIDO del
# workspace (regla 7 / GPU pesada), así que `cargo check --workspace` NUNCA
# lo toca — un bump de proto/frontend/core podía romper la GUI sin que nada
# lo notara hasta correr `gui-ci` a mano. Solo `cargo check` (no el gate
# completo `gui-ci`: nextest+clippy+fmt son caros para correr en cada `just
# ci`) — suficiente para atrapar una API rota.
check-gui:
    cd crates/norte-gui && cargo check

# Lo que corre CI.
ci: lint test cov docs check-gui

# Iteración rápida: todo el gate MENOS cobertura (cov recompila proto/vfs/core
# instrumentados en su propio target y re-corre sus tests: ~34 s fijos incluso
# sin cambios). El gate real pre-commit sigue siendo `just ci`.
ci-fast: lint test docs

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
t crate:
    cargo nextest run -p {{crate}}

# Loop de desarrollo: tests del workspace en cada guardado (exige cargo-watch).
watch:
    cargo watch -x "nextest run --workspace"

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
# `norte-tui` (el TUI) y `norte` (el CLI).
# --locked: exactamente las versiones del Cargo.lock que pasó CI.
install:
    cargo install --path crates/norte-tui --locked
    cargo install --path crates/norte-cli --locked
    @echo "instalados: $(command -v norte-tui) y $(command -v norte)"

uninstall:
    cargo uninstall norte-tui
    cargo uninstall norte-cli

# Gate PROPIO de norte-gui (M5): el crate está EXCLUIDO del workspace a
# propósito (GPUI = deps GPU pesadas; regla 7: solo habla norte-proto) y
# `just ci` no lo cubre — este es su gate a un comando. Correrlo al tocar
# norte-gui o cualquier crate que la GUI consume (frontend/proto/core).
gui-ci:
    cd crates/norte-gui && cargo nextest run && cargo clippy --all-targets -- -D warnings && cargo fmt --check
