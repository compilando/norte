# The build image for everything that gets published (ADR 0112). Ubuntu
# 22.04 is the oldest base with WebKitGTK 4.1, and its glibc (2.35) is the
# floor. The digest is pinned: the same image today and a year from now.
FROM ubuntu@sha256:829f6df217bcbae2b371026e81711d1a787c61b2967ad09d015063663ebafbf7

ARG NODE_VERSION=22.23.2
ENV DEBIAN_FRONTEND=noninteractive

# `.github/workflows/gui.yml` plus what the host takes for granted: `lld`
# because `.cargo/config.toml` forces `-fuse-ld=lld`, `clang`/`cmake` for
# aws-lc-sys, `file`/`xz-utils` for the bundlers, `binutils` for `objdump`.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential clang cmake lld pkg-config curl ca-certificates git file \
      xz-utils binutils patchelf \
      libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
      libjavascriptcoregtk-4.1-dev librsvg2-dev \
 && rm -rf /var/lib/apt/lists/*

# Exact Node, with the checksum nodejs.org publishes.
RUN set -eu; t="node-v${NODE_VERSION}-linux-x64.tar.xz"; \
    base="https://nodejs.org/dist/v${NODE_VERSION}"; cd /tmp; \
    curl -fsSLO "$base/$t"; \
    curl -fsSL "$base/SHASUMS256.txt" | grep " $t\$" | sha256sum -c -; \
    mkdir -p /opt/node; tar -xJf "$t" -C /opt/node --strip-components=1; rm "$t"

ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/opt/node/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

# The toolchain and cargo-dist come from the repo's files, not from numbers
# written here: changing either one changes the image's tag.
COPY rust-toolchain.toml dist-workspace.toml /opt/norte/
# `rustup toolchain install` with no name installs what the file says, but
# does not make it the default: without `rustup default`, `rustc` only works
# inside a directory with `rust-toolchain.toml`, and anything else in the
# image fails with "no default is configured".
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal --default-toolchain none \
 && cd /opt/norte && rustup toolchain install \
 && rustup default "$(grep -E '^channel' rust-toolchain.toml | cut -d'"' -f2)" \
 && cd / && rustc --version
RUN v="$(grep -E '^cargo-dist-version' /opt/norte/dist-workspace.toml | cut -d'"' -f2)" \
 && cd /opt/norte && cargo install cargo-dist --version "$v" --locked \
 && rm -rf /opt/cargo/registry /opt/cargo/git

# The repo arrives via a volume owned by someone else.
RUN git config --system --add safe.directory '*'
