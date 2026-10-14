#!/bin/sh
# Install padlink for the current user without pip: wrapper scripts in ~/.local/bin
# that run this checkout with the system Python (which has python-evdev, pyudev, gi).
set -eu
REPO="$(cd "$(dirname "$0")" && pwd)"
BIN="$HOME/.local/bin"
APPS="$HOME/.local/share/applications"
mkdir -p "$BIN" "$APPS"

for dep in evdev pyudev yaml gi; do
    /usr/bin/python3 -c "import $dep" 2>/dev/null || {
        echo "missing python module '$dep' (Arch: sudo pacman -S python-evdev python-pyudev python-yaml python-gobject gtk4 libadwaita)"
        exit 1
    }
done

cat > "$BIN/padlink" <<SH
#!/bin/sh
PYTHONPATH="$REPO\${PYTHONPATH:+:\$PYTHONPATH}" exec /usr/bin/python3 -m padlink "\$@"
SH
cat > "$BIN/padlink-gtk" <<SH
#!/bin/sh
PYTHONPATH="$REPO\${PYTHONPATH:+:\$PYTHONPATH}" exec /usr/bin/python3 -m padlink.gtk.app "\$@"
SH
chmod +x "$BIN/padlink" "$BIN/padlink-gtk"
install -m644 "$REPO/data/padlink.desktop" "$APPS/padlink.desktop"

echo "installed: $BIN/padlink, $BIN/padlink-gtk, $APPS/padlink.desktop"
if [ ! -e /etc/udev/rules.d/70-padlink.rules ]; then
    echo
    echo "For hidraw rumble on the GameSir/Zikway dongle, also run (needs sudo):"
    echo "  sudo install -m644 $REPO/data/70-padlink.rules /etc/udev/rules.d/ && sudo udevadm control --reload && sudo udevadm trigger"
fi
