#!/usr/bin/env bash
set -euo pipefail

systemctl --user disable --now twodrive-daemon.service 2>/dev/null || true
rm -f "$HOME/.config/systemd/user/twodrive-daemon.service"
rm -f "$HOME/.config/autostart/twodrive-tray.desktop"
rm -f "$HOME/.local/share/applications/twodrive-settings.desktop"
rm -f "$HOME/.local/share/nautilus-python/extensions/twodrive_nautilus.py"
rm -f "$HOME/.local/share/icons/hicolor/scalable/emblems/emblem-twodrive-"*.svg
rm -f "$HOME/.local/bin/twodrive" "$HOME/.local/bin/twodrive-daemon"
rm -f "$HOME/.local/bin/twodrive-tray" "$HOME/.local/bin/twodrive-settings"
if command -v gtk-update-icon-cache >/dev/null; then
    gtk-update-icon-cache -q -t -f "$HOME/.local/share/icons/hicolor" || true
fi
systemctl --user daemon-reload
echo "TwoDrive desktop integration removed. Local config, tokens, cache, and database were preserved."
