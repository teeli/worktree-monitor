//! `wtm comments …`: reading, replying to and closing the current worktree's comments without
//! the TUI, e.g. by an AI agent that handled them, and cleaning up comment files of removed
//! worktrees.

use crate::comments::{self, Closed, Comment, Entry, Gone, Id, Reply, Store};
use crate::git;
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use std::io::{Read, Write};
use std::path::Path;

#[derive(Subcommand)]
pub enum Command {
    /// List, reply to, close or reopen the comments of the current worktree; `prune` cleans up stale
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
    /// List open comments: id, file:line, commit (if any), then the indented body and replies.
    List {
        /// Include closed comments.
        #[arg(long)]
        all: bool,
        /// Print the comments as a JSON array in the format of schema/comments.schema.json,
        /// except that ids (of comments and replies) are strings: they exceed 2^53, which many
        /// JSON parsers round.
        #[arg(long)]
        json: bool,
    },
    /// Reply to a comment, e.g. with how it was handled or a question. Doesn't open or close it.
    Reply {
        /// Id of the comment, as `list` prints it.
        id: Id,
        /// Who replies, e.g. the agent's name.
        #[arg(long, default_value = "agent")]
        by: String,
        /// The reply; words are joined with spaces. `-` reads it from stdin.
        #[arg(required = true, value_name = "TEXT")]
        text: Vec<String>,
    },
    /// Close comments that were fixed or dropped.
    Close {
        #[arg(required = true, value_name = "ID")]
        ids: Vec<Id>,
        /// Who closed them, e.g. the agent's name.
        #[arg(long, default_value = "agent")]
        by: String,
    },
    /// Reopen closed comments.
    Reopen {
        #[arg(required = true, value_name = "ID")]
        ids: Vec<Id>,
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
            let out = &mut std::io::stdout().lock();
            comments_cmd(cmd, &mut store, &mut std::io::stdin().lock(), out)
        }
        CommentsCmd::Prune(args) => {
            let (dir, entries) = comments::scan()?;
            let now = comments::now();
            let out = &mut std::io::stdout().lock();
            prune(&dir, &entries, &args, now, out, &mut std::io::stderr())
        }
    }
}

fn comments_cmd(
    cmd: WorktreeCmd,
    store: &mut Store,
    stdin: &mut impl Read,
    out: &mut impl Write,
) -> Result<()> {
    if let Some(e) = &store.error {
        bail!("comments unreadable: {e}");
    }
    let (ids, closed) = match cmd {
        WorktreeCmd::List { all, json } => return list(store, all, json, out),
        WorktreeCmd::Reply { id, by, text } => return reply(store, id, &by, &text, stdin, out),
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

fn reply(
    store: &mut Store,
    id: Id,
    by: &str,
    text: &[String],
    stdin: &mut impl Read,
    out: &mut impl Write,
) -> Result<()> {
    if by.trim().is_empty() {
        bail!("--by must not be empty");
    }
    let body = if text == ["-"] {
        let mut buf = String::new();
        stdin.read_to_string(&mut buf).context("reading stdin")?;
        buf
    } else {
        text.join(" ")
    };
    let body = body.trim();
    if body.is_empty() {
        bail!("the reply must not be empty");
    }
    if store.add_reply(id, Reply::new(by, body))?.is_none() {
        bail!("no comment with id {id}");
    }
    writeln!(out, "Replied to {id}")?;
    Ok(())
}

fn list(store: &Store, all: bool, json: bool, out: &mut impl Write) -> Result<()> {
    let shown: Vec<&Comment> = comments::order(&store.comments, all, |_| None)
        .into_iter()
        .map(|i| &store.comments[i])
        .collect();
    if json {
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
        for r in &c.replies {
            writeln!(out, "    ↳ {}", r.by)?;
            for line in r.body.lines() {
                writeln!(out, "      {line}")?;
            }
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
    use crate::comments::{LinePos, Side, test_id};
    use std::path::Path;

    fn comment(id: u64, line: u32, commit: Option<&str>) -> Comment {
        Comment {
            id: test_id(id.into()),
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
            replies: Vec::new(),
        }
    }

    fn run(store: &mut Store, cmd: WorktreeCmd) -> (Result<()>, String) {
        run_with(store, cmd, "")
    }

    fn run_with(store: &mut Store, cmd: WorktreeCmd, stdin: &str) -> (Result<()>, String) {
        let mut out = Vec::new();
        let res = comments_cmd(cmd, store, &mut stdin.as_bytes(), &mut out);
        (res, String::from_utf8(out).unwrap())
    }

    fn list(all: bool, json: bool) -> WorktreeCmd {
        WorktreeCmd::List { all, json }
    }

    #[test]
    fn lists_closes_and_reopens() {
        let (id1, id2) = (test_id(1), test_id(2));
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
                ids: vec![id2],
                by: "claude".into(),
            },
        );
        res.unwrap();
        assert_eq!(out, format!("Closed {id2}\n"));
        // Another instance sees it closed.
        let mut s = Store::load_from(wt, repo, Some(path.clone()));
        assert_eq!(
            run(&mut s, list(false, false)).1,
            format!("{id1}  src/a.rs:3\n    fix 1\n    please\n")
        );
        assert_eq!(
            run(&mut s, list(true, false)).1,
            format!(
                "{id1}  src/a.rs:3\n    fix 1\n    please\n\
                 {id2}  src/a.rs:5-7 @ aaaaaaa · closed by claude\n    fix 2\n    please\n"
            )
        );
        let json: serde_json::Value =
            serde_json::from_str(&run(&mut s, list(true, true)).1).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 2);
        assert_eq!(json[1]["id"], id2.to_string());
        assert_eq!(json[1]["closed"]["by"], "claude");

        // Unknown ids fail, after the known ones were handled.
        let ids = vec![id2, test_id(9)];
        let (res, out) = run(&mut s, WorktreeCmd::Reopen { ids });
        assert!(
            res.unwrap_err()
                .to_string()
                .contains(&test_id(9).to_string())
        );
        assert_eq!(out, format!("Reopened {id2}\n"));
        assert!(s.comments.iter().all(|c| c.closed.is_none()));

        let blank = WorktreeCmd::Close {
            ids: vec![id1],
            by: " ".into(),
        };
        assert!(run(&mut s, blank).0.is_err());
    }

    fn reply_cmd(id: u128, by: &str, text: &[&str]) -> WorktreeCmd {
        WorktreeCmd::Reply {
            id: test_id(id),
            by: by.into(),
            text: text.iter().map(|t| t.to_string()).collect(),
        }
    }

    #[test]
    fn replies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comments").join("wt.json");
        let (wt, repo) = (Path::new("/wt"), Path::new("/repo/.git"));
        let mut s = Store::load_from(wt, repo, Some(path.clone()));
        s.upsert(comment(1, 3, None)).unwrap();
        s.set_closed(test_id(1), Some(Closed::now("user"))).unwrap();

        let (res, out) = run(&mut s, reply_cmd(1, "claude", &["Fixed", "in", "abc"]));
        res.unwrap();
        assert_eq!(out, format!("Replied to {}\n", test_id(1)));
        let (res, _) = run_with(
            &mut s,
            reply_cmd(1, "claude", &["-"]),
            "  why?\nreally?\n\n",
        );
        res.unwrap();
        // Replies don't reopen.
        let mut s = Store::load_from(wt, repo, Some(path.clone()));
        let c = &s.comments[0];
        assert!(c.closed.is_some());
        let bodies: Vec<&str> = c.replies.iter().map(|r| r.body.as_str()).collect();
        assert_eq!(bodies, ["Fixed in abc", "why?\nreally?"]);
        assert!(c.replies.iter().all(|r| r.by == "claude"));

        assert_eq!(
            run(&mut s, list(true, false)).1,
            format!(
                "{}  src/a.rs:3 · closed by user\n    fix 1\n    please\n\
                 \x20   ↳ claude\n      Fixed in abc\n\
                 \x20   ↳ claude\n      why?\n      really?\n",
                test_id(1)
            )
        );
        let json: serde_json::Value =
            serde_json::from_str(&run(&mut s, list(true, true)).1).unwrap();
        let r = &json[0]["replies"][1];
        assert_eq!(r["id"], s.comments[0].replies[1].id.to_string());
        assert_eq!(r["by"], "claude");

        let err = |s: &mut Store, cmd, stdin| run_with(s, cmd, stdin).0.unwrap_err().to_string();
        let unknown = test_id(9).to_string();
        assert!(err(&mut s, reply_cmd(9, "a", &["x"]), "").contains(&unknown));
        assert!(err(&mut s, reply_cmd(1, " ", &["x"]), "").contains("--by"));
        assert!(err(&mut s, reply_cmd(1, "a", &[" "]), "").contains("empty"));
        assert!(err(&mut s, reply_cmd(1, "a", &["-"]), "\n").contains("empty"));
        assert_eq!(s.comments[0].replies.len(), 2);
    }

    #[test]
    fn parses_uuid_ids() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            cmd: WorktreeCmd,
        }
        let parse = |args: &[&str]| Cli::try_parse_from(["wtm"].iter().chain(args));
        let id = test_id(1).to_string();
        let Ok(Cli {
            cmd: WorktreeCmd::Close { ids, .. },
        }) = parse(&["close", &id])
        else {
            panic!("close <uuid> must parse");
        };
        assert_eq!(ids, [test_id(1)]);
        assert!(parse(&["close", "1"]).is_err());
        assert!(parse(&["reply", "not-a-uuid", "x"]).is_err());
    }

    #[test]
    fn refuses_unreadable_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wt.json");
        std::fs::write(&path, "not json").unwrap();
        let mut s = Store::load_from(Path::new("/wt"), Path::new("/r"), Some(path.clone()));
        assert!(run(&mut s, list(false, false)).0.is_err());
        let close = WorktreeCmd::Close {
            ids: vec![test_id(1)],
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
