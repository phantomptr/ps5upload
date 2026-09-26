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
#   * Otherwise generate an EPHEMERAL keystore so the artifact is at
#     least installable for sideloading. Caveat: each build's signature
#     differs, so a user must uninstall the previous ephemeral-signed
#     build before installing a newer one. A ::warning:: is emitted.
#
# Output: ./PS5Upload-android.apk (signed, zipaligned, verified) in CWD.

set -euo pipefail

apk_dir="client/src-tauri/gen/android/app/build/outputs/apk"

# Tauri emits the universal release APK as *-release-unsigned.apk. Fall
# back to any release apk if a future Tauri renames it.
# `-print -quit` (not `| head`) so `find` is never SIGPIPE-killed when the
# reader closes early — under `set -o pipefail` that 141 would abort the
# whole script. -quit stops at the first match.
unsigned="$(find "$apk_dir" -name '*-release-unsigned.apk' -print -quit)"
if [ -z "$unsigned" ]; then
  unsigned="$(find "$apk_dir" -name '*release*.apk' ! -name '*-signed.apk' -print -quit)"
fi
if [ -z "$unsigned" ]; then
  echo "::error::no release APK found under $apk_dir"
  find "$apk_dir" -name '*.apk' || true
  exit 1
fi
echo "Unsigned APK: $unsigned"

work="$(mktemp -d)"
keystore="$work/release.jks"

if [ -n "${ANDROID_KEYSTORE_BASE64:-}" ]; then
  echo "Signing with the release keystore from repo secrets."
  printf '%s' "$ANDROID_KEYSTORE_BASE64" | base64 -d >"$keystore"
  store_pass="${ANDROID_KEYSTORE_PASSWORD:?ANDROID_KEYSTORE_PASSWORD is required when ANDROID_KEYSTORE_BASE64 is set}"
  key_alias="${ANDROID_KEY_ALIAS:?ANDROID_KEY_ALIAS is required when ANDROID_KEYSTORE_BASE64 is set}"
  key_pass="${ANDROID_KEY_PASSWORD:-$store_pass}"
else
  echo "::warning::No ANDROID_KEYSTORE_BASE64 secret set — generating an EPHEMERAL keystore. The APK is installable for sideloading, but its signature changes every build, so users must uninstall a previous build before installing a newer one. Configure the keystore secrets for stable, updatable releases."
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

aligned="$work/aligned.apk"
"$bt/zipalign" -f -p 4 "$unsigned" "$aligned"

out="PS5Upload-android.apk"
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
