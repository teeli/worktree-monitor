//! `wtm comments …`: reading and closing the current worktree's comments without the TUI,
//! e.g. by an AI agent that handled them.

use crate::comments::{self, Closed, Comment, Store};
use crate::git;
use anyhow::{Result, bail};
use clap::Subcommand;
use std::io::Write;

#[derive(Subcommand)]
pub enum Command {
    /// List, close or reopen the comments of the current worktree.
    #[command(subcommand)]
    Comments(CommentsCmd),
}

#[derive(Subcommand)]
pub enum CommentsCmd {
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

pub fn run(cmd: Command) -> Result<()> {
    let Command::Comments(cmd) = cmd;
    let repo = git::discover(&std::env::current_dir()?)?;
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let mut store = Store::load(&canon(&repo.toplevel), &canon(&repo.common_dir));
    comments_cmd(cmd, &mut store, &mut std::io::stdout().lock())
}

fn comments_cmd(cmd: CommentsCmd, store: &mut Store, out: &mut impl Write) -> Result<()> {
    if let Some(e) = &store.error {
        bail!("comments unreadable: {e}");
    }
    let (ids, closed) = match cmd {
        CommentsCmd::List { all, json } => return list(store, all, json, out),
        CommentsCmd::Close { ids, by } => {
            if by.trim().is_empty() {
                bail!("--by must not be empty");
            }
            (ids, Some(by))
        }
        CommentsCmd::Reopen { ids } => (ids, None),
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

    fn run(store: &mut Store, cmd: CommentsCmd) -> (Result<()>, String) {
        let mut out = Vec::new();
        let res = comments_cmd(cmd, store, &mut out);
        (res, String::from_utf8(out).unwrap())
    }

    fn list(all: bool, json: bool) -> CommentsCmd {
        CommentsCmd::List { all, json }
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
            CommentsCmd::Close {
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
        let (res, out) = run(&mut s, CommentsCmd::Reopen { ids: vec![2, 9] });
        assert!(res.unwrap_err().to_string().contains('9'));
        assert_eq!(out, "Reopened 2\n");
        assert!(s.comments.iter().all(|c| c.closed.is_none()));

        let blank = CommentsCmd::Close {
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
        let close = CommentsCmd::Close {
            ids: vec![1],
            by: "a".into(),
        };
        assert!(run(&mut s, close).0.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
    }
}
