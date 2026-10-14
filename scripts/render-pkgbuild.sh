#!/bin/sh
# Write a stable Arch PKGBUILD + .SRCINFO for a release tag into OUTDIR, with the checksum of
# GitHub's source tarball for that tag (or of a local tarball, for testing).
#   scripts/render-pkgbuild.sh v0.1.0 dist/ [local-source.tar.gz]
set -eu
TAG="$1"
OUT="$2"
VERSION="${TAG#v}"
cd "$(dirname "$0")/.."
mkdir -p "$OUT"
if [ $# -ge 3 ]; then
    SRC="$3"
else
    SRC="$OUT/padlink-$VERSION-source.tar.gz"
    curl -fsSL -o "$SRC" "https://github.com/GiftSibiya/padlink/archive/refs/tags/$TAG.tar.gz"
fi
SHA=$(sha256sum "$SRC" | cut -d' ' -f1)
sed -e "s/@VERSION@/$VERSION/g" -e "s/@SHA256@/$SHA/g" packaging/arch/PKGBUILD.in > "$OUT/PKGBUILD"
cat > "$OUT/.SRCINFO" <<SRCINFO
pkgbase = padlink
	pkgdesc = Detect, debug and bridge rumble for wireless game controllers
	pkgver = $VERSION
	pkgrel = 1
	url = https://github.com/GiftSibiya/padlink
	arch = x86_64
	license = MIT
	makedepends = cargo
	depends = gtk4
	depends = libadwaita
	depends = librsvg
	depends = systemd-libs
	conflicts = padlink-git
	source = padlink-$VERSION.tar.gz::https://github.com/GiftSibiya/padlink/archive/refs/tags/v$VERSION.tar.gz
	sha256sums = $SHA

pkgname = padlink
SRCINFO
echo "wrote $OUT/PKGBUILD and $OUT/.SRCINFO (sha256 $SHA)"
