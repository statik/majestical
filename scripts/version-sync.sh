#!/usr/bin/env bash
# One version, five files: the root workspace, the GUI npm package, the GUI
# cargo package, tauri.conf.json (what the updater/bundler stamp), and the GUI
# lockfile.
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
  echo "fix: set them all to the same version before tagging a release" >&2
  exit 1
fi

# The GUI lockfile records majestical-desktop's own version, and bumping
# Cargo.toml does not rewrite it — only a build does. Left alone, a tag can
# ship a lockfile still naming the previous version, and CI's
# `--locked`-adjacent steps disagree with the manifest. Checked here so the
# same gate that guards a release catches it.
# `|| true`: with no matching entry the greps exit non-zero, and under
# `set -e` that would kill the script before the message below explains why.
lock=$(grep -A1 '^name = "majestical-desktop"$' apps/desktop/src-tauri/Cargo.lock |
  grep -m1 '^version' | cut -d'"' -f2 || true)
if [[ "$root" != "$lock" ]]; then
  echo "version mismatch: workspace=$root" \
    "src-tauri/Cargo.lock=${lock:-<no majestical-desktop entry>}" >&2
  echo "fix: run cargo build --manifest-path apps/desktop/src-tauri/Cargo.toml after bumping" >&2
  exit 1
fi
echo "version-sync ok: $root"
