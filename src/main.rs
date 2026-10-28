mod app;
mod clipboard;
mod diff;
mod git;
mod keymap;
mod theme;
mod tree;
mod ui;
mod watcher;
mod worker;

use anyhow::Result;
use app::{App, AppEvent};
use clap::Parser;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use std::sync::mpsc;
use std::thread;
use theme::{Theme, ThemeName};

/// Live terminal view of all git worktrees of the current repository.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Color theme: `auto` follows the terminal's color scheme.
    #[arg(long, value_enum, env = "WTM_THEME", default_value = "auto")]
    theme: ThemeName,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let repo = git::discover(&std::env::current_dir()?)?;

    let (tx, rx) = mpsc::channel();
    let jobs = worker::spawn(repo.toplevel.clone(), tx.clone());
    let app = App::new(repo, Theme::new(args.theme), jobs, tx.clone())?;
    watcher::spawn_ticker(tx.clone());

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        prev_hook(info);
    }));

    thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if tx.send(AppEvent::Input(ev)).is_err() {
                break;
            }
        }
    });

    let res = app::run(&mut terminal, app, rx);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}
