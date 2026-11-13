#!/usr/bin/env bash
# Build Smart Clock and add it to the GNOME app menu for this user.
# It does NOT start at login; launch it from the app menu.
#
#   scripts/install-local.sh              build and install / update
#   scripts/install-local.sh --uninstall  remove the app (keeps your data)
set -euo pipefail

BIN="$HOME/.local/bin/smart-clock"
ICON="$HOME/.local/share/icons/hicolor/scalable/apps/smart-clock.svg"
DESKTOP="$HOME/.local/share/applications/smart-clock.desktop"

refresh() {
  update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
  gtk-update-icon-cache -q -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
}

if [[ "${1:-}" == "--uninstall" ]]; then
  rm -f "$BIN" "$ICON" "$DESKTOP"
  refresh
  echo "Removed Smart Clock from the app menu."
  echo "Your alarms, timers, reminders and settings are still in ~/.local/share/smart-clock/."
  exit 0
fi

cd "$(dirname "$0")/.."
cargo build --release -p smart-clock

install -Dm755 target/release/smart-clock "$BIN"
install -Dm644 data/icons/smart-clock.svg "$ICON"
mkdir -p "$(dirname "$DESKTOP")"
sed "s|@BIN@|$BIN|g" data/smart-clock.desktop.in > "$DESKTOP"
chmod 644 "$DESKTOP"
refresh

echo "Installed Smart Clock. Open it from the app menu (search \"Smart Clock\")."
echo "Right-click its icon for \"Open in a Window\". Quit with Alt+F4."
