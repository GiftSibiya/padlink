# Maintainer: Gift Sibiya <https://github.com/GiftSibiya>
pkgname=padlink-git
pkgver=0.1.0
pkgrel=1
pkgdesc='Detect, debug and bridge rumble for wireless game controllers'
arch=('x86_64')
url='https://github.com/GiftSibiya/padlink'
license=('MIT')
depends=('gtk4' 'libadwaita' 'librsvg')
makedepends=('rust' 'git' 'cargo')
provides=('padlink')
conflicts=('padlink')
source=('git+https://github.com/GiftSibiya/padlink.git')
sha256sums=('SKIP')

pkgver() {
  cd padlink
  printf '0.1.0.r%s.%s' "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}

prepare() {
  cd padlink
  export CARGO_HOME="$srcdir/cargo-home"
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd padlink
  export CARGO_HOME="$srcdir/cargo-home"
  cargo build --frozen --release -p padlink-cli -p padlink-gtk
}

package() {
  cd padlink
  install -Dm755 target/release/padlink "$pkgdir/usr/bin/padlink"
  install -Dm755 target/release/padlink-gtk "$pkgdir/usr/bin/padlink-gtk"
  install -Dm644 data/padlink.desktop "$pkgdir/usr/share/applications/padlink.desktop"
  install -Dm644 data/70-padlink.rules "$pkgdir/usr/lib/udev/rules.d/70-padlink.rules"
  install -d "$pkgdir/usr/share/padlink/art"
  install -m644 data/art-templates/*.svg "$pkgdir/usr/share/padlink/art/"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
