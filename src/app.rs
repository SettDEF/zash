//! The window: winit + a wgpu surface, keys and mouse in, frames out.
//!
//! Frames are drawn only when something changed (the terminal reports new
//! output, the window resizes, a key scrolls or selects). Idle means idle.

use crate::config::Config;
use crate::input::{self, Mods, Named};
use crate::keys::{Action, Chord, Keys};
use crate::layout::{Layout, Rect};
use crate::render::{Frame, PaneView, Renderer};
use crate::session::{Grid, Session, Sink, ZEvent};
use alacritty_terminal::event::{Event, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use std::io::Write;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    window: Arc<Window>,
}

/// One terminal: a session and its title.
struct Pane {
    id: u64,
    session: Session,
    title: String,
    /// Neovim drawn by zash while it runs in this pane
    nvim: Option<crate::nvim::Nvim>,
}

/// One tab: its panes, how they are split, and which one has the keyboard.
struct Tab {
    panes: Vec<Pane>,
    layout: Layout,
    focus: u64,
    /// the focused pane fills the tab (tmux zoom)
    zoom: bool,
}

impl Tab {
    fn focused(&self) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == self.focus)
    }
}

struct App {
    cfg: Config,
    cmd: Option<Vec<String>>,
    workdir: Option<std::path::PathBuf>,
    proxy: EventLoopProxy<ZEvent>,
    gpu: Option<Gpu>,
    /// the open tabs; `active` is the one shown
    tabs: Vec<Tab>,
    active: usize,
    /// the tab shown before (last-window)
    last_active: usize,
    next_id: u64,
    /// pane and tab keys, from the tmux config
    keys: Keys,
    /// the prefix was pressed: the next key is a command
    prefix: bool,
    /// a repeatable command (`bind -r`) may run again without the prefix until then
    repeat_until: Option<std::time::Instant>,
    mods: ModifiersState,
    font_pt: f32,
    mouse: (f64, f64),
    /// the web area that has the keyboard (by its view id), and the held button
    web_focus: Option<u32>,
    web_btn: Option<&'static str>,
    /// Ctrl+L in a web area: the address being typed
    web_addr: Option<String>,
    /// the window title last set from a page
    web_title: String,
    selecting: bool,
    scroll_acc: f64,
    /// the button held down in a Neovim pane (for drags)
    nvim_btn: Option<&'static str>,
    /// the button held down for a program that wants the mouse
    mouse_btn: Option<u8>,
    last_cell: (usize, usize),
    /// (time, count, cell) of the last left click: double = word, triple = line
    clicks: (std::time::Instant, u8, (usize, usize)),
    /// smooth scrolling: how far the focused pane is still drawn from its real position (px), and when that was
    glide: f32,
    glide_at: std::time::Instant,
    /// scrollback search (Ctrl+Shift+F) while it is open
    search: Option<crate::search::Search>,
}

pub fn run(cfg: Config, cmd: Option<Vec<String>>, workdir: Option<std::path::PathBuf>) -> Result<(), String> {
    let event_loop = EventLoop::<ZEvent>::with_user_event().build().map_err(|e| e.to_string())?;
    let proxy = event_loop.create_proxy();
    let font_pt = cfg.font_size_pt;
    let mut app = App { cfg, cmd, workdir, proxy, gpu: None, tabs: Vec::new(), active: 0, last_active: 0, next_id: 0, keys: Keys::load(), prefix: false, repeat_until: None, mods: ModifiersState::empty(), font_pt, mouse: (0.0, 0.0), web_focus: None, web_btn: None, web_addr: None, web_title: String::new(), selecting: false, scroll_acc: 0.0, glide: 0.0, glide_at: std::time::Instant::now(), mouse_btn: None, nvim_btn: None, last_cell: (0, 0), clicks: (std::time::Instant::now(), 0, (0, 0)), search: None };
    event_loop.run_app(&mut app).map_err(|e| e.to_string())
}

fn clipboard_get(primary: bool) -> String {
    let mut c = std::process::Command::new("wl-paste");
    c.arg("--no-newline");
    if primary {
        c.arg("--primary");
    }
    c.output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

fn clipboard_set(text: &str, primary: bool) {
    let mut c = std::process::Command::new("wl-copy");
    if primary {
        c.arg("--primary");
    }
    if let Ok(mut child) = c.stdin(std::process::Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

impl App {
    /// The focused pane of the shown tab.
    fn pane(&self) -> Option<&Pane> {
        self.tabs.get(self.active)?.focused()
    }

    fn sess(&self) -> Option<&Session> {
        self.pane().map(|p| &p.session)
    }

    /// The window below the tab bar, in pixels.
    fn area(&self) -> Option<Rect> {
        let g = self.gpu.as_ref()?;
        let top = g.renderer.top;
        Some(Rect { x: 0.0, y: top, w: g.config.width as f32, h: g.config.height as f32 - top })
    }

    /// Where each pane of a tab is (a zoomed tab shows only its focused pane).
    fn pane_rects(&self, tab: usize) -> Vec<(u64, Rect)> {
        let (Some(t), Some(area)) = (self.tabs.get(tab), self.area()) else { return Vec::new() };
        if t.zoom {
            return vec![(t.focus, area)];
        }
        t.layout.rects(area)
    }

    /// The focused pane's rectangle in the shown tab.
    fn focus_rect(&self) -> Option<Rect> {
        let t = self.tabs.get(self.active)?;
        self.pane_rects(self.active).into_iter().find(|(id, _)| *id == t.focus).map(|(_, r)| r)
    }

    fn grid_in(&self, r: Rect) -> Option<Grid> {
        let g = self.gpu.as_ref()?;
        let (cols, rows) = g.renderer.grid_in(r.w, r.h);
        Some(Grid { cols, rows, cell_w: g.renderer.cell_w, cell_h: g.renderer.cell_h })
    }

    /// The grid of the whole tab area (a new tab's first pane).
    fn grid(&self) -> Option<Grid> {
        self.grid_in(self.area()?)
    }

    fn redraw(&self) {
        if let Some(g) = &self.gpu {
            g.window.request_redraw();
        }
    }

    fn sync_size(&mut self) {
        // every pane of every tab, not only the shown ones: a hidden one is the right size when it comes back
        for ti in 0..self.tabs.len() {
            let rects = self.pane_rects(ti);
            for (id, r) in rects {
                if let Some(grid) = self.grid_in(r) {
                    if let Some(p) = self.tabs[ti].panes.iter_mut().find(|p| p.id == id) {
                        p.session.resize(grid);
                        if let Some(n) = &p.nvim {
                            n.resize(grid.cols, grid.rows);
                        }
                    }
                }
            }
        }
        self.redraw();
    }

    fn write(&self, bytes: Vec<u8>) {
        if let Some(s) = self.sess() {
            // typing returns to the live screen and drops a selection
            {
                let mut t = s.term.lock();
                t.scroll_display(Scroll::Bottom);
                t.selection = None;
            }
            s.write(bytes);
        }
    }

    fn mode(&self) -> TermMode {
        self.sess().map(|s| *s.term.lock().mode()).unwrap_or(TermMode::empty())
    }

    /// Window pixels → a point in the focused pane's terminal (accounting for scrollback).
    fn point_at(&self, x: f64, y: f64) -> Option<(Point, Side)> {
        let g = self.gpu.as_ref()?;
        let s = self.sess()?;
        let f = self.focus_rect()?;
        let r = &g.renderer;
        let (ox, oy) = r.origin(f.x, f.y, f.w, f.h);
        let col_f = ((x as f32 - ox) / r.cell_w).max(0.0);
        let col = (col_f as usize).min(s.grid.cols.saturating_sub(1));
        let row = (((y as f32 - oy) / r.cell_h).max(0.0) as usize).min(s.grid.rows.saturating_sub(1));
        let offset = s.term.lock().grid().display_offset() as i32;
        let side = if col_f.fract() < 0.5 { Side::Left } else { Side::Right };
        Some((Point::new(Line(row as i32 - offset), Column(col)), side))
    }

    /// The web area under a window position, and the position inside it (pixels).
    fn web_hit(&self, x: f64, y: f64) -> Option<(std::sync::Arc<crate::web::View>, f32, f32)> {
        let g = self.gpu.as_ref()?;
        let s = self.sess()?;
        let f = self.focus_rect()?;
        let r = &g.renderer;
        let (ox, oy) = r.origin(f.x, f.y, f.w, f.h);
        let top = { let t = s.term.lock(); t.grid().history_size() as i64 - t.grid().display_offset() as i64 };
        let gr = s.graphics.lock().unwrap();
        let offset = s.term.lock().grid().display_offset();
        if offset == 0 {
            for (id, (rect, _)) in &gr.pinned {
                let (Some((cx, cy, cw, ch)), Some(v)) = (rect, gr.webs.get(id)) else { continue };
                let (px, py) = (ox + *cx as f32 * r.cell_w, oy + *cy as f32 * r.cell_h);
                let (lx, ly) = (x as f32 - px, y as f32 - py);
                if lx >= 0.0 && ly >= 0.0 && lx < *cw as f32 * r.cell_w && ly < *ch as f32 * r.cell_h {
                    return Some((v.clone(), lx, ly));
                }
            }
        }
        let gr = s.graphics.lock().unwrap();
        gr.places.iter().rev().find_map(|p| {
            let v = gr.webs.get(&p.img)?;
            let (px, py) = (ox + p.col as f32 * r.cell_w, oy + (p.line as i64 - top) as f32 * r.cell_h);
            let (lx, ly) = (x as f32 - px, y as f32 - py);
            (lx >= 0.0 && ly >= 0.0 && lx < p.cols as f32 * r.cell_w && ly < p.rows as f32 * r.cell_h).then(|| (v.clone(), lx, ly))
        })
    }

    fn web_focused(&self) -> Option<std::sync::Arc<crate::web::View>> {
        let id = self.web_focus?;
        self.sess()?.graphics.lock().unwrap().webs.get(&id).cloned()
    }

    /// A key for the web area that has the keyboard; false: not one of these.
    fn web_key(&mut self, ev: &KeyEvent) -> bool {
        let Some(v) = self.web_focused() else {
            self.web_focus = None;
            return false;
        };
        let m = self.mods;
        let mods = u32::from(m.alt_key()) | u32::from(m.control_key()) << 1 | u32::from(m.shift_key()) << 3;
        // the address line (Ctrl+L): typed here, Enter goes there
        if let Some(a) = self.web_addr.as_mut() {
            match &ev.logical_key {
                Key::Named(NamedKey::Escape) => self.web_addr = None,
                Key::Named(NamedKey::Enter) => {
                    let to = crate::web::address(a);
                    self.web_addr = None;
                    v.navigate(&to);
                }
                Key::Named(NamedKey::Backspace) => {
                    a.pop();
                }
                Key::Named(NamedKey::Space) => a.push(' '),
                Key::Character(c) if m.control_key() && c.eq_ignore_ascii_case("u") => a.clear(),
                Key::Character(_) if !m.control_key() => a.push_str(ev.text.as_deref().unwrap_or("")),
                _ => {}
            }
            self.redraw();
            return true;
        }
        if let Key::Character(c) = &ev.logical_key {
            if m.control_key() && !m.shift_key() && c.eq_ignore_ascii_case("l") {
                self.web_addr = Some(v.url.lock().unwrap().clone());
                self.redraw();
                return true;
            }
            if m.control_key() && !m.shift_key() && c.eq_ignore_ascii_case("r") {
                v.reload();
                return true;
            }
        }
        match &ev.logical_key {
            // Esc gives the keyboard back to the terminal; Shift+Esc is Esc for the page
            Key::Named(NamedKey::Escape) if !m.shift_key() => {
                self.web_focus = None;
                self.redraw();
            }
            Key::Named(NamedKey::Escape) => v.key("Escape", 0),
            Key::Named(NamedKey::ArrowLeft) if m.alt_key() => v.back(),
            Key::Named(NamedKey::ArrowRight) if m.alt_key() => v.forward(),
            Key::Named(NamedKey::F5) => v.reload(),
            Key::Named(n) => {
                let name = match n {
                    NamedKey::Enter => "Enter",
                    NamedKey::Backspace => "Backspace",
                    NamedKey::Tab => "Tab",
                    NamedKey::Delete => "Delete",
                    NamedKey::ArrowLeft => "ArrowLeft",
                    NamedKey::ArrowRight => "ArrowRight",
                    NamedKey::ArrowUp => "ArrowUp",
                    NamedKey::ArrowDown => "ArrowDown",
                    NamedKey::Home => "Home",
                    NamedKey::End => "End",
                    NamedKey::PageUp => "PageUp",
                    NamedKey::PageDown => "PageDown",
                    NamedKey::Space => {
                        v.text(" ");
                        return true;
                    }
                    _ => return true,
                };
                v.key(name, mods);
            }
            Key::Character(c) => {
                if m.control_key() && m.shift_key() && c.eq_ignore_ascii_case("v") {
                    // the system clipboard into the page
                    v.text(&clipboard_get(false));
                } else if m.control_key() || m.alt_key() {
                    v.key(c.as_str(), mods);
                } else {
                    v.text(ev.text.as_deref().unwrap_or(c.as_str()));
                }
            }
            _ => {}
        }
        true
    }

    /// The focused pane's cell under a window position (0-based column, row).
    fn cell_at(&self, x: f64, y: f64) -> (usize, usize) {
        let (Some(g), Some(s), Some(f)) = (self.gpu.as_ref(), self.sess(), self.focus_rect()) else { return (0, 0) };
        let r = &g.renderer;
        let (ox, oy) = r.origin(f.x, f.y, f.w, f.h);
        let col = (((x as f32 - ox) / r.cell_w).max(0.0) as usize).min(s.grid.cols.saturating_sub(1));
        let row = (((y as f32 - oy) / r.cell_h).max(0.0) as usize).min(s.grid.rows.saturating_sub(1));
        (col, row)
    }

    /// Files dropped on the window: the pane under the drop takes them (a shell gets their
    /// paths, quoted; Neovim opens them).
    fn drop_files(&mut self, paths: Vec<std::path::PathBuf>, at: Option<(f64, f64)>) {
        if let (Some((x, y)), Some(area)) = (at, self.area()) {
            let hit = self.tabs.get(self.active).filter(|t| !t.zoom).and_then(|t| t.layout.at(x as f32, y as f32, area));
            if let Some(id) = hit {
                self.focus_pane(id);
            }
        }
        if let Some(n) = self.pane().and_then(|p| p.nvim.as_ref()) {
            for path in &paths {
                let p = path.display().to_string().replace('\\', "\\\\").replace(' ', "\\ ").replace('<', "<lt>");
                n.input(&format!("<Esc>:e {p}<CR>"));
            }
        } else {
            let words: Vec<String> = paths.iter().map(|p| shell_quote(&p.display().to_string())).collect();
            let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
            self.write(input::paste(&format!("{} ", words.join(" ")), bracketed));
        }
        if let Some(g) = &self.gpu {
            g.window.focus_window();
        }
        self.redraw();
    }

    /// Modifiers held, for Neovim's mouse input ("CS", "A", "").
    fn nvim_mods(&self) -> String {
        let m = self.mods;
        format!("{}{}{}", if m.control_key() { "C" } else { "" }, if m.alt_key() { "A" } else { "" }, if m.shift_key() { "S" } else { "" })
    }

    /// Neovim in a pane: attach when the shell announced a server, let go when it quit.
    fn nvim_check(&mut self, tab: usize, pi: usize) {
        let cell_h = self.gpu.as_ref().map(|g| g.renderer.cell_h).unwrap_or(16.0);
        let glide = self.cfg.smooth_scroll_ms;
        let proxy = self.proxy.clone();
        let p = &mut self.tabs[tab].panes[pi];
        if let Some(sock) = p.session.nvim_sock.lock().unwrap().take() {
            let id = p.id;
            let wake = Box::new(move || {
                let _ = proxy.send_event(ZEvent::Term(id, Event::Wakeup));
            });
            match crate::nvim::Nvim::connect(&sock, p.session.grid.cols, p.session.grid.rows, cell_h, glide, wake) {
                Ok(n) => p.nvim = Some(n),
                Err(e) => eprintln!("zash: could not reach nvim at {sock}: {e}"),
            }
        }
        let quit = p.nvim.as_ref().is_some_and(|n| n.state.lock().unwrap().closed);
        if quit {
            p.nvim = None;
        }
        let title = p.nvim.as_ref().and_then(|n| n.state.lock().unwrap().title.clone());
        if let Some(t) = title {
            if p.title != t {
                p.title = t;
                self.show_title();
            }
        }
    }

    /// Scrolls the focused pane's view; with smooth scrolling the move glides from where it was.
    fn scroll_view(&mut self, how: Scroll) {
        let Some(s) = self.sess() else { return };
        let rows = s.grid.rows as f32;
        let moved = {
            let mut t = s.term.lock();
            let before = t.grid().display_offset() as i32;
            t.scroll_display(how);
            t.grid().display_offset() as i32 - before
        };
        if moved != 0 && self.cfg.smooth_scroll_ms > 0.0 {
            let ch = self.gpu.as_ref().map(|g| g.renderer.cell_h).unwrap_or(16.0);
            self.settle_glide();
            // drawn from the old place (up by the lines it moved), easing to the new one;
            // never more than a screen away, so a long jump still lands quickly
            let max = rows * ch;
            self.glide = (self.glide - moved as f32 * ch).clamp(-max, max);
        }
        self.redraw();
    }

    /// Advances the glide to now (exponential: the same at any frame rate).
    fn settle_glide(&mut self) {
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.glide_at).as_secs_f32() * 1000.0;
        self.glide_at = now;
        let tau = (self.cfg.smooth_scroll_ms / 3.0).max(1.0);
        self.glide *= (-dt / tau).exp();
        if self.glide.abs() < 0.5 {
            self.glide = 0.0;
        }
    }

    /// Moves the keyboard to another pane of the shown tab (telling programs that ask about focus).
    fn focus_pane(&mut self, id: u64) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        if t.focus == id || !t.panes.iter().any(|p| p.id == id) {
            return;
        }
        let tell = |p: Option<&Pane>, on: bool| {
            if let Some(p) = p {
                if p.session.term.lock().mode().contains(TermMode::FOCUS_IN_OUT) {
                    p.session.write(if on { b"\x1b[I".to_vec() } else { b"\x1b[O".to_vec() });
                }
            }
        };
        tell(t.focused(), false);
        if let Some(n) = t.focused().and_then(|p| p.nvim.as_ref()) { n.focus(false); }
        t.focus = id;
        tell(t.focused(), true);
        if let Some(n) = t.focused().and_then(|p| p.nvim.as_ref()) { n.focus(true); }
        if t.zoom {
            t.zoom = false;
            self.sync_size();
        }
        self.search = None;
        self.show_title();
    }

    /// Does the program want the mouse? Shift keeps it for zash (selecting text), as everywhere.
    fn app_mouse(&self) -> bool {
        !self.mods.shift_key() && self.mode().intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
    }

    fn send_mouse(&self, button: u8, motion: bool, pressed: bool, cell: (usize, usize)) {
        let m = self.mods;
        let mods = Mods { ctrl: m.control_key(), alt: m.alt_key(), shift: m.shift_key() };
        let sgr = self.mode().contains(TermMode::SGR_MOUSE);
        if let Some(s) = self.sess() {
            s.write(input::mouse(button, motion, pressed, cell.0, cell.1, mods, sgr));
        }
    }

    fn search_key(&mut self, ev: &KeyEvent, mods: Mods) {
        use alacritty_terminal::index::Direction;
        // fields directly (not self.sess()): the search is borrowed mutably at the same time
        let sess = self.tabs.get(self.active).and_then(|t| t.panes.iter().find(|p| p.id == t.focus)).map(|p| &p.session);
        let (Some(s), Some(sess)) = (self.search.as_mut(), sess) else { return };
        let mut t = sess.term.lock();
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => {
                s.close(&mut t);
                drop(t);
                self.search = None;
                self.redraw();
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                let mut q = s.query.clone();
                q.pop();
                s.set_query(&mut t, &q);
            }
            Key::Named(NamedKey::Enter) => s.step(&mut t, if mods.shift { Direction::Right } else { Direction::Left }),
            _ => {
                if let Some(text) = ev.text.as_deref().filter(|x| !x.chars().any(|c| c.is_control())) {
                    let q = format!("{}{text}", s.query);
                    s.set_query(&mut t, &q);
                }
            }
        }
        drop(t);
        self.redraw();
    }

    /// The link under a window position: a real hyperlink (OSC 8) or a URL in the text.
    fn link_at(&self, x: f64, y: f64) -> Option<String> {
        let (p, _) = self.point_at(x, y)?;
        let s = self.sess()?;
        let t = s.term.lock();
        if let Some(h) = t.grid()[p].hyperlink() {
            return Some(h.uri().to_string());
        }
        let cols = t.grid().columns();
        let line = t.bounds_to_string(Point::new(p.line, Column(0)), Point::new(p.line, Column(cols.saturating_sub(1))));
        crate::links::url_at(&line, p.column.0)
    }

    /// Scrolls so the previous (-1) or next (+1) prompt is at the top.
    fn jump_prompt(&mut self, dir: i32) {
        let Some(s) = self.sess() else { return };
        let prompts: Vec<usize> = crate::render::prompts(&s.marks.lock().unwrap()).into_iter().map(|p| p.0).collect();
        let mut t = s.term.lock();
        let hist = t.grid().history_size();
        let top = hist - t.grid().display_offset();
        let target = if dir < 0 { prompts.iter().rev().find(|&&l| l < top).copied() } else { prompts.iter().find(|&&l| l > top).copied() };
        match target {
            // display offset = how far the top is above the live screen
            Some(l) if l <= hist => t.scroll_display(Scroll::Delta(top as i32 - l as i32)),
            _ if dir > 0 => t.scroll_display(Scroll::Bottom),
            _ => {}
        }
        drop(t);
        self.redraw();
    }

    /// The text of the last finished command's output (between its C and D marks).
    fn last_output(&self) -> Option<String> {
        let s = self.sess()?;
        let marks = s.marks.lock().unwrap().clone();
        let d = marks.iter().rposition(|m| m.kind == 'D')?;
        let c = marks[..d].iter().rposition(|m| m.kind == 'C')?;
        let (from, to) = (marks[c].line, marks[d].line);
        if to <= from {
            return Some(String::new());
        }
        let t = s.term.lock();
        let hist = t.grid().history_size() as i32;
        let cols = t.grid().columns();
        let start = Point::new(Line(from as i32 - hist), Column(0));
        let end = Point::new(Line(to as i32 - 1 - hist), Column(cols.saturating_sub(1)));
        Some(t.bounds_to_string(start, end).trim_end().to_string())
    }

    fn spawn_pane(&mut self, cmd: Option<Vec<String>>, dir: Option<std::path::PathBuf>, grid: Grid) -> std::io::Result<Pane> {
        let id = self.next_id;
        self.next_id += 1;
        let session = Session::spawn(cmd, dir, grid, self.cfg.scrollback, Sink::Window(self.proxy.clone(), id))?;
        Ok(Pane { id, session, title: "zash".into(), nvim: None })
    }

    /// The tab bar shows from the second tab: the space below it changes.
    fn fit_tab_bar(&mut self) {
        let n = self.tabs.len();
        if let Some(g) = self.gpu.as_mut() {
            g.renderer.top = if n > 1 { g.renderer.tab_bar_h() } else { 0.0 };
        }
    }

    fn new_tab(&mut self, cmd: Option<Vec<String>>, dir: Option<std::path::PathBuf>) -> std::io::Result<()> {
        // make room for the tab bar before sizing the grid
        if let Some(g) = self.gpu.as_mut() {
            if !self.tabs.is_empty() {
                g.renderer.top = g.renderer.tab_bar_h();
            }
        }
        let grid = self.grid().ok_or_else(|| std::io::Error::other("no window"))?;
        let pane = self.spawn_pane(cmd, dir, grid)?;
        let id = pane.id;
        self.tabs.push(Tab { panes: vec![pane], layout: Layout::Leaf(id), focus: id, zoom: false });
        self.last_active = self.active;
        self.active = self.tabs.len() - 1;
        self.search = None;
        self.sync_size();
        self.show_title();
        Ok(())
    }

    /// Splits the focused pane; the new one opens in the same folder and gets the keyboard.
    fn split(&mut self, side_by_side: bool) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let (at, dir) = (t.focus, t.focused().and_then(|p| p.session.cwd()));
        let Some(grid) = self.focus_rect().and_then(|r| self.grid_in(r)) else { return };
        match self.spawn_pane(None, dir, grid) {
            Ok(p) => {
                let id = p.id;
                let t = &mut self.tabs[self.active];
                t.zoom = false;
                t.panes.push(p);
                t.layout.split(at, id, side_by_side);
                self.sync_size();
                self.focus_pane(id);
            }
            Err(e) => eprintln!("zash: no new pane: {e}"),
        }
    }

    /// A pane's program ended: the pane goes, its neighbour takes the space; the last pane closes the tab.
    fn close_pane(&mut self, ti: usize, id: u64, el: &ActiveEventLoop) {
        let t = &mut self.tabs[ti];
        t.panes.retain(|p| p.id != id);
        let layout = std::mem::replace(&mut t.layout, Layout::Leaf(0));
        match layout.remove(id) {
            Some(l) => {
                if t.focus == id {
                    t.focus = l.ids()[0];
                }
                t.layout = l;
                t.zoom = false;
                self.sync_size();
                self.show_title();
            }
            None => self.close_tab(ti, el),
        }
    }

    fn close_tab(&mut self, i: usize, el: &ActiveEventLoop) {
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            el.exit();
            return;
        }
        if i < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.last_active = self.last_active.min(self.tabs.len() - 1);
        self.fit_tab_bar();
        self.search = None;
        self.sync_size();
        self.show_title();
    }

    fn show_tab(&mut self, i: usize) {
        if i < self.tabs.len() && i != self.active {
            self.last_active = self.active;
            self.active = i;
            self.search = None;
            self.show_title();
        }
    }

    fn switch_tab(&mut self, by: i32) {
        let n = self.tabs.len() as i32;
        if n < 2 {
            return;
        }
        self.show_tab(((self.active as i32 + by).rem_euclid(n)) as usize);
    }

    fn show_title(&self) {
        if let (Some(g), Some(p)) = (&self.gpu, self.pane()) {
            g.window.set_title(&p.title);
        }
        self.redraw();
    }

    /// Hang up on programs: they exit, and their exit closes the pane.
    fn hang_up(&self, pids: Vec<u32>) {
        for pid in pids {
            unsafe { libc::kill(pid as i32, libc::SIGHUP) };
        }
    }

    /// Does what a tmux key binding asked for.
    fn run(&mut self, a: &Action) {
        let area = self.area();
        match a {
            Action::Split { side_by_side } => self.split(*side_by_side),
            Action::Focus(d) => {
                let to = self.tabs.get(self.active).zip(area).and_then(|(t, ar)| t.layout.neighbour(t.focus, *d, ar));
                if let Some(id) = to {
                    self.focus_pane(id);
                }
            }
            Action::FocusNext => {
                if let Some(t) = self.tabs.get(self.active) {
                    let ids = t.layout.ids();
                    let i = ids.iter().position(|x| *x == t.focus).unwrap_or(0);
                    let next = ids[(i + 1) % ids.len()];
                    // tmux's "next pane in zoomed mode" binding zooms right after: keep it zoomed
                    let zoomed = t.zoom;
                    self.focus_pane(next);
                    if zoomed {
                        self.tabs[self.active].zoom = true;
                        self.sync_size();
                    }
                }
            }
            Action::Resize(d, n) => {
                let cell = self.gpu.as_ref().map(|g| if matches!(d, crate::keys::Dir::Left | crate::keys::Dir::Right) { g.renderer.cell_w } else { g.renderer.cell_h });
                if let (Some(ar), Some(c), Some(t)) = (area, cell, self.tabs.get_mut(self.active)) {
                    let f = t.focus;
                    if t.layout.resize(f, *d, c * *n as f32, ar) {
                        self.sync_size();
                    }
                }
            }
            Action::Zoom => {
                if let Some(t) = self.tabs.get_mut(self.active) {
                    if t.panes.len() > 1 {
                        t.zoom = !t.zoom;
                        self.sync_size();
                    }
                }
            }
            Action::ClosePane => {
                let pid = self.pane().map(|p| p.session.pid);
                self.hang_up(pid.into_iter().collect());
            }
            Action::CloseTab => {
                let pids = self.tabs.get(self.active).map(|t| t.panes.iter().map(|p| p.session.pid).collect()).unwrap_or_default();
                self.hang_up(pids);
            }
            Action::NewTab => {
                let dir = self.sess().and_then(|s| s.cwd());
                if let Err(e) = self.new_tab(None, dir) {
                    eprintln!("zash: no new tab: {e}");
                }
            }
            Action::NextTab => self.switch_tab(1),
            Action::PrevTab => self.switch_tab(-1),
            Action::LastTab => self.show_tab(self.last_active),
            Action::SendPrefix => {
                // the prefix itself, to a program inside (a nested tmux)
                let p = self.keys.prefix.clone();
                let key = match p.key.as_str() {
                    "Space" => input::Key::Text(" "),
                    k => input::Key::Text(k),
                };
                let mods = Mods { ctrl: p.ctrl, alt: p.alt, shift: false };
                let app_cursor = self.mode().contains(TermMode::APP_CURSOR);
                if let Some(b) = input::encode(&key, mods, app_cursor) {
                    self.write(b);
                }
            }
            Action::Search => {
                self.search = Some(crate::search::Search::default());
            }
            Action::Paste => {
                let text = clipboard_get(false);
                let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
                self.write(input::paste(&text, bracketed));
            }
            Action::Seq(list) => {
                for a in list.clone() {
                    self.run(&a);
                }
            }
        }
        self.redraw();
    }

    /// A key as the tmux config names it; None for a lone modifier.
    fn chord(ev: &KeyEvent, mods: Mods) -> Option<Chord> {
        let key = match &ev.logical_key {
            Key::Named(n) => match n {
                NamedKey::Space => "Space",
                NamedKey::Escape => "Escape",
                NamedKey::Enter => "Enter",
                NamedKey::Tab => "Tab",
                NamedKey::Backspace => "Backspace",
                NamedKey::ArrowUp => "Up",
                NamedKey::ArrowDown => "Down",
                NamedKey::ArrowLeft => "Left",
                NamedKey::ArrowRight => "Right",
                NamedKey::PageUp => "PageUp",
                NamedKey::PageDown => "PageDown",
                _ => return None,
            }
            .to_string(),
            Key::Character(c) => {
                if mods.ctrl { c.to_lowercase() } else { c.to_string() }
            }
            _ => return None,
        };
        Some(Chord { ctrl: mods.ctrl, alt: mods.alt, key })
    }

    /// Pane and tab keys (prefix, `-n` and repeat bindings). True when the key was used.
    fn tmux_key(&mut self, ev: &KeyEvent, mods: Mods) -> bool {
        // keys the program in front claimed (OSC 7733;keys) are its own: zash steps aside;
        // tmux sends no claims, so a multiplexer in front counts as claiming all
        if !self.prefix {
            let name = Self::chord(ev, mods).map(|c| format!("{}{}{}", if c.ctrl { "C-" } else { "" }, if c.alt { "M-" } else { "" }, c.key));
            let claimed = self.sess().is_some_and(|s| {
                let c = s.claims.lock().unwrap();
                c.all || name.as_ref().is_some_and(|n| c.keys.contains(n))
            });
            if claimed || self.sess().and_then(|s| s.foreground()).is_some_and(|f| f == "zeit" || f.starts_with("tmux")) {
                return false;
            }
        }
        let Some(chord) = Self::chord(ev, mods) else { return self.prefix }; // a modifier alone keeps the prefix waiting
        let now = std::time::Instant::now();
        if self.prefix {
            self.prefix = false;
            if let Some(b) = self.keys.table.get(&chord).cloned() {
                self.repeat_until = b.repeat.then(|| now + std::time::Duration::from_millis(600));
                self.run(&b.action);
            }
            self.redraw();
            // an unbound key after the prefix does nothing (as in tmux)
            return true;
        }
        if let Some(until) = self.repeat_until.take() {
            if now < until {
                if let Some(b) = self.keys.table.get(&chord).cloned().filter(|b| b.repeat) {
                    self.repeat_until = Some(now + std::time::Duration::from_millis(600));
                    self.run(&b.action);
                    return true;
                }
            }
        }
        if chord == self.keys.prefix {
            self.prefix = true;
            self.redraw();
            return true;
        }
        if let Some(b) = self.keys.root.get(&chord).cloned() {
            self.run(&b.action);
            return true;
        }
        false
    }

    fn set_font(&mut self, pt: f32) {
        self.font_pt = pt.clamp(5.0, 72.0);
        if let Some(g) = self.gpu.as_mut() {
            let scale = g.window.scale_factor() as f32;
            g.renderer.set_font(self.font_pt, self.cfg.line_height, self.cfg.padding, scale);
        }
        self.sync_size();
    }

    fn key(&mut self, ev: &KeyEvent) {
        let m = self.mods;
        let mods = Mods { ctrl: m.control_key(), alt: m.alt_key(), shift: m.shift_key() };
        if self.web_focus.is_some() && self.web_key(ev) {
            return;
        }
        if self.search.is_some() {
            return self.search_key(ev, mods);
        }
        // pane and tab keys from the tmux config (prefix, -n, repeat)
        if self.tmux_key(ev, mods) {
            return;
        }
        // zash's own keys first
        if mods.ctrl && mods.shift {
            if let Key::Character(c) = &ev.logical_key {
                match c.to_lowercase().as_str() {
                    "c" => {
                        if let Some(s) = self.sess() {
                            if let Some(t) = s.term.lock().selection_to_string() {
                                clipboard_set(&t, false);
                            }
                        }
                        return;
                    }
                    "t" => {
                        // a new tab, in the folder the current one is in
                        let dir = self.sess().and_then(|s| s.cwd());
                        if let Err(e) = self.new_tab(None, dir) {
                            eprintln!("zash: no new tab: {e}");
                        }
                        return;
                    }
                    "w" => {
                        // hang up on the focused pane's program; its exit closes the pane (the last one, the tab)
                        self.run(&Action::ClosePane);
                        return;
                    }
                    "f" => {
                        self.search = Some(crate::search::Search::default());
                        self.redraw();
                        return;
                    }
                    "o" => {
                        // the last command's output, to the clipboard
                        if let Some(text) = self.last_output() {
                            clipboard_set(&text, false);
                        }
                        return;
                    }
                    "v" => {
                        let text = clipboard_get(false);
                        if let Some(n) = self.pane().and_then(|p| p.nvim.as_ref()) {
                            n.paste(&text);
                            return;
                        }
                        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
                        self.write(input::paste(&text, bracketed));
                        return;
                    }
                    _ => {}
                }
            }
        }
        // tabs: Ctrl+Tab / Ctrl+Shift+Tab, Ctrl+PageDown / Ctrl+PageUp
        if mods.ctrl && !mods.alt {
            let by = match ev.logical_key {
                Key::Named(NamedKey::Tab) => Some(if mods.shift { -1 } else { 1 }),
                Key::Named(NamedKey::PageDown) if !mods.shift => Some(1),
                Key::Named(NamedKey::PageUp) if !mods.shift => Some(-1),
                _ => None,
            };
            if let Some(b) = by {
                self.switch_tab(b);
                return;
            }
        }
        if mods.ctrl && mods.shift {
            let dir = match ev.logical_key {
                Key::Named(NamedKey::ArrowUp) => Some(-1),
                Key::Named(NamedKey::ArrowDown) => Some(1),
                _ => None,
            };
            if let Some(d) = dir {
                self.jump_prompt(d);
                return;
            }
        }
        if mods.ctrl && !mods.shift && !mods.alt {
            if let Key::Character(c) = &ev.logical_key {
                match c.as_str() {
                    "=" | "+" => return self.set_font(self.font_pt + 1.0),
                    "-" => return self.set_font(self.font_pt - 1.0),
                    "0" => return self.set_font(self.cfg.font_size_pt),
                    _ => {}
                }
            }
        }
        // Neovim in the pane: every other key goes to it, in its notation
        if let Some(n) = self.pane().and_then(|p| p.nvim.as_ref()) {
            let named = match &ev.logical_key {
                Key::Named(k) => Some(match k {
                    NamedKey::Enter => "CR",
                    NamedKey::Tab => "Tab",
                    NamedKey::Backspace => "BS",
                    NamedKey::Escape => "Esc",
                    NamedKey::Space => "Space",
                    NamedKey::ArrowUp => "Up",
                    NamedKey::ArrowDown => "Down",
                    NamedKey::ArrowLeft => "Left",
                    NamedKey::ArrowRight => "Right",
                    NamedKey::Home => "Home",
                    NamedKey::End => "End",
                    NamedKey::PageUp => "PageUp",
                    NamedKey::PageDown => "PageDown",
                    NamedKey::Insert => "Insert",
                    NamedKey::Delete => "Del",
                    NamedKey::F1 => "F1", NamedKey::F2 => "F2", NamedKey::F3 => "F3", NamedKey::F4 => "F4",
                    NamedKey::F5 => "F5", NamedKey::F6 => "F6", NamedKey::F7 => "F7", NamedKey::F8 => "F8",
                    NamedKey::F9 => "F9", NamedKey::F10 => "F10", NamedKey::F11 => "F11", NamedKey::F12 => "F12",
                    _ => return,
                }),
                _ => None,
            };
            // Space without modifiers is just a space; with Ctrl it is <C-Space>
            let named = if named == Some("Space") && !mods.ctrl && !mods.alt { None } else { named };
            let text = match &ev.logical_key {
                Key::Character(c) if mods.ctrl || mods.alt => Some(c.as_str()),
                Key::Named(NamedKey::Space) => Some(" "),
                _ => ev.text.as_deref(),
            };
            // letters carry their shift already (A, not <S-a>); named keys say it (<S-Tab>)
            if let Some(k) = crate::nvim::notation(named, text, mods.ctrl, mods.alt, mods.shift && named.is_some()) {
                n.input(&k);
            }
            return;
        }
        if mods.shift && !mods.ctrl {
            let page = match ev.logical_key {
                Key::Named(NamedKey::PageUp) => Some(Scroll::PageUp),
                Key::Named(NamedKey::PageDown) => Some(Scroll::PageDown),
                _ => None,
            };
            if let (Some(p), true) = (page, self.sess().is_some()) {
                if !self.mode().contains(TermMode::ALT_SCREEN) {
                    self.scroll_view(p);
                    return;
                }
            }
        }

        let app_cursor = self.mode().contains(TermMode::APP_CURSOR);
        let named = |k: &NamedKey| -> Option<Named> {
            Some(match k {
                NamedKey::Enter => Named::Enter,
                NamedKey::Tab => Named::Tab,
                NamedKey::Backspace => Named::Backspace,
                NamedKey::Escape => Named::Escape,
                NamedKey::ArrowUp => Named::Up,
                NamedKey::ArrowDown => Named::Down,
                NamedKey::ArrowLeft => Named::Left,
                NamedKey::ArrowRight => Named::Right,
                NamedKey::Home => Named::Home,
                NamedKey::End => Named::End,
                NamedKey::PageUp => Named::PageUp,
                NamedKey::PageDown => Named::PageDown,
                NamedKey::Insert => Named::Insert,
                NamedKey::Delete => Named::Delete,
                NamedKey::F1 => Named::F(1),
                NamedKey::F2 => Named::F(2),
                NamedKey::F3 => Named::F(3),
                NamedKey::F4 => Named::F(4),
                NamedKey::F5 => Named::F(5),
                NamedKey::F6 => Named::F(6),
                NamedKey::F7 => Named::F(7),
                NamedKey::F8 => Named::F(8),
                NamedKey::F9 => Named::F(9),
                NamedKey::F10 => Named::F(10),
                NamedKey::F11 => Named::F(11),
                NamedKey::F12 => Named::F(12),
                _ => return None,
            })
        };
        let bytes = match &ev.logical_key {
            Key::Named(NamedKey::Space) => input::encode(&input::Key::Text(" "), mods, app_cursor),
            Key::Named(k) => named(k).and_then(|n| input::encode(&input::Key::Named(n), mods, app_cursor)),
            Key::Character(c) => {
                // with Ctrl the layout's character matters (Ctrl+C), otherwise the typed text
                let text = if mods.ctrl { c.as_str() } else { ev.text.as_deref().unwrap_or(c.as_str()) };
                let mods = Mods { shift: false, ..mods };
                input::encode(&input::Key::Text(text), mods, app_cursor)
            }
            _ => ev.text.as_deref().and_then(|t| input::encode(&input::Key::Text(t), Mods::default(), app_cursor)),
        };
        if let Some(b) = bytes {
            self.write(b);
        }
    }

    fn render(&mut self) {
        self.settle_glide();
        // the touchpad's part of a line not scrolled yet shows too: pixel-exact scrolling
        let frac_px = {
            let ch = self.gpu.as_ref().map(|g| g.renderer.cell_h).unwrap_or(16.0);
            let free = self.sess().map(|s| {
                let t = s.term.lock();
                let g = t.grid();
                let normal = !t.mode().contains(TermMode::ALT_SCREEN);
                (normal && g.display_offset() < g.history_size(), normal && g.display_offset() > 0)
            });
            match free {
                Some((up, _)) if self.scroll_acc > 0.0 && up && !self.app_mouse() && self.cfg.smooth_scroll_ms > 0.0 => (self.scroll_acc as f32) * ch,
                Some((_, down)) if self.scroll_acc < 0.0 && down && !self.app_mouse() && self.cfg.smooth_scroll_ms > 0.0 => (self.scroll_acc as f32) * ch,
                _ => 0.0,
            }
        };
        let scroll_px = self.glide + frac_px;
        let cell_h = self.gpu.as_ref().map(|g| g.renderer.cell_h).unwrap_or(16.0);
        let Some(tab) = self.tabs.get(self.active) else { return };
        // web areas follow their cells' size in pixels (the window, the font), at the screen's scale
        if let Some(g) = self.gpu.as_ref() {
            crate::web::set_scale(g.window.scale_factor() as f32);
            let (cw, chh) = (g.renderer.cell_w, g.renderer.cell_h);
            // a web area that asked for the keyboard (zash --web) gets it
            if let Some(p) = tab.panes.iter().find(|p| p.id == tab.focus) {
                if let Some(id) = p.session.graphics.lock().unwrap().autofocus.take() {
                    self.web_focus = Some(id);
                }
                // a multiplexer moved its focus onto a web pane (or away from it)
                for (id, on) in std::mem::take(&mut p.session.graphics.lock().unwrap().pin_focus) {
                    if on {
                        self.web_focus = Some(id);
                    } else if self.web_focus == Some(id) {
                        self.web_focus = None;
                    }
                }
            }
            // the page's title in the window's, while it has the keyboard
            if let Some(v) = self.web_focused() {
                let t = v.title.lock().unwrap().clone();
                if !t.is_empty() && t != self.web_title {
                    g.window.set_title(&format!("{t} — zash"));
                    self.web_title = t;
                }
            }
            let tab = &self.tabs[self.active];
            for p in &tab.panes {
                let gr = p.session.graphics.lock().unwrap();
                for pl in &gr.places {
                    if let Some(v) = gr.webs.get(&pl.img) {
                        v.resize((pl.cols as f32 * cw) as u32, (pl.rows as f32 * chh) as u32);
                    }
                }
                for (id, (rect, _)) in &gr.pinned {
                    if let (Some((_, _, w, h)), Some(v)) = (rect, gr.webs.get(id)) {
                        v.resize((*w as f32 * cw) as u32, (*h as f32 * chh) as u32);
                    }
                }
            }
        }
        // every pane of the shown tab, copied out of its terminal
        let rects = self.pane_rects(self.active);
        let mut frames: Vec<(u64, Rect, Frame)> = Vec::new();
        let mut nvim_moving = false;
        for (id, r) in rects {
            let Some(p) = tab.panes.iter().find(|p| p.id == id) else { continue };
            let s = &p.session;
            // Neovim in the pane: its own screen, drawn by zash
            if let Some(n) = &p.nvim {
                let mut st = n.state.lock().unwrap();
                if st.settle() {
                    nvim_moving = true;
                }
                let mut frame = st.frame(&self.cfg.palette);
                if id == tab.focus {
                    frame.tabs = self.tabs.iter().enumerate().map(|(i, t)| (Self::tab_title(t), i == self.active)).collect();
                    if self.prefix {
                        frame.bar = Some(self.prefix_hint());
                    }
                } else {
                    frame.cursor = None;
                }
                frames.push((id, r, frame));
                continue;
            }
            let mut frame = {
                let t = s.term.lock();
                let marks = s.marks.lock().unwrap();
                let pal = if s.reverse.load(std::sync::atomic::Ordering::Relaxed) { self.cfg.palette.reversed() } else { self.cfg.palette.clone() };
                let f = Frame::extract(&t, &pal).with_prompts(&t, &marks).with_rows(&t, &s.ui_rows.lock().unwrap()).with_images(&t, &s.graphics.lock().unwrap());
                if id == tab.focus { f.with_scroll(&t, &pal, scroll_px, cell_h) } else { f }
            };
            if id == tab.focus {
                // a popup only while the cursor is still on its line
                let here = { let t = s.term.lock(); t.grid().history_size() + t.grid().cursor.point.line.0.max(0) as usize };
                frame.popup = s.popup.lock().unwrap().clone().filter(|p| p.line.is_none_or(|l| l == here));
                frame.popup_look = self.cfg.popup.with(&s.popup_style.lock().unwrap());
                if let Some(a) = &self.web_addr {
                    frame.bar = Some(format!("\u{f0ac}  {a}\u{258f}"));
                } else if let Some(q) = &self.search {
                    frame.bar = Some(q.bar());
                } else if self.prefix {
                    frame.bar = Some(self.prefix_hint());
                }
                // a tab without a real title shows the name of the folder its shell is in
                frame.tabs = self.tabs.iter().enumerate().map(|(i, t)| (Self::tab_title(t), i == self.active)).collect();
            } else {
                // only the focused pane shows a cursor
                frame.cursor = None;
            }
            frames.push((id, r, frame));
        }
        let focus = tab.focus;
        let Some(g) = self.gpu.as_mut() else { return };
        let tex = match g.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Suboptimal(_) | wgpu::CurrentSurfaceTexture::Lost => {
                g.surface.configure(&g.device, &g.config);
                g.window.request_redraw();
                return;
            }
            _ => {
                g.window.request_redraw();
                return;
            }
        };
        let view = tex.texture.create_view(&Default::default());
        g.window.pre_present_notify();
        let panes: Vec<PaneView> = frames.iter().map(|(id, r, f)| PaneView { id: *id, x: r.x, y: r.y, w: r.w, h: r.h, frame: f, focused: *id == focus }).collect();
        let animating = g.renderer.render(&g.device, &g.queue, &view, g.config.width, g.config.height, &panes);
        g.queue.present(tex);
        // another frame only while the cursor is still gliding: idle stays idle
        if animating || self.glide != 0.0 || nvim_moving {
            g.window.request_redraw();
        }
    }

    fn tab_title(t: &Tab) -> String {
        let Some(p) = t.focused() else { return "zash".into() };
        if p.title == "zash" || p.title.is_empty() {
            p.session.cwd().and_then(|d| d.file_name().map(|n| n.to_string_lossy().to_string())).unwrap_or_else(|| "zash".into())
        } else {
            p.title.clone()
        }
    }

    /// What the prefix can do next, shown while it waits (the keys from the user's own config).
    fn prefix_hint(&self) -> String {
        let find = |want: &dyn Fn(&Action) -> bool| {
            let mut ks: Vec<&Chord> = self.keys.table.iter().filter(|(_, b)| want(&b.action)).map(|(k, _)| k).collect();
            ks.sort_by(|a, b| a.key.cmp(&b.key));
            ks.first().map(|c| if c.ctrl { format!("^{}", c.key) } else { c.key.clone() })
        };
        let mut parts = Vec::new();
        if let Some(k) = find(&|a| *a == Action::Split { side_by_side: true }) { parts.push(format!("{k} split │")); }
        if let Some(k) = find(&|a| *a == Action::Split { side_by_side: false }) { parts.push(format!("{k} split ─")); }
        if let Some(k) = find(&|a| matches!(a, Action::Focus(_))) { parts.push(format!("{k}… move")); }
        if let Some(k) = find(&|a| *a == Action::Zoom) { parts.push(format!("{k} zoom")); }
        if let Some(k) = find(&|a| *a == Action::ClosePane) { parts.push(format!("{k} close")); }
        if let Some(k) = find(&|a| *a == Action::NewTab) { parts.push(format!("{k} tab")); }
        format!("prefix  {}", parts.join("  ·  "))
    }
}

impl ApplicationHandler<ZEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let mut attrs = Window::default_attributes().with_title("zash").with_transparent(self.cfg.opacity < 1.0).with_inner_size(winit::dpi::LogicalSize::new(960.0, 600.0));
        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attrs = attrs.with_name("zash", "zash");
        }
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("zash: no window: {e}");
                el.exit();
                return;
            }
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(el.owned_display_handle())));
        let surface = instance.create_surface(window.clone()).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            // the integrated GPU is plenty for text and draws less power
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .expect("adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) { wgpu::PresentMode::Mailbox } else { wgpu::PresentMode::Fifo };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            // translucent background needs a compositor that blends; else opaque
            alpha_mode: if self.cfg.opacity < 1.0 && caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
                wgpu::CompositeAlphaMode::PreMultiplied
            } else {
                caps.alpha_modes[0]
            },
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);
        let renderer = Renderer::new(&device, &queue, format, &self.cfg, window.scale_factor() as f32);
        self.gpu = Some(Gpu { surface, device, queue, config, renderer, window });
        let grid = self.grid().unwrap();
        let _ = grid;
        let (cmd, dir) = (self.cmd.take(), self.workdir.take());
        if let Err(e) = self.new_tab(cmd, dir) {
            eprintln!("zash: could not start the shell: {e}");
            el.exit();
        }
        // test hook: tmux commands run at start, `;`-separated (checking splits without key presses)
        if let Ok(cmds) = std::env::var("ZASH_TEST_RUN") {
            for c in cmds.split(';') {
                if let Some(a) = crate::keys::action(c.trim()) {
                    self.run(&a);
                }
            }
        }
        // test hook: open more tabs at start (checking the tab bar without key presses)
        if let Some(n) = std::env::var("ZASH_TEST_TABS").ok().and_then(|v| v.parse::<usize>().ok()) {
            for _ in 1..n {
                let _ = self.new_tab(None, None);
            }
        }
        // files dropped on the window: winit has no drag and drop on Wayland, zash listens itself
        if let Some(g) = &self.gpu {
            use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
            if let Ok(RawDisplayHandle::Wayland(h)) = g.window.display_handle().map(|d| d.as_raw()) {
                let proxy = self.proxy.clone();
                let on_drop: crate::dnd::OnDrop = Box::new(move |paths, x, y| {
                    let _ = proxy.send_event(ZEvent::Drop(paths, x, y));
                });
                if let Err(e) = crate::dnd::listen(h.display.as_ptr(), on_drop) {
                    eprintln!("zash: no drag and drop: {e}");
                }
            }
        }
        let mut watched = vec![self.cfg.colors_file.clone()];
        watched.extend(self.cfg.glass_file.clone());
        watch_theme(self.proxy.clone(), watched);
        // the settings: what can change in a running window does (all but the font family)
        let mut settings = vec![crate::config::Config::file_path()];
        if let Some(n) = std::env::var("ZASH_PRESET").ok().filter(|n| !n.is_empty()) {
            settings.push(crate::config::Config::preset_path(&n));
        }
        watch_files(self.proxy.clone(), settings, || ZEvent::Settings);
        self.redraw();
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: ZEvent) {
        let (id, ev) = match ev {
            ZEvent::Drop(paths, x, y) => {
                let scale = self.gpu.as_ref().map(|g| g.window.scale_factor()).unwrap_or(1.0);
                self.drop_files(paths, Some((x * scale, y * scale)));
                return;
            }
            ZEvent::Settings => {
                let new = crate::config::Config::load();
                let font = (new.font_size_pt, new.line_height, new.padding) != (self.cfg.font_size_pt, self.cfg.line_height, self.cfg.padding);
                // the palette stays tinct's; a new font family takes a new window
                let keep = (self.cfg.palette.clone(), self.cfg.font_family.clone());
                self.cfg = crate::config::Config { palette: keep.0, font_family: keep.1, ..new };
                // the glass setting (the shell's Glass panel) keeps its say over the opacity
                if let Some(o) = self.cfg.glass_file.as_ref().and_then(|g| crate::config::Config::read_glass(g)) {
                    self.cfg.opacity = o;
                }
                if let Some(g) = self.gpu.as_mut() {
                    g.renderer.opacity = self.cfg.opacity;
                    g.renderer.cursor_ms = self.cfg.cursor_ms;
                    g.renderer.cursor_trail = self.cfg.cursor_trail;
                    g.renderer.invalidate();
                }
                if font {
                    self.set_font(self.cfg.font_size_pt);
                }
                self.redraw();
                return;
            }
            ZEvent::Theme => {
                if let Ok(s) = std::fs::read_to_string(&self.cfg.colors_file) {
                    self.cfg.palette = crate::config::Palette::from_tinct_lua(&s);
                }
                if let Some(o) = self.cfg.glass_file.as_ref().and_then(|g| crate::config::Config::read_glass(g)) {
                    self.cfg.opacity = o;
                    if let Some(g) = self.gpu.as_mut() {
                        g.renderer.opacity = o;
                    }
                }
                if let Some(g) = self.gpu.as_mut() {
                    g.renderer.invalidate();
                }
                self.redraw();
                return;
            }
            ZEvent::Term(id, e) => (id, e),
        };
        // the tab and pane this event belongs to (they may have closed meanwhile)
        let Some(tab) = self.tabs.iter().position(|t| t.panes.iter().any(|p| p.id == id)) else { return };
        let Some(pi) = self.tabs[tab].panes.iter().position(|p| p.id == id) else { return };
        let shown = tab == self.active;
        match ev {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => {
                self.nvim_check(tab, pi);
                if shown {
                    self.redraw();
                }
            }
            Event::Title(t) => {
                self.tabs[tab].panes[pi].title = t;
                self.show_title();
            }
            Event::ResetTitle => {
                self.tabs[tab].panes[pi].title = "zash".into();
                self.show_title();
            }
            Event::PtyWrite(s) => {
                if let Some(sess) = Some(&self.tabs[tab].panes[pi].session) {
                    sess.write(s.into_bytes());
                }
            }
            Event::ClipboardStore(ty, text) => clipboard_set(&text, matches!(ty, alacritty_terminal::term::ClipboardType::Selection)),
            Event::ClipboardLoad(ty, f) => {
                let text = clipboard_get(matches!(ty, alacritty_terminal::term::ClipboardType::Selection));
                if let Some(s) = Some(&self.tabs[tab].panes[pi].session) {
                    s.write(f(&text).into_bytes());
                }
            }
            Event::ColorRequest(i, f) => {
                if let Some(s) = Some(&self.tabs[tab].panes[pi].session) {
                    let p = &self.cfg.palette;
                    let c = match i {
                        256 => p.fg,
                        257 => p.bg,
                        258 => p.cursor_bg,
                        i if i < 256 => p.indexed(i as u8),
                        _ => p.fg,
                    };
                    s.write(f(alacritty_terminal::vte::ansi::Rgb { r: c.0, g: c.1, b: c.2 }).into_bytes());
                }
            }
            Event::TextAreaSizeRequest(f) => {
                if let Some(s) = Some(&self.tabs[tab].panes[pi].session) {
                    let g = s.grid;
                    s.write(f(WindowSize { num_lines: g.rows as u16, num_cols: g.cols as u16, cell_width: g.cell_w as u16, cell_height: g.cell_h as u16 }).into_bytes());
                }
            }
            Event::ChildExit(_) | Event::Exit => self.close_pane(tab, id, el),
            Event::Bell => {}
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => {
                if let Some(g) = self.gpu.as_mut() {
                    g.config.width = size.width.max(1);
                    g.config.height = size.height.max(1);
                    g.surface.configure(&g.device, &g.config);
                }
                self.sync_size();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                let pt = self.font_pt;
                self.set_font(pt);
            }
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::DroppedFile(path) => self.drop_files(vec![path], None),
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => self.key(&event),
            WindowEvent::Focused(f) => {
                if let Some(n) = self.pane().and_then(|p| p.nvim.as_ref()) {
                    n.focus(f);
                }
                if self.mode().contains(TermMode::FOCUS_IN_OUT) {
                    if let Some(s) = self.sess() {
                        s.write(if f { b"\x1b[I".to_vec() } else { b"\x1b[O".to_vec() });
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = (position.x, position.y);
                if self.web_focus.is_some() {
                    if let Some((v, x, y)) = self.web_hit(position.x, position.y) {
                        v.mouse("moved", x, y, self.web_btn.unwrap_or("none"), 0);
                        return;
                    }
                }
                let cell = self.cell_at(position.x, position.y);
                if let (Some(n), Some(b)) = (self.pane().and_then(|p| p.nvim.as_ref()), self.nvim_btn) {
                    if cell != self.last_cell {
                        n.mouse(b, "drag", &self.nvim_mods(), cell.1, cell.0);
                    }
                    self.last_cell = cell;
                    return;
                }
                if self.app_mouse() {
                    // drags (and plain motion, if asked) go to the program, once per cell
                    let mode = self.mode();
                    let want = mode.contains(TermMode::MOUSE_MOTION) || (mode.contains(TermMode::MOUSE_DRAG) && self.mouse_btn.is_some());
                    if want && cell != self.last_cell {
                        let b = self.mouse_btn.unwrap_or(3);
                        self.send_mouse(b, true, true, cell);
                    }
                    self.last_cell = cell;
                    return;
                }
                self.last_cell = cell;
                if self.selecting {
                    if let (Some((p, side)), Some(s)) = (self.point_at(position.x, position.y), self.sess()) {
                        if let Some(sel) = s.term.lock().selection.as_mut() {
                            sel.update(p, side);
                        }
                        self.redraw();
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let code = match button { MouseButton::Left => 0, MouseButton::Middle => 1, MouseButton::Right => 2, _ => return };
                let pressed = state == ElementState::Pressed;
                // a click in another pane gives it the keyboard first
                if pressed {
                    let hit = self.tabs.get(self.active).zip(self.area()).filter(|(t, _)| !t.zoom).and_then(|(t, ar)| t.layout.at(self.mouse.0 as f32, self.mouse.1 as f32, ar));
                    if let Some(id) = hit {
                        self.focus_pane(id);
                    }
                }
                // a web area: the click is the page's, and the keyboard goes with it
                if let Some((v, x, y)) = self.web_hit(self.mouse.0, self.mouse.1) {
                    let b = match button { MouseButton::Left => "left", MouseButton::Right => "right", _ => "middle" };
                    if pressed {
                        let now = std::time::Instant::now();
                        let double = self.clicks.0.elapsed() < std::time::Duration::from_millis(400) && self.web_focus == Some(v.id);
                        self.clicks = (now, if double { 2 } else { 1 }, self.last_cell);
                        self.web_focus = Some(v.id);
                        self.web_btn = Some(b);
                        v.mouse("pressed", x, y, b, self.clicks.1 as u32);
                    } else {
                        self.web_btn = None;
                        v.mouse("released", x, y, b, self.clicks.1 as u32);
                    }
                    self.redraw();
                    return;
                } else if pressed {
                    self.web_focus = None;
                }
                if let Some(n) = self.pane().and_then(|p| p.nvim.as_ref()) {
                    let (col, row) = self.cell_at(self.mouse.0, self.mouse.1);
                    let b = match button { MouseButton::Left => "left", MouseButton::Right => "right", _ => "middle" };
                    n.mouse(b, if pressed { "press" } else { "release" }, &self.nvim_mods(), row, col);
                    self.nvim_btn = pressed.then_some(b);
                    return;
                }
                if self.app_mouse() {
                    let cell = self.cell_at(self.mouse.0, self.mouse.1);
                    self.mouse_btn = if pressed { Some(code) } else { None };
                    self.send_mouse(code, false, pressed, cell);
                    return;
                }
                // Ctrl+click: open the link under the pointer
                if button == MouseButton::Left && pressed && self.mods.control_key() {
                    if let Some(url) = self.link_at(self.mouse.0, self.mouse.1) {
                        let _ = std::process::Command::new("xdg-open").arg(&url).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
                        return;
                    }
                }
                match (button, pressed) {
                    (MouseButton::Left, true) => {
                        // 1 click: characters, 2: a word, 3: the line
                        let now = std::time::Instant::now();
                        let same = self.clicks.0.elapsed() < std::time::Duration::from_millis(400) && self.clicks.2 == self.last_cell;
                        self.clicks = (now, if same { self.clicks.1 % 3 + 1 } else { 1 }, self.last_cell);
                        let ty = match self.clicks.1 { 2 => SelectionType::Semantic, 3 => SelectionType::Lines, _ => SelectionType::Simple };
                        if let (Some((p, side)), Some(s)) = (self.point_at(self.mouse.0, self.mouse.1), self.sess()) {
                            s.term.lock().selection = Some(Selection::new(ty, p, side));
                            self.selecting = true;
                            self.redraw();
                        }
                    }
                    (MouseButton::Left, false) => {
                        self.selecting = false;
                        // like every X/Wayland terminal: a selection is also the middle-click paste
                        if let Some(s) = self.sess() {
                            let text = { let t = s.term.lock(); t.selection.as_ref().filter(|s| !s.is_empty()).and(t.selection_to_string()) };
                            if let Some(t) = text.filter(|t| !t.is_empty()) {
                                std::thread::spawn(move || clipboard_set(&t, true));
                            }
                        }
                    }
                    (MouseButton::Middle, true) => {
                        let text = clipboard_get(true);
                        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
                        self.write(input::paste(&text, bracketed));
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let cell_h = self.gpu.as_ref().map(|g| g.renderer.cell_h as f64).unwrap_or(16.0);
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64 * 3.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / cell_h,
                };
                // over a web area the wheel scrolls the page (in its own pixels)
                if let Some((v, x, y)) = self.web_hit(self.mouse.0, self.mouse.1) {
                    let px = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -(y as f32) * 100.0,
                        MouseScrollDelta::PixelDelta(p) => -(p.y as f32),
                    };
                    v.wheel(x, y, px);
                    return;
                }
                self.scroll_acc += lines;
                let whole = self.scroll_acc.trunc() as i32;
                if whole == 0 {
                    return;
                }
                self.scroll_acc -= whole as f64;
                if let Some(n) = self.pane().and_then(|p| p.nvim.as_ref()) {
                    let (col, row) = self.cell_at(self.mouse.0, self.mouse.1);
                    for _ in 0..whole.unsigned_abs() {
                        n.mouse("wheel", if whole > 0 { "up" } else { "down" }, &self.nvim_mods(), row, col);
                    }
                    return;
                }
                let mode = self.mode();
                if self.app_mouse() {
                    let cell = self.cell_at(self.mouse.0, self.mouse.1);
                    for _ in 0..whole.unsigned_abs() {
                        self.send_mouse(if whole > 0 { 64 } else { 65 }, false, true, cell);
                    }
                } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
                    // full-screen programs without the mouse (less, man) get arrow keys
                    let key = if whole > 0 { if mode.contains(TermMode::APP_CURSOR) { "\x1bOA" } else { "\x1b[A" } } else if mode.contains(TermMode::APP_CURSOR) { "\x1bOB" } else { "\x1b[B" };
                    if let Some(s) = self.sess() {
                        s.write(key.repeat(whole.unsigned_abs() as usize).into_bytes());
                    }
                } else if self.sess().is_some() {
                    self.scroll_view(Scroll::Delta(whole));
                }
            }
            _ => {}
        }
    }
}

#[allow(dead_code)]
fn _assert_dims<T: Dimensions>(_: &T) {}

/// tinct rewrites its colour file when the wallpaper changes; zash follows.
/// One stat every two seconds on a background thread: negligible, and no frames
/// are drawn unless the file really changed.
fn watch_theme(proxy: EventLoopProxy<ZEvent>, paths: Vec<std::path::PathBuf>) {
    watch_files(proxy, paths, || ZEvent::Theme)
}

fn watch_files(proxy: EventLoopProxy<ZEvent>, paths: Vec<std::path::PathBuf>, ev: fn() -> ZEvent) {
    std::thread::spawn(move || {
        let stamp = || paths.iter().map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok()).collect::<Vec<_>>();
        let mut last = stamp();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let now = stamp();
            if now != last {
                last = now;
                if proxy.send_event(ev()).is_err() {
                    return;
                }
            }
        }
    });
}


/// A path as one shell word: plain when it is safe, else in single quotes.
fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+,:@%=~".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod quote_tests {
    #[test]
    fn quoting() {
        assert_eq!(super::shell_quote("/tmp/a.txt"), "/tmp/a.txt");
        assert_eq!(super::shell_quote("/tmp/my file.png"), "'/tmp/my file.png'");
        assert_eq!(super::shell_quote("it's"), "'it'\\''s'");
    }
}
