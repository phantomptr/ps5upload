#!/bin/sh
# Self-test for scripts/release/linux-launcher.sh.
#
# The launcher is the first thing that runs on a Linux install and it runs
# under `set -e`, so a failing test inside one of its `&&` chains would abort
# the launch and the app would simply never start — with no error a user
# could act on. That failure mode is invisible to shellcheck, which is why
# this executes the real script rather than inspecting it.
#
# It runs the launcher against a stub AppImage that prints the environment it
# was handed, so the assertions are about what the app actually receives.
set -e

here="$(cd "$(dirname "$0")" && pwd)"
launcher="$here/linux-launcher.sh"
failures=0

check() {
    # check <description> <expected> <actual>
    if [ "$2" != "$3" ]; then
        printf 'FAIL %s\n  expected: %s\n  actual:   %s\n' "$1" "$2" "$3" >&2
        failures=$((failures + 1))
    fi
}

# A stub standing in for the real AppImage: prints one VAR=value per line.
make_sandbox() {
    dir="$(mktemp -d)"
    cp "$launcher" "$dir/PS5Upload.sh"
    chmod +x "$dir/PS5Upload.sh"
    cat > "$dir/PS5Upload.AppImage" <<'STUB'
#!/bin/sh
echo "DMABUF=${WEBKIT_DISABLE_DMABUF_RENDERER:-}"
echo "COMPOSITING=${WEBKIT_DISABLE_COMPOSITING_MODE:-}"
echo "LD_PRELOAD=${LD_PRELOAD:-}"
echo "EXTRACT=${APPIMAGE_EXTRACT_AND_RUN:-}"
echo "ARGS=$*"
STUB
    chmod +x "$dir/PS5Upload.AppImage"
    printf '%s' "$dir"
}

# Run the launcher in a sandbox and echo the stub's output. Any non-zero exit
# is reported rather than swallowed — that is the regression this guards.
run_launcher() {
    d="$(make_sandbox)"
    if ! out="$("$d/PS5Upload.sh" "$@" 2>&1)"; then
        printf 'FAIL launcher exited non-zero\n%s\n' "$out" >&2
        failures=$((failures + 1))
    fi
    rm -rf "$d"
    printf '%s' "$out"
}

field() { printf '%s\n' "$1" | sed -n "s/^$2=//p"; }

# --- The launcher always starts the app -----------------------------------
# Every case below asserts through run_launcher, which fails loudly if the
# script exits non-zero. On a fresh Linux box that is the difference between
# the app opening and nothing happening at all.

# --- Defaults --------------------------------------------------------------
out="$(run_launcher)"
check "DMABUF renderer disabled by default" "1" "$(field "$out" DMABUF)"
check "AppImage self-extracts (no libfuse2 needed)" "1" "$(field "$out" EXTRACT)"

# --- The user's choice always wins ----------------------------------------
out="$(WEBKIT_DISABLE_DMABUF_RENDERER=0 run_launcher)"
check "explicit DMABUF=0 is respected" "0" "$(field "$out" DMABUF)"

out="$(LD_PRELOAD=/tmp/mine.so run_launcher)"
check "an existing LD_PRELOAD is never clobbered" "/tmp/mine.so" \
    "$(field "$out" LD_PRELOAD)"

# --- Arguments reach the app ----------------------------------------------
out="$(run_launcher --flag value)"
check "arguments are forwarded" "--flag value" "$(field "$out" ARGS)"

# --- The preload is scoped, not blanket -----------------------------------
# Wayland alone must change nothing: an AMD/Intel Wayland user whose window
# renders fine today keeps exactly the library resolution they have now.
out="$(XDG_SESSION_TYPE=wayland WAYLAND_DISPLAY=wayland-0 run_launcher)"
check "Wayland without NVIDIA does not preload" "" "$(field "$out" LD_PRELOAD)"

# X11 must never preload, NVIDIA or not.
out="$(XDG_SESSION_TYPE=x11 PS5UPLOAD_FORCE_WAYLAND_PRELOAD=0 run_launcher)"
check "X11 does not preload" "" "$(field "$out" LD_PRELOAD)"

# --- The Wayland+NVIDIA path (issue #285) ---------------------------------
# Forced, because the real trigger reads /proc/driver/nvidia, which cannot be
# faked portably. This exercises the library lookup and the export; whether
# the machine IS NVIDIA-on-Wayland is the detection above.
out="$(PS5UPLOAD_FORCE_WAYLAND_PRELOAD=1 run_launcher)"
preload="$(field "$out" LD_PRELOAD)"
if [ -n "$preload" ]; then
    # Only ever a real file — a stale ldconfig entry must not become an
    # LD_PRELOAD, which would make every launch fail with a linker error.
    if [ ! -e "$preload" ]; then
        printf 'FAIL preloaded a path that does not exist: %s\n' "$preload" >&2
        failures=$((failures + 1))
    fi
    case "$preload" in
        *libwayland-client.so.0*) ;;
        *) printf 'FAIL preloaded the wrong library: %s\n' "$preload" >&2
           failures=$((failures + 1)) ;;
    esac
fi
# No assertion that it IS set: hosts without libwayland (macOS, minimal CI
# containers) correctly set nothing. The guarantee is that whatever it sets
# is a real libwayland-client, never a broken path.

if [ "$failures" -ne 0 ]; then
    printf '%s check(s) failed\n' "$failures" >&2
    exit 1
fi
printf 'linux-launcher self-test OK\n'
