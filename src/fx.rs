//! Text effects (OSC 7733;fx): shimmer, pulse, fade, rainbow, typewriter, glow, scribble on a run of text
//! or its background, each with its own parameters. A run carries its effect as a hyperlink with
//! a `zash-fx:` address, so it stays on its cells through scrolling, scrollback and resizing.
//!
//! ```text
//! ESC ] 7733 ; fx ; shimmer ; target=bg ; speed=1.2 ; color=accent ; loop=2 ST   text   ESC ] 7733 ; fx ; end ST
//! ESC ] 7733 ; fx ; done ST   text   ESC ] 7733 ; fx ; end ST          (a preset from [fx.presets])
//! ESC ] 7733 ; fx ; scribble ; color=accent ; turns=2.2 ; wobble=0.1 ; width=0.13 ; draw=900ms ST
//!     Buffers   ESC ] 7733 ; fx ; end ST      (written, then a loop drawn round it by hand; it stays)
//! ```
//!
//! A program knows it may send these when TERM_PROGRAM=zash (zash sets it, with TERM_PROGRAM_VERSION),
//! or, over SSH too, by asking: `ESC ] 7733 ; q ST` is answered `ESC ] 7733 ; zash ; v=… ; fx=… ;
//! marks=… ; images=kitty ST` (no answer within a moment: not zash).
//! Unknown keys and values are ignored, so programs written for a newer zash still work here.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

use crate::config::Rgb;

/// The hyperlink scheme effects travel in (never opened as a link).
pub const SCHEME: &str = "zash-fx:";

/// `[fx]` in zash.toml.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// false: effects show as plain text
    pub enabled: bool,
    /// false: zfloats keep their plain panel, no GLSL ([zfloat.shaders] is ignored)
    pub shaders: bool,
    /// the most frames a second effects ask for (old machines: lower)
    pub fps: f32,
    /// name → the same text an escape carries (`shimmer;color=good;loop=2`)
    pub presets: HashMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: true, shaders: true, fps: 30.0, presets: HashMap::new() }
    }
}

/// What an `fx;…` escape body asks for.
#[derive(Debug, PartialEq)]
pub enum Escape {
    /// the address of a new run's effect
    Start(String),
    End,
}

impl Escape {
    /// `NAME;key=value;…` starts a run, `end` (or nothing) ends it.
    pub fn parse(body: &str) -> Escape {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let body = body.trim();
        if body.is_empty() || body == "end" {
            return Escape::End;
        }
        // each run its own id, so two runs next to each other stay two runs
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // since=MS: it began that long ago (zeit sends a pane's run again with each redraw of
        // its row; the line goes on being drawn instead of starting over)
        let since = body.split(';').find_map(|p| p.trim().strip_prefix("since=")?.parse::<u64>().ok()).unwrap_or(0);
        Escape::Start(format!("{SCHEME}{id};{};{body}", now_ms().saturating_sub(since)))
    }
}

/// The answer to OSC 7733;q: `ESC ] 7733 ; zash ; v=VERSION ; fx=…  ; marks=… ; images=kitty ST`.
/// A program sends the query, reads the answer (or nothing, in another terminal, after a short
/// wait) and knows what it may send. Over SSH too, where TERM_PROGRAM does not arrive.
pub fn capabilities() -> String {
    format!(
        "\x1b]7733;zash;v={};fx=shimmer,pulse,fade,rainbow,typewriter,glow;marks=scribble,underline,strike,box,check,arrow,bracket;images=kitty\x1b\\",
        env!("CARGO_PKG_VERSION")
    )
}

/// Seconds on the clock effects share (from zash's start). ZASH_FX_AT=SECONDS freezes it: every
/// effect starts at 0 and is drawn as it is that long after (exact, repeatable captures and tests).
pub fn now() -> f32 {
    frozen().unwrap_or_else(|| epoch().elapsed().as_secs_f32())
}

fn now_ms() -> u64 {
    if frozen().is_some() { 0 } else { epoch().elapsed().as_millis() as u64 }
}

pub(crate) fn frozen() -> Option<f32> {
    static F: OnceLock<Option<f32>> = OnceLock::new();
    *F.get_or_init(|| std::env::var("ZASH_FX_AT").ok().and_then(|s| s.parse::<f32>().ok()).filter(|t| t.is_finite() && *t >= 0.0))
}

fn epoch() -> Instant {
    static E: OnceLock<Instant> = OnceLock::new();
    *E.get_or_init(Instant::now)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// a bright band sweeping across
    Shimmer,
    /// the whole run breathing towards the colour
    Pulse,
    /// fading in
    Fade,
    /// hues running along the text
    Rainbow,
    /// characters appearing one by one
    Typewriter,
    /// a soft halo behind the run, breathing
    Glow,
    /// the text written, then a loop drawn around it by hand: more than two turns, wobbling,
    /// its ends apart (turns, wobble, width, draw, write=0 to skip the writing)
    Scribble,
    /// drawn by hand like the scribble, after the text (write=1 writes it first): a line under it,
    /// a line through it, a box round it, a check after it, an arrow pointing at it from the right,
    /// a curly bracket before it
    Underline,
    Strike,
    Box,
    Check,
    Arrow,
    Bracket,
}

impl Kind {
    /// Drawn with the pen (a stroke that stays), not a colour on the cells.
    pub fn draws(self) -> bool {
        matches!(self, Kind::Scribble | Kind::Underline | Kind::Strike | Kind::Box | Kind::Check | Kind::Arrow | Kind::Bracket)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Text,
    Background,
    Both,
}

/// A colour: one of the theme's roles (it follows the palette) or a fixed one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    /// the palette tone: a(ccent), g(ood), b(ad), r(unning), m(uted), f(oreground)
    Role(char),
    Fixed(Rgb),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Life {
    Cycles(f32),
    Seconds(f32),
    Forever,
}

/// One run's effect, read from its address.
#[derive(Clone, Debug, PartialEq)]
pub struct Effect {
    pub kind: Kind,
    pub target: Target,
    /// 1 = the effect's own pace
    pub speed: f32,
    /// 0..1: how far towards the colour
    pub intensity: f32,
    pub color: Paint,
    /// shimmer: the band's half width, in cells; rainbow: cells per full hue turn
    pub width: f32,
    /// seconds before it starts (fade and typewriter keep the text hidden meanwhile)
    pub delay: f32,
    /// scribble: how many times round, how much the line wanders (of the radius), seconds to
    /// draw it, and whether the text is written first
    pub turns: f32,
    pub wobble: f32,
    pub draw: f32,
    pub write: bool,
    life: Life,
    /// seconds on the shared clock when the run began
    start: f32,
}

/// What one cell of a run looks like at a moment. `None`: as it is without the effect.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Look {
    /// the text colour (alpha 0: not drawn)
    pub fg: Option<[f32; 4]>,
    pub bg: Option<[f32; 4]>,
    /// a soft halo behind the run (glow)
    pub halo: Option<[f32; 4]>,
}

impl Effect {
    /// Reads an address (`zash-fx:ID;START;NAME;key=value…`). A preset's own parameters come
    /// first, the escape's on top; an unknown effect name gives nothing (plain text).
    pub fn from_address(addr: &str, s: &Settings) -> Option<Effect> {
        let rest = addr.strip_prefix(SCHEME)?;
        let mut parts = rest.split(';');
        let _id = parts.next()?;
        let start = parts.next()?.parse::<u64>().ok()? as f32 / 1000.0;
        let name = parts.next()?.trim();
        let own: Vec<&str> = parts.collect();
        let (kind_name, preset): (&str, Vec<&str>) = match s.presets.get(name) {
            Some(p) => {
                let mut it = p.split(';');
                (it.next().unwrap_or("").trim(), it.collect())
            }
            None => (name, Vec::new()),
        };
        let kind = match kind_name {
            "shimmer" => Kind::Shimmer,
            "pulse" => Kind::Pulse,
            "fade" => Kind::Fade,
            "rainbow" => Kind::Rainbow,
            "typewriter" => Kind::Typewriter,
            "glow" => Kind::Glow,
            "scribble" => Kind::Scribble,
            "underline" => Kind::Underline,
            "strike" => Kind::Strike,
            "box" => Kind::Box,
            "check" => Kind::Check,
            "arrow" => Kind::Arrow,
            "bracket" => Kind::Bracket,
            _ => return None,
        };
        let once = matches!(kind, Kind::Fade | Kind::Typewriter);
        let mut e = Effect {
            kind,
            target: if kind == Kind::Glow { Target::Background } else { Target::Text },
            speed: 1.0,
            intensity: if kind == Kind::Rainbow { 1.0 } else { 0.8 },
            color: Paint::Role('a'),
            width: match kind {
                Kind::Rainbow => 24.0,
                // the pen, in parts of a cell's height
                Kind::Scribble | Kind::Underline | Kind::Strike | Kind::Box | Kind::Check | Kind::Arrow | Kind::Bracket => 0.13,
                _ => 4.0,
            },
            delay: 0.0,
            turns: 2.2,
            wobble: 0.1,
            // seconds the pen takes: longer shapes take longer
            draw: match kind {
                Kind::Scribble => 0.9,
                Kind::Box => 0.8,
                Kind::Arrow => 0.6,
                Kind::Underline | Kind::Bracket => 0.45,
                _ => 0.35,
            },
            // the loop writes its word first; the other marks go on text that is already there
            write: kind == Kind::Scribble,
            // a drawing stays once drawn
            life: if kind.draws() { Life::Forever } else { Life::Cycles(if once { 1.0 } else { 2.0 }) },
            start,
        };
        for kv in preset.into_iter().chain(own) {
            let Some((k, v)) = kv.split_once('=') else { continue };
            e.set(k.trim(), v.trim());
        }
        Some(e)
    }

    /// One parameter; a value that does not read is ignored (the default stays).
    fn set(&mut self, k: &str, v: &str) {
        match k {
            "target" => match v {
                "text" | "fg" => self.target = Target::Text,
                "bg" | "background" => self.target = Target::Background,
                "both" => self.target = Target::Both,
                _ => {}
            },
            "speed" => self.speed = num(v).filter(|s| *s > 0.0).unwrap_or(self.speed),
            "intensity" => self.intensity = num(v).map(|i| i.clamp(0.0, 1.0)).unwrap_or(self.intensity),
            "width" => self.width = num(v).filter(|w| *w > 0.0).unwrap_or(self.width),
            "delay" => self.delay = secs(v).unwrap_or(self.delay),
            "turns" => self.turns = num(v).map(|t| t.clamp(0.2, 8.0)).unwrap_or(self.turns),
            "wobble" => self.wobble = num(v).map(|w| w.clamp(0.0, 0.5)).unwrap_or(self.wobble),
            "draw" => self.draw = secs(v).filter(|d| *d > 0.0).unwrap_or(self.draw),
            "write" => self.write = !matches!(v, "0" | "false" | "no" | "off"),
            "color" => self.color = paint(v).unwrap_or(self.color),
            "loop" => self.life = if v == "forever" { Life::Forever } else { num(v).filter(|n| *n > 0.0).map(Life::Cycles).unwrap_or(self.life) },
            "duration" => self.life = if v == "forever" { Life::Forever } else { secs(v).filter(|d| *d > 0.0).map(Life::Seconds).unwrap_or(self.life) },
            _ => {}
        }
    }

    pub fn styles_text(&self) -> bool {
        matches!(self.target, Target::Text | Target::Both)
    }

    pub fn styles_background(&self) -> bool {
        matches!(self.target, Target::Background | Target::Both)
    }

    /// One cycle in seconds, for a run of `n` cells.
    fn cycle(&self, n: usize) -> f32 {
        let base = match self.kind {
            Kind::Shimmer => (n as f32 + 2.0 * self.width) / 24.0,
            Kind::Pulse => 1.2,
            Kind::Fade => 0.6,
            Kind::Rainbow => 2.0,
            Kind::Typewriter => n as f32 / 40.0,
            Kind::Glow => 1.6,
            Kind::Scribble | Kind::Underline | Kind::Strike | Kind::Box | Kind::Check | Kind::Arrow | Kind::Bracket => self.write_secs(n) * self.speed + self.draw,
        };
        base.max(0.05) / self.speed
    }

    /// Still moving at `now` (a run of `n` cells)? Afterwards it is drawn as plain text.
    pub fn live(&self, now: f32, n: usize) -> bool {
        let t = now - self.start - self.delay;
        match self.life {
            Life::Forever => true,
            Life::Cycles(c) => t < c * self.cycle(n),
            Life::Seconds(s) => t < s,
        }
    }

    /// Scribble: seconds the writing takes (0 with write=0).
    fn write_secs(&self, n: usize) -> f32 {
        if self.write { (n as f32 / 40.0).max(0.05) / self.speed } else { 0.0 }
    }

    /// Still changing at `now`: another frame is wanted. A scribble stays drawn after it is
    /// done, without asking for frames; the others change as long as they are live.
    pub fn animating(&self, now: f32, n: usize) -> bool {
        match self.kind {
            Kind::Scribble | Kind::Underline | Kind::Strike | Kind::Box | Kind::Check | Kind::Arrow | Kind::Bracket => now - self.start - self.delay < self.cycle(n),
            _ => self.live(now, n),
        }
    }

    /// The effect draws the run's text itself at `now` (the rows leave those cells blank).
    pub fn hides_text(&self, now: f32, n: usize) -> bool {
        match self.kind {
            // only while it is being written; then the text is plain and the loop stays
            Kind::Scribble | Kind::Underline | Kind::Strike | Kind::Box | Kind::Check | Kind::Arrow | Kind::Bracket => self.write && now - self.start - self.delay < self.write_secs(n),
            _ => self.styles_text() && self.live(now, n),
        }
    }

    /// Scribble: how much of the loop is drawn at `now`, 0..1, eased like a hand.
    pub fn stroke_progress(&self, now: f32, n: usize) -> f32 {
        let t = now - self.start - self.delay - self.write_secs(n);
        let p = (t / (self.draw / self.speed)).clamp(0.0, 1.0);
        // slow in, quick through the middle, slowing at the end
        if p < 0.5 { 4.0 * p * p * p } else { 1.0 - (-2.0 * p + 2.0).powi(3) / 2.0 }
    }

    /// The colour a role or fixed paint stands for.
    pub fn paint(&self, tone: impl Fn(char) -> Rgb) -> [f32; 4] {
        match self.color {
            Paint::Role(r) => tone(r).f32(),
            Paint::Fixed(c) => c.f32(),
        }
    }

    /// Scribble: the loop around a box (x0, y0, x1, y1, pixels) for a cell `ch` high, as dots
    /// (centre, radius) close enough to read as a line, drawn up to `progress`. More than two
    /// turns of an ellipse, each a little wider and turned, its radius wandering; the pen thin
    /// at both ends, which do not meet. `seed` (the run's text) makes each word its own loop,
    /// the same every time.
    pub fn scribble(&self, b: [f32; 4], ch: f32, seed: u64, progress: f32) -> Vec<([f32; 2], f32)> {
        use std::f32::consts::TAU;
        if progress <= 0.0 {
            return Vec::new();
        }
        // a small random generator, from the seed
        let mut s = seed ^ 0x9e37_79b9_7f4a_7c15;
        let mut rnd = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f32 / (1u64 << 53) as f32
        };
        let (cx, cy) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
        // a squarer loop than an ellipse (a superellipse, power 3) hugs a line of text; through
        // the box's corners it is 2^(1/3) its half sides. Room on top for the pen,
        // the wobble pulling inwards and the drift, so the loop never runs over the text
        let inner = 1.0 / (1.0 - self.wobble).max(0.5);
        let rx = ((b[2] - b[0]) / 2.0 * 2f32.powf(1.0 / 3.0) + ch * (0.3 + 0.22)) * inner;
        let ry = ((b[3] - b[1]) / 2.0 * 2f32.powf(1.0 / 3.0) + ch * (0.25 + 0.12)) * inner;
        let start = rnd() * TAU;
        let tilt = (rnd() - 0.5) * 0.12;
        let phase: [f32; 3] = [rnd() * TAU, rnd() * TAU, rnd() * TAU];
        let pen = (self.width * ch).max(1.0);
        let total = self.turns * TAU;
        // a step small enough that the dots overlap well (a fifth of the pen), twice over: the
        // squarer shape moves faster along its flat sides than an even step in angle suggests
        let around = TAU * rx.max(ry);
        let steps = ((around * self.turns) / (pen * 0.2) * 2.0).ceil().max(8.0) as usize;
        let last = ((steps as f32) * progress).ceil() as usize;
        let mut out = Vec::with_capacity(last + 1);
        for k in 0..=last.min(steps) {
            let u = k as f32 / steps as f32;
            let a = start + u * total;
            // each turn a little wider, its centre drifting; the radius wandering with a few slow waves
            let grow = 1.0 + 0.1 * (u * self.turns);
            let drift = [ch * 0.22 * (u * self.turns * std::f32::consts::PI + phase[1]).sin(), ch * 0.12 * (u * self.turns * 2.1 + phase[2]).sin()];
            let w = 1.0 + self.wobble * (0.6 * (a * 1.3 + phase[0]).sin() + 0.3 * (a * 2.7 + phase[1]).sin() + 0.1 * (a * 5.1 + phase[2]).sin());
            let sq = |v: f32| v.signum() * v.abs().powf(2.0 / 3.0);
            let (x, y) = (rx * grow * w * sq(a.cos()), ry * grow * w * sq(a.sin()));
            // turned a little, and further with each turn (a hand does not repeat itself)
            let turn = tilt + 0.06 * u * self.turns;
            let (x, y) = (x * turn.cos() - y * turn.sin() + drift[0], x * turn.sin() + y * turn.cos() + drift[1]);
            // the pen: thin where it starts and ends, full in between
            let taper = (std::f32::consts::PI * u).sin().max(0.0).sqrt();
            out.push(([cx + x, cy + y], pen * (0.25 + 0.75 * taper) / 2.0));
        }
        out
    }

    /// Any hand-drawn mark round the box: the loop, or one of the others drawn with `pen`.
    pub fn stroke(&self, b: [f32; 4], ch: f32, seed: u64, progress: f32) -> Vec<([f32; 2], f32)> {
        if self.kind == Kind::Scribble {
            return self.scribble(b, ch, seed, progress);
        }
        let (x0, y0, x1, y1) = (b[0], b[1], b[2], b[3]);
        let ym = (y0 + y1) / 2.0;
        let c = ch;
        // each mark: its strokes (the pen lifts between them), as points the line passes through
        let strokes: Vec<Vec<[f32; 2]>> = match self.kind {
            Kind::Underline => vec![vec![[x0 - 0.15 * c, y1 + 0.16 * c], [(x0 + x1) / 2.0, y1 + 0.26 * c], [x1 + 0.3 * c, y1 + 0.08 * c]]],
            Kind::Strike => vec![vec![[x0 - 0.2 * c, ym + 0.08 * c], [(x0 + x1) / 2.0, ym], [x1 + 0.2 * c, ym - 0.1 * c]]],
            // each corner twice: the smooth line turns sharply there, as a pen does
            Kind::Box => vec![vec![
                [x0 - 0.35 * c, y0 - 0.2 * c],
                [x1 + 0.45 * c, y0 - 0.3 * c],
                [x1 + 0.45 * c, y0 - 0.3 * c],
                [x1 + 0.4 * c, y1 + 0.3 * c],
                [x1 + 0.4 * c, y1 + 0.3 * c],
                [x0 - 0.45 * c, y1 + 0.25 * c],
                [x0 - 0.45 * c, y1 + 0.25 * c],
                // past the start: a hand overshoots the corner it began at
                [x0 - 0.3 * c, y0 - 0.45 * c],
            ]],
            Kind::Check => vec![vec![[x1 + 0.5 * c, ym], [x1 + 0.9 * c, y1 + 0.05 * c], [x1 + 1.8 * c, y0 - 0.6 * c]]],
            Kind::Arrow => {
                let tip = [x1 + 0.45 * c, ym];
                vec![
                    vec![[x1 + 4.0 * c, y0 - 0.9 * c], [x1 + 2.2 * c, y0 - 0.35 * c], tip],
                    vec![[tip[0] + 0.55 * c, tip[1] - 0.5 * c], tip, [tip[0] + 0.65 * c, tip[1] + 0.35 * c]],
                ]
            }
            Kind::Bracket => {
                let (h, x) = (y1 - y0, x0 - 0.7 * c);
                vec![vec![
                    [x + 0.35 * c, y0],
                    [x, y0 + 0.12 * h],
                    [x, ym - 0.12 * h],
                    [x - 0.3 * c, ym],
                    [x, ym + 0.12 * h],
                    [x, y1 - 0.12 * h],
                    [x + 0.35 * c, y1],
                ]]
            }
            _ => return Vec::new(),
        };
        pen(&strokes, (self.width * ch).max(1.0), self.wobble * ch * 0.6, seed, progress)
    }

    /// Cell `i` of a run of `n` at `now`, over these colours. `tone` resolves palette roles.
    pub fn look(&self, now: f32, i: usize, n: usize, fg: [f32; 4], bg: [f32; 4], tone: impl Fn(char) -> Rgb) -> Look {
        let t = now - self.start - self.delay;
        let color = match self.color {
            Paint::Role(r) => tone(r).f32(),
            Paint::Fixed(c) => c.f32(),
        };
        let k = self.intensity;
        let cyc = self.cycle(n);
        // 0..1: how far this cell is towards the colour (or, for fade/typewriter, shown)
        let f = if t < 0.0 {
            0.0
        } else {
            match self.kind {
                Kind::Shimmer => {
                    let pos = (t / cyc).fract() * (n as f32 + 2.0 * self.width) - self.width;
                    (1.0 - (i as f32 + 0.5 - pos).abs() / self.width).max(0.0)
                }
                // starts and ends at rest, so a finished effect never jumps
                Kind::Pulse | Kind::Glow => (1.0 - (t / cyc * std::f32::consts::TAU).cos()) / 2.0,
                Kind::Fade => (t / cyc).clamp(0.0, 1.0),
                Kind::Typewriter => ((t / cyc * n as f32) > i as f32) as u8 as f32,
                Kind::Scribble | Kind::Underline | Kind::Strike | Kind::Box | Kind::Check | Kind::Arrow | Kind::Bracket => match self.write_secs(n) {
                    w if w > 0.0 => ((t / w * n as f32) > i as f32) as u8 as f32,
                    _ => 1.0,
                },
                Kind::Rainbow => 1.0,
            }
        };
        let mut out = Look::default();
        match self.kind {
            Kind::Shimmer | Kind::Pulse => {
                if self.styles_text() {
                    out.fg = Some(mix(fg, color, f * k));
                }
                if self.styles_background() {
                    out.bg = Some(mix(bg, color, f * k));
                }
            }
            Kind::Fade | Kind::Typewriter | Kind::Scribble | Kind::Underline | Kind::Strike | Kind::Box | Kind::Check | Kind::Arrow | Kind::Bracket => {
                if self.styles_text() {
                    out.fg = Some([fg[0], fg[1], fg[2], fg[3] * f]);
                }
                if self.styles_background() {
                    out.bg = Some([bg[0], bg[1], bg[2], bg[3] * f]);
                }
            }
            Kind::Rainbow => {
                let hue = (i as f32 / self.width + t / cyc).fract();
                let c = hsv(hue, 0.6, 1.0);
                if self.styles_text() {
                    out.fg = Some(mix(fg, c, k));
                }
                if self.styles_background() {
                    out.bg = Some(mix(bg, c, k * 0.5));
                }
            }
            Kind::Glow => {
                out.halo = Some([color[0], color[1], color[2], 0.45 * k * (0.35 + 0.65 * f)]);
                if self.styles_text() {
                    out.fg = Some(mix(fg, color, 0.3 * f * k));
                }
            }
        }
        out
    }
}

/// The pen: strokes (each a few points) drawn as one smooth line each (Catmull-Rom), wandering a
/// little to the side (`wobble` pixels, seeded), thin where each stroke starts and ends, as dots
/// close enough to read as a line; drawn as far as `progress` along their whole length.
fn pen(strokes: &[Vec<[f32; 2]>], width: f32, wobble: f32, seed: u64, progress: f32) -> Vec<([f32; 2], f32)> {
    if progress <= 0.0 {
        return Vec::new();
    }
    let cr = |p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], t: f32| {
        let (t2, t3) = (t * t, t * t * t);
        let f = |a: f32, b: f32, c: f32, d: f32| 0.5 * (2.0 * b + (c - a) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2 + (3.0 * b - a - 3.0 * c + d) * t3);
        [f(p0[0], p1[0], p2[0], p3[0]), f(p0[1], p1[1], p2[1], p3[1])]
    };
    // every stroke as a fine polyline, with its length
    let lines: Vec<Vec<[f32; 2]>> = strokes
        .iter()
        .filter(|s| s.len() >= 2)
        .map(|s| {
            let n = s.len();
            let at = |i: isize| s[i.clamp(0, n as isize - 1) as usize];
            let mut out = Vec::new();
            for i in 0..n - 1 {
                let (p0, p1, p2, p3) = (at(i as isize - 1), at(i as isize), at(i as isize + 1), at(i as isize + 2));
                let seg = ((p2[0] - p1[0]).hypot(p2[1] - p1[1]) / 2.0).ceil().max(2.0) as usize;
                for k in 0..seg {
                    out.push(cr(p0, p1, p2, p3, k as f32 / seg as f32));
                }
            }
            out.push(s[n - 1]);
            out
        })
        .collect();
    let len = |l: &Vec<[f32; 2]>| l.windows(2).map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1])).sum::<f32>();
    let total: f32 = lines.iter().map(len).sum();
    let mut budget = total * progress.min(1.0);
    let step = (width * 0.2).max(0.5);
    let phase = (seed % 1000) as f32 * 0.37;
    let mut out = Vec::new();
    for (li, l) in lines.iter().enumerate() {
        let ll = len(l);
        if budget <= 0.0 || ll <= 0.0 {
            break;
        }
        let mut walked = 0.0f32;
        for w in l.windows(2) {
            let (a, b) = (w[0], w[1]);
            let d = (b[0] - a[0]).hypot(b[1] - a[1]);
            if d <= 0.0 {
                continue;
            }
            let (nx, ny) = (-(b[1] - a[1]) / d, (b[0] - a[0]) / d);
            let mut s = 0.0;
            while s < d {
                if walked + s > budget.min(ll) {
                    break;
                }
                let u = (walked + s) / ll;
                let p = [a[0] + (b[0] - a[0]) * s / d, a[1] + (b[1] - a[1]) * s / d];
                // the hand wanders a little to the side, differently in each stroke
                let off = wobble * ((u * 7.0 + phase + li as f32).sin() * 0.7 + (u * 17.0 + phase * 1.7).sin() * 0.3);
                let taper = (std::f32::consts::PI * u).sin().max(0.0).sqrt();
                out.push(([p[0] + nx * off, p[1] + ny * off], width * (0.3 + 0.7 * taper) / 2.0));
                s += step;
            }
            walked += d;
        }
        budget -= ll;
    }
    out
}

fn num(v: &str) -> Option<f32> {
    v.parse::<f32>().ok().filter(|n| n.is_finite())
}

/// `800ms`, `1.5s` or a plain number of seconds.
fn secs(v: &str) -> Option<f32> {
    match v.strip_suffix("ms") {
        Some(ms) => num(ms).map(|m| m / 1000.0),
        None => num(v.strip_suffix('s').unwrap_or(v)),
    }
    .filter(|s| *s >= 0.0)
}

/// A theme role or `#rrggbb`.
fn paint(v: &str) -> Option<Paint> {
    let role = match v {
        "accent" => 'a',
        "good" => 'g',
        "bad" => 'b',
        "running" => 'r',
        "muted" => 'm',
        "fg" | "foreground" => 'f',
        _ => {
            let h = v.strip_prefix('#').filter(|h| h.len() == 6)?;
            let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
            return Some(Paint::Fixed(Rgb(c(0)?, c(2)?, c(4)?)));
        }
    };
    Some(Paint::Role(role))
}

fn mix(a: [f32; 4], b: [f32; 4], f: f32) -> [f32; 4] {
    let f = f.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f, a[3]]
}

fn hsv(h: f32, s: f32, v: f32) -> [f32; 4] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match i as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [r, g, b, 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eff(body: &str, s: &Settings) -> Effect {
        let Escape::Start(addr) = Escape::parse(body) else { panic!("an end") };
        Effect::from_address(&addr, s).expect("an effect")
    }

    #[test]
    fn since_starts_it_earlier() {
        // a run zeit sends again goes on from where it was, not from the start
        std::thread::sleep(std::time::Duration::from_millis(5));
        let s = Settings::default();
        let fresh = eff("underline", &s);
        let older = eff(&format!("underline;since={}", now_ms()), &s);
        assert!(fresh.start - older.start > 0.004, "{} {}", fresh.start, older.start);
    }

    #[test]
    fn parameters_presets_and_unknowns() {
        let mut s = Settings::default();
        s.presets.insert("done".into(), "shimmer;color=good;speed=2".into());
        let e = eff("done;speed=3;target=both;color=#ff8000;nonsense=1;width=x", &s);
        assert_eq!(e.kind, Kind::Shimmer);
        assert_eq!(e.speed, 3.0); // the escape's own beats the preset's
        assert_eq!(e.target, Target::Both);
        assert_eq!(e.color, Paint::Fixed(Rgb(255, 128, 0)));
        assert_eq!(e.width, 4.0); // a bad value keeps the default
        assert_eq!(eff("glow", &s).target, Target::Background);
        assert_eq!(eff("fade;delay=250ms", &s).delay, 0.25);
        let Escape::Start(a) = Escape::parse("sparkle") else { panic!() };
        assert!(Effect::from_address(&a, &s).is_none());
        assert_eq!(Escape::parse("end"), Escape::End);
    }

    #[test]
    fn a_scribble_writes_then_loops_and_stays() {
        let s = Settings::default();
        let e = eff("scribble", &s);
        let n = 7;
        let written = e.start + e.write_secs(n);
        // writing: the text is hidden letter by letter, no loop yet
        assert!(e.hides_text(e.start + 0.01, n));
        assert_eq!(e.stroke_progress(written - 0.01, n), 0.0);
        // then the loop is drawn, and stays without asking for frames
        assert!(!e.hides_text(written + 0.01, n));
        let done = e.start + e.cycle(n) + 0.01;
        assert_eq!(e.stroke_progress(done, n), 1.0);
        assert!(e.live(done + 100.0, n) && !e.animating(done, n));
        assert!(!eff("scribble;write=0", &s).hides_text(e.start + 0.01, n), "write=0: no writing");
    }

    #[test]
    fn a_scribble_is_open_hand_drawn_and_the_same_every_time() {
        let s = Settings::default();
        let e = eff("scribble", &s);
        let b = [100.0, 50.0, 200.0, 70.0];
        let a = e.scribble(b, 20.0, 42, 1.0);
        assert_eq!(a, e.scribble(b, 20.0, 42, 1.0), "one word, one loop");
        assert_ne!(a, e.scribble(b, 20.0, 43, 1.0), "another word, another loop");
        // more than two turns: the dots go round the centre more than 720°
        let (cx, cy) = (150.0, 60.0);
        let mut turned = 0.0f32;
        for w in a.windows(2) {
            let (p, q) = (w[0].0, w[1].0);
            let d = (q[1] - cy).atan2(q[0] - cx) - (p[1] - cy).atan2(p[0] - cx);
            turned += (d + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        }
        assert!(turned.abs() > 2.0 * std::f32::consts::TAU, "turned {}°", turned.to_degrees());
        // the ends do not meet, and the pen is thin there
        let (first, last) = (a[0], a[a.len() - 1]);
        assert!(((first.0[0] - last.0[0]).powi(2) + (first.0[1] - last.0[1]).powi(2)).sqrt() > 5.0, "ends apart");
        let widest = a.iter().map(|d| d.1).fold(0.0, f32::max);
        assert!(first.1 < widest * 0.5 && last.1 < widest * 0.5, "tapered ends");
        // it surrounds the box, and is drawn as far as asked
        assert!(a.iter().all(|d| (d.0[0] < 100.0 || d.0[0] > 200.0) || (d.0[1] < 50.0 || d.0[1] > 70.0)), "never over the text");
        assert!(e.scribble(b, 20.0, 42, 0.5).len() < a.len() && e.scribble(b, 20.0, 42, 0.0).is_empty());
    }

    #[test]
    fn every_mark_draws_as_far_as_asked_and_only_the_loop_writes_first() {
        let s = Settings::default();
        let b = [100.0, 50.0, 200.0, 70.0];
        for name in ["underline", "strike", "box", "check", "arrow", "bracket"] {
            let e = eff(name, &s);
            assert!(e.kind.draws() && !e.write, "{name}: drawn on text already there");
            let all = e.stroke(b, 20.0, 7, 1.0);
            assert!(all.len() > 20, "{name}: a line ({} dots)", all.len());
            assert!(e.stroke(b, 20.0, 7, 0.5).len() < all.len(), "{name}: half is less");
            assert!(e.stroke(b, 20.0, 7, 0.0).is_empty(), "{name}: nothing before it starts");
            // neighbours close: it reads as a line, not dots (the pen lifts between strokes only)
            let gaps = all.windows(2).filter(|w| (w[1].0[0] - w[0].0[0]).hypot(w[1].0[1] - w[0].0[1]) > w[0].1.max(w[1].1) * 2.0).count();
            assert!(gaps <= 1, "{name}: {gaps} gaps");
        }
        // the loop writes its word first; any mark can with write=1
        assert!(eff("scribble", &s).write);
        let u = eff("underline;write=1", &s);
        assert!(u.hides_text(u.start + 0.01, 3));
    }

    #[test]
    fn effects_end_where_they_began() {
        let s = Settings::default();
        let tone = |_| Rgb(255, 255, 255);
        let (fg, bg) = ([0.5, 0.5, 0.5, 1.0], [0.0, 0.0, 0.0, 1.0]);
        let e = eff("pulse;loop=2", &s);
        let end = e.start + 2.0 * e.cycle(5);
        assert!(e.live(end - 0.01, 5) && !e.live(end + 0.01, 5));
        // a pulse at the end of its cycles is back at the text's own colour
        let at_end = e.look(end, 0, 5, fg, bg, tone).fg.unwrap();
        assert!((at_end[0] - 0.5).abs() < 0.01);
        // typewriter: the first character shows before the last
        let t = eff("typewriter", &s);
        let mid = t.start + t.cycle(10) / 2.0;
        assert_eq!(t.look(mid, 0, 10, fg, bg, tone).fg.unwrap()[3], 1.0);
        assert_eq!(t.look(mid, 9, 10, fg, bg, tone).fg.unwrap()[3], 0.0);
        assert!(eff("rainbow;loop=forever", &s).live(1e6, 3));
    }
}
