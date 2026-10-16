//! The window: winit + a wgpu surface, keys and mouse in, frames out.
//!
//! Frames are drawn only when something changed (the terminal reports new
//! output, the window resizes, a key scrolls or selects). Idle means idle.

use crate::config::Config;
use crate::input::{self, Mods, Named};
use crate::render::{Frame, Renderer};
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

/// One tab: a session and its title.
struct Tab {
    id: u64,
    session: Session,
    title: String,
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
    next_id: u64,
    mods: ModifiersState,
    font_pt: f32,
    mouse: (f64, f64),
    selecting: bool,
    scroll_acc: f64,
    /// the button held down for a program that wants the mouse
    mouse_btn: Option<u8>,
    last_cell: (usize, usize),
    /// (time, count, cell) of the last left click: double = word, triple = line
    clicks: (std::time::Instant, u8, (usize, usize)),
    /// scrollback search (Ctrl+Shift+F) while it is open
    search: Option<crate::search::Search>,
}

pub fn run(cfg: Config, cmd: Option<Vec<String>>, workdir: Option<std::path::PathBuf>) -> Result<(), String> {
    let event_loop = EventLoop::<ZEvent>::with_user_event().build().map_err(|e| e.to_string())?;
    let proxy = event_loop.create_proxy();
    let font_pt = cfg.font_size_pt;
    let mut app = App { cfg, cmd, workdir, proxy, gpu: None, tabs: Vec::new(), active: 0, next_id: 0, mods: ModifiersState::empty(), font_pt, mouse: (0.0, 0.0), selecting: false, scroll_acc: 0.0, mouse_btn: None, last_cell: (0, 0), clicks: (std::time::Instant::now(), 0, (0, 0)), search: None };
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
    fn grid(&self) -> Option<Grid> {
        let g = self.gpu.as_ref()?;
        let (cols, rows) = g.renderer.grid_for(g.config.width, g.config.height);
        Some(Grid { cols, rows, cell_w: g.renderer.cell_w, cell_h: g.renderer.cell_h })
    }

    fn redraw(&self) {
        if let Some(g) = &self.gpu {
            g.window.request_redraw();
        }
    }

    fn sync_size(&mut self) {
        // every tab, not only the one shown: a hidden one is the same size when it comes back
        if let Some(grid) = self.grid() {
            for t in &mut self.tabs {
                t.session.resize(grid);
            }
        }
        self.redraw();
    }

    fn write(&self, bytes: Vec<u8>) {
        if let Some(s) = self.tabs.get(self.active).map(|t| &t.session) {
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
        self.tabs.get(self.active).map(|t| &t.session).map(|s| *s.term.lock().mode()).unwrap_or(TermMode::empty())
    }

    /// Window pixels → a point in the terminal (accounting for scrollback).
    fn point_at(&self, x: f64, y: f64) -> Option<(Point, Side)> {
        let g = self.gpu.as_ref()?;
        let s = self.tabs.get(self.active).map(|t| &t.session)?;
        let r = &g.renderer;
        let col_f = ((x as f32 - r.pad) / r.cell_w).max(0.0);
        let col = (col_f as usize).min(s.grid.cols.saturating_sub(1));
        let row = (((y as f32 - r.pad - r.top) / r.cell_h).max(0.0) as usize).min(s.grid.rows.saturating_sub(1));
        let offset = s.term.lock().grid().display_offset() as i32;
        let side = if col_f.fract() < 0.5 { Side::Left } else { Side::Right };
        Some((Point::new(Line(row as i32 - offset), Column(col)), side))
    }

    /// The screen cell under a window position (0-based column, row).
    fn cell_at(&self, x: f64, y: f64) -> (usize, usize) {
        let (Some(g), Some(s)) = (self.gpu.as_ref(), self.tabs.get(self.active).map(|t| &t.session)) else { return (0, 0) };
        let r = &g.renderer;
        let col = (((x as f32 - r.pad) / r.cell_w).max(0.0) as usize).min(s.grid.cols.saturating_sub(1));
        let row = (((y as f32 - r.pad - r.top) / r.cell_h).max(0.0) as usize).min(s.grid.rows.saturating_sub(1));
        (col, row)
    }

    /// Does the program want the mouse? Shift keeps it for zash (selecting text), as everywhere.
    fn app_mouse(&self) -> bool {
        !self.mods.shift_key() && self.mode().intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
    }

    fn send_mouse(&self, button: u8, motion: bool, pressed: bool, cell: (usize, usize)) {
        let m = self.mods;
        let mods = Mods { ctrl: m.control_key(), alt: m.alt_key(), shift: m.shift_key() };
        let sgr = self.mode().contains(TermMode::SGR_MOUSE);
        if let Some(s) = self.tabs.get(self.active).map(|t| &t.session) {
            s.write(input::mouse(button, motion, pressed, cell.0, cell.1, mods, sgr));
        }
    }

    fn search_key(&mut self, ev: &KeyEvent, mods: Mods) {
        use alacritty_terminal::index::Direction;
        let (Some(s), Some(sess)) = (self.search.as_mut(), self.tabs.get(self.active).map(|t| &t.session)) else { return };
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
        let s = self.tabs.get(self.active).map(|t| &t.session)?;
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
        let Some(s) = self.tabs.get(self.active).map(|t| &t.session) else { return };
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
        let s = self.tabs.get(self.active).map(|t| &t.session)?;
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

    fn new_tab(&mut self, cmd: Option<Vec<String>>, dir: Option<std::path::PathBuf>) -> std::io::Result<()> {
        let id = self.next_id;
        self.next_id += 1;
        // the tab bar appears with the second tab: make room before sizing the grid
        let n = self.tabs.len() + 1;
        if let Some(g) = self.gpu.as_mut() {
            g.renderer.top = if n > 1 { g.renderer.tab_bar_h() } else { 0.0 };
        }
        let grid = self.grid().ok_or_else(|| std::io::Error::other("no window"))?;
        let session = Session::spawn(cmd, dir, grid, self.cfg.scrollback, Sink::Window(self.proxy.clone(), id))?;
        self.tabs.push(Tab { id, session, title: "zash".into() });
        self.active = self.tabs.len() - 1;
        self.search = None;
        self.sync_size();
        self.show_title();
        Ok(())
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
        if let Some(g) = self.gpu.as_mut() {
            g.renderer.top = if self.tabs.len() > 1 { g.renderer.tab_bar_h() } else { 0.0 };
        }
        self.search = None;
        self.sync_size();
        self.show_title();
    }

    fn switch_tab(&mut self, by: i32) {
        let n = self.tabs.len() as i32;
        if n < 2 {
            return;
        }
        self.active = ((self.active as i32 + by).rem_euclid(n)) as usize;
        self.search = None;
        self.show_title();
    }

    fn show_title(&self) {
        if let (Some(g), Some(t)) = (&self.gpu, self.tabs.get(self.active)) {
            g.window.set_title(&t.title);
        }
        self.redraw();
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
        if self.search.is_some() {
            return self.search_key(ev, mods);
        }
        // zash's own keys first
        if mods.ctrl && mods.shift {
            if let Key::Character(c) = &ev.logical_key {
                match c.to_lowercase().as_str() {
                    "c" => {
                        if let Some(s) = self.tabs.get(self.active).map(|t| &t.session) {
                            if let Some(t) = s.term.lock().selection_to_string() {
                                clipboard_set(&t, false);
                            }
                        }
                        return;
                    }
                    "t" => {
                        // a new tab, in the folder the current one is in
                        let dir = self.tabs.get(self.active).and_then(|t| t.session.cwd());
                        if let Err(e) = self.new_tab(None, dir) {
                            eprintln!("zash: no new tab: {e}");
                        }
                        return;
                    }
                    "w" => {
                        // hang up on the tab's program; its exit closes the tab
                        if let Some(t) = self.tabs.get(self.active) {
                            unsafe { libc::kill(t.session.pid as i32, libc::SIGHUP) };
                        }
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
        if mods.shift && !mods.ctrl {
            let page = match ev.logical_key {
                Key::Named(NamedKey::PageUp) => Some(Scroll::PageUp),
                Key::Named(NamedKey::PageDown) => Some(Scroll::PageDown),
                _ => None,
            };
            if let (Some(p), Some(s)) = (page, self.tabs.get(self.active).map(|t| &t.session)) {
                if !self.mode().contains(TermMode::ALT_SCREEN) {
                    s.term.lock().scroll_display(p);
                    self.redraw();
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
        let (Some(g), Some(s)) = (self.gpu.as_mut(), self.tabs.get(self.active).map(|t| &t.session)) else { return };
        let mut frame = {
            let t = s.term.lock();
            let marks = s.marks.lock().unwrap();
            Frame::extract(&t, &self.cfg.palette).with_prompts(&t, &marks)
        };
        frame.popup = s.popup.lock().unwrap().clone();
        if let Some(q) = &self.search {
            frame.bar = Some(q.bar());
        }
        // a tab without a real title shows the name of the folder its shell is in
        frame.tabs = self.tabs.iter().enumerate().map(|(i, t)| {
            let title = if t.title == "zash" || t.title.is_empty() {
                t.session.cwd().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string())).unwrap_or_else(|| "zash".into())
            } else {
                t.title.clone()
            };
            (title, i == self.active)
        }).collect();
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
        let (w, h) = (g.config.width as f32, g.config.height as f32);
        let top = g.renderer.top;
        let pane = crate::render::PaneView { id: self.tabs[self.active].id, x: 0.0, y: top, w, h: h - top, frame: &frame, focused: true };
        let animating = g.renderer.render(&g.device, &g.queue, &view, g.config.width, g.config.height, &[pane]);
        g.queue.present(tex);
        // another frame only while the cursor is still gliding: idle stays idle
        if animating {
            g.window.request_redraw();
        }
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
        // test hook: open more tabs at start (checking the tab bar without key presses)
        if let Some(n) = std::env::var("ZASH_TEST_TABS").ok().and_then(|v| v.parse::<usize>().ok()) {
            for _ in 1..n {
                let _ = self.new_tab(None, None);
            }
        }
        let mut watched = vec![self.cfg.colors_file.clone()];
        watched.extend(self.cfg.glass_file.clone());
        watch_theme(self.proxy.clone(), watched);
        self.redraw();
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: ZEvent) {
        let (id, ev) = match ev {
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
        // the tab this event belongs to (it may have closed meanwhile)
        let Some(tab) = self.tabs.iter().position(|t| t.id == id) else { return };
        let shown = tab == self.active;
        match ev {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => {
                if shown {
                    self.redraw();
                }
            }
            Event::Title(t) => {
                self.tabs[tab].title = t;
                self.show_title();
            }
            Event::ResetTitle => {
                self.tabs[tab].title = "zash".into();
                self.show_title();
            }
            Event::PtyWrite(s) => {
                if let Some(sess) = Some(&self.tabs[tab].session) {
                    sess.write(s.into_bytes());
                }
            }
            Event::ClipboardStore(ty, text) => clipboard_set(&text, matches!(ty, alacritty_terminal::term::ClipboardType::Selection)),
            Event::ClipboardLoad(ty, f) => {
                let text = clipboard_get(matches!(ty, alacritty_terminal::term::ClipboardType::Selection));
                if let Some(s) = Some(&self.tabs[tab].session) {
                    s.write(f(&text).into_bytes());
                }
            }
            Event::ColorRequest(i, f) => {
                if let Some(s) = Some(&self.tabs[tab].session) {
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
                if let Some(s) = Some(&self.tabs[tab].session) {
                    let g = s.grid;
                    s.write(f(WindowSize { num_lines: g.rows as u16, num_cols: g.cols as u16, cell_width: g.cell_w as u16, cell_height: g.cell_h as u16 }).into_bytes());
                }
            }
            Event::ChildExit(_) | Event::Exit => self.close_tab(tab, el),
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
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => self.key(&event),
            WindowEvent::Focused(f) => {
                if self.mode().contains(TermMode::FOCUS_IN_OUT) {
                    if let Some(s) = self.tabs.get(self.active).map(|t| &t.session) {
                        s.write(if f { b"\x1b[I".to_vec() } else { b"\x1b[O".to_vec() });
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = (position.x, position.y);
                let cell = self.cell_at(position.x, position.y);
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
                    if let (Some((p, side)), Some(s)) = (self.point_at(position.x, position.y), self.tabs.get(self.active).map(|t| &t.session)) {
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
                        if let (Some((p, side)), Some(s)) = (self.point_at(self.mouse.0, self.mouse.1), self.tabs.get(self.active).map(|t| &t.session)) {
                            s.term.lock().selection = Some(Selection::new(ty, p, side));
                            self.selecting = true;
                            self.redraw();
                        }
                    }
                    (MouseButton::Left, false) => {
                        self.selecting = false;
                        // like every X/Wayland terminal: a selection is also the middle-click paste
                        if let Some(s) = self.tabs.get(self.active).map(|t| &t.session) {
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
                self.scroll_acc += lines;
                let whole = self.scroll_acc.trunc() as i32;
                if whole == 0 {
                    return;
                }
                self.scroll_acc -= whole as f64;
                let mode = self.mode();
                if self.app_mouse() {
                    let cell = self.cell_at(self.mouse.0, self.mouse.1);
                    for _ in 0..whole.unsigned_abs() {
                        self.send_mouse(if whole > 0 { 64 } else { 65 }, false, true, cell);
                    }
                } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
                    // full-screen programs without the mouse (less, man) get arrow keys
                    let key = if whole > 0 { if mode.contains(TermMode::APP_CURSOR) { "\x1bOA" } else { "\x1b[A" } } else if mode.contains(TermMode::APP_CURSOR) { "\x1bOB" } else { "\x1b[B" };
                    if let Some(s) = self.tabs.get(self.active).map(|t| &t.session) {
                        s.write(key.repeat(whole.unsigned_abs() as usize).into_bytes());
                    }
                } else if let Some(s) = self.tabs.get(self.active).map(|t| &t.session) {
                    s.term.lock().scroll_display(Scroll::Delta(whole));
                    self.redraw();
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
    std::thread::spawn(move || {
        let stamp = || paths.iter().map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok()).collect::<Vec<_>>();
        let mut last = stamp();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let now = stamp();
            if now != last {
                last = now;
                if proxy.send_event(ZEvent::Theme).is_err() {
                    return;
                }
            }
        }
    });
}

