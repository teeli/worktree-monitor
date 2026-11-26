//! Line comments, stored as JSON per worktree under the XDG state directory.

use crate::diff::{Diff, DiffLine, LineKind};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Id of a comment or reply: a UUIDv7.
pub type Id = Uuid;

const VERSION: u32 = 2;
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
    #[serde(deserialize_with = "id_compat")]
    pub id: Id,
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
    /// Set once the comment is handled (fixed or dropped); closed comments are hidden by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<Closed>,
    /// Replies, oldest first. They never open or close the comment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<Reply>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    #[serde(deserialize_with = "id_compat")]
    pub id: Id,
    /// Who wrote it: "user" from the TUI, else whatever `wtm comments reply --by` was given.
    pub by: String,
    pub body: String,
    /// Unix seconds.
    pub created: u64,
    pub updated: u64,
}

impl Reply {
    pub fn new(by: &str, body: &str) -> Self {
        let secs = now();
        Reply {
            id: new_id(),
            by: by.to_string(),
            body: body.to_string(),
            created: secs,
            updated: secs,
        }
    }
}

impl Comment {
    /// Newest change to the comment or its replies, in Unix seconds.
    pub fn touched(&self) -> u64 {
        let replies = self.replies.iter().map(|r| r.created.max(r.updated));
        replies.fold(self.created.max(self.updated), u64::max)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closed {
    /// Unix seconds.
    pub at: u64,
    /// Who closed it: "user" from the TUI, else whatever `wtm comments close --by` was given.
    pub by: String,
}

impl Closed {
    pub fn now(by: &str) -> Self {
        Closed {
            at: now(),
            by: by.to_string(),
        }
    }
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

/// Parses a comment file, refusing ones from a newer `wtm`: rewriting them as `VERSION` could
/// lose what this version doesn't know about.
fn parse(data: &[u8]) -> Result<CommentFile> {
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    let Version { version } = serde_json::from_slice(data)?;
    if version > VERSION {
        bail!(
            "version {version} is from a newer wtm (this one reads up to {VERSION}); upgrade wtm"
        );
    }
    Ok(serde_json::from_slice(data)?)
}

/// Atomically writes `file` to `path`, with the schema next to the comments directory.
fn write(path: &Path, mut file: CommentFile) -> Result<CommentFile> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    // The schema lives in the state directory, one level above the comment files.
    file.schema = Some(write_schema(dir.parent().unwrap_or(dir))?);
    // Per process, so concurrent writers (the TUI, `wtm comments`) don't share a temp file.
    let tmp = tmp_path(path);
    std::fs::write(&tmp, serde_json::to_vec_pretty(&file)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(file)
}

fn tmp_path(path: &Path) -> PathBuf {
    path.with_extension(format!("json.{}.tmp", std::process::id()))
}

/// Exclusive lock on the comments directory `dir` (created if missing), held until the file is
/// dropped. Serializes read-modify-write cycles of the TUI and `wtm comments` across processes.
/// Not reentrant: a process must not take it twice.
fn lock(dir: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(".lock");
    let file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.lock()
        .with_context(|| format!("locking {}", path.display()))?;
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
    // Tagging rewrites files, so keep `Store::modify` out meanwhile; a missing directory has
    // nothing to prune and isn't created.
    let _lock = match dir.exists().then(|| lock(dir)).transpose() {
        Ok(l) => l,
        Err(e) => return (0, Some(e)),
    };
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
fn comment_files(dir: &Path) -> Result<impl Iterator<Item = (PathBuf, CommentFile)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => Some(e),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    Ok(entries
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|path| {
            let file = serde_json::from_slice::<CommentFile>(&std::fs::read(&path).ok()?).ok()?;
            (path.file_name() == Some(file_name(&file.worktree).as_ref())).then_some((path, file))
        }))
}

/// Why the worktree of a comment file is considered gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gone {
    /// Its repository exists but no longer has it.
    Worktree,
    /// Its repository exists and still lists it (e.g. on an unmounted volume, or deleted without
    /// `git worktree remove`), or git can't tell. Never stale for being gone.
    Listed,
    /// Its repository is missing too, possibly only on a volume that isn't mounted.
    Repo,
    /// The file is from an older version that didn't record the repository.
    Unknown,
}

/// Why a comment file is stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stale {
    Gone(Gone),
    /// Untouched for this many days.
    Idle(u64),
}

impl std::fmt::Display for Stale {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Stale::Gone(Gone::Worktree) => f.write_str("worktree removed"),
            Stale::Gone(Gone::Listed) => f.write_str("worktree missing"),
            Stale::Gone(Gone::Repo) => f.write_str("repository missing"),
            Stale::Gone(Gone::Unknown) => f.write_str("repository unknown"),
            Stale::Idle(1) => f.write_str("untouched for 1 day"),
            Stale::Idle(days) => write!(f, "untouched for {days} days"),
        }
    }
}

/// A comment file, as seen by `wtm comments prune`.
#[derive(Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub worktree: PathBuf,
    pub comments: usize,
    /// `None` while the worktree exists, or may (e.g. its path is unreachable).
    pub gone: Option<Gone>,
    /// Unix seconds of the newest comment change; the file's mtime when it has no comments.
    pub touched: u64,
}

impl Entry {
    /// Whether the file is stale: its worktree is gone (`Gone::Worktree`, or any with
    /// `deleted_repos`), or it was untouched for at least `max_age` days as of `now`.
    pub fn stale(&self, deleted_repos: bool, max_age: Option<u64>, now: u64) -> Option<Stale> {
        match self.gone {
            Some(g @ Gone::Worktree) => return Some(Stale::Gone(g)),
            Some(g @ (Gone::Repo | Gone::Unknown)) if deleted_repos => {
                return Some(Stale::Gone(g));
            }
            _ => {}
        }
        let days = now.saturating_sub(self.touched) / 86_400;
        max_age
            .is_some_and(|max| days >= max)
            .then_some(Stale::Idle(days))
    }
}

/// The comments directory and all its comment files, across all repositories.
pub fn scan() -> Result<(PathBuf, Vec<Entry>)> {
    let dir =
        comments_dir().ok_or_else(|| anyhow!("no state directory (set XDG_STATE_HOME or HOME)"))?;
    let found = scan_in(&dir)?;
    Ok((dir, found))
}

/// Worktrees a repository still lists, per git.
struct Registered {
    /// As git reports them, and canonical where they (or their parents) can be resolved.
    paths: HashSet<PathBuf>,
    /// Some listed worktree couldn't be resolved, or the main worktree's location isn't
    /// recorded (`git init --separate-git-dir`): any missing worktree may be one of them.
    unsure: bool,
}

fn scan_in(dir: &Path) -> Result<Vec<Entry>> {
    let mut repos: HashMap<PathBuf, Option<Registered>> = HashMap::new();
    let mut found = Vec::new();
    for (path, file) in comment_files(dir)? {
        // Unreachable (e.g. permission denied or a stale network mount) isn't gone.
        let gone = if !matches!(file.worktree.try_exists(), Ok(false)) {
            None
        } else {
            match &file.repo {
                None => Some(Gone::Unknown),
                Some(r) => match r.try_exists() {
                    Ok(false) => Some(Gone::Repo),
                    Err(_) => None,
                    Ok(true) => {
                        let reg = repos.entry(r.clone()).or_insert_with(|| registered(r));
                        match reg {
                            Some(reg) if !reg.unsure && !reg.paths.contains(&file.worktree) => {
                                Some(Gone::Worktree)
                            }
                            _ => Some(Gone::Listed),
                        }
                    }
                },
            }
        };
        let touched = match file.comments.iter().map(Comment::touched).max() {
            Some(t) => t,
            // Unknown counts as now, so it never looks old.
            None => std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or_else(now, |d| d.as_secs()),
        };
        found.push(Entry {
            path,
            worktree: file.worktree,
            comments: file.comments.len(),
            gone,
            touched,
        });
    }
    found.sort_by(|a, b| a.worktree.cmp(&b.worktree));
    Ok(found)
}

/// The worktrees git lists for the common dir `repo`, missing ones included; `None` when git
/// can't tell.
fn registered(repo: &Path) -> Option<Registered> {
    let mut reg = Registered {
        paths: HashSet::new(),
        unsure: false,
    };
    for w in crate::git::list_worktrees_of(repo).ok()? {
        match resolve(&w.path) {
            Some(p) if p == repo => reg.unsure = true,
            Some(p) => {
                reg.paths.insert(p);
            }
            None => reg.unsure = true,
        }
        reg.paths.insert(w.path);
    }
    Some(reg)
}

/// The canonical form of `path`, or of its parent joined with its name when it's missing;
/// `None` when neither can be resolved (e.g. on an unmounted volume).
fn resolve(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok().or_else(|| {
        let (parent, name) = path.parent().zip(path.file_name())?;
        Some(parent.canonicalize().ok()?.join(name))
    })
}

/// Unix seconds.
pub fn now() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    d.as_secs()
}

/// A new, time-ordered id.
pub fn new_id() -> Id {
    Uuid::now_v7()
}

/// Reads an id: a UUID, or a version 1 id (creation time in nanoseconds), which maps to the
/// same UUIDv7 on every read (see `legacy_id`).
fn id_compat<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Id, D::Error> {
    struct IdVisitor;
    impl serde::de::Visitor<'_> for IdVisitor {
        type Value = Id;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a UUID string, or an integer id from version 1")
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Id, E> {
            Uuid::parse_str(v).map_err(|e| E::custom(format_args!("invalid id {v:?}: {e}")))
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Id, E> {
            Ok(legacy_id(v))
        }
    }
    d.deserialize_any(IdVisitor)
}

/// A fixed id for tests.
#[cfg(test)]
pub fn test_id(n: u128) -> Id {
    Id::from_u128(n)
}

/// The UUIDv7 of a version 1 id (Unix time in nanoseconds): the milliseconds as its timestamp and
/// the rest (< 2^20) in its last bytes, which the version and variant bits leave alone, so every
/// value maps to a different UUID.
fn legacy_id(nanos: u64) -> Id {
    let mut bytes = [0; 10];
    bytes[6..].copy_from_slice(&((nanos % 1_000_000) as u32).to_be_bytes());
    uuid::Builder::from_unix_timestamp_millis(nanos / 1_000_000, &bytes).into_uuid()
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

    pub(crate) fn load_from(wt: &Path, repo: &Path, path: Option<PathBuf>) -> Self {
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

    /// Re-reads the file to pick up other writers (another instance, `wtm comments close`).
    /// The files are small, so this reads unconditionally rather than trusting mtimes, which
    /// can be too coarse to tell writes apart. Returns whether the comments changed.
    pub fn reload_if_changed(&mut self) -> bool {
        let (comments, error) = match self.read() {
            Ok(c) => (c, None),
            Err(e) => (self.comments.clone(), Some(format!("{e:#}"))),
        };
        let changed = comments != self.comments || error != self.error;
        (self.comments, self.error) = (comments, error);
        changed
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
        Ok(parse(&data)
            .with_context(|| format!("parsing {}", path.display()))?
            .comments)
    }

    /// Re-reads the file (another instance may have written it), applies `f` and writes it back.
    fn modify(&mut self, f: impl FnOnce(&mut Vec<Comment>)) -> Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| anyhow!("no state directory (set XDG_STATE_HOME or HOME)"))?;
        let _lock = lock(path.parent().unwrap_or(Path::new(".")))?;
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

    pub fn remove(&mut self, id: Id) -> Result<()> {
        self.modify(|all| all.retain(|c| c.id != id))
    }

    /// Applies `f` to the stored comment `id`, leaving all others as they are on disk.
    /// Returns whether the comment still exists.
    pub fn edit(&mut self, id: Id, f: impl FnOnce(&mut Comment)) -> Result<bool> {
        let mut found = false;
        self.modify(|all| {
            if let Some(c) = all.iter_mut().find(|c| c.id == id) {
                f(c);
                found = true;
            }
        })?;
        Ok(found)
    }

    /// Closes (`Some`) or reopens (`None`) comment `id`; closing keeps an earlier close.
    /// Returns whether the comment exists.
    pub fn set_closed(&mut self, id: Id, closed: Option<Closed>) -> Result<bool> {
        self.edit(id, |c| match closed {
            Some(new) => {
                c.closed.get_or_insert(new);
            }
            None => c.closed = None,
        })
    }

    /// Appends `reply` to comment `id`. Returns the reply's id, or `None` if the comment doesn't
    /// exist.
    pub fn add_reply(&mut self, id: Id, reply: Reply) -> Result<Option<Id>> {
        let mut added = None;
        self.edit(id, |c| {
            added = Some(reply.id);
            c.replies.push(reply);
        })?;
        Ok(added)
    }

    /// Replaces the body of reply `reply` of comment `id`, if it changed. Returns whether the
    /// reply exists.
    pub fn edit_reply(&mut self, id: Id, reply: Id, body: &str) -> Result<bool> {
        let unchanged =
            |c: &Comment| c.id == id && c.replies.iter().any(|r| r.id == reply && r.body == body);
        // Nothing to write, and `updated` stays.
        if self.comments.iter().any(unchanged) {
            return Ok(true);
        }
        let mut found = false;
        self.edit(id, |c| {
            if let Some(r) = c.replies.iter_mut().find(|r| r.id == reply) {
                if r.body != body {
                    r.body = body.to_string();
                    r.updated = now();
                }
                found = true;
            }
        })?;
        Ok(found)
    }

    /// Deletes reply `reply` of comment `id`. Returns whether it existed.
    pub fn remove_reply(&mut self, id: Id, reply: Id) -> Result<bool> {
        let mut found = false;
        self.edit(id, |c| {
            let before = c.replies.len();
            c.replies.retain(|r| r.id != reply);
            found = c.replies.len() != before;
        })?;
        Ok(found)
    }

    /// Moves comments to the commits their lines are now in: working tree comments to where
    /// they were committed, comments on rewritten commits to their rewritten ones. Each move
    /// `(id, from, to)` applies only while the comment is still on `from`.
    pub fn move_to_commits(&mut self, moves: &[(Id, Option<String>, String)]) -> Result<()> {
        self.modify(|all| {
            for c in all.iter_mut() {
                if let Some((_, _, to)) = moves
                    .iter()
                    .find(|(id, from, _)| *id == c.id && *from == c.commit)
                {
                    c.commit = Some(to.clone());
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

/// Whether two patches of a file make the same changes, ignoring line numbers and headers
/// (a per-file patch-id), so a commit can be recognized after a rebase.
pub fn same_patch(a: &Diff, b: &Diff) -> bool {
    let changes = |d: &Diff| {
        d.lines
            .iter()
            .filter(|l| matches!(l.kind, LineKind::Add | LineKind::Del))
            .map(|l| (l.kind, l.text.clone()))
            .collect::<Vec<_>>()
    };
    let a = changes(a);
    !a.is_empty() && a == changes(b)
}

/// The commit in `history` (oldest first) that a comment on a rewritten commit, whose patch
/// for the comment's file was `old`, most likely became: the only one with the same patch
/// (a rebase), else the one its line is in (a squash, fixup or conflict resolution; see
/// `resolve_commit`). `None` when its line is gone (e.g. a fixup rewrote it).
pub fn resolve_rewritten<'a>(
    c: &Comment,
    old: &Diff,
    history: &'a [(String, Diff)],
) -> Option<&'a str> {
    let mut same = history.iter().filter(|(_, d)| same_patch(old, d));
    if let (Some((sha, _)), None) = (same.next(), same.next()) {
        return Some(sha);
    }
    resolve_commit(c, history)
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
    /// The comment (index into `Store::comments`) of a range shown further down that covers
    /// diff line `i`.
    pub fn covering(&self, i: usize) -> Option<usize> {
        // Of overlapping ranges, the one ending nearest below `i`, whose box is closest.
        self.by_line
            .iter()
            .filter(|&(&end, a)| a.start <= i && i < end)
            .min_by_key(|&(&end, _)| end)
            .map(|(_, a)| a.comment)
    }

    pub fn total(&self) -> usize {
        self.by_line.len() + self.outdated
    }
}

/// Indices of `comments` in display order: working tree first, then by commit in log order
/// (`rank` gives a commit's position in the log), then commits not in the log by age; within
/// each, by file and line. Closed comments are left out unless `include_closed`.
pub fn order(
    comments: &[Comment],
    include_closed: bool,
    rank: impl Fn(&str) -> Option<usize>,
) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..comments.len())
        .filter(|&i| include_closed || comments[i].closed.is_none())
        .collect();
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

/// Thread entries of the comments at `order` (indices into `comments`), in that order: each
/// comment (`None`) followed by its replies (their ids).
pub fn entries(comments: &[Comment], order: &[usize]) -> Vec<(usize, Option<Id>)> {
    order
        .iter()
        .flat_map(|&ci| {
            let replies = comments[ci].replies.iter().map(move |r| (ci, Some(r.id)));
            std::iter::once((ci, None)).chain(replies)
        })
        .collect()
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
/// whose first line can't be found are shown as single-line comments. Closed comments are
/// left out unless `include_closed`.
pub fn anchor(
    comments: &[Comment],
    include_closed: bool,
    diff: &Diff,
    file: &str,
    commit: Option<&str>,
) -> Anchors {
    let mine: Vec<(usize, LinePos)> = comments
        .iter()
        .enumerate()
        .filter(|(_, c)| c.file == file && c.commit.as_deref() == commit)
        .filter(|(_, c)| include_closed || c.closed.is_none())
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
            id: test_id(id.into()),
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
            closed: None,
            replies: Vec::new(),
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
        c.closed = Some(Closed {
            at: 1,
            by: "user".into(),
        });
        c.replies.push(Reply::new("claude", "done"));
        let json = serde_json::to_value(&c).unwrap();
        let defs = &schema["$defs"];
        assert_eq!(keys(&json), keys(&defs["comment"]["properties"]));
        assert_eq!(keys(&json["start"]), keys(&defs["linePos"]["properties"]));
        assert_eq!(keys(&json["closed"]), keys(&defs["closed"]["properties"]));
        let reply = &json["replies"][0];
        assert_eq!(keys(reply), keys(&defs["reply"]["properties"]));
        assert_eq!(keys(reply), {
            let mut r: Vec<String> = defs["reply"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            r.sort();
            r
        });
        // Optional fields are left out when unset.
        let bare = serde_json::to_value(comment(1, Side::New, 1, "x", None)).unwrap();
        assert!(bare.get("replies").is_none());
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
        s.remove(test_id(1)).unwrap();
        assert_eq!(
            Store::load_from(wt, repo, Some(path.clone())).comments,
            vec![c.clone()]
        );
        // Only comments still on the expected commit move.
        s.move_to_commits(&[(test_id(2), None, "def".into())])
            .unwrap();
        assert_eq!(s.comments[0].commit.as_deref(), Some("abc"));
        s.move_to_commits(&[(test_id(2), Some("abc".into()), "def".into())])
            .unwrap();
        assert_eq!(s.comments[0].commit.as_deref(), Some("def"));
        s.move_to_commits(&[(test_id(2), Some("def".into()), "abc".into())])
            .unwrap();
        // Edits touch only the given comment, and only what `f` changes.
        assert!(s.edit(test_id(2), |c| c.body = "patched".into()).unwrap());
        let stored = &Store::load_from(wt, repo, Some(path.clone())).comments[0];
        assert_eq!((stored.body.as_str(), stored.line), ("patched", 4));
        assert!(!s.edit(test_id(99), |_| unreachable!()).unwrap());

        // Closing keeps the first close; reopening clears it.
        let first = Closed {
            at: 5,
            by: "claude".into(),
        };
        assert!(s.set_closed(test_id(2), Some(first.clone())).unwrap());
        assert!(s.set_closed(test_id(2), Some(Closed::now("user"))).unwrap());
        let stored = Store::load_from(wt, repo, Some(path.clone()));
        assert_eq!(stored.comments[0].closed.as_ref(), Some(&first));
        assert!(s.set_closed(test_id(2), None).unwrap());
        assert!(s.comments[0].closed.is_none());
        assert!(!s.set_closed(test_id(99), None).unwrap());

        // Replies are added, edited and removed without touching the comment's state.
        s.set_closed(test_id(2), Some(first.clone())).unwrap();
        let r1 = Reply {
            id: test_id(10),
            ..Reply::new("claude", "fixed")
        };
        let r2 = Reply {
            id: test_id(11),
            ..Reply::new("user", "thanks")
        };
        assert_eq!(
            s.add_reply(test_id(2), r1.clone()).unwrap(),
            Some(test_id(10))
        );
        assert_eq!(
            s.add_reply(test_id(2), r2.clone()).unwrap(),
            Some(test_id(11))
        );
        assert_eq!(s.add_reply(test_id(99), r1.clone()).unwrap(), None);
        // Unchanged text isn't written, so `updated` stays.
        s.edit(test_id(2), |c| c.replies[0].updated = 3).unwrap();
        assert!(s.edit_reply(test_id(2), test_id(10), "fixed").unwrap());
        assert_eq!(s.comments[0].replies[0].updated, 3);
        assert!(
            s.edit_reply(test_id(2), test_id(10), "fixed in abc")
                .unwrap()
        );
        assert_ne!(s.comments[0].replies[0].updated, 3);
        assert!(!s.edit_reply(test_id(2), test_id(99), "x").unwrap());
        let stored = &Store::load_from(wt, repo, Some(path.clone())).comments[0];
        let bodies: Vec<&str> = stored.replies.iter().map(|r| r.body.as_str()).collect();
        assert_eq!(bodies, ["fixed in abc", "thanks"]);
        assert_eq!(stored.closed.as_ref(), Some(&first));
        assert!(s.remove_reply(test_id(2), test_id(10)).unwrap());
        assert!(!s.remove_reply(test_id(2), test_id(10)).unwrap());
        assert_eq!(s.comments[0].replies, vec![r2]);
        assert_eq!(s.comments[0].closed.as_ref(), Some(&first));
        s.set_closed(test_id(2), None).unwrap();

        std::fs::write(&path, "not json").unwrap();
        let mut broken = Store::load_from(wt, repo, Some(path.clone()));
        assert!(broken.error.is_some());
        assert!(broken.upsert(comment(3, Side::New, 1, "x", None)).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
    }

    #[test]
    fn migrates_integer_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comments").join("wt.json");
        let (wt, repo) = (Path::new("/wt"), Path::new("/repo/.git"));
        let nanos = 1_764_000_000_123_456_789_u64;
        let mut c = serde_json::to_value(comment(0, Side::New, 1, "a", None)).unwrap();
        c["id"] = nanos.into();
        c["replies"] = serde_json::json!([
            {"id": nanos, "by": "user", "body": "r1", "created": 0, "updated": 0},
            {"id": nanos + 1, "by": "user", "body": "r2", "created": 0, "updated": 0},
        ]);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let v1 = serde_json::json!({"version": 1, "worktree": wt, "comments": [c]});
        std::fs::write(&path, v1.to_string()).unwrap();

        let mut s = Store::load_from(wt, repo, Some(path.clone()));
        assert_eq!(s.error, None);
        let c = s.comments[0].clone();
        assert_eq!(c.id.get_version_num(), 7);
        let ts = c.id.get_timestamp().unwrap().to_unix();
        assert_eq!(ts, (nanos / 1_000_000_000, 123_000_000));
        // Stable across reads, distinct per value.
        assert_eq!(
            Store::load_from(wt, repo, Some(path.clone())).comments[0],
            c
        );
        assert_eq!(c.replies[0].id, c.id);
        assert_ne!(c.replies[0].id, c.replies[1].id);

        // Any write persists them as UUIDs, unchanged.
        assert!(s.set_closed(c.id, Some(Closed::now("user"))).unwrap());
        let file: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file["version"], 2);
        assert_eq!(file["comments"][0]["id"], c.id.to_string());
        assert_eq!(
            file["comments"][0]["replies"][1]["id"],
            c.replies[1].id.to_string()
        );
        let reread = &Store::load_from(wt, repo, Some(path)).comments[0];
        assert_eq!((reread.id, &reread.replies), (c.id, &c.replies));
    }

    #[test]
    fn new_ids_are_distinct() {
        let (a, b) = (Reply::new("a", "x"), Reply::new("a", "x"));
        assert_ne!(a.id, b.id);
        assert_eq!(a.id.get_version_num(), 7);
    }

    #[test]
    fn legacy_ids_keep_every_bit() {
        // Values differing only in high or low bits, which version/variant bits could clobber.
        let vals = [
            0,
            1,
            999_999,
            1_000_000,
            1 << 62,
            (1 << 62) | (1 << 63),
            u64::MAX,
            u64::MAX - 1,
        ];
        let ids: HashSet<Id> = vals.iter().map(|&n| legacy_id(n)).collect();
        assert_eq!(ids.len(), vals.len());
        assert!(ids.iter().all(|id| id.get_version_num() == 7));
    }

    #[test]
    fn rejects_bad_ids_and_newer_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comments").join("wt.json");
        let (wt, repo) = (Path::new("/wt"), Path::new("/repo/.git"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut c = serde_json::to_value(comment(1, Side::New, 1, "a", None)).unwrap();
        let load = |c: &serde_json::Value, version| {
            let f = serde_json::json!({"version": version, "worktree": wt, "comments": [c]});
            std::fs::write(&path, f.to_string()).unwrap();
            Store::load_from(wt, repo, Some(path.clone()))
        };
        assert_eq!(load(&c, VERSION).error, None);

        c["id"] = "0193-oops".into();
        let err = load(&c, VERSION).error.unwrap();
        assert!(err.contains("invalid id \"0193-oops\""), "{err}");

        let mut s = load(&c, VERSION + 1);
        assert!(s.error.as_deref().unwrap().contains("newer wtm"));
        // Never rewritten.
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(s.set_closed(test_id(1), None).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn reloads_changes_from_other_writers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comments").join("wt.json");
        let (wt, repo) = (Path::new("/wt"), Path::new("/repo/.git"));
        let mut mine = Store::load_from(wt, repo, Some(path.clone()));
        mine.upsert(comment(1, Side::New, 1, "a", None)).unwrap();
        // Our own write is not a change.
        assert!(!mine.reload_if_changed());
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut other = Store::load_from(wt, repo, Some(path.clone()));
        other
            .set_closed(test_id(1), Some(Closed::now("agent")))
            .unwrap();
        // Seen even when the mtime doesn't change (coarse-grained file systems).
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        assert!(mine.reload_if_changed());
        assert!(mine.comments[0].closed.is_some());
        assert!(!mine.reload_if_changed());
    }

    /// Concurrent writers (each with its own lock handle, like separate processes) don't lose
    /// each other's changes.
    #[test]
    fn concurrent_writers_keep_all_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comments").join("wt.json");
        let writers: Vec<_> = (0..4u64)
            .map(|w| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let mut s = Store::load_from(Path::new("/wt"), Path::new("/r"), Some(path));
                    for k in 0..25 {
                        s.upsert(comment(w * 100 + k, Side::New, 1, "a", None))
                            .unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let s = Store::load_from(Path::new("/wt"), Path::new("/r"), Some(path));
        assert_eq!(s.comments.len(), 100);
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
        let ids = |cs: &[Comment], all| -> Vec<u128> {
            order(cs, all, |sha| log.iter().position(|s| *s == sha))
                .into_iter()
                .map(|i| cs[i].id.as_u128())
                .collect()
        };
        assert_eq!(ids(&cs, false), vec![6, 3, 4, 7, 1, 5, 2]);
        cs[3].closed = Some(Closed::now("user"));
        assert_eq!(ids(&cs, false), vec![6, 3, 7, 1, 5, 2]);
        assert_eq!(ids(&cs, true), vec![6, 3, 4, 7, 1, 5, 2]);
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
    fn resolves_rewritten_commits() {
        let d = diff::parse;
        let h = |sha: &str, raw: &str| (sha.to_string(), d(raw));
        let old = d("@@ -1,2 +1,2 @@\n x\n-y\n+z\n");
        // Same changes at other line numbers, with other context: the same patch.
        assert!(same_patch(&old, &d("@@ -7,3 +7,3 @@\n q\n-y\n+z\n r\n")));
        assert!(!same_patch(&old, &d("@@ -1,2 +1,2 @@\n x\n-y\n+Z\n")));
        assert!(!same_patch(&d(""), &d("")));

        let rebased = vec![
            h("other", "@@ -1,1 +1,2 @@\n x\n+x\n"),
            h("moved", "@@ -5,2 +5,2 @@\n x\n-y\n+z\n"),
        ];
        let r = |c: &Comment, hist: &[(String, Diff)]| {
            resolve_rewritten(c, &old, hist).map(String::from)
        };
        // An unchanged line, which content alone can't place, follows its patch.
        let ctx = comment(1, Side::New, 1, "x", Some("old"));
        assert_eq!(r(&ctx, &rebased).as_deref(), Some("moved"));
        // Squashed with another change: found by its line.
        let squashed = vec![h("sq", "@@ -1,2 +1,3 @@\n x\n-y\n+z\n+w\n")];
        let line = comment(2, Side::New, 2, "z", Some("old"));
        assert_eq!(r(&line, &squashed).as_deref(), Some("sq"));
        assert_eq!(r(&ctx, &squashed), None);
        // A fixup rewrote the line.
        let fixed = vec![h("fx", "@@ -1,2 +1,2 @@\n x\n-y\n+z2\n")];
        assert_eq!(r(&line, &fixed), None);
        // Two identical patches are ambiguous: fall back to the line (nearest, then oldest).
        let twice = vec![
            h("first", "@@ -9,2 +9,2 @@\n x\n-y\n+z\n"),
            rebased[1].clone(),
        ];
        assert_eq!(r(&line, &twice).as_deref(), Some("moved"));
    }

    #[test]
    fn lists_thread_entries() {
        let mut cs = vec![
            comment(1, Side::New, 1, "x", None),
            comment(2, Side::New, 2, "x", None),
        ];
        for id in [7, 8] {
            cs[1].replies.push(Reply {
                id: test_id(id),
                ..Reply::new("a", "r")
            });
        }
        assert_eq!(
            entries(&cs, &[1, 0]),
            vec![
                (1, None),
                (1, Some(test_id(7))),
                (1, Some(test_id(8))),
                (0, None)
            ]
        );
        // The newest reply counts as a change.
        assert_eq!(cs[0].touched(), 0);
        (cs[1].replies[0].created, cs[1].replies[0].updated) = (0, 0);
        cs[1].replies[1] = Reply {
            created: 5,
            updated: 9,
            ..cs[1].replies[1].clone()
        };
        assert_eq!(cs[1].touched(), 9);
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
    fn picks_stale_entries() {
        let day = 86_400;
        let entry = |gone, touched| Entry {
            path: PathBuf::new(),
            worktree: PathBuf::new(),
            comments: 0,
            gone,
            touched,
        };
        let now = 100 * day;
        let fresh = entry(None, now - day);
        let old = entry(None, now - 30 * day);
        let removed = entry(Some(Gone::Worktree), now);
        let old_unknown = entry(Some(Gone::Unknown), now - 40 * day);
        let listed = entry(Some(Gone::Listed), now - 40 * day);
        assert_eq!(listed.stale(true, None, now), None);
        assert_eq!(listed.stale(true, Some(40), now), Some(Stale::Idle(40)));
        assert_eq!(Stale::Idle(1).to_string(), "untouched for 1 day");
        assert_eq!(Stale::Idle(2).to_string(), "untouched for 2 days");
        assert_eq!(fresh.stale(true, Some(2), now), None);
        assert_eq!(fresh.stale(false, Some(1), now), Some(Stale::Idle(1)));
        assert_eq!(old.stale(true, None, now), None);
        assert_eq!(old.stale(false, Some(30), now), Some(Stale::Idle(30)));
        assert_eq!(old.stale(false, Some(31), now), None);
        assert_eq!(
            removed.stale(false, None, now),
            Some(Stale::Gone(Gone::Worktree))
        );
        // Age alone is enough, without `deleted_repos`.
        assert_eq!(old_unknown.stale(false, None, now), None);
        assert_eq!(
            old_unknown.stale(false, Some(30), now),
            Some(Stale::Idle(40))
        );
        assert_eq!(
            old_unknown.stale(true, Some(30), now),
            Some(Stale::Gone(Gone::Unknown))
        );
        // Touched in the future (clock skew) is fresh.
        assert_eq!(entry(None, now + day).stale(false, Some(1), now), None);
    }

    #[test]
    fn scans_comment_files() {
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
        // A worktree added through a symlink, on a volume that's then unmounted: neither it nor
        // its parent resolves, so a removed worktree of the same repo can't be told apart.
        let vol = root.join("vol");
        std::fs::create_dir(&vol).unwrap();
        std::os::unix::fs::symlink(&vol, root.join("link")).unwrap();
        let other = root.join("other");
        git(&root, &["init", "-q", "other"]);
        git(&other, &["commit", "-q", "--allow-empty", "-m", "x"]);
        git(&other, &["worktree", "add", "-q", "../link/sub/linked"]);
        git(&other, &["worktree", "add", "-q", "../other-removed"]);
        git(&other, &["worktree", "remove", "../other-removed"]);
        std::fs::rename(&vol, root.join("vol-unmounted")).unwrap();
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
        put(&root.join("vol/sub/linked"), Some(&other.join(".git")));
        put(&root.join("other-removed"), Some(&other.join(".git")));
        std::fs::write(comments.join("%broken.json"), "not json").unwrap();
        std::fs::copy(
            comments.join(file_name(&root.join("removed"))),
            comments.join("renamed.json"),
        )
        .unwrap();

        let found: Vec<_> = scan_in(&comments)
            .unwrap()
            .into_iter()
            .map(|e| {
                assert_eq!(e.path, comments.join(file_name(&e.worktree)));
                assert_eq!((e.comments, e.touched), (1, 0));
                (e.worktree.strip_prefix(&root).unwrap().to_owned(), e.gone)
            })
            .collect();
        let p = PathBuf::from;
        assert_eq!(
            found,
            vec![
                (p("away"), Some(Gone::Listed)),
                (p("gone-repo"), Some(Gone::Repo)),
                (p("legacy"), Some(Gone::Unknown)),
                (p("main"), None),
                (p("other-removed"), Some(Gone::Listed)),
                (p("rel"), Some(Gone::Listed)),
                (p("removed"), Some(Gone::Worktree)),
                (p("sepwt"), Some(Gone::Listed)),
                (p("vol/sub/linked"), Some(Gone::Listed)),
            ]
        );
        assert!(scan_in(&root.join("none")).unwrap().is_empty());
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
        std::fs::create_dir(tmp_path(&stuck)).unwrap();
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
        let a = anchor(&cs, false, &d, "f", None);
        assert_eq!(a.outdated, 1);
        assert_eq!(at(a), vec![(2, 1), (3, 0), (4, 2)]);
        assert_eq!(at(anchor(&cs, false, &d, "f", Some("s"))), vec![(5, 4)]);
        assert_eq!(anchor(&cs, false, &d, "other", None).total(), 0);
        // Closed comments are only placed when asked for.
        let mut cs = cs;
        cs[0].closed = Some(Closed::now("user"));
        cs[3].closed = Some(Closed::now("user"));
        let a = anchor(&cs, false, &d, "f", None);
        assert_eq!(a.outdated, 0);
        assert_eq!(at(a), vec![(2, 1), (4, 2)]);
        let a = anchor(&cs, true, &d, "f", None);
        assert_eq!(a.outdated, 1);
        assert_eq!(at(a), vec![(2, 1), (3, 0), (4, 2)]);
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
        let a = anchor(&cs, false, &d, "f", None);
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
        let a = anchor(&[drifted, lost], false, &d, "f", None);
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
        assert_eq!(a.covering(1), None);
        assert_eq!(a.covering(2), Some(0));
        assert_eq!(a.covering(3), None);

        // Overlapping ranges: the one ending nearest below wins, regardless of map order.
        let mut a = Anchors::default();
        for (end, comment, start) in [(9, 0, 1), (6, 1, 3), (7, 2, 4)] {
            a.by_line.insert(end, Anchor { comment, start });
        }
        assert_eq!(a.covering(2), Some(0));
        assert_eq!(a.covering(4), Some(1));
        assert_eq!(a.covering(6), Some(2));
        assert_eq!(a.covering(8), Some(0));
    }
}
