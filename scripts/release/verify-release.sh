#!/usr/bin/env bash
#
# Check a published GitHub release before promoting it.
#
#   scripts/release/verify-release.sh vX.Y.Z            # check only
#   scripts/release/verify-release.sh vX.Y.Z --promote  # check, then mark it the latest release
#
# Checks that the release exists, is still a pre-release (publish.yml always creates one),
# and carries every asset publish.yml uploads, none of them empty. The expected names are
# read from publish.yml itself (the "Every expected asset is present" list, the optional
# phone builds it copies, the manifests it writes with --out, and the LICENSE it copies in), so
# this cannot drift from the workflow, and the two counts on the last line agree on a complete
# release. The Android APKs and the iPhone IPA are optional there (their jobs are
# continue-on-error) and only warn here.
#
# --promote runs `gh release edit vX.Y.Z --prerelease=false --latest` once every check
# passes, and refuses a version older than the current latest release. That also fires docker-engine.yml's `promote` job (:latest and :<major.minor>), and it
# redeploys the website (its release snapshot and iPhone source list promoted releases only).
# Promote only after the release has been tried on real hardware.

set -euo pipefail

usage() { echo "usage: $0 vX.Y.Z [--promote]" >&2; exit 2; }
tag="${1:-}"
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+._A-Za-z0-9]*)?$ ]] || usage
promote=0
case "${2:-}" in
  "") ;;
  --promote) promote=1 ;;
  *) usage ;;
esac
version="${tag#v}"

cd "$(dirname "$0")/../.."
workflow=.github/workflows/publish.yml

# Required: the quoted names in the presence check, one per line.
required="$(grep -oE '^ *"(PS5Upload|ps5upload)[^"$]*\$\{TAG_VERSION\}[^"$]*"' "$workflow" \
  | tr -d ' "' | sed "s/\${TAG_VERSION}/$version/")"
# Optional phone builds: the fixed release/ names publish.yml copies them to (no ${arch}).
optional="$(grep -oE '"release/(PS5Upload|ps5upload)[^"$]*\$\{TAG_VERSION\}[^"$]*"' "$workflow" \
  | tr -d '"' | sed -e 's#^release/##' -e "s/\${TAG_VERSION}/$version/" | sort -u \
  | grep -vxF -f <(printf '%s\n' "$required"))"
# Generated manifests (--out release/<name>).
manifests="$(grep -oE -- '--out release/[A-Za-z0-9._-]+' "$workflow" | sed 's#--out release/##' | sort -u)"
# Files copied in under a fixed name (cp ... release/<name>: the LICENSE that ships with the binaries).
fixed="$(grep -oE 'cp( -[a-z]+)* [A-Za-z0-9._/-]+ release/[A-Za-z0-9._-]+$' "$workflow" | sed 's#.* release/##' | sort -u)"
if ! grep -qxF LICENSE <<<"$fixed"; then
  echo "verify-release: $workflow no longer copies LICENSE into release/; did that step change shape?" >&2
  exit 2
fi

n_required="$(printf '%s\n' "$required" | grep -c .)"
if (( n_required < 20 )); then
  echo "verify-release: read only $n_required required names from $workflow; did its asset list change shape?" >&2
  exit 2
fi

json="$(gh release view "$tag" --json isPrerelease,isDraft,url,assets 2>&1)" || {
  echo "verify-release: no release $tag ($json)" >&2
  exit 1
}

fail=0
warn=0
if [[ "$(jq -r .isDraft <<<"$json")" == true ]]; then
  echo "FAIL  $tag is a draft"; fail=1
fi
if [[ "$(jq -r .isPrerelease <<<"$json")" != true ]]; then
  echo "note  $tag is not a pre-release (already promoted?)"
fi

# name<TAB>size for every asset on the release.
assets="$(jq -r '.assets[] | "\(.name)\t\(.size)"' <<<"$json")"
size_of() { awk -F'\t' -v n="$1" '$1 == n { print $2; found=1 } END { if (!found) print "" }' <<<"$assets"; }
human() { awk -v b="$1" 'BEGIN { split("B KiB MiB GiB", u); i=1; while (b >= 1024 && i < 4) { b /= 1024; i++ } printf (i == 1 ? "%d %s" : "%.1f %s"), b, u[i] }'; }

check() { # name, kind (required|optional)
  local size; size="$(size_of "$1")"
  if [[ -z "$size" ]]; then
    if [[ "$2" == optional ]]; then echo "warn  missing (optional): $1"; warn=1
    else echo "FAIL  missing: $1"; fail=1; fi
  elif [[ "$size" == 0 ]]; then
    echo "FAIL  empty: $1"; fail=1
  else
    printf 'ok    %10s  %s\n' "$(human "$size")" "$1"
  fi
}

expected=0
while IFS= read -r n; do [[ -n "$n" ]] && { check "$n" required; expected=$((expected + 1)); }; done <<<"$required"
while IFS= read -r n; do [[ -n "$n" ]] && { check "$n" required; expected=$((expected + 1)); }; done <<<"$manifests"
while IFS= read -r n; do [[ -n "$n" ]] && { check "$n" required; expected=$((expected + 1)); }; done <<<"$fixed"
while IFS= read -r n; do [[ -n "$n" ]] && { check "$n" optional; expected=$((expected + 1)); }; done <<<"$optional"

extra="$(cut -f1 <<<"$assets" | grep -vxF -f <(printf '%s\n' "$required" "$manifests" "$fixed" "$optional" | grep .) || true)"
if [[ -n "$extra" ]]; then
  echo "note  not in publish.yml's list (left alone):"
  while IFS= read -r n; do echo "        $n"; done <<<"$extra"
fi

total="$(grep -c . <<<"$assets" || true)"
echo "---"
echo "$tag: $total assets on the release, $expected expected ($(jq -r .url <<<"$json"))"
if (( fail )); then
  echo "verify-release: $tag is NOT complete" >&2
  exit 1
fi
(( warn )) && echo "verify-release: complete apart from the optional phone builds above"
(( warn )) || echo "verify-release: complete"

if (( promote )); then
  # Never backwards. `--latest` moves GitHub's "latest" pointer even onto an older version, and
  # everything stable follows that pointer: the desktop updater (releases/latest/download/
  # latest.json), the iPhone source, the website, and docker-engine.yml's promote job, whose
  # own never-backwards check compares against that same pointer and so would move :latest
  # back too. Promoting an old pre-release after a newer one is out is a mistake to stop here.
  latest_err="$(mktemp)"
  if latest_tag="$(gh api "repos/{owner}/{repo}/releases/latest" --jq .tag_name 2>"$latest_err")"; then
    latest="${latest_tag#v}"
    if [[ "$latest" != "$version" ]] \
        && [[ "$(printf '%s\n%s\n' "$version" "$latest" | sort -V | head -n 1)" == "$version" ]]; then
      rm -f "$latest_err"
      echo "verify-release: NOT promoted: $tag is older than the latest release, $latest_tag." >&2
      echo "  Promoting it would move the stable channel (updater, iPhone source, site, :latest) back." >&2
      echo "  To publish it without moving latest: gh release edit $tag --prerelease=false --latest=false" >&2
      exit 1
    fi
  elif ! grep -q 'HTTP 404' "$latest_err"; then
    # No answer is not "there is no latest": do not guess.
    cat "$latest_err" >&2
    rm -f "$latest_err"
    echo "verify-release: NOT promoted: could not read the latest release (above)" >&2
    exit 1
  fi
  rm -f "$latest_err"
  gh release edit "$tag" --prerelease=false --latest
  echo "promoted $tag to the latest release"
  # The website's release snapshot and the iPhone (AltStore) source list promoted releases only:
  # the deploy that publish started ran while this was still a pre-release.
  gh workflow run pages.yml --ref main \
    && echo "started the website deploy (pages.yml) for the promoted release" \
    || echo "warn  could not start pages.yml; run: gh workflow run pages.yml --ref main"
fi
