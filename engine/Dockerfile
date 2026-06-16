# Minimal scratch image for ps5upload-engine. Build context is engine/.
# Official multi-arch image: docker pull ghcr.io/phantomptr/ps5upload-engine:latest
#   docker build -t ps5upload-engine engine/
#   docker run --rm -p 19113:19113 -e PS5_ADDR=192.168.1.50:9113 ps5upload-engine
# Env: PS5UPLOAD_ENGINE_PORT (19113), PS5_ADDR (192.168.137.2:9113),
#      PS5UPLOAD_ALLOW_IP (comma-separated IPs allowed past the loopback guard).
#
# SECURITY: the /api/* surface is UNAUTHENTICATED — it can install/uninstall
# titles and read/write/delete files on the PS5. By default only loopback may
# reach it. PS5UPLOAD_ALLOW_IP opens it to the listed IP(s) so a remote desktop
# client can drive a self-hosted engine. An IP allowlist is spoofable and grants
# any process on those hosts full control, so set it ONLY on a trusted LAN and
# NEVER publish this container to an untrusted network or the internet. Bind it
# to the LAN interface (e.g. `-p 192.168.1.10:19113:19113`), not 0.0.0.0 on a
# public host.

FROM rust:alpine AS builder

# g++ compiles unrar's bundled C++ against musl; musl target links static.
RUN apk add --no-cache musl-dev g++ make

WORKDIR /build
COPY . .
RUN cargo build --release -p ps5upload-engine

FROM scratch
COPY --from=builder /build/target/release/ps5upload-engine /ps5upload-engine
EXPOSE 19113
ENTRYPOINT ["/ps5upload-engine"]
