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
# `clean --profraw-only` ANTES: los `.profraw` de una corrida anterior dan
# porcentajes FALSOS (se han visto 58 % y 76 % espurios donde el real era
# 88 %). Sólo eso — `--workspace` borraría además el target instrumentado, que
# es un universo aparte (proto/vfs/core y su árbol) y recompilarlo entero es la
# mayor parte del coste de `just ci`. Ese universo lo tira `just prune`, que sí
# corre `clean --workspace`: se paga cuando hace falta disco, no en cada gate.
cov:
    cargo llvm-cov clean --profraw-only
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

# Rupturas de API pública contra el último tag (ADR 0038, #13).
#
# Se nombran los paquetes UNO A UNO, y no con `--workspace`, por dos razones
# distintas que empujan en la misma dirección:
#
# 1. Lo que importa son las librerías PUBLICABLES (MIT/Apache): son las que
#    consume un tercero. Los binarios AGPL —cli, tui, gui— y las librerías
#    internas AGPL no tienen API pública que romper.
# 2. `--workspace` ABORTA, no avisa, cuando un miembro no existía en la
#    baseline: contra `v0.3.0-alpha.2` se para en `norte-help` con «package
#    not found in <rev>» y no comprueba nada. `norte-help` nació después de
#    ese tag; entra en esta lista con la primera baseline que lo contenga.
#
# NO está en `ci`: cablearla antes de tener release publicado rompería cada
# `just ci` (decisión de gate del ADR 0038). Corre en `just release-check`.
semver baseline="v0.3.0-alpha.2":
    #!/usr/bin/env bash
    # La versión del workspace tiene que haber SUBIDO respecto a la baseline.
    # Si son iguales, cargo-semver-checks decide «no change; assume major» y se
    # salta las 254 comprobaciones de cada crate: sale verde, en cero coma, sin
    # haber mirado nada. Eso es peor que rojo — un gate que puede no comprobar
    # nada y decir que sí no es un gate. Se falla aquí, con el motivo.
    set -euo pipefail
    actual=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
    previa=$(git show {{baseline}}:Cargo.toml | grep -m1 '^version = ' | cut -d'"' -f2)
    if [ "$actual" = "$previa" ]; then
        echo "semver: la versión del workspace ($actual) es la de {{baseline}}." >&2
        echo "cargo-semver-checks se saltaría TODAS las comprobaciones." >&2
        echo "Sube la versión en Cargo.toml antes de correr esto." >&2
        exit 1
    fi
    cargo semver-checks --baseline-rev {{baseline}} \
        -p norte-proto -p norte-vfs -p norte-testkit \
        -p norte-vfs-local -p norte-vfs-sftp -p norte-vfs-object \
        -p norte-vfs-archive -p norte-config -p norte-encoding \
        -p norte-frontend -p norte-i18n -p norte-theme

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
prune days="2":
    #!/usr/bin/env bash
    set -euo pipefail
    antes=$(du -sk target 2>/dev/null | cut -f1 || echo 0)
    cargo llvm-cov clean --workspace 2>/dev/null || true
    rm -rf target/debug/incremental target/release/incremental target/tmp
    # El target de cargo-semver-checks (`just release-check`): 17 G medidos, y
    # se regenera solo. No lo tocaba nadie.
    rm -rf target/semver-checks
    # Los ejecutables de test muertos. ESTE es el grueso: 184 GiB de los 288 G
    # medidos eran 2075 exes en debug/deps, de los que 1262 (122 GiB) llevaban
    # más de un día sin tocarse. Cargo no borra NINGUNO: cada relink deja el
    # anterior ahí para siempre.
    #
    # Se barren SÓLO los ejecutables (fichero sin extensión y con +x), nunca
    # .rlib/.rmeta. Es deliberado: si el barrido se lleva uno que aún estaba
    # vivo, cargo lo vuelve a ENLAZAR (segundos con lld), no a compilar. Un
    # .rlib borrado por error sí costaría una compilación entera.
    barridos=$(find target/debug/deps target/release/deps -maxdepth 1 -type f -executable \
        ! -name '*.*' -mtime +{{days}} -print -delete 2>/dev/null | wc -l)
    despues=$(du -sk target 2>/dev/null | cut -f1 || echo 0)
    echo "exes de test barridos (>{{days}} días): $barridos"
    echo "target: $((antes / 1024 / 1024)) GiB → $((despues / 1024 / 1024)) GiB"
    df -h . | tail -1

# El martillo: tira TODO el target de este árbol.
prune-all:
    cargo clean
    @df -h . | tail -1

# ---------- desarrollo: ejecutar y probar a mano ----------

# El TUI (release: arranque frío <50 ms es presupuesto de la spec §12).
#
# `{{features}}` NO es decorativo aquí: cargo unifica features por invocación y
# keya los artefactos por el conjunto resultante. Sin ellas, este `cargo run`
# compilaba un universo COMPLETO y separado de norte-tui y de todo lo que
# cuelga (~30 G) que ninguna otra receta reusaba jamás.
run:
    cargo run --release -p norte-tui {{features}}

# El TUI en debug (compila más rápido; para iterar). Mismas features que el
# gate → reusa lo que ya compiló `just test`, coste normalmente cero.
dev:
    cargo run -p norte-tui {{features}}

# Pone `ntc` en el PATH apuntando al binario de ESTE árbol. `~/.local/bin` va
# antes que el bin de cargo en el PATH, así que gana al `cargo install`.
#
# Por qué un symlink y no `just install`: `cargo install --path` compila en un
# target temporal PROPIO, o sea un build en frío entero (~4-5 min y otro
# universo de disco) cada vez que quieras probar un cambio. El symlink apunta
# al binario que el gate ya construyó: coste cero y nunca rancio mientras
# corras los tests. `just install` sigue ahí para instalar de verdad.
#
# `dir` (por defecto debug) elige el perfil: `just link release` para medir
# arranque, que es lo único que debug no puede decirte.
link dir="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "{{dir}}" = "release" ]; then
        cargo build --release -p norte-tui -p norte-cli {{features}}
    else
        cargo build -p norte-tui -p norte-cli {{features}}
    fi
    mkdir -p ~/.local/bin
    for b in ntc norte; do
        ln -sfn "$PWD/target/{{dir}}/$b" ~/.local/bin/$b
        printf '%-6s → %s\n' "$b" "$(readlink ~/.local/bin/$b)"
    done
    echo "recuerda: el symlink apunta a ESTE árbol; un 'just prune-all' lo deja colgando"

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
    cargo build -q -p norte-cli {{features}}
    cargo build -q --manifest-path crates/norte-gui/Cargo.toml
    echo "[gui-demo] arrancando daemon efímero en $sock"
    cargo run -q -p norte-cli {{features}} -- daemon run --socket "$sock" --idle-timeout 0 &
    dpid=$!
    trap 'kill "$dpid" 2>/dev/null || true' EXIT INT TERM
    for _ in $(seq 1 100); do [ -S "$sock" ] && break; sleep 0.1; done
    [ -S "$sock" ] || { echo "[gui-demo] el daemon no abrió el socket a tiempo"; exit 1; }
    echo "[gui-demo] lanzando GUI (cierra la ventana para parar el daemon)"
    NORTE_SOCKET="$sock" cargo run --manifest-path crates/norte-gui/Cargo.toml {{args}}

# El CLI de humo (paths NATIVOS): `just cli ls /tmp`, `just cli cp a b`…
# Con `{{features}}` como todo lo que compila el core: sin ellas se fabricaba
# su propio universo de artefactos que ninguna otra receta reusaba.
cli *args:
    cargo run -p norte-cli {{features}} -- {{args}}

# Tests de un crate concreto: `just t norte-vfs`, `just t norte-tui`.
#
# Con las MISMAS features del gate a propósito. Un `cargo nextest run -p
# norte-tui` a secas no es más barato: compila un universo de artefactos
# DISTINTO (otro conjunto de features = otro fingerprint) para ese crate y
# todo su árbol, que además se queda en disco para siempre. Iterar con esta
# receta reaprovecha lo que `just ci` ya compiló, y al revés.
#
# El crate se elige FILTRANDO (`-E package(...)`), no con `-p`: `-p norte-vfs`
# junto a `--features norte-tui/schema` es un error de cargo —«el paquete no
# contiene esas features»— porque con un solo paquete seleccionado ya no hay
# workspace donde resolver el resto. La receta llevaba tiempo rota por eso.
# Filtrar selecciona los mismos tests SIN cambiar el conjunto de paquetes, que
# es justo lo que hace que se reaproveche la compilación del gate.
t crate:
    CARGO_INCREMENTAL=0 cargo nextest run {{core_pkgs}} {{features}} -E 'package({{crate}})'

# Clippy con las features del gate. SIN argumento de crate, y no por descuido:
# clippy no tiene el filtro que `nextest` sí tiene, y recortar los paquetes
# cambia la unificación de features —o sea, el universo de artefactos— con lo
# que se perdería justo lo que hace barata esta receta. Warm cuesta lo que
# cuesta revisar lo que tocaste; el resto sale de la caché.
c:
    CARGO_INCREMENTAL=0 cargo clippy {{core_pkgs}} --all-targets {{features}} -- -D warnings

# Loop de desarrollo: tests del workspace en cada guardado (exige cargo-watch).
# Mismas features que el gate: `cargo watch` sin ellas recompilaba el
# workspace entero en un universo propio a cada guardado de fichero.
watch:
    cargo watch -x "nextest run {{core_pkgs}} {{features}}"

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

# Construye e INSTALA el previewer de syntect: el primer plugin real que se
# puede tener instalado, en vez de existir solo como fixture de un test.
#
# Se monta en `target/plugin-stage/` y se instala desde ahí: el `plugin.wasm`
# es un artefacto de build y no tiene por qué aparecer junto al `plugin.toml`
# en el árbol de fuentes.
#
# Instalar NO aprueba: el plugin queda descubierto y sin consentir, y se
# aprueba y activa en el gestor de extensiones (F12 en la TUI).
plugin-syntect:
    #!/usr/bin/env bash
    set -euo pipefail
    origen=crates/norte-plugin-host/examples-wasm/previewer-syntect
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/previewer-syntect
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    cp $origen/target/wasm32-wasip2/release/previewer_syntect.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" "$@"

# ---------- distribución ----------

# Construye los artefactos de release para ESTA máquina.
#
# `dist-workspace.toml` declara CINCO targets: son los que produciría una
# release desde CI. Aquí solo sale el del host, y `--target` es explícito por
# DOS motivos: sin él dist intenta los cinco y se para en el primer cruce a
# macOS, y los instaladores («global») se generan con la tabla de plataformas
# que se les pase — sin acotarla, el instalador le prometería a un macOS un
# archivo que esta release no contiene y moriría en un 404 en vez de decir
# «no hay binario para tu plataforma».
#
# `rm -rf` primero: dist no limpia, y un `.ps1` de una corrida anterior con
# otra configuración se subiría como si fuera de esta.
#
# Cross-compilar aws-lc-rs es lo que el ADR 0021 dio por frágil; macOS y
# Windows necesitan sus máquinas. Las notas de la release dicen qué lleva; la
# config no se recorta para disimularlo.
dist:
    rm -rf target/distrib
    dist build --artifacts=local --target=$(rustc -vV | sed -n 's/^host: //p')
    dist build --artifacts=global --target=$(rustc -vV | sed -n 's/^host: //p')
    @echo "artefactos en target/distrib/:"
    @ls -1 target/distrib/

# Arranca los binarios DESDE los artefactos construidos (no desde
# target/release).
dist-smoke:
    ./scripts/dist-smoke.sh

# Sube a la release del tag lo construido aquí. El tag ya tiene que existir y
# estar empujado: esto publica, no etiqueta.
#
# Se sube TODO fichero suelto de `target/distrib` en vez de una lista de globs:
# qué produce dist depende de los targets que se le pasen (sin Windows no hay
# `.ps1`), y un glob sin coincidencias se pasaría literal y reventaría la
# subida entera. `-maxdepth 1` deja fuera los directorios de staging.
#
# Los esquemas van con los binarios a propósito (#13): un tercero que quiera
# escribir un cliente no debería tener que clonar el repositorio para saber la
# forma del protocolo.
dist-publish tag:
    ./scripts/dist-smoke.sh
    gh release upload {{tag}} \
        $(find target/distrib -maxdepth 1 -type f) \
        docs/schema/proto.schema.json \
        docs/schema/norte.schema.json \
        docs/schema/keymap.schema.json \
        --clobber
    @echo "subido a {{tag}}. Comprueba: gh release view {{tag}}"
