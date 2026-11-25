#!/bin/sh
# List the open wtm comments of the current worktree, or record one as handled.
#
#   wtm-comments.sh list
#   wtm-comments.sh done <id> <updated> <outcome>
#
# Paths are derived on every call, so it works from any directory inside the worktree.
set -eu

usage() {
    echo "usage: $0 list | done <id> <updated> <outcome>" >&2
    exit 2
}

command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }

# Mirrors state_dir() and file_name() in src/comments.rs.
wt=$(cd "$(git rev-parse --show-toplevel)" && pwd -P)
case "${XDG_STATE_HOME:-}" in
    /*) base=$XDG_STATE_HOME ;;
    *)
        [ -n "${HOME:-}" ] || { echo "no state directory (set XDG_STATE_HOME or HOME)" >&2; exit 1; }
        base=$HOME/.local/state
        ;;
esac
f="$base/wtm/comments/$(printf '%s' "$wt" | sed 's/%/%%/g; s|/|%|g').json"
h="$(git rev-parse --absolute-git-dir)/wtm-handled-comments"
handled=$(cat "$h" 2>/dev/null || true)

case "${1:-}" in
list)
    [ $# -eq 1 ] || usage
    [ -f "$f" ] || { echo "no wtm comments"; exit 0; }
    jq -r --arg h "$handled" '
        def lines:
            if .start == null then "\(.line) (\(.side)"
            elif .start.side == .side then "\(.start.line)-\(.line) (\(.side)"
            else "\(.start.line)-\(.line) (\(.start.side)-\(.side)" end;
        ($h | split("\n") | map(split("\t") | select(length > 2) | "\(.[0])\t\(.[1])")) as $done
        | [.comments[] | . + {key: "\(.id)\t\(.updated)"}] as $all
        | [$all[] | select(.key | IN($done[]) | not)] as $open
        | ($all | length) - ($open | length) as $n
        | if ($open | length) == 0 then "no open wtm comments (\($n) handled)"
          else
            ($open[]
              | "[\(.key)] \(.file):\(lines), \(if .commit then .commit[0:8] else "working tree" end))\n"
                + (if .start then "  first: \(.start.text)\n" else "" end)
                + "  line: \(.line_text)\n  \(.body)\n"),
            "\($open | length) open, \($n) handled"
          end' "$f"
    ;;
done)
    [ $# -eq 4 ] || usage
    case "$2$3" in *[!0-9]* | "") usage ;; esac
    [ -f "$f" ] || { echo "no wtm comments" >&2; exit 1; }
    key=$(printf '%s\t%s' "$2" "$3")
    jq -e --arg k "$key" 'any(.comments[]; "\(.id)\t\(.updated)" == $k)' "$f" >/dev/null ||
        { echo "no comment $2 with updated $3" >&2; exit 1; }
    printf '%s\t%s\n' "$key" "$(printf '%s' "$4" | tr '\t\n' '  ')" >>"$h"
    ;;
*) usage ;;
esac
