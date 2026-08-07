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

# The lockfiles record each workspace member's version. `--workspace` limits
# the update to the path-local crates, so registry dependencies stay locked;
# no build happens. Not `--offline`: resolution still reads the crates.io
# index, which a fresh CI runner has never cached — the v0.2.0-rc1 dry run
# failed exactly there.
cargo update --workspace
cargo update --workspace --manifest-path apps/desktop/src-tauri/Cargo.toml

./scripts/version-sync.sh
