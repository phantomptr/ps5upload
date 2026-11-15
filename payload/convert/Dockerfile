# The toolchain that builds ps5upload-convert (engine/crates/ps5upload-convert) as a PS5 ELF:
# Rust with build-std + the ps5-payload-dev SDK. Built and run by payload/convert/build.sh.
# Ported from PS5 Dump Forge's ps5/Dockerfile (github.com/quer3q/ps5-dump-forge, GPL-3.0-or-later).
# Runs on arm64 or x86-64 hosts: everything here cross-compiles.
#
# Rust, the std patch (prepare-rust-std.py), the target JSON and the shims were verified together
# on a PS5 (shim/UPSTREAM); move them together. The Rust image is pinned by digest, as Forge pins
# it; Debian's clang-19 packages float within trixie (point releases only).
#
# The SDK is the repository's own pin, the helper's (scripts/ps5-sdk.env): build.sh passes its
# tag and SHA-256 as build arguments, so an SDK bump moves the helper and this program together.
# Forge pins v0.43; nothing in the shim set depends on the SDK release (the syscall numbers in
# shim/syscalls.S are the console's).
FROM rust:1.100.0-slim-trixie@sha256:e9d95b74296f7126a83ae3d041aeaba1ff4c478f13abc5cc034fcf7c131c3d4a
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates clang-19 lld-19 llvm-19 make curl unzip python3 \
    && rm -rf /var/lib/apt/lists/*
ENV PS5_PAYLOAD_SDK=/opt/ps5-payload-sdk LLVM_CONFIG=/usr/bin/llvm-config-19
ENV PATH=/usr/lib/llvm-19/bin:$PATH
# The image's own toolchain: build-std needs the rust-src patched below, whatever the host has.
ENV RUSTUP_TOOLCHAIN=1.100.0 RUSTUP_AUTO_INSTALL=0
ARG SDK_TAG
ARG SDK_SHA256
RUN test -n "$SDK_TAG" && test -n "$SDK_SHA256" \
    && curl -fsSL --proto =https --retry 3 \
       "https://github.com/ps5-payload-dev/sdk/releases/download/$SDK_TAG/ps5-payload-sdk.zip" -o /tmp/sdk.zip \
    && echo "$SDK_SHA256  /tmp/sdk.zip" | sha256sum -c - \
    && unzip -q /tmp/sdk.zip -d /opt && rm /tmp/sdk.zip \
    && test -x "$PS5_PAYLOAD_SDK/bin/prospero-clang"
RUN rustup component add rust-src
COPY prepare-rust-std.py /tmp/
RUN python3 /tmp/prepare-rust-std.py && rm /tmp/prepare-rust-std.py
WORKDIR /work
