//! Worktrees: the git worktrees of the open group's repository, with the terminals living in
//! each one. The agent lists them (`AgentEvent::Worktrees`, for every watched folder); this slice
//! keeps the lists, tells which worktree each card's folder is in (`World::card_worktrees`, read
//! by the card's branch badge) and draws a one-line strip under the toolbar when the open group's
//! repository has more than its main checkout. Read-only: nothing here changes the disk.

use std::collections::BTreeMap;

use egui::{Color32, Panel, RichText, ScrollArea, Ui};
use protocol::{AgentEvent, Worktree, WorktreeList};

use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::world::{CardWorktree, ProjectId, World, unix_now};

const DIRTY: Color32 = Color32::from_rgb(230, 180, 80);
const MERGED: Color32 = Color32::from_rgb(90, 200, 120);

/// `root` is `path` or one of its parents, compared by whole path components (`/a/b` holds
/// `/a/b/c` and `/a/b/`, not `/a/bc`).
pub fn contains(root: &str, path: &str) -> bool {
    let (root, path) = (root.trim_end_matches('/'), path.trim_end_matches('/'));
    path == root || path.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'))
}

/// The worktree holding `cwd`: the deepest one, so a worktree nested in another's folder
/// (`.claude/worktrees/x` inside the main checkout) wins. A bare entry has no files.
pub fn worktree_of<'a>(cwd: &str, worktrees: impl IntoIterator<Item = &'a Worktree>) -> Option<&'a Worktree> {
    worktrees.into_iter().filter(|w| !w.bare && contains(&w.path, cwd)).max_by_key(|w| w.path.trim_end_matches('/').len())
}

/// Folder name of a worktree.
pub fn folder_name(wt: &Worktree) -> &str {
    wt.path.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("/")
}

fn card_worktree(wt: &Worktree) -> CardWorktree {
    CardWorktree { path: wt.path.clone(), branch: wt.branch.clone(), linked: (!wt.main).then(|| folder_name(wt).to_string()) }
}

/// "3 min ago", "2 d ago"... for the last commit.
fn age(now: u64, time: u64) -> String {
    match now.saturating_sub(time) {
        0..=59 => t().just_now.into(),
        s @ 60..=3599 => (t().mins_ago)(s / 60),
        s @ 3600..=86_399 => (t().hours_ago)(s / 3600),
        s => (t().days_ago)(s / 86_400),
    }
}

#[derive(Default)]
pub struct Worktrees {
    /// Latest list of each group's repository (key: project id), with the group folder it was
    /// received for.
    lists: BTreeMap<ProjectId, (String, WorktreeList)>,
}

impl Worktrees {
    pub fn list(&self, project: &str) -> Option<&WorktreeList> {
        self.lists.get(project).map(|(_, l)| l)
    }

    /// Every frame: which worktree each card's folder is in, across every known repository.
    pub fn poll(&self, world: &mut World) {
        let all: Vec<&Worktree> = self.lists.values().flat_map(|(_, l)| &l.worktrees).collect();
        let found: BTreeMap<_, _> = world.cards.iter().filter_map(|c| Some((c.id.clone(), card_worktree(worktree_of(c.current_cwd()?, all.iter().copied())?)))).collect();
        if found != world.card_worktrees {
            world.card_worktrees = found;
        }
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, _out: &mut Vec<Event>) {
        match ev {
            Event::Agent(AgentEvent::Worktrees(list)) => {
                // Outside git (empty list) or for a group that no longer watches: forget it.
                match world.project(&list.key).and_then(|p| p.cwd.clone()) {
                    Some(folder) if !list.worktrees.is_empty() => {
                        self.lists.insert(list.key.clone(), (folder, list.clone()));
                    }
                    _ => {
                        self.lists.remove(&list.key);
                    }
                }
            }
            // Linking the same folder again changes nothing (and the agent, still watching it,
            // sends nothing new): keep the list. Another folder, or none: forget it.
            Event::SetProjectCwd(id, cwd) => {
                if self.lists.get(id).is_some_and(|(folder, _)| cwd.as_deref() != Some(folder.as_str())) {
                    self.lists.remove(id);
                }
            }
            Event::RemoveProject(id) => {
                self.lists.remove(id);
            }
            _ => {}
        }
    }

    /// The open group's list, when its repository has more than its main checkout (otherwise
    /// the card badges say all there is to say).
    pub fn shown(&self, world: &World) -> Option<&WorktreeList> {
        let id = world.space.as_deref().filter(|id| world.project(id).is_some())?;
        self.list(id).filter(|l| l.worktrees.iter().filter(|w| !w.bare).count() > 1)
    }

    /// One-line strip under the toolbar. Call after the toolbar, before the side panels.
    pub fn ui(&self, ui: &mut Ui, world: &World, events: &mut Vec<Event>) {
        let Some(list) = self.shown(world) else { return };
        let worktrees: Vec<&Worktree> = list.worktrees.iter().filter(|w| !w.bare).collect();
        Panel::top("worktrees").resizable(false).show(ui, |ui| {
            ScrollArea::horizontal().id_salt("worktrees-strip").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new((t().worktrees_title)(worktrees.len())).strong().color(theme::TEXT_DIM)).on_hover_text(list.repo.as_deref().unwrap_or_default());
                    for wt in worktrees {
                        ui.separator();
                        worktree_ui(ui, world, list, wt, events);
                    }
                });
            });
        });
    }
}

fn worktree_ui(ui: &mut Ui, world: &World, list: &WorktreeList, wt: &Worktree, events: &mut Vec<Event>) {
    let base = list.default_branch.as_deref().unwrap_or_default();
    let name = if wt.main { t().main_checkout } else { folder_name(wt) };
    let hover = match wt.last_commit {
        Some(time) => format!("{}\n{}", wt.path, (t().last_commit)(&age(unix_now(), time))),
        None => wt.path.clone(),
    };
    ui.label(RichText::new(name).strong().color(theme::TEXT)).on_hover_text(hover);
    ui.label(RichText::new(format!("⎇ {}", wt.branch.as_deref().unwrap_or(t().detached_head))).color(theme::TEXT_DIM));
    if wt.dirty == Some(true) {
        ui.label(RichText::new("●").color(DIRTY)).on_hover_text(t().worktree_dirty_hint);
    }
    if let (Some(ahead), Some(behind)) = (wt.ahead, wt.behind)
        && (ahead, behind) != (0, 0)
    {
        ui.label(RichText::new(format!("↑{ahead} ↓{behind}")).color(theme::TEXT_DIM)).on_hover_text((t().ahead_behind_hint)(base));
    }
    if wt.merged == Some(true) {
        ui.label(RichText::new(t().merged).color(MERGED)).on_hover_text((t().merged_hint)(base));
    }
    if wt.locked {
        let label = ui.label(RichText::new(t().locked).color(theme::TEXT_DIM));
        if let Some(reason) = &wt.lock_reason {
            label.on_hover_text(reason);
        }
    }
    if wt.prunable {
        ui.label(RichText::new(t().prunable).color(theme::TEXT_DIM)).on_hover_text(t().prunable_hint);
    }
    let mut any = false;
    for card in world.cards.iter().filter(|c| world.card_worktrees.get(&c.id).is_some_and(|w| w.path == wt.path)) {
        any = true;
        ui.label(RichText::new("●").color(theme::status_color(card.status())));
        if ui.small_button(card.title()).on_hover_text(t().go_to_terminal).clicked() {
            events.push(Event::FlyTo(card.id.clone()));
        }
    }
    if !any {
        ui.label(RichText::new(t().no_terminal_here).small().color(theme::TEXT_DIM));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::world::{Card, Project};
    use egui::{Pos2, Rect, Vec2};

    fn wt(path: &str, main: bool, branch: Option<&str>) -> Worktree {
        Worktree {
            path: path.into(),
            head: Some("a".repeat(40)),
            branch: branch.map(String::from),
            detached: branch.is_none(),
            main,
            bare: false,
            locked: false,
            lock_reason: None,
            prunable: false,
            dirty: Some(false),
            ahead: Some(0),
            behind: Some(0),
            merged: None,
            last_commit: Some(1),
        }
    }

    fn shop() -> Vec<Worktree> {
        vec![wt("/home/me/shop", true, Some("main")), wt("/home/me/shop/.claude/worktrees/invoices", false, Some("feat/invoices")), wt("/home/me/shop-taxes/", false, None)]
    }

    fn of(cwd: &str, list: &[Worktree]) -> Option<String> {
        worktree_of(cwd, list).map(|w| w.path.clone())
    }

    #[test]
    fn paths_match_by_whole_components() {
        assert!(contains("/home/me/shop", "/home/me/shop"));
        assert!(contains("/home/me/shop/", "/home/me/shop/src"));
        assert!(contains("/home/me/shop", "/home/me/shop/src/"));
        assert!(!contains("/home/me/shop", "/home/me/shop-taxes"));
        assert!(!contains("/home/me/shop/src", "/home/me/shop"));
        assert!(contains("/", "/home"), "the root holds everything");
    }

    #[test]
    fn the_deepest_worktree_wins() {
        let list = shop();
        assert_eq!(of("/home/me/shop/src", &list).as_deref(), Some("/home/me/shop"));
        assert_eq!(
            of("/home/me/shop/.claude/worktrees/invoices/src/pdf", &list).as_deref(),
            Some("/home/me/shop/.claude/worktrees/invoices"),
            "nested in the main checkout, attributed to the linked one"
        );
        assert_eq!(of("/home/me/shop/.claude/worktrees", &list).as_deref(), Some("/home/me/shop"));
        assert_eq!(of("/home/me/shop-taxes", &list).as_deref(), Some("/home/me/shop-taxes/"), "trailing slash on the worktree");
        assert_eq!(of("/home/me/shop-taxes/", &list).as_deref(), Some("/home/me/shop-taxes/"), "trailing slash on the folder");
        assert_eq!(of("/home/me/blog", &list), None, "outside any worktree");
        assert_eq!(of("/home/me", &list), None, "a parent is not inside");
        let mut bare = wt("/srv/shop.git", true, None);
        bare.bare = true;
        assert_eq!(of("/srv/shop.git/hooks", &[bare]), None, "a bare repository has no checkout");
    }

    fn world_with_cards(cwds: &[&str]) -> (World, Vec<String>) {
        let mut w = World::default();
        w.projects.push(Project { id: "p".into(), name: "shop".into(), color: [0; 3], rect: Rect::ZERO, cwd: Some("~/shop".into()) });
        let mut ids = vec![];
        for cwd in cwds {
            let card = Card::new(Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0)), Some(cwd.to_string()));
            ids.push(card.id.clone());
            w.add_card(card);
        }
        (w, ids)
    }

    fn list(key: &str, worktrees: Vec<Worktree>) -> WorktreeList {
        WorktreeList { key: key.into(), repo: Some("/home/me/shop".into()), default_branch: Some("main".into()), worktrees }
    }

    #[test]
    fn cards_get_the_worktree_of_their_folder() {
        let (mut w, ids) = world_with_cards(&["/home/me/shop", "/home/me/shop/.claude/worktrees/invoices", "/home/me/blog"]);
        let mut s = Worktrees::default();
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", shop()))), &mut w, &mut vec![]);
        s.poll(&mut w);
        assert_eq!(w.card_worktrees.get(&ids[0]), Some(&CardWorktree { path: "/home/me/shop".into(), branch: Some("main".into()), linked: None }));
        assert_eq!(
            w.card_worktrees.get(&ids[1]),
            Some(&CardWorktree { path: "/home/me/shop/.claude/worktrees/invoices".into(), branch: Some("feat/invoices".into()), linked: Some("invoices".into()) })
        );
        assert_eq!(w.card_worktrees.get(&ids[2]), None);

        // Unlinking the folder forgets the repository, and the badges go.
        s.handle(&Event::SetProjectCwd("p".into(), None), &mut w, &mut vec![]);
        s.poll(&mut w);
        assert!(w.card_worktrees.is_empty());
    }

    #[test]
    fn relinking_the_same_folder_keeps_the_list() {
        let (mut w, _) = world_with_cards(&[]);
        let mut s = Worktrees::default();
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", shop()))), &mut w, &mut vec![]);
        s.handle(&Event::SetProjectCwd("p".into(), Some("~/shop".into())), &mut w, &mut vec![]);
        assert!(s.list("p").is_some(), "same folder: the agent sends nothing new, keep it");
        s.handle(&Event::SetProjectCwd("p".into(), Some("~/blog".into())), &mut w, &mut vec![]);
        assert!(s.list("p").is_none(), "another folder");
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", shop()))), &mut w, &mut vec![]);
        s.handle(&Event::RemoveProject("p".into()), &mut w, &mut vec![]);
        assert!(s.list("p").is_none(), "group removed");
    }

    #[test]
    fn the_strip_shows_only_for_the_open_group_with_linked_worktrees() {
        let (mut w, _) = world_with_cards(&[]);
        let mut s = Worktrees::default();
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", shop()))), &mut w, &mut vec![]);
        assert!(s.shown(&w).is_none(), "'All' view");
        w.space = Some("p".into());
        assert_eq!(s.shown(&w).map(|l| l.worktrees.len()), Some(3));
        // Only the main checkout: nothing to show.
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", vec![wt("/home/me/shop", true, Some("main"))]))), &mut w, &mut vec![]);
        assert!(s.shown(&w).is_none());
        // Outside git: the list is dropped.
        s.handle(&Event::Agent(AgentEvent::Worktrees(WorktreeList { key: "p".into(), repo: None, default_branch: None, worktrees: vec![] })), &mut w, &mut vec![]);
        assert!(s.list("p").is_none());
        // A group without a folder (or unknown) keeps nothing.
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("ghost", shop()))), &mut w, &mut vec![]);
        assert!(s.list("ghost").is_none());
    }

    #[test]
    fn names_and_ages() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        assert_eq!(folder_name(&wt("/home/me/shop-taxes/", false, None)), "shop-taxes");
        assert_eq!(age(10_000, 9_990), "à l'instant");
        assert_eq!(age(10_000, 10_000 - 7_200), "il y a 2 h");
        assert_eq!((t().last_commit)(&age(1_000_000, 1_000_000 - 3 * 86_400)), "Dernier commit il y a 3 j");
    }
}
