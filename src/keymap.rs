//! Key bindings. `BINDINGS` drives both dispatch and the `?` help popup.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Help popup grouping; has no effect on dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    General,
    Worktrees,
    Diff,
    Tree,
}

impl Section {
    pub fn title(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Worktrees => "Worktrees & history",
            Section::Diff => "Diff",
            Section::Tree => "File tree",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Help,
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
    CollapseRecursive,
    FoldAll,
    UnfoldAll,
    ExpandRecursive,
    NextHunk,
    PrevHunk,
    ToggleWrap,
    Escape,
}

pub struct Binding {
    pub section: Section,
    /// Key sequences: space-separated [`key_name`]s, e.g. "g t".
    pub keys: &'static [&'static str],
    pub action: Action,
    /// Help text label for the keys; empty hides the binding from help.
    pub label: &'static str,
    pub desc: &'static str,
}

const fn b(
    section: Section,
    keys: &'static [&'static str],
    action: Action,
    label: &'static str,
    desc: &'static str,
) -> Binding {
    Binding {
        section,
        keys,
        action,
        label,
        desc,
    }
}

use Action as A;
use Section::{Diff, General, Tree, Worktrees};

pub const BINDINGS: &[Binding] = &[
    b(General, &["q", "C-c"], A::Quit, "q  Ctrl-c", "Quit"),
    b(General, &["?"], A::Help, "?", "Toggle this help"),
    b(General, &["r"], A::Refresh, "r", "Force refresh"),
    b(
        General,
        &["y"],
        A::CopyRelative,
        "y",
        "Copy relative path (with :line if selected)",
    ),
    b(
        General,
        &["Y"],
        A::CopyAbsolute,
        "Y",
        "Copy absolute path (with :line if selected)",
    ),
    b(
        Worktrees,
        &["1"],
        A::Worktree(0),
        "1 … 9",
        "Switch to worktree N",
    ),
    b(Worktrees, &["2"], A::Worktree(1), "", ""),
    b(Worktrees, &["3"], A::Worktree(2), "", ""),
    b(Worktrees, &["4"], A::Worktree(3), "", ""),
    b(Worktrees, &["5"], A::Worktree(4), "", ""),
    b(Worktrees, &["6"], A::Worktree(5), "", ""),
    b(Worktrees, &["7"], A::Worktree(6), "", ""),
    b(Worktrees, &["8"], A::Worktree(7), "", ""),
    b(Worktrees, &["9"], A::Worktree(8), "", ""),
    b(
        Worktrees,
        &[",", "[", "g T"],
        A::PrevWorktree,
        ",  [  gT",
        "Previous worktree",
    ),
    b(
        Worktrees,
        &[".", "]", "g t"],
        A::NextWorktree,
        ".  ]  gt",
        "Next worktree",
    ),
    b(
        Worktrees,
        &["n"],
        A::NextFile,
        "n",
        "Next file (or collapsed directory)",
    ),
    b(
        Worktrees,
        &["p"],
        A::PrevFile,
        "p",
        "Previous file (or collapsed directory)",
    ),
    b(
        Worktrees,
        &["N"],
        A::NextCommit,
        "N",
        "Show next (older) commit",
    ),
    b(
        Worktrees,
        &["P"],
        A::PrevCommit,
        "P",
        "Show previous (newer) commit / working tree",
    ),
    b(
        Diff,
        &["j", "}"],
        A::NextHunk,
        "j  }",
        "Next hunk (continues into next file)",
    ),
    b(
        Diff,
        &["k", "{"],
        A::PrevHunk,
        "k  {",
        "Previous hunk (continues into previous file)",
    ),
    b(Diff, &["Down"], A::Down, "↓", "Next line"),
    b(Diff, &["Up"], A::Up, "↑", "Previous line"),
    b(Diff, &["h", "Left"], A::Left, "h  ←", "Scroll left"),
    b(Diff, &["l", "Right"], A::Right, "l  →", "Scroll right"),
    b(Diff, &["C-d"], A::HalfDown, "Ctrl-d", "Half page down"),
    b(Diff, &["C-u"], A::HalfUp, "Ctrl-u", "Half page up"),
    b(
        Diff,
        &["PageDown", "C-f"],
        A::PageDown,
        "PgDn  Ctrl-f",
        "Page down",
    ),
    b(
        Diff,
        &["PageUp", "C-b"],
        A::PageUp,
        "PgUp  Ctrl-b",
        "Page up",
    ),
    b(Diff, &["g g", "Home"], A::Top, "gg  Home", "Go to top"),
    b(Diff, &["G", "End"], A::Bottom, "G  End", "Go to bottom"),
    b(Diff, &["w"], A::ToggleWrap, "w", "Toggle soft wrap"),
    b(Diff, &["Esc"], A::Escape, "Esc", "Clear line selection"),
    b(
        Tree,
        &["Enter", "Space"],
        A::Toggle,
        "Enter  Space",
        "Toggle directory (of the current file)",
    ),
    b(
        Tree,
        &["H", "S-Left"],
        A::CollapseRecursive,
        "H  Shift-←",
        "Collapse directory and all subdirectories",
    ),
    b(
        Tree,
        &["L", "S-Right"],
        A::ExpandRecursive,
        "L  Shift-→",
        "Expand directory and all subdirectories",
    ),
    b(Tree, &["z"], A::FoldAll, "z", "Collapse all directories"),
    b(Tree, &["Z"], A::UnfoldAll, "Z", "Expand all directories"),
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
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
    Some(match ev.code {
        KeyCode::Char(c) if ctrl => format!("C-{}", c.to_ascii_lowercase()),
        KeyCode::Char(' ') => "Space".into(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Tab if shift => "S-Tab".into(),
        KeyCode::BackTab => "S-Tab".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Left if shift => "S-Left".into(),
        KeyCode::Right if shift => "S-Right".into(),
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

/// Resolves a key sequence.
pub fn lookup(seq: &str) -> Lookup {
    if let Some(b) = BINDINGS.iter().find(|b| b.keys.contains(&seq)) {
        return Lookup::Action(b.action);
    }
    if BINDINGS
        .iter()
        .any(|b| b.keys.iter().any(|k| k.starts_with(&format!("{seq} "))))
    {
        return Lookup::Pending;
    }
    Lookup::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_sequences() {
        assert!(matches!(lookup("h"), Lookup::Action(Action::Left)));
        assert!(matches!(lookup("Enter"), Lookup::Action(Action::Toggle)));
        assert!(matches!(lookup("g"), Lookup::Pending));
        assert!(matches!(lookup("."), Lookup::Action(Action::NextWorktree)));
        assert!(matches!(lookup("j"), Lookup::Action(Action::NextHunk)));
        assert!(matches!(
            lookup("S-Left"),
            Lookup::Action(Action::CollapseRecursive)
        ));
        assert!(matches!(lookup("Down"), Lookup::Action(Action::Down)));
        assert!(matches!(
            lookup("g t"),
            Lookup::Action(Action::NextWorktree)
        ));
        assert!(matches!(lookup("g g"), Lookup::Action(Action::Top)));
        assert!(matches!(lookup("z"), Lookup::Action(Action::FoldAll)));
        assert!(matches!(lookup("Z"), Lookup::Action(Action::UnfoldAll)));
        assert!(matches!(lookup("Tab"), Lookup::None));
        assert!(matches!(lookup("E"), Lookup::None));
        assert!(matches!(lookup("3"), Lookup::Action(Action::Worktree(2))));
    }
}
