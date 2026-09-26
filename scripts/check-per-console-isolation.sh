#!/bin/sh
# Nothing from one console may be shown against another.
#
# Individual screens can guard their in-flight calls with
# useStaleHostGuard, but most do not and every new screen starts out not
# using it. The mechanism that actually closes the hole is keying the
# route tree on the selected console: switching remounts every screen,
# so no cached list or scan result survives, and a reply that arrives
# late lands on an unmounted component and is dropped.
#
# That is one line in App.tsx. Deleting it would silently reopen
# cross-console bleed with nothing failing anywhere. Hence this check.
set -eu

APP="${1:-client/src/App.tsx}"

if [ ! -f "$APP" ]; then
    echo "ERROR: $APP not found" >&2
    exit 1
fi

key_line=$(grep -n '<Routes' "$APP" | head -1 || true)
if [ -z "$key_line" ]; then
    echo "ERROR: no <Routes> element found in $APP" >&2
    exit 1
fi

case "$key_line" in
*'key={'*) ;;
*)
    echo "ERROR: <Routes> in $APP has no key." >&2
    echo "  Screens are keyed on the selected console so that switching" >&2
    echo "  consoles remounts them. Without it, one console's data can be" >&2
    echo "  shown under another console's name." >&2
    exit 1
    ;;
esac

case "$key_line" in
*host*) ;;
*)
    echo "ERROR: the <Routes> key in $APP is not derived from the host." >&2
    echo "  Line: $key_line" >&2
    exit 1
    ;;
esac

# A bare `key={host}` remounts on every render once host is empty-ish;
# the fallback keeps it stable when no console is selected.
case "$key_line" in
*'||'*|*'??'*) ;;
*)
    echo "ERROR: the <Routes> key has no fallback for 'no console selected'." >&2
    echo "  Line: $key_line" >&2
    exit 1
    ;;
esac

printf '  %-24s route tree is keyed on the selected console\n' "$(basename "$APP")"
