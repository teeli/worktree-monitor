//! `wtm comments …`: reading and closing the current worktree's comments without the TUI,
//! e.g. by an AI agent that handled them, and cleaning up comment files of removed worktrees.

use crate::comments::{self, Closed, Comment, Entry, Gone, Store};
use crate::git;
use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use std::io::Write;
use std::path::Path;

#[derive(Subcommand)]
pub enum Command {
    /// List, close or reopen the comments of the current worktree; `prune` cleans up stale
    /// comment files of all repositories.
    #[command(subcommand)]
    Comments(CommentsCmd),
}

#[derive(Subcommand)]
pub enum CommentsCmd {
    #[command(flatten)]
    Worktree(WorktreeCmd),
    /// List comment files of removed worktrees across all repositories, and delete them with
    /// `--yes`. Doesn't need a repository.
    Prune(PruneArgs),
}

/// Commands on the comments of the current worktree.
#[derive(Subcommand)]
pub enum WorktreeCmd {
    /// List open comments: id, file:line, commit (if any), then the indented body.
    List {
        /// Include closed comments.
        #[arg(long)]
        all: bool,
        /// Print the comments as a JSON array in the format of schema/comments.schema.json,
        /// except that ids are strings: they exceed 2^53, which many JSON parsers round.
        #[arg(long)]
        json: bool,
    },
    /// Close comments that were fixed or dropped.
    Close {
        #[arg(required = true, value_name = "ID")]
        ids: Vec<u64>,
        /// Who closed them, e.g. the agent's name.
        #[arg(long, default_value = "agent")]
        by: String,
    },
    /// Reopen closed comments.
    Reopen {
        #[arg(required = true, value_name = "ID")]
        ids: Vec<u64>,
    },
}

#[derive(Args)]
pub struct PruneArgs {
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
}

pub fn run(cmd: Command) -> Result<()> {
    let Command::Comments(cmd) = cmd;
    match cmd {
        CommentsCmd::Worktree(cmd) => {
            let repo = git::discover(&std::env::current_dir()?)?;
            let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let mut store = Store::load(&canon(&repo.toplevel), &canon(&repo.common_dir));
            comments_cmd(cmd, &mut store, &mut std::io::stdout().lock())
        }
        CommentsCmd::Prune(args) => {
            let (dir, entries) = comments::scan()?;
            let now = comments::now().0;
            let out = &mut std::io::stdout().lock();
            prune(&dir, &entries, &args, now, out, &mut std::io::stderr())
        }
    }
}

fn comments_cmd(cmd: WorktreeCmd, store: &mut Store, out: &mut impl Write) -> Result<()> {
    if let Some(e) = &store.error {
        bail!("comments unreadable: {e}");
    }
    let (ids, closed) = match cmd {
        WorktreeCmd::List { all, json } => return list(store, all, json, out),
        WorktreeCmd::Close { ids, by } => {
            if by.trim().is_empty() {
                bail!("--by must not be empty");
            }
            (ids, Some(by))
        }
        WorktreeCmd::Reopen { ids } => (ids, None),
    };
    let verb = if closed.is_some() {
        "Closed"
    } else {
        "Reopened"
    };
    let mut missing = Vec::new();
    for id in ids {
        if store.set_closed(id, closed.as_deref().map(Closed::now))? {
            writeln!(out, "{verb} {id}")?;
        } else {
            missing.push(id.to_string());
        }
    }
    if !missing.is_empty() {
        bail!("no comment with id {}", missing.join(", "));
    }
    Ok(())
}

fn list(store: &Store, all: bool, json: bool, out: &mut impl Write) -> Result<()> {
    let shown: Vec<&Comment> = comments::order(&store.comments, all, |_| None)
        .into_iter()
        .map(|i| &store.comments[i])
        .collect();
    if json {
        let shown = shown
            .into_iter()
            .map(|c| {
                let mut v = serde_json::to_value(c)?;
                v["id"] = c.id.to_string().into();
                Ok(v)
            })
            .collect::<Result<Vec<_>>>()?;
        serde_json::to_writer_pretty(&mut *out, &shown)?;
        writeln!(out)?;
        return Ok(());
    }
    if shown.is_empty() {
        writeln!(out, "No {}comments", if all { "" } else { "open " })?;
    }
    for c in shown {
        let mut head = format!("{}  {}:", c.id, c.file);
        match &c.start {
            Some(s) if s.line != c.line => head += &format!("{}-{}", s.line, c.line),
            _ => head += &c.line.to_string(),
        }
        if let Some(sha) = &c.commit {
            head += &format!(" @ {}", sha.get(..7).unwrap_or(sha));
        }
        if let Some(closed) = &c.closed {
            head += &format!(" · closed by {}", closed.by);
        }
        writeln!(out, "{head}")?;
        for line in c.body.lines() {
            writeln!(out, "    {line}")?;
        }
    }
    Ok(())
}

/// Lists the stale comment files among `entries` (in `dir`) and, with `--yes`, deletes them.
/// Deletion failures go to `err`; any makes it fail after trying the rest.
fn prune(
    dir: &Path,
    entries: &[Entry],
    args: &PruneArgs,
    now: u64,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<()> {
    let mut picked = Vec::new();
    let (mut skipped, mut listed) = (0, 0);
    for e in entries {
        match (e.stale(args.deleted_repos, args.age, now), e.gone) {
            (Some(why), _) => picked.push((e, why)),
            (None, Some(Gone::Repo | Gone::Unknown)) => skipped += 1,
            (None, Some(Gone::Listed)) => listed += 1,
            (None, _) => {}
        }
    }
    if picked.is_empty() {
        writeln!(out, "No stale comment files in {}", dir.display())?;
    } else {
        writeln!(out, "Stale comment files in {}:", dir.display())?;
        for (e, why) in &picked {
            writeln!(
                out,
                "  {:>3} comment{:<1}  {}  ({why})",
                e.comments,
                plural(e.comments),
                e.worktree.display()
            )?;
        }
    }
    if skipped > 0 {
        writeln!(
            out,
            "{skipped} file{} of missing or unknown repositories skipped; add --deleted-repos to include them",
            plural(skipped)
        )?;
    }
    if listed > 0 {
        writeln!(
            out,
            "{listed} file{} of missing worktrees their repository still lists skipped; if they were deleted, run `git worktree prune` there first",
            plural(listed)
        )?;
    }
    if picked.is_empty() {
        return Ok(());
    }
    if !args.yes {
        writeln!(out, "Run with --yes to delete them")?;
        return Ok(());
    }
    let mut failed = 0;
    for (e, _) in &picked {
        if let Err(io) = std::fs::remove_file(&e.path) {
            // Best effort: an unwritable stderr mustn't stop the remaining deletions.
            let _ = writeln!(err, "removing {}: {io}", e.path.display());
            failed += 1;
        }
    }
    let removed = picked.len() - failed;
    writeln!(out, "Removed {removed} file{}", plural(removed))?;
    if failed > 0 {
        bail!("{failed} file{} could not be removed", plural(failed));
    }
    Ok(())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comments::{LinePos, Side};
    use std::path::Path;

    fn comment(id: u64, line: u32, commit: Option<&str>) -> Comment {
        Comment {
            id,
            file: "src/a.rs".into(),
            side: Side::New,
            line,
            line_text: "x".into(),
            start: None,
            commit: commit.map(String::from),
            head: None,
            body: format!("fix {id}\nplease"),
            created: 0,
            updated: 0,
            closed: None,
        }
    }

    fn run(store: &mut Store, cmd: WorktreeCmd) -> (Result<()>, String) {
        let mut out = Vec::new();
        let res = comments_cmd(cmd, store, &mut out);
        (res, String::from_utf8(out).unwrap())
    }

    fn list(all: bool, json: bool) -> WorktreeCmd {
        WorktreeCmd::List { all, json }
    }

    #[test]
    fn lists_closes_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comments").join("wt.json");
        let (wt, repo) = (Path::new("/wt"), Path::new("/repo/.git"));
        let mut s = Store::load_from(wt, repo, Some(path.clone()));
        assert_eq!(run(&mut s, list(false, false)).1, "No open comments\n");
        let mut range = comment(2, 7, Some(&"a".repeat(40)));
        range.start = Some(LinePos {
            side: Side::New,
            line: 5,
            text: "y".into(),
        });
        s.upsert(comment(1, 3, None)).unwrap();
        s.upsert(range).unwrap();

        let (res, out) = run(
            &mut s,
            WorktreeCmd::Close {
                ids: vec![2],
                by: "claude".into(),
            },
        );
        res.unwrap();
        assert_eq!(out, "Closed 2\n");
        // Another instance sees it closed.
        let mut s = Store::load_from(wt, repo, Some(path.clone()));
        assert_eq!(
            run(&mut s, list(false, false)).1,
            "1  src/a.rs:3\n    fix 1\n    please\n"
        );
        assert_eq!(
            run(&mut s, list(true, false)).1,
            "1  src/a.rs:3\n    fix 1\n    please\n\
             2  src/a.rs:5-7 @ aaaaaaa · closed by claude\n    fix 2\n    please\n"
        );
        let json: serde_json::Value =
            serde_json::from_str(&run(&mut s, list(true, true)).1).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 2);
        assert_eq!(json[1]["id"], "2");
        assert_eq!(json[1]["closed"]["by"], "claude");

        // Unknown ids fail, after the known ones were handled.
        let (res, out) = run(&mut s, WorktreeCmd::Reopen { ids: vec![2, 9] });
        assert!(res.unwrap_err().to_string().contains('9'));
        assert_eq!(out, "Reopened 2\n");
        assert!(s.comments.iter().all(|c| c.closed.is_none()));

        let blank = WorktreeCmd::Close {
            ids: vec![1],
            by: " ".into(),
        };
        assert!(run(&mut s, blank).0.is_err());
    }

    #[test]
    fn refuses_unreadable_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wt.json");
        std::fs::write(&path, "not json").unwrap();
        let mut s = Store::load_from(Path::new("/wt"), Path::new("/r"), Some(path.clone()));
        assert!(run(&mut s, list(false, false)).0.is_err());
        let close = WorktreeCmd::Close {
            ids: vec![1],
            by: "a".into(),
        };
        assert!(run(&mut s, close).0.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
    }

    fn prune_args(yes: bool, deleted_repos: bool, age: Option<u64>) -> PruneArgs {
        PruneArgs {
            yes,
            deleted_repos,
            age,
        }
    }

    fn run_prune(dir: &Path, entries: &[Entry], args: PruneArgs) -> (Result<()>, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let now = 10 * 86_400;
        let res = prune(dir, entries, &args, now, &mut out, &mut err);
        let s = |v: Vec<u8>| String::from_utf8(v).unwrap();
        (res, s(out), s(err))
    }

    #[test]
    fn prunes_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let entry = |name: &str, comments, gone, touched| {
            let path = d.join(format!("{name}.json"));
            std::fs::write(&path, "{}").unwrap();
            Entry {
                path,
                worktree: format!("/wt/{name}").into(),
                comments,
                gone,
                touched,
            }
        };
        let entries = vec![
            entry("removed", 2, Some(Gone::Worktree), 9 * 86_400),
            entry("norepo", 1, Some(Gone::Repo), 9 * 86_400),
            entry("listed", 1, Some(Gone::Listed), 9 * 86_400),
            entry("old", 3, None, 0),
        ];
        let exists = |n: &str| d.join(format!("{n}.json")).exists();

        // Listing only; skipped files are counted.
        let (res, out, _) = run_prune(d, &entries, prune_args(false, false, None));
        res.unwrap();
        assert_eq!(
            out,
            format!(
                "Stale comment files in {}:\n    2 comments  /wt/removed  (worktree removed)\n\
                 1 file of missing or unknown repositories skipped; add --deleted-repos to include them\n\
                 1 file of missing worktrees their repository still lists skipped; if they were deleted, run `git worktree prune` there first\n\
                 Run with --yes to delete them\n",
                d.display()
            )
        );
        assert!(exists("removed"));

        // Opt-ins pick up more.
        let (_, out, _) = run_prune(d, &entries, prune_args(false, true, Some(7)));
        assert!(
            out.contains("1 comment   /wt/norepo  (repository missing)"),
            "{out}"
        );
        assert!(out.contains("/wt/old  (untouched for 10 days)"), "{out}");
        assert!(!out.contains("/wt/listed  ("), "{out}");

        // Deleting goes on after a failure and then fails.
        std::fs::remove_file(d.join("norepo.json")).unwrap();
        let (res, out, err) = run_prune(d, &entries, prune_args(true, true, None));
        assert!(
            res.unwrap_err()
                .to_string()
                .contains("1 file could not be removed")
        );
        assert!(err.starts_with(&format!("removing {}", d.join("norepo.json").display())));
        assert!(out.ends_with("Removed 1 file\n"), "{out}");
        assert!(!exists("removed") && exists("listed") && exists("old"));

        let (res, out, _) = run_prune(d, &[], prune_args(true, false, None));
        res.unwrap();
        assert_eq!(out, format!("No stale comment files in {}\n", d.display()));
    }
}
