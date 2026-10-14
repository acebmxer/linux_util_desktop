#!/usr/bin/env bash
# Replaces vendor/ with linux_util.sh, lib/ and LICENSE from a linux_util tag
# and records the tag in vendor/linux_util.VERSION.
#
# Usage: scripts/sync_linux_util.sh <tag>      e.g. scripts/sync_linux_util.sh v1.9.0
# Set LINUX_UTIL_REPO to sync from another clone URL or local path.
set -euo pipefail

REPO="${LINUX_UTIL_REPO:-https://github.com/acebmxer/linux_util.git}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR="$ROOT/vendor"

die() { echo "sync_linux_util: $*" >&2; exit 1; }

[[ $# -eq 1 ]] || die "usage: $0 <tag>  (e.g. v1.9.0)"
TAG="$1"
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "tag must look like v1.2.3, got '$TAG'"
command -v git >/dev/null || die "git is required"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

git clone --quiet --depth 1 --branch "$TAG" "$REPO" "$WORK/src" 2>"$WORK/clone.err" \
    || die "could not fetch tag $TAG from $REPO: $(cat "$WORK/clone.err")"
# --branch also accepts branch names; make sure this really is the tag.
git -C "$WORK/src" rev-parse --verify --quiet "refs/tags/$TAG" >/dev/null \
    || die "$TAG is not a tag in $REPO"

mkdir "$WORK/out"
git -C "$WORK/src" archive "$TAG" linux_util.sh lib LICENSE | tar -x -C "$WORK/out" \
    || die "$TAG is missing linux_util.sh, lib/ or LICENSE"

# Replace only what the sync owns; vendor/logs/ (local runs) is left alone.
mkdir -p "$VENDOR"
rm -rf "${VENDOR:?}/linux_util.sh" "${VENDOR:?}/lib" "${VENDOR:?}/LICENSE"
cp -a "$WORK/out/." "$VENDOR/"
echo "$TAG" > "$VENDOR/linux_util.VERSION"

echo "Vendored linux_util $TAG into $VENDOR"
echo "Next: npm run contract"
