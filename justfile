# Interfaz única de comandos: humanos, Claude y CI corren exactamente esto.

default: ci

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint: fmt-check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo deny check

# --no-tests=pass: el esqueleto de fase 1 no tiene tests aún; con código real
# el gate de cobertura (85%) hace imposible un workspace sin tests que pase CI.
# nextest no corre doctests: van aparte (los exige la convención de rustdoc).
# Features EXPLÍCITAS en vez de --all-features: `it-openssh` (norte-vfs-sftp)
# es un test nightly contra Docker (ADR 0013) y NO debe entrar al gate de PR
# —ni correrse ni compilar su árbol (testcontainers/bollard)—. Toda feature
# nueva apta para el gate se añade aquí; las de integración/nightly, no.
test:
    cargo nextest run --workspace --features norte-tui/schema --no-tests=pass --no-fail-fast
    cargo test --workspace --features norte-tui/schema --doc

# Gate de cobertura (mismo umbral que CI): solo crates de lógica (spec §12).
cov:
    cargo llvm-cov nextest -p norte-proto -p norte-vfs -p norte-core --fail-under-lines 85

docs:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Lo que corre CI.
ci: lint test cov docs

# ---------- desarrollo: ejecutar y probar a mano ----------

# El TUI (release: arranque frío <50 ms es presupuesto de la spec §12).
run:
    cargo run --release -p norte-tui

# El TUI en debug (compila más rápido; para iterar).
dev:
    cargo run -p norte-tui

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
# 0014): sftp contra OpenSSH real (atmoz/sftp) y ftp contra pure-ftpd real
# (delfer/alpine-ftp-server). EXIGEN Docker; fuera del gate de PR (lo corre el
# workflow nightly, no `just ci`).
it-remote:
    cargo nextest run -p norte-vfs-sftp --features it-openssh
    cargo nextest run -p norte-vfs-ftp --features it-ftp -E 'binary(realftp)'
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
