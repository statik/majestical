#!/usr/bin/env bash
# Clamp a computed semantic version to the 0.x range.
#
# github-tag-action sends 0.x + a breaking change to 1.0.0; we stay in 0.x
# until 1.0.0 is shipped deliberately. When the previous version is 0.x and
# the computed version is not, replace it with a minor bump of the previous
# version (0.<minor>.<patch> -> 0.<minor+1>.0). Otherwise pass the computed
# version through unchanged.
#
# Usage: clamp-version.sh <previous_version> <computed_version>
# Inputs must be bare semver (no leading "v"); the caller strips any prefix.
set -euo pipefail

prev="${1:?previous_version required}"
computed="${2:?computed_version required}"

prev_major="${prev%%.*}"
computed_major="${computed%%.*}"

if [ "$prev_major" = "0" ] && [ "$computed_major" != "0" ]; then
  rest="${prev#*.}"        # <minor>.<patch>
  prev_minor="${rest%%.*}" # <minor>
  printf '0.%s.0\n' "$((prev_minor + 1))"
else
  printf '%s\n' "$computed"
fi
