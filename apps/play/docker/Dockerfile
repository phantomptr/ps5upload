# PS5Upload Play - Linux build image (also used on macOS hosts through Docker).
# SPDX-License-Identifier: GPL-3.0-or-later
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        clang-18 lld-18 llvm-18 make python3 python3-numpy python3-pil python3-fonttools \
        zlib1g-dev unzip zip wget ca-certificates binutils git librsvg2-bin \
    && rm -rf /var/lib/apt/lists/*
ENV PATH=/usr/lib/llvm-18/bin:$PATH
WORKDIR /work
