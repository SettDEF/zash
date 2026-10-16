//! Drawing a terminal: rectangles (backgrounds, selection, underlines, the
//! cursor) through a tiny instanced pipeline, text through glyphon.
//!
//! Each screen row is one shaped text line, reshaped only when its content
//! changed (a hash per row), so a blinking prompt or a single typed key costs
//! one line of shaping, not a screen. The terminal's lock is held only while a
//! frame is copied out (`Frame::extract`); shaping and drawing happen after.

use crate::config::{Config, Palette, Rgb};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::{Color as AColor, CursorShape, NamedColor};
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, Style, SwashCache, TextArea, TextAtlas, TextBounds,
    TextRenderer, Viewport, Weight,
};
use std::hash::{Hash, Hasher};

// ── a frame: plain data copied out of the terminal ──

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Style2 {
    pub fg: (u8, u8, u8),
    pub bold: bool,
    pub italic: bool,
}

pub struct CellView {
    pub ch: char,
    pub zw: Option<Vec<char>>,
    pub style: Style2,
    pub bg: Option<Rgb>,
    pub underline: bool,
    pub strike: bool,
    pub wide: bool,
    pub spacer: bool,
}

pub struct Frame {
    /// smooth scrolling: rows just above and below the screen (nearest first), and how
    /// far the content is drawn shifted (pixels, + = down) while a scroll glides
    pub above: Vec<Vec<CellView>>,
    pub below: Vec<Vec<CellView>>,
    pub scroll_px: f32,
    /// the shell plugin's completion popup, drawn at the cursor
    pub popup: Option<crate::session::Popup>,
    /// tab titles and which one is shown (drawn as a bar when there are two or more)
    pub tabs: Vec<(String, bool)>,
    /// a bar drawn over the terminal at the top right (search)
    pub bar: Option<String>,
    pub bar_bg: Rgb,
    pub fg: Rgb,
    /// prompt rows on screen with the exit status of the command before them (shell marks)
    pub prompts: Vec<(usize, Option<i32>)>,
    pub cols: usize,
    pub rows: Vec<Vec<CellView>>,
    pub cursor: Option<(usize, usize, CursorShape)>,
    pub bg: Rgb,
    pub cursor_color: Rgb,
    pub fail_color: Rgb,
}

fn resolve(c: AColor, term_colors: &alacritty_terminal::term::color::Colors, p: &Palette, bold: bool) -> Rgb {
    let over = |i: usize| term_colors[i].map(|c| Rgb(c.r, c.g, c.b));
    match c {
        AColor::Spec(rgb) => Rgb(rgb.r, rgb.g, rgb.b),
        AColor::Indexed(i) => {
            // bold text in the first 8 colours brightens, like most terminals
            let i = if bold && i < 8 { i + 8 } else { i };
            over(i as usize).unwrap_or_else(|| p.indexed(i))
        }
        AColor::Named(n) => {
            let idx = n as usize;
            if let Some(c) = over(idx) {
                return c;
            }
            match n {
                NamedColor::Foreground | NamedColor::BrightForeground => p.fg,
                NamedColor::Background => p.bg,
                NamedColor::Cursor => p.cursor_bg,
                NamedColor::DimForeground => dim(p.fg),
                n if (n as usize) < 16 => {
                    let i = n as usize;
                    p.ansi[if bold && i < 8 { i + 8 } else { i }]
                }
                n if (n as usize) >= NamedColor::DimBlack as usize => dim(p.ansi[(n as usize - NamedColor::DimBlack as usize).min(7)]),
                _ => p.fg,
            }
        }
    }
}

fn dim(c: Rgb) -> Rgb {
    Rgb((c.0 as f32 * 0.66) as u8, (c.1 as f32 * 0.66) as u8, (c.2 as f32 * 0.66) as u8)
}

impl Frame {
    pub fn extract<T: alacritty_terminal::event::EventListener>(term: &Term<T>, p: &Palette) -> Frame {
        let content = term.renderable_content();
        let cols = term.columns();
        let lines = term.screen_lines();
        let offset = content.display_offset as i32;
        let mut rows: Vec<Vec<CellView>> = (0..lines).map(|_| Vec::with_capacity(cols)).collect();
        let sel = content.selection;
        for ic in content.display_iter {
            let row = (ic.point.line.0 + offset) as usize;
            if row >= lines {
                continue;
            }
            rows[row].push(cell_view(ic.cell, ic.point, content.colors, p, sel));
        }
        let cursor = (content.cursor.shape != CursorShape::Hidden).then(|| {
            let Point { line: Line(l), column: Column(c) } = content.cursor.point;
            ((l + offset) as usize, c, content.cursor.shape)
        }).filter(|(r, _, _)| *r < lines);
        if let Some((r, c, shape)) = cursor {
            if shape == CursorShape::Block {
                if let Some(cell) = rows[r].get_mut(c) {
                    cell.style.fg = (p.cursor_fg.0, p.cursor_fg.1, p.cursor_fg.2);
                }
            }
        }
        Frame { above: Vec::new(), below: Vec::new(), scroll_px: 0.0, popup: None, tabs: Vec::new(), bar: None, bar_bg: p.selection_bg, fg: p.fg, prompts: Vec::new(), cols, rows, cursor, bg: p.bg, cursor_color: p.cursor_bg, fail_color: p.ansi[1] }
    }

    /// The screen as plain text (headless checks and tests).
    pub fn text(&self) -> String {
        self.rows.iter().map(|r| {
            let s: String = r.iter().filter(|c| !c.spacer).map(|c| c.ch).collect();
            s.trim_end().to_string()
        }).collect::<Vec<_>>().join("\n")
    }
}

// ── rectangles ──

const RECT_WGSL: &str = r#"
struct U { screen: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
@vertex fn vs(@builtin(vertex_index) vi: u32, @location(0) rect: vec4<f32>, @location(1) color: vec4<f32>) -> VOut {
    var corners = array<vec2<f32>, 6>(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0));
    let p = rect.xy + corners[vi] * rect.zw;
    var o: VOut;
    o.pos = vec4(p.x / u.screen.x * 2.0 - 1.0, 1.0 - p.y / u.screen.y * 2.0, 0.0, 1.0);
    o.color = color;
    return o;
}
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> { return i.color; }
"#;

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    xywh: [f32; 4],
    color: [f32; 4],
}

struct Rects {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    buf: wgpu::Buffer,
    cap: usize,
    count: u32,
}

impl Rects {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Rects {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("zash rects"), source: wgpu::ShaderSource::Wgsl(RECT_WGSL.into()) });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &bgl, entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }] });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl)], ..Default::default() });
        let attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("zash rects"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: 32, step_mode: wgpu::VertexStepMode::Instance, attributes: &attrs })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let cap = 4096;
        let buf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (cap * 32) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        Rects { pipeline, uniform, bind, buf, cap, count: 0 }
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, rects: &[Rect], w: f32, h: f32) {
        queue.write_buffer(&self.uniform, 0, &as_bytes(&[w, h, 0.0, 0.0]));
        if rects.len() > self.cap {
            self.cap = rects.len().next_power_of_two();
            self.buf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (self.cap * 32) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        }
        let flat: Vec<f32> = rects.iter().flat_map(|r| r.xywh.into_iter().chain(r.color)).collect();
        if !flat.is_empty() {
            queue.write_buffer(&self.buf, 0, &as_bytes(&flat));
        }
        self.count = rects.len() as u32;
    }

    fn draw(&self, pass: &mut wgpu::RenderPass) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.buf.slice(..));
        pass.draw(0..6, 0..self.count);
    }
}

fn as_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_ne_bytes()).collect()
}

// ── the renderer ──

/// One pane to draw: where it is in the window (pixels) and what it shows.
pub struct PaneView<'a> {
    pub id: u64,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub frame: &'a Frame,
    pub focused: bool,
}

fn dim6(c: Rgb) -> Rgb {
    Rgb((c.0 as f32 * 0.6) as u8, (c.1 as f32 * 0.6) as u8, (c.2 as f32 * 0.6) as u8)
}

pub struct Renderer {
    pub font_system: FontSystem,
    swash: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text: TextRenderer,
    /// text drawn above the overlay (the search bar)
    text2: TextRenderer,
    bars: Vec<(Buffer, f32, f32)>,
    /// height of the tab bar above the rows (0 with one tab)
    pub top: f32,
    rects: Rects,
    /// drawn after the text: marks that must stay visible over glyphs
    overlay: Rects,
    /// shaped rows per pane (by pane id)
    rows: std::collections::HashMap<u64, std::collections::HashMap<u64, Buffer>>,
    /// per pane: the content key of each drawn row, top to bottom (above the screen first)
    order: std::collections::HashMap<u64, Vec<u64>>,
    family: String,
    features: glyphon::cosmic_text::FontFeatures,
    pub font_px: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub pad: f32,
    /// cursor animation: time constant in ms (0: jumps), and the trailing smear
    pub cursor_ms: f32,
    pub cursor_trail: bool,
    /// background opacity (pre-multiplied into the clear colour)
    pub opacity: f32,
    anim: Option<CursorAnim>,
}

/// The drawn cursor chases its real position: the head quickly, the tail a
/// little slower (the smear). Exponential, so it is the same at any frame rate.
struct CursorAnim {
    head: (f32, f32),
    tail: (f32, f32),
    target: (f32, f32),
    last: std::time::Instant,
}

impl Renderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat, cfg: &Config, scale: f32) -> Renderer {
        let mut font_system = FontSystem::new();
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let text = TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        let text2 = TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        let mut features = glyphon::cosmic_text::FontFeatures::new();
        for (tag, v) in &cfg.features {
            let b = tag.as_bytes();
            if b.len() == 4 {
                features.set(glyphon::cosmic_text::FeatureTag::new(&[b[0], b[1], b[2], b[3]]), *v);
            }
        }
        let mut r = Renderer {
            rects: Rects::new(device, format),
            overlay: Rects::new(device, format),
            font_system: { let _ = &mut font_system; font_system },
            swash: SwashCache::new(),
            viewport,
            atlas,
            text,
            text2,
            bars: Vec::new(),
            top: 0.0,
            rows: std::collections::HashMap::new(),
            order: std::collections::HashMap::new(),
            family: cfg.font_family.clone(),
            features,
            font_px: 0.0,
            cell_w: 0.0,
            cell_h: 0.0,
            pad: 0.0,
            cursor_ms: cfg.cursor_ms,
            cursor_trail: cfg.cursor_trail,
            opacity: cfg.opacity,
            anim: None,
        };
        r.set_font(cfg.font_size_pt, cfg.line_height, cfg.padding, scale);
        r
    }

    /// Font size in points (96 dpi, as WezTerm counts them) at a display scale.
    pub fn set_font(&mut self, size_pt: f32, line_height: f32, padding: f32, scale: f32) {
        self.font_px = size_pt * 96.0 / 72.0 * scale;
        self.cell_h = (self.font_px * line_height).round();
        self.pad = (padding * scale).round();
        // the advance of a cell, measured on the real font
        let mut b = Buffer::new(&mut self.font_system, Metrics::new(self.font_px, self.cell_h));
        b.set_size(None, None);
        let attrs = Attrs::new().family(Family::Name(&self.family));
        b.set_text("MMMMMMMMMM", &attrs, Shaping::Advanced, None);
        b.shape_until_scroll(&mut self.font_system, false);
        let w = b.layout_runs().next().map(|r| r.line_w / 10.0).unwrap_or(self.font_px * 0.6);
        self.cell_w = w;
        self.rows.clear();
    }

    /// Forget shaped rows (colours changed).
    pub fn invalidate(&mut self) {
        self.rows.clear();
    }

    /// The tab bar's height.
    pub fn tab_bar_h(&self) -> f32 {
        (self.cell_h * 1.6).round()
    }

    /// Cells that fit in a pane of this many pixels (padding inside it).
    pub fn grid_in(&self, w: f32, h: f32) -> (usize, usize) {
        let cols = ((w - 2.0 * self.pad) / self.cell_w).floor().max(2.0) as usize;
        let rows = ((h - 2.0 * self.pad) / self.cell_h).floor().max(1.0) as usize;
        (cols, rows)
    }

    /// Shapes the rows of a pane: above the screen, the screen, below it (top to bottom).
    /// The cache is keyed by a row's content, not its position: when the screen scrolls,
    /// rows that only moved are not shaped again.
    fn shape_rows(&mut self, pane: u64, frame: &Frame, width: f32) {
        let mut old = self.rows.remove(&pane).unwrap_or_default();
        let mut cache: std::collections::HashMap<u64, Buffer> = std::collections::HashMap::with_capacity(frame.rows.len() + 4);
        let mut order = Vec::with_capacity(frame.rows.len() + frame.above.len() + frame.below.len());
        let all = frame.above.iter().rev().chain(frame.rows.iter()).chain(frame.below.iter());
        for row in all {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.font_px.to_bits().hash(&mut h);
            width.to_bits().hash(&mut h);
            for c in row {
                c.ch.hash(&mut h);
                c.zw.hash(&mut h);
                c.style.hash(&mut h);
                c.spacer.hash(&mut h);
            }
            let key = h.finish();
            order.push(key);
            if cache.contains_key(&key) {
                continue;
            }
            if let Some(b) = old.remove(&key) {
                cache.insert(key, b);
                continue;
            }
            // runs of cells with one style become one span
            let mut spans: Vec<(String, Style2)> = Vec::new();
            for c in row.iter().filter(|c| !c.spacer) {
                let mut s = String::new();
                s.push(if c.ch == '\0' { ' ' } else { c.ch });
                if let Some(z) = &c.zw {
                    s.extend(z.iter());
                }
                match spans.last_mut() {
                    Some((t, st)) if *st == c.style => t.push_str(&s),
                    _ => spans.push((s, c.style)),
                }
            }
            let family = self.family.clone();
            let feats = self.features.clone();
            let mk = |st: &Style2| {
                let mut a = Attrs::new().family(Family::Name(&family)).color(Color::rgb(st.fg.0, st.fg.1, st.fg.2)).font_features(feats.clone());
                if st.bold {
                    a = a.weight(Weight::BOLD);
                }
                if st.italic {
                    a = a.style(Style::Italic);
                }
                a
            };
            let mut b = Buffer::new(&mut self.font_system, Metrics::new(self.font_px, self.cell_h));
            b.set_size(Some(width), Some(self.cell_h));
            let default = Attrs::new().family(Family::Name(&family));
            b.set_rich_text(spans.iter().map(|(t, st)| (t.as_str(), mk(st))), &default, Shaping::Advanced, None);
            b.shape_until_scroll(&mut self.font_system, false);
            cache.insert(key, b);
        }
        self.rows.insert(pane, cache);
        self.order.insert(pane, order);
    }

    fn overlay_list(&self, frame: &Frame, ox: f32, oy: f32, clip: [f32; 4]) -> Vec<Rect> {
        let ch = self.cell_h;
        let oy = oy + frame.scroll_px;
        let mut out = Vec::new();
        // prompt marks: a thin bar at the left edge, red when the command before failed
        for (r, exit) in &frame.prompts {
            let failed = exit.is_some_and(|e| e != 0);
            let mut col = if failed { frame.fail_color.f32() } else { frame.cursor_color.f32() };
            col[3] = if failed { 1.0 } else { 0.7 };
            let w = (self.font_px / 7.0).max(2.0).round();
            out.push(Rect { xywh: [ox - self.pad, oy + *r as f32 * ch, w, ch], color: col });
        }
        clipped(out, clip)
    }

    fn rect_list(&self, frame: &Frame, cur: Option<((f32, f32), (f32, f32))>, ox: f32, oy: f32, clip: [f32; 4]) -> Vec<Rect> {
        let (cw, ch, pad) = (self.cell_w, self.cell_h, ox);
        // while a scroll glides everything is drawn shifted; rows beyond the screen fill the edge
        let oy = oy + frame.scroll_px;
        let cur = cur.map(|((hx, hy), (tx, ty))| ((hx, hy + frame.scroll_px), (tx, ty + frame.scroll_px)));
        let mut out = Vec::new();
        let first = -(frame.above.len() as i64);
        let all = frame.above.iter().rev().chain(frame.rows.iter()).chain(frame.below.iter());
        for (k, row) in all.enumerate() {
            let y = oy + (first + k as i64) as f32 * ch;
            if y + ch < clip[1] || y > clip[1] + clip[3] {
                continue;
            }
            // backgrounds, merged across cells of one colour
            let mut run: Option<(usize, usize, Rgb)> = None;
            for (c, cell) in row.iter().enumerate() {
                match (run, cell.bg) {
                    (Some((s, e, col)), Some(b)) if b == col && e == c => run = Some((s, c + 1, col)),
                    (prev, bg) => {
                        if let Some((s, e, col)) = prev {
                            out.push(Rect { xywh: [pad + s as f32 * cw, y, (e - s) as f32 * cw, ch], color: col.f32() });
                        }
                        run = bg.map(|b| (c, c + 1, b));
                    }
                }
                let fg = Rgb(cell.style.fg.0, cell.style.fg.1, cell.style.fg.2).f32();
                let w = if cell.wide { 2.0 * cw } else { cw };
                let thick = (self.font_px / 14.0).max(1.0).round();
                if cell.underline {
                    out.push(Rect { xywh: [pad + c as f32 * cw, y + ch - thick * 2.0, w, thick], color: fg });
                }
                if cell.strike {
                    out.push(Rect { xywh: [pad + c as f32 * cw, y + ch * 0.55, w, thick], color: fg });
                }
            }
            if let Some((s, e, col)) = run {
                out.push(Rect { xywh: [pad + s as f32 * cw, y, (e - s) as f32 * cw, ch], color: col.f32() });
            }
        }
        if let Some((r, c, shape)) = frame.cursor {
            let (x, y) = cur.map(|c| c.0).unwrap_or((pad + c as f32 * cw, oy + r as f32 * ch));
            // the smear: from the lagging tail to the head, along one line only
            if let Some((_, (tx, ty))) = cur.filter(|_| self.cursor_trail) {
                if (ty - y).abs() < 1.0 && (tx - x).abs() > 1.0 {
                    let (a, b) = (tx.min(x), tx.max(x));
                    let mut col = frame.cursor_color.f32();
                    col[3] = 0.35;
                    out.push(Rect { xywh: [a, y + ch * 0.15, b - a, ch * 0.7], color: col });
                }
            }
            let wide = frame.rows.get(r).and_then(|row| row.get(c)).is_some_and(|cell| cell.wide);
            let w = if wide { 2.0 * cw } else { cw };
            let col = frame.cursor_color.f32();
            let t = (self.font_px / 9.0).max(1.0).round();
            match shape {
                CursorShape::Beam => out.push(Rect { xywh: [x, y, t, ch], color: col }),
                CursorShape::Underline => out.push(Rect { xywh: [x, y + ch - t, w, t], color: col }),
                CursorShape::HollowBlock => {
                    out.push(Rect { xywh: [x, y, w, t], color: col });
                    out.push(Rect { xywh: [x, y + ch - t, w, t], color: col });
                    out.push(Rect { xywh: [x, y, t, ch], color: col });
                    out.push(Rect { xywh: [x + w - t, y, t, ch], color: col });
                }
                _ => {
                    // a block; the character on it is drawn in the cursor's text colour (Frame::extract)
                    out.push(Rect { xywh: [x, y, w, ch], color: col });
                }
            }
        }
        clipped(out, clip)
    }

    /// Draws the panes of a tab into `view` (a window surface or an offscreen texture).
    /// The focused pane carries the chrome: tab bar, completion popup, search bar.
    /// Returns true while something still animates (the caller asks for another frame).
    pub fn render(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView, width: u32, height: u32, panes: &[PaneView]) -> bool {
        let Some(focus) = panes.iter().find(|p| p.focused).or(panes.first()) else { return false };
        let frame = focus.frame;
        // shaped rows of panes that are gone are dropped
        self.rows.retain(|id, _| panes.iter().any(|p| p.id == *id));
        self.order.retain(|id, _| panes.iter().any(|p| p.id == *id));
        let (fox, foy) = (focus.x + self.pad, focus.y + self.pad);
        let (cur, animating) = self.cursor_motion(frame, fox, foy);
        let mut rects = Vec::new();
        let mut over = Vec::new();
        for p in panes {
            self.shape_rows(p.id, p.frame, p.w - 2.0 * self.pad);
            let (ox, oy) = (p.x + self.pad, p.y + self.pad);
            let clip = [p.x, p.y.max(self.top), p.w, p.y + p.h - p.y.max(self.top)];
            rects.extend(self.rect_list(p.frame, if p.focused { cur } else { None }, ox, oy, clip));
            over.extend(self.overlay_list(p.frame, ox, oy, clip));
        }
        if panes.len() > 1 {
            let line = { let mut c = frame.fg.f32(); c[3] = 0.16; c };
            let shade = { let mut c = frame.bg.f32(); c[3] = 0.32; c };
            for p in panes {
                // a hairline where a pane meets its left or upper neighbour
                if p.x > 0.5 {
                    over.push(Rect { xywh: [p.x.round() - 1.0, p.y, 1.0, p.h], color: line });
                }
                if p.y > self.top + 0.5 {
                    over.push(Rect { xywh: [p.x, p.y.round() - 1.0, p.w, 1.0], color: line });
                }
                // panes without the keyboard step back a little
                if !p.focused {
                    over.push(Rect { xywh: [p.x, p.y, p.w, p.h], color: shade });
                }
            }
        }
        self.rects.upload(device, queue, &rects, width as f32, height as f32);
        // labels drawn above everything: the tab bar, the popup and the search bar
        let mut labels: Vec<(Buffer, f32, f32)> = Vec::new();
        let label = |fs: &mut FontSystem, family: &str, txt: &str, fg: Rgb, px: f32, h: f32| {
            let mut b = Buffer::new(fs, Metrics::new(px, h));
            b.set_size(None, Some(h));
            b.set_text(txt, &Attrs::new().family(Family::Name(family)).color(Color::rgb(fg.0, fg.1, fg.2)), Shaping::Advanced, None);
            b.shape_until_scroll(fs, false);
            let w = b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
            (b, w)
        };
        if frame.tabs.len() > 1 {
            // the tab bar: one pill per tab, the active one lifted
            let h = self.top;
            let n = frame.tabs.len() as f32;
            let tw = ((width as f32 - self.cell_w) / n).min(self.cell_w * 28.0);
            for (i, (title, active)) in frame.tabs.iter().enumerate() {
                let x = self.cell_w * 0.5 + i as f32 * tw;
                if *active {
                    over.push(Rect { xywh: [x + 2.0, h * 0.12, tw - 4.0, h * 0.76], color: frame.bar_bg.f32() });
                }
                let max_chars = ((tw / self.cell_w) as usize).saturating_sub(3).max(4);
                let mut t: String = title.chars().take(max_chars).collect();
                if title.chars().count() > max_chars {
                    t.pop();
                    t.push('…');
                }
                let fg = if *active { frame.fg } else { dim6(frame.fg) };
                let (b, w) = label(&mut self.font_system, &self.family, &t, fg, self.font_px * 0.9, self.cell_h);
                labels.push((b, x + (tw - w) / 2.0, (h - self.cell_h) / 2.0));
            }
        }
        if let (Some(pp), Some((cr, cc, _))) = (&frame.popup, frame.cursor) {
            // the completion popup: under the word being completed, above it when there is no room
            let n = pp.items.len();
            let row_h = (self.cell_h * 1.25).round();
            let (left, right, bottom) = (focus.x + self.pad, focus.x + focus.w - self.pad, height as f32 - self.pad);
            // as many rows as fit on the roomier side of the cursor line (up to 10)
            let line_y = foy + cr as f32 * self.cell_h;
            let above_px = line_y - self.top;
            let below_px = bottom - line_y - self.cell_h;
            let room = above_px.max(below_px) - self.cell_h;
            let mut show: usize = ((room / row_h) as usize).clamp(1, 10);
            if n > show {
                // the footer ("3 of 40 · Tab next") takes a row too
                show = show.saturating_sub(1).max(1);
            }
            let first = match pp.sel { Some(s) if s >= show => s + 1 - show, _ => 0 };
            let rows: Vec<usize> = (first..n.min(first + show)).collect();
            let widest = rows.iter().map(|i| pp.items[*i].chars().count()).max().unwrap_or(0);
            let more = n > show;
            let footer_w = if more { 34 } else { 0 };
            let pw = ((widest.max(12).max(footer_w) + 3) as f32 * self.cell_w).min(width as f32 - 2.0 * self.pad);
            let ph = rows.len() as f32 * row_h + self.cell_h * 0.5 + if more { row_h } else { 0.0 };
            let wx = fox + cc.saturating_sub(pp.word) as f32 * self.cell_w - self.cell_w;
            let x = wx.clamp(left.min(width as f32 - pw - self.pad).max(self.pad), (right.max(left + pw) - pw).min(width as f32 - pw - self.pad).max(self.pad));
            let below = line_y + self.cell_h + 2.0;
            let y = if below + ph <= bottom || below_px >= above_px { below } else { (line_y - ph - 4.0).max(self.top) };
            let edge = frame.fg.f32();
            over.push(Rect { xywh: [x - 1.0, y - 1.0, pw + 2.0, ph + 2.0], color: [edge[0], edge[1], edge[2], 0.18] });
            over.push(Rect { xywh: [x, y, pw, ph], color: frame.bar_bg.f32() });
            let dim = dim6(frame.fg);
            for (k, i) in rows.iter().enumerate() {
                let ry = y + self.cell_h * 0.25 + k as f32 * row_h;
                let chosen = pp.sel == Some(*i);
                if chosen {
                    let c = frame.cursor_color.f32();
                    over.push(Rect { xywh: [x + 3.0, ry, pw - 6.0, row_h], color: [c[0], c[1], c[2], 0.28] });
                }
                let (b, _) = label(&mut self.font_system, &self.family, &pp.items[*i], if chosen || pp.sel.is_none() { frame.fg } else { dim }, self.font_px, self.cell_h);
                labels.push((b, x + self.cell_w, ry + (row_h - self.cell_h) / 2.0));
            }
            if more {
                let ry = y + self.cell_h * 0.25 + rows.len() as f32 * row_h;
                let t = format!("{} of {n}  ·  Tab next, Shift+Tab back", pp.sel.map(|s| s + 1).unwrap_or(0));
                let (b, _) = label(&mut self.font_system, &self.family, &t, dim, self.font_px * 0.85, self.cell_h);
                labels.push((b, x + self.cell_w, ry + (row_h - self.cell_h) / 2.0));
            }
        }
        if let Some(txt) = &frame.bar {
            let (b, w) = label(&mut self.font_system, &self.family, txt, frame.fg, self.font_px, self.cell_h);
            let (bw, bh) = (w + 2.0 * self.cell_w, self.cell_h * 1.5);
            // bottom right of the focused pane: found lines are scrolled to the top, so the bar never covers them
            let (x, y) = ((focus.x + focus.w - bw - self.cell_w).max(focus.x), focus.y + focus.h - bh - self.cell_h * 0.5);
            over.push(Rect { xywh: [x, y, bw, bh], color: frame.bar_bg.f32() });
            labels.push((b, x + self.cell_w, y + self.cell_h * 0.25));
        }
        self.bars = labels;
        self.overlay.upload(device, queue, &over, width as f32, height as f32);
        self.viewport.update(queue, Resolution { width, height });
        let ch = self.cell_h;
        let mut areas: Vec<TextArea> = Vec::new();
        for p in panes {
            let (Some(cache), Some(order)) = (self.rows.get(&p.id), self.order.get(&p.id)) else { continue };
            let (ox, oy) = (p.x + self.pad, p.y + self.pad + p.frame.scroll_px);
            let first = -(p.frame.above.len() as i64);
            // each pane's text stays inside its pane (and out of the tab bar)
            let bounds = TextBounds { left: p.x as i32, top: (p.y.max(self.top)) as i32, right: (p.x + p.w) as i32, bottom: (p.y + p.h) as i32 };
            areas.extend(order.iter().enumerate().filter_map(|(i, key)| cache.get(key).map(|b| (i, b))).map(|(i, b)| TextArea {
                buffer: b,
                left: ox,
                top: oy + (first + i as i64) as f32 * ch,
                scale: 1.0,
                bounds,
                default_color: Color::rgb(255, 255, 255),
                custom_glyphs: &[],
            }));
        }
        let _ = self.text.prepare(device, queue, &mut self.font_system, &mut self.atlas, &self.viewport, areas, &mut self.swash);
        let bar_areas: Vec<TextArea> = self.bars.iter().map(|(b, x, y)| TextArea {
            buffer: b,
            left: *x,
            top: *y,
            scale: 1.0,
            bounds: TextBounds { left: 0, top: 0, right: width as i32, bottom: height as i32 },
            default_color: Color::rgb(255, 255, 255),
            custom_glyphs: &[],
        }).collect();
        let _ = self.text2.prepare(device, queue, &mut self.font_system, &mut self.atlas, &self.viewport, bar_areas, &mut self.swash);

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("zash frame") });
        {
            let bg = frame.bg.f32();
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear({
                            // pre-multiplied: the compositor blends (and blurs) what is behind
                            let o = self.opacity as f64;
                            wgpu::Color { r: bg[0] as f64 * o, g: bg[1] as f64 * o, b: bg[2] as f64 * o, a: o }
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.rects.draw(&mut pass);
            let _ = self.text.render(&self.atlas, &self.viewport, &mut pass);
            self.overlay.draw(&mut pass);
            let _ = self.text2.render(&self.atlas, &self.viewport, &mut pass);
        }
        queue.submit(Some(enc.finish()));
        self.atlas.trim();
        animating
    }

    fn cursor_motion(&mut self, frame: &Frame, ox: f32, oy: f32) -> (Option<((f32, f32), (f32, f32))>, bool) {
        let target = frame.cursor.map(|(r, c, _)| (ox + c as f32 * self.cell_w, oy + r as f32 * self.cell_h));
        let (Some(t), true) = (target, self.cursor_ms > 0.0) else {
            self.anim = None;
            return (None, false);
        };
        let now = std::time::Instant::now();
        let a = self.anim.get_or_insert(CursorAnim { head: t, tail: t, target: t, last: now });
        if a.target != t {
            // a far jump (another screen, a distant line) snaps instead of flying across
            if (t.1 - a.head.1).abs() > self.cell_h * 6.0 {
                *a = CursorAnim { head: t, tail: t, target: t, last: now };
            }
            a.target = t;
            // the first step of a move after a pause counts as one frame, not the whole pause
            a.last = now - std::time::Duration::from_millis(16);
        }
        let dt = now.duration_since(a.last).as_secs_f32() * 1000.0;
        a.last = now;
        let k = |tau: f32| 1.0 - (-dt / tau.max(1.0)).exp();
        let (kh, kt) = (k(self.cursor_ms / 3.0), k(self.cursor_ms / 1.5));
        a.head.0 += (t.0 - a.head.0) * kh;
        a.head.1 += (t.1 - a.head.1) * kh;
        a.tail.0 += (t.0 - a.tail.0) * kt;
        a.tail.1 += (t.1 - a.tail.1) * kt;
        let near = |p: (f32, f32)| (p.0 - t.0).abs() + (p.1 - t.1).abs() < 0.5;
        let mut moving = false;
        if near(a.head) { a.head = t } else { moving = true }
        if near(a.tail) { a.tail = t } else { moving = true }
        (Some((a.head, a.tail)), moving)
    }
}

/// Prompt lines (absolute) with the exit status of the command that ran just before each.
pub fn prompts(marks: &[crate::session::Mark]) -> Vec<(usize, Option<i32>)> {
    let mut out = Vec::new();
    let mut last_exit = None;
    for m in marks {
        match m.kind {
            'D' => last_exit = m.exit,
            'A' => {
                out.push((m.line, last_exit.take()));
            }
            _ => {}
        }
    }
    out
}

impl Frame {
    /// Places prompt marks on the rows they fall on (given the scroll position).
    pub fn with_prompts<T: alacritty_terminal::event::EventListener>(mut self, term: &Term<T>, marks: &[crate::session::Mark]) -> Frame {
        let top = term.grid().history_size() as i64 - term.grid().display_offset() as i64;
        let rows = self.rows.len() as i64;
        self.prompts = prompts(marks)
            .into_iter()
            .filter_map(|(line, exit)| {
                let r = line as i64 - top;
                (0..rows).contains(&r).then_some((r as usize, exit))
            })
            .collect();
        self
    }
}

#[cfg(test)]
mod prompt_tests {
    use crate::session::Mark;

    #[test]
    fn exit_status_goes_to_the_next_prompt() {
        let m = |kind, line, exit| Mark { kind, line, exit };
        let marks = [m('A', 0, None), m('C', 2, None), m('D', 2, Some(0)), m('A', 2, None), m('C', 4, None), m('D', 4, Some(1)), m('A', 4, None)];
        assert_eq!(super::prompts(&marks), vec![(0, None), (2, Some(0)), (4, Some(1))]);
    }
}

/// One cell, resolved to what is drawn.
fn cell_view(cell: &alacritty_terminal::term::cell::Cell, point: Point, colors: &alacritty_terminal::term::color::Colors, p: &Palette, sel: Option<alacritty_terminal::selection::SelectionRange>) -> CellView {
    let f = cell.flags;
    let bold = f.contains(Flags::BOLD);
    let mut fg = resolve(cell.fg, colors, p, bold);
    let mut bg = resolve(cell.bg, colors, p, false);
    if f.contains(Flags::DIM) {
        fg = dim(fg);
    }
    if f.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if sel.is_some_and(|s| s.contains(point)) {
        fg = p.selection_fg;
        bg = p.selection_bg;
    }
    // control characters never reach the shaper: a tab is kept in its cell (for copying)
    // but would be expanded to a tab stop and push the rest of the row out of place
    let ch = if f.contains(Flags::HIDDEN) || cell.c.is_control() { ' ' } else { cell.c };
    CellView {
        ch,
        zw: cell.zerowidth().map(|z| z.to_vec()),
        style: Style2 { fg: (fg.0, fg.1, fg.2), bold, italic: f.contains(Flags::ITALIC) },
        bg: (bg != p.bg).then_some(bg),
        underline: f.intersects(Flags::ALL_UNDERLINES),
        strike: f.contains(Flags::STRIKEOUT),
        wide: f.contains(Flags::WIDE_CHAR),
        spacer: f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
    }
}

impl Frame {
    /// While a scroll glides by `px` (+ = content moving down), adds the rows that slide
    /// into view at the edge, so it is never blank.
    pub fn with_scroll<T: alacritty_terminal::event::EventListener>(mut self, term: &Term<T>, p: &Palette, px: f32, cell_h: f32) -> Frame {
        if px.abs() < 0.5 || cell_h <= 0.0 {
            return self;
        }
        let n = (px.abs() / cell_h).ceil() as i32 + 1;
        let grid = term.grid();
        let top = -(grid.display_offset() as i32);
        let lines = term.screen_lines() as i32;
        let hist = grid.history_size() as i32;
        let cols = term.columns();
        let sel = term.selection.as_ref().and_then(|s| s.to_range(term));
        let colors = term.colors();
        let row = |l: i32| -> Vec<CellView> {
            (0..cols).map(|c| {
                let pt = Point::new(Line(l), Column(c));
                cell_view(&grid[pt], pt, colors, p, sel)
            }).collect()
        };
        if px > 0.0 {
            // content moved down: older lines show above the top
            self.above = (1..=n).map(|k| top - k).take_while(|l| *l >= -hist).map(row).collect();
        } else {
            self.below = (0..n).map(|k| top + lines + k).take_while(|l| *l < lines).map(row).collect();
        }
        self.scroll_px = px;
        self
    }
}

/// Rectangles cut to a pane (x, y, w, h): nothing spills into a neighbour or the tab bar.
fn clipped(rects: Vec<Rect>, clip: [f32; 4]) -> Vec<Rect> {
    let (cx0, cy0, cx1, cy1) = (clip[0], clip[1], clip[0] + clip[2], clip[1] + clip[3]);
    rects
        .into_iter()
        .filter_map(|r| {
            let [x, y, w, h] = r.xywh;
            let (x0, y0, x1, y1) = (x.max(cx0), y.max(cy0), (x + w).min(cx1), (y + h).min(cy1));
            (x1 > x0 && y1 > y0).then_some(Rect { xywh: [x0, y0, x1 - x0, y1 - y0], color: r.color })
        })
        .collect()
}
