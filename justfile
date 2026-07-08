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
test:
    cargo nextest run --workspace --no-tests=pass

# Gate de cobertura (mismo umbral que CI): solo crates de lógica (spec §12).
cov:
    cargo llvm-cov nextest -p norte-proto -p norte-vfs -p norte-core --fail-under-lines 85

docs:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Lo que corre CI. `cov` entra al gate cuando norte-proto tenga código (fase 3 de M0);
# hasta entonces llvm-cov no tiene líneas que medir.
ci: lint test docs
