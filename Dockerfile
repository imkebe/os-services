# The Linux build. It lints, tests and packages the service on Ubuntu 22.04,
# so that the AppImage runs on glibc 2.35 and newer, and builds FFmpeg with
# vcpkg. It exports dist/ from the `dist` stage.
# `cargo xtask docker` runs it; CI caches the `deps` stage.
# It builds for the architecture it runs on, amd64 or arm64:
#   docker build --platform linux/arm64 --target dist --output type=local,dest=dist .
# Dockerfile.armhf builds the 32-bit ARM AppImage.

# Git repositories to clone from instead of GitHub, which sometimes refuses
# anonymous clones: empty unless the build passes
# --build-context gitcache=DIR with DIR/vcpkg and DIR/libdrmtap (bare clones).
FROM scratch AS gitcache

FROM ubuntu:22.04 AS toolchain

ARG DEBIAN_FRONTEND=noninteractive
ENV PATH=/root/.cargo/bin:/usr/local/go/bin:${PATH}

# vcpkg builds FFmpeg; the KMS encoder library links the computer's libva,
# libdrm and EGL, so only their headers come from Ubuntu. meson, ninja, libcap and
# libseccomp build libdrmtap. The update check links OpenSSL, which cargo
# builds from source with perl and make; libclang reads its headers.
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    curl \
    file \
    git \
    libcap-dev \
    libclang-dev \
    libdrm-dev \
    libegl-dev \
    libgles-dev \
    libseccomp-dev \
    libva-dev \
    meson \
    nasm \
    ninja-build \
    pkg-config \
    python3 \
    tar \
    unzip \
    zip \
    && rm -rf /var/lib/apt/lists/*
ENV VCPKG_ROOT=/opt/vcpkg

# rust-toolchain.toml selects the Rust version when cargo first runs.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain none
RUN case "$(uname -m)" in \
        x86_64) go=go1.24.4.linux-amd64.tar.gz sum=77e5da33bb72aeaef1ba4418b6fe511bc4d041873cbf82e5aa6318740df98717 ;; \
        aarch64) go=go1.24.4.linux-arm64.tar.gz sum=d5501ee5aca0f258d5fe9bfaed401958445014495dc115f202d43d5210b45241 ;; \
        *) echo "no Go for $(uname -m)" >&2; exit 1 ;; \
    esac \
    && curl --proto '=https' --tlsv1.2 -sSfL "https://go.dev/dl/$go" -o /tmp/go.tar.gz \
    && echo "$sum  /tmp/go.tar.gz" | sha256sum --check --quiet \
    && tar -C /usr/local -xzf /tmp/go.tar.gz \
    && rm /tmp/go.tar.gz

WORKDIR /work

# The slow layers come first and copy only what they read, so a change to
# xtask, packaging or the sources does not rebuild them.
FROM toolchain AS deps

# FFmpeg depends only on the vcpkg manifest and ports. These are the vcpkg
# steps of `cargo xtask setup`, which later finds the libraries installed.
COPY vendor/vcpkg ./vendor/vcpkg
RUN --mount=type=bind,from=gitcache,target=/gitcache \
    baseline=$(sed -n 's/.*"builtin-baseline": *"\([0-9a-f]*\)".*/\1/p' vendor/vcpkg/vcpkg.json) \
    && test -n "$baseline" \
    && case "$(uname -m)" in x86_64) triplet=x64-linux ;; aarch64) triplet=arm64-linux ;; esac \
    && source=https://github.com/microsoft/vcpkg \
    && if [ -d /gitcache/vcpkg ]; then source=/gitcache/vcpkg; fi \
    && git clone --quiet "$source" "$VCPKG_ROOT" \
    && git -C "$VCPKG_ROOT" checkout --quiet --detach "$baseline" \
    && "$VCPKG_ROOT/bootstrap-vcpkg.sh" -disableMetrics \
    && "$VCPKG_ROOT/vcpkg" install --clean-after-build \
        --triplet "$triplet" --host-triplet "$triplet" \
        --x-manifest-root=/work/vendor/vcpkg --x-install-root="$VCPKG_ROOT/installed"

# The crates build from the manifests alone, with placeholder sources and
# the KMS capture that build.rs compiles. The placeholder's own artifacts are
# removed so cargo cannot take them for a build of the real sources.
COPY rust-toolchain.toml Cargo.toml Cargo.lock build.rs ./
COPY .cargo ./.cargo
COPY src/video/kms/native ./src/video/kms/native
COPY vendor/nusb ./vendor/nusb
COPY xtask/Cargo.toml ./xtask/
COPY xtask/src/vcpkg_layout.rs ./xtask/src/
RUN mkdir -p src xtask/src \
    && echo 'fn main() {}' > src/main.rs \
    && echo 'fn main() {}' > xtask/src/main.rs
RUN cargo build --release --locked --package ipkvm-os-services --bins --tests \
    && rm -rf target/release/.fingerprint/ipkvm-os-services-* \
        target/release/deps/ipkvm_os_services-* \
        target/release/ipkvm-os-services*

# The rest of `cargo xtask setup`: libdrmtap, whose header the build checks
# against the copy in src/video/kms/native.
COPY packaging ./packaging
RUN --mount=type=bind,from=gitcache,target=/gitcache \
    if [ -d /gitcache/libdrmtap ]; then export LIBDRMTAP_REPO=/gitcache/libdrmtap; fi \
    && packaging/libdrmtap/build.sh --stage packaging/linux/drmtap \
    && rm -rf src xtask/src

FROM deps AS build
# The version a dev release builds; see src/protocol/mod.rs.
ARG IPKVM_OS_SERVICES_VERSION
COPY . .
# Lint on x86_64, where CI lints. Elsewhere the Linux self-update code,
# which only the x86_64 build uses, is compiled but unused, and clippy's
# warnings are errors.
RUN if [ "$(uname -m)" = x86_64 ]; then cargo xtask lint; fi
RUN cargo xtask test
RUN cargo xtask package

FROM scratch AS dist
COPY --from=build /work/dist/ /
