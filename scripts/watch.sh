#!/bin/sh
# Mode dev : à chaque changement du code, recompile, réinstalle et relance tcanvas.
# Si la compilation échoue, l'application ouverte continue de tourner telle quelle.
#
#   ./scripts/watch.sh
set -u
cd "$(dirname "$0")/.."

# Empreinte des sources : chemin + date de modification de chaque fichier suivi.
fingerprint() {
    find crates scripts Cargo.toml Cargo.lock -type f \( -name '*.rs' -o -name '*.toml' -o -name '*.lock' -o -name '*.sh' \) \
        -printf '%p %T@\n' 2>/dev/null | sort | md5sum
}

deploy() {
    printf '\n[%s] changement détecté → déploiement\n' "$(date +%T)"
    if ./scripts/deploy.sh; then
        printf '[%s] ✔ à jour\n' "$(date +%T)"
    else
        printf '[%s] ✖ échec : l application ouverte reste inchangée\n' "$(date +%T)"
    fi
}

last=$(fingerprint)
deploy
echo "En attente de changements… (Ctrl+C pour arrêter)"
while sleep 1; do
    now=$(fingerprint)
    [ "$now" = "$last" ] && continue
    # On attend que l'éditeur ait fini d'écrire (rien de neuf pendant 1 s).
    while sleep 1; do
        settled=$(fingerprint)
        [ "$settled" = "$now" ] && break
        now=$settled
    done
    last=$now
    deploy
done
