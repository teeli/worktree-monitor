//! Thin wrappers around the `git` CLI plus pure parsers for its machine-readable output.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Hash of the empty tree; used as diff base in repositories without commits.
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// Raw diff output beyond this many bytes is truncated.
const MAX_DIFF_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Repo {
    pub toplevel: PathBuf,
    pub common_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: Option<String>,
    /// Short branch name (without `refs/heads/`).
    pub branch: Option<String>,
    pub bare: bool,
    pub prunable: bool,
}

impl Worktree {
    pub fn label(&self) -> String {
        match (&self.branch, &self.head) {
            (Some(b), _) => b.clone(),
            (None, Some(h)) => format!("({})", &h[..h.len().min(7)]),
            (None, None) => self
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }

    pub fn exists(&self) -> bool {
        self.path.is_dir()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    Conflicted,
}

impl FileStatus {
    pub fn letter(self) -> char {
        match self {
            FileStatus::Modified => 'M',
            FileStatus::Added => 'A',
            FileStatus::Deleted => 'D',
            FileStatus::Renamed => 'R',
            FileStatus::Copied => 'C',
            FileStatus::Untracked => '?',
            FileStatus::Conflicted => 'U',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub short: String,
    pub author: String,
    pub date: String,
    pub refs: String,
    pub parents: Vec<String>,
    pub subject: String,
}

impl Commit {
    /// Diff base for this commit: its first parent, or the empty tree for root commits.
    fn base(&self) -> &str {
        self.parents
            .first()
            .map(String::as_str)
            .unwrap_or(EMPTY_TREE)
    }
}

fn command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "core.quotepath=off", "-c", "color.ui=false"])
        // Never take index.lock: our own refreshes would otherwise trigger fs events in a loop.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C");
    cmd
}

/// Runs git and returns stdout. Exit codes listed in `ok_codes` count as success.
fn run(dir: &Path, args: &[&str], ok_codes: &[i32]) -> Result<Vec<u8>> {
    let out = command(dir)
        .args(args)
        .output()
        .context("failed to run git (is it installed?)")?;
    let code = out.status.code().unwrap_or(-1);
    if !ok_codes.contains(&code) {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("git {}: {}", args.first().unwrap_or(&""), err.trim());
    }
    Ok(out.stdout)
}

fn run_ok(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    run(dir, args, &[0])
}

pub fn discover(cwd: &Path) -> Result<Repo> {
    let out = run_ok(
        cwd,
        &[
            "rev-parse",
            "--show-toplevel",
            "--path-format=absolute",
            "--git-common-dir",
        ],
    )
    .context("not inside a git repository")?;
    let s = String::from_utf8_lossy(&out);
    let mut lines = s.lines();
    let toplevel = lines.next().context("unexpected rev-parse output")?;
    let common = lines.next().context("unexpected rev-parse output")?;
    if toplevel.is_empty() {
        bail!("not inside a git work tree");
    }
    Ok(Repo {
        toplevel: PathBuf::from(toplevel),
        common_dir: PathBuf::from(common),
    })
}

pub fn list_worktrees(repo_dir: &Path) -> Result<Vec<Worktree>> {
    let out = run_ok(repo_dir, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktrees(&String::from_utf8_lossy(&out)))
}

pub fn parse_worktrees(s: &str) -> Vec<Worktree> {
    let mut result = Vec::new();
    let mut cur: Option<Worktree> = None;
    for line in s.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if let Some(wt) = cur.take()
                && !wt.bare
            {
                result.push(wt);
            }
            continue;
        }
        let (key, val) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "worktree" => {
                cur = Some(Worktree {
                    path: PathBuf::from(val),
                    head: None,
                    branch: None,
                    bare: false,
                    prunable: false,
                })
            }
            _ => {
                let Some(wt) = cur.as_mut() else { continue };
                match key {
                    "HEAD" => wt.head = Some(val.to_string()),
                    "branch" => {
                        wt.branch = Some(val.strip_prefix("refs/heads/").unwrap_or(val).to_string())
                    }
                    "bare" => wt.bare = true,
                    "prunable" => wt.prunable = true,
                    _ => {}
                }
            }
        }
    }
    result
}

pub fn status(wt: &Path) -> Result<Vec<FileChange>> {
    let out = run_ok(
        wt,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ],
    )?;
    Ok(parse_status(&out))
}

pub fn parse_status(out: &[u8]) -> Vec<FileChange> {
    let mut fields = out.split(|&b| b == 0).filter(|f| !f.is_empty());
    let mut result = Vec::new();
    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue;
        }
        let (x, y) = (entry[0] as char, entry[1] as char);
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        let renamed = matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C');
        let old_path = if renamed {
            fields
                .next()
                .map(|p| String::from_utf8_lossy(p).into_owned())
        } else {
            None
        };
        let status = match (x, y) {
            ('!', '!') => continue,
            ('?', '?') => FileStatus::Untracked,
            ('D', 'D') | ('A', 'A') | ('U', _) | (_, 'U') => FileStatus::Conflicted,
            _ if x == 'D' || y == 'D' => FileStatus::Deleted,
            _ if x == 'R' || y == 'R' => FileStatus::Renamed,
            _ if x == 'C' || y == 'C' => FileStatus::Copied,
            _ if x == 'A' => FileStatus::Added,
            _ => FileStatus::Modified,
        };
        result.push(FileChange {
            path,
            old_path,
            status,
        });
    }
    result
}

pub fn has_head(wt: &Path) -> bool {
    run_ok(wt, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok()
}

pub fn log(wt: &Path, skip: usize, count: usize) -> Result<Vec<Commit>> {
    if !has_head(wt) {
        return Ok(Vec::new());
    }
    let n = format!("-n{count}");
    let skip = format!("--skip={skip}");
    let out = run_ok(
        wt,
        &[
            "log",
            &n,
            &skip,
            "--format=%H%x1f%h%x1f%an%x1f%ar%x1f%D%x1f%P%x1f%s%x1e",
        ],
    )?;
    Ok(parse_log(&String::from_utf8_lossy(&out)))
}

pub fn parse_log(s: &str) -> Vec<Commit> {
    s.split('\x1e')
        .map(|r| r.trim_start_matches('\n'))
        .filter(|r| !r.is_empty())
        .filter_map(|r| {
            let f: Vec<&str> = r.split('\x1f').collect();
            if f.len() < 7 {
                return None;
            }
            Some(Commit {
                sha: f[0].to_string(),
                short: f[1].to_string(),
                author: f[2].to_string(),
                date: f[3].to_string(),
                refs: f[4].to_string(),
                parents: f[5].split_whitespace().map(str::to_string).collect(),
                subject: f[6].to_string(),
            })
        })
        .collect()
}

pub fn commit_files(wt: &Path, commit: &Commit) -> Result<Vec<FileChange>> {
    let out = run_ok(
        wt,
        &[
            "diff",
            "--no-ext-diff",
            "-M",
            "--name-status",
            "-z",
            commit.base(),
            &commit.sha,
        ],
    )?;
    Ok(parse_name_status(&out))
}

pub fn parse_name_status(out: &[u8]) -> Vec<FileChange> {
    let mut fields = out
        .split(|&b| b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned());
    let mut result = Vec::new();
    while let Some(code) = fields.next() {
        let Some(first) = fields.next() else { break };
        let status = match code.chars().next() {
            Some('A') => FileStatus::Added,
            Some('D') => FileStatus::Deleted,
            Some('R') => FileStatus::Renamed,
            Some('C') => FileStatus::Copied,
            Some('U') => FileStatus::Conflicted,
            _ => FileStatus::Modified,
        };
        if matches!(status, FileStatus::Renamed | FileStatus::Copied) {
            let Some(new) = fields.next() else { break };
            result.push(FileChange {
                path: new,
                old_path: Some(first),
                status,
            });
        } else {
            result.push(FileChange {
                path: first,
                old_path: None,
                status,
            });
        }
    }
    result
}

fn pathspec(file: &FileChange) -> Vec<&str> {
    let mut v = vec!["--", file.path.as_str()];
    if let Some(old) = &file.old_path {
        v.push(old);
    }
    v
}

fn cap(mut out: Vec<u8>) -> String {
    let truncated = out.len() > MAX_DIFF_BYTES;
    out.truncate(MAX_DIFF_BYTES);
    let mut s = String::from_utf8_lossy(&out).into_owned();
    if truncated {
        s.push_str("\n\\ [diff truncated]\n");
    }
    s
}

/// Diff of uncommitted changes (staged + unstaged) for one file against HEAD.
pub fn working_diff(wt: &Path, file: &FileChange) -> Result<String> {
    if file.status == FileStatus::Untracked {
        let out = run(
            wt,
            &[
                "diff",
                "--no-ext-diff",
                "--no-index",
                "--",
                "/dev/null",
                &file.path,
            ],
            &[0, 1],
        )?;
        return Ok(cap(out));
    }
    let base = if has_head(wt) { "HEAD" } else { EMPTY_TREE };
    let mut args = vec!["diff", "--no-ext-diff", "-M", base];
    args.extend(pathspec(file));
    Ok(cap(run_ok(wt, &args)?))
}

pub fn commit_diff(wt: &Path, commit: &Commit, file: &FileChange) -> Result<String> {
    let mut args = vec!["diff", "--no-ext-diff", "-M", commit.base(), &commit.sha];
    args.extend(pathspec(file));
    Ok(cap(run_ok(wt, &args)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_worktree_porcelain() {
        let s = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n\
                 worktree /repo/wt\nHEAD def1234567\ndetached\n\n\
                 worktree /bare\nbare\n\n\
                 worktree /gone\nHEAD 123\nbranch refs/heads/feat/x\nprunable gitdir file points to non-existent location\n";
        let wts = parse_worktrees(s);
        assert_eq!(wts.len(), 3);
        assert_eq!(wts[0].label(), "main");
        assert_eq!(wts[1].label(), "(def1234)");
        assert_eq!(wts[2].branch.as_deref(), Some("feat/x"));
        assert!(wts[2].prunable);
    }

    #[test]
    fn parses_status_z() {
        let out = b" M src/a.rs\0R  new name.rs\0old name.rs\0?? \xc3\xa4.txt\0D  gone\0UU conflict\0!! ignored\0";
        let s = parse_status(out);
        assert_eq!(s.len(), 5);
        assert_eq!(s[0].status, FileStatus::Modified);
        assert_eq!(s[1].path, "new name.rs");
        assert_eq!(s[1].old_path.as_deref(), Some("old name.rs"));
        assert_eq!(s[1].status, FileStatus::Renamed);
        assert_eq!(s[2].path, "ä.txt");
        assert_eq!(s[2].status, FileStatus::Untracked);
        assert_eq!(s[3].status, FileStatus::Deleted);
        assert_eq!(s[4].status, FileStatus::Conflicted);
    }

    #[test]
    fn parses_name_status_z() {
        let out = b"M\0a.rs\0R087\0old.rs\0new.rs\0A\0b c.rs\0";
        let s = parse_name_status(out);
        assert_eq!(s.len(), 3);
        assert_eq!(s[1].path, "new.rs");
        assert_eq!(s[1].old_path.as_deref(), Some("old.rs"));
        assert_eq!(s[2].path, "b c.rs");
        assert_eq!(s[2].status, FileStatus::Added);
    }

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn against_real_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir(&main).unwrap();
        sh(&main, &["init", "-q", "-b", "main"]);

        // Repo without commits: worktree diff falls back to the empty tree.
        std::fs::write(main.join("a.txt"), "one\ntwo\n").unwrap();
        sh(&main, &["add", "a.txt"]);
        assert!(log(&main, 0, 10).unwrap().is_empty());
        let st = status(&main).unwrap();
        assert_eq!(st[0].status, FileStatus::Added);
        assert!(working_diff(&main, &st[0]).unwrap().contains("+one"));

        std::fs::write(main.join("gone.txt"), "x\n").unwrap();
        std::fs::write(main.join("old.txt"), "rename me\nplease\nlots of content\n").unwrap();
        sh(&main, &["add", "."]);
        sh(&main, &["commit", "-q", "-m", "first"]);
        let wt2 = tmp.path().join("wt 2");
        sh(
            &main,
            &["worktree", "add", "-q", "-b", "feat", wt2.to_str().unwrap()],
        );

        std::fs::write(main.join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(main.join("new file.txt"), "hello\n").unwrap();
        std::fs::remove_file(main.join("gone.txt")).unwrap();
        sh(&main, &["mv", "old.txt", "renamed.txt"]);

        let repo = discover(&main).unwrap();
        let wts = list_worktrees(&repo.toplevel).unwrap();
        assert_eq!(wts.len(), 2);
        assert_eq!(wts[1].branch.as_deref(), Some("feat"));
        assert!(wts[1].path.ends_with("wt 2"));

        let mut st = status(&main).unwrap();
        st.sort_by(|a, b| a.path.cmp(&b.path));
        let summary: Vec<_> = st.iter().map(|c| (c.path.as_str(), c.status)).collect();
        assert_eq!(
            summary,
            vec![
                ("a.txt", FileStatus::Modified),
                ("gone.txt", FileStatus::Deleted),
                ("new file.txt", FileStatus::Untracked),
                ("renamed.txt", FileStatus::Renamed),
            ]
        );
        let d = working_diff(&main, &st[0]).unwrap();
        assert!(d.contains("-two") && d.contains("+TWO"));
        assert!(working_diff(&main, &st[2]).unwrap().contains("+hello"));
        assert!(
            working_diff(&main, &st[3])
                .unwrap()
                .contains("rename from old.txt")
        );
        assert!(status(&wt2).unwrap().is_empty());

        sh(&main, &["commit", "-q", "-am", "second"]);
        let commits = log(&main, 0, 10).unwrap();
        assert_eq!(
            commits
                .iter()
                .map(|c| c.subject.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
        let files = commit_files(&main, &commits[0]).unwrap();
        assert!(
            files.iter().any(
                |f| f.status == FileStatus::Renamed && f.old_path.as_deref() == Some("old.txt")
            )
        );
        let a = files.iter().find(|f| f.path == "a.txt").unwrap();
        assert!(commit_diff(&main, &commits[0], a).unwrap().contains("+TWO"));
        // Root commit diffs against the empty tree.
        let root_files = commit_files(&main, &commits[1]).unwrap();
        assert_eq!(root_files.len(), 3);
        assert_eq!(log(&main, 1, 10).unwrap().len(), 1);
    }

    #[test]
    fn parses_log() {
        let s = "aaa\x1fa\x1fAlice\x1f2 hours ago\x1fHEAD -> main\x1fppp qqq\x1fMerge it\x1e\n\
                 bbb\x1fb\x1fBob\x1f3 days ago\x1f\x1f\x1froot\x1e\n";
        let c = parse_log(s);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].parents, vec!["ppp", "qqq"]);
        assert_eq!(c[0].base(), "ppp");
        assert_eq!(c[1].base(), EMPTY_TREE);
        assert_eq!(c[1].subject, "root");
    }
}
