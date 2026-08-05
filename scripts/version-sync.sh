#!/usr/bin/env bash
# One version, four files: the root workspace, the GUI npm package, the GUI
# cargo package, and tauri.conf.json (what the updater/bundler stamp).
set -euo pipefail
cd "$(dirname "$0")/.."
root=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
npm_pkg=$(jq -r .version apps/desktop/package.json)
conf=$(jq -r .version apps/desktop/src-tauri/tauri.conf.json)
gui=$(grep -m1 '^version' apps/desktop/src-tauri/Cargo.toml | cut -d'"' -f2)
# Separate tests rather than one [[ ]] joining them with ||: that combined form
# trips shellcheck's SC2055, and these read as "any of the three drifted".
if [[ "$root" != "$npm_pkg" ]] || [[ "$root" != "$conf" ]] || [[ "$root" != "$gui" ]]; then
  echo "version mismatch: workspace=$root package.json=$npm_pkg tauri.conf.json=$conf src-tauri=$gui" >&2
  echo "fix: set all four to the same version before tagging a release" >&2
  exit 1
fi
echo "version-sync ok: $root"
