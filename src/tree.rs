//! Builds a collapsible directory tree from a flat list of changed paths.

use crate::git::FileChange;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    /// Directory with its full path (used as the collapse key).
    Dir { path: String, expanded: bool },
    /// Index into the change list the tree was built from.
    File(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub depth: usize,
    pub name: String,
    /// Full path of the directory or file; identifies the row across rebuilds.
    pub path: String,
    pub kind: RowKind,
}

#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<(String, usize)>,
}

/// Paths of every directory row the tree can show (with all directories expanded).
pub fn all_dirs(changes: &[FileChange]) -> HashSet<String> {
    build(changes, &HashSet::new())
        .into_iter()
        .filter(|r| matches!(r.kind, RowKind::Dir { .. }))
        .map(|r| r.path)
        .collect()
}

pub fn build(changes: &[FileChange], collapsed: &HashSet<String>) -> Vec<Row> {
    let mut root = Dir::default();
    for (i, c) in changes.iter().enumerate() {
        let mut parts: Vec<&str> = c.path.split('/').filter(|p| !p.is_empty()).collect();
        let Some(file) = parts.pop() else { continue };
        let mut dir = &mut root;
        for p in parts {
            dir = dir.dirs.entry(p.to_string()).or_default();
        }
        dir.files.push((file.to_string(), i));
    }
    let mut rows = Vec::new();
    flatten(&root, "", 0, collapsed, &mut rows);
    rows
}

fn flatten(
    dir: &Dir,
    prefix: &str,
    depth: usize,
    collapsed: &HashSet<String>,
    rows: &mut Vec<Row>,
) {
    for (name, mut child) in &dir.dirs {
        // Compress chains of single-child directories into one row: a/b/c
        let mut name = name.clone();
        while child.files.is_empty() && child.dirs.len() == 1 {
            let (n, c) = child.dirs.iter().next().expect("len checked");
            name = format!("{name}/{n}");
            child = c;
        }
        let path = format!("{prefix}{name}");
        let expanded = !collapsed.contains(&path);
        rows.push(Row {
            depth,
            name,
            path: path.clone(),
            kind: RowKind::Dir {
                path: path.clone(),
                expanded,
            },
        });
        if expanded {
            flatten(child, &format!("{path}/"), depth + 1, collapsed, rows);
        }
    }
    let mut files = dir.files.clone();
    files.sort();
    for (name, idx) in files {
        rows.push(Row {
            depth,
            path: format!("{prefix}{name}"),
            name,
            kind: RowKind::File(idx),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileStatus;

    fn change(p: &str) -> FileChange {
        FileChange {
            path: p.into(),
            old_path: None,
            status: FileStatus::Modified,
        }
    }

    #[test]
    fn builds_compressed_tree() {
        let changes = vec![
            change("src/ui/a.rs"),
            change("README.md"),
            change("src/main.rs"),
            change("x/y/z/w.rs"),
        ];
        let rows = build(&changes, &HashSet::new());
        let names: Vec<_> = rows.iter().map(|r| (r.depth, r.name.as_str())).collect();
        assert_eq!(
            names,
            vec![
                (0, "src"),
                (1, "ui"),
                (2, "a.rs"),
                (1, "main.rs"),
                (0, "x/y/z"),
                (1, "w.rs"),
                (0, "README.md")
            ]
        );
        assert_eq!(rows[6].kind, RowKind::File(1));
        assert_eq!(rows[2].path, "src/ui/a.rs");
        assert_eq!(rows[4].path, "x/y/z");
        let dirs = all_dirs(&changes);
        assert_eq!(
            dirs,
            HashSet::from(["src".into(), "src/ui".into(), "x/y/z".into()])
        );
        assert_eq!(build(&changes, &dirs).len(), 3); // src, x/y/z, README.md
    }

    #[test]
    fn collapses() {
        let changes = vec![change("src/ui/a.rs"), change("src/main.rs")];
        let collapsed = HashSet::from(["src/ui".to_string()]);
        let rows = build(&changes, &collapsed);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[1].kind,
            RowKind::Dir {
                path: "src/ui".into(),
                expanded: false
            }
        );
    }
}
