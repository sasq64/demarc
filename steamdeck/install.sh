#!/bin/sh
# Install demarc on a Steam Deck over ssh (macOS / Linux).
#
#   ./install.sh deck@<address of the Deck>
#
# From the repo (steamdeck/install.sh) it runs package.sh first.
#
# Run it again with a newer demarc.tar.gz to update.
set -eu

if [ $# -ne 1 ]; then
    echo "Usage: $0 deck@<address of the Deck>" >&2
    exit 1
fi
dir=$(dirname "$0")
# In the repo rather than the unpacked zip: build the tarball first.
if [ -x "$dir/package.sh" ]; then
    "$dir/package.sh"
    dir=$dir/../target/steamdeck/demarc-steamdeck
fi
scp "$dir/demarc.tar.gz" "$1:demarc.tar.gz"
ssh "$1" "mkdir -p demarc && tar -xzf demarc.tar.gz -C demarc && rm demarc.tar.gz && bash demarc/setup.sh"
