#!/bin/sh
# Compile the tailnet core for the targets CI ships on, from this Mac:
#   macOS arm64 + x86_64, iOS arm64, Android arm64 + armv7 (NDK clang): the engine crate
#     (engine/crates/ps5upload-tailnet: cc-rs builds the C exactly as CI does)
#   Linux arm64 (glibc): the same crate, built in the rust Docker image
#   Windows x86_64: the C core with MinGW-w64 against real Windows headers, then linked
#     (make -C tailnet win); CI builds it with MSVC through cc-rs
#   PS5: make -C tailnet ps5-link (needs PS5_PAYLOAD_SDK)
# Usage: cross-check.sh [CRATE=ps5upload-tailnet]   Exit status = number of failed targets.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
CRATE=${1:-ps5upload-tailnet}
NDK=${ANDROID_NDK_HOME:-$(ls -d "$HOME"/Library/Android/sdk/ndk/* 2>/dev/null | sort -V | tail -1)}
NB="$NDK/toolchains/llvm/prebuilt/darwin-x86_64/bin"
LOCK=${BUILD_LOCK:-${TMPDIR:-/tmp}/ps5upload-build.lock}
fails=0
run() { # label, command...
	label=$1
	shift
	if out=$("$@" 2>&1); then echo "ok   $label"; else echo "FAIL $label"; echo "$out" | grep -E "error" | head -8; fails=$((fails + 1)); fi
}
locked() { if command -v lockf >/dev/null 2>&1 && [ -n "$LOCK" ]; then lockf "$LOCK" "$@"; else "$@"; fi; }

cd "$REPO/engine" || exit 1
export CC_aarch64_linux_android="$NB/aarch64-linux-android24-clang" AR_aarch64_linux_android="$NB/llvm-ar"
export CC_armv7_linux_androideabi="$NB/armv7a-linux-androideabi24-clang" AR_armv7_linux_androideabi="$NB/llvm-ar"
for t in aarch64-apple-darwin x86_64-apple-darwin aarch64-apple-ios aarch64-linux-android armv7-linux-androideabi; do
	run "$CRATE $t" locked cargo build -q -p "$CRATE" --target "$t"
done
if command -v docker >/dev/null 2>&1; then
	export DOCKER_CONFIG="$HERE/.state/docker"
	run "$CRATE aarch64-unknown-linux-gnu (docker rust:1)" docker run --rm -v "$REPO:/src" -w /src/engine \
		-e CARGO_TARGET_DIR=/src/engine/target/docker-linux docker.io/library/rust:1 \
		cargo build -q -p "$CRATE"
	run "$CRATE aarch64-unknown-linux-musl (docker rust:alpine, the web UI image)" docker run --rm -v "$REPO:/src" \
		-w /src/engine -e CARGO_TARGET_DIR=/src/engine/target/docker-musl docker.io/library/rust:1.99.1-alpine3.24 \
		sh -c 'apk add --no-cache musl-dev >/dev/null 2>&1; cargo build -q -p '"$CRATE"
fi
if command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
	run "tailnet core x86_64-windows (MinGW headers, -Werror)" make -s -C "$REPO/tailnet" win
	printf '#include "tailnet.h"\nint main(void){tn_config c={0};c.state_dir=".";c.control_url="http://127.0.0.1:1";tn_ctx*x;if(tn_create(&c,&x))return 1;tn_start(x);unsigned short p;tn_map(x,6,0x64400001,9120,0,&p);tn_status s;tn_get_status(x,&s,0,0);tn_destroy(x);return 0;}\n' >"$REPO/tailnet/build/win/link.c"
	run "tailnet core x86_64-windows link (ws2_32, iphlpapi, bcrypt)" x86_64-w64-mingw32-gcc -std=c11 -O2 \
		-I"$REPO/tailnet/include" -o "$REPO/tailnet/build/win/link.exe" "$REPO/tailnet/build/win/link.c" \
		"$REPO/tailnet/build/win/libtailnet.a" "$REPO/tailnet/build/win/liblwip.a" "$REPO/tailnet/build/win/libbearssl.a" \
		-lws2_32 -liphlpapi -lbcrypt
fi
if [ -n "${PS5_PAYLOAD_SDK:-}" ]; then
	run "tailnet core PS5 (ps5-link)" locked make -s -C "$REPO/tailnet" ps5-link
fi
echo "cross-check: $fails failed"
exit $fails
