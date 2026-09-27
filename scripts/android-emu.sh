#!/usr/bin/env bash
#
# Android emulator lifecycle for testing the app without a physical device.
#
# Subcommands: create | start | ensure | stop | status | test
#
#   ensure  reuses a running (or still-booting) emulator or attached device,
#           and boots one only when there is none. With PS5UPLOAD_EMU_WINDOW=1
#           it brings the emulator's window to the front, restarting a headless
#           one with its window. Prints "booted" or "preexisting" so a caller
#           knows whether it owns the emulator and should shut it down (see
#           run-android in the Makefile).
#
# `test` leaves the emulator running so re-runs skip the ~25s cold boot; set
# PS5UPLOAD_EMU_TEARDOWN=1 (CI) to shut down one that this run booted.
#
# The emulator boots headless by default. PS5UPLOAD_EMU_WINDOW=1 boots it with
# its window, so the app can be seen and used (make run-android does this;
# make run-android-background and make emu-test stay headless).
#
# Performance: software rendering (PS5UPLOAD_EMU_GPU=host opts into the Mac's
# GPU, which is faster but froze Android 16 in testing). 4 GB RAM
# (PS5UPLOAD_EMU_MEMORY_MB) and half the host's cores up to 6
# (PS5UPLOAD_EMU_CORES) — the AVD's own 2 GB / 4 cores made Android 16 crawl.
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

# Half the host's cores, capped at 6: the AVD default of 4 left Android 16 and
# the app's WebView fighting for CPU on a machine with plenty to spare.
emu_cores() {
  local n
  n="$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 4)"
  n=$((n / 2)); [ "$n" -lt 2 ] && n=2; [ "$n" -gt 6 ] && n=6
  printf '%s' "${PS5UPLOAD_EMU_CORES:-$n}"
}

emu_serial() {
  "$ADB" devices 2>/dev/null | awk 'NR>1 && $1 ~ /^emulator-/ && $2=="device"{print $1; exit}'
}

# ANY attached device, emulator or physical. `ensure` uses this rather than
# emu_serial so that a phone plugged in by hand still wins and we never boot a
# second target behind the developer's back.
any_device() {
  "$ADB" devices 2>/dev/null | awk 'NR>1 && $2=="device"{print $1; exit}'
}

# The emulator process for our AVD, if one is running at all — including one
# still booting, which adb does not list as "device" yet. Checking only adb
# let a second run start another emulator while the first was booting.
emu_running() {
  pgrep -f "(/emulator/emulator|qemu-system[^ ]*) -avd $AVD_NAME( |$)" >/dev/null 2>&1
}

emu_headless() {
  pgrep -f "(/emulator/emulator|qemu-system[^ ]*) -avd $AVD_NAME .*-no-window" >/dev/null 2>&1
}

# Stop our emulator and wait until its process has really exited — a clean
# shutdown can take well over 30s, and a start that runs while the old one is
# still exiting mistakes it for one that is booting. Force-kill if it lingers.
stop_and_wait() {
  cmd_stop
  local waited=0
  while emu_running && [ "$waited" -lt 60 ]; do sleep 1; waited=$((waited + 1)); done
  if emu_running; then
    say "Emulator still exiting after 60s — forcing it to stop"
    pkill -9 -f "(/emulator/emulator|qemu-system[^ ]*) -avd $AVD_NAME( |$)" 2>/dev/null
    sleep 2
  fi
}

# Wait (bounded) for an emulator that is already starting to finish booting.
wait_booted() {
  local waited=0 serial booted
  while [ "$waited" -lt "$BOOT_TIMEOUT_SEC" ]; do
    serial="$(emu_serial)"
    if [ -n "$serial" ]; then
      booted="$("$ADB" -s "$serial" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r\n')"
      [ "$booted" = "1" ] && return 0
    fi
    emu_running || return 2   # it exited instead of booting
    sleep 5; waited=$((waited + 5))
  done
  return 1
}

# Raise the emulator's window. Best effort: macOS via AppleScript, Linux via
# wmctrl when it is installed; elsewhere the window simply stays where it is.
bring_to_front() {
  case "$(uname -s)" in
    Darwin)
      osascript -e 'tell application "System Events" to set frontmost of (first process whose unix id is '"$(pgrep -f "qemu-system.*-avd $AVD_NAME" | head -1)"') to true' >/dev/null 2>&1 || true ;;
    Linux)
      command -v wmctrl >/dev/null 2>&1 && wmctrl -a "Android Emulator" >/dev/null 2>&1 || true ;;
  esac
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
  if emu_running; then
    say "Emulator '$AVD_NAME' is already starting — waiting for it instead of booting another ..."
    local rc=0; wait_booted || rc=$?
    if [ "$rc" = 0 ]; then
      say "✓ Emulator booted ($(emu_serial))"
      return 0
    fi
    [ "$rc" = 2 ] || die "the running emulator did not finish booting within ${BOOT_TIMEOUT_SEC}s"
    say "It exited instead — booting a fresh one ..."
  fi
  "$EMULATOR" -list-avds 2>/dev/null | grep -qx "$AVD_NAME" || cmd_create

  # Software rendering by default, windowed or not. The host GPU (-gpu host)
  # is faster but froze the Android 16 image within minutes on an M1 Max —
  # the emulator itself warns its guest GL layer is unstable above API 35 —
  # so it is opt-in: PS5UPLOAD_EMU_GPU=host.
  local gpu="${PS5UPLOAD_EMU_GPU:-swiftshader_indirect}"
  local window_flags=(-no-window -gpu "$gpu")
  if [ "${PS5UPLOAD_EMU_WINDOW:-0}" = "1" ]; then
    window_flags=(-gpu "$gpu")
    say "Booting '$AVD_NAME' with its window (up to ${BOOT_TIMEOUT_SEC}s) ..."
  else
    say "Booting '$AVD_NAME' headless (up to ${BOOT_TIMEOUT_SEC}s) ..."
  fi
  # -no-window: headless. -no-snapshot-save: never persist a half-booted
  # state, which turns one bad run into every later run failing the same way.
  # -wipe-data would reset each run; we deliberately keep data so an install
  # survives, matching how a real device behaves between builds.
  # `set -m` puts the emulator in its OWN process group. Without it a Ctrl-C
  # aimed at whatever launched us (make, a dev server) is delivered to the
  # whole group and kills the emulator mid-write -- it dies, but uncleanly,
  # and the teardown trap never gets to run. Shutdown must go through
  # `adb emu kill`, which is what cmd_stop does.
  set -m
  nohup "$EMULATOR" -avd "$AVD_NAME" \
    ${window_flags[@]+"${window_flags[@]}"} -no-audio -no-boot-anim -no-snapshot-save \
    -memory "${PS5UPLOAD_EMU_MEMORY_MB:-4096}" -cores "$(emu_cores)" \
    >"$HOME/.ps5upload/android-emu.log" 2>&1 &
  set +m
  local emu_pid=$!

  # A Ctrl-C during the ~25s boot would otherwise orphan a half-booted
  # emulator: it is in its own process group now, so the signal never reaches
  # it, and the caller's teardown trap does not exist yet.
  trap 'say "Interrupted during boot - stopping the emulator..."; kill '"$emu_pid"' 2>/dev/null; exit 130' INT TERM

  local waited=0 serial=""
  while [ "$waited" -lt "$BOOT_TIMEOUT_SEC" ]; do
    serial="$(emu_serial)"
    if [ -n "$serial" ]; then
      local booted
      booted="$("$ADB" -s "$serial" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r\n')"
      if [ "$booted" = "1" ]; then
        trap - INT TERM
        say "✓ Emulator booted ($serial) after ${waited}s"
        return 0
      fi
    fi
    sleep 5; waited=$((waited + 5))
  done
  trap - INT TERM
  kill "$emu_pid" 2>/dev/null
  die "emulator did not finish booting within ${BOOT_TIMEOUT_SEC}s — see ~/.ps5upload/android-emu.log"
}

# Boot an emulator ONLY if nothing is attached, and report on stdout which
# happened: "booted" (we own it, the caller should shut it down) or
# "preexisting" (someone else's device or emulator -- leave it alone).
#
# All human-readable progress goes to stderr, because the caller reads stdout.
cmd_ensure() {
  need_tools
  local want_window="${PS5UPLOAD_EMU_WINDOW:-0}"
  # Our emulator is running (or still booting): reuse it, never boot a second.
  if emu_running; then
    if [ "$want_window" = "1" ] && emu_headless; then
      # A headless emulator cannot grow a window, so restart this one with
      # its window. Its data is kept (no wipe), and it stays up afterwards
      # like any emulator this run did not create.
      say "Emulator is running headless — restarting it with its window ..." >&2
      stop_and_wait >&2
      cmd_start >&2 || die "could not restart the emulator with its window"
      bring_to_front
    else
      if [ -z "$(emu_serial)" ]; then
        say "Emulator is still starting — waiting for it ..." >&2
        local rc=0; wait_booted || rc=$?
        if [ "$rc" = 2 ]; then
          say "It exited instead — booting a fresh one ..." >&2
          cmd_start >&2 || die "could not boot an emulator"
          [ "$want_window" = "1" ] && bring_to_front
          printf 'booted\n'
          return 0
        fi
        [ "$rc" = 0 ] || die "the running emulator did not finish booting within ${BOOT_TIMEOUT_SEC}s"
      fi
      say "✓ Using the running emulator ($(emu_serial))" >&2
      [ "$want_window" = "1" ] && bring_to_front
    fi
    printf 'preexisting\n'
    return 0
  fi
  # A phone plugged in by hand (or another emulator) still wins.
  if [ -n "$(any_device)" ]; then
    say "✓ Using the already-attached device ($(any_device))" >&2
    printf 'preexisting\n'
    return 0
  fi
  cmd_start >&2 || die "could not boot an emulator"
  [ "$want_window" = "1" ] && bring_to_front
  printf 'booted\n'
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

  # Remember whether an emulator was already up: we only tear down one that
  # THIS run booted, and only when asked to. Leaving it running is the default
  # because a cold boot is ~25s and re-runs are the common case; CI wants the
  # opposite, hence PS5UPLOAD_EMU_TEARDOWN=1.
  local preexisting; preexisting="$(emu_serial)"
  cmd_start
  local serial; serial="$(emu_serial)"
  [ -n "$serial" ] || die "emulator is not reporting a serial"
  if [ -z "$preexisting" ] && [ "${PS5UPLOAD_EMU_TEARDOWN:-0}" = "1" ]; then
    trap 'cmd_stop' EXIT INT TERM
  fi

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
  ensure) cmd_ensure ;;
  stop)   cmd_stop ;;
  status) cmd_status ;;
  test)   cmd_test ;;
  *) die "usage: $0 {create|start|ensure|stop|status|test}" ;;
esac
