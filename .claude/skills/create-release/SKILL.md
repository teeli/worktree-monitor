---
name: create-release
description: Cut a new semver release of wtm end to end - pick the version, bump it via PR, tag main, and verify the GitHub Release. Use when asked to release, tag or publish a new version.
argument-hint: "[X.Y.Z | patch | minor | major]"
---

# Create a release

Releases are built by `.github/workflows/release.yml` when a `vX.Y.Z` tag is pushed. The workflow fails unless
the tag matches the `version` in `Cargo.toml` and points to a commit on `main`, so the version is bumped in a
normal PR first.

Every step marked **Confirm** modifies the remote: ask the user for explicit confirmation before running it.
Use `git --no-pager` for git commands that print output.

## 1. Preflight

```sh
git fetch --tags origin
gh auth status
gh run list --branch main --workflow ci.yml -L 1
```

Stop if `gh` is not authenticated or the latest CI run on `main` is not green.

## 2. Choose the version

- Last release: `git describe --tags --abbrev=0 origin/main` (no tags → this is the first release).
- Current version: `cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version'`.
- If the user passed `X.Y.Z`, use it. Otherwise list the changes with
  `git --no-pager log <last-tag>..origin/main --oneline` (all of `origin/main` for the first release) and
  propose a bump from the conventional commit types (or use the `patch|minor|major` argument):
  - `feat!`, `fix!` or `BREAKING CHANGE` → major (minor while the version is `0.x`)
  - `feat` → minor
  - anything else → patch
- Pre-releases use a semver suffix, e.g. `1.0.0-rc.1`; the workflow marks them as pre-releases.
- Stop if `git ls-remote --tags origin vX.Y.Z` returns anything.
- Ask the user to confirm the version.

If `Cargo.toml` on `origin/main` already has the chosen version (e.g. the first release of `0.1.0`), skip to
step 5.

## 3. Bump the version in a PR

```sh
git worktree add -b release/vX.Y.Z ../worktree-monitor-release-vX.Y.Z origin/main
cd ../worktree-monitor-release-vX.Y.Z
```

- Set `version = "X.Y.Z"` in `Cargo.toml`, then run `cargo check` to update `Cargo.lock`.
- `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
- Commit `Cargo.toml` and `Cargo.lock` as `chore(release): vX.Y.Z`. The screenshot does not need regenerating
  for a version bump.
- **Confirm**, then `git push -u origin release/vX.Y.Z` and
  `gh pr create --title "chore(release): vX.Y.Z" --body "Bump version to X.Y.Z."`.

## 4. Merge the PR

- `gh pr checks --watch` and wait for green.
- If the branch is behind `main`, rebase it onto `origin/main` (never merge `main` into it), re-run the checks
  and **confirm** before `git push --force-with-lease`.
- **Confirm**, then `gh pr merge`.

## 5. Tag main

```sh
git fetch origin
git --no-pager show origin/main:Cargo.toml | grep '^version'   # must be X.Y.Z
git tag -a vX.Y.Z -m vX.Y.Z origin/main
```

**Confirm**, then `git push origin vX.Y.Z`.

## 6. Watch the release workflow

```sh
gh run list --workflow release.yml -L 1     # find the run for the tag
gh run watch <run-id> --exit-status
```

## 7. Verify the release

- `gh release view vX.Y.Z` lists 4 `wtm-vX.Y.Z-<target>.tar.gz` files and their `.sha256` files
  (`aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`).
- Download the asset for this machine into the scratchpad directory, check it and run it:

  ```sh
  gh release download vX.Y.Z --pattern 'wtm-vX.Y.Z-<target>.tar.gz*' --dir <scratchpad>
  cd <scratchpad> && shasum -a 256 -c wtm-vX.Y.Z-<target>.tar.gz.sha256
  tar -xzf wtm-vX.Y.Z-<target>.tar.gz && ./wtm-vX.Y.Z-<target>/wtm --version   # prints: worktree-monitor X.Y.Z
  ```

## 8. Clean up

```sh
git worktree remove ../worktree-monitor-release-vX.Y.Z
git branch -D release/vX.Y.Z
```

## If the release fails

- Find the cause with `gh run view <run-id> --log-failed`.
- If no release was created: fix the problem in a new PR and merge it. **Confirm**, then move the tag to the new
  `main` commit:

  ```sh
  git push --delete origin vX.Y.Z && git tag -d vX.Y.Z
  git fetch origin && git tag -a vX.Y.Z -m vX.Y.Z origin/main && git push origin vX.Y.Z
  ```

  (If the fix changed nothing in `Cargo.toml`, the tag still matches the version.)
- If the release was published, never move its tag. Release the fix as a new patch version instead.
