#!/bin/sh
# Refreshes the icon cache and launcher database after the playit window is installed or
# removed, so it shows up with its icon right away. Distributions that refresh these through
# package triggers don't need it, so a missing tool is fine.
if command -v gtk-update-icon-cache >/dev/null 2>&1 && [ -f /usr/share/icons/hicolor/index.theme ]; then
  gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor >/dev/null 2>&1 || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications >/dev/null 2>&1 || true
fi
exit 0
