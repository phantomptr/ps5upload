#!/bin/sh
# PS5Upload Linux launcher (fresh-install safe + white-screen safe).
#
# Why this wrapper exists:
#
# 1. FUSE (Ubuntu 24.04+, fresh arm64 images, most modern desktops):
#    Tauri's type-2 AppImage uses FUSE to self-mount at startup, but
#    those distros ship without libfuse2 by default — so a brand-new
#    user double-clicking PS5Upload.AppImage gets "AppImage requires
#    FUSE to run" and nothing happens. `APPIMAGE_EXTRACT_AND_RUN=1`
#    tells the AppImage runtime to self-extract to /tmp and exec from
#    there instead — no kernel module, no apt install, no daemon.
#
# 2. WebKitGTK white screen (Bazzite, SteamOS, NVIDIA, some Mesa
#    stacks): the app window comes up blank/white because WebKitGTK's
#    accelerated compositing + DMABUF renderer don't render on those
#    GPU/compositor combos. The folder/.deb build can hit this too,
#    but it's most common with the AppImage on gaming distros.
#    Disabling the DMABUF renderer makes WebKit fall back to a path
#    that renders correctly on those stacks, at no perceptible cost.
#    If the window is STILL blank, also disable accelerated
#    compositing: `WEBKIT_DISABLE_COMPOSITING_MODE=1 ./PS5Upload.sh`.
#    That one is not the default because it forces software rendering
#    of the whole page and makes scrolling sluggish for everyone.
#
#    If a white screen persists even with these set, escalate (see
#    FAQ -> "white screen on Linux"): force X11 with `GDK_BACKEND=x11`,
#    then software rendering with `LIBGL_ALWAYS_SOFTWARE=1`.
#
# Shipped in the Linux release .zip alongside PS5Upload.AppImage; the
# release workflow only copies this file into the zip (it doesn't
# generate it inline) so its contents go through the normal repo
# review / lint pipeline. Prefer launching via this wrapper rather than
# the bare PS5Upload.AppImage so both rescues apply.
set -e
here="$(cd "$(dirname "$0")" && pwd)"

# Overridable WebKitGTK rendering workaround — see note (2) above.
#
# Only DMABUF is disabled here. Disabling accelerated COMPOSITING as well
# used to be the default and made scrolling sluggish on every Linux install,
# because it drops the page to software rendering — which this app feels
# acutely, its main screens being long scrolling lists. The app itself now
# turns compositing off for the one stack that needs it (NVIDIA on Wayland);
# see client/src-tauri/src/linux_gpu.rs. It stays forceable by hand:
#   WEBKIT_DISABLE_COMPOSITING_MODE=1 ./PS5Upload.sh
: "${WEBKIT_DISABLE_DMABUF_RENDERER:=1}"
export WEBKIT_DISABLE_DMABUF_RENDERER

# 3. AppImage + Wayland + NVIDIA: bundled libwayland-client mismatch.
#
#    An AppImage carries its own libwayland-client.so.0. When it is older
#    than the compositor the system is actually running, the Wayland
#    connection can fail and the window comes up white — the same symptom as
#    (2) but a different cause, which is why the WebKit switches alone did
#    not fix issue #285. Preloading the SYSTEM copy makes the app link
#    against the library its compositor matches.
#
#    Deliberately scoped to Wayland + NVIDIA, the configuration in that
#    report. Anyone on Wayland today whose window renders correctly keeps
#    exactly the library resolution they have now.
#
#    This cannot move into the app the way the WebKit variables did:
#    LD_PRELOAD is read by the dynamic linker when the process starts, so it
#    has to be set before exec.
#
#    Set LD_PRELOAD yourself to skip this entirely, or set
#    PS5UPLOAD_FORCE_WAYLAND_PRELOAD=1 to apply it when the detection below
#    does not recognise your setup.
if [ -z "${LD_PRELOAD:-}" ]; then
    # XDG_SESSION_TYPE is authoritative when set; a WAYLAND_DISPLAY
    # inherited from a parent shell can outlive the session it named.
    # Mirrors detect_wayland() in linux_gpu.rs.
    is_wayland=0
    if [ -n "${XDG_SESSION_TYPE:-}" ]; then
        case "$XDG_SESSION_TYPE" in
            [Ww]ayland) is_wayland=1 ;;
        esac
    elif [ -n "${WAYLAND_DISPLAY:-}" ]; then
        is_wayland=1
    fi

    # Proprietary NVIDIA driver only — nouveau does not show this and would
    # be changed for nothing.
    has_nvidia=0
    if [ -e /proc/driver/nvidia/version ] || [ -d /sys/module/nvidia ]; then
        has_nvidia=1
    fi

    if [ "${PS5UPLOAD_FORCE_WAYLAND_PRELOAD:-0}" = 1 ]; then
        is_wayland=1
        has_nvidia=1
    fi

    if [ "$is_wayland" = 1 ] && [ "$has_nvidia" = 1 ]; then
        # Ask the dynamic linker where the library actually is rather than
        # guessing a path: /usr/lib64 is right on Fedora/Arch/CachyOS but
        # wrong on Debian/Ubuntu, which use multiarch
        # (/usr/lib/x86_64-linux-gnu). ldconfig is commonly in /sbin, which
        # is not on a non-root PATH on some distros.
        wl_lib=""
        for ldc in ldconfig /sbin/ldconfig /usr/sbin/ldconfig; do
            if command -v "$ldc" >/dev/null 2>&1; then
                wl_lib=$("$ldc" -p 2>/dev/null \
                    | awk '/libwayland-client\.so\.0/ {print $NF; exit}')
                [ -n "$wl_lib" ] && break
            fi
        done
        # Fall back to the usual locations if ldconfig is unavailable.
        if [ -z "$wl_lib" ]; then
            for cand in \
                /usr/lib64/libwayland-client.so.0 \
                /usr/lib/x86_64-linux-gnu/libwayland-client.so.0 \
                /usr/lib/aarch64-linux-gnu/libwayland-client.so.0 \
                /usr/lib/libwayland-client.so.0
            do
                [ -e "$cand" ] && wl_lib="$cand" && break
            done
        fi
        if [ -n "$wl_lib" ] && [ -e "$wl_lib" ]; then
            LD_PRELOAD="$wl_lib"
            export LD_PRELOAD
        fi
    fi
fi

exec env APPIMAGE_EXTRACT_AND_RUN=1 "$here/PS5Upload.AppImage" "$@"
