#!/usr/bin/env bash
# =============================================================================
# Wyrd Greet - Session Locker & greetd Display Manager Greeter Installer
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

USER_BIN_DIR="${PREFIX:-$HOME/.local}/bin"
USER_CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/wyrd"
USER_STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/wyrd"
FORCE_BUILD=false
DRY_RUN=false
NO_SYSTEMD=false
NON_INTERACTIVE=false
GRANT_ACLS=false

print_usage() {
    cat <<EOF
Wyrd Greet Installer

Usage:
  ./install.sh [options]

Options:
      --bin-dir <DIR>    User binary directory (default: ~/.local/bin)
  -b, --build            Force rebuild from source (cargo build --release --locked)
      --grant-acls       Grant POSIX ACL read access on ~/.config/wyrd and ~/.local/state/wyrd to 'greeter'
      --no-systemd       Compatibility flag (no-op; wyrd-greet runs via greetd or wyrd-idle)
  -y, --yes              Run non-interactively with safe defaults (/etc/wyrd copy, no home ACLs)
      --dry-run          Preview planned actions without modifying any files
  -h, --help             Show this help message
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --bin-dir)
            USER_BIN_DIR="$2"
            shift 2
            ;;
        --bin-dir=*)
            USER_BIN_DIR="${1#*=}"
            shift
            ;;
        -b|--build)
            FORCE_BUILD=true
            shift
            ;;
        --grant-acls)
            GRANT_ACLS=true
            shift
            ;;
        --no-systemd)
            NO_SYSTEMD=true
            shift
            ;;
        -y|--yes|--non-interactive)
            NON_INTERACTIVE=true
            shift
            ;;
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        -h|--help)
            print_usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            print_usage
            exit 1
            ;;
    esac
done

run_cmd() {
    if [ "$DRY_RUN" = true ]; then
        echo "[dry-run] $*"
    else
        "$@"
    fi
}

SRC_BIN=""
if [ -f "$SCRIPT_DIR/bin/wyrd-greet" ] && [ "$FORCE_BUILD" = false ]; then
    SRC_BIN="$SCRIPT_DIR/bin/wyrd-greet"
elif [ -f "$SCRIPT_DIR/target/release/wyrd-greet" ] && [ "$FORCE_BUILD" = false ]; then
    SRC_BIN="$SCRIPT_DIR/target/release/wyrd-greet"
fi

if [ -z "$SRC_BIN" ]; then
    echo "==> Building wyrd-greet (release --locked)..."
    run_cmd cargo build --release --locked --manifest-path "$SCRIPT_DIR/Cargo.toml"
    SRC_BIN="$SCRIPT_DIR/target/release/wyrd-greet"
fi

run_cmd mkdir -p "$USER_BIN_DIR"
run_cmd install -Dm755 "$SRC_BIN" "$USER_BIN_DIR/wyrd-greet"
echo "==> Installed user binary (session lock): $USER_BIN_DIR/wyrd-greet"

if [ ! -f "$USER_CFG_DIR/greeter.lua" ] && [ -f "$SCRIPT_DIR/examples/greeter.lua" ]; then
    run_cmd mkdir -p "$USER_CFG_DIR"
    run_cmd install -Dm644 "$SCRIPT_DIR/examples/greeter.lua" "$USER_CFG_DIR/greeter.lua"
    echo "==> Created default config: $USER_CFG_DIR/greeter.lua"
fi

install_system_files() {
    local sudo_prefix=()
    if [ "$(id -u)" -ne 0 ]; then
        sudo_prefix=(sudo)
    fi
    run_cmd "${sudo_prefix[@]}" install -Dm755 "$SRC_BIN" /usr/bin/wyrd-greet
    run_cmd "${sudo_prefix[@]}" install -dm755 /etc/wyrd /var/lib/wyrd
    if [ -f "$USER_CFG_DIR/greeter.lua" ]; then
        run_cmd "${sudo_prefix[@]}" install -Dm644 "$USER_CFG_DIR/greeter.lua" /etc/wyrd/greeter.lua
    elif [ -f "$SCRIPT_DIR/examples/greeter.lua" ]; then
        run_cmd "${sudo_prefix[@]}" install -Dm644 "$SCRIPT_DIR/examples/greeter.lua" /etc/wyrd/greeter.lua
    fi
    if [ -f "$USER_STATE_DIR/wallpaper.toml" ]; then
        run_cmd "${sudo_prefix[@]}" install -Dm644 "$USER_STATE_DIR/wallpaper.toml" /etc/wyrd/wallpaper.toml
        run_cmd "${sudo_prefix[@]}" install -Dm644 "$USER_STATE_DIR/wallpaper.toml" /var/lib/wyrd/wallpaper.toml
    fi
    echo "==> Installed system binary (/usr/bin/wyrd-greet) and synced default state to /etc/wyrd/"
}

if [ "$(id -u)" -eq 0 ] || { [ -d /etc/greetd ] && command -v sudo >/dev/null 2>&1; }; then
    if [ "$DRY_RUN" = true ]; then
        echo "[dry-run] install /usr/bin/wyrd-greet and copy config/wallpaper.toml to /etc/wyrd/"
    else
        install_system_files
    fi
fi

if [ "$GRANT_ACLS" = false ] && [ "$NON_INTERACTIVE" = false ] && [ -t 0 ] && command -v setfacl >/dev/null 2>&1; then
    read -rp "Grant 'greeter' user POSIX ACL read access to ~/.local/state/wyrd and ~/Pictures/Wallpapers? [y/N]: " ans
    if [[ "$ans" =~ ^[Yy] ]]; then
        GRANT_ACLS=true
    fi
fi

if [ "$GRANT_ACLS" = true ]; then
    if command -v setfacl >/dev/null 2>&1; then
        echo "==> Granting POSIX ACLs for 'greeter' user..."
        run_cmd setfacl -m u:greeter:x "$HOME" "$HOME/.config" "$HOME/.local" "$HOME/.local/state"
        [ -d "$HOME/Pictures" ] && run_cmd setfacl -m u:greeter:x "$HOME/Pictures"
        [ -d "$USER_CFG_DIR" ] && run_cmd setfacl -R -m u:greeter:rx "$USER_CFG_DIR"
        [ -d "$USER_STATE_DIR" ] && run_cmd setfacl -R -m u:greeter:rx "$USER_STATE_DIR"
        [ -d "$HOME/Pictures/Wallpapers" ] && run_cmd setfacl -R -m u:greeter:rx "$HOME/Pictures/Wallpapers"
    else
        echo "Warning: 'setfacl' not found; skipping ACL setup (system cache in /etc/wyrd/ will be used)."
    fi
fi
