#!/usr/bin/env bash
# Download the demo databases that ship with a release, into $1 (default: cwd).
#
# The dumps are ~8MB each and are regenerated regularly, so they live on the
# website rather than in git. The release workflow fetches them twice: once in
# the global build job, where dist publishes them as individual release assets
# (see `extra-artifacts` in dist-workspace.toml), and once on the Windows runner,
# which puts them in the zip next to CSDB.BAT / DEMOZOO.BAT.
set -euo pipefail

out="${1:-.}"
mkdir -p "$out"
for db in csdb demozoo; do
    curl --proto '=https' --tlsv1.2 -fLsS --retry 3 \
        -o "$out/$db.txt.gz" "https://minnberg.se/dl/$db.txt.gz"
done
