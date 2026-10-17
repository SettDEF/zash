//! The command line's face: help, usage and errors, styled when they go to a terminal.

use std::io::IsTerminal;

fn styled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

/// `**heading**` bold, `` `name` `` in the accent colour, `<arg>` dim italic.
pub fn render(text: &str) -> String {
    if !styled() {
        return text.replace("**", "").replace('`', "");
    }
    let mut out = String::new();
    let (mut bold, mut code) = (false, false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                bold = !bold;
                out.push_str(if bold { "\x1b[1m" } else { "\x1b[22m" });
            }
            '`' => {
                code = !code;
                out.push_str(if code { "\x1b[36m" } else { "\x1b[39m" });
            }
            '<' if !code => {
                out.push_str("\x1b[2;3m<");
                for d in chars.by_ref() {
                    out.push(d);
                    if d == '>' {
                        break;
                    }
                }
                out.push_str("\x1b[22;23m");
            }
            c => out.push(c),
        }
    }
    out
}

pub fn print(text: &str) {
    println!("{}", render(text.trim_end()));
}

/// `error: …`, the usage, a hint; exits with 2.
pub fn fail(msg: &str, usage: &str, more: &str) -> ! {
    let red = if std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none() { ("\x1b[1;31m", "\x1b[0m") } else { ("", "") };
    eprintln!("{}error:{} {msg}\n", red.0, red.1);
    eprintln!("{}", render(usage.trim_end()).replace("\x1b[36m", "").replace("\x1b[39m", ""));
    if !more.is_empty() {
        eprintln!("\nFor more, try '{more}'.");
    }
    std::process::exit(2);
}

pub const HELP: &str = "**zash** {version}
The terminal of zett: GPU-drawn, themed by the wallpaper, with images and web pages inline.

**Usage:**
  `zash` [<options>] [-- <command> …]
  `zash` <subcommand> [<args>]

**Commands:**
  `web` <address>         Show a web page here (inside zeit: in a pane of its own)
  `open` <address>        Open a web page in a window of its own
  `snapshot` [<out.png>]  Draw without a window and print the screen (for tests)

**Options:**
  `-e`, `--exec` <command>  Run <command> instead of the shell ('a command line' goes through sh)
  `-d`, `--dir` <folder>    Start in <folder>
      `--preset` <name>   Layer a preset over your settings (~/.config/zash/presets/)
      `--config` <file>   Use another settings file
  `-h`, `--help`            Show this help (`zash` <subcommand> `--help` for one command)
  `-V`, `--version`         Show the version

**Keys:**
  Ctrl+Shift+C / V      copy / paste             Ctrl+Shift+F    search
  Ctrl+Shift+↑ / ↓      previous / next prompt   Ctrl+Shift+O    copy the last output
  Ctrl+Shift+T / W      new / close tab          Ctrl+Tab        next tab
  Ctrl+= / - / 0        font size                Ctrl+click      open a link

**Settings:** ~/.config/zash/zash.toml";

pub const WEB_USAGE: &str = "**Usage:** `zash web` <address> [`-r` <rows>] [`-s`]";

pub const WEB_HELP: &str = "Show a web page in this terminal.

**Usage:** `zash web` <address> [`-r` <rows>] [`-s`]

  <address>   a URL, a host (wikipedia.org) or words to search
  `-r` <rows>   the page's height in rows (default 30)
  `-s`          inside zeit: beside this pane (else a zeit window of its own)

**In a page:** click to type into it · Esc back to the terminal · Ctrl+L address ·
Alt+← / → back / forward · Ctrl+R reload · Ctrl+Shift+V paste

`zash open` <address> opens it in a window of its own instead.";

pub const OPEN_USAGE: &str = "**Usage:** `zash open` <address>";
