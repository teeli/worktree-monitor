---
name: review-comments
description: Read the open wtm line comments of the current worktree and report or address them, closing handled ones. Use as part of every code review, or when asked to check or handle wtm comments.
---

# Review wtm comments

wtm comments are the user's line-by-line review feedback on the worktree's changes. Read and close them only
through the `wtm comments` CLI (`src/cli.rs`; README "Comments from the command line"), never by editing the
comment file.

Run it as `wtm comments …` if the installed `wtm` has that subcommand (`wtm comments --help` succeeds), else as
`cargo run -q -- comments …` from inside the worktree. It finds the worktree from any subdirectory.

## 1. List the open comments

```sh
wtm comments list --json
```

`[]`: report "no open wtm comments" and stop. Otherwise each element is a comment object of
`schema/comments.schema.json`, with `id` as a string (use it verbatim; never parse it as a number):

- `file`, `line`, `line_text`: the commented (last) line; `start` (`line`, `text`, `side`) is the first line of
  a range.
- `side`: `new`, or `old` for a line that exists only in the old file (a deleted line). `start.side` can differ.
- `commit`: the commit the line was viewed in; `null` means uncommitted changes.
- `body`: the feedback.

## 2. Find the code each comment refers to

Line numbers may have drifted. Find the lines by their text in the file as of `commit`
(`git show <commit>:<file>`), or in the working tree, then locate them in the current state of the branch.

## 3. Report or address

- **Review only** (e.g. `/code-review`, or reviewing without being asked to change code): report each open
  comment as a finding with your assessment. Change no code and close nothing.
- **Asked to address them:** treat each body as reviewer feedback: make the change, explain why no change is
  needed, or ask when it is ambiguous. A question gets an answer. Follow the repo rules in `CLAUDE.md` for any
  change (worktree, checks, screenshot).

## 4. Close and report

Close a comment once its outcome is final:

```sh
wtm comments close <ID>... --by claude
```

- `fixed`: only after the fix is kept (committed, or accepted by the user), not when the edit is made.
- `not changed (<reason>)` / `answered: <answer>`: after reporting it. The CLI stores no note, so the report is
  the only record of the reason.
- An open question: leave it open.

A wrong close is undone with `wtm comments reopen <ID>...`; the user editing a comment's text reopens it too.

Report one line per comment, before or alongside other review findings:

```
<file>:<line> — <comment summary> → fixed | not changed (<reason>) | answered: <answer> | question: <question>
```
