use crate::git::FileStatus;
use clap::ValueEnum;
use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ThemeName {
    /// Use the terminal's own color scheme (ANSI colors).
    Auto,
    Dracula,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub base: Style,
    pub dim: Style,
    pub border: Style,
    pub border_focused: Style,
    pub title: Style,
    pub tab: Style,
    pub tab_active: Style,
    pub badge: Style,
    /// Cursor row in the focused pane.
    pub cursor: Style,
    /// Cursor row in an unfocused pane.
    pub cursor_inactive: Style,
    /// Selected diff line: background only, so diff colors stay intact.
    pub diff_sel: Style,
    pub diff_sel_inactive: Style,
    pub add: Style,
    pub del: Style,
    pub hunk: Style,
    pub file_header: Style,
    pub sha: Style,
    pub refs: Style,
    pub dir: Style,
    pub modified: Style,
    pub untracked: Style,
    pub renamed: Style,
    pub conflict: Style,
    pub status_pill: Style,
    pub error: Style,
}

impl Theme {
    pub fn new(name: ThemeName) -> Self {
        match name {
            ThemeName::Auto => Self::auto(),
            ThemeName::Dracula => Self::dracula(),
        }
    }

    fn auto() -> Self {
        let s = Style::new();
        Theme {
            base: s,
            dim: s.fg(Color::DarkGray),
            border: s.fg(Color::DarkGray),
            border_focused: s.fg(Color::Cyan),
            title: s.add_modifier(Modifier::BOLD),
            tab: s,
            tab_active: s
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            badge: s.fg(Color::Yellow),
            cursor: s.add_modifier(Modifier::REVERSED),
            cursor_inactive: s.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            // 256-color grays: reliably a little lighter than a dark background.
            diff_sel: Style::new().bg(Color::Indexed(238)),
            diff_sel_inactive: Style::new().bg(Color::Indexed(236)),
            add: s.fg(Color::Green),
            del: s.fg(Color::Red),
            hunk: s.fg(Color::Cyan),
            file_header: s.fg(Color::DarkGray).add_modifier(Modifier::BOLD),
            sha: s.fg(Color::Yellow),
            refs: s.fg(Color::Cyan),
            dir: s.fg(Color::Blue).add_modifier(Modifier::BOLD),
            modified: s.fg(Color::Yellow),
            untracked: s.fg(Color::Magenta),
            renamed: s.fg(Color::Cyan),
            conflict: s.fg(Color::Red).add_modifier(Modifier::BOLD),
            status_pill: s
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            error: s.fg(Color::Red),
        }
    }

    fn dracula() -> Self {
        const BG: Color = Color::Rgb(0x28, 0x2a, 0x36);
        const LINE: Color = Color::Rgb(0x44, 0x47, 0x5a);
        const FG: Color = Color::Rgb(0xf8, 0xf8, 0xf2);
        const COMMENT: Color = Color::Rgb(0x62, 0x72, 0xa4);
        const CYAN: Color = Color::Rgb(0x8b, 0xe9, 0xfd);
        const GREEN: Color = Color::Rgb(0x50, 0xfa, 0x7b);
        const ORANGE: Color = Color::Rgb(0xff, 0xb8, 0x6c);
        const PINK: Color = Color::Rgb(0xff, 0x79, 0xc6);
        const PURPLE: Color = Color::Rgb(0xbd, 0x93, 0xf9);
        const RED: Color = Color::Rgb(0xff, 0x55, 0x55);
        const YELLOW: Color = Color::Rgb(0xf1, 0xfa, 0x8c);
        let s = Style::new().fg(FG).bg(BG);
        Theme {
            base: s,
            dim: s.fg(COMMENT),
            border: s.fg(COMMENT),
            border_focused: s.fg(PURPLE),
            title: s.fg(FG).add_modifier(Modifier::BOLD),
            tab: s.fg(FG),
            tab_active: s.fg(BG).bg(PURPLE).add_modifier(Modifier::BOLD),
            badge: s.fg(ORANGE),
            cursor: Style::new().bg(LINE).add_modifier(Modifier::BOLD),
            cursor_inactive: Style::new().bg(Color::Rgb(0x34, 0x37, 0x46)),
            diff_sel: Style::new().bg(LINE),
            diff_sel_inactive: Style::new().bg(Color::Rgb(0x34, 0x37, 0x46)),
            add: s.fg(GREEN),
            del: s.fg(RED),
            hunk: s.fg(PURPLE),
            file_header: s.fg(COMMENT).add_modifier(Modifier::BOLD),
            sha: s.fg(YELLOW),
            refs: s.fg(CYAN),
            dir: s.fg(PURPLE).add_modifier(Modifier::BOLD),
            modified: s.fg(ORANGE),
            untracked: s.fg(PINK),
            renamed: s.fg(CYAN),
            conflict: s.fg(RED).add_modifier(Modifier::BOLD),
            status_pill: s.fg(BG).bg(PINK).add_modifier(Modifier::BOLD),
            error: s.fg(RED),
        }
    }

    pub fn file_status(&self, st: FileStatus) -> Style {
        match st {
            FileStatus::Modified => self.modified,
            FileStatus::Added => self.add,
            FileStatus::Deleted => self.del,
            FileStatus::Renamed | FileStatus::Copied => self.renamed,
            FileStatus::Untracked => self.untracked,
            FileStatus::Conflicted => self.conflict,
        }
    }
}
