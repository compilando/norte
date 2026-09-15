# La imagen de construcción de todo lo que se publica (ADR 0112). Ubuntu 22.04
# es la base más vieja con WebKitGTK 4.1, y su glibc (2.35) es el suelo.
# El digest está fijado: la misma imagen hoy y dentro de un año.
FROM ubuntu@sha256:829f6df217bcbae2b371026e81711d1a787c61b2967ad09d015063663ebafbf7

ARG NODE_VERSION=22.23.2
ENV DEBIAN_FRONTEND=noninteractive

# `.github/workflows/gui.yml` más lo que el host da por supuesto: `lld` porque
# `.cargo/config.toml` fuerza `-fuse-ld=lld`, `clang`/`cmake` para aws-lc-sys,
# `file`/`xz-utils` para los bundlers, `binutils` para `objdump`.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential clang cmake lld pkg-config curl ca-certificates git file \
      xz-utils binutils patchelf \
      libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
      libjavascriptcoregtk-4.1-dev librsvg2-dev \
 && rm -rf /var/lib/apt/lists/*

# Node exacto, con la suma que publica nodejs.org.
RUN set -eu; t="node-v${NODE_VERSION}-linux-x64.tar.xz"; \
    base="https://nodejs.org/dist/v${NODE_VERSION}"; cd /tmp; \
    curl -fsSLO "$base/$t"; \
    curl -fsSL "$base/SHASUMS256.txt" | grep " $t\$" | sha256sum -c -; \
    mkdir -p /opt/node; tar -xJf "$t" -C /opt/node --strip-components=1; rm "$t"

ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/opt/node/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

# Toolchain y cargo-dist salen de los ficheros del repo, no de números
# escritos aquí: cambiar cualquiera de los dos cambia el tag de la imagen.
COPY rust-toolchain.toml dist-workspace.toml /opt/norte/
# `rustup toolchain install` sin nombre instala lo que dice el fichero, pero no
# lo hace el de por defecto: sin `rustup default`, `rustc` solo funciona dentro
# de un directorio con `rust-toolchain.toml`, y cualquier otra cosa de la
# imagen falla con «no default is configured».
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal --default-toolchain none \
 && cd /opt/norte && rustup toolchain install \
 && rustup default "$(grep -E '^channel' rust-toolchain.toml | cut -d'"' -f2)" \
 && cd / && rustc --version
RUN v="$(grep -E '^cargo-dist-version' /opt/norte/dist-workspace.toml | cut -d'"' -f2)" \
 && cd /opt/norte && cargo install cargo-dist --version "$v" --locked \
 && rm -rf /opt/cargo/registry /opt/cargo/git

# El repo llega por un volumen de otro dueño.
RUN git config --system --add safe.directory '*'
