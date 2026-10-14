#!/bin/sh
# Install a padlink release tarball.
#   ./install.sh                     for this user, into ~/.local
#   sudo ./install.sh --prefix /usr/local   system-wide (also installs the udev rule)
#   ./install.sh --uninstall [--prefix DIR]
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
PREFIX="$HOME/.local"
UNINSTALL=0
while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) PREFIX="$2"; shift 2 ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help) sed -n '2,5p' "$0"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

FILES="bin/padlink bin/padlink-gtk share/applications/padlink.desktop"
if [ "$UNINSTALL" = 1 ]; then
    for f in $FILES; do rm -f "$PREFIX/$f"; done
    rm -rf "$PREFIX/share/padlink"
    echo "removed padlink from $PREFIX"
    exit 0
fi

install -Dm755 "$HERE/bin/padlink" "$PREFIX/bin/padlink"
install -Dm755 "$HERE/bin/padlink-gtk" "$PREFIX/bin/padlink-gtk"
install -Dm644 "$HERE/share/applications/padlink.desktop" "$PREFIX/share/applications/padlink.desktop"
mkdir -p "$PREFIX/share/padlink/art"
install -m644 "$HERE"/share/padlink/art/*.svg "$PREFIX/share/padlink/art/"
echo "installed padlink $(cat "$HERE/CHANGELOG.md" | sed -n 's/^## \[\(.*\)\].*/\1/p' | head -n1) into $PREFIX"

RULE=/etc/udev/rules.d/70-padlink.rules
if [ "$(id -u)" = 0 ]; then
    install -Dm644 "$HERE/lib/udev/rules.d/70-padlink.rules" "$RULE"
    udevadm control --reload 2>/dev/null || true
    echo "installed $RULE (replug the dongle)"
elif [ ! -e "$RULE" ]; then
    echo
    echo "For hidraw rumble on the GameSir/Zikway dongle, also run (needs sudo):"
    echo "  sudo install -m644 $HERE/lib/udev/rules.d/70-padlink.rules /etc/udev/rules.d/ && sudo udevadm control --reload && sudo udevadm trigger"
fi
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "note: $PREFIX/bin is not on your PATH" ;;
esac
