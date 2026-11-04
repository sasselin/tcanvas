#!/bin/sh
# Dev mode: on every code change, rebuild, reinstall and restart tcanvas.
# If the build fails, the running app keeps running unchanged.
#
#   ./scripts/watch.sh
set -u
cd "$(dirname "$0")/.."

# Source fingerprint: path + modification time of every watched file.
fingerprint() {
    find crates scripts Cargo.toml Cargo.lock -type f \( -name '*.rs' -o -name '*.toml' -o -name '*.lock' -o -name '*.sh' \) \
        -printf '%p %T@\n' 2>/dev/null | sort | md5sum
}

deploy() {
    printf '\n[%s] change detected → deploying\n' "$(date +%T)"
    if ./scripts/deploy.sh; then
        printf '[%s] ✔ up to date\n' "$(date +%T)"
    else
        printf '[%s] ✖ failed: the running app is unchanged\n' "$(date +%T)"
    fi
}

last=$(fingerprint)
deploy
echo "Waiting for changes… (Ctrl+C to stop)"
while sleep 1; do
    now=$(fingerprint)
    [ "$now" = "$last" ] && continue
    # Wait until the editor has finished writing (nothing new for 1 s).
    while sleep 1; do
        settled=$(fingerprint)
        [ "$settled" = "$now" ] && break
        now=$settled
    done
    last=$now
    deploy
done
