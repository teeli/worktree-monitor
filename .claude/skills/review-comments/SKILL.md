---
name: review-comments
description: Read the wtm line comments left on the current worktree and report or address the ones not yet handled. Use as part of every code review, or when asked to check or handle wtm comments.
---

# Review wtm comments

wtm stores line comments per worktree outside the repo, as JSON described by `schema/comments.schema.json`.
They are the user's review feedback on the worktree's changes. wtm owns the file: never edit or delete it.

wtm has no way to close a comment, so handled comments are tracked in a log of our own: `wtm-handled-comments`
in the worktree's git dir. It is never committed; for a linked worktree it is removed with the worktree, for the
main checkout it stays in the repo's `.git/`. Each line is `<id>\t<updated>\t<outcome>`. A comment counts as
handled only while its `updated` matches, so a comment edited in wtm comes back.

`wtm-comments.sh` (next to this file, needs `jq`) finds both files on every call, from any directory inside the
worktree. Its path logic mirrors `state_dir()` and `file_name()` in `src/comments.rs`.

## 1. List the open comments

```sh
.claude/skills/review-comments/wtm-comments.sh list
```

"no wtm comments" or "no open wtm comments (N handled)": report that and stop. Otherwise each comment prints as:

```
[<id>	<updated>] <file>:<line or start-end> (<side>, <commit or "working tree">)
  first: <first line of a range>
  line: <last (or only) line>
  <body>
```

- `side`: `new`, or `old` for a line that exists only in the old file (a deleted line). A range spanning both
  prints `old-new`: the first line is on the old side, the last on the new side.
- `commit`: the commit the line was viewed in; "working tree" means uncommitted changes.

## 2. Find the code each comment refers to

Line numbers may have drifted. Find the lines by their text in the file as of the commit
(`git show <commit>:<file>`), or in the working tree, then locate them in the current state of the branch.

## 3. Report or address

- **Review only** (e.g. `/code-review`, or reviewing without being asked to change code): report each open
  comment as a finding with your assessment. Change no code and record nothing.
- **Asked to address them:** treat each body as reviewer feedback: make the change, explain why no change is
  needed, or ask when it is ambiguous. A question gets an answer. Follow the repo rules in `CLAUDE.md` for any
  change (worktree, checks, screenshot).

## 4. Record and report

Record a comment once its outcome is final, copying the id and updated printed in step 1:

```sh
.claude/skills/review-comments/wtm-comments.sh done <id> <updated> "<outcome>"
```

- `fixed`: only after the fix is kept (committed, or accepted by the user), not when the edit is made.
- `not changed (<reason>)` / `answered: <answer>`: after reporting it.
- An open question: don't record it, so it comes back next time.

Report one line per comment, before or alongside other review findings:

```
<file>:<line> — <comment summary> → fixed | not changed (<reason>) | answered: <answer> | question: <question>
```
