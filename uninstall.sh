#!/usr/bin/env bash
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
USER_CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/wyrd"
USER_STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/wyrd"
PURGE=false
KEEP_ACLS=false

print_usage() {
    cat <<EOF
Usage: ./uninstall.sh [--purge] [--keep-acls]

  --purge      Also remove the system greeter and wallpaper snapshots
  --keep-acls  Keep greeter ACLs previously granted by install.sh
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --purge)
            PURGE=true
            shift
            ;;
        --keep-acls)
            KEEP_ACLS=true
            shift
            ;;
        -h|--help)
            print_usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            print_usage >&2
            exit 1
            ;;
    esac
done

needs_root=false
if [ -e /usr/bin/wyrd-greet ] || [ "$PURGE" = true ]; then
    needs_root=true
fi

if [ "$needs_root" = true ] && [ "$(id -u)" -ne 0 ]; then
    if ! command -v sudo >/dev/null 2>&1; then
        echo "Error: sudo is required to remove system files." >&2
        exit 1
    fi
    if ! sudo -n true 2>/dev/null; then
        if [ ! -t 0 ]; then
            echo "Error: non-interactive uninstall requires cached sudo credentials; run 'sudo -v' first." >&2
            exit 1
        fi
        sudo -v
    fi
fi

run_root() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    else
        sudo "$@"
    fi
}

rm -f "$BIN_DIR/wyrd-greet"
if [ -e /usr/bin/wyrd-greet ]; then
    run_root rm -f /usr/bin/wyrd-greet
fi

if [ "$KEEP_ACLS" = false ] && command -v setfacl >/dev/null 2>&1; then
    for path in "$HOME" "$HOME/.config" "$HOME/.local" "$HOME/.local/state" "$HOME/Pictures"; do
        if [ -e "$path" ]; then
            setfacl -x u:greeter "$path" 2>/dev/null || true
        fi
    done
    for path in "$USER_CFG_DIR" "$USER_STATE_DIR" "$HOME/Pictures/Wallpapers"; do
        if [ -d "$path" ]; then
            setfacl -R -x u:greeter "$path" 2>/dev/null || true
        fi
    done
fi

if [ "$PURGE" = true ]; then
    run_root rm -f /etc/wyrd/greeter.lua /etc/wyrd/wallpaper.toml /var/lib/wyrd/wallpaper.toml
    run_root rmdir /etc/wyrd /var/lib/wyrd 2>/dev/null || true
fi

echo "==> wyrd-greet uninstalled."
echo "    Note: if /etc/greetd/config.toml still references 'wyrd-greet', update it before reboot."
