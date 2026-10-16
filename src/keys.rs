//! Pane and tab keys, read from a tmux config: zash does what the user's tmux
#![allow(dead_code)] // wired into the window with the split tree (next)
//! would do for the same keys (the prefix, `bind-key`, `-n`, `-r`), so moving
//! from tmux to zash's own splits needs no new habits. Commands zash has no
//! equivalent for are skipped.
//!
//! Without a tmux config the tmux defaults apply (prefix Ctrl+B, % and " split).

use std::collections::HashMap;

/// What a key does.
#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum Action {
    /// side by side (`split-window -h`) or one above the other (`-v`)
    Split { side_by_side: bool },
    Focus(Dir),
    FocusNext,
    /// grow/shrink the focused pane's edge in a direction, by cells
    Resize(Dir, u16),
    Zoom,
    ClosePane,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    LastTab,
    SendPrefix,
    Search,
    Paste,
    /// several, in order (`select-pane -t :.+ \; resize-pane -Z`)
    Seq(Vec<Action>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// A key as tmux writes it: modifiers and a name (`C-Space`, `M-j`, `\`, `K`, `Escape`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub ctrl: bool,
    pub alt: bool,
    /// a character (as typed, so `K` and `|` already carry the shift) or a key name
    pub key: String,
}

impl Chord {
    pub fn parse(s: &str) -> Option<Chord> {
        let mut c = Chord { ctrl: false, alt: false, key: String::new() };
        let mut rest = s;
        loop {
            if let Some(r) = rest.strip_prefix("C-").filter(|r| !r.is_empty()) {
                c.ctrl = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("M-").filter(|r| !r.is_empty()) {
                c.alt = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix('^').filter(|r| !r.is_empty()) {
                c.ctrl = true;
                rest = r;
            } else {
                break;
            }
        }
        let rest = match rest {
            "\\\\" => "\\",
            r => r,
        };
        c.key = match rest.to_lowercase().as_str() {
            "space" => "Space".into(),
            "escape" | "esc" => "Escape".into(),
            "enter" => "Enter".into(),
            "tab" => "Tab".into(),
            "bspace" => "Backspace".into(),
            "up" => "Up".into(),
            "down" => "Down".into(),
            "left" => "Left".into(),
            "right" => "Right".into(),
            "pageup" | "ppage" => "PageUp".into(),
            "pagedown" | "npage" => "PageDown".into(),
            _ if rest.chars().count() == 1 => {
                // with Ctrl, letters have no case (C-c and C-C are one key)
                if c.ctrl { rest.to_lowercase() } else { rest.to_string() }
            }
            _ => return None,
        };
        Some(c)
    }
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub action: Action,
    /// `-r`: may repeat without the prefix for a moment
    pub repeat: bool,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Keys {
    pub prefix: Chord,
    /// after the prefix
    pub table: HashMap<Chord, Binding>,
    /// `-n`: no prefix needed
    pub root: HashMap<Chord, Binding>,
}

/// One tmux command line → an action zash can do.
fn action(cmd: &str) -> Option<Action> {
    let parts: Vec<Action> = cmd.split("\\;").filter_map(|c| one(c.trim())).collect();
    match parts.len() {
        0 => None,
        1 => parts.into_iter().next(),
        _ => Some(Action::Seq(parts)),
    }
}

fn one(cmd: &str) -> Option<Action> {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    let (name, args) = words.split_first()?;
    let has = |f: &str| args.iter().any(|a| *a == f || (a.starts_with('-') && !a.starts_with("--") && a.len() > 2 && a[1..].contains(&f[1..])));
    let target = args.iter().position(|a| *a == "-t").and_then(|i| args.get(i + 1)).copied();
    Some(match *name {
        "split-window" | "splitw" => Action::Split { side_by_side: has("-h") },
        "select-pane" | "selectp" => match target {
            Some(t) if t.ends_with('+') => Action::FocusNext,
            _ if has("-L") => Action::Focus(Dir::Left),
            _ if has("-R") => Action::Focus(Dir::Right),
            _ if has("-U") => Action::Focus(Dir::Up),
            _ if has("-D") => Action::Focus(Dir::Down),
            _ => return None,
        },
        "resize-pane" | "resizep" => {
            if has("-Z") {
                Action::Zoom
            } else {
                let n = args.iter().filter_map(|a| a.parse::<u16>().ok()).next().unwrap_or(1);
                let d = if has("-L") { Dir::Left } else if has("-R") { Dir::Right } else if has("-U") { Dir::Up } else if has("-D") { Dir::Down } else { return None };
                Action::Resize(d, n)
            }
        }
        "kill-pane" | "killp" => Action::ClosePane,
        "new-window" | "neww" => Action::NewTab,
        "kill-window" | "killw" => Action::CloseTab,
        "next-window" | "next" => Action::NextTab,
        "previous-window" | "prev" => Action::PrevTab,
        "last-window" | "last" => Action::LastTab,
        "select-window" | "selectw" => match target {
            Some(t) if t.ends_with('+') => Action::NextTab,
            Some(t) if t.ends_with('-') => Action::PrevTab,
            _ => return None,
        },
        "send-prefix" => Action::SendPrefix,
        "copy-mode" => Action::Search,
        "paste-buffer" | "pasteb" => Action::Paste,
        _ => return None,
    })
}

impl Keys {
    /// tmux's own defaults (prefix Ctrl+B).
    pub fn defaults() -> Keys {
        let mut k = Keys { prefix: Chord::parse("C-b").unwrap(), table: HashMap::new(), root: HashMap::new() };
        for line in [
            "bind % split-window -h",
            "bind '\"' split-window -v",
            "bind o select-pane -t :.+",
            "bind Left select-pane -L",
            "bind Right select-pane -R",
            "bind Up select-pane -U",
            "bind Down select-pane -D",
            "bind -r M-Left resize-pane -L 5",
            "bind -r M-Right resize-pane -R 5",
            "bind -r M-Up resize-pane -U 5",
            "bind -r M-Down resize-pane -D 5",
            "bind z resize-pane -Z",
            "bind x kill-pane",
            "bind c new-window",
            "bind & kill-window",
            "bind n next-window",
            "bind p previous-window",
            "bind l last-window",
            "bind [ copy-mode",
            "bind ] paste-buffer",
        ] {
            k.line(line);
        }
        k
    }

    /// The defaults, then the user's tmux config on top (as tmux reads it).
    pub fn from_tmux_conf(text: &str) -> Keys {
        let mut k = Keys::defaults();
        for line in text.lines() {
            k.line(line);
        }
        k
    }

    /// The user's tmux config, if there is one.
    pub fn load() -> Keys {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
        let xdg = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| home.join(".config"));
        for p in [xdg.join("tmux/tmux.conf"), home.join(".tmux.conf")] {
            if let Ok(t) = std::fs::read_to_string(&p) {
                return Keys::from_tmux_conf(&t);
            }
        }
        Keys::defaults()
    }

    fn line(&mut self, line: &str) {
        // a comment ends the line (tmux: an unquoted #)
        let line = match line.find(" #").or_else(|| line.find("\t#")) {
            Some(i) => &line[..i],
            None => line,
        };
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            return;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        match words[0] {
            "set" | "set-option" => {
                let rest: Vec<&str> = words[1..].iter().filter(|w| !w.starts_with('-')).copied().collect();
                if rest.first() == Some(&"prefix") {
                    if let Some(c) = rest.get(1).and_then(|k| Chord::parse(unquote(k))) {
                        self.prefix = c;
                    }
                }
            }
            "unbind" | "unbind-key" => {
                let no_prefix = words.iter().any(|w| *w == "-n");
                if let Some(c) = words[1..].iter().find(|w| !w.starts_with('-')).and_then(|k| Chord::parse(unquote(k))) {
                    if no_prefix { self.root.remove(&c) } else { self.table.remove(&c) };
                }
            }
            "bind" | "bind-key" => {
                let (mut no_prefix, mut repeat, mut i) = (false, false, 1);
                while let Some(w) = words.get(i) {
                    match *w {
                        "-n" => no_prefix = true,
                        "-r" => repeat = true,
                        "-T" => {
                            // another key table: only "root" (= -n) is followed
                            if words.get(i + 1) == Some(&"root") { no_prefix = true } else { return }
                            i += 1;
                        }
                        w if w.starts_with('-') && w.len() > 1 && w != "-" => {
                            no_prefix |= w.contains('n');
                            repeat |= w.contains('r');
                        }
                        _ => break,
                    }
                    i += 1;
                }
                let Some(key) = words.get(i).and_then(|k| Chord::parse(unquote(k))) else { return };
                let cmd = words[i + 1..].join(" ");
                let table = if no_prefix { &mut self.root } else { &mut self.table };
                match action(&cmd) {
                    Some(a) => {
                        table.insert(key, Binding { action: a, repeat });
                    }
                    // rebound to something zash does not do: the old meaning is gone too
                    None => {
                        table.remove(&key);
                    }
                }
            }
            _ => {}
        }
    }
}

fn unquote(s: &str) -> &str {
    let s = s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(s);
    s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONF: &str = r##"
unbind-key C-Space
set-option -g prefix C-Space
bind-key C-Space last-window
bind-key a send-prefix
bind-key k select-pane -U
bind-key h select-pane -L
bind-key -n M-o select-pane -t :.+ \; resize-pane -Z	# next pane in zoomed mode
bind-key -n M-j select-pane -t :.+	# next pane
bind-key -n M-h select-window -t :-	# previous window
bind-key -r J resize-pane -D 1
bind-key \\ split-window -h -c "#{pane_current_path}"	# vertical split
bind-key - split-window -v -c "#{pane_current_path}"
bind-key c new-window -c "#{pane_current_path}"
bind-key x kill-pane
bind-key e new-window -n 'conf' 'vim ~/.config/tmux/tmux.conf'
bind-key r source-file ~/.config/tmux/tmux.conf \; display-message "reloaded"
"##;

    fn c(s: &str) -> Chord {
        Chord::parse(s).unwrap()
    }

    #[test]
    fn reads_the_users_tmux_bindings() {
        let k = Keys::from_tmux_conf(CONF);
        assert_eq!(k.prefix, c("C-Space"));
        assert_eq!(k.table[&c("\\")].action, Action::Split { side_by_side: true });
        assert_eq!(k.table[&c("-")].action, Action::Split { side_by_side: false });
        assert_eq!(k.table[&c("h")].action, Action::Focus(Dir::Left));
        assert_eq!(k.table[&c("C-Space")].action, Action::LastTab);
        assert_eq!(k.table[&c("a")].action, Action::SendPrefix);
        assert!(k.table[&c("J")].repeat);
        assert_eq!(k.table[&c("J")].action, Action::Resize(Dir::Down, 1));
        assert_eq!(k.root[&c("M-j")].action, Action::FocusNext);
        assert_eq!(k.root[&c("M-h")].action, Action::PrevTab);
        assert_eq!(k.root[&c("M-o")].action, Action::Seq(vec![Action::FocusNext, Action::Zoom]));
        assert_eq!(k.table[&c("c")].action, Action::NewTab);
        // a command zash cannot run is not bound (and not left at its default)
        assert!(!k.table.contains_key(&c("r")));
        // tmux defaults the config did not touch stay
        assert_eq!(k.table[&c("%")].action, Action::Split { side_by_side: true });
    }

    #[test]
    fn chords() {
        assert_eq!(c("C-c"), c("C-C"));
        assert_ne!(c("K"), c("k"));
        assert_eq!(c("M-j"), Chord { ctrl: false, alt: true, key: "j".into() });
        assert_eq!(c("C-Space").key, "Space");
    }
}
