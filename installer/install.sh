#!/bin/sh
# Installs tcanvas for the current user from an extracted release archive:
#   ~/.local/bin/tcanvas, ~/.local/bin/tcanvas-agent, a menu entry and its icon.
# `./install.sh --uninstall` removes them (layout and tmux sessions are kept).
# PREFIX overrides ~/.local.
set -eu

prefix="${PREFIX:-$HOME/.local}"
here="$(cd "$(dirname "$0")" && pwd)"
bin="$prefix/bin"
apps="$prefix/share/applications"
icons="$prefix/share/icons/hicolor/256x256/apps"

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$bin/tcanvas" "$bin/tcanvas-agent" "$apps/tcanvas.desktop" "$icons/tcanvas.png"
    echo "tcanvas removed from $prefix"
    exit 0
fi

mkdir -p "$bin" "$apps" "$icons"
install -m 755 "$here/tcanvas" "$bin/tcanvas"
install -m 755 "$here/tcanvas-agent" "$bin/tcanvas-agent"
install -m 644 "$here/tcanvas.png" "$icons/tcanvas.png"
sed "s|^Exec=.*|Exec=$bin/tcanvas|" "$here/tcanvas.desktop" > "$apps/tcanvas.desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$apps" >/dev/null 2>&1 || true

echo "tcanvas installed in $bin"
command -v tmux >/dev/null 2>&1 || echo "note: tcanvas needs tmux (e.g. sudo apt install tmux)"
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "note: $bin is not in your PATH" ;;
esac
