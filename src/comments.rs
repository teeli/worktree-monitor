//! Line comments, stored as JSON per worktree under the XDG state directory.

use crate::diff::{Diff, DiffLine, LineKind};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION: u32 = 1;
/// Furthest a committed line may be from where a working tree comment was written.
const MAX_DRIFT: u32 = 100;
/// JSON schema of the comment files; written next to them and referenced by `$schema`.
const SCHEMA: &str = include_str!("../schema/comments.schema.json");
const SCHEMA_FILE: &str = "comments.schema.json";

/// Which side of the diff a line number refers to: deleted lines only exist in the old file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Old,
    New,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    /// Unique within the worktree (creation time in nanoseconds).
    pub id: u64,
    /// Repo-relative path.
    pub file: String,
    pub side: Side,
    pub line: u32,
    /// The line's text when the comment was written, for re-anchoring after edits.
    pub line_text: String,
    /// First line of a comment on a range; `side`/`line` is the last one, where it is shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<LinePos>,
    /// Commit the line was viewed in; `None` for the working tree.
    pub commit: Option<String>,
    /// Worktree HEAD when the comment was written.
    pub head: Option<String>,
    pub body: String,
    /// Unix seconds.
    pub created: u64,
    pub updated: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinePos {
    pub side: Side,
    pub line: u32,
    pub text: String,
}

#[derive(Serialize, Deserialize)]
struct CommentFile {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    schema: Option<String>,
    version: u32,
    worktree: PathBuf,
    /// Canonical git common dir of the worktree's repository; missing in files from older versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repo: Option<PathBuf>,
    comments: Vec<Comment>,
}

/// `$XDG_STATE_HOME/wtm`, else `~/.local/state/wtm`.
pub fn state_dir() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let base = xdg.or_else(|| {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
    })?;
    Some(base.join("wtm"))
}

fn comments_dir() -> Option<PathBuf> {
    state_dir().map(|d| d.join("comments"))
}

/// File name for a worktree's comments: the path with `%` doubled and `/` as `%` (like vim's undodir).
fn file_name(wt: &Path) -> String {
    let s = wt.to_string_lossy().replace('%', "%%").replace('/', "%");
    format!("{s}.json")
}

/// `file://` URL of an absolute path, percent-encoding everything but unreserved characters.
fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                url.push(b as char)
            }
            _ => url.push_str(&format!("%{b:02X}")),
        }
    }
    url
}

/// Writes the schema to `dir` unless it is already there, returning its URL.
fn write_schema(dir: &Path) -> Result<String> {
    let path = dir.join(SCHEMA_FILE);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(SCHEMA) {
        std::fs::write(&path, SCHEMA).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(file_url(&path))
}

/// Atomically writes `file` to `path`, with the schema next to the comments directory.
fn write(path: &Path, mut file: CommentFile) -> Result<CommentFile> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    // The schema lives in the state directory, one level above the comment files.
    file.schema = Some(write_schema(dir.parent().unwrap_or(dir))?);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&file)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(file)
}

/// Deletes the comment files of `repo`'s worktrees that are no longer in `live`, returning how
/// many and the first error. Files from older versions without a repo are tagged if their
/// worktree is live. A failing file doesn't stop the others.
pub fn prune(repo: &Path, live: &[&Path]) -> (usize, Option<anyhow::Error>) {
    match comments_dir() {
        Some(dir) => prune_in(&dir, repo, live),
        None => (0, None),
    }
}

fn prune_in(dir: &Path, repo: &Path, live: &[&Path]) -> (usize, Option<anyhow::Error>) {
    let files = match comment_files(dir) {
        Ok(f) => f,
        Err(e) => return (0, Some(e)),
    };
    let mut removed = 0;
    let mut error = None;
    for (path, mut file) in files {
        let is_live = live.contains(&file.worktree.as_path());
        let res = match &file.repo {
            Some(r) if r == repo && !is_live => std::fs::remove_file(&path)
                .map(|()| removed += 1)
                .with_context(|| format!("removing {}", path.display())),
            None if is_live => {
                file.repo = Some(repo.to_path_buf());
                write(&path, file).map(drop)
            }
            _ => Ok(()),
        };
        if let Err(e) = res {
            error.get_or_insert(e);
        }
    }
    (removed, error)
}

/// The comment files in `dir` with their contents; none when `dir` doesn't exist. Files that
/// can't be read, or whose name doesn't match their worktree, are left out (and so left alone,
/// like `Store` never overwrites them).
fn comment_files(dir: &Path) -> Result<Vec<(PathBuf, CommentFile)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    Ok(entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|path| {
            let file = serde_json::from_slice::<CommentFile>(&std::fs::read(&path).ok()?).ok()?;
            (path.file_name() == Some(file_name(&file.worktree).as_ref())).then_some((path, file))
        })
        .collect())
}

/// Why the worktree of an [`Orphan`] is considered gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gone {
    /// Its repository exists but no longer has it.
    Worktree,
    /// Its repository is missing too, possibly only on a volume that isn't mounted.
    Repo,
    /// The file is from an older version that didn't record the repository.
    Unknown,
}

/// A comment file whose worktree directory no longer exists.
#[derive(Debug)]
pub struct Orphan {
    pub path: PathBuf,
    pub worktree: PathBuf,
    pub comments: usize,
    pub gone: Gone,
}

/// The comments directory and its files whose worktrees are gone, across all repositories.
pub fn orphans() -> Result<(PathBuf, Vec<Orphan>)> {
    let dir =
        comments_dir().ok_or_else(|| anyhow!("no state directory (set XDG_STATE_HOME or HOME)"))?;
    let found = orphans_in(&dir)?;
    Ok((dir, found))
}

/// Worktrees a repository still has, per git; `None` when git can't tell.
struct Registered {
    paths: Vec<PathBuf>,
    /// The main worktree's location isn't recorded (`git init --separate-git-dir`), so any
    /// missing worktree may be it.
    main_unknown: bool,
}

fn orphans_in(dir: &Path) -> Result<Vec<Orphan>> {
    let mut repos: HashMap<PathBuf, Option<Registered>> = HashMap::new();
    let mut found = Vec::new();
    for (path, file) in comment_files(dir)? {
        // Unreachable (e.g. permission denied or a stale network mount) isn't gone.
        if !matches!(file.worktree.try_exists(), Ok(false)) {
            continue;
        }
        let gone = match &file.repo {
            None => Gone::Unknown,
            Some(r) => match r.try_exists() {
                Ok(false) => Gone::Repo,
                Err(_) => continue,
                Ok(true) => {
                    let reg = repos.entry(r.clone()).or_insert_with(|| registered(r));
                    // Missing but still registered (e.g. on an unmounted volume) isn't removed.
                    match reg {
                        Some(reg)
                            if !reg.main_unknown
                                && !reg.paths.iter().any(|p| same_path(p, &file.worktree)) =>
                        {
                            Gone::Worktree
                        }
                        _ => continue,
                    }
                }
            },
        };
        found.push(Orphan {
            path,
            worktree: file.worktree,
            comments: file.comments.len(),
            gone,
        });
    }
    found.sort_by(|a, b| a.worktree.cmp(&b.worktree));
    Ok(found)
}

/// The worktrees git lists for the common dir `repo`, missing ones included.
fn registered(repo: &Path) -> Option<Registered> {
    let paths: Vec<PathBuf> = crate::git::list_worktrees(repo)
        .ok()?
        .into_iter()
        .map(|w| w.path)
        .collect();
    let main_unknown = paths.iter().any(|p| same_path(p, repo));
    Some(Registered {
        paths,
        main_unknown,
    })
}

/// Whether `listed`, a path as git reports it, is the canonical path `wt`.
fn same_path(listed: &Path, wt: &Path) -> bool {
    listed == wt
        || listed.canonicalize().is_ok_and(|p| p == wt)
        || listed
            .parent()
            .zip(listed.file_name())
            .is_some_and(|(p, n)| p.canonicalize().is_ok_and(|p| p.join(n) == wt))
}

pub fn now() -> (u64, u64) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs(), d.as_nanos() as u64)
}

pub struct Store {
    /// Canonical worktree path.
    wt: PathBuf,
    /// Canonical git common dir of the repository.
    repo: PathBuf,
    /// `None` when no state directory could be determined.
    path: Option<PathBuf>,
    pub comments: Vec<Comment>,
    /// Why the file could not be read; it is never overwritten while this is set.
    pub error: Option<String>,
}

impl Store {
    pub fn load(wt: &Path, repo: &Path) -> Self {
        let path = comments_dir().map(|d| d.join(file_name(wt)));
        Self::load_from(wt, repo, path)
    }

    fn load_from(wt: &Path, repo: &Path, path: Option<PathBuf>) -> Self {
        let mut store = Store {
            wt: wt.to_path_buf(),
            repo: repo.to_path_buf(),
            path,
            comments: Vec::new(),
            error: None,
        };
        match store.read() {
            Ok(c) => store.comments = c,
            Err(e) => store.error = Some(format!("{e:#}")),
        }
        store
    }

    fn read(&self) -> Result<Vec<Comment>> {
        let Some(path) = &self.path else {
            return Ok(Vec::new());
        };
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let file: CommentFile =
            serde_json::from_slice(&data).with_context(|| format!("parsing {}", path.display()))?;
        Ok(file.comments)
    }

    /// Re-reads the file (another instance may have written it), applies `f` and writes it back.
    fn modify(&mut self, f: impl FnOnce(&mut Vec<Comment>)) -> Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| anyhow!("no state directory (set XDG_STATE_HOME or HOME)"))?;
        let mut comments = self
            .read()
            .inspect_err(|e| self.error = Some(format!("{e:#}")))?;
        self.error = None;
        f(&mut comments);
        let file = CommentFile {
            schema: None,
            version: VERSION,
            worktree: self.wt.clone(),
            repo: Some(self.repo.clone()),
            comments,
        };
        self.comments = write(&path, file)?.comments;
        Ok(())
    }

    /// Adds `c`, or replaces the comment with the same id.
    pub fn upsert(&mut self, c: Comment) -> Result<()> {
        self.modify(|all| match all.iter_mut().find(|x| x.id == c.id) {
            Some(x) => *x = c,
            None => all.push(c),
        })
    }

    pub fn remove(&mut self, id: u64) -> Result<()> {
        self.modify(|all| all.retain(|c| c.id != id))
    }

    /// Applies `f` to the stored comment `id`, leaving all others as they are on disk.
    /// Returns whether the comment still exists.
    pub fn edit(&mut self, id: u64, f: impl FnOnce(&mut Comment)) -> Result<bool> {
        let mut found = false;
        self.modify(|all| {
            if let Some(c) = all.iter_mut().find(|c| c.id == id) {
                f(c);
                found = true;
            }
        })?;
        Ok(found)
    }

    /// Moves working tree comments to the commits their lines were committed in.
    pub fn move_to_commits(&mut self, moves: &[(u64, String)]) -> Result<()> {
        self.modify(|all| {
            for c in all.iter_mut().filter(|c| c.commit.is_none()) {
                if let Some((_, sha)) = moves.iter().find(|(id, _)| *id == c.id) {
                    c.commit = Some(sha.clone());
                }
            }
        })
    }
}

/// The commit in `history` (oldest first) a working tree comment's line was most likely
/// committed in: one adding (or, on the old side, removing) a line with its text within
/// `MAX_DRIFT` lines of where it was, the nearest such line winning, then the oldest commit.
/// A range comment's first line must appear in that commit's diff too. Comments on unchanged
/// lines can't be told apart from any other commit touching the file, so they find nothing.
pub fn resolve_commit<'a>(c: &Comment, history: &'a [(String, Diff)]) -> Option<&'a str> {
    let changed = match c.side {
        Side::New => LineKind::Add,
        Side::Old => LineKind::Del,
    };
    let matches = |l: &DiffLine| l.kind == changed && l.text == c.line_text;
    let has_start = |d: &Diff| {
        c.start
            .as_ref()
            .is_none_or(|s| d.lines.iter().any(|l| l.text == s.text))
    };
    history
        .iter()
        .enumerate()
        .filter_map(|(k, (sha, d))| {
            let dist = (0..d.lines.len())
                .filter(|&i| matches(&d.lines[i]))
                .filter_map(|i| line_no(d, i, c.side))
                .map(|n| n.abs_diff(c.line))
                .min()?;
            (dist <= MAX_DRIFT && has_start(d)).then_some((dist, k, sha.as_str()))
        })
        .min_by_key(|&(dist, k, _)| (dist, k))
        .map(|(_, _, sha)| sha)
}

/// Where the comments of one diff are shown.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Anchors {
    /// Diff line a comment is shown below → that comment.
    pub by_line: HashMap<usize, Anchor>,
    /// Comments for this file whose line no longer exists in the diff.
    pub outdated: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// Index into `Store::comments`.
    pub comment: usize,
    /// First diff line the comment covers; the line it is shown below for single-line comments.
    pub start: usize,
}

impl Anchors {
    /// Whether diff line `i` is covered by a range comment shown further down.
    pub fn in_range(&self, i: usize) -> bool {
        self.by_line.iter().any(|(&end, a)| a.start <= i && i < end)
    }

    pub fn total(&self) -> usize {
        self.by_line.len() + self.outdated
    }
}

/// Indices of `comments` in display order: working tree first, then by commit in log order
/// (`rank` gives a commit's position in the log), then commits not in the log by age; within
/// each, by file and line.
pub fn order(comments: &[Comment], rank: impl Fn(&str) -> Option<usize>) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..comments.len()).collect();
    idx.sort_by_cached_key(|&i| {
        let c = &comments[i];
        let group = match c.commit.as_deref() {
            None => (0, 0, 0),
            Some(sha) => match rank(sha) {
                Some(r) => (1, r, 0),
                None => (2, 0, c.created),
            },
        };
        (group, c.file.clone(), c.line)
    });
    idx
}

/// Position after/before `cur` in a list of `len`; from outside the list (`None`) the first or
/// last one. `None` past the ends.
pub fn step(len: usize, cur: Option<usize>, forward: bool) -> Option<usize> {
    let next = match (cur, forward) {
        (Some(c), true) => c + 1,
        (Some(c), false) => c.checked_sub(1)?,
        (None, true) => 0,
        (None, false) => len.checked_sub(1)?,
    };
    (next < len).then_some(next)
}

/// Line number of a commentable diff line on `side`.
pub fn line_no(diff: &Diff, idx: usize, side: Side) -> Option<u32> {
    let l = diff.lines.get(idx)?;
    match (l.kind, side) {
        (LineKind::Add | LineKind::Context, Side::New) => l.new,
        (LineKind::Del | LineKind::Context, Side::Old) => l.old,
        _ => None,
    }
}

/// Places the comments for `file` (in `commit`, or the working tree) on diff lines: at their
/// line if its text is unchanged, else on the nearest line with the same text. Range comments
/// whose first line can't be found are shown as single-line comments.
pub fn anchor(comments: &[Comment], diff: &Diff, file: &str, commit: Option<&str>) -> Anchors {
    let mine: Vec<(usize, LinePos)> = comments
        .iter()
        .enumerate()
        .filter(|(_, c)| c.file == file && c.commit.as_deref() == commit)
        .map(|(ci, c)| {
            let end = LinePos {
                side: c.side,
                line: c.line,
                text: c.line_text.clone(),
            };
            (ci, end)
        })
        .collect();
    // Exact matches first, so a drifted comment can't take another one's unchanged line.
    let mut ends: Vec<Option<usize>> = vec![None; mine.len()];
    let mut taken = HashSet::new();
    for exact in [true, false] {
        for ((_, pos), end) in mine.iter().zip(&mut ends) {
            if end.is_none() {
                *end = locate(diff, pos, 0..diff.lines.len(), exact, |i| {
                    !taken.contains(&i)
                });
                taken.extend(*end);
            }
        }
    }
    let mut out = Anchors::default();
    for ((ci, _), end) in mine.into_iter().zip(ends) {
        let Some(end) = end else {
            out.outdated += 1;
            continue;
        };
        let start = comments[ci]
            .start
            .as_ref()
            .and_then(|s| locate(diff, s, 0..end, false, |_| true))
            .unwrap_or(end);
        out.by_line.insert(end, Anchor { comment: ci, start });
    }
    out
}

/// The diff line within `range` with `pos`'s text: at its line number when `exact`, else
/// the nearest one.
fn locate(
    diff: &Diff,
    pos: &LinePos,
    range: std::ops::Range<usize>,
    exact: bool,
    free: impl Fn(usize) -> bool,
) -> Option<usize> {
    let mut lines = range
        .filter_map(|i| Some((i, line_no(diff, i, pos.side)?)))
        .filter(|&(i, _)| diff.lines[i].text == pos.text && free(i));
    let found = if exact {
        lines.find(|&(_, n)| n == pos.line)
    } else {
        lines.min_by_key(|&(_, n)| n.abs_diff(pos.line))
    };
    found.map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff;

    fn comment(id: u64, side: Side, line: u32, text: &str, commit: Option<&str>) -> Comment {
        Comment {
            id,
            file: "f".into(),
            side,
            line,
            line_text: text.into(),
            start: None,
            commit: commit.map(String::from),
            head: None,
            body: format!("c{id}"),
            created: 0,
            updated: 0,
        }
    }

    #[test]
    fn escapes_file_names() {
        assert_eq!(file_name(Path::new("/a/b%c")), "%a%b%%c.json");
        assert_eq!(
            file_url(Path::new("/a b/ä.json")),
            "file:///a%20b/%C3%A4.json"
        );
    }

    /// Every field written for a comment is declared in the schema, and vice versa.
    #[test]
    fn schema_matches_model() {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
        let keys = |v: &serde_json::Value| {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        let mut c = comment(1, Side::New, 1, "x", None);
        c.start = Some(LinePos {
            side: Side::Old,
            line: 1,
            text: "y".into(),
        });
        let json = serde_json::to_value(&c).unwrap();
        let defs = &schema["$defs"];
        assert_eq!(keys(&json), keys(&defs["comment"]["properties"]));
        assert_eq!(keys(&json["start"]), keys(&defs["linePos"]["properties"]));
        let file = CommentFile {
            schema: Some("x".into()),
            version: VERSION,
            worktree: "/wt".into(),
            repo: Some("/repo".into()),
            comments: vec![],
        };
        let json = serde_json::to_value(&file).unwrap();
        assert_eq!(keys(&json), keys(&schema["properties"]));
        assert_eq!(schema["properties"]["version"]["const"], VERSION);
    }

    #[test]
    fn round_trips_and_protects_broken_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comments").join("wt.json");
        let wt = Path::new("/wt");
        let repo = Path::new("/repo/.git");
        let mut s = Store::load_from(wt, repo, Some(path.clone()));
        assert!(s.comments.is_empty() && s.error.is_none());
        s.upsert(comment(1, Side::New, 3, "a", None)).unwrap();
        let schema = dir.path().join(SCHEMA_FILE);
        assert_eq!(std::fs::read_to_string(&schema).unwrap(), SCHEMA);
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["$schema"], file_url(&schema));
        assert_eq!(written["repo"], "/repo/.git");
        let mut c = comment(2, Side::Old, 4, "b", Some("abc"));
        s.upsert(c.clone()).unwrap();
        c.body = "edited".into();
        s.upsert(c.clone()).unwrap();
        let s2 = Store::load_from(wt, repo, Some(path.clone()));
        assert_eq!(s2.comments.len(), 2);
        assert_eq!(s2.comments[1], c);
        s.remove(1).unwrap();
        assert_eq!(
            Store::load_from(wt, repo, Some(path.clone())).comments,
            vec![c.clone()]
        );
        // Only working tree comments move.
        s.move_to_commits(&[(2, "def".into())]).unwrap();
        assert_eq!(s.comments[0].commit.as_deref(), Some("abc"));
        // Edits touch only the given comment, and only what `f` changes.
        assert!(s.edit(2, |c| c.body = "patched".into()).unwrap());
        let stored = &Store::load_from(wt, repo, Some(path.clone())).comments[0];
        assert_eq!((stored.body.as_str(), stored.line), ("patched", 4));
        assert!(!s.edit(99, |_| unreachable!()).unwrap());

        std::fs::write(&path, "not json").unwrap();
        let mut broken = Store::load_from(wt, repo, Some(path.clone()));
        assert!(broken.error.is_some());
        assert!(broken.upsert(comment(3, Side::New, 1, "x", None)).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
    }

    #[test]
    fn orders_comments() {
        let mut cs = vec![
            comment(1, Side::New, 9, "x", Some("old")),
            comment(2, Side::New, 5, "x", Some("gone2")),
            comment(3, Side::New, 2, "x", None),
            comment(4, Side::New, 1, "x", Some("new")),
            comment(5, Side::New, 1, "x", Some("gone1")),
            comment(6, Side::New, 1, "x", None),
            comment(7, Side::New, 1, "x", Some("old")),
        ];
        cs[1].created = 2;
        cs[4].created = 1;
        cs[6].file = "e".into();
        let log = ["new", "old"];
        let o = order(&cs, |sha| log.iter().position(|s| *s == sha));
        let ids: Vec<u64> = o.into_iter().map(|i| cs[i].id).collect();
        assert_eq!(ids, vec![6, 3, 4, 7, 1, 5, 2]);
    }

    #[test]
    fn resolves_committed_comments() {
        let h = |sha: &str, raw: &str| (sha.to_string(), diff::parse(raw));
        let history = vec![
            h("a", "@@ -1,2 +1,2 @@\n x\n-y\n+z\n"),
            h("b", "@@ -1,2 +1,3 @@\n x\n z\n+w\n"),
        ];
        let r = |c: Comment| resolve_commit(&c, &history).map(String::from);
        assert_eq!(r(comment(1, Side::New, 2, "z", None)).as_deref(), Some("a"));
        assert_eq!(r(comment(2, Side::Old, 2, "y", None)).as_deref(), Some("a"));
        assert_eq!(r(comment(3, Side::New, 3, "w", None)).as_deref(), Some("b"));
        // Unchanged lines are ambiguous.
        assert_eq!(r(comment(4, Side::New, 1, "x", None)), None);
        assert_eq!(r(comment(5, Side::New, 1, "gone", None)), None);

        // A common line: the nearest match wins, far-away ones don't count.
        let history = vec![
            h("far", "@@ -500,1 +500,2 @@\n a\n+}\n"),
            h("near", "@@ -10,1 +10,2 @@\n a\n+}\n"),
        ];
        let r = |c: Comment| resolve_commit(&c, &history).map(String::from);
        assert_eq!(
            r(comment(6, Side::New, 12, "}", None)).as_deref(),
            Some("near")
        );
        assert_eq!(r(comment(7, Side::New, 300, "}", None)), None);
        // A range's first line must be in the same commit.
        let mut range = comment(8, Side::New, 11, "}", None);
        range.start = Some(LinePos {
            side: Side::New,
            line: 9,
            text: "missing".into(),
        });
        assert_eq!(r(range), None);
    }

    #[test]
    fn steps_through_list() {
        assert_eq!(step(3, None, true), Some(0));
        assert_eq!(step(3, None, false), Some(2));
        assert_eq!(step(3, Some(1), true), Some(2));
        assert_eq!(step(3, Some(2), true), None);
        assert_eq!(step(3, Some(0), false), None);
        assert_eq!(step(0, None, true), None);
        assert_eq!(step(0, None, false), None);
    }

    #[test]
    fn prunes_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Path::new("/repo/.git");
        let put = |wt: &str, repo: Option<&str>| {
            let path = dir.path().join(file_name(Path::new(wt)));
            let mut json = serde_json::json!({"version": 1, "worktree": wt, "comments": []});
            if let Some(r) = repo {
                json["repo"] = r.into();
            }
            std::fs::write(&path, json.to_string()).unwrap();
            path
        };
        let live = put("/live", Some("/repo/.git"));
        let dead = put("/dead", Some("/repo/.git"));
        let other = put("/other", Some("/elsewhere/.git"));
        let legacy_live = put("/legacy-live", None);
        let legacy_dead = put("/legacy-dead", None);
        let broken = dir.path().join("%broken.json");
        std::fs::write(&broken, "not json").unwrap();
        let renamed = dir.path().join("renamed.json");
        std::fs::copy(&dead, &renamed).unwrap();

        let wts = [Path::new("/live"), Path::new("/legacy-live")];
        let (removed, error) = prune_in(dir.path(), repo, &wts);
        assert_eq!(removed, 1);
        assert!(error.is_none());
        assert!(!dead.exists());
        for p in [&live, &other, &legacy_live, &legacy_dead, &broken, &renamed] {
            assert!(p.exists(), "{} was removed", p.display());
        }
        let tagged: CommentFile =
            serde_json::from_slice(&std::fs::read(&legacy_live).unwrap()).unwrap();
        assert_eq!(tagged.repo.as_deref(), Some(repo));
        let (removed, error) = prune_in(&dir.path().join("missing"), repo, &wts);
        assert_eq!(removed, 0);
        assert!(error.is_none());
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@t"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn finds_orphans() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let comments = root.join("comments");
        std::fs::create_dir(&comments).unwrap();
        let main = root.join("main");
        git(&root, &["init", "-q", "main"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "x"]);
        let repo = main.join(".git");
        // Still registered but missing (e.g. on an unmounted volume); one with a relative path.
        git(&main, &["worktree", "add", "-q", "../away"]);
        git(
            &main,
            &[
                "-c",
                "worktree.useRelativePaths=true",
                "worktree",
                "add",
                "-q",
                "../rel",
            ],
        );
        // Removed properly.
        git(&main, &["worktree", "add", "-q", "../removed"]);
        git(&main, &["worktree", "remove", "../removed"]);
        // The main worktree's location isn't recorded in a separate git dir.
        let sep = root.join("sep.git");
        git(
            &root,
            &[
                "init",
                "-q",
                "--separate-git-dir",
                sep.to_str().unwrap(),
                "sepwt",
            ],
        );
        for wt in ["away", "rel", "sepwt"] {
            std::fs::rename(root.join(wt), root.join(format!("{wt}-moved"))).unwrap();
        }
        let put = |wt: &Path, repo: Option<&Path>| {
            let mut json = serde_json::json!({
                "version": 1,
                "worktree": wt,
                "comments": [comment(1, Side::New, 1, "x", None)],
            });
            if let Some(r) = repo {
                json["repo"] = r.to_str().unwrap().into();
            }
            std::fs::write(comments.join(file_name(wt)), json.to_string()).unwrap();
        };
        put(&main, Some(&repo));
        put(&root.join("away"), Some(&repo));
        put(&root.join("rel"), Some(&repo));
        put(&root.join("removed"), Some(&repo));
        put(&root.join("sepwt"), Some(&sep));
        put(&root.join("gone-repo"), Some(&root.join("gone/.git")));
        put(&root.join("legacy"), None);
        std::fs::write(comments.join("%broken.json"), "not json").unwrap();
        std::fs::copy(
            comments.join(file_name(&root.join("removed"))),
            comments.join("renamed.json"),
        )
        .unwrap();

        let found: Vec<_> = orphans_in(&comments)
            .unwrap()
            .into_iter()
            .map(|o| {
                assert_eq!(o.path, comments.join(file_name(&o.worktree)));
                assert_eq!(o.comments, 1);
                (o.worktree.strip_prefix(&root).unwrap().to_owned(), o.gone)
            })
            .collect();
        let p = PathBuf::from;
        assert_eq!(
            found,
            vec![
                (p("gone-repo"), Gone::Repo),
                (p("legacy"), Gone::Unknown),
                (p("removed"), Gone::Worktree),
            ]
        );
        assert!(orphans_in(&root.join("none")).unwrap().is_empty());
    }

    #[test]
    fn prune_continues_after_errors() {
        let dir = tempfile::tempdir().unwrap();
        let put = |wt: &str, repo: Option<&str>| {
            let path = dir.path().join(file_name(Path::new(wt)));
            let mut json = serde_json::json!({"version": 1, "worktree": wt, "comments": []});
            if let Some(r) = repo {
                json["repo"] = r.into();
            }
            std::fs::write(&path, json.to_string()).unwrap();
            path
        };
        // Tagging this one fails: its temp file path is taken by a directory.
        let stuck = put("/live", None);
        std::fs::create_dir(stuck.with_extension("json.tmp")).unwrap();
        let dead = put("/dead", Some("/repo/.git"));
        let (removed, error) = prune_in(dir.path(), Path::new("/repo/.git"), &[Path::new("/live")]);
        assert_eq!(removed, 1);
        assert!(!dead.exists());
        assert!(error.is_some());
    }

    #[test]
    fn anchors_comments() {
        let d = diff::parse("@@ -1,3 +1,4 @@\n a\n-b\n+x\n+a\n c\n");
        // Lines: 0 hunk, 1 ` a` (1/1), 2 `-b` (old 2), 3 `+x` (new 2), 4 `+a` (new 3), 5 ` c` (3/4).
        let cs = vec![
            comment(1, Side::New, 2, "x", None),       // exact
            comment(2, Side::Old, 2, "b", None),       // exact, old side
            comment(3, Side::New, 4, "a", None),       // drifted: nearest `a` is new 3
            comment(4, Side::New, 1, "gone", None),    // outdated
            comment(5, Side::New, 4, "c", Some("s")),  // other commit
            comment(6, Side::New, 4, "c", Some("s2")), // other commit
        ];
        let at = |a: Anchors| {
            let mut v: Vec<_> = a.by_line.into_iter().map(|(i, a)| (i, a.comment)).collect();
            v.sort();
            v
        };
        let a = anchor(&cs, &d, "f", None);
        assert_eq!(a.outdated, 1);
        assert_eq!(at(a), vec![(2, 1), (3, 0), (4, 2)]);
        assert_eq!(at(anchor(&cs, &d, "f", Some("s"))), vec![(5, 4)]);
        assert_eq!(anchor(&cs, &d, "other", None).total(), 0);
    }

    #[test]
    fn exact_matches_win_over_drifted_ones() {
        // Lines: 0 hunk, 1 ` }` (new 1), 2 `+x` (new 2), 3 ` }` (new 3).
        let d = diff::parse("@@ -1,2 +1,3 @@\n }\n+x\n }\n");
        // The drifted comment comes first and its nearest `}` is new 3, which the second
        // comment is exactly on.
        let cs = vec![
            comment(1, Side::New, 4, "}", None),
            comment(2, Side::New, 3, "}", None),
        ];
        let a = anchor(&cs, &d, "f", None);
        assert_eq!(a.by_line[&3].comment, 1);
        assert_eq!(a.by_line[&1].comment, 0);
    }

    #[test]
    fn anchors_ranges() {
        // Lines: 0 hunk, 1 ` a` (new 1), 2 `+x` (new 2), 3 `+y` (new 3), 4 ` c` (new 4).
        let d = diff::parse("@@ -1,2 +1,4 @@\n a\n+x\n+y\n c\n");
        let pos = |line, text: &str| LinePos {
            side: Side::New,
            line,
            text: text.into(),
        };
        let mut drifted = comment(1, Side::New, 5, "y", None);
        drifted.start = Some(pos(3, "x"));
        let mut lost = comment(2, Side::New, 4, "c", None);
        lost.start = Some(pos(1, "gone"));
        let a = anchor(&[drifted, lost], &d, "f", None);
        assert_eq!(
            a.by_line[&3],
            Anchor {
                comment: 0,
                start: 2
            }
        );
        assert_eq!(
            a.by_line[&4],
            Anchor {
                comment: 1,
                start: 4
            }
        );
        assert!(!a.in_range(1));
        assert!(a.in_range(2));
        assert!(!a.in_range(3));
    }
}
