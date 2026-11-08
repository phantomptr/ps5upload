#!/bin/sh
# Regenerate tailnet/tests/vectors_go.h from Tailscale's Go code.
# TAILSCALE_SRC: a clone of github.com/tailscale/tailscale (default: ~/workspace/github.com/tailscale/tailscale).
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
TS=${TAILSCALE_SRC:-$HOME/workspace/github.com/tailscale/tailscale}
WORK=$(mktemp -d "${TMPDIR:-/tmp}/tnvectors.XXXXXX")
cp "$HERE/main.go" "$WORK/"
cd "$WORK"
cat >go.mod <<MOD
module tnvectors

go 1.27

require tailscale.com v0.0.0
replace tailscale.com => $TS
MOD
# Use the same dependency versions as the Tailscale tree.
cp "$TS/go.sum" go.sum
go mod tidy >/dev/null 2>&1 || go mod tidy
go run . >"$HERE/../../tests/vectors_go.h"
echo "wrote tailnet/tests/vectors_go.h ($(wc -l <"$HERE/../../tests/vectors_go.h") lines); scratch dir $WORK"
