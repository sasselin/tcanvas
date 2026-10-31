#!/bin/sh
# Compile et installe tout, puis relance l'application :
#   - l'agent dans ~/.local/bin (WSL)
#   - tcanvas.exe dans %USERPROFILE%\tcanvas (Windows)
# Les terminaux tmux survivent au redémarrage de l'application.
#
#   ./scripts/deploy.sh            # compile, installe, relance
#   ./scripts/deploy.sh --no-start # sans relancer
set -eu
cd "$(dirname "$0")/.."

start=1
[ "${1:-}" = "--no-start" ] && start=0

ps=/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe
profile_win=$(cmd.exe /c 'echo %USERPROFILE%' 2>/dev/null </dev/null | tr -d '\r')
exe_win="$profile_win\\tcanvas\\tcanvas.exe"
dest=$(wslpath "$profile_win")/tcanvas

# Compile d'abord : si ça échoue, l'application ouverte n'est pas touchée.
echo "→ compilation"
cargo build -q --release -p tcanvas-agent
cargo build -q --release --target x86_64-pc-windows-gnu -p tcanvas

echo "→ agent"
install -D target/release/tcanvas-agent "$HOME/.local/bin/tcanvas-agent"

# Windows verrouille un .exe en cours d'exécution : on ferme l'instance ouverte.
"$ps" -NoProfile -Command "Get-Process tcanvas -ErrorAction SilentlyContinue | Stop-Process -Force; Start-Sleep -Milliseconds 300" </dev/null

mkdir -p "$dest"
cp target/x86_64-pc-windows-gnu/release/tcanvas.exe "$dest/tcanvas.exe"
printf '→ installé : %s\n' "$exe_win"

if [ "$start" = 1 ]; then
    "$ps" -NoProfile -Command "Start-Process -FilePath '$exe_win' -WorkingDirectory '$profile_win\\tcanvas'" </dev/null >/dev/null 2>&1
    echo "→ lancé"
fi
