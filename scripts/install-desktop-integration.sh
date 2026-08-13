#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cargo build --release

install -d "$HOME/.local/bin"
install -m 0755 "$ROOT/target/release/twodrive" "$HOME/.local/bin/twodrive"
install -m 0755 "$ROOT/target/release/twodrive-daemon" "$HOME/.local/bin/twodrive-daemon"
install -m 0755 "$ROOT/scripts/twodrive-tray" "$HOME/.local/bin/twodrive-tray"
install -m 0755 "$ROOT/scripts/twodrive-settings" "$HOME/.local/bin/twodrive-settings"

install -d "$HOME/.config/systemd/user"
install -m 0644 "$ROOT/packaging/systemd/twodrive-daemon.service" "$HOME/.config/systemd/user/twodrive-daemon.service"

install -d "$HOME/.config/autostart"
install -m 0644 "$ROOT/packaging/autostart/twodrive-tray.desktop" "$HOME/.config/autostart/twodrive-tray.desktop"

install -d "$HOME/.local/share/applications"
install -m 0644 "$ROOT/packaging/applications/twodrive-settings.desktop" "$HOME/.local/share/applications/twodrive-settings.desktop"

install -d "$HOME/.local/share/nautilus-python/extensions"
install -m 0644 "$ROOT/packaging/nautilus/twodrive_nautilus.py" "$HOME/.local/share/nautilus-python/extensions/twodrive_nautilus.py"

install -d "$HOME/.local/share/icons/hicolor/scalable/emblems"
install -m 0644 "$ROOT"/packaging/icons/hicolor/scalable/emblems/*.svg "$HOME/.local/share/icons/hicolor/scalable/emblems/"
if command -v gtk-update-icon-cache >/dev/null; then
    gtk-update-icon-cache -q -t -f "$HOME/.local/share/icons/hicolor" || true
fi

systemctl --user daemon-reload
systemctl --user enable --now twodrive-daemon.service

echo "TwoDrive desktop integration installed."
echo "Restart Nautilus with: nautilus -q"
