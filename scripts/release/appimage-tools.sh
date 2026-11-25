#!/usr/bin/env bash
#
# Put the tools `tauri build` uses for the AppImage into its tools cache, each
# from a pinned URL and checked against its SHA-256 before it is kept. Called by
# the Linux legs of .github/workflows/publish.yml before the Tauri build.
#
# Tauri's bundler (tauri-bundler 2.10.5, in @tauri-apps/cli 2.12.5) downloads
# AppRun, linuxdeploy and linuxdeploy-plugin-appimage on its own, without any
# checksum, and the plugin from the moving "continuous" release; it skips a
# download when the file is already in its cache. Filling the cache first means
# the build runs only these files. The names and the linuxdeploy build (07333c6)
# are the bundler's (src/bundle/linux/appimage/linuxdeploy.rs): when the Tauri
# CLI is updated, check that file and update the table below with it.
#
# Usage: appimage-tools.sh <x86_64|aarch64> [cache dir]
#   cache dir defaults to Tauri's: ${XDG_CACHE_HOME:-$HOME/.cache}/tauri
set -euo pipefail

arch="${1:-}"
dest="${2:-${XDG_CACHE_HOME:-$HOME/.cache}/tauri}"

gh_dl="https://github.com"
tauri_bin="$gh_dl/tauri-apps/binary-releases/releases/download"
plugin_rel="$gh_dl/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/1-alpha-20250213-1"

# name in the cache | URL | SHA-256
case "$arch" in
x86_64)
  tools=(
    "AppRun-x86_64|$tauri_bin/apprun-old/AppRun-x86_64|f30140a43a0a59e46db21bdefdf749b9e9f2c6946e92afabbacf98b8ae73fb4f"
    "linuxdeploy-07333c6-x86_64.AppImage|$tauri_bin/linuxdeploy-07333c6/linuxdeploy-x86_64.AppImage|36a2d7e274d12e1050d0e9ecfe11d339ed54720b2bec464c286d53f8b07f5c62"
    "linuxdeploy-plugin-appimage.AppImage|$plugin_rel/linuxdeploy-plugin-appimage-x86_64.AppImage|992d502a248e14ab185448ddf6f6e7d25558cb84d4623c354c3af350c25fccb3"
  )
  ;;
aarch64)
  tools=(
    "AppRun-aarch64|$tauri_bin/apprun-old/AppRun-aarch64|072f17c0895a85c490282fe5395c5007e5fc75da727e553b3b8fb680feb11578"
    "linuxdeploy-07333c6-aarch64.AppImage|$tauri_bin/linuxdeploy-07333c6/linuxdeploy-aarch64.AppImage|556ab80baa98e600aa80f0dcedfb70bca0e1ce7e9f147fb345be3fcc3e91b2b1"
    "linuxdeploy-plugin-appimage.AppImage|$plugin_rel/linuxdeploy-plugin-appimage-aarch64.AppImage|83c292149274965a865dcd44c135cfca8ba28c6b7de3eb628d4b8b5f248af17c"
  )
  ;;
*)
  echo "usage: $0 <x86_64|aarch64> [cache dir]" >&2
  exit 2
  ;;
esac

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

mkdir -p "$dest"
for entry in "${tools[@]}"; do
  IFS='|' read -r name url want <<<"$entry"
  part="$dest/.$name.part"
  rm -f "$part"
  curl --retry 3 --fail --location --silent --show-error -o "$part" "$url"
  got="$(sha256 "$part")"
  if [ "$got" != "$want" ]; then
    rm -f "$part"
    echo "::error::$name: SHA-256 $got does not match the pinned $want ($url); not used" >&2
    exit 1
  fi
  chmod +x "$part"
  # Always replaced: the bundler patches its cached linuxdeploy in place after a run.
  mv -f "$part" "$dest/$name"
  echo "$name: verified ($want)"
done
