//! Application state, event handling and actions.

use crate::clipboard::Clipboard;
use crate::diff::{self, Diff};
use crate::git::{self, Commit, FileChange, Repo, Worktree};
use crate::keymap::{self, Action, Lookup, Scope};
use crate::theme::Theme;
use crate::tree::{self, Row, RowKind};
use crate::watcher::{self, FsWatcher};
use crate::worker::{GitResult, Job};
use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{Event, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

const LOG_PAGE: usize = 50;
/// Every Nth tick also re-lists worktrees and refreshes inactive ones.
const FULL_REFRESH_TICKS: u64 = 3;
const TOAST_TTL: Duration = Duration::from_secs(3);
const WHEEL_LINES: usize = 3;
const HSCROLL_COLS: usize = 8;
const SCROLLOFF: usize = 2;

pub enum AppEvent {
    Input(Event),
    Git(GitResult),
    Fs(Vec<PathBuf>),
    Tick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Tree,
    Diff,
    Commits,
}

impl Pane {
    const ORDER: [Pane; 3] = [Pane::Tree, Pane::Diff, Pane::Commits];

    fn scope(self) -> Scope {
        match self {
            Pane::Tree => Scope::Tree,
            Pane::Diff => Scope::Diff,
            Pane::Commits => Scope::Commits,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Pane::Tree => "FILES",
            Pane::Diff => "DIFF",
            Pane::Commits => "COMMITS",
        }
    }
}

/// Everything shown for one worktree; kept per worktree so switching tabs restores it.
pub struct WtState {
    pub info: Worktree,
    /// Canonical path, for matching file system events.
    canon: PathBuf,
    pub changes: Vec<FileChange>,
    pub loaded: bool,
    pub error: Option<String>,

    pub commits: Vec<Commit>,
    pub log_exhausted: bool,
    log_loading: bool,
    /// Row in the commit list; 0 is the "working tree" pseudo entry.
    pub commit_cursor: usize,
    pub commit_offset: usize,
    /// Selected commit; `None` shows uncommitted work.
    pub commit: Option<Commit>,
    pub commit_files: Vec<FileChange>,
    commit_files_id: u64,
    /// Working tree file to restore when leaving commit mode.
    saved_file: Option<FileChange>,

    collapsed: HashSet<String>,
    pub rows: Vec<Row>,
    pub tree_cursor: usize,
    pub tree_offset: usize,

    /// File whose diff is shown.
    pub file: Option<FileChange>,
    pub diff: Diff,
    pub diff_raw: Option<String>,
    pub diff_error: Option<String>,
    diff_id: u64,
    pub diff_offset: usize,
    pub diff_hscroll: usize,
    pub diff_sel: Option<usize>,
}

impl WtState {
    fn new(info: Worktree) -> Self {
        let canon = std::fs::canonicalize(&info.path).unwrap_or_else(|_| info.path.clone());
        WtState {
            info,
            canon,
            changes: Vec::new(),
            loaded: false,
            error: None,
            commits: Vec::new(),
            log_exhausted: false,
            log_loading: false,
            commit_cursor: 0,
            commit_offset: 0,
            commit: None,
            commit_files: Vec::new(),
            commit_files_id: 0,
            saved_file: None,
            collapsed: HashSet::new(),
            rows: Vec::new(),
            tree_cursor: 0,
            tree_offset: 0,
            file: None,
            diff: Diff::default(),
            diff_raw: None,
            diff_error: None,
            diff_id: 0,
            diff_offset: 0,
            diff_hscroll: 0,
            diff_sel: None,
        }
    }

    pub fn files(&self) -> &[FileChange] {
        if self.commit.is_some() {
            &self.commit_files
        } else {
            &self.changes
        }
    }

    /// Rebuilds the tree rows, keeping the cursor on the same row if it still exists.
    /// Returns whether the cursor row was found again.
    fn rebuild_rows(&mut self) -> bool {
        let key = |r: &Row| (matches!(r.kind, RowKind::Dir { .. }), r.path.clone());
        let at = self.rows.get(self.tree_cursor).map(key);
        self.rows = tree::build(self.files(), &self.collapsed);
        match at.and_then(|k| self.rows.iter().position(|r| key(r) == k)) {
            Some(r) => {
                self.tree_cursor = r;
                true
            }
            None => false,
        }
    }

    fn row_file(&self, i: usize) -> Option<&FileChange> {
        match self.rows.get(i)?.kind {
            RowKind::File(idx) => self.files().get(idx),
            RowKind::Dir { .. } => None,
        }
    }

    fn clear_diff(&mut self) {
        self.diff = Diff::default();
        self.diff_raw = None;
        self.diff_error = None;
        self.diff_offset = 0;
        self.diff_hscroll = 0;
        self.diff_sel = None;
    }
}

/// Screen regions from the last render, used for mouse hit-testing and page sizes.
#[derive(Default)]
pub struct HitAreas {
    pub tabs: Vec<(Rect, usize)>,
    pub tree: Rect,
    pub diff: Rect,
    pub commits: Rect,
}

pub struct App {
    pub repo: Repo,
    pub theme: Theme,
    pub wts: Vec<WtState>,
    pub active: usize,
    pub focus: Pane,
    pub help: bool,
    pub help_scroll: usize,
    pub toast: Option<(String, bool, Instant)>,
    pub hit: HitAreas,
    pub pending_keys: Option<String>,
    quit: bool,
    jobs: Sender<Job>,
    events: Sender<AppEvent>,
    watcher: Option<FsWatcher>,
    common_canon: PathBuf,
    ticks: u64,
    next_id: u64,
    clipboard: Clipboard,
}

impl App {
    pub fn new(
        repo: Repo,
        theme: Theme,
        jobs: Sender<Job>,
        events: Sender<AppEvent>,
    ) -> Result<Self> {
        let infos = git::list_worktrees(&repo.toplevel)?;
        let top = std::fs::canonicalize(&repo.toplevel).unwrap_or_else(|_| repo.toplevel.clone());
        let wts: Vec<WtState> = infos.into_iter().map(WtState::new).collect();
        let active = wts.iter().position(|w| w.canon == top).unwrap_or(0);
        let common_canon =
            std::fs::canonicalize(&repo.common_dir).unwrap_or_else(|_| repo.common_dir.clone());
        let mut app = App {
            repo,
            theme,
            wts,
            active,
            focus: Pane::Tree,
            help: false,
            help_scroll: 0,
            toast: None,
            hit: HitAreas::default(),
            pending_keys: None,
            quit: false,
            jobs,
            events,
            watcher: None,
            common_canon,
            ticks: 0,
            next_id: 1,
            clipboard: Clipboard::default(),
        };
        app.rewatch();
        app.refresh_all();
        Ok(app)
    }

    pub fn active(&self) -> Option<&WtState> {
        self.wts.get(self.active)
    }

    fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn rewatch(&mut self) {
        let mut paths: Vec<PathBuf> = self.wts.iter().map(|w| w.info.path.clone()).collect();
        paths.push(self.repo.common_dir.clone());
        self.watcher = None;
        match watcher::watch(&paths, self.events.clone()) {
            Ok(w) => self.watcher = Some(w),
            Err(e) => self.notify(
                format!("File watching unavailable, polling only: {e}"),
                true,
            ),
        }
    }

    fn refresh_wt(&mut self, i: usize) {
        let Some(st) = self.wts.get_mut(i) else {
            return;
        };
        if !st.info.exists() {
            st.error = Some("worktree directory is missing".into());
            return;
        }
        let wt = st.info.path.clone();
        let count = st.commits.len().max(LOG_PAGE);
        self.send(Job::Status { wt: wt.clone() });
        if i == self.active {
            self.send(Job::Log { wt, skip: 0, count });
        }
    }

    fn refresh_all(&mut self) {
        self.send(Job::Worktrees);
        // Active first so it is served before the others.
        self.refresh_wt(self.active);
        let active = self.active;
        for i in (0..self.wts.len()).filter(|&i| i != active) {
            self.refresh_wt(i);
        }
    }

    fn notify(&mut self, msg: String, error: bool) {
        self.toast = Some((msg, error, Instant::now()));
    }

    pub fn visible_toast(&self) -> Option<(&str, bool)> {
        self.toast
            .as_ref()
            .filter(|(_, _, t)| t.elapsed() < TOAST_TTL)
            .map(|(m, e, _)| (m.as_str(), *e))
    }

    // ---- events -------------------------------------------------------------

    pub fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Input(Event::Key(k)) if k.kind != KeyEventKind::Release => self.on_key(k),
            AppEvent::Input(Event::Mouse(m)) => self.on_mouse(m),
            AppEvent::Input(_) => {}
            AppEvent::Git(res) => self.on_git(res),
            AppEvent::Fs(paths) => self.on_fs(paths),
            AppEvent::Tick => {
                self.ticks += 1;
                if self.ticks.is_multiple_of(FULL_REFRESH_TICKS) {
                    self.refresh_all();
                } else {
                    self.refresh_wt(self.active);
                }
            }
        }
    }

    fn on_fs(&mut self, paths: Vec<PathBuf>) {
        let mut dirty = HashSet::new();
        let mut everything = false;
        for p in paths {
            if let Ok(rel) = p.strip_prefix(&self.common_canon) {
                // Object writes and reflogs never change what we show by themselves.
                if !(rel.starts_with("objects") || rel.starts_with("logs")) {
                    everything = true;
                }
                continue;
            }
            let owner = self
                .wts
                .iter()
                .enumerate()
                .filter(|(_, w)| p.starts_with(&w.canon))
                .max_by_key(|(_, w)| w.canon.as_os_str().len())
                .map(|(i, _)| i);
            if let Some(i) = owner {
                dirty.insert(i);
            }
        }
        if everything {
            self.refresh_all();
        } else {
            for i in dirty {
                self.refresh_wt(i);
            }
        }
    }

    fn on_git(&mut self, res: GitResult) {
        match res {
            GitResult::Worktrees(Ok(infos)) => self.merge_worktrees(infos),
            GitResult::Worktrees(Err(e)) => self.notify(format!("{e:#}"), true),
            GitResult::Status { wt, res } => {
                let active = self.active;
                let Some(i) = self.index_of(&wt) else { return };
                let st = &mut self.wts[i];
                match res {
                    Ok(changes) => {
                        st.error = None;
                        st.loaded = true;
                        st.changes = changes;
                        if st.commit.is_none() {
                            let kept = st.rebuild_rows();
                            self.sync_selection(i, kept);
                            if i == active {
                                self.request_diff(i);
                            }
                        }
                    }
                    Err(e) => st.error = Some(format!("{e:#}")),
                }
            }
            GitResult::Log {
                wt,
                skip,
                count,
                res,
            } => {
                let Some(i) = self.index_of(&wt) else { return };
                let st = &mut self.wts[i];
                st.log_loading = false;
                match res {
                    Ok(commits) => {
                        st.log_exhausted = commits.len() < count;
                        if skip == 0 {
                            // Keep the cursor on the same commit across refreshes.
                            let at = st
                                .commit_cursor
                                .checked_sub(1)
                                .and_then(|c| st.commits.get(c))
                                .map(|c| c.sha.clone());
                            st.commits = commits;
                            if let Some(sha) = at {
                                st.commit_cursor = st
                                    .commits
                                    .iter()
                                    .position(|c| c.sha == sha)
                                    .map_or(0, |p| p + 1);
                            }
                        } else if skip == st.commits.len() {
                            st.commits.extend(commits);
                        }
                        st.commit_cursor = st.commit_cursor.min(st.commits.len());
                    }
                    Err(e) => self.notify(format!("{e:#}"), true),
                }
            }
            GitResult::CommitFiles { wt, id, res } => {
                let Some(i) = self.index_of(&wt) else { return };
                let st = &mut self.wts[i];
                if st.commit_files_id != id {
                    return;
                }
                match res {
                    Ok(files) => {
                        st.commit_files = files;
                        st.rebuild_rows();
                        self.sync_selection(i, false);
                        self.request_diff(i);
                    }
                    Err(e) => st.diff_error = Some(format!("{e:#}")),
                }
            }
            GitResult::Diff { wt, id, res } => {
                let Some(i) = self.index_of(&wt) else { return };
                let st = &mut self.wts[i];
                if st.diff_id != id {
                    return;
                }
                match res {
                    Ok(raw) => {
                        st.diff_error = None;
                        if st.diff_raw.as_deref() != Some(raw.as_str()) {
                            st.diff = diff::parse(&raw);
                            st.diff_raw = Some(raw);
                            let last = st.diff.lines.len().saturating_sub(1);
                            st.diff_sel = st.diff_sel.map(|s| s.min(last));
                            st.diff_offset = st.diff_offset.min(last);
                        }
                    }
                    Err(e) => st.diff_error = Some(format!("{e:#}")),
                }
            }
        }
    }

    fn index_of(&self, wt: &Path) -> Option<usize> {
        self.wts.iter().position(|w| w.info.path == wt)
    }

    fn merge_worktrees(&mut self, infos: Vec<Worktree>) {
        let same_set = infos.len() == self.wts.len()
            && infos
                .iter()
                .zip(&self.wts)
                .all(|(a, b)| a.path == b.info.path);
        if same_set {
            for (info, st) in infos.into_iter().zip(&mut self.wts) {
                st.info = info;
            }
            return;
        }
        let active_path = self.active().map(|w| w.info.path.clone());
        let mut old: Vec<WtState> = std::mem::take(&mut self.wts);
        let mut added = Vec::new();
        for info in infos {
            match old.iter().position(|w| w.info.path == info.path) {
                Some(p) => {
                    let mut st = old.swap_remove(p);
                    st.info = info;
                    self.wts.push(st);
                }
                None => {
                    added.push(self.wts.len());
                    self.wts.push(WtState::new(info));
                }
            }
        }
        self.active = active_path.and_then(|p| self.index_of(&p)).unwrap_or(0);
        self.rewatch();
        for &i in &added {
            self.refresh_wt(i);
        }
        if !added.contains(&self.active) {
            self.refresh_wt(self.active);
        }
    }

    // ---- selection ------------------------------------------------------------

    /// After the row list changed: keeps showing the current file if it still exists
    /// (moving the cursor to it unless `cursor_kept`), otherwise selects the nearest file.
    fn sync_selection(&mut self, i: usize, cursor_kept: bool) {
        let st = &mut self.wts[i];
        if let Some(path) = st.file.as_ref().map(|f| f.path.clone())
            && let Some(current) = st.files().iter().find(|f| f.path == path).cloned()
        {
            // Status letter or rename source may have changed.
            st.file = Some(current);
            // The file may be hidden inside a collapsed directory; then the cursor stays put.
            if !cursor_kept
                && let Some(r) =
                    (0..st.rows.len()).find(|&r| st.row_file(r).is_some_and(|f| f.path == path))
            {
                st.tree_cursor = r;
            }
            return;
        }
        if st.rows.is_empty() {
            st.tree_cursor = 0;
            st.file = None;
            st.clear_diff();
            return;
        }
        let cur = st.tree_cursor.min(st.rows.len() - 1);
        let target = (cur..st.rows.len())
            .chain((0..cur).rev())
            .find(|&r| st.row_file(r).is_some())
            .unwrap_or(cur);
        self.select_row(i, target);
    }

    fn select_row(&mut self, i: usize, row: usize) {
        let st = &mut self.wts[i];
        if st.rows.is_empty() {
            return;
        }
        st.tree_cursor = row.min(st.rows.len() - 1);
        let Some(file) = st.row_file(st.tree_cursor).cloned() else {
            return;
        };
        if st.file.as_ref().map(|f| &f.path) != Some(&file.path) {
            st.file = Some(file);
            st.clear_diff();
            self.request_diff(i);
        }
    }

    fn request_diff(&mut self, i: usize) {
        let id = self.id();
        let st = &mut self.wts[i];
        let Some(file) = st.file.clone() else { return };
        st.diff_id = id;
        let job = Job::Diff {
            wt: st.info.path.clone(),
            id,
            commit: st.commit.clone(),
            file,
        };
        self.send(job);
    }

    fn select_commit(&mut self, cursor: usize) {
        let id = self.id();
        let Some(st) = self.wts.get_mut(self.active) else {
            return;
        };
        st.commit_cursor = cursor.min(st.commits.len());
        let commit = st
            .commit_cursor
            .checked_sub(1)
            .and_then(|c| st.commits.get(c))
            .cloned();
        if commit.as_ref().map(|c| &c.sha) == st.commit.as_ref().map(|c| &c.sha) {
            return;
        }
        if st.commit.is_none() {
            st.saved_file = st.file.take();
        }
        st.file = None;
        st.clear_diff();
        st.tree_cursor = 0;
        st.tree_offset = 0;
        st.commit_files.clear();
        st.commit = commit.clone();
        match commit {
            Some(commit) => {
                st.commit_files_id = id;
                st.rebuild_rows();
                let wt = st.info.path.clone();
                self.send(Job::CommitFiles { wt, id, commit });
            }
            None => {
                st.file = st.saved_file.take();
                st.rebuild_rows();
                let i = self.active;
                self.sync_selection(i, false);
                self.request_diff(i);
            }
        }
    }

    fn switch_worktree(&mut self, i: usize) {
        if i < self.wts.len() && i != self.active {
            self.active = i;
            self.refresh_wt(i);
        }
    }

    // ---- input ------------------------------------------------------------------

    fn on_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        let Some(name) = keymap::key_name(&key) else {
            return;
        };
        if self.help {
            match name.as_str() {
                "j" | "Down" => self.help_scroll += 1,
                "k" | "Up" => self.help_scroll = self.help_scroll.saturating_sub(1),
                _ => self.help = false,
            }
            return;
        }
        let seq = match self.pending_keys.take() {
            Some(p) => format!("{p} {name}"),
            None => name,
        };
        match keymap::lookup(&seq, self.focus.scope()) {
            Lookup::Action(a) => self.act(a),
            Lookup::Pending => self.pending_keys = Some(seq),
            Lookup::None => {}
        }
    }

    fn act(&mut self, action: Action) {
        let n = self.wts.len();
        match action {
            Action::Quit => self.quit = true,
            Action::Help => {
                self.help = true;
                self.help_scroll = 0;
            }
            Action::FocusNext | Action::FocusPrev => {
                let pos = Pane::ORDER
                    .iter()
                    .position(|p| *p == self.focus)
                    .unwrap_or(0);
                let step = if action == Action::FocusNext {
                    1
                } else {
                    Pane::ORDER.len() - 1
                };
                self.focus = Pane::ORDER[(pos + step) % Pane::ORDER.len()];
            }
            Action::Worktree(i) => self.switch_worktree(i),
            Action::PrevWorktree if n > 0 => self.switch_worktree((self.active + n - 1) % n),
            Action::NextWorktree if n > 0 => self.switch_worktree((self.active + 1) % n),
            Action::NextFile => {
                self.step_file(true);
            }
            Action::PrevFile => {
                self.step_file(false);
            }
            Action::NextCommit => self.step_commit(true),
            Action::PrevCommit => self.step_commit(false),
            Action::CopyRelative => self.copy_path(false),
            Action::CopyAbsolute => self.copy_path(true),
            Action::Refresh => {
                self.refresh_all();
                self.notify("Refreshing…".into(), false);
            }
            Action::Down => self.move_by(1, true),
            Action::Up => self.move_by(-1, true),
            Action::HalfDown => self.move_by(self.page(2), false),
            Action::HalfUp => self.move_by(-self.page(2), false),
            Action::PageDown => self.move_by(self.page(1), false),
            Action::PageUp => self.move_by(-self.page(1), false),
            Action::Top => self.move_by(isize::MIN / 2, false),
            Action::Bottom => self.move_by(isize::MAX / 2, false),
            Action::Left | Action::Right => self.left_right(action == Action::Right),
            Action::Toggle => self.toggle(),
            Action::CollapseRecursive | Action::ExpandRecursive => {
                self.set_collapsed_recursive(action == Action::CollapseRecursive)
            }
            Action::NextHunk | Action::PrevHunk => self.jump_hunk(action == Action::NextHunk),
            Action::Escape => match self.focus {
                Pane::Diff => {
                    if let Some(st) = self.wts.get_mut(self.active) {
                        st.diff_sel = None;
                    }
                }
                Pane::Commits => self.select_commit(0),
                Pane::Tree => {}
            },
            Action::PrevWorktree | Action::NextWorktree => {}
        }
    }

    fn pane_height(&self, pane: Pane) -> usize {
        let r = match pane {
            Pane::Tree => self.hit.tree,
            Pane::Diff => self.hit.diff,
            Pane::Commits => self.hit.commits,
        };
        (r.height as usize).max(1)
    }

    fn page(&self, div: usize) -> isize {
        (self.pane_height(self.focus) / div).max(1) as isize
    }

    /// Moves the focused pane's cursor. In the diff, single steps start/move the line
    /// selection; larger jumps scroll and carry an existing selection along.
    fn move_by(&mut self, delta: isize, single: bool) {
        let height = self.pane_height(self.focus);
        let active = self.active;
        let Some(st) = self.wts.get_mut(active) else {
            return;
        };
        match self.focus {
            Pane::Tree => {
                let target = offset(st.tree_cursor, delta, st.rows.len());
                self.select_row(active, target);
                let st = &mut self.wts[active];
                ensure_visible(&mut st.tree_offset, st.tree_cursor, height);
            }
            Pane::Commits => {
                st.commit_cursor = offset(st.commit_cursor, delta, st.commits.len() + 1);
                ensure_visible(&mut st.commit_offset, st.commit_cursor, height);
                self.maybe_load_more();
            }
            Pane::Diff => {
                let len = st.diff.lines.len();
                match st.diff_sel {
                    None if single => {
                        st.diff_sel = Some(offset(
                            st.diff_offset,
                            if delta > 0 { 0 } else { delta },
                            len,
                        ))
                    }
                    None => {
                        st.diff_offset =
                            offset(st.diff_offset, delta, len.saturating_sub(height - 1))
                    }
                    Some(s) => st.diff_sel = Some(offset(s, delta, len)),
                }
                if let Some(s) = st.diff_sel {
                    ensure_visible(&mut st.diff_offset, s, height);
                }
            }
        }
    }

    fn maybe_load_more(&mut self) {
        let Some(st) = self.wts.get_mut(self.active) else {
            return;
        };
        let near_end = st.commit_cursor + 1 >= st.commits.len()
            || st.commit_offset + self.hit.commits.height as usize >= st.commits.len();
        if near_end && !st.log_exhausted && !st.log_loading && !st.commits.is_empty() {
            st.log_loading = true;
            let job = Job::Log {
                wt: st.info.path.clone(),
                skip: st.commits.len(),
                count: LOG_PAGE,
            };
            self.send(job);
        }
    }

    /// Shows the commit after/before the currently shown one (row 0 is the working tree).
    fn step_commit(&mut self, forward: bool) {
        let height = self.pane_height(Pane::Commits);
        let Some(st) = self.wts.get_mut(self.active) else {
            return;
        };
        let current = st
            .commit
            .as_ref()
            .and_then(|c| st.commits.iter().position(|x| x.sha == c.sha))
            .map_or(0, |p| p + 1);
        let target = offset(current, if forward { 1 } else { -1 }, st.commits.len() + 1);
        self.select_commit(target);
        let st = &mut self.wts[self.active];
        ensure_visible(&mut st.commit_offset, st.commit_cursor, height);
        self.maybe_load_more();
    }

    /// Selects the next/previous file in the tree. Returns whether one was found.
    fn step_file(&mut self, forward: bool) -> bool {
        let active = self.active;
        let Some(st) = self.wts.get(active) else {
            return false;
        };
        let cur = st.tree_cursor;
        let found = if forward {
            (cur + 1..st.rows.len()).find(|&r| st.row_file(r).is_some())
        } else {
            (0..cur).rev().find(|&r| st.row_file(r).is_some())
        };
        if let Some(r) = found {
            self.select_row(active, r);
            let h = self.pane_height(Pane::Tree);
            let st = &mut self.wts[active];
            ensure_visible(&mut st.tree_offset, st.tree_cursor, h);
        }
        found.is_some()
    }

    fn left_right(&mut self, right: bool) {
        let Some(st) = self.wts.get_mut(self.active) else {
            return;
        };
        match self.focus {
            Pane::Diff => {
                st.diff_hscroll = if right {
                    st.diff_hscroll + HSCROLL_COLS
                } else {
                    st.diff_hscroll.saturating_sub(HSCROLL_COLS)
                };
            }
            Pane::Tree => {
                let Some(row) = st.rows.get(st.tree_cursor).cloned() else {
                    return;
                };
                match (&row.kind, right) {
                    (
                        RowKind::Dir {
                            path,
                            expanded: true,
                        },
                        false,
                    ) => {
                        st.collapsed.insert(path.clone());
                        st.rebuild_rows();
                    }
                    (
                        RowKind::Dir {
                            path,
                            expanded: false,
                        },
                        true,
                    ) => {
                        st.collapsed.remove(path);
                        st.rebuild_rows();
                    }
                    (RowKind::Dir { expanded: true, .. }, true) => self.move_by(1, true),
                    (_, false) => {
                        if let Some(parent) = (0..st.tree_cursor)
                            .rev()
                            .find(|&r| st.rows[r].depth < row.depth)
                        {
                            st.tree_cursor = parent;
                        }
                    }
                    _ => {}
                }
            }
            Pane::Commits => {}
        }
    }

    /// Collapses or expands the directory at the cursor (or containing the file at the
    /// cursor) together with all of its subdirectories.
    fn set_collapsed_recursive(&mut self, collapse: bool) {
        let Some(st) = self.wts.get_mut(self.active) else {
            return;
        };
        let Some(row) = st.rows.get(st.tree_cursor) else {
            return;
        };
        let dir_row = match row.kind {
            RowKind::Dir { .. } => Some(st.tree_cursor),
            RowKind::File(_) => (0..st.tree_cursor)
                .rev()
                .find(|&r| st.rows[r].depth < row.depth),
        };
        let Some(dir_row) = dir_row else { return };
        let dir = st.rows[dir_row].path.clone();
        let prefix = format!("{dir}/");
        let affected = tree::all_dirs(st.files())
            .into_iter()
            .filter(|d| *d == dir || d.starts_with(&prefix));
        if collapse {
            st.collapsed.extend(affected);
            // The directory row stays visible; anything below it is now hidden.
            st.tree_cursor = dir_row;
        } else {
            for d in affected {
                st.collapsed.remove(&d);
            }
        }
        st.rebuild_rows();
    }

    fn toggle(&mut self) {
        let active = self.active;
        let Some(st) = self.wts.get_mut(active) else {
            return;
        };
        match self.focus {
            Pane::Tree => match st.rows.get(st.tree_cursor).map(|r| r.kind.clone()) {
                Some(RowKind::Dir { path, expanded }) => {
                    if expanded {
                        st.collapsed.insert(path);
                    } else {
                        st.collapsed.remove(&path);
                    }
                    st.rebuild_rows();
                }
                Some(RowKind::File(_)) => self.focus = Pane::Diff,
                None => {}
            },
            Pane::Commits => {
                let c = st.commit_cursor;
                self.select_commit(c);
            }
            Pane::Diff => {}
        }
    }

    fn jump_hunk(&mut self, forward: bool) {
        let Some(st) = self.wts.get_mut(self.active) else {
            return;
        };
        let cur = st.diff_sel.unwrap_or(st.diff_offset);
        let hunks: Vec<usize> = st.diff.hunk_starts().collect();
        let target = if forward {
            hunks.into_iter().find(|&h| h > cur)
        } else {
            hunks.into_iter().rev().find(|&h| h < cur.saturating_sub(1))
        };
        // Hunk jumps select the hunk's first line; the selection (unlike the scroll
        // offset, which rendering clamps) reliably tracks where we are.
        if let Some(h) = target {
            st.diff_offset = h;
            st.diff_sel = Some((h + 1).min(st.diff.lines.len().saturating_sub(1)));
            return;
        }
        // Past the last/first hunk: continue at the top of the next / bottom of the previous file.
        if self.step_file(forward) {
            let st = &mut self.wts[self.active];
            // Positions beyond the end are clamped once the new diff has loaded.
            let pos = if forward { 0 } else { usize::MAX };
            st.diff_offset = pos;
            st.diff_sel = Some(pos);
        }
    }

    fn copy_path(&mut self, absolute: bool) {
        let Some(st) = self.active() else { return };
        let Some(file) = &st.file else {
            self.notify("No file selected".into(), true);
            return;
        };
        let mut text = if absolute {
            st.info.path.join(&file.path).to_string_lossy().into_owned()
        } else {
            file.path.clone()
        };
        if let Some(line) = st
            .diff_sel
            .and_then(|i| st.diff.line_for_copy(i, st.commit.is_some()))
        {
            text = format!("{text}:{line}");
        }
        match self.clipboard.copy(&text) {
            Ok(()) => self.notify(format!("Copied {text}"), false),
            Err(e) => self.notify(format!("Copy failed: {e}"), true),
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        let pane_at = [
            (Pane::Tree, self.hit.tree),
            (Pane::Diff, self.hit.diff),
            (Pane::Commits, self.hit.commits),
        ]
        .into_iter()
        .find(|(_, r)| r.contains(pos));
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.help {
                    self.help = false;
                    return;
                }
                if let Some(&(_, i)) = self.hit.tabs.iter().find(|(r, _)| r.contains(pos)) {
                    self.switch_worktree(i);
                    return;
                }
                let Some((pane, rect)) = pane_at else { return };
                self.focus = pane;
                let rel = (m.row - rect.y) as usize;
                let active = self.active;
                let Some(st) = self.wts.get_mut(active) else {
                    return;
                };
                match pane {
                    Pane::Tree => {
                        let row = st.tree_offset + rel;
                        if row < st.rows.len() {
                            st.tree_cursor = row;
                            if matches!(st.rows[row].kind, RowKind::Dir { .. }) {
                                self.toggle();
                            } else {
                                self.select_row(active, row);
                            }
                        }
                    }
                    Pane::Diff => {
                        let line = st.diff_offset + rel;
                        if line < st.diff.lines.len() {
                            st.diff_sel = Some(line);
                        }
                    }
                    Pane::Commits => {
                        let row = st.commit_offset + rel;
                        if row <= st.commits.len() {
                            self.select_commit(row);
                            self.maybe_load_more();
                        }
                    }
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let down = m.kind == MouseEventKind::ScrollDown;
                if self.hit.tabs.iter().any(|(r, _)| r.y == m.row) {
                    self.act(if down {
                        Action::NextWorktree
                    } else {
                        Action::PrevWorktree
                    });
                    return;
                }
                let Some((pane, rect)) = pane_at else { return };
                let h = rect.height as usize;
                let delta = if down {
                    WHEEL_LINES as isize
                } else {
                    -(WHEEL_LINES as isize)
                };
                let Some(st) = self.wts.get_mut(self.active) else {
                    return;
                };
                match pane {
                    Pane::Tree => {
                        st.tree_offset =
                            offset(st.tree_offset, delta, st.rows.len().saturating_sub(h) + 1)
                    }
                    Pane::Diff => {
                        st.diff_offset = offset(
                            st.diff_offset,
                            delta,
                            st.diff.lines.len().saturating_sub(h) + 1,
                        )
                    }
                    Pane::Commits => {
                        st.commit_offset = offset(
                            st.commit_offset,
                            delta,
                            (st.commits.len() + 1).saturating_sub(h) + 1,
                        );
                        self.maybe_load_more();
                    }
                }
            }
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
                if matches!(pane_at, Some((Pane::Diff, _))) =>
            {
                let right = m.kind == MouseEventKind::ScrollRight;
                if let Some(st) = self.wts.get_mut(self.active) {
                    st.diff_hscroll = if right {
                        st.diff_hscroll + HSCROLL_COLS
                    } else {
                        st.diff_hscroll.saturating_sub(HSCROLL_COLS)
                    };
                }
            }
            _ => {}
        }
    }
}

/// `cur + delta`, clamped to `0..len`.
fn offset(cur: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    cur.saturating_add_signed(delta).min(len - 1)
}

/// Adjusts `offset` so `cursor` lies within the viewport, keeping a few lines of context.
pub fn ensure_visible(offset: &mut usize, cursor: usize, height: usize) {
    let so = SCROLLOFF.min(height.saturating_sub(1) / 2);
    if cursor < *offset + so {
        *offset = cursor.saturating_sub(so);
    } else if cursor + so >= *offset + height {
        *offset = cursor + so + 1 - height;
    }
}

pub fn run(terminal: &mut DefaultTerminal, mut app: App, rx: Receiver<AppEvent>) -> Result<()> {
    loop {
        terminal.draw(|f| crate::ui::draw(f, &mut app))?;
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(ev) => {
                app.handle(ev);
                for ev in rx.try_iter() {
                    app.handle(ev);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if app.quit {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_cursor_visible() {
        let mut off = 0;
        ensure_visible(&mut off, 9, 10);
        assert_eq!(off, 2);
        ensure_visible(&mut off, 3, 10);
        assert_eq!(off, 1);
        ensure_visible(&mut off, 0, 10);
        assert_eq!(off, 0);
    }

    #[test]
    fn clamps_offsets() {
        assert_eq!(offset(5, -10, 20), 0);
        assert_eq!(offset(5, 100, 20), 19);
        assert_eq!(offset(5, 1, 0), 0);
    }
}
