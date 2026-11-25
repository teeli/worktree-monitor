---
name: review-comments
description: Read the wtm line comments left on the current worktree and address the ones not yet handled. Use as part of every code review, or when asked to check or handle wtm comments.
---

# Review wtm comments

wtm stores line comments per worktree outside the repo, as JSON described by `schema/comments.schema.json`.
They are the user's review feedback on the worktree's changes. wtm owns the file: never edit or delete it.

wtm has no way to close a comment, so handled comments are tracked in a log of our own: `wtm-handled-comments`
in the worktree's git dir (`git rev-parse --git-dir`). That dir is private to the worktree, never committed, and
removed with it. Each line is `<id>\t<updated>\t<outcome>`. A comment counts as handled only while its `updated`
matches, so a comment edited in wtm comes back.

## 1. Find the files

The comment file path mirrors `state_dir()` and `file_name()` in `src/comments.rs` (path with `%` doubled and
`/` replaced by `%`):

```sh
wt=$(cd "$(git rev-parse --show-toplevel)" && pwd -P)
case "$XDG_STATE_HOME" in /*) base=$XDG_STATE_HOME ;; *) base=$HOME/.local/state ;; esac
f="$base/wtm/comments/$(printf '%s' "$wt" | sed 's/%/%%/g; s|/|%|g').json"
h="$(git rev-parse --git-dir)/wtm-handled-comments"
[ -f "$f" ] && echo "$f" || echo "no wtm comments"
```

No comment file: report "no wtm comments" and stop.

## 2. List the open comments

Lists comments whose `id` + `updated` is not in the log:

```sh
touch "$h"
jq -r --rawfile h "$h" '
  ($h | split("\n") | map(split("\t") | select(length > 1) | "\(.[0])\t\(.[1])")) as $done
  | .comments[] | select(("\(.id)\t\(.updated)") as $k | $done | index($k) | not)
  | "[\(.id)\t\(.updated)] \(.file):\(if .start then "\(.start.line)-" else "" end)\(.line) (\(.side), \(.commit // null | if . then .[0:8] else "working tree" end))\n  line: \(.line_text)\n  \(.body)\n"' "$f"
```

No output: report "no open wtm comments (N handled)" and stop. To revisit handled ones (e.g. the user asks),
`cat "$h"` and drop the `select`.

- `commit`: the commit the line was viewed in; `null` means the working tree.
- `side: old`: the line exists only in the old file (a deleted line).
- `start`: first line of a range comment; `line` is the last.

## 3. Find the code each comment refers to

Line numbers may have drifted. Find the line by `line_text` (and `start.text` for ranges) in the file as of
`commit` (`git show <commit>:<file>`), or in the working tree when `commit` is null, then locate it in the
current state of the branch.

## 4. Address each comment

Treat the body as reviewer feedback: make the change, explain why no change is needed, or ask when it is
ambiguous. A question gets an answer. Follow the repo rules in `CLAUDE.md` for any change (worktree, checks,
screenshot).

## 5. Record and report

Append every comment that got a final outcome (`fixed`, `not changed`, `answered`) to the log, copying the
`<id>\t<updated>` key printed in step 2. Leave comments with an open `question` out, so they come back next time.

```sh
printf '%s\t%s\t%s\n' <id> <updated> "<outcome>" >> "$h"
```

Report one line per comment, before or alongside other review findings:

```
<file>:<line> — <comment summary> → fixed | not changed (<reason>) | answered: <answer> | question: <question>
```
