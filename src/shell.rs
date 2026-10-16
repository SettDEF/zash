//! Shell integration: the shell tells zash where each prompt, command output
//! and command end is (OSC 133 marks), so zash can jump between commands and
//! mark the ones that failed.
//!
//! For zsh this needs no edit to the user's files: zash starts zsh with
//! ZDOTDIR pointing at a small .zshenv of its own, which puts the user's
//! ZDOTDIR back, sources their .zshenv, and adds two hooks. zsh then reads the
//! user's .zprofile/.zshrc as always. Other shells start unchanged.

use alacritty_terminal::tty::Shell;
use std::collections::HashMap;
use std::path::PathBuf;

pub const ZSHENV: &str = r#"# zash shell integration for zsh. Written by zash at start; edits are overwritten.
# It only restores your ZDOTDIR, sources your .zshenv and adds two hooks.
if [[ -n "$ZASH_ORIG_ZDOTDIR" ]]; then ZDOTDIR="$ZASH_ORIG_ZDOTDIR"; else unset ZDOTDIR; fi
unset ZASH_ORIG_ZDOTDIR
[[ -f "${ZDOTDIR:-$HOME}/.zshenv" ]] && source "${ZDOTDIR:-$HOME}/.zshenv"
if [[ -o interactive ]]; then
  autoload -Uz add-zsh-hook
  typeset -g _zash_ran=0
  # before each prompt: the last command's end (with its exit status), then a new prompt
  _zash_precmd() { local s=$?; (( _zash_ran )) && print -n "\e]133;D;$s\a"; _zash_ran=0; print -n "\e]133;A\a" }
  # a command starts: its output begins here
  _zash_preexec() { _zash_ran=1; print -n "\e]133;C\a" }
  add-zsh-hook precmd _zash_precmd
  add-zsh-hook preexec _zash_preexec
  # ── the completion popup: zsh's own completions, shown by zash while you type ──
  # Tab / Shift+Tab step through it (inserting each choice); with no popup open they
  # do what they did before. ZASH_NO_POPUP=1 turns it off.
  if [[ -z "$ZASH_NO_POPUP" ]]; then
    typeset -ga _zash_show _zash_ins
    typeset -gi _zash_sel=0 _zash_wstart=0 _zash_open=0
    typeset -g _zash_last='' _zash_tab_was='' _zash_btab_was=''
    _zash_send() { print -rn -- $'\e]7733;'"$1"$'\a' >$TTY }
    _zash_close() { (( _zash_open )) && _zash_send close; _zash_open=0 _zash_sel=0 }
    _zash_popup() { local IFS=$'\x1f'; _zash_send "popup;$_zash_sel;$(( CURSOR - _zash_wstart ));${_zash_show[*]}"; _zash_open=1 }
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
    # installed at the first prompt, after your .zshrc has made its own bindings
    _zash_popup_install() {
      add-zsh-hook -d precmd _zash_popup_install
      autoload -Uz add-zle-hook-widget
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
  fi
fi
"#;

fn user_shell() -> Option<String> {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty())
}

/// The shell to start for a new session, with integration where zash has it.
/// None: let the pty start the default shell unchanged.
pub fn integrate(env: &mut HashMap<String, String>) -> Option<Shell> {
    if std::env::var_os("ZASH_NO_SHELL_INTEGRATION").is_some() {
        return None;
    }
    let shell = user_shell()?;
    if !shell.ends_with("/zsh") && shell != "zsh" {
        return None;
    }
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
        .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".local/share"));
    let dir = data.join("zash/shell/zsh");
    if std::fs::create_dir_all(&dir).is_err() || std::fs::write(dir.join(".zshenv"), ZSHENV).is_err() {
        return None;
    }
    if let Ok(orig) = std::env::var("ZDOTDIR") {
        env.insert("ZASH_ORIG_ZDOTDIR".into(), orig);
    }
    env.insert("ZDOTDIR".into(), dir.display().to_string());
    Some(Shell::new(shell, Vec::new()))
}
