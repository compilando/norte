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
#
# `norte-core/testing` abre las puertas que los e2e necesitan y la biblioteca
# publicada NO debe tener (#241): sin ella, `columns_git_e2e` no compila, que
# es exactamente lo que se quiere de una puerta de test.
features := "--features norte-tui/schema --features norte-config/watch --features norte-proto/schema --features norte-core/testing"

# Los paquetes del gate: el workspace entero.
#
# Hasta 2026-08-20 esto excluía `norte-gui`, y no por tiempo sino por
# corrección: GPUI activaba `serde_json/preserve_order` y las features de cargo
# se unifican POR invocación, así que meterla en el mismo `cargo` que el core
# cambiaba el JSON que publicamos (mapas ordenados → orden de inserción) y
# ponía cinco tests en rojo sin que nadie tocara código. Retirada la GUI GPUI
# (ADR 0065), la exclusión sobra — pero la lección no: si un miembro nuevo trae
# una feature que cambia el comportamiento del core, se saca de aquí otra vez.
# `norte-gui-tauri` queda FUERA de ESTE gate, y no porque sea provisional: es
# un frontend soportado desde el 2026-09-01 (ADR 0087). Queda fuera porque
# compilarlo exige WebKitGTK, GTK3 y libsoup3 del sistema, y ninguna otra parte
# del árbol los necesita — un gate que no arranca en una máquina sin ellos deja
# de ser un gate. Está en `members` a propósito: así el `Cargo.lock` fija las
# versiones de Tauri y `cargo fmt --all` lo cubre.
#
# Su gate es `just gui-ci`, y lo corre `.github/workflows/gui.yml` en cada
# cambio que le llegue. Correrlo a mano no bastaba: el 2026-09-01 llevaba rojo
# en `main` sin que nadie lo supiera.
core_pkgs := "--workspace --exclude norte-gui-tauri"

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

lint: fmt-check deny-guests
    CARGO_INCREMENTAL=0 cargo clippy {{core_pkgs}} --all-targets {{features}} -- -D warnings
    cargo deny check

# Advisories de los guests WASM, que están FUERA del workspace y del lock (ver
# deny-guests.toml). Solo `advisories`: no compila nada, resuelve el árbol.
deny-guests:
    #!/usr/bin/env bash
    set -euo pipefail
    for m in crates/norte-plugin-host/examples-wasm/*/Cargo.toml plugins/*/Cargo.toml; do
        cargo deny --manifest-path "$m" --config deny-guests.toml check -A advisory-not-detected advisories
    done

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
    # `--features norte-core/testing` y no `{{features}}`: `cov` selecciona con
    # `-p`, y con un solo paquete seleccionado cargo rechaza una feature de otro
    # («el paquete no contiene esas features») — la misma trampa que la receta
    # `t` documenta. Sin esta feature, `tests/columns_git_e2e.rs` no ve
    # `plugins::run_column_values_for_test` (gateada
    # `cfg(any(test, feature = "testing"))`) y `cov` no compilaba: un test de
    # integración es otro crate y no hereda el `cfg(test)` de la lib. Roto desde
    # que entró ese test, y no se vio porque `cov` es lo último de `just ci` y
    # `ci-fast` no lo incluye.
    CARGO_INCREMENTAL=0 cargo llvm-cov nextest -p norte-proto -p norte-vfs -p norte-core -p norte-vfs-local -p norte-client --features norte-core/testing --fail-under-lines 85

docs:
    RUSTDOCFLAGS="-D warnings" CARGO_INCREMENTAL=0 cargo doc {{core_pkgs}} --no-deps

# Rupturas de API pública contra el último tag (ADR 0038, #13).
#
# Se nombran los paquetes UNO A UNO, y no con `--workspace`, por dos razones
# distintas que empujan en la misma dirección:
#
# 1. Lo que importa son las librerías PUBLICABLES (MIT/Apache): son las que
#    consume un tercero. Los binarios AGPL —cli y tui— y las librerías
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
ci: _disk lint test cov docs
    @just _sellar

# Iteración rápida: todo el gate MENOS cobertura (cov recompila proto/vfs/core
# instrumentados en su propio target y re-corre sus tests: ~34 s fijos incluso
# sin cambios). El gate real pre-commit sigue siendo `just ci`.
ci-fast: _disk lint test docs
    @just _sellar

# Deja constancia de que este CONTENIDO pasó el gate, para que el `pre-push`
# no lo repita. Vive en `target/`: no se versiona ni viaja a otra máquina — el
# sello vale donde se corrió.
#
# Sin esto, quien hace lo correcto (correr el gate y luego empujar) lo paga dos
# veces, y un suelo que cuesta veinte minutos acaba siendo un `--no-verify` de
# costumbre.
#
# Se hashea el ÁRBOL DE TRABAJO y no `HEAD^{tree}`. La primera versión hacía lo
# segundo y no servía para nada en el flujo normal: cuando corres el gate, tus
# cambios todavía no están commiteados, así que sellaba el árbol del commit
# ANTERIOR y el hook volvía a pagarlo entero. Lo demostró el primer push que lo
# usó. Lo que el gate valida son los ficheros del disco, así que es eso lo que
# se sella. Cuesta 0,2 s sobre 1.248 ficheros.
_sellar:
    @just _huella > target/.norte-gate-ok 2>/dev/null || true

# La huella del contenido seguido por git, tal como está en el disco.
_huella:
    @git ls-files -z | xargs -0 sha256sum 2>/dev/null | sha256sum | cut -d' ' -f1

# Instala los hooks del repositorio (`.githooks/`). Una vez por clon.
#
# Hoy hay uno: `pre-push` corre `ci-fast`, y `gui-ci` si el push toca la
# ventana o algo que entra en ella. Existe porque un gate que depende de que
# alguien se acuerde se pudre — `gui-ci` llevaba semanas rojo en `main`
# (ADR 0087) — y es el suelo que no depende de ningún servicio de nadie.
#
# Es un suelo, no una cerradura: `git push --no-verify` lo salta.
#
# Instala los hooks del repositorio. Una vez por clon.
hooks:
    git config core.hooksPath .githooks
    @echo "hooks instalados desde .githooks/ (pre-push: ci-fast [+ gui-ci])"

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
    # Los directorios de `deps` que EXISTAN: sin `release/` (lo normal en una
    # máquina que solo compila en debug) `find` sale con error, y con
    # `pipefail` eso mataba la receta ENTERA justo antes de barrer nada.
    dirs=()
    for d in target/debug/deps target/release/deps; do
        [ -d "$d" ] && dirs+=("$d")
    done
    barridos=0
    if [ ${#dirs[@]} -gt 0 ]; then
        barridos=$(find "${dirs[@]}" -maxdepth 1 -type f -executable \
            ! -name '*.*' -mtime +{{days}} -print -delete 2>/dev/null | wc -l)
    fi
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

# La receta de PRIMERA VEZ en una máquina: deja `ntc`, `norte` y `ntc-gui` en
# el PATH apuntando a este árbol, y no hay nada más que hacer después. Los
# enlaces son symlinks al `target/` de aquí, así que a partir de ese momento
# cualquier build (tuya o del gate) actualiza los tres comandos sola.
#
# No se llama `setup` porque `make setup` ya es otra cosa — el bootstrap del
# toolchain (rustup, just, nextest) — y dos `setup` que hacen cosas distintas
# es exactamente el tipo de detalle que se teclea mal a las dos de la mañana.
#
# Tres cosas que esta receta hace y `just link` + `just link-gui` sueltas no:
#
# - Comprueba que `~/.local/bin` está en el PATH y, si no, dice cómo meterlo
#   en fish. Enlazar en un directorio que nadie mira es el fallo silencioso
#   clásico: la receta dice "hecho" y el comando no existe.
# - La ventana es OPCIONAL. Si falta WebKitGTK/GTK3/libsoup3/npm, la parte
#   gráfica avisa y sigue, en vez de dejar la máquina sin `ntc` — que es el
#   mismo motivo por el que `core_pkgs` deja la GUI fuera del gate.
# - `--gui`/`--no-gui` fuerza la decisión cuando no quieras que la adivine.
#
# `dir` elige perfil igual que en `just link`: `just link-all release`.
link-all dir="debug" gui="auto":
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p ~/.local/bin
    case ":$PATH:" in
        *":$HOME/.local/bin:"*) ;;
        *)
            echo "aviso: ~/.local/bin no está en el PATH. En fish:" >&2
            echo "  fish_add_path ~/.local/bin" >&2
            ;;
    esac
    just link {{dir}}
    quiero_gui={{gui}}
    if [ "$quiero_gui" = "auto" ]; then
        if command -v npm >/dev/null && pkg-config --exists webkit2gtk-4.1 2>/dev/null; then
            quiero_gui=yes
        else
            quiero_gui=no
            echo "aviso: sin npm o sin WebKitGTK 4.1; me salto ntc-gui ('just gui-deps' y 'just link-all {{dir}} yes' cuando los tengas)" >&2
        fi
    fi
    if [ "$quiero_gui" = "yes" ]; then
        if [ ! -d {{gui_dir}}/ui/node_modules ]; then
            just gui-deps
        fi
        if ! just link-gui {{dir}}; then
            echo "aviso: la ventana no se pudo enlazar; ntc y norte sí están" >&2
        fi
    fi
    echo
    echo "en el PATH ahora:"
    for b in ntc norte ntc-gui; do
        if [ -L ~/.local/bin/$b ]; then
            printf '  %-8s → %s\n' "$b" "$(readlink ~/.local/bin/$b)"
        fi
    done

# Quita del PATH los enlaces que puso `just link-all`. No toca lo que instaló
# `cargo install` (para eso está `just uninstall`) ni borra nada del árbol:
# sólo desenlaza, y sólo si el enlace apunta a ESTE árbol — así una sesión en
# un worktree no se lleva por delante los enlaces de otro.
unlink:
    #!/usr/bin/env bash
    set -euo pipefail
    for b in ntc norte ntc-gui norte-gui; do
        dest=$(readlink ~/.local/bin/$b 2>/dev/null || true)
        case "$dest" in
            "$PWD"/*) rm -f ~/.local/bin/$b; echo "quitado: $b" ;;
            "") ;;
            *) echo "intacto: $b (apunta a $dest, otro árbol)" ;;
        esac
    done

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
#
# La ventana gráfica NO entra aquí: tiene su propia receta (`just link-gui`),
# por el mismo motivo por el que `core_pkgs` la excluye del gate — compilarla
# arrastra WebKitGTK, GTK3, libsoup3 y npm, y meterla en esta receta dejaría
# sin `ntc` a cualquier máquina que no los tenga.
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

# Como `just link`, pero `ntc`/`norte` RECOMPILAN antes de arrancar.
#
# El symlink de `just link` apunta al binario que produjo la última
# compilación, no al código que hay ahora: si editas y ejecutas sin haber
# corrido los tests, estás usando lo de antes sin que nada te avise. Esto lo
# cierra poniendo un envoltorio en el PATH que compila y luego ejecuta.
#
# Tres decisiones dentro del envoltorio, y las tres importan:
#
# - Compila con las MISMAS `features` que el gate. Sin ellas cargo keya los
#   artefactos por otro conjunto y fabrica un universo COMPLETO y separado de
#   norte-tui y de todo lo que cuelga (~30 G) que ninguna otra receta reusa.
#   Es la trampa del presupuesto de disco, y a mano es facilísimo pisarla.
# - Si la compilación FALLA, arranca el binario anterior con un aviso en vez de
#   dejarte sin gestor de ficheros. Un árbol a medio editar no debe costarte la
#   herramienta.
# - Si otra sesión está compilando, cargo espera al lock de `target/`. El
#   envoltorio lo DICE antes de bloquearse, porque un arranque mudo de diez
#   segundos parece colgado.
#
# `just link` sigue ahí para cuando quieras coste cero de arranque.
link-fresh:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p ~/.local/bin
    for b in ntc norte; do
        {
            echo '#!/usr/bin/env bash'
            echo "# Generado por 'just link-fresh' en $PWD. No editar a mano."
            echo 'set -uo pipefail'
            echo "tree=\"$PWD\""
            echo "bin=\"\$tree/target/debug/$b\""
            echo 'if [ -e "$tree/target/.cargo-lock" ]; then'
            echo "  printf 'norte: otra compilación tiene el lock de target/, esperando…\\n' >&2"
            echo 'fi'
            echo "if ! cargo build --quiet --manifest-path \"\$tree/Cargo.toml\" -p norte-tui -p norte-cli {{features}}; then"
            echo '  if [ -x "$bin" ]; then'
            echo "    printf 'norte: el árbol no compila; arranco la última build buena\\n' >&2"
            echo '  else'
            echo "    printf 'norte: el árbol no compila y no hay build previa\\n' >&2"
            echo '    exit 1'
            echo '  fi'
            echo 'fi'
            echo 'exec "$bin" "$@"'
        } > ~/.local/bin/$b
        chmod +x ~/.local/bin/$b
        printf '%-6s → envoltorio que recompila antes de arrancar\n' "$b"
    done

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
    # El de la ADR 0002 / #12: el suelo del sistema contra el camino del
    # provider. Es la vara que caduca la decisión de no meter `tokio-uring`.
    cargo bench -p norte-vfs-local --bench local_io

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


# Construye e INSTALA el previewer de syntect: el primer plugin real que se
# puede tener instalado, en vez de existir solo como fixture de un test.
#
# Se monta en `target/plugin-stage/` y se instala desde ahí: el `plugin.wasm`
# es un artefacto de build y no tiene por qué aparecer junto al `plugin.toml`
# en el árbol de fuentes.
#
# Instalar NO aprueba: el plugin queda descubierto y sin consentir, y se
# aprueba y activa en el gestor de extensiones (F12 en la TUI).
#
# `just plugin-syntect force` reemplaza uno ya instalado (retira su
# consentimiento). La palabra y no `--force`: `just` toma cualquier argumento
# que empiece por `-` como una receta más, y no hay `--` que lo evite.
plugin-syntect *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=crates/norte-plugin-host/examples-wasm/previewer-syntect
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/previewer-syntect
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    cp $origen/target/wasm32-wasip2/release/previewer_syntect.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# Construye e INSTALA el plugin oficial de columnas de git (`plugins/git-status`,
# ADR 0057). Mismo montaje que `plugin-syntect`: stage en `target/plugin-stage/`
# y `norte plugin install` desde ahí. Instalar NO aprueba. `force` reemplaza.
plugin-git-status *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/git-status
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/git-status
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/git_status.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# El decorator de iconos por tipo de fichero (`plugins/file-icons`, demo D1).
plugin-file-icons *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/file-icons
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/file-icons
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/file_icons.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# Las columnas de dimensiones y duración (`plugins/media-info`, demo D2).
plugin-media-info *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/media-info
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/media-info
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/media_info.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# El previewer de Markdown (`plugins/markdown`, demo D3).
plugin-markdown *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/markdown
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/markdown
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/markdown_preview.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# El previewer de imágenes (`plugins/image-ansi`, demo D4).
plugin-image-ansi *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/image-ansi
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/image-ansi
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/image_ansi_preview.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# El renamer por fecha (`plugins/date-prefix`, demo C3).
plugin-date-prefix *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/date-prefix
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/date-prefix
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/date_prefix.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# Todos los plugins oficiales, de una vez. `just plugins force` reemplaza los
# ya instalados (y retira su consentimiento, como dice `plugin install`).
[positional-arguments]
plugins *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    just plugin-syntect "$@"
    just plugin-git-status "$@"
    just plugin-file-icons "$@"
    just plugin-media-info "$@"
    just plugin-markdown "$@"
    just plugin-image-ansi "$@"
    just plugin-date-prefix "$@"

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

# ---------------------------------------------------------------------------
# La ventana: el renderer de Tauri (ADR 0087).
#
# Fuera del gate portable a propósito (ver `core_pkgs`): compilarlo exige
# WebKitGTK, GTK3 y libsoup3 del sistema. Su gate es este, y lo corre
# `.github/workflows/gui.yml`; a mano, `just gui-ci`.
# ---------------------------------------------------------------------------

gui_dir := "crates/norte-gui-tauri"

# Las features del gate de la ventana. Existe por la misma razón que `features`
# de arriba: `cargo -p norte-gui-tauri` a secas resuelve un conjunto DISTINTO
# del de `core_pkgs` para los crates compartidos (norte-core y norte-testkit
# entran por dev-dependencies), así que se compilaban y se quedaban en disco
# DOS veces. No puede tomar `{{features}}`: ese conjunto nombra
# `norte-tui/schema`, que no está en este grafo.
gui_features := "--features norte-core/testing"

# Las dependencias de JS, desde el lockfile y sin tocarlo (`npm ci`).
gui-deps:
    cd {{gui_dir}}/ui && npm ci

# El bundle de la webview: typecheck + Vite. Assets locales, nada remoto.
gui-build: 
    cd {{gui_dir}}/ui && npm run build

# Los tests del renderer (vitest, jsdom): ni ventana ni WebKitGTK.
gui-test-ui:
    cd {{gui_dir}}/ui && npm run test

# Formato y lint del renderer.
gui-lint-ui:
    cd {{gui_dir}}/ui && npm run fmt:check && npm run lint && npm run typecheck

# El gate de la ventana, entero. `gui-build` va ANTES de los tests de Rust porque
# uno de ellos audita el bundle empaquetado (`el_bundle_no_llama_a_casa`).
gui-ci: gui-lint-ui gui-test-ui gui-build
    CARGO_INCREMENTAL=0 cargo clippy -p norte-gui-tauri --all-targets {{gui_features}} -- -D warnings
    CARGO_INCREMENTAL=0 cargo nextest run -p norte-gui-tauri {{gui_features}} --no-tests=pass
    CARGO_INCREMENTAL=0 cargo test -p norte-gui-tauri {{gui_features}} --doc

# Arranca el renderer contra el daemon. Necesita un daemon vivo.
gui-run *args: gui-build
    cargo run -p norte-gui-tauri --bin norte-gui -- {{args}}

# Lo mismo en release: es lo ÚNICO que vale para medir (la 3.6).
gui-run-release *args: gui-build
    cargo run --release -p norte-gui-tauri --bin norte-gui -- {{args}}

# Pone `ntc-gui` (y su alias histórico `norte-gui`) en el PATH apuntando al
# binario de ESTE árbol, igual que `just link` hace con `ntc` y `norte`.
#
# Dos nombres para un solo binario a propósito: `ntc-gui` es el que se teclea,
# y hace pareja con `ntc`; `norte-gui` es como se llama el ejecutable dentro
# del crate y como lo nombran los paquetes, así que quitarlo rompería los
# scripts que ya lo usan.
#
# Depende de `gui-build` y no es opcional: `frontendDist` es `ui/dist`, o sea
# que Tauri EMBEBE la webview en el binario al compilar. Sin reconstruir el
# bundle, el enlace apuntaría a un binario con una webview vieja dentro — y
# eso no se ve, porque el ejecutable existe y arranca.
#
# Que esté embebida es también lo que hace que el symlink funcione: el binario
# es autocontenido y no busca `ui/dist` en el cwd.
#
# Monta también los `externalBin` (`binaries/norte-<triple>`, `ntc-<triple>`)
# y eso NO es cosa del empaquetado: el build script de Tauri los exige para
# CUALQUIER compilación del crate, así que en un árbol limpio esta receta
# moría con «resource path `binaries/norte-x86_64-…` doesn't exist» y sólo
# funcionaba si alguien había corrido `just gui-package` antes.
#
# Se COPIAN, no se enlazan: `just gui-package` hace `cp` encima con los
# binarios de release, y un `cp` sobre un symlink escribe A TRAVÉS de él —
# o sea que un enlace aquí dejaría el binario de release dentro de
# `target/debug/`, sin que nada lo dijera.
#
# Separada de `just link` a propósito: ver el comentario de aquella receta.
link-gui dir="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    just gui-build
    perfil=()
    if [ "{{dir}}" = "release" ]; then perfil=(--release); fi
    triple=$(rustc -vV | sed -n 's/^host: //p')
    cargo build "${perfil[@]}" -p norte-cli -p norte-tui {{features}}
    mkdir -p {{gui_dir}}/binaries
    for b in norte ntc; do
        rm -f "{{gui_dir}}/binaries/$b-$triple"
        cp "target/{{dir}}/$b" "{{gui_dir}}/binaries/$b-$triple"
    done
    cargo build "${perfil[@]}" -p norte-gui-tauri --bin norte-gui {{gui_features}}
    mkdir -p ~/.local/bin
    for b in ntc-gui norte-gui; do
        ln -sfn "$PWD/target/{{dir}}/norte-gui" ~/.local/bin/$b
        printf '%-9s → %s\n' "$b" "$(readlink ~/.local/bin/$b)"
    done
    echo "recuerda: el symlink apunta a ESTE árbol; un 'just prune-all' lo deja colgando"

# La build de PRODUCCIÓN, sin empaquetar. Necesita la CLI de Tauri del
# lockfile.
#
# Se ejecuta desde el directorio del CRATE y no desde `ui/` (#256): la CLI
# busca `tauri.conf.json` en el directorio actual y sus subdirectorios, y el
# fichero vive aquí, no bajo `ui/`. Corriéndola desde `ui/` aborta con
# «Couldn't recognize the current folder as a Tauri project» — que es lo que
# hacía esta receta desde que se escribió, y por qué nunca produjo nada.
#
# Se invoca el binario del lockfile por su ruta en vez de con `npx`: `npx`
# resuelve contra el directorio desde el que se llama, y desde el crate no hay
# `node_modules`.
gui-build-release: gui-build
    cd {{gui_dir}} && ./ui/node_modules/.bin/tauri build --no-bundle

# El paquete de verdad: `.deb` y AppImage en `target/release/bundle/`.
#
# **`NO_STRIP=1` no es opcional en un sistema moderno** (#256). El AppImage de
# `linuxdeploy` trae su propio `strip`, de un binutils viejo que no reconoce la
# sección `.relr.dyn` que usan las bibliotecas de una distribución al día. Sin
# la variable, falla con `failed to run linuxdeploy` después de un muro de
# «Unable to recognise the format of the input file» — que no dice en ningún
# sitio que el problema sea el strip.
#
# La salida correcta a medio plazo es construir sobre la baseline más VIEJA de
# glibc/WebKitGTK, que es lo que la tarea 7.1 del plan pide de todas formas;
# esto es lo que hace que el paquete salga hoy, en la máquina de referencia.
# Y el paquete lleva los TRES binarios (#256): `norte-gui`, el daemon `norte`
# y el TUI `ntc`. Un paquete con solo la ventana no arranca en una instalación
# limpia — desde #300 la ventana levanta su daemon, y para eso tiene que
# haberlo. Van como `externalBin`, que es como Tauri mete un ejecutable de
# al lado: en el `.deb` acaban en `/usr/bin`, que es donde la ventana los
# busca (junto a su propio ejecutable, y si no en el `PATH`).
#
# Tauri exige que el fichero fuente lleve el TRIPLE del target en el nombre y
# lo quita al empaquetar, así que se copian con ese sufijo a `binaries/`.
gui-package: gui-build
    #!/usr/bin/env bash
    set -euo pipefail
    triple=$(rustc -vV | sed -n 's/^host: //p')
    cargo build --release -p norte-cli -p norte-tui {{features}}
    mkdir -p {{gui_dir}}/binaries
    for b in norte ntc; do
        rm -f "{{gui_dir}}/binaries/$b-$triple"
        cp "target/release/$b" "{{gui_dir}}/binaries/$b-$triple"
    done
    cd {{gui_dir}} && NO_STRIP=1 ./ui/node_modules/.bin/tauri build

# Instala el PAQUETE en un contenedor limpio y comprueba que ahí dentro
# funciona: los tres binarios, el listado inicial, y la ventana arrancando bajo
# Xvfb sin morirse.
#
# El hermano de `dist-smoke` para la ventana. Aquél desempaqueta los tarballs
# portables; éste hace lo que ninguno hacía: instalar de verdad en una
# distribución que no ha visto este árbol. `empaquetado.rs` comprueba lo que el
# paquete PROMETE (lee `tauri.conf.json`); esto, lo que HACE.
#
# Necesita Docker y un `just gui-package` previo. No necesita CI — que es el
# punto: el fallo de instalación limpia no depende de quién apriete el botón.
#
# Instala el paquete en un contenedor limpio y lo arranca.
gui-smoke imagen="debian:trixie":
    ./scripts/gui-smoke.sh {{imagen}}
