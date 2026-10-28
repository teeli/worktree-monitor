# worktree-monitor

Rust + ratatui TUI (`wtm`) that monitors all git worktrees of a repository. See README.md for features and keys.

## Checks before committing

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
```

## Rules

- **Never commit on `main`.** Every change goes in a new git worktree on a new branch, e.g.
  `git worktree add -b feat/<name> ../worktree-monitor-<name> main`, and is merged from there.
- **Always keep the README screenshot up to date.** After any change to the code, regenerate it with
  `uv run scripts/screenshot.py` and commit the updated `docs/screenshot.png` together with the change. If the
  change adds UI worth showing, adjust the demo repo or key sequence in `scripts/screenshot.py` so it appears.
- Keep the README's key table in sync with `src/keymap.rs` (`BINDINGS` is the single source of truth for the
  `?` help popup).
