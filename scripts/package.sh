#!/bin/sh
# Build the release artifacts into dist/:
#   padlink-<version>-<arch>-linux.tar.gz (+ .sha256)   prebuilt binaries, data, install.sh
#   padlink_<version>-1_<debarch>.deb                    when cargo-deb is installed
# Used by .github/workflows/release.yml; also works locally.
set -eu
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
ARCH=$(uname -m)
NAME="padlink-$VERSION-$ARCH-linux"
STAGE="dist/$NAME"

cargo build --release --locked -p padlink-cli -p padlink-gtk

rm -rf dist
mkdir -p "$STAGE/bin" "$STAGE/share/applications" "$STAGE/share/padlink/art" "$STAGE/lib/udev/rules.d"
install -m755 target/release/padlink target/release/padlink-gtk "$STAGE/bin/"
install -m644 data/padlink.desktop "$STAGE/share/applications/"
install -m644 data/art-templates/*.svg "$STAGE/share/padlink/art/"
install -m644 data/70-padlink.rules "$STAGE/lib/udev/rules.d/"
install -m644 README.md LICENSE CHANGELOG.md "$STAGE/"
install -m755 packaging/install-binary.sh "$STAGE/install.sh"
tar -C dist -czf "dist/$NAME.tar.gz" "$NAME"
(cd dist && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
rm -rf "$STAGE"
echo "built dist/$NAME.tar.gz"

if command -v cargo-deb >/dev/null 2>&1; then
    cargo deb --no-build -p padlink-cli -o dist/
    (cd dist && for d in *.deb; do sha256sum "$d" > "$d.sha256"; done)
else
    echo "cargo-deb not installed: skipping the .deb (cargo install cargo-deb)"
fi
