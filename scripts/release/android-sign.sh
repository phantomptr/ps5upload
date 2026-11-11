#!/usr/bin/env bash
#
# Sign the Tauri-built release APK so it is installable. Called by the
# `build-android` job in .github/workflows/publish.yml.
#
# Why sign here instead of in Gradle: Tauri's generated
# `gen/android/app/build.gradle.kts` ships with NO signing config, and
# `tauri android init` regenerates that file on every run — so patching
# it in CI would be fragile. Instead we let Tauri produce the
# `*-release-unsigned.apk` and sign it ourselves with the SDK's
# `apksigner` (after `zipalign`, which v2+ signatures require first).
#
# Keystore selection:
#   * If ANDROID_KEYSTORE_BASE64 (+ password/alias) repo secrets are set,
#     use that keystore — a STABLE signature, so users get seamless
#     in-place updates across releases. This is what you want for a real
#     distribution channel.
#   * Otherwise the script FAILS: an APK signed with any other key cannot
#     update an installed app, so a release must never get one quietly.
#   * Only with ANDROID_ALLOW_THROWAWAY_KEY=1 (local and test builds, never
#     the publish workflow) does it generate an EPHEMERAL keystore so the
#     artifact is installable for sideloading. Each build's signature
#     differs, so the previous build must be uninstalled first. A
#     ::warning:: is emitted.
#
# Output: ./PS5Upload-android.apk (arm64) and ./PS5Upload-android-armv7.apk
# (signed, zipaligned, verified) in CWD.

set -euo pipefail

apk_dir="client/src-tauri/gen/android/app/build/outputs/apk"

# publish.yml builds one APK per ABI (--split-per-abi): arm64 ships as the plain
# PS5Upload-android.apk (what nearly every phone and tablet needs, and the name the
# updater and the site already use), 32-bit ARM as PS5Upload-android-armv7.apk.
# `-print -quit` (not `| head`) so `find` is never SIGPIPE-killed under pipefail.
unsigned_for() {
  find "$apk_dir/$1" -name '*-release-unsigned.apk' -print -quit 2>/dev/null
}
arm64="$(unsigned_for arm64)"
armv7="$(unsigned_for arm)"
if [ -z "$arm64" ]; then
  echo "::error::no arm64 release APK found under $apk_dir"
  find "$apk_dir" -name '*.apk' || true
  exit 1
fi
echo "Unsigned APKs: $arm64 ${armv7:-(no armv7)}"

work="$(mktemp -d)"
keystore="$work/release.jks"

if [ -n "${ANDROID_KEYSTORE_BASE64:-}" ]; then
  echo "Signing with the release keystore from repo secrets."
  printf '%s' "$ANDROID_KEYSTORE_BASE64" | base64 -d >"$keystore"
  store_pass="${ANDROID_KEYSTORE_PASSWORD:?ANDROID_KEYSTORE_PASSWORD is required when ANDROID_KEYSTORE_BASE64 is set}"
  key_alias="${ANDROID_KEY_ALIAS:?ANDROID_KEY_ALIAS is required when ANDROID_KEYSTORE_BASE64 is set}"
  key_pass="${ANDROID_KEY_PASSWORD:-$store_pass}"
elif [ "${ANDROID_ALLOW_THROWAWAY_KEY:-}" != 1 ]; then
  # A release APK signed with any other key cannot update the app people already have: Android
  # refuses it ("App not installed") until they uninstall, which deletes their settings. Shipping
  # no APK (build-android is not a gate) is the smaller harm, so this stops instead of warning.
  echo "::error::ANDROID_KEYSTORE_BASE64 is not set: refusing to sign a release APK with a throwaway key (it could not update installed apps). Restore the ANDROID_KEYSTORE_* repo secrets. For a local or test build only, set ANDROID_ALLOW_THROWAWAY_KEY=1."
  exit 1
else
  echo "::warning::ANDROID_ALLOW_THROWAWAY_KEY=1 and no ANDROID_KEYSTORE_BASE64 — generating an EPHEMERAL keystore. The APK is installable for sideloading, but its signature changes every build, so a previous build must be uninstalled before installing this one. Not for a release."
  store_pass="ps5upload"
  key_pass="ps5upload"
  key_alias="ps5upload"
  keytool -genkeypair -keystore "$keystore" -alias "$key_alias" \
    -keyalg RSA -keysize 2048 -validity 10000 \
    -storepass "$store_pass" -keypass "$key_pass" \
    -dname "CN=ps5upload, O=phantomptr, C=US"
fi

# Newest installed build-tools (provides zipalign + apksigner).
bt="$(ls -d "$ANDROID_HOME"/build-tools/* 2>/dev/null | sort -V | tail -1)"
if [ -z "$bt" ]; then
  echo "::error::no Android build-tools found under $ANDROID_HOME/build-tools"
  exit 1
fi
echo "Using build-tools: $bt"

sign_one() {
  local unsigned="$1" out="$2" aligned="$work/aligned.apk"
  "$bt/zipalign" -f -p 4 "$unsigned" "$aligned"
  "$bt/apksigner" sign \
    --ks "$keystore" \
    --ks-key-alias "$key_alias" \
    --ks-pass "pass:$store_pass" \
    --key-pass "pass:$key_pass" \
    --out "$out" \
    "$aligned"
  "$bt/apksigner" verify --verbose "$out"
  echo "✓ signed APK: $out"
  ls -la "$out"
}

sign_one "$arm64" PS5Upload-android.apk
if [ -n "$armv7" ]; then
  sign_one "$armv7" PS5Upload-android-armv7.apk
else
  echo "::warning::no armv7 APK was built; the release ships arm64 only"
fi
