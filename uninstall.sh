#!/usr/bin/env bash
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"

rm -f "$BIN_DIR/wyrd-greet"
echo "Removed $BIN_DIR/wyrd-greet"
