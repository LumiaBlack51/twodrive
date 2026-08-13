#!/usr/bin/env bash
set -euo pipefail

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
package_root="$(mktemp -d /tmp/twodrive-deb.XXXXXX)"
trap 'rm -rf -- "$package_root"' EXIT
chmod 0755 "$package_root"

remap_flags="--remap-path-prefix=${HOME}=/home/build --remap-path-prefix=${project_root}=/usr/src/twodrive"
RUSTFLAGS="${RUSTFLAGS:+${RUSTFLAGS} }${remap_flags}" cargo build --workspace --release

install -d "$package_root/DEBIAN"
install -m 0644 "$project_root/packaging/deb/control" "$package_root/DEBIAN/control"
install -d "$package_root/usr/bin"
install -m 0755 "$project_root/target/release/twodrive" "$package_root/usr/bin/twodrive"
install -m 0755 "$project_root/target/release/twodrive-daemon" "$package_root/usr/bin/twodrive-daemon"
install -m 0755 "$project_root/scripts/twodrive-tray" "$package_root/usr/bin/twodrive-tray"
install -m 0755 "$project_root/scripts/twodrive-settings" "$package_root/usr/bin/twodrive-settings"
install -d "$package_root/usr/lib/systemd/user"
install -m 0644 "$project_root/packaging/systemd/twodrive-daemon.service" \
    "$package_root/usr/lib/systemd/user/twodrive-daemon.service"
install -d "$package_root/etc/xdg/autostart"
install -m 0644 "$project_root/packaging/autostart/twodrive-tray.desktop" \
    "$package_root/etc/xdg/autostart/twodrive-tray.desktop"
install -d "$package_root/usr/share/applications"
install -m 0644 "$project_root/packaging/applications/twodrive-settings.desktop" \
    "$package_root/usr/share/applications/twodrive-settings.desktop"
install -d "$package_root/usr/share/nautilus-python/extensions"
install -m 0644 "$project_root/packaging/nautilus/twodrive_nautilus.py" \
    "$package_root/usr/share/nautilus-python/extensions/twodrive_nautilus.py"
install -d "$package_root/usr/share/icons/hicolor/scalable/emblems"
install -m 0644 "$project_root"/packaging/icons/hicolor/scalable/emblems/*.svg \
    "$package_root/usr/share/icons/hicolor/scalable/emblems/"
install -d "$package_root/usr/share/doc/twodrive"
install -m 0644 "$project_root/README.md" "$package_root/usr/share/doc/twodrive/README.md"
install -m 0644 "$project_root/LICENSE" "$package_root/usr/share/doc/twodrive/copyright"
gzip -n -9 -c "$project_root/debian/changelog" \
    > "$package_root/usr/share/doc/twodrive/changelog.Debian.gz"

install -d "$project_root/dist"
dpkg-deb --build --root-owner-group "$package_root" \
    "$project_root/dist/twodrive_0.2.2-2_amd64.deb"
