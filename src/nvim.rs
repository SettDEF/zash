//! Neovim inside zash, the way Neovide does it: zash is Neovim's screen.
//!
//! The shell plugin starts `nvim --headless --listen SOCKET` (the user's own
//! config, nothing changes for them) and tells zash the socket (OSC 7733
//! `nvim;SOCKET`). zash connects, attaches as a UI (`nvim_ui_attach`, line
//! grid, true colour) and from then on draws Neovim's grid itself and sends
//! keys and mouse straight to it. Because zash gets Neovim's real events, it
//! can animate what a terminal cannot: scrolled regions glide (the old lines
//! slide out while the new ones come in), and the cursor flies.
//!
//! When Neovim quits the socket closes and the pane is a terminal again.

use crate::config::{Palette, Rgb};
use crate::render::{CellView, Frame, Style2};
use alacritty_terminal::vte::ansi::CursorShape;
use rmpv::Value;
use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Default)]
struct Attr {
    fg: Option<Rgb>,
    bg: Option<Rgb>,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    reverse: bool,
}

#[derive(Clone)]
struct Cell {
    text: String,
    hl: u64,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { text: " ".into(), hl: 0 }
    }
}

#[derive(Clone, Copy)]
struct ModeShape {
    shape: CursorShape,
}

/// A scroll that is still gliding: the region, how far it is drawn from its real place,
/// and the lines that were there before (they fill the edge while it moves).
pub struct Glide {
    pub top: usize,
    pub bot: usize,
    pub px: f32,
    /// rows that scrolled out, nearest to the region edge first
    pub old: Vec<Vec<CellView>>,
    at: Instant,
}

#[derive(Default)]
pub struct State {
    cols: usize,
    rows: usize,
    cells: Vec<Vec<Cell>>,
    attrs: HashMap<u64, Attr>,
    fg: Option<Rgb>,
    bg: Option<Rgb>,
    cursor: (usize, usize),
    modes: Vec<ModeShape>,
    mode: usize,
    pub title: Option<String>,
    /// the socket closed: Neovim quit
    pub closed: bool,
    /// a whole frame arrived (`flush`): worth drawing
    pub dirty: bool,
    pub glide: Option<Glide>,
    /// pixels per row and scroll animation length, from the renderer and config
    pub cell_h: f32,
    pub glide_ms: f32,
}

pub struct Nvim {
    pub state: Arc<Mutex<State>>,
    writer: Arc<Mutex<BufWriter<UnixStream>>>,
    next_id: Mutex<u64>,
}

fn rgb(v: &Value) -> Option<Rgb> {
    let n = v.as_i64()?;
    if n < 0 {
        return None;
    }
    Some(Rgb(((n >> 16) & 0xff) as u8, ((n >> 8) & 0xff) as u8, (n & 0xff) as u8))
}

fn get<'a>(map: &'a [(Value, Value)], key: &str) -> Option<&'a Value> {
    map.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
}

impl Nvim {
    /// Connects to the socket (Neovim may still be starting: tries for a few seconds) and attaches.
    pub fn connect(sock: &str, cols: usize, rows: usize, cell_h: f32, glide_ms: f32, wake: Box<dyn Fn() + Send>) -> std::io::Result<Nvim> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match UnixStream::connect(sock) {
                Ok(s) => break s,
                Err(e) if Instant::now() > deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(30)),
            }
        };
        let reader = BufReader::new(stream.try_clone()?);
        let writer = Arc::new(Mutex::new(BufWriter::new(stream)));
        let state = Arc::new(Mutex::new(State { cell_h, glide_ms, ..Default::default() }));
        let st = state.clone();
        std::thread::Builder::new().name("zash nvim".into()).spawn(move || read_loop(reader, st, wake))?;
        let n = Nvim { state, writer, next_id: Mutex::new(1) };
        let mut opts = Vec::new();
        opts.push((Value::from("ext_linegrid"), Value::from(true)));
        opts.push((Value::from("rgb"), Value::from(true)));
        n.request("nvim_ui_attach", vec![Value::from(cols as u64), Value::from(rows as u64), Value::Map(opts)]);
        Ok(n)
    }

    fn request(&self, method: &str, params: Vec<Value>) {
        let id = {
            let mut i = self.next_id.lock().unwrap();
            *i += 1;
            *i
        };
        let msg = Value::Array(vec![Value::from(0u64), Value::from(id), Value::from(method), Value::Array(params)]);
        if let Ok(mut w) = self.writer.lock() {
            let _ = rmpv::encode::write_value(&mut *w, &msg);
            let _ = w.flush();
        }
    }

    /// Keys in Neovim's notation (`a`, `<C-w>`, `<CR>`, `<lt>`).
    pub fn input(&self, keys: &str) {
        self.request("nvim_input", vec![Value::from(keys)]);
    }

    pub fn paste(&self, text: &str) {
        self.request("nvim_paste", vec![Value::from(text), Value::from(true), Value::from(-1i64)]);
    }

    /// button: left right middle wheel; action: press drag release up down left right
    pub fn mouse(&self, button: &str, action: &str, mods: &str, row: usize, col: usize) {
        self.request("nvim_input_mouse", vec![Value::from(button), Value::from(action), Value::from(mods), Value::from(0u64), Value::from(row as u64), Value::from(col as u64)]);
    }

    pub fn resize(&self, cols: usize, rows: usize) {
        self.request("nvim_ui_try_resize", vec![Value::from(cols as u64), Value::from(rows as u64)]);
    }

    pub fn focus(&self, on: bool) {
        self.request("nvim_ui_set_focus", vec![Value::from(on)]);
    }
}

fn read_loop(mut r: BufReader<UnixStream>, st: Arc<Mutex<State>>, wake: Box<dyn Fn() + Send>) {
    loop {
        let Ok(msg) = rmpv::decode::read_value(&mut r) else { break };
        let Some(a) = msg.as_array() else { continue };
        // notifications: [2, method, params]; responses [1, id, err, result] are not needed
        if a.first().and_then(|v| v.as_u64()) != Some(2) || a.get(1).and_then(|v| v.as_str()) != Some("redraw") {
            continue;
        }
        let Some(batches) = a.get(2).and_then(|v| v.as_array()) else { continue };
        let mut flush = false;
        {
            let mut s = st.lock().unwrap();
            for b in batches {
                let Some(ev) = b.as_array() else { continue };
                let Some(name) = ev.first().and_then(|v| v.as_str()) else { continue };
                for args in &ev[1..] {
                    let Some(args) = args.as_array() else { continue };
                    if s.event(name, args) {
                        flush = true;
                    }
                }
            }
            if flush {
                s.dirty = true;
            }
        }
        if flush {
            wake();
        }
    }
    st.lock().unwrap().closed = true;
    wake();
}

impl State {
    /// One redraw event; true for `flush` (a frame is complete).
    fn event(&mut self, name: &str, a: &[Value]) -> bool {
        let n = |i: usize| a.get(i).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let i = |i: usize| a.get(i).and_then(|v| v.as_i64()).unwrap_or(0);
        match name {
            "grid_resize" => {
                let (w, h) = (n(1), n(2));
                self.cols = w;
                self.rows = h;
                self.cells.resize_with(h, Vec::new);
                for row in &mut self.cells {
                    row.resize_with(w, Cell::default);
                }
                self.glide = None;
            }
            "default_colors_set" => {
                self.fg = a.first().and_then(rgb);
                self.bg = a.get(1).and_then(rgb);
            }
            "hl_attr_define" => {
                let id = n(0);
                let Some(m) = a.get(1).and_then(|v| v.as_map()) else { return false };
                let flag = |k: &str| get(m, k).and_then(|v| v.as_bool()).unwrap_or(false);
                self.attrs.insert(id as u64, Attr {
                    fg: get(m, "foreground").and_then(rgb),
                    bg: get(m, "background").and_then(rgb),
                    bold: flag("bold"),
                    italic: flag("italic"),
                    underline: flag("underline") || flag("undercurl") || flag("underdouble") || flag("underdotted") || flag("underdashed"),
                    strike: flag("strikethrough"),
                    reverse: flag("reverse"),
                });
            }
            "grid_clear" => {
                for row in &mut self.cells {
                    row.iter_mut().for_each(|c| *c = Cell::default());
                }
                self.glide = None;
            }
            "grid_cursor_goto" => self.cursor = (n(1), n(2)),
            "grid_line" => {
                let (row, mut col) = (n(1), n(2));
                let Some(cells) = a.get(3).and_then(|v| v.as_array()) else { return false };
                let mut hl = 0u64;
                for c in cells {
                    let Some(c) = c.as_array() else { continue };
                    let text = c.first().and_then(|v| v.as_str()).unwrap_or(" ").to_string();
                    if let Some(h) = c.get(1).and_then(|v| v.as_u64()) {
                        hl = h;
                    }
                    let repeat = c.get(2).and_then(|v| v.as_u64()).unwrap_or(1) as usize;
                    for _ in 0..repeat {
                        if let Some(cell) = self.cells.get_mut(row).and_then(|r| r.get_mut(col)) {
                            *cell = Cell { text: text.clone(), hl };
                        }
                        col += 1;
                    }
                }
            }
            "grid_scroll" => {
                let (top, bot, left, right, rows) = (n(1), n(2), n(3), n(4), i(5));
                self.scroll(top, bot, left, right, rows);
            }
            "mode_info_set" => {
                let Some(list) = a.get(1).and_then(|v| v.as_array()) else { return false };
                self.modes = list.iter().map(|m| {
                    let m = m.as_map().map(|v| v.as_slice()).unwrap_or(&[]);
                    let shape = match get(m, "cursor_shape").and_then(|v| v.as_str()) {
                        Some("vertical") => CursorShape::Beam,
                        Some("horizontal") => CursorShape::Underline,
                        _ => CursorShape::Block,
                    };
                    ModeShape { shape }
                }).collect();
            }
            "mode_change" => self.mode = n(1),
            "set_title" => self.title = a.first().and_then(|v| v.as_str()).map(String::from),
            "flush" => return true,
            _ => {}
        }
        false
    }

    /// `grid_scroll`: the region's rows move up (rows > 0) or down; the lines coming in
    /// arrive in grid_line events after this. With glide on, the old lines are kept to
    /// slide out while the new ones slide in.
    fn scroll(&mut self, top: usize, bot: usize, left: usize, right: usize, rows: i64) {
        let full_width = left == 0 && right >= self.cols;
        if full_width && self.glide_ms > 0.0 && self.cell_h > 0.0 {
            let k = rows.unsigned_abs() as usize;
            // the rows leaving the region: up → its top rows, down → its bottom rows
            let leaving: Vec<Vec<CellView>> = if rows > 0 {
                (top..(top + k).min(bot)).rev().map(|r| self.row_view(r)).collect()
            } else {
                ((bot.saturating_sub(k)).max(top)..bot).map(|r| self.row_view(r)).collect()
            };
            let px = rows as f32 * self.cell_h;
            match self.glide.as_mut() {
                // more scrolling while one glides: it carries on from where it is
                Some(g) if g.top == top && g.bot == bot && (g.px >= 0.0) == (px >= 0.0) => {
                    g.px = (g.px + px).clamp(-((bot - top) as f32) * self.cell_h, ((bot - top) as f32) * self.cell_h);
                    let mut old = leaving;
                    old.extend(std::mem::take(&mut g.old));
                    old.truncate(bot - top);
                    g.old = old;
                }
                _ => self.glide = Some(Glide { top, bot, px, old: leaving, at: Instant::now() }),
            }
        } else {
            self.glide = None;
        }
        // move the cells
        if rows > 0 {
            let k = rows as usize;
            for r in top..bot {
                if r + k < bot {
                    let src: Vec<Cell> = self.cells[r + k][left..right.min(self.cols)].to_vec();
                    self.cells[r][left..right.min(self.cols)].clone_from_slice(&src);
                }
            }
        } else {
            let k = (-rows) as usize;
            for r in (top..bot).rev() {
                if r >= top + k {
                    let src: Vec<Cell> = self.cells[r - k][left..right.min(self.cols)].to_vec();
                    self.cells[r][left..right.min(self.cols)].clone_from_slice(&src);
                }
            }
        }
    }

    fn attr(&self, hl: u64) -> Attr {
        self.attrs.get(&hl).copied().unwrap_or_default()
    }

    fn row_view(&self, r: usize) -> Vec<CellView> {
        let (dfg, dbg) = (self.fg.unwrap_or(Rgb(220, 220, 220)), self.bg.unwrap_or(Rgb(0, 0, 0)));
        let row = &self.cells[r];
        let mut out: Vec<CellView> = Vec::with_capacity(row.len());
        for c in row {
            let at = self.attr(c.hl);
            let (mut fg, mut bg) = (at.fg.unwrap_or(dfg), at.bg.unwrap_or(dbg));
            if at.reverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            // a wide character is followed by an empty cell
            if c.text.is_empty() {
                if let Some(prev) = out.last_mut() {
                    prev.wide = true;
                }
                out.push(CellView { ch: ' ', zw: None, style: Style2 { fg: (fg.0, fg.1, fg.2), bold: false, italic: false }, bg: (bg != dbg).then_some(bg), underline: false, strike: false, wide: false, spacer: true });
                continue;
            }
            let mut chars = c.text.chars();
            let ch = chars.next().unwrap_or(' ');
            let rest: Vec<char> = chars.collect();
            out.push(CellView {
                ch,
                zw: (!rest.is_empty()).then_some(rest),
                style: Style2 { fg: (fg.0, fg.1, fg.2), bold: at.bold, italic: at.italic },
                bg: (bg != dbg).then_some(bg),
                underline: at.underline,
                strike: at.strike,
                wide: false,
                spacer: false,
            });
        }
        out
    }

    /// Advances the glide to now; true while it still moves.
    pub fn settle(&mut self) -> bool {
        let Some(g) = self.glide.as_mut() else { return false };
        let now = Instant::now();
        let dt = now.duration_since(g.at).as_secs_f32() * 1000.0;
        g.at = now;
        g.px *= (-dt / (self.glide_ms / 3.0).max(1.0)).exp();
        if g.px.abs() < 0.5 {
            self.glide = None;
            return false;
        }
        true
    }

    /// Neovim's screen as a frame for the renderer.
    pub fn frame(&self, p: &Palette) -> Frame {
        let rows: Vec<Vec<CellView>> = (0..self.rows).map(|r| self.row_view(r)).collect();
        let bg = self.bg.unwrap_or(p.bg);
        let fg = self.fg.unwrap_or(p.fg);
        let shape = self.modes.get(self.mode).map(|m| m.shape).unwrap_or(CursorShape::Block);
        let cursor = (self.cursor.0 < self.rows).then_some((self.cursor.0, self.cursor.1, shape));
        let mut rows = rows;
        if let Some((r, c, CursorShape::Block)) = cursor {
            // the character under a block cursor is drawn in the background colour
            if let Some(cell) = rows.get_mut(r).and_then(|row| row.get_mut(c)) {
                cell.style.fg = (bg.0, bg.1, bg.2);
            }
        }
        Frame {
            above: Vec::new(),
            below: Vec::new(),
            scroll_px: 0.0,
            popup_look: Default::default(),
            region: self.glide.as_ref().map(|g| crate::render::Region { top: g.top, bot: g.bot, px: g.px, old: g.old.clone() }),
            popup: None,
            tabs: Vec::new(),
            bar: None,
            bar_bg: p.selection_bg,
            fg,
            prompts: Vec::new(),
            cols: self.cols,
            rows,
            cursor,
            bg,
            cursor_color: p.cursor_bg,
            fail_color: p.ansi[1],
        }
    }
}

/// A key event as Neovim notation; None when Neovim has no name for it.
pub fn notation(named: Option<&str>, text: Option<&str>, ctrl: bool, alt: bool, shift: bool) -> Option<String> {
    let mut mods = String::new();
    if ctrl {
        mods.push_str("C-");
    }
    if alt {
        mods.push_str("M-");
    }
    if let Some(n) = named {
        if shift {
            mods.push_str("S-");
        }
        return Some(format!("<{mods}{n}>"));
    }
    let t = text?;
    let mut chars = t.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        // several characters (an input method): as they are
        return Some(t.replace('<', "<lt>"));
    }
    let key = match c {
        '<' => "lt".to_string(),
        '\\' if !mods.is_empty() => "Bslash".to_string(),
        ' ' => "Space".to_string(),
        c => c.to_string(),
    };
    if mods.is_empty() {
        return Some(if c == '<' { "<lt>".into() } else { key });
    }
    Some(format!("<{mods}{key}>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_notation() {
        assert_eq!(notation(None, Some("a"), false, false, false).as_deref(), Some("a"));
        assert_eq!(notation(None, Some("<"), false, false, false).as_deref(), Some("<lt>"));
        assert_eq!(notation(None, Some("w"), true, false, false).as_deref(), Some("<C-w>"));
        assert_eq!(notation(Some("CR"), None, false, false, false).as_deref(), Some("<CR>"));
        assert_eq!(notation(Some("Tab"), None, false, false, true).as_deref(), Some("<S-Tab>"));
        assert_eq!(notation(None, Some("j"), false, true, false).as_deref(), Some("<M-j>"));
    }

    #[test]
    fn scroll_keeps_the_leaving_rows_for_the_glide() {
        let mut s = State { cell_h: 10.0, glide_ms: 100.0, ..Default::default() };
        s.event("grid_resize", &[Value::from(1u64), Value::from(3u64), Value::from(4u64)]);
        for r in 0..4u64 {
            let cells = Value::Array(vec![Value::Array(vec![Value::from(format!("{r}")), Value::from(0u64), Value::from(3u64)])]);
            s.event("grid_line", &[Value::from(1u64), Value::from(r), Value::from(0u64), cells, Value::from(false)]);
        }
        // scroll the whole grid up by one
        s.event("grid_scroll", &[1u64, 0, 4, 0, 3].iter().map(|v| Value::from(*v)).chain([Value::from(1i64), Value::from(0i64)]).collect::<Vec<_>>());
        let g = s.glide.as_ref().unwrap();
        assert_eq!(g.px, 10.0);
        assert_eq!(g.old.len(), 1);
        assert_eq!(g.old[0][0].ch, '0', "row 0 left the region");
        assert_eq!(s.cells[0][0].text, "1", "rows moved up");
    }
}
