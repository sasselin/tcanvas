//! Hooks zsh qui publient début, fin et code de sortie de chaque commande
//! dans des options de pane tmux (`@tc_started`, `@tc_ended`, `@tc_exit`).

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

pub const ZSH_HOOKS: &str = r#"# tcanvas : statut des commandes (installé par `tcanvas-agent install-hooks`)
if [[ -n "$TMUX" && "$TMUX" == *tcanvas* ]]; then
  zmodload zsh/datetime 2>/dev/null
  _tcanvas_preexec() {
    _tcanvas_running=1
    tmux set -p -t "$TMUX_PANE" @tc_started "$EPOCHSECONDS" 2>/dev/null
  }
  _tcanvas_precmd() {
    local code=$?
    (( ${_tcanvas_running:-0} )) || return 0
    _tcanvas_running=0
    tmux set -p -t "$TMUX_PANE" @tc_exit "$code" \; set -p -t "$TMUX_PANE" @tc_ended "$EPOCHSECONDS" 2>/dev/null
  }
  autoload -Uz add-zsh-hook
  add-zsh-hook preexec _tcanvas_preexec
  add-zsh-hook precmd _tcanvas_precmd
fi
"#;

const SOURCE_LINE: &str = "[ -f ~/.config/tcanvas/hooks.zsh ] && source ~/.config/tcanvas/hooks.zsh";

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

fn hooks_path() -> PathBuf {
    home().join(".config/tcanvas/hooks.zsh")
}

pub fn installed() -> bool {
    hooks_path().exists()
        && fs::read_to_string(home().join(".zshrc")).is_ok_and(|rc| rc.contains(SOURCE_LINE))
}

/// Écrit `~/.config/tcanvas/hooks.zsh` et l'ajoute une seule fois à `~/.zshrc`.
pub fn install() -> io::Result<()> {
    let path = hooks_path();
    fs::create_dir_all(path.parent().expect("hooks path has a parent"))?;
    fs::write(&path, ZSH_HOOKS)?;
    println!("Hooks écrits dans {}", path.display());

    let zshrc = home().join(".zshrc");
    let rc = fs::read_to_string(&zshrc).unwrap_or_default();
    if rc.contains(SOURCE_LINE) {
        println!("{} source déjà les hooks.", zshrc.display());
    } else {
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&zshrc)?;
        writeln!(f, "\n{SOURCE_LINE}")?;
        println!("Ligne ajoutée à {}. Les nouveaux terminaux en profiteront.", zshrc.display());
    }
    Ok(())
}
