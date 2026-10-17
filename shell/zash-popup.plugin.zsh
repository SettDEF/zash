# zash-popup — zsh's own completions in a popup while you type.
#
# Tab / Shift+Tab step through the popup, inserting each choice; with no popup
# open they do what they did before. Works in any terminal:
#   zash          a real popup at the cursor (also through tmux, with allow-passthrough)
#   elsewhere     a list under the prompt
#
# Settings (set before the first prompt):
#   ZASH_POPUP_STYLE=auto|zash|text   how to show it (auto: zash when inside zash)
#   ZASH_POPUP_MAX=10                 rows in the text list
#   ZASH_NO_POPUP=1                   off
#   ZASH_POPUP_LOG=/tmp/popup.log     log what the completion system offers (debugging)
#
# Or with zstyle (also before the first prompt):
#   zstyle ':zash:popup' min-chars 2        letters typed before it opens (default 1)
#   zstyle ':zash:popup' max-items 100      at most this many matches (default 200)
#   zstyle ':zash:popup' sort alpha         alpha (default), length, or none (zsh's order)
#   zstyle ':zash:popup' trigger typing     typing (default) or tab (opens only on Tab)
#   zstyle ':zash:popup' history-items 5    earlier commands that start with the line (0 = none)
#   zstyle ':zash:popup' opacity 0.85       the look, for zash (see [popup] in zash.toml):
#   zstyle ':zash:popup' radius 14          opacity radius blur shadow border rows
#   zstyle ':zash:popup' blur yes
#
# Load it from .zshrc (zplug: zplug "~/.local/share/zash/shell/zsh", from:local)
# or let zash load it on its own.

[[ -o interactive && -z "$ZASH_NO_POPUP" ]] || return 0
(( ${+_zash_popup_loaded} )) && return 0
typeset -g _zash_popup_loaded=1

zmodload zsh/zutil
autoload -Uz add-zsh-hook add-zle-hook-widget

typeset -ga _zash_show _zash_ins _zash_kind
typeset -gi _zash_sel=0 _zash_wstart=0 _zash_open=0 _zash_busy=0
typeset -g _zash_last='' _zash_tab_was='' _zash_btab_was='' _zash_style=''

# zash draws the popup when zash is the terminal; through tmux the message is wrapped
_zash_pick_style() {
  case ${ZASH_POPUP_STYLE:-auto} in
    zash|text) _zash_style=$ZASH_POPUP_STYLE ;;
    *) if [[ $TERM_PROGRAM == zash || -n $ZASH ]]; then _zash_style=zash; else _zash_style=text; fi ;;
  esac
}

_zash_send() {
  if [[ -n $TMUX ]]; then
    # tmux passthrough: ESC doubled inside DCS tmux; … ST
    print -rn -- $'\ePtmux;\e\e]7733;'"$1"$'\a\e\\' >$TTY
  else
    print -rn -- $'\e]7733;'"$1"$'\a' >$TTY
  fi
}

# the text list: one row per item under the prompt, the chosen one marked
_zash_text() {
  local max=${ZASH_POPUP_MAX:-10} n=$#_zash_show i first=1 out=''
  (( _zash_sel > max )) && first=$(( _zash_sel - max + 1 ))
  for (( i = first; i <= n && i < first + max; i++ )); do
    if (( i == _zash_sel )); then out+="▌ ${_zash_show[i]}"$'\n'; else out+="  ${_zash_show[i]}"$'\n'; fi
  done
  (( n > max )) && out+="  ${_zash_sel:-0}/$n · Tab next, Shift+Tab back"
  zle -M -- "${out%$'\n'}"
}

_zash_close() {
  if (( _zash_open )); then
    if [[ $_zash_style == zash ]]; then _zash_send close; else zle -M ''; fi
  fi
  _zash_open=0 _zash_sel=0
}

_zash_popup() {
  if [[ $_zash_style == zash ]]; then
    local IFS=$'\x1f'
    # the look from zstyle, once per shell (zash keeps it for this pane)
    if [[ -n $_zash_look && -z $_zash_look_sent ]]; then _zash_send "style;$_zash_look"; typeset -g _zash_look_sent=1; fi
    _zash_send "popup;$_zash_sel;$(( CURSOR - _zash_wstart ));${_zash_show[*]}"
  else
    _zash_text
  fi
  _zash_open=1
}

# a completion widget that changes nothing: it records what zsh would offer
_zash_capture() {
  _zash_show=() _zash_ins=()
  _zash_wstart=$(( CURSOR - ${#PREFIX} - ${#IPREFIX} ))
  local -A _zash_seen
  local -i _zash_pass=0
  compadd() {
    [[ -n $ZASH_POPUP_LOG ]] && print -r -- "compadd $* | PREFIX=$PREFIX IPREFIX=$IPREFIX" >>$ZASH_POPUP_LOG
    # internal calls that only ask for matches pass straight through
    if [[ ${@[1,(i)(-|--)]} == *-(O|A|D)\ * ]]; then builtin compadd "$@"; return; fi
    local -a __hits; local -A __o
    zparseopts -E -A __o P: p: S: s: W: f Q
    # -A only collects; its status says nothing was added, so it is not checked
    builtin compadd -A __hits "$@"
    (( $#__hits )) || return 0
    # like real completion: only the first matcher-list pass that finds anything
    (( _zash_pass )) || _zash_pass=${_matcher_num:-1}
    (( ${_matcher_num:-1} > _zash_pass )) && return 0
    local h ins d
    for h in $__hits; do
      # quoted like Tab would, but a leading $ or ~ stays live ($GAMEDIR/, ~/x)
      if (( ${+__o[-Q]} )); then ins=$h
      elif [[ $h == [\$~]* ]]; then ins=${h[1]}${(q)h[2,-1]}
      else ins=${(q)h}; fi
      ins=$IPREFIX${__o[-P]}${__o[-p]}$ins
      if (( ${+__o[-f]} )); then
        # -W is the folder the names are in; without it the inserted prefix is the path
        if (( ${+__o[-W]} )); then d=${__o[-W]%/}/$h; else d=${__o[-p]}$h; fi
        [[ -d ${d/#\~/$HOME} ]] && { ins+=/; h+=/ }
      fi
      (( ${+_zash_seen[$ins]} )) && continue
      _zash_seen[$ins]=1
      _zash_ins+=("$ins") _zash_show+=("$h")
      (( $#_zash_ins >= ${_zash_max:-200} )) && break
    done
    return 0
  }
  _main_complete 2>/dev/null
  unfunction compadd
  _zash_sort
  _zash_kind=(${_zash_ins/*/c})
  compstate[insert]='' compstate[list]=''
  return 0
}

_zash_redraw() {
  # never inside another completion (Tab's own menu, a listing): zsh cannot nest them, and
  # trying once crashed the shell. Nor while our own capture runs.
  (( _zash_busy )) && return
  # (inside this hook $WIDGET is the hook itself; the widget that just ran is $LASTWIDGET)
  case $LASTWIDGET in
    *complete*|*expand*|menu-*|*-menu*|list-choices) return ;;
  esac
  [[ $KEYMAP == menuselect || $KEYMAP == listscroll ]] && return
  [[ $BUFFER == "$_zash_last" ]] && return
  _zash_last=$BUFFER
  # typing only opens it with trigger=typing, and after min-chars letters of the word
  if [[ $_zash_trigger == tab && $_zash_open == 0 ]]; then return; fi
  local word=${LBUFFER##*[[:space:]]}
  if (( ${#word} < ${_zash_min:-1} )) && (( ! _zash_open )); then return; fi
  # only while typing a word at the end of the line, in insert mode
  if [[ $KEYMAP == vicmd || -n $RBUFFER || -z $LBUFFER || $LBUFFER == *[[:space:]] || $#BUFFER -gt 400 ]]; then _zash_close; return; fi
  _zash_busy=1
  zle _zash_cap 2>/dev/null
  _zash_busy=0
  _zash_history
  if (( $#_zash_ins == 0 )) || { (( $#_zash_ins == 1 )) && [[ ${_zash_ins[1]} == "${LBUFFER[_zash_wstart+1,-1]}" ]] }; then _zash_close; return; fi
  _zash_sel=0
  _zash_popup
}

_zash_step() {
  local n=$#_zash_ins
  (( n )) || { _zash_close; return }
  (( _zash_sel = (_zash_sel - 1 + $1 + n) % n + 1 ))
  if [[ ${_zash_kind[_zash_sel]} == h ]]; then
    # an earlier command: the whole line
    BUFFER=${_zash_ins[_zash_sel]}
    CURSOR=$#BUFFER
  else
    LBUFFER=${LBUFFER[1,_zash_wstart]}${_zash_ins[_zash_sel]}
  fi
  _zash_last=$BUFFER
  _zash_popup
}

_zash_tab() { if (( _zash_open )); then _zash_step 1; else
  if [[ $_zash_trigger == tab ]]; then
    _zash_busy=1
    zle _zash_cap 2>/dev/null
    _zash_busy=0
    _zash_history
    if (( $#_zash_ins > 1 )); then _zash_sel=0; _zash_popup; _zash_step 1; return; fi
  fi
  zle "${_zash_tab_was:-expand-or-complete}"; fi }
_zash_btab() { if (( _zash_open )); then _zash_step -1; elif [[ -n $_zash_btab_was ]]; then zle "$_zash_btab_was"; fi }
_zash_finish() { _zash_close; _zash_last='' }

# installed at the first prompt, after .zshrc has made its own bindings (bindkey -v resets them)
_zash_popup_install() {
  add-zsh-hook -d precmd _zash_popup_install
  _zash_pick_style
  _zash_read_styles
  zle -C _zash_cap complete-word _zash_capture
  zle -N _zash_tab; zle -N _zash_btab; zle -N _zash_redraw; zle -N _zash_finish
  local w
  w=${${(z)$(bindkey -M viins '^I')}[2]}; [[ $w != _zash_tab && $w != undefined-key ]] && _zash_tab_was=$w
  w=${${(z)$(bindkey -M viins '^[[Z')}[2]}; [[ $w != _zash_btab && $w != undefined-key ]] && _zash_btab_was=$w
  local m; for m in viins emacs; do bindkey -M $m '^I' _zash_tab; bindkey -M $m '^[[Z' _zash_btab; done
  add-zle-hook-widget line-pre-redraw _zash_redraw
  add-zle-hook-widget line-finish _zash_finish
}
add-zsh-hook precmd _zash_popup_install

# matches in a useful order: those starting with what was typed first, then by the chosen sort
_zash_sort() {
  local how=${_zash_sortby:-alpha}
  [[ $how == none || $#_zash_ins -lt 2 ]] && return
  local typed=${(L)PREFIX} i
  local -a keyed
  for (( i = 1; i <= $#_zash_ins; i++ )); do
    local s=${_zash_show[i]} first=1
    [[ ${(L)s} == ${typed}* ]] && first=0
    if [[ $how == length ]]; then
      keyed+=("$first$(printf '%04d' ${#s})${(L)s}"$'\x1f'"$i")
    else
      keyed+=("$first${(L)s}"$'\x1f'"$i")
    fi
  done
  local -a ins show
  for k in ${(o)keyed}; do
    i=${k##*$'\x1f'}
    ins+=("${_zash_ins[i]}") show+=("${_zash_show[i]}")
  done
  _zash_ins=("${ins[@]}") _zash_show=("${show[@]}")
}

# the zstyles, read once at the first prompt; the look goes to zash with the first popup
_zash_read_styles() {
  local v
  zstyle -s ':zash:popup' min-chars v && typeset -g _zash_min=$v
  zstyle -s ':zash:popup' max-items v && typeset -g _zash_max=$v
  zstyle -s ':zash:popup' sort v && typeset -g _zash_sortby=$v
  zstyle -s ':zash:popup' trigger v && typeset -g _zash_trigger=$v
  zstyle -s ':zash:popup' history-items v && typeset -g _zash_hist_n=$v
  local -a look
  local k
  for k in opacity radius blur shadow border rows; do
    zstyle -s ':zash:popup' $k v && look+=("$k=$v")
  done
  typeset -g _zash_look=${(j:,:)look}
}

# earlier commands that start with the line typed so far, newest first, after the completions
zmodload -F zsh/parameter p:history 2>/dev/null
_zash_history() {
  local n=${_zash_hist_n:-5}
  (( n > 0 && ${#LBUFFER} >= 2 )) || return
  local -A seen
  local h k found=0
  local line=${(b)LBUFFER}
  # every entry that starts with the line (zsh does the matching), newest first
  local -a keys=(${(k)history[(R)${~line}*]})
  for k in ${(On)keys}; do
    h=${history[$k]}
    [[ $h != "$BUFFER" && $h != *$'\n'* ]] || continue
    (( ${+seen[$h]} )) && continue
    seen[$h]=1
    _zash_ins+=("$h") _zash_show+=("↺ $h") _zash_kind+=(h)
    (( ++found >= n )) && break
  done
}
