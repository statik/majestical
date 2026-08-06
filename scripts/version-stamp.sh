#!/usr/bin/env bash
# Stamp a version into the five files version-sync.sh checks. Build-time only:
# the release workflow runs this before building, and the result is never
# committed — git tags are the source of truth for released versions, and the
# versions committed to the repository stay frozen (docs/RELEASING.md).
set -euo pipefail
version="${1:?usage: version-stamp.sh <version>}"
cd "$(dirname "$0")/.."

stamp_toml() {
  local file="$1"
  local count
  count=$(grep -c '^version = ' "$file")
  if [[ "$count" != 1 ]]; then
    echo "expected exactly one '^version = ' line in $file, found $count" >&2
    echo "fix: teach version-stamp.sh the file's new shape before trusting it" >&2
    exit 1
  fi
  # -i.bak is the portable in-place form; BSD sed (macOS CI) rejects a bare -i.
  sed -i.bak "s/^version = \".*\"/version = \"$version\"/" "$file"
  rm "$file.bak"
}

# sed rather than jq: jq rewrites the whole file in its own style, and a
# format-preserving edit keeps a local run of this script from dirtying
# anything but version lines.
stamp_json() {
  local file="$1"
  local count
  count=$(grep -c '^  "version": "' "$file")
  if [[ "$count" != 1 ]]; then
    echo "expected exactly one '  \"version\":' line in $file, found $count" >&2
    echo "fix: teach version-stamp.sh the file's new shape before trusting it" >&2
    exit 1
  fi
  sed -i.bak "s/^  \"version\": \".*\"/  \"version\": \"$version\"/" "$file"
  rm "$file.bak"
}

stamp_toml Cargo.toml
stamp_toml apps/desktop/src-tauri/Cargo.toml
stamp_json apps/desktop/package.json
stamp_json apps/desktop/src-tauri/tauri.conf.json

# The lockfiles record each workspace member's version. Only path-local
# crates change, so this needs no network and no build.
cargo update --workspace --offline
cargo update --workspace --offline --manifest-path apps/desktop/src-tauri/Cargo.toml

./scripts/version-sync.sh
