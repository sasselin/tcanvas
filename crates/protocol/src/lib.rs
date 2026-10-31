//! Messages échangés entre l'application (Windows) et `tcanvas-agent` (WSL).
//! Transport : un objet JSON par ligne sur stdin/stdout de l'agent.

use serde::{Deserialize, Serialize};

/// Socket tmux dédié (`tmux -L tcanvas`), isolé du tmux habituel.
pub const TMUX_SOCKET: &str = "tcanvas";
/// Préfixe des sessions tmux gérées par le canevas.
pub const SESSION_PREFIX: &str = "tc_";
pub const DEFAULT_STALL_SECS: u64 = 60;

/// Socket tmux effectif : `TCANVAS_TMUX_SOCKET` permet aux tests d'utiliser un serveur jetable.
pub fn tmux_socket() -> String {
    std::env::var("TCANVAS_TMUX_SOCKET").unwrap_or_else(|_| TMUX_SOCKET.to_string())
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(tag = "kind", content = "code")]
pub enum TermStatus {
    /// Le shell attend une commande.
    Idle,
    /// Une commande tourne et produit de la sortie (ou une app plein écran est ouverte).
    Running,
    /// Une commande tourne mais n'affiche plus rien et ne consomme pas de CPU.
    Stalled,
    /// La dernière ligne ressemble à une question (`[y/N]`, `password:`…).
    WaitingInput,
    /// La dernière commande a échoué avec ce code.
    Exited(i32),
    /// Le pane tmux est mort.
    Dead,
}

impl TermStatus {
    pub fn label(self) -> &'static str {
        match self {
            TermStatus::Idle => "inactif",
            TermStatus::Running => "actif",
            TermStatus::Stalled => "bloqué",
            TermStatus::WaitingInput => "attend saisie",
            TermStatus::Exited(_) => "échec",
            TermStatus::Dead => "arrêté",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct SessionInfo {
    pub session: String,
    pub status: TermStatus,
    /// Commande au premier plan (ligne de commande complète), si une commande tourne.
    pub command: Option<String>,
    pub cwd: Option<String>,
    /// Code et durée de la dernière commande terminée (nécessite les hooks shell).
    pub last_exit: Option<i32>,
    pub last_duration_secs: Option<u64>,
    /// Dernières lignes non vides de l'écran, pour l'aperçu dézoomé.
    pub tail: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type")]
pub enum AgentEvent {
    Hello { version: String, hooks_installed: bool },
    Session(SessionInfo),
    Gone { session: String },
    Error { message: String },
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type")]
pub enum AgentCmd {
    SetStall { session: String, secs: u64 },
    Kill { session: String },
}

pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut line = serde_json::to_string(msg).expect("protocol message serializes");
    line.push('\n');
    line
}

pub fn decode<'a, T: Deserialize<'a>>(line: &'a str) -> serde_json::Result<T> {
    serde_json::from_str(line.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_event() {
        let ev = AgentEvent::Session(SessionInfo {
            session: "tc_ab12".into(),
            status: TermStatus::Exited(2),
            command: None,
            cwd: Some("/home".into()),
            last_exit: Some(2),
            last_duration_secs: Some(31),
            tail: vec!["$ false".into()],
        });
        let line = encode(&ev);
        assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
        assert_eq!(decode::<AgentEvent>(&line).unwrap(), ev);
    }

    #[test]
    fn roundtrip_cmd() {
        let cmd = AgentCmd::SetStall { session: "tc_x".into(), secs: 5 };
        assert_eq!(decode::<AgentCmd>(&encode(&cmd)).unwrap(), cmd);
    }
}
