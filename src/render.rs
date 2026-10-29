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

/// A zfloat's buttons start this many cells from its frame's right edge, where zeit expects the
/// clicks (zeit's BUTTONS_FROM_RIGHT): the × sits next to the corner.
const ZFLOAT_BUTTONS: f32 = 7.0;

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
    /// the text effect on this cell (its zash-fx: hyperlink, fx.rs), if any
    pub fx: Option<alacritty_terminal::term::cell::Hyperlink>,
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
    /// a cursor a full-screen program draws itself while it hides the real one: the one cell
    /// shown inverted on its row (the lowest such row)
    pub drawn_cursor: Option<(usize, usize)>,
    /// set by apply_cursor: `cursor` is such a drawn one (zash adds only its glide)
    pub cursor_drawn: bool,
    /// set by apply_cursor: zash's look (glide, trail, style) applies where the cursor is
    pub cursor_styled: bool,
    /// smooth scrolling: rows just above and below the screen (nearest first), and how
    /// far the content is drawn shifted (pixels, + = down) while a scroll glides
    pub above: Vec<Vec<CellView>>,
    pub below: Vec<Vec<CellView>>,
    pub scroll_px: f32,
    /// the shell plugin's completion popup, drawn at the cursor
    pub popup: Option<crate::session::Popup>,
    pub popup_look: crate::config::PopupLook,
    pub zfloat_look: crate::config::ZfloatLook,
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
    /// pen strokes near the screen (ink.rs), and the absolute line at its top
    pub ink: Vec<crate::ink::Stroke>,
    pub ink_top: f64,
    /// the program's floats in cells (OSC 7733;layers): a shadow and an edge under each
    pub layers: Vec<[usize; 4]>,
    /// the program's UI hints (OSC 7733;ui) on screen, each with the screen row of its top
    pub ui: Vec<(crate::ui::Region, isize)>,
    /// the palette the frame was taken in (UI roles resolve against it)
    pub pal: Palette,
    /// where the pen hovers while drawing: (line, col, radius in cells, the eraser end)
    pub pen_hover: Option<(f64, f32, f32, bool)>,
    /// a multiplexer's app popup: zash draws its frame (the cells under it are blanked)
    pub float: Option<crate::graphics::ZFloat>,
    /// while the zfloat is dragged: the cells under its edge (its id; column, row in the pane, text, fg, bg)
    pub under: Option<(u64, Vec<(usize, usize, String, Rgb, Option<Rgb>, u8)>)>,
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
        // the default colours as the program set them (OSC 10/11: the shell sends the new theme's after
        // a wallpaper change), else the palette's
        let (def_fg, def_bg) = (resolve(AColor::Named(NamedColor::Foreground), content.colors, p, false), resolve(AColor::Named(NamedColor::Background), content.colors, p, false));
        // per row: how many cells are inverted, and the last one (a TUI's own cursor is a lone one)
        let mut inverted: Vec<(u32, usize)> = vec![(0, 0); lines];
        for ic in content.display_iter {
            let row = (ic.point.line.0 + offset) as usize;
            if row >= lines {
                continue;
            }
            if ic.cell.flags.contains(Flags::INVERSE) {
                inverted[row] = (inverted[row].0 + 1, ic.point.column.0);
            }
            rows[row].push(cell_view(ic.cell, ic.point, content.colors, p, sel));
        }
        let cursor = (content.cursor.shape != CursorShape::Hidden).then(|| {
            let Point { line: Line(l), column: Column(c) } = content.cursor.point;
            ((l + offset) as usize, c, content.cursor.shape)
        }).filter(|(r, _, _)| *r < lines);
        let drawn_cursor = if cursor.is_none() { inverted.iter().rposition(|(n, _)| *n == 1).map(|r| (r, inverted[r].1)) } else { None };
        let f = Frame { drawn_cursor, cursor_drawn: false, cursor_styled: true, float: None, under: None, popup_look: crate::config::PopupLook::default(), zfloat_look: Default::default(), region: None, above: Vec::new(), below: Vec::new(), scroll_px: 0.0, popup: None, tabs: Vec::new(), bar: None, bar_bg: p.selection_bg, fg: def_fg, prompts: Vec::new(), cols, rows, cursor, bg: def_bg, cursor_color: p.cursor_bg, fail_color: p.ansi[1], small: Vec::new(), tones: [p.ansi[2], p.ansi[1], p.ansi[3], p.ansi[4]], images: Vec::new(), ink: Vec::new(), ink_top: 0.0, layers: Vec::new(), ui: Vec::new(), pal: p.clone(), pen_hover: None };
        f
    }

    /// The cursor zash draws, by `[cursor]`: a TUI's own one when the real cursor is hidden, the
    /// style over the program's, and whether zash's look applies where it is (scope: in a zfloat
    /// or not). Once the zfloat is known (after with_images). A block takes the cursor's text colour.
    pub fn apply_cursor(&mut self, conf: &crate::config::CursorConf, cursor_fg: Rgb) {
        use crate::config::CursorScope;
        if self.cursor.is_none() && conf.drawn {
            if let Some((r, c)) = self.drawn_cursor {
                self.cursor = Some((r, c, CursorShape::Block));
                self.cursor_drawn = true;
            }
        }
        let Some((r, c, mut shape)) = self.cursor else { return };
        let inside = self.float.as_ref().is_some_and(|z| r >= z.y && r < z.y + z.h && c >= z.x && c < z.x + z.w);
        self.cursor_styled = match conf.scope {
            CursorScope::All => true,
            CursorScope::Terminal => !inside,
            CursorScope::Zfloat => inside,
        };
        if self.cursor_drawn {
            return; // the program draws the cell itself
        }
        if let Some(s) = conf.style.filter(|_| self.cursor_styled) {
            shape = s;
        }
        self.cursor = Some((r, c, shape));
        if shape == CursorShape::Block {
            if let Some(cell) = self.rows.get_mut(r).and_then(|row| row.get_mut(c)) {
                cell.style.fg = (cursor_fg.0, cursor_fg.1, cursor_fg.2);
            }
        }
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
    @location(4) hole: vec4<f32>,
};
@vertex fn vs(@builtin(vertex_index) vi: u32, @location(0) rect: vec4<f32>, @location(1) color: vec4<f32>, @location(2) p: vec2<f32>, @location(3) hole: vec4<f32>) -> VOut {
    var corners = array<vec2<f32>, 6>(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0));
    let c = corners[vi];
    let q = rect.xy + c * rect.zw;
    var o: VOut;
    o.pos = vec4(q.x / u.screen.x * 2.0 - 1.0, 1.0 - q.y / u.screen.y * 2.0, 0.0, 1.0);
    o.color = color;
    o.local = (c - vec2(0.5)) * rect.zw;
    o.half = rect.zw * 0.5;
    o.p = p;
    o.hole = hole;
    return o;
}
// p.x: corner radius, p.y: edge softness (a shadow is a large softness); both 0: a plain box
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
    if (i.hole.z > 0.0 && i.pos.x >= i.hole.x && i.pos.x < i.hole.x + i.hole.z && i.pos.y >= i.hole.y && i.pos.y < i.hole.y + i.hole.w) { return vec4(0.0); }
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
pub(crate) struct Rect {
    pub(crate) xywh: [f32; 4],
    pub(crate) color: [f32; 4],
    /// corner radius and edge softness (0, 0: a plain box)
    pub(crate) p: [f32; 2],
    /// nothing drawn inside this (x, y, w, h; w 0: none): a shadow cut out under its float
    pub(crate) hole: [f32; 4],
}

pub(crate) const NO_HOLE: [f32; 4] = [0.0; 4];

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
        Rects::with_blend(device, format, wgpu::BlendState::ALPHA_BLENDING)
    }

    /// `blend` other than the usual: the veil mixes colour but keeps the window's alpha (see-through
    /// as the cells it stands for)
    fn with_blend(device: &wgpu::Device, format: wgpu::TextureFormat, blend: wgpu::BlendState) -> Rects {
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
        let attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x2, 3 => Float32x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("zash rects"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: 56, step_mode: wgpu::VertexStepMode::Instance, attributes: &attrs })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(blend), write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let cap = 4096;
        let buf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (cap * 56) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        Rects { pipeline, uniform, bind, buf, cap, count: 0 }
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, rects: &[Rect], w: f32, h: f32) {
        queue.write_buffer(&self.uniform, 0, &as_bytes(&[w, h, 0.0, 0.0]));
        if rects.len() > self.cap {
            self.cap = rects.len().next_power_of_two();
            self.buf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (self.cap * 56) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        }
        let flat: Vec<f32> = rects.iter().flat_map(|r| r.xywh.into_iter().chain(r.color).chain(r.p).chain(r.hole)).collect();
        if !flat.is_empty() {
            queue.write_buffer(&self.buf, 0, &as_bytes(&flat));
        }
        self.count = rects.len() as u32;
    }

    fn draw(&self, pass: &mut wgpu::RenderPass) {
        self.draw_range(pass, 0, u32::MAX);
    }

    /// Rectangles `from..to` of the last upload (something else is drawn between two ranges).
    fn draw_range(&self, pass: &mut wgpu::RenderPass, from: u32, to: u32) {
        let to = to.min(self.count);
        if from >= to {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.buf.slice(..));
        pass.draw(0..6, from..to);
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
    /// the completion popup as last drawn: its rect, the chosen item, and each shown item's row (wheel and clicks, app.rs)
    pub popup_hit: Option<([f32; 4], Option<usize>, Vec<(usize, [f32; 4])>)>,
    /// letters under a dragged zfloat, in the strip it uncovers (buffer, x, y, clip l t r b)
    unders: Vec<(Buffer, f32, f32, [f32; 4])>,
    /// UI hints' big text (OSC 7733;ui): shaped once per content, and where each goes this frame (key, x, y, clip l t r b)
    ui_text: std::collections::HashMap<u64, Buffer>,
    ui_placed: Vec<(u64, f32, f32, [f32; 4])>,
    /// height of the tab bar above the rows (0 with one tab)
    pub top: f32,
    rects: Rects,
    images: Images,
    /// drawn after the text: marks that must stay visible over glyphs
    overlay: Rects,
    /// over the text, under the overlay: floats' covers coming and going, see-through as the window
    veil: Rects,
    /// shaped rows per pane (by pane id)
    /// per pane, per row content: its segments (x offset in pixels, shaped text), each on its column
    rows: std::collections::HashMap<u64, std::collections::HashMap<u64, Vec<(f32, Buffer)>>>,
    /// per pane: the content key of each drawn row, top to bottom (above the screen first)
    order: std::collections::HashMap<u64, Vec<u64>>,
    family: String,
    /// UI hints' font=ui
    ui_family: String,
    /// a font= name to the family fontdb has (fc-match for generics and aliases), found once per name
    ui_fonts: std::collections::HashMap<String, Option<String>>,
    features: glyphon::cosmic_text::FontFeatures,
    pub font_px: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub pad: f32,
    /// cursor animation: time constant in ms (0: jumps), and the trailing smear
    pub cursor_ms: f32,
    pub cursor_trail: bool,
    /// where the zfloat was last frame (id, pixel origin): when it moves, the cursor moves with it at once
    float_at: Option<(u64, (f32, f32))>,
    /// background opacity (pre-multiplied into the clear colour)
    pub opacity: f32,
    /// where the mouse is (pixels), for hover on what zash draws itself (zfloat buttons)
    pub pointer: Option<(f32, f32)>,
    /// UI hints with an id this frame: name and pixel rect (app.rs: hover and click events)
    pub ui_hits: Vec<(String, [f32; 4])>,
    /// the UI hint pressed now (its id), drawn pressed
    pub ui_press: Option<String>,
    /// each interactive hint's hover and press, eased (0..1), and the time since the last frame for it
    ui_level: std::collections::HashMap<String, [f32; 2]>,
    ui_dt: f32,
    ui_tick: Option<std::time::Instant>,
    /// a zfloat dragged by its title: where the press was (pixels), its id, cell x, y, w, h then. It is
    /// drawn where the pointer puts it, to the pixel, while zeit moves it in cells
    pub float_drag: Option<(f32, f32, u64, usize, usize, usize, usize)>,
    /// zeit has reported this drag (zunder): its end, not only zash's release, ends it
    pub float_drag_seen: bool,
    anim: Option<CursorAnim>,
    /// text placement: centred in the leftover space, and moved by an offset (pixels)
    pub center: bool,
    pub offset: (f32, f32),
    /// the backdrop blur stage, made the first time a popup asks for it
    blur: Option<Blur>,
    /// text effects (OSC 7733;fx): their settings, shaped characters and next frame
    pub fx: crate::fx_draw::FxLayer,
    /// GLSL panels for zfloats: which file for which title, the compiled ones, and this frame's
    /// (the shader and the rectangle index it is drawn at, between the panel's frame and its cells)
    pub zfloat_shaders: crate::config::ZfloatShaders,
    /// programs' floats animated in, out and along (layers.rs), and how (`[layers]`)
    pub layer_anim: crate::layers::Layers,
    pub layers_conf: crate::config::LayersConf,
    /// the shader over the floats' shadows ([layers] shader), compiled when first needed
    pub layer_shader: Option<String>,
    layer_fx: Option<crate::layer_fx::LayerFx>,
    layer_fx_failed: Option<String>,
    layer_fx_t0: std::time::Instant,
    shaders: crate::panel_shader::Shaders,
    panel_shader: Option<(std::path::PathBuf, u32)>,
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
            veil: Rects::with_blend(device, format, wgpu::BlendState { color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::SrcAlpha, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add }, alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add } }),
            font_system: { let _ = &mut font_system; font_system },
            swash: SwashCache::new(),
            viewport,
            atlas,
            text,
            text2,
            bars: Vec::new(),
            popup_hit: None,
            unders: Vec::new(),
            ui_text: Default::default(),
            ui_placed: Vec::new(),
            top: 0.0,
            rows: std::collections::HashMap::new(),
            order: std::collections::HashMap::new(),
            family: cfg.font_family.clone(),
            ui_family: cfg.ui_font.clone(),
            ui_fonts: std::collections::HashMap::new(),
            features,
            font_px: 0.0,
            cell_w: 0.0,
            cell_h: 0.0,
            pad: 0.0,
            cursor_ms: cfg.cursor_ms,
            cursor_trail: cfg.cursor_trail,
            float_at: None,
            opacity: cfg.opacity,
            pointer: None,
            ui_hits: Vec::new(),
            ui_press: None,
            ui_level: std::collections::HashMap::new(),
            ui_dt: 0.0,
            ui_tick: None,
            float_drag: None,
            float_drag_seen: false,
            anim: None,
            fx: crate::fx_draw::FxLayer::new(cfg.fx.clone()),
            zfloat_shaders: cfg.zfloat_shaders.clone(),
            layer_anim: Default::default(),
            layers_conf: cfg.layers,
            layer_shader: cfg.layer_shader.clone(),
            layer_fx: None,
            layer_fx_failed: None,
            layer_fx_t0: std::time::Instant::now(),
            shaders: Default::default(),
            panel_shader: None,
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
        self.fx.font_changed();
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
    /// `hidden`: runs whose characters an effect draws this frame (left blank here).
    fn shape_rows(&mut self, pane: u64, frame: &Frame, width: f32, hidden: &std::collections::HashSet<String>) {
        let mut old = self.rows.remove(&pane).unwrap_or_default();
        let mut cache: std::collections::HashMap<u64, Vec<(f32, Buffer)>> = std::collections::HashMap::with_capacity(frame.rows.len() + 4);
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
                crate::fx_draw::hides(c, hidden).hash(&mut h);
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
            // on the grid: plain characters (their advance is the cell's) go together, each other one
            // (symbols, icons, wide ones, from fonts of another width) is a segment at its own column,
            // so an advance a little off never shifts the rest of the row; runs of one style are a span
            let mut segs: Vec<(usize, Vec<(String, Style2)>)> = Vec::new();
            let mut spans: Vec<(String, Style2)> = Vec::new();
            let mut start = 0usize;
            for (col, c) in row.iter().enumerate() {
                if c.spacer {
                    continue;
                }
                // (empty cells and powerline shapes are drawn as spaces: plain too)
                let plain = (c.ch == '\0' || powerline(c.ch).is_some() || (' '..='~').contains(&c.ch)) && c.zw.is_none();
                if !plain && !spans.is_empty() {
                    segs.push((start, std::mem::take(&mut spans)));
                }
                // a segment starts at its first character's column (after a wide one: past its spacer)
                if spans.is_empty() {
                    start = col;
                }
                let mut s = String::new();
                // powerline shapes are drawn as rectangles (rect_list), not as glyphs
                // an effect drawing this cell's text leaves it blank here
                let blank = crate::fx_draw::hides(c, hidden);
                s.push(if blank || c.ch == '\0' || powerline(c.ch).is_some() { ' ' } else { c.ch });
                if let Some(z) = c.zw.as_ref().filter(|_| !blank) {
                    s.extend(z.iter());
                }
                match spans.last_mut() {
                    Some((t, st)) if *st == c.style => t.push_str(&s),
                    _ => spans.push((s, c.style)),
                }
                if !plain {
                    segs.push((start, std::mem::take(&mut spans)));
                }
            }
            if !spans.is_empty() {
                segs.push((start, spans));
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
            let default = Attrs::new().family(Family::Name(&family));
            let mut shaped = Vec::with_capacity(segs.len());
            for (col, spans) in segs {
                let x = col as f32 * self.cell_w;
                let mut b = Buffer::new(&mut self.font_system, Metrics::new(self.font_px, self.cell_h));
                b.set_size(Some((width - x).max(self.cell_w)), Some(self.cell_h));
                b.set_rich_text(spans.iter().map(|(t, st)| (t.as_str(), mk(st))), &default, Shaping::Advanced, None);
                b.shape_until_scroll(&mut self.font_system, false);
                shaped.push((x, b));
            }
            cache.insert(key, shaped);
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
            out.push(Rect { xywh: [ox - self.pad, oy + *r as f32 * ch, w, ch], color: col, p: [0.0, 0.0], hole: NO_HOLE });
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
            // shapes and lines go on top of every background of the row (a run is only flushed when
            // it ends, which can be after a shape in it: an explicit status-line colour hid the pill ends)
            let mut fore: Vec<Rect> = Vec::new();
            // backgrounds, merged across cells of one colour
            let mut run: Option<(usize, usize, Rgb)> = None;
            for (c, cell) in row.iter().enumerate() {
                match (run, cell.bg) {
                    (Some((s, e, col)), Some(b)) if b == col && e == c => run = Some((s, c + 1, col)),
                    (prev, bg) => {
                        if let Some((s, e, col)) = prev {
                            out.push(Rect { xywh: [pad + s as f32 * cw, y, (e - s) as f32 * cw, ch], color: col.f32(), p: [0.0, 0.0], hole: NO_HOLE });
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
                    fore.push(match shape {
                        // a rounded end reaches into the next cell (the pill's body, same colour)
                        Powerline::RoundLeft => Rect { xywh: [x, y, 2.0 * cw, ch], color: fg, p: [r, 0.0], hole: NO_HOLE },
                        Powerline::RoundRight => Rect { xywh: [x - cw, y, 2.0 * cw, ch], color: fg, p: [r, 0.0], hole: NO_HOLE },
                        Powerline::TriRight => Rect { xywh: [x, y, cw, ch], color: fg, p: [0.0, -1.0], hole: NO_HOLE },
                        Powerline::TriLeft => Rect { xywh: [x, y, cw, ch], color: fg, p: [0.0, -2.0], hole: NO_HOLE },
                    });
                }
                let w = if cell.wide { 2.0 * cw } else { cw };
                let thick = (self.font_px / 14.0).max(1.0).round();
                if cell.underline {
                    fore.push(Rect { xywh: [pad + c as f32 * cw, y + ch - thick * 2.0, w, thick], color: fg, p: [0.0, 0.0], hole: NO_HOLE });
                }
                if cell.strike {
                    fore.push(Rect { xywh: [pad + c as f32 * cw, y + ch * 0.55, w, thick], color: fg, p: [0.0, 0.0], hole: NO_HOLE });
                }
            }
            if let Some((s, e, col)) = run {
                out.push(Rect { xywh: [pad + s as f32 * cw, y, (e - s) as f32 * cw, ch], color: col.f32(), p: [0.0, 0.0], hole: NO_HOLE });
            }
            out_row.extend(fore);
            match band {
                Some((b0, b1)) => out_all.extend(clipped(out_row, [clip[0], b0, clip[2], b1 - b0])),
                None => out_all.extend(out_row),
            }
        }
        let mut out = out_all;
        if let Some((r, c, shape)) = frame.cursor {
            let (x, y) = cur.map(|c| c.0).unwrap_or((pad + c as f32 * cw, oy + r as f32 * ch + shift));
            // the smear: from the lagging tail to the head, along one line only
            if let Some((_, (tx, ty))) = cur.filter(|_| self.cursor_trail && frame.cursor_styled) {
                if (ty - y).abs() < 1.0 && (tx - x).abs() > 1.0 {
                    let (a, b) = (tx.min(x), tx.max(x));
                    let mut col = frame.cursor_color.f32();
                    col[3] = 0.35;
                    out.push(Rect { xywh: [a, y + ch * 0.15, b - a, ch * 0.7], color: col, p: [0.0, 0.0], hole: NO_HOLE });
                }
            }
            // a cursor the program draws itself: it has its block, zash only adds the trail above
            if frame.cursor_drawn {
                return clipped(out, clip);
            }
            let wide = frame.rows.get(r).and_then(|row| row.get(c)).is_some_and(|cell| cell.wide);
            let w = if wide { 2.0 * cw } else { cw };
            let col = frame.cursor_color.f32();
            let t = (self.font_px / 9.0).max(1.0).round();
            match shape {
                CursorShape::Beam => out.push(Rect { xywh: [x, y, t, ch], color: col, p: [0.0, 0.0], hole: NO_HOLE }),
                CursorShape::Underline => out.push(Rect { xywh: [x, y + ch - t, w, t], color: col, p: [0.0, 0.0], hole: NO_HOLE }),
                CursorShape::HollowBlock => {
                    out.push(Rect { xywh: [x, y, w, t], color: col, p: [0.0, 0.0], hole: NO_HOLE });
                    out.push(Rect { xywh: [x, y + ch - t, w, t], color: col, p: [0.0, 0.0], hole: NO_HOLE });
                    out.push(Rect { xywh: [x, y, t, ch], color: col, p: [0.0, 0.0], hole: NO_HOLE });
                    out.push(Rect { xywh: [x + w - t, y, t, ch], color: col, p: [0.0, 0.0], hole: NO_HOLE });
                }
                _ => {
                    // a block; the character on it is drawn in the cursor's text colour (Frame::extract)
                    out.push(Rect { xywh: [x, y, w, ch], color: col, p: [0.0, 0.0], hole: NO_HOLE });
                }
            }
        }
        clipped(out, clip)
    }

    /// A pane's UI hints (ui.rs): panels and pills into `rects` (under the cells' backgrounds), words placed for the text pass.
    /// Interactive ones (id=) are put in `ui_hits` and drawn lifted under the pointer, more while pressed
    /// (eased over ~120 ms); returns true while such a change is still under way.
    fn ui_regions(&mut self, frame: &Frame, ox: f32, oy: f32, clip: [f32; 4], rects: &mut Vec<Rect>) -> bool {
        use crate::ui::{Kind, State};
        let (cw, ch) = (self.cell_w, self.cell_h);
        let dt = self.ui_dt;
        let mut moving = false;
        for (r, y) in &frame.ui {
            let px = [ox + r.rect[0] as f32 * cw, oy + *y as f32 * ch + frame.scroll_px, r.rect[2] as f32 * cw, r.rect[3] as f32 * ch];
            let Some(mut col) = crate::ui::role(&r.role, &frame.pal) else { continue };
            let disabled = r.state == State::Disabled;
            if let Some(id) = r.id.as_deref() {
                let inside = self.pointer.is_some_and(|(mx, my)| mx >= px[0] && mx < px[0] + px[2] && my >= px[1] && my < px[1] + px[3] && mx >= clip[0] && my >= clip[1] && mx < clip[0] + clip[2] && my < clip[1] + clip[3]);
                if !disabled { self.ui_hits.push((id.to_string(), px)) }
                let want = [f32::from(inside && !disabled), f32::from(self.ui_press.as_deref() == Some(id))];
                let lv = self.ui_level.entry(id.to_string()).or_insert([0.0, 0.0]);
                for k in 0..2 {
                    let step = dt / 0.12;
                    lv[k] = if lv[k] < want[k] { (lv[k] + step).min(want[k]) } else { (lv[k] - step).max(want[k]) };
                    moving |= lv[k] != want[k];
                }
                let on = crate::ui::on_role(&r.role, &frame.pal);
                col = crate::ui::mix(col, on, 0.08 * lv[0] + 0.10 * lv[1]);
            }
            if r.state == State::Done {
                col = crate::ui::role("primary-container", &frame.pal).unwrap_or(col);
            }
            let fill = |c: Rgb| { let mut f = c.f32(); if disabled { f[3] *= 0.38 } f };
            // the keyboard is here (the program says so): an accent ring, 2px out
            let ring = |b: [f32; 4], rad: f32| (r.state == State::Focus).then(|| Rect { xywh: [b[0] - 2.0, b[1] - 2.0, b[2] + 4.0, b[3] + 4.0], color: frame.pal.cursor_bg.f32(), p: [rad + 2.0, 0.0], hole: NO_HOLE });
            // out by `pad` cells (as wide across as up and down: an even margin round the text)
            let grow = |p: f32, py: f32| [px[0] - p, px[1] - py, px[2] + 2.0 * p, px[3] + 2.0 * py];
            match &r.kind {
                Kind::Panel { radius, border, shadow, pad } => {
                    let b = grow(pad * cw, (pad * cw).min(ch * 0.5));
                    let rad = radius.unwrap_or((ch * 0.55).round()).min(b[3] / 2.0);
                    let mut out = if *shadow > 0.0 { layer_frame(b, ch, frame.fg.f32(), 1.0, shadow * 0.6) } else { Vec::new() };
                    if let Some(e) = border.as_deref().and_then(|e| crate::ui::role(e, &frame.pal)) {
                        let mut c = e.f32();
                        c[3] = 0.5;
                        out.push(Rect { xywh: [b[0] - 1.0, b[1] - 1.0, b[2] + 2.0, b[3] + 2.0], color: c, p: [rad + 1.0, 0.0], hole: NO_HOLE });
                    }
                    out.extend(ring(b, rad));
                    out.push(Rect { xywh: b, color: fill(col), p: [rad, 0.0], hole: NO_HOLE });
                    rects.extend(clipped(out, clip));
                }
                Kind::Button { label, pad } => {
                    let b = grow(pad * cw, (ch * 0.08).round());
                    let rad = (ch * 0.35).round().min(b[3] / 2.0);
                    let mut out: Vec<Rect> = ring(b, rad).into_iter().collect();
                    out.push(Rect { xywh: b, color: fill(col), p: [rad, 0.0], hole: NO_HOLE });
                    rects.extend(clipped(out, clip));
                    if let Some(l) = label {
                        let on = crate::ui::on_role(&r.role, &frame.pal);
                        self.ui_label(l, if disabled { crate::ui::mix(on, col, 0.62) } else { on }, px, clip, true);
                    }
                }
                Kind::Input { label, placeholder } => {
                    let b = grow(0.5 * cw, (ch * 0.08).round());
                    let rad = (ch * 0.3).round().min(b[3] / 2.0);
                    // the edge: the outline, the accent while it has the keyboard
                    let focus = r.state == State::Focus;
                    let e = if focus { frame.pal.cursor_bg } else { crate::ui::role("outline", &frame.pal).unwrap_or(frame.fg) };
                    let w = if focus { 2.0 } else { 1.0 };
                    let out = vec![
                        Rect { xywh: [b[0] - w, b[1] - w, b[2] + 2.0 * w, b[3] + 2.0 * w], color: fill(e), p: [rad + w, 0.0], hole: NO_HOLE },
                        Rect { xywh: b, color: fill(col), p: [rad, 0.0], hole: NO_HOLE },
                    ];
                    rects.extend(clipped(out, clip));
                    if let Some(l) = label {
                        let t = if *placeholder { crate::ui::role("on-surface-variant", &frame.pal).unwrap_or(frame.fg) } else { crate::ui::on_role("surface", &frame.pal) };
                        self.ui_label(l, t, px, clip, false);
                    }
                }
                Kind::Progress { value } => {
                    // a thin rounded track through the middle of the rect, the done part in the role's colour
                    let t = (ch * 0.28).round().max(3.0);
                    let ty = px[1] + (px[3] - t) / 2.0;
                    let track = crate::ui::role("surface-container-highest", &frame.pal).unwrap_or(frame.bg);
                    let mut out = vec![Rect { xywh: [px[0], ty, px[2], t], color: fill(track), p: [t / 2.0, 0.0], hole: NO_HOLE }];
                    if *value > 0.0 {
                        out.push(Rect { xywh: [px[0], ty, (px[2] * value).max(t), t], color: fill(col), p: [t / 2.0, 0.0], hole: NO_HOLE });
                    }
                    rects.extend(clipped(out, clip));
                }
                Kind::Pill { label, fg, pad } => {
                    let b = grow(pad * cw, (ch * 0.08).round());
                    let mut out: Vec<Rect> = ring(b, b[3] / 2.0).into_iter().collect();
                    out.push(Rect { xywh: b, color: fill(col), p: [b[3] / 2.0, 0.0], hole: NO_HOLE });
                    rects.extend(clipped(out, clip));
                    if let Some(l) = label {
                        let on = fg.as_deref().and_then(|f| crate::ui::role(f, &frame.pal)).unwrap_or_else(|| crate::ui::on_role(&r.role, &frame.pal));
                        self.ui_label(l, on, px, clip, true);
                    }
                }
                Kind::Text(l) => self.ui_label(l, col, px, clip, false),
                Kind::Line(e) => {
                    use crate::ui::Edge;
                    let t = (ch / 18.0).round().max(1.0);
                    let xywh = match e {
                        Edge::Top => [px[0], px[1], px[2], t],
                        Edge::Bottom => [px[0], px[1] + px[3] - t, px[2], t],
                        Edge::Left => [px[0], px[1], t, px[3]],
                        Edge::Right => [px[0] + px[2] - t, px[1], t, px[3]],
                    };
                    rects.extend(clipped(vec![Rect { xywh, color: fill(col), p: [0.0, 0.0], hole: NO_HOLE }], clip));
                }
            }
        }
        moving
    }

    /// A UI hint's words, shaped once (kept by what they are) and placed: left, centred in the rect's height, cut at it and the pane.
    fn ui_label(&mut self, l: &crate::ui::Label, col: Rgb, px: [f32; 4], clip: [f32; 4], centre: bool) {
        let size = (self.font_px * l.scale).round();
        // the family a name means: the cell font, the UI font (a sans when it is not installed), a generic, or a family
        let mono = matches!(l.font.as_str(), "mono" | "cell");
        let name = match l.font.as_str() {
            "mono" | "cell" => self.family.clone(),
            "ui" => self.ui_family.clone(),
            n => n.to_string(),
        };
        if !mono && !self.ui_fonts.contains_key(&name) {
            let have = |n: &str| self.font_system.db().faces().any(|f| f.families.iter().any(|(fam, _)| fam.eq_ignore_ascii_case(n)));
            let found = if have(&name) { Some(name.clone()) } else {
                std::process::Command::new("fc-match").args(["-f", "%{family[0]}", &name]).output().ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|f| !f.is_empty() && have(f))
            };
            self.ui_fonts.insert(name.clone(), found);
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (&l.text, l.scale.to_bits(), l.weight, &name, col.0, col.1, col.2, size.to_bits()).hash(&mut h);
        let key = h.finish();
        if !self.ui_text.contains_key(&key) {
            let found = if mono { Some(name.clone()) } else { self.ui_fonts.get(&name).cloned().flatten() };
            let fam = match found.as_deref() {
                Some(n) => Family::Name(n),
                None if name == "serif" => Family::Serif,
                None => Family::SansSerif,
            };
            let mut b = Buffer::new(&mut self.font_system, Metrics::new(size, size * 1.2));
            b.set_size(None, Some(size * 1.2));
            let mut a = Attrs::new().family(fam).color(Color::rgb(col.0, col.1, col.2)).weight(Weight(l.weight));
            if mono {
                a = a.font_features(self.features.clone());
            }
            b.set_text(&l.text, &a, Shaping::Advanced, None);
            b.shape_until_scroll(&mut self.font_system, false);
            self.ui_text.insert(key, b);
        }
        let top = (px[1] + (px[3] - size * 1.2) / 2.0).round();
        let wide = self.ui_text.get(&key).map_or(0.0, |b| b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max));
        let left = if centre { (px[0] + (px[2] - wide) / 2.0).round() } else { px[0] };
        let (c0, c1) = ([clip[0], clip[1]], [clip[0] + clip[2], clip[1] + clip[3]]);
        let cut = [px[0].max(c0[0]), (px[1] - size * 0.3).max(c0[1]), (px[0] + px[2]).min(c1[0]), (px[1] + px[3] + size * 0.3).min(c1[1])];
        if cut[2] > cut[0] && cut[3] > cut[1] {
            self.ui_placed.push((key, left, top, cut));
        }
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
        let (cur, mut animating) = self.cursor_motion(frame, fox, foy);
        // text effects: one clock for the whole frame, and its cost for the budget (fx_draw.rs)
        let fx_start = std::time::Instant::now();
        let fx_now = crate::fx::now();
        let mut fx_moving = false;
        let mut fx_glyphs: Vec<(usize, Vec<crate::fx_draw::Glyph>)> = Vec::new();
        self.panel_shader = None;
        self.shaders.tick();
        self.unders.clear();
        self.ui_placed.clear();
        self.ui_hits.clear();
        // the hints' fades: real time between frames, at most a tenth of a second (a frame after a pause jumps)
        let now = std::time::Instant::now();
        self.ui_dt = self.ui_tick.map(|t| now.duration_since(t).as_secs_f32().min(0.1)).unwrap_or(0.0);
        self.ui_tick = Some(now);
        let mut rects = Vec::new();
        let mut over = Vec::new();
        let mut veil: Vec<Rect> = Vec::new();
        // the shadows' shader: the floats it gets (pixels, how far each has come), the cell height
        let fx_on = self.layer_fx_ready(device);
        let shadow_k = if fx_on { 0.0 } else { self.layers_conf.shadow };
        let mut fx_floats: Vec<([f32; 4], f32)> = Vec::new();
        let mut fx_cell = 0.0f32;
        // floats (a multiplexer's app popups): their pixel rect, for the title and buttons later
        let mut floats: Vec<([f32; 4], crate::graphics::ZFloat, Rgb, Rgb, Rgb)> = Vec::new();
        // per pane: a dragged zfloat's cell rect and how far it is drawn from it (its contents move along)
        let mut moves: Vec<Option<([f32; 4], f32, f32)>> = vec![None; panes.len()];
        for (pi, p) in panes.iter().enumerate() {
            let spans = crate::fx_draw::FxLayer::spans(p.frame);
            let hidden = if spans.is_empty() { Default::default() } else { self.fx.hidden(&spans, fx_now) };
            self.shape_rows(p.id, p.frame, p.w - 2.0 * self.pad, &hidden);
            let (ox, oy) = self.origin(p.x, p.y, p.w, p.h);
            let clip = [p.x, p.y.max(self.top), p.w, p.y + p.h - p.y.max(self.top)];
            // where the zfloat is drawn this frame: drawn marks inside it are cut at its edges, the ones
            // under it are cut away where it covers them
            let mut float_rect: Option<[f32; 4]> = None;
            if !p.frame.ui.is_empty() {
                animating |= self.ui_regions(p.frame, ox, oy, clip, &mut rects);
            }
            if let Some(z) = &p.frame.float {
                // under the text: a soft shadow, the edge (the accent when it has the keyboard), the panel
                let mut r = [ox + z.x as f32 * self.cell_w, oy + z.y as f32 * self.cell_h, z.w as f32 * self.cell_w, z.h as f32 * self.cell_h];
                // how far from its cell (at most a cell and a half: zeit catches up), its size unchanged (a move, not a resize)
                // only while zeit says the drag goes on (it sends what is under the edge then): a release zash
                // missed must not leave the zfloat following the pointer, its text over the cells beside it
                let dragging = p.frame.under.as_ref().is_some_and(|(u, _)| *u == z.id);
                if !dragging && self.float_drag.is_some_and(|d| d.2 == z.id) && p.frame.under.is_none() && self.float_drag_seen {
                    self.float_drag = None;
                }
                self.float_drag_seen |= dragging;
                let shift = self.float_drag.zip(self.pointer).filter(|((_, _, id, _, _, w, h), _)| *id == z.id && *w == z.w && *h == z.h).filter(|_| dragging).map(|((px, py, _, x0, y0, _, _), (mx, my))| {
                    let dx = (x0 as f32 - z.x as f32) * self.cell_w + (mx - px);
                    let dy = (y0 as f32 - z.y as f32) * self.cell_h + (my - py);
                    // never out of the window: as far as the pane's edges (none past an edge it is docked at,
                    // which it is stretched to already)
                    let right = if z.x + z.w >= p.frame.cols { 0.0 } else { (p.x + p.w - (r[0] + r[2])).max(0.0) };
                    let down = if z.y + z.h >= p.frame.rows.len() { 0.0 } else { (p.y + p.h - (r[1] + r[3])).max(0.0) };
                    let (left, up) = ((r[0] - p.x).max(0.0), (r[1] - p.y.max(self.top)).max(0.0));
                    // and never past the multiplexer's area (zeit's tab bar stays clear)
                    let (left, right, up, down) = match z.area {
                        Some((ax, ay, aw, ah)) => {
                            let (x0, y0) = (ox + ax as f32 * self.cell_w, oy + ay as f32 * self.cell_h);
                            let (x1, y1) = (ox + (ax + aw) as f32 * self.cell_w, oy + (ay + ah) as f32 * self.cell_h);
                            (left.min((r[0] - x0).max(0.0)), right.min((x1 - r[0] - r[2]).max(0.0)), up.min((r[1] - y0).max(0.0)), down.min((y1 - r[1] - r[3]).max(0.0)))
                        }
                        None => (left, right, up, down),
                    };
                    (dx.clamp(-1.5 * self.cell_w, 1.5 * self.cell_w).clamp(-left, right), dy.clamp(-1.5 * self.cell_h, 1.5 * self.cell_h).clamp(-up, down))
                });
                // always (not only while dragged): the rows through it are cut at its edges, so glyphs wider
                // than their cell neither reach out of it nor into it from the text beside it
                let (sdx, sdy) = shift.unwrap_or((0.0, 0.0));
                moves[pi] = Some((r, sdx, sdy));
                // docked at the last column or row: out to the window's edge (cells rarely fill it)
                let edge = 4.0;
                if z.x + z.w >= p.frame.cols {
                    r[2] = (p.x + p.w - edge - r[0]).max(r[2]);
                }
                if z.y + z.h >= p.frame.rows.len() {
                    r[3] = (p.y + p.h - edge - r[1]).max(r[3]);
                }
                if let Some((dx, dy)) = shift {
                    r[0] += dx;
                    r[1] += dy;
                }
                // the strip the shift uncovers (its cells hold the zfloat in zeit): what zeit says is under it
                if let (Some((c, dx, dy)), Some((uid, cells))) = (moves[pi], p.frame.under.as_ref()) {
                    if *uid == z.id {
                        let (x0, y0, x1, y1) = (c[0], c[1], c[0] + c[2], c[1] + c[3]);
                        let side = if dx > 0.0 { [x0, y0, (x0 + dx).min(x1), y1] } else { [(x1 + dx).max(x0), y0, x1, y1] };
                        let (vx0, vx1) = if dx > 0.0 { (side[2], x1) } else { (x0, side[0]) };
                        let top = if dy > 0.0 { [vx0, y0, vx1, (y0 + dy).min(y1)] } else { [vx0, (y1 + dy).max(y0), vx1, y1] };
                        for (col, row, text, fg, bg, flags) in cells {
                            let cell = [ox + *col as f32 * self.cell_w, oy + *row as f32 * self.cell_h];
                            for s in [side, top] {
                                let cut = [cell[0].max(s[0]), cell[1].max(s[1]), (cell[0] + self.cell_w).min(s[2]), (cell[1] + self.cell_h).min(s[3])];
                                if cut[2] - cut[0] < 0.5 || cut[3] - cut[1] < 0.5 {
                                    continue;
                                }
                                if let Some(bg) = bg {
                                    rects.push(Rect { xywh: [cut[0], cut[1], cut[2] - cut[0], cut[3] - cut[1]], color: bg.f32(), p: [0.0, 0.0], hole: NO_HOLE });
                                }
                                if !text.trim().is_empty() {
                                    let mut b = Buffer::new(&mut self.font_system, Metrics::new(self.font_px, self.cell_h));
                                    b.set_size(None, Some(self.cell_h));
                                    // as the pane draws its text: the font's features, bold, italic
                                    let mut a = Attrs::new().family(Family::Name(&self.family)).color(Color::rgb(fg.0, fg.1, fg.2)).font_features(self.features.clone());
                                    if flags & 1 != 0 {
                                        a = a.weight(Weight::BOLD);
                                    }
                                    if flags & 2 != 0 {
                                        a = a.style(Style::Italic);
                                    }
                                    b.set_text(text, &a, Shaping::Advanced, None);
                                    b.shape_until_scroll(&mut self.font_system, false);
                                    self.unders.push((b, cell[0], cell[1], cut));
                                }
                            }
                        }
                    }
                }
                let rad = 12.0;
                // the look from [zfloat] in zash.toml (live): how solid the panel is, what its frame is
                let look = p.frame.zfloat_look;
                // the shadow fades with the panel (under a see-through one it only muddies it)
                rects.push(Rect { xywh: [r[0] - 8.0, r[1] - 6.0, r[2] + 16.0, r[3] + 16.0], color: [0.0, 0.0, 0.0, 0.30 * look.opacity * look.opacity], p: [rad, 8.0], hole: NO_HOLE });
                let mut panel = z.bg.unwrap_or(p.frame.bar_bg).f32();
                panel[3] = look.opacity;
                let edge = match look.border {
                    crate::config::ZfloatBorder::Accent if z.focus => Some({ let mut c = p.frame.cursor_color.f32(); c[3] = 0.85; c }),
                    crate::config::ZfloatBorder::Accent => Some({ let mut c = p.frame.fg.f32(); c[3] = 0.16; c }),
                    // panel: its own colour, so no line of another one
                    crate::config::ZfloatBorder::Panel | crate::config::ZfloatBorder::None => None,
                };
                if let Some(edge) = edge {
                    rects.push(Rect { xywh: [r[0] - 1.0, r[1] - 1.0, r[2] + 2.0, r[3] + 2.0], color: edge, p: [rad + 1.0, 0.0], hole: NO_HOLE });
                }
                // the panel: its shader if the title has one that compiles, else its colour
                // the shader zeit names for it, else zash.toml's for its title; a preset's values morph in
                let name = z.shader.clone().or_else(|| self.zfloat_shaders.for_title(&z.title).cloned()).filter(|n| n != "none" && self.fx.shaders_on());
                let shader = name.map(|n| { let o = self.zfloat_shaders.options.get(&n).cloned().unwrap_or_default(); self.shaders.resolve(&n, o) })
                    .filter(|c| self.shaders.get(device, queue, self.format, &c.path).is_some());
                match shader {
                    Some(choice) => {
                        let place = crate::panel_shader::Place { rect: r, screen: [width as f32, height as f32], panel: { let mut c = z.bg.unwrap_or(p.frame.bar_bg).f32(); c[3] = 1.0; c }, accent: p.frame.cursor_color.f32(), focus: z.focus, opacity: look.opacity.max(0.05), radius: rad };
                        let (values, phase) = self.shaders.values(z.id, &choice, z.morph.unwrap_or(self.zfloat_shaders.morph), fx_now);
                        if let Some(s) = self.shaders.loaded(&choice.path) {
                            s.update(queue, &place, fx_now, phase, &values);
                        }
                        self.panel_shader = Some((choice.path, rects.len() as u32));
                        fx_moving = true;
                    }
                    None => rects.push(Rect { xywh: r, color: panel, p: [rad, 0.0], hole: NO_HOLE }),
                }
                // the title bar: a slightly lighter band on its row (square under it) (no line under it: the user, 2026-10-21)
                let f = p.frame.fg.f32();
                // (a tint over the panel, so a see-through panel stays as see-through under it)
                rects.push(Rect { xywh: [r[0], r[1], r[2], self.cell_h], color: [f[0], f[1], f[2], 0.05], p: [rad.min(self.cell_h / 2.0), 0.0], hole: NO_HOLE });
                float_rect = Some(r);
                floats.push((r, z.clone(), p.frame.fg, p.frame.cursor_color, p.frame.fail_color));
            }
            // the program's floats (zett's popups, OSC 7733;layers) raised: a soft shadow and a hairline
            // edge under the float's cells, which cover the rest (as zeit's zfloat panel, as Neovide)
            rects.extend(moved(self.rect_list(p.frame, if p.focused { cur } else { None }, ox, oy, clip), moves[pi]));
            // after the cells (the pane's background is one of them): a drop shadow below and to the
            // right, outside the float so it does not tint it, and a hairline round it
            // the floats this frame matched to last frame's (layers.rs): one coming fades in from the
            // background, one moving has its frame slide, one gone fades and shrinks out (below)
            let (cw, ch) = (self.cell_w, self.cell_h);
            let px = |l: &[usize; 4]| [ox + l[0] as f32 * cw, oy + l[1] as f32 * ch, l[2] as f32 * cw, l[3] as f32 * ch];
            let dur = std::time::Duration::from_millis(self.layers_conf.ms as u64);
            let anim = self.layers_conf.animate && !dur.is_zero();
            let t_now = std::time::Instant::now();
            if anim {
                let rows = &p.frame.rows;
                let cursor = p.frame.cursor.map(|(r, c, _)| (r, c));
                self.layer_anim.update(p.id, &p.frame.layers, t_now, |r| layer_cells(rows, r), |r| crate::layers::side_of(r, cursor, rows.len()));
            }
            let shown: Vec<([f32; 4], f32, crate::layers::Side)> = if anim {
                self.layer_anim.seen.get(&p.id).into_iter().flatten().map(|s| {
                    let k = |since: std::time::Instant| crate::layers::ease(t_now.duration_since(since).as_secs_f32() / dur.as_secs_f32());
                    let to = px(&s.rect);
                    let at = match s.from {
                        Some((f, t)) if k(t) < 1.0 => {
                            let (from, e) = (px(&f), k(t));
                            [0, 1, 2, 3].map(|i| from[i] + (to[i] - from[i]) * e)
                        }
                        _ => to,
                    };
                    (at, k(s.born), s.side)
                }).collect()
            } else {
                p.frame.layers.iter().map(|l| (px(l), 1.0, crate::layers::Side::Centre)).collect()
            };
            for (r, a, side) in &shown {
                // coming: it unrolls from the side it opens on (the background holds the rest) and
                // the background over what is out thins away
                // "none": there at once (as Neovide); "fade": in place, only the fade
                let a = &if self.layers_conf.open == crate::config::LayerOpen::None { 1.0 } else { *a };
                let open = if self.layers_conf.open == crate::config::LayerOpen::Unroll { crate::layers::unrolled(*r, *side, *a) } else { *r };
                // shadow_over: drawn after the text (on the veil, the window's alpha kept), so it darkens it
                let frame = clipped(layer_frame(open, ch, p.frame.fg.f32(), (*a * 2.0).min(1.0), shadow_k), clip);
                fx_floats.push((open, (*a * 2.0).min(1.0)));
                fx_cell = ch;
                if self.layers_conf.shadow_over { veil.extend(frame) } else { rects.extend(frame) }
                if *a < 1.0 {
                    // as the window clears: its colour times its opacity (pre-multiplied), on the veil
                    let o = self.opacity;
                    let mut bg = { let b = p.frame.bg.f32(); [b[0] * o, b[1] * o, b[2] * o, 1.0] };
                    let mut cover = outside(*r, open).into_iter().map(|xywh| Rect { xywh, color: bg, p: [0.0, 0.0], hole: NO_HOLE }).collect::<Vec<_>>();
                    bg[3] = (1.0 - a) * 0.8;
                    cover.push(Rect { xywh: open, color: bg, p: [0.0, 0.0], hole: NO_HOLE });
                    veil.extend(clipped(cover, clip));
                }
            }
            // gone: its last cells over what is under it now, fading and rolling back to its side
            if anim && self.layers_conf.fade_out && self.layers_conf.open != crate::config::LayerOpen::None {
                let ghosts: Vec<([usize; 4], Vec<crate::layers::GhostCell>, f32, crate::layers::Side)> = self.layer_anim.gone.iter().filter(|g| g.pane == p.id).map(|g| (g.rect, g.cells.clone(), 1.0 - crate::layers::ease(t_now.duration_since(g.at).as_secs_f32() / dur.as_secs_f32()), g.side)).collect();
                for (rect, cells, k, side) in ghosts {
                    let full = px(&rect);
                    let r = if self.layers_conf.open == crate::config::LayerOpen::Unroll { crate::layers::unrolled(full, side, k) } else { full };
                    // its text stays put, cut by the box as that draws in
                    let s = 1.0;
                    let (cx, cy) = (full[0] + full[2] / 2.0, full[1] + full[3] / 2.0);
                    over.extend(clipped(layer_frame(r, ch, p.frame.fg.f32(), k, shadow_k), clip));
                    fx_floats.push((r, k));
                    fx_cell = ch;
                    // its backgrounds as they were drawn: the window's under the cells without their
                    // own, each cell's own over it (one colour for all of it flashed the highlight's)
                    let o = self.opacity;
                    let base = { let b = p.frame.bg.f32(); [b[0] * o, b[1] * o, b[2] * o] };
                    veil.push(Rect { xywh: r, color: [base[0], base[1], base[2], k], p: [0.0, 0.0], hole: NO_HOLE });
                    for (col, row, _, _, bg, _) in &cells {
                        let Some(b) = bg else { continue };
                        let (x, y) = (ox + (rect[0] + col) as f32 * cw, oy + (rect[1] + row) as f32 * ch);
                        let cut = [x.max(r[0]), y.max(r[1]), (x + cw).min(r[0] + r[2]), (y + ch).min(r[1] + r[3])];
                        if cut[2] - cut[0] > 0.5 && cut[3] - cut[1] > 0.5 {
                            over.push(Rect { xywh: [cut[0], cut[1], cut[2] - cut[0], cut[3] - cut[1]], color: [b.0 as f32 / 255.0, b.1 as f32 / 255.0, b.2 as f32 / 255.0, k], p: [0.0, 0.0], hole: NO_HOLE });
                        }
                    }
                    for (col, row, text, fg, _, flags) in cells.iter().filter(|c| !c.2.trim().is_empty()) {
                        let x = cx + (ox + *col as f32 * cw + rect[0] as f32 * cw - cx) * s;
                        let y = cy + (oy + *row as f32 * ch + rect[1] as f32 * ch - cy) * s;
                        let mut b = Buffer::new(&mut self.font_system, Metrics::new(self.font_px, ch));
                        b.set_size(None, Some(ch));
                        let mut at = Attrs::new().family(Family::Name(&self.family)).color(Color::rgba(fg.0, fg.1, fg.2, (k * 255.0) as u8)).font_features(self.features.clone());
                        if flags & 1 != 0 {
                            at = at.weight(Weight::BOLD);
                        }
                        if flags & 2 != 0 {
                            at = at.style(Style::Italic);
                        }
                        b.set_text(text, &at, Shaping::Advanced, None);
                        b.shape_until_scroll(&mut self.font_system, false);
                        self.unders.push((b, x, y, [r[0], r[1], r[0] + r[2], r[1] + r[3]]));
                    }
                }
            }
            if !spans.is_empty() {
                let d = self.fx.draw(&mut self.font_system, &self.family, self.font_px, self.cell_w, self.cell_h, p.frame, &spans, ox, oy, fx_now);
                fx_moving |= d.moving;
                match float_rect {
                    Some(fr) => rects.extend(split_by_float(d.rects, clip, fr)),
                    None => rects.extend(clipped(d.rects, clip)),
                }
                fx_glyphs.push((pi, d.glyphs));
            }
            over.extend(moved(self.overlay_list(p.frame, ox, oy, clip), moves[pi]));
            // pen strokes over the text (ink.rs), and where the pen hovers while drawing
            if !p.frame.ink.is_empty() {
                over.extend(clipped(crate::ink::dots(&p.frame.ink, p.frame.ink_top, p.frame.rows.len() as f64, ox, oy, self.cell_w, self.cell_h), clip));
            }
            if let Some((line, col, rad, eraser)) = p.frame.pen_hover {
                let (x, y) = (ox + col * self.cell_w, oy + (line - p.frame.ink_top) as f32 * self.cell_h);
                let r = (rad * self.cell_h).max(2.0);
                let mut c = p.frame.cursor_color.f32();
                c[3] = if eraser { 0.25 } else { 0.55 };
                over.push(Rect { xywh: [x - r, y - r, 2.0 * r, 2.0 * r], color: c, p: [r, 0.9], hole: NO_HOLE });
                if eraser {
                    let f = p.frame.fg.f32();
                    over.push(Rect { xywh: [x - 1.5, y - 1.5, 3.0, 3.0], color: [f[0], f[1], f[2], 0.9], p: [1.5, 0.5], hole: NO_HOLE });
                }
            }
        }
        if panes.len() > 1 {
            let line = { let mut c = frame.fg.f32(); c[3] = 0.16; c };
            let shade = { let mut c = frame.bg.f32(); c[3] = 0.32; c };
            for p in panes {
                // a hairline where a pane meets its left or upper neighbour
                if p.x > 0.5 {
                    over.push(Rect { xywh: [p.x.round() - 1.0, p.y, 1.0, p.h], color: line, p: [0.0, 0.0], hole: NO_HOLE });
                }
                if p.y > self.top + 0.5 {
                    over.push(Rect { xywh: [p.x, p.y.round() - 1.0, p.w, 1.0], color: line, p: [0.0, 0.0], hole: NO_HOLE });
                }
                // panes without the keyboard step back a little
                if !p.focused {
                    over.push(Rect { xywh: [p.x, p.y, p.w, p.h], color: shade, p: [0.0, 0.0], hole: NO_HOLE });
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
            // from the frame's right edge (a docked frame reaches past its last cell to the window's)
            let bx = r[0] + r[2] - ZFLOAT_BUTTONS * self.cell_w + self.cell_w * 0.25;
            for (k, glyph) in [(1.0, "⌄"), (3.0, "–"), (5.0, "×")].into_iter().filter(|_| z.buttons) {
                let cx = bx + (k + 0.5) * self.cell_w;
                let cy = r[1] + self.cell_h * 0.5;
                // under the pointer: a little larger and stronger
                let hot = self.pointer.is_some_and(|(px, py)| (px - cx).hypot(py - cy) < self.cell_h * 0.6);
                let d = self.cell_h * if hot { 0.86 } else { 0.74 };
                let col = if glyph == "×" { let mut c = fail.f32(); c[3] = if hot { 1.0 } else if z.focus { 0.85 } else { 0.35 }; c } else { let mut c = fg.f32(); c[3] = if hot { 0.30 } else { 0.12 }; c };
                over.push(Rect { xywh: [cx - d / 2.0, cy - d / 2.0, d, d], color: col, p: [d / 2.0, 0.0], hole: NO_HOLE });
                let (b, w) = label(&mut self.font_system, &self.family, glyph, if glyph == "×" { Rgb(255, 255, 255) } else { *fg }, self.font_px * 0.9, self.cell_h);
                // centred by what the glyph draws, not by its line: ⌄ sits on the baseline, – on the
                // middle, × on the maths axis, and a font of another shape moves each its own way
                let (x, y) = match ink_center(&mut self.font_system, &mut self.swash, &b) {
                    Some((ix, iy)) => (cx - ix, cy - iy),
                    None => (cx - w / 2.0, r[1]),
                };
                labels.push((b, x, y));
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
                    over.push(Rect { xywh: [x0 + cw * 0.3, y + ch - 2.0, (x1 - x0 - cw * 0.6) * pr, 2.0], color: c, p: [0.0, 0.0], hole: NO_HOLE });
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
                    over.push(Rect { xywh: [x + 2.0, h * 0.12, tw - 4.0, h * 0.76], color: frame.bar_bg.f32(), p: [0.0, 0.0], hole: NO_HOLE });
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
        // (a popup with nothing in it is not drawn: it showed as an empty bar over other panes)
        self.popup_hit = None;
        if let (Some(pp), Some((cr, cc, _))) = (frame.popup.as_ref().filter(|p| !p.items.is_empty()), frame.cursor) {
            let cr = pp.row.unwrap_or(cr);
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
            // items with \x02 in front are the second column (the commands Claude Code ran): beside
            // the first when both fit in the pane, else in the one list, marked
            let second: Vec<usize> = (0..n).filter(|i| pp.items[*i].starts_with('\u{2}')).collect();
            let first_col: Vec<usize> = (0..n).filter(|i| !pp.items[*i].starts_with('\u{2}')).collect();
            let text = |i: usize| -> String { pp.items[i].strip_prefix('\u{2}').unwrap_or(&pp.items[i]).to_string() };
            let wide_of = |ix: &[usize]| ix.iter().map(|i| text(*i).chars().count()).max().unwrap_or(0);
            let pane_cells = ((right - left) / self.cell_w) as usize;
            let _ = wide_of;
            // two columns from 60 cells on, each at most half the pane (long commands end in …)
            let two = !second.is_empty() && !first_col.is_empty() && pane_cells >= 60;
            let cap = if two { (pane_cells.saturating_sub(8) / 2).max(20) } else { pane_cells.saturating_sub(4).max(20) };
            // the columns: (title, items); one column holds everything when there is no room for two
            let cols: Vec<(Option<&str>, Vec<usize>)> = if two { vec![(None, first_col.clone()), (Some("✦ Claude"), second.clone())] } else { vec![(None, (0..n).collect())] };
            let label_of = |i: usize| -> String {
                let s = if !two && pp.items[i].starts_with('\u{2}') { format!("✦ {}", text(i)) } else { text(i) };
                if s.chars().count() > cap { format!("{}…", s.chars().take(cap - 1).collect::<String>()) } else { s }
            };
            let mut show: usize = ((room / row_h) as usize).clamp(1, look.rows.max(1));
            let longest = cols.iter().map(|c| c.1.len()).max().unwrap_or(0);
            let heads = if two { 1 } else { 0 };
            show = show.saturating_sub(heads).max(1);
            if longest > show {
                // the footer ("3 of 40 · Tab next") takes a row too
                show = show.saturating_sub(1).max(1);
            }
            // each column scrolls so its chosen item is in view
            let visible: Vec<Vec<usize>> = cols.iter().map(|(_, ix)| {
                let at = pp.sel.and_then(|s| ix.iter().position(|i| *i == s));
                let first = match at { Some(s) if s >= show => s + 1 - show, _ => 0 };
                ix.iter().skip(first).take(show).copied().collect()
            }).collect();
            let col_w: Vec<f32> = visible.iter().zip(&cols).map(|(v, (t, _))| {
                let w = v.iter().map(|i| label_of(*i).chars().count()).max().unwrap_or(0).max(t.map(|t| t.chars().count()).unwrap_or(0));
                (w.max(12) + 3) as f32 * self.cell_w
            }).collect();
            let more = longest > show;
            let footer_w = if more { 34.0 * self.cell_w } else { 0.0 };
            let gap = self.cell_w;
            let pw = (col_w.iter().sum::<f32>() + gap * (col_w.len() as f32 - 1.0)).max(footer_w + 3.0 * self.cell_w).min(width as f32 - 2.0 * self.pad);
            let rows_n = visible.iter().map(|v| v.len()).max().unwrap_or(0);
            // the chosen item whole when its row cuts it (long commands): wrapped under the list
            let full_lines: Vec<String> = match pp.sel {
                Some(s) if s < n && label_of(s).ends_with('…') => {
                    let per = ((pw / self.cell_w) as usize).saturating_sub(3).max(10);
                    let chars: Vec<char> = text(s).chars().collect();
                    let mut v: Vec<String> = chars.chunks(per).map(|c| c.iter().collect()).collect();
                    if v.len() > 4 {
                        v.truncate(4);
                        v[3].push('…');
                    }
                    v
                }
                _ => Vec::new(),
            };
            let full_h = if full_lines.is_empty() { 0.0 } else { full_lines.len() as f32 * self.cell_h + self.cell_h * 0.6 };
            let ph = (rows_n + heads) as f32 * row_h + self.cell_h * 0.5 + if more { row_h } else { 0.0 } + full_h;
            let wx = fox + pp.col.unwrap_or_else(|| cc.saturating_sub(pp.word)) as f32 * self.cell_w - self.cell_w;
            let x = wx.clamp(left.min(width as f32 - pw - self.pad).max(self.pad), (right.max(left + pw) - pw).min(width as f32 - pw - self.pad).max(self.pad));
            let below = line_y + self.cell_h + 2.0;
            let y = if below + ph <= bottom || below_px >= above_px { below } else { (line_y - ph - 4.0).max(self.top) };
            let r = look.radius;
            if look.shadow {
                // a soft shadow, a little lower than the popup
                let s = 14.0;
                over.push(Rect { xywh: [x - s, y - s + 4.0, pw + 2.0 * s, ph + 2.0 * s], color: [0.0, 0.0, 0.0, 0.38], p: [r, s], hole: NO_HOLE });
            }
            if look.blur > 0.0 {
                backdrop = Some(([x, y, pw, ph], r, look.blur));
            }
            if look.border {
                let e = frame.fg.f32();
                over.push(Rect { xywh: [x - 1.0, y - 1.0, pw + 2.0, ph + 2.0], color: [e[0], e[1], e[2], 0.16], p: [r + 1.0, 0.0], hole: NO_HOLE });
            }
            let mut bg = frame.bar_bg.f32();
            bg[3] = look.opacity;
            over.push(Rect { xywh: [x, y, pw, ph], color: bg, p: [r, 0.0], hole: NO_HOLE });
            let dim = dim6(frame.fg);
            let mut hits: Vec<(usize, [f32; 4])> = Vec::new();
            let mut cx = x;
            for (c, ((title, _), vis)) in cols.iter().zip(&visible).enumerate() {
                let cw = col_w[c];
                if c > 0 {
                    // a hairline between the columns
                    let e = frame.fg.f32();
                    over.push(Rect { xywh: [cx - gap / 2.0, y + 6.0, 1.0, ph - full_h - 12.0], color: [e[0], e[1], e[2], 0.12], p: [0.0, 0.0], hole: NO_HOLE });
                }
                if let Some(t) = title {
                    let (b, _) = label(&mut self.font_system, &self.family, t, dim, self.font_px * 0.85, self.cell_h);
                    labels.push((b, cx + self.cell_w, y + self.cell_h * 0.25 + (row_h - self.cell_h) / 2.0));
                }
                for (k, i) in vis.iter().enumerate() {
                    let ry = y + self.cell_h * 0.25 + (k + heads) as f32 * row_h;
                    hits.push((*i, [cx, ry, cw, row_h]));
                    let chosen = pp.sel == Some(*i);
                    if chosen {
                        let cl = frame.cursor_color.f32();
                        over.push(Rect { xywh: [cx + 4.0, ry, cw - 8.0, row_h], color: [cl[0], cl[1], cl[2], 0.30], p: [(r * 0.6).max(2.0), 0.0], hole: NO_HOLE });
                    }
                    let (b, _) = label(&mut self.font_system, &self.family, &label_of(*i), if chosen || pp.sel.is_none() { frame.fg } else { dim }, self.font_px, self.cell_h);
                    labels.push((b, cx + self.cell_w, ry + (row_h - self.cell_h) / 2.0));
                }
                cx += cw + gap;
            }
            if more {
                let ry = y + self.cell_h * 0.25 + (rows_n + heads) as f32 * row_h;
                let t = format!("{} of {n}  ·  Tab next, Shift+Tab back", pp.sel.map(|s| s + 1).unwrap_or(0));
                let (b, _) = label(&mut self.font_system, &self.family, &t, dim, self.font_px * 0.85, self.cell_h);
                labels.push((b, x + self.cell_w, ry + (row_h - self.cell_h) / 2.0));
            }
            if !full_lines.is_empty() {
                // a hairline, then the chosen command whole
                let top = y + ph - full_h;
                let e = frame.fg.f32();
                over.push(Rect { xywh: [x + 8.0, top, pw - 16.0, 1.0], color: [e[0], e[1], e[2], 0.12], p: [0.0, 0.0], hole: NO_HOLE });
                for (k, l) in full_lines.iter().enumerate() {
                    let (b, _) = label(&mut self.font_system, &self.family, l, frame.fg, self.font_px, self.cell_h);
                    labels.push((b, x + self.cell_w, top + self.cell_h * 0.3 + k as f32 * self.cell_h));
                }
            }
            self.popup_hit = Some(([x, y, pw, ph], pp.sel, hits));
        }
        if let Some(txt) = &frame.bar {
            let (b, w) = label(&mut self.font_system, &self.family, txt, frame.fg, self.font_px, self.cell_h);
            let (bw, bh) = (w + 2.0 * self.cell_w, self.cell_h * 1.5);
            // bottom right of the focused pane: found lines are scrolled to the top, so the bar never covers them
            let (x, y) = ((focus.x + focus.w - bw - self.cell_w).max(focus.x), focus.y + focus.h - bh - self.cell_h * 0.5);
            over.push(Rect { xywh: [x, y, bw, bh], color: frame.bar_bg.f32(), p: [0.0, 0.0], hole: NO_HOLE });
            labels.push((b, x + self.cell_w, y + self.cell_h * 0.25));
        }
        self.bars = labels;
        // big text no longer shown is dropped
        if !self.ui_text.is_empty() {
            let used: std::collections::HashSet<u64> = self.ui_placed.iter().map(|u| u.0).collect();
            self.ui_text.retain(|k, _| used.contains(k));
        }
        // floats coming, going or moving: frames until they are still (layers.rs)
        {
            let dur = std::time::Duration::from_millis(self.layers_conf.ms as u64);
            let t = std::time::Instant::now();
            self.layer_anim.prune(t, dur);
            animating |= self.layers_conf.animate && self.layer_anim.busy(t, dur);
        }
        self.overlay.upload(device, queue, &over, width as f32, height as f32);
        self.veil.upload(device, queue, &veil, width as f32, height as f32);
        self.viewport.update(queue, Resolution { width, height });
        let ch = self.cell_h;
        let mut areas: Vec<TextArea> = Vec::new();
        for (pi, p) in panes.iter().enumerate() {
            let (Some(cache), Some(order)) = (self.rows.get(&p.id), self.order.get(&p.id)) else { continue };
            let (ox, oy) = self.origin(p.x, p.y, p.w, p.h);
            // each pane's text stays inside its pane (and out of the tab bar); a gliding region's rows inside the region
            let pane_top = p.y.max(self.top);
            let places = placed(p.frame, oy, ch);
            let area = |b, left: f32, top: f32, bounds: [f32; 4]| TextArea {
                buffer: b,
                left,
                top,
                scale: 1.0,
                bounds: TextBounds { left: bounds[0] as i32, top: bounds[1] as i32, right: bounds[2] as i32, bottom: bounds[3] as i32 },
                default_color: Color::rgb(255, 255, 255),
                custom_glyphs: &[],
            };
            for (segs, y, band) in order.iter().zip(places).filter_map(|(key, (_, y, band))| cache.get(key).map(|b| (b, y, band))) {
            for (sx, b) in segs {
                let ox = ox + sx;
                let (t, bo) = band.map(|(b0, b1)| (b0.max(pane_top), b1.min(p.y + p.h))).unwrap_or((pane_top, p.y + p.h));
                let all = [p.x, t, p.x + p.w, bo];
                match moves[pi].filter(|(r, _, _)| y + ch > r[1] && y < r[1] + r[3]) {
                    // a row through a dragged zfloat: beside it as it is, its part inside it moved with it
                    Some((r, dx, dy)) => {
                        areas.push(area(b, ox, y, [all[0], all[1], r[0], all[3]]));
                        areas.push(area(b, ox, y, [r[0] + r[2], all[1], all[2], all[3]]));
                        let inside = [r[0] + dx, r[1] + dy, r[0] + r[2] + dx, r[1] + r[3] + dy];
                        areas.push(area(b, ox + dx, y + dy, [inside[0].max(all[0]), inside[1].max(pane_top), inside[2].min(all[2]), inside[3].min(p.y + p.h)]));
                    }
                    None => areas.push(area(b, ox, y, all)),
                }
            }
            }
            // characters an effect draws: each in its colour this frame, clipped as its row
            let fx = &self.fx;
            for (_, glyphs) in fx_glyphs.iter().filter(|(i, _)| *i == pi) {
                areas.extend(glyphs.iter().filter_map(|g| fx.buffer(&g.key).map(|b| {
                    let (t, bo) = g.band.map(|(b0, b1)| (b0.max(pane_top), b1.min(p.y + p.h))).unwrap_or((pane_top, p.y + p.h));
                    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                    TextArea {
                        buffer: b,
                        left: g.x,
                        top: g.y,
                        scale: 1.0,
                        bounds: TextBounds { left: p.x as i32, top: t as i32, right: (p.x + p.w) as i32, bottom: bo as i32 },
                        default_color: Color::rgba(c(g.color[0]), c(g.color[1]), c(g.color[2]), c(g.color[3])),
                        custom_glyphs: &[],
                    }
                })));
            }
        }
        // UI hints' big text, over the cells it stands for
        areas.extend(self.ui_placed.iter().filter_map(|(k, x, y, c)| self.ui_text.get(k).map(|b| TextArea {
            buffer: b,
            left: *x,
            top: *y,
            scale: 1.0,
            bounds: TextBounds { left: c[0] as i32, top: c[1] as i32, right: c[2].ceil() as i32, bottom: c[3].ceil() as i32 },
            default_color: Color::rgb(255, 255, 255),
            custom_glyphs: &[],
        })));
        let _ = self.text.prepare(device, queue, &mut self.font_system, &mut self.atlas, &self.viewport, areas, &mut self.swash);
        let bar_areas: Vec<TextArea> = self.bars.iter().map(|(b, x, y)| TextArea {
            buffer: b,
            left: *x,
            top: *y,
            scale: 1.0,
            bounds: TextBounds { left: 0, top: 0, right: width as i32, bottom: height as i32 },
            default_color: Color::rgb(255, 255, 255),
            custom_glyphs: &[],
        }).chain(self.unders.iter().map(|(b, x, y, c)| TextArea {
            buffer: b,
            left: *x,
            top: *y,
            scale: 1.0,
            bounds: TextBounds { left: c[0] as i32, top: c[1] as i32, right: c[2].ceil() as i32, bottom: c[3].ceil() as i32 },
            default_color: Color::rgb(255, 255, 255),
            custom_glyphs: &[],
        })).collect();
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
            // floats whose shadows a shader draws: the frame offscreen, shown, then the shader over
            // the shadows reading it
            None if fx_on && !fx_floats.is_empty() => {
                let blur = self.blur.get_or_insert_with(|| Blur::new(device, self.format));
                blur.textures(device, width, height);
                let (_, _, _, scene, _, plain) = blur.tex.as_ref().unwrap();
                // the frame without its text too: where the two differ is text (zText), what is
                // under a letter the shader can see (zPlain)
                {
                    let mut p = pass(&mut enc, plain, wgpu::LoadOp::Clear(clear));
                    draw_rects(&self.rects, &self.shaders, &self.panel_shader, &mut p);
                    self.images.draw(&mut p, width, height);
                    self.veil.draw(&mut p);
                }
                {
                    let mut p = pass(&mut enc, scene, wgpu::LoadOp::Clear(clear));
                    draw_rects(&self.rects, &self.shaders, &self.panel_shader, &mut p);
                    self.images.draw(&mut p, width, height);
                    let _ = self.text.render(&self.atlas, &self.viewport, &mut p);
                    self.veil.draw(&mut p);
                }
                let to_screen = blur.bind(device, scene, 3);
                fullscreen(&mut enc, view, &blur.blit, &to_screen, wgpu::LoadOp::Clear(clear), None);
                // where shadows can fall: the floats and the reach of their shadows
                let m = fx_cell * 3.0;
                let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, 0.0f32, 0.0f32);
                for (r, _) in &fx_floats {
                    x0 = x0.min(r[0] - m);
                    y0 = y0.min(r[1] - m);
                    x1 = x1.max(r[0] + r[2] + m);
                    y1 = y1.max(r[1] + r[3] + m);
                }
                let (w, h) = (width as f32, height as f32);
                let (sx, sy) = (x0.clamp(0.0, w) as u32, y0.clamp(0.0, h) as u32);
                let scissor = [sx, sy, (x1.clamp(0.0, w) as u32).saturating_sub(sx), (y1.clamp(0.0, h) as u32).saturating_sub(sy)];
                let o = self.opacity;
                let bgp = [bg[0] * o, bg[1] * o, bg[2] * o, o];
                if let Some(fx) = self.layer_fx.as_ref() {
                    fx.draw(device, queue, &mut enc, view, scene, plain, &fx_floats, [w, h], self.layer_fx_t0.elapsed().as_secs_f32(), bgp, fx_cell, self.layers_conf.shadow, scissor);
                }
                let mut p = pass(&mut enc, view, wgpu::LoadOp::Load);
                self.overlay.draw(&mut p);
                let _ = self.text2.render(&self.atlas, &self.viewport, &mut p);
            }
            None => {
                // the usual frame: one pass
                let mut p = pass(&mut enc, view, wgpu::LoadOp::Clear(clear));
                draw_rects(&self.rects, &self.shaders, &self.panel_shader, &mut p);
                self.images.draw(&mut p, width, height);
                let _ = self.text.render(&self.atlas, &self.viewport, &mut p);
                self.veil.draw(&mut p);
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
                    draw_rects(&self.rects, &self.shaders, &self.panel_shader, &mut p);
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
                self.veil.draw(&mut p);
                self.overlay.draw(&mut p);
                let _ = self.text2.render(&self.atlas, &self.viewport, &mut p);
            }
        }
        // the shader moves on its own while a float shows
        let animating = animating || (fx_on && !fx_floats.is_empty());
        queue.submit(Some(enc.finish()));
        self.atlas.trim();
        self.fx.frame_done(fx_moving, fx_start.elapsed());
        animating
    }

    fn cursor_motion(&mut self, frame: &Frame, ox: f32, oy: f32) -> (Option<((f32, f32), (f32, f32))>, bool) {
        let target = frame.cursor.map(|(r, c, _)| (ox + c as f32 * self.cell_w, oy + r as f32 * self.cell_h));
        // zash's glide only where its look applies ([cursor] scope); elsewhere the cursor jumps
        let (Some(t), true) = (target, self.cursor_ms > 0.0 && frame.cursor_styled) else {
            self.anim = None;
            return (None, false);
        };
        let now = std::time::Instant::now();
        let a = self.anim.get_or_insert(CursorAnim { head: t, tail: t, target: t, last: now });
        // the zfloat moved (dragged, or zeit moved it): the cursor goes with it, no glide; only its own
        // moves inside it glide
        let fz = frame.float.as_ref().map(|z| (z.id, (ox + z.x as f32 * self.cell_w, oy + z.y as f32 * self.cell_h)));
        let size = frame.float.as_ref().map(|z| (z.w as f32 * self.cell_w, z.h as f32 * self.cell_h)).unwrap_or_default();
        if let (Some((id, p)), Some((id0, p0))) = (fz, self.float_at) {
            // (a cursor in the panes below stays where it is)
            let inside = t.0 >= p.0 && t.0 < p.0 + size.0 && t.1 >= p.1 && t.1 < p.1 + size.1;
            if id == id0 && p != p0 && inside {
                let d = (p.0 - p0.0, p.1 - p0.1);
                for q in [&mut a.head, &mut a.tail, &mut a.target] {
                    q.0 += d.0;
                    q.1 += d.1;
                }
            }
        }
        self.float_at = fz;
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
    // "its own background" is one that differs from the default in force, not from the palette's: after
    // an OSC 11 every default cell drew a box, which hid what is drawn over it (rounded pill ends)
    let def_bg = resolve(AColor::Named(NamedColor::Background), colors, p, false);
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
        bg: (bg != def_bg).then_some(bg),
        underline: f.intersects(Flags::ALL_UNDERLINES),
        strike: f.contains(Flags::STRIKEOUT),
        wide: f.contains(Flags::WIDE_CHAR),
        spacer: f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
        fx: cell.hyperlink().filter(|h| h.uri().starts_with(crate::fx::SCHEME)),
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
            (x1 > x0 && y1 > y0).then_some(Rect { xywh: [x0, y0, x1 - x0, y1 - y0], color: r.color, p: r.p, hole: r.hole })
        })
        .collect()
}

/// Every row to draw, top to bottom, with its y and (for a gliding region) the band it
/// is cut to: rows above the screen, the screen, rows below it, then a region's old rows.
pub(crate) fn placed(frame: &Frame, oy: f32, ch: f32) -> Vec<(&Vec<CellView>, f32, Option<(f32, f32)>)> {
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
    /// The pane's pen strokes that show, given the scroll position.
    /// The pen's strokes on this screen; `float`: where the zfloat is now (its strokes go with it).
    pub fn with_ink<T: alacritty_terminal::event::EventListener>(mut self, term: &Term<T>, ink: &crate::ink::Ink, float: crate::ink::FloatAt, panes: &[crate::ink::ZPane]) -> Frame {
        if !ink.is_empty() {
            self.ink_top = term.grid().history_size() as f64 - term.grid().display_offset() as f64;
            self.ink = ink.shown(self.ink_top, self.rows.len() as f64, float, panes);
        }
        self
    }

    /// The cells under a dragged zfloat's edge, in this palette's colours.
    pub fn with_under(mut self, g: &crate::graphics::Graphics, p: &Palette) -> Frame {
        use crate::graphics::UColor;
        self.under = g.under.as_ref().map(|u| {
            let c = |c: UColor| match c { UColor::Default => None, UColor::Index(i) => Some(p.indexed(i)), UColor::Rgb(r) => Some(r) };
            (u.id, u.cells.iter().map(|(x, y, t, fg, bg, fl)| {
                let (f, b) = (c(*fg).unwrap_or(self.fg), c(*bg));
                // reverse video: its colours swapped, as the pane draws it
                let (f, b) = if fl & 8 != 0 { (b.unwrap_or(self.bg), Some(f)) } else { (f, b) };
                (u.x + x, u.y + y, t.clone(), f, b, *fl)
            }).collect())
        });
        self
    }

    pub fn with_images<T: alacritty_terminal::event::EventListener>(mut self, term: &Term<T>, g: &crate::graphics::Graphics) -> Frame {
        // floats only on the live screen (scrolled back, the program's screen is not what shows)
        if term.grid().display_offset() == 0 && term.mode().contains(alacritty_terminal::term::TermMode::ALT_SCREEN) {
            self.layers = g.layers.clone();
        }
        // UI hints on the screen they were sent on, where their lines are now; big text hides its cells
        if let Some(u) = g.ui.as_ref().filter(|u| u.alt == term.mode().contains(alacritty_terminal::term::TermMode::ALT_SCREEN)) {
            let shift = term.grid().history_size() as isize - u.top as isize - term.grid().display_offset() as isize;
            let rows = self.rows.len() as isize;
            for r in &u.regions {
                let y = r.rect[1] as isize - shift;
                if y + r.rect[3] as isize <= 0 || y >= rows {
                    continue;
                }
                if matches!(r.kind, crate::ui::Kind::Text(_) | crate::ui::Kind::Pill { label: Some(_), .. } | crate::ui::Kind::Button { label: Some(_), .. } | crate::ui::Kind::Input { label: Some(_), .. } | crate::ui::Kind::Progress { .. }) {
                    for row in self.rows.iter_mut().skip(y.max(0) as usize).take((y + r.rect[3] as isize - y.max(0)) as usize) {
                        for c in row.iter_mut().skip(r.rect[0]).take(r.rect[2]) {
                            c.ch = ' ';
                            c.zw = None;
                        }
                    }
                }
                self.ui.push((r.clone(), y));
            }
        }
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
            } else if let Some(a) = g.anims.get(&p.img) {
                let frame = a.frame.lock().unwrap().clone();
                if let Some(img) = frame {
                    let generation = a.generation.load(std::sync::atomic::Ordering::Relaxed);
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

/// The rectangles under the text, with a zfloat's shader panel drawn in their midst (between
/// its frame and the cells inside it). Fields, not the renderer: the blur stage borrows it.
fn draw_rects(rects: &Rects, shaders: &crate::panel_shader::Shaders, panel: &Option<(std::path::PathBuf, u32)>, pass: &mut wgpu::RenderPass) {
    match panel.as_ref().and_then(|(path, at)| shaders.loaded(path).map(|s| (s, *at))) {
        Some((shader, at)) => {
            rects.draw_range(pass, 0, at);
            shader.draw(pass);
            rects.draw_range(pass, at, u32::MAX);
        }
        None => rects.draw(pass),
    }
}

#[cfg(test)]
mod cursor_tests {
    use crate::config::{CursorConf, CursorScope, Palette};
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::{test::TermSize, Config, Term};
    use alacritty_terminal::vte::ansi::{CursorShape, Processor};

    fn frame(bytes: &[u8]) -> super::Frame {
        let mut t = Term::new(Config::default(), &TermSize::new(20, 5), VoidListener);
        let mut p: Processor = Processor::new();
        p.advance(&mut t, bytes);
        super::Frame::extract(&t, &Palette::default())
    }

    #[test]
    fn a_tui_drawn_cursor_is_followed_and_a_menu_row_is_not() {
        // real cursor hidden; a highlighted menu row (many inverted cells), then "> ab" with one inverted cell
        let mut f = frame(b"\x1b[?25l\x1b[7mmenu item\x1b[0m\r\n> ab\x1b[7m \x1b[0m");
        assert_eq!(f.drawn_cursor, Some((1, 4)));
        f.apply_cursor(&CursorConf::default(), Palette::default().cursor_fg);
        assert!(f.cursor_drawn);
        assert_eq!(f.cursor.map(|(r, c, _)| (r, c)), Some((1, 4)));
        // switched off: no cursor at all, as before
        let mut g = frame(b"\x1b[?25l> ab\x1b[7m \x1b[0m");
        g.apply_cursor(&CursorConf { drawn: false, ..Default::default() }, Palette::default().cursor_fg);
        assert!(g.cursor.is_none());
    }

    #[test]
    fn style_and_scope() {
        let beam = CursorConf { style: Some(CursorShape::Beam), ..Default::default() };
        let mut f = frame(b"abc");
        f.apply_cursor(&beam, Palette::default().cursor_fg);
        assert_eq!(f.cursor.map(|c| c.2), Some(CursorShape::Beam));
        // "zfloat" scope with the cursor outside any zfloat: the program's own shape, no glide
        let mut g = frame(b"abc");
        g.apply_cursor(&CursorConf { scope: CursorScope::Zfloat, ..beam }, Palette::default().cursor_fg);
        assert!(!g.cursor_styled);
        assert_eq!(g.cursor.map(|c| c.2), Some(CursorShape::Block));
        assert!(CursorConf::shape("wobbly").is_err());
    }
}

/// A dragged zfloat's rects (cells, cursor, lines) drawn as far from their cells as it is.
fn moved(mut rects: Vec<Rect>, m: Option<([f32; 4], f32, f32)>) -> Vec<Rect> {
    if let Some((r, dx, dy)) = m {
        for q in rects.iter_mut() {
            let (cx, cy) = (q.xywh[0] + q.xywh[2] / 2.0, q.xywh[1] + q.xywh[3] / 2.0);
            if cx >= r[0] && cx < r[0] + r[2] && cy >= r[1] && cy < r[1] + r[3] {
                q.xywh[0] += dx;
                q.xywh[1] += dy;
            }
        }
    }
    rects
}

/// The middle of what a laid-out buffer actually draws (its glyphs' ink), from its top-left corner.
fn ink_center(fs: &mut FontSystem, swash: &mut SwashCache, b: &Buffer) -> Option<(f32, f32)> {
    let (mut l, mut t, mut r, mut bt) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for run in b.layout_runs() {
        for g in run.glyphs {
            let p = g.physical((0.0, 0.0), 1.0);
            let Some(img) = swash.get_image_uncached(fs, p.cache_key) else { continue };
            let pl = img.placement;
            if pl.width == 0 || pl.height == 0 {
                continue;
            }
            let x = p.x as f32 + pl.left as f32;
            let y = run.line_y + p.y as f32 - pl.top as f32;
            l = l.min(x);
            r = r.max(x + pl.width as f32);
            t = t.min(y);
            bt = bt.max(y + pl.height as f32);
        }
    }
    (l < r && t < bt).then(|| ((l + r) / 2.0, (t + bt) / 2.0))
}

/// Drawn marks (pen dots) over a pane with a zfloat: a dot inside the zfloat is cut at its edges,
/// one outside is cut away where the zfloat covers it (the four bands around it), all within `clip`.
fn split_by_float(rects: Vec<Rect>, clip: [f32; 4], f: [f32; 4]) -> Vec<Rect> {
    let inside = |r: &Rect| {
        let (cx, cy) = (r.xywh[0] + r.xywh[2] / 2.0, r.xywh[1] + r.xywh[3] / 2.0);
        cx >= f[0] && cx < f[0] + f[2] && cy >= f[1] && cy < f[1] + f[3]
    };
    let (ins, outs): (Vec<Rect>, Vec<Rect>) = rects.into_iter().partition(inside);
    let fc = clipped_rect(f, clip);
    let mut out = clipped(ins, fc);
    let (cx0, cy0, cx1, cy1) = (clip[0], clip[1], clip[0] + clip[2], clip[1] + clip[3]);
    let (fx0, fy0, fx1, fy1) = (f[0], f[1], f[0] + f[2], f[1] + f[3]);
    // above, below, left of, right of the zfloat
    let bands = [
        [cx0, cy0, clip[2], (fy0 - cy0).max(0.0)],
        [cx0, fy1, clip[2], (cy1 - fy1).max(0.0)],
        [cx0, fy0, (fx0 - cx0).max(0.0), f[3]],
        [fx1, fy0, (cx1 - fx1).max(0.0), f[3]],
    ];
    for b in bands.iter().filter(|b| b[2] > 0.0 && b[3] > 0.0) {
        out.extend(clipped(outs.clone(), clipped_rect(*b, clip)));
    }
    out
}

/// `r` cut to `clip` (an empty one when they do not meet).
fn clipped_rect(r: [f32; 4], clip: [f32; 4]) -> [f32; 4] {
    let x0 = r[0].max(clip[0]);
    let y0 = r[1].max(clip[1]);
    let x1 = (r[0] + r[2]).min(clip[0] + clip[2]);
    let y1 = (r[1] + r[3]).min(clip[1] + clip[3]);
    [x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0)]
}

/// A float's cells (inside it): for its fade-out once it is gone.
fn layer_cells(rows: &[Vec<CellView>], r: &[usize; 4]) -> Vec<crate::layers::GhostCell> {
    let mut out = Vec::new();
    for (y, row) in rows.iter().enumerate().skip(r[1]).take(r[3]) {
        for (x, c) in row.iter().enumerate().skip(r[0]).take(r[2]) {
            if c.spacer {
                continue;
            }
            let mut text = c.ch.to_string();
            if let Some(z) = &c.zw {
                text.extend(z.iter());
            }
            let flags = u8::from(c.style.bold) | (u8::from(c.style.italic) << 1);
            out.push((x - r[0], y - r[1], text, c.style.fg, c.bg.map(|b| (b.0, b.1, b.2)), flags));
        }
    }
    out
}

/// The parts of `r` outside `inner` (which lies inside it): up to four strips.
fn outside(r: [f32; 4], inner: [f32; 4]) -> Vec<[f32; 4]> {
    let (x0, y0, x1, y1) = (r[0], r[1], r[0] + r[2], r[1] + r[3]);
    let (ix0, iy0, ix1, iy1) = (inner[0].max(x0), inner[1].max(y0), (inner[0] + inner[2]).min(x1), (inner[1] + inner[3]).min(y1));
    [[x0, y0, r[2], iy0 - y0], [x0, iy1, r[2], y1 - iy1], [x0, iy0, ix0 - x0, iy1 - iy0], [ix1, iy0, x1 - ix1, iy1 - iy0]]
        .into_iter().filter(|s| s[2] > 0.5 && s[3] > 0.5).collect()
}

/// A float's frame, as Neovide lights its floats: a faint shadow all round and a soft one cast
/// down by a light above, both cut out under it (no line round it: the text's own border shows). `a` how far it has come
/// (0 nothing, 1 all of it), `shadow` how dark (`[layers] shadow`).
fn layer_frame(r: [f32; 4], ch: f32, _fg: [f32; 4], a: f32, shadow: f32) -> Vec<Rect> {
    let (soft, drop) = (ch * 0.9, ch * 0.4);
    let mut out = Vec::new();
    if shadow > 0.0 {
        let around = (ch * 0.25).round();
        // the shape is drawn inset by its softness: out by 1.5 of it, its middle at the edge
        out.push(Rect { xywh: [r[0] - 2.0 * around, r[1] - 2.0 * around, r[2] + 4.0 * around, r[3] + 4.0 * around], color: [0.0, 0.0, 0.0, 0.12 * shadow * a], p: [2.0, around], hole: r });
        out.push(Rect { xywh: [r[0] - soft * 1.5, r[1] + drop - soft * 1.5, r[2] + soft * 3.0, r[3] + soft * 3.0], color: [0.0, 0.0, 0.0, 0.45 * shadow * a], p: [2.0, soft], hole: r });
    }
    out
}

impl Renderer {
    /// The shadows' shader is there to draw with: compiled when it is named, or its file changed;
    /// one that does not compile is said once and the usual shadow drawn.
    fn layer_fx_ready(&mut self, device: &wgpu::Device) -> bool {
        let Some(name) = self.layer_shader.clone() else {
            self.layer_fx = None;
            return false;
        };
        if self.layer_fx.as_ref().is_some_and(|f| f.source == name && !f.stale()) {
            return true;
        }
        if self.layer_fx.is_none() && self.layer_fx_failed.as_deref() == Some(name.as_str()) {
            return false;
        }
        match crate::layer_fx::LayerFx::load(device, self.format, &name) {
            Ok(f) => {
                self.layer_fx = Some(f);
                self.layer_fx_failed = None;
                true
            }
            Err(e) => {
                eprintln!("zash: [layers] shader: {e}");
                self.layer_fx = None;
                self.layer_fx_failed = Some(name);
                false
            }
        }
    }
}
