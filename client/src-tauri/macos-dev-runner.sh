#!/usr/bin/env bash
# Stands in for `cargo` on macOS (see tauri.macos.conf.json, build.runner).
#
# On macOS 27 a WKWebView in a process that is not inside an .app bundle never
# runs its page: the window opens and stays blank, the WebContent process idles
# (0% CPU) and never talks to the network. `tauri dev` runs the bare
# target/debug binary, so `make run-client` showed a black window while the
# released app (a bundle) worked. The same binary works when it runs from
# inside a bundle, so for `run` this builds as cargo would, wraps the binary in
# a minimal "PS5Upload Dev.app" next to it and execs it from there, which keeps
# tauri dev's log output and restart-on-change. Everything else (build, check,
# a release `tauri build`) passes straight to cargo.
#
# The bundle carries the app's own identifier (tauri.conf.json). macOS grants
# local network access per app identity: with a new identifier (tried
# "….dev") or none at all, WebKit's loads stayed blocked with no prompt, while
# the identifier the installed app already holds the grant for works. The app's
# data folder was already keyed on that identifier, so nothing new is shared.
set -euo pipefail

if [ "${1:-}" != "run" ]; then
  exec cargo "$@"
fi
shift

# cargo run [build flags] [-- app args]
build_args=()
app_args=()
while [ $# -gt 0 ]; do
  if [ "$1" = "--" ]; then
    shift
    app_args=("$@")
    break
  fi
  build_args+=("$1")
  shift
done

# Build, and learn the binary's path from cargo itself (target dir, profile and
# --target are all whatever the flags say). Diagnostics still go to the terminal.
exe=""
while IFS= read -r line; do
  case "$line" in
    *'"reason":"compiler-artifact"'*'"executable":"'*)
      path="${line##*\"executable\":\"}"
      path="${path%%\"*}"
      [ -n "$path" ] && exe="$path"
      ;;
  esac
done < <(cargo build ${build_args[@]+"${build_args[@]}"} --message-format=json-render-diagnostics)
if [ -z "$exe" ] || [ ! -x "$exe" ]; then
  echo "macos-dev-runner: cargo build did not report the app binary" >&2
  exit 1
fi

name="$(basename "$exe")"
ident="$(sed -n 's/^[[:space:]]*"identifier":[[:space:]]*"\([^"]*\)".*/\1/p' tauri.conf.json | head -1)"
ident="${ident:-com.phantomptr.ps5upload}"
app="$(dirname "$exe")/PS5Upload Dev.app"
mkdir -p "$app/Contents/MacOS"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>${name}</string>
  <key>CFBundleIdentifier</key><string>${ident}</string>
  <key>CFBundleName</key><string>PS5Upload Dev</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSLocalNetworkUsageDescription</key><string>PS5Upload connects to your PS5 on the local network.</string>
</dict>
</plist>
PLIST
cp -f "$exe" "$app/Contents/MacOS/$name"
codesign --force --sign - "$app" >/dev/null 2>&1 || true

exec "$app/Contents/MacOS/$name" ${app_args[@]+"${app_args[@]}"}
