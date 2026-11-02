# tcanvas

Tous vos terminaux WSL sur un **canevas infini**, à la Figma ou Miro. Plus d'onglets : chaque
terminal est une carte que l'on déplace et redimensionne. On les regroupe par projet, on filtre,
on saute directement à celui qu'on cherche et on est prévenu quand l'un d'eux se bloque.
Un projet lié à un dossier montre **en direct les fichiers modifiés et leurs diffs** : idéal pour
suivre ce que fait Claude (ou toute autre IA) pendant qu'il travaille.

- Application Windows native (`tcanvas.exe`, Rust + egui/wgpu, émulation `alacritty_terminal`).
- Chaque carte est une session **tmux** dans WSL : fermez l'application, les terminaux continuent
  de tourner ; rouvrez-la, tout est là.
- Un petit agent Linux (`tcanvas-agent`) surveille les terminaux et signale « bloqué »,
  « attend une réponse », « échec » et « terminé ».

## Installation

Dans WSL :

```sh
# 1. L'agent (obligatoire pour les statuts et notifications)
cargo install --path crates/agent --root ~/.local      # → ~/.local/bin/tcanvas-agent

# 2. Les hooks zsh (facultatif : codes de sortie et durées des commandes)
tcanvas-agent install-hooks                            # ajoute une ligne à ~/.zshrc

# 3. L'application Windows (compilée depuis WSL)
rustup target add x86_64-pc-windows-gnu
sudo apt install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu -p tcanvas
cp target/x86_64-pc-windows-gnu/release/tcanvas.exe /mnt/c/Users/<vous>/Applications/
```

Prérequis dans WSL : `tmux` (≥ 3.0) et `zsh` (pour les hooks).

### Développement : tout reste synchronisé

```sh
./scripts/deploy.sh   # une fois : compile, installe l'agent + l'exe Windows, relance l'app
./scripts/watch.sh    # en continu : à chaque changement du code, recompile et relance
```

Si la compilation échoue, l'application ouverte n'est pas touchée. Relancer l'application ne coûte
rien : les terminaux vivent dans tmux. Astuce : lancez `watch.sh` dans un terminal de tcanvas
lui-même, il survit aux redémarrages qu'il provoque.

## Utilisation

| Geste | Effet |
|---|---|
| Double-clic sur le vide | Nouveau terminal ici (dans le projet sous le curseur) |
| **+ Terminal** sur l'étiquette d'un groupe · gros bouton au centre d'un groupe vide | Nouveau terminal dans ce groupe (dans son dossier lié), posé à côté des autres |
| Glisser le vide · clic milieu | Déplacer la vue (main fermée) |
| **Maj maintenue** · Maj + glisser | Outil main : déplacer la vue n'importe où, même par-dessus un terminal |
| Molette · Ctrl+molette | Défiler · zoomer autour du curseur |
| Clic sur une carte | Lui donner le clavier |
| Glisser la barre de titre · coin bas-droit | Déplacer · redimensionner (le terminal suit) |
| Lâcher un terminal **sur le cadre d'un groupe** · **hors de tout cadre** | Il rejoint ce groupe · il sort de son groupe (pointillés rouges pendant le glissement) |
| Clic droit sur la barre de titre → **Déplacer vers** | Envoyer le terminal dans un autre groupe (ou hors groupe), même depuis un espace où les autres sont cachés |
| Double-clic sur la barre de titre | Centrer et zoomer sur la carte |
| **×** sur la barre de titre | Fermer le terminal, après confirmation : le shell et **tous** ses processus |
| **×** sur l'étiquette d'un groupe | Fermer le groupe **et tous ses terminaux**, après confirmation (liste des commandes en cours) |
| Clic droit sur la barre de titre | Renommer, délai « bloqué », déplacer vers, fermer |
| **Alt + glisser** d'une carte vers une autre | Tirer un **fil** |
| Clic sur un fil · clic droit | Voler jusqu'au bout opposé · supprimer le fil |
| Clic sur la mini-carte | Y aller |
| Étiquette d'un projet : glisser · **double-clic** · clic droit | Déplacer avec ses cartes · **renommer sur place** (Entrée valide, Échap annule) · dossier / couleur |
| **📁** à côté de l'étiquette | **Lier un dossier** : sélecteur qui parcourt WSL (fil d'Ariane, ⌂, ⬆, dossiers récents, filtre, repère ⎇ git, chemin éditable), ouvert sur le dossier d'un terminal du groupe ; « Délier » pour retirer |
| **Δ Changements** (barre du haut, groupe lié ouvert) | Panneau des fichiers modifiés et de leurs diffs, en direct |
| Puce d'un projet dans la barre du haut | **Ouvrir ce projet seul**, cadré et centré en un clic : les autres projets sont cachés (🔔 = un terminal y demande l'attention, ✎ = on y écrit des fichiers) |
| Puce **Tous** | Vue d'ensemble de tous les projets, pour organiser |

Raccourcis globaux (même quand un terminal a le clavier) :

| Raccourci | Effet |
|---|---|
| `Ctrl+Maj+1` … `9` | Ouvrir le projet n° 1 à 9 (numéro affiché sur son étiquette) |
| `Ctrl+Maj+K` | Palette « aller à » : terminaux **et projets** de tous les espaces (ouvre le bon projet) |
| `Ctrl+Maj+N` | Nouveau terminal |
| `Ctrl+Maj+W` | Fermer le terminal focalisé (confirmation : Entrée ferme, Échap annule) |
| `Ctrl+Maj+F` | Filtrer (Entrée : cadrer les correspondances) |
| `Ctrl+Maj+0` | Tout voir (dans l'espace ouvert) |

Ctrl+C, Ctrl+K, Ctrl+plus/moins, Échap, Tab… vont au terminal. Ctrl+V colle. La sélection à la
souris (dans tmux) est copiée dans le presse-papiers Windows.

**Espaces** : chaque projet est un canevas à part. On n'en voit qu'un à la fois ; un terminal
créé dans un projet ouvert en fait partie. Les terminaux hors projet n'apparaissent que dans « Tous ».
Un projet s'ouvre toujours cadré sur tout son contenu ; « Tous » reprend la vue d'ensemble laissée.
Le projet ouvert et la vue sont sauvegardés.

**Rien ne se chevauche** : un groupe qui grandit écarte les groupes voisins (avec leurs terminaux) ;
un terminal hors groupe posé ou créé contre un cadre saute à la place libre la plus proche ; dans un
groupe, le terminal posé garde sa place et ses voisins s'écartent.

**Molette** : elle fait défiler ce qui est sous le pointeur — terminal (même sans le clavier),
panneau, liste, menu, fenêtre — et ne déplace la vue que sur le vide du canevas.

**Filtre** : les puces de statut comptent les terminaux de l'espace ouvert. Un clic ne garde que
ce statut et cadre la vue sur ses terminaux (Ctrl+clic : combiner) ; re-cliquer, ou **× filtre**,
retire le filtre et ramène la vue d'avant. Les autres terminaux sont estompés, jamais cachés.

**Groupes** : un terminal appartient à un groupe, et le cadre du groupe **épouse toujours ses
terminaux** (il grandit et rétrécit avec eux). Pendant qu'on glisse un terminal, le cadre de son
groupe ne bouge pas : le lâcher dehors le sort du groupe, le lâcher sur un autre cadre l'y fait entrer.

**Dossier lié et changements** : les nouveaux terminaux d'un groupe lié s'ouvrent dans son dossier.
L'agent y surveille les fichiers toutes les 2 s (`git status` + `git diff HEAD`, fichiers non suivis
compris, sans jamais verrouiller l'index). Le panneau **Δ Changements** liste les fichiers, le plus
récent en haut, avec `+/−` ; ceux touchés à l'instant sont surlignés. Avec **Suivre** (par défaut),
le diff du dernier fichier modifié s'affiche tout seul ; cliquer un fichier arrête de suivre. Un ✎
sur la puce d'un autre projet signale qu'on y écrit en ce moment. Hors git : liste des fichiers
modifiés depuis le début du suivi, sans diff.

**Fermeture** : `tcanvas-agent kill` arrête vraiment tout — descendance des shells, processus de la même
session (`nohup`), et ceux qui se sont détachés (`setsid`, démons) repérés par leur `TMUX_PANE`. SIGHUP +
SIGTERM d'abord, SIGKILL pour les récalcitrants, puis vérification qu'il ne reste rien.

**Notifications** : un toast Windows (et une bordure qui pulse sur la carte) quand un terminal est
bloqué (aucune sortie depuis 60 s, CPU au repos — réglable par carte), attend une réponse
(`[y/N]`, `password:`…), échoue (code ≠ 0, sauf Ctrl+C), termine une commande de plus de 30 s,
ou s'arrête. Rien si vous regardez déjà ce terminal.

L'icône (`assets/icon.svg`, variante `icon-small.svg` pour 16–32 px) est embarquée dans l'exe
Windows (`crates/app/build.rs`, `embed-resource` + `windres` de mingw-w64) et dans la fenêtre.
Pour régénérer les PNG et l'ICO : `sudo apt install librsvg2-bin imagemagick`, puis `rsvg-convert -w N`
et `convert icon-*.png icon.ico`.

## Architecture

```
tcanvas.exe (Windows)                         WSL
  carte ── ConPTY ── wsl.exe ── sh ── tmux -L tcanvas attach ──┐
  agent_link ── stdin/stdout JSON ── tcanvas-agent ──── tmux list-panes + /proc
```

Workspace en **slices verticales** :

```
crates/protocol   messages app ⇄ agent (serde, une ligne JSON par message)
crates/agent      features/{tmux, status, hooks, kill, changes, serve}
crates/app        shell/ (monde, bus d'événements, thème, polices)
                  features/{canvas, terminal_card, projects, spaces, changes, wires,
                            filters, jump, notifications, agent_link, persistence}
```

Les slices ne s'appellent pas entre elles : elles lisent le `World` et émettent des `Event`
que le `shell` distribue à toutes.

Le canevas est sauvegardé dans `%APPDATA%\tcanvas\config\layout.json` (écriture atomique ; un
fichier illisible est mis de côté, jamais écrasé). Les sessions tmux orphelines (`tc_*`) sont
réadoptées automatiquement.

Variables utiles : `TCANVAS_TMUX_SOCKET` (socket tmux, défaut `tcanvas`), `TCANVAS_CONFIG_DIR`,
`TCANVAS_AGENT` (chemin de l'agent dans WSL).

## Tests

```sh
cargo test --workspace          # Linux : unitaires, UI sans écran, bout en bout réel (tmux + PTY + agent)
./scripts/test-windows.sh       # la même suite compilée pour Windows, exécutée sous Windows
                                # (vrai ConPTY + wsl.exe) depuis WSL
cargo clippy --workspace --all-targets
```

Le test de bout en bout ouvre un vrai terminal, tape dedans, vérifie le statut « bloqué » et la
notification, **ferme puis rouvre l'application** et vérifie que c'est le même shell, puis ferme
le terminal. Il lie aussi un groupe à un dépôt git temporaire, vérifie qu'un terminal du groupe
s'y ouvre et qu'un fichier écrit depuis ce terminal apparaît dans les changements. Il utilise un serveur tmux jetable et ne touche jamais à vos sessions.
