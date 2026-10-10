#!/bin/sh
# Install demarc on a Steam Deck over ssh (macOS / Linux).
#
#   ./install.sh deck@<address of the Deck>
#
# Run it again with a newer demarc.tar.gz to update.
set -eu

if [ $# -ne 1 ]; then
    echo "Usage: $0 deck@<address of the Deck>" >&2
    exit 1
fi
scp "$(dirname "$0")/demarc.tar.gz" "$1:demarc.tar.gz"
ssh "$1" "mkdir -p demarc && tar -xzf demarc.tar.gz -C demarc && rm demarc.tar.gz && bash demarc/setup.sh"
