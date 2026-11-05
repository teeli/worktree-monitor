//! File system watching (debounced) plus a periodic tick as a fallback for missed events.

use crate::app::AppEvent;
use anyhow::Context;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

const DEBOUNCE: Duration = Duration::from_millis(150);
pub const TICK: Duration = Duration::from_secs(2);

pub struct FsWatcher {
    _debouncer: Debouncer<RecommendedWatcher>,
}

/// Watches every existing path recursively; missing paths (e.g. a removed worktree directory)
/// are skipped. Fails if any existing path cannot be watched (e.g. the inotify limit), so the
/// caller falls back to polling instead of silently missing events.
pub fn watch(paths: &[PathBuf], tx: Sender<AppEvent>) -> anyhow::Result<FsWatcher> {
    let mut debouncer = new_debouncer(DEBOUNCE, move |res: DebounceEventResult| {
        if let Ok(events) = res {
            let paths = events.into_iter().map(|e| e.path).collect();
            let _ = tx.send(AppEvent::Fs(paths));
        }
    })?;
    for p in paths.iter().filter(|p| p.exists()) {
        debouncer
            .watcher()
            .watch(p, RecursiveMode::Recursive)
            .with_context(|| format!("watching {}", p.display()))?;
    }
    Ok(FsWatcher {
        _debouncer: debouncer,
    })
}

pub fn spawn_ticker(tx: Sender<AppEvent>) {
    thread::spawn(move || {
        loop {
            thread::sleep(TICK);
            if tx.send(AppEvent::Tick).is_err() {
                return;
            }
        }
    });
}
