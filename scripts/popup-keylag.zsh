#!/usr/bin/env zsh
# popup-keylag.zsh: how long each key takes while typing a line, with the zash completion popup
# on (its lookup runs on every key). A throwaway zsh in a pty, the real completion functions.
#
#   scripts/popup-keylag.zsh 'pacman -S firefox' [PLUGIN]
#   → keys 17  total 103 ms  worst 20 ms
# Fresh each run: XDG_CACHE_HOME is a temp folder, so commands remembered as slow start empty.
emulate -L zsh
zmodload zsh/zpty zsh/datetime
[[ $1 == (-h|--help|'') ]] && { sed -n '2,7p' $0 | sed 's/^# \{0,1\}//'; exit 0 }
line=$1
plugin=${2:-${0:A:h:h}/shell/zash-popup.plugin.zsh}
dir=$(mktemp -d); trap 'rm -rf $dir' EXIT
# a .zshrc of its own: with none, zsh's new-user wizard eats the keys
cat > $dir/.zshrc <<RC
autoload -Uz compinit; compinit -C -d $dir/.zcompdump
zstyle ':zash:popup' trigger typing
source $plugin
PS1='>'
RC
zpty -b z "XDG_CACHE_HOME=$dir/cache ZDOTDIR=$dir zsh -i"
sleep 3; zpty -r -t z out >/dev/null 2>&1
zpty -w -n z $'\n'; sleep 0.5; zpty -r -t z out >/dev/null 2>&1
integer i; float t0 ms tot=0 worst=0
for (( i = 1; i <= ${#line}; i++ )); do
  t0=$EPOCHREALTIME
  zpty -w -n z "${line[i]}"
  # the key is back once the shell echoed it (the popup's redraw runs before that)
  while ! zpty -r -t z out 2>/dev/null; do (( EPOCHREALTIME - t0 > 5 )) && break; done
  sleep 0.05; zpty -r -t z out 2>/dev/null
  ms=$(( (EPOCHREALTIME - t0 - 0.05) * 1000 ))
  (( tot += ms )); (( ms > worst )) && worst=$ms
done
printf 'keys %d  total %.0f ms  worst %.0f ms\n' ${#line} $tot $worst
zpty -d z
