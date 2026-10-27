#!/usr/bin/env zsh
# claude-history: every shell command Claude Code ran (all sessions), newest first, one per line,
# for the popup's second column. Reads the transcripts in ~/.claude/projects; only those changed
# since the last run, so it is cheap after the first one. Multi-line commands are left out.
#   claude-history [--rebuild]    writes ${XDG_CACHE_HOME:-~/.cache}/zash/claude-cmds
emulate -L zsh
setopt extendedglob nullglob
local cache=${XDG_CACHE_HOME:-$HOME/.cache}/zash/claude-cmds
local stamped=$cache.ts            # "ISO-time<TAB>command", every one seen
local lock=$cache.lock
command mkdir -p ${cache:h}
[[ $1 == --rebuild ]] && command rm -f $cache $stamped
# one run at a time (shells start together)
zmodload zsh/system 2>/dev/null
exec {fd}>>$lock
zsystem flock -t 0 -f fd $lock 2>/dev/null || exit 0
local -a files
if [[ -e $stamped ]]; then
  files=(~/.claude/projects/*/*.jsonl(.Ne:'[[ $REPLY -nt $stamped ]]':))
else
  files=(~/.claude/projects/*/*.jsonl(.N))
fi
(( $#files )) || exit 0
local tmp=$cache.new
{
  [[ -e $stamped ]] && cat $stamped
  for f in $files; do
    jq -r 'select(.type=="assistant") | .timestamp as $t | .message.content[]?
           | select(.type=="tool_use" and .name=="Bash") | .input.command // empty
           | select(test("\n") | not) | "\($t)\t\(.)"' $f 2>/dev/null
  done
} | sort -r -t $'\t' -k1,1 | awk -F'\t' '!seen[$2]++' | head -20000 >$tmp.ts
command mv -f $tmp.ts $stamped
cut -f2- $stamped >$tmp && command mv -f $tmp $cache
