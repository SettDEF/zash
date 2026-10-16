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
#
# Load it from .zshrc (zplug: zplug "~/.local/share/zash/shell/zsh", from:local)
# or let zash load it on its own.

[[ -o interactive && -z "$ZASH_NO_POPUP" ]] || return 0
(( ${+_zash_popup_loaded} )) && return 0
typeset -g _zash_popup_loaded=1

zmodload zsh/zutil
autoload -Uz add-zsh-hook add-zle-hook-widget

typeset -ga _zash_show _zash_ins
typeset -gi _zash_sel=0 _zash_wstart=0 _zash_open=0
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
      (( ${+__o[-Q]} )) && ins=$h || ins=${(q)h}
      ins=$IPREFIX${__o[-P]}${__o[-p]}$ins
      if (( ${+__o[-f]} )); then
        # -W is the folder the names are in; without it the inserted prefix is the path
        if (( ${+__o[-W]} )); then d=${__o[-W]%/}/$h; else d=${__o[-p]}$h; fi
        [[ -d ${d/#\~/$HOME} ]] && { ins+=/; h+=/ }
      fi
      (( ${+_zash_seen[$ins]} )) && continue
      _zash_seen[$ins]=1
      _zash_ins+=("$ins") _zash_show+=("$h")
      (( $#_zash_ins >= 200 )) && break
    done
    return 0
  }
  _main_complete 2>/dev/null
  unfunction compadd
  compstate[insert]='' compstate[list]=''
  return 0
}

_zash_redraw() {
  [[ $BUFFER == "$_zash_last" ]] && return
  _zash_last=$BUFFER
  # only while typing a word at the end of the line, in insert mode
  if [[ $KEYMAP == vicmd || -n $RBUFFER || -z $LBUFFER || $LBUFFER == *[[:space:]] || $#BUFFER -gt 400 ]]; then _zash_close; return; fi
  zle _zash_cap
  if (( $#_zash_ins == 0 )) || { (( $#_zash_ins == 1 )) && [[ ${_zash_ins[1]} == "${LBUFFER[_zash_wstart+1,-1]}" ]] }; then _zash_close; return; fi
  _zash_sel=0
  _zash_popup
}

_zash_step() {
  local n=$#_zash_ins
  (( n )) || { _zash_close; return }
  (( _zash_sel = (_zash_sel - 1 + $1 + n) % n + 1 ))
  LBUFFER=${LBUFFER[1,_zash_wstart]}${_zash_ins[_zash_sel]}
  _zash_last=$BUFFER
  _zash_popup
}

_zash_tab() { if (( _zash_open )); then _zash_step 1; else zle "${_zash_tab_was:-expand-or-complete}"; fi }
_zash_btab() { if (( _zash_open )); then _zash_step -1; elif [[ -n $_zash_btab_was ]]; then zle "$_zash_btab_was"; fi }
_zash_finish() { _zash_close; _zash_last='' }

# installed at the first prompt, after .zshrc has made its own bindings (bindkey -v resets them)
_zash_popup_install() {
  add-zsh-hook -d precmd _zash_popup_install
  _zash_pick_style
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
