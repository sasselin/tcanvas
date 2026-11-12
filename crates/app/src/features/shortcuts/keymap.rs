//! Every shortcut and mouse gesture of the app, as data: the global shortcuts are routed from
//! `GLOBAL`, and the "Keyboard shortcuts" window lists `sections()`. Pure, no UI.

use egui::Key;

use crate::shell::i18n::{Texts, t};

/// Global shortcuts: always for the app, even when a terminal has focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    Jump,
    NewCard,
    Search,
    FitAll,
    /// Project #1 to #9 (creation order).
    Project(usize),
    CloseCard,
    /// The "Keyboard shortcuts" window.
    Help,
    /// The "Tasks" panel.
    Tasks,
}

/// Ctrl+Shift+<letter> shortcuts. Digits (projects, fit all) are handled apart: physical key.
pub const GLOBAL: &[(Shortcut, Key)] =
    &[(Shortcut::Jump, Key::P), (Shortcut::NewCard, Key::N), (Shortcut::Search, Key::F), (Shortcut::CloseCard, Key::W), (Shortcut::Help, Key::K), (Shortcut::Tasks, Key::T)];

const DIGITS: [Key; 10] = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];

pub fn shortcut_of(ev: &egui::Event) -> Option<Shortcut> {
    let egui::Event::Key { key, physical_key, pressed: true, modifiers, .. } = ev else { return None };
    if !(modifiers.ctrl && modifiers.shift && !modifiers.alt) {
        return None;
    }
    // Digits: physical key, so Ctrl+Shift+1 also works on AZERTY (where 1 is on "&").
    let digit = [physical_key.as_ref(), Some(key)].into_iter().flatten().find_map(|k| DIGITS.iter().position(|d| d == k));
    if let Some(n) = digit {
        return Some(if n == 0 { Shortcut::FitAll } else { Shortcut::Project(n) });
    }
    GLOBAL.iter().find(|(_, k)| k == key).map(|(s, _)| *s)
}

/// What triggers an action: modifiers plus a key or a mouse gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Combo {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub input: Input,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Key(Key),
    /// 1 to 9.
    Digits,
    Click,
    DoubleClick,
    RightClick,
    Drag,
    MiddleDrag,
    Wheel,
}

impl Combo {
    pub const fn plain(input: Input) -> Self {
        Self { ctrl: false, shift: false, alt: false, input }
    }

    pub const fn ctrl(input: Input) -> Self {
        Self { ctrl: true, ..Self::plain(input) }
    }

    pub const fn shift(input: Input) -> Self {
        Self { shift: true, ..Self::plain(input) }
    }

    pub const fn alt(input: Input) -> Self {
        Self { alt: true, ..Self::plain(input) }
    }

    pub const fn ctrl_shift(input: Input) -> Self {
        Self { ctrl: true, shift: true, ..Self::plain(input) }
    }

    /// "Ctrl+Shift+K", "Alt+right-click"… in the UI language.
    pub fn format(&self) -> String {
        let tx = t();
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_owned());
        }
        if self.shift {
            parts.push(tx.shift_key.to_owned());
        }
        if self.alt {
            parts.push("Alt".to_owned());
        }
        parts.push(input_name(tx, self.input));
        parts.join("+")
    }
}

fn input_name(tx: &Texts, input: Input) -> String {
    match input {
        Input::Key(k) => k.name().to_owned(),
        Input::Digits => "1…9".to_owned(),
        Input::Click => tx.mouse_click.to_owned(),
        Input::DoubleClick => tx.mouse_double_click.to_owned(),
        Input::RightClick => tx.mouse_right_click.to_owned(),
        Input::Drag => tx.mouse_drag.to_owned(),
        Input::MiddleDrag => tx.mouse_middle_drag.to_owned(),
        Input::Wheel => tx.mouse_wheel.to_owned(),
    }
}

pub struct Row {
    /// Alternatives: any of them does the action.
    pub combos: Vec<Combo>,
    pub action: &'static str,
}

pub struct Section {
    pub title: &'static str,
    pub rows: Vec<Row>,
}

/// What a global shortcut does, for the list.
pub fn action(tx: &Texts, s: Shortcut) -> &'static str {
    match s {
        Shortcut::Jump => tx.key_jump,
        Shortcut::NewCard => tx.key_new_card,
        Shortcut::Search => tx.key_search,
        Shortcut::FitAll => tx.key_fit_all,
        Shortcut::Project(_) => tx.key_project,
        Shortcut::CloseCard => tx.key_close_card,
        Shortcut::Help => tx.key_help,
        Shortcut::Tasks => tx.key_tasks,
    }
}

/// The combo of the "Tasks" panel, shown in its button's hint.
pub fn tasks_combo() -> Combo {
    let key = GLOBAL.iter().find(|(s, _)| *s == Shortcut::Tasks).map_or(Key::T, |(_, k)| *k);
    Combo::ctrl_shift(Input::Key(key))
}

/// The combo of the "Keyboard shortcuts" window, shown next to its entries.
pub fn help_combo() -> Combo {
    let key = GLOBAL.iter().find(|(s, _)| *s == Shortcut::Help).map_or(Key::K, |(_, k)| *k);
    Combo::ctrl_shift(Input::Key(key))
}

pub fn sections() -> Vec<Section> {
    let tx = t();
    let row = |combos: Vec<Combo>, action: &'static str| Row { combos, action };
    let mut global: Vec<Row> = GLOBAL.iter().map(|(s, k)| row(vec![Combo::ctrl_shift(Input::Key(*k))], action(tx, *s))).collect();
    global.push(row(vec![Combo::ctrl_shift(Input::Digits)], action(tx, Shortcut::Project(1))));
    global.push(row(vec![Combo::ctrl_shift(Input::Key(Key::Num0))], action(tx, Shortcut::FitAll)));
    vec![
        Section { title: tx.keys_global, rows: global },
        Section {
            title: tx.keys_terminal,
            rows: vec![
                row(vec![Combo::ctrl(Input::Key(Key::C))], tx.key_interrupt),
                row(vec![Combo::ctrl_shift(Input::Key(Key::C))], tx.key_copy),
                row(vec![Combo::ctrl(Input::Key(Key::V)), Combo::ctrl_shift(Input::Key(Key::V)), Combo::plain(Input::RightClick)], tx.key_paste),
                row(vec![Combo::alt(Input::RightClick)], tx.key_tmux_menu),
                row(vec![Combo::plain(Input::Click)], tx.key_focus_card),
            ],
        },
        Section {
            title: tx.keys_canvas,
            rows: vec![
                row(vec![Combo::plain(Input::Wheel), Combo::plain(Input::Drag), Combo::plain(Input::MiddleDrag)], tx.key_pan),
                row(vec![Combo::shift(Input::Drag)], tx.key_hand),
                row(vec![Combo::ctrl(Input::Wheel)], tx.key_zoom),
                row(vec![Combo::plain(Input::DoubleClick)], tx.key_new_card_here),
                row(vec![Combo::plain(Input::Click)], tx.key_minimap),
            ],
        },
        Section {
            title: tx.keys_cards,
            rows: vec![
                row(vec![Combo::plain(Input::Drag)], tx.key_move_card),
                row(vec![Combo::plain(Input::DoubleClick)], tx.key_fly_to),
                row(vec![Combo::plain(Input::RightClick)], tx.key_card_menu),
                row(vec![Combo::plain(Input::Drag)], tx.key_resize),
                row(vec![Combo::plain(Input::Drag)], tx.key_move_group),
                row(vec![Combo::alt(Input::Drag)], tx.key_link_groups),
                row(vec![Combo::plain(Input::DoubleClick)], tx.key_rename_group),
                row(vec![Combo::plain(Input::RightClick)], tx.key_delete_wire),
                row(vec![Combo::plain(Input::Drag)], tx.key_reorder_chips),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::i18n::{self, Lang};
    use egui::Modifiers;

    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    #[test]
    fn shortcuts_need_ctrl_shift_and_no_alt() {
        let cs = Modifiers::CTRL | Modifiers::SHIFT;
        assert_eq!(shortcut_of(&key(Key::P, cs)), Some(Shortcut::Jump));
        assert_eq!(shortcut_of(&key(Key::K, cs)), Some(Shortcut::Help));
        assert_eq!(shortcut_of(&key(Key::J, cs)), None);
        assert_eq!(shortcut_of(&key(Key::N, cs)), Some(Shortcut::NewCard));
        assert_eq!(shortcut_of(&key(Key::T, cs)), Some(Shortcut::Tasks));
        assert_eq!(shortcut_of(&key(Key::P, Modifiers::CTRL)), None, "Ctrl+P stays with the shell");
        assert_eq!(shortcut_of(&key(Key::P, cs | Modifiers::ALT)), None);
        assert_eq!(shortcut_of(&egui::Event::Text("K".into())), None);
    }

    #[test]
    fn digit_shortcuts_use_the_physical_key() {
        let cs = Modifiers::CTRL | Modifiers::SHIFT;
        assert_eq!(shortcut_of(&key(Key::Num3, cs)), Some(Shortcut::Project(3)));
        assert_eq!(shortcut_of(&key(Key::Num0, cs)), Some(Shortcut::FitAll));
        // AZERTY: the "&" key produces a logical key with no digit, but the physical key is 1.
        let azerty = egui::Event::Key { key: Key::Exclamationmark, physical_key: Some(Key::Num1), pressed: true, repeat: false, modifiers: cs };
        assert_eq!(shortcut_of(&azerty), Some(Shortcut::Project(1)));
        assert_eq!(shortcut_of(&key(Key::Num1, Modifiers::CTRL)), None);
    }

    /// The window shows what the keyboard routing actually does.
    #[test]
    fn listed_global_keys_trigger_their_shortcut() {
        i18n::set(Lang::En);
        let global = &sections()[0];
        for (s, k) in GLOBAL {
            assert_eq!(shortcut_of(&key(*k, Modifiers::CTRL | Modifiers::SHIFT)), Some(*s));
            let listed = global.rows.iter().find(|r| r.combos.contains(&Combo::ctrl_shift(Input::Key(*k)))).expect("listed");
            assert_eq!(listed.action, action(t(), *s));
        }
        for row in &global.rows {
            for combo in &row.combos {
                assert!(combo.ctrl && combo.shift && !combo.alt, "global shortcuts are Ctrl+Shift");
                let probe = match combo.input {
                    Input::Key(k) => k,
                    Input::Digits => Key::Num5,
                    other => panic!("no mouse in global shortcuts: {other:?}"),
                };
                let s = shortcut_of(&key(probe, Modifiers::CTRL | Modifiers::SHIFT)).expect("routed");
                assert_eq!(row.action, action(t(), s));
            }
        }
    }

    #[test]
    fn combos_read_in_the_ui_language() {
        i18n::set(Lang::Fr);
        assert_eq!(help_combo().format(), "Ctrl+Maj+K");
        assert_eq!(Combo::alt(Input::RightClick).format(), "Alt+clic droit");
        assert_eq!(Combo::ctrl_shift(Input::Digits).format(), "Ctrl+Maj+1…9");
        i18n::set(Lang::En);
        assert_eq!(help_combo().format(), "Ctrl+Shift+K");
        assert_eq!(Combo::ctrl(Input::Wheel).format(), "Ctrl+wheel");
    }
}
