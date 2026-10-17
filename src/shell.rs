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

/// The completion popup plugin, written next to the .zshenv (and loadable from a .zshrc).
pub const POPUP_PLUGIN: &str = include_str!("../shell/zash-popup.plugin.zsh");

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
  # nvim in zash: zash is its screen (smooth scrolling, a flying cursor), as Neovide does.
  # Inside tmux, in pipes, or with ZASH_NO_NVIM_UI set it is plain nvim.
  if [[ -z "$ZASH_NO_NVIM_UI" ]]; then
    nvim() {
      if [[ -n "$TMUX" || ! -t 0 || ! -t 1 ]]; then command nvim "$@"; return; fi
      local sock="${XDG_RUNTIME_DIR:-/tmp}/zash-nvim-$$-$RANDOM.sock"
      print -n "\e]7733;nvim;$sock\a"
      command nvim --headless --listen "$sock" "$@"
    }
  fi
  # the completion popup (the same file a .zshrc can load; it never loads twice)
  source "${${(%):-%x}:A:h}/zash-popup.plugin.zsh"
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
    if std::fs::create_dir_all(&dir).is_err() || std::fs::write(dir.join(".zshenv"), ZSHENV).is_err() || std::fs::write(dir.join("zash-popup.plugin.zsh"), POPUP_PLUGIN).is_err() {
        return None;
    }
    if let Ok(orig) = std::env::var("ZDOTDIR") {
        env.insert("ZASH_ORIG_ZDOTDIR".into(), orig);
    }
    env.insert("ZDOTDIR".into(), dir.display().to_string());
    Some(Shell::new(shell, Vec::new()))
}
