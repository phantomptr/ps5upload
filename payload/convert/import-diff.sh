#!/usr/bin/env bash
# Lists what ps5upload-convert.elf imports that the helper's ELF does not.
#
#   payload/convert/import-diff.sh [helper.elf]     (default: payload/ps5upload.elf)
#
# build.sh already checks every import against the SDK's stubs, but stubs can declare a function
# a firmware does not have. The helper runs on every console we support, so an import it shares is
# proven; the ones printed here are proven only by the converter's first run on a console
# (HARDWARE.md says which run exercises which).
#
# The ELFs go to the toolchain image's llvm-readelf on stdin: no local LLVM is needed.
set -euo pipefail
cd "$(dirname "$0")/../.."
helper=${1:-payload/ps5upload.elf}
convert=payload/convert/ps5upload-convert.elf
image=ps5upload-convert-toolchain
for f in "$helper" "$convert"; do
  [[ -f $f ]] || { echo "import-diff.sh: $f is missing (build it first)" >&2; exit 1; }
done
imports() {
  docker run --rm -i "$image" sh -c 'cat > /tmp/e && llvm-readelf --wide --dyn-syms /tmp/e 2>/dev/null' < "$1" |
    awk '$7 == "UND" && $8 != "" { sub(/@.*/, "", $8); print $8 }' | LC_ALL=C sort -u
}
ours=$(imports "$convert")
theirs=$(imports "$helper")
only=$(LC_ALL=C comm -23 <(echo "$ours") <(echo "$theirs"))
echo "converter imports: $(wc -l <<< "$ours" | xargs); helper imports: $(wc -l <<< "$theirs" | xargs)"
echo "imported by the converter and not by the helper ($(grep -c . <<< "$only" || true)):"
echo "$only"
