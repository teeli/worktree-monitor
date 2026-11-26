//! Background thread running git commands so the UI never blocks.
//! Queued jobs are coalesced: repeated refreshes of the same thing collapse into one.

use crate::app::AppEvent;
use crate::comments::{self, Comment};
use crate::diff;
use crate::git::{self, Base, Commit, Conflict, FileChange, Worktree};
use anyhow::Result;
use std::path::PathBuf;
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
    /// Finds the commits working tree comments on now unchanged files were committed in.
    ResolveComments {
        wt: PathBuf,
        /// HEAD the search runs up to; echoed back to discard stale results.
        head: String,
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

/// Where a committed working tree comment's lines went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Commit(String),
    /// In no commit since the comment was written: the changes were discarded.
    Nowhere,
    /// The history could not be read (e.g. the comment's HEAD no longer exists).
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
        Job::ResolveComments { wt, head, comments } => {
            let res = resolve_comments(&wt, &comments);
            GitResult::ResolveComments { wt, head, res }
        }
    }
}

fn resolve_comments(wt: &std::path::Path, comments: &[Comment]) -> Vec<(comments::Id, Resolved)> {
    // One history per (HEAD when written, file); `None` when it can't be read.
    let mut histories = std::collections::HashMap::new();
    comments
        .iter()
        .map(|c| {
            let Some(head) = &c.head else {
                return (c.id, Resolved::Unknown);
            };
            let history = histories
                .entry((head.clone(), c.file.clone()))
                .or_insert_with(|| {
                    let h = git::file_history(wt, head, &c.file).ok()?;
                    Some(
                        h.into_iter()
                            .map(|(sha, raw)| (sha, diff::parse(&raw)))
                            .collect::<Vec<_>>(),
                    )
                });
            let res = match history {
                None => Resolved::Unknown,
                Some(h) => match comments::resolve_commit(c, h) {
                    Some(sha) => Resolved::Commit(sha.to_string()),
                    None => Resolved::Nowhere,
                },
            };
            (c.id, res)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
