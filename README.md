# norte

[![CI](https://github.com/compilando/norte/actions/workflows/ci.yml/badge.svg)](https://github.com/compilando/norte/actions/workflows/ci.yml)

File manager ortodoxo de nueva generación con arquitectura *headless-core*: un
core en Rust que expone un protocolo estable y un VFS universal, sobre el que se
montan frontends intercambiables (TUI, GUI, CLI) y sobre el que los agentes de
IA operan de forma gobernada (MCP, policy engine, journal, auditoría).

> Codename provisional (spec Anexo A). Estado: **M0 — esqueleto** en construcción.

- Spec fundacional: [`docs/spec/norte-spec.md`](docs/spec/norte-spec.md)
- Mapa del repo: [`ARCHITECTURE.md`](ARCHITECTURE.md)
- Decisiones: [`docs/adr/`](docs/adr/)

## Desarrollo

```bash
just ci      # fmt-check + clippy -D warnings + deny + nextest + docs (lo mismo que CI)
just test    # solo tests (cargo nextest)
just cov     # gate de cobertura local (85% en proto/vfs/core)
```

Toolchain pineada en `rust-toolchain.toml`; MSRV = stable − 2, testeada en CI.

## Licencias

Modelo Zed (spec §16.2): `norte-proto`, `norte-vfs*`, `norte-testkit` y el futuro
SDK de plugins son **MIT OR Apache-2.0** (escribe frontends, providers y plugins
sin fricción legal). `norte-core` y los frontends oficiales son **AGPL-3.0-only**.
Cada crate lleva sus ficheros `LICENSE-*`.

## Telemetría

Ninguna. Ni opt-in (spec §16.6). Los diagnósticos son locales.
