#!/usr/bin/env bash
#
# Android emulator lifecycle for testing the app without a physical device.
#
# Subcommands: create | start | stop | status | test
#
# ARTIFACTS LIVE OUTSIDE THE REPO, at ~/.ps5upload/android-test/<stamp>/.
# That is deliberate and not a style choice: this repo has already leaked
# debug screenshots into a PUBLIC repo (see the "Dev screenshots" section of
# .gitignore), because an ignore list only ever catches the filenames someone
# already used. A file that is not in the working tree cannot be committed by
# a stray `git add -A`, which is the failure mode that actually happened.
set -uo pipefail

SDK="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
EMULATOR="$SDK/emulator/emulator"
ADB="$SDK/platform-tools/adb"
AVDMANAGER="$SDK/cmdline-tools/latest/bin/avdmanager"

AVD_NAME="${PS5UPLOAD_AVD:-ps5upload-test}"
APP_ID="com.phantomptr.ps5upload"
# Boot can be slow on a cold AVD; well past that and something is wrong, so
# fail loudly rather than hang a CI job or a developer's terminal forever.
BOOT_TIMEOUT_SEC="${PS5UPLOAD_EMU_BOOT_TIMEOUT:-300}"
ART_ROOT="$HOME/.ps5upload/android-test"

say()  { printf '%s\n' "$*"; }
die()  { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

need_tools() {
  [ -x "$EMULATOR" ]   || die "emulator not found at $EMULATOR — install it via Android Studio's SDK Manager."
  [ -x "$ADB" ]        || die "adb not found at $ADB"
  [ -x "$AVDMANAGER" ] || die "avdmanager not found at $AVDMANAGER — install the SDK command-line tools."
}

# The AVD's system image. Prefer one already on disk: a fresh download is
# multiple GB, and the project targets SDK 36, which is present.
pick_image() {
  local want
  for want in \
    "system-images;android-36.1;google_apis;arm64-v8a" \
    "system-images;android-36;google_apis;arm64-v8a" \
    "system-images;android-35;google_apis;arm64-v8a" \
    "system-images;android-34;google_apis;arm64-v8a"; do
    local path="${want//;//}"
    if [ -d "$SDK/${path#system-images/}" ] || [ -d "$SDK/$path" ]; then
      printf '%s' "$want"; return 0
    fi
  done
  return 1
}

emu_serial() {
  "$ADB" devices 2>/dev/null | awk 'NR>1 && $1 ~ /^emulator-/ && $2=="device"{print $1; exit}'
}

cmd_create() {
  need_tools
  if "$EMULATOR" -list-avds 2>/dev/null | grep -qx "$AVD_NAME"; then
    say "✓ AVD '$AVD_NAME' already exists"
    return 0
  fi
  local img
  img="$(pick_image)" || die "no arm64-v8a system image installed. Install one:
  $SDK/cmdline-tools/latest/bin/sdkmanager 'system-images;android-36.1;google_apis;arm64-v8a'"
  say "Creating AVD '$AVD_NAME' from $img ..."
  # `-d pixel_6` gives a sane screen size; without a device profile the AVD
  # defaults to a shape that makes screenshots hard to read.
  printf 'no\n' | "$AVDMANAGER" create avd -n "$AVD_NAME" -k "$img" -d pixel_6 --force >/dev/null \
    || die "avdmanager could not create the AVD"
  say "✓ Created AVD '$AVD_NAME'"
}

cmd_start() {
  need_tools
  local existing; existing="$(emu_serial)"
  if [ -n "$existing" ]; then
    say "✓ Emulator already running ($existing)"
    return 0
  fi
  "$EMULATOR" -list-avds 2>/dev/null | grep -qx "$AVD_NAME" || cmd_create

  say "Booting '$AVD_NAME' headless (up to ${BOOT_TIMEOUT_SEC}s) ..."
  # -no-window: headless. -no-snapshot-save: never persist a half-booted
  # state, which turns one bad run into every later run failing the same way.
  # -wipe-data would reset each run; we deliberately keep data so an install
  # survives, matching how a real device behaves between builds.
  nohup "$EMULATOR" -avd "$AVD_NAME" \
    -no-window -no-audio -no-boot-anim -no-snapshot-save \
    -gpu swiftshader_indirect \
    >"$HOME/.ps5upload/android-emu.log" 2>&1 &

  local waited=0 serial=""
  while [ "$waited" -lt "$BOOT_TIMEOUT_SEC" ]; do
    serial="$(emu_serial)"
    if [ -n "$serial" ]; then
      local booted
      booted="$("$ADB" -s "$serial" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r\n')"
      if [ "$booted" = "1" ]; then
        say "✓ Emulator booted ($serial) after ${waited}s"
        return 0
      fi
    fi
    sleep 5; waited=$((waited + 5))
  done
  die "emulator did not finish booting within ${BOOT_TIMEOUT_SEC}s — see ~/.ps5upload/android-emu.log"
}

cmd_stop() {
  need_tools
  local serial; serial="$(emu_serial)"
  if [ -z "$serial" ]; then say "No emulator running"; return 0; fi
  "$ADB" -s "$serial" emu kill >/dev/null 2>&1
  say "✓ Stopped $serial"
}

cmd_status() {
  need_tools
  say "AVDs:        $("$EMULATOR" -list-avds 2>/dev/null | tr '\n' ' ')"
  local running; running="$(emu_serial)"
  say "Running:     ${running:-none}"
  say "Artifacts:   $ART_ROOT (outside the repo — cannot be committed)"
}

# Install the APK, launch it, and PROVE what rendered.
#
# A screencap is in the critical path on purpose. The last Android session
# here had three separate faults hiding behind a `make run-android` that
# printed nothing and exited 0; the screenshot was the only thing that told
# the truth. An exit code is not evidence that the app came up.
cmd_test() {
  need_tools
  local apk="client/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk"
  [ -f "$apk" ] || die "no APK at $apk — run 'make android-build' first"

  cmd_start
  local serial; serial="$(emu_serial)"
  [ -n "$serial" ] || die "emulator is not reporting a serial"

  local stamp; stamp="$(date +%Y%m%d-%H%M%S)"
  local out="$ART_ROOT/$stamp"
  mkdir -p "$out"

  say "Installing $APP_ID on $serial ..."
  if ! "$ADB" -s "$serial" install -r "$apk" >"$out/install.log" 2>&1; then
    # A key mismatch is the common one, and reinstalling clears app data.
    if grep -qi 'UPDATE_INCOMPATIBLE\|signatures do not match' "$out/install.log"; then
      say "Existing install was signed with a different key — reinstalling fresh (app data is cleared)"
      "$ADB" -s "$serial" uninstall "$APP_ID" >/dev/null 2>&1
      "$ADB" -s "$serial" install "$apk" >>"$out/install.log" 2>&1 \
        || { cat "$out/install.log"; die "install failed — see $out/install.log"; }
    else
      cat "$out/install.log"; die "install failed — see $out/install.log"
    fi
  fi
  say "✓ Installed"

  # Pre-grant the runtime permissions the app asks for on first run. Without
  # this the very first screencap is a system permission dialog covering the
  # UI, which is exactly the frame we need to be able to read.
  for perm in android.permission.POST_NOTIFICATIONS; do
    "$ADB" -s "$serial" shell pm grant "$APP_ID" "$perm" >/dev/null 2>&1 || true
  done

  "$ADB" -s "$serial" logcat -c >/dev/null 2>&1

  # Ask the package manager which activity the launcher would start, then start
  # exactly that. `monkey` looks like the obvious tool here, but it exits -5 and
  # launches nothing when it cannot match its own filter, and it does that
  # silently -- a green run that never opened the app.
  local activity
  activity="$("$ADB" -s "$serial" shell cmd package resolve-activity --brief \
    -c android.intent.category.LAUNCHER "$APP_ID" 2>/dev/null \
    | tr -d '\r' | grep "^$APP_ID/" | head -1)"
  [ -n "$activity" ] || die "no launcher activity for $APP_ID -- is the APK the right one?"
  say "Launching $activity ..."
  "$ADB" -s "$serial" shell am start -W -n "$activity" >"$out/launch.log" 2>&1 || true
  if ! grep -q "^Status: ok" "$out/launch.log"; then
    cat "$out/launch.log"
    die "activity did not start -- see $out/launch.log"
  fi

  # Give the WebView a moment to lay out before capturing.
  sleep 8
  "$ADB" -s "$serial" shell screencap -p /sdcard/ps5upload.png >/dev/null 2>&1
  "$ADB" -s "$serial" pull /sdcard/ps5upload.png "$out/screen.png" >/dev/null 2>&1
  "$ADB" -s "$serial" logcat -d -t 2000 >"$out/logcat.txt" 2>&1

  # Did the process actually survive, or did it start and die?
  local pid
  pid="$("$ADB" -s "$serial" shell pidof "$APP_ID" 2>/dev/null | tr -d '\r\n')"

  say ""
  say "── result ─────────────────────────────────────────"
  if [ -n "$pid" ]; then say "app running:  yes (pid $pid)"; else say "app running:  NO — it started and exited"; fi
  if [ -s "$out/screen.png" ]; then
    say "screenshot:   $out/screen.png ($(wc -c <"$out/screen.png" | tr -d ' ') bytes)"
  else
    say "screenshot:   MISSING — could not capture"
  fi
  say "logcat:       $out/logcat.txt"
  say "artifacts:    $out  (outside the repo)"
  local crashes
  crashes="$(grep -ciE "FATAL EXCEPTION|ANR in|$APP_ID.*(crash|died)" "$out/logcat.txt" 2>/dev/null || echo 0)"
  say "crash lines:  $crashes"
  say "───────────────────────────────────────────────────"

  [ -n "$pid" ] || exit 1
}

case "${1:-}" in
  create) cmd_create ;;
  start)  cmd_start ;;
  stop)   cmd_stop ;;
  status) cmd_status ;;
  test)   cmd_test ;;
  *) die "usage: $0 {create|start|stop|status|test}" ;;
esac
