//! One terminal session: a program in a pty, the emulated screen, and the
//! thread moving bytes between them (alacritty_terminal's event loop).

use alacritty_terminal::event::{Event, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term};
use alacritty_terminal::tty;
use std::sync::Arc;
use std::sync::mpsc;

/// What the window's event loop receives: the terminal's events, or zash's own.
pub enum ZEvent {
    Term(Event),
    /// tinct wrote new colours
    Theme,
}

/// Where the terminal's events go: the window's event loop, or a channel
/// (headless snapshots and tests).
#[derive(Clone)]
pub enum Sink {
    Window(winit::event_loop::EventLoopProxy<ZEvent>),
    Channel(mpsc::Sender<Event>),
}

#[derive(Clone)]
pub struct Listener(pub Sink);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match &self.0 {
            Sink::Window(p) => {
                let _ = p.send_event(ZEvent::Term(event));
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

pub struct Session {
    pub term: Arc<FairMutex<Term<Listener>>>,
    notifier: Notifier,
    pub grid: Grid,
}

impl Session {
    /// Starts `cmd` (or the login shell) in a new pty.
    pub fn spawn(cmd: Option<Vec<String>>, grid: Grid, scrollback: usize, sink: Sink) -> std::io::Result<Session> {
        let config = term::Config { scrolling_history: scrollback, kitty_keyboard: false, ..Default::default() };
        let listener = Listener(sink);
        let term = Arc::new(FairMutex::new(Term::new(config, &grid.term_size(), listener.clone())));

        let mut env = std::collections::HashMap::new();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        env.insert("TERM_PROGRAM".into(), "zash".into());
        let shell = cmd.and_then(|c| {
            let mut it = c.into_iter();
            it.next().map(|p| tty::Shell::new(p, it.collect()))
        });
        let options = tty::Options { shell, working_directory: None, drain_on_exit: true, env };
        let pty = tty::new(&options, grid.window_size(), 0)?;
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let notifier = Notifier(event_loop.channel());
        event_loop.spawn();
        Ok(Session { term, notifier, grid })
    }

    /// Bytes for the program, as if typed.
    pub fn write(&self, bytes: Vec<u8>) {
        self.notifier.notify(bytes);
    }

    pub fn resize(&mut self, grid: Grid) {
        if grid.cols == self.grid.cols && grid.rows == self.grid.rows {
            self.grid = grid;
            return;
        }
        self.grid = grid;
        self.term.lock().resize(grid.term_size());
        let _ = self.notifier.0.send(Msg::Resize(grid.window_size()));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}
