//! One terminal session: a program in a pty, the emulated screen, and the
//! thread moving bytes between them (zash's own loop, which also reads shell marks).

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use std::fs::File;
use std::io::{Read, Write};
use std::sync::Mutex;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term};
use alacritty_terminal::tty;
use std::sync::Arc;
use std::sync::mpsc;

/// What the window's event loop receives: the terminal's events, or zash's own.
pub enum ZEvent {
    /// an event of the terminal in tab `id`
    Term(u64, Event),
    /// tinct wrote new colours
    Theme,
}

/// Where the terminal's events go: the window's event loop, or a channel
/// (headless snapshots and tests).
#[derive(Clone)]
pub enum Sink {
    Window(winit::event_loop::EventLoopProxy<ZEvent>, u64),
    Channel(mpsc::Sender<Event>),
}

#[derive(Clone)]
pub struct Listener(pub Sink, pub Arc<std::sync::OnceLock<Arc<Mutex<File>>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        // replies to the program (device attributes, cursor position, colours asked for) go
        // straight back from the reader thread: a program waiting with a short timeout gets them
        if let (Event::PtyWrite(s), Some(w)) = (&event, self.1.get()) {
            if let Ok(mut f) = w.lock() {
                let _ = std::io::Write::write_all(&mut *f, s.as_bytes());
            }
            return;
        }
        match &self.0 {
            Sink::Window(p, id) => {
                let _ = p.send_event(ZEvent::Term(*id, event));
            }
            Sink::Channel(tx) => {
                let _ = tx.send(event);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub cell_w: f32,
    pub cell_h: f32,
}

impl Grid {
    fn window_size(&self) -> WindowSize {
        WindowSize { num_lines: self.rows as u16, num_cols: self.cols as u16, cell_width: self.cell_w as u16, cell_height: self.cell_h as u16 }
    }
    fn term_size(&self) -> term::test::TermSize {
        term::test::TermSize::new(self.cols, self.rows)
    }
}

/// A shell-integration mark (OSC 133): where a prompt, a command's output or a
/// command's end is, as an absolute line (0 = the oldest line in scrollback).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark {
    /// 'A' prompt starts, 'B' input starts, 'C' output starts, 'D' command finished
    pub kind: char,
    pub line: usize,
    /// for 'D': the exit status the shell reported
    pub exit: Option<i32>,
}

pub struct Session {
    /// the shell plugin's completion popup (None: closed)
    pub popup: Arc<Mutex<Option<Popup>>>,
    /// DECSCNM: reverse video for the whole screen
    pub reverse: Arc<std::sync::atomic::AtomicBool>,
    pub term: Arc<FairMutex<Term<Listener>>>,
    writer: Arc<Mutex<File>>,
    pty: Arc<Mutex<tty::Pty>>,
    pub grid: Grid,
    pub marks: Arc<Mutex<Vec<Mark>>>,
    /// the program's process id (its folder opens new tabs in the same place)
    pub pid: u32,
}

/// Finds OSC 133 marks in a chunk of output, carrying a partial sequence over
/// to the next chunk. Returns (byte offset just after each mark, the mark's
/// letter, its exit status).
#[derive(Default)]
pub struct MarkScanner {
    carry: Vec<u8>,
}

/// What a zash escape carried: a shell mark, or the completion popup's state.
#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    Mark(char, Option<i32>),
    /// DECSCNM: the whole screen in reverse video (light background), on or off
    Reverse(bool),
    Popup(Option<Popup>),
}

/// The completion popup the shell plugin shows (OSC 7733).
#[derive(Clone, Debug, PartialEq)]
pub struct Popup {
    pub items: Vec<String>,
    /// the chosen item (Tab), none before the first Tab
    pub sel: Option<usize>,
    /// how many columns the word being completed already has: the popup lines up with its start
    pub word: usize,
}

impl Popup {
    /// `popup;SEL;WORD;item\x1fitem…` (SEL 0 = none chosen) or `close`.
    pub fn parse(body: &str) -> Option<Option<Popup>> {
        if body == "close" {
            return Some(None);
        }
        let rest = body.strip_prefix("popup;")?;
        let mut it = rest.splitn(3, ';');
        let sel: usize = it.next()?.parse().ok()?;
        let word: usize = it.next()?.parse().ok()?;
        let items: Vec<String> = it.next().unwrap_or("").split('\x1f').filter(|s| !s.is_empty()).map(String::from).collect();
        if items.is_empty() {
            return Some(None);
        }
        Some(Some(Popup { sel: sel.checked_sub(1).filter(|s| *s < items.len()), word, items }))
    }
}

impl MarkScanner {
    /// Nothing carried over from the last chunk.
    pub fn idle(&self) -> bool {
        self.carry.is_empty()
    }

    /// The zash escapes in a chunk of output (OSC 133 marks, OSC 7733 popup), with the
    /// offset in the chunk right after each one ends.
    pub fn scan(&mut self, chunk: &[u8]) -> Vec<(usize, Found)> {
        const HEAD: &[u8] = b"\x1b]";
        const LONGEST: usize = 7; // "\x1b]7733;"
        let carried = self.carry.len();
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut i = 0;
        while let Some(p) = data[i..].windows(HEAD.len()).position(|w| w == HEAD) {
            let start = i + p + HEAD.len();
            // the terminator: BEL or ESC \
            let end = data[start..].iter().position(|&b| b == 0x07 || b == 0x1b).map(|e| start + e);
            let Some(end) = end else {
                // unfinished: keep it for the next chunk (bounded: a runaway OSC is dropped)
                if data.len() - (i + p) < 1 << 16 {
                    self.carry = data[i + p..].to_vec();
                }
                return out;
            };
            let body = String::from_utf8_lossy(&data[start..end]);
            let after = if data[end] == 0x1b { end + 2 } else { end + 1 };
            // offsets relative to this chunk (the carried part was already parsed)
            let at = after.saturating_sub(carried).min(chunk.len());
            if let Some(m) = body.strip_prefix("133;") {
                let mut parts = m.split(';');
                if let Some(kind) = parts.next().and_then(|k| k.chars().next()) {
                    let exit = if kind == 'D' { parts.next().and_then(|v| v.parse().ok()) } else { None };
                    out.push((at, Found::Mark(kind, exit)));
                }
            } else if let Some(m) = body.strip_prefix("7733;") {
                if let Some(p) = Popup::parse(m) {
                    out.push((at, Found::Popup(p)));
                }
            }
            i = end + 1;
        }
        // DECSCNM (CSI ? 5 h / l), which the emulator does not keep: zash swaps the colours itself
        for (pat, on) in [(&b"\x1b[?5h"[..], true), (&b"\x1b[?5l"[..], false)] {
            let mut from = 0;
            while let Some(p) = data[from..].windows(pat.len()).position(|w| w == pat) {
                let after = from + p + pat.len();
                if after > carried {
                    out.push(((after - carried).min(chunk.len()), Found::Reverse(on)));
                }
                from = after;
            }
        }
        out.sort_by_key(|(at, _)| *at);
        // a sequence may begin right at the end of the chunk
        let tail = data.len().saturating_sub(LONGEST - 1);
        if let Some(p) = data[tail.max(i)..].iter().position(|&b| b == 0x1b) {
            self.carry = data[tail.max(i) + p..].to_vec();
        }
        out
    }
}

impl Session {
    /// Starts `cmd` (or the login shell) in a new pty, read by zash's own loop.
    pub fn spawn(cmd: Option<Vec<String>>, workdir: Option<std::path::PathBuf>, grid: Grid, scrollback: usize, sink: Sink) -> std::io::Result<Session> {
        let config = term::Config { scrolling_history: scrollback, kitty_keyboard: false, ..Default::default() };
        let replies = Arc::new(std::sync::OnceLock::new());
        let listener = Listener(sink, replies.clone());
        let term = Arc::new(FairMutex::new(Term::new(config, &grid.term_size(), listener.clone())));

        let mut env = program_env();
        let shell = match cmd {
            Some(c) => {
                let mut it = c.into_iter();
                it.next().map(|p| tty::Shell::new(p, it.collect()))
            }
            None => crate::shell::integrate(&mut env),
        };
        let options = tty::Options { shell, working_directory: workdir, drain_on_exit: true, env };
        let pty = tty::new(&options, grid.window_size(), 0)?;
        let reader = pty.file().try_clone()?;
        let writer = Arc::new(Mutex::new(pty.file().try_clone()?));
        let _ = replies.set(writer.clone());
        let pid = pty.child().id();
        let pty = Arc::new(Mutex::new(pty));
        let marks = Arc::new(Mutex::new(Vec::new()));
        let popup = Arc::new(Mutex::new(None));
        let reverse = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let (t, m, l, pp, rv) = (term.clone(), marks.clone(), listener.clone(), popup.clone(), reverse.clone());
        std::thread::Builder::new().name("zash pty".into()).spawn(move || read_loop(reader, t, m, pp, rv, l))?;
        Ok(Session { term, writer, pty, grid, marks, popup, reverse, pid })
    }

    /// Bytes for the program, as if typed.
    pub fn write(&self, bytes: Vec<u8>) {
        let mut w = self.writer.lock().unwrap();
        let mut rest: &[u8] = &bytes;
        // the pty is non-blocking: a big paste may need a few rounds
        while !rest.is_empty() {
            match w.write(rest) {
                Ok(n) => rest = &rest[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::Interrupted => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(_) => return,
            }
        }
    }

    /// The folder the program is in now (where a new tab should start).
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        std::fs::read_link(format!("/proc/{}/cwd", self.pid)).ok()
    }

    pub fn resize(&mut self, grid: Grid) {
        if grid.cols == self.grid.cols && grid.rows == self.grid.rows {
            self.grid = grid;
            return;
        }
        self.grid = grid;
        self.term.lock().resize(grid.term_size());
        use alacritty_terminal::event::OnResize;
        self.pty.lock().unwrap().on_resize(grid.window_size());
    }
}

/// Reads the program's output, notes shell marks, feeds the parser.
fn read_loop(mut reader: File, term: Arc<FairMutex<Term<Listener>>>, marks: Arc<Mutex<Vec<Mark>>>, popup: Arc<Mutex<Option<Popup>>>, reverse: Arc<std::sync::atomic::AtomicBool>, listener: Listener) {
    use alacritty_terminal::grid::Dimensions;
    let mut parser: alacritty_terminal::vte::ansi::Processor = alacritty_terminal::vte::ansi::Processor::new();
    let mut scanner = MarkScanner::default();
    let mut buf = vec![0u8; 1 << 16];
    let fd = std::os::fd::AsRawFd::as_raw_fd(&reader);
    loop {
        // wait for output (the fd is non-blocking)
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let r = unsafe { libc::poll(&mut pfd, 1, -1) };
        if r < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        // read everything that is waiting under one lock (up to 1 MB), as long
        // as the output keeps coming: far fewer lock and wake-up round trips
        let mut t = term.lock();
        let mut processed = 0usize;
        let mut gone = false;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => { gone = true; break }
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                // EIO: the program is gone
                Err(_) => { gone = true; break }
            };
            let chunk = &buf[..n];
            // most output has no escape at all: skip the mark scan then
            let found = if chunk.contains(&0x1b) || !scanner.idle() { scanner.scan(chunk) } else { Vec::new() };
            let mut at = 0;
            for (end, what) in found {
                parser.advance(&mut *t, &chunk[at..end]);
                at = end;
                let (kind, exit) = match what {
                    Found::Popup(p) => {
                        *popup.lock().unwrap() = p;
                        continue;
                    }
                    Found::Reverse(on) => {
                        reverse.store(on, std::sync::atomic::Ordering::Relaxed);
                        continue;
                    }
                    Found::Mark(k, e) => (k, e),
                };
                // a command starting or a new prompt: the popup is over
                if kind == 'C' || kind == 'A' {
                    *popup.lock().unwrap() = None;
                }
                let line = t.grid().history_size() + t.grid().cursor.point.line.0.max(0) as usize;
                let mut m = marks.lock().unwrap();
                m.push(Mark { kind, line, exit });
                // scrollback is bounded; so is the list of marks
                if m.len() > 20_000 {
                    m.drain(..10_000);
                }
            }
            parser.advance(&mut *t, &chunk[at..]);
            processed += n;
            if processed >= 1 << 20 {
                break;
            }
        }
        let synced = parser.sync_bytes_count() >= processed;
        drop(t);
        if processed > 0 && !synced {
            listener.send_event(Event::Wakeup);
        }
        if gone {
            break;
        }
    }
    listener.send_event(Event::Exit);
}

#[cfg(test)]
mod mark_tests {
    use super::{Found, MarkScanner, Popup};

    #[test]
    fn popup_messages() {
        let mut s = MarkScanner::default();
        let out = s.scan(b"\x1b]7733;popup;2;3;Documents/\x1fDownloads/\x07x\x1b]7733;close\x07");
        assert_eq!(out[0].1, Found::Popup(Some(Popup { items: vec!["Documents/".into(), "Downloads/".into()], sel: Some(1), word: 3 })));
        assert_eq!(out[1].1, Found::Popup(None));
        assert_eq!(Popup::parse("popup;0;1;a"), Some(Some(Popup { items: vec!["a".into()], sel: None, word: 1 })));
        assert_eq!(Popup::parse("nonsense"), None);
    }

    #[test]
    fn finds_marks() {
        let mut s = MarkScanner::default();
        let out = s.scan(b"ab\x1b]133;A\x07$ ls\r\n\x1b]133;C\x07out\x1b]133;D;1\x1b\\");
        assert_eq!(out.iter().map(|o| o.1.clone()).collect::<Vec<_>>(), vec![Found::Mark('A', None), Found::Mark('C', None), Found::Mark('D', Some(1))]);
        assert_eq!(out[0].0, 10, "offset just after the BEL");
    }

    #[test]
    fn mark_split_across_reads() {
        let mut s = MarkScanner::default();
        assert!(s.scan(b"text\x1b]13").is_empty());
        let out = s.scan(b"3;D;0\x07more");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, Found::Mark('D', Some(0)));
        assert_eq!(out[0].0, 6, "offset within the second chunk");
    }
}

/// Variables other terminals and multiplexers set. A zash started from inside
/// one of them must not pass them on: programs would think they run there
/// (tmux-aware tools, image protocols, true-colour detection all go wrong).
pub const FOREIGN: &[&str] = &[
    "TMUX", "TMUX_PANE", "STY", "ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID",
    "WEZTERM_PANE", "WEZTERM_UNIX_SOCKET", "WEZTERM_EXECUTABLE", "WEZTERM_EXECUTABLE_DIR", "WEZTERM_CONFIG_DIR", "WEZTERM_CONFIG_FILE",
    "KITTY_WINDOW_ID", "KITTY_PID", "KITTY_PUBLIC_KEY", "KITTY_INSTALLATION_DIR", "KITTY_LISTEN_ON",
    "ALACRITTY_WINDOW_ID", "ALACRITTY_SOCKET", "ALACRITTY_LOG",
    "VTE_VERSION", "GNOME_TERMINAL_SCREEN", "GNOME_TERMINAL_SERVICE", "KONSOLE_VERSION", "KONSOLE_DBUS_SESSION", "KONSOLE_DBUS_SERVICE",
    "ITERM_SESSION_ID", "TERMINAL_EMULATOR", "TERM_SESSION_ID", "WT_SESSION", "GHOSTTY_RESOURCES_DIR", "GHOSTTY_BIN_DIR",
    "COLUMNS", "LINES",
];

/// What zash itself sets for the programs it runs (on top of the inherited
/// environment, from which FOREIGN was removed at start-up).
pub fn program_env() -> std::collections::HashMap<String, String> {
    let mut env = std::collections::HashMap::new();
    env.insert("TERM".into(), "xterm-256color".into());
    env.insert("COLORTERM".into(), "truecolor".into());
    env.insert("TERM_PROGRAM".into(), "zash".into());
    env.insert("TERM_PROGRAM_VERSION".into(), env!("CARGO_PKG_VERSION").into());
    env.insert("ZASH".into(), "1".into());
    // a launcher can start zash without a locale; programs then fall back to ASCII
    let has_utf8 = ["LC_ALL", "LC_CTYPE", "LANG"].iter().any(|k| std::env::var(k).is_ok_and(|v| v.to_uppercase().contains("UTF-8") || v.to_uppercase().contains("UTF8")));
    if !has_utf8 {
        env.insert("LANG".into(), "C.UTF-8".into());
    }
    env
}

/// Called once at start-up, before any thread exists.
pub fn scrub_environment() {
    for k in FOREIGN {
        // SAFETY: start-up, single-threaded
        unsafe { std::env::remove_var(k) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_for_programs() {
        let e = program_env();
        assert_eq!(e["TERM"], "xterm-256color");
        assert_eq!(e["COLORTERM"], "truecolor");
        assert_eq!(e["TERM_PROGRAM"], "zash");
        assert!(FOREIGN.contains(&"TMUX") && FOREIGN.contains(&"WEZTERM_PANE"));
    }
}
