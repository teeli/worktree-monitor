//! Background thread running git commands so the UI never blocks.
//! Queued jobs are coalesced: repeated refreshes of the same thing collapse into one.

use crate::app::AppEvent;
use crate::comments::{self, Comment};
use crate::diff::{self, Diff};
use crate::git::{self, Base, Commit, Conflict, FileChange, Worktree};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

#[derive(Debug, Clone)]
pub enum Job {
    Worktrees,
    Status {
        wt: PathBuf,
    },
    Log {
        wt: PathBuf,
        skip: usize,
        count: usize,
        base: Option<String>,
    },
    Conflicts {
        wt: PathBuf,
        base: String,
        /// (HEAD, base) commits this check is for; echoed back to discard stale results.
        key: ConflictKey,
    },
    CommitFiles {
        wt: PathBuf,
        id: u64,
        commit: Commit,
    },
    Diff {
        wt: PathBuf,
        id: u64,
        commit: Option<Commit>,
        file: FileChange,
    },
    /// Finds the commits working tree comments on now unchanged files were committed in, and
    /// the rewritten commits (rebase, squash, fixup) of comments on commits no longer in history.
    ResolveComments {
        wt: PathBuf,
        /// HEAD the search runs up to; echoed back to discard stale results.
        head: String,
        /// Base branch whose commits rewritten comments never move to.
        base: Option<String>,
        comments: Vec<Comment>,
    },
}

pub type ConflictKey = (String, String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum JobKey {
    Worktrees,
    Status(PathBuf),
    Log(PathBuf, usize),
    Conflicts(PathBuf),
    CommitFiles,
    Diff,
    ResolveComments(PathBuf),
}

impl Job {
    fn key(&self) -> JobKey {
        match self {
            Job::Worktrees => JobKey::Worktrees,
            Job::Status { wt } => JobKey::Status(wt.clone()),
            Job::Log { wt, skip, .. } => JobKey::Log(wt.clone(), *skip),
            Job::Conflicts { wt, .. } => JobKey::Conflicts(wt.clone()),
            // Only the latest file list / diff request matters.
            Job::CommitFiles { .. } => JobKey::CommitFiles,
            Job::Diff { .. } => JobKey::Diff,
            Job::ResolveComments { wt, .. } => JobKey::ResolveComments(wt.clone()),
        }
    }
}

pub enum GitResult {
    Worktrees {
        res: Result<Vec<Worktree>>,
        base: Option<Base>,
    },
    Status {
        wt: PathBuf,
        res: Result<Vec<FileChange>>,
    },
    Log {
        wt: PathBuf,
        skip: usize,
        count: usize,
        res: Result<Vec<Commit>>,
    },
    Conflicts {
        wt: PathBuf,
        key: ConflictKey,
        res: Result<Option<Conflict>>,
    },
    CommitFiles {
        wt: PathBuf,
        id: u64,
        res: Result<Vec<FileChange>>,
    },
    Diff {
        wt: PathBuf,
        id: u64,
        res: Result<String>,
    },
    ResolveComments {
        wt: PathBuf,
        head: String,
        res: Vec<(comments::Id, Resolved)>,
    },
}

/// Where a committed working tree comment's lines went, or which commit a comment's rewritten
/// commit became.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Commit(String),
    /// In no commit since: the changes were discarded, or a rewrite changed the lines.
    Nowhere,
    /// Nothing to do (the comment's commit is still in history), or the history could not be
    /// read (e.g. the comment's HEAD or commit no longer exists).
    Unknown,
}

pub fn spawn(repo_dir: PathBuf, events: Sender<AppEvent>) -> Sender<Job> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || run(repo_dir, rx, events));
    tx
}

fn run(repo_dir: PathBuf, rx: Receiver<Job>, events: Sender<AppEvent>) {
    while let Ok(first) = rx.recv() {
        let mut jobs = vec![first];
        jobs.extend(rx.try_iter());
        for job in coalesce(jobs) {
            let res = execute(&repo_dir, job);
            if events.send(AppEvent::Git(res)).is_err() {
                return;
            }
        }
    }
}

/// Keeps the last job per key, preserving the order of those last occurrences.
fn coalesce(jobs: Vec<Job>) -> Vec<Job> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Job> = jobs
        .into_iter()
        .rev()
        .filter(|j| seen.insert(j.key()))
        .collect();
    out.reverse();
    out
}

fn execute(repo_dir: &std::path::Path, job: Job) -> GitResult {
    match job {
        Job::Worktrees => GitResult::Worktrees {
            res: git::list_worktrees(repo_dir),
            base: git::resolve_base(repo_dir),
        },
        Job::Status { wt } => {
            let res = git::status(&wt);
            GitResult::Status { wt, res }
        }
        Job::Log {
            wt,
            skip,
            count,
            base,
        } => {
            let res = git::log(&wt, skip, count, base.as_deref());
            GitResult::Log {
                wt,
                skip,
                count,
                res,
            }
        }
        Job::Conflicts { wt, base, key } => {
            // Merge exactly the commits in the memo key, so key and result always agree.
            let (head, base_sha) = &key;
            let res = git::conflicts_with_base(&wt, &base, base_sha, head);
            GitResult::Conflicts { wt, key, res }
        }
        Job::CommitFiles { wt, id, commit } => {
            let res = git::commit_files(&wt, &commit);
            GitResult::CommitFiles { wt, id, res }
        }
        Job::Diff {
            wt,
            id,
            commit,
            file,
        } => {
            let res = match &commit {
                Some(c) => git::commit_diff(&wt, c, &file),
                None => git::working_diff(&wt, &file),
            };
            GitResult::Diff { wt, id, res }
        }
        Job::ResolveComments {
            wt,
            head,
            base,
            comments,
        } => {
            let res = resolve_comments(&wt, base.as_deref(), &comments);
            GitResult::ResolveComments { wt, head, res }
        }
    }
}

/// Parsed `git::file_history`, per (since, exclude, file); `None` when it can't be read.
type Histories = HashMap<(String, Option<String>, String), Option<Vec<(String, Diff)>>>;

fn resolve_comments(
    wt: &Path,
    base: Option<&str>,
    comments: &[Comment],
) -> Vec<(comments::Id, Resolved)> {
    let mut histories = Histories::new();
    comments
        .iter()
        .map(|c| {
            let res = match &c.commit {
                None => resolve_committed(wt, c, &mut histories),
                Some(old) => resolve_rewritten(wt, base, c, old, &mut histories),
            };
            (c.id, res)
        })
        .collect()
}

/// The commit a working tree comment's lines were committed in since it was written.
fn resolve_committed(wt: &Path, c: &Comment, histories: &mut Histories) -> Resolved {
    let Some(head) = &c.head else {
        return Resolved::Unknown;
    };
    match history(histories, wt, head, None, &c.file) {
        None => Resolved::Unknown,
        Some(h) => match comments::resolve_commit(c, h) {
            Some(sha) => Resolved::Commit(sha.to_string()),
            None => Resolved::Nowhere,
        },
    }
}

/// The commit that `old`, no longer in history, became when the branch was rewritten.
fn resolve_rewritten(
    wt: &Path,
    base: Option<&str>,
    c: &Comment,
    old: &str,
    histories: &mut Histories,
) -> Resolved {
    // Still in history, gone, or unreadable: nothing to do.
    if !matches!(git::in_history(wt, old), Ok(Some(false))) {
        return Resolved::Unknown;
    }
    let (Ok(Some(since)), Ok(patch)) =
        (git::merge_base(wt, old), git::file_patch(wt, old, &c.file))
    else {
        return Resolved::Unknown;
    };
    let old_diff = diff::parse(&patch);
    match history(histories, wt, &since, base, &c.file) {
        None => Resolved::Unknown,
        Some(h) => match comments::resolve_rewritten(c, &old_diff, h) {
            Some(sha) => Resolved::Commit(sha.to_string()),
            None => Resolved::Nowhere,
        },
    }
}

fn history<'a>(
    histories: &'a mut Histories,
    wt: &Path,
    since: &str,
    exclude: Option<&str>,
    file: &str,
) -> Option<&'a [(String, Diff)]> {
    histories
        .entry((since.into(), exclude.map(String::from), file.into()))
        .or_insert_with(|| {
            let h = git::file_history(wt, since, exclude, file).ok()?;
            Some(
                h.into_iter()
                    .map(|(sha, raw)| (sha, diff::parse(&raw)))
                    .collect(),
            )
        })
        .as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn on_commit(file: &str, line: u32, text: &str, sha: &str) -> Comment {
        Comment {
            id: comments::Id::now_v7(),
            file: file.into(),
            side: comments::Side::New,
            line,
            line_text: text.into(),
            start: None,
            commit: Some(sha.into()),
            head: Some(sha.into()),
            body: "b".into(),
            created: 0,
            updated: 0,
            closed: None,
            replies: Vec::new(),
        }
    }

    #[test]
    fn follows_rewritten_commits() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path();
        let write = |f: &str, s: &str| std::fs::write(wt.join(f), s).unwrap();
        let commit = |msg: &str| {
            sh(wt, &["commit", "-qam", msg]);
            sh(wt, &["rev-parse", "HEAD"])
        };
        sh(wt, &["init", "-q", "-b", "main"]);
        write("a", "1\n2\n3\n");
        write("b", "x\n");
        sh(wt, &["add", "."]);
        commit("base");
        sh(wt, &["checkout", "-qb", "feat"]);
        write("a", "1\nTWO\n3\n");
        let one = commit("one");
        write("a", "1\nTWO\n3\nfour\n");
        let two = commit("two");
        // Rebase onto a main that also touches the file and adds the commented text.
        sh(wt, &["checkout", "-q", "main"]);
        write("a", "0\n1\n2\n3\n");
        write("b", "x\nTWO\n");
        commit("main");
        sh(wt, &["checkout", "-q", "feat"]);
        let changed = on_commit("a", 2, "TWO", &one);
        let context = on_commit("a", 3, "3", &two); // unchanged line: found by its patch
        let kept = on_commit("a", 2, "TWO", &two);
        let res = |cs: &[Comment]| -> Vec<Resolved> {
            resolve_comments(wt, Some("main"), cs)
                .into_iter()
                .map(|(_, r)| r)
                .collect()
        };
        // Still in history.
        assert_eq!(res(std::slice::from_ref(&kept)), vec![Resolved::Unknown]);

        sh(wt, &["rebase", "-q", "main"]);
        let one2 = sh(wt, &["rev-parse", "HEAD~"]);
        let two2 = sh(wt, &["rev-parse", "HEAD"]);
        assert_eq!(
            res(&[changed, context]),
            vec![Resolved::Commit(one2), Resolved::Commit(two2.clone())]
        );

        // Squash both into one commit.
        sh(wt, &["reset", "-q", "--soft", "HEAD~2"]);
        let squashed = commit("squashed");
        let changed2 = on_commit("a", 3, "TWO", &two2);
        assert_eq!(res(&[changed2]), vec![Resolved::Commit(squashed.clone())]);

        // A fixup rewrites the commented line.
        write("a", "0\n1\nTWO!\n3\nfour\n");
        sh(wt, &["commit", "-q", "--amend", "-a", "--no-edit"]);
        let on_squashed = on_commit("a", 3, "TWO", &squashed);
        assert_eq!(res(&[on_squashed]), vec![Resolved::Nowhere]);

        // A commit that no longer exists can't be followed.
        let gone = on_commit("a", 3, "TWO", &"0".repeat(40));
        assert_eq!(res(&[gone]), vec![Resolved::Unknown]);
    }

    #[test]
    fn coalesces_jobs() {
        let a = PathBuf::from("/a");
        let b = PathBuf::from("/b");
        let jobs = vec![
            Job::Status { wt: a.clone() },
            Job::Status { wt: b.clone() },
            Job::Worktrees,
            Job::Status { wt: a.clone() },
            Job::Conflicts {
                wt: a.clone(),
                base: "main".into(),
                key: Default::default(),
            },
            Job::Conflicts {
                wt: a.clone(),
                base: "main".into(),
                key: Default::default(),
            },
        ];
        let out = coalesce(jobs);
        assert_eq!(out.len(), 4);
        assert!(matches!(&out[0], Job::Status { wt } if *wt == b));
        assert!(matches!(&out[1], Job::Worktrees));
        assert!(matches!(&out[2], Job::Status { wt } if *wt == a));
        assert!(matches!(&out[3], Job::Conflicts { wt, .. } if *wt == a));
    }
}
