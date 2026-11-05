#!/bin/sh
# Build and install tcanvas.exe into %USERPROFILE%\tcanvas, then restart the app.
# The exe embeds the agent and updates ~/.local/bin/tcanvas-agent itself on startup.
# The tmux terminals survive the app restart.
#
#   ./scripts/deploy.sh            # build, install, restart
#   ./scripts/deploy.sh --no-start # without restarting
set -eu
cd "$(dirname "$0")/.."

start=1
[ "${1:-}" = "--no-start" ] && start=0

ps=/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe
profile_win=$(cmd.exe /c 'echo %USERPROFILE%' 2>/dev/null </dev/null | tr -d '\r')
exe_win="$profile_win\\tcanvas\\tcanvas.exe"
dest=$(wslpath "$profile_win")/tcanvas

# Build first: if it fails, the running app is left untouched.
echo "→ build"
cargo build -q --release --target x86_64-pc-windows-gnu -p tcanvas

# Windows locks a running .exe: stop the running instance first.
"$ps" -NoProfile -Command "Get-Process tcanvas -ErrorAction SilentlyContinue | Stop-Process -Force; Start-Sleep -Milliseconds 300" </dev/null

mkdir -p "$dest"
cp target/x86_64-pc-windows-gnu/release/tcanvas.exe "$dest/tcanvas.exe"
printf '→ installed: %s\n' "$exe_win"

if [ "$start" = 1 ]; then
    "$ps" -NoProfile -Command "Start-Process -FilePath '$exe_win' -WorkingDirectory '$profile_win\\tcanvas'" </dev/null >/dev/null 2>&1
    echo "→ started"
fi
