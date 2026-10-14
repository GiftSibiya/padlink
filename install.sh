#!/bin/sh
# Install padlink for the current user, into ~/.local/bin:
#   padlink      the command line and rumble bridge (Rust)
#   padlink-gtk  the GTK app (Rust, links padlink-core; no Python)
set -eu
REPO="$(cd "$(dirname "$0")" && pwd)"
BIN="$HOME/.local/bin"
APPS="$HOME/.local/share/applications"
ART="$HOME/.local/share/padlink/art"
mkdir -p "$BIN" "$APPS" "$ART"

command -v cargo >/dev/null 2>&1 || {
    echo "cargo not found (Arch: sudo pacman -S rustup && rustup default stable)"
    exit 1
}
cargo build --release --manifest-path "$REPO/Cargo.toml" -p padlink-cli -p padlink-gtk
install -m755 "$REPO/target/release/padlink" "$BIN/padlink"
install -m755 "$REPO/target/release/padlink-gtk" "$BIN/padlink-gtk"
install -m644 "$REPO/data/padlink.desktop" "$APPS/padlink.desktop"
for f in "$REPO"/data/art-templates/*.svg; do
    install -m644 "$f" "$ART/$(basename "$f")"
done
echo "installed: $BIN/padlink, $BIN/padlink-gtk, $APPS/padlink.desktop, $ART"

if [ ! -e /etc/udev/rules.d/70-padlink.rules ]; then
    echo
    echo "For hidraw rumble on the GameSir/Zikway dongle, also run (needs sudo):"
    echo "  sudo install -m644 $REPO/data/70-padlink.rules /etc/udev/rules.d/ && sudo udevadm control --reload && sudo udevadm trigger"
fi
