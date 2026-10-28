//! Key bindings. `BINDINGS` drives both dispatch and the `?` help popup.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Tree,
    Diff,
    Commits,
}

impl Scope {
    pub fn title(self) -> &'static str {
        match self {
            Scope::Global => "Global",
            Scope::Tree => "File tree",
            Scope::Diff => "Diff",
            Scope::Commits => "Commits",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Help,
    FocusNext,
    FocusPrev,
    Worktree(usize),
    PrevWorktree,
    NextWorktree,
    NextFile,
    PrevFile,
    NextCommit,
    PrevCommit,
    CopyRelative,
    CopyAbsolute,
    Refresh,
    Down,
    Up,
    HalfDown,
    HalfUp,
    PageDown,
    PageUp,
    Top,
    Bottom,
    Left,
    Right,
    Toggle,
    NextHunk,
    PrevHunk,
    Escape,
}

pub struct Binding {
    pub scope: Scope,
    /// Key sequences: space-separated [`key_name`]s, e.g. "g t".
    pub keys: &'static [&'static str],
    pub action: Action,
    /// Help text label for the keys; empty hides the binding from help.
    pub label: &'static str,
    pub desc: &'static str,
}

const fn b(
    scope: Scope,
    keys: &'static [&'static str],
    action: Action,
    label: &'static str,
    desc: &'static str,
) -> Binding {
    Binding {
        scope,
        keys,
        action,
        label,
        desc,
    }
}

use Action as A;
use Scope::{Commits, Diff, Global, Tree};

pub const BINDINGS: &[Binding] = &[
    b(Global, &["q", "C-c"], A::Quit, "q  Ctrl-c", "Quit"),
    b(Global, &["?"], A::Help, "?", "Toggle this help"),
    b(Global, &["Tab"], A::FocusNext, "Tab", "Focus next pane"),
    b(
        Global,
        &["S-Tab"],
        A::FocusPrev,
        "Shift-Tab",
        "Focus previous pane",
    ),
    b(
        Global,
        &["1"],
        A::Worktree(0),
        "1 … 9",
        "Switch to worktree N",
    ),
    b(Global, &["2"], A::Worktree(1), "", ""),
    b(Global, &["3"], A::Worktree(2), "", ""),
    b(Global, &["4"], A::Worktree(3), "", ""),
    b(Global, &["5"], A::Worktree(4), "", ""),
    b(Global, &["6"], A::Worktree(5), "", ""),
    b(Global, &["7"], A::Worktree(6), "", ""),
    b(Global, &["8"], A::Worktree(7), "", ""),
    b(Global, &["9"], A::Worktree(8), "", ""),
    b(
        Global,
        &[",", "[", "g T"],
        A::PrevWorktree,
        ",  [  gT",
        "Previous worktree",
    ),
    b(
        Global,
        &[".", "]", "g t"],
        A::NextWorktree,
        ".  ]  gt",
        "Next worktree",
    ),
    b(Global, &["n"], A::NextFile, "n", "Next file"),
    b(Global, &["p"], A::PrevFile, "p", "Previous file"),
    b(
        Global,
        &["N"],
        A::NextCommit,
        "N",
        "Show next (older) commit",
    ),
    b(
        Global,
        &["P"],
        A::PrevCommit,
        "P",
        "Show previous (newer) commit / working tree",
    ),
    b(
        Global,
        &["y"],
        A::CopyRelative,
        "y",
        "Copy relative path (with :line if selected)",
    ),
    b(
        Global,
        &["Y"],
        A::CopyAbsolute,
        "Y",
        "Copy absolute path (with :line if selected)",
    ),
    b(Global, &["r"], A::Refresh, "r", "Force refresh"),
    // Shared movement, interpreted per pane.
    b(Global, &["j", "Down"], A::Down, "j  ↓", "Move down"),
    b(Global, &["k", "Up"], A::Up, "k  ↑", "Move up"),
    b(Global, &["C-d"], A::HalfDown, "Ctrl-d", "Half page down"),
    b(Global, &["C-u"], A::HalfUp, "Ctrl-u", "Half page up"),
    b(
        Global,
        &["PageDown", "C-f"],
        A::PageDown,
        "PgDn  Ctrl-f",
        "Page down",
    ),
    b(
        Global,
        &["PageUp", "C-b"],
        A::PageUp,
        "PgUp  Ctrl-b",
        "Page up",
    ),
    b(Global, &["g g", "Home"], A::Top, "gg  Home", "Go to top"),
    b(Global, &["G", "End"], A::Bottom, "G  End", "Go to bottom"),
    b(
        Tree,
        &["h", "Left"],
        A::Left,
        "h  ←",
        "Collapse directory / go to parent",
    ),
    b(Tree, &["l", "Right"], A::Right, "l  →", "Expand directory"),
    b(
        Tree,
        &["Enter", "Space"],
        A::Toggle,
        "Enter  Space",
        "Toggle directory",
    ),
    b(Diff, &["h", "Left"], A::Left, "h  ←", "Scroll left"),
    b(Diff, &["l", "Right"], A::Right, "l  →", "Scroll right"),
    b(Diff, &["J", "}"], A::NextHunk, "J  }", "Next hunk"),
    b(Diff, &["K", "{"], A::PrevHunk, "K  {", "Previous hunk"),
    b(Diff, &["Esc"], A::Escape, "Esc", "Clear line selection"),
    b(
        Commits,
        &["Enter", "Space"],
        A::Toggle,
        "Enter  Space",
        "Show commit (top row: working tree)",
    ),
    b(Commits, &["Esc"], A::Escape, "Esc", "Back to working tree"),
];

/// Mouse help shown alongside key bindings.
pub const MOUSE_HELP: &[(&str, &str)] = &[
    ("Click", "Select tab / file / diff line / commit"),
    ("Wheel", "Scroll pane under cursor"),
];

/// Canonical name for a key event: printable chars as-is ("j", "G", "?"),
/// Ctrl+char as "C-x", other keys by name ("Tab", "S-Tab", "Down", "Enter").
pub fn key_name(ev: &KeyEvent) -> Option<String> {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    Some(match ev.code {
        KeyCode::Char(c) if ctrl => format!("C-{}", c.to_ascii_lowercase()),
        KeyCode::Char(' ') => "Space".into(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Tab if ev.modifiers.contains(KeyModifiers::SHIFT) => "S-Tab".into(),
        KeyCode::BackTab => "S-Tab".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::PageUp => "PageUp".into(),
        KeyCode::PageDown => "PageDown".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        _ => return None,
    })
}

pub enum Lookup {
    Action(Action),
    /// The sequence is a prefix of a longer binding; wait for more keys.
    Pending,
    None,
}

/// Resolves a key sequence; pane-specific bindings take precedence over global ones.
pub fn lookup(seq: &str, pane: Scope) -> Lookup {
    let scoped = BINDINGS.iter().filter(|b| b.scope == pane);
    let global = BINDINGS.iter().filter(|b| b.scope == Scope::Global);
    let candidates = || scoped.clone().chain(global.clone());
    if let Some(b) = candidates().find(|b| b.keys.contains(&seq)) {
        return Lookup::Action(b.action);
    }
    if candidates().any(|b| b.keys.iter().any(|k| k.starts_with(&format!("{seq} ")))) {
        return Lookup::Pending;
    }
    Lookup::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_scoped_and_sequences() {
        assert!(matches!(
            lookup("h", Scope::Tree),
            Lookup::Action(Action::Left)
        ));
        assert!(matches!(lookup("h", Scope::Commits), Lookup::None));
        assert!(matches!(
            lookup("Enter", Scope::Commits),
            Lookup::Action(Action::Toggle)
        ));
        assert!(matches!(lookup("g", Scope::Diff), Lookup::Pending));
        assert!(matches!(
            lookup(".", Scope::Commits),
            Lookup::Action(Action::NextWorktree)
        ));
        assert!(matches!(
            lookup("J", Scope::Diff),
            Lookup::Action(Action::NextHunk)
        ));
        assert!(matches!(
            lookup("g t", Scope::Diff),
            Lookup::Action(Action::NextWorktree)
        ));
        assert!(matches!(
            lookup("g g", Scope::Tree),
            Lookup::Action(Action::Top)
        ));
        assert!(matches!(lookup("E", Scope::Tree), Lookup::None));
        assert!(matches!(
            lookup("3", Scope::Tree),
            Lookup::Action(Action::Worktree(2))
        ));
    }
}
