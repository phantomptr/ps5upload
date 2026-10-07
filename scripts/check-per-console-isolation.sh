#!/bin/sh
# Nothing from one console may be shown against another.
#
# Individual screens can guard their in-flight calls with
# useStaleHostGuard, but most do not and every new screen starts out not
# using it. The mechanism that actually closes the hole is in App.tsx:
# each selected console has its own tree of screens. The trees are told
# apart by a key derived from the console, only the selected one is on
# show, and a hidden one reads the connection state frozen as it was for
# its own console (ConnectionScope) and keeps its own address. So no
# cached list or scan result is shown under another console, and a reply
# that arrives late lands on the tree of the console that asked.
#
# Those are a few lines. Deleting any of them would silently reopen
# cross-console bleed with nothing failing anywhere. Hence this check.
set -eu

APP="${1:-client/src/App.tsx}"
STORE="${2:-client/src/state/connection.ts}"

for f in "$APP" "$STORE"; do
    if [ ! -f "$f" ]; then
        echo "ERROR: $f not found" >&2
        exit 1
    fi
done

need() { # need <file> <fixed string> <what it guarantees>
    if ! grep -qF -- "$2" "$1"; then
        echo "ERROR: $1 no longer has: $2" >&2
        echo "  $3" >&2
        echo "  Without it, one console's data can be shown under another console's name." >&2
        exit 1
    fi
}

need "$APP" 'const here = host || NO_CONSOLE;' \
    "Each tree of screens is keyed on the selected console, with a fallback for 'no console selected'."
need "$APP" '<Activity key={key} mode={active ? "visible" : "hidden"}>' \
    "One tree per console, and only the selected console's tree is on show."
need "$APP" '<ConnectionScope host={treeHost} frozen={!active}>' \
    "A hidden console's tree reads its own console's connection state, not the selected one's."
need "$APP" 'if (!active && !connectionSnapshotFor(treeHost)) return null;' \
    "A hidden tree with nothing remembered for its console is dropped rather than shown live."
need "$APP" '<AppRoutes location={active ? location : last} />' \
    "A hidden console's tree stays on the address it was last on."
need "$STORE" 'return frozen ? pick(frozen) : live;' \
    "useConnectionStore answers from the frozen state inside a hidden console's tree."

printf '  %-24s one tree of screens per console, hidden ones frozen on their own console\n' "$(basename "$APP")"
