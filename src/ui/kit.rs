//! UI components on the layout engine: a program builds a tree once and gets both the zash UI hints
//! (OSC 7733;ui) with their cells and a plain rendering for other terminals (box lines, reverse video).
//! Std only, so it can move into its own crate.
//!
//!   let ui = row([activity_bar([...]), sidebar(24, [...]), column([tabs([...]), editor(lines, 3)])]);
//!   let s = ui.render(cols, rows);   // s.plain for other terminals, s.zash() for zash

#![allow(dead_code)] // a library: not every part is used by zash itself

use super::layout::{Align, Dir, Node, Rect, Size};
use super::tokens::{self, role, Gap, Type};

/// A cell's look in the plain rendering (fg: an ANSI colour 0..15).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct St {
    pub fg: Option<u8>,
    pub bold: bool,
    pub dim: bool,
    pub rev: bool,
    pub ul: bool,
}

pub const RED: u8 = 1;
pub const GREEN: u8 = 2;
pub const YELLOW: u8 = 3;
pub const BLUE: u8 = 4;
pub const MAGENTA: u8 = 5;
pub const CYAN: u8 = 6;

/// A span of words with a look (code, status segments).
#[derive(Clone, Debug, PartialEq)]
pub struct Span(pub String, pub St);

pub fn span(t: &str, fg: Option<u8>) -> Span {
    Span(t.into(), St { fg, ..St::default() })
}

/// A background behind a container: a role, a radius (px), border and shadow; `boxed` draws box lines in plain.
#[derive(Clone, Debug, PartialEq)]
pub struct Surface {
    pub role: &'static str,
    pub r: f32,
    pub border: bool,
    pub shadow: bool,
    pub boxed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TreeItem {
    pub icon: String,
    pub fg: Option<u8>,
    pub label: String,
    /// a folder: open or closed
    pub open: Option<bool>,
    pub selected: bool,
    pub children: Vec<TreeItem>,
}

#[derive(Clone, Debug, PartialEq)]
enum K {
    Box,
    Text { text: String, ty: Type, role: &'static str, mono: Option<St> },
    Item { tree: bool, depth: usize, chevron: Option<bool>, icon: String, fg: Option<u8>, label: String, badge: String, selected: bool },
    Button { label: String, primary: bool },
    Badge { label: String },
    Divider { across: bool },
    Spacer,
    Activity { icon: String, active: bool },
    Tab { icon: String, fg: Option<u8>, name: String, active: bool, dirty: bool },
    PanelTab { label: String, active: bool },
    Spans(Vec<Span>),
    Editor { lines: Vec<Vec<Span>>, current: usize },
}

/// A node of the tree: what it is, its layout, its children.
#[derive(Clone, Debug, PartialEq)]
pub struct El {
    k: K,
    dir: Dir,
    w: Size,
    h: Size,
    gap: Gap,
    pad: [usize; 4],
    align: Align,
    max: (usize, usize),
    surface: Option<Surface>,
    kids: Vec<El>,
}

fn el(k: K, w: Size, h: Size) -> El {
    El { k, dir: Dir::Column, w, h, gap: Gap::None, pad: [0; 4], align: Align::Start, max: (0, 0), surface: None, kids: Vec::new() }
}

impl El {
    pub fn w(mut self, s: Size) -> El { self.w = s; self }
    pub fn h(mut self, s: Size) -> El { self.h = s; self }
    pub fn gap(mut self, g: Gap) -> El { self.gap = g; self }
    pub fn pad(mut self, p: [usize; 4]) -> El { self.pad = p; self }
    pub fn align(mut self, a: Align) -> El { self.align = a; self }
    pub fn max_w(mut self, n: usize) -> El { self.max.0 = n; self }
    pub fn surface(mut self, s: Surface) -> El { self.surface = Some(s); self }
    /// A plain background in a layer role (r=0).
    pub fn bg(self, role: &'static str) -> El { self.surface(Surface { role, r: 0.0, border: false, shadow: false, boxed: false }) }
}

// containers

pub fn column(kids: impl IntoIterator<Item = El>) -> El {
    El { kids: kids.into_iter().collect(), ..el(K::Box, Size::Fill(1), Size::Fit) }
}

pub fn row(kids: impl IntoIterator<Item = El>) -> El {
    El { dir: Dir::Row, kids: kids.into_iter().collect(), ..el(K::Box, Size::Fill(1), Size::Fit) }
}

pub fn spacer() -> El {
    el(K::Spacer, Size::Fill(1), Size::Fill(1))
}

/// A line between parts (`across`: horizontal); box lines in plain, nothing in zash (the surfaces part them).
pub fn divider(across: bool) -> El {
    if across { el(K::Divider { across }, Size::Fill(1), Size::Fixed(1)) } else { el(K::Divider { across }, Size::Fixed(1), Size::Fill(1)) }
}

/// A card: layer 2, normal rounding, the shell's inner margin; box lines in plain.
pub fn card(kids: impl IntoIterator<Item = El>) -> El {
    column(kids).pad(tokens::CARD_PAD).gap(Gap::Row).surface(Surface { role: role::LAYER2, r: tokens::radius::NORMAL, border: true, shadow: true, boxed: true })
}

/// The sidebar: a fixed width, layer 1, sections a section gap apart.
pub fn sidebar(width: usize, kids: impl IntoIterator<Item = El>) -> El {
    column(kids).w(Size::Fixed(width)).h(Size::Fill(1)).pad(tokens::SIDEBAR_PAD).gap(Gap::Section).bg(role::LAYER1)
}

// words

pub fn text(t: &str, ty: Type) -> El {
    let role = if ty == Type::Caption { role::SUBTEXT } else { role::ON };
    el(K::Text { text: t.into(), ty, role, mono: None }, Size::Fit, Size::Fit)
}
pub fn caption(t: &str) -> El { text(t, Type::Caption) }
pub fn body(t: &str) -> El { text(t, Type::Body) }
pub fn title(t: &str) -> El { text(t, Type::Title) }
pub fn heading(t: &str) -> El { text(t, Type::Heading) }
pub fn display(t: &str) -> El { text(t, Type::Display) }
/// Words in the cell font (numbers, paths): the cells themselves in zash too.
pub fn mono(t: &str, st: St) -> El {
    el(K::Text { text: t.into(), ty: Type::Body, role: role::ON, mono: Some(st) }, Size::Fit, Size::Fit)
}
pub fn spans(s: Vec<Span>) -> El {
    el(K::Spans(s), Size::Fit, Size::Fixed(1))
}

/// A section's header: caption words, indented to sit between a row's icon and label.
pub fn section_header(t: &str) -> El {
    caption(&t.to_uppercase()).pad([tokens::SECTION_INDENT, 0, 0, 0])
}

/// A sidebar section: a header and its rows, no gap.
pub fn section(t: &str, kids: impl IntoIterator<Item = El>) -> El {
    let mut k = vec![section_header(t)];
    k.extend(kids);
    column(k)
}

// rows

/// A list row: icon, label, a badge on the right; selected is the pill.
pub fn item(icon: &str, label: &str) -> El {
    el(K::Item { tree: false, depth: 0, chevron: None, icon: icon.into(), fg: None, label: label.into(), badge: String::new(), selected: false }, Size::Fill(1), Size::Fixed(1))
}

impl El {
    pub fn badge(mut self, b: &str) -> El {
        if let K::Item { badge, .. } = &mut self.k { *badge = b.into() }
        self
    }
    pub fn selected(mut self, s: bool) -> El {
        if let K::Item { selected, .. } = &mut self.k { *selected = s }
        self
    }
    pub fn fg(mut self, c: u8) -> El {
        match &mut self.k {
            K::Item { fg, .. } | K::Tab { fg, .. } => *fg = Some(c),
            _ => {}
        }
        self
    }
}

/// Rows: `selected` the index of the chosen one.
pub fn list(selected: Option<usize>, items: impl IntoIterator<Item = El>) -> El {
    column(items.into_iter().enumerate().map(|(i, e)| e.selected(Some(i) == selected)))
}

/// A file's icon and colour by its extension.
pub fn file_icon(name: &str) -> (&'static str, Option<u8>) {
    match name.rsplit_once('.').map(|(_, e)| e) {
        Some("rs") => ("\u{e7a8}", Some(YELLOW)),
        Some("md") => ("\u{f48a}", Some(BLUE)),
        Some("toml") => ("\u{e615}", Some(MAGENTA)),
        Some("lua") => ("\u{e620}", Some(BLUE)),
        Some("sh") => ("\u{f489}", Some(GREEN)),
        Some("json") => ("\u{e60b}", Some(YELLOW)),
        Some("lock") => ("\u{f023}", None),
        _ => ("\u{f15b}", None),
    }
}

pub fn file(name: &str) -> TreeItem {
    let (icon, fg) = file_icon(name);
    TreeItem { icon: icon.into(), fg, label: name.into(), open: None, selected: false, children: Vec::new() }
}

pub fn folder(name: &str, open: bool, children: impl IntoIterator<Item = TreeItem>) -> TreeItem {
    let icon = if open { "\u{f07c}" } else { "\u{f07b}" };
    TreeItem { icon: icon.into(), fg: Some(BLUE), label: name.into(), open: Some(open), selected: false, children: children.into_iter().collect() }
}

impl TreeItem {
    pub fn selected(mut self) -> TreeItem { self.selected = true; self }
}

/// A file tree: folders with a chevron, the open ones' children indented under them.
pub fn tree(items: impl IntoIterator<Item = TreeItem>) -> El {
    fn flat(t: &TreeItem, depth: usize, out: &mut Vec<El>) {
        out.push(el(K::Item { tree: true, depth, chevron: t.open, icon: t.icon.clone(), fg: t.fg, label: t.label.clone(), badge: String::new(), selected: t.selected }, Size::Fill(1), Size::Fixed(1)));
        if t.open == Some(true) {
            t.children.iter().for_each(|c| flat(c, depth + 1, out));
        }
    }
    let mut rows = Vec::new();
    items.into_iter().for_each(|t| flat(&t, 0, &mut rows));
    column(rows)
}

// controls

pub fn button(label: &str, primary: bool) -> El {
    el(K::Button { label: label.into(), primary }, Size::Fit, Size::Fixed(1))
}

/// Buttons a row gap apart.
pub fn buttons(kids: impl IntoIterator<Item = El>) -> El {
    row(kids).gap(Gap::Row).w(Size::Fit)
}

/// A small filled label (a status line's mode).
pub fn badge(label: &str) -> El {
    el(K::Badge { label: label.into() }, Size::Fit, Size::Fixed(1))
}

/// A narrow column of icons; the active one marked; `bottom` icons pinned to the end.
pub fn activity_bar(icons: &[&str], active: usize, bottom: &[&str]) -> El {
    let ico = |i: &&str, a| el(K::Activity { icon: i.to_string(), active: a }, Size::Fill(1), Size::Fixed(1));
    let mut k: Vec<El> = icons.iter().enumerate().map(|(n, i)| ico(i, n == active)).collect();
    k.push(spacer());
    k.extend(bottom.iter().map(|i| ico(i, false)));
    column(k).w(Size::Fixed(4)).h(Size::Fill(1)).pad([0, 1, 0, 1]).gap(Gap::Section).bg(role::LAYER1)
}

/// Editor tabs: (name, active, dirty); the active one raised to the editor's surface.
pub fn tabs(t: &[(&str, bool, bool)]) -> El {
    row(t.iter().map(|(n, a, d)| {
        let (icon, fg) = file_icon(n);
        el(K::Tab { icon: icon.into(), fg, name: n.to_string(), active: *a, dirty: *d }, Size::Fit, Size::Fixed(1))
    })).h(Size::Fixed(1)).bg(role::LAYER1)
}

/// Where the file is: parts joined by ›.
pub fn breadcrumb(parts: &[&str]) -> El {
    let mut s = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 { s.push(Span(" › ".into(), St { dim: true, ..St::default() })) }
        s.push(Span(p.to_string(), St { dim: i + 1 < parts.len(), ..St::default() }));
    }
    spans(s).pad([1, 0, 0, 0])
}

/// Code with line numbers; `current` (0-based) marked.
pub fn editor(lines: Vec<Vec<Span>>, current: usize) -> El {
    el(K::Editor { lines, current }, Size::Fill(1), Size::Fill(1))
}

/// A bottom panel: its tabs (the first `active`), then its body.
pub fn panel(tab_names: &[&str], active: usize, body: impl IntoIterator<Item = El>) -> El {
    let head = row(tab_names.iter().enumerate().map(|(i, n)| el(K::PanelTab { label: n.to_uppercase(), active: i == active }, Size::Fit, Size::Fixed(1)))).gap(Gap::Section).pad([0, 0, 0, 1]);
    let mut k = vec![head];
    k.extend(body);
    column(k).pad([2, 1, 2, 0]).h(Size::Fill(1)).bg(role::HIGHLIGHT)
}

/// The status line: segments on the left, then on the right, full width.
pub fn status_bar(left: impl IntoIterator<Item = El>, right: impl IntoIterator<Item = El>) -> El {
    let mut k: Vec<El> = left.into_iter().collect();
    k.push(spacer().h(Size::Fixed(1)));
    k.extend(right);
    row(k).gap(Gap::Section).h(Size::Fixed(1)).pad([0, 0, 1, 0]).bg(role::LAYER1)
}

/// A crude Rust highlighter for demos: keywords, types, calls, strings, numbers, comments.
pub fn highlight(line: &str) -> Vec<Span> {
    const KW: &[&str] = &["fn", "let", "mut", "pub", "use", "impl", "for", "in", "if", "else", "match", "return", "struct", "enum", "mod", "self", "Self", "const", "as", "where"];
    let mut out: Vec<Span> = Vec::new();
    let push = |out: &mut Vec<Span>, t: String, st: St| match out.last_mut() {
        Some(Span(s, l)) if *l == st => s.push_str(&t),
        _ => out.push(Span(t, st)),
    };
    let c: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < c.len() {
        let st = |fg| St { fg: Some(fg), ..St::default() };
        if c[i] == '/' && c.get(i + 1) == Some(&'/') {
            push(&mut out, c[i..].iter().collect(), St { dim: true, ..St::default() });
            break;
        }
        if c[i] == '"' {
            let e = c[i + 1..].iter().position(|&x| x == '"').map_or(c.len(), |p| i + p + 2);
            push(&mut out, c[i..e].iter().collect(), st(GREEN));
            i = e;
            continue;
        }
        if c[i].is_alphanumeric() || c[i] == '_' {
            let s = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') { i += 1 }
            let w: String = c[s..i].iter().collect();
            let look = if KW.contains(&w.as_str()) { st(MAGENTA) }
                else if w.chars().all(|x| x.is_ascii_digit()) { st(CYAN) }
                else if w.starts_with(char::is_uppercase) { st(YELLOW) }
                else if c.get(i) == Some(&'(') || c.get(i) == Some(&'!') { st(BLUE) }
                else { St::default() };
            push(&mut out, w, look);
            continue;
        }
        push(&mut out, c[i].to_string(), St::default());
        i += 1;
    }
    out
}

// rendering

/// What a render gives: `plain` for any terminal; `text` + `osc` for zash (cells under the hints).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Screen {
    pub plain: String,
    pub text: String,
    pub osc: String,
}

impl Screen {
    pub fn zash(&self) -> String {
        format!("{}{}", self.text, self.osc)
    }
}

/// A tree and its last render: laid out again only when the size or the tree changes.
pub struct Ui {
    root: El,
    last: Option<((usize, usize), Screen)>,
}

impl Ui {
    pub fn new(root: El) -> Ui { Ui { root, last: None } }
    pub fn set(&mut self, root: El) {
        if root != self.root { self.root = root; self.last = None }
    }
    pub fn render(&mut self, cols: usize, rows: usize) -> &Screen {
        if self.last.as_ref().map(|l| l.0) != Some((cols, rows)) {
            self.last = Some(((cols, rows), self.root.render(cols, rows)));
        }
        &self.last.as_ref().unwrap().1
    }
}

struct Canvas {
    cells: Vec<Vec<(char, St)>>,
    w: usize,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Canvas {
        Canvas { cells: vec![vec![(' ', St::default()); w]; h], w }
    }
    /// Words at x, y, cut at `max` cells (the last one an ellipsis when cut).
    fn put(&mut self, x: usize, y: usize, t: &str, st: St, max: usize) -> usize {
        let Some(line) = self.cells.get_mut(y) else { return 0 };
        let n = t.chars().count();
        let max = max.min(self.w.saturating_sub(x));
        for (i, ch) in t.chars().enumerate().take(max) {
            line[x + i] = (if n > max && i + 1 == max { '…' } else { ch }, st);
        }
        n.min(max)
    }
    fn style(&mut self, r: Rect, f: impl Fn(&mut St)) {
        for y in r.y..(r.y + r.h).min(self.cells.len()) {
            for x in r.x..(r.x + r.w).min(self.w) {
                f(&mut self.cells[y][x].1);
            }
        }
    }
    /// Cursor moves and SGR, each line from its first column, trailing blanks dropped.
    fn emit(&self) -> String {
        let mut s = String::from("\x1b[?25l\x1b[H\x1b[2J");
        for (y, line) in self.cells.iter().enumerate() {
            let end = line.iter().rposition(|c| *c != (' ', St::default())).map_or(0, |p| p + 1);
            if end == 0 { continue }
            s.push_str(&format!("\x1b[{};1H", y + 1));
            let mut cur = St::default();
            for &(ch, st) in &line[..end] {
                if st != cur {
                    s.push_str(&sgr(st));
                    cur = st;
                }
                s.push(ch);
            }
            s.push_str("\x1b[0m");
        }
        s
    }
}

fn sgr(st: St) -> String {
    let mut p = vec!["0".to_string()];
    if st.bold { p.push("1".into()) }
    if st.dim { p.push("2".into()) }
    if st.ul { p.push("4".into()) }
    if st.rev { p.push("7".into()) }
    if let Some(c) = st.fg { p.push(if c < 8 { format!("{}", 30 + c) } else { format!("{}", 82 + c) }) }
    format!("\x1b[{}m", p.join(";"))
}

fn enc(t: &str) -> String {
    t.replace('%', "%25").replace(',', "%2C").replace(';', "%3B")
}

fn words(t: &str, r: Rect, ty: Type, role: &str, weight: u16) -> String {
    format!("text,{},{},{},{},text={},font=ui,weight={weight},scale={},role={role}", r.x, r.y, r.w, r.h, enc(t), ty.scale())
}

impl El {
    fn node(&self) -> Node {
        let across = self.dir == Dir::Row;
        let fit = match &self.k {
            K::Text { text, ty, mono, .. } => (if mono.is_some() { text.chars().count() } else { ty.width(text.chars().count()) }, ty.rows()),
            K::Item { tree, depth, label, badge, .. } => (tokens::ITEM_LABEL + 2 * depth + 2 * *tree as usize + label.chars().count() + if badge.is_empty() { 1 } else { badge.len() + 2 }, 1),
            K::Button { label, .. } => (label.chars().count() + 4, 1),
            K::Badge { label } => (label.chars().count() + 2, 1),
            K::Tab { name, .. } => (name.chars().count() + 7, 1),
            K::PanelTab { label, .. } => (label.chars().count(), 1),
            K::Spans(s) => (s.iter().map(|x| x.0.chars().count()).sum(), 1),
            K::Activity { .. } => (2, 1),
            _ => (0, 0),
        };
        let fit = (fit.0 + self.pad[0] + self.pad[2], fit.1 + self.pad[1] + self.pad[3]);
        Node { dir: self.dir, w: self.w, h: self.h, gap: self.gap.cells(across), pad: self.pad, align: self.align, fit, max: self.max, children: self.kids.iter().map(El::node).collect() }
    }

    /// The tree on a `cols` x `rows` screen.
    pub fn render(&self, cols: usize, rows: usize) -> Screen {
        let rects = self.node().layout(Rect::new(0, 0, cols, rows));
        let mut out = Screen::default();
        for plain in [true, false] {
            let mut c = Canvas::new(cols, rows);
            let mut hints = Vec::new();
            self.draw(&rects, &mut 0, &mut c, &mut hints, plain);
            if plain { out.plain = c.emit() } else {
                out.text = c.emit();
                out.osc = format!("\x1b]7733;ui;{}\x07", hints.join(";"));
            }
        }
        out
    }

    fn draw(&self, rects: &[Rect], at: &mut usize, c: &mut Canvas, h: &mut Vec<String>, plain: bool) {
        let r = rects[*at];
        *at += 1;
        if let Some(s) = &self.surface {
            if plain && s.boxed && r.w >= 2 && r.h >= 2 {
                let d = St { dim: true, ..St::default() };
                c.put(r.x, r.y, &format!("╭{}╮", "─".repeat(r.w - 2)), d, r.w);
                c.put(r.x, r.y + r.h - 1, &format!("╰{}╯", "─".repeat(r.w - 2)), d, r.w);
                for y in r.y + 1..r.y + r.h - 1 {
                    c.put(r.x, y, "│", d, 1);
                    c.put(r.x + r.w - 1, y, "│", d, 1);
                }
            } else if !plain && r.w > 0 && r.h > 0 {
                let mut p = format!("panel,{},{},{},{},role={},r={},pad=0", r.x, r.y, r.w, r.h, s.role, s.r);
                if s.border { p.push_str(&format!(",border={}", role::BORDER)) }
                if s.shadow { p.push_str(",shadow=1") }
                h.push(p);
            }
        }
        for k in &self.kids {
            k.draw(rects, at, c, h, plain);
        }
        let i = r.inset(self.pad);
        if i.w == 0 || i.h == 0 { return }
        let dim = St { dim: true, ..St::default() };
        match &self.k {
            K::Box | K::Spacer => {}
            K::Text { text, ty, role: ro, mono } => {
                let st = mono.unwrap_or(St { bold: matches!(ty, Type::Title | Type::Heading | Type::Display), dim: *ty == Type::Caption, ..St::default() });
                c.put(i.x, i.y, text, st, i.w);
                if !plain && mono.is_none() {
                    h.push(words(text, Rect { h: ty.rows(), ..i }, *ty, ro, ty.weight()));
                }
            }
            K::Item { tree, depth, chevron, icon, fg, label, badge, selected } => {
                let ind = 2 * depth + if *tree { 2 } else { 0 };
                if let Some(open) = chevron {
                    c.put(i.x + ind - 1, i.y, if *open { "\u{f078}" } else { "\u{f054}" }, dim, 1);
                }
                let (ix, lx) = (i.x + ind + tokens::ITEM_ICON, i.x + ind + tokens::ITEM_LABEL);
                c.put(ix, i.y, icon, St { fg: *fg, ..St::default() }, 1);
                let bx = i.x + i.w - 1 - badge.chars().count();
                let lw = if badge.is_empty() { i.w } else { bx.saturating_sub(1) } .saturating_sub(lx - i.x);
                c.put(lx, i.y, label, St::default(), lw);
                if !badge.is_empty() { c.put(bx, i.y, badge, if *selected { St::default() } else { dim }, badge.len()); }
                if plain && *selected { c.style(i, |s| { s.rev = true; s.dim = false; s.fg = None }) }
                if !plain {
                    if *selected { h.push(format!("pill,{},{},{},1,role={},pad={}", i.x, i.y, i.w, role::PRIMARY_CONTAINER, tokens::PILL_INSET)) }
                    let ro = if *selected { role::ON_PRIMARY_CONTAINER } else { role::ON };
                    h.push(words(label, Rect::new(lx, i.y, lw, 1), Type::Body, ro, if *selected { 600 } else { 450 }));
                }
            }
            K::Button { label, primary } => {
                let w = label.chars().count() + 4;
                if plain {
                    if *primary { c.put(i.x, i.y, &format!("  {label}  "), St { rev: true, bold: true, ..St::default() }, w) } else { c.put(i.x, i.y, &format!("[ {label} ]"), St::default(), w) };
                } else {
                    c.put(i.x + 2, i.y, label, St::default(), w);
                    let ro = if *primary { role::PRIMARY } else { role::LAYER3 };
                    h.push(format!("pill,{},{},{},1,role={ro},text={},font=ui,weight=600,pad=0", i.x, i.y, w.min(i.w), enc(label)));
                }
            }
            K::Badge { label } => {
                c.put(i.x, i.y, &format!(" {label} "), St { rev: plain, bold: true, ..St::default() }, i.w);
                if !plain { h.push(format!("pill,{},{},{},1,role={},text={},font=mono,weight=700,pad=0.1", i.x, i.y, i.w, role::PRIMARY, enc(label))) }
            }
            K::Divider { across } => if plain {
                if *across { c.put(i.x, i.y, &"─".repeat(i.w), dim, i.w); } else { for y in i.y..i.y + i.h { c.put(i.x, y, "│", dim, 1); } }
            }
            K::Activity { icon, active } => {
                c.put(i.x + 1, i.y, icon, St { dim: !active, bold: *active, ..St::default() }, 1);
                if *active {
                    if plain { c.put(i.x, i.y, "▌", St { fg: Some(CYAN), ..St::default() }, 1); } else { h.push(format!("pill,{},{},2,1,role={},pad=0.4", i.x + 1, i.y, role::PRIMARY_CONTAINER)) }
                }
            }
            K::Tab { icon, fg, name, active, dirty } => {
                if !plain && *active { h.push(format!("panel,{},{},{},1,role={},r={},pad=0", i.x, i.y, i.w, role::LAYER0, tokens::radius::VERYSMALL)) }
                c.put(i.x + 2, i.y, icon, St { fg: *fg, ..St::default() }, 1);
                let st = St { bold: *active && plain, dim: !active, ..St::default() };
                let n = c.put(i.x + 4, i.y, name, st, i.w.saturating_sub(6));
                c.put(i.x + 5 + n, i.y, if *dirty { "●" } else { "×" }, if *dirty { St::default() } else { dim }, 1);
                if plain { c.put(i.x + i.w - 1, i.y, "│", dim, 1); }
                if !plain { h.push(words(name, Rect::new(i.x + 4, i.y, n, 1), Type::Body, if *active { role::ON } else { role::SUBTEXT }, if *active { 500 } else { 400 })) }
            }
            K::PanelTab { label, active } => {
                c.put(i.x, i.y, label, St { ul: *active && plain, dim: !active, bold: *active, ..St::default() }, i.w);
                if !plain { h.push(words(label, i, Type::Caption, if *active { role::ON } else { role::SUBTEXT }, if *active { 650 } else { 500 })) }
            }
            K::Spans(s) => {
                let mut x = i.x;
                for Span(t, st) in s {
                    x += c.put(x, i.y, t, *st, (i.x + i.w).saturating_sub(x));
                }
            }
            K::Editor { lines, current } => {
                let digits = lines.len().max(1).to_string().len().max(3);
                for (n, l) in lines.iter().enumerate().take(i.h) {
                    let y = i.y + n;
                    let cur = n == *current;
                    c.put(i.x, y, &format!("{:>digits$} ", n + 1), if cur { St { fg: Some(CYAN), bold: true, ..St::default() } } else { dim }, digits + 1);
                    let mut x = i.x + digits + 2;
                    for Span(t, st) in l {
                        x += c.put(x, y, t, *st, (i.x + i.w).saturating_sub(x));
                    }
                    if cur && !plain { h.push(format!("panel,{},{y},{},1,role={},r=0,pad=0", i.x, i.w, role::HIGHLIGHT)) }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plain rendering back as text lines: cursor moves followed, SGR dropped, trailing blanks cut.
    fn lines(s: &str) -> Vec<String> {
        let mut grid: Vec<Vec<char>> = Vec::new();
        let (mut r, mut c) = (0, 0);
        let mut it = s.chars().peekable();
        while let Some(ch) = it.next() {
            if ch == '\x1b' && it.peek() == Some(&'[') {
                it.next();
                let mut p = String::new();
                while let Some(&x) = it.peek() { it.next(); if x.is_ascii_alphabetic() { if x == 'H' { let mut n = p.split(';').map(|v| v.parse::<usize>().unwrap_or(1)); r = n.next().unwrap_or(1).max(1) - 1; c = n.next().unwrap_or(1).max(1) - 1; } break } p.push(x) }
                continue;
            }
            grid.resize(grid.len().max(r + 1), Vec::new());
            let l = &mut grid[r];
            l.resize(l.len().max(c + 1), ' ');
            l[c] = ch;
            c += 1;
        }
        grid.iter().map(|l| l.iter().collect::<String>().trim_end().to_string()).collect()
    }

    #[test]
    fn sidebar_plain_and_hints() {
        let ui = row([sidebar(16, [title("zett"), list(Some(0), [item("I", "Inbox").badge("12"), item("T", "Today")])]), divider(false), column([heading("Hi")]).pad([2, 1, 0, 0])]);
        let s = ui.render(40, 8);
        let l = lines(&s.plain);
        assert_eq!(l[1], " zett           │  Hi");
        assert_eq!(l[3], "  I  Inbox  12  │");
        assert_eq!(l[4], "  T  Today      │");
        // hints: the sidebar's surface, the pill on the chosen row, the heading over two rows
        assert!(s.osc.starts_with("\x1b]7733;ui;panel,0,0,16,8,role=surface-container-high,r=0,pad=0;"));
        assert!(s.osc.contains("pill,1,3,14,1,role=primary-container,pad=0.3"));
        assert!(s.osc.contains("text,19,1,4,2,text=Hi,font=ui,weight=600,scale=1.6"));
        // zash's cells have no reverse video; the plain ones do
        assert!(s.plain.contains(";7m") && !s.text.contains(";7m"));
    }

    #[test]
    fn card_box_buttons_and_tree() {
        let s = card([title("Ship"), buttons([button("Open", true), button("Later", false)])]).render(24, 6);
        let l = lines(&s.plain);
        assert_eq!(l[0], "╭──────────────────────╮");
        assert_eq!(l[2], "│   Open   [ Later ]   │");
        assert_eq!(l[5], "╰──────────────────────╯");
        let t = tree([folder("src", true, [file("main.rs").selected()]), folder("docs", false, [file("x.md")]), file("README.md")]).render(20, 4);
        let l = lines(&t.plain);
        assert_eq!(l.len(), 4, "a closed folder hides its children");
        assert!(!t.plain.contains("x.md"));
        assert!(l[1].starts_with("     \u{e7a8}   main.rs") || l[1].contains("main.rs"));
        assert!(t.osc.contains("pill,0,1,20,1"));
    }

    #[test]
    fn ui_caches_until_resize_or_change() {
        let mut u = Ui::new(body("a"));
        let a = u.render(10, 2).clone();
        assert_eq!(&a, u.render(10, 2));
        u.set(body("b"));
        assert_ne!(&a, u.render(10, 2));
    }
}
