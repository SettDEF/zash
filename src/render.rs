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

#[derive(Clone)]
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

/// A part of the screen whose scroll is gliding (Neovim's grid_scroll): rows `top..bot` are
/// drawn `px` from their place, and the rows that left (nearest first) fill the edge.
#[derive(Clone)]
pub struct Region {
    pub top: usize,
    pub bot: usize,
    pub px: f32,
    pub old: Vec<Vec<CellView>>,
}

pub struct Frame {
    pub region: Option<Region>,
    /// smooth scrolling: rows just above and below the screen (nearest first), and how
    /// far the content is drawn shifted (pixels, + = down) while a scroll glides
    pub above: Vec<Vec<CellView>>,
    pub below: Vec<Vec<CellView>>,
    pub scroll_px: f32,
    /// the shell plugin's completion popup, drawn at the cursor
    pub popup: Option<crate::session::Popup>,
    pub popup_look: crate::config::PopupLook,
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
    /// rows drawn small with columns (zeit's job lines); their cells are blanked
    pub small: Vec<crate::session::UiRow>,
    /// the tones of those columns: good, bad, running, accent
    pub tones: [Rgb; 4],
    /// images (Kitty graphics) on screen
    pub images: Vec<ImageView>,
    /// a multiplexer's app popup: zash draws its frame (the cells under it are blanked)
    pub float: Option<crate::graphics::Float>,
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
        Frame { float: None, popup_look: crate::config::PopupLook::default(), region: None, above: Vec::new(), below: Vec::new(), scroll_px: 0.0, popup: None, tabs: Vec::new(), bar: None, bar_bg: p.selection_bg, fg: p.fg, prompts: Vec::new(), cols, rows, cursor, bg: p.bg, cursor_color: p.cursor_bg, fail_color: p.ansi[1], small: Vec::new(), tones: [p.ansi[2], p.ansi[1], p.ansi[3], p.ansi[4]], images: Vec::new() }
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
struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local: vec2<f32>,
    @location(2) half: vec2<f32>,
    @location(3) p: vec2<f32>,
};
@vertex fn vs(@builtin(vertex_index) vi: u32, @location(0) rect: vec4<f32>, @location(1) color: vec4<f32>, @location(2) p: vec2<f32>) -> VOut {
    var corners = array<vec2<f32>, 6>(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0));
    let c = corners[vi];
    let q = rect.xy + c * rect.zw;
    var o: VOut;
    o.pos = vec4(q.x / u.screen.x * 2.0 - 1.0, 1.0 - q.y / u.screen.y * 2.0, 0.0, 1.0);
    o.color = color;
    o.local = (c - vec2(0.5)) * rect.zw;
    o.half = rect.zw * 0.5;
    o.p = p;
    return o;
}
// p.x: corner radius, p.y: edge softness (a shadow is a large softness); both 0: a plain box
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
    // p.y -1: a triangle pointing right, -2: pointing left (powerline separators)
    if (i.p.y < -0.5) {
        let uv = i.local / (2.0 * i.half) + vec2(0.5);
        var inside = (1.0 - uv.x) - abs(uv.y * 2.0 - 1.0);
        if (i.p.y < -1.5) { inside = uv.x - abs(uv.y * 2.0 - 1.0); }
        let a = clamp(inside * i.half.x * 2.0 + 0.5, 0.0, 1.0);
        return vec4(i.color.rgb, i.color.a * a);
    }
    if (i.p.x <= 0.0 && i.p.y <= 0.0) { return i.color; }
    let soft = max(i.p.y, 0.6);
    let r = min(i.p.x + i.p.y, min(i.half.x, i.half.y));
    let q = abs(i.local) - i.half + vec2(r);
    let d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
    let a = 1.0 - smoothstep(-soft, soft, d + i.p.y);
    return vec4(i.color.rgb, i.color.a * a);
}
"#;

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    xywh: [f32; 4],
    color: [f32; 4],
    /// corner radius and edge softness (0, 0: a plain box)
    p: [f32; 2],
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
        let attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x2];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("zash rects"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: 40, step_mode: wgpu::VertexStepMode::Instance, attributes: &attrs })],
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
        let buf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (cap * 40) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        Rects { pipeline, uniform, bind, buf, cap, count: 0 }
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, rects: &[Rect], w: f32, h: f32) {
        queue.write_buffer(&self.uniform, 0, &as_bytes(&[w, h, 0.0, 0.0]));
        if rects.len() > self.cap {
            self.cap = rects.len().next_power_of_two();
            self.buf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (self.cap * 40) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        }
        let flat: Vec<f32> = rects.iter().flat_map(|r| r.xywh.into_iter().chain(r.color).chain(r.p)).collect();
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
    images: Images,
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
    /// text placement: centred in the leftover space, and moved by an offset (pixels)
    pub center: bool,
    pub offset: (f32, f32),
    /// the backdrop blur stage, made the first time a popup asks for it
    blur: Option<Blur>,
    format: wgpu::TextureFormat,
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
            images: Images::new(device, format),
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
            blur: None,
            format,
            center: cfg.center,
            offset: cfg.offset,
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
        let (dx, dy) = (self.offset.0.max(0.0), self.offset.1.max(0.0));
        let cols = ((w - 2.0 * self.pad - dx) / self.cell_w).floor().max(2.0) as usize;
        let rows = ((h - 2.0 * self.pad - dy) / self.cell_h).floor().max(1.0) as usize;
        (cols, rows)
    }

    /// Where a pane's first cell starts: after the padding and the offset, and (centred)
    /// with the space the cells do not fill shared on both sides.
    pub fn origin(&self, x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
        let (cols, rows) = self.grid_in(w, h);
        let (mut ox, mut oy) = (x + self.pad + self.offset.0, y + self.pad + self.offset.1);
        if self.center {
            let spare_x = w - 2.0 * self.pad - self.offset.0.max(0.0) - cols as f32 * self.cell_w;
            let spare_y = h - 2.0 * self.pad - self.offset.1.max(0.0) - rows as f32 * self.cell_h;
            ox += (spare_x / 2.0).max(0.0).floor();
            oy += (spare_y / 2.0).max(0.0).floor();
        }
        (ox, oy)
    }

    /// Shapes the rows of a pane: above the screen, the screen, below it (top to bottom).
    /// The cache is keyed by a row's content, not its position: when the screen scrolls,
    /// rows that only moved are not shaped again.
    fn shape_rows(&mut self, pane: u64, frame: &Frame, width: f32) {
        let mut old = self.rows.remove(&pane).unwrap_or_default();
        let mut cache: std::collections::HashMap<u64, Buffer> = std::collections::HashMap::with_capacity(frame.rows.len() + 4);
        let mut order = Vec::with_capacity(frame.rows.len() + frame.above.len() + frame.below.len());
        for (row, _, _) in placed(frame, 0.0, self.cell_h) {
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
                // powerline shapes are drawn as rectangles (rect_list), not as glyphs
                s.push(if c.ch == '\0' || powerline(c.ch).is_some() { ' ' } else { c.ch });
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
            out.push(Rect { xywh: [ox - self.pad, oy + *r as f32 * ch, w, ch], color: col, p: [0.0, 0.0] });
        }
        clipped(out, clip)
    }

    fn rect_list(&self, frame: &Frame, cur: Option<((f32, f32), (f32, f32))>, ox: f32, oy: f32, clip: [f32; 4]) -> Vec<Rect> {
        let (cw, ch, pad) = (self.cell_w, self.cell_h, ox);
        // while a scroll glides rows are drawn shifted; rows beyond the screen (or a region's
        // old rows) fill the edge, each cut to its band
        let shift = frame.scroll_px + frame.region.as_ref().filter(|g| frame.cursor.is_some_and(|(r, _, _)| r >= g.top && r < g.bot)).map(|g| g.px).unwrap_or(0.0);
        let cur = cur.map(|((hx, hy), (tx, ty))| ((hx, hy + shift), (tx, ty + shift)));
        let mut out_all: Vec<Rect> = Vec::new();
        for (row, y, band) in placed(frame, oy, ch) {
            if y + ch < clip[1] || y > clip[1] + clip[3] {
                continue;
            }
            let mut out_row: Vec<Rect> = Vec::new();
            let out = &mut out_row;
            // backgrounds, merged across cells of one colour
            let mut run: Option<(usize, usize, Rgb)> = None;
            for (c, cell) in row.iter().enumerate() {
                match (run, cell.bg) {
                    (Some((s, e, col)), Some(b)) if b == col && e == c => run = Some((s, c + 1, col)),
                    (prev, bg) => {
                        if let Some((s, e, col)) = prev {
                            out.push(Rect { xywh: [pad + s as f32 * cw, y, (e - s) as f32 * cw, ch], color: col.f32(), p: [0.0, 0.0] });
                        }
                        run = bg.map(|b| (c, c + 1, b));
                    }
                }
                let fg = Rgb(cell.style.fg.0, cell.style.fg.1, cell.style.fg.2).f32();
                // powerline shapes, drawn here at the full cell height (the font's glyphs are only as
                // tall as the font, so a pill's ends would stop short of its middle)
                if let Some(shape) = powerline(cell.ch) {
                    let x = pad + c as f32 * cw;
                    let r = (ch / 2.0).min(cw);
                    out.push(match shape {
                        // a rounded end reaches into the next cell (the pill's body, same colour)
                        Powerline::RoundLeft => Rect { xywh: [x, y, 2.0 * cw, ch], color: fg, p: [r, 0.0] },
                        Powerline::RoundRight => Rect { xywh: [x - cw, y, 2.0 * cw, ch], color: fg, p: [r, 0.0] },
                        Powerline::TriRight => Rect { xywh: [x, y, cw, ch], color: fg, p: [0.0, -1.0] },
                        Powerline::TriLeft => Rect { xywh: [x, y, cw, ch], color: fg, p: [0.0, -2.0] },
                    });
                }
                let w = if cell.wide { 2.0 * cw } else { cw };
                let thick = (self.font_px / 14.0).max(1.0).round();
                if cell.underline {
                    out.push(Rect { xywh: [pad + c as f32 * cw, y + ch - thick * 2.0, w, thick], color: fg, p: [0.0, 0.0] });
                }
                if cell.strike {
                    out.push(Rect { xywh: [pad + c as f32 * cw, y + ch * 0.55, w, thick], color: fg, p: [0.0, 0.0] });
                }
            }
            if let Some((s, e, col)) = run {
                out.push(Rect { xywh: [pad + s as f32 * cw, y, (e - s) as f32 * cw, ch], color: col.f32(), p: [0.0, 0.0] });
            }
            match band {
                Some((b0, b1)) => out_all.extend(clipped(out_row, [clip[0], b0, clip[2], b1 - b0])),
                None => out_all.extend(out_row),
            }
        }
        let mut out = out_all;
        if let Some((r, c, shape)) = frame.cursor {
            let (x, y) = cur.map(|c| c.0).unwrap_or((pad + c as f32 * cw, oy + r as f32 * ch + shift));
            // the smear: from the lagging tail to the head, along one line only
            if let Some((_, (tx, ty))) = cur.filter(|_| self.cursor_trail) {
                if (ty - y).abs() < 1.0 && (tx - x).abs() > 1.0 {
                    let (a, b) = (tx.min(x), tx.max(x));
                    let mut col = frame.cursor_color.f32();
                    col[3] = 0.35;
                    out.push(Rect { xywh: [a, y + ch * 0.15, b - a, ch * 0.7], color: col, p: [0.0, 0.0] });
                }
            }
            let wide = frame.rows.get(r).and_then(|row| row.get(c)).is_some_and(|cell| cell.wide);
            let w = if wide { 2.0 * cw } else { cw };
            let col = frame.cursor_color.f32();
            let t = (self.font_px / 9.0).max(1.0).round();
            match shape {
                CursorShape::Beam => out.push(Rect { xywh: [x, y, t, ch], color: col, p: [0.0, 0.0] }),
                CursorShape::Underline => out.push(Rect { xywh: [x, y + ch - t, w, t], color: col, p: [0.0, 0.0] }),
                CursorShape::HollowBlock => {
                    out.push(Rect { xywh: [x, y, w, t], color: col, p: [0.0, 0.0] });
                    out.push(Rect { xywh: [x, y + ch - t, w, t], color: col, p: [0.0, 0.0] });
                    out.push(Rect { xywh: [x, y, t, ch], color: col, p: [0.0, 0.0] });
                    out.push(Rect { xywh: [x + w - t, y, t, ch], color: col, p: [0.0, 0.0] });
                }
                _ => {
                    // a block; the character on it is drawn in the cursor's text colour (Frame::extract)
                    out.push(Rect { xywh: [x, y, w, ch], color: col, p: [0.0, 0.0] });
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
        let (fox, foy) = self.origin(focus.x, focus.y, focus.w, focus.h);
        let (cur, animating) = self.cursor_motion(frame, fox, foy);
        let mut rects = Vec::new();
        let mut over = Vec::new();
        // floats (a multiplexer's app popups): their pixel rect, for the title and buttons later
        let mut floats: Vec<([f32; 4], crate::graphics::Float, Rgb, Rgb, Rgb)> = Vec::new();
        for p in panes {
            self.shape_rows(p.id, p.frame, p.w - 2.0 * self.pad);
            let (ox, oy) = self.origin(p.x, p.y, p.w, p.h);
            let clip = [p.x, p.y.max(self.top), p.w, p.y + p.h - p.y.max(self.top)];
            if let Some(z) = &p.frame.float {
                // under the text: a soft shadow, the edge (the accent when it has the keyboard), the panel
                let r = [ox + z.x as f32 * self.cell_w, oy + z.y as f32 * self.cell_h, z.w as f32 * self.cell_w, z.h as f32 * self.cell_h];
                let rad = 12.0;
                rects.push(Rect { xywh: [r[0] - 14.0, r[1] - 10.0, r[2] + 28.0, r[3] + 28.0], color: [0.0, 0.0, 0.0, 0.34], p: [rad, 14.0] });
                let edge = if z.focus { let mut c = p.frame.cursor_color.f32(); c[3] = 0.85; c } else { let mut c = p.frame.fg.f32(); c[3] = 0.16; c };
                rects.push(Rect { xywh: [r[0] - 1.0, r[1] - 1.0, r[2] + 2.0, r[3] + 2.0], color: edge, p: [rad + 1.0, 0.0] });
                let mut panel = p.frame.bar_bg.f32();
                panel[3] = 0.97;
                rects.push(Rect { xywh: r, color: panel, p: [rad, 0.0] });
                floats.push((r, z.clone(), p.frame.fg, p.frame.cursor_color, p.frame.fail_color));
            }
            rects.extend(self.rect_list(p.frame, if p.focused { cur } else { None }, ox, oy, clip));
            over.extend(self.overlay_list(p.frame, ox, oy, clip));
        }
        if panes.len() > 1 {
            let line = { let mut c = frame.fg.f32(); c[3] = 0.16; c };
            let shade = { let mut c = frame.bg.f32(); c[3] = 0.32; c };
            for p in panes {
                // a hairline where a pane meets its left or upper neighbour
                if p.x > 0.5 {
                    over.push(Rect { xywh: [p.x.round() - 1.0, p.y, 1.0, p.h], color: line, p: [0.0, 0.0] });
                }
                if p.y > self.top + 0.5 {
                    over.push(Rect { xywh: [p.x, p.y.round() - 1.0, p.w, 1.0], color: line, p: [0.0, 0.0] });
                }
                // panes without the keyboard step back a little
                if !p.focused {
                    over.push(Rect { xywh: [p.x, p.y, p.w, p.h], color: shade, p: [0.0, 0.0] });
                }
            }
        }
        self.rects.upload(device, queue, &rects, width as f32, height as f32);
        // images: placed in their pane's cells, clipped to the pane
        let mut imgs = Vec::new();
        for p in panes {
            let (ox, oy) = self.origin(p.x, p.y, p.w, p.h);
            let oy = oy + p.frame.scroll_px;
            for iv in &p.frame.images {
                let rect = [ox + iv.col as f32 * self.cell_w, oy + iv.row as f32 * self.cell_h, iv.cols as f32 * self.cell_w, iv.rows as f32 * self.cell_h];
                let top = p.y.max(self.top);
                imgs.push((iv.clone(), rect, [p.x, top, p.w, p.y + p.h - top]));
            }
        }
        self.images.prepare(device, queue, &imgs, width, height);
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
        // the floats' title bars: the name, and the buttons on the cells the multiplexer reads clicks from
        for (r, z, fg, accent, fail) in &floats {
            let dim = dim6(*fg);
            let (b, _) = label(&mut self.font_system, &self.family, &z.title, if z.focus { *fg } else { dim }, self.font_px, self.cell_h);
            labels.push((b, r[0] + self.cell_w * 1.5, r[1]));
            let bx = r[0] + (z.w as f32 - 9.0) * self.cell_w;
            for (k, glyph) in [(1.0, "⌄"), (3.0, "–"), (5.0, "×")] {
                let cx = bx + (k + 0.5) * self.cell_w;
                let cy = r[1] + self.cell_h * 0.5;
                let d = self.cell_h * 0.78;
                let col = if glyph == "×" { let mut c = fail.f32(); c[3] = if z.focus { 0.85 } else { 0.35 }; c } else { let mut c = fg.f32(); c[3] = 0.12; c };
                over.push(Rect { xywh: [cx - d / 2.0, cy - d / 2.0, d, d], color: col, p: [d / 2.0, 0.0] });
                let (b, w) = label(&mut self.font_system, &self.family, glyph, if glyph == "×" { Rgb(255, 255, 255) } else { *fg }, self.font_px * 0.9, self.cell_h);
                labels.push((b, cx - w / 2.0, r[1]));
            }
            let _ = accent;
        }
        // small rows: the command left, its columns right-aligned, a progress bar under it
        for p in panes {
            if p.frame.small.is_empty() {
                continue;
            }
            let (ox, oy) = self.origin(p.x, p.y, p.w, p.h);
            let (cw, ch) = (self.cell_w, self.cell_h);
            let px = (self.font_px * 0.8).round();
            for sr in &p.frame.small {
                let y = oy + sr.row as f32 * ch;
                let x0 = ox + sr.col as f32 * cw;
                let first_button = sr.buttons.iter().map(|b| b.0).min().unwrap_or(sr.width);
                let x1 = ox + (sr.col + first_button) as f32 * cw;
                let mut right = x1 - cw * 0.6;
                for (at, glyph) in &sr.buttons {
                    let (b, w) = label(&mut self.font_system, &self.family, glyph, p.frame.tone('a'), self.font_px, ch);
                    labels.push((b, ox + (sr.col + at) as f32 * cw + (cw - w) / 2.0, y));
                }
                for (tone, text) in sr.cols.iter().rev() {
                    let (b, w) = label(&mut self.font_system, &self.family, text, p.frame.tone(*tone), px, ch);
                    right -= w;
                    labels.push((b, right, y));
                    right -= cw * 1.4;
                }
                // the command gets what is left (about 0.8 cells a character at this size)
                let room = (((right - x0) / (cw * 0.8)) as usize).saturating_sub(2);
                let mut left: String = sr.left.chars().take(room).collect();
                if sr.left.chars().count() > room && room > 1 {
                    left.pop();
                    left.push('…');
                }
                let (b, _) = label(&mut self.font_system, &self.family, &left, p.frame.fg, px, ch);
                labels.push((b, x0 + cw * 0.3, y));
                if let Some(pr) = sr.progress {
                    let mut c = p.frame.tone('r').f32();
                    c[3] = 0.9;
                    over.push(Rect { xywh: [x0 + cw * 0.3, y + ch - 2.0, (x1 - x0 - cw * 0.6) * pr, 2.0], color: c, p: [0.0, 0.0] });
                }
            }
        }
        if frame.tabs.len() > 1 {
            // the tab bar: one pill per tab, the active one lifted
            let h = self.top;
            let n = frame.tabs.len() as f32;
            let tw = ((width as f32 - self.cell_w) / n).min(self.cell_w * 28.0);
            for (i, (title, active)) in frame.tabs.iter().enumerate() {
                let x = self.cell_w * 0.5 + i as f32 * tw;
                if *active {
                    over.push(Rect { xywh: [x + 2.0, h * 0.12, tw - 4.0, h * 0.76], color: frame.bar_bg.f32(), p: [0.0, 0.0] });
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
        let mut backdrop: Option<([f32; 4], f32, f32)> = None;
        if let (Some(pp), Some((cr, cc, _))) = (&frame.popup, frame.cursor) {
            // the completion popup: under the word being completed, above it when there is no room
            let look = frame.popup_look;
            let n = pp.items.len();
            let row_h = (self.cell_h * 1.25).round();
            let (left, right, bottom) = (focus.x + self.pad, focus.x + focus.w - self.pad, height as f32 - self.pad);
            // as many rows as fit on the roomier side of the cursor line (up to the look's rows)
            let line_y = foy + cr as f32 * self.cell_h;
            let above_px = line_y - self.top;
            let below_px = bottom - line_y - self.cell_h;
            let room = above_px.max(below_px) - self.cell_h;
            let mut show: usize = ((room / row_h) as usize).clamp(1, look.rows.max(1));
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
            let r = look.radius;
            if look.shadow {
                // a soft shadow, a little lower than the popup
                let s = 14.0;
                over.push(Rect { xywh: [x - s, y - s + 4.0, pw + 2.0 * s, ph + 2.0 * s], color: [0.0, 0.0, 0.0, 0.38], p: [r, s] });
            }
            if look.blur > 0.0 {
                backdrop = Some(([x, y, pw, ph], r, look.blur));
            }
            if look.border {
                let e = frame.fg.f32();
                over.push(Rect { xywh: [x - 1.0, y - 1.0, pw + 2.0, ph + 2.0], color: [e[0], e[1], e[2], 0.16], p: [r + 1.0, 0.0] });
            }
            let mut bg = frame.bar_bg.f32();
            bg[3] = look.opacity;
            over.push(Rect { xywh: [x, y, pw, ph], color: bg, p: [r, 0.0] });
            let dim = dim6(frame.fg);
            for (k, i) in rows.iter().enumerate() {
                let ry = y + self.cell_h * 0.25 + k as f32 * row_h;
                let chosen = pp.sel == Some(*i);
                if chosen {
                    let c = frame.cursor_color.f32();
                    over.push(Rect { xywh: [x + 4.0, ry, pw - 8.0, row_h], color: [c[0], c[1], c[2], 0.30], p: [(r * 0.6).max(2.0), 0.0] });
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
            over.push(Rect { xywh: [x, y, bw, bh], color: frame.bar_bg.f32(), p: [0.0, 0.0] });
            labels.push((b, x + self.cell_w, y + self.cell_h * 0.25));
        }
        self.bars = labels;
        self.overlay.upload(device, queue, &over, width as f32, height as f32);
        self.viewport.update(queue, Resolution { width, height });
        let ch = self.cell_h;
        let mut areas: Vec<TextArea> = Vec::new();
        for p in panes {
            let (Some(cache), Some(order)) = (self.rows.get(&p.id), self.order.get(&p.id)) else { continue };
            let (ox, oy) = self.origin(p.x, p.y, p.w, p.h);
            // each pane's text stays inside its pane (and out of the tab bar); a gliding region's rows inside the region
            let pane_top = p.y.max(self.top);
            let places = placed(p.frame, oy, ch);
            areas.extend(order.iter().zip(places).filter_map(|(key, (_, y, band))| cache.get(key).map(|b| (b, y, band))).map(|(b, y, band)| TextArea {
                buffer: b,
                left: ox,
                top: y,
                scale: 1.0,
                bounds: {
                    let (t, bo) = band.map(|(b0, b1)| (b0.max(pane_top), b1.min(p.y + p.h))).unwrap_or((pane_top, p.y + p.h));
                    TextBounds { left: p.x as i32, top: t as i32, right: (p.x + p.w) as i32, bottom: bo as i32 }
                },
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
        let bg = frame.bg.f32();
        // pre-multiplied: the compositor blends (and blurs) what is behind
        let o = self.opacity as f64;
        let clear = wgpu::Color { r: bg[0] as f64 * o, g: bg[1] as f64 * o, b: bg[2] as f64 * o, a: o };
        let pass = |enc: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>| -> wgpu::RenderPass<'static> {
            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load, store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            }).forget_lifetime()
        };
        match backdrop {
            None => {
                // the usual frame: one pass
                let mut p = pass(&mut enc, view, wgpu::LoadOp::Clear(clear));
                self.rects.draw(&mut p);
                self.images.draw(&mut p, width, height);
                let _ = self.text.render(&self.atlas, &self.viewport, &mut p);
                self.overlay.draw(&mut p);
                let _ = self.text2.render(&self.atlas, &self.viewport, &mut p);
            }
            Some((rect, radius, amount)) => {
                // a popup with blur: the terminal offscreen, blurred inside the popup, then the popup on top
                let blur = self.blur.get_or_insert_with(|| Blur::new(device, self.format));
                blur.textures(device, width, height);
                let (_, _, _, scene, _, tmp) = blur.tex.as_ref().unwrap();
                {
                    let mut p = pass(&mut enc, scene, wgpu::LoadOp::Clear(clear));
                    self.rects.draw(&mut p);
                    self.images.draw(&mut p, width, height);
                    let _ = self.text.render(&self.atlas, &self.viewport, &mut p);
                }
                let (w, h) = (width as f32, height as f32);
                let steps = (amount / 2.0).clamp(1.0, 12.0);
                let spread = amount / steps;
                let block = |dir: [f32; 2]| -> Vec<u8> {
                    let v = [dir[0], dir[1], steps, 0.0, rect[0], rect[1], rect[2], rect[3], w, h, 0.0, 0.0, radius, 0.0, 0.0, 0.0];
                    as_bytes(&v)
                };
                queue.write_buffer(&blur.uniform, 0, &block([spread / w, 0.0]));
                queue.write_buffer(&blur.uniform, 256, &block([0.0, spread / h]));
                queue.write_buffer(&blur.uniform, 512, &block([0.0, 0.0]));
                queue.write_buffer(&blur.uniform, 768, &block([0.0, 0.0]));
                // the region the blur needs: the popup plus the blur's reach
                let m = amount * 1.5;
                let sx = (rect[0] - m).max(0.0) as u32;
                let sy = (rect[1] - m).max(0.0) as u32;
                let sw = ((rect[0] + rect[2] + m).min(w) as u32).saturating_sub(sx);
                let sh = ((rect[1] + rect[3] + m).min(h) as u32).saturating_sub(sy);
                let to_screen = blur.bind(device, scene, 3);
                let h_pass = blur.bind(device, scene, 0);
                let v_pass = blur.bind(device, tmp, 1);
                let masked = blur.bind(device, scene, 2);
                fullscreen(&mut enc, view, &blur.blit, &to_screen, wgpu::LoadOp::Clear(clear), None);
                fullscreen(&mut enc, tmp, &blur.blur, &h_pass, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), Some([sx, sy, sw, sh]));
                fullscreen(&mut enc, scene, &blur.blur, &v_pass, wgpu::LoadOp::Load, Some([sx, sy, sw, sh]));
                fullscreen(&mut enc, view, &blur.mask, &masked, wgpu::LoadOp::Load, Some([sx, sy, sw, sh]));
                let mut p = pass(&mut enc, view, wgpu::LoadOp::Load);
                self.overlay.draw(&mut p);
                let _ = self.text2.render(&self.atlas, &self.viewport, &mut p);
            }
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
    /// Small rows (OSC 7733;rows) over the live screen: their cells give way to them.
    pub fn with_rows<T: alacritty_terminal::event::EventListener>(mut self, term: &Term<T>, rows: &[crate::session::UiRow]) -> Frame {
        if term.grid().display_offset() != 0 {
            return self;
        }
        for r in rows {
            if let Some(row) = self.rows.get_mut(r.row) {
                for c in row.iter_mut().skip(r.col).take(r.width) {
                    c.ch = ' ';
                    c.zw = None;
                }
                self.small.push(r.clone());
            }
        }
        self
    }

    pub fn tone(&self, t: char) -> Rgb {
        match t {
            'g' => self.tones[0],
            'b' => self.tones[1],
            'r' => self.tones[2],
            'a' => self.tones[3],
            'm' => dim6(self.fg),
            _ => self.fg,
        }
    }
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
            (x1 > x0 && y1 > y0).then_some(Rect { xywh: [x0, y0, x1 - x0, y1 - y0], color: r.color, p: r.p })
        })
        .collect()
}

/// Every row to draw, top to bottom, with its y and (for a gliding region) the band it
/// is cut to: rows above the screen, the screen, rows below it, then a region's old rows.
fn placed(frame: &Frame, oy: f32, ch: f32) -> Vec<(&Vec<CellView>, f32, Option<(f32, f32)>)> {
    let oy = oy + frame.scroll_px;
    let mut out = Vec::with_capacity(frame.rows.len() + frame.above.len() + frame.below.len());
    let n = frame.above.len() as f32;
    for (k, row) in frame.above.iter().rev().enumerate() {
        out.push((row, oy + (k as f32 - n) * ch, None));
    }
    let region = frame.region.as_ref().filter(|g| g.top < g.bot && g.bot <= frame.rows.len());
    let band = region.map(|g| (oy + g.top as f32 * ch, oy + g.bot as f32 * ch));
    for (r, row) in frame.rows.iter().enumerate() {
        match region {
            Some(g) if r >= g.top && r < g.bot => out.push((row, oy + r as f32 * ch + g.px, band)),
            _ => out.push((row, oy + r as f32 * ch, None)),
        }
    }
    for (k, row) in frame.below.iter().enumerate() {
        out.push((row, oy + (frame.rows.len() + k) as f32 * ch, None));
    }
    if let Some(g) = region {
        for (k, row) in g.old.iter().enumerate() {
            // content moved down (px > 0): the old rows sit above the region's top, else below its bottom
            let y = if g.px > 0.0 { oy + (g.top as f32 - 1.0 - k as f32) * ch + g.px } else { oy + (g.bot + k) as f32 * ch + g.px };
            out.push((row, y, band));
        }
    }
    out
}

// ── backdrop blur (behind the completion popup) ──

const BLUR_WGSL: &str = r#"
struct P { dir: vec4<f32>, rect: vec4<f32>, screen: vec4<f32>, corner: vec4<f32> };
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
@group(0) @binding(2) var<uniform> p: P;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) vi: u32) -> VOut {
    var xy = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var o: VOut;
    o.pos = vec4(xy[vi], 0.0, 1.0);
    o.uv = vec2(xy[vi].x * 0.5 + 0.5, 0.5 - xy[vi].y * 0.5);
    return o;
}
@fragment fn blit(i: VOut) -> @location(0) vec4<f32> { return textureSample(t, s, i.uv); }
// one direction of a gaussian; p.dir.xy: the step in uv, p.dir.z: how many steps each side
@fragment fn blur(i: VOut) -> @location(0) vec4<f32> {
    let n = i32(p.dir.z);
    var acc = vec4(0.0);
    var wsum = 0.0;
    for (var k = -n; k <= n; k = k + 1) {
        let x = f32(k) / max(f32(n), 1.0);
        let w = exp(-x * x * 2.0);
        acc = acc + textureSample(t, s, i.uv + p.dir.xy * f32(k)) * w;
        wsum = wsum + w;
    }
    return acc / wsum;
}
// the blurred picture, only inside the popup's rounded rectangle
@fragment fn mask(i: VOut) -> @location(0) vec4<f32> {
    let px = i.uv * p.screen.xy;
    let half = p.rect.zw * 0.5;
    let c = p.rect.xy + half;
    let r = min(p.corner.x, min(half.x, half.y));
    let q = abs(px - c) - half + vec2(r);
    let d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
    let a = 1.0 - smoothstep(-0.7, 0.7, d);
    let col = textureSample(t, s, i.uv);
    return vec4(col.rgb, col.a * a);
}
"#;

struct Blur {
    blit: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    mask: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    format: wgpu::TextureFormat,
    /// the terminal drawn offscreen, and a second texture for the two blur passes
    tex: Option<(u32, u32, wgpu::Texture, wgpu::TextureView, wgpu::Texture, wgpu::TextureView)>,
}

impl Blur {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Blur {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("zash blur"), source: wgpu::ShaderSource::Wgsl(BLUR_WGSL.into()) });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layout)], ..Default::default() });
        let make = |entry: &str, blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("zash blur"),
                layout: Some(&pl),
                vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
                fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some(entry), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })] }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, address_mode_u: wgpu::AddressMode::ClampToEdge, address_mode_v: wgpu::AddressMode::ClampToEdge, ..Default::default() });
        // four uniform blocks (h pass, v pass, mask, blit), 64 bytes each, 256 apart
        let uniform = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 256 * 4, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        Blur { blit: make("blit", None), blur: make("blur", None), mask: make("mask", Some(wgpu::BlendState::ALPHA_BLENDING)), layout, sampler, uniform, format, tex: None }
    }

    fn textures(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        if self.tex.as_ref().is_some_and(|t| t.0 == w && t.1 == h) {
            return;
        }
        let mk = || {
            let t = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("zash backdrop"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let v = t.create_view(&Default::default());
            (t, v)
        };
        let (a, av) = mk();
        let (b, bv) = mk();
        self.tex = Some((w, h, a, av, b, bv));
    }

    fn bind(&self, device: &wgpu::Device, view: &wgpu::TextureView, slot: u64) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &self.uniform, offset: slot * 256, size: wgpu::BufferSize::new(64) }) },
            ],
        })
    }
}

/// One full-screen pass of the blur shader into `target` (optionally only inside `scissor`).
fn fullscreen(enc: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, pipeline: &wgpu::RenderPipeline, bind: &wgpu::BindGroup, load: wgpu::LoadOp<wgpu::Color>, scissor: Option<[u32; 4]>) {
    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load, store: wgpu::StoreOp::Store } })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    if let Some([x, y, w, h]) = scissor {
        pass.set_scissor_rect(x, y, w.max(1), h.max(1));
    }
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind, &[]);
    pass.draw(0..3, 0..1);
}

enum Powerline {
    RoundLeft,
    RoundRight,
    TriRight,
    TriLeft,
}

/// The powerline separators zash draws itself (U+E0B0 , U+E0B2 , U+E0B4 , U+E0B6 ).
fn powerline(c: char) -> Option<Powerline> {
    match c {
        '\u{e0b0}' => Some(Powerline::TriRight),
        '\u{e0b2}' => Some(Powerline::TriLeft),
        '\u{e0b4}' => Some(Powerline::RoundRight),
        '\u{e0b6}' => Some(Powerline::RoundLeft),
        _ => None,
    }
}

// ── images (Kitty graphics) ──

/// An image on screen: its pixels (shared), and where in the pane's cells.
#[derive(Clone)]
pub struct ImageView {
    pub img: crate::graphics::Image,
    /// a live surface (a web area): its texture key and frame number
    pub live: Option<(u64, u64)>,
    /// screen row of its top (negative: partly scrolled out above)
    pub row: i64,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
}

impl Frame {
    /// The placements that show on the screen, given the scroll position.
    pub fn with_images<T: alacritty_terminal::event::EventListener>(mut self, term: &Term<T>, g: &crate::graphics::Graphics) -> Frame {
        // pinned to screen cells by a multiplexer: there while the screen is not scrolled back
        if term.grid().display_offset() == 0 {
            for (id, (rect, _)) in &g.pinned {
                let (Some((x, y, w, h)), Some(v)) = (rect, g.webs.get(id)) else { continue };
                // the page covers its cells (whatever text the multiplexer drew there)
                for row in self.rows.iter_mut().skip(*y).take(*h) {
                    for c in row.iter_mut().skip(*x).take(*w) {
                        c.ch = ' ';
                        c.zw = None;
                    }
                }
                let frame = v.frame.lock().unwrap().clone();
                if let Some(img) = frame {
                    let generation = v.generation.load(std::sync::atomic::Ordering::Relaxed);
                    self.images.push(ImageView { img, live: Some((*id as u64 | 1 << 40, generation)), row: *y as i64, col: *x, cols: *w, rows: *h });
                }
            }
        }
        // a float: its frame ring of cells is zash's to draw (the text frame the multiplexer drew goes)
        if let Some(z) = g.float.clone().filter(|_| term.grid().display_offset() == 0) {
            for (y, row) in self.rows.iter_mut().enumerate().skip(z.y).take(z.h) {
                for (x, c) in row.iter_mut().enumerate().skip(z.x).take(z.w) {
                    if y == z.y || y + 1 == z.y + z.h || x == z.x || x + 1 == z.x + z.w {
                        c.ch = ' ';
                        c.zw = None;
                        c.bg = None;
                        c.underline = false;
                        c.strike = false;
                    }
                }
            }
            self.float = Some(z);
        }
        let top = term.grid().history_size() as i64 - term.grid().display_offset() as i64;
        let lines = self.rows.len() as i64;
        for p in &g.places {
            let row = p.line as i64 - top;
            if row + p.rows as i64 <= 0 || row >= lines {
                continue;
            }
            if let Some(v) = g.webs.get(&p.img) {
                let frame = v.frame.lock().unwrap().clone();
                if let Some(img) = frame {
                    let generation = v.generation.load(std::sync::atomic::Ordering::Relaxed);
                    self.images.push(ImageView { img, live: Some((p.img as u64 | 1 << 40, generation)), row, col: p.col, cols: p.cols, rows: p.rows });
                }
            } else if let Some(img) = g.images.get(&p.img) {
                self.images.push(ImageView { img: img.clone(), live: None, row, col: p.col, cols: p.cols, rows: p.rows });
            }
        }
        self
    }
}

const IMAGE_WGSL: &str = r#"
struct U { screen: vec4<f32>, rect: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
struct V { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    var corners = array<vec2<f32>, 6>(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0));
    let c = corners[i];
    let px = u.rect.xy + c * u.rect.zw;
    var o: V;
    o.pos = vec4(px.x / u.screen.x * 2.0 - 1.0, 1.0 - px.y / u.screen.y * 2.0, 0.0, 1.0);
    o.uv = c;
    return o;
}
@fragment fn fs(v: V) -> @location(0) vec4<f32> {
    let c = textureSample(tex, smp, v.uv);
    return vec4(c.rgb * c.a, c.a);
}
"#;

/// Textured quads for images; a texture per image, kept while it shows.
pub(crate) struct Images {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// by the pixels' address: (texture, its size)
    textures: std::collections::HashMap<usize, (wgpu::Texture, (u32, u32), u64)>,
    /// this frame's draws: bind group and scissor rect
    draws: Vec<(wgpu::BindGroup, wgpu::Buffer, [u32; 4])>,
}

impl Images {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Images {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("zash images"), source: wgpu::ShaderSource::Wgsl(IMAGE_WGSL.into()) });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::VERTEX, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl)], ..Default::default() });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("zash images"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        Images { pipeline, bgl, sampler, textures: Default::default(), draws: Vec::new() }
    }

    /// This frame's images: textures made for new ones, unused ones dropped.
    pub(crate) fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, list: &[(ImageView, [f32; 4], [f32; 4])], w: u32, h: u32) {
        self.draws.clear();
        let mut seen = std::collections::HashSet::new();
        for (iv, rect, clip) in list {
            // a still image by its pixels; a live surface by its key, updated when its frame changes
            let key = iv.live.map(|l| l.0 as usize).unwrap_or(std::sync::Arc::as_ptr(&iv.img.rgba) as usize);
            let generation = iv.live.map(|l| l.1).unwrap_or(0);
            seen.insert(key);
            let (tw, th) = (iv.img.w, iv.img.h);
            let stale = self.textures.get(&key).is_some_and(|t| t.1 != (tw, th));
            if stale {
                self.textures.remove(&key);
            }
            if let Some(t) = self.textures.get_mut(&key).filter(|t| t.2 != generation) {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo { texture: &t.0, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    &iv.img.rgba,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * tw), rows_per_image: Some(th) },
                    wgpu::Extent3d { width: tw, height: th, depth_or_array_layers: 1 },
                );
                t.2 = generation;
            }
            if !self.textures.contains_key(&key) {
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("zash image"),
                    size: wgpu::Extent3d { width: tw.max(1), height: th.max(1), depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    &iv.img.rgba,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * tw), rows_per_image: Some(th) },
                    wgpu::Extent3d { width: tw, height: th, depth_or_array_layers: 1 },
                );
                self.textures.insert(key, (tex, (tw, th), generation));
            }
            let (tex, _, _) = &self.textures[&key];
            let view = tex.create_view(&Default::default());
            let ubuf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 32, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            queue.write_buffer(&ubuf, 0, &as_bytes(&[w as f32, h as f32, 0.0, 0.0, rect[0], rect[1], rect[2], rect[3]]));
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            // the pane's rectangle, inside the window
            let x0 = clip[0].max(0.0) as u32;
            let y0 = clip[1].max(0.0) as u32;
            let x1 = ((clip[0] + clip[2]).min(w as f32)).max(0.0) as u32;
            let y1 = ((clip[1] + clip[3]).min(h as f32)).max(0.0) as u32;
            if x1 > x0 && y1 > y0 {
                self.draws.push((bind, ubuf, [x0, y0, x1 - x0, y1 - y0]));
            }
        }
        self.textures.retain(|k, _| seen.contains(k));
    }

    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass, w: u32, h: u32) {
        if self.draws.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        for (bind, _, s) in &self.draws {
            pass.set_scissor_rect(s[0], s[1], s[2], s[3]);
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..6, 0..1);
        }
        pass.set_scissor_rect(0, 0, w, h);
    }
}
