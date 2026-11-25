---
name: review-comments
description: Read the open wtm line comments of a worktree and report or address them, replying with the outcome and closing handled ones. Use as part of every code review, or when asked to check or handle wtm comments.
---

# Review wtm comments

wtm comments are the user's line-by-line review feedback on a worktree's changes, each with a thread of replies.
Read, reply to and close them only through the `wtm comments` CLI (`src/cli.rs`; README "Comments from the command
line"), never by editing the comment file.

## 1. Find the worktree

The CLI reads the comments of the worktree it runs in, so run every command below from the worktree of the
branch under review, not from wherever you happen to be:

- Reviewing the current checkout: use it.
- Reviewing a PR or branch: get the branch (`gh pr view <N> --json headRefName --jq .headRefName`) and its
  worktree:

  ```sh
  git worktree list --porcelain | awk -v b="refs/heads/<branch>" '/^worktree /{w=substr($0,10)} $0=="branch "b{print w}'
  ```

  No worktree: report "no worktree for <branch>, no wtm comments" and skip the rest.

## 2. List the open comments

In that worktree, run `wtm comments list --json`. If that fails (an installed `wtm` without the subcommand or
flag), run `cargo run -q -- comments list --json` there instead. Use whichever worked for `reply` and `close` too.

- Both fail (e.g. the branch doesn't build or predates the CLI): report "couldn't read wtm comments: <error>"
  and go on with the rest of the review. Never report that as "no comments".
- `[]`: report "no open wtm comments" and skip the rest.

Each element is a comment object as described in `schema/comments.schema.json`; read the field descriptions
there. Use `id` (a string) verbatim, never as a number.

Read each comment's whole thread: `body`, then `replies` (oldest first, absent when there are none; `by` is
`"user"` for the user, else an agent's name). A later user reply can answer an earlier agent question or change
what the comment asks for; the latest request wins. A comment whose last reply is your own open question, with
no user reply after it, is still waiting on the user: report it as such and don't act on it.

## 3. Find the code each comment refers to

Comments follow their text, not their line numbers, which drift with edits, rebases and renames:

1. Search the current files of the branch for `line_text` (and `start.text` for a range), starting with `file`.
2. Only if that fails, look at the revision the comment was written against: for `side: new`, `commit` (or the
   working tree when `commit` is null); for `side: old` (a deleted line; `start.side` can differ), the old side
   of that diff: `<commit>^`, or `head` when `commit` is null. These commits may be gone after a rebase.
3. Still not found: report the comment as not locatable, with its text, rather than guessing a line.

## 4. Report or address

- **Review only** (e.g. `/code-review`, or reviewing without being asked to change code): assess each open
  comment. Change no code and close nothing.
- **Asked to address them:** treat each thread as reviewer feedback: make the change, explain why no change is
  needed, or ask when it is ambiguous. A question gets an answer. Follow the repo rules in `CLAUDE.md` for any
  change (worktree, checks, screenshot).

## 5. Reply, close and report

Only when addressing comments (never in a review-only run), record each outcome in the comment's thread, from the
worktree of step 1, once it is final. Pass the text on stdin so it can span lines and start with `-`:

```sh
wtm comments reply <ID> --by claude - <<'EOF'
Fixed in <short sha>: <what changed>
EOF
wtm comments close <ID>... --by claude
```

Replies never open or close a comment, so close separately:

- `fixed`: reply "Fixed in <short sha>: <what changed>", then close — only after the fix is kept (committed, or
  accepted by the user), not when the edit is made.
- `not changed (<reason>)`: reply with the reason, then close.
- `answered: <answer>`: reply with the answer, then close.
- `question: <question>`: reply with the question and leave the comment open; the user answers in the thread.
- Review-only run: no replies and no closes.

Don't repeat a reply the thread already has from an earlier run. A wrong close is undone with
`wtm comments reopen <ID>...`; the user editing a comment's text reopens it too.

Report the comments in their own **wtm comments** section, separate from other review findings and not
counted against any findings limit, one line per comment:

```
<file>:<line> — <comment summary> → <assessment> | fixed | not changed (<reason>) | answered: <answer> | question: <question>
```
