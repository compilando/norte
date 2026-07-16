# 0021 — Distribución: binarios prebuilt e instalador `curl | sh` con cargo-dist

- Estado: accepted
- Fecha: 2026-07-15
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §15/§18 (release), §16 (licencias/telemetría),
  ADR 0011 (daemon/panics supervisados). Sin hito propio: infraestructura de
  release para la alpha (v0.3.0-alpha.1).

## Contexto y problema

El proyecto no tenía forma de INSTALARSE salvo compilar desde fuente (779 deps,
~5 min). El objetivo: un instalador de una línea, `curl -sSf …/installer.sh |
sh`, con binarios prebuilt para cualquier SO —como uv, starship, rustup—, y
build optimizado.

## Decisión

### D1 — `cargo-dist` (`dist` 0.32) como tooling de release

Se adopta **cargo-dist**: la herramienta estándar del ecosistema Rust para
exactamente esto. `dist init` generó `.github/workflows/release.yml` +
`dist-workspace.toml`. Al empujar un tag de versión (`v*`), el workflow:

- compila los binarios para cada target en runners de GitHub,
- genera **`norte-tui-installer.sh`** (shell) y **`.ps1`** (PowerShell) + un
  `sha256.sum`,
- crea el GitHub Release con notas derivadas del `CHANGELOG.md` y sube todo.

Instalación resultante (cualquier linux/macOS):

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/compilando/norte/releases/latest/download/norte-tui-installer.sh | sh
```

Alternativa evaluada y descartada: workflow + `install.sh` a mano — reinventa
lo que cargo-dist ya pule (checksums, musl, firma macOS, Windows, PATH) con
coste de mantenimiento permanente.

### D2 — Targets: gnu-linux + macOS + windows; musl diferido

`aarch64/x86_64-apple-darwin`, `aarch64/x86_64-unknown-linux-gnu`,
`x86_64-pc-windows-msvc`. glibc cubre las distros modernas. **musl estático**
(el «cualquier distro vieja/Alpine») queda DIFERIDO: `aws-lc-rs` (la pila
cripto de russh/rustls/opendal) sobre musl exige toolchain cruzado y es frágil;
se añadirá cuando se valide en CI. Windows-arm y otros, según demanda.

### D3 — Solo el TUI en el release (un instalador)

`norte-cli` (binario `norte`) es el «banco de pruebas de M0» (spec): se marca
`[package.metadata.dist] dist = false`, de modo que el release tiene UNA sola
app —el TUI— y por tanto UN instalador. El CLI se compila desde fuente
(`cargo install --path crates/norte-cli`). El nombre del comando distribuido
(`norte-tui`) y si el CLI debe entrar más adelante quedan como decisión de
producto abierta, no de este ADR.

### D4 — Build optimizado (`[profile.release]`)

`lto = "thin"`, `codegen-units = 1`, `strip = true`: ~30 % menos tamaño de
binario (26 MiB → 18 MiB). `opt-level = 3` se conserva (arranque frío <50 ms es
presupuesto de §12). **`panic` se queda en `unwind`** (default) a propósito: el
core supervisa los panics de tasks con `catch_unwind` (spec §17.7,
`Internal{panic:true}`) — `abort` lo rompería. `cargo-dist` añade
`[profile.dist]` (hereda `release`) para sus builds.

## Consecuencias

Positivas: instalación de una línea multiplataforma, checksums, releases
automáticos desde un tag, notas desde el changelog; cero mantenimiento de
scripts de release propios; binario un tercio más pequeño.

Negativas / deuda: el workflow de release NO se puede runtime-verificar en
local (corre en GitHub Actions al hacer tag) — el primer release exige empujar
un tag NUEVO ya con `release.yml` en `main` (el `v0.3.0-alpha.1` actual es
previo al workflow y no lo dispara). musl diferido. El grueso del binario (18
MiB) es el grafo de providers (opendal/aws/russh cripto) embebido; recortarlo
más = feature-gate de providers (refactor, futuro). Nueva dep de tooling
(cargo-dist, solo en CI/dev — no entra en el árbol del binario).
