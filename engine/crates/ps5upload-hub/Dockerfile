# The hub as a small container (Fly.io, or anywhere that runs images). Build context: this crate
# directory, so `fly deploy` works from here. The engine workspace is not in the context, so the
# build uses a one-crate workspace (deploy/workspace.toml) and a lock pruned from the engine's
# (deploy/Cargo.lock, refreshed by deploy/refresh-lock.sh).
#
#   docker build --platform linux/amd64 -t ps5upload-hub .

FROM rust:1.99.1-alpine3.24 AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY deploy/workspace.toml Cargo.toml
COPY deploy/Cargo.lock Cargo.lock
COPY Cargo.toml ps5upload-hub/Cargo.toml
COPY src ps5upload-hub/src
# Alpine's Rust targets musl and links statically by default: one self-contained binary.
RUN cargo build --release --locked -p ps5upload-hub \
 && cp target/release/ps5upload-hub /ps5upload-hub

# No shell, no package manager, runs as an unprivileged user.
FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /ps5upload-hub /usr/local/bin/ps5upload-hub
USER nonroot
ENV HUB_HTTP_LISTEN=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/ps5upload-hub"]
