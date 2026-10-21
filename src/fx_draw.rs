//! Drawing text effects (fx.rs): each frame, cells whose effect is live get their colours. Their
//! characters are shaped once and recoloured, never shaped again; backgrounds are rectangles.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, Weight};

use crate::config::Rgb;
use crate::fx::{Effect, Settings};
use crate::render::{placed, CellView, Frame, Rect};

/// A cell's text and look: one shaped buffer each, drawn in any colour.
pub(crate) type GlyphKey = (String, bool, bool);

/// A character drawn by itself this frame, in its effect's colour.
pub(crate) struct Glyph {
    pub key: GlyphKey,
    pub x: f32,
    pub y: f32,
    pub color: [f32; 4],
    /// the band its row is clipped to (a gliding scroll region), as for the rows
    pub band: Option<(f32, f32)>,
}

/// What effects draw this frame: rectangles under the text, and characters.
#[derive(Default)]
pub(crate) struct Drawn {
    pub rects: Vec<Rect>,
    pub glyphs: Vec<Glyph>,
    /// something is still moving: another frame is wanted
    pub moving: bool,
}

pub(crate) struct FxLayer {
    settings: Settings,
    effects: HashMap<String, Option<Effect>>,
    glyphs: HashMap<GlyphKey, Buffer>,
    /// frames a second for effects; lowered when frames come out slow (old machines)
    fps: f32,
    slow: u32,
    /// when the next effect frame is wanted; None while nothing moves
    pub due: Option<Instant>,
}

/// A frame slower than this counts against the effect rate.
const SLOW_FRAME: Duration = Duration::from_millis(8);

impl FxLayer {
    pub fn new(settings: Settings) -> FxLayer {
        let fps = settings.fps.clamp(1.0, 120.0);
        FxLayer { settings, effects: HashMap::new(), glyphs: HashMap::new(), fps, slow: 0, due: None }
    }

    /// New settings (zash.toml changed): presets may read differently now.
    pub fn set_settings(&mut self, s: Settings) {
        if s != self.settings {
            self.fps = s.fps.clamp(1.0, 120.0);
            self.settings = s;
            self.effects.clear();
        }
    }

    /// Glyph buffers were shaped for one font size: a new size drops them.
    pub fn font_changed(&mut self) {
        self.glyphs.clear();
    }

    fn effect(&mut self, uri: &str) -> Option<Effect> {
        if !self.settings.enabled {
            return None;
        }
        let s = &self.settings;
        self.effects.entry(uri.to_string()).or_insert_with(|| Effect::from_address(uri, s)).clone()
    }

    /// Cells per run on screen, so an effect knows its run's length.
    pub fn spans(frame: &Frame) -> HashMap<String, usize> {
        let mut n = HashMap::new();
        for row in frame.above.iter().chain(&frame.rows).chain(&frame.below) {
            for c in row.iter().filter(|c| !c.spacer) {
                if let Some(h) = &c.fx {
                    *n.entry(h.uri().to_string()).or_insert(0) += 1;
                }
            }
        }
        n
    }

    /// Runs whose text is drawn by the effect this frame (the rows leave those cells blank).
    pub fn hidden(&mut self, spans: &HashMap<String, usize>, now: f32) -> HashSet<String> {
        spans
            .iter()
            .filter(|(uri, n)| self.effect(uri).is_some_and(|e| e.hides_text(now, **n)))
            .map(|(uri, _)| uri.clone())
            .collect()
    }

    /// The live effects of one pane, at its origin.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&mut self, fs: &mut FontSystem, family: &str, font_px: f32, cw: f32, ch: f32, frame: &Frame, spans: &HashMap<String, usize>, ox: f32, oy: f32, now: f32) -> Drawn {
        let mut out = Drawn::default();
        if !self.settings.enabled || spans.is_empty() {
            return out;
        }
        let mut index: HashMap<&str, usize> = HashMap::new();
        // scribbles: each run's box, text, effect and length
        let mut loops: HashMap<String, ([f32; 4], String, Effect, usize)> = HashMap::new();
        // a halo per row segment of a run: (uri, row y, x0, x1, colour)
        let mut halos: Vec<(String, f32, f32, f32, [f32; 4])> = Vec::new();
        for (row, y, band) in placed(frame, oy, ch) {
            for (col, c) in row.iter().enumerate() {
                if c.spacer {
                    continue;
                }
                let Some(h) = &c.fx else { continue };
                let uri = h.uri();
                let i = index.entry(uri).or_insert(0);
                let at = *i;
                *i += 1;
                let n = spans.get(uri).copied().unwrap_or(1);
                let Some(e) = self.effect(uri).filter(|e| e.live(now, n)) else { continue };
                // only what still changes asks for frames (a drawn scribble just stays)
                out.moving |= e.animating(now, n);
                let x = ox + col as f32 * cw;
                let w = if c.wide { 2.0 * cw } else { cw };
                if e.kind == crate::fx::Kind::Scribble {
                    // its box and text, for the loop drawn after the cells
                    let s = loops.entry(uri.to_string()).or_insert_with(|| ([f32::MAX, f32::MAX, f32::MIN, f32::MIN], String::new(), e.clone(), n));
                    s.0 = [s.0[0].min(x), s.0[1].min(y), s.0[2].max(x + w), s.0[3].max(y + ch)];
                    s.1.push(c.ch);
                    if !e.hides_text(now, n) {
                        continue;
                    }
                }
                let fg = Rgb(c.style.fg.0, c.style.fg.1, c.style.fg.2).f32();
                let bg = c.bg.unwrap_or(frame.bg).f32();
                let look = e.look(now, at, n, fg, bg, |t| frame.tone(t));
                if let Some(color) = look.bg {
                    out.rects.push(Rect { xywh: [x, y, w, ch], color, p: [0.0, 0.0] });
                }
                if let Some(color) = look.halo {
                    match halos.last_mut() {
                        Some((u, hy, _, x1, _)) if u == uri && *hy == y && (*x1 - x).abs() < 0.5 => *x1 = x + w,
                        _ => halos.push((uri.to_string(), y, x, x + w, color)),
                    }
                }
                if let Some(color) = look.fg.filter(|c| c[3] > 0.01) {
                    let mut text = String::from(c.ch);
                    text.extend(c.zw.iter().flatten());
                    if text.trim().is_empty() {
                        continue;
                    }
                    let key = (text, c.style.bold, c.style.italic);
                    if !self.glyphs.contains_key(&key) {
                        self.glyphs.insert(key.clone(), shape(fs, family, font_px, ch, &key));
                    }
                    out.glyphs.push(Glyph { key, x, y, color, band });
                }
            }
        }
        // the loops around scribbled runs: dots along the path, under the text
        for (_, (b, text, e, n)) in loops {
            let p = e.stroke_progress(now, n);
            let seed = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, c| (h ^ c as u64).wrapping_mul(0x0100_0000_01b3));
            let color = e.paint(|t| frame.tone(t));
            for ([x, y], r) in e.scribble(b, ch, seed, p) {
                out.rects.push(Rect { xywh: [x - r, y - r, 2.0 * r, 2.0 * r], color, p: [r, 0.6] });
            }
        }
        // a soft glow around each segment, under the text
        for (_, y, x0, x1, color) in halos {
            let spread = ch * 0.3;
            out.rects.push(Rect { xywh: [x0 - spread, y - spread * 0.5, x1 - x0 + 2.0 * spread, ch + spread], color, p: [ch * 0.5, ch * 0.45] });
        }
        out
    }

    /// GLSL panels may be drawn (the setting, and the budget guard has not turned them off).
    pub fn shaders_on(&self) -> bool {
        self.settings.shaders
    }

    pub fn buffer(&self, key: &GlyphKey) -> Option<&Buffer> {
        self.glyphs.get(key)
    }

    /// After a frame: when to draw the next one, and the frame's cost against the budget.
    /// Frames that keep coming out slow halve the rate (down to 10 a second); if even that is
    /// too much, effects stop and their text shows plain.
    pub fn frame_done(&mut self, moving: bool, took: Duration) {
        if moving && took > SLOW_FRAME {
            self.slow += 1;
            if self.slow >= 10 {
                self.slow = 0;
                if self.fps > 10.0 {
                    self.fps = (self.fps / 2.0).max(10.0);
                    eprintln!("zash: effect frames take {} ms: {} a second now", took.as_millis(), self.fps);
                } else {
                    self.settings.enabled = false;
                    self.settings.shaders = false;
                    self.effects.clear();
                    eprintln!("zash: effect frames still take {} ms at 10 a second: effects and panel shaders off ([fx] in zash.toml)", took.as_millis());
                }
            }
        } else if moving {
            self.slow = self.slow.saturating_sub(1);
        }
        self.due = (moving && (self.settings.enabled || self.settings.shaders)).then(|| Instant::now() + Duration::from_secs_f32(1.0 / self.fps));
    }
}

/// One character, without a colour of its own: drawn in its text area's colour.
fn shape(fs: &mut FontSystem, family: &str, font_px: f32, ch: f32, key: &GlyphKey) -> Buffer {
    let mut attrs = Attrs::new().family(Family::Name(family));
    if key.1 {
        attrs = attrs.weight(Weight::BOLD);
    }
    if key.2 {
        attrs = attrs.style(Style::Italic);
    }
    let mut b = Buffer::new(fs, Metrics::new(font_px, ch));
    b.set_size(None, Some(ch));
    b.set_text(&key.0, &attrs, Shaping::Advanced, None);
    b.shape_until_scroll(fs, false);
    b
}

/// The cell's own text, for a row that leaves an effect's characters blank.
pub(crate) fn hides(c: &CellView, hidden: &HashSet<String>) -> bool {
    c.fx.as_ref().is_some_and(|h| hidden.contains(h.uri()))
}
