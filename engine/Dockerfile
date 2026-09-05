# Minimal scratch image for ps5upload-engine. Build context is engine/.
# Official multi-arch image: docker pull ghcr.io/phantomptr/ps5upload-engine:latest
#   docker build -t ps5upload-engine engine/
#   docker run --rm -p 19113:19113 -e PS5_ADDR=192.168.1.50:9113 ps5upload-engine
# Env: PS5UPLOAD_ENGINE_PORT (19113), PS5_ADDR (192.168.137.2:9113),
#      PS5UPLOAD_ALLOW_IP (comma-separated IPs allowed past the loopback guard),
#      PS5UPLOAD_PAYLOAD_DIR (directory holding ps5upload.elf +
#      ezremote-dpi.elf, overriding the images built into this binary —
#      only needed if you built the image without them).
#
# IMPORTANT for Docker Desktop (macOS/Windows): the engine's loopback guard
# rejects requests that don't come from 127.0.0.1/::1. Docker's port-forwarding
# NATs connections through the VM bridge, so the container sees a bridge IP
# (e.g. 172.17.0.1), NOT loopback — and all requests get 403 "loopback only".
# Two workarounds:
#   1. Set PS5UPLOAD_ALLOW_IP to the Docker bridge gateway IP:
#        docker run --rm -p 19113:19113 \
#          -e PS5_ADDR=192.168.1.50:9113 \
#          -e PS5UPLOAD_ALLOW_IP=172.17.0.1 \
#          ps5upload-engine
#   2. On Linux hosts, use --network host (connections are true loopback):
#        docker run --rm --network host \
#          -e PS5_ADDR=192.168.1.50:9113 \
#          ps5upload-engine
# Note: on macOS/Windows Docker Desktop, --network host does NOT work (the
# daemon runs in a VM and can't bind to the host's LAN). Use option 1 and
# note that the PS5 must be reachable from the Docker VM's network (may
# require additional routing/bridging for the VM to reach the LAN).
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

# Built from the REPO ROOT, not engine/, because the workspace patches
# `unrar` to the vendored copy at third_party/unrar (see that directory's
# README). A build context of engine/ puts that path outside the context
# and cargo fails with "unable to update /third_party/unrar".
WORKDIR /build
COPY engine engine
COPY third_party third_party
# The PS5 helper ELFs the engine embeds (build.rs looks for them at
# <repo root>/payload). Without this the image builds fine but installs
# UPDATES and DLC with no installer to send — the exact #295 failure —
# because `have_bundled_dpi` is never set. ezremote-dpi.elf is committed,
# so this always works; ps5upload.elf appears only if you ran
# `make payload` first (it needs the PS5 payload SDK).
COPY payload payload
WORKDIR /build/engine
RUN cargo build --release -p ps5upload-engine

FROM scratch
COPY --from=builder /build/engine/target/release/ps5upload-engine /ps5upload-engine
EXPOSE 19113
# Run as an unprivileged user (65532 = nobody in distroless convention).
# scratch has no /etc/passwd, so we use a raw numeric UID — the kernel
# doesn't need a passwd entry to run a process. Port 19113 > 1024 so
# no CAP_NET_BIND_SERVICE needed.
USER 65532:65532
# Health: GET /api/jobs returns 200 once the HTTP server is live (no PS5
# contact needed). scratch has no shell/wget, so no in-image HEALTHCHECK;
# use an external probe (Docker Compose healthcheck with wget sidecar,
# Kubernetes liveness/readiness probe hitting /api/jobs).
ENTRYPOINT ["/ps5upload-engine"]
