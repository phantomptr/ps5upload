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
#    Disabling both makes WebKit fall back to a path that renders
#    correctly — negligible cost for this app's plain UI (no WebGL /
#    heavy canvas). We set them only when the user hasn't already
#    chosen a value, so anyone who wants the accelerated path back can
#    run e.g. `WEBKIT_DISABLE_COMPOSITING_MODE=0 ./PS5Upload.sh`.
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

# Overridable WebKitGTK rendering workarounds — see note (2) above.
: "${WEBKIT_DISABLE_COMPOSITING_MODE:=1}"
: "${WEBKIT_DISABLE_DMABUF_RENDERER:=1}"
export WEBKIT_DISABLE_COMPOSITING_MODE WEBKIT_DISABLE_DMABUF_RENDERER

exec env APPIMAGE_EXTRACT_AND_RUN=1 "$here/PS5Upload.AppImage" "$@"
