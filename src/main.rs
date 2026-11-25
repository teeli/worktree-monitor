mod app;
mod cli;
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
        #[arg(long, value_name = "DAYS", value_parser = clap::value_parser!(u64).range(1..))]
        age: Option<u64>,
    },
    #[command(flatten)]
    Cli(cli::Command),
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
    if let Some(Command::Cli(cmd)) = args.command {
        return cli::run(cmd);
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
    use comments::Gone;
    let (dir, entries) = comments::scan()?;
    let now = comments::now().0;
    let mut picked = Vec::new();
    let (mut skipped, mut listed) = (0, 0);
    for e in &entries {
        match (e.stale(deleted_repos, age, now), e.gone) {
            (Some(why), _) => picked.push((e, why)),
            (None, Some(Gone::Repo | Gone::Unknown)) => skipped += 1,
            (None, Some(Gone::Listed)) => listed += 1,
            (None, _) => {}
        }
    }
    if picked.is_empty() {
        println!("No stale comment files in {}", dir.display());
    } else {
        println!("Stale comment files in {}:", dir.display());
        for (e, why) in &picked {
            println!(
                "  {:>3} comment{:<1}  {}  ({why})",
                e.comments,
                plural(e.comments),
                e.worktree.display()
            );
        }
    }
    if skipped > 0 {
        println!(
            "{skipped} file{} of missing or unknown repositories skipped; add --deleted-repos to include them",
            plural(skipped)
        );
    }
    if listed > 0 {
        println!(
            "{listed} file{} of missing worktrees their repository still lists skipped; if they were deleted, run `git worktree prune` there first",
            plural(listed)
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
    println!("Removed {removed} file{}", plural(removed));
    if failed > 0 {
        anyhow::bail!("{failed} file{} could not be removed", plural(failed));
    }
    Ok(())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
