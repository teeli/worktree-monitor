mod app;
mod clipboard;
mod comments;
mod diff;
mod git;
mod input;
mod keymap;
mod theme;
mod tree;
mod ui;
mod watcher;
mod worker;

use anyhow::Result;
use app::{App, AppEvent};
use clap::{Parser, Subcommand};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
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
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List comment files of removed worktrees across all repositories, and delete them with
    /// `--yes`. Doesn't need a repository.
    Prune {
        /// Delete the listed files instead of only listing them.
        #[arg(long, short)]
        yes: bool,
        /// Also include files whose repository no longer exists on disk, or wasn't recorded
        /// (files from older versions). Careful: a repository on a volume that isn't mounted
        /// looks deleted too.
        #[arg(long)]
        deleted_repos: bool,
        /// Also include files whose comments weren't added or edited in this many days, even
        /// if their worktree still exists.
        #[arg(long, value_name = "DAYS")]
        age: Option<u64>,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(Command::Prune {
        yes,
        deleted_repos,
        age,
    }) = args.command
    {
        return prune(yes, deleted_repos, age);
    }
    let repo = git::discover(&std::env::current_dir()?)?;

    let (tx, rx) = mpsc::channel();
    let jobs = worker::spawn(repo.toplevel.clone(), tx.clone());
    let app = App::new(repo, Theme::new(args.theme), jobs, tx.clone())?;
    watcher::spawn_ticker(tx.clone());

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
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
    let _ = execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste
    );
    ratatui::restore();
    res
}

/// `wtm prune`: lists (and with `yes`, deletes) stale comment files.
fn prune(yes: bool, deleted_repos: bool, age: Option<u64>) -> Result<()> {
    use comments::{Gone, Stale};
    let (dir, entries) = comments::scan()?;
    let now = comments::now().0;
    let mut picked = Vec::new();
    let mut skipped = 0;
    for e in &entries {
        match e.stale(deleted_repos, age, now) {
            Some(why) => picked.push((e, why)),
            None if matches!(e.gone, Some(Gone::Repo | Gone::Unknown)) => skipped += 1,
            None => {}
        }
    }
    if picked.is_empty() {
        println!("No stale comment files in {}", dir.display());
    } else {
        println!("Stale comment files in {}:", dir.display());
        for (e, why) in &picked {
            let s = if e.comments == 1 { "" } else { "s" };
            let why = match why {
                Stale::Gone(Gone::Worktree) => "worktree removed".to_string(),
                Stale::Gone(Gone::Repo) => "repository missing".to_string(),
                Stale::Gone(Gone::Unknown) => "repository unknown".to_string(),
                Stale::Idle(1) => "untouched for 1 day".to_string(),
                Stale::Idle(days) => format!("untouched for {days} days"),
            };
            println!(
                "  {:>3} comment{s:<1}  {}  ({why})",
                e.comments,
                e.worktree.display()
            );
        }
    }
    if skipped > 0 {
        let s = if skipped == 1 { "" } else { "s" };
        println!(
            "{skipped} file{s} of missing or unknown repositories skipped; add --deleted-repos to include them"
        );
    }
    if picked.is_empty() {
        return Ok(());
    }
    if !yes {
        println!("Run with --yes to delete them");
        return Ok(());
    }
    let mut failed = 0;
    for (e, _) in &picked {
        if let Err(err) = std::fs::remove_file(&e.path) {
            eprintln!("removing {}: {err}", e.path.display());
            failed += 1;
        }
    }
    let removed = picked.len() - failed;
    let s = if removed == 1 { "" } else { "s" };
    println!("Removed {removed} file{s}");
    if failed > 0 {
        anyhow::bail!("{failed} file(s) could not be removed");
    }
    Ok(())
}
