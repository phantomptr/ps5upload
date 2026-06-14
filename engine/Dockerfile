# Minimal scratch image for ps5upload-engine. Build context is engine/.
#   docker build -t ps5upload-engine engine/
#   docker run --rm -p 19113:19113 -e PS5_ADDR=192.168.1.50:9113 ps5upload-engine
# Env: PS5UPLOAD_ENGINE_PORT (19113), PS5_ADDR (192.168.137.2:9113),
#      PS5UPLOAD_ALLOW_IP (comma-separated IPs allowed past the loopback guard).

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
