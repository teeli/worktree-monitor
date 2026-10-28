//! Background thread running git commands so the UI never blocks.
//! Queued jobs are coalesced: repeated refreshes of the same thing collapse into one.

use crate::app::AppEvent;
use crate::git::{self, Commit, FileChange, Worktree};
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
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum JobKey {
    Worktrees,
    Status(PathBuf),
    Log(PathBuf, usize),
    CommitFiles,
    Diff,
}

impl Job {
    fn key(&self) -> JobKey {
        match self {
            Job::Worktrees => JobKey::Worktrees,
            Job::Status { wt } => JobKey::Status(wt.clone()),
            Job::Log { wt, skip, .. } => JobKey::Log(wt.clone(), *skip),
            // Only the latest file list / diff request matters.
            Job::CommitFiles { .. } => JobKey::CommitFiles,
            Job::Diff { .. } => JobKey::Diff,
        }
    }
}

pub enum GitResult {
    Worktrees(Result<Vec<Worktree>>),
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
        Job::Worktrees => GitResult::Worktrees(git::list_worktrees(repo_dir)),
        Job::Status { wt } => {
            let res = git::status(&wt);
            GitResult::Status { wt, res }
        }
        Job::Log { wt, skip, count } => {
            let res = git::log(&wt, skip, count);
            GitResult::Log {
                wt,
                skip,
                count,
                res,
            }
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
    }
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
        ];
        let out = coalesce(jobs);
        assert_eq!(out.len(), 3);
        assert!(matches!(&out[0], Job::Status { wt } if *wt == b));
        assert!(matches!(&out[1], Job::Worktrees));
        assert!(matches!(&out[2], Job::Status { wt } if *wt == a));
    }
}
