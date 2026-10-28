//! Unified diff parsing with per-line old/new line numbers.

const MAX_LINES: usize = 50_000;
const TAB: &str = "    ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    FileHeader,
    Hunk,
    Add,
    Del,
    Context,
    Meta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    /// Line content without the leading +/-/space marker, tabs expanded.
    pub text: String,
}

#[derive(Debug, Default, Clone)]
pub struct Diff {
    pub lines: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    // @@ -a[,b] +c[,d] @@ ...
    let mut parts = line.split_whitespace().skip(1);
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let start = |s: &str| s.split(',').next()?.parse::<u32>().ok();
    Some((start(old)?, start(new)?))
}

pub fn parse(raw: &str) -> Diff {
    let mut diff = Diff::default();
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    for line in raw.lines() {
        if diff.lines.len() >= MAX_LINES {
            diff.lines.push(DiffLine {
                kind: LineKind::Meta,
                old: None,
                new: None,
                text: format!("[diff truncated at {MAX_LINES} lines]"),
            });
            break;
        }
        let (kind, o, n, text) = if line.starts_with("diff ") {
            in_hunk = false;
            (LineKind::FileHeader, None, None, line)
        } else if line.starts_with("@@") {
            in_hunk = true;
            if let Some((a, c)) = parse_hunk_header(line) {
                (old, new) = (a, c);
            }
            (LineKind::Hunk, None, None, line)
        } else if !in_hunk {
            let kind = if line.starts_with("Binary files") {
                LineKind::Meta
            } else {
                LineKind::FileHeader
            };
            (kind, None, None, line)
        } else if let Some(t) = line.strip_prefix('+') {
            new += 1;
            diff.added += 1;
            (LineKind::Add, None, Some(new - 1), t)
        } else if let Some(t) = line.strip_prefix('-') {
            old += 1;
            diff.removed += 1;
            (LineKind::Del, Some(old - 1), None, t)
        } else if line.starts_with('\\') {
            (LineKind::Meta, None, None, line)
        } else {
            let t = line.strip_prefix(' ').unwrap_or(line);
            old += 1;
            new += 1;
            (LineKind::Context, Some(old - 1), Some(new - 1), t)
        };
        diff.lines.push(DiffLine {
            kind,
            old: o,
            new: n,
            text: text.replace('\t', TAB),
        });
    }
    diff
}

impl Diff {
    pub fn max_line_no(&self) -> u32 {
        self.lines
            .iter()
            .filter_map(|l| l.old.max(l.new))
            .max()
            .unwrap_or(0)
    }

    pub fn hunk_starts(&self) -> impl Iterator<Item = usize> + '_ {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind == LineKind::Hunk)
            .map(|(i, _)| i)
    }

    /// Line number to put after `path:` when copying the line at `idx`.
    /// Added/context lines use the new-file line. Deleted lines use the old line when
    /// `prefer_old`, otherwise the position the deletion would occupy in the new file.
    pub fn line_for_copy(&self, idx: usize, prefer_old: bool) -> Option<u32> {
        let line = self.lines.get(idx)?;
        match line.kind {
            LineKind::Add | LineKind::Context => line.new,
            LineKind::Del if prefer_old => line.old,
            LineKind::Del => {
                let in_hunk =
                    |l: &&DiffLine| !matches!(l.kind, LineKind::Hunk | LineKind::FileHeader);
                if let Some(n) = self.lines[idx..]
                    .iter()
                    .take_while(in_hunk)
                    .find_map(|l| l.new)
                {
                    return Some(n);
                }
                self.lines[..idx]
                    .iter()
                    .rev()
                    .take_while(in_hunk)
                    .find_map(|l| l.new)
                    .map(|n| n + 1)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n\
@@ -10,4 +10,5 @@ fn x\n ctx\n-old\n+new1\n+new2\n ctx2\n\\ No newline at end of file\n\
@@ -30 +31,0 @@\n-gone\n";

    #[test]
    fn numbers_lines() {
        let d = parse(SAMPLE);
        assert_eq!(d.added, 2);
        assert_eq!(d.removed, 2);
        let l = &d.lines;
        assert_eq!(l[0].kind, LineKind::FileHeader);
        assert_eq!(l[3].kind, LineKind::FileHeader);
        assert_eq!(l[4].kind, LineKind::Hunk);
        assert_eq!((l[5].old, l[5].new), (Some(10), Some(10)));
        assert_eq!((l[6].kind, l[6].old), (LineKind::Del, Some(11)));
        assert_eq!((l[7].kind, l[7].new), (LineKind::Add, Some(11)));
        assert_eq!(l[8].new, Some(12));
        assert_eq!((l[9].old, l[9].new), (Some(12), Some(13)));
        assert_eq!(l[10].kind, LineKind::Meta);
        assert_eq!(l[12].old, Some(30));
        assert_eq!(d.hunk_starts().collect::<Vec<_>>(), vec![4, 11]);
    }

    #[test]
    fn copy_line_numbers() {
        let d = parse(SAMPLE);
        assert_eq!(d.line_for_copy(5, false), Some(10));
        assert_eq!(d.line_for_copy(6, false), Some(11)); // deletion → next new line
        assert_eq!(d.line_for_copy(6, true), Some(11)); // old line
        assert_eq!(d.line_for_copy(4, false), None); // hunk header
        assert_eq!(d.line_for_copy(12, true), Some(30));
        assert_eq!(d.line_for_copy(12, false), None); // no new-side context at all
    }

    #[test]
    fn expands_tabs() {
        let d = parse("@@ -1 +1 @@\n+\tx\n");
        assert_eq!(d.lines[1].text, "    x");
    }
}
