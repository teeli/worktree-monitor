//! Line comments, stored as JSON per worktree under the XDG state directory.

use crate::diff::{Diff, LineKind};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION: u32 = 1;
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

pub fn now() -> (u64, u64) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs(), d.as_nanos() as u64)
}

pub struct Store {
    /// Canonical worktree path.
    wt: PathBuf,
    /// `None` when no state directory could be determined.
    path: Option<PathBuf>,
    pub comments: Vec<Comment>,
    /// Why the file could not be read; it is never overwritten while this is set.
    pub error: Option<String>,
}

impl Store {
    pub fn load(wt: &Path) -> Self {
        let path = state_dir().map(|d| d.join("comments").join(file_name(wt)));
        Self::load_from(wt, path)
    }

    fn load_from(wt: &Path, path: Option<PathBuf>) -> Self {
        let mut store = Store {
            wt: wt.to_path_buf(),
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
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        // The schema lives in the state directory, one level above the comment files.
        let schema = write_schema(dir.parent().unwrap_or(dir))?;
        let file = CommentFile {
            schema: Some(schema),
            version: VERSION,
            worktree: self.wt.clone(),
            comments,
        };
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&file)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        self.comments = file.comments;
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
        let mut s = Store::load_from(wt, Some(path.clone()));
        assert!(s.comments.is_empty() && s.error.is_none());
        s.upsert(comment(1, Side::New, 3, "a", None)).unwrap();
        let schema = dir.path().join(SCHEMA_FILE);
        assert_eq!(std::fs::read_to_string(&schema).unwrap(), SCHEMA);
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["$schema"], file_url(&schema));
        let mut c = comment(2, Side::Old, 4, "b", Some("abc"));
        s.upsert(c.clone()).unwrap();
        c.body = "edited".into();
        s.upsert(c.clone()).unwrap();
        let s2 = Store::load_from(wt, Some(path.clone()));
        assert_eq!(s2.comments.len(), 2);
        assert_eq!(s2.comments[1], c);
        s.remove(1).unwrap();
        assert_eq!(Store::load_from(wt, Some(path.clone())).comments, vec![c]);

        std::fs::write(&path, "not json").unwrap();
        let mut broken = Store::load_from(wt, Some(path.clone()));
        assert!(broken.error.is_some());
        assert!(broken.upsert(comment(3, Side::New, 1, "x", None)).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
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
