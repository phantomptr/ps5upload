#!/bin/sh
# PS5Upload Linux launcher (fresh-install safe).
#
# Why this wrapper exists:
#   Ubuntu 24.04+ ships without libfuse2 by default. Tauri's type-2
#   AppImage uses FUSE to self-mount at startup — so a brand-new
#   `ubuntu-desktop` user double-clicking PS5Upload.AppImage gets
#   "AppImage requires FUSE to run" and nothing happens. The same
#   trap exists on freshly-installed arm64 images and most other
#   modern desktops that dropped libfuse2 in favour of fuse3.
#
#   `APPIMAGE_EXTRACT_AND_RUN=1` tells the AppImage runtime to
#   self-extract to /tmp and exec from there instead of fuse-mounting
#   — no kernel module, no apt install, no daemon. Users with
#   libfuse2 already installed can still run PS5Upload.AppImage
#   directly if they prefer the slightly faster fuse-mount path.
#
# Shipped in the Linux release .zip alongside PS5Upload.AppImage; the
# release workflow only copies this file into the zip (it doesn't
# generate it inline) so its contents go through the normal repo
# review / lint pipeline.
set -e
here="$(cd "$(dirname "$0")" && pwd)"
exec env APPIMAGE_EXTRACT_AND_RUN=1 "$here/PS5Upload.AppImage" "$@"
