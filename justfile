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
test:
    cargo nextest run --workspace --all-features --no-tests=pass --no-fail-fast
    cargo test --workspace --doc

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

# Benchmarks de los presupuestos de la spec §12 (manual: tardan).
bench:
    cargo bench -p norte-tui --bench presupuestos
