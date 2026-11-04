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

/// Problème signalé par l'agent. Le protocole transporte le sens, pas le texte :
/// l'application le traduit dans la langue de Windows.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind")]
pub enum Problem {
    /// Le chemin existe mais n'est pas un dossier.
    NotADir { path: String },
    DirNotFound { path: String },
    /// Lecture du dossier impossible (détail de l'OS, non traduit).
    Unreadable { detail: String },
    /// Le sha demandé n'a pas la forme d'un sha.
    CommitRefused,
    CommitNotFound,
    /// Chemin de fichier absolu ou qui sort du dépôt.
    PathRefused,
    NotAGitRepo,
    /// Le fichier ne diffère pas de HEAD.
    NoDiff,
    TmuxMissing { detail: String },
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
#[serde(tag = "kind", content = "from")]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    /// Renommé depuis ce chemin.
    Renamed(String),
    /// Nouveau fichier pas encore suivi par git.
    Untracked,
}

impl ChangeKind {
    pub fn letter(&self) -> char {
        match self {
            ChangeKind::Modified => 'M',
            ChangeKind::Added => 'A',
            ChangeKind::Deleted => 'D',
            ChangeKind::Renamed(_) => 'R',
            ChangeKind::Untracked => '?',
        }
    }
}

/// Un fichier modifié dans un dossier surveillé (chemin relatif à la racine).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct FileChange {
    pub path: String,
    pub kind: ChangeKind,
    /// Lignes ajoutées / retirées par rapport à HEAD (inconnu hors git ou pour un binaire).
    pub added: Option<u32>,
    pub removed: Option<u32>,
    /// Dernière modification (secondes Unix ; 0 si le fichier n'existe plus).
    pub mtime: u64,
    pub binary: bool,
}

/// État d'un dossier surveillé, renvoyé à chaque changement.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct ChangeSet {
    /// Clé donnée par l'application (id du projet).
    pub key: String,
    /// Chemin absolu résolu (`~` développé), si le dossier existe.
    pub root: Option<String>,
    pub git: bool,
    pub branch: Option<String>,
    /// HEAD détachée (`branch` est alors `None`).
    #[serde(default)]
    pub detached: bool,
    pub error: Option<Problem>,
    pub files: Vec<FileChange>,
    /// La liste a été coupée (trop de fichiers).
    pub truncated: bool,
    /// Derniers commits de la branche, du plus récent au plus ancien (vide hors git).
    #[serde(default)]
    pub commits: Vec<CommitInfo>,
}

/// Un commit récent du dépôt surveillé.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct CommitInfo {
    pub sha: String,
    pub summary: String,
    pub author: String,
    /// Date du commit (secondes Unix).
    pub time: u64,
}

/// Un sous-dossier, pour le sélecteur de dossiers.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct DirEntry {
    pub name: String,
    /// Contient un dépôt git (`.git`).
    pub git: bool,
}

/// Contenu d'un dossier WSL, en réponse à `AgentCmd::ListDir`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct DirListing {
    /// Chemin tel que demandé (sert à apparier la réponse).
    pub request: String,
    /// Chemin absolu résolu, si le dossier existe.
    pub path: Option<String>,
    pub parent: Option<String>,
    pub git: bool,
    /// Sous-dossiers visibles (les dossiers cachés sont omis), triés par nom.
    pub dirs: Vec<DirEntry>,
    pub error: Option<Problem>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type")]
pub enum AgentEvent {
    Hello { version: String, hooks_installed: bool },
    Session(SessionInfo),
    Gone { session: String },
    Error { problem: Problem },
    Changes(ChangeSet),
    /// `text` est vide quand `problem` explique pourquoi il n'y a pas de diff.
    Diff {
        key: String,
        file: String,
        text: String,
        truncated: bool,
        #[serde(default)]
        problem: Option<Problem>,
    },
    DirListing(DirListing),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type")]
pub enum AgentCmd {
    SetStall { session: String, secs: u64 },
    Kill { session: String },
    /// Surveille les fichiers modifiés de `path` (relancé si déjà surveillé avec un autre chemin).
    Watch { key: String, path: String },
    Unwatch { key: String },
    /// Diff d'un fichier (chemin relatif) d'un dossier surveillé.
    Diff { key: String, file: String },
    /// Contenu d'un commit (`git show`) d'un dossier surveillé ; réponse : `AgentEvent::Diff`
    /// avec `file` = `sha`.
    ShowCommit { key: String, sha: String },
    /// Sous-dossiers de `path` (`~` accepté), pour le sélecteur de dossiers.
    ListDir { path: String },
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
    fn roundtrip_changes() {
        let ev = AgentEvent::Changes(ChangeSet {
            key: "p1".into(),
            root: Some("/home/me/api".into()),
            git: true,
            branch: Some("main".into()),
            detached: false,
            error: None,
            files: vec![
                FileChange { path: "src/a.rs".into(), kind: ChangeKind::Modified, added: Some(3), removed: Some(1), mtime: 42, binary: false },
                FileChange { path: "b.rs".into(), kind: ChangeKind::Renamed("old.rs".into()), added: None, removed: None, mtime: 0, binary: true },
            ],
            truncated: false,
            commits: vec![],
        });
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        let ev = AgentEvent::DirListing(DirListing {
            request: "~".into(),
            path: Some("/home/me".into()),
            parent: Some("/home".into()),
            git: false,
            dirs: vec![DirEntry { name: "api".into(), git: true }],
            error: Some(Problem::Unreadable { detail: "denied".into() }),
        });
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        let ev = AgentEvent::Diff { key: "p1".into(), file: "a.rs".into(), text: String::new(), truncated: false, problem: Some(Problem::NotAGitRepo) };
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        let ev = AgentEvent::Error { problem: Problem::TmuxMissing { detail: "no such file".into() } };
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        let cmd = AgentCmd::Diff { key: "p1".into(), file: "src/a.rs".into() };
        assert_eq!(decode::<AgentCmd>(&encode(&cmd)).unwrap(), cmd);
    }

    #[test]
    fn roundtrip_cmd() {
        let cmd = AgentCmd::SetStall { session: "tc_x".into(), secs: 5 };
        assert_eq!(decode::<AgentCmd>(&encode(&cmd)).unwrap(), cmd);
    }
}
