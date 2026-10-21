//! One terminal session: a program in a pty, the emulated screen, and the
//! thread moving bytes between them (zash's own loop, which also reads shell marks).

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use std::fs::File;
use std::io::Read;
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
    /// the settings file (or the preset) changed
    Settings,
    /// files dropped on the window (Wayland), and where (logical pixels)
    Drop(Vec<std::path::PathBuf>, f64, f64),
    /// the pen, while drawing mode holds it (pen.rs)
    Pen(crate::pen::Pen),
}

/// Where the terminal's events go: the window's event loop, or a channel
/// (headless snapshots and tests).
#[derive(Clone)]
pub enum Sink {
    Window(winit::event_loop::EventLoopProxy<ZEvent>, u64),
    Channel(mpsc::Sender<Event>),
}

#[derive(Clone)]
pub struct Listener(pub Sink, pub Arc<std::sync::OnceLock<mpsc::Sender<Vec<u8>>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        // replies to the program (device attributes, cursor position, colours asked for) go
        // straight back from the reader thread: a program waiting with a short timeout gets them
        if let (Event::PtyWrite(s), Some(w)) = (&event, self.1.get()) {
            let _ = w.send(s.as_bytes().to_vec());
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
    pub(crate) fn window_size(&self) -> WindowSize {
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
    /// a Neovim server the shell started, waiting for zash to attach
    pub nvim_sock: Arc<Mutex<Option<String>>>,
    /// the popup's look as this pane's shell set it (zstyle), over zash.toml
    pub popup_style: Arc<Mutex<String>>,
    /// small two-column rows (zeit's job lines), by screen row
    pub ui_rows: Arc<Mutex<Vec<UiRow>>>,
    /// keys the program in front claimed (it gets them, not zash)
    pub claims: Arc<Mutex<Claims>>,
    /// images (Kitty graphics) and where they are placed
    pub graphics: Arc<Mutex<crate::graphics::Graphics>>,
    /// what the pen drew on this pane (ink.rs)
    pub ink: Arc<Mutex<crate::ink::Ink>>,
    /// a cell in pixels (images are sized in cells)
    pub cell_px: Arc<Mutex<(f32, f32)>>,
    pub term: Arc<FairMutex<Term<Listener>>>,
    /// what goes to the program: a thread of its own writes it, so neither the window nor the
    /// reader ever waits on a program that does not read (that froze zash)
    writer: mpsc::Sender<Vec<u8>>,
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
    /// the shell started Neovim as a server on this socket: zash becomes its screen
    Nvim(String),
    /// the popup's look from the shell's zstyles (`opacity=0.8,blur=yes`)
    PopupStyle(String),
    /// rows another program (zeit) draws as small two-column lines: OSC 7733;rows;…
    Rows(Vec<UiRow>),
    /// a Kitty graphics command: what follows `ESC _ G`
    Graphics(String),
    /// a web area: OSC 7733;web;rows=N;url=…
    Web(String),
    /// a web area pinned to screen cells by a multiplexer: webrect, webhide or webclose, and the rest
    WebPin(String, String),
    /// keys a program wants for itself: OSC 7733;keys;all|none|claim=…|release=…
    Keys(String),
    /// a text effect starts or ends: OSC 7733;fx;NAME;key=value… or fx;end (fx.rs)
    Fx(String),
    /// DECSCNM: the whole screen in reverse video (light background), on or off
    Reverse(bool),
    /// OSC 7733;q: a program asks what this zash can do (works over SSH, unlike TERM_PROGRAM)
    Query,
    Popup(Option<Popup>),
}

/// The completion popup the shell plugin shows (OSC 7733).
/// A row drawn small (zeit's job line): the command on the left, any number of
/// columns right-aligned, each with its tone, and an optional progress bar.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct UiRow {
    pub row: usize,
    pub col: usize,
    pub width: usize,
    pub left: String,
    /// (tone, text), drawn from the right edge leftwards in reverse order
    pub cols: Vec<(char, String)>,
    /// 0..1, drawn as a thin bar under the row
    pub progress: Option<f32>,
    /// buttons at fixed cells (offset from `col`), so a click lands where they show
    pub buttons: Vec<(usize, String)>,
}

impl UiRow {
    /// `ROW␟COL␟WIDTH␟LEFT␟TONE:TEXT␟…␟%:0.42`, rows split by ␞. Tones: g(ood),
    /// b(ad), r(unning), m(uted), a(ccent), or anything else for plain.
    pub fn parse_list(s: &str) -> Vec<UiRow> {
        s.split('\x1e')
            .filter_map(|item| {
                let f: Vec<&str> = item.split('\x1f').collect();
                let mut r = UiRow { row: f.first()?.parse().ok()?, col: f.get(1)?.parse().ok()?, width: f.get(2)?.parse().ok()?, left: f.get(3).unwrap_or(&"").to_string(), ..Default::default() };
                for c in f.iter().skip(4) {
                    match c.split_once(':') {
                        Some(("%", v)) => r.progress = v.parse::<f32>().ok().map(|p| p.clamp(0.0, 1.0)),
                        Some(("@", v)) => {
                            if let Some((at, glyph)) = v.split_once(':') {
                                if let Ok(at) = at.parse() {
                                    r.buttons.push((at, glyph.to_string()));
                                }
                            }
                        }
                        Some((t, text)) if t.chars().count() == 1 => r.cols.push((t.chars().next().unwrap(), text.to_string())),
                        _ => r.cols.push((' ', c.to_string())),
                    }
                }
                Some(r)
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Popup {
    pub items: Vec<String>,
    /// the line (from the oldest in scrollback) the cursor was on when it came: a popup
    /// whose shell went away (crash, exit) disappears once the cursor leaves that line
    pub line: Option<usize>,
    /// the chosen item (Tab), none before the first Tab
    pub sel: Option<usize>,
    /// how many columns the word being completed already has: the popup lines up with its start
    pub word: usize,
    /// the column the word starts at, taken when the popup came (the live cursor wanders while
    /// the shell redraws the line: the popup followed it and jumped)
    pub col: Option<usize>,
    /// the screen row it came on (the same: zeit repaints with the cursor elsewhere for a moment)
    pub row: Option<usize>,
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
        Some(Some(Popup { sel: sel.checked_sub(1).filter(|s| *s < items.len()), word, items, line: None, col: None, row: None }))
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
            } else if let Some(look) = body.strip_prefix("7733;style;") {
                out.push((at, Found::PopupStyle(look.to_string())));
            } else if let Some(k) = body.strip_prefix("7733;keys;") {
                out.push((at, Found::Keys(k.to_string())));
            } else if body == "7733;q" {
                out.push((at, Found::Query));
            } else if let Some(f) = body.strip_prefix("7733;fx;").or_else(|| (body == "7733;fx").then_some("")) {
                out.push((at, Found::Fx(f.to_string())));
            } else if let Some((kind, rest)) = body.strip_prefix("7733;").and_then(|b| b.split_once(';')).filter(|(k, _)| matches!(*k, "webrect" | "webhide" | "webclose" | "zfloat" | "zfloatclose")) {
                out.push((at, Found::WebPin(kind.to_string(), rest.to_string())));
            } else if let Some(w) = body.strip_prefix("7733;web;") {
                out.push((at, Found::Web(w.to_string())));
            } else if let Some(list) = body.strip_prefix("7733;rows;").or_else(|| (body == "7733;rows").then_some("")) {
                out.push((at, Found::Rows(UiRow::parse_list(list))));
            } else if let Some(sock) = body.strip_prefix("7733;nvim;") {
                out.push((at, Found::Nvim(sock.to_string())));
            } else if let Some(m) = body.strip_prefix("7733;") {
                if let Some(p) = Popup::parse(m) {
                    out.push((at, Found::Popup(p)));
                }
            }
            i = end + 1;
        }
        // Kitty graphics (APC G … ESC \): images; one cut across reads waits for its end
        let mut j = 0;
        while let Some(p) = data[j..].windows(3).position(|w| w == b"\x1b_G") {
            let start = j + p + 3;
            match data[start..].windows(2).position(|w| w == b"\x1b\\") {
                Some(e) => {
                    let body = String::from_utf8_lossy(&data[start..start + e]).to_string();
                    let after = start + e + 2;
                    out.push((after.saturating_sub(carried).min(chunk.len()), Found::Graphics(body)));
                    j = after;
                }
                None => {
                    if data.len() - (j + p) < 1 << 20 {
                        self.carry = data[j + p..].to_vec();
                    }
                    out.sort_by_key(|(at, _)| *at);
                    return out;
                }
            }
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
    /// The program in front in this terminal (the pty's foreground process group).
    pub fn foreground(&self) -> Option<String> {
        use std::os::fd::AsRawFd;
        let fd = self.pty.lock().ok()?.file().as_raw_fd();
        let pg = unsafe { libc::tcgetpgrp(fd) };
        if pg <= 0 {
            return None;
        }
        std::fs::read_to_string(format!("/proc/{pg}/comm")).ok().map(|s| s.trim().to_string())
    }

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
        let writer = spawn_writer(pty.file().try_clone()?)?;
        let _ = replies.set(writer.clone());
        let pid = pty.child().id();
        let pty = Arc::new(Mutex::new(pty));
        let marks = Arc::new(Mutex::new(Vec::new()));
        let popup = Arc::new(Mutex::new(None));
        let reverse = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let nvim_sock = Arc::new(Mutex::new(None));
        let popup_style = Arc::new(Mutex::new(String::new()));

        let ui_rows = Arc::new(Mutex::new(Vec::new()));
        let graphics: Arc<Mutex<crate::graphics::Graphics>> = Arc::default();
        let cell_px = Arc::new(Mutex::new((grid.cell_w, grid.cell_h)));
        let (t, m, l, pp, rv, nv, ps, ur) = (term.clone(), marks.clone(), listener.clone(), popup.clone(), reverse.clone(), nvim_sock.clone(), popup_style.clone(), ui_rows.clone());
        let claims: Arc<Mutex<Claims>> = Arc::default();
        let (gr, cp, cl) = (graphics.clone(), cell_px.clone(), claims.clone());
        std::thread::Builder::new().name("zash pty".into()).spawn(move || read_loop(reader, t, m, pp, rv, nv, ps, ur, gr, cp, cl, l))?;
        Ok(Session { term, writer, pty, grid, marks, popup, reverse, nvim_sock, popup_style, ui_rows, graphics, ink: Arc::default(), cell_px, claims, pid })
    }

    /// Bytes for the program, as if typed.
    /// Queues bytes for the program; never waits.
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.writer.send(bytes);
    }

    /// The folder the program is in now (where a new tab should start).
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        std::fs::read_link(format!("/proc/{}/cwd", self.pid)).ok()
    }

    pub fn resize(&mut self, grid: Grid) {
        *self.cell_px.lock().unwrap() = (grid.cell_w, grid.cell_h);
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
fn read_loop(mut reader: File, term: Arc<FairMutex<Term<Listener>>>, marks: Arc<Mutex<Vec<Mark>>>, popup: Arc<Mutex<Option<Popup>>>, reverse: Arc<std::sync::atomic::AtomicBool>, nvim_sock: Arc<Mutex<Option<String>>>, popup_style: Arc<Mutex<String>>, ui_rows: Arc<Mutex<Vec<UiRow>>>, graphics: Arc<Mutex<crate::graphics::Graphics>>, cell_px: Arc<Mutex<(f32, f32)>>, claims: Arc<Mutex<Claims>>, listener: Listener) {
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
                        let line = t.grid().history_size() + t.grid().cursor.point.line.0.max(0) as usize;
                        let col = t.grid().cursor.point.column.0.saturating_sub(p.as_ref().map_or(0, |p| p.word));
                        *popup.lock().unwrap() = p.map(|p| Popup { line: Some(line), col: Some(col), row: Some(t.grid().cursor.point.line.0.max(0) as usize), ..p });
                        continue;
                    }
                    Found::Reverse(on) => {
                        reverse.store(on, std::sync::atomic::Ordering::Relaxed);
                        continue;
                    }
                    Found::PopupStyle(look) => {
                        *popup_style.lock().unwrap() = look;
                        continue;
                    }
                    Found::Nvim(sock) => {
                        *nvim_sock.lock().unwrap() = Some(sock);
                        continue;
                    }
                    Found::Rows(rows) => {
                        *ui_rows.lock().unwrap() = rows;
                        continue;
                    }
                    Found::Keys(body) => {
                        claims.lock().unwrap().apply(&body);
                        continue;
                    }
                    Found::Fx(body) => {
                        // the effect rides the cells' hyperlink: it stays on them through scrolling,
                        // scrollback and resizing, and ends where the run ends
                        use alacritty_terminal::vte::ansi::{Handler, Hyperlink};
                        match crate::fx::Escape::parse(&body) {
                            crate::fx::Escape::Start(uri) => t.set_hyperlink(Some(Hyperlink { id: None, uri })),
                            crate::fx::Escape::End => t.set_hyperlink(None),
                        }
                        continue;
                    }
                    Found::WebPin(kind, body) => {
                        let cell = *cell_px.lock().unwrap();
                        let wl = listener.clone();
                        let wake: Box<dyn Fn() + Send + Sync> = Box::new(move || {
                            use alacritty_terminal::event::EventListener;
                            wl.send_event(alacritty_terminal::event::Event::Wakeup);
                        });
                        graphics.lock().unwrap().pin(&kind, &body, cell, wake);
                        continue;
                    }
                    Found::Web(body) => {
                        if let Some((rows, focus, url)) = crate::web::parse(&body) {
                            let at = (t.grid().history_size() + t.grid().cursor.point.line.0.max(0) as usize, t.grid().cursor.point.column.0);
                            let cols = t.grid().columns().saturating_sub(at.1).max(1);
                            let (cw, ch) = *cell_px.lock().unwrap();
                            let id = graphics.lock().unwrap().next_web_id();
                            let wl = listener.clone();
                            let wake: Box<dyn Fn() + Send + Sync> = Box::new(move || {
                                use alacritty_terminal::event::EventListener;
                                wl.send_event(alacritty_terminal::event::Event::Wakeup);
                            });
                            if let Some(v) = crate::web::View::open(id, &url, (cols as f32 * cw) as u32, (rows as f32 * ch) as u32, wake) {
                                let mut g = graphics.lock().unwrap();
                                if focus {
                                    g.autofocus = Some(v.id);
                                }
                                g.add_web(v, at, cols, rows);
                            }
                        }
                        continue;
                    }
                    Found::Query => {
                        use alacritty_terminal::event::EventListener;
                        listener.send_event(alacritty_terminal::event::Event::PtyWrite(crate::fx::capabilities()));
                        continue;
                    }
                    Found::Graphics(body) => {
                        let at = (t.grid().history_size() + t.grid().cursor.point.line.0.max(0) as usize, t.grid().cursor.point.column.0);
                        let cell = *cell_px.lock().unwrap();
                        let o = graphics.lock().unwrap().command(&body, cell, at);
                        if let Some(r) = o.reply {
                            use alacritty_terminal::event::EventListener;
                            listener.send_event(alacritty_terminal::event::Event::PtyWrite(r));
                        }
                        // past the image, as Kitty does: to its last row, after its last column
                        if let Some((down, right)) = o.advance {
                            let mut seq = "\n".repeat(down);
                            if right > 0 {
                                seq.push_str(&format!("\x1b[{right}C"));
                            }
                            parser.advance(&mut *t, seq.as_bytes());
                        }
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
        assert_eq!(out[0].1, Found::Popup(Some(Popup { items: vec!["Documents/".into(), "Downloads/".into()], sel: Some(1), word: 3, line: None, col: None, row: None })));
        assert_eq!(out[1].1, Found::Popup(None));
        assert_eq!(Popup::parse("popup;0;1;a"), Some(Some(Popup { items: vec!["a".into()], sel: None, word: 1, line: None, col: None, row: None })));
        assert_eq!(Popup::parse("nonsense"), None);
    }

    #[test]
    fn a_program_can_ask_what_zash_can_do() {
        let mut s = MarkScanner::default();
        let out = s.scan(b"x\x1b]7733;q\x1b\\y\x1b]7733;qq\x07");
        assert_eq!(out.len(), 1, "only the exact query");
        assert_eq!(out[0].1, Found::Query);
        let a = crate::fx::capabilities();
        assert!(a.starts_with("\x1b]7733;zash;v=") && a.ends_with("\x1b\\") && a.contains("marks=scribble"));
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

#[cfg(test)]
mod ui_row_tests {
    use super::*;

    #[test]
    fn rows_message() {
        let mut s = MarkScanner::default();
        let out = s.scan(b"\x1b]7733;rows;3\x1f0\x1f80\x1f\xe2\x96\xb8 cargo build\x1fm:bottom\x1fg:\xe2\x9c\x93 3.2s\x1e9\x1f40\x1f40\x1fmake\x1fr:\xe2\x9f\xb3 4s\x1f%:0.5\x07");
        let Found::Rows(r) = &out[0].1 else { panic!("{out:?}") };
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].row, r[0].col, r[0].width), (3, 0, 80));
        assert_eq!(r[0].cols, vec![('m', "bottom".to_string()), ('g', "✓ 3.2s".to_string())]);
        assert_eq!(r[1].progress, Some(0.5));
        let b = UiRow::parse_list("1\x1f0\x1f40\x1fx\x1fg:ok\x1f@:36:■\x1f@:38:⇢");
        assert_eq!(b[0].buttons, vec![(36, "■".to_string()), (38, "⇢".to_string())]);
        assert_eq!(b[0].cols.len(), 1);
        assert_eq!(r[0].left, "▸ cargo build");
        assert_eq!(r[1].cols[0].1, "⟳ 4s");
        let out = s.scan(b"\x1b]7733;rows;\x07");
        assert_eq!(out[0].1, Found::Rows(Vec::new()));
    }
}

/// Keys the program in front wants for itself (OSC 7733;keys): `all` the multiplexer
/// keys (prefix and no-prefix bindings), or single chords in tmux notation (`C-Space`, `M-h`).
#[derive(Default, Debug, PartialEq)]
pub struct Claims {
    pub all: bool,
    pub keys: std::collections::HashSet<String>,
}

impl Claims {
    pub fn apply(&mut self, msg: &str) {
        match msg {
            "all" => self.all = true,
            "none" => *self = Claims::default(),
            m => {
                let list = |s: &str| s.split(',').map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect::<Vec<_>>();
                if let Some(l) = m.strip_prefix("claim=") {
                    self.keys.extend(list(l));
                } else if let Some(l) = m.strip_prefix("release=") {
                    for k in list(l) {
                        self.keys.remove(&k);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod claim_tests {
    use super::*;

    #[test]
    fn claims_add_up_and_go() {
        let mut s = MarkScanner::default();
        let out = s.scan(b"\x1b]7733;keys;claim=M-h,C-Space\x07");
        let Found::Keys(k) = &out[0].1 else { panic!() };
        let mut c = Claims::default();
        c.apply(k);
        assert!(c.keys.contains("M-h") && c.keys.contains("C-Space") && !c.all);
        c.apply("release=M-h");
        assert!(!c.keys.contains("M-h"));
        c.apply("all");
        assert!(c.all);
        c.apply("none");
        assert_eq!(c, Claims::default());
    }
}

/// The thread that writes to a program: it waits for the program to read, nobody else does.
fn spawn_writer(mut f: File) -> std::io::Result<mpsc::Sender<Vec<u8>>> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new().name("zash pty writer".into()).spawn(move || {
        for bytes in rx {
            let mut rest: &[u8] = &bytes;
            // the pty is non-blocking: a big paste (or a program that reads slowly) takes rounds
            while !rest.is_empty() {
                match std::io::Write::write(&mut f, rest) {
                    Ok(n) => rest = &rest[n..],
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::Interrupted => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    // the program is gone
                    Err(_) => return,
                }
            }
        }
    })?;
    Ok(tx)
}

#[cfg(test)]
mod writer_tests {
    use super::*;

    /// A program that never reads: writing far more than the pty holds must not wait (the
    /// window called this; waiting froze zash, and with the reader's replies it deadlocked).
    #[test]
    fn writing_never_waits_on_the_program() {
        let (tx, _rx) = mpsc::channel();
        let grid = Grid { cols: 80, rows: 24, cell_w: 9.0, cell_h: 18.0 };
        let s = Session::spawn(Some(vec!["sleep".into(), "20".into()]), None, grid, 100, Sink::Channel(tx)).expect("spawn");
        let started = std::time::Instant::now();
        for _ in 0..2000 {
            s.write(vec![b'x'; 512]);
        }
        assert!(started.elapsed() < std::time::Duration::from_millis(500), "write waited {:?}", started.elapsed());
    }
}

#[cfg(test)]
mod fx_scan_tests {
    #[test]
    fn effects_are_found_with_their_parameters() {
        let mut s = super::MarkScanner::default();
        let out = s.scan(b"a\x1b]7733;fx;shimmer;speed=2\x07text\x1b]7733;fx;end\x07");
        let found: Vec<_> = out.into_iter().map(|(_, f)| f).collect();
        assert_eq!(found, vec![super::Found::Fx("shimmer;speed=2".into()), super::Found::Fx("end".into())]);
    }
}
